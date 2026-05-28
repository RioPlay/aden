// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Aden Query (.adq) interpreter for graph queries.
//!
//! Supports: node(), incoming(), outgoing(), where, limit, order_by

use crate::graph::AdenGraph;
use crate::nodes::{DocumentNode, AdenEdge, GraphNode};
use petgraph::Direction;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub nodes: Vec<String>,
    pub total: usize,
}

pub struct AdqInterpreter<'a> {
    graph: &'a AdenGraph<DocumentNode, AdenEdge>,
}

impl std::fmt::Debug for AdqInterpreter<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdqInterpreter").field("graph", &"AdenGraph").finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error(
        "unknown ADQ function: '{0}'. Valid: node(anchor), incoming(anchor), outgoing(anchor), nodes, edges"
    )]
    UnknownFunction(String),
    #[error("invalid anchor: {0}")]
    InvalidAnchor(String),
    #[error("parse error: {0}")]
    Parse(String),
}

impl<'a> AdqInterpreter<'a> {
    pub fn new(graph: &'a AdenGraph<DocumentNode, AdenEdge>) -> Self {
        Self { graph }
    }

    pub fn execute(&self, adq_script: &str) -> Result<QueryResult, QueryError> {
        let script = adq_script.trim();

        // Parse function calls: node(anchor), incoming(anchor), outgoing(anchor)
        // Also support: node anchor, incoming anchor
        if let Some(paren_pos) = script.find('(') {
            let func_name = script[..paren_pos].trim();
            let arg = script[paren_pos + 1..].trim_end_matches(')').trim();

            match func_name {
                "node" => self.exec_node(&[arg]),
                "incoming" => self.exec_incoming(&[arg]),
                "outgoing" => self.exec_outgoing(&[arg]),
                _ => Err(QueryError::UnknownFunction(func_name.to_string())),
            }
        } else {
            // Simple command without parentheses
            match script {
                "nodes" => self.exec_all_nodes(&[]),
                "edges" => self.exec_all_edges(&[]),
                _ => Err(QueryError::UnknownFunction(script.to_string())),
            }
        }
    }

    fn exec_node(&self, args: &[&str]) -> Result<QueryResult, QueryError> {
        let anchor = args
            .first()
            .ok_or_else(|| QueryError::Parse("node() requires anchor".to_string()))?;
        let anchor = anchor.trim_matches(|c| c == '(' || c == ')' || c == ';');

        if self.graph.get_index(anchor).is_none() {
            return Err(QueryError::InvalidAnchor(anchor.to_string()));
        }

        Ok(QueryResult {
            nodes: vec![anchor.to_string()],
            total: 1,
        })
    }

    fn exec_incoming(&self, args: &[&str]) -> Result<QueryResult, QueryError> {
        let anchor = args
            .first()
            .ok_or_else(|| QueryError::Parse("incoming() requires anchor".to_string()))?;
        let anchor = anchor.trim_matches(|c| c == '(' || c == ')' || c == ';');

        let idx = self
            .graph
            .get_index(anchor)
            .ok_or_else(|| QueryError::InvalidAnchor(anchor.to_string()))?;

        let mut nodes = Vec::new();
        for neighbor in self
            .graph
            .graph
            .neighbors_directed(idx, Direction::Incoming)
        {
            if let Some(node) = self.graph.graph.node_weight(neighbor) {
                nodes.push(node.anchor().to_string());
            }
        }

        let total = nodes.len();
        Ok(QueryResult { nodes, total })
    }

    fn exec_outgoing(&self, args: &[&str]) -> Result<QueryResult, QueryError> {
        let anchor = args
            .first()
            .ok_or_else(|| QueryError::Parse("outgoing() requires anchor".to_string()))?;
        let anchor = anchor.trim_matches(|c| c == '(' || c == ')' || c == ';');

        let idx = self
            .graph
            .get_index(anchor)
            .ok_or_else(|| QueryError::InvalidAnchor(anchor.to_string()))?;

        let mut nodes = Vec::new();
        for neighbor in self
            .graph
            .graph
            .neighbors_directed(idx, Direction::Outgoing)
        {
            if let Some(node) = self.graph.graph.node_weight(neighbor) {
                nodes.push(node.anchor().to_string());
            }
        }

        let total = nodes.len();
        Ok(QueryResult { nodes, total })
    }

    #[allow(dead_code)]
    fn exec_where(&self, args: &[&str]) -> Result<QueryResult, QueryError> {
        // Simple where: anchor contains "term" or type = "Note"
        let mut nodes = Vec::new();

        for idx in self.graph.graph.node_indices() {
            if let Some(node) = self.graph.graph.node_weight(idx) {
                let mut matches = true;
                for arg in args {
                    let arg = arg.trim_matches(|c| c == '(' || c == ')' || c == ';');
                    if arg.starts_with("anchor:") || arg.starts_with("anchor=") {
                        let term = arg
                            .split(':')
                            .nth(1)
                            .unwrap_or(arg.split('=').nth(1).unwrap_or(""));
                        if !node.anchor().contains(term) {
                            matches = false;
                        }
                    }
                }
                if matches {
                    nodes.push(node.anchor().to_string());
                }
            }
        }

        let total = nodes.len();
        Ok(QueryResult { nodes, total })
    }

    fn exec_all_nodes(&self, _args: &[&str]) -> Result<QueryResult, QueryError> {
        let mut nodes = Vec::new();
        for idx in self.graph.graph.node_indices() {
            if let Some(node) = self.graph.graph.node_weight(idx) {
                nodes.push(node.anchor().to_string());
            }
        }
        let total = nodes.len();
        Ok(QueryResult { nodes, total })
    }

    fn exec_all_edges(&self, _args: &[&str]) -> Result<QueryResult, QueryError> {
        let mut nodes = Vec::new();
        for edge in self.graph.graph.edge_indices() {
            let (src, dst) = self.graph.graph.edge_endpoints(edge).unwrap();
            if let (Some(src_node), Some(dst_node)) = (
                self.graph.graph.node_weight(src),
                self.graph.graph.node_weight(dst),
            ) {
                nodes.push(format!("{} -> {}", src_node.anchor(), dst_node.anchor()));
            }
        }
        let total = nodes.len();
        Ok(QueryResult { nodes, total })
    }
}

pub fn execute_adq(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    script: &str,
) -> Result<QueryResult, QueryError> {
    let interpreter = AdqInterpreter::new(graph);
    interpreter.execute(script)
}
