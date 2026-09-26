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

use crate::util::{discover_source_files_scoped, find_project_root, normalize_sep};

const MAX_PREVIEW_BYTES: usize = 512;
// Includes the JSON envelope, freshness receipt, follow-up actions and newline.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
// Each serialized match has more than 128 bytes of field names alone.
const MAX_RETAINED_MATCHES: usize = MAX_RESPONSE_BYTES / 128;

/// A single content match, enriched with its enclosing symbol.
#[derive(serde::Serialize)]
struct Match {
    file: String,
    line: usize,
    text: String,
    text_truncated: bool,
    /// All offsets are zero-based UTF-8 byte offsets within the original line.
    text_start_byte: usize,
    text_end_byte: usize,
    match_start_byte: usize,
    match_end_byte: usize,
    line_bytes: usize,
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
    // Fail before walking the repo. A literal search for `foo|bar` returns
    // zero hits and an agent treats that as "absent". Alternation/wildcard/
    // escapes must opt into `--regex`.
    if !regex && requires_regex_flag(pattern) {
        return Err(Box::new(super::AgentCliError::new(
            "needs_regex",
            format!(
                "pattern '{pattern}' looks like a regex (alternation/wildcard/escape) but was used as a literal"
            ),
            "retry with regex=true (CLI: --regex). Use a plain substring for literal search.",
        )));
    }

    // Reject an invalid scope before freshness work or discovery can broaden it.
    let scope = std::fs::canonicalize(path).map_err(|e| {
        super::AgentCliError::new(
            "invalid_scope",
            format!("cannot search '{}': {e}", path.display()),
            "pass an existing file or directory as the search path",
        )
    })?;
    if !scope.is_file() && !scope.is_dir() {
        return Err(Box::new(super::AgentCliError::new(
            "invalid_scope",
            format!(
                "search path '{}' is not a file or directory",
                path.display()
            ),
            "pass an existing file or directory as the search path",
        )));
    }
    // Git accepts directories, not files. Resolving from a file itself can fall
    // back to a nested package manifest and lose the repository's anchor keys.
    let scope_dir = if scope.is_file() {
        scope.parent().unwrap_or(&scope)
    } else {
        &scope
    };
    let root = find_project_root(scope_dir);
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
    let find_match = |line: &str| -> Option<std::ops::Range<usize>> {
        if let Some(re) = &re {
            re.find(line).map(|m| m.range())
        } else if ignore_case {
            find_case_insensitive(line, &needle_lower)
        } else {
            line.find(pattern).map(|start| start..start + pattern.len())
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
    let files: Vec<std::path::PathBuf> = if scope.is_file() {
        vec![scope.clone()]
    } else {
        let directory = normalized_search_scope(&scope, &root)?;
        discover_source_files_scoped(&directory, &root)?
    };
    // Count every match, but retain at most `limit` hits per file. Keeping each
    // file's earliest hits is sufficient for the final deterministic
    // file/line top-K and prevents `--limit` from allocating for every match in
    // a huge or generated corpus.
    let retained_limit = limit.min(MAX_RETAINED_MATCHES);
    let per_file: Vec<(usize, Vec<Match>)> = files
        .par_iter()
        .map(|file| {
            let rel_str = aden_paths::relative_to(&root, file)
                .map(|rel| normalize_sep(&rel))
                .unwrap_or_else(|| {
                    let rel = file.strip_prefix(&root).unwrap_or(file);
                    normalize_sep(rel)
                });
            let content = match std::fs::read_to_string(file) {
                Ok(c) => c,
                Err(_) => return (0, Vec::new()), // binary / unreadable — skip
            };
            let spans = spans_by_file.get(&rel_str);
            let mut hits = Vec::new();
            let mut count = 0usize;
            for (i, line) in content.lines().enumerate() {
                let Some(found) = find_match(line) else {
                    continue;
                };
                let line_no = i + 1;
                let enclosing = spans.and_then(|s| enclosing_symbol(s, line_no));
                if symbol_only && enclosing.is_none() {
                    continue;
                }
                count += 1;
                if hits.len() < retained_limit {
                    let preview = match_preview(line, &found);
                    hits.push(Match {
                        file: rel_str.clone(),
                        line: line_no,
                        text: line[preview.clone()].to_string(),
                        text_truncated: preview.start > 0 || preview.end < line.len(),
                        text_start_byte: preview.start,
                        text_end_byte: preview.end,
                        match_start_byte: found.start,
                        match_end_byte: found.end,
                        line_bytes: line.len(),
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
    matches.truncate(retained_limit);

    // A literal search that finds nothing but whose pattern carries regex
    // metacharacters is almost always a misfired regex — e.g. `Ready|ready`
    // matched literally, pipe and all, hits nothing. A human sees the empty
    // result and adjusts; an agent consuming `{"total": 0}` would wrongly
    // conclude the term is absent. Surface a hint so "zero means absent" stays
    // trustworthy. Fires only on zero results, so a successful search is never
    // cluttered and behavior never changes.
    let regex_hint = (total == 0 && !regex && looks_like_regex(pattern)).then_some(
        "pattern was matched literally but contains regex metacharacters; \
         retry with regex=true for alternation/classes, or ignore_case=true for case folding",
    );

    let mut envelope = bounded_json(&root, &matches, total, limit, regex_hint);
    let returned = envelope["returned"].as_u64().unwrap_or(0) as usize;
    let mut actions = Vec::new();
    if total == 0 || total > returned || matches.iter().take(returned).any(|m| m.text_truncated) {
        for m in matches.iter().take(returned) {
            // A clipped understand result can lead here via a file-scoped
            // search. Sending the same symbol back to understand would repeat
            // that request indefinitely for a large one-line definition.
            let action = if scope.is_file() && m.text_truncated {
                None
            } else if let Some(anchor) = &m.anchor {
                super::next_actions::inspect(
                    anchor,
                    &root,
                    "Read the enclosing symbol for this search hit",
                )
            } else if scope.is_dir() && !regex && !ignore_case && !symbol_only {
                super::next_actions::search(
                    pattern,
                    &root.join(&m.file),
                    "Narrow the search to a file containing this term",
                )
            } else {
                None
            };
            if let Some(action) = action
                && !actions.contains(&action)
            {
                actions.push(action);
            }
            if actions.len() == 3 {
                break;
            }
        }
        if actions.is_empty() {
            actions.extend(super::next_actions::tree(
                scope_dir,
                "Browse the searched directory to choose a narrower scope or different term",
            ));
        }
    }
    envelope["next_actions"] = serde_json::json!(actions);
    let envelope = cap_serialized_response(envelope, total, limit);
    let returned = envelope["returned"].as_u64().unwrap_or(0) as usize;

    if json {
        println!("{}", serde_json::to_string(&envelope)?);
        return Ok(());
    }

    if total == 0 {
        println!("No matches for '{}'.", display_pattern(pattern));
        if let Some(hint) = &regex_hint {
            println!("  ↳ hint: {hint}");
        }
        super::next_actions::print(envelope["next_actions"].as_array().unwrap());
        return Ok(());
    }

    println!(
        "Found {} match(es) for '{}':",
        total,
        display_pattern(pattern)
    );
    for m in matches.iter().take(returned) {
        // SECURITY: m.text is a raw line from an untrusted source file — strip
        // terminal escape sequences before printing (audit MEDIUM-3).
        let text = format!(
            "{}{}{}",
            if m.text_start_byte > 0 { "…" } else { "" },
            crate::util::sanitize_terminal(&m.text),
            if m.text_end_byte < m.line_bytes {
                "…"
            } else {
                ""
            }
        );
        let file = crate::util::sanitize_terminal(&m.file);
        match &m.symbol {
            Some(sym) => println!(
                "{}:{}  ({}): {}",
                file,
                m.line,
                crate::util::sanitize_terminal(sym),
                text
            ),
            None => println!("{}:{}: {}", file, m.line, text),
        }
    }
    if total > returned {
        println!(
            "  ... and {} more (refine the pattern or narrow the search path; output is byte-bounded)",
            total - returned
        );
    }
    // Self-document the discovery→assembly loop: the enclosing symbol shown per
    // hit is exactly the anchor `asm`/`understand` take, so the agent can pivot
    // from a search hit straight to full context without a second lookup.
    if let Some(sym) = matches
        .iter()
        .take(returned)
        .find_map(|m| m.symbol.as_deref())
    {
        println!("  ↳ expand a hit into full context: `asm --from {sym}` (or `understand {sym}`)");
    }
    super::next_actions::print(envelope["next_actions"].as_array().unwrap());
    Ok(())
}

/// Resolve a caller-provided directory before applying project-relative ignore
/// rules. CLI defaults arrive as relative `.`; passing that through unchanged
/// makes `strip_prefix(absolute_root)` fail in discovery, which disables every
/// built-in ignore and can walk enormous `target/` or `node_modules/` trees.
///
/// Prefer a path that sits under `root` after Windows verbatim normalization so
/// `relative_to` / ignore rules keep working even when canonicalize returns
/// `\\?\…` while the project root came from git as `C:/…`.
fn normalized_search_scope(path: &Path, root: &Path) -> std::io::Result<PathBuf> {
    if !path.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "search directory does not exist",
        ));
    }
    let canon = std::fs::canonicalize(path)?;
    if aden_paths::is_under(root, &canon) {
        // Walk from the plain project root when the scope *is* the root so
        // child paths from read_dir share the root's spelling and strip cleanly.
        if aden_paths::relative_to(root, &canon)
            .map(|rel| rel.as_os_str().is_empty())
            .unwrap_or(false)
        {
            return std::fs::canonicalize(root);
        }
        return Ok(canon);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "search directory is outside the project root",
    ))
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
        // Forward-slash relative keys. Prefer aden_paths::relative_to so Windows
        // absolute/`\\?\` stored paths still join tree/grep lookups that use
        // project-relative `/`-normalized keys.
        let rel = if file.is_absolute() {
            aden_paths::relative_to(root, file)
                .map(|r| normalize_sep(&r))
                .unwrap_or_else(|| normalize_sep(file.strip_prefix(root).unwrap_or(file)))
        } else {
            normalize_sep(file)
        };
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

/// Preserve lowercase-substring matching while mapping Unicode expansions
/// (for example İ → i + combining dot) back to valid original byte offsets.
fn find_case_insensitive(line: &str, needle_lower: &str) -> Option<std::ops::Range<usize>> {
    let lowered = line.to_lowercase();
    let start = lowered.find(needle_lower)?;
    if needle_lower.is_empty() {
        return Some(0..0);
    }
    let end = start + needle_lower.len();
    let mut lower_offset = 0;
    let mut original_start = None;
    for (offset, ch) in line.char_indices() {
        let next = lower_offset + ch.to_lowercase().map(char::len_utf8).sum::<usize>();
        if original_start.is_none() && start < next {
            original_start = Some(offset);
        }
        if end <= next {
            return Some(original_start.unwrap_or(offset)..offset + ch.len_utf8());
        }
        lower_offset = next;
    }
    None
}

/// A byte-bounded window around the first match. Long matches keep their start;
/// exact match offsets still describe the entire match even when it is clipped.
fn match_preview(line: &str, found: &std::ops::Range<usize>) -> std::ops::Range<usize> {
    if line.len() <= MAX_PREVIEW_BYTES {
        return 0..line.len();
    }
    let context = MAX_PREVIEW_BYTES
        .saturating_sub(found.len())
        .min(MAX_PREVIEW_BYTES / 2);
    let mut start = found.start.saturating_sub(context / 2);
    let mut end = (start + MAX_PREVIEW_BYTES).min(line.len());
    start = end.saturating_sub(MAX_PREVIEW_BYTES);
    while !line.is_char_boundary(start) {
        start += 1;
    }
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    start..end
}

fn display_pattern(pattern: &str) -> String {
    let preview = match_preview(pattern, &(0..pattern.len()));
    let mut text = crate::util::sanitize_terminal(&pattern[preview]);
    if pattern.len() > MAX_PREVIEW_BYTES {
        text.push('…');
    }
    text
}

/// Budget the actual serialized response, not just preview lengths: paths,
/// anchors, escaped control characters and freshness all consume context too.
fn bounded_json(
    root: &Path,
    matches: &[Match],
    total: usize,
    limit: usize,
    hint: Option<&str>,
) -> serde_json::Value {
    let page: Vec<_> = matches
        .iter()
        .map(|m| serde_json::to_value(m).unwrap())
        .collect();
    let mut env = serde_json::json!({
        "total": total,
        "matches": page,
        "limit": limit,
        "max_response_bytes": MAX_RESPONSE_BYTES,
        "max_preview_bytes": MAX_PREVIEW_BYTES,
        "offset_encoding": "zero-based UTF-8 bytes within the original line; end offsets are exclusive",
        "next_actions": [],
    });
    if let Some(h) = hint {
        env["hint"] = serde_json::Value::String(h.to_string());
    }
    let env = super::augment_read_json(root, env);
    cap_serialized_response(env, total, limit)
}

fn cap_serialized_response(
    mut env: serde_json::Value,
    total: usize,
    limit: usize,
) -> serde_json::Value {
    loop {
        let page = env["matches"].as_array().unwrap();
        let returned = page.len();
        let text_truncated = page.iter().any(|m| m["text_truncated"] == true);
        env["returned"] = returned.into();
        env["truncated"] = (returned < total).into();
        env["text_truncated"] = text_truncated.into();
        env["limit_truncated"] = (total > limit).into();
        env["response_bytes_truncated"] = (returned < total.min(limit)).into();
        if serde_json::to_vec(&env).unwrap().len() < MAX_RESPONSE_BYTES {
            return env;
        }
        if env["matches"].as_array_mut().unwrap().pop().is_none() {
            // Normally only match rows can exhaust the budget. Keep even a
            // malformed, oversized freshness receipt from escaping the bound.
            if env["next_actions"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
            {
                env["next_actions"] = serde_json::json!([]);
            } else {
                return serde_json::json!({
                    "matches": [],
                    "total": total,
                    "returned": 0,
                    "truncated": total > 0,
                    "text_truncated": false,
                    "limit_truncated": total > limit,
                    "response_bytes_truncated": total.min(limit) > 0,
                    "metadata_truncated": true,
                    "max_response_bytes": MAX_RESPONSE_BYTES,
                    "next_actions": [],
                });
            }
        }
    }
}

/// Strong regex intent: searching these as a literal is almost never useful
/// and an empty result is a lie. Weaker signals (`(`, `[`) stay a zero-hit
/// hint so `foo(` / `items[0]` still search.
fn requires_regex_flag(pattern: &str) -> bool {
    pattern.contains('|')
        || pattern.contains('\\')
        || pattern.contains(".*")
        || pattern.contains(".+")
}

/// Heuristic: does a *literal* (non-regex) pattern look like it was actually
/// meant as a regex? Used only to nudge `regex=true` on a zero-result literal
/// search — a soft hint, never a behavior change. Flags the high-signal regex
/// idioms (alternation, character classes, groups, escapes, wildcards) and
/// deliberately skips bare `.`/`*`/`+`/`?`, which appear in literal code
/// searches too often (`foo.bar`, `x++`, globs) to be a reliable signal.
fn looks_like_regex(pattern: &str) -> bool {
    requires_regex_flag(pattern) || pattern.contains('[') || pattern.contains('(')
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_PREVIEW_BYTES, MAX_RESPONSE_BYTES, cap_serialized_response, find_case_insensitive,
        looks_like_regex, match_preview, normalized_search_scope, requires_regex_flag,
    };
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
    fn strong_regex_intent_requires_the_flag() {
        assert!(requires_regex_flag("Ready|ready"));
        assert!(requires_regex_flag("foo.*bar"));
        assert!(requires_regex_flag(r"\bword\b"));
        assert!(!requires_regex_flag("foo("));
        assert!(!requires_regex_flag("items[0]"));
        assert!(!requires_regex_flag("foo.bar"));
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
        assert_eq!(
            normalized_search_scope(Path::new("."), &root).unwrap(),
            root
        );
    }

    #[test]
    fn preview_keeps_match_and_unicode_byte_boundaries() {
        let line = format!("{}needle{}", "🌱".repeat(500), "文".repeat(500));
        let found = line.find("needle").unwrap();
        let preview = match_preview(&line, &(found..found + 6));
        assert!(preview.len() <= MAX_PREVIEW_BYTES);
        assert!(preview.start > 0);
        assert!(preview.end < line.len());
        assert!(line[preview].contains("needle"));
    }

    #[test]
    fn lowercase_expansion_maps_back_to_original_offsets() {
        let line = "🌱İSTANBUL";
        assert_eq!(find_case_insensitive(line, "i"), Some(4..6));
        assert_eq!(find_case_insensitive(line, "i\u{307}stanbul"), Some(4..13));
        assert_eq!(find_case_insensitive(line, "istanbul"), None);
        assert_eq!(find_case_insensitive(line, ""), Some(0..0));
    }

    #[test]
    fn serialized_budget_counts_escaped_paths_and_oversized_identities() {
        let envelope = serde_json::json!({
            "matches": [
                {"file": "short.rs", "text": "needle", "text_truncated": false},
                {"file": "\"\\\n".repeat(MAX_RESPONSE_BYTES), "text": "needle", "text_truncated": false}
            ],
            "next_actions": [],
            "total": 2,
        });
        let envelope = cap_serialized_response(envelope, 2, 50);
        assert!(serde_json::to_vec(&envelope).unwrap().len() < MAX_RESPONSE_BYTES);
        assert_eq!(envelope["returned"], 1);
        assert_eq!(envelope["total"], 2);
        assert_eq!(envelope["truncated"], true);
        assert_eq!(envelope["response_bytes_truncated"], true);
        assert_eq!(envelope["limit_truncated"], false);
    }
}
