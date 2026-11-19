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
use aden_core::symbol::SymbolSuggestions;
use aden_core::{Document, EdgeType, SourceSpan};
use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
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

#[cfg(test)]
thread_local! {
    // Fail after staging every keyspace, before entering Fjall's commit.
    static FAIL_BULK_BEFORE_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SymbolLexiconRecord<'a> {
    segment_lower: &'a str,
    leaf_lower: &'a str,
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
        segment_lower: &forms.segment_lower,
        leaf_lower: &forms.leaf_lower,
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

/// Merge borrowed neighbors in first-seen order. Replaying a healthy row is
/// read-only; duplicate entries in older rows are repaired when touched.
fn merge_adjacency<'a>(
    stored: Option<&'a [u8]>,
    additions: Vec<(&'a str, EdgeType)>,
) -> Result<Option<Vec<u8>>, StoreError> {
    let mut current: Vec<(&str, EdgeType)> = match stored {
        Some(bytes) => deserialize(bytes)?,
        None => Vec::new(),
    };
    // Leaf symbols commonly have just one neighbor in this direction.
    if let [edge] = additions.as_slice()
        && current.len() <= 1
    {
        if current.first() == Some(edge) {
            return Ok(None);
        }
        current.push(*edge);
        return serialize(&current).map(Some);
    }

    let old_len = current.len();
    let mut seen = HashSet::new();
    current.retain(|edge| seen.insert(*edge));
    let repaired_duplicates = current.len() != old_len;
    current.extend(additions.into_iter().filter(|edge| seen.insert(*edge)));
    if repaired_duplicates || current.len() != old_len {
        return serialize(&current).map(Some);
    }
    Ok(None)
}

impl FjallStorage {
    /// Remove generated call and test edges for the supplied callers atomically.
    /// Only their outgoing lists and the affected targets' incoming lists are read.
    /// `Tests`, like `Calls`, points from the caller to the tested symbol.
    pub fn clear_generated_calls(&self, callers: &[&str]) -> Result<(), StoreError> {
        if callers.is_empty() {
            return Ok(());
        }
        let mut batch = self.db.batch();
        let mut seen = HashSet::new();
        let mut incoming_removals: HashMap<String, HashSet<(String, EdgeType)>> = HashMap::new();
        for &caller in callers {
            if !seen.insert(caller) {
                continue;
            }
            let key = outgoing_key(caller);
            let Some(bytes) = self.outgoing.get(&key)? else {
                continue;
            };
            let mut outgoing: Vec<(String, EdgeType)> = deserialize(&bytes)?;
            let previous_len = outgoing.len();
            outgoing.retain(|(target, edge_type)| {
                if matches!(edge_type, EdgeType::Calls | EdgeType::Tests) {
                    batch.remove(&self.edges, edge_key(caller, target, edge_type));
                    incoming_removals
                        .entry(target.clone())
                        .or_default()
                        .insert((caller.to_owned(), *edge_type));
                    false
                } else {
                    true
                }
            });
            if outgoing.len() != previous_len {
                if outgoing.is_empty() {
                    batch.remove(&self.outgoing, key);
                } else {
                    batch.insert(&self.outgoing, key, serialize(&outgoing)?);
                }
            }
        }
        if incoming_removals.is_empty() {
            return Ok(());
        }
        for (target, removals) in incoming_removals {
            let key = incoming_key(&target);
            if let Some(bytes) = self.incoming.get(&key)? {
                let mut incoming: Vec<(String, EdgeType)> = deserialize(&bytes)?;
                incoming.retain(|edge| !removals.contains(edge));
                if incoming.is_empty() {
                    batch.remove(&self.incoming, key);
                } else {
                    batch.insert(&self.incoming, key, serialize(&incoming)?);
                }
            }
        }
        batch.commit()?;
        Ok(())
    }

    /// Recognize the current engine's format marker without opening the store.
    ///
    /// Reads only the four-byte header of `path/version`, without creating files
    /// or acquiring the database lock. A recognized marker proves neither store
    /// integrity nor graph freshness. Missing, unreadable, or unknown markers
    /// return `false`; callers must use the normal engine open/recovery path to
    /// determine their meaning, never infer that the store should be deleted.
    pub fn has_current_format_marker(path: impl AsRef<Path>) -> bool {
        let Ok(mut marker) = std::fs::File::open(path.as_ref().join("version")) else {
            return false;
        };
        let mut header = [0; 4];
        if marker.read_exact(&mut header).is_err() {
            return false;
        }
        // Fjall 3.1.8's version.rs::parse_file_header recognizes FJL + version,
        // ignoring trailing bytes; db.rs::check_version accepts only V3. Keep
        // this positive-only shortcut aligned with that dependency contract.
        header[..3] == *b"FJL" && header[3] == u8::from(fjall::FormatVersion::V3)
    }

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

    /// Read source spans for selected anchors while preserving the bulk reader's
    /// projection precedence. A populated projection is authoritative, even when
    /// it is partial; document fallback is used only for an empty projection.
    pub fn get_source_spans_for<'a>(
        &self,
        anchors: impl IntoIterator<Item = &'a str>,
    ) -> Result<Vec<(String, SourceSpan)>, StoreError> {
        let projection_populated = !self.source_spans.is_empty()?;
        let mut spans = Vec::new();
        for anchor in anchors {
            if projection_populated {
                if let Some(value) = self.source_spans.get(anchor.as_bytes())? {
                    spans.push((anchor.to_string(), deserialize(&value)?));
                }
            } else if let Some(doc) = self.get_document(anchor)?
                && let Some(span) = doc.source_span
            {
                spans.push((anchor.to_string(), span));
            }
        }
        Ok(spans)
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
        #[cfg(test)]
        if FAIL_BULK_BEFORE_COMMIT.with(|fail| fail.replace(false)) {
            return Err(StoreError::Io("injected failure before bulk commit".into()));
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

        let mut outgoing: HashMap<&str, Vec<(&str, EdgeType)>> = HashMap::new();
        let mut incoming: HashMap<&str, Vec<(&str, EdgeType)>> = HashMap::new();
        let mut batch = self.db.batch();
        // On a cold store every canonical edge is new. On replay each index
        // is checked independently so a missing mirror or key is still repaired.
        let empty = self.edges.is_empty()?;
        for (src, dst, edge_type) in edges {
            let key = edge_key(src, dst, edge_type);
            if empty || !self.edges.get(&key)?.is_some_and(|value| value.is_empty()) {
                batch.insert(&self.edges, key, b"");
            }
            outgoing.entry(src).or_default().push((dst, *edge_type));
            incoming.entry(dst).or_default().push((src, *edge_type));
        }

        let mut stage_row = |space: &Keyspace, key: String, additions| -> Result<(), StoreError> {
            let stored = space.get(&key)?;
            if let Some(bytes) = merge_adjacency(stored.as_deref(), additions)? {
                batch.insert(space, key, bytes);
            }
            Ok(())
        };
        for (src, additions) in outgoing {
            stage_row(&self.outgoing, outgoing_key(src), additions)?;
        }
        for (dst, additions) in incoming {
            stage_row(&self.incoming, incoming_key(dst), additions)?;
        }
        if !batch.is_empty() {
            batch.commit()?;
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
            let key = guard.key()?;
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
            let key = guard.key()?;
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
            let key = guard.key()?;
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
            let key = guard.key()?;
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
        let probes =
            std::iter::once(natural.as_str()).chain((lower != natural).then_some(lower.as_str()));
        let mut exact = HashSet::new();
        for form in probes {
            let prefix = format!("{SYMBOL_EXACT_PREFIX}{KEY_SEP}{form}{KEY_SEP}");
            for guard in self.index.prefix(prefix.as_bytes()) {
                let key = guard.key()?;
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
        let mut max_distance = aden_core::symbol::typo_max_distance(symbol_len);
        let record_prefix = format!("{SYMBOL_RECORD_PREFIX}{KEY_SEP}");
        let mut substring = SymbolSuggestions::default();
        let mut typo = SymbolSuggestions::default();
        let mut records_scanned = 0usize;
        let mut distance_evaluations = 0usize;
        for guard in self.index.prefix(record_prefix.as_bytes()) {
            let (key, value) = guard.into_inner()?;
            records_scanned += 1;
            let key = String::from_utf8_lossy(&key);
            let Some(anchor) = key.strip_prefix(&record_prefix) else {
                continue;
            };
            let record: SymbolLexiconRecord<'_> = deserialize(&value)?;
            if !lower.is_empty() && record.segment_lower.contains(&lower) {
                // A substring wins over every typo, including those in later records.
                max_distance = None;
                substring.insert(0, anchor);
                continue;
            }
            let Some(max_distance) = max_distance else {
                continue;
            };
            let segment = (record.segment_len, record.segment_lower);
            let leaf = (record.leaf_len, record.leaf_lower);
            let distance = std::iter::once(segment)
                .chain((leaf != segment).then_some(leaf))
                .filter(|(len, _)| len.abs_diff(symbol_len) <= max_distance)
                .filter_map(|(_, candidate)| {
                    distance_evaluations += 1;
                    aden_core::symbol::edit_distance_within(&lower, candidate, max_distance)
                })
                .min();
            if let Some(distance) = distance {
                typo.insert(distance, anchor);
            }
        }

        let candidates = if substring.is_empty() {
            typo
        } else {
            substring
        };
        Ok(Some(SymbolCandidateLookup {
            anchors: candidates.into_anchors(),
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
            let keys: Vec<_> = part.iter().filter_map(|g| g.key().ok()).collect();
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
    fn clear_generated_calls_is_scoped_idempotent_and_persistent() {
        let path = test_store_path("clear-generated-calls");
        let storage = FjallStorage::new(path.to_str().unwrap()).unwrap();
        let edges = vec![
            ("test".into(), "target".into(), EdgeType::Calls),
            ("test".into(), "target".into(), EdgeType::Tests),
            ("test".into(), "target".into(), EdgeType::Uses),
            ("caller".into(), "target".into(), EdgeType::Calls),
            ("caller".into(), "caller".into(), EdgeType::Calls),
            ("other".into(), "target".into(), EdgeType::Calls),
            ("target".into(), "test".into(), EdgeType::Calls),
        ];
        storage.put_edges_bulk(&edges).unwrap();
        let callers = ["test", "caller", "test", "missing"];
        storage.clear_generated_calls(&[]).unwrap();
        storage.clear_generated_calls(&callers).unwrap();
        storage.clear_generated_calls(&callers).unwrap();
        storage.flush().unwrap();
        drop(storage);

        let storage = FjallStorage::open_existing(path.to_str().unwrap()).unwrap();
        for (src, dst, edge_type) in &edges {
            let removed = matches!(src.as_str(), "test" | "caller")
                && matches!(edge_type, EdgeType::Calls | EdgeType::Tests);
            assert_eq!(storage.edge_exists(src, dst, edge_type).unwrap(), !removed);
            assert_eq!(
                storage
                    .get_outgoing_edges(src)
                    .unwrap()
                    .contains(&(dst.clone(), *edge_type)),
                !removed
            );
            assert_eq!(
                storage
                    .get_incoming_edges(dst)
                    .unwrap()
                    .contains(&(src.clone(), *edge_type)),
                !removed
            );
        }
        // A fresh generated edge can be inserted after clearing stale ones.
        storage
            .put_edge("test", "new-target", EdgeType::Calls)
            .unwrap();
        assert!(
            storage
                .edge_exists("test", "new-target", &EdgeType::Calls)
                .unwrap()
        );
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn clear_generated_calls_read_failure_leaves_all_indexes_unchanged() {
        let path = test_store_path("clear-generated-calls-failure");
        let storage = FjallStorage::new(path.to_str().unwrap()).unwrap();
        storage
            .put_edge("caller", "target", EdgeType::Calls)
            .unwrap();
        storage
            .incoming
            .insert(incoming_key("target"), b"invalid adjacency")
            .unwrap();
        let outgoing_before = storage.outgoing.get(outgoing_key("caller")).unwrap();
        assert!(storage.clear_generated_calls(&["caller"]).is_err());
        assert!(
            storage
                .edge_exists("caller", "target", &EdgeType::Calls)
                .unwrap()
        );
        assert_eq!(
            storage.outgoing.get(outgoing_key("caller")).unwrap(),
            outgoing_before
        );
        assert_eq!(
            storage
                .incoming
                .get(incoming_key("target"))
                .unwrap()
                .unwrap()
                .as_ref(),
            b"invalid adjacency"
        );
        // Repair the bad mirror and retry the same bounded operation.
        storage
            .incoming
            .insert(
                incoming_key("target"),
                serialize(&vec![("caller".to_string(), EdgeType::Calls)]).unwrap(),
            )
            .unwrap();
        storage.clear_generated_calls(&["caller"]).unwrap();
        assert!(
            !storage
                .edge_exists("caller", "target", &EdgeType::Calls)
                .unwrap()
        );
        assert!(storage.get_incoming_edges("target").unwrap().is_empty());
        assert!(storage.get_outgoing_edges("caller").unwrap().is_empty());
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn repaired_document_batch_aborts_before_commit_and_recovers_coherently() {
        let path = test_store_path("atomic-repair");
        let storage = FjallStorage::new(path.to_str().unwrap()).unwrap();
        let anchor = "aden://module/example.rs#entry";
        let mut old = symbol_doc(anchor);
        old.source_span = Some(SourceSpan {
            file: "example.rs".into(),
            start_line: 1,
            end_line: 2,
            start_byte: 0,
            end_byte: 20,
        });
        storage
            .put_documents_bulk(&[(old.clone(), "old base".into())])
            .unwrap();
        storage.finalize_symbol_lexicon().unwrap();
        storage.flush().unwrap();
        let mut fresh = symbol_doc(anchor);
        fresh.attributes.insert("signature".into(), "new".into());
        let old_index: Vec<_> = storage
            .index
            .iter()
            .map(|v| v.into_inner().unwrap())
            .collect();

        FAIL_BULK_BEFORE_COMMIT.with(|fail| fail.set(true));
        let error = storage
            .put_documents_bulk(&[(fresh.clone(), "new base".into())])
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected failure before bulk commit")
        );
        assert_eq!(storage.get_document(anchor).unwrap(), Some(old.clone()));
        assert_eq!(
            storage.get_base_snapshot(anchor).unwrap().as_deref(),
            Some("old base")
        );
        assert_eq!(
            storage.get_source_spans().unwrap(),
            vec![(anchor.into(), old.source_span.unwrap())]
        );
        assert_eq!(
            storage
                .index
                .iter()
                .map(|v| v.into_inner().unwrap())
                .collect::<Vec<_>>(),
            old_index
        );
        storage.flush().unwrap();
        drop(storage);

        let storage = FjallStorage::open_existing(path.to_str().unwrap()).unwrap();
        assert_eq!(
            storage.get_base_snapshot(anchor).unwrap().as_deref(),
            Some("old base")
        );
        assert_ne!(storage.get_document(anchor).unwrap(), Some(fresh.clone()));
        // A successful repair must rebuild missing derived index records too.
        let (keys, record) = symbol_index_entries(anchor).unwrap();
        storage.index.remove(symbol_record_key(anchor)).unwrap();
        for key in &keys {
            storage.index.remove(key).unwrap();
        }
        storage
            .put_documents_bulk(&[(fresh.clone(), "new base".into())])
            .unwrap();
        assert_eq!(storage.get_document(anchor).unwrap(), Some(fresh.clone()));
        assert_eq!(
            storage.get_base_snapshot(anchor).unwrap().as_deref(),
            Some("new base")
        );
        assert!(
            storage.get_source_spans().unwrap().is_empty(),
            "stale source projection survived repair"
        );
        assert_eq!(
            storage
                .index
                .get(symbol_record_key(anchor))
                .unwrap()
                .unwrap()
                .as_ref(),
            record
        );
        for key in keys {
            assert!(storage.index.get(key).unwrap().is_some());
        }
        assert_eq!(
            storage
                .lookup_symbol_candidates("entry")
                .unwrap()
                .unwrap()
                .anchors,
            vec![anchor.to_string()]
        );
        storage.flush().unwrap();
        drop(storage);

        let storage = FjallStorage::open_existing(path.to_str().unwrap()).unwrap();
        assert_eq!(storage.get_document(anchor).unwrap(), Some(fresh));
        assert_eq!(
            storage.get_base_snapshot(anchor).unwrap().as_deref(),
            Some("new base")
        );
        assert!(storage.get_source_spans().unwrap().is_empty());
        assert_eq!(
            storage
                .lookup_symbol_candidates("entry")
                .unwrap()
                .unwrap()
                .anchors,
            vec![anchor.to_string()]
        );
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn bulk_edge_replay_skips_healthy_rows_and_repairs_each_index() {
        let path = test_store_path("edge-replay");
        let storage = FjallStorage::new(path.to_str().unwrap()).unwrap();
        let calls = EdgeType::Calls;
        let uses = EdgeType::Uses;
        let replay = vec![
            ("src".into(), "left".into(), calls),
            ("src".into(), "right".into(), uses),
            ("src".into(), "left".into(), calls),
        ];
        storage.put_edges_bulk(&replay).unwrap();
        let healthy = storage.db.seqno();
        storage.put_edges_bulk(&replay).unwrap();
        assert_eq!(storage.db.seqno(), healthy);

        // Leave unrelated entries intact while repairing all three indexes.
        storage
            .edges
            .remove(edge_key("src", "left", &calls))
            .unwrap();
        storage.incoming.remove(incoming_key("right")).unwrap();
        let duplicate = vec![("left", calls), ("left", calls)];
        storage
            .outgoing
            .insert(outgoing_key("src"), serialize(&duplicate).unwrap())
            .unwrap();
        storage.put_edges_bulk(&replay).unwrap();
        assert!(storage.edge_exists("src", "left", &calls).unwrap());
        assert_eq!(
            storage.get_incoming_edges("right").unwrap(),
            [("src".into(), uses)]
        );
        assert_eq!(
            storage.get_outgoing_edges("src").unwrap(),
            [("left".into(), calls), ("right".into(), uses)]
        );
        let repaired = storage.db.seqno();
        storage.put_edges_bulk(&replay).unwrap();
        assert_eq!(storage.db.seqno(), repaired);

        // An unreadable mirror must fail the whole batch, including new keys.
        storage
            .incoming
            .insert(incoming_key("bad"), &[0xff][..])
            .unwrap();
        let damaged = storage.db.seqno();
        assert!(
            storage
                .put_edges_bulk(&[("src".into(), "bad".into(), uses)])
                .is_err()
        );
        assert_eq!(storage.db.seqno(), damaged);
        assert!(!storage.edge_exists("src", "bad", &uses).unwrap());
        assert_eq!(storage.get_outgoing_edges("src").unwrap().len(), 2);
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
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
    fn symbol_lexicon_orders_and_caps_unicode_recovery() {
        let path = test_store_path("unicode-symbol-lexicon");
        let storage = FjallStorage::new(path.to_str().unwrap()).unwrap();
        let close = [
            "aden://module/p0.rs#cafes",
            "aden://module/p1.rs#cafes",
            "aden://module/p2.rs#cafes",
            "aden://module/p3.rs#cafes",
            "aden://module/p4.rs#cafes",
            "aden://module/p5.rs#cafes",
        ];
        let qualified = "aden://module/z.rs#N:CAFÊS";
        let far = [
            "aden://module/a.rs#caves",
            "aden://module/b.rs#caves",
            "aden://module/c.rs#caves",
        ];
        let misses = [
            "aden://module/d.rs#bound",
            "aden://module/e.rs#very_long_symbol",
        ];
        for anchor in [qualified]
            .into_iter()
            .chain(close)
            .chain(far)
            .chain(misses)
        {
            storage.put_document(&symbol_doc(anchor)).unwrap();
        }
        let old_record = ("cafes".to_owned(), "cafes".to_owned(), 5usize, 5usize);
        let old_bytes = serialize(&old_record).unwrap();
        let (_, new_bytes) = symbol_index_entries(close[0]).unwrap();
        assert_eq!(new_bytes, old_bytes);
        storage
            .index
            .insert(symbol_record_key(close[0]), old_bytes)
            .unwrap();
        storage.finalize_symbol_lexicon().unwrap();

        let typo = storage.lookup_symbol_candidates("cafés").unwrap().unwrap();
        let expected: Vec<_> = close.into_iter().chain([qualified, far[0]]).collect();
        assert_eq!(typo.anchors, expected);
        assert!(!typo.exact_index_hit);
        assert_eq!(typo.records_scanned, 12);
        // Both forms of the qualified anchor are scored. The long miss is
        // length-filtered and each plain record has a single distinct form.
        assert_eq!(typo.distance_evaluations, 12);

        let contains: Vec<_> = (0..10)
            .map(|index| format!("aden://module/0{index}.rs#cafés_menu"))
            .collect();
        for anchor in contains.iter().rev() {
            storage.put_document(&symbol_doc(anchor)).unwrap();
        }
        let substring = storage.lookup_symbol_candidates("cafés").unwrap().unwrap();
        assert_eq!(substring.anchors, contains[..8]);
        assert!(!substring.exact_index_hit);
        assert_eq!(substring.records_scanned, 22);
        assert_eq!(substring.distance_evaluations, 0);

        // Even a full set of substring matches must not hide a corrupt later record.
        storage
            .index
            .insert(symbol_record_key(qualified), &[0xff][..])
            .unwrap();
        assert!(matches!(
            storage.lookup_symbol_candidates("cafés"),
            Err(StoreError::Serialization(_))
        ));

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
        assert_eq!(
            storage.get_source_spans_for([anchor, "absent"]).unwrap(),
            spans
        );
        assert_eq!(storage.lookup_symbol_candidates("entry").unwrap(), None);
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn keyed_spans_keep_partial_projection_authoritative() {
        let path = test_store_path("partial-keyed-source-spans");
        let storage = FjallStorage::new(path.to_str().unwrap()).unwrap();
        let present = Document {
            anchor: "present".to_string(),
            source_span: Some(SourceSpan {
                file: "/repo/present.rs".to_string(),
                start_line: 1,
                end_line: 1,
                start_byte: 0,
                end_byte: 1,
            }),
            ..Document::default()
        };
        let missing_projection = Document {
            anchor: "missing-projection".to_string(),
            source_span: Some(SourceSpan {
                file: "/repo/missing.rs".to_string(),
                start_line: 2,
                end_line: 2,
                start_byte: 2,
                end_byte: 3,
            }),
            ..Document::default()
        };
        storage.put_document(&present).unwrap();
        storage.put_document(&missing_projection).unwrap();
        storage
            .source_spans
            .remove(missing_projection.anchor.as_bytes())
            .unwrap();

        assert_eq!(
            storage
                .get_source_spans_for(
                    [present.anchor.as_str(), missing_projection.anchor.as_str(),]
                )
                .unwrap(),
            vec![(present.anchor, present.source_span.unwrap())]
        );
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
