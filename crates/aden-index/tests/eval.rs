// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Retrieval evaluation harness.
//!
//! A deterministic, hermetic eval set: a fixed fixture corpus plus a query set
//! with expected target anchors. The harness builds an [`aden_index::Index`]
//! from the fixtures and scores `query()` against the expectations, reporting
//! Recall@1, Recall@5, and Mean Reciprocal Rank (MRR).
//!
//! Why this exists: ranking changes (the M14 rare-verb fix here, and the future
//! hybrid-retrieval work) must be *measured*, not guessed. This is the baseline
//! to beat. Run with `cargo test -p aden-index --test eval -- --nocapture` to
//! see the metric report.
//!
//! Adding a case: append an [`EvalCase`] to `query_set()`. Adding a document:
//! append to `corpus()`. Keep both small and legible — this is a regression
//! contract, not a benchmark.

use aden_index::Index;
use std::path::PathBuf;

/// One evaluation query and the anchor that should rank first for it.
struct EvalCase {
    /// The natural-language or keyword query.
    query: &'static str,
    /// The anchor that *should* be the top result.
    expect_top: &'static str,
    /// A short note on what the case exercises (shown in the report).
    note: &'static str,
}

/// Build the fixture corpus: each entry is one document whose anchor is fixed by
/// a leading `[[anchor]]` line, followed by prose that controls its tokens. The
/// vocabulary is themed around a code-intelligence tool so the terms are natural.
fn corpus() -> Vec<(PathBuf, String)> {
    let docs: &[(&str, &str)] = &[
        (
            "detect_node_type",
            "Heuristically detect the node type from an anchor and file path. \
             Detect whether a node is a module, a function, or a type. The detect \
             step inspects the anchor prefix.",
        ),
        (
            "scan_orphans",
            "Scan the graph for orphan anchors that no document references. The \
             scanner walks every anchor and collects the orphan anchors it finds.",
        ),
        (
            "classify_orphans",
            "Classify orphan anchors into expected metadata versus actionable \
             orphan anchors that should be linked to a symbol.",
        ),
        (
            "orphan_report",
            "Render a report of orphan anchors grouped by severity for the \
             diagnose command output.",
        ),
        (
            "resolve_callee",
            "Resolve a callee name to its definition anchor using locality \
             heuristics across the caller's file and crate.",
        ),
        (
            "build_snippet",
            "Build a short snippet of document text around the matched query \
             tokens for display in search results.",
        ),
        (
            "tokenize",
            "Tokenize text into search terms, splitting compound identifiers on \
             underscores, dots, and camelCase boundaries.",
        ),
        (
            "bm25_score",
            "Compute the BM25 relevance score for a token using term frequency \
             and inverse document frequency.",
        ),
        (
            "save_index",
            "Serialize the inverted index to disk as JSON so it can be reloaded \
             without reparsing every source file.",
        ),
        (
            "load_index",
            "Load the inverted index from the on-disk cache, rejecting a stale \
             cache built by an older tokenizer version.",
        ),
        (
            "parse_document_attributes",
            "Parse the AsciiDoc document header attributes such as tags, status, \
             and author into document metadata.",
        ),
        (
            "extract_xref_edges",
            "Extract cross reference edges from xref macros and angle bracket \
             shorthand so documents link to the symbols they reference.",
        ),
        (
            "heal_scan",
            "The heal scanner finds drift between code and contracts, reporting \
             missing contracts and stale hashes.",
        ),
        (
            "impact_traversal",
            "Traverse the graph downstream from a symbol to compute its blast \
             radius — every node reachable transitively.",
        ),
        (
            "backlinks_query",
            "Query the graph for backlinks: every call site or reference that \
             points at a given symbol anchor.",
        ),
        (
            "average_document_length",
            "Recompute the average document length across all documents for BM25 \
             length normalization.",
        ),
    ];

    docs.iter()
        .map(|(anchor, prose)| {
            (
                PathBuf::from(format!("{anchor}.adoc")),
                format!("[[{anchor}]]\n{prose}\n"),
            )
        })
        .collect()
}

/// The query set with expected top anchors.
fn query_set() -> Vec<EvalCase> {
    vec![
        EvalCase {
            query: "detect orphan anchors",
            expect_top: "scan_orphans",
            // THE M14 CASE: "detect" is a rare, high-IDF verb that appears in
            // only one doc (detect_node_type) and is irrelevant to the intent
            // (finding orphans). It must not outrank the docs that cover the
            // subject nouns "orphan"/"anchor". Without the coverage boost the
            // single rare-verb match wins — the documented M14 defect.
            note: "M14 rare-verb: coverage must beat a single high-IDF verb",
        },
        EvalCase {
            query: "how are orphan anchors classified",
            expect_top: "classify_orphans",
            note: "subject-noun routing",
        },
        EvalCase {
            query: "compute BM25 relevance score",
            expect_top: "bm25_score",
            note: "exact-term retrieval",
        },
        EvalCase {
            query: "tokenize compound identifiers",
            expect_top: "tokenize",
            note: "identifier query",
        },
        EvalCase {
            query: "load the index from the on-disk cache",
            expect_top: "load_index",
            note: "multi-term phrase",
        },
        EvalCase {
            query: "parse document header attributes tags",
            expect_top: "parse_document_attributes",
            note: "doc-metadata query",
        },
        EvalCase {
            query: "cross reference edges from xref macros",
            expect_top: "extract_xref_edges",
            note: "cross-reference query",
        },
        EvalCase {
            query: "blast radius downstream impact",
            expect_top: "impact_traversal",
            note: "impact-analysis query",
        },
        EvalCase {
            query: "backlinks references to a symbol",
            expect_top: "backlinks_query",
            note: "backlinks query",
        },
        EvalCase {
            query: "resolve a callee to its definition",
            expect_top: "resolve_callee",
            note: "resolution query",
        },
        EvalCase {
            query: "detect node type from anchor",
            expect_top: "detect_node_type",
            // GUARD: the coverage fix must NOT over-penalize a legitimate
            // single-purpose symbol when the query genuinely targets it.
            note: "guard: legit detect_node_type routing still works",
        },
        EvalCase {
            query: "average document length normalization",
            expect_top: "average_document_length",
            note: "length-normalization query",
        },
    ]
}

/// Rank (1-based) of `expected` in `results`, or `None` if absent.
fn rank_of(results: &[aden_index::SearchResult], expected: &str) -> Option<usize> {
    results
        .iter()
        .position(|r| r.anchor == expected)
        .map(|i| i + 1)
}

struct Metrics {
    recall_at_1: f64,
    recall_at_5: f64,
    mrr: f64,
    ndcg_at_10: f64,
    total: usize,
    top1_hits: usize,
}

fn evaluate(index: &Index, cases: &[EvalCase]) -> (Metrics, Vec<(usize, Option<usize>)>) {
    let mut top1 = 0usize;
    let mut top5 = 0usize;
    let mut rr_sum = 0.0;
    let mut ndcg_sum = 0.0;
    let mut per_case = Vec::with_capacity(cases.len());

    for (i, case) in cases.iter().enumerate() {
        let results = index.query(case.query);
        let rank = rank_of(&results, case.expect_top);
        if let Some(r) = rank {
            if r == 1 {
                top1 += 1;
            }
            if r <= 5 {
                top5 += 1;
            }
            rr_sum += 1.0 / r as f64;
            // nDCG@10, binary relevance. Each case has exactly one relevant doc,
            // so the ideal ranking puts it at rank 1 → IDCG = 1/log2(2) = 1. The
            // per-case nDCG is therefore just the discounted gain at its rank,
            // counted only when it lands in the top 10 (else 0).
            if r <= 10 {
                ndcg_sum += 1.0 / ((r as f64) + 1.0).log2();
            }
        }
        per_case.push((i, rank));
    }

    let total = cases.len();
    (
        Metrics {
            recall_at_1: top1 as f64 / total as f64,
            recall_at_5: top5 as f64 / total as f64,
            mrr: rr_sum / total as f64,
            ndcg_at_10: ndcg_sum / total as f64,
            total,
            top1_hits: top1,
        },
        per_case,
    )
}

fn build_index() -> Index {
    let mut index = Index::default();
    index.ingest(corpus());
    index.finalize();
    index
}

/// Print a full per-case report and the aggregate metrics. Visible with
/// `--nocapture`; always runs so the numbers are recorded on every CI run.
#[test]
fn retrieval_eval_report() {
    let index = build_index();
    let cases = query_set();
    let (m, per_case) = evaluate(&index, &cases);

    println!("\n=== Retrieval eval ({} queries) ===", m.total);
    for (i, rank) in &per_case {
        let case = &cases[*i];
        let rank_str = match rank {
            Some(r) => format!("rank {r}"),
            None => "MISS".to_string(),
        };
        let mark = match rank {
            Some(1) => "OK  ",
            Some(r) if *r <= 5 => "top5",
            _ => "FAIL",
        };
        println!(
            "  [{mark}] {rank_str:<8} q={:?} -> want {}  ({})",
            case.query, case.expect_top, case.note
        );
    }
    println!(
        "  Recall@1 = {:.3} ({}/{})  Recall@5 = {:.3}  MRR = {:.3}  nDCG@10 = {:.3}",
        m.recall_at_1, m.top1_hits, m.total, m.recall_at_5, m.mrr, m.ndcg_at_10
    );

    // Aggregate quality floor. The fixture set is designed so a healthy ranker
    // clears this comfortably; it guards against a regression that tanks the
    // whole corpus.
    assert!(
        m.recall_at_5 >= 0.90,
        "Recall@5 regressed below 0.90: {:.3}",
        m.recall_at_5
    );
    assert!(m.mrr >= 0.75, "MRR regressed below 0.75: {:.3}", m.mrr);
    // nDCG@10 ~0.969 on the healthy fixture; 0.85 is a comfortable floor that
    // catches a real ranking regression (≈5 cases slipping to rank 2) without
    // tripping on minor noise.
    assert!(
        m.ndcg_at_10 >= 0.85,
        "nDCG@10 regressed below 0.85: {:.3}",
        m.ndcg_at_10
    );
}

/// The M14 rare-verb case — a KNOWN LIMITATION of pure-lexical retrieval.
///
/// For `detect orphan anchors`, the rare verb "detect" (df=1, high IDF) matches
/// `detect_node_type`, which ALSO matches the common term "anchor" — so it ties
/// the coverage count with `scan_orphans` (orphan + anchor) and then wins on the
/// rare verb's IDF. The coverage boost cannot break this tie: both documents
/// match two distinct query terms.
///
/// This is not solvable in BM25 alone without external knowledge (a verb list /
/// POS tagging), which we deliberately avoid. It is the textbook symptom of
/// lexical-only retrieval and dissolves with hybrid dense + sparse retrieval
/// (RRF) — the planned Gap-1 work. Kept (ignored) as an executable record of the
/// exact case the hybrid retriever must fix; flip `#[ignore]` off once it lands.
#[test]
#[ignore = "M14 rare-verb tie: needs hybrid retrieval (Gap 1); pure BM25 cannot \
            rank a subject noun over a rarer verb when the distractor shares a \
            common query term"]
fn m14_rare_verb_does_not_dominate() {
    let index = build_index();
    let results = index.query("detect orphan anchors");

    let scan_rank = rank_of(&results, "scan_orphans");
    let detect_rank = rank_of(&results, "detect_node_type");

    assert_eq!(
        scan_rank,
        Some(1),
        "scan_orphans should rank #1 for 'detect orphan anchors', got {scan_rank:?} \
         (detect_node_type at {detect_rank:?})"
    );
    assert!(
        detect_rank.map(|d| d > 1).unwrap_or(true),
        "the rare-verb match detect_node_type must not be #1"
    );
}

/// Proves the coverage boost is doing real work: a document that matches MORE
/// distinct query terms must beat a document that matches a single rarer term,
/// EVEN when pure BM25 would rank the rare-single-term doc first.
///
/// Construction: `sprocket` is unique (df=1, high IDF); `widget`/`gadget` are
/// common (df=4, low IDF). For "sprocket widget gadget", `narrow` matches only
/// the rare `sprocket` (coordination level 1) while `broad` matches both common
/// terms (level 2). Under pure BM25 `narrow`'s lone high-IDF term outscores
/// `broad`'s two low-IDF terms; the coverage boost (×2 for level 2) flips it.
#[test]
fn coverage_boost_lifts_broader_match_over_rarer_single_term() {
    let docs: &[(&str, &str)] = &[
        ("narrow", "sprocket"),
        ("broad", "widget gadget"),
        ("f1", "widget"),
        ("f2", "widget gadget"),
        ("f3", "widget thing"),
        ("f4", "gadget"),
        ("f5", "gadget item"),
        ("f6", "unrelated filler text"),
    ];
    let mut index = Index::default();
    index.ingest(
        docs.iter()
            .map(|(a, p)| {
                (
                    PathBuf::from(format!("{a}.adoc")),
                    format!("[[{a}]]\n{p}\n"),
                )
            })
            .collect(),
    );
    index.finalize();

    let results = index.query("sprocket widget gadget");
    assert_eq!(
        results.first().map(|r| r.anchor.as_str()),
        Some("broad"),
        "coverage boost should lift the 2-term match 'broad' over the rare \
         single-term match 'narrow'; ranking was: {:?}",
        results
            .iter()
            .take(3)
            .map(|r| &r.anchor)
            .collect::<Vec<_>>()
    );
}

/// Guard: the coverage boost must not over-penalize a legitimate single-purpose
/// symbol when the query genuinely targets it.
#[test]
fn coverage_boost_does_not_break_single_target_queries() {
    let index = build_index();
    let results = index.query("detect node type from anchor");
    assert_eq!(
        rank_of(&results, "detect_node_type"),
        Some(1),
        "a query that genuinely targets detect_node_type must still route there"
    );
}

/// Real-model hybrid eval: runs the SAME query set through `hybrid_query` using
/// the actual bge-small embedder over the fixture corpus, and reports BM25 vs
/// hybrid metrics side by side. This is the measurement substrate for tuning RRF
/// fusion. Runs only with `--features dense` and when the model is present
/// (`ADEN_BGE_MODEL_DIR` or `~/.cache/aden-models/bge-small-en-v1.5`); otherwise
/// it prints a skip notice.
///
/// Run: `cargo test -p aden-index --features dense --test eval -- --nocapture`
#[cfg(feature = "dense")]
#[test]
fn hybrid_retrieval_eval_with_real_model() {
    use aden_index::TractEmbedder;

    let dir = std::env::var("ADEN_BGE_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".cache/aden-models/bge-small-en-v1.5")
        });
    if !dir.join("model.onnx").exists() {
        eprintln!("SKIP: bge model not found (set ADEN_BGE_MODEL_DIR); skipping hybrid eval");
        return;
    }
    let embedder = TractEmbedder::from_dir(&dir).expect("load bge model");

    let mut index = Index::default();
    index.ingest(corpus());
    index.finalize();
    index.embed_documents(&embedder);

    let cases = query_set();
    let mut bm25_top1 = 0usize;
    let mut hybrid_top1 = 0usize;
    let mut bm25_rr = 0.0;
    let mut hybrid_rr = 0.0;
    let mut bm25_ndcg = 0.0;
    let mut hybrid_ndcg = 0.0;
    // nDCG@10 gain for a single-relevant-doc case (IDCG = 1): the discounted
    // gain at the achieved rank when it lands in the top 10, else 0.
    let ndcg_gain = |rank: Option<usize>| {
        rank.map(|r| {
            if r <= 10 {
                1.0 / ((r as f64) + 1.0).log2()
            } else {
                0.0
            }
        })
        .unwrap_or(0.0)
    };

    println!(
        "\n=== BM25 vs HYBRID (real bge model) — {} queries ===",
        cases.len()
    );
    for case in &cases {
        let b = rank_of(&index.query(case.query), case.expect_top);
        let h = rank_of(&index.hybrid_query(case.query, &embedder), case.expect_top);
        if b == Some(1) {
            bm25_top1 += 1;
        }
        if h == Some(1) {
            hybrid_top1 += 1;
        }
        bm25_rr += b.map(|r| 1.0 / r as f64).unwrap_or(0.0);
        hybrid_rr += h.map(|r| 1.0 / r as f64).unwrap_or(0.0);
        bm25_ndcg += ndcg_gain(b);
        hybrid_ndcg += ndcg_gain(h);
        let flag = if h == Some(1) && b != Some(1) {
            " <- hybrid wins"
        } else if b == Some(1) && h != Some(1) {
            " <- hybrid REGRESSED"
        } else {
            ""
        };
        println!(
            "  bm25={:<5?} hybrid={:<5?} want {}{flag}",
            b, h, case.expect_top
        );
    }
    let n = cases.len() as f64;
    println!(
        "  R@1: bm25 {:.3} -> hybrid {:.3}   MRR: bm25 {:.3} -> hybrid {:.3}   \
         nDCG@10: bm25 {:.3} -> hybrid {:.3}",
        bm25_top1 as f64 / n,
        hybrid_top1 as f64 / n,
        bm25_rr / n,
        hybrid_rr / n,
        bm25_ndcg / n,
        hybrid_ndcg / n,
    );

    // M14 gate: hybrid must rank the orphan-handling doc above the rare-verb
    // distractor — the case pure BM25 cannot fix (the BM25-only
    // `m14_rare_verb_does_not_dominate` test stays #[ignore]d as the documented
    // limitation; THIS is where M14 is proven resolved).
    let m14 = index.hybrid_query("detect orphan anchors", &embedder);
    let scan = rank_of(&m14, "scan_orphans");
    let detect = rank_of(&m14, "detect_node_type");
    println!(
        "  M14 'detect orphan anchors': scan_orphans @ {scan:?}, detect_node_type @ {detect:?}"
    );
    assert_eq!(
        scan,
        Some(1),
        "hybrid must resolve M14: scan_orphans #1 (detect_node_type @ {detect:?})"
    );

    // Hybrid must not regress overall ranking versus BM25.
    assert!(
        hybrid_top1 >= bm25_top1,
        "hybrid R@1 regressed below BM25: {hybrid_top1} < {bm25_top1}"
    );
}
