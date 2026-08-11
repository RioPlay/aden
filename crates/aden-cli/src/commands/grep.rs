// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Structure-aware content search.
//!
//! `aden grep` is not a grep clone — it is grep's job done with the knowledge
//! graph already in hand. Every match is tagged with the symbol it lives inside
//! (resolved from the stored span data), so the result tells you *what* the hit
//! belongs to, not just *where* the line is. The enclosing symbol name feeds
//! straight back into `aden asm --from <symbol>` to expand context — turning a
//! search hit into a graph entry point with no second tool.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::util::{discover_source_files_scoped, find_project_root};

/// A single content match, enriched with its enclosing symbol.
struct Match {
    file: String,
    line: usize,
    text: String,
    /// Short name of the enclosing symbol, if the line falls inside one.
    symbol: Option<String>,
    /// Full anchor of the enclosing symbol (for JSON / programmatic pivots).
    anchor: Option<String>,
}

/// Span of a stored symbol within a file, used to locate the enclosing symbol.
/// Shared with `impact_diff` (git-diff → enclosing-symbol resolution).
pub(crate) struct Span {
    pub(crate) anchor: String,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

pub fn cmd_grep(
    pattern: &str,
    path: &Path,
    regex: bool,
    ignore_case: bool,
    symbol_only: bool,
    limit: usize,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let root = find_project_root(path);
    let _stale_hint = super::StaleHintGuard::new(&root, json);
    // Keep the graph current so enclosing-symbol resolution is accurate.
    super::ensure_fresh(&root);

    // Build the matcher. Literal substring by default (the common case and the
    // fastest); opt into regex explicitly.
    let re = if regex {
        let built = regex::RegexBuilder::new(pattern)
            .case_insensitive(ignore_case)
            .build()
            .map_err(|e| format!("invalid regex '{}': {}", pattern, e))?;
        Some(built)
    } else {
        None
    };
    let needle_lower = pattern.to_lowercase();
    let line_matches = |line: &str| -> bool {
        if let Some(re) = &re {
            re.is_match(line)
        } else if ignore_case {
            line.to_lowercase().contains(&needle_lower)
        } else {
            line.contains(pattern)
        }
    };

    // Per-file symbol spans, from the store, for enclosing-symbol resolution.
    let spans_by_file = load_symbol_spans(&root);

    // Scope the search to PATH. A file searches just that file; a subdirectory
    // searches under it; the project root searches everything. Previously the
    // PATH argument only selected the project root for discovery, so
    // `grep <pat> some/file.rs` silently scanned the WHOLE repo (and could dump
    // megabytes when it matched minified asset lines). Symbol attribution still
    // keys off the project root, so hits keep their enclosing-symbol tags.
    let files: Vec<std::path::PathBuf> = if path.is_file() {
        vec![std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())]
    } else {
        let scope = normalized_search_scope(path, &root);
        discover_source_files_scoped(&scope, &root)?
    };
    // Count every match, but retain at most `limit` hits per file. Keeping each
    // file's earliest hits is sufficient for the final deterministic
    // file/line top-K and prevents `--limit` from allocating for every match in
    // a huge or generated corpus.
    let per_file: Vec<(usize, Vec<Match>)> = files
        .par_iter()
        .map(|file| {
            let rel = file.strip_prefix(&root).unwrap_or(file);
            let rel_str = rel.to_string_lossy().to_string();
            let content = match std::fs::read_to_string(file) {
                Ok(c) => c,
                Err(_) => return (0, Vec::new()), // binary / unreadable — skip
            };
            let spans = spans_by_file.get(&rel_str);
            let mut hits = Vec::new();
            let mut count = 0usize;
            for (i, line) in content.lines().enumerate() {
                if !line_matches(line) {
                    continue;
                }
                let line_no = i + 1;
                let enclosing = spans.and_then(|s| enclosing_symbol(s, line_no));
                if symbol_only && enclosing.is_none() {
                    continue;
                }
                count += 1;
                if hits.len() < limit {
                    hits.push(Match {
                        file: rel_str.clone(),
                        line: line_no,
                        text: line.trim().to_string(),
                        symbol: enclosing.map(|sp| short_name(&sp.anchor)),
                        anchor: enclosing.map(|sp| sp.anchor.clone()),
                    });
                }
            }
            (count, hits)
        })
        .collect();
    let total = per_file.iter().map(|(count, _)| count).sum();
    let mut matches: Vec<Match> = per_file.into_iter().flat_map(|(_, hits)| hits).collect();

    // Deterministic ordering: by file, then line; discard per-file candidates
    // that fall beyond the global top-K.
    matches.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    matches.truncate(limit);

    // A literal search that finds nothing but whose pattern carries regex
    // metacharacters is almost always a misfired regex — e.g. `Ready|ready`
    // matched literally, pipe and all, hits nothing. A human sees the empty
    // result and adjusts; an agent consuming `{"total": 0}` would wrongly
    // conclude the term is absent. Surface a hint so "zero means absent" stays
    // trustworthy. Fires only on zero results, so a successful search is never
    // cluttered and behavior never changes.
    let regex_hint = (total == 0 && !regex && looks_like_regex(pattern)).then(|| {
        format!(
            "pattern '{}' was matched literally but contains regex metacharacters; \
             retry with regex=true for alternation/classes, or ignore_case=true for case folding",
            pattern
        )
    });

    if json {
        print_json(&root, &matches, total, limit, regex_hint.as_deref());
        return Ok(());
    }

    if total == 0 {
        println!("No matches for '{}'.", pattern);
        if let Some(hint) = &regex_hint {
            println!("  ↳ hint: {hint}");
        }
        return Ok(());
    }

    println!("Found {} match(es) for '{}':", total, pattern);
    for m in matches.iter().take(limit) {
        // SECURITY: m.text is a raw line from an untrusted source file — strip
        // terminal escape sequences before printing (audit MEDIUM-3).
        let text = crate::util::sanitize_terminal(&m.text);
        match &m.symbol {
            Some(sym) => println!("{}:{}  ({}): {}", m.file, m.line, sym, text),
            None => println!("{}:{}: {}", m.file, m.line, text),
        }
    }
    if total > limit {
        println!(
            "  ... and {} more (raise --limit, or refine the pattern)",
            total - limit
        );
    }
    // Self-document the discovery→assembly loop: the enclosing symbol shown per
    // hit is exactly the anchor `asm`/`understand` take, so the agent can pivot
    // from a search hit straight to full context without a second lookup.
    if let Some(sym) = matches.iter().take(limit).find_map(|m| m.symbol.as_deref()) {
        println!("  ↳ expand a hit into full context: `asm --from {sym}` (or `understand {sym}`)");
    }
    Ok(())
}

/// Resolve a caller-provided directory before applying project-relative ignore
/// rules. CLI defaults arrive as relative `.`; passing that through unchanged
/// makes `strip_prefix(absolute_root)` fail in discovery, which disables every
/// built-in ignore and can walk enormous `target/` or `node_modules/` trees.
fn normalized_search_scope(path: &Path, root: &Path) -> PathBuf {
    if !path.is_dir() {
        return root.to_path_buf();
    }
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Load `source_file -> [span]` so each match can be attributed to its
/// enclosing symbol. The normal path streams a lightweight projection from
/// fjall; if a concurrent writer owns the store, ADR-011's immutable snapshot
/// remains the lock-free fallback.
pub(crate) fn load_symbol_spans(root: &Path) -> HashMap<String, Vec<Span>> {
    use aden_store::{GraphStorage, Storage};

    let (store_path, _) = aden_paths::resolve_read_store(root);
    let projected = store_path
        .to_str()
        .and_then(|path| Storage::open_existing(path).ok())
        .and_then(|storage| storage.get_source_spans().ok());
    let records = projected.unwrap_or_else(|| {
        aden_graph::snapshot::try_read_fresh(root)
            .map(|(docs, _)| {
                docs.into_iter()
                    .filter_map(|(anchor, document)| {
                        document.source_span.map(|span| (anchor, span))
                    })
                    .collect()
            })
            .unwrap_or_default()
    });

    let mut by_file: HashMap<String, Vec<Span>> = HashMap::new();
    for (anchor, span) in records {
        let file = Path::new(&span.file);
        let rel = file
            .strip_prefix(root)
            .unwrap_or(file)
            .to_string_lossy()
            .to_string();
        by_file.entry(rel).or_default().push(Span {
            anchor,
            start: span.start_line,
            end: span.end_line,
        });
    }
    by_file
}

/// The most specific symbol whose span contains `line` (smallest enclosing
/// span wins, so a method beats the file-level node it sits in).
pub(crate) fn enclosing_symbol(spans: &[Span], line: usize) -> Option<&Span> {
    spans
        .iter()
        .filter(|s| s.start <= line && line <= s.end)
        .min_by_key(|s| s.end.saturating_sub(s.start))
}

/// Short symbol name from a full anchor (`...#name` or trailing path segment).
fn short_name(anchor: &str) -> String {
    if let Some(pos) = anchor.rfind('#') {
        anchor[pos + 1..].to_string()
    } else {
        anchor.rsplit('/').next().unwrap_or(anchor).to_string()
    }
}

/// Emit a structured envelope rather than a bare array. The agent-facing client
/// needs the total count and an explicit `truncated` flag — the human footer
/// ("... and N more (raise --limit)") is noise to a program. `returned` is how
/// many of `total` matches are in the array after `limit` applies.
fn print_json(root: &Path, matches: &[Match], total: usize, limit: usize, hint: Option<&str>) {
    let returned = total.min(limit);
    let page: Vec<serde_json::Value> = matches
        .iter()
        .take(limit)
        .map(|m| {
            serde_json::json!({
                "file": m.file,
                "line": m.line,
                "symbol": m.symbol,
                "anchor": m.anchor,
                "text": m.text,
            })
        })
        .collect();
    let mut env = serde_json::json!({
        "total": total,
        "returned": returned,
        "truncated": total > limit,
        "matches": page,
    });
    if let Some(h) = hint {
        env["hint"] = serde_json::Value::String(h.to_string());
    }
    let env = super::augment_read_json(root, env);
    println!("{}", serde_json::to_string(&env).unwrap_or_default());
}

/// Heuristic: does a *literal* (non-regex) pattern look like it was actually
/// meant as a regex? Used only to nudge `regex=true` on a zero-result literal
/// search — a soft hint, never a behavior change. Flags the high-signal regex
/// idioms (alternation, character classes, groups, escapes, wildcards) and
/// deliberately skips bare `.`/`*`/`+`/`?`, which appear in literal code
/// searches too often (`foo.bar`, `x++`, globs) to be a reliable signal.
fn looks_like_regex(pattern: &str) -> bool {
    pattern.contains('|')
        || pattern.contains('[')
        || pattern.contains('(')
        || pattern.contains('\\')
        || pattern.contains(".*")
        || pattern.contains(".+")
}

#[cfg(test)]
mod tests {
    use super::{looks_like_regex, normalized_search_scope};
    use std::path::Path;

    #[test]
    fn flags_regex_idioms() {
        assert!(looks_like_regex("Ready|ready")); // alternation — the original miss
        assert!(looks_like_regex("[A-Z]")); // character class
        assert!(looks_like_regex("fn (foo|bar)")); // group + alternation
        assert!(looks_like_regex(r"\bword\b")); // escape
        assert!(looks_like_regex("foo.*bar")); // wildcard
    }

    #[test]
    fn ignores_plain_literals() {
        // Bare literals — including code that happens to contain `.`/`*`/`?`/`+`
        // — must NOT be flagged, or every zero-result search would nag.
        assert!(!looks_like_regex("cmd_ready"));
        assert!(!looks_like_regex("foo.bar"));
        assert!(!looks_like_regex("x++"));
        assert!(!looks_like_regex("value?"));
        assert!(!looks_like_regex("get_all_edges"));
    }

    #[test]
    fn relative_default_scope_is_normalized_under_absolute_root() {
        let root = std::fs::canonicalize(".").expect("test cwd must exist");
        assert_eq!(normalized_search_scope(Path::new("."), &root), root);
    }
}
