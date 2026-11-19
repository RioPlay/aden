// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
use std::path::Path;
use std::process::{Command, Output};

fn run(root: &Path, data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(args)
        .current_dir(root)
        .env("ADEN_DATA_DIR", data)
        .env_remove("ADEN_STORE")
        .output()
        .unwrap()
}

#[test]
fn preview_apply_preserve_intent_rebuild_and_refuse_active_writer() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    let data = temp.path().join("data");
    std::fs::create_dir_all(root.join(".aden/overlays")).unwrap();
    std::fs::write(root.join("lib.rs"), "pub fn cleanup_symbol() {}\n").unwrap();
    std::fs::write(root.join(".aden/overlays/keep.adoc"), "intent").unwrap();
    let generate = run(&root, &data, &["gen", ".", "--quiet"]);
    assert!(
        generate.status.success(),
        "{}",
        String::from_utf8_lossy(&generate.stderr)
    );
    let project = std::fs::read_dir(data.join("projects"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let preview = run(&root, &data, &["store", "clean", ".", "--json"]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(report["applied"], false);
    assert!(report["bytes"].as_u64().unwrap() > 0);
    assert!(project.join("store").exists());
    let lock = aden_core::lock::FileLock::acquire_timeout(
        project.join("store.lock"),
        std::time::Duration::ZERO,
    )
    .unwrap();
    let refused = run(&root, &data, &["store", "clean", ".", "--apply"]);
    assert!(
        !refused.status.success(),
        "active writer must refuse cleanup"
    );
    assert!(project.join("store").exists());
    drop(lock);
    let clean = run(&root, &data, &["store", "clean", ".", "--apply", "--json"]);
    assert!(
        clean.status.success(),
        "{}",
        String::from_utf8_lossy(&clean.stderr)
    );
    assert!(!project.join("store").exists());
    assert!(!project.join("graph.snapshot").exists());
    assert!(
        project.join("store.lock").exists(),
        "persistent lock inode must survive"
    );
    assert_eq!(
        std::fs::read(root.join(".aden/overlays/keep.adoc")).unwrap(),
        b"intent"
    );
    let query = run(
        &root,
        &data,
        &["locate", "--symbol", "cleanup_symbol", "--json"],
    );
    assert!(
        query.status.success(),
        "{}",
        String::from_utf8_lossy(&query.stderr)
    );
    assert!(String::from_utf8_lossy(&query.stdout).contains("cleanup_symbol"));
    assert!(
        project.join("graph.snapshot").exists(),
        "read must rebuild after cleanup"
    );
}

#[test]
fn preview_is_nonmutating_and_shared_override_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    let data = temp.path().join("data");
    std::fs::create_dir_all(root.join(".aden")).unwrap();
    let preview = run(&root, &data, &["store", "clean"]);
    assert!(preview.status.success());
    assert!(!data.exists(), "empty preview must not create storage");
    let output = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["store", "clean", "--apply"])
        .current_dir(&root)
        .env("ADEN_STORE", &data)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("ADEN_STORE"));
    assert!(!data.exists());
}

#[test]
fn moved_portable_storage_can_be_cleaned_without_losing_intent() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("moved-project");
    let data = root.join(".aden/data");
    std::fs::create_dir_all(data.join("cache")).unwrap();
    std::fs::write(root.join(".aden/storage.toml"), "portable = true\n").unwrap();
    std::fs::write(data.join("cache/generated"), b"cache").unwrap();
    std::fs::write(
        data.join("meta.json"),
        serde_json::to_vec(&serde_json::json!({
            "root": temp.path().join("old-project"), "key": "old-key"
        }))
        .unwrap(),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["store", "clean", "--scope", "cache", "--apply"])
        .current_dir(&root)
        .env_remove("ADEN_DATA_DIR")
        .env_remove("ADEN_STORE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!data.join("cache").exists());
    assert!(root.join(".aden/storage.toml").exists());
    assert!(data.join("meta.json").exists());
}

#[test]
fn malformed_metadata_and_redirected_cache_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    let data = temp.path().join("data");
    std::fs::create_dir_all(root.join(".aden")).unwrap();
    let project = data.join("projects").join(aden_paths::project_key(&root));
    std::fs::create_dir_all(project.join("cache")).unwrap();
    std::fs::write(project.join("cache/keep"), b"keep").unwrap();
    std::fs::write(project.join("meta.json"), b"not-json").unwrap();
    assert!(
        !run(&root, &data, &["store", "clean", "--apply"])
            .status
            .success()
    );
    assert!(project.join("cache/keep").exists());
    std::fs::remove_file(project.join("meta.json")).unwrap();
    let outside = temp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep"), b"outside").unwrap();
    let link = project.join("cache/redirect");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    #[cfg(windows)]
    assert!(Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command",
            "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:ADEN_TEST_LINK -Target $env:ADEN_TEST_TARGET | Out-Null"])
        .env("ADEN_TEST_LINK", &link).env("ADEN_TEST_TARGET", &outside)
        .status().unwrap().success());
    let refused = run(&root, &data, &["store", "clean", "--apply"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("reparse points"));
    assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"outside");
    assert!(project.join("cache/keep").exists());
}
