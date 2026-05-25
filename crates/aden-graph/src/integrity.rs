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
/// IMPORTANT: Only checks contracts that explicitly declare `:source_file:`.
/// Human-facing docs contracts (e.g. `docs/module-*.adoc`) without `:source_file:`
/// are skipped — their freshness is semantic, not hash-based.
pub fn check_hashes(graph: &AdenGraph) -> Vec<(String, String)> {
    let mut issues = Vec::new();
    for node in graph.graph.node_indices() {
        let doc = &graph.graph[node];
        if let Some(expected_hash) = doc.doc.attributes.get("source_hash") {
            // Only validate hashes for contracts that explicitly point to a source file.
            // Human-facing docs contracts (no :source_file:) are intentionally skipped.
            let Some(source_file) = doc.doc.attributes.get("source_file") else {
                continue;
            };
            let target_path = std::path::PathBuf::from(source_file);
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
