// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use aden_graph::Direction;
use aden_store::GraphStorage;

use crate::types::{AnchorPattern, QueryIntent};
use crate::util::{
    find_project_root, fmt_score, load_or_build_index, node_to_json, parse_single_edge_type,
    perform_check, query_index, query_relevance_confidence, valid_edge_types,
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

/// Deterministically decompose a multi-part question into evidence-role search
/// queries. This is intentionally lexical: routing never depends on an LLM.
fn evidence_facet_queries(question: &str) -> Vec<String> {
    let lower = question.to_lowercase();
    let repeated_interrogative = ["how", "what", "which", "where", "who", "when", "why"]
        .iter()
        .any(|word| lower.contains(&format!(" and {word} ")));
    if !lower.contains(',') && !repeated_interrogative {
        return Vec::new();
    }
    let normalized = lower.replace(" and ", "|").replace(',', "|");
    let facets: Vec<String> = normalized
        .split('|')
        .filter_map(|facet| {
            let terms: BTreeSet<String> = facet
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .filter(|word| word.len() >= 3 && !SYMBOL_STOP_WORDS.contains(word))
                .map(str::to_string)
                .collect();
            (terms.len() >= 3).then(|| terms.into_iter().collect::<Vec<_>>().join(" "))
        })
        .collect();
    if (2..=3).contains(&facets.len()) {
        facets
    } else {
        Vec::new()
    }
}

fn explicit_snake_symbol(question: &str) -> Option<String> {
    question
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .find(|word| word.contains('_') && word.len() >= 3)
        .map(str::to_string)
}

fn exact_symbol_anchor(idx: &aden_index::Index, question: &str) -> Option<String> {
    let symbol = explicit_snake_symbol(question)?;
    query_index(idx, &symbol)
        .into_iter()
        .find(|result| result.anchor.ends_with(&format!("#{symbol}")))
        .map(|result| result.anchor)
}

fn search_result_role(result: &SearchResult) -> String {
    result
        .source_path
        .components()
        .take(5)
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn facet_anchor_score(facet: &str, result: &SearchResult) -> usize {
    let terms: HashSet<&str> = facet.split_whitespace().collect();
    let anchor = result.anchor.to_lowercase();
    let overlap = anchor
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| terms.contains(token))
        .count();
    let section_specificity = if anchor.contains("/h3") || anchor.contains("/h4") {
        2
    } else if anchor.contains("/h2") {
        1
    } else {
        0
    };
    overlap * 4 + section_specificity
}

/// Select one source per question facet, preferring distinct architectural
/// layers. Three seeds cover the common entry/component/contract shape while
/// keeping assembly bounded.
fn evidence_facet_seeds(idx: &aden_index::Index, question: &str) -> Vec<String> {
    let mut selected = Vec::new();
    let mut files = HashSet::new();
    let mut roles = HashSet::new();
    let facets = evidence_facet_queries(question);
    let rankings = std::thread::scope(|scope| {
        let handles: Vec<_> = facets
            .iter()
            .map(|facet| scope.spawn(move || query_index(idx, facet)))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_default())
            .collect::<Vec<_>>()
    });
    for (facet, results) in facets.iter().zip(&rankings) {
        let allow_prose = facet.split_whitespace().any(|term| term == "adr");
        let eligible = |result: &&SearchResult| {
            !is_test_result(result)
                && (allow_prose || !AnchorPattern::is_prose_doc(&result.anchor))
                && !files.contains(&result.source_path)
                && !roles.contains(&search_result_role(result))
        };
        let candidate = if allow_prose {
            results
                .iter()
                .filter(eligible)
                .max_by_key(|result| facet_anchor_score(facet, result))
        } else {
            results.iter().find(eligible)
        };
        if let Some(result) = candidate.or_else(|| {
            results.iter().find(|result| {
                !is_test_result(result)
                    && (allow_prose || !AnchorPattern::is_prose_doc(&result.anchor))
                    && !files.contains(&result.source_path)
            })
        }) {
            files.insert(result.source_path.clone());
            roles.insert(search_result_role(result));
            selected.push(result.anchor.clone());
            if selected.len() >= 3 {
                break;
            }
        }
    }
    // Cross-boundary lifecycle questions often name two facets but require a
    // third integration/component layer. Fill it from the facet rankings while
    // preserving source-role diversity.
    if selected.len() >= 2 && selected.len() < 3 {
        'fill: for results in rankings.iter().rev() {
            for result in results {
                let role = search_result_role(result);
                if is_test_result(result)
                    || AnchorPattern::is_prose_doc(&result.anchor)
                    || files.contains(&result.source_path)
                    || roles.contains(&role)
                {
                    continue;
                }
                files.insert(result.source_path.clone());
                roles.insert(role);
                selected.push(result.anchor.clone());
                break 'fill;
            }
        }
    }
    selected
}

/// Split an explicit prose conjunction into same-file section searches. This is
/// Restricted to thin primaries: it addresses questions whose answer-bearing
/// facts live in sibling sections without increasing the serialized budget.
fn prose_same_file_facet_seeds(
    idx: &aden_index::Index,
    question: &str,
    source: &Path,
) -> Vec<String> {
    let facets = prose_conjunction_facets(question);
    if facets.len() < 2 || facets.len() > 3 {
        return Vec::new();
    }
    let mut selected = Vec::new();
    for facet in facets {
        if let Some(result) = query_index(idx, facet).into_iter().find(|result| {
            AnchorPattern::is_prose_doc(&result.anchor)
                && result.source_path == source
                && !selected.contains(&result.anchor)
        }) {
            selected.push(result.anchor);
        }
    }
    if selected.len() >= 2 {
        selected
    } else {
        Vec::new()
    }
}

fn prose_conjunction_facets(question: &str) -> Vec<&str> {
    question
        .split(" and ")
        .map(str::trim)
        .filter(|facet| facet.split_whitespace().count() >= 3)
        .collect()
}

/// Rank short paragraph passages across one already-selected prose file.
/// This is deliberately a *post-file-selection* operation: it cannot pull in
/// another file or change navigation, and its fixed four-window cap prevents a
/// long paragraph from consuming the whole context budget. Evidence-role terms
/// make interrogatives concrete without an LLM (preparation → prerequisites,
/// danger → risks, "when does it stop" → boundaries/tradeoffs).
fn focused_prose_passages(source: &str, question: &str, budget: usize) -> Option<String> {
    use std::collections::{HashMap, HashSet};

    fn terms(text: &str) -> HashSet<String> {
        const STOP: &[&str] = &[
            "the", "and", "for", "from", "with", "into", "what", "when", "where", "which", "does",
            "should", "would", "could", "this", "that", "about", "versus", "how", "why", "your",
            "have", "stay",
        ];
        text.split(|c: char| !c.is_ascii_alphanumeric())
            .filter_map(|raw| {
                let mut term = raw.to_ascii_lowercase();
                if term.ends_with("ily") && term.len() > 5 {
                    term.truncate(term.len() - 3);
                    term.push('y');
                } else if term.ends_with("ly") && term.len() > 5 {
                    term.truncate(term.len() - 2);
                }
                if term.ends_with('s') && term.len() > 4 {
                    term.pop();
                }
                (term.len() > 2 && !STOP.contains(&term.as_str())).then_some(term)
            })
            .collect()
    }

    let mut query_terms = terms(question);
    let q = question.to_ascii_lowercase();
    let roles: &[&str] = if q.contains("prepare") {
        &["before", "prerequisite", "ensure", "clean", "ready"]
    } else if q.contains("danger") || q.contains("risk") {
        &[
            "danger",
            "destroy",
            "overwrite",
            "cannot",
            "unrecoverable",
            "risk",
        ]
    } else if q.contains("when") || q.contains("stop helping") {
        &[
            "threshold",
            "below",
            "above",
            "limit",
            "tradeoff",
            "advantage",
            "disadvantage",
        ]
    } else if q.contains("fail") || q.contains("failure") {
        &[
            "failure",
            "fail",
            "collapse",
            "unbalanced",
            "imbalance",
            "limitation",
            "disappear",
        ]
    } else if q.contains("waste") || q.contains("efficien") {
        &[
            "waste",
            "fragmentation",
            "eliminate",
            "packing",
            "utilization",
            "overhead",
        ]
    } else if q.contains("install") || q.contains("setup") || q.contains("set up") {
        &[
            "install",
            "setup",
            "component",
            "add",
            "configure",
            "rustup",
        ]
    } else {
        &[]
    };
    query_terms.extend(roles.iter().map(|term| (*term).to_string()));
    // Preparation and execution commonly appear as two halves of one question
    // ("prepare for and manually resolve ..."). Keep the primary evidence role
    // above, but also reserve lexical weight for hands-on resolution evidence.
    // This is deliberately narrower than accumulating every matching role.
    if q.contains("manual") && (q.contains("resolve") || q.contains("merge")) {
        query_terms.extend(["manual", "fix", "command"].map(str::to_string));
    }

    let mut passages = Vec::new();
    let mut pending_heading: Option<String> = None;
    for raw in source.split("\n\n") {
        let clean = aden_asm::traverse::strip_asciidoc_markup(raw)
            .trim()
            .to_string();
        let is_heading = raw
            .lines()
            .any(|line| line.trim_start().starts_with('=') || line.trim_start().starts_with("# "));
        if is_heading && clean.len() < 120 {
            pending_heading = Some(clean);
            continue;
        }
        if clean.len() < 20 {
            continue;
        }
        passages.push(match pending_heading.take() {
            Some(heading) => format!("{heading}\n{clean}"),
            None => clean,
        });
    }
    if passages.len() < 4 || query_terms.is_empty() {
        return None;
    }
    let mut df: HashMap<String, usize> = HashMap::new();
    let passage_terms: Vec<HashSet<String>> = passages
        .iter()
        .map(|passage| {
            let found = terms(passage);
            for term in query_terms.intersection(&found) {
                *df.entry(term.clone()).or_default() += 1;
            }
            found
        })
        .collect();
    let mut ranked: Vec<(f64, usize)> = passage_terms
        .iter()
        .enumerate()
        .map(|(i, found)| {
            let score = query_terms
                .intersection(found)
                .map(|term| ((passages.len() + 1) as f64 / (df[term] + 1) as f64).ln() + 1.0)
                .sum();
            (score, i)
        })
        .filter(|(score, _)| *score > 0.0)
        .collect();
    ranked.sort_by(|(sa, ia), (sb, ib)| sb.total_cmp(sa).then_with(|| ia.cmp(ib)));

    let max_bytes = budget.saturating_mul(4);
    let per_passage = max_bytes / 4;
    let separator = "\n\n---\n\n";
    let mut output = String::new();
    let coverage_stop = std::env::var_os("ADEN_PROSE_COVERAGE_STOP_OFF").is_none();
    let mut uncovered: HashSet<String> = if coverage_stop {
        passage_terms
            .iter()
            .flat_map(|found| query_terms.intersection(found).cloned())
            .collect()
    } else {
        HashSet::new()
    };
    let mut required_facet_passages: HashSet<usize> = if coverage_stop {
        prose_conjunction_facets(question)
            .into_iter()
            .filter_map(|facet| {
                let facet_terms = terms(facet);
                passage_terms
                    .iter()
                    .enumerate()
                    .map(|(i, found)| {
                        let score: f64 = facet_terms
                            .intersection(found)
                            .filter_map(|term| df.get(term).map(|count| (term, count)))
                            .map(|(_, count)| {
                                ((passages.len() + 1) as f64 / (*count + 1) as f64).ln() + 1.0
                            })
                            .sum();
                        (score, i)
                    })
                    .filter(|(score, _)| *score > 0.0)
                    .max_by(|(sa, ia), (sb, ib)| sa.total_cmp(sb).then_with(|| ib.cmp(ia)))
                    .map(|(_, i)| i)
            })
            .collect()
    } else {
        HashSet::new()
    };
    for (_, i) in ranked.into_iter().take(4) {
        let overhead = if !output.is_empty() {
            separator.len()
        } else {
            0
        };
        let remaining = max_bytes.saturating_sub(output.len() + overhead);
        if remaining < 64 {
            break;
        }
        let passage = &passages[i];
        let mut take = passage.len().min(per_passage).min(remaining);
        while take > 0 && !passage.is_char_boundary(take) {
            take -= 1;
        }
        if !output.is_empty() {
            output.push_str(separator);
        }
        output.push_str(passage[..take].trim_end());
        if coverage_stop {
            uncovered.retain(|term| !passage_terms[i].contains(term));
            required_facet_passages.remove(&i);
            if uncovered.is_empty() && required_facet_passages.is_empty() {
                break;
            }
        }
    }
    (!output.is_empty()).then_some(output)
}

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
const ALTERNATE_RELATIVE_BAND: f64 = 0.05;

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
    let alternate_band = ANCHOR_NOISE_BAND.max(top_score.abs() * ALTERNATE_RELATIVE_BAND);
    let primary_source = results
        .iter()
        .find(|result| result.anchor == primary)
        .map(|result| result.source_path.clone());
    let mut sources = HashSet::new();
    if let Some(source) = primary_source.filter(|source| !source.as_os_str().is_empty()) {
        sources.insert(source);
    }
    let mut out: Vec<String> = Vec::new();
    for r in results {
        if (top_score - r.score) > alternate_band {
            break; // results are score-ordered; nothing past here is in-band
        }
        // Skip the primary, dupes, and test fixtures — alternates seed extra
        // context, so a dir-only test file shouldn't pad the answer either.
        if r.anchor == primary
            || out.contains(&r.anchor)
            || is_test_result(r)
            || (!r.source_path.as_os_str().is_empty() && sources.contains(&r.source_path))
        {
            continue;
        }
        out.push(r.anchor.clone());
        if !r.source_path.as_os_str().is_empty() {
            sources.insert(r.source_path.clone());
        }
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
            || (w.chars().any(|c| c.is_ascii_lowercase())
                && w.chars().skip(1).any(|c| c.is_ascii_uppercase()))
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
                    .map(|fragment| {
                        let lower = fragment.to_lowercase();
                        lower == *sym
                            || lower.rsplit(['.', ':']).find(|part| !part.is_empty())
                                == Some(sym.as_str())
                    })
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
        // A generic word that happens to occur in a symbol name must not
        // override a clearly winning prose document ("format a project" used
        // to jump from README#Running to convert_message_format_to_*). Explicit
        // call syntax was handled above; implicit matches only arbitrate an
        // actual score ambiguity.
        if AnchorPattern::is_prose_doc(&results[0].anchor)
            && !AnchorPattern::is_prose_doc(&result.anchor)
            && !has_symbolish_token(query)
            && (results[0].score - result.score)
                > ANCHOR_NOISE_BAND.max(results[0].score.abs() * ALTERNATE_RELATIVE_BAND)
        {
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
        query_tokens.contains(&sym_lower)
            || sym_stem.iter().any(|st| query_tokens.contains(st))
            || sym_lower
                .split(['.', ':'])
                .filter(|part| part.len() >= 3)
                .any(|part| query_tokens.iter().any(|token| token == part))
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
    max_issues: Option<usize>,
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
    let (errors, warnings, info) = crate::util::classify_check_messages(&messages);
    let policy = aden_policy::audit_policy(path);
    let fails = !errors.is_empty() || (!warnings.is_empty() && min_severity <= 1);

    // Machine-readable for the global `-j/--json` flag (MCP Phase 2B envelope).
    if json {
        let cap = max_issues.unwrap_or(20);
        let summary = crate::util::build_gate_summary(&errors, &warnings, &info, !fails, cap);
        let outcome = crate::commands::outcome::OutcomeEnvelope::evaluated(
            errors.len() + if min_severity <= 1 { warnings.len() } else { 0 },
            if min_severity <= 1 { 0 } else { warnings.len() },
            if errors.is_empty() {
                "healthy"
            } else {
                "unhealthy"
            },
            crate::commands::outcome::policy_label(policy.violations.len(), policy.unwired),
            "not_evaluated",
        );
        let env = serde_json::json!({
            "ok": summary.ok,
            "counts": summary.counts,
            "top_issues": summary.top_issues,
            "truncated": summary.truncated,
            "policy_mode": policy.mode,
            "policy_violations": policy.violations,
            "policy_unwired": policy.unwired,
            "result": outcome,
        });
        println!("{}", serde_json::to_string(&env)?);
        if fails {
            std::process::exit(1);
        }
        return Ok(());
    }

    if let Some(cap) = max_issues {
        let summary = crate::util::build_gate_summary(&errors, &warnings, &info, !fails, cap);
        println!("{}", crate::util::gate_summary_line(&summary));
        for issue in &summary.top_issues {
            println!("{issue}");
        }
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
    pub select: bool,
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
    if opts.strict && opts.inspect {
        return Err("--strict cannot be combined with --inspect: inspection output is not a context assembly and has no bounded serialization contract".into());
    }
    if opts.strict && opts.out.is_some() {
        return Err("--strict cannot be combined with --out: strict mode bounds the serialized stdout response; omit --out and capture stdout instead".into());
    }
    validate_strict_budget(opts.strict, opts.budget)?;
    // A stale hint is ordinary terminal chrome. In strict mode it cannot be
    // appended after the response boundary; AP-103/AP-101B will carry this in
    // the versioned receipt instead.
    let _stale_hint = super::StaleHintGuard::new(&opts.path, opts.strict);
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

    // Opt-in query-aware selection (`--select`): gather the neighborhood, then
    // select to budget by relevance to `--from` rather than walking by structural
    // priority. Benched (assembly_ab harness) to pull query-relevant deep nodes
    // into tight budgets that the structural walk never reaches (Go: 0/3 -> 3/3),
    // signal-agnostic so the cheap BM25 score is enough. Default OFF: the off-topic
    // safety gate is the open rank-calibration problem, so this stays explicit.
    // Compute the relevance map AND its cross-query calibrated confidence together
    // (one index load, one query). The confidence is the off-topic safety gate: when
    // the query has no good match (low best-cosine), gather-then-select defers to the
    // structural walk instead of churning the bundle on noise. `None` (BM25-only / no
    // model) leaves the gate at full strength, the prior behavior.
    let (relevance, relevance_confidence) = if opts.select {
        let index = load_or_build_index(&opts.path)?;
        let map: std::collections::HashMap<String, f32> = query_index(&index, &opts.from)
            .into_iter()
            .map(|r| (r.anchor, r.score as f32))
            .collect();
        let conf = query_relevance_confidence(&index, &opts.from);
        ((!map.is_empty()).then_some(map), conf)
    } else {
        (None, None)
    };
    let relevance_select = relevance.is_some();

    let assembly_budget = if opts.strict && opts.format == "json" {
        effective_budget.saturating_sub(16)
    } else {
        effective_budget
    };
    let asm_opts = AssemblyOptions {
        start_anchor: resolved_anchor,
        max_depth: opts.depth,
        token_budget: assembly_budget,
        edge_types: opts.edge_types.clone(),
        block_filter: Vec::new(),
        include_tags: opts.include_tags.clone(),
        exclude_tags: opts.exclude_tags.clone(),
        attributes: opts.attributes.clone(),
        llm_mode,
        hydrate_root: None,
        relevance,
        relevance_select,
        // Off-topic safety gate, validated on the assembly_ab harness (two languages,
        // recall-neutral) before wiring here. `--select` stays opt-in until a broader
        // multi-language eval justifies flipping the default.
        relevance_confidence,
    };

    let output = match opts.format.as_str() {
        "adg" => assemble_adg(&graph, &asm_opts)?,
        "json" => {
            let documents: serde_json::Value =
                serde_json::from_str(&assemble_adg(&graph, &asm_opts)?)?;
            serde_json::to_string(&serde_json::json!({
                "schema_version": 1,
                "context_receipt": { "schema_version": 1 },
                "documents": documents,
            }))?
        }
        "aden" | "llm" => assemble(&graph, &asm_opts)?,
        _ => {
            return Err(format!(
                "Unknown format: '{}'. Use 'json' (default), 'llm', 'adg', or 'aden' (raw AsciiDoc).",
                opts.format
            )
            .into());
        }
    };

    if let Some(out_path) = &opts.out {
        std::fs::write(out_path, &output)?;
        println!("Written assembly to {}", out_path.display());
    } else if opts.strict {
        print!("{}", strict_serialized_response(&output, effective_budget));
    } else if opts.silent {
        print!("{output}");
    } else {
        println!("{output}");
    }
    Ok(())
}

/// Phase 6 (provenance): annotate a query-result node with the edge type(s) that
/// reached it and whether any is *inferred* — an embedding-derived edge
/// (`SimilarTo`, from the dense similarity pass, per Phase 2) rather than an edge
/// authored in source/markup or parsed from structure. Note `inferred` is
/// narrower than "semantic": authored conceptual edges (`Mentions`, `IsA`,
/// `PartOf`) are semantic but NOT inferred, so they read as hard facts here.
/// Purely additive: only inserts keys, so existing JSON consumers are unaffected.
fn annotate_edge_provenance(node: &mut serde_json::Value, via: &[aden_core::EdgeType]) {
    if let Some(obj) = node.as_object_mut() {
        let names: Vec<serde_json::Value> = via
            .iter()
            .map(|e| serde_json::Value::String(format!("{:?}", e)))
            .collect();
        let inferred = via.iter().any(|e| e.is_inferred());
        obj.insert(
            "via_edge_types".to_string(),
            serde_json::Value::Array(names),
        );
        obj.insert("inferred".to_string(), serde_json::Value::Bool(inferred));
    }
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
    let _stale_hint = super::StaleHintGuard::new(path, format == "json");
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
                let via = edges_between(node, neighbor);
                let passes = match filter_type {
                    Some(ft) => via.contains(&ft),
                    None => true,
                };
                if !passes {
                    continue;
                }
                if visited.insert(neighbor) {
                    let mut nj = node_to_json(&graph.graph[neighbor], d + 1);
                    annotate_edge_provenance(&mut nj, &via);
                    results.push(nj);
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
            let via = edges_between(neighbor, target_idx);
            if let Some(ft) = filter_type
                && !via.contains(&ft)
            {
                continue;
            }
            if seen.insert(neighbor) {
                let mut nj = node_to_json(&graph.graph[neighbor], 1);
                annotate_edge_provenance(&mut nj, &via);
                results.push(nj);
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
                let via = edges_between(node, neighbor);
                if !via.iter().any(|et| impact_types.contains(et)) {
                    continue;
                }
                if visited.insert(neighbor) {
                    let mut nj = node_to_json(&graph.graph[neighbor], d + 1);
                    annotate_edge_provenance(&mut nj, &via);
                    results.push(nj);
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
        "json" => {
            let env = super::augment_read_json(path, serde_json::Value::Array(results));
            println!("{}", serde_json::to_string_pretty(&env)?);
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

/// Clamp an already-rendered response to Aden's public byte-based token budget.
///
/// Assembly accounts for its own separators, but command-level framing and
/// future supplements are independent producers. This boundary helper is the
/// last line of defense for `ask --strict`: it preserves UTF-8 validity and
/// guarantees `response.len().div_ceil(4) <= budget`.
const MINIMAL_INCOMPLETE_RECEIPT: &str =
    r#"{"context_receipt":{"schema_version":1},"incomplete":true}"#;
const MIN_STRICT_BUDGET: usize = 15;

fn validate_strict_budget(strict: bool, budget: usize) -> Result<(), Box<dyn std::error::Error>> {
    if strict && budget < MIN_STRICT_BUDGET {
        return Err(format!(
            "--strict requires --budget >= {MIN_STRICT_BUDGET} so the minimal incomplete receipt can fit"
        )
        .into());
    }
    Ok(())
}

/// Serialize an agent-facing strict response at the final boundary.
///
/// Never cut a UTF-8/JSON/ADG/AsciiDoc response at an arbitrary byte. When an
/// upstream producer exceeds its contract (or has no useful result), return a
/// complete, machine-readable incomplete receipt instead.
fn strict_serialized_response(response: &str, budget: usize) -> String {
    // At the minimum budget only the receipt is guaranteed meaningful. A
    // renderer may otherwise emit a syntactically complete but semantically
    // empty shell (`[\n\n]`, a title, or separators) and imply completeness.
    if budget > MIN_STRICT_BUDGET
        && !response.trim().is_empty()
        && response.len().div_ceil(4) <= budget
    {
        response.to_string()
    } else {
        debug_assert!(MINIMAL_INCOMPLETE_RECEIPT.len().div_ceil(4) <= budget);
        MINIMAL_INCOMPLETE_RECEIPT.to_string()
    }
}

fn prepend_provenance_if_fits(response: &str, source: &str, budget: usize) -> String {
    if source.is_empty() || response.contains(source) {
        return response.to_string();
    }
    let header = format!("// source: {source}\n");
    if (header.len() + response.len()).div_ceil(4) <= budget {
        return format!("{header}{response}");
    }

    // Prose aliases can render twice at the front (`alias\n\nalias\nTitle`).
    // Replace only that redundant chrome with compact provenance when a normal
    // header has no headroom. The replacement must not grow the response.
    let Some(first_end) = response.find('\n') else {
        return response.to_string();
    };
    let leader = &response[..first_end];
    let duplicate = format!("\n{leader}\n");
    let remainder = &response[first_end + 1..];
    if leader.is_empty() || !remainder.starts_with(&duplicate) {
        return response.to_string();
    }
    let consumed = first_end + 1 + duplicate.len();
    let basename = Path::new(source)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(source);
    let compact = format!("@{basename}\n\n");
    if compact.len() > consumed {
        return response.to_string();
    }
    let candidate = format!("{compact}{}", &response[consumed..]);
    if candidate.len().div_ceil(4) <= budget {
        candidate
    } else {
        response.to_string()
    }
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
    // ADR-011: prefer graph.snapshot (via try_read_fresh) for lock-free reads.
    if let Some((docs, _)) = aden_graph::snapshot::try_read_fresh(path)
        && let Some(d) = docs.get(anchor)
    {
        return d.attributes.get("source_file").cloned();
    }

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

/// Assembles a seed anchor's neighborhood at a given depth/budget/block filter,
/// optionally folding in the seed's callers (incoming Calls/Uses). Holds the
/// per-`ask` invariants (edge types, default block filter, hydration root,
/// search relevance) so the thin-stub escalation ladder can re-assemble a seed
/// many ways without re-threading them. Hydration is always on: `ask` answers
/// from real source bodies, not just stored summaries (the store never holds
/// function bodies).
struct AskAssembler<'a> {
    path: &'a Path,
    edge_types: Vec<aden_core::EdgeType>,
    block_filter: Vec<aden_asm::traverse::BlockKind>,
    hydrate_root: PathBuf,
    relevance: Option<std::collections::HashMap<String, f32>>,
}

impl AskAssembler<'_> {
    /// Core assembly helper — returns (text, included_anchors) so callers can
    /// resolve source files for baseline estimation without a second traversal.
    fn assemble_seed_with(
        &self,
        seed: &str,
        seed_depth: usize,
        seed_budget: usize,
        filter: &[aden_asm::traverse::BlockKind],
        with_callers: bool,
    ) -> Result<(String, Vec<String>), Box<dyn std::error::Error>> {
        use aden_asm::traverse::{AssemblyOptions, assemble_with_anchors_mmr};
        let graph = if with_callers {
            aden_graph::cache::build_neighborhood_with_callers(
                self.path,
                seed,
                seed_depth,
                &self.edge_types,
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
            aden_graph::cache::build_neighborhood_cached(
                self.path,
                seed,
                seed_depth,
                &self.edge_types,
            )?
        };
        let opts = AssemblyOptions {
            start_anchor: seed.to_string(),
            max_depth: seed_depth,
            token_budget: seed_budget,
            edge_types: self.edge_types.clone(),
            block_filter: filter.to_vec(),
            include_tags: Vec::new(),
            exclude_tags: Vec::new(),
            attributes: Vec::new(),
            llm_mode: true, // aden ask always targets an LLM — emit clean prose
            hydrate_root: Some(self.hydrate_root.clone()),
            relevance: self.relevance.clone(),
            relevance_select: false,
            relevance_confidence: None,
        };
        // Prune near-duplicate neighbors before they spend budget. τ=0.8 skips
        // only genuine near-dups (≥80% token overlap); the headroom probe showed
        // this alters 42–100% of hubs, trading redundant context for distinct.
        Ok(assemble_with_anchors_mmr(&graph, &opts, Some(0.8))?)
    }

    /// Convenience wrapper for callers that only need the assembled text.
    fn assemble_seed_str(
        &self,
        seed: &str,
        seed_depth: usize,
        seed_budget: usize,
        filter: &[aden_asm::traverse::BlockKind],
        with_callers: bool,
    ) -> Result<String, Box<dyn std::error::Error>> {
        self.assemble_seed_with(seed, seed_depth, seed_budget, filter, with_callers)
            .map(|(text, _)| text)
    }

    /// Assemble at the intent's default block filter with callers off.
    fn assemble_seed(
        &self,
        seed: &str,
        seed_depth: usize,
        seed_budget: usize,
    ) -> Result<String, Box<dyn std::error::Error>> {
        self.assemble_seed_str(seed, seed_depth, seed_budget, &self.block_filter, false)
    }
}

/// Render the `--explain` routing trace: the signals selection used, which path
/// decided, and whether the thin-stub fallback swapped or was suppressed. The
/// `Primary` line is the routed anchor BEFORE any fallback; the summary's
/// `Anchor` line is the FINAL anchor — when they differ, `Fallback` says why.
/// (Format consumed by scripts/eval_corpus.py --mode ask.)
fn print_ask_explain(
    xp: &AskExplain,
    intent: &QueryIntent,
    intent_was_overridden: bool,
    path: &Path,
    primary_anchor: &str,
) {
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
            "//   Candidates (top {}, * = within alternate band max({}, {:.0}% of top score)):",
            xp.candidates.len(),
            ANCHOR_NOISE_BAND,
            ALTERNATE_RELATIVE_BAND * 100.0
        );
        for line in &xp.candidates {
            println!("//     {}", line);
        }
    }
    println!("//   Decision : {}", xp.decision);
    println!("//   Fallback : {}", xp.fallback);
    match anchor_source_file(path, primary_anchor) {
        Some(src) if !src.is_empty() => {
            println!("//   Primary  : {} (source: {})", primary_anchor, src)
        }
        _ => println!("//   Primary  : {}", primary_anchor),
    }
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
    json_output: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("ask requires a directory path".into());
    }
    if strict && model.is_some() {
        return Err("--strict cannot be combined with --model: model output is generated outside Aden's serialized context budget".into());
    }
    validate_strict_budget(strict, budget)?;
    let _stale_hint = super::StaleHintGuard::new(path, strict);
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
    let (start_anchor, avg_score, alt_candidates, relevance, routing_note, evidence_routed) =
        if let Some(anchor) = from_override {
            xp.decision = "pinned by --from (no search routing)".to_string();
            (anchor.to_string(), None, Vec::new(), None, None, false)
        } else {
            let idx = load_or_build_index(path)?;
            let results = crate::util::query_index_with_navigation(&idx, question, path);
            if results.is_empty() {
                let no_results = format!(
                    "No relevant documents found for: {}\nTips:\n  - Use more specific keywords from the codebase.\n  - Try `aden search <term>` to see available anchors.\n  - Or pin an anchor with --from <anchor>.\n",
                    question
                );
                if strict {
                    print!("{}", strict_serialized_response("", budget));
                } else {
                    print!("{no_results}");
                }
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
            // True when overview routing deliberately promoted a prose doc over the
            // (in-band) rank-1 result. In that case the in-band "alternates" below are
            // that intentional bypass, not a near-tie — so the user-facing note must
            // not cry "ambiguous".
            let mut overview_promoted = false;
            let mut primary = if overview {
                let indegree = doc_reference_indegree(path);
                match resolve_anchor_overview(question, &results, &token_count, &indegree) {
                    Some((anchor, why)) => {
                        xp.decision = why;
                        overview_promoted = true;
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
                let (anchor, why) =
                    resolve_anchor_fuzzy_with_reason(question, &results, token_count);
                xp.decision = why.to_string();
                anchor
            };
            let lower_question = question.to_lowercase();
            let relationship_query = [" call", "caller", "request path", "impact", "depend"]
                .iter()
                .any(|signal| lower_question.contains(signal));
            let exact_anchor = if relationship_query {
                None
            } else {
                exact_symbol_anchor(&idx, question)
            };
            let exact_routed = exact_anchor.is_some();
            if let Some(anchor) = exact_anchor {
                xp.decision = "exact symbol token".to_string();
                primary = anchor;
            }
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
                            if (top_score - r.score)
                                <= ANCHOR_NOISE_BAND.max(top_score.abs() * ALTERNATE_RELATIVE_BAND)
                            {
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
            // Multi-part questions need distinct evidence roles, not merely BM25
            // near-ties. Facet seeds replace the primary only when at least two
            // independent roles were found; otherwise preserve existing routing.
            let precise_routed = exact_routed;
            let prose_facet_seeds = if !precise_routed
                && std::env::var_os("ADEN_PROSE_FACETS_OFF").is_none()
                && AnchorPattern::is_prose_doc(&primary)
                && idx.doc_token_count(&primary) <= THIN_STUB_TOKEN_THRESHOLD
            {
                results
                    .iter()
                    .find(|result| result.anchor == primary)
                    .map(|result| prose_same_file_facet_seeds(&idx, question, &result.source_path))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let facet_seeds = if !prose_facet_seeds.is_empty() {
                prose_facet_seeds
            } else if precise_routed {
                Vec::new()
            } else {
                evidence_facet_seeds(&idx, question)
            };
            let evidence_routed = facet_seeds.len() >= 2;
            let alts = if evidence_routed {
                primary = facet_seeds[0].clone();
                xp.decision = format!(
                    "deterministic evidence-role routing across {} facets",
                    facet_seeds.len()
                );
                facet_seeds.into_iter().skip(1).take(2).collect()
            } else if precise_routed {
                Vec::new()
            } else {
                // Up to 2 in-band alternates, deduped against the (possibly
                // non-rank-1) primary. Empty means a clear winner.
                inband_alternate_candidates(&primary, &results, 1)
            };
            // Decide the honest low-confidence note here, where we know whether the
            // alternates are a genuine near-tie or the deliberate overview bypass.
            let routing_note = if alts.is_empty() {
                None
            } else if evidence_routed {
                Some(format!(
                    "// note: evidence-role routing selected {} distinct source facets.",
                    alts.len() + 1
                ))
            } else if overview_promoted {
                Some(format!(
                    "// note: overview routing chose a prose doc over {} in-band result(s) \
                 (an intentional pick, not a tie); showing it plus shallow alternates. Pin a \
                 specific anchor with --from <anchor>.",
                    alts.len()
                ))
            } else {
                Some(format!(
                    "// note: routing ambiguous — {} near-tie candidate(s); showing the primary plus \
                 shallow alternates at a reduced budget. Re-run with --explain for scores, or pin \
                 with --from <anchor>.",
                    alts.len()
                ))
            };
            // Forward the search relevance into assembly frontier ordering: the same
            // hybrid (dense+BM25) scores that routed the seed now break structural
            // ties toward query-relevant neighbors. Anchors absent from the map score
            // 0.0 in `ordered_neighbors`, so an unmatched neighborhood degrades
            // exactly to the prior structural (edge_priority, anchor) order.
            let relevance: std::collections::HashMap<String, f32> = results
                .iter()
                .map(|r| (r.anchor.to_owned(), r.score as f32))
                .collect();
            (
                primary,
                Some(avg),
                alts,
                Some(relevance),
                routing_note,
                evidence_routed,
            )
        };

    // Apply the relevance boost by default; `--strict` opts out and treats
    // --budget as an exact cap (deterministic size for callers/agents). The
    // user's --budget is the BASE the boost multiplies.
    let effective_budget = match (strict, avg_score) {
        (false, Some(avg)) => {
            let boosted = auto_boosted_budget(budget, avg);
            // Confidence gating: in-band alternates mean routing is ambiguous —
            // there is no clear winner. Betting a fully-boosted budget on a
            // single, possibly-misranked anchor is the "fail big" failure mode
            // (a wrong primary balloons into a huge, confident-looking block).
            // Instead pull the budget halfway back toward the base; the reclaimed
            // tokens are spent on the shallow alternates below, so we fail small.
            if alt_candidates.is_empty() {
                boosted
            } else {
                (budget + boosted) / 2
            }
        }
        _ => budget,
    };

    // `--strict` caps the complete response, not merely the assembled body.
    // Routing chrome is useful interactively but is outside the assembler's
    // accounting, so it must stay out of strict output.
    if explain && !strict && !json_output {
        println!("// Aden Ask: '{}' → [[{}]]", question, start_anchor);
        if from_override.is_some() {
            println!("// (pinned by --from)");
        }
        // Honest low-confidence signal in the MAIN output (not just --explain),
        // decided at routing time so it distinguishes a genuine near-tie from a
        // deliberate overview pick (see `routing_note`).
        if let Some(note) = &routing_note {
            println!("{note}");
        }
        println!();
    }

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

    if explain && !strict {
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
    }

    // Step 3: Resolve the starting anchor against the store (no full-graph
    // load). Prefer an unambiguous exact/suffix match; if the search-derived
    // anchor cannot be resolved, fall back to the deterministic project root
    // rather than a random node, so the agent gets a coherent overview.
    let start_anchor = match aden_graph::cache::resolve_anchor_in_store(path, &start_anchor) {
        Some(a) => a,
        None => {
            if aden_graph::cache::resolve_anchor_in_store(path, "mod-project").is_some() {
                if explain {
                    eprintln!(
                        "NOTE: '{}' is not a graph anchor; using project root 'mod-project'.",
                        start_anchor
                    );
                }
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

    // Bundle the per-`ask` invariants so the thin-stub escalation ladder can
    // re-assemble a seed many ways without re-threading them (see AskAssembler).
    let asm = AskAssembler {
        path,
        edge_types: edge_types.clone(),
        block_filter: block_filter.clone(),
        hydrate_root: hydrate_root.clone(),
        relevance: relevance.clone(),
    };

    // Clear winner ⇒ today's behavior exactly: one seed, full budget. Ambiguous ⇒
    // primary takes the majority of the budget at full depth, and each shallow
    // alternate gets an even slice of the remainder, appended with a brief header.
    // This is paid for ONLY on near-ties, and the total stays within the budget.
    // `primary_text` tracks the body contributed by the PRIMARY anchor alone,
    // so the thin-stub check below can't be fooled by a fat alternate padding
    // the combined output past the threshold.
    let primary_text;
    let assembled = if resolved_alts.is_empty() {
        let (seed_text, _) =
            asm.assemble_seed_with(&start_anchor, depth, effective_budget, &block_filter, false)?;
        primary_text = seed_text.clone();
        seed_text
    } else {
        let role_budget = effective_budget / (resolved_alts.len() + 1);
        let primary_budget = if evidence_routed {
            role_budget
        } else {
            effective_budget * 60 / 100
        };
        let shallow_depth = depth.min(1);
        let (primary_seed_text, _) =
            asm.assemble_seed_with(&start_anchor, depth, primary_budget, &block_filter, false)?;
        let mut combined = primary_seed_text;
        primary_text = combined.clone();
        let mut used = combined.len().div_ceil(4);
        for alt in &resolved_alts {
            let sep = "\n\n---\n\n";
            let header = format!("// supporting evidence: [[{}]]\n", alt);
            let overhead = sep.len().div_ceil(4) + header.len().div_ceil(4);
            let remaining = effective_budget.saturating_sub(used + overhead);
            if remaining < 32 {
                break;
            }
            let alt_budget = if evidence_routed {
                remaining.min(role_budget)
            } else {
                remaining
            };
            let alt_text = asm.assemble_seed(alt, shallow_depth, alt_budget)?;
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
                    let body = asm.assemble_seed(&canon, depth, effective_budget).ok()?;
                    if body.len().div_ceil(4) <= est {
                        return None;
                    }
                    if explain {
                        eprintln!(
                            "NOTE: '{}' assembled thin (~{} tokens); broadening within the document to its canonical anchor [[{}]] (cross-reference in-degree {}).",
                            start_anchor, est, canon, d
                        );
                    }
                    xp.fallback = format!(
                        "thin (~{} tokens); broadened WITHIN the document to [[{}]] (cross-reference in-degree {})",
                        est, canon, d
                    );
                    Some((body, canon))
                });
            match broadened {
                Some(pair) => pair,
                None => {
                    if explain {
                        eprintln!(
                            "NOTE: '{}' assembled thin (~{} tokens) but is a prose document; keeping it (fallback swap suppressed).",
                            start_anchor, est
                        );
                    }
                    xp.fallback = format!(
                        "suppressed: primary assembled thin (~{} tokens) but is a prose document — kept",
                        est
                    );
                    (assembled, start_anchor)
                }
            }
        } else if resolved_alts.is_empty()
            && !AnchorPattern::is_prose_doc(&start_anchor)
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
                if let Ok(body) = asm.assemble_seed_str(
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
                    if explain {
                        eprintln!(
                            "NOTE: '{}' assembled underfull (~{} substantive tokens of {} budget); escalated without changing the anchor ({}).",
                            start_anchor, subst, effective_budget, label
                        );
                    }
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
                                let Ok(body) = asm.assemble_seed_str(
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
                            if explain {
                                eprintln!(
                                    "NOTE: '{}' stayed thin through escalation (~{} substantive tokens); kept, supplemented with community '{}' ({} symbols, members by query relevance).",
                                    start_anchor, best_subst, label, n
                                );
                            }
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
                        if explain {
                            eprintln!(
                                "NOTE: '{}' assembled thin (~{} substantive tokens) and nothing cleared the floor ({}); keeping the routed anchor.",
                                start_anchor, best_subst, floor
                            );
                        }
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

    // Passage-granular path for thick, explicitly multi-facet prose. File
    // selection remains authoritative; only the bytes chosen from that one
    // source file change. The external passage and routing matrices establish
    // a net quality+cost win; retain an OFF switch for diagnostic A/B runs.
    let assembled = if std::env::var_os("ADEN_PROSE_PASSAGES_OFF").is_none()
        && from_override.is_none()
        && AnchorPattern::is_prose_doc(&start_anchor)
        && prose_conjunction_facets(question).len() >= 2
    {
        anchor_source_file(path, &start_anchor)
            .and_then(|source_path| {
                std::fs::read_to_string(hydrate_root.join(&source_path))
                    .ok()
                    .map(|source| (source_path, source))
            })
            // Passage selection pays only when the file is larger than four
            // complete context windows. Below that, normal section assembly is
            // already selective enough and preserves useful local continuity.
            .filter(|(_, source)| source.len() > effective_budget.saturating_mul(4 * 4))
            .and_then(|(source_path, source)| {
                let provenance_tokens = format!("// source: {source_path}\n").len().div_ceil(4);
                focused_prose_passages(
                    &source,
                    question,
                    effective_budget.saturating_sub(provenance_tokens),
                )
            })
            .unwrap_or(assembled)
    } else {
        assembled
    };

    // Step 4: Send to LLM or print raw context. Strict stdout is bounded body
    // plus an opportunistic source header only when existing headroom pays for it.
    if let Some(model_spec) = model {
        if explain {
            print_ask_explain(&xp, &intent, intent_was_overridden, path, &primary_anchor);
            println!("// ────────────────────────────────────────────────");
        }
        query_llm(model_spec, question, &assembled, &start_anchor)?;
    } else if json_output {
        let context = if strict {
            anchor_source_file(path, &start_anchor)
                .map(|source| prepend_provenance_if_fits(&assembled, &source, effective_budget))
                .map(|body| strict_serialized_response(&body, effective_budget))
                .unwrap_or_else(|| strict_serialized_response(&assembled, effective_budget))
        } else {
            assembled.clone()
        };
        let payload = serde_json::json!({
            "schema_version": 1,
            "context_receipt": { "schema_version": 1 },
            "question": question,
            "anchor": start_anchor,
            "source_file": anchor_source_file(path, &start_anchor),
            "context": context,
            "intent": format!("{:?}", intent).to_lowercase(),
            "depth": depth,
            "budget": effective_budget,
            "strict": strict,
            "supporting_anchors": resolved_alts,
            "explain": explain.then_some(serde_json::json!({
                "decision": xp.decision,
                "fallback": xp.fallback,
                "primary_anchor": primary_anchor,
            })),
        });
        if strict {
            let max_bytes = effective_budget.saturating_mul(4);
            let full = serde_json::to_string(&payload)?;
            if full.len() <= max_bytes {
                print!("{full}");
            } else {
                // At tight budgets the envelope itself competes with evidence.
                // Preserve valid, versioned JSON and the largest context prefix;
                // omit optional routing metadata rather than violating --strict.
                let mut bounded_context = payload["context"].as_str().unwrap_or_default();
                loop {
                    let compact = serde_json::to_string(&serde_json::json!({
                        "schema_version": 1,
                        "context": bounded_context,
                        "truncated": true,
                    }))?;
                    if compact.len() <= max_bytes || bounded_context.is_empty() {
                        print!("{compact}");
                        break;
                    }
                    let next = bounded_context
                        .char_indices()
                        .rev()
                        .nth(15)
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    bounded_context = &bounded_context[..next];
                }
            }
        } else {
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
    } else if strict {
        // Assembly normally maintains this invariant. The final serializer
        // protects the public boundary from future receipts/supplements.
        let strict_body = anchor_source_file(path, &start_anchor)
            .map(|source| prepend_provenance_if_fits(&assembled, &source, effective_budget))
            .unwrap_or_else(|| assembled.clone());
        print!(
            "{}",
            strict_serialized_response(&strict_body, effective_budget)
        );
    } else if !explain {
        // Outcome-only default: internal routing and arbitration are invisible.
        print!("{}", assembled);
    } else {
        // Explain mode shows context with routing metadata and receipts.
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
                "<!-- Supporting evidence (shallow): {} -->",
                resolved_alts.join(", ")
            );
        }
        let strategy = format!("{:?}", intent);
        println!("<!-- Strategy: {} -->", strategy);
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
            print_ask_explain(&xp, &intent, intent_was_overridden, path, &primary_anchor);
        }
        println!("// ────────────────────────────────────────────────");
        println!("// Aden Ask Summary");
        println!("//   Question: {}", question);
        println!("//   Anchor  : [[{}]]", start_anchor);
        if !resolved_alts.is_empty() {
            println!("//   Evidence: {} (shallow)", resolved_alts.join(", "));
        }
        println!("//   Strategy: {} | Depth: {}", strategy, depth);
        println!(
            "//   Nodes   : {} | ~{} tokens ({} bytes) / {} budget ({})",
            node_count, est_tokens, bytes, budget_note, budget_label
        );
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

    #[test]
    fn evidence_facets_split_explicit_independent_roles_without_an_llm() {
        let facets = evidence_facet_queries(
            "Which services validate incoming requests, and which workers persist accepted records?",
        );
        assert_eq!(facets.len(), 2);
        assert!(facets[0].contains("validate"));
        assert!(facets[1].contains("persist"));
    }

    #[test]
    fn evidence_facets_leave_single_intent_questions_alone() {
        assert!(evidence_facet_queries("What does parse_file do?").is_empty());
    }

    #[test]
    fn prose_facets_split_explicit_conjunction_without_an_llm() {
        assert_eq!(
            prose_conjunction_facets(
                "How were token savings calculated and what did the timeline viewer add"
            ),
            vec![
                "How were token savings calculated",
                "what did the timeline viewer add"
            ]
        );
        assert!(prose_conjunction_facets("How does reset work?").len() < 2);
    }

    #[test]
    fn focused_prose_passages_cover_distinct_evidence_roles_within_budget() {
        let source = [
            "Overview\n\nA long generic introduction about the operation and its history.",
            "Preparation\n\nBefore starting, ensure the workspace is clean and ready.",
            "Resolution\n\nManually resolve the conflict with the merge tool.",
            "Background\n\nUnrelated implementation history and acknowledgements.",
            "Appendix\n\nMore unrelated notes to make this a multi-passage document.",
        ]
        .join("\n\n");
        let output = focused_prose_passages(
            &source,
            "How should I prepare and manually resolve the conflict?",
            128,
        )
        .expect("focused passages");
        assert!(output.contains("workspace is clean"));
        assert!(output.contains("Manually resolve"));
        assert!(output.len() <= 128 * 4);
    }

    #[test]
    fn focused_prose_passages_return_none_for_tiny_sources() {
        assert!(focused_prose_passages("one short paragraph", "when does it stop?", 128).is_none());
    }

    #[test]
    fn focused_prose_passages_keep_headings_and_route_failure_evidence() {
        let source = [
            "== Overview\n\nA general introduction to sparse routing.",
            "== Normal path\n\nTokens select experts using learned scores.",
            "== Failure boundary\n\nAt batch size 1 the balancing signal disappears and routing becomes unbalanced.",
            "== Appendix\n\nAdditional unrelated implementation notes.",
        ]
        .join("\n\n");
        let output = focused_prose_passages(&source, "Why can expert load balancing fail?", 128)
            .expect("focused passages");
        assert!(output.contains("Failure boundary"));
        assert!(output.contains("signal disappears"));
        assert!(output.len() <= 128 * 4);
    }

    #[test]
    fn focused_prose_passages_cover_preparation_and_manual_resolution() {
        let filler = "Complex merge conflicts can have many causes and require careful review.";
        let source = format!(
            "==== Merge Conflicts\n\nFirst make sure your working directory is clean before starting.\n\n{}\n\n{}\n\n{}\n\n===== Manual File Re-merging\n\nExtract all three stages into the working directory.\n\nNow manually fix the whitespace issue and re-merge with the git merge-file command.",
            filler, filler, filler
        );
        let output = focused_prose_passages(
            &source,
            "How should I prepare for and manually resolve a complex merge conflict?",
            256,
        )
        .expect("long prose should be passage-ranked");

        assert!(output.contains("working directory is clean"));
        assert!(output.contains("git merge-file command"));
    }

    #[test]
    fn focused_prose_passages_stop_after_proving_term_coverage() {
        let source = "==== Credential helpers\n\nCache keeps credentials in memory while store saves them on disk.\n\nAn unrelated historical paragraph supplies enough source length for passage selection.\n\nAnother unrelated implementation paragraph discusses portability concerns.\n\nA final unrelated paragraph discusses configuration syntax and examples.";
        let output = focused_prose_passages(
            source,
            "How do credential cache and store helpers differ?",
            256,
        )
        .expect("long prose should be passage-ranked");

        assert!(output.contains("Cache keeps credentials"));
        assert!(!output.contains("---"));
    }

    #[test]
    fn focused_prose_passages_retain_each_conjunction_facet_winner() {
        let source = "==== Serving\n\nPagedAttention avoids fragmentation in the KV cache.\n\nA generic serving overview mentions batching and waste.\n\nContinuous batching reconfigures the live batch after each decoding step.\n\nAn unrelated paragraph discusses deployment topology and monitoring.";
        let output = focused_prose_passages(
            source,
            "How do PagedAttention and continuous batching reduce serving waste?",
            256,
        )
        .expect("long prose should be passage-ranked");

        assert!(output.contains("PagedAttention avoids fragmentation"));
        assert!(output.contains("Continuous batching reconfigures"));
    }

    #[test]
    fn evidence_facets_do_not_split_generic_conjunctions() {
        assert!(
            evidence_facet_queries("How are transactions committed and conflicts reported?")
                .is_empty()
        );
    }

    #[test]
    fn evidence_facets_split_balanced_conjunction_roles() {
        let facets = evidence_facet_queries(
            "How do concurrent consumers load snapshots and how do serialized producers publish updates?",
        );
        assert_eq!(facets.len(), 2);
    }

    #[test]
    fn evidence_facets_do_not_inject_domain_vocabulary() {
        let facets = evidence_facet_queries(
            "Where are request handlers registered, which registry lists available handlers, and which runtime handlers become active?",
        );
        assert_eq!(facets.len(), 3);
        assert!(facets.iter().all(|facet| !facet.contains("tool")));
        assert!(facets.iter().all(|facet| !facet.contains("session")));
    }

    #[test]
    fn explicit_snake_symbols_are_detected_without_call_syntax() {
        assert_eq!(
            explicit_snake_symbol("What boundaries does run_aden_command enforce?"),
            Some("run_aden_command".to_string())
        );
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

    #[test]
    fn strict_serialized_response_never_exceeds_budget_and_preserves_utf8() {
        let input = "café 你好 🚀 ".repeat(80);
        for budget in [15, 16, 64] {
            let output = strict_serialized_response(&input, budget);
            assert!(
                output.len().div_ceil(4) <= budget,
                "{} tokens exceeded strict budget {}",
                output.len().div_ceil(4),
                budget
            );
            assert_eq!(output, MINIMAL_INCOMPLETE_RECEIPT);
        }
        assert!(validate_strict_budget(true, 14).is_err());
        assert!(validate_strict_budget(true, 15).is_ok());
    }

    #[test]
    fn provenance_uses_only_existing_budget_headroom() {
        let body = "useful context";
        let with_source = prepend_provenance_if_fits(body, "src/main.rs", 32);
        assert!(with_source.starts_with("// source: src/main.rs\n"));
        assert!(with_source.ends_with(body));

        let full = "x".repeat(128);
        assert_eq!(prepend_provenance_if_fits(&full, "src/main.rs", 32), full);
        assert_eq!(prepend_provenance_if_fits(body, "", 32), body);
        assert_eq!(
            prepend_provenance_if_fits("source: src/main.rs", "src/main.rs", 32),
            "source: src/main.rs"
        );

        let prose = format!("_alias\n\n_alias\nTitle\n{}", "x".repeat(96));
        let compact = prepend_provenance_if_fits(
            &prose,
            "book/sections/aliases.adoc",
            prose.len().div_ceil(4),
        );
        assert!(compact.starts_with("@aliases.adoc\n\nTitle\n"));
        assert!(compact.len() <= prose.len());
        assert!(compact.ends_with(&"x".repeat(96)));
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

    #[test]
    fn ask_routing_does_not_let_generic_symbol_word_override_clear_prose_winner() {
        let results = vec![
            result("aden://doc/rustfmt/README.md/h2running", 40.0),
            result(
                "aden://module/rustfmt/src/main.rs#convert_message_format_to_args",
                23.0,
            ),
        ];
        let chosen = resolve_anchor_fuzzy(
            "How do I install rustfmt and format an entire project?",
            &results,
            |_| 100,
        );
        assert_eq!(chosen, "aden://doc/rustfmt/README.md/h2running");
    }

    #[test]
    fn explicit_call_matches_final_component_of_qualified_symbol() {
        let results = vec![
            result("aden://doc/kin/.github/docs/openapi3.txt#p315", 49.0),
            result(
                "aden://module/kin/openapi3/license.go#License.MarshalYAML",
                44.0,
            ),
        ];
        let chosen = resolve_anchor_fuzzy("Fix License MarshalYAML()", &results, |_| 100);
        assert_eq!(
            chosen,
            "aden://module/kin/openapi3/license.go#License.MarshalYAML"
        );
    }

    #[test]
    fn camelcase_target_matches_component_of_qualified_method() {
        let results = vec![
            result("aden://doc/pi/docs/extensions.md/h1extensions", 82.0),
            result(
                "aden://module/pi/src/agent-session.ts#AgentSession._installAgentToolHooks",
                43.0,
            ),
        ];
        let chosen = resolve_anchor_fuzzy(
            "How does AgentSession intercept extension tool calls?",
            &results,
            |_| 100,
        );
        assert_eq!(
            chosen,
            "aden://module/pi/src/agent-session.ts#AgentSession._installAgentToolHooks"
        );
    }

    #[test]
    fn all_caps_document_acronym_is_not_a_camelcase_symbol_target() {
        assert!(!has_symbolish_token(
            "What README sequence validates a request?"
        ));
        assert!(has_symbolish_token(
            "How does AgentSession validate a request?"
        ));
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

    #[test]
    fn inband_alternates_use_relative_band_and_distinct_files() {
        let results = vec![
            result_with_path("a", "generated/maps.sh", 1500.0),
            result_with_path("a-helper", "generated/maps.sh", 1475.0),
            result_with_path("b", "src/maplike.go", 1429.0),
            result_with_path("c", "src/unrelated.go", 1300.0),
        ];
        assert_eq!(inband_alternate_candidates("a", &results, 1), vec!["b"]);
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
