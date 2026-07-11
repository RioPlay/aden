// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::graph::AdenGraph;
use crate::nodes::{AdenEdge, DocumentNode, GraphNode};
use aden_core::{Block, Table};
use petgraph::Direction;

/// For every node in the graph, append (or update) a `== Referenced By` table
/// listing all incoming edges.
pub fn inject_backlinks(graph: &mut AdenGraph<DocumentNode, AdenEdge>) {
    let mut backlink_tables: Vec<(petgraph::graph::NodeIndex, Vec<Vec<String>>)> = Vec::new();

    for node in graph.graph.node_indices() {
        let mut rows: Vec<Vec<String>> = Vec::new();
        for neighbor in graph.graph.neighbors_directed(node, Direction::Incoming) {
            let edge = graph.graph.find_edge(neighbor, node);
            let edge_type = edge
                .and_then(|e| graph.graph.edge_weight(e))
                .map(|t| format!("{:?}", t.edge_type))
                .unwrap_or_else(|| "Unknown".to_string());
            rows.push(vec![graph.graph[neighbor].anchor().to_string(), edge_type]);
        }
        if !rows.is_empty() {
            backlink_tables.push((node, rows));
        }
    }

    for (node, rows) in backlink_tables {
        // Remove existing "Referenced By" block if present
        let doc = &mut graph.graph[node];
        doc.doc.blocks.retain(|b| {
            if let Block::Paragraph(text) = b {
                !text.starts_with("== Referenced By")
            } else {
                true
            }
        });
        // Add new backlink table
        doc.doc
            .blocks
            .push(Block::Paragraph("== Referenced By".to_string()));
        doc.doc.blocks.push(Block::Table(Table {
            headers: vec!["Anchor".to_string(), "Edge Type".to_string()],
            rows,
        }));
    }
}
