// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wave 3 graph-type activation evals (graph-type-roadmap.adoc, episodic layer):
//!
//! 1. `Supersedes` edges — a prose cross-reference on a line with supersede
//!    language becomes a directed NEW —Supersedes→ OLD edge. Both phrasings
//!    resolve to the same direction: "Superseded by <<new>>" (passive, in the
//!    old doc) and "supersedes <<old>>" (active, in the new doc).
//! 2. `Justifies` edges — an ADR doc whose prose mentions a code symbol
//!    co-emits ADR —Justifies→ symbol alongside the weaker `Mentions`
//!    (reclassification, the Tests-alongside-Calls pattern). Non-ADR docs
//!    must NOT emit Justifies.
//! 3. `AssociatedWith` edges — files that change together in git history
//!    (≥3 co-commits, bulk commits skipped) get bidirectional module-level
//!    AssociatedWith edges: the Hebbian "what else changes with this" signal.
//!
//! Polyglot by construction: supersede capture is asserted in BOTH AsciiDoc
//! and Markdown; resolution is format-neutral in `link_store_edges`.

use std::path::{Path, PathBuf};
use std::process::Command;

const CHAIN_RS: &str = r#"
pub fn helper_fn(word: &str) -> String {
    format!("{word}!")
}

pub fn caller_fn() -> String {
    helper_fn("hello")
}
"#;

const ALPHA_RS: &str = "pub fn alpha_fn() -> u8 { 1 }\n";
const BETA_RS: &str = "pub fn beta_fn() -> u8 { 2 }\n";
const GAMMA_RS: &str = "pub fn gamma_fn() -> u8 { 3 }\n";

/// Old ADR: passive supersede phrasing ("Superseded by <<adr-002>>") — the
/// REFERENCED doc supersedes THIS one.
const ADR1_ADOC: &str = r#":status: superseded

[[adr-001]]
= ADR-001: Old Decision

== Status

Superseded by <<adr-002>>.
"#;

/// New ADR: active phrasing ("supersedes <<adr-001>>") — THIS doc supersedes
/// the referenced one. Also mentions `helper_fn` for the Justifies eval.
const ADR2_ADOC: &str = r#":status: accepted

[[adr-002]]
= ADR-002: New Decision

The `helper_fn` routine is the decided approach.

== Relation

This ADR supersedes <<adr-001>> in full.
"#;

/// Non-ADR doc mentioning the same symbol: must stay Mentions-only.
/// The plain link line deliberately carries no supersede language — that is
/// exactly what `plain_refs_stay_relates_to` asserts.
const README_MD: &str = r#"# Guide

The `helper_fn` routine renders a greeting.

A plain reference to [the old decision](#old-decision) — an ordinary link.
"#;

/// Markdown supersede phrasing, both directions, same file.
const NOTES_MD: &str = r#"# Notes

## Old decision

Superseded by [the new decision](#new-decision).

## New decision

This supersedes [the old decision](#old-decision).
"#;

fn unique_dir(label: &str) -> PathBuf {
    // pid+nanos alone collides on macOS (µs clock granularity) when parallel
    // test threads enter in the same tick; the counter disambiguates.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "aden-wave3-{label}-{}-{}-{}",
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

/// Append a line to a file and commit, so the file shows up in that commit.
fn touch_and_commit(project: &Path, files: &[&str], msg: &str) {
    for f in files {
        let p = project.join(f);
        let mut content = std::fs::read_to_string(&p).unwrap_or_default();
        content.push_str("// touched\n");
        std::fs::write(&p, content).unwrap();
    }
    git(project, &["add", "-A"]);
    git(project, &["commit", "-q", "-m", msg]);
}

fn scaffold() -> (PathBuf, PathBuf) {
    let project = unique_dir("proj");
    let data = unique_dir("data");
    std::fs::write(project.join("chain.rs"), CHAIN_RS).unwrap();
    std::fs::write(project.join("alpha.rs"), ALPHA_RS).unwrap();
    std::fs::write(project.join("beta.rs"), BETA_RS).unwrap();
    std::fs::write(project.join("gamma.rs"), GAMMA_RS).unwrap();
    std::fs::write(project.join("adr-001-old.adoc"), ADR1_ADOC).unwrap();
    std::fs::write(project.join("adr-002-new.adoc"), ADR2_ADOC).unwrap();
    std::fs::write(project.join("README.md"), README_MD).unwrap();
    std::fs::write(project.join("notes.md"), NOTES_MD).unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["config", "user.email", "wave3@test.invalid"]);
    git(&project, &["config", "user.name", "Wave3 Test"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "fixture"]);
    // Co-change history: alpha+beta co-committed 3 more times (4 total with the
    // fixture commit — over the ≥3 threshold); gamma rides along only once
    // (2 total — under it).
    touch_and_commit(&project, &["alpha.rs", "beta.rs", "gamma.rs"], "co 1");
    touch_and_commit(&project, &["alpha.rs", "beta.rs"], "co 2");
    touch_and_commit(&project, &["alpha.rs", "beta.rs"], "co 3");
    (project, data)
}

/// Census via `aden viz --mode graph --full -j` (same surface as wave2 evals).
fn census_edges(project: &Path, data: &Path) -> Vec<(String, String, String)> {
    let out = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["viz", "--mode", "graph", "--full", "--format", "json"])
        .current_dir(project)
        .env("ADEN_DATA_DIR", data)
        .output()
        .expect("aden binary must run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "viz --mode graph failed.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("viz --mode graph must emit JSON");
    let by_id: std::collections::HashMap<String, String> = json["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .map(|n| {
            (
                n["id"].as_str().unwrap_or_default().to_string(),
                n["anchor"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    json["edges"]
        .as_array()
        .expect("edges array")
        .iter()
        .map(|e| {
            (
                by_id[e["from"].as_str().unwrap_or_default()].clone(),
                by_id[e["to"].as_str().unwrap_or_default()].clone(),
                e["type"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn edges_of_type<'a>(
    edges: &'a [(String, String, String)],
    ty: &str,
) -> Vec<&'a (String, String, String)> {
    edges.iter().filter(|(_, _, t)| t == ty).collect()
}

/// Eval (Supersedes, AsciiDoc, passive): "Superseded by <<adr-002>>" inside
/// adr-001 must yield NEW —Supersedes→ OLD (from adr-002, to adr-001) and
/// nothing in the reverse direction.
#[test]
fn asciidoc_superseded_by_points_new_to_old() {
    let (project, data) = scaffold();
    let edges = census_edges(&project, &data);
    let sup = edges_of_type(&edges, "Supersedes");
    assert!(
        sup.iter()
            .any(|(f, t, _)| f.contains("adr-002") && t.contains("adr-001-old")),
        "adr-002 must Supersede adr-001 (from the passive 'Superseded by' line); got: {sup:?}"
    );
    assert!(
        !sup.iter()
            .any(|(f, t, _)| f.contains("adr-001-old") && t.contains("adr-002")),
        "the OLD doc must never supersede the NEW one; got: {sup:?}"
    );
}

/// Eval (Supersedes, AsciiDoc, active): "This ADR supersedes <<adr-001>>"
/// inside adr-002 must also yield NEW —Supersedes→ OLD.
#[test]
fn asciidoc_supersedes_points_new_to_old() {
    let (project, data) = scaffold();
    let edges = census_edges(&project, &data);
    let sup = edges_of_type(&edges, "Supersedes");
    assert!(
        sup.iter()
            .any(|(f, t, _)| f.contains("adr-002-new") && t.contains("adr-001")),
        "adr-002's active 'supersedes' line must emit adr-002 —Supersedes→ adr-001; got: {sup:?}"
    );
}

/// Eval (Supersedes, Markdown): both phrasings in notes.md must converge on
/// new-decision —Supersedes→ old-decision.
#[test]
fn markdown_supersede_both_directions() {
    let (project, data) = scaffold();
    let edges = census_edges(&project, &data);
    let sup = edges_of_type(&edges, "Supersedes");
    assert!(
        sup.iter().any(|(f, t, _)| f.contains("notes.md")
            && f.to_lowercase().contains("new-decision")
            && t.to_lowercase().contains("old-decision")),
        "notes.md new-decision must Supersede old-decision; got: {sup:?}"
    );
    assert!(
        !sup.iter()
            .any(|(f, t, _)| f.to_lowercase().contains("old-decision")
                && t.to_lowercase().contains("new-decision")),
        "old-decision must never supersede new-decision; got: {sup:?}"
    );
}

/// Eval (Supersedes, precision): a plain cross-reference with no supersede
/// language must NOT emit Supersedes (it stays RelatesTo).
#[test]
fn plain_refs_stay_relates_to() {
    let (project, data) = scaffold();
    let edges = census_edges(&project, &data);
    let sup = edges_of_type(&edges, "Supersedes");
    assert!(
        !sup.iter().any(|(f, _, _)| f.contains("README.md")),
        "README's plain ref has no supersede language and must not emit Supersedes; got: {sup:?}"
    );
}

/// Eval (Justifies): the ADR doc's backtick mention of `helper_fn` must
/// co-emit ADR —Justifies→ helper_fn alongside Mentions.
#[test]
fn adr_mention_co_emits_justifies() {
    let (project, data) = scaffold();
    let edges = census_edges(&project, &data);
    let just = edges_of_type(&edges, "Justifies");
    assert!(
        just.iter()
            .any(|(f, t, _)| f.contains("adr-002") && t.ends_with("#helper_fn")),
        "adr-002 mentions helper_fn and must Justify it; got: {just:?}"
    );
    let mentions = edges_of_type(&edges, "Mentions");
    assert!(
        mentions
            .iter()
            .any(|(f, t, _)| f.contains("adr-002") && t.ends_with("#helper_fn")),
        "the Mentions edge must be kept alongside Justifies; got: {mentions:?}"
    );
}

/// Eval (Justifies, precision): a non-ADR doc's mention must NOT emit
/// Justifies — only ADRs make decisions.
#[test]
fn non_adr_mention_does_not_justify() {
    let (project, data) = scaffold();
    let edges = census_edges(&project, &data);
    let just = edges_of_type(&edges, "Justifies");
    assert!(
        !just.iter().any(|(f, _, _)| f.contains("README.md")),
        "README is not an ADR; its mentions must not emit Justifies; got: {just:?}"
    );
}

/// Eval (AssociatedWith): alpha.rs and beta.rs co-changed in ≥3 commits must
/// get bidirectional module-level AssociatedWith edges; gamma.rs (2 co-commits)
/// must not.
#[test]
fn cochanged_files_get_associated_with() {
    let (project, data) = scaffold();
    let edges = census_edges(&project, &data);
    let assoc = edges_of_type(&edges, "AssociatedWith");
    assert!(
        assoc
            .iter()
            .any(|(f, t, _)| f.contains("alpha.rs") && t.contains("beta.rs")),
        "alpha.rs and beta.rs co-changed 4x and must be AssociatedWith; got: {assoc:?}"
    );
    assert!(
        assoc
            .iter()
            .any(|(f, t, _)| f.contains("beta.rs") && t.contains("alpha.rs")),
        "AssociatedWith must be bidirectional; got: {assoc:?}"
    );
    assert!(
        !assoc.iter().any(
            |(f, t, _)| (f.contains("gamma.rs") && t.contains("alpha.rs"))
                || (f.contains("alpha.rs") && t.contains("gamma.rs"))
        ),
        "gamma.rs co-changed only 2x (below the ≥3 threshold) and must stay unlinked; got: {assoc:?}"
    );
}
