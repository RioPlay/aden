// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! MCP (Model Context Protocol) server for Aden.
//!
//! This is a **thin director** — no business logic, no graph pre-loading.
//! Each MCP tool maps 1:1 to an `aden` CLI subcommand.
//! When called, we convert JSON args to CLI flags and run the binary.
//!
//! Built on the official `rmcp` Rust SDK.

use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    model::*,
    service::{RequestContext, RoleServer},
    transport::stdio,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ── Server struct ───────────────────────────────────────────

#[derive(Clone)]
pub struct AdenMcpServer {
    project_dir: PathBuf,
}

impl AdenMcpServer {
    pub fn new(project_dir: PathBuf) -> Self {
        Self { project_dir }
    }
}

/// Guidance surfaced to the LLM at session start. Frames Aden as a
/// language-agnostic context compiler and gives the canonical workflow so the
/// model uses the tools correctly on *any* project (not just Rust).
const SERVER_INSTRUCTIONS: &str = "\
Aden is a language-agnostic referential context compiler: it turns ANY codebase \
or documentation (Rust, Python, Go, TypeScript, Java, Ruby, PHP, C/C++, and 300+ \
more — plus Markdown/AsciiDoc docs) into a queryable knowledge graph. Every \
result is derived from the project you point it at.\n\n\
WHY ADEN: its unique value is the GRAPH — 'what calls, references, or breaks if I \
change this symbol?' (blast radius and downstream impact). Plain text search and \
embeddings cannot answer that; the graph can, with high precision. That is the \
reason to reach for aden, especially before you change code.\n\n\
TWO RULES (they apply to any subagents you spawn — they do NOT inherit these \
instructions, so tell them too):\n\
- Before you READ unfamiliar code or find a symbol: first move is \
`grep`/`ask`/`locate` — NOT Bash `grep`/`find`/`cat`/`head`/`tail` or the Read \
tool. These return structure-aware results (every hit tagged with its enclosing \
symbol) and assemble just the relevant neighborhood under a token budget, far \
cheaper than reading whole files.\n\
- Before you EDIT or refactor a symbol: first move is `understand <symbol>` (or \
`query backlinks=`/`impact=`) — the callers and downstream nodes at risk. Never \
change a symbol without knowing what references it. This is aden's reason to exist.\n\n\
The graph is fresh by construction: the read tools auto-reindex any source that \
changed since the last run, so you do NOT need to call `gen` first. Only run \
`gen` after large external changes — cloning a new repo, a big merge, or \
generated code appearing outside your edits.\n\n\
EXPLORE a codebase:\n\
1. `grep \"pattern\"` — structure-aware content search; every hit tagged with its \
enclosing symbol (the anchor you feed to `locate`/`asm`).\n\
2. `ask` a natural-language question, or `locate` a symbol's definition and call sites.\n\
3. `understand <symbol>` for one-shot comprehension (definition + callers + \
downstream impact), or `asm`/`query` to traverse the graph yourself. `list` and \
`communities` orient you in an unfamiliar tree.\n\n\
CHANGE code safely (aden's killer loop):\n\
1. `locate`/`understand` the target symbol.\n\
2. `understand <symbol>` (or `query backlinks=<anchor>` / `impact=<anchor>`) \
BEFORE editing — see every caller and downstream node at risk.\n\
3. Make the edit.\n\
4. `impact-diff` maps your git diff to the symbols it touches and re-checks the \
blast radius; `check` validates the graph and gates CI; `test`/`lint`/`audit` verify.\n\n\
The `path` argument defaults to the current project directory for every tool. By \
default only the ESSENTIAL tools are listed (grep, locate, understand, ask, asm, \
query — the find->comprehend->blast-radius loop). Set ADEN_MCP_SURFACE=standard to \
also list the change-safety / verify / orient tools (check, impact-diff, list, \
communities, status, diagnose, test, lint, audit), or =full for the build/setup/\
admin tools too. Every tool stays callable by name at any level, so nothing is \
ever out of reach.";

// ── Tool declaration ──────────────────────────────────────────

/// Surface tier — how broad an enablement a tool needs to be LISTED. All tools
/// stay callable by name regardless of tier; this gates the default `list_tools`
/// registry only, so a session is not flooded with build/setup/admin tools.
/// Ordered Essential < Standard < Full, so `tool_tier(name) <= requested_surface()`
/// selects every tool at or below the requested level.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum Tier {
    Essential,
    Standard,
    Full,
}

/// ESSENTIAL surface (the default): the find -> comprehend -> blast-radius loop.
/// The smallest set that delivers aden's core value. Everything else is opt-in.
const ESSENTIAL_TOOLS: &[&str] = &["grep", "locate", "understand", "ask", "asm", "query"];

/// STANDARD surface (`ADEN_MCP_SURFACE=standard`): adds change-safety, verify, and
/// orientation tools on top of Essential.
const STANDARD_TOOLS: &[&str] = &[
    "check",
    "impact-diff",
    "list",
    "communities",
    "status",
    "diagnose",
    "test",
    "lint",
    "audit",
];

/// Tier of a tool by name. Anything not Essential/Standard is Full (the
/// build/setup/admin tools surfaced only at `ADEN_MCP_SURFACE=full`).
fn tool_tier(name: &str) -> Tier {
    if ESSENTIAL_TOOLS.contains(&name) {
        Tier::Essential
    } else if STANDARD_TOOLS.contains(&name) {
        Tier::Standard
    } else {
        Tier::Full
    }
}

/// What a tool does to the project, surfaced to MCP clients as tool
/// annotations (`ToolAnnotations`). The load-bearing signal is read-only-ness:
/// clients that gate tool calls behind a permission prompt can auto-approve
/// read-only tools, removing the per-call friction that otherwise pushes an
/// agent back to Bash. Mutating tools are deliberately NOT marked read-only so
/// that confirmation step survives.
#[derive(PartialEq, Clone, Copy)]
enum Effect {
    /// Pure navigation/inspection. A transparent incremental reindex does not
    /// count as a mutation. `read_only_hint = true`.
    Read,
    /// Rebuilds derived store state from source; idempotent and never touches
    /// the working tree. Not read-only, but non-destructive.
    Rebuild,
    /// May modify the working tree or config, or run project code. Treated as
    /// destructive so clients keep a confirmation step.
    Mutate,
}

impl Effect {
    /// Map the effect to MCP tool annotations. Per the MCP spec the
    /// `destructive`/`idempotent` hints are only meaningful when
    /// `read_only == false`, so the Read arm sets just `read_only`/`open_world`;
    /// the Mutate arm leaves `destructive`/`idempotent` unset so clients fall
    /// back to the conservative spec defaults (destructive=true, idempotent=false).
    fn annotations(self) -> ToolAnnotations {
        match self {
            Effect::Read => ToolAnnotations::new().read_only(true).open_world(false),
            Effect::Rebuild => ToolAnnotations::new()
                .read_only(false)
                .destructive(false)
                .idempotent(true)
                .open_world(false),
            Effect::Mutate => ToolAnnotations::new().read_only(false),
        }
    }
}

/// A tool the LLM can invoke.  Zero code per tool — just metadata.
struct ToolSpec {
    name: &'static str,
    /// Human-readable display name, surfaced via the MCP `title` field so the
    /// tool reads as purpose-built rather than a bare verb.
    title: &'static str,
    description: &'static str,
    args: &'static [(&'static str, &'static str)], // (arg_name, arg_type: "string"|"boolean"|"integer"|"number")
    /// Read / Rebuild / Mutate — drives the MCP tool annotations.
    effect: Effect,
}

/// Required argument names per tool (must match the `arg_name` strings in the
/// TOOLS table exactly). Anything not listed is optional — `path` always
/// defaults to the current project directory, so it is never required.
fn required_args(tool: &str) -> &'static [&'static str] {
    match tool {
        "ask" => &["question"],
        "search" => &["query"],
        "grep" => &["pattern"],
        "kickoff" => &["name"],
        "new" => &["name", "lang"],
        "workflow" => &["template"],
        // `from` carries the anchor for asm; the CLI marks it required.
        "asm" => &["from"],
        // CLI makes `status` optional (Option<String>); only id + task are required.
        "session" => &["agent_id", "task"],
        "emergency" => &["reason"],
        _ => &[],
    }
}

/// Allowed values for an enumerable argument, surfaced as a JSON-schema `enum`
/// so a client can validate the value and an LLM can see the valid set up front
/// instead of discovering a bad value via an opaque CLI usage error.
///
/// These are HINTS, not server-enforced: the CLI parses these args as plain
/// strings (not clap `ValueEnum`), so the accepted set lives only in `--help`
/// prose and may evolve. Enforcing a possibly-stale set server-side could block
/// a newly-valid value, so we only advertise it. The `mcp_enum_values_exist_in_cli_help`
/// contract test pins every value below to the CLI `--help`, catching drift if a
/// value is renamed or removed. Empty slice = no constraint.
fn arg_enum(tool: &str, arg: &str) -> &'static [&'static str] {
    match (tool, arg) {
        // Subcommand-dispatch verbs (clap subcommands — the most stable set).
        ("federation", "action") => &["list", "add", "remove", "config"],
        ("mcp", "action") => &["install", "uninstall", "list"],
        // Severity thresholds — note check uses Forbid, lint uses Error.
        ("check", "severity") => &["Suggest", "Warn", "Forbid"],
        ("lint", "severity") => &["Suggest", "Warn", "Error"],
        // Output formats — each tool's accepted set differs.
        ("viz", "format") => &["mermaid", "dot", "asciidoc", "json"],
        ("asm", "format") => &["llm", "adg", "aden"],
        ("audit", "format") => &["text", "json", "adoc"],
        ("diagnose", "format") => &["text", "json"],
        ("query", "format") => &["json", "table"],
        // Other closed value sets.
        ("viz", "mode") => &["blast", "reach", "connectivity", "communities"],
        ("ask", "intent") => &[
            "debug", "usage", "explain", "refactor", "impact", "list", "compare", "count",
            "general",
        ],
        ("audit", "lang") => &["rust", "python", "go", "ts", "php"],
        ("search", "doc_type") => &["module", "adr", "plan", "use-case"],
        ("test", "scope") => &["unit", "integration", "all"],
        _ => &[],
    }
}

/// "At least one of" argument groups that a flat `required` list cannot express
/// (that is an AND). Rendered as JSON-schema `anyOf: [{required: [...]}, …]`.
/// `locate` needs one of `symbol`/`caller_of`; `validate_args` enforces the same
/// rule at call time, so this is the schema-level hint for the same constraint.
fn any_of_required(tool: &str) -> &'static [&'static [&'static str]] {
    match tool {
        "locate" => &[&["symbol"], &["caller_of"]],
        _ => &[],
    }
}

/// Cross-argument validation that a flat JSON-schema `required` list cannot
/// express (e.g. "at least one of A or B"). Returns `Err(message)` when the
/// constraint is violated. Runs after schema validation, before shelling out.
fn validate_args(
    tool: &str,
    args: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let present = |k: &str| args.get(k).map(|v| !v.is_null()).unwrap_or(false);
    if tool == "locate" && !present("symbol") && !present("caller_of") {
        return Err("locate requires at least one of `symbol` or `caller_of`".to_string());
    }
    // viz has TWO optional positionals (`aden viz [ANCHOR] [DIR]`). If `path`
    // is supplied without `anchor`, clap would bind the path to the ANCHOR
    // slot and silently visualize a nonsense symbol — reject the combination
    // instead. (Same ambiguity exists for direct CLI users; here we can catch
    // it at the boundary.)
    if tool == "viz" && present("path") && !present("anchor") {
        return Err(
            "viz: `path` cannot be passed without `anchor` (the CLI parses the first \
             positional as ANCHOR); supply `anchor`, or omit `path` to use the project root"
                .to_string(),
        );
    }
    // Type-check declared boolean args: a non-bool value (e.g. the string
    // "true") must be rejected, not silently coerced to false and dropped.
    if let Some(spec) = TOOLS.iter().find(|t| t.name == tool) {
        for &(arg_name, arg_type) in spec.args {
            if arg_type != "boolean" {
                continue;
            }
            if let Some(v) = args.get(arg_name)
                && !v.is_null()
                && !v.is_boolean()
            {
                return Err(format!(
                    "argument `{}` must be a boolean (true/false), got {}",
                    arg_name,
                    match v {
                        serde_json::Value::String(_) => "a string",
                        serde_json::Value::Number(_) => "a number",
                        serde_json::Value::Array(_) => "an array",
                        serde_json::Value::Object(_) => "an object",
                        _ => "a non-boolean value",
                    }
                ));
            }
        }
    }
    Ok(())
}

/// Returns true if `arg` should be passed positionally (no `--` prefix) for `tool`.
fn is_positional(tool: &str, arg: &str) -> bool {
    match (tool, arg) {
        // path is positional for every command (diagnose was migrated from a
        // `--path` flag to a positional DIR for consistency).
        (_, "path") => true,
        // ask:   aden ask <QUESTION> [DIR]
        ("ask", "question") => true,
        // search: aden search <QUERY> [DIR]
        ("search", "query") => true,
        // grep:  aden grep <PATTERN> [DIR]
        ("grep", "pattern") => true,
        // new:   aden new <NAME> --lang <LANG> [DIR]  (lang is a flag, not positional)
        ("new", "name") => true,
        // workflow: aden workflow <TEMPLATE> [DIR]
        ("workflow", "template") => true,
        // query-adq: aden query-adq <SCRIPT> [DIR]
        ("query-adq", "script") => true,
        // understand: aden understand <SYMBOL> [DIR]
        ("understand", "symbol") => true,
        // viz:   aden viz [ANCHOR] [DIR]
        ("viz", "anchor") => true,
        // federation/mcp dispatch on a positional subcommand token (list, add, …)
        ("federation" | "mcp", "action") => true,
        _ => false,
    }
}

/// True if the positional `arg` of `tool` is a clap SUBCOMMAND token (a fixed
/// verb like `list`/`install`) rather than a free-form value positional.
///
/// A `--` end-of-options terminator must NOT be emitted before a subcommand —
/// clap would fail to dispatch it. It is only needed before value positionals
/// (path/question/pattern/…), where an attacker-controlled leading-dash value
/// could otherwise be parsed as a flag.
fn is_subcommand_dispatch(tool: &str, arg: &str) -> bool {
    matches!((tool, arg), ("federation" | "mcp", "action"))
}

/// Contract-test accessor: every tool paired with its declared `(arg, type)` list.
/// Lets `aden-cli` assert that no MCP-emittable flag has drifted from the CLI.
pub fn tool_arg_specs() -> Vec<(&'static str, &'static [(&'static str, &'static str)])> {
    TOOLS.iter().map(|t| (t.name, t.args)).collect()
}

/// Contract-test accessor: every `(tool, arg, allowed-values)` the MCP schema
/// constrains with an `enum`. Lets `aden-cli` assert each value still appears in
/// that command's `--help`, catching drift when a value is renamed or removed.
pub fn tool_arg_enums() -> Vec<(&'static str, &'static str, &'static [&'static str])> {
    let mut out = Vec::new();
    for t in TOOLS {
        for &(arg, _ty) in t.args {
            let vals = arg_enum(t.name, arg);
            if !vals.is_empty() {
                out.push((t.name, arg, vals));
            }
        }
    }
    out
}

/// Contract-test accessor: does `tool` take `arg` positionally (vs. as a `--flag`)?
pub fn arg_is_positional(tool: &str, arg: &str) -> bool {
    is_positional(tool, arg)
}

/// Extra CLI flags the MCP appends so a read tool emits machine-readable output
/// instead of terminal chrome. Only tools that actually have a JSON path belong
/// here; the flags are skipped if the agent already supplied them (e.g. a
/// `format` arg), so we never pass a flag twice. Expanded per Phase 2 as
/// `search`/`list`/`ask` gain JSON envelopes.
fn structured_output_flags(tool: &str) -> &'static [&'static str] {
    match tool {
        // These honor the global `-j/--json` and print a structured envelope.
        "grep" | "search" | "list" | "test" | "impact-diff" | "communities" => &["--json"],
        _ => &[],
    }
}

/// Translate MCP JSON arguments into `aden` CLI arguments for `spec`.
///
/// The first element is the subcommand name. Snake_case arg names are converted
/// to the kebab-case long flags clap actually accepts (`edge_types` →
/// `--edge-types`); positional args are emitted bare. Pure and side-effect-free
/// so the flag mapping is unit-tested directly.
fn build_cli_args(
    spec: &ToolSpec,
    args: &serde_json::Map<String, serde_json::Value>,
    extra_flags: &[&str],
) -> Vec<String> {
    let mut cmd_args: Vec<String> = vec![spec.name.to_string()];
    // Positional values are collected separately so we can emit a single `--`
    // end-of-options terminator before them. Without it, a caller-supplied
    // value beginning with `-` (e.g. path="--fix") would be parsed by clap as a
    // flag rather than data — argument smuggling into the selected subcommand
    // (security: audit finding MEDIUM-1).
    let mut positionals: Vec<String> = Vec::new();
    for &(arg_name, arg_type) in spec.args {
        let val = match args.get(arg_name) {
            Some(v) => v,
            None => continue,
        };
        let flag = format!("--{}", arg_name.replace('_', "-"));
        match arg_type {
            "boolean" if val.as_bool().unwrap_or(false) => {
                cmd_args.push(flag);
            }
            "string" | "integer" | "number" => {
                let s = match arg_type {
                    "integer" => val
                        .as_u64()
                        .map(|n| n.to_string())
                        .or_else(|| val.as_i64().map(|n| n.to_string())),
                    // JSON "number" (e.g. communities --resolution 1.5): accept
                    // both integral and fractional values; `{}` on f64 renders
                    // 2.0 as "2", which clap's f64 parser accepts fine.
                    "number" => val.as_f64().map(|n| n.to_string()),
                    _ => val.as_str().map(|s| s.to_string()),
                };
                if let Some(s) = s {
                    if is_subcommand_dispatch(spec.name, arg_name) {
                        // A subcommand verb (federation/mcp `action`) must be a
                        // bare token before any `--`, so clap can dispatch it.
                        cmd_args.push(s);
                    } else if is_positional(spec.name, arg_name) {
                        positionals.push(s);
                    } else {
                        cmd_args.push(flag);
                        cmd_args.push(s);
                    }
                }
            }
            _ => {}
        }
    }
    // Extra machine-output flags (e.g. `--json`) go with the flags, before the
    // `--` terminator, so clap parses them as flags rather than positionals.
    for flag in extra_flags {
        if !cmd_args.iter().any(|a| a == flag) {
            cmd_args.push(flag.to_string());
        }
    }
    if !positionals.is_empty() {
        cmd_args.push("--".to_string());
        cmd_args.extend(positionals);
    }
    cmd_args
}

/// Build the JSON Schema + `Tool` for a spec. Single builder so `get_tool` and
/// `list_tools` can never drift apart.
fn tool_from_spec(spec: &ToolSpec) -> Tool {
    let mut props = serde_json::Map::new();
    for &(arg_name, ty) in spec.args {
        let mut p = serde_json::Map::new();
        p.insert("type".to_string(), serde_json::json!(ty));
        // Constrain enumerable args (e.g. federation/mcp `action`) so a client
        // can validate the value and an LLM sees the valid set, instead of
        // discovering a bad verb via an opaque CLI error.
        let allowed = arg_enum(spec.name, arg_name);
        if !allowed.is_empty() {
            p.insert("enum".to_string(), serde_json::json!(allowed));
        }
        props.insert(arg_name.to_string(), serde_json::Value::Object(p));
    }
    let mut schema = JsonObject::new();
    schema.insert("type".to_string(), serde_json::json!("object"));
    schema.insert("properties".to_string(), serde_json::Value::Object(props));
    // Reject unknown/misspelled args at the schema boundary instead of silently
    // ignoring them (e.g. a typo'd `dept` for `depth` would otherwise no-op).
    schema.insert("additionalProperties".to_string(), serde_json::json!(false));
    let required = required_args(spec.name);
    if !required.is_empty() {
        schema.insert("required".to_string(), serde_json::json!(required));
    }
    // "At least one of" constraints (e.g. locate's symbol/caller_of) cannot be a
    // flat `required` list; express them as `anyOf` so the schema itself hints
    // the rule that `validate_args` also enforces at call time.
    let any_of = any_of_required(spec.name);
    if !any_of.is_empty() {
        let clauses: Vec<serde_json::Value> = any_of
            .iter()
            .map(|group| serde_json::json!({ "required": group }))
            .collect();
        schema.insert("anyOf".to_string(), serde_json::json!(clauses));
    }
    // A friendly display title + Read/Rebuild/Mutate annotations let clients
    // present read tools as auto-approvable (no per-call permission wall) while
    // keeping a confirmation step on the mutating ones.
    Tool::new(spec.name, spec.description, Arc::new(schema))
        .with_title(spec.title)
        .annotate(spec.effect.annotations())
}

/// The tool surface the operator requested, gating which tools `list_tools`
/// returns (all tools stay callable by name regardless). Default is ESSENTIAL —
/// the smallest high-value set. `ADEN_MCP_SURFACE=essential|standard|full` (or
/// 1|2|3) widens it; the legacy `ADEN_MCP_FULL=1` is kept as an alias for `full`.
fn requested_surface() -> Tier {
    if let Ok(v) = std::env::var("ADEN_MCP_SURFACE") {
        match v.trim().to_ascii_lowercase().as_str() {
            "essential" | "1" | "min" | "minimal" | "core" => return Tier::Essential,
            "standard" | "2" | "std" | "extended" => return Tier::Standard,
            "full" | "3" | "all" => return Tier::Full,
            // Unrecognized value: fall through to the legacy toggle / default.
            _ => {}
        }
    }
    if parse_full(std::env::var("ADEN_MCP_FULL").ok().as_deref()) {
        return Tier::Full;
    }
    Tier::Essential
}

/// Pure parse of the legacy `ADEN_MCP_FULL` toggle, split out so it is testable
/// without mutating process-global environment state.
fn parse_full(v: Option<&str>) -> bool {
    matches!(
        v.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "full" | "yes")
    )
}

/// Every MCP tool maps 1:1 to `aden <name> <args>`.
static TOOLS: &[ToolSpec] = &[
    // ── Tier is assigned by NAME (see ESSENTIAL_TOOLS / STANDARD_TOOLS up top),
    //    not by position in this slice, so reordering tools never changes the
    //    surface. ESSENTIAL (default) = the find -> comprehend -> blast-radius loop;
    //    STANDARD adds change-safety/verify/orient; FULL adds setup/build/admin.
    //    `ADEN_MCP_SURFACE=essential|standard|full` (legacy `ADEN_MCP_FULL=1` = full)
    //    selects the level; every tool stays callable by name regardless. The slice
    //    is kept grouped Essential-first below purely for readability. ──
    ToolSpec {
        name: "grep",
        title: "Search code (structure-aware)",
        description: "Search code by content — use this INSTEAD OF running grep/ripgrep/cat/head/tail through Bash or using the Read tool on whole files: every hit is tagged with the name of the symbol that encloses it, so you skip the follow-up 'which function is this in?' step. e.g. grep(pattern=\"fn authenticate\"). Pass that symbol name to `locate` for its anchor, then feed the anchor to `asm`/`query`. Auto-reindexes changed files first; no setup or `gen` call needed.",
        args: &[
            ("pattern", "string"),
            ("path", "string"),
            ("regex", "boolean"),
            ("ignore_case", "boolean"),
            ("symbol_only", "boolean"),
            ("limit", "integer"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "understand",
        title: "Understand a symbol",
        description: "Before you READ or change a symbol, reach for this FIRST (not a manual cat/head/Read): resolves the name to its best-matching anchor (exact match preferred), shows its definition location, lists backlinks (callers/references) and downstream impact, and assembles a context block — one-shot comprehension AND blast-radius check. This is the thing plain grep and embeddings cannot give you. e.g. understand(symbol=\"MergeProposal\"). Replaces the manual locate → query --backlinks → query --impact → asm chain.",
        args: &[
            ("symbol", "string"),
            ("path", "string"),
            ("budget", "integer"),
            ("json", "boolean"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "ask",
        title: "Ask about the codebase",
        description: "First move when entering an unfamiliar area: ask a natural-language question INSTEAD OF grepping, cat-ing, or Read-ing files yourself — routes to the most relevant anchor and returns its assembled graph NEIGHBORHOOD (the symbol plus its connected context under a token budget), not just a text snippet. e.g. ask(question=\"where is auth enforced?\"). Auto-reindexes changed files first; no setup or `gen` call needed.",
        // `path` is a CLI positional ([DIR], second after QUESTION) — it was
        // missing here historically, not unsupported. Declaration order matters:
        // positionals are emitted in spec order, so question must precede path.
        args: &[
            ("question", "string"),
            ("path", "string"),
            ("budget", "integer"),
            ("from", "string"),
            ("model", "string"),
            ("intent", "string"),
            ("depth", "integer"),
            ("edge_types", "string"),
            ("strict", "boolean"),
            ("explain", "boolean"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "locate",
        title: "Locate a symbol",
        description: "Find a symbol's definition and call sites, returning its anchor — feed that anchor into `asm`/`query`. Use this INSTEAD OF grepping for a function name through Bash. e.g. locate(symbol=\"propose\"). Auto-reindexes changed files first; no setup needed. For JSON output pass format=json.",
        args: &[
            ("symbol", "string"),
            ("caller_of", "string"),
            ("path", "string"),
            ("limit", "integer"),
            ("show_context", "integer"),
            ("format", "string"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "asm",
        title: "Assemble context",
        description: "Assemble a token-dense context prompt for an anchor (pass it via `from`; resolve a symbol name to its anchor with `locate` first): walks the graph from that node under a token budget and returns just the relevant neighborhood INSTEAD OF you reading whole files. e.g. asm(from=\"fn-propose\"). Auto-reindexes changed files first; no setup needed.",
        args: &[
            ("from", "string"),
            ("path", "string"),
            ("depth", "integer"),
            ("budget", "integer"),
            ("edge_types", "string"),
            ("format", "string"),
            ("inspect", "boolean"),
            ("out", "string"),
            ("include_tag", "string"),
            ("exclude_tag", "string"),
            ("set_attr", "string"),
            ("silent", "boolean"),
            ("auto", "boolean"),
            ("strict", "boolean"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "query",
        title: "Query the graph",
        description: "Traverse the knowledge graph from an anchor (pass it via `from`; resolve a symbol name with `locate` first) and emit JSON. Use backlinks=<anchor> for blast radius (what references a symbol) or impact=<anchor> for downstream reach — answers 'what breaks if I change this?', which plain grep cannot. e.g. query(backlinks=\"fn-authenticate\"). Auto-reindexes changed files first; no setup needed.",
        args: &[
            ("path", "string"),
            ("from", "string"),
            ("edge_type", "string"),
            ("depth", "integer"),
            ("backlinks", "string"),
            ("impact", "string"),
            ("format", "string"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "check",
        title: "Validate the graph",
        description: "Validate the graph and gate CI: flags unresolved <<refs>>, circular includes, orphan anchors, typed-edge violations, stale source hashes, and incomplete contracts. severity=Suggest|Warn|Forbid sets the fail threshold and exits non-zero past it. For duplicate-anchor detection and a 0-100 health score, use `diagnose`.",
        args: &[("path", "string"), ("severity", "string")],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "search",
        title: "Keyword search (BM25)",
        description: "Full-text keyword (BM25) search across the indexed graph; returns matching anchors to feed into `asm`/`query`. Use `grep` instead when you want content matches tagged by their enclosing symbol. Auto-reindexes changed files first; no setup needed.",
        // query (positional) must precede path (positional [DIR]) — spec order
        // is emission order.
        args: &[
            ("query", "string"),
            ("path", "string"),
            ("limit", "integer"),
            ("offset", "integer"),
            ("doc_type", "string"),
            ("semantics", "boolean"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "communities",
        title: "Find code clusters",
        description: "Detect functional communities — clusters of symbols that call/use each \
                      other densely (modularity community detection), independent of the \
                      directory layout. Good for orienting in a codebase. `min_size` filters \
                      out small clusters (default 2).",
        args: &[
            ("min_size", "integer"),
            ("limit", "integer"),
            ("resolution", "number"),
            ("path", "string"),
        ],
        effect: Effect::Read,
    },
    // A read/export tool: viz is the non-interactive graph-slice exporter (text
    // Mermaid/DOT/AsciiDoc/JSON), so it is MCP-suitable — unlike its interactive
    // sibling `view` (browser, long-running) which stays off the surface.
    ToolSpec {
        name: "viz",
        title: "Export graph diagram",
        description: "Export a graph slice as a text diagram (Mermaid/DOT/AsciiDoc/JSON) for docs, \
                      PRs, or CI. mode=blast (dependents at risk if the anchor changes, default) | \
                      reach (dependencies it relies on) | connectivity (both directions) | \
                      communities (clusters; anchor not needed). `scope` restricts to a \
                      project-relative subtree (e.g. net/ on a huge repo); `resolution` is the \
                      community-detection gamma (higher = finer clusters). Non-interactive sibling \
                      of the browser `view` command.",
        // Both positionals: anchor must precede path (spec order is emission
        // order, matching `aden viz [ANCHOR] [DIR]`).
        args: &[
            ("anchor", "string"),
            ("mode", "string"),
            ("depth", "integer"),
            ("format", "string"),
            ("full", "boolean"),
            ("scope", "string"),
            ("resolution", "number"),
            ("path", "string"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "impact-diff",
        title: "Diff blast radius",
        description: "Map a git diff to the symbols it touches and report the blast radius \
                      (transitive dependents at risk) before committing. `since` diffs against a ref \
                      (e.g. HEAD~1, main); `staged` analyzes staged changes; default is the \
                      working tree.",
        args: &[
            ("since", "string"),
            ("staged", "boolean"),
            ("scope", "string"),
            ("path", "string"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "list",
        title: "List anchors",
        description: "List the anchors (symbols/docs) aden has indexed — a quick inventory of what the graph knows about the project. `filter` narrows by substring, `verbose` adds node types and locations. Use to discover entry points or confirm a symbol was indexed before you `locate`/`understand` it.",
        args: &[
            ("path", "string"),
            ("filter", "string"),
            ("limit", "integer"),
            ("verbose", "boolean"),
            ("semantics", "boolean"),
            ("offset", "integer"),
            ("unlimited", "boolean"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "ready",
        title: "Pre-commit gate",
        description: "Fast pre-commit gate — gen + lint + check + heal drift scan + audit. Aden-only, no external tool dependencies. Use before every commit. Prefer over ci-check for local dev loops.",
        args: &[("path", "string"), ("fix", "boolean")],
        effect: Effect::Mutate,
    },
    ToolSpec {
        name: "lint",
        title: "Lint source",
        description: "Lint source files. Use dead_code=true to flag symbols with no incoming graph edges (Function/Type nodes with zero callers). Conservative by default — skips entry points and public API; set include_public=true to widen.",
        args: &[
            ("path", "string"),
            ("severity", "string"),
            ("fix", "boolean"),
            ("json", "boolean"),
            ("unlimited", "boolean"),
            ("dead_code", "boolean"),
            ("include_public", "boolean"),
        ],
        effect: Effect::Mutate,
    },
    ToolSpec {
        name: "test",
        title: "Run tests",
        description: "Discover and run tests.",
        args: &[
            ("path", "string"),
            ("scope", "string"),
            ("filter", "string"),
            ("list", "boolean"),
        ],
        effect: Effect::Mutate,
    },
    ToolSpec {
        name: "heal",
        title: "Heal doc drift",
        description: "Self-healing documentation engine: scan for drift, propose patches, apply reviewed changes.",
        args: &[
            ("path", "string"),
            ("fix", "boolean"),
            ("gc", "boolean"),
            ("propose", "boolean"),
            ("since", "string"),
            ("apply", "string"),
            // NOTE: `--watch` is intentionally NOT exposed over MCP — it is a
            // long-running daemon that always trips the request/response
            // timeout. (It is still confined defensively in `confine_path_args`
            // in case a raw client smuggles the key.) Exempted on the CLI side
            // in the mcp_flag_parity test's REVERSE_EXEMPT.
        ],
        effect: Effect::Mutate,
    },
    ToolSpec {
        name: "status",
        title: "Health dashboard",
        description: "Read-only one-glance dashboard: heal-drift health score plus an orphan breakdown (same classifier as `check`). No fixes, no deep scan — a quick pulse before deciding whether to run `diagnose`/`ready`.",
        args: &[("path", "string")],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "gen",
        title: "Index the project",
        description: "Incrementally compile source into the per-user store (store-first: never writes the working tree). A directory indexes the whole project; a single file re-indexes just that file. For a clean cache-clearing rebuild use `regen`; to also prune deleted symbols use `sync`.",
        args: &[
            ("path", "string"),
            ("auto", "boolean"),
            ("quiet", "boolean"),
            ("propose", "boolean"),
            ("force_regen", "boolean"),
        ],
        effect: Effect::Rebuild,
    },
    ToolSpec {
        name: "sync",
        title: "Reconcile the store",
        description: "Reconcile the store — gen + check + heal with gc (prunes deleted symbols). Use after large merges or file deletions, NOT as a routine pre-commit step (use `ready` for that). Pass no_gc=true to skip garbage-collection.",
        args: &[("path", "string"), ("no_gc", "boolean")],
        effect: Effect::Mutate,
    },
    ToolSpec {
        name: "audit",
        title: "Security audit",
        description: "OWASP-aligned security audit: scan source for vulnerabilities.",
        args: &[
            ("path", "string"),
            ("lang", "string"),
            ("format", "string"),
            ("strict", "boolean"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "ci-check",
        title: "CI gate suite",
        description: "Full CI gate suite: aden check, project tests, aden lint, secret scan, attribution check, OWASP audit, merge-conflict-marker scan, insecure-protocol check, cargo clippy, cargo audit, contract freshness. Use before push to remote. For local dev use `ready` instead.",
        args: &[("path", "string")],
        effect: Effect::Mutate,
    },
    ToolSpec {
        name: "diagnose",
        title: "Diagnose graph health",
        description: "Deterministic structural scan of the graph: stale refs, duplicate anchors, invalid edges, orphans, circular includes, missing source files; emits a 0-100 health score (format=json for machine output). Overlaps `check` but additionally flags duplicate anchors and low-confidence nodes and reports a score; read-only — finds nothing about your environment (that's `doctor`).",
        args: &[("path", "string"), ("format", "string")],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "regen",
        title: "Full rebuild",
        description: "Full from-scratch rebuild: clears the gen/graph caches and (unless $ADEN_STORE is pinned/shared) the per-user store, then regenerates and prunes stale anchors. NOT an alias for `gen` — use after renames/deletions leave stale anchors or after corruption; for routine incremental indexing use `gen`.",
        args: &[("path", "string")],
        effect: Effect::Rebuild,
    },
    // ── FULL-tier: setup, build/store, and admin tools (plus a few superseded by
    //    an Essential tool, e.g. `search` → `grep`). Listed only at
    //    ADEN_MCP_SURFACE=full; always callable by name. ──
    ToolSpec {
        name: "new",
        title: "New project",
        description: "Create a new project from a language template.",
        args: &[("name", "string"), ("lang", "string"), ("path", "string")],
        effect: Effect::Mutate,
    },
    ToolSpec {
        name: "init",
        title: "Init workspace",
        description: "Scaffold .agent/ workspace and templates.",
        args: &[
            ("path", "string"),
            ("with_secure_refs", "boolean"),
            ("agents_md", "boolean"),
        ],
        effect: Effect::Mutate,
    },
    ToolSpec {
        name: "kickoff",
        title: "Kickoff document",
        description: "Create a structured kickoff document.",
        args: &[
            ("name", "string"),
            ("interactive", "boolean"),
            ("path", "string"),
        ],
        effect: Effect::Mutate,
    },
    ToolSpec {
        name: "workflow",
        title: "Instantiate template",
        description: "Instantiate templates with substitutions.",
        args: &[
            ("template", "string"),
            ("out", "string"),
            ("from", "string"),
            ("path", "string"),
        ],
        effect: Effect::Mutate,
    },
    ToolSpec {
        name: "session",
        title: "Log session entry",
        description: "Append entry to .agent/session.adoc.",
        args: &[
            ("agent_id", "string"),
            ("task", "string"),
            ("files", "string"),
            ("status", "string"),
            ("path", "string"),
        ],
        effect: Effect::Mutate,
    },
    ToolSpec {
        name: "review",
        title: "Review heal proposals",
        description: "Semantic review of pending heal proposals. Only meaningful after `heal propose=true` has written proposals.",
        args: &[
            ("path", "string"),
            ("since", "string"),
            ("budget", "integer"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "complete",
        title: "Find undocumented contracts",
        description: "List contracts missing required documentation. Reports only — automatic LLM filling is NOT implemented; --model just previews the fill prompt. For drift in existing docs use `heal`, not this.",
        args: &[
            ("path", "string"),
            ("dry_run", "boolean"),
            ("model", "string"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "query-adq",
        title: "Run ADQ script",
        description: "Execute an Aden Query (.adq) script — multi-step filtered graph traversal (node/incoming/outgoing/where) beyond what `query` expresses in one call. For simple backlinks/impact use `query`.",
        args: &[("script", "string"), ("path", "string")],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "doctor",
        title: "Check environment",
        description: "Probe the host environment, NOT the graph: git, language toolchains (rustc/cargo, node, python, go), signing keys. Use when a command fails for environmental reasons (missing tool/binary). For graph/content problems use `diagnose`; for reference errors use `check`.",
        args: &[("path", "string")],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "licenses",
        title: "Generate attributions",
        description: "Generate third-party dependency attribution.",
        args: &[("path", "string"), ("out", "string"), ("full", "boolean")],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "federation",
        title: "Manage federation",
        description: "Manage a multi-repo workspace. action is a subcommand: list, add, remove, config. Over MCP only list/config run (add/remove need a path/name the bridge can't pass — use the CLI). Operates on the federation manifest only; it does not index or query code.",
        args: &[("action", "string")],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "emergency",
        title: "Downgrade policies",
        description: "Downgrade Forbid policies to Warn with justification.",
        args: &[("reason", "string"), ("path", "string"), ("ttl", "string")],
        effect: Effect::Mutate,
    },
    ToolSpec {
        name: "mcp",
        title: "Manage MCP integration",
        description: "MCP (Model Context Protocol) integration management. action is a subcommand: install, uninstall, list. Over MCP only `list` runs (install/uninstall are a one-time terminal setup step).",
        args: &[("action", "string")],
        effect: Effect::Read,
    },
];

// ── ServerHandler impl ────────────────────────────────────────

impl ServerHandler for AdenMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("aden-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(SERVER_INSTRUCTIONS)
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        TOOLS.iter().find(|t| t.name == name).map(tool_from_spec)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        // Default to the ESSENTIAL surface to keep the per-session registry slim;
        // ADEN_MCP_SURFACE=standard|full (or the legacy ADEN_MCP_FULL=1) widens it.
        // Tools below the requested level stay callable by name via `call_tool`, so
        // nothing is ever unreachable.
        let level = requested_surface();
        let tools: Vec<Tool> = TOOLS
            .iter()
            .filter(|t| tool_tier(t.name) <= level)
            .map(tool_from_spec)
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let tool_name = request.name.as_ref();
        let args = request.arguments.unwrap_or_default();

        // Validate tool exists
        let spec = TOOLS.iter().find(|t| t.name == tool_name).ok_or_else(|| {
            McpError::invalid_params(format!("unknown tool: {}", tool_name), None)
        })?;

        // SECURITY (audit HIGH-1): confine any caller-supplied filesystem path
        // to the server's project_dir before shelling out. The CLI does not
        // confine paths (find_project_root will happily resolve `/etc`), so an
        // MCP client could otherwise read or write arbitrary host files via the
        // `path` argument. Reject out-of-tree paths at the boundary.
        if let Err(e) = confine_path_args(tool_name, &args, &self.project_dir) {
            return Ok(CallToolResult::error(vec![Content::text(e)]));
        }

        // `locate` needs at least one of `symbol`/`caller_of`; neither can be
        // expressed as a JSON-schema `required` (that is an AND). Validate the
        // "at least one of" constraint here so an empty call fails fast with a
        // clear message instead of shelling out to a CLI usage error.
        if let Err(e) = validate_args(tool_name, &args) {
            return Ok(CallToolResult::error(vec![Content::text(e)]));
        }

        // Build CLI args: `aden <name> [--flag|--key value] [extra] -- [positional]`.
        // Read tools print terminal chrome (truncation footers, banners) by
        // default; request machine-readable JSON so the agent receives a
        // structured envelope instead. These extra flags must be emitted with
        // the other flags — BEFORE the `--` terminator — or clap would treat
        // them as positional data.
        let cmd_args = build_cli_args(spec, &args, structured_output_flags(spec.name));

        // Run
        let output = run_aden_command(
            &self.project_dir,
            spec.name,
            &cmd_args.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        )
        .await;

        match output {
            Ok(clean) => Ok(CallToolResult::success(vec![Content::text(clean)])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
        }
    }
}

// ── Path confinement ────────────────────────────────────────

/// Reject any caller-supplied `path` argument that resolves outside
/// `project_dir`. Returns `Err(message)` if a path escapes the project root.
///
/// Confinement is enforced HERE, at the MCP boundary, rather than in the CLI:
/// the CLI is also used directly by trusted operators who may legitimately
/// point it anywhere, but an MCP client is untrusted and must stay sandboxed
/// to the project the server was launched for.
fn confine_path_args(
    tool: &str,
    args: &serde_json::Map<String, serde_json::Value>,
    project_dir: &Path,
) -> Result<(), String> {
    let root = canonical_or_self(project_dir);
    // Every caller-supplied argument that names a filesystem location must be
    // confined — not just `path`. Some tools also take a path via `out`
    // (asm/workflow write target) or `from` (workflow source file). NOTE:
    // `from` is a filesystem path only for `workflow`; for asm/ask/query it is
    // a graph ANCHOR, not a path, so it must NOT be confined there.
    let path_args: &[&str] = match tool {
        "asm" => &["path", "out"],
        "workflow" => &["path", "out", "from"],
        // `heal --watch <DIR>` names a directory to monitor; confine it so a
        // client cannot point the watcher at an out-of-tree path like `/etc`.
        "heal" => &["path", "watch"],
        // `licenses --out <FILE>` is a write target.
        "licenses" => &["path", "out"],
        _ => &["path"],
    };
    for key in path_args {
        let Some(raw) = args.get(*key).and_then(|v| v.as_str()) else {
            continue;
        };
        if raw.is_empty() {
            continue;
        }
        let p = Path::new(raw);
        let candidate = if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        };
        let resolved = resolve_existing_prefix(&candidate);
        if !resolved.starts_with(&root) {
            return Err(format!(
                "{} argument '{}' resolves outside the project directory '{}' and is \
                 refused (the MCP server is confined to its project root)",
                key,
                raw,
                root.display()
            ));
        }
    }
    Ok(())
}

/// Canonicalize `p`, falling back to a lexically-normalized form if it does not
/// exist yet (so a not-yet-created target like `aden new`'s dir still checks).
fn canonical_or_self(p: &Path) -> std::path::PathBuf {
    p.canonicalize().unwrap_or_else(|_| normalize_lexical(p))
}

/// Canonicalize the deepest existing ancestor of `p`, then re-append the
/// remaining (not-yet-existing) components — and lexically resolve any `..` so
/// a non-existent path cannot smuggle traversal past the containment check.
fn resolve_existing_prefix(p: &Path) -> std::path::PathBuf {
    let mut ancestor = p;
    loop {
        if let Ok(c) = ancestor.canonicalize() {
            let rest = p.strip_prefix(ancestor).unwrap_or(Path::new(""));
            return normalize_lexical(&c.join(rest));
        }
        match ancestor.parent() {
            Some(par) => ancestor = par,
            None => return normalize_lexical(p),
        }
    }
}

/// Lexically resolve `.`/`..` without touching the filesystem.
fn normalize_lexical(p: &Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ── CLI bridge ──────────────────────────────────────────────

/// Resolve the `aden` CLI binary.
///
/// Order: `ADEN_BIN` env override → a sibling of the running `aden-mcp`
/// executable (the usual install layout) → bare `aden` on `PATH`. Hardcoding
/// `"aden"` breaks whenever the MCP server runs from a context where the CLI
/// is installed but not on `PATH` (a very common MCP-client launch setup).
fn resolve_aden_binary() -> std::ffi::OsString {
    if let Some(explicit) = std::env::var_os("ADEN_BIN") {
        return explicit;
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join(if cfg!(windows) { "aden.exe" } else { "aden" });
        if sibling.is_file() {
            return sibling.into_os_string();
        }
    }
    std::ffi::OsString::from("aden")
}

/// Hard ceiling on how long a single shelled-out `aden` invocation may run.
/// MCP tool calls are request/response, so a tool that never returns (a
/// `watch` daemon, `heal --watch`, or a runaway `gen` on a huge untrusted
/// repo) would otherwise block the JSON-RPC stream indefinitely. Time out
/// instead and surface a clean error to the caller.
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Index-rebuild tools whose stdout is progress chrome ("Generated N nodes",
/// "Emitted N edges", "INFO: …") rather than an answer. ONLY these get their
/// stdout chrome-stripped for the MCP channel. Every other tool — especially
/// the diagnostics (`check`/`status`/`diagnose`) whose findings ARE printed as
/// `INFO:` lines — returns stdout verbatim. Blanket `INFO:` stripping was the
/// bug that made `check` come back empty over MCP: every line it prints is an
/// `INFO:` line, so the filter ate the entire result.
fn strips_index_chrome(tool: &str) -> bool {
    matches!(tool, "gen" | "regen")
}

/// Post-process a successful command's stdout for return over MCP, per the
/// `strips_index_chrome` policy. Line-ending normalization (join with `\n`,
/// trailing newline dropped) is identical for both paths; only the filter
/// differs, so non-index tools are returned unchanged save normalization.
fn clean_stdout(tool: &str, raw: &str) -> String {
    let strip = strips_index_chrome(tool);
    raw.lines()
        .filter(|l| {
            if !strip {
                return true;
            }
            let t = l.trim_start();
            !(t.starts_with("INFO:") || t.starts_with("Generated") || t.starts_with("Emitted"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn run_aden_command(project_dir: &Path, tool: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new(resolve_aden_binary());
    cmd.args(args).current_dir(project_dir);

    // SECURITY: do not leak host env (model keys, tokens, etc.) to the child `aden`.
    // Allowlist only what is required for basic operation (ADEN_* for config,
    // PATH/HOME for binaries and dirs, USER for some tools).
    cmd.env_clear();
    for (k, v) in std::env::vars() {
        if k.starts_with("ADEN_") || matches!(k.as_str(), "PATH" | "HOME" | "USER" | "SHELL") {
            cmd.env(&k, &v);
        }
    }
    // Read tools must not silently `gen` — the host may be running `aden ready`
    // or another writer. MCP is read-mostly; callers run `gen` explicitly.
    cmd.env("ADEN_SKIP_AUTO_GEN", "1");

    let child = cmd.output();

    let output = match tokio::time::timeout(COMMAND_TIMEOUT, child).await {
        Ok(result) => result.map_err(|e| format!("failed to run aden: {}", e))?,
        Err(_) => {
            return Err(format!(
                "aden command timed out after {}s. Long-running tools like `watch` \
                 are not usable over MCP (each tool call is request/response); run \
                 them from a terminal instead.",
                COMMAND_TIMEOUT.as_secs()
            ));
        }
    };

    if output.status.success() {
        let raw = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(clean_stdout(tool, &raw))
    } else {
        let mut err = String::from_utf8_lossy(&output.stderr).into_owned();
        let out = String::from_utf8_lossy(&output.stdout);
        if !out.trim().is_empty() {
            err.push_str(&format!("\n(stdout): {}", out));
        }
        Err(sanitize_error(&err))
    }
}

/// Sanitize CLI stderr before returning it to an (untrusted) MCP caller.
///
/// Raw stderr can leak host-specific detail — absolute filesystem paths,
/// `RUST_BACKTRACE` frames, addresses — that an MCP client has no business
/// seeing. Drop panic backtrace noise, redact absolute paths, collapse blank
/// runs, and cap the total length so a runaway error can't flood the channel.
fn sanitize_error(raw: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in raw.lines() {
        let t = line.trim_end();
        let lt = t.trim_start();
        // Drop backtrace frames and the backtrace hint banner entirely.
        if lt.starts_with("note: run with")
            || lt.starts_with("note: Some details")
            || lt.starts_with("stack backtrace:")
            || lt.starts_with("at ")
            || (lt.chars().take_while(|c| c.is_ascii_digit()).count() > 0
                && lt.contains(": ")
                && lt
                    .split_once(": ")
                    .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())))
        {
            continue;
        }
        lines.push(redact_abs_paths(t));
    }
    let mut msg = lines.join("\n");
    msg = msg.trim().to_string();
    if msg.is_empty() {
        msg = "aden command failed (no error output)".to_string();
    }
    // Cap length so a pathological error cannot flood the JSON-RPC stream.
    const MAX: usize = 4000;
    if msg.len() > MAX {
        let mut end = MAX;
        while !msg.is_char_boundary(end) {
            end -= 1;
        }
        msg.truncate(end);
        msg.push_str("\n… (error output truncated)");
    }
    msg
}

/// Replace absolute filesystem paths in `s` with a `<path>` placeholder so host
/// directory layout does not leak to the MCP client.
fn redact_abs_paths(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        // Unix absolute path: a `/` that starts a token (preceded by start or
        // whitespace/quote/paren) and is followed by a path-like char.
        let token_start = i == 0
            || s[..i]
                .chars()
                .next_back()
                .is_some_and(|p| p.is_whitespace() || matches!(p, '\'' | '"' | '(' | '[' | '='));
        if c == '/' && token_start && s[i + 1..].starts_with(|n: char| n.is_alphanumeric()) {
            // consume the rest of the path token
            while let Some(&(_, nc)) = chars.peek() {
                if nc.is_whitespace() || matches!(nc, '\'' | '"' | ')' | ']' | ',') {
                    break;
                }
                chars.next();
            }
            out.push_str("<path>");
            continue;
        }
        out.push(c);
    }
    out
}

// ── Public serve ────────────────────────────────────────────

pub async fn serve(project_dir: PathBuf) -> anyhow::Result<()> {
    let server = AdenMcpServer::new(project_dir);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tools_table_is_non_empty() {
        assert!(!TOOLS.is_empty(), "TOOLS table should not be empty");
    }

    #[test]
    fn removed_tools_are_absent() {
        // `watch` always times out over MCP; `suggest` was a stale recommender.
        for dead in ["watch", "suggest"] {
            assert!(
                !TOOLS.iter().any(|t| t.name == dead),
                "{dead} must not be on the MCP surface"
            );
        }
    }

    #[test]
    fn essential_surface_is_the_navigation_loop() {
        // The default (Essential) surface, in presentation order, is exactly the
        // find -> comprehend -> blast-radius loop — nothing more.
        let essential: Vec<&str> = TOOLS
            .iter()
            .map(|t| t.name)
            .filter(|n| tool_tier(n) == Tier::Essential)
            .collect();
        assert_eq!(
            essential,
            ["grep", "understand", "ask", "locate", "asm", "query"],
            "Essential surface drifted; got: {essential:?}"
        );
        // Change-safety / verify / orient tools are Standard, NOT in the default.
        for t in [
            "check",
            "impact-diff",
            "test",
            "lint",
            "audit",
            "diagnose",
            "list",
        ] {
            assert_eq!(tool_tier(t), Tier::Standard, "{t} should be Standard tier");
        }
    }

    #[test]
    fn surface_tiers_widen_essential_to_standard_to_full() {
        let count = |lvl: Tier| TOOLS.iter().filter(|t| tool_tier(t.name) <= lvl).count();
        let (e, s, f) = (
            count(Tier::Essential),
            count(Tier::Standard),
            count(Tier::Full),
        );
        assert_eq!(e, ESSENTIAL_TOOLS.len(), "essential surface size");
        assert_eq!(
            s,
            ESSENTIAL_TOOLS.len() + STANDARD_TOOLS.len(),
            "standard = essential + standard list"
        );
        assert_eq!(f, TOOLS.len(), "full surface is every registered tool");
        assert!(e < s && s < f, "tiers must strictly widen: {e} < {s} < {f}");
    }

    #[test]
    fn tier_lists_are_valid_and_disjoint() {
        let names: std::collections::HashSet<&str> = TOOLS.iter().map(|t| t.name).collect();
        for n in ESSENTIAL_TOOLS.iter().chain(STANDARD_TOOLS) {
            assert!(names.contains(n), "{n} is tiered but not a registered tool");
        }
        for n in ESSENTIAL_TOOLS {
            assert!(
                !STANDARD_TOOLS.contains(n),
                "{n} is in both Essential and Standard"
            );
        }
    }

    #[test]
    fn parse_full_toggle() {
        for on in ["1", "true", "TRUE", " full ", "yes", "Yes"] {
            assert!(parse_full(Some(on)), "{on:?} should enable full surface");
        }
        for off in ["0", "false", "", "no", "core", "2"] {
            assert!(!parse_full(Some(off)), "{off:?} should not enable full");
        }
        assert!(!parse_full(None), "unset must not force the full surface");
    }

    #[test]
    fn test_get_tool_finds_all_known_tools() {
        let server = AdenMcpServer::new(PathBuf::from("."));
        for spec in TOOLS.iter() {
            let tool = server.get_tool(spec.name);
            assert!(
                tool.is_some(),
                "get_tool should return Some for known tool: {}",
                spec.name
            );
            let tool = tool.unwrap();
            assert_eq!(tool.name.as_ref(), spec.name);
            assert_eq!(
                tool.description.as_ref().map(|c| c.as_ref()),
                Some(spec.description)
            );
        }
    }

    #[test]
    fn test_get_tool_returns_none_for_unknown() {
        let server = AdenMcpServer::new(PathBuf::from("."));
        assert!(server.get_tool("nonexistent").is_none());
    }

    fn spec(name: &str) -> &'static ToolSpec {
        TOOLS.iter().find(|t| t.name == name).unwrap()
    }

    #[test]
    fn snake_case_args_become_kebab_flags() {
        // Regression: `edge_types` must map to `--edge-types`, not `--edge_types`
        // (clap derives kebab-case long flags), otherwise the CLI rejects it.
        let mut args = serde_json::Map::new();
        args.insert("edge_types".into(), serde_json::json!("uses,calls"));
        let out = build_cli_args(spec("asm"), &args, &[]);
        assert_eq!(out, vec!["asm", "--edge-types", "uses,calls"]);
    }

    #[test]
    fn positional_and_boolean_args_render_correctly() {
        let mut args = serde_json::Map::new();
        args.insert("question".into(), serde_json::json!("how does X work"));
        args.insert("budget".into(), serde_json::json!(2048));
        let out = build_cli_args(spec("ask"), &args, &[]);
        // question is positional (no flag); budget is a value flag.
        assert!(out.contains(&"how does X work".to_string()));
        assert!(!out.contains(&"--question".to_string()));
        assert_eq!(out[0], "ask");

        let mut g = serde_json::Map::new();
        g.insert("auto".into(), serde_json::json!(true));
        g.insert("path".into(), serde_json::json!("."));
        let out = build_cli_args(spec("gen"), &g, &[]);
        assert!(out.contains(&"--auto".to_string())); // boolean flag, no value
        assert!(out.contains(&".".to_string())); // path positional
    }

    #[test]
    fn positionals_follow_a_double_dash_terminator() {
        // Security (MEDIUM-1): a leading-dash value must not smuggle a CLI flag.
        // A `--` terminator is emitted before the first value positional, so
        // clap treats `--fix` as data, not the heal --fix boolean.
        let mut args = serde_json::Map::new();
        args.insert("path".into(), serde_json::json!("--fix"));
        let out = build_cli_args(spec("heal"), &args, &[]);
        let dd = out
            .iter()
            .position(|a| a == "--")
            .expect("-- terminator present");
        let val = out.iter().position(|a| a == "--fix").unwrap();
        assert!(dd < val, "the -- must precede the smuggled value: {out:?}");
    }

    #[test]
    fn subcommand_dispatch_has_no_double_dash() {
        // federation/mcp `action` is a clap SUBCOMMAND verb — it must stay a
        // bare token with NO `--` before it, or clap can't dispatch it.
        let mut args = serde_json::Map::new();
        args.insert("action".into(), serde_json::json!("list"));
        let out = build_cli_args(spec("federation"), &args, &[]);
        assert_eq!(out, vec!["federation", "list"], "no -- before a subcommand");
    }

    #[test]
    fn confine_path_rejects_outside_project() {
        let proj = std::env::temp_dir();
        // Absolute escape via `path`.
        let mut esc = serde_json::Map::new();
        esc.insert("path".into(), serde_json::json!("/etc"));
        assert!(
            confine_path_args("grep", &esc, &proj).is_err(),
            "/etc must be refused"
        );
        // `..` traversal escape.
        let mut trav = serde_json::Map::new();
        trav.insert("path".into(), serde_json::json!("../../../../etc"));
        assert!(
            confine_path_args("grep", &trav, &proj).is_err(),
            ".. escape must be refused"
        );
        // In-tree relative path is allowed.
        let mut ok = serde_json::Map::new();
        ok.insert("path".into(), serde_json::json!("."));
        assert!(
            confine_path_args("grep", &ok, &proj).is_ok(),
            "'.' must be allowed"
        );
        // No path arg → nothing to confine.
        assert!(confine_path_args("grep", &serde_json::Map::new(), &proj).is_ok());
    }

    #[test]
    fn confine_checks_out_and_from_not_just_path() {
        // Regression: confinement originally only guarded `path`, so asm/workflow
        // could write outside the project via `out` (or read via workflow `from`).
        let proj = std::env::temp_dir();
        // asm --out escaping the root must be refused.
        let mut asm_out = serde_json::Map::new();
        asm_out.insert("from".into(), serde_json::json!("mod-x")); // anchor, not a path — must be ignored
        asm_out.insert("out".into(), serde_json::json!("/etc/aden-pwned"));
        assert!(
            confine_path_args("asm", &asm_out, &proj).is_err(),
            "asm --out /etc must be refused"
        );
        // workflow `from` (a real file path) escaping must be refused.
        let mut wf = serde_json::Map::new();
        wf.insert("from".into(), serde_json::json!("../../../../etc/passwd"));
        assert!(
            confine_path_args("workflow", &wf, &proj).is_err(),
            "workflow --from escape must be refused"
        );
        // asm `from` as an anchor (no path semantics for asm) must NOT trip the check.
        let mut asm_anchor = serde_json::Map::new();
        asm_anchor.insert("from".into(), serde_json::json!("aden://module/x#y"));
        assert!(
            confine_path_args("asm", &asm_anchor, &proj).is_ok(),
            "asm anchor from must be allowed"
        );
    }

    #[test]
    fn grep_requests_structured_json_output() {
        // The MCP injects `--json` for grep so the agent gets the structured
        // envelope, not the human truncation footer.
        let mut args = serde_json::Map::new();
        args.insert("pattern".into(), serde_json::json!("TODO"));
        let cmd = build_cli_args(spec("grep"), &args, structured_output_flags("grep"));
        assert!(
            cmd.contains(&"--json".to_string()),
            "grep must request --json: {cmd:?}"
        );
        assert_eq!(cmd.iter().filter(|a| *a == "--json").count(), 1);
        // Critical ordering: --json (a flag) must come BEFORE the `--`
        // terminator, or clap would parse it as a positional. The pattern is
        // a value positional, so a `--` is present.
        let dd = cmd.iter().position(|a| a == "--");
        let js = cmd.iter().position(|a| a == "--json").unwrap();
        if let Some(dd) = dd {
            assert!(js < dd, "--json must precede the -- terminator: {cmd:?}");
        }
    }

    #[test]
    fn non_read_tools_get_no_structured_flags() {
        assert!(structured_output_flags("gen").is_empty());
        assert!(structured_output_flags("status").is_empty());
    }

    #[test]
    fn structured_output_tools_request_json() {
        // Tools with a real JSON envelope must auto-request --json over MCP.
        for t in ["grep", "search", "list", "test"] {
            assert_eq!(
                structured_output_flags(t),
                &["--json"],
                "{t} should request --json"
            );
        }
    }

    #[test]
    fn required_args_surface_in_schema() {
        let server = AdenMcpServer::new(PathBuf::from("."));
        let tool = server.get_tool("ask").unwrap();
        let req = tool
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        assert!(req.iter().any(|v| v == "question"));
        // gen has no required args (path defaults to ".").
        let gen_tool = server.get_tool("gen").unwrap();
        assert!(gen_tool.input_schema.get("required").is_none());
    }

    #[test]
    fn locate_requires_symbol_or_caller_of() {
        // Empty call must be rejected.
        assert!(validate_args("locate", &serde_json::Map::new()).is_err());
        // Either arg alone is sufficient.
        let mut a = serde_json::Map::new();
        a.insert("symbol".into(), serde_json::json!("foo"));
        assert!(validate_args("locate", &a).is_ok());
        let mut b = serde_json::Map::new();
        b.insert("caller_of".into(), serde_json::json!("foo"));
        assert!(validate_args("locate", &b).is_ok());
    }

    #[test]
    fn boolean_args_reject_non_bool_values() {
        let mut a = serde_json::Map::new();
        a.insert("auto".into(), serde_json::json!("true")); // string, not bool
        let err = validate_args("gen", &a).unwrap_err();
        assert!(err.contains("auto") && err.contains("boolean"), "{err}");
        // A real bool passes.
        let mut ok = serde_json::Map::new();
        ok.insert("auto".into(), serde_json::json!(true));
        assert!(validate_args("gen", &ok).is_ok());
    }

    #[test]
    fn schema_rejects_unknown_properties() {
        let server = AdenMcpServer::new(PathBuf::from("."));
        let tool = server.get_tool("gen").unwrap();
        assert_eq!(
            tool.input_schema.get("additionalProperties"),
            Some(&serde_json::json!(false))
        );
    }

    #[test]
    fn renamed_snake_case_args_emit_kebab_flags() {
        // session agent_id → --agent-id; complete dry_run → --dry-run; lint dead_code → --dead-code
        let mut s = serde_json::Map::new();
        s.insert("agent_id".into(), serde_json::json!("a1"));
        s.insert("task".into(), serde_json::json!("t"));
        let out = build_cli_args(spec("session"), &s, &[]);
        assert!(out.contains(&"--agent-id".to_string()) && out.contains(&"a1".to_string()));

        let mut c = serde_json::Map::new();
        c.insert("dry_run".into(), serde_json::json!(true));
        assert!(build_cli_args(spec("complete"), &c, &[]).contains(&"--dry-run".to_string()));

        let mut l = serde_json::Map::new();
        l.insert("dead_code".into(), serde_json::json!(true));
        assert!(build_cli_args(spec("lint"), &l, &[]).contains(&"--dead-code".to_string()));
    }

    #[test]
    fn number_args_render_as_value_flags() {
        // communities --resolution is an f64: both fractional and integral JSON
        // numbers must flow through.
        let mut args = serde_json::Map::new();
        args.insert("resolution".into(), serde_json::json!(1.5));
        let out = build_cli_args(spec("communities"), &args, &[]);
        assert_eq!(out, vec!["communities", "--resolution", "1.5"]);

        let mut int = serde_json::Map::new();
        int.insert("resolution".into(), serde_json::json!(2));
        let out = build_cli_args(spec("communities"), &int, &[]);
        assert_eq!(out, vec!["communities", "--resolution", "2"]);
    }

    #[test]
    fn viz_positionals_emit_anchor_before_path() {
        // `aden viz [ANCHOR] [DIR]` — anchor must be the first positional.
        let mut args = serde_json::Map::new();
        args.insert("anchor".into(), serde_json::json!("build_cli_args"));
        args.insert("path".into(), serde_json::json!("."));
        args.insert("format".into(), serde_json::json!("dot"));
        let out = build_cli_args(spec("viz"), &args, &[]);
        let a = out.iter().position(|x| x == "build_cli_args").unwrap();
        let p = out.iter().position(|x| x == ".").unwrap();
        let dd = out.iter().position(|x| x == "--").unwrap();
        assert!(dd < a && a < p, "expected -- anchor path order: {out:?}");
        assert!(out.contains(&"--format".to_string()));
    }

    #[test]
    fn viz_path_without_anchor_is_rejected() {
        // Two optional positionals: a lone path would bind to the ANCHOR slot.
        let mut args = serde_json::Map::new();
        args.insert("path".into(), serde_json::json!("crates"));
        assert!(validate_args("viz", &args).is_err());
        // anchor alone, or anchor+path, or neither, are all fine.
        assert!(validate_args("viz", &serde_json::Map::new()).is_ok());
        let mut ok = serde_json::Map::new();
        ok.insert("anchor".into(), serde_json::json!("foo"));
        assert!(validate_args("viz", &ok).is_ok());
        ok.insert("path".into(), serde_json::json!("crates"));
        assert!(validate_args("viz", &ok).is_ok());
    }

    #[test]
    fn ask_path_is_second_positional() {
        // ask gained `path` ([DIR] after QUESTION): question first, path second.
        let mut args = serde_json::Map::new();
        args.insert("question".into(), serde_json::json!("what is X"));
        args.insert("path".into(), serde_json::json!("src"));
        args.insert("strict".into(), serde_json::json!(true));
        let out = build_cli_args(spec("ask"), &args, &[]);
        let q = out.iter().position(|x| x == "what is X").unwrap();
        let p = out.iter().position(|x| x == "src").unwrap();
        assert!(q < p, "question must precede path: {out:?}");
        assert!(out.contains(&"--strict".to_string()));
    }

    #[test]
    fn licenses_out_is_confined() {
        let proj = std::env::temp_dir();
        let mut esc = serde_json::Map::new();
        esc.insert("out".into(), serde_json::json!("/etc/aden-pwned"));
        assert!(
            confine_path_args("licenses", &esc, &proj).is_err(),
            "licenses out=/etc must be refused"
        );
    }

    #[test]
    fn heal_watch_dir_is_confined() {
        let proj = std::env::temp_dir();
        let mut esc = serde_json::Map::new();
        esc.insert("watch".into(), serde_json::json!("/etc"));
        assert!(
            confine_path_args("heal", &esc, &proj).is_err(),
            "heal watch=/etc must be refused"
        );
    }

    #[test]
    fn sanitize_error_redacts_paths_and_drops_backtrace() {
        let raw = "thread 'main' panicked at /home/user/secret/foo.rs:10\n\
                   note: run with `RUST_BACKTRACE=1` for a backtrace\n\
                   stack backtrace:\n   0: core::panicking\n   1: aden::main";
        let out = sanitize_error(raw);
        assert!(!out.contains("/home/user/secret"), "abs path leaked: {out}");
        assert!(out.contains("<path>"));
        assert!(!out.contains("stack backtrace"));
        assert!(!out.contains("RUST_BACKTRACE"));
    }

    #[test]
    fn test_tool_schema_has_properties() {
        let server = AdenMcpServer::new(PathBuf::from("."));
        let tool = server.get_tool("gen").unwrap();
        let schema = tool.input_schema.as_ref();
        assert!(schema.contains_key("type"));
        assert!(schema.contains_key("properties"));
        let props = schema.get("properties").unwrap().as_object().unwrap();
        assert!(props.contains_key("path"));
        assert!(props.contains_key("auto"));
    }

    #[test]
    fn clean_stdout_preserves_diagnostic_findings() {
        // `check`/`diagnose`/`status` print findings as `INFO:` lines. Blanket
        // INFO-stripping made `check` come back EMPTY over MCP — guard it.
        let findings = "INFO: All <<refs>> resolve.\nINFO: All contracts complete.";
        assert_eq!(clean_stdout("check", findings), findings);
        assert_eq!(clean_stdout("diagnose", findings), findings);
        assert_eq!(clean_stdout("status", findings), findings);
    }

    #[test]
    fn clean_stdout_strips_index_chrome_for_rebuilds() {
        let raw = "INFO: indexing\nGenerated 10 nodes\nEmitted 5 edges\nstore updated";
        assert_eq!(clean_stdout("gen", raw), "store updated");
        assert_eq!(clean_stdout("regen", raw), "store updated");
    }

    #[test]
    fn clean_stdout_is_verbatim_for_read_tools() {
        // A real result line may legitimately start with "Generated"; it must
        // survive for any non-rebuild tool (regression: over-eager filtering).
        let raw = "Found 1 match\nGenerated config helper";
        assert_eq!(clean_stdout("grep", raw), raw);
    }

    #[test]
    fn heal_watch_is_not_on_the_mcp_surface() {
        // `heal --watch` is a daemon: it always trips the MCP request/response
        // timeout, so it must not be advertised as a callable arg.
        assert!(
            !spec("heal").args.iter().any(|(a, _)| *a == "watch"),
            "heal must not expose `watch` over MCP"
        );
    }

    #[test]
    fn locate_schema_declares_any_of_symbol_or_caller_of() {
        let server = AdenMcpServer::new(PathBuf::from("."));
        let tool = server.get_tool("locate").unwrap();
        let any_of = tool
            .input_schema
            .get("anyOf")
            .and_then(|v| v.as_array())
            .expect("locate schema should declare anyOf");
        let names: Vec<&str> = any_of
            .iter()
            .filter_map(|c| c.get("required"))
            .filter_map(|r| r.as_array())
            .flat_map(|a| a.iter().filter_map(|v| v.as_str()))
            .collect();
        assert!(
            names.contains(&"symbol") && names.contains(&"caller_of"),
            "anyOf should require symbol or caller_of, got {names:?}"
        );
    }

    #[test]
    fn federation_action_enum_constrains_subcommands() {
        let server = AdenMcpServer::new(PathBuf::from("."));
        let tool = server.get_tool("federation").unwrap();
        let vals: Vec<&str> = tool
            .input_schema
            .get("properties")
            .and_then(|p| p.get("action"))
            .and_then(|a| a.get("enum"))
            .and_then(|e| e.as_array())
            .expect("federation.action should be enum-constrained")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            vals.contains(&"list") && vals.contains(&"config"),
            "enum should list valid subcommands, got {vals:?}"
        );
    }

    #[test]
    fn constrained_value_args_are_enum_hinted() {
        // A spread across the constrained-arg classes: severity, mode, format,
        // doc_type. Each must surface its valid set as a JSON-schema enum.
        let server = AdenMcpServer::new(PathBuf::from("."));
        let cases = [
            ("check", "severity", "Forbid"),
            ("viz", "mode", "connectivity"),
            ("asm", "format", "adg"),
            ("search", "doc_type", "use-case"),
        ];
        for (tool, arg, val) in cases {
            let t = server.get_tool(tool).unwrap();
            let en = t
                .input_schema
                .get("properties")
                .and_then(|p| p.get(arg))
                .and_then(|a| a.get("enum"))
                .and_then(|e| e.as_array())
                .unwrap_or_else(|| panic!("{tool}.{arg} should be enum-constrained"));
            assert!(
                en.iter().any(|v| v == val),
                "{tool}.{arg} enum should contain {val}, got {en:?}"
            );
        }
    }
}
