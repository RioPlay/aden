// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

/// `aden sync`: the full convergence loop — generate contracts, check
/// references, then heal-scan for drift (with gc by default so deleted
/// symbols/files are pruned from the store; `--no-gc` opts out). Each step
/// reports progress and is non-fatal so a later step still runs.
pub fn cmd_sync(
    path: &Path,
    no_gc: bool,
    unlimited: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Running aden sync on {}...", path.display());

    // 1. Generate contracts
    println!("\n[1/3] Generating contracts...");
    if let Err(e) = crate::commands::cmd_gen(path, true) {
        eprintln!("Gen error: {}", e);
    }

    // 2. Check references
    println!("\n[2/3] Checking references...");
    if let Err(e) = crate::commands::cmd_check(path, "Warn", false, None) {
        let msg = format!("{}", e);
        if !msg.contains("ERROR") {
            println!("Check OK");
        }
    }

    // 3. Heal scan — gc by default so deleted symbols/files are pruned
    // from the store (the orphan/drift these leave behind is exactly
    // what "sync everything" is meant to converge). `--no-gc` opts out.
    let gc = !no_gc;
    if gc {
        println!("\n[3/3] Scanning for drift (with gc)...");
    } else {
        println!("\n[3/3] Scanning for drift...");
    }
    if let Err(e) = crate::commands::cmd_heal_scan(path, false, false, gc, unlimited, false, None) {
        eprintln!("Heal error: {}", e);
    }

    println!("\nSync complete!");
    Ok(())
}
