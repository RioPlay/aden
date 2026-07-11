// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Disk cache for the Aden knowledge graph.
//!
//! Store-first: the graph is read from the ADR-011 read snapshot when present
//! and fresh, otherwise from the fjall (LSM-tree) store at `store/`, which
//! `aden gen` writes.

use crate::bridge::GraphBridge;
use crate::graph::AdenGraph;
use crate::nodes::{AdenEdge, DocumentNode};
use crate::snapshot;
use aden_core::{Document, EdgeType};
use aden_store::{GraphStorage, Storage};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// Try loading the graph for read commands (ADR-011 snapshot-first, fjall fallback).
pub fn try_load(path: &Path) -> Option<AdenGraph<DocumentNode, AdenEdge>> {
    let (store_path, is_legacy) = aden_paths::resolve_read_store(path);
    if is_legacy {
        eprintln!("{}", aden_paths::legacy_notice(path));
    }

    if let Some((docs, edges)) = snapshot::try_read_fresh(path) {
        return Some(build_graph_from_docs_and_edges(docs, edges, path));
    }

    if store_path.exists()
        && let Ok(storage) = Storage::open_existing(store_path.to_str()?)
        && let Ok((docs, edges)) = GraphBridge::load_from_storage(&storage)
        && !docs.is_empty()
    {
        return Some(build_graph_from_docs_and_edges(docs, edges, path));
    }
    None
}

fn build_graph_from_docs_and_edges(
    docs: HashMap<String, Document>,
    edges: Vec<(String, String, EdgeType)>,
    root: &Path,
) -> AdenGraph<DocumentNode, AdenEdge> {
    // Use AdenGraph wrapper to enforce dedup (M3) and populate indices.
    let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();

    // Intent overlays: fold durable [human]/[agent] blocks into the in-memory
    // document so the context-assembling readers (asm/ask) surface them. The store
    // stays pure-generated (so the reconcile base is clean); folding is in-memory
    // only. The slug set is read once so this is free when no overlays exist.
    let overlay_slugs = aden_core::overlay::overlay_slugs(root);

    // First pass: insert nodes (now enforces no duplicate anchors)
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
        let _ = graph.add_node(node); // duplicates shouldn't occur; ignored for build
    }

    // Second pass: insert edges using public API (enforces value dedup)
    for (src, dst, edge_type) in edges {
        let _ = graph.add_edge_by_anchor(&src, &dst, AdenEdge { edge_type });
    }

    graph
}

/// Resolve a user anchor against the store without loading the whole graph.
///
/// Returns the anchor unchanged if it exists exactly; otherwise resolves a bare
/// symbol/module name to a single full anchor by `#suffix` match (reading only
/// the anchor *keys*). Returns `None` if unknown or ambiguous — callers should
/// treat that as "not found" rather than guessing.
pub fn resolve_anchor_in_store(dir: &Path, anchor: &str) -> Option<String> {
    // ADR-011: snapshot-first (lock-free) when a fresh graph.snapshot covers the store.
    // Critical for concurrent readers (MCP + heal/merge --fix, ask/asm while gen runs).
    // Resolve directly against the document map. The old path called
    // `try_load`, materializing every node and edge into petgraph merely to ask
    // whether one key existed; `ask` then loaded the snapshot a second time for
    // neighborhood assembly. On documentation-heavy repos that duplicate build
    // dominated request latency.
    if let Some((docs, _)) = snapshot::try_read_fresh(dir) {
        if docs.contains_key(anchor) {
            return Some(anchor.to_string());
        }
        let matches: Vec<&String> = docs
            .keys()
            .filter(|candidate| candidate.rsplit('#').next() == Some(anchor))
            .collect();
        if matches.len() == 1 {
            return Some(matches[0].to_string());
        }
        return None;
    }

    // Fallback to direct fjall (may see Locked under active readers/writers).
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
    build_neighborhood_impl(dir, start, depth, edge_types, &[], &|_, _| true)
}

/// [`build_neighborhood_cached`] plus the seed's INCOMING `caller_types`
/// neighbors (its callers/users) folded in as a depth-1 frontier. Outgoing-only
/// traversal starves at high-in-degree leaves — a constructor or called-into
/// symbol with few resolved callees yields a two-node neighborhood while all
/// its context sits on incoming edges. The callers are attached with a
/// REVERSED edge (`seed -> caller`) so the assembler's outgoing-only walk
/// reaches them; the store's true edge direction is untouched — the reversal
/// exists only in this ephemeral assembly graph. `caller_filter` lets the
/// caller veto individual anchors (e.g. test fixtures) without this layer
/// having to know any such policy; it receives the candidate's anchor AND its
/// `source_file` attribute, because module-form anchors flatten the directory
/// and can hide e.g. a `tests/` path segment.
pub fn build_neighborhood_with_callers(
    dir: &Path,
    start: &str,
    depth: usize,
    edge_types: &[EdgeType],
    caller_types: &[EdgeType],
    caller_filter: &dyn Fn(&str, Option<&str>) -> bool,
) -> Result<AdenGraph<DocumentNode, AdenEdge>, crate::graph::GraphError> {
    build_neighborhood_impl(dir, start, depth, edge_types, caller_types, caller_filter)
}

/// Bound on folded-in callers: a popular utility can have hundreds; the first
/// `MAX_CALLERS` in deterministic (anchor-sorted) order are enough context.
const MAX_CALLERS: usize = 16;

/// Measured out-degree above which a name-flagged hub anchor is treated as a
/// real hub and not expanded as an intermediate node.
const HUB_DEGREE_CAP: usize = 32;

fn build_neighborhood_from_materialized(
    (docs, all_edges): snapshot::SnapshotData,
    dir: &Path,
    start: &str,
    depth: usize,
    edge_types: &[EdgeType],
    caller_types: &[EdgeType],
    caller_filter: &dyn Fn(&str, Option<&str>) -> bool,
) -> Result<AdenGraph<DocumentNode, AdenEdge>, crate::graph::GraphError> {
    let mut outgoing: HashMap<String, Vec<(String, EdgeType)>> = HashMap::new();
    // Only callers of the seed can ever be folded in. The old implementation
    // cloned every edge into a second, whole-graph incoming map even when
    // `caller_types` was empty (the common ask/asm path). Move owned strings
    // into the outgoing map and retain this one targeted incoming list instead.
    let mut incoming_start: Vec<(String, EdgeType)> = Vec::new();
    for (src, dst, et) in all_edges {
        if !caller_types.is_empty() && dst == start && caller_types.contains(&et) {
            incoming_start.push((src.clone(), et));
        }
        outgoing.entry(src).or_default().push((dst, et));
    }

    const MAX_NODES: usize = 10_000;
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(start.to_string());
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((start.to_string(), 0));
    let mut edges: Vec<(String, String, EdgeType)> = Vec::new();

    while let Some((current, d)) = queue.pop_front() {
        if d >= depth || visited.len() >= MAX_NODES {
            continue;
        }
        let outgoing_edges = outgoing.get(&current).map(|v| v.as_slice()).unwrap_or(&[]);
        if d > 0 && is_hub_anchor(&current) && outgoing_edges.len() > HUB_DEGREE_CAP {
            continue;
        }
        for (nbr, et) in outgoing_edges {
            if !edge_types.is_empty() && !edge_types.contains(et) {
                continue;
            }
            edges.push((current.clone(), nbr.clone(), *et));
            if visited.insert(nbr.clone()) && visited.len() <= MAX_NODES {
                queue.push_back((nbr.clone(), d + 1));
            }
        }
    }

    let is_index_node = |a: &str| {
        let l = a.to_lowercase();
        l.starts_with("mod-") || l.starts_with("module-") || !l.contains('#')
    };
    if !caller_types.is_empty() {
        let mut callers: Vec<(String, EdgeType)> = incoming_start
            .into_iter()
            .filter(|(src, et)| {
                caller_types.contains(et) && !is_index_node(src) && !visited.contains(src)
            })
            .collect();
        callers.sort_by(|a, b| a.0.cmp(&b.0));
        callers.dedup_by(|a, b| a.0 == b.0);
        let mut kept = 0usize;
        for (src, et) in callers {
            if kept >= MAX_CALLERS {
                break;
            }
            let source_file = docs
                .get(&src)
                .and_then(|d| d.attributes.get("source_file").map(String::as_str));
            if !caller_filter(&src, source_file) {
                continue;
            }
            edges.push((start.to_string(), src.clone(), et));
            visited.insert(src);
            kept += 1;
        }
    }

    let mut visited_docs: HashMap<String, Document> = HashMap::new();
    for anchor in &visited {
        if let Some(doc) = docs.get(anchor) {
            visited_docs.insert(anchor.clone(), doc.clone());
        }
    }

    Ok(build_graph_from_docs_and_edges(visited_docs, edges, dir))
}

fn build_neighborhood_impl(
    dir: &Path,
    start: &str,
    depth: usize,
    edge_types: &[EdgeType],
    caller_types: &[EdgeType],
    caller_filter: &dyn Fn(&str, Option<&str>) -> bool,
) -> Result<AdenGraph<DocumentNode, AdenEdge>, crate::graph::GraphError> {
    // ADR-011: snapshot-first, depth-bounded BFS in memory (lock-free reads).
    if let Some(data) = snapshot::try_read_fresh(dir) {
        return build_neighborhood_from_materialized(
            data,
            dir,
            start,
            depth,
            edge_types,
            caller_types,
            caller_filter,
        );
    }

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
        // expanding them as intermediaries. The name flag is a heuristic — a
        // symbol in a single-file crate's `lib.rs` is NOT an index node — so a
        // name-flagged node is only blocked when its measured fan-out is
        // actually hub-sized; the real hubs (synthesized `mod-*` nodes,
        // file-level docs of large files) stay blocked by their degree.
        let outgoing = storage.get_outgoing_edges(&current).unwrap_or_default();
        if d > 0 && is_hub_anchor(&current) && outgoing.len() > HUB_DEGREE_CAP {
            continue;
        }
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

    // Fold in the seed's callers/users (incoming edges of the requested types),
    // anchor-sorted for determinism. Module-INDEX nodes are excluded (a hub
    // "calling" the seed is containment scaffolding, not caller context) — but
    // only true index nodes: a real symbol that happens to live in a crate's
    // `lib.rs` is a legitimate caller, so the broader `is_hub_anchor` test
    // (which sweeps in every `lib.rs#symbol`) is deliberately not used here.
    let is_index_node = |a: &str| {
        let l = a.to_lowercase();
        l.starts_with("mod-") || l.starts_with("module-") || !l.contains('#')
    };
    if !caller_types.is_empty() {
        let mut callers: Vec<(String, EdgeType)> = storage
            .get_incoming_edges(start)
            .unwrap_or_default()
            .into_iter()
            .filter(|(src, et)| {
                caller_types.contains(et) && !is_index_node(src) && !visited.contains(src)
            })
            .collect();
        callers.sort_by(|a, b| a.0.cmp(&b.0));
        callers.dedup_by(|a, b| a.0 == b.0);
        let mut kept = 0usize;
        for (src, et) in callers {
            if kept >= MAX_CALLERS {
                break;
            }
            let source_file = storage
                .get_document(&src)
                .ok()
                .flatten()
                .and_then(|d| d.attributes.get("source_file").cloned());
            if !caller_filter(&src, source_file.as_deref()) {
                continue;
            }
            edges.push((start.to_string(), src.clone(), et));
            visited.insert(src);
            kept += 1;
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
