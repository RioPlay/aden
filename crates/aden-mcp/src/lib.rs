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

// ── Tool declaration ──────────────────────────────────────────

/// A tool the LLM can invoke.  Zero code per tool — just metadata.
struct ToolSpec {
    name: &'static str,
    description: &'static str,
    args: &'static [(&'static str, &'static str)], // (arg_name, arg_type: "string"|"boolean"|"integer")
}

/// Every MCP tool maps 1:1 to `aden <name> <args>`.
static TOOLS: &[ToolSpec] = &[
    ToolSpec { name: "init",       description: "Scaffold .agent/ workspace and templates.",                                                                    args: &[("path", "string")] },
    ToolSpec { name: "new",        description: "Create a new project from a language template.",                                                               args: &[("name", "string"), ("lang", "string"), ("path", "string")] },
    ToolSpec { name: "kickoff",    description: "Create a structured kickoff document.",                                                                         args: &[("brief", "string"), ("path", "string")] },
    ToolSpec { name: "workflow",   description: "Instantiate templates with substitutions.",                                                                  args: &[("template", "string"), ("output", "string"), ("from", "string")] },
    ToolSpec { name: "gen",        description: "Generate contracts from source. Use --auto for whole project.",                                                args: &[("path", "string"), ("auto", "boolean"), ("merge", "boolean"), ("propose", "boolean"), ("format", "string"), ("quiet", "boolean"), ("detect-out-dir", "boolean"), ("out-dir", "string")] },
    ToolSpec { name: "check",      description: "Validate cross-references and graph integrity.",                                                             args: &[("path", "string")] },
    ToolSpec { name: "lint",       description: "Lint source files.",                                                                                         args: &[("path", "string"), ("severity", "string"), ("fix", "boolean"), ("json", "boolean")] },
    ToolSpec { name: "test",       description: "Discover and run tests.",                                                                                      args: &[("path", "string"), ("scope", "string"), ("filter", "string"), ("list", "boolean")] },
    ToolSpec { name: "asm",        description: "Assemble a context prompt from the knowledge graph.",                                                          args: &[("anchor", "string"), ("depth", "integer"), ("budget", "integer"), ("edge_types", "string"), ("format", "string"), ("from", "string"), ("inspect", "boolean"), ("out", "string"), ("include_tag", "string"), ("exclude_tag", "string"), ("set_attr", "string"), ("silent", "boolean"), ("auto", "boolean"), ("strict", "boolean")] },
    ToolSpec { name: "query",      description: "Query the knowledge graph and emit JSON.",                                                                       args: &[("anchor", "string"), ("depth", "integer"), ("backlinks", "boolean"), ("impact", "boolean"), ("format", "string")] },
    ToolSpec { name: "query-adq",  description: "Execute an Aden Query (.adq) script.",                                                                       args: &[("script", "string"), ("path", "string")] },
    ToolSpec { name: "ask",        description: "Ask a natural-language question. Routes to the best matching anchor.",                                            args: &[("question", "string"), ("budget", "integer"), ("from", "string"), ("model", "string")] },
    ToolSpec { name: "search",     description: "Full-text search with BM25 ranking.",                                                                           args: &[("query", "string"), ("limit", "integer"), ("offset", "integer"), ("doc_type", "string"), ("semantics", "boolean")] },
    ToolSpec { name: "list",       description: "List all indexed anchors.",                                                                                    args: &[("path", "string"), ("filter", "string"), ("limit", "integer"), ("verbose", "boolean"), ("semantics", "boolean"), ("offset", "integer"), ("unlimited", "boolean")] },
    ToolSpec { name: "locate",     description: "Find symbol definition and call sites.",                                                                       args: &[("symbol", "string"), ("caller_of", "string"), ("path", "string"), ("limit", "integer"), ("json", "boolean"), ("show_context", "integer"), ("format", "string")] },
    ToolSpec { name: "heal",       description: "Self-healing documentation engine: scan for drift, propose patches, apply reviewed changes.",                      args: &[("path", "string"), ("fix", "boolean"), ("gc", "boolean"), ("propose", "boolean"), ("since", "string"), ("apply", "string"), ("watch", "string")] },
    ToolSpec { name: "status",     description: "Show project health status at a glance.",                                                                        args: &[("path", "string")] },
    ToolSpec { name: "sync",       description: "Run gen + check + heal in one pass.",                                                                          args: &[("path", "string")] },
    ToolSpec { name: "ci-check",   description: "Full CI gates: build, test, lint, check.",                                                                     args: &[("path", "string")] },
    ToolSpec { name: "regen",      description: "Regenerate all contracts from source (alias: gen . --auto --quiet).",                                          args: &[("path", "string")] },
    ToolSpec { name: "complete",   description: "Fill incomplete contracts with LLM-generated content.",                                                        args: &[("path", "string"), ("dry-run", "boolean"), ("model", "string")] },
    ToolSpec { name: "watch",      description: "Watch source files and auto-regenerate contracts.",                                                            args: &[("path", "string"), ("sync", "boolean"), ("graph-sync", "boolean"), ("restore", "boolean")] },
    ToolSpec { name: "session",    description: "Append entry to .agent/session.adoc.",                                                                       args: &[("agent-id", "string"), ("task", "string"), ("status", "string")] },
    ToolSpec { name: "federation", description: "List or manage multi-repo workspace.",                                                                         args: &[("action", "string")] },
    ToolSpec { name: "emergency",  description: "Downgrade Forbid policies to Warn with justification.",                                                       args: &[("reason", "string"), ("path", "string"), ("ttl", "string")] },
    ToolSpec { name: "doctor",     description: "Environment diagnostics.",                                                                                     args: &[("path", "string")] },
    ToolSpec { name: "audit",      description: "OWASP-style security audit: scan source for vulnerabilities.",                                                 args: &[("path", "string"), ("lang", "string"), ("format", "string"), ("strict", "boolean")] },
    ToolSpec { name: "suggest",    description: "Get a recommended aden command for an intent.",                                                                 args: &[("intent", "string")] },
    ToolSpec { name: "licenses",   description: "Generate third-party dependency attribution.",                                                                   args: &[("path", "string"), ("full", "boolean")] },
    ToolSpec { name: "review",     description: "Semantic review of pending proposals.",                                                                        args: &[("path", "string"), ("since", "string"), ("budget", "integer")] },
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
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        TOOLS.iter().find(|t| t.name == name).map(|t| {
            let mut props = serde_json::Map::new();
            for &(arg_name, ty) in t.args {
                let mut p = serde_json::Map::new();
                p.insert("type".to_string(), serde_json::json!(ty));
                props.insert(arg_name.to_string(), serde_json::Value::Object(p));
            }
            let mut schema = JsonObject::new();
            schema.insert("type".to_string(), serde_json::json!("object"));
            schema.insert("properties".to_string(), serde_json::Value::Object(props));
            Tool::new(t.name, t.description, Arc::new(schema))
        })
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools: Vec<Tool> = TOOLS
            .iter()
            .map(|t| {
                let mut props = serde_json::Map::new();
                for &(arg_name, ty) in t.args {
                    let mut p = serde_json::Map::new();
                    p.insert("type".to_string(), serde_json::json!(ty));
                    props.insert(arg_name.to_string(), serde_json::Value::Object(p));
                }
                let mut schema = JsonObject::new();
                schema.insert("type".to_string(), serde_json::json!("object"));
                schema.insert("properties".to_string(), serde_json::Value::Object(props));
                Tool::new(t.name, t.description, Arc::new(schema))
            })
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
        let spec = TOOLS
            .iter()
            .find(|t| t.name == tool_name)
            .ok_or_else(|| {
                McpError::invalid_params(format!("unknown tool: {}", tool_name), None)
            })?;

        // Build CLI args: `aden <name> [positional] [--flag|--key value] ...`
        let mut cmd_args: Vec<String> = vec![tool_name.to_string()];

        /// Returns true if the arg should be passed positionally (no -- prefix).
        fn is_positional(tool: &str, arg: &str) -> bool {
            match (tool, arg) {
                // path is positional for every command
                (_, "path") => true,
                // ask:   aden ask <QUESTION> [DIR]
                ("ask", "question") => true,
                // search: aden search <QUERY> [DIR]
                ("search", "query") => true,
                // suggest: aden suggest <INTENT>
                ("suggest", "intent") => true,
                // new:   aden new <NAME> <LANG> [DIR]
                ("new", "name" | "lang") => true,
                // kickoff: aden kickoff <BRIEF> [DIR]
                ("kickoff", "brief") => true,
                _ => false,
            }
        }

        for &(arg_name, arg_type) in spec.args {
            let val = match args.get(arg_name) {
                Some(v) => v,
                None => continue,
            };
            match arg_type {
                "boolean" if val.as_bool().unwrap_or(false) => {
                    cmd_args.push(format!("--{}", arg_name));
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
                        if is_positional(tool_name, arg_name) {
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

// ── CLI bridge ──────────────────────────────────────────────

async fn run_aden_command(project_dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new("aden")
        .args(args)
        .current_dir(project_dir)
        .output()
        .await
        .map_err(|e| format!("failed to run aden: {}", e))?;

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
