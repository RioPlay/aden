// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Graph-aware routing — use aden's actual prose→code edges, the thing every flat experiment
// today ignored. (measurement harness, #[ignore]d, needs --features dense, writes nothing.)
//
// The gap to the oracle is a domain bridge (cluster→louvain) that lives in PROSE, not the
// terse code card. aden already wrote the edges to walk it: a doc/comment paragraph that
// describes the code in NL `Mentions`/`Demonstrates`/`Documents` the code symbol (verified
// against link_store_edges via aden-on-aden). So instead of matching NL→code directly, we do
// the two-hop the graph offers: dense-match the PROSE (NL↔NL, which dense does well), then
// SPREAD each top seed's relevance along its outgoing prose→code edges to the symbol it
// describes — and rerank by the spread. This routes NL→prose→code through the real graph.
//
// Coverage matters: Mentions/Demonstrates only link unambiguous names, so they're sparse;
// the harness reports how many boosts actually fired.
//
// Arms: DENSE · ROUTE γ=2 · γ=8 · γ=20 · ORACLE. (seed top-50, edges
// Mentions/Demonstrates/Documents/RelatesTo.)
// Run: cargo test -p aden-cli --features dense --test graph_route_ab -- --include-ignored --nocapture

#![cfg_attr(not(feature = "dense"), allow(dead_code))]

use aden_core::EdgeType;
use aden_index::Index;
use aden_store::{GraphStorage, Storage};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const SEED_K: usize = 10;

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

/// Build the dense index AND keep the store handle open for edge traversal.
fn load(repo: &Path) -> Option<(Index, Storage)> {
    let root = aden_paths::resolve_root(repo);
    let (store_path, _) = aden_paths::resolve_read_store(&root);
    let storage = Storage::open_existing(store_path.to_str()?).ok()?;
    let docs = storage.get_all_documents().ok()?;
    let mut entries: Vec<(PathBuf, String)> = docs
        .values()
        .map(|d| {
            let p = d
                .attributes
                .get("source_file")
                .cloned()
                .unwrap_or_else(|| d.anchor.clone());
            (PathBuf::from(p), index_text(d))
        })
        .collect();
    if entries.is_empty() {
        return None;
    }
    entries.sort_by(|a, b| a.1.cmp(&b.1));
    let mut index = Index::default();
    index.ingest(entries);
    index.finalize();
    Some((index, storage))
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

/// Prose→code (and containment) edge types we route along.
fn is_route_edge(et: &EdgeType) -> bool {
    // Precise prose→code only: the unambiguous, human-authored NL bridges. Excludes
    // Documents (carries module→symbol containment, which floods) and RelatesTo (prose→prose).
    matches!(et, EdgeType::Mentions | EdgeType::Demonstrates)
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
            "    {label:<10} R@1 {}/{n}   R@5 {}/{n}   R@10 {}/{n}   MRR {:.3}",
            self.r1,
            self.r5,
            self.r10,
            self.mrr / n as f64
        )
    }
}

/// Rank the dense list after adding the prose→code spread (`gamma * normalized_boost`).
fn route(
    dense: &[aden_index::SearchResult],
    boost: &HashMap<String, f64>,
    gamma: f64,
) -> Vec<String> {
    let max_b = boost.values().cloned().fold(0.0_f64, f64::max).max(1e-9);
    let mut scored: Vec<(&str, f64)> = dense
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let b = boost.get(&r.anchor).copied().unwrap_or(0.0) / max_b;
            (r.anchor.as_str(), 1.0 / (i + 1) as f64 + gamma * b)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    scored.into_iter().map(|(a, _)| a.to_string()).collect()
}

#[test]
#[ignore = "graph-aware routing (needs --features dense); reads project store; writes nothing"]
fn graph_route_report() {
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
        let Some((mut index, storage)) = load(&repo) else {
            eprintln!("SKIP: no project store cards");
            return;
        };
        let Some(e) = load_embedder() else {
            eprintln!("SKIP: bge model not found");
            return;
        };
        index.embed_documents(&e);

        let probes = probes();
        let n = probes.len();
        println!("\n=== Graph-aware routing (prose→code edges, {n} probes) ===");

        let gammas = [2.0, 8.0, 20.0];
        let mut dense_m = Metrics::default();
        let mut ms: Vec<Metrics> = gammas.iter().map(|_| Metrics::default()).collect();
        let mut orc = Metrics::default();
        let mut total_boosts = 0usize;

        for p in &probes {
            let dense = index.dense_query(p.query, &e);

            // Spread each top-SEED_K seed's relevance along its prose→code edges.
            let mut boost: HashMap<String, f64> = HashMap::new();
            for (i, r) in dense.iter().take(SEED_K).enumerate() {
                let w = 1.0 / (i + 1) as f64;
                if let Ok(edges) = storage.get_outgoing_edges(&r.anchor) {
                    for (tgt, et) in edges {
                        if is_route_edge(&et) {
                            *boost.entry(tgt).or_default() += w;
                        }
                    }
                }
            }
            total_boosts += boost.len();

            dense_m.add(rank_anchors(
                &dense.iter().map(|r| r.anchor.clone()).collect::<Vec<_>>(),
                p.accept,
            ));
            for (g, m) in gammas.iter().zip(ms.iter_mut()) {
                m.add(rank_anchors(&route(&dense, &boost, *g), p.accept));
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

        println!(
            "  prose→code boosts fired: {total_boosts} total over {n} probes (avg {:.1}/probe)",
            total_boosts as f64 / n as f64
        );
        println!("\n  rank-of-gold:");
        println!("{}", dense_m.line("DENSE", n));
        for (g, m) in gammas.iter().zip(ms.iter()) {
            println!("{}", m.line(&format!("ROUTE γ={g:.0}"), n));
        }
        println!("{}", orc.line("ORACLE", n));

        assert!(!probes.is_empty(), "no probes");
    }
}
