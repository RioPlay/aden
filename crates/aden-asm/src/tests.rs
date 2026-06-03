// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
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
    use crate::{preprocess, traverse};
    use crate::traverse::strip_asciidoc_markup;
    use std::collections::HashMap;

    #[test]
    fn test_preprocess_simple_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            ":key: value\n\n[[anchor]]\n= Title\n\nHello world",
        )
        .unwrap();
        let mut visited = Vec::new();
        let out = preprocess::preprocess(tmp.path(), &HashMap::new(), &mut visited, 0).unwrap();
        assert!(out.contains("[[anchor]]"));
        assert!(out.contains("Hello world"));
    }

    #[test]
    fn test_preprocess_does_not_panic_on_no_includes() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "[[solo]]\n= Solo\n\nNo includes here.").unwrap();
        let mut visited = Vec::new();
        let out = preprocess::preprocess(tmp.path(), &HashMap::new(), &mut visited, 0).unwrap();
        assert!(out.contains("No includes here."));
    }

    #[test]
    fn test_assemble_options_default() {
        let opts = traverse::AssemblyOptions {
            start_anchor: "start".to_string(),
            max_depth: 3,
            token_budget: 1000,
            edge_types: vec![],
            block_filter: vec![],
            include_tags: vec![],
            exclude_tags: vec![],
            attributes: vec![],
            llm_mode: false,
        };
        assert_eq!(opts.start_anchor, "start");
        assert_eq!(opts.max_depth, 3);
    }

    // --- strip_asciidoc_markup tests ---

    #[test]
    fn test_strip_removes_anchor_lines() {
        let input = "[[my-anchor]]\n= Title\n\nSome content.";
        let out = strip_asciidoc_markup(input);
        assert!(!out.contains("[[my-anchor]]"), "anchor line must be removed");
        assert!(out.contains("Title"));
        assert!(out.contains("Some content."));
    }

    #[test]
    fn test_strip_removes_attribute_lines() {
        let input = ":source_file: foo.rs\n:author: Alice\n\nActual prose.";
        let out = strip_asciidoc_markup(input);
        assert!(!out.contains(":source_file:"), "attribute line must be removed");
        assert!(!out.contains(":author:"), "attribute line must be removed");
        assert!(out.contains("Actual prose."));
    }

    #[test]
    fn test_strip_removes_block_delimiters() {
        let input = "----\nsome code\n----\nAfter block.";
        let out = strip_asciidoc_markup(input);
        assert!(!out.contains("----"), "block delimiters must be removed");
        assert!(out.contains("some code"));
        assert!(out.contains("After block."));
    }

    #[test]
    fn test_strip_removes_role_annotations() {
        let input = "[source,rust]\nlet x = 1;\n[NOTE]\nBe careful.";
        let out = strip_asciidoc_markup(input);
        assert!(!out.contains("[source,rust]"), "role annotation must be removed");
        assert!(!out.contains("[NOTE]"), "role annotation must be removed");
        assert!(out.contains("let x = 1;"));
        assert!(out.contains("Be careful."));
    }

    #[test]
    fn test_strip_removes_table_delimiter() {
        let input = "|===\n| Col1 | Col2\n|===";
        let out = strip_asciidoc_markup(input);
        assert!(!out.contains("|==="), "table delimiter must be removed");
        // Generic table rows are compacted: pipes stripped, cells space-separated.
        // Data values must still be present.
        assert!(out.contains("Col1"), "cell data Col1 must be preserved");
        assert!(out.contains("Col2"), "cell data Col2 must be preserved");
    }

    #[test]
    fn test_strip_compacts_property_table() {
        // The signature table collapses to a single dense line:
        // `name(params) -> ret`. The Name row (duplicates the node title) and
        // the Visibility row (repeats on every symbol) are dropped.
        let input = "|===\n|Property|Value\n|Name|assemble\n|Visibility|Public\n|param graph|graph: &Graph\n|Returns|String\n|===";
        let out = strip_asciidoc_markup(input);
        assert!(!out.contains("|==="), "table delimiter must be removed");
        assert!(!out.contains("|Property|Value"), "header row must be removed");
        assert!(
            out.contains("assemble(graph: &Graph) -> String"),
            "signature must collapse to one line, got: {out:?}"
        );
        assert!(!out.contains("name: assemble"), "redundant Name row must be dropped");
        assert!(!out.to_lowercase().contains("visibility"), "Visibility row must be dropped");
    }

    #[test]
    fn test_strip_compacts_callee_table() {
        let input = "|===\n|Callee|Line\n|foo|12\n|bar|34\n|===";
        let out = strip_asciidoc_markup(input);
        assert!(!out.contains("|==="), "table delimiter must be removed");
        assert!(!out.contains("|Callee|Line"), "callee header must be removed");
        assert!(out.contains("calls:"), "callee table must produce calls: line");
        assert!(out.contains("foo(12)"), "callee with line number must be compacted");
        assert!(out.contains("bar(34)"), "callee with line number must be compacted");
    }

    #[test]
    fn test_strip_removes_edge_calls_lines() {
        let input = "Some text.\nedge::calls[Vec::new]\nedge::calls[foo]\nMore text.";
        let out = strip_asciidoc_markup(input);
        assert!(!out.contains("edge::calls"), "edge::calls lines must be removed");
        assert!(out.contains("Some text."), "surrounding text must be preserved");
        assert!(out.contains("More text."), "surrounding text must be preserved");
    }

    #[test]
    fn test_strip_converts_headings_to_plain_text() {
        let input = "= Top Title\n== Section One\n=== Subsection\n==== Deep";
        let out = strip_asciidoc_markup(input);
        assert!(out.contains("Top Title"), "top-level title must be kept");
        assert!(out.contains("Section One:"), "section heading must get colon suffix");
        assert!(out.contains("Subsection:"), "subsection must get colon suffix");
        assert!(out.contains("Deep:"), "deep heading must get colon suffix");
        // No leading '=' characters should remain in heading lines
        for line in out.lines() {
            assert!(
                !line.starts_with('='),
                "no line should start with '=' after stripping: {line:?}"
            );
        }
    }

    #[test]
    fn test_strip_replaces_xrefs_with_display_text() {
        let input = "See <<mod-aden-core,Core Module>> for details.";
        let out = strip_asciidoc_markup(input);
        assert!(out.contains("Core Module"), "display text must be kept");
        assert!(!out.contains("<<"), "xref syntax must be removed");
    }

    #[test]
    fn test_strip_replaces_bare_xrefs_with_anchor_name() {
        let input = "Defined in <<aden-graph>>.";
        let out = strip_asciidoc_markup(input);
        assert!(!out.contains("<<"), "xref syntax must be removed");
        // bare anchor becomes display text with dashes replaced by spaces
        assert!(out.contains("aden graph"), "dashes in bare xref should become spaces");
    }

    #[test]
    fn test_strip_collapses_multiple_blank_lines() {
        let input = "First.\n\n\n\nSecond.";
        let out = strip_asciidoc_markup(input);
        // Should have at most one blank line between paragraphs
        assert!(!out.contains("\n\n\n"), "multiple blanks must be collapsed to one");
        assert!(out.contains("First."));
        assert!(out.contains("Second."));
    }

    #[test]
    fn test_strip_trims_leading_and_trailing_blanks() {
        let input = "\n\n[[anchor]]\n\nContent here.\n\n";
        let out = strip_asciidoc_markup(input);
        assert!(!out.starts_with('\n'), "leading blank must be trimmed");
        assert!(!out.ends_with('\n'), "trailing blank must be trimmed");
        assert!(out.contains("Content here."));
    }

    #[test]
    fn test_strip_plain_prose_is_unchanged() {
        let input = "This is plain prose with no markup at all.";
        let out = strip_asciidoc_markup(input);
        assert_eq!(out, input);
    }
}
