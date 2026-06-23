// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

// Budget-sweep eval (measurement harness, #[ignore]d, real repos).
//
// Question: what token budget does structural assembly (`asm`/`ask`) actually
// need to capture a symbol's full connected neighborhood — and how does that
// "ideal budget" vary across corpora of different size and richness?
//
// Method: for each corpus, pick the richest hub seeds (largest neighborhoods).
// The set of anchors the assembler includes at a very large "reach" budget is
// the gold (complete) neighborhood. Recall at each smaller budget then traces
// the accuracy-per-token curve; the knee — the smallest budget reaching
// KNEE_PCT% of the gold — is the practical ideal budget for that corpus.
//
// Gold is STRUCTURAL (what assembly includes given an unlimited budget), not
// answer-correctness: a scalable proxy that needs no hand labels, so it runs on
// every gen'd corpus. Pair with assembly_ab (query-focused, hand-authored gold)
// for the correctness axis.
//
// Corpora: every gen'd repo under ~/Projects/eval-repos (override the root with
// ADEN_EVAL_REPOS). Each must have had `aden gen <repo>` run.
//
// Run: cargo test -p aden-cli --test budget_sweep -- --include-ignored --nocapture

use aden_asm::traverse::{AssemblyOptions, assemble_with_anchors};
use std::collections::HashSet;
use std::path::PathBuf;

const DEPTH: usize = 2;
const REACH_BUDGET: usize = 16_384; // "unlimited" — defines the gold neighborhood
const BUDGETS: &[usize] = &[64, 128, 256, 512, 1024, 2048, 4096, 8192];
const CANDIDATE_CAP: usize = 80; // nodes sampled per corpus when picking hubs
const SEEDS: usize = 10; // richest hubs benchmarked per corpus
const MIN_GOLD: usize = 3; // a hub needs a non-trivial neighborhood to be informative
const KNEE_PCT: usize = 90; // knee = smallest budget reaching this % of the gold
const DEFAULT_BUDGET: usize = 4096; // AssemblyOptions default, for the readout
const MAX_CORPUS_NODES: usize = 50_000; // above this, CANDIDATE_CAP probes can't sample hubs representatively

fn eval_root() -> PathBuf {
    std::env::var("ADEN_EVAL_REPOS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(
                dirs::home_dir()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            )
            .join("Projects/eval-repos")
        })
}

fn corpora() -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(eval_root()) else {
        return Vec::new();
    };
    // Every subdirectory is a candidate; whether it was actually `aden gen`'d is
    // gated below by the loaded graph. `gen` is store-first — it drops no marker
    // in the repo, so a path check would miss store-only corpora.
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
fn budget_sweep_report() {
    let corpora = corpora();
    if corpora.is_empty() {
        eprintln!(
            "SKIP: no gen'd eval repos under {} (set ADEN_EVAL_REPOS)",
            eval_root().display()
        );
        return;
    }

    println!("\n=== Budget sweep: ideal token budget by corpus ===");
    println!(
        "depth={DEPTH} reach={REACH_BUDGET} seeds/corpus={SEEDS} knee={KNEE_PCT}% of gold\nbudgets: {BUDGETS:?}\n"
    );
    println!(
        "{:<14} {:>6} {:>7} {:>6}   recall@budget",
        "corpus", "nodes", "|gold|", "knee"
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

        // An un-gen'd path loads as a doc-only / empty graph; assembly traverses
        // edges, so skip anything without the gen'd (edge-rich) store graph.
        if graph.node_count() == 0 || graph.edge_count() == 0 {
            println!("{name:<14} SKIP (not gen'd — empty/edge-less graph)");
            continue;
        }
        if graph.node_count() > MAX_CORPUS_NODES {
            println!(
                "{name:<14} {:>6} SKIP (too large for representative sampling)",
                graph.node_count()
            );
            continue;
        }

        // Anchors the assembler includes for `seed` at `budget`.
        let inc = |seed: &str, budget: usize| -> HashSet<String> {
            assemble_with_anchors(&graph, &opts(seed, budget))
                .map(|(_, anchors)| anchors.into_iter().collect())
                .unwrap_or_default()
        };

        // Pick the richest hubs from an even stride across ALL anchors (first-N
        // badly under-samples large graphs — their hubs aren't at the front).
        // Score each candidate by its reach-budget neighborhood size; keep the
        // SEEDS largest. Trivial leaves make the budget question meaningless.
        let all: Vec<String> = graph
            .all_nodes()
            .into_iter()
            .map(|(a, _)| a.to_string())
            .collect();
        let step = (all.len() / CANDIDATE_CAP).max(1);
        let mut scored: Vec<(String, HashSet<String>)> = all
            .iter()
            .step_by(step)
            .take(CANDIDATE_CAP)
            .map(|anchor| {
                let gold = inc(anchor, REACH_BUDGET);
                (anchor.clone(), gold)
            })
            .filter(|(_, gold)| gold.len() >= MIN_GOLD)
            .collect();
        scored.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        scored.truncate(SEEDS);

        if scored.is_empty() {
            println!("{name:<14} SKIP (no hub with >= {MIN_GOLD} neighbors in sample)");
            continue;
        }

        // Mean recall across the corpus's hubs at each budget (gold = reach set).
        let mut recall = vec![0f64; BUDGETS.len()];
        for (seed, gold) in &scored {
            for (bi, &b) in BUDGETS.iter().enumerate() {
                let hit = inc(seed, b).intersection(gold).count();
                recall[bi] += hit as f64 / gold.len() as f64;
            }
        }
        for r in &mut recall {
            *r /= scored.len() as f64;
        }

        let target = KNEE_PCT as f64 / 100.0;
        let knee = BUDGETS
            .iter()
            .zip(&recall)
            .find(|&(_, &r)| r >= target)
            .map(|(&b, _)| b.to_string())
            .unwrap_or_else(|| format!(">{}", BUDGETS.last().unwrap_or(&0)));

        let nodes = graph.node_count();
        let avg_gold =
            scored.iter().map(|(_, g)| g.len()).sum::<usize>() as f64 / scored.len() as f64;
        let curve = recall
            .iter()
            .map(|r| format!("{r:.2}"))
            .collect::<Vec<_>>()
            .join(" ");
        let at_default = BUDGETS
            .iter()
            .position(|&b| b == DEFAULT_BUDGET)
            .map(|i| format!("{:.2}", recall[i]))
            .unwrap_or_else(|| "-".into());
        println!(
            "{name:<14} {nodes:>6} {avg_gold:>7.1} {knee:>6}   [{curve}]  default@{DEFAULT_BUDGET}={at_default}"
        );
    }

    println!(
        "\nknee = smallest budget capturing {KNEE_PCT}% of a hub's full (reach-budget) neighborhood;"
    );
    println!(
        "below it, asm/ask truncates real context. Compare knee to the {DEFAULT_BUDGET} default"
    );
    println!("per corpus to see where the default is over- or under-provisioned.");
    println!("Caveat: structural gold (reach-budget assembly), not answer-correctness — pair with");
    println!("assembly_ab for the query/answer axis.");
}
