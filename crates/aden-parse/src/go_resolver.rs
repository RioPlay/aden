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

use crate::extractor::{
    LanguageExtractor, build_code_attributes, infer_project_name, infer_project_root, make_anchor,
    project_relative_file,
};
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
        let project_root = infer_project_root(path);
        let file_name_owned = project_relative_file(path, &project_root);
        let file_name = file_name_owned.as_str();

        // Collect symbols and imports from the file.
        let mut symbols: Vec<GoSymbol> = Vec::new();
        let mut imports: Vec<GoImport> = Vec::new();
        walk_package_decl(
            tree.root_node(),
            source,
            &module_path,
            file_name,
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
                file_name,
            ) {
                docs.push(doc);
            }
        }

        // Emit a file-level Module document carrying edge::imports for each
        // import path found in the file.
        //
        // Target-string form: the raw import path string, quotes stripped.
        //   `import "fmt"`      → `fmt`
        //   `import "net/http"` → `net/http`
        if !imports.is_empty() {
            let mut seen: Vec<String> = Vec::new();
            let mut import_edges: Vec<String> = Vec::new();
            for imp in &imports {
                let edge = format!("edge::imports[{}]", imp.path);
                if !seen.contains(&imp.path) {
                    seen.push(imp.path.clone());
                    import_edges.push(edge);
                }
            }
            if !import_edges.is_empty() {
                let file_anchor = make_anchor(&module_path, file_name, "");
                let file_doc = Document {
                    anchor: file_anchor,
                    node_type: NodeType::Module,
                    attributes: Default::default(),
                    blocks: vec![Block::Listing {
                        language: None,
                        code: import_edges.join("\n"),
                    }],
                    source_span: None,
                    metadata: None,
                    confidence: 0.85,
                };
                docs.insert(0, file_doc);
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
    // No go.mod: fall back to the shared project-name inference (VCS-root
    // top dir / parent dir) instead of a literal "unknown", so manifest-less
    // Go files in a polyglot repo share the same project prefix as Python/TS
    // files beside them rather than collapsing into an inconsistent
    // `aden://module/unknown/…` group.
    infer_project_name(path)
}

#[derive(Debug)]
struct GoImport {
    alias: Option<String>, // local alias (e.g. "fmt" or "http")
    path: String,          // full import path (e.g. "fmt" or "net/http")
}

#[derive(Debug)]
struct GoSymbol<'a> {
    name: String,
    kind: NodeType,
    node: tree_sitter::Node<'a>,
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
                        // Use this spec's own node, not the outer
                        // `type_declaration`. In a grouped `type ( A ...; B ... )`
                        // all specs otherwise shared the whole-block span.
                        node: child,
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
                        // `*Point` — the pointee type is a child of the
                        // pointer_type, with no field name in tree-sitter-go, so
                        // descend to the first type node rather than looking up a
                        // (nonexistent) "type" field, which silently dropped every
                        // pointer-receiver method's type qualifier.
                        if let Some(t) = first_go_type_name(inner_child, source) {
                            return Some(t);
                        }
                    } else if inner_child.kind() == "type_identifier"
                        || inner_child.kind() == "generic_type"
                    {
                        return first_go_type_name(inner_child, source);
                    }
                }
            }
        }
    }
    None
}

/// The bare type name of a Go type node: a `type_identifier` directly, or the
/// underlying identifier of a `pointer_type`/`generic_type` (`*Command`,
/// `Tree[T]` → `Command`, `Tree`). Strips the package qualifier of a
/// `qualified_type` to the local name.
fn first_go_type_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        "type_identifier" => Some(node_text(node, source).to_string()),
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(
                    child.kind(),
                    "type_identifier" | "pointer_type" | "generic_type" | "qualified_type"
                ) && let Some(t) = first_go_type_name(child, source)
                {
                    return Some(t);
                }
            }
            None
        }
    }
}

/// The receiver *variable* name of a method, e.g. `c` in `func (c *Command) M()`.
/// Mirrors [`extract_receiver_type`] but returns the binding (the plain
/// `identifier` sibling of the type), which is needed to rewrite calls through
/// that variable into calls on its type. Returns `None` for plain functions and
/// for anonymous receivers (`func (*Command) M()`).
fn extract_receiver_var(node: tree_sitter::Node, source: &str) -> Option<String> {
    let recv_list = node.child_by_field_name("receiver")?;
    let mut cursor = recv_list.walk();
    for child in recv_list.children(&mut cursor) {
        if child.kind() == "parameter_list" || child.kind() == "parameter_declaration" {
            let mut inner = child.walk();
            for inner_child in child.children(&mut inner) {
                if inner_child.kind() == "identifier" {
                    return Some(node_text(inner_child, source).to_string());
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

/// Collect the raw type strings referenced in a symbol's signature so they can
/// be turned into `edge::uses`. For functions/methods this is the parameter
/// types and the result type; for `type_declaration`s backing a struct it is
/// the field types. The strings are fed through the shared
/// `extract_type_idents` helper, which keeps only PascalCase user types and
/// drops builtins — so capturing a slightly wide set here is harmless.
fn collect_go_signature_types(node: tree_sitter::Node, source: &str) -> Vec<String> {
    let mut types: Vec<String> = Vec::new();
    match node.kind() {
        "function_declaration" | "method_declaration" => {
            // Parameter types live under the `parameters` field; multiple
            // returns use a second `parameter_list`, a single return is the
            // bare type node under the `result` field.
            if let Some(params) = node.child_by_field_name("parameters") {
                collect_param_decl_types(params, source, &mut types);
            }
            if let Some(result) = node.child_by_field_name("result") {
                if result.kind() == "parameter_list" {
                    collect_param_decl_types(result, source, &mut types);
                } else {
                    push_go_type(result, source, &mut types);
                }
            }
        }
        "type_declaration" => {
            // type Foo struct { a Bar; b Baz } — capture each field type.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "type_spec" {
                    collect_type_spec_field_types(child, source, &mut types);
                }
            }
        }
        // A symbol's node is now its own `type_spec` (see walk_package_decl), so
        // handle that directly too.
        "type_spec" => {
            collect_type_spec_field_types(node, source, &mut types);
        }
        _ => {}
    }
    types
}

/// Given a `type_spec` backing a struct, push each field's type.
fn collect_type_spec_field_types(spec: tree_sitter::Node, source: &str, out: &mut Vec<String>) {
    if let Some(ty) = spec.child_by_field_name("type")
        && ty.kind() == "struct_type"
    {
        let mut sc = ty.walk();
        for fld_list in ty.children(&mut sc) {
            if fld_list.kind() == "field_declaration_list" {
                let mut fc = fld_list.walk();
                for fld in fld_list.children(&mut fc) {
                    if fld.kind() == "field_declaration"
                        && let Some(fty) = fld.child_by_field_name("type")
                    {
                        push_go_type(fty, source, out);
                    }
                }
            }
        }
    }
}

/// Walk a `parameter_list`, pushing the `type` of each `parameter_declaration`.
fn collect_param_decl_types(list: tree_sitter::Node, source: &str, out: &mut Vec<String>) {
    let mut cursor = list.walk();
    for child in list.children(&mut cursor) {
        if (child.kind() == "parameter_declaration"
            || child.kind() == "variadic_parameter_declaration")
            && let Some(ty) = child.child_by_field_name("type")
        {
            push_go_type(ty, source, out);
        }
    }
}

fn push_go_type(node: tree_sitter::Node, source: &str, out: &mut Vec<String>) {
    let text = node_text(node, source).trim().to_string();
    if !text.is_empty() && !out.contains(&text) {
        out.push(text);
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
        let mut calls = resolve_go_call_sites(body, source, all_symbols, imports);
        // Receiver-variable resolution: inside `func (c *Command) M()`, a call
        // `c.Other()` targets a method on the receiver's OWN type. We know both
        // the receiver variable (`c`) and its type (`Command`, the prefix of this
        // method's qualified name), so rewrite `c.Other` → `Command.Other`. The
        // linker then resolves it by exact name — the Go analogue of self/this
        // resolution, with zero false-edge risk (only the actual receiver var is
        // rewritten; other locals are left for ordinary name-based linking).
        if let (Some(recv_var), Some((recv_type, _))) = (
            extract_receiver_var(sym.node, source),
            sym.name.rsplit_once('.'),
        ) {
            let prefix = format!("{}.", recv_var);
            for call in &mut calls {
                if let Some(rest) = call.callee.strip_prefix(&prefix)
                    && !rest.is_empty()
                    && !rest.contains('.')
                {
                    call.callee = format!("{}.{}", recv_type, rest);
                }
            }
        }
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

    // Type-usage edges: types named in the signature (params/result) or struct
    // fields are Used, so a type that is used but never called is not a false
    // dead-code candidate. Only names matching a stored symbol become edges.
    {
        let mut type_uses: Vec<String> = Vec::new();
        for ty in collect_go_signature_types(sym.node, source) {
            for t in crate::tree_sitter_common::extract_type_idents(&ty) {
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

use crate::tree_sitter_common::node_to_span;
