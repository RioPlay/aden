//! An offline viewer must not discard anchors before the user can search them.
#![cfg(feature = "view")]

use std::process::Command;

#[test]
fn viewer_keeps_symbols_beyond_both_former_export_caps() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let source: String = (0..4005)
        .map(|i| format!("pub fn symbol_{i:04}() {{}}\n"))
        .collect();
    std::fs::write(project.join("symbols.rs"), source).unwrap();
    let output_path = dir.path().join("viewer.html");
    let output = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["view", "--no-open", "--out"])
        .arg(&output_path)
        .arg("--project")
        .arg(&project)
        .env("ADEN_DATA_DIR", dir.path().join("data"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let html = std::fs::read_to_string(&output_path).unwrap();
    assert!(html.contains("href=\"viewer-simple.html\""));
    let simple = std::fs::read_to_string(dir.path().join("viewer-simple.html")).unwrap();
    assert!(simple.contains("#symbol_4004"));
    assert!(!simple.contains("ForceGraph"));
    let data = html.split("const DATA = ").nth(1).unwrap();
    // Stream one JSON value; the following JS and CRLF/LF are not JSON.
    let value = serde_json::Deserializer::from_str(data)
        .into_iter::<serde_json::Value>()
        .next()
        .unwrap()
        .unwrap();
    let nodes = value["nodes"].as_array().unwrap();
    let simple_value =
        serde_json::Deserializer::from_str(simple.split("const DATA = ").nth(1).unwrap())
            .into_iter::<serde_json::Value>()
            .next()
            .unwrap()
            .unwrap();
    assert_eq!(simple_value["nodes"], value["nodes"]);
    assert_eq!(simple_value["edges"], value["edges"]);
    for unused in ["activity", "all_anchors", "communities"] {
        assert!(
            simple_value.get(unused).is_none(),
            "unused simple payload field: {unused}"
        );
    }
    assert!(!simple.contains("/*SEARCH_HELPERS*/"));
    assert!(simple.contains("const RICH_VIEW = \"viewer.html\""));
    assert!(nodes.len() >= 4005);
    assert_eq!(value["shown_nodes"], value["total_nodes"]);
    assert_eq!(value["all_anchors"].as_array().unwrap().len(), nodes.len());
    assert!(
        nodes
            .iter()
            .any(|n| n["anchor"].as_str().unwrap().ends_with("#symbol_4004"))
    );
    assert!(nodes.iter().any(|n| {
        n["snippet"]
            .as_str()
            .is_some_and(|s| s.contains("pub fn symbol_"))
    }));
}

#[test]
fn simple_export_is_standalone_and_rejects_animation_flags() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(
        project.join("lib.rs"),
        "pub fn caller() { callee(); }\npub fn callee() {}\n",
    )
    .unwrap();
    let output_path = dir.path().join("plain #1 Š.html");
    let output = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["view", "--simple", "--no-open", "--out"])
        .arg(&output_path)
        .arg("--project")
        .arg(&project)
        .env("ADEN_DATA_DIR", dir.path().join("data"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let html = std::fs::read_to_string(output_path).unwrap();
    assert!(html.contains("#caller") && html.contains("#callee"));
    assert!(!html.contains("ForceGraph") && !html.contains("requestAnimationFrame"));
    assert!(!html.contains("/*ADEN_DATA*/"));
    assert!(!dir.path().join("plain #1 Š-3d.html").exists());
    assert!(!dir.path().join("plain #1 Š-simple.html").exists());
    for flag in ["--3d", "--replay"] {
        let result = Command::new(env!("CARGO_BIN_EXE_aden"))
            .args(["view", "--simple", flag, "--no-open"])
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("cannot be used"));
    }
}
