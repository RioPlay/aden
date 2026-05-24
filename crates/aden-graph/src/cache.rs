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
//! Disk cache for the Aden knowledge graph.
//!
//! Serialises the graph to JSON keyed by a content hash of all `.adoc`/`.aden`
//! files in the watched directory. On subsequent runs the cache is only rebuilt
//! when the stored hash no longer matches, or when `aden gen` has emitted new
//! contracts.

use crate::graph::{AdenGraph, DocumentNode};
use crate::parser::parse_file;
use aden_core::{Document, EdgeType};
use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

const CACHE_DIR: &str = ".aden/cache";
const GRAPH_CACHE_FILE: &str = "graph-cache.json";
const INDEX_FILE: &str = "cache-index.json";

/// A serialisable snapshot of a graph node.
#[derive(Debug, Serialize, Deserialize)]
struct CachedNode {
    anchor: String,
    doc: Document,
    source_path: PathBuf,
}

/// A serialisable snapshot of a graph edge.
#[derive(Debug, Serialize, Deserialize)]
struct CachedEdge {
    source: String,
    target: String,
    edge_type: EdgeType,
}

/// A serialisable graph snapshot.
#[derive(Debug, Serialize, Deserialize)]
struct CachedGraph {
    nodes: Vec<CachedNode>,
    edges: Vec<CachedEdge>,
}

/// Build a stable content hash for the set of `.adoc`/`.aden` files in `dir`.
fn compute_cache_key(dir: &Path) -> Result<String, std::io::Error> {
    let mut hasher = Hasher::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if (ext == "adoc" || ext == "aden") && path.is_file() {
                paths.push(path);
            } else if path.is_dir() {
                // recurse — cheap since aden graphs are shallow
                if let Ok(sub) = walk_adoc_files(&path) {
                    paths.extend(sub);
                }
            }
        } else if path.is_dir() {
            if let Ok(sub) = walk_adoc_files(&path) {
                paths.extend(sub);
            }
        }
    }
    paths.sort();
    for p in &paths {
        hasher.update(p.to_string_lossy().as_bytes());
        if let Ok(meta) = std::fs::metadata(p) {
            if let Ok(mtime) = meta.modified() {
                hasher.update(&mtime.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos().to_le_bytes());
            }
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn walk_adoc_files(dir: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            paths.extend(walk_adoc_files(&path)?);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if ext == "adoc" || ext == "aden" {
                paths.push(path);
            }
        }
    }
    Ok(paths)
}

fn cache_dir_for(path: &Path) -> PathBuf {
    path.join(CACHE_DIR)
}

/// Try loading a cached graph; returns `None` if missing or stale.
pub fn try_load(path: &Path) -> Option<AdenGraph> {
    let cache_dir = cache_dir_for(path);
    let current_key = compute_cache_key(path).ok()?;
    let index_path = cache_dir.join(INDEX_FILE);
    let graph_path = cache_dir.join(GRAPH_CACHE_FILE);

    if !graph_path.exists() || !index_path.exists() {
        return None;
    }

    let stored_index: HashMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(&index_path).ok()?).ok()?;

    if stored_index.get("graph_key") != Some(&current_key) {
        return None;
    }

    let cg: CachedGraph = serde_json::from_str(&std::fs::read_to_string(&graph_path).ok()?).ok()?;
    Some(build_graph_from_cache(cg))
}

/// Save a fully built graph to disk cache.
pub fn save(graph: &AdenGraph, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let cache_dir = cache_dir_for(path);
    std::fs::create_dir_all(&cache_dir)?;

    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    for node_idx in graph.graph.node_indices() {
        let node = &graph.graph[node_idx];
        nodes.push(CachedNode {
            anchor: node.anchor.clone(),
            doc: node.doc.clone(),
            source_path: node.source_path.clone(),
        });
    }

    for edge_idx in graph.graph.edge_indices() {
        let (src, tgt) = graph.graph.edge_endpoints(edge_idx).expect("valid edge");
        let src_node = &graph.graph[src];
        let tgt_node = &graph.graph[tgt];
        let edge_type = graph.graph[edge_idx];
        edges.push(CachedEdge {
            source: src_node.anchor.clone(),
            target: tgt_node.anchor.clone(),
            edge_type,
        });
    }

    let cg = CachedGraph { nodes, edges };
    let graph_json = serde_json::to_string_pretty(&cg)?;
    let graph_path = cache_dir.join(GRAPH_CACHE_FILE);
    let mut file = std::fs::File::create(&graph_path)?;
    file.write_all(graph_json.as_bytes())?;

    let key = compute_cache_key(path)?;
    let mut index: HashMap<String, String> = HashMap::new();
    index.insert("graph_key".to_string(), key);
    let index_json = serde_json::to_string_pretty(&index)?;
    let index_path = cache_dir.join(INDEX_FILE);
    let mut file = std::fs::File::create(&index_path)?;
    file.write_all(index_json.as_bytes())?;

    Ok(())
}

fn build_graph_from_cache(cg: CachedGraph) -> AdenGraph {
    use petgraph::graph::DiGraph;
    let mut graph = DiGraph::new();
    let mut anchor_to_index: HashMap<String, petgraph::graph::NodeIndex> = HashMap::new();
    let mut path_to_index: HashMap<PathBuf, petgraph::graph::NodeIndex> = HashMap::new();

    // First pass: insert nodes
    for cn in cg.nodes {
        let node = DocumentNode {
            anchor: cn.anchor.clone(),
            doc: cn.doc,
            parsed: parse_file(&cn.source_path).unwrap_or_else(|_| crate::parser::ParsedDocument {
                source_path: cn.source_path.to_string_lossy().to_string(),
                attributes: HashMap::new(),
                anchors: vec![cn.anchor.clone()],
                refs: Vec::new(),
                includes: Vec::new(),
                edges: Vec::new(),
                conditional_stack: Vec::new(),
                raw_content: String::new(),
                semantic_diffs: Vec::new(),
                blocks: Vec::new(),
            }),
            source_path: cn.source_path.clone(),
        };
        let idx = graph.add_node(node);
        anchor_to_index.insert(cn.anchor.clone(), idx);
        path_to_index.insert(cn.source_path, idx);
    }

    // Second pass: insert edges
    for ce in cg.edges {
        if let (Some(&src), Some(&tgt)) = (anchor_to_index.get(&ce.source), anchor_to_index.get(&ce.target)) {
            graph.add_edge(src, tgt, ce.edge_type);
        }
    }

    AdenGraph {
        graph,
        anchor_to_index,
        path_to_index,
        filter: aden_core::filter::AdenFilter::from_directory(Path::new(".")),
    }
}

/// Build a graph, using the on-disk cache when possible.
pub fn build_from_directory_cached(dir: &Path) -> Result<AdenGraph, crate::graph::GraphError> {
    if let Some(cached) = try_load(dir) {
        return Ok(cached);
    }
    let graph = crate::graph::AdenGraph::build_from_directory(dir)?;
    let _ = save(&graph, dir);
    Ok(graph)
}
