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
use crate::nodes::{AdenEdge, DocumentNode, GraphNode};
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::HashSet;

/// Detect cycles in the graph using DFS.
/// Returns a list of anchor names that form cycles (first node of each cycle).
pub fn find_cycles(graph: &AdenGraph<DocumentNode, AdenEdge>) -> Vec<Vec<String>> {
    let mut visited = HashSet::new();
    let mut rec_stack = HashSet::new();
    let mut cycles = Vec::new();

    for node in graph.graph.node_indices() {
        if !visited.contains(&node) {
            let mut path = Vec::new();
            dfs_cycle(
                graph,
                node,
                &mut visited,
                &mut rec_stack,
                &mut path,
                &mut cycles,
            );
        }
    }

    cycles
}

fn dfs_cycle(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    node: NodeIndex,
    visited: &mut HashSet<NodeIndex>,
    rec_stack: &mut HashSet<NodeIndex>,
    path: &mut Vec<String>,
    cycles: &mut Vec<Vec<String>>,
) {
    visited.insert(node);
    rec_stack.insert(node);
    path.push(graph.graph[node].anchor().to_string());

    for edge in graph
        .graph
        .edges_directed(node, petgraph::Direction::Outgoing)
    {
        if edge.weight().edge_type != aden_core::EdgeType::Requires {
            continue;
        }
        let neighbor = edge.target();
        if !visited.contains(&neighbor) {
            dfs_cycle(graph, neighbor, visited, rec_stack, path, cycles);
        } else if rec_stack.contains(&neighbor) {
            // Found a cycle — extract the cycle from path
            if let Some(pos) = path
                .iter()
                .position(|a| *a == graph.graph[neighbor].anchor())
            {
                let cycle = path[pos..].to_vec();
                cycles.push(cycle);
            }
        }
    }

    path.pop();
    rec_stack.remove(&node);
}

/// Check if the graph contains any include-based cycles.
pub fn has_cycles(graph: &AdenGraph<DocumentNode, AdenEdge>) -> bool {
    !find_cycles(graph).is_empty()
}
