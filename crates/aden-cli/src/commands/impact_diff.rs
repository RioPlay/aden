// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Git-diff impact: map a `git diff` to the symbols it touches, then report the
//! blast radius (everything downstream) before you commit.
//!
//! This reuses three pieces aden already has: the per-file symbol spans from the
//! store (`grep::load_symbol_spans`), the smallest-enclosing-symbol resolver
//! (`grep::enclosing_symbol`), and the impact traversal over the typed-edge graph
//! (same edge set as `query --impact`). The only new logic is parsing unified
//! diff hunk headers into changed line ranges.

use crate::commands::grep::{enclosing_symbol, load_symbol_spans};
use crate::util::find_project_root;
use aden_graph::Direction;
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::Path;

/// Downstream (impact) edge types — a change to a symbol can break things reached
/// through these. Mirrors `cmd_query`'s `--impact` set so the two agree.
fn impact_edge_types() -> [aden_core::EdgeType; 6] {
    [
        aden_core::EdgeType::Uses,
        aden_core::EdgeType::Calls,
        aden_core::EdgeType::Constrains,
        aden_core::EdgeType::Invokes,
        aden_core::EdgeType::Implements,
        aden_core::EdgeType::Mutates,
    ]
}

pub fn cmd_impact_diff(
    path: &Path,
    since: Option<&str>,
    staged: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let root = find_project_root(path);
    // Keep the store fresh so span/edge resolution reflects the current code.
    super::ensure_fresh(&root);

    let diff = run_git_diff(&root, since, staged)?;
    let changed = parse_unified_diff(&diff);

    if changed.is_empty() {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "changed_files": 0, "touched": [], "blast_radius": 0,
                    "impacted": [], "risk": "none"
                })
            );
        } else {
            println!("No changed lines found (nothing to analyze).");
        }
        return Ok(());
    }

    // Changed lines -> the smallest symbol enclosing each, from the store spans.
    let spans_by_file = load_symbol_spans(&root);
    let mut touched: BTreeSet<String> = BTreeSet::new();
    for (file, lines) in &changed {
        if let Some(spans) = spans_by_file.get(file) {
            for &line in lines {
                if let Some(sp) = enclosing_symbol(spans, line) {
                    touched.insert(sp.anchor.clone());
                }
            }
        }
    }

    // Impact traversal per touched symbol over the typed-edge graph.
    let graph = aden_graph::cache::build_from_directory_cached(&root)?;
    let impact_types = impact_edge_types();

    // Per touched symbol: its transitive downstream set (excluding itself).
    let mut per_symbol: Vec<(String, BTreeSet<String>)> = Vec::new();
    let mut union: BTreeSet<String> = BTreeSet::new();
    for anchor in &touched {
        let mut downstream: BTreeSet<String> = BTreeSet::new();
        if let Some(start) = graph.get_index(anchor) {
            let mut visited = HashSet::new();
            let mut queue = VecDeque::new();
            visited.insert(start);
            queue.push_back(start);
            while let Some(node) = queue.pop_front() {
                for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
                    let is_impact = graph
                        .graph
                        .edges_connecting(node, neighbor)
                        .any(|e| impact_types.contains(&e.weight().edge_type));
                    if is_impact && visited.insert(neighbor) {
                        let a = &graph.graph[neighbor].doc.anchor;
                        if a != anchor {
                            downstream.insert(a.clone());
                            union.insert(a.clone());
                        }
                        queue.push_back(neighbor);
                    }
                }
            }
        }
        per_symbol.push((anchor.clone(), downstream));
    }

    let blast = union.len();
    let risk = risk_tier(blast);

    if json {
        let touched_json: Vec<serde_json::Value> = per_symbol
            .iter()
            .map(|(a, d)| {
                serde_json::json!({
                    "anchor": a,
                    "downstream_count": d.len(),
                    "downstream": d.iter().take(50).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "changed_files": changed.len(),
                "touched": touched_json,
                "blast_radius": blast,
                "impacted": union.iter().collect::<Vec<_>>(),
                "risk": risk,
            })
        );
        return Ok(());
    }

    println!(
        "Git-diff impact: {} changed file(s), {} touched symbol(s)",
        changed.len(),
        touched.len()
    );
    if touched.is_empty() {
        println!("  (no changed line fell inside a known symbol — docs, comments, or new files)");
    } else {
        println!("\nTouched symbols → blast radius:");
        // Most-impactful first so the riskiest change is at the top.
        let mut sorted = per_symbol.clone();
        sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
        for (anchor, downstream) in sorted.iter().take(20) {
            println!("  {:<4} {}", downstream.len(), short(anchor));
        }
        println!(
            "\nBlast radius: {} distinct downstream symbol(s). Risk: {}",
            blast,
            risk.to_uppercase()
        );
        println!(
            "  (risk is a heuristic on downstream breadth: 0=none, ≤5 low, ≤20 medium, else high)"
        );
    }
    Ok(())
}

/// Short, human display name from a full anchor.
fn short(anchor: &str) -> String {
    anchor
        .rsplit(['#', '/'])
        .next()
        .unwrap_or(anchor)
        .to_string()
}

/// Heuristic risk tier from the count of distinct downstream symbols.
fn risk_tier(blast: usize) -> &'static str {
    match blast {
        0 => "none",
        1..=5 => "low",
        6..=20 => "medium",
        _ => "high",
    }
}

/// Run `git diff --unified=0` in the appropriate mode and return its raw output.
/// `--unified=0` gives exact changed line ranges with no surrounding context.
fn run_git_diff(
    root: &Path,
    since: Option<&str>,
    staged: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut args: Vec<String> = vec!["diff".into(), "--unified=0".into(), "--no-color".into()];
    if staged {
        args.push("--cached".into());
    }
    if let Some(rev) = since {
        args.push(rev.to_string());
    }
    let output = std::process::Command::new("git")
        .args(&args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("running git diff: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Parse a unified diff into `new-file path -> set of changed line numbers`.
///
/// Reads `+++ b/<path>` to track the current new file and `@@ -.. +start,count @@`
/// hunk headers for the changed line ranges in the NEW file. A `count` of 0 (a
/// pure deletion) maps to the line at `start` (the join point), so a deleted
/// symbol body still attributes to its enclosing symbol.
fn parse_unified_diff(diff: &str) -> BTreeMap<String, BTreeSet<usize>> {
    let mut out: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    let mut current: Option<String> = None;

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            // "+++ b/path", "+++ /dev/null" (file deleted), or "+++ path".
            current = if rest == "/dev/null" {
                None
            } else {
                Some(rest.strip_prefix("b/").unwrap_or(rest).to_string())
            };
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@ ") {
            let Some(file) = current.as_ref() else {
                continue;
            };
            if let Some((start, count)) = parse_hunk_new_range(rest) {
                let entry = out.entry(file.clone()).or_default();
                if count == 0 {
                    entry.insert(start.max(1));
                } else {
                    for ln in start..start + count {
                        entry.insert(ln);
                    }
                }
            }
        }
    }
    out
}

/// From the part after `@@ `, extract the new-file `(start, count)`. The header
/// looks like `-old,n +start,count @@ optional`; `count` defaults to 1 when the
/// `,count` is omitted (git's convention for a single-line hunk).
fn parse_hunk_new_range(rest: &str) -> Option<(usize, usize)> {
    // Find the `+` token (the new-file range).
    let plus = rest.split_whitespace().find(|t| t.starts_with('+'))?;
    let spec = plus.strip_prefix('+')?;
    let mut parts = spec.splitn(2, ',');
    let start: usize = parts.next()?.parse().ok()?;
    let count: usize = match parts.next() {
        Some(c) => c.parse().ok()?,
        None => 1,
    };
    Some((start, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_range_with_and_without_count() {
        assert_eq!(parse_hunk_new_range("-1,2 +10,3 @@ fn foo"), Some((10, 3)));
        assert_eq!(parse_hunk_new_range("-5 +7 @@"), Some((7, 1))); // single line
        assert_eq!(parse_hunk_new_range("-1,4 +0,0 @@"), Some((0, 0))); // file deletion
    }

    #[test]
    fn parses_changed_lines_per_file() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -10,0 +11,2 @@ fn f
+new line
+another
@@ -20,1 +22,1 @@ fn g
+changed
diff --git a/src/b.rs b/src/b.rs
--- a/src/b.rs
+++ b/src/b.rs
@@ -3,2 +3,0 @@ fn h
";
        let m = parse_unified_diff(diff);
        assert_eq!(m.get("src/a.rs").unwrap(), &BTreeSet::from([11, 12, 22]));
        // a pure deletion (+3,0) attributes to the join line 3
        assert_eq!(m.get("src/b.rs").unwrap(), &BTreeSet::from([3]));
    }

    #[test]
    fn deleted_file_is_skipped() {
        let diff = "\
diff --git a/gone.rs b/gone.rs
--- a/gone.rs
+++ /dev/null
@@ -1,5 +0,0 @@
";
        assert!(parse_unified_diff(diff).is_empty());
    }

    #[test]
    fn risk_tiers() {
        assert_eq!(risk_tier(0), "none");
        assert_eq!(risk_tier(3), "low");
        assert_eq!(risk_tier(15), "medium");
        assert_eq!(risk_tier(99), "high");
    }
}
