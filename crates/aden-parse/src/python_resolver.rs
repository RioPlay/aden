// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Deep Python resolver — full import + call-site analysis.
//!
//! Handles:
//!   • `import foo`, `from foo import bar`, relative imports
//!   • Intra-file and cross-module call resolution (best-effort)
//!   • Emits `edge::calls[]` macros for graph ingestion

use crate::extractor::{build_code_attributes, make_anchor, LanguageExtractor};
use aden_core::{Block, Document, NodeType, Parameter, Result};
use std::path::Path;

/// Deep Python extractor.
pub struct PythonResolver;

impl PythonResolver {
    pub fn new() -> Self {
        Self
    }
}

impl LanguageExtractor for PythonResolver {
    fn language_id(&self) -> &'static str {
        "python"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["py"]
    }

    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>> {
        let language = tree_sitter_language_pack::get_language("python")
            .map_err(|e| aden_core::Error::Parse(format!("language-pack: {}", e)))?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language)
            .map_err(|e| aden_core::Error::Parse(e.to_string()))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| aden_core::Error::Parse("tree-sitter returned None".to_string()))?;

        let proj_name = infer_python_project_name(path);
        let file_name = path.file_name().unwrap_or_default().to_string_lossy();

        // Phase 1: collect local symbols + imports.
        let mut ctx = ExtractionContext {
            symbols: Vec::new(),
            imports: Vec::new(),
            doc_comments: Vec::new(),
        };
        collect_module_items(tree.root_node(), source, &mut ctx);

        // Phase 2: emit Documents with call-site resolution.
        let mut docs = Vec::new();
        for sym in &ctx.symbols {
            if let Some(doc) = emit_symbol_document(sym, source, path, &ctx, &proj_name, &file_name) {
                docs.push(doc);
            }
        }

        Ok(docs)
    }
}

/// Information about a single local symbol.
#[derive(Debug)]
struct SymbolInfo<'a> {
    name: String,
    qualified_name: String,
    kind: NodeType,
    node: tree_sitter::Node<'a>,
    params: Vec<Parameter>,
    doc_comment: Option<String>,
    is_async: bool,
}

/// An import binding: the local name maps to a (module_path, original_name) pair.
#[derive(Debug)]
struct ImportBinding {
    local_name: String,
    module_path: String,
    original_name: Option<String>, // Some for `from x import y`, None for `import x`
}

#[allow(dead_code)]
struct ExtractionContext<'a> {
    symbols: Vec<SymbolInfo<'a>>,
    imports: Vec<ImportBinding>,
    doc_comments: Vec<String>,
}

/// Walk a module-level AST and populate symbols + imports.
fn collect_module_items<'a>(node: tree_sitter::Node<'a>, source: &str, ctx: &mut ExtractionContext<'a>) {
    if !node.is_named() {
        return;
    }
    match node.kind() {
        "function_definition" | "async_function_definition" => {
            let sym = extract_function_symbol(node, source, "");
            ctx.symbols.push(sym);
        }
        "class_definition" => {
            let sym = extract_class_symbol(node, source);
            ctx.symbols.push(sym);
        }
        "import_statement" => {
            extract_import_statement(node, source, ctx);
        }
        "import_from_statement" => {
            extract_import_from_statement(node, source, ctx);
        }
        "expression_statement" => {
            // Could be a @decorator applied to a class/function; skip for now.
        }
        _ => {}
    }

    // Recurse into children (functions/classes are already handled above,
    // but we need to catch top-level siblings).
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_module_items(child, source, ctx);
    }
}

fn extract_function_symbol<'a>(node: tree_sitter::Node<'a>, source: &str, prefix: &str) -> SymbolInfo<'a> {
    let is_async = node.kind() == "async_function_definition";
    let name_node = node.child_by_field_name("name");
    let name = name_node
        .map(|n| node_text(n, source).to_string())
        .unwrap_or_else(|| "<anon>".to_string());
    let qualified = if prefix.is_empty() {
        name.clone()
    } else {
        format!("{}.{}", prefix, name)
    };

    let mut params = Vec::new();
    if let Some(params_node) = node.child_by_field_name("parameters") {
        let mut pc = params_node.walk();
        for param in params_node.children(&mut pc) {
            if param.kind() == "identifier" {
                params.push(Parameter {
                    name: node_text(param, source).to_string(),
                    ty: "Unknown".to_string(),
                    default_value: None,
                });
            } else if param.kind() == "typed_parameter" {
                let p_name = param
                    .child_by_field_name("name")
                    .map(|n| node_text(n, source).to_string())
                    .unwrap_or_else(|| node_text(param, source).to_string());
                params.push(Parameter {
                    name: p_name,
                    ty: "Unknown".to_string(),
                    default_value: None,
                });
            }
        }
    }

    let doc_comment = extract_preceding_docstring(node, source);

    SymbolInfo {
        name,
        qualified_name: qualified,
        kind: NodeType::Function,
        node,
        params,
        doc_comment,
        is_async,
    }
}

fn extract_class_symbol<'a>(node: tree_sitter::Node<'a>, source: &str) -> SymbolInfo<'a> {
    let name_node = node.child_by_field_name("name");
    let name = name_node
        .map(|n| node_text(n, source).to_string())
        .unwrap_or_else(|| "<anon>".to_string());

    // Collect methods in the body
    let mut params = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if child.kind() == "function_definition" || child.kind() == "async_function_definition" {
                let method = extract_function_symbol(child, source, &name);
                params.push(Parameter {
                    name: method.qualified_name.clone(),
                    ty: format!("{:?}", method.kind),
                    default_value: None,
                });
            }
        }
    }

    let doc_comment = extract_preceding_docstring(node, source);

    SymbolInfo {
        name: name.clone(),
        qualified_name: name,
        kind: NodeType::Type,
        node,
        params,
        doc_comment,
        is_async: false,
    }
}

/// Parse `import foo` and `import foo.bar`.
fn extract_import_statement<'a>(node: tree_sitter::Node<'a>, source: &str, ctx: &mut ExtractionContext<'a>) {
    if let Some(dotted) = node.child_by_field_name("name") {
        let path = node_text(dotted, source).to_string();
        // `import foo.bar as baz`  →  alias is "baz"
        let alias = find_alias(node, source);
        let local = alias.as_deref().unwrap_or_else(|| path.split('.').next().unwrap_or(&path));
        ctx.imports.push(ImportBinding {
            local_name: local.to_string(),
            module_path: path,
            original_name: None,
        });
    }
}

/// Parse `from foo import bar`, `from foo import bar as baz`, `from . import x`.
fn extract_import_from_statement<'a>(node: tree_sitter::Node<'a>, source: &str, ctx: &mut ExtractionContext<'a>) {
    let module_path = if let Some(module_node) = node.child_by_field_name("module_name") {
        node_text(module_node, source).to_string()
    } else {
        // relative import: from . import x
        let mut rel_parts = Vec::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "relative_import" {
                let text = node_text(child, source);
                let dots = text.chars().filter(|c| *c == '.').count();
                rel_parts.push(".".repeat(dots));
            }
        }
        rel_parts.join("")
    };

    // Collect each `name` / `alias` pair.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "dotted_name" {
            // `from foo.bar import baz` - module_name is "foo.bar"
            continue;
        }
        if child.kind() == "identifier" {
            let local_name = node_text(child, source).to_string();
            ctx.imports.push(ImportBinding {
                local_name: local_name.clone(),
                module_path: module_path.clone(),
                original_name: Some(local_name),
            });
        }
        // alias children handled by walking further
    }
}

fn find_alias(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return Some(node_text(child, source).to_string());
        }
    }
    None
}

/// Emit a Document for a symbol, including resolved call sites.
fn emit_symbol_document<'a>(
    sym: &SymbolInfo<'a>,
    source: &str,
    path: &Path,
    ctx: &ExtractionContext<'a>,
    proj_name: &str,
    file_name: &str,
) -> Option<Document> {
    let anchor = make_anchor(proj_name, file_name, &sym.name);
    let span = node_to_span(sym.node, path);
    let attrs = build_code_attributes(source, &format!("{:?}", sym.kind).to_lowercase(), Some(path), Some(&span));
    let mut blocks = Vec::new();

    if let Some(ref doc) = sym.doc_comment {
        blocks.push(Block::Paragraph(doc.clone()));
    }

    // Signature table
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

    // Resolve call sites inside the symbol body
    let calls = resolve_call_sites(sym.node, source, ctx);
    if !calls.is_empty() {
        let call_rows: Vec<Vec<String>> = calls.iter()
            .map(|c| vec![c.callee.clone(), c.line.to_string()])
            .collect();
        blocks.push(Block::Table(aden_core::Table {
            headers: vec!["Callee".to_string(), "Line".to_string()],
            rows: call_rows,
        }));

        let edge_code = calls.iter()
            .map(|c| format!("edge::calls[{}]", c.callee))
            .collect::<Vec<_>>()
            .join("\n");
        blocks.push(Block::Listing { language: None, code: edge_code });
    }

    Some(Document {
        anchor,
        node_type: sym.kind.clone(),
        attributes: attrs,
        blocks,
        source_span: Some(span),
    })
}

/// A resolved call site.
struct CallSite {
    callee: String,
    line: usize,
}

/// Recursively find call expressions and attempt to resolve them.
fn resolve_call_sites<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    ctx: &ExtractionContext<'a>,
) -> Vec<CallSite> {
    let mut calls = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        calls.extend(resolve_call_sites(child, source, ctx));
    }

    if node.kind() == "call" {
        if let Some(func) = node.child_by_field_name("function") {
            let callee = resolve_callee(func, source, ctx);
            if !callee.is_empty() && callee.len() >= 3 && !is_std_noise(&callee) {
                let line = func.start_position().row + 1;
                calls.push(CallSite { callee, line });
            }
        }
    }
    calls
}

fn resolve_callee<'a>(node: tree_sitter::Node<'a>, source: &str, ctx: &ExtractionContext<'a>) -> String {
    match node.kind() {
        "identifier" => {
            let name = node_text(node, source).trim().to_string();
            // Try to resolve via imports.
            for imp in &ctx.imports {
                if imp.local_name == name {
                    return format!("{}.{}", imp.module_path, imp.original_name.as_ref().unwrap_or(&imp.local_name));
                }
            }
            // Local symbol?
            if ctx.symbols.iter().any(|s| s.name == name) {
                return name;
            }
            name
        }
        "attribute" => {
            // e.g. `module.func` → try to resolve module prefix
            let obj = node.child_by_field_name("object").map(|n| node_text(n, source).trim().to_string());
            let attr = node.child_by_field_name("attribute").map(|n| node_text(n, source).trim().to_string());
            match (obj, attr) {
                (Some(o), Some(a)) => {
                    for imp in &ctx.imports {
                        if imp.local_name == o {
                            return format!("{}.{}", imp.module_path, a);
                        }
                    }
                    format!("{}.{}", o, a)
                }
                _ => node_text(node, source).trim().to_string(),
            }
        }
        "call" => resolve_callee(
            node.child_by_field_name("function").unwrap_or(node),
            source,
            ctx,
        ),
        _ => node_text(node, source).trim().to_string(),
    }
}

/// Standard-library and common utility functions to skip.
fn is_std_noise(name: &str) -> bool {
    const SKIP: &[&str] = &[
        "print", "len", "range", "str", "int", "float", "list", "dict", "set",
        "tuple", "map", "filter", "sorted", "sum", "min", "max",
        "isinstance", "hasattr", "getattr", "setattr", "delattr",
        "open", "read", "write", "close", "join", "split", "strip",
        "append", "extend", "insert", "remove", "pop", "clear",
        "keys", "values", "items", "get", "update",
        "add", "discard", "copy", "deepcopy", "clone",
        "format", "repr", "ascii", "ord", "chr", "bin", "hex", "oct",
        "abs", "round", "pow", "divmod", "complex", "bool",
        "enumerate", "zip", "reversed", "iter", "next",
        "any", "all", "bool", "bytes", "bytearray", "memoryview",
        "super", "self", "cls",
    ];
    SKIP.contains(&name)
}

fn extract_preceding_docstring<'a>(node: tree_sitter::Node<'a>, source: &str) -> Option<String> {
    // In Python, docstrings are string literals immediately inside a function/class body.
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if child.kind() == "expression_statement" {
                let mut inner = child.walk();
                for inner_child in child.children(&mut inner) {
                    if inner_child.kind() == "string" {
                        let text = node_text(inner_child, source).trim();
                        return Some(text.to_string());
                    }
                }
            }
            // Only look at the first statement
            break;
        }
    }
    None
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

/// Best-effort project name from filesystem markers.
fn infer_python_project_name(path: &Path) -> String {
    path.ancestors()
        .find(|p| {
            p.join("pyproject.toml").exists()
                || p.join("setup.py").exists()
                || p.join("setup.cfg").exists()
                || p.join("Cargo.toml").exists()
                || p.join("package.json").exists()
        })
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
