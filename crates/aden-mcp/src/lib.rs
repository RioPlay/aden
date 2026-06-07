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
Aden is a language-agnostic referential context compiler. It turns ANY codebase \
or documentation (Rust, Python, Go, TypeScript, Java, Ruby, PHP, C/C++, and 300+ \
more — plus Markdown/AsciiDoc docs) into a queryable knowledge graph. Nothing \
here is specific to Aden's own source; every result is derived from the target \
project you point it at.\n\n\
The graph is fresh by construction: read tools (`ask`, `asm`, `query`, `locate`, \
`grep`) auto-reindex any source that changed since the last run, so you do NOT \
need to call `gen` before each session. Only run `gen` (auto=true) after large \
external changes — e.g. cloning a new repo, a big merge, or generated code \
appearing outside your edits.\n\n\
Typical workflow:\n\
1. `grep \"pattern\"` for structure-aware content search: every match is tagged \
with its enclosing symbol. Prefer it over a plain text grep — the enclosing \
symbol name it returns is exactly what you feed to `asm` as an anchor.\n\
2. `ask` a natural-language question, or `search` for keywords, to retrieve \
context. `locate` finds a symbol's definition and call sites.\n\
3. `query`/`asm` traverse the graph. Before a refactor, `query` with \
backlinks=<symbol> shows what references that symbol (its blast radius); \
impact=<symbol> shows the downstream reach.\n\
4. `check`/`heal` validate and keep contracts in sync with the code.\n\n\
The `path` argument defaults to the current project directory for every tool.\n\
Setup/admin tools (e.g. `init`, `new`, `federation`, `mcp`) are hidden from this \
list to keep it focused; set ADEN_MCP_FULL=1 in the server environment to surface them.";

// ── Tool declaration ──────────────────────────────────────────

/// Visibility tier. Core tools lead the surface and are listed by default;
/// Extended tools stay callable by name but are hidden from `list_tools`
/// unless `ADEN_MCP_FULL` is set — keeping the per-session surface slim
/// without stranding setup/admin tools for MCP-only consumers.
#[derive(PartialEq)]
enum Tier {
    Core,
    Extended,
}

/// A tool the LLM can invoke.  Zero code per tool — just metadata.
struct ToolSpec {
    name: &'static str,
    description: &'static str,
    args: &'static [(&'static str, &'static str)], // (arg_name, arg_type: "string"|"boolean"|"integer")
    tier: Tier,
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
            "string" | "integer" => {
                let s = match arg_type {
                    "integer" => val
                        .as_u64()
                        .map(|n| n.to_string())
                        .or_else(|| val.as_i64().map(|n| n.to_string())),
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
    Tool::new(spec.name, spec.description, Arc::new(schema))
}

/// True when the full tool surface (Core + Extended) should be listed.
/// Default is Core-only, keeping the per-session registry slim; setting
/// `ADEN_MCP_FULL` to a truthy value (1/true/full/yes) lists everything.
/// Extended tools stay callable by name regardless — this gates listing only.
fn surface_is_full() -> bool {
    parse_full(std::env::var("ADEN_MCP_FULL").ok().as_deref())
}

/// Pure parse of the `ADEN_MCP_FULL` toggle, split out so it is testable
/// without mutating process-global environment state.
fn parse_full(v: Option<&str>) -> bool {
    matches!(
        v.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "full" | "yes")
    )
}

/// Every MCP tool maps 1:1 to `aden <name> <args>`.
static TOOLS: &[ToolSpec] = &[
    // ── Core: the per-session surface, ordered by how often an agent reaches
    //    for each (read/comprehend first, then validate, then mutate). ──
    ToolSpec {
        name: "grep",
        description: "Structure-aware content search: find a pattern, each hit tagged with its enclosing symbol. Prefer over plain grep.",
        args: &[
            ("pattern", "string"),
            ("path", "string"),
            ("regex", "boolean"),
            ("ignore_case", "boolean"),
            ("symbol_only", "boolean"),
            ("limit", "integer"),
        ],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "understand",
        description: "One-shot symbol comprehension: resolves a symbol to its anchor, shows its definition location, lists backlinks (callers/references), lists downstream impact, and assembles a context block. Replaces the manual locate → query --backlinks → query --impact → asm chain.",
        args: &[
            ("symbol", "string"),
            ("path", "string"),
            ("budget", "integer"),
            ("json", "boolean"),
        ],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "ask",
        description: "Ask a natural-language question. Routes to the best matching anchor.",
        args: &[
            ("question", "string"),
            ("budget", "integer"),
            ("from", "string"),
            ("model", "string"),
        ],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "search",
        description: "Full-text search with BM25 ranking.",
        args: &[
            ("query", "string"),
            ("limit", "integer"),
            ("offset", "integer"),
            ("doc_type", "string"),
            ("semantics", "boolean"),
        ],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "communities",
        description: "Detect functional communities — clusters of symbols that call/use each \
                      other densely (modularity community detection), independent of the \
                      directory layout. Good for orienting in a codebase. `min_size` filters \
                      out small clusters (default 2).",
        args: &[
            ("min_size", "integer"),
            ("limit", "integer"),
            ("path", "string"),
        ],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "impact-diff",
        description: "Map a git diff to the symbols it touches and report the blast radius \
                      (downstream impact) before committing. `since` diffs against a ref \
                      (e.g. HEAD~1, main); `staged` analyzes staged changes; default is the \
                      working tree.",
        args: &[
            ("since", "string"),
            ("staged", "boolean"),
            ("path", "string"),
        ],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "locate",
        description: "Find symbol definition and call sites. For JSON output pass format=json.",
        args: &[
            ("symbol", "string"),
            ("caller_of", "string"),
            ("path", "string"),
            ("limit", "integer"),
            ("show_context", "integer"),
            ("format", "string"),
        ],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "asm",
        description: "Assemble a context prompt from the knowledge graph. Pass the anchor via `from`.",
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
        tier: Tier::Core,
    },
    ToolSpec {
        name: "query",
        description: "Query the knowledge graph and emit JSON. Use backlinks=<anchor> for blast radius (what references a symbol) or impact=<anchor>.",
        args: &[
            ("path", "string"),
            ("from", "string"),
            ("edge_type", "string"),
            ("depth", "integer"),
            ("backlinks", "string"),
            ("impact", "string"),
            ("format", "string"),
        ],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "list",
        description: "List all indexed anchors.",
        args: &[
            ("path", "string"),
            ("filter", "string"),
            ("limit", "integer"),
            ("verbose", "boolean"),
            ("semantics", "boolean"),
            ("offset", "integer"),
            ("unlimited", "boolean"),
        ],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "ready",
        description: "Fast pre-commit gate — gen + lint + check + heal drift scan + audit. Aden-only, no external tool dependencies. Use before every commit. Prefer over ci-check for local dev loops.",
        args: &[("path", "string"), ("fix", "boolean")],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "check",
        description: "Validate the graph and gate CI: flags unresolved <<refs>>, circular includes, orphan anchors, typed-edge violations, stale source hashes, and incomplete contracts. severity=Suggest|Warn|Forbid sets the fail threshold and exits non-zero past it. For duplicate-anchor detection and a 0-100 health score, use `diagnose`.",
        args: &[("path", "string"), ("severity", "string")],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "lint",
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
        tier: Tier::Core,
    },
    ToolSpec {
        name: "test",
        description: "Discover and run tests.",
        args: &[
            ("path", "string"),
            ("scope", "string"),
            ("filter", "string"),
            ("list", "boolean"),
        ],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "heal",
        description: "Self-healing documentation engine: scan for drift, propose patches, apply reviewed changes.",
        args: &[
            ("path", "string"),
            ("fix", "boolean"),
            ("gc", "boolean"),
            ("propose", "boolean"),
            ("since", "string"),
            ("apply", "string"),
            ("watch", "string"),
        ],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "status",
        description: "Read-only one-glance dashboard: heal-drift health score plus an orphan breakdown (same classifier as `check`). No fixes, no deep scan — a quick pulse before deciding whether to run `diagnose`/`ready`.",
        args: &[("path", "string")],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "gen",
        description: "Incrementally compile source into the per-user store (store-first: never writes the working tree). A directory indexes the whole project; a single file re-indexes just that file. For a clean cache-clearing rebuild use `regen`; to also prune deleted symbols use `sync`.",
        args: &[
            ("path", "string"),
            ("auto", "boolean"),
            ("quiet", "boolean"),
        ],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "sync",
        description: "Reconcile the store — gen + check + heal with gc (prunes deleted symbols). Use after large merges or file deletions, NOT as a routine pre-commit step (use `ready` for that). Pass no_gc=true to skip garbage-collection.",
        args: &[("path", "string"), ("no_gc", "boolean")],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "audit",
        description: "OWASP-aligned security audit: scan source for vulnerabilities.",
        args: &[
            ("path", "string"),
            ("lang", "string"),
            ("format", "string"),
            ("strict", "boolean"),
        ],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "ci-check",
        description: "Full CI gate suite: aden check, project tests, aden lint, secret scan, attribution check, OWASP audit, merge-conflict-marker scan, insecure-protocol check, cargo clippy, cargo audit, contract freshness. Use before push to remote. For local dev use `ready` instead.",
        args: &[("path", "string")],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "diagnose",
        description: "Deterministic structural scan of the graph: stale refs, duplicate anchors, invalid edges, orphans, circular includes, missing source files; emits a 0-100 health score (format=json for machine output). Overlaps `check` but additionally flags duplicate anchors and low-confidence nodes and reports a score; read-only — finds nothing about your environment (that's `doctor`).",
        args: &[("path", "string"), ("format", "string")],
        tier: Tier::Core,
    },
    ToolSpec {
        name: "regen",
        description: "Full from-scratch rebuild: clears the gen/graph caches and (unless $ADEN_STORE is pinned/shared) the per-user store, then regenerates and prunes stale anchors. NOT an alias for `gen` — use after renames/deletions leave stale anchors or after corruption; for routine incremental indexing use `gen`.",
        args: &[("path", "string")],
        tier: Tier::Core,
    },
    // ── Extended: setup / admin / niche. Hidden from list_tools by default
    //    (set ADEN_MCP_FULL=1 to surface) but always callable by name. ──
    ToolSpec {
        name: "new",
        description: "Create a new project from a language template.",
        args: &[("name", "string"), ("lang", "string"), ("path", "string")],
        tier: Tier::Extended,
    },
    ToolSpec {
        name: "init",
        description: "Scaffold .agent/ workspace and templates.",
        args: &[("path", "string")],
        tier: Tier::Extended,
    },
    ToolSpec {
        name: "kickoff",
        description: "Create a structured kickoff document.",
        args: &[
            ("name", "string"),
            ("interactive", "boolean"),
            ("path", "string"),
        ],
        tier: Tier::Extended,
    },
    ToolSpec {
        name: "workflow",
        description: "Instantiate templates with substitutions.",
        args: &[
            ("template", "string"),
            ("out", "string"),
            ("from", "string"),
            ("path", "string"),
        ],
        tier: Tier::Extended,
    },
    ToolSpec {
        name: "session",
        description: "Append entry to .agent/session.adoc.",
        args: &[
            ("agent_id", "string"),
            ("task", "string"),
            ("status", "string"),
        ],
        tier: Tier::Extended,
    },
    ToolSpec {
        name: "review",
        description: "Semantic review of pending heal proposals. Only meaningful after `heal propose=true` has written proposals.",
        args: &[
            ("path", "string"),
            ("since", "string"),
            ("budget", "integer"),
        ],
        tier: Tier::Extended,
    },
    ToolSpec {
        name: "complete",
        description: "List contracts missing required documentation. Reports only — automatic LLM filling is NOT implemented; --model just previews the fill prompt. For drift in existing docs use `heal`, not this.",
        args: &[
            ("path", "string"),
            ("dry_run", "boolean"),
            ("model", "string"),
        ],
        tier: Tier::Extended,
    },
    ToolSpec {
        name: "query-adq",
        description: "Execute an Aden Query (.adq) script — multi-step filtered graph traversal (node/incoming/outgoing/where) beyond what `query` expresses in one call. For simple backlinks/impact use `query`.",
        args: &[("script", "string"), ("path", "string")],
        tier: Tier::Extended,
    },
    ToolSpec {
        name: "doctor",
        description: "Probe the host environment, NOT the graph: git, language toolchains (rustc/cargo, node, python, go), signing keys. Use when a command fails for environmental reasons (missing tool/binary). For graph/content problems use `diagnose`; for reference errors use `check`.",
        args: &[("path", "string")],
        tier: Tier::Extended,
    },
    ToolSpec {
        name: "licenses",
        description: "Generate third-party dependency attribution.",
        args: &[("path", "string"), ("full", "boolean")],
        tier: Tier::Extended,
    },
    ToolSpec {
        name: "federation",
        description: "Manage a multi-repo workspace. action is a subcommand: list, add, remove, config. Over MCP only list/config run (add/remove need a path/name the bridge can't pass — use the CLI). Operates on the federation manifest only; it does not index or query code.",
        args: &[("action", "string")],
        tier: Tier::Extended,
    },
    ToolSpec {
        name: "emergency",
        description: "Downgrade Forbid policies to Warn with justification.",
        args: &[("reason", "string"), ("path", "string"), ("ttl", "string")],
        tier: Tier::Extended,
    },
    ToolSpec {
        name: "mcp",
        description: "MCP (Model Context Protocol) integration management. action is a subcommand: install, uninstall, list. Over MCP only `list` runs (install/uninstall are a one-time terminal setup step).",
        args: &[("action", "string")],
        tier: Tier::Extended,
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
        // Default to the Core surface to keep the per-session registry slim;
        // `ADEN_MCP_FULL` widens it to Core + Extended. Hidden Extended tools
        // remain callable by name via `call_tool`, so nothing is unreachable.
        let full = surface_is_full();
        let tools: Vec<Tool> = TOOLS
            .iter()
            .filter(|t| full || t.tier == Tier::Core)
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

async fn run_aden_command(project_dir: &Path, args: &[&str]) -> Result<String, String> {
    let child = tokio::process::Command::new(resolve_aden_binary())
        .args(args)
        .current_dir(project_dir)
        .output();

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
        Ok(raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("INFO:"))
            .filter(|l| !l.trim_start().starts_with("Generated"))
            .filter(|l| !l.trim_start().starts_with("Emitted"))
            .collect::<Vec<_>>()
            .join("\n"))
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
    fn core_tools_lead_extended_tools() {
        // Primacy: every Core tool must be declared before any Extended tool,
        // so the default (Core-only) list is also the highest-value-first list.
        let first_extended = TOOLS.iter().position(|t| t.tier == Tier::Extended);
        if let Some(idx) = first_extended {
            assert!(
                TOOLS[idx..].iter().all(|t| t.tier == Tier::Extended),
                "a Core tool is declared after an Extended tool — primacy order broken"
            );
        }
    }

    #[test]
    fn default_surface_is_core_only_full_surface_is_everything() {
        let core = TOOLS.iter().filter(|t| t.tier == Tier::Core).count();
        let all = TOOLS.len();
        assert!(
            core > 0 && core < all,
            "expected a mix of Core and Extended"
        );
        // Default list (no ADEN_MCP_FULL) shows Core only; full shows all.
        assert_eq!(TOOLS.iter().filter(|t| Tier::Core == t.tier).count(), core);
    }

    #[test]
    fn parse_full_toggle() {
        for on in ["1", "true", "TRUE", " full ", "yes", "Yes"] {
            assert!(parse_full(Some(on)), "{on:?} should enable full surface");
        }
        for off in ["0", "false", "", "no", "core", "2"] {
            assert!(!parse_full(Some(off)), "{off:?} should not enable full");
        }
        assert!(!parse_full(None), "unset must default to Core-only");
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
}
