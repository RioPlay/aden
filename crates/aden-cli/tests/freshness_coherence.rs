// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Generation reuse must describe the bytes parsed, not filesystem timestamps.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use serde_json::Value;

const BEFORE: &str = "pub fn before_fn() -> u32 { 1 }\n";
const AFTER: &str = "pub fn afterr_fn() -> u32 { 2 }\n";

struct Fixture {
    _temp: tempfile::TempDir,
    project: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("fixture directory");
        let project = temp.path().join("repo");
        let data = temp.path().join("data");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("lib.rs"), BEFORE).unwrap();
        let git = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&project)
            .output()
            .expect("git must be available");
        assert!(
            git.status.success(),
            "{}",
            String::from_utf8_lossy(&git.stderr)
        );
        Self {
            _temp: temp,
            project,
            data,
        }
    }

    fn source(&self) -> PathBuf {
        self.project.join("lib.rs")
    }

    fn run(&self, args: &[&str]) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_aden"))
            .args(args)
            .current_dir(&self.project)
            .env("ADEN_DATA_DIR", &self.data)
            .env_remove("ADEN_STORE")
            .env_remove("ADEN_SKIP_AUTO_GEN")
            .output()
            .expect("aden must run");
        assert!(
            output.status.success(),
            "aden {args:?} failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "aden {args:?} returned invalid JSON: {error}: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn generate(&self) -> String {
        String::from_utf8(self.run(&["gen", ".", "--quiet"]).stdout).unwrap()
    }

    fn locate(&self, symbol: &str) -> Value {
        self.json(&["locate", symbol, ".", "--json"])
    }

    fn cache_path(&self) -> PathBuf {
        let output = self.run(&["store", "path"]);
        let store = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        store
            .parent()
            .expect("store project directory")
            .join("gen-cache.json")
    }
}

fn set_modified(path: &Path, modified: SystemTime) {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    assert_eq!(
        std::fs::metadata(path).unwrap().modified().unwrap(),
        modified
    );
}

fn assert_definition(result: &Value, name: &str) {
    assert_eq!(result["match_kind"], "definition", "{result:#}");
    assert_eq!(result["total"], 1, "{result:#}");
    assert!(
        result["items"][0]["anchor"]
            .as_str()
            .is_some_and(|anchor| anchor.ends_with(&format!("#{name}"))),
        "{result:#}"
    );
    assert_eq!(result["freshness"], "current", "{result:#}");
}

#[test]
fn preserved_mtime_equal_size_rewrite_refreshes_actual_symbols() {
    let fixture = Fixture::new();
    fixture.generate();
    let before = fixture.locate("before_fn");
    assert_definition(&before, "before_fn");
    let modified = std::fs::metadata(fixture.source())
        .unwrap()
        .modified()
        .unwrap();

    assert_eq!(BEFORE.len(), AFTER.len());
    std::fs::write(fixture.source(), AFTER).unwrap();
    set_modified(&fixture.source(), modified);

    // Exercise automatic read-time refresh, not explicit generation or grep's
    // direct text search: the stored symbol and lexicon must actually change.
    let after = fixture.locate("afterr_fn");
    assert_definition(&after, "afterr_fn");
    assert_eq!(after["context_receipt"]["refresh_cause"], "source_changed");
    assert_ne!(
        after["context_receipt"]["graph_revision"],
        before["context_receipt"]["graph_revision"]
    );
    let deleted = fixture.locate("before_fn");
    assert_eq!(deleted["total"], 0, "deleted symbol survived: {deleted:#}");
    assert_eq!(deleted["freshness"], "current");

    let warm = fixture.locate("afterr_fn");
    assert_definition(&warm, "afterr_fn");
    assert_eq!(warm["context_receipt"]["refresh_cause"], "none");
    assert_eq!(
        warm["context_receipt"]["graph_revision"],
        after["context_receipt"]["graph_revision"]
    );
}

#[test]
fn unchanged_bytes_reuse_cache_even_after_timestamp_change() {
    let fixture = Fixture::new();
    fixture.generate();
    let modified = std::fs::metadata(fixture.source())
        .unwrap()
        .modified()
        .unwrap();
    set_modified(&fixture.source(), modified + Duration::from_secs(5));

    let summary = fixture.generate();
    assert!(
        summary.contains("Stored 0 contracts. Skipped 1 unchanged files."),
        "{summary}"
    );
    assert_definition(&fixture.locate("before_fn"), "before_fn");
}

#[test]
fn cache_entries_without_content_digest_are_reparsed() {
    let fixture = Fixture::new();
    fixture.generate();
    let cache_path = fixture.cache_path();
    let mut cache: Value = serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    let entries = cache["entries"]
        .as_object_mut()
        .expect("generation entries");
    assert_eq!(entries.len(), 1);
    for entry in entries.values_mut() {
        entry.as_object_mut().unwrap().remove("source_digest");
    }
    std::fs::write(&cache_path, serde_json::to_vec(&cache).unwrap()).unwrap();

    let summary = fixture.generate();
    assert!(
        summary.contains("Stored 1 contracts. Skipped 0 unchanged files."),
        "{summary}"
    );
    let reparsed: Value = serde_json::from_slice(&std::fs::read(cache_path).unwrap()).unwrap();
    assert!(
        reparsed["entries"]
            .as_object()
            .unwrap()
            .values()
            .all(|entry| {
                entry["source_digest"]
                    .as_str()
                    .is_some_and(|digest| !digest.is_empty())
            })
    );
}
