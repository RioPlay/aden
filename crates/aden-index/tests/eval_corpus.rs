// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Real-corpus retrieval evaluation harness (scaffold).
//!
//! Where `eval.rs` is a *synthetic, hermetic* fixture (one-sentence docs hand-built
//! to exercise a specific ranking property), this harness scores retrieval over a
//! **directory of real document files** plus a `queries.tsv` of natural-language
//! queries with the source file each should route to. It is the substrate for a
//! *published* recall number — the CQS-style "R@1 / R@5 / R@20 on a real corpus"
//! that turns "aden has hybrid retrieval" into a defensible figure.
//!
//! ## Layout
//!
//! A corpus directory contains:
//! - `docs/` — `.adoc` / `.txt` / `.aden` document files (walked recursively).
//! - `queries.tsv` — tab-separated `query <TAB> expected_file_stem <TAB> note`
//!   (the `note` column is optional). Lines starting with `#` are comments.
//!
//! Expectations are keyed on the **source file stem** (via `SearchResult::source_path`),
//! not the internal `[[anchor]]`, so cases stay stable regardless of how a file's
//! anchors are derived.
//!
//! ## Corpus selection
//!
//! - Default: the committed starter corpus at `tests/corpus/` (small but real prose;
//!   always runs in CI so the numbers are recorded every build).
//! - Override: set `ADEN_EVAL_CORPUS_DIR=/path/to/corpus` to point at a larger,
//!   prepared real-repo eval set (same layout). This is how you grow toward a
//!   publishable benchmark without touching this file.
//!
//! ## Running
//!
//! ```text
//! cargo test -p aden-index --test eval_corpus -- --nocapture            # BM25
//! cargo test -p aden-index --features dense --test eval_corpus -- --nocapture  # + hybrid
//! ```
//!
//! ## Growing this into a published benchmark
//!
//! 1. Drop a real repository's docs (or symbol cards) into a corpus dir.
//! 2. Author a `queries.tsv` of realistic questions → the file that answers each.
//!    Aim for the CQS scale (~200 queries) for a credible number.
//! 3. Run under `--features dense` with the bge model fetched; publish the
//!    BM25-vs-hybrid R@1 / R@5 / MRR the report prints.

use aden_index::Index;
use std::path::{Path, PathBuf};

/// One real-corpus evaluation case: a query and the file stem that should rank top.
struct Case {
    query: String,
    /// Expected `source_path` file stem (e.g. `hybrid-retrieval` for `hybrid-retrieval.adoc`).
    expect_stem: String,
    note: String,
}

/// Resolve the corpus directory: `ADEN_EVAL_CORPUS_DIR` if set, else the committed
/// `tests/corpus` next to this test (via `CARGO_MANIFEST_DIR`).
fn corpus_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("ADEN_EVAL_CORPUS_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("corpus")
}

/// Recursively collect indexable document files under `dir`.
fn collect_docs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_docs(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("adoc") | Some("txt") | Some("aden")
        ) {
            out.push(path);
        }
    }
}

/// Load the corpus as `(path, text)` pairs the index can ingest.
fn load_corpus(docs_dir: &Path) -> Vec<(PathBuf, String)> {
    let mut paths = Vec::new();
    collect_docs(docs_dir, &mut paths);
    paths.sort(); // deterministic ingest order
    paths
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|t| (p, t)))
        .collect()
}

/// Parse `queries.tsv`: `query <TAB> expected_stem [<TAB> note]`, `#` comments skipped.
fn load_queries(path: &Path) -> Vec<Case> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|line| {
            let mut cols = line.split('\t').map(str::trim);
            let query = cols.next()?.to_string();
            let expect_stem = cols.next()?.to_string();
            if query.is_empty() || expect_stem.is_empty() {
                return None;
            }
            let note = cols.next().unwrap_or("").to_string();
            Some(Case {
                query,
                expect_stem,
                note,
            })
        })
        .collect()
}

/// 1-based rank of the first result whose source-file stem equals `stem`.
fn rank_of_stem(results: &[aden_index::SearchResult], stem: &str) -> Option<usize> {
    results
        .iter()
        .position(|r| {
            r.source_path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case(stem))
                .unwrap_or(false)
        })
        .map(|i| i + 1)
}

#[derive(Clone, Copy)]
struct Metrics {
    recall_at_1: f64,
    recall_at_5: f64,
    mrr: f64,
    total: usize,
    top1_hits: usize,
}

fn metrics_from_ranks(ranks: &[Option<usize>]) -> Metrics {
    let total = ranks.len().max(1);
    let mut top1 = 0;
    let mut top5 = 0;
    let mut rr = 0.0;
    for r in ranks.iter().flatten() {
        if *r == 1 {
            top1 += 1;
        }
        if *r <= 5 {
            top5 += 1;
        }
        rr += 1.0 / *r as f64;
    }
    Metrics {
        recall_at_1: top1 as f64 / total as f64,
        recall_at_5: top5 as f64 / total as f64,
        mrr: rr / total as f64,
        total: ranks.len(),
        top1_hits: top1,
    }
}

/// Load corpus + queries, or `None` (with a printed skip notice) if absent.
fn load_eval() -> Option<(Index, Vec<Case>)> {
    let dir = corpus_dir();
    let docs = load_corpus(&dir.join("docs"));
    let cases = load_queries(&dir.join("queries.tsv"));
    if docs.is_empty() || cases.is_empty() {
        eprintln!(
            "SKIP: no corpus eval set at {} (docs={}, queries={}). \
             Set ADEN_EVAL_CORPUS_DIR or populate tests/corpus/.",
            dir.display(),
            docs.len(),
            cases.len()
        );
        return None;
    }
    let mut index = Index::default();
    index.ingest(docs);
    index.finalize();
    Some((index, cases))
}

/// BM25 retrieval over the real corpus. Always runs (skips only if no corpus is
/// present), so the numbers are recorded on every build, and asserts a modest
/// regression floor.
#[test]
fn corpus_retrieval_report() {
    let Some((index, cases)) = load_eval() else {
        return;
    };

    let ranks: Vec<Option<usize>> = cases
        .iter()
        .map(|c| rank_of_stem(&index.query(&c.query), &c.expect_stem))
        .collect();
    let m = metrics_from_ranks(&ranks);

    println!("\n=== Real-corpus eval — BM25 ({} queries) ===", m.total);
    for (c, rank) in cases.iter().zip(&ranks) {
        let (mark, rank_str) = match rank {
            Some(1) => ("OK  ", "rank 1".to_string()),
            Some(r) if *r <= 5 => ("top5", format!("rank {r}")),
            Some(r) => ("far ", format!("rank {r}")),
            None => ("MISS", "—".to_string()),
        };
        println!(
            "  [{mark}] {rank_str:<8} q={:?} -> {}  ({})",
            c.query, c.expect_stem, c.note
        );
    }
    println!(
        "  Recall@1 = {:.3} ({}/{})  Recall@5 = {:.3}  MRR = {:.3}",
        m.recall_at_1, m.top1_hits, m.total, m.recall_at_5, m.mrr
    );

    // Conservative regression floor for the committed starter corpus. A real
    // ranker clears this comfortably; it guards against a change that tanks the
    // whole corpus. Tune upward as the corpus grows. (When ADEN_EVAL_CORPUS_DIR
    // points elsewhere the floor still applies — relax it there if needed.)
    assert!(
        m.recall_at_5 >= 0.80,
        "Recall@5 regressed below 0.80 on the real corpus: {:.3}",
        m.recall_at_5
    );
    assert!(
        m.mrr >= 0.60,
        "MRR regressed below 0.60 on the real corpus: {:.3}",
        m.mrr
    );
}

/// BM25-vs-hybrid on the real corpus, using the actual bge-small embedder. This is
/// the report whose numbers are worth publishing. Runs only with `--features dense`
/// and when the model is present; otherwise prints a skip notice.
#[cfg(feature = "dense")]
#[test]
fn corpus_hybrid_report() {
    use aden_index::TractEmbedder;

    let Some((mut index, cases)) = load_eval() else {
        return;
    };

    let model_dir = std::env::var("ADEN_BGE_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(
                dirs::home_dir()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            )
            .join(".cache/aden-models/bge-small-en-v1.5")
        });
    if !model_dir.join("model.onnx").exists() {
        eprintln!(
            "SKIP: bge model not found (set ADEN_BGE_MODEL_DIR); skipping hybrid corpus eval"
        );
        return;
    }
    let embedder = TractEmbedder::from_dir(&model_dir).expect("load bge model");
    index.embed_documents(&embedder);

    let mut bm25_ranks = Vec::with_capacity(cases.len());
    let mut hybrid_ranks = Vec::with_capacity(cases.len());

    println!(
        "\n=== Real-corpus eval — BM25 vs HYBRID (real bge model, {} queries) ===",
        cases.len()
    );
    for c in &cases {
        let b = rank_of_stem(&index.query(&c.query), &c.expect_stem);
        let h = rank_of_stem(&index.hybrid_query(&c.query, &embedder), &c.expect_stem);
        let flag = match (b, h) {
            (b, Some(1)) if b != Some(1) => " <- hybrid wins",
            (Some(1), h) if h != Some(1) => " <- hybrid REGRESSED",
            _ => "",
        };
        println!("  bm25={b:<6?} hybrid={h:<6?} -> {}{flag}", c.expect_stem);
        bm25_ranks.push(b);
        hybrid_ranks.push(h);
    }

    let bm25 = metrics_from_ranks(&bm25_ranks);
    let hybrid = metrics_from_ranks(&hybrid_ranks);
    println!(
        "  R@1: bm25 {:.3} -> hybrid {:.3}   R@5: bm25 {:.3} -> hybrid {:.3}   MRR: bm25 {:.3} -> hybrid {:.3}",
        bm25.recall_at_1,
        hybrid.recall_at_1,
        bm25.recall_at_5,
        hybrid.recall_at_5,
        bm25.mrr,
        hybrid.mrr,
    );

    // Hybrid must not regress top-1 recall versus BM25 on the real corpus.
    assert!(
        hybrid.top1_hits >= bm25.top1_hits,
        "hybrid R@1 regressed below BM25 on the real corpus: {} < {}",
        hybrid.top1_hits,
        bm25.top1_hits
    );
}
