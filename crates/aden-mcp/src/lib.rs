// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! MCP (Model Context Protocol) server for Aden.
//!
//! This is a **thin director** — no business logic, no graph pre-loading.
//! Each MCP tool maps 1:1 to an `aden` CLI subcommand.
//! When called, we convert JSON args to CLI flags and run the binary.
//!
//! Protocol: JSON-RPC 2.0 over stdio.

use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::Command;

// ── JSON-RPC types ──────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: Option<serde_json::Value>,
}

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
}

impl JsonRpcResponse {
    fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self { jsonrpc: "2.0".to_string(), id, result: Some(result), error: None }
    }
    fn error(id: Option<serde_json::Value>, code: i32, message: &str) -> Self {
        Self { jsonrpc: "2.0".to_string(), id, result: None, error: Some(JsonRpcError { code, message: message.to_string() }) }
    }
}

// ── Tool declaration ────────────────────────────────────────

/// A tool the LLM can invoke.  Zero code per tool — just metadata.
struct ToolSpec {
    name: &'static str,
    description: &'static str,
    args: &'static [(&'static str, &'static str)], // (arg_name, arg_type: "string"|"boolean"|"integer")
}

/// Every MCP tool maps 1:1 to `aden <name> <args>`.
static TOOLS: &[ToolSpec] = &[
    ToolSpec { name: "init",       description: "Scaffold .agent/ workspace and templates.",                                                                    args: &[("path", "string")] },
    ToolSpec { name: "gen",        description: "Generate contracts from source. Use --auto for whole project.",                                                args: &[("path", "string"), ("auto", "boolean"), ("merge", "boolean"), ("propose", "boolean"), ("format", "string"), ("quiet", "boolean")] },
    ToolSpec { name: "check",      description: "Validate cross-references and graph integrity.",                                                             args: &[("path", "string")] },
    ToolSpec { name: "lint",       description: "Lint source files.",                                                                                         args: &[("path", "string"), ("severity", "string"), ("fix", "boolean"), ("json", "boolean")] },
    ToolSpec { name: "test",       description: "Discover and run tests.",                                                                                      args: &[("path", "string"), ("scope", "string"), ("filter", "string"), ("list", "boolean")] },
    ToolSpec { name: "asm",        description: "Assemble LLM-dense context from a graph node via BFS.",                                                       args: &[("anchor", "string"), ("depth", "integer"), ("budget", "integer"), ("edge_types", "string"), ("format", "string"), ("from", "string")] },
    ToolSpec { name: "query",      description: "Return graph neighborhood as JSON.",                                                                            args: &[("anchor", "string"), ("depth", "integer"), ("backlinks", "boolean"), ("impact", "boolean"), ("format", "string")] },
    ToolSpec { name: "ask",        description: "Ask a natural-language question. Routes to the best matching anchor.",                                            args: &[("question", "string"), ("budget", "integer"), ("from", "string")] },
    ToolSpec { name: "search",     description: "Full-text search with BM25 ranking.",                                                                           args: &[("query", "string"), ("limit", "integer")] },
    ToolSpec { name: "list",       description: "List all indexed anchors.",                                                                                    args: &[("path", "string"), ("filter", "string"), ("limit", "integer"), ("verbose", "boolean")] },
    ToolSpec { name: "locate",     description: "Find symbol definition and call sites.",                                                                       args: &[("symbol", "string"), ("path", "string"), ("limit", "integer"), ("json", "boolean")] },
    ToolSpec { name: "heal",       description: "Detect drift (stale contracts, orphans).",                                                                     args: &[("path", "string"), ("fix", "boolean"), ("gc", "boolean"), ("propose", "boolean"), ("since", "string")] },
    ToolSpec { name: "status",     description: "Quick health score (0-100).",                                                                                   args: &[("path", "string")] },
    ToolSpec { name: "sync",       description: "Run gen + check + heal in one pass.",                                                                          args: &[("path", "string"), ("format", "string")] },
    ToolSpec { name: "ci_check",   description: "Full CI gates: build, test, lint, check.",                                                                     args: &[("path", "string"), ("severity", "string")] },
    ToolSpec { name: "doctor",     description: "Environment diagnostics.",                                                                                     args: &[("path", "string")] },
    ToolSpec { name: "audit",      description: "Security audit: scan for OWASP patterns.",                                                                      args: &[("path", "string"), ("strict", "boolean")] },
    ToolSpec { name: "suggest",    description: "Get a recommended aden command for an intent.",                                                                 args: &[("intent", "string")] },
    ToolSpec { name: "new",        description: "Create a new project from a language template.",                                                               args: &[("name", "string"), ("lang", "string"), ("path", "string")] },
    ToolSpec { name: "kickoff",    description: "Create a structured kickoff document.",                                                                         args: &[("brief", "string"), ("path", "string")] },
    ToolSpec { name: "licenses",   description: "Generate third-party dependency attribution.",                                                                   args: &[("path", "string"), ("full", "boolean")] },
    ToolSpec { name: "review",     description: "Semantic review of pending proposals.",                                                                        args: &[("path", "string"), ("since", "string"), ("budget", "integer")] },
];

// ── Serve loop ──────────────────────────────────────────────

pub fn serve(project_dir: &Path) -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    let project_arc = std::sync::Arc::from(project_dir.to_path_buf());

    writeln!(stderr, "aden-mcp: serving {}", project_dir.display())?;

    for line in stdin.lock().lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }

        let req: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let _ = send(&mut stdout, &JsonRpcResponse::error(None, -32700, &format!("parse error: {}", e)));
                continue;
            }
        };

        let resp = dispatch(&req, &project_arc);
        if send(&mut stdout, &resp).is_err() { break; }
    }
    Ok(())
}

fn send(stdout: &mut io::Stdout, resp: &JsonRpcResponse) -> io::Result<()> {
    writeln!(stdout, "{}", serde_json::to_string(resp).map_err(io::Error::other)?)?;
    stdout.flush()
}

fn dispatch(req: &JsonRpcRequest, project_dir: &std::sync::Arc<std::path::PathBuf>) -> JsonRpcResponse {
    match req.method.as_str() {
        "initialize" => handle_initialize(req),
        "tools/list" => handle_tools_list(req),
        "tools/call" => handle_tools_call(req, project_dir),
        _ => JsonRpcResponse::error(req.id.clone(), -32601, &format!("method not found: {}", req.method)),
    }
}

fn handle_initialize(req: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(req.id.clone(), serde_json::json!({
        "protocolVersion": "2024-11-05",
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "aden-mcp", "version": env!("CARGO_PKG_VERSION") }
    }))
}

/// Build MCP tool schemas from the declarative TOOLS table.
fn handle_tools_list(req: &JsonRpcRequest) -> JsonRpcResponse {
    let tools: Vec<serde_json::Value> = TOOLS.iter().map(|t| {
        let mut props = serde_json::Map::new();
        for &(name, ty) in t.args {
            let mut p = serde_json::Map::new();
            p.insert("type".to_string(), serde_json::json!(ty));
            props.insert(name.to_string(), serde_json::Value::Object(p));
        }
        serde_json::json!({
            "name": t.name,
            "description": t.description,
            "inputSchema": { "type": "object", "properties": props }
        })
    }).collect();
    JsonRpcResponse::success(req.id.clone(), serde_json::json!({ "tools": tools }))
}

/// Generic dispatch: JSON args → CLI flags → `aden <tool> <...>` → return stdout.
fn handle_tools_call(req: &JsonRpcRequest, project_dir: &std::sync::Arc<std::path::PathBuf>) -> JsonRpcResponse {
    let params = match &req.params {
        Some(p) => p.clone(),
        None => return JsonRpcResponse::error(req.id.clone(), -32602, "missing params"),
    };

    let tool_name = match params.get("name").and_then(|n| n.as_str()) {
        Some(n) => n,
        None => return JsonRpcResponse::error(req.id.clone(), -32602, "missing tool name"),
    };

    let args = params.get("arguments")
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));

    // Find the tool spec to know argument types
    let spec = match TOOLS.iter().find(|t| t.name == tool_name) {
        Some(s) => s,
        None => return JsonRpcResponse::error(req.id.clone(), -32602, &format!("unknown tool: {}", tool_name)),
    };

    // Build CLI args: `aden <name> [positional] [--flag|--key value] ...`
    let mut cmd_args: Vec<String> = vec![tool_name.to_string()];

    for &(arg_name, arg_type) in spec.args {
        let val = match args.get(arg_name) {
            Some(v) => v,
            None => continue,
        };
        match arg_type {
            "boolean" => {
                if val.as_bool().unwrap_or(false) {
                    cmd_args.push(format!("--{}", arg_name));
                }
            }
            "string" | "integer" => {
                let s = match arg_type {
                    "integer" => val.as_u64().map(|n| n.to_string()).or_else(|| val.as_i64().map(|n| n.to_string())),
                    _ => val.as_str().map(|s| s.to_string()),
                };
                if let Some(s) = s {
                    if arg_name == "path" && tool_name != "new" && tool_name != "kickoff" {
                        // "path" for most tools is positional → no --path prefix
                        cmd_args.push(s);
                    } else {
                        cmd_args.push(format!("--{}", arg_name));
                        cmd_args.push(s);
                    }
                }
            }
            _ => {}
        }
    }

    // Run
    let output = run_aden_command(project_dir, &cmd_args.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    match output {
        Ok(clean) => JsonRpcResponse::success(req.id.clone(), serde_json::json!({ "output": clean })),
        Err(e) => JsonRpcResponse::error(req.id.clone(), -32603, &e),
    }
}

// ── CLI bridge ──────────────────────────────────────────────

fn run_aden_command(project_dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("aden").args(args).current_dir(project_dir).output()
        .map_err(|e| format!("failed to run aden: {}", e))?;

    if output.status.success() {
        let raw = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(raw.lines()
            .filter(|l| !l.trim_start().starts_with("INFO:"))
            .filter(|l| !l.trim_start().starts_with("Generated"))
            .filter(|l| !l.trim_start().starts_with("Emitted"))
            .collect::<Vec<_>>().join("\n"))
    } else {
        let mut err = String::from_utf8_lossy(&output.stderr).into_owned();
        let out = String::from_utf8_lossy(&output.stdout);
        if !out.trim().is_empty() { err.push_str(&format!("\n(stdout): {}", out)); }
        Err(err)
    }
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

    #[test]
    fn test_tools_list_generates_schema() {
        let req = JsonRpcRequest { jsonrpc: "2.0".to_string(), id: None, method: "tools/list".to_string(), params: None };
        let resp = handle_tools_list(&req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let tools = result.get("tools").unwrap().as_array().unwrap();
        assert!(!tools.is_empty(), "tools/list should return non-empty list");
    }
}
