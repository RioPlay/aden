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

use percent_encoding::percent_decode_str;
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    model::*,
    service::{NotificationContext, RequestContext, RoleServer},
    transport::stdio,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{Semaphore, watch};

/// Maximum simultaneous CLI children. This limit protects the graph store from
/// a client fan-out; callers should batch independent MCP requests at or below it.
const MAX_CONCURRENT_CLI_CHILDREN: usize = 2;

/// The result shared by duplicate read calls. This remains private so the MCP
/// transport preserves its existing success/error envelopes.
#[derive(Clone)]
enum SharedReadResult {
    Output(String),
    CommandError(String),
    Busy,
}

enum ReadFlight {
    Leader(watch::Sender<Option<SharedReadResult>>),
    Follower(watch::Receiver<Option<SharedReadResult>>),
}

/// Removes an in-flight key even if the leader request is cancelled while its
/// child process is running. Without this guard, followers can inherit a
/// receiver that will never receive a value and every later identical request
/// remains stuck behind the abandoned leader.
struct ReadFlightGuard {
    key: String,
    sender: Option<watch::Sender<Option<SharedReadResult>>>,
    flights: Arc<Mutex<HashMap<String, watch::Receiver<Option<SharedReadResult>>>>>,
}

impl ReadFlightGuard {
    fn new(
        key: String,
        sender: watch::Sender<Option<SharedReadResult>>,
        flights: Arc<Mutex<HashMap<String, watch::Receiver<Option<SharedReadResult>>>>>,
    ) -> Self {
        Self {
            key,
            sender: Some(sender),
            flights,
        }
    }

    fn finish(mut self, result: SharedReadResult) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(Some(result));
            self.flights
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&self.key);
        }
    }
}

impl Drop for ReadFlightGuard {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(Some(SharedReadResult::CommandError(
                "identical in-flight read was cancelled; retry this call".to_string(),
            )));
            self.flights
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&self.key);
        }
    }
}

// ── Server struct ───────────────────────────────────────────

#[derive(Clone)]
pub struct AdenMcpServer {
    /// Active project root (may update when the host reports new Roots).
    project_dir: Arc<RwLock<PathBuf>>,
    /// Every workspace root reported by the host. Keeping the complete set
    /// prevents multi-root clients from silently routing everything to root 0.
    workspace_roots: Arc<RwLock<Vec<PathBuf>>>,
    /// Whether the host has supplied authoritative Roots and whether that
    /// workspace is still available. An empty Roots update must not silently
    /// retain and query the previous repository.
    roots_seen: Arc<RwLock<bool>>,
    workspace_available: Arc<RwLock<bool>>,
    /// When true, argv/`ADEN_PROJECT` pin wins over Roots auto-detect.
    pinned: bool,
    /// Bound concurrent CLI children per MCP server. Rejecting when saturated
    /// is preferable to silently queueing expensive graph/build work.
    command_slots: Arc<Semaphore>,
    /// Duplicate read calls share one CLI child instead of wasting a slot. The
    /// key includes the resolved project and normalized argv, never crosses a
    /// workspace boundary, and exists only until that child completes.
    in_flight_reads: Arc<Mutex<HashMap<String, watch::Receiver<Option<SharedReadResult>>>>>,
}

impl AdenMcpServer {
    pub fn new(project_dir: PathBuf) -> Self {
        Self::with_options(project_dir, false)
    }

    pub fn with_options(project_dir: PathBuf, pinned: bool) -> Self {
        Self {
            workspace_roots: Arc::new(RwLock::new(vec![project_dir.clone()])),
            roots_seen: Arc::new(RwLock::new(false)),
            workspace_available: Arc::new(RwLock::new(true)),
            project_dir: Arc::new(RwLock::new(project_dir)),
            pinned,
            command_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_CLI_CHILDREN)),
            in_flight_reads: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Join an identical in-flight read, or become its sole leader. Keep this
    /// synchronous: the lock is held only while inspecting a small map.
    fn join_read_flight(&self, key: String) -> ReadFlight {
        let mut flights = self
            .in_flight_reads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(receiver) = flights.get(&key) {
            return ReadFlight::Follower(receiver.clone());
        }
        let (sender, receiver) = watch::channel(None);
        flights.insert(key, receiver);
        ReadFlight::Leader(sender)
    }

    #[cfg(test)]
    fn finish_read_flight(
        &self,
        key: &str,
        sender: watch::Sender<Option<SharedReadResult>>,
        result: SharedReadResult,
    ) {
        // Notify followers before dropping the map entry. A subsequent request
        // may become a new leader, but it can never miss this completed result.
        let _ = sender.send(Some(result));
        self.in_flight_reads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(key);
    }

    fn guard_read_flight(
        &self,
        key: String,
        sender: watch::Sender<Option<SharedReadResult>>,
    ) -> ReadFlightGuard {
        ReadFlightGuard::new(key, sender, Arc::clone(&self.in_flight_reads))
    }

    fn project_dir(&self) -> PathBuf {
        self.project_dir
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_project_dir(&self, dir: PathBuf) {
        if let Ok(mut g) = self.project_dir.write() {
            *g = dir;
        }
    }

    fn workspace_roots(&self) -> Vec<PathBuf> {
        self.workspace_roots
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_workspace_roots(&self, roots: Vec<PathBuf>) {
        if roots.is_empty() {
            return;
        }
        self.set_project_dir(roots[0].clone());
        if let Ok(mut g) = self.workspace_roots.write() {
            *g = roots;
        }
        if let Ok(mut seen) = self.roots_seen.write() {
            *seen = true;
        }
        if let Ok(mut available) = self.workspace_available.write() {
            *available = true;
        }
    }

    fn mark_workspace_unavailable_if_roots_seen(&self) {
        let seen = *self.roots_seen.read().unwrap_or_else(|e| e.into_inner());
        if seen && let Ok(mut available) = self.workspace_available.write() {
            *available = false;
        }
    }

    fn workspace_available(&self) -> bool {
        *self
            .workspace_available
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }
}

/// Guidance surfaced to the LLM at session start. Frames Aden as a
/// language-agnostic context compiler and gives the canonical workflow so the
/// model uses the tools correctly on *any* project (not just Rust).
const SERVER_INSTRUCTIONS: &str = "\
Use Aden for bounded, structure-aware code navigation. Read tools auto-refresh; \
omit `gen`, budgets, and tuning arguments in normal calls.\n\n\
Workflow: use `tree(symbols=true)` for a compact codebase map (scope `path` if truncated); then `grep` \
for independent evidence -> `locate` ambiguous names -> `understand` a known \
symbol. Use `query` for backlinks/impact and `asm` for bounded context. Before \
editing, inspect callers/downstream impact.\n\n\
Correctness rules:\n\
- `ask` handles one bounded question about a named symbol, file, subsystem, or \
relationship. Broad audits, exhaustive lists, rankings, and remaining-work \
questions return `needs_narrowing`; do not paraphrase around that guard.\n\
- Graph resolution is heuristic. No result does not prove absence; retry with \
`grep` and inspect alternatives/source ranges.\n\
- Context is bounded, not complete source. Treat ambiguous routing, stale \
receipts, truncation, and incomplete results as reasons to refine discovery.\n\
- Use native Git/filesystem/build/test tools for history, inventory, and external \
verification. Never infer repository-wide completeness from one Aden result.\n\n\
`path` defaults to the client workspace; set it only to disambiguate multiple \
repositories. At most two distinct calls run concurrently; retry `server_busy` \
after one completes. The Essential registry is intentionally minimal. Set \
ADEN_MCP_SURFACE=standard or full only when those extra tools are wanted.";

// ── Tool declaration ──────────────────────────────────────────

/// Surface tier — how broad an enablement a tool needs to be listed and called.
/// This keeps a session from being flooded with build/setup/admin tools and
/// prevents a caller from bypassing the operator's selected execution surface.
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
const ESSENTIAL_TOOLS: &[&str] = &[
    "tree",
    "grep",
    "locate",
    "understand",
    "ask",
    "asm",
    "query",
];

/// Keep the default MCP registry cheap enough to include in every model turn.
/// Advanced tuning remains available when the operator opts into the Standard
/// or Full surface; the Essential surface advertises only the arguments needed
/// for the normal discovery -> comprehension -> traversal loop.
fn essential_arg_visible(tool: &str, arg: &str) -> bool {
    match tool {
        "tree" => matches!(arg, "path" | "symbols"),
        "grep" => matches!(arg, "pattern" | "path" | "regex"),
        "locate" => matches!(arg, "symbol" | "caller_of" | "path"),
        "understand" => matches!(arg, "symbol" | "path"),
        "ask" => matches!(arg, "question" | "path" | "from"),
        "query" => matches!(
            arg,
            "from" | "backlinks" | "impact" | "path" | "max_results"
        ),
        "asm" => matches!(arg, "from" | "path"),
        _ => true,
    }
}

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

fn tool_enabled(name: &str, surface: Tier) -> bool {
    tool_tier(name) <= surface
}

/// Registry order is behavioral guidance for LLMs: exact discovery and symbol
/// disambiguation precede heuristic routing. This order is intentionally
/// independent of the declaration table and surface tiers.
fn tool_display_rank(name: &str) -> usize {
    match name {
        "tree" => 0,
        "grep" => 1,
        "locate" => 2,
        "understand" => 3,
        "ask" => 4,
        "query" => 5,
        "asm" => 6,
        "impact-diff" => 10,
        "check" => 11,
        "status" => 12,
        "list" => 13,
        "communities" => 14,
        "diagnose" => 15,
        "test" => 16,
        "lint" => 17,
        "audit" => 18,
        _ => 100,
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
        // The CLI defaults `--lang` to rust; requiring it over MCP would make
        // an otherwise valid `aden new <name>` impossible for agents.
        "new" => &["name"],
        "workflow" => &["template"],
        // `from` carries the anchor for asm; the CLI marks it required.
        "asm" => &["from"],
        // CLI makes `status` optional (Option<String>); only id + task are required.
        "session" => &["agent_id", "task"],
        "emergency" => &["reason"],
        _ => &[],
    }
}

/// Schema defaults which are part of the stable CLI/MCP contract.  Keep this
/// deliberately small: each value is also asserted against clap's help by the
/// CLI parity tests, preventing an MCP client from receiving a stale hint.
fn arg_default(tool: &str, arg: &str) -> Option<serde_json::Value> {
    match (tool, arg) {
        // Every MCP tool executes inside the resolved project when path is
        // absent.  Exposing it makes the server's most important implicit
        // default visible to schema-aware clients.
        // `gen` accepts zero or more paths (`[PATH]...`) rather than the
        // normal single `[DIR]` with clap's rendered default. Keep its schema
        // honest: omitting it still uses the server project, but it is not a
        // CLI `default` promise clients can display.
        (tool, "path") if tool != "gen" => Some(serde_json::json!(".")),
        ("ask", "budget") => Some(serde_json::json!(4096)),
        // Agents benefit from the bounded, token-lean outline by default;
        // terminal users retain the CLI's graphical default.
        ("tree", "symbols") => Some(serde_json::json!(true)),
        ("tree", "depth") => Some(serde_json::json!(4)),
        ("tree", "symbol_depth") => Some(serde_json::json!(0)),
        ("grep", "limit") => Some(serde_json::json!(100)),
        ("asm", "depth") => Some(serde_json::json!(2)),
        ("asm", "budget") => Some(serde_json::json!(8192)),
        ("asm", "format") => Some(serde_json::json!("json")),
        ("query", "depth") => Some(serde_json::json!(3)),
        ("query", "format") => Some(serde_json::json!("json")),
        ("query", "max_results") => Some(serde_json::json!(1000)),
        ("locate", "format") => Some(serde_json::json!("plain")),
        ("locate", "limit") => Some(serde_json::json!(50)),
        ("understand", "budget") => Some(serde_json::json!(4000)),
        ("new", "lang") => Some(serde_json::json!("rust")),
        ("check" | "lint", "severity") => Some(serde_json::json!("Warn")),
        ("search" | "list", "limit") => Some(serde_json::json!(50)),
        ("search" | "list", "offset") => Some(serde_json::json!(0)),
        ("communities", "min_size") => Some(serde_json::json!(2)),
        ("communities", "limit") => Some(serde_json::json!(30)),
        ("communities", "resolution") => Some(serde_json::json!(1.0)),
        ("viz", "mode") => Some(serde_json::json!("blast")),
        ("viz", "depth") => Some(serde_json::json!(2)),
        ("viz", "format") => Some(serde_json::json!("mermaid")),
        ("viz", "resolution") => Some(serde_json::json!(1.0)),
        ("audit" | "diagnose", "format") => Some(serde_json::json!("text")),
        ("review", "budget") => Some(serde_json::json!(2048)),
        ("emergency", "ttl") => Some(serde_json::json!("24h")),
        _ => None,
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
        // MCP exposes only the read-only subcommands.  `add`/`remove` and
        // `install`/`uninstall` mutate host/project configuration and must not
        // hide behind a Read annotation.
        ("federation", "action") => &["list", "config"],
        ("mcp", "action") => &["list"],
        // Severity thresholds — note check uses Forbid, lint uses Error.
        ("check", "severity") => &["Suggest", "Warn", "Forbid"],
        ("lint", "severity") => &["Suggest", "Warn", "Error"],
        // Output formats — each tool's accepted set differs.
        ("viz", "format") => &["mermaid", "dot", "asciidoc", "json"],
        ("asm", "format") => &["json", "llm", "adg", "aden"],
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
    if let Some(action) = args.get("action").and_then(serde_json::Value::as_str) {
        let allowed = arg_enum(tool, "action");
        if !allowed.is_empty() && !allowed.contains(&action) {
            return Err(format!(
                "{tool}: `{action}` is not available over MCP; use a terminal for mutating configuration actions"
            ));
        }
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
    // Enforce the advertised schema at runtime too. Some MCP clients do not
    // validate tool arguments, and silently dropping a wrong-typed value would
    // make the CLI run with an unintended default.
    if let Some(spec) = TOOLS.iter().find(|t| t.name == tool) {
        for (name, value) in args {
            let declared_type = spec
                .args
                .iter()
                .find(|(declared, _)| declared == name)
                .map(|(_, ty)| *ty)
                .or_else(|| {
                    (name == "require_fresh" && supports_authoritative_freshness(tool))
                        .then_some("boolean")
                });
            let Some(arg_type) = declared_type else {
                return Err(format!("unknown argument `{name}` for {tool}"));
            };
            if value.is_null() {
                continue;
            }
            let valid = match arg_type {
                "boolean" => value.is_boolean(),
                "integer" => value.as_u64().is_some(),
                "string" => value.is_string(),
                _ => false,
            };
            if !valid {
                return Err(format!(
                    "argument `{name}` must be a non-negative {arg_type}"
                ));
            }
            let allowed = arg_enum(tool, name);
            if !allowed.is_empty()
                && let Some(value) = value.as_str()
                && !allowed.contains(&value)
            {
                return Err(format!(
                    "argument `{name}` must be one of: {}",
                    allowed.join(", ")
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

/// Every advertised read surface, exposed for end-to-end contract fixtures.
#[doc(hidden)]
pub fn agent_read_tools() -> Vec<&'static str> {
    TOOLS
        .iter()
        .filter(|spec| spec.effect == Effect::Read)
        .map(|spec| spec.name)
        .collect()
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

/// Contract-test accessor for schema requiredness.  Kept public only so the
/// CLI integration suite can independently pin the MCP declaration to clap.
pub fn tool_required_args(tool: &str) -> &'static [&'static str] {
    required_args(tool)
}

/// Contract-test accessor for documented MCP defaults.
pub fn tool_arg_default(tool: &str, arg: &str) -> Option<serde_json::Value> {
    arg_default(tool, arg)
}

/// Defaults applied by the MCP transport rather than by clap itself.
pub fn tool_arg_default_is_transport_override(tool: &str, arg: &str) -> bool {
    matches!((tool, arg), ("tree", "symbols"))
}

/// Extra CLI flags the MCP appends so a read tool emits machine-readable output
/// instead of terminal chrome. Only tools that actually have a JSON path belong
/// here; the flags are skipped if the agent already supplied them (e.g. a
/// `format` arg), so we never pass a flag twice. Use the CLI's `-j` shorthand:
/// it is injected on every MCP read, so saving those tokens has the widest
/// transport impact.
fn structured_output_flags(tool: &str) -> &'static [&'static str] {
    match tool {
        // These honor the global `-j/--json` and print a structured envelope.
        "tree" | "grep" | "search" | "list" | "test" | "impact-diff" | "communities" | "ask"
        | "asm" | "query" | "locate" => &["-j"],
        // `understand` has a command-local `--json` option without `-j`.
        // Passing the global shorthand after the subcommand is rejected by clap.
        "understand" => &["--json"],
        // Phase 2B: compact gate summaries for agent verify workflow.
        "check" => &["-j", "--max-issues", "20"],
        "heal" => &["-j", "--max-issues", "10"],
        "status" => &["-j"],
        _ => &[],
    }
}

/// Prefer short flags for the core agent navigation loop. These aliases are
/// defined by clap on the matching CLI subcommands; retaining long flags keeps
/// direct CLI calls backward-compatible while MCP uses the compact spelling by
/// default. Unlisted arguments deliberately retain their readable long form.
fn compact_flag(tool: &str, arg: &str) -> Option<&'static str> {
    match (tool, arg) {
        // Most commands inherit the global `-j`; `understand` declares a
        // command-local long-only option that shadows it.
        ("understand", "json") => Some("--json"),
        (_, "json") => Some("-j"),
        ("grep", "regex") => Some("-r"),
        ("grep", "ignore_case") => Some("-i"),
        ("grep", "symbol_only") => Some("-s"),
        ("grep", "limit") => Some("-n"),
        ("locate", "symbol") => Some("-s"),
        ("locate", "caller_of") => Some("-c"),
        ("locate", "format") => Some("-F"),
        ("locate", "show_context") => Some("-C"),
        ("locate", "limit") => Some("-n"),
        ("understand", "budget") => Some("-b"),
        ("ask", "from") | ("asm", "from") | ("query", "from") => Some("-f"),
        ("ask", "budget") | ("asm", "budget") => Some("-b"),
        ("ask", "intent") => Some("-i"),
        ("ask", "depth") | ("asm", "depth") | ("query", "depth") => Some("-d"),
        ("ask", "edge_types") | ("asm", "edge_types") => Some("-e"),
        ("ask", "strict") | ("asm", "strict") => Some("-s"),
        ("ask", "explain") => Some("-x"),
        ("asm", "out") => Some("-o"),
        ("asm", "format") | ("query", "format") => Some("-F"),
        ("query", "edge_type") => Some("-e"),
        ("query", "backlinks") => Some("-b"),
        ("query", "impact") => Some("-i"),
        _ => None,
    }
}

fn cli_flag(tool: &str, arg: &str) -> String {
    compact_flag(tool, arg)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("--{}", arg.replace('_', "-")))
}

/// Translate MCP JSON arguments into `aden` CLI arguments for `spec`.
///
/// The first element is the subcommand name. Core navigation fields use their
/// documented clap short flag; every other snake_case field uses the kebab-case
/// long spelling (`edge_types` → `--edge-types`). Positional args are emitted
/// bare. Pure and side-effect-free so the flag mapping is unit-tested directly.
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
        let flag = cli_flag(spec.name, arg_name);
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

/// MCP is an agent-facing transport, where a caller's budget must bound the
/// text handed back to the model. Default assembly tools to the CLI's strict
/// mode unless the caller explicitly supplied `strict` (including `false`).
/// This is transport policy only; interactive CLI defaults do not change.
fn apply_mcp_budget_defaults(
    tool: &str,
    args: &serde_json::Map<String, serde_json::Value>,
    cmd_args: &mut Vec<String>,
) {
    if matches!(tool, "ask" | "asm") && !args.contains_key("strict") {
        let insert_at = cmd_args
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or(cmd_args.len());
        cmd_args.insert(insert_at, cli_flag(tool, "strict"));
    }
}

/// Agent-facing orientation should be bounded without requiring every model to
/// remember a transport-specific hint. Keep the interactive CLI's graphical
/// default, but make an omitted MCP `symbols` argument select the compact symbol
/// outline. An explicit `false` remains an opt-out for callers that want the
/// human-style tree.
fn apply_mcp_tree_default(
    tool: &str,
    args: &serde_json::Map<String, serde_json::Value>,
    cmd_args: &mut Vec<String>,
) {
    if tool == "tree" && !args.contains_key("symbols") {
        let insert_at = cmd_args
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or(cmd_args.len());
        cmd_args.insert(insert_at, cli_flag(tool, "symbols"));
    }
}

/// Build the exact CLI argv used by the MCP wrapper, including agent-facing
/// strict-budget defaults. Public only for the cross-crate transport golden:
/// production callers should invoke the MCP server.
#[doc(hidden)]
pub fn prepare_cli_args_for_mcp(
    tool: &str,
    args: &serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<String>, String> {
    let spec = TOOLS
        .iter()
        .find(|candidate| candidate.name == tool)
        .ok_or_else(|| format!("unknown tool: {tool}"))?;
    let mut cmd_args = build_cli_args(spec, args, structured_output_flags(tool));
    if supports_authoritative_freshness(tool)
        && args
            .get("require_fresh")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    {
        cmd_args.insert(0, "--require-fresh".to_string());
    }
    apply_mcp_budget_defaults(tool, args, &mut cmd_args);
    apply_mcp_tree_default(tool, args, &mut cmd_args);
    Ok(cmd_args)
}

/// Build the JSON Schema + `Tool` for a spec. The default Essential registry
/// deliberately omits expert tuning knobs, reducing the schema repeatedly sent
/// to models. Standard/Full and direct schema inspection retain the complete
/// compatibility surface.
fn tool_from_spec_for_surface(spec: &ToolSpec, surface: Tier) -> Tool {
    let compact = surface == Tier::Essential && tool_tier(spec.name) == Tier::Essential;
    let mut props = serde_json::Map::new();
    for &(arg_name, ty) in spec.args {
        if compact && !essential_arg_visible(spec.name, arg_name) {
            continue;
        }
        let mut p = serde_json::Map::new();
        p.insert("type".to_string(), serde_json::json!(ty));
        // Constrain enumerable args (e.g. federation/mcp `action`) so a client
        // can validate the value and an LLM sees the valid set, instead of
        // discovering a bad verb via an opaque CLI error.
        let allowed = arg_enum(spec.name, arg_name);
        if !allowed.is_empty() {
            p.insert("enum".to_string(), serde_json::json!(allowed));
        }
        if let Some(default) = arg_default(spec.name, arg_name) {
            p.insert("default".to_string(), default);
        }
        props.insert(arg_name.to_string(), serde_json::Value::Object(p));
    }
    if supports_authoritative_freshness(spec.name) && !compact {
        props.insert(
            "require_fresh".to_string(),
            serde_json::json!({
                "type": "boolean",
                "default": false,
                "description": "Wait briefly for an authoritative current graph or fail actionably"
            }),
        );
    }
    let mut schema = JsonObject::new();
    schema.insert("type".to_string(), serde_json::json!("object"));
    schema.insert("properties".to_string(), serde_json::Value::Object(props));
    // Reject unknown/misspelled args at the schema boundary instead of silently
    // ignoring them (e.g. a typo'd `dept` for `depth` would otherwise no-op).
    schema.insert("additionalProperties".to_string(), serde_json::json!(false));
    schema.insert(
        "x-aden-output-mode".to_string(),
        serde_json::json!(match spec.effect {
            Effect::Read => "json-receipt-v1",
            Effect::Rebuild => "text-progress",
            Effect::Mutate => "text-result",
        }),
    );
    // Keep the safety classification discoverable even in MCP clients that do
    // not render standard ToolAnnotations in their tool palette.
    schema.insert(
        "x-aden-effect".to_string(),
        serde_json::json!(match spec.effect {
            Effect::Read => "read",
            Effect::Rebuild => "rebuild",
            Effect::Mutate => "mutate",
        }),
    );
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

fn tool_from_spec(spec: &ToolSpec) -> Tool {
    tool_from_spec_for_surface(spec, Tier::Full)
}

/// Whether this MCP tool is a graph-read surface that may request an
/// authoritative source-to-graph binding. The CLI flag is global for clap
/// parsing, but mutation/admin tools intentionally do not advertise it.
pub fn supports_authoritative_freshness(tool: &str) -> bool {
    matches!(
        tool,
        "tree"
            | "grep"
            | "search"
            | "ask"
            | "asm"
            | "query"
            | "locate"
            | "understand"
            | "impact-diff"
            | "communities"
            | "scope"
            | "viz"
    )
}

/// Testable view of the generated MCP schema. `require_fresh` is additive to
/// a ToolSpec rather than stored in its static argument table, so parity tests
/// must inspect the final schema rather than only `tool_arg_specs()`.
pub fn tool_advertises_authoritative_freshness(tool: &str) -> bool {
    TOOLS
        .iter()
        .find(|spec| spec.name == tool)
        .is_some_and(|spec| {
            tool_from_spec(spec).input_schema["properties"]
                .get("require_fresh")
                .is_some()
        })
}

/// The tool surface the operator requested, gating which tools `list_tools` returns
/// and which tools `call_tool` may execute. Default is ESSENTIAL —
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
    //    selects the level; hidden tools require explicit surface opt-in. The slice
    //    is kept grouped Essential-first below purely for readability. ──
    ToolSpec {
        name: "tree",
        title: "Outline the codebase",
        description: "Bounded codebase map. MCP defaults to symbols=true for exact names and line ranges grouped by file; set symbols=false for a human-style directory tree. If truncated, rerun with path set to a project-relative subtree.",
        args: &[
            ("path", "string"),
            ("symbols", "boolean"),
            ("depth", "integer"),
            ("symbol_depth", "integer"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "grep",
        title: "Search code (structure-aware)",
        description: "Structure-aware content search. Each hit names its enclosing symbol, avoiding whole-file reads. Start here for independent evidence; pass a hit to locate/understand/query. Auto-refreshes; path defaults to the workspace.",
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
        description: "Definition, backlinks, impact, and bounded context for a known symbol. Use locate first for ambiguous names. Missing dynamic/local/test symbols are not proof of absence; retry with grep. Read the located source range before subtle edits.",
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
        title: "Ask one bounded conceptual question (heuristic)",
        description: "Ask one bounded question about a named symbol, file, subsystem, or relationship. Repo-wide audits, exhaustive lists, rankings, and remaining-work questions return needs_narrowing instead of guessing. Inspect routing_confidence; verify ambiguous results with grep/locate. Omit budget normally.",
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
            ("expand", "boolean"),
            ("strict", "boolean"),
            ("explain", "boolean"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "locate",
        title: "Locate a symbol",
        description: "Resolve a symbol or its callers to canonical anchors with ranked alternatives. Use before understand/query for short or repeated names. Full-text fallbacks are weaker; retry missing nested/local/test symbols with grep.",
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
        description: "Return a bounded, token-dense graph neighborhood for a canonical anchor or unique natural symbol name instead of whole files. Ambiguous names return ranked candidates without guessing. Auto-refreshes; omit budget normally.",
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
            ("select", "boolean"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "query",
        title: "Query the graph",
        description: "Traverse from a canonical anchor or unique natural symbol name. backlinks finds references; impact finds downstream reach. Ambiguous names return ranked candidates without guessing. Returns bounded JSON and auto-refreshes.",
        args: &[
            ("path", "string"),
            ("from", "string"),
            ("edge_type", "string"),
            ("depth", "integer"),
            ("backlinks", "string"),
            ("impact", "string"),
            ("format", "string"),
            ("max_results", "integer"),
        ],
        effect: Effect::Read,
    },
    ToolSpec {
        name: "check",
        title: "Validate the graph",
        description: "Validate the graph and gate CI: flags unresolved <<refs>>, circular includes, orphan anchors, typed-edge violations, stale source hashes, and incomplete contracts. severity=Suggest|Warn|Forbid sets the fail threshold and exits non-zero past it. For duplicate-anchor detection and a 0-100 health score, use `diagnose`.",
        args: &[
            ("path", "string"),
            ("severity", "string"),
            ("max_issues", "integer"),
        ],
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
            ("max_issues", "integer"),
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
        description: "Reconcile the store — gen + check + heal. Use after large merges or file deletions, NOT as a routine pre-commit step (use `ready` for that). Set gc=true only when you intentionally want to prune deleted symbols; no_gc=true is a deprecated compatibility alias.",
        args: &[("path", "string"), ("gc", "boolean"), ("no_gc", "boolean")],
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
    //    ADEN_MCP_SURFACE=full; calls require that explicit surface. ──
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
            ("templates", "boolean"),
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
        // The same surface is enforced in `call_tool`, so hidden expensive tools
        // cannot be reached accidentally by name.
        let level = requested_surface();
        let mut specs: Vec<_> = TOOLS
            .iter()
            .filter(|tool| tool_enabled(tool.name, level))
            .collect();
        specs.sort_by_key(|tool| tool_display_rank(tool.name));
        let tools: Vec<Tool> = specs
            .into_iter()
            .map(|spec| tool_from_spec_for_surface(spec, level))
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let tool_name = request.name.as_ref();
        let mut args = request.arguments.unwrap_or_default();

        // Auto-detect workspace from MCP Roots / host env unless explicitly pinned.
        if !self.pinned {
            refresh_project_from_client(self, &context).await;
        }
        if !self.workspace_available() {
            return Ok(CallToolResult::error(vec![Content::text(
                boundary_error_for_mcp(
                    tool_name,
                    "workspace_unavailable",
                    "the MCP host cleared its workspace Roots; refusing to reuse the previous repository",
                    true,
                    "open a workspace in the host or restart Aden with an explicit project pin",
                ),
            )]));
        }
        let project_dir = self.project_dir();
        let workspace_roots = self.workspace_roots();

        // Semantic graph operations are expensive against an accidental
        // multi-repository parent. Select a unique repository deterministically
        // or fail before spawning the CLI; an explicit path always wins.
        let had_explicit_path = args.get("path").is_some();
        if let Err(e) = resolve_repository_scope(tool_name, &mut args, &workspace_roots) {
            return Ok(CallToolResult::error(vec![Content::text(e)]));
        }
        // Scope inference writes an absolute repository path into `args`. Use
        // that same selected repository as the child CWD instead of always
        // using the first MCP Root.
        let project_dir = execution_dir_for_args(&args, &project_dir);

        // Validate tool exists
        let Some(spec) = TOOLS.iter().find(|t| t.name == tool_name) else {
            return Ok(CallToolResult::error(vec![Content::text(
                boundary_error_for_mcp(
                    tool_name,
                    "unknown_tool",
                    &format!("unknown tool: {tool_name}"),
                    true,
                    "list the server tools and retry with a supported tool name",
                ),
            )]));
        };

        // Tiering is an execution boundary, not only a discovery hint. A client
        // that knows a hidden tool name must still explicitly opt into the
        // wider surface before it can launch an expensive subprocess.
        let surface = requested_surface();
        if !tool_enabled(spec.name, surface) {
            let required = match tool_tier(spec.name) {
                Tier::Essential => "essential",
                Tier::Standard => "standard",
                Tier::Full => "full",
            };
            return Ok(CallToolResult::error(vec![Content::text(
                boundary_error_for_mcp(
                    tool_name,
                    "tool_surface_disabled",
                    &format!("{tool_name} is not enabled on the {surface:?} MCP surface"),
                    true,
                    &format!(
                        "restart Aden MCP with ADEN_MCP_SURFACE={required} (or full) and retry"
                    ),
                ),
            )]));
        }

        // SECURITY (audit HIGH-1): confine any caller-supplied filesystem path
        // to the server's project_dir before shelling out. The CLI does not
        // confine paths (find_project_root will happily resolve `/etc`), so an
        // MCP client could otherwise read or write arbitrary host files via the
        // `path` argument. Reject out-of-tree paths at the boundary.
        if let Err(e) = confine_path_args_to_roots(tool_name, &args, &workspace_roots) {
            return Ok(CallToolResult::error(vec![Content::text(
                boundary_error_for_mcp(
                    tool_name,
                    "path_outside_workspace",
                    &e,
                    true,
                    "retry with a path inside one of the declared MCP workspace roots",
                ),
            )]));
        }

        // `locate` needs at least one of `symbol`/`caller_of`; neither can be
        // expressed as a JSON-schema `required` (that is an AND). Validate the
        // "at least one of" constraint here so an empty call fails fast with a
        // clear message instead of shelling out to a CLI usage error.
        if let Err(e) = validate_args(tool_name, &args) {
            return Ok(CallToolResult::error(vec![Content::text(
                boundary_error_for_mcp(
                    tool_name,
                    "invalid_arguments",
                    &e,
                    true,
                    "correct the named argument and retry",
                ),
            )]));
        }

        // Build CLI args: `aden <name> [--flag|--key value] [extra] -- [positional]`.
        // Read tools print terminal chrome (truncation footers, banners) by
        // default; request machine-readable JSON so the agent receives a
        // structured envelope instead. These extra flags must be emitted with
        // the other flags — BEFORE the `--` terminator — or clap would treat
        // them as positional data.
        let cmd_args = prepare_cli_args_for_mcp(spec.name, &args)
            .expect("validated MCP tool must have a CLI argument specification");

        let started = std::time::Instant::now();
        let deadline = tool_timeout(spec.name);
        let command_args: Vec<_> = cmd_args.iter().map(String::as_str).collect();

        // LLMs commonly retry or fan out an identical discovery call. Coalesce
        // only pure reads before acquiring a CLI slot: followers share the
        // leader's bounded result, while distinct work still gets backpressure.
        let output = if spec.effect == Effect::Read {
            let key = read_flight_key(&project_dir, spec.name, &cmd_args);
            match self.join_read_flight(key.clone()) {
                ReadFlight::Leader(sender) => {
                    let flight = self.guard_read_flight(key.clone(), sender);
                    let shared = match self.command_slots.clone().try_acquire_owned() {
                        Ok(_slot) => match run_aden_command_with_timeout(
                            &project_dir,
                            spec.name,
                            &command_args,
                            deadline,
                        )
                        .await
                        {
                            Ok(output) => SharedReadResult::Output(output),
                            Err(error) => SharedReadResult::CommandError(error),
                        },
                        Err(_) => SharedReadResult::Busy,
                    };
                    flight.finish(shared.clone());
                    shared
                }
                ReadFlight::Follower(receiver) => {
                    match await_shared_read(receiver, deadline).await {
                        Some(shared) => shared,
                        None => SharedReadResult::CommandError(
                            "timed out waiting for the identical in-flight read".to_string(),
                        ),
                    }
                }
            }
        } else {
            match self.command_slots.clone().try_acquire_owned() {
                Ok(_slot) => match run_aden_command_with_timeout(
                    &project_dir,
                    spec.name,
                    &command_args,
                    deadline,
                )
                .await
                {
                    Ok(output) => SharedReadResult::Output(output),
                    Err(error) => SharedReadResult::CommandError(error),
                },
                Err(_) => SharedReadResult::Busy,
            }
        };

        let output = match output {
            SharedReadResult::Output(output) => Ok(output),
            SharedReadResult::CommandError(error) => Err(error),
            SharedReadResult::Busy => {
                return Ok(CallToolResult::error(vec![Content::text(
                    server_busy_error(spec.name),
                )]));
            }
        };

        match output {
            Ok(clean) => {
                let agent_response = agent_response_for_mcp(spec.name, &clean);
                let selected_root = args
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| project_dir.to_str().unwrap_or("."));
                let scope_source = if had_explicit_path {
                    "explicit_path"
                } else if args.get("path").is_some() {
                    "repository_resolver"
                } else {
                    "workspace_root"
                };
                // A strict ask/asm budget is a hard serialized transport cap.
                // Do not let optional observability metadata displace evidence
                // or replace the CLI's minimal strict receipt.
                let strict_context = matches!(spec.name, "ask" | "asm")
                    && args
                        .get("strict")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true);
                let received = if strict_context {
                    agent_response
                } else {
                    attach_execution_receipt(
                        &agent_response,
                        selected_root,
                        scope_source,
                        started.elapsed(),
                        deadline,
                    )
                };
                let bounded = enforce_mcp_response_budget(spec.name, &args, &received);
                Ok(CallToolResult::success(vec![Content::text(bounded)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(
                agent_error_for_mcp(spec.name, &e),
            )])),
        }
    }

    async fn on_roots_list_changed(&self, context: NotificationContext<RoleServer>) {
        if self.pinned {
            return;
        }
        if let Ok(list) = context.peer.list_roots().await {
            let roots = file_roots(&list.roots);
            if roots.is_empty() {
                self.mark_workspace_unavailable_if_roots_seen();
            } else {
                self.set_workspace_roots(roots);
            }
        }
    }
}

/// Update `project_dir` from MCP Roots, then common host workspace env vars.
async fn refresh_project_from_client(server: &AdenMcpServer, context: &RequestContext<RoleServer>) {
    if let Ok(list) = context.peer.list_roots().await {
        let roots = file_roots(&list.roots);
        if !roots.is_empty() {
            server.set_workspace_roots(roots);
            return;
        }
        if let Some(dir) = project_from_env_hints() {
            server.set_workspace_roots(vec![dir]);
        } else {
            server.mark_workspace_unavailable_if_roots_seen();
        }
        return;
    }
    if let Some(dir) = project_from_env_hints() {
        server.set_workspace_roots(vec![dir]);
    }
}

/// Every valid `file://` root reported by the host, in host order.
fn file_roots(roots: &[Root]) -> Vec<PathBuf> {
    roots
        .iter()
        .filter_map(|root| file_uri_to_path(&root.uri))
        .filter_map(|path| {
            if path.is_dir() {
                Some(path)
            } else {
                path.parent().filter(|p| p.is_dir()).map(Path::to_path_buf)
            }
        })
        .collect()
}

fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    decode_file_uri_path(uri, cfg!(windows)).map(PathBuf::from)
}

/// Decode an MCP `file://` Root into platform-native path text. Keeping the
/// platform choice explicit makes Windows drive/UNC behavior testable on every
/// CI host rather than hiding it behind `#[cfg(windows)]`.
fn decode_file_uri_path(uri: &str, windows: bool) -> Option<String> {
    let scheme = uri.get(..7)?;
    if !scheme.eq_ignore_ascii_case("file://") {
        return None;
    }
    let rest = &uri[7..];

    let encoded = if rest.starts_with('/') {
        rest.to_string()
    } else {
        let (authority, path) = rest.split_once('/')?;
        if authority.eq_ignore_ascii_case("localhost") {
            format!("/{path}")
        } else if windows && !authority.is_empty() && !path.is_empty() {
            // A non-local authority is a valid Windows UNC root:
            // file://server/share/repo -> //server/share/repo.
            format!("//{authority}/{path}")
        } else {
            // On Unix a remote authority is not a local filesystem path.
            return None;
        }
    };

    let decoded = percent_decode_str(&encoded).decode_utf8().ok()?;
    let decoded = if windows {
        decoded
            .strip_prefix('/')
            .filter(|path| path.as_bytes().get(1) == Some(&b':'))
            .unwrap_or(&decoded)
    } else {
        &decoded
    };
    (!decoded.is_empty()).then(|| decoded.to_string())
}

/// Host-specific workspace hints when Roots is empty/unsupported.
fn project_from_env_hints() -> Option<PathBuf> {
    // Use the platform-native path-list parser so Windows drive letters are not
    // mistaken for separators.
    if let Some(paths) = std::env::var_os("WORKSPACE_FOLDER_PATHS")
        && let Some(path) = std::env::split_paths(&paths).find(|path| path.is_dir())
    {
        return Some(path);
    }
    for key in [
        "VSCODE_WORKSPACE_FOLDER",
        "CURSOR_WORKSPACE",
        "CLAUDE_PROJECT_DIR",
    ] {
        if let Some(path) = std::env::var_os(key).map(PathBuf::from)
            && path.is_dir()
        {
            return Some(path);
        }
    }
    None
}

// ── Workspace scope resolution ──────────────────────────────

fn needs_repository_scope(tool: &str) -> bool {
    TOOLS.iter().any(|spec| {
        spec.name == tool
            && spec.effect == Effect::Read
            && spec.args.iter().any(|(name, _)| *name == "path")
    })
}

fn is_repository_root(path: &Path) -> bool {
    [
        ".git",
        ".aden",
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
    ]
    .iter()
    .any(|marker| path.join(marker).exists())
}

/// Return bounded, deterministic repository candidates. A host-provided
/// multi-root workspace is authoritative. For a single broad container root,
/// inspect immediate children only; never recursively crawl the workspace.
fn repository_candidates(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut candidates = if roots.len() > 1 {
        roots.iter().filter(|p| p.is_dir()).cloned().collect()
    } else if let Some(root) = roots.first() {
        if is_repository_root(root) {
            vec![root.clone()]
        } else {
            std::fs::read_dir(root)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .take(100)
                .map(|entry| entry.path())
                .filter(|path| path.is_dir() && is_repository_root(path))
                .collect()
        }
    } else {
        Vec::new()
    };
    candidates.sort();
    candidates.dedup();
    candidates
}

fn normalized_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn scope_request_text(args: &serde_json::Map<String, serde_json::Value>) -> String {
    args.values()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>()
        .join(" ")
}

fn ambiguous_workspace_error(tool: &str, roots: &[PathBuf]) -> String {
    let candidates: Vec<_> = roots
        .iter()
        .take(20)
        .map(|path| {
            serde_json::json!({
                "name": path.file_name().and_then(|s| s.to_str()).unwrap_or("repository"),
                "path": path,
            })
        })
        .collect();
    serde_json::json!({
        "schema_version": 1,
        "tool": tool,
        "error": {
            "code": "ambiguous_workspace",
            "message": "This workspace contains multiple repositories; choose one before running this graph operation.",
            "safe_to_retry": true,
            "recovery": "Retry with path set to one of the candidate paths.",
            "candidates": candidates,
        }
    })
    .to_string()
}

/// Return the directory from which the child CLI must run. Repository-scope
/// inference supplies an absolute directory; honoring it prevents a multi-root
/// request selected for repository B from executing under root A.
fn execution_dir_for_args(
    args: &serde_json::Map<String, serde_json::Value>,
    fallback: &Path,
) -> PathBuf {
    let Some(path) = args.get("path").and_then(|value| value.as_str()) else {
        return fallback.to_path_buf();
    };
    let path = PathBuf::from(path);
    if path.is_absolute() && path.is_dir() {
        path
    } else if path.is_absolute() && path.is_file() {
        path.parent().unwrap_or(fallback).to_path_buf()
    } else {
        fallback.to_path_buf()
    }
}

/// Add an inferred path only when selection is deterministic. Explicit caller
/// scope always wins; fuzzy and substring matching are intentionally forbidden.
fn resolve_repository_scope(
    tool: &str,
    args: &mut serde_json::Map<String, serde_json::Value>,
    workspace_roots: &[PathBuf],
) -> Result<(), String> {
    if !needs_repository_scope(tool) || args.get("path").is_some() {
        return Ok(());
    }
    let candidates = repository_candidates(workspace_roots);
    if candidates.len() <= 1 {
        if let Some(path) = candidates.first() {
            args.insert(
                "path".into(),
                serde_json::Value::String(path.display().to_string()),
            );
        }
        return Ok(());
    }

    let words = normalized_words(&scope_request_text(args));
    let matches: Vec<_> = candidates
        .iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| words.iter().any(|word| word == &name.to_lowercase()))
                .unwrap_or(false)
        })
        .collect();
    if matches.len() == 1 {
        args.insert(
            "path".into(),
            serde_json::Value::String(matches[0].display().to_string()),
        );
        Ok(())
    } else {
        Err(ambiguous_workspace_error(tool, &candidates))
    }
}

// ── Path confinement ────────────────────────────────────────

fn confine_path_args_to_roots(
    tool: &str,
    args: &serde_json::Map<String, serde_json::Value>,
    roots: &[PathBuf],
) -> Result<(), String> {
    if roots
        .iter()
        .any(|root| confine_path_args(tool, args, root).is_ok())
    {
        Ok(())
    } else {
        Err(format!(
            "path is outside the MCP workspace roots: {}",
            roots
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

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
    #[cfg(test)]
    if let Some(binary) = TEST_ADEN_BINARY.lock().ok().and_then(|value| value.clone()) {
        return binary;
    }
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

/// Test-only executable override, avoiding process-global environment mutation
/// while exercising the real MCP transport and child-process bridge.
#[cfg(test)]
static TEST_ADEN_BINARY: std::sync::Mutex<Option<std::ffi::OsString>> = std::sync::Mutex::new(None);

/// Hard ceiling on how long a single shelled-out `aden` invocation may run.
/// MCP tool calls are request/response, so a tool that never returns (a
/// `watch` daemon, `heal --watch`, or a runaway `gen` on a huge untrusted
/// repo) would otherwise block the JSON-RPC stream indefinitely. Time out
/// instead and surface a clean error to the caller.
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Cost-class deadline for one CLI child. Cheap discovery should not inherit a
/// two-minute stall budget, while builds and external verification retain the
/// hard ceiling needed for large repositories.
fn tool_timeout(tool: &str) -> std::time::Duration {
    match tool {
        "grep" | "locate" | "search" | "list" | "status" => std::time::Duration::from_secs(30),
        "ask" | "asm" | "query" | "understand" | "impact-diff" | "communities" => {
            std::time::Duration::from_secs(60)
        }
        _ => COMMAND_TIMEOUT,
    }
}

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
/// In particular, the nested `context_receipt` emitted by structured CLI read
/// commands is protocol-neutral metadata and must pass through untouched.
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

/// Preserve successful CLI JSON for the MCP response channel.
///
/// This narrow public adapter exists for cross-crate contract fixtures; MCP
/// clients should use the server rather than calling it directly.
#[doc(hidden)]
pub fn preserve_cli_output_for_mcp(tool: &str, raw: &str) -> String {
    let cleaned = clean_stdout(tool, raw);
    if !matches!(tool, "ask" | "asm" | "query" | "locate" | "understand")
        || serde_json::from_str::<serde_json::Value>(&cleaned).is_ok()
    {
        return cleaned;
    }

    // ask/asm currently render their token-dense context as text even when
    // the global JSON switch is present. MCP still needs a versioned,
    // machine-readable response contract, so preserve that text losslessly in
    // a minimal envelope. Once those CLI commands gain native JSON this branch
    // naturally disappears because the parsed JSON is returned unchanged.
    serde_json::json!({
        "schema_version": 1,
        "tool": tool,
        // Kept for AP-101A compatibility with existing MCP consumers.
        "output": cleaned,
        "result": {"output": cleaned},
        // Text-first CLI surfaces cannot expose a top-level payload field that
        // collides with receipt metadata.  Keep the receipt namespaced even
        // in this compatibility bridge; native JSON producers carry the
        // revision/fingerprint fields verbatim instead.
        "context_receipt": {
            "schema_version": 1,
            "freshness": "unavailable",
            "refresh_cause": "mcp_transport_receipt_unavailable"
        },
        "incomplete": true,
    })
    .to_string()
}

/// Final agent-facing response contract.  MCP read calls are always JSON and
/// always carry a receipt; this prevents a model from having to infer safety,
/// freshness, or truncation from terminal-oriented prose.  Native CLI JSON is
/// retained verbatim when it already contains a receipt.
#[doc(hidden)]
pub fn agent_response_for_mcp(tool: &str, raw: &str) -> String {
    let raw = if serde_json::from_str::<serde_json::Value>(raw).is_ok() {
        raw.to_string()
    } else {
        // Text-first read commands may inherit progress/freshness notes from
        // the CLI. They are terminal chrome, not answer context; remove only
        // the well-known renderer prefixes before placing the answer in the
        // MCP receipt envelope. Native JSON remains byte-for-byte semantic
        // equivalent to the CLI response.
        raw.lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !(trimmed.starts_with("INFO:")
                    || trimmed.starts_with("NOTE:")
                    || trimmed.starts_with("Generated ")
                    || trimmed.starts_with("Emitted "))
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let cleaned = preserve_cli_output_for_mcp(tool, &raw);
    let is_read = TOOLS
        .iter()
        .find(|spec| spec.name == tool)
        .is_some_and(|spec| spec.effect == Effect::Read);
    if !is_read {
        return cleaned;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(&cleaned).ok();
    let already_versioned = parsed.as_ref().is_some_and(|value| {
        value.get("context_receipt").is_some()
            || (matches!(tool, "ask" | "asm")
                && value
                    .get("schema_version")
                    .and_then(serde_json::Value::as_u64)
                    == Some(1)
                && (value.get("context").is_some() || value.get("documents").is_some()))
    });
    if already_versioned {
        return cleaned;
    }
    let payload = parsed.unwrap_or(serde_json::Value::String(cleaned));
    serde_json::json!({
        "schema_version": 1,
        "tool": tool,
        "result": payload,
        "context_receipt": {
            "schema_version": 1,
            "freshness": "unavailable",
            "refresh_cause": "mcp_transport_receipt_unavailable"
        },
        "incomplete": true
    })
    .to_string()
}

fn server_busy_error(tool: &str) -> String {
    serde_json::json!({
        "schema_version": 1,
        "tool": tool,
        "error": {
            "code": "server_busy",
            "message": format!(
                "Aden is already running its {MAX_CONCURRENT_CLI_CHILDREN}-command concurrency limit"
            ),
            "safe_to_retry": true,
            "concurrency_limit": MAX_CONCURRENT_CLI_CHILDREN,
            "recovery": format!(
                "retry only this call after an active call completes; keep concurrent Aden calls at or below {MAX_CONCURRENT_CLI_CHILDREN}"
            ),
        }
    })
    .to_string()
}

fn boundary_error_for_mcp(
    tool: &str,
    code: &str,
    message: &str,
    safe_to_retry: bool,
    recovery: &str,
) -> String {
    serde_json::json!({
        "schema_version": 1,
        "tool": tool,
        "error": {
            "code": code,
            "message": sanitize_error(message),
            "safe_to_retry": safe_to_retry,
            "recovery": recovery,
        }
    })
    .to_string()
}

fn machine_resolution_error(raw: &str) -> Option<serde_json::Value> {
    let value = raw
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())?;
    let error = value.get("error")?;
    let code = error.get("code")?.as_str()?;
    if !matches!(code, "ambiguous_symbol" | "anchor_not_found") {
        return None;
    }
    let message = sanitize_error(error.get("message")?.as_str()?);
    let collection_field = if code == "ambiguous_symbol" {
        "candidates"
    } else {
        "suggestions"
    };
    let values: Vec<String> = error
        .get(collection_field)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .take(32)
        .map(sanitize_error)
        .collect();
    Some(serde_json::json!({
        "schema_version": 1,
        "error": {
            "code": code,
            "message": message,
            (collection_field): values,
        }
    }))
}

/// Structured, actionable MCP error. The content remains sanitized, but the
/// recovery instruction and safety state are machine-readable for agents.
#[doc(hidden)]
pub fn agent_error_for_mcp(tool: &str, raw: &str) -> String {
    if let Some(machine) = machine_resolution_error(raw) {
        let error = &machine["error"];
        let code = error["code"].as_str().unwrap_or("command_failed");
        let message = error["message"].as_str().unwrap_or("aden command failed");
        let recovery = if code == "ambiguous_symbol" {
            "retry with one exact candidate anchor from error.candidates"
        } else {
            "inspect error.suggestions, then retry with one exact canonical anchor"
        };
        let response = boundary_error_for_mcp(tool, code, message, true, recovery);
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&response) else {
            return response;
        };
        let field = if code == "ambiguous_symbol" {
            "candidates"
        } else {
            "suggestions"
        };
        value["error"][field] = error[field].clone();
        return value.to_string();
    }

    // Compatibility fallback for older Aden binaries and non-resolution errors.
    let message = sanitize_error(raw);
    let is_read = TOOLS
        .iter()
        .find(|spec| spec.name == tool)
        .is_some_and(|spec| spec.effect == Effect::Read);
    let recovery = if message.contains("authoritative freshness required") {
        "wait for the active writer to finish, then retry; or unset ADEN_SKIP_AUTO_GEN for a refreshable read"
    } else if message.contains("timed out") {
        "retry with a narrower request or run the long-running workflow from a terminal"
    } else if message.contains("Ambiguous symbol") {
        "retry with one exact candidate anchor from error.candidates"
    } else if message.contains("Symbol or anchor") && message.contains("not found") {
        "run locate or grep for the requested name, then retry with a returned canonical anchor"
    } else if !is_read {
        "inspect the reported state before retrying; the operation may have changed project state"
    } else {
        "correct the named argument or project state, then retry this read; no project mutation was performed"
    };
    let code = if message.contains("timed out") {
        "timeout"
    } else if message.contains("authoritative freshness required") {
        "freshness_required"
    } else if message.contains("Ambiguous symbol") {
        "ambiguous_symbol"
    } else if message.contains("Symbol or anchor") && message.contains("not found") {
        "anchor_not_found"
    } else {
        "command_failed"
    };
    let response = boundary_error_for_mcp(tool, code, &message, is_read, recovery);
    let collection_field = match code {
        "ambiguous_symbol" => "candidates",
        "anchor_not_found" => "suggestions",
        _ => return response,
    };
    let recovery_anchors: Vec<&str> = message
        .lines()
        .filter_map(|line| line.strip_prefix("  - "))
        .collect();
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&response) else {
        return response;
    };
    value["error"][collection_field] = serde_json::json!(recovery_anchors);
    value.to_string()
}

fn attach_execution_receipt(
    response: &str,
    selected_root: &str,
    scope_source: &str,
    elapsed: std::time::Duration,
    deadline: std::time::Duration,
) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(response) else {
        return response.to_string();
    };
    let Some(object) = value.as_object_mut() else {
        return response.to_string();
    };
    object.insert(
        "execution".to_string(),
        serde_json::json!({
            "schema_version": 1,
            "selected_root": selected_root,
            "scope_source": scope_source,
            "elapsed_ms": elapsed.as_millis(),
            "deadline_ms": deadline.as_millis(),
            "subprocess_status": "success",
        }),
    );
    serde_json::to_string(&value).unwrap_or_else(|_| response.to_string())
}

const MINIMAL_INCOMPLETE_RECEIPT: &str =
    r#"{"context_receipt":{"schema_version":1},"incomplete":true}"#;

/// Enforce the caller's strict budget after MCP's own envelope is serialized.
/// JSON keys and string escaping are response bytes too.
#[doc(hidden)]
pub fn enforce_mcp_response_budget(
    tool: &str,
    args: &serde_json::Map<String, serde_json::Value>,
    response: &str,
) -> String {
    if !matches!(tool, "ask" | "asm") {
        return response.to_string();
    }
    let strict = args
        .get("strict")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    if !strict {
        return response.to_string();
    }
    let default_budget = if tool == "ask" { 4096 } else { 8192 };
    let budget = args
        .get("budget")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default_budget);
    if response.len().div_ceil(4) <= budget {
        response.to_string()
    } else {
        MINIMAL_INCOMPLETE_RECEIPT.to_string()
    }
}

/// Whether this tool historically forced `ADEN_SKIP_AUTO_GEN` (pre zero-friction).
/// Kept for tests; MCP no longer injects skip — silent gen is non-blocking
/// single-flight, so auto-fresh is safe. Hosts may still set `ADEN_SKIP_AUTO_GEN`
/// themselves (escape hatch; re-injected via `ADEN_*` allowlist).
#[cfg(test)]
fn mcp_skips_auto_gen(tool: &str) -> bool {
    TOOLS
        .iter()
        .find(|t| t.name == tool)
        .is_some_and(|t| t.effect == Effect::Read)
}

/// Stable, in-memory identity for a read execution. It is intentionally based
/// on the fully resolved argv and project path, so calls with different flags,
/// scopes, or graph freshness requirements never share a result.
fn read_flight_key(project_dir: &Path, tool: &str, args: &[String]) -> String {
    let mut key = String::with_capacity(
        project_dir.as_os_str().len() + tool.len() + args.iter().map(String::len).sum::<usize>(),
    );
    key.push_str(&project_dir.to_string_lossy());
    key.push('\u{1f}');
    key.push_str(tool);
    for arg in args {
        key.push('\u{1f}');
        key.push_str(arg);
    }
    key
}

/// Wait only for the already-running duplicate call, never for a general work
/// queue. The caller's normal cost-class deadline remains authoritative.
async fn await_shared_read(
    mut receiver: watch::Receiver<Option<SharedReadResult>>,
    deadline: std::time::Duration,
) -> Option<SharedReadResult> {
    if let Some(result) = receiver.borrow().clone() {
        return Some(result);
    }
    match tokio::time::timeout(deadline, receiver.changed()).await {
        Ok(Ok(())) => receiver.borrow().clone(),
        Ok(Err(_)) | Err(_) => None,
    }
}

async fn run_aden_command_with_timeout(
    project_dir: &Path,
    tool: &str,
    args: &[&str],
    deadline: std::time::Duration,
) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new(resolve_aden_binary());
    cmd.args(args).current_dir(project_dir).kill_on_drop(true);

    // SECURITY: do not leak host env (model keys, tokens, etc.) to the child `aden`.
    // Allowlist only what is required for basic operation (ADEN_* for config,
    // PATH/HOME for binaries and dirs, USER for some tools).
    cmd.env_clear();
    for (k, v) in std::env::vars() {
        if k.starts_with("ADEN_") || matches!(k.as_str(), "PATH" | "HOME" | "USER" | "SHELL") {
            cmd.env(&k, &v);
        }
    }
    // Request typed stderr only for this MCP-owned child. This overrides any
    // inherited ADEN_* value and leaves normal terminal CLI prose unchanged.
    cmd.env("ADEN_MCP_MACHINE_ERRORS", "1");
    // Zero-friction: do NOT force ADEN_SKIP_AUTO_GEN on reads. Silent gen
    // fail-opens under contention; shell and MCP share auto-fresh.

    // `kill_on_drop` is load-bearing: when the timeout drops the output future,
    // terminate the subprocess instead of letting it keep locks or mutate the
    // store after MCP has already reported failure.
    let child = cmd.output();

    let output = match tokio::time::timeout(deadline, child).await {
        Ok(result) => result.map_err(|e| format!("failed to run aden: {}", e))?,
        Err(_) => {
            return Err(format!(
                "aden command timed out after {}s. Long-running tools like `watch` \
                 are not usable over MCP (each tool call is request/response); run \
                 them from a terminal instead.",
                deadline.as_secs_f64()
            ));
        }
    };

    if output.status.success() {
        let raw = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(preserve_cli_output_for_mcp(tool, &raw))
    } else {
        let mut err = String::from_utf8_lossy(&output.stderr).into_owned();
        let out = String::from_utf8_lossy(&output.stdout);
        if !out.trim().is_empty() {
            err.push_str(&format!("\n(stdout): {}", out));
        }
        Err(preserve_cli_error_for_mcp(&err))
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

/// Apply the MCP transport's error policy to real CLI stderr.
///
/// This is public solely for cross-crate black-box contract fixtures. Keeping
/// the fixture on the production sanitizer prevents an imitation in the test
/// suite from drifting away from what an MCP caller actually receives.
#[doc(hidden)]
pub fn preserve_cli_error_for_mcp(raw: &str) -> String {
    machine_resolution_error(raw)
        .map(|value| value.to_string())
        .unwrap_or_else(|| sanitize_error(raw))
}

/// Replace absolute filesystem paths in `s` with a `<path>` placeholder so host
/// directory layout does not leak to the MCP client.
fn redact_abs_paths(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        // Absolute path on Unix (e.g., /home/user/repo) or Windows (e.g., C:\Users\repo).
        // A token-starting separator is preceded by start of string, whitespace, quote,
        // paren, bracket, or equals sign. Followed by an alphanumeric char.
        let token_start = i == 0
            || s[..i]
                .chars()
                .next_back()
                .is_some_and(|p| p.is_whitespace() || matches!(p, '\'' | '"' | '(' | '[' | '='));

        // Unix-style absolute path: / followed by alphanumeric
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

        // Windows drive path: a token-starting ASCII drive letter followed by
        // `:\` or `:/`. Requiring token_start is load-bearing: without it the
        // trailing `n:/` in an `aden://...` anchor looks like a drive path.
        let tail = &s[i..];
        if token_start
            && c.is_ascii_alphabetic()
            && tail.as_bytes().get(1) == Some(&b':')
            && matches!(tail.as_bytes().get(2), Some(b'\\' | b'/'))
        {
            while let Some(&(_, nc)) = chars.peek() {
                if nc.is_whitespace() || matches!(nc, '\'' | '"' | ')' | ']' | ',') {
                    break;
                }
                chars.next();
            }
            out.push_str("<path>");
            continue;
        }

        // UNC/device path: `\\server\share` (or the slash-normalized `//server/share`).
        // A leading `//` that belongs to a URI is not at a token boundary because
        // it follows the scheme colon, so protocol anchors remain intact.
        if token_start
            && ((c == '\\' && tail.as_bytes().get(1) == Some(&b'\\'))
                || (c == '/' && tail.as_bytes().get(1) == Some(&b'/')))
            && tail
                .as_bytes()
                .get(2)
                .is_some_and(u8::is_ascii_alphanumeric)
        {
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
    serve_with_options(project_dir, false).await
}

pub async fn serve_with_options(project_dir: PathBuf, pinned: bool) -> anyhow::Result<()> {
    let server = AdenMcpServer::with_options(project_dir, pinned);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::ClientHandler;
    use tokio::sync::Mutex as AsyncMutex;

    #[derive(Clone)]
    struct RootsClient {
        roots: Arc<RwLock<Vec<Root>>>,
    }

    impl ClientHandler for RootsClient {
        fn get_info(&self) -> ClientInfo {
            ClientInfo::default()
        }

        fn list_roots(
            &self,
            _context: rmcp::service::RequestContext<rmcp::service::RoleClient>,
        ) -> impl std::future::Future<Output = Result<ListRootsResult, McpError>> + Send + '_
        {
            let roots = self
                .roots
                .read()
                .map(|roots| roots.clone())
                .unwrap_or_default();
            std::future::ready(Ok(ListRootsResult::new(roots)))
        }
    }

    static MCP_ROOT_TEST_ENV: AsyncMutex<()> = AsyncMutex::const_new(());

    /// Locate the sibling CLI built by this `cargo test` invocation.
    ///
    /// Deriving from the running test executable honors `CARGO_TARGET_DIR` and
    /// custom target profiles. `CARGO_MANIFEST_DIR/../../target` does neither.
    fn aden_cli_test_binary() -> PathBuf {
        if let Some(path) = option_env!("CARGO_BIN_EXE_aden") {
            let path = PathBuf::from(path);
            if path.is_file() {
                return path;
            }
        }

        let mut path = std::env::current_exe().expect("current aden-mcp test executable");
        path.pop();
        if path.file_name().is_some_and(|name| name == "deps") {
            path.pop();
        }
        path.push(if cfg!(windows) { "aden.exe" } else { "aden" });
        assert!(
            path.is_file(),
            "aden CLI test binary missing beside Cargo test artifacts: {}",
            path.display()
        );
        path
    }

    #[cfg(unix)]
    #[test]
    fn timed_out_child_is_killed_before_delayed_mutation() {
        use std::os::unix::fs::PermissionsExt;

        let _env = MCP_ROOT_TEST_ENV.blocking_lock();
        let root = std::env::temp_dir().join(format!("aden-timeout-kill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let marker = root.join("late-mutation");
        let fake = root.join("fake-aden.sh");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\nsleep 1\nprintf late > '{}'\n", marker.display()),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&fake).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake, permissions).unwrap();
        *TEST_ADEN_BINARY.lock().unwrap() = Some(fake.into_os_string());

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime
            .block_on(run_aden_command_with_timeout(
                &root,
                "grep",
                &["grep", "needle", "."],
                std::time::Duration::from_millis(50),
            ))
            .unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        std::thread::sleep(std::time::Duration::from_millis(1200));
        assert!(
            !marker.exists(),
            "timed-out process survived and mutated project state"
        );

        *TEST_ADEN_BINARY.lock().unwrap() = None;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn mcp_children_receive_machine_error_mode() {
        use std::os::unix::fs::PermissionsExt;

        let _env = MCP_ROOT_TEST_ENV.blocking_lock();
        let root = std::env::temp_dir().join(format!("aden-machine-errors-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let fake = root.join("fake-aden.sh");
        std::fs::write(
            &fake,
            "#!/bin/sh\nprintf '{\"machine_errors\":\"%s\"}' \"$ADEN_MCP_MACHINE_ERRORS\"\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&fake).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake, permissions).unwrap();
        *TEST_ADEN_BINARY.lock().unwrap() = Some(fake.into_os_string());

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let output = runtime
            .block_on(run_aden_command_with_timeout(
                &root,
                "grep",
                &["grep", "needle", "."],
                std::time::Duration::from_secs(2),
            ))
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["machine_errors"], "1");

        *TEST_ADEN_BINARY.lock().unwrap() = None;
        std::fs::remove_dir_all(root).unwrap();
    }

    fn mcp_root_fixture(label: &str, symbol: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("aden-mcp-roots-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("src")).unwrap();
        std::fs::write(path.join("src/lib.rs"), format!("pub fn {symbol}() {{}}\n")).unwrap();
        path
    }

    fn file_uri(path: &Path) -> String {
        format!("file://{}", path.display())
    }

    #[test]
    fn test_tools_table_is_non_empty() {
        assert!(!TOOLS.is_empty(), "TOOLS table should not be empty");
    }

    #[test]
    fn tool_schema_exposes_effect_when_clients_hide_annotations() {
        for (tool, expected) in [("grep", "read"), ("gen", "rebuild"), ("test", "mutate")] {
            assert_eq!(
                tool_from_spec(spec(tool)).input_schema["x-aden-effect"],
                serde_json::json!(expected),
                "{tool} effect classification"
            );
        }
    }

    #[test]
    fn busy_error_tells_clients_how_to_back_off() {
        let value: serde_json::Value = serde_json::from_str(&server_busy_error("grep")).unwrap();
        assert_eq!(value["error"]["code"], "server_busy");
        assert_eq!(
            value["error"]["concurrency_limit"],
            serde_json::json!(MAX_CONCURRENT_CLI_CHILDREN)
        );
        assert_eq!(value["error"]["safe_to_retry"], true);
        assert!(
            value["error"]["recovery"]
                .as_str()
                .is_some_and(|text| text.contains("retry only this call"))
        );
    }

    #[test]
    fn legacy_ambiguous_symbol_errors_expose_structured_candidates() {
        let raw = "Ambiguous symbol 'parse'.\nCandidates:\n  - aden://module/a.rs#parse\n  - aden://module/b.rs#parse\nRecovery: use an exact anchor.";
        let value: serde_json::Value =
            serde_json::from_str(&agent_error_for_mcp("query", raw)).unwrap();
        assert_eq!(value["error"]["code"], "ambiguous_symbol");
        assert_eq!(value["error"]["safe_to_retry"], true);
        assert_eq!(
            value["error"]["candidates"],
            serde_json::json!(["aden://module/a.rs#parse", "aden://module/b.rs#parse"])
        );
        assert!(
            value["error"]["recovery"]
                .as_str()
                .is_some_and(|text| text.contains("exact candidate"))
        );
    }

    #[test]
    fn legacy_not_found_symbol_errors_expose_structured_suggestions() {
        let raw = "Symbol or anchor 'prase' not found.\nSuggestions:\n  - aden://module/a.rs#parse\n  - aden://module/b.rs#parse\nRecovery: run locate.";
        let value: serde_json::Value =
            serde_json::from_str(&agent_error_for_mcp("query", raw)).unwrap();
        assert_eq!(value["error"]["code"], "anchor_not_found");
        assert_eq!(value["error"]["safe_to_retry"], true);
        assert_eq!(
            value["error"]["suggestions"],
            serde_json::json!(["aden://module/a.rs#parse", "aden://module/b.rs#parse"])
        );
    }

    #[test]
    fn typed_resolution_errors_do_not_scrape_display_lines() {
        let raw = serde_json::json!({
            "schema_version": 1,
            "error": {
                "code": "anchor_not_found",
                "input": "prase",
                "message": "No prose list is required for this machine error.",
                "suggestions": [
                    "aden://module/a.rs#parse",
                    "/private/workspace/secret.rs#parse"
                ]
            }
        })
        .to_string();
        let preserved = preserve_cli_error_for_mcp(&format!("warning before error\n{raw}\n"));
        let preserved: serde_json::Value = serde_json::from_str(&preserved).unwrap();
        assert_eq!(preserved["error"]["code"], "anchor_not_found");
        assert_eq!(
            preserved["error"]["suggestions"][0],
            "aden://module/a.rs#parse"
        );
        assert_ne!(
            preserved["error"]["suggestions"][1],
            "/private/workspace/secret.rs#parse"
        );

        let value: serde_json::Value =
            serde_json::from_str(&agent_error_for_mcp("query", &preserved.to_string())).unwrap();
        assert_eq!(value["error"]["code"], "anchor_not_found");
        assert_eq!(
            value["error"]["suggestions"],
            preserved["error"]["suggestions"]
        );
        assert!(
            value["error"]["recovery"]
                .as_str()
                .is_some_and(|text| text.contains("error.suggestions"))
        );
    }

    #[tokio::test]
    async fn duplicate_reads_share_one_in_flight_result() {
        let server = AdenMcpServer::new(PathBuf::from("."));
        let key = read_flight_key(
            Path::new("/workspace"),
            "grep",
            &["grep".into(), "needle".into()],
        );
        let leader = match server.join_read_flight(key.clone()) {
            ReadFlight::Leader(sender) => sender,
            ReadFlight::Follower(_) => panic!("first read must lead"),
        };
        let follower = match server.join_read_flight(key.clone()) {
            ReadFlight::Follower(receiver) => receiver,
            ReadFlight::Leader(_) => panic!("duplicate read must follow"),
        };
        server.finish_read_flight(
            &key,
            leader,
            SharedReadResult::Output("shared result".to_string()),
        );
        assert!(matches!(
            await_shared_read(follower, std::time::Duration::from_millis(10)).await,
            Some(SharedReadResult::Output(result)) if result == "shared result"
        ));
        assert!(matches!(
            server.join_read_flight(key),
            ReadFlight::Leader(_)
        ));
    }

    #[test]
    fn abandoned_read_leader_releases_the_flight_key() {
        let server = AdenMcpServer::new(PathBuf::from("."));
        let key = "cancelled-read".to_string();
        let sender = match server.join_read_flight(key.clone()) {
            ReadFlight::Leader(sender) => sender,
            ReadFlight::Follower(_) => panic!("first read must lead"),
        };
        drop(server.guard_read_flight(key.clone(), sender));
        assert!(matches!(
            server.join_read_flight(key),
            ReadFlight::Leader(_)
        ));
    }

    #[test]
    fn read_flight_key_never_shares_different_arguments_or_projects() {
        let grep = vec!["grep".to_string(), "needle".to_string()];
        assert_ne!(
            read_flight_key(Path::new("/one"), "grep", &grep),
            read_flight_key(Path::new("/two"), "grep", &grep)
        );
        assert_ne!(
            read_flight_key(Path::new("/one"), "grep", &grep),
            read_flight_key(Path::new("/one"), "grep", &["grep".into(), "other".into()])
        );
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
        // The default surface starts with one token-lean whole-project map,
        // then the find -> comprehend -> blast-radius loop. Deterministic
        // discovery stays before heuristic `ask` routing.
        let mut essential: Vec<&str> = TOOLS
            .iter()
            .map(|t| t.name)
            .filter(|n| tool_tier(n) == Tier::Essential)
            .collect();
        essential.sort_by_key(|name| tool_display_rank(name));
        assert_eq!(
            essential,
            [
                "tree",
                "grep",
                "locate",
                "understand",
                "ask",
                "query",
                "asm"
            ],
            "Essential presentation order drifted; got: {essential:?}"
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
    fn hidden_tools_require_explicit_surface_opt_in() {
        assert!(tool_enabled("grep", Tier::Essential));
        assert!(!tool_enabled("check", Tier::Essential));
        assert!(!tool_enabled("gen", Tier::Standard));
        assert!(tool_enabled("gen", Tier::Full));
    }

    #[test]
    fn mcp_guidance_states_observed_retrieval_limits() {
        let ask = TOOLS.iter().find(|tool| tool.name == "ask").unwrap();
        let understand = TOOLS.iter().find(|tool| tool.name == "understand").unwrap();
        assert!(ask.description.contains("one bounded question"));
        assert!(ask.description.contains("needs_narrowing"));
        assert!(understand.description.contains("not proof of absence"));
        for required in [
            "return `needs_narrowing`",
            "No result does not prove absence",
            "native Git/filesystem/build/test tools",
            "Context is bounded, not complete source",
            "`path` defaults to the client workspace",
            "At most two distinct calls run concurrently",
            "omit `gen`, budgets, and tuning arguments",
        ] {
            assert!(
                SERVER_INSTRUCTIONS.contains(required),
                "missing MCP limitation guidance: {required}"
            );
        }
    }

    #[test]
    fn tool_deadlines_match_cost_classes() {
        assert_eq!(tool_timeout("grep").as_secs(), 30);
        assert_eq!(tool_timeout("query").as_secs(), 60);
        assert_eq!(tool_timeout("test").as_secs(), 120);
        assert_eq!(tool_timeout("gen").as_secs(), 120);
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
    fn essential_tool_schemas_hide_tuning_knobs_and_stay_token_lean() {
        assert!(
            SERVER_INSTRUCTIONS.len() < 1_600,
            "startup guidance grew to {} bytes",
            SERVER_INSTRUCTIONS.len()
        );
        let mut total_bytes = 0usize;
        for name in ESSENTIAL_TOOLS {
            let spec = TOOLS.iter().find(|tool| tool.name == *name).unwrap();
            let compact = tool_from_spec_for_surface(spec, Tier::Essential);
            total_bytes += compact.description.as_ref().map_or(0, |text| text.len());
            total_bytes += serde_json::to_vec(&compact.input_schema).unwrap().len();
        }
        assert!(
            total_bytes < 4_500,
            "essential registry grew to {total_bytes} bytes"
        );

        let ask_spec = TOOLS.iter().find(|tool| tool.name == "ask").unwrap();
        let compact = tool_from_spec_for_surface(ask_spec, Tier::Essential);
        let compact_props = compact.input_schema["properties"].as_object().unwrap();
        assert_eq!(
            compact_props.keys().cloned().collect::<Vec<_>>(),
            ["from", "path", "question"]
        );
        assert_eq!(
            compact.input_schema["required"],
            serde_json::json!(["question"])
        );
        assert!(!compact_props.contains_key("budget"));
        assert!(!compact_props.contains_key("strict"));
        assert!(!compact_props.contains_key("require_fresh"));

        let full = tool_from_spec_for_surface(ask_spec, Tier::Standard);
        let full_props = full.input_schema["properties"].as_object().unwrap();
        assert!(full_props.contains_key("budget"));
        assert!(full_props.contains_key("strict"));
        assert!(full_props.contains_key("require_fresh"));
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
        // Core MCP calls prefer the CLI's documented short form. This remains
        // a tool-specific mapping: unknown fields still use clap's kebab-case
        // long spelling rather than inventing aliases.
        let mut args = serde_json::Map::new();
        args.insert("edge_types".into(), serde_json::json!("uses,calls"));
        let out = build_cli_args(spec("asm"), &args, &[]);
        assert_eq!(out, vec!["asm", "-e", "uses,calls"]);
    }

    #[test]
    fn compact_flags_cover_the_core_navigation_loop() {
        let cases = [
            ("grep", "limit", "-n"),
            ("locate", "caller_of", "-c"),
            ("understand", "budget", "-b"),
            ("understand", "json", "--json"),
            ("ask", "edge_types", "-e"),
            ("asm", "from", "-f"),
            ("query", "backlinks", "-b"),
        ];
        for (tool, arg, expected) in cases {
            assert_eq!(compact_flag(tool, arg), Some(expected), "{tool}.{arg}");
        }
        assert_eq!(compact_flag("ask", "model"), None);
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
    fn mcp_defaults_assembly_tools_to_strict_before_positionals() {
        let mut args = serde_json::Map::new();
        args.insert("question".into(), serde_json::json!("what is X"));
        args.insert("path".into(), serde_json::json!("src"));
        let mut out = build_cli_args(spec("ask"), &args, &[]);
        apply_mcp_budget_defaults("ask", &args, &mut out);
        let strict = out.iter().position(|arg| arg == "-s").unwrap();
        let terminator = out.iter().position(|arg| arg == "--").unwrap();
        assert!(
            strict < terminator,
            "strict must remain a CLI flag: {out:?}"
        );

        // An explicit false is an intentional caller choice, not an omitted
        // default; preserve it for compatibility.
        args.insert("strict".into(), serde_json::json!(false));
        let mut explicit = build_cli_args(spec("ask"), &args, &[]);
        apply_mcp_budget_defaults("ask", &args, &mut explicit);
        assert!(!explicit.iter().any(|arg| arg == "-s"));

        let mut asm_args = serde_json::Map::new();
        asm_args.insert("from".into(), serde_json::json!("mod-x"));
        asm_args.insert("path".into(), serde_json::json!("src"));
        let mut asm_out = build_cli_args(spec("asm"), &asm_args, &[]);
        apply_mcp_budget_defaults("asm", &asm_args, &mut asm_out);
        let strict = asm_out.iter().position(|arg| arg == "-s").unwrap();
        let terminator = asm_out.iter().position(|arg| arg == "--").unwrap();
        assert!(
            strict < terminator,
            "asm strict must remain a CLI flag: {asm_out:?}"
        );
    }

    #[test]
    fn mcp_tree_defaults_to_bounded_symbols_but_preserves_explicit_opt_out() {
        let server = AdenMcpServer::new(PathBuf::from("."));
        let tool = server.get_tool("tree").unwrap();
        assert_eq!(tool.input_schema["properties"]["symbols"]["default"], true);

        let omitted = prepare_cli_args_for_mcp("tree", &serde_json::Map::new()).unwrap();
        assert!(omitted.iter().any(|arg| arg == "--symbols"));

        let mut explicit_false = serde_json::Map::new();
        explicit_false.insert("symbols".into(), serde_json::json!(false));
        let graphical = prepare_cli_args_for_mcp("tree", &explicit_false).unwrap();
        assert!(!graphical.iter().any(|arg| arg == "--symbols"));

        let mut explicit_true = serde_json::Map::new();
        explicit_true.insert("symbols".into(), serde_json::json!(true));
        let compact = prepare_cli_args_for_mcp("tree", &explicit_true).unwrap();
        assert!(compact.iter().any(|arg| arg == "--symbols"));
    }

    #[test]
    fn read_tools_expose_and_forward_authoritative_freshness() {
        let server = AdenMcpServer::new(PathBuf::from("."));
        let tool = server.get_tool("grep").unwrap();
        assert_eq!(
            tool.input_schema["properties"]["require_fresh"]["default"],
            false
        );

        let mut args = serde_json::Map::new();
        args.insert("pattern".into(), serde_json::json!("needle"));
        args.insert("require_fresh".into(), serde_json::json!(true));
        let argv = prepare_cli_args_for_mcp("grep", &args).unwrap();
        assert_eq!(argv.first().map(String::as_str), Some("--require-fresh"));
        assert!(argv.iter().any(|arg| arg == "grep"));

        assert!(
            server.get_tool("gen").unwrap().input_schema["properties"]
                .get("require_fresh")
                .is_none(),
            "mutating tools must not advertise a read-authority option"
        );
    }

    #[tokio::test]
    async fn mcp_roots_switch_routes_require_fresh_reads_to_the_new_workspace() {
        // This uses rmcp's real duplex client/server transport.  It is not a
        // direct `set_project_dir` unit test: each tool call makes the protocol
        // `roots/list` request that a real MCP host answers.
        let _env = MCP_ROOT_TEST_ENV.lock().await;
        let root_a = mcp_root_fixture("a", "only_a");
        let root_b = mcp_root_fixture("b", "only_b");
        let cli = aden_cli_test_binary();
        *TEST_ADEN_BINARY.lock().unwrap() = Some(cli.into_os_string());

        let roots = Arc::new(RwLock::new(vec![Root::new(file_uri(&root_a))]));
        let client_handler = RootsClient {
            roots: roots.clone(),
        };
        let (server_transport, client_transport) = tokio::io::duplex(16 * 1024);
        let server = AdenMcpServer::new(root_a.clone());
        let server_task = tokio::spawn(async move { server.serve(server_transport).await });
        let client = client_handler.serve(client_transport).await.unwrap();

        let call = |needle: &str| {
            CallToolRequestParams::new("grep").with_arguments(
                serde_json::json!({"pattern": needle, "require_fresh": true})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
        };
        let a = client.call_tool(call("only_a")).await.unwrap();
        let a_text = a.content[0].raw.as_text().unwrap().text.clone();
        assert!(a_text.contains("only_a"), "A response: {a_text}");
        let a_json: serde_json::Value = serde_json::from_str(&a_text).unwrap();

        *roots.write().unwrap() = vec![Root::new(file_uri(&root_b))];
        let b = client.call_tool(call("only_b")).await.unwrap();
        let b_text = b.content[0].raw.as_text().unwrap().text.clone();
        assert!(b_text.contains("only_b"), "B response: {b_text}");
        assert!(
            !b_text.contains("only_a"),
            "A leaked into B response: {b_text}"
        );
        let b_json: serde_json::Value = serde_json::from_str(&b_text).unwrap();
        assert_eq!(b_json["context_receipt"]["freshness"], "current");
        assert_ne!(
            a_json["context_receipt"]["graph_revision"],
            b_json["context_receipt"]["graph_revision"]
        );
        assert_ne!(
            a_json["context_receipt"]["observed_source_fingerprint"],
            b_json["context_receipt"]["observed_source_fingerprint"]
        );

        // Clearing Roots after a valid workspace must invalidate the old root,
        // not silently keep querying repository B.
        *roots.write().unwrap() = Vec::new();
        let unavailable = client.call_tool(call("only_b")).await.unwrap();
        let unavailable_text = unavailable.content[0].raw.as_text().unwrap().text.clone();
        let unavailable_json: serde_json::Value = serde_json::from_str(&unavailable_text).unwrap();
        assert_eq!(unavailable_json["error"]["code"], "workspace_unavailable");
        assert_eq!(unavailable_json["error"]["safe_to_retry"], true);

        client.cancel().await.unwrap();
        let _ = server_task.await;
        *TEST_ADEN_BINARY.lock().unwrap() = None;
    }

    #[tokio::test]
    async fn ap107_live_mcp_strict_budget_counts_the_serialized_transport() {
        // AP-107 must exercise the production rmcp duplex transport rather
        // than the response adapter directly. The client sees exactly the
        // serialized text handed to an LLM after MCP has added its envelope.
        let _env = MCP_ROOT_TEST_ENV.lock().await;
        let root = mcp_root_fixture("ap107-budget", "budget_symbol");
        let cli = aden_cli_test_binary();
        let direct_cli = cli.clone();
        *TEST_ADEN_BINARY.lock().unwrap() = Some(cli.into_os_string());

        let roots = Arc::new(RwLock::new(vec![Root::new(file_uri(&root))]));
        let client_handler = RootsClient { roots };
        let (server_transport, client_transport) = tokio::io::duplex(16 * 1024);
        let server = AdenMcpServer::new(root.clone());
        let server_task = tokio::spawn(async move { server.serve(server_transport).await });
        let client = client_handler.serve(client_transport).await.unwrap();

        let reply = client
            .call_tool(
                CallToolRequestParams::new("ask").with_arguments(
                    serde_json::json!({"question":"budget_symbol", "budget":15})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .unwrap();
        let text = reply.content[0].raw.as_text().unwrap().text.clone();
        assert!(
            text.len().div_ceil(4) <= 15,
            "MCP serialized {} bytes over a 15-token strict budget: {text}",
            text.len()
        );
        let response: serde_json::Value = serde_json::from_str(&text).expect("MCP JSON");
        assert_eq!(response["schema_version"], 1);
        assert_eq!(response["truncated"], true, "tiny MCP response: {response}");

        // The MCP director is a transport adapter, not a second query engine.
        // Exercise the exact public CLI request it emits (same cwd, question,
        // strict mode, JSON mode, and 15-token budget) and compare semantic
        // payloads. There is no intentional delta for this bounded envelope.
        let direct = std::process::Command::new(direct_cli)
            .args([
                "ask",
                "budget_symbol",
                "--strict",
                "--budget",
                "15",
                "--json",
            ])
            .current_dir(&root)
            .output()
            .expect("equivalent public CLI ask");
        assert!(
            direct.status.success(),
            "CLI stderr: {}",
            String::from_utf8_lossy(&direct.stderr)
        );
        let direct: serde_json::Value = serde_json::from_slice(&direct.stdout).expect("CLI JSON");
        assert_eq!(
            response, direct,
            "MCP transport changed the strict CLI receipt payload"
        );

        client.cancel().await.unwrap();
        let _ = server_task.await;
        *TEST_ADEN_BINARY.lock().unwrap() = None;
    }

    #[tokio::test]
    async fn every_advertised_read_tool_has_a_real_mcp_agent_contract() {
        // E3: drive every declared read surface over rmcp's duplex transport,
        // not through the response adapter directly.  A tool may legitimately
        // reject a fixture request (for example a missing ADQ script), but its
        // error must still be structured and actionable; successes must carry
        // a receipt and never expose unstructured terminal output.
        let _env = MCP_ROOT_TEST_ENV.lock().await;
        let root = mcp_root_fixture("read-matrix", "matrix_symbol");
        let cli = aden_cli_test_binary();
        *TEST_ADEN_BINARY.lock().unwrap() = Some(cli.into_os_string());

        let roots = Arc::new(RwLock::new(vec![Root::new(file_uri(&root))]));
        let client_handler = RootsClient { roots };
        let (server_transport, client_transport) = tokio::io::duplex(16 * 1024);
        let server = AdenMcpServer::new(root);
        let server_task = tokio::spawn(async move { server.serve(server_transport).await });
        let client = client_handler.serve(client_transport).await.unwrap();

        let args = |tool: &str| -> serde_json::Map<String, serde_json::Value> {
            let value = match tool {
                "grep" => serde_json::json!({"pattern":"matrix_symbol","require_fresh":true}),
                "understand" => serde_json::json!({"symbol":"matrix_symbol","require_fresh":true}),
                "ask" => serde_json::json!({"question":"matrix_symbol","require_fresh":true}),
                "locate" => serde_json::json!({"symbol":"matrix_symbol","require_fresh":true}),
                "asm" => serde_json::json!({"from":"mod-project","require_fresh":true}),
                "query" => serde_json::json!({"from":"mod-project","require_fresh":true}),
                "search" => serde_json::json!({"query":"matrix_symbol"}),
                "viz" => serde_json::json!({"anchor":"mod-project"}),
                "federation" => serde_json::json!({"action":"config"}),
                "mcp" => serde_json::json!({"action":"list"}),
                "query-adq" => serde_json::json!({"script":"missing.adq"}),
                _ => serde_json::json!({}),
            };
            value.as_object().unwrap().clone()
        };

        for tool in agent_read_tools() {
            let request = CallToolRequestParams::new(tool).with_arguments(args(tool));
            let reply = client.call_tool(request).await.unwrap();
            let text = reply.content[0].raw.as_text().unwrap().text.clone();
            let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| {
                panic!("{tool} returned terminal chrome, not JSON: {e}: {text}")
            });
            if let Some(error) = value.get("error") {
                // This fixture contains a real, uniquely indexed symbol, so
                // understand must execute successfully. This catches transport
                // argv drift (for example injecting unsupported `-j`) instead
                // of accepting a structured command_failed envelope as parity.
                assert_ne!(tool, "understand", "understand transport failed: {value}");
                assert_eq!(error["safe_to_retry"], true, "{tool}: {value}");
                assert!(
                    error["recovery"].as_str().is_some_and(|v| !v.is_empty()),
                    "{tool}: {value}"
                );
            } else {
                assert_eq!(
                    value["context_receipt"]["schema_version"], 1,
                    "{tool}: {value}"
                );
                assert!(value["context_receipt"].is_object(), "{tool}: {value}");
            }
        }

        client.cancel().await.unwrap();
        let _ = server_task.await;
        *TEST_ADEN_BINARY.lock().unwrap() = None;
    }

    #[test]
    fn execution_receipt_records_scope_and_timing() {
        let response = attach_execution_receipt(
            r#"{"schema_version":1,"matches":[]}"#,
            "/workspace/aden",
            "repository_resolver",
            std::time::Duration::from_millis(42),
            std::time::Duration::from_secs(60),
        );
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["execution"]["selected_root"], "/workspace/aden");
        assert_eq!(value["execution"]["scope_source"], "repository_resolver");
        assert_eq!(value["execution"]["elapsed_ms"], 42);
        assert_eq!(value["execution"]["deadline_ms"], 60_000);
        assert_eq!(value["execution"]["subprocess_status"], "success");
    }

    #[test]
    fn mcp_envelope_is_counted_in_strict_response_budget() {
        let mut args = serde_json::Map::new();
        args.insert("budget".into(), serde_json::json!(15));
        let expanded = serde_json::json!({
            "schema_version": 1,
            "tool": "ask",
            "output": "context that fit before JSON keys and escaping were added"
        })
        .to_string();
        let bounded = enforce_mcp_response_budget("ask", &args, &expanded);
        assert_eq!(bounded, MINIMAL_INCOMPLETE_RECEIPT);
        assert!(bounded.len().div_ceil(4) <= 15);

        args.insert("strict".into(), serde_json::json!(false));
        assert_eq!(
            enforce_mcp_response_budget("ask", &args, &expanded),
            expanded
        );
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
    fn repository_scope_exact_name_inference_and_ambiguity() {
        let root = std::env::temp_dir().join(format!("aden-scope-{}", std::process::id()));
        let aden = root.join("aden");
        let pi = root.join("pi");
        std::fs::create_dir_all(aden.join(".git")).unwrap();
        std::fs::create_dir_all(pi.join(".git")).unwrap();

        let mut named = serde_json::Map::new();
        named.insert(
            "question".into(),
            serde_json::json!("is Aden documentation current?"),
        );
        resolve_repository_scope("ask", &mut named, std::slice::from_ref(&root)).unwrap();
        assert_eq!(named["path"], serde_json::json!(aden.display().to_string()));

        let mut neutral = serde_json::Map::new();
        neutral.insert("question".into(), serde_json::json!("where are the docs?"));
        let error = resolve_repository_scope("ask", &mut neutral, std::slice::from_ref(&root))
            .expect_err("neutral request must not guess");
        assert!(error.contains("ambiguous_workspace"));
        assert!(error.contains("safe_to_retry"));

        let mut grep = serde_json::Map::new();
        grep.insert("pattern".into(), serde_json::json!("authentication"));
        assert!(
            resolve_repository_scope("grep", &mut grep, std::slice::from_ref(&root)).is_err(),
            "all repository-bound read tools must share ambiguity handling"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inferred_repository_scope_also_selects_child_working_directory() {
        let root = std::env::temp_dir().join(format!("aden-root-routing-{}", std::process::id()));
        let api = root.join("api");
        let web = root.join("web");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::create_dir_all(&web).unwrap();
        let mut args = serde_json::Map::new();
        args.insert("path".into(), serde_json::json!(web.display().to_string()));
        assert_eq!(execution_dir_for_args(&args, &api), web);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_scope_wins_and_any_declared_root_is_allowed() {
        let root = std::env::temp_dir().join(format!("aden-roots-{}", std::process::id()));
        let api = root.join("api");
        let web = root.join("web");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::create_dir_all(&web).unwrap();
        let roots = vec![api, web.clone()];
        let mut args = serde_json::Map::new();
        args.insert("question".into(), serde_json::json!("inspect api"));
        args.insert("path".into(), serde_json::json!(web.display().to_string()));

        resolve_repository_scope("ask", &mut args, &roots).unwrap();
        assert_eq!(args["path"], serde_json::json!(web.display().to_string()));
        assert!(confine_path_args_to_roots("ask", &args, &roots).is_ok());

        std::fs::remove_dir_all(root).unwrap();
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
        // The MCP injects compact `-j` for grep so the agent gets the structured
        // envelope, not the human truncation footer.
        let mut args = serde_json::Map::new();
        args.insert("pattern".into(), serde_json::json!("TODO"));
        let cmd = build_cli_args(spec("grep"), &args, structured_output_flags("grep"));
        assert!(
            cmd.contains(&"-j".to_string()),
            "grep must request -j: {cmd:?}"
        );
        assert_eq!(cmd.iter().filter(|a| *a == "-j").count(), 1);
        // Critical ordering: -j (a flag) must come BEFORE the `--` terminator,
        // or clap would parse it as a positional. The pattern is a value
        // positional, so a `--` is present.
        let dd = cmd.iter().position(|a| a == "--");
        let js = cmd.iter().position(|a| a == "-j").unwrap();
        if let Some(dd) = dd {
            assert!(js < dd, "-j must precede the -- terminator: {cmd:?}");
        }
    }

    #[test]
    fn non_read_tools_get_no_structured_flags() {
        assert!(structured_output_flags("gen").is_empty());
    }

    #[test]
    fn gate_tools_request_json_and_max_issues() {
        assert_eq!(
            structured_output_flags("check"),
            &["-j", "--max-issues", "20"]
        );
        assert_eq!(
            structured_output_flags("heal"),
            &["-j", "--max-issues", "10"]
        );
        assert_eq!(structured_output_flags("status"), &["-j"]);
    }

    #[test]
    fn read_effect_tools_are_still_classified_for_docs() {
        // Historical Effect::Read classification (used for docs/tests only).
        // MCP no longer forces ADEN_SKIP_AUTO_GEN on these tools.
        assert!(mcp_skips_auto_gen("grep"));
        assert!(mcp_skips_auto_gen("understand"));
        assert!(!mcp_skips_auto_gen("gen"));
        assert!(!mcp_skips_auto_gen("ready"));
    }

    #[test]
    fn file_uri_to_path_parses_absolute() {
        let p = file_uri_to_path("file:///home/user/proj").unwrap();
        assert_eq!(p, PathBuf::from("/home/user/proj"));
        let p2 = file_uri_to_path("file://localhost/tmp/x").unwrap();
        assert_eq!(p2, PathBuf::from("/tmp/x"));
        assert_eq!(
            file_uri_to_path("file:///tmp/a%20b%23c%25/%E2%9C%93").unwrap(),
            PathBuf::from("/tmp/a b#c%/✓")
        );
        assert!(file_uri_to_path("https://example.test/repo").is_none());
    }

    #[test]
    fn file_uri_decoding_covers_unix_windows_and_unc_on_every_host() {
        assert_eq!(
            decode_file_uri_path("FILE:///tmp/a%20b", false).as_deref(),
            Some("/tmp/a b")
        );
        assert_eq!(
            decode_file_uri_path("file:///C:/Users/Ada/My%20Repo", true).as_deref(),
            Some("C:/Users/Ada/My Repo")
        );
        assert_eq!(
            decode_file_uri_path("file://LOCALHOST/C:/repo", true).as_deref(),
            Some("C:/repo")
        );
        assert_eq!(
            decode_file_uri_path("file://server/share/repo", true).as_deref(),
            Some("//server/share/repo")
        );
        assert!(decode_file_uri_path("file://server/share/repo", false).is_none());
        assert!(decode_file_uri_path("file://localhostevil/repo", false).is_none());
    }

    #[test]
    fn structured_output_tools_request_json() {
        // Tools with a real JSON envelope must auto-request --json over MCP.
        for t in [
            "grep",
            "search",
            "list",
            "test",
            "impact-diff",
            "communities",
            "ask",
            "asm",
            "query",
            "locate",
        ] {
            assert_eq!(structured_output_flags(t), &["-j"], "{t} should request -j");
        }
        assert_eq!(
            structured_output_flags("understand"),
            &["--json"],
            "understand's command-local JSON option has no -j alias"
        );
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

        // `aden new <name>` defaults `--lang` to rust, so the MCP schema must
        // not turn that optional CLI default into a required agent argument.
        let new_tool = server.get_tool("new").unwrap();
        let new_required = new_tool
            .input_schema
            .get("required")
            .unwrap()
            .as_array()
            .unwrap();
        assert!(new_required.iter().any(|v| v == "name"));
        assert!(!new_required.iter().any(|v| v == "lang"));
    }

    #[test]
    fn schema_exposes_shared_and_assembly_defaults() {
        let server = AdenMcpServer::new(PathBuf::from("."));
        let asm = server.get_tool("asm").unwrap();
        let properties = asm
            .input_schema
            .get("properties")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(properties["path"]["default"], serde_json::json!("."));
        assert_eq!(properties["depth"]["default"], serde_json::json!(2));
        assert_eq!(properties["budget"]["default"], serde_json::json!(8192));
        assert_eq!(properties["format"]["default"], serde_json::json!("json"));

        // `gen [PATH]...` has no clap-rendered default, so never advertise one.
        let gen_tool = server.get_tool("gen").unwrap();
        assert!(
            gen_tool.input_schema["properties"]["path"]
                .get("default")
                .is_none()
        );
    }

    #[test]
    fn query_max_results_is_exposed_and_forwarded() {
        let server = AdenMcpServer::new(PathBuf::from("."));
        let query = server.get_tool("query").unwrap();
        assert_eq!(
            query.input_schema["properties"]["max_results"]["default"],
            serde_json::json!(1000)
        );

        let mut args = serde_json::Map::new();
        args.insert("from".into(), serde_json::json!("cmd_query"));
        args.insert("max_results".into(), serde_json::json!(7));
        let argv = prepare_cli_args_for_mcp("query", &args).unwrap();
        let flag = argv.iter().position(|arg| arg == "--max-results").unwrap();
        assert_eq!(argv.get(flag + 1).map(String::as_str), Some("7"));
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
    fn runtime_validation_rejects_unknown_wrong_typed_and_invalid_enum_args() {
        let mut unknown = serde_json::Map::new();
        unknown.insert("dept".into(), serde_json::json!(2));
        assert!(
            validate_args("ask", &unknown)
                .unwrap_err()
                .contains("unknown")
        );

        let mut negative = serde_json::Map::new();
        negative.insert("depth".into(), serde_json::json!(-1));
        assert!(
            validate_args("ask", &negative)
                .unwrap_err()
                .contains("non-negative")
        );

        let mut bad_enum = serde_json::Map::new();
        bad_enum.insert("intent".into(), serde_json::json!("invent"));
        assert!(
            validate_args("ask", &bad_enum)
                .unwrap_err()
                .contains("one of")
        );
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
        assert!(out.contains(&"-s".to_string()));
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
    fn redact_abs_paths_handles_windows_drive_letters() {
        // Unix-style absolute path
        let out = redact_abs_paths("Error at /home/user/repo/src/main.rs:42");
        assert!(out.contains("<path>"), "Unix path not redacted: {out}");
        assert!(!out.contains("/home/user/repo"));

        // Windows-style drive letter with backslash separator
        let out = redact_abs_paths("Error at C:\\Users\\repo\\src\\main.rs:42");
        assert!(out.contains("<path>"), "Windows path not redacted: {out}");
        assert!(!out.contains("C:\\Users\\repo"));

        // Windows-style drive letter with forward slash separator (also valid)
        let out = redact_abs_paths("Error at C:/Users/repo/src/main.rs:42");
        assert!(out.contains("<path>"), "Windows path not redacted: {out}");

        // Mixed separators in same string
        let out = redact_abs_paths("Path1: /home/user and Path2: C:\\Users\\repo");
        assert_eq!(
            out.matches("<path>").count(),
            2,
            "Both paths should be redacted: {out}"
        );

        // Relative paths and Aden's protocol anchors must NOT be redacted.
        let out = redact_abs_paths(
            "Error at ./src/main.rs or ../config.yaml; anchor aden://module/a.rs#parse",
        );
        assert!(
            !out.contains("<path>"),
            "non-absolute value redacted: {out}"
        );
        assert!(out.contains("aden://module/a.rs#parse"));

        // UNC-style Windows path (network share).
        let out = redact_abs_paths(r"Path at \\server\share\repo");
        assert_eq!(out, "Path at <path>");
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
    fn text_first_context_tools_get_a_versioned_json_envelope() {
        for tool in ["ask", "asm"] {
            let value: serde_json::Value =
                serde_json::from_str(&preserve_cli_output_for_mcp(tool, "dense context\nblock"))
                    .unwrap();
            assert_eq!(value["schema_version"], 1);
            assert_eq!(value["tool"], tool);
            assert_eq!(value["output"], "dense context\nblock");
        }

        let native = r#"{"context_receipt":{"schema_version":1},"items":[]}"#;
        assert_eq!(preserve_cli_output_for_mcp("query", native), native);
    }

    #[test]
    fn text_first_agent_responses_drop_terminal_chrome_but_keep_legacy_output() {
        let response = agent_response_for_mcp(
            "ask",
            "INFO: Resolved query\nNOTE: index may lag\nuseful context",
        );
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["output"], "useful context");
        assert_eq!(value["result"]["output"], "useful context");
        assert_eq!(value["context_receipt"]["schema_version"], 1);
        assert_eq!(value["incomplete"], true);
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
        let rejected = serde_json::json!({"action":"remove"})
            .as_object()
            .unwrap()
            .clone();
        assert!(
            validate_args("federation", &rejected)
                .unwrap_err()
                .contains("not available over MCP")
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
