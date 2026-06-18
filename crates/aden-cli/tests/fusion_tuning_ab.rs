// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Fusion tuning — what's the BEST way to combine BM25 + dense? (measurement harness,
// #[ignore]d, reads the project store, writes nothing.)
//
// The rank calibration found that equal-weight RRF (aden's shipped hybrid) is WORSE than
// pure dense on the synonym-mismatch regime (MRR 0.061 vs 0.146): fusing BM25's rank-~100
// garbage drags dense's rank-~3 gold down. This harness sweeps fusion strategies over the
// SAME bm25 + dense result lists (one embed pass) to find the optimal combine — and to
// answer: does ANY fusion beat pure dense (so BM25 still adds value when weighted right),
// or should the weak-BM25 regime just use dense?
//
// Arms (rank-based RRF, k=60, varying the dense weight):
//   BM25 · DENSE(pure) · RRF 1:1 (current) · RRF 1:3 · RRF 1:5 · ORACLE(reference)
//
// Run: cargo test -p aden-cli --features dense --test fusion_tuning_ab -- --include-ignored --nocapture

// The whole harness body is behind `cfg(feature = "dense")`; without it the helpers are
// (correctly) unused, so suppress dead-code noise only in that build.
#![cfg_attr(not(feature = "dense"), allow(dead_code))]

use aden_index::{Index, SearchResult};
use std::collections::HashMap;
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

/// Weighted Reciprocal Rank Fusion (k=60) over two ranked lists → fused anchor order.
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

fn anchors(results: &[SearchResult]) -> Vec<String> {
    results.iter().map(|r| r.anchor.clone()).collect()
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
            "    {label:<14} R@1 {}/{n}   R@5 {}/{n}   R@10 {}/{n}   MRR {:.3}",
            self.r1,
            self.r5,
            self.r10,
            self.mrr / n as f64
        )
    }
}

#[test]
#[ignore = "fusion tuning sweep (needs --features dense); reads project store; writes nothing"]
fn fusion_tuning_report() {
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
        let Some((mut index, n_cards)) = build_index(&repo) else {
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
        println!("\n=== Fusion tuning ({n_cards} cards, {n} probes) ===");

        let mut bm = Metrics::default();
        let mut de = Metrics::default();
        let mut r11 = Metrics::default();
        let mut r13 = Metrics::default();
        let mut r15 = Metrics::default();
        let mut orc = Metrics::default();

        for p in &probes {
            let b = index.query(p.query);
            let d = index.dense_query(p.query, &e);
            let o = index.query(&format!("{} {}", p.query, p.expand));

            bm.add(rank_anchors(&anchors(&b), p.accept));
            de.add(rank_anchors(&anchors(&d), p.accept));
            r11.add(rank_anchors(&rrf(&b, &d, 1.0, 1.0), p.accept));
            r13.add(rank_anchors(&rrf(&b, &d, 1.0, 3.0), p.accept));
            r15.add(rank_anchors(&rrf(&b, &d, 1.0, 5.0), p.accept));
            orc.add(rank_anchors(&anchors(&o), p.accept));
        }

        println!("\n  rank-of-gold:");
        println!("{}", bm.line("BM25", n));
        println!("{}", de.line("DENSE (pure)", n));
        println!("{}", r11.line("RRF 1:1", n));
        println!("{}", r13.line("RRF 1:3", n));
        println!("{}", r15.line("RRF 1:5", n));
        println!("{}", orc.line("ORACLE", n));

        assert!(n_cards > 0, "no cards");
    }
}
