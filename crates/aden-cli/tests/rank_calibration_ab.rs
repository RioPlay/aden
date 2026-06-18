// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Rank-of-gold calibration — was R@1-only measurement hiding the signal? (measurement
// harness, #[ignore]d, reads the project store, writes nothing.)
//
// Every prior gate scored top-1 hit/miss (R@1) on BM25. That is the harshest possible
// metric: an arm that moves the gold from rank 200 to rank 3 scores MISS and looks dead.
// This harness measures rank-of-gold at R@1, R@5, and MRR across:
//   * BM25            — lexical baseline.
//   * HYBRID          — BM25 + dense (bge) via RRF — where retrieval quality actually lives.
//   * BM25 + ORACLE   — hand-authored correct-sense expansion (the ceiling).
// If HYBRID's R@5/MRR is strong even when R@1 is 0, then aden's retrieval was fine all
// along and the R@1 lens was the problem — and a relationship layer should be judged on
// rank shift, not top-1 flips.
//
// Run: cargo test -p aden-cli --features dense --test rank_calibration_ab -- --include-ignored --nocapture

use aden_index::{Index, SearchResult};
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

fn build_index(repo: &Path) -> Option<(Index, usize)> {
    use aden_store::{GraphStorage, Storage};
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
    let n = entries.len();
    let mut index = Index::default();
    index.ingest(entries);
    index.finalize();
    Some((index, n))
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

/// 1-based rank of the first result whose anchor carries an accepted symbol; None if the
/// gold is absent from the returned ranking.
fn rank_of(results: &[SearchResult], accept: &[&str]) -> Option<usize> {
    results
        .iter()
        .position(|r| accept.iter().any(|t| r.anchor.contains(t)))
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

#[test]
#[ignore = "rank-of-gold calibration (R@1/R@5/MRR + dense); reads project store; writes nothing"]
fn rank_calibration_report() {
    let Some(repo) = corpus() else {
        eprintln!("SKIP: corpus dir not found");
        return;
    };
    #[cfg_attr(not(feature = "dense"), allow(unused_mut))]
    let Some((mut index, n_cards)) = build_index(&repo) else {
        eprintln!("SKIP: no project store cards — run `aden gen`");
        return;
    };

    #[cfg(feature = "dense")]
    let embedder = load_embedder();
    #[cfg(feature = "dense")]
    if let Some(e) = &embedder {
        index.embed_documents(e);
    }
    let hybrid_on = {
        #[cfg(feature = "dense")]
        {
            embedder.is_some()
        }
        #[cfg(not(feature = "dense"))]
        {
            false
        }
    };

    let probes = probes();
    let n = probes.len();
    println!("\n=== Rank-of-gold calibration ({n_cards} cards, {n} probes) ===");
    println!(
        "hybrid arm: {}\n",
        if hybrid_on {
            "ON (bge)"
        } else {
            "OFF (no dense/model)"
        }
    );

    let (mut bm25, mut dense, mut hybrid, mut oracle) = (
        Metrics::default(),
        Metrics::default(),
        Metrics::default(),
        Metrics::default(),
    );
    for p in &probes {
        let b = rank_of(&index.query(p.query), p.accept);
        let o = rank_of(&index.query(&format!("{} {}", p.query, p.expand)), p.accept);
        let (d, h) = {
            #[cfg(feature = "dense")]
            {
                if let Some(e) = &embedder {
                    (
                        rank_of(&index.dense_query(p.query, e), p.accept),
                        rank_of(&index.hybrid_query(p.query, e), p.accept),
                    )
                } else {
                    (None, None)
                }
            }
            #[cfg(not(feature = "dense"))]
            {
                (None, None)
            }
        };
        bm25.add(b);
        dense.add(d);
        hybrid.add(h);
        oracle.add(o);

        let fr = |r: Option<usize>| r.map_or("--".to_string(), |x| x.to_string());
        println!(
            "  bm25 #{:<5} dense #{:<5} hybrid #{:<5} oracle #{:<4}  q: {}",
            fr(b),
            fr(d),
            fr(h),
            fr(o),
            p.query
        );
    }

    println!("\n  rank-of-gold:");
    println!("{}", bm25.line("BM25", n));
    if hybrid_on {
        println!("{}", dense.line("DENSE (pure)", n));
        println!("{}", hybrid.line("HYBRID (RRF)", n));
    }
    println!("{}", oracle.line("BM25 + ORACLE", n));

    assert!(n_cards > 0, "no cards");
}
