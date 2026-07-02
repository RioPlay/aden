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

pub fn cmd_communities(
    path: &Path,
    min_size: usize,
    limit: usize,
    resolution: f64,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let root = find_project_root(path);
    let _stale_hint = super::StaleHintGuard::new(&root, json);
    super::ensure_fresh(&root);

    let graph = aden_graph::cache::build_from_directory_cached(&root)?;
    let all = aden_graph::community::detect_communities(&graph, resolution);
    // Keep communities of at least `min_size` (default hides singletons, which
    // are just unconnected symbols and add noise).
    let communities: Vec<Vec<String>> = all.into_iter().filter(|c| c.len() >= min_size).collect();

    if json {
        let arr: Vec<serde_json::Value> = communities
            .iter()
            .map(|members| {
                serde_json::json!({
                    "size": members.len(),
                    "label": dominant_module(members),
                    "members": members,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({ "communities": arr.len(), "items": arr })
        );
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
}
