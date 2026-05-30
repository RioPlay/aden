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
//! sled "trees" map 1:1 onto fjall "partitions", so the two backends are
//! interchangeable behind the [`GraphStorage`] trait.

use crate::{
    deserialize, deserialize_document, doc_key, edge_key, incoming_key, meta_key, outgoing_key,
    serialize, serialize_document, GraphStorage, StoreError, TreeName, KEY_SEP,
};
use aden_core::{Document, EdgeType};
use fjall::{Config, Keyspace, PartitionCreateOptions, PartitionHandle, PersistMode};
use std::collections::{HashMap, HashSet};

impl From<fjall::Error> for StoreError {
    fn from(e: fjall::Error) -> Self {
        StoreError::Io(e.to_string())
    }
}

/// A Fjall (LSM-tree) backed implementation of [`GraphStorage`].
pub struct FjallStorage {
    // Keyspace must outlive the partitions; kept for `persist` (flush).
    keyspace: Keyspace,
    docs: PartitionHandle,
    edges: PartitionHandle,
    outgoing: PartitionHandle,
    incoming: PartitionHandle,
    index: PartitionHandle,
    meta: PartitionHandle,
}

impl FjallStorage {
    /// Open (or create) a Fjall store at the given path.
    pub fn new(path: &str) -> Result<Self, StoreError> {
        let keyspace = Config::new(path).open()?;
        let open = |name: &str| -> Result<PartitionHandle, StoreError> {
            keyspace
                .open_partition(name, PartitionCreateOptions::default())
                .map_err(StoreError::from)
        };
        Ok(Self {
            docs: open(TreeName::Docs.name())?,
            edges: open(TreeName::Edges.name())?,
            outgoing: open(TreeName::Outgoing.name())?,
            incoming: open(TreeName::Incoming.name())?,
            index: open(TreeName::Index.name())?,
            meta: open(TreeName::Meta.name())?,
            keyspace,
        })
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
        for item in self.docs.iter() {
            let (key, value) = item?;
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
        self.edges.insert(edge_key(src, dst, &edge_type), [])?;

        let out_key = outgoing_key(src);
        let mut out: Vec<(String, EdgeType)> = match self.outgoing.get(&out_key)? {
            Some(b) => deserialize(&b)?,
            None => vec![],
        };
        out.push((dst.to_string(), edge_type.clone()));
        self.outgoing.insert(&out_key, serialize(&out)?)?;

        let in_key = incoming_key(dst);
        let mut inc: Vec<(String, EdgeType)> = match self.incoming.get(&in_key)? {
            Some(b) => deserialize(&b)?,
            None => vec![],
        };
        inc.push((src.to_string(), edge_type.clone()));
        self.incoming.insert(&in_key, serialize(&inc)?)?;
        Ok(())
    }

    fn put_edges_bulk(&self, edges: &[(String, String, EdgeType)]) -> Result<(), StoreError> {
        let mut out_add: HashMap<&str, Vec<(String, EdgeType)>> = HashMap::new();
        let mut in_add: HashMap<&str, Vec<(String, EdgeType)>> = HashMap::new();
        for (src, dst, edge_type) in edges {
            self.edges.insert(edge_key(src, dst, edge_type), [])?;
            out_add
                .entry(src.as_str())
                .or_default()
                .push((dst.clone(), edge_type.clone()));
            in_add
                .entry(dst.as_str())
                .or_default()
                .push((src.clone(), edge_type.clone()));
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
        // Remove the doc record.
        self.docs.remove(doc_key(anchor))?;

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

    fn get_edges_by_type(
        &self,
        edge_type: &EdgeType,
    ) -> Result<Vec<(String, String)>, StoreError> {
        let edge_str = format!("{:?}", edge_type);
        let prefix = format!("edge{KEY_SEP}");
        let mut edges = Vec::new();
        for item in self.edges.iter() {
            let (key, _) = item?;
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

    fn edge_exists(&self, src: &str, dst: &str, edge_type: &EdgeType) -> Result<bool, StoreError> {
        Ok(self.edges.contains_key(edge_key(src, dst, edge_type))?)
    }

    fn get_all_anchors(&self) -> Result<HashSet<String>, StoreError> {
        let mut anchors = HashSet::new();
        for item in self.docs.iter() {
            let (key, _) = item?;
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
            let keys: Vec<_> = part.iter().filter_map(|i| i.ok().map(|(k, _)| k)).collect();
            for k in keys {
                part.remove(k)?;
            }
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), StoreError> {
        self.keyspace.persist(PersistMode::SyncAll)?;
        Ok(())
    }
}
