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
    GraphStorage, KEY_SEP, StoreError, SymbolCandidateLookup, TreeName, base_key, deserialize,
    deserialize_document, doc_key, edge_key, incoming_key, meta_key, outgoing_key, serialize,
    serialize_document,
};
use aden_core::{Document, EdgeType, SourceSpan};
use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};
use std::collections::{HashMap, HashSet};
use std::thread::sleep;
use std::time::{Duration, Instant};

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
    source_spans: Keyspace,
    edges: Keyspace,
    outgoing: Keyspace,
    incoming: Keyspace,
    index: Keyspace,
    meta: Keyspace,
    bases: Keyspace,
}

/// Default wait for a contended fjall open (readers holding the DB lock).
pub const FJALL_OPEN_RETRY_TIMEOUT: Duration = Duration::from_secs(600);

fn store_error_is_locked(err: &StoreError) -> bool {
    matches!(err, StoreError::Io(msg) if msg.contains("Locked"))
}

// v2 indexes language-native backslash-qualified symbols (notably PHP).
pub const SYMBOL_LEXICON_VERSION: &str = "2";
const SYMBOL_LEXICON_META_KEY: &str = "symbol_lexicon_version";
const SYMBOL_EXACT_PREFIX: &str = "se";
const SYMBOL_RECORD_PREFIX: &str = "sr";

#[derive(serde::Serialize, serde::Deserialize)]
struct SymbolLexiconRecord {
    segment_lower: String,
    leaf_lower: String,
    segment_len: usize,
    leaf_len: usize,
}

fn symbol_exact_key(form: &str, anchor: &str) -> String {
    format!("{SYMBOL_EXACT_PREFIX}{KEY_SEP}{form}{KEY_SEP}{anchor}")
}

fn symbol_record_key(anchor: &str) -> String {
    format!("{SYMBOL_RECORD_PREFIX}{KEY_SEP}{anchor}")
}

fn symbol_index_entries(anchor: &str) -> Result<(Vec<String>, Vec<u8>), StoreError> {
    let forms = aden_core::symbol::natural_anchor_forms(anchor);
    let mut keys = vec![
        symbol_exact_key(&forms.segment, anchor),
        symbol_exact_key(&forms.segment_lower, anchor),
        symbol_exact_key(&forms.leaf, anchor),
        symbol_exact_key(&forms.leaf_lower, anchor),
    ];
    keys.extend(
        forms
            .qualified_prefixes_lower
            .iter()
            .map(|prefix| symbol_exact_key(prefix, anchor)),
    );
    keys.sort();
    keys.dedup();
    let record = SymbolLexiconRecord {
        segment_len: forms.segment_lower.chars().count(),
        leaf_len: forms.leaf_lower.chars().count(),
        segment_lower: forms.segment_lower,
        leaf_lower: forms.leaf_lower,
    };
    Ok((keys, serialize(&record)?))
}

/// Symbols historically persisted spans in attributes and rehydrated the
/// struct field on reads. Keep the projection compatible with both forms.
fn effective_source_span(doc: &Document) -> Option<SourceSpan> {
    doc.source_span
        .clone()
        .or_else(|| SourceSpan::from_attributes(&doc.attributes))
}

impl FjallStorage {
    /// Open (or create) a Fjall store at the given path.
    pub fn new(path: &str) -> Result<Self, StoreError> {
        Self::open_once(path)
    }

    /// Open with retry when fjall reports `Locked` — typical when MCP read
    /// tools still have the live store open. Waits up to `timeout`, emitting a
    /// `NOTE:` every 5s (ADR-011 Phase 2).
    pub fn new_with_retry(path: &str, timeout: Duration) -> Result<Self, StoreError> {
        let deadline = Instant::now() + timeout;
        let started = Instant::now();
        let mut last_note = started;
        const NOTE_EVERY: Duration = Duration::from_secs(5);
        const POLL: Duration = Duration::from_millis(100);

        loop {
            match Self::open_once(path) {
                Ok(storage) => return Ok(storage),
                Err(e) if store_error_is_locked(&e) => {
                    let now = Instant::now();
                    if now.duration_since(last_note) >= NOTE_EVERY {
                        eprintln!(
                            "NOTE: waiting for fjall store at {path} (readers may be active; \
                             waited {}s)…",
                            now.duration_since(started).as_secs()
                        );
                        last_note = now;
                    }
                    if now >= deadline {
                        return Err(StoreError::Io(format!(
                            "store locked at {path} — readers or another writer still have the \
                             database open (waited {timeout:?}). Retry when agents are idle, or \
                             run from a shell after `aden gen` publishes graph.snapshot (ADR-011)."
                        )));
                    }
                    sleep(POLL);
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn open_once(path: &str) -> Result<Self, StoreError> {
        let db = Database::builder(path).open()?;
        let open = |name: &str| -> Result<Keyspace, StoreError> {
            db.keyspace(name, KeyspaceCreateOptions::default)
                .map_err(StoreError::from)
        };
        Ok(Self {
            docs: open(TreeName::Docs.name())?,
            source_spans: open(TreeName::SourceSpans.name())?,
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

    fn get_source_spans(&self) -> Result<Vec<(String, SourceSpan)>, StoreError> {
        let mut spans = Vec::with_capacity(self.source_spans.approximate_len());
        for guard in self.source_spans.iter() {
            let (anchor, value) = guard.into_inner()?;
            spans.push((
                String::from_utf8(anchor.to_vec()).map_err(|e| {
                    StoreError::Serialization(format!("source-span anchor is not UTF-8: {e}"))
                })?,
                deserialize(&value)?,
            ));
        }
        if !spans.is_empty() || self.docs.is_empty()? {
            return Ok(spans);
        }

        // Compatibility fallback for pinned/shared stores that cannot be
        // auto-rebuilt during the layout migration. It is intentionally
        // read-only; normal per-project stores rebuild once and stay on the
        // compact partition thereafter.
        for guard in self.docs.iter() {
            let (key, value) = guard.into_inner()?;
            let key = String::from_utf8_lossy(&key);
            let Some(anchor) = key.strip_prefix("doc:") else {
                continue;
            };
            if let Some(span) = deserialize_document(&value)?.source_span {
                spans.push((anchor.to_string(), span));
            }
        }
        Ok(spans)
    }

    fn put_document(&self, doc: &Document) -> Result<(), StoreError> {
        let (symbol_keys, symbol_record) = symbol_index_entries(&doc.anchor)?;
        let mut batch = self.db.batch();
        batch.insert(&self.docs, doc_key(&doc.anchor), serialize_document(doc)?);
        if let Some(span) = effective_source_span(doc) {
            batch.insert(&self.source_spans, &doc.anchor, serialize(&span)?);
        } else {
            batch.remove(&self.source_spans, &doc.anchor);
        }
        batch.insert(&self.index, symbol_record_key(&doc.anchor), symbol_record);
        for key in symbol_keys {
            batch.insert(&self.index, key, b"");
        }
        batch.commit()?;
        Ok(())
    }

    fn delete_document(&self, anchor: &str) -> Result<(), StoreError> {
        let (symbol_keys, _) = symbol_index_entries(anchor)?;
        let mut batch = self.db.batch();
        batch.remove(&self.docs, doc_key(anchor));
        batch.remove(&self.source_spans, anchor);
        batch.remove(&self.index, symbol_record_key(anchor));
        for key in symbol_keys {
            batch.remove(&self.index, key);
        }
        // A document's base snapshot is meaningless without the document.
        batch.remove(&self.bases, base_key(anchor));
        batch.commit()?;
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
        let mut batch = self.db.batch();
        for (doc, snapshot) in docs {
            let (symbol_keys, symbol_record) = symbol_index_entries(&doc.anchor)?;
            batch.insert(&self.docs, doc_key(&doc.anchor), serialize_document(doc)?);
            if let Some(span) = effective_source_span(doc) {
                batch.insert(&self.source_spans, &doc.anchor, serialize(&span)?);
            } else {
                batch.remove(&self.source_spans, &doc.anchor);
            }
            batch.insert(&self.bases, base_key(&doc.anchor), snapshot.as_bytes());
            batch.insert(&self.index, symbol_record_key(&doc.anchor), symbol_record);
            for key in symbol_keys {
                batch.insert(&self.index, key, b"");
            }
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
        if edges.is_empty() {
            return Ok(());
        }

        // Read and merge each affected adjacency list once, then commit the
        // canonical edge records and both adjacency mirrors in one journal
        // batch. The former implementation grouped adjacency rewrites but
        // still issued one Fjall write per edge, leaving a large batch only
        // partially visible after a crash and amplifying journal traffic.
        let mut out_add: HashMap<&str, Vec<(String, EdgeType)>> = HashMap::new();
        let mut in_add: HashMap<&str, Vec<(String, EdgeType)>> = HashMap::new();
        for (src, dst, edge_type) in edges {
            out_add
                .entry(src.as_str())
                .or_default()
                .push((dst.clone(), *edge_type));
            in_add
                .entry(dst.as_str())
                .or_default()
                .push((src.clone(), *edge_type));
        }

        let mut outgoing_updates = Vec::with_capacity(out_add.len());
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
            outgoing_updates.push((key, serialize(&cur)?));
        }

        let mut incoming_updates = Vec::with_capacity(in_add.len());
        for (dst, mut adds) in in_add {
            let key = incoming_key(dst);
            let mut cur: Vec<(String, EdgeType)> = match self.incoming.get(&key)? {
                Some(b) => deserialize(&b)?,
                None => vec![],
            };
            cur.append(&mut adds);
            let mut seen = HashSet::new();
            cur.retain(|e| seen.insert(e.clone()));
            incoming_updates.push((key, serialize(&cur)?));
        }

        let mut batch = self.db.batch();
        for (src, dst, edge_type) in edges {
            batch.insert(&self.edges, edge_key(src, dst, edge_type), &[] as &[u8]);
        }
        for (key, value) in outgoing_updates {
            batch.insert(&self.outgoing, key, value);
        }
        for (key, value) in incoming_updates {
            batch.insert(&self.incoming, key, value);
        }
        batch.commit()?;
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
        // Load the affected lists before committing. Grouping removals by
        // neighbour is essential: a pair can have several edge types, and
        // independently staging each rewrite would make the last batch entry
        // resurrect entries removed by an earlier one.
        let out: Vec<(String, EdgeType)> = match self.outgoing.get(outgoing_key(anchor))? {
            Some(b) => deserialize(&b)?,
            None => vec![],
        };
        let inc: Vec<(String, EdgeType)> = match self.incoming.get(incoming_key(anchor))? {
            Some(b) => deserialize(&b)?,
            None => vec![],
        };

        let mut incoming_removals: HashMap<&str, HashSet<EdgeType>> = HashMap::new();
        for (dst, et) in &out {
            incoming_removals
                .entry(dst.as_str())
                .or_default()
                .insert(*et);
        }
        let mut outgoing_removals: HashMap<&str, HashSet<EdgeType>> = HashMap::new();
        for (src, et) in &inc {
            outgoing_removals
                .entry(src.as_str())
                .or_default()
                .insert(*et);
        }

        let mut incoming_updates = Vec::with_capacity(incoming_removals.len());
        for (dst, types) in incoming_removals {
            let key = incoming_key(dst);
            if let Some(b) = self.incoming.get(&key)? {
                let mut neighbors: Vec<(String, EdgeType)> = deserialize(&b)?;
                neighbors.retain(|(node, et)| !(node == anchor && types.contains(et)));
                incoming_updates.push((key, serialize(&neighbors)?));
            }
        }
        let mut outgoing_updates = Vec::with_capacity(outgoing_removals.len());
        for (src, types) in outgoing_removals {
            let key = outgoing_key(src);
            if let Some(b) = self.outgoing.get(&key)? {
                let mut neighbors: Vec<(String, EdgeType)> = deserialize(&b)?;
                neighbors.retain(|(node, et)| !(node == anchor && types.contains(et)));
                outgoing_updates.push((key, serialize(&neighbors)?));
            }
        }

        let (symbol_keys, _) = symbol_index_entries(anchor)?;
        let mut batch = self.db.batch();
        // Remove the doc record and its merge base — a stale snapshot would
        // make a future re-gen of the same anchor merge against a ghost.
        batch.remove(&self.docs, doc_key(anchor));
        batch.remove(&self.source_spans, anchor);
        batch.remove(&self.bases, base_key(anchor));
        batch.remove(&self.index, symbol_record_key(anchor));
        for key in symbol_keys {
            batch.remove(&self.index, key);
        }
        for (dst, et) in &out {
            batch.remove(&self.edges, edge_key(anchor, dst, et));
        }
        for (src, et) in &inc {
            batch.remove(&self.edges, edge_key(src, anchor, et));
        }
        for (key, value) in incoming_updates {
            batch.insert(&self.incoming, key, value);
        }
        for (key, value) in outgoing_updates {
            batch.insert(&self.outgoing, key, value);
        }
        // Finally drop the anchor's own adjacency lists.
        batch.remove(&self.outgoing, outgoing_key(anchor));
        batch.remove(&self.incoming, incoming_key(anchor));
        batch.commit()?;
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
                if parts.len() == 3
                    && let Some(&et) = type_map.get(parts[2])
                {
                    edges.push((parts[0].to_string(), parts[1].to_string(), et));
                }
            }
        }
        Ok(edges)
    }

    fn incoming_counts_by_target(
        &self,
        edge_types: &[EdgeType],
    ) -> Result<HashMap<String, usize>, StoreError> {
        let requested: HashSet<String> = edge_types
            .iter()
            .map(|edge_type| format!("{edge_type:?}"))
            .collect();
        let prefix = format!("edge{KEY_SEP}");
        let mut counts = HashMap::new();
        for guard in self.edges.iter() {
            let (key, _) = guard.into_inner()?;
            let key = String::from_utf8_lossy(&key);
            let Some(suffix) = key.strip_prefix(&prefix) else {
                continue;
            };
            let mut parts = suffix.split(KEY_SEP);
            let (Some(_source), Some(target), Some(edge_type), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            if requested.is_empty() || requested.contains(edge_type) {
                *counts.entry(target.to_string()).or_insert(0) += 1;
            }
        }
        Ok(counts)
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

    fn lookup_symbol_candidates(
        &self,
        symbol: &str,
    ) -> Result<Option<SymbolCandidateLookup>, StoreError> {
        let version = self.meta.get(meta_key(SYMBOL_LEXICON_META_KEY))?;
        if version.as_deref() != Some(SYMBOL_LEXICON_VERSION.as_bytes()) {
            return Ok(None);
        }

        let natural = aden_core::symbol::natural_symbol_form(symbol);
        let lower = natural.to_lowercase();
        let probes = [natural.as_str(), lower.as_str()];
        let mut exact = HashSet::new();
        for form in probes {
            let prefix = format!("{SYMBOL_EXACT_PREFIX}{KEY_SEP}{form}{KEY_SEP}");
            for guard in self.index.prefix(prefix.as_bytes()) {
                let (key, _) = guard.into_inner()?;
                let key = String::from_utf8_lossy(&key);
                if let Some(anchor) = key.strip_prefix(&prefix) {
                    exact.insert(anchor.to_string());
                }
            }
        }
        if !exact.is_empty() {
            let mut anchors: Vec<String> = exact.into_iter().collect();
            anchors.sort();
            return Ok(Some(SymbolCandidateLookup {
                anchors,
                records_scanned: 0,
                distance_evaluations: 0,
                exact_index_hit: true,
            }));
        }

        let symbol_len = lower.chars().count();
        let max_distance = aden_core::symbol::typo_max_distance(symbol_len);
        let record_prefix = format!("{SYMBOL_RECORD_PREFIX}{KEY_SEP}");
        let mut substring = Vec::new();
        let mut typo = Vec::new();
        let mut records_scanned = 0usize;
        let mut distance_evaluations = 0usize;
        for guard in self.index.prefix(record_prefix.as_bytes()) {
            let (key, value) = guard.into_inner()?;
            records_scanned += 1;
            let key = String::from_utf8_lossy(&key);
            let Some(anchor) = key.strip_prefix(&record_prefix) else {
                continue;
            };
            let record: SymbolLexiconRecord = deserialize(&value)?;
            if !lower.is_empty() && record.segment_lower.contains(&lower) {
                substring.push(anchor.to_string());
                continue;
            }
            let Some(max_distance) = max_distance else {
                continue;
            };
            let distance = [
                (record.segment_lower.as_str(), record.segment_len),
                (record.leaf_lower.as_str(), record.leaf_len),
            ]
            .into_iter()
            .filter(|(_, len)| len.abs_diff(symbol_len) <= max_distance)
            .map(|(candidate, _)| {
                distance_evaluations += 1;
                aden_core::symbol::edit_distance(&lower, candidate)
            })
            .min();
            if let Some(distance) = distance.filter(|distance| *distance <= max_distance) {
                typo.push((distance, anchor.to_string()));
            }
        }

        let anchors = if !substring.is_empty() {
            substring.sort();
            substring.dedup();
            substring.into_iter().take(8).collect()
        } else {
            typo.sort_by(
                |(left_distance, left_anchor), (right_distance, right_anchor)| {
                    left_distance
                        .cmp(right_distance)
                        .then_with(|| left_anchor.cmp(right_anchor))
                },
            );
            typo.dedup_by(|left, right| left.1 == right.1);
            typo.into_iter().take(8).map(|(_, anchor)| anchor).collect()
        };
        Ok(Some(SymbolCandidateLookup {
            anchors,
            records_scanned,
            distance_evaluations,
            exact_index_hit: false,
        }))
    }

    fn invalidate_symbol_lexicon(&self) -> Result<(), StoreError> {
        self.meta.remove(meta_key(SYMBOL_LEXICON_META_KEY))?;
        Ok(())
    }

    fn finalize_symbol_lexicon(&self) -> Result<(), StoreError> {
        self.meta.insert(
            meta_key(SYMBOL_LEXICON_META_KEY),
            SYMBOL_LEXICON_VERSION.as_bytes(),
        )?;
        Ok(())
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
                if let Some(want) = edge_type
                    && &et != want
                {
                    continue;
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
            &self.source_spans,
            &self.edges,
            &self.outgoing,
            &self.incoming,
            &self.index,
            &self.meta,
            &self.bases,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_store_path(name: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "aden-fjall-store-test-{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
            name
        ))
    }

    fn symbol_doc(anchor: &str) -> Document {
        Document {
            anchor: anchor.to_string(),
            node_type: aden_core::NodeType::Function,
            attributes: HashMap::new(),
            blocks: vec![],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        }
    }

    #[test]
    fn symbol_lexicon_uses_exact_keys_and_bounds_typo_candidates() {
        let path = test_store_path("symbol-lexicon");
        let storage = FjallStorage::new(path.to_str().unwrap()).unwrap();
        for anchor in [
            "aden://module/a.rs#parse",
            "aden://module/b.rs#parse",
            "aden://module/c.rs#parse_document",
        ] {
            storage.put_document(&symbol_doc(anchor)).unwrap();
        }
        for index in 0..1000 {
            storage
                .put_document(&symbol_doc(&format!(
                    "aden://module/noise.rs#very_long_generated_symbol_{index}"
                )))
                .unwrap();
        }
        assert_eq!(storage.lookup_symbol_candidates("parse").unwrap(), None);
        storage.finalize_symbol_lexicon().unwrap();
        assert!(
            storage.index.approximate_len() <= 3 * 1003,
            "symbol lexicon exceeded three keys per simple anchor"
        );

        let exact = storage.lookup_symbol_candidates("parse").unwrap().unwrap();
        assert!(exact.exact_index_hit);
        assert_eq!(exact.records_scanned, 0);
        assert_eq!(exact.distance_evaluations, 0);
        assert_eq!(
            exact.anchors,
            [
                "aden://module/a.rs#parse".to_string(),
                "aden://module/b.rs#parse".to_string()
            ]
        );

        let substring = storage
            .lookup_symbol_candidates("document")
            .unwrap()
            .unwrap();
        assert_eq!(substring.anchors, ["aden://module/c.rs#parse_document"]);
        assert!(!substring.exact_index_hit);

        let typo = storage.lookup_symbol_candidates("prase").unwrap().unwrap();
        assert!(typo.records_scanned >= 1003);
        assert!(typo.distance_evaluations <= 6, "{typo:?}");
        assert_eq!(typo.anchors.len(), 2);
        assert!(typo.anchors.iter().all(|anchor| anchor.ends_with("#parse")));

        storage.invalidate_symbol_lexicon().unwrap();
        assert_eq!(storage.lookup_symbol_candidates("parse").unwrap(), None);
        storage.finalize_symbol_lexicon().unwrap();
        storage.delete_node("aden://module/a.rs#parse").unwrap();
        let exact = storage.lookup_symbol_candidates("parse").unwrap().unwrap();
        assert_eq!(exact.anchors, ["aden://module/b.rs#parse"]);
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn symbol_lexicon_indexes_php_backslash_shorthand() {
        let path = test_store_path("php-symbol-lexicon");
        let storage = FjallStorage::new(path.to_str().unwrap()).unwrap();
        let anchor = r"aden://module/shop/Service.php#App\Billing\InvoiceService\send";
        storage.put_document(&symbol_doc(anchor)).unwrap();
        storage.finalize_symbol_lexicon().unwrap();

        for shorthand in ["send", r"App\Billing", r"App\Billing\InvoiceService"] {
            let result = storage
                .lookup_symbol_candidates(shorthand)
                .unwrap()
                .unwrap_or_else(|| panic!("missing PHP shorthand {shorthand}"));
            assert!(result.exact_index_hit, "{shorthand}: {result:?}");
            assert_eq!(result.anchors, [anchor.to_string()]);
        }

        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn legacy_docs_without_projection_still_return_source_spans() {
        let path = test_store_path("legacy-source-spans");
        let storage = FjallStorage::new(path.to_str().unwrap()).unwrap();
        let anchor = "aden://module/example.rs#entry";
        let doc = Document {
            anchor: anchor.to_string(),
            node_type: aden_core::NodeType::Function,
            attributes: HashMap::new(),
            blocks: vec![],
            source_span: Some(SourceSpan {
                file: "/repo/example.rs".to_string(),
                start_line: 3,
                end_line: 5,
                start_byte: 10,
                end_byte: 30,
            }),
            metadata: None,
            confidence: 1.0,
        };
        // Bypass put_document to model a store created before source_spans.
        storage
            .docs
            .insert(doc_key(anchor), serialize_document(&doc).unwrap())
            .unwrap();

        let spans = storage.get_source_spans().unwrap();
        assert_eq!(spans, vec![(anchor.to_string(), doc.source_span.unwrap())]);
        assert_eq!(storage.lookup_symbol_candidates("entry").unwrap(), None);
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn clear_removes_base_snapshots() {
        let path = test_store_path("clear-bases");
        let storage = FjallStorage::new(path.to_str().unwrap()).unwrap();
        storage
            .put_base_snapshot("aden://module/example.rs#entry", "old base")
            .unwrap();

        storage.clear().unwrap();

        assert_eq!(
            storage
                .get_base_snapshot("aden://module/example.rs#entry")
                .unwrap(),
            None
        );
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }
}
