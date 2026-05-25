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
        /// Target directory to initialize (default: current directory)
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Create a new project from a language template with aden scaffolding
    New {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(long, value_name = "LANG", default_value = "rust")]
        lang: String,
        /// Parent directory for the new project (default: current directory)
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Create a kickoff document for a new initiative (interactive or from a brief)
    Kickoff {
        #[arg(long, value_name = "NAME")]
        name: String,
        #[arg(long, help = "Interactive wizard mode")]
        interactive: bool,
        /// Project directory (default: current directory)
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
        /// Project directory (default: current directory)
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
    },
    /// Output the local graph neighborhood as a debug report
    Graph {
        #[arg(value_name = "ANCHOR")]
        from: String,
        /// Project directory (default: current directory)
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(long, value_name = "N", default_value = "3")]
        depth: usize,
    },
    /// Assemble a context prompt from the knowledge graph
    Asm {
        #[arg(long, value_name = "ANCHOR")]
        from: String,
        /// Project directory (default: current directory)
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
    },
    /// Query the knowledge graph and emit JSON
    Query {
        /// Project directory (default: current directory)
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
    /// Ask a natural-language question; Aden resolves it to a subgraph and assembles context.
    Ask {
        #[arg(value_name = "QUESTION")]
        question: String,
        /// Project directory (default: current directory)
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
        /// Project directory (default: current directory)
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
        /// Project directory (default: current directory)
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
        /// Directory to scan (default: current directory)
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
        /// Project directory (default: current directory)
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Generate third-party accreditation report from Cargo.lock
    Licenses {
        /// Project directory (default: current directory)
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(long, value_name = "FILE", help = "Write output to file instead of stdout")]
        out: Option<PathBuf>,
    },
    /// OWASP-style security audit: scan source for vulnerabilities
    Audit {
        /// Directory to scan (default: current directory)
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
        Commands::Init { path } => cmd_init(&path),
        Commands::Gen { path, out_dir, auto, merge, propose } => cmd_gen(&path, out_dir.as_deref(), auto, merge, propose),
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
        Commands::Ask { question, from, budget, model, path } => {
            cmd_ask(&path, &question, from.as_deref(), budget, model.as_deref())
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
        Commands::Heal { path, propose, since, apply, watch } => {
            if let Some(id) = apply {
                cmd_heal_apply(&path, &id)
            } else if let Some(watch_path) = watch {
                #[cfg(feature = "watch")]
                { cmd_heal_watch(&watch_path) }
                #[cfg(not(feature = "watch"))]
                { Err("watch feature is not enabled in this build".into()) }
            } else if let Some(ref git_ref) = since {
                cmd_heal_scan_since(&path, propose, git_ref)
            } else {
                cmd_heal_scan(&path, propose)
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
        Commands::Audit { path, lang, format, strict } => cmd_audit(&path, lang.as_deref(), &format, strict),
        Commands::New { name, lang, path } => cmd_new(&name, &lang, &path),
        Commands::Kickoff { name, interactive, path } => cmd_kickoff(&name, interactive, &path),
        Commands::Workflow { template, from, out, path } => cmd_workflow(&template, from.as_deref(), out.as_deref(), &path),
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
    }
}

// ── Project Acceleration Commands ───────────────────────────────

/// Reject project names that could traverse directories.
fn validate_name(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if name.contains('/') || name.contains('\\') || name == ".." || name.starts_with("../") {
        return Err(format!(
            "Invalid project name '{}': must not contain path separators or parent references",
            name
        )
        .into());
    }
    Ok(())
}

/// Create a new project from a language template.
/// Scaffolds build system, aden workspace, and initial documents.
fn cmd_new(name: &str, lang: &str, parent: &Path) -> Result<(), Box<dyn std::error::Error>> {
    validate_name(name)?;
    let project_dir = parent.join(name);
    if project_dir.exists() {
        return Err(format!("Directory {} already exists", project_dir.display()).into());
    }
    std::fs::create_dir_all(&project_dir)?;

    let lang_lower = lang.to_lowercase();
    match lang_lower.as_str() {
        "rust" | "rs" => scaffold_rust(&project_dir, name)?,
        "go" | "golang" => scaffold_go(&project_dir, name)?,
        "ts" | "typescript" | "js" | "javascript" => scaffold_js(&project_dir, name)?,
        _ => return Err(format!("Language '{}' not yet supported. Try: rust, go, typescript", lang).into()),
    }

    // Run aden init in the new project
    cmd_init(&project_dir)?;

    // Create initial docs directory
    let docs_dir = project_dir.join("docs");
    std::fs::create_dir_all(&docs_dir)?;

    // Create README with project identity
    let readme = format!(r###"= {name}
:proj: {name}
:lang: {lang}

[[readme]]
= {name}

Project scaffolded by `aden new {name} --lang={lang}`.

== Quick Start

[source,bash]
----
# Build
<your-build-command>

# Check aden graph integrity
aden check .

# Generate contracts after code changes
aden gen src/ --auto

# Run CI gates
aden ci-check .
----

== Documentation

* xref:kickoff.adoc[Project Kickoff]
* xref:design.adoc[Design Document]
* xref:spec.adoc[Specification]
* xref:adr-001.adoc[Architecture Decisions]

== Navigation

. <<kickoff-{name}>>
. <<design-{name}>>
"###,
        name = name,
        lang = lang_lower
    );
    std::fs::write(docs_dir.join("README.adoc"), readme)?;

    println!("✓ Created project {} in {}", name, project_dir.display());
    println!("✓ Language: {}", lang_lower);
    println!("✓ Scaffolding: aden init, docs/, build system");
    println!("  Next steps:");
    println!("    cd {}", project_dir.display());
    println!("    aden kickoff --interactive --name {}", name);
    println!("    aden workflow design --from docs/kickoff.adoc");
    println!("    aden workflow spec --from docs/design.adoc");
    Ok(())
}

fn scaffold_rust(dir: &Path, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let cargo_toml = format!(r####"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"
authors = [""]
license = "AGPL-3.0-or-later"
description = "{name} — Add your project description here"
repository = "https://github.com/<user>/{name}"

[dependencies]
thiserror = "2"
serde = {{ version = "2", features = ["derive"] }}

[[bin]]
name = "{name}"
path = "src/main.rs"
"####, name = name);
    std::fs::write(dir.join("Cargo.toml"), cargo_toml)?;

    let src_dir = dir.join("src");
    std::fs::create_dir_all(&src_dir)?;
    std::fs::write(
        src_dir.join("main.rs"),
        "fn main() {\n    println!(\"Hello from {}!\");\n}\n".replace("{}", name),
    )?;

    std::fs::write(dir.join(".gitignore"), "/target/\nCargo.lock\n")?;
    Ok(())
}

fn scaffold_go(dir: &Path, _name: &str) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::write(dir.join("go.mod"), "module example.com/project\n\ngo 1.24\n")?;
    let main_go = r##"package main

import "fmt"

func main() {
    fmt.Println("Hello, World!")
}
"##;
    std::fs::create_dir_all(dir.join("cmd").join("server"))?;
    std::fs::write(dir.join("cmd").join("server").join("main.go"), main_go)?;
    std::fs::write(dir.join(".gitignore"), "*.exe\n*.dll\n*.so\ndist/\n")?;
    Ok(())
}

fn scaffold_js(dir: &Path, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let pkg = format!(r###"{{
  "name": "{name}",
  "version": "0.1.0",
  "description": "",
  "main": "src/index.ts",
  "scripts": {{
    "build": "tsc",
    "dev": "tsx watch src/index.ts"
  }},
  "devDependencies": {{
    "typescript": "^5.8"
  }}
}}"###, name = name);
    std::fs::write(dir.join("package.json"), pkg)?;
    std::fs::create_dir_all(dir.join("src"))?;
    std::fs::write(dir.join("src").join("index.ts"), "console.log('Hello, World!');\n")?;
    std::fs::write(dir.join("tsconfig.json"), "\n")?;
    std::fs::write(dir.join(".gitignore"), "node_modules/\ndist/\n*.log\n")?;
    Ok(())
}

/// Interactive kickoff wizard. Fills the kickoff template via Q&A.
fn cmd_kickoff(
    name: &str,
    interactive: bool,
    repo: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_name(name)?;
    use std::io::{self, Write};

    let kickoff_template = include_str!("../../../.agent/templates/kickoff.adoc");
    let out_path = repo.join("docs").join(format!("kickoff-{}.adoc", name));
    std::fs::create_dir_all(out_path.parent().unwrap_or(Path::new(".")))?;

    if interactive {
        println!("=== Aden Kickoff Wizard ===");
        println!("Answer a few questions to scaffold the kickoff document.\n");

        let q = |prompt: &str| -> Result<String, Box<dyn std::error::Error>> {
            print!("{}", prompt);
            io::stdout().flush()?;
            let mut buf = String::new();
            io::stdin().read_line(&mut buf)?;
            Ok(buf.trim().to_string())
        };

        let problem = q("What problem does this solve? ")?;
        let who = q("Who has this problem? ")?;
        let _success = q("What does success look like? ")?;
        let _non_goal = q("What is explicitly NOT in scope? ")?;
        let _deadline = q("Deadline (or 'TBD')? ")?;
        let owner = q("Primary owner? ")?;

        let resolved = kickoff_template
            .replace("{project}", name)
            .replace("{date}", aden_core::rfc3339_now().split('T').next().unwrap_or("2026-01-01"))
            .replace("{author}", &owner)
            .replace("{idea}", &name.to_lowercase().replace(" ", "-"));

        // Replace template placeholders with guided content
        let mut output = resolved;
        // Replace first blank line in Problem section
        output = output.replace(
            ". Who has this problem?\n. ",
            &format!(". Who has this problem?\n  *Answer:* {}\n. ", who),
        );
        output = output.replace(
            ". What do they do today without your solution?\n",
            &format!(". What do they do today without your solution?\n  *Answer:* {}\n", problem),
        );

        std::fs::write(&out_path, output)?;
        println!("\n✓ Generated {}", out_path.display());
        println!("  Review and edit before proceeding to `aden workflow design`.");
    } else {
        // Non-interactive: just fill placeholders from template
        let resolved = kickoff_template
            .replace("{project}", name)
            .replace("{date}", aden_core::rfc3339_now().split('T').next().unwrap_or("2026-01-01"))
            .replace("{author}", "<author>")
            .replace("{idea}", &name.to_lowercase().replace(" ", "-"));
        std::fs::write(&out_path, resolved)?;
        println!("Generated kickoff template: {}", out_path.display());
        println!("  Fill in the blank sections, then run:");
        println!("    aden workflow design --from {}", out_path.display());
    }

    Ok(())
}

/// Reject paths containing parent-directory references.
fn safe_relative(path_str: &str) -> Result<(), Box<dyn std::error::Error>> {
    if path_str.contains("..") {
        return Err(format!(
            "Path traversal blocked: '{}' contains '..'",
            path_str
        )
        .into());
    }
    Ok(())
}

/// Workflow engine: instantiate a template from a source document.
fn cmd_workflow(
    template: &str,
    from: Option<&str>,
    out: Option<&Path>,
    repo: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let templates: std::collections::HashMap<&str, &str> = [
        ("design", include_str!("../../../.agent/templates/design.adoc")),
        ("spec", include_str!("../../../.agent/templates/spec.adoc")),
        ("task", include_str!("../../../.agent/templates/task.adoc")),
        ("adr", include_str!("../../../.agent/templates/adr.adoc")),
        ("kickoff", include_str!("../../../.agent/templates/kickoff.adoc")),
        ("plan", include_str!("../../../.agent/templates/plan.adoc")),
        ("context", include_str!("../../../.agent/templates/context.adoc")),
        ("module", include_str!("../../../.agent/templates/module.adoc")),
        ("runbook", include_str!("../../../.agent/templates/runbook.adoc")),
        ("glossary", include_str!("../../../.agent/templates/glossary.adoc")),
        ("constraints", include_str!("../../../.agent/templates/constraints.adoc")),
    ]
    .into_iter()
    .collect();

    let tmpl = templates.get(template).ok_or_else(|| {
        format!(
            "Unknown template '{}'. Supported: {}",
            template,
            templates.keys().copied().collect::<Vec<_>>().join(", ")
        )
    })?;

    // Resolve placeholders from source doc if --from is given
    let mut resolved = tmpl.to_string();
    if let Some(src_path_str) = from {
        safe_relative(src_path_str)?;
        let src_path = repo.join(src_path_str);
        if src_path.exists() {
            let src_text = std::fs::read_to_string(&src_path)?;
            // Extract key-value pairs from AsciiDoc attributes
            for line in src_text.lines() {
                if line.starts_with(':') && line.contains(": ")
                    && let Some((key, value)) = line.trim().split_once(": ") {
                        let key = key.trim_start_matches(':');
                        let placeholder = format!("{{{key}}}");
                        resolved = resolved.replace(&placeholder, value.trim());
                    }
            }
            // Extract anchor as {feature}/{idea} if present
            if let Some(anchor) = src_text.lines().find(|l| l.starts_with("[[")) {
                let inner = anchor.trim_start_matches("[[").trim_end_matches("]]");
                let clean = inner.replace(['{', '}'], "");
                resolved = resolved.replace("{feature}", &clean);
                resolved = resolved.replace("{idea}", &clean);
            }
        }
    }

    // Default values for any remaining placeholders
    let now = aden_core::rfc3339_now().split('T').next().unwrap_or("2026-01-01").to_string();
    resolved = resolved.replace("{date}", &now);
    resolved = resolved.replace("{author}", "<author>");
    resolved = resolved.replace("{project_name}", repo.file_name().and_then(|n| n.to_str()).unwrap_or("unknown"));
    resolved = resolved.replace("{project}", repo.file_name().and_then(|n| n.to_str()).unwrap_or("unknown"));
    resolved = resolved.replace("{feature}", "feature-name");
    resolved = resolved.replace("{idea}", "idea-name");
    resolved = resolved.replace("{number}", "001");
    resolved = resolved.replace("{phase}", "0");
    resolved = resolved.replace("{standard}", "unknown");
    resolved = resolved.replace("{lang}", "unknown");
    resolved = resolved.replace("{ai_name}", "agent");
    resolved = resolved.replace("{primary_lang}", "unknown");
    resolved = resolved.replace("{framework}", "unknown");
    resolved = resolved.replace("{edition}", "2024");
    resolved = resolved.replace("{glossary}", "(fill me in)");
    resolved = resolved.replace("{dependencies}", "(fill me in)");

    // Auto-next step suggestion
    let next_hint = match template {
        "kickoff" => Some("aden workflow design --from docs/kickoff-<name>.adoc"),
        "design" => Some("aden workflow adr --from docs/design-<name>.adoc"),
        "adr" => Some("aden workflow spec --from docs/design-<name>.adoc"),
        "spec" => Some("aden workflow task --from docs/spec-<name>.adoc"),
        "task" => Some("start implementing, then run: aden gen src/"),
        _ => None,
    };

    if let Some(out_path) = out {
        safe_relative(&out_path.to_string_lossy())?;
    }
    let dest = if let Some(out_path) = out {
        out_path.to_path_buf()
    } else {
        let safe = template.to_lowercase().replace(" ", "-");
        repo.join("docs").join(format!("{}-unnamed.adoc", safe))
    };

    std::fs::create_dir_all(dest.parent().unwrap_or(Path::new(".")))?;
    std::fs::write(&dest, resolved)?;
    println!("✓ Generated workflow document: {}", dest.display());
    if let Some(hint) = next_hint {
        println!("  Next step: {}", hint);
    }

    Ok(())
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

    // Reference templates (user-editable starting points — do NOT put live
    // files like context.adoc / session.adoc here; they are generated below).
    for (name, content) in [
        ("plan.adoc", include_str!("../../../.agent/templates/plan.adoc")),
        ("module.adoc", include_str!("../../../.agent/templates/module.adoc")),
        ("aden-guide.adoc", include_str!("../../../.agent/templates/aden-guide.adoc")),
        ("style-guide.adoc", include_str!("../../../.agent/templates/style-guide.adoc")),
        ("research.adoc", include_str!("../../../.agent/templates/research.adoc")),
        ("constraints.adoc", include_str!("../../../.agent/templates/constraints.adoc")),
        ("onboarding.adoc", include_str!("../../../.agent/templates/onboarding.adoc")),
        ("protocol.adoc", include_str!("../../../.agent/templates/protocol.adoc")),
        ("glossary.adoc", include_str!("../../../.agent/templates/glossary.adoc")),
        ("policy.adoc", include_str!("../../../.agent/templates/policy.adoc")),
        ("kickoff.adoc", include_str!("../../../.agent/templates/kickoff.adoc")),
        ("design.adoc", include_str!("../../../.agent/templates/design.adoc")),
        ("spec.adoc", include_str!("../../../.agent/templates/spec.adoc")),
        ("task.adoc", include_str!("../../../.agent/templates/task.adoc")),
        ("adr.adoc", include_str!("../../../.agent/templates/adr.adoc")),
        ("runbook.adoc", include_str!("../../../.agent/templates/runbook.adoc")),
        ("retrospective.adoc", include_str!("../../../.agent/templates/retrospective.adoc")),
    ] {
        std::fs::write(templates_dir.join(name), content)?;
    }

    std::fs::write(
        agent_dir.join("README.adoc"),
        include_str!("../../../.agent/README.adoc"),
    )?;

    // Generate live context.adoc and session.adoc from templates
    let project_name = target.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let context_tpl = include_str!("../../../.agent/templates/context.adoc");
    let context_content = context_tpl
        .replace("{project}", project_name)
        .replace("{lang}", "unknown")
        .replace("{ai_name}", "agent")
        .replace("{standard}", "unknown")
        .replace("{edition}", "2024")
        .replace("{dependencies}", "| | |");
    std::fs::write(agent_dir.join("context.adoc"), context_content)?;

    let session_tpl = include_str!("../../../.agent/templates/session.adoc");
    let session_content = session_tpl
        .replace("{project}", project_name);
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
contracts/
.aden/
target/
.git/

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

/// Auto-document a codebase: discover source files, skip unchanged,
/// emit structured contracts, and generate an index.
fn cmd_gen(
    path: &Path,
    out_dir: Option<&Path>,
    auto: bool,
    merge: bool,
    propose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if path.is_file() {
        // Single-file mode: backward compatible
        let source = std::fs::read_to_string(path)?;
        let docs = aden_parse::parse_file(path, &source)?;

        if merge || propose {
            return cmd_gen_contract(path, &source, docs, out_dir, propose);
        }
        return emit_docs(docs, out_dir, path);
    }

    if !path.is_dir() {
        return Err("Path does not exist or is not a file/directory".into());
    }

    let root = find_project_root(path);
    let effective_out = out_dir.unwrap_or_else(|| Path::new("contracts"));

    if merge || propose {
        return cmd_gen_merge(&root, effective_out, propose);
    }

    if auto {
        // ── AUTO MODE: workspace-aware incremental generation ────────────────
        let sources = discover_source_files(&root)?;
        if sources.is_empty() {
            eprintln!("No source files discovered in {}. Is this a supported project?", root.display());
            return Ok(());
        }

        std::fs::create_dir_all(effective_out)?;

        let cache_path = root.join(".aden").join("gen-cache.json");
        let mut cache = load_gen_cache(&cache_path);
        let mut generated = Vec::new();
        let mut skipped = 0usize;

        for src_path in &sources {
            let rel = src_path.strip_prefix(&root).unwrap_or(src_path);
            let contract_rel = rel.with_extension("adoc");
            let contract_path = effective_out.join(&contract_rel);

            // Ensure parent dirs exist
            if let Some(parent) = contract_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            // Check mtime cache
            let src_mtime = src_path.metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let entry = cache.entries.get(contract_path.to_string_lossy().as_ref());
            if let Some(e) = entry
                && e.source_mtime == src_mtime.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
                    && contract_path.exists()
                {
                    skipped += 1;
                    continue;
                }

            let source = match std::fs::read_to_string(src_path) {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => continue,
                Err(e) => {
                    eprintln!("WARN: Failed to read {}: {}", src_path.display(), e);
                    continue;
                }
            };

            let docs = match aden_parse::parse_file(src_path, &source) {
                Ok(d) => d,
                Err(aden_core::Error::UnsupportedLanguage(_)) => continue,
                Err(e) => {
                    eprintln!("WARN: Parse failed for {}: {}", src_path.display(), e);
                    continue;
                }
            };

            for doc in &docs {
                let file_name = format!("{}.adoc", sanitize_anchor(&doc.anchor));
                let file_path = contract_path.parent()
                    .unwrap_or(effective_out)
                    .join(&file_name);
                let mut doc_clone = doc.clone();
                sanitize_source_file(&mut doc_clone);
                std::fs::write(&file_path, aden_emit::emit_document(&doc_clone))?;
                generated.push(file_name.clone());
                println!("Emitted {}", file_path.display());

                // Update cache
                let mtime_secs = src_mtime.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
                cache.entries.insert(
                    file_path.to_string_lossy().to_string(),
                    GenCacheEntry {
                        source_mtime: mtime_secs,
                        source_path: src_path.to_string_lossy().to_string(),
                    }
                );
            }
        }

        save_gen_cache(&cache_path, &cache)?;

        // Generate index
        if !generated.is_empty() {
            let index_path = effective_out.join("README.adoc");
            let mut index = String::new();
            index.push_str("= Contracts Index\n\n");
            index.push_str("Auto-generated by `aden gen --auto .`\n\n");
            index.push_str("|===\n|Symbol |File |Anchor\n");
            for name in &generated {
                index.push_str(&format!("|{} |{} |[[{}]]\n", name, name, name.trim_end_matches(".adoc")));
            }
            index.push_str("|===\n");
            std::fs::write(&index_path, index)?;
            println!("Generated index: {}", index_path.display());
        }

        println!("\nGenerated {} contracts. Skipped {} unchanged files.", generated.len(), skipped);
    } else {
        // ── LEGACY MODE: flat parse_directory output ────────────────────────
        let docs = aden_parse::parse_directory(path)?;
        return emit_docs(docs, out_dir, path);
    }

    // Invalidate caches after generating contracts so next query rebuilds
    let cache_dir = path.join(".aden/cache");
    let _ = std::fs::remove_file(cache_dir.join("graph-cache.json"));
    let _ = std::fs::remove_file(cache_dir.join("index-cache.json"));
    let _ = std::fs::remove_file(cache_dir.join("cache-index.json"));

    Ok(())
}

/// Walk up from `start` looking for a directory containing `.aden/`.
fn find_aden_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    loop {
        if current.join(".aden").is_dir() {
            return Some(current);
        }
        if let Some(parent) = current.parent() {
            current = parent.to_path_buf();
        } else {
            return None;
        }
    }
}

/// Compute the base-cache path for a given contract output path.
fn base_cache_path(contract_path: &Path) -> Option<PathBuf> {
    let file_name = contract_path.file_name()?.to_str()?;
    // If the contract path is absolute, find .aden from its parent;
    // otherwise assume CWD is the project root.
    let root = if contract_path.is_absolute() {
        find_aden_root(contract_path.parent()?)?
    } else {
        find_aden_root(std::env::current_dir().ok()?.as_path())?
    };
    Some(root.join(".aden").join("contract-base").join(file_name))
}

/// Single-file contract generation with three-way merge support.
fn cmd_gen_contract(
    _path: &Path,
    _source: &str,
    docs: Vec<aden_core::Document>,
    out_dir: Option<&Path>,
    propose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_core::contract::{parse_contract, ContractDocument, ContractState, MergeAction, ParseMode};

    if docs.is_empty() {
        return Ok(());
    }

    let effective_out = out_dir.unwrap_or_else(|| Path::new("contracts"));
    std::fs::create_dir_all(effective_out)?;

    let contract_path = effective_out.join(format!("{}.adoc", sanitize_anchor(&docs[0].anchor)));

    // Ground: freshly generated contract from AST
    let ground_doc = ContractDocument::from_document(&docs[0]);

    // Base: last pure generated content (from .aden/contract-base/)
    let base_doc = if let Some(base_path) = base_cache_path(&contract_path) {
        if base_path.exists() {
            let existing = std::fs::read_to_string(&base_path)?;
            parse_contract(&existing, ParseMode::Permissive).unwrap_or_else(|_| ground_doc.clone())
        } else {
            ground_doc.clone()
        }
    } else {
        ground_doc.clone()
    };

    // Working: current contract file on disk (with possible human edits)
    let working_doc = if contract_path.exists() {
        let existing = std::fs::read_to_string(&contract_path)?;
        parse_contract(&existing, ParseMode::Permissive).unwrap_or_else(|e| {
            eprintln!(
                "WARN: Failed to parse existing contract {}: {}. Treating as fresh.",
                contract_path.display(), e
            );
            ground_doc.clone()
        })
    } else {
        ground_doc.clone()
    };

    let state = ContractState::new(ground_doc.clone(), base_doc, working_doc);
    let proposal = state.propose()?;

    if propose {
        println!("// Merge Proposal for {}", contract_path.display());
        println!("//   Preserved: {} | Updated: {} | Conflicts: {} | Inserted: {} | Deleted: {}",
                 proposal.preserved_count, proposal.updated_count, proposal.conflict_count,
                 proposal.inserted_count, proposal.deleted_count);
        for action in &proposal.actions {
            match action {
                MergeAction::UpdateGenerated { index, .. } => {
                    println!("  UPDATE [generated] @ block {}", index);
                }
                MergeAction::PreserveHuman { index } => {
                    println!("  PRESERVE human/agent block @ {}", index);
                }
                MergeAction::Conflict { index, reason } => {
                    println!("  CONFLICT @ block {}: {}", index, reason);
                }
                MergeAction::InsertGenerated { after_index, .. } => {
                    println!("  INSERT [generated] after block {}", after_index);
                }
                MergeAction::DeleteGenerated { index, reason } => {
                    println!("  DELETE [generated] @ block {}: {}", index, reason);
                }
            }
        }
        return Ok(());
    }

    // Merge mode: apply and write
    let merged = state.apply(&proposal)?;
    let output = aden_emit::emit_contract_document(&merged);
    std::fs::write(&contract_path, output)?;
    println!("Merged contract: {}", contract_path.display());

    // Update base cache so next run has clean generated snapshot
    if let Some(base_path) = base_cache_path(&contract_path) {
        if let Some(parent) = base_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&base_path, aden_emit::emit_contract_document(&ground_doc))?;
    }

    Ok(())
}

/// Directory-mode contract generation with merge support.
fn cmd_gen_merge(
    root: &Path,
    effective_out: &Path,
    propose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_core::contract::{parse_contract, ContractDocument, ContractState, ParseMode};

    let sources = discover_source_files(root)?;
    if sources.is_empty() {
        eprintln!("No source files discovered in {}.", root.display());
        return Ok(());
    }

    std::fs::create_dir_all(effective_out)?;

    for src_path in &sources {
        let source = match std::fs::read_to_string(src_path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => continue,
            Err(e) => {
                eprintln!("WARN: Failed to read {}: {}", src_path.display(), e);
                continue;
            }
        };

        let docs = match aden_parse::parse_file(src_path, &source) {
            Ok(d) => d,
            Err(aden_core::Error::UnsupportedLanguage(_)) => continue,
            Err(e) => {
                eprintln!("WARN: Parse failed for {}: {}", src_path.display(), e);
                continue;
            }
        };

        for doc in &docs {
            let file_name = format!("{}.adoc", sanitize_anchor(&doc.anchor));
            let contract_path = effective_out.join(&file_name);

            let ground_doc = ContractDocument::from_document(doc);

            // Base: last pure generated content
            let base_doc = if let Some(base_path) = base_cache_path(&contract_path) {
                if base_path.exists() {
                    let existing = std::fs::read_to_string(&base_path)?;
                    parse_contract(&existing, ParseMode::Permissive).unwrap_or_else(|_| ground_doc.clone())
                } else {
                    ground_doc.clone()
                }
            } else {
                ground_doc.clone()
            };

            // Working: current contract file on disk
            let working_doc = if contract_path.exists() {
                let existing = std::fs::read_to_string(&contract_path)?;
                parse_contract(&existing, ParseMode::Permissive).unwrap_or_else(|_| ground_doc.clone())
            } else {
                ground_doc.clone()
            };

            let state = ContractState::new(ground_doc.clone(), base_doc, working_doc);
            let proposal = state.propose()?;

            if propose {
                println!("// Proposal: {}", contract_path.display());
                println!("//   Preserved: {} | Updated: {} | Conflicts: {} | Inserted: {} | Deleted: {}",
                         proposal.preserved_count, proposal.updated_count, proposal.conflict_count,
                         proposal.inserted_count, proposal.deleted_count);
            } else {
                let merged = state.apply(&proposal)?;
                let output = aden_emit::emit_contract_document(&merged);
                std::fs::write(&contract_path, output)?;
                println!("Merged contract: {}", contract_path.display());

                // Update base cache
                if let Some(base_path) = base_cache_path(&contract_path) {
                    if let Some(parent) = base_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&base_path, aden_emit::emit_contract_document(&ground_doc))?;
                }
            }
        }
    }

    Ok(())
}

fn emit_docs(
    mut docs: Vec<aden_core::Document>,
    out_dir: Option<&Path>,
    _source: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if docs.is_empty() {
        return Ok(());
    }
    // SECURITY: Strip absolute paths from source_file attributes before emitting
    for doc in &mut docs {
        sanitize_source_file(doc);
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

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Default, Serialize, Deserialize)]
struct GenCache {
    entries: HashMap<String, GenCacheEntry>,
}

#[derive(Serialize, Deserialize)]
struct GenCacheEntry {
    source_mtime: u64,
    source_path: String,
}

fn load_gen_cache(path: &Path) -> GenCache {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_gen_cache(path: &Path, cache: &GenCache) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(cache)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Find project root by walking up from `start` looking for Cargo.toml,
/// aden.toml, go.mod, or package.json.
/// Recursively walk a directory and collect files matching any of `exts`.
/// Skips paths that contain any substring in `skip_patterns`.
fn walk_src_files(
    dir: &Path,
    exts: &[&str],
    out: &mut Vec<PathBuf>,
    skip_patterns: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read_dir: {}", e))? {
        let entry = entry.map_err(|e| format!("entry: {}", e))?;
        let p = entry.path();
        let p_str = p.to_string_lossy();
        if skip_patterns.iter().any(|pat| p_str.contains(pat)) {
            continue;
        }
        if entry.file_type()?.is_symlink() {
            continue;
        }
        if entry.file_type()?.is_dir() {
            walk_src_files(&p, exts, out, skip_patterns)?;
        } else if entry.file_type()?.is_file()
            && let Some(ext) = p.extension().and_then(|e| e.to_str())
                && exts.contains(&ext) {
                    out.push(p);
                }
    }
    Ok(())
}

fn find_project_root(start: &Path) -> PathBuf {
    let mut current = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    loop {
        if current.join("Cargo.toml").exists()
            || current.join("aden.toml").exists()
            || current.join("go.mod").exists()
            || current.join("package.json").exists()
        {
            return current;
        }
        if let Some(parent) = current.parent() {
            current = parent.to_path_buf();
        } else {
            return start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
        }
    }
}

/// Discover source files based on build system detected at `root`.
fn discover_source_files(root: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();

    if root.join("Cargo.toml").exists() {
        // Rust: walk all .rs files, prioritizing src/ directories
        walk_src_files(root, &["rs"], &mut files, &["/.git/", "/target/"])?;
        files.sort_by(|a, b| {
            let a_is_src = a.to_string_lossy().contains("/src/");
            let b_is_src = b.to_string_lossy().contains("/src/");
            b_is_src.cmp(&a_is_src)
        });
    } else if root.join("go.mod").exists() {
        // Go: walk **/*.go excluding vendor/
        walk_src_files(root, &["go"], &mut files, &["/vendor/", "/.git/"])?;
    } else if root.join("package.json").exists() {
        // JS/TS: walk src/**/*.{ts,tsx,js,jsx,mjs}
        walk_src_files(root, &["ts", "tsx", "js", "jsx", "mjs", "cjs"], &mut files, &["/node_modules/", "/.git/"])?;
    } else {
        // Generic fallback: all supported extensions
        walk_src_files(root, &["rs", "py", "js", "ts", "go", "c", "cpp", "h"], &mut files, &["/.git/", "/target/"])?;
    }

    Ok(files)
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
    use aden_graph::{Direction};

    if !path.is_dir() {
        return Err("graph requires a directory path".into());
    }

    let graph = aden_graph::cache::build_from_directory_cached(path)?;
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
    

    if !path.is_dir() {
        return Err("asm requires a directory path".into());
    }

    let graph = aden_graph::cache::build_from_directory_cached(path)?;
    let opts = AssemblyOptions {
        start_anchor: from.to_string(),
        max_depth: depth,
        token_budget: budget,
        edge_types,
        block_filter: Vec::new(),
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

/// Strip absolute prefix from source_file attributes to prevent
/// username / home-directory leakage in emitted contracts.
fn sanitize_source_file(doc: &mut aden_core::Document) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    if let Some(source_file) = doc.attributes.get("source_file") {
        let p = std::path::Path::new(source_file);
        if p.is_absolute()
            && let Ok(rel) = p.strip_prefix(&cwd) {
                doc.attributes.insert(
                    "source_file".to_string(),
                    rel.to_string_lossy().to_string(),
                );
            }
    }
}

fn sanitize_anchor(anchor: &str) -> String {
    let s = anchor
        .replace(['/', '#'], "-")
        .replace(":", "-")
        .replace(" ", "-");
    // Truncate to 128 characters to stay well under POSIX max-filename
    // limits while remaining human-readable.
    if s.len() > 128 {
        let hash = aden_core::stable_hash(s.as_bytes());
        format!("{}-{}", &s[..118], &hash[..8])
    } else {
        s
    }
}

fn cmd_query(
    path: &Path,
    from: Option<&str>,
    edge_type: Option<&str>,
    depth: usize,
    backlinks: Option<&str>,
    impact: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_graph::{Direction};
    use std::collections::{HashSet, VecDeque};

    if !path.is_dir() {
        return Err("query requires a directory path".into());
    }

    let graph = aden_graph::cache::build_from_directory_cached(path)?;

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

fn block_filter_for_intent(intent: &QueryIntent) -> Vec<aden_asm::traverse::BlockKind> {
    use aden_asm::traverse::BlockKind::*;
    match intent {
        QueryIntent::Debug => vec![Table, Admonition, Paragraph],
        QueryIntent::Usage => vec![Listing, Table, DescriptionList],
        QueryIntent::Explain => vec![Paragraph, Table, Listing],
        QueryIntent::Refactor => vec![Table, Admonition, Paragraph],
        QueryIntent::Impact => vec![Table, Listing],
        QueryIntent::General => vec![Paragraph, Table, Listing, Admonition, DescriptionList],
    }
}

fn cmd_ask(
    path: &Path,
    question: &str,
    from_override: Option<&str>,
    budget: usize,
    model: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_asm::traverse::{assemble, AssemblyOptions};
    
    

    if !path.is_dir() {
        return Err("ask requires a directory path".into());
    }

    // Step 1: Resolve question to an anchor via search, or use override
    let start_anchor = if let Some(anchor) = from_override {
        anchor.to_string()
    } else {
        let idx = load_or_build_index(path)?;
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
    let graph = aden_graph::cache::build_from_directory_cached(path)?;
    let block_filter = block_filter_for_intent(&intent);
    let opts = AssemblyOptions {
        start_anchor: start_anchor.clone(),
        max_depth: depth,
        token_budget: budget,
        edge_types,
        block_filter,
    };
    let assembled = assemble(&graph, &opts)?;

    // Step 4: Send to LLM or print raw context
    if let Some(model_spec) = model {
        query_llm(model_spec, question, &assembled, &start_anchor)?;
    } else {
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
        println!("//   Nodes   : {} | Bytes: {} / {} ({})", node_count, consumed, budget, budget_label);
        println!("// ────────────────────────────────────────────────");
    }

    Ok(())
}

fn query_llm(
    model_spec: &str,
    question: &str,
    context: &str,
    anchor: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let system_prompt = format!(
        r#"You are an expert software engineering assistant analyzing a codebase.
The user asked: "{}"
I have retrieved the relevant context starting from anchor [[{}]].
Please answer the question based ONLY on the provided context. If the context does not contain enough information, say so explicitly.

Context begins below (--- separates different documents):
"#,
        question, anchor
    );

    let full_prompt = format!("{}\n{}\n", system_prompt, context);

    let (provider, model_name) = if let Some(pos) = model_spec.find(':') {
        (&model_spec[..pos], &model_spec[pos + 1..])
    } else {
        // Auto-detect: try ollama first
        if std::process::Command::new("ollama").arg("list").output().is_ok() {
            ("ollama", model_spec)
        } else {
            return Err("No LLM provider prefix given (e.g., ollama:llama3) and ollama is not available".into());
        }
    };

    match provider {
        "ollama" => {
            println!("Asking ollama ({}) via stdin...", model_name);
            let mut child = std::process::Command::new("ollama")
                .args(["run", model_name])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()?;

            if let Some(stdin) = child.stdin.take() {
                use std::io::Write;
                let mut stdin = stdin;
                stdin.write_all(full_prompt.as_bytes())?;
                // drop stdin to signal EOF
            }

            let output = child.wait_with_output()?;
            if output.status.success() {
                let response = String::from_utf8_lossy(&output.stdout);
                println!("\n=== LLM Response ===\n{}", response);
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("ollama run failed: {}", stderr).into());
            }
        }
        "openai" => {
            let api_key = std::env::var("OPENAI_API_KEY")
                .map_err(|_| "OPENAI_API_KEY not set. Export it to use --model openai:<name>")?;
            println!("QueryingOpenAI ({})...", model_name);

            let payload = serde_json::json!({
                "model": model_name,
                "messages": [
                    { "role": "system", "content": &system_prompt },
                    { "role": "user", "content": context }
                ],
                "temperature": 0.3,
                "max_tokens": 2048
            });

            let output = std::process::Command::new("curl")
                .args([
                    "-sS", "https://api.openai.com/v1/chat/completions",
                    "-H", &format!("Authorization: Bearer {}", api_key),
                    "-H", "Content-Type: application/json",
                    "-d", &payload.to_string(),
                ])
                .output()?;

            if output.status.success() {
                let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
                if let Some(content) = json["choices"][0]["message"]["content"].as_str() {
                    println!("\n=== LLM Response ===\n{}", content);
                } else {
                    println!("Unexpected OpenAI response: {}", String::from_utf8_lossy(&output.stdout));
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("OpenAI API call failed: {}", stderr).into());
            }
        }
        other => {
            return Err(format!(
                "Unknown LLM provider '{}'. Supported: ollama:<model>, openai:<model>",
                other
            )
            .into());
        }
    }

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
    

    if !path.is_dir() {
        return Err("search requires a directory path".into());
    }

    let index = load_or_build_index(path)?;
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
    
    use serde_json::json;

    if !path.is_dir() {
        return Err("locate requires a directory path".into());
    }

    let graph = aden_graph::cache::build_from_directory_cached(path)?;

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
                            Ok(mut docs) if !docs.is_empty() => {
                                for doc in &mut docs {
                                    sanitize_source_file(doc);
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
    use aden_graph::{cycles::find_cycles, integrity::check_hashes};
    use std::collections::HashSet;
    use std::io::Read;

    let mut messages = Vec::new();
    let mut all_anchors: HashSet<String> = HashSet::new();

    let graph = aden_graph::cache::build_from_directory_cached(path)?;

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
        content = new_content;
    } else {
        // Fallback: append to end
        content.push('\n');
        content.push_str(&entry);
    }

    // Atomic write: temp file + rename to prevent race conditions between agents
    let temp_path = session_path.with_extension("tmp");
    std::fs::write(&temp_path, &content)?;
    std::fs::rename(&temp_path, &session_path)?;

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
        } else if trimmed.starts_with("version = ") && !is_aden_crate
            && let Some(name) = current_name.clone() {
                let version = trimmed
                    .trim_start_matches("version = ")
                    .trim_matches('"')
                    .to_string();
                packages.push((name, version));
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
    // SECURITY: `--` terminates option parsing to prevent argument confusion.
    let output = std::process::Command::new("git")
        .args(["diff", "--name-only", "--", since])
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
    // SECURITY: `--` terminates option parsing to prevent argument confusion.
    let output = std::process::Command::new("git")
        .args(["diff", "--name-only", "--", since])
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

    let proposal = aden_propose::load(id, repo_path)
        .map_err(|e| format!("Failed to load proposal '{}': {}", id, e))?;

    println!("Applying proposal: {}", id);
    println!("  Drift type: {}", proposal.drift_type);
    println!("  Target: {}", proposal.target_path.display());
    println!("  Confidence: {:.2}", proposal.confidence);
    println!();

    if proposal.confidence < 0.9 {
        println!("WARNING: Low-confidence proposal ({:.2}). Review carefully.", proposal.confidence);
    }

    // Dispatch based on drift type
    match proposal.drift_type.as_str() {
        "StaleHash" => apply_stale_hash(&proposal)?,
        "MissingContract" => apply_missing_contract(&proposal)?,
        "BrokenReference" => {
            println!("BrokenReference requires manual review. The patch content:");
            println!("---");
            println!("{}", proposal.patch_asciidoc);
            println!("---");
            println!("Cannot auto-apply: requires finding the correct replacement anchor.");
        }
        other => {
            println!("Unknown drift type '{}'. Cannot auto-apply.", other);
            println!("Patch content:");
            println!("---");
            println!("{}", proposal.patch_asciidoc);
            println!("---");
        }
    }

    // Mark proposal as applied in the store
    let mut updated = proposal;
    updated.status = aden_propose::ProposalStatus::Applied;
    aden_propose::persist(&updated, repo_path)?;

    println!("\nProposal {} marked as APPLIED.", id);
    Ok(())
}

fn apply_stale_hash(proposal: &aden_propose::Proposal) -> Result<(), Box<dyn std::error::Error>> {
    let target = &proposal.target_path;
    if !target.exists() {
        return Err(format!("Target file not found: {}", target.display()).into());
    }

    let content = std::fs::read_to_string(target)?;
    let new_line = proposal.patch_asciidoc.trim();

    if !new_line.starts_with(":source_hash:") {
        return Err("Patch does not contain a valid :source_hash: line".into());
    }

    let updated = content
        .lines()
        .map(|line| {
            if line.trim_start().starts_with(":source_hash:") {
                new_line
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    std::fs::write(target, updated)?;
    println!("Updated source hash in {}", target.display());
    Ok(())
}

fn apply_missing_contract(proposal: &aden_propose::Proposal) -> Result<(), Box<dyn std::error::Error>> {
    let target = &proposal.target_path;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(target, &proposal.patch_asciidoc)?;
    println!("Created contract at {}", target.display());
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

// ── OWASP Security Audit ──────────────────────────────────────────

use regex::Regex;
use std::sync::OnceLock;

/// Severity of an OWASP finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum OwaspSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl std::fmt::Display for OwaspSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OwaspSeverity::Info    => write!(f, "INFO"),
            OwaspSeverity::Low     => write!(f, "LOW"),
            OwaspSeverity::Medium  => write!(f, "MED"),
            OwaspSeverity::High    => write!(f, "HIGH"),
            OwaspSeverity::Critical => write!(f, "CRIT"),
        }
    }
}

/// A single OWASP-style finding.
struct OwaspFinding {
    owasp_id: &'static str,
    category: &'static str,
    severity: OwaspSeverity,
    file: PathBuf,
    line: usize,
    snippet: String,
    description: &'static str,
    remediation: &'static str,
}

/// Language-agnostic OWASP Top 10 coding vulnerability scanner.
/// Detects injection, hardcoded secrets, weak crypto, debug modes, eval,
/// command injection, SSRF, error swallowing, and unsafe blocks.
fn cmd_audit(
    path: &Path,
    lang_filter: Option<&str>,
    format: &str,
    strict: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut findings: Vec<OwaspFinding> = Vec::new();

    // Determine which languages to scan
    let scan_all = lang_filter.is_none();
    let want_lang = lang_filter.map(|s| s.to_lowercase());

    // Extensions mapped to language IDs
    let lang_exts: Vec<(&str, &str)> = vec![
        ("rs", "rust"), ("py", "python"), ("go", "go"),
        ("js", "ts"), ("ts", "ts"), ("jsx", "ts"), ("tsx", "ts"),
        ("php", "php"), ("java", "java"), ("cpp", "cpp"), ("c", "c"),
        ("h", "c"), ("hpp", "cpp"),
    ];

    // Collect source files
    let mut files = Vec::new();
    if path.is_file() {
        files.push(path.to_path_buf());
    } else {
        for entry in walkdir::WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if !p.is_file() { continue; }
            if let Some(ext) = p.extension().and_then(|e| e.to_str())
                && let Some(l) = lang_exts.iter().find(|(e, _)| *e == ext.to_lowercase())
                    && (scan_all || want_lang.as_deref() == Some(l.1)) {
                        files.push(p.to_path_buf());
                    }
        }
    }

    type OwaspPattern = (Regex, Option<&'static str>, &'static str, &'static str, OwaspSeverity, &'static str, &'static str);

    // Build pattern table
    static OWASP_PATTERNS: OnceLock<Vec<OwaspPattern>> = OnceLock::new();
    let patterns = OWASP_PATTERNS.get_or_init(|| {
        vec![
            // A03 - Injection: eval / exec / Function (JavaScript / Python / Ruby)
            (Regex::new(r"(?i)\beval\s*\(").unwrap(),                          Some("ts"),   "A03", "Injection",        OwaspSeverity::Critical,
             "Untrusted input passed to eval().",                               "Avoid eval(); use JSON.parse() or safe parsers."),
            (Regex::new(r"(?i)\bexec\s*\(").unwrap(),                         Some("python"), "A03", "Injection",     OwaspSeverity::Critical,
             "Use of exec() on untrusted data.",                                "Remove exec(); validate all input with allow-lists."),
            (Regex::new(r"(?i)\bFunction\s*\(").unwrap(),                      Some("ts"),   "A03", "Injection",        OwaspSeverity::Critical,
             "Dynamic function creation from strings.",                         "Avoid Function(); use static function definitions."),

            // A03 - SQL Injection: string-concat in SQL-like strings
            (Regex::new(r#"(?i)(SELECT|INSERT|UPDATE|DELETE|DROP)\s+[^;]*(\+|\$\{|\{|\{|%s|%d)"#).unwrap(), None, "A03", "SQL Injection", OwaspSeverity::High,
             "SQL built via string concatenation or interpolation.",           "Use parameterized queries / prepared statements."),

            // A03 - Command Injection
            (Regex::new(r#"(?i)(os\.system|subprocess\.call|subprocess\.run|subprocess\.Popen)\s*\([^)]*(shell\s*=\s*True)"#).unwrap(), Some("python"), "A03", "Command Injection", OwaspSeverity::High,
             "Command execution via shell=True or string formatting.",          "Pass arguments as lists (not shell strings) and validate."),
            (Regex::new(r#"(?i)child_process\.(exec|execSync)\s*\([^)]*\+[^)]*\)"#).unwrap(), Some("ts"), "A03", "Command Injection", OwaspSeverity::High,
             "Node child_process.exec with string concatenation.",             "Use child_process.execFile or spawn with argument arrays."),
            (Regex::new(r#"(?i)\.arg\s*\(\s*format!"#).unwrap(),                  Some("rust"), "A03", "Command Injection", OwaspSeverity::Medium,
             "Command arguments built with format!.",                            "Use separate .arg() calls; never interpolate user data."),

            // A04 - Insecure Design: pickle / yaml.load
            (Regex::new(r#"(?i)\bpickle\.loads?\s*\("#).unwrap(),               Some("python"), "A04", "Insecure Deserialization", OwaspSeverity::Critical,
             "Deserialization of untrusted data with pickle.",                   "Use JSON or MessagePack; never unpickle untrusted input."),
            (Regex::new(r#"(?i)\byaml\.load\s*\("#).unwrap(),                  Some("python"), "A04", "Insecure Deserialization", OwaspSeverity::High,
             "yaml.load() is unsafe; yaml.safe_load() should be used.",         "Replace yaml.load() with yaml.safe_load()."),

            // A05 - Security Misconfiguration
            (Regex::new(r#"(?i)(DEBUG\s*=\s*True|debug:\s*true|APP_DEBUG\s*=\s*true)"#).unwrap(), None, "A05", "Security Misconfiguration", OwaspSeverity::Medium,
             "Debug mode enabled in production-like code.",                      "Set DEBUG=False/False in production; read from env vars."),
            (Regex::new(r#"(?i)(CORS_ORIGIN_ALLOW_ALL|Access-Control-Allow-Origin\s*:\s*\*)"#).unwrap(), None, "A05", "Security Misconfiguration", OwaspSeverity::Medium,
             "Permissive CORS wildcard allows any origin.",                     "Restrict origins to an allowed list in production."),

            // A07 - ID & Auth Failures / Cryptographic Failures
            (Regex::new(r#"(?i)(md5|sha1)\s*\("#).unwrap(),                    None, "A07", "Cryptographic Failure",     OwaspSeverity::Medium,
             "Weak hash algorithm (MD5 or SHA1) detected.",                     "Use SHA-256+ or Argon2 for passwords, Blake3 for checksums."),
            (Regex::new(r#"(?i)(password|passwd|pwd|secret|token|api_key)\s*=\s*['\"][^'\"]+['\"]"#).unwrap(), None, "A07", "Hardcoded Secret", OwaspSeverity::High,
             "Possible hardcoded credential in source.",                         "Load secrets from environment variables or a vault."),
            (Regex::new(r#"(?i)(DISABLE_SSL_VERIFICATION|tls_verify\s*=\s*false|verify\s*:\s*false)"#).unwrap(), None, "A07", "Insecure Transport", OwaspSeverity::High,
             "TLS/SSL certificate verification disabled.",                       "Never disable TLS verification in production."),

            // A08 - Software & Data Integrity Failures
            (Regex::new(r#"(?i)InsecureRequestWarning|urllib3\.disable_warnings|warnings\.filterwarnings\s*\([^)]*ignore"#).unwrap(), Some("python"), "A08", "Integrity Failure", OwaspSeverity::Low,
             "Security warnings suppressed.",                                     "Handle warnings properly; do not blanket-ignore them."),

            // A09 - Security Logging Failures
            (Regex::new(r#"(?i)catch\s*\{[^}]*\}|catch\s*\([^)]*\)\s*\{[^}]*\}|except\s*[^:]+:\s*pass|except:\s*pass"#).unwrap(), None, "A09", "Logging Failure", OwaspSeverity::Medium,
             "Empty catch / except block swallows errors silently.",              "Log exceptions before suppressing; never silently pass."),

            // A10 - SSRF
            (Regex::new(r#"(?i)(http\.Get|http\.Post|fetch\s*\(|reqwest::get|axios\.|curl_exec)\s*\([^)]*(req\.|[a-zA-Z_]*(request|params|body|input|user))"#).unwrap(), None, "A10", "SSRF", OwaspSeverity::High,
             "HTTP request built directly from user input.",                      "Validate and sanitize URLs against an allow-list."),

            // Extra - Memory Safety
            (Regex::new(r#"(?i)\bunsafe\s*\{"#).unwrap(),                         Some("rust"), "A04", "Memory Safety",          OwaspSeverity::Medium,
             "unsafe block detected.",                                           "Minimize unsafe; document invariants and get review."),
            (Regex::new(r#"(?i)\bunsafe\s*fn\b"#).unwrap(),                       Some("rust"), "A04", "Memory Safety",          OwaspSeverity::Medium,
             "unsafe function detected.",                                        "Require audit for every unsafe fn; prefer safe APIs."),

            // Extra - Raw pointers (C/C++ / Rust)
            (Regex::new(r#"(?i)\bgets\s*\("#).unwrap(),                           None, "A03", "Buffer Overflow",         OwaspSeverity::Critical,
             "gets() is unsafe and removed in C11.",                             "Use fgets() or getline() with length limits."),
            (Regex::new(r#"(?i)\bstrcpy\s*\("#).unwrap(),                         None, "A03", "Buffer Overflow",         OwaspSeverity::High,
             "strcpy() can overflow; use strncpy or strlcpy.",                  "Replace strcpy with strncpy/strlcpy."),
            (Regex::new(r#"(?i)\bstrcat\s*\("#).unwrap(),                         None, "A03", "Buffer Overflow",         OwaspSeverity::High,
             "strcat() can overflow; use strncat.",                             "Replace strcat with strncat/strlcat."),
        ]
    });

    // Scan files
    let mut total_scanned = 0usize;
    for file in &files {
        total_scanned += 1;

        // Skip documentation directories — they contain example vulnerability strings
        let file_str = file.to_string_lossy();
        if file_str.contains("/.agent/") || file_str.contains("/docs/") {
            continue;
        }

        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        let lang = lang_exts.iter().find(|(e, _)| *e == ext).map(|(_, l)| *l);

        for (line_no, line) in text.lines().enumerate() {
            let trimmed = line.trim();

            // Skip comment lines and string literals that contain patterns
            if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("*")
                || trimmed.starts_with("##") || trimmed.starts_with("#") {
                continue;
            }

            // Skip lines that define regex patterns or are clearly string literals
            if trimmed.starts_with('"') || trimmed.starts_with('\'') || trimmed.contains("Regex::new") {
                continue;
            }

            for (re, pat_lang, owasp_id, category, severity, desc, fix) in patterns.iter() {
                if let Some(pl) = pat_lang
                    && Some(*pl) != lang { continue; }
                if re.is_match(line) {
                    findings.push(OwaspFinding {
                        owasp_id,
                        category,
                        severity: *severity,
                        file: file.clone(),
                        line: line_no + 1,
                        snippet: line.trim().to_string(),
                        description: desc,
                        remediation: fix,
                    });
                }
            }
        }
    }

    // Output
    let is_json = format == "json";
    let is_adoc = format == "adoc";

    if findings.is_empty() {
        if is_json {
            println!("{{\"findings\": [], \"summary\": {{\"total\": 0, \"critical\": 0, \"high\": 0, \"medium\": 0, \"low\": 0, \"info\": 0, \"scanned\": {total_scanned}}}}}");
        } else if is_adoc {
            println!("= OWASP Security Audit\n:date: {}\n\n== Summary\n\n| Severity | Count\n| Critical | 0\n| High     | 0\n| Medium   | 0\n| Low      | 0\n| Info     | 0\n\n_{total_scanned} files scanned. No findings._\n", aden_core::rfc3339_now().split('T').next().unwrap_or(""));
        } else {
            println!("  No OWASP coding vulnerabilities found in {total_scanned} file(s).");
        }
        return Ok(());
    }

    // Sort by severity descending
    findings.sort_by_key(|b| std::cmp::Reverse(b.severity));

    let counts = |sev: OwaspSeverity| findings.iter().filter(|f| f.severity == sev).count();
    let crit = counts(OwaspSeverity::Critical);
    let high = counts(OwaspSeverity::High);
    let med  = counts(OwaspSeverity::Medium);
    let low  = counts(OwaspSeverity::Low);
    let info = counts(OwaspSeverity::Info);

    if is_json {
        println!("{{");
        println!("  \"findings\": [");
        for (i, f) in findings.iter().enumerate() {
            let comma = if i + 1 < findings.len() { "," } else { "" };
            println!("    {{");
            println!("      \"owasp_id\": \"{}\"," , f.owasp_id);
            println!("      \"category\": \"{}\"," , f.category);
            println!("      \"severity\": \"{}\"," , f.severity);
            println!("      \"file\": \"{}\"," , f.file.display());
            println!("      \"line\": {}," , f.line);
            println!("      \"snippet\": \"{}\"," , f.snippet.replace('\"', "\\\""));
            println!("      \"description\": \"{}\"," , f.description.replace('\"', "\\\""));
            println!("      \"remediation\": \"{}\"" , f.remediation.replace('\"', "\\\""));
            println!("    }}{comma}");
        }
        println!("  ],");
        println!("  \"summary\": {{");
        println!("    \"total\": {}, \"critical\": {}, \"high\": {}, \"medium\": {}, \"low\": {}, \"info\": {}, \"scanned\": {}",
            findings.len(), crit, high, med, low, info, total_scanned);
        println!("  }}");
        println!("}}");
    } else if is_adoc {
        let header = format!("= OWASP Security Audit Report\n:date: {}\n:toc: auto\n\n== Summary\n\n| Severity | Count\n| Critical | {crit}\n| High     | {high}\n| Medium   | {med}\n| Low      | {low}\n| Info     | {info}\n\n_{total_scanned} files scanned._\n\n== Findings\n",
            aden_core::rfc3339_now().split('T').next().unwrap_or(""));
        print!("{header}");
        for f in &findings {
            println!("=== [{} {}] {}:{}\n\n`{}`\n\n*Description:* {}\n\n*Remediation:* {}\n", f.severity, f.owasp_id, f.file.display(), f.line, f.snippet, f.description, f.remediation);
        }
    } else {
        println!("  === OWASP Security Audit Findings ===");
        println!("  {} file(s) scanned | {} total finding(s)", total_scanned, findings.len());
        println!("  Severity counts: CRIT={crit} HIGH={high} MED={med} LOW={low} INFO={info}");
        println!();
        for f in &findings {
            println!("  [{}] {} | {}:{}\n    Code: {}\n    {}\n    Fix: {}\n", f.severity, f.owasp_id, f.file.display(), f.line, f.snippet, f.description, f.remediation);
        }
    }

    if strict && (crit > 0 || high > 0) {
        return Err(format!("{} critical/high OWASP finding(s) detected (strict mode)", crit + high).into());
    }
    Ok(())
}

fn run_project_tests(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let has_cargo = path.join("Cargo.toml").exists();
    let has_go_mod = path.join("go.mod").exists();
    let has_pkg_json = path.join("package.json").exists();
    let has_pyproject = path.join("pyproject.toml").exists();
    let has_setup_py = path.join("setup.py").exists();
    let has_reqs = path.join("requirements.txt").exists();

    if has_cargo {
        let output = std::process::Command::new("cargo")
            .args(["test", "--workspace", "--quiet"])
            .current_dir(path)
            .output()?;
        if !output.status.success() {
            return Err(format!("cargo test failed:\n{}", String::from_utf8_lossy(&output.stderr)).into());
        }
        return Ok(());
    }

    if has_go_mod {
        let output = std::process::Command::new("go")
            .args(["test", "./..."])
            .current_dir(path)
            .output()?;
        if !output.status.success() {
            return Err(format!("go test failed:\n{}", String::from_utf8_lossy(&output.stderr)).into());
        }
        return Ok(());
    }

    if has_pkg_json {
        // prefer npm, fall back to yarn or pnpm
        let runner = if std::process::Command::new("npm").arg("--version").output().is_ok() {
            "npm"
        } else if std::process::Command::new("yarn").arg("--version").output().is_ok() {
            "yarn"
        } else if std::process::Command::new("pnpm").arg("--version").output().is_ok() {
            "pnpm"
        } else {
            return Err("No JS package manager found (npm/yarn/pnpm)".into());
        };
        let output = std::process::Command::new(runner)
            .args(["test"])
            .current_dir(path)
            .output()?;
        if !output.status.success() {
            return Err(format!("{} test failed:\n{}", runner, String::from_utf8_lossy(&output.stderr)).into());
        }
        return Ok(());
    }

    if has_pyproject || has_setup_py || has_reqs {
        // try pytest first, then python -m unittest
        let output = std::process::Command::new("pytest")
            .args(["-q"])
            .current_dir(path)
            .output()?;
        if output.status.success() {
            return Ok(());
        }
        let output = std::process::Command::new("python")
            .args(["-m", "pytest", "-q"])
            .current_dir(path)
            .output()?;
        if output.status.success() {
            return Ok(());
        }
        return Err("Python tests failed or no test runner found (tried pytest, python -m pytest)".into());
    }

    Err("No recognized test framework found (checked Cargo.toml, go.mod, package.json, pyproject.toml, setup.py, requirements.txt)".into())
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

    gate!("tests", {
        run_project_tests(path)
    });

    gate!("secret scan", {
        use aden_core::filter::AdenFilter;
        use regex::Regex;
        use std::sync::OnceLock;

        static SECRET_PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
        let patterns = SECRET_PATTERNS.get_or_init(|| {
            vec![
                (Regex::new(r"-----BEGIN (RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----").unwrap(), "private key"),
                (Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(), "AWS access key"),
                (Regex::new(r"ghp_[a-zA-Z0-9]{36}").unwrap(), "GitHub token"),
                (Regex::new(r"gho_[a-zA-Z0-9]{36}").unwrap(), "GitHub OAuth"),
                (Regex::new(r"\b[0-9a-zA-Z]{32,64}\b").unwrap(), "long hex secret (possible API key)"),
                (Regex::new(r#"api[_-]?key\s*=\s*['"][^'"]{8,}['"]"#).unwrap(), "API key assignment"),
                (Regex::new(r#"password\s*=\s*['"][^'"]{4,}['"]"#).unwrap(), "hardcoded password"),
                (Regex::new(r#"secret\s*=\s*['"][^'"]{8,}['"]"#).unwrap(), "hardcoded secret"),
                (Regex::new(r#"token\s*=\s*['"][^'"]{8,}['"]"#).unwrap(), "hardcoded token"),
                (Regex::new(r"eyJ[a-zA-Z0-9_-]*\.eyJ[a-zA-Z0-9_-]*\.[a-zA-Z0-9_-]*").unwrap(), "JWT token"),
                (Regex::new(r"bearer\s+[a-zA-Z0-9_\-\.]{20,}").unwrap(), "Bearer token"),
                (Regex::new(r"mongodb(\+srv)?://[^:]+:[^@]+@").unwrap(), "MongoDB connection string"),
                (Regex::new(r"postgres(ql)?://[^:]+:[^@]+@").unwrap(), "PostgreSQL connection string"),
                (Regex::new(r"mysql://[^:]+:[^@]+@").unwrap(), "MySQL connection string"),
                (Regex::new(r"redis://:[^@]+@").unwrap(), "Redis connection string"),
                (Regex::new(r"\.env\.[a-zA-Z]+\s*\n").unwrap(), "env file"),
                (Regex::new(r"DATABASE_URL\s*=\s*").unwrap(), "DATABASE_URL"),
                (Regex::new(r"sk-[a-zA-Z0-9]{48,}").unwrap(), "OpenAI/sk key"),
            ]
        });

        let non_text_exts: std::collections::HashSet<&str> = [
            "png", "jpg", "jpeg", "gif", "svg", "ico", "bmp",
            "pdf", "zip", "tar", "gz", "bz2", "xz", "7z", "rar",
            "mp3", "mp4", "avi", "mov", "mkv", "wav", "flac",
            "wasm", "so", "dll", "dylib", "exe", "bin", "o", "a",
            "ttf", "otf", "woff", "woff2", "eot", "jpg", "mp3", "mp4",
        ].iter().copied().collect();

        const MAX_SCAN_SIZE: u64 = 1024 * 1024;
        let mut found = 0;
        let filter = AdenFilter::from_directory(path);

        for entry in walkdir::WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if !p.is_file() { continue; }
            if let Ok(rel) = p.strip_prefix(path)
                && filter.should_skip(rel) { continue; }
            if let Some(ext) = p.extension().and_then(|e| e.to_str())
                && non_text_exts.contains(ext.to_lowercase().as_str()) { continue; }
            if let Ok(meta) = std::fs::metadata(p)
                && meta.len() > MAX_SCAN_SIZE { continue; }
            if let Ok(text) = std::fs::read_to_string(p) {
                for (re, name) in patterns {
                    for cap in re.find_iter(&text) {
                        let line_start = text[..cap.start()].rfind('\n').map(|i| i + 1).unwrap_or(0);
                        let line_end = text[cap.end()..].find('\n').map(|i| cap.end() + i).unwrap_or(text.len());
                        let line = &text[line_start..line_end];
                        if line.contains("Regex::new") { continue; }
                        // Skip ignore-file entries that list .env patterns (not actual env contents)
                        if *name == "env file" {
                            let trimmed = line.trim();
                            if trimmed.starts_with(".env") || trimmed.starts_with("*.env") {
                                continue;
                            }
                        }
                        let snippet = &text[cap.start().saturating_sub(20)..(cap.end() + 20).min(text.len())];
                        println!("  {}Secret ({}) in {}: ...{}...{}", red, name, p.display(), snippet.replace('\n', " "), reset);
                        found += 1;
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
        if path.join("Cargo.lock").exists() && !path.join("NOTICE.md").exists() {
            Err(Box::<dyn std::error::Error>::from("NOTICE.md missing. Run 'aden licenses --out NOTICE.md'.".to_string()))
        } else {
            Ok(())
        }
    });

    gate!("owasp audit", {
        cmd_audit(path, None, "text", true)
    });

    gate!("merge conflict markers", {
        use aden_core::filter::AdenFilter;
        let mut found = 0;
        let filter = AdenFilter::from_directory(path);
        for entry in walkdir::WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if !p.is_file() { continue; }
            if let Ok(rel) = p.strip_prefix(path)
                && filter.should_skip(rel) { continue; }
            if let Ok(text) = std::fs::read_to_string(p) {
                for line in text.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("<<<<<<< ") || trimmed.starts_with(">>>>>>> ") || trimmed == "=======" {
                        println!("  {}Merge conflict marker in {}: {}{}", red, p.display(), trimmed, reset);
                        found += 1;
                    }
                }
            }
        }
        if found > 0 {
            Err(Box::<dyn std::error::Error>::from(format!("{} merge conflict marker(s) detected", found)))
        } else {
            Ok(())
        }
    });

    warn!("insecure protocol", {
        use aden_core::filter::AdenFilter;
        let mut found = 0;
        let insecure_re = Regex::new(r"(?i)http://\S+").unwrap();
        let skip_exts: std::collections::HashSet<&str> = ["lock", "adoc", "md", "txt", "svg", "html", "xml"].iter().copied().collect();
        let filter = AdenFilter::from_directory(path);
        for entry in walkdir::WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if !p.is_file() { continue; }
            if let Ok(rel) = p.strip_prefix(path)
                && filter.should_skip(rel) { continue; }
            if let Some(ext) = p.extension().and_then(|e| e.to_str())
                && skip_exts.contains(ext) { continue; }
            if let Ok(text) = std::fs::read_to_string(p) {
                for line in text.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("//") || trimmed.starts_with("#") || trimmed.starts_with("<!--") {
                        continue;
                    }
                    if line.contains("Regex::new") || line.contains("xmlns=") {
                        continue;
                    }
                    if insecure_re.is_match(line) {
                        println!("  {}Insecure http:// URL in {}: {}{}", red, p.display(), line.trim(), reset);
                        found += 1;
                    }
                }
            }
        }
        if found > 0 {
            Err(Box::<dyn std::error::Error>::from(format!("{} insecure http:// URL(s) detected", found)))
        } else {
            Ok(())
        }
    });

    // ── WARNING GATES ─────────────────────────────────────
    // These catch StaleHash / MissingContract — expected during active development.
    // They warn but do NOT block the commit.

    warn!("cargo clippy", {
        if !path.join("Cargo.toml").exists() {
            Ok(())
        } else {
            let output = std::process::Command::new("cargo")
                .args(["clippy", "--workspace", "--", "-W", "clippy::unwrap_used", "-W", "clippy::expect_used", "-W", "clippy::panic"])
                .current_dir(path)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()?;
            if output.status.success() {
                Ok(())
            } else {
                Err(Box::<dyn std::error::Error>::from(
                    format!("cargo clippy found issues:\n{}", String::from_utf8_lossy(&output.stderr))
                ))
            }
        }
    });

    warn!("cargo audit", {
        if !path.join("Cargo.toml").exists() {
            Ok(())
        } else {
            let output = std::process::Command::new("cargo")
                .args(["audit"])
                .current_dir(path)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()?;
            if output.status.success() {
                Ok(())
            } else {
                // cargo audit is optional; warn if it's not installed
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("not found") || stderr.contains("No such file") {
                    Err(Box::<dyn std::error::Error>::from("cargo audit not installed. Install with: cargo install cargo-audit".to_string()))
                } else {
                    Err(Box::<dyn std::error::Error>::from(
                        format!("cargo audit found vulnerabilities:\n{}", stderr)
                    ))
                }
            }
        }
    });

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

/// Load the search index from disk cache, or build and cache it.
fn load_or_build_index(path: &Path) -> Result<aden_index::Index, Box<dyn std::error::Error>> {
    if let Some(cached) = aden_index::try_load(path) {
        return Ok(cached);
    }
    let index = aden_index::Index::from_directory(path)?;
    let _ = aden_index::save(&index, path);
    Ok(index)
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
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
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

    match event {
        aden_heal::DriftEvent::StaleHash { target_path, expected_hash, actual_hash } => {
            confidence = 0.99;
            writeln!(rationale, "Source hash mismatch detected.").unwrap();
            writeln!(rationale, "Expected: {}", expected_hash).unwrap();
            writeln!(rationale, "Actual:   {}", actual_hash).unwrap();
            writeln!(rationale, "The contract at {} needs regeneration.", target_path).unwrap();

            target = PathBuf::from(target_path);
            writeln!(patch, ":source_hash: {}", actual_hash).unwrap();

            Ok(Proposal {
                id,
                target_path: target,
                drift_type: "StaleHash".to_string(),
                confidence,
                status: ProposalStatus::PendingReview,
                rationale,
                patch_asciidoc: patch,
            })
        }
        aden_heal::DriftEvent::MissingContract { source_path, anchor, symbol_name } => {
            confidence = 0.85;
            writeln!(rationale, "No contract found for public symbol '{}'.", symbol_name).unwrap();
            writeln!(rationale, "Source: {}", source_path).unwrap();
            writeln!(rationale, "Suggested anchor: {}", anchor).unwrap();

            target = PathBuf::from(source_path).with_extension("adoc");
            writeln!(patch, "[[{}]]", anchor).unwrap();
            writeln!(patch, "= {}", symbol_name).unwrap();
            writeln!(patch).unwrap();
            writeln!(patch, "agent-note::STUB[Auto-generated by aden-heal. Review before removing this note.]").unwrap();

            Ok(Proposal {
                id,
                target_path: target,
                drift_type: "MissingContract".to_string(),
                confidence,
                status: ProposalStatus::PendingReview,
                rationale,
                patch_asciidoc: patch,
            })
        }
        aden_heal::DriftEvent::BrokenReference { contract_path, ref_anchor, line } => {
            confidence = 0.70;
            writeln!(rationale, "Broken reference detected.").unwrap();
            writeln!(rationale, "Contract: {}", contract_path).unwrap();
            writeln!(rationale, "Missing anchor: {}", ref_anchor).unwrap();

            target = PathBuf::from(contract_path);
            writeln!(patch, "// TODO: Fix broken reference to <<{}>> on line {}", ref_anchor, line).unwrap();

            Ok(Proposal {
                id,
                target_path: target,
                drift_type: "BrokenReference".to_string(),
                confidence,
                status: ProposalStatus::PendingReview,
                rationale,
                patch_asciidoc: patch,
            })
        }
        other => {
            writeln!(rationale, "Drift event detected: {:?}", other).unwrap();
            writeln!(patch, "// Proposed changes for drift event").unwrap();

            Ok(Proposal {
                id,
                target_path: target,
                drift_type: "Unknown".to_string(),
                confidence,
                status: ProposalStatus::PendingReview,
                rationale,
                patch_asciidoc: patch,
            })
        }
    }
}
