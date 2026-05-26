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
//! Disk cache for the Aden knowledge graph.
//!
//! Indexed JSON cache with git integration for time-travel queries.
//! Stores graph keyed by content hash AND git ref, enabling:
//! - Fast incremental loads via content-key validation
//! - Time-travel queries via git ref lookup
//! - Version-aware context assembly
//!
//! Cache structure:
//!   .aden/cache/
//!   ├── graph-cache.json    # HEAD cache (indexed by content hash)
//!   ├── graph-index.json   # Metadata: version, refs, anchors
//!   └── refs/              # Git-ref snapshots for time-travel

use crate::graph::{AdenGraph, DocumentNode};
use crate::parser::parse_file;
use aden_core::{Document, EdgeType};
use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

const CACHE_DIR: &str = ".aden/cache";
const CACHE_FILE: &str = "graph-cache.json";
const INDEX_FILE: &str = "graph-index.json";
const REFS_DIR: &str = "refs";

/// Edge representation for the indexed graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedEdge {
    source: String,
    target: String,
    edge_type: EdgeType,
}

/// Indexed graph snapshot with anchors for fast lookup.
#[derive(Debug, Serialize, Deserialize)]
struct IndexedGraph {
    /// Metadata about this cache snapshot
    meta: CacheMetadata,
    /// Anchor-indexed nodes for fast lookup
    anchors: HashMap<String, AnchorEntry>,
    /// All edges in the graph
    edges: Vec<CachedEdge>,
}

/// Metadata about the cached graph version
#[derive(Debug, Serialize, Deserialize)]
struct CacheMetadata {
    /// Cache format version
    version: String,
    /// Git ref (commit, branch, tag) this cache represents
    git_ref: Option<String>,
    /// Content hash of source files
    content_hash: String,
    /// Timestamp of last update
    last_updated: String,
    /// Total anchor count
    anchor_count: usize,
}

/// Individual anchor entry for indexed lookup
#[derive(Debug, Serialize, Deserialize)]
struct AnchorEntry {
    anchor: String,
    doc: Document,
    source_path: PathBuf,
}

/// Get current git ref (commit hash, branch, or tag) for the repository.
fn get_current_git_ref(dir: &Path) -> Option<String> {
    // Try to get current commit hash
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .ok()?;

    if output.status.success() {
        let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if hash.len() >= 7 {
            Some(hash[..7].to_string())
        } else {
            Some(hash)
        }
    } else {
        None
    }
}

/// Build a stable content hash for the set of `.adoc`/`.aden` files in `dir`.
fn compute_content_hash(dir: &Path) -> Result<String, std::io::Error> {
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
        } else if path.is_dir()
            && let Ok(sub) = walk_adoc_files(&path)
        {
            paths.extend(sub);
        }
    }
    paths.sort();
    for p in &paths {
        hasher.update(p.to_string_lossy().as_bytes());
        if let Ok(meta) = std::fs::metadata(p)
            && let Ok(mtime) = meta.modified()
        {
            hasher.update(
                &mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
                    .to_le_bytes(),
            );
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
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && (ext == "adoc" || ext == "aden")
        {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn cache_dir_for(path: &Path) -> PathBuf {
    path.join(CACHE_DIR)
}

fn refs_dir_for(path: &Path) -> PathBuf {
    cache_dir_for(path).join(REFS_DIR)
}

/// Load graph by specific git ref for time-travel queries.
/// Returns None if no cache exists for that ref.
pub fn try_load_at_ref(path: &Path, git_ref: &str) -> Option<AdenGraph> {
    let refs_dir = refs_dir_for(path);
    let ref_cache = refs_dir.join(format!("{}.json", git_ref));

    if !ref_cache.exists() {
        return None;
    }

    let indexed: IndexedGraph =
        serde_json::from_str(&std::fs::read_to_string(&ref_cache).ok()?).ok()?;
    Some(build_graph_from_indexed(indexed))
}

/// Try loading a cached graph; returns `None` if missing or stale.
pub fn try_load(path: &Path) -> Option<AdenGraph> {
    let cache_dir = cache_dir_for(path);
    let current_hash = compute_content_hash(path).ok()?;
    let index_path = cache_dir.join(INDEX_FILE);
    let graph_path = cache_dir.join(CACHE_FILE);

    if !graph_path.exists() || !index_path.exists() {
        return None;
    }

    let stored_index: HashMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(&index_path).ok()?).ok()?;

    if stored_index.get("content_hash") != Some(&current_hash) {
        return None;
    }

    let indexed: IndexedGraph =
        serde_json::from_str(&std::fs::read_to_string(&graph_path).ok()?).ok()?;
    Some(build_graph_from_indexed(indexed))
}

/// Save a fully built graph to disk cache with git ref tracking.
pub fn save(graph: &AdenGraph, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let cache_dir = cache_dir_for(path);
    let refs_dir = refs_dir_for(path);
    std::fs::create_dir_all(&cache_dir)?;
    std::fs::create_dir_all(&refs_dir)?;

    // Build indexed structure for O(1) anchor lookup
    let mut anchors = HashMap::new();
    let mut edges = Vec::new();
    let mut anchor_count = 0;

    for node_idx in graph.graph.node_indices() {
        let node = &graph.graph[node_idx];
        anchor_count += 1;
        anchors.insert(
            node.anchor.clone(),
            AnchorEntry {
                anchor: node.anchor.clone(),
                doc: node.doc.clone(),
                source_path: node.source_path.clone(),
            },
        );
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

    // Get current git ref
    let git_ref = get_current_git_ref(path);
    let content_hash = compute_content_hash(path)?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string());

    // Create indexed graph
    let meta = CacheMetadata {
        version: "1.0".to_string(),
        git_ref: git_ref.clone(),
        content_hash: content_hash.clone(),
        last_updated: timestamp.clone(),
        anchor_count,
    };

    let indexed = IndexedGraph {
        meta,
        anchors,
        edges,
    };

    // Save main cache (HEAD)
    let graph_json = serde_json::to_string_pretty(&indexed)?;
    let graph_path = cache_dir.join(CACHE_FILE);
    let mut file = std::fs::File::create(&graph_path)?;
    file.write_all(graph_json.as_bytes())?;

    // Update index with current hash
    let mut index: HashMap<String, String> = HashMap::new();
    index.insert("content_hash".to_string(), content_hash);
    index.insert("last_updated".to_string(), timestamp);
    if let Some(ref ref_) = git_ref {
        index.insert("git_ref".to_string(), ref_.clone());
    }
    let index_json = serde_json::to_string_pretty(&index)?;
    let index_path = cache_dir.join(INDEX_FILE);
    let mut file = std::fs::File::create(&index_path)?;
    file.write_all(index_json.as_bytes())?;

    // Save to refs directory for time-travel if we have a git ref
    if let Some(ref ref_) = git_ref {
        let ref_json = serde_json::to_string_pretty(&indexed)?;
        let ref_path = refs_dir.join(format!("{}.json", ref_));
        let mut file = std::fs::File::create(&ref_path)?;
        file.write_all(ref_json.as_bytes())?;
    }

    Ok(())
}

fn build_graph_from_indexed(indexed: IndexedGraph) -> AdenGraph {
    use petgraph::graph::DiGraph;
    let mut graph = DiGraph::new();
    let mut anchor_to_index: HashMap<String, petgraph::graph::NodeIndex> = HashMap::new();
    let mut path_to_index: HashMap<PathBuf, petgraph::graph::NodeIndex> = HashMap::new();

    // First pass: insert nodes from indexed anchors
    for (_anchor, entry) in indexed.anchors {
        let node = DocumentNode {
            anchor: entry.anchor.clone(),
            doc: entry.doc,
            parsed: parse_file(&entry.source_path).unwrap_or_else(|_| {
                crate::parser::ParsedDocument {
                    source_path: entry.source_path.to_string_lossy().to_string(),
                    attributes: HashMap::new(),
                    anchors: vec![entry.anchor.clone()],
                    refs: Vec::new(),
                    includes: Vec::new(),
                    edges: Vec::new(),
                    conditional_stack: Vec::new(),
                    raw_content: String::new(),
                    semantic_diffs: Vec::new(),
                    blocks: Vec::new(),
                    tagged_regions: Vec::new(),
                    conditional_regions: Vec::new(),
                    metadata: None,
                }
            }),
            source_path: entry.source_path.clone(),
        };
        let idx = graph.add_node(node);
        anchor_to_index.insert(entry.anchor.clone(), idx);
        path_to_index.insert(entry.source_path, idx);
    }

    // Second pass: insert edges
    for ce in indexed.edges {
        if let (Some(&src), Some(&tgt)) = (
            anchor_to_index.get(&ce.source),
            anchor_to_index.get(&ce.target),
        ) {
            graph.add_edge(src, tgt, ce.edge_type);
        }
    }

    AdenGraph {
        graph,
        anchor_to_index,
        path_to_index,
        filter: aden_core::filter::AdenFilter::from_directory(Path::new(".")),
        backlinks_cache: None,
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
