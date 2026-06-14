// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use aden_core::AdenConfig;
use aden_graph::Direction;
use aden_store::GraphStorage;

use crate::types::{AnchorPattern, QueryIntent};
use crate::util::{
    find_project_root, fmt_score, load_or_build_index, node_to_json, parse_single_edge_type,
    perform_check, query_index, valid_edge_types,
};
#[cfg(feature = "watch")]
use crate::util::{sanitize_anchor, sanitize_source_file};
use aden_index::SearchResult;

/// Words too common in natural-language questions to be treated as symbol names.
/// "include", "output", "context", "decide" would match struct/function names
/// spuriously in many codebases.
const SYMBOL_STOP_WORDS: &[&str] = &[
    // question words
    "how", "what", "why", "when", "where", "which", "does", "do", "did", "is", "are", "was", "were",
    "will", "would", "can", "could", "should", // connectives
    "the", "an", "in", "to", "of", "and", "or", "not", "that", "this", "with", "for", "from",
    "into", "on", "by", "at", "its", "it", "a",
    // common verbs that collide with symbol names in many codebases
    "include", "output", "input", "get", "set", "new", "add", "find", "build", "make", "run", "use",
    "put", "take", "call", "handle", "process", "check", "update", "create", "delete", "remove",
    "read", "write", "send", "receive", "parse", "emit", "render", "load", "save", "open", "close",
    "start", "stop", "init", "reset", "fetch", "log", "print", "format", "encode", "decode",
    "next", "map", "list", "count",
    // generic nouns that are often symbol names *and* common English words
    "context", "result", "error", "data", "value", "node", "graph", "block", "item", "type", "name",
    "path", "file", "line", "text", "token", "key", "time", "index", "state", "kind", "source",
    "target", "mode", "level",
];

/// Extract explicit `func()` or `Type::method()` references from a query.
/// These are unambiguous intent signals — the user told us exactly what they
/// want to know about.
fn extract_explicit_symbols(query: &str) -> Vec<String> {
    let mut symbols = Vec::new();
    // Match word characters immediately followed by `(`
    let chars = query.char_indices().peekable();
    for (i, c) in chars {
        if c == '(' {
            // Walk backwards to collect the symbol name
            let before = &query[..i];
            let sym: String = before
                .chars()
                .rev()
                .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            if sym.len() >= 2 {
                symbols.push(sym.to_lowercase());
            }
        }
    }
    symbols
}

/// BM25-score window, below the top score, treated as "effectively tied" with
/// the leader. Used both by [`resolve_anchor_fuzzy`] (structural tiebreak) and by
/// `cmd_ask` (to decide whether routing is ambiguous enough to seed alternates).
const ANCHOR_NOISE_BAND: f64 = 5.0;

/// True if an anchor points at a test/spec file. Such symbols (test fixtures,
/// `callback` helpers, assertion-message strings) routinely share a name with
/// the user's query word yet carry almost no explanatory content, so they must
/// not win `ask` routing over the production symbol they exercise. Detection is
/// language-agnostic: it keys off the conventional test path/name markers shared
/// across Python, Go, JS/TS, Rust, Java, Ruby, etc. The match is gated so that a
/// query genuinely about the test suite can still reach these via the relaxation
/// fallback when *every* candidate is a test symbol.
pub(crate) fn is_test_anchor(anchor: &str) -> bool {
    let a = anchor.to_lowercase();
    const MARKERS: &[&str] = &[
        "/test/",
        "/tests/",
        "/spec/",
        "/specs/",
        "/__tests__/",
        // A file literally named `tests.<ext>` (Rust's conventional split-out
        // test module, `src/tests.rs`) — matched with the leading separator so
        // a `my_tests.rs` production file is NOT swept in (that case is the
        // `_tests.` marker's, which requires the underscore).
        "/tests.",
        "/test_",
        "_test.",
        "_tests.",
        ".test.",
        ".spec.",
        "_spec.",
        "spec_",
    ];
    MARKERS.iter().any(|m| a.contains(m))
}

/// True if a root-relative source path is a test/spec file, by the same
/// conventional markers as [`is_test_anchor`]. The path is relative
/// (`tests/type_check/foo.py`), so a slash is prepended before the marker
/// check — several markers (`/tests/`, `/spec/`) are anchored on a leading
/// separator that a relative path lacks at its first segment. `gen` uses this
/// to classify a symbol's call sites as `Tests` edges (graph-type roadmap
/// Wave 1): the module-form anchor flattens the directory, so test-ness must
/// be derived from the real source path, exactly as in [`is_test_result`].
pub(crate) fn is_test_source_path(rel_path: &str) -> bool {
    !rel_path.is_empty() && is_test_anchor(&format!("/{rel_path}"))
}

/// True if a search result points at a test/spec file, checking BOTH the anchor
/// AND its real `source_path`. The module-form anchor flattens the directory
/// (`aden://module/flask/typing_route.py#…` for a file that actually lives at
/// `tests/type_check/typing_route.py`), so the anchor alone can hide a fixture's
/// test-ness — the `source_path` is where the `tests/` marker survives. Routing
/// must use this, not bare `is_test_anchor`, so dir-only test files can't win.
fn is_test_result(result: &SearchResult) -> bool {
    if is_test_anchor(&result.anchor) {
        return true;
    }
    // The source_path is relative (`tests/type_check/foo.py`), so prepend a slash
    // before the marker check — several markers (`/tests/`, `/spec/`) are anchored
    // on a leading separator that a relative path lacks at its first segment.
    let src = result.source_path.to_string_lossy();
    if src.is_empty() {
        return false;
    }
    is_test_anchor(&format!("/{src}"))
}

/// A result carrying fewer indexed tokens than this is a thin stub (abstract
/// base method, one-line shim) — real but near-contentless. Routing prefers a
/// substantive symbol over a thin one within the score noise band so `ask`
/// lands on the dispatcher that actually does the work, not its 17-token
/// abstract declaration, before the post-assembly thin-stub guard has to
/// broaden all the way to `mod-project`.
const SUBSTANTIVE_TOKEN_FLOOR: usize = 40;

/// Collect up to `max` distinct anchors that sit within [`ANCHOR_NOISE_BAND`] of
/// the top score and are *not* the already-chosen `primary` (a near-tie set). The
/// list is in rank order, deduped, and excludes the primary so callers can treat
/// it as shallow "also consider" seeds. Returns empty when there is a clear winner.
fn inband_alternate_candidates(primary: &str, results: &[SearchResult], max: usize) -> Vec<String> {
    if results.is_empty() {
        return Vec::new();
    }
    let top_score = results[0].score;
    let mut out: Vec<String> = Vec::new();
    for r in results {
        if (top_score - r.score) > ANCHOR_NOISE_BAND {
            break; // results are score-ordered; nothing past here is in-band
        }
        // Skip the primary, dupes, and test fixtures — alternates seed extra
        // context, so a dir-only test file shouldn't pad the answer either.
        if r.anchor == primary || out.contains(&r.anchor) || is_test_result(r) {
            continue;
        }
        out.push(r.anchor.clone());
        if out.len() >= max {
            break;
        }
    }
    out
}

// ── Conceptual / overview routing ───────────────────────────────────────────
//
// Broad questions ("What is X?", "philosophy", "high-level architecture") are
// answered by curated prose, not by whichever implementation symbol happens to
// echo a query token. The signal below is computed BEFORE anchor selection and,
// when it fires, routing prefers substantive prose/doc anchors within the BM25
// noise band. When it does not fire, selection is byte-identical to the default
// path. Everything here is structural (anchor scheme, graph in-degree, token
// counts) — nothing is aden-specific or format-specific.

/// Phrasings that mark a question as a high-level/conceptual one. Matched as
/// substrings of the lowercased question; `("how does", "work")` style pairs
/// are handled separately in [`is_overview_query`].
const OVERVIEW_PHRASES: &[&str] = &[
    "what is",
    "what's",
    "philosophy",
    "overview",
    "high level",
    "high-level",
    "architecture",
    "design",
    "core idea",
    "big picture",
    // problem-statement questions ("what problem does X solve") are identity/
    // motivation questions — answered by prose, not by a symbol named "solve".
    "what problem",
];

/// True if the question contains a token that looks like a concrete code
/// symbol or path: explicit call syntax, snake_case, `::` paths, file paths,
/// dotted attribute access, or interior capitals (camelCase/PascalCase). Such
/// a token is a precise target — the overview preference must stand down and
/// let exact symbol matching win (e.g. "how does resolve_anchor_fuzzy work").
fn has_symbolish_token(question: &str) -> bool {
    if !extract_explicit_symbols(question).is_empty() {
        return true;
    }
    question.split_whitespace().any(|w| {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != ':' && c != '.');
        w.contains('_')
            || w.contains("::")
            || w.contains('/')
            || (w.contains('.') && !w.ends_with('.'))
            // Interior capitals: "AdenConfig", "dispatchRequest" — but not a
            // capitalized first letter ("Aden"), which is just English.
            || w.chars().skip(1).any(|c| c.is_ascii_uppercase())
    })
}

/// The overview-question signal: broad intent (General/Explain/Compare), no
/// symbol-like token, and conceptual phrasing. Computed BEFORE anchor
/// selection; when false, routing behavior is unchanged.
pub(crate) fn is_overview_query(question: &str, intent: &QueryIntent) -> bool {
    if !matches!(
        intent,
        QueryIntent::General | QueryIntent::Explain | QueryIntent::Compare
    ) {
        return false;
    }
    if has_symbolish_token(question) {
        return false;
    }
    let q = question.to_lowercase();
    OVERVIEW_PHRASES.iter().any(|p| q.contains(p)) || (q.contains("how does") && q.contains("work"))
}

/// The `<proj>` segment of a scheme-form anchor (`aden://doc/<proj>/…`,
/// `aden://module/<proj>/…`). `None` for legacy short anchors (`mod-*`, …).
fn anchor_project_segment(anchor: &str) -> Option<&str> {
    anchor
        .strip_prefix("aden://doc/")
        .or_else(|| anchor.strip_prefix("aden://module/"))
        .and_then(|r| r.split('/').next())
}

/// Tokens of the dominant (most frequent) project segment across the results —
/// a derivation of "what this project is called" from the corpus itself, with
/// no configuration. Ties break to the lexicographically smaller segment for
/// determinism.
fn dominant_project_tokens(results: &[SearchResult]) -> HashSet<String> {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for r in results {
        if let Some(seg) = anchor_project_segment(&r.anchor) {
            *counts.entry(seg).or_insert(0) += 1;
        }
    }
    let Some((seg, _)) = counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
    else {
        return HashSet::new();
    };
    seg.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// True when every substantive query token is part of the project's own name
/// ("What is Aden?" reduces to the single token "aden"). For such a question
/// lexical retrieval carries no information — every anchor mentions the project
/// name — so routing should head for the corpus's front door instead of
/// whichever section BM25-noise ranked first.
fn query_is_project_identity(query: &str, results: &[SearchResult]) -> bool {
    let toks: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| s.len() >= 3)
        .map(|s| s.to_lowercase())
        .filter(|s| !SYMBOL_STOP_WORDS.contains(&s.as_str()))
        .collect();
    if toks.is_empty() {
        return false;
    }
    let proj = dominant_project_tokens(results);
    !proj.is_empty() && toks.iter().all(|t| proj.contains(t))
}

/// True for the conventional entry-point documents of a corpus: a file whose
/// stem is `readme` or `index` (any extension, any case). The same class of
/// cross-ecosystem convention as the test-path markers in [`is_test_anchor`]:
/// GitHub renders README as the project's front page; `index.*` is the root of
/// virtually every docs tree. `file` is the `<proj>/<relpath>` form returned by
/// [`super::generate::doc_anchor_file`].
fn is_entry_doc_file(file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    let stem = name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    matches!(stem.as_str(), "readme" | "index")
}

/// Overview-mode anchor selection. Within the BM25 noise band, prefer prose
/// document anchors (`aden://doc/…` scheme) over code symbols:
///
/// 1. Collect the in-band, non-test prose candidates (rank order, deduped).
///    None ⇒ return `None`, and the caller falls through to the default path
///    unchanged.
/// 2. For a project-identity question (every substantive token is the project
///    name), prefer the conventional entry docs (README/index) among them,
///    picking by (cross-reference in-degree, token substance, rank). For any
///    other overview question the top-ranked in-band prose anchor wins — BM25
///    is trusted, only the doc-vs-code preference is applied.
///
/// If the chosen anchor turns out to be a thin structural shell (a bare
/// heading node), the post-assembly fallback in `cmd_ask` broadens WITHIN the
/// document — see the prose-doc arm of the thin-stub handling there.
///
/// Returns `(anchor, reason)`; the reason feeds `--explain`.
fn resolve_anchor_overview(
    query: &str,
    results: &[SearchResult],
    token_count: &dyn Fn(&str) -> usize,
    doc_indegree: &std::collections::HashMap<String, usize>,
) -> Option<(String, String)> {
    if results.is_empty() {
        return None;
    }
    let top_score = results[0].score;
    let mut docs: Vec<&SearchResult> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for r in results {
        if (top_score - r.score) > ANCHOR_NOISE_BAND {
            break; // score-ordered; nothing past here is in-band
        }
        if is_test_result(r) || !AnchorPattern::is_prose_doc(&r.anchor) {
            continue;
        }
        if seen.insert(r.anchor.as_str()) {
            docs.push(r);
        }
    }
    if docs.is_empty() {
        return None;
    }

    let indeg = |anchor: &str| doc_indegree.get(anchor).copied().unwrap_or(0);

    let identity = query_is_project_identity(query, results);
    let (candidate, reason) = if identity {
        let entries: Vec<&&SearchResult> = docs
            .iter()
            .filter(|r| {
                super::generate::doc_anchor_file(&r.anchor)
                    .map(is_entry_doc_file)
                    .unwrap_or(false)
            })
            .collect();
        let (pool, pool_label): (Vec<&&SearchResult>, &str) = if entries.is_empty() {
            (docs.iter().collect(), "in-band prose docs")
        } else {
            (entries, "entry docs (README/index)")
        };
        // Best by (in-degree, substance); earliest (highest score) wins ties —
        // candidates are iterated in rank order and replaced only on a
        // strictly-greater key, mirroring `selection_key`'s tie handling.
        let mut best: Option<&&SearchResult> = None;
        for r in pool {
            let key = |x: &SearchResult| (indeg(&x.anchor), token_count(&x.anchor));
            if best.map(|b| key(r) > key(b)).unwrap_or(true) {
                best = Some(r);
            }
        }
        let chosen = *best?;
        (
            chosen,
            format!(
                "overview: project-identity question; best of {} by (in-degree, substance)",
                pool_label
            ),
        )
    } else {
        (
            docs[0],
            "overview: top-ranked prose doc within the noise band".to_string(),
        )
    };

    Some((candidate.anchor.clone(), reason))
}

/// The canonical anchor of the document containing `anchor`: the SAME-FILE doc
/// anchor with the highest incoming `RelatesTo`/`Documents` in-degree (ties →
/// lexicographically smaller anchor). This is the node the rest of the corpus
/// actually cross-references — prose refs are real graph edges — so it sits
/// inside the reference web and assembles connected context where a bare
/// heading shell assembles almost nothing. `None` when no same-file anchor is
/// referenced at all (or `anchor` itself is already the canonical one).
fn same_file_canonical_anchor(
    anchor: &str,
    doc_indegree: &std::collections::HashMap<String, usize>,
) -> Option<(String, usize)> {
    let file = super::generate::doc_anchor_file(anchor)?;
    // Only an anchor MORE referenced than the current one is "the" canonical
    // node — if the routed anchor already is the file's most-referenced one,
    // there is nothing better within the document.
    let own = doc_indegree.get(anchor).copied().unwrap_or(0);
    doc_indegree
        .iter()
        .filter(|(a, d)| {
            **d > own && a.as_str() != anchor && super::generate::doc_anchor_file(a) == Some(file)
        })
        .max_by(|(a1, d1), (a2, d2)| d1.cmp(d2).then_with(|| a2.cmp(a1)))
        .map(|(a, d)| (a.clone(), *d))
}

/// Incoming `RelatesTo`/`Documents` in-degree for every prose doc anchor in the
/// graph — the live "explanatory importance" signal: prose cross-references are
/// real graph edges (ADR-006), so heavily-referenced documents are exactly the
/// pillar overviews a conceptual question wants. Built only when the overview
/// signal fires; the graph load is cached.
fn doc_reference_indegree(path: &Path) -> std::collections::HashMap<String, usize> {
    let mut map = std::collections::HashMap::new();
    let Ok(graph) = aden_graph::cache::build_from_directory_cached(path) else {
        return map;
    };
    for e in graph.graph.edge_indices() {
        let et = graph.graph[e].edge_type;
        if !matches!(
            et,
            aden_core::EdgeType::RelatesTo | aden_core::EdgeType::Documents
        ) {
            continue;
        }
        let Some((_, tgt)) = graph.graph.edge_endpoints(e) else {
            continue;
        };
        let anchor = &graph.graph[tgt].doc.anchor;
        if AnchorPattern::is_prose_doc(anchor) {
            *map.entry(anchor.clone()).or_insert(0) += 1;
        }
    }
    map
}

/// Resolve a natural-language query to the best matching anchor.
///
/// Strategy (in order):
///
/// 1. **Explicit call syntax** — `func()` or `Type::method()` in the query
///    is an unambiguous signal.  Match against `#symbol` anchors first.
///
/// 2. **Qualified symbol token match** — query tokens that are ≥3 chars,
///    not in the stop-word list, and exactly match a `#symbol` name in the
///    top results.  Requires the match to appear in a top-20 result.
///
/// 3. **Score-driven selection with tiebreaker** — pick the highest-scoring
///    result.  Within a 5-point noise band, prefer by `AnchorPattern`
///    (Symbol > Adr > Plan > Module > …).
///
/// No hardcoded word→module mappings.  The search index is the source of
/// truth; this function only applies generic structural preferences on top.
fn resolve_anchor_fuzzy(
    query: &str,
    results: &[SearchResult],
    token_count: impl Fn(&str) -> usize,
) -> String {
    resolve_anchor_fuzzy_with_reason(query, results, token_count).0
}

/// [`resolve_anchor_fuzzy`] plus a human-readable label of WHICH selection
/// step produced the anchor — surfaced by `ask --explain`. Identical logic;
/// the wrapper above keeps the established signature for `asm` and the tests.
fn resolve_anchor_fuzzy_with_reason(
    query: &str,
    results: &[SearchResult],
    token_count: impl Fn(&str) -> usize,
) -> (String, &'static str) {
    if results.is_empty() {
        return ("readme".to_string(), "no search results; default readme");
    }

    // Step 1: explicit `func()` syntax — highest confidence.
    let explicit = extract_explicit_symbols(query);
    if !explicit.is_empty() {
        // Search all results (not just top-10) for an exact symbol name match.
        for sym in &explicit {
            if let Some(hit) = results.iter().find(|r| {
                r.anchor
                    .rsplit('#')
                    .next()
                    .map(|s| s.to_lowercase() == *sym)
                    .unwrap_or(false)
            }) {
                return (
                    hit.anchor.clone(),
                    "explicit call syntax in query matched a symbol anchor exactly",
                );
            }
        }
    }

    // Step 2: qualified token match — tokens that are specific enough to be
    // symbol names (≥3 chars, not a stop word, not a single common letter).
    // Tokens are STEMMED (via the same stemmer the BM25 index uses) before the
    // symbol-name comparison so "how does the indexing work" fast-paths to a
    // symbol named `index`, consistent with the BM25 stem path. We keep both the
    // raw lowercase form and its stem, so an exact symbol match still wins even
    // when the symbol name itself doesn't stem cleanly.
    let query_tokens: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| s.len() >= 3 && !SYMBOL_STOP_WORDS.contains(&s.to_lowercase().as_str()))
        .flat_map(|s| {
            let lower = s.to_lowercase();
            let stemmed = aden_index::tokenize(s);
            std::iter::once(lower).chain(stemmed)
        })
        .collect();

    // Selection key shared by Step 2 (symbol-token matches) and Step 3 (score
    // band), most-significant first:
    //   1. structural pattern (Symbol > Module > …) — a real symbol always beats
    //      a module overview;
    //   2. substantiveness — among same-pattern candidates, a symbol with real
    //      content beats a thin stub (17-token abstract base method), so `ask`
    //      lands on the dispatcher rather than its abstract declaration.
    // Candidates are iterated in score-descending order and replaced only on a
    // strictly-greater key, so the earliest (highest BM25 score) wins ties.
    let selection_key = |r: &SearchResult| {
        (
            AnchorPattern::from_anchor(&r.anchor).tiebreak(),
            (token_count(&r.anchor) >= SUBSTANTIVE_TOKEN_FLOOR) as u8,
        )
    };
    let pick_best = |best: Option<&SearchResult>, r: &SearchResult| -> bool {
        match best {
            // keep existing on tie (earlier == higher score); replace on greater
            Some(b) => selection_key(r) > selection_key(b),
            None => true,
        }
    };

    // Step 2 selection: among the (non-test) results whose symbol name matches a
    // query token, choose by `selection_key` rather than taking the first hit —
    // otherwise a thin abstract `View.dispatch_request` short-circuits routing
    // before the substantive `Flask.dispatch_request` is ever considered. A
    // test-file symbol must not win here just because its name echoes a query
    // word; it falls through to the relaxation fallback if nothing better exists.
    let symbol_token_match = |result: &SearchResult| -> bool {
        if is_test_result(result) {
            return false;
        }
        let Some(sym) = result.anchor.rsplit('#').next() else {
            return false;
        };
        if sym.len() < 3 {
            return false;
        }
        let sym_lower = sym.to_lowercase();
        if SYMBOL_STOP_WORDS.contains(&sym_lower.as_str()) {
            return false;
        }
        let sym_stem = aden_index::tokenize(&sym_lower);
        query_tokens.contains(&sym_lower) || sym_stem.iter().any(|st| query_tokens.contains(st))
    };
    let mut step2_best: Option<&SearchResult> = None;
    for result in results.iter().take(20).filter(|r| symbol_token_match(r)) {
        if pick_best(step2_best, result) {
            step2_best = Some(result);
        }
    }
    if let Some(hit) = step2_best {
        return (
            hit.anchor.clone(),
            "query token matched a symbol name (structural tiebreak among matches)",
        );
    }

    // Step 3: score-driven selection with structural tiebreaker.
    // Within a 5-point noise band of the top score, prefer Symbol over Module.
    // Exception: do NOT select a symbol anchor whose bare name is a stop word
    // (e.g. the query "How does error handling work?" must not route to `#Error`
    // just because BM25 ranked it highest — "error" is a stop word and the user
    // was asking a general question, not asking about a specific Error type).
    let top_score = results[0].score;
    let noise_band = ANCHOR_NOISE_BAND;

    // Helper: true if anchor is a symbol whose bare name is a stop word.
    let is_stopword_symbol = |anchor: &str| -> bool {
        if let Some(sym) = anchor.rsplit('#').next()
            && anchor.contains('#')
        {
            return SYMBOL_STOP_WORDS.contains(&sym.to_lowercase().as_str());
        }
        false
    };

    // First pass: pick best within noise band (by the shared `selection_key`),
    // excluding stop-word symbols and test-file symbols (which carry a name but
    // little explanatory content).
    let mut best: Option<&SearchResult> = None;
    for r in results
        .iter()
        .filter(|r| (top_score - r.score) <= noise_band)
        .filter(|r| !is_stopword_symbol(&r.anchor))
        .filter(|r| !is_test_result(r))
    {
        if pick_best(best, r) {
            best = Some(r);
        }
    }

    // Fallback: if every in-band candidate was a stop-word or test symbol, relax
    // and take the structurally-preferred top result (the query may genuinely be
    // about the test suite, or there may simply be nothing else to offer).
    if let Some(best) = best {
        return (
            best.anchor.clone(),
            "best within score noise band (structural tiebreak + substantiveness)",
        );
    }
    let relaxed = results
        .iter()
        .max_by_key(|r| AnchorPattern::from_anchor(&r.anchor).tiebreak())
        .unwrap_or(&results[0]);
    (
        relaxed.anchor.clone(),
        "all in-band candidates were test/stop-word symbols; relaxed to structurally-best result",
    )
}

pub fn cmd_check(
    path: &Path,
    severity: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("check requires a directory path".into());
    }

    let min_severity = match severity.to_lowercase().as_str() {
        "suggest" => 0,
        "warn" => 1,
        "forbid" => 2,
        _ => {
            return Err(format!(
                "Invalid severity '{}': use Suggest, Warn, or Forbid",
                severity
            )
            .into());
        }
    };

    let messages = perform_check(path)?;

    // Machine-readable for the global `-j/--json` flag (previously ignored).
    // Classify the human messages by prefix into errors/warnings/info, mirroring
    // the exit semantics of the text path below.
    if json {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        let mut info = Vec::new();
        for m in &messages {
            if let Some(rest) = m.strip_prefix("ERROR: ") {
                errors.push(rest.to_string());
            } else if let Some(rest) = m.strip_prefix("WARNING: ") {
                warnings.push(rest.to_string());
            } else if let Some(rest) = m.strip_prefix("INFO: ") {
                info.push(rest.to_string());
            } else {
                info.push(m.clone());
            }
        }
        let fails = !errors.is_empty() || (!warnings.is_empty() && min_severity <= 1);
        let env = serde_json::json!({
            "ok": !fails,
            "errors": errors,
            "warnings": warnings,
            "info": info,
        });
        println!("{}", serde_json::to_string_pretty(&env)?);
        if fails {
            std::process::exit(1);
        }
        return Ok(());
    }

    let mut exit_code = 0i32;
    for msg in &messages {
        if msg.starts_with("ERROR:") {
            // ERROR maps to Forbid (severity 2)
            if min_severity <= 2 {
                eprintln!("{msg}");
                exit_code = 1;
            } else {
                println!("{msg}");
            }
        } else if msg.starts_with("WARNING:") {
            // WARNING maps to Warn (severity 1)
            if min_severity <= 1 {
                eprintln!("{msg}");
                exit_code = 1;
            } else {
                println!("{msg}");
            }
        } else {
            println!("{msg}");
        }
    }

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

#[derive(Clone)]
pub struct AsmOptions {
    pub path: PathBuf,
    pub from: String,
    pub depth: usize,
    pub budget: usize,
    pub edge_types: Vec<aden_core::EdgeType>,
    pub out: Option<PathBuf>,
    pub format: String,
    pub silent: bool,
    pub auto: bool,
    pub strict: bool,
    pub inspect: bool,
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub attributes: Vec<String>,
}

/// Scales `avg_score` into the additive budget multiplier for `--auto`.
const AUTO_BOOST_SCALE: f64 = 2.0;
/// Ceiling on the boost; with scale 2.0 the effective budget tops out at ×4.
const AUTO_BOOST_MAX: f64 = 3.0;
/// Hard cap on the auto-scaled budget, in tokens.
const AUTO_BUDGET_CAP: usize = 32_000;

/// Compute the `--auto` effective token budget from a base budget and the
/// average search relevance.
///
/// Smooth (un-truncated) boost: intermediate relevance scales continuously
/// instead of snapping to integer buckets. The boost is `avg_score * SCALE`
/// clamped to `[0, AUTO_BOOST_MAX]`, the budget is multiplied by `1 + boost`,
/// rounded once, and capped at [`AUTO_BUDGET_CAP`]. With the default scale of
/// 2.0 and max of 3.0 the effective budget tops out at ×4 of the base.
fn auto_boosted_budget(base: usize, avg_score: f64) -> usize {
    let boost = (avg_score * AUTO_BOOST_SCALE).clamp(0.0, AUTO_BOOST_MAX);
    ((base as f64 * (1.0 + boost)).round() as usize).min(AUTO_BUDGET_CAP)
}

pub fn cmd_asm(opts: AsmOptions) -> Result<(), Box<dyn std::error::Error>> {
    use aden_asm::traverse::{AssemblyOptions, assemble, assemble_adg};

    if !opts.path.is_dir() {
        return Err("asm requires a directory path".into());
    }
    super::ensure_fresh(&opts.path);

    let (from_anchor, effective_budget) = if opts.auto && !opts.strict {
        let index = load_or_build_index(&opts.path)?;
        let results = query_index(&index, &opts.from);
        // Exact-first: if the user passed an anchor that already resolves
        // exactly in the store, keep it. `--auto` must not fuzzy-re-resolve a
        // valid anchor onto a thinner doc-shell node — the non-auto path would
        // have produced full content for the same URI, and silently swapping in
        // a lower-relevance node is the worst failure mode for an LLM. We only
        // fall back to fuzzy resolution when the exact anchor is NOT found, and
        // either way the search results still feed the relevance boost.
        let resolved =
            if aden_graph::cache::resolve_anchor_in_store(&opts.path, &opts.from).is_some() {
                opts.from.clone()
            } else {
                resolve_anchor_fuzzy(&opts.from, &results, |a| index.doc_token_count(a))
            };
        if resolved != opts.from {
            eprintln!("INFO: Resolved '{}' → '{}'", opts.from, resolved);
        }
        let budget = if results.is_empty() {
            opts.budget
        } else {
            let avg_score: f64 =
                results.iter().map(|r| r.score).sum::<f64>() / results.len() as f64;
            // Smooth (un-truncated) boost: intermediate relevance scales
            // continuously instead of snapping to integer buckets. The ×4
            // high-relevance ceiling is preserved via AUTO_BOOST_MAX.
            auto_boosted_budget(opts.budget, avg_score)
        };
        (resolved, budget)
    } else {
        (opts.from.clone(), opts.budget)
    };

    // Resolve the anchor against the store (cheap — no full-graph load). A bare
    // symbol/module name resolves to a single full anchor by `#suffix` match;
    // unknown/ambiguous is a hard error. We never silently substitute a fuzzy
    // match — emitting an unrelated node is the worst failure mode for an LLM.
    let resolved_anchor = aden_graph::cache::resolve_anchor_in_store(&opts.path, &from_anchor)
        .ok_or_else(|| {
            format!(
                "Anchor '{}' not found or ambiguous. Run 'aden list .' to see available anchors.",
                from_anchor
            )
        })?;

    // Stream the read path: load only the neighborhood reachable from the start
    // within depth, not the entire graph. At kernel scale a full load OOMs / takes
    // tens of seconds; this fetches just the documents it actually traverses.
    let graph = aden_graph::cache::build_neighborhood_cached(
        &opts.path,
        &resolved_anchor,
        opts.depth,
        &opts.edge_types,
    )?;

    if opts.inspect {
        println!("=== Context Assembly Inspection ===");
        println!("Start: {}", resolved_anchor.clone());
        println!("Depth: {}", opts.depth);
        println!(
            "Budget: {} tokens (auto={}, strict={})",
            effective_budget, opts.auto, opts.strict
        );
        println!("\n=== Nodes to be included ===");

        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        if let Some(start_idx) = graph.get_index(&resolved_anchor) {
            queue.push_back((start_idx, 0usize));
            while let Some((node, d)) = queue.pop_front() {
                if visited.contains(&node) || d > opts.depth {
                    continue;
                }
                visited.insert(node);
                println!("  [{}] {}", d, graph.graph[node].doc.anchor);
                for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
                    if !visited.contains(&neighbor) {
                        queue.push_back((neighbor, d + 1));
                    }
                }
            }
        }
        return Ok(());
    }

    // llm_mode=true is the default. Only raw AsciiDoc (--format aden) disables it.
    let llm_mode = opts.format != "aden";
    let asm_opts = AssemblyOptions {
        start_anchor: resolved_anchor,
        max_depth: opts.depth,
        token_budget: effective_budget,
        edge_types: opts.edge_types.clone(),
        block_filter: Vec::new(),
        include_tags: opts.include_tags.clone(),
        exclude_tags: opts.exclude_tags.clone(),
        attributes: opts.attributes.clone(),
        llm_mode,
        hydrate_root: None,
    };

    let output = match opts.format.as_str() {
        "adg" => assemble_adg(&graph, &asm_opts)?,
        "aden" | "llm" => assemble(&graph, &asm_opts)?,
        _ => {
            return Err(format!(
                "Unknown format: '{}'. Use 'llm' (default), 'adg', or 'aden' (raw AsciiDoc).",
                opts.format
            )
            .into());
        }
    };

    if let Some(out_path) = &opts.out {
        std::fs::write(out_path, output)?;
        println!("Written assembly to {}", out_path.display());
    } else {
        if opts.silent {
            print!("{}", output);
        } else {
            println!("{}", output);
        }
    }
    Ok(())
}

pub fn cmd_query(
    path: &Path,
    from: Option<&str>,
    edge_type: Option<&str>,
    depth: usize,
    backlinks: Option<&str>,
    impact: Option<&str>,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("query requires a directory path".into());
    }
    super::ensure_fresh(path);

    let graph = aden_graph::cache::build_from_directory_cached(path)?;

    let mode_count = from.is_some() as u8 + backlinks.is_some() as u8 + impact.is_some() as u8;
    if mode_count != 1 {
        return Err("exactly one of --from, --backlinks, or --impact must be specified".into());
    }

    // Resolve bare/suffix names to the full store anchors the graph uses.
    let from = from.map(|a| {
        aden_graph::cache::resolve_anchor_in_store(path, a).unwrap_or_else(|| a.to_string())
    });
    let backlinks = backlinks.map(|a| {
        aden_graph::cache::resolve_anchor_in_store(path, a).unwrap_or_else(|| a.to_string())
    });
    let impact = impact.map(|a| {
        aden_graph::cache::resolve_anchor_in_store(path, a).unwrap_or_else(|| a.to_string())
    });

    let mut results = Vec::new();

    // Parse the --edge-type filter once up front so every mode (--from,
    // --backlinks, --impact) can honor it.
    let filter_type = if let Some(et) = edge_type {
        let valid = valid_edge_types().join(", ");
        Some(
            parse_single_edge_type(et)
                .ok_or_else(|| format!("invalid edge type: '{}'. Valid: {}", et, valid))?,
        )
    } else {
        None
    };

    // Collect the edge types of ALL parallel edges from `a` to `b`. petgraph is a
    // multigraph, so a single (a -> b) pair may carry several edges of different
    // types; `find_edge` would return an arbitrary one. Callers test whether ANY
    // edge matches the desired type/filter (mirrors `traverse::ordered_neighbors`).
    let edges_between = |a, b| -> Vec<aden_core::EdgeType> {
        graph
            .graph
            .edges_connecting(a, b)
            .map(|e| e.weight().edge_type)
            .collect()
    };

    if let Some(anchor) = &from {
        let start_idx = graph.get_index(anchor).ok_or_else(|| {
            format!(
                "Anchor '{}' not found. Run 'aden list .' to see available anchors.",
                anchor
            )
        })?;

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        visited.insert(start_idx);
        queue.push_back((start_idx, 0usize));
        results.push(node_to_json(&graph.graph[start_idx], 0));

        while let Some((node, d)) = queue.pop_front() {
            if d > depth {
                continue;
            }
            for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
                // A (node -> neighbor) pair may carry several parallel edges of
                // different types (e.g. Calls AND Documents). `find_edge` returns
                // an arbitrary one, so a node could be wrongly skipped. Check ALL
                // edges between the pair and keep the neighbor if ANY edge matches
                // the filter (mirrors `traverse::ordered_neighbors`).
                let passes = match filter_type {
                    Some(ft) => edges_between(node, neighbor).contains(&ft),
                    None => true,
                };
                if !passes {
                    continue;
                }
                if visited.insert(neighbor) {
                    results.push(node_to_json(&graph.graph[neighbor], d + 1));
                    queue.push_back((neighbor, d + 1));
                }
            }
        }
    } else if let Some(anchor) = &backlinks {
        let target_idx = graph.get_index(anchor).ok_or_else(|| {
            format!(
                "Anchor '{}' not found. Run 'aden list .' to see available anchors.",
                anchor
            )
        })?;
        let mut seen = HashSet::new();
        for neighbor in graph
            .graph
            .neighbors_directed(target_idx, Direction::Incoming)
        {
            // Honor --edge-type: keep an incoming neighbor only if at least one
            // edge from it to the target matches the requested type. Check ALL
            // parallel edges (a source may link via several edge types).
            if let Some(ft) = filter_type
                && !edges_between(neighbor, target_idx).contains(&ft)
            {
                continue;
            }
            if seen.insert(neighbor) {
                results.push(node_to_json(&graph.graph[neighbor], 1));
            }
        }
    } else if let Some(anchor) = &impact {
        let start_idx = graph.get_index(anchor).ok_or_else(|| {
            format!(
                "Anchor '{}' not found. Run 'aden list .' to see available anchors.",
                anchor
            )
        })?;
        // The one shared impact SET (util.rs) — local copies of this list have
        // drifted twice (viz, understand); never inline it again.
        let impact_types = crate::util::impact_edge_types();

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        visited.insert(start_idx);
        queue.push_back((start_idx, 0usize));
        results.push(node_to_json(&graph.graph[start_idx], 0));

        while let Some((node, d)) = queue.pop_front() {
            for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
                // Parallel edges: keep the neighbor if ANY edge between the pair
                // is an impact edge, instead of testing only `find_edge`'s
                // arbitrary pick.
                if !edges_between(node, neighbor)
                    .iter()
                    .any(|et| impact_types.contains(et))
                {
                    continue;
                }
                if visited.insert(neighbor) {
                    results.push(node_to_json(&graph.graph[neighbor], d + 1));
                    queue.push_back((neighbor, d + 1));
                }
            }
        }
    }

    match format {
        "table" => {
            println!("| Anchor | Depth | Node Type |\n|=== |");
            for r in results {
                println!("| {} | {} | {} |", r["anchor"], r["depth"], r["node_type"]);
            }
        }
        _ => {
            println!("{}", serde_json::to_string_pretty(&results)?);
        }
    }
    Ok(())
}

/// Intent classification helpers.
///
/// Scores EVERY [`QueryIntent`] against the question and picks the highest,
/// rather than first-match. This fixes two defects of the old chain: missing
/// synonyms (e.g. "impact", "what breaks", "callers") and greedy shadowing
/// (a generic "what is" no longer beats a specific "what is affected by …").
///
/// Single-word keywords match whole words in the question's token set, with a
/// stem-lite `starts_with` for keywords ≥4 chars (so `fail`→`fails`/`failing`,
/// `break`→`breaks`). Multi-word phrases (containing a space) match as
/// substrings of the full lowercased question.
/// True if query word `word` is the short keyword `kw` (`<4` chars) or one of
/// its common inflected forms (`-s`, `-es`, `-ed`, `-d`, `-ing`, with a doubled
/// final consonant variant). Deliberately conservative: it matches `use`→`uses`/
/// `used`/`using` but NOT unrelated longer words like `user` or `useful`.
fn is_short_inflection(word: &str, kw: &str) -> bool {
    if word == kw {
        return true;
    }
    if !word.starts_with(kw) {
        return false;
    }
    let suffix = &word[kw.len()..];
    matches!(suffix, "s" | "es" | "ed" | "d" | "ing")
        // doubled-consonant forms: run→running, sum→summed
        || (kw.chars().last().is_some_and(|c| c.is_ascii_alphabetic())
            && suffix.len() >= 4
            && suffix.starts_with(kw.chars().last().unwrap())
            && matches!(&suffix[1..], "ed" | "ing"))
}

pub fn classify_intent(question: &str) -> QueryIntent {
    use QueryIntent::*;

    let q = question.to_lowercase();
    let words: HashSet<&str> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();

    // (single-word keywords, multi-word phrases) per intent. Phrases are
    // matched by substring on the full string; single words by whole-word
    // (with a stem-lite prefix match for words ≥4 chars).
    let intents: &[(QueryIntent, &[&str], &[&str])] = &[
        (
            Debug,
            &[
                "fail",
                "error",
                "panic",
                "crash",
                "broken",
                "break",
                "bug",
                "wrong",
                "overshoot",
                "hang",
                "debug",
                "troubleshoot",
                "diagnose",
            ],
            &["doesn't work", "not working", "why is", "why does"],
        ),
        (
            Usage,
            &["usage", "example"],
            &["how do i", "how to", "how can i", "how should i use"],
        ),
        (
            Refactor,
            &[
                "refactor",
                "rewrite",
                "rename",
                "restructure",
                "extract",
                "simplify",
            ],
            &["clean up", "move ", "split "],
        ),
        (
            Impact,
            &[
                "depend",
                "dependency",
                "caller",
                "consumer",
                "affected",
                "affect",
                "impact",
                "downstream",
                "references",
                "ripple",
            ],
            &[
                "blast radius",
                "what uses",
                "who calls",
                "what calls",
                "what breaks",
                "what is affected",
                "if i change",
                "if i modify",
            ],
        ),
        (
            Explain,
            &["explain", "describe", "overview", "purpose"],
            &["what is", "what does", "how does", "what's"],
        ),
        (
            List,
            &["list", "enumerate"],
            &["show me all", "give me a list", "what are all"],
        ),
        (
            Compare,
            &["compare", "versus", "differ"],
            &["vs ", "difference between"],
        ),
        (
            // Count's signal is the leading phrase ("how many", "number of",
            // "count of"). The bare word "total" was dropped: it is far more
            // often incidental English ("the total impact of X") than a counting
            // request, and as a single-word tie it wrongly pulled Impact/Debug
            // questions into a depth-1 tally.
            Count,
            &[],
            &["how many", "number of", "count of"],
        ),
    ];

    // Score every intent.
    let scores: Vec<usize> = intents
        .iter()
        .map(|(_, keywords, phrases)| {
            let mut score = 0usize;
            for kw in *keywords {
                let hit = words.contains(kw)
                    || (kw.len() >= 4 && words.iter().any(|w| w.starts_with(kw)))
                    // Short keywords (<4 chars) only got an exact-word match, so
                    // `use` missed `uses`/`used`/`using`. Allow the keyword plus a
                    // small set of inflectional suffixes — tight enough to avoid
                    // matching unrelated longer words (`user`, `useful`).
                    || (kw.len() < 4
                        && words.iter().any(|w| is_short_inflection(w, kw)));
                if hit {
                    score += 1;
                }
            }
            for phrase in *phrases {
                if q.contains(phrase) {
                    score += 1;
                }
            }
            score
        })
        .collect();

    let max_score = scores.iter().copied().max().unwrap_or(0);
    // General is the only intent chosen when nothing matched.
    if max_score == 0 {
        return General;
    }

    // Tie/fallback priority order. Among intents tied for the highest score,
    // pick the one earliest here. Count precedes Impact so a counting question
    // that co-mentions an Impact word ("how many callers does X have") resolves
    // to Count (a depth-1 tally) rather than a depth-3 blast-radius traversal;
    // pure Impact questions ("blast radius of X", no Count phrase) outscore Count
    // and are unaffected by the relative order.
    const PRIORITY: &[QueryIntent] = &[
        Debug, Count, Impact, Refactor, Usage, Compare, List, Explain,
    ];
    let tied = |variant: &QueryIntent| -> bool {
        intents.iter().zip(scores.iter()).any(|((iv, ..), s)| {
            *s == max_score && std::mem::discriminant(iv) == std::mem::discriminant(variant)
        })
    };
    for variant in PRIORITY {
        if tied(variant) {
            return variant.clone();
        }
    }
    General
}

pub fn edge_types_for_intent(intent: &QueryIntent) -> Vec<aden_core::EdgeType> {
    use aden_core::EdgeType::*;
    // Include both code edges AND semantic edges for all intents.
    // NOTE: `PartOf` is deliberately excluded — it is the symbol->module
    // containment edge, and traversing it turns every module into a hub that
    // drags in all sibling symbols (and their doc code-blocks), flooding the
    // context. Module-level overviews are still available via
    // `aden asm --from mod-<name>` (which traverses all edges by default).
    // `Mentions` rides with the semantic set: from a prose anchor it hops to
    // the code symbols the prose names (Wave 2) — exactly the doc→code bridge
    // conceptual answers need, and harmless from code anchors (no outgoing
    // Mentions there).
    let semantic = vec![
        IsA,
        RelatesTo,
        SimilarTo,
        AssociatedWith,
        Explains,
        Mentions,
    ];
    let mut edges: Vec<aden_core::EdgeType> = match intent {
        // Emitter-less edge types (Constrains/Invokes) were trimmed from every
        // intent set (ADR-007 §1): naming a type with zero live edges filters
        // nothing and reads like coverage.
        QueryIntent::Debug => vec![Documents, Calls, Requires]
            .into_iter()
            .chain(semantic.clone())
            .collect(),
        // `Demonstrates` on usage/explain intents pulls a doc listing that
        // exercises the symbol into context — the "working example" payoff.
        QueryIntent::Usage => vec![Uses, Requires, Documents, Demonstrates]
            .into_iter()
            .chain(semantic.clone())
            .collect(),
        QueryIntent::Explain => vec![Uses, Calls, Implements, Documents, Demonstrates]
            .into_iter()
            .chain(semantic.clone())
            .collect(),
        QueryIntent::Refactor => vec![Calls, Uses, Mutates, Supersedes, Amends]
            .into_iter()
            .chain(semantic.clone())
            .collect(),
        QueryIntent::Impact => vec![Uses, Calls]
            .into_iter()
            .chain(semantic.clone())
            .collect(),
        QueryIntent::List => vec![Uses, Documents]
            .into_iter()
            .chain(semantic.clone())
            .collect(),
        QueryIntent::Compare => vec![Uses, Documents]
            .into_iter()
            .chain(semantic.clone())
            .collect(),
        QueryIntent::Count => vec![Documents, Uses]
            .into_iter()
            .chain(semantic.clone())
            .collect(),
        QueryIntent::General => vec![Uses, Documents].into_iter().chain(semantic).collect(),
    };
    // Module containment used to ride on `Documents` edges (module→symbol); those are
    // now typed `Contains`. Any intent that traversed `Documents` to reach a module's
    // members must also traverse `Contains`, or top-down overviews would go dark.
    if edges.contains(&Documents) && !edges.contains(&Contains) {
        edges.push(Contains);
    }
    // The call graph is the densest, most useful relationship for code questions,
    // so it must be traversable for EVERY intent — not just Explain/Refactor.
    if !edges.contains(&Calls) {
        edges.push(Calls);
    }
    edges
}

pub fn depth_for_intent(intent: &QueryIntent) -> usize {
    // Depths match the documented strategy table (docs/commands.adoc). Shallow
    // traversal keeps the assembled context dense and on-topic; budget still
    // bounds total size, but a tight depth keeps what's included relevant.
    match intent {
        QueryIntent::Debug => 3,
        QueryIntent::Usage => 2,
        QueryIntent::Explain => 2,
        QueryIntent::Refactor => 4,
        QueryIntent::Impact => 3,
        QueryIntent::List => 2,
        QueryIntent::Compare => 3,
        QueryIntent::Count => 1,
        QueryIntent::General => 2,
    }
}

pub fn block_filter_for_intent(intent: &QueryIntent) -> Vec<aden_asm::traverse::BlockKind> {
    use aden_asm::traverse::BlockKind::*;
    match intent {
        QueryIntent::Debug => vec![Table, Admonition, Paragraph],
        QueryIntent::Usage => vec![Listing, Table, DescriptionList],
        QueryIntent::Explain => vec![Paragraph, Table, Listing],
        QueryIntent::Refactor => vec![Table, Admonition, Paragraph],
        QueryIntent::Impact => vec![Table, Listing],
        QueryIntent::List => vec![Table, Listing, DescriptionList],
        QueryIntent::Compare => vec![Paragraph, Table],
        QueryIntent::Count => vec![Table, Listing],
        QueryIntent::General => vec![Paragraph, Table, Listing, Admonition, DescriptionList],
    }
}

/// Assembled output below this substantive-token estimate is treated as a thin
/// stub — the anchor resolved to a bare declaration with no useful content.
/// `ask` escalates through the rendering/topology ladder (see `cmd_ask`) when
/// this threshold is not met; the effective floor scales with the budget as
/// `max(THIN_STUB_TOKEN_THRESHOLD, budget / 10)`.
const THIN_STUB_TOKEN_THRESHOLD: usize = 150;

/// Substantive-token estimate of an assembled body: the byte-based estimator
/// (`bytes.div_ceil(4)`, the same one the assembler budgets with) applied to
/// the body MINUS the line classes that carry no explanatory value — blank
/// lines, `---` separators, `//` header/meta lines, bare-title lines (a line
/// that is only a symbol/module name), and `calls:` scaffolding. Raw byte
/// length rewarded callee tables and bare titles (the bulk-as-substance trap),
/// letting a near-empty assembly pass for substantial. Mirrors the density
/// eval's definition (`scripts/eval_ask_density.py`) so the floor checked here
/// is the metric the gates measure.
fn substantive_token_estimate(body: &str) -> usize {
    let is_bare_title = |t: &str| {
        !t.is_empty()
            && t.chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '-' | '.' | '#' | '/'))
    };
    let bytes: usize = body
        .lines()
        .filter(|line| {
            let t = line.trim();
            !(t.is_empty()
                || t == "---"
                || t.starts_with("//")
                || t.starts_with("calls:")
                || is_bare_title(t))
        })
        .map(|line| line.len() + 1)
        .sum();
    bytes.div_ceil(4)
}

/// For a thin-routed anchor, find its functional community and return richer
/// replacement-seed candidates: `(seeds, label, member_count)`, with `seeds`
/// ranked by RELEVANCE to the question (BM25-scored against it), never by
/// degree: degree ≠ relevance, and picking the cluster's highest-degree hub
/// (its center of gravity) re-created the M14 hub-over-leaf bias at the
/// fallback layer — swapping a correctly-routed leaf for whatever
/// `mod-`adjacent giant anchors the community. Synthetic `mod-*` hub nodes,
/// test anchors, and the anchor itself are excluded; unranked members score 0
/// and lose ties lexicographically, so the ranking is deterministic. The
/// caller tries seeds in order and keeps the first whose assembly is actually
/// substantive. Returns `None` when the anchor isn't in a multi-member
/// community.
fn community_seeds_for(
    path: &Path,
    anchor: &str,
    question: &str,
    max_seeds: usize,
) -> Option<(Vec<String>, String, usize)> {
    let communities = {
        let graph = aden_graph::cache::build_from_directory_cached(path).ok()?;
        aden_graph::community::detect_communities(&graph, 1.0)
    };
    let group = communities
        .into_iter()
        .find(|c| c.iter().any(|a| a == anchor))?;
    if group.len() < 2 {
        return None;
    }
    let idx = load_or_build_index(path).ok()?;
    let results = query_index(&idx, question);
    let score_of = |a: &str| {
        results
            .iter()
            .find(|r| r.anchor == a)
            .map(|r| r.score)
            .unwrap_or(0.0)
    };
    // Test-ness must be judged by the real source path too: the module-form
    // anchor flattens the directory (`…/mcp_flag_parity.rs#…` for a file in
    // `tests/`), exactly the [`is_test_result`] caveat.
    let is_test_member = |a: &str| {
        is_test_anchor(a)
            || anchor_source_file(path, a)
                .map(|f| is_test_source_path(&f))
                .unwrap_or(false)
    };
    let mut members: Vec<&String> = group
        .iter()
        .filter(|a| !a.starts_with("mod-") && a.as_str() != anchor && !is_test_member(a))
        .collect();
    members.sort_by(|a, b| {
        score_of(b)
            .partial_cmp(&score_of(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.cmp(b))
    });
    let seeds: Vec<String> = members.into_iter().take(max_seeds).cloned().collect();
    if seeds.is_empty() {
        return None;
    }
    let label = crate::commands::communities::dominant_module(&group);
    let n = group.len();
    Some((seeds, label, n))
}

/// The real source file of an anchor, from the store document's `source_file`
/// attribute (best-effort; `None` when the store/document/attribute is absent).
/// `ask --explain` uses it so a bare doc anchor can be judged by the file it
/// lives in (e.g. `…#philosophy` → `docs/philosophy.adoc`).
fn anchor_source_file(path: &Path, anchor: &str) -> Option<String> {
    let (store_path, _) = aden_paths::resolve_read_store(path);
    let storage = aden_store::Storage::open_existing(store_path.to_str()?).ok()?;
    storage
        .get_document(anchor)
        .ok()
        .flatten()
        .and_then(|d| d.attributes.get("source_file").cloned())
}

/// Routing transparency payload for `ask --explain`: top candidates with the
/// signals selection actually used, which path decided, and whether the
/// thin-stub fallback swapped (or was suppressed). Printed in the same `// `
/// comment style as the summary block (`asm --inspect` is the model).
#[derive(Default)]
struct AskExplain {
    overview_engaged: bool,
    overview_note: String,
    decision: String,
    fallback: String,
    candidates: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_ask(
    path: &Path,
    question: &str,
    from_override: Option<&str>,
    budget: usize,
    model: Option<&str>,
    intent_override: Option<QueryIntent>,
    depth_override: Option<usize>,
    edge_types_override: Option<Vec<aden_core::EdgeType>>,
    strict: bool,
    explain: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_asm::traverse::{AssemblyOptions, assemble_with_anchors};
    use aden_core::savings::{BASELINE_MAX_FILES, SavingsEstimate};

    if !path.is_dir() {
        return Err("ask requires a directory path".into());
    }
    super::ensure_fresh(path);

    // Intent is classified up front so the overview signal can feed ANCHOR
    // SELECTION (the historical gap: intent only shaped traversal AFTER the
    // anchor was already chosen). Pure function — computing it earlier changes
    // nothing else.
    let intent_was_overridden = intent_override.is_some();
    let intent = intent_override.unwrap_or_else(|| classify_intent(question));

    let mut xp = AskExplain::default();

    // Step 1: Resolve question to an anchor via search, or use override.
    // `ask` is the fuzzy "answer my question well" path, so it leans on the
    // relevance boost BY DEFAULT (unlike `asm`, which is hard-cap and opt-in via
    // --auto). Capture the average search relevance so the budget can be scaled;
    // a pinned --from anchor has no search relevance, so it stays at base.
    // `alt_candidates` are near-tie runner-up anchors (search-space names, not yet
    // store-resolved). They stay empty for a clear winner or a pinned `--from`, so
    // those paths assemble exactly as before. When routing is ambiguous they let us
    // seed shallow context from the alternates rather than betting the whole budget
    // on a single, possibly-misranked anchor.
    let (start_anchor, avg_score, alt_candidates) = if let Some(anchor) = from_override {
        xp.decision = "pinned by --from (no search routing)".to_string();
        (anchor.to_string(), None, Vec::new())
    } else {
        let idx = load_or_build_index(path)?;
        let results = query_index(&idx, question);
        if results.is_empty() {
            println!("No relevant documents found for: {}", question);
            println!(
                "Tips:\n  - Use more specific keywords from the codebase.\n  - Try `aden search <term>` to see available anchors.\n  - Or pin an anchor with --from <anchor>."
            );
            return Ok(());
        }
        let avg: f64 = results.iter().map(|r| r.score).sum::<f64>() / results.len() as f64;

        // Conceptual routing: when the question is a broad/overview one, prefer
        // substantive prose docs within the noise band (see the module-level
        // block comment above `OVERVIEW_PHRASES`). A miss (no prose in band)
        // falls through to the default selection unchanged.
        let overview = is_overview_query(question, &intent);
        xp.overview_engaged = overview;
        xp.overview_note = if overview {
            "engaged (broad intent + overview phrasing, no symbol-like token)".to_string()
        } else {
            "not engaged".to_string()
        };
        let token_count = |a: &str| idx.doc_token_count(a);
        let primary = if overview {
            let indegree = doc_reference_indegree(path);
            match resolve_anchor_overview(question, &results, &token_count, &indegree) {
                Some((anchor, why)) => {
                    xp.decision = why;
                    anchor
                }
                None => {
                    let (anchor, why) =
                        resolve_anchor_fuzzy_with_reason(question, &results, token_count);
                    xp.decision =
                        format!("overview engaged but no prose doc in score band; {}", why);
                    anchor
                }
            }
        } else {
            let (anchor, why) = resolve_anchor_fuzzy_with_reason(question, &results, token_count);
            xp.decision = why.to_string();
            anchor
        };
        if explain {
            let top_score = results[0].score;
            xp.candidates = results
                .iter()
                .take(8)
                .enumerate()
                .map(|(i, r)| {
                    format!(
                        "{}. {} {} score={} pattern={:?} class={} tokens={} test={}",
                        i + 1,
                        if (top_score - r.score) <= ANCHOR_NOISE_BAND {
                            "*"
                        } else {
                            " "
                        },
                        r.anchor,
                        fmt_score(r.score),
                        AnchorPattern::from_anchor(&r.anchor),
                        if AnchorPattern::is_prose_doc(&r.anchor) {
                            "doc"
                        } else {
                            "code"
                        },
                        idx.doc_token_count(&r.anchor),
                        if is_test_result(r) { "yes" } else { "no" },
                    )
                })
                .collect();
        }
        // Up to 2 in-band alternates, deduped against the (possibly non-rank-1)
        // primary. Empty ⇒ clear winner ⇒ unchanged single-seed behavior below.
        let alts = inband_alternate_candidates(&primary, &results, 2);
        (primary, Some(avg), alts)
    };

    // Apply the relevance boost by default; `--strict` opts out and treats
    // --budget as an exact cap (deterministic size for callers/agents). The
    // user's --budget is the BASE the boost multiplies.
    let effective_budget = match (strict, avg_score) {
        (false, Some(avg)) => auto_boosted_budget(budget, avg),
        _ => budget,
    };

    println!("// Aden Ask: '{}' → [[{}]]", question, start_anchor);
    if from_override.is_some() {
        println!("// (pinned by --from)");
    }
    println!();

    // Step 2: Route assembly strategy from the (already classified) intent.
    // Any of intent, depth, or edge types may be pinned by the caller to bypass
    // automatic routing (`aden ask --intent/--depth/--edge-types`).
    let edge_types = edge_types_override.unwrap_or_else(|| edge_types_for_intent(&intent));
    let depth = depth_override.unwrap_or_else(|| depth_for_intent(&intent));

    let strategy_label = if intent_was_overridden {
        format!("{:?} (override)", intent)
    } else {
        format!("{:?}", intent)
    };

    println!(
        "// Strategy: {} | Depth: {} | Edges: {:?}",
        strategy_label,
        depth,
        edge_types
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!();

    // Step 3: Resolve the starting anchor against the store (no full-graph
    // load). Prefer an unambiguous exact/suffix match; if the search-derived
    // anchor cannot be resolved, fall back to the deterministic project root
    // rather than a random node, so the agent gets a coherent overview.
    let start_anchor = match aden_graph::cache::resolve_anchor_in_store(path, &start_anchor) {
        Some(a) => a,
        None => {
            if aden_graph::cache::resolve_anchor_in_store(path, "mod-project").is_some() {
                eprintln!(
                    "NOTE: '{}' is not a graph anchor; using project root 'mod-project'.",
                    start_anchor
                );
                "mod-project".to_string()
            } else {
                return Err(format!(
                    "Anchor '{}' not found. Run 'aden list .' to see available anchors.",
                    start_anchor
                )
                .into());
            }
        }
    };

    // The store-resolved PRIMARY, captured before any thin-stub fallback can
    // swap the final anchor — `--explain` reports both so a swap is never
    // silent (`// Primary :` vs the summary's `// Anchor :`).
    let primary_anchor = start_anchor.clone();

    // Resolve the near-tie alternates against the store, using the same validator
    // as the primary. Skip any that don't resolve, collapse onto the primary, or
    // duplicate each other. We deliberately do NOT fall back to `mod-project` for
    // alternates — an alternate that doesn't resolve is simply dropped.
    let resolved_alts: Vec<String> = {
        let mut seen = vec![start_anchor.clone()];
        let mut out = Vec::new();
        for cand in &alt_candidates {
            if let Some(a) = aden_graph::cache::resolve_anchor_in_store(path, cand)
                && !seen.contains(&a)
            {
                seen.push(a.clone());
                out.push(a);
            }
        }
        out
    };

    let block_filter = block_filter_for_intent(&intent);
    let edge_types_str = edge_types
        .iter()
        .map(|e| format!("{:?}", e))
        .collect::<Vec<_>>()
        .join(", ");

    // Source-span hydration root: node `source_file` attributes are
    // project-root-relative, so spans must be resolved against the real root
    // even when `path` is a subdirectory of the project.
    let hydrate_root = find_project_root(path);

    // Helper: assemble one seed's neighborhood at a given depth/budget with a
    // given block filter, optionally folding in the seed's callers (incoming
    // Calls/Uses) — the escalation ladder's rendering and topology knobs.
    // Hydration is always on: `ask` answers from real source bodies, not just
    // stored summaries (the store never holds function bodies).
    // Core assembly helper — returns (text, included_anchors) so callers can
    // resolve source files for baseline estimation without a second traversal.
    let assemble_seed_with = |seed: &str,
                              seed_depth: usize,
                              seed_budget: usize,
                              filter: &[aden_asm::traverse::BlockKind],
                              with_callers: bool|
     -> Result<(String, Vec<String>), Box<dyn std::error::Error>> {
        let graph = if with_callers {
            aden_graph::cache::build_neighborhood_with_callers(
                path,
                seed,
                seed_depth,
                &edge_types,
                // Incoming context worth folding in: callers/users, plus doc
                // listings that exercise the seed (Demonstrates runs
                // listing→symbol, so a symbol's working examples sit on its
                // incoming side — Wave 2's "show me an example" payoff).
                // `Mentions` is deliberately NOT folded: a prose name-drop is
                // a hint, and 16 caller slots are better spent on real code.
                &[
                    aden_core::EdgeType::Calls,
                    aden_core::EdgeType::Uses,
                    aden_core::EdgeType::Demonstrates,
                ],
                // Test fixtures assert, they don't explain — same policy that
                // keeps them from winning routing, judged by BOTH the anchor
                // and the real source path (anchors flatten `tests/` away).
                &|a, src| !is_test_anchor(a) && !src.map(is_test_source_path).unwrap_or(false),
            )?
        } else {
            aden_graph::cache::build_neighborhood_cached(path, seed, seed_depth, &edge_types)?
        };
        let opts = AssemblyOptions {
            start_anchor: seed.to_string(),
            max_depth: seed_depth,
            token_budget: seed_budget,
            edge_types: edge_types.clone(),
            block_filter: filter.to_vec(),
            include_tags: Vec::new(),
            exclude_tags: Vec::new(),
            attributes: Vec::new(),
            llm_mode: true, // aden ask always targets an LLM — emit clean prose
            hydrate_root: Some(hydrate_root.clone()),
        };
        Ok(assemble_with_anchors(&graph, &opts)?)
    };
    // Convenience wrapper for callers that only need the assembled text.
    let assemble_seed_str = |seed: &str,
                             seed_depth: usize,
                             seed_budget: usize,
                             filter: &[aden_asm::traverse::BlockKind],
                             with_callers: bool|
     -> Result<String, Box<dyn std::error::Error>> {
        assemble_seed_with(seed, seed_depth, seed_budget, filter, with_callers)
            .map(|(text, _)| text)
    };
    let assemble_seed = |seed: &str, seed_depth: usize, seed_budget: usize| {
        assemble_seed_str(seed, seed_depth, seed_budget, &block_filter, false)
    };

    // Clear winner ⇒ today's behavior exactly: one seed, full budget. Ambiguous ⇒
    // primary takes the majority of the budget at full depth, and each shallow
    // alternate gets an even slice of the remainder, appended with a brief header.
    // This is paid for ONLY on near-ties, and the total stays within the budget.
    // `primary_text` tracks the body contributed by the PRIMARY anchor alone,
    // so the thin-stub check below can't be fooled by a fat alternate padding
    // the combined output past the threshold.
    // Anchors included in the primary seed's assembly — used for baseline-file
    // resolution in the savings estimate. Captured from the primary call only;
    // alternates and thin-stub fallbacks contribute bytes but not more files to
    // the baseline (the primary is what drove routing).
    let primary_anchors: Vec<String>;
    let primary_text;
    let assembled = if resolved_alts.is_empty() {
        let (seed_text, seed_anchors) =
            assemble_seed_with(&start_anchor, depth, effective_budget, &block_filter, false)?;
        primary_anchors = seed_anchors;
        primary_text = seed_text.clone();
        seed_text
    } else {
        let primary_budget = effective_budget * 60 / 100;
        let shallow_depth = depth.min(1);
        let (primary_seed_text, seed_anchors) =
            assemble_seed_with(&start_anchor, depth, primary_budget, &block_filter, false)?;
        primary_anchors = seed_anchors;
        let mut combined = primary_seed_text;
        primary_text = combined.clone();
        let mut used = combined.len().div_ceil(4);
        for alt in &resolved_alts {
            let sep = "\n\n---\n\n";
            let header = format!("// alternate (ambiguous match): [[{}]]\n", alt);
            let overhead = sep.len().div_ceil(4) + header.len().div_ceil(4);
            let remaining = effective_budget.saturating_sub(used + overhead);
            if remaining < 32 {
                break;
            }
            let alt_text = assemble_seed(alt, shallow_depth, remaining)?;
            if alt_text.trim().is_empty() {
                continue;
            }
            combined.push_str(sep);
            combined.push_str(&header);
            combined.push_str(&alt_text);
            used += overhead + alt_text.len().div_ceil(4);
        }
        combined
    };

    // Thin-stub handling: if the resolved anchor assembled almost nothing
    // (bare declaration, empty node), broaden — but never by silently swapping
    // a correctly-routed anchor for a community hub (the defect that replaced
    // `tool_from_spec` with `AdenMcpServer::new` after routing was RIGHT).
    //
    // Prose document anchors keep their dedicated path: a document the router
    // chose deliberately IS the answer to the question (especially a
    // conceptual one); a thin doc broadens WITHIN its own document or stays.
    //
    // CODE anchors go through a floor-checked escalation ladder instead:
    // rendering is broadened before topology, the anchor only ever changes on
    // the final community rung, and only for a replacement that itself clears
    // the floor — otherwise the routed anchor is kept with an explicit NOTE.
    let (assembled, start_anchor) = {
        let est = primary_text.len().div_ceil(4);
        if est < THIN_STUB_TOKEN_THRESHOLD && AnchorPattern::is_prose_doc(&start_anchor) {
            // Broaden WITHIN the document, never away from it: the routed file
            // IS the answer; the canonical (most cross-referenced) same-file
            // anchor sits in the prose reference web and assembles connected
            // context where a bare heading shell yields ~17 tokens. Used only
            // when it actually assembles more than the shell did.
            let broadened = same_file_canonical_anchor(&start_anchor, &doc_reference_indegree(path))
                .and_then(|(canon, d)| {
                    let body = assemble_seed(&canon, depth, effective_budget).ok()?;
                    if body.len().div_ceil(4) <= est {
                        return None;
                    }
                    eprintln!(
                        "NOTE: '{}' assembled thin (~{} tokens); broadening within the document to its canonical anchor [[{}]] (cross-reference in-degree {}).",
                        start_anchor, est, canon, d
                    );
                    xp.fallback = format!(
                        "thin (~{} tokens); broadened WITHIN the document to [[{}]] (cross-reference in-degree {})",
                        est, canon, d
                    );
                    Some((body, canon))
                });
            match broadened {
                Some(pair) => pair,
                None => {
                    eprintln!(
                        "NOTE: '{}' assembled thin (~{} tokens) but is a prose document; keeping it (fallback swap suppressed).",
                        start_anchor, est
                    );
                    xp.fallback = format!(
                        "suppressed: primary assembled thin (~{} tokens) but is a prose document — kept",
                        est
                    );
                    (assembled, start_anchor)
                }
            }
        } else if !AnchorPattern::is_prose_doc(&start_anchor)
            && substantive_token_estimate(&primary_text) < effective_budget / 2
        {
            // F3 — escalation ladder for underfull/thin CODE anchors, driven
            // by SUBSTANTIVE tokens (not raw bytes — bare titles and `calls:`
            // scaffolding don't count). Two thresholds:
            //   target (50% of budget) — the densify goal: below it the
            //     non-anchor-changing rungs run, in order:
            //       (a) re-render without the intent block filter;
            //       (b) fold in the seed's callers (incoming Calls/Uses) —
            //           outgoing-only traversal starves at leaves;
            //       (c) deepen the traversal by 1;
            //     keeping the best body seen, stopping early at the target.
            //   floor (max(150, 15% of budget)) — the thin bar: only below it
            //     may rung (d) community-broaden, seeded by the most
            //     query-relevant member (never by degree), never for a pinned
            //     `--from`, and only when the replacement itself clears the
            //     floor — otherwise the routed anchor is kept, with a NOTE.
            let target = effective_budget / 2;
            let floor = THIN_STUB_TOKEN_THRESHOLD.max(effective_budget * 15 / 100);
            let subst = substantive_token_estimate(&primary_text);
            let unfiltered: Vec<aden_asm::traverse::BlockKind> = Vec::new();
            let rungs: [(&str, usize, bool); 3] = [
                ("unfiltered rendering", depth, false),
                ("callers added", depth, true),
                ("depth +1", depth + 1, true),
            ];
            let mut best_body = assembled;
            let mut best_subst = subst;
            let mut best_rung: Option<&str> = None;
            for (label, d, with_callers) in rungs {
                if let Ok(body) = assemble_seed_str(
                    &start_anchor,
                    d,
                    effective_budget,
                    &unfiltered,
                    with_callers,
                ) {
                    let s = substantive_token_estimate(&body);
                    if s > best_subst {
                        best_body = body;
                        best_subst = s;
                        best_rung = Some(label);
                    }
                    if best_subst >= target {
                        break;
                    }
                }
            }
            if best_subst >= floor {
                if let Some(label) = best_rung {
                    eprintln!(
                        "NOTE: '{}' assembled underfull (~{} substantive tokens of {} budget); escalated without changing the anchor ({}).",
                        start_anchor, subst, effective_budget, label
                    );
                    xp.fallback = format!(
                        "underfull (~{} substantive tokens of {} budget); escalated WITHOUT changing the anchor ({})",
                        subst, effective_budget, label
                    );
                }
                (best_body, start_anchor)
            } else {
                // (d) Community supplement — the routed anchor's own
                // neighborhood is exhausted below the floor, so the remaining
                // budget is packed with the assemblies of the community's most
                // QUERY-RELEVANT members (never its highest-degree hub: degree
                // ≠ relevance, and a degree pick re-created the M14
                // hub-over-leaf bias here, swapping a correct anchor for a
                // `mod-`adjacent giant). The routed anchor is KEPT — its body
                // stays first and the summary anchor stays truthful; the
                // members ride behind explicit `// community member:` markers.
                // Every appended byte (headers and separators included) is
                // charged against the budget before assembling each member, so
                // the combined body stays within it.
                let supplemented = if from_override.is_none() {
                    community_seeds_for(path, &start_anchor, question, 3).and_then(
                        |(seeds, label, n)| {
                            let mut combined = best_body.clone();
                            let mut used = combined.len().div_ceil(4);
                            let header = format!(
                                "\n\n---\n\n// Functional community supplement: {} ({} symbols, most query-relevant members; anchor [[{}]] kept)",
                                label, n, start_anchor
                            );
                            let mut header_pending = Some(header);
                            for seed in seeds {
                                let marker = format!("\n\n// community member: [[{}]]\n", seed);
                                let overhead = header_pending.as_ref().map_or(0, |h| h.len())
                                    + marker.len();
                                let remaining = effective_budget
                                    .saturating_sub(used + overhead.div_ceil(4));
                                if remaining < 64 {
                                    break;
                                }
                                let Ok(body) = assemble_seed_str(
                                    &seed,
                                    depth.min(2),
                                    remaining,
                                    &unfiltered,
                                    true,
                                ) else {
                                    continue;
                                };
                                if body.trim().is_empty() {
                                    continue;
                                }
                                if let Some(h) = header_pending.take() {
                                    combined.push_str(&h);
                                    used += h.len().div_ceil(4);
                                }
                                combined.push_str(&marker);
                                combined.push_str(&body);
                                used += marker.len().div_ceil(4) + body.len().div_ceil(4);
                            }
                            if substantive_token_estimate(&combined) < floor {
                                return None;
                            }
                            eprintln!(
                                "NOTE: '{}' stayed thin through escalation (~{} substantive tokens); kept, supplemented with community '{}' ({} symbols, members by query relevance).",
                                start_anchor, best_subst, label, n
                            );
                            Some(combined)
                        },
                    )
                } else {
                    None
                };
                match supplemented {
                    Some(body) => {
                        xp.fallback = format!(
                            "thin after full escalation; kept [[{}]] and supplemented with its community's most query-relevant members",
                            start_anchor
                        );
                        (body, start_anchor)
                    }
                    None => {
                        eprintln!(
                            "NOTE: '{}' assembled thin (~{} substantive tokens) and nothing cleared the floor ({}); keeping the routed anchor.",
                            start_anchor, best_subst, floor
                        );
                        xp.fallback = format!(
                            "thin; escalation exhausted — kept [[{}]] (nothing cleared floor {})",
                            start_anchor, floor
                        );
                        (best_body, start_anchor)
                    }
                }
            }
        } else {
            (assembled, start_anchor)
        }
    };

    // Truthful fallback record for --explain (the suppressed case set its own).
    if xp.fallback.is_empty() {
        xp.fallback = if start_anchor == primary_anchor {
            "none".to_string()
        } else {
            format!(
                "thin-stub fallback swapped [[{}]] → [[{}]]",
                primary_anchor, start_anchor
            )
        };
    }

    // `--explain` block: the routing decision made transparent. The `Primary`
    // line is the routed anchor BEFORE any fallback; the summary's `Anchor`
    // line below remains the FINAL anchor — when they differ, the `Fallback`
    // line says why. (Format consumed by scripts/eval_corpus.py --mode ask.)
    let print_explain = |xp: &AskExplain| {
        println!("// ── Ask Routing Explain ─────────────────────────");
        println!(
            "//   Intent   : {:?}{}",
            intent,
            if intent_was_overridden {
                " (override)"
            } else {
                ""
            }
        );
        println!("//   Overview : {}", xp.overview_note);
        if !xp.candidates.is_empty() {
            println!(
                "//   Candidates (top {}, * = within noise band {} of top score):",
                xp.candidates.len(),
                ANCHOR_NOISE_BAND
            );
            for line in &xp.candidates {
                println!("//     {}", line);
            }
        }
        println!("//   Decision : {}", xp.decision);
        println!("//   Fallback : {}", xp.fallback);
        match anchor_source_file(path, &primary_anchor) {
            Some(src) if !src.is_empty() => {
                println!("//   Primary  : {} (source: {})", primary_anchor, src)
            }
            _ => println!("//   Primary  : {}", primary_anchor),
        }
    };

    // Savings estimate: compare tokens Aden returned against a grep-read
    // baseline of the distinct source files the primary assembly touched.
    // Baseline = sum of on-disk bytes for up to BASELINE_MAX_FILES distinct
    // source files, capped and priced at the default tier (Opus 4.8).
    let savings_est = {
        let mut seen_files: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut baseline_bytes: usize = 0;
        for anchor in &primary_anchors {
            if seen_files.len() >= BASELINE_MAX_FILES {
                break;
            }
            if let Some(src) = anchor_source_file(path, anchor) {
                if src.is_empty() || seen_files.contains(&src) {
                    continue;
                }
                // source_file attribute is repo-root-relative; resolve against
                // the same hydration root that the assembler uses.
                let abs = hydrate_root.join(&src);
                if let Ok(meta) = std::fs::metadata(&abs) {
                    seen_files.insert(src);
                    baseline_bytes += meta.len() as usize;
                }
            }
        }
        let baseline_files = seen_files.len();
        SavingsEstimate::from_bytes(assembled.len(), baseline_bytes, baseline_files)
    };
    // Persist the estimate to the ledger (best-effort; never errors the command).
    super::savings_store::record(&hydrate_root, &savings_est);

    // Step 4: Send to LLM or print raw context
    if let Some(model_spec) = model {
        if explain {
            print_explain(&xp);
            println!("// ────────────────────────────────────────────────");
        }
        query_llm(model_spec, question, &assembled, &start_anchor)?;
    } else {
        // Show context with metadata for LLMs
        println!("<!-- ADEN CONTEXT ASSEMBLY -->");
        println!("<!-- Question: {} -->", question);
        // Note the boost when the default relevance scaling raised the budget
        // above the user's base, so the effective cap the assembler used is
        // transparent.
        let boosted = effective_budget > budget;
        let budget_note = if boosted {
            format!("{} (boosted from {})", effective_budget, budget)
        } else {
            format!("{}", effective_budget)
        };
        println!(
            "<!-- Anchor: {} | Depth: {} | Budget: {} -->",
            start_anchor, depth, budget_note
        );
        if !resolved_alts.is_empty() {
            // Make the ambiguity-driven seeding transparent: these shallow seeds
            // were appended because routing was a near-tie.
            println!(
                "<!-- Alternates (ambiguous, shallow): {} -->",
                resolved_alts.join(", ")
            );
        }
        println!("<!-- Strategy: {:?} -->", intent);
        println!("<!-- Edge Types: {} -->", edge_types_str);
        println!();

        let bytes = assembled.len();
        // Tokens estimated with the same ~4-bytes/token heuristic the assembler
        // budgets against, so the label compares like with like.
        let est_tokens = bytes.div_ceil(4);
        let budget_label = if est_tokens > effective_budget {
            "OVER BUDGET"
        } else {
            "on budget"
        };
        // llm_mode joins documents with "\n\n---\n\n"; count those to get nodes.
        let node_count = if assembled.is_empty() {
            0
        } else {
            assembled.matches("\n\n---\n\n").count() + 1
        };

        println!("{}", assembled);
        println!();
        if explain {
            print_explain(&xp);
        }
        println!("// ────────────────────────────────────────────────");
        println!("// Aden Ask Summary");
        println!("//   Question: {}", question);
        println!("//   Anchor  : [[{}]]", start_anchor);
        if !resolved_alts.is_empty() {
            println!(
                "//   Alts    : {} (ambiguous, shallow)",
                resolved_alts.join(", ")
            );
        }
        println!("//   Strategy: {:?} | Depth: {}", intent, depth);
        println!(
            "//   Nodes   : {} | ~{} tokens ({} bytes) / {} budget ({})",
            node_count, est_tokens, bytes, budget_note, budget_label
        );
        println!("{}", savings_est.footer_line());
        println!("// ────────────────────────────────────────────────");
    }

    Ok(())
}

fn query_llm(
    model_spec: &str,
    question: &str,
    context: &str,
    anchor: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let system_prompt = format!(
        r#"You are an expert software engineering assistant analyzing a codebase.
The user asked: "{}"
I have retrieved the relevant context starting from anchor [[{}]].
Please answer the question based ONLY on the provided context. If the context does not contain enough information, say so explicitly.

Context begins below (--- separates different documents):
"#,
        question, anchor
    );

    let full_prompt = format!("{}\n{}\n", system_prompt, context);

    let (provider, model_name) = if let Some(pos) = model_spec.find(':') {
        (&model_spec[..pos], &model_spec[pos + 1..])
    } else {
        // Auto-detect: try ollama first
        if std::process::Command::new("ollama")
            .arg("list")
            .output()
            .is_ok()
        {
            ("ollama", model_spec)
        } else {
            return Err(
                "No LLM provider prefix given (e.g., ollama:llama3) and ollama is not available"
                    .into(),
            );
        }
    };

    match provider {
        "ollama" => {
            println!("Asking ollama ({}) via stdin...", model_name);
            let mut child = std::process::Command::new("ollama")
                .args(["run", model_name])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()?;

            if let Some(stdin) = child.stdin.take() {
                use std::io::Write;
                let mut stdin = stdin;
                stdin.write_all(full_prompt.as_bytes())?;
                // drop stdin to signal EOF
            }

            let output = child.wait_with_output()?;
            if output.status.success() {
                let response = String::from_utf8_lossy(&output.stdout);
                println!("\n=== LLM Response ===\n{}", response);
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("ollama run failed: {}", stderr).into());
            }
        }
        "openai" => {
            let api_key = std::env::var("OPENAI_API_KEY")
                .map_err(|_| "OPENAI_API_KEY not set. Export it to use --model openai:<name>")?;
            println!("QueryingOpenAI ({})...", model_name);

            let payload = serde_json::json!({
                "model": model_name,
                "messages": [
                    { "role": "system", "content": &system_prompt },
                    { "role": "user", "content": context }
                ],
                "temperature": 0.3,
                "max_tokens": 2048
            });

            let output = std::process::Command::new("curl")
                .args([
                    "-sS",
                    "https://api.openai.com/v1/chat/completions",
                    "-H",
                    &format!("Authorization: Bearer {}", api_key),
                    "-H",
                    "Content-Type: application/json",
                    "-d",
                    &payload.to_string(),
                ])
                .output()?;

            if output.status.success() {
                let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
                if let Some(content) = json["choices"][0]["message"]["content"].as_str() {
                    println!("\n=== LLM Response ===\n{}", content);
                } else {
                    println!(
                        "Unexpected OpenAI response: {}",
                        String::from_utf8_lossy(&output.stdout)
                    );
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("OpenAI API call failed: {}", stderr).into());
            }
        }
        other => {
            return Err(format!(
                "Unknown LLM provider '{}'. Supported: ollama:<model>, openai:<model>",
                other
            )
            .into());
        }
    }

    Ok(())
}

pub fn cmd_query_adq(path: &Path, script: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("query-adq requires a directory path".into());
    }

    let graph = aden_graph::cache::build_from_directory_cached(path)?;
    let result = aden_graph::query::execute_adq(&graph, script)?;

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

/// True if `anchor` belongs to the requested `--doc-type` (already lower-cased
/// and validated by the caller). Matches the real anchor shapes: code symbols
/// use the `aden://module/…` scheme; docs encode their type in the filename
/// segment; legacy metadata anchors use short `kind-` prefixes.
fn anchor_matches_doc_type(anchor: &str, dtl: &str) -> bool {
    let a = anchor.to_lowercase();
    match dtl {
        "module" | "mod" => a.starts_with("aden://module/") || a.starts_with("mod-"),
        "adr" => a.starts_with("adr-") || a.contains("/adr-") || a.contains("/adr."),
        "plan" => a.starts_with("plan-") || a.contains("/plan-") || a.contains("/plan."),
        "use-case" | "usecase" => {
            a.starts_with("use-case-")
                || a.contains("/use-case")
                || a.contains("/use_case")
                || a.contains("/usecase")
        }
        "agent" => a.starts_with("agent-") || a.contains("/agent.") || a.contains("/agents."),
        _ => false,
    }
}

pub fn cmd_search(
    path: &Path,
    query: &str,
    limit: usize,
    offset: usize,
    doc_type: Option<&str>,
    include_semantics: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("search requires a directory path".into());
    }
    super::ensure_fresh(path);

    // Load config to check for private patterns (ADRs, retros, etc.)
    let config = AdenConfig::load(path);

    let index = load_or_build_index(path)?;
    let mut results = query_index(&index, query);

    // Filter out private anchors (ADRs, retros, kickoffs, etc.) in public mode
    let is_public = matches!(config.profile.mode, aden_core::ProfileMode::Public);
    if is_public {
        results.retain(|r| !config.is_private_anchor(&r.anchor));
    }

    // Filter by document type if specified. The doc-type lives in the anchor
    // URI scheme (code symbols are `aden://module/…`) or the document's filename
    // segment for docs (`…/adr-001.adoc`, `…/plan-phase2.adoc`, `…/use-cases.adoc`,
    // `…/agent.md`), plus legacy short-form anchors (`mod-`, `adr-`, …). A bare
    // `starts_with("mod-")` matched only the 25 legacy anchors and dropped all
    // 1000+ real `aden://module/…` symbols, so the most common filter returned
    // zero. Match against the real anchor shapes instead.
    if let Some(dt) = doc_type {
        let dtl = dt.to_lowercase();
        if !matches!(
            dtl.as_str(),
            "module" | "mod" | "adr" | "plan" | "use-case" | "usecase" | "agent"
        ) {
            eprintln!(
                "Warning: Unknown doc type '{}'. Valid: module, adr, plan, use-case, agent",
                dt
            );
            return Err(format!(
                "Invalid --type '{}'. Use: module, adr, plan, use-case, agent",
                dt
            )
            .into());
        }
        results.retain(|r| anchor_matches_doc_type(&r.anchor, &dtl));
    }

    // If --semantics, also search the graph for semantic relationships
    let mut semantic_results: Vec<(String, String)> = Vec::new();
    if include_semantics && let Ok(graph) = aden_graph::cache::build_from_directory_cached(path) {
        let query_lower = query.to_lowercase();
        for edge_idx in graph.graph.edge_indices() {
            let (src, tgt) = graph.graph.edge_endpoints(edge_idx).expect("valid edge");
            let edge_type = &graph.graph[edge_idx];
            let semantic_types = [
                aden_core::EdgeType::IsA,
                aden_core::EdgeType::PartOf,
                aden_core::EdgeType::RelatesTo,
                aden_core::EdgeType::SimilarTo,
                aden_core::EdgeType::Causes,
                aden_core::EdgeType::Implies,
                aden_core::EdgeType::SynonymOf,
                aden_core::EdgeType::AntonymOf,
                aden_core::EdgeType::AssociatedWith,
                aden_core::EdgeType::PrerequisiteFor,
                aden_core::EdgeType::Explains,
                aden_core::EdgeType::IsEquivalentTo,
            ];
            if semantic_types.contains(&edge_type.edge_type) {
                let src_anchor = graph.graph[src].doc.anchor.to_lowercase();
                let tgt_anchor = graph.graph[tgt].doc.anchor.to_lowercase();
                if src_anchor.contains(&query_lower) || tgt_anchor.contains(&query_lower) {
                    semantic_results.push((
                        graph.graph[tgt].doc.anchor.clone(),
                        format!("{:?} via {:?}", edge_type, graph.graph[src].doc.anchor),
                    ));
                }
            }
        }
    }

    // Machine-readable envelope for agents: explicit counts + pagination so the
    // caller never has to parse the human table or guess whether more exists.
    if json {
        let total = results.len();
        let page: Vec<_> = results.iter().skip(offset).take(limit).collect();
        let env = serde_json::json!({
            "total": total,
            "returned": page.len(),
            "offset": offset,
            "truncated": offset + page.len() < total,
            "results": page.iter().map(|r| serde_json::json!({
                "anchor": r.anchor,
                "score": r.score,
                "snippet": r.snippet,
            })).collect::<Vec<_>>(),
            "semantic": semantic_results.iter().map(|(anchor, rel)| serde_json::json!({
                "anchor": anchor,
                "relationship": rel,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&env)?);
        return Ok(());
    }

    if results.is_empty() && semantic_results.is_empty() {
        println!("No results for '{}'", query);
        return Ok(());
    }

    let total = results.len();
    let limited: Vec<_> = results.into_iter().skip(offset).take(limit).collect();

    println!(
        "Showing {}/{} results (offset={})",
        limited.len(),
        total,
        offset
    );
    println!("| Anchor | Score | Snippet |");
    println!("|=== |");
    for r in &limited {
        let snippet = if r.snippet.len() > 80 {
            format!("{}...", &r.snippet[..80])
        } else {
            r.snippet.clone()
        };
        println!("| {} | {} | {} |", r.anchor, fmt_score(r.score), snippet);
    }

    // Print semantic results if any
    if !semantic_results.is_empty() {
        println!();
        println!("Semantic relationships (--semantics):");
        println!("| Anchor | Relationship |");
        println!("|=== |");
        for (anchor, rel) in &semantic_results {
            println!("| {} | {} |", anchor, rel);
        }
    }
    Ok(())
}

/// Standard `*`/`?` glob match. `*` matches any sequence; `?` matches one char.
/// Used by `cmd_list --filter` so callers can write `mod-aden-*` or `*asm*`.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (pl, tl) = (p.len(), t.len());
    let mut dp = vec![vec![false; tl + 1]; pl + 1];
    dp[0][0] = true;
    for i in 1..=pl {
        if p[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for i in 1..=pl {
        for j in 1..=tl {
            if p[i - 1] == '*' {
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
            } else if p[i - 1] == '?' || p[i - 1] == t[j - 1] {
                dp[i][j] = dp[i - 1][j - 1];
            }
        }
    }
    dp[pl][tl]
}

/// Returns true when `anchor` satisfies `pattern`.
/// Glob patterns (containing `*` or `?`) use full glob semantics;
/// plain strings fall back to substring match for backward compatibility.
fn anchor_matches_filter(anchor: &str, pattern: &str) -> bool {
    if pattern.contains('*') || pattern.contains('?') {
        glob_matches(pattern, anchor)
    } else {
        anchor.contains(pattern)
    }
}

pub fn cmd_list(
    path: &Path,
    filter: Option<&str>,
    verbose: bool,
    limit: usize,
    offset: usize,
    semantics_only: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("list requires a directory path".into());
    }
    super::ensure_fresh(path);

    let graph = aden_graph::cache::build_from_directory_cached(path)?;

    // If semantics_only, collect only nodes that are part of semantic relationships
    let anchors: Vec<String> = if semantics_only {
        let mut semantic_anchors: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for edge_idx in graph.graph.edge_indices() {
            let edge_type = &graph.graph[edge_idx];
            let semantic_types = [
                aden_core::EdgeType::IsA,
                aden_core::EdgeType::PartOf,
                aden_core::EdgeType::RelatesTo,
                aden_core::EdgeType::SimilarTo,
                aden_core::EdgeType::Causes,
                aden_core::EdgeType::Implies,
                aden_core::EdgeType::SynonymOf,
                aden_core::EdgeType::AntonymOf,
                aden_core::EdgeType::AssociatedWith,
                aden_core::EdgeType::PrerequisiteFor,
                aden_core::EdgeType::Explains,
                aden_core::EdgeType::IsEquivalentTo,
            ];
            if semantic_types.contains(&edge_type.edge_type) {
                let (src, tgt) = graph.graph.edge_endpoints(edge_idx).expect("valid edge");
                semantic_anchors.insert(graph.graph[src].doc.anchor.clone());
                semantic_anchors.insert(graph.graph[tgt].doc.anchor.clone());
            }
        }
        semantic_anchors.into_iter().collect()
    } else {
        graph
            .graph
            .node_indices()
            .filter_map(|idx| graph.graph.node_weight(idx).map(|n| n.doc.anchor.clone()))
            .collect()
    };

    let filtered: Vec<_> = match filter {
        Some(f) => anchors
            .iter()
            .filter(|a| anchor_matches_filter(a, f))
            .cloned()
            .collect(),
        None => anchors,
    };
    let total_count = filtered.len();
    let limited: Vec<_> = filtered.into_iter().skip(offset).take(limit).collect();

    // Machine-readable envelope for agents: counts + pagination, no table chrome.
    if json {
        let items: Vec<serde_json::Value> = limited
            .iter()
            .map(|anchor| {
                if verbose {
                    let (node_type, source) = graph
                        .anchor_to_index
                        .get(anchor)
                        .and_then(|idx| graph.graph.node_weight(*idx))
                        .map(|n| {
                            (
                                n.doc
                                    .attributes
                                    .get("node-type")
                                    .cloned()
                                    .unwrap_or_else(|| "unknown".to_string()),
                                n.source_path.to_string_lossy().to_string(),
                            )
                        })
                        .unwrap_or_else(|| ("unknown".to_string(), String::new()));
                    serde_json::json!({"anchor": anchor, "type": node_type, "source": source})
                } else {
                    serde_json::json!(anchor)
                }
            })
            .collect();
        let env = serde_json::json!({
            "total": total_count,
            "returned": limited.len(),
            "offset": offset,
            "truncated": offset + limited.len() < total_count,
            "anchors": items,
        });
        println!("{}", serde_json::to_string_pretty(&env)?);
        return Ok(());
    }

    let offset_info = if offset > 0 {
        format!(" (offset={})", offset)
    } else {
        String::new()
    };
    println!(
        "Anchors in {}{} (showing {}/total {})",
        path.display(),
        offset_info,
        limited.len(),
        total_count
    );
    println!();

    if verbose {
        println!("| Anchor | Type | Source File |");
        println!("|=== |");
        for anchor in &limited {
            if let Some(idx) = graph.anchor_to_index.get(anchor)
                && let Some(n) = graph.graph.node_weight(*idx)
            {
                let node_type = n
                    .doc
                    .attributes
                    .get("node-type")
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                let source = n.source_path.to_string_lossy().to_string();
                println!("| {} | {} | {} |", anchor, node_type, source);
            }
        }
    } else {
        println!("| Anchor |");
        println!("|=== |");
        for anchor in &limited {
            println!("| {} |", anchor);
        }
    }

    if limited.len() == limit && total_count > limit {
        println!(
            "\n... {} more (use --limit or --offset to see more)",
            total_count - limit
        );
    }

    Ok(())
}

/// Resolve a bare symbol name to a single full store anchor, mirroring the
/// suffix/`#name` matching that `locate` uses against the store's anchor keys.
///
/// Returns the unique best match: an exact `#symbol` suffix match is preferred,
/// otherwise the first (sorted) anchor whose lowercased form ends with the
/// symbol or contains `#symbol`. `None` when nothing matches — callers turn
/// that into a helpful "not found" message. Factored out so it is unit-testable
/// without a live store.
fn pick_symbol_anchor(symbol: &str, anchors: &[String]) -> Option<String> {
    let sym = symbol.to_lowercase();
    let mut matched: Vec<&String> = anchors
        .iter()
        .filter(|a| {
            let al = a.to_lowercase();
            al.ends_with(&format!("#{}", sym))
                || al.ends_with(&sym)
                || al.contains(&format!("#{}", sym))
        })
        .collect();
    matched.sort();
    // Prefer an exact `#symbol` suffix (a real symbol anchor) over a looser
    // tail match, so `parse` resolves to `…#parse` not `…#reparse`.
    matched
        .iter()
        .find(|a| a.to_lowercase().ends_with(&format!("#{}", sym)))
        .or_else(|| matched.first())
        .map(|a| (*a).clone())
}

/// Backlinks of `anchor` (incoming references) as JSON nodes, one entry per
/// distinct referencer in iteration order. petgraph is a multigraph, so
/// `neighbors_directed` yields a neighbor once per parallel edge (e.g. a module
/// that both Contains and Calls the symbol); without the dedup `understand`
/// listed the same backlink multiple times.
fn collect_unique_backlinks(
    graph: &aden_graph::AdenGraph<aden_graph::DocumentNode, aden_graph::AdenEdge>,
    anchor: &str,
) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    let Some(idx) = graph.get_index(anchor) else {
        return out;
    };
    let mut seen = HashSet::new();
    for neighbor in graph.graph.neighbors_directed(idx, Direction::Incoming) {
        if seen.insert(neighbor) {
            out.push(node_to_json(&graph.graph[neighbor], 1));
        }
    }
    out
}

/// `aden understand <symbol>` — one-shot symbol comprehension.
///
/// Bundles what previously took four separate invocations (`locate`,
/// `query --backlinks`, `query --impact`, `asm`) into a single coherent report:
///
/// 1. resolve the symbol to its store anchor + definition location,
/// 2. list backlinks (incoming references — who calls/references it),
/// 3. list downstream impact (outgoing reach over the shared impact edge set),
/// 4. assemble a context block from that anchor within `budget` tokens.
///
/// Reuses the shared `resolve_anchor_in_store` resolution and the same graph
/// traversal / assembly internals the individual commands use.
pub fn cmd_understand(
    symbol: &str,
    path: &Path,
    budget: usize,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_asm::traverse::{AssemblyOptions, assemble};
    use serde_json::json;

    if !path.is_dir() {
        return Err("understand requires a directory path".into());
    }
    super::ensure_fresh(path);

    // Step 1: resolve the symbol to a full store anchor. Try the shared exact
    // resolver first; fall back to suffix matching over the store's anchor keys
    // (same strategy `locate` uses) so a bare symbol name still resolves.
    let anchor = match aden_graph::cache::resolve_anchor_in_store(path, symbol) {
        Some(a) => a,
        None => {
            let (store_path, _) = aden_paths::resolve_read_store(path);
            let anchors = aden_store::Storage::open_existing(
                store_path.to_str().ok_or("invalid store path")?,
            )
            .ok()
            .and_then(|s| s.get_all_anchors().ok())
            .unwrap_or_default();
            let anchors: Vec<String> = anchors.into_iter().collect();
            match pick_symbol_anchor(symbol, &anchors) {
                Some(a) => a,
                None => {
                    let msg = format!(
                        "No symbol found matching '{}'. Run 'aden list .' to see available anchors.",
                        symbol
                    );
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&json!({
                                "symbol": symbol,
                                "anchor": null,
                                "error": msg,
                            }))?
                        );
                    } else {
                        println!("{}", msg);
                    }
                    return Ok(());
                }
            }
        }
    };

    // Load the full graph once; all three structural views read from it.
    let graph = aden_graph::cache::build_from_directory_cached(path)?;
    let idx = graph.get_index(&anchor).ok_or_else(|| {
        format!(
            "Anchor '{}' not found in graph. Run 'aden list .' to see available anchors.",
            anchor
        )
    })?;

    // Definition location from the node's attributes.
    let def = {
        let node = &graph.graph[idx];
        let attrs = &node.doc.attributes;
        json!({
            "anchor": anchor,
            "node_type": attrs.get("node-type").cloned()
                .unwrap_or_else(|| format!("{:?}", node.doc.node_type)),
            "file": attrs.get("source_file").cloned().unwrap_or_default(),
            "start_line": attrs.get("start_line").cloned().unwrap_or_default(),
            "end_line": attrs.get("end_line").cloned().unwrap_or_default(),
        })
    };

    // Step 2: backlinks — incoming references (mirrors `query --backlinks`).
    let backlinks = collect_unique_backlinks(&graph, &anchor);

    // Step 3: downstream impact — outgoing reach over impact edge types
    // (mirrors `query --impact`). Uses the one shared SET: this local copy had
    // silently drifted (it was missing Implements/Mutates, so understand's
    // impact view truncated at trait boundaries that `query --impact` crossed).
    let impact_types = crate::util::impact_edge_types();
    let mut impact: Vec<serde_json::Value> = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    visited.insert(idx);
    queue.push_back((idx, 0usize));
    while let Some((node, d)) = queue.pop_front() {
        for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
            let weight = graph
                .graph
                .find_edge(node, neighbor)
                .and_then(|e| graph.graph.edge_weight(e))
                .map(|e| &e.edge_type)
                .copied()
                .unwrap_or(aden_core::EdgeType::Uses);
            if !impact_types.contains(&weight) {
                continue;
            }
            if visited.insert(neighbor) {
                impact.push(node_to_json(&graph.graph[neighbor], d + 1));
                queue.push_back((neighbor, d + 1));
            }
        }
    }

    // Step 4: assemble a context block from the anchor within budget, via the
    // same neighborhood-stream + assemble path `asm` uses.
    let edge_types: Vec<aden_core::EdgeType> = Vec::new();
    let neigh = aden_graph::cache::build_neighborhood_cached(path, &anchor, 3, &edge_types)?;
    let asm_opts = AssemblyOptions {
        start_anchor: anchor.clone(),
        max_depth: 3,
        token_budget: budget,
        edge_types,
        block_filter: Vec::new(),
        include_tags: Vec::new(),
        exclude_tags: Vec::new(),
        attributes: Vec::new(),
        llm_mode: true,
        hydrate_root: None,
    };
    let context = assemble(&neigh, &asm_opts)?;

    if json {
        let env = json!({
            "symbol": symbol,
            "anchor": anchor,
            "definition": def,
            "backlinks": backlinks,
            "impact": impact,
            "context": context,
        });
        println!("{}", serde_json::to_string_pretty(&env)?);
        return Ok(());
    }

    // Human report.
    println!("# Understanding '{}'", symbol);
    println!();
    println!("## Definition");
    let file = def["file"].as_str().unwrap_or("");
    let line = def["start_line"].as_str().unwrap_or("");
    let nt = def["node_type"].as_str().unwrap_or("");
    if file.is_empty() {
        println!("  {} [{}]", nt, anchor);
    } else {
        println!("  {} {} ({}:{})", anchor, nt, file, line);
    }
    println!();

    println!("## Backlinks ({} reference(s))", backlinks.len());
    if backlinks.is_empty() {
        println!("  (none — unused, an entry point, or invoked via dynamic dispatch)");
    } else {
        for b in &backlinks {
            println!("  {}", b["anchor"].as_str().unwrap_or(""));
        }
    }
    println!();

    println!("## Downstream impact ({} node(s))", impact.len());
    if impact.is_empty() {
        println!("  (none)");
    } else {
        for i in &impact {
            println!("  [{}] {}", i["depth"], i["anchor"].as_str().unwrap_or(""));
        }
    }
    println!();

    println!("## Context (budget {} tokens)", budget);
    println!();
    println!("{}", context);
    Ok(())
}

fn print_locate_results(hits: &[serde_json::Value], format: &str, context: Option<usize>) {
    if format == "json" {
        println!(
            "{}",
            serde_json::to_string_pretty(&hits).unwrap_or_default()
        );
        return;
    }
    let ctx = context.unwrap_or(0);
    for h in hits {
        let file = h["file"].as_str().unwrap_or("");
        let start = h["start_line"].as_str().unwrap_or("");
        let end = h["end_line"].as_str().unwrap_or("");
        let anchor = h["anchor"].as_str().unwrap_or("");
        let nt = h["node_type"].as_str().unwrap_or("");

        // Extract symbol name from anchor for brevity
        let symbol = anchor.split('#').next_back().unwrap_or(anchor);

        if file.is_empty() || start.is_empty() {
            println!("{} {} [{}]", symbol, nt, anchor);
        } else {
            println!("{} {} {}:{}", symbol, nt, file, start);
        }

        // Show context if requested
        if ctx > 0
            && !file.is_empty()
            && let Ok(lines) = std::fs::read_to_string(file)
        {
            let start_num: usize = start.parse().unwrap_or(1);
            let end_num: usize = end.parse().unwrap_or(start_num);
            let before = start_num.saturating_sub(ctx);
            let after = end_num + ctx;
            let all_lines: Vec<&str> = lines.lines().collect();
            if before < all_lines.len() && before < after {
                println!(
                    "  Context (lines {}-{}):",
                    before + 1,
                    after.min(all_lines.len())
                );
                for (i, line) in all_lines.iter().enumerate().take(after).skip(before) {
                    let line_num = i + 1;
                    let marker = if line_num >= start_num && line_num <= end_num {
                        ">"
                    } else {
                        " "
                    };
                    println!("{}{:4}: {}", marker, line_num, line);
                }
            }
        }
    }
    // Self-document the discovery→assembly loop: the symbol shown is exactly the
    // anchor `asm`/`understand` take, so the agent can pivot from a locate hit
    // straight to full context without a second lookup.
    if let Some(first) = hits.first() {
        let anchor = first["anchor"].as_str().unwrap_or("");
        let symbol = anchor.split('#').next_back().unwrap_or(anchor);
        if !symbol.is_empty() {
            println!(
                "  ↳ expand into full context: `asm --from {symbol}` (or `understand {symbol}`)"
            );
        }
    }
}

pub fn cmd_locate(
    path: &Path,
    symbol: Option<&str>,
    caller_of: Option<&str>,
    format: &str,
    limit: usize,
    context: Option<usize>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use serde_json::json;

    if !path.is_dir() {
        // The positional argument is the project DIR (default "."); the symbol
        // goes in `--symbol`. A user who typed `aden locate myFn` lands here with
        // a non-directory positional and no flag — point them at the right form
        // instead of the bare "requires a directory path".
        if symbol.is_none() && caller_of.is_none() {
            return Err(format!(
                "'{}' is not a directory. To find a symbol use:\n  \
                 aden locate --symbol {} [DIR]\n  \
                 aden locate --caller-of {} [DIR]",
                path.display(),
                path.display(),
                path.display(),
            )
            .into());
        }
        return Err(format!("locate: '{}' is not a directory", path.display()).into());
    }
    super::ensure_fresh(path);

    // JSON is requested via either the global `-j/--json` flag or `--format json`.
    // In JSON mode every human header ("Found N match(es)…") is suppressed so the
    // stream is a single machine-parseable value, never JSON prefixed by prose.
    let want_json = json || format == "json";

    // If --symbol is given, find the definition.
    if let Some(sym) = symbol {
        // Match against anchor *keys* in the store and deserialize only the
        // documents that match. Building the full petgraph here is what made
        // `locate` take ~47s on the kernel (1.2M nodes); this is bounded by the
        // number of matches.
        let (store_path, _) = aden_paths::resolve_read_store(path);
        let storage =
            aden_store::Storage::open_existing(store_path.to_str().ok_or("invalid store path")?)
                .map_err(|e| format!("failed to open store: {}", e))?;
        let all_anchors = storage.get_all_anchors().unwrap_or_default();

        let sym_lower = sym.to_lowercase();
        let mut matched: Vec<&String> = all_anchors
            .iter()
            .filter(|a| {
                let al = a.to_lowercase();
                al.ends_with(&sym_lower)
                    || al.contains(&format!("#{}", sym_lower))
                    || al.contains(&sym_lower)
            })
            .collect();
        // Precision: surface the exact symbol definition (and its members) before
        // incidental substring hits (doc headings, `OtherGroup`, code blocks). The
        // trailing path/anchor segment is compared against the query name:
        //   rank 0 — segment == name, same case  (the definition the user typed)
        //   rank 1 — segment == name, any case   (e.g. `group` fn vs `Group` type)
        //   rank 2 — segment starts `name.`/`::`  (its methods/members)
        //   rank 3 — any other substring match    (incidental)
        let locate_rank = |a: &str| -> u8 {
            let seg = a.rsplit(['#', '/']).next().unwrap_or("");
            let seg_lower = seg.to_lowercase();
            if seg == sym {
                0
            } else if seg_lower == sym_lower {
                1
            } else if seg_lower.starts_with(&format!("{}.", sym_lower))
                || seg_lower.starts_with(&format!("{}::", sym_lower))
            {
                2
            } else {
                3
            }
        };
        matched.sort_by(|a, b| locate_rank(a).cmp(&locate_rank(b)).then_with(|| a.cmp(b)));

        let hits: Vec<serde_json::Value> = matched
            .iter()
            .take(limit)
            .filter_map(|a| {
                let doc = storage.get_document(a).ok().flatten()?;
                let attrs = &doc.attributes;
                Some(json!({
                    "anchor": a,
                    "node_type": attrs.get("node-type").cloned()
                        .unwrap_or_else(|| format!("{:?}", doc.node_type)),
                    "file": attrs.get("source_file").cloned().unwrap_or_default(),
                    "start_line": attrs.get("start_line").cloned().unwrap_or_default(),
                    "end_line": attrs.get("end_line").cloned().unwrap_or_default(),
                }))
            })
            .collect();

        if hits.is_empty() {
            // Fall back to the full-text search index.
            let index = load_or_build_index(path)?;
            let search_results = query_index(&index, sym);
            if want_json {
                // Machine-readable: emit the (possibly empty) full-text hits as a
                // JSON array, never the human "Found … / No symbol found" prose.
                let arr: Vec<serde_json::Value> = search_results
                    .iter()
                    .take(limit)
                    .map(|r| json!({ "anchor": r.anchor, "score": r.score, "snippet": r.snippet }))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&arr)?);
                return Ok(());
            }
            if !search_results.is_empty() {
                println!(
                    "Found {} match(es) in full-text index for '{}':",
                    search_results.len(),
                    sym
                );
                println!("| Anchor | Score | Snippet |");
                println!("|=== |");
                for r in search_results.iter().take(limit) {
                    let snippet = if r.snippet.len() > 60 {
                        format!("{}...", &r.snippet[..60])
                    } else {
                        r.snippet.clone()
                    };
                    println!("| {} | {} | {} |", r.anchor, fmt_score(r.score), snippet);
                }
                return Ok(());
            }
            println!("No symbol found matching '{}'", sym);
            println!(
                "Hint: Try 'aden search \"{}\"' to find related anchors",
                sym
            );
            return Ok(());
        }

        if want_json {
            println!("{}", serde_json::to_string_pretty(&hits)?);
        } else {
            println!("Found {} match(es) for '{}':", matched.len(), sym);
            print_locate_results(&hits, format, context);
        }
        return Ok(());
    }

    // If --caller-of is given, list callers via incoming `Calls` edges in the
    // knowledge graph. This is the reverse of `query --backlinks`, filtered to
    // call edges, with each caller enriched by its source file + line from the
    // store. The call graph is already populated by `gen` (link_store_edges),
    // so no new metadata is required — earlier this branch was a stub.
    if let Some(target) = caller_of {
        use serde_json::json;

        let graph = aden_graph::cache::build_from_directory_cached(path)?;

        // A bare symbol (e.g. `assemble`) may resolve to several anchors across
        // modules; union the callers of every matching definition.
        let tl = target.to_lowercase();
        let targets: Vec<_> = graph
            .graph
            .node_indices()
            .filter(|&i| {
                let al = graph.graph[i].doc.anchor.to_lowercase();
                al.ends_with(&tl) || al.contains(&format!("#{}", tl))
            })
            .collect();

        if targets.is_empty() {
            if want_json {
                println!("[]");
                return Ok(());
            }
            println!("No symbol found matching '{}'", target);
            println!(
                "Hint: Try 'aden locate . --symbol {}' to confirm it is indexed.",
                target
            );
            return Ok(());
        }

        // The matched definitions themselves are never their own callers. A bare
        // name matches loosely (`#fold_overlay` also matches `#fold_overlay_blocks`),
        // so a target that legitimately calls a sibling target would otherwise be
        // reported as a self-caller on its own definition line — exclude them.
        let target_anchors: HashSet<String> = targets
            .iter()
            .map(|&i| graph.graph[i].doc.anchor.clone())
            .collect();

        // Collect unique callers via incoming `Calls` edges.
        let mut seen = HashSet::new();
        let mut callers: Vec<String> = Vec::new();
        for &t in &targets {
            for neighbor in graph.graph.neighbors_directed(t, Direction::Incoming) {
                let is_call = graph
                    .graph
                    .find_edge(neighbor, t)
                    .and_then(|e| graph.graph.edge_weight(e))
                    .map(|e| e.edge_type == aden_core::EdgeType::Calls)
                    .unwrap_or(false);
                if is_call {
                    let a = graph.graph[neighbor].doc.anchor.clone();
                    if !target_anchors.contains(&a) && seen.insert(a.clone()) {
                        callers.push(a);
                    }
                }
            }
        }
        callers.sort();

        if callers.is_empty() {
            if want_json {
                println!("[]");
                return Ok(());
            }
            println!(
                "No callers found for '{}' (unused, an entry point, or invoked via dynamic dispatch).",
                target
            );
            return Ok(());
        }

        // Enrich each caller with file:line from the store (best-effort).
        let (store_path, _) = aden_paths::resolve_read_store(path);
        let storage =
            aden_store::Storage::open_existing(store_path.to_str().ok_or("invalid store path")?)
                .ok();
        let hits: Vec<serde_json::Value> = callers
            .iter()
            .take(limit)
            .map(|a| {
                let (file, line) = storage
                    .as_ref()
                    .and_then(|s| s.get_document(a).ok().flatten())
                    .map(|doc| {
                        (
                            doc.attributes
                                .get("source_file")
                                .cloned()
                                .unwrap_or_default(),
                            doc.attributes
                                .get("start_line")
                                .cloned()
                                .unwrap_or_default(),
                        )
                    })
                    .unwrap_or_default();
                json!({ "anchor": a, "file": file, "start_line": line })
            })
            .collect();

        if want_json {
            println!("{}", serde_json::to_string_pretty(&hits)?);
        } else {
            println!("Found {} caller(s) of '{}':", hits.len(), target);
            for h in &hits {
                let file = h["file"].as_str().unwrap_or("");
                let line = h["start_line"].as_str().unwrap_or("");
                let loc = if file.is_empty() {
                    String::new()
                } else {
                    format!("  ({}:{})", file, line)
                };
                println!("  {}{}", h["anchor"].as_str().unwrap_or(""), loc);
            }
        }
        return Ok(());
    }

    Err("locate requires one of --symbol or --caller-of".into())
}

#[cfg(feature = "watch")]
pub fn cmd_watch(
    path: &Path,
    graph_sync: bool,
    restore: bool,
    sync_all: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    if !path.is_dir() {
        return Err("watch requires a directory path".into());
    }

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // Setup ctrl-c handler
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl-C handler");

    // Optional: Restore graph from cache for faster startup
    let mut graph: Option<aden_graph::AdenGraph<aden_graph::DocumentNode, aden_graph::AdenEdge>> =
        None;
    if graph_sync && restore {
        println!("Restoring graph from cache...");
        match aden_graph::cache::build_from_directory_cached(path) {
            Ok(g) => {
                let anchor_count = g.graph.node_indices().count();
                println!("Restored graph ({} anchors)", anchor_count);
                graph = Some(g);
            }
            Err(e) => {
                println!("Note: Could not restore graph (will build fresh): {}", e);
            }
        }
    }

    let (tx, rx) = channel();
    let mut watcher = RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = tx.send(event);
            }
        },
        Config::default(),
    )?;

    watcher.watch(path, RecursiveMode::Recursive)?;
    println!(
        "Watching {} for changes... Press Ctrl+C to stop.",
        path.display()
    );

    // Supported source extensions that parse_file can handle
    let source_exts = [
        "rs",
        "py",
        "js",
        "ts",
        "tsx",
        "jsx",
        "mjs",
        "cjs",
        "go",
        "java",
        "c",
        "cpp",
        "cc",
        "cxx",
        "h",
        "hpp",
        "rb",
        "cs",
        "swift",
        "kt",
        "scala",
        "zig",
        "lua",
        "hs",
        "ml",
        "php",
        "ex",
        "exs",
        "erl",
        "gleam",
        "sh",
        "bash",
        "dockerfile",
        "html",
        "css",
        "scss",
        "vue",
        "svelte",
        "proto",
        "tf",
        "cmake",
    ];

    // Contracts directory
    let contracts_dir = path.join("contracts");
    std::fs::create_dir_all(&contracts_dir)?;

    // Debounce state
    let debounce_duration = Duration::from_millis(100);
    let mut pending_paths: HashSet<std::path::PathBuf> = HashSet::new();
    let mut last_process_time = Instant::now();

    // Graph sync state
    let _graph_arc = if graph_sync {
        graph.map(|g| std::sync::Arc::new(std::sync::Mutex::new(g)))
    } else {
        None
    };

    if graph_sync {
        println!("Graph sync enabled - contracts and graph stay current.");
    }

    // Main event loop
    while running.load(Ordering::SeqCst) {
        // Process events with debouncing
        for event in rx.try_iter() {
            for p in &event.paths {
                if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                    let ext = ext.to_lowercase();
                    if source_exts.contains(&ext.as_str()) || ext == "adoc" || ext == "aden" {
                        pending_paths.insert(p.clone());
                    }
                }
            }
        }

        // Only process if debounce window passed
        if !pending_paths.is_empty() && last_process_time.elapsed() >= debounce_duration {
            let paths_to_process: Vec<_> = pending_paths.drain().collect();
            last_process_time = Instant::now();
            let mut contracts_regenerated = 0usize;

            // Process each changed file
            for p in &paths_to_process {
                if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                    let ext = ext.to_lowercase();

                    if source_exts.contains(&ext.as_str()) {
                        // Source file change - regenerate contract
                        println!(
                            "INFO: Source change: {}",
                            p.file_name().unwrap_or_default().to_string_lossy()
                        );
                        if let Ok(source) = std::fs::read_to_string(p) {
                            match aden_parse::parse_file(p, &source) {
                                Ok(mut docs) if !docs.is_empty() => {
                                    let watch_root = find_project_root(path);
                                    for doc in &mut docs {
                                        sanitize_source_file(doc, &watch_root);
                                        let safe_anchor = sanitize_anchor(&doc.anchor);
                                        let out_path =
                                            contracts_dir.join(format!("{}.adoc", safe_anchor));
                                        if std::fs::write(&out_path, aden_emit::emit_document(doc))
                                            .is_ok()
                                        {
                                            contracts_regenerated += 1;
                                        }
                                    }
                                }
                                Ok(_) => {}
                                Err(aden_core::Error::UnsupportedLanguage(_)) => {}
                                Err(e) => eprintln!("ERROR: Parse failed: {}", e),
                            }
                        }
                    } else if ext == "adoc" || ext == "aden" {
                        // Doc file change - validate
                        if let Err(e) = perform_check(path) {
                            eprintln!("ERROR: Check failed: {}", e);
                        }
                    }
                }
            }

            // Summary
            if contracts_regenerated > 0 {
                println!("INFO: Regenerated {} contract(s)", contracts_regenerated);
            }

            // Graph sync: keep the store-first knowledge graph current. The
            // per-file .adoc emit above does not touch .aden/store, so without
            // this `query`/`asm`/`locate` would serve a stale graph. Re-running
            // the gen path indexes changed files, prunes deleted symbols, and
            // re-links edges — the same logic `aden gen` uses — so the graph
            // stays consistent (not just the contract files).
            if graph_sync && !paths_to_process.is_empty() {
                match crate::commands::generate::cmd_gen(path, true) {
                    Ok(()) => println!("INFO: Graph synced ({})", path.display()),
                    Err(e) => eprintln!("ERROR: Graph sync failed: {}", e),
                }
            }

            // Unified sync mode: run gen + check + heal
            if sync_all && !paths_to_process.is_empty() {
                println!("INFO: Running unified sync...");

                // Run check
                if let Err(e) = crate::util::perform_check(path) {
                    let has_errors = format!("{:?}", e).contains("ERROR:");
                    if has_errors {
                        eprintln!("CHECK: {}", e);
                    }
                } else {
                    println!("CHECK: All references valid");
                }

                // Run heal scan (summary only)
                #[cfg(feature = "watch")]
                {
                    use aden_heal::{Scanner, generate};
                    let scanner = Scanner::new(path);
                    if let Ok(events) = scanner.scan() {
                        let report = generate(events.clone(), path);
                        println!("HEAL: Health score = {:.2}", report.overall_score);
                        if !events.is_empty() {
                            println!("HEAL: {} drift event(s) detected", events.len());
                        }
                    }
                }
            }
        }

        // Small sleep to prevent CPU spinning
        std::thread::sleep(Duration::from_millis(10));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::QueryIntent;

    // ---- understand: backlink listing dedups parallel-edge referencers.

    fn backlink_fixture_node(anchor: &str) -> aden_graph::DocumentNode {
        aden_graph::DocumentNode {
            doc: aden_core::Document {
                anchor: anchor.to_string(),
                node_type: aden_core::NodeType::Function,
                attributes: std::collections::HashMap::new(),
                blocks: Vec::new(),
                source_span: None,
                metadata: None,
                confidence: 0.9,
            },
            parsed: None,
            source_path: PathBuf::from(format!("{anchor}.adoc")),
        }
    }

    /// Regression: `understand` printed the same backlink once per parallel
    /// edge (observed: `mod-aden-mcp` listed twice). A referencer connected by
    /// several edge types (Contains + Calls + Uses) must appear exactly once,
    /// and distinct referencers must all survive the dedup.
    #[test]
    fn understand_backlinks_dedup_parallel_edges() {
        let mut g = aden_graph::AdenGraph::<aden_graph::DocumentNode, aden_graph::AdenEdge>::new();
        let target = g.add_node(backlink_fixture_node("target"));
        let module = g.add_node(backlink_fixture_node("mod-caller"));
        let other = g.add_node(backlink_fixture_node("other-caller"));
        // Use raw petgraph add_edge: AdenGraph::add_edge skips duplicates, but
        // real builds create parallel edges of different types directly.
        for et in [
            aden_core::EdgeType::Contains,
            aden_core::EdgeType::Calls,
            aden_core::EdgeType::Uses,
        ] {
            g.graph
                .add_edge(module, target, aden_graph::AdenEdge { edge_type: et });
        }
        g.graph.add_edge(
            other,
            target,
            aden_graph::AdenEdge {
                edge_type: aden_core::EdgeType::Calls,
            },
        );

        let backlinks = collect_unique_backlinks(&g, "target");
        let anchors: Vec<&str> = backlinks
            .iter()
            .map(|b| b["anchor"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(
            anchors.len(),
            2,
            "each referencer must appear exactly once, got {anchors:?}"
        );
        assert!(anchors.contains(&"mod-caller"), "got {anchors:?}");
        assert!(anchors.contains(&"other-caller"), "got {anchors:?}");
    }

    /// An anchor missing from the graph yields no backlinks (and no panic).
    #[test]
    fn understand_backlinks_unknown_anchor_is_empty() {
        let g = aden_graph::AdenGraph::<aden_graph::DocumentNode, aden_graph::AdenEdge>::new();
        assert!(collect_unique_backlinks(&g, "nope").is_empty());
    }

    // ---- classify_intent: previously-misrouting phrasings now route correctly.

    /// "what breaks if I change X" used to fall through to General (the old
    /// first-match chain had no "what breaks"/"if i change" signal). It must
    /// now route to an actionable intent (Debug via "break"/"breaks", or Impact
    /// via the "what breaks"/"if i change" phrases) — never General.
    #[test]
    fn classify_what_breaks_if_i_change_not_general() {
        let intent = classify_intent("what breaks if I change parse_file");
        assert!(
            matches!(intent, QueryIntent::Debug | QueryIntent::Impact),
            "expected Debug or Impact, got {:?}",
            intent
        );
        assert!(
            !matches!(intent, QueryIntent::General),
            "must not route to General"
        );
    }

    /// "what is affected by modifying X": the generic Explain "what is" phrase
    /// used to shadow this. Score-and-max gives Impact (affected + what is
    /// affected = 2) the win over Explain ("what is" = 1).
    #[test]
    fn classify_what_is_affected_routes_impact_not_explain() {
        let intent = classify_intent("what is affected by modifying the scanner");
        assert!(
            matches!(intent, QueryIntent::Impact),
            "expected Impact, got {:?}",
            intent
        );
        assert!(
            !matches!(intent, QueryIntent::Explain),
            "must not route to Explain"
        );
    }

    #[test]
    fn classify_what_is_the_impact_routes_impact() {
        let intent = classify_intent("what is the impact of changing the store");
        assert!(
            matches!(intent, QueryIntent::Impact),
            "expected Impact, got {:?}",
            intent
        );
    }

    #[test]
    fn classify_blast_radius_routes_impact() {
        let intent = classify_intent("blast radius of resolve_anchor");
        assert!(
            matches!(intent, QueryIntent::Impact),
            "expected Impact, got {:?}",
            intent
        );
    }

    #[test]
    fn classify_how_do_i_use_routes_usage() {
        let intent = classify_intent("how do I use the assembler");
        assert!(
            matches!(intent, QueryIntent::Usage),
            "expected Usage, got {:?}",
            intent
        );
    }

    #[test]
    fn classify_how_does_work_routes_explain() {
        let intent = classify_intent("how does the cache work");
        assert!(
            matches!(intent, QueryIntent::Explain),
            "expected Explain, got {:?}",
            intent
        );
    }

    /// "how to use X" (the imperative phrasing, distinct from "how do I use X")
    /// also routes to Usage via the "how to" phrase.
    #[test]
    fn classify_how_to_use_routes_usage() {
        let intent = classify_intent("how to use the assembler");
        assert!(
            matches!(intent, QueryIntent::Usage),
            "expected Usage, got {:?}",
            intent
        );
    }

    #[test]
    fn classify_how_many_routes_count() {
        let intent = classify_intent("how many edge types are there");
        assert!(
            matches!(intent, QueryIntent::Count),
            "expected Count, got {:?}",
            intent
        );
    }

    /// "how many callers does X have" ties Count ("how many") with Impact
    /// ("caller"); Count must win now that it precedes Impact in PRIORITY,
    /// so a counting question is a depth-1 tally, not a blast-radius traversal.
    #[test]
    fn classify_how_many_callers_routes_count() {
        let intent = classify_intent("how many callers does parse_file have");
        assert!(
            matches!(intent, QueryIntent::Count),
            "expected Count, got {:?}",
            intent
        );
    }

    /// "how many functions depend on X" likewise resolves to Count, not Impact.
    #[test]
    fn classify_how_many_depend_routes_count() {
        let intent = classify_intent("how many functions depend on the store");
        assert!(
            matches!(intent, QueryIntent::Count),
            "expected Count, got {:?}",
            intent
        );
    }

    /// "how do I debug X" ties Debug ("debug") with Usage ("how do i"); Debug
    /// wins because it leads PRIORITY.
    #[test]
    fn classify_how_do_i_debug_routes_debug() {
        let intent = classify_intent("how do I debug the assembler");
        assert!(
            matches!(intent, QueryIntent::Debug),
            "expected Debug, got {:?}",
            intent
        );
    }

    #[test]
    fn classify_refactor_routes_refactor() {
        let intent = classify_intent("refactor the traverse module");
        assert!(
            matches!(intent, QueryIntent::Refactor),
            "expected Refactor, got {:?}",
            intent
        );
    }

    /// Empty/no-signal questions fall back to General.
    #[test]
    fn classify_no_signal_routes_general() {
        let intent = classify_intent("the quick brown fox");
        assert!(
            matches!(intent, QueryIntent::General),
            "expected General, got {:?}",
            intent
        );
    }

    // ---- auto_boosted_budget: float boost math.

    /// Boost is monotonically non-decreasing in avg_score.
    #[test]
    fn auto_boost_monotonic_in_avg_score() {
        let base = 1_000usize;
        let mut prev = 0usize;
        let mut score = 0.0f64;
        while score <= 2.0 {
            let b = auto_boosted_budget(base, score);
            assert!(
                b >= prev,
                "budget decreased at score {}: {} < {}",
                score,
                b,
                prev
            );
            prev = b;
            score += 0.05;
        }
    }

    /// Very high relevance is capped at AUTO_BUDGET_CAP, and the ×4 ceiling from
    /// AUTO_BOOST_MAX holds for a base small enough not to hit the cap first.
    #[test]
    fn auto_boost_capped() {
        // Huge avg_score: boost clamps to AUTO_BOOST_MAX (3.0) → ×4 of base.
        let base = 1_000usize;
        assert_eq!(auto_boosted_budget(base, 1_000.0), 4_000);
        // A base large enough that ×4 would exceed the hard cap is clamped.
        assert_eq!(auto_boosted_budget(100_000, 1_000.0), AUTO_BUDGET_CAP);
        // zero relevance leaves the budget untouched.
        assert_eq!(auto_boosted_budget(base, 0.0), base);
    }

    /// A mid relevance produces a non-integer-multiple boost, proving the old
    /// integer truncation is gone. avg_score 0.3 → boost 0.6 → 1.6× base.
    #[test]
    fn auto_boost_mid_relevance_is_fractional() {
        // 1000 * (1 + 0.3*2.0) = 1000 * 1.6 = 1600 — not a clean integer
        // multiple of base, which the old `(score as usize)`-style truncation
        // could never produce (it would have snapped to 1000 or 2000).
        assert_eq!(auto_boosted_budget(1_000, 0.3), 1_600);
        assert_ne!(auto_boosted_budget(1_000, 0.3), 1_000);
        assert_ne!(auto_boosted_budget(1_000, 0.3), 2_000);
        // 1234 * (1 + 0.25*2.0) = 1234 * 1.5 = 1851 (rounded from 1851.0).
        assert_eq!(auto_boosted_budget(1_234, 0.25), 1_851);
    }

    // ---- ask budget selection: boost by default, --strict opts out.
    //
    // The full `cmd_ask` path requires a built index / store (I/O), so we cannot
    // drive it from a unit test without a fixture. Instead we guard the pure
    // budget-selection expression `match (strict, avg_score)` that `cmd_ask`
    // uses (query.rs ~line 785): with search relevance present and strict=false
    // the boost helper is applied; strict=true (or no relevance) returns the
    // base budget unchanged. The behavioral stage covers the end-to-end wiring.
    fn select_ask_budget(strict: bool, budget: usize, avg_score: Option<f64>) -> usize {
        match (strict, avg_score) {
            (false, Some(avg)) => auto_boosted_budget(budget, avg),
            _ => budget,
        }
    }

    #[test]
    fn ask_boosts_budget_by_default() {
        // Default (non-strict) ask with positive relevance scales the budget via
        // the shared boost helper — not the raw --budget.
        let base = 1_000usize;
        let avg = 0.3f64;
        assert_eq!(
            select_ask_budget(false, base, Some(avg)),
            auto_boosted_budget(base, avg)
        );
        assert!(
            select_ask_budget(false, base, Some(avg)) > base,
            "boost must exceed base for positive relevance"
        );
    }

    // ---- understand: symbol -> anchor resolution.

    /// An exact `#symbol` suffix wins over a looser tail match, so `parse`
    /// resolves to `…#parse` and never to `…#reparse`.
    #[test]
    fn understand_picks_exact_symbol_suffix() {
        let anchors = vec![
            "src/a.rs#reparse".to_string(),
            "src/b.rs#parse".to_string(),
            "src/c.rs#parser".to_string(),
        ];
        assert_eq!(
            super::pick_symbol_anchor("parse", &anchors),
            Some("src/b.rs#parse".to_string())
        );
    }

    /// Case-insensitive match, and an unknown symbol yields None so the caller
    /// can emit the "run aden list" hint.
    #[test]
    fn understand_resolution_is_case_insensitive_and_missing_is_none() {
        let anchors = vec!["crates/x.rs#AssembleContext".to_string()];
        assert_eq!(
            super::pick_symbol_anchor("assemblecontext", &anchors),
            Some("crates/x.rs#AssembleContext".to_string())
        );
        assert_eq!(super::pick_symbol_anchor("nope_not_here", &anchors), None);
    }

    #[test]
    fn ask_strict_uses_exact_budget() {
        // --strict bypasses the boost entirely: budget is the exact cap.
        let base = 1_000usize;
        assert_eq!(select_ask_budget(true, base, Some(0.3)), base);
        assert_eq!(select_ask_budget(true, base, Some(1_000.0)), base);
        // A pinned --from anchor has no search relevance (None) → base, even
        // when not strict.
        assert_eq!(select_ask_budget(false, base, None), base);
    }

    fn result(anchor: &str, score: f64) -> SearchResult {
        SearchResult {
            anchor: anchor.to_string(),
            source_path: std::path::PathBuf::new(),
            score,
            snippet: String::new(),
        }
    }

    #[test]
    fn test_anchor_detection_is_language_agnostic() {
        assert!(is_test_anchor("aden://module/p/test_context.py#callback"));
        assert!(is_test_anchor("aden://module/p/command_test.go#TestRun"));
        assert!(is_test_anchor("aden://module/p/test/main.ts#run"));
        assert!(is_test_anchor("aden://module/p/foo.test.ts#x"));
        assert!(is_test_anchor("aden://module/p/foo.spec.js#x"));
        assert!(is_test_anchor("aden://module/p/__tests__/foo.js#x"));
        // Production code must NOT be flagged.
        assert!(!is_test_anchor("aden://module/p/src/click/core.py#Command"));
        assert!(!is_test_anchor("aden://module/p/command.go#Execute"));
        assert!(!is_test_anchor("aden://module/p/latest.ts#x"));
    }

    #[test]
    fn ask_routing_skips_test_fixture_for_production_symbol() {
        // The Python-click repro: query word "callback" matches a test fixture
        // symbol, but the production symbol must win.
        let results = vec![
            result("aden://module/p/test_context.py#callback", 30.0),
            result("aden://module/p/core.py#Command", 29.0),
        ];
        let chosen =
            resolve_anchor_fuzzy("how does a command invoke the callback", &results, |_| 100);
        assert_eq!(chosen, "aden://module/p/core.py#Command");
    }

    fn result_with_path(anchor: &str, source: &str, score: f64) -> SearchResult {
        SearchResult {
            anchor: anchor.to_string(),
            source_path: std::path::PathBuf::from(source),
            score,
            snippet: String::new(),
        }
    }

    #[test]
    fn ask_routing_skips_dir_only_test_fixture_via_source_path() {
        // The module-form anchor flattens the directory, so `typing_route.py`
        // hides that it lives under tests/. With source_path-aware detection the
        // production symbol must still win even though the fixture scores higher.
        let results = vec![
            result_with_path(
                "aden://module/flask/typing_route.py#View.dispatch_request",
                "tests/type_check/typing_route.py",
                30.0,
            ),
            result_with_path(
                "aden://module/flask/app.py#Flask.dispatch_request",
                "src/flask/app.py",
                28.0,
            ),
        ];
        let chosen = resolve_anchor_fuzzy("how is dispatching handled", &results, |_| 100);
        assert_eq!(chosen, "aden://module/flask/app.py#Flask.dispatch_request");
    }

    /// Wave 1 (`Tests` edges): gen classifies a symbol's source file with the
    /// SAME markers ask-routing uses — relative paths gain a leading slash so
    /// the first segment can match `/tests/`-style markers.
    #[test]
    fn test_source_path_matches_relative_first_segment() {
        assert!(is_test_source_path("tests/greeter_test.rs"));
        assert!(is_test_source_path(
            "crates/aden-cli/tests/mcp_flag_parity.rs"
        ));
        assert!(is_test_source_path("src/__tests__/app.spec.ts"));
        // Rust's conventional split-out test module file.
        assert!(is_test_source_path("crates/aden-parse/src/tests.rs"));
        assert!(is_test_source_path("tests.rs"));
        assert!(!is_test_source_path("src/lib.rs"));
        // A production file that merely ENDS in "tests" must not be swept in.
        assert!(!is_test_source_path("src/mytests.rs"));
        assert!(!is_test_source_path(""));
    }

    #[test]
    fn ask_routing_prefers_substantive_symbol_over_thin_stub() {
        // Two production symbols in-band; the thin abstract method scores higher
        // but the substantive dispatcher (more indexed tokens) must win.
        let results = vec![
            result("aden://module/flask/views.py#View.dispatch_request", 30.0),
            result("aden://module/flask/app.py#Flask.dispatch_request", 28.0),
        ];
        let token_count = |a: &str| {
            if a.contains("app.py") { 200 } else { 10 }
        };
        let chosen = resolve_anchor_fuzzy("how are views dispatched", &results, token_count);
        assert_eq!(chosen, "aden://module/flask/app.py#Flask.dispatch_request");
    }

    #[test]
    fn ask_routing_relaxes_when_only_test_symbols_exist() {
        // If every candidate is a test symbol, routing must still return one
        // rather than nothing (query may genuinely be about the test suite).
        let results = vec![
            result("aden://module/p/test_a.py#helper", 30.0),
            result("aden://module/p/test_b.py#other", 29.0),
        ];
        let chosen = resolve_anchor_fuzzy("explain the helper test", &results, |_| 100);
        assert!(is_test_anchor(&chosen));
    }

    #[test]
    fn inband_alternates_empty_for_clear_winner() {
        // Runner-up is more than the noise band below the leader → no alternates.
        let results = vec![
            result("a", 30.0),
            result("b", 30.0 - ANCHOR_NOISE_BAND - 0.1),
            result("c", 5.0),
        ];
        assert!(inband_alternate_candidates("a", &results, 2).is_empty());
    }

    #[test]
    fn inband_alternates_collects_near_ties_excluding_primary() {
        // b and c are within the band; a (the primary) is excluded; d is out of band.
        let results = vec![
            result("a", 30.0),
            result("b", 28.0),
            result("c", 26.0),
            result("d", 10.0),
        ];
        assert_eq!(
            inband_alternate_candidates("a", &results, 2),
            vec!["b", "c"]
        );
        // `max` caps the count and rank order is preserved.
        assert_eq!(inband_alternate_candidates("a", &results, 1), vec!["b"]);
        // When the primary is the rank-2 anchor, it is still excluded.
        assert_eq!(
            inband_alternate_candidates("b", &results, 2),
            vec!["a", "c"]
        );
    }

    #[test]
    fn inband_alternates_dedupes_repeated_anchors() {
        let results = vec![result("a", 30.0), result("b", 29.0), result("b", 28.0)];
        assert_eq!(inband_alternate_candidates("a", &results, 5), vec!["b"]);
    }

    // ---- conceptual/overview routing: broad questions prefer curated prose.

    /// The polyglot proof: a mixed Go+Markdown corpus — nothing aden-specific.
    /// A broad conceptual question routes to the prose overview even though
    /// code symbols outscore it within the noise band; the discrimination is
    /// purely the anchor SCHEME (`aden://doc/…`), never a filename or format.
    #[test]
    fn ask_routing_overview_prefers_prose_over_code() {
        let q = "what is the design philosophy of widgetd";
        let intent = classify_intent(q);
        assert!(
            is_overview_query(q, &intent),
            "signal must engage for {q:?}"
        );
        let results = vec![
            result_with_path(
                "aden://module/widgetd/server.go#Server.Run",
                "internal/server.go",
                30.0,
            ),
            result_with_path(
                "aden://doc/widgetd/docs/design.md/h1widgetd-design",
                "docs/design.md",
                29.0,
            ),
            result_with_path("aden://module/widgetd/main.go#main", "cmd/main.go", 28.0),
        ];
        let chosen =
            resolve_anchor_overview(q, &results, &|_| 100, &std::collections::HashMap::new());
        assert_eq!(
            chosen.expect("prose doc in band must be chosen").0,
            "aden://doc/widgetd/docs/design.md/h1widgetd-design"
        );
    }

    /// A project-identity question ("what is <project>?") carries no lexical
    /// signal — every anchor mentions the project name — so routing heads for
    /// the corpus front door (README/index) rather than whichever section
    /// BM25-noise ranked first.
    #[test]
    fn ask_routing_identity_question_prefers_entry_doc() {
        let q = "what is widgetd";
        let intent = classify_intent(q);
        assert!(is_overview_query(q, &intent));
        let results = vec![
            result("aden://doc/widgetd/guide.md/h2advanced-usage", 9.5),
            result("aden://doc/widgetd/README.md/h2quick-start", 9.0),
            result("aden://module/widgetd/main.go#main", 8.9),
        ];
        let chosen =
            resolve_anchor_overview(q, &results, &|_| 100, &std::collections::HashMap::new());
        assert_eq!(
            chosen.expect("entry doc must be chosen").0,
            "aden://doc/widgetd/README.md/h2quick-start"
        );
    }

    /// The overview preference must stand down for queries that name a code
    /// symbol (snake_case / PascalCase / call syntax) and for non-broad intents
    /// — those keep today's routing byte-identically.
    #[test]
    fn overview_signal_stands_down_for_precise_or_narrow_queries() {
        for q in [
            "how does resolve_anchor_fuzzy work", // snake_case symbol
            "what is AdenConfig",                 // PascalCase symbol
            "what does parse_file() return",      // explicit call syntax
            "how do I use the assembler",         // Usage intent, not broad
        ] {
            let intent = classify_intent(q);
            assert!(
                !is_overview_query(q, &intent),
                "signal must NOT engage for {q:?}"
            );
        }
        // …and a question with no overview phrasing stays off too.
        let q = "how does aden integrate with AI agents";
        assert!(!is_overview_query(q, &classify_intent(q)));
    }

    /// No prose doc within the noise band ⇒ the overview resolver yields None
    /// and the caller falls through to the default selection unchanged.
    #[test]
    fn overview_falls_through_when_no_prose_in_band() {
        let results = vec![
            result("aden://module/p/core.py#Engine", 30.0),
            result("aden://doc/p/manual.md/h1manual", 20.0), // out of band
        ];
        assert!(
            resolve_anchor_overview(
                "what is the architecture",
                &results,
                &|_| 100,
                &std::collections::HashMap::new()
            )
            .is_none()
        );
    }

    /// Within-document broadening: the canonical anchor is the SAME-FILE doc
    /// anchor with the highest cross-reference in-degree; anchors of other
    /// files never qualify, and an anchor that already is the most-referenced
    /// one has nothing better.
    #[test]
    fn same_file_canonical_prefers_most_referenced_same_file_anchor() {
        let mut indeg = std::collections::HashMap::new();
        indeg.insert(
            "aden://doc/p/philosophy.adoc#philosophy".to_string(),
            6usize,
        );
        indeg.insert("aden://doc/p/other.adoc#other".to_string(), 9usize);
        assert_eq!(
            same_file_canonical_anchor("aden://doc/p/philosophy.adoc/h1aden-philosophy", &indeg),
            Some(("aden://doc/p/philosophy.adoc#philosophy".to_string(), 6))
        );
        // Already the canonical one — no better same-file target exists.
        assert_eq!(
            same_file_canonical_anchor("aden://doc/p/philosophy.adoc#philosophy", &indeg),
            None
        );
    }
}
