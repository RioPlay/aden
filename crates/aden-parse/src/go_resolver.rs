// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Deep Go resolver — import + call-site analysis.
//!
//! Go is the easiest deep resolver because:
//!   • Strict module system (`module path` in `go.mod`)
//!   • No relative imports (always fully qualified)
//!   • No cyclic imports
//!   • `tree-sitter-go` grammar is simple and stable

use crate::extractor::{LanguageExtractor, build_code_attributes, make_anchor};
use aden_core::{Block, Document, NodeType, Result};
use std::path::Path;

/// Deep Go extractor.
pub struct GoResolver;

impl Default for GoResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl GoResolver {
    pub fn new() -> Self {
        Self
    }
}

impl LanguageExtractor for GoResolver {
    fn language_id(&self) -> &'static str {
        "go"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["go"]
    }

    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>> {
        let language = tree_sitter_language_pack::get_language("go")
            .map_err(|e| aden_core::Error::Parse(format!("language-pack: {}", e)))?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language)
            .map_err(|e| aden_core::Error::Parse(e.to_string()))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| aden_core::Error::Parse("tree-sitter returned None".to_string()))?;

        let module_path = find_go_module(path);
        let file_name = path.file_name().unwrap_or_default().to_string_lossy();

        // Collect symbols and imports from the file.
        let mut symbols: Vec<GoSymbol> = Vec::new();
        let mut imports: Vec<GoImport> = Vec::new();
        walk_package_decl(
            tree.root_node(),
            source,
            &module_path,
            &file_name,
            &mut symbols,
            &mut imports,
        );

        // Emit Documents with call-site resolution.
        let mut docs = Vec::new();
        for sym in &symbols {
            if let Some(doc) = emit_go_symbol(
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

fn find_go_module(path: &Path) -> String {
    // Walk up from the source file looking for go.mod.
    for ancestor in path.ancestors() {
        let go_mod = ancestor.join("go.mod");
        if let Ok(content) = std::fs::read_to_string(go_mod) {
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("module ") {
                    return rest.trim().to_string();
                }
            }
        }
    }
    "unknown".to_string()
}

#[derive(Debug)]
struct GoImport {
    alias: Option<String>, // local alias (e.g. "fmt" or "http")
    path: String,          // full import path (e.g. "fmt" or "net/http")
}

#[derive(Debug)]
#[allow(dead_code)]
struct GoSymbol<'a> {
    name: String,
    kind: NodeType,
    node: tree_sitter::Node<'a>,
    receiver: Option<String>, // e.g. "Point" for `func (p Point) Distance()`
    doc_comment: Option<String>,
}

/// Walk the AST and collect all top-level declarations.
fn walk_package_decl<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    _module: &str,
    _file_name: &str,
    symbols: &mut Vec<GoSymbol<'a>>,
    imports: &mut Vec<GoImport>,
) {
    if !node.is_named() {
        return;
    }

    match node.kind() {
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let doc = extract_go_doc_comment(node, source);
                symbols.push(GoSymbol {
                    name,
                    kind: NodeType::Function,
                    node,
                    receiver: None,
                    doc_comment: doc,
                });
            }
        }
        "method_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let receiver = extract_receiver_type(node, source);
                let qualified = receiver
                    .as_ref()
                    .map(|r| format!("{}.{}", r, name))
                    .unwrap_or_else(|| name.clone());
                let doc = extract_go_doc_comment(node, source);
                symbols.push(GoSymbol {
                    name: qualified,
                    kind: NodeType::Function,
                    node,
                    receiver,
                    doc_comment: doc,
                });
            }
        }
        "type_declaration" => {
            // type Point struct { ... }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "type_spec"
                    && let Some(name_node) = child.child_by_field_name("name")
                {
                    let name = node_text(name_node, source).to_string();
                    let doc = extract_go_doc_comment(node, source);
                    symbols.push(GoSymbol {
                        name,
                        kind: NodeType::Type,
                        node,
                        receiver: None,
                        doc_comment: doc,
                    });
                }
            }
        }
        "import_declaration" | "import_spec" => {
            extract_import(node, source, imports);
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_package_decl(child, source, _module, _file_name, symbols, imports);
    }
}

fn extract_receiver_type(node: tree_sitter::Node, source: &str) -> Option<String> {
    if let Some(recv_list) = node.child_by_field_name("receiver") {
        let mut cursor = recv_list.walk();
        for child in recv_list.children(&mut cursor) {
            if child.kind() == "parameter_list" || child.kind() == "parameter_declaration" {
                let mut inner = child.walk();
                for inner_child in child.children(&mut inner) {
                    if inner_child.kind() == "pointer_type" {
                        // *Point
                        if let Some(elem) = inner_child.child_by_field_name("type") {
                            return Some(node_text(elem, source).to_string());
                        }
                    } else if inner_child.kind() == "type_identifier" {
                        return Some(node_text(inner_child, source).to_string());
                    }
                }
            }
        }
    }
    None
}

fn extract_import(node: tree_sitter::Node, source: &str, imports: &mut Vec<GoImport>) {
    match node.kind() {
        "import_declaration" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_import(child, source, imports);
            }
        }
        "import_spec" => {
            let alias = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string());
            let path = node
                .child_by_field_name("path")
                .map(|n| {
                    let raw = node_text(n, source);
                    raw.trim_matches('"').to_string()
                })
                .unwrap_or_default();
            if !path.is_empty() {
                imports.push(GoImport { alias, path });
            }
        }
        "import_spec_list" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_import(child, source, imports);
            }
        }
        _ => {}
    }
}

fn extract_go_doc_comment(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut current = node.prev_named_sibling();
    while let Some(sib) = current {
        if sib.kind() == "comment" {
            let text = node_text(sib, source).trim();
            if text.starts_with("//") {
                return Some(text.to_string());
            }
        } else {
            break;
        }
        current = sib.prev_named_sibling();
    }
    None
}

fn emit_go_symbol<'a>(
    sym: &GoSymbol<'a>,
    source: &str,
    path: &Path,
    all_symbols: &[GoSymbol<'a>],
    imports: &[GoImport],
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
        "Go {} from module `{}`.",
        format!("{:?}", sym.kind).to_lowercase(),
        module
    )));

    // Resolve call sites inside the function body
    if let Some(body) = sym.node.child_by_field_name("body") {
        let calls = resolve_go_call_sites(body, source, all_symbols, imports);
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
    })
}

struct GoCallSite {
    callee: String,
    line: usize,
}

fn resolve_go_call_sites<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    all_symbols: &[GoSymbol<'a>],
    imports: &[GoImport],
) -> Vec<GoCallSite> {
    let mut calls = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        calls.extend(resolve_go_call_sites(child, source, all_symbols, imports));
    }

    if node.kind() == "call_expression"
        && let Some(func) = node.child_by_field_name("function")
    {
        let callee = resolve_go_callee(func, source, all_symbols, imports);
        if !callee.is_empty() && callee.len() >= 2 && !is_go_std_noise(&callee) {
            let line = func.start_position().row + 1;
            calls.push(GoCallSite { callee, line });
        }
    }
    calls
}

fn resolve_go_callee(
    node: tree_sitter::Node,
    source: &str,
    all_symbols: &[GoSymbol],
    imports: &[GoImport],
) -> String {
    match node.kind() {
        "identifier" => {
            let name = node_text(node, source).trim().to_string();
            // Is this a local function?
            if all_symbols.iter().any(|s| s.name == name) {
                return name;
            }
            // Is this a builtin or standard library function?
            name
        }
        "selector_expression" => {
            // e.g. `fmt.Println`, `http.Get`, `p.Distance`
            let obj = node
                .child_by_field_name("operand")
                .map(|n| node_text(n, source).trim().to_string());
            let sel = node
                .child_by_field_name("field")
                .map(|n| node_text(n, source).trim().to_string());
            match (obj, sel) {
                (Some(o), Some(s)) => {
                    // Try to resolve the package prefix
                    for imp in imports {
                        if imp.alias.as_ref().unwrap_or(&imp.path) == &o {
                            return format!("{}.{}", imp.path, s);
                        }
                    }
                    // Could be a method call on a local type
                    format!("{}.{}", o, s)
                }
                (Some(o), None) => o,
                (None, Some(s)) => s,
                _ => node_text(node, source).trim().to_string(),
            }
        }
        "call_expression" => resolve_go_callee(
            node.child_by_field_name("function").unwrap_or(node),
            source,
            all_symbols,
            imports,
        ),
        _ => node_text(node, source).trim().to_string(),
    }
}

fn is_go_std_noise(name: &str) -> bool {
    const SKIP: &[&str] = &[
        "len",
        "cap",
        "make",
        "new",
        "append",
        "copy",
        "delete",
        "close",
        "panic",
        "recover",
        "print",
        "println",
        "string",
        "int",
        "float64",
        "bool",
        "error",
        "range",
        "map",
        "chan",
        "select",
        "defer",
        "fmt.Sprintf",
        "fmt.Printf",
        "fmt.Println",
        "fmt.Print",
        "fmt.Fprintf",
        "fmt.Sprintf",
        "fmt.Errorf",
        "strings.Join",
        "strings.Split",
        "strings.Trim",
        "strconv.Itoa",
        "strconv.Atoi",
        "time.Now",
        "time.Sleep",
        "os.Open",
        "os.Create",
        "os.Exit",
        "io.Copy",
        "io.ReadAll",
        "io.WriteString",
        "bytes.Buffer",
        "bytes.NewBuffer",
        "bytes.NewBufferString",
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
