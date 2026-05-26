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
            "description": "Scaffold .agent/ templates in target repository",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Target directory (default .)" }
                }
            }
        },
        {
            "name": "gen",
            "description": "Parse source file(s) and emit .aden / .adoc contracts",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Source file or directory path" },
                    "auto": { "type": "boolean", "description": "Generate contracts for all source files in directory" },
                    "merge": { "type": "boolean", "description": "Preserve human blocks, update generated only" },
                    "propose": { "type": "boolean", "description": "Dry-run merge preview" },
                    "format": { "type": "string", "enum": ["adoc", "md", "adg"], "description": "Output format" }
                }
            }
        },
        {
            "name": "check",
            "description": "Verify all <<refs>> resolve to existing [[anchors]]",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to check (default .)" }
                }
            }
        },
        {
            "name": "lint",
            "description": "Lint source files using tree-sitter AST analysis",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to lint (default .)" },
                    "severity": { "type": "string", "enum": ["Suggest", "Warn", "Error"], "description": "Minimum severity level" },
                    "fix": { "type": "boolean", "description": "Auto-fix issues" },
                    "json": { "type": "boolean", "description": "Output JSON" }
                }
            }
        },
        {
            "name": "test",
            "description": "Discover and run tests across all languages",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to test (default .)" },
                    "scope": { "type": "string", "enum": ["unit", "integration", "all"], "description": "Test scope" },
                    "filter": { "type": "string", "description": "Filter tests by pattern" },
                    "list": { "type": "boolean", "description": "List tests without running" }
                }
            }
        },
        {
            "name": "asm",
            "description": "Assemble a context prompt from the knowledge graph",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "anchor": { "type": "string", "description": "Starting anchor" },
                    "depth": { "type": "integer", "description": "Traversal depth (default 2)" },
                    "budget": { "type": "integer", "description": "Token budget (default 4096)" },
                    "edge_types": { "type": "string", "description": "Comma-separated edge types" },
                    "format": { "type": "string", "enum": ["aden", "adg"], "description": "Output format" }
                },
                "required": ["anchor"]
            }
        },
        {
            "name": "query",
            "description": "Query the knowledge graph and emit JSON",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "anchor": { "type": "string", "description": "Starting anchor" },
                    "depth": { "type": "integer", "description": "Traversal depth (default 2)" },
                    "backlinks": { "type": "boolean", "description": "Find incoming edges" },
                    "impact": { "type": "boolean", "description": "Transitive closure" },
                    "format": { "type": "string", "enum": ["table", "json"], "description": "Output format" }
                }
            }
        },
        {
            "name": "ask",
            "description": "Ask a natural-language question; Aden resolves it to a subgraph and assembles context.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": { "type": "string", "description": "Question to ask" },
                    "budget": { "type": "integer", "description": "Max tokens (default 4096)" },
                    "from": { "type": "string", "description": "Pin to specific anchor" }
                },
                "required": ["question"]
            }
        },
        {
            "name": "search",
            "description": "Search the knowledge graph for documents matching a query",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "limit": { "type": "integer", "description": "Max results (default 10)" }
                },
                "required": ["query"]
            }
        },
        {
            "name": "list",
            "description": "List all anchors and contracts in the knowledge graph",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filter": { "type": "string", "description": "Filter by pattern" },
                    "verbose": { "type": "boolean", "description": "Verbose output" },
                    "limit": { "type": "integer", "description": "Max results (default 20)" }
                }
            }
        },
        {
            "name": "locate",
            "description": "Locate a symbol definition or its call sites in the knowledge graph",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Symbol name to find" },
                    "limit": { "type": "integer", "description": "Max results" }
                },
                "required": ["symbol"]
            }
        },
        {
            "name": "heal",
            "description": "Self-healing documentation engine: scan for drift, propose patches, apply reviewed changes",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to heal (default .)" },
                    "fix": { "type": "boolean", "description": "Auto-fix drift" },
                    "gc": { "type": "boolean", "description": "Garbage collect orphans" },
                    "propose": { "type": "boolean", "description": "Propose patches without applying" }
                }
            }
        },
        {
            "name": "ci_check",
            "description": "Run all local CI gates before committing (check, heal, test, secret-scan)",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to check (default .)" }
                }
            }
        },
        {
            "name": "doctor",
            "description": "Diagnose the environment: tool versions, repo health, signing keys",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to check (default .)" }
                }
            }
        },
        {
            "name": "audit",
            "description": "OWASP-style security audit: scan source for vulnerabilities",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to audit (default .)" },
                    "strict": { "type": "boolean", "description": "Fail on any finding" }
                }
            }
        },
        {
            "name": "suggest",
            "description": "AI assistant: describe what you want to do, get the right aden command",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "intent": { "type": "string", "description": "What you want to do" }
                },
                "required": ["intent"]
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
