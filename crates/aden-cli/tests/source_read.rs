// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::Value;
use std::process::{Command, Output};

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='source-fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("source.txt"), bytes).unwrap();
        Self { dir }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_aden"))
            .arg("read")
            .args(args)
            .current_dir(self.dir.path())
            .env("ADEN_DATA_DIR", self.dir.path().join("data"))
            .env("ADEN_MCP_MACHINE_ERRORS", "1")
            .env_remove("ADEN_STORE")
            .env("ADEN_MCP_VERSION", env!("CARGO_PKG_VERSION"))
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn error(&self, args: &[&str], code: &str) {
        let out = self.run(args);
        assert!(
            !out.status.success(),
            "unexpected success: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let value: Value = serde_json::from_slice(&out.stderr).unwrap();
        assert_eq!(value["error"]["code"], code, "{value}");
    }
}

#[test]
fn reads_requested_literal_lines_deterministically_without_creating_an_index() {
    let f = Fixture::new(b"one\r\ntwo\r\nthree\r\n");
    let args = ["source.txt", "--start-line", "2", "--end-line", "2"];
    let first = f.run(&args);
    let second = f.run(&args);
    assert!(first.status.success());
    assert_eq!(first.stdout, second.stdout);
    let value: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(value["source"]["text"], "two\r\n");
    assert_eq!(value["source"]["start_byte"], 5);
    assert_eq!(value["source"]["end_byte"], 10);
    assert_eq!(
        value["source"]["source_hash"],
        aden_core::stable_hash(b"one\r\ntwo\r\nthree\r\n")
    );
    assert_eq!(value["observation"], "file_bytes_read");
    assert!(value.get("freshness").is_none());
    assert!(value.get("context_receipt").is_none());
    assert!(!f.dir.path().join("data").exists());
    assert!(!f.dir.path().join(".aden").exists());
}

#[test]
fn expected_hash_rejects_changed_source_even_with_preserved_timestamp() {
    let f = Fixture::new(b"before\n");
    let first = f.json(&["source.txt"]);
    let hash = first["source"]["source_hash"].as_str().unwrap();
    assert_eq!(
        f.json(&["source.txt", "--expected-hash", hash])["source"]["expected_hash_matched"],
        true
    );
    let path = f.dir.path().join("source.txt");
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(&path, b"after!\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    f.error(&["source.txt", "--expected-hash", hash], "source_changed");
}

#[test]
fn byte_limit_preserves_utf8_boundaries_and_reports_omission() {
    let f = Fixture::new("αβγ\nnext\n".as_bytes());
    let value = f.json(&["source.txt", "--max-bytes", "3"]);
    assert_eq!(value["source"]["text"], "α");
    assert_eq!(value["source"]["returned_bytes"], 2);
    assert_eq!(value["source"]["last_line_complete"], false);
    assert_eq!(value["truncated"], true);
    assert_eq!(value["source"]["total_lines"], 2);
}

#[test]
fn tiny_byte_limits_reject_nonadvancing_utf8_continuations() {
    for content in ["αbeta\n", "🦀beta\n"] {
        let f = Fixture::new(content.as_bytes());
        f.error(&["source.txt", "--max-bytes", "1"], "invalid_read_range");
        let value = f.json(&["source.txt", "--max-bytes", "4"]);
        assert!(value["next_byte"].as_u64().unwrap() > 0);
    }
    let f = Fixture::new("a🦀\n".as_bytes());
    let first = f.json(&["source.txt", "--max-bytes", "1"]);
    assert_eq!(first["next_byte"], 1);
    f.error(
        &[
            "source.txt",
            "--max-bytes",
            "1",
            "--start-byte",
            "1",
            "--expected-hash",
            first["source"]["source_hash"].as_str().unwrap(),
        ],
        "invalid_read_range",
    );
}

#[test]
fn long_lines_can_be_completed_with_hash_pinned_byte_continuation() {
    let content = format!("{}tail\nnext\n", "α".repeat(5000));
    let f = Fixture::new(content.as_bytes());
    let first = f.json(&["source.txt"]);
    let offset = first["next_byte"].as_u64().unwrap().to_string();
    let hash = first["source"]["source_hash"].as_str().unwrap();
    let second = f.json(&[
        "source.txt",
        "--start-byte",
        &offset,
        "--expected-hash",
        hash,
    ]);
    assert_eq!(
        format!(
            "{}{}",
            first["source"]["text"].as_str().unwrap(),
            second["source"]["text"].as_str().unwrap()
        ),
        content
    );
    assert_eq!(second["truncated"], false);
    assert_eq!(second["source"]["start_byte"], 8192);
    f.error(
        &["source.txt", "--start-byte", "1", "--expected-hash", hash],
        "invalid_source_cursor",
    );
    std::fs::write(f.dir.path().join("source.txt"), "changed\n").unwrap();
    f.error(
        &[
            "source.txt",
            "--start-byte",
            &offset,
            "--expected-hash",
            hash,
        ],
        "source_changed",
    );
}

#[test]
fn rejects_invalid_ranges_missing_files_and_binary_content() {
    let f = Fixture::new(b"one\n");
    f.error(&["source.txt", "--start-line", "0"], "invalid_read_range");
    f.error(
        &["source.txt", "--start-line", "2", "--end-line", "1"],
        "invalid_read_range",
    );
    f.error(&["source.txt", "--max-bytes", "8193"], "invalid_read_range");
    f.error(
        &["source.txt", "--start-line", "2"],
        "source_range_unavailable",
    );
    f.error(&["missing.txt"], "source_unavailable");
    std::fs::write(f.dir.path().join("source.txt"), b"binary\0").unwrap();
    f.error(&["source.txt"], "source_encoding");
    std::fs::write(f.dir.path().join("source.txt"), [0xff]).unwrap();
    f.error(&["source.txt"], "source_encoding");
}

#[test]
fn respects_project_exclusions_and_refuses_path_escape() {
    let f = Fixture::new(b"secret\n");
    f.error(&["../outside.txt"], "source_boundary");
    let absolute = f.dir.path().join("source.txt");
    f.error(&[absolute.to_str().unwrap()], "source_boundary");
    std::fs::write(f.dir.path().join(".adenignore"), "source.txt\n").unwrap();
    f.error(&["source.txt"], "source_excluded");
}

#[test]
fn nested_directory_is_the_exact_read_boundary_even_inside_a_git_repository() {
    let f = Fixture::new(b"parent-only\n");
    // A .git marker is sufficient to expose an accidental root-expansion path.
    std::fs::create_dir(f.dir.path().join(".git")).unwrap();
    std::fs::create_dir(f.dir.path().join("nested")).unwrap();
    std::fs::write(f.dir.path().join("nested/local.txt"), "nested\n").unwrap();
    let value = f.json(&["local.txt", "nested"]);
    assert_eq!(value["source"]["text"], "nested\n");
    f.error(&["source.txt", "nested"], "source_unavailable");
    f.error(&["../source.txt", "nested"], "source_boundary");
}

#[test]
fn nested_read_cannot_bypass_enclosing_project_exclusions() {
    let f = Fixture::new(b"root\n");
    std::fs::create_dir(f.dir.path().join("nested")).unwrap();
    std::fs::write(f.dir.path().join("nested/secret.txt"), "excluded\n").unwrap();
    std::fs::write(f.dir.path().join(".adenignore"), "nested/secret.txt\n").unwrap();
    f.error(&["nested/secret.txt"], "source_excluded");
    f.error(&["secret.txt", "nested"], "source_excluded");
}

#[test]
fn byte_continuation_preserves_the_original_end_line() {
    let content = format!("{}\nOUTSIDE_REQUEST\n", "x".repeat(9000));
    let f = Fixture::new(content.as_bytes());
    let first = f.json(&["source.txt", "--end-line", "1"]);
    let next = &first["continuation"];
    assert_eq!(next["end_line"], 1);
    let offset = next["start_byte"].as_u64().unwrap().to_string();
    let end = next["end_line"].as_u64().unwrap().to_string();
    let second = f.json(&[
        "source.txt",
        "--start-byte",
        &offset,
        "--end-line",
        &end,
        "--expected-hash",
        next["expected_hash"].as_str().unwrap(),
    ]);
    let joined = format!(
        "{}{}",
        first["source"]["text"].as_str().unwrap(),
        second["source"]["text"].as_str().unwrap()
    );
    assert_eq!(joined, format!("{}\n", "x".repeat(9000)));
    assert!(!joined.contains("OUTSIDE_REQUEST"));
    assert_eq!(second["truncated"], false);
}

#[test]
fn empty_file_is_complete_and_large_files_fail_boundedly() {
    let f = Fixture::new(b"");
    let value = f.json(&["source.txt"]);
    assert_eq!(value["source"]["text"], "");
    assert_eq!(value["source"]["total_lines"], 0);
    assert_eq!(value["truncated"], false);
    let file = std::fs::File::options()
        .write(true)
        .open(f.dir.path().join("source.txt"))
        .unwrap();
    file.set_len(16 * 1024 * 1024 + 1).unwrap();
    f.error(&["source.txt"], "source_size_limit");
}

#[test]
fn human_output_and_mcp_bridge_preserve_the_same_literal_excerpt() {
    let f = Fixture::new(b"first\nsecond\n");
    let out = f.run(&["source.txt", "--human", "--start-line", "2"]);
    assert!(out.status.success());
    assert_eq!(out.stdout, b"second\n");
    let raw = f.run(&["source.txt"]);
    let cli = String::from_utf8(raw.stdout).unwrap();
    let bridged = aden_mcp::preserve_cli_output_for_mcp("read", &cli);
    assert_eq!(
        serde_json::from_str::<Value>(&cli).unwrap(),
        serde_json::from_str::<Value>(&bridged).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn refuses_symlink_escape() {
    let f = Fixture::new(b"inside\n");
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("external.txt");
    std::fs::write(&target, "outside\n").unwrap();
    std::os::unix::fs::symlink(target, f.dir.path().join("link.txt")).unwrap();
    f.error(&["link.txt"], "source_boundary");
}
