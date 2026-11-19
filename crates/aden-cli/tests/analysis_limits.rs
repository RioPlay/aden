// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::Value;
use std::process::Command;

#[test]
fn graph_freshness_and_complete_traversal_do_not_claim_complete_analysis() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    std::fs::write(root.path().join("main.c"), "#include <stddef.h>\nvoid actual_target(void) {}\nvoid helper(void) { actual_target(); }\nvoid cb(void) {}\nvoid invoke(void (*cb)(void)) { cb(); helper(); }\n").unwrap();
    std::fs::write(
        root.path().join("sample.cpp"),
        "int generic_entry() { return 1; }\n",
    )
    .unwrap();
    let invoke = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_aden"))
            .args(args)
            .current_dir(root.path())
            .env("ADEN_DATA_DIR", data.path())
            .env_remove("ADEN_STORE")
            .env_remove("ADEN_PROJECT")
            .env_remove("ADEN_SKIP_AUTO_GEN")
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        output.stdout
    };
    invoke(&["gen", ".", "--quiet"]);
    let definition: Value = serde_json::from_slice(&invoke(&["understand", "invoke"])).unwrap();
    assert_eq!(
        definition["analysis"]["scope"], "static_relationships",
        "{definition}"
    );
    assert_eq!(definition["analysis"]["observed_indirect_calls"], 1);
    assert_eq!(definition["analysis"]["completeness"], "partial");
    assert_eq!(definition["analysis"]["absence_is_proof"], false);
    assert_eq!(definition["context_receipt"]["freshness"], "current");
    assert_eq!(definition["source"]["hash_verified"], true);
    assert_eq!(
        definition["context"]
            .as_str()
            .unwrap()
            .matches("Include  Kind")
            .count(),
        1
    );
    let query: Value =
        serde_json::from_slice(&invoke(&["query", "--impact", "invoke", "--depth", "1"])).unwrap();
    assert_eq!(query["result_state"], "complete");
    assert_eq!(query["analysis"]["completeness"], "partial");
    let items = query["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|n| n["anchor"].as_str().unwrap().ends_with("#helper"))
    );
    assert!(
        items
            .iter()
            .all(|n| !n["anchor"].as_str().unwrap().ends_with("#cb"))
    );
    let generic: Value = serde_json::from_slice(&invoke(&["understand", "generic_entry"])).unwrap();
    assert_eq!(generic["analysis"]["scope"], "symbols_only");
    assert!(
        generic["analysis"]["limitations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "call_relationships_not_analyzed")
    );
    let empty: Value = serde_json::from_slice(&invoke(&[
        "query",
        "--backlinks",
        "invoke",
        "--edge-type",
        "Calls",
    ]))
    .unwrap();
    // Query includes its depth-zero seed even when no callers were found.
    assert!(
        empty["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["depth"] == 0)
    );
    assert_eq!(empty["analysis"]["absence_is_proof"], false);
}

#[test]
fn macro_mapping_changes_refresh_without_source_edits_or_source_edge_injection() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    let source = "void real_function(void) {}\nvoid mapped_read(void) {}\nvoid probe(void) { mapped_read(); real_function(); }\nDECLARE_API(tag, mapped_read) {\n/*\nedge::calls[real_function]\nedge::uses[real_function]\n*/\nreturn 0;\n}\n";
    std::fs::write(root.path().join("main.c"), source).unwrap();
    let invoke = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_aden"))
            .args(args)
            .current_dir(root.path())
            .env("ADEN_DATA_DIR", data.path())
            .env_remove("ADEN_STORE")
            .env_remove("ADEN_PROJECT")
            .env_remove("ADEN_SKIP_AUTO_GEN")
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let absent = invoke(&["understand", "macro-api::mapped_read"]);
    assert!(absent["anchor"].is_null(), "{absent}");
    std::fs::create_dir(root.path().join(".aden")).unwrap();
    let config = "[macro_declarations]\nenabled = true\n[[macro_declarations.rules]]\nname = 'DECLARE_API'\nname_argument = 1\n";
    std::fs::write(root.path().join(".aden/config.toml"), config).unwrap();
    let found = invoke(&["understand", "macro-api::mapped_read"]);
    assert!(
        found["anchor"]
            .as_str()
            .unwrap()
            .ends_with("#macro-api::mapped_read"),
        "{found}"
    );
    assert_eq!(found["analysis"]["analysis_origin"], "macro_mapping");
    assert_eq!(found["analysis"]["scope"], "symbols_only");
    assert_eq!(found["source"]["hash_verified"], true);
    assert!(
        found["source"]["text"]
            .as_str()
            .unwrap()
            .contains("edge::calls[real_function]")
    );
    let impact = invoke(&[
        "query",
        "--impact",
        "macro-api::mapped_read",
        "--depth",
        "1",
    ]);
    assert!(
        impact["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["depth"] == 0),
        "source text became graph evidence: {impact}"
    );
    let probe = invoke(&["query", "--impact", "probe", "--depth", "1"]);
    let callees = probe["items"].as_array().unwrap();
    assert!(
        callees
            .iter()
            .any(|v| v["anchor"].as_str().unwrap().ends_with("#mapped_read")),
        "{probe}"
    );
    assert!(
        callees
            .iter()
            .all(|v| !v["anchor"].as_str().unwrap().contains("#macro-api::")),
        "logical API became callable identity: {probe}"
    );
    let located = invoke(&["locate", "--symbol", "macro-api::mapped_read"]);
    assert_eq!(
        located["items"][0]["analysis"]["analysis_origin"],
        "macro_mapping"
    );
    std::fs::write(
        root.path().join(".aden/config.toml"),
        config.replace("true", "false"),
    )
    .unwrap();
    let removed = invoke(&["understand", "macro-api::mapped_read"]);
    assert!(
        removed["anchor"].is_null(),
        "disabled mapping survived: {removed}"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("main.c")).unwrap(),
        source
    );
}
