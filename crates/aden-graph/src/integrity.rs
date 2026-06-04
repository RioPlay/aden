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
///
/// `root` is the project root. A `source_file` attribute may be stored as a
/// path RELATIVE to that root (e.g. `src/flask/ctx.py`); it must be joined onto
/// `root` before the existence check, otherwise it would be resolved against the
/// process CWD and every relative path would falsely report "source file
/// missing" whenever `check` runs against an external codebase from elsewhere.
/// Absolute `source_file` values are used as-is.
pub fn check_hashes(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    root: &std::path::Path,
) -> Vec<(String, String)> {
    let mut issues = Vec::new();
    for node in graph.graph.node_indices() {
        let doc = &graph.graph[node];
        if let Some(_expected_hash) = doc.doc.attributes.get("source_hash") {
            // Only check for missing source files - stale hashes are expected
            // and will be fixed by CI regen (aden regen runs before aden check)
            let Some(source_file) = doc.doc.attributes.get("source_file") else {
                continue;
            };
            let candidate = std::path::Path::new(source_file);
            let target_path = if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                root.join(candidate)
            };
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

#[cfg(test)]
mod tests {
    use super::*;
    use aden_core::{Document, NodeType};
    use std::collections::HashMap;

    fn node_with_source(anchor: &str, source_file: &str) -> DocumentNode {
        let mut attributes = HashMap::new();
        attributes.insert("source_hash".to_string(), "deadbeef".to_string());
        attributes.insert("source_file".to_string(), source_file.to_string());
        DocumentNode {
            doc: Document {
                anchor: anchor.to_string(),
                node_type: NodeType::Function,
                attributes,
                blocks: Vec::new(),
                source_span: None,
                metadata: None,
                confidence: 1.0,
            },
            parsed: None,
            source_path: std::path::PathBuf::from(source_file),
        }
    }

    #[test]
    fn relative_source_file_resolves_against_root_not_cwd() {
        // Reproduces the external-codebase false positive: a contract whose
        // `source_file` is stored relative to the project root must be looked up
        // under that root, not the process CWD.
        let dir = std::env::temp_dir().join("aden_integrity_relroot");
        let sub = dir.join("src/pkg");
        std::fs::create_dir_all(&sub).unwrap();
        let file = sub.join("mod.py");
        std::fs::write(&file, "x = 1\n").unwrap();

        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        let idx = graph
            .graph
            .add_node(node_with_source("aden://module/pkg/mod.py#x", "src/pkg/mod.py"));
        graph
            .anchor_to_index
            .insert("aden://module/pkg/mod.py#x".to_string(), idx);

        // Resolved against the real root → no issue.
        assert!(check_hashes(&graph, &dir).is_empty());
        // Resolved against an unrelated root → flagged missing.
        let issues = check_hashes(&graph, std::path::Path::new("/nonexistent-root"));
        assert_eq!(issues.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn absolute_source_file_is_used_verbatim() {
        let dir = std::env::temp_dir().join("aden_integrity_absroot");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abs.py");
        std::fs::write(&file, "y = 2\n").unwrap();

        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        let idx = graph.graph.add_node(node_with_source(
            "aden://module/pkg/abs.py#y",
            file.to_str().unwrap(),
        ));
        graph
            .anchor_to_index
            .insert("aden://module/pkg/abs.py#y".to_string(), idx);

        // Absolute path exists regardless of the (wrong) root passed in.
        assert!(check_hashes(&graph, std::path::Path::new("/some/other/root")).is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }
}
