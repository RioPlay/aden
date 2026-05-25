// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Deep TypeScript / JavaScript resolver.
//!
//! Handles:
//!   • ES modules: `import { x } from './path'`, `import x from './path'`
//!   • CommonJS: `const x = require('./path')`
//!   • Function declarations, class declarations, method definitions
//!   • Cross-file call resolution (best-effort, extension inference)
//!
//! Phase 2 second pass (not yet implemented):
//!   • Path aliases from `tsconfig.json` / `jsconfig.json`
//!   • Dynamic imports `import('./path')`

use crate::extractor::{LanguageExtractor, build_code_attributes, make_anchor};
use aden_core::{Block, Document, NodeType, Parameter, Result};
use std::path::Path;

/// Deep TypeScript / JavaScript extractor.
pub struct TypeScriptResolver;

impl Default for TypeScriptResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl TypeScriptResolver {
    pub fn new() -> Self {
        Self
    }
}

impl LanguageExtractor for TypeScriptResolver {
    fn language_id(&self) -> &'static str {
        "typescript"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["ts", "tsx", "js", "jsx", "mjs", "cjs"]
    }

    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>> {
        // Try TypeScript parser first, fall back to JavaScript if TS fails.
        let language = tree_sitter_language_pack::get_language("typescript")
            .or_else(|_| tree_sitter_language_pack::get_language("javascript"))
            .map_err(|e| aden_core::Error::Parse(format!("language-pack: {}", e)))?;

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language)
            .map_err(|e| aden_core::Error::Parse(e.to_string()))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| aden_core::Error::Parse("tree-sitter returned None".to_string()))?;

        let proj_name = infer_ts_project_name(path);
        let file_name = path.file_name().unwrap_or_default().to_string_lossy();

        let mut symbols: Vec<TsSymbol> = Vec::new();
        let mut imports: Vec<TsImport> = Vec::new();
        walk_program(tree.root_node(), source, &mut symbols, &mut imports);

        let mut docs = Vec::new();
        for sym in &symbols {
            if let Some(doc) = emit_ts_symbol(
                sym, source, path, &symbols, &imports, &proj_name, &file_name,
            ) {
                docs.push(doc);
            }
        }

        Ok(docs)
    }
}

#[derive(Debug)]
struct TsImport {
    local_name: String,
    source_path: String, // the raw string literal from the import/require
    original_name: Option<String>, // None for default/namespace imports
}

#[derive(Debug)]
struct TsSymbol<'a> {
    name: String,
    kind: NodeType,
    node: tree_sitter::Node<'a>,
    params: Vec<Parameter>,
    doc_comment: Option<String>,
    is_async: bool,
    is_export: bool,
}

fn walk_program<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    symbols: &mut Vec<TsSymbol<'a>>,
    imports: &mut Vec<TsImport>,
) {
    if !node.is_named() {
        return;
    }

    match node.kind() {
        "function_declaration" | "function" => {
            if let Some(sym) = extract_function_symbol(node, source, false) {
                symbols.push(sym);
            }
        }
        "arrow_function" => {
            // Could be assigned to a variable; name comes from parent variable_declarator
            if let Some(sym) = extract_arrow_function_symbol(node, source) {
                symbols.push(sym);
            }
        }
        "class_declaration" | "class" => {
            if let Some(sym) = extract_class_symbol(node, source) {
                symbols.push(sym);
            }
        }
        "method_definition" => {
            if let Some(sym) = extract_method_symbol(node, source) {
                symbols.push(sym);
            }
        }
        "import_statement" | "import_declaration" => {
            extract_import_statement(node, source, imports);
        }
        "expression_statement" => {
            extract_require_statement(node, source, imports);
        }
        "export_statement" | "export_declaration" => {
            // If the export wraps a declaration, unwrap and mark as exported.
            if let Some(declaration) = node.child_by_field_name("declaration") {
                if let Some(mut sym) = extract_function_symbol(declaration, source, true) {
                    sym.is_export = true;
                    symbols.push(sym);
                } else if let Some(mut sym) = extract_class_symbol(declaration, source) {
                    sym.is_export = true;
                    symbols.push(sym);
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_program(child, source, symbols, imports);
    }
}

fn extract_function_symbol<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    exported: bool,
) -> Option<TsSymbol<'a>> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source).to_string();
    let is_async =
        node.kind() == "function_declaration" || node_text(node, source).contains("async");
    let params = extract_ts_params(node, source);
    let doc = extract_ts_doc_comment(node, source);

    Some(TsSymbol {
        name,
        kind: NodeType::Function,
        node,
        params,
        doc_comment: doc,
        is_async,
        is_export: exported,
    })
}

fn extract_arrow_function_symbol<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
) -> Option<TsSymbol<'a>> {
    // Look for a parent variable_declarator to get the name.
    let mut current = node.parent()?;
    while let Some(parent) = current.parent() {
        if parent.kind() == "variable_declarator" {
            let name_node = parent.child_by_field_name("name")?;
            let name = node_text(name_node, source).to_string();
            let params = extract_ts_params(node, source);
            let doc = extract_ts_doc_comment(parent, source);
            return Some(TsSymbol {
                name,
                kind: NodeType::Function,
                node,
                params,
                doc_comment: doc,
                is_async: false,
                is_export: false,
            });
        }
        current = parent;
        if current.kind() == "statement_block" || current.kind() == "program" {
            break;
        }
    }
    None
}

fn extract_class_symbol<'a>(node: tree_sitter::Node<'a>, source: &str) -> Option<TsSymbol<'a>> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source).to_string();
    let doc = extract_ts_doc_comment(node, source);

    Some(TsSymbol {
        name,
        kind: NodeType::Type,
        node,
        params: Vec::new(),
        doc_comment: doc,
        is_async: false,
        is_export: false,
    })
}

fn extract_method_symbol<'a>(node: tree_sitter::Node<'a>, source: &str) -> Option<TsSymbol<'a>> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source).to_string();
    let is_async = node.kind().contains("async") || node_text(node, source).contains("async");
    let params = extract_ts_params(node, source);
    let doc = extract_ts_doc_comment(node, source);

    // Qualify with class name if inside a class
    let class_name = node
        .parent()
        .and_then(|p| p.child_by_field_name("name"))
        .map(|n| node_text(n, source).to_string());
    let qualified = class_name
        .map(|c| format!("{}.{}", c, name))
        .unwrap_or(name);

    Some(TsSymbol {
        name: qualified,
        kind: NodeType::Function,
        node,
        params,
        doc_comment: doc,
        is_async,
        is_export: false,
    })
}

fn extract_ts_params(node: tree_sitter::Node, source: &str) -> Vec<Parameter> {
    let mut params = Vec::new();
    if let Some(params_node) = node.child_by_field_name("parameters") {
        let mut cursor = params_node.walk();
        for child in params_node.children(&mut cursor) {
            if child.kind() == "formal_parameter" || child.kind() == "required_parameter" {
                let mut p_cursor = child.walk();
                for p_child in child.children(&mut p_cursor) {
                    if p_child.kind() == "identifier" {
                        let name = node_text(p_child, source).to_string();
                        params.push(Parameter {
                            name,
                            ty: "Unknown".to_string(),
                            default_value: None,
                        });
                    }
                }
            } else if child.kind() == "identifier" {
                let name = node_text(child, source).to_string();
                params.push(Parameter {
                    name,
                    ty: "Unknown".to_string(),
                    default_value: None,
                });
            }
        }
    }
    params
}

fn extract_import_statement(node: tree_sitter::Node, source: &str, imports: &mut Vec<TsImport>) {
    let source_node = node.child_by_field_name("source");
    let source_path = source_node
        .map(|n| {
            node_text(n, source)
                .trim_matches('"')
                .trim_matches('\'')
                .to_string()
        })
        .unwrap_or_default();
    if source_path.is_empty() {
        return;
    }

    // import { a, b } from './path'
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "import_clause" {
            let mut inner = child.walk();
            for inner_child in child.children(&mut inner) {
                if inner_child.kind() == "named_imports" {
                    // { a, b }
                    let mut named_cursor = inner_child.walk();
                    for spec in inner_child.children(&mut named_cursor) {
                        if spec.kind() == "import_specifier" {
                            let local = spec
                                .child_by_field_name("name")
                                .map(|n| node_text(n, source).to_string())
                                .unwrap_or_default();
                            let original = spec
                                .child_by_field_name("alias")
                                .map(|n| node_text(n, source).to_string());
                            imports.push(TsImport {
                                local_name: local.clone(),
                                source_path: source_path.clone(),
                                original_name: Some(original.unwrap_or(local)),
                            });
                        }
                    }
                } else if inner_child.kind() == "identifier" {
                    // default import: import x from './path'
                    let name = node_text(inner_child, source).to_string();
                    imports.push(TsImport {
                        local_name: name,
                        source_path: source_path.clone(),
                        original_name: None,
                    });
                } else if inner_child.kind() == "namespace_import" {
                    // import * as x from './path'
                    if let Some(name) = inner_child.child_by_field_name("name") {
                        let local = node_text(name, source).to_string();
                        imports.push(TsImport {
                            local_name: local,
                            source_path: source_path.clone(),
                            original_name: None,
                        });
                    }
                }
            }
        }
    }
}

fn extract_require_statement(node: tree_sitter::Node, source: &str, imports: &mut Vec<TsImport>) {
    // expression_statement → call_expression(require) → identifier
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "call_expression"
            && let Some(func) = child.child_by_field_name("function")
            && func.kind() == "identifier"
            && node_text(func, source) == "require"
            && let Some(args) = child.child_by_field_name("arguments")
        {
            let mut arg_cursor = args.walk();
            for arg in args.children(&mut arg_cursor) {
                if arg.kind() == "string" || arg.kind() == "string_fragment" {
                    let raw = node_text(arg, source);
                    let path = raw.trim_matches('"').trim_matches('\'').to_string();
                    // Try to find the variable name from parent variable_declarator
                    let local = node
                        .parent()
                        .and_then(|p| p.parent())
                        .and_then(|gp| gp.child_by_field_name("name"))
                        .map(|n| node_text(n, source).to_string())
                        .unwrap_or_else(|| "require".to_string());
                    imports.push(TsImport {
                        local_name: local,
                        source_path: path,
                        original_name: None,
                    });
                }
            }
        }
    }
}

fn extract_ts_doc_comment(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut current = node.prev_named_sibling();
    while let Some(sib) = current {
        if sib.kind() == "comment" {
            let text = node_text(sib, source).trim();
            if text.starts_with("/**") || text.starts_with("///") || text.starts_with("/*") {
                return Some(text.to_string());
            }
        } else {
            break;
        }
        current = sib.prev_named_sibling();
    }
    None
}

fn emit_ts_symbol<'a>(
    sym: &TsSymbol<'a>,
    source: &str,
    path: &Path,
    all_symbols: &[TsSymbol<'a>],
    imports: &[TsImport],
    proj_name: &str,
    file_name: &str,
) -> Option<Document> {
    let anchor = make_anchor(proj_name, file_name, &sym.name);
    let span = node_to_span(sym.node, path);
    let attrs = build_code_attributes(
        source,
        &format!("{:?}", sym.kind).to_lowercase(),
        Some(path),
        Some(&span),
    );
    let mut blocks = Vec::new();

    if sym.is_export {
        blocks.push(Block::Paragraph("Exported symbol.".to_string()));
    }

    if let Some(ref doc) = sym.doc_comment {
        blocks.push(Block::Paragraph(doc.clone()));
    }

    let mut sig_rows = vec![vec!["Name".to_string(), sym.name.clone()]];
    for p in &sym.params {
        sig_rows.push(vec![format!("param {}", p.name), p.ty.clone()]);
    }
    if sym.is_async {
        sig_rows.push(vec!["Async".to_string(), "true".to_string()]);
    }
    if !sig_rows.is_empty() {
        blocks.push(Block::Paragraph("== Signature".to_string()));
        blocks.push(Block::Table(aden_core::Table {
            headers: vec!["Property".to_string(), "Value".to_string()],
            rows: sig_rows,
        }));
    }

    // Resolve call sites
    let calls = resolve_ts_call_sites(sym.node, source, all_symbols, imports);
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

    Some(Document {
        anchor,
        node_type: sym.kind.clone(),
        attributes: attrs,
        blocks,
        source_span: Some(span),
    })
}

struct TsCallSite {
    callee: String,
    line: usize,
}

fn resolve_ts_call_sites<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    all_symbols: &[TsSymbol<'a>],
    imports: &[TsImport],
) -> Vec<TsCallSite> {
    let mut calls = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        calls.extend(resolve_ts_call_sites(child, source, all_symbols, imports));
    }

    if node.kind() == "call_expression"
        && let Some(func) = node.child_by_field_name("function")
    {
        let callee = resolve_ts_callee(func, source, all_symbols, imports);
        if !callee.is_empty() && callee.len() >= 2 && !is_ts_std_noise(&callee) {
            let line = func.start_position().row + 1;
            calls.push(TsCallSite { callee, line });
        }
    }
    calls
}

fn resolve_ts_callee(
    node: tree_sitter::Node,
    source: &str,
    _all_symbols: &[TsSymbol],
    imports: &[TsImport],
) -> String {
    match node.kind() {
        "identifier" => {
            let name = node_text(node, source).trim().to_string();
            // Try import resolution
            for imp in imports {
                if imp.local_name == name {
                    return format!(
                        "{}.{}",
                        imp.source_path,
                        imp.original_name.as_ref().unwrap_or(&name)
                    );
                }
            }
            name
        }
        "member_expression" => {
            let obj = node
                .child_by_field_name("object")
                .map(|n| node_text(n, source).trim().to_string());
            let prop = node
                .child_by_field_name("property")
                .map(|n| node_text(n, source).trim().to_string());
            match (obj, prop) {
                (Some(o), Some(p)) => {
                    for imp in imports {
                        if imp.local_name == o {
                            return format!("{}.{}", imp.source_path, p);
                        }
                    }
                    // Could be a method on a local class instance
                    format!("{}.{}", o, p)
                }
                (Some(o), None) => o,
                (None, Some(p)) => p,
                _ => node_text(node, source).trim().to_string(),
            }
        }
        "call_expression" => resolve_ts_callee(
            node.child_by_field_name("function").unwrap_or(node),
            source,
            _all_symbols,
            imports,
        ),
        _ => node_text(node, source).trim().to_string(),
    }
}

fn is_ts_std_noise(name: &str) -> bool {
    const SKIP: &[&str] = &[
        "console.log",
        "console.error",
        "console.warn",
        "console.info",
        "console.debug",
        "console.assert",
        "console.trace",
        "Math.abs",
        "Math.floor",
        "Math.ceil",
        "Math.round",
        "Math.random",
        "Math.max",
        "Math.min",
        "Math.pow",
        "Math.sqrt",
        "Array.from",
        "Array.isArray",
        "Array.of",
        "Object.keys",
        "Object.values",
        "Object.entries",
        "Object.assign",
        "JSON.parse",
        "JSON.stringify",
        "setTimeout",
        "setInterval",
        "clearTimeout",
        "clearInterval",
        "parseInt",
        "parseFloat",
        "isNaN",
        "isFinite",
        "String.prototype.trim",
        "String.prototype.split",
        "String.prototype.slice",
        "toString",
        "valueOf",
        "hasOwnProperty",
        "require",
        "module.exports",
        "exports",
        "process.exit",
        "process.nextTick",
        "Buffer.from",
        "Buffer.alloc",
        "Buffer.allocUnsafe",
        "Date.now",
        "Date.parse",
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

fn infer_ts_project_name(path: &Path) -> String {
    path.ancestors()
        .find(|p| {
            p.join("package.json").exists()
                || p.join("tsconfig.json").exists()
                || p.join("jsconfig.json").exists()
        })
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
