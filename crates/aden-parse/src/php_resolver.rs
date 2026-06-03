// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Deep PHP resolver — symbol + call-site analysis.
//!
//! Handles:
//!   • Class, interface, trait, enum declarations
//!   • Function declarations (global)
//!   • Method declarations (instance, static)
//!   • Property declarations
//!   • Intra-file and cross-module call resolution (best-effort)
//!   • Emits `edge::calls[]` macros for graph ingestion
//!
//! Phase 2 second pass (not yet implemented):
//!   • Composer autoload path resolution
//!   • Magic methods (__construct, __invoke, etc.)
//!   • Closure binding analysis
//!   • Trait conflict resolution

use crate::extractor::{LanguageExtractor, build_code_attributes, make_anchor};
use aden_core::{Block, Document, NodeType, Parameter, Result};
use std::path::Path;

/// Deep PHP extractor.
pub struct PhpResolver;

impl Default for PhpResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl PhpResolver {
    pub fn new() -> Self {
        Self
    }
}

impl LanguageExtractor for PhpResolver {
    fn language_id(&self) -> &'static str {
        "php"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["php"]
    }

    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>> {
        let language = tree_sitter_language_pack::get_language("php")
            .map_err(|e| aden_core::Error::Parse(format!("language-pack: {}", e)))?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language)
            .map_err(|e| aden_core::Error::Parse(e.to_string()))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| aden_core::Error::Parse("tree-sitter returned None".to_string()))?;

        let namespace = infer_php_namespace(path, source);
        let file_name = path.file_name().unwrap_or_default().to_string_lossy();

        let mut symbols: Vec<PhpSymbol> = Vec::new();
        walk_program(
            tree.root_node(),
            source,
            &namespace,
            &file_name,
            &mut symbols,
        );

        let mut docs = Vec::new();
        for sym in &symbols {
            if let Some(doc) = emit_php_symbol(
                sym, source, path, &symbols, &namespace, &file_name,
            ) {
                docs.push(doc);
            }
        }

        Ok(docs)
    }
}

#[derive(Debug)]
struct PhpSymbol<'a> {
    qualified_name: String,
    kind: NodeType,
    node: tree_sitter::Node<'a>,
    params: Vec<Parameter>,
    doc_comment: Option<String>,
    visibility: String,
    is_static: bool,
}

fn infer_php_namespace(path: &Path, source: &str) -> String {
    if let Some(ns) = extract_namespace_from_source(source) {
        return ns;
    }
    let path_str = path.to_string_lossy();
    // src/Foo/Bar.php -> Foo\Bar
    if let Some(idx) = path_str.rfind("/src/") {
        let start = idx + 5;
        let end = path_str.rfind('/').unwrap_or(path_str.len()).max(start);
        let after = &path_str[start..end];
        return after.replace('/', "\\");
    }
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn extract_namespace_from_source(source: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("namespace ") {
            let ns = trimmed
                .strip_prefix("namespace ")
                .unwrap()
                .trim_end_matches(';')
                .trim();
            return Some(ns.to_string());
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
        if let Some(inner) = trimmed
            .strip_prefix("/**")
            .and_then(|s| s.strip_suffix("*/"))
        {
            // `/**/` (len 4) yields inner == "" here — the old `&trimmed[3..len-2]`
            // slice panicked (begin > end) on that untrusted input.
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
            if !lines.is_empty() {
                comments.push(lines.join("\n"));
            }
            break;
        } else if trimmed.starts_with("///") {
            comments.push(trimmed.trim_start_matches('/').trim_start().to_string());
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
        let text = node_text(child, source).trim();
        if matches!(text, "public" | "private" | "protected") {
            return text.to_string();
        }
    }
    "public".to_string() // PHP default
}

fn has_modifier(node: tree_sitter::Node, source: &str, modifier: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let text = node_text(child, source);
        if text.trim() == modifier {
            return true;
        }
    }
    false
}

fn walk_program<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    namespace: &str,
    file_name: &str,
    symbols: &mut Vec<PhpSymbol<'a>>,
) {
    if !node.is_named() {
        return;
    }

    match node.kind() {
        "namespace_definition" => {
            // Namespace may wrap declarations
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_program(child, source, namespace, file_name, symbols);
            }
        }
        "class_declaration"
        | "interface_declaration"
        | "trait_declaration"
        | "enum_declaration"
        | "enum_case" => {
            parse_type_declaration(node, source, namespace, file_name, symbols);
        }
        "function_definition" => {
            parse_function(node, source, namespace, file_name, symbols);
        }
        "method_declaration" => {
            parse_method(node, source, namespace, file_name, symbols);
        }
        "property_declaration" => {
            parse_property(node, source, namespace, file_name, symbols);
        }
        "program"
        | "declaration_list"
        | "class_interface_clause"
        | "class_body"
        | "trait_body"
        | "interface_body"
        | "enum_body"
        | "compound_statement"
        | "statement"
        | "block" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_program(child, source, namespace, file_name, symbols);
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named() {
                    walk_program(child, source, namespace, file_name, symbols);
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
    symbols: &mut Vec<PhpSymbol<'a>>,
) {
    let name = if let Some(name_node) = node.child_by_field_name("name") {
        node_text(name_node, source).to_string()
    } else if node.kind() == "enum_case" {
        if let Some(name_node) = node.child_by_field_name("name") {
            node_text(name_node, source).to_string()
        } else {
            return;
        }
    } else {
        return;
    };

    let vis = extract_visibility(node, source);
    let is_static = has_modifier(node, source, "static");
    let kind = match node.kind() {
        "interface_declaration" => NodeType::Type,
        "trait_declaration" => NodeType::Type,
        "enum_declaration" | "enum_case" => NodeType::Type,
        _ => NodeType::Type,
    };

    let qname = if let Some(parent) = find_parent_type_name(node, source) {
        format!("{}\\{}\\{}", namespace, parent, name)
    } else {
        format!("{}\\{}", namespace, name)
    };

    symbols.push(PhpSymbol {
        qualified_name: qname,
        kind,
        node,
        params: Vec::new(),
        doc_comment: extract_doc_comment(node, source),
        visibility: vis,
        is_static,
    });

    // Walk body for nested declarations
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(
            child.kind(),
            "class_body" | "trait_body" | "interface_body" | "enum_body" | "declaration_list"
        ) {
            let mut cc = child.walk();
            for grandchild in child.children(&mut cc) {
                if grandchild.is_named() {
                    walk_program(grandchild, source, namespace, file_name, symbols);
                }
            }
        }
    }
}

fn parse_function<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    namespace: &str,
    _file_name: &str,
    symbols: &mut Vec<PhpSymbol<'a>>,
) {
    if let Some(name_node) = node.child_by_field_name("name") {
        let name = node_text(name_node, source).to_string();
        let vis = extract_visibility(node, source);

        let mut params = Vec::new();
        if let Some(params_node) = node.child_by_field_name("parameters") {
            let mut pc = params_node.walk();
            for param in params_node.children(&mut pc) {
                if param.kind() == "formal_parameter" || param.kind() == "parameter" {
                    let param_type = param
                        .child_by_field_name("type")
                        .map(|n| node_text(n, source).to_string())
                        .unwrap_or_default();
                    let param_name = param
                        .child_by_field_name("name")
                        .map(|n| node_text(n, source).to_string())
                        .unwrap_or_default();
                    params.push(Parameter {
                        name: param_name.trim_start_matches('$').to_string(),
                        ty: param_type,
                        default_value: None,
                    });
                }
            }
        }

        let qname = format!("{}\\{}", namespace, name);
        symbols.push(PhpSymbol {
            qualified_name: qname,
            kind: NodeType::Function,
            node,
            params,
            doc_comment: extract_doc_comment(node, source),
            visibility: vis,
            is_static: false,
        });
    }
}

fn parse_method<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    namespace: &str,
    _file_name: &str,
    symbols: &mut Vec<PhpSymbol<'a>>,
) {
    if let Some(name_node) = node.child_by_field_name("name") {
        let name = node_text(name_node, source).to_string();
        let vis = extract_visibility(node, source);
        let is_static = has_modifier(node, source, "static");

        let mut params = Vec::new();
        if let Some(params_node) = node.child_by_field_name("parameters") {
            let mut pc = params_node.walk();
            for param in params_node.children(&mut pc) {
                if matches!(param.kind(), "formal_parameter" | "parameter") {
                    let param_type = param
                        .child_by_field_name("type")
                        .map(|n| node_text(n, source).to_string())
                        .unwrap_or_default();
                    let param_name = param
                        .child_by_field_name("name")
                        .map(|n| node_text(n, source).to_string())
                        .unwrap_or_default();
                    params.push(Parameter {
                        name: param_name.trim_start_matches('$').to_string(),
                        ty: param_type,
                        default_value: None,
                    });
                }
            }
        }

        let class_name = find_parent_type_name(node, source).unwrap_or_default();
        let qname = if class_name.is_empty() {
            format!("{}\\{}", namespace, name)
        } else {
            format!("{}\\{}\\{}", namespace, class_name, name)
        };

        symbols.push(PhpSymbol {
            qualified_name: qname,
            kind: NodeType::Function,
            node,
            params,
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
    _symbols: &mut Vec<PhpSymbol<'a>>,
) {
    // Properties are intentionally not emitted as standalone Documents.
    // Their descriptions live inside the parent Type document.
}

fn find_parent_type_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if matches!(
            parent.kind(),
            "class_declaration"
                | "interface_declaration"
                | "trait_declaration"
                | "enum_declaration"
        ) && let Some(name_node) = parent.child_by_field_name("name")
        {
            return Some(node_text(name_node, source).to_string());
        }
        current = parent;
    }
    None
}

fn emit_php_symbol(
    sym: &PhpSymbol,
    source: &str,
    path: &Path,
    _all_symbols: &[PhpSymbol],
    namespace: &str,
    file_name: &str,
) -> Option<Document> {
    // Qualify with the enclosing class so same-named methods across classes don't
    // collapse to one anchor (data loss). `qualified_name` is
    // `<namespace>\<Class>\<method>`; strip the namespace prefix `make_anchor`
    // already supplies and normalize `\` to `.` → fragment `Class.method`.
    // Top-level type anchors are unchanged.
    let fragment = sym
        .qualified_name
        .strip_prefix(&format!("{namespace}\\"))
        .unwrap_or(&sym.qualified_name)
        .replace('\\', ".");
    let anchor = make_anchor(namespace, file_name, &fragment);
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
    rows.push(vec!["Qualified".to_string(), sym.qualified_name.clone()]);

    blocks.push(Block::Paragraph("== Signature".to_string()));
    blocks.push(Block::Table(aden_core::Table {
        headers: vec!["Property".to_string(), "Value".to_string()],
        rows,
    }));

    // Extract call sites
    if sym.kind == NodeType::Function
        && let Some(body) = sym.node.child_by_field_name("body")
    {
        let calls = extract_php_call_sites(body, source);
        let filtered: Vec<_> = calls
            .into_iter()
            .filter(|(c, _)| !is_php_std_noise(c))
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

    // Type-usage edges: types named in typed params are `Uses`d, so a type that
    // is used (but never "called") is not a false dead-code candidate. Only names
    // that resolve to a stored symbol actually become edges.
    {
        let mut type_uses: Vec<String> = Vec::new();
        for p in &sym.params {
            for t in crate::tree_sitter_common::extract_type_idents(&p.ty) {
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

const PHP_SKIP_CALLEES: &[&str] = &[
    "echo",
    "print",
    "die",
    "exit",
    "isset",
    "unset",
    "empty",
    "eval",
    "include",
    "require",
    "strlen",
    "count",
    "sizeof",
    "array_merge",
    "array_filter",
    "array_map",
    "array_reduce",
    "explode",
    "implode",
    "substr",
    "strpos",
    "str_replace",
    "preg_match",
    "preg_replace",
    "trim",
    "ltrim",
    "rtrim",
    "strtolower",
    "strtoupper",
    "ucfirst",
    "lcfirst",
    "date",
    "time",
    "strtotime",
    "microtime",
    "gmdate",
    "json_encode",
    "json_decode",
    "serialize",
    "unserialize",
    "file_get_contents",
    "file_put_contents",
    "fopen",
    "fclose",
    "fread",
    "fwrite",
    "header",
    "http_response_code",
    "setcookie",
    "session_start",
    "session_destroy",
    "mysql_connect",
    "mysql_query",
    "mysqli_query",
    "PDO",
    "prepare",
    "execute",
    "fetch",
    "fetchAll",
    "var_dump",
    "print_r",
    "debug_backtrace",
    "debug_print_backtrace",
    "class_exists",
    "method_exists",
    "property_exists",
    "is_array",
    "is_string",
    "is_int",
    "is_numeric",
    "array_push",
    "array_pop",
    "array_shift",
    "array_unshift",
    "array_key_exists",
    "in_array",
    "new",
    "clone",
    "__construct",
    "__destruct",
    "__toString",
    "__invoke",
    "__get",
    "__set",
    "__call",
    "__callStatic",
];

fn is_php_std_noise(name: &str) -> bool {
    PHP_SKIP_CALLEES.contains(&name) || name.starts_with("__")
}

fn extract_php_call_sites(node: tree_sitter::Node, source: &str) -> Vec<(String, usize)> {
    let mut calls = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        calls.extend(extract_php_call_sites(child, source));
    }

    match node.kind() {
        "function_call_expression" | "call_expression" => {
            if let Some(func) = node.child_by_field_name("function") {
                let callee = resolve_php_callee_name(func, source);
                if !callee.is_empty() && callee.len() >= 2 {
                    let line = func.start_position().row + 1;
                    calls.push((callee, line));
                }
            }
        }
        "object_creation_expression" | "new_expression" => {
            if let Some(typ) = node.child_by_field_name("name") {
                let type_name = node_text(typ, source).to_string();
                if !type_name.is_empty() {
                    let line = typ.start_position().row + 1;
                    calls.push((format!("new {}", type_name), line));
                }
            }
        }
        "member_access_expression" | "property access" => {
            if let Some(name) = node.child_by_field_name("name") {
                let callee = node_text(name, source).to_string();
                if !callee.is_empty() {
                    let line = name.start_position().row + 1;
                    calls.push((callee, line));
                }
            }
        }
        _ => {}
    }
    calls
}

fn resolve_php_callee_name(node: tree_sitter::Node, source: &str) -> String {
    match node.kind() {
        "name" | "identifier" | "variable_name" | "variable" => {
            node_text(node, source).trim_start_matches('$').to_string()
        }
        "qualified_name" | "namespace_name" => node_text(node, source).to_string(),
        "member_access_expression" => {
            if let Some(name) = node.child_by_field_name("name") {
                node_text(name, source).to_string()
            } else {
                node_text(node, source).to_string()
            }
        }
        _ => node_text(node, source).to_string(),
    }
}
