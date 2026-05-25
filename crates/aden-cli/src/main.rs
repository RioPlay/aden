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
mod commands;
mod mcp;
mod types;
mod util;

use crate::commands::query::AsmOptions;

use clap::{Parser, Subcommand, ValueHint};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "aden",
    version = "0.1.0",
    about = "Aden — A Dense Referential Context Compiler"
)]
struct Cli {
    #[arg(long, global = true, help = "Remove all limits (show full results)")]
    unlimited: bool,
    #[arg(short, long, global = true, help = "Output JSON where supported")]
    json: bool,
    #[arg(short, long, global = true, help = "Verbose output")]
    verbose: bool,
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
        #[arg(
            value_name = "TEMPLATE",
            help = "Template to instantiate: kickoff, design, spec, task, adr"
        )]
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
        paths: Vec<PathBuf>,
        #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath, help = "Output directory (default: ./contracts/)")]
        out_dir: Option<PathBuf>,
        #[arg(
            long,
            help = "Auto-detect existing contract structure (contracts/crates/<name>/src/)"
        )]
        detect_out_dir: bool,
        #[arg(
            long,
            help = "Auto-discover source files and generate contracts for the whole project (default when PATH is directory)"
        )]
        auto: bool,
        #[arg(
            long,
            help = "Three-way merge: update only [generated] blocks while preserving [human]/[agent] blocks"
        )]
        merge: bool,
        #[arg(long, help = "Dry-run: output MergeActions without writing files")]
        propose: bool,
        #[arg(
            long,
            value_name = "FORMAT",
            default_value = "adoc",
            help = "Output format: adoc (AsciiDoc), md (Markdown)"
        )]
        format: String,
    },
    /// Verify all <<refs>> resolve to existing [[anchors]]
    Check {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
        #[arg(
            long,
            value_name = "SEVERITY",
            default_value = "Warn",
            help = "Minimum severity to fail: Suggest, Warn, Forbid"
        )]
        severity: String,
    },
    /// Lint source files using tree-sitter AST analysis
    Lint {
        #[arg(value_name = "PATH", default_value = ".", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
        #[arg(
            long,
            value_name = "SEVERITY",
            default_value = "Warn",
            help = "Minimum severity to report: Suggest, Warn, Error"
        )]
        severity: String,
        #[arg(long, help = "Fix issues where possible")]
        fix: bool,
        #[arg(long, help = "Output JSON format")]
        json: bool,
    },
    /// Discover and run tests across all languages
    Test {
        #[arg(value_name = "PATH", default_value = ".", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
        #[arg(
            long,
            value_name = "SCOPE",
            help = "Test scope: unit, integration, all"
        )]
        scope: Option<String>,
        #[arg(long, help = "Run only these tests (filter)")]
        filter: Option<String>,
        #[arg(long, help = "Don't run tests, only list them")]
        list: bool,
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
        #[arg(
            long,
            value_name = "FORMAT",
            default_value = "aden",
            help = "Output format: aden (human-readable), adg (compact JSON)"
        )]
        format: String,
        #[arg(long, help = "Silent mode: skip intro, output only context")]
        silent: bool,
        #[arg(long, help = "Auto mode: adjust budget based on relevance scores")]
        auto: bool,
        #[arg(long, help = "Inspect: show what would be included without outputting")]
        inspect: bool,
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
        #[arg(
            long,
            value_name = "FORMAT",
            default_value = "json",
            help = "Output format: json, table"
        )]
        format: String,
    },
    /// Execute an Aden Query (.adq) script
    QueryAdq {
        #[arg(
            value_name = "SCRIPT",
            help = "ADQ script: node(anchor), incoming(anchor), outgoing(anchor), where anchor:term"
        )]
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
        #[arg(
            long,
            value_name = "MODEL",
            help = "LLM model: ollama:<name>, openai:<name>, or auto"
        )]
        model: Option<String>,
    },
    /// Search the knowledge graph for documents matching a query
    Search {
        #[arg(value_name = "QUERY")]
        query: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(
            long,
            value_name = "N",
            default_value = "50",
            help = "Limit number of results"
        )]
        limit: usize,
    },
    /// List all anchors and contracts in the knowledge graph (alias: ls)
    List {
        #[arg(
            long,
            value_name = "PATTERN",
            help = "Filter by pattern (e.g., 'mod-aden-*')"
        )]
        filter: Option<String>,
        #[arg(long, help = "Show detailed information for each anchor")]
        verbose: bool,
        #[arg(
            long,
            value_name = "N",
            default_value = "50",
            help = "Limit number of results"
        )]
        limit: usize,
        #[arg(long, help = "Show all results (no limit)")]
        unlimited: bool,
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
        #[arg(
            long,
            value_name = "N",
            default_value = "50",
            help = "Limit number of results"
        )]
        limit: usize,
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
        #[arg(
            long,
            help = "Auto-fix StaleHash and MissingContract drift (high confidence only)"
        )]
        fix: bool,
        #[arg(
            long,
            help = "Garbage collect orphaned contracts (contracts without matching source)"
        )]
        gc: bool,
        #[arg(
            long,
            value_name = "REF",
            help = "Limit scan to files changed since git ref"
        )]
        since: Option<String>,
        #[arg(long, value_name = "ID", help = "Apply a specific proposal by ID")]
        apply: Option<String>,
        #[cfg(feature = "watch")]
        #[arg(
            long,
            value_name = "DIR",
            help = "Watch directory and auto-heal on changes"
        )]
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
        #[arg(
            long,
            value_name = "REF",
            help = "Review only files changed since git ref"
        )]
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
        #[arg(
            long,
            value_name = "FILE",
            help = "Write output to file instead of stdout"
        )]
        out: Option<PathBuf>,
    },
    /// Multi-repository workspace management
    Federation {
        #[command(subcommand)]
        action: FederationAction,
    },
    /// OWASP-style security audit: scan source for vulnerabilities
    Audit {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(
            long,
            value_name = "LANG",
            help = "Filter to a specific language (rust, python, go, ts, php). Default: auto-detect all"
        )]
        lang: Option<String>,
        #[arg(
            long,
            value_name = "FORMAT",
            default_value = "text",
            help = "Output format: text, json, adoc"
        )]
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
        #[arg(
            long,
            value_name = "REASON",
            help = "Justification for emergency override"
        )]
        reason: String,
        #[arg(
            long,
            value_name = "TTL",
            default_value = "24h",
            help = "Time-to-live: 1h, 24h, 7d"
        )]
        ttl: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// AI assistant: describe what you want to do, get the right aden command
    Suggest {
        #[arg(
            value_name = "INTENT",
            help = "What you want to do (e.g., 'generate docs for my code', 'find how X works')"
        )]
        intent: String,
    },
}

#[derive(Subcommand)]
enum McpAction {
    /// Install aden MCP into supported AI platforms
    Install {
        #[arg(
            long,
            value_name = "PLATFORM",
            help = "Target platform: opencode, claude, cursor, codex, zed, windsurf"
        )]
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
    /// Start HTTP server for CI/agent integration
    Serve {
        #[arg(long, value_name = "PORT", default_value = "3030")]
        port: u16,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum FederationAction {
    /// List repositories in the workspace
    List,
    /// Add a repository to the workspace
    Add {
        #[arg(value_name = "PATH")]
        path: PathBuf,
        #[arg(long, value_name = "NAME", help = "Friendly name for the repository")]
        name: Option<String>,
    },
    /// Remove a repository from the workspace
    Remove {
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Show workspace configuration
    Config,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let _verbose = cli.verbose;
    let _unlimited = cli.unlimited;
    let _global_json = cli.json;

    match cli.command {
        Commands::Init { path } => commands::cmd_init(&path),
        Commands::Gen {
            paths,
            out_dir,
            detect_out_dir,
            auto,
            merge,
            propose,
            format,
        } => {
            let effective_path = if paths.is_empty() {
                std::path::PathBuf::from(".")
            } else {
                paths[0].clone()
            };
            commands::cmd_gen(
                &effective_path,
                out_dir.as_deref(),
                detect_out_dir,
                auto,
                merge,
                propose,
                &format,
            )
        }
        Commands::Check { path, severity } => commands::cmd_check(&path, &severity),
        Commands::Lint {
            path,
            severity,
            fix,
            json,
        } => commands::cmd_lint(&path, &severity, fix, json),
        Commands::Test {
            path,
            scope,
            filter,
            list,
        } => commands::cmd_test(&path, scope.as_deref(), filter.as_deref(), list),
        Commands::Asm {
            from,
            depth,
            budget,
            edge_types,
            out,
            path,
            format: asm_format,
            silent,
            auto,
            inspect,
        } => {
            let types = edge_types
                .map(|s| util::parse_edge_types(&s))
                .unwrap_or_default();
            commands::cmd_asm(AsmOptions {
                path,
                from,
                depth,
                budget,
                edge_types: types,
                out,
                format: asm_format,
                silent,
                auto,
                inspect,
            })
        }
        Commands::Query {
            from,
            edge_type,
            depth,
            backlinks,
            impact,
            format,
            path,
        } => commands::cmd_query(
            &path,
            from.as_deref(),
            edge_type.as_deref(),
            depth,
            backlinks.as_deref(),
            impact.as_deref(),
            &format,
        ),
        Commands::QueryAdq { script, path } => commands::cmd_query_adq(&path, &script),
        Commands::Ask {
            question,
            from,
            budget,
            model,
            path,
        } => commands::cmd_ask(&path, &question, from.as_deref(), budget, model.as_deref()),
        Commands::Search { query, path, limit } => {
            let effective_limit = if cli.unlimited { usize::MAX } else { limit };
            commands::cmd_search(&path, &query, effective_limit)
        }
        Commands::List {
            filter,
            verbose,
            limit,
            unlimited,
            path,
        } => {
            let effective_limit = if unlimited || cli.unlimited {
                usize::MAX
            } else {
                limit
            };
            commands::cmd_list(&path, filter.as_deref(), verbose, effective_limit)
        }
        Commands::Locate {
            symbol,
            caller_of,
            format,
            path,
            limit,
        } => {
            let effective_limit = if cli.unlimited { usize::MAX } else { limit };
            commands::cmd_locate(
                &path,
                symbol.as_deref(),
                caller_of.as_deref(),
                &format,
                effective_limit,
            )
        }
        #[cfg(feature = "watch")]
        Commands::Watch { path } => commands::cmd_watch(&path),
        Commands::Heal {
            path,
            propose,
            fix,
            gc,
            since,
            apply,
            watch,
        } => {
            if let Some(id) = apply {
                commands::cmd_heal_apply(&path, &id)
            } else if let Some(watch_path) = watch {
                #[cfg(feature = "watch")]
                {
                    commands::cmd_heal_watch(&watch_path)
                }
                #[cfg(not(feature = "watch"))]
                {
                    Err("watch feature is not enabled in this build".into())
                }
            } else if let Some(ref git_ref) = since {
                commands::cmd_heal_scan_since(&path, propose, git_ref)
            } else {
                commands::cmd_heal_scan(&path, propose, fix, gc)
            }
        }
        Commands::CiCheck { path } => commands::cmd_ci_check(&path),
        Commands::Doctor { path } => commands::cmd_doctor(&path),
        Commands::Review {
            path,
            budget,
            since,
        } => {
            if let Some(ref git_ref) = since {
                commands::cmd_review_since(&path, budget, git_ref)
            } else {
                commands::cmd_review(&path, budget)
            }
        }
        Commands::Session {
            agent_id,
            task,
            files,
            status,
            path,
        } => commands::cmd_session(
            &path,
            &agent_id,
            &task,
            files.as_deref(),
            status.as_deref().unwrap_or("in_progress"),
        ),
        Commands::Licenses { path, out } => commands::cmd_licenses(&path, out.as_deref()),
        Commands::Federation { action } => commands::cmd_federation(&action),
        Commands::Audit {
            path,
            lang,
            format,
            strict,
        } => commands::cmd_audit(&path, lang.as_deref(), &format, strict),
        Commands::New { name, lang, path } => commands::cmd_new(&name, &lang, &path),
        Commands::Kickoff {
            name,
            interactive,
            path,
        } => commands::cmd_kickoff(&name, interactive, &path),
        Commands::Workflow {
            template,
            from,
            out,
            path,
        } => commands::cmd_workflow(&template, from.as_deref(), out.as_deref(), &path),
        Commands::Mcp { action } => match action {
            McpAction::Install {
                platform,
                binary,
                project,
                all,
                dry_run,
            } => {
                let platforms = platform.map_or_else(Vec::new, |p| vec![p]);
                mcp::run_install(
                    &platforms,
                    binary.as_deref(),
                    project.as_deref(),
                    all,
                    dry_run,
                )
            }
            McpAction::Uninstall {
                platform,
                all,
                dry_run,
            } => {
                let platforms = platform.map_or_else(Vec::new, |p| vec![p]);
                mcp::run_uninstall(&platforms, all, dry_run)
            }
            McpAction::List => mcp::run_list(),
            McpAction::Serve { port, path } => mcp::run_http_server(&path, port),
        },
        Commands::Emergency { reason, ttl, path } => commands::cmd_emergency(&path, &reason, &ttl),
        Commands::Suggest { intent } => commands::cmd_suggest(&intent),
    }
}
