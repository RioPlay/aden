// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! SQLite WAL-backed implementation of [`GraphStorage`].
//!
//! Uses SQLite 3 in WAL mode with a consolidated 7-table normalized schema
//! (`files`, `nodes`, `node_bodies`, `bases`, `edges`, `symbol_exact`, `meta`),
//! `lz4_flex`-compressed Postcard payloads, and lazy canonical merge bases so
//! concurrent CLI/MCP readers never suffer exclusive-lock contention while
//! `gen` writes in single atomic transactions.

use crate::{
    FileRecord, GraphStorage, StoreError, SymbolCandidateLookup, deserialize_document,
    serialize_document,
};
use aden_core::symbol::SymbolSuggestions;
use aden_core::{Document, EdgeType, NodeType, SourceSpan};
use rusqlite::{
    Connection, ErrorCode, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Magic header for the `<store>/version` file (`ADNS` + format version byte).
const STORE_FORMAT_MAGIC: &[u8; 4] = b"ADNS";
const STORE_FORMAT_VERSION: u8 = 1;
const SQLITE_USER_VERSION: i32 = 1;
const STORE_DB_FILENAME: &str = "graph.sqlite";

const BLOB_RAW_TAG: u8 = 0;
const BLOB_LZ4_TAG: u8 = 1;
const BLOB_COMPRESS_THRESHOLD: usize = 96;
const BLOB_MAX_UNCOMPRESSED_BYTES: usize = 128 * 1024 * 1024;

/// Default wait for a contended SQLite write lock.
pub const SQLITE_OPEN_RETRY_TIMEOUT: Duration = Duration::from_secs(600);

// v2 indexes language-native backslash-qualified symbols (notably PHP).
pub const SYMBOL_LEXICON_VERSION: &str = "2";
const SYMBOL_LEXICON_META_KEY: &str = "symbol_lexicon_version";

#[cfg(test)]
thread_local! {
    // Fail after staging all batch writes inside the transaction, before commit.
    static FAIL_BULK_BEFORE_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn map_sqlite_err(err: rusqlite::Error) -> StoreError {
    StoreError::Io(err.to_string())
}

fn sqlite_error_is_locked(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

fn store_error_is_locked(err: &StoreError) -> bool {
    matches!(
        err,
        StoreError::Io(msg)
            if msg.contains("DatabaseBusy")
                || msg.contains("DatabaseLocked")
                || msg.contains("database is locked")
                || msg.contains("database table is locked")
    )
}

/// Exhaustive edge-type encoding — adding an `EdgeType` variant breaks compilation here.
fn edge_to_u8(edge_type: EdgeType) -> u8 {
    match edge_type {
        EdgeType::Uses => 0,
        EdgeType::UsedBy => 1,
        EdgeType::Implements => 2,
        EdgeType::Extends => 3,
        EdgeType::Imports => 4,
        EdgeType::Tests => 5,
        EdgeType::Documents => 6,
        EdgeType::Contains => 7,
        EdgeType::Constrains => 8,
        EdgeType::Justifies => 9,
        EdgeType::Invokes => 10,
        EdgeType::Requires => 11,
        EdgeType::Mutates => 12,
        EdgeType::Calls => 13,
        EdgeType::Supersedes => 14,
        EdgeType::Amends => 15,
        EdgeType::Verifies => 16,
        EdgeType::Demonstrates => 17,
        EdgeType::Mentions => 18,
        EdgeType::DefinesTerm => 19,
        EdgeType::IsA => 20,
        EdgeType::PartOf => 21,
        EdgeType::RelatesTo => 22,
        EdgeType::SimilarTo => 23,
        EdgeType::Causes => 24,
        EdgeType::Implies => 25,
        EdgeType::SynonymOf => 26,
        EdgeType::AntonymOf => 27,
        EdgeType::AssociatedWith => 28,
        EdgeType::PrerequisiteFor => 29,
        EdgeType::Explains => 30,
        EdgeType::IsEquivalentTo => 31,
    }
}

fn u8_to_edge(code: u8) -> Result<EdgeType, StoreError> {
    match code {
        0 => Ok(EdgeType::Uses),
        1 => Ok(EdgeType::UsedBy),
        2 => Ok(EdgeType::Implements),
        3 => Ok(EdgeType::Extends),
        4 => Ok(EdgeType::Imports),
        5 => Ok(EdgeType::Tests),
        6 => Ok(EdgeType::Documents),
        7 => Ok(EdgeType::Contains),
        8 => Ok(EdgeType::Constrains),
        9 => Ok(EdgeType::Justifies),
        10 => Ok(EdgeType::Invokes),
        11 => Ok(EdgeType::Requires),
        12 => Ok(EdgeType::Mutates),
        13 => Ok(EdgeType::Calls),
        14 => Ok(EdgeType::Supersedes),
        15 => Ok(EdgeType::Amends),
        16 => Ok(EdgeType::Verifies),
        17 => Ok(EdgeType::Demonstrates),
        18 => Ok(EdgeType::Mentions),
        19 => Ok(EdgeType::DefinesTerm),
        20 => Ok(EdgeType::IsA),
        21 => Ok(EdgeType::PartOf),
        22 => Ok(EdgeType::RelatesTo),
        23 => Ok(EdgeType::SimilarTo),
        24 => Ok(EdgeType::Causes),
        25 => Ok(EdgeType::Implies),
        26 => Ok(EdgeType::SynonymOf),
        27 => Ok(EdgeType::AntonymOf),
        28 => Ok(EdgeType::AssociatedWith),
        29 => Ok(EdgeType::PrerequisiteFor),
        30 => Ok(EdgeType::Explains),
        31 => Ok(EdgeType::IsEquivalentTo),
        _ => Err(StoreError::Serialization(format!(
            "unknown edge code {code}"
        ))),
    }
}

/// Exhaustive node-type encoding — adding a `NodeType` variant breaks compilation here.
fn node_type_to_u8(node_type: &NodeType) -> u8 {
    match node_type {
        NodeType::Module => 0,
        NodeType::Function => 1,
        NodeType::Type => 2,
        NodeType::Script => 3,
        NodeType::Adr => 4,
        NodeType::Runbook => 5,
        NodeType::Plan => 6,
        NodeType::Context => 7,
        NodeType::Manifest => 8,
        NodeType::Spec => 9,
        NodeType::Note => 10,
        NodeType::Term => 11,
    }
}

#[allow(dead_code)]
fn u8_to_node_type(code: u8) -> Result<NodeType, StoreError> {
    match code {
        0 => Ok(NodeType::Module),
        1 => Ok(NodeType::Function),
        2 => Ok(NodeType::Type),
        3 => Ok(NodeType::Script),
        4 => Ok(NodeType::Adr),
        5 => Ok(NodeType::Runbook),
        6 => Ok(NodeType::Plan),
        7 => Ok(NodeType::Context),
        8 => Ok(NodeType::Manifest),
        9 => Ok(NodeType::Spec),
        10 => Ok(NodeType::Note),
        11 => Ok(NodeType::Term),
        _ => Err(StoreError::Serialization(format!(
            "unknown node_type code {code}"
        ))),
    }
}

fn emit_canonical_base(doc: &Document) -> String {
    aden_emit::emit_contract_document(&aden_core::contract::ContractDocument::from_document(doc))
}

fn compress_blob(raw: &[u8]) -> Vec<u8> {
    if raw.len() >= BLOB_COMPRESS_THRESHOLD && raw.len() <= BLOB_MAX_UNCOMPRESSED_BYTES {
        let compressed = lz4_flex::block::compress(raw);
        if 1 + 4 + compressed.len() < raw.len() {
            let mut out = Vec::with_capacity(5 + compressed.len());
            out.push(BLOB_LZ4_TAG);
            out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
            out.extend_from_slice(&compressed);
            return out;
        }
    }
    let mut out = Vec::with_capacity(1 + raw.len());
    out.push(BLOB_RAW_TAG);
    out.extend_from_slice(raw);
    out
}

fn decompress_blob(blob: &[u8]) -> Result<Vec<u8>, StoreError> {
    let Some((&tag, rest)) = blob.split_first() else {
        return Err(StoreError::Serialization("empty compressed blob".into()));
    };
    match tag {
        BLOB_RAW_TAG => Ok(rest.to_vec()),
        BLOB_LZ4_TAG => {
            if rest.len() < 4 {
                return Err(StoreError::Serialization(
                    "truncated lz4 length header".into(),
                ));
            }
            let (len_bytes, payload) = rest.split_at(4);
            let orig_len = u32::from_le_bytes(len_bytes.try_into().unwrap()) as usize;
            if orig_len > BLOB_MAX_UNCOMPRESSED_BYTES {
                return Err(StoreError::Serialization(format!(
                    "compressed blob exceeds {BLOB_MAX_UNCOMPRESSED_BYTES} byte limit ({orig_len})"
                )));
            }
            lz4_flex::block::decompress(payload, orig_len)
                .map_err(|e| StoreError::Serialization(format!("lz4 decompress: {e}")))
        }
        _ => Err(StoreError::Serialization(format!(
            "unknown blob encoding tag {tag}"
        ))),
    }
}

/// Symbols historically persisted spans in attributes and rehydrated the
/// struct field on reads. Keep the projection compatible with both forms.
fn effective_source_span(doc: &Document) -> Option<SourceSpan> {
    doc.source_span
        .clone()
        .or_else(|| SourceSpan::from_attributes(&doc.attributes))
}

struct SymbolIndexData {
    keys: Vec<String>,
    segment_lower: String,
    leaf_lower: String,
    segment_len: usize,
    leaf_len: usize,
}

fn build_symbol_index_data(anchor: &str) -> SymbolIndexData {
    let forms = aden_core::symbol::natural_anchor_forms(anchor);
    let mut keys = vec![
        forms.segment.clone(),
        forms.segment_lower.clone(),
        forms.leaf.clone(),
        forms.leaf_lower.clone(),
    ];
    keys.extend(forms.qualified_prefixes_lower);
    keys.sort();
    keys.dedup();
    let segment_len = forms.segment_lower.chars().count();
    let leaf_len = forms.leaf_lower.chars().count();
    SymbolIndexData {
        keys,
        segment_lower: forms.segment_lower,
        leaf_lower: forms.leaf_lower,
        segment_len,
        leaf_len,
    }
}

fn check_version_marker_bytes(version_path: &Path) -> Result<bool, StoreError> {
    let mut file = match std::fs::File::open(version_path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(StoreError::Io(err.to_string())),
    };
    let mut header = [0_u8; 5];
    let mut read = 0;
    while read < header.len() {
        match file.read(&mut header[read..]) {
            Ok(0) => {
                return Err(StoreError::IncompatibleVersion(format!(
                    "truncated store format marker at {}",
                    version_path.display()
                )));
            }
            Ok(n) => read += n,
            Err(err) => return Err(StoreError::Io(err.to_string())),
        }
    }
    if &header[..4] != STORE_FORMAT_MAGIC || header[4] != STORE_FORMAT_VERSION {
        return Err(StoreError::IncompatibleVersion(format!(
            "unsupported store format marker {header:?} at {}",
            version_path.display()
        )));
    }
    Ok(true)
}

fn dir_is_empty(dir: &Path) -> Result<bool, StoreError> {
    let mut entries = std::fs::read_dir(dir).map_err(|e| StoreError::Io(e.to_string()))?;
    Ok(entries
        .next()
        .transpose()
        .map_err(|e| StoreError::Io(e.to_string()))?
        .is_none())
}

fn configure_connection(conn: &Connection, fresh_db: bool) -> Result<(), StoreError> {
    conn.busy_timeout(Duration::from_millis(5000))
        .map_err(map_sqlite_err)?;
    conn.set_prepared_statement_cache_capacity(64);
    if fresh_db {
        conn.execute_batch(
            "PRAGMA page_size = 4096;\n\
             PRAGMA auto_vacuum = INCREMENTAL;",
        )
        .map_err(map_sqlite_err)?;
    }
    // Keep mmap disabled on Windows so concurrent readers never block remove_dir_all/TRUNCATE.
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;\n\
         PRAGMA synchronous = NORMAL;\n\
         PRAGMA temp_store = MEMORY;\n\
         PRAGMA cache_size = -32768;\n\
         PRAGMA wal_autocheckpoint = 4000;\n\
         PRAGMA mmap_size = 0;\n\
         PRAGMA foreign_keys = OFF;",
    )
    .map_err(map_sqlite_err)?;
    Ok(())
}

fn init_schema(conn: &mut Connection) -> Result<(), StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sqlite_err)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS files (\n\
            id INTEGER PRIMARY KEY,\n\
            rel_path TEXT NOT NULL UNIQUE,\n\
            mtime INTEGER,\n\
            digest TEXT,\n\
            disposition TEXT\n\
         );\n\
         CREATE TABLE IF NOT EXISTS nodes (\n\
            id INTEGER PRIMARY KEY,\n\
            anchor TEXT NOT NULL UNIQUE,\n\
            has_doc INTEGER NOT NULL DEFAULT 0,\n\
            has_canonical_base INTEGER NOT NULL DEFAULT 0,\n\
            node_type INTEGER,\n\
            file_id INTEGER,\n\
            start_line INTEGER,\n\
            end_line INTEGER,\n\
            start_byte INTEGER,\n\
            end_byte INTEGER,\n\
            segment_lower TEXT,\n\
            leaf_lower TEXT,\n\
            segment_len INTEGER,\n\
            leaf_len INTEGER\n\
         );\n\
         CREATE INDEX IF NOT EXISTS idx_nodes_file ON nodes (file_id) WHERE file_id IS NOT NULL;\n\
         CREATE INDEX IF NOT EXISTS idx_nodes_type ON nodes (node_type) WHERE has_doc = 1;\n\
         CREATE TABLE IF NOT EXISTS node_bodies (\n\
            node_id INTEGER PRIMARY KEY,\n\
            doc_blob BLOB NOT NULL\n\
         ) WITHOUT ROWID;\n\
         CREATE TABLE IF NOT EXISTS bases (\n\
            node_id INTEGER PRIMARY KEY,\n\
            base_blob BLOB NOT NULL\n\
         ) WITHOUT ROWID;\n\
         CREATE TABLE IF NOT EXISTS edges (\n\
            src_id INTEGER NOT NULL,\n\
            dst_id INTEGER NOT NULL,\n\
            edge_type INTEGER NOT NULL,\n\
            call_line INTEGER,\n\
            PRIMARY KEY (src_id, dst_id, edge_type)\n\
         ) WITHOUT ROWID;\n\
         CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges (dst_id, edge_type, src_id);\n\
         CREATE TABLE IF NOT EXISTS symbol_exact (\n\
            form TEXT NOT NULL,\n\
            node_id INTEGER NOT NULL,\n\
            PRIMARY KEY (form, node_id)\n\
         ) WITHOUT ROWID;\n\
         CREATE INDEX IF NOT EXISTS idx_symbol_exact_node ON symbol_exact (node_id);\n\
         CREATE TABLE IF NOT EXISTS meta (\n\
            key TEXT PRIMARY KEY,\n\
            value TEXT NOT NULL\n\
         ) WITHOUT ROWID;\n\
         PRAGMA user_version = 1;",
    )
    .map_err(map_sqlite_err)?;
    tx.commit().map_err(map_sqlite_err)?;
    Ok(())
}

fn ensure_schema_indexes(conn: &Connection) -> Result<(), StoreError> {
    let has_idx: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = 'idx_symbol_exact_node')",
            [],
            |r| r.get(0),
        )
        .map_err(map_sqlite_err)?;
    if !has_idx {
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_symbol_exact_node ON symbol_exact (node_id);",
        )
        .map_err(map_sqlite_err)?;
    }
    Ok(())
}

fn validate_existing_schema(conn: &Connection) -> Result<(), StoreError> {
    let user_version: i32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(map_sqlite_err)?;
    if user_version != SQLITE_USER_VERSION {
        return Err(StoreError::IncompatibleVersion(format!(
            "unexpected sqlite user_version {user_version} (expected {SQLITE_USER_VERSION})"
        )));
    }
    Ok(())
}

fn intern_node_cached(
    tx: &Transaction<'_>,
    anchor: &str,
    cache: &mut HashMap<String, i64>,
) -> Result<i64, StoreError> {
    if let Some(&id) = cache.get(anchor) {
        return Ok(id);
    }
    let id = intern_node(tx, anchor)?;
    cache.insert(anchor.to_string(), id);
    Ok(id)
}

fn intern_node(tx: &Transaction<'_>, anchor: &str) -> Result<i64, StoreError> {
    let existing = {
        let mut stmt = tx
            .prepare_cached("SELECT id FROM nodes WHERE anchor = ?1")
            .map_err(map_sqlite_err)?;
        stmt.query_row([anchor], |row| row.get::<_, i64>(0))
            .optional()
            .map_err(map_sqlite_err)?
    };
    if let Some(id) = existing {
        return Ok(id);
    }
    let mut insert = tx
        .prepare_cached("INSERT INTO nodes (anchor) VALUES (?1)")
        .map_err(map_sqlite_err)?;
    insert.execute([anchor]).map_err(map_sqlite_err)?;
    Ok(tx.last_insert_rowid())
}

fn intern_file_cached(
    tx: &Transaction<'_>,
    rel_path: &str,
    cache: &mut HashMap<String, i64>,
) -> Result<i64, StoreError> {
    if let Some(&id) = cache.get(rel_path) {
        return Ok(id);
    }
    let existing = {
        let mut stmt = tx
            .prepare_cached("SELECT id FROM files WHERE rel_path = ?1")
            .map_err(map_sqlite_err)?;
        stmt.query_row([rel_path], |row| row.get::<_, i64>(0))
            .optional()
            .map_err(map_sqlite_err)?
    };
    let id = if let Some(id) = existing {
        id
    } else {
        let mut insert = tx
            .prepare_cached("INSERT INTO files (rel_path) VALUES (?1)")
            .map_err(map_sqlite_err)?;
        insert.execute([rel_path]).map_err(map_sqlite_err)?;
        tx.last_insert_rowid()
    };
    cache.insert(rel_path.to_string(), id);
    Ok(id)
}

fn lookup_node_id(tx: &Transaction<'_>, anchor: &str) -> Result<Option<i64>, StoreError> {
    let mut stmt = tx
        .prepare_cached("SELECT id FROM nodes WHERE anchor = ?1")
        .map_err(map_sqlite_err)?;
    stmt.query_row([anchor], |row| row.get::<_, i64>(0))
        .optional()
        .map_err(map_sqlite_err)
}

fn prune_unreferenced_node(tx: &Transaction<'_>, id: i64) -> Result<(), StoreError> {
    let mut stmt = tx
        .prepare_cached(
            "DELETE FROM nodes WHERE id = ?1 \
             AND has_doc = 0 \
             AND NOT EXISTS (SELECT 1 FROM bases WHERE node_id = ?1) \
             AND NOT EXISTS (SELECT 1 FROM edges WHERE src_id = ?1 OR dst_id = ?1)",
        )
        .map_err(map_sqlite_err)?;
    stmt.execute([id]).map_err(map_sqlite_err)?;
    Ok(())
}

fn prune_orphan_file_if_untracked(tx: &Transaction<'_>, file_id: i64) -> Result<(), StoreError> {
    let mut stmt = tx
        .prepare_cached(
            "DELETE FROM files WHERE id = ?1 \
             AND mtime IS NULL AND digest IS NULL AND disposition IS NULL \
             AND NOT EXISTS (SELECT 1 FROM nodes WHERE file_id = ?1)",
        )
        .map_err(map_sqlite_err)?;
    stmt.execute([file_id]).map_err(map_sqlite_err)?;
    Ok(())
}

enum BaseUpdateMode<'a> {
    /// `put_document`: preserve existing base; materialize lazy canonical base
    /// on overwrite (CoW) before replacing `node_bodies`; leave brand-new rows
    /// at `has_canonical_base = 0`.
    PreserveExistingCoW,
    /// `put_documents_bulk_canonical`: store doc with lazy canonical base
    /// (`has_canonical_base = 1`) and drop any override row in `bases`.
    CanonicalLazy,
    /// `put_documents_bulk`: store doc with an explicit base snapshot string
    /// in `bases` (`has_canonical_base = 0`).
    ExplicitOverride(&'a str),
}

fn upsert_document_by_anchor_in_tx(
    tx: &Transaction<'_>,
    doc: &Document,
    file_cache: &mut HashMap<String, i64>,
    base_mode: BaseUpdateMode<'_>,
) -> Result<(), StoreError> {
    let raw = serialize_document(doc)?;
    let doc_blob = compress_blob(&raw);
    let span = effective_source_span(doc);
    let file_path = span
        .as_ref()
        .map(|s| s.file.as_str())
        .or_else(|| doc.attributes.get("source_file").map(String::as_str));
    let new_file_id = match file_path {
        Some(path) if !path.is_empty() => Some(intern_file_cached(tx, path, file_cache)?),
        _ => None,
    };
    let (start_line, end_line, start_byte, end_byte) = match &span {
        Some(s) => (
            Some(s.start_line as i64),
            Some(s.end_line as i64),
            Some(s.start_byte as i64),
            Some(s.end_byte as i64),
        ),
        None => (None, None, None, None),
    };
    let sym = build_symbol_index_data(&doc.anchor);
    let node_type_code = i64::from(node_type_to_u8(&doc.node_type));

    let existing: Option<(i64, bool, bool, Option<i64>)> = {
        let mut state_stmt = tx
            .prepare_cached(
                "SELECT id, has_doc, has_canonical_base, file_id FROM nodes WHERE anchor = ?1",
            )
            .map_err(map_sqlite_err)?;
        state_stmt
            .query_row([&doc.anchor], |r| {
                Ok((
                    r.get(0)?,
                    r.get::<_, i64>(1)? != 0,
                    r.get::<_, i64>(2)? != 0,
                    r.get(3)?,
                ))
            })
            .optional()
            .map_err(map_sqlite_err)?
    };

    let Some((node_id, prev_has_doc, prev_has_canonical_base, old_file_id)) = existing else {
        // Fast path for brand-new nodes: single INSERT into nodes and zero stale cleanup.
        let new_has_canonical_base: i64 =
            i64::from(matches!(base_mode, BaseUpdateMode::CanonicalLazy));
        {
            let mut ins_node = tx
                .prepare_cached(
                    "INSERT INTO nodes (\
                        anchor, has_doc, has_canonical_base, node_type, file_id, \
                        start_line, end_line, start_byte, end_byte, \
                        segment_lower, leaf_lower, segment_len, leaf_len\
                     ) VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                )
                .map_err(map_sqlite_err)?;
            ins_node
                .execute(params![
                    &doc.anchor,
                    new_has_canonical_base,
                    node_type_code,
                    new_file_id,
                    start_line,
                    end_line,
                    start_byte,
                    end_byte,
                    sym.segment_lower,
                    sym.leaf_lower,
                    sym.segment_len as i64,
                    sym.leaf_len as i64,
                ])
                .map_err(map_sqlite_err)?;
        }
        let node_id = tx.last_insert_rowid();
        {
            let mut ins_body = tx
                .prepare_cached("INSERT INTO node_bodies (node_id, doc_blob) VALUES (?1, ?2)")
                .map_err(map_sqlite_err)?;
            ins_body
                .execute(params![node_id, doc_blob])
                .map_err(map_sqlite_err)?;
        }
        if let BaseUpdateMode::ExplicitOverride(snapshot) = base_mode {
            let base_blob = compress_blob(snapshot.as_bytes());
            let mut ins_base = tx
                .prepare_cached("INSERT INTO bases (node_id, base_blob) VALUES (?1, ?2)")
                .map_err(map_sqlite_err)?;
            ins_base
                .execute(params![node_id, base_blob])
                .map_err(map_sqlite_err)?;
        }
        {
            let mut ins_exact = tx
                .prepare_cached("INSERT INTO symbol_exact (form, node_id) VALUES (?1, ?2)")
                .map_err(map_sqlite_err)?;
            for key in &sym.keys {
                ins_exact
                    .execute(params![key, node_id])
                    .map_err(map_sqlite_err)?;
            }
        }
        return Ok(());
    };

    let new_has_canonical_base: i64 = match base_mode {
        BaseUpdateMode::PreserveExistingCoW => {
            if prev_has_doc && prev_has_canonical_base {
                let has_explicit: bool = {
                    let mut check = tx
                        .prepare_cached("SELECT EXISTS(SELECT 1 FROM bases WHERE node_id = ?1)")
                        .map_err(map_sqlite_err)?;
                    check
                        .query_row([node_id], |r| r.get(0))
                        .map_err(map_sqlite_err)?
                };
                if !has_explicit {
                    let old_blob: Option<Vec<u8>> = {
                        let mut body_stmt = tx
                            .prepare_cached("SELECT doc_blob FROM node_bodies WHERE node_id = ?1")
                            .map_err(map_sqlite_err)?;
                        body_stmt
                            .query_row([node_id], |r| r.get(0))
                            .optional()
                            .map_err(map_sqlite_err)?
                    };
                    if let Some(blob) = old_blob {
                        let raw = decompress_blob(&blob)?;
                        let old_doc = deserialize_document(&raw)?;
                        let canonical_text = emit_canonical_base(&old_doc);
                        let base_blob = compress_blob(canonical_text.as_bytes());
                        let mut ins_base = tx
                            .prepare_cached(
                                "INSERT INTO bases (node_id, base_blob) VALUES (?1, ?2) \
                                 ON CONFLICT(node_id) DO UPDATE SET base_blob = excluded.base_blob",
                            )
                            .map_err(map_sqlite_err)?;
                        ins_base
                            .execute(params![node_id, base_blob])
                            .map_err(map_sqlite_err)?;
                    }
                }
            }
            0
        }
        BaseUpdateMode::CanonicalLazy => {
            if !prev_has_canonical_base {
                let mut del_base = tx
                    .prepare_cached("DELETE FROM bases WHERE node_id = ?1")
                    .map_err(map_sqlite_err)?;
                del_base.execute([node_id]).map_err(map_sqlite_err)?;
            }
            1
        }
        BaseUpdateMode::ExplicitOverride(snapshot) => {
            let base_blob = compress_blob(snapshot.as_bytes());
            let mut ins_base = tx
                .prepare_cached(
                    "INSERT INTO bases (node_id, base_blob) VALUES (?1, ?2) \
                     ON CONFLICT(node_id) DO UPDATE SET base_blob = excluded.base_blob",
                )
                .map_err(map_sqlite_err)?;
            ins_base
                .execute(params![node_id, base_blob])
                .map_err(map_sqlite_err)?;
            0
        }
    };

    {
        let mut upsert_body = tx
            .prepare_cached(
                "INSERT INTO node_bodies (node_id, doc_blob) VALUES (?1, ?2) \
                 ON CONFLICT(node_id) DO UPDATE SET doc_blob = excluded.doc_blob",
            )
            .map_err(map_sqlite_err)?;
        upsert_body
            .execute(params![node_id, doc_blob])
            .map_err(map_sqlite_err)?;
    }

    {
        let mut update_node = tx
            .prepare_cached(
                "UPDATE nodes SET \
                    has_doc = 1, \
                    has_canonical_base = ?2, \
                    node_type = ?3, \
                    file_id = ?4, \
                    start_line = ?5, \
                    end_line = ?6, \
                    start_byte = ?7, \
                    end_byte = ?8, \
                    segment_lower = ?9, \
                    leaf_lower = ?10, \
                    segment_len = ?11, \
                    leaf_len = ?12 \
                 WHERE id = ?1",
            )
            .map_err(map_sqlite_err)?;
        update_node
            .execute(params![
                node_id,
                new_has_canonical_base,
                node_type_code,
                new_file_id,
                start_line,
                end_line,
                start_byte,
                end_byte,
                sym.segment_lower,
                sym.leaf_lower,
                sym.segment_len as i64,
                sym.leaf_len as i64,
            ])
            .map_err(map_sqlite_err)?;
    }

    if let Some(prev_fid) = old_file_id
        && Some(prev_fid) != new_file_id
    {
        prune_orphan_file_if_untracked(tx, prev_fid)?;
    }

    if prev_has_doc {
        let mut del_exact = tx
            .prepare_cached("DELETE FROM symbol_exact WHERE node_id = ?1")
            .map_err(map_sqlite_err)?;
        del_exact.execute([node_id]).map_err(map_sqlite_err)?;
    }
    {
        let mut ins_exact = tx
            .prepare_cached("INSERT OR IGNORE INTO symbol_exact (form, node_id) VALUES (?1, ?2)")
            .map_err(map_sqlite_err)?;
        for key in &sym.keys {
            ins_exact
                .execute(params![key, node_id])
                .map_err(map_sqlite_err)?;
        }
    }
    Ok(())
}

fn load_node_table(conn: &Connection) -> Result<HashMap<i64, String>, StoreError> {
    let mut stmt = conn
        .prepare_cached("SELECT id, anchor FROM nodes")
        .map_err(map_sqlite_err)?;
    let mut rows = stmt.query([]).map_err(map_sqlite_err)?;
    let mut map = HashMap::new();
    while let Some(row) = rows.next().map_err(map_sqlite_err)? {
        let id: i64 = row.get(0).map_err(map_sqlite_err)?;
        let anchor: String = row.get(1).map_err(map_sqlite_err)?;
        map.insert(id, anchor);
    }
    Ok(map)
}

fn row_to_span(
    file: String,
    start_line: i64,
    end_line: i64,
    start_byte: i64,
    end_byte: i64,
) -> SourceSpan {
    SourceSpan {
        file,
        start_line: start_line.max(0) as usize,
        end_line: end_line.max(0) as usize,
        start_byte: start_byte.max(0) as usize,
        end_byte: end_byte.max(0) as usize,
    }
}

/// A SQLite WAL-backed implementation of [`GraphStorage`].
pub struct SqliteStorage {
    conn: Mutex<Connection>,
}

impl SqliteStorage {
    fn lock_conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Remove generated call and test edges for the supplied callers atomically.
    /// `Tests`, like `Calls`, points from the caller to the tested symbol.
    pub fn clear_generated_calls(&self, callers: &[&str]) -> Result<(), StoreError> {
        if callers.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock_conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_err)?;
        let calls_code = i64::from(edge_to_u8(EdgeType::Calls));
        let tests_code = i64::from(edge_to_u8(EdgeType::Tests));
        let mut seen = HashSet::new();
        for &caller in callers {
            if !seen.insert(caller) {
                continue;
            }
            let Some(caller_id) = lookup_node_id(&tx, caller)? else {
                continue;
            };
            let mut del = tx
                .prepare_cached("DELETE FROM edges WHERE src_id = ?1 AND edge_type IN (?2, ?3)")
                .map_err(map_sqlite_err)?;
            del.execute(params![caller_id, calls_code, tests_code])
                .map_err(map_sqlite_err)?;
        }
        tx.commit().map_err(map_sqlite_err)?;
        Ok(())
    }

    /// Recognize the current engine's format marker without opening SQLite.
    ///
    /// Reads only the five-byte header of `path/version`, without creating files
    /// or opening database handles. Missing, unreadable, or unknown markers
    /// return `false` so the normal open/recovery path can classify them.
    pub fn has_current_format_marker(path: impl AsRef<Path>) -> bool {
        let Ok(mut marker) = std::fs::File::open(path.as_ref().join("version")) else {
            return false;
        };
        let mut header = [0_u8; 5];
        if marker.read_exact(&mut header).is_err() {
            return false;
        }
        &header[..4] == STORE_FORMAT_MAGIC && header[4] == STORE_FORMAT_VERSION
    }

    /// Open or create a SQLite graph store at the given directory path.
    pub fn new(path: &str) -> Result<Self, StoreError> {
        Self::open_once(path, true)
    }

    /// Open with retry when SQLite reports Busy/Locked. Waits up to `timeout`,
    /// emitting a `NOTE:` every 5s.
    pub fn new_with_retry(path: &str, timeout: Duration) -> Result<Self, StoreError> {
        let deadline = Instant::now() + timeout;
        let started = Instant::now();
        let mut last_note = started;
        const NOTE_EVERY: Duration = Duration::from_secs(5);
        const POLL: Duration = Duration::from_millis(100);

        loop {
            match Self::open_once(path, true) {
                Ok(storage) => return Ok(storage),
                Err(e) if store_error_is_locked(&e) => {
                    let now = Instant::now();
                    if now.duration_since(last_note) >= NOTE_EVERY {
                        eprintln!(
                            "NOTE: waiting for sqlite store at {path} (waited {}s)…",
                            now.duration_since(started).as_secs()
                        );
                        last_note = now;
                    }
                    if now >= deadline {
                        return Err(StoreError::Io(format!(
                            "store locked at {path} — another writer still holds the database lock \
                             (waited {timeout:?})."
                        )));
                    }
                    sleep(POLL);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Open an existing SQLite store without creating directories or files.
    pub fn open_existing(path: &str) -> Result<Self, StoreError> {
        Self::open_once(path, false)
    }

    fn open_once(path: &str, create: bool) -> Result<Self, StoreError> {
        let dir = Path::new(path);
        let version_path = dir.join("version");
        let db_path = dir.join(STORE_DB_FILENAME);

        if !dir.exists() {
            if !create {
                return Err(StoreError::NotFound(format!(
                    "no store at {path}; run 'aden gen' at the project root first"
                )));
            }
            std::fs::create_dir_all(dir).map_err(|e| StoreError::Io(e.to_string()))?;
        } else if !dir.is_dir() {
            return Err(StoreError::Io(format!(
                "store path is not a directory: {path}"
            )));
        }

        let empty = dir_is_empty(dir)?;
        if empty && !create {
            return Err(StoreError::NotFound(format!(
                "no store at {path}; run 'aden gen' at the project root first"
            )));
        }

        let marker_valid = check_version_marker_bytes(&version_path)?;
        if !empty {
            if !marker_valid {
                return Err(StoreError::IncompatibleVersion(format!(
                    "missing or legacy store format marker in {path}"
                )));
            }
            if !db_path.is_file() {
                return Err(StoreError::IncompatibleVersion(format!(
                    "store missing {STORE_DB_FILENAME} in {path}"
                )));
            }
        }

        let fresh_db = !db_path.is_file();
        if !create && fresh_db {
            return Err(StoreError::NotFound(format!(
                "no store database at {}; run 'aden gen' first",
                db_path.display()
            )));
        }

        let flags = if create {
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX
        };

        let mut conn = Connection::open_with_flags(&db_path, flags).map_err(|e| {
            if sqlite_error_is_locked(&e) {
                StoreError::Io(format!("DatabaseLocked: {e}"))
            } else {
                StoreError::Io(e.to_string())
            }
        })?;
        configure_connection(&conn, fresh_db)?;
        if fresh_db {
            init_schema(&mut conn)?;
            if !marker_valid {
                let mut marker = Vec::with_capacity(5);
                marker.extend_from_slice(STORE_FORMAT_MAGIC);
                marker.push(STORE_FORMAT_VERSION);
                std::fs::write(&version_path, &marker)
                    .map_err(|e| StoreError::Io(e.to_string()))?;
            }
        } else {
            validate_existing_schema(&conn)?;
            if create {
                ensure_schema_indexes(&conn)?;
            }
        }

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Read source spans for selected anchors while preserving projection precedence:
    /// a non-empty projected `nodes` span set is authoritative even when partial;
    /// document blob fallback runs only when no node carries a projected span.
    pub fn get_source_spans_for<'a>(
        &self,
        anchors: impl IntoIterator<Item = &'a str>,
    ) -> Result<Vec<(String, SourceSpan)>, StoreError> {
        let conn = self.lock_conn();
        let projection_populated: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes \
                 WHERE has_doc = 1 AND file_id IS NOT NULL AND start_line IS NOT NULL)",
                [],
                |r| r.get(0),
            )
            .map_err(map_sqlite_err)?;
        let mut spans = Vec::new();
        if projection_populated {
            let mut stmt = conn
                .prepare_cached(
                    "SELECT f.rel_path, n.start_line, n.end_line, n.start_byte, n.end_byte \
                     FROM nodes n \
                     JOIN files f ON f.id = n.file_id \
                     WHERE n.anchor = ?1 \
                       AND n.has_doc = 1 \
                       AND n.file_id IS NOT NULL \
                       AND n.start_line IS NOT NULL \
                       AND n.end_line IS NOT NULL \
                       AND n.start_byte IS NOT NULL \
                       AND n.end_byte IS NOT NULL",
                )
                .map_err(map_sqlite_err)?;
            for anchor in anchors {
                if let Some(span) = stmt
                    .query_row([anchor], |row| {
                        Ok(row_to_span(
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    })
                    .optional()
                    .map_err(map_sqlite_err)?
                {
                    spans.push((anchor.to_string(), span));
                }
            }
        } else {
            let mut stmt = conn
                .prepare_cached(
                    "SELECT nb.doc_blob FROM node_bodies nb \
                     JOIN nodes n ON n.id = nb.node_id \
                     WHERE n.anchor = ?1",
                )
                .map_err(map_sqlite_err)?;
            for anchor in anchors {
                if let Some(blob) = stmt
                    .query_row([anchor], |row| row.get::<_, Vec<u8>>(0))
                    .optional()
                    .map_err(map_sqlite_err)?
                {
                    let raw = decompress_blob(&blob)?;
                    if let Some(span) = deserialize_document(&raw)?.source_span {
                        spans.push((anchor.to_string(), span));
                    }
                }
            }
        }
        Ok(spans)
    }
}

impl GraphStorage for SqliteStorage {
    fn get_document(&self, anchor: &str) -> Result<Option<Document>, StoreError> {
        let conn = self.lock_conn();
        let mut stmt = conn
            .prepare_cached(
                "SELECT nb.doc_blob FROM node_bodies nb \
                 JOIN nodes n ON n.id = nb.node_id \
                 WHERE n.anchor = ?1 AND n.has_doc = 1",
            )
            .map_err(map_sqlite_err)?;
        let Some(blob) = stmt
            .query_row([anchor], |row| row.get::<_, Vec<u8>>(0))
            .optional()
            .map_err(map_sqlite_err)?
        else {
            return Ok(None);
        };
        let raw = decompress_blob(&blob)?;
        Ok(Some(deserialize_document(&raw)?))
    }

    fn get_all_documents(&self) -> Result<HashMap<String, Document>, StoreError> {
        let conn = self.lock_conn();
        let mut stmt = conn
            .prepare_cached(
                "SELECT n.anchor, nb.doc_blob FROM node_bodies nb \
                 JOIN nodes n ON n.id = nb.node_id \
                 WHERE n.has_doc = 1",
            )
            .map_err(map_sqlite_err)?;
        let mut rows = stmt.query([]).map_err(map_sqlite_err)?;
        let mut docs = HashMap::new();
        while let Some(row) = rows.next().map_err(map_sqlite_err)? {
            let anchor: String = row.get(0).map_err(map_sqlite_err)?;
            let blob: Vec<u8> = row.get(1).map_err(map_sqlite_err)?;
            let raw = decompress_blob(&blob)?;
            docs.insert(anchor, deserialize_document(&raw)?);
        }
        Ok(docs)
    }

    fn visit_documents_ordered(
        &self,
        on_count: &mut dyn FnMut(usize) -> Result<(), StoreError>,
        on_doc: &mut dyn FnMut(String, Document) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let mut conn = self.lock_conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(map_sqlite_err)?;
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM nodes n \
                 JOIN node_bodies nb ON nb.node_id = n.id \
                 WHERE n.has_doc = 1",
                [],
                |row| row.get(0),
            )
            .map_err(map_sqlite_err)?;
        on_count(count.max(0) as usize)?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "SELECT n.anchor, nb.doc_blob FROM nodes n \
                     JOIN node_bodies nb ON nb.node_id = n.id \
                     WHERE n.has_doc = 1 \
                     ORDER BY n.anchor ASC",
                )
                .map_err(map_sqlite_err)?;
            let mut rows = stmt.query([]).map_err(map_sqlite_err)?;
            while let Some(row) = rows.next().map_err(map_sqlite_err)? {
                let anchor: String = row.get(0).map_err(map_sqlite_err)?;
                let blob: Vec<u8> = row.get(1).map_err(map_sqlite_err)?;
                let raw = decompress_blob(&blob)?;
                on_doc(anchor, deserialize_document(&raw)?)?;
            }
        }
        tx.commit().map_err(map_sqlite_err)?;
        Ok(())
    }

    fn get_source_spans(&self) -> Result<Vec<(String, SourceSpan)>, StoreError> {
        let conn = self.lock_conn();
        let projection_populated: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes \
                 WHERE has_doc = 1 AND file_id IS NOT NULL AND start_line IS NOT NULL)",
                [],
                |r| r.get(0),
            )
            .map_err(map_sqlite_err)?;
        let mut spans = Vec::new();
        if projection_populated {
            let mut stmt = conn
                .prepare_cached(
                    "SELECT n.anchor, f.rel_path, n.start_line, n.end_line, n.start_byte, n.end_byte \
                     FROM nodes n \
                     JOIN files f ON f.id = n.file_id \
                     WHERE n.has_doc = 1 \
                       AND n.file_id IS NOT NULL \
                       AND n.start_line IS NOT NULL \
                       AND n.end_line IS NOT NULL \
                       AND n.start_byte IS NOT NULL \
                       AND n.end_byte IS NOT NULL \
                     ORDER BY n.anchor ASC",
                )
                .map_err(map_sqlite_err)?;
            let mut rows = stmt.query([]).map_err(map_sqlite_err)?;
            while let Some(row) = rows.next().map_err(map_sqlite_err)? {
                let anchor: String = row.get(0).map_err(map_sqlite_err)?;
                spans.push((
                    anchor,
                    row_to_span(
                        row.get(1).map_err(map_sqlite_err)?,
                        row.get(2).map_err(map_sqlite_err)?,
                        row.get(3).map_err(map_sqlite_err)?,
                        row.get(4).map_err(map_sqlite_err)?,
                        row.get(5).map_err(map_sqlite_err)?,
                    ),
                ));
            }
            return Ok(spans);
        }

        let mut stmt = conn
            .prepare_cached(
                "SELECT n.anchor, nb.doc_blob FROM node_bodies nb \
                 JOIN nodes n ON n.id = nb.node_id \
                 WHERE n.has_doc = 1 \
                 ORDER BY n.anchor ASC",
            )
            .map_err(map_sqlite_err)?;
        let mut rows = stmt.query([]).map_err(map_sqlite_err)?;
        while let Some(row) = rows.next().map_err(map_sqlite_err)? {
            let anchor: String = row.get(0).map_err(map_sqlite_err)?;
            let blob: Vec<u8> = row.get(1).map_err(map_sqlite_err)?;
            let raw = decompress_blob(&blob)?;
            if let Some(span) = deserialize_document(&raw)?.source_span {
                spans.push((anchor, span));
            }
        }
        Ok(spans)
    }

    fn get_source_file(&self, anchor: &str) -> Result<Option<String>, StoreError> {
        let conn = self.lock_conn();
        let mut stmt = conn
            .prepare_cached(
                "SELECT f.rel_path FROM nodes n \
                 JOIN files f ON f.id = n.file_id \
                 WHERE n.anchor = ?1 AND n.has_doc = 1",
            )
            .map_err(map_sqlite_err)?;
        stmt.query_row([anchor], |row| row.get::<_, String>(0))
            .optional()
            .map_err(map_sqlite_err)
    }

    fn sync_file_records(&self, records: &[FileRecord]) -> Result<(), StoreError> {
        let mut conn = self.lock_conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_err)?;

        let keep: HashSet<&str> = records.iter().map(|r| r.rel_path.as_str()).collect();
        let mut stale_ids = Vec::new();
        {
            let mut stmt = tx
                .prepare_cached(
                    "SELECT id, rel_path FROM files \
                     WHERE mtime IS NOT NULL OR digest IS NOT NULL OR disposition IS NOT NULL",
                )
                .map_err(map_sqlite_err)?;
            let mut rows = stmt.query([]).map_err(map_sqlite_err)?;
            while let Some(row) = rows.next().map_err(map_sqlite_err)? {
                let id: i64 = row.get(0).map_err(map_sqlite_err)?;
                let rel_path: String = row.get(1).map_err(map_sqlite_err)?;
                if !keep.contains(rel_path.as_str()) {
                    stale_ids.push(id);
                }
            }
        }
        {
            let mut clear_stale = tx
                .prepare_cached(
                    "UPDATE files SET mtime = NULL, digest = NULL, disposition = NULL WHERE id = ?1",
                )
                .map_err(map_sqlite_err)?;
            for id in stale_ids {
                clear_stale.execute([id]).map_err(map_sqlite_err)?;
                prune_orphan_file_if_untracked(&tx, id)?;
            }
        }

        {
            let mut upsert = tx
                .prepare_cached(
                    "INSERT INTO files (rel_path, mtime, digest, disposition) VALUES (?1, ?2, ?3, ?4) \
                     ON CONFLICT(rel_path) DO UPDATE SET \
                        mtime = excluded.mtime, \
                        digest = excluded.digest, \
                        disposition = excluded.disposition",
                )
                .map_err(map_sqlite_err)?;
            for rec in records {
                let mtime = rec.mtime.map(|m| m as i64);
                upsert
                    .execute(params![
                        rec.rel_path,
                        mtime,
                        rec.digest.as_deref(),
                        rec.disposition.as_deref(),
                    ])
                    .map_err(map_sqlite_err)?;
            }
        }

        tx.commit().map_err(map_sqlite_err)?;
        Ok(())
    }

    fn get_nodes_in_file(&self, rel_path: &str) -> Result<Vec<String>, StoreError> {
        let conn = self.lock_conn();
        let mut stmt = conn
            .prepare_cached(
                "SELECT n.anchor FROM nodes n \
                 JOIN files f ON f.id = n.file_id \
                 WHERE f.rel_path = ?1 AND n.has_doc = 1 \
                 ORDER BY COALESCE(n.start_line, 0) ASC, n.anchor ASC",
            )
            .map_err(map_sqlite_err)?;
        let mut rows = stmt.query([rel_path]).map_err(map_sqlite_err)?;
        let mut anchors = Vec::new();
        while let Some(row) = rows.next().map_err(map_sqlite_err)? {
            anchors.push(row.get::<_, String>(0).map_err(map_sqlite_err)?);
        }
        Ok(anchors)
    }

    fn put_document(&self, doc: &Document) -> Result<(), StoreError> {
        let mut conn = self.lock_conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_err)?;
        let mut file_cache = HashMap::new();
        upsert_document_by_anchor_in_tx(
            &tx,
            doc,
            &mut file_cache,
            BaseUpdateMode::PreserveExistingCoW,
        )?;
        tx.commit().map_err(map_sqlite_err)?;
        Ok(())
    }

    fn delete_document(&self, anchor: &str) -> Result<(), StoreError> {
        let mut conn = self.lock_conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_err)?;
        if let Some(id) = lookup_node_id(&tx, anchor)? {
            let old_file_id: Option<i64> = {
                let mut stmt = tx
                    .prepare_cached("SELECT file_id FROM nodes WHERE id = ?1")
                    .map_err(map_sqlite_err)?;
                stmt.query_row([id], |r| r.get(0)).map_err(map_sqlite_err)?
            };
            for sql in [
                "DELETE FROM node_bodies WHERE node_id = ?1",
                "DELETE FROM bases WHERE node_id = ?1",
                "DELETE FROM symbol_exact WHERE node_id = ?1",
                "UPDATE nodes SET \
                    has_doc = 0, \
                    has_canonical_base = 0, \
                    node_type = NULL, \
                    file_id = NULL, \
                    start_line = NULL, \
                    end_line = NULL, \
                    start_byte = NULL, \
                    end_byte = NULL, \
                    segment_lower = NULL, \
                    leaf_lower = NULL, \
                    segment_len = NULL, \
                    leaf_len = NULL \
                 WHERE id = ?1",
            ] {
                let mut stmt = tx.prepare_cached(sql).map_err(map_sqlite_err)?;
                stmt.execute([id]).map_err(map_sqlite_err)?;
            }
            prune_unreferenced_node(&tx, id)?;
            if let Some(fid) = old_file_id {
                prune_orphan_file_if_untracked(&tx, fid)?;
            }
        }
        tx.commit().map_err(map_sqlite_err)?;
        Ok(())
    }

    fn put_base_snapshot(&self, anchor: &str, text: &str) -> Result<(), StoreError> {
        let mut conn = self.lock_conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_err)?;
        let id = intern_node(&tx, anchor)?;
        let blob = compress_blob(text.as_bytes());
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT INTO bases (node_id, base_blob) VALUES (?1, ?2) \
                     ON CONFLICT(node_id) DO UPDATE SET base_blob = excluded.base_blob",
                )
                .map_err(map_sqlite_err)?;
            stmt.execute(params![id, blob]).map_err(map_sqlite_err)?;
        }
        {
            let mut stmt = tx
                .prepare_cached("UPDATE nodes SET has_canonical_base = 0 WHERE id = ?1")
                .map_err(map_sqlite_err)?;
            stmt.execute([id]).map_err(map_sqlite_err)?;
        }
        tx.commit().map_err(map_sqlite_err)?;
        Ok(())
    }

    fn get_base_snapshot(&self, anchor: &str) -> Result<Option<String>, StoreError> {
        let conn = self.lock_conn();
        let row: Option<(i64, bool, bool, Option<Vec<u8>>)> = {
            let mut stmt = conn
                .prepare_cached(
                    "SELECT n.id, n.has_doc, n.has_canonical_base, b.base_blob \
                     FROM nodes n \
                     LEFT JOIN bases b ON b.node_id = n.id \
                     WHERE n.anchor = ?1",
                )
                .map_err(map_sqlite_err)?;
            stmt.query_row([anchor], |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, i64>(1)? != 0,
                    row.get::<_, i64>(2)? != 0,
                    row.get(3)?,
                ))
            })
            .optional()
            .map_err(map_sqlite_err)?
        };
        let Some((node_id, has_doc, has_canonical_base, explicit_blob)) = row else {
            return Ok(None);
        };
        if let Some(blob) = explicit_blob {
            let raw = decompress_blob(&blob)?;
            let text = String::from_utf8(raw).map_err(|e| {
                StoreError::Serialization(format!("base snapshot for '{anchor}' not UTF-8: {e}"))
            })?;
            return Ok(Some(text));
        }
        if has_doc && has_canonical_base {
            let mut stmt = conn
                .prepare_cached("SELECT doc_blob FROM node_bodies WHERE node_id = ?1")
                .map_err(map_sqlite_err)?;
            let Some(doc_blob) = stmt
                .query_row([node_id], |row| row.get::<_, Vec<u8>>(0))
                .optional()
                .map_err(map_sqlite_err)?
            else {
                return Ok(None);
            };
            let raw = decompress_blob(&doc_blob)?;
            let doc = deserialize_document(&raw)?;
            return Ok(Some(emit_canonical_base(&doc)));
        }
        Ok(None)
    }

    fn delete_base_snapshot(&self, anchor: &str) -> Result<(), StoreError> {
        let mut conn = self.lock_conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_err)?;
        if let Some(id) = lookup_node_id(&tx, anchor)? {
            {
                let mut stmt = tx
                    .prepare_cached("DELETE FROM bases WHERE node_id = ?1")
                    .map_err(map_sqlite_err)?;
                stmt.execute([id]).map_err(map_sqlite_err)?;
            }
            {
                let mut stmt = tx
                    .prepare_cached("UPDATE nodes SET has_canonical_base = 0 WHERE id = ?1")
                    .map_err(map_sqlite_err)?;
                stmt.execute([id]).map_err(map_sqlite_err)?;
            }
            prune_unreferenced_node(&tx, id)?;
        }
        tx.commit().map_err(map_sqlite_err)?;
        Ok(())
    }

    fn put_documents_bulk(&self, docs: &[(Document, String)]) -> Result<(), StoreError> {
        if docs.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock_conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_err)?;
        let mut file_cache = HashMap::new();
        for (doc, snapshot) in docs {
            upsert_document_by_anchor_in_tx(
                &tx,
                doc,
                &mut file_cache,
                BaseUpdateMode::ExplicitOverride(snapshot),
            )?;
        }
        #[cfg(test)]
        if FAIL_BULK_BEFORE_COMMIT.with(|fail| fail.replace(false)) {
            return Err(StoreError::Io("injected failure before bulk commit".into()));
        }
        tx.commit().map_err(map_sqlite_err)?;
        Ok(())
    }

    fn put_documents_bulk_canonical(&self, docs: &[Document]) -> Result<(), StoreError> {
        if docs.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock_conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_err)?;
        let mut file_cache = HashMap::new();
        for doc in docs {
            upsert_document_by_anchor_in_tx(
                &tx,
                doc,
                &mut file_cache,
                BaseUpdateMode::CanonicalLazy,
            )?;
        }
        #[cfg(test)]
        if FAIL_BULK_BEFORE_COMMIT.with(|fail| fail.replace(false)) {
            return Err(StoreError::Io("injected failure before bulk commit".into()));
        }
        tx.commit().map_err(map_sqlite_err)?;
        Ok(())
    }

    fn get_outgoing_edges(&self, anchor: &str) -> Result<Vec<(String, EdgeType)>, StoreError> {
        let conn = self.lock_conn();
        let mut stmt = conn
            .prepare_cached(
                "SELECT dst.anchor, e.edge_type \
                 FROM edges e \
                 JOIN nodes src ON src.id = e.src_id \
                 JOIN nodes dst ON dst.id = e.dst_id \
                 WHERE src.anchor = ?1",
            )
            .map_err(map_sqlite_err)?;
        let mut rows = stmt.query([anchor]).map_err(map_sqlite_err)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(map_sqlite_err)? {
            let target: String = row.get(0).map_err(map_sqlite_err)?;
            let code: u8 = row.get(1).map_err(map_sqlite_err)?;
            out.push((target, u8_to_edge(code)?));
        }
        Ok(out)
    }

    fn get_incoming_edges(&self, anchor: &str) -> Result<Vec<(String, EdgeType)>, StoreError> {
        let conn = self.lock_conn();
        let mut stmt = conn
            .prepare_cached(
                "SELECT src.anchor, e.edge_type \
                 FROM edges e \
                 JOIN nodes dst ON dst.id = e.dst_id \
                 JOIN nodes src ON src.id = e.src_id \
                 WHERE dst.anchor = ?1",
            )
            .map_err(map_sqlite_err)?;
        let mut rows = stmt.query([anchor]).map_err(map_sqlite_err)?;
        let mut inc = Vec::new();
        while let Some(row) = rows.next().map_err(map_sqlite_err)? {
            let source: String = row.get(0).map_err(map_sqlite_err)?;
            let code: u8 = row.get(1).map_err(map_sqlite_err)?;
            inc.push((source, u8_to_edge(code)?));
        }
        Ok(inc)
    }

    fn put_edge(&self, src: &str, dst: &str, edge_type: EdgeType) -> Result<(), StoreError> {
        let mut conn = self.lock_conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_err)?;
        let src_id = intern_node(&tx, src)?;
        let dst_id = intern_node(&tx, dst)?;
        let code = i64::from(edge_to_u8(edge_type));
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT OR IGNORE INTO edges (src_id, dst_id, edge_type) VALUES (?1, ?2, ?3)",
                )
                .map_err(map_sqlite_err)?;
            stmt.execute(params![src_id, dst_id, code])
                .map_err(map_sqlite_err)?;
        }
        tx.commit().map_err(map_sqlite_err)?;
        Ok(())
    }

    fn put_edges_bulk(&self, edges: &[(String, String, EdgeType)]) -> Result<(), StoreError> {
        if edges.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock_conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_err)?;
        let mut id_cache = if edges.len() >= 1024 {
            let mut map = HashMap::new();
            {
                let mut load_stmt = tx
                    .prepare_cached("SELECT anchor, id FROM nodes")
                    .map_err(map_sqlite_err)?;
                let mut rows = load_stmt.query([]).map_err(map_sqlite_err)?;
                while let Some(row) = rows.next().map_err(map_sqlite_err)? {
                    let anchor: String = row.get(0).map_err(map_sqlite_err)?;
                    let id: i64 = row.get(1).map_err(map_sqlite_err)?;
                    map.insert(anchor, id);
                }
            }
            map
        } else {
            HashMap::new()
        };
        {
            let mut insert_edge = tx
                .prepare_cached(
                    "INSERT OR IGNORE INTO edges (src_id, dst_id, edge_type) VALUES (?1, ?2, ?3)",
                )
                .map_err(map_sqlite_err)?;
            for (src, dst, edge_type) in edges {
                let src_id = intern_node_cached(&tx, src, &mut id_cache)?;
                let dst_id = intern_node_cached(&tx, dst, &mut id_cache)?;
                let code = i64::from(edge_to_u8(*edge_type));
                insert_edge
                    .execute(params![src_id, dst_id, code])
                    .map_err(map_sqlite_err)?;
            }
        }
        tx.commit().map_err(map_sqlite_err)?;
        Ok(())
    }

    fn delete_edge(&self, src: &str, dst: &str, edge_type: &EdgeType) -> Result<(), StoreError> {
        let mut conn = self.lock_conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_err)?;
        if let (Some(src_id), Some(dst_id)) = (lookup_node_id(&tx, src)?, lookup_node_id(&tx, dst)?)
        {
            let code = i64::from(edge_to_u8(*edge_type));
            {
                let mut stmt = tx
                    .prepare_cached(
                        "DELETE FROM edges WHERE src_id = ?1 AND dst_id = ?2 AND edge_type = ?3",
                    )
                    .map_err(map_sqlite_err)?;
                stmt.execute(params![src_id, dst_id, code])
                    .map_err(map_sqlite_err)?;
            }
            prune_unreferenced_node(&tx, src_id)?;
            if dst_id != src_id {
                prune_unreferenced_node(&tx, dst_id)?;
            }
        }
        tx.commit().map_err(map_sqlite_err)?;
        Ok(())
    }

    fn delete_node(&self, anchor: &str) -> Result<(), StoreError> {
        self.delete_nodes_bulk(&[anchor])
    }

    fn delete_nodes_bulk(&self, anchors: &[&str]) -> Result<(), StoreError> {
        if anchors.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock_conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_err)?;
        let mut seen = HashSet::with_capacity(anchors.len());
        let mut candidate_file_ids = HashSet::new();
        for &anchor in anchors {
            if !seen.insert(anchor) {
                continue;
            }
            let Some(id) = lookup_node_id(&tx, anchor)? else {
                continue;
            };
            {
                let mut fstmt = tx
                    .prepare_cached("SELECT file_id FROM nodes WHERE id = ?1")
                    .map_err(map_sqlite_err)?;
                if let Some(fid) = fstmt
                    .query_row([id], |r| r.get::<_, Option<i64>>(0))
                    .map_err(map_sqlite_err)?
                {
                    candidate_file_ids.insert(fid);
                }
            }
            for sql in [
                "DELETE FROM node_bodies WHERE node_id = ?1",
                "DELETE FROM bases WHERE node_id = ?1",
                "DELETE FROM symbol_exact WHERE node_id = ?1",
                "DELETE FROM edges WHERE src_id = ?1",
                "DELETE FROM edges WHERE dst_id = ?1",
                "DELETE FROM nodes WHERE id = ?1",
            ] {
                let mut stmt = tx.prepare_cached(sql).map_err(map_sqlite_err)?;
                stmt.execute([id]).map_err(map_sqlite_err)?;
            }
        }
        for fid in candidate_file_ids {
            prune_orphan_file_if_untracked(&tx, fid)?;
        }
        tx.commit().map_err(map_sqlite_err)?;
        Ok(())
    }

    fn get_edges_by_type(&self, edge_type: &EdgeType) -> Result<Vec<(String, String)>, StoreError> {
        let conn = self.lock_conn();
        let nodes = load_node_table(&conn)?;
        let code = i64::from(edge_to_u8(*edge_type));
        let mut stmt = conn
            .prepare_cached("SELECT src_id, dst_id FROM edges WHERE edge_type = ?1")
            .map_err(map_sqlite_err)?;
        let mut rows = stmt.query([code]).map_err(map_sqlite_err)?;
        let mut edges = Vec::new();
        while let Some(row) = rows.next().map_err(map_sqlite_err)? {
            let src_id: i64 = row.get(0).map_err(map_sqlite_err)?;
            let dst_id: i64 = row.get(1).map_err(map_sqlite_err)?;
            if let (Some(src), Some(dst)) = (nodes.get(&src_id), nodes.get(&dst_id)) {
                edges.push((src.clone(), dst.clone()));
            }
        }
        Ok(edges)
    }

    fn get_all_edges(&self) -> Result<Vec<(String, String, EdgeType)>, StoreError> {
        let conn = self.lock_conn();
        let nodes = load_node_table(&conn)?;
        let mut stmt = conn
            .prepare_cached("SELECT src_id, dst_id, edge_type FROM edges")
            .map_err(map_sqlite_err)?;
        let mut rows = stmt.query([]).map_err(map_sqlite_err)?;
        let mut edges = Vec::new();
        while let Some(row) = rows.next().map_err(map_sqlite_err)? {
            let src_id: i64 = row.get(0).map_err(map_sqlite_err)?;
            let dst_id: i64 = row.get(1).map_err(map_sqlite_err)?;
            let code: u8 = row.get(2).map_err(map_sqlite_err)?;
            if let (Some(src), Some(dst)) = (nodes.get(&src_id), nodes.get(&dst_id)) {
                edges.push((src.clone(), dst.clone(), u8_to_edge(code)?));
            }
        }
        Ok(edges)
    }

    fn incoming_counts_by_target(
        &self,
        edge_types: &[EdgeType],
    ) -> Result<HashMap<String, usize>, StoreError> {
        let conn = self.lock_conn();
        let nodes = load_node_table(&conn)?;
        let mut counts = HashMap::new();
        if edge_types.is_empty() {
            let mut stmt = conn
                .prepare_cached("SELECT dst_id, COUNT(*) FROM edges GROUP BY dst_id")
                .map_err(map_sqlite_err)?;
            let mut rows = stmt.query([]).map_err(map_sqlite_err)?;
            while let Some(row) = rows.next().map_err(map_sqlite_err)? {
                let dst_id: i64 = row.get(0).map_err(map_sqlite_err)?;
                let count: i64 = row.get(1).map_err(map_sqlite_err)?;
                if let Some(target) = nodes.get(&dst_id)
                    && count > 0
                {
                    counts.insert(target.clone(), count as usize);
                }
            }
        } else {
            let requested: HashSet<u8> = edge_types.iter().copied().map(edge_to_u8).collect();
            let mut stmt = conn
                .prepare_cached(
                    "SELECT dst_id, edge_type, COUNT(*) FROM edges GROUP BY dst_id, edge_type",
                )
                .map_err(map_sqlite_err)?;
            let mut rows = stmt.query([]).map_err(map_sqlite_err)?;
            while let Some(row) = rows.next().map_err(map_sqlite_err)? {
                let dst_id: i64 = row.get(0).map_err(map_sqlite_err)?;
                let code: u8 = row.get(1).map_err(map_sqlite_err)?;
                let count: i64 = row.get(2).map_err(map_sqlite_err)?;
                if requested.contains(&code)
                    && count > 0
                    && let Some(target) = nodes.get(&dst_id)
                {
                    *counts.entry(target.clone()).or_insert(0) += count as usize;
                }
            }
        }
        Ok(counts)
    }

    fn edge_exists(&self, src: &str, dst: &str, edge_type: &EdgeType) -> Result<bool, StoreError> {
        let conn = self.lock_conn();
        let code = i64::from(edge_to_u8(*edge_type));
        let mut stmt = conn
            .prepare_cached(
                "SELECT EXISTS(\
                    SELECT 1 FROM edges e \
                    JOIN nodes src ON src.id = e.src_id \
                    JOIN nodes dst ON dst.id = e.dst_id \
                    WHERE src.anchor = ?1 AND dst.anchor = ?2 AND e.edge_type = ?3\
                )",
            )
            .map_err(map_sqlite_err)?;
        let exists: bool = stmt
            .query_row(params![src, dst, code], |r| r.get(0))
            .map_err(map_sqlite_err)?;
        Ok(exists)
    }

    fn get_all_anchors(&self) -> Result<HashSet<String>, StoreError> {
        let conn = self.lock_conn();
        let mut stmt = conn
            .prepare_cached("SELECT anchor FROM nodes WHERE has_doc = 1")
            .map_err(map_sqlite_err)?;
        let mut rows = stmt.query([]).map_err(map_sqlite_err)?;
        let mut anchors = HashSet::new();
        while let Some(row) = rows.next().map_err(map_sqlite_err)? {
            anchors.insert(row.get::<_, String>(0).map_err(map_sqlite_err)?);
        }
        Ok(anchors)
    }

    fn lookup_symbol_candidates(
        &self,
        symbol: &str,
    ) -> Result<Option<SymbolCandidateLookup>, StoreError> {
        let conn = self.lock_conn();
        let version: Option<String> = {
            let mut stmt = conn
                .prepare_cached("SELECT value FROM meta WHERE key = ?1")
                .map_err(map_sqlite_err)?;
            stmt.query_row([SYMBOL_LEXICON_META_KEY], |r| r.get(0))
                .optional()
                .map_err(map_sqlite_err)?
        };
        if version.as_deref() != Some(SYMBOL_LEXICON_VERSION) {
            return Ok(None);
        }

        let natural = aden_core::symbol::natural_symbol_form(symbol);
        let lower = natural.to_lowercase();
        let mut exact = HashSet::new();
        {
            let mut stmt = conn
                .prepare_cached(
                    "SELECT n.anchor FROM symbol_exact se \
                     JOIN nodes n ON n.id = se.node_id \
                     WHERE se.form = ?1",
                )
                .map_err(map_sqlite_err)?;
            for form in std::iter::once(natural.as_str())
                .chain((lower != natural).then_some(lower.as_str()))
            {
                let mut rows = stmt.query([form]).map_err(map_sqlite_err)?;
                while let Some(row) = rows.next().map_err(map_sqlite_err)? {
                    exact.insert(row.get::<_, String>(0).map_err(map_sqlite_err)?);
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
        let mut substring = SymbolSuggestions::default();
        let mut typo = SymbolSuggestions::default();
        let mut records_scanned = 0usize;
        let mut distance_evaluations = 0usize;

        let mut stmt = conn
            .prepare_cached(
                "SELECT anchor, segment_lower, leaf_lower, segment_len, leaf_len \
                 FROM nodes \
                 WHERE has_doc = 1 AND segment_lower IS NOT NULL \
                 ORDER BY anchor ASC",
            )
            .map_err(map_sqlite_err)?;
        let mut rows = stmt.query([]).map_err(map_sqlite_err)?;
        while let Some(row) = rows.next().map_err(map_sqlite_err)? {
            records_scanned += 1;
            let anchor: String = row.get(0).map_err(map_sqlite_err)?;
            let segment_lower: String = row.get(1).map_err(map_sqlite_err)?;
            let leaf_lower: String = row.get(2).map_err(map_sqlite_err)?;
            let segment_len: usize = row.get::<_, i64>(3).map_err(map_sqlite_err)?.max(0) as usize;
            let leaf_len: usize = row.get::<_, i64>(4).map_err(map_sqlite_err)?.max(0) as usize;

            if !lower.is_empty() && segment_lower.contains(&lower) {
                // A substring wins over every typo, including those in later records.
                max_distance = None;
                substring.insert(0, &anchor);
                continue;
            }
            let Some(limit) = max_distance else {
                continue;
            };
            let segment = (segment_len, segment_lower.as_str());
            let leaf = (leaf_len, leaf_lower.as_str());
            let distance = std::iter::once(segment)
                .chain((leaf != segment).then_some(leaf))
                .filter(|(len, _)| len.abs_diff(symbol_len) <= limit)
                .filter_map(|(_, candidate)| {
                    distance_evaluations += 1;
                    aden_core::symbol::edit_distance_within(&lower, candidate, limit)
                })
                .min();
            if let Some(distance) = distance {
                typo.insert(distance, &anchor);
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
        let conn = self.lock_conn();
        let mut stmt = conn
            .prepare_cached("DELETE FROM meta WHERE key = ?1")
            .map_err(map_sqlite_err)?;
        stmt.execute([SYMBOL_LEXICON_META_KEY])
            .map_err(map_sqlite_err)?;
        Ok(())
    }

    fn finalize_symbol_lexicon(&self) -> Result<(), StoreError> {
        self.put_meta(SYMBOL_LEXICON_META_KEY, SYMBOL_LEXICON_VERSION)
    }

    fn get_meta(&self, key: &str) -> Result<Option<String>, StoreError> {
        let conn = self.lock_conn();
        let mut stmt = conn
            .prepare_cached("SELECT value FROM meta WHERE key = ?1")
            .map_err(map_sqlite_err)?;
        stmt.query_row([key], |row| row.get(0))
            .optional()
            .map_err(map_sqlite_err)
    }

    fn put_meta(&self, key: &str, value: &str) -> Result<(), StoreError> {
        let conn = self.lock_conn();
        let mut stmt = conn
            .prepare_cached(
                "INSERT INTO meta (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )
            .map_err(map_sqlite_err)?;
        stmt.execute(params![key, value]).map_err(map_sqlite_err)?;
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
        let conn = self.lock_conn();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM nodes WHERE has_doc = 1", [], |r| {
                r.get(0)
            })
            .map_err(map_sqlite_err)?;
        Ok(count.max(0) as usize)
    }

    fn count_edges(&self) -> Result<usize, StoreError> {
        let conn = self.lock_conn();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
            .map_err(map_sqlite_err)?;
        Ok(count.max(0) as usize)
    }

    fn clear(&self) -> Result<(), StoreError> {
        let mut conn = self.lock_conn();
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_sqlite_err)?;
            tx.execute_batch(
                "DELETE FROM edges;\n\
                 DELETE FROM symbol_exact;\n\
                 DELETE FROM node_bodies;\n\
                 DELETE FROM bases;\n\
                 DELETE FROM meta;\n\
                 DELETE FROM nodes;\n\
                 DELETE FROM files;",
            )
            .map_err(map_sqlite_err)?;
            tx.commit().map_err(map_sqlite_err)?;
        }
        conn.execute_batch("PRAGMA incremental_vacuum;")
            .map_err(map_sqlite_err)?;
        Ok(())
    }

    fn flush(&self) -> Result<(), StoreError> {
        let conn = self.lock_conn();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA incremental_vacuum;")
            .map_err(map_sqlite_err)?;
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
            "aden-sqlite-store-test-{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
            name
        ))
    }

    fn symbol_doc(anchor: &str) -> Document {
        Document {
            anchor: anchor.to_string(),
            node_type: NodeType::Function,
            attributes: HashMap::new(),
            blocks: vec![],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        }
    }

    #[test]
    fn blob_compression_round_trips_small_and_large_payloads() {
        let small = b"hello world";
        let encoded_small = compress_blob(small);
        assert_eq!(encoded_small[0], BLOB_RAW_TAG);
        assert_eq!(decompress_blob(&encoded_small).unwrap(), small);

        let large = "fn repetitive_symbol() { let x = 42; }\n".repeat(40);
        let encoded_large = compress_blob(large.as_bytes());
        assert_eq!(encoded_large[0], BLOB_LZ4_TAG);
        assert!(encoded_large.len() < large.len());
        assert_eq!(decompress_blob(&encoded_large).unwrap(), large.as_bytes());
    }

    #[test]
    fn concurrent_readers_succeed_alongside_writer() {
        let path = test_store_path("concurrent-wal");
        let writer = SqliteStorage::new(path.to_str().unwrap()).unwrap();
        writer
            .put_document(&symbol_doc("aden://module/a.rs#alpha"))
            .unwrap();
        writer.flush().unwrap();

        let reader = SqliteStorage::open_existing(path.to_str().unwrap()).unwrap();
        assert!(
            reader
                .get_document("aden://module/a.rs#alpha")
                .unwrap()
                .is_some()
        );
        writer
            .put_document(&symbol_doc("aden://module/a.rs#beta"))
            .unwrap();
        assert!(
            reader
                .get_document("aden://module/a.rs#beta")
                .unwrap()
                .is_some()
        );
        drop(reader);
        drop(writer);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn legacy_fjall_directory_without_sqlite_marker_returns_incompatible() {
        let path = test_store_path("legacy-fjall");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("version"), b"FJL\x03").unwrap();
        assert!(matches!(
            SqliteStorage::new(path.to_str().unwrap()),
            Err(StoreError::IncompatibleVersion(_))
        ));
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn canonical_bulk_stores_zero_base_rows_and_preserves_cow_on_put_document() {
        let path = test_store_path("lazy-canonical-cow");
        let storage = SqliteStorage::new(path.to_str().unwrap()).unwrap();
        let anchor = "aden://module/example.rs#entry";
        let mut doc = symbol_doc(anchor);
        doc.attributes
            .insert("source_file".into(), "src/lib.rs".into());
        doc.attributes.insert("source_hash".into(), "v1".into());
        let expected_v1_base = emit_canonical_base(&doc);

        storage
            .put_documents_bulk_canonical(std::slice::from_ref(&doc))
            .unwrap();
        let base_rows: i64 = {
            let conn = storage.lock_conn();
            conn.query_row("SELECT COUNT(*) FROM bases", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(base_rows, 0, "canonical bulk must store zero rows in bases");
        assert_eq!(
            storage.get_base_snapshot(anchor).unwrap().as_deref(),
            Some(expected_v1_base.as_str())
        );

        storage.flush().unwrap();
        drop(storage);

        let reopened = SqliteStorage::open_existing(path.to_str().unwrap()).unwrap();
        assert_eq!(
            reopened.get_base_snapshot(anchor).unwrap().as_deref(),
            Some(expected_v1_base.as_str())
        );

        let mut modified = doc;
        modified
            .attributes
            .insert("source_hash".into(), "v2".into());
        reopened.put_document(&modified).unwrap();

        assert_eq!(reopened.get_document(anchor).unwrap(), Some(modified));
        assert_eq!(
            reopened.get_base_snapshot(anchor).unwrap().as_deref(),
            Some(expected_v1_base.as_str()),
            "put_document overwrite must CoW-materialize prior canonical base"
        );
        let base_rows_after_cow: i64 = {
            let conn = reopened.lock_conn();
            conn.query_row("SELECT COUNT(*) FROM bases", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(base_rows_after_cow, 1);

        drop(reopened);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn canonical_bulk_aborts_atomically_on_injected_failure() {
        let path = test_store_path("atomic-canonical");
        let storage = SqliteStorage::new(path.to_str().unwrap()).unwrap();
        let anchor = "aden://module/example.rs#entry";
        let mut old = symbol_doc(anchor);
        old.attributes.insert("signature".into(), "old".into());
        storage
            .put_documents_bulk_canonical(std::slice::from_ref(&old))
            .unwrap();
        let old_base = emit_canonical_base(&old);

        let mut fresh = symbol_doc(anchor);
        fresh.attributes.insert("signature".into(), "new".into());
        FAIL_BULK_BEFORE_COMMIT.with(|fail| fail.set(true));
        let err = storage.put_documents_bulk_canonical(&[fresh]).unwrap_err();
        assert!(err.to_string().contains("injected failure"));

        assert_eq!(storage.get_document(anchor).unwrap(), Some(old));
        assert_eq!(
            storage.get_base_snapshot(anchor).unwrap().as_deref(),
            Some(old_base.as_str())
        );
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn files_table_interning_sync_and_queries() {
        let path = test_store_path("files-table");
        let storage = SqliteStorage::new(path.to_str().unwrap()).unwrap();

        let mut first = symbol_doc("aden://module/lib.rs#first");
        first.source_span = Some(SourceSpan {
            file: "src/lib.rs".into(),
            start_line: 10,
            end_line: 15,
            start_byte: 100,
            end_byte: 150,
        });
        let mut second = symbol_doc("aden://module/lib.rs#second");
        second.source_span = Some(SourceSpan {
            file: "src/lib.rs".into(),
            start_line: 2,
            end_line: 6,
            start_byte: 10,
            end_byte: 60,
        });

        storage
            .put_documents_bulk_canonical(&[first, second])
            .unwrap();

        let file_count: i64 = {
            let conn = storage.lock_conn();
            conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(file_count, 1);
        assert_eq!(
            storage
                .get_source_file("aden://module/lib.rs#first")
                .unwrap()
                .as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            storage.get_nodes_in_file("src/lib.rs").unwrap(),
            vec![
                "aden://module/lib.rs#second".to_string(),
                "aden://module/lib.rs#first".to_string(),
            ]
        );

        storage
            .sync_file_records(&[
                FileRecord {
                    rel_path: "src/lib.rs".into(),
                    mtime: Some(12345),
                    digest: Some("deadbeef".into()),
                    disposition: Some("indexed".into()),
                },
                FileRecord {
                    rel_path: "ignored.txt".into(),
                    mtime: Some(99),
                    digest: None,
                    disposition: Some("Ignored".into()),
                },
            ])
            .unwrap();
        let synced_count: i64 = {
            let conn = storage.lock_conn();
            conn.query_row(
                "SELECT COUNT(*) FROM files WHERE disposition IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(synced_count, 2);

        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn clear_generated_calls_is_scoped_idempotent_and_persistent() {
        let path = test_store_path("clear-generated-calls");
        let storage = SqliteStorage::new(path.to_str().unwrap()).unwrap();
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

        let storage = SqliteStorage::open_existing(path.to_str().unwrap()).unwrap();
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
    fn repaired_document_batch_aborts_before_commit_and_recovers_coherently() {
        let path = test_store_path("atomic-repair");
        let storage = SqliteStorage::new(path.to_str().unwrap()).unwrap();
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
        storage.flush().unwrap();
        drop(storage);

        let storage = SqliteStorage::open_existing(path.to_str().unwrap()).unwrap();
        assert_eq!(
            storage.get_base_snapshot(anchor).unwrap().as_deref(),
            Some("old base")
        );
        assert_ne!(storage.get_document(anchor).unwrap(), Some(fresh.clone()));
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
                .lookup_symbol_candidates("entry")
                .unwrap()
                .unwrap()
                .anchors,
            vec![anchor.to_string()]
        );
        storage.flush().unwrap();
        drop(storage);

        let storage = SqliteStorage::open_existing(path.to_str().unwrap()).unwrap();
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
    fn bulk_edge_replay_is_idempotent() {
        let path = test_store_path("edge-replay");
        let storage = SqliteStorage::new(path.to_str().unwrap()).unwrap();
        let calls = EdgeType::Calls;
        let uses = EdgeType::Uses;
        let replay = vec![
            ("src".into(), "left".into(), calls),
            ("src".into(), "right".into(), uses),
            ("src".into(), "left".into(), calls),
        ];
        storage.put_edges_bulk(&replay).unwrap();
        storage.put_edges_bulk(&replay).unwrap();
        assert_eq!(storage.count_edges().unwrap(), 2);
        assert!(storage.edge_exists("src", "left", &calls).unwrap());
        assert_eq!(
            storage.get_incoming_edges("right").unwrap(),
            [("src".into(), uses)]
        );
        let mut out = storage.get_outgoing_edges("src").unwrap();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(out, [("left".into(), calls), ("right".into(), uses)]);
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn symbol_lexicon_uses_exact_keys_and_bounds_typo_candidates() {
        let path = test_store_path("symbol-lexicon");
        let storage = SqliteStorage::new(path.to_str().unwrap()).unwrap();
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
        let storage = SqliteStorage::new(path.to_str().unwrap()).unwrap();
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
        storage.finalize_symbol_lexicon().unwrap();

        let typo = storage.lookup_symbol_candidates("cafés").unwrap().unwrap();
        let expected: Vec<_> = close.into_iter().chain([qualified, far[0]]).collect();
        assert_eq!(typo.anchors, expected);
        assert!(!typo.exact_index_hit);
        assert_eq!(typo.records_scanned, 12);
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

        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn symbol_lexicon_indexes_php_backslash_shorthand() {
        let path = test_store_path("php-symbol-lexicon");
        let storage = SqliteStorage::new(path.to_str().unwrap()).unwrap();
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
        let storage = SqliteStorage::new(path.to_str().unwrap()).unwrap();
        let anchor = "aden://module/example.rs#entry";
        let doc = Document {
            anchor: anchor.to_string(),
            node_type: NodeType::Function,
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
        {
            let mut conn = storage.lock_conn();
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let id = intern_node(&tx, anchor).unwrap();
            tx.execute("UPDATE nodes SET has_doc = 1 WHERE id = ?1", [id])
                .unwrap();
            let blob = compress_blob(&serialize_document(&doc).unwrap());
            tx.execute(
                "INSERT INTO node_bodies (node_id, doc_blob) VALUES (?1, ?2)",
                params![id, blob],
            )
            .unwrap();
            tx.commit().unwrap();
        }

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
        let storage = SqliteStorage::new(path.to_str().unwrap()).unwrap();
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
        {
            let conn = storage.lock_conn();
            conn.execute(
                "UPDATE nodes SET file_id = NULL, start_line = NULL, end_line = NULL, \
                 start_byte = NULL, end_byte = NULL WHERE anchor = ?1",
                [missing_projection.anchor.as_str()],
            )
            .unwrap();
        }

        assert_eq!(
            storage
                .get_source_spans_for([present.anchor.as_str(), missing_projection.anchor.as_str()])
                .unwrap(),
            vec![(present.anchor, present.source_span.unwrap())]
        );
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn clear_removes_base_snapshots() {
        let path = test_store_path("clear-bases");
        let storage = SqliteStorage::new(path.to_str().unwrap()).unwrap();
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

    #[test]
    fn symbol_exact_delete_uses_node_id_index_and_upgrades_legacy_v1_indexes() {
        let path = test_store_path("symbol-exact-index");
        let storage = SqliteStorage::new(path.to_str().unwrap()).unwrap();
        storage
            .put_documents_bulk_canonical(&[symbol_doc("aden://module/example.rs#alpha")])
            .unwrap();

        {
            let conn = storage.lock_conn();
            let mut stmt = conn
                .prepare("EXPLAIN QUERY PLAN DELETE FROM symbol_exact WHERE node_id = ?1")
                .unwrap();
            let details: Vec<String> = stmt
                .query_map([1_i64], |row| row.get::<_, String>(3))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            let joined = details.join(" | ");
            assert!(
                joined.contains("idx_symbol_exact_node"),
                "expected DELETE plan to use idx_symbol_exact_node, got: {joined}"
            );
            assert!(
                !joined.to_ascii_uppercase().contains("SCAN"),
                "unexpected full-table SCAN in DELETE plan: {joined}"
            );

            conn.execute_batch("DROP INDEX idx_symbol_exact_node;")
                .unwrap();
        }
        storage.flush().unwrap();
        drop(storage);

        let ro = SqliteStorage::open_existing(path.to_str().unwrap()).unwrap();
        {
            let conn = ro.lock_conn();
            let has_idx: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = 'idx_symbol_exact_node')",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(
                !has_idx,
                "open_existing must not mutate schema or take DDL locks on read open"
            );
        }
        drop(ro);

        let rw = SqliteStorage::new(path.to_str().unwrap()).unwrap();
        {
            let conn = rw.lock_conn();
            let has_idx: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = 'idx_symbol_exact_node')",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(has_idx, "writer open must restore idx_symbol_exact_node");
        }
        drop(rw);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn canonical_bulk_duplicate_anchor_updates_in_place_without_stale_symbols() {
        let path = test_store_path("dup-canonical-bulk");
        let storage = SqliteStorage::new(path.to_str().unwrap()).unwrap();
        let anchor = "aden://module/example.rs#shared_entry";

        let mut first = symbol_doc(anchor);
        first.source_span = Some(SourceSpan {
            file: "src/old.rs".into(),
            start_line: 1,
            end_line: 3,
            start_byte: 0,
            end_byte: 25,
        });
        first.attributes.insert("source_hash".into(), "h1".into());

        let mut second = symbol_doc(anchor);
        second.source_span = Some(SourceSpan {
            file: "src/new.rs".into(),
            start_line: 10,
            end_line: 14,
            start_byte: 100,
            end_byte: 180,
        });
        second.attributes.insert("source_hash".into(), "h2".into());

        storage
            .put_documents_bulk_canonical(&[first, second.clone()])
            .unwrap();
        storage.finalize_symbol_lexicon().unwrap();

        assert_eq!(storage.get_document(anchor).unwrap(), Some(second.clone()));
        assert_eq!(
            storage.get_source_spans().unwrap(),
            vec![(anchor.to_string(), second.source_span.clone().unwrap())]
        );
        assert_eq!(
            storage.get_nodes_in_file("src/old.rs").unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            storage.get_nodes_in_file("src/new.rs").unwrap(),
            vec![anchor.to_string()]
        );

        let exact = storage
            .lookup_symbol_candidates("shared_entry")
            .unwrap()
            .unwrap();
        assert!(exact.exact_index_hit);
        assert_eq!(exact.anchors, vec![anchor.to_string()]);

        {
            let conn = storage.lock_conn();
            let bases_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM bases", [], |r| r.get(0))
                .unwrap();
            let file_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
                .unwrap();
            let exact_rows: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM symbol_exact WHERE node_id = (SELECT id FROM nodes WHERE anchor = ?1)",
                    [anchor],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(bases_count, 0);
            assert_eq!(file_count, 1, "superseded src/old.rs row must be pruned");
            assert_eq!(exact_rows, 1);
        }

        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn visit_documents_ordered_matches_all_documents_and_propagates_errors() {
        let path = test_store_path("visit-ordered");
        let storage = SqliteStorage::new(path.to_str().unwrap()).unwrap();

        let anchors = [
            "aden://module/z.rs#zeta",
            "aden://module/a.rs#alpha",
            "aden://module/m.rs#mu",
        ];
        let mut docs = Vec::new();
        for anchor in anchors {
            let mut doc = symbol_doc(anchor);
            doc.attributes.insert("tag".into(), anchor.into());
            docs.push(doc);
        }
        storage.put_documents_bulk_canonical(&docs).unwrap();
        // Also insert an edge-only node (has_doc = 0) to confirm it is excluded.
        storage
            .put_edge(
                "aden://module/a.rs#alpha",
                "unresolved_ext",
                EdgeType::Calls,
            )
            .unwrap();

        let expected = storage.get_all_documents().unwrap();
        let mut reported_count = None;
        let mut visited = Vec::new();
        storage
            .visit_documents_ordered(
                &mut |count| {
                    reported_count = Some(count);
                    Ok(())
                },
                &mut |anchor, doc| {
                    visited.push((anchor, doc));
                    Ok(())
                },
            )
            .unwrap();

        assert_eq!(reported_count, Some(3));
        let visited_anchors: Vec<&str> = visited.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(
            visited_anchors,
            vec![
                "aden://module/a.rs#alpha",
                "aden://module/m.rs#mu",
                "aden://module/z.rs#zeta",
            ]
        );
        for (anchor, doc) in &visited {
            assert_eq!(expected.get(anchor), Some(doc));
        }

        let err = storage
            .visit_documents_ordered(&mut |_| Ok(()), &mut |anchor, _| {
                if anchor.ends_with("#mu") {
                    Err(StoreError::Serialization("stop-here".into()))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(err.to_string().contains("stop-here"));

        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }
}
