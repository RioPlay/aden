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
use serde::{Deserialize, Serialize};
use std::fmt::Write;

pub mod check;
pub mod templates;

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
