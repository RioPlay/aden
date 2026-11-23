// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `aden store` — manage the per-user, per-project graph stores introduced by
//! ADR-003. The graph store no longer lives in the project tree; it lives under
//! the per-user data dir keyed by project. These subcommands surface where a
//! store is, enumerate every project's store, reclaim orphans and legacy engines,
//! and migrate legacy in-tree stores.

use crate::util::find_project_root;
use std::path::Path;

#[path = "store_clean.rs"]
mod clean;
#[path = "store_housekeeping.rs"]
pub mod housekeeping;

pub use clean::cmd_store_clean;

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
    let mut count = 0usize;
    let mut total_bytes = 0u64;
    let mut entries: Vec<_> = std::fs::read_dir(&projects)?
        .filter_map(|e| e.ok().map(|entry| entry.path()))
        .collect();
    entries.sort();

    for dir in entries {
        if !dir.is_dir() {
            continue;
        }
        count += 1;
        let key = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let format = housekeeping::store_format_label(&dir);
        let size = dir_size(&dir);
        total_bytes = total_bytes.saturating_add(size);
        match aden_paths::read_meta(&dir) {
            Some(meta) => {
                let orphaned = !Path::new(&meta.root).exists();
                println!(
                    "{key}  [{format}]{}  {}  ({})",
                    if orphaned { " [orphaned]" } else { "" },
                    meta.root,
                    human_size(size),
                );
            }
            None => println!("{key}  [{format}]  <no meta.json>  ({})", human_size(size)),
        }
    }
    if count == 0 {
        println!("No stores yet.");
    } else {
        println!("Total: {count} store(s), {}", human_size(total_bytes));
    }
    Ok(())
}

/// `aden store prune` — remove orphaned project stores, unreadable legacy
/// Fjall stores, empty shells, and (when `--stale-days` is set) dormant stores.
pub fn cmd_store_prune(
    dry_run: bool,
    orphans_only: bool,
    stale_days: Option<u64>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let projects = aden_paths::data_root().join("projects");
    let policy = housekeeping::PrunePolicy::explicit(dry_run, orphans_only, stale_days);
    let report = housekeeping::prune_projects_dir(&projects, None, &policy)?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "schema_version": 1,
                "dry_run": report.dry_run,
                "reclaimed_bytes": report.reclaimed_bytes,
                "skipped_locked": report.skipped_locked,
                "skipped_errors": report.skipped_errors,
                "entries": report.entries,
            })
        );
        return Ok(());
    }

    if !projects.is_dir() {
        println!("Nothing to prune ({} does not exist).", projects.display());
        return Ok(());
    }

    for entry in &report.entries {
        let verb = if dry_run { "would prune" } else { "pruned" };
        let target_label = entry.root.as_deref().unwrap_or("<no meta.json>");
        println!(
            "{verb} {} [{}] [{}] ({}) -> {}",
            entry.path.display(),
            entry.reason,
            entry.action,
            human_size(entry.bytes),
            target_label,
        );
    }
    println!(
        "{} {} across {} store(s){}{}.",
        if dry_run {
            "would reclaim"
        } else {
            "reclaimed"
        },
        human_size(report.reclaimed_bytes),
        report.entries.len(),
        if report.skipped_locked > 0 {
            format!("; skipped {} locked", report.skipped_locked)
        } else {
            String::new()
        },
        if report.skipped_errors > 0 {
            format!("; skipped {} error(s)", report.skipped_errors)
        } else {
            String::new()
        },
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

/// Best-effort recursive byte size of a directory tree without following symlinks/junctions.
fn dir_size(dir: &Path) -> u64 {
    housekeeping::safe_tree_size(dir).unwrap_or(0)
}

/// Render a byte count as a short human-readable string.
fn human_size(bytes: u64) -> String {
    housekeeping::human_size(bytes)
}
