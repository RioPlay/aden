// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Deep C resolver — function + struct + include + call-site analysis.
//!
//! Handles:
//!   • Function definitions and prototypes
//!   • Struct, union, enum, and typedef declarations
//!   • `#include` resolution (local `"..."` and system `<...>`)
//!   • Intra-file call-graph edges via `edge::calls[]` macros

use crate::extractor::{LanguageExtractor, build_code_attributes, make_anchor};
use aden_core::{Block, Document, NodeType, Result};
use std::path::Path;

/// Deep C extractor.
pub struct CResolver;

impl Default for CResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl CResolver {
    pub fn new() -> Self {
        Self
    }
}

impl LanguageExtractor for CResolver {
    fn language_id(&self) -> &'static str {
        "c"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["c", "h"]
    }

    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>> {
        let language = tree_sitter_language_pack::get_language("c")
            .map_err(|e| aden_core::Error::Parse(format!("language-pack: {}", e)))?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language)
            .map_err(|e| aden_core::Error::Parse(e.to_string()))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| aden_core::Error::Parse("tree-sitter returned None".to_string()))?;

        let module_name = infer_project_name(path);
        let file_name = path.file_name().unwrap_or_default().to_string_lossy();

        let mut symbols: Vec<CSymbol> = Vec::new();
        let mut includes: Vec<CInclude> = Vec::new();
        walk_translation_unit(tree.root_node(), source, &mut symbols, &mut includes);

        let mut docs = Vec::new();
        for sym in &symbols {
            if let Some(doc) = emit_c_symbol(
                sym,
                source,
                path,
                &symbols,
                &includes,
                &module_name,
                &file_name,
            ) {
                docs.push(doc);
            }
        }

        Ok(docs)
    }
}

struct CInclude {
    path: String,
    is_system: bool,
}

#[derive(Debug)]
struct CSymbol<'a> {
    name: String,
    kind: NodeType,
    node: tree_sitter::Node<'a>,
    doc_comment: Option<String>,
}

fn infer_project_name(path: &Path) -> String {
    path.ancestors()
        .find(|p| {
            p.join("Cargo.toml").exists()
                || p.join("package.json").exists()
                || p.join("pyproject.toml").exists()
                || p.join("setup.py").exists()
                || p.join("go.mod").exists()
                || p.join("tsconfig.json").exists()
                || p.join("Makefile").exists()
                || p.join("CMakeLists.txt").exists()
        })
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn walk_translation_unit<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    symbols: &mut Vec<CSymbol<'a>>,
    includes: &mut Vec<CInclude>,
) {
    if !node.is_named() {
        return;
    }

    match node.kind() {
        "function_definition" => {
            if let Some(name) = extract_function_name(node, source) {
                let doc = extract_c_doc_comment(node, source);
                symbols.push(CSymbol {
                    name,
                    kind: NodeType::Function,
                    node,
                    doc_comment: doc,
                });
            }
        }
        "declaration" => {
            if let Some((name, kind)) = extract_declaration_info(node, source) {
                let doc = extract_c_doc_comment(node, source);
                symbols.push(CSymbol {
                    name,
                    kind,
                    node,
                    doc_comment: doc,
                });
            }
        }
        "type_definition" => {
            if let Some(name) = extract_typedef_name(node, source) {
                let doc = extract_c_doc_comment(node, source);
                symbols.push(CSymbol {
                    name,
                    kind: NodeType::Type,
                    node,
                    doc_comment: doc,
                });
            }
        }
        "struct_specifier" | "union_specifier" | "enum_specifier" => {
            // Only extract top-level definitions, not type references in parameters.
            if node
                .parent()
                .map(|p| p.kind() == "translation_unit")
                .unwrap_or(false)
                && let Some(name) = extract_type_specifier_name(node, source)
            {
                let doc = extract_c_doc_comment(node, source);
                symbols.push(CSymbol {
                    name,
                    kind: NodeType::Type,
                    node,
                    doc_comment: doc,
                });
            }
        }
        "preproc_include" => {
            extract_include(node, source, includes);
        }
        _ => {}
    }

    // Recurse into children, but skip compound_statement subtrees
    // to avoid extracting local variables as top-level symbols.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "compound_statement" {
            continue;
        }
        walk_translation_unit(child, source, symbols, includes);
    }
}

/// Extract function name from a function_definition node.
fn extract_function_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    let declarator = node.child_by_field_name("declarator")?;
    find_declarator_name(declarator, source)
}

/// Extract name + kind from a declaration node (prototype, struct, enum, etc.).
fn extract_declaration_info(node: tree_sitter::Node, source: &str) -> Option<(String, NodeType)> {
    // 1. Function prototype / definition (declarator contains the name)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if (kind == "function_declarator"
            || kind == "init_declarator"
            || kind == "parenthesized_declarator")
            && let Some(name) = find_declarator_name(child, source)
        {
            return Some((name, NodeType::Function));
        }
    }
    // 2. Struct / union / enum — search declaration subtree recursively
    if let Some(name) = find_type_name(node, source) {
        return Some((name, NodeType::Type));
    }
    None
}

/// Recursively search for struct/union/enum or typedef name inside a declaration.
fn find_type_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    if node.kind() == "struct_specifier"
        || node.kind() == "union_specifier"
        || node.kind() == "enum_specifier"
    {
        return node
            .child_by_field_name("name")
            .map(|n| node_text(n, source).trim().to_string())
            .or_else(|| {
                let mut c = node.walk();
                for child in node.children(&mut c) {
                    if child.kind() == "type_identifier" || child.kind() == "identifier" {
                        return Some(node_text(child, source).trim().to_string());
                    }
                }
                None
            });
    }
    if node.kind() == "type_definition" {
        return extract_typedef_name(node, source);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(name) = find_type_name(child, source) {
            return Some(name);
        }
    }
    None
}

/// Extract name from a struct/union/enum specifier node.
fn extract_type_specifier_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    node.child_by_field_name("name")
        .map(|n| node_text(n, source).trim().to_string())
        .or_else(|| {
            let mut c = node.walk();
            for child in node.children(&mut c) {
                if child.kind() == "type_identifier" || child.kind() == "identifier" {
                    return Some(node_text(child, source).trim().to_string());
                }
            }
            None
        })
}

/// Extract typedef name.
fn extract_typedef_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if kind == "type_identifier" || kind == "identifier" {
            return Some(node_text(child, source).trim().to_string());
        }
    }
    None
}

/// Recursively find an identifier inside a declarator tree.
fn find_declarator_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    if node.kind() == "identifier" {
        return Some(node_text(node, source).trim().to_string());
    }
    if (node.kind() == "pointer_declarator"
        || node.kind() == "parenthesized_declarator"
        || node.kind() == "array_declarator"
        || node.kind() == "function_declarator")
        && let Some(inner) = node.child_by_field_name("declarator")
    {
        return find_declarator_name(inner, source);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(name) = find_declarator_name(child, source) {
            return Some(name);
        }
    }
    None
}

fn extract_include(node: tree_sitter::Node, source: &str, includes: &mut Vec<CInclude>) {
    let mut path_str = None;
    let mut is_system = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "string_literal" => {
                let raw = node_text(child, source).trim();
                path_str = Some(raw.trim_matches('"').to_string());
            }
            "system_lib_string" => {
                let raw = node_text(child, source).trim();
                path_str = Some(raw.trim_matches('<').trim_matches('>').to_string());
                is_system = true;
            }
            "preproc_file_specifier" => {
                let raw = node_text(child, source).trim();
                path_str = Some(raw.to_string());
            }
            _ => {}
        }
    }
    if let Some(path) = path_str {
        includes.push(CInclude { path, is_system });
    }
}

fn extract_c_doc_comment(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut current = node.prev_named_sibling();
    while let Some(sib) = current {
        if sib.kind() == "comment" {
            let text = node_text(sib, source).trim();
            if text.starts_with("/**") || text.starts_with("/*!") || text.starts_with("///") {
                return Some(text.to_string());
            }
        } else {
            break;
        }
        current = sib.prev_named_sibling();
    }
    None
}

fn emit_c_symbol<'a>(
    sym: &CSymbol<'a>,
    source: &str,
    path: &Path,
    all_symbols: &[CSymbol<'a>],
    includes: &[CInclude],
    module: &str,
    file_name: &str,
) -> Option<Document> {
    let anchor = make_anchor(module, file_name, &sym.name);
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

    blocks.push(Block::Paragraph(format!(
        "C {} from `{}`.",
        format!("{:?}", sym.kind).to_lowercase(),
        module
    )));

    // Include table (module-level, emitted once per file on first symbol)
    if !includes.is_empty() {
        let inc_rows: Vec<Vec<String>> = includes
            .iter()
            .map(|i| {
                vec![
                    i.path.clone(),
                    if i.is_system {
                        "system".to_string()
                    } else {
                        "local".to_string()
                    },
                ]
            })
            .collect();
        blocks.push(Block::Table(aden_core::Table {
            headers: vec!["Include".to_string(), "Kind".to_string()],
            rows: inc_rows,
        }));
    }

    // Resolve call sites inside the function body
    if let Some(body) = sym.node.child_by_field_name("body") {
        let calls = resolve_c_call_sites(body, source, all_symbols);
        if !calls.is_empty() {
            let call_rows: Vec<Vec<String>> = calls
                .iter()
                .map(|c| vec![c.callee.clone(), c.line.to_string()])
                .collect();
            blocks.push(Block::Table(aden_core::Table {
                headers: vec!["Callee".to_string(), "Line".to_string()],
                rows: call_rows,
            }));
            let edge_code = calls
                .iter()
                .map(|c| format!("edge::calls[{}]", c.callee))
                .collect::<Vec<_>>()
                .join("\n");
            blocks.push(Block::Listing {
                language: None,
                code: edge_code,
            });
        }
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

struct CCallSite {
    callee: String,
    line: usize,
}

fn resolve_c_call_sites<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    all_symbols: &[CSymbol<'a>],
) -> Vec<CCallSite> {
    let mut calls = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        calls.extend(resolve_c_call_sites(child, source, all_symbols));
    }

    if node.kind() == "call_expression"
        && let Some(func) = node.child_by_field_name("function")
    {
        let callee = resolve_c_callee(func, source, all_symbols);
        if !callee.is_empty() && callee.len() >= 2 && !is_c_std_noise(&callee) {
            let line = func.start_position().row + 1;
            calls.push(CCallSite { callee, line });
        }
    }
    calls
}

fn resolve_c_callee(node: tree_sitter::Node, source: &str, all_symbols: &[CSymbol]) -> String {
    match node.kind() {
        "identifier" => {
            let name = node_text(node, source).trim().to_string();
            if all_symbols.iter().any(|s| s.name == name) {
                return name;
            }
            name
        }
        "field_expression" => {
            let obj = node
                .child_by_field_name("argument")
                .map(|n| node_text(n, source).trim().to_string());
            let field = node
                .child_by_field_name("field")
                .map(|n| node_text(n, source).trim().to_string());
            match (obj, field) {
                (Some(o), Some(f)) => format!("{}.{}", o, f),
                (Some(o), None) => o,
                (None, Some(f)) => f,
                _ => node_text(node, source).trim().to_string(),
            }
        }
        "call_expression" => resolve_c_callee(
            node.child_by_field_name("function").unwrap_or(node),
            source,
            all_symbols,
        ),
        _ => node_text(node, source).trim().to_string(),
    }
}

fn is_c_std_noise(name: &str) -> bool {
    const SKIP: &[&str] = &[
        "printf", "fprintf", "sprintf", "snprintf", "malloc", "calloc", "realloc", "free",
        "memcpy", "memmove", "memset", "memcmp", "strcpy", "strncpy", "strcat", "strncat",
        "strcmp", "strncmp", "strlen", "strchr", "fopen", "fclose", "fread", "fwrite", "scanf",
        "sscanf", "fscanf", "getchar", "putchar", "puts", "gets", "assert", "exit", "abort",
        "qsort", "abs", "labs", "rand", "srand", "time", "sin", "cos", "tan", "sqrt", "pow", "log",
        "exp", "ceil", "floor", "round", "fabs", "sizeof", "offsetof",
    ];
    SKIP.contains(&name)
}

fn node_text<'a>(node: tree_sitter::Node<'a>, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

fn node_to_span<'a>(node: tree_sitter::Node<'a>, path: &Path) -> aden_core::SourceSpan {
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
