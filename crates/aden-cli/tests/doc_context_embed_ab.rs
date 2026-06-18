// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// DOC-SIDE lever (the one class untried) — beat dense at the SOURCE, not by post-processing.
// Every query-side trick (rerank, gate, PRF) ties-or-hurts dense, so instead of reordering
// dense's output we change what dense EMBEDS: enrich each symbol card with the gist of its graph
// neighbours (callers + callees, from the real aden graph) before embedding. A function is then
// represented by what it does AND its role in the system — naturally generated from the graph,
// no dictionary, no hand-tuning. Hypothesis: the gold for "group into clusters" embeds closer to
// the query once it carries its louvain/modularity neighbours.
//
// Arms: DENSE (plain card) vs DENSE-CTX (graph-enriched card) vs ORACLE (plain + hand expansion).
// (measurement harness, #[ignore]d, needs --features dense; clean corpus; writes nothing.)
//
// Run: cargo test -p aden-cli --features dense --test doc_context_embed_ab -- --include-ignored --nocapture

#![cfg_attr(not(feature = "dense"), allow(dead_code))]

mod common;

use std::path::PathBuf;

const NBR_PER_DIR: usize = 6; // neighbours per direction folded into a card
const HEAD: usize = 140; // chars of a neighbour's gist appended

fn corpus() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("ADEN_REAL_CORPUS") {
        let p = PathBuf::from(d);
        return p.is_dir().then_some(p);
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.is_dir().then_some(root)
}

fn index_text(doc: &aden_core::Document) -> String {
    aden_emit::emit_document(doc)
        .lines()
        .filter(|l| !l.trim_start().starts_with(":last-verified:"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(feature = "dense")]
fn load_embedder() -> Option<aden_index::TractEmbedder> {
    let dir = std::env::var("ADEN_BGE_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".cache/aden-models/bge-small-en-v1.5")
        });
    if !dir.join("model.onnx").exists() {
        return None;
    }
    aden_index::TractEmbedder::from_dir(&dir).ok()
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    for x in &mut v {
        *x /= norm;
    }
    v
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[derive(Default, Clone)]
struct Metrics {
    r1: usize,
    r5: usize,
    mrr: f64,
}
impl Metrics {
    fn add(&mut self, rank: Option<usize>) {
        if let Some(r) = rank {
            self.r1 += (r == 1) as usize;
            self.r5 += (r <= 5) as usize;
            self.mrr += 1.0 / r as f64;
        }
    }
    fn line(&self, label: &str, n: usize) -> String {
        format!(
            "    {label:<12} R@1 {:>2}/{n}  R@5 {:>2}/{n}  MRR {:.3}",
            self.r1,
            self.r5,
            self.mrr / n as f64
        )
    }
}

#[test]
#[ignore = "doc-side graph-context embedding (needs --features dense); clean corpus; writes nothing"]
fn doc_context_embed_report() {
    #[cfg(not(feature = "dense"))]
    {
        eprintln!("SKIP: rebuild with --features dense");
    }
    #[cfg(feature = "dense")]
    {
        use aden_index::EmbeddingProvider;
        use aden_store::{GraphStorage, Storage};
        use rayon::prelude::*;

        let Some(repo) = corpus() else {
            eprintln!("SKIP: corpus dir not found");
            return;
        };
        let root = aden_paths::resolve_root(&repo);
        let (store_path, _) = aden_paths::resolve_read_store(&root);
        let Some(storage) = store_path
            .to_str()
            .and_then(|s| Storage::open_existing(s).ok())
        else {
            eprintln!("SKIP: no project store");
            return;
        };
        let Ok(docs) = storage.get_all_documents() else {
            eprintln!("SKIP: cannot read documents");
            return;
        };
        // (anchor, plain card text), test cards excluded (leak filter).
        let cards: Vec<(String, String)> = docs
            .values()
            .filter(|d| {
                !d.anchor.contains("/tests/")
                    && !d
                        .attributes
                        .get("source_file")
                        .is_some_and(|s| s.contains("/tests/"))
            })
            .map(|d| (d.anchor.clone(), index_text(d)))
            .collect();
        if cards.is_empty() {
            eprintln!("SKIP: no store cards");
            return;
        }
        let Some(emb) = load_embedder() else {
            eprintln!("SKIP: bge model not found");
            return;
        };
        let n = cards.len();
        let anchors: Vec<&str> = cards.iter().map(|(a, _)| a.as_str()).collect();

        // One pass over outgoing edges builds BOTH directions of neighbour context.
        use std::collections::HashMap;
        let idx: HashMap<&str, usize> = anchors.iter().enumerate().map(|(i, &a)| (a, i)).collect();
        let mut out_nbrs: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut in_nbrs: Vec<Vec<usize>> = vec![Vec::new(); n];
        for i in 0..n {
            if let Ok(out) = storage.get_outgoing_edges(&cards[i].0) {
                for (tgt, _) in out {
                    if let Some(&j) = idx.get(tgt.as_str())
                        && j != i
                    {
                        out_nbrs[i].push(j);
                        in_nbrs[j].push(i);
                    }
                }
            }
        }
        let head = |t: &str| -> String {
            t.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(HEAD)
                .collect()
        };
        let enriched: Vec<String> = (0..n)
            .map(|i| {
                let mut ctx = String::new();
                for &j in out_nbrs[i].iter().take(NBR_PER_DIR) {
                    ctx.push(' ');
                    ctx.push_str(&head(&cards[j].1));
                }
                for &j in in_nbrs[i].iter().take(NBR_PER_DIR) {
                    ctx.push(' ');
                    ctx.push_str(&head(&cards[j].1));
                }
                format!("{}\n{ctx}", cards[i].1)
            })
            .collect();
        let avg_nbrs = (out_nbrs
            .iter()
            .chain(in_nbrs.iter())
            .map(|v| v.len().min(NBR_PER_DIR))
            .sum::<usize>()) as f64
            / n as f64;

        // Embed plain and graph-enriched cards (parallel).
        let plain: Vec<Vec<f32>> = cards
            .par_iter()
            .map(|(_, t)| normalize(emb.embed(t)))
            .collect();
        let ctx: Vec<Vec<f32>> = enriched
            .par_iter()
            .map(|t| normalize(emb.embed(t)))
            .collect();

        let probes = common::PROBES;
        let np = probes.len();

        // Dataset self-audit (leak / dead gold).
        let (mut leaks, mut dead) = (0usize, 0usize);
        for &(q, accept, _) in probes {
            let qset: std::collections::HashSet<String> =
                aden_index::tokenize(q).into_iter().collect();
            for a in accept {
                let spaced = a.replace("::", " ").replace('_', " ");
                if aden_index::tokenize(&spaced)
                    .into_iter()
                    .any(|t| t.len() >= 4 && qset.contains(&t))
                {
                    leaks += 1;
                }
                if !anchors.iter().any(|an| an.contains(a)) {
                    dead += 1;
                }
            }
        }
        println!("  dataset audit: {leaks} leak(s), {dead} dead gold(s) over {np} probes");

        let rank = |bank: &[Vec<f32>], qv: &[f32], accept: &[&str]| -> Option<usize> {
            let mut s: Vec<(usize, f32)> = bank
                .iter()
                .enumerate()
                .map(|(i, v)| (i, cosine(qv, v)))
                .collect();
            s.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            s.iter()
                .position(|(i, _)| accept.iter().any(|t| anchors[*i].contains(t)))
                .map(|p| p + 1)
        };

        let (mut dense_m, mut ctx_m, mut oracle_m) =
            (Metrics::default(), Metrics::default(), Metrics::default());
        for &(q, accept, oracle) in probes {
            let qv = normalize(emb.embed(q));
            dense_m.add(rank(&plain, &qv, accept));
            ctx_m.add(rank(&ctx, &qv, accept));
            let ov = normalize(emb.embed(&format!("{q} {oracle}")));
            oracle_m.add(rank(&plain, &ov, accept));
        }

        println!(
            "\n=== Doc-side graph-context embedding ({np} probes, {n} cards, avg {avg_nbrs:.1} nbrs/card) ==="
        );
        println!("{}", dense_m.line("DENSE", np));
        println!("{}", ctx_m.line("DENSE-CTX", np));
        println!("{}", oracle_m.line("ORACLE", np));

        assert!(!cards.is_empty(), "no cards");
    }
}
