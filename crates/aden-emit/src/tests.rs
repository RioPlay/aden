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
    use super::super::*;
    use std::collections::HashMap;

    #[test]
    fn test_module_contract_builder() {
        let attrs = HashMap::new();
        let doc = templates::module_contract(
            "mod-foo",
            "module::foo",
            vec![vec!["Foo".to_string(), "struct".to_string(), "A foo".to_string()]],
            vec!["Invariant 1".to_string()],
            vec!["Error 1".to_string()],
            vec!["Side effect 1".to_string()],
            attrs,
        );
        assert_eq!(doc.anchor, "mod-foo");
        assert_eq!(doc.node_type, aden_core::NodeType::Module);
        assert!(!doc.blocks.is_empty());
    }

    #[test]
    fn test_emit_document_has_anchor() {
        let mut attrs = HashMap::new();
        attrs.insert("source_hash".to_string(), "abc123".to_string());
        let doc = aden_core::Document {
            anchor: "test-anchor".to_string(),
            node_type: aden_core::NodeType::Note,
            attributes: attrs,
            blocks: vec![aden_core::Block::Paragraph("Hello".to_string())],
        };
        let out = emit_document(&doc);
        assert!(out.contains("[[test-anchor]]"));
        assert!(out.contains("= test-anchor"));
        assert!(out.contains(":source_hash: abc123"));
    }

    #[test]
    fn test_emit_table_has_header() {
        let table = aden_core::Table {
            headers: vec!["A".to_string(), "B".to_string()],
            rows: vec![vec!["1".to_string(), "2".to_string()]],
        };
        let doc = aden_core::Document {
            anchor: "table-test".to_string(),
            node_type: aden_core::NodeType::Note,
            attributes: HashMap::new(),
            blocks: vec![aden_core::Block::Table(table)],
        };
        let out = emit_document(&doc);
        assert!(out.contains("|==="));
        assert!(out.contains("|A|B"));
        assert!(out.contains("|1|2"));
    }
}
