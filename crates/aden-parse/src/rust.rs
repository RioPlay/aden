// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::extractor::{
    LanguageExtractor, build_code_attributes, make_anchor, project_relative_file,
};
use aden_core::{Block, Document, FieldDef, NodeType, Parameter, Result, Visibility};
use std::path::Path;

/// Deep Rust extractor — implements `LanguageExtractor` for fully-resolved
/// call-site analysis, visibility, doc comments, and edge macros.
pub struct RustExtractor;

impl Default for RustExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl RustExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl LanguageExtractor for RustExtractor {
    fn language_id(&self) -> &'static str {
        "rust"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>> {
        extract_documents_inner(path, source)
    }
}

/// Extract Documents from a Rust source file using tree-sitter.
pub fn extract_documents_inner(path: &Path, source: &str) -> Result<Vec<Document>> {
    let language = tree_sitter_language_pack::get_language("rust")
        .map_err(|e| aden_core::Error::Parse(format!("language-pack: {}", e)))?;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language)
        .map_err(|e| aden_core::Error::Parse(e.to_string()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| aden_core::Error::Parse("tree-sitter returned None".to_string()))?;
    let root = tree.root_node();
    let (crate_name, project_root) = infer_crate_name_and_root(path);
    // Compute the project-root-relative file component (Gap 11: eliminates
    // basename collisions) and apply module-entry mapping (Gap 7: mod.rs /
    // lib.rs represent their parent module directory).
    let raw_rel = project_relative_file(path, &project_root);
    let file_name_owned = apply_module_entry_mapping_rust(&raw_rel);
    let file_name = file_name_owned.as_str();
    let mut docs = Vec::new();

    let mut cursor = root.walk();
    let children: Vec<_> = root.children(&mut cursor).collect();
    let mut buffered_comments: Vec<String> = Vec::new();

    for child in children {
        if !child.is_named() {
            continue;
        }
        match child.kind() {
            "line_comment" => {
                if let Some(comment) = process_line_comment(child, source) {
                    buffered_comments.push(comment);
                }
            }
            "block_comment" => {
                if let Some(comment) = process_block_comment(child, source) {
                    buffered_comments.push(comment);
                }
            }
            "function_item" | "function_signature_item" => {
                if let Some(doc) = extract_function(
                    child,
                    source,
                    path,
                    &crate_name,
                    file_name,
                    &buffered_comments,
                    None,
                ) {
                    docs.push(doc);
                }
                buffered_comments.clear();
            }
            "impl_item" => {
                extract_impl_methods(child, source, path, &crate_name, file_name, &mut docs);
                buffered_comments.clear();
            }
            "const_item" | "static_item" => {
                if let Some(doc) = extract_const_or_static(
                    child,
                    source,
                    path,
                    &crate_name,
                    file_name,
                    &buffered_comments,
                ) {
                    docs.push(doc);
                }
                buffered_comments.clear();
            }
            "type_item" => {
                if let Some(doc) = extract_type_alias(
                    child,
                    source,
                    path,
                    &crate_name,
                    file_name,
                    &buffered_comments,
                ) {
                    docs.push(doc);
                }
                buffered_comments.clear();
            }
            "struct_item" => {
                if let Some(doc) = extract_struct(
                    child,
                    source,
                    path,
                    &crate_name,
                    file_name,
                    &buffered_comments,
                ) {
                    docs.push(doc);
                }
                buffered_comments.clear();
            }
            "enum_item" => {
                if let Some(doc) = extract_enum(
                    child,
                    source,
                    path,
                    &crate_name,
                    file_name,
                    &buffered_comments,
                ) {
                    docs.push(doc);
                }
                buffered_comments.clear();
            }
            "mod_item" => {
                if let Some(doc) = extract_module(
                    child,
                    source,
                    path,
                    &crate_name,
                    file_name,
                    &buffered_comments,
                ) {
                    docs.push(doc);
                }
                buffered_comments.clear();
            }
            "trait_item" => {
                if let Some(doc) = extract_trait(
                    child,
                    source,
                    path,
                    &crate_name,
                    file_name,
                    &buffered_comments,
                ) {
                    docs.push(doc);
                }
                buffered_comments.clear();
            }
            _ => {}
        }
    }
    Ok(docs)
}

/// Read the `name` field from the `[package]` section of a `Cargo.toml`.
///
/// Simple line-scanning: find `[package]`, then take the first `name = "…"`
/// that appears before the next `[` section header.  No TOML parser dependency.
fn package_name_from_cargo_toml(manifest: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let mut in_package = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package {
            // Match `name = "value"` or `name = 'value'`
            if let Some(rest) = trimmed.strip_prefix("name") {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('=') {
                    let rest = rest.trim();
                    for q in ['"', '\''] {
                        if let Some(inner) = rest.strip_prefix(q).and_then(|s| s.split(q).next()) {
                            return Some(inner.to_owned());
                        }
                    }
                }
            }
        }
    }
    None
}

/// Infer the Cargo package name for the file at `path`.
///
/// Strategy (nearest-ancestor-first):
/// 1. Walk ancestors to find the nearest `Cargo.toml`.
/// 2. Line-scan that manifest for the `name` field under `[package]`.
/// 3. Fall back to the parent directory name when parsing yields nothing.
#[cfg_attr(not(test), allow(dead_code))]
fn infer_crate_name(path: &Path) -> String {
    infer_crate_name_and_root(path).0
}

/// Returns `(crate_name, project_root)` for `path`.
///
/// The project root is the directory that contains the crate's `Cargo.toml`
/// (or the file's parent when no manifest exists).  It is used by
/// `project_relative_file` to build the file component of module anchors.
fn infer_crate_name_and_root(path: &Path) -> (String, std::path::PathBuf) {
    // Walk from the file's parent upwards.
    let start = path.parent().unwrap_or(path);
    for ancestor in start.ancestors() {
        let manifest = ancestor.join("Cargo.toml");
        if manifest.exists() {
            if let Some(name) = package_name_from_cargo_toml(&manifest) {
                let root =
                    std::fs::canonicalize(ancestor).unwrap_or_else(|_| ancestor.to_path_buf());
                return (name, root);
            }
            // Workspace root without [package]: stop here and fall through.
            break;
        }
    }
    // Directory-name fallback.
    let parent = path.parent().unwrap_or(path);
    let name = parent
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let root = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    (name, root)
}

/// Gap 7 — module-entry mapping for Rust.
///
/// When the project-root-relative file path ends with `mod.rs` or `lib.rs`,
/// the file IS its parent module directory, so the file component of the anchor
/// should be that parent directory path rather than the file name.
///
/// Examples (with forward-slash paths):
/// - `src/commands/mod.rs` → `src/commands`
/// - `src/lib.rs`          → `src`
/// - `src/commands/heal.rs` → unchanged (`src/commands/heal.rs`)
///
/// A bare `mod.rs` or `lib.rs` at the project root (i.e., the relative path
/// has no `/`) maps to an empty parent → returns the bare filename unchanged
/// (no parent to collapse to).
fn apply_module_entry_mapping_rust(rel: &str) -> String {
    let last = rel.rsplit('/').next().unwrap_or(rel);
    if matches!(last, "mod.rs" | "lib.rs")
        && let Some(parent) = rel.rsplit_once('/').map(|(p, _)| p)
        && !parent.is_empty()
    {
        return parent.to_string();
    }
    rel.to_string()
}

fn node_text<'a>(node: tree_sitter::Node<'a>, source: &'a str) -> &'a str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

use crate::tree_sitter_common::node_to_span;

fn get_visibility_with_source(node: tree_sitter::Node, source: &str) -> Visibility {
    if let Some(vis) = node.child_by_field_name("visibility_modifier") {
        let text = node_text(vis, source);
        if text.starts_with("pub(") {
            if text.contains("crate") {
                Visibility::Internal
            } else if text.contains("super") {
                Visibility::Restricted
            } else {
                Visibility::Public
            }
        } else {
            Visibility::Public
        }
    } else {
        Visibility::Private
    }
}

fn process_line_comment(node: tree_sitter::Node, source: &str) -> Option<String> {
    let text = node_text(node, source);
    if text.starts_with("///") && !text.starts_with("////") {
        Some(text.trim_start_matches("///").trim_start().to_string())
    } else {
        None
    }
}

fn process_block_comment(node: tree_sitter::Node, source: &str) -> Option<String> {
    let text = node_text(node, source);
    if let Some(inner) = text.strip_prefix("/**").and_then(|s| s.strip_suffix("*/")) {
        // `/**/` (len 4) yields inner == "" here — the old `&text[3..len-2]`
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
        Some(lines.join("\n"))
    } else {
        None
    }
}

fn extract_function(
    node: tree_sitter::Node,
    source: &str,
    path: &Path,
    crate_name: &str,
    file_name: &str,
    buffered_comments: &[String],
    // When this function is an associated fn/method inside an `impl T`, the
    // type name qualifies the anchor (`T::method`) so two same-named methods in
    // different impls don't collide on one anchor (silent overwrite / data loss).
    type_prefix: Option<&str>,
) -> Option<Document> {
    let name_node = node.child_by_field_name("name")?;
    let bare_name = node_text(name_node, source);
    let qualified;
    let name: &str = match type_prefix {
        Some(t) => {
            qualified = format!("{}::{}", t, bare_name);
            &qualified
        }
        None => bare_name,
    };
    let vis = get_visibility_with_source(node, source);
    let mut is_async = false;
    let mut is_unsafe = false;
    if let Some(modifiers) = node.child_by_field_name("function_modifiers") {
        let mod_text = node_text(modifiers, source);
        if mod_text.contains("async") {
            is_async = true;
        }
        if mod_text.contains("unsafe") {
            is_unsafe = true;
        }
    }
    let mut params = Vec::new();
    if let Some(params_node) = node.child_by_field_name("parameters") {
        let mut pc = params_node.walk();
        for param in params_node.children(&mut pc) {
            if param.kind() == "parameter" {
                let pat = param.child_by_field_name("pattern")?;
                let ty = param.child_by_field_name("type")?;
                params.push(Parameter {
                    name: node_text(pat, source).to_string(),
                    ty: node_text(ty, source).to_string(),
                    default_value: None,
                });
            } else if param.kind() == "self_parameter" {
                params.push(Parameter {
                    name: "self".to_string(),
                    ty: node_text(param, source).to_string(),
                    default_value: None,
                });
            }
        }
    }
    let return_type = node
        .child_by_field_name("return_type")
        .map(|n| node_text(n, source).to_string());
    let doc_comment = if buffered_comments.is_empty() {
        None
    } else {
        Some(buffered_comments.join("\n"))
    };
    let anchor = make_anchor(crate_name, file_name, name);
    let span = node_to_span(node, path);
    let attrs = build_code_attributes(source, "function", Some(path), Some(&span));
    let mut blocks = Vec::new();
    if let Some(doc) = doc_comment {
        blocks.push(Block::Paragraph(doc));
    }
    let mut sig_rows = vec![vec!["Visibility".to_string(), format!("{:?}", vis)]];
    if is_async {
        sig_rows.push(vec!["Async".to_string(), "true".to_string()]);
    }
    if is_unsafe {
        sig_rows.push(vec!["Unsafe".to_string(), "true".to_string()]);
    }
    for p in &params {
        sig_rows.push(vec![format!("param {}", p.name), p.ty.clone()]);
    }
    if let Some(ref rt) = return_type {
        sig_rows.push(vec!["Returns".to_string(), rt.clone()]);
    }
    blocks.push(Block::Paragraph("== Signature".to_string()));
    blocks.push(Block::Table(aden_core::Table {
        headers: vec!["Property".to_string(), "Value".to_string()],
        rows: sig_rows,
    }));
    // Extract call sites from function body
    if let Some(body) = node.child_by_field_name("body") {
        let calls = extract_call_sites(body, source);
        let filtered: Vec<_> = calls
            .into_iter()
            .filter(|(c, _)| !is_std_noise(c))
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
            // Emit typed edge macros as a listing block for graph ingestion
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

    // Type-usage edges: types referenced in the signature are `Uses`d, so a type
    // that is used (but never "called") is not a false dead-code candidate. Only
    // names that resolve to a stored symbol become edges (see link_store_edges).
    {
        let mut type_uses: Vec<String> = Vec::new();
        for p in &params {
            for t in extract_type_idents(&p.ty) {
                if !type_uses.contains(&t) {
                    type_uses.push(t);
                }
            }
        }
        if let Some(ref rt) = return_type {
            for t in extract_type_idents(rt) {
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

    if !buffered_comments.is_empty() {
        blocks.push(Block::Admonition {
            kind: aden_core::AdmonitionKind::Note,
            text: "Extracted from source code via tree-sitter. Confidence is heuristic."
                .to_string(),
        });
    }
    Some(Document {
        anchor,
        node_type: NodeType::Function,
        attributes: attrs,
        blocks,
        source_span: None,
        metadata: None,
        confidence: 0.9,
    })
}

/// Strip generic arguments from a type/trait reference so the base name can
/// match a stored symbol anchor: `From<u8>` → `From`,
/// `AdenGraph<DocumentNode, AdenEdge>` → `AdenGraph`. Scoped paths
/// (`fmt::Display`) are kept — the linker's implements resolution falls back
/// segment-by-segment, and an external path simply fails to resolve (no edge,
/// no false edge).
fn strip_generic_args(text: &str) -> &str {
    text.split('<').next().unwrap_or(text).trim()
}

/// True when the function's receiver is `&mut self` (incl. `&'a mut self`) —
/// the cheap, honest "this method mutates its parent type's state" signal the
/// graph-type roadmap sanctions for `Mutates` emission. By-value `mut self`
/// (consuming) and `&self` receivers do not qualify.
fn has_mut_self_receiver(node: tree_sitter::Node, source: &str) -> bool {
    let Some(params) = node.child_by_field_name("parameters") else {
        return false;
    };
    let mut pc = params.walk();
    for p in params.children(&mut pc) {
        if p.kind() == "self_parameter" {
            let t = node_text(p, source);
            return t.starts_with('&') && t.contains("mut");
        }
    }
    false
}

/// Walk an `impl T { ... }` block and emit a Document for each associated fn /
/// method. The top-level loop does not recurse, so without this these symbols
/// were invisible. Anchors are qualified with the impl type (`T::method`) so
/// same-named methods across different impls don't collide.
///
/// Wave 1 (graph-type roadmap): each method additionally carries typed edge
/// macros for graph linking —
/// * `edge::implements[Trait::method]` when the impl is `impl Trait for T`
///   (method-level preferred; the linker falls back to the trait itself when
///   the trait method is not a stored symbol), and
/// * `edge::mutates[T]` when the receiver is `&mut self`.
fn extract_impl_methods(
    node: tree_sitter::Node,
    source: &str,
    path: &Path,
    crate_name: &str,
    file_name: &str,
    docs: &mut Vec<Document>,
) {
    // The impl's target type: the `type` field if present, else the first
    // `type_identifier` child (e.g. `impl Foo`).
    let type_name = node
        .child_by_field_name("type")
        .or_else(|| {
            let mut cursor = node.walk();
            node.children(&mut cursor)
                .find(|c| c.kind() == "type_identifier")
        })
        .map(|n| node_text(n, source).to_string());
    let type_prefix = type_name.as_deref();
    // The trait being implemented (`impl Trait for T` → `Trait`); None for an
    // inherent impl. Generic args are stripped so `From<u8>` links as `From`.
    let trait_name: Option<String> = node
        .child_by_field_name("trait")
        .map(|n| strip_generic_args(node_text(n, source)).to_string())
        .filter(|t| !t.is_empty());
    // Mutates targets the parent type's stored anchor, which uses the bare
    // name (`Foo`, not `Foo<T>` or `path::Foo`).
    let mutates_target: Option<String> = type_name
        .as_deref()
        .map(strip_generic_args)
        .and_then(|t| t.rsplit("::").next())
        .map(str::to_string)
        .filter(|t| !t.is_empty());

    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut bc = body.walk();
    let mut buffered: Vec<String> = Vec::new();
    for child in body.children(&mut bc) {
        match child.kind() {
            "line_comment" => {
                if let Some(c) = process_line_comment(child, source) {
                    buffered.push(c);
                }
            }
            "block_comment" => {
                if let Some(c) = process_block_comment(child, source) {
                    buffered.push(c);
                }
            }
            "function_item" | "function_signature_item" => {
                if let Some(mut doc) = extract_function(
                    child,
                    source,
                    path,
                    crate_name,
                    file_name,
                    &buffered,
                    type_prefix,
                ) {
                    let mut edge_lines: Vec<String> = Vec::new();
                    if let Some(trait_base) = trait_name.as_deref()
                        && let Some(method) = child.child_by_field_name("name")
                    {
                        edge_lines.push(format!(
                            "edge::implements[{trait_base}::{}]",
                            node_text(method, source)
                        ));
                    }
                    if let Some(target) = mutates_target.as_deref()
                        && has_mut_self_receiver(child, source)
                    {
                        edge_lines.push(format!("edge::mutates[{target}]"));
                    }
                    if !edge_lines.is_empty() {
                        doc.blocks.push(Block::Listing {
                            language: None,
                            code: edge_lines.join("\n"),
                        });
                    }
                    docs.push(doc);
                }
                buffered.clear();
            }
            _ => {
                buffered.clear();
            }
        }
    }
}

/// Emit a Document for a top-level `const` or `static` item. These have a
/// `name` (identifier), a `type`, and a `value`; without this arm they were
/// invisible.
fn extract_const_or_static(
    node: tree_sitter::Node,
    source: &str,
    path: &Path,
    crate_name: &str,
    file_name: &str,
    buffered_comments: &[String],
) -> Option<Document> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    let vis = get_visibility_with_source(node, source);
    let kind_label = if node.kind() == "static_item" {
        "Static"
    } else {
        "Const"
    };
    let ty = node
        .child_by_field_name("type")
        .map(|n| node_text(n, source).to_string());
    let anchor = make_anchor(crate_name, file_name, name);
    let span = node_to_span(node, path);
    let attrs = build_code_attributes(source, "constant", Some(path), Some(&span));
    // NodeType has no dedicated `Constant`; a top-level const/static is a named
    // value item, closest modelled as `Type` (the `Kind` row carries the detail).
    let mut blocks = Vec::new();
    if !buffered_comments.is_empty() {
        blocks.push(Block::Paragraph(buffered_comments.join("\n")));
    }
    let mut rows: Vec<Vec<String>> = vec![
        vec!["Kind".to_string(), kind_label.to_string()],
        vec!["Visibility".to_string(), format!("{:?}", vis)],
    ];
    if let Some(ref t) = ty {
        rows.push(vec!["Type".to_string(), t.clone()]);
    }
    blocks.push(Block::Table(aden_core::Table {
        headers: vec!["Property".to_string(), "Value".to_string()],
        rows,
    }));
    // Type-usage edges from the declared type.
    if let Some(ref t) = ty {
        let uses: Vec<String> = extract_type_idents(t);
        if !uses.is_empty() {
            let uses_code = uses
                .iter()
                .map(|u| format!("edge::uses[{}]", u))
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
        node_type: NodeType::Type,
        attributes: attrs,
        blocks,
        source_span: None,
        metadata: None,
        confidence: 0.9,
    })
}

/// Emit a Document for a top-level `type Alias = ...;`. The RHS types become
/// `Uses` edges so the alias links to what it references.
fn extract_type_alias(
    node: tree_sitter::Node,
    source: &str,
    path: &Path,
    crate_name: &str,
    file_name: &str,
    buffered_comments: &[String],
) -> Option<Document> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    let vis = get_visibility_with_source(node, source);
    let value = node
        .child_by_field_name("type")
        .map(|n| node_text(n, source).to_string());
    let anchor = make_anchor(crate_name, file_name, name);
    let span = node_to_span(node, path);
    let attrs = build_code_attributes(source, "type", Some(path), Some(&span));
    let mut blocks = Vec::new();
    if !buffered_comments.is_empty() {
        blocks.push(Block::Paragraph(buffered_comments.join("\n")));
    }
    let mut rows: Vec<Vec<String>> = vec![
        vec!["Kind".to_string(), "TypeAlias".to_string()],
        vec!["Visibility".to_string(), format!("{:?}", vis)],
    ];
    if let Some(ref v) = value {
        rows.push(vec!["Aliases".to_string(), v.clone()]);
    }
    blocks.push(Block::Table(aden_core::Table {
        headers: vec!["Property".to_string(), "Value".to_string()],
        rows,
    }));
    if let Some(ref v) = value {
        let uses: Vec<String> = extract_type_idents(v);
        if !uses.is_empty() {
            let uses_code = uses
                .iter()
                .map(|u| format!("edge::uses[{}]", u))
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
        node_type: NodeType::Type,
        attributes: attrs,
        blocks,
        source_span: None,
        metadata: None,
        confidence: 0.9,
    })
}

/// Pull plausible user/library type identifiers out of a type string (e.g.
/// `&HashMap<String, Vec<DocumentNode>>` → `DocumentNode`) so they can be linked
/// as `Uses` edges. Keeps PascalCase names and skips ubiquitous std containers
/// and primitives — those never resolve to a repo symbol anyway, so dropping
/// them keeps the store lean. Linking stays language-agnostic: only names that
/// match a stored symbol actually become edges.
fn extract_type_idents(ty: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in ty.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else {
            push_type_ident(&mut out, &cur);
            cur.clear();
        }
    }
    push_type_ident(&mut out, &cur);
    out
}

fn push_type_ident(out: &mut Vec<String>, ident: &str) {
    let Some(first) = ident.chars().next() else {
        return;
    };
    // Types are PascalCase by convention; a lowercase ident is a primitive,
    // lifetime, or keyword (`str`, `usize`, `dyn`, `mut`, …) — skip it.
    if !first.is_ascii_uppercase() {
        return;
    }
    const SKIP: &[&str] = &[
        "String", "Vec", "Option", "Result", "Box", "Rc", "Arc", "HashMap", "HashSet", "BTreeMap",
        "BTreeSet", "Cow", "Path", "PathBuf", "Self", "Ok", "Err", "Some", "None", "VecDeque",
        "Cell", "RefCell", "Mutex", "RwLock", "Duration", "Instant",
    ];
    if SKIP.contains(&ident) {
        return;
    }
    if !out.iter().any(|x| x == ident) {
        out.push(ident.to_string());
    }
}

/// Standard-library and common utility functions to exclude from call-graph extraction.
/// These generate noise (to_string, push, unwrap, etc.) without meaningful cross-module edges.
const SKIP_CALLEES: &[&str] = &[
    "to_string",
    "to_string_lossy",
    "to_str",
    "to_path_buf",
    "to_owned",
    "clone",
    "copy",
    "eq",
    "ne",
    "partial_cmp",
    "cmp",
    "push",
    "pop",
    "insert",
    "remove",
    "clear",
    "extend",
    "append",
    "map",
    "filter",
    "fold",
    "collect",
    "join",
    "split",
    "iter",
    "into_iter",
    "contains",
    "is_empty",
    "len",
    "get",
    "get_mut",
    "entry",
    "unwrap",
    "unwrap_or",
    "unwrap_or_else",
    "expect",
    "ok",
    "err",
    "map_err",
    "new",
    "default",
    "from",
    "into",
    "try_from",
    "try_into",
    "parse",
    "format",
    "write",
    "writeln",
    "print",
    "println",
    "eprintln",
    "walk",
    "children",
    "goto_first_child",
    "goto_next_sibling",
    "goto_parent",
    "kind",
    "utf8_text",
    "start_position",
    "end_position",
    "start_byte",
    "end_byte",
    "is_named",
    "is_ok",
    "is_err",
    "is_some",
    "is_none",
    "as_ref",
    "as_mut",
    "as_str",
    "as_bytes",
    "as_path",
    "chars",
    "lines",
    "bytes",
    "trim",
    "trim_start",
    "trim_end",
    "read_to_string",
    "read_dir",
    "read",
    "write_all",
    "create_dir_all",
    "canonicalize",
    "join",
    "parent",
    "extension",
    "file_name",
    "file_stem",
];

fn is_std_noise(name: &str) -> bool {
    SKIP_CALLEES.contains(&name)
}

/// Recursively walk an AST subtree and collect all `call_expression` nodes.
/// Returns a list of (callee_name, 1-based_line_number) for each *meaningful* call found.
/// Filters out std-lib noise and very short names.
fn extract_call_sites(node: tree_sitter::Node, source: &str) -> Vec<(String, usize)> {
    let mut calls = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        calls.extend(extract_call_sites(child, source));
    }
    if node.kind() == "call_expression"
        && let Some(func) = node.child_by_field_name("function")
    {
        let callee = resolve_callee_name(func, source);
        if !callee.is_empty() && callee.len() >= 3 && !SKIP_CALLEES.contains(&callee.as_str()) {
            let line = func.start_position().row + 1;
            calls.push((callee, line));
        }
    }
    calls
}

fn resolve_callee_name(node: tree_sitter::Node, source: &str) -> String {
    match node.kind() {
        "identifier" => node_text(node, source).to_string(),
        "field_expression" => {
            if let Some(name) = node.child_by_field_name("field") {
                node_text(name, source).to_string()
            } else {
                node_text(node, source).to_string()
            }
        }
        "scoped_identifier" => {
            let mut name_parts = Vec::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "identifier" {
                    name_parts.push(node_text(child, source));
                }
            }
            if name_parts.len() >= 2 {
                format!(
                    "{}::{}",
                    name_parts[name_parts.len() - 2],
                    name_parts[name_parts.len() - 1]
                )
            } else if !name_parts.is_empty() {
                name_parts.last().unwrap().to_string()
            } else {
                node_text(node, source).to_string()
            }
        }
        _ => node_text(node, source).to_string(),
    }
}

fn extract_struct(
    node: tree_sitter::Node,
    source: &str,
    path: &Path,
    crate_name: &str,
    file_name: &str,
    buffered_comments: &[String],
) -> Option<Document> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    let _vis = get_visibility_with_source(node, source);
    let mut fields = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        let mut bc = body.walk();
        for child in body.children(&mut bc) {
            if child.kind() == "field_declaration"
                && let Some(fname) = child.child_by_field_name("name")
                && let Some(fty) = child.child_by_field_name("type")
            {
                let f_vis = get_visibility_with_source(child, source);
                fields.push(FieldDef {
                    name: node_text(fname, source).to_string(),
                    ty: node_text(fty, source).to_string(),
                    visibility: f_vis,
                });
            }
        }
    }
    let doc_comment = if buffered_comments.is_empty() {
        None
    } else {
        Some(buffered_comments.join("\n"))
    };
    let anchor = make_anchor(crate_name, file_name, name);
    let span = node_to_span(node, path);
    let attrs = build_code_attributes(source, "type", Some(path), Some(&span));
    let mut blocks = Vec::new();
    if let Some(doc) = doc_comment {
        blocks.push(Block::Paragraph(doc));
    }
    let mut rows: Vec<Vec<String>> = vec![vec!["Kind".to_string(), "Struct".to_string()]];
    for f in &fields {
        rows.push(vec![
            format!("field {}", f.name),
            format!("{} (vis: {:?})", f.ty, f.visibility),
        ]);
    }
    blocks.push(Block::Table(aden_core::Table {
        headers: vec!["Property".to_string(), "Value".to_string()],
        rows,
    }));
    if !buffered_comments.is_empty() {
        blocks.push(Block::Admonition {
            kind: aden_core::AdmonitionKind::Note,
            text: "Extracted from source code via tree-sitter. Confidence is heuristic."
                .to_string(),
        });
    }
    Some(Document {
        anchor,
        node_type: NodeType::Type,
        attributes: attrs,
        blocks,
        source_span: None,
        metadata: None,
        confidence: 0.9,
    })
}

fn extract_enum(
    node: tree_sitter::Node,
    source: &str,
    path: &Path,
    crate_name: &str,
    file_name: &str,
    buffered_comments: &[String],
) -> Option<Document> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    let vis = get_visibility_with_source(node, source);
    let mut variants = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        let mut bc = body.walk();
        for child in body.children(&mut bc) {
            if child.kind() == "enum_variant"
                && let Some(vname) = child.child_by_field_name("name")
            {
                variants.push(FieldDef {
                    name: node_text(vname, source).to_string(),
                    ty: String::new(),
                    visibility: vis.clone(),
                });
            }
        }
    }
    let doc_comment = if buffered_comments.is_empty() {
        None
    } else {
        Some(buffered_comments.join("\n"))
    };
    let anchor = make_anchor(crate_name, file_name, name);
    let span = node_to_span(node, path);
    let attrs = build_code_attributes(source, "type", Some(path), Some(&span));
    let mut blocks = Vec::new();
    if let Some(doc) = doc_comment {
        blocks.push(Block::Paragraph(doc));
    }
    let mut rows: Vec<Vec<String>> = vec![vec!["Kind".to_string(), "Enum".to_string()]];
    for v in &variants {
        rows.push(vec![format!("variant {}", v.name), v.name.clone()]);
    }
    blocks.push(Block::Table(aden_core::Table {
        headers: vec!["Property".to_string(), "Value".to_string()],
        rows,
    }));
    if !buffered_comments.is_empty() {
        blocks.push(Block::Admonition {
            kind: aden_core::AdmonitionKind::Note,
            text: "Extracted from source code via tree-sitter. Confidence is heuristic."
                .to_string(),
        });
    }
    Some(Document {
        anchor,
        node_type: NodeType::Type,
        attributes: attrs,
        blocks,
        source_span: None,
        metadata: None,
        confidence: 0.9,
    })
}

fn extract_module(
    node: tree_sitter::Node,
    source: &str,
    path: &Path,
    crate_name: &str,
    file_name: &str,
    buffered_comments: &[String],
) -> Option<Document> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    let anchor = make_anchor(crate_name, file_name, name);
    let span = node_to_span(node, path);
    let attrs = build_code_attributes(source, "module", Some(path), Some(&span));
    let mut blocks = Vec::new();
    if !buffered_comments.is_empty() {
        blocks.push(Block::Paragraph(buffered_comments.join("\n")));
    }
    blocks.push(Block::Paragraph(format!("Module declaration for `{name}`")));
    Some(Document {
        anchor,
        node_type: NodeType::Module,
        attributes: attrs,
        blocks,
        source_span: None,
        metadata: None,
        confidence: 0.9,
    })
}

fn extract_trait(
    node: tree_sitter::Node,
    source: &str,
    path: &Path,
    crate_name: &str,
    file_name: &str,
    buffered_comments: &[String],
) -> Option<Document> {
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(name_node, source);
    let anchor = make_anchor(crate_name, file_name, name);
    let span = node_to_span(node, path);
    let attrs = build_code_attributes(source, "type", Some(path), Some(&span));
    let mut blocks = Vec::new();
    if !buffered_comments.is_empty() {
        blocks.push(Block::Paragraph(buffered_comments.join("\n")));
    }
    blocks.push(Block::Paragraph(format!("Trait `{name}`")));
    Some(Document {
        anchor,
        node_type: NodeType::Type,
        attributes: attrs,
        blocks,
        source_span: None,
        metadata: None,
        confidence: 0.9,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Helper: write a file at `dir/rel` and return the full path.
    fn write(dir: &TempDir, rel: &str, content: &str) -> std::path::PathBuf {
        let p = dir.path().join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
        p
    }

    // ── Test 1: Cargo.toml [package] name takes priority over dir name ────────
    //
    // Dir is named `my-pkg-dir` but the manifest declares `name = "my_pkg"`.
    // Before the fix this returned "my-pkg-dir"; after it must return "my_pkg".
    #[test]
    fn infer_crate_name_reads_package_name_from_manifest() {
        let tmp = TempDir::new().unwrap();
        write(
            &tmp,
            "my-pkg-dir/Cargo.toml",
            "[package]\nname = \"my_pkg\"\nversion = \"0.1.0\"\n",
        );
        let src = write(&tmp, "my-pkg-dir/src/lib.rs", "");
        assert_eq!(infer_crate_name(&src), "my_pkg");
    }

    // ── Test 2: no Cargo.toml → falls back to parent directory name ──────────
    //
    // This pins the pre-existing fallback behaviour so it is never regressed.
    // path.parent().file_name() for `my-crate/src/lib.rs` is `src`.
    #[test]
    fn infer_crate_name_falls_back_to_dir_name_when_no_manifest() {
        let tmp = TempDir::new().unwrap();
        let src = write(&tmp, "my-crate/src/lib.rs", "");
        // No Cargo.toml anywhere under tmp.
        let result = infer_crate_name(&src);
        assert_eq!(result, "src");
    }

    // ── Test 3: workspace root has only [workspace]; member has [package] ────
    //
    // Walking up from the file hits the *member* Cargo.toml first (nearest
    // ancestor).  That manifest has a valid [package] name, so it wins.
    #[test]
    fn infer_crate_name_member_name_wins_over_workspace_root() {
        let tmp = TempDir::new().unwrap();
        // Workspace root — no [package] section
        write(
            &tmp,
            "Cargo.toml",
            "[workspace]\nmembers = [\"my-pkg-dir\"]\n",
        );
        // Member manifest with a real package name
        write(
            &tmp,
            "my-pkg-dir/Cargo.toml",
            "[package]\nname = \"my_member\"\nversion = \"0.1.0\"\n",
        );
        let src = write(&tmp, "my-pkg-dir/src/lib.rs", "");
        assert_eq!(infer_crate_name(&src), "my_member");
    }

    /// Gap 7: mod.rs should map its file component to the parent directory path,
    /// not the bare "mod.rs" filename.  A function in `src/commands/mod.rs`
    /// under crate `mycrate` must produce anchor
    /// `aden://module/mycrate/src/commands#fn_name`, NOT
    /// `aden://module/mycrate/mod.rs#fn_name`.
    #[test]
    fn mod_rs_anchor_maps_to_parent_module() {
        let tmp = TempDir::new().unwrap();
        write(
            &tmp,
            "Cargo.toml",
            "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\n",
        );
        let src = write(&tmp, "src/commands/mod.rs", "pub fn dispatch() {}\n");
        let docs = extract_documents_inner(&src, "pub fn dispatch() {}\n").unwrap();
        assert!(!docs.is_empty(), "must extract at least one document");
        let anchor = &docs[0].anchor;
        assert_eq!(
            anchor, "aden://module/mycrate/src/commands#dispatch",
            "mod.rs in src/commands/ must produce file component 'src/commands', got {anchor:?}"
        );
    }

    /// Gap 7: lib.rs at the crate root should map to the parent module path.
    /// A function in `src/lib.rs` under crate `mycrate` must produce anchor
    /// `aden://module/mycrate/src#fn_name`, NOT `aden://module/mycrate/lib.rs#fn_name`.
    #[test]
    fn lib_rs_anchor_maps_to_parent_module() {
        let tmp = TempDir::new().unwrap();
        write(
            &tmp,
            "Cargo.toml",
            "[package]\nname = \"mycrate\"\nversion = \"0.1.0\"\n",
        );
        let src = write(&tmp, "src/lib.rs", "pub fn entry() {}\n");
        let docs = extract_documents_inner(&src, "pub fn entry() {}\n").unwrap();
        assert!(!docs.is_empty(), "must extract at least one document");
        let anchor = &docs[0].anchor;
        assert_eq!(
            anchor, "aden://module/mycrate/src#entry",
            "lib.rs in src/ must produce file component 'src', got {anchor:?}"
        );
    }
}
