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
fn long_version_reports_reproducible_build_identity_and_formats() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    let version = String::from_utf8(output.stdout).unwrap();
    assert!(version.contains("Build:"), "{version}");
    assert!(version.contains("Features:"), "{version}");
    assert!(version.contains("snapshot-v1"), "{version}");
    assert!(version.contains("index-layout-v3"), "{version}");
    assert!(version.contains("gen-logic-v8"), "{version}");
    assert!(version.contains("symbol-lexicon-v2"), "{version}");
    assert!(
        !version.contains("Built at:"),
        "timestamps break reproducibility: {version}"
    );
}

#[test]
fn core_navigation_commands_advertise_shorthand_flags() {
    let cases: &[(&str, &[&str])] = &[
        ("grep", &["-r", "-s", "-n"]),
        ("locate", &["-s", "-c", "-F", "-C", "-n"]),
        ("understand", &["-b"]),
        ("ask", &["-f", "-b", "-i", "-d", "-e", "-s", "-x"]),
        ("asm", &["-f", "-d", "-b", "-e", "-o", "-F", "-s"]),
        ("query", &["-f", "-e", "-d", "-b", "-i", "-F"]),
    ];

    for (command, flags) in cases {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
            .args([*command, "--help"])
            .output()
            .expect("aden binary must be built");
        assert!(output.status.success(), "{command} --help failed");
        let help = String::from_utf8_lossy(&output.stdout);
        for flag in *flags {
            assert!(help.contains(flag), "{command} help lacks {flag}: {help}");
        }
    }
}

#[test]
fn top_level_help_is_focused_but_hidden_commands_remain_callable() {
    let bin = env!("CARGO_BIN_EXE_aden");
    let help = std::process::Command::new(bin)
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    for core in ["tree", "grep", "locate", "understand", "ask", "impact-diff"] {
        assert!(help.contains(core), "core command {core} missing: {help}");
    }
    for optional in ["overlay", "kickoff", "workflow", "session", "audit"] {
        assert!(
            !help.contains(&format!("  {optional} ")),
            "{optional} leaked into focused help: {help}"
        );
    }
    assert!(
        help.lines().count() < 55,
        "top-level help regrew to {} lines",
        help.lines().count()
    );

    let catalog = std::process::Command::new(bin)
        .arg("commands")
        .output()
        .unwrap();
    assert!(catalog.status.success());
    let catalog = String::from_utf8(catalog.stdout).unwrap();
    assert!(catalog.contains("Optional Aden methodology"));
    assert!(catalog.contains("Prefer native project tools"));

    for command in [
        "init",
        "new",
        "gen",
        "check",
        "view",
        "doctor",
        "store",
        "agents-md",
        "overlay",
        "kickoff",
        "workflow",
        "complete",
        "heal",
        "review",
        "session",
        "federation",
        "emergency",
        "regen",
        "query-adq",
        "search",
        "list",
        "communities",
        "viz",
        "scope",
        "config",
        "sync",
        "watch",
        "suggest",
        "diagnose",
        "timeline",
        "lint",
        "test",
        "audit",
        "ready",
        "ci-check",
        "licenses",
    ] {
        let hidden = std::process::Command::new(bin)
            .args([command, "--help"])
            .output()
            .unwrap();
        assert!(
            hidden.status.success(),
            "catalog command {command} stopped parsing"
        );
    }
}

#[test]
fn test_check_finds_no_issues() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
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

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
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
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["context_receipt"]["schema_version"], 1);
    assert!(
        payload["documents"].to_string().contains("Hello world."),
        "Should contain the document body in the default agent response, got: {}",
        stdout
    );
}

#[test]
fn asm_output_modes_are_predictable_without_an_explicit_budget() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);
    let bin = env!("CARGO_BIN_EXE_aden");

    let explicit_llm = std::process::Command::new(bin)
        .args([
            "asm",
            "--format",
            "llm",
            "--from",
            "readme",
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be built");
    assert!(explicit_llm.status.success());
    let llm = String::from_utf8(explicit_llm.stdout).unwrap();
    assert!(llm.contains("Hello world."), "{llm}");
    assert!(serde_json::from_str::<serde_json::Value>(&llm).is_err());

    let human_default = std::process::Command::new(bin)
        .args(["--human", "asm", "--from", "readme", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be built");
    assert!(human_default.status.success());
    let human = String::from_utf8(human_default.stdout).unwrap();
    assert!(human.contains("Hello world."), "{human}");
    assert!(serde_json::from_str::<serde_json::Value>(&human).is_err());

    let help = std::process::Command::new(bin)
        .args(["asm", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("[default: json]"), "{help}");
    assert!(!help.contains("[default: llm]"), "{help}");
}

#[test]
fn test_ask_returns_context() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["ask", "What is module a?", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "aden ask failed: {}", stdout);
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["context_receipt"]["schema_version"], 1);
    assert_eq!(payload["result_state"], "bounded");
    assert_eq!(payload["question_fit"], "bounded");
    assert_eq!(payload["budget"], 4096);
    assert_eq!(payload["expanded"], false);
    assert!(payload["routing_confidence"].is_string());
    assert!(
        payload["context"].as_str().is_some_and(
            |context| context.contains("module-a") && context.contains("This is module a.")
        ),
        "Should mention module-a and its body in the default agent response. stdout:\n{}",
        stdout
    );
}

#[test]
fn ask_definition_lookup_prefers_the_exact_production_symbol() {
    let dir = temp_project::temp_dir();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(
        dir.join("src/app.py"),
        "class Flask:\n    def open_resource(self):\n        pass\n",
    )
    .unwrap();
    std::fs::write(dir.join("tests/test_app.py"), "class Flask:\n    pass\n").unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["ask", "Where is Flask defined?", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be built");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["result_state"], "bounded");
    assert_eq!(payload["routing_confidence"], "clear");
    assert_eq!(
        payload["depth"], 0,
        "definition lookup should avoid a graph-wide expansion"
    );
    let anchor = payload["anchor"].as_str().unwrap();
    assert!(
        anchor.ends_with("/src/app.py#Flask") && !anchor.contains("/tests/"),
        "definition lookup must not route to Flask.open_resource or the test fixture: {anchor}"
    );
}

#[test]
fn ask_fails_small_for_repository_wide_questions() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args([
            "ask",
            "Find all security issues across the repository",
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be built");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["result_state"], "needs_narrowing");
    assert_eq!(payload["question_fit"], "repository_wide");
    assert_eq!(payload["context"], "");
    assert!(payload["anchor"].is_null());
    assert!(
        output.stdout.len() < 2_000,
        "narrowing response should fail small, got {} bytes",
        output.stdout.len()
    );
}

#[test]
fn ask_from_pin_allows_broad_wording_because_the_scope_is_bounded() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args([
            "ask",
            "Audit the entire repository",
            "--from",
            "readme",
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be built");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["result_state"], "bounded");
    assert_eq!(payload["routing_confidence"], "pinned");
    assert_eq!(payload["completeness"], "bounded");
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
    assert!(argv.iter().any(|arg| arg == "-s"));
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

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args([
            "query",
            "--from",
            "readme",
            "--depth",
            "2",
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be built");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "aden query failed: {}", stdout);
    assert!(
        stdout.contains("module-a"),
        "Should show module-a in graph. stdout:\n{}",
        stdout
    );
}

#[test]
fn tree_symbols_is_a_compact_exact_codebase_outline() {
    let dir = temp_project::temp_dir();
    std::fs::write(
        dir.join("app.rs"),
        "fn parse_config() {\n    validate_input();\n}\n\nfn validate_input() {}\n",
    )
    .unwrap();
    std::fs::write(dir.join("main.go"), "package main\n\nfunc GoEntry() {}\n").unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["tree", "--symbols", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be built");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["format"], "symbol-outline-v1");
    assert_eq!(payload["result_state"], "complete");
    assert_eq!(payload["truncated"], false);
    assert_eq!(payload["symbol_count"], payload["returned_symbol_count"]);
    assert_eq!(payload["context_receipt"]["freshness"], "current");
    let outline = payload["outline"].as_str().unwrap();
    assert!(outline.contains("app.rs:"), "{outline}");
    assert!(outline.contains("1-3 parse_config"), "{outline}");
    assert!(outline.contains("5-5 validate_input"), "{outline}");
    assert!(outline.contains("main.go:"), "{outline}");
    assert!(outline.contains("3-3 GoEntry"), "{outline}");
    assert!(
        !outline.contains("validate_input();"),
        "source bodies must not leak into the outline"
    );
}

#[test]
fn tree_symbols_honors_a_subtree_scope() {
    let dir = temp_project::temp_dir();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"tree-scope-qa\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn included() {}\n").unwrap();
    std::fs::write(dir.join("tests/hidden.rs"), "fn excluded() {}\n").unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["tree", "--symbols", &dir.join("src").to_string_lossy()])
        .output()
        .expect("aden binary must be built");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["scope"], "src");
    assert_eq!(payload["file_count"], 1);
    let outline = payload["outline"].as_str().unwrap();
    assert!(outline.contains("src/lib.rs:"), "{outline}");
    assert!(outline.contains("included"), "{outline}");
    assert!(!outline.contains("hidden.rs"), "{outline}");
    assert!(!outline.contains("excluded"), "{outline}");
}

#[test]
fn init_is_zero_footprint_by_default() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["init", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be built");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "aden init failed: {stdout}");
    assert!(!dir.join(".agent").exists(), ".agent must be opt-in");
    assert!(!dir.join(".aden").exists(), ".aden must be opt-in");
    assert!(
        !dir.join(".adenignore").exists(),
        ".adenignore must be opt-in"
    );
    assert!(
        stdout.contains("no .aden directory is required"),
        "{stdout}"
    );
}

#[test]
fn init_templates_preserves_explicit_legacy_scaffolding() {
    let dir = temp_project::temp_dir();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["init", "--templates", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be built");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "aden init --templates failed: {stdout}"
    );
    assert!(dir.join(".agent").is_dir());
    assert!(dir.join(".aden").is_dir());
    assert!(dir.join(".adenignore").exists());
}

#[test]
fn new_project_has_no_aden_specific_files_by_default() {
    let parent = temp_project::temp_dir();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["new", "minimal-app", &parent.to_string_lossy()])
        .output()
        .expect("aden binary must be built");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let project = parent.join("minimal-app");
    assert!(project.join("Cargo.toml").exists());
    assert!(!project.join(".aden").exists());
    assert!(!project.join(".agent").exists());
    assert!(!project.join("AGENTS.md").exists());
    assert!(!project.join(".adenignore").exists());
}

#[test]
fn project_override_does_not_persist_state_in_the_working_tree() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["--project", &dir.to_string_lossy(), "grep", "Hello world"])
        .output()
        .expect("aden binary must be built");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !dir.join(".aden").exists(),
        "--project must not create .aden/project.conf"
    );
}

#[test]
fn test_search_finds_results() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
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

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args([
            "query",
            "--from",
            "readme",
            "--depth",
            "2",
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be built");

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
    let _ = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["locate", "--symbol", "module-a", &dir.to_string_lossy()])
        .output();

    // Test gen (may have no source files)
    let _ = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["gen", "--auto", &dir.to_string_lossy()])
        .output();

    // Test lint
    let lint = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["lint", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be built");
    assert!(lint.status.success(), "lint should pass");

    // Test test --list
    let test = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["test", "--list", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be built");
    assert!(test.status.success(), "test --list should work");

    // Test heal
    let heal = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["heal", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be built");
    assert!(heal.status.success(), "heal should run");

    // Test list
    let list = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["list", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be built");
    assert!(list.status.success(), "list should work");
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("readme"),
        "Should list readme anchor"
    );

    // Test mcp list
    let mcp = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["mcp", "list"])
        .output()
        .expect("aden binary must be built");
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
    let gen_out = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["gen", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be built");
    assert!(gen_out.status.success(), "initial gen should succeed");

    // Rename the method, then regen.
    std::fs::write(
        &src,
        "package m\ntype Foo struct{}\nfunc (c *Foo) Baz() {}\n",
    )
    .unwrap();
    let regen = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["regen", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be built");
    assert!(regen.status.success(), "regen should succeed");

    let list = std::process::Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["list", "--unlimited", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be built");
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
