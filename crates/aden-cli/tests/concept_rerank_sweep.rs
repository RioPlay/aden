// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Concept-rerank sweep — validate the +71% win (MRR 0.241) and find the rerank's real ceiling
// by sweeping its three knobs: lambda (boost weight), head (rerank depth), and k (neighbours
// per query term). Builds the SAME concept graph as concept_graph.rs (POS-tagged
// context-centroid nodes, mutual-kNN), then ONLY consumes it as a reranker over dense.
// (measurement harness, #[ignore]d, needs --features dense; writes nothing; .aden/store
// untouched.)
//
// The graph build (embed every card) is the only cost; the 36-config sweep is instant because
// dense scores and neighbour lists are precomputed once per probe. The headline config
// (lambda=0.5, head=20, k=8) appears in the table so the 0.241 is reproducible/anchored.
//
// Run: cargo test -p aden-cli --features dense --test concept_rerank_sweep -- --include-ignored --nocapture

#![cfg_attr(not(feature = "dense"), allow(dead_code))]

use std::path::{Path, PathBuf};

const MIN_DF: usize = 4;
const MAX_DF_FRAC: f64 = 0.15;
const NBR_BUILD: usize = 16; // build this many neighbours per concept; sweep k <= this

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

/// Per-card (anchor, token set, indexed text) for the whole store.
fn load_cards(repo: &Path) -> Vec<(String, Vec<String>, String)> {
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
    docs.values()
        // EVAL HYGIENE: exclude the test harnesses themselves — they pair each probe query with
        // its gold symbol name in one card, which LEAKS the answer key into the concept graph.
        .filter(|d| {
            !d.anchor.contains("/tests/")
                && !d
                    .attributes
                    .get("source_file")
                    .is_some_and(|s| s.contains("/tests/"))
        })
        .map(|d| {
            let text = index_text(d);
            (d.anchor.clone(), aden_index::tokenize(&text), text)
        })
        .collect()
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
}

#[test]
#[ignore = "concept-rerank sweep (needs --features dense); reads project store; writes nothing"]
fn concept_rerank_sweep() {
    #[cfg(not(feature = "dense"))]
    {
        eprintln!("SKIP: rebuild with --features dense");
    }
    #[cfg(feature = "dense")]
    {
        use aden_index::EmbeddingProvider;
        use rayon::prelude::*;
        use std::collections::HashMap;

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
        let n = cards.len();

        // Embed every card (parallel — the production path). Keep raw vecs for the centroid
        // (matches concept_graph.rs) and a normalized copy for the dense cosine.
        let card_vecs: Vec<Vec<f32>> = cards.par_iter().map(|(_, _, t)| emb.embed(t)).collect();
        let card_norm: Vec<Vec<f32>> = card_vecs.iter().map(|v| normalize(v.clone())).collect();
        let dim = card_vecs.first().map_or(0, |v| v.len());
        let anchors: Vec<&str> = cards.iter().map(|(a, _, _)| a.as_str()).collect();

        // Postings: concept term -> cards it appears in (df-gated, word-like).
        let max_df = (MAX_DF_FRAC * n as f64) as usize;
        let mut postings: HashMap<&str, Vec<usize>> = HashMap::new();
        for (ci, (_, toks, _)) in cards.iter().enumerate() {
            for t in toks {
                let len_ok = (3..=18).contains(&t.len());
                let wordish = t.chars().all(|c| c.is_ascii_alphanumeric())
                    && t.chars().any(|c| c.is_ascii_alphabetic());
                let hex = t.len() >= 8 && t.chars().all(|c| c.is_ascii_hexdigit());
                if len_ok && wordish && !hex {
                    postings.entry(t.as_str()).or_default().push(ci);
                }
            }
        }
        postings.retain(|_, v| (MIN_DF..=max_df).contains(&v.len()));

        let concepts: Vec<&str> = {
            let mut c: Vec<&str> = postings.keys().copied().collect();
            c.sort();
            c
        };
        // Context-centroid per concept: the model's sense of it IN this corpus.
        let centroid: HashMap<&str, Vec<f32>> = concepts
            .par_iter()
            .map(|&c| {
                let cc = &postings[c];
                let mut mean = vec![0.0f32; dim];
                for &i in cc {
                    for (m, &x) in mean.iter_mut().zip(&card_vecs[i]) {
                        *m += x;
                    }
                }
                let k = cc.len() as f32;
                for m in &mut mean {
                    *m /= k;
                }
                (c, normalize(mean))
            })
            .collect();
        // Top-NBR_BUILD neighbours per concept (the sweep's k truncates this list).
        let topk: HashMap<&str, Vec<&str>> = concepts
            .par_iter()
            .map(|&c| {
                let cv = &centroid[c];
                let mut sims: Vec<(&str, f32)> = concepts
                    .iter()
                    .filter(|&&o| o != c)
                    .map(|&o| (o, cosine(cv, &centroid[o])))
                    .collect();
                sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                sims.truncate(NBR_BUILD);
                (c, sims.into_iter().map(|(o, _)| o).collect())
            })
            .collect();

        let probes: &[(&str, &[&str], &str)] = &[
            (
                "store a batch of relationships between nodes in one operation",
                &["put_edges_bulk"],
                "append bulk typed edges deduplicate",
            ),
            (
                "group the graph into clusters of tightly connected nodes",
                &["detect_communities"],
                "community detection louvain modularity",
            ),
            (
                "blend two ranked result lists into a single ordering",
                &["rrf_fuse"],
                "reciprocal rank fusion combine rankings",
            ),
            (
                "how aligned are two embedding vectors",
                &["cosine_similarity"],
                "cosine similarity vector",
            ),
            (
                "fewest single character edits to turn one word into another",
                &["levenshtein_distance"],
                "levenshtein edit distance",
            ),
            (
                "figure out which definition a function call points to",
                &["resolve_callee"],
                "resolve callee definition anchor",
            ),
            (
                "decide what category of question the user is asking",
                &["classify_intent"],
                "classify intent query category",
            ),
            (
                "detect a leaked password or api key inside text",
                &["content_has_high_confidence_secret"],
                "secret credential api key detection",
            ),
            (
                "collect the nodes surrounding a starting symbol up to some depth",
                &["build_neighborhood"],
                "neighborhood traversal depth graph",
            ),
            (
                "find everything that points at a given node",
                &["get_incoming_edges"],
                "incoming edges backlinks callers references",
            ),
            (
                "how many tokens were avoided versus reading whole files",
                &["SavingsEstimate"],
                "savings estimate tokens baseline bytes",
            ),
            (
                "anchors in the graph that nothing else references",
                &["scan_orphans"],
                "scan orphan anchors unreferenced dangling",
            ),
        ];
        let np = probes.len();

        // Precompute once per probe: sorted dense scores, dense+oracle rank, neighbour lists.
        let rank_in = |sorted: &[(usize, f32)], accept: &[&str]| -> Option<usize> {
            sorted
                .iter()
                .position(|(i, _)| accept.iter().any(|t| anchors[*i].contains(t)))
                .map(|p| p + 1)
        };
        let mut sims_all: Vec<Vec<(usize, f32)>> = Vec::new();
        let mut nbrs_all: Vec<Vec<Vec<&str>>> = Vec::new();
        let mut accepts: Vec<&[&str]> = Vec::new();
        let mut dense_m = Metrics::default();
        let mut oracle_m = Metrics::default();
        for &(q, accept, oracle) in probes {
            let qv = normalize(emb.embed(q));
            let mut sims: Vec<(usize, f32)> = card_norm
                .iter()
                .enumerate()
                .map(|(i, v)| (i, cosine(&qv, v)))
                .collect();
            sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            dense_m.add(rank_in(&sims, accept));

            let ovec = normalize(emb.embed(&format!("{q} {oracle}")));
            let mut os: Vec<(usize, f32)> = card_norm
                .iter()
                .enumerate()
                .map(|(i, v)| (i, cosine(&ovec, v)))
                .collect();
            os.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            oracle_m.add(rank_in(&os, accept));

            let nbrs: Vec<Vec<&str>> = aden_index::tokenize(q)
                .iter()
                .filter_map(|t| topk.get(t.as_str()).cloned())
                .collect();
            sims_all.push(sims);
            nbrs_all.push(nbrs);
            accepts.push(accept);
        }

        // Sweep the three knobs; rerank is instant on the precomputed scores.
        let lambdas = [0.25f32, 0.5, 1.0, 2.0];
        let heads = [10usize, 20, 50];
        let ks = [4usize, 8, 16];
        // (mrr, r5, r1, lambda, head, k)
        let mut rows: Vec<(f64, usize, usize, f32, usize, usize)> = Vec::new();
        for &lambda in &lambdas {
            for &head in &heads {
                for &k in &ks {
                    let mut m = Metrics::default();
                    for pi in 0..np {
                        let sims = &sims_all[pi];
                        let mut nbr: Vec<&str> = Vec::new();
                        for inner in &nbrs_all[pi] {
                            for &o in inner.iter().take(k) {
                                if !nbr.contains(&o) {
                                    nbr.push(o);
                                }
                            }
                        }
                        let head_n = head.min(sims.len());
                        let maxd = sims.first().map(|x| x.1).unwrap_or(1.0).max(1e-9);
                        let mut h: Vec<(usize, f32)> = sims[..head_n]
                            .iter()
                            .map(|&(i, s)| {
                                let toks = &cards[i].1;
                                let overlap =
                                    nbr.iter().filter(|w| toks.iter().any(|t| t == *w)).count()
                                        as f32;
                                (i, s / maxd + lambda * overlap)
                            })
                            .collect();
                        h.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                        let rank = h
                            .iter()
                            .map(|x| x.0)
                            .chain(sims[head_n..].iter().map(|x| x.0))
                            .position(|i| accepts[pi].iter().any(|t| anchors[i].contains(t)))
                            .map(|p| p + 1);
                        m.add(rank);
                    }
                    rows.push((m.mrr / np as f64, m.r5, m.r1, lambda, head, k));
                }
            }
        }
        rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());

        let base = dense_m.mrr / np as f64;
        println!("\n=== Concept-rerank sweep ({np} probes, {n} cards, dim {dim}) ===");
        println!(
            "  DENSE   R@1 {}/{np}  R@5 {}/{np}  MRR {:.3}",
            dense_m.r1, dense_m.r5, base
        );
        println!(
            "  ORACLE  R@1 {}/{np}  R@5 {}/{np}  MRR {:.3}",
            oracle_m.r1,
            oracle_m.r5,
            oracle_m.mrr / np as f64
        );
        println!("\n  rerank configs, best MRR first:");
        for (mrr, r5, r1, lambda, head, k) in rows.iter().take(12) {
            println!(
                "    λ={lambda:<4} head={head:<3} k={k:<3}   R@1 {r1}/{np}  R@5 {r5}/{np}  MRR {mrr:.3}"
            );
        }
        let (bmrr, _, _, bl, bh, bk) = rows[0];
        println!(
            "\n  BEST: λ={bl} head={bh} k={bk} → MRR {bmrr:.3}  ({:+.1}% over dense {base:.3}; headline λ=0.5/head=20/k=8 = 0.241)",
            (bmrr - base) / base * 100.0
        );

        assert!(!rows.is_empty(), "no sweep rows");
    }
}
