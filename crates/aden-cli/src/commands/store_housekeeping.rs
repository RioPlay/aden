// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared housekeeping logic for automatic post-generation pruning and `aden store prune`.

use aden_core::lock::{LockState, inspect_lock};
use serde::Serialize;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const HOUSEKEEPING_STAMP: &str = "housekeeping.stamp";
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const GENERATED_ARTIFACTS: [&str; 3] = ["store", "graph.snapshot", "gen-cache.json"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreFormat {
    CurrentSqlite,
    LegacyFjall(u8),
    Empty,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupAction {
    RemoveProjectDir,
    RemoveGeneratedFiles,
}

impl CleanupAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RemoveProjectDir => "remove_project_dir",
            Self::RemoveGeneratedFiles => "remove_generated_files",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PruneEntry {
    pub path: PathBuf,
    pub root: Option<String>,
    pub reason: &'static str,
    pub action: &'static str,
    pub bytes: u64,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct PruneReport {
    pub dry_run: bool,
    pub reclaimed_bytes: u64,
    pub entries: Vec<PruneEntry>,
    pub skipped_locked: usize,
    pub skipped_errors: usize,
}

#[derive(Debug, Clone)]
pub struct PrunePolicy {
    pub dry_run: bool,
    pub prune_orphans: bool,
    pub prune_legacy: bool,
    pub prune_empty_shells: bool,
    pub stale_days: Option<u64>,
    pub orphan_grace: Duration,
    pub legacy_grace: Duration,
    pub empty_shell_grace: Duration,
}

impl PrunePolicy {
    pub fn automatic() -> Self {
        Self {
            dry_run: false,
            prune_orphans: true,
            prune_legacy: true,
            prune_empty_shells: true,
            stale_days: None,
            orphan_grace: Duration::from_secs(24 * 60 * 60),
            legacy_grace: Duration::from_secs(10 * 60),
            empty_shell_grace: Duration::from_secs(7 * 24 * 60 * 60),
        }
    }

    pub fn explicit(dry_run: bool, orphans_only: bool, stale_days: Option<u64>) -> Self {
        Self {
            dry_run,
            prune_orphans: true,
            prune_legacy: !orphans_only,
            prune_empty_shells: !orphans_only,
            stale_days,
            orphan_grace: Duration::ZERO,
            legacy_grace: Duration::ZERO,
            empty_shell_grace: Duration::ZERO,
        }
    }
}

pub fn redirected(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

pub fn validate_ancestors(path: &Path) -> io::Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if redirected(&meta) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cleanup refuses symbolic links and reparse points",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub fn safe_tree_size(path: &Path) -> io::Result<u64> {
    let mut pending = vec![path.to_path_buf()];
    let mut bytes = 0u64;
    while let Some(current) = pending.pop() {
        let meta = fs::symlink_metadata(&current)?;
        if redirected(&meta) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cleanup refuses symbolic links and reparse points",
            ));
        }
        if meta.is_dir() {
            for entry in fs::read_dir(&current)? {
                pending.push(entry?.path());
            }
        } else if meta.is_file() {
            bytes = bytes.saturating_add(meta.len());
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cleanup refuses special files",
            ));
        }
    }
    Ok(bytes)
}

pub fn store_format_kind(store_dir: &Path) -> StoreFormat {
    let Ok(store_meta) = fs::symlink_metadata(store_dir) else {
        return StoreFormat::Empty;
    };
    if redirected(&store_meta) || !store_meta.is_dir() {
        return StoreFormat::Unknown;
    }
    let version_path = store_dir.join("version");
    let Ok(ver_meta) = fs::symlink_metadata(&version_path) else {
        return StoreFormat::Unknown;
    };
    if redirected(&ver_meta) || !ver_meta.is_file() {
        return StoreFormat::Unknown;
    }
    let Ok(mut file) = fs::File::open(&version_path) else {
        return StoreFormat::Unknown;
    };
    let mut header = [0u8; 5];
    let mut len = 0;
    while len < header.len() {
        match file.read(&mut header[len..]) {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(_) => return StoreFormat::Unknown,
        }
    }
    match &header[..len] {
        b"ADNS\x01" => StoreFormat::CurrentSqlite,
        [b'F', b'J', b'L', v @ (2 | 3), ..] => StoreFormat::LegacyFjall(*v),
        _ => StoreFormat::Unknown,
    }
}

pub fn store_format_label(project_dir: &Path) -> &'static str {
    match store_format_kind(&project_dir.join("store")) {
        StoreFormat::CurrentSqlite => "sqlite",
        StoreFormat::LegacyFjall(2) => "legacy:FJL2",
        StoreFormat::LegacyFjall(3) => "legacy:FJL3",
        StoreFormat::LegacyFjall(_) => "legacy",
        StoreFormat::Empty => "empty",
        StoreFormat::Unknown => "unknown",
    }
}

fn is_project_locked(dir: &Path) -> bool {
    let lock_paths = [
        dir.join("store.lock"),
        dir.join("store.generation").join("worker.lock"),
        dir.join("store.generation").join("start.lock"),
    ];
    for lock_path in lock_paths {
        match fs::symlink_metadata(&lock_path) {
            Ok(meta) if redirected(&meta) => return true,
            Ok(_) => match inspect_lock(&lock_path) {
                LockState::Active(_) | LockState::Unreadable(_) => return true,
                LockState::Missing | LockState::Stale(_) | LockState::Malformed => {}
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return true,
        }
    }
    false
}

fn read_meta_strict(dir: &Path) -> Option<aden_paths::Meta> {
    let meta_path = dir.join("meta.json");
    let meta = fs::symlink_metadata(&meta_path).ok()?;
    if redirected(&meta) || !meta.is_file() || meta.len() > 65_536 {
        return None;
    }
    let bytes = fs::read(&meta_path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn newest_activity_time(dir: &Path) -> Option<SystemTime> {
    let candidates = [
        dir.to_path_buf(),
        dir.join("meta.json"),
        dir.join("store"),
        dir.join("store").join("graph.sqlite"),
        dir.join("store").join("version"),
        dir.join("graph.snapshot"),
        dir.join("gen-cache.json"),
        dir.join("freshness.json"),
        dir.join("store.lock.owner"),
    ];
    let mut newest: Option<SystemTime> = None;
    for path in candidates {
        if let Ok(meta) = fs::symlink_metadata(&path)
            && !redirected(&meta)
            && let Ok(modified) = meta.modified()
        {
            newest = Some(match newest {
                Some(prev) => prev.max(modified),
                None => modified,
            });
        }
    }
    newest
}

fn elapsed_since_activity(dir: &Path, now: SystemTime) -> Option<Duration> {
    let modified = newest_activity_time(dir)?;
    now.duration_since(modified).ok()
}

struct PlannedRemoval {
    action: CleanupAction,
    reason: &'static str,
    root: Option<String>,
    targets: Vec<PathBuf>,
    bytes: u64,
}

fn classify_project_dir(
    dir: &Path,
    policy: &PrunePolicy,
    now: SystemTime,
) -> Result<Option<PlannedRemoval>, ()> {
    let store_dir = dir.join("store");
    let format = store_format_kind(&store_dir);
    let is_legacy = matches!(format, StoreFormat::LegacyFjall(_));
    let meta = read_meta_strict(dir);
    let root_str = meta.as_ref().map(|m| m.root.clone());
    let root_exists = meta.as_ref().map(|m| Path::new(&m.root).exists());
    let age = elapsed_since_activity(dir, now).unwrap_or(Duration::ZERO);

    let (action, reason) =
        if policy.prune_orphans && root_exists == Some(false) && age >= policy.orphan_grace {
            let reason = if is_legacy {
                "orphaned_legacy"
            } else {
                "orphaned_root"
            };
            (CleanupAction::RemoveProjectDir, reason)
        } else if policy.prune_legacy && meta.is_none() && is_legacy && age >= policy.legacy_grace {
            (CleanupAction::RemoveProjectDir, "legacy_fjall_no_meta")
        } else if policy.prune_legacy
            && root_exists == Some(true)
            && is_legacy
            && age >= policy.legacy_grace
        {
            (CleanupAction::RemoveGeneratedFiles, "legacy_fjall")
        } else if policy.prune_empty_shells
            && meta.is_none()
            && format == StoreFormat::Empty
            && !dir.join("embeddings-cache.json").exists()
            && age >= policy.empty_shell_grace
        {
            (CleanupAction::RemoveProjectDir, "empty_shell")
        } else if let Some(days) = policy.stale_days
            && root_exists == Some(true)
            && format == StoreFormat::CurrentSqlite
            && age >= Duration::from_secs(days.saturating_mul(86_400))
        {
            (CleanupAction::RemoveGeneratedFiles, "stale_store")
        } else {
            return Ok(None);
        };

    let targets: Vec<PathBuf> = match action {
        CleanupAction::RemoveProjectDir => {
            if safe_tree_size(dir).is_err() {
                return Err(());
            }
            vec![dir.to_path_buf()]
        }
        CleanupAction::RemoveGeneratedFiles => {
            let mut paths = Vec::new();
            for name in GENERATED_ARTIFACTS {
                let candidate = dir.join(name);
                match fs::symlink_metadata(&candidate) {
                    Ok(_) => {
                        if safe_tree_size(&candidate).is_err() {
                            return Err(());
                        }
                        paths.push(candidate);
                    }
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => return Err(()),
                }
            }
            if paths.is_empty() {
                return Ok(None);
            }
            paths
        }
    };

    let mut bytes = 0u64;
    for target in &targets {
        bytes = bytes.saturating_add(safe_tree_size(target).map_err(|_| ())?);
    }

    Ok(Some(PlannedRemoval {
        action,
        reason,
        root: root_str,
        targets,
        bytes,
    }))
}

pub fn prune_projects_dir(
    projects_dir: &Path,
    current_project_dir: Option<&Path>,
    policy: &PrunePolicy,
) -> io::Result<PruneReport> {
    let mut report = PruneReport {
        dry_run: policy.dry_run,
        ..PruneReport::default()
    };
    if !projects_dir.is_dir() {
        return Ok(report);
    }
    if validate_ancestors(projects_dir).is_err() {
        report.skipped_errors += 1;
        return Ok(report);
    }

    let current_canon = current_project_dir.and_then(|p| fs::canonicalize(p).ok());
    let now = SystemTime::now();

    let mut dirs: Vec<PathBuf> = fs::read_dir(projects_dir)?
        .filter_map(|e| e.ok().map(|entry| entry.path()))
        .collect();
    dirs.sort();

    for dir in dirs {
        let Ok(meta) = fs::symlink_metadata(&dir) else {
            continue;
        };
        if redirected(&meta) {
            report.skipped_errors += 1;
            continue;
        }
        if !meta.is_dir() {
            continue;
        }
        if let Some(ref cur) = current_canon
            && fs::canonicalize(&dir).ok().as_ref() == Some(cur)
        {
            continue;
        }
        if is_project_locked(&dir) {
            report.skipped_locked += 1;
            continue;
        }
        let Ok(plan) = classify_project_dir(&dir, policy, now) else {
            report.skipped_errors += 1;
            continue;
        };
        let Some(plan) = plan else {
            continue;
        };

        if !policy.dry_run {
            if is_project_locked(&dir) {
                report.skipped_locked += 1;
                continue;
            }
            let mut removed_ok = true;
            for target in &plan.targets {
                if validate_ancestors(target).is_err() || safe_tree_size(target).is_err() {
                    removed_ok = false;
                    break;
                }
                let res = match fs::symlink_metadata(target) {
                    Ok(m) if m.is_dir() => fs::remove_dir_all(target),
                    Ok(_) => fs::remove_file(target),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e),
                };
                if res.is_err() {
                    removed_ok = false;
                    break;
                }
            }
            if !removed_ok {
                report.skipped_errors += 1;
                continue;
            }
        }

        report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(plan.bytes);
        report.entries.push(PruneEntry {
            path: dir,
            root: plan.root,
            reason: plan.reason,
            action: plan.action.as_str(),
            bytes: plan.bytes,
        });
    }

    Ok(report)
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// Best-effort throttled sweep of legacy Fjall / orphaned stores at most once every 24h.
pub fn maybe_run_automatic_housekeeping(current_project_dir: Option<&Path>, silent: bool) {
    if std::env::var_os("ADEN_STORE").is_some()
        || std::env::var_os("ADEN_SKIP_HOUSEKEEPING").is_some()
    {
        return;
    }
    if std::env::var_os("ADEN_DATA_DIR").is_some() {
        #[cfg(debug_assertions)]
        let test_enabled = std::env::var_os("ADEN_TEST_ENABLE_AUTO_HOUSEKEEPING").as_deref()
            == Some(std::ffi::OsStr::new("1"));
        #[cfg(not(debug_assertions))]
        let test_enabled = false;
        if !test_enabled {
            return;
        }
    }

    let data_root = aden_paths::data_root();
    let projects_dir = data_root.join("projects");
    if !projects_dir.is_dir() {
        return;
    }
    let stamp = data_root.join(HOUSEKEEPING_STAMP);
    let now = SystemTime::now();
    if let Ok(meta) = fs::symlink_metadata(&stamp)
        && !redirected(&meta)
        && let Ok(modified) = meta.modified()
        && let Ok(elapsed) = now.duration_since(modified)
        && elapsed < HOUSEKEEPING_INTERVAL
    {
        return;
    }

    // Best-effort stamp update before sweeping so concurrent gen runs skip redundant work.
    if validate_ancestors(&data_root).is_ok() {
        let secs = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let _ = fs::write(&stamp, format!("{secs}\n"));
    }

    let policy = {
        #[cfg(debug_assertions)]
        {
            let mut p = PrunePolicy::automatic();
            if std::env::var_os("ADEN_TEST_HOUSEKEEPING_NO_GRACE").as_deref()
                == Some(std::ffi::OsStr::new("1"))
            {
                p.orphan_grace = Duration::ZERO;
                p.legacy_grace = Duration::ZERO;
                p.empty_shell_grace = Duration::ZERO;
            }
            p
        }
        #[cfg(not(debug_assertions))]
        {
            PrunePolicy::automatic()
        }
    };

    if let Ok(report) = prune_projects_dir(&projects_dir, current_project_dir, &policy)
        && !silent
        && report.reclaimed_bytes > 0
    {
        println!(
            "Housekeeping reclaimed {} across {} old/orphaned store(s).",
            human_size(report.reclaimed_bytes),
            report.entries.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::FileTimes;

    fn write_meta(dir: &Path, root: &Path) {
        fs::create_dir_all(dir).unwrap();
        let payload = serde_json::json!({
            "root": root.to_string_lossy(),
            "key": dir.file_name().unwrap().to_string_lossy(),
        });
        fs::write(dir.join("meta.json"), serde_json::to_vec(&payload).unwrap()).unwrap();
    }

    fn backdate_tree(dir: &Path, age: Duration) {
        let past = SystemTime::now() - age;
        let times = FileTimes::new().set_modified(past);
        let mut stack = vec![dir.to_path_buf()];
        let mut all = Vec::new();
        while let Some(cur) = stack.pop() {
            if let Ok(read) = fs::read_dir(&cur) {
                for entry in read.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        stack.push(p.clone());
                    }
                    all.push(p);
                }
            }
            all.push(cur);
        }
        // Files first, then directories so child updates don't overwrite parent mtime.
        all.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
        for p in all {
            #[cfg(windows)]
            let opened = {
                use std::os::windows::fs::OpenOptionsExt;
                // FILE_WRITE_ATTRIBUTES (0x100) + FILE_FLAG_BACKUP_SEMANTICS (0x0200_0000)
                // allows updating directory mtimes on Windows as well as file mtimes.
                fs::OpenOptions::new()
                    .access_mode(0x0100)
                    .custom_flags(0x0200_0000)
                    .open(&p)
            };
            #[cfg(not(windows))]
            let opened = fs::File::options().write(true).open(&p);
            if let Ok(f) = opened {
                let _ = f.set_times(times);
            }
        }
    }

    #[test]
    fn automatic_policy_respects_grace_windows_and_reclaims_aged_entries() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        fs::create_dir_all(&projects).unwrap();

        let fresh_orphan = projects.join("fresh_orphan");
        write_meta(&fresh_orphan, &temp.path().join("missing-a"));
        fs::create_dir_all(fresh_orphan.join("store")).unwrap();
        fs::write(fresh_orphan.join("store/version"), b"ADNS\x01").unwrap();

        let fresh_legacy = projects.join("fresh_legacy");
        let live_root = temp.path().join("live-root");
        fs::create_dir_all(&live_root).unwrap();
        write_meta(&fresh_legacy, &live_root);
        fs::create_dir_all(fresh_legacy.join("store")).unwrap();
        fs::write(fresh_legacy.join("store/version"), b"FJL\x03").unwrap();

        let aged_legacy = projects.join("aged_legacy");
        write_meta(&aged_legacy, &live_root);
        fs::create_dir_all(aged_legacy.join("store")).unwrap();
        fs::write(aged_legacy.join("store/version"), b"FJL\x03").unwrap();
        fs::write(aged_legacy.join("store/0.jnl"), vec![7u8; 4096]).unwrap();
        fs::write(aged_legacy.join("embeddings-cache.json"), b"keep").unwrap();
        backdate_tree(&aged_legacy, Duration::from_secs(3600));

        let report = prune_projects_dir(&projects, None, &PrunePolicy::automatic()).unwrap();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].path, aged_legacy);
        assert_eq!(report.entries[0].reason, "legacy_fjall");
        assert!(fresh_orphan.exists(), "fresh orphan inside 24h grace kept");
        assert!(fresh_legacy.exists(), "fresh legacy inside 10m grace kept");
        assert!(!aged_legacy.join("store").exists());
        assert!(aged_legacy.join("embeddings-cache.json").exists());
        assert!(aged_legacy.join("meta.json").exists());
    }

    #[test]
    fn unknown_and_future_store_markers_are_never_treated_as_legacy() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        let live_root = temp.path().join("live-root");
        fs::create_dir_all(&live_root).unwrap();

        let future = projects.join("future_store");
        write_meta(&future, &live_root);
        fs::create_dir_all(future.join("store")).unwrap();
        fs::write(future.join("store/version"), b"ADNS\x02").unwrap();
        fs::write(future.join("store/data"), b"keep").unwrap();

        let report =
            prune_projects_dir(&projects, None, &PrunePolicy::explicit(false, false, None))
                .unwrap();
        assert!(report.entries.is_empty());
        assert!(future.join("store/data").exists());
    }

    #[test]
    fn housekeeping_skips_redirected_targets_without_touching_outside_files() {
        let temp = tempfile::tempdir().unwrap();
        let base = aden_paths::strip_verbatim(&temp.path().canonicalize().unwrap());
        let projects = base.join("projects");
        let outside = base.join("outside");
        fs::create_dir_all(&projects).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret"), b"intact").unwrap();

        let cand = projects.join("redirected_proj");
        write_meta(&cand, &base.join("missing-root"));
        let link = cand.join("store");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        assert!(
            std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:ADEN_TEST_LINK -Target $env:ADEN_TEST_TARGET | Out-Null",
                ])
                .env("ADEN_TEST_LINK", &link)
                .env("ADEN_TEST_TARGET", &outside)
                .status()
                .unwrap()
                .success()
        );

        let report =
            prune_projects_dir(&projects, None, &PrunePolicy::explicit(false, false, None))
                .unwrap();
        assert!(report.entries.is_empty());
        assert_eq!(report.skipped_errors, 1);
        assert_eq!(fs::read(outside.join("secret")).unwrap(), b"intact");
    }
}
