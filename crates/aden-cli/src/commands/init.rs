use std::path::Path;

use crate::util::validate_name;

/// Create a new project from a language template.
/// Scaffolds build system, aden workspace, and initial documents.
pub fn cmd_new(name: &str, lang: &str, parent: &Path) -> Result<(), Box<dyn std::error::Error>> {
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
        _ => {
            return Err(format!(
                "Language '{}' not yet supported. Try: rust, go, typescript",
                lang
            )
            .into());
        }
    }

    // Run aden init in the new project
    cmd_init(&project_dir)?;

    // Create initial docs directory
    let docs_dir = project_dir.join("docs");
    std::fs::create_dir_all(&docs_dir)?;

    // Create README with project identity
    let readme = format!(
        r###"= {name}
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
    Ok(())
}

fn scaffold_rust(dir: &Path, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let cargo_toml = format!(
        r####"[package]
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
"####,
        name = name
    );
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
    std::fs::write(
        dir.join("go.mod"),
        "module example.com/project\n\ngo 1.24\n",
    )?;
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
    let pkg = format!(
        r###"{{
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
}}"###,
        name = name
    );
    std::fs::write(dir.join("package.json"), pkg)?;
    std::fs::create_dir_all(dir.join("src"))?;
    std::fs::write(
        dir.join("src").join("index.ts"),
        "console.log('Hello, World!');\n",
    )?;
    std::fs::write(dir.join("tsconfig.json"), "\n")?;
    std::fs::write(dir.join(".gitignore"), "node_modules/\ndist/\n*.log\n")?;
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
pub fn cmd_init(target: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let agent_dir = target.join(".agent");
    let templates_dir = agent_dir.join("templates");
    std::fs::create_dir_all(&templates_dir)?;

    // Reference templates (user-editable starting points — do NOT put live
    // files like context.adoc / session.adoc here; they are generated below).
    for (name, content) in [
        (
            "plan.adoc",
            include_str!("../../../../.agent/templates/plan.adoc"),
        ),
        (
            "module.adoc",
            include_str!("../../../../.agent/templates/module.adoc"),
        ),
        (
            "aden-guide.adoc",
            include_str!("../../../../.agent/templates/aden-guide.adoc"),
        ),
        (
            "style-guide.adoc",
            include_str!("../../../../.agent/templates/style-guide.adoc"),
        ),
        (
            "research.adoc",
            include_str!("../../../../.agent/templates/research.adoc"),
        ),
        (
            "constraints.adoc",
            include_str!("../../../../.agent/templates/constraints.adoc"),
        ),
        (
            "onboarding.adoc",
            include_str!("../../../../.agent/templates/onboarding.adoc"),
        ),
        (
            "protocol.adoc",
            include_str!("../../../../.agent/templates/protocol.adoc"),
        ),
        (
            "glossary.adoc",
            include_str!("../../../../.agent/templates/glossary.adoc"),
        ),
        (
            "policy.adoc",
            include_str!("../../../../.agent/templates/policy.adoc"),
        ),
        (
            "kickoff.adoc",
            include_str!("../../../../.agent/templates/kickoff.adoc"),
        ),
        (
            "design.adoc",
            include_str!("../../../../.agent/templates/design.adoc"),
        ),
        (
            "spec.adoc",
            include_str!("../../../../.agent/templates/spec.adoc"),
        ),
        (
            "task.adoc",
            include_str!("../../../../.agent/templates/task.adoc"),
        ),
        (
            "adr.adoc",
            include_str!("../../../../.agent/templates/adr.adoc"),
        ),
        (
            "runbook.adoc",
            include_str!("../../../../.agent/templates/runbook.adoc"),
        ),
        (
            "retrospective.adoc",
            include_str!("../../../../.agent/templates/retrospective.adoc"),
        ),
    ] {
        std::fs::write(templates_dir.join(name), content)?;
    }

    std::fs::write(
        agent_dir.join("README.adoc"),
        include_str!("../../../../.agent/README.adoc"),
    )?;

    // Generate live context.adoc and session.adoc from templates
    let project_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let context_tpl = include_str!("../../../../.agent/templates/context.adoc");
    let context_content = context_tpl
        .replace("{project}", project_name)
        .replace("{lang}", "unknown")
        .replace("{ai_name}", "agent")
        .replace("{standard}", "unknown")
        .replace("{edition}", "2024")
        .replace("{dependencies}", "| | |")
        .replace("agent-context-template", "agent-context");
    std::fs::write(agent_dir.join("context.adoc"), context_content)?;

    let session_tpl = include_str!("../../../../.agent/templates/session.adoc");
    let session_content = session_tpl
        .replace("{project}", project_name)
        .replace("agent-session-template", "agent-session");
    std::fs::write(agent_dir.join("session.adoc"), session_content)?;

    let onboarding_tpl = include_str!("../../../../.agent/templates/onboarding.adoc");
    let onboarding_content = onboarding_tpl
        .replace("{project}", project_name)
        .replace("agent-onboarding-template", "agent-onboarding")
        .replace("agent-context-template", "agent-context")
        .replace("agent-session-template", "agent-session");
    std::fs::write(agent_dir.join("onboarding.adoc"), onboarding_content)?;

    // Security-first scaffolding: contracts are build artifacts
    let aden_dir = target.join(".aden");
    std::fs::create_dir_all(&aden_dir)?;

    // Bootstrap constitution
    let constitution = r###":status: active
:version: 1.0
:ratified: 2026-05-25
:precedence: 100

[[aden-constitution]]
= Aden Constitution

[constitution]
== Core Directives

[rule="Forbid"]
- Never expose secrets, keys, or tokens in any contract
- Never commit `contracts/` or `.aden/` directories
- Never bypass `aden check` before commit

[rule="Warn"]
- All code must build before commit
- All tests must pass before commit
- No TODO/unimplemented in production

[rule="Suggest"]
- Use explicit typed edges
- Include `:source_hash:` in contracts
- Run `aden heal` before commit

== Agent Hierarchy
| Human | Highest | Can override all |
| Constitution | Fixed | Cannot be overridden |
| Agent | Lowest | Limited overrides |

== Related
. <<project-context>>
. <<protocol>>
"###;
    std::fs::write(aden_dir.join("constitution.adoc"), constitution)?;

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
    println!(
        "Generated {} template files.",
        templates_dir.read_dir()?.count()
    );
    println!("Next: AI agents should read .agent/onboarding.adoc before starting work.");
    Ok(())
}
