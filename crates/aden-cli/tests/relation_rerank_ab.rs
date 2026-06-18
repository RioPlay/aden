// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Relationship reranker (Layer 1) — do corpus relationships rerank dense's top-K up?
// (measurement harness, #[ignore]d, needs --features dense, writes nothing.)
//
// The rank calibration proved the signal lives in the DENSE path (gold at rank ~2–7), and
// query expansion is the wrong mechanism. This tests the RIGHT one: take dense's top-20 and
// RERANK by how strongly each candidate card's terms RELATE to the query's terms in the
// corpus graph — PPMI co-occurrence, which is domain-specific by construction (cluster↔louvain
// is strong HERE because this codebase uses Louvain). rel(card) = Σ_qt max_ct PPMI(qt, ct),
// over df-gated terms. Reranked order = blend of dense rank + rel; sweep the rel weight.
//
// This is Layer 1 of the multi-layer plan (flat corpus relationships). If it lifts the rank,
// community-scoping + multi-signal validation (Layers 2–3) build on it; if not, we learn the
// scoping is what's load-bearing. Either way we measure, not guess.
//
// Arms: DENSE (w_rel=0) · RERANK w=0.5 · w=1.0 · w=2.0 · ORACLE(reference).
// Run: cargo test -p aden-cli --features dense --test relation_rerank_ab -- --include-ignored --nocapture

#![cfg_attr(not(feature = "dense"), allow(dead_code))]

use aden_index::{Index, SearchResult};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// term/card → set-of-anchors map (postings and per-card token sets share this shape).
type TermMap = HashMap<String, HashSet<String>>;

const MIN_DF: usize = 3;
const MAX_DF_FRAC: f64 = 0.20;
const TOPK: usize = 20;

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

/// Build the index, the per-card token sets keyed by anchor, and term→{anchors} postings.
fn load(repo: &Path) -> Option<(Index, TermMap, TermMap)> {
    use aden_store::{GraphStorage, Storage};
    let root = aden_paths::resolve_root(repo);
    let (store_path, _) = aden_paths::resolve_read_store(&root);
    let storage = Storage::open_existing(store_path.to_str()?).ok()?;
    let docs = storage.get_all_documents().ok()?;

    let mut entries: Vec<(PathBuf, String)> = Vec::new();
    let mut card_tokens: HashMap<String, HashSet<String>> = HashMap::new();
    let mut postings: HashMap<String, HashSet<String>> = HashMap::new();
    for d in docs.values() {
        let text = index_text(d);
        let toks: HashSet<String> = aden_index::tokenize(&text).into_iter().collect();
        for t in &toks {
            postings
                .entry(t.clone())
                .or_default()
                .insert(d.anchor.clone());
        }
        card_tokens.insert(d.anchor.clone(), toks);
        let p = d
            .attributes
            .get("source_file")
            .cloned()
            .unwrap_or_else(|| d.anchor.clone());
        entries.push((PathBuf::from(p), text));
    }
    if entries.is_empty() {
        return None;
    }
    entries.sort_by(|a, b| a.1.cmp(&b.1));
    let mut index = Index::default();
    index.ingest(entries);
    index.finalize();
    Some((index, card_tokens, postings))
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

/// Positive PMI of two terms over the term↔card graph; 0 if either is absent or they never
/// co-occur. Domain-specific by construction (counts are over THIS corpus).
fn ppmi(a: &str, b: &str, postings: &HashMap<String, HashSet<String>>, n: usize) -> f64 {
    let (Some(pa), Some(pb)) = (postings.get(a), postings.get(b)) else {
        return 0.0;
    };
    let (small, big) = if pa.len() <= pb.len() {
        (pa, pb)
    } else {
        (pb, pa)
    };
    let co = small.iter().filter(|x| big.contains(*x)).count();
    if co == 0 {
        return 0.0;
    }
    ((co as f64 * n as f64) / (pa.len() as f64 * pb.len() as f64))
        .log2()
        .max(0.0)
}

/// Relationship score of a card for a query: each query term takes its best-related card
/// term (max PPMI), summed. Only df-informative terms participate (skips boilerplate + rare).
fn rel(
    qts: &[String],
    card: &HashSet<String>,
    postings: &HashMap<String, HashSet<String>>,
    n: usize,
    max_df: usize,
) -> f64 {
    let ok = |t: &str| {
        let d = postings.get(t).map_or(0, |s| s.len());
        (MIN_DF..=max_df).contains(&d)
    };
    qts.iter()
        .filter(|qt| ok(qt))
        .map(|qt| {
            card.iter()
                .filter(|ct| ok(ct))
                .map(|ct| ppmi(qt, ct, postings, n))
                .fold(0.0_f64, f64::max)
        })
        .sum()
}

struct Probe {
    query: &'static str,
    accept: &'static [&'static str],
    expand: &'static str,
}

fn probes() -> Vec<Probe> {
    vec![
        Probe {
            query: "store a batch of relationships between nodes in one operation",
            accept: &["put_edges_bulk"],
            expand: "append bulk typed edges deduplicate",
        },
        Probe {
            query: "group the graph into clusters of tightly connected nodes",
            accept: &["detect_communities"],
            expand: "community detection louvain modularity",
        },
        Probe {
            query: "blend two ranked result lists into a single ordering",
            accept: &["rrf_fuse"],
            expand: "reciprocal rank fusion combine rankings",
        },
        Probe {
            query: "how aligned are two embedding vectors",
            accept: &["cosine_similarity"],
            expand: "cosine similarity vector",
        },
        Probe {
            query: "fewest single character edits to turn one word into another",
            accept: &["levenshtein_distance"],
            expand: "levenshtein edit distance",
        },
        Probe {
            query: "figure out which definition a function call points to",
            accept: &["resolve_callee"],
            expand: "resolve callee definition anchor",
        },
        Probe {
            query: "decide what category of question the user is asking",
            accept: &["classify_intent"],
            expand: "classify intent query category",
        },
        Probe {
            query: "detect a leaked password or api key inside text",
            accept: &["content_has_high_confidence_secret"],
            expand: "secret credential api key detection",
        },
        Probe {
            query: "collect the nodes surrounding a starting symbol up to some depth",
            accept: &["build_neighborhood"],
            expand: "neighborhood traversal depth graph",
        },
        Probe {
            query: "find everything that points at a given node",
            accept: &["get_incoming_edges"],
            expand: "incoming edges backlinks callers references",
        },
        Probe {
            query: "how many tokens were avoided versus reading whole files",
            accept: &["SavingsEstimate"],
            expand: "savings estimate tokens baseline bytes",
        },
        Probe {
            query: "anchors in the graph that nothing else references",
            accept: &["scan_orphans"],
            expand: "scan orphan anchors unreferenced dangling",
        },
    ]
}

fn rank_anchors(list: &[String], accept: &[&str]) -> Option<usize> {
    list.iter()
        .position(|a| accept.iter().any(|t| a.contains(t)))
        .map(|p| p + 1)
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
            "    {label:<16} R@1 {}/{n}   R@5 {}/{n}   R@10 {}/{n}   MRR {:.3}",
            self.r1,
            self.r5,
            self.r10,
            self.mrr / n as f64
        )
    }
}

/// Rerank dense's top-`TOPK` by `dense_rank_score + w_rel * normalized_rel`, leaving the
/// tail (rank > TOPK) in dense order. Returns the reranked anchor list.
fn rerank(dense: &[SearchResult], rels: &[f64], w_rel: f64) -> Vec<String> {
    let max_rel = rels.iter().cloned().fold(0.0_f64, f64::max).max(1e-9);
    let mut head: Vec<(usize, f64)> = (0..rels.len())
        .map(|i| (i, 1.0 / (i + 1) as f64 + w_rel * rels[i] / max_rel))
        .collect();
    head.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    head.into_iter()
        .map(|(i, _)| dense[i].anchor.clone())
        .chain(dense.iter().skip(rels.len()).map(|r| r.anchor.clone()))
        .collect()
}

#[test]
#[ignore = "relationship reranker (needs --features dense); reads project store; writes nothing"]
fn relation_rerank_report() {
    #[cfg(not(feature = "dense"))]
    {
        eprintln!("SKIP: rebuild with --features dense");
    }
    #[cfg(feature = "dense")]
    {
        let Some(repo) = corpus() else {
            eprintln!("SKIP: corpus dir not found");
            return;
        };
        let Some((mut index, card_tokens, postings)) = load(&repo) else {
            eprintln!("SKIP: no project store cards");
            return;
        };
        let Some(e) = load_embedder() else {
            eprintln!("SKIP: bge model not found");
            return;
        };
        index.embed_documents(&e);

        let n_cards = card_tokens.len();
        let max_df = (MAX_DF_FRAC * n_cards as f64) as usize;
        let probes = probes();
        let n = probes.len();
        println!("\n=== Relationship reranker — Layer 1 ({n_cards} cards, {n} probes) ===");

        let mut dense_m = Metrics::default();
        let mut r05 = Metrics::default();
        let mut r10w = Metrics::default();
        let mut r20w = Metrics::default();
        let mut orc = Metrics::default();

        let empty = HashSet::new();
        for p in &probes {
            let dense = index.dense_query(p.query, &e);
            let head: Vec<&SearchResult> = dense.iter().take(TOPK).collect();
            let qts: Vec<String> = aden_index::tokenize(p.query);
            let rels: Vec<f64> = head
                .iter()
                .map(|r| {
                    let card = card_tokens.get(&r.anchor).unwrap_or(&empty);
                    rel(&qts, card, &postings, n_cards, max_df)
                })
                .collect();

            dense_m.add(rank_anchors(&rerank(&dense, &rels, 0.0), p.accept));
            r05.add(rank_anchors(&rerank(&dense, &rels, 0.5), p.accept));
            r10w.add(rank_anchors(&rerank(&dense, &rels, 1.0), p.accept));
            r20w.add(rank_anchors(&rerank(&dense, &rels, 2.0), p.accept));
            orc.add(rank_anchors(
                &index
                    .query(&format!("{} {}", p.query, p.expand))
                    .iter()
                    .map(|r| r.anchor.clone())
                    .collect::<Vec<_>>(),
                p.accept,
            ));
        }

        println!("\n  rank-of-gold:");
        println!("{}", dense_m.line("DENSE (rel=0)", n));
        println!("{}", r05.line("RERANK w=0.5", n));
        println!("{}", r10w.line("RERANK w=1.0", n));
        println!("{}", r20w.line("RERANK w=2.0", n));
        println!("{}", orc.line("ORACLE (ref)", n));

        assert!(n_cards > 0, "no cards");
    }
}
