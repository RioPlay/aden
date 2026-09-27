// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::process::{Command, Output};

fn aden(root: &std::path::Path, data: &std::path::Path, args: &[&str]) -> Output {
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
fn search_cursor_is_revision_and_query_bound_with_typed_continuation() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("search.rs"),
        "fn cursor_needle_one() {}\nfn cursor_needle_two() {}\nfn cursor_needle_three() {}\n",
    )
    .unwrap();
    aden(root.path(), data.path(), &["gen", "."]);

    let first = aden(
        root.path(),
        data.path(),
        &["search", "cursor", ".", "--limit", "1"],
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["returned"], 1);
    assert_eq!(first["truncated"], true);
    let cursor = first["next_cursor"].as_str().unwrap();
    assert!(cursor.starts_with("v1:"));
    assert_eq!(first["next_actions"][0]["tool"], "search");
    assert_eq!(first["next_actions"][0]["arguments"]["cursor"], cursor);

    let second = aden(
        root.path(),
        data.path(),
        &["search", "cursor", ".", "--limit", "1", "--cursor", cursor],
    );
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second["offset"], 1);
    assert_ne!(
        first["results"][0]["anchor"],
        second["results"][0]["anchor"]
    );

    let wrong_query = aden(
        root.path(),
        data.path(),
        &[
            "search",
            "different",
            ".",
            "--limit",
            "1",
            "--cursor",
            cursor,
        ],
    );
    let wrong_query: serde_json::Value = serde_json::from_slice(&wrong_query.stdout).unwrap();
    assert_eq!(wrong_query["error"]["code"], "cursor_stale");
    assert_eq!(wrong_query["next_actions"][0]["tool"], "search");
    assert!(
        wrong_query["next_actions"][0]["arguments"]
            .get("cursor")
            .is_none()
    );
}
