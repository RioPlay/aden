// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::process::Command;

fn aden(root: &std::path::Path, data: &std::path::Path, args: &[&str]) -> std::process::Output {
    let output = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(args)
        .current_dir(root)
        .env("ADEN_DATA_DIR", data)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn understand_includes_verified_source_and_unambiguous_relationship_directions() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("graph.rs"),
        "fn caller() { target(); }\nfn target() {\n    dependency();\n}\nfn dependency() {}\n",
    )
    .unwrap();
    aden(root.path(), data.path(), &["gen", "."]);
    let output = aden(root.path(), data.path(), &["understand", "target", "."]);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["source"]["state"], "complete");
    assert_eq!(
        report["source"]["text"],
        "fn target() {\n    dependency();\n}\n"
    );
    assert_eq!(report["source"]["start_line"], 2);
    assert_eq!(report["source"]["end_line"], 4);
    assert_eq!(report["context_receipt"]["schema_version"], 1);
    for omitted in [
        "symbol",
        "freshness",
        "index_stale",
        "alternates",
        "next_actions",
        "content_budget",
        "relationships",
    ] {
        assert!(
            report.get(omitted).is_none(),
            "unexpected {omitted}: {report}"
        );
    }
    for duplicate in ["anchor", "file", "start_line", "end_line"] {
        assert!(report["definition"].get(duplicate).is_none());
    }
    assert!(
        report["backlinks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|node| node.get("inferred").is_none())
    );
    assert!(report["source"].get("indexed_start_line").is_none());
    assert!(report["source"].get("indexed_end_line").is_none());
    assert!(report["source"].get("last_line_complete").is_none());

    let verbose = aden(
        root.path(),
        data.path(),
        &["--verbose", "understand", "target", "."],
    );
    let verbose: serde_json::Value = serde_json::from_slice(&verbose.stdout).unwrap();
    assert_eq!(verbose["freshness"], "current");
    assert_eq!(verbose["index_stale"], false);
    assert_eq!(verbose["context_receipt"]["freshness"], "current");
    assert_eq!(
        verbose["relationships"]["backlinks"]["direction"],
        "incoming"
    );
    assert_eq!(verbose["relationships"]["impact"]["direction"], "outgoing");
    assert!(verbose["content_budget"].is_object());
    assert!(verbose["alternates"].is_array());
    assert!(verbose["next_actions"].is_array());
    assert!(
        report["backlinks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["anchor"].as_str().unwrap().ends_with("#caller"))
    );
    assert!(
        report["impact"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["anchor"].as_str().unwrap().ends_with("#dependency"))
    );
    assert!(
        !report["context"]
            .as_str()
            .unwrap()
            .contains("fn target() {\n    dependency();\n}")
    );
    let human = aden(
        root.path(),
        data.path(),
        &["--human", "understand", "target", "."],
    );
    let human = String::from_utf8_lossy(&human.stdout);
    assert!(human.contains("Used by / potentially affected (incoming"));
    assert!(human.contains("Depends on (outgoing"));
    assert!(human.contains("Source (complete)"));
}

#[test]
fn understand_source_and_graph_share_content_budget_and_offer_scoped_continuation() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let body = "    // 日本語🙂 bounded source\n".repeat(200);
    std::fs::write(
        root.path().join("long.rs"),
        format!("fn long_body() {{\n{body}}}\n"),
    )
    .unwrap();
    aden(root.path(), data.path(), &["gen", "."]);
    let output = aden(
        root.path(),
        data.path(),
        &["understand", "long_body", ".", "--budget", "128"],
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["source"]["state"], "clipped");
    let source = report["source"]["text"].as_str().unwrap();
    let context = report["context"].as_str().unwrap();
    assert!(source.len().div_ceil(4) + context.len().div_ceil(4) <= 128);
    assert!(
        report["source"]["end_line"].as_u64().unwrap()
            < report["source"]["indexed_end_line"].as_u64().unwrap()
    );
    let action = &report["next_actions"][0];
    assert_eq!(action["tool"], "grep");
    assert!(
        action["arguments"]["path"]
            .as_str()
            .unwrap()
            .ends_with("long.rs")
    );
}

#[test]
fn understand_sanitizes_source_only_when_rendering_for_a_terminal() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let content =
        "fn terminal_source() {\n    // \u{1b}[31mred\u{1b}[0m \u{1b}]52;c;clipboard\u{7}\n}\n";
    std::fs::write(root.path().join("source.rs"), content).unwrap();
    aden(root.path(), data.path(), &["gen", "."]);
    let json = aden(
        root.path(),
        data.path(),
        &["understand", "terminal_source", "."],
    );
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(report["source"]["text"], content);
    let human = aden(
        root.path(),
        data.path(),
        &["--human", "understand", "terminal_source", "."],
    );
    let human = String::from_utf8_lossy(&human.stdout);
    assert!(
        human.contains(r"// \x1b[31mred\x1b[0m \x1b]52;c;clipboard\x07"),
        "source controls should remain visible as printable escapes: {human}"
    );
    assert!(
        !human.contains('\u{1b}'),
        "terminal escape leaked: {human:?}"
    );
    assert!(!human.contains('\u{7}'), "terminal bell leaked: {human:?}");
}
