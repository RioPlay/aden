// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

use crate::util::quick_health_score;

pub fn cmd_doctor(path: &Path, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Collect structured check results regardless of output mode, then either
    // print the human report or emit JSON. Each check is {name, ok, detail}.
    struct Check {
        name: String,
        ok: bool,
        detail: String,
    }
    let mut checks: Vec<Check> = Vec::new();
    let mut issues: Vec<String> = Vec::new();

    macro_rules! chk {
        ($name:expr, $ok:expr, $detail:expr, $error:expr) => {{
            let ok: bool = $ok;
            let detail: String = $detail.to_string();
            if !ok && $error {
                issues.push($name.to_string());
            }
            // `$error` is consumed above (blocking checks push to `issues`); the
            // Check struct itself only needs name/ok/detail for rendering.
            checks.push(Check {
                name: $name.to_string(),
                ok,
                detail,
            });
        }};
    }

    // git
    let git_ok = std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok();
    chk!(
        "git",
        git_ok,
        if git_ok { "git found" } else { "git NOT FOUND" },
        true
    );

    // Toolchains (language-agnostic — probe whatever manifests exist)
    let is_rust = path.join("Cargo.toml").exists();
    let is_node = path.join("package.json").exists();
    let is_python = path.join("pyproject.toml").exists() || path.join("setup.py").exists();
    let is_go = path.join("go.mod").exists();

    if is_rust {
        for tool in &["rustc", "cargo"] {
            let ok = std::process::Command::new(tool)
                .arg("--version")
                .output()
                .is_ok();
            chk!(
                format!("{tool} (Rust)"),
                ok,
                if ok {
                    format!("{tool} found")
                } else {
                    format!("{tool} NOT FOUND")
                },
                true
            );
        }
    }
    for (flag, tools, req) in [
        (is_node, vec!["node", "npm"], false),
        (is_python, vec!["python3"], false),
        (is_go, vec!["go"], false),
    ] {
        if flag {
            for tool in tools {
                let ok = std::process::Command::new(tool)
                    .arg("--version")
                    .output()
                    .is_ok();
                chk!(
                    tool,
                    ok,
                    if ok {
                        format!("{tool} found")
                    } else {
                        format!("{tool} not found")
                    },
                    req
                );
            }
        }
    }

    // aden CLI
    let aden_ok = std::process::Command::new("aden")
        .arg("--version")
        .output()
        .is_ok();
    chk!(
        "aden CLI",
        aden_ok,
        if aden_ok {
            "aden found in PATH"
        } else {
            "aden NOT in PATH"
        },
        true
    );

    // Signing key (optional)
    let key_dir = dirs::home_dir()
        .unwrap_or_default()
        .join(".aden")
        .join("keys");
    let signing_key = key_dir.read_dir().ok().and_then(|mut d| {
        d.find_map(|e| {
            let e = e.ok()?;
            let s = e.file_name().to_string_lossy().to_string();
            if s.ends_with(".pub") { Some(s) } else { None }
        })
    });
    chk!(
        "signing key",
        signing_key.is_some(),
        signing_key
            .as_deref()
            .unwrap_or("none (optional — used for contract attestation)"),
        false
    );

    // Store location (ADR-003: graph lives in the per-user data dir, not in-tree)
    let store_root = crate::util::find_project_root(path);
    let store_path = aden_paths::store_dir(&store_root);
    let store_exists = store_path.exists();
    chk!(
        "store",
        store_exists,
        if store_exists {
            format!("{}", store_path.display())
        } else {
            format!("{} (run 'aden gen')", store_path.display())
        },
        false
    );

    // Repo scaffold
    chk!(
        ".agent/",
        path.join(".agent").is_dir(),
        if path.join(".agent").is_dir() {
            ".agent/ present"
        } else {
            "not present — run 'aden init'"
        },
        false
    );
    chk!(
        ".adenignore",
        path.join(".adenignore").exists(),
        if path.join(".adenignore").exists() {
            "present"
        } else {
            "not present — built-in defaults used"
        },
        false
    );

    // Documentation
    let has_docs = path.join("docs").is_dir()
        || path.join("doc").is_dir()
        || path.join("documentation").is_dir()
        || path.join("README.md").exists()
        || path.join("README.adoc").exists()
        || path.join("README.rst").exists()
        || path.join(".agent").is_dir();
    if !has_docs {
        issues.push("no documentation found".to_string());
    }
    chk!(
        "docs",
        has_docs,
        if has_docs {
            "documentation present"
        } else {
            "no docs/README found"
        },
        true
    );

    // Graph connectivity (what this tool actually measures: are all nodes connected?
    // Different from `heal`'s contract-freshness score — see `aden heal` for that.)
    let graph_score = quick_health_score(path).ok();
    if let Some(s) = graph_score {
        const EPS: f64 = 0.01;
        let ok = (1.0 - s).abs() < EPS;
        if !ok {
            issues.push(format!("graph connectivity {:.2} (target 1.00)", s));
        }
        chk!(
            "graph connectivity",
            ok,
            format!(
                "{:.2}/1.00{}",
                s,
                if ok {
                    ""
                } else {
                    " — run 'aden heal .' to see drift"
                }
            ),
            false
        );
    }

    if json {
        let env = serde_json::json!({
            "ok": issues.is_empty(),
            "issues": issues,
            "checks": checks.iter().map(|c| serde_json::json!({
                "name": c.name,
                "ok": c.ok,
                "detail": c.detail,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&env)?);
        return Ok(());
    }

    println!("— Version Control —");
    for c in checks.iter().filter(|c| c.name == "git") {
        println!("{} {}", if c.ok { "✓" } else { "✗" }, c.detail);
    }
    println!("\n— Project Language Toolchain —");
    let toolchains: Vec<_> = checks
        .iter()
        .filter(|c| {
            matches!(
                c.name.as_str(),
                "rustc (Rust)" | "cargo (Rust)" | "node" | "npm" | "python3" | "go"
            )
        })
        .collect();
    if toolchains.is_empty() {
        println!("  (no recognised project manifest — skipping toolchain check)");
    } else {
        for c in &toolchains {
            println!("{} {}", if c.ok { "✓" } else { "⚠" }, c.detail);
        }
    }
    println!("\n— Aden —");
    for c in checks.iter().filter(|c| c.name == "aden CLI") {
        println!("{} {}", if c.ok { "✓" } else { "✗" }, c.detail);
    }
    for c in checks.iter().filter(|c| c.name == "signing key") {
        println!("{} {}", if c.ok { "✓" } else { " " }, c.detail);
    }
    println!("\n— Repo Health —");
    for c in checks
        .iter()
        .filter(|c| matches!(c.name.as_str(), ".agent/" | ".adenignore" | "docs"))
    {
        println!("{} {}", if c.ok { "✓" } else { "⚠" }, c.detail);
    }
    println!("\n— Knowledge Graph Connectivity —");
    for c in checks.iter().filter(|c| c.name == "graph connectivity") {
        println!(
            "{} Health Score: {}",
            if c.ok { "✓" } else { "⚠" },
            c.detail
        );
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
