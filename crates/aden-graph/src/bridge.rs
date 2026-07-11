// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bridge between AdenGraph (petgraph) and GraphStorage (aden-store).
//!
//! Converts between in-memory petgraph structures and the persistent
//! storage layer. This allows aden-graph to keep its existing API while
//! using aden-store for persistence.

use aden_core::{Document, EdgeType};
use aden_store::{GraphStorage, StoreError};
use std::collections::{HashMap, HashSet};

/// Documents loaded from storage (anchor → document) paired with their edges
/// as `(source, target, edge_type)` triples.
type LoadedGraph = (HashMap<String, Document>, Vec<(String, String, EdgeType)>);

/// Bridge between AdenGraph and GraphStorage.
///
/// Provides methods to sync an in-memory graph to storage and load it back.
pub struct GraphBridge;

impl GraphBridge {
    /// Sync an in-memory graph to storage.
    ///
    /// Clears existing data and repopulates from the graph.
    pub fn sync_to_storage<S: GraphStorage>(
        storage: &S,
        docs: &HashMap<String, Document>,
        edges: &[(String, String, EdgeType)],
    ) -> Result<(), StoreError> {
        // Clear existing data
        storage.clear()?;

        // Insert documents
        for (anchor, doc) in docs {
            let mut doc = doc.clone();
            doc.anchor = anchor.clone();
            storage.put_document(&doc)?;
        }

        // Insert edges
        for (src, dst, edge_type) in edges {
            storage.put_edge(src, dst, *edge_type)?;
        }

        Ok(())
    }

    /// Load a graph from storage into in-memory structures.
    pub fn load_from_storage<S: GraphStorage>(storage: &S) -> Result<LoadedGraph, StoreError> {
        let docs = storage.get_all_documents()?;
        // Single-pass: reads the edge type from each key instead of one full
        // scan per type (32 scans → 1). The default trait impl falls back to
        // the per-type loop for backends that don't override get_all_edges.
        let edges = storage.get_all_edges()?;
        Ok((docs, edges))
    }

    /// Save metadata to storage.
    pub fn save_meta<S: GraphStorage>(
        storage: &S,
        key: &str,
        value: &str,
    ) -> Result<(), StoreError> {
        storage.put_meta(key, value)
    }

    /// Load metadata from storage.
    pub fn load_meta<S: GraphStorage>(
        storage: &S,
        key: &str,
    ) -> Result<Option<String>, StoreError> {
        storage.get_meta(key)
    }

    /// Get all anchors from storage.
    pub fn get_all_anchors<S: GraphStorage>(storage: &S) -> Result<HashSet<String>, StoreError> {
        storage.get_all_anchors()
    }

    /// Run BFS from storage.
    pub fn bfs<S: GraphStorage>(
        storage: &S,
        start: &str,
        depth: usize,
        edge_type: Option<&EdgeType>,
    ) -> Result<Vec<(String, String)>, StoreError> {
        storage.bfs(start, depth, edge_type)
    }

    /// Get neighborhood from storage.
    pub fn neighborhood<S: GraphStorage>(
        storage: &S,
        anchor: &str,
        depth: usize,
    ) -> Result<HashMap<String, Vec<(String, EdgeType)>>, StoreError> {
        storage.neighborhood(anchor, depth)
    }

    /// Count nodes in storage.
    pub fn count_nodes<S: GraphStorage>(storage: &S) -> Result<usize, StoreError> {
        storage.count_nodes()
    }

    /// Count edges in storage.
    pub fn count_edges<S: GraphStorage>(storage: &S) -> Result<usize, StoreError> {
        storage.count_edges()
    }
}
