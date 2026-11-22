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
        let commands: Vec<&str> = hello["commands"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        for expected in [
            "locate",
            "understand",
            "query",
            "read",
            "grep",
            "tree",
            "search",
            "asm",
        ] {
            assert!(
                commands.contains(&expected),
                "missing {expected} in {hello}"
            );
        }
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
        let body = self.machine_body(args);
        assert_eq!(body["context_receipt"]["freshness"], "current", "{body}");
        body
    }

    /// A successful JSON body, without assuming the command carries a graph
    /// freshness receipt (`read` deliberately makes no graph claim).
    fn machine_body(&mut self, args: &[&str]) -> Value {
        let response = self.request(args);
        assert_eq!(response["ok"], true, "{response}");
        serde_json::from_str(response["stdout"].as_str().unwrap()).unwrap()
    }

    fn assert_rejected(&mut self, args: &[&str]) {
        let response = self.request(args);
        assert_eq!(response["ok"], false, "{args:?}: {response}");
        assert!(
            response["stderr"]
                .as_str()
                .unwrap()
                .contains("resident worker accepts"),
            "{args:?}: {response}"
        );
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
fn resident_source_and_index_reads_match_cli_bodies_on_one_process() {
    let fixture = Fixture::new();
    fixture.cli(&["gen", ".", "--quiet"]);
    let mut worker = Worker::new(&fixture);
    let pid = worker.pid;
    for args in [
        vec![
            "read",
            "lib.rs",
            ".",
            "--start-line",
            "1",
            "--end-line",
            "2",
        ],
        vec!["grep", "leaf", "."],
        vec!["tree", "."],
        vec!["tree", ".", "--files"],
        vec!["search", "leaf", "."],
        // Repeat to exercise the retained index on an unchanged cache.
        vec!["search", "leaf", "."],
        vec!["asm", "--from", "leaf", "--strict", "."],
    ] {
        let expected: Value = serde_json::from_str(&fixture.cli(&args)).unwrap();
        let actual = worker.machine_body(&args);
        assert_eq!(actual, expected, "{args:?}");
        assert_eq!(worker.pid, pid);
    }
}

#[test]
fn resident_rejects_non_json_inspect_and_file_asm() {
    let fixture = Fixture::new();
    fixture.cli(&["gen", ".", "--quiet"]);
    let mut worker = Worker::new(&fixture);
    worker.assert_rejected(&["asm", "--from", "leaf", "--format", "llm", "."]);
    worker.assert_rejected(&["asm", "--from", "leaf", "--inspect", "."]);
    worker.assert_rejected(&["asm", "--from", "leaf", "-o", "out.txt", "."]);
    assert!(!fixture.root.path().join("out.txt").exists());
    // The worker stays usable after each rejection.
    let body = worker.machine_body(&["asm", "--from", "leaf", "--strict", "."]);
    assert!(body.is_object(), "{body}");
}

#[test]
fn resident_search_reflects_regenerated_index() {
    let fixture = Fixture::new();
    fixture.cli(&["gen", ".", "--quiet"]);
    let mut worker = Worker::new(&fixture);
    let anchors = |body: &Value| -> Vec<String> {
        body["results"]
            .as_array()
            .unwrap_or_else(|| panic!("search body has no result list: {body}"))
            .iter()
            .filter_map(|item| item["anchor"].as_str().map(str::to_string))
            .collect()
    };
    let before = worker.machine_body(&["search", "sprocket", "."]);
    assert!(
        !anchors(&before)
            .iter()
            .any(|anchor| anchor.ends_with("#sprocket_widget")),
        "{before}"
    );
    std::fs::write(
        fixture.root.path().join("lib.rs"),
        "pub fn entry() { leaf(); }\npub fn leaf() {}\npub fn sprocket_widget() {}\n",
    )
    .unwrap();
    // A separate process regenerates the graph and replaces the index cache
    // while the worker still retains the previous decoded index.
    fixture.cli(&["gen", ".", "--quiet"]);
    let after = worker.machine_body(&["search", "sprocket", "."]);
    assert!(
        anchors(&after)
            .iter()
            .any(|anchor| anchor.ends_with("#sprocket_widget")),
        "{after}"
    );
    let expected: Value = serde_json::from_str(&fixture.cli(&["search", "sprocket", "."])).unwrap();
    assert_eq!(after, expected);
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
