// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! PR-0 gate: `build_from_storage` must reconstruct the same graph the
//! directory parser builds, once documents and edges are persisted.
//!
//! Flow:
//! 1. Doc-only fixture → `AdenGraph::build_from_directory`
//! 2. Sync docs + edges into a temp store via `GraphBridge`
//! 3. `AdenGraph::build_from_storage` → compare canonical edge triples + anchors
//!
//! Also exercises the post-`gen` store path: after `aden gen` on a small code
//! fixture, raw `get_all_edges()` must match `build_from_storage` edge types.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use aden_core::EdgeType;
use aden_graph::bridge::GraphBridge;
use aden_graph::graph::AdenGraph;
use aden_graph::nodes::AdenEdge;
use aden_graph::snapshot;
use aden_store::{GraphStorage, Storage};

fn edge_key(s: &str, t: &str, et: EdgeType) -> (String, String, String) {
    (s.to_string(), t.to_string(), format!("{et:?}"))
}

const GLOSSARY_ADOC: &str = "\
= Glossary

[[_term]]Term::
The canonical definition.

[[_other]]Other::
References <<_term>>.
";

const POST_ADOC: &str = "\
= Post

== Intro

See <<_term>> and <<_other>>.
";

fn unique_dir(label: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "aden-equiv-{label}-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn canonical_edges(
    graph: &AdenGraph<aden_graph::nodes::DocumentNode, AdenEdge>,
) -> BTreeSet<(String, String, String)> {
    graph
        .all_edges()
        .into_iter()
        .map(|(s, t, e)| edge_key(&s, &t, e.edge_type))
        .collect()
}

fn canonical_anchors(
    graph: &AdenGraph<aden_graph::nodes::DocumentNode, AdenEdge>,
) -> BTreeSet<String> {
    graph.all_nodes().into_iter().map(|(a, _)| a).collect()
}

/// Directory-built graph round-trips through storage unchanged.
#[test]
fn directory_graph_round_trips_through_storage() {
    let dir = unique_dir("doc");
    std::fs::write(dir.join("glossary.adoc"), GLOSSARY_ADOC).unwrap();
    std::fs::write(dir.join("post.adoc"), POST_ADOC).unwrap();

    let from_dir = AdenGraph::build_from_directory(&dir).expect("directory build");
    assert!(
        !from_dir.all_nodes().is_empty(),
        "fixture must produce at least one node"
    );

    let mut docs = std::collections::HashMap::new();
    for (anchor, node) in from_dir.all_nodes() {
        docs.insert(anchor, node.doc);
    }
    let edges: Vec<(String, String, EdgeType)> = from_dir
        .all_edges()
        .into_iter()
        .map(|(s, t, e)| (s, t, e.edge_type))
        .collect();

    let store_dir = unique_dir("store");
    let storage = Storage::new(store_dir.to_str().unwrap()).unwrap();
    GraphBridge::sync_to_storage(&storage, &docs, &edges).unwrap();
    storage.flush().unwrap();

    let from_storage =
        AdenGraph::build_from_storage(&storage).expect("storage rebuild must succeed");

    assert_eq!(
        canonical_anchors(&from_dir),
        canonical_anchors(&from_storage),
        "anchor sets must match after storage round-trip"
    );
    assert_eq!(
        canonical_edges(&from_dir),
        canonical_edges(&from_storage),
        "edge triples must match after storage round-trip"
    );

    // Raw storage edges must also agree with the rebuilt graph.
    let raw: BTreeSet<(String, String, String)> = storage
        .get_all_edges()
        .unwrap()
        .into_iter()
        .map(|(s, t, et)| edge_key(&s, &t, et))
        .collect();
    assert_eq!(raw, canonical_edges(&from_storage));
}

const GREETER_RS: &str = r#"
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}
"#;

fn git(project: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .expect("git must be available");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn find_store(data: &Path) -> PathBuf {
    let projects = data.join("projects");
    let mut entries: Vec<_> = std::fs::read_dir(&projects)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.path());
    entries[0].path().join("store")
}

/// After `aden gen`, storage edges and `build_from_storage` must agree.
#[test]
fn gen_store_edges_match_build_from_storage() {
    let project = unique_dir("code");
    let data = unique_dir("data");
    std::fs::write(project.join("greeter.rs"), GREETER_RS).unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["config", "user.email", "equiv@test.invalid"]);
    git(&project, &["config", "user.name", "Equiv Test"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "fixture"]);

    let out = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["gen", "."])
        .current_dir(&project)
        .env("ADEN_DATA_DIR", &data)
        .output()
        .expect("aden must run");
    assert!(
        out.status.success(),
        "gen failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let store_path = find_store(&data);
    let storage = Storage::open_existing(store_path.to_str().unwrap()).unwrap();
    let graph = AdenGraph::build_from_storage(&storage).unwrap();

    let raw: BTreeSet<(String, String, String)> = storage
        .get_all_edges()
        .unwrap()
        .into_iter()
        .map(|(s, t, et)| edge_key(&s, &t, et))
        .collect();
    let rebuilt = canonical_edges(&graph);

    assert_eq!(
        raw, rebuilt,
        "every persisted edge must appear in build_from_storage"
    );
    assert!(
        !rebuilt.is_empty(),
        "gen fixture must produce at least one edge"
    );
}

fn find_project_dir(data: &Path) -> PathBuf {
    let projects = data.join("projects");
    let mut entries: Vec<_> = std::fs::read_dir(&projects)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.path());
    entries[0].path()
}

/// ADR-011: `gen` must publish a loadable read snapshot.
#[test]
fn gen_publishes_read_snapshot() {
    let project = unique_dir("snap");
    let data = unique_dir("snap-data");
    std::fs::write(project.join("greeter.rs"), GREETER_RS).unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["config", "user.email", "snap@test.invalid"]);
    git(&project, &["config", "user.name", "Snap Test"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "fixture"]);

    let out = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["gen", "."])
        .current_dir(&project)
        .env("ADEN_DATA_DIR", &data)
        .output()
        .expect("aden must run");
    assert!(
        out.status.success(),
        "gen failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let project_dir = find_project_dir(&data);
    let snapshot_path = project_dir.join("graph.snapshot");
    assert!(
        snapshot_path.is_file(),
        "graph.snapshot missing at {}",
        snapshot_path.display()
    );

    let store_path = find_store(&data);
    assert!(snapshot::snapshot_covers_store(&snapshot_path, &store_path));

    let (docs, edges) = snapshot::read_snapshot_file(&snapshot_path).unwrap();
    assert!(!docs.is_empty());
    assert!(!edges.is_empty());

    // Read path uses the snapshot via subprocess (no process-global env mutation).
    let status_out = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["status", ".", "-j"])
        .current_dir(&project)
        .env("ADEN_DATA_DIR", &data)
        .output()
        .expect("aden status must run");
    assert!(
        status_out.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&status_out.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&status_out.stdout).expect("status must emit JSON");
    assert!(
        json.get("read_snapshot")
            .and_then(|v| v.as_str())
            .is_some_and(|p| !p.is_empty()),
        "status must report read_snapshot: {json}"
    );
}
