// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Whitening A/B (step 1 of the iterate-the-loop plan) — does reshaping the embedding
// geometry lift the dense BASE ranking? (measurement harness, #[ignore]d, needs
// --features dense, writes nothing.)
//
// Raw transformer embeddings are anisotropic (a "narrow cone"): a dominant mean direction
// makes everything look similar, flattening cosine. Whitening removes that. This tests the
// two cheapest, dep-free forms over the corpus card vectors, measured at rank-of-gold:
//   * RAW       — embeddings as-is (should reproduce the dense baseline; a sanity check).
//   * CENTERED  — subtract the corpus mean vector (kills the dominant direction).
//   * DIAG      — centered, then divided by per-dimension std (diagonal whitening).
// If centering/diag lift the base, escalate to full PCA whitening + the graph-regularized
// variant (pull SynonymOf together, push AntonymOf apart) — and then the rerank layers sit
// on a higher base, which is the compounding loop.
//
// Run: cargo test -p aden-cli --features dense --test whitening_ab -- --include-ignored --nocapture

#![cfg_attr(not(feature = "dense"), allow(dead_code))]

use std::path::{Path, PathBuf};

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

/// (anchor, indexed text) for every store card — the same text the dense path embeds.
fn load_cards(repo: &Path) -> Vec<(String, String)> {
    use aden_store::{GraphStorage, Storage};
    let root = aden_paths::resolve_root(repo);
    let (store_path, _) = aden_paths::resolve_read_store(&root);
    let Some(s) = store_path.to_str() else {
        return Vec::new();
    };
    let Ok(storage) = Storage::open_existing(s) else {
        return Vec::new();
    };
    let Ok(docs) = storage.get_all_documents() else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = docs
        .values()
        .map(|d| (d.anchor.clone(), index_text(d)))
        .collect();
    out.sort();
    out
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

/// Per-dimension mean and std over a set of vectors (std floored to avoid div-by-zero).
fn mean_std(vecs: &[Vec<f32>]) -> (Vec<f32>, Vec<f32>) {
    let dim = vecs.first().map_or(0, |v| v.len());
    let n = vecs.len().max(1) as f32;
    let mut mean = vec![0.0f32; dim];
    for v in vecs {
        for (m, &x) in mean.iter_mut().zip(v) {
            *m += x;
        }
    }
    for m in &mut mean {
        *m /= n;
    }
    let mut var = vec![0.0f32; dim];
    for v in vecs {
        for ((va, &x), &m) in var.iter_mut().zip(v).zip(&mean) {
            let d = x - m;
            *va += d * d;
        }
    }
    let std = var.iter().map(|v| (v / n).sqrt().max(1e-6)).collect();
    (mean, std)
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    for x in &mut v {
        *x /= norm;
    }
    v
}

/// Apply a whitening transform then L2-normalize: `RAW` (mean=None), `CENTERED`
/// (mean=Some, std=None), or `DIAG` (mean=Some, std=Some).
fn transform(v: &[f32], mean: Option<&[f32]>, std: Option<&[f32]>) -> Vec<f32> {
    let w: Vec<f32> = match (mean, std) {
        (None, _) => v.to_vec(),
        (Some(m), None) => v.iter().zip(m).map(|(x, mm)| x - mm).collect(),
        (Some(m), Some(s)) => v
            .iter()
            .zip(m)
            .zip(s)
            .map(|((x, mm), sd)| (x - mm) / sd)
            .collect(),
    };
    normalize(w)
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

struct Probe {
    query: &'static str,
    accept: &'static [&'static str],
}

fn probes() -> Vec<Probe> {
    vec![
        Probe {
            query: "store a batch of relationships between nodes in one operation",
            accept: &["put_edges_bulk"],
        },
        Probe {
            query: "group the graph into clusters of tightly connected nodes",
            accept: &["detect_communities"],
        },
        Probe {
            query: "blend two ranked result lists into a single ordering",
            accept: &["rrf_fuse"],
        },
        Probe {
            query: "how aligned are two embedding vectors",
            accept: &["cosine_similarity"],
        },
        Probe {
            query: "fewest single character edits to turn one word into another",
            accept: &["levenshtein_distance"],
        },
        Probe {
            query: "figure out which definition a function call points to",
            accept: &["resolve_callee"],
        },
        Probe {
            query: "decide what category of question the user is asking",
            accept: &["classify_intent"],
        },
        Probe {
            query: "detect a leaked password or api key inside text",
            accept: &["content_has_high_confidence_secret"],
        },
        Probe {
            query: "collect the nodes surrounding a starting symbol up to some depth",
            accept: &["build_neighborhood"],
        },
        Probe {
            query: "find everything that points at a given node",
            accept: &["get_incoming_edges"],
        },
        Probe {
            query: "how many tokens were avoided versus reading whole files",
            accept: &["SavingsEstimate"],
        },
        Probe {
            query: "anchors in the graph that nothing else references",
            accept: &["scan_orphans"],
        },
    ]
}

#[derive(Default)]
struct Metrics {
    r1: usize,
    r5: usize,
    r10: usize,
    mrr: f64,
}
impl Metrics {
    fn add(&mut self, rank: Option<usize>) {
        if let Some(r) = rank {
            self.r1 += (r == 1) as usize;
            self.r5 += (r <= 5) as usize;
            self.r10 += (r <= 10) as usize;
            self.mrr += 1.0 / r as f64;
        }
    }
    fn line(&self, label: &str, n: usize) -> String {
        format!(
            "    {label:<10} R@1 {}/{n}   R@5 {}/{n}   R@10 {}/{n}   MRR {:.3}",
            self.r1,
            self.r5,
            self.r10,
            self.mrr / n as f64
        )
    }
}

/// Rank of the first card (by transformed cosine) whose anchor carries an accepted symbol.
fn rank(cards: &[(String, Vec<f32>)], q: &[f32], accept: &[&str]) -> Option<usize> {
    let mut scored: Vec<(&str, f32)> = cards.iter().map(|(a, v)| (a.as_str(), dot(q, v))).collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    scored
        .iter()
        .position(|(a, _)| accept.iter().any(|t| a.contains(t)))
        .map(|p| p + 1)
}

#[test]
#[ignore = "whitening A/B (needs --features dense); reads project store; writes nothing"]
fn whitening_report() {
    #[cfg(not(feature = "dense"))]
    {
        eprintln!("SKIP: rebuild with --features dense");
    }
    #[cfg(feature = "dense")]
    {
        use aden_index::EmbeddingProvider;
        use rayon::prelude::*;
        let Some(repo) = corpus() else {
            eprintln!("SKIP: corpus dir not found");
            return;
        };
        let cards = load_cards(&repo);
        if cards.is_empty() {
            eprintln!("SKIP: no store cards");
            return;
        }
        let Some(emb) = load_embedder() else {
            eprintln!("SKIP: bge model not found");
            return;
        };

        // Embed every card once.
        let raw: Vec<(String, Vec<f32>)> = cards
            .par_iter()
            .map(|(a, t)| (a.clone(), emb.embed(t)))
            .collect();
        let vecs: Vec<Vec<f32>> = raw.iter().map(|(_, v)| v.clone()).collect();
        let (mean, std) = mean_std(&vecs);

        // Pre-transform the card sets per whitening variant.
        let build = |m: Option<&[f32]>, s: Option<&[f32]>| -> Vec<(String, Vec<f32>)> {
            raw.iter()
                .map(|(a, v)| (a.clone(), transform(v, m, s)))
                .collect()
        };
        let raw_n = build(None, None);
        let cen_n = build(Some(&mean), None);
        let diag_n = build(Some(&mean), Some(&std));

        let probes = probes();
        let n = probes.len();
        println!(
            "\n=== Whitening A/B ({} cards, dim {}, {n} probes) ===",
            raw.len(),
            mean.len()
        );

        let (mut mr, mut mc, mut md) = (Metrics::default(), Metrics::default(), Metrics::default());
        for p in &probes {
            let qv = emb.embed(p.query);
            mr.add(rank(&raw_n, &transform(&qv, None, None), p.accept));
            mc.add(rank(&cen_n, &transform(&qv, Some(&mean), None), p.accept));
            md.add(rank(
                &diag_n,
                &transform(&qv, Some(&mean), Some(&std)),
                p.accept,
            ));
        }

        println!("\n  rank-of-gold:");
        println!("{}", mr.line("RAW", n));
        println!("{}", mc.line("CENTERED", n));
        println!("{}", md.line("DIAG", n));

        assert!(!raw.is_empty(), "no cards embedded");
    }
}
