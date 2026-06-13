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

use crate::extractor::{
    LanguageExtractor, build_code_attributes, infer_project_name, infer_project_root, make_anchor,
    project_relative_file,
};
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

        let proj_name = infer_project_name(path);
        let project_root = infer_project_root(path);
        let file_name_owned = project_relative_file(path, &project_root);
        let file_name = file_name_owned.as_str();

        let mut symbols: Vec<TsSymbol> = Vec::new();
        let mut imports: Vec<TsImport> = Vec::new();
        walk_program(tree.root_node(), source, &mut symbols, &mut imports);

        let mut docs = Vec::new();
        for sym in &symbols {
            if let Some(doc) =
                emit_ts_symbol(sym, source, path, &symbols, &imports, &proj_name, file_name)
            {
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
        "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
            if let Some(sym) = extract_type_symbol(node, source) {
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
            // We extract the declaration here and then recurse into its *children*
            // (not the declaration node itself) to catch nested symbols such as
            // methods inside an exported class.  Without this guard the plain
            // `_ => {}` recursion below would visit the `function_declaration` /
            // `class_declaration` node a second time and emit a duplicate symbol.
            if let Some(declaration) = node.child_by_field_name("declaration") {
                if let Some(mut sym) = extract_function_symbol(declaration, source, true) {
                    sym.is_export = true;
                    symbols.push(sym);
                } else if let Some(mut sym) = extract_class_symbol(declaration, source) {
                    sym.is_export = true;
                    symbols.push(sym);
                } else if matches!(
                    declaration.kind(),
                    "interface_declaration" | "type_alias_declaration" | "enum_declaration"
                ) && let Some(mut sym) = extract_type_symbol(declaration, source)
                {
                    sym.is_export = true;
                    symbols.push(sym);
                }
                // Recurse into the declaration's children (e.g. class body for
                // methods) but NOT into the declaration node itself — that was
                // already handled above and re-entering it would produce duplicates.
                let mut inner_cursor = declaration.walk();
                for child in declaration.children(&mut inner_cursor) {
                    walk_program(child, source, symbols, imports);
                }
            }
            // Also recurse into any non-declaration children of the export node
            // (e.g. export-list clauses, `from` specifiers that carry imports).
            let decl_id = node.child_by_field_name("declaration").map(|d| d.id());
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if decl_id != Some(child.id()) {
                    walk_program(child, source, symbols, imports);
                }
            }
            return;
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
    // Only treat the arrow as a named function when it is the *direct value* of a
    // variable declarator, i.e. `const f = () => {...}`. An arrow that is merely
    // nested inside the initializer — e.g. an argument to a builder call like
    // `export const ZodFile = base$(() => {...})` or `$ZodIssueTooSmall = z(...)` —
    // does NOT name a function; the declared symbol is a value/type, not a
    // callable. Walking up to *any* ancestor declarator (the old behaviour)
    // mislabeled every such schema/value as a Function, which is what made the
    // zod corpus 73% false-dead. Require the declarator's `value` field to be
    // exactly this arrow node.
    let parent = node.parent()?;
    if parent.kind() != "variable_declarator" {
        return None;
    }
    match parent.child_by_field_name("value") {
        Some(v) if v.id() == node.id() => {}
        _ => return None,
    }
    let name_node = parent.child_by_field_name("name")?;
    let name = node_text(name_node, source).to_string();
    let params = extract_ts_params(node, source);
    let doc = extract_ts_doc_comment(parent, source);
    Some(TsSymbol {
        name,
        kind: NodeType::Function,
        node,
        params,
        doc_comment: doc,
        is_async: false,
        is_export: false,
    })
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

/// Extract a TypeScript type-level declaration — `interface`, `type` alias, or
/// `enum`. These are all `NodeType::Type`; previously they were not extracted at
/// all (so an interface used only as a type annotation looked like dead code, and
/// nothing pointed back at it).
fn extract_type_symbol<'a>(node: tree_sitter::Node<'a>, source: &str) -> Option<TsSymbol<'a>> {
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

    // Qualify with the enclosing class name. A method_definition's parent is the
    // `class_body`, not the class declaration, so the old `parent().name` lookup
    // always failed and every method collapsed to a bare, colliding name (e.g.
    // 49 different `getSizing`s). Walk up to the class declaration instead.
    let class_name = {
        let mut cur = node.parent();
        let mut found = None;
        while let Some(n) = cur {
            if matches!(
                n.kind(),
                "class_declaration" | "class" | "abstract_class_declaration"
            ) {
                found = n
                    .child_by_field_name("name")
                    .map(|x| node_text(x, source).to_string());
                break;
            }
            cur = n.parent();
        }
        found
    };
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
            if matches!(
                child.kind(),
                "formal_parameter" | "required_parameter" | "optional_parameter"
            ) {
                // Capture the TS type annotation (the `type` field holds a
                // `type_annotation` node like `: MyType`) so type-usage edges can
                // be emitted from it. Falls back to "Unknown" for untyped params.
                let ty = child
                    .child_by_field_name("type")
                    .map(|t| {
                        node_text(t, source)
                            .trim_start_matches(':')
                            .trim()
                            .to_string()
                    })
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "Unknown".to_string());
                let mut p_cursor = child.walk();
                for p_child in child.children(&mut p_cursor) {
                    if p_child.kind() == "identifier" {
                        let name = node_text(p_child, source).to_string();
                        params.push(Parameter {
                            name,
                            ty: ty.clone(),
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

    let mut sig_rows = Vec::new();
    for p in &sym.params {
        // Drop "Unknown" type noise; key already carries the param name.
        let ty = if p.ty == "Unknown" {
            String::new()
        } else {
            p.ty.clone()
        };
        sig_rows.push(vec![format!("param {}", p.name), ty]);
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

    // Implements / extends edges for class declarations.
    //
    // `class Foo implements IBar, IBaz` → class_heritage → implements_clause →
    //   type_identifier children → emit edge::implements[IBar], edge::implements[IBaz].
    //
    // `class Foo extends Base` → class_heritage → extends_clause →
    //   type_identifier child → emit edge::extends[Base].
    //
    // `class_heritage` has no field name in the TypeScript grammar; it is located
    // by scanning the class_declaration's named children for kind "class_heritage",
    // then scanning *its* children for the appropriate clause kind.
    if sym.kind == NodeType::Type {
        let mut implements_targets: Vec<String> = Vec::new();
        let mut extends_targets: Vec<String> = Vec::new();
        let mut heritage_cursor = sym.node.walk();
        for heritage_child in sym.node.children(&mut heritage_cursor) {
            if heritage_child.kind() == "class_heritage" {
                let mut hc = heritage_child.walk();
                for clause in heritage_child.children(&mut hc) {
                    if clause.kind() == "implements_clause" {
                        let mut ic = clause.walk();
                        for item in clause.children(&mut ic) {
                            if item.kind() == "type_identifier" {
                                implements_targets.push(node_text(item, source).to_string());
                            }
                        }
                    } else if clause.kind() == "extends_clause" {
                        // `extends Base` — in the TS grammar the superclass appears
                        // as an "identifier" (not "type_identifier") directly under
                        // extends_clause.
                        let mut ec = clause.walk();
                        for item in clause.children(&mut ec) {
                            if item.kind() == "identifier" || item.kind() == "type_identifier" {
                                extends_targets.push(node_text(item, source).to_string());
                            }
                        }
                    }
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

    // Resolve call sites. Methods are named `Class.method`; the part before the
    // last `.` is the enclosing class, used to resolve `this.x()` → `Class.x`.
    let enclosing_class = sym.name.rsplit_once('.').map(|(c, _)| c);
    let calls = resolve_ts_call_sites(sym.node, source, all_symbols, imports, enclosing_class);
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

    // Type-usage edges: types named in parameter annotations are `Uses`d, so a
    // type used only as an annotation (never "called") is not a false dead-code
    // candidate. Skip the "Unknown" placeholder used for untyped params. Only
    // names that resolve to a stored symbol actually become edges.
    {
        let mut type_uses: Vec<String> = Vec::new();
        for p in &sym.params {
            if p.ty == "Unknown" {
                continue;
            }
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

struct TsCallSite {
    callee: String,
    line: usize,
}

fn resolve_ts_call_sites<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    all_symbols: &[TsSymbol<'a>],
    imports: &[TsImport],
    enclosing_class: Option<&str>,
) -> Vec<TsCallSite> {
    let mut calls = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        calls.extend(resolve_ts_call_sites(
            child,
            source,
            all_symbols,
            imports,
            enclosing_class,
        ));
    }

    if node.kind() == "call_expression"
        && let Some(func) = node.child_by_field_name("function")
    {
        let callee = resolve_ts_callee(func, source, all_symbols, imports, enclosing_class);
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
    enclosing_class: Option<&str>,
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
                    // `this.x()` / `super.x()` are calls to the enclosing class's
                    // own method, which we can resolve exactly to `Class.x`.
                    if (o == "this" || o == "super")
                        && let Some(cls) = enclosing_class
                    {
                        return format!("{}.{}", cls, p);
                    }
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
            enclosing_class,
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
