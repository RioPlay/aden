// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Tests for the hybrid-retrieval fusion core: cosine similarity, Reciprocal
//! Rank Fusion, and `Index::{embed_documents, dense_query, hybrid_query}`.
//!
//! Scope note: these prove the fusion *mechanics* are correct and deterministic,
//! and that hybrid delivers its signature win — surfacing a semantically related
//! document that shares NO terms with the query (which pure BM25 cannot). They do
//! NOT use a real embedding model; the deterministic `TopicProvider` below stands
//! in for one. Whether a real model actually resolves M14 on real data is
//! measured separately by the retrieval eval harness once the model lands.

use aden_index::{cosine_similarity, rrf_fuse, EmbeddingProvider, Index};
use std::path::PathBuf;

/// A deterministic stand-in embedding provider: a tiny topic model. Each text
/// maps to a fixed-length vector whose components count occurrences of a topic's
/// keywords. Texts about the same concept get similar vectors even when they
/// share no surface tokens — exactly the property a real embedding model
/// provides, and the reason hybrid retrieval beats lexical-only search.
///
/// It is NOT a per-document answer key: it uses a general topic lexicon, so the
/// "semantic rescue" it enables is earned by concept overlap, not hard-coded.
struct TopicProvider;

impl TopicProvider {
    const TOPICS: &'static [&'static [&'static str]] = &[
        // auth
        &["login", "authentication", "auth", "bearer", "session", "credential", "signin", "password"],
        // orphan / cleanup
        &["orphan", "dangling", "unreferenced", "prune", "scan"],
        // parse
        &["parse", "attribute", "header", "syntax", "grammar"],
        // index / search
        &["index", "search", "query", "rank", "score"],
    ];
}

impl EmbeddingProvider for TopicProvider {
    fn embed(&self, text: &str) -> Vec<f32> {
        let lower = text.to_lowercase();
        let words: Vec<&str> = lower
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        Self::TOPICS
            .iter()
            .map(|topic| {
                words
                    .iter()
                    .filter(|w| topic.contains(*w))
                    .count() as f32
            })
            .collect()
    }

    fn dim(&self) -> usize {
        Self::TOPICS.len()
    }
}

fn doc(anchor: &str, prose: &str) -> (PathBuf, String) {
    (
        PathBuf::from(format!("{anchor}.adoc")),
        format!("[[{anchor}]]\n{prose}\n"),
    )
}

fn rank_of(results: &[aden_index::SearchResult], anchor: &str) -> Option<usize> {
    results.iter().position(|r| r.anchor == anchor).map(|i| i + 1)
}

// --- cosine_similarity ---

#[test]
fn cosine_identical_orthogonal_and_mismatch() {
    assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
    assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    assert!((cosine_similarity(&[1.0, 1.0], &[2.0, 2.0]) - 1.0).abs() < 1e-6);
    // length mismatch and zero vectors yield 0.0 (no direction).
    assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0]), 0.0);
    assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
}

// --- rrf_fuse ---

#[test]
fn rrf_fuse_blends_and_is_deterministic() {
    // a: ranks (1, 3); b: ranks (2, 1); c: ranks (3, 2).
    // score(b) = 1/62 + 1/61  (highest — strong in both)
    // score(a) = 1/61 + 1/63
    // score(c) = 1/63 + 1/62
    let fused = rrf_fuse(
        &[
            vec!["a".into(), "b".into(), "c".into()],
            vec!["b".into(), "c".into(), "a".into()],
        ],
        60.0,
    );
    let order: Vec<&str> = fused.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(order[0], "b", "item strong in both lists should win: {order:?}");
    // A doc ranked #1 by only ONE retriever (a is #1 in list one) does not beat a
    // doc ranked well by BOTH (b is #2 and #1) — the whole point of fusion.
    assert!(
        order.iter().position(|x| *x == "b").unwrap()
            < order.iter().position(|x| *x == "a").unwrap()
    );
    // Deterministic: identical inputs → identical output.
    let again = rrf_fuse(
        &[
            vec!["a".into(), "b".into(), "c".into()],
            vec!["b".into(), "c".into(), "a".into()],
        ],
        60.0,
    );
    assert_eq!(fused, again);
}

// --- hybrid_query plumbing ---

fn build_indexed() -> Index {
    let mut index = Index::default();
    index.ingest(vec![
        doc("auth_middleware", "Validate the bearer credential and start a session."),
        doc("parse_header", "Parse the document header syntax and grammar."),
        doc("search_ranker", "Rank and score search query results."),
        doc("prune_dangling", "Prune dangling unreferenced nodes."),
    ]);
    index.finalize();
    index
}

#[test]
fn hybrid_falls_back_to_bm25_without_embeddings() {
    let index = build_indexed(); // embed_documents NOT called
    assert!(!index.has_embeddings());
    let provider = TopicProvider;
    let hybrid = index.hybrid_query("parse document header", &provider);
    let bm25 = index.query("parse document header");
    assert_eq!(
        hybrid.iter().map(|r| &r.anchor).collect::<Vec<_>>(),
        bm25.iter().map(|r| &r.anchor).collect::<Vec<_>>(),
        "with no embeddings, hybrid must equal pure BM25"
    );
}

#[test]
fn hybrid_surfaces_semantic_match_with_no_shared_terms() {
    let mut index = build_indexed();
    let provider = TopicProvider;
    index.embed_documents(&provider);
    assert!(index.has_embeddings());

    // The query shares NO surface tokens with the auth_middleware doc
    // ("bearer credential session") — BM25 cannot connect them.
    let query = "user login authentication";
    let bm25 = index.query(query);
    assert!(
        rank_of(&bm25, "auth_middleware").is_none(),
        "precondition: BM25 alone should not find auth_middleware (no shared terms); got {:?}",
        bm25.iter().map(|r| &r.anchor).collect::<Vec<_>>()
    );

    // Hybrid, via the dense topic signal, surfaces it at the top.
    let hybrid = index.hybrid_query(query, &provider);
    assert_eq!(
        hybrid.first().map(|r| r.anchor.as_str()),
        Some("auth_middleware"),
        "hybrid should surface the semantically-related doc; got {:?}",
        hybrid.iter().map(|r| &r.anchor).collect::<Vec<_>>()
    );
}

#[test]
fn hybrid_query_is_deterministic() {
    let mut index = build_indexed();
    let provider = TopicProvider;
    index.embed_documents(&provider);
    let a = index.hybrid_query("prune orphan nodes", &provider);
    let b = index.hybrid_query("prune orphan nodes", &provider);
    assert_eq!(
        a.iter().map(|r| (&r.anchor, r.score)).collect::<Vec<_>>(),
        b.iter().map(|r| (&r.anchor, r.score)).collect::<Vec<_>>(),
    );
}
