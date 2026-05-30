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
    model::*,
    service::{RequestContext, RoleServer},
    transport::stdio,
    ErrorData as McpError,
    ServerHandler, ServiceExt,
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
The `path` argument defaults to the current project directory for every tool.";

// ── Tool declaration ──────────────────────────────────────────

/// A tool the LLM can invoke.  Zero code per tool — just metadata.
struct ToolSpec {
    name: &'static str,
    description: &'static str,
    args: &'static [(&'static str, &'static str)], // (arg_name, arg_type: "string"|"boolean"|"integer")
}

/// Required argument names per tool (must match the `arg_name` strings in the
/// TOOLS table exactly). Anything not listed is optional — `path` always
/// defaults to the current project directory, so it is never required.
fn required_args(tool: &str) -> &'static [&'static str] {
    match tool {
        "ask" => &["question"],
        "search" => &["query"],
        "grep" => &["pattern"],
        "suggest" => &["intent"],
        "kickoff" => &["name"],
        "new" => &["name", "lang"],
        "workflow" => &["template"],
        // `from` carries the anchor for asm; the CLI marks it required.
        "asm" => &["from"],
        // CLI makes `status` optional (Option<String>); only id + task are required.
        "session" => &["agent-id", "task"],
        "emergency" => &["reason"],
        _ => &[],
    }
}

/// Returns true if `arg` should be passed positionally (no `--` prefix) for `tool`.
fn is_positional(tool: &str, arg: &str) -> bool {
    match (tool, arg) {
        // diagnose is the lone command whose `path` is a `--path` flag, not a
        // positional. Must come before the catch-all `path` rule below.
        ("diagnose", "path") => false,
        // path is positional for every other command
        (_, "path") => true,
        // ask:   aden ask <QUESTION> [DIR]
        ("ask", "question") => true,
        // search: aden search <QUERY> [DIR]
        ("search", "query") => true,
        // grep:  aden grep <PATTERN> [DIR]
        ("grep", "pattern") => true,
        // suggest: aden suggest <INTENT>
        ("suggest", "intent") => true,
        // new:   aden new <NAME> --lang <LANG> [DIR]  (lang is a flag, not positional)
        ("new", "name") => true,
        // workflow: aden workflow <TEMPLATE> [DIR]
        ("workflow", "template") => true,
        // query-adq: aden query-adq <SCRIPT> [DIR]
        ("query-adq", "script") => true,
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
        "grep" | "search" | "list" | "test" => &["--json"],
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
    let required = required_args(spec.name);
    if !required.is_empty() {
        schema.insert("required".to_string(), serde_json::json!(required));
    }
    Tool::new(spec.name, spec.description, Arc::new(schema))
}

/// Every MCP tool maps 1:1 to `aden <name> <args>`.
static TOOLS: &[ToolSpec] = &[
    ToolSpec { name: "init",       description: "Scaffold .agent/ workspace and templates.",                                                                    args: &[("path", "string")] },
    ToolSpec { name: "new",        description: "Create a new project from a language template.",                                                               args: &[("name", "string"), ("lang", "string"), ("path", "string")] },
    ToolSpec { name: "kickoff",    description: "Create a structured kickoff document.",                                                                         args: &[("name", "string"), ("interactive", "boolean"), ("path", "string")] },
    ToolSpec { name: "workflow",   description: "Instantiate templates with substitutions.",                                                                  args: &[("template", "string"), ("out", "string"), ("from", "string"), ("path", "string")] },
    ToolSpec { name: "gen",        description: "Generate contracts from source. Use --auto for whole project.",                                                args: &[("path", "string"), ("auto", "boolean"), ("quiet", "boolean")] },
    ToolSpec { name: "check",      description: "Validate cross-references and graph integrity.",                                                             args: &[("path", "string"), ("severity", "string")] },
    ToolSpec { name: "lint",       description: "Lint source files.",                                                                                         args: &[("path", "string"), ("severity", "string"), ("fix", "boolean"), ("json", "boolean"), ("unlimited", "boolean")] },
    ToolSpec { name: "test",       description: "Discover and run tests.",                                                                                      args: &[("path", "string"), ("scope", "string"), ("filter", "string"), ("list", "boolean")] },
    ToolSpec { name: "asm",        description: "Assemble a context prompt from the knowledge graph. Pass the anchor via `from`.",                               args: &[("from", "string"), ("path", "string"), ("depth", "integer"), ("budget", "integer"), ("edge_types", "string"), ("format", "string"), ("inspect", "boolean"), ("out", "string"), ("include_tag", "string"), ("exclude_tag", "string"), ("set_attr", "string"), ("silent", "boolean"), ("auto", "boolean"), ("strict", "boolean")] },
    ToolSpec { name: "query",      description: "Query the knowledge graph and emit JSON. Use backlinks=<anchor> for blast radius (what references a symbol) or impact=<anchor>.", args: &[("path", "string"), ("from", "string"), ("edge_type", "string"), ("depth", "integer"), ("backlinks", "string"), ("impact", "string"), ("format", "string")] },
    ToolSpec { name: "query-adq",  description: "Execute an Aden Query (.adq) script.",                                                                       args: &[("script", "string"), ("path", "string")] },
    ToolSpec { name: "ask",        description: "Ask a natural-language question. Routes to the best matching anchor.",                                            args: &[("question", "string"), ("budget", "integer"), ("from", "string"), ("model", "string")] },
    ToolSpec { name: "search",     description: "Full-text search with BM25 ranking.",                                                                           args: &[("query", "string"), ("limit", "integer"), ("offset", "integer"), ("doc_type", "string"), ("semantics", "boolean")] },
    ToolSpec { name: "list",       description: "List all indexed anchors.",                                                                                    args: &[("path", "string"), ("filter", "string"), ("limit", "integer"), ("verbose", "boolean"), ("semantics", "boolean"), ("offset", "integer"), ("unlimited", "boolean")] },
    ToolSpec { name: "grep",       description: "Structure-aware content search: find a pattern, each hit tagged with its enclosing symbol. Prefer over plain grep.", args: &[("pattern", "string"), ("path", "string"), ("regex", "boolean"), ("ignore_case", "boolean"), ("symbol_only", "boolean"), ("limit", "integer")] },
    ToolSpec { name: "locate",     description: "Find symbol definition and call sites. For JSON output pass format=json.",                                      args: &[("symbol", "string"), ("caller_of", "string"), ("path", "string"), ("limit", "integer"), ("show_context", "integer"), ("format", "string")] },
    ToolSpec { name: "heal",       description: "Self-healing documentation engine: scan for drift, propose patches, apply reviewed changes.",                      args: &[("path", "string"), ("fix", "boolean"), ("gc", "boolean"), ("propose", "boolean"), ("since", "string"), ("apply", "string"), ("watch", "string")] },
    ToolSpec { name: "status",     description: "Show project health status at a glance.",                                                                        args: &[("path", "string")] },
    ToolSpec { name: "sync",       description: "Run gen + check + heal in one pass.",                                                                          args: &[("path", "string")] },
    ToolSpec { name: "ci-check",   description: "Run all local CI gates: aden check, project tests, aden lint, secret scan, attribution check, OWASP audit, and merge-conflict-marker scan (plus warning-only clippy/cargo-audit).", args: &[("path", "string")] },
    ToolSpec { name: "regen",      description: "Regenerate all contracts from source (alias: gen . --auto --quiet).",                                          args: &[("path", "string")] },
    ToolSpec { name: "complete",   description: "Scan for incomplete contracts and report them. Note: automatic LLM filling is not yet implemented — this currently lists the contracts that need completion and (with --model) previews the prompt.", args: &[("path", "string"), ("dry-run", "boolean"), ("model", "string")] },
    ToolSpec { name: "watch",      description: "Watch source files and auto-regenerate contracts. WARNING: this is a long-running daemon and is not usable over MCP (the call will time out) — run it from a terminal. Use `gen`/`sync` for one-shot updates.", args: &[("path", "string"), ("sync", "boolean"), ("graph-sync", "boolean"), ("restore", "boolean")] },
    ToolSpec { name: "session",    description: "Append entry to .agent/session.adoc.",                                                                       args: &[("agent-id", "string"), ("task", "string"), ("status", "string")] },
    ToolSpec { name: "federation", description: "List or manage multi-repo workspace.",                                                                         args: &[("action", "string")] },
    ToolSpec { name: "emergency",  description: "Downgrade Forbid policies to Warn with justification.",                                                       args: &[("reason", "string"), ("path", "string"), ("ttl", "string")] },
    ToolSpec { name: "doctor",     description: "Environment diagnostics.",                                                                                     args: &[("path", "string")] },
    ToolSpec { name: "audit",      description: "OWASP-aligned security audit: scan source for vulnerabilities.",                                                args: &[("path", "string"), ("lang", "string"), ("format", "string"), ("strict", "boolean")] },
    ToolSpec { name: "suggest",    description: "Get a recommended aden command for your intent.",                                                                 args: &[("intent", "string")] },
    ToolSpec { name: "licenses",   description: "Generate third-party dependency attribution.",                                                                   args: &[("path", "string"), ("full", "boolean")] },
    ToolSpec { name: "review",     description: "Semantic review of pending proposals.",                                                                        args: &[("path", "string"), ("since", "string"), ("budget", "integer")] },
    ToolSpec { name: "mcp",        description: "MCP (Model Context Protocol) integration management. action is a subcommand: install, uninstall, list.",          args: &[("action", "string")] },
    ToolSpec { name: "diagnose",   description: "Deterministic diagnostic scanner for knowledge graphs.",                                                         args: &[("path", "string"), ("format", "string")] },
];

// ── ServerHandler impl ────────────────────────────────────────

impl ServerHandler for AdenMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(
            ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(Implementation::new(
            "aden-mcp",
            env!("CARGO_PKG_VERSION"),
        ))
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
        let tools: Vec<Tool> = TOOLS.iter().map(tool_from_spec).collect();
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
        let spec = TOOLS
            .iter()
            .find(|t| t.name == tool_name)
            .ok_or_else(|| {
                McpError::invalid_params(format!("unknown tool: {}", tool_name), None)
            })?;

        // SECURITY (audit HIGH-1): confine any caller-supplied filesystem path
        // to the server's project_dir before shelling out. The CLI does not
        // confine paths (find_project_root will happily resolve `/etc`), so an
        // MCP client could otherwise read or write arbitrary host files via the
        // `path` argument. Reject out-of-tree paths at the boundary.
        if let Err(e) = confine_path_args(&self.project_dir, &args) {
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
    project_dir: &Path,
    args: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    // The only caller-supplied argument that names a filesystem location is
    // `path` (every tool's project/dir argument). Other string args are
    // symbols, queries, anchors, etc. — not paths.
    let Some(raw) = args.get("path").and_then(|v| v.as_str()) else {
        return Ok(());
    };
    if raw.is_empty() {
        return Ok(());
    }

    let root = canonical_or_self(project_dir);
    let candidate = {
        let p = Path::new(raw);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
    };
    let resolved = resolve_existing_prefix(&candidate);

    if resolved.starts_with(&root) {
        Ok(())
    } else {
        Err(format!(
            "path '{}' resolves outside the project directory '{}' and is refused \
             (the MCP server is confined to its project root)",
            raw,
            root.display()
        ))
    }
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
        Err(err)
    }
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
        let dd = out.iter().position(|a| a == "--").expect("-- terminator present");
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
        // Absolute escape.
        let mut esc = serde_json::Map::new();
        esc.insert("path".into(), serde_json::json!("/etc"));
        assert!(confine_path_args(&proj, &esc).is_err(), "/etc must be refused");
        // `..` traversal escape.
        let mut trav = serde_json::Map::new();
        trav.insert("path".into(), serde_json::json!("../../../../etc"));
        assert!(confine_path_args(&proj, &trav).is_err(), ".. escape must be refused");
        // In-tree relative path is allowed.
        let mut ok = serde_json::Map::new();
        ok.insert("path".into(), serde_json::json!("."));
        assert!(confine_path_args(&proj, &ok).is_ok(), "'.' must be allowed");
        // No path arg → nothing to confine.
        assert!(confine_path_args(&proj, &serde_json::Map::new()).is_ok());
    }

    #[test]
    fn grep_requests_structured_json_output() {
        // The MCP injects `--json` for grep so the agent gets the structured
        // envelope, not the human truncation footer.
        let mut args = serde_json::Map::new();
        args.insert("pattern".into(), serde_json::json!("TODO"));
        let cmd = build_cli_args(spec("grep"), &args, structured_output_flags("grep"));
        assert!(cmd.contains(&"--json".to_string()), "grep must request --json: {cmd:?}");
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
            assert_eq!(structured_output_flags(t), &["--json"], "{t} should request --json");
        }
    }

    #[test]
    fn required_args_surface_in_schema() {
        let server = AdenMcpServer::new(PathBuf::from("."));
        let tool = server.get_tool("ask").unwrap();
        let req = tool.input_schema.get("required").unwrap().as_array().unwrap();
        assert!(req.iter().any(|v| v == "question"));
        // gen has no required args (path defaults to ".").
        let gen_tool = server.get_tool("gen").unwrap();
        assert!(gen_tool.input_schema.get("required").is_none());
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
