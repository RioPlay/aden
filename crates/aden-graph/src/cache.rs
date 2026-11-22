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
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

pub(crate) mod symbols;

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
            doc,
            source_path,
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
    symbols::Query::new(symbol).rank_anchor(anchor)
}

fn ranked_anchor_matches<'a>(
    symbol: &str,
    anchors: &'a [String],
) -> (Option<&'a String>, Vec<(u8, &'a String)>) {
    let query = symbols::Query::new(symbol);
    let mut exact = None;
    let mut matched = Vec::new();
    for anchor in anchors {
        if anchor == symbol {
            exact = Some(anchor);
        }
        let rank = query.rank_anchor(anchor);
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
    let mut suggestions = aden_core::symbol::SymbolSuggestions::default();
    for anchor in anchors {
        let segment = natural_symbol_form(anchor.rsplit(['#', '/']).next().unwrap_or(""));
        let segment = segment.to_lowercase();
        let leaf = segment.rsplit_once(['.', ':']).map(|(_, leaf)| leaf);
        let distance = std::iter::once(segment.as_str())
            .chain(leaf)
            .filter_map(|candidate| {
                aden_core::symbol::edit_distance_within(&symbol, candidate, max_distance)
            })
            .min();
        if let Some(distance) = distance {
            suggestions.insert(distance, anchor);
        }
    }
    suggestions.into_anchors()
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
    // A scheme-stripped anchor — `proj/path#symbol` as printed in docs or
    // hand-trimmed from `aden://module/proj/path#symbol` — normalizes to its
    // canonical scheme-prefixed form when that form exists. This is an exact
    // lookup, never fuzzy: if neither scheme-prefixed form is in the anchor
    // set, the input flows through unchanged.
    let normalized: String;
    let anchor = if !anchor.contains("://") && anchor.contains('#') {
        let module = format!("aden://module/{anchor}");
        if anchors.iter().any(|candidate| candidate == &module) {
            normalized = module;
            &normalized
        } else {
            let doc = format!("aden://doc/{anchor}");
            if anchors.iter().any(|candidate| candidate == &doc) {
                normalized = doc;
                &normalized
            } else {
                anchor
            }
        }
    } else {
        anchor
    };
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
        && let Some(resolution) = resolve_anchor_with_storage(&storage, anchor)
    {
        return resolution;
    }

    if let Some((docs, _)) = snapshot::try_read_fresh(dir) {
        let anchors = docs.into_keys().collect::<Vec<_>>();
        return resolve_anchor_from_list(anchor, &anchors);
    }
    AnchorResolution::NotFound {
        suggestions: Vec::new(),
    }
}

fn resolve_anchor_with_storage(storage: &Storage, anchor: &str) -> Option<AnchorResolution> {
    if matches!(storage.get_document(anchor), Ok(Some(_))) {
        return Some(AnchorResolution::Exact(anchor.to_string()));
    }
    // Scheme-stripped anchor spelling (`proj/path#sym`, as pasted minus
    // the `aden://` scheme): probe the two canonical scheme forms
    // exactly. This must happen before the symbol-lexicon shortcut —
    // the lexicon returns an empty candidate set for path-shaped input
    // and would otherwise end resolution with a bare not-found.
    if !anchor.contains("://") && anchor.contains('#') {
        for prefix in ["aden://module/", "aden://doc/"] {
            let candidate = format!("{prefix}{anchor}");
            if matches!(storage.get_document(&candidate), Ok(Some(_))) {
                return Some(AnchorResolution::Exact(candidate));
            }
        }
    }
    if let Ok(Some(lookup)) = storage.lookup_symbol_candidates(anchor) {
        return Some(resolve_anchor_from_list(anchor, &lookup.anchors));
    }
    if let Ok(anchors) = storage.get_all_anchors() {
        let anchors = anchors.into_iter().collect::<Vec<_>>();
        return Some(resolve_anchor_from_list(anchor, &anchors));
    }
    None
}

enum ReadSource {
    Store(Storage),
    Snapshot(snapshot::SnapshotData),
    SharedSnapshot(std::sync::Arc<snapshot::SharedSnapshot>),
    Unavailable(String),
}

/// One graph read request. Resolution and traversal share the same store handle
/// or immutable snapshot. A resident worker may reuse a byte-verified immutable
/// snapshot; every command still re-establishes freshness independently.
pub struct GraphReadSession<'a> {
    dir: &'a Path,
    source: ReadSource,
    anchors: OnceCell<Vec<String>>,
    outgoing: OnceCell<HashMap<String, Vec<(String, EdgeType)>>>,
    incoming: OnceCell<HashMap<String, Vec<(String, EdgeType)>>>,
}

impl<'a> GraphReadSession<'a> {
    pub fn open(dir: &'a Path) -> Self {
        if let Some(snapshot) = snapshot::try_read_session(dir) {
            return Self::with_source(dir, ReadSource::SharedSnapshot(snapshot));
        }
        let source = match Self::open_store(dir) {
            Ok(storage) => ReadSource::Store(storage),
            Err(error) => match snapshot::try_read_fresh(dir) {
                Some(data) => ReadSource::Snapshot(data),
                None => ReadSource::Unavailable(error),
            },
        };
        Self::with_source(dir, source)
    }

    fn open_store(dir: &Path) -> Result<Storage, String> {
        let (store_path, _) = aden_paths::resolve_read_store(dir);
        let path = store_path.to_str().ok_or("invalid store path")?;
        Storage::open_existing(path).map_err(|error| error.to_string())
    }

    /// For a project with a published revision, prefer its verified small or
    /// store-backed view. Never fall back through an unchecked snapshot read.
    pub fn open_for_revision(dir: &'a Path, matches_revision: impl FnOnce(&[u8]) -> bool) -> Self {
        let path = aden_paths::graph_snapshot_file(dir);
        let source = match snapshot::read_small_verified(&path, matches_revision) {
            Ok(Some(data)) => ReadSource::Snapshot(data),
            Ok(None) | Err(_) => match Self::open_store(dir) {
                Ok(storage) => ReadSource::Store(storage),
                Err(error) => ReadSource::Unavailable(error),
            },
        };
        Self::with_source(dir, source)
    }

    /// Exhaustive symbol listing is faster from an available snapshot on the
    /// measured tiny and scale fixtures. Keep selective store access as fallback
    /// without changing ranking coverage or introducing a size heuristic.
    pub fn open_snapshot_first(dir: &'a Path) -> Self {
        if let Some(snapshot) = snapshot::try_read_session(dir) {
            return Self::with_source(dir, ReadSource::SharedSnapshot(snapshot));
        }
        match snapshot::try_read_fresh(dir) {
            Some(data) => Self::with_source(dir, ReadSource::Snapshot(data)),
            None => Self::open(dir),
        }
    }

    fn with_source(dir: &'a Path, source: ReadSource) -> Self {
        Self {
            dir,
            source,
            anchors: OnceCell::new(),
            outgoing: OnceCell::new(),
            incoming: OnceCell::new(),
        }
    }

    /// Exhaustive anchor keys without deserializing every document. Locate's
    /// ranked suggestions and totals require this stronger coverage than the
    /// bounded symbol lexicon used for definitive resolution.
    pub fn anchors(&self) -> Result<&[String], crate::graph::GraphError> {
        if let ReadSource::SharedSnapshot(snapshot) = &self.source {
            return Ok(snapshot.anchors());
        }
        if self.anchors.get().is_none() {
            let anchors = match &self.source {
                ReadSource::Store(storage) => storage
                    .get_all_anchors()
                    .map_err(|error| crate::graph::GraphError::Io(error.to_string()))?
                    .into_iter()
                    .collect(),
                ReadSource::Snapshot((docs, _)) => docs.keys().cloned().collect(),
                ReadSource::SharedSnapshot(_) => unreachable!(),
                ReadSource::Unavailable(error) => {
                    return Err(crate::graph::GraphError::Io(error.clone()));
                }
            };
            let _ = self.anchors.set(anchors);
        }
        Ok(self.anchors.get().unwrap())
    }

    pub fn analyze_anchor(
        &self,
        anchor: &str,
    ) -> Result<AnchorResolutionAnalysis, crate::graph::GraphError> {
        if let ReadSource::SharedSnapshot(snapshot) = &self.source {
            return Ok(snapshot.analyze_anchor(anchor));
        }
        Ok(analyze_anchor_list(anchor, self.anchors()?))
    }

    /// Resolve with complete anchor coverage. A resident snapshot can use its
    /// prepared resolution indexes.
    pub fn resolve_anchor_exhaustive(
        &self,
        anchor: &str,
    ) -> Result<AnchorResolution, crate::graph::GraphError> {
        if let ReadSource::SharedSnapshot(snapshot) = &self.source {
            return Ok(snapshot.resolve_anchor(anchor));
        }
        Ok(resolve_anchor_from_list(anchor, self.anchors()?))
    }

    pub fn document(&self, anchor: &str) -> Result<Option<Document>, crate::graph::GraphError> {
        match &self.source {
            ReadSource::Store(storage) => storage
                .get_document(anchor)
                .map_err(|error| crate::graph::GraphError::Io(error.to_string())),
            ReadSource::Snapshot((docs, _)) => Ok(docs.get(anchor).cloned()),
            ReadSource::SharedSnapshot(snapshot) => Ok(snapshot.data.0.get(anchor).cloned()),
            ReadSource::Unavailable(error) => Err(crate::graph::GraphError::Io(error.clone())),
        }
    }

    fn snapshot_adjacency(
        &self,
        direction: crate::Direction,
    ) -> &HashMap<String, Vec<(String, EdgeType)>> {
        if let ReadSource::SharedSnapshot(snapshot) = &self.source {
            return snapshot.adjacency(direction);
        }
        let cell = match direction {
            crate::Direction::Incoming => &self.incoming,
            crate::Direction::Outgoing => &self.outgoing,
        };
        cell.get_or_init(|| {
            let mut adjacency: HashMap<String, Vec<(String, EdgeType)>> = HashMap::new();
            if let ReadSource::Snapshot((_, edges)) = &self.source {
                for (src, dst, edge_type) in edges {
                    let (key, neighbor) = match direction {
                        crate::Direction::Incoming => (dst, src),
                        crate::Direction::Outgoing => (src, dst),
                    };
                    adjacency
                        .entry(key.clone())
                        .or_default()
                        .push((neighbor.clone(), *edge_type));
                }
            }
            adjacency
        })
    }

    fn adjacent(
        &self,
        anchor: &str,
        direction: crate::Direction,
    ) -> Result<Vec<(String, EdgeType)>, crate::graph::GraphError> {
        match &self.source {
            ReadSource::Store(storage) => match direction {
                crate::Direction::Incoming => storage.get_incoming_edges(anchor),
                crate::Direction::Outgoing => storage.get_outgoing_edges(anchor),
            }
            .map_err(|error| crate::graph::GraphError::Io(error.to_string())),
            ReadSource::Snapshot(_) | ReadSource::SharedSnapshot(_) => Ok(self
                .snapshot_adjacency(direction)
                .get(anchor)
                .cloned()
                .unwrap_or_default()),
            ReadSource::Unavailable(error) => Err(crate::graph::GraphError::Io(error.clone())),
        }
    }

    pub fn incoming_edges(
        &self,
        anchor: &str,
    ) -> Result<Vec<(String, EdgeType)>, crate::graph::GraphError> {
        self.adjacent(anchor, crate::Direction::Incoming)
    }

    pub fn resolve_anchor(&self, anchor: &str) -> AnchorResolution {
        match &self.source {
            ReadSource::Store(storage) => resolve_anchor_with_storage(storage, anchor),
            ReadSource::Snapshot((docs, _)) => Some(resolve_anchor_from_list(
                anchor,
                &docs.keys().cloned().collect::<Vec<_>>(),
            )),
            ReadSource::SharedSnapshot(snapshot) => Some(snapshot.resolve_anchor(anchor)),
            ReadSource::Unavailable(_) => None,
        }
        .unwrap_or_else(|| AnchorResolution::NotFound {
            suggestions: Vec::new(),
        })
    }

    /// Load only a query's reachable documents. The extra result beyond the
    /// requested limit lets the caller report truncation honestly. Unlike
    /// assembly context, explicit queries do not prune intermediate hub nodes.
    pub fn into_query_neighborhood(
        self,
        start: &str,
        depth: usize,
        direction: crate::Direction,
        edge_types: &[EdgeType],
        exclude_structural: bool,
        max_results: usize,
    ) -> Result<AdenGraph<DocumentNode, AdenEdge>, crate::graph::GraphError> {
        self.query_neighborhood(
            start,
            depth,
            direction,
            edge_types,
            exclude_structural,
            max_results,
        )
    }

    pub fn query_neighborhood(
        &self,
        start: &str,
        depth: usize,
        direction: crate::Direction,
        edge_types: &[EdgeType],
        exclude_structural: bool,
        max_results: usize,
    ) -> Result<AdenGraph<DocumentNode, AdenEdge>, crate::graph::GraphError> {
        build_query_neighborhood(
            self.dir,
            start,
            depth,
            direction,
            edge_types,
            exclude_structural,
            max_results,
            |anchor| self.document(anchor).map_err(|error| error.to_string()),
            |anchor| {
                self.adjacent(anchor, direction)
                    .map_err(|error| error.to_string())
            },
        )
    }

    /// The same hub pruning, depth and node limits as the standalone assembler,
    /// while reusing this request's store or materialized snapshot.
    pub fn assembly_neighborhood(
        &self,
        start: &str,
        depth: usize,
        edge_types: &[EdgeType],
    ) -> Result<AdenGraph<DocumentNode, AdenEdge>, crate::graph::GraphError> {
        self.assembly_neighborhood_with_callers(start, depth, edge_types, &[], &|_, _| true)
    }

    /// Reuses this request's store or snapshot while folding filtered incoming
    /// callers onto the seed, matching [`build_neighborhood_with_callers`].
    pub fn assembly_neighborhood_with_callers(
        &self,
        start: &str,
        depth: usize,
        edge_types: &[EdgeType],
        caller_types: &[EdgeType],
        caller_filter: &dyn Fn(&str, Option<&str>) -> bool,
    ) -> Result<AdenGraph<DocumentNode, AdenEdge>, crate::graph::GraphError> {
        match &self.source {
            ReadSource::Store(storage) => build_neighborhood_from_storage(
                storage,
                self.dir,
                start,
                depth,
                edge_types,
                caller_types,
                caller_filter,
            ),
            ReadSource::Snapshot((docs, _)) => build_neighborhood_from_adjacency(
                docs,
                self.snapshot_adjacency(crate::Direction::Outgoing),
                filtered_incoming(start, caller_types, || {
                    self.snapshot_adjacency(crate::Direction::Incoming)
                }),
                self.dir,
                start,
                depth,
                edge_types,
                caller_types,
                caller_filter,
            ),
            ReadSource::SharedSnapshot(snapshot) => build_neighborhood_from_adjacency(
                &snapshot.data.0,
                snapshot.adjacency(crate::Direction::Outgoing),
                filtered_incoming(start, caller_types, || {
                    snapshot.adjacency(crate::Direction::Incoming)
                }),
                self.dir,
                start,
                depth,
                edge_types,
                caller_types,
                caller_filter,
            ),
            ReadSource::Unavailable(error) => Err(crate::graph::GraphError::Io(error.clone())),
        }
    }

    /// Load the complete graph through this request's already-open store or snapshot.
    pub fn full_graph(
        &self,
    ) -> Result<AdenGraph<DocumentNode, AdenEdge>, crate::graph::GraphError> {
        match &self.source {
            ReadSource::Store(storage) => GraphBridge::load_from_storage(storage)
                .map_err(|error| crate::graph::GraphError::Io(error.to_string()))
                .map(|(docs, edges)| build_graph_from_docs_and_edges(docs, edges, self.dir)),
            ReadSource::Snapshot((docs, edges)) => Ok(build_graph_from_docs_and_edges(
                docs.clone(),
                edges.clone(),
                self.dir,
            )),
            ReadSource::SharedSnapshot(snapshot) => Ok(build_graph_from_docs_and_edges(
                snapshot.data.0.clone(),
                snapshot.data.1.clone(),
                self.dir,
            )),
            ReadSource::Unavailable(error) => Err(crate::graph::GraphError::Io(error.clone())),
        }
    }

    /// Count incoming edges per target anchor across the active store or snapshot.
    pub fn incoming_counts_by_target(
        &self,
        edge_types: &[EdgeType],
    ) -> Result<HashMap<String, usize>, crate::graph::GraphError> {
        let count_edges = |edges: &[(String, String, EdgeType)]| {
            let mut seen = HashSet::new();
            let mut counts = HashMap::new();
            for (src, dst, edge_type) in edges {
                if (edge_types.is_empty() || edge_types.contains(edge_type))
                    && seen.insert((src.as_str(), dst.as_str(), *edge_type))
                {
                    *counts.entry(dst.clone()).or_insert(0) += 1;
                }
            }
            counts
        };
        match &self.source {
            ReadSource::Store(storage) => storage
                .incoming_counts_by_target(edge_types)
                .map_err(|error| crate::graph::GraphError::Io(error.to_string())),
            ReadSource::Snapshot((_, edges)) => Ok(count_edges(edges)),
            ReadSource::SharedSnapshot(snapshot) => Ok(count_edges(&snapshot.data.1)),
            ReadSource::Unavailable(error) => Err(crate::graph::GraphError::Io(error.clone())),
        }
    }
}

fn filtered_incoming<'a>(
    start: &str,
    caller_types: &[EdgeType],
    get_incoming: impl FnOnce() -> &'a HashMap<String, Vec<(String, EdgeType)>>,
) -> Vec<(String, EdgeType)> {
    if caller_types.is_empty() {
        Vec::new()
    } else {
        get_incoming()
            .get(start)
            .map(|edges| {
                edges
                    .iter()
                    .filter(|(_, et)| caller_types.contains(et))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[allow(clippy::too_many_arguments)]
fn build_query_neighborhood(
    dir: &Path,
    start: &str,
    depth: usize,
    direction: crate::Direction,
    edge_types: &[EdgeType],
    exclude_structural: bool,
    max_results: usize,
    mut document: impl FnMut(&str) -> Result<Option<Document>, String>,
    mut adjacent: impl FnMut(&str) -> Result<Vec<(String, EdgeType)>, String>,
) -> Result<AdenGraph<DocumentNode, AdenEdge>, crate::graph::GraphError> {
    let mut docs = HashMap::new();
    if let Some(doc) = document(start).map_err(crate::graph::GraphError::Io)? {
        docs.insert(start.to_string(), doc);
    }
    let mut queue = VecDeque::from([(start.to_string(), 0usize)]);
    let mut edges = Vec::new();
    let node_limit = max_results.saturating_add(1).max(1);
    'traversal: while let Some((current, current_depth)) = queue.pop_front() {
        if current_depth >= depth {
            continue;
        }
        // Group parallel edges before filtering, so provenance includes every
        // recorded relationship between the pair, including nonmatching types.
        let mut grouped: std::collections::BTreeMap<String, Vec<EdgeType>> =
            std::collections::BTreeMap::new();
        for (neighbor, edge_type) in adjacent(&current).map_err(crate::graph::GraphError::Io)? {
            let types = grouped.entry(neighbor).or_default();
            if !types.contains(&edge_type) {
                types.push(edge_type);
            }
        }
        for (neighbor, types) in grouped {
            if (!edge_types.is_empty() && !types.iter().any(|t| edge_types.contains(t)))
                || (exclude_structural
                    && types
                        .iter()
                        .all(|t| matches!(t, EdgeType::Contains | EdgeType::PartOf)))
            {
                continue;
            }
            if !docs.contains_key(&neighbor) {
                let Some(doc) = document(&neighbor).map_err(crate::graph::GraphError::Io)? else {
                    continue;
                };
                docs.insert(neighbor.clone(), doc);
                queue.push_back((neighbor.clone(), current_depth + 1));
            }
            for edge_type in types {
                let (src, dst) = match direction {
                    crate::Direction::Incoming => (neighbor.clone(), current.clone()),
                    crate::Direction::Outgoing => (current.clone(), neighbor.clone()),
                };
                edges.push((src, dst, edge_type));
            }
            if docs.len() >= node_limit {
                break 'traversal;
            }
        }
    }
    Ok(build_graph_from_docs_and_edges(docs, edges, dir))
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

    build_neighborhood_from_adjacency(
        &docs,
        &outgoing,
        incoming_start,
        dir,
        start,
        depth,
        edge_types,
        caller_types,
        caller_filter,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_neighborhood_from_adjacency(
    docs: &HashMap<String, Document>,
    outgoing: &HashMap<String, Vec<(String, EdgeType)>>,
    incoming_start: Vec<(String, EdgeType)>,
    dir: &Path,
    start: &str,
    depth: usize,
    edge_types: &[EdgeType],
    caller_types: &[EdgeType],
    caller_filter: &dyn Fn(&str, Option<&str>) -> bool,
) -> Result<AdenGraph<DocumentNode, AdenEdge>, crate::graph::GraphError> {
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

    build_neighborhood_from_storage(
        &storage,
        dir,
        start,
        depth,
        edge_types,
        caller_types,
        caller_filter,
    )
}

fn build_neighborhood_from_storage(
    storage: &Storage,
    dir: &Path,
    start: &str,
    depth: usize,
    edge_types: &[EdgeType],
    caller_types: &[EdgeType],
    caller_filter: &dyn Fn(&str, Option<&str>) -> bool,
) -> Result<AdenGraph<DocumentNode, AdenEdge>, crate::graph::GraphError> {
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
mod query_neighborhood_tests {
    use super::*;

    fn snapshot_session(dir: &Path) -> GraphReadSession<'_> {
        let docs = ["alpha", "beta", "container"]
            .into_iter()
            .map(|anchor| {
                (
                    anchor.to_string(),
                    Document {
                        anchor: anchor.to_string(),
                        ..Document::default()
                    },
                )
            })
            .collect();
        GraphReadSession {
            dir,
            anchors: OnceCell::new(),
            outgoing: OnceCell::new(),
            incoming: OnceCell::new(),
            source: ReadSource::Snapshot((
                docs,
                vec![
                    ("alpha".into(), "beta".into(), EdgeType::Calls),
                    ("alpha".into(), "beta".into(), EdgeType::Contains),
                    ("container".into(), "beta".into(), EdgeType::Contains),
                    ("alpha".into(), "missing-document".into(), EdgeType::Calls),
                ],
            )),
        }
    }

    #[test]
    fn resident_snapshot_matches_uncached_resolution_queries_and_assembly() {
        let dir = tempfile::tempdir().unwrap();
        let uncached = snapshot_session(dir.path());
        let ReadSource::Snapshot(data) = &uncached.source else {
            unreachable!()
        };
        let shared = GraphReadSession::with_source(
            dir.path(),
            ReadSource::SharedSnapshot(std::sync::Arc::new(snapshot::SharedSnapshot::new(
                data.clone(),
            ))),
        );
        for symbol in ["alpha", "beta", "missing"] {
            assert_eq!(
                shared.resolve_anchor(symbol),
                uncached.resolve_anchor(symbol)
            );
            assert_eq!(
                shared.document(symbol).unwrap(),
                uncached.document(symbol).unwrap()
            );
            assert_eq!(
                shared.incoming_edges(symbol).unwrap(),
                uncached.incoming_edges(symbol).unwrap()
            );
        }
        fn evidence(
            graph: AdenGraph<DocumentNode, AdenEdge>,
        ) -> (HashSet<String>, HashSet<(String, String, EdgeType)>) {
            (
                graph
                    .all_nodes()
                    .into_iter()
                    .map(|(anchor, _)| anchor)
                    .collect(),
                graph
                    .all_edges()
                    .into_iter()
                    .map(|(a, b, edge)| (a, b, edge.edge_type))
                    .collect(),
            )
        }
        for direction in [crate::Direction::Outgoing, crate::Direction::Incoming] {
            assert_eq!(
                evidence(
                    shared
                        .query_neighborhood("beta", 2, direction, &[], false, 10)
                        .unwrap()
                ),
                evidence(
                    uncached
                        .query_neighborhood("beta", 2, direction, &[], false, 10)
                        .unwrap()
                )
            );
        }
        assert_eq!(
            evidence(
                shared
                    .assembly_neighborhood("alpha", 2, &[EdgeType::Calls])
                    .unwrap()
            ),
            evidence(
                uncached
                    .assembly_neighborhood("alpha", 2, &[EdgeType::Calls])
                    .unwrap()
            )
        );
    }

    #[test]
    fn exhaustive_resolution_preserves_shared_and_uncached_snapshot_outcomes() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = "aden://module/demo/lib.rs#parse";
        let docs = [
            canonical,
            "aden://doc/demo/lib.rs#parse",
            "aden://module/demo/parse.rs#Namespace::parse",
        ]
        .into_iter()
        .map(|anchor| {
            (
                anchor.to_owned(),
                Document {
                    anchor: anchor.to_owned(),
                    ..Document::default()
                },
            )
        })
        .collect();
        let data = (docs, Vec::new());
        let shared = GraphReadSession::with_source(
            dir.path(),
            ReadSource::SharedSnapshot(std::sync::Arc::new(snapshot::SharedSnapshot::new(
                data.clone(),
            ))),
        );
        let uncached = GraphReadSession::with_source(dir.path(), ReadSource::Snapshot(data));
        for symbol in [
            canonical,
            "demo/lib.rs#parse",
            "parse",
            "Namespace",
            "ars",
            "prase",
            "missing",
        ] {
            let expected = uncached.analyze_anchor(symbol).unwrap().resolution;
            assert_eq!(
                shared.resolve_anchor_exhaustive(symbol).unwrap(),
                expected,
                "{symbol}"
            );
            assert_eq!(
                uncached.resolve_anchor_exhaustive(symbol).unwrap(),
                expected,
                "{symbol}"
            );
        }
    }

    #[test]
    fn selective_query_preserves_parallel_types_and_filters_structural_only_pairs() {
        let dir = tempfile::tempdir().unwrap();
        let forward = snapshot_session(dir.path())
            .into_query_neighborhood(
                "alpha",
                1,
                crate::Direction::Outgoing,
                &[EdgeType::Calls],
                false,
                100,
            )
            .unwrap();
        assert_eq!(
            forward.all_nodes().len(),
            2,
            "dangling edges must not create query results"
        );
        let types: HashSet<_> = forward
            .all_edges()
            .into_iter()
            .map(|(_, _, edge)| edge.edge_type)
            .collect();
        assert_eq!(types, HashSet::from([EdgeType::Calls, EdgeType::Contains]));

        let reverse = snapshot_session(dir.path())
            .into_query_neighborhood("beta", 1, crate::Direction::Incoming, &[], true, 100)
            .unwrap();
        assert_eq!(
            reverse
                .all_nodes()
                .into_iter()
                .map(|(anchor, _)| anchor)
                .collect::<HashSet<_>>(),
            HashSet::from(["alpha".into(), "beta".into()])
        );
        assert_eq!(
            reverse.all_edges().len(),
            2,
            "a Calls+Contains pair is a valid reference with both provenance types"
        );

        let explicit_membership = snapshot_session(dir.path())
            .into_query_neighborhood(
                "beta",
                1,
                crate::Direction::Incoming,
                &[EdgeType::Contains],
                false,
                100,
            )
            .unwrap();
        assert_eq!(explicit_membership.all_nodes().len(), 3);
    }

    #[test]
    fn failed_adjacency_read_is_an_error_instead_of_an_empty_neighborhood() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_query_neighborhood(
            dir.path(),
            "seed",
            1,
            crate::Direction::Outgoing,
            &[],
            false,
            100,
            |anchor| {
                Ok(Some(Document {
                    anchor: anchor.into(),
                    ..Document::default()
                }))
            },
            |_| Err("representative storage read failure".into()),
        );
        assert!(
            matches!(graph, Err(crate::graph::GraphError::Io(error)) if error.contains("storage read failure"))
        );
    }

    #[test]
    fn unavailable_backend_is_distinct_from_a_valid_empty_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let unavailable = GraphReadSession::open(dir.path());
        assert!(unavailable.analyze_anchor("missing").is_err());
        assert!(unavailable.resolve_anchor_exhaustive("missing").is_err());
        assert!(unavailable.document("missing").is_err());
        assert!(unavailable.incoming_edges("missing").is_err());

        let mut empty = snapshot_session(dir.path());
        empty.source = ReadSource::Snapshot((HashMap::new(), Vec::new()));
        let analysis = empty.analyze_anchor("missing").unwrap();
        assert!(matches!(
            analysis.resolution,
            AnchorResolution::NotFound { .. }
        ));
        assert!(analysis.ranked_matches.is_empty());
        assert!(empty.document("missing").unwrap().is_none());
        assert!(empty.incoming_edges("missing").unwrap().is_empty());
    }

    #[test]
    fn shared_snapshot_preserves_assembly_hub_pruning_and_complete_query_views() {
        let dir = tempfile::tempdir().unwrap();
        let entry = "aden://module/project/lib.rs#entry";
        let hub = "aden://module/project/lib.rs#dispatch";
        let mut anchors = vec![entry.to_string(), hub.to_string()];
        let mut edges = vec![(entry.into(), hub.into(), EdgeType::Calls)];
        for index in 0..40 {
            let leaf = format!("aden://module/project/lib.rs#leaf_{index}");
            edges.push((hub.into(), leaf.clone(), EdgeType::Calls));
            anchors.push(leaf);
        }
        let docs = anchors
            .into_iter()
            .map(|anchor| {
                (
                    anchor.clone(),
                    Document {
                        anchor,
                        ..Document::default()
                    },
                )
            })
            .collect();
        let mut session = snapshot_session(dir.path());
        session.source = ReadSource::Snapshot((docs, edges));
        let query = session
            .query_neighborhood(
                entry,
                2,
                crate::Direction::Outgoing,
                &[EdgeType::Calls],
                false,
                usize::MAX,
            )
            .unwrap();
        assert_eq!(query.all_nodes().len(), 42);
        let assembly = session
            .assembly_neighborhood(entry, 2, &[EdgeType::Calls])
            .unwrap();
        assert_eq!(assembly.all_nodes().len(), 2);
        assert_eq!(assembly.all_edges().len(), 1);
        assert_eq!(
            session
                .query_neighborhood(
                    entry,
                    2,
                    crate::Direction::Outgoing,
                    &[EdgeType::Calls],
                    false,
                    usize::MAX
                )
                .unwrap()
                .all_nodes()
                .len(),
            42
        );
    }

    #[test]
    fn session_caller_assembly_full_graph_and_counts_match_across_sources() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join("store");
        let caller = "aden://module/app/lib.rs#caller";
        let test_caller = "aden://module/app/lib.rs#test_caller";
        let seed = "aden://module/app/lib.rs#seed";
        let callee = "aden://module/app/lib.rs#callee";
        let guide = "aden://doc/app/guide.adoc#intro";

        let doc_with_file = |anchor: &str, file: &str| Document {
            anchor: anchor.into(),
            attributes: HashMap::from([("source_file".into(), file.into())]),
            ..Document::default()
        };
        let docs = HashMap::from([
            (caller.into(), doc_with_file(caller, "src/lib.rs")),
            (
                test_caller.into(),
                doc_with_file(test_caller, "tests/unit.rs"),
            ),
            (seed.into(), doc_with_file(seed, "src/lib.rs")),
            (callee.into(), doc_with_file(callee, "src/lib.rs")),
            (guide.into(), doc_with_file(guide, "docs/guide.adoc")),
        ]);
        let edges = vec![
            (caller.into(), seed.into(), EdgeType::Calls),
            (test_caller.into(), seed.into(), EdgeType::Calls),
            (seed.into(), callee.into(), EdgeType::Calls),
            (guide.into(), seed.into(), EdgeType::Documents),
        ];

        let storage = Storage::new(store_dir.to_str().unwrap()).unwrap();
        for doc in docs.values() {
            storage.put_document(doc).unwrap();
        }
        storage.put_edges_bulk(&edges).unwrap();

        let store_session = GraphReadSession::with_source(dir.path(), ReadSource::Store(storage));
        let snap_session = GraphReadSession::with_source(
            dir.path(),
            ReadSource::Snapshot((docs.clone(), edges.clone())),
        );
        let shared_session = GraphReadSession::with_source(
            dir.path(),
            ReadSource::SharedSnapshot(std::sync::Arc::new(snapshot::SharedSnapshot::new((
                docs, edges,
            )))),
        );

        let filter = |anchor: &str, source_file: Option<&str>| {
            !anchor.contains("#test_") && source_file.is_none_or(|file| !file.starts_with("tests/"))
        };
        let sessions = [&store_session, &snap_session, &shared_session];
        let expected_counts =
            HashMap::from([(seed.to_string(), 2usize), (callee.to_string(), 1usize)]);
        for session in sessions {
            let asm = session
                .assembly_neighborhood_with_callers(
                    seed,
                    1,
                    &[EdgeType::Calls],
                    &[EdgeType::Calls],
                    &filter,
                )
                .unwrap();
            let nodes: HashSet<String> = asm.all_nodes().into_iter().map(|(a, _)| a).collect();
            assert_eq!(
                nodes,
                HashSet::from([caller.into(), seed.into(), callee.into()])
            );
            let asm_edges: HashSet<(String, String, EdgeType)> = asm
                .all_edges()
                .into_iter()
                .map(|(a, b, edge)| (a, b, edge.edge_type))
                .collect();
            assert!(asm_edges.contains(&(seed.into(), caller.into(), EdgeType::Calls)));
            assert!(asm_edges.contains(&(seed.into(), callee.into(), EdgeType::Calls)));

            let full = session.full_graph().unwrap();
            assert_eq!(full.all_nodes().len(), 5);
            assert_eq!(full.all_edges().len(), 4);

            assert_eq!(
                session
                    .incoming_counts_by_target(&[EdgeType::Calls])
                    .unwrap(),
                expected_counts
            );
            assert_eq!(
                session
                    .incoming_counts_by_target(&[])
                    .unwrap()
                    .get(seed)
                    .copied(),
                Some(3)
            );
        }
    }
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
