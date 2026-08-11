// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

/// `aden sync`: generate contracts, check references, and scan for drift.
/// All phases run so one failure does not hide later diagnostics, but the final
/// status is an error when any phase failed. Garbage collection is explicit:
/// `--gc` is opt-in because it removes store data.
pub fn cmd_sync(path: &Path, gc: bool, unlimited: bool) -> Result<(), Box<dyn std::error::Error>> {
    println!("Running aden sync on {}...", path.display());
    let mut failures = Vec::new();

    println!("\n[1/3] Generating contracts...");
    if let Err(error) = crate::commands::cmd_gen(path, true) {
        eprintln!("Gen error: {error}");
        failures.push(format!("gen: {error}"));
    }

    println!("\n[2/3] Checking references...");
    if let Err(error) = crate::commands::cmd_check(path, "Warn", false, None) {
        eprintln!("Check error: {error}");
        failures.push(format!("check: {error}"));
    }

    println!("\n[3/3] Scanning for drift...");
    if let Err(error) =
        crate::commands::cmd_heal_scan(path, false, false, false, unlimited, false, None)
    {
        eprintln!("Heal error: {error}");
        failures.push(format!("heal: {error}"));
    }
    if gc {
        println!("\n[gc] Removing deleted symbols/files from the store...");
        if let Err(error) = crate::commands::heal::cmd_heal_gc(path) {
            eprintln!("GC error: {error}");
            failures.push(format!("gc: {error}"));
        }
    }

    if failures.is_empty() {
        println!("\nSync complete!");
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "sync completed with {} failed phase(s): {}",
            failures.len(),
            failures.join("; ")
        ))
        .into())
    }
}
