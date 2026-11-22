// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_core::EdgeType;
use aden_store::GraphStorage;
use serde_json::Value;
use std::collections::BTreeSet;
use std::process::{Command, Output};

struct Fixture {
    root: tempfile::TempDir,
    data: tempfile::TempDir,
}

impl Fixture {
    fn new(files: &[(&str, &str)]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname='lookup-session'\nversion='0.1.0'\n",
        )
        .unwrap();
        for (file, source) in files {
            std::fs::write(root.path().join(file), source).unwrap();
        }
        let fixture = Self { root, data };
        fixture.success(&["gen", ".", "--quiet"]);
        fixture
    }

    fn run(&self, args: &[&str]) -> Output {
        // Allows the same behavioral assertions to reject a preserved baseline
        // executable without rebuilding or rewriting the fixture/checks.
        Command::new(
            std::env::var_os("ADEN_LOOKUP_TEST_BINARY")
                .unwrap_or_else(|| env!("CARGO_BIN_EXE_aden").into()),
        )
        .args(args)
        .current_dir(self.root.path())
        .env("ADEN_DATA_DIR", self.data.path())
        .env_remove("ADEN_STORE")
        .env_remove("ADEN_SKIP_AUTO_GEN")
        .output()
        .unwrap()
    }

    fn success(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.success(args);
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["context_receipt"]["freshness"], "current", "{value}");
        value
    }

    fn project_data(&self) -> std::path::PathBuf {
        std::fs::read_dir(self.data.path().join("projects"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
    }

    /// Bind a manually published snapshot to its exact bytes while preserving
    /// the source observation and policy fields produced by `gen`.
    fn publish_valid_snapshot(&self, bytes: &[u8]) {
        let project_data = self.project_data();
        std::fs::write(project_data.join("graph.snapshot"), bytes).unwrap();
        let manifest_path = project_data.join("freshness.json");
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["graph_revision"] = Value::String(snapshot_revision(bytes));
        std::fs::write(manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    }

    /// Seed recorded relationships without depending on ambiguous bare-call
    /// resolution. Both storage views receive the same explicit graph through
    /// their normal APIs/codecs.
    fn add_edges(&self, additions: &[(&str, &str, EdgeType)]) {
        let project_data = self.project_data();
        let storage =
            aden_store::Storage::open_existing(project_data.join("store").to_str().unwrap())
                .unwrap();
        let docs = storage.get_all_documents().unwrap();
        for (src, dst, edge_type) in additions {
            let src = docs.keys().find(|anchor| anchor.ends_with(src)).unwrap();
            let dst = docs.keys().find(|anchor| anchor.ends_with(dst)).unwrap();
            storage.put_edge(src, dst, *edge_type).unwrap();
        }
        storage.flush().unwrap();
        let (docs, edges) = aden_graph::bridge::GraphBridge::load_from_storage(&storage).unwrap();
        drop(storage);
        let bytes = aden_graph::snapshot::encode_snapshot(&docs, &edges).unwrap();
        self.publish_valid_snapshot(&bytes);
    }
}

fn item_anchors(value: &Value) -> BTreeSet<String> {
    value["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["anchor"].as_str().unwrap().to_string())
        .collect()
}

fn snapshot_revision(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn relationship_set(value: &Value, key: &str) -> BTreeSet<(String, u64, String)> {
    value[key]
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
fn locate_preserves_all_ranked_matches_totals_and_zero_limit_without_fulltext_fallback() {
    let mut source = String::from("pub fn parse() {}\n");
    for index in 0..12 {
        source.push_str(&format!("pub fn reparse_{index:02}() {{}}\n"));
    }
    let fixture = Fixture::new(&[("symbols.rs", &source)]);
    let all = fixture.json(&["locate", "--symbol", "parse", "--limit", "100", "--json"]);
    assert_eq!(all["match_kind"], "definition");
    assert_eq!(all["resolution"]["state"], "unique");
    assert_eq!(all["total"], 13);
    assert_eq!(all["returned"], 13);
    assert_eq!(all["truncated"], false);
    // Force the actual store fallback before restoring the preferred snapshot.
    // The absent-file assertion rules out an automatic snapshot regeneration
    // accidentally turning this back into a second snapshot-only observation.
    let snapshot = fixture.project_data().join("graph.snapshot");
    let saved_snapshot = snapshot.with_extension("snapshot.saved");
    std::fs::rename(&snapshot, &saved_snapshot).unwrap();
    let store_all = fixture.json(&["locate", "--symbol", "parse", "--limit", "100", "--json"]);
    let store_zero = fixture.json(&["locate", "--symbol", "parse", "--limit", "0", "--json"]);
    let stayed_absent = !snapshot.exists();
    std::fs::rename(&saved_snapshot, &snapshot).unwrap();
    assert!(
        stayed_absent,
        "store fallback control unexpectedly regenerated the snapshot"
    );
    assert_eq!(store_all["items"], all["items"]);
    assert_eq!(store_all["total"], all["total"]);
    assert_eq!(store_zero["total"], 13);
    assert_eq!(store_zero["returned"], 0);
    assert_eq!(store_zero["match_kind"], "definition");
    assert!(
        all["items"][0]["anchor"]
            .as_str()
            .unwrap()
            .ends_with("#parse")
    );
    for limit in [0, 3] {
        let limited = fixture.json(&[
            "locate",
            "--symbol",
            "parse",
            "--limit",
            &limit.to_string(),
            "--json",
        ]);
        assert_eq!(limited["match_kind"], "definition");
        assert_eq!(limited["total"], 13);
        assert_eq!(limited["returned"], limit);
        assert_eq!(limited["truncated"], true);
    }
    let suggestion = fixture.json(&["locate", "--symbol", "pars", "--limit", "100", "--json"]);
    assert_eq!(suggestion["match_kind"], "symbol_suggestions");
    assert_eq!(suggestion["resolution"]["state"], "not_found");
    assert_eq!(suggestion["total"], 13);
    let unresolved = fixture.json(&["understand", "pars", "--json"]);
    assert!(unresolved["anchor"].is_null());
    assert_eq!(unresolved["resolution"]["state"], "not_indexed");
}

#[test]
fn callers_union_ambiguous_definitions_and_accept_exact_or_scheme_stripped_anchors() {
    let fixture = Fixture::new(&[
        (
            "a.rs",
            "pub fn parse() { parse(); }\npub fn caller_a() { parse(); }\npub fn reparse() {}\npub fn excluded() { reparse(); }\n",
        ),
        (
            "b.rs",
            "pub fn parse() {}\npub fn caller_b() { parse(); }\n",
        ),
    ]);
    fixture.add_edges(&[
        ("a.rs#caller_a", "a.rs#parse", EdgeType::Calls),
        ("b.rs#caller_b", "b.rs#parse", EdgeType::Calls),
        ("a.rs#parse", "b.rs#parse", EdgeType::Calls),
    ]);
    let definitions = fixture.json(&["locate", "--symbol", "parse", "--json"]);
    assert_eq!(definitions["resolution"]["state"], "ambiguous");
    assert_eq!(
        definitions["resolution"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let ambiguous = fixture.json(&["understand", "parse", "--json"]);
    assert!(ambiguous["anchor"].is_null());
    assert_eq!(ambiguous["resolution"]["state"], "ambiguous");
    let callers = fixture.json(&["locate", "--caller-of", "parse", "--json"]);
    assert_eq!(callers["total"], 2);
    assert_eq!(
        item_anchors(&callers)
            .iter()
            .map(|anchor| anchor.rsplit('#').next().unwrap())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["caller_a", "caller_b"])
    );
    let exact = definitions["resolution"]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|anchor| anchor.as_str().filter(|anchor| anchor.contains("a.rs#")))
        .unwrap();
    for spelling in [exact, exact.trim_start_matches("aden://module/")] {
        let callers = fixture.json(&["locate", "--caller-of", spelling, "--json"]);
        assert_eq!(callers["total"], 1);
        assert!(
            callers["items"][0]["anchor"]
                .as_str()
                .unwrap()
                .ends_with("#caller_a")
        );
    }
    let missing = fixture.json(&["locate", "--caller-of", "definitely_absent", "--json"]);
    assert_eq!(missing["match_kind"], "target_not_found");
    assert_eq!(missing["total"], 0);
}

#[test]
fn understand_and_lookup_have_store_snapshot_parity_and_verified_source() {
    let fixture = Fixture::new(&[(
        "graph.rs",
        "pub fn caller() { target(); }\npub fn target() { dependency(); }\npub fn dependency() {}\npub fn unrelated() {}\n",
    )]);
    let located = fixture.json(&["locate", "--symbol", "target", "--json"]);
    let understood = fixture.json(&["understand", "target", "--json"]);
    let callers = fixture.json(&["locate", "--caller-of", "target", "--json"]);
    let store_path = fixture.project_data().join("store");
    let lock_path = aden_core::lock::store_lock_path(&store_path);
    let writer_lock =
        aden_core::lock::FileLock::acquire_timeout(&lock_path, std::time::Duration::ZERO).unwrap();
    assert!(
        aden_core::lock::FileLock::acquire_timeout(&lock_path, std::time::Duration::ZERO).is_err()
    );
    let fallback_located = fixture.json(&["locate", "--symbol", "target", "--json"]);
    assert_eq!(fallback_located["items"], located["items"]);
    assert_eq!(fallback_located["total"], located["total"]);
    let fallback = fixture.json(&["understand", "target", "--json"]);
    assert_eq!(fallback["definition"], understood["definition"]);
    assert_eq!(fallback["source"], understood["source"]);
    assert_eq!(fallback["source"]["hash_verified"], true);
    assert_eq!(
        fallback["source"]["text"],
        "pub fn target() { dependency(); }\n"
    );
    assert_eq!(
        relationship_set(&fallback, "backlinks"),
        relationship_set(&understood, "backlinks")
    );
    assert_eq!(
        relationship_set(&fallback, "impact"),
        relationship_set(&understood, "impact")
    );
    assert!(fallback["context"].as_str().unwrap().contains("dependency"));
    assert!(!fallback["context"].as_str().unwrap().contains("unrelated"));
    let fallback_callers = fixture.json(&["locate", "--caller-of", "target", "--json"]);
    assert_eq!(fallback_callers["items"], callers["items"]);
    drop(writer_lock);
}

#[test]
fn caller_with_parallel_noncall_edge_is_retained_in_snapshot_fallback() {
    let fixture = Fixture::new(&[(
        "calls.rs",
        "pub fn target() {}\npub fn caller() { target(); }\n",
    )]);
    let project_data = fixture.project_data();
    let storage =
        aden_store::Storage::open_existing(project_data.join("store").to_str().unwrap()).unwrap();
    let (docs, mut edges) = aden_graph::bridge::GraphBridge::load_from_storage(&storage).unwrap();
    let target = docs
        .keys()
        .find(|anchor| anchor.ends_with("#target"))
        .unwrap()
        .clone();
    let caller = docs
        .keys()
        .find(|anchor| anchor.ends_with("#caller"))
        .unwrap()
        .clone();
    assert!(edges.iter().any(|(src, dst, edge_type)| src == &caller
        && dst == &target
        && *edge_type == EdgeType::Calls));
    // A real multigraph pair: Calls followed by Contains makes find_edge select
    // the non-call edge in the old graph path. Publish through the snapshot codec
    // in this isolated fixture while the store is deliberately unavailable.
    edges.push((caller.clone(), target.clone(), EdgeType::Contains));
    let bytes = aden_graph::snapshot::encode_snapshot(&docs, &edges).unwrap();
    fixture.publish_valid_snapshot(&bytes);
    let callers = fixture.json(&["locate", "--caller-of", "target", "--json"]);
    assert_eq!(callers["total"], 1);
    assert_eq!(callers["items"][0]["anchor"], caller);
    let understood = fixture.json(&["understand", "target", "--json"]);
    let caller_backlink = understood["backlinks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["anchor"] == caller)
        .unwrap();
    assert!(
        caller_backlink["via_edge_types"]
            .as_array()
            .unwrap()
            .contains(&Value::String("Calls".into()))
    );
    assert!(
        caller_backlink["via_edge_types"]
            .as_array()
            .unwrap()
            .contains(&Value::String("Contains".into()))
    );
    drop(storage);
}

#[test]
fn stale_and_corrupt_snapshots_never_claim_old_graph_current_when_store_is_locked() {
    let fixture = Fixture::new(&[(
        "graph.rs",
        "pub fn target() { old_dependency(); }\npub fn old_dependency() {}\n",
    )]);
    let snapshot = fixture.project_data().join("graph.snapshot");
    let old_snapshot = std::fs::read(&snapshot).unwrap();

    std::fs::write(
        fixture.root.path().join("graph.rs"),
        "pub fn target() { new_dependency(); }\npub fn new_dependency() {}\n",
    )
    .unwrap();
    fixture.success(&["gen", ".", "--quiet"]);
    let current_snapshot = std::fs::read(&snapshot).unwrap();
    assert_ne!(old_snapshot, current_snapshot);

    // The manifest still binds the v2 source fingerprint and snapshot digest.
    // A valid v1 snapshot with newer filesystem mtime must therefore fall back
    // to the v2 store while it is available.
    std::fs::write(&snapshot, &old_snapshot).unwrap();
    let stale_fallback = fixture.json(&["understand", "target", "--json"]);
    assert!(
        stale_fallback["context"]
            .as_str()
            .unwrap()
            .contains("new_dependency")
    );
    assert!(
        !stale_fallback["context"]
            .as_str()
            .unwrap()
            .contains("old_dependency")
    );

    std::fs::write(&snapshot, b"corrupt snapshot bytes").unwrap();
    let corrupt_fallback = fixture.json(&["understand", "target", "--json"]);
    assert!(
        corrupt_fallback["context"]
            .as_str()
            .unwrap()
            .contains("new_dependency")
    );
    assert!(
        !corrupt_fallback["context"]
            .as_str()
            .unwrap()
            .contains("old_dependency")
    );

    let store_path = fixture.project_data().join("store");
    let lock_path = aden_core::lock::store_lock_path(&store_path);
    let writer_lock =
        aden_core::lock::FileLock::acquire_timeout(&lock_path, std::time::Duration::ZERO).unwrap();
    assert!(
        aden_core::lock::FileLock::acquire_timeout(&lock_path, std::time::Duration::ZERO).is_err()
    );

    std::fs::write(&snapshot, &old_snapshot).unwrap();
    let locked_old = fixture.run(&["understand", "target", "--json"]);
    if locked_old.status.success() {
        let value: Value = serde_json::from_slice(&locked_old.stdout).unwrap();
        let current = value["context_receipt"]["freshness"] == "current";
        let has_old_graph = value.to_string().contains("old_dependency");
        assert!(
            !(current && has_old_graph),
            "locked store served an old graph as current: {value}"
        );
    }

    // A digest-matching snapshot remains usable when the store is locked, and
    // restoring it leaves the next request on the v2 graph.
    std::fs::write(&snapshot, &current_snapshot).unwrap();
    let locked_current = fixture.json(&["understand", "target", "--json"]);
    assert!(
        locked_current["context"]
            .as_str()
            .unwrap()
            .contains("new_dependency")
    );
    assert!(
        !locked_current["context"]
            .as_str()
            .unwrap()
            .contains("old_dependency")
    );
    drop(writer_lock);
}

#[test]
fn method_lookup_and_next_request_after_preserved_timestamp_edit_remain_verified() {
    let fixture = Fixture::new(&[(
        "method.rs",
        "pub struct Engine;\nimpl Engine { pub fn flush(&self) {} pub fn run(&self) { self.flush(); } }\n",
    )]);
    let located = fixture.json(&["locate", "--symbol", "Engine::flush", "--json"]);
    assert_eq!(located["resolution"]["state"], "unique");
    let understood = fixture.json(&["understand", "Engine::flush", "--json"]);
    assert_eq!(understood["source"]["hash_verified"], true);
    assert!(
        understood["anchor"]
            .as_str()
            .unwrap()
            .ends_with("#Engine::flush")
    );
    let source = fixture.root.path().join("method.rs");
    let original = std::fs::read_to_string(&source).unwrap();
    let modified = std::fs::metadata(&source).unwrap().modified().unwrap();
    std::fs::write(&source, original.replace("flush", "drain")).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let updated = fixture.json(&["understand", "Engine::drain", "--json"]);
    assert_eq!(updated["source"]["hash_verified"], true);
    assert!(
        updated["source"]["text"]
            .as_str()
            .unwrap()
            .contains("fn drain")
    );
    assert!(
        !updated["source"]["text"]
            .as_str()
            .unwrap()
            .contains("fn flush")
    );
    let old = fixture.json(&["locate", "--symbol", "Engine::flush", "--json"]);
    assert_eq!(old["resolution"]["state"], "not_found");
    assert_ne!(old["match_kind"], "definition");
    assert!(
        item_anchors(&old)
            .iter()
            .all(|anchor| !anchor.ends_with("#Engine::flush"))
    );
}
