// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

use crate::util::find_project_root;

fn coverage_summary(root: &Path) -> serde_json::Value {
    let cache = crate::util::load_gen_cache(&aden_paths::gen_cache_file(root));
    let mut dispositions = serde_json::Map::new();
    dispositions.insert("indexed".into(), serde_json::json!(cache.entries.len()));
    for entry in cache.dispositions.values() {
        let key = match entry.disposition {
            aden_core::filter::FileDisposition::Indexed => "indexed",
            aden_core::filter::FileDisposition::Ignored => "ignored",
            aden_core::filter::FileDisposition::Redacted => "redacted",
            aden_core::filter::FileDisposition::SecretPath => "secret_path",
            aden_core::filter::FileDisposition::SecretContent => "secret_content",
            aden_core::filter::FileDisposition::Unsupported => "unsupported",
            aden_core::filter::FileDisposition::InvalidEncoding => "invalid_encoding",
            aden_core::filter::FileDisposition::ParseFailed => "parse_failed",
            aden_core::filter::FileDisposition::IoFailed => "io_failed",
        };
        let count = dispositions
            .entry(key)
            .or_insert_with(|| serde_json::json!(0));
        *count = serde_json::json!(count.as_u64().unwrap_or(0) + 1);
    }
    serde_json::Value::Object(dispositions)
}

/// `aden status`: a quick health + orphan snapshot. Health is the
/// heal-drift metric (stale docs vs. code); orphans use the SAME classifier
/// `check` uses, so expected metadata docs are never reported as scary orphans.
/// Honors the global `-j/--json` flag.
pub fn cmd_status(path: &Path, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let aden_path = path.join(".aden");
    let root = find_project_root(path);
    let store_path = aden_paths::store_dir(&root);
    let lock_path = aden_paths::store_lock_file(&root);
    let snapshot_path = aden_paths::graph_snapshot_file(&root);
    let coverage = coverage_summary(&root);

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

    // Machine-readable for the global `-j/--json` flag (MCP Phase 2B envelope).
    if json {
        let ok = health >= 0.8 && actionable.is_empty();
        let top_issues: Vec<String> = actionable
            .iter()
            .take(20)
            .map(|a| format!("orphan: {a}"))
            .collect();
        let policy = aden_policy::audit_policy(path);
        let outcome = crate::commands::outcome::OutcomeEnvelope::evaluated(
            if health < 0.8 { 1 } else { 0 },
            actionable.len() + policy.violations.len(),
            if health < 0.8 { "unhealthy" } else { "healthy" },
            crate::commands::outcome::policy_label(policy.violations.len(), policy.unwired),
            "not_evaluated",
        );
        let env = serde_json::json!({
            "ok": ok,
            "counts": {
                "errors": if health < 0.8 { 1usize } else { 0usize },
                "warnings": actionable.len(),
                "info": expected_n,
            },
            "top_issues": top_issues,
            "truncated": actionable.len() > 20,
            "path": path.display().to_string(),
            "aden_dir": aden_path.display().to_string(),
            "store": store_path.display().to_string(),
            "store_writer": aden_core::lock::read_holder(&lock_path).map(|h| {
                serde_json::json!({
                    "pid": h.pid,
                    "held_secs": std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs().saturating_sub(h.acquired_secs))
                        .unwrap_or(0),
                })
            }),
            "read_snapshot": snapshot_path
                .is_file()
                .then(|| snapshot_path.display().to_string()),
            "health_score": health,
            "health": health_pct,
            "orphans": {
                "expected": expected_n,
                "actionable_count": actionable.len(),
            },
            "policy_mode": policy.mode,
            "policy_violations": policy.violations,
            "coverage": coverage,
            "result": outcome,
        });
        println!("{}", serde_json::to_string(&env)?);
        return Ok(());
    }

    println!("Aden Status: {}", path.display());
    println!("Active .aden: {}", aden_path.display());
    println!("Store: {}", store_path.display());
    println!("Coverage: {}", coverage);
    if let Some(holder) = aden_core::lock::read_holder(&lock_path) {
        println!(
            "Store writer: active ({})",
            aden_core::lock::describe_holder(holder)
        );
    }
    if snapshot_path.is_file() {
        println!("Read snapshot: {}", snapshot_path.display());
    }
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

    Ok(())
}
