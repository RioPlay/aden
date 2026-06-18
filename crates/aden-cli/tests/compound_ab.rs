// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Compounding — does reranking the FUSION-FIXED base beat reranking pure dense?
// (measurement harness, #[ignore]d, needs --features dense, writes nothing.)
//
// Two independent wins this session: the fusion fix (dense-weighted RRF 1:5, base MRR 0.182
// vs pure dense 0.146) and the L2 reranker (rerank pure dense → 0.203). They act at different
// stages — fusion makes the base, the reranker reorders the base's top-K — so they should
// stack. This measures whether they do: rerank the RRF-1:5 base and compare to reranking pure
// dense. If the rerank lift carries onto the better base, that's the compounding the
// iterate-the-loop plan predicts; if not, the two are redundant and we pick one.
//
// Arms: DENSE · HYBRID(RRF 1:5) · RERANK→DENSE · RERANK→HYBRID · ORACLE. (rerank weight 2.0,
// window 100, L2 signal = PPMI + OEWN agreement.)
// Run: cargo test -p aden-cli --features dense --test compound_ab -- --include-ignored --nocapture

#![cfg_attr(not(feature = "dense"), allow(dead_code))]

use aden_core::EdgeType;
use aden_index::{Index, SearchResult};
use aden_store::{GraphStorage, Storage};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

type TermMap = HashMap<String, HashSet<String>>;

const MIN_DF: usize = 3;
const MAX_DF_FRAC: f64 = 0.20;
const TOPK: usize = 100;

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

fn load(repo: &Path) -> Option<(Index, TermMap, TermMap)> {
    let root = aden_paths::resolve_root(repo);
    let (store_path, _) = aden_paths::resolve_read_store(&root);
    let storage = Storage::open_existing(store_path.to_str()?).ok()?;
    let docs = storage.get_all_documents().ok()?;
    let mut entries: Vec<(PathBuf, String)> = Vec::new();
    let mut card_tokens: TermMap = HashMap::new();
    let mut postings: TermMap = HashMap::new();
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

fn lexicon_path() -> PathBuf {
    std::env::var("ADEN_LEXICON_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache/aden/lexicon")
        })
}

fn build_syn(lex: Option<&Storage>, qts: &[String]) -> HashMap<String, HashSet<String>> {
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    let Some(l) = lex else {
        return out;
    };
    for qt in qts {
        if let Ok(edges) = l.get_outgoing_edges(&format!("aden://term/oewn/{qt}")) {
            let s: HashSet<String> = edges
                .into_iter()
                .filter(|(_, et)| matches!(et, EdgeType::SynonymOf))
                .map(|(tgt, _)| tgt.rsplit('/').next().unwrap_or(&tgt).to_string())
                .collect();
            if !s.is_empty() {
                out.insert(qt.clone(), s);
            }
        }
    }
    out
}

fn ppmi(a: &str, b: &str, postings: &TermMap, n: usize) -> f64 {
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

fn rel_score(
    qts: &[String],
    card: &HashSet<String>,
    postings: &TermMap,
    n: usize,
    max_df: usize,
    syn: &HashMap<String, HashSet<String>>,
) -> f64 {
    let df = |t: &str| postings.get(t).map_or(0, |s| s.len());
    let ok = |t: &str| (MIN_DF..=max_df).contains(&df(t));
    qts.iter()
        .filter(|qt| ok(qt))
        .map(|qt| {
            let qsyn = syn.get(qt);
            card.iter()
                .filter(|ct| ok(ct))
                .map(|ct| {
                    let p = ppmi(qt, ct, postings, n);
                    let o = if qsyn.is_some_and(|s| s.contains(ct)) {
                        3.0
                    } else {
                        0.0
                    };
                    p + o
                })
                .fold(0.0_f64, f64::max)
        })
        .sum()
}

/// Weighted RRF (k=60) of two ranked lists → fused anchor order (the fusion fix).
fn rrf(bm25: &[SearchResult], dense: &[SearchResult], w_bm: f64, w_de: f64) -> Vec<String> {
    const K: f64 = 60.0;
    let mut score: HashMap<&str, f64> = HashMap::new();
    for (i, r) in bm25.iter().enumerate() {
        *score.entry(r.anchor.as_str()).or_default() += w_bm / (K + (i + 1) as f64);
    }
    for (i, r) in dense.iter().enumerate() {
        *score.entry(r.anchor.as_str()).or_default() += w_de / (K + (i + 1) as f64);
    }
    let mut v: Vec<(&str, f64)> = score.into_iter().collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(b.0)));
    v.into_iter().map(|(a, _)| a.to_string()).collect()
}

/// Rerank a base ranked anchor list by `base_rank_score + w_rel * normalized_rel` over its
/// top-`rels.len()`, leaving the tail in base order.
fn rerank_list(base: &[String], rels: &[f64], w_rel: f64) -> Vec<String> {
    let max_rel = rels.iter().cloned().fold(0.0_f64, f64::max).max(1e-9);
    let mut head: Vec<(usize, f64)> = (0..rels.len())
        .map(|i| (i, 1.0 / (i + 1) as f64 + w_rel * rels[i] / max_rel))
        .collect();
    head.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    head.into_iter()
        .map(|(i, _)| base[i].clone())
        .chain(base.iter().skip(rels.len()).cloned())
        .collect()
}

fn rank_anchors(list: &[String], accept: &[&str]) -> Option<usize> {
    list.iter()
        .position(|a| accept.iter().any(|t| a.contains(t)))
        .map(|p| p + 1)
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

#[test]
#[ignore = "compounding A/B (needs --features dense); reads project store; writes nothing"]
fn compound_report() {
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
        let lex = Storage::open_existing(lexicon_path().to_str().unwrap_or_default()).ok();
        let probes = probes();
        let n = probes.len();
        println!("\n=== Compounding ({n_cards} cards, {n} probes) ===");

        let (mut md, mut mh, mut mrd, mut mrh, mut orc) = (
            Metrics::default(),
            Metrics::default(),
            Metrics::default(),
            Metrics::default(),
            Metrics::default(),
        );
        let empty = HashSet::new();
        for p in &probes {
            let bm = index.query(p.query);
            let de = index.dense_query(p.query, &e);
            let qts: Vec<String> = aden_index::tokenize(p.query);
            let syn = build_syn(lex.as_ref(), &qts);

            let dense_base: Vec<String> = de.iter().map(|r| r.anchor.clone()).collect();
            let hybrid_base: Vec<String> = rrf(&bm, &de, 1.0, 5.0);

            let rels_for = |base: &[String]| -> Vec<f64> {
                base.iter()
                    .take(TOPK)
                    .map(|a| {
                        let c = card_tokens.get(a).unwrap_or(&empty);
                        rel_score(&qts, c, &postings, n_cards, max_df, &syn)
                    })
                    .collect()
            };
            let rd = rels_for(&dense_base);
            let rh = rels_for(&hybrid_base);

            md.add(rank_anchors(&dense_base, p.accept));
            mh.add(rank_anchors(&hybrid_base, p.accept));
            mrd.add(rank_anchors(&rerank_list(&dense_base, &rd, 2.0), p.accept));
            mrh.add(rank_anchors(&rerank_list(&hybrid_base, &rh, 2.0), p.accept));
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
        println!("{}", md.line("DENSE", n));
        println!("{}", mh.line("HYBRID 1:5", n));
        println!("{}", mrd.line("RERANK→DENSE", n));
        println!("{}", mrh.line("RERANK→HYBRID", n));
        println!("{}", orc.line("ORACLE", n));

        assert!(n_cards > 0, "no cards");
    }
}
