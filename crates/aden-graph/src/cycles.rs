// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

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
