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
            "name": "list_symbols",
            "description": "List all symbols (functions, types, modules) in the current project with their anchors.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Optional filter by name substring." }
                }
            }
        },
        {
            "name": "impact_analysis",
            "description": "Show what depends on a given symbol (reverse call graph / backlinks).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "anchor": { "type": "string", "description": "The symbol anchor (e.g. 'aden://module/requests/sessions.py#Session')." }
                },
                "required": ["anchor"]
            }
        },
        {
            "name": "context_for",
            "description": "Assemble a token-budgeted AsciiDoc context artifact for a symbol.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "anchor": { "type": "string", "description": "The symbol anchor." },
                    "budget": { "type": "integer", "description": "Max tokens (default 4096)." },
                    "depth": { "type": "integer", "description": "Graph traversal depth (default 3)." }
                },
                "required": ["anchor"]
            }
        },
        {
            "name": "ask",
            "description": "Ask a natural-language question about the codebase. Returns token-bounded context assembled from the knowledge graph.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": { "type": "string", "description": "Question to ask (e.g., 'How does authentication work?')" },
                    "budget": { "type": "integer", "description": "Max tokens (default 4096)." },
                    "from": { "type": "string", "description": "Pin to specific anchor (optional, enables smarter resolution)." }
                },
                "required": ["question"]
            }
        },
        {
            "name": "search",
            "description": "Full-text search across all contracts. Returns ranked results with snippets.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" }
                },
                "required": ["query"]
            }
        },
        {
            "name": "asm",
            "description": "Assemble context from a specific anchor with customizable depth and token budget.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "anchor": { "type": "string", "description": "Starting anchor (e.g., 'mod-aden-core')" },
                    "depth": { "type": "integer", "description": "Traversal depth (default 2)" },
                    "budget": { "type": "integer", "description": "Token budget (default 4096)" }
                },
                "required": ["anchor"]
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
        "list_symbols" => tool_list_symbols(project_dir, &args),
        "impact_analysis" => tool_impact_analysis(project_dir, &args),
        "context_for" => tool_context_for(project_dir, &args),
        "ask" => tool_ask(project_dir, &args),
        "search" => tool_search(project_dir, &args),
        "asm" => tool_asm(project_dir, &args),
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

fn tool_list_symbols(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("");

    // Walk and parse
    let docs =
        aden_parse::parse_directory(project_dir).map_err(|e| format!("parse error: {}", e))?;

    let mut symbols = Vec::new();
    for doc in docs {
        let name = doc.anchor.split('#').next_back().unwrap_or(&doc.anchor);
        if query.is_empty() || name.to_lowercase().contains(&query.to_lowercase()) {
            symbols.push(serde_json::json!({
                "anchor": doc.anchor,
                "type": format!("{:?}", doc.node_type),
                "file": doc.attributes.get("source_file"),
                "line": doc.attributes.get("start_line"),
            }));
        }
    }

    Ok(serde_json::json!(symbols))
}

fn tool_impact_analysis(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let anchor = args
        .get("anchor")
        .and_then(|a| a.as_str())
        .ok_or("Missing 'anchor' argument")?;

    let output = run_aden_command(project_dir, &["query", "--backlinks", anchor])?;
    Ok(serde_json::json!({ "impact": output }))
}

fn tool_context_for(
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let anchor = args
        .get("anchor")
        .and_then(|a| a.as_str())
        .ok_or("Missing 'anchor' argument")?;
    let budget = args.get("budget").and_then(|b| b.as_u64()).unwrap_or(4096) as usize;
    let depth = args.get("depth").and_then(|d| d.as_u64()).unwrap_or(3) as usize;

    let output = run_aden_command(
        project_dir,
        &[
            "asm",
            "--from",
            anchor,
            "--budget",
            &budget.to_string(),
            "--depth",
            &depth.to_string(),
        ],
    )?;
    Ok(serde_json::json!({ "content": output }))
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

    let output = run_aden_command(project_dir, &["search", query])?;
    Ok(serde_json::json!({ "results": output }))
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
            "asm",
            "--from",
            anchor,
            "--budget",
            &budget.to_string(),
            "--depth",
            &depth.to_string(),
        ],
    )?;
    Ok(serde_json::json!({ "content": output }))
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
