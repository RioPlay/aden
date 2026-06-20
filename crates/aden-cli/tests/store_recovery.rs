// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration test (ADR-009): aden auto-recovers a store left in an unreadable
//! storage-engine format. Corrupting fjall's on-disk `version` marker simulates
//! a fjall V2 store opened by a fjall-3 binary (the post-#44 upgrade hazard).
//!
//! Runs the freshly-built binary (`CARGO_BIN_EXE_aden`) over a hermetic git
//! fixture with an isolated `ADEN_DATA_DIR`, so the per-user store and caches
//! live entirely under a temp dir and never touch the developer's real store.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LIB_RS: &str = r#"
/// A simple greeting helper.
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}
"#;

fn unique_dir(label: &str) -> PathBuf {
    // pid+nanos alone collides on coarse clocks when parallel test threads enter
    // in the same tick; the counter disambiguates.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "aden-recover-{label}-{}-{}-{}",
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

/// Scaffold a tiny git fixture and return `(project_dir, data_dir)`.
fn scaffold() -> (PathBuf, PathBuf) {
    let project = unique_dir("proj");
    let data = unique_dir("data");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["config", "user.email", "recover@test.invalid"]);
    git(&project, &["config", "user.name", "Recover Test"]);
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

/// Locate the single `projects/<key>/store` directory under `data`.
fn find_store(data: &Path) -> PathBuf {
    let projects = data.join("projects");
    let mut entries: Vec<_> = std::fs::read_dir(&projects)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", projects.display()))
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1, "expected exactly one project key dir");
    entries.remove(0).path().join("store")
}

/// Simulate an incompatible (e.g. fjall V2) store by overwriting the engine's
/// on-disk version marker; fjall's `check_version` then rejects it as an
/// unreadable format — the same signal a genuine V2 store produces.
fn corrupt_store_version(store: &Path) {
    std::fs::write(store.join("version"), b"FJL\x02").unwrap();
}

/// A pure READ against an incompatible store must auto-recover (rebuild from
/// source) rather than degrade to "No symbol found". Regression guard for the
/// `ensure_fresh` recovery probe (and that it only returns "handled" on success).
#[test]
fn read_auto_recovers_incompatible_store() {
    let (project, data) = scaffold();
    assert!(run(&project, &data, &["gen", "."]).status.success());
    corrupt_store_version(&find_store(&data));

    let out = run(&project, &data, &["understand", "greet", "."]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("greet") && !stdout.contains("No symbol found"),
        "a read on an incompatible store must auto-recover.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `gen` on an incompatible store must rebuild (not hard-error); and deleting
/// only `store/` while leaving the gen-cache must still full-rebuild, not skip
/// every file as "unchanged" against an empty store (the silent-empty trap).
#[test]
fn gen_recovers_and_empty_store_guard_rebuilds() {
    let (project, data) = scaffold();
    assert!(run(&project, &data, &["gen", "."]).status.success());
    let store = find_store(&data);

    corrupt_store_version(&store);
    let out = run(&project, &data, &["gen", "."]);
    assert!(
        out.status.success(),
        "gen must recover an incompatible store, not error.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Delete ONLY the store dir; keep the gen-cache. gen must re-scan all files.
    std::fs::remove_dir_all(&store).unwrap();
    let out = run(&project, &data, &["gen", "."]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(
        !stdout.contains("Stored 0 contracts"),
        "deleting store/ then gen must rebuild, not skip all as unchanged.\nstdout: {stdout}"
    );
}
