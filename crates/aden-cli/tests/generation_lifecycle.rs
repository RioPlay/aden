// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Actual subprocess acceptance for generation lifetime and cooperative recovery.
#![cfg(debug_assertions)]
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Assertion failures also release this fixture's cooperative worker.
        let _ = std::fs::remove_file(self.root.join(".aden-test-generation-hold"));
    }
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo with spaces");
        let data = temp.path().join("data");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("lib.rs"), "pub fn durable_symbol() {}\n").unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        Self {
            _temp: temp,
            root,
            data,
        }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_aden"));
        c.args(args)
            .current_dir(&self.root)
            .env("ADEN_DATA_DIR", &self.data)
            .env_remove("ADEN_STORE")
            .env_remove("ADEN_SKIP_AUTO_GEN")
            .env_remove("ADEN_TEST_GENERATION_DELAY_MS")
            .env_remove("ADEN_TEST_GENERATION_HOLD")
            .env_remove("ADEN_TEST_HOLD_AFTER_FILE_WRITE")
            .env_remove("ADEN_TEST_MUTATE_DURING_GEN")
            .env_remove("ADEN_TEST_FAIL_AFTER_FILE_WRITE");
        c
    }
    fn bounded_output(&self, mut command: Command) -> std::process::Output {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(command.output().unwrap());
        });
        match receiver.recv_timeout(Duration::from_secs(15)) {
            Ok(output) => output,
            Err(error) => {
                // Release only this fixture's worker before reporting a capture
                // EOF regression, so the negative path cannot strand a process.
                let _ = std::fs::remove_file(self.root.join(".aden-test-generation-hold"));
                let output = receiver
                    .recv_timeout(Duration::from_secs(40))
                    .expect("caller should recover after releasing owned worker");
                panic!(
                    "caller output remained open while worker was held: {error}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }
    fn json(&self, args: &[&str]) -> Value {
        let o = self.command(args).output().unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        serde_json::from_slice(&o.stdout).unwrap()
    }
    fn start(&self, delay: &str) -> Value {
        let o = self
            .command(&["gen", ".", "--background", "--json"])
            .env("ADEN_TEST_GENERATION_DELAY_MS", delay)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice(&o.stdout).unwrap()
    }
    fn status(&self) -> Value {
        self.json(&["jobs", "status", ".", "--json"])
    }
    fn wait(&self, state: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            let s = self.status();
            if s["state"] == state {
                return s;
            }
            assert!(Instant::now() < deadline, "Expected {state}, got {s}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
#[test]
fn background_starts_coalesce_and_cancelled_job_can_recover() {
    let f = Fixture::new();
    std::fs::write(f.root.join(".aden-test-generation-hold"), b"hold").unwrap();
    let mut launch = f.command(&["gen", ".", "--background", "--json"]);
    launch.env("ADEN_TEST_GENERATION_HOLD", "1");
    let output = f.bounded_output(launch);
    assert!(output.status.success());
    let first: Value = serde_json::from_slice(&output.stdout).unwrap();
    let held = f.wait("running");
    assert_eq!(held["job_id"], first["job_id"]);
    let children: Vec<_> = (0..6)
        .map(|_| {
            f.command(&["gen", ".", "--background", "--json"])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success());
        let joined: Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(joined["job_id"], first["job_id"]);
    }
    let id = first["job_id"].as_str().unwrap();
    let wrong = f
        .command(&["jobs", "cancel", ".", "--job-id", "abcdef", "--json"])
        .output()
        .unwrap();
    assert!(!wrong.status.success());
    f.json(&["jobs", "cancel", ".", "--job-id", id, "--json"]);
    f.wait("cancelled");
    std::fs::remove_file(f.root.join(".aden-test-generation-hold")).unwrap();
    let next = f.start("0");
    assert_ne!(next["job_id"], first["job_id"]);
    f.wait("completed");
    let located = f.json(&["locate", "durable_symbol", ".", "--json"]);
    assert_eq!(located["freshness"], "current");
    assert_eq!(located["total"], 1);
}
#[test]
fn strict_read_deadline_does_not_terminate_generation() {
    let f = Fixture::new();
    std::fs::write(f.root.join(".aden-test-generation-hold"), b"hold").unwrap();
    let began = Instant::now();
    let mut read = f.command(&["locate", "durable_symbol", ".", "--json", "--require-fresh"]);
    read.env("ADEN_TEST_GENERATION_HOLD", "1");
    let o = f.bounded_output(read);
    assert!(
        !o.status.success(),
        "strict read must reject pending first build"
    );
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("within 5s"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    eprintln!("strict read total process elapsed: {:?}", began.elapsed());
    let pending = f.status();
    assert!(
        matches!(pending["state"].as_str(), Some("running" | "queued")),
        "{pending}"
    );
    std::fs::remove_file(f.root.join(".aden-test-generation-hold")).unwrap();
    f.wait("completed");
    let located = f.json(&["locate", "durable_symbol", ".", "--json", "--require-fresh"]);
    assert_eq!(located["freshness"], "current");
    assert_eq!(located["total"], 1);
}
#[test]
fn killing_only_read_caller_leaves_owned_generation_running() {
    let f = Fixture::new();
    let mut read = f
        .command(&["locate", "durable_symbol", ".", "--json", "--require-fresh"])
        .env("ADEN_TEST_GENERATION_DELAY_MS", "2500")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pending = f.wait("running");
    assert_ne!(pending["worker_pid"].as_u64(), Some(read.id() as u64));
    read.kill().unwrap();
    read.wait().unwrap();
    let done = f.wait("completed");
    assert_eq!(done["job_id"], pending["job_id"]);
    assert_eq!(done["completed_files"], done["total_files"]);
    let located = f.json(&["locate", "durable_symbol", ".", "--json"]);
    assert_eq!(located["freshness"], "current");
}
#[test]
fn source_changes_during_generation_cannot_record_completion() {
    let f = Fixture::new();
    let o = f
        .command(&["gen", ".", "--background", "--json"])
        .env("ADEN_TEST_MUTATE_DURING_GEN", "lib.rs")
        .output()
        .unwrap();
    assert!(o.status.success());
    let failed = f.wait("failed");
    assert!(
        failed["error"]
            .as_str()
            .unwrap()
            .contains("authoritative current snapshot"),
        "{failed}"
    );
    f.start("0");
    f.wait("completed");
}

#[test]
fn interrupted_file_write_then_source_revert_prunes_uncommitted_symbols() {
    let f = Fixture::new();
    assert!(
        f.command(&["gen", ".", "--quiet"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let path = f.command(&["store", "path"]).output().unwrap();
    assert!(path.status.success());
    let store = PathBuf::from(String::from_utf8(path.stdout).unwrap().trim());
    let journal = store.with_extension("generation").join("recovery.json");
    assert!(!journal.exists());
    std::fs::write(f.root.join("lib.rs"), "pub fn interrupted_symbol() {}\n").unwrap();
    let failed = f
        .command(&["gen", ".", "--quiet"])
        .env("ADEN_TEST_FAIL_AFTER_FILE_WRITE", "lib.rs")
        .output()
        .unwrap();
    assert!(
        !failed.status.success(),
        "injected failure must reject partial generation"
    );
    assert!(journal.is_file());
    let intent: Value = serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
    assert!(
        intent["files"]["lib.rs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().ends_with("#interrupted_symbol"))
    );
    std::fs::write(f.root.join("lib.rs"), "pub fn durable_symbol() {}\n").unwrap();
    let recovered = f.command(&["gen", ".", "--quiet"]).output().unwrap();
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    assert!(
        !journal.exists(),
        "successful publication must clear recovery intent"
    );
    let original = f.json(&["locate", "durable_symbol", ".", "--json"]);
    assert_eq!(original["freshness"], "current");
    assert_eq!(original["total"], 1);
    let orphan = f.json(&["locate", "interrupted_symbol", ".", "--json"]);
    assert_eq!(
        orphan["total"], 0,
        "interrupted new symbol must be pruned after source reversion"
    );
}

#[test]
fn sync_caller_termination_preserves_internal_generation_worker() {
    let f = Fixture::new();
    std::fs::write(f.root.join(".aden-test-generation-hold"), b"hold").unwrap();
    let mut caller = f
        .command(&["sync", "."])
        .env("ADEN_TEST_GENERATION_HOLD", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let held = f.wait("running");
    assert_ne!(held["worker_pid"].as_u64(), Some(caller.id() as u64));
    caller.kill().unwrap();
    caller.wait().unwrap();
    std::fs::remove_file(f.root.join(".aden-test-generation-hold")).unwrap();
    let done = f.wait("completed");
    assert_eq!(done["job_id"], held["job_id"]);
    let located = f.json(&["locate", "durable_symbol", ".", "--json"]);
    assert_eq!(located["freshness"], "current");
    assert_eq!(located["total"], 1);
}
