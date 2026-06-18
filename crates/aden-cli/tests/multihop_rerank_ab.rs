// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Multi-hop traversal (RioPlay's model): don't stop at a query term's direct concept-graph
// neighbours — keep hopping along the SimilarTo edges until the graph stops giving anything new.
// Each reached node's weight is the CORRELATION PRODUCT along its path times a per-hop decay
// gamma; a path is abandoned when its weight falls below FLOOR ("can no longer hop"). So hop-1
// reproduces the validated 0.289 direct-rerank, and deeper hops reach the domain jargon two or
// three steps away (cluster -> community -> louvain) that the gold card uses but the query never
// says — weighted down so they break ties without flooding.
//
// Arms (rank-of-gold): DENSE, HOP-1 (= the 0.289 baseline), HOP-2, HOP-3, HOP-4, ORACLE. The
// depth-2 result is the proof of concept: if going past direct neighbours doesn't help there,
// deeper won't either. Each arm also reports its average reached-set size (the flooding cost).
// (measurement harness, #[ignore]d, needs --features dense; reads the project store; writes
// nothing; .aden/store untouched.)
//
// Run: cargo test -p aden-cli --features dense --test multihop_rerank_ab -- --include-ignored --nocapture

#![cfg_attr(not(feature = "dense"), allow(dead_code))]

use std::path::{Path, PathBuf};

const MIN_DF: usize = 4;
const MAX_DF_FRAC: f64 = 0.15;
const KNN: usize = 4; // concept-graph neighbours per node (sweep winner)
const HEAD: usize = 20; // rerank depth (sweep winner)
const GAMMA: f64 = 0.5; // per-hop decay
const FLOOR: f64 = 0.05; // abandon a path below this weight ("can no longer hop")
const DEPTHS: [usize; 4] = [1, 2, 3, 4];

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
    fn line(&self, label: &str, n: usize) -> String {
        format!(
            "    {label:<10} R@1 {}/{n}  R@5 {}/{n}  MRR {:.3}",
            self.r1,
            self.r5,
            self.mrr / n as f64
        )
    }
}

#[test]
#[ignore = "multi-hop concept-graph rerank (needs --features dense); reads project store; writes nothing"]
fn multihop_rerank_report() {
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

        let card_vecs: Vec<Vec<f32>> = cards.par_iter().map(|(_, _, t)| emb.embed(t)).collect();
        let card_norm: Vec<Vec<f32>> = card_vecs.iter().map(|v| normalize(v.clone())).collect();
        let dim = card_vecs.first().map_or(0, |v| v.len());
        let anchors: Vec<&str> = cards.iter().map(|(a, _, _)| a.as_str()).collect();

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
        // Concept graph: top-KNN neighbours per node, with the correlation = the edge weight.
        let topk: HashMap<&str, Vec<(&str, f32)>> = concepts
            .par_iter()
            .map(|&c| {
                let cv = &centroid[c];
                let mut sims: Vec<(&str, f32)> = concepts
                    .iter()
                    .filter(|&&o| o != c)
                    .map(|&o| (o, cosine(cv, &centroid[o])))
                    .collect();
                sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                sims.truncate(KNN);
                (c, sims)
            })
            .collect();

        // BFS from a query's terms along the concept graph to `depth` hops. A reached node's
        // weight is path-correlation-product * gamma^(hop-1); paths below FLOOR are abandoned.
        let expand = |qtoks: &[String], depth: usize| -> Vec<(String, f64)> {
            let mut best: HashMap<&str, f64> = HashMap::new();
            let mut frontier: Vec<(&str, f64)> = Vec::new();
            for qt in qtoks {
                if let Some((k, _)) = topk.get_key_value(qt.as_str()) {
                    frontier.push((*k, 1.0)); // seed: query term at hop 0, weight 1.0
                }
            }
            for hop in 1..=depth {
                let mult = if hop == 1 { 1.0 } else { GAMMA };
                let mut next: HashMap<&str, f64> = HashMap::new();
                for &(node, w) in &frontier {
                    let Some(ns) = topk.get(node) else { continue };
                    for &(nbr, corr) in ns {
                        let nw = w * corr as f64 * mult;
                        if nw < FLOOR || qtoks.iter().any(|q| q.as_str() == nbr) {
                            continue;
                        }
                        best.entry(nbr).and_modify(|e| *e = e.max(nw)).or_insert(nw);
                        next.entry(nbr).and_modify(|e| *e = e.max(nw)).or_insert(nw);
                    }
                }
                if next.is_empty() {
                    break; // can no longer hop
                }
                frontier = next.into_iter().collect();
            }
            best.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
        };

        let rerank =
            |sims: &[(usize, f32)], nbrs: &[(String, f64)], accept: &[&str]| -> Option<usize> {
                let head_n = HEAD.min(sims.len());
                let maxd = sims.first().map(|x| x.1).unwrap_or(1.0).max(1e-9) as f64;
                let mut h: Vec<(usize, f64)> = sims[..head_n]
                    .iter()
                    .map(|&(i, s)| {
                        let toks = &cards[i].1;
                        let boost: f64 = nbrs
                            .iter()
                            .filter(|(stem, _)| toks.iter().any(|t| t == stem))
                            .map(|(_, w)| *w)
                            .sum();
                        (i, s as f64 / maxd + boost)
                    })
                    .collect();
                h.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                h.iter()
                    .map(|x| x.0)
                    .chain(sims[head_n..].iter().map(|x| x.0))
                    .position(|i| accept.iter().any(|t| anchors[i].contains(t)))
                    .map(|p| p + 1)
            };

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

        let rank_in = |sorted: &[(usize, f32)], accept: &[&str]| -> Option<usize> {
            sorted
                .iter()
                .position(|(i, _)| accept.iter().any(|t| anchors[*i].contains(t)))
                .map(|p| p + 1)
        };

        // Precompute dense scores + oracle once per probe.
        let mut sims_all: Vec<Vec<(usize, f32)>> = Vec::new();
        let mut qtoks_all: Vec<Vec<String>> = Vec::new();
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

            sims_all.push(sims);
            qtoks_all.push(aden_index::tokenize(q));
            accepts.push(accept);
        }

        // Each depth arm: expand to `depth`, rerank, and track reached-set size (flooding cost).
        let mut arms: Vec<(Metrics, f64)> = Vec::new();
        for &depth in &DEPTHS {
            let mut m = Metrics::default();
            let mut reached_total = 0usize;
            for pi in 0..np {
                let nbrs = expand(&qtoks_all[pi], depth);
                reached_total += nbrs.len();
                m.add(rerank(&sims_all[pi], &nbrs, accepts[pi]));
            }
            arms.push((m, reached_total as f64 / np as f64));
        }

        println!(
            "\n=== Multi-hop concept-graph rerank ({np} probes, {n} cards, gamma {GAMMA}, floor {FLOOR}) ==="
        );
        println!("\n  rank-of-gold (reached = avg nodes in the traversal set):");
        println!("{}", dense_m.line("DENSE", np));
        for (i, &depth) in DEPTHS.iter().enumerate() {
            let (m, reached) = &arms[i];
            println!(
                "{}   [reached {reached:.0}]",
                m.line(&format!("HOP-{depth}"), np)
            );
        }
        println!("{}", oracle_m.line("ORACLE", np));

        assert!(!arms.is_empty(), "no depth arms");
    }
}
