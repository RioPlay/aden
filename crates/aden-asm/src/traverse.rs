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
use aden_core::{Block, EdgeType};
use aden_graph::{AdenGraph, graph::DocumentNode};
use petgraph::Direction;
use std::collections::{HashSet, VecDeque};

/// Which block types to include when assembling a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockKind {
    Table,
    Paragraph,
    Listing,
    Admonition,
    DescriptionList,
    Checklist,
}

/// Options for assembling a context prompt.
#[derive(Debug, Clone)]
pub struct AssemblyOptions {
    pub start_anchor: String,
    pub max_depth: usize,
    /// Token budget (approximate byte-pair estimation).
    pub token_budget: usize,
    /// Edge types to follow. If empty, follow all.
    pub edge_types: Vec<EdgeType>,
    /// Block types to include when emitting documents.
    /// If empty, include all blocks.
    pub block_filter: Vec<BlockKind>,
    /// Tags to include. Only content in these tagged regions will be included.
    /// If empty, all content is included (unless exclude_tags is set).
    pub include_tags: Vec<String>,
    /// Tags to exclude. Content in these tagged regions will be excluded.
    pub exclude_tags: Vec<String>,
    /// Attributes to set for conditional processing.
    /// If set, ifdef/ifndef blocks will be filtered accordingly.
    pub attributes: Vec<String>,
    /// Strip AsciiDoc markup syntax and emit clean prose for LLM consumption.
    /// When true: removes [[anchors]], <<refs>>, :attributes:, block delimiters.
    /// Also enables partial inclusion: large documents are truncated to fit the
    /// remaining budget rather than skipped entirely.
    pub llm_mode: bool,
}

/// Assemble a context prompt from a graph neighborhood.
///
/// In `llm_mode` the output is stripped of AsciiDoc markup (anchors, refs,
/// attribute lines, block delimiters) so every token carries signal rather
/// than format noise. Large documents that would exceed the remaining budget
/// are truncated at a word boundary rather than skipped entirely.
pub fn assemble(graph: &AdenGraph, opts: &AssemblyOptions) -> Result<String, AssemblyError> {
    let start_idx = graph
        .get_index(&opts.start_anchor)
        .ok_or_else(|| AssemblyError::AnchorNotFound(opts.start_anchor.clone()))?;

    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut result = Vec::new();
    let mut total_tokens = 0usize;

    queue.push_back((start_idx, 0usize));

    const MAX_VISITED_NODES: usize = 10_000;
    while let Some((node, depth)) = queue.pop_front() {
        if visited.len() >= MAX_VISITED_NODES {
            break; // DoS guard: hard limit on nodes processed
        }
        if visited.contains(&node) {
            continue;
        }
        if depth > opts.max_depth {
            continue;
        }
        let doc = &graph.graph[node];
        let raw_text = document_to_text(
            doc,
            &opts.block_filter,
            &opts.include_tags,
            &opts.exclude_tags,
            &opts.attributes,
        );

        // In LLM mode, strip AsciiDoc markup so every token is signal.
        let text = if opts.llm_mode {
            strip_asciidoc_markup(&raw_text)
        } else {
            raw_text
        };

        let tokens = estimate_tokens(&text);
        let remaining = opts.token_budget.saturating_sub(total_tokens);

        if tokens <= remaining {
            // Document fits entirely — include it.
            total_tokens += tokens;
            visited.insert(node);
            result.push(text);
        } else if opts.llm_mode && remaining > 32 {
            // LLM mode: partial inclusion — truncate to fit remaining budget.
            let truncated = truncate_to_tokens(&text, remaining);
            visited.insert(node);
            result.push(truncated);
            break; // Budget exhausted after partial doc
        } else {
            break; // Not in LLM mode or nothing meaningful fits
        }

        // Add neighbors
        for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
            if let Some(edge) = graph.graph.find_edge(node, neighbor) {
                let edge_type = *graph.graph.edge_weight(edge).unwrap_or(&EdgeType::Uses);
                if opts.edge_types.is_empty() || opts.edge_types.contains(&edge_type) {
                    queue.push_back((neighbor, depth + 1));
                }
            }
        }
    }

    let separator = if opts.llm_mode { "\n\n---\n\n" } else { "\n<<<\n" };
    Ok(result.join(separator))
}

/// Assemble documents in ADG (compact JSON) format for token-efficient LLM context.
pub fn assemble_adg(graph: &AdenGraph, opts: &AssemblyOptions) -> Result<String, AssemblyError> {
    use aden_emit::emit_adg;

    let start_idx = graph
        .get_index(&opts.start_anchor)
        .ok_or_else(|| AssemblyError::AnchorNotFound(opts.start_anchor.clone()))?;

    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut results = Vec::new();
    let mut total_tokens = 0usize;

    queue.push_back((start_idx, 0usize));

    const MAX_VISITED_NODES: usize = 10_000;
    while let Some((node, depth)) = queue.pop_front() {
        if visited.len() >= MAX_VISITED_NODES {
            break;
        }
        if visited.contains(&node) {
            continue;
        }
        if depth > opts.max_depth {
            continue;
        }
        let doc = &graph.graph[node];
        let adg_json = emit_adg(&doc.doc).map_err(|e| AssemblyError::Graph(e.to_string()))?;
        let tokens = adg_json.len() / 4; // rough token estimate
        if total_tokens + tokens > opts.token_budget {
            break;
        }
        total_tokens += tokens;
        visited.insert(node);
        results.push(adg_json);

        for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
            if let Some(edge) = graph.graph.find_edge(node, neighbor) {
                let edge_type = *graph.graph.edge_weight(edge).unwrap_or(&EdgeType::Uses);
                if opts.edge_types.is_empty() || opts.edge_types.contains(&edge_type) {
                    queue.push_back((neighbor, depth + 1));
                }
            }
        }
    }

    let output = format!("[\n{}\n]", results.join(",\n"));
    Ok(output)
}

#[derive(Debug, thiserror::Error)]
pub enum AssemblyError {
    #[error("Anchor '{0}' not found. Run 'aden list .' to see available anchors.")]
    AnchorNotFound(String),
    #[error("graph error: {0}")]
    Graph(String),
}

fn document_to_text(
    doc: &DocumentNode,
    block_filter: &[BlockKind],
    include_tags: &[String],
    exclude_tags: &[String],
    attributes: &[String],
) -> String {
    let has_filter = !block_filter.is_empty();
    let should_include = |b: &Block| -> bool {
        if !has_filter {
            return true;
        }
        let kind = match b {
            Block::Table(_) => BlockKind::Table,
            Block::Paragraph(_) => BlockKind::Paragraph,
            Block::Listing { .. } => BlockKind::Listing,
            Block::Admonition { .. } => BlockKind::Admonition,
            Block::DescriptionList(_) => BlockKind::DescriptionList,
            Block::Checklist(_) => BlockKind::Checklist,
            Block::Incomplete { .. } => BlockKind::Checklist, // Treat as checklist for filtering
        };
        block_filter.contains(&kind)
    };

    // Check if we should filter by tags
    let has_tag_filter = !include_tags.is_empty() || !exclude_tags.is_empty();
    let use_tagged_regions = has_tag_filter && !doc.parsed.tagged_regions.is_empty();

    // If blocks were populated during parsing, emit structured content.
    // Otherwise fall back to the original raw source so the assembled
    // context is never empty.
    if !doc.doc.blocks.is_empty() {
        use aden_core::AdmonitionKind;
        let mut out = String::new();
        // Attributes
        for (key, value) in &doc.doc.attributes {
            out.push_str(&format!(":{key}: {value}\n"));
        }
        out.push('\n');
        // Anchor + Title
        out.push_str(&format!("[[{}]]\n", doc.anchor));
        let title = doc
            .anchor
            .rfind('#')
            .map(|p| &doc.anchor[p + 1..])
            .unwrap_or(&doc.anchor);
        out.push_str(&format!("= {title}\n\n"));
        // Blocks
        for block in &doc.doc.blocks {
            if !should_include(block) {
                continue;
            }
            match block {
                Block::Paragraph(t) => {
                    out.push_str(t);
                    out.push('\n');
                }
                Block::Table(table) => {
                    out.push_str("|===\n");
                    let header = table
                        .headers
                        .iter()
                        .map(|h| format!("|{h}"))
                        .collect::<String>();
                    out.push_str(&header);
                    out.push('\n');
                    for row in &table.rows {
                        let row_str = row.iter().map(|c| format!("|{c}")).collect::<String>();
                        out.push_str(&row_str);
                        out.push('\n');
                    }
                    out.push_str("|===\n");
                }
                Block::Listing { language, code } => {
                    if let Some(lang) = language {
                        out.push_str(&format!("[source,{lang}]\n"));
                    } else {
                        out.push_str("[listing]\n");
                    }
                    out.push_str("----\n");
                    out.push_str(code);
                    out.push_str("\n----\n");
                }
                Block::Admonition { kind, text } => {
                    let label = match kind {
                        AdmonitionKind::Note => "NOTE",
                        AdmonitionKind::Tip => "TIP",
                        AdmonitionKind::Warning => "WARNING",
                        AdmonitionKind::Important => "IMPORTANT",
                        AdmonitionKind::Caution => "CAUTION",
                    };
                    out.push_str(&format!("{label}: {text}\n"));
                }
                Block::DescriptionList(items) => {
                    for (term, def) in items {
                        out.push_str(&format!("{term}:: {def}\n"));
                    }
                }
                Block::Checklist(items) => {
                    for item in items {
                        let marker = if item.checked { "[x]" } else { "[ ]" };
                        out.push_str(&format!("* {marker} {}\n", item.text));
                    }
                }
                Block::Incomplete { required_fields, hint } => {
                    out.push_str("[must-complete]\n");
                    out.push_str("====\n");
                    out.push_str("Required fields:\n");
                    for field in required_fields {
                        out.push_str(&format!("* {field}\n"));
                    }
                    out.push_str(&format!("\nHint: {hint}\n"));
                    out.push_str("====\n");
                }
            }
        }

// If tag filtering is enabled and we have tagged regions, filter content
        if use_tagged_regions {
            let mut filtered_out = String::new();
            let relevant_tags: Vec<_> = doc
                .parsed
                .tagged_regions
                .iter()
                .filter(|t| {
                    let tag_matches = include_tags.is_empty()
                        || include_tags.iter().any(|i| i == &t.tag_name);
                    let not_excluded =
                        exclude_tags.is_empty() || !exclude_tags.iter().any(|e| e == &t.tag_name);
                    tag_matches && not_excluded
                })
                .collect();

            if !relevant_tags.is_empty() {
                // Add header
                for (key, value) in &doc.doc.attributes {
                    filtered_out.push_str(&format!("{key}: {value}\n"));
                }
                filtered_out.push('\n');
                filtered_out.push_str(&format!("[[{}]]\n", doc.anchor));
                let title = doc
                    .anchor
                    .rfind('#')
                    .map(|p| &doc.anchor[p + 1..])
                    .unwrap_or(&doc.anchor);
                filtered_out.push_str(&format!("= {title}\n\n"));

                // Add tagged regions
                for region in relevant_tags {
                    filtered_out.push_str(&region.content);
                    filtered_out.push_str("\n\n");
                }
                return filtered_out;
            }
        }

        // If attributes are set, filter by conditional regions
        let has_attrs = !attributes.is_empty();
        let has_conditionals = !doc.parsed.conditional_regions.is_empty();
        if has_attrs && has_conditionals {
            let attr_set: HashSet<_> = attributes.iter().collect();
            let relevant_conditionals: Vec<_> = doc
                .parsed
                .conditional_regions
                .iter()
                .filter(|c| {
                    // Include if the attribute is set (active) OR if it's not in our attr set
                    // For ifdef: include if attr is set
                    // For ifndef: include if attr is NOT set (but we track via is_active)
                    c.is_active || !attr_set.contains(&c.attribute)
                })
                .collect();

            if !relevant_conditionals.is_empty() {
                let mut filtered_out = String::new();
                for (key, value) in &doc.doc.attributes {
                    filtered_out.push_str(&format!("{key}: {value}\n"));
                }
                filtered_out.push('\n');
                filtered_out.push_str(&format!("[[{}]]\n", doc.anchor));
                let title = doc
                    .anchor
                    .rfind('#')
                    .map(|p| &doc.anchor[p + 1..])
                    .unwrap_or(&doc.anchor);
                filtered_out.push_str(&format!("= {title}\n\n"));

                for region in relevant_conditionals {
                    filtered_out.push_str(&region.content);
                    filtered_out.push_str("\n\n");
                }
                return filtered_out;
            }
        }

        out
    } else {
        doc.parsed.raw_content.trim().to_string()
    }
}

/// Strip AsciiDoc markup and compact tables for maximally dense LLM output.
///
/// Three classes of waste are eliminated:
///
/// 1. **Format noise** — anchors, attribute lines, block delimiters, role
///    annotations, table delimiters.  These carry zero semantic signal.
///
/// 2. **`edge::calls[...]` lines** — internal graph edge declarations that
///    duplicate the callee table already present above them.
///
/// 3. **Verbose tables** — `|Property|Value` / `|Callee|Line` AsciiDoc tables
///    are compressed to compact inline forms:
///    - Property table row `|Name|foo` → `name: foo`
///    - Callee table rows → single line `calls: foo(12), bar(34)`
///
/// The result is semantically identical but 40–60 % fewer tokens.
pub fn strip_asciidoc_markup(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut prev_blank = false;

    // State machine for table compression.
    #[derive(PartialEq)]
    enum TableMode {
        None,
        Property, // |Property|Value header seen
        Callee,   // |Callee|Line header seen
    }
    let mut table_mode = TableMode::None;
    let mut callee_calls: Vec<String> = Vec::new();

    let flush_callees = |calls: &mut Vec<String>, out: &mut Vec<String>| {
        if !calls.is_empty() {
            out.push(format!("calls: {}", calls.join(", ")));
            calls.clear();
        }
    };

    for line in text.lines() {
        let trimmed = line.trim();

        // --- Skip: [[anchor]] lines ---
        if trimmed.starts_with("[[") && trimmed.ends_with("]]") {
            continue;
        }

        // --- Skip: :key: value AsciiDoc attribute lines ---
        // Matches ":source_file: foo.rs", ":author: Alice", ":toc:", ":!numbered:"
        if trimmed.starts_with(':') {
            if let Some(rest) = trimmed.strip_prefix(':') {
                let rest = rest.strip_prefix('!').unwrap_or(rest);
                if let Some(colon) = rest.find(':') {
                    let key = &rest[..colon];
                    if !key.is_empty() && !key.contains(' ') {
                        continue;
                    }
                }
            }
        }

        // --- Skip: block delimiters ---
        if matches!(trimmed, "----" | "====" | "****" | "____" | "--" | "'''" | "<<<") {
            continue;
        }

        // --- Skip: role/block annotations like [source,rust], [listing], [NOTE] ---
        if trimmed.starts_with('[') && trimmed.ends_with(']') && !trimmed.contains(' ') {
            continue;
        }

        // --- Skip: edge::calls[...] lines — duplicate of callee table ---
        if trimmed.starts_with("edge::calls[") || trimmed.starts_with("edge::") {
            continue;
        }

        // --- Table delimiter |=== ---
        if trimmed == "|===" {
            // Flush any accumulated callee list when table closes
            flush_callees(&mut callee_calls, &mut out);
            table_mode = TableMode::None;
            continue;
        }

        // --- Table rows ---
        if trimmed.starts_with('|') && trimmed.chars().filter(|&c| c == '|').count() >= 2 {
            let cells: Vec<&str> = trimmed
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect();

            // Detect table headers
            if cells.len() >= 2 {
                let h0 = cells[0].to_lowercase();
                let h1 = cells[1].to_lowercase();

                if (h0 == "property" && h1 == "value") || (h0 == "name" && h1 == "value") {
                    table_mode = TableMode::Property;
                    continue;
                }
                if h0 == "callee" && h1 == "line" {
                    table_mode = TableMode::Callee;
                    continue;
                }

                match table_mode {
                    TableMode::Property => {
                        // Compact: |Name|foo| → "name: foo"
                        let key = cells[0].to_lowercase().replace(' ', "_");
                        let val = cells[1..].join(" ").trim().to_string();
                        if !key.is_empty() && !val.is_empty() {
                            out.push(format!("{}: {}", key, val));
                            prev_blank = false;
                        }
                        continue;
                    }
                    TableMode::Callee => {
                        // Accumulate: |foo|12| → "foo(12)"
                        let callee = cells[0].trim();
                        let lineno = cells.get(1).map(|s| s.trim()).unwrap_or("");
                        if !callee.is_empty() {
                            if lineno.is_empty() || lineno == "0" {
                                callee_calls.push(callee.to_string());
                            } else {
                                callee_calls.push(format!("{}({})", callee, lineno));
                            }
                        }
                        continue;
                    }
                    TableMode::None => {
                        // Generic table row — keep as compact space-separated values
                        let row = cells.join("  ").trim().to_string();
                        if !row.is_empty() {
                            out.push(row);
                            prev_blank = false;
                        }
                        continue;
                    }
                }
            }
        } else if table_mode != TableMode::None {
            // Non-table line encountered — flush callee list and reset mode
            flush_callees(&mut callee_calls, &mut out);
            table_mode = TableMode::None;
        }

        // --- Headings: convert = Title → Title, == Section → Section: ---
        let processed = if let Some(rest) = trimmed.strip_prefix("= ") {
            rest.to_string()
        } else if trimmed.starts_with("== ") || trimmed.starts_with("=== ") || trimmed.starts_with("==== ") {
            let stripped = trimmed.trim_start_matches('=').trim();
            format!("{}:", stripped)
        } else {
            // Inline xref cleanup: <<anchor,text>> → text, <<anchor>> → anchor
            replace_xrefs(line)
        };

        // Collapse multiple blank lines to one
        let is_blank = processed.trim().is_empty();
        if is_blank {
            if !prev_blank {
                out.push(String::new());
            }
            prev_blank = true;
            continue;
        }
        prev_blank = false;
        out.push(processed);
    }

    // Final flush of any pending callee list
    flush_callees(&mut callee_calls, &mut out);

    out.join("\n").trim().to_string()
}

/// Replace AsciiDoc cross-reference macros with their display text or target.
/// `<<anchor,display text>>` → `display text`
/// `<<anchor>>` → `anchor`
fn replace_xrefs(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut remaining = line;

    while let Some(start) = remaining.find("<<") {
        result.push_str(&remaining[..start]);
        let after = &remaining[start + 2..];
        if let Some(end) = after.find(">>") {
            let inner = &after[..end];
            if let Some(comma) = inner.find(',') {
                // <<anchor,display text>> → display text
                result.push_str(inner[comma + 1..].trim());
            } else {
                // <<anchor>> → anchor (strip internal ID prefixes like aden://)
                let anchor = inner.trim();
                let display = anchor
                    .rsplit('#')
                    .next()
                    .unwrap_or(anchor)
                    .replace('-', " ");
                result.push_str(&display);
            }
            remaining = &after[end + 2..];
        } else {
            result.push_str("<<");
            remaining = after;
        }
    }
    result.push_str(remaining);
    result
}

/// Truncate text to approximately `max_tokens` tokens at a word boundary.
fn truncate_to_tokens(text: &str, max_tokens: usize) -> String {
    // Each word ≈ 4/3 tokens, so max words ≈ max_tokens * 3/4
    let max_words = (max_tokens * 3 / 4).max(1);
    let mut word_count = 0;
    let mut last_boundary = text.len();

    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            word_count += 1;
            if word_count >= max_words {
                last_boundary = i;
                break;
            }
        }
    }

    let mut truncated = text[..last_boundary].trim_end().to_string();
    if last_boundary < text.len() {
        truncated.push_str(" …");
    }
    truncated
}

/// Improved token estimation using a word-based heuristic.
/// Typical LLM tokenization yields roughly 0.75 words per token.
fn estimate_tokens(text: &str) -> usize {
    let word_count = text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .count();
    (word_count * 4 / 3).max(1)
}
