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
use crate::extractor::{LanguageExtractor, build_code_attributes, make_anchor};
use aden_core::{Block, Document, FieldDef, NodeType, Parameter, Result, Visibility};
use std::path::Path;

/// Deep Rust extractor — implements `LanguageExtractor` for fully-resolved
/// call-site analysis, visibility, doc comments, and edge macros.
pub struct RustExtractor;

impl Default for RustExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl RustExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl LanguageExtractor for RustExtractor {
    fn language_id(&self) -> &'static str {
        "rust"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>> {
        extract_documents_inner(path, source)
    }
}

/// Extract Documents from a Rust source file using tree-sitter.
pub fn extract_documents_inner(path: &Path, source: &str) -> Result<Vec<Document>> {
    let language = tree_sitter_language_pack::get_language("rust")
        .map_err(|e| aden_core::Error::Parse(format!("language-pack: {}", e)))?;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language)
        .map_err(|e| aden_core::Error::Parse(e.to_string()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| aden_core::Error::Parse("tree-sitter returned None".to_string()))?;
    let root = tree.root_node();
    let crate_name = infer_crate_name(path);
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let mut docs = Vec::new();

    let mut cursor = root.walk();
    let children: Vec<_> = root.children(&mut cursor).collect();
    let mut buffered_comments: Vec<String> = Vec::new();

    for child in children {
        if !child.is_named() {
            continue;
        }
        match child.kind() {
            "line_comment" => {
                if let Some(comment) = process_line_comment(child, source) {
                    buffered_comments.push(comment);
                }
            }
            "block_comment" => {
                if let Some(comment) = process_block_comment(child, source) {
                    buffered_comments.push(comment);
                }
            }
            "function_item" | "function_signature_item" => {
                if let Some(doc) = extract_function(
                    child,
                    source,
                    path,
                    &crate_name,
                    &file_name,
                    &buffered_comments,
                ) {
                    docs.push(doc);
                }
                buffered_comments.clear();
            }
            "struct_item" => {
                if let Some(doc) = extract_struct(
                    child,
                    source,
                    path,
                    &crate_name,
                    &file_name,
                    &buffered_comments,
                ) {
                    docs.push(doc);
                }
                buffered_comments.clear();
            }
            "enum_item" => {
                if let Some(doc) = extract_enum(
                    child,
                    source,
                    path,
                    &crate_name,
                    &file_name,
                    &buffered_comments,
                ) {
                    docs.push(doc);
                }
                buffered_comments.clear();
            }
            "mod_item" => {
                if let Some(doc) = extract_module(
                    child,
                    source,
                    path,
                    &crate_name,
                    &file_name,
                    &buffered_comments,
                ) {
                    docs.push(doc);
                }
                buffered_comments.clear();
            }
            "trait_item" => {
                if let Some(doc) = extract_trait(
                    child,
                    source,
                    path,
                    &crate_name,
                    &file_name,
                    &buffered_comments,
                ) {
                    docs.push(doc);
                }
                buffered_comments.clear();
            }
            _ => {}
        }
    }
    Ok(docs)
}

fn infer_crate_name(path: &Path) -> String {
    let components: Vec<_> = path.components().collect();
    for (i, component) in components.iter().enumerate() {
        if let std::path::Component::Normal(name) = component
            && **name == *std::ffi::OsStr::new("crates")
            && i + 1 < components.len()
            && let std::path::Component::Normal(crate_name) = &components[i + 1]
        {
            return crate_name.to_string_lossy().to_string();
        }
    }
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn node_text<'a>(node: tree_sitter::Node<'a>, source: &'a str) -> &'a str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

/// Convert a tree-sitter node into an Aden SourceSpan.
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

fn get_visibility_with_source(node: tree_sitter::Node, source: &str) -> Visibility {
    if let Some(vis) = node.child_by_field_name("visibility_modifier") {
        let text = node_text(vis, source);
        if text.starts_with("pub(") {
            if text.contains("crate") {
                Visibility::Crate
            } else if text.contains("super") {
                Visibility::Super
            } else {
                Visibility::Public
            }
        } else {
            Visibility::Public
        }
    } else {
        Visibility::Private
    }
}

fn process_line_comment(node: tree_sitter::Node, source: &str) -> Option<String> {
    let text = node_text(node, source);
    if text.starts_with("///") && !text.starts_with("////") {
        Some(text.trim_start_matches("///").trim_start().to_string())
    } else {
        None
    }
}

fn process_block_comment(node: tree_sitter::Node, source: &str) -> Option<String> {
    let text = node_text(node, source);
    if text.starts_with("/**") && text.ends_with("*/") {
        let inner = &text[3..text.len() - 2];
        let lines: Vec<String> = inner
            .lines()
            .map(|l| {
                l.trim_start()
                    .trim_start_matches('*')
                    .trim_start()
                    .to_string()
            })
            .filter(|l| !l.is_empty())
            .collect();
        Some(lines.join("\n"))
    } else {
        None
    }
}

fn extract_function(
    node: tree_sitter::Node,
    source: &str,
    path: &Path,
    crate_name: &str,
    file_name: &str,
    buffered_comments: &[String],
) -> Option<Document> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    let vis = get_visibility_with_source(node, source);
    let mut is_async = false;
    let mut is_unsafe = false;
    if let Some(modifiers) = node.child_by_field_name("function_modifiers") {
        let mod_text = node_text(modifiers, source);
        if mod_text.contains("async") {
            is_async = true;
        }
        if mod_text.contains("unsafe") {
            is_unsafe = true;
        }
    }
    let mut params = Vec::new();
    if let Some(params_node) = node.child_by_field_name("parameters") {
        let mut pc = params_node.walk();
        for param in params_node.children(&mut pc) {
            if param.kind() == "parameter" {
                let pat = param.child_by_field_name("pattern")?;
                let ty = param.child_by_field_name("type")?;
                params.push(Parameter {
                    name: node_text(pat, source).to_string(),
                    ty: node_text(ty, source).to_string(),
                    default_value: None,
                });
            } else if param.kind() == "self_parameter" {
                params.push(Parameter {
                    name: "self".to_string(),
                    ty: node_text(param, source).to_string(),
                    default_value: None,
                });
            }
        }
    }
    let return_type = node
        .child_by_field_name("return_type")
        .map(|n| node_text(n, source).to_string());
    let doc_comment = if buffered_comments.is_empty() {
        None
    } else {
        Some(buffered_comments.join("\n"))
    };
    let anchor = make_anchor(crate_name, file_name, name);
    let span = node_to_span(node, path);
    let attrs = build_code_attributes(source, "function", Some(path), Some(&span));
    let mut blocks = Vec::new();
    if let Some(doc) = doc_comment {
        blocks.push(Block::Paragraph(doc));
    }
    let mut sig_rows = vec![
        vec!["Name".to_string(), name.to_string()],
        vec!["Visibility".to_string(), format!("{:?}", vis)],
    ];
    if is_async {
        sig_rows.push(vec!["Async".to_string(), "true".to_string()]);
    }
    if is_unsafe {
        sig_rows.push(vec!["Unsafe".to_string(), "true".to_string()]);
    }
    for p in &params {
        sig_rows.push(vec![
            format!("param {}", p.name),
            format!("{}: {}", p.name, p.ty),
        ]);
    }
    if let Some(ref rt) = return_type {
        sig_rows.push(vec!["Returns".to_string(), rt.clone()]);
    }
    blocks.push(Block::Paragraph("== Signature".to_string()));
    blocks.push(Block::Table(aden_core::Table {
        headers: vec!["Property".to_string(), "Value".to_string()],
        rows: sig_rows,
    }));
    // Extract call sites from function body
    if let Some(body) = node.child_by_field_name("body") {
        let calls = extract_call_sites(body, source);
        let filtered: Vec<_> = calls
            .into_iter()
            .filter(|(c, _)| !is_std_noise(c))
            .collect();
        if !filtered.is_empty() {
            let call_rows: Vec<Vec<String>> = filtered
                .iter()
                .map(|(callee, line)| vec![callee.clone(), line.to_string()])
                .collect();
            blocks.push(Block::Table(aden_core::Table {
                headers: vec!["Callee".to_string(), "Line".to_string()],
                rows: call_rows,
            }));
            // Emit typed edge macros as a listing block for graph ingestion
            let edge_code: String = filtered
                .iter()
                .map(|(callee, _)| format!("edge::calls[{}]", callee))
                .collect::<Vec<_>>()
                .join("\n");
            blocks.push(Block::Listing {
                language: None,
                code: edge_code,
            });
        }
    }

    if !buffered_comments.is_empty() {
        blocks.push(Block::Admonition {
            kind: aden_core::AdmonitionKind::Note,
            text: "Extracted from source code via tree-sitter. Confidence is heuristic."
                .to_string(),
        });
    }
    Some(Document {
        anchor,
        node_type: NodeType::Type,
        attributes: attrs,
        blocks,
        source_span: None,
    })
}

/// Standard-library and common utility functions to exclude from call-graph extraction.
/// These generate noise (to_string, push, unwrap, etc.) without meaningful cross-module edges.
const SKIP_CALLEES: &[&str] = &[
    "to_string",
    "to_string_lossy",
    "to_str",
    "to_path_buf",
    "to_owned",
    "clone",
    "copy",
    "eq",
    "ne",
    "partial_cmp",
    "cmp",
    "push",
    "pop",
    "insert",
    "remove",
    "clear",
    "extend",
    "append",
    "map",
    "filter",
    "fold",
    "collect",
    "join",
    "split",
    "iter",
    "into_iter",
    "contains",
    "is_empty",
    "len",
    "get",
    "get_mut",
    "entry",
    "unwrap",
    "unwrap_or",
    "unwrap_or_else",
    "expect",
    "ok",
    "err",
    "map_err",
    "new",
    "default",
    "from",
    "into",
    "try_from",
    "try_into",
    "parse",
    "format",
    "write",
    "writeln",
    "print",
    "println",
    "eprintln",
    "walk",
    "children",
    "goto_first_child",
    "goto_next_sibling",
    "goto_parent",
    "kind",
    "utf8_text",
    "start_position",
    "end_position",
    "start_byte",
    "end_byte",
    "is_named",
    "is_ok",
    "is_err",
    "is_some",
    "is_none",
    "as_ref",
    "as_mut",
    "as_str",
    "as_bytes",
    "as_path",
    "chars",
    "lines",
    "bytes",
    "trim",
    "trim_start",
    "trim_end",
    "read_to_string",
    "read_dir",
    "read",
    "write_all",
    "create_dir_all",
    "canonicalize",
    "join",
    "parent",
    "extension",
    "file_name",
    "file_stem",
];

fn is_std_noise(name: &str) -> bool {
    SKIP_CALLEES.contains(&name)
}

/// Recursively walk an AST subtree and collect all `call_expression` nodes.
/// Returns a list of (callee_name, 1-based_line_number) for each *meaningful* call found.
/// Filters out std-lib noise and very short names.
fn extract_call_sites(node: tree_sitter::Node, source: &str) -> Vec<(String, usize)> {
    let mut calls = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        calls.extend(extract_call_sites(child, source));
    }
    if node.kind() == "call_expression"
        && let Some(func) = node.child_by_field_name("function")
    {
        let callee = resolve_callee_name(func, source);
        if !callee.is_empty() && callee.len() >= 3 && !SKIP_CALLEES.contains(&callee.as_str()) {
            let line = func.start_position().row + 1;
            calls.push((callee, line));
        }
    }
    calls
}

fn resolve_callee_name(node: tree_sitter::Node, source: &str) -> String {
    match node.kind() {
        "identifier" => node_text(node, source).to_string(),
        "field_expression" => {
            if let Some(name) = node.child_by_field_name("field") {
                node_text(name, source).to_string()
            } else {
                node_text(node, source).to_string()
            }
        }
        "scoped_identifier" => {
            let mut name_parts = Vec::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "identifier" {
                    name_parts.push(node_text(child, source));
                }
            }
            if name_parts.len() >= 2 {
                format!(
                    "{}::{}",
                    name_parts[name_parts.len() - 2],
                    name_parts[name_parts.len() - 1]
                )
            } else if !name_parts.is_empty() {
                name_parts.last().unwrap().to_string()
            } else {
                node_text(node, source).to_string()
            }
        }
        _ => node_text(node, source).to_string(),
    }
}

fn extract_struct(
    node: tree_sitter::Node,
    source: &str,
    path: &Path,
    crate_name: &str,
    file_name: &str,
    buffered_comments: &[String],
) -> Option<Document> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    let _vis = get_visibility_with_source(node, source);
    let mut fields = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        let mut bc = body.walk();
        for child in body.children(&mut bc) {
            if child.kind() == "field_declaration"
                && let Some(fname) = child.child_by_field_name("name")
                && let Some(fty) = child.child_by_field_name("type")
            {
                let f_vis = get_visibility_with_source(child, source);
                fields.push(FieldDef {
                    name: node_text(fname, source).to_string(),
                    ty: node_text(fty, source).to_string(),
                    visibility: f_vis,
                });
            }
        }
    }
    let doc_comment = if buffered_comments.is_empty() {
        None
    } else {
        Some(buffered_comments.join("\n"))
    };
    let anchor = make_anchor(crate_name, file_name, name);
    let span = node_to_span(node, path);
    let attrs = build_code_attributes(source, "type", Some(path), Some(&span));
    let mut blocks = Vec::new();
    if let Some(doc) = doc_comment {
        blocks.push(Block::Paragraph(doc));
    }
    let mut rows: Vec<Vec<String>> = vec![vec!["Kind".to_string(), "Struct".to_string()]];
    for f in &fields {
        rows.push(vec![
            format!("field {}", f.name),
            format!("{} (vis: {:?})", f.ty, f.visibility),
        ]);
    }
    blocks.push(Block::Table(aden_core::Table {
        headers: vec!["Property".to_string(), "Value".to_string()],
        rows,
    }));
    if !buffered_comments.is_empty() {
        blocks.push(Block::Admonition {
            kind: aden_core::AdmonitionKind::Note,
            text: "Extracted from source code via tree-sitter. Confidence is heuristic."
                .to_string(),
        });
    }
    Some(Document {
        anchor,
        node_type: NodeType::Type,
        attributes: attrs,
        blocks,
        source_span: None,
    })
}

fn extract_enum(
    node: tree_sitter::Node,
    source: &str,
    path: &Path,
    crate_name: &str,
    file_name: &str,
    buffered_comments: &[String],
) -> Option<Document> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    let vis = get_visibility_with_source(node, source);
    let mut variants = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        let mut bc = body.walk();
        for child in body.children(&mut bc) {
            if child.kind() == "enum_variant"
                && let Some(vname) = child.child_by_field_name("name")
            {
                variants.push(FieldDef {
                    name: node_text(vname, source).to_string(),
                    ty: String::new(),
                    visibility: vis.clone(),
                });
            }
        }
    }
    let doc_comment = if buffered_comments.is_empty() {
        None
    } else {
        Some(buffered_comments.join("\n"))
    };
    let anchor = make_anchor(crate_name, file_name, name);
    let span = node_to_span(node, path);
    let attrs = build_code_attributes(source, "type", Some(path), Some(&span));
    let mut blocks = Vec::new();
    if let Some(doc) = doc_comment {
        blocks.push(Block::Paragraph(doc));
    }
    let mut rows: Vec<Vec<String>> = vec![vec!["Kind".to_string(), "Enum".to_string()]];
    for v in &variants {
        rows.push(vec![format!("variant {}", v.name), v.name.clone()]);
    }
    blocks.push(Block::Table(aden_core::Table {
        headers: vec!["Property".to_string(), "Value".to_string()],
        rows,
    }));
    if !buffered_comments.is_empty() {
        blocks.push(Block::Admonition {
            kind: aden_core::AdmonitionKind::Note,
            text: "Extracted from source code via tree-sitter. Confidence is heuristic."
                .to_string(),
        });
    }
    Some(Document {
        anchor,
        node_type: NodeType::Type,
        attributes: attrs,
        blocks,
        source_span: None,
    })
}

fn extract_module(
    node: tree_sitter::Node,
    source: &str,
    path: &Path,
    crate_name: &str,
    file_name: &str,
    buffered_comments: &[String],
) -> Option<Document> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    let anchor = make_anchor(crate_name, file_name, name);
    let span = node_to_span(node, path);
    let attrs = build_code_attributes(source, "module", Some(path), Some(&span));
    let mut blocks = Vec::new();
    if !buffered_comments.is_empty() {
        blocks.push(Block::Paragraph(buffered_comments.join("\n")));
    }
    blocks.push(Block::Paragraph(format!("Module declaration for `{name}`")));
    Some(Document {
        anchor,
        node_type: NodeType::Module,
        attributes: attrs,
        blocks,
        source_span: None,
    })
}

fn extract_trait(
    node: tree_sitter::Node,
    source: &str,
    path: &Path,
    crate_name: &str,
    file_name: &str,
    buffered_comments: &[String],
) -> Option<Document> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    let anchor = make_anchor(crate_name, file_name, name);
    let span = node_to_span(node, path);
    let attrs = build_code_attributes(source, "type", Some(path), Some(&span));
    let mut blocks = Vec::new();
    if !buffered_comments.is_empty() {
        blocks.push(Block::Paragraph(buffered_comments.join("\n")));
    }
    blocks.push(Block::Paragraph(format!("Trait `{name}`")));
    Some(Document {
        anchor,
        node_type: NodeType::Type,
        attributes: attrs,
        blocks,
        source_span: None,
    })
}
