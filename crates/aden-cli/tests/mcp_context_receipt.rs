// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! CLI-to-MCP Context Receipt contract fixture.

use std::process::Command;

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
}
