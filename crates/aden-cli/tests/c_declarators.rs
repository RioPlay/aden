// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::Value;
use std::process::Command;

#[test]
fn c_declarators_preserve_functions_without_inventing_callback_targets() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("main.c"),
        "#define __initdata\n\
         void (*__initdata late_time_init)(void);\n\
         int initialized = 1;\n\
         int (*get_callback(void))(int);\n\
         void real_function(void) {}\n\
         void start_kernel(void) { real_function(); if (late_time_init) late_time_init(); }\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("foreign.c"),
        "static void late_time_init(void) {}\n",
    )
    .unwrap();
    let invoke = |args: &[&str]| {
        let result = Command::new(env!("CARGO_BIN_EXE_aden"))
            .args(args)
            .current_dir(root.path())
            .env("ADEN_DATA_DIR", data.path())
            .env_remove("ADEN_STORE")
            .env_remove("ADEN_PROJECT")
            .env_remove("ADEN_SKIP_AUTO_GEN")
            .output()
            .unwrap();
        assert!(result.status.success(), "{args:?}: {result:?}");
        result.stdout
    };
    invoke(&["gen", ".", "--quiet"]);
    let initialized: Value =
        serde_json::from_slice(&invoke(&["locate", "--symbol", "initialized"])).unwrap();
    assert_eq!(
        initialized["resolution"]["state"], "not_found",
        "{initialized}"
    );
    let callback: Value =
        serde_json::from_slice(&invoke(&["locate", "--symbol", "late_time_init"])).unwrap();
    assert_eq!(callback["resolution"]["state"], "unique", "{callback}");
    assert!(
        callback["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| { item["file"] != "main.c" }),
        "the pointer declaration must not become a function: {callback}"
    );
    for name in ["real_function", "get_callback", "start_kernel"] {
        let value: Value = serde_json::from_slice(&invoke(&["locate", "--symbol", name])).unwrap();
        assert_eq!(value["resolution"]["state"], "unique", "{value}");
    }
    let value: Value = serde_json::from_slice(&invoke(&[
        "query",
        "--impact",
        "start_kernel",
        "--depth",
        "1",
    ]))
    .unwrap();
    let anchors: Vec<_> = value["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["anchor"].as_str())
        .collect();
    assert!(
        anchors
            .iter()
            .any(|anchor| anchor.ends_with("#real_function"))
    );
    assert!(
        anchors
            .iter()
            .all(|anchor| !anchor.ends_with("#late_time_init")),
        "unexpected direct callback edge: {value}"
    );
    invoke(&["gen", ".", "--quiet"]);
    let warm: Value = serde_json::from_slice(&invoke(&[
        "query",
        "--impact",
        "start_kernel",
        "--depth",
        "1",
    ]))
    .unwrap();
    assert_eq!(value["items"], warm["items"]);
}
