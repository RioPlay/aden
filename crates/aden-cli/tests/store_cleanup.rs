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
    let (_temp, base) = cleanup_fixture();
    let root = base.join("project");
    let data = base.join("data");
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
    let (_temp, base) = cleanup_fixture();
    let root = base.join("project");
    let data = base.join("data");
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
    let (_temp, base) = cleanup_fixture();
    let root = base.join("moved-project");
    let data = root.join(".aden/data");
    std::fs::create_dir_all(data.join("cache")).unwrap();
    std::fs::write(root.join(".aden/storage.toml"), "portable = true\n").unwrap();
    std::fs::write(data.join("cache/generated"), b"cache").unwrap();
    std::fs::write(
        data.join("meta.json"),
        serde_json::to_vec(&serde_json::json!({
            "root": base.join("old-project"), "key": "old-key"
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
    let (_temp, base) = cleanup_fixture();
    let root = base.join("project");
    let data = base.join("data");
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
    let outside = base.join("outside");
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

fn cleanup_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    // Resolve system aliases before deliberately adding redirects to the fixture.
    let base = aden_paths::strip_verbatim(&temp.path().canonicalize().unwrap());
    (temp, base)
}

#[test]
fn store_prune_cleans_orphans_legacy_fjall_and_empty_shells_while_preserving_active_and_locked() {
    let (_temp, base) = cleanup_fixture();
    let data = base.join("data");
    let projects = data.join("projects");
    let live_legacy_root = base.join("live-legacy");
    let live_sqlite_root = base.join("live-sqlite");
    let live_locked_root = base.join("live-locked");
    let live_future_root = base.join("live-future");
    for dir in [
        &live_legacy_root,
        &live_sqlite_root,
        &live_locked_root,
        &live_future_root,
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }

    let seed = |key: &str, root: Option<&Path>, version: Option<&[u8]>, with_embed: bool| {
        let dir = projects.join(key);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(root) = root {
            std::fs::write(
                dir.join("meta.json"),
                serde_json::to_vec(&serde_json::json!({
                    "root": root,
                    "key": key,
                }))
                .unwrap(),
            )
            .unwrap();
        }
        if let Some(ver) = version {
            std::fs::create_dir_all(dir.join("store")).unwrap();
            std::fs::write(dir.join("store/version"), ver).unwrap();
            std::fs::write(dir.join("store/payload"), vec![9u8; 2048]).unwrap();
            std::fs::write(dir.join("graph.snapshot"), b"snap").unwrap();
            std::fs::write(dir.join("gen-cache.json"), b"{}").unwrap();
        }
        if with_embed {
            std::fs::write(dir.join("embeddings-cache.json"), b"keep-embeddings").unwrap();
        }
        dir
    };

    let missing = base.join("does-not-exist");
    let orphan_legacy = seed("01_orphan_legacy", Some(&missing), Some(b"FJL\x03"), false);
    let live_legacy = seed(
        "02_live_legacy",
        Some(&live_legacy_root),
        Some(b"FJL\x03"),
        true,
    );
    let empty_shell = seed("03_empty_shell", None, None, false);
    let locked_legacy = seed(
        "04_locked_legacy",
        Some(&live_locked_root),
        Some(b"FJL\x02"),
        false,
    );
    let _guard = aden_core::lock::FileLock::acquire_timeout(
        locked_legacy.join("store.lock"),
        std::time::Duration::ZERO,
    )
    .unwrap();
    let live_sqlite = seed(
        "05_live_sqlite",
        Some(&live_sqlite_root),
        Some(b"ADNS\x01"),
        false,
    );
    let future_store = seed(
        "06_future_store",
        Some(&live_future_root),
        Some(b"ADNS\x02"),
        false,
    );

    let dry = run(&base, &data, &["store", "prune", "--dry-run", "--json"]);
    assert!(
        dry.status.success(),
        "{}",
        String::from_utf8_lossy(&dry.stderr)
    );
    let dry_report: serde_json::Value = serde_json::from_slice(&dry.stdout).unwrap();
    assert_eq!(dry_report["dry_run"], true);
    assert_eq!(dry_report["skipped_locked"], 1);
    assert_eq!(dry_report["entries"].as_array().unwrap().len(), 3);
    assert!(orphan_legacy.exists());
    assert!(live_legacy.join("store").exists());
    assert!(empty_shell.exists());

    let applied = run(&base, &data, &["store", "prune", "--json"]);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(report["dry_run"], false);
    assert_eq!(report["skipped_locked"], 1);
    assert!(report["reclaimed_bytes"].as_u64().unwrap() >= 4096);

    assert!(
        !orphan_legacy.exists(),
        "orphaned legacy dir must be removed"
    );
    assert!(!empty_shell.exists(), "empty shell must be removed");
    assert!(live_legacy.exists(), "live root dir must remain");
    assert!(!live_legacy.join("store").exists());
    assert!(!live_legacy.join("graph.snapshot").exists());
    assert!(!live_legacy.join("gen-cache.json").exists());
    assert!(live_legacy.join("meta.json").exists());
    assert_eq!(
        std::fs::read(live_legacy.join("embeddings-cache.json")).unwrap(),
        b"keep-embeddings"
    );
    assert!(locked_legacy.join("store").exists(), "locked dir preserved");
    assert!(live_sqlite.join("store").exists(), "live sqlite preserved");
    assert!(
        future_store.join("store").exists(),
        "future store preserved"
    );
}

#[test]
fn automatic_housekeeping_runs_throttled_after_gen_and_respects_opt_out() {
    let (_temp, base) = cleanup_fixture();
    let root = base.join("proj");
    let data = base.join("data");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("lib.rs"), "pub fn auto_hk() {}\n").unwrap();

    let dead_a = data.join("projects/aaa_dead_fjall");
    std::fs::create_dir_all(dead_a.join("store")).unwrap();
    std::fs::write(
        dead_a.join("meta.json"),
        serde_json::to_vec(&serde_json::json!({
            "root": base.join("deleted-repo-a"),
            "key": "aaa_dead_fjall",
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(dead_a.join("store/version"), b"FJL\x03").unwrap();
    std::fs::write(dead_a.join("store/blob"), vec![1u8; 4096]).unwrap();

    // Opt-out flag skips housekeeping even when enabled.
    let skipped = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["gen", "."])
        .current_dir(&root)
        .env("ADEN_DATA_DIR", &data)
        .env("ADEN_TEST_ENABLE_AUTO_HOUSEKEEPING", "1")
        .env("ADEN_TEST_HOUSEKEEPING_NO_GRACE", "1")
        .env("ADEN_SKIP_HOUSEKEEPING", "1")
        .env_remove("ADEN_STORE")
        .output()
        .unwrap();
    assert!(skipped.status.success());
    assert!(dead_a.exists());

    // First enabled run sweeps dead_a and writes housekeeping.stamp.
    let first = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["gen", ".", "--force-regen"])
        .current_dir(&root)
        .env("ADEN_DATA_DIR", &data)
        .env("ADEN_TEST_ENABLE_AUTO_HOUSEKEEPING", "1")
        .env("ADEN_TEST_HOUSEKEEPING_NO_GRACE", "1")
        .env_remove("ADEN_SKIP_HOUSEKEEPING")
        .env_remove("ADEN_STORE")
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        !dead_a.exists(),
        "first auto housekeeping must sweep dead_a"
    );
    assert!(data.join("housekeeping.stamp").is_file());
    assert!(String::from_utf8_lossy(&first.stdout).contains("Housekeeping reclaimed"));

    // A newly planted dead store within the 24h stamp window is skipped (throttled).
    let dead_b = data.join("projects/bbb_dead_fjall");
    std::fs::create_dir_all(dead_b.join("store")).unwrap();
    std::fs::write(
        dead_b.join("meta.json"),
        serde_json::to_vec(&serde_json::json!({
            "root": base.join("deleted-repo-b"),
            "key": "bbb_dead_fjall",
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(dead_b.join("store/version"), b"FJL\x03").unwrap();

    let second = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["gen", ".", "--force-regen"])
        .current_dir(&root)
        .env("ADEN_DATA_DIR", &data)
        .env("ADEN_TEST_ENABLE_AUTO_HOUSEKEEPING", "1")
        .env("ADEN_TEST_HOUSEKEEPING_NO_GRACE", "1")
        .env_remove("ADEN_SKIP_HOUSEKEEPING")
        .env_remove("ADEN_STORE")
        .output()
        .unwrap();
    assert!(second.status.success());
    assert!(
        dead_b.exists(),
        "second gen within 24h must throttle housekeeping"
    );
}
