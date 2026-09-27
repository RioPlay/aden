// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

use aden_core::AdenConfig;

use crate::util::{fmt_score, load_or_build_index, query_index_with_navigation};

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

const SEARCH_CURSOR_VERSION: &str = "v1";

fn search_scope_hash(query: &str, doc_type: Option<&str>, semantics: bool) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in query
        .bytes()
        .chain([0])
        .chain(doc_type.unwrap_or_default().bytes())
        .chain([u8::from(semantics)])
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn encode_search_cursor(revision: &str, scope_hash: u64, offset: usize) -> String {
    format!("{SEARCH_CURSOR_VERSION}:{revision}:{scope_hash:016x}:{offset}")
}

fn decode_search_cursor(cursor: &str) -> Result<(&str, u64, usize), &'static str> {
    let mut parts = cursor.split(':');
    let version = parts.next().ok_or("missing cursor version")?;
    let revision = parts.next().ok_or("missing graph revision")?;
    let scope_hash = parts.next().ok_or("missing query binding")?;
    let offset = parts.next().ok_or("missing result offset")?;
    if version != SEARCH_CURSOR_VERSION || parts.next().is_some() || revision.is_empty() {
        return Err("unsupported or malformed search cursor");
    }
    let scope_hash = u64::from_str_radix(scope_hash, 16).map_err(|_| "invalid query binding")?;
    let offset = offset.parse().map_err(|_| "invalid result offset")?;
    Ok((revision, scope_hash, offset))
}

pub struct SearchOptions<'a> {
    pub path: &'a Path,
    pub query: &'a str,
    pub limit: usize,
    pub offset: usize,
    pub cursor: Option<&'a str>,
    pub doc_type: Option<&'a str>,
    pub include_semantics: bool,
    pub json: bool,
}

pub fn cmd_search(opts: SearchOptions<'_>) -> Result<(), Box<dyn std::error::Error>> {
    let SearchOptions {
        path,
        query,
        limit,
        offset,
        cursor,
        doc_type,
        include_semantics,
        json,
    } = opts;
    if !path.is_dir() {
        return Err("search requires a directory path".into());
    }
    let _stale_hint = super::StaleHintGuard::new(path, json);
    super::ensure_fresh(path);

    if cursor.is_some() && !json {
        return Err("search --cursor requires structured JSON output".into());
    }
    let scope_hash = search_scope_hash(query, doc_type, include_semantics);
    let decoded_cursor = cursor.map(decode_search_cursor).transpose();
    let effective_offset = decoded_cursor
        .as_ref()
        .ok()
        .and_then(|decoded| decoded.as_ref().map(|(_, _, offset)| *offset))
        .unwrap_or(offset);

    // Load config to check for private patterns (ADRs, retros, etc.)
    let config = AdenConfig::load(path);

    let index = load_or_build_index(path)?;
    let mut results = query_index_with_navigation(&index, query, path);

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
        let page: Vec<_> = results.iter().skip(effective_offset).take(limit).collect();
        let mut env = super::augment_read_json(
            path,
            serde_json::json!({
                "total": total,
                "returned": page.len(),
                "offset": effective_offset,
                "truncated": effective_offset + page.len() < total,
                "results": page.iter().map(|r| serde_json::json!({
                    "anchor": r.anchor,
                    "score": r.score,
                    "snippet": r.snippet,
                })).collect::<Vec<_>>(),
                "semantic": semantic_results.iter().map(|(anchor, rel)| serde_json::json!({
                    "anchor": anchor,
                    "relationship": rel,
                })).collect::<Vec<_>>(),
            }),
        );
        let revision = env["context_receipt"]["graph_revision"].as_str();
        let cursor_error = match decoded_cursor {
            Err(message) => Some(("cursor_invalid", message.to_string())),
            Ok(Some((expected_revision, expected_scope, _)))
                if Some(expected_revision) != revision || expected_scope != scope_hash =>
            {
                Some((
                    "cursor_stale",
                    "search cursor does not match this graph revision or query".to_string(),
                ))
            }
            Ok(_) => None,
        };
        if let Some((code, message)) = cursor_error {
            let restart = super::next_actions::search_results_page(
                query,
                path,
                limit,
                None,
                doc_type,
                include_semantics,
                "Restart this search from the current graph revision",
            );
            let error = serde_json::json!({
                "schema_version": 1,
                "error": {
                    "code": code,
                    "message": message,
                    "recovery": "restart from offset 0 using the current graph revision",
                },
                "next_actions": restart.into_iter().collect::<Vec<_>>(),
                "context_receipt": env["context_receipt"].clone(),
                "freshness": env["freshness"].clone(),
                "index_stale": env["index_stale"].clone(),
            });
            println!("{}", serde_json::to_string(&error)?);
            return Ok(());
        }

        if effective_offset + page.len() < total
            && let Some(revision) = revision
        {
            let next_cursor = encode_search_cursor(
                revision,
                scope_hash,
                effective_offset.saturating_add(page.len()),
            );
            let action = super::next_actions::search_results_page(
                query,
                path,
                limit,
                Some(&next_cursor),
                doc_type,
                include_semantics,
                "Continue this search on the same graph revision",
            );
            env["next_cursor"] = next_cursor.into();
            env["next_actions"] = serde_json::json!(action.into_iter().collect::<Vec<_>>());
        }
        println!("{}", serde_json::to_string(&env)?);
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
    let _stale_hint = super::StaleHintGuard::new(path, json);
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
        let env = super::augment_read_json(
            path,
            serde_json::json!({
                "total": total_count,
                "returned": limited.len(),
                "offset": offset,
                "truncated": offset + limited.len() < total_count,
                "anchors": items,
            }),
        );
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

#[cfg(test)]
mod cursor_tests {
    use super::*;

    #[test]
    fn cursor_round_trips_and_rejects_malformed_values() {
        let cursor = encode_search_cursor("revision", 0x1234, 42);
        assert_eq!(decode_search_cursor(&cursor), Ok(("revision", 0x1234, 42)));
        assert!(decode_search_cursor("v2:revision:0000000000001234:42").is_err());
        assert!(decode_search_cursor("v1:revision:wrong:42").is_err());
        assert!(decode_search_cursor("v1:revision:0000000000001234:nope").is_err());
    }

    #[test]
    fn cursor_scope_binds_query_filters_and_semantics() {
        let base = search_scope_hash("query", Some("module"), false);
        assert_ne!(base, search_scope_hash("other", Some("module"), false));
        assert_ne!(base, search_scope_hash("query", Some("adr"), false));
        assert_ne!(base, search_scope_hash("query", Some("module"), true));
    }
}
