// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `aden store` — manage the per-user, per-project graph stores introduced by
//! ADR-003. The graph store no longer lives in the project tree; it lives under
//! the per-user data dir keyed by project. These subcommands surface where a
//! store is, enumerate every project's store, reclaim orphans, and migrate
//! legacy in-tree stores.

use crate::util::find_project_root;
use std::path::Path;

/// `aden store path` — print the resolved store directory for `path`'s project.
pub fn cmd_store_path(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let root = find_project_root(path);
    println!("{}", aden_paths::store_dir(&root).display());
    Ok(())
}

/// `aden store list` — enumerate every per-user project store via its meta.json.
pub fn cmd_store_list() -> Result<(), Box<dyn std::error::Error>> {
    let projects = aden_paths::data_root().join("projects");
    if !projects.is_dir() {
        println!("No stores yet ({} does not exist).", projects.display());
        return Ok(());
    }
    let mut found = false;
    for entry in std::fs::read_dir(&projects)? {
        let entry = entry?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        found = true;
        let key = entry.file_name().to_string_lossy().to_string();
        match aden_paths::read_meta(&dir) {
            Some(meta) => {
                let orphaned = !Path::new(&meta.root).exists();
                let size = dir_size(&dir);
                println!(
                    "{key}  {}{}  ({})",
                    meta.root,
                    if orphaned { "  [orphaned]" } else { "" },
                    human_size(size),
                );
            }
            None => println!("{key}  <no meta.json>  ({})", human_size(dir_size(&dir))),
        }
    }
    if !found {
        println!("No stores yet.");
    }
    Ok(())
}

/// `aden store prune` — remove project stores whose recorded root is gone.
pub fn cmd_store_prune(dry_run: bool) -> Result<(), Box<dyn std::error::Error>> {
    let projects = aden_paths::data_root().join("projects");
    if !projects.is_dir() {
        println!("Nothing to prune ({} does not exist).", projects.display());
        return Ok(());
    }
    let mut reclaimed = 0u64;
    for entry in std::fs::read_dir(&projects)? {
        let entry = entry?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        // Only prune entries we can positively identify as orphaned: a readable
        // meta.json whose recorded root no longer exists. Entries without meta
        // are left alone (we can't prove they're orphaned).
        let Some(meta) = aden_paths::read_meta(&dir) else {
            continue;
        };
        if Path::new(&meta.root).exists() {
            continue;
        }
        let size = dir_size(&dir);
        reclaimed += size;
        if dry_run {
            println!(
                "would remove {} ({}) -> {}",
                dir.display(),
                human_size(size),
                meta.root
            );
        } else {
            std::fs::remove_dir_all(&dir)?;
            println!(
                "removed {} ({}) -> {}",
                dir.display(),
                human_size(size),
                meta.root
            );
        }
    }
    println!(
        "{} {}",
        if dry_run {
            "would reclaim"
        } else {
            "reclaimed"
        },
        human_size(reclaimed)
    );
    Ok(())
}

/// `aden store migrate` — explicitly move a legacy in-tree store to central.
pub fn cmd_store_migrate(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let root = find_project_root(path);
    let legacy = aden_paths::legacy_store_dir(&root);
    let central = aden_paths::store_dir(&root);
    if !legacy.is_dir() {
        println!("No legacy in-tree store at {}.", legacy.display());
        return Ok(());
    }
    if central.exists() {
        println!(
            "Central store already exists at {}; leaving legacy store in place.",
            central.display()
        );
        return Ok(());
    }
    crate::util::migrate_legacy_store(&root);
    println!("Store now at {}", central.display());
    Ok(())
}

/// Best-effort recursive byte size of a directory tree.
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => total += dir_size(&entry.path()),
                Ok(ft) if ft.is_file() => {
                    if let Ok(meta) = entry.metadata() {
                        total += meta.len();
                    }
                }
                _ => {}
            }
        }
    }
    total
}

/// Render a byte count as a short human-readable string.
fn human_size(bytes: u64) -> String {
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
