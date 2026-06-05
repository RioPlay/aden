// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;
use std::path::PathBuf;

use crate::util::{find_project_root, generate_proposal_id, is_safe_id};

/// Render a drift event as a single concise line.
///
/// The derived `Debug` form inlines bulky fields — most painfully
/// `StaleMarkdown`, which lists every changed source file — so printing one
/// event per line with `{:?}` floods an agent's context with thousands of
/// lines. This keeps each event to its identifying path/anchor plus a count.
fn summarize_drift_event(event: &aden_heal::DriftEvent) -> String {
    use aden_heal::DriftEvent::*;
    match event {
        StaleHash { target_path, .. } => format!("StaleHash: {}", target_path),
        SignatureMismatch { anchor, .. } => format!("SignatureMismatch: {}", anchor),
        MissingContract {
            symbol_name,
            source_path,
            ..
        } => {
            format!("MissingContract: {} ({})", symbol_name, source_path)
        }
        OrphanAnchor { anchor, .. } => format!("OrphanAnchor: {}", anchor),
        BrokenReference {
            contract_path,
            ref_anchor,
            line,
        } => {
            format!(
                "BrokenReference: <<{}>> in {}:{}",
                ref_anchor, contract_path, line
            )
        }
        DeadLink {
            contract_path,
            include_path,
        } => {
            format!("DeadLink: {} -> {}", contract_path, include_path)
        }
        MarkdownDrift { md_path, .. } => format!("MarkdownDrift: {}", md_path),
        StaleMarkdown {
            md_path,
            source_files_changed,
        } => {
            format!(
                "StaleMarkdown: {} ({} source file(s) changed)",
                md_path,
                source_files_changed.len()
            )
        }
        MissingMarkdownTemplate {
            md_path,
            template_source,
        } => {
            format!(
                "MissingMarkdownTemplate: {} (from {})",
                md_path, template_source
            )
        }
        DocSignatureDivergence {
            doc_path,
            line,
            symbol_name,
            documented_params,
            actual_params,
        } => {
            format!(
                "DocSignatureDivergence: {}() documents {} param(s) but code has {} — {}:{}",
                symbol_name, documented_params, actual_params, doc_path, line
            )
        }
    }
}

/// The drift-event variant name, for aggregating skip counts by kind.
fn drift_kind_name(event: &aden_heal::DriftEvent) -> &'static str {
    use aden_heal::DriftEvent::*;
    match event {
        StaleHash { .. } => "StaleHash",
        SignatureMismatch { .. } => "SignatureMismatch",
        MissingContract { .. } => "MissingContract",
        OrphanAnchor { .. } => "OrphanAnchor",
        BrokenReference { .. } => "BrokenReference",
        DeadLink { .. } => "DeadLink",
        MarkdownDrift { .. } => "MarkdownDrift",
        StaleMarkdown { .. } => "StaleMarkdown",
        MissingMarkdownTemplate { .. } => "MissingMarkdownTemplate",
        DocSignatureDivergence { .. } => "DocSignatureDivergence",
    }
}

pub fn cmd_heal_scan_since(
    path: &Path,
    propose: bool,
    since: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_heal::{Scanner, generate};

    println!("Aden Incremental Scan (since {})", since);
    println!("================================");

    // Get changed files from git
    // SECURITY: `--` terminates option parsing to prevent argument confusion.
    let output = std::process::Command::new("git")
        .args(["diff", "--name-only", "--", since])
        .current_dir(path)
        .output()?;

    let changed = String::from_utf8_lossy(&output.stdout);
    let files: Vec<&str> = changed.lines().filter(|l| !l.is_empty()).collect();

    if files.is_empty() {
        println!("No files changed since {}. Nothing to scan.", since);
        return Ok(());
    }

    println!("Scanning {} changed files...", files.len());

    // Run targeted drift scan
    let scanner = Scanner::new(path);
    let all_events = scanner.scan()?;

    // Filter to changed files
    let relevant_events: Vec<aden_heal::DriftEvent> = all_events
        .into_iter()
        .filter(|e| {
            let target = match e {
                aden_heal::DriftEvent::StaleHash { target_path, .. } => target_path,
                aden_heal::DriftEvent::SignatureMismatch { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::MissingContract { source_path, .. } => source_path,
                aden_heal::DriftEvent::OrphanAnchor { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::BrokenReference { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::DeadLink { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::MarkdownDrift { md_path, .. } => md_path,
                aden_heal::DriftEvent::StaleMarkdown { md_path, .. } => md_path,
                aden_heal::DriftEvent::MissingMarkdownTemplate { md_path, .. } => md_path,
                aden_heal::DriftEvent::DocSignatureDivergence { doc_path, .. } => doc_path,
            };
            files.iter().any(|f| target.contains(f))
        })
        .collect();

    let report = generate(relevant_events.clone(), path);
    println!("Health Score: {:.2}/1.00", report.overall_score);
    println!("Drift Events: {}", report.events.len());

    if propose && !report.events.is_empty() {
        println!("Generating proposals...");
        let proposals_dir = path.join(".aden").join("proposals");
        clear_stale_proposals(&proposals_dir)?;
        for event in &report.events {
            let proposal = generate_proposal(event, path)?;
            let store_path = aden_propose::persist(&proposal, path)?;
            println!("  Generated: {}", store_path.display());
        }
    }

    Ok(())
}

pub fn cmd_heal_scan(
    path: &Path,
    propose: bool,
    fix: bool,
    gc: bool,
    unlimited: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_heal::{Scanner, generate};

    if gc {
        return cmd_heal_gc(path);
    }

    println!("Aden Self-Healing Documentation Engine");
    println!("========================================");
    println!("Scanning: {}", path.display());
    println!();

    let scanner = Scanner::new(path);
    match scanner.scan() {
        Ok(events) => {
            let report = generate(events.clone(), path);

            println!("Health Score: {:.2}/1.00", report.overall_score);
            println!("Total Drift Events: {}", report.events.len());
            println!();

            if report.events.is_empty() {
                println!("INFO: No drift detected. Documentation is healthy.");
                return Ok(());
            }

            // Group by severity
            let mut critical = Vec::new();
            let mut high = Vec::new();
            let mut medium = Vec::new();
            let mut low = Vec::new();

            for event in &report.events {
                match event.severity() {
                    aden_heal::DriftSeverity::Critical => critical.push(event),
                    aden_heal::DriftSeverity::High => high.push(event),
                    aden_heal::DriftSeverity::Medium => medium.push(event),
                    aden_heal::DriftSeverity::Low => low.push(event),
                }
            }

            // Cap how many events are listed per severity group so a large repo
            // doesn't bury the agent in thousands of lines (a single
            // StaleMarkdown debug-prints its entire changed-files list). The
            // count is always shown; pass --unlimited for the full enumeration.
            const HEAL_GROUP_CAP: usize = 5;
            let print_group = |name: &str, events: &Vec<&aden_heal::DriftEvent>, _path: &Path| {
                if !events.is_empty() {
                    println!("\n=== {} ({} events) ===", name, events.len());
                    let shown = if unlimited {
                        events.len()
                    } else {
                        events.len().min(HEAL_GROUP_CAP)
                    };
                    for (i, event) in events.iter().take(shown).enumerate() {
                        println!("  {}. {}", i + 1, summarize_drift_event(event));

                        let fix_hint = match event {
                            aden_heal::DriftEvent::StaleHash { target_path, .. } => {
                                let rel = PathBuf::from(
                                    target_path.strip_prefix("./").unwrap_or(target_path),
                                );
                                if rel.starts_with("crates/") {
                                    let parts: Vec<_> =
                                        rel.iter().map(|s| s.to_string_lossy()).collect();
                                    if parts.len() >= 3 && parts[0] == "crates" {
                                        let crate_name = &parts[1];
                                        Some(format!(
                                            "  Hint: aden gen {} --out-dir contracts/crates/{}/src/",
                                            target_path, crate_name
                                        ))
                                    } else {
                                        None
                                    }
                                } else {
                                    Some(format!(
                                        "  Hint: aden gen {} --out-dir contracts/",
                                        target_path
                                    ))
                                }
                            }
                            aden_heal::DriftEvent::MissingContract { source_path, .. } => {
                                Some(format!("  Hint: aden gen {} --auto .", source_path))
                            }
                            aden_heal::DriftEvent::BrokenReference {
                                contract_path: _,
                                ref_anchor,
                                ..
                            } => Some(format!(
                                "  Hint: aden search {} to find correct anchor",
                                ref_anchor
                            )),
                            aden_heal::DriftEvent::StaleMarkdown { md_path: _, .. } => Some(
                                "  Hint: aden gen . --format md --out-dir . to regenerate markdown"
                                    .to_string(),
                            ),
                            _ => None,
                        };

                        if let Some(hint) = fix_hint {
                            println!("{}", hint);
                        }
                    }
                    if shown < events.len() {
                        println!(
                            "  ... and {} more (run 'aden heal . --unlimited' for the full list)",
                            events.len() - shown
                        );
                    }
                }
            };

            print_group("CRITICAL", &critical, path);
            print_group("HIGH", &high, path);
            print_group("MEDIUM", &medium, path);
            print_group("LOW", &low, path);

            if fix {
                use std::collections::BTreeMap;
                println!("\n--fix flag set. Attempting auto-fix...");
                let mut fixed_count = 0;
                let mut failed_count = 0;
                // Aggregate skipped events by drift kind instead of printing one
                // noisy line per event (a large repo has hundreds of orphans).
                let mut skipped_by_kind: BTreeMap<&'static str, usize> = BTreeMap::new();

                for event in &report.events {
                    let confidence = match event {
                        aden_heal::DriftEvent::StaleHash { .. } => 0.99,
                        aden_heal::DriftEvent::MissingContract { .. } => 0.85,
                        aden_heal::DriftEvent::SignatureMismatch { .. } => 0.90,
                        aden_heal::DriftEvent::StaleMarkdown { .. } => 0.80,
                        aden_heal::DriftEvent::MarkdownDrift { .. } => 0.75,
                        _ => 0.0,
                    };

                    if confidence < 0.8 {
                        *skipped_by_kind.entry(drift_kind_name(event)).or_default() += 1;
                        failed_count += 1;
                        continue;
                    }

                    match event {
                        aden_heal::DriftEvent::StaleHash {
                            target_path,
                            actual_hash,
                            ..
                        } => {
                            let target = PathBuf::from(target_path);
                            if target.exists() {
                                let content = std::fs::read_to_string(&target)?;
                                let updated = content
                                    .lines()
                                    .map(|line| {
                                        if line.trim_start().starts_with(":source_hash:") {
                                            format!(":source_hash: {}", actual_hash)
                                        } else {
                                            line.to_string()
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                std::fs::write(&target, updated)?;
                                println!("  Fixed: {} (updated hash)", target.display());
                                fixed_count += 1;
                            } else {
                                // Store-first StaleHash: target_path is a store
                                // pseudo-path (".aden/store:aden://…"), not a file
                                // on disk, so there is no :source_hash: line for
                                // heal to rewrite — the store's recorded hash is
                                // refreshed by `aden gen`, not heal. Count it as a
                                // skipped event with guidance so it never silently
                                // vanishes from both the Fixed and Skipped tallies
                                // (the prior behaviour: dropped from each).
                                *skipped_by_kind
                                    .entry("StaleHash in store (run `aden gen` to refresh)")
                                    .or_default() += 1;
                                failed_count += 1;
                            }
                        }
                        aden_heal::DriftEvent::MissingContract { .. } => {
                            // Aden is store-first: contracts live in .aden/store,
                            // not as disk .adoc stubs. Stub files have no face value,
                            // immediately show as StaleHash drift, and clutter the
                            // repo. Missing contracts are regenerated by `aden gen`,
                            // not by heal --fix. Always skip.
                            *skipped_by_kind.entry("MissingContract").or_default() += 1;
                        }
                        aden_heal::DriftEvent::SignatureMismatch {
                            anchor,
                            contract_path,
                            expected_sig: _,
                            actual_sig,
                        } => {
                            // Store-first: the scanner emits a pseudo-path of the
                            // form ".aden/store:<anchor>" — there is no on-disk
                            // contract file to rewrite. Update the signature in the
                            // store via the graph API instead. Any failure is logged
                            // and counted as skipped, never `?`-aborting the whole run.
                            if let Some(store_anchor) = contract_path.strip_prefix(".aden/store:") {
                                match update_store_signature(path, store_anchor, actual_sig) {
                                    Ok(true) => {
                                        println!(
                                            "  Fixed: {} (updated signature in store)",
                                            anchor
                                        );
                                        fixed_count += 1;
                                    }
                                    Ok(false) => {
                                        eprintln!(
                                            "  WARN: signature anchor {} not found in store; skipping",
                                            anchor
                                        );
                                        *skipped_by_kind
                                            .entry("SignatureMismatch (anchor not in store)")
                                            .or_default() += 1;
                                        failed_count += 1;
                                    }
                                    Err(e) => {
                                        eprintln!(
                                            "  WARN: failed to update signature for {}: {}; skipping",
                                            anchor, e
                                        );
                                        *skipped_by_kind
                                            .entry("SignatureMismatch (store update failed)")
                                            .or_default() += 1;
                                        failed_count += 1;
                                    }
                                }
                            } else {
                                // Legacy on-disk contract path: rewrite the
                                // :source_sig: line in place, but tolerate a missing
                                // file rather than aborting the whole --fix run.
                                let sig_str = actual_sig.join(",");
                                match std::fs::read_to_string(contract_path) {
                                    Ok(content) => {
                                        let updated = content
                                            .lines()
                                            .map(|line| {
                                                if line.trim_start().starts_with(":source_sig:") {
                                                    format!(":source_sig: {}", sig_str)
                                                } else {
                                                    line.to_string()
                                                }
                                            })
                                            .collect::<Vec<_>>()
                                            .join("\n");
                                        if let Err(e) = std::fs::write(contract_path, updated) {
                                            eprintln!(
                                                "  WARN: failed to write {}: {}; skipping",
                                                contract_path, e
                                            );
                                            *skipped_by_kind
                                                .entry("SignatureMismatch (write failed)")
                                                .or_default() += 1;
                                            failed_count += 1;
                                        } else {
                                            println!(
                                                "  Fixed: {} (updated signature)",
                                                contract_path
                                            );
                                            fixed_count += 1;
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!(
                                            "  WARN: cannot read {}: {}; skipping",
                                            contract_path, e
                                        );
                                        *skipped_by_kind
                                            .entry("SignatureMismatch (file missing)")
                                            .or_default() += 1;
                                        failed_count += 1;
                                    }
                                }
                            }
                        }
                        _ => {
                            *skipped_by_kind.entry(drift_kind_name(event)).or_default() += 1;
                            failed_count += 1;
                        }
                    }
                }

                println!("\nFixed: {} event(s)", fixed_count);
                if failed_count > 0 {
                    println!(
                        "Skipped: {} event(s) requiring manual review:",
                        failed_count
                    );
                    for (kind, count) in &skipped_by_kind {
                        println!("  {} × {}", count, kind);
                    }
                    println!("  (run 'aden heal . --propose' to generate patches for review)");
                }

                return Ok(());
            }

            if propose {
                println!("\n--propose flag set. Generating patches...");
                let store_dir = path.join(".aden").join("proposals");
                clear_stale_proposals(&store_dir)?;

                for event in &report.events {
                    let proposal = generate_proposal(event, path)?;
                    let store_path = aden_propose::persist(&proposal, path)?;
                    println!("  Generated proposal: {}", store_path.display());
                }
                println!("\nReview proposals in: {}", store_dir.display());
                println!("Apply with: aden heal --apply <proposal-id>");
            } else {
                println!("\nRun with --propose to generate patch files for review.");
                println!(
                    "Or use --fix to repair on-disk contract hashes/signatures; \
                     store-resident drift (StaleHash/MissingContract) is refreshed by `aden gen`."
                );
            }

            Ok(())
        }
        Err(e) => {
            eprintln!("ERROR: Scan failed: {}", e);
            Err(e.into())
        }
    }
}

/// Update the recorded signature of a store-resident contract to `actual_sig`.
///
/// The scanner derives a contract's "signature" from the `param ` rows of the
/// doc's Table block(s) (see `extract_sig_from_doc`). To heal a
/// `SignatureMismatch` we rewrite those `param ` rows' value column in the
/// store document and persist it via `put_document`. Returns `Ok(true)` if the
/// anchor existed and was updated, `Ok(false)` if no such anchor is in the store.
fn update_store_signature(
    path: &Path,
    anchor: &str,
    actual_sig: &[String],
) -> Result<bool, Box<dyn std::error::Error>> {
    use aden_core::Block;
    use aden_store::{GraphStorage, Storage};

    let root = find_project_root(path);
    let (store_path, _) = aden_paths::resolve_read_store(&root);
    if !store_path.is_dir() {
        return Ok(false);
    }
    let storage = Storage::open_existing(store_path.to_str().ok_or("invalid store path")?)?;

    let mut doc = match storage.get_document(anchor)? {
        Some(d) => d,
        None => return Ok(false),
    };

    // Replace the value column of each `param ` row in order, in document order,
    // across all Table blocks — matching how `extract_sig_from_doc` reads them.
    let mut next = 0usize;
    for block in &mut doc.blocks {
        if let Block::Table(table) = block {
            for row in &mut table.rows {
                if row.len() >= 2 && row[0].starts_with("param ") {
                    if let Some(val) = actual_sig.get(next) {
                        row[1] = val.clone();
                    }
                    next += 1;
                }
            }
        }
    }

    storage.put_document(&doc)?;
    storage.flush()?;
    Ok(true)
}

pub fn cmd_heal_apply(repo_path: &Path, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !is_safe_id(id) {
        return Err(format!("Invalid proposal ID: {}", id).into());
    }

    let proposal = aden_propose::load(id, repo_path)
        .map_err(|e| format!("Failed to load proposal '{}': {}", id, e))?;

    println!("Applying proposal: {}", id);
    println!("  Drift type: {}", proposal.drift_type);
    println!("  Target: {}", proposal.target_path.display());
    println!("  Confidence: {:.2}", proposal.confidence);
    println!();

    if proposal.confidence < 0.9 {
        println!(
            "WARNING: Low-confidence proposal ({:.2}). Review carefully.",
            proposal.confidence
        );
    }

    // Dispatch based on drift type
    match proposal.drift_type.as_str() {
        "StaleHash" => apply_stale_hash(&proposal)?,
        "MissingContract" => apply_missing_contract(&proposal)?,
        "BrokenReference" => {
            println!("BrokenReference requires manual review. The patch content:");
            println!("---");
            println!("{}", proposal.patch_asciidoc);
            println!("---");
            println!("Cannot auto-apply: requires finding the correct replacement anchor.");
        }
        other => {
            println!("Unknown drift type '{}'. Cannot auto-apply.", other);
            println!("Patch content:");
            println!("---");
            println!("{}", proposal.patch_asciidoc);
            println!("---");
        }
    }

    // Mark proposal as applied in the store
    let mut updated = proposal;
    updated.status = aden_propose::ProposalStatus::Applied;
    aden_propose::persist(&updated, repo_path)?;

    println!("\nProposal {} marked as APPLIED.", id);
    Ok(())
}

pub fn apply_stale_hash(
    proposal: &aden_propose::Proposal,
) -> Result<(), Box<dyn std::error::Error>> {
    let target = &proposal.target_path;
    if !target.exists() {
        return Err(format!("Target file not found: {}", target.display()).into());
    }

    let content = std::fs::read_to_string(target)?;
    let new_line = proposal.patch_asciidoc.trim();

    if !new_line.starts_with(":source_hash:") {
        return Err("Patch does not contain a valid :source_hash: line".into());
    }

    let updated = content
        .lines()
        .map(|line| {
            if line.trim_start().starts_with(":source_hash:") {
                new_line
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    std::fs::write(target, updated)?;
    println!("Updated source hash in {}", target.display());
    Ok(())
}

pub fn apply_missing_contract(
    proposal: &aden_propose::Proposal,
) -> Result<(), Box<dyn std::error::Error>> {
    let target = &proposal.target_path;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(target, &proposal.patch_asciidoc)?;
    println!("Created contract at {}", target.display());
    Ok(())
}

#[cfg(feature = "watch")]
pub fn cmd_heal_watch(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use aden_heal::{Scanner, generate};
    use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc::channel;

    println!("Aden Self-Healing Watch Mode");
    println!("Watching: {} for changes...", path.display());
    println!("Triggers targeted drift scan on each change.");
    println!();

    let (tx, rx) = channel();
    let mut watcher = RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = tx.send(event);
            }
        },
        Config::default(),
    )?;

    watcher.watch(path, RecursiveMode::Recursive)?;

    for event in rx {
        for p in &event.paths {
            if let Some(ext) = p.extension().and_then(|e| e.to_str())
                && matches!(ext, "rs" | "ps1" | "adoc" | "aden")
            {
                println!("\n[INFO] Change detected: {}", p.display());
                println!("[INFO] Running targeted drift scan...");

                let scanner = Scanner::new(path);
                if let Ok(events) = scanner.scan() {
                    let report = generate(events.clone(), path);
                    println!("Health Score: {:.2}", report.overall_score);
                    for event in events.iter().take(5) {
                        println!("  - {:?} ({:?})", event, event.severity());
                    }
                    if events.len() > 5 {
                        println!("  ... and {} more events", events.len() - 5);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Clear stale proposal patches before a fresh `--propose` run and ensure the
/// directory exists. Without this, every run appended a new timestamped set and
/// left the old ones, so the directory grew without bound (2175 files observed).
/// Each propose run now produces exactly the current drift set.
fn clear_stale_proposals(proposals_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(proposals_dir)?;
    for entry in std::fs::read_dir(proposals_dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("adoc")
            && p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".patch.adoc"))
        {
            std::fs::remove_file(&p)?;
        }
    }
    Ok(())
}

pub fn generate_proposal(
    event: &aden_heal::DriftEvent,
    repo_path: &Path,
) -> Result<aden_propose::Proposal, Box<dyn std::error::Error>> {
    use aden_propose::{Proposal, ProposalStatus};
    use std::fmt::Write;

    let id = generate_proposal_id();
    let mut rationale = String::new();
    let mut patch = String::new();
    let mut target = repo_path.to_path_buf();
    let mut confidence = 0.5;

    match event {
        aden_heal::DriftEvent::StaleHash {
            target_path,
            expected_hash,
            actual_hash,
        } => {
            confidence = 0.99;
            writeln!(rationale, "Source hash mismatch detected.").unwrap();
            writeln!(rationale, "Expected: {}", expected_hash).unwrap();
            writeln!(rationale, "Actual:   {}", actual_hash).unwrap();
            writeln!(
                rationale,
                "The contract at {} needs regeneration.",
                target_path
            )
            .unwrap();

            target = PathBuf::from(target_path);
            writeln!(patch, ":source_hash: {}", actual_hash).unwrap();

            Ok(Proposal {
                id,
                target_path: target,
                drift_type: "StaleHash".to_string(),
                confidence,
                status: ProposalStatus::PendingReview,
                rationale,
                patch_asciidoc: patch,
            })
        }
        aden_heal::DriftEvent::MissingContract {
            source_path,
            anchor,
            symbol_name,
        } => {
            confidence = 0.85;
            writeln!(
                rationale,
                "No contract found for public symbol '{}'.",
                symbol_name
            )
            .unwrap();
            writeln!(rationale, "Source: {}", source_path).unwrap();
            writeln!(rationale, "Suggested anchor: {}", anchor).unwrap();
            writeln!(rationale).unwrap();
            writeln!(
                rationale,
                "Aden is store-first: contracts live in .aden/store, not as on-disk\n\
                 .adoc stubs. A hand-written stub has no recorded source_hash, so it\n\
                 immediately re-flags as StaleHash on the next scan and self-drifts.\n\
                 The correct remedy is to generate the contract into the store:\n\
                 \n  aden gen {} --auto .\n",
                source_path
            )
            .unwrap();

            // Do NOT target an on-disk .adoc stub. Point the proposal at the
            // source file (advisory only) and emit the regeneration command as
            // the patch body. The drift_type is deliberately NOT "MissingContract"
            // so cmd_heal_apply does not materialize a self-drifting stub via
            // apply_missing_contract; it falls through to the manual-review path.
            target = PathBuf::from(source_path);
            writeln!(
                patch,
                "// Run to populate the store-resident contract for `{}`:",
                symbol_name
            )
            .unwrap();
            writeln!(patch, "// aden gen {} --auto .", source_path).unwrap();

            Ok(Proposal {
                id,
                target_path: target,
                drift_type: "MissingContractAdvisory".to_string(),
                confidence,
                status: ProposalStatus::PendingReview,
                rationale,
                patch_asciidoc: patch,
            })
        }
        aden_heal::DriftEvent::BrokenReference {
            contract_path,
            ref_anchor,
            line,
        } => {
            confidence = 0.70;
            writeln!(rationale, "Broken reference detected.").unwrap();
            writeln!(rationale, "Contract: {}", contract_path).unwrap();
            writeln!(rationale, "Missing anchor: {}", ref_anchor).unwrap();

            target = PathBuf::from(contract_path);
            writeln!(
                patch,
                "// TODO: Fix broken reference to <<{}>> on line {}",
                ref_anchor, line
            )
            .unwrap();

            Ok(Proposal {
                id,
                target_path: target,
                drift_type: "BrokenReference".to_string(),
                confidence,
                status: ProposalStatus::PendingReview,
                rationale,
                patch_asciidoc: patch,
            })
        }
        aden_heal::DriftEvent::StaleMarkdown {
            md_path,
            source_files_changed,
        } => {
            confidence = 0.80;
            writeln!(rationale, "Stale markdown file detected.").unwrap();
            writeln!(rationale, "Markdown: {}", md_path).unwrap();
            writeln!(
                rationale,
                "Source files changed since markdown was updated:"
            )
            .unwrap();
            for f in source_files_changed {
                writeln!(rationale, "  - {}", f).unwrap();
            }
            writeln!(rationale, "Run 'aden gen . --format md' to regenerate.",).unwrap();

            target = PathBuf::from(md_path);
            writeln!(patch, "# Regenerate with: aden gen . --format md").unwrap();

            Ok(Proposal {
                id,
                target_path: target,
                drift_type: "StaleMarkdown".to_string(),
                confidence,
                status: ProposalStatus::PendingReview,
                rationale,
                patch_asciidoc: patch,
            })
        }
        aden_heal::DriftEvent::OrphanAnchor {
            anchor,
            contract_path,
        } => {
            confidence = 0.5;
            writeln!(
                rationale,
                "Orphan anchor: store anchor with no corresponding live source symbol."
            )
            .unwrap();
            writeln!(rationale, "Anchor: {}", anchor).unwrap();
            writeln!(rationale, "Contract: {}", contract_path).unwrap();

            // Option A: aden stays offline and deterministic. Rather than emit a
            // body it cannot author (orphan-anchor remediation is prose, not a
            // template), it emits a structured DRAFT REQUEST that the calling LLM
            // agent fills in, then submits via `heal --apply`.
            write_draft_request(
                &mut patch,
                "OrphanAnchor",
                &[("anchor", anchor), ("contract_path", contract_path)],
                "This store anchor has no live source symbol. Either (1) author the \
                 documentation prose that should reference it and cite it with \
                 <<anchor>>, or (2) reply DELETE if the anchor is dead (then run \
                 `aden heal . --gc`).",
            );

            Ok(Proposal {
                id,
                target_path: target,
                drift_type: "OrphanAnchorDraft".to_string(),
                confidence,
                status: ProposalStatus::PendingReview,
                rationale,
                patch_asciidoc: patch,
            })
        }
        other => {
            writeln!(rationale, "Drift event detected: {:?}", other).unwrap();
            write_draft_request(
                &mut patch,
                drift_kind_name(other),
                &[("debug", &format!("{:?}", other))],
                "Aden has no deterministic template for this drift type. An LLM agent \
                 should draft the appropriate fix below, then submit via `heal --apply`.",
            );

            Ok(Proposal {
                id,
                target_path: target,
                drift_type: "Unknown".to_string(),
                confidence,
                status: ProposalStatus::PendingReview,
                rationale,
                patch_asciidoc: patch,
            })
        }
    }
}

/// Emit a structured, LLM-fillable DRAFT REQUEST into a proposal patch body.
///
/// Option A of the LLM-drafted-proposals design: aden itself stays offline and
/// deterministic; instead of writing a body it cannot author, it emits the
/// context an in-loop LLM agent needs to draft the fix. The agent replaces the
/// block below the `----` delimiter with the drafted `.adoc` content (or the
/// literal `DELETE`) and submits it via `heal --apply`.
fn write_draft_request(
    patch: &mut String,
    drift_type: &str,
    fields: &[(&str, &str)],
    instruction: &str,
) {
    use std::fmt::Write;
    writeln!(patch, "// === DRAFT REQUEST v1 (LLM-fillable) ===").unwrap();
    writeln!(patch, "// drift_type: {}", drift_type).unwrap();
    for (k, v) in fields {
        writeln!(patch, "// {}: {}", k, v).unwrap();
    }
    writeln!(patch, "//").unwrap();
    writeln!(patch, "// INSTRUCTION: {}", instruction).unwrap();
    writeln!(
        patch,
        "// Replace everything below the delimiter with the drafted content (or DELETE)."
    )
    .unwrap();
    writeln!(patch, "----").unwrap();
}

/// Garbage-collect stale nodes from the store (store-first).
///
/// A code-symbol node is stale when the source file it came from no longer
/// exists on disk (or is no longer a discovered source). This is the
/// authoritative sweep that complements gen's incremental auto-prune: gen
/// prunes as it re-indexes changed/removed files; `heal --gc` re-scans the
/// whole store and removes anything orphaned by deletions that gen never saw
/// (e.g. files removed while gen wasn't run, or a store left stale by an old
/// additive-only build).
///
/// Synthesized hub nodes (`mod-*`) and any node without a `source_file`
/// attribute (doc headings created without one, metadata) are never removed —
/// they are not tied to a single source file.
pub fn cmd_heal_gc(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use crate::util::discover_source_files;
    use aden_store::{GraphStorage, Storage};

    println!("Aden Garbage Collector");
    println!("======================\n");

    let root = find_project_root(path);

    let (store_path, _) = aden_paths::resolve_read_store(&root);
    if !store_path.is_dir() {
        println!("No store found at {}. Nothing to GC.", store_path.display());
        return Ok(());
    }
    let storage = Storage::open_existing(store_path.to_str().ok_or("invalid store path")?)
        .map_err(|e| format!("Failed to open store: {}", e))?;

    // Live source files, relative to root — the same form sanitize_source_file
    // writes into each doc's `source_file` attribute.
    let live: std::collections::HashSet<String> = discover_source_files(&root)?
        .iter()
        .map(|s| {
            s.strip_prefix(&root)
                .unwrap_or(s)
                .to_string_lossy()
                .to_string()
        })
        .collect();

    let docs = storage
        .get_all_documents()
        .map_err(|e| format!("Failed to read store: {}", e))?;

    let mut stale: Vec<String> = Vec::new();
    let mut kept = 0usize;
    for (anchor, doc) in &docs {
        // Never GC synthesized hubs or nodes not tied to a source file.
        if anchor.starts_with("mod-") {
            kept += 1;
            continue;
        }
        match doc.attributes.get("source_file") {
            Some(src) => {
                // A node is stale when its source is no longer a LIVE source
                // file — i.e. it is not in the set `discover_source_files`
                // returns. That set already honours `.adenignore`/`.adenallow`
                // and the built-in ignores, so this single condition covers all
                // three reconciled cases under the store-first model:
                //   1. source deleted from disk            → not live → pruned
                //   2. source newly excluded by .adenignore → not live → pruned
                //   3. source still indexed                 → live    → kept
                // gen stops indexing an ignored file, so leaving its stale node
                // in the store would be drift; GC removes it.
                if !live.contains(src) {
                    stale.push(anchor.clone());
                } else {
                    kept += 1;
                }
            }
            None => kept += 1, // no source_file → not a prunable code symbol
        }
    }

    let mut removed = 0usize;
    for anchor in &stale {
        match storage.delete_node(anchor) {
            Ok(()) => {
                println!("  Removing orphaned: {}", anchor);
                removed += 1;
            }
            Err(e) => eprintln!("  WARN: failed to remove {}: {}", anchor, e),
        }
    }
    storage
        .flush()
        .map_err(|e| format!("Store flush failed: {}", e))?;

    // Prune orphaned contract adoc files from contracts/ whose anchor no longer
    // exists in the knowledge graph. These accumulate when symbols are renamed or
    // refactored — gen creates new contracts but never deletes old ones.
    let contracts_dir = root.join("contracts");
    let mut contracts_removed = 0usize;
    let mut contracts_kept = 0usize;
    if contracts_dir.is_dir() {
        // Build the live anchor set from the (now-cleaned) store.
        let live_anchors: std::collections::HashSet<String> = storage
            .get_all_documents()
            .unwrap_or_default()
            .into_keys()
            .collect();

        for entry in walkdir::WalkDir::new(&contracts_dir)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "adoc" {
                continue;
            }
            // Extract the anchor from the contract file (first [[anchor]] line).
            let content = match std::fs::read_to_string(p) {
                Ok(c) => c,
                Err(_) => {
                    contracts_kept += 1;
                    continue;
                }
            };
            let anchor = content.lines().find_map(|line| {
                let trimmed = line.trim();
                if trimmed.starts_with("[[") && trimmed.ends_with("]]") {
                    Some(trimmed[2..trimmed.len() - 2].to_string())
                } else {
                    None
                }
            });
            match anchor {
                Some(a) if !live_anchors.contains(&a) => match std::fs::remove_file(p) {
                    Ok(()) => {
                        contracts_removed += 1;
                    }
                    Err(e) => eprintln!("  WARN: failed to remove contract {}: {}", p.display(), e),
                },
                _ => contracts_kept += 1,
            }
        }
    }

    // Surface (never delete) intent overlays whose anchor no longer exists.
    // Overlays are human/agent-authored and git-tracked, so silently removing
    // one would destroy durable intent — exactly what the merge gate protects.
    // GC only reports them; the human decides whether to delete or re-point.
    let overlays_dir = root.join(".aden").join("overlays");
    let mut orphan_overlays: Vec<String> = Vec::new();
    if overlays_dir.is_dir() {
        let live_anchors: std::collections::HashSet<String> = storage
            .get_all_documents()
            .unwrap_or_default()
            .into_keys()
            .collect();
        for entry in std::fs::read_dir(&overlays_dir)
            .into_iter()
            .flatten()
            .flatten()
        {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("adoc") {
                continue;
            }
            // The :anchor: header is authoritative for which symbol this annotates.
            if let Ok(content) = std::fs::read_to_string(&p) {
                let declared = content.lines().find_map(|l| {
                    l.trim()
                        .strip_prefix(":anchor:")
                        .map(|v| v.trim().to_string())
                });
                if let Some(a) = declared
                    && !live_anchors.contains(&a)
                {
                    orphan_overlays.push(format!("{} (anchor: {})", p.display(), a));
                }
            }
        }
    }
    if !orphan_overlays.is_empty() {
        println!(
            "\nWARNING: {} intent overlay(s) annotate a symbol that no longer exists:",
            orphan_overlays.len()
        );
        for o in &orphan_overlays {
            println!("  - {o}");
        }
        println!(
            "  These are NOT deleted (they hold your intent). Re-point or remove them manually."
        );
    }

    println!(
        "\nGC complete: {} store node(s) removed, {} kept; {} orphaned contract(s) pruned, {} kept",
        removed, kept, contracts_removed, contracts_kept
    );
    Ok(())
}
