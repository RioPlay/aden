// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Generic language extractor powered by `tree-sitter-language-pack`.
//!
//! This extractor works for **any** language the pack advertises (305+)
//! by walking the AST with a cross-language symbol-detection heuristic.
//! It is intentionally shallow: it emits `Document`s for functions,
//! classes, structs, and modules, but does **not** resolve cross-file
//! call sites.  That is the job of deep language resolvers (Phase 2).

use crate::extractor::{LanguageExtractor, build_code_attributes, infer_project_name, make_anchor};
use aden_core::{Block, Document, NodeType, Result};
use std::path::Path;

/// Node kinds that represent functions across tree-sitter grammars.
const FUNCTION_KINDS: &[&str] = &[
    "function_item",
    "function_definition",
    "function_declaration",
    "method_definition",
    "method_declaration",
    "arrow_function",
    "generator_function",
    "generator_function_declaration",
    "function",
    "method",
    "singleton_method",
    // PowerShell (airbus-cert grammar)
    "function_statement",
    "class_method_definition",
];

/// Node kinds that represent types/classes/structs.
const TYPE_KINDS: &[&str] = &[
    "struct_item",
    "struct_declaration",
    "struct_type",
    "struct_specifier",
    "class_definition",
    "class_declaration",
    "class",
    "interface_declaration",
    "interface_type",
    "protocol_declaration",
    "enum_item",
    "enum_declaration",
    "enum_specifier",
    "enum_class_declaration",
    "type_declaration",
    "type_definition",
    "type_spec",
    "type_alias_item",
    "type_alias_declaration",
    "trait_item",
    "trait_declaration",
    "impl_item",
    "impl_declaration",
    "record_declaration",
    "object_declaration",
    "union_specifier",
    "component_declaration",
    // PowerShell (airbus-cert grammar)
    "class_statement",
];

/// Node kinds that represent modules/namespaces/packages.
const MODULE_KINDS: &[&str] = &[
    "mod_item",
    "module_declaration",
    "namespace_declaration",
    "package_declaration",
    "source_file",
    "translation_unit",
    "module",
];

/// A shallow extractor for any language supported by tree-sitter-language-pack.
pub struct GenericExtractor {
    language_id: &'static str,
}

impl GenericExtractor {
    pub fn new(language_id: &'static str) -> Self {
        Self { language_id }
    }
}

impl LanguageExtractor for GenericExtractor {
    fn language_id(&self) -> &'static str {
        self.language_id
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        // Generic extractor is instantiated per-language at runtime;
        // this slice is intentionally empty because the Router maps
        // extensions to `new(lang)` dynamically.
        &[]
    }

    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>> {
        // Attempt to get a tree-sitter Language from the pack.
        // If the pack does not include this language, return empty gracefully.
        let language = match tree_sitter_language_pack::get_language(self.language_id) {
            Ok(l) => l,
            Err(_e) => {
                return Ok(Vec::new());
            }
        };

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language)
            .map_err(|e| aden_core::Error::Parse(format!("tree-sitter: {}", e)))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| aden_core::Error::Parse("tree-sitter returned None".to_string()))?;

        let crate_name = infer_project_name(path);
        let file_name = path.file_name().unwrap_or_default().to_string_lossy();
        let mut docs = Vec::new();

        // Walk the AST and collect symbols.
        walk_tree(
            &tree.root_node(),
            source,
            path,
            &crate_name,
            &file_name,
            &mut docs,
        );

        Ok(docs)
    }
}

/// Recursively walk an AST and emit Documents for recognised node kinds.
fn walk_tree(
    node: &tree_sitter::Node,
    source: &str,
    path: &Path,
    crate_name: &str,
    file_name: &str,
    docs: &mut Vec<Document>,
) {
    if !node.is_named() {
        return;
    }

    let kind = node.kind();

    // Check if this node is a symbol we care about.
    let node_type = if FUNCTION_KINDS.contains(&kind) {
        Some(NodeType::Function)
    } else if TYPE_KINDS.contains(&kind) {
        Some(NodeType::Type)
    } else if MODULE_KINDS.contains(&kind) {
        Some(NodeType::Module)
    } else {
        None
    };

    if let Some(nt) = node_type
        && let Some(name) = extract_node_name(*node, source)
    {
        let anchor = make_anchor(crate_name, file_name, &name);
        let span = node_to_span(*node, path);
        let attrs = build_code_attributes(
            source,
            &format!("{:?}", nt).to_lowercase(),
            Some(path),
            Some(&span),
        );
        let blocks = vec![Block::Paragraph(format!(
            "Extracted from {} source via generic AST walker.",
            crate_name
        ))];
        docs.push(Document {
            anchor,
            node_type: nt,
            attributes: attrs,
            blocks,
            source_span: Some(span),
            metadata: None,
            confidence: 0.9,
        });
    }

    // Recurse into children.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree(&child, source, path, crate_name, file_name, docs);
    }
}

/// Best-effort name extraction from a definition node.
/// Tries `child_by_field_name("name")`, then `identifier` children,
/// then `declarator` sub-trees.
fn extract_node_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    // Most grammars use a "name" field.
    if let Some(name_node) = node.child_by_field_name("name") {
        let text = node_text(name_node, source).trim();
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }

    // Fallback: look for any direct child whose kind contains "identifier"
    // (type_identifier, field_identifier, property_identifier, etc.), or a
    // grammar-specific name node (PowerShell: function_name / simple_name).
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let ck = child.kind();
        if ck == "identifier"
            || ck.ends_with("_identifier")
            || ck == "function_name"
            || ck == "simple_name"
        {
            let text = node_text(child, source).trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }

    // C-style: declarator → identifier
    if let Some(decl) = node.child_by_field_name("declarator") {
        return extract_node_name(decl, source);
    }

    None
}

fn node_text<'a>(node: tree_sitter::Node<'a>, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

fn node_to_span(node: tree_sitter::Node, path: &Path) -> aden_core::SourceSpan {
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
