// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// Original author and maintainer: RioPlay
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
use aden_core::contract::{ContractDocument, RegionBlock};
use aden_core::{AdmonitionKind, Block, Document, Table};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fmt::Write;

pub mod check;
pub mod templates;

fn infer_module_anchor(source_path: Option<&str>) -> Option<String> {
    let path_str = source_path?;

    // Try common crate patterns:
    // - crates/{name}/src/*.rs (workspace)
    // - src/{name}/*.rs (monorepo)
    // - lib/{name}/*.rs
    // - {name}/src/*.rs (language-specific layouts)

    // Pattern 1: crates/crate-name/src/
    if let Ok(re) = Regex::new(r"(?:^|/)crates/([^/]+)/src/")
        && let Some(caps) = re.captures(path_str)
            && let Some(m) = caps.get(1) {
                return Some(format!("mod-{}", m.as_str()));
            }

    // Pattern 2: src/module-name/ or src/module_name/
    if let Ok(re) = Regex::new(r"(?:^|/)src/([^/]+)")
        && let Some(caps) = re.captures(path_str)
            && let Some(m) = caps.get(1) {
                let name = m.as_str();
                // Skip common non-module directories
                if !name.eq_ignore_ascii_case("test") && !name.eq_ignore_ascii_case("tests")
                    && !name.eq_ignore_ascii_case("bin") && !name.eq_ignore_ascii_case("example")
                    && !name.eq_ignore_ascii_case("examples") && !name.starts_with('.') {
                    return Some(format!("mod-{}", name));
                }
            }

    // Pattern 3: lib/module-name/ (Elixir, Ruby, etc.)
    if let Ok(re) = Regex::new(r"(?:^|/)lib/([^/]+)")
        && let Some(caps) = re.captures(path_str)
            && let Some(m) = caps.get(1) {
                return Some(format!("mod-{}", m.as_str()));
            }

    // Pattern 4: Root-level module (e.g., src/lib.rs or src/main.rs -> mod-root)
    if let Ok(re) = Regex::new(r"(?:^|/)src/(?:lib|main)\.rs$")
        && re.is_match(path_str) {
            return Some("mod-root".to_string());
        }

    None
}

#[cfg(test)]
mod tests;

/// Emit a single Document as an AsciiDoc string.
pub fn emit_document(doc: &Document) -> String {
    let mut out = String::new();
    // Attributes
    for (key, value) in &doc.attributes {
        writeln!(out, ":{key}: {value}").unwrap();
    }
    writeln!(out).unwrap();
    // Anchor + Title
    writeln!(out, "[[{}]]", doc.anchor).unwrap();
    let title = derive_title(doc);
    writeln!(out, "= {title}").unwrap();
    writeln!(out).unwrap();
    // Blocks
    for block in &doc.blocks {
        emit_block(&mut out, block);
        writeln!(out).unwrap();
    }
    out
}

/// Emit multiple Documents separated by page breaks.
pub fn emit(documents: &[Document]) -> String {
    documents
        .iter()
        .map(emit_document)
        .collect::<Vec<_>>()
        .join("\n<<<\n")
}

fn derive_title(doc: &Document) -> String {
    // Use symbol name from anchor if available, else anchor itself.
    if let Some(pos) = doc.anchor.rfind('#') {
        doc.anchor[pos + 1..].to_string()
    } else {
        doc.anchor.clone()
    }
}

fn emit_block(out: &mut String, block: &Block) {
    match block {
        Block::Paragraph(text) => {
            writeln!(out, "{text}").unwrap();
        }
        Block::Table(table) => {
            emit_table(out, table);
        }
        Block::Listing { language, code } => {
            if let Some(lang) = language {
                writeln!(out, "[source,{lang}]").unwrap();
            } else {
                writeln!(out, "[listing]").unwrap();
            }
            writeln!(out, "----").unwrap();
            writeln!(out, "{code}").unwrap();
            writeln!(out, "----").unwrap();
        }
        Block::Admonition { kind, text } => {
            let label = match kind {
                AdmonitionKind::Note => "NOTE",
                AdmonitionKind::Tip => "TIP",
                AdmonitionKind::Warning => "WARNING",
                AdmonitionKind::Important => "IMPORTANT",
                AdmonitionKind::Caution => "CAUTION",
            };
            writeln!(out, "{label}: {text}").unwrap();
        }
        Block::DescriptionList(items) => {
            for (term, def) in items {
                writeln!(out, "{term}:: {def}").unwrap();
            }
        }
    }
}

fn emit_table(out: &mut String, table: &Table) {
    writeln!(out, "|===").unwrap();
    // Header row
    let header_row: String = table
        .headers
        .iter()
        .map(|h| format!("|{h}"))
        .collect::<String>();
    writeln!(out, "{header_row}").unwrap();
    writeln!(out).unwrap();
    // Data rows
    for row in &table.rows {
        let row_str: String = row.iter().map(|c| format!("|{c}")).collect::<String>();
        writeln!(out, "{row_str}").unwrap();
    }
    writeln!(out, "|===").unwrap();
}

/// Emit a ContractDocument (region-aware) as AsciiDoc text.
///
/// Each region block is wrapped in its corresponding region marker
/// and a `----` delimiter block.
pub fn emit_contract_document(doc: &ContractDocument) -> String {
    let mut out = String::new();

    // Header attributes
    for (key, value) in &doc.header_attrs {
        writeln!(out, ":{key}: {value}").unwrap();
    }
    if !doc.header_attrs.is_empty() {
        writeln!(out).unwrap();
    }

    // Region blocks
    for block in &doc.blocks {
        emit_region_block(&mut out, block);
        writeln!(out).unwrap();
    }

    // Prose (permissive mode leftovers)
    for line in &doc.prose {
        writeln!(out, "{line}").unwrap();
    }

    // Auto-link to module doc for source files
    if let Some(module_anchor) = infer_module_anchor(doc.header_attrs.get("source_file").map(|s| s.as_str())) {
        writeln!(out).unwrap();
        writeln!(out, "See also: <<{}>>", module_anchor).unwrap();
    }

    out
}

fn emit_region_block(out: &mut String, block: &RegionBlock) {
    let region_tag = match &block.tag {
        Some(tag) => format!("{}#{}", block.region, tag),
        None => block.region.to_string(),
    };

    // Write region header with attributes
    write!(out, "[{region_tag}]").unwrap();
    if !block.attributes.is_empty() {
        let attrs: Vec<String> = block
            .attributes
            .iter()
            .map(|(k, v)| format!(" :{k}: {v}"))
            .collect();
        write!(out, "{}", attrs.join("")).unwrap();
    }
    writeln!(out).unwrap();

    // Delimited block
    writeln!(out, "----").unwrap();
    writeln!(out, "{}", block.content).unwrap();
    writeln!(out, "----").unwrap();
}

/// Deterministic Aden Graph (ADG) format for CI comparison and compact storage.
/// Same source produces identical bytes (SHA-256 match).
#[derive(Debug, Serialize, Deserialize)]
#[allow(dead_code)]
struct AdgNode {
    anchor: String,
    title: String,
    block_count: usize,
    content_hash: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[allow(dead_code)]
struct AdgEdge {
    source: String,
    target: String,
    edge_type: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AdgDocument {
    anchor: String,
    title: String,
    blocks: Vec<AdgBlock>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum AdgBlock {
    Paragraph { text: String },
    Table { headers: Vec<String>, row_count: usize },
    Listing { language: Option<String>, lines: usize },
    Admonition { kind: String, text: String },
    DescriptionList { item_count: usize },
}

/// Emit a Document in deterministic ADG format (canonical JSON).
/// Use this for CI SHA-256 comparison and compact LLM context.
pub fn emit_adg(doc: &Document) -> Result<String, serde_json::Error> {
    let blocks: Vec<AdgBlock> = doc.blocks.iter().map(|b| match b {
        Block::Paragraph(text) => AdgBlock::Paragraph { text: text.clone() },
        Block::Table(t) => AdgBlock::Table {
            headers: t.headers.clone(),
            row_count: t.rows.len(),
        },
        Block::Listing { language, code } => AdgBlock::Listing {
            language: language.clone(),
            lines: code.lines().count(),
        },
        Block::Admonition { kind, text } => AdgBlock::Admonition {
            kind: format!("{:?}", kind),
            text: text.clone(),
        },
        Block::DescriptionList(items) => AdgBlock::DescriptionList {
            item_count: items.len(),
        },
    }).collect();

    let adoc = AdgDocument {
        anchor: doc.anchor.clone(),
        title: doc.attributes.get("title").cloned().unwrap_or_else(|| doc.anchor.clone()),
        blocks,
    };

    serde_json::to_string(&adoc)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityConstraint {
    CompileTime,
    Runtime,
    TestOnly,
}

impl SecurityConstraint {
    pub fn as_str(&self) -> &'static str {
        match self {
            SecurityConstraint::CompileTime => "compile_time",
            SecurityConstraint::Runtime => "runtime",
            SecurityConstraint::TestOnly => "test_only",
        }
    }
}

pub fn emit_security_block(
    tag: &str,
    constraint: SecurityConstraint,
    pattern: &str,
    description: &str,
) -> String {
    let mut out = String::new();
    writeln!(out, "[[security#{tag}]]").unwrap();
    writeln!(out, "[security#{tag} :constraint: {}]", constraint.as_str()).unwrap();
    writeln!(out, "----").unwrap();
    writeln!(out, ":forbid_import: {}", pattern).unwrap();
    writeln!(out).unwrap();
    writeln!(out, "{}", description).unwrap();
    writeln!(out, "----").unwrap();
    out
}

pub fn emit_security_compile_time(tag: &str, pattern: &str, description: &str) -> String {
    emit_security_block(tag, SecurityConstraint::CompileTime, pattern, description)
}

pub fn emit_security_runtime(tag: &str, pattern: &str, description: &str) -> String {
    emit_security_block(tag, SecurityConstraint::Runtime, pattern, description)
}

pub fn emit_security_test_only(tag: &str, pattern: &str, description: &str) -> String {
    emit_security_block(tag, SecurityConstraint::TestOnly, pattern, description)
}

/// Emit a single Document as GitHub-Flavored Markdown.
pub fn emit_document_md(doc: &Document) -> String {
    let mut out = String::new();

    // Frontmatter-style attributes (optional, for compatibility)
    if !doc.attributes.is_empty() {
        writeln!(out, "---").unwrap();
        for (key, value) in &doc.attributes {
            writeln!(out, "{}: {}", key, value).unwrap();
        }
        writeln!(out, "---").unwrap();
        writeln!(out).unwrap();
    }

    // Anchor as HTML comment
    writeln!(out, "<!-- [[{}]] -->", doc.anchor).unwrap();
    let title = derive_title(doc);
    writeln!(out, "# {title}").unwrap();
    writeln!(out).unwrap();

    // Blocks
    for block in &doc.blocks {
        emit_block_md(&mut out, block);
        writeln!(out).unwrap();
    }

    out
}

/// Emit multiple Documents separated by horizontal rules.
pub fn emit_md(documents: &[Document]) -> String {
    documents
        .iter()
        .map(emit_document_md)
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}

fn emit_block_md(out: &mut String, block: &Block) {
    match block {
        Block::Paragraph(text) => {
            // Convert AsciiDoc cross-references to Markdown links
            let text = convert_xref_to_md_links(text);
            writeln!(out, "{}", text).unwrap();
        }
        Block::Table(table) => {
            emit_table_md(out, table);
        }
        Block::Listing { language, code } => {
            let lang = language.as_deref().unwrap_or("");
            writeln!(out, "```{lang}").unwrap();
            writeln!(out, "{}", code).unwrap();
            writeln!(out, "```").unwrap();
        }
        Block::Admonition { kind, text } => {
            let (emoji, label) = match kind {
                AdmonitionKind::Note => ("📝", "Note"),
                AdmonitionKind::Tip => ("💡", "Tip"),
                AdmonitionKind::Warning => ("⚠️", "Warning"),
                AdmonitionKind::Important => ("🔒", "Important"),
                AdmonitionKind::Caution => ("⛔", "Caution"),
            };
            writeln!(out, "> **{emoji} {label}**: {text}").unwrap();
        }
        Block::DescriptionList(items) => {
            for (term, def) in items {
                writeln!(out, "**{term}**: {def}").unwrap();
            }
        }
    }
}

fn emit_table_md(out: &mut String, table: &Table) {
    // GFM table requires header row
    if table.headers.is_empty() {
        return;
    }

    // Header row
    let header_row: String = table
        .headers
        .iter()
        .map(|h| h.replace('|', "\\|"))
        .collect::<Vec<_>>()
        .join(" | ");
    writeln!(out, "| {}", header_row).unwrap();

    // Separator row
    let sep: String = table
        .headers
        .iter()
        .map(|_| "---")
        .collect::<Vec<_>>()
        .join(" | ");
    writeln!(out, "| {}", sep).unwrap();

    // Data rows
    for row in &table.rows {
        let row_str: String = row
            .iter()
            .map(|c| c.replace('|', "\\|"))
            .collect::<Vec<_>>()
            .join(" | ");
        writeln!(out, "| {}", row_str).unwrap();
    }
}

/// Convert AsciiDoc <<ref>> cross-references to Markdown [ref](#ref) links.
fn convert_xref_to_md_links(text: &str) -> String {
    let mut result = String::new();
    let mut remaining = text;
    while let Some(start) = remaining.find("<<") {
        result.push_str(&remaining[..start]);
        remaining = &remaining[start + 2..];
        if let Some(end) = remaining.find(">>") {
            let reference = &remaining[..end];
            // Check if there's display text: <<reference#display>>
            let (ref_part, display) = if let Some(hash_pos) = reference.find('#') {
                (&reference[..hash_pos], Some(&reference[hash_pos + 1..]))
            } else {
                (reference, None)
            };
            match display {
                Some(d) => result.push_str(&format!("[{}](#{})", d, ref_part)),
                None => result.push_str(&format!("[{}](#{})", ref_part, ref_part)),
            }
            remaining = &remaining[end + 2..];
        } else {
            result.push_str("<<");
            break;
        }
    }
    result.push_str(remaining);
    result
}

/// Emit a ContractDocument (region-aware) as Markdown.
/// Region blocks are rendered as comments to preserve structure.
pub fn emit_contract_document_md(doc: &ContractDocument) -> String {
    let mut out = String::new();

    // Frontmatter attributes
    if !doc.header_attrs.is_empty() {
        writeln!(out, "---").unwrap();
        for (key, value) in &doc.header_attrs {
            writeln!(out, "{}: {}", key, value).unwrap();
        }
        writeln!(out, "---").unwrap();
        writeln!(out).unwrap();
    }

    // Region blocks
    for block in &doc.blocks {
        emit_region_block_md(&mut out, block);
        writeln!(out).unwrap();
    }

    // Prose
    for line in &doc.prose {
        writeln!(out, "{}", line).unwrap();
    }

    out
}

fn emit_region_block_md(out: &mut String, block: &RegionBlock) {
    let region_tag = match &block.tag {
        Some(tag) => format!("{}#{}", block.region, tag),
        None => block.region.to_string(),
    };

    // Write region marker as HTML comment
    write!(out, "<!-- [{}]", region_tag).unwrap();
    if !block.attributes.is_empty() {
        let attrs: Vec<String> = block
            .attributes
            .iter()
            .map(|(k, v)| format!(" :{}: {}", k, v))
            .collect();
        write!(out, "{}", attrs.join("")).unwrap();
    }
    writeln!(out, " -->").unwrap();

    // Content in a code block if it looks like code, otherwise prose
    if block.content.contains('\n') || block.content.len() > 200 {
        writeln!(out, "```").unwrap();
        writeln!(out, "{}", block.content).unwrap();
        writeln!(out, "```").unwrap();
    } else {
        writeln!(out, "{}", block.content).unwrap();
    }

    writeln!(out, "<!-- [/{}] -->", region_tag).unwrap();
}

/// Template variable expansion for generated content.
/// Replaces variables like ${crates}, ${commands} with auto-generated content.
pub fn expand_template_variables(
    template: &str,
    vars: &TemplateVars,
) -> String {
    let mut result = template.to_string();

    // Expand ${crates}
    if result.contains("${crates}") {
        let crates_table = vars.render_crates_table();
        result = result.replace("${crates}", &crates_table);
    }

    // Expand ${commands}
    if result.contains("${commands}") {
        let commands_table = vars.render_commands_table();
        result = result.replace("${commands}", &commands_table);
    }

    // Expand ${modules}
    if result.contains("${modules}") {
        let modules_table = vars.render_modules_table();
        result = result.replace("${modules}", &modules_table);
    }

    result
}

/// Variables for template expansion.
pub struct TemplateVars {
    pub crates: Vec<CrateInfo>,
    pub commands: Vec<CommandInfo>,
    pub modules: Vec<ModuleInfo>,
}

#[derive(Debug, Clone)]
pub struct CrateInfo {
    pub name: String,
    pub responsibility: String,
}

#[derive(Debug, Clone)]
pub struct CommandInfo {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct ModuleInfo {
    pub name: String,
    pub anchor: String,
}

impl Default for TemplateVars {
    fn default() -> Self {
        Self {
            crates: vec![
                CrateInfo { name: "aden-core".to_string(), responsibility: "Schema: Document, Block, Edge, Symbol".to_string() },
                CrateInfo { name: "aden-parse".to_string(), responsibility: "Language routers and AST extraction".to_string() },
                CrateInfo { name: "aden-emit".to_string(), responsibility: "Deterministic AsciiDoc emitter".to_string() },
                CrateInfo { name: "aden-graph".to_string(), responsibility: "Referential integrity: DiGraph, cycle detection".to_string() },
                CrateInfo { name: "aden-asm".to_string(), responsibility: "Context assembly: BFS traversal, token budgeting".to_string() },
                CrateInfo { name: "aden-heal".to_string(), responsibility: "Drift detection and health scoring".to_string() },
                CrateInfo { name: "aden-propose".to_string(), responsibility: "Patch generation and proposal lifecycle".to_string() },
                CrateInfo { name: "aden-cli".to_string(), responsibility: "Binary (aden) with all commands".to_string() },
            ],
            commands: vec![
                CommandInfo { name: "aden gen".to_string(), description: "Parse source and emit contracts".to_string() },
                CommandInfo { name: "aden check".to_string(), description: "Verify all references resolve".to_string() },
                CommandInfo { name: "aden heal".to_string(), description: "Scan for drift and propose fixes".to_string() },
                CommandInfo { name: "aden asm".to_string(), description: "Assemble context prompt".to_string() },
                CommandInfo { name: "aden query".to_string(), description: "Query the knowledge graph".to_string() },
                CommandInfo { name: "aden ask".to_string(), description: "Natural language question to graph".to_string() },
                CommandInfo { name: "aden search".to_string(), description: "Full-text search in contracts".to_string() },
                CommandInfo { name: "aden ci-check".to_string(), description: "Run all local CI gates".to_string() },
            ],
            modules: vec![],
        }
    }
}

impl TemplateVars {
    pub fn render_crates_table(&self) -> String {
        let mut out = String::new();
        out.push_str("| Crate | Responsibility\n");
        out.push_str("|===]\n");
        for CrateInfo { name, responsibility } in &self.crates {
            out.push_str(&format!("| `{}` | {}\n", name, responsibility));
        }
        out.push_str("|===\n");
        out
    }

    pub fn render_commands_table(&self) -> String {
        let mut out = String::new();
        out.push_str("| Command | Description\n");
        out.push_str("|===\n");
        for CommandInfo { name, description } in &self.commands {
            out.push_str(&format!("| `{}` | {}\n", name, description));
        }
        out.push_str("|===\n");
        out
    }

    pub fn render_modules_table(&self) -> String {
        let mut out = String::new();
        out.push_str("| Module | Anchor\n");
        out.push_str("|===\n");
        for ModuleInfo { name, anchor } in &self.modules {
            out.push_str(&format!("| {} | `{}`\n", name, anchor));
        }
        out.push_str("|===\n");
        out
    }
}
