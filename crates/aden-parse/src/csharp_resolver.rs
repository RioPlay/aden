// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Deep C# resolver — symbol + call-site analysis.
//!
//! Handles:
//!   • Class, interface, enum, struct declarations
//!   • Method declarations, constructors, properties
//!   • Extension methods (best-effort detection)
//!   • Intra-file and cross-module call resolution (best-effort)
//!   • Emits `edge::calls[]` macros for graph ingestion
//!
//! Phase 2 second pass (not yet implemented):
//!   • MSBuild/Project path resolution
//!   • `partial` class aggregation
//!   • Generic constraint tracking

use crate::extractor::{
    LanguageExtractor, build_code_attributes, infer_project_root, make_anchor,
    project_relative_file,
};
use aden_core::{Block, Document, NodeType, Parameter, Result};
use std::path::Path;

/// Deep C# extractor.
pub struct CSharpResolver;

impl Default for CSharpResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl CSharpResolver {
    pub fn new() -> Self {
        Self
    }
}

impl LanguageExtractor for CSharpResolver {
    fn language_id(&self) -> &'static str {
        "csharp"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["cs"]
    }

    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>> {
        let language = tree_sitter_language_pack::get_language("csharp")
            .map_err(|e| aden_core::Error::Parse(format!("language-pack: {}", e)))?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language)
            .map_err(|e| aden_core::Error::Parse(e.to_string()))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| aden_core::Error::Parse("tree-sitter returned None".to_string()))?;

        let namespace = infer_cs_namespace(path, source);
        let project_root = infer_project_root(path);
        let file_name_owned = project_relative_file(path, &project_root);
        let file_name = file_name_owned.as_str();

        let mut symbols: Vec<CsSymbol> = Vec::new();
        walk_compilation_unit(
            tree.root_node(),
            source,
            &namespace,
            file_name,
            &mut symbols,
        );

        let mut docs = Vec::new();
        for sym in &symbols {
            if let Some(doc) = emit_cs_symbol(sym, source, path, &symbols, &namespace, file_name) {
                docs.push(doc);
            }
        }

        Ok(docs)
    }
}

#[derive(Debug)]
struct CsSymbol<'a> {
    name: String,
    qualified_name: String,
    kind: NodeType,
    node: tree_sitter::Node<'a>,
    params: Vec<Parameter>,
    return_type: Option<String>,
    doc_comment: Option<String>,
    visibility: String,
    is_static: bool,
}

fn infer_cs_namespace(path: &Path, source: &str) -> String {
    if let Some(ns) = extract_namespace_from_source(source) {
        return ns;
    }
    let path_str = path.to_string_lossy();
    // Common C# source paths: src/MyProject/Models/Foo.cs -> MyProject.Models
    if let Some(idx) = path_str.rfind("/src/") {
        let start = idx + 5;
        let end = path_str.rfind('/').unwrap_or(path_str.len()).max(start);
        let after = &path_str[start..end];
        return after.replace('/', ".");
    }
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn extract_namespace_from_source(source: &str) -> Option<String> {
    // C# files may have one or more namespace declarations
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("namespace ") {
            let ns = trimmed.strip_prefix("namespace ").unwrap().trim();
            // Remove { if present: "namespace Foo {"
            let ns_clean = ns.trim_end_matches('{').trim();
            return Some(ns_clean.to_string());
        }
    }
    None
}

fn node_text<'a>(node: tree_sitter::Node<'a>, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

fn node_to_span(node: tree_sitter::Node, path: &Path) -> aden_core::SourceSpan {
    let s = node.start_position();
    let e = node.end_position();
    aden_core::SourceSpan {
        file: path.to_string_lossy().to_string(),
        start_line: s.row + 1,
        end_line: e.row + 1,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    }
}

fn extract_doc_comment(node: tree_sitter::Node, source: &str) -> Option<String> {
    let node_start = node.start_byte();
    let text_before = &source[..node_start];
    let lines: Vec<&str> = text_before.lines().rev().collect();
    let mut comments = Vec::new();
    for line in &lines {
        let trimmed = line.trim();
        if trimmed.starts_with("///") {
            let content = trimmed.trim_start_matches('/').trim_start();
            comments.push(content.to_string());
        } else if !trimmed.is_empty() {
            break;
        }
    }
    if comments.is_empty() {
        None
    } else {
        Some(comments.into_iter().rev().collect::<Vec<_>>().join("\n"))
    }
}

fn extract_visibility(node: tree_sitter::Node, source: &str) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let text = node_text(child, source);
        if matches!(
            text.trim(),
            "public"
                | "private"
                | "protected"
                | "internal"
                | "file"
                | "protected internal"
                | "private protected"
        ) {
            return text.trim().to_string();
        }
    }
    // Default for classes is internal, for members is private
    "private".to_string()
}

fn has_modifier(node: tree_sitter::Node, source: &str, modifier: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let text = node_text(child, source);
        if text.trim() == modifier {
            return true;
        }
        // Check modifier_list / attribute_list children
        if child.kind() == "modifier" || child.kind() == "modifier_list" {
            let mut cc = child.walk();
            for mod_child in child.children(&mut cc) {
                if node_text(mod_child, source).trim() == modifier {
                    return true;
                }
            }
        }
    }
    false
}

fn walk_compilation_unit<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    namespace: &str,
    file_name: &str,
    symbols: &mut Vec<CsSymbol<'a>>,
) {
    if !node.is_named() {
        return;
    }

    match node.kind() {
        "class_declaration"
        | "interface_declaration"
        | "enum_declaration"
        | "struct_declaration"
        | "record_declaration"
        | "record_struct_declaration" => {
            parse_type_declaration(node, source, namespace, file_name, symbols);
        }
        "delegate_declaration" => {
            parse_delegate(node, source, namespace, file_name, symbols);
        }
        "method_declaration" | "local_function_statement" => {
            parse_method(node, source, namespace, file_name, symbols);
        }
        "constructor_declaration" => {
            parse_constructor(node, source, namespace, file_name, symbols);
        }
        "property_declaration" => {
            parse_property(node, source, namespace, file_name, symbols);
        }
        "field_declaration" => {
            parse_field(node, source, namespace, file_name, symbols);
        }
        "compilation_unit"
        | "namespace_declaration"
        | "namespace_block"
        | "class_body"
        | "interface_body"
        | "struct_body"
        | "statement"
        | "block"
        | "declaration_list" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_compilation_unit(child, source, namespace, file_name, symbols);
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named() {
                    walk_compilation_unit(child, source, namespace, file_name, symbols);
                }
            }
        }
    }
}

fn parse_type_declaration<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    namespace: &str,
    file_name: &str,
    symbols: &mut Vec<CsSymbol<'a>>,
) {
    if let Some(name_node) = node.child_by_field_name("name") {
        let name = node_text(name_node, source).to_string();
        let vis = extract_visibility(node, source);
        let is_static = has_modifier(node, source, "static");

        let qname = format!("{}.{}", namespace, name);
        symbols.push(CsSymbol {
            name: name.clone(),
            qualified_name: qname,
            kind: NodeType::Type,
            node,
            params: Vec::new(),
            return_type: None,
            doc_comment: extract_doc_comment(node, source),
            visibility: vis,
            is_static,
        });

        // Walk body
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if matches!(
                child.kind(),
                "class_body"
                    | "interface_body"
                    | "struct_body"
                    | "enum_body"
                    | "record_declaration_body"
            ) {
                let mut cc = child.walk();
                for grandchild in child.children(&mut cc) {
                    if grandchild.is_named() {
                        walk_compilation_unit(grandchild, source, namespace, file_name, symbols);
                    }
                }
            }
        }
    }
}

fn parse_method<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    namespace: &str,
    _file_name: &str,
    symbols: &mut Vec<CsSymbol<'a>>,
) {
    if let Some(name_node) = node.child_by_field_name("name") {
        let name = node_text(name_node, source).to_string();
        let vis = extract_visibility(node, source);
        let is_static = has_modifier(node, source, "static");

        let mut params = Vec::new();
        if let Some(params_node) = node.child_by_field_name("parameters") {
            let mut pc = params_node.walk();
            for param in params_node.children(&mut pc) {
                if param.kind() == "parameter" {
                    let param_type = param
                        .child_by_field_name("type")
                        .map(|n| node_text(n, source).to_string())
                        .unwrap_or_default();
                    let param_name = param
                        .child_by_field_name("name")
                        .map(|n| node_text(n, source).to_string())
                        .unwrap_or_default();
                    params.push(Parameter {
                        name: param_name,
                        ty: param_type,
                        default_value: None,
                    });
                }
            }
        }

        // Return type: `method_declaration` uses the `returns` field, while
        // `local_function_statement` uses `type` — accept either.
        let return_type = node
            .child_by_field_name("returns")
            .or_else(|| node.child_by_field_name("type"))
            .map(|n| node_text(n, source).to_string());

        let class_name = find_parent_type_name(node, source).unwrap_or_default();
        let qname = if class_name.is_empty() {
            format!("{}.{}", namespace, name)
        } else {
            format!("{}.{}/{}", namespace, class_name, name)
        };

        symbols.push(CsSymbol {
            name,
            qualified_name: qname,
            kind: NodeType::Function,
            node,
            params,
            return_type,
            doc_comment: extract_doc_comment(node, source),
            visibility: vis,
            is_static,
        });
    }
}

fn parse_constructor<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    namespace: &str,
    _file_name: &str,
    symbols: &mut Vec<CsSymbol<'a>>,
) {
    let class_name = find_parent_type_name(node, source).unwrap_or_else(|| "<init>".to_string());
    let vis = extract_visibility(node, source);

    let mut params = Vec::new();
    if let Some(params_node) = node.child_by_field_name("parameters") {
        let mut pc = params_node.walk();
        for param in params_node.children(&mut pc) {
            if param.kind() == "parameter" {
                let param_type = param
                    .child_by_field_name("type")
                    .map(|n| node_text(n, source).to_string())
                    .unwrap_or_default();
                let param_name = param
                    .child_by_field_name("name")
                    .map(|n| node_text(n, source).to_string())
                    .unwrap_or_default();
                params.push(Parameter {
                    name: param_name,
                    ty: param_type,
                    default_value: None,
                });
            }
        }
    }

    let qname = format!("{}.{}/.ctor", namespace, class_name);
    symbols.push(CsSymbol {
        name: ".ctor".to_string(),
        qualified_name: qname,
        kind: NodeType::Function,
        node,
        params,
        return_type: None,
        doc_comment: extract_doc_comment(node, source),
        visibility: vis,
        is_static: false,
    });
}

fn parse_delegate<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    namespace: &str,
    _file_name: &str,
    symbols: &mut Vec<CsSymbol<'a>>,
) {
    if let Some(name_node) = node.child_by_field_name("name") {
        let name = node_text(name_node, source).to_string();
        let vis = extract_visibility(node, source);
        let is_static = has_modifier(node, source, "static");

        let mut params = Vec::new();
        if let Some(params_node) = node.child_by_field_name("parameters") {
            let mut pc = params_node.walk();
            for param in params_node.children(&mut pc) {
                if param.kind() == "parameter" {
                    let param_type = param
                        .child_by_field_name("type")
                        .map(|n| node_text(n, source).to_string())
                        .unwrap_or_default();
                    let param_name = param
                        .child_by_field_name("name")
                        .map(|n| node_text(n, source).to_string())
                        .unwrap_or_default();
                    params.push(Parameter {
                        name: param_name,
                        ty: param_type,
                        default_value: None,
                    });
                }
            }
        }

        // Delegate return type: `delegate_declaration` uses the `type` field
        // (`returns` in some grammar versions) — accept either.
        let return_type = node
            .child_by_field_name("type")
            .or_else(|| node.child_by_field_name("returns"))
            .map(|n| node_text(n, source).to_string());

        let qname = format!("{}.{}", namespace, name);
        symbols.push(CsSymbol {
            name,
            qualified_name: qname,
            kind: NodeType::Type,
            node,
            params,
            return_type,
            doc_comment: extract_doc_comment(node, source),
            visibility: vis,
            is_static,
        });
    }
}

fn parse_property<'a>(
    _node: tree_sitter::Node<'a>,
    _source: &str,
    _namespace: &str,
    _file_name: &str,
    _symbols: &mut Vec<CsSymbol<'a>>,
) {
    // Properties are intentionally not emitted as standalone Documents.
    // Their descriptions live inside the parent Type document.
}

fn parse_field<'a>(
    _node: tree_sitter::Node<'a>,
    _source: &str,
    _namespace: &str,
    _file_name: &str,
    _symbols: &mut Vec<CsSymbol<'a>>,
) {
    // Fields are intentionally not emitted as standalone Documents.
    // Their descriptions live inside the parent Type document.
}

fn find_parent_type_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if matches!(
            parent.kind(),
            "class_declaration"
                | "interface_declaration"
                | "struct_declaration"
                | "record_declaration"
                | "record_struct_declaration"
        ) && let Some(name_node) = parent.child_by_field_name("name")
        {
            return Some(node_text(name_node, source).to_string());
        }
        current = parent;
    }
    None
}

fn emit_cs_symbol(
    sym: &CsSymbol,
    source: &str,
    path: &Path,
    _all_symbols: &[CsSymbol],
    namespace: &str,
    file_name: &str,
) -> Option<Document> {
    let anchor = make_anchor(namespace, file_name, &sym.name);
    let span = node_to_span(sym.node, path);
    let attrs = build_code_attributes(
        source,
        &format!("{:?}", sym.kind).to_lowercase(),
        Some(path),
        Some(&span),
    );
    let mut blocks = Vec::new();

    if let Some(ref doc) = sym.doc_comment {
        blocks.push(Block::Paragraph(doc.clone()));
    }

    let mut rows: Vec<Vec<String>> = vec![
        vec!["Kind".to_string(), format!("{:?}", sym.kind)],
        vec!["Visibility".to_string(), sym.visibility.clone()],
    ];
    if sym.is_static {
        rows.push(vec!["Static".to_string(), "true".to_string()]);
    }
    for p in &sym.params {
        rows.push(vec![format!("param {}", p.name), p.ty.clone()]);
    }
    if let Some(ref rt) = sym.return_type {
        rows.push(vec!["Returns".to_string(), rt.clone()]);
    }
    rows.push(vec!["Qualified".to_string(), sym.qualified_name.clone()]);

    blocks.push(Block::Paragraph("== Signature".to_string()));
    blocks.push(Block::Table(aden_core::Table {
        headers: vec!["Property".to_string(), "Value".to_string()],
        rows,
    }));

    // Extract call sites
    if sym.kind == NodeType::Function
        && sym.name != ".ctor"
        && let Some(body) = sym.node.child_by_field_name("body")
    {
        let calls = extract_cs_call_sites(body, source);
        let filtered: Vec<_> = calls
            .into_iter()
            .filter(|(c, _)| !is_cs_std_noise(c))
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

    // Type-usage edges: types named in the signature (params + return) are
    // `Used`, so a type that is used but never called is not a false dead-code
    // candidate. Only names that resolve to a stored symbol become edges.
    {
        let mut type_uses: Vec<String> = Vec::new();
        for p in &sym.params {
            for t in crate::tree_sitter_common::extract_type_idents(&p.ty) {
                if !type_uses.contains(&t) {
                    type_uses.push(t);
                }
            }
        }
        if let Some(ref rt) = sym.return_type {
            for t in crate::tree_sitter_common::extract_type_idents(rt) {
                if !type_uses.contains(&t) {
                    type_uses.push(t);
                }
            }
        }
        if !type_uses.is_empty() {
            let uses_code = type_uses
                .iter()
                .map(|t| format!("edge::uses[{}]", t))
                .collect::<Vec<_>>()
                .join("\n");
            blocks.push(Block::Listing {
                language: None,
                code: uses_code,
            });
        }
    }

    if sym.doc_comment.is_some() {
        blocks.push(Block::Admonition {
            kind: aden_core::AdmonitionKind::Note,
            text: "Extracted from source code via tree-sitter. Confidence is heuristic."
                .to_string(),
        });
    }

    Some(Document {
        anchor,
        node_type: sym.kind.clone(),
        attributes: attrs,
        blocks,
        source_span: Some(span),
        metadata: None,
        confidence: 0.9,
    })
}

const CS_SKIP_CALLEES: &[&str] = &[
    "ToString",
    "Equals",
    "GetHashCode",
    "Clone",
    "CompareTo",
    "GetType",
    "GetTypeCode",
    "Length",
    "Count",
    "Capacity",
    "ToList",
    "ToArray",
    "ToDictionary",
    "Select",
    "Where",
    "OrderBy",
    "ThenBy",
    "GroupBy",
    "Aggregate",
    "First",
    "FirstOrDefault",
    "Single",
    "SingleOrDefault",
    "Last",
    "LastOrDefault",
    "Any",
    "All",
    "Contains",
    "Exists",
    "Find",
    "FindAll",
    "Add",
    "Remove",
    "Clear",
    "Insert",
    "RemoveAt",
    "AddRange",
    "WriteLine",
    "Write",
    "ReadLine",
    "Read",
    "Flush",
    "Debug",
    "Trace",
    "Log",
    "Information",
    "Warning",
    "Error",
    "GetService",
    "GetRequiredService",
    "AddSingleton",
    "AddScoped",
    "AddTransient",
    "Configure",
    "Use",
    "Map",
    "MapControllers",
    "MapGet",
    "MapPost",
    "BuildServiceProvider",
    "CreateScope",
    "Parse",
    "TryParse",
    "Format",
    "Stringify",
    "Serialize",
    "Deserialize",
    "AsSpan",
    "AsMemory",
    "ToImmutable",
    "ToFrozen",
    "GetValue",
    "SetValue",
    "GetProperty",
    "SetProperty",
    "Invoke",
    "BeginInvoke",
    "EndInvoke",
];

fn is_cs_std_noise(name: &str) -> bool {
    CS_SKIP_CALLEES.contains(&name)
        || (name.starts_with("Get") && name.len() <= 7)
        || (name.starts_with("Set") && name.len() <= 7)
}

fn extract_cs_call_sites(node: tree_sitter::Node, source: &str) -> Vec<(String, usize)> {
    let mut calls = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        calls.extend(extract_cs_call_sites(child, source));
    }

    match node.kind() {
        "invocation_expression" => {
            if let Some(func) = node.child_by_field_name("function") {
                let callee = resolve_cs_callee_name(func, source);
                if !callee.is_empty() && callee.len() >= 2 {
                    let line = func.start_position().row + 1;
                    calls.push((callee, line));
                }
            }
        }
        "object_creation_expression" => {
            if let Some(typ) = node.child_by_field_name("type") {
                let type_name = node_text(typ, source).to_string();
                if !type_name.is_empty() {
                    let line = typ.start_position().row + 1;
                    calls.push((format!("new {}", type_name), line));
                }
            }
        }
        _ => {}
    }
    calls
}

fn resolve_cs_callee_name(node: tree_sitter::Node, source: &str) -> String {
    match node.kind() {
        "identifier" => node_text(node, source).to_string(),
        "member_access_expression" | "qualified_name" => {
            if let Some(name) = node.child_by_field_name("name") {
                node_text(name, source).to_string()
            } else {
                // Fallback: extract rightmost identifier
                let mut last = String::new();
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "identifier" || child.kind() == "generic_name" {
                        last = node_text(child, source).to_string();
                    }
                }
                if !last.is_empty() {
                    last
                } else {
                    node_text(node, source).to_string()
                }
            }
        }
        "generic_name" => {
            if let Some(name) = node.child_by_field_name("name") {
                node_text(name, source).to_string()
            } else {
                node_text(node, source).to_string()
            }
        }
        _ => node_text(node, source).to_string(),
    }
}
