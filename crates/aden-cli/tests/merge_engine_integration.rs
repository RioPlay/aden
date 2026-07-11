// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Integration tests for the Phase 5 merge-engine `--fix` and `--apply` paths.
//!
//! Each test is hermetically isolated:
//! * per-test unique temp dirs (pid + nanos + counter)
//! * `ADEN_DATA_DIR` env var pins the store to the test's data dir
//! * `aden_store::Storage` is opened directly to assert store state
//!
//! Anchor naming: for a Rust source file at `src/lib.rs` inside a git repo
//! with no Cargo.toml, `infer_project_name` returns `"src"` (first component
//! under the VCS root), so all anchors take the form
//! `aden://module/src/lib.rs#<symbol>`.  Overlay filenames are produced by
//! `sanitize_anchor_filename`, which preserves `.`, yielding e.g.
//! `aden---module-src-lib.rs-alpha.adoc`.
//!
//! Scenarios:
//! 1. `human_preservation_clean_merge` — overlay untouched, generated updated
//! 2. `conflict_generates_proposed_marker` — same-tag human block → [proposed]
//! 3. `apply_mergereconcile_proposal` — `--propose` then `--apply <id>` round-trip
//! 4. `idempotence_no_source_change` — zero Fixed after gen with no source changes
//! 5. `polyglot_python_powershell` — .py + .ps1 gen/heal cycle without panics

use std::path::{Path, PathBuf};
use std::process::Command;

use aden_store::{GraphStorage, Storage};

// ── well-known anchor names ───────────────────────────────────────────────────
//
// These are stable because `infer_project_name` returns `"src"` for all our
// test fixtures (no Cargo.toml → falls through to VCS root logic, first
// path component under root = "src").  `make_anchor("src","lib.rs",<sym>)`
// always yields `aden://module/src/lib.rs#<sym>`.
const ALPHA_ANCHOR: &str = "aden://module/src/lib.rs#alpha";
const BETA_ANCHOR: &str = "aden://module/src/lib.rs#beta";
const GAMMA_ANCHOR: &str = "aden://module/src/lib.rs#gamma";
/// Overlay filename for an anchor, as produced by `sanitize_anchor_filename`.
/// `.` is preserved, `:`, `/`, `#` become `-`.
fn overlay_filename(anchor: &str) -> String {
    let slug: String = anchor
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{slug}.adoc")
}

// ── shared helpers ────────────────────────────────────────────────────────────

fn unique_dir(label: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "aden-merge-integ-{label}-{}-{}-{}",
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

fn open_store(data: &Path) -> Storage {
    let store_path = find_store(data);
    Storage::open_existing(store_path.to_str().unwrap())
        .unwrap_or_else(|e| panic!("cannot open store at {}: {e}", store_path.display()))
}

fn run_gen(project: &Path, data: &Path) {
    let out = aden(project, data, &["gen", "."]);
    assert!(
        out.status.success(),
        "aden gen . failed:\n  stdout: {}\n  stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_init_commit(project: &Path, message: &str) {
    git(project, &["init", "-q"]);
    git(
        project,
        &["config", "user.email", "merge-integ@test.invalid"],
    );
    git(project, &["config", "user.name", "Merge Integ Test"]);
    git(project, &["add", "-A"]);
    git(project, &["commit", "-q", "-m", message]);
}

// ── test 1: human_preservation_clean_merge ───────────────────────────────────

/// A [human] block in the overlay whose tag does NOT collide with any
/// generated block tag must survive a source change as a clean merge
/// (Applied outcome); the overlay file itself must be byte-identical.
///
/// Regression target: before Phase 5, `--fix` for store-resident StaleHash
/// reported "StaleHash in store (run aden gen to refresh)" and did nothing.
#[test]
fn human_preservation_clean_merge() {
    let project = unique_dir("human-clean");
    let data = unique_dir("human-clean-data");

    let src_dir = project.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(
        src_dir.join("lib.rs"),
        "/// Compute the alpha value.\npub fn alpha() -> i32 { 1 }\n",
    )
    .unwrap();

    git_init_commit(&project, "fixture: v1");
    run_gen(&project, &data);

    // Overlay for ALPHA_ANCHOR with a [human] block tagged "user-note"
    // (different tag from the generated block "aden://module/src/lib.rs#alpha").
    let overlay_dir = project.join(".aden").join("overlays");
    std::fs::create_dir_all(&overlay_dir).unwrap();
    let overlay_file = overlay_dir.join(overlay_filename(ALPHA_ANCHOR));
    let overlay_content = format!(
        ":anchor: {ALPHA_ANCHOR}\n\n[human#user-note]\n----\nThis human note must be preserved.\n----\n"
    );
    std::fs::write(&overlay_file, &overlay_content).unwrap();
    let overlay_before = std::fs::read(&overlay_file).unwrap();

    // Mutate source (doc comment changes → contract content changes → UpdateGenerated).
    std::fs::write(
        src_dir.join("lib.rs"),
        "/// Compute the updated alpha value.\npub fn alpha() -> i32 { 2 }\n",
    )
    .unwrap();

    let out = aden(&project, &data, &["heal", ".", "--fix"]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "aden heal . --fix failed:\n  stdout: {stdout}\n  stderr: {stderr}"
    );

    // --fix must report a clean apply (merge-applied).
    assert!(
        stdout.contains("merge-applied"),
        "expected 'merge-applied' in --fix stdout:\n{stdout}"
    );

    // The overlay file must be byte-identical — --fix never modifies overlays.
    let overlay_after = std::fs::read(&overlay_file).unwrap();
    assert_eq!(
        overlay_before, overlay_after,
        "overlay file was modified by --fix; human blocks must be preserved on disk"
    );

    // The base snapshot must be updated to reflect the new source.
    let store = open_store(&data);
    let snapshot = store
        .get_base_snapshot(ALPHA_ANCHOR)
        .expect("get_base_snapshot must not error")
        .expect("base snapshot for ALPHA_ANCHOR must exist after --fix");
    assert!(
        !snapshot.is_empty(),
        "base snapshot for ALPHA_ANCHOR must not be empty"
    );
}

// ── test 2: conflict_generates_proposed_marker ───────────────────────────────

/// A [human] block with the SAME tag as the generated block forces a
/// Conflict.  The merge engine must write a MergeReconcile proposal
/// containing `[proposed` markers; the store must NOT be mutated.
///
/// Regression target: before Phase 5, there was no conflict path — the event
/// was silently skipped.
#[test]
fn conflict_generates_proposed_marker() {
    let project = unique_dir("conflict-prop");
    let data = unique_dir("conflict-prop-data");

    let src_dir = project.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(
        src_dir.join("lib.rs"),
        "/// Returns the beta value.\npub fn beta() -> i32 { 1 }\n",
    )
    .unwrap();

    git_init_commit(&project, "fixture: v1");
    run_gen(&project, &data);

    // Capture the stored document BEFORE --fix to assert no mutation.
    let doc_before = {
        let s = open_store(&data);
        s.get_document(BETA_ANCHOR)
            .expect("get_document must not error")
            .expect("beta must be in store before test")
    };

    // Overlay with a [human] block tagged BETA_ANCHOR — same tag as the
    // generated block produced by from_document → triggers Conflict.
    let overlay_dir = project.join(".aden").join("overlays");
    std::fs::create_dir_all(&overlay_dir).unwrap();
    let overlay_file = overlay_dir.join(overlay_filename(BETA_ANCHOR));
    let overlay_content = format!(
        ":anchor: {BETA_ANCHOR}\n\n[human#{BETA_ANCHOR}]\n----\nDo not overwrite this human decision.\n----\n"
    );
    std::fs::write(&overlay_file, &overlay_content).unwrap();

    // Mutate source → StaleHash with conflicting overlay.
    std::fs::write(
        src_dir.join("lib.rs"),
        "/// Returns the updated beta value.\npub fn beta() -> i32 { 2 }\n",
    )
    .unwrap();

    let out = aden(&project, &data, &["heal", ".", "--fix"]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "aden heal . --fix must exit 0 even on conflicts:\n  stdout: {stdout}\n  stderr: {stderr}"
    );

    // A conflict must be reported.
    assert!(
        stdout.contains("Conflict:") || stdout.contains("MergeConflict"),
        "expected conflict indicator in stdout:\n{stdout}"
    );

    // A MergeReconcile proposal must have been written.
    let proposals = aden_propose::list(&project).expect("aden_propose::list must succeed");
    let conflict_proposals: Vec<_> = proposals
        .iter()
        .filter(|p| p.drift_type == "MergeReconcile")
        .collect();
    assert!(
        !conflict_proposals.is_empty(),
        "no MergeReconcile proposal written for conflict; all proposals: {proposals:?}"
    );

    // The proposal must contain a [proposed block marker.
    let p = conflict_proposals[0];
    assert!(
        p.patch_asciidoc.contains("[proposed"),
        "MergeReconcile proposal must contain '[proposed' marker:\n{}",
        p.patch_asciidoc
    );

    // Conflict text must NOT use AsciiDoc comment syntax (lines starting with
    // `//`).  In AsciiDoc, `//` opens a comment and is invisible in rendered
    // output, so a conflict marker beginning with `//` silently vanishes for
    // anyone viewing the rendered proposal.
    let asciidoc_comment_line = p
        .patch_asciidoc
        .lines()
        .any(|l| l.trim_start().starts_with("//"));
    assert!(
        !asciidoc_comment_line,
        "conflict marker must not use `//` comment syntax (invisible in AsciiDoc):\n{}",
        p.patch_asciidoc
    );

    // The conflict text must contain a visible `CONFLICT:` label so that
    // anyone reading the rendered proposal can see what happened.
    assert!(
        p.patch_asciidoc.contains("CONFLICT:"),
        "conflict marker must contain visible 'CONFLICT:' text:\n{}",
        p.patch_asciidoc
    );

    // The stored document must NOT have been mutated (conflict → store untouched).
    let doc_after = open_store(&data)
        .get_document(BETA_ANCHOR)
        .expect("get_document must not error")
        .expect("beta must still be in store after --fix conflict");
    assert_eq!(
        doc_before.anchor, doc_after.anchor,
        "stored document anchor must be unchanged after conflict"
    );
}

// ── test 3: apply_mergereconcile_proposal ─────────────────────────────────────

/// `heal . --propose` followed by `heal . --apply <id>` must cleanly apply
/// the MergeReconcile proposal when there is no overlay conflict.
///
/// Regression target: before Phase 5, `cmd_heal_apply` for MergeReconcile
/// printed a placeholder message and left the proposal PendingReview.
#[test]
fn apply_mergereconcile_proposal() {
    let project = unique_dir("apply-proposal");
    let data = unique_dir("apply-proposal-data");

    let src_dir = project.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(
        src_dir.join("lib.rs"),
        "/// Returns the gamma value.\npub fn gamma() -> i32 { 1 }\n",
    )
    .unwrap();

    git_init_commit(&project, "fixture: v1");
    run_gen(&project, &data);

    // No overlay → clean merge is guaranteed.

    // Mutate source → StaleHash.
    std::fs::write(
        src_dir.join("lib.rs"),
        "/// Returns the updated gamma value.\npub fn gamma() -> i32 { 2 }\n",
    )
    .unwrap();

    // --propose must write a MergeReconcile proposal.
    let propose_out = aden(&project, &data, &["heal", ".", "--propose"]);
    assert!(
        propose_out.status.success(),
        "aden heal . --propose failed:\n  stdout: {}\n  stderr: {}",
        String::from_utf8_lossy(&propose_out.stdout),
        String::from_utf8_lossy(&propose_out.stderr)
    );

    let proposals = aden_propose::list(&project).expect("list must succeed");
    let merge_proposals: Vec<_> = proposals
        .iter()
        .filter(|p| p.drift_type == "MergeReconcile")
        .collect();
    assert!(
        !merge_proposals.is_empty(),
        "no MergeReconcile proposal after --propose; all proposals: {proposals:?}"
    );

    let proposal_id = merge_proposals[0].id.clone();

    // --apply <id> must succeed and report Applied.
    let apply_out = aden(&project, &data, &["heal", ".", "--apply", &proposal_id]);
    let apply_stdout = String::from_utf8_lossy(&apply_out.stdout).to_string();
    let apply_stderr = String::from_utf8_lossy(&apply_out.stderr).to_string();
    assert!(
        apply_out.status.success(),
        "aden heal . --apply {proposal_id} failed:\n  stdout: {apply_stdout}\n  stderr: {apply_stderr}"
    );

    assert!(
        apply_stdout.contains("Applied") || apply_stdout.contains("applied"),
        "expected 'Applied' in --apply output:\n{apply_stdout}"
    );

    // The base snapshot must now reflect the new source.
    let store = open_store(&data);
    let snapshot = store
        .get_base_snapshot(GAMMA_ANCHOR)
        .expect("get_base_snapshot must not error")
        .expect("base snapshot for GAMMA_ANCHOR must exist after --apply");
    assert!(!snapshot.is_empty(), "base snapshot must not be empty");
}

// ── test 4: idempotence_no_source_change ──────────────────────────────────────

/// Running `aden heal . --fix` immediately after `aden gen .` with no source
/// changes must report zero fixes.  A second run must also produce zero fixes.
///
/// Regression: a naive implementation might try to "re-apply" an already-clean
/// store on every run.
#[test]
fn idempotence_no_source_change() {
    let project = unique_dir("idempotent");
    let data = unique_dir("idempotent-data");

    let src_dir = project.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(
        src_dir.join("lib.rs"),
        "/// Returns the delta value.\npub fn delta() -> i32 { 42 }\n",
    )
    .unwrap();

    git_init_commit(&project, "fixture");
    run_gen(&project, &data);

    // First --fix with no source change.
    let out1 = aden(&project, &data, &["heal", ".", "--fix"]);
    let stdout1 = String::from_utf8_lossy(&out1.stdout).to_string();
    assert!(
        out1.status.success(),
        "first --fix failed:\n{}",
        String::from_utf8_lossy(&out1.stderr)
    );
    assert!(
        !stdout1.contains("merge-applied"),
        "expected no 'merge-applied' after gen with no source change:\n{stdout1}"
    );

    // Second --fix — idempotent.
    let out2 = aden(&project, &data, &["heal", ".", "--fix"]);
    let stdout2 = String::from_utf8_lossy(&out2.stdout).to_string();
    assert!(
        out2.status.success(),
        "second --fix failed:\n{}",
        String::from_utf8_lossy(&out2.stderr)
    );
    assert!(
        !stdout2.contains("merge-applied"),
        "expected no 'merge-applied' on second run (idempotence):\n{stdout2}"
    );
}

// ── test 5: polyglot_python_powershell ───────────────────────────────────────

/// The gen/heal/fix cycle must complete without errors or panics for Python
/// (.py) and PowerShell (.ps1) source files, AND the store must contain
/// function-level documents for both languages after `gen`.
///
/// Anchor format notes (updated after fix(parse) unification):
/// - Python: `infer_project_name` now delegates to the shared canonical function;
///   for a git-only repo with files under `src/`, the VCS-root fallback returns
///   `"src"` (first path component under the git root). Anchor:
///   `aden://module/src/module.py#<sym>`.
/// - PowerShell: resolves to `"src"` (same logic, same result), so the anchor is
///   `aden://module/src/utils.ps1#<sym>`.
///
/// Both Python and PowerShell now agree on the project component for files under src/.
///
/// Both `compute_checksum` and `Get-ProjectMetadata` are tier-1 extractions:
/// the Python extractor and the generic tree-sitter PowerShell extractor both
/// produce function-level documents with the function name as the symbol.
#[test]
fn polyglot_python_powershell() {
    let project = unique_dir("polyglot");
    let data = unique_dir("polyglot-data");

    let src_dir = project.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    // Python source V1
    std::fs::write(
        src_dir.join("module.py"),
        "def compute_checksum(data):\n    \"\"\"Compute a checksum.\"\"\"\n    return hash(data)\n",
    )
    .unwrap();

    // PowerShell source V1. PowerShell is NOT in the build-time grammar set
    // (TSLP_LANGUAGES), so it only parses when `grammars-download` can fetch the
    // grammar (or a prior download seeded the on-disk cache). Gate every
    // PowerShell-specific step behind that feature so the default, network-free
    // `cargo test` stays deterministic on a fresh CI — mirroring the gating on
    // `powershell_generic_smoke` in aden-parse.
    #[cfg(feature = "grammars-download")]
    std::fs::write(
        src_dir.join("utils.ps1"),
        "function Get-ProjectMetadata {\n    <#\n    .SYNOPSIS\n    Returns project metadata.\n    #>\n    return @{ version = '1.0' }\n}\n",
    )
    .unwrap();

    git_init_commit(&project, "fixture: v1");
    run_gen(&project, &data);

    // ── Store content assertions (Task 1: strengthen from exit-0-only) ────────
    //
    // After gen, the store must contain at least one document per language.
    // We assert against the real anchor strings discovered by red-green probing.
    {
        let store = open_store(&data);
        let all_anchors = store
            .get_all_anchors()
            .expect("get_all_anchors must succeed");

        // Python: `compute_checksum` function must be extracted and stored.
        // After fix(parse): Python now uses the shared infer_project_name, which for a
        // git-only repo returns "src" (first path component under .git root). Anchor
        // changed from "unknown" to "src" — both Python and PowerShell now agree.
        let py_anchor = "aden://module/src/module.py#compute_checksum";
        assert!(
            all_anchors.iter().any(|a| a.contains("compute_checksum")),
            "expected an anchor containing 'compute_checksum' in the store after gen;\n\
             all anchors: {all_anchors:#?}"
        );
        let py_doc = store
            .get_document(py_anchor)
            .expect("get_document must not error for Python anchor")
            .unwrap_or_else(|| {
                // If the exact anchor differs from our probe, surface all matching anchors.
                let matches: Vec<_> = all_anchors
                    .iter()
                    .filter(|a| a.contains("compute_checksum"))
                    .collect();
                panic!(
                    "Python document at '{py_anchor}' not found;\n\
                     anchors containing 'compute_checksum': {matches:?}"
                )
            });
        assert_eq!(
            py_doc.anchor, py_anchor,
            "Python document anchor must match expected value"
        );

        // PowerShell: `Get-ProjectMetadata` function must be extracted and stored.
        // The PowerShell extractor (generic tree-sitter) resolves project name as "src".
        // Only asserted under `grammars-download` — the slim default build cannot
        // load the PowerShell grammar, so the .ps1 above isn't even written there.
        #[cfg(feature = "grammars-download")]
        {
            let ps1_anchor = "aden://module/src/utils.ps1#Get-ProjectMetadata";
            assert!(
                all_anchors
                    .iter()
                    .any(|a| a.contains("Get-ProjectMetadata")),
                "expected an anchor containing 'Get-ProjectMetadata' in the store after gen;\n\
                 all anchors: {all_anchors:#?}"
            );
            let ps1_doc = store
                .get_document(ps1_anchor)
                .expect("get_document must not error for PowerShell anchor")
                .unwrap_or_else(|| {
                    let matches: Vec<_> = all_anchors
                        .iter()
                        .filter(|a| a.contains("Get-ProjectMetadata"))
                        .collect();
                    panic!(
                        "PowerShell document at '{ps1_anchor}' not found;\n\
                         anchors containing 'Get-ProjectMetadata': {matches:?}"
                    )
                });
            assert_eq!(
                ps1_doc.anchor, ps1_anchor,
                "PowerShell document anchor must match expected value"
            );
        }
    }

    // Mutate both source files — doc comment changes.
    std::fs::write(
        src_dir.join("module.py"),
        "def compute_checksum(data):\n    \"\"\"Compute a fast checksum of input data.\"\"\"\n    return abs(hash(data))\n",
    )
    .unwrap();
    #[cfg(feature = "grammars-download")]
    std::fs::write(
        src_dir.join("utils.ps1"),
        "function Get-ProjectMetadata {\n    <#\n    .SYNOPSIS\n    Returns current project metadata.\n    #>\n    return @{ version = '1.1' }\n}\n",
    )
    .unwrap();

    // gen/heal/fix cycle must not panic or return a non-zero exit code.
    let fix_out = aden(&project, &data, &["heal", ".", "--fix"]);
    let fix_stdout = String::from_utf8_lossy(&fix_out.stdout).to_string();
    let fix_stderr = String::from_utf8_lossy(&fix_out.stderr).to_string();
    assert!(
        fix_out.status.success(),
        "aden heal . --fix failed for polyglot project:\n  stdout: {fix_stdout}\n  stderr: {fix_stderr}"
    );

    // No panics in stderr.
    for line in fix_stderr.lines() {
        let lower = line.to_ascii_lowercase();
        assert!(
            !lower.starts_with("thread") || !lower.contains("panic"),
            "panic detected in stderr:\n{fix_stderr}"
        );
    }

    // A second run must also complete cleanly.
    let fix_out2 = aden(&project, &data, &["heal", ".", "--fix"]);
    assert!(
        fix_out2.status.success(),
        "second aden heal . --fix failed for polyglot:\n  stdout: {}\n  stderr: {}",
        String::from_utf8_lossy(&fix_out2.stdout),
        String::from_utf8_lossy(&fix_out2.stderr)
    );
}

// ── test 6: apply_mergereconcile_proposal_python ─────────────────────────────

/// `heal . --propose` followed by `heal . --apply <id>` must produce a
/// well-formed `MergeReconcile` proposal for a **Python** source file.
///
/// Regression target: before the fix, Python docstrings were silently dropped
/// by `extract_preceding_docstring` (it checked for an `expression_statement`
/// wrapper that the tree-sitter-python grammar does not emit for triple-quoted
/// docstrings). This caused ground == base in `reconcile_contract` (both lacked
/// the docstring block), so `propose()` emitted zero actions → `generate_merge_proposal`
/// returned `None` → the legacy fallback produced a degenerate file containing only
/// `:source_hash: <hex>` with a `pid-nanos` filename that `aden_propose::list()`
/// rejected as missing its `[[id]]` anchor.
///
/// Anchor note: with the unified `infer_project_name`, `src/module.py` in a
/// git-only repo resolves to project `"src"`, so the anchor is
/// `aden://module/src/module.py#compute_checksum` (the test discovers it
/// dynamically from the store either way).
#[test]
fn apply_mergereconcile_proposal_python() {
    let project = unique_dir("apply-proposal-py");
    let data = unique_dir("apply-proposal-py-data");

    let src_dir = project.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    // Python source V1 — with a triple-quoted docstring.
    std::fs::write(
        src_dir.join("module.py"),
        "def compute_checksum(data):\n    \"\"\"Compute a checksum.\"\"\"\n    return hash(data)\n",
    )
    .unwrap();

    git_init_commit(&project, "fixture: v1");
    run_gen(&project, &data);

    // Inspect the anchor the Python extractor produced.
    let py_anchor = {
        let store = open_store(&data);
        let all = store
            .get_all_anchors()
            .expect("get_all_anchors must succeed");
        all.into_iter()
            .find(|a| a.contains("compute_checksum"))
            .unwrap_or_else(|| panic!("compute_checksum anchor not found in store after gen"))
    };

    // Mutate: change the docstring → StaleHash for the Python anchor.
    std::fs::write(
        src_dir.join("module.py"),
        "def compute_checksum(data):\n    \"\"\"Compute a fast checksum of input data.\"\"\"\n    return abs(hash(data))\n",
    )
    .unwrap();

    // --propose must write a well-formed MergeReconcile proposal (not the
    // degenerate `:source_hash:` stub from the legacy fallback).
    let propose_out = aden(&project, &data, &["heal", ".", "--propose"]);
    let propose_stdout = String::from_utf8_lossy(&propose_out.stdout).to_string();
    let propose_stderr = String::from_utf8_lossy(&propose_out.stderr).to_string();
    assert!(
        propose_out.status.success(),
        "aden heal . --propose failed for Python:\n  stdout: {propose_stdout}\n  stderr: {propose_stderr}"
    );

    // aden_propose::list() must see at least one proposal with drift_type=MergeReconcile.
    let proposals = aden_propose::list(&project).expect("list must succeed");
    let merge_proposals: Vec<_> = proposals
        .iter()
        .filter(|p| p.drift_type == "MergeReconcile")
        .collect();
    assert!(
        !merge_proposals.is_empty(),
        "expected MergeReconcile proposal for Python anchor; all proposals: {proposals:?}\n\
         Regression: Python docstrings were silently dropped so ground==base → zero actions → legacy fallback"
    );

    // The proposal target must reference the Python anchor.
    let p = merge_proposals[0];
    assert!(
        p.target_path.to_string_lossy().contains("compute_checksum"),
        "proposal target must reference compute_checksum; got: {}",
        p.target_path.display()
    );

    // The proposal must have a semantic [[id]] anchor (not a pid-nanos fallback).
    assert!(
        p.patch_asciidoc.contains("[[merge-"),
        "proposal patch_asciidoc must contain [[merge-<slug>]] anchor;\n\
         got patch_asciidoc:\n{}",
        &p.patch_asciidoc[..p.patch_asciidoc.len().min(300)]
    );

    let proposal_id = p.id.clone();

    // --apply <id> must succeed and report Applied.
    let apply_out = aden(&project, &data, &["heal", ".", "--apply", &proposal_id]);
    let apply_stdout = String::from_utf8_lossy(&apply_out.stdout).to_string();
    let apply_stderr = String::from_utf8_lossy(&apply_out.stderr).to_string();
    assert!(
        apply_out.status.success(),
        "aden heal . --apply {proposal_id} failed for Python:\n  stdout: {apply_stdout}\n  stderr: {apply_stderr}"
    );
    assert!(
        apply_stdout.contains("Applied") || apply_stdout.contains("applied"),
        "expected 'Applied' in --apply output:\n{apply_stdout}"
    );

    // The base snapshot must now exist and be non-empty.
    let store = open_store(&data);
    let snapshot = store
        .get_base_snapshot(&py_anchor)
        .expect("get_base_snapshot must not error")
        .expect("base snapshot must exist after --apply");
    assert!(
        !snapshot.is_empty(),
        "base snapshot for Python anchor must not be empty after --apply"
    );

    // The snapshot must reflect the NEW docstring content.
    assert!(
        snapshot.contains("fast checksum"),
        "base snapshot must contain the updated docstring after --apply;\n\
         got snapshot:\n{snapshot}"
    );
}
