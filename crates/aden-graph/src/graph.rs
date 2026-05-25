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
use crate::parser::{parse_file, ParsedDocument};
use aden_core::{Document, EdgeType, NodeType};
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A node in the Aden graph.
#[derive(Debug, Clone)]
pub struct DocumentNode {
    pub anchor: String,
    pub doc: Document,
    pub parsed: ParsedDocument,
    pub source_path: PathBuf,
}

/// The Aden knowledge graph.
#[derive(Debug)]
pub struct AdenGraph {
    pub graph: DiGraph<DocumentNode, EdgeType>,
    pub anchor_to_index: HashMap<String, NodeIndex>,
    pub path_to_index: HashMap<PathBuf, NodeIndex>,
    pub filter: aden_core::filter::AdenFilter,
    #[doc(hidden)]
    pub(crate) backlinks_cache: Option<HashMap<String, Vec<String>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("duplicate anchor: {0}")]
    DuplicateAnchor(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("IO error: {0}")]
    Io(String),
    #[error("unresolved reference: {0}")]
    UnresolvedReference(String),
    #[error("orphan document: {0}")]
    OrphanDocument(String),
}

impl Default for AdenGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl AdenGraph {
    /// Create an empty graph.
    pub fn new() -> Self {
        use aden_core::filter::AdenFilter;
        Self {
            graph: DiGraph::new(),
            anchor_to_index: HashMap::new(),
            path_to_index: HashMap::new(),
            filter: AdenFilter::from_directory(Path::new(".")),
            backlinks_cache: None,
        }
    }

    /// Parse all `.adoc` / `.aden` files in a directory and build the graph.
    pub fn build_from_directory(dir: &Path) -> Result<Self, GraphError> {
        use aden_core::filter::AdenFilter;
        let mut graph = Self::new();
        graph.filter = AdenFilter::from_directory(dir);
        let mut files = Vec::new();
        graph.collect_files(dir, &mut files)?;

        // First pass: add all nodes
        for path in &files {
            let parsed = parse_file(path).map_err(|e| GraphError::Parse(e.to_string()))?;
            let primary_anchor = parsed.anchors.first()
                .cloned()
                .unwrap_or_else(|| path.file_stem().unwrap_or_default().to_string_lossy().to_string());

            let doc = Document {
                anchor: primary_anchor.clone(),
                node_type: aden_core::NodeType::Note, // will be refined by attribute
                attributes: parsed.attributes.clone(),
                blocks: parsed.blocks.clone(),
                source_span: None,
            };

            let anchors = parsed.anchors.clone();
            let node = DocumentNode {
                anchor: primary_anchor.clone(),
                doc,
                parsed,
                source_path: path.clone(),
            };

            let idx = graph.graph.add_node(node);
            // Register ALL anchors in the file, not just the primary one
            // Skip duplicates silently - first anchor wins (maintains backward compatibility)
            for anchor in &anchors {
                if !graph.anchor_to_index.contains_key(anchor) {
                    graph.anchor_to_index.insert(anchor.clone(), idx);
                }
            }
            graph.path_to_index.insert(path.clone(), idx);
        }

        // Second pass: add edges (refs + includes + explicit edge macros)
        for path in &files {
            if let Some(&idx) = graph.path_to_index.get(path) {
                let parsed = &graph.graph[idx].parsed.clone();
                // Include edges → Requires
                for inc in &parsed.includes {
                    if let Ok(inc_path) = resolve_include_path(path, &inc.path, dir)
                        && let Some(&target_idx) = graph.path_to_index.get(&inc_path) {
                            graph.graph.add_edge(idx, target_idx, EdgeType::Requires);
                        }
                }
                // Reference edges → Uses (default for refs)
                for r in &parsed.refs {
                    if let Some(&target_idx) = graph.anchor_to_index.get(r)
                        && !graph.graph.contains_edge(idx, target_idx) {
                            graph.graph.add_edge(idx, target_idx, EdgeType::Uses);
                        }
                }
                // Explicit edge macros
                for e in &parsed.edges {
                    if let Some(&target_idx) = graph.anchor_to_index.get(&e.target) {
                        let edge_type = parse_edge_type(&e.edge_type);
                        if !graph.graph.contains_edge(idx, target_idx) {
                            graph.graph.add_edge(idx, target_idx, edge_type);
                        }
                    }
                }
            }
        }

        Ok(graph)
    }

    fn collect_files(&self, dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), GraphError> {
        // Attempt to find project root for relative-path filtering
        let root = self.find_root(dir);
        self.collect_files_inner(dir, &root, files)?;
        Ok(())
    }

    fn find_root(&self, start: &Path) -> PathBuf {
        let mut current = start.to_path_buf();
        loop {
            if current.join("Cargo.toml").exists() || current.join("aden.toml").exists() || current.join(".adenignore").exists() {
                return current;
            }
            if let Some(parent) = current.parent() {
                current = parent.to_path_buf();
            } else {
                return start.to_path_buf();
            }
        }
    }

    fn collect_files_inner(&self, dir: &Path, root: &Path, files: &mut Vec<PathBuf>) -> Result<(), GraphError> {
        for entry in std::fs::read_dir(dir).map_err(|e| GraphError::Io(e.to_string()))? {
            let entry = entry.map_err(|e| GraphError::Io(e.to_string()))?;
            let path = entry.path();
            if let Ok(rel) = path.strip_prefix(root)
                && self.filter.should_skip(rel) {
                    continue;
                }
            if path.is_dir() {
                self.collect_files_inner(&path, root, files)?;
            } else if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if ext == "adoc" || ext == "aden" {
                    files.push(path);
                }
            }
        }
        Ok(())
    }

    /// Find all unresolved references in the graph.
    pub fn unresolved_refs(&self) -> Vec<(String, String)> {
        let mut issues = Vec::new();
        for node in self.graph.node_indices() {
            let parsed = &self.graph[node].parsed;
            for r in &parsed.refs {
                if !self.anchor_to_index.contains_key(r) {
                    issues.push((parsed.source_path.clone(), r.clone()));
                }
            }
        }
        issues
    }

    /// Find orphan nodes (no incoming edges, no outgoing edges).
    pub fn orphans(&self) -> Vec<String> {
        let mut orphans = Vec::new();
        for node in self.graph.node_indices() {
            let in_count = self.graph.neighbors_directed(node, petgraph::Direction::Incoming).count();
            let out_count = self.graph.neighbors_directed(node, petgraph::Direction::Outgoing).count();
            if in_count == 0 && out_count == 0 {
                orphans.push(self.graph[node].anchor.clone());
            }
        }
        orphans
    }

    /// Get a node by anchor.
    pub fn get_node(&self, anchor: &str) -> Option<&DocumentNode> {
        self.anchor_to_index.get(anchor).map(|&idx| &self.graph[idx])
    }

    /// Get node index by anchor.
    pub fn get_index(&self, anchor: &str) -> Option<NodeIndex> {
        self.anchor_to_index.get(anchor).copied()
    }

    /// Get all anchors that reference this anchor (backlinks).
    /// Uses cached computation for O(1) lookup after first call.
    pub fn get_backlinks(&mut self, anchor: &str) -> Vec<String> {
        // Compute backlinks cache on first call
        if self.backlinks_cache.is_none() {
            self.compute_backlinks_cache();
        }

        self.backlinks_cache
            .as_ref()
            .and_then(|cache| cache.get(anchor).cloned())
            .unwrap_or_default()
    }

    /// Compute backlinks cache for fast reverse lookups.
    /// This is an O(n) operation that enables O(1) reverse lookups thereafter.
    fn compute_backlinks_cache(&mut self) {
        let mut cache: HashMap<String, Vec<String>> = HashMap::new();

        // Initialize all anchors in the cache
        for anchor in self.anchor_to_index.keys() {
            cache.insert(anchor.clone(), Vec::new());
        }

        // Build reverse edges
        for edge_idx in self.graph.edge_indices() {
            if let Some((src, tgt)) = self.graph.edge_endpoints(edge_idx) {
                let src_anchor = &self.graph[src].anchor;
                let tgt_anchor = &self.graph[tgt].anchor;

                if let Some(targets) = cache.get_mut(tgt_anchor) {
                    targets.push(src_anchor.clone());
                }
            }
        }

        self.backlinks_cache = Some(cache);
    }

    /// Validate that every edge satisfies the node-type compatibility matrix.
    pub fn validate_typed_edges(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for edge_idx in self.graph.edge_indices() {
            let (source_idx, target_idx) = self.graph.edge_endpoints(edge_idx)
                .expect("edge_endpoints called with valid index from edge_indices iterator");
            let source = &self.graph[source_idx];
            let target = &self.graph[target_idx];
            let edge_type = self.graph[edge_idx];

            let valid = match edge_type {
                EdgeType::Uses => {
                    let source_is_doc = matches!(
                        source.doc.node_type,
                        NodeType::Note | NodeType::Adr | NodeType::Plan | NodeType::Spec | NodeType::Context | NodeType::Manifest | NodeType::Runbook
                    );
                    source_is_doc || (
                        matches!(
                            source.doc.node_type,
                            NodeType::Module | NodeType::Function | NodeType::Script
                        ) && matches!(
                            target.doc.node_type,
                            NodeType::Module | NodeType::Function
                        )
                    )
                }
                EdgeType::Calls => {
                    matches!(
                        source.doc.node_type,
                        NodeType::Module | NodeType::Function | NodeType::Script
                    ) && matches!(
                        target.doc.node_type,
                        NodeType::Module | NodeType::Function
                    )
                }
                EdgeType::Implements => {
                    matches!(source.doc.node_type, NodeType::Function | NodeType::Type)
                        && matches!(target.doc.node_type, NodeType::Type)
                }
                EdgeType::Tests | EdgeType::Verifies => {
                    matches!(
                        source.doc.node_type,
                        NodeType::Module | NodeType::Function
                    ) && matches!(
                        target.doc.node_type,
                        NodeType::Module | NodeType::Function
                    )
                }
                EdgeType::Documents | EdgeType::Constrains | EdgeType::Justifies => {
                    matches!(
                        source.doc.node_type,
                        NodeType::Adr | NodeType::Plan | NodeType::Spec | NodeType::Note
                    ) && matches!(
                        target.doc.node_type,
                        NodeType::Module | NodeType::Function | NodeType::Script
                    )
                }
                EdgeType::Invokes => {
                    matches!(source.doc.node_type, NodeType::Runbook | NodeType::Script)
                        && matches!(
                            target.doc.node_type,
                            NodeType::Script | NodeType::Function
                        )
                }
                // Requires edges from include::[] are structural; skip type validation.
                EdgeType::Requires => true,
                EdgeType::Mutates => {
                    matches!(source.doc.node_type, NodeType::Function)
                        && matches!(target.doc.node_type, NodeType::Module | NodeType::Manifest)
                }
                EdgeType::Supersedes | EdgeType::Amends => {
                    matches!(
                        source.doc.node_type,
                        NodeType::Adr | NodeType::Plan | NodeType::Spec | NodeType::Note
                    ) && matches!(
                        target.doc.node_type,
                        NodeType::Adr | NodeType::Plan | NodeType::Spec | NodeType::Note
                    )
                }
            };

            if !valid {
                errors.push(format!(
                    "Invalid edge {:?} from {} ({:?}) to {} ({:?})",
                    edge_type, source.anchor, source.doc.node_type, target.anchor, target.doc.node_type
                ));
            }
        }
        errors
    }
}

/// Resolve an include path, preventing directory traversal attacks.
/// Returns an error if the resolved path escapes `root`.
fn resolve_include_path(current: &Path, include: &str, root: &Path) -> Result<PathBuf, GraphError> {
    let base = current.parent().unwrap_or(Path::new("."));
    let candidate = base.join(include);
    if !candidate.exists() {
        return Ok(candidate); // still return the path for graph edges even if missing
    }
    let canon = candidate.canonicalize().map_err(|e| {
        GraphError::Io(format!("canonicalize failed for '{}': {}", include, e))
    })?;
    let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if !canon.starts_with(&root_canon) {
        return Err(GraphError::Io(format!(
            "Include path '{}' escapes root directory. Denied for security.",
            include
        )));
    }
    Ok(canon)
}

fn parse_edge_type(s: &str) -> EdgeType {
    match s {
        "uses" => EdgeType::Uses,
        "implements" => EdgeType::Implements,
        "tests" => EdgeType::Tests,
        "documents" => EdgeType::Documents,
        "constrains" => EdgeType::Constrains,
        "justifies" => EdgeType::Justifies,
        "invokes" => EdgeType::Invokes,
        "requires" => EdgeType::Requires,
        "mutates" => EdgeType::Mutates,
        "calls" => EdgeType::Calls,
        "supersedes" => EdgeType::Supersedes,
        "amends" => EdgeType::Amends,
        "verifies" => EdgeType::Verifies,
        _ => EdgeType::Uses,
    }
}
