// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Simple inverted-index search over `.adoc` / `.aden` documents.
//!
//! Tokenizes by whitespace and strips punctuation.  Indexes anchors,
//! attributes (keys and values), table cell text, and description-list
//! terms.  Ignores a small set of English stop words.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use aden_core::filter::AdenFilter;
use rayon::prelude::*;
use rust_stemmers::{Algorithm, Stemmer};

/// Local ONNX dense-embedding provider (hybrid retrieval). Behind the `dense`
/// feature so the default build carries no ML dependencies.
#[cfg(feature = "dense")]
mod dense;
#[cfg(feature = "dense")]
pub use dense::TractEmbedder;

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
    /// Tokenizer/format version the cache on disk was built with. Bumped whenever
    /// the tokenization pipeline changes so a stale cache is rebuilt rather than
    /// silently shadowing the new logic. A versionless (pre-stemming) cache
    /// deserializes to `0` and is rejected by [`try_load`].
    #[serde(default)]
    version: u32,
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
    /// anchor -> dense embedding vector (for hybrid retrieval). Empty unless an
    /// [`EmbeddingProvider`] has been run via [`Index::embed_documents`]; when
    /// empty, [`Index::hybrid_query`] degrades to pure BM25. `serde(default)` so
    /// a pre-embedding cache still deserializes (it is rejected by the version
    /// check anyway and rebuilt).
    #[serde(default)]
    embeddings: HashMap<String, Vec<f32>>,
    /// anchor -> content hash of the text that produced the stored embedding.
    /// Lets [`Index::embed_documents`] re-embed only documents whose text changed
    /// (incremental embedding) instead of the whole corpus every time. `serde(default)`
    /// so older caches deserialize; the version bump rebuilds them once.
    #[serde(default)]
    embedding_hashes: HashMap<String, u64>,
}

/// BM25 parameters
const BM25_K1: f64 = 1.5;
const BM25_B: f64 = 0.75;

/// Multi-token coverage boost weight (M14). A document matching `k` distinct
/// query terms is scaled by `1 + COVERAGE_WEIGHT*(k-1)`. At `1.0` the multiplier
/// *equals the coordination level* `k` (2 matched terms → 2×, 3 → 3×) — a
/// principled, non-arbitrary model rather than a tuned magic number: matching
/// twice as many distinct query concepts roughly doubles relevance confidence.
/// This is what keeps a single rare high-IDF term from outranking a document
/// that covers more of the query. Validated against the retrieval eval harness
/// (`crates/aden-index/tests/eval.rs`); see [`Index::query`].
const COVERAGE_WEIGHT: f64 = 1.0;

const INDEX_CACHE_BASENAME: &str = "index-cache.json";

/// Current tokenizer/format version. Bumped on ANY change to how tokens are
/// produced, so a stale cache is rebuilt rather than silently shadowing the new
/// logic. v1: pre-stemming, versionless (deserializes to `0`). v2: conservative
/// suffix stemming in [`tokenize`]/[`Index::query`]. v3: `-ss` guard + `get`
/// stop word. v4: `-us`/`-is` guard (status, focus, analysis stay whole).
/// v5: `-is`/`-es` plural normalization (analyses→analysis) so Greek `-sis`
/// nouns and their plurals share a stem.
/// v6: identifier sub-token expansion in [`tokenize`] (camelCase / `_-/.:`).
/// v7: per-anchor dense embeddings stored for hybrid retrieval (new `embeddings`
/// field on [`Index`]). The stored format changed, so older caches rebuild.
/// v8: per-anchor embedding content hashes (`embedding_hashes`) for *incremental*
/// embedding — re-embed only changed docs. Bumped so v7 caches (which carry no
/// hashes) rebuild once cleanly rather than trusting vectors of unknown freshness.
/// v10: MAX_SEQ 128 -> 512 so full prose sections embed (CLS pooling, single
/// forward). Changes the stored vectors without the doc text changing, so the
/// persisted index rebuilds and `EMBED_PARAM_VERSION` busts the content-addressed
/// embedding cache in parallel. (A whole-doc mean-pool variant was trialed on this
/// version and reverted as net-negative for retrieval — see `EMBED_PARAM_VERSION`.)
const CURRENT_INDEX_VERSION: u32 = 10;

/// Reciprocal Rank Fusion constant — Cormack, Clarke & Buettcher, SIGIR 2009 use
/// 60. Damps the contribution of low-ranked items so a top hit in either
/// retriever dominates a long tail in the other.
const RRF_K: f64 = 60.0;

/// Build an index, using the on-disk cache when possible.
/// `key` should be a hash of all `.adoc`/`.aden` file paths + mtimes.
pub fn try_load(dir: &std::path::Path) -> Option<Index> {
    let index_path = aden_paths::cache_dir(dir).join(INDEX_CACHE_BASENAME);
    if !index_path.exists() {
        return None;
    }
    let text = std::fs::read_to_string(&index_path).ok()?;
    let index: Index = serde_json::from_str(&text).ok()?;
    // Reject a cache built by an older tokenizer (e.g. pre-stemming). Returning
    // `None` forces `load_or_build_index` to rebuild from source.
    if index.version != CURRENT_INDEX_VERSION {
        return None;
    }
    Some(index)
}

/// Save the index to disk cache.
pub fn save(index: &Index, dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let cache_dir = aden_paths::cache_dir(dir);
    std::fs::create_dir_all(&cache_dir)?;
    let index_path = cache_dir.join(INDEX_CACHE_BASENAME);
    let json = serde_json::to_string_pretty(index)?;
    std::fs::write(&index_path, json)?;
    Ok(())
}

/// Load the content-addressed embedding cache (hash → vector) for `dir`, or an
/// empty map if none exists yet. Lives outside the gen-wiped `cache/` dir (see
/// [`aden_paths::embeddings_cache_file`]) so vectors survive index rebuilds.
pub fn load_embedding_cache(dir: &std::path::Path) -> HashMap<String, Vec<f32>> {
    let path = aden_paths::embeddings_cache_file(dir);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Persist the content-addressed embedding cache for `dir`.
pub fn save_embedding_cache(
    cache: &HashMap<String, Vec<f32>>,
    dir: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = aden_paths::embeddings_cache_file(dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string(cache)?)?;
    Ok(())
}

/// Set of common English stop words ignored during indexing and querying.
/// Note: "may" (modal verb) is excluded when capitalized as month names.
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "could", "should", "might", "must",
    "shall", "can", "need", "dare", "ought", "used", "get", "to", "of", "in", "for", "on", "with",
    "at", "by", "from", "as", "into", "through", "during", "before", "after", "above", "below",
    "between", "under", "again", "further", "then", "once", "it", "its", "it's", "this", "that",
    "these", "those", "i", "you", "he", "she", "we", "they", "me", "him", "her", "us", "them",
    "my", "your", "his", "our", "their", "what", "which", "who", "when", "where", "why", "how",
    "all", "each", "few", "more", "most", "other", "some", "such", "no", "nor", "not", "only",
    "own", "same", "so", "than", "too", "very", "just", "now",
];

fn is_stop_word(word: &str) -> bool {
    static STOP_WORD_SET: OnceLock<std::collections::HashSet<&'static str>> = OnceLock::new();
    STOP_WORD_SET
        .get_or_init(|| STOP_WORDS.iter().copied().collect())
        .contains(word)
}

static ENGLISH_STEMMER: OnceLock<Stemmer> = OnceLock::new();

/// True if `word` is the `-es` plural of a Greek-derived `-is` noun
/// (analysis/analyses, thesis/theses, crisis/crises, basis/bases, ...). These
/// are rewritten to their `-is` singular before stemming so both spellings
/// converge to one stem. Restricted to known endings to avoid mangling common
/// `-ses` words like "cases", "phrases", or "houses".
fn is_is_plural(word: &str) -> bool {
    // Distinctive `-is`→`-es` endings; each implies a `-sis`/`-xis` singular.
    const IS_PLURAL_ENDINGS: &[&str] = &[
        "analyses", "yses",   // analyses, paralyses, ...
        "theses", // theses, hypotheses, parentheses, syntheses
        "crises", // crises
        "neuroses", "noses",    // neuroses, diagnoses, prognoses, psychoses
        "axes",     // axes (axis) — also "axe", but axis dominates in code/docs
        "bases",    // bases (basis)
        "ellipses", // ellipses
    ];
    word.len() >= 5 && IS_PLURAL_ENDINGS.iter().any(|s| word.ends_with(s))
}

/// Normalize an English word to an approximate root form using the Snowball
/// Porter2 algorithm (via `rust-stemmers`). Handles consonant-doubling
/// (`running`→`run`, `mapping`→`map`) and irregular plurals
/// (`analyses`→`analysi`, `analysis`→`analysi`) that the previous hand-rolled
/// suffix stripper could not converge.
///
/// Guards applied before delegation:
/// - Only purely-alphabetic ASCII tokens are stemmed; anything with a digit or
///   `_` (code identifiers, `SemanticNormalizer` outputs) passes through untouched.
/// - Words ending in `-uses` (e.g. `statuses`, `focuses`, `nexuses`) have `-es`
///   stripped before Porter2 sees them. Porter2 step-1a fires on the trailing `s`
///   first and collapses `statuses` → `statu`; stripping `-es` first exposes
///   the `-us` stem so Porter2 returns `status`.
fn stem(word: &str) -> String {
    // Guard: only stem purely-alphabetic ASCII tokens.
    if word.is_empty() || !word.bytes().all(|b| b.is_ascii_alphabetic()) {
        return word.to_string();
    }

    // Pre-guard: `-us` plurals formed with `-es` (statuses, focuses, nexuses).
    // Porter2 step-1a fires on the trailing `s` and gives the wrong stem.
    // Strip `-es` first to expose the `-us` base, then let Porter2 finish.
    // Pre-guard: `-is` singulars and their `-es` plurals (analysis/analyses,
    // basis/bases, thesis/theses). Porter2 stems `analysis` → `analysi` but
    // `analyses` → `analys`, so the two never converge. Normalize both to the
    // `-is` base (`analysi`-style) by rewriting a trailing `-es` plural back to
    // `-is` before Porter2 sees it: `analyses` → `analysis`, `bases` → `basis`.
    let normalized;
    let word_for_porter = if word.len() >= 6 && word.ends_with("uses") {
        &word[..word.len() - 2] // "statuses" → "status"
    } else if is_is_plural(word) {
        // "analyses" → "analysis", "theses" → "thesis", "crises" → "crisis".
        // Restricted to recognized Greek `-sis`/`-ses` endings so common words
        // ("cases", "phrases", "houses") are NOT rewritten.
        normalized = format!("{}is", &word[..word.len() - 2]);
        &normalized
    } else {
        word
    };

    let stemmer = ENGLISH_STEMMER.get_or_init(|| Stemmer::create(Algorithm::English));
    stemmer.stem(word_for_porter).into_owned()
}

/// Tokenize a string into lowercase, punctuation-stripped, stemmed words.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in text.split_whitespace() {
        // Keep the original case here so camelCase humps survive into
        // `split_subtokens` below; lowercase only for the full-token form.
        let trimmed = w.trim_matches(|c: char| c.is_ascii_punctuation());
        let cleaned = trimmed.to_lowercase();
        if cleaned.is_empty() || is_stop_word(&cleaned) {
            continue;
        }
        // Always index the full (stemmed) form so an exact identifier query —
        // `dispatch_request`, `url_map` — still matches a single strong posting.
        let full = stem(&cleaned);
        out.push(full.clone());

        // For compound identifiers (snake_case, kebab-case, dotted paths,
        // camelCase) ALSO index each component so a natural-language sub-word
        // query matches the production symbol that carries it. Without this,
        // `aden ask "how does dispatching work"` can't see `Flask.dispatch_request`
        // (indexed only as the whole token) and falls through to whatever stray
        // test fixture happens to have a bare `dispatch`/`url` param — the
        // ask-routing-to-tests defect. Plain prose words have a single sub-token
        // and are unaffected.
        let subs = split_subtokens(trimmed);
        if subs.len() > 1 {
            for sub in subs {
                if sub.len() < 2 || is_stop_word(&sub) {
                    continue;
                }
                let s = stem(&sub);
                if s != full && !out.contains(&s) {
                    out.push(s);
                }
            }
        }
    }
    out
}

/// Split an identifier-like word into lowercase sub-tokens on the conventional
/// separators (`_`, `-`, `/`, `.`, `:`, whitespace) and on camelCase humps.
/// A plain word yields a single element (itself); a compound identifier like
/// `url_map` or `dispatchRequest` yields its components. Shared by `tokenize`
/// (posting expansion) and `token_boundary_match` (query matching) so both sides
/// agree on what a sub-token is.
fn split_subtokens(haystack: &str) -> Vec<String> {
    let mut subtokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut prev_lower = false;
    for ch in haystack.chars() {
        if ch.is_ascii_alphanumeric() {
            // camelCase / lower→Upper hump starts a new sub-token.
            if ch.is_ascii_uppercase() && prev_lower && !current.is_empty() {
                subtokens.push(std::mem::take(&mut current));
            }
            current.push(ch.to_ascii_lowercase());
            prev_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        } else {
            if !current.is_empty() {
                subtokens.push(std::mem::take(&mut current));
            }
            prev_lower = false;
        }
    }
    if !current.is_empty() {
        subtokens.push(current);
    }
    subtokens
}

/// True if stemmed query token `token` matches `haystack` on a word/token
/// boundary rather than as a raw substring. `haystack` is split on common
/// identifier/slug separators (`_`, `-`, `/`, `.`, `:`, whitespace) and on
/// camelCase humps; `token` matches a sub-token when it equals that sub-token
/// or is a prefix of it (so a stem matches its inflected surface form, e.g.
/// `deliv` → `delivery`, but `run` no longer matches `runtime`).
fn token_boundary_match(haystack: &str, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    // Split into sub-tokens on separators and camelCase boundaries.
    let subtokens = split_subtokens(haystack);
    // A stem (already lowercased) matches when it equals a sub-token or is a
    // prefix of one. Prefix tolerance keeps inflected forms matching their stem
    // without re-introducing the mid-word substring problem.
    subtokens.iter().any(|sub| sub.starts_with(token))
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
            let cost = if s1_bytes[i - 1] == s2_bytes[j - 1] {
                0
            } else {
                1
            };
            matrix[i][j] = (matrix[i - 1][j] + 1)
                .min(matrix[i][j - 1] + 1)
                .min(matrix[i - 1][j - 1] + cost);
        }
    }

    matrix[len1][len2]
}

/// Parse an `.adoc` / `.aden` / `.txt` file and return the anchor, the source path,
/// and a bag-of-words mapping `token -> count`.
fn parse_adoc(path: &Path, text: &str) -> Option<(String, PathBuf, HashMap<String, usize>)> {
    let mut anchor: Option<String> = None;
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut in_table = false;
    let is_txt = path.extension().and_then(|e| e.to_str()).unwrap_or("") == "txt";

    // For .txt files, derive anchor from filename (without extension)
    if is_txt && let Some(stem) = path.file_stem() {
        let stem_str = stem.to_string_lossy().to_lowercase();
        // Replace spaces/dashes with hyphens for valid anchor
        let anchor_str = stem_str.replace([' ', '_'], "-");
        anchor = Some(anchor_str);
        // Add filename tokens
        for token in tokenize(&stem.to_string_lossy()) {
            *counts.entry(token).or_insert(0) += 1;
        }
    }

    let mut in_listing = false;
    // Tracks whether the current table is a callee/implementation-metadata table.
    // Callee tables (|Callee|Line, |Property|Value for signatures) contain
    // function call sites and parameter types — useful for display but not for
    // semantic search. Indexing them causes false positive matches (e.g. the
    // word "output" as a callee name matching queries about output).
    let mut in_metadata_table = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Track listing block delimiters (----). Content inside is code, not prose.
        // We skip indexing it to avoid polluting the search index with implementation
        // details like `edge::calls[output]` that would cause false positives.
        if trimmed == "----" {
            in_listing = !in_listing;
            continue;
        }
        if in_listing {
            continue;
        }

        // Skip edge::calls[] lines even outside listing blocks (belt-and-suspenders).
        if trimmed.starts_with("edge::") {
            continue;
        }

        // Anchor: [[...]]
        if trimmed.starts_with("[[") && trimmed.ends_with("]]") {
            let a = trimmed[2..trimmed.len() - 2].trim().to_string();
            if !a.is_empty() {
                anchor = Some(a.clone());
                // For symbol anchors (aden://module/...#symbol), only tokenize the
                // fragment after '#'. The full URI path would otherwise inject
                // path-component tokens like "get", "cache", "core" into the index,
                // causing false positive scores on unrelated queries.
                let index_str = if a.contains('#') {
                    a.rsplit('#').next().unwrap_or(&a).to_string()
                } else {
                    a.clone()
                };
                for token in tokenize(&index_str) {
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
            if in_table {
                // Leaving a table — reset metadata flag.
                in_metadata_table = false;
            }
            in_table = !in_table;
            continue;
        }

        // Table cell text
        if in_table && trimmed.starts_with('|') {
            // Detect callee table header: |Callee|Line
            // This table lists call sites (function names + line numbers) which are
            // implementation detail. Indexing them causes false positives — e.g.
            // a callee named "output" boosting `get_current_git_ref` for queries
            // about output. The Signature table (|Property|Value) is kept because
            // it contains meaningful tokens (param names, return types).
            if trimmed.starts_with("|Callee") {
                in_metadata_table = true;
            }
            // Skip indexing cells from metadata tables.
            if in_metadata_table {
                continue;
            }
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

    // Return anchor - either explicit or derived from filename
    if let Some(a) = anchor {
        return Some((a, path.to_path_buf(), counts));
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
                // Truncate by chars, not bytes: a raw `&snippet[..200]` byte slice
                // panics when byte 200 falls inside a multi-byte UTF-8 char (e.g. `→`).
                if snippet.chars().count() > 200 {
                    let truncated: String = snippet.chars().take(200).collect();
                    return format!("{truncated}...");
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

/// A source of text embeddings for dense (semantic) retrieval.
///
/// Implementations MUST be deterministic — the same input text always yields the
/// same vector — to preserve aden's reproducibility guarantee. The production
/// implementation will wrap a bundled, offline embedding model; tests use a
/// small hand-crafted provider. Kept as a trait so the heavy model dependency
/// stays out of the fusion core and can be swapped without touching callers.
pub trait EmbeddingProvider: Sync {
    /// Embed a single text into a fixed-length vector.
    fn embed(&self, text: &str) -> Vec<f32>;
    /// The dimensionality of the vectors this provider produces.
    fn dim(&self) -> usize;
}

/// Project a document's text to the part that determines its *meaning*, dropping
/// provenance/location attribute lines that gen rewrites on every run without a
/// semantic change: the `:last-verified:` timestamp (changes every gen), the
/// `:start_line:`/`:end_line:`/`:start_byte:`/`:end_byte:` span (shifts when
/// unrelated code above it moves), and the per-file `:source_hash:`. Embedding and
/// hashing this stable projection — rather than the raw text — is what lets the
/// content-addressed embedding cache actually hit across gens: without it every
/// symbol's hash changes on every reindex and the whole corpus re-embeds.
fn stable_embed_text(text: &str) -> String {
    const VOLATILE_ATTRS: &[&str] = &[
        ":last-verified:",
        ":start_line:",
        ":end_line:",
        ":start_byte:",
        ":end_byte:",
        ":source_hash:",
    ];
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim_start();
        if VOLATILE_ATTRS.iter().any(|attr| trimmed.starts_with(attr)) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The value of a `:attr:` line in a document, if present (first match).
fn doc_attr<'a>(text: &'a str, attr: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|l| l.trim_start().strip_prefix(attr))
        .map(str::trim)
}

/// Stable cache key for a document's embedding.
///
/// For code symbols, key on the symbol's `:source_hash:` (a hash of the actual
/// source, so it changes only when the code changes) plus the anchor (per-symbol
/// identity). This is immune to the noise that makes the rendered contract text
/// differ between otherwise-identical gens — the `:last-verified:` timestamp and
/// HashMap-ordered include/edge tables — which would otherwise bust the cache for
/// thousands of unchanged symbols on every reindex. Documents without a source
/// hash (prose: Markdown/AsciiDoc/`.txt`) fall back to hashing their stable text
/// projection.
/// Bumped whenever an embedding-model parameter changes the produced vectors
/// without the document text changing — e.g. `MAX_SEQ` (truncation length), the
/// model itself, or the pooling strategy. Folded into [`embed_key`] so such a
/// change busts the content-addressed embedding cache and forces a re-embed;
/// otherwise stale 128-token vectors would persist for every unchanged doc.
///   v1: implicit (pre-versioning).
///   v2: MAX_SEQ 128 -> 512 (full prose sections embed; CLS pooling, single
///       forward). A chunk + mean-pool variant was trialed and reverted: a Pro Git
///       A/B (2026-06-23) showed whole-doc mean-pooling was net-negative for
///       retrieval at both caps, so only the cap raise ships. Bumped past any
///       locally-built pooled cache to force a clean re-embed.
const EMBED_PARAM_VERSION: u64 = 4;

fn embed_key(anchor: &str, text: &str) -> u64 {
    let base = match doc_attr(text, ":source_hash:") {
        Some(source_hash) => format!("{source_hash}\u{1f}{anchor}"),
        None => stable_embed_text(text),
    };
    text_hash(&format!("v{EMBED_PARAM_VERSION}\u{1f}{base}"))
}

/// Stable 64-bit content hash (FNV-1a) used to detect whether a document's text
/// changed since its embedding was computed. Pure arithmetic, so it is
/// deterministic across runs and platforms — required for the reproducible,
/// incremental embedding path in [`Index::embed_documents`]. Not cryptographic;
/// a collision would at worst skip re-embedding a changed doc, which is
/// astronomically unlikely at 64 bits for this corpus scale.
fn text_hash(text: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Cosine similarity of two equal-length vectors, in `[-1.0, 1.0]`. Returns
/// `0.0` for a length mismatch or a zero-magnitude vector (no direction).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Map a raw dense cosine similarity to a cross-query CALIBRATED confidence in
/// `[0,1]` — "does this query have a genuinely good match at all?".
///
/// Rank-based fusion (RRF) discards magnitude, and every scale-free measure of a
/// within-query score distribution (separation, z-score, central tendency) reads
/// best-of-noise as a confident peak, so none of them separates an on-topic query
/// from an off-topic one. The only signal that does is the ABSOLUTE cosine of the
/// best match — but "absolute" only means something once anchored to the embedder's
/// own semantic band. For bge-small-en-v1.5 that band was measured on the
/// `assembly_ab` bench across Python (Flask) and Go (kin-openapi): on-topic best
/// cosines land `>= 0.72`, off-topic `<= 0.69`, consistent with the model's general
/// behavior (relevant pairs ~0.7-0.85, unrelated ~0.3-0.6). The ramp below maps that
/// band to `[0,1]` — a smooth transition, NOT a cliff, so confidence degrades
/// gracefully. The constants are a property of the EMBEDDER (re-validate if the
/// model changes), not of any one repo.
pub fn semantic_match_confidence(cosine: f32) -> f32 {
    /// Below this cosine, bge-small considers the best match clearly off-topic.
    const BAND_LO: f32 = 0.66;
    /// Above this cosine, clearly on-topic.
    const BAND_HI: f32 = 0.74;
    ((cosine - BAND_LO) / (BAND_HI - BAND_LO)).clamp(0.0, 1.0)
}

/// Reciprocal Rank Fusion — Cormack, Clarke & Buettcher, SIGIR 2009
/// ("Reciprocal rank fusion outperforms condorcet and individual rank learning
/// methods"). Fuses several ranked lists by summing `1 / (k + rank)` for each
/// item across the lists, where `rank` is 1-based.
///
/// RRF uses only *ranks*, never the underlying scores, so it combines retrievers
/// on different score scales (BM25 vs. cosine) without normalization — and is
/// robust to small cross-platform float drift in the dense scores, since only
/// the rank order matters. Output is sorted by fused score descending, ties
/// broken by item id, so the result is fully deterministic.
pub fn rrf_fuse(rankings: &[Vec<String>], k: f64) -> Vec<(String, f64)> {
    let mut fused: HashMap<String, f64> = HashMap::new();
    for ranking in rankings {
        for (idx, item) in ranking.iter().enumerate() {
            let rank = (idx + 1) as f64;
            *fused.entry(item.clone()).or_insert(0.0) += 1.0 / (k + rank);
        }
    }
    let mut out: Vec<(String, f64)> = fused.into_iter().collect();
    out.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    out
}

impl Index {
    /// Build an index from all `.adoc` and `.aden` files under `dir`.
    pub fn from_directory(dir: &Path) -> Result<Self, std::io::Error> {
        let mut files = Vec::new();
        let filter = AdenFilter::from_directory(dir);
        Self::collect_files(dir, &filter, &mut files)?;

        // Parallel: read every file as (path, text). Apply the same secret floor
        // the gen/lint paths use so a credential sitting in an on-disk doc/notes
        // file (.txt/.adoc/.md) never enters the searchable index, where `search`/
        // `ask` could surface it verbatim (CWE-200/798). Store-resident contracts
        // arrive via `ingest` from gen, which already filtered at the source.
        let entries: Vec<(PathBuf, String)> = files
            .par_iter()
            .filter(|path| {
                let rel = path.strip_prefix(dir).unwrap_or(path);
                !aden_core::filter::is_secret_path(rel)
            })
            .filter_map(|path| {
                std::fs::read_to_string(path)
                    .ok()
                    .map(|t| (path.clone(), t))
            })
            .filter(|(_, text)| !aden_core::filter::content_has_high_confidence_secret(text))
            .collect();

        let mut index = Index::default();
        index.ingest(entries);
        index.finalize();
        Ok(index)
    }

    /// Ingest a batch of `(source_path, text)` documents into the index.
    ///
    /// `source_path` is used only as the recorded location and (for `.txt`) as
    /// the anchor source — it does **not** have to exist on disk. This is what
    /// lets the index include contracts that live in the fjall store rather than
    /// as files, so `search`/`ask` can see code symbols emitted by
    /// `aden gen --auto`. Anchors already present are left untouched (earlier
    /// ingestions win), so on-disk contracts take precedence over store copies.
    ///
    /// Call [`Index::finalize`] once after all `ingest` calls to recompute the
    /// BM25 average document length.
    pub fn ingest(&mut self, entries: Vec<(PathBuf, String)>) {
        let mut parsed: Vec<_> = entries
            .into_par_iter()
            .filter_map(|(path, text)| parse_adoc(&path, &text).map(|p| (p, text)))
            .collect();

        // Determinism: the dedup below is "first occurrence wins", and `entries` may
        // arrive in non-deterministic parallel order (file walk / store iteration). If
        // an anchor appears more than once with differing token counts, which copy wins
        // — and thus `doc_lengths`/`avg_doc_length` and every downstream BM25 score —
        // would otherwise vary run-to-run. Sort by (anchor, source_path) so the index is
        // byte-identical regardless of collection order. (Same class as the
        // `detect_communities` sort-before-Louvain determinism fix.)
        parsed.sort_by(|a, b| a.0.0.cmp(&b.0.0).then_with(|| a.0.1.cmp(&b.0.1)));

        for ((anchor, source_path, counts), text) in parsed {
            if self.doc_lengths.contains_key(&anchor) {
                continue; // already indexed (e.g. an on-disk copy) — don't double count
            }
            self.anchor_paths.insert(anchor.clone(), source_path);
            let doc_len: usize = counts.values().sum();
            self.doc_lengths.insert(anchor.clone(), doc_len);
            for (token, count) in &counts {
                self.inverted
                    .entry(token.clone())
                    .or_default()
                    .push((anchor.clone(), *count));
            }
            self.anchor_text.insert(anchor, text);
        }
    }

    /// Indexed token count for an anchor's document (0 if unknown). A proxy for
    /// substantiveness: a tiny count means a thin stub (abstract method, shim),
    /// which `ask` routing should pass over in favour of the symbol that carries
    /// the real content.
    pub fn doc_token_count(&self, anchor: &str) -> usize {
        self.doc_lengths.get(anchor).copied().unwrap_or(0)
    }

    /// Whether the index has any posting for `term` (an already-tokenized term). Used by
    /// query-time lexicon expansion to GROUND candidate synonyms to the corpus vocabulary, so
    /// a dictionary only ever adds words the corpus actually uses (the WSD-noise fix the prose
    /// ablation validated: dictionary synonyms absent from the corpus are dropped before they
    /// can mislead ranking).
    pub fn knows_term(&self, term: &str) -> bool {
        self.inverted.contains_key(term)
    }

    /// Document frequency of an already-tokenized `term` (number of postings). 0 if absent.
    pub fn doc_frequency(&self, term: &str) -> usize {
        self.inverted.get(term).map_or(0, |v| v.len())
    }

    /// Number of indexed documents — the denominator for document-frequency bands.
    pub fn corpus_len(&self) -> usize {
        self.doc_lengths.len()
    }

    /// Whether `term` is *discriminative*: its document frequency sits in the band
    /// `MIN_DF..=MAX_DF_FRAC*N` — frequent enough to not be hapax noise, rare enough
    /// to not be a ubiquitous stop-word-like token. This is the SAME band `ppmi_rerank`
    /// uses; query-time lexicon expansion reuses it so a dictionary can only inject
    /// synonyms that actually narrow the result set, never high-frequency common-word
    /// synonyms (the noise that regressed external prose/code retrieval when expansion
    /// grounded on mere presence). Mirrors `ppmi_rerank`'s `MIN_DF` / `MAX_DF_FRAC`.
    pub fn term_is_discriminative(&self, term: &str) -> bool {
        const MIN_DF: usize = 3;
        const MAX_DF_FRAC: f64 = 0.20;
        let n = self.corpus_len();
        if n == 0 {
            return false;
        }
        let df = self.doc_frequency(term);
        let max_df = (MAX_DF_FRAC * n as f64) as usize;
        df >= MIN_DF && df <= max_df.max(MIN_DF)
    }

    /// Fraction of indexed anchors that are CODE symbols (the `aden://module/...` scheme), vs
    /// prose/doc anchors (`aden://doc/...`). A cheap corpus-substrate signal for auto-gating the
    /// dual-substrate retrieval levers: a code-bearing corpus benefits from the PPMI rerank even
    /// for natural-language queries (the NL-over-code case), a prose corpus does not. Returns 0.0
    /// for an empty index.
    pub fn code_anchor_fraction(&self) -> f64 {
        if self.anchor_text.is_empty() {
            return 0.0;
        }
        let code = self
            .anchor_text
            .keys()
            .filter(|a| a.contains("://module") || a.contains("://symbol"))
            .count();
        code as f64 / self.anchor_text.len() as f64
    }

    /// Recompute the BM25 average document length. Call once after ingestion.
    pub fn finalize(&mut self) {
        let total_len: usize = self.doc_lengths.values().sum();
        let doc_count = self.doc_lengths.len();
        self.avg_doc_length = if doc_count > 0 {
            total_len as f64 / doc_count as f64
        } else {
            1.0
        };
        // Stamp the tokenizer version. Both build paths (`from_directory` and the
        // store-merge rebuild in `load_or_build_index`) end in `finalize`, so this
        // single site covers every freshly-built index that gets cached.
        self.version = CURRENT_INDEX_VERSION;
    }

    fn collect_files(
        dir: &Path,
        filter: &AdenFilter,
        out: &mut Vec<PathBuf>,
    ) -> Result<(), std::io::Error> {
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
                    && filter.should_skip(rel)
                {
                    continue;
                }
                Self::collect_files_inner(&path, root, filter, out)?;
            } else if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                // On-disk source for the search index: AsciiDoc only (.adoc/.aden).
                // `.txt` is deliberately NOT indexed file-level here: gen emits one
                // paragraph `Note` per `.txt` paragraph into the store, and
                // load_or_build_index merges those paragraph-granular nodes in.
                // Indexing the whole file here too duplicated that coverage with a
                // coarser, less-dense blob (the `note` vs `note.txt#p1` pair); the
                // store's paragraph nodes are now the canonical .txt coverage.
                if ext == "adoc" || ext == "aden" {
                    // For files in different directories, skip prefix check (security: only for project root)
                    let parent = path
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| path.clone());
                    let is_cross_dir = root != parent;
                    if !is_cross_dir
                        && let Ok(rel) = path.strip_prefix(root)
                        && filter.should_skip(rel)
                    {
                        continue;
                    }
                    out.push(path);
                }
            }
        }
        Ok(())
    }

    /// Helper: Add BM25 scores for a single token.
    ///
    /// Also records, in `coverage`, that this (distinct) query token matched the
    /// anchor — i.e. the per-document *coordination level*. The coverage count
    /// feeds the multi-token boost in [`Index::query`], which keeps a single rare
    /// high-IDF term from dominating a document that covers more of the query.
    fn add_bm25_scores(
        &self,
        token: &str,
        n: f64,
        scores: &mut HashMap<String, f64>,
        coverage: &mut HashMap<String, usize>,
    ) {
        if let Some(postings) = self.inverted.get(token) {
            let df = postings.len() as f64;
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln().max(0.0);

            for (anchor, tf) in postings {
                let doc_len = self.doc_lengths.get(anchor).copied().unwrap_or(1);
                let tf_normalized = (*tf as f64 * (BM25_K1 + 1.0))
                    / (*tf as f64
                        + BM25_K1 * (1.0 - BM25_B + BM25_B * doc_len as f64 / self.avg_doc_length));
                *scores.entry(anchor.clone()).or_insert(0.0) += idf * tf_normalized;
                // One increment per distinct query token (postings hold each
                // anchor once per token), so this is the count of distinct query
                // terms the document matches.
                *coverage.entry(anchor.clone()).or_insert(0) += 1;
            }
        }
    }

    /// Query the index with semantic normalization and BM25 scoring.
    pub fn query(&self, query_str: &str) -> Vec<SearchResult> {
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

        let mut scores: HashMap<String, f64> = HashMap::new();
        // anchor -> number of DISTINCT query tokens it matched (coordination level).
        let mut coverage: HashMap<String, usize> = HashMap::new();

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
            for (anchor, score) in scores.iter_mut() {
                let matched = coverage.get(anchor).copied().unwrap_or(1);
                if matched > 1 {
                    *score *= 1.0 + COVERAGE_WEIGHT * (matched - 1) as f64;
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
            .filter(|t| !is_stop_word(t))
            .map(stem)
            .collect();
        for (anchor, score) in scores.iter_mut() {
            let anchor_lower = anchor.to_lowercase();
            let source_path = self.anchor_paths.get(anchor);

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
            // Tokens are stemmed (see `significant_query_tokens`), so an inflected
            // query still matches the anchor slug (substring match is unaffected —
            // a stem is a prefix of its surface form).
            // Anchor boost requires a *token-boundary* match, not a raw substring.
            // A stemmed query token must equal one of the anchor's own tokens (or
            // be a prefix of it ending on a sub-token boundary) so `run` no longer
            // boosts `runtime` and `deliv` no longer boosts unrelated slugs.
            let anchor_match = if is_symbol {
                // Match on the symbol name fragment only, with significant tokens only.
                if let Some(fragment) = anchor.rsplit('#').next() {
                    let frag_lower = fragment.to_lowercase();
                    significant_query_tokens
                        .iter()
                        .any(|t| token_boundary_match(&frag_lower, t))
                } else {
                    false
                }
            } else {
                significant_query_tokens
                    .iter()
                    .any(|t| token_boundary_match(&anchor_lower, t))
            };

            // Title match (first line of document text). Uses the same stemmed,
            // stop-word-filtered tokens and the same token-boundary rule so the
            // title boost is consistent with the anchor boost and BM25.
            let title_match = self
                .anchor_text
                .get(anchor)
                .and_then(|text| text.lines().next())
                .map(|first_line| {
                    let first_line_lower = first_line.to_lowercase();
                    significant_query_tokens
                        .iter()
                        .any(|t| token_boundary_match(&first_line_lower, t))
                })
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

    /// Populate per-anchor dense embeddings from each document's text using the
    /// given provider. The vectors are stored (and serialized) so queries don't
    /// re-embed the corpus.
    ///
    /// *Incremental*: only documents whose text changed since their stored vector
    /// was computed are (re)embedded — tracked by a per-anchor content hash
    /// ([`Index::embedding_hashes`]). Unchanged docs keep their vector; removed
    /// anchors are dropped. This is what keeps the reindex-on-read path cheap:
    /// editing one file in a large repo re-embeds one document, not the whole
    /// corpus (embedding is by far the dominant cost — bge inference on CPU runs
    /// into tens of minutes for tens of thousands of symbols). The first build on
    /// a fresh index still embeds everything; subsequent runs are proportional to
    /// the edit. Without this, [`Index::hybrid_query`] degrades to pure BM25.
    pub fn embed_documents(&mut self, provider: &dyn EmbeddingProvider) {
        use rayon::prelude::*;
        // Documents needing (re)embedding: no stored vector, or text changed since
        // the stored vector was computed. A vector with no recorded hash (a v7
        // cache loaded before the version bump) is trusted as current — but the
        // version bump means that case does not arise in practice.
        let stale: Vec<(String, String)> = self
            .anchor_text
            .iter()
            .filter(|(anchor, text)| match self.embedding_hashes.get(*anchor) {
                Some(stored) => *stored != embed_key(anchor, text),
                None => !self.embeddings.contains_key(*anchor),
            })
            .map(|(a, t)| (a.clone(), stable_embed_text(t)))
            .collect();

        // Embed only the stale set, in parallel (the dominant cost).
        let fresh: HashMap<String, Vec<f32>> = stale
            .par_iter()
            .map(|(anchor, text)| (anchor.clone(), provider.embed(text)))
            .collect();

        // Rebuild over the CURRENT doc set: take a fresh vector if we just made
        // one, else reuse the stored vector; anchors absent from `anchor_text`
        // (deleted symbols) fall out. Record each kept vector's stable key.
        let mut embeddings = HashMap::with_capacity(self.anchor_text.len());
        let mut hashes = HashMap::with_capacity(self.anchor_text.len());
        for (anchor, text) in &self.anchor_text {
            if let Some(vec) = fresh.get(anchor).or_else(|| self.embeddings.get(anchor)) {
                embeddings.insert(anchor.clone(), vec.clone());
                hashes.insert(anchor.clone(), embed_key(anchor, text));
            }
        }
        self.embeddings = embeddings;
        self.embedding_hashes = hashes;
    }

    /// Like [`Index::embed_documents`], but reuses a *content-addressed* embedding
    /// cache that survives index rebuilds.
    ///
    /// `gen` wipes the index cache on every run, which would otherwise force a full
    /// re-embed of the corpus on the next query (catastrophic on large repos — tens
    /// of minutes). This keeps the expensive vectors in a separate cache keyed by
    /// the document's content hash, so a rebuild (or even a renamed anchor) reuses
    /// every vector whose text is unchanged and embeds only what is genuinely new.
    /// Newly computed vectors are added to `cache` for the caller to persist.
    pub fn embed_documents_cached(
        &mut self,
        provider: &dyn EmbeddingProvider,
        cache: &mut HashMap<String, Vec<f32>>,
    ) {
        use rayon::prelude::*;
        // Distinct content hashes not yet in the cache → the only texts to embed.
        // Dedup by hash so identical content (common for boilerplate) embeds once.
        let mut seen = std::collections::HashSet::new();
        let missing: Vec<(String, String)> = self
            .anchor_text
            .iter()
            .map(|(anchor, text)| (embed_key(anchor, text).to_string(), stable_embed_text(text)))
            .filter(|(key, _)| !cache.contains_key(key) && seen.insert(key.clone()))
            .collect();

        let fresh: Vec<(String, Vec<f32>)> = missing
            .par_iter()
            .map(|(key, text)| (key.clone(), provider.embed(text)))
            .collect();
        cache.extend(fresh);

        // Project the cache onto the current anchors by each symbol's stable key.
        let mut embeddings = HashMap::with_capacity(self.anchor_text.len());
        let mut hashes = HashMap::with_capacity(self.anchor_text.len());
        for (anchor, text) in &self.anchor_text {
            let key = embed_key(anchor, text);
            if let Some(vec) = cache.get(&key.to_string()) {
                embeddings.insert(anchor.clone(), vec.clone());
                hashes.insert(anchor.clone(), key);
            }
        }
        self.embeddings = embeddings;
        self.embedding_hashes = hashes;
    }

    /// Whether dense embeddings are available (i.e. [`Index::embed_documents`]
    /// has been run). When false, hybrid retrieval is pure BM25.
    pub fn has_embeddings(&self) -> bool {
        !self.embeddings.is_empty()
    }

    /// Dense (semantic) ranking of indexed anchors for `query`, by cosine
    /// similarity against the stored embeddings. A flat scan — deterministic and
    /// dependency-free, with no ANN structure (fine up to tens of thousands of
    /// anchors; revisit with HNSW only if a corpus demands it). Empty when no
    /// embeddings are stored.
    pub fn dense_query(&self, query: &str, provider: &dyn EmbeddingProvider) -> Vec<SearchResult> {
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
                let source_path = self.anchor_paths.get(&anchor)?.clone();
                let text = self.anchor_text.get(&anchor)?;
                let snippet = build_snippet(text, &query_tokens);
                Some(SearchResult {
                    anchor,
                    source_path,
                    score: score as f64,
                    snippet,
                })
            })
            .collect()
    }

    /// Derive top-`top_k` nearest-neighbor anchor pairs by embedding cosine
    /// similarity — the raw material for `SimilarTo`/`RelatesTo` graph edges built
    /// from *semantic closeness* rather than authored cross-references. Only
    /// neighbors scoring at least `min_cosine` are kept. Empty when no embeddings
    /// are stored, so it is always safe to call (pure BM25 corpora derive nothing).
    ///
    /// Returns directed `(src, dst, score)` triples — each `src` paired with its
    /// strongest neighbors. `SimilarTo` is symmetric, so callers typically write
    /// both directions (graph edge dedup collapses the repeats).
    ///
    /// `anchor_prefix` restricts the scan to anchors with that URI prefix (e.g.
    /// `"aden://doc/"` for prose-only). The index co-locates code and prose
    /// embeddings, so without this the O(n²·dim) cross-product runs over every
    /// code symbol on a code-heavy repo and the caller throws nearly all of it
    /// away — filtering BEFORE the scan keeps `n` at the prose-doc count. Pass
    /// `None` to compare all anchors.
    ///
    /// A flat O(n²·dim) scan like [`dense_query`] — deterministic (anchors are
    /// processed in sorted order; neighbors ranked by score desc, then anchor
    /// asc) and dependency-free. Fine up to tens of thousands of anchors; revisit
    /// with an ANN index only if a corpus demands it.
    pub fn similar_pairs(
        &self,
        top_k: usize,
        min_cosine: f32,
        anchor_prefix: Option<&str>,
    ) -> Vec<(String, String, f32)> {
        if self.embeddings.is_empty() || top_k == 0 {
            return Vec::new();
        }
        let mut anchors: Vec<String> = self.embeddings.keys().cloned().collect();
        if let Some(prefix) = anchor_prefix {
            anchors.retain(|a| a.starts_with(prefix));
        }
        anchors.sort_unstable();
        let mut out: Vec<(String, String, f32)> = Vec::new();
        for src in &anchors {
            let Some(src_vec) = self.embeddings.get(src) else {
                continue;
            };
            let mut scored: Vec<(&String, f32)> = anchors
                .iter()
                .filter(|dst| dst.as_str() != src.as_str())
                .filter_map(|dst| {
                    let v = self.embeddings.get(dst)?;
                    let s = cosine_similarity(src_vec, v);
                    (s >= min_cosine).then_some((dst, s))
                })
                .collect();
            scored.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(b.0))
            });
            scored.truncate(top_k);
            for (dst, s) in scored {
                out.push((src.clone(), dst.clone(), s));
            }
        }
        out
    }

    /// Hybrid retrieval: fuse the BM25 and dense rankings with Reciprocal Rank
    /// Fusion (see [`rrf_fuse`]). The two retrievers are complementary — BM25
    /// nails exact identifiers and rare terms, dense captures meaning and
    /// paraphrase — and RRF combines them with no score normalization. Degrades
    /// to pure BM25 when no embeddings are stored, so it is always safe to call.
    pub fn hybrid_query(&self, query: &str, provider: &dyn EmbeddingProvider) -> Vec<SearchResult> {
        let bm25 = self.query(query);
        if self.embeddings.is_empty() {
            return bm25;
        }
        let dense = self.dense_query(query, provider);

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

    /// Rerank the top-`top_k` of a base result list by corpus PPMI co-occurrence between the
    /// query terms and each candidate document's terms. This is a purely CORPUS-DERIVED signal
    /// (positive pointwise mutual information over the index's own postings), with no external
    /// dictionary: the lexical-merge ablation showed dictionaries dilute it, while PPMI alone
    /// lifted code retrieval over the hybrid base (compound_ab A/B: MRR 0.216 -> 0.289). Terms
    /// are df-gated (`MIN_DF..=20% of corpus`) to drop both hapax noise and ubiquitous tokens.
    /// The tail past `top_k` keeps its base order. Returns `base` unchanged when there is no
    /// usable signal (empty corpus/base, or no in-band query term).
    pub fn ppmi_rerank(
        &self,
        query: &str,
        base: Vec<SearchResult>,
        top_k: usize,
    ) -> Vec<SearchResult> {
        const MIN_DF: usize = 3;
        const MAX_DF_FRAC: f64 = 0.20;
        const W_REL: f64 = 2.0;

        let n = self.doc_lengths.len();
        if n == 0 || base.is_empty() {
            return base;
        }
        let max_df = (MAX_DF_FRAC * n as f64) as usize;
        let df = |t: &str| self.inverted.get(t).map_or(0, |v| v.len());
        let in_band = |t: &str| (MIN_DF..=max_df).contains(&df(t));
        let posting_set = |t: &str| -> std::collections::HashSet<&str> {
            self.inverted
                .get(t)
                .map(|v| v.iter().map(|(a, _)| a.as_str()).collect())
                .unwrap_or_default()
        };

        // Posting sets for the in-band query terms (computed once).
        let q_sets: Vec<std::collections::HashSet<&str>> = tokenize(query)
            .into_iter()
            .filter(|t| in_band(t))
            .map(|t| posting_set(&t))
            .collect();
        if q_sets.is_empty() {
            return base;
        }

        // PPMI of two terms via their posting (anchor) sets: max(0, log2(co*n / (dfa*dfb))).
        let ppmi = |qset: &std::collections::HashSet<&str>, ct: &str| -> f64 {
            let clen = df(ct);
            if clen == 0 || qset.is_empty() {
                return 0.0;
            }
            let cset = posting_set(ct);
            let (small, big) = if qset.len() <= cset.len() {
                (qset, &cset)
            } else {
                (&cset, qset)
            };
            let co = small.iter().filter(|x| big.contains(*x)).count();
            if co == 0 {
                return 0.0;
            }
            ((co as f64 * n as f64) / (qset.len() as f64 * clen as f64))
                .log2()
                .max(0.0)
        };

        let k = top_k.min(base.len());
        let mut head: Vec<(usize, f64)> = (0..k)
            .map(|i| {
                let card: std::collections::HashSet<String> = self
                    .anchor_text
                    .get(&base[i].anchor)
                    .map(|t| tokenize(t).into_iter().collect())
                    .unwrap_or_default();
                // Each query term scores its single best (max-PPMI) in-band card term; sum over
                // query terms. Mirrors the validated `rel_score` minus the dictionary bonus.
                let rel: f64 = q_sets
                    .iter()
                    .map(|qset| {
                        card.iter()
                            .filter(|ct| in_band(ct))
                            .map(|ct| ppmi(qset, ct))
                            .fold(0.0_f64, f64::max)
                    })
                    .sum();
                (i, rel)
            })
            .collect();

        let max_rel = head.iter().map(|(_, r)| *r).fold(0.0_f64, f64::max);
        if max_rel <= 0.0 {
            return base; // no co-occurrence signal in the window
        }
        // Blend base rank (1/(rank)) with normalized relevance; reorder the window only.
        head.sort_by(|a, b| {
            let sa = 1.0 / (a.0 + 1) as f64 + W_REL * a.1 / max_rel;
            let sb = 1.0 / (b.0 + 1) as f64 + W_REL * b.1 / max_rel;
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut out: Vec<SearchResult> = head.into_iter().map(|(i, _)| base[i].clone()).collect();
        out.extend(base.into_iter().skip(k));
        out
    }
}

// Tests live here, mid-file, with the semantic-normalization helpers below them
// kept adjacent to the code they support; relocating them adds churn for no gain.
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Serializes tests that mutate the process-global `ADEN_DATA_DIR`. Without
    /// it, parallel cache tests interleave set/remove of the env var and resolve
    /// `save`/`try_load` to different cache dirs (flaky, surfaced on Windows CI).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Pin `ADEN_DATA_DIR` to an isolated tempdir for the duration of a cache
    /// test, holding `ENV_LOCK` so no other test mutates it concurrently. The
    /// returned guard restores the previous value (and releases the lock) on drop.
    fn isolated_data_dir() -> (tempfile::TempDir, impl Drop) {
        struct Restore {
            prev: Option<std::ffi::OsString>,
            _lock: std::sync::MutexGuard<'static, ()>,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                match self.prev.take() {
                    Some(v) => unsafe { std::env::set_var("ADEN_DATA_DIR", v) },
                    None => unsafe { std::env::remove_var("ADEN_DATA_DIR") },
                }
            }
        }
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("ADEN_DATA_DIR");
        let data = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ADEN_DATA_DIR", data.path()) };
        (data, Restore { prev, _lock: lock })
    }

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
    fn build_snippet_truncates_multibyte_without_panicking() {
        // Regression: a raw `&snippet[..200]` byte slice panicked when byte 200
        // landed inside a multi-byte UTF-8 char. Build a long line whose 200th
        // char boundary is straddled by `→` (3 bytes) and assert no panic.
        let mut line = "x".repeat(199);
        line.push('→'); // char #200 is multi-byte, its bytes span the old cut point
        line.push_str(&"y".repeat(50));
        let snippet = build_snippet(&line, &["x".to_string()]);
        assert!(snippet.ends_with("..."));
        // 200 chars kept + "..."; must be valid UTF-8 (String guarantees it).
        assert_eq!(snippet.chars().filter(|c| *c != '.').count(), 200);
        assert!(snippet.contains('→'));
    }

    #[test]
    fn build_snippet_short_line_unchanged() {
        let snippet = build_snippet("a short → line", &["short".to_string()]);
        assert_eq!(snippet, "a short → line");
    }

    /// An [`EmbeddingProvider`] that counts how many times it embeds, so a test can
    /// assert that re-embedding only touches changed documents.
    struct CountingEmbedder {
        calls: std::sync::atomic::AtomicUsize,
    }
    impl CountingEmbedder {
        fn new() -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn count(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::Relaxed)
        }
    }
    impl EmbeddingProvider for CountingEmbedder {
        fn embed(&self, text: &str) -> Vec<f32> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Deterministic 4-d vector derived from the text's content hash.
            let h = text_hash(text);
            (0..4)
                .map(|i| ((h >> (i * 8)) & 0xff) as f32 / 255.0)
                .collect()
        }
        fn dim(&self) -> usize {
            4
        }
    }

    #[test]
    fn embed_documents_only_reembeds_changed_docs() {
        let mut index = Index::default();
        index.ingest(vec![
            (PathBuf::from("a.adoc"), "[[a]]\nalpha text".to_string()),
            (PathBuf::from("b.adoc"), "[[b]]\nbeta text".to_string()),
            (PathBuf::from("c.adoc"), "[[c]]\ngamma text".to_string()),
        ]);
        index.finalize();
        let emb = CountingEmbedder::new();

        // First build embeds every document.
        index.embed_documents(&emb);
        assert_eq!(emb.count(), 3, "first build embeds all docs");
        assert_eq!(index.embeddings.len(), 3);
        assert_eq!(index.embedding_hashes.len(), 3);

        // Re-embedding an unchanged corpus does zero work (the regression this
        // whole change exists to prevent: a full re-embed on every read).
        index.embed_documents(&emb);
        assert_eq!(emb.count(), 3, "unchanged corpus must not re-embed");

        // Change one document's text -> exactly one re-embed; its vector updates.
        let old_a = index.embeddings["a"].clone();
        index
            .anchor_text
            .insert("a".to_string(), "alpha text CHANGED".to_string());
        index.embed_documents(&emb);
        assert_eq!(emb.count(), 4, "one changed doc -> one re-embed");
        assert_ne!(
            index.embeddings["a"], old_a,
            "changed doc got a fresh vector"
        );
        assert_eq!(index.embeddings.len(), 3);

        // Removing a document drops its vector and embeds nothing.
        index.anchor_text.remove("b");
        index.embed_documents(&emb);
        assert_eq!(emb.count(), 4, "removal embeds nothing");
        assert!(!index.embeddings.contains_key("b"));
        assert!(!index.embedding_hashes.contains_key("b"));
        assert_eq!(index.embeddings.len(), 2);
    }

    #[test]
    fn similar_pairs_links_nearest_neighbors() {
        let mut index = Index::default();
        // Hand-set embeddings: a and b nearly identical; c orthogonal to both.
        index.embeddings.insert("a".to_string(), vec![1.0, 0.0]);
        index.embeddings.insert("b".to_string(), vec![0.99, 0.141]);
        index.embeddings.insert("c".to_string(), vec![0.0, 1.0]);

        let pairs = index.similar_pairs(1, 0.5, None);
        // a's nearest is b and b's nearest is a (symmetric); c has no neighbor
        // above the 0.5 cosine threshold.
        assert!(pairs.iter().any(|(s, d, _)| s == "a" && d == "b"));
        assert!(pairs.iter().any(|(s, d, _)| s == "b" && d == "a"));
        assert!(
            !pairs.iter().any(|(s, _, _)| s == "c"),
            "c has no above-threshold neighbor"
        );
        // top_k = 1 -> at most one neighbor per source.
        assert!(pairs.iter().filter(|(s, _, _)| s == "a").count() <= 1);
    }

    #[test]
    fn similar_pairs_empty_without_embeddings() {
        let index = Index::default();
        assert!(index.similar_pairs(5, 0.0, None).is_empty());
    }

    #[test]
    fn embed_documents_cached_reuses_across_rebuilds() {
        // Simulate the real CLI path: `gen` wipes the index, so each query rebuilds
        // a fresh Index. The content-addressed cache must let the rebuild reuse
        // every vector whose source is unchanged, embedding only what's new.
        let docs = vec![
            (
                PathBuf::from("a.adoc"),
                ":source_hash: AAA\n[[a]]\nalpha".to_string(),
            ),
            (
                PathBuf::from("b.adoc"),
                ":source_hash: BBB\n[[b]]\nbeta".to_string(),
            ),
        ];
        let emb = CountingEmbedder::new();
        let mut cache = HashMap::new();

        let mut idx1 = Index::default();
        idx1.ingest(docs.clone());
        idx1.finalize();
        idx1.embed_documents_cached(&emb, &mut cache);
        assert_eq!(emb.count(), 2, "cold cache embeds both");
        assert_eq!(idx1.embeddings.len(), 2);

        // A noisy re-render of the SAME sources: the volatile `:last-verified:`
        // timestamp differs, the source hashes do not. A from-scratch rebuild must
        // reuse both vectors via the cache (zero new embeds) — the whole point.
        let rerendered = vec![
            (
                PathBuf::from("a.adoc"),
                ":last-verified: 2026-01-02T00:00:00Z\n:source_hash: AAA\n[[a]]\nalpha".to_string(),
            ),
            (
                PathBuf::from("b.adoc"),
                ":last-verified: 2026-09-09T09:09:09Z\n:source_hash: BBB\n[[b]]\nbeta".to_string(),
            ),
        ];
        let mut idx2 = Index::default();
        idx2.ingest(rerendered);
        idx2.finalize();
        idx2.embed_documents_cached(&emb, &mut cache);
        assert_eq!(
            emb.count(),
            2,
            "rebuild with unchanged sources re-embeds nothing"
        );
        assert_eq!(idx2.embeddings.len(), 2);

        // One source actually changes (new source hash) -> exactly one new embed.
        let changed = vec![
            (
                PathBuf::from("a.adoc"),
                ":source_hash: AAA2\n[[a]]\nalpha rewritten".to_string(),
            ),
            (
                PathBuf::from("b.adoc"),
                ":source_hash: BBB\n[[b]]\nbeta".to_string(),
            ),
        ];
        let mut idx3 = Index::default();
        idx3.ingest(changed);
        idx3.finalize();
        idx3.embed_documents_cached(&emb, &mut cache);
        assert_eq!(emb.count(), 3, "only the changed source re-embeds");
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
    fn tokenize_expands_compound_identifiers() {
        // A snake_case symbol must index its components so a sub-word query
        // ("dispatch") matches it — the core ask-routing fix. The full token is
        // kept too so exact identifier queries still hit a strong posting.
        let tokens = tokenize("dispatch_request");
        assert!(
            tokens.contains(&"dispatch_request".to_string()),
            "full token preserved: {tokens:?}"
        );
        assert!(tokens.contains(&"dispatch".to_string()), "{tokens:?}");
        assert!(tokens.contains(&"request".to_string()), "{tokens:?}");

        // camelCase humps split the same way.
        let camel = tokenize("dispatchRequest");
        assert!(camel.contains(&"dispatch".to_string()), "{camel:?}");
        assert!(camel.contains(&"request".to_string()), "{camel:?}");

        // Dotted qualified names split on '.' too.
        let dotted = tokenize("Flask.dispatch_request");
        assert!(dotted.contains(&"dispatch".to_string()), "{dotted:?}");
        assert!(dotted.contains(&"flask".to_string()), "{dotted:?}");

        // Plain prose words are unchanged: one stem, no spurious sub-tokens.
        let prose = tokenize("routing");
        assert_eq!(prose, vec!["rout".to_string()]);
    }

    #[test]
    fn stem_collapses_plurals() {
        // Simple -s plurals converge on the singular.
        assert_eq!(stem("overlays"), "overlay");
        assert_eq!(stem("overlay"), "overlay");
        assert_eq!(stem("readers"), "reader");
        assert_eq!(stem("reader"), "reader");
        // Porter2 normalises -y/-ies to the same base ("deliveri"); both forms
        // converge so query/index agree even though the stem differs from the
        // surface singular.
        assert_eq!(stem("deliveries"), stem("delivery"));
        assert_eq!(stem("deliveries"), "deliveri");
    }

    #[test]
    fn stem_consonant_cluster_words_stay_whole_and_match_their_plural() {
        // The bare `-s` rule must not strip words ending in -ss/-us/-is; the
        // singular must stay whole AND converge with its `-es`-stripped plural.
        for (singular, plural) in [
            // -ss
            ("process", "processes"),
            ("access", "accesses"),
            ("address", "addresses"),
            ("class", "classes"),
            ("success", "successes"),
            // -us
            ("status", "statuses"),
            ("focus", "focuses"),
            ("nexus", "nexuses"),
            // -is words are now stemmed by Porter2 (analysis→analysi etc.);
            // see stem_is_words_stemmed_consistently for those cases.
        ] {
            assert_eq!(stem(singular), singular, "`{singular}` must stay whole");
            assert_eq!(
                stem(singular),
                stem(plural),
                "`{singular}`/`{plural}` must stem to the same token"
            );
        }
    }

    #[test]
    fn stem_is_words_stemmed_consistently() {
        // Porter2 strips the trailing -s from -is words (analysis→analysi,
        // basis→basi, thesis→thesi). The stems are short but valid; the same
        // token is produced for both singular and inflected query forms so
        // search still works. The old guard that kept these unchanged is gone —
        // they now benefit from the same normalisation as other words.
        assert_eq!(stem("analysis"), "analysi");
        assert_eq!(stem("basis"), "basi");
        assert_eq!(stem("thesis"), "thesi");
        // -es plurals of -is nouns must converge to the same stem as the
        // singular (Porter2 alone gives analyses→"analys" ≠ analysis→"analysi").
        assert_eq!(stem("analyses"), stem("analysis"));
        assert_eq!(stem("theses"), stem("thesis"));
        assert_eq!(stem("crises"), stem("crisis"));
        // Common -ses words must NOT be rewritten to -sis.
        assert_eq!(stem("cases"), stem("case"));
        assert_eq!(stem("phrases"), stem("phrase"));
        // Verify short-base verb consonant-doubling is now fixed (L4).
        assert_eq!(stem("running"), "run");
        assert_eq!(stem("mapping"), "map");
        assert_eq!(stem("logging"), "log");
        assert_eq!(stem("running"), stem("run"));
    }

    #[test]
    fn stem_collapses_verb_and_adverb_inflections() {
        // Porter2 strips -ing/-ed and then applies further suffix rules.
        // deliver* → "deliv" (Porter2 also strips the -er suffix in R2).
        assert_eq!(stem("delivering"), "deliv");
        assert_eq!(stem("delivered"), "deliv");
        // Inflections of the same verb still converge (both → "deliv").
        assert_eq!(stem("delivering"), stem("delivered"));
        assert_eq!(stem("quickly"), "quick");
        // render* → "render" (the -er suffix is not in R2 for "render").
        assert_eq!(stem("rendering"), "render");
        assert_eq!(stem("rendered"), "render");
        assert_eq!(stem("rendering"), stem("rendered"));
    }

    #[test]
    fn stem_short_words_not_over_reduced() {
        // Porter2 has its own length and R1-region guards; very short words
        // that would be mangled are left intact.
        assert_eq!(stem("ring"), "ring"); // stem of "r" has no vowel → no strip
        assert_eq!(stem("bus"), "bus"); // Porter2 R1-region check prevents "bu"
        // Porter2 applies its suffix rules more aggressively than the old
        // hand-rolled stemmer, producing correct base forms:
        assert_eq!(stem("based"), "base"); // -ed stripped, leaving the stem
        assert_eq!(stem("oily"), "oili"); // y→i normalisation (Porter2 step 1c)
    }

    #[test]
    fn stem_skips_non_alphabetic_tokens() {
        // Code identifiers and normalized numerics must pass through verbatim so
        // they keep matching their indexed forms and SemanticNormalizer outputs.
        assert_eq!(stem("fold_overlay"), "fold_overlay");
        assert_eq!(stem("5"), "5");
        assert_eq!(stem("05"), "05");
        assert_eq!(stem("utf8"), "utf8");
    }

    #[test]
    fn tokenize_and_query_stem_consistently() {
        // The whole point: an inflected query token matches the stemmed posting.
        let indexed = tokenize("The overlay is delivered to every reader");
        assert!(indexed.contains(&"overlay".to_string()));
        assert!(indexed.contains(&"deliv".to_string())); // Porter2: delivered → deliv
        assert!(indexed.contains(&"reader".to_string()));

        // A distractor that mentions "overlay" only incidentally (inside a grep
        // example) — without stem-consistent ranking this used to outrank the real
        // doc for the vague query.
        let dir2 = temp_dir_with_files(&[
            (
                "overlay.adoc",
                "[[overlay-delivery]]\n= Overlay Delivery\n\nThe overlay is delivered to every reader. Overlay delivery folds each contract into the assembled view so the reader sees one document.\n",
            ),
            (
                "grep.adoc",
                "[[grep-examples]]\n= Grep Examples\n\nRun a search across the tree. Example: grep -r overlay . to find matches. This page is about command-line search recipes.\n",
            ),
        ]);
        let index = Index::from_directory(dir2.path()).unwrap();
        // Inflected, plural query form routes to the doc about the singular topic,
        // and ranks it FIRST — the anchor/title boost is stem-consistent, so
        // "overlays"/"delivered"/"readers" boost the "overlay-delivery" anchor.
        let results = index.query("how do overlays get delivered to readers");
        assert_eq!(
            results.first().map(|r| r.anchor.as_str()),
            Some("overlay-delivery"),
            "stemmed query should rank the overlay-delivery doc first, got: {:?}",
            results.iter().map(|r| &r.anchor).collect::<Vec<_>>()
        );
    }

    #[test]
    fn query_orders_score_ties_deterministically() {
        // Two documents identical except their anchor name produce the SAME BM25
        // score for a shared term. Without a tiebreak the order followed arbitrary
        // HashMap iteration (flipping `ask` routing run-to-run); the lexicographic
        // secondary key must put "aaa-doc" before "zzz-doc" every time.
        let dir = temp_dir_with_files(&[
            ("zzz.adoc", "[[zzz-doc]]\n= Topic\n\nwidget widget widget\n"),
            ("aaa.adoc", "[[aaa-doc]]\n= Topic\n\nwidget widget widget\n"),
        ]);
        let index = Index::from_directory(dir.path()).unwrap();
        let got: Vec<String> = index
            .query("widget")
            .into_iter()
            .map(|r| r.anchor)
            .collect();
        assert_eq!(
            got,
            vec!["aaa-doc".to_string(), "zzz-doc".to_string()],
            "tied scores must order by anchor name deterministically, got: {:?}",
            got
        );
        // Stable across repeated queries within the same process.
        let again: Vec<String> = index
            .query("widget")
            .into_iter()
            .map(|r| r.anchor)
            .collect();
        assert_eq!(got, again);
    }

    #[test]
    fn try_load_rejects_stale_version() {
        // A versionless (pre-stemming) cache deserializes with version 0 and must
        // be rejected so the index is rebuilt with the current tokenizer.
        let dir = tempfile::tempdir().unwrap();
        // ADR-003: caches live in the per-user data dir keyed per project. Pin it
        // to an isolated tempdir (under ENV_LOCK) so the test neither reads nor
        // pollutes the real user data dir, and cannot race a parallel cache test.
        let (_data, _guard) = isolated_data_dir();
        let cache_dir = aden_paths::cache_dir(dir.path());
        std::fs::create_dir_all(&cache_dir).unwrap();
        // Minimal valid Index JSON without a `version` field.
        std::fs::write(
            cache_dir.join("index-cache.json"),
            r#"{"inverted":{},"anchor_paths":{},"anchor_text":{},"doc_lengths":{},"avg_doc_length":1.0}"#,
        )
        .unwrap();
        assert!(
            try_load(dir.path()).is_none(),
            "stale versionless cache must be rejected"
        );

        // A freshly built + saved index carries the current version and loads back.
        let mut index = Index::default();
        index.finalize();
        assert_eq!(index.version, CURRENT_INDEX_VERSION);
        save(&index, dir.path()).unwrap();
        assert!(
            try_load(dir.path()).is_some(),
            "current-version cache must load"
        );
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
    fn test_fuzzy_matching() {
        let dir = temp_dir_with_files(&[(
            "test.adoc",
            r#"[[test-anchor]]
= Testing Module

This is a test document about modules.
"#,
        )]);

        let index = Index::from_directory(dir.path()).unwrap();

        // Exact match should work
        let results = index.query("test");
        assert!(!results.is_empty(), "Should find exact match");

        // Fuzzy match with typo (levenshtein distance <= 2)
        let results = index.query("tset"); // typo of "test"
        // Note: fuzzy matching may or may not find this depending on implementation
        // This test documents the expected behavior
        if !results.is_empty() {
            assert!(
                results.iter().any(|r| r.anchor == "test-anchor"),
                "Should find test-anchor with fuzzy match"
            );
        }
    }

    #[test]
    fn test_search_ranking_bm25() {
        let dir = temp_dir_with_files(&[
            (
                "high-relevance.adoc",
                r#"[[high]]
= Test Module

This document is all about testing and modules.
test test test module module
"#,
            ),
            (
                "low-relevance.adoc",
                r#"[[low]]
= Other Stuff

Just mentioning test once.
"#,
            ),
        ]);

        let index = Index::from_directory(dir.path()).unwrap();
        let results = index.query("test module");

        assert_eq!(results.len(), 2, "Should find both documents");

        // high-relevance should score higher due to more occurrences
        assert_eq!(
            results[0].anchor, "high",
            "Higher relevance document should rank first"
        );
        assert!(
            results[0].score > results[1].score,
            "Score should be higher for more relevant document: {} vs {}",
            results[0].score,
            results[1].score
        );
    }

    #[test]
    fn test_anchor_title_boost() {
        let dir = temp_dir_with_files(&[
            (
                "title-match.adoc",
                r#"[[exact]]
= ExactMatch Title

Some content here.
"#,
            ),
            (
                "content-match.adoc",
                r#"[[other]]
= Other Title

This mentions exactmatch in the content.
"#,
            ),
        ]);

        let index = Index::from_directory(dir.path()).unwrap();
        let results = index.query("exactmatch");

        // At least the title match should be found
        assert!(
            results.iter().any(|r| r.anchor == "exact"),
            "Should find title match document"
        );
    }

    #[test]
    fn test_index_persistence() {
        // Isolate the on-disk cache from the real data dir and from parallel
        // tests that mutate ADEN_DATA_DIR (the _data dir and _guard live until
        // end of scope).
        let (_data, _guard) = isolated_data_dir();
        let dir = temp_dir_with_files(&[(
            "test.adoc",
            r#"[[test-anchor]]
= Test

Hello world.
"#,
        )]);

        // Build index
        let index = Index::from_directory(dir.path()).unwrap();

        // Save to directory
        save(&index, dir.path()).unwrap();

        // Load from directory
        let loaded = try_load(dir.path()).unwrap();

        // Query should work on loaded index
        let results = loaded.query("hello");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].anchor, "test-anchor");
    }

    #[test]
    fn test_index_rebuild() {
        let dir = temp_dir_with_files(&[(
            "test.adoc",
            r#"[[test-anchor]]
= Test

Original content.
"#,
        )]);

        // Build initial index
        let index1 = Index::from_directory(dir.path()).unwrap();
        let results1 = index1.query("original");
        assert_eq!(results1.len(), 1);

        // Modify file
        std::fs::write(
            dir.path().join("test.adoc"),
            r#"[[test-anchor]]
= Test

New content after rebuild.
"#,
        )
        .unwrap();

        // Rebuild index
        let index2 = Index::from_directory(dir.path()).unwrap();
        let results2 = index2.query("new");
        assert_eq!(results2.len(), 1);

        // Old content should not be found
        let results_old = index2.query("original");
        assert!(
            results_old.is_empty(),
            "Old content should not be in rebuilt index"
        );
    }

    #[test]
    fn test_semantic_normalization() {
        // Test boolean normalization
        let bool_forms = SemanticNormalizer::normalize("yep");
        assert!(bool_forms.iter().any(|f| f == "true"), "yep -> true");

        let bool_forms2 = SemanticNormalizer::normalize("nope");
        assert!(bool_forms2.iter().any(|f| f == "false"), "nope -> false");

        // Test time normalization
        let time_forms = SemanticNormalizer::normalize("midnight");
        assert!(
            time_forms.iter().any(|f| f == "00:00" || f == "midnight"),
            "midnight normalization"
        );

        // Test number normalization
        let num_forms = SemanticNormalizer::normalize("5");
        assert!(
            num_forms.iter().any(|f| f == "fifth" || f == "5"),
            "5 -> fifth"
        );

        let num_forms2 = SemanticNormalizer::normalize("first");
        assert!(
            num_forms2.iter().any(|f| f == "1" || f == "first"),
            "first -> 1"
        );

        // Test month normalization
        let month_forms = SemanticNormalizer::normalize("May");
        assert!(month_forms.iter().any(|f| f == "5"), "May -> 5");
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
        assert!(
            results.is_empty(),
            "Should not find .agent/templates/ files"
        );

        let results = index.query("style guide");
        assert!(
            results.is_empty(),
            "Should not find .agent/templates/ files"
        );

        let results = index.query("agent context");
        assert!(results.is_empty(), "Should not find .agent/ files");

        let results = index.query("main");
        assert_eq!(results.len(), 1, "Should find docs/main.adoc");
        assert_eq!(results[0].anchor, "main-doc");
    }
}

// =============================================================================
// SEMANTIC NORMALIZATION - Bridging human intuition with computer data
// =============================================================================

/// Normalizes input to canonical forms for semantic matching.
/// This is the key to "knowing" that "May" = "5" = "05" = "May" = "fifth month"
pub struct SemanticNormalizer;

impl SemanticNormalizer {
    /// Normalize a query term to all possible canonical forms.
    pub fn normalize(term: &str) -> Vec<String> {
        let mut forms = vec![term.to_lowercase()];

        // Add number word forms
        if let Some(num) = Self::word_to_number(term) {
            forms.push(num.clone());
            forms.push(format!("{:02}", num.parse::<usize>().unwrap_or(0)));
            forms.push(num.parse::<usize>().unwrap_or(0).to_string());
        }

        // Add month aliases
        if let Some(month) = Self::month_to_number(term) {
            forms.push(month.clone());
            forms.push(format!("{:02}", month.parse::<usize>().unwrap_or(0)));
            forms.push(Self::number_to_month(&month).unwrap_or_default());
            forms.push(Self::number_to_month_name(&month).unwrap_or_default());
        }

        // Add ordinal forms
        if let Some(ordinal) = Self::ordinal_to_number(term) {
            forms.push(ordinal);
        }

        // Add time canonical forms (bidirectional: midnight -> 00:00, 5pm -> 17:00)
        if let Some(canonical) = Self::time_to_canonical(term) {
            forms.push(canonical.clone());
            // Also add reverse keywords (00:00 -> midnight)
            for kw in Self::canonical_to_keywords(&canonical) {
                if !forms.contains(&kw) {
                    forms.push(kw);
                }
            }
        }

        // Add boolean canonical forms (bidirectional: yep -> true, nope -> false)
        if let Some(canonical) = Self::bool_to_canonical(term) {
            forms.push(canonical.clone());
            // Also add reverse keywords (true -> yep, false -> nope)
            for kw in Self::bool_canonical_to_keywords(&canonical) {
                if !forms.contains(&kw) {
                    forms.push(kw);
                }
            }
        }

        forms
    }

    /// Convert word numbers to digits ("seven" -> "7", "five" -> "5")
    fn word_to_number(s: &str) -> Option<String> {
        let words: HashMap<&str, &str> = HashMap::from([
            ("zero", "0"),
            ("one", "1"),
            ("two", "2"),
            ("three", "3"),
            ("four", "4"),
            ("five", "5"),
            ("six", "6"),
            ("seven", "7"),
            ("eight", "8"),
            ("nine", "9"),
            ("ten", "10"),
            ("eleven", "11"),
            ("twelve", "12"),
            ("thirteen", "13"),
            ("fourteen", "14"),
            ("fifteen", "15"),
            ("sixteen", "16"),
            ("seventeen", "17"),
            ("eighteen", "18"),
            ("nineteen", "19"),
            ("twenty", "20"),
            ("thirty", "30"),
            ("forty", "40"),
            ("fifty", "50"),
            ("sixty", "60"),
            ("seventy", "70"),
            ("eighty", "80"),
            ("ninety", "90"),
            ("first", "1"),
            ("second", "2"),
            ("third", "3"),
            ("fourth", "4"),
            ("fifth", "5"),
            ("sixth", "6"),
            ("seventh", "7"),
            ("eighth", "8"),
            ("ninth", "9"),
            ("tenth", "10"),
        ]);
        words.get(s.to_lowercase().as_str()).map(|s| s.to_string())
    }

    /// Convert month names to numbers ("May" -> "5", "June" -> "6")
    fn month_to_number(s: &str) -> Option<String> {
        let months: HashMap<&str, &str> = HashMap::from([
            ("january", "1"),
            ("jan", "1"),
            ("february", "2"),
            ("feb", "2"),
            ("march", "3"),
            ("mar", "3"),
            ("april", "4"),
            ("apr", "4"),
            ("may", "5"),
            ("june", "6"),
            ("jun", "6"),
            ("july", "7"),
            ("jul", "7"),
            ("august", "8"),
            ("aug", "8"),
            ("september", "9"),
            ("sep", "9"),
            ("sept", "9"),
            ("october", "10"),
            ("oct", "10"),
            ("november", "11"),
            ("nov", "11"),
            ("december", "12"),
            ("dec", "12"),
        ]);
        months.get(s.to_lowercase().as_str()).map(|s| s.to_string())
    }

    /// Convert number to month name (5 -> "May")
    fn number_to_month(n: &str) -> Option<String> {
        let months = [
            "", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let idx: usize = n.parse().ok()?;
        if (1..=12).contains(&idx) {
            Some(months[idx].to_string())
        } else {
            None
        }
    }

    /// Convert number to full month name (5 -> "May")
    fn number_to_month_name(n: &str) -> Option<String> {
        let months = [
            "",
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        let idx: usize = n.parse().ok()?;
        if (1..=12).contains(&idx) {
            Some(months[idx].to_string())
        } else {
            None
        }
    }

    /// Convert ordinal to number ("5th" -> "5", "first" -> "1")
    fn ordinal_to_number(s: &str) -> Option<String> {
        let s_lower = s.to_lowercase();
        // Handle "5th", "1st", "2nd", "3rd", etc.
        if let Some(pos) = s_lower.find(|c: char| !c.is_ascii_digit()) {
            let num_part = &s_lower[..pos];
            if !num_part.is_empty() {
                let suffix = &s_lower[pos..];
                if suffix == "st" || suffix == "nd" || suffix == "rd" || suffix == "th" {
                    return Some(num_part.to_string());
                }
            }
        }
        // Also handle word ordinals
        Self::word_to_number(&s_lower)
    }

    // =============================================================================
    // TIME DETERMINISMS - Comprehensive bidirectional time mapping
    // =============================================================================

    /// Table of time determinisms: keyword → canonical form
    /// Covers ALL 24 hours + common times, bidirectional
    const TIME_DETERMINISMS: &[(&str, &str)] = &[
        // === MIDNIGHT ===
        ("midnight", "00:00"),
        ("12am", "00:00"),
        ("12 am", "00:00"),
        ("twelve am", "00:00"),
        ("twelve thirty am", "00:30"),
        ("12:30am", "00:30"),
        // === 1AM - 5AM ===
        ("1am", "01:00"),
        ("1 am", "01:00"),
        ("one am", "01:00"),
        ("1:00am", "01:00"),
        ("1:30am", "01:30"),
        ("one thirty am", "01:30"),
        ("2am", "02:00"),
        ("2 am", "02:00"),
        ("two am", "02:00"),
        ("2:00am", "02:00"),
        ("2:30am", "02:30"),
        ("3am", "03:00"),
        ("3 am", "03:00"),
        ("three am", "03:00"),
        ("3:00am", "03:00"),
        ("3:30am", "03:30"),
        ("4am", "04:00"),
        ("4 am", "04:00"),
        ("four am", "04:00"),
        ("4:00am", "04:00"),
        ("4:30am", "04:30"),
        ("5am", "05:00"),
        ("5 am", "05:00"),
        ("five am", "05:00"),
        ("5:00am", "05:00"),
        ("5:30am", "05:30"),
        // === DAWN / EARLY MORNING (6AM) ===
        ("dawn", "06:00"),
        ("sunrise", "06:00"),
        ("6am", "06:00"),
        ("6 am", "06:00"),
        ("six am", "06:00"),
        ("6:00am", "06:00"),
        ("6:30am", "06:30"),
        // === 7AM - 11AM ===
        ("7am", "07:00"),
        ("7 am", "07:00"),
        ("seven am", "07:00"),
        ("7:00am", "07:00"),
        ("7:30am", "07:30"),
        ("8am", "08:00"),
        ("8 am", "08:00"),
        ("eight am", "08:00"),
        ("8:00am", "08:00"),
        ("8:30am", "08:30"),
        ("9am", "09:00"),
        ("9 am", "09:00"),
        ("nine am", "09:00"),
        ("9:00am", "09:00"),
        ("9:30am", "09:30"),
        ("10am", "10:00"),
        ("10 am", "10:00"),
        ("ten am", "10:00"),
        ("10:00am", "10:00"),
        ("10:30am", "10:30"),
        ("11am", "11:00"),
        ("11 am", "11:00"),
        ("eleven am", "11:00"),
        ("11:00am", "11:00"),
        ("11:30am", "11:30"),
        // === NOON ===
        ("noon", "12:00"),
        ("midday", "12:00"),
        ("12pm", "12:00"),
        ("12 pm", "12:00"),
        ("twelve pm", "12:00"),
        ("12:00pm", "12:00"),
        ("12:30pm", "12:30"),
        // === 1PM - 5PM ===
        ("1pm", "13:00"),
        ("1 pm", "13:00"),
        ("one pm", "13:00"),
        ("1:00pm", "13:00"),
        ("1:30pm", "13:30"),
        ("2pm", "14:00"),
        ("2 pm", "14:00"),
        ("two pm", "14:00"),
        ("2:00pm", "14:00"),
        ("2:30pm", "14:30"),
        ("3pm", "15:00"),
        ("3 pm", "15:00"),
        ("three pm", "15:00"),
        ("3:00pm", "15:00"),
        ("3:30pm", "15:30"),
        ("4pm", "16:00"),
        ("4 pm", "16:00"),
        ("four pm", "16:00"),
        ("4:00pm", "16:00"),
        ("4:30pm", "16:30"),
        ("5pm", "17:00"),
        ("5 pm", "17:00"),
        ("five pm", "17:00"),
        ("5:00pm", "17:00"),
        ("5:30pm", "17:30"),
        // === DUSK / EVENING (6PM) ===
        ("dusk", "18:00"),
        ("sunset", "18:00"),
        ("evening", "18:00"),
        ("6pm", "18:00"),
        ("6 pm", "18:00"),
        ("six pm", "18:00"),
        ("6:00pm", "18:00"),
        ("6:30pm", "18:30"),
        // === 7PM - 11PM ===
        ("7pm", "19:00"),
        ("7 pm", "19:00"),
        ("seven pm", "19:00"),
        ("7:00pm", "19:00"),
        ("7:30pm", "19:30"),
        ("8pm", "20:00"),
        ("8 pm", "20:00"),
        ("eight pm", "20:00"),
        ("8:00pm", "20:00"),
        ("8:30pm", "20:30"),
        ("9pm", "21:00"),
        ("9 pm", "21:00"),
        ("nine pm", "21:00"),
        ("9:00pm", "21:00"),
        ("9:30pm", "21:30"),
        ("10pm", "22:00"),
        ("10 pm", "22:00"),
        ("ten pm", "22:00"),
        ("10:00pm", "22:00"),
        ("10:30pm", "22:30"),
        ("11pm", "23:00"),
        ("11 pm", "23:00"),
        ("eleven pm", "23:00"),
        ("11:00pm", "23:00"),
        ("11:30pm", "23:30"),
        // === 24-HOUR DIRECT ===
        ("00:00", "midnight"),
        ("0:00", "midnight"),
        ("01:00", "1am"),
        ("02:00", "2am"),
        ("03:00", "3am"),
        ("04:00", "4am"),
        ("05:00", "5am"),
        ("06:00", "6am"),
        ("07:00", "7am"),
        ("08:00", "8am"),
        ("09:00", "9am"),
        ("10:00", "10am"),
        ("11:00", "11am"),
        ("12:00", "noon"),
        ("13:00", "1pm"),
        ("14:00", "2pm"),
        ("15:00", "3pm"),
        ("16:00", "4pm"),
        ("17:00", "5pm"),
        ("18:00", "6pm"),
        ("19:00", "7pm"),
        ("20:00", "8pm"),
        ("21:00", "9pm"),
        ("22:00", "10pm"),
        ("23:00", "11pm"),
        // === TIME PERIODS ===
        ("morning", "AM"),
        ("afternoon", "PM"),
        ("evening", "PM"),
        ("night", "PM"),
        ("am", "AM"),
        ("pm", "PM"),
        ("a.m.", "AM"),
        ("p.m.", "PM"),
        // === DAY PARTS ===
        ("today", "today"),
        ("tomorrow", "tomorrow"),
        ("yesterday", "yesterday"),
    ];

    /// Normalize time terms to canonical forms (bidirectional)
    fn time_to_canonical(term: &str) -> Option<String> {
        let t = term.to_lowercase();
        for (keyword, canonical) in Self::TIME_DETERMINISMS {
            if *keyword == t {
                return Some(canonical.to_string());
            }
        }
        // Try compound patterns: 5pm, 3am, 10am, 8pm, etc.
        Self::parse_compound_time(&t)
    }

    /// Parse compound time patterns like "5pm" -> "17:00", "3am" -> "03:00"
    fn parse_compound_time(t: &str) -> Option<String> {
        let t_clean = t.replace(" ", "");
        // Match patterns like: 5pm, 5am, 12pm, 12am, 5:30pm, etc.
        let re = regex::Regex::new(r"^(\d{1,2})(?::(\d{2}))?(am|pm)$").ok()?;

        if let Some(caps) = re.captures(&t_clean) {
            let hour: usize = caps.get(1)?.as_str().parse().ok()?;
            let minute: usize = caps
                .get(2)
                .map(|m| m.as_str().parse().unwrap_or(0))
                .unwrap_or(0);
            let is_pm = caps.get(3)?.as_str() == "pm";

            // Validate hour
            if hour > 12 || minute > 59 {
                return None;
            }

            let mut hour_24 = hour;
            if hour == 12 {
                hour_24 = if is_pm { 12 } else { 0 };
            } else if is_pm {
                hour_24 += 12;
            }

            return Some(format!("{:02}:{:02}", hour_24, minute));
        }

        // Also handle "5 p.m." with dots
        let re2 = regex::Regex::new(r"^(\d{1,2})\s*(a\.?m\.?|p\.?m\.?)$").ok()?;
        if let Some(caps) = re2.captures(&t_clean) {
            let hour: usize = caps.get(1)?.as_str().parse().ok()?;
            let suffix = caps.get(2)?.as_str().to_lowercase();
            let is_pm = suffix.contains('p');

            let mut hour_24 = hour;
            if hour == 12 {
                hour_24 = if is_pm { 12 } else { 0 };
            } else if is_pm {
                hour_24 += 12;
            }

            return Some(format!("{:02}:00", hour_24));
        }

        None
    }

    /// Get all keyword forms for a canonical time (build from TIME_DETERMINISMS table)
    fn canonical_to_keywords(canonical: &str) -> Vec<String> {
        let c = canonical.to_lowercase();
        let mut keywords = vec![c.clone()];

        // Find all keywords that map TO this canonical
        for (keyword, canon) in Self::TIME_DETERMINISMS {
            if *canon == c && !keywords.contains(&keyword.to_string()) {
                keywords.push(keyword.to_string());
            }
        }

        keywords
    }

    // =============================================================================
    // BOOLEAN DETERMINISMS - Comprehensive boolean/slang mapping
    // =============================================================================

    /// Table of boolean determinisms: keyword → canonical form
    /// Covers all ways humans express yes/no, true/false
    const BOOL_DETERMINISMS: &[(&str, &str)] = &[
        // === AFFIRMATIVE (TRUE) ===
        ("yes", "true"),
        ("yeah", "true"),
        ("yep", "true"),
        ("yup", "true"),
        ("yea", "true"),
        ("aye", "true"),
        ("sure", "true"),
        ("ok", "true"),
        ("okay", "true"),
        ("correct", "true"),
        ("right", "true"),
        ("true", "true"),
        ("on", "true"),
        ("enabled", "true"),
        ("active", "true"),
        ("enable", "true"),
        ("allow", "true"),
        ("approved", "true"),
        ("accepted", "true"),
        ("confirmed", "true"),
        ("valid", "true"),
        ("positive", "true"),
        ("indeed", "true"),
        ("absolutely", "true"),
        ("definitely", "true"),
        ("certainly", "true"),
        // === NEGATIVE (FALSE) ===
        ("no", "false"),
        ("nope", "false"),
        ("nah", "false"),
        ("never", "false"),
        ("false", "false"),
        ("off", "false"),
        ("disabled", "false"),
        ("inactive", "false"),
        ("disable", "false"),
        ("disallow", "false"),
        ("rejected", "false"),
        ("denied", "false"),
        ("declined", "false"),
        ("invalid", "false"),
        ("negative", "false"),
        ("wrong", "false"),
        ("incorrect", "false"),
        ("not", "false"),
        ("none", "false"),
        // === NUMERIC BOOLEANS ===
        ("1", "true"),
        ("true", "true"),
        ("0", "false"),
        ("false", "false"),
        ("-1", "false"),
        // === BIDIRECTIONAL MAPPINGS ===
        ("true", "true"),
        ("false", "false"),
    ];

    /// Normalize boolean terms to canonical forms
    fn bool_to_canonical(term: &str) -> Option<String> {
        let t = term.to_lowercase();
        for (keyword, canonical) in Self::BOOL_DETERMINISMS {
            if *keyword == t {
                return Some(canonical.to_string());
            }
        }
        None
    }

    /// Get all keyword forms for a boolean canonical
    fn bool_canonical_to_keywords(canonical: &str) -> Vec<String> {
        let c = canonical.to_lowercase();
        let mut keywords = vec![c.clone()];

        for (keyword, canon) in Self::BOOL_DETERMINISMS {
            if *canon == c && !keywords.contains(&keyword.to_string()) {
                keywords.push(keyword.to_string());
            }
        }

        keywords
    }

    /// Generate AsciiDoc contracts for all determinisms
    /// Creates a master table with edges for graph traversal
    pub fn generate_determinism_contracts() -> String {
        let mut doc = String::new();

        doc.push_str(
            r#":determinism-version: 1.0
:determinism-count: 

[[determinisms]]
= Determinism Index

This document contains all semantic determinisms used for query expansion
and graph-based semantic reasoning. Each mapping creates bidirectional
edges in the knowledge graph via `edge::is_equivalent_to`.

== Boolean Determinisms

|===
|Keyword |Canonical |Category
"#,
        );

        // Add boolean determinisms
        let mut bool_true: Vec<&str> = Vec::new();
        let mut bool_false: Vec<&str> = Vec::new();
        for (keyword, canonical) in Self::BOOL_DETERMINISMS {
            if *canonical == "true" && !bool_true.contains(keyword) {
                bool_true.push(keyword);
            } else if *canonical == "false" && !bool_false.contains(keyword) {
                bool_false.push(keyword);
            }
        }
        for kw in &bool_true {
            doc.push_str(&format!("|{} |true |boolean\n", kw));
        }
        for kw in &bool_false {
            doc.push_str(&format!("|{} |false |boolean\n", kw));
        }

        doc.push_str("|===\n\n");
        doc.push_str("edge::is_equivalent_to[true]\n");
        doc.push_str("edge::is_equivalent_to[false]\n\n");

        // Time determinisms (simplified - key times only)
        doc.push_str("== Time Determinisms (Key Times)\n\n");
        doc.push_str("|===\n");
        doc.push_str("|Keyword |Canonical |Category\n");

        let key_times = [
            ("midnight", "00:00"),
            ("noon", "12:00"),
            ("dawn", "06:00"),
            ("dusk", "18:00"),
            ("morning", "AM"),
            ("afternoon", "PM"),
            ("evening", "PM"),
            ("night", "PM"),
        ];

        for (kw, canon) in &key_times {
            doc.push_str(&format!("|{} |{} |time\n", kw, canon));
            // Add reverse mapping
            doc.push_str(&format!("|{} |{} |time\n", canon, kw));
        }

        doc.push_str("|===\n\n");

        // Add edge declarations for time
        for (_kw, canon) in &key_times {
            doc.push_str(&format!("edge::is_equivalent_to[{}]\n", canon));
        }
        doc.push('\n');

        // Number determinisms
        doc.push_str("== Number Determinisms\n\n");
        doc.push_str("|===\n");
        doc.push_str("|Keyword |Canonical |Category\n");

        let numbers = [
            ("zero", "0"),
            ("one", "1"),
            ("two", "2"),
            ("three", "3"),
            ("four", "4"),
            ("five", "5"),
            ("six", "6"),
            ("seven", "7"),
            ("eight", "8"),
            ("nine", "9"),
            ("ten", "10"),
            ("first", "1"),
            ("second", "2"),
            ("third", "3"),
            ("fourth", "4"),
            ("fifth", "5"),
            ("sixth", "6"),
            ("seventh", "7"),
            ("eighth", "8"),
            ("ninth", "9"),
            ("tenth", "10"),
        ];

        for (kw, canon) in &numbers {
            doc.push_str(&format!("|{} |{} |number\n", kw, canon));
            doc.push_str(&format!("|{} |{} |number\n", canon, kw));
        }

        doc.push_str("|===\n\n");

        // Add edge declarations for numbers
        for (_, canon) in &numbers {
            doc.push_str(&format!("edge::is_equivalent_to[{}]\n", canon));
        }
        doc.push('\n');

        // Month determinisms
        doc.push_str("== Month Determinisms\n\n");
        doc.push_str("|===\n");
        doc.push_str("|Keyword |Canonical |Category\n");

        let months = [
            ("january", "1"),
            ("jan", "1"),
            ("february", "2"),
            ("feb", "2"),
            ("march", "3"),
            ("mar", "3"),
            ("april", "4"),
            ("apr", "4"),
            ("may", "5"),
            ("june", "6"),
            ("jun", "6"),
            ("july", "7"),
            ("jul", "7"),
            ("august", "8"),
            ("aug", "8"),
            ("september", "9"),
            ("sep", "9"),
            ("sept", "9"),
            ("october", "10"),
            ("oct", "10"),
            ("november", "11"),
            ("nov", "11"),
            ("december", "12"),
            ("dec", "12"),
        ];

        for (kw, canon) in &months {
            doc.push_str(&format!("|{} |{} |month\n", kw, canon));
            doc.push_str(&format!("|{} |{} |month\n", canon, kw));
        }

        doc.push_str("|===\n\n");

        // Add edge declarations for months
        for (_, canon) in &months {
            doc.push_str(&format!("edge::is_equivalent_to[{}]\n", canon));
        }

        doc
    }
}
