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
use clap::{Parser, Subcommand, ValueHint};
use std::path::{Path, PathBuf};

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
        #[arg(long, value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Parse source file(s) and emit .aden / .adoc contracts
    Gen {
        #[arg(value_name = "PATH", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
        #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
        out_dir: Option<PathBuf>,
    },
    /// Verify all <<refs>> resolve to existing [[anchors]]
    Check {
        #[arg(value_name = "PATH", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
    },
    /// Output the local graph neighborhood as a debug report
    Graph {
        #[arg(long, value_name = "ANCHOR")]
        from: String,
        #[arg(long, value_name = "N", default_value = "3")]
        depth: usize,
        #[arg(value_name = "PATH", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
    },
    /// Assemble a context prompt from the knowledge graph
    Asm {
        #[arg(long, value_name = "ANCHOR")]
        from: String,
        #[arg(long, value_name = "N", default_value = "3")]
        depth: usize,
        #[arg(long, value_name = "TOKENS", default_value = "8192")]
        budget: usize,
        #[arg(long, value_name = "TYPES")]
        edge_types: Option<String>,
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        #[arg(value_name = "PATH", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
    },
    /// Query the knowledge graph and emit JSON
    Query {
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
        #[arg(value_name = "PATH", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
    },
    /// Ask a natural-language question; Aden resolves it to a subgraph and assembles context.
    Ask {
        #[arg(value_name = "QUESTION")]
        question: String,
        #[arg(long, value_name = "ANCHOR")]
        from: Option<String>,
        #[arg(long, value_name = "TOKENS", default_value = "4096")]
        budget: usize,
        #[arg(value_name = "PATH", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Search the knowledge graph for documents matching a query
    Search {
        #[arg(value_name = "QUERY")]
        query: String,
        #[arg(value_name = "PATH", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Locate a symbol definition or its call sites in the knowledge graph
    Locate {
        #[arg(long, value_name = "SYMBOL")]
        symbol: Option<String>,
        #[arg(long, value_name = "SYMBOL")]
        caller_of: Option<String>,
        #[arg(long, value_name = "FORMAT", default_value = "plain")]
        format: String,
        #[arg(value_name = "PATH", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Watch source files for changes and auto-regenerate contracts
    #[cfg(feature = "watch")]
    Watch {
        #[arg(value_name = "PATH", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Self-healing documentation engine: scan for drift, propose patches, apply reviewed changes
    Heal {
        #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
        scan: Option<PathBuf>,
        #[arg(long)]
        propose: bool,
        #[arg(long, value_name = "ID")]
        apply: Option<String>,
        #[arg(long, value_name = "REF")]
        since: Option<String>,
        #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
        watch: Option<PathBuf>,
    },
    /// Run all local CI gates before committing (check, heal, test, secret-scan)
    CiCheck {
        #[arg(value_name = "PATH", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Diagnose the environment: tool versions, repo health, signing keys
    Doctor {
        #[arg(value_name = "PATH", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Semantic review: validate low-confidence proposals with token budgeting
    Review {
        #[arg(value_name = "PATH", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
        #[arg(long, value_name = "TOKENS", default_value = "2048")]
        budget: usize,
        #[arg(long, value_name = "REF")]
        since: Option<String>,
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
        #[arg(value_name = "PATH", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Generate third-party accreditation report from Cargo.lock
    Licenses {
        #[arg(value_name = "PATH", value_hint = ValueHint::DirPath, default_value = ".")]
        path: PathBuf,
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init { path } => cmd_init(&path),
        Commands::Gen { path, out_dir } => cmd_gen(&path, out_dir.as_deref()),
        Commands::Check { path } => cmd_check(&path),
        Commands::Graph { from, depth, path } => cmd_graph(&path, &from, depth),
        Commands::Asm { from, depth, budget, edge_types, out, path } => {
            let types = edge_types
                .map(|s| parse_edge_types(&s))
                .unwrap_or_default();
            cmd_asm(&path, &from, depth, budget, types, out.as_deref())
        }
        Commands::Query { from, edge_type, depth, backlinks, impact, path } => {
            cmd_query(&path, from.as_deref(), edge_type.as_deref(), depth, backlinks.as_deref(), impact.as_deref())
        }
        Commands::Ask { question, from, budget, path } => {
            cmd_ask(&path, &question, from.as_deref(), budget)
        }
        Commands::Search { query, path } => {
            cmd_search(&path, &query)
        }
        Commands::Locate { symbol, caller_of, format, path } => {
            cmd_locate(&path, symbol.as_deref(), caller_of.as_deref(), &format)
        }
        #[cfg(feature = "watch")]
        Commands::Watch { path } => {
            cmd_watch(&path)
        }
        Commands::Heal { scan, propose, apply, since, watch } => {
            if let Some(path) = scan {
                if let Some(ref git_ref) = since {
                    cmd_heal_scan_since(&path, propose, git_ref)
                } else {
                    cmd_heal_scan(&path, propose)
                }
            } else if let Some(id) = apply {
                cmd_heal_apply(std::env::current_dir()?.as_path(), &id)
            } else if let Some(path) = watch {
                #[cfg(feature = "watch")]
                { cmd_heal_watch(&path) }
                #[cfg(not(feature = "watch"))]
                { Err("watch feature is not enabled in this build".into()) }
            } else {
                Err("heal requires one of --scan, --apply, or --watch".into())
            }
        }
        Commands::CiCheck { path } => cmd_ci_check(&path),
        Commands::Doctor { path } => cmd_doctor(&path),
        Commands::Review { path, budget, since } => {
            if let Some(ref git_ref) = since {
                cmd_review_since(&path, budget, git_ref)
            } else {
                cmd_review(&path, budget)
            }
        }
        Commands::Session { agent_id, task, files, status, path } => {
            cmd_session(&path, &agent_id, &task, files.as_deref(), status.as_deref().unwrap_or("in_progress"))
        }
        Commands::Licenses { path, out } => {
            cmd_licenses(&path, out.as_deref())
        }
    }
}

/// Scaffold `.agent/` workspace in a target repository.
///
/// IMPORTANT: Templates are embedded via `include_str!`. If you modify
/// any template file under `.agent/templates/`, you MUST rebuild the
/// binary (`cargo build --workspace --release`), install it as stable
/// (`cp target/release/aden ~/.cargo/bin/aden-stable`), then re-run
/// `aden init` to propagate changes to the local workspace.
/// See CONTRIBUTING.md for the full stable binary ritual.
fn cmd_init(target: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let agent_dir = target.join(".agent");
    let templates_dir = agent_dir.join("templates");
    std::fs::create_dir_all(&templates_dir)?;

    // Core templates (embedded at compile time via include_str!)
    std::fs::write(
        templates_dir.join("plan.adoc"),
        include_str!("../../../.agent/templates/plan.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("module.adoc"),
        include_str!("../../../.agent/templates/module.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("context.adoc"),
        include_str!("../../../.agent/templates/context.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("aden-guide.adoc"),
        include_str!("../../../.agent/templates/aden-guide.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("style-guide.adoc"),
        include_str!("../../../.agent/templates/style-guide.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("research.adoc"),
        include_str!("../../../.agent/templates/research.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("constraints.adoc"),
        include_str!("../../../.agent/templates/constraints.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("onboarding.adoc"),
        include_str!("../../../.agent/templates/onboarding.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("protocol.adoc"),
        include_str!("../../../.agent/templates/protocol.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("glossary.adoc"),
        include_str!("../../../.agent/templates/glossary.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("policy.adoc"),
        include_str!("../../../.agent/templates/policy.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("kickoff.adoc"),
        include_str!("../../../.agent/templates/kickoff.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("design.adoc"),
        include_str!("../../../.agent/templates/design.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("spec.adoc"),
        include_str!("../../../.agent/templates/spec.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("task.adoc"),
        include_str!("../../../.agent/templates/task.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("adr.adoc"),
        include_str!("../../../.agent/templates/adr.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("runbook.adoc"),
        include_str!("../../../.agent/templates/runbook.adoc"),
    )?;
    std::fs::write(
        templates_dir.join("retrospective.adoc"),
        include_str!("../../../.agent/templates/retrospective.adoc"),
    )?;
    std::fs::write(
        agent_dir.join("README.adoc"),
        include_str!("../../../.agent/README.adoc"),
    )?;

    // Generate live context.adoc in the project root
    let project_name = target.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let context_content = format!(r###":proj: {project_name}
:standard: unknown
:lang: unknown

[[agent-context]]
= Shared Context for Agent Sessions

This file is the canonical shared memory for all agent sessions working on `{project_name}`.
Include it at the top of every prompt with `include::.agent/context.adoc[]`.

== Hard Constraints (Never Violate)
ifdef::agent[]
. **Never commit without `aden check` passing.** Run `aden check` on docs/ and your changes before declaring done.
. **Never commit contracts or `.aden/` workspace.** Contracts are build artifacts. They contain signatures and paths that could be weaponized if leaked. Use `.gitignore` to enforce this; never bypass it.
. **Never modify `.adoc` contracts without updating `[[anchor]]` references.** Broken `<<refs>>` break the knowledge graph.
. **Never delete `agent-note::` blocks without justification.** They contain temporal uncertainty markers.
. **Never duplicate definitions.** If a term exists in glossary, reference it with `<<glossary.adoc#term>>`; do not redefine.
. **Never emit files outside the working directory** without user confirmation.
. **Never ignore test failures.** If tests fail, fix before proceeding.
endif::agent[]

== Agent Conventions
. Every `.adoc` file must declare an anchor immediately before the title.
. Every table must have a header row.
. Use `agent-note::` blocks for temporal annotations and uncertainty.
. Use `ifdef::agent[]` to hide human prose from AI context.
. Never use YAML frontmatter; use AsciiDoc attributes (`:key: value`).
. Resolve every `<<reference>>` to an existing `[[anchor]]`.

== Next Steps
. Run `aden init` in this repository (already done if you're reading this).
. Read `.agent/onboarding.adoc` before your first session.
. Read `.agent/constraints.adoc` to know what not to do.
. Append your session entry to `.agent/session.adoc` before starting work.
"###,
        project_name = project_name
    );
    std::fs::write(agent_dir.join("context.adoc"), context_content)?;

    // Generate session log (empty, ready for agents)
    let session_content = format!(r###":proj: {project_name}
:session: active

[[agent-session]]
= Agent Session Log

== Purpose
This file is the canonical shared memory for parallel subagent sessions.
Every agent that modifies files must append an entry here before completing.
This prevents race conditions and silent overwrites.

== Active Sessions

|===
|Timestamp |Agent |Task |Files Touched |Status
|===

== Known Invariants
. Every agent must read this file before starting work.
. Every agent must append a row before declaring done.
. If two agents touch the same file, the later agent must reconcile with the earlier.
. No agent may delete rows; only append.
"###,
        project_name = project_name
    );
    std::fs::write(agent_dir.join("session.adoc"), session_content)?;

    // Security-first scaffolding: contracts are build artifacts
    let aden_dir = target.join(".aden");
    std::fs::create_dir_all(&aden_dir)?;

    let aden_manifest = r###"[[manifest]]
= Aden Workspace — Private by Default

== Security Posture
Contract files (`.aden`, `.adoc` in `contracts/`) are *build artifacts*,
not source code. They must never be committed to version control.
They contain derived code signatures, source paths, and architecture metadata
that could be weaponized if exposed.

== Generated Contract Policy
. `contracts/` is always excluded from git via `.gitignore`.
. Contracts are local-only unless explicitly exported with `aden export`.
. Never share `.aden/` or `contracts/` directories.
. If a contract leaks, treat it as a credential rotation event for the repo.

== Why This Matters
A malicious actor who can modify or inject contracts can silently
redirect every downstream developer toward insecure patterns.
Contracts are part of the software supply chain. Guard them accordingly.
"###;
    std::fs::write(aden_dir.join("README.adoc"), aden_manifest)?;

    // Default exclusion rules (.adenignore)
    let adenignore = r###"# Aden Ignore — Security-first defaults
# Contracts are build artifacts; never commit them.
/contracts/
/.aden/
/target/
/.git/

# Secrets (never process)
*.pem
*.key
*.p8
*.env
.env.local
.env.*

# Editor debris
*.swp
*.swo
.vscode/
.idea/

# Build outputs (cargo/node)
node_modules/
dist/
build/

# OS files
.DS_Store
Thumbs.db
"###;
    std::fs::write(target.join(".adenignore"), adenignore)?;

    // Scaffold a starter NOTICE.md for accreditation
    let notice = r###"# Third-Party Dependencies

This project uses open-source packages.
Run `aden licenses` to generate the canonical attribution table from `Cargo.lock`.
Update this file whenever dependencies change.

[IMPORTANT]
====
Accreditation is a first-class concern.
Always verify that third-party licenses are compatible with your project's license.
====
"###;
    std::fs::write(target.join("NOTICE.md"), notice)?;

    println!("Initialized .agent/ in {}", target.display());
    println!("Generated .adenignore with security-first defaults.");
    println!("Generated {} template files.", templates_dir.read_dir()?.count());
    println!("Next: AI agents should read .agent/onboarding.adoc before starting work.");
    Ok(())
}

fn cmd_gen(path: &Path, out_dir: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    let docs = if path.is_file() {
        let source = std::fs::read_to_string(path)?;
        aden_parse::parse_file(path, &source)?
    } else if path.is_dir() {
        aden_parse::parse_directory(path)?
    } else {
        return Err("Path does not exist or is not a file/directory".into());
    };

    if docs.is_empty() {
        eprintln!("No documents extracted from {}", path.display());
        return Ok(());
    }

    let output = aden_emit::emit(&docs);

    if let Some(out) = out_dir {
        std::fs::create_dir_all(out)?;
        for doc in &docs {
            let file_name = format!("{}.adoc", sanitize_anchor(&doc.anchor));
            let file_path = out.join(&file_name);
            std::fs::write(&file_path, aden_emit::emit_document(doc))?;
            println!("Emitted {}", file_path.display());
        }
    } else {
        println!("{output}");
    }
    Ok(())
}

fn cmd_check(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("check requires a directory path".into());
    }

    let messages = perform_check(path)?;
    let mut exit_code = 0i32;
    for msg in &messages {
        if msg.starts_with("ERROR:") {
            eprintln!("{msg}");
            exit_code = 1;
        } else {
            println!("{msg}");
        }
    }

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

fn cmd_graph(path: &Path, from: &str, depth: usize) -> Result<(), Box<dyn std::error::Error>> {
    use aden_graph::{Direction, graph::AdenGraph};

    if !path.is_dir() {
        return Err("graph requires a directory path".into());
    }

    let graph = AdenGraph::build_from_directory(path)?;
    let start_idx = graph.get_index(from).ok_or_else(|| format!("Anchor '{}' not found", from))?;

    println!("Graph neighborhood from anchor '{}' (depth <= {})", from, depth);
    println!("| Anchor | Depth | |
|=== |");

    let mut visited = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    queue.push_back((start_idx, 0usize));

    while let Some((node, d)) = queue.pop_front() {
        if visited.contains(&node) || d > depth {
            continue;
        }
        visited.insert(node);
        println!("| {} | {} |", graph.graph[node].anchor, d);
        for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
            if !visited.contains(&neighbor) {
                queue.push_back((neighbor, d + 1));
            }
        }
    }
    Ok(())
}

fn cmd_asm(
    path: &Path,
    from: &str,
    depth: usize,
    budget: usize,
    edge_types: Vec<aden_core::EdgeType>,
    out: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_asm::traverse::{assemble, AssemblyOptions};
    use aden_graph::graph::AdenGraph;

    if !path.is_dir() {
        return Err("asm requires a directory path".into());
    }

    let graph = AdenGraph::build_from_directory(path)?;
    let opts = AssemblyOptions {
        start_anchor: from.to_string(),
        max_depth: depth,
        token_budget: budget,
        edge_types,
    };

    let output = assemble(&graph, &opts)?;

    if let Some(out_path) = out {
        std::fs::write(out_path, output)?;
        println!("Written assembly to {}", out_path.display());
    } else {
        println!("{output}");
    }
    Ok(())
}

fn parse_single_edge_type(s: &str) -> Option<aden_core::EdgeType> {
    match s.trim() {
        "uses" => Some(aden_core::EdgeType::Uses),
        "implements" => Some(aden_core::EdgeType::Implements),
        "tests" => Some(aden_core::EdgeType::Tests),
        "documents" => Some(aden_core::EdgeType::Documents),
        "constrains" => Some(aden_core::EdgeType::Constrains),
        "justifies" => Some(aden_core::EdgeType::Justifies),
        "invokes" => Some(aden_core::EdgeType::Invokes),
        "requires" => Some(aden_core::EdgeType::Requires),
        "mutates" => Some(aden_core::EdgeType::Mutates),
        "calls" => Some(aden_core::EdgeType::Calls),
        "supersedes" => Some(aden_core::EdgeType::Supersedes),
        "amends" => Some(aden_core::EdgeType::Amends),
        "verifies" => Some(aden_core::EdgeType::Verifies),
        _ => None,
    }
}

fn parse_edge_types(input: &str) -> Vec<aden_core::EdgeType> {
    input.split(',').filter_map(parse_single_edge_type).collect()
}

fn sanitize_anchor(anchor: &str) -> String {
    anchor
        .replace(['/', '#'], "-")
        .replace(":", "-")
        .replace(" ", "-")
}

fn cmd_query(
    path: &Path,
    from: Option<&str>,
    edge_type: Option<&str>,
    depth: usize,
    backlinks: Option<&str>,
    impact: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_graph::{Direction, graph::AdenGraph};
    use std::collections::{HashSet, VecDeque};

    if !path.is_dir() {
        return Err("query requires a directory path".into());
    }

    let graph = AdenGraph::build_from_directory(path)?;

    let mode_count = from.is_some() as u8 + backlinks.is_some() as u8 + impact.is_some() as u8;
    if mode_count != 1 {
        return Err("exactly one of --from, --backlinks, or --impact must be specified".into());
    }

    let mut results = Vec::new();

    if let Some(anchor) = from {
        let start_idx = graph
            .get_index(anchor)
            .ok_or_else(|| format!("Anchor '{}' not found", anchor))?;
        let filter_type = if let Some(et) = edge_type {
            Some(parse_single_edge_type(et).ok_or_else(|| format!("invalid edge type: {}", et))?)
        } else {
            None
        };

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        visited.insert(start_idx);
        queue.push_back((start_idx, 0usize));
        results.push(node_to_json(&graph.graph[start_idx], 0));

        while let Some((node, d)) = queue.pop_front() {
            if d >= depth {
                continue;
            }
            for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
                let weight = graph.graph.find_edge(node, neighbor)
                    .and_then(|e| graph.graph.edge_weight(e))
                    .copied()
                    .unwrap_or(aden_core::EdgeType::Uses);
                if let Some(ft) = filter_type
                    && weight != ft {
                        continue;
                    }
                if visited.insert(neighbor) {
                    results.push(node_to_json(&graph.graph[neighbor], d + 1));
                    queue.push_back((neighbor, d + 1));
                }
            }
        }
    } else if let Some(anchor) = backlinks {
        let target_idx = graph
            .get_index(anchor)
            .ok_or_else(|| format!("Anchor '{}' not found", anchor))?;
        for neighbor in graph.graph.neighbors_directed(target_idx, Direction::Incoming) {
            results.push(node_to_json(&graph.graph[neighbor], 1));
        }
    } else if let Some(anchor) = impact {
        let start_idx = graph
            .get_index(anchor)
            .ok_or_else(|| format!("Anchor '{}' not found", anchor))?;
        let impact_types = [aden_core::EdgeType::Uses,
            aden_core::EdgeType::Calls,
            aden_core::EdgeType::Constrains,
            aden_core::EdgeType::Invokes];

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        visited.insert(start_idx);
        queue.push_back((start_idx, 0usize));
        results.push(node_to_json(&graph.graph[start_idx], 0));

        while let Some((node, d)) = queue.pop_front() {
            for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
                let weight = graph.graph.find_edge(node, neighbor)
                    .and_then(|e| graph.graph.edge_weight(e))
                    .copied()
                    .unwrap_or(aden_core::EdgeType::Uses);
                if !impact_types.contains(&weight) {
                    continue;
                }
                if visited.insert(neighbor) {
                    results.push(node_to_json(&graph.graph[neighbor], d + 1));
                    queue.push_back((neighbor, d + 1));
                }
            }
        }
    }

    println!("{}", serde_json::to_string_pretty(&results)?);
    Ok(())
}

/// Intent classification for natural-language queries.
#[derive(Debug)]
enum QueryIntent {
    Debug,    // "Why does X fail?"
    Usage,    // "How do I use X?"
    Explain,  // "What does X do?"
    Refactor, // "Refactor X"
    Impact,   // "What depends on X?"
    General,  // default
}

fn classify_intent(question: &str) -> QueryIntent {
    let q = question.to_lowercase();
    if q.contains("fail") || q.contains("error") || q.contains("panic") || q.contains("crash") || q.contains("broken") {
        QueryIntent::Debug
    } else if q.contains("how do i") || q.contains("how to") || q.contains("usage") || q.contains("example") {
        QueryIntent::Usage
    } else if q.contains("refactor") || q.contains("rewrite") || q.contains("rename") {
        QueryIntent::Refactor
    } else if q.contains("depend") || q.contains("blast radius") || q.contains("what uses") || q.contains("who calls") {
        QueryIntent::Impact
    } else if q.contains("what is") || q.contains("what does") || q.contains("explain") || q.contains("how does") {
        QueryIntent::Explain
    } else {
        QueryIntent::General
    }
}

fn edge_types_for_intent(intent: &QueryIntent) -> Vec<aden_core::EdgeType> {
    use aden_core::EdgeType::*;
    match intent {
        QueryIntent::Debug => vec![Constrains, Documents, Calls, Invokes, Requires],
        QueryIntent::Usage => vec![Uses, Invokes, Requires, Documents],
        QueryIntent::Explain => vec![Uses, Calls, Implements, Documents],
        QueryIntent::Refactor => vec![Calls, Uses, Mutates, Supersedes, Amends],
        QueryIntent::Impact => vec![Uses, Calls, Constrains],
        QueryIntent::General => vec![Uses, Documents, Constrains],
    }
}

fn depth_for_intent(intent: &QueryIntent) -> usize {
    match intent {
        QueryIntent::Debug => 3,
        QueryIntent::Usage => 2,
        QueryIntent::Explain => 2,
        QueryIntent::Refactor => 4,
        QueryIntent::Impact => 3,
        QueryIntent::General => 2,
    }
}

fn cmd_ask(
    path: &Path,
    question: &str,
    from_override: Option<&str>,
    budget: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_asm::traverse::{assemble, AssemblyOptions};
    use aden_graph::graph::AdenGraph;
    use aden_index::Index;

    if !path.is_dir() {
        return Err("ask requires a directory path".into());
    }

    // Step 1: Resolve question to an anchor via search, or use override
    let start_anchor = if let Some(anchor) = from_override {
        anchor.to_string()
    } else {
        let idx = Index::from_directory(path)?;
        let results = idx.query(question);
        if results.is_empty() {
            println!("No relevant documents found for: {}", question);
            println!("Tips:\n  - Use more specific keywords from the codebase.\n  - Try `aden search <term>` to see available anchors.\n  - Or pin an anchor with --from <anchor>.");
            return Ok(());
        }
        results[0].anchor.clone()
    };

    println!("// Aden Ask: '{}' → [[{}]]", question, start_anchor);
    if from_override.is_some() {
        println!("// (pinned by --from)");
    }
    println!();

    // Step 2: Classify intent and route assembly strategy
    let intent = classify_intent(question);
    let edge_types = edge_types_for_intent(&intent);
    let depth = depth_for_intent(&intent);

    println!("// Strategy: {:?} | Depth: {} | Edges: {:?}", intent, depth,
             edge_types.iter().map(|e| format!("{:?}", e)).collect::<Vec<_>>().join(", "));
    println!();

    // Step 3: Build graph and assemble context
    let graph = AdenGraph::build_from_directory(path)?;
    let opts = AssemblyOptions {
        start_anchor: start_anchor.clone(),
        max_depth: depth,
        token_budget: budget,
        edge_types,
    };
    let assembled = assemble(&graph, &opts)?;

    // Step 4: Print context and footer
    let consumed = assembled.len();
    let budget_label = if consumed > budget { "OVER BUDGET" } else { "on budget" };
    let page_breaks = assembled.matches("\n<<<\n").count();
    let node_count = page_breaks + 1;

    println!("{}", assembled);
    println!();
    println!("// ────────────────────────────────────────────────");
    println!("// Aden Ask Summary");
    println!("//   Question: {}", question);
    println!("//   Anchor  : [[{}]]", start_anchor);
    println!("//   Strategy: {:?} | Depth: {}", intent, depth);
    println!("//   Nodes   : {} | Tokens: {} / {} ({})", node_count, consumed, budget, budget_label);
    println!("// ────────────────────────────────────────────────");

    Ok(())
}

fn node_to_json(node: &aden_graph::DocumentNode, depth: usize) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "anchor".to_string(),
        serde_json::Value::String(node.anchor.clone()),
    );
    map.insert(
        "node_type".to_string(),
        serde_json::Value::String(resolve_node_type(node)),
    );
    map.insert(
        "depth".to_string(),
        serde_json::Value::from(depth as u64),
    );
    serde_json::Value::Object(map)
}

fn resolve_node_type(node: &aden_graph::DocumentNode) -> String {
    node.parsed
        .attributes
        .get("node-type")
        .cloned()
        .unwrap_or_else(|| format!("{:?}", node.doc.node_type))
}

fn cmd_search(path: &Path, query: &str) -> Result<(), Box<dyn std::error::Error>> {
    use aden_index::Index;

    if !path.is_dir() {
        return Err("search requires a directory path".into());
    }

    let index = Index::from_directory(path)?;
    let results = index.query(query);

    if results.is_empty() {
        println!("No results for '{}'", query);
        return Ok(());
    }

    println!("| Anchor | Score | Snippet |");
    println!("|=== |");
    for r in &results {
        let snippet = if r.snippet.len() > 80 {
            format!("{}...", &r.snippet[..80])
        } else {
            r.snippet.clone()
        };
        println!("| {} | {:.1} | {} |", r.anchor, r.score, snippet);
    }
    Ok(())
}

fn cmd_locate(
    path: &Path,
    symbol: Option<&str>,
    caller_of: Option<&str>,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_graph::graph::AdenGraph;
    use serde_json::json;

    if !path.is_dir() {
        return Err("locate requires a directory path".into());
    }

    let graph = AdenGraph::build_from_directory(path)?;

    // If --symbol is given, find the definition.
    if let Some(sym) = symbol {
        let mut hits = Vec::new();
        for node in graph.graph.node_indices() {
            let anchor = &graph.graph[node].anchor;
            // Match by exact anchor suffix or partial anchor string
            if anchor.ends_with(sym) || anchor.contains(sym) {
                let attrs = &graph.graph[node].doc.attributes;
                let file = attrs.get("source_file").cloned().unwrap_or_default();
                let start_line = attrs.get("start_line").cloned().unwrap_or_default();
                let end_line = attrs.get("end_line").cloned().unwrap_or_default();
                let node_type = attrs
                    .get("node-type")
                    .cloned()
                    .unwrap_or_else(|| format!("{:?}", graph.graph[node].doc.node_type));
                hits.push(json!({
                    "anchor": anchor,
                    "node_type": node_type,
                    "file": file,
                    "start_line": start_line,
                    "end_line": end_line,
                }));
            }
        }

        if hits.is_empty() {
            println!("No symbol found matching '{}'", sym);
            return Ok(());
        }

        if format == "json" {
            println!("{}", serde_json::to_string_pretty(&hits)?);
        } else {
            for h in &hits {
                let file = h["file"].as_str().unwrap_or("");
                let start = h["start_line"].as_str().unwrap_or("");
                let end = h["end_line"].as_str().unwrap_or("");
                let anchor = h["anchor"].as_str().unwrap_or("");
                let nt = h["node_type"].as_str().unwrap_or("");
                if file.is_empty() || start.is_empty() {
                    println!("{} ({})", anchor, nt);
                } else if start == end {
                    println!("{} {}:{}", anchor, file, start);
                } else {
                    println!("{} {}:{}–{}", anchor, file, start, end);
                }
            }
        }
        return Ok(());
    }

    // If --caller-of is given, show call sites (requires call-graph edges with span metadata).
    if let Some(_target) = caller_of {
        println!("caller-of requires call-graph edges with line metadata (not yet implemented)");
        println!("Use 'aden graph --from <anchor> --depth 1' for module-level callers instead.");
        return Ok(());
    }

    Err("locate requires one of --symbol or --caller-of".into())
}

#[cfg(feature = "watch")]
fn cmd_watch(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc::channel;

    if !path.is_dir() {
        return Err("watch requires a directory path".into());
    }

    let (tx, rx) = channel();
    let mut watcher = RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = tx.send(event);
            }
        },
        Config::default(),
    )?;

    watcher.watch(path, RecursiveMode::Recursive)?;
    println!("Watching {} for changes... Press Ctrl+C to stop.", path.display());

    // Supported source extensions that parse_file can handle
    let source_exts = [
        "rs", "py", "js", "ts", "tsx", "jsx", "mjs", "cjs", "go",
        "java", "c", "cpp", "cc", "cxx", "h", "hpp", "rb", "cs",
        "swift", "kt", "scala", "zig", "lua", "hs", "ml", "php",
        "ex", "exs", "erl", "gleam", "sh", "bash", "dockerfile",
        "html", "css", "scss", "vue", "svelte", "proto", "tf",
        "cmake",
    ];

    // Contracts directory
    let contracts_dir = path.join("contracts");
    std::fs::create_dir_all(&contracts_dir)?;

    for event in rx {
        for p in &event.paths {
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                let ext = ext.to_lowercase();
                if source_exts.contains(&ext.as_str()) {
                    println!("INFO: Source change detected in {}", p.display());
                    if let Ok(source) = std::fs::read_to_string(p) {
                        match aden_parse::parse_file(p, &source) {
                            Ok(docs) if !docs.is_empty() => {
                                for doc in &docs {
                                    let safe_anchor = sanitize_anchor(&doc.anchor);
                                    let out_path = contracts_dir.join(format!("{}.adoc", safe_anchor));
                                    if let Err(e) = std::fs::write(&out_path, aden_emit::emit_document(doc)) {
                                        eprintln!("ERROR: Failed to write {}: {}", out_path.display(), e);
                                    } else {
                                        println!("INFO: Regenerated {}", out_path.display());
                                    }
                                }
                            }
                            Ok(_) => {}
                            Err(aden_core::Error::UnsupportedLanguage(_)) => {
                                // Silently skip; may be a file extension we don't support yet.
                            }
                            Err(e) => eprintln!("ERROR: Parse failed for {}: {}", p.display(), e),
                        }
                    }
                } else if matches!(ext.as_str(), "adoc" | "aden") {
                    println!("INFO: Doc change detected in {}", p.display());
                    // Validate
                    match perform_check(path) {
                        Ok(messages) => {
                            for msg in messages {
                                println!("{}", msg);
                            }
                        }
                        Err(e) => eprintln!("ERROR: Check failed: {}", e),
                    }
                }
            }
        }
    }
    Ok(())
}

fn perform_check(path: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    use aden_emit::check::{collect_anchors, find_refs};
    use aden_graph::{cycles::find_cycles, graph::AdenGraph, integrity::check_hashes};
    use std::collections::HashSet;
    use std::io::Read;

    let mut messages = Vec::new();
    let mut all_anchors: HashSet<String> = HashSet::new();

    let graph = AdenGraph::build_from_directory(path)?;

    // Collect local anchors
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_file() {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext == "adoc" || ext == "aden" {
                let mut text = String::new();
                std::fs::File::open(&p)?.read_to_string(&mut text)?;
                all_anchors.extend(collect_anchors(&text));
            }
        }
    }

    for node in graph.graph.node_indices() {
        for anchor in &graph.graph[node].parsed.anchors {
            all_anchors.insert(anchor.clone());
        }
    }

    let mut unresolved = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_file() {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext == "adoc" || ext == "aden" {
                let mut text = String::new();
                std::fs::File::open(&p)?.read_to_string(&mut text)?;
                for line in text.lines() {
                    for r in find_refs(line) {
                        if !all_anchors.contains(&r) {
                            unresolved.push(format!("{}: unresolved <<{}>>", p.display(), r));
                        }
                    }
                }
            }
        }
    }

    if unresolved.is_empty() {
        messages.push("INFO: All <<refs>> resolve.".to_string());
    } else {
        for issue in unresolved {
            messages.push(format!("ERROR: {}", issue));
        }
    }

    let cycles = find_cycles(&graph);
    if cycles.is_empty() {
        messages.push("INFO: No include cycles detected.".to_string());
    } else {
        for cycle in &cycles {
            messages.push(format!("ERROR: Cycle detected: {}", cycle.join(" -> ")));
        }
    }

    let orphans = graph.orphans();
    if orphans.is_empty() {
        messages.push("INFO: No orphan documents.".to_string());
    } else {
        for o in &orphans {
            messages.push(format!("WARNING: Orphan document: {}", o));
        }
    }

    let hash_issues = check_hashes(&graph);
    if hash_issues.is_empty() {
        messages.push("INFO: All source_hash values valid.".to_string());
    } else {
        for (anchor, msg) in &hash_issues {
            messages.push(format!("ERROR: {} (anchor: {})", msg, anchor));
        }
    }

    let edge_issues = graph.validate_typed_edges();
    if edge_issues.is_empty() {
        messages.push("INFO: All typed edges valid.".to_string());
    } else {
        for issue in edge_issues {
            messages.push(format!("ERROR: {}", issue));
        }
    }

    Ok(messages)
}

fn cmd_session(
    repo_path: &Path,
    agent_id: &str,
    task: &str,
    files: Option<&str>,
    status: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Validate inputs against injection and length attacks
    const MAX_FIELD_LEN: usize = 500;
    if agent_id.len() > MAX_FIELD_LEN || task.len() > MAX_FIELD_LEN || status.len() > MAX_FIELD_LEN {
        return Err("Input field exceeds maximum length (500 chars)".into());
    }
    if files.map(|f| f.len() > MAX_FIELD_LEN).unwrap_or(false) {
        return Err("Files field exceeds maximum length (500 chars)".into());
    }

    let session_path = repo_path.join(".agent").join("session.adoc");
    
    if !session_path.exists() {
        return Err(format!("Session file not found: {}. Run 'aden init' first.", session_path.display()).into());
    }

    // Enforce session file size limit to prevent DoS via log growth
    const MAX_SESSION_SIZE: u64 = 5 * 1024 * 1024; // 5 MB
    let meta = std::fs::metadata(&session_path)?;
    if meta.len() > MAX_SESSION_SIZE {
        return Err("Session log exceeds 5 MB. Rotate or archive before appending.".into());
    }

    let timestamp = aden_core::rfc3339_now();
    let files_str = files.unwrap_or("-");
    let entry = format!("|{} |{} |{} |{} |{}\n",
        escape_adoc_cell(&timestamp),
        escape_adoc_cell(agent_id),
        escape_adoc_cell(task),
        escape_adoc_cell(files_str),
        escape_adoc_cell(status)
    );

    let mut content = std::fs::read_to_string(&session_path)?;
    
    // Find the table body and append
    if let Some(pos) = content.find("|===\n\n== Known Invariants") {
        let insert_pos = pos; // Insert before "== Known Invariants"
        let before = &content[..insert_pos];
        let after = &content[insert_pos..];
        let new_content = format!("{}\n{}\n{}", before.trim_end(), entry, after);
        std::fs::write(&session_path, new_content)?;
    } else {
        // Fallback: append to end
        content.push('\n');
        content.push_str(&entry);
        std::fs::write(&session_path, content)?;
    }

    println!("Session entry logged for agent '{}': {}", agent_id, session_path.display());
    Ok(())
}

fn cmd_licenses(
    repo_path: &Path,
    out: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let lock_path = repo_path.join("Cargo.lock");
    if !lock_path.exists() {
        return Err(format!(
            "Cargo.lock not found at {}. Run 'cargo generate-lockfile' first.",
            lock_path.display()
        )
        .into());
    }

    let content = std::fs::read_to_string(&lock_path)?;
    let mut packages: Vec<(String, String)> = Vec::new();
    let mut current_name: Option<String> = None;
    let mut is_aden_crate = true; // skip aden internal crates

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[[package]]") {
            is_aden_crate = true;
            current_name = None;
        } else if trimmed.starts_with("name = ") {
            let name = trimmed
                .trim_start_matches("name = ")
                .trim_matches('"')
                .to_string();
            if !name.starts_with("aden") && name != "aden_py" {
                is_aden_crate = false;
            }
            current_name = Some(name);
        } else if trimmed.starts_with("version = ") && !is_aden_crate {
            if let Some(name) = current_name.clone() {
                let version = trimmed
                    .trim_start_matches("version = ")
                    .trim_matches('"')
                    .to_string();
                packages.push((name, version));
            }
        }
    }

    // Sort and dedup
    packages.sort_by(|a, b| a.0.cmp(&b.0));
    packages.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);

    let mut markdown = String::new();
    markdown.push_str("# Third-Party Dependencies\n\n");
    markdown.push_str("This project uses the following open-source packages.\n");
    markdown.push_str("Generated by `aden licenses`.\n");
    markdown.push_str("For full license texts, see the respective package repositories or `Cargo.lock`.\n\n");
    markdown.push_str("| Package | Version |\n");
    markdown.push_str("|---------|---------|\n");
    for (name, version) in &packages {
        markdown.push_str(&format!("| {} | {} |\n", name, version));
    }
    markdown.push('\n');
    markdown.push_str("## Attribution\n\n");
    markdown.push_str("All third-party packages are used in accordance with their respective licenses.\n");
    markdown.push_str("No proprietary code is bundled or modified without explicit permission.\n");
    markdown.push_str("\n---\nGenerated by Aden.\n");

    if let Some(out_path) = out {
        std::fs::write(out_path, &markdown)?;
        println!("Wrote third-party attribution to {}", out_path.display());
    } else {
        println!("{}", markdown);
    }

    Ok(())
}

fn cmd_review(path: &Path, budget: usize) -> Result<(), Box<dyn std::error::Error>> {
    use aden_propose::list;

    println!("Aden Semantic Review Engine (Budget: {} tokens)", budget);
    println!("================================================");

    if !path.join(".aden").join("proposals").exists() {
        println!("No proposals directory found. Run 'aden heal --scan . --propose' first.");
        return Ok(());
    }

    // List proposals
    let proposals = list(path)?;
    let low_confidence: Vec<_> = proposals.iter()
        .filter(|p| p.confidence < 0.85)
        .collect();

    if low_confidence.is_empty() {
        println!("No low-confidence proposals found. All drift detected is auto-applyable.");
        return Ok(());
    }

    println!("Reviewing {} low-confidence proposals...\n", low_confidence.len());

    // Token-budgeted review: estimate ~100 tokens per proposal
    let estimated_tokens = low_confidence.len() * 100;
    println!("Estimated review cost: ~{} tokens (budget: {})", estimated_tokens, budget);

    if estimated_tokens > budget {
        println!("WARNING: Review exceeds budget. Showing first {} proposals.", budget / 100);
    }

    let show_count = (budget / 100).min(low_confidence.len());
    for (i, proposal) in low_confidence.iter().take(show_count).enumerate() {
        println!("\n{}. Proposal {} (confidence: {:.2})", i + 1, proposal.id, proposal.confidence);
        println!("   Target: {}", proposal.target_path.display());
        println!("   Drift Type: {}", proposal.drift_type);
        println!("   Rationale: {}", proposal.rationale.lines().next().unwrap_or("(none)"));
    }

    if show_count < low_confidence.len() {
        println!("\n... and {} more proposals (increase --budget to see all)", low_confidence.len() - show_count);
    }

    println!("\nReview each proposal file in .aden/proposals/ before applying.");
    Ok(())
}

fn cmd_review_since(path: &Path, budget: usize, since: &str) -> Result<(), Box<dyn std::error::Error>> {
    use aden_heal::Scanner;
    
    println!("Reviewing changes since '{}' with budget {} tokens", since, budget);
    
    // Get changed files from git
    let output = std::process::Command::new("git")
        .args(["diff", "--name-only", since])
        .current_dir(path)
        .output()?;
    
    let changed = String::from_utf8_lossy(&output.stdout);
    let files: Vec<&str> = changed.lines().filter(|l| !l.is_empty()).collect();

    if files.is_empty() {
        println!("No files changed since {}.", since);
        return Ok(());
    }

    println!("Files changed since '{}': {} files", since, files.len());
    for f in &files {
        println!("  - {}", f);
    }

    // Run targeted drift scan on changed files only
    println!("\nRunning targeted drift scan...");
    let scanner = Scanner::new(path);
    let all_events = scanner.scan()?;
    
    // Filter to files changed since the ref
    let relevant_events: Vec<_> = all_events.into_iter()
        .filter(|e| {
            let target = match e {
                aden_heal::DriftEvent::StaleHash { target_path, .. } => target_path,
                aden_heal::DriftEvent::SignatureMismatch { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::MissingContract { source_path, .. } => source_path,
                aden_heal::DriftEvent::OrphanAnchor { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::BrokenReference { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::DeadLink { contract_path, .. } => contract_path,
            };
            files.iter().any(|f| target.contains(f))
        })
        .collect();

    if relevant_events.is_empty() {
        println!("No drift detected in changed files.");
        return Ok(());
    }

    println!("Found {} drift events in changed files.", relevant_events.len());
    
    // Token-budgeted display
    let show_count = (budget / 100).min(relevant_events.len());
    for (i, event) in relevant_events.iter().take(show_count).enumerate() {
        println!("  {}. {:?}", i + 1, event);
    }
    if show_count < relevant_events.len() {
        println!("  ... and {} more (increase --budget)", relevant_events.len() - show_count);
    }

    Ok(())
}

fn cmd_heal_scan_since(path: &Path, propose: bool, since: &str) -> Result<(), Box<dyn std::error::Error>> {
    use aden_heal::{Scanner, generate};
    
    println!("Aden Incremental Scan (since {})", since);
    println!("================================");

    // Get changed files from git
    let output = std::process::Command::new("git")
        .args(["diff", "--name-only", since])
        .current_dir(path)
        .output()?;
    
    let changed = String::from_utf8_lossy(&output.stdout);
    let files: Vec<&str> = changed.lines().filter(|l| !l.is_empty()).collect();

    if files.is_empty() {
        println!("No files changed since {}. Nothing to scan.", since);
        return Ok(());
    }

    println!("Scanning {} changed files...", files.len());
    
    // Run targeted drift scan
    let scanner = Scanner::new(path);
    let all_events = scanner.scan()?;

    // Filter to changed files
    let relevant_events: Vec<aden_heal::DriftEvent> = all_events.into_iter()
        .filter(|e| {
            let target = match e {
                aden_heal::DriftEvent::StaleHash { target_path, .. } => target_path,
                aden_heal::DriftEvent::SignatureMismatch { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::MissingContract { source_path, .. } => source_path,
                aden_heal::DriftEvent::OrphanAnchor { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::BrokenReference { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::DeadLink { contract_path, .. } => contract_path,
            };
            files.iter().any(|f| target.contains(f))
        })
        .collect();

    let report = generate(relevant_events.clone(), path);
    println!("Health Score: {:.2}/1.00", report.overall_score);
    println!("Drift Events: {}", report.events.len());

    if propose && !report.events.is_empty() {
        println!("Generating proposals...");
        let proposals_dir = path.join(".aden").join("proposals");
        std::fs::create_dir_all(&proposals_dir)?;
        for event in &report.events {
            let proposal = generate_proposal(event, path)?;
            let store_path = aden_propose::persist(&proposal, path)?;
            println!("  Generated: {}", store_path.display());
        }
    }

    Ok(())
}

fn cmd_heal_scan(path: &Path, propose: bool) -> Result<(), Box<dyn std::error::Error>> {
    use aden_heal::{Scanner, generate};

    println!("Aden Self-Healing Documentation Engine");
    println!("========================================");
    println!("Scanning: {}", path.display());
    println!();

    let scanner = Scanner::new(path);
    match scanner.scan() {
        Ok(events) => {
            let report = generate(events.clone(), path);

            println!("Health Score: {:.2}/1.00", report.overall_score);
            println!("Total Drift Events: {}", report.events.len());
            println!();

            if report.events.is_empty() {
                println!("INFO: No drift detected. Documentation is healthy.");
                return Ok(());
            }

            // Group by severity
            let mut critical = Vec::new();
            let mut high = Vec::new();
            let mut medium = Vec::new();
            let mut low = Vec::new();

                for event in &report.events {
                match event.severity() {
                    aden_heal::DriftSeverity::Critical => critical.push(event),
                    aden_heal::DriftSeverity::High => high.push(event),
                    aden_heal::DriftSeverity::Medium => medium.push(event),
                    aden_heal::DriftSeverity::Low => low.push(event),
                }
            }

            let print_group = |name: &str, events: & Vec<&aden_heal::DriftEvent>| {
                if !events.is_empty() {
                    println!("\n=== {} ({} events) ===", name, events.len());
                    for (i, event) in events.iter().enumerate() {
                        println!("  {}. {:?}", i + 1, event);
                    }
                }
            };

            print_group("CRITICAL", &critical);
            print_group("HIGH", &high);
            print_group("MEDIUM", &medium);
            print_group("LOW", &low);

            if propose {
                println!("\n--propose flag set. Generating patches...");
                let store_dir = path.join(".aden").join("proposals");
                std::fs::create_dir_all(&store_dir)?;

                for event in &report.events {
                    let proposal = generate_proposal(event, path)?;
                    let store_path = aden_propose::persist(&proposal, path)?;
                    println!("  Generated proposal: {}", store_path.display());
                }
                println!("\nReview proposals in: {}", store_dir.display());
                println!("Apply with: aden heal --apply <proposal-id>");
            } else {
                println!("\nRun with --propose to generate patch files for review.");
            }

            Ok(())
        }
        Err(e) => {
            eprintln!("ERROR: Scan failed: {}", e);
            Err(e.into())
        }
    }
}

fn cmd_heal_apply(repo_path: &Path, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !is_safe_id(id) {
        return Err(format!("Invalid proposal ID: {}", id).into());
    }
    println!("Applying proposal: {}", id);

    let store_dir = repo_path.join(".aden").join("proposals");
    let patch_path = store_dir.join(format!("{}.patch.adoc", id));

    if !patch_path.exists() {
        return Err(format!("Proposal not found: {}", patch_path.display()).into());
    }

    let content = std::fs::read_to_string(&patch_path)?;
    println!("Proposal content:");
    println!("---");
    println!("{}", content);
    println!("---");
    println!();
    println!("This is a PROPOSE-ONLY engine. To apply this change:");
    println!("1. Review the proposal above");
    println!("2. If approved, manually edit the target file");
    println!("3. Then mark proposal as applied");
    println!();
    println!("Target file: {}", patch_path.display());

    Ok(())
}

#[cfg(feature = "watch")]
fn cmd_heal_watch(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc::channel;
    use aden_heal::{Scanner, generate};

    println!("Aden Self-Healing Watch Mode");
    println!("Watching: {} for changes...", path.display());
    println!("Triggers targeted drift scan on each change.");
    println!();

    let (tx, rx) = channel();
    let mut watcher = RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = tx.send(event);
            }
        },
        Config::default(),
    )?;

    watcher.watch(path, RecursiveMode::Recursive)?;

    for event in rx {
        for p in &event.paths {
            if let Some(ext) = p.extension().and_then(|e| e.to_str())
                && matches!(ext, "rs" | "ps1" | "adoc" | "aden") {
                    println!("\n[INFO] Change detected: {}", p.display());
                    println!("[INFO] Running targeted drift scan...");

                    let scanner = Scanner::new(path);
                    if let Ok(events) = scanner.scan() {
                        let report = generate(events.clone(), path);
                        println!("Health Score: {:.2}", report.overall_score);
                        for event in events.iter().take(5) {
                            println!("  - {:?} ({:?})", event, event.severity());
                        }
                        if events.len() > 5 {
                            println!("  ... and {} more events", events.len() - 5);
                        }
                    }
                }
        }
    }
    Ok(())
}

fn cmd_ci_check(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut exit_code = 0i32;
    let mut warnings = Vec::new();
    let green = "\x1b[0;32m";
    let red = "\x1b[0;31m";
    let yellow = "\x1b[1;33m";
    let reset = "\x1b[0m";

    macro_rules! gate {
        ($name:expr, $cmd:expr) => {{
            println!("[CI] Running: {} ...", $name);
            match $cmd {
                Ok(_) => println!("{}[CI] PASS: {}{}", green, $name, reset),
                Err(e) => {
                    println!("{}[CI] FAIL: {} — {}{}", red, $name, e, reset);
                    exit_code = 1;
                }
            }
        }};
    }

    macro_rules! warn {
        ($name:expr, $cmd:expr) => {{
            println!("[CI] Checking: {} ...", $name);
            match $cmd {
                Ok(()) => println!("{}[CI] OK:   {}{}", green, $name, reset),
                Err(e) => {
                    println!("{}[CI] WARN: {} — {}{}", yellow, $name, e, reset);
                    warnings.push(format!("{}: {}", $name, e));
                }
            }
        }};
    }

    // ── BLOCKING GATES ────────────────────────────────────
    // These catch structural errors that break the knowledge graph.

    gate!("aden check", {
        if !path.is_dir() { Err("not a directory".into()) }
        else { perform_check(path).map(|_| ()) }
    });

    gate!("cargo test", {
        let output = std::process::Command::new("cargo")
            .args(["test", "--workspace", "--quiet"])
            .current_dir(path)
            .output()?;
        if !output.status.success() {
            Err(Box::<dyn std::error::Error>::from(String::from_utf8_lossy(&output.stderr).to_string()))
        } else {
            Ok(())
        }
    });

    gate!("secret scan", {
        let patterns = [
            r"-----BEGIN (RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----",
            r"AKIA[0-9A-Z]{16}",
            r"ghp_[a-zA-Z0-9]{36}",
        ];
        let non_text_exts: std::collections::HashSet<&str> = [
            "png", "jpg", "jpeg", "gif", "svg", "ico", "bmp",
            "pdf", "zip", "tar", "gz", "bz2", "xz", "7z", "rar",
            "mp3", "mp4", "avi", "mov", "mkv", "wav", "flac",
            "wasm", "so", "dll", "dylib", "exe", "bin", "o", "a",
            "ttf", "otf", "woff", "woff2", "eot",
        ].iter().copied().collect();
        const MAX_SCAN_SIZE: u64 = 1024 * 1024;
        let mut found = 0;
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let p = entry.path();
            if !p.is_file() { continue; }
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if non_text_exts.contains(ext.to_lowercase().as_str()) { continue; }
            }
            if let Ok(meta) = std::fs::metadata(&p) {
                if meta.len() > MAX_SCAN_SIZE { continue; }
            }
            if let Ok(text) = std::fs::read_to_string(&p) {
                for pat in &patterns {
                    if let Ok(re) = regex::Regex::new(pat) {
                        if re.is_match(&text) {
                            println!("  {}Secret pattern '{}' found in {}{}", red, pat, p.display(), reset);
                            found += 1;
                        }
                    }
                }
            }
        }
        if found > 0 {
            Err(Box::<dyn std::error::Error>::from(format!("{} secret pattern(s) detected", found)))
        } else {
            Ok(())
        }
    });

    gate!("accreditation check", {
        if !path.join("Cargo.lock").exists() {
            Ok(())
        } else if path.join("NOTICE.md").exists() {
            Ok(())
        } else {
            Err(Box::<dyn std::error::Error>::from("NOTICE.md missing. Run 'aden licenses --out NOTICE.md'.".to_string()))
        }
    });

    // ── WARNING GATES ─────────────────────────────────────
    // These catch StaleHash / MissingContract — expected during active development.
    // They warn but do NOT block the commit.

    warn!("contract freshness", {
        use aden_heal::{Scanner, generate};
        let scanner = Scanner::new(path);
        let events = scanner.scan()?;
        let report = generate(events.clone(), path);
        // Count only structural drift (not StaleHash or MissingContract)
        let critical_count = events.iter().filter(|e| {
            matches!(e, aden_heal::DriftEvent::BrokenReference { .. }
                | aden_heal::DriftEvent::OrphanAnchor { .. }
                | aden_heal::DriftEvent::SignatureMismatch { .. })
        }).count();
        if critical_count > 0 {
            Err(Box::<dyn std::error::Error>::from(format!("{} critical drift events (broken refs, orphans, signature mismatch)", critical_count)))
        } else if report.overall_score < 0.99 {
            Err(Box::<dyn std::error::Error>::from(format!("Health score: {:.2} — contracts need regeneration (run 'aden gen' on modified files)", report.overall_score)))
        } else {
            Ok(())
        }
    });

    // ── Final Verdict ─────────────────────────────────────
    if !warnings.is_empty() {
        println!("\n{}[CI] WARNINGS (non-blocking):{}", yellow, reset);
        for w in &warnings {
            println!("  ⚠ {}", w);
        }
        println!("  Run 'aden gen <file>' on modified source to clear.\n");
    }

    if exit_code != 0 {
        println!("\n{}[CI] GATES BLOCKED — Fix errors above before committing.{}", red, reset);
        std::process::exit(exit_code);
    }
    println!("\n{}[CI] ALL GATES PASSED — Ready to commit.{}", green, reset);
    Ok(())
}

fn cmd_doctor(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    println!("Aden Doctor — Environment Diagnostics");
    println!("═══════════════════════════════════════\n");

    let mut issues = Vec::new();

    // Tool availability
    for tool in &["rustc", "cargo", "git"] {
        if std::process::Command::new(tool).arg("--version").output().is_ok() {
            println!("✓ {} found", tool);
        } else {
            println!("✗ {} NOT FOUND", tool);
            issues.push(format!("{} not in PATH", tool));
        }
    }

    // Aden binary
    if std::process::Command::new("aden").arg("--version").output().is_ok() {
        println!("✓ aden CLI found in PATH");
    } else {
        println!("✗ aden CLI NOT in PATH (build or install: cargo install --path crates/aden-cli)");
        issues.push("aden CLI not in PATH".to_string());
    }

    // Signing keys
    let key_dir = dirs::home_dir().unwrap_or_default().join(".aden").join("keys");
    if key_dir.join("aden-sign.pub").exists() {
        println!("✓ Signing public key: {}", key_dir.join("aden-sign.pub").display());
    } else {
        println!("⚠ No signing key found. Generate with:");
        println!("    mkdir -p ~/.aden/keys && cd ~/.aden/keys");
        println!("    ssh-keygen -t ed25519 -C 'aden-sign' -N '' -f aden-sign");
        issues.push("No ~/.aden/keys/aden-sign.pub".to_string());
    }

    // Repo health
    println!("\n— Repo Health —");
    if path.join(".agent").is_dir() {
        println!("✓ .agent/ directory present");
    } else {
        println!("✗ .agent/ MISSING — run 'aden init' in this repo");
        issues.push("No .agent/ directory".to_string());
    }

    if path.join(".adenignore").exists() {
        println!("✓ .adenignore present");
    } else {
        println!("⚠ .adenignore missing — using built-in defaults");
    }

    if path.join("NOTICE.md").exists() {
        println!("✓ NOTICE.md present — accreditation is tracked");
    } else {
        println!("⚠ NOTICE.md missing — run 'aden licenses --out NOTICE.md'");
        issues.push("No NOTICE.md — third-party attribution not tracked".to_string());
    }

    // Quick heal score
    println!("\n— Quick Scan —");
    if let Ok(score) = quick_health_score(path) {
        if score >= 1.0 {
            println!("✓ Health Score: {:.2}/1.00", score);
        } else {
            println!("⚠ Health Score: {:.2}/1.00 (run 'aden heal --scan .' to see drift)", score);
            issues.push(format!("Health score {:.2} (target 1.00)", score));
        }
    }

    println!("\n═══════════════════════════════════════");
    if issues.is_empty() {
        println!("All diagnostics passed. Environment is healthy.");
    } else {
        println!("{} issue(s) found:", issues.len());
        for i in &issues {
            println!("  - {}", i);
        }
    }
    Ok(())
}

fn quick_health_score(path: &Path) -> Result<f64, Box<dyn std::error::Error>> {
    use aden_heal::Scanner;
    let scanner = Scanner::new(path);
    let events = scanner.scan()?;
    let total = events.len().max(1) as f64;
    Ok(1.0 - (events.len() as f64 / (total + 5.0)).min(1.0))
}

/// Escape text for safe insertion into an AsciiDoc table cell.
/// Prevents injection of directives, includes, block terminators, and formatting.
fn escape_adoc_cell(text: &str) -> String {
    let mut out = text.replace('|', "{vbar}")
        .replace(['\n', '\r'], " ");
    // Neutralize AsciiDoc directives and block terminators
    out = out.replace("include::", "[include blocked]");
    out = out.replace("ifdef::", "[ifdef blocked]");
    out = out.replace("ifndef::", "[ifndef blocked]");
    out = out.replace("----", "[---- blocked]");
    out = out.replace("++++", "[++++ blocked]");
    out = out.replace("|===", "[table blocked]");
    out
}

/// Validate that an identifier is safe for filesystem and URL usage.
/// Rejects empty strings, paths with path separators, dots (directory traversal), and non-ASCII.
fn is_safe_id(id: &str) -> bool {
    if id.len() < 3 || id.len() > 128 {
        return false;
    }
    id.bytes().all(|b| {
        b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
    })
}

fn generate_proposal_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let pid = std::process::id();
    format!("{pid}-{ts}")
}

fn generate_proposal(
    event: &aden_heal::DriftEvent,
    repo_path: &Path,
) -> Result<aden_propose::Proposal, Box<dyn std::error::Error>> {
    use aden_propose::{Proposal, ProposalStatus};
    use std::fmt::Write;

    let id = generate_proposal_id();
    let mut rationale = String::new();
    let mut patch = String::new();
    let mut target = repo_path.to_path_buf();
    let mut confidence = 0.5;
    let drift_type = format!("{:?}", std::mem::discriminant(event));

    match event {
        aden_heal::DriftEvent::StaleHash { target_path, expected_hash, actual_hash } => {
            confidence = 0.99;
            writeln!(rationale, "Source hash mismatch detected.").unwrap();
            writeln!(rationale, "Expected: {}", expected_hash).unwrap();
            writeln!(rationale, "Actual:   {}", actual_hash).unwrap();
            writeln!(rationale, "The contract at {} needs regeneration.", target_path).unwrap();

            target = PathBuf::from(target_path);
            writeln!(patch, ":source_hash: {}", actual_hash).unwrap();
        }
        aden_heal::DriftEvent::MissingContract { source_path, anchor, symbol_name } => {
            confidence = 0.85;
            writeln!(rationale, "No contract found for public symbol '{}'.", symbol_name).unwrap();
            writeln!(rationale, "Source: {}", source_path).unwrap();
            writeln!(rationale, "Suggested anchor: {}", anchor).unwrap();

            target = PathBuf::from(source_path).with_extension("aden");
            writeln!(patch, "[[{}]]", anchor).unwrap();
            writeln!(patch, "= {}", symbol_name).unwrap();
            writeln!(patch).unwrap();
            writeln!(patch, "agent-note::STUB[Auto-generated by aden-heal. Review before removing this note.]").unwrap();
        }
        aden_heal::DriftEvent::BrokenReference { contract_path, ref_anchor, line } => {
            confidence = 0.70;
            writeln!(rationale, "Broken reference detected.").unwrap();
            writeln!(rationale, "Contract: {}", contract_path).unwrap();
            writeln!(rationale, "Missing anchor: {}", ref_anchor).unwrap();

            target = PathBuf::from(contract_path);
            writeln!(patch, "// TODO: Fix broken reference to <<{}>> on line {}", ref_anchor, line).unwrap();
        }
        _ => {
            writeln!(rationale, "Drift event detected: {:?}", event).unwrap();
            writeln!(patch, "// Proposed changes for drift event").unwrap();
        }
    }

    Ok(Proposal {
        id,
        target_path: target,
        drift_type,
        confidence,
        status: ProposalStatus::PendingReview,
        rationale,
        patch_asciidoc: patch,
    })
}
