// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! AP-107: a cross-interface adversarial gate for the Trust Foundation.
//!
//! This intentionally uses the public CLI. The paired live rmcp duplex test in
//! `aden-mcp` owns the production transport assertion; together they pin that
//! trust evidence remains truthful when the failure modes coexist.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn unique_dir(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "aden-trust-gate-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).expect("fixture directory");
    path
}

fn run(project: &Path, data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(args)
        .current_dir(project)
        .env("ADEN_DATA_DIR", data)
        .output()
        .expect("aden runs")
}

fn run_with_env(project: &Path, data: &Path, args: &[&str], key: &str, value: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(args)
        .current_dir(project)
        .env("ADEN_DATA_DIR", data)
        .env(key, value)
        .output()
        .expect("aden runs")
}

fn json(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "aden failed (status {:?}): stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("public JSON envelope")
}

fn coverage_total(coverage: &serde_json::Value) -> u64 {
    coverage
        .as_object()
        .expect("coverage object")
        .values()
        .map(|value| value.as_u64().expect("coverage count"))
        .sum()
}

#[test]
fn adversarial_cli_and_mcp_evidence_remains_truthful_together() {
    let project = unique_dir("project");
    let data = unique_dir("data");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("README.adoc"), "[[root]]\n= Trust fixture\n").unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn visible_symbol() {}\n").unwrap();
    // Build the credential-like value at runtime so this test source itself is
    // never an accidental secret-scanning fixture.
    let token = String::from_utf8_lossy(&[
        103, 104, 112, 95, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 97, 98, 99, 100, 101, 102, 103,
        104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 117, 118, 119, 120, 121,
        122,
    ]);
    std::fs::write(
        project.join("credential_fixture.rs"),
        format!("const TOKEN: &str = \"{token}\";\n"),
    )
    .unwrap();
    std::fs::write(
        project.join("unsupported.xyz"),
        "not a supported language\n",
    )
    .unwrap();
    std::fs::write(
        project.join("graph.rs"),
        "pub struct AdenGraph<N, E> { n: std::marker::PhantomData<(N, E)> }\nimpl<N, E> AdenGraph<N, E> { pub fn bfs(&self) {} }\n",
    )
    .unwrap();
    std::fs::write(
        project.join("other.rs"),
        "pub struct AdenGraph<T, U> { n: std::marker::PhantomData<(T, U)> }\nimpl<T, U> AdenGraph<T, U> { pub fn bfs(&self) {} }\n",
    )
    .unwrap();

    assert!(run(&project, &data, &["gen", "."]).status.success());
    let status = json(&run(&project, &data, &["status", ".", "--json"]));
    let coverage = &status["coverage"];
    assert!(coverage["indexed"].as_u64().unwrap_or(0) >= 2, "{status}");
    assert_eq!(coverage["unsupported"], 1, "{status}");
    assert_eq!(coverage["secret_content"], 1, "{status}");
    let cache_path = PathBuf::from(status["store"].as_str().expect("status store path"))
        .parent()
        .expect("project cache directory")
        .join("gen-cache.json");
    let cache: serde_json::Value =
        serde_json::from_slice(&std::fs::read(cache_path).expect("persisted disposition cache"))
            .expect("cache JSON");
    let discovered_universe = cache["entries"].as_object().unwrap().len()
        + cache["dispositions"].as_object().unwrap().len();
    assert_eq!(
        coverage_total(coverage) as usize,
        discovered_universe,
        "every discovered file must have exactly one persisted disposition: {status}"
    );

    // DX-101 remains natural at the public boundary: generic spelling and
    // whitespace resolve, while equally good candidates stay explicit.
    let generic = json(&run(
        &project,
        &data,
        &[
            "locate",
            "--json",
            "--symbol",
            "AdenGraph < N, E > :: bfs",
            ".",
        ],
    ));
    assert!(generic.to_string().contains("bfs"), "{generic}");
    let ambiguous = json(&run(
        &project,
        &data,
        &["understand", "--json", "AdenGraph::bfs", "."],
    ));
    assert_eq!(ambiguous["resolution"]["state"], "ambiguous", "{ambiguous}");
    assert!(
        ambiguous["resolution"]["candidates"]
            .as_array()
            .unwrap()
            .len()
            >= 2
    );

    // A public strict response must retain its receipt while being bounded.
    let project_arg = project.to_string_lossy().into_owned();
    let cli = run(
        &project,
        &data,
        &[
            "ask",
            "visible_symbol",
            "--strict",
            "--budget",
            "15",
            &project_arg,
        ],
    );
    let cli_value = json(&cli);
    assert_eq!(cli_value["context_receipt"]["schema_version"], 1);
    assert_eq!(
        cli_value["incomplete"], true,
        "tiny strict response: {cli_value}"
    );
    assert!(
        cli.stdout.len().div_ceil(4) <= 15,
        "strict budget must include every serialized byte: {}",
        String::from_utf8_lossy(&cli.stdout)
    );
    assert!(
        !String::from_utf8_lossy(&cli.stdout).contains(&*token),
        "credential-like content escaped the CLI receipt: {}",
        String::from_utf8_lossy(&cli.stdout)
    );

    let clean = json(&run(&project, &data, &["--json", "audit", "."]));
    assert_eq!(clean["result"]["outcome"], "clean");

    // A forced stale read cannot assert authoritative currentness, and a
    // normal public read must then recover to a current, receipt-bearing view.
    std::fs::write(project.join("src/lib.rs"), "pub fn changed_symbol() {}\n").unwrap();
    let stale = run_with_env(
        &project,
        &data,
        &["--require-fresh", "grep", "changed_symbol", ".", "--json"],
        "ADEN_SKIP_AUTO_GEN",
        "1",
    );
    assert_eq!(stale.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&stale.stderr).contains("authoritative freshness required"));
    let recovered = json(&run(
        &project,
        &data,
        &["--require-fresh", "grep", "changed_symbol", ".", "--json"],
    ));
    assert_eq!(recovered["freshness"], "current");
    assert_eq!(recovered["context_receipt"]["freshness"], "current");

    // Mutation while a read refreshes is explicitly degraded; it must never
    // become a false `current` claim.
    std::fs::write(project.join("src/lib.rs"), "pub fn mutation_target() {}\n").unwrap();
    let mutating = run_with_env(
        &project,
        &data,
        &["grep", "mutation_target", ".", "--json"],
        "ADEN_TEST_MUTATE_DURING_GEN",
        "src/lib.rs",
    );
    let mutating = json(&mutating);
    assert_ne!(mutating["freshness"], "current", "{mutating}");
    assert_eq!(mutating["index_stale"], true, "{mutating}");

    // Advisory security findings retain their documented non-blocking exit,
    // but the result contract must visibly distinguish them from a clean run.
    std::fs::write(project.join("advisory.py"), "exec(user_input)\n").unwrap();
    let advisory = json(&run(&project, &data, &["--json", "audit", "."]));
    assert_eq!(advisory["result"]["outcome"], "passed_with_findings");
    assert!(
        advisory["result"]["advisory_findings"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );
    let blocked = run(&project, &data, &["--json", "audit", ".", "--strict"]);
    assert!(!blocked.status.success(), "strict findings must block");
    let blocked: serde_json::Value = serde_json::from_slice(&blocked.stdout).expect("blocked JSON");
    assert_eq!(blocked["result"]["outcome"], "blocked");
}
