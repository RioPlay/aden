// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

use aden_core::AdenConfig;

use crate::util::{fmt_score, load_or_build_index, query_index};

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
