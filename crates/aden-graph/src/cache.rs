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
//! Store-first: the graph is read directly from the fjall (LSM-tree) store at
//! `.aden/store`, which `aden gen` writes. There is no separate cache database —
//! the store IS the cache.

use crate::bridge::GraphBridge;
use crate::graph::AdenGraph;
use crate::nodes::{AdenEdge, DocumentNode};
use aden_core::{Document, EdgeType};
use aden_store::{GraphStorage, Storage};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// Try loading the graph from the store-first fjall store (`.aden/store`).
pub fn try_load(path: &Path) -> Option<AdenGraph<DocumentNode, AdenEdge>> {
    // ADR-003: read the per-user central store (or a legacy in-tree store, with
    // a deprecation notice); never create one on a read.
    let (store_path, is_legacy) = aden_paths::resolve_read_store(path);
    if store_path.exists() {
        if is_legacy {
            eprintln!("{}", aden_paths::legacy_notice(path));
        }
        if let Ok(storage) = Storage::open_existing(store_path.to_str()?)
            && let Ok((docs, edges)) = GraphBridge::load_from_storage(&storage)
            && !docs.is_empty()
        {
            return Some(build_graph_from_docs_and_edges(docs, edges, path));
        }
    }
    None
}

fn build_graph_from_docs_and_edges(
    docs: HashMap<String, Document>,
    edges: Vec<(String, String, EdgeType)>,
    root: &Path,
) -> AdenGraph<DocumentNode, AdenEdge> {
    use petgraph::graph::DiGraph;
    let mut graph = DiGraph::new();
    let mut anchor_to_index: HashMap<String, petgraph::graph::NodeIndex> = HashMap::new();
    let mut path_to_index: HashMap<PathBuf, petgraph::graph::NodeIndex> = HashMap::new();

    // Intent overlays: fold durable [human]/[agent] blocks into the in-memory
    // document so the context-assembling readers (asm/ask) surface them. The store
    // stays pure-generated (so the reconcile base is clean); folding is in-memory
    // only. The slug set is read once so this is free when no overlays exist.
    let overlay_slugs = aden_core::overlay::overlay_slugs(root);

    // First pass: insert nodes
    for (anchor, mut doc) in docs {
        if !overlay_slugs.is_empty()
            && overlay_slugs.contains(&aden_core::overlay::sanitize_anchor_filename(&anchor))
        {
            aden_core::overlay::fold_overlay(root, &anchor, &mut doc);
        }
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
    let (store_path, _) = aden_paths::resolve_read_store(dir);
    let storage = Storage::open_existing(store_path.to_str()?).ok()?;
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
/// True for module-index "hub" anchors that have O(hundreds) of edges each.
/// These are useful as the root (seed) of a traversal but cause an O(N²) node
/// explosion when traversed as intermediaries — e.g. depth-3 BFS through a
/// `mod-aden-core` node visits every symbol in that crate, inflating results
/// from ~60 to >1000 lines with low-value index content.
fn is_hub_anchor(anchor: &str) -> bool {
    let a = anchor.to_lowercase();
    // Legacy short-form module index anchors.
    if a.starts_with("mod-") || a.starts_with("module-") {
        return true;
    }
    // Store-scheme module index files (the crate root's mod.rs / lib.rs / main.rs).
    // e.g. aden://module/aden-core/mod.rs  aden://module/aden-core/lib.rs
    if a.starts_with("aden://module/") {
        let path_part = a.trim_start_matches("aden://module/");
        // No '#' means this is a file-level document (not a symbol), which tends
        // to be a high-fanout index node.
        if !path_part.contains('#') {
            return true;
        }
        // Symbol inside a module-root file is also likely a hub.
        if path_part.contains("/mod.rs#")
            || path_part.contains("/lib.rs#")
            || path_part.contains("/main.rs#")
        {
            return true;
        }
    }
    false
}

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
    let (store_path, _) = aden_paths::resolve_read_store(dir);
    let storage = Storage::open_existing(
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
        // Hub nodes (module-index anchors: `mod-*`, `module-*`, `aden://module/<crate>/mod.rs`)
        // have O(hundreds) edges each. When reached as an INTERMEDIATE node (d>0)
        // they explode the BFS from ~60 to ~1000+ nodes at depth 3 while adding
        // low-value index content. Allow them as the seed (d==0) but stop
        // expanding them as intermediaries.
        if d > 0 && is_hub_anchor(&current) {
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

    Ok(build_graph_from_docs_and_edges(docs, edges, dir))
}

/// Build a graph, using the on-disk cache when possible.
pub fn build_from_directory_cached(
    dir: &Path,
) -> Result<AdenGraph<DocumentNode, AdenEdge>, crate::graph::GraphError> {
    if let Some(cached) = try_load(dir) {
        return Ok(cached);
    }
    crate::graph::AdenGraph::build_from_directory(dir)
}
