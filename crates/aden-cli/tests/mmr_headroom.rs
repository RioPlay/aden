// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

// MMR headroom probe (measurement harness, #[ignore]d, real repos).
//
// How much redundancy does MMR actually prune on real code? For rich hub seeds
// across the eval corpora, assemble at the default budget with and without MMR
// (`assemble_with_anchors_mmr` None vs Some(tau)) and report, per corpus and tau:
//   dropped = anchors plain assembly included that MMR skipped as redundant
//   added   = distinct anchors MMR pulled in with the freed budget
//   changed = % of hubs where MMR altered the bundle at all
// High added/changed ⇒ real neighborhoods carry prunable redundancy, so wiring
// MMR into `ask` frees budget for diverse context; near-zero ⇒ low headroom.
//
// Corpora: every gen'd repo under ~/Projects/eval-repos (override ADEN_EVAL_REPOS).
// Run: cargo test -p aden-cli --test mmr_headroom -- --include-ignored --nocapture

use aden_asm::traverse::{AssemblyOptions, assemble_with_anchors_mmr};
use std::collections::HashSet;
use std::path::PathBuf;

const DEPTH: usize = 2;
const BUDGET: usize = 4096; // aden's default assembly budget
const REACH: usize = 16_384; // "full neighborhood" size, for hub-richness ranking
const TAUS: &[f32] = &[0.6, 0.7, 0.8];
const CANDIDATE_CAP: usize = 60;
const SEEDS: usize = 12;
const MIN_NBHD: usize = 5;
const MAX_CORPUS_NODES: usize = 50_000;

fn eval_root() -> PathBuf {
    std::env::var("ADEN_EVAL_REPOS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Projects/eval-repos")
        })
}

fn corpora() -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(eval_root()) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

fn opts(seed: &str, budget: usize) -> AssemblyOptions {
    AssemblyOptions {
        start_anchor: seed.to_string(),
        max_depth: DEPTH,
        token_budget: budget,
        ..Default::default()
    }
}

#[test]
#[ignore = "measurement harness, not a CI gate; reads external repos"]
fn mmr_headroom_report() {
    let corpora = corpora();
    if corpora.is_empty() {
        eprintln!(
            "SKIP: no gen'd eval repos under {} (set ADEN_EVAL_REPOS)",
            eval_root().display()
        );
        return;
    }

    println!("\n=== MMR headroom: redundancy pruned at the {BUDGET}-token default ===");
    println!(
        "depth={DEPTH} seeds/corpus={SEEDS} taus={TAUS:?}  (dropped/added/changed% per tau)\n"
    );
    println!(
        "{:<14} {:>6} {:>6}   per-tau dropped / added / changed",
        "corpus", "nodes", "|plain|"
    );

    for repo in corpora {
        let name = repo
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let graph = match aden_graph::cache::build_from_directory_cached(&repo) {
            Ok(g) => g,
            Err(e) => {
                println!("{name:<14} SKIP (load: {e})");
                continue;
            }
        };
        if graph.node_count() == 0 || graph.edge_count() == 0 {
            println!("{name:<14} SKIP (not gen'd — empty/edge-less)");
            continue;
        }
        if graph.node_count() > MAX_CORPUS_NODES {
            println!("{name:<14} {:>6} SKIP (too large)", graph.node_count());
            continue;
        }

        let inc = |seed: &str, budget: usize, mmr: Option<f32>| -> HashSet<String> {
            assemble_with_anchors_mmr(&graph, &opts(seed, budget), mmr)
                .map(|(_, a)| a.into_iter().collect())
                .unwrap_or_default()
        };

        // Stride-sample rich hubs: score by reach-neighborhood size, keep the
        // SEEDS largest (trivial leaves carry no redundancy to measure).
        let all: Vec<String> = graph
            .all_nodes()
            .into_iter()
            .map(|(a, _)| a.to_string())
            .collect();
        let step = (all.len() / CANDIDATE_CAP).max(1);
        let mut scored: Vec<(String, usize)> = all
            .iter()
            .step_by(step)
            .take(CANDIDATE_CAP)
            .map(|a| (a.clone(), inc(a, REACH, None).len()))
            .filter(|(_, n)| *n >= MIN_NBHD)
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        scored.truncate(SEEDS);
        if scored.is_empty() {
            println!("{name:<14} SKIP (no rich hubs in sample)");
            continue;
        }

        let mut plain_total = 0usize;
        let mut dropped = vec![0usize; TAUS.len()];
        let mut added = vec![0usize; TAUS.len()];
        let mut changed = vec![0usize; TAUS.len()];
        for (seed, _) in &scored {
            let plain = inc(seed, BUDGET, None);
            plain_total += plain.len();
            for (ti, &tau) in TAUS.iter().enumerate() {
                let pruned = inc(seed, BUDGET, Some(tau));
                let d = plain.difference(&pruned).count();
                let a = pruned.difference(&plain).count();
                dropped[ti] += d;
                added[ti] += a;
                if d > 0 || a > 0 {
                    changed[ti] += 1;
                }
            }
        }

        let k = scored.len() as f64;
        let nodes = graph.node_count();
        let mean_plain = plain_total as f64 / k;
        let mut row = String::new();
        for (ti, &tau) in TAUS.iter().enumerate() {
            row.push_str(&format!(
                "  τ{tau}:{:.1}/{:.1}/{:.0}%",
                dropped[ti] as f64 / k,
                added[ti] as f64 / k,
                100.0 * changed[ti] as f64 / k
            ));
        }
        println!("{name:<14} {nodes:>6} {mean_plain:>6.1}{row}");
    }

    println!("\ndropped = redundant anchors MMR skipped; added = distinct anchors the freed");
    println!("budget pulled in; changed% = hubs where MMR altered the bundle. High added");
    println!("means real neighborhoods carry prunable redundancy → wiring MMR frees budget.");
}
