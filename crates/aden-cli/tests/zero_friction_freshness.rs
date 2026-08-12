// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Zero-friction freshness contracts:
//! - delete/rename detection marks the index stale
//! - silent gen does not hang forever when the write lock is held
//! - JSON envelopes expose `freshness`

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const LIB_RS: &str = r#"
/// Greeting helper.
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}
"#;

const OTHER_RS: &str = r#"
/// Second module.
pub fn other() -> u32 {
    42
}
"#;

fn unique_dir(label: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "aden-zf-{label}-{}-{}-{}",
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
        .expect("git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn scaffold() -> (PathBuf, PathBuf) {
    let project = unique_dir("proj");
    let data = unique_dir("data");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
    std::fs::write(project.join("src/other.rs"), OTHER_RS).unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["config", "user.email", "zf@test.invalid"]);
    git(&project, &["config", "user.name", "ZF Test"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "fixture"]);
    (project, data)
}

fn run(project: &Path, data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(args)
        .current_dir(project)
        .env("ADEN_DATA_DIR", data)
        .output()
        .expect("aden binary must run")
}

fn json(project: &Path, data: &Path, args: &[&str]) -> serde_json::Value {
    let output = run(project, data, args);
    assert!(
        output.status.success(),
        "aden {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "aden {:?} returned invalid JSON ({e}): {}",
            args,
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn store_path(project: &Path, data: &Path) -> PathBuf {
    let output = run(project, data, &["store", "path"]);
    assert!(
        output.status.success(),
        "store path failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}

#[test]
fn old_layout_manifest_and_gen_cache_trigger_one_automatic_rebuild() {
    let (project, data) = scaffold();
    let first = json(&project, &data, &["tree", "--symbols", "."]);
    assert_eq!(first["freshness"], "current");

    let state_dir = store_path(&project, &data)
        .parent()
        .expect("store has project-state parent")
        .to_path_buf();
    let manifest_path = state_dir.join("freshness.json");
    let cache_path = state_dir.join("gen-cache.json");

    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest
        .as_object_mut()
        .unwrap()
        .remove("index_layout_version");
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

    let mut cache: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    cache["version"] = 6.into();
    std::fs::write(&cache_path, serde_json::to_vec(&cache).unwrap()).unwrap();

    let rebuilt = json(&project, &data, &["tree", "--symbols", "."]);
    assert_eq!(rebuilt["freshness"], "current");
    assert!(rebuilt["returned_symbol_count"].as_u64().unwrap_or(0) >= 2);

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["index_layout_version"], 3);
    let cache: serde_json::Value =
        serde_json::from_slice(&std::fs::read(cache_path).unwrap()).unwrap();
    assert_eq!(cache["version"], 9);

    let _ = std::fs::remove_dir_all(project);
    let _ = std::fs::remove_dir_all(data);
}

#[test]
fn delete_source_makes_index_stale_then_grep_prunes() {
    let (project, data) = scaffold();
    let gen_out = run(&project, &data, &["gen", "."]);
    assert!(
        gen_out.status.success(),
        "gen failed: {}",
        String::from_utf8_lossy(&gen_out.stderr)
    );

    // Delete a source file — index must treat this as stale and prune on refresh.
    std::fs::remove_file(project.join("src/other.rs")).unwrap();

    let started = Instant::now();
    let grep = run(
        &project,
        &data,
        &["grep", "fn other", ".", "--json", "--limit", "20"],
    );
    assert!(
        grep.status.success(),
        "grep after delete failed: {}",
        String::from_utf8_lossy(&grep.stderr)
    );
    // Must not hang for multi-minute lock queues.
    assert!(
        started.elapsed() < Duration::from_secs(120),
        "grep after delete took too long: {:?}",
        started.elapsed()
    );

    let out = String::from_utf8_lossy(&grep.stdout);
    // After refresh, the deleted symbol should not dominate results.
    // (total may still be 0 matches — that is success.)
    assert!(
        !out.contains("src/other.rs")
            || out.contains("\"total\":0")
            || out.contains("\"total\": 0"),
        "unexpected grep after delete: {out}"
    );
}

#[test]
fn status_json_includes_health_after_gen() {
    let (project, data) = scaffold();
    let gen_out = run(&project, &data, &["gen", "."]);
    assert!(gen_out.status.success());
    let st = run(&project, &data, &["status", ".", "-j"]);
    assert!(st.status.success());
    let out = String::from_utf8_lossy(&st.stdout);
    assert!(
        out.contains("health") || out.contains("ok"),
        "status json: {out}"
    );
}

#[test]
fn gen_immediate_read_is_current_and_rename_rearms_refresh() {
    let (project, data) = scaffold();
    assert!(run(&project, &data, &["gen", "."]).status.success());

    // A completed explicit generation must publish its manifest before the
    // very next public read, not rely on a later cache warm-up.
    let immediate = json(&project, &data, &["grep", "Greeting", ".", "--json"]);
    assert_eq!(immediate["freshness"], "current");

    std::fs::rename(project.join("src/other.rs"), project.join("src/renamed.rs")).unwrap();
    let after = json(&project, &data, &["grep", "fn other", ".", "--json"]);
    assert_eq!(after["freshness"], "current");
    let rendered = after.to_string();
    assert!(
        !rendered.contains("src/other.rs"),
        "renamed-away path survived a current response: {rendered}"
    );
}

#[test]
fn same_second_equal_size_edit_refreshes_and_receipt_proves_revision() {
    let (project, data) = scaffold();
    assert!(run(&project, &data, &["gen", "."]).status.success());
    let before = json(&project, &data, &["grep", "Greeting", ".", "--json"]);
    let before_receipt = &before["context_receipt"];
    assert_eq!(before_receipt["freshness"], "current");
    let old_revision = before_receipt["graph_revision"]
        .as_str()
        .expect("receipt graph revision")
        .to_string();
    let old_fingerprint = before_receipt["observed_source_fingerprint"]
        .as_str()
        .expect("receipt source fingerprint")
        .to_string();

    // Keep byte length identical and do not sleep: whole-second mtime checks
    // cannot reliably distinguish this rewrite.
    let changed = LIB_RS.replace("Greeting", "Saluting");
    assert_eq!(changed.len(), LIB_RS.len());
    std::fs::write(project.join("src/lib.rs"), changed).unwrap();

    let after = json(
        &project,
        &data,
        &["--require-fresh", "grep", "Saluting", ".", "--json"],
    );
    assert_eq!(after["freshness"], "current");
    assert_eq!(after["context_receipt"]["refresh_cause"], "source_changed");
    assert_ne!(
        after["context_receipt"]["graph_revision"], old_revision,
        "a successful regeneration must publish a new graph revision"
    );
    assert_ne!(
        after["context_receipt"]["observed_source_fingerprint"],
        old_fingerprint
    );
}

#[test]
fn frozen_authoritative_read_fails_actionably_then_recovers() {
    let (project, data) = scaffold();
    assert!(run(&project, &data, &["gen", "."]).status.success());
    std::fs::write(
        project.join("src/lib.rs"),
        LIB_RS.replace("Greeting", "Saluting"),
    )
    .unwrap();

    let blocked = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["--require-fresh", "grep", "Saluting", ".", "--json"])
        .current_dir(&project)
        .env("ADEN_DATA_DIR", &data)
        .env("ADEN_SKIP_AUTO_GEN", "1")
        .output()
        .unwrap();
    assert_eq!(blocked.status.code(), Some(2));
    let error = String::from_utf8_lossy(&blocked.stderr);
    assert!(
        error.contains("authoritative freshness required"),
        "{error}"
    );
    assert!(error.contains("ADEN_SKIP_AUTO_GEN"), "{error}");

    let recovered = json(
        &project,
        &data,
        &["--require-fresh", "grep", "Saluting", ".", "--json"],
    );
    assert_eq!(recovered["freshness"], "current");
}

#[test]
fn absent_or_corrupt_manifest_never_proves_authoritative_currentness() {
    let (project, data) = scaffold();
    assert!(run(&project, &data, &["gen", "."]).status.success());
    let manifest = store_path(&project, &data)
        .parent()
        .expect("per-project store parent")
        .join("freshness.json");
    assert!(manifest.is_file(), "gen must publish freshness manifest");
    std::fs::write(&manifest, b"{not valid json").unwrap();

    // Frozen mode cannot silently trust the old cache/whole-second mtime
    // heuristic when the binding between source and graph is damaged.
    let blocked = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["--require-fresh", "grep", "Greeting", ".", "--json"])
        .current_dir(&project)
        .env("ADEN_DATA_DIR", &data)
        .env("ADEN_SKIP_AUTO_GEN", "1")
        .output()
        .unwrap();
    assert_eq!(blocked.status.code(), Some(2));

    let recovered = json(
        &project,
        &data,
        &["--require-fresh", "grep", "Greeting", ".", "--json"],
    );
    assert_eq!(recovered["freshness"], "current");
    assert!(manifest.is_file());
}

#[test]
fn failed_generation_cannot_leave_an_authoritative_current_claim() {
    let (project, data) = scaffold();
    assert!(run(&project, &data, &["gen", "."]).status.success());
    let manifest = store_path(&project, &data)
        .parent()
        .expect("per-project store parent")
        .join("freshness.json");
    std::fs::remove_file(&manifest).unwrap();
    std::fs::create_dir(&manifest).unwrap(); // atomic manifest rename must fail
    std::fs::write(
        project.join("src/lib.rs"),
        LIB_RS.replace("Greeting", "Saluting"),
    )
    .unwrap();

    let started = Instant::now();
    let output = run(
        &project,
        &data,
        &["--require-fresh", "grep", "Saluting", ".", "--json"],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(started.elapsed() < Duration::from_secs(7));
    assert!(String::from_utf8_lossy(&output.stderr).contains("authoritative freshness required"));
}

#[test]
fn authoritative_writer_contention_has_one_wall_clock_deadline() {
    let (project, data) = scaffold();
    assert!(run(&project, &data, &["gen", "."]).status.success());
    std::fs::write(
        project.join("src/lib.rs"),
        LIB_RS.replace("Greeting", "Saluting"),
    )
    .unwrap();
    let store = store_path(&project, &data);
    let held = aden_core::lock::FileLock::acquire_timeout(
        aden_core::lock::store_lock_path(&store),
        Duration::ZERO,
    )
    .expect("fixture owns writer lock");

    let started = Instant::now();
    let blocked = run(
        &project,
        &data,
        &["--require-fresh", "grep", "Saluting", ".", "--json"],
    );
    drop(held);
    assert_eq!(blocked.status.code(), Some(2));
    assert!(
        started.elapsed() < Duration::from_secs(7),
        "authoritative read exceeded one 5s deadline: {:?}",
        started.elapsed()
    );
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("within 5s"),
        "{}",
        String::from_utf8_lossy(&blocked.stderr)
    );
}

#[test]
fn mid_query_mutation_never_returns_false_current() {
    let (project, data) = scaffold();
    assert!(run(&project, &data, &["gen", "."]).status.success());
    std::fs::write(
        project.join("src/lib.rs"),
        LIB_RS.replace("Greeting", "Saluting"),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["grep", "Saluting", ".", "--json"])
        .current_dir(&project)
        .env("ADEN_DATA_DIR", &data)
        .env("ADEN_TEST_MUTATE_DURING_GEN", "src/lib.rs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "read-triggered gen failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_ne!(
        value["freshness"], "current",
        "a source mutation during refresh must remain visible: {value}"
    );
    assert_eq!(value["index_stale"], true);
}

#[test]
fn independent_repositories_keep_independent_revisions() {
    let (project_a, data) = scaffold();
    let (project_b, _) = scaffold();
    std::fs::write(
        project_b.join("src/lib.rs"),
        LIB_RS.replace("Greeting", "Welcome!"),
    )
    .unwrap();
    assert!(run(&project_a, &data, &["gen", "."]).status.success());
    assert!(run(&project_b, &data, &["gen", "."]).status.success());

    let a = json(&project_a, &data, &["grep", "Greeting", ".", "--json"]);
    let b = json(&project_b, &data, &["grep", "Welcome", ".", "--json"]);
    assert_ne!(
        a["context_receipt"]["graph_revision"],
        b["context_receipt"]["graph_revision"]
    );
    assert_ne!(
        a["context_receipt"]["observed_source_fingerprint"],
        b["context_receipt"]["observed_source_fingerprint"]
    );
}
