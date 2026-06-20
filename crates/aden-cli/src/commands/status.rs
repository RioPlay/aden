// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

use crate::util::find_project_root;

/// `aden status`: a quick health + orphan + savings snapshot. Health is the
/// heal-drift metric (stale docs vs. code); orphans use the SAME classifier
/// `check` uses, so expected metadata docs are never reported as scary orphans.
/// Honors the global `-j/--json` flag.
pub fn cmd_status(path: &Path, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let aden_path = path.join(".aden");

    // Health is a heal-drift metric (stale docs vs. code), separate
    // from orphans. Keep it as the honest drift signal.
    let health = crate::util::quick_health_score(path).unwrap_or(0.0);
    let health_pct = (health * 100.0).round() as i32;

    // Orphan breakdown via the SAME classifier `check` uses, so status
    // never reports expected metadata docs as scary orphans. Computed once.
    let (expected_n, actionable): (usize, Vec<String>) =
        match aden_graph::cache::build_from_directory_cached(path) {
            Ok(g) => {
                let (expected, actionable) = crate::util::classify_orphans(&g);
                (expected.len(), actionable)
            }
            Err(_) => (0, Vec::new()),
        };

    // Machine-readable for the global `-j/--json` flag (previously ignored).
    if json {
        let env = serde_json::json!({
            "path": path.display().to_string(),
            "aden_dir": aden_path.display().to_string(),
            "store": aden_paths::store_dir(&find_project_root(path)).display().to_string(),
            "health_score": health,
            "health": health_pct,
            "orphans": {
                "expected": expected_n,
                "actionable_count": actionable.len(),
                "actionable": actionable,
            },
        });
        println!("{}", serde_json::to_string_pretty(&env)?);
        return Ok(());
    }

    println!("Aden Status: {}", path.display());
    println!("Active .aden: {}", aden_path.display());
    println!(
        "Store: {}",
        aden_paths::store_dir(&find_project_root(path)).display()
    );
    let emoji = if health >= 0.95 {
        "✅"
    } else if health >= 0.8 {
        "⚠️"
    } else {
        "❌"
    };
    println!("{} Health: {}/100", emoji, health_pct);

    if actionable.is_empty() {
        if expected_n == 0 {
            println!("✅ No orphan documents");
        } else {
            println!(
                "✅ No actionable orphans ({} expected metadata doc(s), which is normal)",
                expected_n
            );
        }
    } else {
        println!(
            "⚠️ {} actionable orphan document(s) (run 'aden heal . --gc' to remove if deleted)",
            actionable.len()
        );
        if expected_n > 0 {
            println!("   (plus {} expected metadata doc(s) — normal)", expected_n);
        }
    }

    // Savings summary from the persistent ledger: this session + all-time.
    let repo_root = find_project_root(path);
    let summary = crate::commands::savings_store::load_summary(&repo_root);
    if summary.all_time.queries == 0 {
        println!("Savings (est.): no queries recorded yet");
    } else {
        use aden_core::savings::humanize_count;
        let line = |label: &str, l: &crate::commands::savings_store::SavingsLedger| {
            println!(
                "{label}: {} aden call{} → est. ~{} tool calls + ~{} tokens saved vs grep-and-read",
                l.queries,
                if l.queries == 1 { "" } else { "s" },
                l.tool_calls_saved,
                humanize_count(l.saved_tokens),
            );
        };
        println!("Savings estimate (vs grep-and-read) [est.]:");
        line("  This session", &summary.session);
        line("  All-time    ", &summary.all_time);
    }

    Ok(())
}
