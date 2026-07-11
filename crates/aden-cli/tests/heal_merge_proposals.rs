// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Integration test: `aden heal . --propose` routes store-resident drift events
//! through the three-way merge engine and writes a `MergeReconcile` proposal.
//!
//! Hermetic pattern mirrors crates/aden-cli/tests/wave3_edges.rs:
//! * per-test unique temp dirs (pid + nanos + counter)
//! * ADEN_DATA_DIR env var isolates the store
//! * assert_cmd for binary invocation

use std::path::{Path, PathBuf};
use std::process::Command;

// ── helpers ──────────────────────────────────────────────────────────────────

fn unique_dir(label: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "aden-merge-{label}-{}-{}-{}",
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
        "git {:?} failed:\n  stdout: {}\n  stderr: {}",
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn aden(project: &Path, data: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(args)
        .current_dir(project)
        .env("ADEN_DATA_DIR", data)
        .output()
        .expect("aden binary must be available")
}

// ── fixture ───────────────────────────────────────────────────────────────────

const LIB_V1: &str = r#"/// Alpha computes the alpha value.
pub fn alpha() -> i32 { 1 }
"#;

const LIB_V2: &str = r#"/// Alpha computes the new alpha value.
/// Returns a doubled result.
pub fn alpha() -> i32 { 2 }
"#;

/// Scaffold a minimal Rust project, run `aden gen .`, then rewrite the source
/// to create a StaleHash drift event. Returns (project_dir, data_dir).
fn scaffold() -> (PathBuf, PathBuf) {
    let project = unique_dir("proj");
    let data = unique_dir("data");

    // Minimal Rust project — no Cargo.toml needed; aden indexes raw .rs files.
    let src_dir = project.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("lib.rs"), LIB_V1).unwrap();

    // Git init so aden can resolve the project root.
    git(&project, &["init", "-q"]);
    git(
        &project,
        &["config", "user.email", "merge-test@test.invalid"],
    );
    git(&project, &["config", "user.name", "Merge Test"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "fixture: v1"]);

    // Generate contracts into the store so base snapshots exist.
    let gen_out = aden(&project, &data, &["gen", "."]);
    assert!(
        gen_out.status.success(),
        "aden gen . failed:\n  stdout: {}\n  stderr: {}",
        String::from_utf8_lossy(&gen_out.stdout),
        String::from_utf8_lossy(&gen_out.stderr)
    );

    // Rewrite the source to create drift.
    std::fs::write(src_dir.join("lib.rs"), LIB_V2).unwrap();
    // Do NOT commit — the store still has v1; the file on disk is v2.

    (project, data)
}

// ── test ──────────────────────────────────────────────────────────────────────

/// After `aden gen .` + source rewrite, `aden heal . --propose` must write a
/// MergeReconcile proposal whose patch_asciidoc contains:
/// * `:drift_type: MergeReconcile`
/// * `== Merged contract`
/// * at least one `[generated#` region tag
///
/// And `aden_propose::list(project_root)` must be able to parse it back with
/// `drift_type == "MergeReconcile"` and `status == PendingReview`.
#[test]
fn heal_propose_emits_merge_reconcile_proposal() {
    let (project, data) = scaffold();

    let heal_out = aden(&project, &data, &["heal", ".", "--propose"]);
    let stdout = String::from_utf8_lossy(&heal_out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&heal_out.stderr).to_string();
    assert!(
        heal_out.status.success(),
        "aden heal . --propose failed:\n  stdout: {stdout}\n  stderr: {stderr}"
    );

    // Proposals directory must exist and contain at least one .patch.adoc file.
    let proposals_dir = project.join(".aden").join("proposals");
    assert!(
        proposals_dir.exists(),
        "proposals dir not created: {stdout}\n{stderr}"
    );

    let adoc_files: Vec<_> = std::fs::read_dir(&proposals_dir)
        .expect("cannot read proposals dir")
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x == "adoc")
                .unwrap_or(false)
        })
        .collect();
    assert!(
        !adoc_files.is_empty(),
        "no .patch.adoc files in proposals dir"
    );

    // At least one proposal must be a MergeReconcile.
    let merge_proposal = adoc_files.iter().find(|e| {
        let contents = std::fs::read_to_string(e.path()).unwrap_or_default();
        contents.contains(":drift_type: MergeReconcile")
    });
    let merge_file = merge_proposal
        .expect("no proposal with ':drift_type: MergeReconcile' found")
        .path();
    let contents = std::fs::read_to_string(&merge_file).unwrap();

    assert!(
        contents.contains("== Merged contract"),
        "proposal missing '== Merged contract' section:\n{contents}"
    );
    assert!(
        contents.contains("[generated#"),
        "proposal missing '[generated#' region tag:\n{contents}"
    );

    // parse_proposal round-trip via aden_propose::list.
    let proposals = aden_propose::list(&project).expect("list must succeed");
    let merge_proposals: Vec<_> = proposals
        .iter()
        .filter(|p| p.drift_type == "MergeReconcile")
        .collect();
    assert!(
        !merge_proposals.is_empty(),
        "aden_propose::list returned no MergeReconcile proposal; all proposals: {proposals:?}"
    );
    let p = merge_proposals[0];
    assert_eq!(
        p.status,
        aden_propose::ProposalStatus::PendingReview,
        "MergeReconcile proposal must remain PendingReview, got {:?}",
        p.status
    );
}
