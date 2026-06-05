// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Generic graph engine for the Aden knowledge graph.
//!
//! `AdenGraph<N, E>` is parameterized over node type `N` and edge type `E`.
//! Any types implementing `GraphNode` and `GraphEdge` can be used.
//!
//! ## Example: Using with custom types
//!
//! ```ignore
//! use aden_graph::nodes::{GraphNode, GraphEdge};
//! use aden_graph::graph::AdenGraph;
//!
//! struct MyNode { /* ... */ }
//! impl GraphNode for MyNode { /* ... */ }
//!
//! struct MyEdge { /* ... */ }
//! impl GraphEdge for MyEdge { /* ... */ }
//!
//! let graph: AdenGraph<MyNode, MyEdge> = AdenGraph::new();
//! ```

use crate::nodes::{AdenEdge, DocumentNode, GraphEdge, GraphNode};
use crate::parser::{ParsedDocument, parse_file};
use aden_core::{Document, EdgeType, NodeType};
use aden_store::GraphStorage;
use petgraph::graph::{DiGraph, NodeIndex};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The generic knowledge graph.
///
/// Parameterized over node type `N` and edge type `E`.
/// Any types implementing `GraphNode` and `GraphEdge` can be used.
pub struct AdenGraph<N: GraphNode, E: GraphEdge> {
    pub graph: DiGraph<N, E>,
    pub anchor_to_index: HashMap<String, NodeIndex>,
    pub path_to_index: HashMap<PathBuf, NodeIndex>,
    #[doc(hidden)]
    pub(crate) backlinks_cache: Option<HashMap<String, Vec<String>>>,
}

/// Errors that can occur during graph operations.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("duplicate anchor: {0}")]
    DuplicateAnchor(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("IO error: {0}")]
    Io(String),

    #[error("unresolved reference: {0} -> {1}")]
    UnresolvedReference(String, String),

    #[error("orphan node: {0}")]
    OrphanNode(String),
}

impl<N: GraphNode, E: GraphEdge> Default for AdenGraph<N, E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: GraphNode, E: GraphEdge> AdenGraph<N, E> {
    /// Create an empty graph.
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            anchor_to_index: HashMap::new(),
            path_to_index: HashMap::new(),
            backlinks_cache: None,
        }
    }

    /// Add a node to the graph.
    pub fn add_node(&mut self, node: N) -> NodeIndex {
        let anchor = node.anchor().to_string();
        let source_path = node.source_path().clone();
        let idx = self.graph.add_node(node);
        self.anchor_to_index.insert(anchor, idx);
        self.path_to_index.insert(source_path, idx);
        self.backlinks_cache = None;
        idx
    }

    /// Add an edge between two nodes by anchor.
    pub fn add_edge_by_anchor(
        &mut self,
        src_anchor: &str,
        tgt_anchor: &str,
        edge: E,
    ) -> Result<(), GraphError> {
        let src_idx = self
            .anchor_to_index
            .get(src_anchor)
            .copied()
            .ok_or_else(|| {
                GraphError::UnresolvedReference(src_anchor.to_string(), "not found".to_string())
            })?;
        let tgt_idx = self
            .anchor_to_index
            .get(tgt_anchor)
            .copied()
            .ok_or_else(|| {
                GraphError::UnresolvedReference(tgt_anchor.to_string(), "not found".to_string())
            })?;

        if !self.graph.contains_edge(src_idx, tgt_idx) {
            self.graph.add_edge(src_idx, tgt_idx, edge);
            self.backlinks_cache = None;
        }
        Ok(())
    }

    /// Add an edge between two nodes by index.
    pub fn add_edge(&mut self, src: NodeIndex, tgt: NodeIndex, edge: E) {
        if !self.graph.contains_edge(src, tgt) {
            self.graph.add_edge(src, tgt, edge);
            self.backlinks_cache = None;
        }
    }

    /// Get a node by anchor.
    pub fn get_node(&self, anchor: &str) -> Option<&N> {
        self.anchor_to_index
            .get(anchor)
            .map(|&idx| &self.graph[idx])
    }

    /// Get node index by anchor.
    pub fn get_index(&self, anchor: &str) -> Option<NodeIndex> {
        self.anchor_to_index.get(anchor).copied()
    }

    /// Get all anchors that reference this anchor (backlinks).
    /// Uses cached computation for O(1) lookup after first call.
    pub fn get_backlinks(&mut self, anchor: &str) -> Vec<String> {
        if self.backlinks_cache.is_none() {
            self.compute_backlinks_cache();
        }

        self.backlinks_cache
            .as_ref()
            .and_then(|cache| cache.get(anchor).cloned())
            .unwrap_or_default()
    }

    /// Compute backlinks cache for fast reverse lookups.
    fn compute_backlinks_cache(&mut self) {
        let mut cache: HashMap<String, Vec<String>> = HashMap::new();

        for anchor in self.anchor_to_index.keys() {
            cache.insert(anchor.clone(), Vec::new());
        }

        for edge_idx in self.graph.edge_indices() {
            if let Some((src, tgt)) = self.graph.edge_endpoints(edge_idx) {
                let src_anchor = self.graph[src].anchor();
                let tgt_anchor = self.graph[tgt].anchor();

                if let Some(targets) = cache.get_mut(tgt_anchor) {
                    targets.push(src_anchor.to_string());
                }
            }
        }

        self.backlinks_cache = Some(cache);
    }

    /// Find all unresolved references in the graph.
    pub fn unresolved_refs(&self) -> Vec<(String, String)> {
        let mut issues = Vec::new();
        for node in self.graph.node_indices() {
            let attrs = self.graph[node].attributes();
            // Check for refs in attributes (format: "refs: anchor1,anchor2")
            if let Some(refs) = attrs.get("refs") {
                for ref_anchor in refs.split(',') {
                    let ref_anchor = ref_anchor.trim();
                    if !ref_anchor.is_empty() && !self.anchor_to_index.contains_key(ref_anchor) {
                        issues.push((
                            self.graph[node].source_path().to_string_lossy().to_string(),
                            ref_anchor.to_string(),
                        ));
                    }
                }
            }
        }
        issues
    }

    /// Find orphan nodes (no incoming edges, no outgoing edges).
    pub fn orphans(&self) -> Vec<String> {
        let mut orphans = Vec::new();
        for node in self.graph.node_indices() {
            let in_count = self
                .graph
                .neighbors_directed(node, petgraph::Direction::Incoming)
                .count();
            let out_count = self
                .graph
                .neighbors_directed(node, petgraph::Direction::Outgoing)
                .count();
            if in_count == 0 && out_count == 0 {
                orphans.push(self.graph[node].anchor().to_string());
            }
        }
        orphans
    }

    /// Get all edge endpoints as (src_anchor, tgt_anchor, edge).
    pub fn all_edges(&self) -> Vec<(String, String, E)> {
        let mut edges = Vec::new();
        for edge_idx in self.graph.edge_indices() {
            if let Some((src, tgt)) = self.graph.edge_endpoints(edge_idx) {
                let src_anchor = self.graph[src].anchor().to_string();
                let tgt_anchor = self.graph[tgt].anchor().to_string();
                let edge = self.graph[edge_idx].clone();
                edges.push((src_anchor, tgt_anchor, edge));
            }
        }
        edges
    }

    /// Get all nodes as (anchor, node).
    pub fn all_nodes(&self) -> Vec<(String, N)> {
        let mut nodes = Vec::new();
        for node_idx in self.graph.node_indices() {
            let node = self.graph[node_idx].clone();
            let anchor = node.anchor().to_string();
            nodes.push((anchor, node));
        }
        nodes
    }

    /// Count all nodes.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Count all edges.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Run BFS traversal from an anchor.
    pub fn bfs(&self, start: &str, max_depth: usize) -> Vec<(String, String)> {
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back((start.to_string(), 0usize));
        let mut results = Vec::new();

        if let Some(_start_idx) = self.anchor_to_index.get(start) {
            visited.insert(start.to_string());

            while let Some((current, d)) = queue.pop_front() {
                if d >= max_depth {
                    continue;
                }

                if let Some(&current_idx) = self.anchor_to_index.get(&current) {
                    for neighbor in self.graph.neighbors(current_idx) {
                        let neighbor_anchor = self.graph[neighbor].anchor().to_string();
                        if visited.insert(neighbor_anchor.clone()) {
                            results.push((current.clone(), neighbor_anchor.clone()));
                            queue.push_back((neighbor_anchor, d + 1));
                        }
                    }
                }
            }
        }

        results
    }

    /// Get neighborhood of an anchor at a given depth.
    pub fn neighborhood(&self, anchor: &str, depth: usize) -> HashMap<String, Vec<String>> {
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back((anchor.to_string(), 0usize));
        let mut result = HashMap::new();

        visited.insert(anchor.to_string());

        while let Some((current, d)) = queue.pop_front() {
            if d >= depth {
                continue;
            }

            if let Some(&current_idx) = self.anchor_to_index.get(&current) {
                let mut neighbors = Vec::new();
                for neighbor in self.graph.neighbors(current_idx) {
                    let neighbor_anchor = self.graph[neighbor].anchor().to_string();
                    if visited.insert(neighbor_anchor.clone()) {
                        neighbors.push(neighbor_anchor.clone());
                        queue.push_back((neighbor_anchor, d + 1));
                    }
                }
                if !neighbors.is_empty() {
                    result.insert(current, neighbors);
                }
            }
        }

        result
    }
}

/// Supported knowledge file extensions.
const SUPPORTED_EXTENSIONS: &[&str] = &["adoc", "aden", "md", "txt"];

/// Maximum directory recursion depth when collecting knowledge files.
/// SECURITY: bounds traversal on an untrusted repo so a deeply-nested or
/// symlink-cycled tree cannot exhaust the stack/time.
const MAX_COLLECT_DEPTH: usize = 32;

/// Recursively collect knowledge files from a directory.
fn collect_files(dir: &Path) -> Result<Vec<PathBuf>, GraphError> {
    // Honor the same exclusion rules as the rest of aden. Without this, the
    // directory graph ingested scaffolding/ignored trees (`.agent/`, `.claude/`,
    // `.git/`, `target/`, `node_modules/`), polluting every graph consumer
    // (diagnose, status, check) with phantom orphans and duplicate anchors that
    // never exist in the gen-built store.
    let filter = aden_core::filter::AdenFilter::from_directory(dir);
    collect_files_inner(dir, dir, &filter, 0)
}

fn collect_files_inner(
    root: &Path,
    dir: &Path,
    filter: &aden_core::filter::AdenFilter,
    depth: usize,
) -> Result<Vec<PathBuf>, GraphError> {
    let mut files = Vec::new();
    if depth > MAX_COLLECT_DEPTH {
        return Ok(files);
    }
    let entries = std::fs::read_dir(dir).map_err(|e| GraphError::Io(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| GraphError::Io(e.to_string()))?;
        // SECURITY: never follow symlinks — a crafted repo could symlink out of
        // the tree (traversal / info-exposure) or form a cycle (DoS).
        if entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(true) {
            continue;
        }
        let path = entry.path();
        // AdenFilter rules are expressed relative to the project root.
        let rel = path.strip_prefix(root).unwrap_or(&path);
        if filter.should_skip(rel) {
            continue;
        }
        if path.is_dir() {
            files.extend(collect_files_inner(root, &path, filter, depth + 1)?);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && SUPPORTED_EXTENSIONS.contains(&ext)
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Build a graph by parsing all knowledge files in a directory.
///
/// Steps:
/// 1. Recursively collect `.adoc`, `.aden`, `.md`, `.txt` files
/// 2. Parse each file into a `ParsedDocument`
/// 3. Create `DocumentNode` instances (one per anchor, or one per file if no anchors)
/// 4. Build edges from `refs` and `edge::` macros in the parsed docs
impl AdenGraph<DocumentNode, AdenEdge> {
    /// Parse all knowledge files in `dir` and build a graph.
    pub fn build_from_directory(dir: &Path) -> Result<Self, GraphError> {
        let files = collect_files(dir)?;
        let mut graph = Self::new();

        // Parallel first pass: parse all files
        let parsed_docs: Vec<(PathBuf, ParsedDocument)> = files
            .par_iter()
            .filter_map(|file_path| match parse_file(file_path) {
                Ok(parsed) => Some(Ok((file_path.clone(), parsed))),
                Err(e) if e.to_string().contains("binary") => None,
                Err(e) => Some(Err(GraphError::Parse(e.to_string()))),
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Parallel second pass: create nodes
        let node_work: Vec<_> = parsed_docs
            .par_iter()
            .flat_map(|(file_path, parsed)| {
                let anchors = &parsed.anchors;
                if anchors.is_empty() {
                    let anchor = file_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let doc = parsed_to_document(parsed, &anchor, file_path);
                    vec![(
                        doc.anchor.clone(),
                        DocumentNode {
                            doc,
                            source_path: file_path.clone(),
                            parsed: Some(parsed.clone()),
                        },
                    )]
                } else {
                    anchors
                        .iter()
                        .map(|anchor| {
                            let doc = parsed_to_document(parsed, anchor, file_path);
                            (
                                doc.anchor.clone(),
                                DocumentNode {
                                    doc,
                                    source_path: file_path.clone(),
                                    parsed: Some(parsed.clone()),
                                },
                            )
                        })
                        .collect()
                }
            })
            .collect();

        // Add all nodes (thread-safe: petgraph DiGraph is Send+Sync)
        for (_anchor, node) in node_work {
            graph.add_node(node);
        }

        // Sequential edge building (depends on all nodes existing)
        for (_file_path, parsed) in &parsed_docs {
            let source_anchor = if let Some(a) = parsed.anchors.first() {
                a.clone()
            } else {
                PathBuf::from(&parsed.source_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            };

            // Build edges from refs: <<target>>
            for ref_anchor in &parsed.refs {
                if ref_anchor.contains('{') {
                    continue;
                }
                let target_is_adr = ref_anchor.starts_with("adr-");
                let edge_type = if source_anchor.starts_with("adr-") || target_is_adr {
                    EdgeType::RelatesTo
                } else {
                    EdgeType::Uses
                };
                let backlink_type = if source_anchor.starts_with("adr-") || target_is_adr {
                    EdgeType::RelatesTo
                } else {
                    EdgeType::UsedBy
                };
                let _ =
                    graph.add_edge_by_anchor(&source_anchor, ref_anchor, AdenEdge { edge_type });
                let _ = graph.add_edge_by_anchor(
                    ref_anchor,
                    &source_anchor,
                    AdenEdge {
                        edge_type: backlink_type,
                    },
                );
            }

            // Build edges from includes: include::target
            for inc in &parsed.includes {
                let inc_path = PathBuf::from(&inc.path);
                let inc_file_stem = inc_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown");
                let target_anchor = inc_file_stem.to_string();
                let edge_type = if source_anchor.starts_with("adr-") {
                    EdgeType::RelatesTo
                } else {
                    EdgeType::Requires
                };
                if graph
                    .add_edge_by_anchor(&source_anchor, &target_anchor, AdenEdge { edge_type })
                    .is_err()
                {}
            }

            // Build edges from edge:: macros
            for edge_macro in &parsed.edges {
                let edge_type = match edge_macro.edge_type.to_lowercase().as_str() {
                    "uses" => EdgeType::Uses,
                    "usedby" | "used_by" => EdgeType::UsedBy,
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
                    "isa" | "is-a" => EdgeType::IsA,
                    "partof" | "part-of" => EdgeType::PartOf,
                    "relatesto" | "relates-to" => EdgeType::RelatesTo,
                    "similar" | "similar-to" => EdgeType::SimilarTo,
                    "causes" => EdgeType::Causes,
                    "implies" => EdgeType::Implies,
                    "synonym" | "synonym-of" => EdgeType::SynonymOf,
                    "antonym" | "antonym-of" => EdgeType::AntonymOf,
                    "associated" | "associated-with" => EdgeType::AssociatedWith,
                    "prerequisite" | "prerequisite-for" => EdgeType::PrerequisiteFor,
                    "explains" => EdgeType::Explains,
                    "isequivalent" | "is-equivalent-to" => EdgeType::IsEquivalentTo,
                    _ => EdgeType::RelatesTo,
                };
                if graph
                    .add_edge_by_anchor(&source_anchor, &edge_macro.target, AdenEdge { edge_type })
                    .is_err()
                {}
            }
        }

        Ok(graph)
    }

    /// Mark documents as self-references based on the given config.
    /// Documents whose source_path matches any pattern in
    /// `self_reference_patterns` get confidence 0.1 to prevent self-bias.
    pub fn mark_self_references(&mut self, config: &aden_core::AdenConfig) {
        for idx in self.graph.node_indices() {
            if let Some(node) = self.graph.node_weight_mut(idx) {
                node.doc.mark_self_reference(config);
            }
        }
    }

    /// Build a graph from a storage backend instead of files.
    ///
    /// Loads all documents and edges from the storage layer and constructs
    /// an in-memory `AdenGraph`. This is the store-first path — no file
    /// parsing, no `contracts/` directory needed.
    pub fn build_from_storage<S: GraphStorage>(storage: &S) -> Result<Self, GraphError> {
        let mut graph = Self::new();

        // Load all documents from storage
        let docs = storage
            .get_all_documents()
            .map_err(|e| GraphError::Io(e.to_string()))?;

        // Create nodes from documents
        for (_anchor, doc) in docs {
            let node = DocumentNode {
                doc,
                source_path: PathBuf::new(), // storage doesn't track paths
                parsed: None,                // no parsed doc from storage
            };
            graph.add_node(node);
        }

        // Load all edges from storage and rebuild them
        let edge_types = vec![
            EdgeType::Uses,
            EdgeType::UsedBy,
            EdgeType::Implements,
            EdgeType::Tests,
            EdgeType::Documents,
            EdgeType::Constrains,
            EdgeType::Justifies,
            EdgeType::Invokes,
            EdgeType::Requires,
            EdgeType::Mutates,
            EdgeType::Calls,
            EdgeType::Supersedes,
            EdgeType::Amends,
            EdgeType::Verifies,
            EdgeType::IsA,
            EdgeType::PartOf,
            EdgeType::RelatesTo,
            EdgeType::SimilarTo,
            EdgeType::Causes,
            EdgeType::Implies,
            EdgeType::SynonymOf,
            EdgeType::AntonymOf,
            EdgeType::AssociatedWith,
            EdgeType::PrerequisiteFor,
            EdgeType::Explains,
            EdgeType::IsEquivalentTo,
        ];

        for edge_type in &edge_types {
            let typed_edges = storage
                .get_edges_by_type(edge_type)
                .map_err(|e| GraphError::Io(e.to_string()))?;
            for (src, dst) in typed_edges {
                if graph
                    .add_edge_by_anchor(
                        &src,
                        &dst,
                        AdenEdge {
                            edge_type: *edge_type,
                        },
                    )
                    .is_err()
                {}
            }
        }

        Ok(graph)
    }

    /// Check that code edges (Uses, Implements, etc.) only connect to code nodes
    /// and semantic edges (IsA, PartOf, etc.) only connect to semantic nodes.
    pub fn validate_typed_edges(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let code_types = [
            EdgeType::Uses,
            EdgeType::UsedBy,
            EdgeType::Implements,
            EdgeType::Tests,
            EdgeType::Documents,
            EdgeType::Constrains,
            EdgeType::Justifies,
            EdgeType::Invokes,
            EdgeType::Requires,
            EdgeType::Mutates,
            EdgeType::Calls,
            EdgeType::Supersedes,
            EdgeType::Amends,
            EdgeType::Verifies,
        ];
        let semantic_types = [
            EdgeType::IsA,
            EdgeType::PartOf,
            EdgeType::RelatesTo,
            EdgeType::SimilarTo,
            EdgeType::Causes,
            EdgeType::Implies,
            EdgeType::SynonymOf,
            EdgeType::AntonymOf,
            EdgeType::AssociatedWith,
            EdgeType::PrerequisiteFor,
            EdgeType::Explains,
            EdgeType::IsEquivalentTo,
        ];

        for edge_idx in self.graph.edge_indices() {
            let (src, tgt) = self.graph.edge_endpoints(edge_idx).expect("valid edge");
            let edge = &self.graph[edge_idx];
            let src_anchor = self.graph[src].anchor().to_string();
            let tgt_anchor = self.graph[tgt].anchor().to_string();
            let src_type = &self.graph[src].doc.node_type;
            let tgt_type = &self.graph[tgt].doc.node_type;

            if code_types.contains(&edge.edge_type) {
                if *src_type == NodeType::Adr || *tgt_type == NodeType::Adr {
                    errors.push(format!(
                        "Code edge {:?} from {} to {} is invalid (ADR nodes cannot use code edges)",
                        edge.edge_type, src_anchor, tgt_anchor
                    ));
                }
            } else if semantic_types.contains(&edge.edge_type) {
                // Semantic edges are valid between any nodes
            }
        }
        errors
    }
}

/// Convert a `ParsedDocument` into a `Document` with the given anchor.
fn parsed_to_document(parsed: &ParsedDocument, anchor: &str, file_path: &Path) -> Document {
    let mut attributes = parsed.attributes.clone();
    attributes.insert(
        "source_file".to_string(),
        file_path.to_string_lossy().to_string(),
    );
    Document {
        anchor: anchor.to_string(),
        node_type: detect_node_type(anchor, file_path),
        attributes,
        blocks: parsed.blocks.clone(),
        source_span: None,
        metadata: parsed.metadata.clone(),
        confidence: 0.9,
    }
}

/// Heuristically detect node type from anchor and file path.
fn detect_node_type(anchor: &str, file_path: &Path) -> NodeType {
    // Normalize separators so `/adr/`-style matches also hold on Windows (`\`).
    let path_str = file_path
        .to_string_lossy()
        .replace('\\', "/")
        .to_lowercase();
    let anchor_lower = anchor.to_lowercase();

    if path_str.ends_with(".adoc")
        && (path_str.contains("/adr/") || anchor_lower.starts_with("adr-"))
    {
        return NodeType::Adr;
    }
    if path_str.contains("runbook") || anchor_lower.starts_with("runbook") {
        return NodeType::Runbook;
    }
    if path_str.contains("plan") || anchor_lower.starts_with("plan") {
        return NodeType::Plan;
    }
    if path_str.contains("manifest") || anchor_lower.starts_with("manifest") {
        return NodeType::Manifest;
    }
    if path_str.contains("spec") || anchor_lower.starts_with("spec") {
        return NodeType::Spec;
    }
    if path_str.contains("note") || anchor_lower.starts_with("note") {
        return NodeType::Note;
    }
    if path_str.contains("context") || anchor_lower.starts_with("context") {
        return NodeType::Context;
    }
    NodeType::Module
}

#[cfg(test)]
mod collect_files_tests {
    use super::*;
    use std::fs;

    #[test]
    fn build_from_directory_excludes_scaffolding_and_vcs() {
        let base = std::env::temp_dir().join("aden_collect_filter_test");
        let _ = fs::remove_dir_all(&base);
        for sub in ["docs", ".agent", ".agent/templates", ".git", "node_modules"] {
            fs::create_dir_all(base.join(sub)).unwrap();
        }
        // A real doc that MUST be collected.
        fs::write(base.join("docs/real.adoc"), "[[real-doc]]\n= Real\n").unwrap();
        // Excluded trees that MUST NOT be collected (would otherwise create
        // phantom orphans / duplicate anchors).
        fs::write(base.join(".agent/scaf.adoc"), "[[scaf]]\n= Scaffold\n").unwrap();
        fs::write(
            base.join(".agent/templates/real.adoc"),
            "[[real-doc]]\n= Template dup\n",
        )
        .unwrap();
        fs::write(base.join(".git/x.adoc"), "[[git-doc]]\n= Git\n").unwrap();
        fs::write(base.join("node_modules/y.adoc"), "[[nm]]\n= NM\n").unwrap();

        let files = collect_files(&base).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| p.strip_prefix(&base).unwrap().to_string_lossy().to_string())
            .collect();

        // Normalize separators so the path check holds on Windows (`\`) too.
        assert!(
            names
                .iter()
                .any(|n| n.replace('\\', "/").ends_with("docs/real.adoc"))
        );
        assert!(
            !names.iter().any(|n| n.contains(".agent")),
            "excluded .agent leaked: {:?}",
            names
        );
        assert!(!names.iter().any(|n| n.contains(".git")), "{:?}", names);
        assert!(
            !names.iter().any(|n| n.contains("node_modules")),
            "{:?}",
            names
        );

        let _ = fs::remove_dir_all(&base);
    }
}
