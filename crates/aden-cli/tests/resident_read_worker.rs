// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Fixture {
    root: tempfile::TempDir,
    data: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            root: tempfile::tempdir().unwrap(),
            data: tempfile::tempdir().unwrap(),
        };
        std::fs::write(
            fixture.root.path().join("Cargo.toml"),
            "[package]\nname='resident-fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(
            fixture.root.path().join("lib.rs"),
            "pub fn entry() { leaf(); }\npub fn leaf() {}\n",
        )
        .unwrap();
        fixture
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aden"));
        command
            .current_dir(self.root.path())
            .env("ADEN_DATA_DIR", self.data.path())
            .env("ADEN_MCP_MACHINE_ERRORS", "1")
            .env("ADEN_MCP_VERSION", env!("CARGO_PKG_VERSION"))
            .env_remove("ADEN_STORE")
            .env_remove("ADEN_SKIP_AUTO_GEN");
        command
    }

    fn cli(&self, args: &[&str]) -> String {
        let output = self.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .trim_end()
            .to_string()
    }
}

struct Worker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    pid: u64,
}

impl Worker {
    fn new(fixture: &Fixture) -> Self {
        let mut child = fixture
            .command()
            .arg("__read-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let hello: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(hello["protocol"], "aden-read-worker-v1");
        let pid = hello["pid"].as_u64().unwrap();
        assert_eq!(pid, u64::from(child.id()));
        Self {
            child,
            input,
            output,
            pid,
        }
    }

    fn request(&mut self, args: &[&str]) -> Value {
        writeln!(self.input, "{}", json!({"args":args})).unwrap();
        self.input.flush().unwrap();
        let mut line = String::new();
        assert!(
            self.output.read_line(&mut line).unwrap() > 0,
            "worker exited"
        );
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["pid"], self.pid);
        assert_eq!(response["protocol"], "aden-read-worker-v1");
        response
    }

    fn body(&mut self, args: &[&str]) -> Value {
        let response = self.request(args);
        assert_eq!(response["ok"], true, "{response}");
        let body: Value = serde_json::from_str(response["stdout"].as_str().unwrap()).unwrap();
        assert_eq!(body["context_receipt"]["freshness"], "current", "{body}");
        body
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn resident_sequence_reuses_process_and_preserves_cli_bodies() {
    let fixture = Fixture::new();
    fixture.cli(&["gen", ".", "--quiet"]);
    let mut worker = Worker::new(&fixture);
    for args in [
        vec!["locate", "--symbol", "leaf", "--json", "."],
        vec!["understand", "leaf", "--json", "."],
        vec!["query", "--from", "entry", "--depth", "1", "--json", "."],
        vec!["locate", "--caller-of", "leaf", "--json", "."],
    ] {
        let expected: Value = serde_json::from_str(&fixture.cli(&args)).unwrap();
        let actual = worker.body(&args);
        assert_eq!(actual, expected, "{args:?}");
    }
}

#[test]
fn resident_recovers_after_rejected_command_and_resolution_error() {
    let fixture = Fixture::new();
    let mut worker = Worker::new(&fixture);
    let forbidden = worker.request(&["gen", "."]);
    assert_eq!(forbidden["ok"], false);
    assert!(!fixture.data.path().join("projects").exists());
    let missing = worker.request(&["query", "--from", "missing_symbol", "."]);
    assert_eq!(missing["ok"], false, "{missing}");
    assert!(
        missing["stderr"]
            .as_str()
            .unwrap()
            .contains("anchor_not_found"),
        "{missing}"
    );
    let found = worker.body(&["locate", "--symbol", "leaf", "."]);
    assert_eq!(found["match_kind"], "definition");
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
}

#[test]
fn resident_refreshes_after_equal_length_preserved_timestamp_edit() {
    let fixture = Fixture::new();
    // The first graph may legitimately still be building. This case exercises
    // the transition from an existing publication to an equal-length edit.
    fixture.cli(&["gen", ".", "--quiet"]);
    let mut worker = Worker::new(&fixture);
    let initial = worker.body(&["understand", "leaf", "."]);
    let old_revision = initial["context_receipt"]["graph_revision"].clone();
    let source = fixture.root.path().join("lib.rs");
    let metadata = source.metadata().unwrap();
    let times = std::fs::FileTimes::new().set_modified(metadata.modified().unwrap());
    std::fs::write(&source, "pub fn entry() { neww(); }\npub fn neww() {}\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_times(times)
        .unwrap();
    let updated = worker.body(&["understand", "neww", "."]);
    assert_ne!(updated["context_receipt"]["graph_revision"], old_revision);
    assert!(
        updated["source"]["text"]
            .as_str()
            .unwrap()
            .contains("fn neww")
    );
    assert_eq!(updated["source"]["hash_verified"], true);
    let missing = worker.body(&["locate", "--symbol", "leaf", "."]);
    assert_ne!(missing["match_kind"], "definition");
    assert_eq!(missing["items"].as_array().unwrap().len(), 0);
    let query = worker.body(&["query", "--from", "entry", "--depth", "1", "."]);
    let items = query["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|item| item["anchor"].as_str().unwrap().ends_with("#neww"))
    );
    assert!(
        !items
            .iter()
            .any(|item| item["anchor"].as_str().unwrap().ends_with("#leaf"))
    );
}
