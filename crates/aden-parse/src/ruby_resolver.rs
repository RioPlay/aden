// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Deep Ruby resolver — symbol + call-site analysis.
//!
//! Handles:
//!   • Class declarations, module declarations
//!   • Method definitions (instance, singleton, class methods)
//!   • Block/proc/lambda references
//!   • Intra-file and cross-module call resolution (best-effort)
//!   • Emits `edge::calls[]` macros for graph ingestion
//!
//! Phase 2 second pass (not yet implemented):
//!   • Gem path resolution
//!   • Rails DSL awareness (`has_many`, `before_action`, etc.)
//!   • Dynamic method dispatch (`send`, `method_missing`)

use crate::extractor::{LanguageExtractor, build_code_attributes, make_anchor};
use aden_core::{Block, Document, NodeType, Parameter, Result};
use std::path::Path;

/// Deep Ruby extractor.
pub struct RubyResolver;

impl Default for RubyResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl RubyResolver {
    pub fn new() -> Self {
        Self
    }
}

impl LanguageExtractor for RubyResolver {
    fn language_id(&self) -> &'static str {
        "ruby"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["rb"]
    }

    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>> {
        let language = tree_sitter_language_pack::get_language("ruby")
            .map_err(|e| aden_core::Error::Parse(format!("language-pack: {}", e)))?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language)
            .map_err(|e| aden_core::Error::Parse(e.to_string()))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| aden_core::Error::Parse("tree-sitter returned None".to_string()))?;

        let module_name = infer_ruby_module(path);
        let file_name = path.file_name().unwrap_or_default().to_string_lossy();

        let mut symbols: Vec<RubySymbol> = Vec::new();
        walk_program(
            tree.root_node(),
            source,
            &module_name,
            &file_name,
            &mut symbols,
        );

        let mut docs = Vec::new();
        for sym in &symbols {
            if let Some(doc) = emit_ruby_symbol(
                sym,
                source,
                path,
                &symbols,
                &module_name,
                &file_name,
            ) {
                docs.push(doc);
            }
        }

        Ok(docs)
    }
}

#[derive(Debug)]
struct RubySymbol<'a> {
    qualified_name: String,
    kind: NodeType,
    node: tree_sitter::Node<'a>,
    params: Vec<Parameter>,
    doc_comment: Option<String>,
    is_singleton: bool,           // true for singleton methods (def self.foo)
    parent_class: Option<String>, // for class declarations
}

fn infer_ruby_module(path: &Path) -> String {
    let path_str = path.to_string_lossy();
    // Look for lib/ or app/ directories
    if let Some(idx) = path_str.rfind("/lib/") {
        let after = &path_str[idx + 5..];
        return after.trim_end_matches(".rb").replace('/', ".");
    }
    if let Some(idx) = path_str.rfind("/app/") {
        let after = &path_str[idx + 5..];
        return after.trim_end_matches(".rb").replace('/', ".");
    }
    path.file_stem()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
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
        if trimmed.starts_with("#") {
            if !trimmed.starts_with("#!") {
                let content = trimmed.trim_start_matches('#').trim_start();
                if !content.is_empty() {
                    comments.push(content.to_string());
                }
            }
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

fn walk_program<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    module: &str,
    file_name: &str,
    symbols: &mut Vec<RubySymbol<'a>>,
) {
    if !node.is_named() {
        return;
    }

    match node.kind() {
        "class" | "singleton_class" => {
            parse_class(node, source, module, file_name, symbols);
        }
        "module" => {
            parse_module(node, source, module, file_name, symbols);
        }
        "method" | "singleton_method" | "accessor" | "setter" => {
            parse_method(node, source, module, file_name, symbols);
        }
        "program"
        | "body_statement"
        | "do_block"
        | "block"
        | "class_body_statement"
        | "module_body_statement"
        | "then"
        | "else"
        | "elsif" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_program(child, source, module, file_name, symbols);
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named() {
                    walk_program(child, source, module, file_name, symbols);
                }
            }
        }
    }
}

fn parse_class<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    module: &str,
    file_name: &str,
    symbols: &mut Vec<RubySymbol<'a>>,
) {
    let name = if let Some(name_node) = node.child_by_field_name("name") {
        node_text(name_node, source).to_string()
    } else {
        // Singleton class: class << self
        "singleton_class".to_string()
    };

    let parent_class = node
        .child_by_field_name("superclass")
        .map(|n| node_text(n, source).to_string());

    let qname = format!("{}.{}", module, name);
    symbols.push(RubySymbol {
        qualified_name: qname,
        kind: NodeType::Type,
        node,
        params: Vec::new(),
        doc_comment: extract_doc_comment(node, source),
        is_singleton: false,
        parent_class: parent_class.clone(),
    });

    // Walk body for nested methods
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "body_statement" || child.kind() == "class_body_statement" {
            let mut cc = child.walk();
            for grandchild in child.children(&mut cc) {
                if grandchild.is_named() {
                    walk_program(
                        grandchild,
                        source,
                        &format!("{}.{}", module, name),
                        file_name,
                        symbols,
                    );
                }
            }
        }
    }
}

fn parse_module<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    module: &str,
    file_name: &str,
    symbols: &mut Vec<RubySymbol<'a>>,
) {
    let name = if let Some(name_node) = node.child_by_field_name("name") {
        node_text(name_node, source).to_string()
    } else {
        return;
    };

    let qname = format!("{}.{}", module, name);
    symbols.push(RubySymbol {
        qualified_name: qname,
        kind: NodeType::Module,
        node,
        params: Vec::new(),
        doc_comment: extract_doc_comment(node, source),
        is_singleton: false,
        parent_class: None,
    });

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "body_statement" || child.kind() == "module_body_statement" {
            let mut cc = child.walk();
            for grandchild in child.children(&mut cc) {
                if grandchild.is_named() {
                    walk_program(
                        grandchild,
                        source,
                        &format!("{}.{}", module, name),
                        file_name,
                        symbols,
                    );
                }
            }
        }
    }
}

fn parse_method<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    module: &str,
    _file_name: &str,
    symbols: &mut Vec<RubySymbol<'a>>,
) {
    let (name, is_singleton) = if let Some(name_node) = node.child_by_field_name("name") {
        let n = node_text(name_node, source).to_string();
        (
            n.clone(),
            node.kind() == "singleton_method" || n.starts_with("self."),
        )
    } else if node.kind() == "accessor" {
        // attr_reader, attr_writer, attr_accessor
        let text = node_text(node, source);
        let n = if text.contains("attr_reader") {
            "attr_reader"
        } else if text.contains("attr_writer") {
            "attr_writer"
        } else {
            "attr_accessor"
        };
        (n.to_string(), false)
    } else {
        return;
    };

    let mut params = Vec::new();
    if let Some(params_node) = node.child_by_field_name("parameters") {
        let mut pc = params_node.walk();
        for param in params_node.children(&mut pc) {
            if matches!(
                param.kind(),
                "identifier"
                    | "parameter"
                    | "block_parameter"
                    | "optional_parameter"
                    | "keyword_parameter"
            ) {
                let param_name = node_text(param, source).to_string();
                let mut default = None;
                if param.kind() == "optional_parameter" {
                    default = param
                        .child_by_field_name("value")
                        .map(|v| node_text(v, source).to_string());
                }
                params.push(Parameter {
                    name: param_name
                        .trim_start_matches('&')
                        .trim_start_matches('*')
                        .to_string(),
                    ty: String::new(),
                    default_value: default,
                });
            }
        }
    }

    let display_name = if is_singleton && !name.starts_with("self.") {
        format!("self.{}", name)
    } else {
        name.clone()
    };

    // `module` already encodes the enclosing class/module chain (walk_program
    // descends into class bodies with it), so qualify on that alone. Appending
    // the parent type again produced a doubled `A.A/run`; the `module.` strip in
    // emit then yields a clean `A.run` fragment, distinct per class.
    let _ = find_parent_type_name; // retained for other call sites
    let qname = format!("{}.{}", module, display_name);

    symbols.push(RubySymbol {
        qualified_name: qname,
        kind: NodeType::Function,
        node,
        params,
        doc_comment: extract_doc_comment(node, source),
        is_singleton,
        parent_class: None,
    });
}

fn find_parent_type_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if matches!(parent.kind(), "class" | "module" | "singleton_class")
            && let Some(name_node) = parent.child_by_field_name("name")
        {
            return Some(node_text(name_node, source).to_string());
        }
        current = parent;
    }
    None
}

fn emit_ruby_symbol(
    sym: &RubySymbol,
    source: &str,
    path: &Path,
    _all_symbols: &[RubySymbol],
    module: &str,
    file_name: &str,
) -> Option<Document> {
    // Qualify the anchor with the enclosing class/module so same-named methods in
    // different classes of one file don't collapse to one anchor (data loss).
    // `qualified_name` is `<module>.<Class>.<method>`; strip the leading
    // project-module prefix that `make_anchor` already supplies so the fragment
    // stays `Class.method` and top-level/class anchors are unchanged.
    let fragment = sym
        .qualified_name
        .strip_prefix(&format!("{module}."))
        .unwrap_or(&sym.qualified_name);
    let anchor = make_anchor(module, file_name, fragment);
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

    let mut rows: Vec<Vec<String>> = vec![vec!["Kind".to_string(), format!("{:?}", sym.kind)]];
    if sym.is_singleton {
        rows.push(vec!["Singleton".to_string(), "true".to_string()]);
    }
    if let Some(ref parent) = sym.parent_class {
        rows.push(vec!["Parent".to_string(), parent.clone()]);
    }
    for p in &sym.params {
        let desc = p
            .default_value
            .as_ref()
            .map(|d| format!("= {}", d))
            .unwrap_or_default();
        rows.push(vec![format!("param {}", p.name), desc]);
    }
    rows.push(vec!["Qualified".to_string(), sym.qualified_name.clone()]);

    blocks.push(Block::Paragraph("== Signature".to_string()));
    blocks.push(Block::Table(aden_core::Table {
        headers: vec!["Property".to_string(), "Value".to_string()],
        rows,
    }));

    // Extract call sites from method body
    if sym.kind == NodeType::Function
        && let Some(body) = sym.node.child_by_field_name("body")
    {
        let calls = extract_ruby_call_sites(body, source);
        let filtered: Vec<_> = calls
            .into_iter()
            .filter(|(c, _)| !is_ruby_std_noise(c))
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

const RUBY_SKIP_CALLEES: &[&str] = &[
    "puts",
    "print",
    "p",
    "pp",
    "to_s",
    "to_i",
    "to_f",
    "to_sym",
    "to_a",
    "to_h",
    "to_json",
    "map",
    "select",
    "filter",
    "reject",
    "find",
    "detect",
    "each",
    "each_with_index",
    "reduce",
    "inject",
    "fold",
    "collect",
    "flat_map",
    "compact",
    "uniq",
    "length",
    "size",
    "count",
    "empty?",
    "blank?",
    "present?",
    "gsub",
    "sub",
    "split",
    "join",
    "strip",
    "chomp",
    "downcase",
    "upcase",
    "new",
    "initialize",
    "allocate",
    "include?",
    "include",
    "extend",
    "prepend",
    "freeze",
    "dup",
    "clone",
    "taint",
    "untaint",
    "require",
    "require_relative",
    "load",
    "autoload",
    "attr_reader",
    "attr_writer",
    "attr_accessor",
    "attr",
    "raise",
    "fail",
    "throw",
    "catch",
    "loop",
    "times",
    "upto",
    "downto",
    "step",
    "open",
    "read",
    "write",
    "close",
    "flush",
    "rewind",
    "send",
    "__send__",
    "method_missing",
    "respond_to?",
    "is_a?",
    "kind_of?",
    "instance_of?",
    "nil?",
    "nil",
    "true",
    "false",
    "tap",
    "then",
    "yield_self",
];

fn is_ruby_std_noise(name: &str) -> bool {
    RUBY_SKIP_CALLEES.contains(&name)
        || name.starts_with("to_")
        || name.ends_with("?") && name.len() <= 8
}

fn extract_ruby_call_sites(node: tree_sitter::Node, source: &str) -> Vec<(String, usize)> {
    let mut calls = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        calls.extend(extract_ruby_call_sites(child, source));
    }

    match node.kind() {
        "call" | "method_call" | "command_call" | "command" | "call_without_parentheses" => {
            if let Some(method) = node.child_by_field_name("method") {
                let callee = resolve_ruby_callee_name(method, source);
                if !callee.is_empty() && callee.len() >= 2 {
                    let line = method.start_position().row + 1;
                    calls.push((callee, line));
                }
            } else {
                // Try first identifier child
                let mut c = node.walk();
                for child in node.children(&mut c) {
                    if child.kind() == "identifier" || child.kind() == "method_identifier" {
                        let text = node_text(child, source);
                        if text.len() >= 2 {
                            calls.push((text.to_string(), child.start_position().row + 1));
                        }
                        break;
                    }
                }
            }
        }
        "block" | "do_block" => {
            // Blocks contain yield calls and method calls
            if let Some(body) = node.child_by_field_name("body") {
                calls.extend(extract_ruby_call_sites(body, source));
            }
        }
        _ => {}
    }
    calls
}

fn resolve_ruby_callee_name(node: tree_sitter::Node, source: &str) -> String {
    match node.kind() {
        "identifier" | "method_identifier" => node_text(node, source).to_string(),
        _ => {
            // Extract last identifier
            let mut last = String::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "identifier" || child.kind() == "method_identifier" {
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
}
