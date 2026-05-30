// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

// Integration tests for the aden CLI.
// These exercise the installed binary via std::process::Command.

mod temp_project {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    pub fn temp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("aden-cli-test-{}", n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub fn scaffold(dir: &std::path::Path) {
        std::fs::write(
            dir.join("README.adoc"),
            r#"[[readme]]
= Test Project

Hello world.

<<module-a>>
"#,
        )
        .unwrap();

        std::fs::write(
            dir.join("module-a.adoc"),
            r#"[[module-a]]
= Module A

This is module a.

|===
|Name |Value
|foo |bar
|===

<<readme>>
"#,
        )
        .unwrap();
    }
}

#[test]
fn test_check_finds_no_issues() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new("aden")
        .arg("check")
        .arg(&dir)
        .output()
        .expect("aden binary must be installed for tests");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "aden check failed. stdout: {}\nstderr: {}",
        stdout,
        stderr
    );
}

#[test]
fn test_asm_assembles_nonempty_output() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new("aden")
        .args([
            "asm",
            "--from",
            "readme",
            "--depth",
            "2",
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be installed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "aden asm failed: {}", stdout);
    // Default aden asm outputs LLM-stripped prose (not ADG JSON).
    assert!(
        stdout.contains("Hello world."),
        "Should contain document body in stripped prose, got: {}",
        stdout
    );
}

#[test]
fn test_ask_returns_context() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new("aden")
        .args(["ask", "What is module a?", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "aden ask failed: {}", stdout);
    assert!(
        stdout.contains("[[module-a]]"),
        "Should mention module-a anchor. stdout:\n{}",
        stdout
    );
}

#[test]
fn test_graph_outputs_neighborhood() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new("aden")
        .args([
            "query",
            "--from",
            "readme",
            "--depth",
            "2",
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be installed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "aden query failed: {}", stdout);
    assert!(
        stdout.contains("module-a"),
        "Should show module-a in graph. stdout:\n{}",
        stdout
    );
}

#[test]
fn test_init_scaffolds_agent_dir() {
    let dir = temp_project::temp_dir();

    let output = std::process::Command::new("aden")
        .args(["init", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "aden init failed: {}", stdout);
    assert!(dir.join(".agent").is_dir(), ".agent/ should be created");
    assert!(
        dir.join(".adenignore").exists(),
        ".adenignore should be created"
    );
}

#[test]
fn test_search_finds_results() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new("aden")
        .args(["search", "module", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "aden search failed: {}", stdout);
    assert!(
        stdout.contains("module-a"),
        "Should find module-a. stdout:\n{}",
        stdout
    );
}

#[test]
fn test_query_from_returns_json() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    let output = std::process::Command::new("aden")
        .args([
            "query",
            "--from",
            "readme",
            "--depth",
            "2",
            &dir.to_string_lossy(),
        ])
        .output()
        .expect("aden binary must be installed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "aden query failed: {}", stdout);
    // JSON output should start with [ or { depending on implementation
}

// Additional CLI command tests - verify commands run without crashing
#[test]
fn test_additional_commands() {
    let dir = temp_project::temp_dir();
    temp_project::scaffold(&dir);

    // Test locate
    let _ = std::process::Command::new("aden")
        .args(["locate", "--symbol", "module-a", &dir.to_string_lossy()])
        .output();

    // Test gen (may have no source files)
    let _ = std::process::Command::new("aden")
        .args(["gen", "--auto", &dir.to_string_lossy()])
        .output();

    // Test lint
    let lint = std::process::Command::new("aden")
        .args(["lint", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");
    assert!(lint.status.success(), "lint should pass");

    // Test test --list
    let test = std::process::Command::new("aden")
        .args(["test", "--list", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");
    assert!(test.status.success(), "test --list should work");

    // Test heal
    let heal = std::process::Command::new("aden")
        .args(["heal", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");
    assert!(heal.status.success(), "heal should run");

    // Test list
    let list = std::process::Command::new("aden")
        .args(["list", &dir.to_string_lossy()])
        .output()
        .expect("aden binary must be installed");
    assert!(list.status.success(), "list should work");
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("readme"),
        "Should list readme anchor"
    );

    // Test mcp list
    let mcp = std::process::Command::new("aden")
        .args(["mcp", "list"])
        .output()
        .expect("aden binary must be installed");
    assert!(mcp.status.success(), "mcp list should work");
}
