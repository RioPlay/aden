use std::path::{Path, PathBuf};

use crate::util::{generate_proposal_id, is_safe_id};

pub fn cmd_heal_scan_since(path: &Path, propose: bool, since: &str) -> Result<(), Box<dyn std::error::Error>> {
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
    let relevant_events: Vec<aden_heal::DriftEvent> = all_events.into_iter()
        .filter(|e| {
            let target = match e {
                aden_heal::DriftEvent::StaleHash { target_path, .. } => target_path,
                aden_heal::DriftEvent::SignatureMismatch { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::MissingContract { source_path, .. } => source_path,
                aden_heal::DriftEvent::OrphanAnchor { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::BrokenReference { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::DeadLink { contract_path, .. } => contract_path,
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

pub fn cmd_heal_scan(path: &Path, propose: bool) -> Result<(), Box<dyn std::error::Error>> {
    use aden_heal::{Scanner, generate};

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

            let print_group = |name: &str, events: &Vec<&aden_heal::DriftEvent>| {
                if !events.is_empty() {
                    println!("\n=== {} ({} events) ===", name, events.len());
                    for (i, event) in events.iter().enumerate() {
                        println!("  {}. {:?}", i + 1, event);
                    }
                }
            };

            print_group("CRITICAL", &critical);
            print_group("HIGH", &high);
            print_group("MEDIUM", &medium);
            print_group("LOW", &low);

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
        println!("WARNING: Low-confidence proposal ({:.2}). Review carefully.", proposal.confidence);
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

pub fn apply_stale_hash(proposal: &aden_propose::Proposal) -> Result<(), Box<dyn std::error::Error>> {
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

pub fn apply_missing_contract(proposal: &aden_propose::Proposal) -> Result<(), Box<dyn std::error::Error>> {
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
    use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc::channel;
    use aden_heal::{Scanner, generate};

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
                && matches!(ext, "rs" | "ps1" | "adoc" | "aden") {
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
        aden_heal::DriftEvent::StaleHash { target_path, expected_hash, actual_hash } => {
            confidence = 0.99;
            writeln!(rationale, "Source hash mismatch detected.").unwrap();
            writeln!(rationale, "Expected: {}", expected_hash).unwrap();
            writeln!(rationale, "Actual:   {}", actual_hash).unwrap();
            writeln!(rationale, "The contract at {} needs regeneration.", target_path).unwrap();

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
        aden_heal::DriftEvent::MissingContract { source_path, anchor, symbol_name } => {
            confidence = 0.85;
            writeln!(rationale, "No contract found for public symbol '{}'.", symbol_name).unwrap();
            writeln!(rationale, "Source: {}", source_path).unwrap();
            writeln!(rationale, "Suggested anchor: {}", anchor).unwrap();

            target = PathBuf::from(source_path).with_extension("adoc");
            writeln!(patch, "[[{}]]", anchor).unwrap();
            writeln!(patch, "= {}", symbol_name).unwrap();
            writeln!(patch).unwrap();
            writeln!(patch, "agent-note::STUB[Auto-generated by aden-heal. Review before removing this note.]").unwrap();

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
        aden_heal::DriftEvent::BrokenReference { contract_path, ref_anchor, line } => {
            confidence = 0.70;
            writeln!(rationale, "Broken reference detected.").unwrap();
            writeln!(rationale, "Contract: {}", contract_path).unwrap();
            writeln!(rationale, "Missing anchor: {}", ref_anchor).unwrap();

            target = PathBuf::from(contract_path);
            writeln!(patch, "// TODO: Fix broken reference to <<{}>> on line {}", ref_anchor, line).unwrap();

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