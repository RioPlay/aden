// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// Original author and maintainer: RioPlay
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
//! Simple inverted-index search over `.adoc` / `.aden` documents.
//!
//! Tokenizes by whitespace and strips punctuation.  Indexes anchors,
//! attributes (keys and values), table cell text, and description-list
//! terms.  Ignores a small set of English stop words.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use aden_core::filter::AdenFilter;

/// A single search result.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub anchor: String,
    pub source_path: PathBuf,
    pub score: f64,
    pub snippet: String,
}

/// In-memory inverted index built from a directory of `.adoc`/`.aden` files.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Index {
    /// token -> [(anchor, occurrences_in_document)]
    inverted: HashMap<String, Vec<(String, usize)>>,
    /// anchor -> source path
    anchor_paths: HashMap<String, PathBuf>,
    /// anchor -> full file text (for snippet generation)
    anchor_text: HashMap<String, String>,
    /// anchor -> token count (for BM25 scoring)
    doc_lengths: HashMap<String, usize>,
    /// Average document length across all documents
    avg_doc_length: f64,
}

/// BM25 parameters
const BM25_K1: f64 = 1.5;
const BM25_B: f64 = 0.75;

const INDEX_CACHE_FILE: &str = ".aden/cache/index-cache.json";

/// Build an index, using the on-disk cache when possible.
/// `key` should be a hash of all `.adoc`/`.aden` file paths + mtimes.
pub fn try_load(dir: &std::path::Path) -> Option<Index> {
    let index_path = dir.join(INDEX_CACHE_FILE);
    if !index_path.exists() {
        return None;
    }
    let text = std::fs::read_to_string(&index_path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Save the index to disk cache.
pub fn save(index: &Index, dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let cache_dir = dir.join(".aden/cache");
    std::fs::create_dir_all(&cache_dir)?;
    let index_path = cache_dir.join("index-cache.json");
    let json = serde_json::to_string_pretty(index)?;
    std::fs::write(&index_path, json)?;
    Ok(())
}

/// Set of common English stop words ignored during indexing and querying.
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "could", "should", "may", "might",
    "must", "shall", "can", "need", "dare", "ought", "used", "to", "of", "in", "for", "on", "with",
    "at", "by", "from", "as", "into", "through", "during", "before", "after", "above", "below",
    "between", "under", "again", "further", "then", "once", "it", "its", "it's", "this", "that",
    "these", "those", "i", "you", "he", "she", "we", "they", "me", "him", "her", "us", "them",
    "my", "your", "his", "our", "their", "what", "which", "who", "when", "where", "why", "how",
    "all", "each", "few", "more", "most", "other", "some", "such", "no", "nor", "not", "only",
    "own", "same", "so", "than", "too", "very", "just", "now",
];

fn is_stop_word(word: &str) -> bool {
    STOP_WORDS.contains(&word)
}

/// Tokenize a string into lowercase words with punctuation stripped.
pub fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| c.is_ascii_punctuation())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty() && !is_stop_word(w))
        .collect()
}

fn levenshtein_distance(s1: &str, s2: &str) -> usize {
    let len1 = s1.len();
    let len2 = s2.len();

    if len1 == 0 {
        return len2;
    }
    if len2 == 0 {
        return len1;
    }

    let mut matrix = vec![vec![0usize; len2 + 1]; len1 + 1];

    // Initializing first column - classic DP pattern
    #[allow(clippy::needless_range_loop)]
    for i in 0..=len1 {
        matrix[i][0] = i;
    }
    // Initializing first row - clippy suggestion doesn't apply here
    #[allow(clippy::needless_range_loop)]
    for j in 0..=len2 {
        matrix[0][j] = j;
    }

    let s1_bytes = s1.as_bytes();
    let s2_bytes = s2.as_bytes();

    for i in 1..=len1 {
        for j in 1..=len2 {
            let cost = if s1_bytes[i - 1] == s2_bytes[j - 1] { 0 } else { 1 };
            matrix[i][j] = (matrix[i - 1][j] + 1)
                .min(matrix[i][j - 1] + 1)
                .min(matrix[i - 1][j - 1] + cost);
        }
    }

    matrix[len1][len2]
}

/// Parse an `.adoc` / `.aden` file and return the anchor, the source path,
/// and a bag-of-words mapping `token -> count`.
fn parse_adoc(path: &Path, text: &str) -> Option<(String, PathBuf, HashMap<String, usize>)> {
    let mut anchor: Option<String> = None;
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut in_table = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Anchor: [[...]]
        if trimmed.starts_with("[[") && trimmed.ends_with("]]") {
            let a = trimmed[2..trimmed.len() - 2].trim().to_string();
            if !a.is_empty() {
                anchor = Some(a.clone());
                for token in tokenize(&a) {
                    *counts.entry(token).or_insert(0) += 1;
                }
            }
            continue;
        }

        // Attribute: :key: value
        if trimmed.starts_with(':') && !trimmed.starts_with("::") {
            if let Some(rel_pos) = trimmed[1..].find(':') {
                let key_end = 1 + rel_pos; // absolute index of the second colon
                let key = &trimmed[1..key_end];
                let value = trimmed[key_end + 1..].trim();
                for token in tokenize(key) {
                    *counts.entry(token).or_insert(0) += 1;
                }
                for token in tokenize(value) {
                    *counts.entry(token).or_insert(0) += 1;
                }
            }
            continue;
        }

        // Table boundaries
        if trimmed == "|===" {
            in_table = !in_table;
            continue;
        }

        // Table cell text
        if in_table && trimmed.starts_with('|') {
            let cell_text = trimmed[1..].trim();
            for token in tokenize(cell_text) {
                *counts.entry(token).or_insert(0) += 1;
            }
            continue;
        }

        // Description list term: term:: definition (or term::definition)
        // Skip AsciiDoc directive lines (ifdef::, endif::, ifndef::, ifeval::)
        if let Some(pos) = trimmed.find("::") {
            let term = &trimmed[..pos];
            if !term.is_empty()
                && !term.starts_with("ifdef")
                && !term.starts_with("endif")
                && !term.starts_with("ifndef")
                && !term.starts_with("ifeval")
            {
                let def = &trimmed[pos + 2..].trim_start();
                for token in tokenize(term) {
                    *counts.entry(token).or_insert(0) += 1;
                }
                for token in tokenize(def) {
                    *counts.entry(token).or_insert(0) += 1;
                }
                continue;
            }
        }

        // Plain text (paragraphs, section titles, etc.)
        if !in_table && !trimmed.is_empty() {
            for token in tokenize(trimmed) {
                *counts.entry(token).or_insert(0) += 1;
            }
        }
    }

    anchor.map(|a| (a, path.to_path_buf(), counts))
}

fn build_snippet(text: &str, query_tokens: &[String]) -> String {
    // Find the first line that contains any query token.
    for line in text.lines() {
        let line_lower = line.to_lowercase();
        for token in query_tokens {
            if line_lower.contains(token) {
                let snippet = line.trim();
                if snippet.len() > 200 {
                    return format!("{}...", &snippet[..200]);
                }
                return snippet.to_string();
            }
        }
    }
    // Fallback: first non-empty line or empty string.
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

impl Index {
    /// Build an index from all `.adoc` and `.aden` files under `dir`.
    pub fn from_directory(dir: &Path) -> Result<Self, std::io::Error> {
        let mut index = Index::default();
        let mut files = Vec::new();
        let filter = AdenFilter::from_directory(dir);
        Self::collect_files(dir, &filter, &mut files)?;

        for path in files {
            let text = std::fs::read_to_string(&path)?;
            if let Some((anchor, source_path, counts)) = parse_adoc(&path, &text) {
                index.anchor_paths.insert(anchor.clone(), source_path);
                index.anchor_text.insert(anchor.clone(), text.clone());
                let doc_len: usize = counts.values().sum();
                index.doc_lengths.insert(anchor.clone(), doc_len);
                for (token, count) in counts {
                    index
                        .inverted
                        .entry(token)
                        .or_default()
                        .push((anchor.clone(), count));
                }
            }
        }

        // Compute average document length for BM25
        let total_len: usize = index.doc_lengths.values().sum();
        let doc_count = index.doc_lengths.len();
        index.avg_doc_length = if doc_count > 0 {
            total_len as f64 / doc_count as f64
        } else {
            1.0
        };

        Ok(index)
    }

    fn collect_files(dir: &Path, filter: &AdenFilter, out: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
        Self::collect_files_inner(dir, dir, filter, out)
    }

    fn collect_files_inner(
        dir: &Path,
        root: &Path,
        filter: &AdenFilter,
        out: &mut Vec<PathBuf>,
    ) -> Result<(), std::io::Error> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false) {
                continue;
            }
            if path.is_dir() {
                if let Ok(rel) = path.strip_prefix(root)
                    && filter.should_skip(rel) {
                        continue;
                    }
                Self::collect_files_inner(&path, root, filter, out)?;
            } else if path.is_file() {
                if let Ok(rel) = path.strip_prefix(root)
                    && filter.should_skip(rel) {
                        continue;
                    }
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if ext == "adoc" || ext == "aden" {
                    out.push(path);
                }
            }
        }
        Ok(())
    }

    /// Query the index and return ranked search results using BM25.
    pub fn query(&self, query_str: &str) -> Vec<SearchResult> {
        let tokens = tokenize(query_str);
        if tokens.is_empty() {
            return Vec::new();
        }

        let n = self.inverted.values().map(|v| v.len()).sum::<usize>() as f64;
        if n == 0.0 {
            return Vec::new();
        }

        let mut scores: HashMap<String, f64> = HashMap::new();

        // BM25 scoring
        for token in &tokens {
            if let Some(postings) = self.inverted.get(token) {
                let df = postings.len() as f64;
                // IDF: log((N - df + 0.5) / (df + 0.5))
                let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln().max(0.0);

                for (anchor, tf) in postings {
                    let doc_len = self.doc_lengths.get(anchor).copied().unwrap_or(1);
                    let tf_normalized = (*tf as f64 * (BM25_K1 + 1.0))
                        / (*tf as f64
                            + BM25_K1
                                * (1.0 - BM25_B + BM25_B * doc_len as f64 / self.avg_doc_length));
                    *scores.entry(anchor.clone()).or_insert(0.0) += idf * tf_normalized;
                }
            }
        }

        // Apply title/anchor boosts
        let query_lower = query_str.to_lowercase();
        for (anchor, score) in scores.iter_mut() {
            let anchor_lower = anchor.to_lowercase();
            let source_path = self.anchor_paths.get(anchor);

            // Penalize source-file anchors (e.g., aden://module/...#function_name)
            // These are specific symbols, not module-level docs
            let is_source_anchor = anchor.contains("://module/") || anchor.contains("/src/");
            if is_source_anchor {
                *score *= 0.1; // 90% penalty for source file anchors
            }

            // Penalize .agent/ templates - they're for AI agents, not human-facing docs
            if let Some(path) = source_path {
                let path_str = path.to_string_lossy().to_lowercase();
                if path_str.contains(".agent/") || path_str.contains(".agent\\") {
                    *score *= 0.01; // 99% penalty - almost exclude from search results
                }
            }

            // 20x boost if query term appears in anchor (mod-*, adr-* patterns)
            if query_lower
                .split_whitespace()
                .any(|t| anchor_lower.contains(t))
            {
                *score *= 20.0;
            }
            // Additional 10x boost for title match (first line)
            if let Some(text) = self.anchor_text.get(anchor)
                && let Some(first_line) = text.lines().next()
                && query_lower
                    .split_whitespace()
                    .any(|t| first_line.to_lowercase().contains(t))
            {
                *score *= 10.0;
            }
        }

        let mut results: Vec<SearchResult> = scores
            .into_iter()
            .filter_map(|(anchor, score)| {
                let source_path = self.anchor_paths.get(&anchor)?.clone();
                let text = self.anchor_text.get(&anchor)?;
                let snippet = build_snippet(text, &tokens);
                Some(SearchResult {
                    anchor,
                    source_path,
                    score,
                    snippet,
                })
            })
            .collect();

        // Sort by descending score
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // If no results or weak results (score < 1.0), try fuzzy search
        if results.is_empty() || results.first().map(|r| r.score < 1.0).unwrap_or(true) {
            let fuzzy = self.fuzzy_query(&tokens);
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

    fn fuzzy_query(&self, tokens: &[String]) -> Vec<SearchResult> {
        if tokens.is_empty() {
            return Vec::new();
        }

        let mut fuzzy_matches: Vec<(String, f64)> = Vec::new();
        let query_term = &tokens[0].to_lowercase();

        for anchor in self.anchor_paths.keys() {
            let anchor_lower = anchor.to_lowercase();
            let dist = levenshtein_distance(query_term, &anchor_lower);
            let all_chars_match = query_term.chars().all(|c| anchor_lower.contains(c));
            if dist <= 2 || (query_term.len() >= 2 && all_chars_match) {
                fuzzy_matches.push((anchor.clone(), 1.0));
            }
        }

        fuzzy_matches.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        fuzzy_matches
            .into_iter()
            .filter_map(|(anchor, score)| {
                let source_path = self.anchor_paths.get(&anchor)?.clone();
                let doc_text = self.anchor_text.get(&anchor)?.clone();
                let snippet = build_snippet(&doc_text, tokens);
                Some(SearchResult {
                    anchor,
                    source_path,
                    score,
                    snippet,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir_with_files(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut file = std::fs::File::create(&path).unwrap();
            file.write_all(content.as_bytes()).unwrap();
        }
        dir
    }

    #[test]
    fn tokenize_strips_punctuation() {
        let tokens = tokenize("Hello, world! This is a test.");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"test".to_string()));
        assert!(!tokens.contains(&"a".to_string())); // stop word
        assert!(!tokens.contains(&"is".to_string())); // stop word
    }

    #[test]
    fn index_from_directory_basic() {
        let dir = temp_dir_with_files(&[(
            "test.adoc",
            r#":author: Alice
[[test-anchor]]
= Title

Hello world from the test document.

|===
|Col A |Col B

|cell one |cell two
|===

description term:: definition text here.
"#,
        )]);

        let index = Index::from_directory(dir.path()).unwrap();
        let results = index.query("hello world");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].anchor, "test-anchor");
        assert!(results[0].score > 0.0);
        assert_eq!(results[0].source_path.file_name().unwrap(), "test.adoc");
    }

    #[test]
    fn query_finds_table_cells() {
        let dir = temp_dir_with_files(&[(
            "table.adoc",
            r#"[[table-doc]]
= Table

|===
|Color |Shape

|blue |square
|===
"#,
        )]);

        let index = Index::from_directory(dir.path()).unwrap();
        let results = index.query("blue square");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].anchor, "table-doc");
        assert!(results[0].score >= 2.0);
    }

    #[test]
    fn query_finds_description_list_terms() {
        let dir = temp_dir_with_files(&[(
            "dl.adoc",
            r#"[[dl-doc]]
= DL

foo:: bar baz.
"#,
        )]);

        let index = Index::from_directory(dir.path()).unwrap();
        let results = index.query("foo");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].anchor, "dl-doc");

        let results = index.query("baz");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].anchor, "dl-doc");
    }

    #[test]
    fn query_finds_attribute_keys_and_values() {
        let dir = temp_dir_with_files(&[(
            "attr.adoc",
            r#":project: Zebra
[[attr-doc]]
= Attr
"#,
        )]);

        let index = Index::from_directory(dir.path()).unwrap();
        let results = index.query("zebra");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].anchor, "attr-doc");

        let results = index.query("project");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].anchor, "attr-doc");
    }

    #[test]
    fn query_ranks_by_score() {
        let dir = temp_dir_with_files(&[
            (
                "a.adoc",
                r#"[[a]]
= A

hello hello hello.
"#,
            ),
            (
                "b.adoc",
                r#"[[b]]
= B

hello.
"#,
            ),
        ]);

        let index = Index::from_directory(dir.path()).unwrap();
        let results = index.query("hello");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].anchor, "a");
        assert_eq!(results[1].anchor, "b");
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn query_returns_empty_for_no_match() {
        let dir = temp_dir_with_files(&[(
            "empty.adoc",
            r#"[[empty]]
= Empty
"#,
        )]);

        let index = Index::from_directory(dir.path()).unwrap();
        let results = index.query("nonexistent");
        assert!(results.is_empty());
    }

    #[test]
    fn query_ignores_stop_words() {
        let dir = temp_dir_with_files(&[(
            "stop.adoc",
            r#"[[stop]]
= Stop

The and or but.
"#,
        )]);

        let index = Index::from_directory(dir.path()).unwrap();
        let results = index.query("the and or");
        assert!(results.is_empty());
    }

    #[test]
    fn snippet_contains_matching_line() {
        let dir = temp_dir_with_files(&[(
            "snippet.adoc",
            r#"[[snippet]]
= Snippet

First line.
Second line with zebra.
Third line.
"#,
        )]);

        let index = Index::from_directory(dir.path()).unwrap();
        let results = index.query("zebra");
        assert_eq!(results.len(), 1);
        assert!(results[0].snippet.contains("zebra"));
    }

    #[test]
    fn index_skips_non_adoc_files() {
        let dir = temp_dir_with_files(&[
            (
                "readme.md",
                r#"[[md]]
# Markdown

hello.
"#,
            ),
            (
                "doc.adoc",
                r#"[[adoc]]
= Adoc

hello.
"#,
            ),
        ]);

        let index = Index::from_directory(dir.path()).unwrap();
        let results = index.query("hello");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].anchor, "adoc");
    }

    #[test]
    fn index_excludes_agent_templates() {
        let dir = temp_dir_with_files(&[
            (
                "docs/main.adoc",
                r#"[[main-doc]]
= Main

This is the main documentation.
"#,
            ),
            (
                ".agent/templates/onboarding.adoc",
                r#"[[agent-onboarding-template]]
= Onboarding

Welcome to the project.
"#,
            ),
            (
                ".agent/templates/style-guide.adoc",
                r#"[[style-guide]]
= Style Guide

Follow these rules.
"#,
            ),
            (
                ".agent/context.adoc",
                r#"[[agent-context]]
= Agent Context

Project context.
"#,
            ),
        ]);

        let index = Index::from_directory(dir.path()).unwrap();
        let results = index.query("onboarding");
        assert!(results.is_empty(), "Should not find .agent/templates/ files");

        let results = index.query("style guide");
        assert!(results.is_empty(), "Should not find .agent/templates/ files");

        let results = index.query("agent context");
        assert!(results.is_empty(), "Should not find .agent/ files");

        let results = index.query("main");
        assert_eq!(results.len(), 1, "Should find docs/main.adoc");
        assert_eq!(results[0].anchor, "main-doc");
    }
}
