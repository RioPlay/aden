// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Disk cache for the Aden knowledge graph.
//!
//! Full-graph reads prefer the lock-free ADR-011 snapshot. Selective anchor and
//! neighborhood reads use fjall's keyed documents/adjacency lists first so a
//! small question does not deserialize an entire repository; they fall back to
//! the snapshot when a concurrent writer makes the store unavailable.

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

/// How a user-supplied anchor or natural symbol name resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorResolution {
    Exact(String),
    Unique { anchor: String },
    Ambiguous { candidates: Vec<String> },
    NotFound { suggestions: Vec<String> },
}

impl AnchorResolution {
    /// Return a definitive anchor only when resolution did not require guessing.
    pub fn resolved(self) -> Option<String> {
        match self {
            Self::Exact(anchor) | Self::Unique { anchor } => Some(anchor),
            Self::Ambiguous { .. } | Self::NotFound { .. } => None,
        }
    }
}

/// Normalize generic and whitespace-heavy human symbol spellings.
pub fn natural_symbol_form(value: &str) -> String {
    aden_core::symbol::natural_symbol_form(value)
}

/// Rank an anchor against a natural symbol spelling. Lower is better; rank 5
/// is a substring suggestion and must never be selected automatically.
pub fn anchor_match_rank(anchor: &str, symbol: &str) -> u8 {
    let sym = natural_symbol_form(symbol);
    let sym_lower = sym.to_lowercase();
    let seg = natural_symbol_form(anchor.rsplit(['#', '/']).next().unwrap_or(""));
    let seg_lower = seg.to_lowercase();
    let leaf = seg.rsplit(['.', ':']).next().unwrap_or(&seg);
    let leaf_lower = leaf.to_lowercase();
    if seg == sym {
        0
    } else if seg_lower == sym_lower {
        1
    } else if leaf == sym {
        2
    } else if leaf_lower == sym_lower {
        3
    } else if seg_lower.starts_with(&format!("{sym_lower}."))
        || seg_lower.starts_with(&format!("{sym_lower}::"))
    {
        4
    } else if !sym_lower.is_empty() && seg_lower.contains(&sym_lower) {
        5
    } else {
        u8::MAX
    }
}

fn ranked_anchor_matches<'a>(
    symbol: &str,
    anchors: &'a [String],
) -> (Option<&'a String>, Vec<(u8, &'a String)>) {
    let mut exact = None;
    let mut matched = Vec::new();
    for anchor in anchors {
        if anchor == symbol {
            exact = Some(anchor);
        }
        let rank = anchor_match_rank(anchor, symbol);
        if rank != u8::MAX {
            matched.push((rank, anchor));
        }
    }
    matched.sort_by(|(left_rank, left_anchor), (right_rank, right_anchor)| {
        left_rank
            .cmp(right_rank)
            .then_with(|| left_anchor.cmp(right_anchor))
    });
    (exact, matched)
}

/// Return every equally-best natural-name candidate in deterministic order.
pub fn ranked_anchor_candidates(symbol: &str, anchors: &[String]) -> Vec<String> {
    let (_, matched) = ranked_anchor_matches(symbol, anchors);
    let Some((best_rank, _)) = matched.first() else {
        return Vec::new();
    };
    matched
        .iter()
        .take_while(|(rank, _)| rank == best_rank)
        .map(|(_, anchor)| (*anchor).clone())
        .collect()
}

/// Return typo recovery candidates without ever promoting them to resolutions.
/// Short queries are excluded to avoid noisy suggestions; results are bounded and
/// ordered by edit distance, then canonical anchor for deterministic clients.
fn typo_anchor_suggestions(symbol: &str, anchors: &[String]) -> Vec<String> {
    let symbol = natural_symbol_form(symbol).to_lowercase();
    let symbol_len = symbol.chars().count();
    let Some(max_distance) = aden_core::symbol::typo_max_distance(symbol_len) else {
        return Vec::new();
    };
    let mut suggestions = Vec::new();
    for anchor in anchors {
        let segment = natural_symbol_form(anchor.rsplit(['#', '/']).next().unwrap_or(""));
        let segment = segment.to_lowercase();
        let leaf = segment.rsplit(['.', ':']).next().unwrap_or(&segment);
        let distance = [segment.as_str(), leaf]
            .into_iter()
            .filter(|candidate| candidate.chars().count().abs_diff(symbol_len) <= max_distance)
            .map(|candidate| aden_core::symbol::edit_distance(&symbol, candidate))
            .min();
        if let Some(distance) = distance.filter(|distance| *distance <= max_distance) {
            suggestions.push((distance, anchor.clone()));
        }
    }
    suggestions.sort_by(
        |(left_distance, left_anchor), (right_distance, right_anchor)| {
            left_distance
                .cmp(right_distance)
                .then_with(|| left_anchor.cmp(right_anchor))
        },
    );
    suggestions.dedup_by(|left, right| left.1 == right.1);
    suggestions
        .into_iter()
        .take(8)
        .map(|(_, anchor)| anchor)
        .collect()
}

/// Resolution plus every ranked discovery match from the same anchor scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorResolutionAnalysis {
    pub resolution: AnchorResolution,
    pub ranked_matches: Vec<String>,
}

/// Analyze an already-loaded anchor set once. Structural commands consume the
/// resolution; discovery commands can also present the broader ranked matches.
pub fn analyze_anchor_list(anchor: &str, anchors: &[String]) -> AnchorResolutionAnalysis {
    let (exact, matched) = ranked_anchor_matches(anchor, anchors);
    let mut ranked_matches = Vec::with_capacity(matched.len() + usize::from(exact.is_some()));
    if let Some(exact) = exact {
        ranked_matches.push(exact.clone());
    }
    ranked_matches.extend(
        matched
            .iter()
            .filter(|(_, candidate)| Some(*candidate) != exact)
            .map(|(_, candidate)| (*candidate).clone()),
    );

    let resolution = if exact.is_some() {
        AnchorResolution::Exact(anchor.to_string())
    } else if let Some((best_rank, _)) = matched.first() {
        let candidates: Vec<String> = matched
            .iter()
            .take_while(|(rank, _)| rank == best_rank)
            .map(|(_, candidate)| (*candidate).clone())
            .collect();
        if *best_rank >= 5 {
            AnchorResolution::NotFound {
                suggestions: candidates.into_iter().take(8).collect(),
            }
        } else if candidates.len() == 1 {
            AnchorResolution::Unique {
                anchor: candidates.into_iter().next().unwrap(),
            }
        } else {
            AnchorResolution::Ambiguous { candidates }
        }
    } else {
        AnchorResolution::NotFound {
            suggestions: typo_anchor_suggestions(anchor, anchors),
        }
    };

    AnchorResolutionAnalysis {
        resolution,
        ranked_matches,
    }
}

/// Resolve against an already-loaded anchor set. This is the shared deterministic
/// contract used by read commands that have already paid to load the index.
pub fn resolve_anchor_from_list(anchor: &str, anchors: &[String]) -> AnchorResolution {
    analyze_anchor_list(anchor, anchors).resolution
}

/// Resolve an exact anchor, module alias, or natural symbol name without loading
/// the complete graph. Ambiguity is preserved rather than collapsed to missing.
pub fn resolve_anchor_detailed(dir: &Path, anchor: &str) -> AnchorResolution {
    let (store_path, _) = aden_paths::resolve_read_store(dir);
    if let Some(store_path) = store_path.to_str()
        && let Ok(storage) = Storage::open_existing(store_path)
    {
        if matches!(storage.get_document(anchor), Ok(Some(_))) {
            return AnchorResolution::Exact(anchor.to_string());
        }
        if let Ok(Some(lookup)) = storage.lookup_symbol_candidates(anchor) {
            return resolve_anchor_from_list(anchor, &lookup.anchors);
        }
        if let Ok(anchors) = storage.get_all_anchors() {
            let anchors = anchors.into_iter().collect::<Vec<_>>();
            return resolve_anchor_from_list(anchor, &anchors);
        }
    }

    if let Some((docs, _)) = snapshot::try_read_fresh(dir) {
        let anchors = docs.into_keys().collect::<Vec<_>>();
        return resolve_anchor_from_list(anchor, &anchors);
    }
    AnchorResolution::NotFound {
        suggestions: Vec::new(),
    }
}

/// Compatibility wrapper for callers that only need a definitive anchor.
pub fn resolve_anchor_in_store(dir: &Path, anchor: &str) -> Option<String> {
    resolve_anchor_detailed(dir, anchor).resolved()
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
    const MAX_NODES: usize = 10_000;
    let (store_path, _) = aden_paths::resolve_read_store(dir);
    let store_path = store_path
        .to_str()
        .ok_or_else(|| crate::graph::GraphError::Io("invalid store path".into()))?;
    // Selective traversal is why this function exists: read only adjacency
    // lists and documents reached from the seed. Snapshot-first behavior used
    // to deserialize every document and edge at kernel scale before discarding
    // almost all of them. A concurrent writer can make fjall unavailable; in
    // that case the immutable ADR-011 snapshot remains the lock-free fallback.
    let storage = match Storage::open_existing(store_path) {
        Ok(storage) => storage,
        Err(store_error) => {
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
            return Err(crate::graph::GraphError::Io(store_error.to_string()));
        }
    };

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

#[cfg(test)]
mod resolution_tests {
    use super::*;

    #[test]
    fn exact_anchor_wins_over_natural_matches() {
        let anchors = vec![
            "mod-project".to_string(),
            "aden://module/a.rs#mod-project".to_string(),
            "aden://module/a.rs#mod-project.member".to_string(),
        ];
        let analysis = analyze_anchor_list("mod-project", &anchors);
        assert_eq!(
            analysis.resolution,
            AnchorResolution::Exact("mod-project".to_string())
        );
        assert_eq!(analysis.ranked_matches, anchors);
    }

    #[test]
    fn natural_resolution_is_unique_or_explicitly_ambiguous() {
        let unique = vec!["aden://module/a.rs#GraphBridge".to_string()];
        assert_eq!(
            resolve_anchor_from_list("graphbridge", &unique),
            AnchorResolution::Unique {
                anchor: unique[0].clone()
            }
        );

        let ambiguous = vec![
            "aden://module/b.rs#parse".to_string(),
            "aden://module/a.rs#parse".to_string(),
        ];
        assert_eq!(
            resolve_anchor_from_list("parse", &ambiguous),
            AnchorResolution::Ambiguous {
                candidates: vec![ambiguous[1].clone(), ambiguous[0].clone()]
            }
        );
    }

    #[test]
    fn substring_matches_are_suggestions_not_resolutions() {
        let anchors = vec!["aden://module/a.rs#parse_document".to_string()];
        assert_eq!(
            resolve_anchor_from_list("document", &anchors),
            AnchorResolution::NotFound {
                suggestions: anchors
            }
        );
    }

    #[test]
    fn typos_are_bounded_deterministic_suggestions_not_resolutions() {
        let anchors = vec![
            "aden://module/z.rs#resolve_anchor_detail".to_string(),
            "aden://module/b.rs#resolve_anchor_detailed".to_string(),
            "aden://module/a.rs#resolve_anchor_detailed".to_string(),
            "aden://module/x.rs#unrelated".to_string(),
        ];
        assert_eq!(
            resolve_anchor_from_list("resovle_anchor_detailed", &anchors),
            AnchorResolution::NotFound {
                suggestions: vec![anchors[2].clone(), anchors[1].clone()]
            }
        );
        assert_eq!(
            resolve_anchor_from_list("prase", &["aden://module/a.rs#parse".to_string()]),
            AnchorResolution::NotFound {
                suggestions: vec!["aden://module/a.rs#parse".to_string()]
            }
        );
        assert_eq!(
            resolve_anchor_from_list("xy", &anchors),
            AnchorResolution::NotFound {
                suggestions: Vec::new()
            }
        );
    }
}
