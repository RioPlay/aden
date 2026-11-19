// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::Value;
use std::process::{Command, Output};

fn run(root: &std::path::Path, data: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(args)
        .current_dir(root)
        .env("ADEN_DATA_DIR", data)
        .output()
        .unwrap()
}

fn json(root: &std::path::Path, data: &std::path::Path, args: &[&str]) -> Value {
    let out = run(root, data, args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn understand_continuation_reads_exact_remaining_verified_crlf_utf8_source() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let body = format!(
        "{}    let greeting = \"日本語🙂\";\r\n}}\r\n",
        "    // long body\r\n".repeat(80)
    );
    let literal = format!("fn continued() {{\r\n{body}");
    std::fs::write(root.path().join("source.rs"), &literal).unwrap();
    assert!(
        run(root.path(), data.path(), &["gen", "."])
            .status
            .success()
    );

    let first = json(
        root.path(),
        data.path(),
        &["understand", "continued", ".", "--budget", "128"],
    );
    let source = &first["source"];
    assert_eq!(source["state"], "clipped");
    assert_eq!(source["hash_verified"], true);
    let continuation = &source["continuation"];
    let file = continuation["file"].as_str().unwrap();
    let mut reconstructed = source["text"].as_str().unwrap().to_owned();
    let mut cursor = continuation["start_byte"].as_u64().unwrap();
    let end_line = continuation["end_line"].as_u64().unwrap().to_string();
    let hash = continuation["expected_hash"].as_str().unwrap();
    let max_bytes = continuation["max_bytes"].as_u64().unwrap().to_string();
    assert!(max_bytes.parse::<u64>().unwrap() <= 8192);
    let mut chunks = 0;
    loop {
        chunks += 1;
        assert!(chunks <= 16, "continuation did not reach the symbol end");
        let cursor_arg = cursor.to_string();
        let chunk = json(
            root.path(),
            data.path(),
            &[
                "read",
                file,
                "--start-byte",
                &cursor_arg,
                "--end-line",
                &end_line,
                "--expected-hash",
                hash,
                "--max-bytes",
                &max_bytes,
            ],
        );
        reconstructed.push_str(chunk["source"]["text"].as_str().unwrap());
        let Some(next) = chunk["continuation"].as_object() else {
            break;
        };
        cursor = next["start_byte"].as_u64().unwrap();
        assert_eq!(next["expected_hash"], hash);
    }
    assert!(chunks > 1, "fixture should require multiple bounded reads");
    assert_eq!(reconstructed, literal);

    let cursor_arg = continuation["start_byte"].as_u64().unwrap().to_string();
    std::fs::write(root.path().join(file), format!("{literal}// changed\r\n")).unwrap();
    let changed = run(
        root.path(),
        data.path(),
        &[
            "read",
            file,
            "--start-byte",
            &cursor_arg,
            "--end-line",
            &end_line,
            "--expected-hash",
            hash,
            "--max-bytes",
            &max_bytes,
        ],
    );
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("expected_hash"));
}

#[test]
fn complete_or_unavailable_understand_source_has_no_continuation() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("source.rs"), "fn short() {}\r\n").unwrap();
    assert!(
        run(root.path(), data.path(), &["gen", "."])
            .status
            .success()
    );
    let complete = json(root.path(), data.path(), &["understand", "short", "."]);
    assert_eq!(complete["source"]["state"], "complete");
    assert!(
        complete["source"]
            .get("continuation")
            .is_none_or(Value::is_null)
    );

    std::fs::remove_file(root.path().join("source.rs")).unwrap();
    let unavailable = json(root.path(), data.path(), &["understand", "short", "."]);
    assert_eq!(unavailable["source"]["state"], "unavailable");
    assert!(unavailable["source"].get("continuation").is_none());
}
