// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Reranker headroom — top-K window + IDF weighting (tuning L1/L2, not a new layer).
// (measurement harness, #[ignore]d, needs --features dense, writes nothing.)
//
// The L2 reranker (PPMI + OEWN agreement) plateaued at MRR 0.198 — but it only reorders
// dense's top-20, and the calibration showed many golds sit BEYOND rank 20 (store #56,
// secret #50, neighbourhood #81, orphan #522): the reranker can't reach them. This tests
// two cheap tunings of the existing layer: a bigger window (top-K) so buried golds are
// reachable, and IDF weighting of query terms so distinctive words (cluster, louvain) count
// more than common ones (graph, node). No new signal — just squeezing L1/L2.
//
// Arms: DENSE · L2@20 · L2@50 · L2@100 · L2@100+IDF · ORACLE.
// Run: cargo test -p aden-cli --features dense --test rerank_topk_ab -- --include-ignored --nocapture

#![cfg_attr(not(feature = "dense"), allow(dead_code))]

use aden_core::EdgeType;
use aden_index::{Index, SearchResult};
use aden_store::{GraphStorage, Storage};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

type TermMap = HashMap<String, HashSet<String>>;

const MIN_DF: usize = 3;
const MAX_DF_FRAC: f64 = 0.20;

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

/// L2 relationship score (PPMI + OEWN agreement), optionally IDF-weighting each query term.
fn rel_score(
    qts: &[String],
    card: &HashSet<String>,
    postings: &TermMap,
    n: usize,
    max_df: usize,
    syn: &HashMap<String, HashSet<String>>,
    idf: bool,
) -> f64 {
    let df = |t: &str| postings.get(t).map_or(0, |s| s.len());
    let ok = |t: &str| (MIN_DF..=max_df).contains(&df(t));
    qts.iter()
        .filter(|qt| ok(qt))
        .map(|qt| {
            let w = if idf {
                (n as f64 / df(qt).max(1) as f64).ln().max(0.0)
            } else {
                1.0
            };
            let qsyn = syn.get(qt);
            let best = card
                .iter()
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
                .fold(0.0_f64, f64::max);
            w * best
        })
        .sum()
}

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
            "    {label:<14} R@1 {}/{n}   R@5 {}/{n}   R@10 {}/{n}   MRR {:.3}",
            self.r1,
            self.r5,
            self.r10,
            self.mrr / n as f64
        )
    }
}

#[test]
#[ignore = "reranker top-K + IDF tuning (needs --features dense); reads project store; writes nothing"]
fn rerank_topk_report() {
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
        println!("\n=== Reranker top-K + IDF tuning ({n_cards} cards, {n} probes) ===");

        // (topk, idf) configs.
        let cfgs: &[(usize, bool, &str)] = &[
            (20, false, "L2 @20"),
            (50, false, "L2 @50"),
            (100, false, "L2 @100"),
            (100, true, "L2 @100+IDF"),
        ];
        let mut dense_m = Metrics::default();
        let mut ms: Vec<Metrics> = cfgs.iter().map(|_| Metrics::default()).collect();
        let mut orc = Metrics::default();

        let empty = HashSet::new();
        for p in &probes {
            let dense = index.dense_query(p.query, &e);
            let qts: Vec<String> = aden_index::tokenize(p.query);
            let syn = build_syn(lex.as_ref(), &qts);

            dense_m.add(rank_anchors(
                &dense.iter().map(|r| r.anchor.clone()).collect::<Vec<_>>(),
                p.accept,
            ));
            for (cfg, m) in cfgs.iter().zip(ms.iter_mut()) {
                let (topk, idf, _) = *cfg;
                let rels: Vec<f64> = dense
                    .iter()
                    .take(topk)
                    .map(|r| {
                        let c = card_tokens.get(&r.anchor).unwrap_or(&empty);
                        rel_score(&qts, c, &postings, n_cards, max_df, &syn, idf)
                    })
                    .collect();
                m.add(rank_anchors(&rerank(&dense, &rels, 2.0), p.accept));
            }
            orc.add(rank_anchors(
                &index
                    .query(&format!("{} {}", p.query, p.expand))
                    .iter()
                    .map(|r| r.anchor.clone())
                    .collect::<Vec<_>>(),
                p.accept,
            ));
        }

        println!("\n  rank-of-gold (rerank weight 2.0):");
        println!("{}", dense_m.line("DENSE", n));
        for (cfg, m) in cfgs.iter().zip(ms.iter()) {
            println!("{}", m.line(cfg.2, n));
        }
        println!("{}", orc.line("ORACLE", n));

        assert!(n_cards > 0, "no cards");
    }
}
