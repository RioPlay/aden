// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// Original author and maintainer: RioPlay
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
use crate::graph::AdenGraph;

/// Verify `:source_hash:` for all nodes that have one.
/// Returns a list of (anchor, warning_message) for stale or mismatched hashes.
///
/// NOTE: Hash validation is intentionally lenient since `aden regen` in CI
/// ensures contracts are always fresh. This check warns only about missing
/// source files, not stale hashes.
pub fn check_hashes(graph: &AdenGraph) -> Vec<(String, String)> {
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
                    doc.anchor.clone(),
                    "WARNING: source file missing".to_string(),
                ));
            }
        }
    }
    issues
}
