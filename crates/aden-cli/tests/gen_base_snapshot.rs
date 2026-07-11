// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration test: `aden gen` must record a base snapshot for every
//! source-document it writes to the store.
//!
//! The snapshot text is the canonical `emit_contract_document` output for the
//! stored `Document`, so `parse_contract(snapshot) == ContractDocument::from_document(stored_doc)`
//! (byte-level roundtrip).  This property is what makes three-way merge
//! possible: the `base` layer is always recoverable from the store without
//! re-running the parser.

use std::path::{Path, PathBuf};
use std::process::Command;

use aden_core::contract::{ContractDocument, ParseMode};
use aden_emit::emit_contract_document;
use aden_store::{GraphStorage, Storage};

const LIB_RS: &str = r#"
/// A simple greeting helper.
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

/// A numeric identity function.
pub fn identity(x: u32) -> u32 {
    x
}
"#;

fn unique_dir(label: &str) -> PathBuf {
    // pid+nanos alone collides on macOS (µs clock granularity) when parallel
    // test threads enter in the same tick; the counter disambiguates.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "aden-snap-{label}-{}-{}-{}",
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

/// Scaffold a tiny fixture project and return `(project_dir, data_dir)`.
fn scaffold() -> (PathBuf, PathBuf) {
    let project = unique_dir("proj");
    let data = unique_dir("data");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["config", "user.email", "snap@test.invalid"]);
    git(&project, &["config", "user.name", "Snap Test"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "fixture"]);
    (project, data)
}

/// Run `aden gen .` with an isolated `ADEN_DATA_DIR` and assert it succeeds.
fn run_gen(project: &Path, data: &Path) {
    let out = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["gen", "."])
        .current_dir(project)
        .env("ADEN_DATA_DIR", data)
        .output()
        .expect("aden binary must run");
    assert!(
        out.status.success(),
        "aden gen failed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Locate the single `projects/<key>/store` directory under `data`.
fn find_store(data: &Path) -> PathBuf {
    let projects = data.join("projects");
    let mut entries: Vec<_> = std::fs::read_dir(&projects)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", projects.display()))
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one project key dir under {}; got {}",
        projects.display(),
        entries.len()
    );
    entries.remove(0).path().join("store")
}

/// Core assertion: every document written by `aden gen` must have a base
/// snapshot that round-trips through `emit_contract_document`.
#[test]
fn gen_records_base_snapshot_for_every_stored_doc() {
    let (project, data) = scaffold();
    run_gen(&project, &data);

    let store_path = find_store(&data);
    let store = Storage::open_existing(store_path.to_str().unwrap())
        .unwrap_or_else(|e| panic!("cannot open store at {}: {e}", store_path.display()));

    let all_docs = store
        .get_all_documents()
        .expect("get_all_documents must succeed");

    assert!(
        !all_docs.is_empty(),
        "store must contain at least one document after gen"
    );

    // Filter to source-symbol docs only: module/synthesized nodes (anchors
    // beginning with "mod-") carry no meaningful canonical contract text.
    let source_docs: Vec<_> = all_docs
        .values()
        .filter(|d| !d.anchor.starts_with("mod-"))
        .collect();

    assert!(
        !source_docs.is_empty(),
        "store must contain at least one source-symbol document; all anchors: {:?}",
        all_docs.keys().collect::<Vec<_>>()
    );

    for doc in source_docs {
        let expected_text = emit_contract_document(&ContractDocument::from_document(doc));

        let snapshot = store
            .get_base_snapshot(&doc.anchor)
            .unwrap_or_else(|e| panic!("get_base_snapshot({}) failed: {e}", doc.anchor));

        // RED gate: snapshot must be Some — this fails before implementation.
        let text = snapshot.unwrap_or_else(|| {
            panic!(
                "get_base_snapshot({}) returned None — base snapshot was not recorded",
                doc.anchor
            )
        });

        assert_eq!(
            text, expected_text,
            "snapshot for {} must equal emit_contract_document(ContractDocument::from_document(doc))",
            doc.anchor
        );

        // Also assert the snapshot round-trips through parse_contract.
        aden_core::contract::parse_contract(&text, ParseMode::Permissive)
            .unwrap_or_else(|e| panic!("parse_contract failed for {}: {e}", doc.anchor));
    }
}
