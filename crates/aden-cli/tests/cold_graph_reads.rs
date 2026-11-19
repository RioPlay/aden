// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Initial generation must report pending, while published snapshots remain readable.
#![cfg(debug_assertions)]

use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.root.join(".aden-test-generation-hold"));
    }
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project with spaces");
        std::fs::create_dir_all(&root).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q", "--template="])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(root.join("lib.rs"), "pub fn recovery_symbol() {}\n").unwrap();
        Self {
            root,
            data: temp.path().join("data"),
            _temp: temp,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aden"));
        command
            .current_dir(&self.root)
            .args(args)
            .env("ADEN_DATA_DIR", &self.data)
            .env("ADEN_MCP_VERSION", env!("CARGO_PKG_VERSION"))
            .env("ADEN_MCP_MACHINE_ERRORS", "1");
        for key in [
            "ADEN_STORE",
            "ADEN_SKIP_AUTO_GEN",
            "ADEN_PROJECT",
            "ADEN_TEST_GENERATION_HOLD",
            "ADEN_TEST_GENERATION_DELAY_MS",
            "ADEN_TEST_HOLD_AFTER_FILE_WRITE",
            "ADEN_TEST_FAIL_AFTER_FILE_WRITE",
        ] {
            command.env_remove(key);
        }
        command
    }

    fn output(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.output(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn wait_for(&self, field: &str, expected: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let status = self.json(&["jobs", "status", ".", "--json"]);
            if status[field] == expected {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "expected {field}={expected}: {status}"
            );
            assert!(
                !matches!(
                    status["state"].as_str(),
                    Some("failed" | "cancelled" | "interrupted")
                ),
                "{status}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn hold_after_write(&self) -> Value {
        std::fs::write(self.root.join(".aden-test-generation-hold"), "").unwrap();
        let output = self
            .command(&["gen", ".", "--background", "--json"])
            .env("ADEN_TEST_HOLD_AFTER_FILE_WRITE", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.wait_for("phase", "test_hold_after_write")
    }

    fn release(&self) {
        std::fs::remove_file(self.root.join(".aden-test-generation-hold")).unwrap();
        self.wait_for("state", "completed");
    }
}

#[test]
fn initial_graph_reads_report_pending_then_recover_without_hiding_source() {
    let fixture = Fixture::new();
    let held = fixture.hold_after_write();
    let job_id = held["job_id"].as_str().unwrap();
    let requests: &[&[&str]] = &[
        &["locate", "--symbol", "recovery_symbol", "."],
        &["understand", "recovery_symbol", "."],
        &["query", "--from", "recovery_symbol", "."],
        &["asm", "--from", "recovery_symbol", "."],
        &["ask", "Where is recovery_symbol defined?", "."],
    ];
    // Independent read-only requests share one deliberately held fixture writer.
    std::thread::scope(|scope| {
        let tasks: Vec<_> = requests
            .iter()
            .map(|args| {
                let fixture = &fixture;
                scope.spawn(move || {
                    let started = Instant::now();
                    let output = fixture.output(args);
                    assert!(
                        started.elapsed() < Duration::from_secs(15),
                        "read exceeded its bounded wait"
                    );
                    assert!(
                        !output.status.success(),
                        "{args:?} returned success without a snapshot"
                    );
                    assert!(
                        output.stdout.is_empty(),
                        "{args:?}: stdout={} stderr={}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                    let error: Value =
                        serde_json::from_slice(&output.stderr).unwrap_or_else(|_| {
                            panic!("{args:?}: {}", String::from_utf8_lossy(&output.stderr))
                        });
                    assert_eq!(
                        error["error"]["code"], "index_generation_pending",
                        "{args:?}: {error}"
                    );
                    assert!(error["error"]["message"].as_str().unwrap().contains(job_id));
                    assert!(
                        error["error"]["recovery"]
                            .as_str()
                            .unwrap()
                            .contains("generation_status")
                    );
                })
            })
            .collect();
        for task in tasks {
            task.join().unwrap();
        }
    });
    let source = fixture.json(&["read", "lib.rs"]);
    assert_eq!(source["source"]["text"], "pub fn recovery_symbol() {}\n");
    let files = fixture.json(&["tree", "--files", "."]);
    assert!(files["outline"].as_str().unwrap().contains("lib.rs"));
    fixture.release();
    let recovered = fixture.json(&["locate", "--symbol", "recovery_symbol", "."]);
    assert_eq!(recovered["match_kind"], "definition");
    assert!(
        recovered["items"][0]["anchor"]
            .as_str()
            .unwrap()
            .ends_with("#recovery_symbol")
    );
}

#[test]
fn published_snapshot_stays_readable_while_refresh_holds_the_store() {
    let fixture = Fixture::new();
    let generated = fixture.output(&["gen", "."]);
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    std::fs::write(
        fixture.root.join("lib.rs"),
        "pub fn replacement_symbol() {}\n",
    )
    .unwrap();
    fixture.hold_after_write();
    let old = fixture.json(&["locate", "--symbol", "recovery_symbol", "."]);
    assert_eq!(old["match_kind"], "definition");
    assert_eq!(old["index_stale"], true);
    let source = fixture.json(&["read", "lib.rs"]);
    assert_eq!(source["source"]["text"], "pub fn replacement_symbol() {}\n");
    fixture.release();
    let current = fixture.json(&["locate", "--symbol", "replacement_symbol", "."]);
    assert_eq!(current["match_kind"], "definition");
    assert_eq!(current["index_stale"], false);
}
