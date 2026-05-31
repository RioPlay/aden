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
    pub fn load_from_storage<S: GraphStorage>(
        storage: &S,
    ) -> Result<LoadedGraph, StoreError> {
        let docs = storage.get_all_documents()?;
        let mut edges = Vec::new();

        // Get all edge types
        let edge_types = Self::all_edge_types();
        for edge_type in &edge_types {
            let typed_edges = storage.get_edges_by_type(edge_type)?;
            for (src, dst) in typed_edges {
                edges.push((src, dst, *edge_type));
            }
        }

        Ok((docs, edges))
    }

    /// Get all edge types.
    fn all_edge_types() -> Vec<EdgeType> {
        vec![
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
        ]
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
    pub fn get_all_anchors<S: GraphStorage>(
        storage: &S,
    ) -> Result<HashSet<String>, StoreError> {
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
