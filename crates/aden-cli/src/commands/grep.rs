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

use std::cell::LazyCell;
use std::collections::{BTreeSet, HashMap};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use rayon::prelude::*;

use crate::util::{discover_source_files_scoped, find_project_root, normalize_sep};

const MAX_PREVIEW_BYTES: usize = 512;
// Includes the JSON envelope, freshness receipt, follow-up actions and newline.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
// Each serialized match has more than 128 bytes of field names alone.
const MAX_RETAINED_MATCHES: usize = MAX_RESPONSE_BYTES / 128;
const GREP_SCAN_BUFFER_BYTES: usize = 64 * 1024;

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
    // Load the span projection only if an eligible match needs attribution.
    // Workers share the first lookup. Symbol-only counts still need spans.
    let spans_by_file = LazyLock::new(|| load_symbol_spans(&root));
    let relative_root = aden_paths::RelativeRoot::new(&root);
    // Retain candidates for at most one parallel file batch at a time. Each
    // batch keeps up to one page per worker, then folds that bounded set into
    // the globally ordered top-K. This avoids allocating previews for every
    // file in a large corpus while keeping file reads parallel.
    let retained_limit = limit.min(MAX_RETAINED_MATCHES);
    let batch_size = rayon::current_num_threads().max(1);
    let mut total = 0usize;
    let mut matches: Vec<Match> = Vec::new();
    for file_batch in files.chunks(batch_size) {
        // Once the page is full, its last key is an immutable cutoff for this batch.
        // Later paths, and same-path lines at/after the cutoff, cannot enter
        // the stable global top-K.
        let cutoff = if retained_limit > 0 && matches.len() == retained_limit {
            matches.last().map(|hit| (hit.file.as_str(), hit.line))
        } else {
            None
        };
        let per_file: Vec<(usize, Vec<Match>)> = file_batch
            .par_iter()
            .map(|file| {
                let rel_str = relative_root
                    .relative_to(file)
                    .map(|rel| normalize_sep(&rel))
                    .unwrap_or_else(|| {
                        let rel = file.strip_prefix(&root).unwrap_or(file);
                        normalize_sep(rel)
                    });
                let last_eligible_line = match cutoff {
                    Some((file, _)) if rel_str.as_str() > file => 0,
                    Some((file, line)) if rel_str == file => line - 1,
                    _ => usize::MAX,
                };
                let mut hits = Vec::new();
                let mut count = 0usize;
                let spans_for_file = LazyCell::new(|| spans_by_file.get(&rel_str));
                let file_handle = match std::fs::File::open(file) {
                    Ok(file) => file,
                    Err(_) => return (0, Vec::new()),
                };
                let mut reader = BufReader::with_capacity(GREP_SCAN_BUFFER_BYTES, file_handle);
                let literal_skip =
                    (!regex && !ignore_case && !pattern.is_empty()).then_some(pattern);
                let result = scan_utf8_lines(&mut reader, literal_skip, |line, line_no| {
                    let Some(found) = find_match(line) else {
                        return;
                    };
                    let keep = line_no <= last_eligible_line && hits.len() < retained_limit;
                    let enclosing = if symbol_only {
                        let Some(span) =
                            spans_for_file.and_then(|spans| enclosing_symbol(spans, line_no))
                        else {
                            return;
                        };
                        Some(span)
                    } else {
                        None
                    };
                    count += 1;
                    if keep {
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
                });
                if result.is_err() {
                    // Match read_to_string: invalid UTF-8 or a read error drops
                    // all earlier hits and counts for this file.
                    return (0, Vec::new());
                }
                (count, hits)
            })
            .collect();
        let previous_len = matches.len();
        for (count, hits) in per_file {
            total += count;
            matches.extend(hits);
        }
        if matches.len() != previous_len {
            matches.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
            matches.truncate(retained_limit);
        }
    }

    // Ordinary hits need attribution only after file validation and global top-K.
    // Symbol-only mode already resolved spans while scanning. A normal search
    // normalizes spans only for files that produced a retained hit.
    if !symbol_only {
        let mut files = BTreeSet::new();
        for hit in &matches {
            files.insert(hit.file.clone());
        }
        let spans_for_hits = load_symbol_spans_for_files(&root, &files);
        for hit in &mut matches {
            if let Some(span) = spans_for_hits
                .get(&hit.file)
                .and_then(|spans| enclosing_symbol(spans, hit.line))
            {
                hit.symbol = Some(short_name(&span.anchor));
                hit.anchor = Some(span.anchor.clone());
            }
        }
    }

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
    // Choose from source evidence already on this page. Never execute a predicted call.
    let visible = &matches[..returned];
    if !regex && !ignore_case && is_identifier(pattern) {
        let exact = visible
            .iter()
            .filter(|hit| hit.symbol.as_deref() == Some(pattern));
        let mut anchors = exact.clone().filter_map(|hit| hit.anchor.as_deref());
        if let Some(anchor) = anchors.next() {
            let needs_resolution = returned != total
                || exact.clone().any(|hit| hit.text_truncated)
                || anchors.any(|other| other != anchor);
            let action = if needs_resolution {
                super::next_actions::locate_symbol(
                    pattern,
                    &root,
                    "Resolve the matching definitions before choosing callers",
                )
            } else {
                super::next_actions::backlinks(
                    anchor,
                    &root,
                    "Find direct callers and references to this symbol",
                )
            };
            actions.extend(action);
        } else if scope.is_file()
            && visible
                .iter()
                .any(|hit| !hit.text_truncated && looks_like_call(hit, pattern))
        {
            // A file-scoped search may show only an invocation. Its enclosing
            // anchor belongs to the caller, so resolve the searched name first.
            actions.extend(super::next_actions::locate_symbol(
                pattern,
                &root,
                "Resolve this name at project scope before following its references",
            ));
        }
    }
    if actions.is_empty()
        && returned == total
        && let Some(anchor) = visible.first().and_then(|hit| hit.anchor.as_deref())
        && visible
            .iter()
            .all(|hit| !hit.text_truncated && hit.anchor.as_deref() == Some(anchor))
    {
        actions.extend(super::next_actions::inspect(
            anchor,
            &root,
            "Inspect the source and relationships of the enclosing symbol",
        ));
    }
    if actions.is_empty()
        && (total == 0
            || total > returned
            || matches.iter().take(returned).any(|m| m.text_truncated))
    {
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
        crate::read_worker::emit_machine_body(serde_json::to_string(&envelope)?);
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
    if envelope["next_actions"]
        .as_array()
        .is_some_and(Vec::is_empty)
        && let Some(sym) = matches
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
/// enclosing symbol. A revision-checked `graph.snapshot` wins when the read
/// store is not pinned or legacy. Otherwise this streams the store's source-span projection
/// and, if a writer holds that store, ADR-011's snapshot.
pub(crate) fn load_symbol_spans(root: &Path) -> HashMap<String, Vec<Span>> {
    load_symbol_spans_for(root, None)
}

pub(super) fn load_symbol_spans_for(
    root: &Path,
    anchors: Option<&BTreeSet<String>>,
) -> HashMap<String, Vec<Span>> {
    group_source_spans(root, load_span_records(root, anchors))
}

/// Spans for files that already produced hits. Other stored paths are not
/// canonicalized: a repository-sized snapshot otherwise normalizes every node
/// for a single page of matches.
fn load_symbol_spans_for_files(
    root: &Path,
    files: &BTreeSet<String>,
) -> HashMap<String, Vec<Span>> {
    if files.is_empty() {
        return HashMap::new();
    }
    let records = load_span_records(root, None)
        .into_iter()
        .filter(|(_, span)| span_file_matches_hits(&span.file, files));
    group_source_spans(root, records)
}

const SNAPSHOT_SPAN_BYTES: u64 = 16 * 1024 * 1024;

enum VerifiedSnapshot {
    Records(Vec<(String, aden_core::SourceSpan)>),
    FallbackAllowed,
    RevisionRejected,
}

fn load_span_records(
    root: &Path,
    anchors: Option<&BTreeSet<String>>,
) -> Vec<(String, aden_core::SourceSpan)> {
    let _trace = crate::read_trace::span(crate::read_trace::Stage::SourceSpans);
    let snapshot_first =
        std::env::var_os("ADEN_STORE").is_none() && !aden_paths::resolve_read_store(root).1;
    let allow_unchecked_snapshot = if snapshot_first {
        match read_verified_snapshot_spans(root, anchors) {
            VerifiedSnapshot::Records(records) => return records,
            VerifiedSnapshot::FallbackAllowed => true,
            VerifiedSnapshot::RevisionRejected => false,
        }
    } else {
        true
    };
    read_store_or_fresh_spans(root, anchors, allow_unchecked_snapshot)
}

fn read_verified_snapshot_spans(
    root: &Path,
    anchors: Option<&BTreeSet<String>>,
) -> VerifiedSnapshot {
    let Some(expected) = crate::indexer::fresh::published_revision(root) else {
        return VerifiedSnapshot::FallbackAllowed;
    };
    let Some(bytes) =
        read_bounded_regular_file(&aden_paths::graph_snapshot_file(root), SNAPSHOT_SPAN_BYTES)
    else {
        return VerifiedSnapshot::FallbackAllowed;
    };
    if !crate::indexer::fresh::snapshot_matches_revision(&bytes, &expected) {
        return VerifiedSnapshot::RevisionRejected;
    }
    let Ok((docs, _)) = aden_graph::snapshot::decode_snapshot(&bytes) else {
        return VerifiedSnapshot::RevisionRejected;
    };
    if docs.is_empty() {
        return VerifiedSnapshot::RevisionRejected;
    }
    VerifiedSnapshot::Records(sorted_span_records(span_records_from_docs(docs, anchors)))
}

fn read_bounded_regular_file(path: &Path, limit: u64) -> Option<Vec<u8>> {
    if !std::fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    let length = metadata.len();
    if length > limit {
        return None;
    }
    let mut bytes = Vec::new();
    let mut limited = std::io::Read::take(file, limit + 1);
    std::io::Read::read_to_end(&mut limited, &mut bytes).ok()?;
    (bytes.len() as u64 == length).then_some(bytes)
}

fn read_store_or_fresh_spans(
    root: &Path,
    anchors: Option<&BTreeSet<String>>,
    allow_unchecked_snapshot: bool,
) -> Vec<(String, aden_core::SourceSpan)> {
    use aden_store::{GraphStorage, Storage};

    let (store_path, _) = aden_paths::resolve_read_store(root);
    let projected = store_path
        .to_str()
        .and_then(|path| Storage::open_existing(path).ok())
        .and_then(|storage| match anchors {
            Some(keys) => storage
                .get_source_spans_for(keys.iter().map(String::as_str))
                .ok(),
            None => storage.get_source_spans().ok(),
        });
    projected.unwrap_or_else(|| {
        if !allow_unchecked_snapshot {
            return Vec::new();
        }
        aden_graph::snapshot::try_read_fresh(root)
            .map(|(docs, _)| sorted_span_records(span_records_from_docs(docs, anchors)))
            .unwrap_or_default()
    })
}

fn span_records_from_docs(
    docs: HashMap<String, aden_core::Document>,
    anchors: Option<&BTreeSet<String>>,
) -> Vec<(String, aden_core::SourceSpan)> {
    docs.into_iter()
        .filter(|(anchor, _)| anchors.is_none_or(|keys| keys.contains(anchor)))
        .filter_map(|(anchor, document)| {
            document
                .source_span
                .or_else(|| aden_core::SourceSpan::from_attributes(&document.attributes))
                .map(|span| (anchor, span))
        })
        .collect()
}

fn sorted_span_records(
    mut records: Vec<(String, aden_core::SourceSpan)>,
) -> Vec<(String, aden_core::SourceSpan)> {
    records.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    records
}

/// True when `file` can normalize to one of `files` without canonicalizing
/// every absolute path in the snapshot. Relative keys compare directly.
/// Absolute paths must end at a path boundary with a wanted key.
fn span_file_matches_hits(file: &str, files: &BTreeSet<String>) -> bool {
    let mut normalized = normalize_sep(Path::new(file));
    while let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_string();
    }
    if files.contains(&normalized) {
        return true;
    }
    if normalized == "."
        || normalized.contains("/./")
        || normalized.contains("/../")
        || normalized.starts_with("../")
        || normalized.ends_with("/.")
        || normalized.ends_with("/..")
    {
        return true;
    }
    if !Path::new(file).is_absolute() {
        return false;
    }
    files
        .iter()
        .any(|key| path_has_relative_suffix(&normalized, key))
}

fn path_has_relative_suffix(normalized: &str, key: &str) -> bool {
    let Some(start) = normalized.len().checked_sub(key.len()) else {
        return false;
    };
    if start == 0 || normalized.as_bytes()[start - 1] != b'/' {
        return false;
    }
    let tail = &normalized[start..];
    #[cfg(windows)]
    {
        tail.eq_ignore_ascii_case(key)
    }
    #[cfg(not(windows))]
    {
        tail == key
    }
}

pub(crate) fn group_source_spans(
    root: &Path,
    records: impl IntoIterator<Item = (String, aden_core::SourceSpan)>,
) -> HashMap<String, Vec<Span>> {
    let mut by_file: HashMap<String, Vec<Span>> = HashMap::new();
    let mut relative_root = None;
    let mut relative_by_raw_file = HashMap::new();
    for (anchor, span) in records {
        let rel = relative_by_raw_file
            .entry(span.file)
            .or_insert_with_key(|file| {
                normalize_span_file(root, Path::new(file), &mut relative_root)
            });
        let record = Span {
            anchor,
            start: span.start_line,
            end: span.end_line,
        };
        if let Some(spans) = by_file.get_mut(rel.as_str()) {
            spans.push(record);
        } else {
            by_file.insert(rel.clone(), vec![record]);
        }
    }
    by_file
}

/// Normalize a stored span path to the same forward-slash key used by source
/// lookups. The root is prepared lazily so relative-only span sets do no root
/// canonicalization, and callers processing batches can reuse it.
fn normalize_span_file(
    root: &Path,
    file: &Path,
    relative_root: &mut Option<aden_paths::RelativeRoot>,
) -> String {
    if !file.is_absolute() {
        return normalize_sep(file);
    }

    let normalized_root = relative_root.get_or_insert_with(|| aden_paths::RelativeRoot::new(root));
    normalized_root
        .relative_to(file)
        .map(|relative| normalize_sep(&relative))
        .unwrap_or_else(|| normalize_sep(file.strip_prefix(root).unwrap_or(file)))
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

/// Visit UTF-8 lines without retaining a complete file. Complete lines in the
/// reader buffer are validated and scanned in one borrowed chunk; a line that
/// crosses a buffer boundary is accumulated in one reusable allocation.
fn scan_utf8_lines<R: BufRead>(
    reader: &mut R,
    skip_literal: Option<&str>,
    mut visit: impl FnMut(&str, usize),
) -> io::Result<()> {
    let mut line_number = 1usize;
    let mut remainder = Vec::new();

    loop {
        let buffered = match reader.fill_buf() {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if buffered.is_empty() {
            return Ok(());
        }

        let (bytes, consumed) = if let Some(end) = buffered.iter().rposition(|&b| b == b'\n') {
            (&buffered[..=end], end + 1)
        } else {
            // read_until consumes the buffered prefix and retries interrupted reads.
            remainder.clear();
            reader.read_until(b'\n', &mut remainder)?;
            (remainder.as_slice(), 0)
        };
        let text = std::str::from_utf8(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if skip_literal.is_some_and(|literal| !text.contains(literal)) {
            line_number += count_lines_like_str(text.as_bytes());
        } else {
            for line in text.lines() {
                visit(line, line_number);
                line_number += 1;
            }
        }
        reader.consume(consumed);
    }
}

/// `str::lines().count()` without building line slices. `\n` terminates,
/// including the `\n` of `\r\n`. A trailing newline does not add a line, and a
/// non-empty chunk with no trailing newline still counts its last line.
fn count_lines_like_str(bytes: &[u8]) -> usize {
    let breaks = memchr::memchr_iter(b'\n', bytes).count();
    breaks + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"))
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
    let page = env["matches"].as_array().unwrap();
    let mut returned = page.len();
    let mut truncated_rows = page.iter().filter(|m| m["text_truncated"] == true).count();
    let set_metadata = |env: &mut serde_json::Value, returned: usize, text_truncated: bool| {
        env["returned"] = returned.into();
        env["truncated"] = (returned < total).into();
        env["text_truncated"] = text_truncated.into();
        env["limit_truncated"] = (total > limit).into();
        env["response_bytes_truncated"] = (returned < total.min(limit)).into();

        // Only these four values change while trimming: decimal digits for
        // returned, and three JSON booleans (false is five bytes, true four).
        let digits = returned.checked_ilog10().unwrap_or(0) as usize + 1;
        digits + 15
            - usize::from(returned < total)
            - usize::from(text_truncated)
            - usize::from(returned < total.min(limit))
    };
    let mut metadata_bytes = set_metadata(&mut env, returned, truncated_rows > 0);
    let mut scratch = serde_json::to_vec(&env).unwrap();
    let mut response_bytes = scratch.len();
    loop {
        // Reserve the byte printed as the trailing newline.
        if response_bytes < MAX_RESPONSE_BYTES {
            return env;
        }
        if let Some(row) = env["matches"].as_array_mut().unwrap().pop() {
            returned -= 1;
            truncated_rows -= usize::from(row["text_truncated"] == true);
            let next_metadata_bytes = set_metadata(&mut env, returned, truncated_rows > 0);
            // Removing a tail row removes its exact escaped JSON bytes and
            // one comma if another row remains. The array brackets stay.
            scratch.clear();
            serde_json::to_writer(&mut scratch, &row).unwrap();
            response_bytes =
                response_bytes - scratch.len() - usize::from(returned > 0) - metadata_bytes
                    + next_metadata_bytes;
            metadata_bytes = next_metadata_bytes;
        } else {
            // Normally only match rows can exhaust the budget. Keep even a
            // malformed, oversized freshness receipt from escaping the bound.
            if env["next_actions"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
            {
                env["next_actions"] = serde_json::json!([]);
                scratch.clear();
                serde_json::to_writer(&mut scratch, &env).unwrap();
                response_bytes = scratch.len();
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

fn is_identifier(pattern: &str) -> bool {
    let mut chars = pattern.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

/// Cheap lexical clue only. A locate request resolves the name in the graph.
fn looks_like_call(hit: &Match, pattern: &str) -> bool {
    let start = hit.match_start_byte - hit.text_start_byte;
    let Some((prefix, suffix)) = hit.text.split_at_checked(start) else {
        return false;
    };
    (start != 0 || hit.text_start_byte == 0)
        && !prefix
            .chars()
            .next_back()
            .is_some_and(|c| c == '_' || c.is_alphanumeric())
        && suffix
            .strip_prefix(pattern)
            .is_some_and(|rest| rest.trim_start().starts_with('('))
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
        MAX_PREVIEW_BYTES, MAX_RESPONSE_BYTES, cap_serialized_response, count_lines_like_str,
        find_case_insensitive, is_identifier, looks_like_regex, match_preview, normalize_span_file,
        normalized_search_scope, requires_regex_flag, scan_utf8_lines,
    };
    use std::io::BufReader;
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
    fn exact_symbol_pivots_require_plain_identifiers() {
        assert!(is_identifier("recallableComposerPrompt"));
        assert!(is_identifier("_private2"));
        assert!(is_identifier("éclair"));
        for value in ["", "foo()", "foo::bar", "a-b"] {
            assert!(!is_identifier(value), "{value:?}");
        }
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
    fn chunk_scanner_matches_str_lines_across_boundaries() {
        let text = "first\r\n\n🌱 value\r\nlast\r";
        let expected: Vec<_> = text
            .lines()
            .enumerate()
            .map(|(index, line)| (line.to_string(), index + 1))
            .collect();
        for capacity in [1, 2, 3, 8] {
            let mut reader = BufReader::with_capacity(capacity, std::io::Cursor::new(text));
            let mut actual = Vec::new();
            scan_utf8_lines(&mut reader, None, |line, line_number| {
                actual.push((line.to_string(), line_number));
            })
            .unwrap();
            assert_eq!(actual, expected, "buffer capacity {capacity}");
        }
    }

    #[test]
    fn skipped_chunks_count_lf_crlf_empty_and_unterminated_lines() {
        let samples = [
            "alpha\nbeta\ngamma\n",
            "alpha\r\nbeta\r\ngamma\r\n",
            "alpha\n\n\nbeta\n",
            "alpha\nbeta\ngamma",
            "\n",
            "",
            "only",
            "a\r\n\r\nb",
            "line1\nline2\r\n\nline4",
            "\r\n",
            "a\n\nb",
        ];
        for text in samples {
            assert_eq!(
                count_lines_like_str(text.as_bytes()),
                text.lines().count(),
                "{text:?}"
            );
            let marked = if text.is_empty() || text.ends_with('\n') {
                format!("{text}zzz-marker")
            } else {
                format!("{text}\nzzz-marker")
            };
            let expected: Vec<_> = marked
                .lines()
                .enumerate()
                .filter_map(|(index, line)| line.contains("zzz-marker").then_some(index + 1))
                .collect();
            for capacity in [1usize, 2, 3, 4, 7, 64] {
                let mut reader =
                    BufReader::with_capacity(capacity, std::io::Cursor::new(marked.as_str()));
                let mut lines = Vec::new();
                scan_utf8_lines(&mut reader, Some("zzz-marker"), |line, number| {
                    if line.contains("zzz-marker") {
                        lines.push(number);
                    }
                })
                .unwrap();
                assert_eq!(lines, expected, "text {text:?} capacity {capacity}");
            }
        }
    }

    #[test]
    fn chunk_scanner_skips_nonmatching_chunks_and_rejects_late_invalid_utf8() {
        let text = "zero\nneedle\n中\nneedle2";
        let mut reader = BufReader::with_capacity(2, std::io::Cursor::new(text));
        let mut matches = Vec::new();
        scan_utf8_lines(&mut reader, Some("needle"), |line, line_number| {
            matches.push((line.to_string(), line_number));
        })
        .unwrap();
        assert_eq!(
            matches,
            [("needle".to_string(), 2), ("needle2".to_string(), 4)]
        );

        let bytes = b"valid\ninvalid\xff\n";
        let mut reader = BufReader::with_capacity(3, std::io::Cursor::new(bytes));
        let mut visited = Vec::new();
        assert!(
            scan_utf8_lines(&mut reader, None, |line, line_number| {
                visited.push((line.to_string(), line_number));
            })
            .is_err()
        );
        assert_eq!(visited, [("valid".to_string(), 1)]);
    }

    #[test]
    fn span_path_normalization_reuses_root_and_preserves_fallbacks() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("src/file.rs");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "fn f() {}\n").unwrap();
        let alias = root.path().join("src/../src/file.rs");
        let mut normalized_root = None;

        assert_eq!(
            normalize_span_file(root.path(), Path::new("src/file.rs"), &mut normalized_root),
            "src/file.rs"
        );
        assert!(
            normalized_root.is_none(),
            "relative paths need no root resolution"
        );
        assert_eq!(
            normalize_span_file(root.path(), &source, &mut normalized_root),
            normalize_span_file(root.path(), &alias, &mut normalized_root)
        );

        let missing_inside = root.path().join("src/missing.rs");
        assert_eq!(
            normalize_span_file(root.path(), &missing_inside, &mut normalized_root),
            "src/missing.rs"
        );
        let outside = root.path().parent().unwrap().join("outside-missing.rs");
        assert_eq!(
            normalize_span_file(root.path(), &outside, &mut normalized_root),
            crate::util::normalize_sep(&outside)
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

    #[test]
    fn serialized_budget_matches_tail_trimming_at_exact_boundaries() {
        // Reference the original policy without its incremental accounting.
        // These fixtures always fit after removing at most two tail rows.
        fn reference(mut env: serde_json::Value, total: usize, limit: usize) -> serde_json::Value {
            loop {
                let page = env["matches"].as_array().unwrap();
                let returned = page.len();
                let text_truncated = page.iter().any(|row| row["text_truncated"] == true);
                env["returned"] = returned.into();
                env["truncated"] = (returned < total).into();
                env["text_truncated"] = text_truncated.into();
                env["limit_truncated"] = (total > limit).into();
                env["response_bytes_truncated"] = (returned < total.min(limit)).into();
                if serde_json::to_vec(&env).unwrap().len() < MAX_RESPONSE_BYTES {
                    return env;
                }
                assert!(env["matches"].as_array_mut().unwrap().pop().is_some());
            }
        }

        for retained in [1, 9, 10, 99, 100] {
            for empty_rows in [false, true] {
                for with_metadata in [false, true] {
                    let count = retained + 1;
                    let total = count + if with_metadata { 7 } else { 0 };
                    let rows: Vec<_> = (0..count)
                        .map(|index| {
                            if empty_rows {
                                serde_json::json!({})
                            } else {
                                serde_json::json!({
                                    "file": format!("文\\\"{index}\n.rs"),
                                    "text": "🌱\t\"\\needle",
                                    "text_truncated": index == count - 1,
                                })
                            }
                        })
                        .collect();
                    let mut env = serde_json::json!({
                        "matches": rows,
                        "total": total,
                        "padding": "",
                    });
                    if with_metadata {
                        env["receipt"] = serde_json::json!({"identity": "文🌱\n\\\""});
                        env["next_actions"] = serde_json::json!([{"query": "文🌱\t\\\""}]);
                    }
                    let mut prefix = env.clone();
                    prefix["matches"].as_array_mut().unwrap().truncate(retained);
                    let prefix = reference(prefix, total, count);
                    let prefix_bytes = serde_json::to_vec(&prefix).unwrap().len();
                    for extra in 0..3 {
                        env["padding"] = "p"
                            .repeat(MAX_RESPONSE_BYTES - 2 - prefix_bytes + extra)
                            .into();
                        let expected = reference(env.clone(), total, count);
                        let actual = cap_serialized_response(env.clone(), total, count);
                        assert_eq!(actual, expected);
                        let actual_bytes = serde_json::to_vec(&actual).unwrap().len();
                        assert!(actual_bytes < MAX_RESPONSE_BYTES);
                        if extra == 1 {
                            assert_eq!(actual["returned"], retained);
                            assert_eq!(actual_bytes + 1, MAX_RESPONSE_BYTES);
                        } else if extra == 2 {
                            assert_eq!(actual["returned"], retained - 1);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn serialized_budget_recovers_from_oversized_actions_and_receipts() {
        for oversized_receipt in [false, true] {
            let mut env = serde_json::json!({
                "matches": [{"text": "needle", "text_truncated": true}],
                "total": 1,
                "receipt": "kept",
                "next_actions": ["x".repeat(MAX_RESPONSE_BYTES)],
            });
            if oversized_receipt {
                env["receipt"] = "x".repeat(MAX_RESPONSE_BYTES).into();
            }
            let actual = cap_serialized_response(env, 1, 1);
            assert!(serde_json::to_vec(&actual).unwrap().len() < MAX_RESPONSE_BYTES);
            assert_eq!(actual["matches"], serde_json::json!([]));
            assert_eq!(actual["next_actions"], serde_json::json!([]));
            assert_eq!(actual["returned"], 0);
            assert_eq!(actual["text_truncated"], false);
            assert_eq!(actual["response_bytes_truncated"], true);
            if oversized_receipt {
                assert_eq!(actual["metadata_truncated"], true);
            } else {
                assert_eq!(actual["receipt"], "kept");
                assert!(actual.get("metadata_truncated").is_none());
            }
        }
    }

    #[test]
    fn span_file_filter_retains_dot_segments_and_skips_clean_misses() {
        let hits = std::collections::BTreeSet::from(["src/lib.rs".to_string()]);
        assert!(super::span_file_matches_hits("./src/lib.rs", &hits));
        assert!(super::span_file_matches_hits("/repo/src/./lib.rs", &hits));
        assert!(super::span_file_matches_hits(
            "/repo/a/../src/lib.rs",
            &hits
        ));
        assert!(!super::span_file_matches_hits("src/other.rs", &hits));
        assert!(!super::span_file_matches_hits("/repo/src/other.rs", &hits));
    }

    #[test]
    fn verified_spans_reject_mismatched_snapshot_even_in_session_cache_mode() {
        struct ResetSession(bool);
        impl Drop for ResetSession {
            fn drop(&mut self) {
                aden_graph::snapshot::set_session_cache_enabled(self.0);
            }
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='grep-rev-test'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn item() {}\n").unwrap();

        let _restore = ResetSession(aden_graph::snapshot::session_cache_enabled());
        aden_graph::snapshot::set_session_cache_enabled(true);

        let mut docs = std::collections::HashMap::new();
        let doc = aden_core::Document {
            anchor: "aden://module/lib#item".to_string(),
            source_span: Some(aden_core::SourceSpan {
                file: "lib.rs".to_string(),
                start_line: 1,
                end_line: 1,
                start_byte: 0,
                end_byte: 0,
            }),
            ..Default::default()
        };
        docs.insert(doc.anchor.clone(), doc);
        let encoded = aden_graph::snapshot::encode_snapshot(&docs, &[]).unwrap();
        let snap_path = aden_paths::graph_snapshot_file(dir.path());
        std::fs::create_dir_all(snap_path.parent().unwrap()).unwrap();
        std::fs::write(&snap_path, &encoded).unwrap();
        crate::indexer::fresh::publish_freshness_manifest(
            dir.path(),
            "deadbeefdeadbeef".into(),
            "0000000000000000".into(),
        )
        .unwrap();

        assert!(super::load_span_records(dir.path(), None).is_empty());
    }
}
