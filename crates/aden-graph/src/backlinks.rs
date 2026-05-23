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
use crate::graph::AdenGraph;
use aden_core::{Block, Table};
use petgraph::Direction;

/// For every node in the graph, append (or update) a `== Referenced By` table
/// listing all incoming edges.
pub fn inject_backlinks(graph: &mut AdenGraph) {
    let mut backlink_tables: Vec<(petgraph::graph::NodeIndex, Vec<Vec<String>>)> = Vec::new();

    for node in graph.graph.node_indices() {
        let mut rows: Vec<Vec<String>> = Vec::new();
        for neighbor in graph.graph.neighbors_directed(node, Direction::Incoming) {
            let edge = graph.graph.find_edge(neighbor, node);
            let edge_type = edge
                .and_then(|e| graph.graph.edge_weight(e))
                .map(|t| format!("{:?}", t))
                .unwrap_or_else(|| "Unknown".to_string());
            rows.push(vec![
                graph.graph[neighbor].anchor.clone(),
                edge_type,
            ]);
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
        doc.doc.blocks.push(Block::Paragraph("== Referenced By".to_string()));
        doc.doc.blocks.push(Block::Table(Table {
            headers: vec!["Anchor".to_string(), "Edge Type".to_string()],
            rows,
        }));
    }
}
