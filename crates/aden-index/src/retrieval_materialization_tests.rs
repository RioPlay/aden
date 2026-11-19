// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

// Frozen eager implementation from the pre-optimization source. This reference
// deliberately retains allocation and snippet work before fusion so equivalence
// and timing compare the same corpus, provider, complete rankings, and outputs.
impl Index {
    fn eager_query(&self, query_str: &str) -> Vec<SearchResult> {
        // First, extract all tokens including stop words for semantic normalization
        let raw_tokens: Vec<String> = query_str
            .split_whitespace()
            .map(|w| {
                w.trim_matches(|c: char| c.is_ascii_punctuation())
                    .to_lowercase()
            })
            .filter(|w| !w.is_empty())
            .collect();

        if raw_tokens.is_empty() {
            return Vec::new();
        }

        // Apply semantic normalization to get all forms (May -> 5, May -> may, etc.)
        // This runs BEFORE stop word filtering so "May" -> "5" isn't lost
        let mut all_query_tokens: Vec<String> = Vec::new();
        for token in &raw_tokens {
            let normalized = SemanticNormalizer::normalize(token);
            for form in normalized {
                if !all_query_tokens.contains(&form) {
                    all_query_tokens.push(form);
                }
            }
        }

        // Filter stop words from the expanded token set, then stem as the final
        // step — mirroring `tokenize` on the index side — so query terms match the
        // stemmed postings. Stemming runs AFTER `SemanticNormalizer` so forms like
        // "5"/"05" are produced first and then passed through untouched by `stem`
        // (its alphabetic-only guard).
        let mut tokens: Vec<String> = Vec::new();
        for t in all_query_tokens {
            if is_stop_word(&t) {
                continue;
            }
            let stemmed = stem(&t);
            // Dedupe: distinct normalized forms can stem to the same token; scoring
            // a token twice would inflate its BM25 contribution.
            if !tokens.contains(&stemmed) {
                tokens.push(stemmed);
            }
        }

        if tokens.is_empty() {
            return Vec::new();
        }

        let n = self.inverted.values().map(|v| v.len()).sum::<usize>() as f64;
        if n == 0.0 {
            return Vec::new();
        }

        let mut scores: HashMap<DocumentId, f64> = HashMap::new();
        // document ID -> number of DISTINCT query tokens matched.
        let mut coverage: HashMap<DocumentId, usize> = HashMap::new();

        // Phase 1: Direct BM25 scoring (already includes normalized forms)
        for token in &tokens {
            self.add_bm25_scores(token, n, &mut scores, &mut coverage);
        }

        // M14 fix — multi-token coverage boost. Pure BM25 sums per-term scores,
        // so a single RARE term (high IDF) can outrank a document that matches
        // MORE of the query with common terms. Example: "detect orphan anchors"
        // routed to `detect_node_type` (matches only the rare verb "detect")
        // over `scan_orphans` (matches the subject nouns orphan + anchor). This
        // is the classic "coordination level" signal from IR: reward a document
        // for covering more DISTINCT query terms. The boost is multiplicative and
        // gentle — a doc matching k distinct terms is scaled by
        // 1 + COVERAGE_WEIGHT*(k-1), so a single-term match is unchanged (no
        // penalty to genuine single-target queries) while broader coverage wins
        // ties against a lone rare term. Deterministic; validated by the
        // retrieval eval harness (`crates/aden-index/tests/eval.rs`).
        if tokens.len() > 1 {
            for (doc_id, score) in scores.iter_mut() {
                let matched = coverage.get(doc_id).copied().unwrap_or(1);
                if matched > 1 {
                    *score *= 1.0 + COVERAGE_WEIGHT * (matched - 1) as f64;
                }
            }
        }

        // Adjacent multi-term matches break a remaining coverage tie. A query
        // like "detect orphan anchors" contains the meaningful phrase "orphan
        // anchors"; a document that preserves that phrase has stronger lexical
        // evidence than one that merely mentions both words elsewhere. This is
        // deliberately applied after BM25 and coverage, so it cannot create a
        // result for a document without normal term matches.
        if tokens.len() > 1 {
            for (doc_id, score) in scores.iter_mut() {
                if self
                    .document(*doc_id)
                    .is_some_and(|document| Self::contains_query_bigram(&document.text, &tokens))
                {
                    *score *= PHRASE_BOOST;
                }
            }
        }

        // Apply title/anchor boosts
        let query_lower = query_str.to_lowercase();
        // Stemmed, stop-word-filtered query tokens for the ranking boosts, so the
        // anchor/title boosts key off the SAME normalization as BM25. Without this,
        // the plural "overlays" would fail to boost an "overlay-delivery" anchor
        // even though BM25 already matches the stemmed "overlay". Computed once.
        let significant_query_tokens: Vec<String> = query_lower
            .split_whitespace()
            .map(|term| term.trim_matches(|ch: char| ch.is_ascii_punctuation()))
            .filter(|term| !term.is_empty() && !is_stop_word(term))
            .map(stem)
            .collect();
        for (doc_id, score) in scores.iter_mut() {
            let Some(document) = self.document(*doc_id) else {
                continue;
            };
            let anchor = &document.anchor;
            let source_path = Some(&document.source_path);

            // Penalize .agent/ templates - they're for AI agents, not human-facing docs
            if let Some(path) = source_path {
                let path_str = path.to_string_lossy().to_lowercase();
                if path_str.contains(".agent/") || path_str.contains(".agent\\") {
                    *score *= 0.01; // 99% penalty — almost exclude from search results
                }
                // Down-weight reference / vendored material: it's legitimately
                // searchable but should not outrank the project's own code/docs for
                // "how does X work" questions. A CWE catalog under research/ (many
                // "[rank N]" headings) was hijacking queries like "search ranking".
                let p = path_str.replace('\\', "/");
                if p.contains("research/")
                    || p.contains("vendor/")
                    || p.contains("third_party/")
                    || p.contains("third-party/")
                    || p.contains("node_modules/")
                {
                    *score *= 0.1; // 90% penalty — keep findable, but below project content
                }
            }

            // Symbol anchors (any anchor containing '#') are concrete function/struct/enum
            // definitions. They are the highest-value result for specific questions.
            // Previously these were penalised by 90% which made them nearly invisible.
            // Now: no penalty. They compete on raw BM25 score, and the query router's
            // explicit symbol-name detection in resolve_anchor_fuzzy() handles routing.
            let is_symbol = anchor.contains('#');

            // Boost: query term appears literally in the anchor string itself.
            // For symbol anchors only boost on the fragment (#name) part so
            // "aden://module/foo/bar.rs#assemble" boosts when query contains "assemble".
            // For non-symbol anchors boost on the full anchor slug.
            // Only non-stop-word query tokens may trigger the anchor-match boost.
            // Without this guard, a query like "How does output get written?" boosts
            // `get_current_git_ref` 30x because the fragment contains "get".
            // Tokens are stemmed (see `significant_query_tokens`), so inflected
            // query forms converge with the anchor/title tokens. Exact token or
            // sub-token equality prevents `run` from boosting `runtime` while
            // preserving camelCase humps such as `fetchRunState`.
            let anchor_match = if is_symbol {
                // Match on the symbol name fragment only, with significant tokens only.
                if let Some(fragment) = anchor.rsplit('#').next() {
                    token_boundary_match(fragment, &significant_query_tokens)
                } else {
                    false
                }
            } else {
                token_boundary_match(anchor, &significant_query_tokens)
            };

            // Title match (first line of document text). Uses the same stemmed,
            // stop-word-filtered tokens and the same token-boundary rule so the
            // title boost is consistent with the anchor boost and BM25.
            let title_match = document
                .text
                .lines()
                .next()
                .map(|first_line| token_boundary_match(first_line, &significant_query_tokens))
                .unwrap_or(false);

            // Cap the combined boost. The anchor boost (30x symbol / 20x slug) and
            // title boost (10x) were previously multiplied unconditionally → up to
            // 300x, letting a weak BM25 hit matching both swamp a strong hit. Apply
            // only the single largest applicable boost instead of stacking.
            let anchor_boost: f64 = if anchor_match {
                if is_symbol { 30.0 } else { 20.0 }
            } else {
                1.0
            };
            let title_boost: f64 = if title_match { 10.0 } else { 1.0 };
            *score *= anchor_boost.max(title_boost);
        }

        let mut results: Vec<SearchResult> = scores
            .into_iter()
            .filter_map(|(doc_id, score)| {
                let document = self.document(doc_id)?;
                let snippet = build_snippet(&document.text, &tokens);
                Some(SearchResult {
                    anchor: document.anchor.clone(),
                    source_path: document.source_path.clone(),
                    score,
                    snippet,
                })
            })
            .collect();

        // Sort by descending score, then by anchor name as a deterministic
        // tiebreak. Without the secondary key, equal BM25 scores inherit the
        // arbitrary `HashMap` iteration order, which made `ask` routing — and the
        // top-K primary/alternate split that consumes this order — flip
        // run-to-run on ties. Lexicographic tiebreak makes the whole pipeline
        // stable for free.
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.anchor.cmp(&b.anchor))
        });

        // If no results or weak results (score < 1.0), try fuzzy search
        if results.is_empty() || results.first().map(|r| r.score < 1.0).unwrap_or(true) {
            let fuzzy = self.eager_fuzzy_query(&tokens);
            if !fuzzy.is_empty() {
                if results.is_empty() {
                    return fuzzy;
                }
                // Append fuzzy results to main results
                results.extend(fuzzy);
            }
        }

        results
    }

    fn eager_fuzzy_query(&self, tokens: &[String]) -> Vec<SearchResult> {
        if tokens.is_empty() {
            return Vec::new();
        }

        let mut fuzzy_matches: Vec<(String, f64)> = Vec::new();
        let query_term = &tokens[0].to_lowercase();

        for document in &self.documents {
            let anchor = &document.anchor;
            let anchor_lower = anchor.to_lowercase();
            // Match either against the whole anchor or its symbol-fragment / last
            // path segment, whichever is closer — a small edit distance on a long
            // slug would otherwise never fire.
            let candidate = anchor_lower
                .rsplit(['#', '/'])
                .next()
                .unwrap_or(&anchor_lower);
            let dist = levenshtein_distance(query_term, candidate)
                .min(levenshtein_distance(query_term, &anchor_lower));
            // Only a genuine near-miss qualifies. The old order-agnostic
            // "all query chars appear somewhere" path is dropped: it matched any
            // anchor sharing a character set (e.g. "io" matched anything with an
            // i and an o) and flat-scored 1.0, swamping exact hits.
            if dist <= 2 {
                // Score strictly below an exact match (which scores >= 1.0 via
                // BM25 + boosts): closer fuzzy hits rank higher, but all stay
                // under 1.0 so a real match always wins. dist 0 → 0.9, 1 → 0.45,
                // 2 → 0.3.
                let score = 0.9 / (dist as f64 + 1.0);
                fuzzy_matches.push((anchor.clone(), score));
            }
        }

        // Sort by descending fuzzy score, then anchor-name tiebreak for a stable
        // order independent of the arbitrary key iteration order. (The tiebreak
        // also avoids a NaN `unwrap` panic.)
        fuzzy_matches.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        fuzzy_matches
            .into_iter()
            .filter_map(|(anchor, score)| {
                let document = self.document_by_anchor(&anchor)?;
                let snippet = build_snippet(&document.text, tokens);
                Some(SearchResult {
                    anchor,
                    source_path: document.source_path.clone(),
                    score,
                    snippet,
                })
            })
            .collect()
    }

    fn eager_dense_query(
        &self,
        query: &str,
        provider: &dyn EmbeddingProvider,
    ) -> Vec<SearchResult> {
        if self.embeddings.is_empty() {
            return Vec::new();
        }
        let q = provider.embed(query);
        let query_tokens = tokenize(query);
        let mut scored: Vec<(String, f32)> = self
            .embeddings
            .iter()
            .map(|(anchor, vec)| (anchor.clone(), cosine_similarity(&q, vec)))
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored
            .into_iter()
            .filter_map(|(anchor, score)| {
                let document = self.document_by_anchor(&anchor)?;
                let snippet = build_snippet(&document.text, &query_tokens);
                Some(SearchResult {
                    anchor,
                    source_path: document.source_path.clone(),
                    score: score as f64,
                    snippet,
                })
            })
            .collect()
    }

    fn eager_hybrid_query(
        &self,
        query: &str,
        provider: &dyn EmbeddingProvider,
    ) -> Vec<SearchResult> {
        let bm25 = self.eager_query(query);
        if self.embeddings.is_empty() {
            return bm25;
        }
        let dense = self.eager_dense_query(query, provider);

        let bm25_ranks: Vec<String> = bm25.iter().map(|r| r.anchor.clone()).collect();
        let dense_ranks: Vec<String> = dense.iter().map(|r| r.anchor.clone()).collect();
        let fused = rrf_fuse(&[bm25_ranks, dense_ranks], RRF_K);

        // Re-attach source path + snippet by anchor; carry the fused RRF score.
        let by_anchor: HashMap<&str, &SearchResult> = bm25
            .iter()
            .chain(dense.iter())
            .map(|r| (r.anchor.as_str(), r))
            .collect();
        fused
            .into_iter()
            .filter_map(|(anchor, score)| {
                let base = by_anchor.get(anchor.as_str())?;
                Some(SearchResult {
                    anchor: anchor.clone(),
                    source_path: base.source_path.clone(),
                    score,
                    snippet: base.snippet.clone(),
                })
            })
            .collect()
    }
}

struct TestProvider;

impl EmbeddingProvider for TestProvider {
    fn embed(&self, text: &str) -> Vec<f32> {
        vec![
            text.matches("parse").count() as f32,
            text.matches("query").count() as f32,
            text.matches("May").count() as f32,
            1.0,
        ]
    }

    fn dim(&self) -> usize {
        4
    }
}

fn fixture(count: usize) -> Index {
    let mut index = Index::default();
    index.ingest(
        (0..count)
            .map(|i| {
                let anchor = format!("entry_{i:04}");
                let topic = match i % 3 {
                    0 => "parse query May calendar",
                    1 => "query ranking retrieval",
                    _ => "unrelated material",
                };
                (
                    PathBuf::from(format!("docs/{anchor}.adoc")),
                    format!(
                        "[[{anchor}]]\n{topic}\n{}",
                        "Context before a later parse query match. ".repeat(24)
                    ),
                )
            })
            .collect(),
    );
    index.finalize();
    index
}

fn assert_equivalent(index: &Index, query: &str) {
    assert_eq!(
        index.query(query),
        index.eager_query(query),
        "lexical query: {query:?}"
    );
    assert_eq!(
        index.dense_query(query, &TestProvider),
        index.eager_dense_query(query, &TestProvider),
        "dense query: {query:?}",
    );
    assert_eq!(
        index.hybrid_query(query, &TestProvider),
        index.eager_hybrid_query(query, &TestProvider),
        "hybrid query: {query:?}",
    );
}

#[test]
fn retrieval_materialization_preserves_complete_results_exactly() {
    let mut index = fixture(127);
    for query in [
        "",
        "the and",
        "parse query",
        "May",
        "entry_0000",
        "entry_000x",
        "missing",
    ] {
        assert_equivalent(&index, query);
    }
    index.embed_documents(&TestProvider);
    for query in [
        "",
        "the and",
        "parse query",
        "May",
        "entry_0000",
        "entry_000x",
        "missing",
    ] {
        assert_equivalent(&index, query);
    }
    // Simulate a partial embedding set and a stale vector left in a cache.
    // Missing documents are excluded before RRF ranks are assigned.
    index.embeddings.retain(|anchor, _| anchor.ends_with('0'));
    index.embeddings.insert("absent".into(), vec![0.0; 4]);
    for query in ["May", "parse query", "entry_000x"] {
        assert_equivalent(&index, query);
    }
    index.embeddings.clear();
    assert_equivalent(&index, "parse query");
}

#[test]
fn retrieval_materialization_preserves_fuzzy_duplicate_rank_contributions() {
    let mut index = Index::default();
    index.ingest(vec![(
        PathBuf::from(".agent/alpha.adoc"),
        format!("[[alpha]]\nalpha {}", "context ".repeat(1000)),
    )]);
    index.finalize();
    // The template penalty makes a lexical hit weak enough to append its fuzzy
    // match too. Do not deduplicate before RRF: both ranks must contribute.
    let lexical = index.eager_query("alpha");
    assert_eq!(lexical.len(), 2);
    assert_eq!(lexical[0].anchor, lexical[1].anchor);
    assert_equivalent(&index, "alpha");
    index.embed_documents(&TestProvider);
    assert_equivalent(&index, "alpha");
}

#[test]
#[ignore = "matched retrieval timing experiment; run explicitly with --nocapture"]
fn retrieval_materialization_matched_benchmark() {
    use std::hint::black_box;
    use std::time::{Duration, Instant};

    let mut index = fixture(2000);
    index.embed_documents(&TestProvider);
    let query = "May parse query";
    assert_equivalent(&index, query);
    let mut eager = Duration::ZERO;
    let mut deferred = Duration::ZERO;
    // Alternate order to reduce warm-cache and scheduling bias. Inference,
    // index construction and correctness checks are outside the timing region.
    for round in 0..20 {
        for optimized in if round % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            let start = Instant::now();
            let results = if optimized {
                index.hybrid_query(black_box(query), &TestProvider)
            } else {
                index.eager_hybrid_query(black_box(query), &TestProvider)
            };
            black_box(&results);
            let elapsed = start.elapsed();
            if optimized {
                deferred += elapsed;
            } else {
                eager += elapsed;
            }
            assert_eq!(results.len(), 2000);
        }
    }
    eprintln!(
        "matched retrieval: docs=2000 queries=20 outputs=all provider=deterministic-4d eager_total_us={} deferred_total_us={} (includes ranking, snippets, allocation; excludes embedding corpus and drop)",
        eager.as_micros(),
        deferred.as_micros(),
    );
}
