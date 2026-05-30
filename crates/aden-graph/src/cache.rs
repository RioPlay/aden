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
//! Uses the fjall (LSM-tree) store + Postcard for fast binary storage with git
//! ref tracking.
//! Stores graph keyed by content hash AND git ref, enabling:
//! - Fast incremental loads via content-key validation
//! - Time-travel queries via git ref lookup
//! - Version-aware context assembly
//!
//! Cache structure:
//!   .aden/cache/
//!   ├── sled/              # Sled database (Postcard)
//!   └── refs/              # Git-ref snapshots for time-travel

use crate::bridge::GraphBridge;
use crate::graph::AdenGraph;
use crate::nodes::{AdenEdge, DocumentNode, GraphNode};
use aden_core::{Document, EdgeType};
use aden_store::{GraphStorage, Storage};
use blake3::Hasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

const CACHE_DIR: &str = ".aden/cache";
const REFS_DIR: &str = "refs";
const SLED_DB: &str = "sled";

/// Get current git ref (commit hash, branch, or tag) for the repository.
fn get_current_git_ref(dir: &Path) -> Option<String> {
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

/// Build a stable content hash for the set of knowledge files in `dir`.
fn compute_content_hash(dir: &Path) -> Result<String, std::io::Error> {
    let mut hasher = Hasher::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        // SECURITY: never follow symlinks (traversal / cycle DoS on untrusted repos).
        if entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(true) {
            continue;
        }
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if (ext == "adoc" || ext == "aden" || ext == "md" || ext == "txt") && path.is_file() {
                paths.push(path);
            } else if path.is_dir() {
                if let Ok(sub) = walk_knowledge_files(&path) {
                    paths.extend(sub);
                }
            }
        } else if path.is_dir()
            && let Ok(sub) = walk_knowledge_files(&path)
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

fn walk_knowledge_files(dir: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    walk_knowledge_files_inner(dir, 0)
}

fn walk_knowledge_files_inner(dir: &Path, depth: usize) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut paths = Vec::new();
    // SECURITY: bound recursion and never follow symlinks — an untrusted repo
    // could otherwise cycle (DoS) or escape the tree.
    if depth > 32 {
        return Ok(paths);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(true) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            paths.extend(walk_knowledge_files_inner(&path, depth + 1)?);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && (ext == "adoc" || ext == "aden" || ext == "md" || ext == "txt")
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

fn sled_db_path(path: &Path) -> PathBuf {
    cache_dir_for(path).join(SLED_DB)
}

/// Try loading a cached graph from Sled.
/// Checks in order: 1) contracts store (.aden/store), 2) legacy cache (.aden/cache/sled)
pub fn try_load(path: &Path) -> Option<AdenGraph<DocumentNode, AdenEdge>> {
    // 1. Try contracts store first (store-first architecture)
    let store_path = path.join(".aden").join("store");
    if store_path.exists() {
        if let Ok(storage) = Storage::new(store_path.to_str()?) {
            if let Ok((docs, edges)) = GraphBridge::load_from_storage(&storage) {
                if !docs.is_empty() {
                    return Some(build_graph_from_docs_and_edges(docs, edges));
                }
            }
        }
    }

    // 2. Fall back to legacy cache
    let sled_path = sled_db_path(path);
    if !sled_path.exists() {
        return None;
    }

    if let Ok(storage) = Storage::new(sled_path.to_str()?) {
        let current_hash = compute_content_hash(path).ok()?;
        let stored_hash = GraphBridge::load_meta(&storage, "content_hash").ok()??;

        if stored_hash == current_hash {
            let (docs, edges) = GraphBridge::load_from_storage(&storage).ok()?;
            return Some(build_graph_from_docs_and_edges(docs, edges));
        }
    }

    None
}

/// Save a fully built graph to Sled + JSON with git ref tracking.
pub fn save(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let cache_dir = cache_dir_for(path);
    let sled_path = sled_db_path(path);
    let refs_dir = refs_dir_for(path);
    std::fs::create_dir_all(&cache_dir)?;
    std::fs::create_dir_all(&refs_dir)?;

    // Extract docs and edges for storage
    let mut docs = HashMap::new();
    let mut edge_tuples = Vec::new();

    for node_idx in graph.graph.node_indices() {
        let node = &graph.graph[node_idx];
        let anchor = node.anchor().to_string();
        docs.insert(anchor.clone(), node.doc.clone());
    }

    for edge_idx in graph.graph.edge_indices() {
        let (src, tgt) = graph.graph.edge_endpoints(edge_idx).expect("valid edge");
        let src_node = &graph.graph[src];
        let tgt_node = &graph.graph[tgt];
        let edge_type = &graph.graph[edge_idx];
        let src_anchor = src_node.anchor().to_string();
        let tgt_anchor = tgt_node.anchor().to_string();
        edge_tuples.push((src_anchor, tgt_anchor, edge_type.edge_type.clone()));
    }

    // Get current git ref
    let git_ref = get_current_git_ref(path);
    let content_hash = compute_content_hash(path)?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string());

    // Save to Sled
    let storage = Storage::new(sled_path.to_str().ok_or("invalid path")?)?;
    GraphBridge::sync_to_storage(&storage, &docs, &edge_tuples)?;
    GraphBridge::save_meta(&storage, "content_hash", &content_hash)?;
    GraphBridge::save_meta(&storage, "last_updated", &timestamp)?;
    if let Some(ref ref_) = git_ref {
        GraphBridge::save_meta(&storage, "git_ref", ref_)?;
    }
    drop(storage);

    Ok(())
}

fn build_graph_from_docs_and_edges(
    docs: HashMap<String, Document>,
    edges: Vec<(String, String, EdgeType)>,
) -> AdenGraph<DocumentNode, AdenEdge> {
    use petgraph::graph::DiGraph;
    let mut graph = DiGraph::new();
    let mut anchor_to_index: HashMap<String, petgraph::graph::NodeIndex> = HashMap::new();
    let mut path_to_index: HashMap<PathBuf, petgraph::graph::NodeIndex> = HashMap::new();

    // First pass: insert nodes
    for (anchor, doc) in docs {
        let source_path = doc
            .attributes
            .get("source_file")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("{}.adoc", anchor)));

        let node = DocumentNode {
            doc: doc.clone(),
            source_path: source_path.clone(),
            parsed: None,
        };
        let idx = graph.add_node(node);
        anchor_to_index.insert(anchor.clone(), idx);
        path_to_index.insert(source_path, idx);
    }

    // Second pass: insert edges
    for (src, dst, edge_type) in edges {
        if let (Some(&src_idx), Some(&dst_idx)) =
            (anchor_to_index.get(&src), anchor_to_index.get(&dst))
        {
            graph.add_edge(src_idx, dst_idx, AdenEdge { edge_type });
        }
    }

    AdenGraph {
        graph,
        anchor_to_index,
        path_to_index,
        backlinks_cache: None,
    }
}

/// Resolve a user anchor against the store without loading the whole graph.
///
/// Returns the anchor unchanged if it exists exactly; otherwise resolves a bare
/// symbol/module name to a single full anchor by `#suffix` match (reading only
/// the anchor *keys*). Returns `None` if unknown or ambiguous — callers should
/// treat that as "not found" rather than guessing.
pub fn resolve_anchor_in_store(dir: &Path, anchor: &str) -> Option<String> {
    let store_path = dir.join(".aden").join("store");
    let storage = Storage::new(store_path.to_str()?).ok()?;
    if matches!(storage.get_document(anchor), Ok(Some(_))) {
        return Some(anchor.to_string());
    }
    let anchors = storage.get_all_anchors().ok()?;
    if anchors.contains(anchor) {
        return Some(anchor.to_string());
    }
    let matches: Vec<&String> = anchors
        .iter()
        .filter(|a| a.rsplit('#').next() == Some(anchor))
        .collect();
    if matches.len() == 1 {
        Some(matches[0].clone())
    } else {
        None
    }
}

/// Build a graph containing only the neighborhood reachable from `start` within
/// `depth`, following `edge_types` (empty = all). This is the streaming read
/// path: instead of loading the entire store into a petgraph (which OOMs / takes
/// tens of seconds at kernel scale), it walks per-node adjacency lists from the
/// start anchor and fetches only the documents it actually visits.
///
/// A hard node cap bounds pathological fan-out (e.g. starting at a module that
/// contains thousands of symbols), mirroring the assembler's own DoS guard.
pub fn build_neighborhood_cached(
    dir: &Path,
    start: &str,
    depth: usize,
    edge_types: &[EdgeType],
) -> Result<AdenGraph<DocumentNode, AdenEdge>, crate::graph::GraphError> {
    const MAX_NODES: usize = 10_000;
    let store_path = dir.join(".aden").join("store");
    let storage = Storage::new(
        store_path
            .to_str()
            .ok_or_else(|| crate::graph::GraphError::Io("invalid store path".into()))?,
    )
    .map_err(|e| crate::graph::GraphError::Io(e.to_string()))?;

    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(start.to_string());
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((start.to_string(), 0));
    let mut edges: Vec<(String, String, EdgeType)> = Vec::new();

    while let Some((current, d)) = queue.pop_front() {
        if d >= depth || visited.len() >= MAX_NODES {
            continue;
        }
        let outgoing = storage.get_outgoing_edges(&current).unwrap_or_default();
        for (nbr, et) in outgoing {
            if !edge_types.is_empty() && !edge_types.contains(&et) {
                continue;
            }
            edges.push((current.clone(), nbr.clone(), et));
            if visited.insert(nbr.clone()) && visited.len() <= MAX_NODES {
                queue.push_back((nbr, d + 1));
            }
        }
    }

    // Fetch only the documents we actually visited.
    let mut docs: HashMap<String, Document> = HashMap::new();
    for anchor in &visited {
        if let Ok(Some(doc)) = storage.get_document(anchor) {
            docs.insert(anchor.clone(), doc);
        }
    }

    Ok(build_graph_from_docs_and_edges(docs, edges))
}

/// Build a graph, using the on-disk cache when possible.
pub fn build_from_directory_cached(
    dir: &Path,
) -> Result<AdenGraph<DocumentNode, AdenEdge>, crate::graph::GraphError> {
    if let Some(cached) = try_load(dir) {
        return Ok(cached);
    }
    let graph = crate::graph::AdenGraph::build_from_directory(dir)?;
    let _ = save(&graph, dir);
    Ok(graph)
}
