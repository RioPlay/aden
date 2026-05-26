#![allow(clippy::module_inception)]
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
#[cfg(test)]
mod tests {
    use crate::{AdenGraph, DocumentNode, cycles, parser};
    use aden_core::{Document, EdgeType, NodeType};
    use std::collections::HashMap;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_test_file(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    #[test]
    fn test_parse_file_extracts_anchor() {
        let tmp = create_test_file("[[test-anchor]]\n= Title\n\nHello");
        let parsed = parser::parse_file(tmp.path()).unwrap();
        assert_eq!(parsed.anchors, vec!["test-anchor"]);
    }

    #[test]
    fn test_parse_file_extracts_attributes() {
        let tmp = create_test_file(":key: value\n\n[[anchor]]\n= T\n");
        let parsed = parser::parse_file(tmp.path()).unwrap();
        assert_eq!(parsed.attributes.get("key"), Some(&"value".to_string()));
    }

    #[test]
    fn test_graph_build_from_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut file1 = std::fs::File::create(dir.path().join("a.adoc")).unwrap();
        file1
            .write_all(b"[[anchor-a]]\n= A\n\n<<anchor-b>>")
            .unwrap();
        let mut file2 = std::fs::File::create(dir.path().join("b.adoc")).unwrap();
        file2.write_all(b"[[anchor-b]]\n= B\n").unwrap();

        let graph = AdenGraph::build_from_directory(dir.path()).unwrap();
        assert!(graph.get_node("anchor-a").is_some());
        assert!(graph.get_node("anchor-b").is_some());
    }

    #[test]
    fn test_cycle_detection_no_cycle() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.adoc"), "[[a]]\n= A\n").unwrap();
        std::fs::write(dir.path().join("b.adoc"), "[[b]]\n= B\n").unwrap();
        let graph = AdenGraph::build_from_directory(dir.path()).unwrap();
        let cycles = cycles::find_cycles(&graph);
        assert!(cycles.is_empty());
    }

    #[test]
    fn test_cycle_detection_cycle() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.adoc"), "[[a]]\n= A\n\ninclude::b.adoc[]").unwrap();
        std::fs::write(dir.path().join("b.adoc"), "[[b]]\n= B\n\ninclude::a.adoc[]").unwrap();
        let graph = AdenGraph::build_from_directory(dir.path()).unwrap();
        let cycles = cycles::find_cycles(&graph);
        assert!(!cycles.is_empty());
    }

    #[test]
    fn test_semantic_diff_parsing() {
        let content = r#"[[test-anchor]]
= Title

agent-note::ADDED[2024-01-01]
agent-note::CHANGED[2024-02-01] Refactored module structure
agent-note::DEPRECATED[2024-03-01] Use new_module instead
"#;
        let tmp = create_test_file(content);
        let parsed = parser::parse_file(tmp.path()).unwrap();

        assert_eq!(parsed.semantic_diffs.len(), 3);

        match &parsed.semantic_diffs[0] {
            parser::SemanticDiff::Added { date } => assert_eq!(date, "2024-01-01"),
            other => panic!("Expected Added variant, got {:?}", other),
        }

        match &parsed.semantic_diffs[1] {
            parser::SemanticDiff::Changed { date, description } => {
                assert_eq!(date, "2024-02-01");
                assert_eq!(description, "Refactored module structure");
            }
            other => panic!("Expected Changed variant, got {:?}", other),
        }

        match &parsed.semantic_diffs[2] {
            parser::SemanticDiff::Deprecated { date, replacement } => {
                assert_eq!(date, "2024-03-01");
                assert_eq!(replacement.as_deref(), Some("Use new_module instead"));
            }
            other => panic!("Expected Deprecated variant, got {:?}", other),
        }
    }

    #[test]
    fn test_typed_edge_validation_valid() {
        let mut graph = AdenGraph::new();
        let parsed = parser::ParsedDocument {
            source_path: "/tmp/a.adoc".to_string(),
            attributes: HashMap::new(),
            anchors: vec!["mod-a".to_string()],
            refs: Vec::new(),
            includes: Vec::new(),
            edges: Vec::new(),
            conditional_stack: Vec::new(),
            raw_content: String::new(),
            semantic_diffs: Vec::new(),
            blocks: Vec::new(),
            tagged_regions: Vec::new(),
            conditional_regions: Vec::new(),
            metadata: None,
        };
        let doc1 = Document {
            anchor: "mod-a".to_string(),
            node_type: NodeType::Module,
            attributes: HashMap::new(),
            blocks: Vec::new(),
            source_span: None,
            metadata: None,
        };
        let doc2 = Document {
            anchor: "func-b".to_string(),
            node_type: NodeType::Function,
            attributes: HashMap::new(),
            blocks: Vec::new(),
            source_span: None,
            metadata: None,
        };
        let node1 = DocumentNode {
            anchor: "mod-a".to_string(),
            doc: doc1,
            parsed: parsed.clone(),
            source_path: std::path::PathBuf::from("/tmp/a.adoc"),
        };
        let node2 = DocumentNode {
            anchor: "func-b".to_string(),
            doc: doc2,
            parsed,
            source_path: std::path::PathBuf::from("/tmp/b.adoc"),
        };
        let idx1 = graph.graph.add_node(node1);
        let idx2 = graph.graph.add_node(node2);
        graph.anchor_to_index.insert("mod-a".to_string(), idx1);
        graph.anchor_to_index.insert("func-b".to_string(), idx2);
        graph.graph.add_edge(idx1, idx2, EdgeType::Uses);

        let errors = graph.validate_typed_edges();
        assert!(errors.is_empty(), "Expected no errors, got: {:?}", errors);
    }

    #[test]
    fn test_typed_edge_validation_invalid() {
        let mut graph = AdenGraph::new();
        let parsed = parser::ParsedDocument {
            source_path: "/tmp/a.adoc".to_string(),
            attributes: HashMap::new(),
            anchors: vec!["note-a".to_string()],
            refs: Vec::new(),
            includes: Vec::new(),
            edges: Vec::new(),
            conditional_stack: Vec::new(),
            raw_content: String::new(),
            semantic_diffs: Vec::new(),
            blocks: Vec::new(),
            tagged_regions: Vec::new(),
            conditional_regions: Vec::new(),
            metadata: None,
        };
        let doc1 = Document {
            anchor: "note-a".to_string(),
            node_type: NodeType::Note,
            attributes: HashMap::new(),
            blocks: Vec::new(),
            source_span: None,
            metadata: None,
        };
        let doc2 = Document {
            anchor: "adr-b".to_string(),
            node_type: NodeType::Adr,
            attributes: HashMap::new(),
            blocks: Vec::new(),
            source_span: None,
            metadata: None,
        };
        let node1 = DocumentNode {
            anchor: "note-a".to_string(),
            doc: doc1,
            parsed: parsed.clone(),
            source_path: std::path::PathBuf::from("/tmp/a.adoc"),
        };
        let node2 = DocumentNode {
            anchor: "adr-b".to_string(),
            doc: doc2,
            parsed,
            source_path: std::path::PathBuf::from("/tmp/b.adoc"),
        };
        let idx1 = graph.graph.add_node(node1);
        let idx2 = graph.graph.add_node(node2);
        graph.anchor_to_index.insert("note-a".to_string(), idx1);
        graph.anchor_to_index.insert("adr-b".to_string(), idx2);
        // Calls between two document nodes is invalid
        graph.graph.add_edge(idx1, idx2, EdgeType::Calls);

        let errors = graph.validate_typed_edges();
        assert!(
            !errors.is_empty(),
            "Expected validation errors for invalid Calls edge between documents"
        );
    }

    // ── Structured Block Parsing Tests ───────────────────────────

    #[test]
    fn test_parse_table_blocks() {
        let content = r#":proj: test
[[test-anchor]]
= Title

|===
|Name |Type
|foo |bar
|baz |qux
|===
"#;
        let tmp = create_test_file(content);
        let parsed = parser::parse_file(tmp.path()).unwrap();
        let tables: Vec<_> = parsed
            .blocks
            .iter()
            .filter_map(|b| match b {
                aden_core::Block::Table(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(tables.len(), 1, "Expected one table block");
        assert_eq!(tables[0].headers, vec!["Name", "Type"]);
        assert_eq!(tables[0].rows.len(), 2);
        assert_eq!(tables[0].rows[0], vec!["foo", "bar"]);
        assert_eq!(tables[0].rows[1], vec!["baz", "qux"]);
    }

    #[test]
    fn test_parse_listing_block() {
        let content = r#":proj: test
[[test-anchor]]
= Title

[source,rust]
----
fn main() {
    println!("hello");
}
----
"#;
        let tmp = create_test_file(content);
        let parsed = parser::parse_file(tmp.path()).unwrap();
        let listings: Vec<_> = parsed
            .blocks
            .iter()
            .filter_map(|b| match b {
                aden_core::Block::Listing { language, code } => {
                    Some((language.clone(), code.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(listings.len(), 1, "Expected one listing block");
        assert_eq!(listings[0].0, Some("rust".to_string()));
        assert!(listings[0].1.contains("fn main()"));
    }

    #[test]
    fn test_parse_admonition_block() {
        let content = r#":proj: test
[[test-anchor]]
= Title

WARNING: Do not commit secrets.
"#;
        let tmp = create_test_file(content);
        let parsed = parser::parse_file(tmp.path()).unwrap();
        let admonitions: Vec<_> = parsed
            .blocks
            .iter()
            .filter_map(|b| match b {
                aden_core::Block::Admonition { kind, text } => Some((kind.clone(), text.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(admonitions.len(), 1);
        assert_eq!(admonitions[0].0, aden_core::AdmonitionKind::Warning);
        assert_eq!(admonitions[0].1, "Do not commit secrets.");
    }

    #[test]
    fn test_parse_description_list_block() {
        let content = r#":proj: test
[[test-anchor]]
= Title

foo:: The foo module.
bar:: The bar module.
"#;
        let tmp = create_test_file(content);
        let parsed = parser::parse_file(tmp.path()).unwrap();
        let desc_lists: Vec<_> = parsed
            .blocks
            .iter()
            .filter_map(|b| match b {
                aden_core::Block::DescriptionList(items) => Some(items.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(desc_lists.len(), 2, "Expected two description list blocks");
        assert_eq!(desc_lists[0][0].0, "foo");
        assert_eq!(desc_lists[0][0].1, "The foo module.");
        assert_eq!(desc_lists[1][0].0, "bar");
        assert_eq!(desc_lists[1][0].1, "The bar module.");
    }

    #[test]
    fn test_parse_paragraph_blocks() {
        let content = r#":proj: test
[[test-anchor]]
= Title

First paragraph.
Still first paragraph.

Second paragraph.
"#;
        let tmp = create_test_file(content);
        let parsed = parser::parse_file(tmp.path()).unwrap();
        let paragraphs: Vec<_> = parsed
            .blocks
            .iter()
            .filter_map(|b| match b {
                aden_core::Block::Paragraph(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(paragraphs.len(), 2, "Expected two paragraph blocks");
        assert!(paragraphs[0].contains("First paragraph."));
        assert!(paragraphs[0].contains("Still first paragraph."));
        assert!(paragraphs[1].contains("Second paragraph."));
    }

    #[test]
    fn test_parse_mixed_blocks() {
        let content = r#":proj: test
[[test-anchor]]
= Title

Intro paragraph.

|===
|A |B
|1 |2
|===

IMPORTANT: Check this.

[source,bash]
----
echo hi
----

Closing paragraph.
"#;
        let tmp = create_test_file(content);
        let parsed = parser::parse_file(tmp.path()).unwrap();

        let mut found_paragraph = false;
        let mut found_table = false;
        let mut found_admonition = false;
        let mut found_listing = false;

        for block in &parsed.blocks {
            match block {
                aden_core::Block::Paragraph(t)
                    if (t.contains("Intro") || t.contains("Closing")) =>
                {
                    found_paragraph = true;
                }
                aden_core::Block::Table(t) if t.headers == vec!["A", "B"] => {
                    found_table = true;
                }
                aden_core::Block::Admonition { kind, .. }
                    if *kind == aden_core::AdmonitionKind::Important =>
                {
                    found_admonition = true;
                }
                aden_core::Block::Listing { language, .. }
                    if language.as_deref() == Some("bash") =>
                {
                    found_listing = true;
                }
                _ => {}
            }
        }
        assert!(found_paragraph, "Should find paragraph blocks");
        assert!(found_table, "Should find table block");
        assert!(found_admonition, "Should find admonition block");
        assert!(found_listing, "Should find listing block");
    }
}
