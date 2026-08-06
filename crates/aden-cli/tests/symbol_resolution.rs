// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static FIXTURE_ID: AtomicUsize = AtomicUsize::new(0);

fn fixture() -> (std::path::PathBuf, std::path::PathBuf) {
    let id = FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "aden-symbol-resolution-{}-{id}",
        std::process::id()
    ));
    let data = std::env::temp_dir().join(format!(
        "aden-symbol-resolution-data-{}-{id}",
        std::process::id(),
    ));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        root.join("graph.rs"),
        "pub struct AdenGraph<N, E> { n: std::marker::PhantomData<(N, E)> }\nimpl<N, E> AdenGraph<N, E> { pub fn bfs(&self) {} }\n",
    )
    .unwrap();
    (root, data)
}

fn aden(root: &std::path::Path, data: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(args)
        .current_dir(root)
        .env("ADEN_DATA_DIR", data)
        .output()
        .unwrap()
}

fn aden_machine_error(
    root: &std::path::Path,
    data: &std::path::Path,
    args: &[&str],
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(args)
        .current_dir(root)
        .env("ADEN_DATA_DIR", data)
        .env("ADEN_MCP_MACHINE_ERRORS", "1")
        .output()
        .unwrap()
}

#[test]
fn locate_and_understand_accept_generic_qualified_symbol_shorthand() {
    let (root, data) = fixture();
    assert!(aden(&root, &data, &["gen", "."]).status.success());

    for spelling in ["AdenGraph::bfs", "AdenGraph < N, E > :: bfs"] {
        let locate = aden(&root, &data, &["-j", "locate", "--symbol", spelling, "."]);
        assert!(
            locate.status.success(),
            "locate stderr={}",
            String::from_utf8_lossy(&locate.stderr)
        );
        let located: serde_json::Value = serde_json::from_slice(&locate.stdout).unwrap();
        assert!(located.to_string().contains("bfs"), "{spelling}: {located}");
        assert_eq!(located["resolution"]["state"], "unique");
        assert_eq!(located["resolution"]["complete"], true);

        let understand = aden(&root, &data, &["-j", "understand", spelling, "."]);
        assert!(
            understand.status.success(),
            "understand stderr={}",
            String::from_utf8_lossy(&understand.stderr)
        );
        let understood: serde_json::Value = serde_json::from_slice(&understand.stdout).unwrap();
        assert!(
            understood["anchor"]
                .as_str()
                .unwrap_or_default()
                .contains("bfs"),
            "{spelling}: {understood}"
        );
    }

    let natural = aden(&root, &data, &["-j", "locate", "--symbol", "bfs", "."]);
    let natural: serde_json::Value = serde_json::from_slice(&natural.stdout).unwrap();
    let canonical = natural["resolution"]["anchor"].as_str().unwrap();
    let exact = aden(&root, &data, &["-j", "locate", "--symbol", canonical, "."]);
    assert!(exact.status.success());
    let exact: serde_json::Value = serde_json::from_slice(&exact.stdout).unwrap();
    assert_eq!(exact["resolution"]["state"], "exact");
    assert!(
        exact["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );

    let missing = aden(&root, &data, &["understand", "definitely_missing", "."]);
    assert!(missing.status.success());
    let text = String::from_utf8_lossy(&missing.stdout);
    assert!(
        text.contains("aden locate --symbol"),
        "missing recovery: {text}"
    );
    assert!(
        !text.contains("aden list ."),
        "broad-listing recovery regressed: {text}"
    );

    let substring = aden(&root, &data, &["-j", "understand", "fs", "."]);
    assert!(substring.status.success());
    let value: serde_json::Value = serde_json::from_slice(&substring.stdout).unwrap();
    assert_eq!(value["anchor"], serde_json::Value::Null);
    assert_eq!(value["resolution"]["state"], "not_indexed");
    assert!(
        value["resolution"]["suggestions"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|item| item.as_str().is_some_and(|anchor| anchor.contains("bfs"))))
    );

    let located_substring = aden(&root, &data, &["-j", "locate", "--symbol", "fs", "."]);
    assert!(located_substring.status.success());
    let value: serde_json::Value = serde_json::from_slice(&located_substring.stdout).unwrap();
    assert_eq!(value["match_kind"], "symbol_suggestions");
    assert_eq!(value["resolution"]["state"], "not_found");
    assert_eq!(value["resolution"]["complete"], false);
    assert!(
        value["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );

    let typo = aden(&root, &data, &["-j", "locate", "--symbol", "bgs", "."]);
    assert!(typo.status.success());
    let value: serde_json::Value = serde_json::from_slice(&typo.stdout).unwrap();
    assert_eq!(value["resolution"]["state"], "not_found");
    assert_eq!(value["resolution"]["complete"], false);
    assert!(
        value["resolution"]["suggestions"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|item| item.as_str().is_some_and(|anchor| anchor.contains("bfs"))))
    );

    let human_typo = aden(&root, &data, &["--human", "locate", "--symbol", "bgs", "."]);
    assert!(human_typo.status.success());
    let text = String::from_utf8_lossy(&human_typo.stdout);
    assert!(text.contains("Did you mean"), "human typo recovery: {text}");
    assert!(text.contains("bfs"), "human typo recovery: {text}");

    for command in ["asm", "ask"] {
        let args: Vec<&str> = if command == "asm" {
            vec!["asm", "--from", "bgs", "."]
        } else {
            vec!["ask", "--from", "bgs", "How does it work?", "."]
        };
        let structural_typo = aden(&root, &data, &args);
        assert!(!structural_typo.status.success(), "{command} accepted typo");
        let error = String::from_utf8_lossy(&structural_typo.stderr);
        assert!(
            error.contains("Suggestions:"),
            "{command} typo recovery: {error}"
        );
        assert!(error.contains("bfs"), "{command} typo recovery: {error}");
        assert!(
            !String::from_utf8_lossy(&structural_typo.stdout).contains("mod-project"),
            "{command} silently fell back to project root"
        );
    }

    let machine = aden_machine_error(&root, &data, &["asm", "--from", "bgs", "."]);
    assert!(!machine.status.success());
    let value: serde_json::Value = serde_json::from_slice(&machine.stderr).unwrap();
    assert_eq!(value["error"]["code"], "anchor_not_found");
    assert!(
        value["error"]["suggestions"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|item| item.as_str().is_some_and(|anchor| anchor.contains("bfs"))))
    );
}

#[test]
fn asm_and_all_query_modes_accept_a_unique_bare_symbol() {
    let (root, data) = fixture();
    assert!(aden(&root, &data, &["gen", "."]).status.success());

    let asm = aden(&root, &data, &["asm", "--from", "bfs", "."]);
    assert!(
        asm.status.success(),
        "asm stderr={}",
        String::from_utf8_lossy(&asm.stderr)
    );
    assert!(String::from_utf8_lossy(&asm.stdout).contains("bfs"));

    let ask = aden(
        &root,
        &data,
        &[
            "ask",
            "--strict",
            "--budget",
            "512",
            "--from",
            "bfs",
            "How does bfs work?",
            ".",
        ],
    );
    assert!(
        ask.status.success(),
        "ask stderr={}",
        String::from_utf8_lossy(&ask.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&ask.stdout).unwrap();
    assert!(
        value["anchor"]
            .as_str()
            .is_some_and(|anchor| anchor.contains("bfs"))
    );

    for mode in ["--from", "--backlinks", "--impact"] {
        let out = aden(
            &root,
            &data,
            &["query", "--format", "json", mode, "bfs", "."],
        );
        assert!(
            out.status.success(),
            "{mode} stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(value.to_string().contains("bfs"), "{mode}: {value}");
    }
}

#[test]
fn understand_returns_ambiguous_json_instead_of_choosing_an_equal_generic_match() {
    let (root, data) = fixture();
    std::fs::write(
        root.join("other.rs"),
        "pub struct AdenGraph<T, U> { n: std::marker::PhantomData<(T, U)> }\nimpl<T, U> AdenGraph<T, U> { pub fn bfs(&self) {} }\n",
    )
    .unwrap();
    assert!(aden(&root, &data, &["gen", "."]).status.success());

    let located = aden(
        &root,
        &data,
        &["-j", "locate", "--symbol", "AdenGraph::bfs", "."],
    );
    assert!(located.status.success());
    let located: serde_json::Value = serde_json::from_slice(&located.stdout).unwrap();
    assert_eq!(located["match_kind"], "ambiguous_definitions");
    assert_eq!(located["resolution"]["state"], "ambiguous");
    assert_eq!(located["resolution"]["complete"], false);
    assert!(
        located["resolution"]["candidates"]
            .as_array()
            .unwrap()
            .len()
            >= 2
    );

    let ask = aden(
        &root,
        &data,
        &["ask", "--from", "AdenGraph::bfs", "How does it work?", "."],
    );
    assert!(!ask.status.success());
    let error = String::from_utf8_lossy(&ask.stderr);
    assert!(error.contains("Ambiguous symbol"), "ask ambiguity: {error}");
    assert!(error.contains("Candidates:"), "ask ambiguity: {error}");
    assert!(!String::from_utf8_lossy(&ask.stdout).contains("mod-project"));

    let out = aden(&root, &data, &["-j", "understand", "AdenGraph::bfs", "."]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["anchor"], serde_json::Value::Null);
    assert_eq!(value["resolution"]["state"], "ambiguous");
    assert_eq!(value["resolution"]["complete"], false);
    assert!(value["resolution"]["candidates"].as_array().unwrap().len() >= 2);
    assert!(
        value["resolution"]["recovery"]
            .as_str()
            .unwrap()
            .contains("aden locate --symbol")
    );

    for args in [
        vec!["asm", "--from", "AdenGraph::bfs", "."],
        vec![
            "query",
            "--format",
            "json",
            "--impact",
            "AdenGraph::bfs",
            ".",
        ],
    ] {
        let ambiguous = aden(&root, &data, &args);
        assert!(!ambiguous.status.success(), "args={args:?}");
        let stderr = String::from_utf8_lossy(&ambiguous.stderr);
        assert!(stderr.contains("Ambiguous symbol"), "{args:?}: {stderr}");
        assert!(stderr.contains("Candidates:"), "{args:?}: {stderr}");
        assert!(stderr.matches("  - ").count() >= 2, "{args:?}: {stderr}");
    }

    let machine = aden_machine_error(&root, &data, &["asm", "--from", "AdenGraph::bfs", "."]);
    assert!(!machine.status.success());
    let value: serde_json::Value = serde_json::from_slice(&machine.stderr).unwrap();
    assert_eq!(value["error"]["code"], "ambiguous_symbol");
    assert!(value["error"]["candidates"].as_array().unwrap().len() >= 2);

    let disabled_machine_mode = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["asm", "--from", "AdenGraph::bfs", "."])
        .current_dir(&root)
        .env("ADEN_DATA_DIR", &data)
        .env("ADEN_MCP_MACHINE_ERRORS", "0")
        .output()
        .unwrap();
    assert!(!disabled_machine_mode.status.success());
    let stderr = String::from_utf8_lossy(&disabled_machine_mode.stderr);
    assert!(stderr.starts_with("Error: Ambiguous symbol"), "{stderr}");
}
