// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! CLI-to-MCP Context Receipt contract fixture.

use std::process::Command;

const AI_INTEGRATION_GUIDE: &str = include_str!("../../../docs/ai-integration.adoc");

fn assert_authoritative_receipt(value: &serde_json::Value, tool: &str) {
    let receipt = &value["context_receipt"];
    assert_eq!(receipt["schema_version"], 1, "{tool}: {value}");
    assert_eq!(receipt["freshness"], "current", "{tool}: {value}");
    assert!(
        receipt["graph_revision"]
            .as_str()
            .is_some_and(|revision| !revision.is_empty()),
        "{tool} omitted the graph revision: {value}"
    );
    assert!(
        receipt["observed_source_fingerprint"]
            .as_str()
            .is_some_and(|fingerprint| !fingerprint.is_empty()),
        "{tool} omitted the observed source fingerprint: {value}"
    );
    assert!(
        receipt["refresh_cause"].as_str().is_some(),
        "{tool} omitted the refresh cause: {value}"
    );
    assert_eq!(value["freshness"], "current", "{tool}: {value}");
    assert_eq!(value["index_stale"], false, "{tool}: {value}");
}

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

#[test]
fn llm_default_journey_needs_no_explicit_budget_and_proves_its_graph_revision() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("example.rs"),
        "/// Entry point for the receipt fixture.\nfn main() { helper(); }\nfn helper() {}\n",
    )
    .unwrap();

    // Exercise the defaults exactly as an LLM-facing caller does: structured
    // output, automatic freshness, and no explicit token budget.
    let ask = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["ask", "Where is the main entry point?", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        ask.status.success(),
        "{}",
        String::from_utf8_lossy(&ask.stderr)
    );
    let ask_value: serde_json::Value = serde_json::from_slice(&ask.stdout).unwrap();
    assert_authoritative_receipt(&ask_value, "ask");
    let anchor = ask_value["anchor"]
        .as_str()
        .expect("ask must select an anchor for the fixture");
    assert!(ask_value["context"].as_str().unwrap().contains("main"));

    let asm = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["asm", "--from", anchor, "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        asm.status.success(),
        "{}",
        String::from_utf8_lossy(&asm.stderr)
    );
    let asm_value: serde_json::Value = serde_json::from_slice(&asm.stdout).unwrap();
    assert_authoritative_receipt(&asm_value, "asm");
    assert!(
        asm_value["documents"]
            .as_array()
            .is_some_and(|docs| !docs.is_empty())
    );
}

#[test]
fn strict_agent_defaults_keep_full_receipts_when_they_fit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("example.rs"),
        "/// Entry point for strict receipt QA.\nfn main() {}\n",
    )
    .unwrap();

    let ask = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["ask", "--strict", "Where is the entry point?", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        ask.status.success(),
        "{}",
        String::from_utf8_lossy(&ask.stderr)
    );
    let ask_value: serde_json::Value = serde_json::from_slice(&ask.stdout).unwrap();
    assert_authoritative_receipt(&ask_value, "strict ask");
    let anchor = ask_value["anchor"].as_str().unwrap();

    let asm = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["asm", "--strict", "--from", anchor, "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        asm.status.success(),
        "{}",
        String::from_utf8_lossy(&asm.stderr)
    );
    let asm_value: serde_json::Value = serde_json::from_slice(&asm.stdout).unwrap();
    assert_authoritative_receipt(&asm_value, "strict asm");
    assert!(
        asm_value["documents"]
            .as_array()
            .is_some_and(|docs| !docs.is_empty())
    );
}

#[test]
fn empty_default_ask_reports_truthful_building_freshness() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["ask", "definitely absent symbol", "."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["result_state"], "empty");
    assert_eq!(value["freshness"], "building");
    assert_eq!(value["index_stale"], true);
    assert_eq!(value["context_receipt"]["schema_version"], 1);
    assert_eq!(value["context_receipt"]["freshness"], "building");
    assert_eq!(value["context_receipt"]["refresh_cause"], "store_missing");
    assert!(
        value["context_receipt"]["observed_source_fingerprint"]
            .as_str()
            .is_some_and(|fingerprint| !fingerprint.is_empty())
    );
    assert!(value["context_receipt"].get("graph_revision").is_none());
}

#[test]
fn every_advertised_mcp_read_has_a_json_receipt_and_structured_recovery() {
    for tool in aden_mcp::agent_read_tools() {
        let response = aden_mcp::agent_response_for_mcp(tool, "terminal-oriented fixture output");
        let value: serde_json::Value = serde_json::from_str(&response)
            .unwrap_or_else(|e| panic!("{tool} is not agent JSON: {e}: {response}"));
        assert_eq!(value["context_receipt"]["schema_version"], 1, "{tool}");
        assert!(
            value.get("result").is_some() || value.get("items").is_some(),
            "{tool}"
        );
    }

    let error: serde_json::Value = serde_json::from_str(&aden_mcp::agent_error_for_mcp(
        "grep",
        "aden: authoritative freshness required, but refresh did not complete within 5s",
    ))
    .unwrap();
    assert_eq!(error["error"]["safe_to_retry"], true);
    assert!(
        error["error"]["recovery"]
            .as_str()
            .unwrap()
            .contains("retry")
    );
}
