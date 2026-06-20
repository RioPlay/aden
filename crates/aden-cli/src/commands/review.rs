// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

pub fn cmd_review(path: &Path, budget: usize) -> Result<(), Box<dyn std::error::Error>> {
    use aden_propose::list;

    println!("Aden Semantic Review Engine (Budget: {} tokens)", budget);
    println!("================================================");

    if !path.join(".aden").join("proposals").exists() {
        println!("No proposals directory found. Run \"aden heal --scan . --propose\" first.");
        return Ok(());
    }

    let proposals = list(path)?;
    let low_confidence: Vec<_> = proposals.iter().filter(|p| p.confidence < 0.85).collect();

    if low_confidence.is_empty() {
        println!("No low-confidence proposals found. All drift detected is auto-applyable.");
        return Ok(());
    }

    println!(
        "Reviewing {} low-confidence proposals...\n",
        low_confidence.len()
    );

    let estimated_tokens = low_confidence.len() * 100;
    println!(
        "Estimated review cost: ~{} tokens (budget: {})",
        estimated_tokens, budget
    );

    if estimated_tokens > budget {
        println!(
            "WARNING: Review exceeds budget. Showing first {} proposals.",
            budget / 100
        );
    }

    let show_count = (budget / 100).min(low_confidence.len());
    for (i, proposal) in low_confidence.iter().take(show_count).enumerate() {
        println!(
            "\n{}. Proposal {} (confidence: {:.2})",
            i + 1,
            proposal.id,
            proposal.confidence
        );
        println!("   Target: {}", proposal.target_path.display());
        println!("   Drift Type: {}", proposal.drift_type);
        println!(
            "   Rationale: {}",
            proposal.rationale.lines().next().unwrap_or("(none)")
        );
    }

    if show_count < low_confidence.len() {
        println!(
            "\n... and {} more proposals (increase --budget to see all)",
            low_confidence.len() - show_count
        );
    }

    println!("\nReview each proposal file in .aden/proposals/ before applying.");
    Ok(())
}

pub fn cmd_review_since(
    path: &Path,
    budget: usize,
    since: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_heal::Scanner;

    println!(
        "Reviewing changes since '{}' with budget {} tokens",
        since, budget
    );

    let output = std::process::Command::new("git")
        .args(["diff", "--name-only", "--", since])
        .current_dir(path)
        .output()?;

    let changed = String::from_utf8_lossy(&output.stdout);
    let files: Vec<&str> = changed.lines().filter(|l| !l.is_empty()).collect();

    if files.is_empty() {
        println!("No files changed since {}.", since);
        return Ok(());
    }

    println!("Files changed since '{}': {} files", since, files.len());
    for f in &files {
        println!("  - {}", f);
    }

    println!("\nRunning targeted drift scan...");
    let scanner = Scanner::new(path);
    let all_events = scanner.scan()?;

    let relevant_events: Vec<_> = all_events
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

    if relevant_events.is_empty() {
        println!("No drift detected in changed files.");
        return Ok(());
    }

    println!(
        "Found {} drift events in changed files.",
        relevant_events.len()
    );

    let show_count = (budget / 100).min(relevant_events.len());
    for (i, event) in relevant_events.iter().take(show_count).enumerate() {
        println!("  {}. {:?}", i + 1, event);
    }
    if show_count < relevant_events.len() {
        println!(
            "  ... and {} more (increase --budget)",
            relevant_events.len() - show_count
        );
    }

    Ok(())
}
