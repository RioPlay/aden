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
use aden_core::{AdmonitionKind, Block, Table};
use regex::Regex;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;

static ATTR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^:\s*([^:\s]+)\s*:\s*(.*)$").expect("static regex"));
static ANCHOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[\[([^\]]+)\]\]\s*$").expect("static regex"));
static REF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<<([^>,]+)(?:,[^>]*)?>>").expect("static regex"));
static INCLUDE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^include::([^\[]+)\[(.*)\]\s*$").expect("static regex"));
static IFDEF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^ifdef::([^\[]+)\[\]\s*$").expect("static regex"));
static IFNDEF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^ifndef::([^\[]+)\[\]\s*$").expect("static regex"));
static IFEVAL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^ifeval::\[([^\]]+)\]\s*$").expect("static regex"));
static ENDIF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^endif::\[\]\s*$").expect("static regex"));
static TAG_START_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^//\s*tag=(\w+)$").expect("static regex"));
static TAG_END_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^//\s*end::(\w+)$").expect("static regex"));
static SEMANTIC_DIFF_CHANGED_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^agent-note::CHANGED\[([^\]]+)\]\s*(.*)$").expect("static regex")
});
static SEMANTIC_DIFF_DEPRECATED_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^agent-note::DEPRECATED\[([^\]]+)\]\s*(.*)$").expect("static regex")
});
static SEMANTIC_DIFF_ADDED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^agent-note::ADDED\[([^\]]+)\]\s*$").expect("static regex"));
static TITLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^=+\s+.*$").expect("static regex"));
static SOURCE_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[source(?:,\s*([^\]]+))?\]$").expect("static regex"));
static ADMONITION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(NOTE|TIP|WARNING|IMPORTANT|CAUTION):\s*(.*)$").expect("static regex")
});
static DESC_LIST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?)::\s*(.*)$").expect("static regex"));
static CHECKLIST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\*\s*\[([ xX])\]\s*(.*)$").expect("static regex"));
static CHECKLIST_UNCHECKED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[\s*\]\s*(.*)$").expect("static regex"));
static CHECKLIST_CHECKED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[[ xX]\]\s*(.*)$").expect("static regex"));

/// A document parsed from an AsciiDoc source.
#[derive(Debug, Clone)]
pub struct ParsedDocument {
    pub source_path: String,
    pub attributes: HashMap<String, String>,
    pub anchors: Vec<String>,
    pub refs: Vec<String>,
    pub includes: Vec<Include>,
    pub edges: Vec<EdgeMacro>,
    pub conditional_stack: Vec<Conditional>,
    pub raw_content: String,
    pub semantic_diffs: Vec<SemanticDiff>,
    /// Structured content blocks extracted from the AsciiDoc body.
    pub blocks: Vec<Block>,
    /// Tagged regions for selective include (// tag=name ... // end::name)
    pub tagged_regions: Vec<TaggedRegion>,
    /// Conditional regions (ifdef/ifndef content)
    pub conditional_regions: Vec<ConditionalRegion>,
    /// Document-level metadata.
    pub metadata: Option<aden_core::DocumentMetadata>,
}

/// An `include::path[attributes]` directive.
#[derive(Debug, Clone)]
pub struct Include {
    pub path: String,
    pub tags: Option<String>,
    pub lines: Option<String>,
    pub leveloffset: Option<i32>,
}

/// A custom `edge::type[...]` macro.
#[derive(Debug, Clone)]
pub struct EdgeMacro {
    pub edge_type: String,
    pub target: String,
}

/// A semantic diff entry parsed from agent-note macros.
#[derive(Debug, Clone)]
pub enum SemanticDiff {
    Changed {
        date: String,
        description: String,
    },
    Deprecated {
        date: String,
        replacement: Option<String>,
    },
    Added {
        date: String,
    },
}

/// A conditional block.
#[derive(Debug, Clone)]
pub enum Conditional {
    Ifdef { attr: String, active: bool },
    Ifndef { attr: String, active: bool },
    Ifeval { expr: String, active: bool },
}

/// A conditional region with content (ifdef/ifndef blocks)
#[derive(Debug, Clone)]
pub struct ConditionalRegion {
    pub attribute: String,
    pub content: String,
    pub is_active: bool,
    pub line_start: usize,
    pub line_end: usize,
}

/// A tagged region for selective include (// tag=name ... // end::name)
#[derive(Debug, Clone)]
pub struct TaggedRegion {
    pub tag_name: String,
    pub content: String,
    pub line_start: usize,
    pub line_end: usize,
}

/// Errors during AsciiDoc parsing.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("IO error: {0}")]
    Io(String),
}

/// Structured block parser state.
#[derive(Debug, Clone, Copy, PartialEq)]
enum BlockState {
    Idle,
    InTable,
    InListing,
    InParagraph,
}

/// Parse an AsciiDoc file into a `ParsedDocument`.
pub fn parse_file(path: &Path) -> Result<ParsedDocument, ParseError> {
    let mut file = std::fs::File::open(path).map_err(|e| ParseError::Io(e.to_string()))?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .map_err(|e| ParseError::Io(e.to_string()))?;

    let mut attrs = HashMap::new();
    let mut anchors = Vec::new();
    let mut refs = Vec::new();
    let mut includes = Vec::new();
    let mut edges = Vec::new();
    let mut conditional_stack = Vec::new();
    let mut semantic_diffs = Vec::new();
    let mut blocks: Vec<Block> = Vec::new();
    let mut tagged_regions: Vec<TaggedRegion> = Vec::new();
    let mut active_tags: Vec<String> = Vec::new();
    let mut current_tag_content: Vec<String> = Vec::new();
    let mut tag_start_line: usize = 0;
    let mut conditional_regions: Vec<ConditionalRegion> = Vec::new();
    let mut active_conditional_attrs: Vec<String> = Vec::new();
    let mut current_conditional_content: Vec<String> = Vec::new();
    let mut conditional_start_line: usize = 0;

    let mut state = BlockState::Idle;
    let mut table_headers: Vec<String> = Vec::new();
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut listing_code = String::new();
    let mut listing_lang: Option<String> = None;
    let mut paragraph_text = String::new();
    let mut saw_table_delim = false; // true after first |=== encountered inside table
    let mut in_header = true;
    let mut last_line_was_source_directive = false;
    let mut line_number: usize = 0;

    for line in raw.lines() {
        line_number += 1;
        let trimmed = line.trim();

        // Skip comments (but not doc comments)
        let is_comment = trimmed.starts_with("//") && !trimmed.starts_with("///");

        // End of header block when we hit a blank line after attributes
        if in_header && trimmed.is_empty() && !attrs.is_empty() {
            in_header = false;
        }

        // ── Metadata extraction (always run in parallel) ──────────────────

        // Attributes
        if (in_header || trimmed.starts_with(':'))
            && let Some(cap) = ATTR_RE.captures(trimmed)
        {
            let key = cap[1].to_string();
            let value = cap[2].to_string();
            attrs.insert(key, value);
            // Attributes aren't body content
            if state == BlockState::InParagraph {
                flush_paragraph(&mut paragraph_text, &mut blocks);
                state = BlockState::Idle;
            }
            continue;
        }

        // Anchors
        if let Some(cap) = ANCHOR_RE.captures(trimmed) {
            anchors.push(cap[1].to_string());
            if state == BlockState::InParagraph {
                flush_paragraph(&mut paragraph_text, &mut blocks);
                state = BlockState::Idle;
            }
            continue;
        }

        // Source directive
        if let Some(cap) = SOURCE_BLOCK_RE.captures(trimmed) {
            listing_lang = cap.get(1).map(|m| m.as_str().trim().to_string());
            last_line_was_source_directive = true;
            if state == BlockState::InParagraph {
                flush_paragraph(&mut paragraph_text, &mut blocks);
                state = BlockState::Idle;
            }
            continue;
        }

        // Includes
        if let Some(cap) = INCLUDE_RE.captures(trimmed) {
            let path = cap[1].to_string();
            let attrs_str = &cap[2];
            let mut tag = None;
            let mut lines_spec = None;
            let mut leveloff = None;
            for part in attrs_str.split(';') {
                let part = part.trim();
                if let Some(val) = part.strip_prefix("tags=") {
                    tag = Some(val.trim_matches('"').to_string());
                } else if let Some(val) = part.strip_prefix("lines=") {
                    lines_spec = Some(val.trim_matches('"').to_string());
                } else if let Some(val) = part.strip_prefix("leveloffset=")
                    && let Ok(v) = val.trim_matches('"').parse::<i32>()
                {
                    leveloff = Some(v);
                }
            }
            includes.push(Include {
                path,
                tags: tag,
                lines: lines_spec,
                leveloffset: leveloff,
            });
            if state == BlockState::InParagraph {
                flush_paragraph(&mut paragraph_text, &mut blocks);
                state = BlockState::Idle;
            }
            last_line_was_source_directive = false;
            continue;
        }

        // Conditionals
        if let Some(cap) = IFDEF_RE.captures(trimmed) {
            let attr = cap[1].to_string();
            let active = attrs.contains_key(&attr);
            conditional_stack.push(Conditional::Ifdef { attr: attr.clone(), active });
            active_conditional_attrs.push(attr);
            current_conditional_content.clear();
            conditional_start_line = line_number;
            if state == BlockState::InParagraph {
                flush_paragraph(&mut paragraph_text, &mut blocks);
                state = BlockState::Idle;
            }
            last_line_was_source_directive = false;
            continue;
        }
        if let Some(cap) = IFNDEF_RE.captures(trimmed) {
            let attr = cap[1].to_string();
            let active = !attrs.contains_key(&attr);
            conditional_stack.push(Conditional::Ifndef { attr: attr.clone(), active });
            active_conditional_attrs.push(attr);
            current_conditional_content.clear();
            conditional_start_line = line_number;
            if state == BlockState::InParagraph {
                flush_paragraph(&mut paragraph_text, &mut blocks);
                state = BlockState::Idle;
            }
            last_line_was_source_directive = false;
            continue;
        }
        if let Some(cap) = IFEVAL_RE.captures(trimmed) {
            let expr = cap[1].to_string();
            let active = eval_ifeval(&expr, &attrs);
            conditional_stack.push(Conditional::Ifeval { expr, active });
            // For ifeval, we don't track content the same way
            if state == BlockState::InParagraph {
                flush_paragraph(&mut paragraph_text, &mut blocks);
                state = BlockState::Idle;
            }
            last_line_was_source_directive = false;
            continue;
        }
        if ENDIF_RE.is_match(trimmed) {
            if !active_conditional_attrs.is_empty() {
                let attr = active_conditional_attrs.pop();
                let is_active = conditional_stack.iter().last()
                    .map(|c| match c {
                        Conditional::Ifdef { active, .. } => *active,
                        Conditional::Ifndef { active, .. } => *active,
                        Conditional::Ifeval { active, .. } => *active,
                    })
                    .unwrap_or(false);
                if !current_conditional_content.is_empty() {
                    conditional_regions.push(ConditionalRegion {
                        attribute: attr.unwrap_or_default(),
                        content: current_conditional_content.join("\n"),
                        is_active,
                        line_start: conditional_start_line,
                        line_end: line_number,
                    });
                    current_conditional_content.clear();
                }
            }
            conditional_stack.pop();
            if state == BlockState::InParagraph {
                flush_paragraph(&mut paragraph_text, &mut blocks);
                state = BlockState::Idle;
            }
            last_line_was_source_directive = false;
            continue;
        }

        // Track content inside active conditionals
        if !active_conditional_attrs.is_empty() && !trimmed.is_empty() {
            current_conditional_content.push(line.to_string());
        }

        // Tagged regions (// tag=name and // end::name)
        if let Some(cap) = TAG_START_RE.captures(trimmed) {
            let tag_name = cap[1].to_string();
            active_tags.push(tag_name.clone());
            current_tag_content.clear();
            tag_start_line = line_number;
            continue;
        }
        if let Some(cap) = TAG_END_RE.captures(trimmed) {
            let tag_name = cap[1].to_string();
            if let Some(active_tag) = active_tags.last()
                && active_tag == &tag_name
            {
                    let content = current_tag_content.join("\n");
                    tagged_regions.push(TaggedRegion {
                        tag_name: tag_name.clone(),
                        content,
                        line_start: tag_start_line,
                        line_end: line_number,
                    });
                    active_tags.pop();
                    current_tag_content.clear();
            }
            continue;
        }
        // Track content inside tags
        if !active_tags.is_empty() && !is_comment && !trimmed.is_empty() {
            current_tag_content.push(line.to_string());
        }

        // Title lines are structural, not body content
        if TITLE_RE.is_match(trimmed) {
            if state == BlockState::InParagraph {
                flush_paragraph(&mut paragraph_text, &mut blocks);
                state = BlockState::Idle;
            }
            last_line_was_source_directive = false;
            continue;
        }

        // References (xrefs) - only if not inside a backtick-quoted span
        let mut refs_on_line: Vec<String> = Vec::new();
        for cap in REF_RE.captures_iter(line) {
            let m = cap.get(0).expect("regex group 0 always exists for a match");
            // Check if this match is inside backticks
            let prefix = &line[..m.start()];
            let backtick_count = prefix.matches('`').count();
            if backtick_count % 2 == 0 {
                let ref_text = cap[1].trim().to_string();
                if !ref_text.is_empty() && !ref_text.contains(' ') {
                    refs_on_line.push(ref_text);
                }
            }
        }
        refs.extend(refs_on_line);

        // edge:: macros
        if trimmed.starts_with("edge::")
            && let Some(end) = trimmed.find('[')
        {
            let edge_type = trimmed[6..end].to_string();
            let rest = &trimmed[end + 1..];
            if let Some(close) = rest.rfind(']') {
                let target = rest[..close].to_string();
                edges.push(EdgeMacro { edge_type, target });
            }
        }

        // Semantic diffs
        if let Some(cap) = SEMANTIC_DIFF_CHANGED_RE.captures(trimmed) {
            let date = cap[1].to_string();
            let description = cap[2].to_string();
            semantic_diffs.push(SemanticDiff::Changed { date, description });
        } else if let Some(cap) = SEMANTIC_DIFF_DEPRECATED_RE.captures(trimmed) {
            let date = cap[1].to_string();
            let replacement = cap[2].to_string();
            let replacement = if replacement.is_empty() {
                None
            } else {
                Some(replacement)
            };
            semantic_diffs.push(SemanticDiff::Deprecated { date, replacement });
        } else if let Some(cap) = SEMANTIC_DIFF_ADDED_RE.captures(trimmed) {
            let date = cap[1].to_string();
            semantic_diffs.push(SemanticDiff::Added { date });
        }

        // Skip pure comment lines for body content
        if is_comment {
            if state == BlockState::InParagraph {
                flush_paragraph(&mut paragraph_text, &mut blocks);
                state = BlockState::Idle;
            }
            last_line_was_source_directive = false;
            continue;
        }

        // ── Block-level body content parsing ──────────────────────────────

        match state {
            BlockState::Idle => {
                if trimmed == "|===" {
                    state = BlockState::InTable;
                    table_headers.clear();
                    table_rows.clear();
                    saw_table_delim = false;
                } else if trimmed == "----" {
                    state = BlockState::InListing;
                    listing_code.clear();
                    if !last_line_was_source_directive {
                        listing_lang = None;
                    }
                } else if let Some(cap) = ADMONITION_RE.captures(trimmed) {
                    let kind = match &cap[1] {
                        "NOTE" => AdmonitionKind::Note,
                        "TIP" => AdmonitionKind::Tip,
                        "WARNING" => AdmonitionKind::Warning,
                        "IMPORTANT" => AdmonitionKind::Important,
                        "CAUTION" => AdmonitionKind::Caution,
                        _ => AdmonitionKind::Note,
                    };
                    let text = cap[2].to_string();
                    blocks.push(Block::Admonition { kind, text });
                } else if let Some(cap) = DESC_LIST_RE.captures(trimmed) {
                    let term = cap[1].trim().to_string();
                    let def = cap[2].trim().to_string();
                    blocks.push(Block::DescriptionList(vec![(term, def)]));
                } else if let Some(cap) = CHECKLIST_RE.captures(trimmed) {
                    let checked = !cap[1].trim().is_empty();
                    let text = cap[2].trim().to_string();
                    blocks.push(Block::Checklist(vec![aden_core::ChecklistItem { checked, text }]));
                } else if let Some(cap) = CHECKLIST_CHECKED_RE.captures(trimmed) {
                    let text = cap[1].trim().to_string();
                    blocks.push(Block::Checklist(vec![aden_core::ChecklistItem { checked: true, text }]));
                } else if let Some(cap) = CHECKLIST_UNCHECKED_RE.captures(trimmed) {
                    let text = cap[1].trim().to_string();
                    blocks.push(Block::Checklist(vec![aden_core::ChecklistItem { checked: false, text }]));
                } else if !trimmed.is_empty() {
                    // Start a paragraph
                    paragraph_text.clear();
                    paragraph_text.push_str(trimmed);
                    state = BlockState::InParagraph;
                }
                last_line_was_source_directive = false;
            }

            BlockState::InTable => {
                if trimmed == "|===" {
                    // End table
                    blocks.push(Block::Table(Table {
                        headers: table_headers.clone(),
                        rows: table_rows.clone(),
                    }));
                    state = BlockState::Idle;
                } else if let Some(after_pipe) = trimmed.strip_prefix('|') {
                    let cells: Vec<String> = after_pipe
                        .split('|')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !cells.is_empty() {
                        if !saw_table_delim {
                            // First row after opening delim is headers
                            table_headers = cells;
                            saw_table_delim = true;
                        } else {
                            table_rows.push(cells);
                        }
                    }
                }
                // Skip empty lines inside table
                last_line_was_source_directive = false;
            }

            BlockState::InListing => {
                if trimmed == "----" {
                    blocks.push(Block::Listing {
                        language: listing_lang.clone(),
                        code: listing_code.trim_end_matches('\n').to_string(),
                    });
                    state = BlockState::Idle;
                    listing_lang = None;
                } else {
                    listing_code.push_str(line);
                    listing_code.push('\n');
                }
                last_line_was_source_directive = false;
            }

            BlockState::InParagraph => {
                if trimmed.is_empty() {
                    flush_paragraph(&mut paragraph_text, &mut blocks);
                    state = BlockState::Idle;
                } else if trimmed == "|===" {
                    // Table starts, flush paragraph first
                    flush_paragraph(&mut paragraph_text, &mut blocks);
                    state = BlockState::InTable;
                    table_headers.clear();
                    table_rows.clear();
                    saw_table_delim = false;
                } else if trimmed == "----" {
                    flush_paragraph(&mut paragraph_text, &mut blocks);
                    state = BlockState::InListing;
                    listing_code.clear();
                    if !last_line_was_source_directive {
                        listing_lang = None;
                    }
                } else if let Some(cap) = ADMONITION_RE.captures(trimmed) {
                    flush_paragraph(&mut paragraph_text, &mut blocks);
                    let kind = match &cap[1] {
                        "NOTE" => AdmonitionKind::Note,
                        "TIP" => AdmonitionKind::Tip,
                        "WARNING" => AdmonitionKind::Warning,
                        "IMPORTANT" => AdmonitionKind::Important,
                        "CAUTION" => AdmonitionKind::Caution,
                        _ => AdmonitionKind::Note,
                    };
                    let text = cap[2].to_string();
                    blocks.push(Block::Admonition { kind, text });
                    state = BlockState::Idle;
                } else if let Some(cap) = DESC_LIST_RE.captures(trimmed) {
                    flush_paragraph(&mut paragraph_text, &mut blocks);
                    let term = cap[1].trim().to_string();
                    let def = cap[2].trim().to_string();
                    blocks.push(Block::DescriptionList(vec![(term, def)]));
                    state = BlockState::Idle;
                } else {
                    paragraph_text.push('\n');
                    paragraph_text.push_str(trimmed);
                }
                last_line_was_source_directive = false;
            }
        }
    }

    // Flush any trailing block
    match state {
        BlockState::InParagraph => flush_paragraph(&mut paragraph_text, &mut blocks),
        BlockState::InTable => {
            blocks.push(Block::Table(Table {
                headers: table_headers,
                rows: table_rows,
            }));
        }
        BlockState::InListing => {
            blocks.push(Block::Listing {
                language: listing_lang,
                code: listing_code.trim_end_matches('\n').to_string(),
            });
        }
        BlockState::Idle => {}
    }

    // If no anchors found, generate one from filename
    if anchors.is_empty()
        && let Some(stem) = path.file_stem()
    {
        anchors.push(stem.to_string_lossy().to_string());
    }

    let metadata = extract_metadata(&attrs);
    Ok(ParsedDocument {
        source_path: path.to_string_lossy().to_string(),
        attributes: attrs,
        anchors,
        refs,
        includes,
        edges,
        conditional_stack,
        raw_content: raw,
        semantic_diffs,
        blocks,
        tagged_regions,
        conditional_regions,
        metadata,
    })
}

/// Extract document metadata from attributes.
fn extract_metadata(attrs: &HashMap<String, String>) -> Option<aden_core::DocumentMetadata> {
    let has_metadata = attrs.contains_key("author")
        || attrs.contains_key("email")
        || attrs.contains_key("revision")
        || attrs.contains_key("version")
        || attrs.contains_key("date")
        || attrs.contains_key("copyright")
        || attrs.contains_key("license");

    if has_metadata {
        Some(aden_core::DocumentMetadata {
            author: attrs.get("author").cloned(),
            email: attrs.get("email").cloned(),
            revision: attrs.get("revision").cloned(),
            version: attrs.get("version").cloned(),
            date: attrs.get("date").cloned(),
            copyright: attrs.get("copyright").cloned(),
            license: attrs.get("license").cloned(),
        })
    } else {
        None
    }
}

/// Flush accumulated paragraph text into a Block::Paragraph.
fn flush_paragraph(text: &mut String, blocks: &mut Vec<Block>) {
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        blocks.push(Block::Paragraph(trimmed.to_string()));
    }
    text.clear();
}

/// Evaluate an `ifeval` expression.
/// Supported forms:
/// - `{attr} == value`
/// - `{attr} != value`
/// - `{attr} < value` (numeric)
/// - `{attr} > value` (numeric)
/// - `{attr} <= value` (numeric)
/// - `{attr} >= value` (numeric)
fn eval_ifeval(expr: &str, attrs: &HashMap<String, String>) -> bool {
    let trimmed = expr.trim();
    // Find the operator
    let operators = ["<=", ">=", "==", "!=", "<", ">"];
    for op in &operators {
        if let Some(pos) = trimmed.find(op) {
            let left = trimmed[..pos].trim();
            let right = trimmed[pos + op.len()..].trim();
            // Resolve attribute on left side
            let left_val = if left.starts_with('{') && left.ends_with('}') {
                let attr_name = &left[1..left.len() - 1];
                attrs.get(attr_name).map(|s| s.as_str()).unwrap_or("")
            } else {
                left
            };
            let right = right.trim_matches('"');
            return match *op {
                "==" => left_val == right,
                "!=" => left_val != right,
                "<" | ">" | "<=" | ">=" => {
                    let l_num = left_val.parse::<f64>();
                    let r_num = right.parse::<f64>();
                    match (l_num, r_num) {
                        (Ok(l), Ok(r)) => match *op {
                            "<" => l < r,
                            ">" => l > r,
                            "<=" => l <= r,
                            ">=" => l >= r,
                            _ => false,
                        },
                        _ => false,
                    }
                }
                _ => false,
            };
        }
    }
    false
}
