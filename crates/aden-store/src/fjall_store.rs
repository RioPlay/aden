// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fjall-backed implementation of [`GraphStorage`].
//!
//! Fjall is a pure-Rust LSM-tree engine. Compared to sled (which is still alpha
//! and pulls unmaintained transitive deps), it gives aden's write-heavy `gen`
//! ingest better throughput and — via blob compression + key/value separation —
//! a much smaller on-disk store, which is what matters as data pools grow.
//!
//! The key/value layout mirrors [`SledStorage`](crate::SledStorage) exactly;
//! sled "trees" map 1:1 onto fjall "keyspaces", so the two backends are
//! interchangeable behind the [`GraphStorage`] trait.
//!
//! ## Fjall 3 API mapping
//!
//! | fjall 2.x             | fjall 3.x                |
//! |-----------------------|--------------------------|
//! | `Keyspace` (root)     | `Database`               |
//! | `PartitionHandle`     | `Keyspace`               |
//! | `PartitionCreateOptions` | `KeyspaceCreateOptions` |
//! | `Config::new(p).open()` | `Database::builder(p).open()` |
//! | `ks.open_partition(n, opts)` | `db.keyspace(n, opts_fn)` |
//! | Iterator yields `Result<(key, val)>` | Iterator yields `Guard`; call `.into_inner()` |

use crate::{
    GraphStorage, KEY_SEP, StoreError, TreeName, base_key, deserialize, deserialize_document,
    doc_key, edge_key, incoming_key, meta_key, outgoing_key, serialize, serialize_document,
};
use aden_core::{Document, EdgeType};
use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};
use std::collections::{HashMap, HashSet};

impl From<fjall::Error> for StoreError {
    fn from(e: fjall::Error) -> Self {
        // A version mismatch means the on-disk store was written in a different
        // engine format (e.g. after a fjall major-version upgrade). Surface it as
        // a distinct, catchable variant so a creation path can wipe-and-rebuild
        // the (rebuildable, ADR-003) cache. Every OTHER fjall error stays on the
        // generic `Io` path — a real I/O failure must never be confused with a
        // recoverable format change, or it could trigger deletion of a store that
        // is merely temporarily unreadable.
        if matches!(e, fjall::Error::InvalidVersion(_)) {
            StoreError::IncompatibleVersion(e.to_string())
        } else {
            StoreError::Io(e.to_string())
        }
    }
}

/// A Fjall (LSM-tree) backed implementation of [`GraphStorage`].
pub struct FjallStorage {
    // Database must outlive the keyspaces; kept for `persist` (flush).
    db: Database,
    docs: Keyspace,
    edges: Keyspace,
    outgoing: Keyspace,
    incoming: Keyspace,
    index: Keyspace,
    meta: Keyspace,
    bases: Keyspace,
}

impl FjallStorage {
    /// Open (or create) a Fjall store at the given path.
    pub fn new(path: &str) -> Result<Self, StoreError> {
        let db = Database::builder(path).open()?;
        let open = |name: &str| -> Result<Keyspace, StoreError> {
            db.keyspace(name, KeyspaceCreateOptions::default)
                .map_err(StoreError::from)
        };
        Ok(Self {
            docs: open(TreeName::Docs.name())?,
            edges: open(TreeName::Edges.name())?,
            outgoing: open(TreeName::Outgoing.name())?,
            incoming: open(TreeName::Incoming.name())?,
            index: open(TreeName::Index.name())?,
            meta: open(TreeName::Meta.name())?,
            bases: open(TreeName::Bases.name())?,
            db,
        })
    }

    /// Open an *existing* Fjall store without creating it.
    ///
    /// Unlike [`new`](Self::new) — which calls `Database::builder(path).open()` and so
    /// materializes the directory — this returns [`StoreError::NotFound`] when
    /// the store is absent. Read commands use this so that running e.g.
    /// `aden query` before `aden gen` yields a clear error rather than silently
    /// creating an empty store (ADR-003 §5: reads never create).
    pub fn open_existing(path: &str) -> Result<Self, StoreError> {
        if !std::path::Path::new(path).exists() {
            return Err(StoreError::NotFound(format!(
                "no store at {path}; run 'aden gen' at the project root first"
            )));
        }
        Self::new(path)
    }
}

impl GraphStorage for FjallStorage {
    fn get_document(&self, anchor: &str) -> Result<Option<Document>, StoreError> {
        match self.docs.get(doc_key(anchor))? {
            Some(bytes) => Ok(Some(deserialize_document(&bytes)?)),
            None => Ok(None),
        }
    }

    fn get_all_documents(&self) -> Result<HashMap<String, Document>, StoreError> {
        let mut docs = HashMap::new();
        for guard in self.docs.iter() {
            let (key, value) = guard.into_inner()?;
            let key_str = String::from_utf8_lossy(&key);
            if let Some(anchor) = key_str.strip_prefix("doc:") {
                docs.insert(anchor.to_string(), deserialize_document(&value)?);
            }
        }
        Ok(docs)
    }

    fn put_document(&self, doc: &Document) -> Result<(), StoreError> {
        self.docs
            .insert(doc_key(&doc.anchor), serialize_document(doc)?)?;
        Ok(())
    }

    fn delete_document(&self, anchor: &str) -> Result<(), StoreError> {
        self.docs.remove(doc_key(anchor))?;
        // A document's base snapshot is meaningless without the document.
        self.bases.remove(base_key(anchor))?;
        Ok(())
    }

    fn put_base_snapshot(&self, anchor: &str, text: &str) -> Result<(), StoreError> {
        self.bases.insert(base_key(anchor), text.as_bytes())?;
        Ok(())
    }

    fn put_documents_bulk(&self, docs: &[(Document, String)]) -> Result<(), StoreError> {
        // One atomic fjall batch per call: every item shares a single journal
        // append and sequence number, replacing 2*N individual `insert` calls
        // (doc + base snapshot) with one. Because the whole batch gets the same
        // seqno, anchors within `docs` MUST be distinct (the gen caller batches
        // one source file at a time to guarantee this); the doc and its base
        // commit together so a crash never leaves one without the other.
        // Durability is the caller's trailing `flush()` (SyncAll) — the commit
        // here only appends to the journal and applies to the memtable.
        if docs.is_empty() {
            return Ok(());
        }
        let mut batch = self.keyspace.batch();
        for (doc, snapshot) in docs {
            batch.insert(&self.docs, doc_key(&doc.anchor), serialize_document(doc)?);
            batch.insert(&self.bases, base_key(&doc.anchor), snapshot.as_bytes());
        }
        batch.commit()?;
        Ok(())
    }

    fn get_base_snapshot(&self, anchor: &str) -> Result<Option<String>, StoreError> {
        match self.bases.get(base_key(anchor))? {
            Some(bytes) => Ok(Some(String::from_utf8(bytes.to_vec()).map_err(|e| {
                StoreError::Serialization(format!("base snapshot for '{anchor}' not UTF-8: {e}"))
            })?)),
            None => Ok(None),
        }
    }

    fn delete_base_snapshot(&self, anchor: &str) -> Result<(), StoreError> {
        self.bases.remove(base_key(anchor))?;
        Ok(())
    }

    fn get_outgoing_edges(&self, anchor: &str) -> Result<Vec<(String, EdgeType)>, StoreError> {
        match self.outgoing.get(outgoing_key(anchor))? {
            Some(bytes) => Ok(deserialize(&bytes)?),
            None => Ok(vec![]),
        }
    }

    fn get_incoming_edges(&self, anchor: &str) -> Result<Vec<(String, EdgeType)>, StoreError> {
        match self.incoming.get(incoming_key(anchor))? {
            Some(bytes) => Ok(deserialize(&bytes)?),
            None => Ok(vec![]),
        }
    }

    fn put_edge(&self, src: &str, dst: &str, edge_type: EdgeType) -> Result<(), StoreError> {
        self.edges
            .insert(edge_key(src, dst, &edge_type), &[] as &[u8])?;

        let out_key = outgoing_key(src);
        let mut out: Vec<(String, EdgeType)> = match self.outgoing.get(&out_key)? {
            Some(b) => deserialize(&b)?,
            None => vec![],
        };
        out.push((dst.to_string(), edge_type));
        self.outgoing.insert(&out_key, serialize(&out)?)?;

        let in_key = incoming_key(dst);
        let mut inc: Vec<(String, EdgeType)> = match self.incoming.get(&in_key)? {
            Some(b) => deserialize(&b)?,
            None => vec![],
        };
        inc.push((src.to_string(), edge_type));
        self.incoming.insert(&in_key, serialize(&inc)?)?;
        Ok(())
    }

    fn put_edges_bulk(&self, edges: &[(String, String, EdgeType)]) -> Result<(), StoreError> {
        let mut out_add: HashMap<&str, Vec<(String, EdgeType)>> = HashMap::new();
        let mut in_add: HashMap<&str, Vec<(String, EdgeType)>> = HashMap::new();
        for (src, dst, edge_type) in edges {
            self.edges
                .insert(edge_key(src, dst, edge_type), &[] as &[u8])?;
            out_add
                .entry(src.as_str())
                .or_default()
                .push((dst.clone(), *edge_type));
            in_add
                .entry(dst.as_str())
                .or_default()
                .push((src.clone(), *edge_type));
        }
        for (src, mut adds) in out_add {
            let key = outgoing_key(src);
            let mut cur: Vec<(String, EdgeType)> = match self.outgoing.get(&key)? {
                Some(b) => deserialize(&b)?,
                None => vec![],
            };
            cur.append(&mut adds);
            // Dedup so re-indexing an unchanged edge does not inflate the list.
            // EdgeType has no Ord, so dedup via a seen-set, preserving order.
            let mut seen = HashSet::new();
            cur.retain(|e| seen.insert(e.clone()));
            self.outgoing.insert(&key, serialize(&cur)?)?;
        }
        for (dst, mut adds) in in_add {
            let key = incoming_key(dst);
            let mut cur: Vec<(String, EdgeType)> = match self.incoming.get(&key)? {
                Some(b) => deserialize(&b)?,
                None => vec![],
            };
            cur.append(&mut adds);
            let mut seen = HashSet::new();
            cur.retain(|e| seen.insert(e.clone()));
            self.incoming.insert(&key, serialize(&cur)?)?;
        }
        Ok(())
    }

    fn delete_edge(&self, src: &str, dst: &str, edge_type: &EdgeType) -> Result<(), StoreError> {
        self.edges.remove(edge_key(src, dst, edge_type))?;

        // Drop ONLY the (dst, edge_type) entry from src's outgoing list and the
        // (src, edge_type) entry from dst's incoming list. The retain predicate
        // must match the endpoint as well as the type — matching on type alone
        // would wipe every same-type neighbour from the adjacency list.
        let out_key = outgoing_key(src);
        if let Some(b) = self.outgoing.get(&out_key)? {
            let mut out: Vec<(String, EdgeType)> = deserialize(&b)?;
            out.retain(|(n, t)| !(n == dst && t == edge_type));
            self.outgoing.insert(&out_key, serialize(&out)?)?;
        }
        let in_key = incoming_key(dst);
        if let Some(b) = self.incoming.get(&in_key)? {
            let mut inc: Vec<(String, EdgeType)> = deserialize(&b)?;
            inc.retain(|(n, t)| !(n == src && t == edge_type));
            self.incoming.insert(&in_key, serialize(&inc)?)?;
        }
        Ok(())
    }

    fn delete_node(&self, anchor: &str) -> Result<(), StoreError> {
        // Remove the doc record and its merge base — a stale snapshot would
        // make a future re-gen of the same anchor merge against a ghost.
        self.docs.remove(doc_key(anchor))?;
        self.bases.remove(base_key(anchor))?;

        // Outgoing: for each (dst, et), drop the edge key and remove anchor from
        // dst's incoming mirror list.
        let out: Vec<(String, EdgeType)> = match self.outgoing.get(outgoing_key(anchor))? {
            Some(b) => deserialize(&b)?,
            None => vec![],
        };
        for (dst, et) in &out {
            self.edges.remove(edge_key(anchor, dst, et))?;
            let in_key = incoming_key(dst);
            if let Some(b) = self.incoming.get(&in_key)? {
                let mut inc: Vec<(String, EdgeType)> = deserialize(&b)?;
                inc.retain(|(n, t)| !(n == anchor && t == et));
                self.incoming.insert(&in_key, serialize(&inc)?)?;
            }
        }

        // Incoming: for each (src, et), drop the edge key and remove anchor from
        // src's outgoing mirror list.
        let inc: Vec<(String, EdgeType)> = match self.incoming.get(incoming_key(anchor))? {
            Some(b) => deserialize(&b)?,
            None => vec![],
        };
        for (src, et) in &inc {
            self.edges.remove(edge_key(src, anchor, et))?;
            let out_key = outgoing_key(src);
            if let Some(b) = self.outgoing.get(&out_key)? {
                let mut o: Vec<(String, EdgeType)> = deserialize(&b)?;
                o.retain(|(n, t)| !(n == anchor && t == et));
                self.outgoing.insert(&out_key, serialize(&o)?)?;
            }
        }

        // Finally drop the anchor's own adjacency lists.
        self.outgoing.remove(outgoing_key(anchor))?;
        self.incoming.remove(incoming_key(anchor))?;
        Ok(())
    }

    fn get_edges_by_type(&self, edge_type: &EdgeType) -> Result<Vec<(String, String)>, StoreError> {
        let edge_str = format!("{:?}", edge_type);
        let prefix = format!("edge{KEY_SEP}");
        let mut edges = Vec::new();
        for guard in self.edges.iter() {
            let (key, _) = guard.into_inner()?;
            let key_str = String::from_utf8_lossy(&key);
            if let Some(suffix) = key_str.strip_prefix(&prefix) {
                let parts: Vec<&str> = suffix.split(KEY_SEP).collect();
                if parts.len() == 3 && parts[2] == edge_str {
                    edges.push((parts[0].to_string(), parts[1].to_string()));
                }
            }
        }
        Ok(edges)
    }

    fn get_all_edges(&self) -> Result<Vec<(String, String, EdgeType)>, StoreError> {
        // Build a debug-string → EdgeType lookup once so we can bucket by type
        // in a single scan of the edges partition instead of one scan per type.
        let type_map: HashMap<String, EdgeType> = EdgeType::ALL
            .iter()
            .map(|et| (format!("{et:?}"), *et))
            .collect();

        let prefix = format!("edge{KEY_SEP}");
        let mut edges = Vec::new();
        for guard in self.edges.iter() {
            let (key, _) = guard.into_inner()?;
            let key_str = String::from_utf8_lossy(&key);
            if let Some(suffix) = key_str.strip_prefix(&prefix) {
                let parts: Vec<&str> = suffix.split(KEY_SEP).collect();
                if parts.len() == 3 {
                    if let Some(&et) = type_map.get(parts[2]) {
                        edges.push((parts[0].to_string(), parts[1].to_string(), et));
                    }
                }
            }
        }
        Ok(edges)
    }

    fn edge_exists(&self, src: &str, dst: &str, edge_type: &EdgeType) -> Result<bool, StoreError> {
        Ok(self.edges.contains_key(edge_key(src, dst, edge_type))?)
    }

    fn get_all_anchors(&self) -> Result<HashSet<String>, StoreError> {
        let mut anchors = HashSet::new();
        for guard in self.docs.iter() {
            let (key, _) = guard.into_inner()?;
            let key_str = String::from_utf8_lossy(&key);
            if let Some(anchor) = key_str.strip_prefix("doc:") {
                anchors.insert(anchor.to_string());
            }
        }
        Ok(anchors)
    }

    fn get_meta(&self, key: &str) -> Result<Option<String>, StoreError> {
        match self.meta.get(meta_key(key))? {
            Some(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).to_string())),
            None => Ok(None),
        }
    }

    fn put_meta(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.meta.insert(meta_key(key), value.as_bytes())?;
        Ok(())
    }

    fn bfs(
        &self,
        start: &str,
        depth: usize,
        edge_type: Option<&EdgeType>,
    ) -> Result<Vec<(String, String)>, StoreError> {
        // Walk per-node adjacency lists (bounded), unlike a full edge scan.
        let mut visited = HashSet::new();
        let mut queue = vec![(start.to_string(), 0usize)];
        let mut results = Vec::new();
        visited.insert(start.to_string());
        while let Some((current, d)) = queue.pop() {
            if d >= depth {
                continue;
            }
            for (dst, et) in self.get_outgoing_edges(&current)? {
                if let Some(want) = edge_type {
                    if &et != want {
                        continue;
                    }
                }
                results.push((current.clone(), dst.clone()));
                if visited.insert(dst.clone()) {
                    queue.push((dst, d + 1));
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
                continue;
            }
            let outgoing = self.get_outgoing_edges(&current)?;
            if !outgoing.is_empty() {
                result.insert(current.clone(), outgoing.clone());
            }
            for (src, _) in self.get_incoming_edges(&current)? {
                if visited.insert(src.clone()) {
                    queue.push((src, d + 1));
                }
            }
            for (dst, _) in &outgoing {
                if visited.insert(dst.clone()) {
                    queue.push((dst.clone(), d + 1));
                }
            }
        }
        Ok(result)
    }

    fn count_nodes(&self) -> Result<usize, StoreError> {
        Ok(self.docs.approximate_len())
    }

    fn count_edges(&self) -> Result<usize, StoreError> {
        Ok(self.edges.approximate_len())
    }

    fn clear(&self) -> Result<(), StoreError> {
        for part in [
            &self.docs,
            &self.edges,
            &self.outgoing,
            &self.incoming,
            &self.index,
            &self.meta,
        ] {
            let keys: Vec<_> = part
                .iter()
                .filter_map(|g| g.into_inner().ok().map(|(k, _)| k))
                .collect();
            for k in keys {
                part.remove(k)?;
            }
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), StoreError> {
        self.db.persist(PersistMode::SyncAll)?;
        Ok(())
    }
}
