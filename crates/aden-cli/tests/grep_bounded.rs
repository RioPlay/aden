// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::process::Command;

#[test]
fn grep_limit_bounds_retained_results_but_keeps_exact_total() {
    let root = std::env::temp_dir().join(format!("aden-grep-bounded-{}", std::process::id()));
    let data = root.join("data");
    let project = root.join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='grep-bounded'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    let lines = (0..100)
        .map(|i| format!("// bounded_match {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(project.join("src/a.rs"), &lines).unwrap();
    std::fs::write(project.join("src/b.rs"), &lines).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["-j", "grep", "bounded_match", "--limit", "3"])
        .arg(&project)
        .env("ADEN_DATA_DIR", &data)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["total"], 200);
    assert_eq!(value["returned"], 3);
    assert_eq!(value["truncated"], true);
    assert_eq!(value["matches"].as_array().unwrap().len(), 3);

    std::fs::remove_dir_all(root).unwrap();
}
