// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

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

    // Pattern 1: crates/crate-name/src/ — Rust workspace layout only.
    // Guard on `.rs` extension: the function receives only the string path with no
    // filesystem access, so we cannot stat a Cargo.toml ancestor.  Checking the
    // extension is the strongest signal available and prevents misfires on Python /
    // Go projects that happen to have a `crates/` directory.
    if path_str.ends_with(".rs")
        && let Ok(re) = Regex::new(r"(?:^|/)crates/([^/]+)/src/")
        && let Some(caps) = re.captures(path_str)
        && let Some(m) = caps.get(1)
    {
        return Some(format!("module-{}", m.as_str()));
    }

    // Pattern 2: src/module-name/ or src/module_name/
    if let Ok(re) = Regex::new(r"(?:^|/)src/([^/]+)")
        && let Some(caps) = re.captures(path_str)
        && let Some(m) = caps.get(1)
    {
        let name = m.as_str();
        // Skip common non-module directories
        if !name.eq_ignore_ascii_case("test")
            && !name.eq_ignore_ascii_case("tests")
            && !name.eq_ignore_ascii_case("bin")
            && !name.eq_ignore_ascii_case("example")
            && !name.eq_ignore_ascii_case("examples")
            && !name.starts_with('.')
        {
            return Some(format!("module-{}", name));
        }
    }

    // Pattern 3: lib/module-name/ (Elixir, Ruby, etc.)
    if let Ok(re) = Regex::new(r"(?:^|/)lib/([^/]+)")
        && let Some(caps) = re.captures(path_str)
        && let Some(m) = caps.get(1)
    {
        return Some(format!("module-{}", m.as_str()));
    }

    // Pattern 4: Root-level module (e.g., src/lib.rs or src/main.rs -> mod-root)
    if let Ok(re) = Regex::new(r"(?:^|/)src/(?:lib|main)\.rs$")
        && re.is_match(path_str)
    {
        return Some("module-root".to_string());
    }

    None
}

#[cfg(test)]
mod contract_roundtrip_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod anchor_heuristic_tests {
    use super::infer_module_anchor;

    /// Pattern 1 must NOT fire on non-Rust files even when the path contains
    /// `crates/<name>/src/`.  A Python file in that layout must fall through to
    /// Pattern 2, never to Pattern 1.
    #[test]
    fn python_path_does_not_match_pattern1() {
        // Before the guard, Pattern 1 would have captured "foo" and returned
        // Some("module-foo").  With the `.rs` extension guard, Pattern 1 is
        // skipped; Pattern 2 captures the segment after `src/` instead.
        let result = infer_module_anchor(Some("crates/foo/src/utils.py"));
        assert_ne!(
            result.as_deref(),
            Some("module-foo"),
            "Pattern 1 must not fire for .py files in a crates/ layout"
        );
    }

    /// Rust file in a workspace layout must still match Pattern 1.
    #[test]
    fn rust_workspace_path_matches_pattern1() {
        let result = infer_module_anchor(Some("crates/aden-emit/src/lib.rs"));
        // Pattern 1 fires first (`.rs` guard passes, `crates/aden-emit/src/` matches).
        assert_eq!(result.as_deref(), Some("module-aden-emit"));
    }

    /// A pure Python path (no `crates/`) must not match Pattern 1.
    #[test]
    fn python_path_outside_crates_does_not_match_pattern1() {
        let result = infer_module_anchor(Some("pkg/foo/utils.py"));
        // Pattern 2 won't match either (no src/), so this should be None.
        assert_ne!(result.as_deref(), Some("module-foo"));
    }
}

/// Emit a single Document as an AsciiDoc string.
pub fn emit_document(doc: &Document) -> String {
    let mut out = String::new();
    // Attributes — sorted by key so the emitted text is byte-stable. `attributes`
    // is a HashMap, so iterating it directly yields a non-deterministic order; the
    // search index ingests this text, and an unstable order makes the index cache
    // (and snippets) differ run-to-run. Sorting makes it reproducible.
    let mut attrs: Vec<_> = doc.attributes.iter().collect();
    attrs.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value) in attrs {
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
        Block::Checklist(items) => {
            for item in items {
                let marker = if item.checked { "[x]" } else { "[ ]" };
                writeln!(out, "* {marker} {}", item.text).unwrap();
            }
        }
        Block::Incomplete {
            required_fields,
            hint,
        } => {
            writeln!(out, "[must-complete]").unwrap();
            writeln!(out, "====").unwrap();
            writeln!(out, "Required fields:").unwrap();
            for field in required_fields {
                writeln!(out, "* {field}").unwrap();
            }
            writeln!(out).unwrap();
            writeln!(out, "Hint: {hint}").unwrap();
            writeln!(out, "====").unwrap();
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
///
/// This is the canonical serializer: `aden_core::contract::parse_contract`
/// is its exact inverse, so the output must contain nothing that is not in
/// the document (derived content like module links belongs in
/// [`emit_contract_document_rendered`]).
pub fn emit_contract_document(doc: &ContractDocument) -> String {
    let mut out = String::new();

    // Header attributes — sorted by key so the emitted text is byte-stable
    // (HashMap iteration order is non-deterministic run-to-run).
    let mut attrs: Vec<_> = doc.header_attrs.iter().collect();
    attrs.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value) in attrs {
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

/// Render a ContractDocument for human consumption: the canonical text plus
/// a derived "See also" link to the module document inferred from the
/// `source_file` header attribute.
///
/// The link is presentation-only and deliberately NOT part of
/// [`emit_contract_document`]: the canonical form must round-trip exactly
/// through `parse_contract`, and a derived line would be read back as prose
/// and accrete on every gen/heal cycle.
pub fn emit_contract_document_rendered(doc: &ContractDocument) -> String {
    let mut out = emit_contract_document(doc);
    if let Some(module_anchor) =
        infer_module_anchor(doc.header_attrs.get("source_file").map(|s| s.as_str()))
    {
        out.push('\n');
        writeln!(out, "See also: <<{module_anchor}>>").unwrap();
    }
    out
}

/// Pick a `-` delimiter no content line collides with: the parser closes a
/// block on an exact (trimmed) match, so a literal `----` line in content
/// forces a longer fence.
fn choose_delimiter(content: &str) -> String {
    let mut len = 4usize;
    loop {
        let candidate = "-".repeat(len);
        if !content.lines().any(|l| l.trim() == candidate) {
            return candidate;
        }
        len += 1;
    }
}

fn emit_region_block(out: &mut String, block: &RegionBlock) {
    let region_tag = match &block.tag {
        Some(tag) => format!("{}#{}", block.region, tag),
        None => block.region.to_string(),
    };

    // Region header: `[region#tag :attr: value ...]` — attributes live
    // INSIDE the brackets, or the parser will not recognize the header.
    write!(out, "[{region_tag}").unwrap();
    if !block.attributes.is_empty() {
        let mut attrs: Vec<String> = block
            .attributes
            .iter()
            .map(|(k, v)| format!(" :{k}: {v}"))
            .collect();
        attrs.sort(); // HashMap order is non-deterministic; sort for byte-stable output
        write!(out, "{}", attrs.join("")).unwrap();
    }
    writeln!(out, "]").unwrap();

    // Delimited block
    let delimiter = choose_delimiter(&block.content);
    writeln!(out, "{delimiter}").unwrap();
    writeln!(out, "{}", block.content).unwrap();
    writeln!(out, "{delimiter}").unwrap();
}

/// Deterministic Aden Graph (ADG) format for CI comparison and compact storage.
/// Same source produces identical bytes (SHA-256 match).
#[derive(Debug, Serialize, Deserialize)]
struct AdgDocument {
    anchor: String,
    title: String,
    blocks: Vec<AdgBlock>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum AdgBlock {
    Paragraph {
        text: String,
    },
    Table {
        headers: Vec<String>,
        row_count: usize,
    },
    Listing {
        language: Option<String>,
        lines: usize,
    },
    Admonition {
        kind: String,
        text: String,
    },
    DescriptionList {
        item_count: usize,
    },
    Checklist {
        item_count: usize,
    },
    Incomplete {
        field_count: usize,
        hint: String,
    },
}

/// Emit a Document in deterministic ADG format (canonical JSON).
/// Use this for CI SHA-256 comparison and compact LLM context.
pub fn emit_adg(doc: &Document) -> Result<String, serde_json::Error> {
    let blocks: Vec<AdgBlock> = doc
        .blocks
        .iter()
        .map(|b| match b {
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
            Block::Checklist(items) => AdgBlock::Checklist {
                item_count: items.len(),
            },
            Block::Incomplete {
                required_fields,
                hint,
            } => AdgBlock::Incomplete {
                field_count: required_fields.len(),
                hint: hint.clone(),
            },
        })
        .collect();

    let adoc = AdgDocument {
        anchor: doc.anchor.clone(),
        title: doc
            .attributes
            .get("title")
            .cloned()
            .unwrap_or_else(|| doc.anchor.clone()),
        blocks,
    };

    serde_json::to_string(&adoc)
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
        Block::Checklist(items) => {
            for item in items {
                let checkbox = if item.checked { "[x]" } else { "[ ]" };
                writeln!(out, "- {checkbox} {}", item.text).unwrap();
            }
        }
        Block::Incomplete {
            required_fields,
            hint,
        } => {
            writeln!(out, "**Required fields:**").unwrap();
            for field in required_fields {
                writeln!(out, "- {field}").unwrap();
            }
            writeln!(out).unwrap();
            writeln!(out, "**Hint:** {hint}").unwrap();
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

/// Convert AsciiDoc cross-references to Markdown links.
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
