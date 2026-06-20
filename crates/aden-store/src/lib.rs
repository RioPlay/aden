// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Storage abstraction layer for the Aden knowledge graph.
//!
//! Defines the [`GraphStorage`] trait and provides a fjall (LSM-tree)
//! implementation with Postcard serialization.
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
//! | `bases` | `base:{anchor}` | canonical contract text as of last gen (UTF-8) |

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
///
/// Rehydrates `source_span` from attributes when absent — the read-side inverse
/// of [`aden_core::Document::with_span`]. Symbols persist their span only in
/// attributes (`source_file`/`start_line`/`end_line`/`start_byte`/`end_byte`);
/// without this the struct field is permanently `None` for every stored symbol,
/// so consumers reading `.source_span` (e.g. the dead-code linter) silently
/// degrade. Span-less nodes (prose/term/note) have no source attributes, so
/// `from_attributes` returns `None` and they are left untouched.
pub fn deserialize_document(bytes: &[u8]) -> Result<Document, StoreError> {
    let mut doc: Document = deserialize(bytes)?;
    if doc.source_span.is_none() {
        doc.source_span = aden_core::SourceSpan::from_attributes(&doc.attributes);
    }
    Ok(doc)
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

/// Build a base-snapshot key.
pub fn base_key(anchor: &str) -> String {
    format!("base:{anchor}")
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
    Bases,
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
            Self::Bases => "bases",
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

    /// Record the canonical contract text for `anchor` as produced by the
    /// last `gen` run — the `base` layer for three-way merges.
    ///
    /// Stored as the `emit_contract_document` text rather than a serialized
    /// struct: `parse_contract` is its exact inverse, so the round-trip is
    /// lossless, and text survives struct-schema evolution and stays
    /// debuggable with plain tools.
    fn put_base_snapshot(&self, anchor: &str, text: &str) -> Result<(), StoreError>;

    /// Load the base snapshot recorded by the last `gen` run, if any.
    /// `None` for stores written before snapshots existed (callers fall back
    /// to reconstructing the base from the stored Document).
    fn get_base_snapshot(&self, anchor: &str) -> Result<Option<String>, StoreError>;

    /// Remove the base snapshot for `anchor`. `delete_document` and
    /// `delete_node` cascade this automatically.
    fn delete_base_snapshot(&self, anchor: &str) -> Result<(), StoreError>;

    /// Get outgoing edges for an anchor.
    fn get_outgoing_edges(&self, anchor: &str) -> Result<Vec<(String, EdgeType)>, StoreError>;

    /// Get incoming edges for an anchor.
    fn get_incoming_edges(&self, anchor: &str) -> Result<Vec<(String, EdgeType)>, StoreError>;

    /// Put an edge.
    fn put_edge(&self, src: &str, dst: &str, edge_type: EdgeType) -> Result<(), StoreError>;

    /// Insert many edges in one pass, writing each node's adjacency list once.
    ///
    /// `put_edge` is read-modify-write per edge, so building N edges out of a
    /// single high-degree node (e.g. a module that contains thousands of
    /// symbols) is O(N^2). This groups by endpoint and rewrites each adjacency
    /// list a single time — O(E) — which is what makes linking a large repo
    /// (kernel-scale) feasible. Default impl falls back to repeated `put_edge`.
    fn put_edges_bulk(&self, edges: &[(String, String, EdgeType)]) -> Result<(), StoreError> {
        for (src, dst, et) in edges {
            self.put_edge(src, dst, *et)?;
        }
        Ok(())
    }

    /// Delete an edge.
    fn delete_edge(&self, src: &str, dst: &str, edge_type: &EdgeType) -> Result<(), StoreError>;

    /// Delete a node and ALL of its incident edges in one operation.
    ///
    /// This is the safe way to remove a symbol during incremental re-index:
    /// `delete_document` alone leaves dangling adjacency entries. `delete_node`
    /// removes the document, every edge incident to `anchor` (both directions),
    /// the back-references in each neighbour's mirror list, and the anchor's own
    /// outgoing/incoming adjacency lists — so no dangling reference survives.
    ///
    /// The default implementation composes the existing per-edge APIs; backends
    /// may override for efficiency.
    fn delete_node(&self, anchor: &str) -> Result<(), StoreError> {
        for (dst, et) in self.get_outgoing_edges(anchor)? {
            self.delete_edge(anchor, &dst, &et)?;
        }
        for (src, et) in self.get_incoming_edges(anchor)? {
            self.delete_edge(&src, anchor, &et)?;
        }
        self.delete_document(anchor)?;
        Ok(())
    }

    /// Get all edges of a type.
    fn get_edges_by_type(&self, edge_type: &EdgeType) -> Result<Vec<(String, String)>, StoreError>;

    /// Get all edges in one pass, bucketed by type.
    ///
    /// Default implementation calls `get_edges_by_type` for every variant (one
    /// full scan per type). Backends should override with a single scan that
    /// reads the type from the key — reducing 32 scans to 1 for graph loads.
    fn get_all_edges(&self) -> Result<Vec<(String, String, EdgeType)>, StoreError> {
        let mut edges = Vec::new();
        for et in &EdgeType::ALL {
            for (src, dst) in self.get_edges_by_type(et)? {
                edges.push((src, dst, *et));
            }
        }
        Ok(edges)
    }

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

// ─── Backend selection ───────────────────────────────────────────────────────

pub mod fjall_store;
pub use fjall_store::FjallStorage;

/// The active storage backend. Aden uses the fjall (pure-Rust LSM-tree) engine.
/// The `Storage` alias keeps call sites engine-agnostic, so the `GraphStorage`
/// trait remains the single seam if another backend is ever added.
pub type Storage = FjallStorage;

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

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_path() -> String {
        std::env::temp_dir()
            .join(format!("aden-store-test-{}", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .into_owned()
    }

    fn note_doc(anchor: &str) -> Document {
        Document {
            anchor: anchor.to_string(),
            node_type: aden_core::NodeType::Note,
            attributes: HashMap::new(),
            blocks: vec![],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        }
    }

    #[test]
    fn base_snapshot_round_trips() {
        let path = temp_path();
        let storage = Storage::new(&path).unwrap();
        let text = ":anchor: foo\n\n[generated#foo]\n----\nfn foo() {}\n----\n";
        storage.put_base_snapshot("foo", text).unwrap();
        assert_eq!(
            storage.get_base_snapshot("foo").unwrap().as_deref(),
            Some(text)
        );
        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn base_snapshot_missing_is_none() {
        let path = temp_path();
        let storage = Storage::new(&path).unwrap();
        assert_eq!(storage.get_base_snapshot("nope").unwrap(), None);
        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn base_snapshot_overwrites() {
        let path = temp_path();
        let storage = Storage::new(&path).unwrap();
        storage.put_base_snapshot("foo", "v1").unwrap();
        storage.put_base_snapshot("foo", "v2").unwrap();
        assert_eq!(
            storage.get_base_snapshot("foo").unwrap().as_deref(),
            Some("v2")
        );
        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn delete_node_cascades_base_snapshot() {
        // A pruned symbol must not leave a stale merge base behind — a later
        // re-gen of the same anchor would three-way merge against a ghost.
        let path = temp_path();
        let storage = Storage::new(&path).unwrap();
        storage.put_document(&note_doc("foo")).unwrap();
        storage.put_base_snapshot("foo", "snapshot text").unwrap();
        storage.delete_node("foo").unwrap();
        assert!(storage.get_document("foo").unwrap().is_none());
        assert_eq!(storage.get_base_snapshot("foo").unwrap(), None);
        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn delete_document_cascades_base_snapshot() {
        let path = temp_path();
        let storage = Storage::new(&path).unwrap();
        storage.put_document(&note_doc("foo")).unwrap();
        storage.put_base_snapshot("foo", "snapshot text").unwrap();
        storage.delete_document("foo").unwrap();
        assert_eq!(storage.get_base_snapshot("foo").unwrap(), None);
        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn test_put_and_get_document() {
        let path = temp_path();
        let storage = Storage::new(&path).unwrap();

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
        let storage = Storage::new(&path).unwrap();

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

        storage.put_edge("src", "dst", EdgeType::Uses).unwrap();

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
        let storage = Storage::new(&path).unwrap();

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
        let storage = Storage::new(&path).unwrap();

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
            storage.put_edge("center", target, EdgeType::Uses).unwrap();
        }

        let neighborhood = storage.neighborhood("center", 1).unwrap();
        assert_eq!(neighborhood.len(), 1); // only "center" at depth 1
        assert_eq!(neighborhood["center"].len(), 3);

        fs::remove_dir_all(&path).ok();
    }

    fn note(anchor: &str) -> Document {
        Document {
            anchor: anchor.to_string(),
            node_type: aden_core::NodeType::Note,
            attributes: HashMap::new(),
            blocks: vec![],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        }
    }

    #[test]
    fn delete_edge_removes_only_the_named_endpoint() {
        // Regression: delete_edge used to match on edge_type only, so removing
        // one (src,A,Uses) edge wiped (src,B,Uses) too. It must remove exactly
        // the named (src,dst,type) edge and leave the sibling intact.
        let path = temp_path();
        let storage = Storage::new(&path).unwrap();
        for a in &["src", "A", "B"] {
            storage.put_document(&note(a)).unwrap();
        }
        storage.put_edge("src", "A", EdgeType::Uses).unwrap();
        storage.put_edge("src", "B", EdgeType::Uses).unwrap();

        storage.delete_edge("src", "A", &EdgeType::Uses).unwrap();

        let out = storage.get_outgoing_edges("src").unwrap();
        assert_eq!(
            out.len(),
            1,
            "only the (src,A) edge should be gone: {out:?}"
        );
        assert_eq!(out[0].0, "B");
        assert!(!storage.edge_exists("src", "A", &EdgeType::Uses).unwrap());
        assert!(storage.edge_exists("src", "B", &EdgeType::Uses).unwrap());
        // B's incoming mirror must still reference src.
        assert_eq!(storage.get_incoming_edges("B").unwrap().len(), 1);

        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn delete_node_cascades_and_leaves_no_dangling_edges() {
        // center is referenced by `up` and references `down`. Deleting center
        // must remove its doc, its edges in both directions, and the mirror
        // back-references in up.outgoing and down.incoming.
        let path = temp_path();
        let storage = Storage::new(&path).unwrap();
        for a in &["center", "up", "down"] {
            storage.put_document(&note(a)).unwrap();
        }
        storage.put_edge("up", "center", EdgeType::Calls).unwrap();
        storage.put_edge("center", "down", EdgeType::Calls).unwrap();

        storage.delete_node("center").unwrap();

        // Doc gone.
        assert!(storage.get_document("center").unwrap().is_none());
        assert!(!storage.get_all_anchors().unwrap().contains("center"));
        // No dangling edges referencing center in either neighbour's mirror.
        assert!(
            storage.get_outgoing_edges("up").unwrap().is_empty(),
            "up should no longer list center as a callee"
        );
        assert!(
            storage.get_incoming_edges("down").unwrap().is_empty(),
            "down should no longer list center as a caller"
        );
        assert!(
            !storage
                .edge_exists("up", "center", &EdgeType::Calls)
                .unwrap()
        );
        assert!(
            !storage
                .edge_exists("center", "down", &EdgeType::Calls)
                .unwrap()
        );
        // Neighbours themselves survive.
        assert!(storage.get_document("up").unwrap().is_some());
        assert!(storage.get_document("down").unwrap().is_some());

        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn test_meta() {
        let path = temp_path();
        let storage = Storage::new(&path).unwrap();

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
        assert_eq!(retrieved.attributes.get("key"), doc.attributes.get("key"));
    }

    #[test]
    fn deserialize_rehydrates_source_span_from_attributes() {
        // Symbols persist their span only in attributes — the extractor writes
        // source_file/start_line/end_line/start_byte/end_byte and leaves the
        // `source_span` struct field None on the wire. Deserialization must
        // rebuild the field (the read-side inverse of `Document::with_span`) so
        // consumers that read `.source_span` (e.g. the dead-code linter) get the
        // real file:line instead of degrading.
        let mut attrs = HashMap::new();
        attrs.insert("source_file".to_string(), "/repo/src/lib.rs".to_string());
        attrs.insert("start_line".to_string(), "42".to_string());
        attrs.insert("end_line".to_string(), "99".to_string());
        attrs.insert("start_byte".to_string(), "1000".to_string());
        attrs.insert("end_byte".to_string(), "2500".to_string());

        let doc = Document {
            anchor: "aden://module/src/lib.rs#foo".to_string(),
            node_type: aden_core::NodeType::Function,
            attributes: attrs,
            blocks: vec![],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };

        let bytes = serialize_document(&doc).unwrap();
        let retrieved = deserialize_document(&bytes).unwrap();

        let span = retrieved
            .source_span
            .expect("source_span must be rehydrated from attributes");
        assert_eq!(span.file, "/repo/src/lib.rs");
        assert_eq!(span.start_line, 42);
        assert_eq!(span.end_line, 99);
        assert_eq!(span.start_byte, 1000);
        assert_eq!(span.end_byte, 2500);
    }

    #[test]
    fn deserialize_leaves_spanless_documents_untouched() {
        // A node with no source attributes (prose/term/note) must stay None,
        // never synthesize a bogus span.
        let doc = note_doc("plain");
        let bytes = serialize_document(&doc).unwrap();
        let retrieved = deserialize_document(&bytes).unwrap();
        assert!(retrieved.source_span.is_none());
    }

    #[test]
    fn test_count_nodes_edges() {
        let path = temp_path();
        let storage = Storage::new(&path).unwrap();

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
