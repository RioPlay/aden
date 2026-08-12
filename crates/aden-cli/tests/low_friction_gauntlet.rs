// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Product-level low-friction gauntlet (CLI half).
//!
//! Ports `scripts/test-low-friction-gauntlet.py` steps 1–5 into a Rust
//! integration test so Windows and Linux share one gate without Python.
//! MCP transport coverage lives in `aden-mcp`'s `mcp_live_gauntlet` test.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn aden_bin() -> &'static str {
    env!("CARGO_BIN_EXE_aden")
}

fn isolated_project() -> (tempfile::TempDir, PathBuf) {
    let work = tempfile::tempdir().expect("tempdir");
    let data = work.path().join("data");
    let project = work.path().join("project");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("main.rs"),
        "/// First main entry point.\nfn main() { helper(); }\nfn helper() {}\n",
    )
    .unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname=\"gauntlet\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .unwrap();
    // SAFETY: integration test process; ADEN_DATA_DIR must isolate graph stores.
    unsafe {
        std::env::set_var("ADEN_DATA_DIR", &data);
    }
    (work, project)
}

fn run(project: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(aden_bin())
        .args(args)
        .current_dir(project)
        .output()
        .unwrap_or_else(|e| panic!("spawn aden {:?}: {e}", args));
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn run_ok(project: &Path, args: &[&str]) -> String {
    let (code, stdout, stderr) = run(project, args);
    assert_eq!(code, 0, "aden {args:?} failed ({code}): {stderr}\n{stdout}");
    stdout
}

fn run_json(project: &Path, args: &[&str]) -> Value {
    let stdout = run_ok(project, args);
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("json parse for {args:?}: {e}\n{stdout}"))
}

fn ask(project: &Path) -> Value {
    let value = run_json(project, &["ask", "Where is the main entry point?", "."]);
    assert!(value.get("anchor").and_then(|v| v.as_str()).is_some(), "{value}");
    assert!(
        value
            .get("context")
            .and_then(|v| v.as_str())
            .is_some_and(|c| !c.is_empty()),
        "{value}"
    );
    let receipt = value
        .get("context_receipt")
        .expect("context_receipt")
        .as_object()
        .expect("receipt object");
    for field in [
        "freshness",
        "graph_revision",
        "observed_source_fingerprint",
        "refresh_cause",
    ] {
        assert!(
            receipt.get(field).and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()),
            "missing receipt field {field}: {value}"
        );
    }
    value
}

#[test]
fn low_friction_cli_gauntlet() {
    let (_work, project) = isolated_project();

    // 1/6 cold-start defaults and zero project footprint
    let first = ask(&project);
    assert_eq!(first["budget"], 4096, "{first}");
    assert_eq!(first["expanded"], false, "{first}");
    assert!(!project.join(".aden").exists());
    assert!(!project.join(".agent").exists());
    let anchor = first["anchor"].as_str().unwrap().to_string();
    let assembly = run_json(&project, &["asm", "--from", &anchor, "."]);
    assert!(
        assembly
            .get("documents")
            .and_then(|d| d.as_array())
            .is_some_and(|a| !a.is_empty()),
        "{assembly}"
    );
    assert_eq!(
        assembly["context_receipt"]["graph_revision"],
        first["context_receipt"]["graph_revision"]
    );

    // 2/6 compact outline and unsafe-question guard
    let outline_payload = run_json(&project, &["tree", "--symbols", "."]);
    let outline = outline_payload["outline"].as_str().unwrap_or("");
    assert_eq!(outline_payload["format"], "symbol-outline-v1");
    assert_eq!(outline_payload["result_state"], "complete");
    assert_eq!(outline_payload["truncated"], false);
    assert_eq!(
        outline_payload["symbol_count"],
        outline_payload["returned_symbol_count"]
    );
    assert!(outline.contains("main.rs:"), "{outline}");
    // Outline lines look like "1-3 main" / "5-5 helper".
    assert!(
        outline.split_whitespace().any(|w| w == "main"),
        "expected main symbol in outline: {outline}"
    );
    assert!(
        outline.split_whitespace().any(|w| w == "helper"),
        "expected helper symbol in outline: {outline}"
    );
    assert!(!outline.contains("helper();"), "{outline}");

    let broad_raw = run_ok(
        &project,
        &["ask", "Find all security issues across the repository", "."],
    );
    let broad: Value = serde_json::from_str(&broad_raw).unwrap();
    assert_eq!(broad["result_state"], "needs_narrowing");
    assert_eq!(broad["context"], "");
    assert!(broad_raw.len() < 2_000, "broad ask too large: {}", broad_raw.len());

    let top_help = run_ok(&project, &["--help"]);
    assert!(top_help.contains("  tree "), "{top_help}");
    assert!(top_help.contains("  understand "), "{top_help}");
    assert!(!top_help.contains("  overlay "), "{top_help}");
    assert!(!top_help.contains("  audit "), "{top_help}");
    let commands = run_ok(&project, &["commands"]);
    assert!(
        commands.contains("Optional Aden methodology"),
        "{commands}"
    );

    // 3/6 output-mode and strict-boundary matrix
    let llm = run_ok(&project, &["asm", "--format", "llm", "--from", &anchor, "."]);
    assert!(llm.contains("main"), "{llm}");
    assert!(!llm.trim_start().starts_with('{'), "{llm}");
    let human = run_ok(&project, &["--human", "asm", "--from", &anchor, "."]);
    assert!(human.contains("main"), "{human}");
    assert!(!human.trim_start().starts_with('{'), "{human}");
    let help_text = run_ok(&project, &["asm", "--help"]);
    assert!(help_text.contains("[default: json]"), "{help_text}");
    assert!(!help_text.contains("[default: llm]"), "{help_text}");

    let strict = run_json(
        &project,
        &["ask", "--strict", "Where is the main entry point?", "."],
    );
    assert!(
        strict["context_receipt"]
            .get("graph_revision")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty()),
        "{strict}"
    );
    let tiny_raw = run_ok(
        &project,
        &[
            "ask",
            "--strict",
            "--budget",
            "15",
            "Where is the main entry point?",
            ".",
        ],
    );
    let approx_tokens = (tiny_raw.len() + 3) / 4;
    assert!(approx_tokens <= 15, "tiny ask over budget: {approx_tokens}\n{tiny_raw}");
    let tiny: Value = serde_json::from_str(&tiny_raw).unwrap();
    assert!(
        tiny.get("incomplete")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || tiny
                .get("truncated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        "{tiny}"
    );

    // 4/6 no-change and same-size edit refresh
    let unchanged = ask(&project);
    assert_eq!(
        unchanged["context_receipt"]["graph_revision"],
        first["context_receipt"]["graph_revision"]
    );
    std::fs::write(
        project.join("main.rs"),
        "/// Other main entry point.\nfn main() { helper(); }\nfn helper() {}\n",
    )
    .unwrap();
    let changed = ask(&project);
    assert_eq!(
        changed["context_receipt"]["refresh_cause"], "source_changed",
        "{changed}"
    );
    assert_ne!(
        changed["context_receipt"]["graph_revision"],
        first["context_receipt"]["graph_revision"]
    );
    assert_ne!(
        changed["context_receipt"]["observed_source_fingerprint"],
        first["context_receipt"]["observed_source_fingerprint"]
    );

    // 5/6 huge-outline bound, explicit escape, and subtree recovery
    let large: String = (0..4_200)
        .map(|index| format!("fn generated_symbol_{index}() {{}}\n"))
        .collect();
    std::fs::write(project.join("large.rs"), large).unwrap();
    let focused = project.join("focused");
    std::fs::create_dir_all(&focused).unwrap();
    std::fs::write(focused.join("focus.rs"), "fn only_this_subtree() {}\n").unwrap();

    let bounded_outline = run_json(&project, &["tree", "--symbols", "."]);
    assert_eq!(bounded_outline["result_state"], "truncated", "{bounded_outline}");
    assert_eq!(bounded_outline["truncated"], true, "{bounded_outline}");
    assert!(
        bounded_outline["returned_symbol_count"].as_u64().unwrap()
            < bounded_outline["symbol_count"].as_u64().unwrap(),
        "{bounded_outline}"
    );
    let outline_bytes = bounded_outline["outline"].as_str().unwrap_or("").len();
    assert!(outline_bytes <= 96 * 1024, "outline too large: {outline_bytes}");
    let next = bounded_outline["next_action"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(next.contains("subtree"), "{bounded_outline}");

    let full_outline = run_json(&project, &["--unlimited", "tree", "--symbols", "."]);
    assert_eq!(full_outline["result_state"], "complete", "{full_outline}");
    assert_eq!(
        full_outline["returned_symbol_count"],
        full_outline["symbol_count"]
    );
    assert_eq!(
        full_outline["symbol_count"],
        bounded_outline["symbol_count"]
    );

    let scoped_outline = run_json(&project, &["tree", "--symbols", "focused"]);
    assert_eq!(scoped_outline["result_state"], "complete", "{scoped_outline}");
    assert_eq!(scoped_outline["file_count"], 1, "{scoped_outline}");
    let scoped_text = scoped_outline["outline"].as_str().unwrap_or("");
    assert!(
        scoped_text.contains("only_this_subtree"),
        "{scoped_outline}"
    );
    assert!(
        !scoped_text.contains("generated_symbol"),
        "{scoped_outline}"
    );
}
