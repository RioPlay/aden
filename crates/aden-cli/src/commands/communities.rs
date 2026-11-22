// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `aden communities` — surface the codebase's functional clusters.
//!
//! Runs modularity community detection (see `aden_graph::community`) over the
//! knowledge graph and reports the clusters — groups of symbols that call/use
//! each other densely — independent of the directory layout. Useful for
//! orienting in an unfamiliar repo and as routing targets for `ask`.

use crate::util::find_project_root;
use std::collections::BTreeMap;
use std::path::Path;

const SUMMARY_SAMPLE_LIMIT: usize = 8;

pub fn cmd_communities(
    path: &Path,
    min_size: usize,
    limit: usize,
    resolution: f64,
    summary: bool,
    max_members: usize,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let root = find_project_root(path);
    let _stale_hint = super::StaleHintGuard::new(&root, json);
    super::ensure_fresh(&root);

    let graph = aden_graph::cache::build_from_directory_cached(&root)?;
    let all = aden_graph::community::detect_communities(&graph, resolution);
    // Keep communities of at least `min_size` (default hides singletons, which
    // are just unconnected symbols and add noise), ordered largest-first.
    let communities = prepare_communities(all, min_size);

    if json {
        let payload =
            format_communities_json_value(&communities, min_size, limit, summary, max_members);
        let env = super::augment_read_json(&root, payload);
        println!("{}", serde_json::to_string_pretty(&env)?);
        return Ok(());
    }

    if communities.is_empty() {
        println!("No communities of size >= {min_size} found.");
        return Ok(());
    }

    println!(
        "Found {} community/communities (size >= {}):",
        communities.len(),
        min_size
    );
    for (i, members) in communities.iter().take(limit).enumerate() {
        println!(
            "\nCommunity {} — {} symbols [mostly {}]",
            i + 1,
            members.len(),
            dominant_module(members)
        );
        for m in members.iter().take(8) {
            println!("  - {}", short(m));
        }
        if members.len() > 8 {
            println!("  ... and {} more", members.len() - 8);
        }
    }
    if communities.len() > limit {
        println!(
            "\n... and {} more communities (raise --limit)",
            communities.len() - limit
        );
    }
    Ok(())
}

/// Filter communities to those with at least `min_size` members and sort
/// descending by size (tie-breaking lexicographically by `first()` for
/// deterministic output).
fn prepare_communities(all: Vec<Vec<String>>, min_size: usize) -> Vec<Vec<String>> {
    let mut communities: Vec<Vec<String>> =
        all.into_iter().filter(|c| c.len() >= min_size).collect();
    communities.sort_by(|a, b| {
        b.len()
            .cmp(&a.len())
            .then_with(|| a.first().cmp(&b.first()))
    });
    communities
}

/// Format prepared (already filtered and sorted) communities as bounded JSON.
fn format_communities_json_value(
    communities: &[Vec<String>],
    min_size: usize,
    limit: usize,
    summary: bool,
    max_members: usize,
) -> serde_json::Value {
    let total = communities.len();
    let arr: Vec<serde_json::Value> = communities
        .iter()
        .take(limit)
        .map(|members| {
            let label = dominant_module(members);
            if summary {
                let shown = members.len().min(SUMMARY_SAMPLE_LIMIT);
                serde_json::json!({
                    "size": members.len(),
                    "label": label,
                    "sample_members": &members[..shown],
                    "members_truncated": members.len() > SUMMARY_SAMPLE_LIMIT,
                })
            } else {
                let shown = if max_members == 0 {
                    members.len()
                } else {
                    max_members.min(members.len())
                };
                serde_json::json!({
                    "size": members.len(),
                    "label": label,
                    "members": &members[..shown],
                    "members_truncated": members.len() > shown,
                })
            }
        })
        .collect();
    let returned = arr.len();
    serde_json::json!({
        "communities": returned,
        "total_communities": total,
        "returned": returned,
        "truncated": total > returned,
        "min_size": min_size,
        "limit": limit,
        "summary": summary,
        "items": arr,
    })
}

/// The source *area* (directory) most members live in, as a human label for the
/// cluster. Directory-based so it is meaningful for ANY language — a Rust
/// workspace (`crates/aden-index/src`), a Python package (`src/retrieval`), a JS
/// app (`app/api`) — not just multi-crate repos. Falls back to "mixed".
pub(crate) fn dominant_module(members: &[String]) -> String {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for m in members {
        *counts.entry(area_of(m)).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map(|(m, _)| m)
        .unwrap_or_else(|| "mixed".to_string())
}

/// The directory portion of the file an anchor belongs to — language-agnostic.
/// `aden://module/<project>/<path/to/file.ext>#sym` → `<path/to>` (the file's
/// directory). For a `mod-<name>` hub or an unrecognized anchor, returns the name
/// or "mixed".
fn area_of(anchor: &str) -> String {
    if let Some(rest) = anchor
        .strip_prefix("aden://module/")
        .or_else(|| anchor.strip_prefix("aden://doc/"))
    {
        // rest = "<project>/<path/to/file.ext>#sym" (or "...#sym" absent).
        let path = rest.split('#').next().unwrap_or(rest);
        match path.rfind('/') {
            // Directory of the file; drop the leading "<project>/" so the label
            // is the area, not the whole path, when there's depth to spare.
            Some(slash) => path[..slash].to_string(),
            None => path.to_string(),
        }
    } else if let Some(rest) = anchor.strip_prefix("mod-") {
        rest.to_string()
    } else {
        "mixed".to_string()
    }
}

/// Short, human display name from a full anchor.
fn short(anchor: &str) -> String {
    anchor
        .rsplit(['#', '/'])
        .next()
        .unwrap_or(anchor)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_of_is_directory_based_and_polyglot() {
        // Rust workspace
        assert_eq!(
            area_of("aden://module/aden/crates/aden-index/src/lib.rs#query"),
            "aden/crates/aden-index/src"
        );
        // Python single project
        assert_eq!(
            area_of("aden://module/myproj/src/retrieval/embed.py#encode"),
            "myproj/src/retrieval"
        );
        // mod hub + unknown
        assert_eq!(area_of("mod-aden-cli"), "aden-cli");
        assert_eq!(area_of("weird"), "mixed");
    }

    #[test]
    fn dominant_module_picks_majority_area() {
        let members = vec![
            "aden://module/p/src/api/a.ts#x".to_string(),
            "aden://module/p/src/api/b.ts#y".to_string(),
            "aden://module/p/src/db/c.ts#z".to_string(),
        ];
        assert_eq!(dominant_module(&members), "p/src/api");
    }

    fn make_members(prefix: &str, count: usize) -> Vec<String> {
        (0..count)
            .map(|i| format!("aden://module/p/src/{prefix}/f{i:02}.rs#sym_{i:02}"))
            .collect()
    }

    #[test]
    fn prepare_communities_filters_and_sorts_largest_first_deterministically() {
        let raw = vec![
            make_members("singleton", 1),
            make_members("tie_b", 3),
            make_members("small", 2),
            make_members("largest", 5),
            make_members("tie_a", 3),
        ];
        let prepared = prepare_communities(raw, 2);
        assert_eq!(prepared.len(), 4);
        assert_eq!(prepared[0].len(), 5);
        assert_eq!(dominant_module(&prepared[0]), "p/src/largest");
        assert_eq!(prepared[1].len(), 3);
        assert_eq!(dominant_module(&prepared[1]), "p/src/tie_a");
        assert_eq!(prepared[2].len(), 3);
        assert_eq!(dominant_module(&prepared[2]), "p/src/tie_b");
        assert_eq!(prepared[3].len(), 2);
        assert_eq!(dominant_module(&prepared[3]), "p/src/small");
    }

    #[test]
    fn format_communities_json_enforces_limit_and_reports_truncation() {
        let communities = prepare_communities(
            vec![
                make_members("a", 2),
                make_members("b", 6),
                make_members("c", 4),
            ],
            2,
        );
        let limited = format_communities_json_value(&communities, 2, 2, false, 50);
        assert_eq!(limited["communities"], 2);
        assert_eq!(limited["total_communities"], 3);
        assert_eq!(limited["returned"], 2);
        assert_eq!(limited["truncated"], true);
        assert_eq!(limited["min_size"], 2);
        assert_eq!(limited["limit"], 2);
        assert_eq!(limited["summary"], false);
        let items = limited["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["size"], 6);
        assert_eq!(items[0]["label"], "p/src/b");
        assert_eq!(items[1]["size"], 4);
        assert_eq!(items[1]["label"], "p/src/c");

        let full = format_communities_json_value(&communities, 2, 10, false, 50);
        assert_eq!(full["communities"], 3);
        assert_eq!(full["total_communities"], 3);
        assert_eq!(full["returned"], 3);
        assert_eq!(full["truncated"], false);
    }

    #[test]
    fn format_communities_json_summary_emits_sample_members_without_members() {
        let communities =
            prepare_communities(vec![make_members("big", 12), make_members("small", 5)], 2);
        let val = format_communities_json_value(&communities, 2, 30, true, 50);
        assert_eq!(val["summary"], true);
        let items = val["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);

        let big = &items[0];
        assert_eq!(big["size"], 12);
        assert!(big.get("members").is_none());
        assert_eq!(big["sample_members"].as_array().unwrap().len(), 8);
        assert_eq!(big["members_truncated"], true);

        let small = &items[1];
        assert_eq!(small["size"], 5);
        assert!(small.get("members").is_none());
        assert_eq!(small["sample_members"].as_array().unwrap().len(), 5);
        assert_eq!(small["members_truncated"], false);
    }

    #[test]
    fn format_communities_json_respects_max_members_cap_and_zero_unlimited() {
        let communities =
            prepare_communities(vec![make_members("large", 15), make_members("exact", 4)], 2);

        let capped = format_communities_json_value(&communities, 2, 30, false, 4);
        let items = capped["items"].as_array().unwrap();
        assert_eq!(items[0]["size"], 15);
        assert_eq!(items[0]["members"].as_array().unwrap().len(), 4);
        assert_eq!(items[0]["members_truncated"], true);
        assert!(items[0].get("sample_members").is_none());

        assert_eq!(items[1]["size"], 4);
        assert_eq!(items[1]["members"].as_array().unwrap().len(), 4);
        assert_eq!(items[1]["members_truncated"], false);

        let unlimited = format_communities_json_value(&communities, 2, 30, false, 0);
        let unlimited_items = unlimited["items"].as_array().unwrap();
        assert_eq!(unlimited_items[0]["size"], 15);
        assert_eq!(unlimited_items[0]["members"].as_array().unwrap().len(), 15);
        assert_eq!(unlimited_items[0]["members_truncated"], false);
    }
}
