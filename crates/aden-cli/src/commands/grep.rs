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
use std::path::Path;

use rayon::prelude::*;

use crate::util::{discover_source_files, find_project_root};

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
struct Span {
    anchor: String,
    start: usize,
    end: usize,
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

    let files = discover_source_files(&root)?;
    let mut matches: Vec<Match> = files
        .par_iter()
        .flat_map_iter(|file| {
            let rel = file.strip_prefix(&root).unwrap_or(file);
            let rel_str = rel.to_string_lossy().to_string();
            let content = match std::fs::read_to_string(file) {
                Ok(c) => c,
                Err(_) => return Vec::new().into_iter(), // binary / unreadable — skip
            };
            let spans = spans_by_file.get(&rel_str);
            let mut hits = Vec::new();
            for (i, line) in content.lines().enumerate() {
                if line_matches(line) {
                    let line_no = i + 1;
                    let enclosing = spans.and_then(|s| enclosing_symbol(s, line_no));
                    hits.push(Match {
                        file: rel_str.clone(),
                        line: line_no,
                        text: line.trim().to_string(),
                        symbol: enclosing.map(|sp| short_name(&sp.anchor)),
                        anchor: enclosing.map(|sp| sp.anchor.clone()),
                    });
                }
            }
            hits.into_iter()
        })
        .filter(|m| !symbol_only || m.symbol.is_some())
        .collect();

    // Deterministic ordering: by file, then line.
    matches.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    let total = matches.len();

    if json {
        print_json(&matches, limit);
        return Ok(());
    }

    if total == 0 {
        println!("No matches for '{}'.", pattern);
        return Ok(());
    }

    println!("Found {} match(es) for '{}':", total, pattern);
    for m in matches.iter().take(limit) {
        match &m.symbol {
            Some(sym) => println!("{}:{}  ({}): {}", m.file, m.line, sym, m.text),
            None => println!("{}:{}: {}", m.file, m.line, m.text),
        }
    }
    if total > limit {
        println!(
            "  ... and {} more (raise --limit, or refine the pattern)",
            total - limit
        );
    }
    Ok(())
}

/// Load `source_file -> [span]` from the store so each match can be attributed
/// to the symbol that encloses it.
fn load_symbol_spans(root: &Path) -> HashMap<String, Vec<Span>> {
    use aden_store::{GraphStorage, Storage};

    let mut by_file: HashMap<String, Vec<Span>> = HashMap::new();
    let store_path = root.join(".aden").join("store");
    let Some(store_str) = store_path.to_str() else {
        return by_file;
    };
    let Ok(storage) = Storage::new(store_str) else {
        return by_file;
    };
    let Ok(docs) = storage.get_all_documents() else {
        return by_file;
    };
    for (anchor, doc) in docs {
        let (Some(file), Some(start), Some(end)) = (
            doc.attributes.get("source_file"),
            doc.attributes.get("start_line").and_then(|s| s.parse::<usize>().ok()),
            doc.attributes.get("end_line").and_then(|s| s.parse::<usize>().ok()),
        ) else {
            continue;
        };
        // Normalize the stored path to be relative to the project root so it
        // matches the relative paths used while scanning.
        let rel = Path::new(file)
            .strip_prefix(root)
            .unwrap_or(Path::new(file))
            .to_string_lossy()
            .to_string();
        by_file.entry(rel).or_default().push(Span { anchor, start, end });
    }
    by_file
}

/// The most specific symbol whose span contains `line` (smallest enclosing
/// span wins, so a method beats the file-level node it sits in).
fn enclosing_symbol(spans: &[Span], line: usize) -> Option<&Span> {
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
fn print_json(matches: &[Match], limit: usize) {
    let total = matches.len();
    let returned = total.min(limit);
    let items: Vec<String> = matches
        .iter()
        .take(limit)
        .map(|m| {
            format!(
                "    {{\"file\": {}, \"line\": {}, \"symbol\": {}, \"anchor\": {}, \"text\": {}}}",
                json_str(&m.file),
                m.line,
                m.symbol.as_deref().map(json_str).unwrap_or_else(|| "null".to_string()),
                m.anchor.as_deref().map(json_str).unwrap_or_else(|| "null".to_string()),
                json_str(&m.text),
            )
        })
        .collect();
    println!(
        "{{\"total\": {}, \"returned\": {}, \"truncated\": {}, \"matches\": [\n{}\n]}}",
        total,
        returned,
        total > limit,
        items.join(",\n")
    );
}

/// Minimal JSON string escaping (quotes, backslashes, control chars).
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
