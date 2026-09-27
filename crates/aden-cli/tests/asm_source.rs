// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::process::Command;

#[test]
fn asm_json_includes_verified_source_for_the_seed_symbol() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let source =
        "fn source_first() {\n    important_dependency();\n}\nfn important_dependency() {}\n";
    std::fs::write(root.path().join("source.rs"), source).unwrap();

    let gen_output = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["gen", "."])
        .current_dir(root.path())
        .env("ADEN_DATA_DIR", data.path())
        .output()
        .unwrap();
    assert!(
        gen_output.status.success(),
        "{}",
        String::from_utf8_lossy(&gen_output.stderr)
    );

    let output = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["asm", "--from", "source_first", "--budget", "512", "."])
        .current_dir(root.path())
        .env("ADEN_DATA_DIR", data.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let documents = payload["documents"].as_array().unwrap();
    let seed = documents
        .iter()
        .find(|document| {
            document["anchor"]
                .as_str()
                .unwrap()
                .ends_with("#source_first")
        })
        .expect("seed document");
    assert_eq!(seed["source"]["hash_verified"], true);
    assert_eq!(seed["source"]["state"], "complete");
    assert!(
        seed["source"]["text"]
            .as_str()
            .unwrap()
            .contains("important_dependency();")
    );
}
