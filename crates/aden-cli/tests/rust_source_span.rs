// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_store::GraphStorage;
use serde_json::Value;
use std::process::Command;

fn run(root: &std::path::Path, data: &std::path::Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(args)
        .current_dir(root)
        .env("ADEN_DATA_DIR", data)
        .env_remove("ADEN_STORE")
        .env("ADEN_SKIP_AUTO_GEN", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn rust_function_spans_survive_store_and_snapshot_without_trusting_stale_source() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let source = "// 日本語🙂\r\n#[inline]\r\npub fn target() {\r\n    let _ = \"é\";\r\n}\r\nstruct Sample;\r\nimpl Sample {\r\n    #[inline]\r\n    fn method(&self) {}\r\n}\r\n";
    let file = root.path().join("source.rs");
    std::fs::write(&file, source).unwrap();
    let generated = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["gen", ".", "--quiet"])
        .current_dir(root.path())
        .env("ADEN_DATA_DIR", data.path())
        .env_remove("ADEN_STORE")
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );

    let project_data = std::fs::read_dir(data.path().join("projects"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let storage =
        aden_store::Storage::open_existing(project_data.join("store").to_str().unwrap()).unwrap();
    let documents = storage.get_all_documents().unwrap();
    drop(storage);
    let snapshot_bytes = std::fs::read(project_data.join("graph.snapshot")).unwrap();
    let (snapshot, _) = aden_graph::snapshot::decode_snapshot(&snapshot_bytes).unwrap();
    for (name, text, first, last) in [
        (
            "target",
            "pub fn target() {\r\n    let _ = \"é\";\r\n}",
            3,
            5,
        ),
        ("Sample::method", "fn method(&self) {}", 9, 9),
    ] {
        let (anchor, doc) = documents
            .iter()
            .find(|(anchor, _)| anchor.ends_with(&format!("#{name}")))
            .unwrap();
        let span = doc
            .source_span
            .as_ref()
            .expect("persisted Rust function source span");
        assert_eq!(
            std::path::Path::new(&span.file),
            file.canonicalize().unwrap()
        );
        assert_eq!((span.start_line, span.end_line), (first, last));
        assert_eq!(span.start_byte, source.find(text).unwrap());
        assert_eq!(span.end_byte, span.start_byte + text.len());
        let saved = snapshot.get(anchor).unwrap().source_span.as_ref().unwrap();
        assert_eq!(
            serde_json::to_value(saved).unwrap(),
            serde_json::to_value(span).unwrap()
        );
    }

    let current = run(root.path(), data.path(), &["understand", "target", "."]);
    assert_eq!(current["source"]["state"], "complete");
    assert_eq!(current["source"]["hash_verified"], true);
    assert_eq!(
        current["source"]["text"],
        "pub fn target() {\r\n    let _ = \"é\";\r\n}\r\n"
    );

    std::fs::write(&file, source.replace("é", "changed")).unwrap();
    let changed = run(root.path(), data.path(), &["understand", "target", "."]);
    assert_ne!(changed["source"]["hash_verified"], true);
    assert!(changed["source"]["text"].is_null(), "{changed}");

    std::fs::write(&file, source).unwrap();
    let restored = run(root.path(), data.path(), &["understand", "target", "."]);
    assert_eq!(restored["source"]["state"], "complete");
    assert_eq!(restored["source"]["hash_verified"], true);

    std::fs::remove_file(&file).unwrap();
    let missing = run(root.path(), data.path(), &["understand", "target", "."]);
    assert_eq!(missing["source"]["state"], "unavailable");
    assert!(missing["source"]["text"].is_null());
}

#[test]
fn inline_rust_symbols_and_test_relationships_survive_warm_generation() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname=\"inline-probe\"\nversion=\"0.0.0\"\nedition=\"2024\"\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("src/lib.rs"),
        "fn target() {}\n\
         mod left {\n\
             fn helper() {}\n\
             fn caller() { helper(); }\n\
             mod checks {\n\
                 use super::*;\n\
                 #[test] fn exercises_helper() { assert_eq!(helper(), ()); }\n\
                 fn utility() { helper(); }\n\
             }\n\
         }\n\
         mod right { fn helper() {} fn caller() { helper(); } }\n",
    )
    .unwrap();
    let generate = || {
        let output = Command::new(env!("CARGO_BIN_EXE_aden"))
            .args(["gen", ".", "--quiet"])
            .current_dir(root.path())
            .env("ADEN_DATA_DIR", data.path())
            .env_remove("ADEN_STORE")
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
    };
    generate();
    let locate = run(
        root.path(),
        data.path(),
        &["locate", "--symbol", "left::helper"],
    );
    assert_eq!(locate["resolution"]["state"], "unique", "{locate}");
    let helper = locate["items"][0]["anchor"].as_str().unwrap();
    for _ in 0..2 {
        let backlinks = run(
            root.path(),
            data.path(),
            &["query", "--backlinks", helper, "--depth", "1"],
        );
        let items = backlinks["items"].as_array().unwrap();
        for (suffix, is_test) in [
            ("#left::caller", false),
            ("#left::checks::exercises_helper", true),
            ("#left::checks::utility", false),
        ] {
            let item = items
                .iter()
                .find(|item| item["anchor"].as_str().is_some_and(|s| s.ends_with(suffix)))
                .unwrap_or_else(|| panic!("missing {suffix}: {backlinks}"));
            let edges = item["via_edge_types"].as_array().unwrap();
            assert!(edges.iter().any(|edge| edge == "Calls"), "{item}");
            assert_eq!(edges.iter().any(|edge| edge == "Tests"), is_test, "{item}");
        }
        assert!(items.iter().all(|item| {
            !item["anchor"]
                .as_str()
                .is_some_and(|s| s.ends_with("#right::caller"))
        }));
        generate();
    }
}
