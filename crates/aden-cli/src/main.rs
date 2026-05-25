// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// Original author and maintainer: RioPlay
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
mod mcp;
mod types;
mod util;
mod commands;

use clap::{Parser, Subcommand, ValueHint};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "aden", version = "0.1.0", about = "Aden — A Dense Referential Context Compiler")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scaffold .agent/ templates in target repository
    Init {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Create a new project from a language template with aden scaffolding
    New {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(long, value_name = "LANG", default_value = "rust")]
        lang: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Create a kickoff document for a new initiative (interactive or from a brief)
    Kickoff {
        #[arg(long, value_name = "NAME")]
        name: String,
        #[arg(long, help = "Interactive wizard mode")]
        interactive: bool,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Workflow engine: instantiate templates with substitutions and chain documents
    Workflow {
        #[arg(value_name = "TEMPLATE", help = "Template to instantiate: kickoff, design, spec, task, adr")]
        template: String,
        #[arg(long, value_name = "FILE", help = "Source document to derive from")]
        from: Option<String>,
        #[arg(long, value_name = "FILE", help = "Output path")]
        out: Option<PathBuf>,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Parse source file(s) and emit .aden / .adoc contracts
    Gen {
        #[arg(value_name = "PATH", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
        #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath, help = "Output directory (default: ./contracts/)")]
        out_dir: Option<PathBuf>,
        #[arg(long, help = "Auto-discover source files and generate contracts for the whole project")]
        auto: bool,
        #[arg(long, help = "Three-way merge: update only [generated] blocks while preserving [human]/[agent] blocks")]
        merge: bool,
        #[arg(long, help = "Dry-run: output MergeActions without writing files")]
        propose: bool,
    },
    /// Verify all <<refs>> resolve to existing [[anchors]]
    Check {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
        #[arg(long, value_name = "SEVERITY", default_value = "Warn", help = "Minimum severity to fail: Suggest, Warn, Forbid")]
        severity: String,
    },
    /// Output the local graph neighborhood as a debug report
    Graph {
        #[arg(value_name = "ANCHOR")]
        from: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(long, value_name = "N", default_value = "3")]
        depth: usize,
    },
    /// Assemble a context prompt from the knowledge graph
    Asm {
        #[arg(long, value_name = "ANCHOR")]
        from: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(long, value_name = "N", default_value = "3")]
        depth: usize,
        #[arg(long, value_name = "TOKENS", default_value = "8192")]
        budget: usize,
        #[arg(long, value_name = "TYPES")]
        edge_types: Option<String>,
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        #[arg(long, value_name = "FORMAT", default_value = "aden", help = "Output format: aden (human-readable), adg (compact JSON)")]
        format: String,
    },
    /// Query the knowledge graph and emit JSON
    Query {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
        #[arg(long, value_name = "ANCHOR")]
        from: Option<String>,
        #[arg(long, value_name = "TYPE")]
        edge_type: Option<String>,
        #[arg(long, value_name = "N", default_value = "3")]
        depth: usize,
        #[arg(long, value_name = "ANCHOR")]
        backlinks: Option<String>,
        #[arg(long, value_name = "ANCHOR")]
        impact: Option<String>,
    },
    /// Execute an Aden Query (.adq) script
    QueryAdq {
        #[arg(value_name = "SCRIPT", help = "ADQ script: node(anchor), incoming(anchor), outgoing(anchor), where anchor:term")]
        script: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Ask a natural-language question; Aden resolves it to a subgraph and assembles context.
    Ask {
        #[arg(value_name = "QUESTION")]
        question: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(long, value_name = "ANCHOR")]
        from: Option<String>,
        #[arg(long, value_name = "TOKENS", default_value = "4096")]
        budget: usize,
        #[arg(long, value_name = "MODEL", help = "LLM model: ollama:<name>, openai:<name>, or auto")]
        model: Option<String>,
    },
    /// Search the knowledge graph for documents matching a query
    Search {
        #[arg(value_name = "QUERY")]
        query: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Locate a symbol definition or its call sites in the knowledge graph
    Locate {
        #[arg(long, value_name = "SYMBOL", help = "Find definition of this symbol")]
        symbol: Option<String>,
        #[arg(long, value_name = "SYMBOL", help = "Find call sites of this symbol")]
        caller_of: Option<String>,
        #[arg(long, value_name = "FORMAT", default_value = "plain")]
        format: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Watch source files for changes and auto-regenerate contracts
    #[cfg(feature = "watch")]
    Watch {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Self-healing documentation engine: scan for drift, propose patches, apply reviewed changes
    Heal {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(long, help = "Generate patch proposals for review")]
        propose: bool,
        #[arg(long, value_name = "REF", help = "Limit scan to files changed since git ref")]
        since: Option<String>,
        #[arg(long, value_name = "ID", help = "Apply a specific proposal by ID")]
        apply: Option<String>,
        #[cfg(feature = "watch")]
        #[arg(long, value_name = "DIR", help = "Watch directory and auto-heal on changes")]
        watch: Option<PathBuf>,
    },
    /// Run all local CI gates before committing (check, heal, test, secret-scan)
    CiCheck {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Diagnose the environment: tool versions, repo health, signing keys
    Doctor {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Semantic review: validate low-confidence proposals with token budgeting
    Review {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
        #[arg(long, value_name = "REF", help = "Review only files changed since git ref")]
        since: Option<String>,
        #[arg(long, value_name = "TOKENS", default_value = "2048")]
        budget: usize,
    },
    /// Atomic session lock: append entry to .agent/session.adoc
    Session {
        #[arg(long, value_name = "ID")]
        agent_id: String,
        #[arg(long, value_name = "DESC")]
        task: String,
        #[arg(long, value_name = "FILES")]
        files: Option<String>,
        #[arg(long, value_name = "STATUS")]
        status: Option<String>,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Generate third-party accreditation report from Cargo.lock
    Licenses {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(long, value_name = "FILE", help = "Write output to file instead of stdout")]
        out: Option<PathBuf>,
    },
    /// OWASP-style security audit: scan source for vulnerabilities
    Audit {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(long, value_name = "LANG", help = "Filter to a specific language (rust, python, go, ts, php). Default: auto-detect all")]
        lang: Option<String>,
        #[arg(long, value_name = "FORMAT", default_value = "text", help = "Output format: text, json, adoc")]
        format: String,
        #[arg(long, help = "Exit non-zero on any finding (default: warnings only)")]
        strict: bool,
    },
    /// MCP (Model Context Protocol) integration management
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Emergency override: downgrade global Forbid to Warn, auto-expires
    Emergency {
        #[arg(long, value_name = "REASON", help = "Justification for emergency override")]
        reason: String,
        #[arg(long, value_name = "TTL", default_value = "24h", help = "Time-to-live: 1h, 24h, 7d")]
        ttl: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum McpAction {
    /// Install aden MCP into supported AI platforms
    Install {
        #[arg(long, value_name = "PLATFORM", help = "Target platform: opencode, claude, cursor, codex, zed, windsurf")]
        platform: Option<String>,
        #[arg(long, value_name = "PATH", help = "Path to aden-mcp binary")]
        binary: Option<PathBuf>,
        #[arg(long, value_name = "PATH", help = "Project directory to serve")]
        project: Option<PathBuf>,
        #[arg(long, help = "Install for all platforms, not just detected ones")]
        all: bool,
        #[arg(long, help = "Show what would be done without writing files")]
        dry_run: bool,
    },
    /// Remove aden MCP from supported AI platforms
    Uninstall {
        #[arg(long, value_name = "PLATFORM", help = "Target platform")]
        platform: Option<String>,
        #[arg(long, help = "Remove from all supported platforms")]
        all: bool,
        #[arg(long, help = "Show what would be done without writing files")]
        dry_run: bool,
    },
    /// List supported platforms and their status
    List,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init { path } => commands::cmd_init(&path),
        Commands::Gen { path, out_dir, auto, merge, propose } => {
            commands::cmd_gen(&path, out_dir.as_deref(), auto, merge, propose)
        }
        Commands::Check { path, severity } => commands::cmd_check(&path, &severity),
        Commands::Graph { from, depth, path } => commands::cmd_graph(&path, &from, depth),
        Commands::Asm { from, depth, budget, edge_types, out, path, format: asm_format } => {
            let types = edge_types
                .map(|s| util::parse_edge_types(&s))
                .unwrap_or_default();
            commands::cmd_asm(&path, &from, depth, budget, types, out.as_deref(), &asm_format)
        }
        Commands::Query { from, edge_type, depth, backlinks, impact, path } => {
            commands::cmd_query(&path, from.as_deref(), edge_type.as_deref(), depth, backlinks.as_deref(), impact.as_deref())
        }
        Commands::QueryAdq { script, path } => {
            commands::cmd_query_adq(&path, &script)
        }
        Commands::Ask { question, from, budget, model, path } => {
            commands::cmd_ask(&path, &question, from.as_deref(), budget, model.as_deref())
        }
        Commands::Search { query, path } => commands::cmd_search(&path, &query),
        Commands::Locate { symbol, caller_of, format, path } => {
            commands::cmd_locate(&path, symbol.as_deref(), caller_of.as_deref(), &format)
        }
        #[cfg(feature = "watch")]
        Commands::Watch { path } => commands::cmd_watch(&path),
        Commands::Heal { path, propose, since, apply, watch } => {
            if let Some(id) = apply {
                commands::cmd_heal_apply(&path, &id)
            } else if let Some(watch_path) = watch {
                #[cfg(feature = "watch")]
                { commands::cmd_heal_watch(&watch_path) }
                #[cfg(not(feature = "watch"))]
                { Err("watch feature is not enabled in this build".into()) }
            } else if let Some(ref git_ref) = since {
                commands::cmd_heal_scan_since(&path, propose, git_ref)
            } else {
                commands::cmd_heal_scan(&path, propose)
            }
        }
        Commands::CiCheck { path } => commands::cmd_ci_check(&path),
        Commands::Doctor { path } => commands::cmd_doctor(&path),
        Commands::Review { path, budget, since } => {
            if let Some(ref git_ref) = since {
                commands::cmd_review_since(&path, budget, git_ref)
            } else {
                commands::cmd_review(&path, budget)
            }
        }
        Commands::Session { agent_id, task, files, status, path } => {
            commands::cmd_session(&path, &agent_id, &task, files.as_deref(), status.as_deref().unwrap_or("in_progress"))
        }
        Commands::Licenses { path, out } => commands::cmd_licenses(&path, out.as_deref()),
        Commands::Audit { path, lang, format, strict } => {
            commands::cmd_audit(&path, lang.as_deref(), &format, strict)
        }
        Commands::New { name, lang, path } => commands::cmd_new(&name, &lang, &path),
        Commands::Kickoff { name, interactive, path } => commands::cmd_kickoff(&name, interactive, &path),
        Commands::Workflow { template, from, out, path } => {
            commands::cmd_workflow(&template, from.as_deref(), out.as_deref(), &path)
        }
        Commands::Mcp { action } => match action {
            McpAction::Install { platform, binary, project, all, dry_run } => {
                let platforms = platform.map_or_else(Vec::new, |p| vec![p]);
                mcp::run_install(&platforms, binary.as_deref(), project.as_deref(), all, dry_run)
            }
            McpAction::Uninstall { platform, all, dry_run } => {
                let platforms = platform.map_or_else(Vec::new, |p| vec![p]);
                mcp::run_uninstall(&platforms, all, dry_run)
            }
            McpAction::List => mcp::run_list(),
        },
        Commands::Emergency { reason, ttl, path } => {
            commands::cmd_emergency(&path, &reason, &ttl)
        }
    }
}
