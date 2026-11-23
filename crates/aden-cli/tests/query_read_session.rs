// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::Value;
use std::collections::BTreeSet;
use std::process::{Command, Output};

const CHAIN: &str = "pub fn leaf() {}\npub fn middle() { leaf(); }\npub fn entry() { middle(); }\n";

struct Fixture {
    root: tempfile::TempDir,
    data: tempfile::TempDir,
}

impl Fixture {
    fn new(source: &str) -> Self {
        Self::with_sources(&[("lib.rs", source)])
    }

    fn with_sources(sources: &[(&str, &str)]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname='query-session'\nversion='0.1.0'\n",
        )
        .unwrap();
        for (path, source) in sources {
            std::fs::write(root.path().join(path), source).unwrap();
        }
        let fixture = Self { root, data };
        let generated = fixture.run(&["gen", ".", "--quiet"]);
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        fixture
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aden"));
        command
            .current_dir(self.root.path())
            .env("ADEN_DATA_DIR", self.data.path())
            .env_remove("ADEN_STORE")
            .env_remove("ADEN_SKIP_AUTO_GEN");
        command
    }

    fn query(&self, mode: &str, seed: &str, limit: usize) -> Value {
        let output = self.run(&[
            "query",
            mode,
            seed,
            "--depth",
            "2",
            "--edge-type",
            "Calls",
            "--max-results",
            &limit.to_string(),
            "--format",
            "json",
        ]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["context_receipt"]["freshness"], "current");
        value
    }

    fn store(&self) -> std::path::PathBuf {
        std::fs::read_dir(self.data.path().join("projects"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("store")
    }

    fn assembly_args(seed: &str) -> [&str; 8] {
        [
            "asm", "--from", seed, "--depth", "2", "--format", "json", ".",
        ]
    }

    fn assembly(&self, seed: &str) -> Value {
        let output = self.run(&Self::assembly_args(seed));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["context_receipt"]["freshness"], "current");
        value
    }
}

fn observed(value: &Value) -> BTreeSet<(String, u64, String)> {
    value["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["anchor"].as_str().unwrap().to_string(),
                item["depth"].as_u64().unwrap(),
                item["via_edge_types"].to_string(),
            )
        })
        .collect()
}

#[test]
fn query_store_and_writer_contended_snapshot_return_same_neighborhoods() {
    let fixture = Fixture::new(CHAIN);
    let expected: Vec<_> = [
        ("--from", "entry"),
        ("--impact", "entry"),
        ("--backlinks", "leaf"),
    ]
    .into_iter()
    .map(|(mode, seed)| (mode, seed, fixture.query(mode, seed, 100)))
    .collect();
    let store_path = fixture.store();
    let lock_path = aden_core::lock::store_lock_path(&store_path);
    let writer_lock =
        aden_core::lock::FileLock::acquire_timeout(&lock_path, std::time::Duration::ZERO).unwrap();
    assert!(
        aden_core::lock::FileLock::acquire_timeout(&lock_path, std::time::Duration::ZERO).is_err(),
        "fixture must hold an active writer lock across processes"
    );
    for (mode, seed, expected) in expected {
        let fallback = fixture.query(mode, seed, 100);
        assert_eq!(observed(&fallback), observed(&expected), "{mode}");
        assert_eq!(fallback["truncated"], false);
        assert_eq!(fallback["items"].as_array().unwrap().len(), 3);
    }
    drop(writer_lock);
    assert_eq!(fixture.query("--impact", "entry", 100)["truncated"], false);
}

#[test]
fn high_fanout_queries_report_exact_limit_and_real_omissions() {
    let mut source = String::from("pub fn leaf() {}\n");
    for index in 0..80 {
        source.push_str(&format!("pub fn caller_{index}() {{ leaf(); }}\n"));
    }
    source.push_str("pub fn entry() {\n");
    for index in 0..80 {
        source.push_str(&format!("caller_{index}();\n"));
    }
    source.push_str("}\n");
    let fixture = Fixture::new(&source);
    for (mode, seed) in [
        ("--from", "entry"),
        ("--impact", "entry"),
        ("--backlinks", "leaf"),
    ] {
        for limit in [0, 1, 17, 82, 100] {
            let value = fixture.query(mode, seed, limit);
            assert_eq!(
                value["items"].as_array().unwrap().len(),
                limit.min(82),
                "{mode}/{limit}"
            );
            assert_eq!(value["truncated"], limit < 82, "{mode}/{limit}");
            if limit >= 82 {
                assert_eq!(
                    value["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|item| item["depth"] == 2)
                        .count(),
                    1
                );
            }
        }
    }
}

#[test]
fn explicit_forward_query_does_not_silently_prune_a_high_degree_intermediate_symbol() {
    let mut source = String::new();
    for index in 0..40 {
        source.push_str(&format!("pub fn leaf_{index}() {{}}\n"));
    }
    source.push_str("pub fn dispatch() {\n");
    for index in 0..40 {
        source.push_str(&format!("leaf_{index}();\n"));
    }
    source.push_str("}\npub fn entry() { dispatch(); }\n");
    let fixture = Fixture::new(&source);
    let complete = fixture.query("--from", "entry", 100);
    assert_eq!(complete["items"].as_array().unwrap().len(), 42);
    assert_eq!(complete["truncated"], false);
    assert_eq!(
        complete["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["depth"] == 2)
            .count(),
        40
    );
    let bounded = fixture.query("--from", "entry", 10);
    assert_eq!(bounded["items"].as_array().unwrap().len(), 10);
    assert_eq!(bounded["truncated"], true);
}

#[test]
fn next_query_observes_timestamp_preserving_symbol_replacement() {
    let fixture = Fixture::new(CHAIN);
    fixture.query("--backlinks", "leaf", 100);
    let source = fixture.root.path().join("lib.rs");
    let modified = std::fs::metadata(&source).unwrap().modified().unwrap();
    std::fs::write(&source, CHAIN.replace("leaf", "twig")).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    for (mode, seed) in [
        ("--from", "entry"),
        ("--impact", "entry"),
        ("--backlinks", "twig"),
    ] {
        let value = fixture.query(mode, seed, 100);
        let anchors: BTreeSet<_> = value["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["anchor"].as_str().unwrap().rsplit('#').next().unwrap())
            .collect();
        assert_eq!(anchors, BTreeSet::from(["entry", "middle", "twig"]));
        assert_eq!(value["truncated"], false);
    }
    let old = fixture.run(&["locate", "--symbol", "leaf", "--json"]);
    assert!(old.status.success());
    let old: Value = serde_json::from_slice(&old.stdout).unwrap();
    assert_eq!(old["total"], 0);
}

#[test]
fn query_and_asm_agree_with_and_without_snapshot_and_under_writer_contention() {
    let fixture = Fixture::new(CHAIN);
    let store_path = fixture.store();
    let snapshot = store_path.with_file_name("graph.snapshot");
    let bytes = std::fs::read(&snapshot).unwrap();
    let read = || {
        (
            fixture.query("--from", "entry", 100),
            fixture.assembly("entry"),
        )
    };
    let expected = read();
    assert_eq!(expected.0["returned"], 3);
    assert!(expected.1["documents"].as_array().unwrap().len() >= 3);

    std::fs::remove_file(&snapshot).unwrap();
    assert_eq!(read(), expected, "SQLite store fallback");
    std::fs::write(&snapshot, bytes).unwrap();

    let lock_path = aden_core::lock::store_lock_path(&store_path);
    let _writer_lock =
        aden_core::lock::FileLock::acquire_timeout(&lock_path, std::time::Duration::ZERO).unwrap();
    let _concurrent_reader =
        aden_store::Storage::open_existing(store_path.to_str().unwrap()).unwrap();
    assert_eq!(read(), expected, "writer-contended snapshot");
}

#[test]
fn query_and_asm_reject_a_decodable_snapshot_with_the_wrong_revision() {
    let fixture = Fixture::new(CHAIN);
    let store_path = fixture.store();
    let snapshot = store_path.with_file_name("graph.snapshot");
    let expected_query = fixture.query("--from", "entry", 100);
    let expected_asm = fixture.assembly("entry");

    let (mut docs, mut edges) = aden_graph::snapshot::read_snapshot_file(&snapshot).unwrap();
    let entry = expected_query["items"][0]["anchor"].as_str().unwrap();
    assert!(docs.remove(entry).is_some());
    edges.retain(|(source, target, _)| source != entry && target != entry);
    let altered = aden_graph::snapshot::encode_snapshot(&docs, &edges).unwrap();
    std::fs::write(&snapshot, altered).unwrap();
    assert_eq!(fixture.query("--from", "entry", 100), expected_query);
    assert_eq!(fixture.assembly("entry"), expected_asm);

    // Under SQLite WAL, an open store handle allows concurrent readers to reject
    // a stale/tampered snapshot and fall back cleanly to the authoritative store
    // when no writer holds `store.lock`.
    let _reader = aden_store::Storage::open_existing(store_path.to_str().unwrap()).unwrap();
    assert_eq!(fixture.query("--from", "entry", 100), expected_query);
    assert_eq!(fixture.assembly("entry"), expected_asm);
    drop(_reader);

    // A rejected wrong-revision snapshot must fail closed while an active
    // writer holds `store.lock`, rather than reading mid-gen SQLite or
    // falling back to unchecked snapshot bytes.
    let lock_path = aden_core::lock::store_lock_path(&store_path);
    let _writer_lock =
        aden_core::lock::FileLock::acquire_timeout(&lock_path, std::time::Duration::ZERO).unwrap();
    assert!(
        !fixture
            .run(&Fixture::assembly_args("leaf"))
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["query", "--from", "leaf", "--format", "json"])
            .status
            .success()
    );
}

#[test]
fn asm_symbol_resolution_matches_query_and_is_independent_of_snapshot() {
    let fixture = Fixture::with_sources(&[
        ("lib.rs", CHAIN),
        ("first.rs", "pub fn duplicate() {}\n"),
        ("second.rs", "pub fn duplicate() {}\n"),
    ]);
    let canonical = fixture.query("--from", "entry", 100)["items"][0]["anchor"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(fixture.assembly("entry"), fixture.assembly(&canonical));

    let errors = |input| {
        let asm = fixture.run(&Fixture::assembly_args(input));
        let query = fixture.run(&["query", "--from", input, "--format", "json"]);
        assert!(!asm.status.success());
        assert!(!query.status.success());
        assert_eq!(asm.stderr, query.stderr);
        String::from_utf8(asm.stderr).unwrap()
    };
    let ambiguous = errors("duplicate");
    let missing = errors("this_symbol_does_not_exist");
    assert!(
        ambiguous.contains("Ambiguous symbol 'duplicate'")
            || ambiguous.contains("ambiguous_symbol")
    );
    assert!(missing.contains("not found") || missing.contains("anchor_not_found"));

    std::fs::remove_file(fixture.store().with_file_name("graph.snapshot")).unwrap();
    assert_eq!(fixture.assembly("entry"), fixture.assembly(&canonical));
    assert_eq!(errors("duplicate"), ambiguous);
    assert_eq!(errors("this_symbol_does_not_exist"), missing);
}

#[test]
fn warm_json_ask_reads_callers_and_fingerprints_before_and_after() {
    let fixture = Fixture::new(CHAIN);
    let read = || {
        let output = fixture
            .command()
            .args([
                "-j",
                "ask",
                "--from",
                "leaf",
                "--depth",
                "1",
                "--edge-types",
                "Calls",
                "What calls leaf?",
            ])
            .env("ADEN_READ_TRACE", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["context_receipt"]["freshness"], "current");
        assert!(value["context"].as_str().unwrap().contains("fn middle"));
        let trace = String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|value| value["type"] == "aden_read_trace")
            .unwrap();
        assert_eq!(trace["counters"]["fingerprint_scans"], 2);
        value
    };

    let expected = read();
    std::fs::remove_file(fixture.store().with_file_name("graph.snapshot")).unwrap();
    assert_eq!(read(), expected);
}
