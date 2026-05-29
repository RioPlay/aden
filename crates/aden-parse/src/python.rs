// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Python source extraction via tree-sitter.
//! Proof of concept for the extensible language adapter architecture.

use crate::{build_code_attributes, make_anchor, tree_sitter_common};
use aden_core::{Block, Document, NodeType, Parameter, Result, Visibility};
use std::path::Path;

/// Extract Documents from a Python source file.
/// Extracts: functions, async functions, classes, methods.
pub fn extract_documents(path: &Path, source: &str) -> Result<Vec<Document>> {
    #[cfg(not(feature = "python-parser"))]
    {
        // Graceful degradation: if python-parser feature is not enabled,
        // return a minimal stub document so the file is tracked.
        let anchor = make_anchor("python-module", path.file_name().unwrap_or_default().to_str().unwrap_or("unnamed"), "module");
        let attrs = build_code_attributes(source, "module", Some(path), None);
        return Ok(vec![Document {
            anchor,
            node_type: NodeType::Module,
            attributes: attrs,
            blocks: vec![Block::Paragraph("Python source file (python-parser feature not enabled).".to_string())],
            source_span: None,
        ,
        confidence: 0.9,}]);
    }

    #[cfg(feature = "python-parser")]
    {
        let mut parser = tree_sitter::Parser::new();
        let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
        parser
            .set_language(&language)
            .map_err(|e| aden_core::Error::Parse(format!("tree-sitter python: {}", e)))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| aden_core::Error::Parse("tree-sitter returned None".to_string()))?;

        let proj_name = tree_sitter_common::infer_project_name(path);
        let file_name = path.file_name().unwrap_or_default().to_string_lossy();
        let mut docs = Vec::new();

        // Extract functions
        let func_query = r#"
            (function_definition
              name: (identifier) @func_name
              parameters: (parameters) @func_params
              body: (block) @func_body) @func
            (decorated_definition
              definition: (function_definition
                name: (identifier) @func_name
                parameters: (parameters) @func_params
                body: (block) @func_body) @func)
        "#;
        let funcs = tree_sitter_common::query_nodes(&tree, source, &language, func_query);
        let mut seen_funcs: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (capture, node) in funcs {
            if capture == "func_name" {
                let name = tree_sitter_common::node_text(node, source);
                if seen_funcs.insert(name.to_string()) {
                    let params = extract_python_params(&node, source);
                    let sig_rows = params.iter().map(|p| vec![format!("param {}", p.name), format!("type: {}", p.type_hint.as_deref().unwrap_or("Unknown"))]).collect();
                    let span = tree_sitter_common::node_to_span(node, path);
                    let mut blocks: Vec<Block> = Vec::new();
                    if !sig_rows.is_empty() {
                        blocks.push(Block::Table(aden_core::Table {
                            headers: vec!["Property".to_string(), "Value".to_string()],
                            rows: sig_rows,
                        }));
                    }
                    let anchor = make_anchor(&proj_name, &file_name, name);
                    let attrs = build_code_attributes(source, "function", Some(path), Some(&span));
                    docs.push(Document { anchor, node_type: NodeType::Function, attributes: attrs, blocks, source_span: Some(span) ,
        confidence: 0.9,});
                }
            }
        }

        // Extract classes
        let class_query = r#"
            (class_definition
              name: (identifier) @class_name
              body: (block) @class_body) @class
        "#;
        let classes = tree_sitter_common::query_nodes(&tree, source, &language, class_query);
        let mut seen_classes: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (capture, node) in classes {
            if capture == "class_name" {
                let name = tree_sitter_common::node_text(node, source);
                if seen_classes.insert(name.to_string()) {
                    let span = tree_sitter_common::node_to_span(node, path);
                    let anchor = make_anchor(&proj_name, &file_name, name);
                    let attrs = build_code_attributes(source, "type", Some(path), Some(&span));
                    docs.push(Document {
                        anchor,
                        node_type: NodeType::Struct, // closest mapping
                        attributes: attrs,
                        blocks: vec![],
                        source_span: Some(span),
                    ,
        confidence: 0.9,});
                }
            }
        }

        Ok(docs)
    }
}

#[cfg(feature = "python-parser")]
fn extract_python_params(func_name_node: &tree_sitter::Node, source: &str) -> Vec<Parameter> {
    let mut params = Vec::new();
    let mut parent = func_name_node.parent();
    while let Some(p) = parent {
        if p.kind() == "function_definition" || p.kind() == "decorated_definition" {
            if let Some(params_node) = p.child_by_field_name("parameters") {
                let mut cursor = params_node.walk();
                for child in params_node.children(&mut cursor) {
                    if child.kind() == "identifier" {
                        let name = tree_sitter_common::node_text(child, source).to_string();
                        params.push(Parameter { name, type_hint: None, default_value: None });
                    } else if child.kind() == "typed_parameter" {
                        let name = if let Some(id) = child.child_by_field_name("name") {
                            tree_sitter_common::node_text(id, source).to_string()
                        } else {
                            tree_sitter_common::node_text(child, source).to_string()
                        };
                        let type_hint = child.children(& mut child.walk()).find(|n| n.kind() == "type").map(|n| tree_sitter_common::node_text(n, source).to_string());
                        params.push(Parameter { name, type_hint, default_value: None });
                    }
                }
            }
            break;
        }
        parent = p.parent();
    }
    params
}
