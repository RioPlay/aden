// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! MCP (Model Context Protocol) server for Aden.
//!
//! Exposes Aden's knowledge graph as MCP tools for Claude Code, Cursor,
//! Zed, Windsurf, and any other MCP-compatible AI agent platform.
//!
//! Protocol: JSON-RPC 2.0 over stdio.
//! Reference: https://modelcontextprotocol.io/specification

use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};
use std::path::Path;

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
pub fn serve(project_dir: &Path) -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();

    // Pre-load graph if possible (best-effort; failures are logged to stderr).
    let graph = aden_graph::graph::AdenGraph::build_from_directory(project_dir).ok();
    let graph_ref = std::sync::Arc::new(std::sync::Mutex::new(graph));
    let project_arc = std::sync::Arc::from(project_dir.to_path_buf());

    writeln!(stderr, "aden-mcp: serving project {}", project_dir.display())?;

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

        let resp = dispatch(&req, &project_arc, &graph_ref);
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
    graph: &std::sync::Arc<std::sync::Mutex<Option<aden_graph::graph::AdenGraph>>>,
) -> JsonRpcResponse {
    match req.method.as_str() {
        "initialize" => handle_initialize(req),
        "tools/list" => handle_tools_list(req),
        "tools/call" => handle_tools_call(req, project_dir, graph),
        _ => JsonRpcResponse::error(req.id.clone(), -32601, &format!("Method not found: {}", req.method)),
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
        }
    ]);
    JsonRpcResponse::success(req.id.clone(), serde_json::json!({ "tools": tools }))
}

fn handle_tools_call(
    req: &JsonRpcRequest,
    project_dir: &std::sync::Arc<std::path::PathBuf>,
    graph: &std::sync::Arc<std::sync::Mutex<Option<aden_graph::graph::AdenGraph>>>,
) -> JsonRpcResponse {
    let params = match &req.params {
        Some(p) => p.clone(),
        None => return JsonRpcResponse::error(req.id.clone(), -32602, "Missing params"),
    };

    let tool_name = match params.get("name").and_then(|n| n.as_str()) {
        Some(n) => n,
        None => return JsonRpcResponse::error(req.id.clone(), -32602, "Missing tool name"),
    };

    let args = params.get("arguments").cloned().unwrap_or(serde_json::Value::Object(Default::default()));

    let result = match tool_name {
        "list_symbols" => tool_list_symbols(project_dir, &args),
        "impact_analysis" => tool_impact_analysis(graph, &args),
        "context_for" => tool_context_for(project_dir, graph, &args),
        _ => return JsonRpcResponse::error(req.id.clone(), -32602, &format!("Unknown tool: {}", tool_name)),
    };

    match result {
        Ok(content) => JsonRpcResponse::success(req.id.clone(), content),
        Err(e) => JsonRpcResponse::error(req.id.clone(), -32603, &e),
    }
}

// ── Tool implementations ────────────────────────────────────

fn tool_list_symbols(project_dir: &std::sync::Arc<std::path::PathBuf>, args: &serde_json::Value) -> Result<serde_json::Value, String> {
    let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("");

    // Walk and parse
    let docs = aden_parse::parse_directory(project_dir)
        .map_err(|e| format!("parse error: {}", e))?;

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
    graph: &std::sync::Arc<std::sync::Mutex<Option<aden_graph::graph::AdenGraph>>>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let anchor = args.get("anchor")
        .and_then(|a| a.as_str())
        .ok_or("Missing 'anchor' argument")?;

    let guard = graph.lock().map_err(|e| e.to_string())?;
    let g = guard.as_ref().ok_or("Graph not built")?;

    // Find backlinks (incoming edges)
    let mut callers = Vec::new();
    if let Some(idx) = g.get_index(anchor) {
        for neighbor in g.graph.neighbors_directed(idx, aden_graph::Direction::Incoming) {
            callers.push(serde_json::json!({
                "anchor": g.graph[neighbor].anchor.clone(),
                "type": format!("{:?}", g.graph[neighbor].doc.node_type),
            }));
        }
    }

    Ok(serde_json::json!({
        "target": anchor,
        "callers": callers,
        "count": callers.len(),
    }))
}

fn tool_context_for(
    _project_dir: &Path,
    graph: &std::sync::Arc<std::sync::Mutex<Option<aden_graph::graph::AdenGraph>>>,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let anchor = args.get("anchor")
        .and_then(|a| a.as_str())
        .ok_or("Missing 'anchor' argument")?;
    let budget = args.get("budget").and_then(|b| b.as_u64()).unwrap_or(4096) as usize;
    let depth = args.get("depth").and_then(|d| d.as_u64()).unwrap_or(3) as usize;

    let guard = graph.lock().map_err(|e| e.to_string())?;
    let g = guard.as_ref().ok_or("Graph not built")?;

    let opts = aden_asm::traverse::AssemblyOptions {
        start_anchor: anchor.to_string(),
        max_depth: depth,
        token_budget: budget,
        edge_types: vec![
            aden_core::EdgeType::Uses,
            aden_core::EdgeType::Calls,
            aden_core::EdgeType::Implements,
            aden_core::EdgeType::Documents,
        ],
        block_filter: Vec::new(),
    };

    let assembled = aden_asm::traverse::assemble(g, &opts)
        .map_err(|e| format!("assembly error: {}", e))?;

    Ok(serde_json::json!({
        "anchor": anchor,
        "budget": budget,
        "depth": depth,
        "tokens": assembled.len(),
        "content": assembled,
    }))
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
