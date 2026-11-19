// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Prepared graph-ranking forms for one immutable snapshot's anchors.
//! The four indexes contain positions, not additional copies of symbol strings.

use super::{AnchorResolution, AnchorResolutionAnalysis, natural_symbol_form};
use std::borrow::Cow;

struct Forms {
    segment: String,
    segment_lower: String,
    leaf_start: usize,
    leaf_lower: String,
}

impl Forms {
    fn new(anchor: &str) -> Self {
        let segment = natural_symbol_form(anchor.rsplit(['#', '/']).next().unwrap_or(""));
        let segment_lower = segment.to_lowercase();
        // Keep graph ranking's ./: separators. The store's PHP-aware core
        // forms have a different contract and cannot replace these forms.
        let leaf = segment.rsplit(['.', ':']).next().unwrap_or(&segment);
        let leaf_start = segment.len() - leaf.len();
        let leaf_lower = leaf.to_lowercase();
        Self {
            segment,
            segment_lower,
            leaf_start,
            leaf_lower,
        }
    }

    fn key(&self, rank: usize) -> &str {
        match rank {
            0 => &self.segment,
            1 => &self.segment_lower,
            2 => &self.segment[self.leaf_start..],
            3 => &self.leaf_lower,
            _ => unreachable!(),
        }
    }

    fn rank(&self, query: &Query) -> u8 {
        for rank in 0..4 {
            if self.key(rank) == query.key(rank) {
                return rank as u8;
            }
        }
        if self.segment_lower.starts_with(&query.dot_prefix)
            || self.segment_lower.starts_with(&query.scope_prefix)
        {
            4
        } else if !query.lower.is_empty() && self.segment_lower.contains(&query.lower) {
            5
        } else {
            u8::MAX
        }
    }
}

struct PreparedForms {
    ranking: Forms,
    segment_chars: usize,
    typo_leaf_chars: usize,
}

impl PreparedForms {
    fn new(anchor: &str) -> Self {
        let ranking = Forms::new(anchor);
        let segment_chars = ranking.segment_lower.chars().count();
        let typo_leaf_chars = ranking
            .segment_lower
            .rsplit(['.', ':'])
            .next()
            .unwrap_or(&ranking.segment_lower)
            .chars()
            .count();
        Self {
            ranking,
            segment_chars,
            typo_leaf_chars,
        }
    }
}

pub(super) struct Query {
    symbol: String,
    lower: String,
    dot_prefix: String,
    scope_prefix: String,
}

impl Query {
    pub(super) fn new(symbol: &str) -> Self {
        let symbol = natural_symbol_form(symbol);
        let lower = symbol.to_lowercase();
        Self {
            dot_prefix: format!("{lower}."),
            scope_prefix: format!("{lower}::"),
            symbol,
            lower,
        }
    }

    pub(super) fn rank_anchor(&self, anchor: &str) -> u8 {
        Forms::new(anchor).rank(self)
    }

    fn key(&self, rank: usize) -> &str {
        if rank == 0 || rank == 2 {
            &self.symbol
        } else {
            &self.lower
        }
    }
}

pub(crate) struct PreparedSymbols {
    forms: Vec<PreparedForms>,
    indexes: [Vec<usize>; 4],
}

impl PreparedSymbols {
    pub(crate) fn exact_anchor(symbol: &str, contains: impl Fn(&str) -> bool) -> Option<String> {
        let canonical = Self::normalize_alias(symbol, &contains);
        contains(&canonical).then(|| canonical.into_owned())
    }

    /// The owning snapshot supplies the same anchor vector on each call.
    pub(crate) fn new(anchors: &[String]) -> Self {
        let forms: Vec<_> = anchors
            .iter()
            .map(|anchor| PreparedForms::new(anchor))
            .collect();
        let indexes = std::array::from_fn(|rank| {
            let mut positions: Vec<_> = (0..anchors.len()).collect();
            positions.sort_unstable_by(|&left, &right| {
                forms[left]
                    .ranking
                    .key(rank)
                    .cmp(forms[right].ranking.key(rank))
                    .then_with(|| anchors[left].cmp(&anchors[right]))
                    .then_with(|| left.cmp(&right))
            });
            positions
        });
        Self { forms, indexes }
    }

    fn normalize_alias<'a>(symbol: &'a str, contains: &impl Fn(&str) -> bool) -> Cow<'a, str> {
        if !symbol.contains("://") && symbol.contains('#') {
            for prefix in ["aden://module/", "aden://doc/"] {
                let canonical = format!("{prefix}{symbol}");
                if contains(&canonical) {
                    return Cow::Owned(canonical);
                }
            }
        }
        Cow::Borrowed(symbol)
    }

    pub(crate) fn resolve(
        &self,
        symbol: &str,
        anchors: &[String],
        contains: impl Fn(&str) -> bool,
    ) -> AnchorResolution {
        let symbol = Self::normalize_alias(symbol, &contains);
        if contains(&symbol) {
            return AnchorResolution::Exact(symbol.into_owned());
        }
        let query = Query::new(&symbol);
        // Earlier ranks must win even if a later index has a more specific
        // looking spelling. A tie range is already in canonical anchor order.
        for rank in 0..4 {
            let positions = &self.indexes[rank];
            let key = query.key(rank);
            let start =
                positions.partition_point(|&index| self.forms[index].ranking.key(rank) < key);
            let end =
                positions.partition_point(|&index| self.forms[index].ranking.key(rank) <= key);
            let candidates = &positions[start..end];
            match candidates {
                [] => {}
                [index] => {
                    return AnchorResolution::Unique {
                        anchor: anchors[*index].clone(),
                    };
                }
                _ => {
                    return AnchorResolution::Ambiguous {
                        candidates: candidates
                            .iter()
                            .map(|&index| anchors[index].clone())
                            .collect(),
                    };
                }
            }
        }
        let matched = self.ranked_matches(&query, anchors);
        self.resolution(&symbol, &query, false, &matched, anchors)
    }

    pub(crate) fn analyze(
        &self,
        symbol: &str,
        anchors: &[String],
        contains: impl Fn(&str) -> bool,
    ) -> AnchorResolutionAnalysis {
        let symbol = Self::normalize_alias(symbol, &contains);
        let exact = contains(&symbol);
        let query = Query::new(&symbol);
        let matched = self.ranked_matches(&query, anchors);
        let mut ranked_matches = Vec::with_capacity(matched.len() + usize::from(exact));
        if exact {
            ranked_matches.push(symbol.to_string());
        }
        ranked_matches.extend(matched.iter().filter_map(|&(_, index)| {
            if exact && anchors[index].as_str() == symbol.as_ref() {
                None
            } else {
                Some(anchors[index].clone())
            }
        }));
        AnchorResolutionAnalysis {
            resolution: self.resolution(&symbol, &query, exact, &matched, anchors),
            ranked_matches,
        }
    }

    fn ranked_matches(&self, query: &Query, anchors: &[String]) -> Vec<(u8, usize)> {
        let mut matched: Vec<_> = self
            .forms
            .iter()
            .enumerate()
            .filter_map(|(index, forms)| {
                let rank = forms.ranking.rank(query);
                (rank != u8::MAX).then_some((rank, index))
            })
            .collect();
        matched.sort_by(|&(left_rank, left), &(right_rank, right)| {
            left_rank
                .cmp(&right_rank)
                .then_with(|| anchors[left].cmp(&anchors[right]))
        });
        matched
    }

    fn resolution(
        &self,
        symbol: &str,
        query: &Query,
        exact: bool,
        matched: &[(u8, usize)],
        anchors: &[String],
    ) -> AnchorResolution {
        if exact {
            return AnchorResolution::Exact(symbol.to_string());
        }
        if let Some(&(best_rank, _)) = matched.first() {
            let candidates: Vec<_> = matched
                .iter()
                .take_while(|(rank, _)| *rank == best_rank)
                .map(|&(_, index)| anchors[index].clone())
                .collect();
            if best_rank >= 5 {
                AnchorResolution::NotFound {
                    suggestions: candidates.into_iter().take(8).collect(),
                }
            } else if candidates.len() == 1 {
                AnchorResolution::Unique {
                    anchor: candidates.into_iter().next().unwrap(),
                }
            } else {
                AnchorResolution::Ambiguous { candidates }
            }
        } else {
            AnchorResolution::NotFound {
                suggestions: self.typo_suggestions(query, anchors),
            }
        }
    }

    fn typo_suggestions(&self, query: &Query, anchors: &[String]) -> Vec<String> {
        let symbol_len = query.lower.chars().count();
        let Some(max_distance) = aden_core::symbol::typo_max_distance(symbol_len) else {
            return Vec::new();
        };
        let mut suggestions = aden_core::symbol::SymbolSuggestions::default();
        for (index, forms) in self.forms.iter().enumerate() {
            // The reference lowercases the whole segment before taking its
            // typo leaf. Unicode case context makes this distinct from rank 3.
            let segment = forms.ranking.segment_lower.as_str();
            let leaf = segment
                .rsplit_once(['.', ':'])
                .map(|(_, leaf)| (leaf, forms.typo_leaf_chars));
            let distance = std::iter::once((segment, forms.segment_chars))
                .chain(leaf)
                .filter(|(_, length)| length.abs_diff(symbol_len) <= max_distance)
                .filter_map(|(candidate, _)| {
                    aden_core::symbol::edit_distance_within(&query.lower, candidate, max_distance)
                })
                .min();
            if let Some(distance) = distance {
                suggestions.insert(distance, &anchors[index]);
            }
        }
        suggestions.into_anchors()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{analyze_anchor_list, anchor_match_rank};
    use std::collections::HashSet;

    fn assert_parity(anchors: &[String], queries: &[String]) {
        let prepared = PreparedSymbols::new(anchors);
        let exact: HashSet<_> = anchors.iter().map(String::as_str).collect();
        for query in queries {
            let reference = analyze_anchor_list(query, anchors);
            assert_eq!(
                prepared.analyze(query, anchors, |anchor| exact.contains(anchor)),
                reference,
                "analysis query {query:?}"
            );
            assert_eq!(
                prepared.resolve(query, anchors, |anchor| exact.contains(anchor)),
                reference.resolution,
                "resolution query {query:?}"
            );
        }
    }

    #[test]
    fn prepared_lookup_preserves_graph_ranking_unicode_aliases_and_recovery() {
        let mut anchors: Vec<String> = [
            "aden://module/demo/a.rs#Thing<T>::run",
            "aden://module/demo/a.rs#THING::RUN",
            "aden://module/demo/b.rs#Thing::run",
            "aden://module/demo/a.rs#run",
            "aden://module/demo/a.rs#RUN",
            "aden://module/demo/a.rs#Outer<T>::Inner<U>::resolve<V>",
            "aden://module/demo/a.rs#Thing::runner",
            "aden://module/demo/a.rs#Thing.run_extra",
            "aden://module/demo/a.rs#Thing:run",
            "aden://module/demo/a.rs#::leading",
            "aden://module/demo/a.rs#Trailing::",
            "aden://module/demo/a.rs#space < Outer < T > > :: run",
            "aden://module/demo/a.rs#unterminated<T",
            "aden://module/demo/a.rs#extra>>closer",
            "aden://module/demo/a.rs#Ο.Σ",
            "aden://module/demo/a.rs#İ::Çalış",
            "aden://module/demo/a.rs#Straße::ÜBER",
            "aden://module/demo/a.rs#東京::処理",
            r"aden://module/demo/a.php#App\Billing\Service\send",
            "aden://module/demo/a.rs#",
            "aden://doc/demo/chapter.adoc/",
            "aden://doc/demo/same#run",
            "aden://module/demo/same#run",
            "demo/same#run",
            "aden://doc/demo/doc-only#answer",
            "bare_anchor",
            "",
            "aden://module/demo/a.rs#run",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        for index in 0..12 {
            anchors.push(format!("aden://module/demo/{index}.rs#prefix_find_{index}"));
        }
        let mut queries: Vec<String> = [
            "run",
            "RUN",
            "Run",
            "rUn",
            "Thing",
            "thing",
            "Thing<T> :: run",
            "Thing::ru",
            "runner",
            "runne",
            "rnu",
            "resolve",
            "resovle",
            "Outer :: Inner :: resolve",
            "demo/same#run",
            "demo/doc-only#answer",
            "demo/missing#run",
            "bare_anchor",
            "find",
            "prefix_find",
            "prefix_finx_1",
            "missing_symbol",
            "x",
            "",
            "  ",
            "<T>",
            "::",
            "Trailing::",
            "leading",
            "unterminated<T",
            "extra>>closer",
            "Σ",
            "σ",
            "ς",
            "Ο.Σ",
            "ο.ς",
            "Çalış",
            "çalış",
            "İ",
            "i\u{307}",
            "Straße",
            "ÜBER",
            "über",
            "処理",
            "東京",
            "send",
            r"Service\send",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        queries.extend(anchors.iter().cloned());
        assert_parity(&anchors, &queries);
        anchors.reverse();
        assert_parity(&anchors, &queries);
        assert_parity(&[], &queries);

        // Graph shorthand deliberately does not adopt core's PHP separator.
        let php = vec![r"aden://module/demo/a.php#App\Billing\Service\send".to_string()];
        let prepared = PreparedSymbols::new(&php);
        assert!(matches!(
            prepared.resolve("send", &php, |anchor| php
                .iter()
                .any(|candidate| candidate == anchor)),
            AnchorResolution::NotFound { .. }
        ));
    }

    #[test]
    fn prepared_lookup_matches_deterministic_generated_rank_ties_and_misses() {
        let mut anchors = Vec::new();
        for index in 0..160 {
            let name = format!("method_{:02}", index % 29);
            let segment = match index % 5 {
                0 => format!("Group< T{index} >::{name}"),
                1 => format!("Type{}.{}", index / 9, name.to_uppercase()),
                2 => name.clone(),
                3 => format!("Group::Inner<T>::{name}"),
                _ => format!("UnicodeΣ::{name}"),
            };
            anchors.push(format!("aden://module/generated/file_{index}.rs#{segment}"));
        }
        let mut queries = vec![
            "Group".into(),
            "Type".into(),
            "Inner".into(),
            "absent_value".into(),
            "method_".into(),
        ];
        for index in 0..35 {
            queries.extend([
                format!("method_{index:02}"),
                format!("METHOD_{index:02}"),
                format!("method_{index:02}x"),
                format!("Group < T > :: method_{index:02}"),
            ]);
        }
        for index in (0..anchors.len()).step_by(7) {
            queries.push(anchors[index].clone());
            queries.push(
                anchors[index]
                    .strip_prefix("aden://module/")
                    .unwrap()
                    .to_string(),
            );
        }
        assert_parity(&anchors, &queries);
        anchors.rotate_left(63);
        assert_parity(&anchors, &queries);
    }

    #[test]
    fn four_integer_indexes_preserve_rank_precedence_and_complete_discovery() {
        for (anchors, query, rank) in [
            (vec!["Thing", "Namespace::Thing"], "Thing", 0),
            (vec!["THING", "Namespace::thing"], "thing", 1),
            (vec!["Namespace::Thing", "Other::THING"], "Thing", 2),
            (vec!["Namespace::THING", "Other::THING"], "thing", 3),
        ] {
            let anchors: Vec<_> = anchors
                .into_iter()
                .map(|name| format!("aden://module/test.rs#{name}"))
                .collect();
            let prepared = PreparedSymbols::new(&anchors);
            assert_eq!(prepared.indexes.len(), 4);
            assert!(
                prepared
                    .indexes
                    .iter()
                    .all(|index| index.len() == anchors.len())
            );
            let analysis = prepared.analyze(query, &anchors, |anchor| {
                anchors.iter().any(|candidate| candidate == anchor)
            });
            assert_eq!(analysis, analyze_anchor_list(query, &anchors));
            assert_eq!(anchor_match_rank(&analysis.ranked_matches[0], query), rank);
            assert_eq!(analysis.ranked_matches.len(), anchors.len());
        }
    }

    #[test]
    fn leaf_and_qualified_name_keep_distinct_unicode_lowercase_forms() {
        let anchor = "aden://module/test.rs#Ο.Σ";
        // Final sigma depends on the preceding cased letter in the full name.
        // The leaf starts a fresh lowercase conversion and uses ordinary sigma.
        for (query, expected) in [("ο.ς", 1), ("σ", 3), ("ς", 5), ("ο", 4)] {
            assert_eq!(anchor_match_rank(anchor, query), expected, "{query}");
        }
    }

    #[test]
    fn list_ranks_mixed_unicode_matches_in_contract_order() {
        let query = " ç a l ı ş < T > ";
        let cases = [
            ("zed_çalış", 5),
            ("çalış::field", 4),
            ("Namespace::Çalış", 3),
            ("unrelated", u8::MAX),
            ("Namespace::çalış", 2),
            ("Çalış", 1),
            ("çalış", 0),
            ("çalış.member", 4),
            ("çalış:member", 5),
        ];
        let anchors: Vec<_> = cases
            .iter()
            .map(|(symbol, _)| format!("aden://module/test.rs#{symbol}"))
            .collect();
        for (anchor, (_, rank)) in anchors.iter().zip(cases) {
            assert_eq!(anchor_match_rank(anchor, query), rank, "{anchor}");
        }

        let analysis = analyze_anchor_list(query, &anchors);
        let expected = [6, 5, 4, 2, 7, 1, 0, 8].map(|index| anchors[index].clone());
        assert_eq!(analysis.ranked_matches, expected);
        assert_eq!(
            analysis.resolution,
            AnchorResolution::Unique {
                anchor: anchors[6].clone(),
            }
        );
    }

    #[test]
    #[ignore = "release-only diagnostic; no production performance claim"]
    fn prepared_symbol_lookup_release_diagnostic() {
        use std::hint::black_box;
        use std::time::Instant;
        assert!(
            !std::hint::black_box(cfg!(debug_assertions)),
            "run this ignored diagnostic with --release"
        );
        let mut anchors: Vec<_> = (0..4096)
            .map(|index| {
                format!(
                    "aden://module/bench/file_{index}.rs#Group{}::method_{index:04}",
                    index / 32
                )
            })
            .collect();
        anchors.extend([
            "aden://module/bench/a.rs#A::run".into(),
            "aden://module/bench/b.rs#B::run".into(),
        ]);
        let exact: HashSet<_> = anchors.iter().map(String::as_str).collect();
        let queries: Vec<String> = [
            anchors[123].clone(),
            anchors[123].strip_prefix("aden://module/").unwrap().into(),
            "Group3::method_0123".into(),
            "group3::method_0123".into(),
            "method_0123".into(),
            "METHOD_0123".into(),
            "run".into(),
            "Group3".into(),
            "method_01".into(),
            "method_012x".into(),
            "unrelated_unknown_symbol".into(),
        ]
        .into();
        let start = Instant::now();
        let prepared = PreparedSymbols::new(&anchors);
        let preparation_us = start.elapsed().as_micros();
        for query in &queries {
            let old = analyze_anchor_list(query, &anchors);
            assert_eq!(
                prepared.analyze(query, &anchors, |anchor| exact.contains(anchor)),
                old
            );
            assert_eq!(
                prepared.resolve(query, &anchors, |anchor| exact.contains(anchor)),
                old.resolution
            );
        }
        let repetitions = 12;
        let time = |prepared_arm: bool, analysis: bool| {
            let start = Instant::now();
            for _ in 0..repetitions {
                for query in &queries {
                    if analysis {
                        black_box(if prepared_arm {
                            prepared.analyze(black_box(query), black_box(&anchors), |anchor| {
                                exact.contains(anchor)
                            })
                        } else {
                            analyze_anchor_list(black_box(query), black_box(&anchors))
                        });
                    } else {
                        black_box(if prepared_arm {
                            prepared.resolve(black_box(query), black_box(&anchors), |anchor| {
                                exact.contains(anchor)
                            })
                        } else {
                            analyze_anchor_list(black_box(query), black_box(&anchors)).resolution
                        });
                    }
                }
            }
            start.elapsed().as_micros()
        };
        // Alternate timing order for the distinct resolve and exhaustive paths.
        let old_resolve_us = time(false, false);
        let prepared_resolve_us = time(true, false);
        let prepared_analysis_us = time(true, true);
        let old_analysis_us = time(false, true);
        let form_heap_bytes: usize = prepared
            .forms
            .iter()
            .map(|forms| {
                forms.ranking.segment.capacity()
                    + forms.ranking.segment_lower.capacity()
                    + forms.ranking.leaf_lower.capacity()
            })
            .sum();
        let integer_index_bytes: usize = prepared
            .indexes
            .iter()
            .map(|index| index.capacity() * std::mem::size_of::<usize>())
            .sum();
        println!(
            "{}",
            serde_json::json!({"anchors": anchors.len(), "queries_per_repetition": queries.len(), "repetitions": repetitions, "preparation_us": preparation_us, "normalized_string_capacity_bytes": form_heap_bytes, "integer_index_capacity_bytes": integer_index_bytes, "reference_resolve_us": old_resolve_us, "prepared_resolve_us": prepared_resolve_us, "reference_exhaustive_analysis_us": old_analysis_us, "prepared_exhaustive_analysis_us": prepared_analysis_us})
        );
    }
}
