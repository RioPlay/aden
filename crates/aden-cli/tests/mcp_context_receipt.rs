// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! CLI-to-MCP Context Receipt contract fixture.

use std::process::Command;

const AI_INTEGRATION_GUIDE: &str = include_str!("../../../docs/ai-integration.adoc");

#[test]
fn cli_json_receipt_survives_the_mcp_bridge() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("example.rs"), "fn receipt_fixture() {}\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["grep", "--json", "receipt_fixture"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let cli = String::from_utf8(output.stdout).unwrap();
    let mcp = aden_mcp::preserve_cli_output_for_mcp("grep", &cli);
    let value: serde_json::Value = serde_json::from_str(&mcp).unwrap();
    assert_eq!(value["context_receipt"]["schema_version"], 1);
    assert!(
        value.get("freshness").is_some(),
        "legacy field must remain available"
    );

    // MCP may normalize the terminal newline, but it must not reinterpret the
    // successful command's JSON. Compare parsed values so this fixture pins
    // semantic response equality rather than incidental whitespace.
    let cli_value: serde_json::Value = serde_json::from_str(&cli).unwrap();
    assert_eq!(value, cli_value);
}

#[test]
fn cli_error_survives_the_mcp_bridge_with_only_documented_sanitization() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["locate"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success(), "missing locate input must fail");

    let cli_error = String::from_utf8(output.stderr).unwrap();
    let mcp_error = aden_mcp::preserve_cli_error_for_mcp(&cli_error);

    // The actionable clap contract must remain intact over MCP even though
    // host paths/backtraces are removed at the transport boundary.
    assert!(mcp_error.contains("locate requires"), "{mcp_error}");
    assert!(mcp_error.contains("--symbol"), "{mcp_error}");
    assert!(mcp_error.contains("--caller-of"), "{mcp_error}");
    assert!(!mcp_error.contains(dir.path().to_string_lossy().as_ref()));
    assert!(!mcp_error.contains("RUST_BACKTRACE"));
    assert!(mcp_error.contains("<path>"), "{mcp_error}");
    assert!(
        mcp_error.ends_with("Error: locate requires one of --symbol or --caller-of"),
        "{mcp_error}"
    );
}

#[test]
fn documented_structured_read_examples_execute_with_versioned_receipts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("example.rs"), "fn known_symbol() {}\n").unwrap();

    for example in [
        "aden grep --json \"known_symbol\" .",
        "aden locate --json --symbol \"known_symbol\" .",
    ] {
        assert!(
            AI_INTEGRATION_GUIDE.contains(example),
            "missing `{example}`"
        );
    }

    for args in [
        vec!["grep", "--json", "known_symbol", "."],
        vec!["locate", "--json", "--symbol", "known_symbol", "."],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_aden"))
            .args(&args)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "aden {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["context_receipt"]["schema_version"], 1);
    }
}

#[test]
fn primary_mcp_read_workflows_are_versioned_json() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("example.rs"),
        "fn known_symbol() {}\nfn caller() { known_symbol(); }\n",
    )
    .unwrap();

    let locate = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["locate", "--json", "--symbol", "known_symbol", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(locate.status.success());
    let locate_value: serde_json::Value = serde_json::from_slice(&locate.stdout).unwrap();
    let anchor = locate_value["items"][0]["anchor"]
        .as_str()
        .unwrap()
        .to_owned();

    let cases: Vec<(&str, Vec<String>)> = vec![
        (
            "ask",
            ["ask", "--json", "--strict", "known_symbol", "."]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        ),
        (
            "asm",
            vec![
                "asm".into(),
                "--json".into(),
                "--strict".into(),
                "--from".into(),
                anchor.clone(),
                ".".into(),
            ],
        ),
        (
            "query",
            vec![
                "query".into(),
                "--json".into(),
                "--from".into(),
                anchor,
                ".".into(),
            ],
        ),
        (
            "locate",
            ["locate", "--json", "--symbol", "known_symbol", "."]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        ),
        (
            "understand",
            ["understand", "--json", "known_symbol", "."]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        ),
    ];

    for (tool, args) in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_aden"))
            .args(&args)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "aden {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        let bridged = aden_mcp::preserve_cli_output_for_mcp(
            tool,
            String::from_utf8_lossy(&output.stdout).as_ref(),
        );
        let value: serde_json::Value = serde_json::from_str(&bridged)
            .unwrap_or_else(|e| panic!("{tool} MCP output is not JSON: {e}: {bridged}"));
        let version = value
            .get("schema_version")
            .or_else(|| value.pointer("/context_receipt/schema_version"));
        assert_eq!(version, Some(&serde_json::json!(1)), "{tool}: {value}");
    }
}
