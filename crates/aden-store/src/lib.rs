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
//! Storage abstraction layer for the Aden knowledge graph.
//!
//! Defines the [`GraphStorage`] trait and provides a Sled implementation
//! with Postcard serialization.
//!
//! ## Design Philosophy
//!
//! `.adoc` files are the source of truth. The storage layer is a materialized
//! view — fast reads, fast writes, swappable implementation.
//!
//! ## Key-Value Layout
//!
//! | Tree | Key | Value |
//! |---|---|---|
//! | `docs` | `doc:{anchor}` | Serialized Document |
//! | `edges` | `edge<US>{src}<US>{dst}<US>{type}` | () (edge existence) |
//! | `outgoing` | `out:{anchor}` | Vec<(anchor, edge_type)> |
//! | `incoming` | `in:{anchor}` | Vec<(anchor, edge_type)> |
//! | `index` | `idx:{term}` | Vec<(anchor, score)> |
//! | `meta` | `meta:{key}` | value |

use aden_core::{Document, EdgeType};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

// ─── Serialization ───────────────────────────────────────────────────────────

/// Serialize any serde-compatible type to bytes using Postcard.
pub fn serialize<T: Serialize>(value: &T) -> Result<Vec<u8>, StoreError> {
    postcard::to_allocvec(value).map_err(|e| StoreError::Serialization(e.to_string()))
}

/// Deserialize bytes to any serde-compatible type using Postcard.
pub fn deserialize<T: for<'a> Deserialize<'a>>(bytes: &[u8]) -> Result<T, StoreError> {
    postcard::from_bytes(bytes).map_err(|e| StoreError::Serialization(e.to_string()))
}

/// Serialize a Document to bytes.
pub fn serialize_document(doc: &Document) -> Result<Vec<u8>, StoreError> {
    serialize(doc)
}

/// Deserialize a Document from bytes.
pub fn deserialize_document(bytes: &[u8]) -> Result<Document, StoreError> {
    deserialize(bytes)
}

/// Serialize a Vec of (String, EdgeType) tuples to bytes.
pub fn serialize_edges(edges: &[(String, EdgeType)]) -> Result<Vec<u8>, StoreError> {
    postcard::to_allocvec(edges).map_err(|e| StoreError::Serialization(e.to_string()))
}

/// Deserialize a Vec of (String, EdgeType) tuples from bytes.
pub fn deserialize_edges(bytes: &[u8]) -> Result<Vec<(String, EdgeType)>, StoreError> {
    deserialize(bytes)
}

// ─── Key Builders ────────────────────────────────────────────────────────────

/// Build a document key.
pub fn doc_key(anchor: &str) -> String {
    format!("doc:{anchor}")
}

/// Field separator for composite keys. ASCII Unit Separator (0x1F) never
/// appears in anchors (which contain `:` `/` `#` etc.), so splitting on it is
/// unambiguous — unlike `:`, which collides with symbol anchors like
/// `aden://module/crate/file.rs#sym` and silently broke edge loading.
pub const KEY_SEP: char = '\u{1f}';

/// Build an edge key.
pub fn edge_key(src: &str, dst: &str, edge_type: &EdgeType) -> String {
    format!("edge{KEY_SEP}{src}{KEY_SEP}{dst}{KEY_SEP}{edge_type:?}")
}

/// Build an outgoing edge key.
pub fn outgoing_key(anchor: &str) -> String {
    format!("out:{anchor}")
}

/// Build an incoming edge key.
pub fn incoming_key(anchor: &str) -> String {
    format!("in:{anchor}")
}

/// Build an index key.
pub fn index_key(term: &str) -> String {
    format!("idx:{term}")
}

/// Build a metadata key.
pub fn meta_key(key: &str) -> String {
    format!("meta:{key}")
}

// ─── Tree Names ──────────────────────────────────────────────────────────────

/// Tree names in the Sled database.
pub enum TreeName {
    Docs,
    Edges,
    Outgoing,
    Incoming,
    Index,
    Meta,
}

impl TreeName {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Docs => "docs",
            Self::Edges => "edges",
            Self::Outgoing => "outgoing",
            Self::Incoming => "incoming",
            Self::Index => "index",
            Self::Meta => "meta",
        }
    }
}

// ─── Graph Storage Trait ─────────────────────────────────────────────────────

/// The storage interface for the Aden knowledge graph.
///
/// This trait abstracts away the underlying storage engine. Implementations
/// can be Sled, TiKV, or any other KV store. The trait is designed to
/// never change — only the implementation swaps.
pub trait GraphStorage: Send + Sync {
    /// Get a document by anchor.
    fn get_document(&self, anchor: &str) -> Result<Option<Document>, StoreError>;

    /// Get all documents (for full graph rebuild).
    fn get_all_documents(&self) -> Result<HashMap<String, Document>, StoreError>;

    /// Put a document.
    fn put_document(&self, doc: &Document) -> Result<(), StoreError>;

    /// Delete a document.
    fn delete_document(&self, anchor: &str) -> Result<(), StoreError>;

    /// Get outgoing edges for an anchor.
    fn get_outgoing_edges(&self, anchor: &str) -> Result<Vec<(String, EdgeType)>, StoreError>;

    /// Get incoming edges for an anchor.
    fn get_incoming_edges(&self, anchor: &str) -> Result<Vec<(String, EdgeType)>, StoreError>;

    /// Put an edge.
    fn put_edge(
        &self,
        src: &str,
        dst: &str,
        edge_type: EdgeType,
    ) -> Result<(), StoreError>;

    /// Insert many edges in one pass, writing each node's adjacency list once.
    ///
    /// `put_edge` is read-modify-write per edge, so building N edges out of a
    /// single high-degree node (e.g. a module that contains thousands of
    /// symbols) is O(N^2). This groups by endpoint and rewrites each adjacency
    /// list a single time — O(E) — which is what makes linking a large repo
    /// (kernel-scale) feasible. Default impl falls back to repeated `put_edge`.
    fn put_edges_bulk(&self, edges: &[(String, String, EdgeType)]) -> Result<(), StoreError> {
        for (src, dst, et) in edges {
            self.put_edge(src, dst, et.clone())?;
        }
        Ok(())
    }

    /// Delete an edge.
    fn delete_edge(
        &self,
        src: &str,
        dst: &str,
        edge_type: &EdgeType,
    ) -> Result<(), StoreError>;

    /// Get all edges of a type.
    fn get_edges_by_type(
        &self,
        edge_type: &EdgeType,
    ) -> Result<Vec<(String, String)>, StoreError>;

    /// Check if an edge exists.
    fn edge_exists(&self, src: &str, dst: &str, edge_type: &EdgeType) -> Result<bool, StoreError>;

    /// Get all anchors in the graph.
    fn get_all_anchors(&self) -> Result<HashSet<String>, StoreError>;

    /// Get a metadata value.
    fn get_meta(&self, key: &str) -> Result<Option<String>, StoreError>;

    /// Set a metadata value.
    fn put_meta(&self, key: &str, value: &str) -> Result<(), StoreError>;

    /// Run a BFS traversal from an anchor.
    fn bfs(
        &self,
        start: &str,
        depth: usize,
        edge_type: Option<&EdgeType>,
    ) -> Result<Vec<(String, String)>, StoreError>;

    /// Get the neighborhood of an anchor at a given depth.
    fn neighborhood(
        &self,
        anchor: &str,
        depth: usize,
    ) -> Result<HashMap<String, Vec<(String, EdgeType)>>, StoreError>;

    /// Count all nodes.
    fn count_nodes(&self) -> Result<usize, StoreError>;

    /// Count all edges.
    fn count_edges(&self) -> Result<usize, StoreError>;

    /// Clear all data (for testing).
    fn clear(&self) -> Result<(), StoreError>;

    /// Flush changes to disk.
    fn flush(&self) -> Result<(), StoreError>;
}

// ─── Sled Implementation ─────────────────────────────────────────────────────

/// A Sled-backed implementation of [`GraphStorage`].
///
/// Uses Postcard serialization for compact, fast binary storage.
pub struct SledStorage {
    db: sled::Db,
}

impl SledStorage {
    /// Create a new Sled storage at the given path.
    pub fn new(path: &str) -> Result<Self, StoreError> {
        let db = sled::open(path).map_err(|e| StoreError::Io(e.to_string()))?;
        Ok(Self { db })
    }

    /// Get a tree by name, creating it if it doesn't exist.
    fn get_tree(&self, name: &str) -> Result<sled::Tree, StoreError> {
        match self.db.open_tree(name) {
            Ok(tree) => Ok(tree),
            Err(e) => Err(StoreError::Io(e.to_string())),
        }
    }
}

impl GraphStorage for SledStorage {
    fn get_document(&self, anchor: &str) -> Result<Option<Document>, StoreError> {
        let key = doc_key(anchor);
        let tree = self.get_tree(TreeName::Docs.name())?;
        match tree.get(key.as_bytes())? {
            Some(bytes) => Ok(Some(deserialize_document(&bytes)?)),
            None => Ok(None),
        }
    }

    fn get_all_documents(&self) -> Result<HashMap<String, Document>, StoreError> {
        let tree = self.get_tree(TreeName::Docs.name())?;
        let mut docs = HashMap::new();
        for item in tree.iter() {
            let (key, value) = item.map_err(|e| StoreError::Io(e.to_string()))?;
            let key_str = String::from_utf8_lossy(&key);
            if let Some(anchor) = key_str.strip_prefix("doc:") {
                let doc = deserialize_document(&value)?;
                docs.insert(anchor.to_string(), doc);
            }
        }
        Ok(docs)
    }

    fn put_document(&self, doc: &Document) -> Result<(), StoreError> {
        let key = doc_key(&doc.anchor);
        let bytes = serialize_document(doc)?;
        let tree = self.get_tree(TreeName::Docs.name())?;
        tree.insert(key, bytes).map_err(|e| StoreError::Io(e.to_string()))?;
        Ok(())
    }

    fn delete_document(&self, anchor: &str) -> Result<(), StoreError> {
        let key = doc_key(anchor);
        let tree = self.get_tree(TreeName::Docs.name())?;
        tree.remove(key).map_err(|e| StoreError::Io(e.to_string()))?;
        Ok(())
    }

    fn get_outgoing_edges(&self, anchor: &str) -> Result<Vec<(String, EdgeType)>, StoreError> {
        let key = outgoing_key(anchor);
        let tree = self.get_tree(TreeName::Outgoing.name())?;
        match tree.get(key.as_bytes())? {
            Some(bytes) => {
                let edges: Vec<(String, EdgeType)> = deserialize(&bytes)?;
                Ok(edges)
            }
            None => Ok(vec![]),
        }
    }

    fn get_incoming_edges(&self, anchor: &str) -> Result<Vec<(String, EdgeType)>, StoreError> {
        let key = incoming_key(anchor);
        let tree = self.get_tree(TreeName::Incoming.name())?;
        match tree.get(key.as_bytes())? {
            Some(bytes) => {
                let edges: Vec<(String, EdgeType)> = deserialize(&bytes)?;
                Ok(edges)
            }
            None => Ok(vec![]),
        }
    }

    fn put_edge(
        &self,
        src: &str,
        dst: &str,
        edge_type: EdgeType,
    ) -> Result<(), StoreError> {
        // Store edge existence
        let edge_key = edge_key(src, dst, &edge_type);
        let edges_tree = self.get_tree(TreeName::Edges.name())?;
        edges_tree
            .insert(edge_key, vec![])
            .map_err(|e| StoreError::Io(e.to_string()))?;

        // Update outgoing edges
        let out_key = outgoing_key(src);
        let out_tree = self.get_tree(TreeName::Outgoing.name())?;
        let mut edges: Vec<(String, EdgeType)> = match out_tree.get(out_key.as_bytes())? {
            Some(bytes) => deserialize(&bytes)?,
            None => vec![],
        };
        edges.push((dst.to_string(), edge_type.clone()));
        let bytes = serialize(&edges)?;
        out_tree
            .insert(out_key, bytes)
            .map_err(|e| StoreError::Io(e.to_string()))?;

        // Update incoming edges
        let in_key = incoming_key(dst);
        let in_tree = self.get_tree(TreeName::Incoming.name())?;
        let mut edges: Vec<(String, EdgeType)> = match in_tree.get(in_key.as_bytes())? {
            Some(bytes) => deserialize(&bytes)?,
            None => vec![],
        };
        edges.push((src.to_string(), edge_type.clone()));
        let bytes = serialize(&edges)?;
        in_tree
            .insert(in_key, bytes)
            .map_err(|e| StoreError::Io(e.to_string()))?;

        Ok(())
    }

    fn put_edges_bulk(&self, edges: &[(String, String, EdgeType)]) -> Result<(), StoreError> {
        use std::collections::HashMap;

        // Group additions per endpoint so each adjacency list is read+written
        // exactly once instead of once per edge.
        let mut out_add: HashMap<&str, Vec<(String, EdgeType)>> = HashMap::new();
        let mut in_add: HashMap<&str, Vec<(String, EdgeType)>> = HashMap::new();

        let edges_tree = self.get_tree(TreeName::Edges.name())?;
        for (src, dst, edge_type) in edges {
            edges_tree
                .insert(edge_key(src, dst, edge_type), vec![])
                .map_err(|e| StoreError::Io(e.to_string()))?;
            out_add
                .entry(src.as_str())
                .or_default()
                .push((dst.clone(), edge_type.clone()));
            in_add
                .entry(dst.as_str())
                .or_default()
                .push((src.clone(), edge_type.clone()));
        }

        let out_tree = self.get_tree(TreeName::Outgoing.name())?;
        for (src, mut adds) in out_add {
            let key = outgoing_key(src);
            let mut cur: Vec<(String, EdgeType)> = match out_tree.get(key.as_bytes())? {
                Some(bytes) => deserialize(&bytes)?,
                None => Vec::new(),
            };
            cur.append(&mut adds);
            out_tree
                .insert(key, serialize(&cur)?)
                .map_err(|e| StoreError::Io(e.to_string()))?;
        }

        let in_tree = self.get_tree(TreeName::Incoming.name())?;
        for (dst, mut adds) in in_add {
            let key = incoming_key(dst);
            let mut cur: Vec<(String, EdgeType)> = match in_tree.get(key.as_bytes())? {
                Some(bytes) => deserialize(&bytes)?,
                None => Vec::new(),
            };
            cur.append(&mut adds);
            in_tree
                .insert(key, serialize(&cur)?)
                .map_err(|e| StoreError::Io(e.to_string()))?;
        }

        Ok(())
    }

    fn delete_edge(
        &self,
        src: &str,
        dst: &str,
        edge_type: &EdgeType,
    ) -> Result<(), StoreError> {
        // Delete edge existence
        let edge_key = edge_key(src, dst, edge_type);
        let edges_tree = self.get_tree(TreeName::Edges.name())?;
        edges_tree.remove(edge_key).map_err(|e| StoreError::Io(e.to_string()))?;

        // Update outgoing edges
        let out_key = outgoing_key(src);
        let out_tree = self.get_tree(TreeName::Outgoing.name())?;
        let mut edges: Vec<(String, EdgeType)> = match out_tree.get(out_key.as_bytes())? {
            Some(bytes) => deserialize(&bytes)?,
            None => vec![],
        };
        edges.retain(|(_, t)| t != edge_type);
        let bytes = serialize(&edges)?;
        out_tree
            .insert(out_key, bytes)
            .map_err(|e| StoreError::Io(e.to_string()))?;

        // Update incoming edges
        let in_key = incoming_key(dst);
        let in_tree = self.get_tree(TreeName::Incoming.name())?;
        let mut edges: Vec<(String, EdgeType)> = match in_tree.get(in_key.as_bytes())? {
            Some(bytes) => deserialize(&bytes)?,
            None => vec![],
        };
        edges.retain(|(_, t)| t != edge_type);
        let bytes = serialize(&edges)?;
        in_tree
            .insert(in_key, bytes)
            .map_err(|e| StoreError::Io(e.to_string()))?;

        Ok(())
    }

    fn get_edges_by_type(
        &self,
        edge_type: &EdgeType,
    ) -> Result<Vec<(String, String)>, StoreError> {
        let tree = self.get_tree(TreeName::Edges.name())?;
        let edge_str = format!("{:?}", edge_type);
        let mut edges = Vec::new();
        for item in tree.iter() {
            let (key, _) = item.map_err(|e| StoreError::Io(e.to_string()))?;
            let key_str = String::from_utf8_lossy(&key);
            if let Some(suffix) = key_str.strip_prefix(&format!("edge{KEY_SEP}")) {
                let parts: Vec<&str> = suffix.split(KEY_SEP).collect();
                if parts.len() == 3 && parts[2] == edge_str {
                    edges.push((parts[0].to_string(), parts[1].to_string()));
                }
            }
        }
        Ok(edges)
    }

    fn edge_exists(&self, src: &str, dst: &str, edge_type: &EdgeType) -> Result<bool, StoreError> {
        let key = edge_key(src, dst, edge_type);
        let tree = self.get_tree(TreeName::Edges.name())?;
        Ok(tree.get(key.as_bytes())?.is_some())
    }

    fn get_all_anchors(&self) -> Result<HashSet<String>, StoreError> {
        let tree = self.get_tree(TreeName::Docs.name())?;
        let mut anchors = HashSet::new();
        for item in tree.iter() {
            let (key, _) = item.map_err(|e| StoreError::Io(e.to_string()))?;
            let key_str = String::from_utf8_lossy(&key);
            if let Some(anchor) = key_str.strip_prefix("doc:") {
                anchors.insert(anchor.to_string());
            }
        }
        Ok(anchors)
    }

    fn get_meta(&self, key: &str) -> Result<Option<String>, StoreError> {
        let meta_key = meta_key(key);
        let tree = self.get_tree(TreeName::Meta.name())?;
        Ok(tree
            .get(meta_key.as_bytes())?
            .map(|v| String::from_utf8_lossy(&v).to_string()))
    }

    fn put_meta(&self, key: &str, value: &str) -> Result<(), StoreError> {
        let meta_key = meta_key(key);
        let tree = self.get_tree(TreeName::Meta.name())?;
        tree.insert(meta_key, value.as_bytes())
            .map_err(|e| StoreError::Io(e.to_string()))?;
        Ok(())
    }

    fn bfs(
        &self,
        start: &str,
        depth: usize,
        edge_type: Option<&EdgeType>,
    ) -> Result<Vec<(String, String)>, StoreError> {
        let mut visited = HashSet::new();
        let mut queue = vec![(start.to_string(), 0usize)];
        let mut results = Vec::new();

        visited.insert(start.to_string());

        while let Some((current, d)) = queue.pop() {
            if d > depth {
                break;
            }

            let edges = if let Some(et) = edge_type {
                self.get_edges_by_type(et)?
            } else {
                let mut all = Vec::new();
                all.extend(self.get_edges_by_type(&EdgeType::Uses)?);
                all.extend(self.get_edges_by_type(&EdgeType::UsedBy)?);
                all.extend(self.get_edges_by_type(&EdgeType::Requires)?);
                all
            };

            for (src, dst) in &edges {
                if src == &current && !visited.contains(dst) {
                    visited.insert(dst.clone());
                    results.push((current.clone(), dst.clone()));
                    if d < depth {
                        queue.push((dst.clone(), d + 1));
                    }
                }
            }
        }

        Ok(results)
    }

    fn neighborhood(
        &self,
        anchor: &str,
        depth: usize,
    ) -> Result<HashMap<String, Vec<(String, EdgeType)>>, StoreError> {
        let mut visited = HashSet::new();
        let mut queue = vec![(anchor.to_string(), 0usize)];
        let mut result = HashMap::new();

        visited.insert(anchor.to_string());

        while let Some((current, d)) = queue.pop() {
            if d > depth {
                break;
            }

            // Get outgoing edges
            let outgoing = self.get_outgoing_edges(&current)?;
            if !outgoing.is_empty() {
                result.insert(current.clone(), outgoing.clone());
            }

            // Get incoming edges
            let incoming = self.get_incoming_edges(&current)?;
            for (src, _edge_type) in &incoming {
                if !visited.contains(src) {
                    visited.insert(src.clone());
                    queue.push((src.clone(), d + 1));
                }
            }

            for (dst, _edge_type) in &outgoing {
                if !visited.contains(dst) {
                    visited.insert(dst.clone());
                    queue.push((dst.clone(), d + 1));
                }
            }
        }

        Ok(result)
    }

    fn count_nodes(&self) -> Result<usize, StoreError> {
        let tree = self.get_tree(TreeName::Docs.name())?;
        Ok(tree.len())
    }

    fn count_edges(&self) -> Result<usize, StoreError> {
        let tree = self.get_tree(TreeName::Edges.name())?;
        Ok(tree.len())
    }

    fn clear(&self) -> Result<(), StoreError> {
        for tree_name in [
            TreeName::Docs,
            TreeName::Edges,
            TreeName::Outgoing,
            TreeName::Incoming,
            TreeName::Index,
            TreeName::Meta,
        ] {
            let tree = self.get_tree(tree_name.name())?;
            for item in tree.iter() {
                let (key, _) = item.map_err(|e| StoreError::Io(e.to_string()))?;
                tree.remove(key).map_err(|e| StoreError::Io(e.to_string()))?;
            }
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), StoreError> {
        self.db
            .flush()
            .map_err(|e| StoreError::Io(e.to_string()))?;
        Ok(())
    }
}

// ─── Error Types ─────────────────────────────────────────────────────────────

/// Errors that can occur during storage operations.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("IO error: {0}")]
    Io(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("duplicate: {0}")]
    Duplicate(String),
}

impl From<sled::Error> for StoreError {
    fn from(e: sled::Error) -> Self {
        StoreError::Io(e.to_string())
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_path() -> String {
        let path = format!("/tmp/aden-store-test-{}", uuid::Uuid::new_v4());
        path
    }

    #[test]
    fn test_put_and_get_document() {
        let path = temp_path();
        let storage = SledStorage::new(&path).unwrap();

        let doc = Document {
            anchor: "test-anchor".to_string(),
            node_type: aden_core::NodeType::Note,
            attributes: HashMap::new(),
            blocks: vec![],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };

        storage.put_document(&doc).unwrap();
        let retrieved = storage.get_document("test-anchor").unwrap().unwrap();

        assert_eq!(retrieved.anchor, "test-anchor");
        assert_eq!(retrieved.node_type, aden_core::NodeType::Note);

        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn test_put_and_get_edge() {
        let path = temp_path();
        let storage = SledStorage::new(&path).unwrap();

storage
            .put_document(&Document {
                anchor: "src".to_string(),
                node_type: aden_core::NodeType::Note,
                attributes: HashMap::new(),
                blocks: vec![],
                source_span: None,
                metadata: None,
                confidence: 1.0,
            })
            .unwrap();

        storage
            .put_document(&Document {
                anchor: "dst".to_string(),
                node_type: aden_core::NodeType::Note,
                attributes: HashMap::new(),
                blocks: vec![],
                source_span: None,
                metadata: None,
                confidence: 1.0,
            })
            .unwrap();

        storage
            .put_edge("src", "dst", EdgeType::Uses)
            .unwrap();

        let outgoing = storage.get_outgoing_edges("src").unwrap();
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].0, "dst");
        assert_eq!(outgoing[0].1, EdgeType::Uses);

        let incoming = storage.get_incoming_edges("dst").unwrap();
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].0, "src");
        assert_eq!(incoming[0].1, EdgeType::Uses);

        assert!(storage.edge_exists("src", "dst", &EdgeType::Uses).unwrap());

        storage.delete_edge("src", "dst", &EdgeType::Uses).unwrap();
        assert!(!storage.edge_exists("src", "dst", &EdgeType::Uses).unwrap());

        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn test_bfs_traversal() {
        let path = temp_path();
        let storage = SledStorage::new(&path).unwrap();

        // Create a chain: A → B → C
        for anchor in &["A", "B", "C"] {
            storage
                .put_document(&Document {
                    anchor: anchor.to_string(),
                    node_type: aden_core::NodeType::Note,
                    attributes: HashMap::new(),
                    blocks: vec![],
                    source_span: None,
                    metadata: None,
                    confidence: 1.0,
                })
                .unwrap();
        }

        storage.put_edge("A", "B", EdgeType::Uses).unwrap();
        storage.put_edge("B", "C", EdgeType::Uses).unwrap();

        let results = storage.bfs("A", 2, None).unwrap();
        assert_eq!(results.len(), 2);

        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn test_neighborhood() {
        let path = temp_path();
        let storage = SledStorage::new(&path).unwrap();

        // Create a star: center → A, center → B, center → C
        for anchor in &["center", "A", "B", "C"] {
            storage
                .put_document(&Document {
                    anchor: anchor.to_string(),
                    node_type: aden_core::NodeType::Note,
                    attributes: HashMap::new(),
                    blocks: vec![],
                    source_span: None,
                    metadata: None,
                    confidence: 1.0,
                })
                .unwrap();
        }

        for target in &["A", "B", "C"] {
            storage
                .put_edge("center", target, EdgeType::Uses)
                .unwrap();
        }

        let neighborhood = storage.neighborhood("center", 1).unwrap();
        assert_eq!(neighborhood.len(), 1); // only "center" at depth 1
        assert_eq!(neighborhood["center"].len(), 3);

        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn test_meta() {
        let path = temp_path();
        let storage = SledStorage::new(&path).unwrap();

        storage.put_meta("version", "1.0").unwrap();
        let version = storage.get_meta("version").unwrap().unwrap();
        assert_eq!(version, "1.0");

        let missing = storage.get_meta("nonexistent").unwrap();
        assert!(missing.is_none());

        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn test_serialization_roundtrip() {
        let doc = Document {
            anchor: "test".to_string(),
            node_type: aden_core::NodeType::Note,
            attributes: {
                let mut m = HashMap::new();
                m.insert("key".to_string(), "value".to_string());
                m
            },
            blocks: vec![],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };

        let bytes = serialize_document(&doc).unwrap();
        let retrieved = deserialize_document(&bytes).unwrap();

        assert_eq!(retrieved.anchor, doc.anchor);
        assert_eq!(retrieved.node_type, doc.node_type);
        assert_eq!(
            retrieved.attributes.get("key"),
            doc.attributes.get("key")
        );
    }

    #[test]
    fn test_count_nodes_edges() {
        let path = temp_path();
        let storage = SledStorage::new(&path).unwrap();

        assert_eq!(storage.count_nodes().unwrap(), 0);
        assert_eq!(storage.count_edges().unwrap(), 0);

        storage
            .put_document(&Document {
                anchor: "A".to_string(),
                node_type: aden_core::NodeType::Note,
                attributes: HashMap::new(),
                blocks: vec![],
                source_span: None,
                metadata: None,
                confidence: 1.0,
            })
            .unwrap();

        storage
            .put_document(&Document {
                anchor: "B".to_string(),
                node_type: aden_core::NodeType::Note,
                attributes: HashMap::new(),
                blocks: vec![],
                source_span: None,
                metadata: None,
                confidence: 1.0,
            })
            .unwrap();

        storage.put_edge("A", "B", EdgeType::Uses).unwrap();

        assert_eq!(storage.count_nodes().unwrap(), 2);
        assert_eq!(storage.count_edges().unwrap(), 1);

        fs::remove_dir_all(&path).ok();
    }
}
