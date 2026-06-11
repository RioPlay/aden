// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Direction evals for `aden viz` anchor-centred slices (ADR-007 §2, issue H4):
//!
//! * `--mode blast` must traverse INCOMING impact edges — the dependents at
//!   risk if the anchor changes — agreeing with `impact-diff`. (It used to
//!   walk outgoing dependencies while *claiming* to mirror impact-diff.)
//! * `--mode reach` keeps the outgoing dependencies view, agreeing with
//!   `query --impact`.
//! * Rendered edges keep their stored caller→callee orientation in both modes.
//!
//! These run the freshly-built binary (`CARGO_BIN_EXE_aden`) against a
//! hermetic git fixture with an isolated `ADEN_DATA_DIR`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Fixture: `caller_fn` calls `helper_fn`; nothing calls `caller_fn`.
const CHAIN_RS: &str = r#"
pub fn helper_fn(word: &str) -> String {
    format!("{word}!")
}

pub fn caller_fn() -> String {
    helper_fn("hello")
}
"#;

fn unique_dir(label: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "aden-vizdir-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
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

fn scaffold() -> (PathBuf, PathBuf) {
    let project = unique_dir("proj");
    let data = unique_dir("data");
    std::fs::write(project.join("chain.rs"), CHAIN_RS).unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["config", "user.email", "vizdir@test.invalid"]);
    git(&project, &["config", "user.name", "VizDir Test"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "fixture"]);
    (project, data)
}

/// Run `aden viz <anchor> --mode <mode> -j <project>` and parse the JSON.
fn viz_json(project: &Path, data: &Path, anchor: &str, mode: &str) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["viz", anchor, "--mode", mode, "--format", "json"])
        .arg(project)
        .env("ADEN_DATA_DIR", data)
        .output()
        .expect("aden binary must run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "viz --mode {mode} failed.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("viz --format json must emit JSON ({e}); got: {stdout}"))
}

/// All node anchors in the slice.
fn node_anchors(json: &serde_json::Value) -> Vec<String> {
    json["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .map(|n| n["anchor"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Rendered edges as (from_anchor, to_anchor, type), resolving node ids.
fn edge_anchors(json: &serde_json::Value) -> Vec<(String, String, String)> {
    let by_id: std::collections::HashMap<&str, &str> = json["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .map(|n| {
            (
                n["id"].as_str().unwrap_or_default(),
                n["anchor"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    json["edges"]
        .as_array()
        .expect("edges array")
        .iter()
        .map(|e| {
            (
                by_id[e["from"].as_str().unwrap_or_default()].to_string(),
                by_id[e["to"].as_str().unwrap_or_default()].to_string(),
                e["type"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Eval (H4): blast of the callee must include its caller — the dependent at
/// risk — and the rendered edge must keep the stored caller→callee direction.
#[test]
fn blast_walks_incoming_dependents() {
    let (project, data) = scaffold();
    let json = viz_json(&project, &data, "helper_fn", "blast");
    let nodes = node_anchors(&json);
    assert!(
        nodes.iter().any(|a| a.ends_with("#caller_fn")),
        "blast of helper_fn must include its caller caller_fn; got: {nodes:?}"
    );
    let edges = edge_anchors(&json);
    assert!(
        edges.iter().any(|(f, t, ty)| f.ends_with("#caller_fn")
            && t.ends_with("#helper_fn")
            && ty == "Calls"),
        "edge must keep stored caller→callee orientation; got: {edges:?}"
    );
}

/// Eval (H4, negative): blast of the CALLER must not include its callee — a
/// dependency is not a dependent. This is exactly the direction the old
/// outgoing traversal got backwards.
#[test]
fn blast_excludes_dependencies() {
    let (project, data) = scaffold();
    let json = viz_json(&project, &data, "caller_fn", "blast");
    let nodes = node_anchors(&json);
    assert!(
        !nodes.iter().any(|a| a.ends_with("#helper_fn")),
        "blast of caller_fn must NOT include its callee helper_fn (that is reach); got: {nodes:?}"
    );
}

/// Guard (positional trap): a directory path in the ANCHOR position must be
/// an error with guidance, not a silent census of the CWD — `aden viz <path>
/// --mode graph` used to ignore the misplaced "anchor" and visualize whatever
/// directory the command happened to run from.
#[test]
fn directory_in_anchor_position_errors_with_guidance() {
    let (project, data) = scaffold();
    let out = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["viz"])
        .arg(&project) // the foot-gun: project path as the first positional
        .args(["--mode", "graph", "--format", "json"])
        .env("ADEN_DATA_DIR", &data)
        .output()
        .expect("aden binary must run");
    assert!(
        !out.status.success(),
        "a directory in ANCHOR position must be rejected, not silently visualize the cwd"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("directory") && stderr.contains("ANCHOR"),
        "error must explain the positional order; got: {stderr}"
    );
}

/// Eval: reach of the caller is the outgoing dependencies view (`query
/// --impact` semantics) and must include the callee.
#[test]
fn reach_walks_outgoing_dependencies() {
    let (project, data) = scaffold();
    let json = viz_json(&project, &data, "caller_fn", "reach");
    let nodes = node_anchors(&json);
    assert!(
        nodes.iter().any(|a| a.ends_with("#helper_fn")),
        "reach of caller_fn must include its callee helper_fn; got: {nodes:?}"
    );
    let edges = edge_anchors(&json);
    assert!(
        edges.iter().any(|(f, t, ty)| f.ends_with("#caller_fn")
            && t.ends_with("#helper_fn")
            && ty == "Calls"),
        "reach edge must keep stored caller→callee orientation; got: {edges:?}"
    );
}
