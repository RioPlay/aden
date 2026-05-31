// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Deep Kotlin resolver — import + call-site analysis.
//!
//! Handles:
//!   • `package`, `import` resolution (single, wildcard, aliased)
//!   • Class, object, interface, data class declarations
//!   • Function declarations (top-level, member, extension)
//!   • Property declarations (val, var, const val)
//!   • Intra-file and cross-module call resolution (best-effort)
//!   • Emits `edge::calls[]` macros for graph ingestion
//!
//! Phase 2 second pass (not yet implemented):
//!   • Gradle module path resolution
//!   • Type alias resolution
//!   • Reified type parameter tracking

use crate::extractor::{LanguageExtractor, build_code_attributes, make_anchor};
use aden_core::{Block, Document, NodeType, Parameter, Result};
use std::path::Path;

/// Deep Kotlin extractor.
pub struct KotlinResolver;

impl Default for KotlinResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl KotlinResolver {
    pub fn new() -> Self {
        Self
    }
}

impl LanguageExtractor for KotlinResolver {
    fn language_id(&self) -> &'static str {
        "kotlin"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["kt", "kts"]
    }

    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>> {
        let language = tree_sitter_language_pack::get_language("kotlin")
            .map_err(|e| aden_core::Error::Parse(format!("language-pack: {}", e)))?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language)
            .map_err(|e| aden_core::Error::Parse(e.to_string()))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| aden_core::Error::Parse("tree-sitter returned None".to_string()))?;

        let module_path = infer_kotlin_package(path, source);
        let file_name = path.file_name().unwrap_or_default().to_string_lossy();

        let mut imports: Vec<KotlinImport> = Vec::new();
        let mut symbols: Vec<KotlinSymbol> = Vec::new();
        walk_source_file(
            tree.root_node(),
            source,
            &module_path,
            &file_name,
            &mut imports,
            &mut symbols,
        );

        let mut docs = Vec::new();
        for sym in &symbols {
            if let Some(doc) = emit_kotlin_symbol(
                sym,
                source,
                path,
                &symbols,
                &imports,
                &module_path,
                &file_name,
            ) {
                docs.push(doc);
            }
        }

        Ok(docs)
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct KotlinImport {
    package: String,
    name: String, // class name or "*"
    alias: Option<String>,
    is_wildcard: bool,
}

#[derive(Debug)]
struct KotlinSymbol<'a> {
    name: String,
    qualified_name: String,
    kind: NodeType,
    node: tree_sitter::Node<'a>,
    params: Vec<Parameter>,
    return_type: Option<String>, // Declared return type, if any
    doc_comment: Option<String>,
    visibility: String,
    is_extension: bool,
    receiver_type: Option<String>, // For extension functions
}

fn infer_kotlin_package(path: &Path, source: &str) -> String {
    if let Some(pkg) = extract_package_from_source(source) {
        return pkg;
    }
    let path_str = path.to_string_lossy();
    // src/main/kotlin/com/example/Foo.kt -> com.example
    if let Some(idx) = path_str.rfind("/kotlin/") {
        let start = idx + 8;
        let end = path_str.rfind('/').unwrap_or(path_str.len()).max(start);
        let after = &path_str[start..end];
        return after.replace('/', ".");
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
            return Some(trimmed.strip_prefix("package ").unwrap().trim().to_string());
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
        } else if !trimmed.is_empty() {
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
        if matches!(text, "public" | "private" | "protected" | "internal") {
            return text.to_string();
        }
    }
    "public".to_string() // Kotlin default visibility is public
}

fn walk_source_file<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    package: &str,
    file_name: &str,
    imports: &mut Vec<KotlinImport>,
    symbols: &mut Vec<KotlinSymbol<'a>>,
) {
    if !node.is_named() {
        return;
    }

    match node.kind() {
        "package_header" => {}
        "import_header" => {
            if let Some(imp) = parse_import(node, source) {
                imports.push(imp);
            }
        }
        "class_declaration"
        | "object_declaration"
        | "interface_declaration"
        | "companion_object"
        | "enum_class_declaration"
        | "data_class_declaration"
        | "sealed_class_declaration"
        | "type_alias"
        | "type_alias_declaration" => {
            parse_type_declaration(node, source, package, file_name, symbols);
        }
        "function_declaration" => {
            parse_function(node, source, package, file_name, symbols);
        }
        "property_declaration" => {
            parse_property(node, source, package, file_name, symbols);
        }
        "source_file" | "statements" | "class_body" | "object_body" | "property_delegate"
        | "block" | "expression" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_source_file(child, source, package, file_name, imports, symbols);
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named() {
                    walk_source_file(child, source, package, file_name, imports, symbols);
                }
            }
        }
    }
}

fn parse_import(node: tree_sitter::Node, source: &str) -> Option<KotlinImport> {
    let text = node_text(node, source).trim();
    if !text.starts_with("import ") {
        return None;
    }
    let without_import = &text["import ".len()..];
    // Handle alias: import foo.Bar as Baz
    let (main, alias) = if let Some(idx) = without_import.rfind(" as ") {
        (
            &without_import[..idx],
            Some(without_import[idx + 4..].trim().to_string()),
        )
    } else {
        (without_import, None)
    };

    let is_wildcard = main.ends_with(".*");
    if is_wildcard {
        let pkg = main[..main.len() - 1].trim_end_matches('.').to_string();
        return Some(KotlinImport {
            package: pkg.clone(),
            name: "*".to_string(),
            alias: None,
            is_wildcard,
        });
    }

    let parts: Vec<&str> = main.split('.').collect();
    if parts.is_empty() {
        return None;
    }
    let name = parts.last()?.to_string();
    let pkg = if parts.len() > 1 {
        parts[..parts.len() - 1].join(".")
    } else {
        String::new()
    };

    Some(KotlinImport {
        package: pkg,
        name,
        alias,
        is_wildcard,
    })
}

fn parse_type_declaration<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    package: &str,
    file_name: &str,
    symbols: &mut Vec<KotlinSymbol<'a>>,
) {
    // Most declarations expose a `name` field; `type_alias` may instead carry a
    // bare `type_identifier` child, so fall back to that.
    let name_node = node.child_by_field_name("name").or_else(|| {
        let mut cursor = node.walk();
        node.children(&mut cursor)
            .find(|c| c.kind() == "type_identifier")
    });
    if let Some(name_node) = name_node {
        let name = node_text(name_node, source).to_string();
        let vis = extract_visibility(node, source);
        let kind = match node.kind() {
            "interface_declaration" => NodeType::Type,
            "object_declaration" | "companion_object" => NodeType::Type,
            _ => NodeType::Type,
        };

        let qname = format!("{}.{}", package, name);
        symbols.push(KotlinSymbol {
            name: name.clone(),
            qualified_name: qname,
            kind,
            node,
            params: Vec::new(),
            return_type: None,
            doc_comment: extract_doc_comment(node, source),
            visibility: vis,
            is_extension: false,
            receiver_type: None,
        });

        // Walk nested declarations
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if matches!(
                child.kind(),
                "class_body" | "object_body" | "enum_class_body"
            ) {
                let mut cc = child.walk();
                for grandchild in child.children(&mut cc) {
                    if grandchild.is_named() {
                        walk_source_file(
                            grandchild,
                            source,
                            package,
                            file_name,
                            &mut Vec::new(),
                            symbols,
                        );
                    }
                }
            }
        }
    }
}

fn parse_function<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    package: &str,
    _file_name: &str,
    symbols: &mut Vec<KotlinSymbol<'a>>,
) {
    if let Some(name_node) = node.child_by_field_name("name") {
        let name = node_text(name_node, source).to_string();
        let vis = extract_visibility(node, source);

        // Check for extension function: receiver_type
        let mut receiver_type = None;
        let mut is_extension = false;
        if let Some(recv) = node.child_by_field_name("receiver") {
            receiver_type = Some(node_text(recv, source).to_string());
            is_extension = true;
        } else {
            // Try to find receiver type field in tree-sitter kotlin grammar
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "user_type" || child.kind().contains("type") {
                    // This might be the receiver
                    if child.start_byte() < name_node.start_byte() {
                        receiver_type = Some(node_text(child, source).to_string());
                        is_extension = true;
                        break;
                    }
                }
            }
        }

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

        // Declared return type: in the tree-sitter Kotlin grammar the return
        // type sits in the `type` field of the function (the `: Foo` after the
        // parameter list). Only capture nodes that follow the parameter list so
        // a receiver/value-parameter type is never mistaken for it.
        let return_type = node
            .child_by_field_name("type")
            .filter(|t| {
                node.child_by_field_name("parameters")
                    .map(|p| t.start_byte() >= p.end_byte())
                    .unwrap_or(true)
            })
            .map(|n| node_text(n, source).to_string());

        let class_name = find_parent_type_name(node, source);
        let display_name = if let Some(ref recv) = receiver_type {
            format!("{}.{}", recv, name)
        } else {
            name.clone()
        };

        let qname = if let Some(cls) = class_name {
            format!("{}.{}/{}", package, cls, display_name)
        } else {
            format!("{}.{}", package, display_name)
        };

        symbols.push(KotlinSymbol {
            name: display_name,
            qualified_name: qname,
            kind: NodeType::Function,
            node,
            params,
            return_type,
            doc_comment: extract_doc_comment(node, source),
            visibility: vis,
            is_extension,
            receiver_type,
        });
    }
}

fn parse_property<'a>(
    _node: tree_sitter::Node<'a>,
    _source: &str,
    _package: &str,
    _file_name: &str,
    _symbols: &mut Vec<KotlinSymbol<'a>>,
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
                | "object_declaration"
                | "interface_declaration"
                | "enum_class_declaration"
        ) && let Some(name_node) = parent.child_by_field_name("name")
        {
            return Some(node_text(name_node, source).to_string());
        }
        current = parent;
    }
    None
}

fn emit_kotlin_symbol(
    sym: &KotlinSymbol,
    source: &str,
    path: &Path,
    _all_symbols: &[KotlinSymbol],
    _imports: &[KotlinImport],
    package: &str,
    file_name: &str,
) -> Option<Document> {
    let anchor = make_anchor(package, file_name, &sym.name);
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
    if sym.is_extension
        && let Some(ref recv) = sym.receiver_type
    {
        rows.push(vec![
            "Extension".to_string(),
            format!("{}.{}", recv, sym.name),
        ]);
    }
    for p in &sym.params {
        rows.push(vec![
            format!("param {}", p.name),
            format!("{}: {}", p.name, p.ty),
        ]);
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
        && let Some(body) = sym.node.child_by_field_name("body")
    {
        let calls = extract_kotlin_call_sites(body, source);
        let filtered: Vec<_> = calls
            .into_iter()
            .filter(|(c, _)| !is_kotlin_std_noise(c))
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

    // Type-usage edges: types named in the signature (params + return type) are
    // Used, so a type that is used but never called is not a false dead-code
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

const KOTLIN_SKIP_CALLEES: &[&str] = &[
    "toString",
    "equals",
    "hashCode",
    "clone",
    "compareTo",
    "let",
    "run",
    "with",
    "apply",
    "also",
    "takeIf",
    "takeUnless",
    "println",
    "print",
    "TODO",
    "size",
    "length",
    "isEmpty",
    "isNotEmpty",
    "isNullOrEmpty",
    "isNullOrBlank",
    "map",
    "filter",
    "reduce",
    "fold",
    "flatMap",
    "forEach",
    "collect",
    "first",
    "last",
    "single",
    "find",
    "any",
    "none",
    "all",
    "assertEquals",
    "assertTrue",
    "assertFalse",
    "assertNull",
    "getLogger",
    "log",
    "info",
    "warn",
    "error",
    "debug",
    "it",
    "this",
    "super",
    "require",
    "check",
    "error",
    "get",
    "set",
    "put",
    "add",
    "remove",
    "clear",
];

fn is_kotlin_std_noise(name: &str) -> bool {
    KOTLIN_SKIP_CALLEES.contains(&name)
        || (name.starts_with("get") && name.len() <= 6)
        || (name.starts_with("set") && name.len() <= 6)
}

fn extract_kotlin_call_sites(node: tree_sitter::Node, source: &str) -> Vec<(String, usize)> {
    let mut calls = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        calls.extend(extract_kotlin_call_sites(child, source));
    }

    match node.kind() {
        "call_expression" | "function_call" | "method_call" => {
            if let Some(func) = node.child_by_field_name("function") {
                let callee = resolve_kotlin_callee_name(func, source);
                if !callee.is_empty() && callee.len() >= 2 {
                    let line = func.start_position().row + 1;
                    calls.push((callee, line));
                }
            } else {
                // Try first named child as callee
                let mut c = node.walk();
                for child in node.children(&mut c) {
                    if child.is_named() {
                        let text = node_text(child, source).trim();
                        if !text.is_empty() && text.len() >= 2 {
                            calls.push((text.to_string(), child.start_position().row + 1));
                        }
                        break;
                    }
                }
            }
        }
        "object_creation" | "constructor_invocation" => {
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

fn resolve_kotlin_callee_name(node: tree_sitter::Node, source: &str) -> String {
    match node.kind() {
        "identifier" | "simple_identifier" => node_text(node, source).to_string(),
        "navigation_expression" | "field_expression" | "call_expression" => {
            if let Some(name) = node.child_by_field_name("field") {
                node_text(name, source).to_string()
            } else if let Some(name) = node.child_by_field_name("name") {
                node_text(name, source).to_string()
            } else {
                // Try to extract the rightmost identifier
                let mut last = String::new();
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "identifier" || child.kind() == "simple_identifier" {
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
        "scoped_identifier" | "qualified_identifier" => {
            let mut parts = Vec::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "identifier" || child.kind() == "simple_identifier" {
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
