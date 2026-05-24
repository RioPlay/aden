// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Generic tree-sitter extraction utilities used by all language adapters.
//!
//! Aden supports any language with a tree-sitter grammar. Adding a new
//! language requires ~50 lines: define tree-sitter queries for functions,
//! types, and modules, then wire into `parse_file` via the extension map.

use std::path::Path;

/// Convert tree-sitter source text from a node.
pub fn node_text<'a>(node: tree_sitter::Node<'a>, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

/// Convert a tree-sitter node to a SourceSpan.
pub fn node_to_span(node: tree_sitter::Node, path: &Path) -> aden_core::SourceSpan {
    let start = node.start_position();
    let end = node.end_position();
    aden_core::SourceSpan {
        file: path.to_string_lossy().to_string(),
        start_line: start.row + 1,
        end_line: end.row + 1,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    }
}

/// Determine visibility from a node that optionally has visibility modifiers.
/// Default implementation: public if no modifier found.
pub fn infer_visibility_default(node: tree_sitter::Node, _source: &str) -> aden_core::Visibility {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if kind.contains("public") || kind == "pub" {
            return aden_core::Visibility::Public;
        }
        if kind.contains("private") || kind == "private" {
            return aden_core::Visibility::Private;
        }
    }
    aden_core::Visibility::Public
}

/// Extract doc comments from preceding sibling nodes.
pub fn extract_doc_comments(node: tree_sitter::Node, source: &str) -> Vec<String> {
    let mut comments = Vec::new();
    let mut current = node.prev_sibling();
    while let Some(sib) = current {
        if sib.kind() == "comment" || sib.kind().ends_with("comment") {
            let text = node_text(sib, source).trim();
            if text.starts_with("///") || text.starts_with("/**") || text.starts_with("##'") {
                comments.push(text.to_string());
            }
        } else {
            break;
        }
        current = sib.prev_sibling();
    }
    comments.reverse();
    comments
}

/// Infer crate / project name from path (fallback for languages without crates).
pub fn infer_project_name(path: &Path) -> String {
    path.ancestors()
        .find(|p| p.join("Cargo.toml").exists() || p.join("package.json").exists() || p.join("pyproject.toml").exists() || p.join("setup.py").exists())
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
