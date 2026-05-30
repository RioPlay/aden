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
        MissingContract { symbol_name, source_path, .. } => {
            format!("MissingContract: {} ({})", symbol_name, source_path)
        }
        OrphanAnchor { anchor, .. } => format!("OrphanAnchor: {}", anchor),
        BrokenReference { contract_path, ref_anchor, line } => {
            format!("BrokenReference: <<{}>> in {}:{}", ref_anchor, contract_path, line)
        }
        DeadLink { contract_path, include_path } => {
            format!("DeadLink: {} -> {}", contract_path, include_path)
        }
        MarkdownDrift { md_path, .. } => format!("MarkdownDrift: {}", md_path),
        StaleMarkdown { md_path, source_files_changed } => {
            format!("StaleMarkdown: {} ({} source file(s) changed)", md_path, source_files_changed.len())
        }
        MissingMarkdownTemplate { md_path, template_source } => {
            format!("MissingMarkdownTemplate: {} (from {})", md_path, template_source)
        }
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
        std::fs::create_dir_all(&proposals_dir)?;
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
            const HEAL_GROUP_CAP: usize = 10;
            let print_group = |name: &str, events: &Vec<&aden_heal::DriftEvent>, _path: &Path| {
                if !events.is_empty() {
                    println!("\n=== {} ({} events) ===", name, events.len());
                    let shown = if unlimited { events.len() } else { events.len().min(HEAL_GROUP_CAP) };
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
                println!("\n--fix flag set. Attempting auto-fix...");
                let mut fixed_count = 0;
                let mut failed_count = 0;

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
                        println!("  Skipping {:?}: low confidence ({:.2})", event, confidence);
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
                            }
                        }
                        aden_heal::DriftEvent::MissingContract {
                            source_path,
                            anchor,
                            symbol_name,
                            ..
                        } => {
                            let contract_path = PathBuf::from(source_path).with_extension("adoc");
                            if let Some(parent) = contract_path.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            let content = format!(
                                "[[{}]]\n= {}\n\nagent-note::STUB[Auto-generated by aden-heal]\n",
                                anchor, symbol_name
                            );
                            std::fs::write(&contract_path, content)?;
                            println!(
                                "  Fixed: {} (created stub contract)",
                                contract_path.display()
                            );
                            fixed_count += 1;
                        }
                        aden_heal::DriftEvent::SignatureMismatch {
                            contract_path,
                            expected_sig: _,
                            actual_sig,
                            ..
                        } => {
                            let sig_str = actual_sig.join(",");
                            let content = std::fs::read_to_string(contract_path)?;
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
                            std::fs::write(contract_path, updated)?;
                            println!("  Fixed: {} (updated signature)", contract_path);
                            fixed_count += 1;
                        }
                        _ => {
                            println!("  Skipping {:?}: cannot auto-fix", event);
                            failed_count += 1;
                        }
                    }
                }

                println!("\nFixed: {} events", fixed_count);
                if failed_count > 0 {
                    println!("Skipped: {} events (require manual review)", failed_count);
                }

                return Ok(());
            }

            if propose {
                println!("\n--propose flag set. Generating patches...");
                let store_dir = path.join(".aden").join("proposals");
                std::fs::create_dir_all(&store_dir)?;

                for event in &report.events {
                    let proposal = generate_proposal(event, path)?;
                    let store_path = aden_propose::persist(&proposal, path)?;
                    println!("  Generated proposal: {}", store_path.display());
                }
                println!("\nReview proposals in: {}", store_dir.display());
                println!("Apply with: aden heal --apply <proposal-id>");
            } else {
                println!("\nRun with --propose to generate patch files for review.");
                println!("Or use --fix to auto-fix StaleHash and MissingContract events.");
            }

            Ok(())
        }
        Err(e) => {
            eprintln!("ERROR: Scan failed: {}", e);
            Err(e.into())
        }
    }
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

            target = PathBuf::from(source_path).with_extension("adoc");
            writeln!(patch, "[[{}]]", anchor).unwrap();
            writeln!(patch, "= {}", symbol_name).unwrap();
            writeln!(patch).unwrap();
            writeln!(
                patch,
                "agent-note::STUB[Auto-generated by aden-heal. Review before removing this note.]"
            )
            .unwrap();

            Ok(Proposal {
                id,
                target_path: target,
                drift_type: "MissingContract".to_string(),
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
        other => {
            writeln!(rationale, "Drift event detected: {:?}", other).unwrap();
            writeln!(patch, "// Proposed changes for drift event").unwrap();

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

    let store_path = root.join(".aden").join("store");
    if !store_path.is_dir() {
        println!("No store found at {}. Nothing to GC.", store_path.display());
        return Ok(());
    }
    let storage = Storage::new(store_path.to_str().ok_or("invalid store path")?)
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
                // Stale if the source file is gone from the project, or missing
                // on disk (covers absolute-path entries from an older gen run).
                let on_disk = root.join(src).exists();
                if !live.contains(src) && !on_disk {
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

    println!("\nGC complete: {} node(s) removed, {} kept", removed, kept);
    Ok(())
}
