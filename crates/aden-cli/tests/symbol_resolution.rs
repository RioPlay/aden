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
}
