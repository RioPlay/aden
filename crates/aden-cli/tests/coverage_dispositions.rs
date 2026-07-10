// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! AP-102 end-to-end coverage receipts and exclusion recovery.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn unique_dir(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "aden-coverage-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn run(project: &Path, data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(args)
        .current_dir(project)
        .env("ADEN_DATA_DIR", data)
        .output()
        .expect("aden binary")
}

fn run_with_parse_failure(project: &Path, data: &Path, path: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(args)
        .current_dir(project)
        .env("ADEN_DATA_DIR", data)
        .env("ADEN_TEST_FORCE_PARSE_FAILED", path)
        .output()
        .expect("aden binary")
}

fn status(project: &Path, data: &Path) -> serde_json::Value {
    let output = run(project, data, &["-j", "status", "."]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("status JSON")
}

fn fixture() -> (PathBuf, PathBuf) {
    let project = unique_dir("project");
    let data = unique_dir("data");
    std::fs::write(project.join("main.rs"), "pub fn visible() {}\n").unwrap();
    std::fs::write(project.join("ignored.rs"), "pub fn ignored() {}\n").unwrap();
    std::fs::write(project.join("notes.xyz"), "unsupported\n").unwrap();
    std::fs::write(project.join("private.key"), "not read\n").unwrap();
    std::fs::write(project.join("invalid.rs"), [0xff, 0xfe]).unwrap();
    std::fs::write(
        project.join("parse_fail.rs"),
        "pub fn parser_fixture() {}\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(project.join("unreadable.rs"), "pub fn unreadable() {}\n").unwrap();
        std::fs::set_permissions(
            project.join("unreadable.rs"),
            std::fs::Permissions::from_mode(0),
        )
        .unwrap();
    }
    let token = String::from_utf8_lossy(&[
        103, 104, 112, 95, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 97, 98, 99, 100, 101, 102, 103,
        104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 117, 118, 119, 120, 121,
        122,
    ]);
    std::fs::write(
        project.join("embedded.rs"),
        format!("const TOKEN: &str = \"{token}\";\n"),
    )
    .unwrap();
    std::fs::write(project.join(".adenignore"), "ignored.rs\n").unwrap();
    (project, data)
}

#[test]
fn gen_status_accounts_for_exclusions_and_recovers_them() {
    let (project, data) = fixture();
    let generated = run_with_parse_failure(&project, &data, "parse_fail.rs", &["gen", "."]);
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );

    let first = status(&project, &data);
    let coverage = &first["coverage"];
    assert!(coverage["indexed"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(coverage["ignored"], 1);
    assert!(coverage["unsupported"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(coverage["secret_path"], 1);
    assert_eq!(coverage["secret_content"], 1);
    assert_eq!(coverage["invalid_encoding"], 1);
    assert_eq!(coverage["parse_failed"], 1);
    #[cfg(unix)]
    assert_eq!(coverage["io_failed"], 1);

    std::fs::write(
        project.join("embedded.rs"),
        "pub fn recovered_secret_file() {}\n",
    )
    .unwrap();
    std::fs::write(
        project.join("invalid.rs"),
        "pub fn recovered_encoding() {}\n",
    )
    .unwrap();
    std::fs::write(project.join(".adenignore"), "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            project.join("unreadable.rs"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
    }
    let regenerated = run(&project, &data, &["gen", ".", "--force-regen"]);
    assert!(
        regenerated.status.success(),
        "{}",
        String::from_utf8_lossy(&regenerated.stderr)
    );
    let recovered = status(&project, &data);
    assert_eq!(
        recovered["coverage"]["secret_content"]
            .as_u64()
            .unwrap_or(0),
        0
    );
    assert_eq!(
        recovered["coverage"]["invalid_encoding"]
            .as_u64()
            .unwrap_or(0),
        0
    );
    assert_eq!(recovered["coverage"]["ignored"].as_u64().unwrap_or(0), 0);
    assert_eq!(
        recovered["coverage"]["parse_failed"].as_u64().unwrap_or(0),
        0
    );
    #[cfg(unix)]
    assert_eq!(recovered["coverage"]["io_failed"].as_u64().unwrap_or(0), 0);
}

#[test]
fn gen_persists_coverage_when_no_file_is_indexable() {
    let project = unique_dir("empty-eligible-project");
    let data = unique_dir("empty-eligible-data");
    std::fs::write(project.join("notes.xyz"), "unsupported\n").unwrap();
    let generated = run(&project, &data, &["gen", "."]);
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let result = status(&project, &data);
    assert_eq!(result["coverage"]["indexed"], 0);
    assert_eq!(result["coverage"]["unsupported"], 1);
}
