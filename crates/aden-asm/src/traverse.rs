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
use aden_core::EdgeType;
use aden_graph::{AdenGraph, graph::DocumentNode};
use petgraph::Direction;
use std::collections::{HashSet, VecDeque};

/// Options for assembling a context prompt.
#[derive(Debug, Clone)]
pub struct AssemblyOptions {
    pub start_anchor: String,
    pub max_depth: usize,
    /// Token budget (approximate byte-pair estimation).
    pub token_budget: usize,
    /// Edge types to follow. If empty, follow all.
    pub edge_types: Vec<EdgeType>,
}

/// Assemble a flat `.adoc` prompt from a graph neighborhood.
pub fn assemble(graph: &AdenGraph, opts: &AssemblyOptions) -> Result<String, AssemblyError> {
    let start_idx = graph.get_index(&opts.start_anchor).ok_or_else(|| {
        AssemblyError::MissingAnchor(opts.start_anchor.clone())
    })?;

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
        let text = document_to_text(doc);
        let tokens = estimate_tokens(&text);
        if total_tokens + tokens > opts.token_budget {
            break;
        }
        total_tokens += tokens;
        visited.insert(node);
        result.push(text);

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

    Ok(result.join("\n<<<\n"))
}

#[derive(Debug, thiserror::Error)]
pub enum AssemblyError {
    #[error("missing anchor: {0}")]
    MissingAnchor(String),
    #[error("graph error: {0}")]
    Graph(String),
}

fn document_to_text(doc: &DocumentNode) -> String {
    use aden_core::{Block, AdmonitionKind};
    let mut out = String::new();
    // Attributes
    for (key, value) in &doc.doc.attributes {
        out.push_str(&format!(":{key}: {value}\n"));
    }
    out.push('\n');
    // Anchor + Title
    out.push_str(&format!("[[{}]]\n", doc.anchor));
    let title = doc.anchor.rfind('#').map(|p| &doc.anchor[p+1..]).unwrap_or(&doc.anchor);
    out.push_str(&format!("= {title}\n\n"));
    // Blocks
    for block in &doc.doc.blocks {
        match block {
            Block::Paragraph(t) => {
                out.push_str(t);
                out.push('\n');
            }
            Block::Table(table) => {
                out.push_str("|===\n");
                let header = table.headers.iter().map(|h| format!("|{h}")).collect::<String>();
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
        }
    }
    out
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
