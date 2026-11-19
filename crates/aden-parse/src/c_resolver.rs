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

use crate::extractor::{
    CodeSource, LanguageExtractor, infer_project_name, infer_project_root, make_anchor,
    project_relative_file,
};
use aden_core::{Block, Document, NodeType, Result};
use std::collections::{HashMap, HashSet};
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
        let language = crate::get_ts_language("c")
            .map_err(|e| aden_core::Error::Parse(format!("language-pack: {}", e)))?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language)
            .map_err(|e| aden_core::Error::Parse(e.to_string()))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| aden_core::Error::Parse("tree-sitter returned None".to_string()))?;

        let file_source = &CodeSource::new(source, path);
        let module_name = infer_project_name(path);
        let project_root = infer_project_root(path);
        let file_name_owned = project_relative_file(path, &project_root);
        let file_name = file_name_owned.as_str();

        let mut symbols: Vec<CSymbol> = Vec::new();
        let mut includes: Vec<CInclude> = Vec::new();
        let mut non_function_bindings = HashSet::new();
        walk_translation_unit(
            tree.root_node(),
            source,
            &mut symbols,
            &mut includes,
            &mut non_function_bindings,
        );
        let context = CFileContext {
            includes: &includes,
            non_function_bindings: &non_function_bindings,
        };

        let mut docs = Vec::new();
        for sym in &symbols {
            if let Some(doc) = emit_c_symbol(
                file_source,
                sym,
                source,
                &symbols,
                &context,
                &module_name,
                file_name,
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

struct CFileContext<'a> {
    includes: &'a [CInclude],
    non_function_bindings: &'a HashSet<String>,
}

#[derive(Debug)]
struct CSymbol<'a> {
    name: String,
    kind: NodeType,
    node: tree_sitter::Node<'a>,
    doc_comment: Option<String>,
}

fn walk_translation_unit<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    symbols: &mut Vec<CSymbol<'a>>,
    includes: &mut Vec<CInclude>,
    non_function_bindings: &mut HashSet<String>,
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
            for (name, kind) in extract_declaration_info(node, source, non_function_bindings) {
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
        walk_translation_unit(child, source, symbols, includes, non_function_bindings);
    }
}

/// Extract function name from a function_definition node.
fn extract_function_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    let declarator = node.child_by_field_name("declarator")?;
    find_declarator_name(declarator, source)
}

/// Extract name + kind from a declaration node (prototype, struct, enum, etc.).
fn extract_declaration_info(
    node: tree_sitter::Node,
    source: &str,
    non_function_bindings: &mut HashSet<String>,
) -> Vec<(String, NodeType)> {
    let mut symbols = Vec::new();
    // K&R parameter declarations are direct children of a function definition,
    // outside its skipped body. Their bindings are not translation-unit globals.
    let inside_function = std::iter::successors(node.parent(), |parent| parent.parent())
        .any(|parent| parent.kind() == "function_definition");
    // Inspect each declarator independently: a variable can precede a prototype.
    let mut cursor = node.walk();
    for child in node.children_by_field_name("declarator", &mut cursor) {
        if let Some((name, is_function)) = declarator_binding(child, source) {
            if is_function {
                symbols.push((name, NodeType::Function));
            } else if !inside_function {
                non_function_bindings.insert(name);
            }
        }
    }
    if symbols.is_empty()
        && let Some(name) = find_type_name(node, source)
    {
        symbols.push((name, NodeType::Type));
    }
    symbols
}

/// The derived operator nearest the identifier determines what it declares.
/// For example, `*f(void)` declares a function, but `(*f)(void)` a pointer.
fn declarator_binding(mut node: tree_sitter::Node, source: &str) -> Option<(String, bool)> {
    let mut is_function = false;
    let mut initialized = false;
    loop {
        match node.kind() {
            "identifier" => {
                return Some((
                    node_text(node, source).trim().to_string(),
                    is_function && !initialized,
                ));
            }
            "function_declarator" => is_function = true,
            "pointer_declarator" | "array_declarator" => is_function = false,
            "parenthesized_declarator" | "attributed_declarator" => {}
            // A function cannot have an initializer. Never inspect its value.
            "init_declarator" => initialized = true,
            _ => return None,
        }
        node = inner_declarator(node)?;
    }
}

fn inner_declarator(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    if let Some(inner) = node.child_by_field_name("declarator") {
        return Some(inner);
    }
    if matches!(
        node.kind(),
        "parenthesized_declarator" | "attributed_declarator"
    ) {
        // These C grammar wrappers have no declarator field.
        let mut cursor = node.walk();
        return node.named_children(&mut cursor).find(|child| {
            matches!(
                child.kind(),
                "identifier"
                    | "function_declarator"
                    | "pointer_declarator"
                    | "array_declarator"
                    | "parenthesized_declarator"
                    | "attributed_declarator"
            )
        });
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
    file_source: &CodeSource<'_>,
    sym: &CSymbol<'a>,
    source: &str,
    all_symbols: &[CSymbol<'a>],
    context: &CFileContext<'_>,
    module: &str,
    file_name: &str,
) -> Option<Document> {
    let path = file_source.path;
    let anchor = make_anchor(module, file_name, &sym.name);
    let span = node_to_span(sym.node, path);
    let attrs = file_source.attributes(&format!("{:?}", sym.kind).to_lowercase(), &span);
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
    if !context.includes.is_empty() {
        let inc_rows: Vec<Vec<String>> = context
            .includes
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
    if sym.node.child_by_field_name("body").is_some() {
        let calls =
            resolve_c_call_sites(sym.node, source, all_symbols, context.non_function_bindings);
        if !calls.is_empty() {
            let (direct_calls, indirect_calls): (Vec<_>, Vec<_>) =
                calls.iter().partition(|c| !c.indirect);
            // `Callee` rows are also consumed as direct graph-edge evidence.
            // Keep known object invocations visible without making them eligible
            // to link to a same-named function in another translation unit.
            for (header, call_sites) in [
                ("Callee", &direct_calls),
                ("Indirect callee", &indirect_calls),
            ] {
                if !call_sites.is_empty() {
                    blocks.push(Block::Table(aden_core::Table {
                        headers: vec![header.to_string(), "Line".to_string()],
                        rows: call_sites
                            .iter()
                            .map(|c| vec![c.callee.clone(), c.line.to_string()])
                            .collect(),
                    }));
                }
            }
            let edge_code = direct_calls
                .iter()
                .map(|c| format!("edge::calls[{}]", c.callee))
                .collect::<Vec<_>>()
                .join("\n");
            if !edge_code.is_empty() {
                blocks.push(Block::Listing {
                    language: None,
                    code: edge_code,
                });
            }
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
    indirect: bool,
}

struct CBindingScopes<'a> {
    globals: &'a HashSet<String>,
    // `true` is an object, `false` is a locally declared function.
    scopes: Vec<HashMap<String, bool>>,
}

impl CBindingScopes<'_> {
    fn declare(&mut self, name: String, indirect: bool) {
        self.scopes.last_mut().unwrap().insert(name, indirect);
    }

    fn is_indirect(&self, name: &str) -> bool {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
            .unwrap_or_else(|| self.globals.contains(name))
    }
}

fn resolve_c_call_sites<'a>(
    function: tree_sitter::Node<'a>,
    source: &str,
    all_symbols: &[CSymbol<'a>],
    globals: &HashSet<String>,
) -> Vec<CCallSite> {
    let mut bindings = CBindingScopes {
        globals,
        scopes: vec![HashMap::new()],
    };
    // Only the parameter list nearest the defined function's name belongs to
    // this function. Nested callback and returned-function parameters do not.
    let mut declarator = function.child_by_field_name("declarator");
    let mut parameters = None;
    while let Some(node) = declarator {
        if node.kind() == "function_declarator" {
            parameters = node.child_by_field_name("parameters");
        }
        declarator = inner_declarator(node);
    }
    if let Some(parameters) = parameters {
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            if parameter.kind() == "parameter_declaration" {
                if let Some(declarator) = parameter.child_by_field_name("declarator")
                    && let Some((name, _)) = declarator_binding(declarator, source)
                {
                    // C adjusts function and array parameters to object pointers.
                    bindings.declare(name, true);
                }
            } else if parameter.kind() == "identifier" {
                bindings.declare(node_text(parameter, source).to_string(), true);
            }
        }
    }
    // Old-style parameter declarations precede the body as direct children.
    let mut cursor = function.walk();
    for child in function.named_children(&mut cursor) {
        if child.kind() == "declaration" {
            let mut cursor = child.walk();
            for declarator in child.children_by_field_name("declarator", &mut cursor) {
                if let Some((name, _)) = declarator_binding(declarator, source) {
                    bindings.declare(name, true);
                }
            }
        }
    }
    let mut calls = Vec::new();
    if let Some(body) = function.child_by_field_name("body") {
        walk_c_call_sites(body, source, all_symbols, &mut bindings, &mut calls);
    }
    calls
}

fn walk_c_call_sites<'a>(
    node: tree_sitter::Node<'a>,
    source: &str,
    all_symbols: &[CSymbol<'a>],
    bindings: &mut CBindingScopes<'_>,
    calls: &mut Vec<CCallSite>,
) {
    if matches!(node.kind(), "parameter_list" | "function_definition") {
        return;
    }
    let scoped = matches!(node.kind(), "compound_statement" | "for_statement");
    if scoped {
        bindings.scopes.push(HashMap::new());
    }
    if node.kind() == "declaration" {
        let mut cursor = node.walk();
        for (index, child) in node.children(&mut cursor).enumerate() {
            if node.field_name_for_child(index as u32) != Some("declarator") {
                walk_c_call_sites(child, source, all_symbols, bindings, calls);
                continue;
            }
            let declarator = if child.kind() == "init_declarator" {
                child.child_by_field_name("declarator").unwrap_or(child)
            } else {
                child
            };
            // A name comes into scope after its declarator, before its initializer.
            walk_c_call_sites(declarator, source, all_symbols, bindings, calls);
            if let Some((name, is_function)) = declarator_binding(child, source) {
                bindings.declare(name, !is_function);
            }
            if child.kind() == "init_declarator"
                && let Some(value) = child.child_by_field_name("value")
            {
                walk_c_call_sites(value, source, all_symbols, bindings, calls);
            }
        }
    } else {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk_c_call_sites(child, source, all_symbols, bindings, calls);
        }
    }

    if node.kind() == "call_expression"
        && let Some(func) = node.child_by_field_name("function")
    {
        let callee = resolve_c_callee(func, source, all_symbols);
        if !callee.is_empty() && callee.len() >= 2 && !is_c_std_noise(&callee) {
            let line = func.start_position().row + 1;
            let indirect =
                callee_binding_name(func, source).is_some_and(|name| bindings.is_indirect(name));
            calls.push(CCallSite {
                callee,
                line,
                indirect,
            });
        }
    }
    if scoped {
        bindings.scopes.pop();
    }
}

fn callee_binding_name<'a>(mut node: tree_sitter::Node<'a>, source: &'a str) -> Option<&'a str> {
    loop {
        match node.kind() {
            "identifier" => return Some(node_text(node, source).trim()),
            "parenthesized_expression" => node = node.named_child(0)?,
            "pointer_expression" | "subscript_expression" => {
                node = node.child_by_field_name("argument")?;
            }
            _ => return None,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_functions(source: &str, expected: &[&str]) {
        let language = crate::get_ts_language("c").unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut symbols = Vec::new();
        let mut includes = Vec::new();
        let mut non_function_bindings = HashSet::new();
        walk_translation_unit(
            tree.root_node(),
            source,
            &mut symbols,
            &mut includes,
            &mut non_function_bindings,
        );
        let actual: Vec<_> = symbols
            .iter()
            .filter(|symbol| symbol.kind == NodeType::Function)
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert_eq!(actual, expected, "{}", tree.root_node().to_sexp());

        // Exercise document emission as well as the internal classification.
        let documents = CResolver::new()
            .extract_documents(source, Path::new("declarators.c"))
            .unwrap();
        let functions: Vec<_> = documents
            .iter()
            .filter(|document| document.node_type == NodeType::Function)
            .collect();
        assert_eq!(functions.len(), expected.len());
        for (document, name) in functions.iter().zip(expected) {
            assert!(
                document.anchor.ends_with(&format!("#{name}")),
                "{}",
                document.anchor
            );
            assert!(document.source_span.is_some());
        }
    }

    #[test]
    fn c_declarators_reject_linux_annotated_callback_variable() {
        let source = "#define __initdata\nvoid (*__initdata late_time_init)(void);\nvoid start_kernel(void) { late_time_init(); }\n";
        assert_functions(source, &["start_kernel"]);
        assert_indirect_call_evidence(source, &["late_time_init"]);
    }

    #[test]
    fn c_declarators_preserve_prototypes_and_pointer_returning_functions() {
        assert_functions(
            r#"
void ordinary(void);
void annotated(void) __attribute__((noreturn));
char *returns_pointer(void);
void (*returns_callback(void))(int);
int (parenthesized)(void);
int (attributed [[deprecated]])(void);
int (((nested)))(void);
void accepts_callback(void (*callback)(int));
int *defined(void) { return 0; }
"#,
            &[
                "ordinary",
                "annotated",
                "returns_pointer",
                "returns_callback",
                "parenthesized",
                "attributed",
                "nested",
                "accepts_callback",
                "defined",
            ],
        );
    }

    #[test]
    fn c_declarators_reject_globals_initializers_and_callback_arrays() {
        assert_functions(
            r#"
int global = 1;
int *pointer = 0;
int (parenthesized_global);
void (*callback)(void);
void (*initialized_callback)(void) = 0;
void (*callbacks[2])(void);
void (*(*nested_pointer)[2])(void);
void use_callbacks(void (*parameter_callback)(void)) {
    void (*local_callback)(void) = parameter_callback;
    local_callback();
}
"#,
            &["use_callbacks"],
        );
    }

    #[test]
    fn c_declarators_find_all_functions_after_variables() {
        assert_functions(
            "int global = 1, actual(void), *pointer, second(int), (*callback)(void);",
            &["actual", "second"],
        );
    }

    fn assert_indirect_call_evidence(source: &str, names: &[&str]) {
        let documents = CResolver::new()
            .extract_documents(source, Path::new("declarators.c"))
            .unwrap();
        for name in names {
            let call_line = source
                .lines()
                .position(|line| line.contains(&format!("{name}();")))
                .unwrap()
                + 1;
            assert!(
                documents.iter().any(|document| {
                    document.blocks.iter().any(|block| {
                        matches!(block, Block::Table(table) if table.headers == ["Indirect callee", "Line"]
                        && table.rows.iter().any(|row| row[0] == *name && row[1] == call_line.to_string()))
                    })
                }),
                "missing indirect call-table evidence for {name} at line {call_line}"
            );
            assert!(
                !documents.iter().any(|document| {
                    document.blocks.iter().any(|block| {
                        matches!(block, Block::Table(table) if table.headers == ["Callee", "Line"]
                        && table.rows.iter().any(|row| row[0] == *name))
                    })
                }),
                "object call incorrectly emitted in direct Callee table: {name}"
            );
            assert!(
                !documents.iter().any(|document| {
                    document.blocks.iter().any(|block| {
                        matches!(block, Block::Listing { code, .. }
                        if code.lines().any(|line| line == format!("edge::calls[{name}]")))
                    })
                }),
                "object call incorrectly emitted as direct Calls edge: {name}"
            );
        }
    }

    #[test]
    fn c_declarators_keep_global_object_calls_unresolved() {
        let source = r#"
void (*callback)(void);
void (*initialized_callback)(void) = 0;
int global = 1;
void real_function(void);
void caller(void) {
    callback();
    initialized_callback();
    global();
    real_function();
}
"#;
        assert_indirect_call_evidence(source, &["callback", "initialized_callback", "global"]);
        let documents = CResolver::new()
            .extract_documents(source, Path::new("declarators.c"))
            .unwrap();
        assert!(documents.iter().any(|document| {
            document.blocks.iter().any(|block| {
                matches!(block, Block::Listing { code, .. }
                    if code.lines().any(|line| line == "edge::calls[real_function]"))
            })
        }));
        assert!(documents.iter().any(|document| {
            document.blocks.iter().any(|block| {
                matches!(block, Block::Table(table) if table.headers == ["Callee", "Line"]
                    && table.rows.iter().any(|row| row[0] == "real_function"))
            })
        }));
    }

    #[test]
    fn c_declarators_do_not_treat_kr_parameters_as_global_bindings() {
        let source = r#"
void callback(void);
void accepts(callback)
void (*callback)(void);
{ callback(); }
void direct_caller(void) { callback(); }
"#;
        let documents = CResolver::new()
            .extract_documents(source, Path::new("declarators.c"))
            .unwrap();
        let caller = documents
            .iter()
            .find(|document| document.anchor.ends_with("#direct_caller"))
            .unwrap();
        assert!(caller.blocks.iter().any(|block| {
            matches!(block, Block::Listing { code, .. }
                if code.lines().any(|line| line == "edge::calls[callback]"))
        }));
        assert_scoped_calls(source, "accepts", &[("callback", "{ callback(); }", true)]);
    }

    fn assert_scoped_calls(source: &str, function: &str, expected: &[(&str, &str, bool)]) {
        let documents = CResolver::new()
            .extract_documents(source, Path::new("scoped.c"))
            .unwrap();
        let document = documents
            .iter()
            .find(|document| document.anchor.ends_with(&format!("#{function}")))
            .unwrap();
        let mut actual = Vec::new();
        let mut edges = Vec::new();
        for block in &document.blocks {
            match block {
                Block::Table(table)
                    if table.headers == ["Callee", "Line"]
                        || table.headers == ["Indirect callee", "Line"] =>
                {
                    for row in &table.rows {
                        actual.push((
                            row[0].clone(),
                            row[1].parse::<usize>().unwrap(),
                            table.headers[0] == "Indirect callee",
                        ));
                    }
                }
                Block::Listing { code, .. } => {
                    edges.extend(code.lines().map(str::to_string));
                }
                _ => {}
            }
        }
        let mut expected_rows: Vec<_> = expected
            .iter()
            .map(|(name, marker, indirect)| {
                (
                    name.to_string(),
                    source
                        .lines()
                        .position(|line| line.contains(marker))
                        .unwrap()
                        + 1,
                    *indirect,
                )
            })
            .collect();
        actual.sort();
        expected_rows.sort();
        assert_eq!(actual, expected_rows, "{function}: {source}");
        let mut expected_edges: Vec<_> = expected
            .iter()
            .filter(|(_, _, indirect)| !indirect)
            .map(|(name, _, _)| format!("edge::calls[{name}]"))
            .collect();
        edges.sort();
        edges.dedup();
        expected_edges.sort();
        expected_edges.dedup();
        assert_eq!(edges, expected_edges, "{function}: {source}");
    }

    #[test]
    fn c_scoped_callbacks_bind_only_own_parameters() {
        let source = r#"
void callback(void);
void nested_name(void);
void ordinary(void);
void parameter_user(void (*callback)(int nested_name), void adjusted(void)) {
    callback(1); // pointer parameter
    adjusted(); // adjusted function parameter
    nested_name(); // nested prototype name is not a binding here
    ordinary(); // ordinary direct call
}
void independent(void) { callback(); } // independent caller
"#;
        assert_scoped_calls(
            source,
            "parameter_user",
            &[
                ("callback", "// pointer parameter", true),
                ("adjusted", "// adjusted function parameter", true),
                ("nested_name", "// nested prototype name", false),
                ("ordinary", "// ordinary direct call", false),
            ],
        );
        assert_scoped_calls(
            source,
            "independent",
            &[("callback", "// independent caller", false)],
        );
    }

    #[test]
    fn c_scoped_callbacks_restore_outer_binding_after_block() {
        let source = r#"
void callback(void);
void actual_target(void);
void scoped(void) {
    callback(); // before block
    {
        void (*callback)(void) = actual_target;
        callback(); // inside block
    }
    callback(); // after block
}
void sibling(void) { callback(); } // sibling caller
"#;
        assert_scoped_calls(
            source,
            "scoped",
            &[
                ("callback", "// before block", false),
                ("callback", "// inside block", true),
                ("callback", "// after block", false),
            ],
        );
        assert_scoped_calls(
            source,
            "sibling",
            &[("callback", "// sibling caller", false)],
        );
    }

    #[test]
    fn c_scoped_callbacks_limit_for_initializer_binding_to_loop() {
        let source = r#"
int callback(void);
void loops(void) {
    for (int (*callback)(void) = 0;
         callback(); // loop condition
         callback()) { // loop update
        callback(); // loop body
    }
    callback(); // after loop
}
"#;
        assert_scoped_calls(
            source,
            "loops",
            &[
                ("callback", "// loop condition", true),
                ("callback", "// loop update", true),
                ("callback", "// loop body", true),
                ("callback", "// after loop", false),
            ],
        );
    }

    #[test]
    fn c_scoped_callbacks_local_function_declaration_overrides_object() {
        let source = r#"
void callback(void);
void overrides(void) {
    void (*callback)(void) = 0;
    callback(); // outer object
    {
        extern void callback(void);
        callback(); // inner function
    }
    callback(); // restored object
}
"#;
        assert_scoped_calls(
            source,
            "overrides",
            &[
                ("callback", "// outer object", true),
                ("callback", "// inner function", false),
                ("callback", "// restored object", true),
            ],
        );
    }

    #[test]
    fn c_scoped_callbacks_observe_declarator_and_initializer_order() {
        let source = r#"
int callback(void);
void ordered(void) {
    {
        int callback[callback()]; // declarator bound uses outer function
    }
    {
        int (*callback)(void) = (callback(), 0); // initializer uses new object
        callback(); // initialized object
    }
    callback(); // after declarations
}
"#;
        assert_scoped_calls(
            source,
            "ordered",
            &[
                ("callback", "// declarator bound", false),
                ("callback", "// initializer uses", true),
                ("callback", "// initialized object", true),
                ("callback", "// after declarations", false),
            ],
        );
    }

    #[test]
    fn c_scoped_callbacks_handle_pointer_callees_and_returned_prototypes() {
        let source = r#"
void returned_parameter(void);
int (*factory(void (*callback)(void)))(int returned_parameter) {
    (callback)(); // parenthesized callback
    (*callback)(); // dereferenced callback
    returned_parameter(); // returned prototype name is not a binding here
    return 0;
}
"#;
        assert_scoped_calls(
            source,
            "factory",
            &[
                ("(callback)", "// parenthesized callback", true),
                ("(*callback)", "// dereferenced callback", true),
                ("returned_parameter", "// returned prototype name", false),
            ],
        );
    }

    #[test]
    fn c_declarators_preserve_types_and_declaration_source_span() {
        let source = "struct Example { int value; };\ntypedef int Count;\n/** Prototype. */\nint global = 1, actual(void);\n";
        let documents = CResolver::new()
            .extract_documents(source, Path::new("declarators.c"))
            .unwrap();
        for name in ["Example", "Count"] {
            assert!(documents.iter().any(|document| {
                document.node_type == NodeType::Type
                    && document.anchor.ends_with(&format!("#{name}"))
            }));
        }
        let function = documents
            .iter()
            .find(|document| document.node_type == NodeType::Function)
            .unwrap();
        let span = function.source_span.as_ref().unwrap();
        assert_eq!(span.start_line, 4);
        assert_eq!(span.end_line, 4);
        assert!(function.blocks.iter().any(|block| {
            matches!(block, Block::Paragraph(text) if text == "/** Prototype. */")
        }));
    }
}
