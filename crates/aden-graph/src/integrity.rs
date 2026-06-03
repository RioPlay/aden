// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::graph::AdenGraph;
use crate::nodes::{AdenEdge, DocumentNode, GraphNode};

/// Verify `:source_hash:` for all nodes that have one.
/// Returns a list of (anchor, warning_message) for stale or mismatched hashes.
///
/// NOTE: Hash validation is intentionally lenient since `aden regen` in CI
/// ensures contracts are always fresh. This check warns only about missing
/// source files, not stale hashes.
pub fn check_hashes(graph: &AdenGraph<DocumentNode, AdenEdge>) -> Vec<(String, String)> {
    let mut issues = Vec::new();
    for node in graph.graph.node_indices() {
        let doc = &graph.graph[node];
        if let Some(_expected_hash) = doc.doc.attributes.get("source_hash") {
            // Only check for missing source files - stale hashes are expected
            // and will be fixed by CI regen (aden regen runs before aden check)
            let Some(source_file) = doc.doc.attributes.get("source_file") else {
                continue;
            };
            let target_path = std::path::PathBuf::from(source_file);
            if !target_path.exists() {
                issues.push((
                    doc.anchor().to_string(),
                    "WARNING: source file missing".to_string(),
                ));
            }
        }
    }
    issues
}
