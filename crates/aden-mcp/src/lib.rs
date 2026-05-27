// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! MCP (Model Context Protocol) server for Aden.
//!
//! Exposes Aden's knowledge graph as MCP tools for Claude Code, Cursor,
//! Zed, Windsurf, and any other MCP-compatible AI agent platform.
//!
//! This is a thin wrapper that invokes the aden CLI - no business logic here.
//!
//! Protocol: JSON-RPC 2.0 over stdio.
//! Reference: https://modelcontextprotocol.io/specification

use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::Command;

/// Run aden CLI command and return output
fn run_aden_command(project_dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("aden")
        .args(args)
        .current_dir(project_dir)
        .output()
        .map_err(|e| format!("failed to run aden: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into())
    }
}

/// MCP JSON-RPC request.
#[derive(Debug, Deserialize, Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: Option<serde_json::Value>,
}

/// MCP JSON-RPC response.
#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

impl JsonRpcResponse {
    fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Option<serde_json::Value>, code: i32, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
        }
    }
}

/// Run the MCP server event loop.
/// Reads JSON-RPC requests from stdin, writes responses to stdout.
/// This is a thin wrapper that invokes the aden CLI - no graph pre-loading.
pub fn serve(project_dir: &Path) -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();

    let project_arc = std::sync::Arc::from(project_dir.to_path_buf());

    writeln!(
        stderr,
        "aden-mcp: serving project {} (CLI wrapper mode)",
        project_dir.display()
    )?;

    let reader = stdin.lock();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                let _ = writeln!(stderr, "aden-mcp: read error: {}", e);
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let req: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse::error(None, -32700, &format!("Parse error: {}", e));
                let _ = send_response(&mut stdout, &resp);
                continue;
            }
        };

        let resp = dispatch(&req, &project_arc);
        if let Err(e) = send_response(&mut stdout, &resp) {
            let _ = writeln!(stderr, "aden-mcp: write error: {}", e);
            break;
        }
    }

    Ok(())
}

fn send_response(stdout: &mut io::Stdout, resp: &JsonRpcResponse) -> io::Result<()> {
    let json = serde_json::to_string(resp).map_err(io::Error::other)?;
    writeln!(stdout, "{}", json)?;
    stdout.flush()
}

fn dispatch(
    req: &JsonRpcRequest,
    project_dir: &std::sync::Arc<std::path::PathBuf>,
) -> JsonRpcResponse {
    match req.method.as_str() {
        "initialize" => handle_initialize(req),
        "tools/list" => handle_tools_list(req),
        "tools/call" => handle_tools_call(req, project_dir),
        _ => JsonRpcResponse::error(
            req.id.clone(),
            -32601,
            &format!("Method not found: {}", req.method),
        ),
    }
}

fn handle_initialize(req: &JsonRpcRequest) -> JsonRpcResponse {
    let result = serde_json::json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "aden-mcp",
            "version": env!("CARGO_PKG_VERSION"),
        }
    });
    JsonRpcResponse::success(req.id.clone(), result)
}

fn handle_tools_list(req: &JsonRpcRequest) -> JsonRpcResponse {
    let tools = serde_json::json!([
        {
            "name": "init",
            "description": "Scaffold context templates and agent workspace files in a project directory. Creates .agent/ with onboarding, session, and constraint documents.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Target directory (default .)" }
                }
            }
        },
        {
            "name": "gen",
            "description": "Parse source files (any language) and generate knowledge graph contracts. Supports three-way merge to preserve human-written notes alongside generated content.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Source file or directory path" },
                    "auto": { "type": "boolean", "description": "Generate contracts for all source files in directory" },
                    "merge": { "type": "boolean", "description": "Preserve human-written blocks, update generated sections only" },
                    "propose": { "type": "boolean", "description": "Dry-run: preview what would change without writing files" },
                    "format": { "type": "string", "enum": ["adoc", "md", "adg"], "description": "Output format: adoc (default), md (Markdown), adg (compact JSON)" }
                }
            }
        },
        {
            "name": "check",
            "description": "Validate that all cross-references in the knowledge graph resolve to real nodes. Catches broken links, duplicate identifiers, and include cycles.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to check (default .)" }
                }
            }
        },
        {
            "name": "lint",
            "description": "Lint source files using tree-sitter AST analysis across multiple languages. Reports style, safety, and structural issues.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to lint (default .)" },
                    "severity": { "type": "string", "enum": ["Suggest", "Warn", "Error"], "description": "Minimum severity level to report" },
                    "fix": { "type": "boolean", "description": "Auto-fix issues where possible" },
                    "json": { "type": "boolean", "description": "Output results as JSON" }
                }
            }
        },
        {
            "name": "test",
            "description": "Discover and run tests across all languages. Supports Rust, Python, Go, TypeScript, Java, Ruby, PHP, C#, and more.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to test (default .)" },
                    "scope": { "type": "string", "enum": ["unit", "integration", "all"], "description": "Test scope" },
                    "filter": { "type": "string", "description": "Filter tests by name pattern" },
                    "list": { "type": "boolean", "description": "List available tests without running them" }
                }
            }
        },
        {
            "name": "asm",
            "description": "Assemble focused context from a graph node using BFS traversal. Returns dense, LLM-ready context within a token budget. BEST FOR: deep understanding of a specific topic. Use depth=1 for focused, depth=2-3 for broader.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "anchor": { "type": "string", "description": "Starting node identifier (use search or list to find identifiers)" },
                    "depth": { "type": "integer", "description": "Graph traversal depth. 1=focused, 2=moderate, 3=broad. Default: 2" },
                    "budget": { "type": "integer", "description": "Token budget (default 4096)" },
                    "edge_types": { "type": "string", "description": "Comma-separated edge types to follow (e.g. 'Uses,Calls'). Empty = follow all." },
                    "format": { "type": "string", "enum": ["aden", "adg"], "description": "Output format: aden=structured text (default), adg=compact JSON" }
                },
                "required": ["anchor"]
            }
        },
        {
            "name": "query",
            "description": "Get graph neighborhood as structured JSON. Shows what a node depends on, what depends on it, and transitive relationships. BEST FOR: understanding dependencies and call graphs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "anchor": { "type": "string", "description": "Starting node identifier" },
                    "depth": { "type": "integer", "description": "Traversal depth (default 2). Keep at 3 or less for performance." },
                    "backlinks": { "type": "boolean", "description": "Include incoming edges (what references this node)? Default: false" },
                    "impact": { "type": "boolean", "description": "Compute full transitive closure (all indirect dependents)? Default: false" },
                    "format": { "type": "string", "enum": ["table", "json"], "description": "Output format. Default: json" }
                }
            }
        },
        {
            "name": "ask",
            "description": "Ask a natural language question about the indexed codebase. Searches the knowledge graph, classifies intent, selects relevant nodes, and assembles a focused answer within a token budget. BEST FOR: exploratory questions and conceptual understanding.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": { "type": "string", "description": "Natural language question about the codebase" },
                    "budget": { "type": "integer", "description": "Max tokens (default 4096). Use 2048 for focused, 8192 for broad." },
                    "from": { "type": "string", "description": "Optional: pin traversal to a specific starting node" }
                },
                "required": ["question"]
            }
        },
        {
            "name": "search",
            "description": "Full-text search across all indexed nodes. Returns matching nodes with relevance scores. BEST FOR: finding nodes when you know specific keywords or symbol names.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search keywords or symbol names" },
                    "limit": { "type": "integer", "description": "Max results (default 10). Use 5 for focused, 20 for broad." }
                },
                "required": ["query"]
            }
        },
        {
            "name": "list",
            "description": "List all indexed nodes in the knowledge graph with their types and anchors.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filter": { "type": "string", "description": "Filter results by pattern" },
                    "verbose": { "type": "boolean", "description": "Include additional metadata per node" },
                    "limit": { "type": "integer", "description": "Max results (default 20)" }
                }
            }
        },
        {
            "name": "locate",
            "description": "Find where a named symbol is defined and where it is called or referenced. Works across all languages. BEST FOR: code navigation.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Symbol name to locate (function, type, class, variable, etc.)" },
                    "limit": { "type": "integer", "description": "Max results (default 10)" }
                },
                "required": ["symbol"]
            }
        },
        {
            "name": "heal",
            "description": "Detect and repair drift between source code and knowledge graph contracts. Identifies stale, missing, or orphaned nodes and proposes or applies fixes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to scan (default .)" },
                    "fix": { "type": "boolean", "description": "Auto-apply fixes for detected drift" },
                    "gc": { "type": "boolean", "description": "Remove orphaned contract nodes with no source" },
                    "propose": { "type": "boolean", "description": "Generate fix proposals without applying them" }
                }
            }
        },
        {
            "name": "ci_check",
            "description": "Run a full pre-commit validation suite: graph integrity check, project tests, linting, and secret scanning. Language-agnostic.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to check (default .)" }
                }
            }
        },
        {
            "name": "doctor",
            "description": "Diagnose the environment: detects installed tools based on the project's language, checks aden availability, and reports knowledge graph health.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to check (default .)" }
                }
            }
        },
        {
            "name": "audit",
            "description": "Security audit: scan source files for common vulnerabilities (OWASP patterns, hardcoded secrets, unsafe patterns) across all supported languages.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to audit (default .)" },
                    "strict": { "type": "boolean", "description": "Fail on any finding, even informational" }
                }
            }
        },
        {
            "name": "suggest",
            "description": "Describe what you want to accomplish and get a recommended aden command with an example.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "intent": { "type": "string", "description": "What you want to do (e.g. 'find all callers of authenticate', 'fix stale docs')" }
                },
                "required": ["intent"]
            }
        },
        {
            "name": "new",
            "description": "Create a new project from a language starter template with aden context scaffolding pre-configured.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Project name" },
                    "lang": { "type": "string", "enum": ["rust", "go", "js", "python"], "description": "Language template to use" },
                    "path": { "type": "string", "description": "Target directory" }
                },
                "required": ["name"]
            }
        },
        {
            "name": "kickoff",
            "description": "Create a structured kickoff document for a new initiative or feature. Captures goals, success criteria, risks, and next steps.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "brief": { "type": "string", "description": "Initiative brief or description" },
                    "path": { "type": "string", "description": "Output directory (default .)" }
                }
            }
        },
        {
            "name": "licenses",
            "description": "Generate a third-party dependency attribution report. Reads from Cargo.lock (Rust projects). Update NOTICE.md with the output.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Project root (default .)" },
                    "full": { "type": "boolean", "description": "Fetch full license text and metadata from the registry" }
                }
            }
        },
        {
            "name": "review",
            "description": "Semantic review of pending proposals: validates low-confidence changes against the knowledge graph within a token budget.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to review (default .)" },
                    "since": { "type": "string", "description": "Review changes since this date (ISO 8601 format)" },
                    "budget": { "type": "integer", "description": "Token budget for context assembly (default 4096)" }
                }
            }
        },
        {
            "name": "status",
            "description": "Show knowledge graph health at a glance: health score, orphan count, and drift summary. BEST FOR: quick pre-change health check.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Project root (default .)" }
                }
            }
        },
        {
            "name": "sync",
            "description": "Full graph refresh: generate contracts, validate cross-references, and detect drift in one pass. BEST FOR: resynchronising after large code changes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Project root (default .)" }
                }
            }
        }
    ]);
    JsonRpcResponse::success(req.id.clone(), serde_json::json!({ "tools": tools }))
}

fn handle_tools_call(
    req: &JsonRpcRequest,
    project_dir: &std::sync::Arc<std::path::PathBuf>,
) -> JsonRpcResponse {
    let params = match &req.params {
        Some(p) => p.clone(),
        None => return JsonRpcResponse::error(req.id.clone(), -32602, "Missing params"),
    };

    let tool_name = match params.get("name").and_then(|n| n.as_str()) {
        Some(n) => n,
        None => return JsonRpcResponse::error(req.id.clone(), -32602, "Missing tool name"),
    };

    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));

    let result = match tool_name {
        "init" => tool_generic(project_dir, &["init"]),
        "gen" => tool_gen(project_dir, &args),
        "check" => tool_generic(project_dir, &["check"]),
        "lint" => tool_lint(project_dir, &args),
        "test" => tool_test(project_dir, &args),
        "asm" => tool_asm(project_dir, &args),
        "query" => tool_query(project_dir, &args),
        "ask" => tool_ask(project_dir, &args),
        "search" => tool_search(project_dir, &args),
        "list" => tool_list(project_dir, &args),
        "locate" => tool_locate(project_dir, &args),
        "heal" => tool_heal(project_dir, &args),
        "ci_check" => tool_generic(project_dir, &["ci-check"]),
        "doctor" => tool_generic(project_dir, &["doctor"]),
        "audit" => tool_generic(project_dir, &["audit"]),
        "suggest" => tool_suggest(project_dir, &args),
        "new" => tool_new(project_dir, &args),
        "kickoff" => tool_kickoff(project_dir, &args),
        "licenses" => tool_licenses(project_dir, &args),
        "review" => tool_review(project_dir, &args),
        "status" => tool_status(project_dir, &args),
        "sync" => tool_sync(project_dir, &args),
        _ => {
            return JsonRpcResponse::error(
                req.id.clone(),
                -32602,
                &format!("Unknown tool: {}", tool_name),
            );
        }
    };

    match result {
        Ok(content) => JsonRpcResponse::success(req.id.clone(), content),
        Err(e) => JsonRpcResponse::error(req.id.clone(), -32603, &e),
    }
}

// ── Tool implementations ────────────────────────────────────

fn tool_generic(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    base_args: &[&str],
) -> Result<serde_json::Value, String> {
    let output = run_aden_command(project_dir, base_args)?;
    Ok(serde_json::json!({ "output": output }))
}

fn tool_gen(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut cmd = vec!["gen".to_string()];
    if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
        cmd.push(path.to_string());
    }
    if args.get("auto").and_then(|a| a.as_bool()).unwrap_or(false) {
        cmd.push("--auto".to_string());
    }
    if args.get("merge").and_then(|m| m.as_bool()).unwrap_or(false) {
        cmd.push("--merge".to_string());
    }
    if args.get("propose").and_then(|p| p.as_bool()).unwrap_or(false) {
        cmd.push("--propose".to_string());
    }
    if let Some(format) = args.get("format").and_then(|f| f.as_str()) {
        cmd.push("--format".to_string());
        cmd.push(format.to_string());
    }
    let output = run_aden_command(project_dir, &cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;
    Ok(serde_json::json!({ "output": output }))
}

fn tool_lint(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut cmd = vec!["lint".to_string()];
    if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
        cmd.push(path.to_string());
    }
    if let Some(severity) = args.get("severity").and_then(|s| s.as_str()) {
        cmd.push("--severity".to_string());
        cmd.push(severity.to_string());
    }
    if args.get("fix").and_then(|f| f.as_bool()).unwrap_or(false) {
        cmd.push("--fix".to_string());
    }
    if args.get("json").and_then(|j| j.as_bool()).unwrap_or(false) {
        cmd.push("--json".to_string());
    }
    let output = run_aden_command(project_dir, &cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;
    Ok(serde_json::json!({ "output": output }))
}

fn tool_test(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut cmd = vec!["test".to_string()];
    if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
        cmd.push(path.to_string());
    }
    if let Some(scope) = args.get("scope").and_then(|s| s.as_str()) {
        cmd.push("--scope".to_string());
        cmd.push(scope.to_string());
    }
    if let Some(filter) = args.get("filter").and_then(|f| f.as_str()) {
        cmd.push("--filter".to_string());
        cmd.push(filter.to_string());
    }
    if args.get("list").and_then(|l| l.as_bool()).unwrap_or(false) {
        cmd.push("--list".to_string());
    }
    let output = run_aden_command(project_dir, &cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;
    Ok(serde_json::json!({ "output": output }))
}

fn tool_asm(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let anchor = args
        .get("anchor")
        .and_then(|a| a.as_str())
        .ok_or("Missing 'anchor' argument")?;
    let budget = args.get("budget").and_then(|b| b.as_u64()).unwrap_or(4096) as usize;
    let depth = args.get("depth").and_then(|d| d.as_u64()).unwrap_or(2) as usize;

    let output = run_aden_command(
        project_dir,
        &[
            "asm", "--from", anchor, "--budget", &budget.to_string(), "--depth", &depth.to_string(),
        ],
    )?;
    Ok(serde_json::json!({ "content": output }))
}

fn tool_query(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut cmd = vec!["query".to_string()];

    if let Some(anchor) = args.get("anchor").and_then(|a| a.as_str()) {
        cmd.push("--from".to_string());
        cmd.push(anchor.to_string());
    }
    if let Some(depth) = args.get("depth").and_then(|d| d.as_u64()) {
        cmd.push("--depth".to_string());
        cmd.push(depth.to_string());
    }
    if args.get("backlinks").and_then(|b| b.as_bool()).unwrap_or(false) {
        cmd.push("--backlinks".to_string());
    }
    if args.get("impact").and_then(|i| i.as_bool()).unwrap_or(false) {
        cmd.push("--impact".to_string());
    }
    if let Some(format) = args.get("format").and_then(|f| f.as_str()) {
        cmd.push("--format".to_string());
        cmd.push(format.to_string());
    }

    let output = run_aden_command(project_dir, &cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;
    Ok(serde_json::json!({ "results": output }))
}

fn tool_ask(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let question = args
        .get("question")
        .and_then(|q| q.as_str())
        .ok_or("Missing 'question' argument")?;
    let budget = args.get("budget").and_then(|b| b.as_u64()).unwrap_or(4096) as usize;

    let output = run_aden_command(
        project_dir,
        &["ask", question, "--budget", &budget.to_string()],
    )?;
    Ok(serde_json::json!({ "content": output }))
}

fn tool_search(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let query = args
        .get("query")
        .and_then(|q| q.as_str())
        .ok_or("Missing 'query' argument")?;

    let mut cmd = vec!["search".to_string(), query.to_string()];
    if let Some(limit) = args.get("limit").and_then(|l| l.as_u64()) {
        cmd.push("--limit".to_string());
        cmd.push(limit.to_string());
    }

    let output = run_aden_command(project_dir, &cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;
    Ok(serde_json::json!({ "results": output }))
}

fn tool_list(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut cmd = vec!["list".to_string()];

    if let Some(filter) = args.get("filter").and_then(|f| f.as_str()) {
        cmd.push("--filter".to_string());
        cmd.push(filter.to_string());
    }
    if args.get("verbose").and_then(|v| v.as_bool()).unwrap_or(false) {
        cmd.push("--verbose".to_string());
    }
    if let Some(limit) = args.get("limit").and_then(|l| l.as_u64()) {
        cmd.push("--limit".to_string());
        cmd.push(limit.to_string());
    }

    let output = run_aden_command(project_dir, &cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;
    Ok(serde_json::json!({ "output": output }))
}

fn tool_locate(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let symbol = args
        .get("symbol")
        .and_then(|s| s.as_str())
        .ok_or("Missing 'symbol' argument")?;

    let mut cmd = vec!["locate".to_string(), "--symbol".to_string(), symbol.to_string()];
    if let Some(limit) = args.get("limit").and_then(|l| l.as_u64()) {
        cmd.push("--limit".to_string());
        cmd.push(limit.to_string());
    }

    let output = run_aden_command(project_dir, &cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;
    Ok(serde_json::json!({ "output": output }))
}

fn tool_heal(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut cmd = vec!["heal".to_string()];

    if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
        cmd.push(path.to_string());
    }
    if args.get("fix").and_then(|f| f.as_bool()).unwrap_or(false) {
        cmd.push("--fix".to_string());
    }
    if args.get("gc").and_then(|g| g.as_bool()).unwrap_or(false) {
        cmd.push("--gc".to_string());
    }
    if args.get("propose").and_then(|p| p.as_bool()).unwrap_or(false) {
        cmd.push("--propose".to_string());
    }

    let output = run_aden_command(project_dir, &cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>())?;
    Ok(serde_json::json!({ "output": output }))
}

fn tool_suggest(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let intent = args
        .get("intent")
        .and_then(|i| i.as_str())
        .ok_or("Missing 'intent' argument")?;

    let output = run_aden_command(project_dir, &["suggest", intent])?;
    Ok(serde_json::json!({ "suggestion": output }))
}

fn tool_new(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let name = args
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or("Missing 'name' argument")?;
    
    let mut cmd_args = vec!["new", name];
    if let Some(lang) = args.get("lang").and_then(|l| l.as_str()) {
        cmd_args.push("--lang");
        cmd_args.push(lang);
    }
    if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
        cmd_args.push(path);
    }

    let output = run_aden_command(project_dir, &cmd_args)?;
    Ok(serde_json::json!({ "result": output }))
}

fn tool_kickoff(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut cmd_args = vec!["kickoff"];
    
    if let Some(brief) = args.get("brief").and_then(|b| b.as_str()) {
        cmd_args.push("--brief");
        cmd_args.push(brief);
    }
    if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
        cmd_args.push(path);
    } else {
        cmd_args.push(".");
    }

    let output = run_aden_command(project_dir, &cmd_args)?;
    Ok(serde_json::json!({ "result": output }))
}

fn tool_licenses(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut cmd_args = vec!["licenses"];
    
    if let Some(full) = args.get("full").and_then(|f| f.as_bool()) {
        if full {
            cmd_args.push("--full");
        }
    }
    if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
        cmd_args.push(path);
    } else {
        cmd_args.push(".");
    }

    let output = run_aden_command(project_dir, &cmd_args)?;
    Ok(serde_json::json!({ "report": output }))
}

fn tool_review(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut cmd_args = vec!["review".to_string()];
    
    if let Some(since) = args.get("since").and_then(|s| s.as_str()) {
        cmd_args.push("--since".to_string());
        cmd_args.push(since.to_string());
    }
    if let Some(budget) = args.get("budget").and_then(|b| b.as_u64()) {
        cmd_args.push("--budget".to_string());
        cmd_args.push(budget.to_string());
    }
    if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
        cmd_args.push(path.to_string());
    } else {
        cmd_args.push(".".to_string());
    }

    let cmd_refs: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();
    let output = run_aden_command(project_dir, &cmd_refs)?;
    Ok(serde_json::json!({ "review": output }))
}

fn tool_status(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    _args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let cmd_args: Vec<&str> = vec!["status".as_ref()];
    let output = run_aden_command(project_dir, &cmd_args)?;
    Ok(serde_json::json!({ "status": output }))
}

fn tool_sync(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    _args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let cmd_args: Vec<&str> = vec!["sync".as_ref()];
    let output = run_aden_command(project_dir, &cmd_args)?;
    Ok(serde_json::json!({ "sync": output }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_rpc_parse() {
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let req: JsonRpcRequest = serde_json::from_str(line).unwrap();
        assert_eq!(req.method, "initialize");
    }
}
