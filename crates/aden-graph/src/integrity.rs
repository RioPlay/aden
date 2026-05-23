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
pub fn check_hashes(graph: &AdenGraph) -> Vec<(String, String)> {
    let mut issues = Vec::new();
    for node in graph.graph.node_indices() {
        let doc = &graph.graph[node];
        if let Some(expected_hash) = doc.doc.attributes.get("source_hash") {
            // If the document has a `source_file` attribute (e.g. a generated contract
            // pointing back to its original source), read that file. Otherwise fall back
            // to the document's own path for self-describing documents.
            let target_path = doc
                .doc
                .attributes
                .get("source_file")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| doc.source_path.clone());
            match std::fs::read(&target_path) {
                Ok(content) => {
                    let actual_hash = aden_core::stable_hash(&content);
                    if &actual_hash != expected_hash {
                        issues.push((
                            doc.anchor.clone(),
                            "ERROR: source_hash mismatch".to_string(),
                        ));
                    }
                }
                Err(_) => {
                    issues.push((
                        doc.anchor.clone(),
                        "ERROR: source file missing".to_string(),
                    ));
                }
            }
        }
    }
    issues
}
