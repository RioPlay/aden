// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Regression coverage for `query` hop limits across every traversal mode.

use std::path::{Path, PathBuf};
use std::process::Command;

const CHAIN_RS: &str = r#"
pub fn leaf() {}

pub fn middle() {
    leaf();
}

pub fn root() {
    middle();
}
"#;

fn unique_dir(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "aden-query-depth-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn git(project: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn scaffold() -> (PathBuf, PathBuf) {
    let project = unique_dir("project");
    let data = unique_dir("data");
    std::fs::write(project.join("chain.rs"), CHAIN_RS).unwrap();
    git(&project, &["init", "-q"]);
    git(
        &project,
        &["config", "user.email", "query-depth@test.invalid"],
    );
    git(&project, &["config", "user.name", "Query depth test"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-qm", "fixture"]);
    (project, data)
}

fn query(project: &Path, data: &Path, mode: &str, anchor: &str, depth: usize) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["query", "--format", "json", mode, anchor, "--depth"])
        .arg(depth.to_string())
        .arg(project)
        .env("ADEN_DATA_DIR", data)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "query {mode} {anchor} at depth {depth} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn bounded_query(
    project: &Path,
    data: &Path,
    mode: &str,
    anchor: &str,
    depth: usize,
    max_results: usize,
) -> serde_json::Value {
    let max_results = max_results.to_string();
    let output = Command::new(env!("CARGO_BIN_EXE_aden"))
        .arg("query")
        .args(["--max-results", &max_results])
        .args(["--format", "json", mode, anchor, "--depth"])
        .arg(depth.to_string())
        .arg(project)
        .env("ADEN_DATA_DIR", data)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "bounded query {mode} {anchor} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn anchors(result: &serde_json::Value) -> Vec<String> {
    result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["anchor"].as_str().unwrap().to_string())
        .collect()
}

fn depths(result: &serde_json::Value) -> Vec<u64> {
    result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["depth"].as_u64().unwrap())
        .collect()
}

#[test]
fn query_depth_bounds_from_backlinks_and_impact() {
    let (project, data) = scaffold();

    for (mode, anchor, distant) in [
        ("--from", "root", "leaf"),
        ("--impact", "root", "leaf"),
        ("--backlinks", "leaf", "root"),
    ] {
        let zero_result = query(&project, &data, mode, anchor, 0);
        let zero = anchors(&zero_result);
        assert_eq!(
            zero.len(),
            1,
            "{mode} must not traverse at depth zero: {zero:?}"
        );
        assert!(zero[0].ends_with(&format!("#{anchor}")));
        assert!(depths(&zero_result).iter().all(|depth| *depth == 0));

        let one_result = query(&project, &data, mode, anchor, 1);
        let one = anchors(&one_result);
        assert!(
            depths(&one_result).iter().all(|depth| *depth <= 1),
            "{mode} emitted a node beyond depth one: {one:?}"
        );
        assert!(
            !one.iter()
                .any(|item| item.ends_with(&format!("#{distant}"))),
            "{mode} reached two hops at depth one: {one:?}"
        );

        let two_result = query(&project, &data, mode, anchor, 2);
        let two = anchors(&two_result);
        assert!(depths(&two_result).iter().all(|depth| *depth <= 2));
        assert!(
            two.iter()
                .any(|item| item.ends_with(&format!("#{distant}"))),
            "{mode} did not reach the second hop at depth two: {two:?}"
        );
        if mode == "--backlinks" {
            assert!(
                !two.iter().any(|item| item.starts_with("mod-")),
                "default backlinks leaked structural containers: {two:?}"
            );
        }
    }
}

#[test]
fn query_max_results_bounds_every_mode_and_reports_truncation() {
    let (project, data) = scaffold();

    for (mode, anchor) in [
        ("--from", "root"),
        ("--impact", "root"),
        ("--backlinks", "leaf"),
    ] {
        let result = bounded_query(&project, &data, mode, anchor, 2, 1);
        assert_eq!(result["items"].as_array().unwrap().len(), 1, "{mode}");
        assert_eq!(result["returned"], 1, "{mode}");
        assert_eq!(result["limit"], 1, "{mode}");
        assert_eq!(result["truncated"], true, "{mode}");
        assert_eq!(result["result_state"], "truncated", "{mode}");
    }
}
