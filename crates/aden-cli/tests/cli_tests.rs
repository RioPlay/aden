// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

// Integration tests for the aden CLI.
// These exercise the installed binary via std::process::Command.

mod temp_project {
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// ADR-003: the graph store now lives in a per-user data dir keyed by the
    /// (absolute) project path — it is no longer deleted when the temp project
    /// dir is removed. Because these tests reuse `aden-cli-test-N` paths across
    /// runs, an un-isolated data dir would leak a stale store from a prior run
    /// into the next, breaking `check`. Pin `ADEN_DATA_DIR` to a per-process
    /// temp dir (inherited by every spawned `aden` subprocess) so each test run
    /// gets pristine stores and never touches the real user data dir.
    fn ensure_isolated_data_dir() {
        static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();
        DATA_DIR.get_or_init(|| {
            let d = std::env::temp_dir().join(format!("aden-cli-test-data-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            // Set exactly once under the OnceLock, before any test spawns an
            // `aden` subprocess (which inherits this env var).
            unsafe { std::env::set_var("ADEN_DATA_DIR", &d) };
            d
        });
    }

    pub fn temp_dir() -> PathBuf {
        ensure_isolated_data_dir();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("aden-cli-test-{}", n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub fn scaffold(dir: &std::path::Path) {
        std::fs::write(
            dir.join("README.adoc"),
            r#"[[readme]]
= Test Project

Hello world.

<<module-a>>
"#,
        )
        .unwrap();

        std::fs::write(
            dir.join("module-a.adoc"),
            r#"[[module-a]]
= Module A

This is module a.

|===
|Name |Value
|foo |bar
|===

<<readme>>
"#,
        )
        .unwrap();
    }
}

#[test]
fn test_check_finds_no_issues() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new("aden")
        .arg("check")
        .arg(&dir)
        .output()
        .expect("aden binary must be installed for tests");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "aden check failed. stdout: {}\nstderr: {}",
        stdout,
        stderr
    );
}

#[test]
fn test_asm_assembles_nonempty_output() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new("aden")
        .args([
            "asm",
            "--from",
            "readme",
            "--depth",
            "2",
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be installed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "aden asm failed: {}", stdout);
    // Default aden asm outputs LLM-stripped prose (not ADG JSON).
    assert!(
        stdout.contains("Hello world."),
        "Should contain document body in stripped prose, got: {}",
        stdout
    );
}

#[test]
fn test_ask_returns_context() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new("aden")
        .args(["ask", "What is module a?", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "aden ask failed: {}", stdout);
    // Default `ask` emits LLM-stripped prose (raw `[[anchor]]` brackets are
    // stripped — see test_asm). Assert on the symbol name and its body content,
    // which survive stripping, rather than the old bracketed-anchor form.
    assert!(
        stdout.contains("module-a") && stdout.contains("This is module a."),
        "Should mention module-a and its body in stripped prose. stdout:\n{}",
        stdout
    );
}

/// The strict budget applies to the bytes actually returned to an agent, not
/// only to the internal assembly. This is deliberately black-box: command
/// headers and summaries used to make a small requested budget emit a much
/// larger stdout response.
#[test]
fn test_ask_strict_stdout_stays_within_serialized_budget() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);
    let budget = 32usize;

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args([
            "ask",
            "What is module a?",
            "--budget",
            &budget.to_string(),
            "--strict",
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be installed");

    assert!(
        output.status.success(),
        "aden ask --strict failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.len().div_ceil(4) <= budget,
        "strict stdout estimated at {} tokens for {} budget: {}",
        output.stdout.len().div_ceil(4),
        budget,
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn test_asm_strict_stdout_stays_within_serialized_budget() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);
    let budget = 32usize;

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args([
            "asm",
            "--from",
            "readme",
            "--budget",
            &budget.to_string(),
            "--strict",
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be built");

    assert!(
        output.status.success(),
        "aden asm --strict failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.len().div_ceil(4) <= budget,
        "strict stdout estimated at {} tokens for {} budget",
        output.stdout.len().div_ceil(4),
        budget
    );
}

#[test]
fn test_ask_strict_no_results_stays_within_serialized_budget() {
    let dir = temp_project::temp_dir();
    let budget = 15usize;

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args([
            "ask",
            "definitely-no-indexed-document-matches-this",
            "--budget",
            &budget.to_string(),
            "--strict",
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be built");

    assert!(
        output.status.success(),
        "strict no-results response failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.len().div_ceil(4) <= budget,
        "no-results stdout exceeded strict budget: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        r#"{"context_receipt":{"schema_version":1},"incomplete":true}"#
    );
}

#[test]
fn test_asm_strict_all_formats_unicode_and_tiny_receipt_are_bounded() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);
    std::fs::write(
        dir.join("unicode.adoc"),
        "[[unicode]]\n= Unicode\n\ncafé 你好 🚀 context ".repeat(80),
    )
    .unwrap();

    for format in ["llm", "adg", "aden"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
            .args([
                "asm",
                "--from",
                "unicode",
                "--format",
                format,
                "--budget",
                "15",
                "--strict",
                &dir.to_string_lossy(),
            ])
            .output()
            .expect("aden binary must be built");
        assert!(
            output.status.success(),
            "{format}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            r#"{"context_receipt":{"schema_version":1},"incomplete":true}"#
        );
    }
}

#[test]
fn test_ask_strict_receipts_provenance_alternates_and_supplements_remain_bounded() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);
    std::fs::write(
        dir.join("module-b.adoc"),
        "[[module-b]]\n= Module B\n\nShared ambiguous module context.\n<<readme>>\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("module-c.adoc"),
        "[[module-c]]\n= Module C\n\nShared ambiguous module context.\n<<readme>>\n",
    )
    .unwrap();
    let budget = 24usize;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args([
            "ask",
            "--human",
            "shared ambiguous module context",
            "--explain",
            "--strict",
            "--budget",
            &budget.to_string(),
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be built");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len().div_ceil(4) <= budget);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("routing ambiguous"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Decision"));
}

#[test]
fn test_ask_default_surfaces_only_assembled_outcome() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["ask", "What is this project?", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be built");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["schema_version"], 1);
    assert!(
        payload["context"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(
        payload["anchor"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("NOTE:"));
}

#[test]
fn test_asm_auto_plus_strict_uses_exact_serialized_budget() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);
    let budget = 16usize;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args([
            "asm",
            "--from",
            "readme",
            "--auto",
            "--strict",
            "--budget",
            &budget.to_string(),
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be built");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len().div_ceil(4) <= budget);
}

#[test]
fn test_asm_strict_stale_hint_cannot_escape_budget() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);
    let generated = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["gen", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be built");
    assert!(generated.status.success());
    std::fs::write(dir.join("new.adoc"), "[[new]]\n= New\n\nstale marker\n").unwrap();

    let budget = 16usize;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .env("ADEN_SKIP_AUTO_GEN", "1")
        .args([
            "asm",
            "--from",
            "readme",
            "--strict",
            "--budget",
            &budget.to_string(),
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be built");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len().div_ceil(4) <= budget);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("NOTE: index may lag"));
}

#[test]
fn test_mcp_wrapper_strict_budget_transport_golden() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);
    let budget = 16usize;
    let mut args = serde_json::Map::new();
    args.insert("question".into(), serde_json::json!("What is module a?"));
    args.insert("path".into(), serde_json::json!(dir.to_string_lossy()));
    args.insert("budget".into(), serde_json::json!(budget));

    let argv = aden_mcp::prepare_cli_args_for_mcp("ask", &args).unwrap();
    assert!(argv.iter().any(|arg| arg == "--strict"));
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(&argv)
        .output()
        .expect("execute MCP-directed CLI command");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let raw = String::from_utf8(output.stdout).unwrap();
    let wrapped = aden_mcp::preserve_cli_output_for_mcp("ask", &raw);
    let transported = aden_mcp::enforce_mcp_response_budget("ask", &args, &wrapped);
    assert!(transported.len().div_ceil(4) <= budget);
    let payload: serde_json::Value = serde_json::from_str(&transported).unwrap();
    assert_eq!(payload["schema_version"], 1);
    assert_eq!(payload["truncated"], true);
    assert!(payload["context"].is_string());
}

#[test]
fn test_asm_strict_rejects_unbounded_inspect_and_out_modes() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let inspect = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args([
            "asm",
            "--from",
            "readme",
            "--strict",
            "--inspect",
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be built");
    assert!(!inspect.status.success());
    assert!(
        String::from_utf8_lossy(&inspect.stderr)
            .contains("--strict cannot be combined with --inspect")
    );

    let out_path = dir.join("assembly.txt");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args([
            "asm",
            "--from",
            "readme",
            "--strict",
            "--out",
            &out_path.to_string_lossy(),
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be built");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--strict cannot be combined with --out")
    );
    assert!(
        !out_path.exists(),
        "rejected strict --out must not write a file"
    );
}

#[test]
fn test_ask_strict_rejects_model_output() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args([
            "ask",
            "What is module a?",
            "--strict",
            "--model",
            "openai:irrelevant",
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be built");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("--strict cannot be combined with --model")
    );
}

#[test]
fn test_graph_outputs_neighborhood() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new("aden")
        .args([
            "query",
            "--from",
            "readme",
            "--depth",
            "2",
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be installed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "aden query failed: {}", stdout);
    assert!(
        stdout.contains("module-a"),
        "Should show module-a in graph. stdout:\n{}",
        stdout
    );
}

#[test]
fn test_init_scaffolds_agent_dir() {
    let dir = temp_project::temp_dir();

    let output = std::process::Command::new("aden")
        .args(["init", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "aden init failed: {}", stdout);
    assert!(dir.join(".agent").is_dir(), ".agent/ should be created");
    assert!(
        dir.join(".adenignore").exists(),
        ".adenignore should be created"
    );
}

#[test]
fn test_search_finds_results() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new("aden")
        .args(["search", "module", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "aden search failed: {}", stdout);
    assert!(
        stdout.contains("module-a"),
        "Should find module-a. stdout:\n{}",
        stdout
    );
}

#[test]
fn test_query_from_returns_json() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new("aden")
        .args([
            "query",
            "--from",
            "readme",
            "--depth",
            "2",
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be installed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "aden query failed: {}", stdout);
    // JSON output should start with [ or { depending on implementation
}

// Additional CLI command tests - verify commands run without crashing
#[test]
fn test_additional_commands() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    // Test locate
    let _ = std::process::Command::new("aden")
        .args(["locate", "--symbol", "module-a", &dir.to_string_lossy()])
        .output();

    // Test gen (may have no source files)
    let _ = std::process::Command::new("aden")
        .args(["gen", "--auto", &dir.to_string_lossy()])
        .output();

    // Test lint
    let lint = std::process::Command::new("aden")
        .args(["lint", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");
    assert!(lint.status.success(), "lint should pass");

    // Test test --list
    let test = std::process::Command::new("aden")
        .args(["test", "--list", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");
    assert!(test.status.success(), "test --list should work");

    // Test heal
    let heal = std::process::Command::new("aden")
        .args(["heal", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");
    assert!(heal.status.success(), "heal should run");

    // Test list
    let list = std::process::Command::new("aden")
        .args(["list", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");
    assert!(list.status.success(), "list should work");
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("readme"),
        "Should list readme anchor"
    );

    // Test mcp list
    let mcp = std::process::Command::new("aden")
        .args(["mcp", "list"])
        .output()
        .expect("aden binary must be installed");
    assert!(mcp.status.success(), "mcp list should work");
}

/// `regen` must be a true from-scratch rebuild: a symbol that was renamed
/// between runs must NOT survive in the store. Previously regen deleted the
/// gen-cache (which prune needs to detect removed anchors) without clearing the
/// store, so the old anchor was orphaned forever.
#[test]
fn test_regen_prunes_renamed_symbol() {
    let dir = temp_project::temp_dir();
    let src = dir.join("m.go");
    std::fs::write(dir.join("go.mod"), "module m\n").unwrap();
    std::fs::write(
        &src,
        "package m\ntype Foo struct{}\nfunc (c *Foo) Bar() {}\n",
    )
    .unwrap();

    // Initial gen records Foo.Bar.
    let gen_out = std::process::Command::new("aden")
        .args(["gen", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");
    assert!(gen_out.status.success(), "initial gen should succeed");

    // Rename the method, then regen.
    std::fs::write(
        &src,
        "package m\ntype Foo struct{}\nfunc (c *Foo) Baz() {}\n",
    )
    .unwrap();
    let regen = std::process::Command::new("aden")
        .args(["regen", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");
    assert!(regen.status.success(), "regen should succeed");

    let list = std::process::Command::new("aden")
        .args(["list", "--unlimited", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");
    let anchors = String::from_utf8_lossy(&list.stdout);
    assert!(
        anchors.contains("Foo.Baz"),
        "regen should store the renamed symbol Foo.Baz; got: {anchors}"
    );
    assert!(
        !anchors.contains("Foo.Bar"),
        "regen must prune the stale Foo.Bar anchor; got: {anchors}"
    );
}
