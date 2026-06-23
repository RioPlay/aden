// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Deep Java resolver — symbol + call-site analysis.
//!
//! Handles:
//!   • Class, interface, enum declarations
//!   • Method declarations, constructors
//!   • Field declarations
//!   • Intra-file and cross-module call resolution (best-effort)
//!   • Emits `edge::calls[]` macros for graph ingestion
//!
//! Phase 2 second pass (not yet implemented):
//!   • Maven/Gradle module path resolution
//!   • Annotation processor awareness

use crate::extractor::{
    LanguageExtractor, build_code_attributes, infer_project_root, make_anchor,
    project_relative_file,
};
use aden_core::{Block, Document, NodeType, Parameter, Result};
use std::path::Path;

/// Deep Java extractor.
pub struct JavaResolver;

impl Default for JavaResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl JavaResolver {
    pub fn new() -> Self {
        Self
    }
}

impl LanguageExtractor for JavaResolver {
    fn language_id(&self) -> &'static str {
        "java"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["java"]
    }

    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>> {
        let language = crate::get_ts_language("java")
            .map_err(|e| aden_core::Error::Parse(format!("language-pack: {}", e)))?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language)
            .map_err(|e| aden_core::Error::Parse(e.to_string()))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| aden_core::Error::Parse("tree-sitter returned None".to_string()))?;

        let module_path = infer_java_package(path, source);
        let project_root = infer_project_root(path);
        let file_name_owned = project_relative_file(path, &project_root);
        let file_name = file_name_owned.as_str();

        // Phase 1: collect local symbols
        let mut symbols: Vec<JavaSymbol> = Vec::new();
        walk_compilation_unit(
            tree.root_node(),
            source,
            &module_path,
            file_name,
            &mut symbols,
        );

        // Phase 2: emit Documents with call-site resolution.
        let mut docs = Vec::new();
        for sym in &symbols {
            if let Some(doc) =
                emit_java_symbol(sym, source, path, &symbols, &module_path, file_name)
            {
                docs.push(doc);
            }
        }

        Ok(docs)
    }
}

/// Information about a single local symbol.
#[derive(Debug)]
struct JavaSymbol<'a> {
    qualified_name: String, // package.Class#method
    kind: NodeType,
    node: tree_sitter::Node<'a>,
    params: Vec<Parameter>,
    doc_comment: Option<String>,
    visibility: String, // public, private, protected, package-private
    is_static: bool,
}

fn infer_java_package(path: &Path, source: &str) -> String {
    // Try to read package from source first
    if let Some(pkg) = extract_package_from_source(source) {
        return pkg;
    }
    // Fallback: infer from directory structure
    // (src/main/java/com/example/Foo.java -> com.example). Walk path *components*
    // rather than matching "/java/" or "\\java\\" in the stringified path, so it
    // works identically regardless of the OS path separator.
    let comps: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if let Some(idx) = comps.iter().rposition(|c| c == "java") {
        // Package = the directory components after the last `java`, excluding the
        // file name (the final component). An empty slice means a default-package
        // file (e.g. src/main/java/Foo.java) -> return "" rather than falling
        // through to the parent-dir heuristic below, which would wrongly yield
        // "java". This matches the prior string-rfind behavior.
        return comps[idx + 1..comps.len().saturating_sub(1)].join(".");
    }
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn extract_package_from_source(source: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("package ") {
            let pkg = trimmed
                .strip_prefix("package ")
                .unwrap()
                .trim_end_matches(';')
                .trim();
            return Some(pkg.to_string());
        }
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

fn extract_doc_comment(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut comments = Vec::new();
    let _cursor = node.walk();
    // Look for preceding comments among siblings
    // (tree-sitter usually places them as children or preceding siblings)
    // Simpler approach: scan backwards in source text from node start
    let node_start = node.start_byte();
    let text_before = &source[..node_start];
    let lines: Vec<&str> = text_before.lines().rev().collect();
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
        } else if trimmed.starts_with("//") {
            // Javadoc comments don't use // typically; stop here
            break;
        } else if trimmed.is_empty() {
            continue;
        } else {
            break;
        }
    }
    if comments.is_empty() {
        None
    } else {
        Some(comments.join("\n"))
    }
}

fn extract_visibility(node: tree_sitter::Node, source: &str) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let text = node_text(child, source);
        if matches!(text, "public" | "private" | "protected") {
            return text.to_string();
        }
    }
    "package-private".to_string()
}

fn has_modifier(node: tree_sitter::Node, source: &str, modifier: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if node_text(child, source) == modifier {
            return true;
        }
    }
    false
}

fn walk_compilation_unit<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    package: &str,
    file_name: &str,
    symbols: &mut Vec<JavaSymbol<'a>>,
) {
    if !node.is_named() {
        return;
    }

    match node.kind() {
        "package_declaration" => {
            // Already handled by extract_package_from_source
        }
        "import_declaration" => {
            // Imports are not consumed; intentionally ignored.
        }
        "class_declaration" | "interface_declaration" | "enum_declaration" => {
            parse_type_declaration(node, source, package, file_name, symbols);
        }
        "method_declaration" => {
            parse_method(node, source, package, file_name, symbols);
        }
        "constructor_declaration" => {
            parse_constructor(node, source, package, file_name, symbols);
        }
        "field_declaration" => {
            parse_field(node, source, package, file_name, symbols);
        }
        "program"
        | "body"
        | "class_body"
        | "interface_body"
        | "enum_body"
        | "enum_body_declarations"
        | "statement"
        | "block" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_compilation_unit(child, source, package, file_name, symbols);
            }
        }
        _ => {
            // Recurse into all other named nodes
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named() {
                    walk_compilation_unit(child, source, package, file_name, symbols);
                }
            }
        }
    }
}

fn parse_type_declaration<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    package: &str,
    file_name: &str,
    symbols: &mut Vec<JavaSymbol<'a>>,
) {
    if let Some(name_node) = node.child_by_field_name("name") {
        let name = node_text(name_node, source).to_string();
        let kind = match node.kind() {
            "class_declaration" => NodeType::Type,
            "interface_declaration" => NodeType::Type,
            "enum_declaration" => NodeType::Type,
            _ => NodeType::Type,
        };

        let vis = extract_visibility(node, source);
        let is_static = has_modifier(node, source, "static");

        let qname = format!("{}.{}", package, name);
        symbols.push(JavaSymbol {
            qualified_name: qname,
            kind,
            node,
            params: Vec::new(),
            doc_comment: extract_doc_comment(node, source),
            visibility: vis,
            is_static,
        });

        // Walk the body for nested members
        if let Some(body) = node.child_by_field_name("body") {
            let mut cursor = body.walk();
            for child in body.children(&mut cursor) {
                if child.is_named() {
                    walk_compilation_unit(child, source, package, file_name, symbols);
                }
            }
        }

        // For interfaces/classes with explicit body field but also enum_body
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if matches!(
                child.kind(),
                "class_body" | "interface_body" | "enum_body" | "enum_body_declarations"
            ) {
                let mut cc = child.walk();
                for grandchild in child.children(&mut cc) {
                    if grandchild.is_named() {
                        walk_compilation_unit(grandchild, source, package, file_name, symbols);
                    }
                }
            }
        }
    }
}

fn parse_method<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    package: &str,
    _file_name: &str,
    symbols: &mut Vec<JavaSymbol<'a>>,
) {
    if let Some(name_node) = node.child_by_field_name("name") {
        let name = node_text(name_node, source).to_string();
        let vis = extract_visibility(node, source);
        let is_static = has_modifier(node, source, "static");

        let mut params = Vec::new();
        if let Some(params_node) = node.child_by_field_name("parameters") {
            let mut pc = params_node.walk();
            for param in params_node.children(&mut pc) {
                if param.kind() == "formal_parameter" {
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

        // Try to find nearest class/interface name for qualified name
        let class_name = find_parent_type_name(node, source).unwrap_or_default();
        let qname = if class_name.is_empty() {
            format!("{}.{}", package, name)
        } else {
            format!("{}.{}/{}", package, class_name, name)
        };

        symbols.push(JavaSymbol {
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

fn parse_constructor<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    package: &str,
    _file_name: &str,
    symbols: &mut Vec<JavaSymbol<'a>>,
) {
    let class_name = find_parent_type_name(node, source).unwrap_or_else(|| "<init>".to_string());
    let vis = extract_visibility(node, source);

    let mut params = Vec::new();
    if let Some(params_node) = node.child_by_field_name("parameters") {
        let mut pc = params_node.walk();
        for param in params_node.children(&mut pc) {
            if param.kind() == "formal_parameter" {
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

    let qname = format!("{}.{}/<init>", package, class_name);
    symbols.push(JavaSymbol {
        qualified_name: qname,
        kind: NodeType::Function,
        node,
        params,
        doc_comment: extract_doc_comment(node, source),
        visibility: vis,
        is_static: false,
    });
}

fn parse_field<'a>(
    _node: tree_sitter::Node<'a>,
    _source: &str,
    _package: &str,
    _file_name: &str,
    _symbols: &mut Vec<JavaSymbol<'a>>,
) {
    // Fields are intentionally not emitted as standalone Documents.
    // Their descriptions live inside the parent Type document.
}

fn find_parent_type_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if matches!(
            parent.kind(),
            "class_declaration" | "interface_declaration" | "enum_declaration"
        ) && let Some(name_node) = parent.child_by_field_name("name")
        {
            return Some(node_text(name_node, source).to_string());
        }
        current = parent;
    }
    None
}

fn emit_java_symbol(
    sym: &JavaSymbol,
    source: &str,
    path: &Path,
    _all_symbols: &[JavaSymbol],
    package: &str,
    file_name: &str,
) -> Option<Document> {
    // Qualify with the enclosing class so same-named methods across classes don't
    // collapse to one anchor (data loss). `qualified_name` is
    // `<package>.<Class>/<method>` (methods) or `<package>.<Class>` (types);
    // strip the package prefix `make_anchor` already supplies and normalize the
    // `/` member separator to `.` → fragment `Class.method`. Type anchors (no
    // member part) are unchanged.
    let fragment = sym
        .qualified_name
        .strip_prefix(&format!("{package}."))
        .unwrap_or(&sym.qualified_name)
        .replace('/', ".");
    let anchor = make_anchor(package, file_name, &fragment);
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

    // Implements / extends edges for type declarations.
    //
    // `class Foo implements Bar, Baz` → super_interfaces → type_list →
    //   type_identifier children → emit edge::implements[Bar], edge::implements[Baz].
    //
    // `class Foo extends Parent` → superclass → type_identifier →
    //   emit edge::extends[Parent].
    //   Inheritance and interface satisfaction are now distinct edge types.
    if sym.kind == NodeType::Type {
        let mut implements_targets: Vec<String> = Vec::new();
        let mut extends_targets: Vec<String> = Vec::new();

        // `implements InterfaceA, InterfaceB` — Java grammar: super_interfaces > type_list
        if let Some(super_ifaces) = sym.node.child_by_field_name("interfaces") {
            let type_list_node = super_ifaces
                .children(&mut super_ifaces.walk())
                .find(|n| n.kind() == "type_list")
                .unwrap_or(super_ifaces);
            let mut cursor = type_list_node.walk();
            for child in type_list_node.children(&mut cursor) {
                if child.kind() == "type_identifier" {
                    implements_targets.push(node_text(child, source).to_string());
                }
            }
        }

        // `extends ParentClass` — Java grammar: superclass > type_identifier
        // Routed to edge::extends (not edge::implements) to distinguish
        // class inheritance from interface satisfaction.
        if let Some(superclass) = sym.node.child_by_field_name("superclass") {
            let mut cursor = superclass.walk();
            for child in superclass.children(&mut cursor) {
                if child.kind() == "type_identifier" {
                    extends_targets.push(node_text(child, source).to_string());
                }
            }
        }

        if !implements_targets.is_empty() {
            let edge_code = implements_targets
                .iter()
                .map(|t| format!("edge::implements[{t}]"))
                .collect::<Vec<_>>()
                .join("\n");
            blocks.push(Block::Listing {
                language: None,
                code: edge_code,
            });
        }
        if !extends_targets.is_empty() {
            let edge_code = extends_targets
                .iter()
                .map(|t| format!("edge::extends[{t}]"))
                .collect::<Vec<_>>()
                .join("\n");
            blocks.push(Block::Listing {
                language: None,
                code: edge_code,
            });
        }
    }

    // Extract call sites from method body
    if sym.kind == NodeType::Function
        && let Some(body) = sym.node.child_by_field_name("body")
    {
        let calls = extract_java_call_sites(body, source);
        let filtered: Vec<_> = calls
            .into_iter()
            .filter(|(c, _)| !is_java_std_noise(c))
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

    // Type-usage edges: types named in the signature (param types + the method
    // return type) are `Used`, so a type that is used but never called is not a
    // false dead-code candidate. Only names that resolve to a stored symbol
    // actually become edges (see link_store_edges).
    {
        let mut type_uses: Vec<String> = Vec::new();
        for p in &sym.params {
            for t in crate::tree_sitter_common::extract_type_idents(&p.ty) {
                if !type_uses.contains(&t) {
                    type_uses.push(t);
                }
            }
        }
        // The method return type lives on the `type` field of the declaration
        // node (constructors have none, which is fine).
        if let Some(rt) = sym.node.child_by_field_name("type") {
            let rt_text = node_text(rt, source);
            for t in crate::tree_sitter_common::extract_type_idents(rt_text) {
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

/// Standard-library and common utility functions to exclude from call-graph extraction.
const JAVA_SKIP_CALLEES: &[&str] = &[
    "toString",
    "equals",
    "hashCode",
    "clone",
    "compareTo",
    "length",
    "size",
    "isEmpty",
    "get",
    "put",
    "add",
    "remove",
    "clear",
    "iterator",
    "stream",
    "forEach",
    "map",
    "filter",
    "collect",
    "println",
    "print",
    "printf",
    "format",
    "valueOf",
    "parseInt",
    "parseDouble",
    "System",
    "out",
    "err",
    "in",
    "assertEquals",
    "assertTrue",
    "assertFalse",
    "assertNull",
    "assertNotNull",
    "getLogger",
    "log",
    "info",
    "warn",
    "error",
    "debug",
    "of",
    "builder",
    "build",
];

fn is_java_std_noise(name: &str) -> bool {
    JAVA_SKIP_CALLEES.contains(&name)
        || name.starts_with("get") && name.len() <= 6
        || name.starts_with("set") && name.len() <= 6
}

fn extract_java_call_sites(node: tree_sitter::Node, source: &str) -> Vec<(String, usize)> {
    let mut calls = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        calls.extend(extract_java_call_sites(child, source));
    }

    match node.kind() {
        "method_invocation" => {
            if let Some(func) = node.child_by_field_name("name") {
                let callee = resolve_java_callee_name(func, source);
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

fn resolve_java_callee_name(node: tree_sitter::Node, source: &str) -> String {
    match node.kind() {
        "identifier" => node_text(node, source).to_string(),
        "field_access" => {
            if let Some(field) = node.child_by_field_name("field") {
                node_text(field, source).to_string()
            } else {
                node_text(node, source).to_string()
            }
        }
        "scoped_identifier" => {
            let mut parts = Vec::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "identifier" {
                    parts.push(node_text(child, source));
                }
            }
            if parts.len() >= 2 {
                format!("{}.{}", parts[parts.len() - 2], parts[parts.len() - 1])
            } else if let Some(last) = parts.last() {
                last.to_string()
            } else {
                node_text(node, source).to_string()
            }
        }
        _ => node_text(node, source).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // All cases pass an empty source so the package-from-source short-circuit is
    // skipped and the directory-structure fallback is exercised.
    #[test]
    fn infer_package_standard_maven_layout() {
        let p = Path::new("src/main/java/com/example/Foo.java");
        assert_eq!(infer_java_package(p, ""), "com.example");
    }

    #[test]
    fn infer_package_default_package_is_empty() {
        // Nothing between `java/` and the file -> default package, empty string
        // (NOT "java", which the pre-fix component logic wrongly returned).
        let p = Path::new("src/main/java/Foo.java");
        assert_eq!(infer_java_package(p, ""), "");
    }

    #[test]
    fn infer_package_last_java_root_wins() {
        let p = Path::new("a/java/b/java/com/x/Bar.java");
        assert_eq!(infer_java_package(p, ""), "com.x");
    }

    #[test]
    fn infer_package_no_java_component_falls_back_to_parent_dir() {
        let p = Path::new("some/other/dir/Baz.java");
        assert_eq!(infer_java_package(p, ""), "dir");
    }

    #[test]
    fn infer_package_from_source_takes_precedence() {
        let p = Path::new("src/main/java/com/example/Foo.java");
        assert_eq!(
            infer_java_package(p, "package com.override;\n"),
            "com.override"
        );
    }
}
