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
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname='query-session'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(root.path().join("lib.rs"), source).unwrap();
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
        Command::new(env!("CARGO_BIN_EXE_aden"))
            .args(args)
            .current_dir(self.root.path())
            .env("ADEN_DATA_DIR", self.data.path())
            .env_remove("ADEN_STORE")
            .env_remove("ADEN_SKIP_AUTO_GEN")
            .output()
            .unwrap()
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
    let storage = aden_store::Storage::open_existing(store_path.to_str().unwrap()).unwrap();
    assert!(
        aden_store::Storage::open_existing(store_path.to_str().unwrap()).is_err(),
        "fixture must make the keyed store unavailable to another process"
    );
    for (mode, seed, expected) in expected {
        let fallback = fixture.query(mode, seed, 100);
        assert_eq!(observed(&fallback), observed(&expected), "{mode}");
        assert_eq!(fallback["truncated"], false);
        assert_eq!(fallback["items"].as_array().unwrap().len(), 3);
    }
    drop(storage);
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
