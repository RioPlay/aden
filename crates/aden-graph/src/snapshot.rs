// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Read snapshot for concurrent graph loads (ADR-011).
//!
//! `gen` publishes an atomic `graph.snapshot` after linking edges. Read commands
//! load from the snapshot when it is fresher than the live fjall store, avoiding
//! fjall's process-level `Locked` errors under multi-agent workloads.

use crate::bridge::GraphBridge;
use aden_core::{Document, EdgeType};
use aden_store::{GraphStorage, StoreError};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// On-disk format version. Bump when the byte layout changes.
pub const SNAPSHOT_VERSION: u32 = 1;

const MAGIC: &[u8; 8] = b"ADENSNAP";

/// Docs + edges tuple returned by snapshot readers. Named type to keep
/// signatures short for clippy.
pub type SnapshotData = (HashMap<String, Document>, Vec<(String, String, EdgeType)>);

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("snapshot I/O: {0}")]
    Io(#[from] io::Error),
    #[error("snapshot encode: {0}")]
    Encode(String),
    #[error("snapshot decode: {0}")]
    Decode(String),
    #[error("snapshot incompatible: {0}")]
    Incompatible(String),
    #[error("store error: {0}")]
    Store(#[from] StoreError),
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SnapshotPayload {
    docs: HashMap<String, Document>,
    edges: Vec<(String, String, EdgeType)>,
}

/// Serialize docs + edges into the ADR-011 v1 wire format.
pub fn encode_snapshot(
    docs: &HashMap<String, Document>,
    edges: &[(String, String, EdgeType)],
) -> Result<Vec<u8>, SnapshotError> {
    let payload = SnapshotPayload {
        docs: docs.clone(),
        edges: edges.to_vec(),
    };
    let body = postcard::to_allocvec(&payload).map_err(|e| SnapshotError::Encode(e.to_string()))?;
    let mut out = Vec::with_capacity(MAGIC.len() + 4 + body.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&SNAPSHOT_VERSION.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Decode a snapshot file. Returns `(docs, edges)`.
pub fn decode_snapshot(bytes: &[u8]) -> Result<SnapshotData, SnapshotError> {
    if bytes.len() < MAGIC.len() + 4 {
        return Err(SnapshotError::Incompatible("truncated header".into()));
    }
    if &bytes[..MAGIC.len()] != MAGIC {
        return Err(SnapshotError::Incompatible("bad magic".into()));
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    if version != SNAPSHOT_VERSION {
        return Err(SnapshotError::Incompatible(format!(
            "unsupported snapshot version {version} (expected {SNAPSHOT_VERSION})"
        )));
    }
    let payload: SnapshotPayload =
        postcard::from_bytes(&bytes[12..]).map_err(|e| SnapshotError::Decode(e.to_string()))?;
    Ok((payload.docs, payload.edges))
}

/// True when `snapshot` exists and is at least as new as every file under `store`.
///
/// If `gen` updated fjall but crashed before publishing, the store tree is newer
/// than the snapshot and readers must fall back to fjall.
pub fn snapshot_covers_store(snapshot: &Path, store: &Path) -> bool {
    let Ok(snap_meta) = snapshot.metadata() else {
        return false;
    };
    let Ok(snap_mtime) = snap_meta.modified() else {
        return false;
    };
    let Some(store_mtime) = newest_mtime_under(store) else {
        return false;
    };
    snap_mtime >= store_mtime
}

fn newest_mtime_under(dir: &Path) -> Option<SystemTime> {
    let meta = dir.metadata().ok()?;
    let mut newest = meta.modified().ok()?;
    if meta.is_dir() {
        let read = std::fs::read_dir(dir).ok()?;
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(child) = newest_mtime_under(&path)
                    && child > newest
                {
                    newest = child;
                }
            } else if let Ok(m) = entry.metadata().and_then(|m| m.modified())
                && m > newest
            {
                newest = m;
            }
        }
    }
    Some(newest)
}

/// Atomically publish snapshot bytes to `dest` (`dest.tmp` + rename).
pub fn publish_bytes(dest: &Path, bytes: &[u8]) -> Result<(), SnapshotError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp: PathBuf = dest.with_extension("snapshot.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// Encode a snapshot from live storage without writing it yet.
///
/// Call [`publish_bytes`] after the storage handle is dropped so a final fjall
/// flush cannot make the on-disk store newer than the snapshot file.
pub fn prepare_from_storage<S: GraphStorage>(storage: &S) -> Result<Vec<u8>, SnapshotError> {
    let (docs, edges) = GraphBridge::load_from_storage(storage)?;
    encode_snapshot(&docs, &edges)
}

/// Load docs + edges from storage and publish to `dest`.
pub fn publish_from_storage<S: GraphStorage>(
    dest: &Path,
    storage: &S,
) -> Result<(), SnapshotError> {
    let bytes = prepare_from_storage(storage)?;
    publish_bytes(dest, &bytes)
}

/// Read a snapshot file from disk.
pub fn read_snapshot_file(path: &Path) -> Result<SnapshotData, SnapshotError> {
    let bytes = std::fs::read(path)?;
    decode_snapshot(&bytes)
}

/// Load docs + edges from a fresh read snapshot when one covers the live store.
///
/// Returns `None` when the snapshot is missing, stale, or empty — callers fall
/// back to opening fjall.
pub fn try_read_fresh(root: &Path) -> Option<SnapshotData> {
    let (store_path, _) = aden_paths::resolve_read_store(root);
    let snapshot_path = aden_paths::graph_snapshot_file(root);
    if !snapshot_path.is_file() || !store_path.exists() {
        return None;
    }
    if !snapshot_covers_store(&snapshot_path, &store_path) {
        return None;
    }
    let data = read_snapshot_file(&snapshot_path).ok()?;
    if data.0.is_empty() {
        return None;
    }
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aden_core::{Document, NodeType};

    #[test]
    fn round_trip_snapshot_v1() {
        let mut docs = HashMap::new();
        docs.insert(
            "aden://module/foo.adoc#bar".into(),
            Document {
                anchor: "aden://module/foo.adoc#bar".into(),
                node_type: NodeType::Function,
                attributes: HashMap::new(),
                blocks: vec![],
                source_span: None,
                metadata: None,
                confidence: 1.0,
            },
        );
        let edges = vec![("a".into(), "b".into(), EdgeType::Calls)];
        let bytes = encode_snapshot(&docs, &edges).unwrap();
        let (docs2, edges2) = decode_snapshot(&bytes).unwrap();
        assert_eq!(docs2.len(), 1);
        assert_eq!(edges2.len(), 1);
        assert_eq!(edges2[0].2, EdgeType::Calls);
    }

    #[test]
    fn freshness_gate_prefers_newer_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        let snap = dir.path().join("graph.snapshot");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("0.jnl"), b"data").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&snap, b"ADENSNAP").unwrap();
        assert!(snapshot_covers_store(&snap, &store));

        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(store.join("1.jnl"), b"newer").unwrap();
        assert!(
            !snapshot_covers_store(&snap, &store),
            "stale snapshot must not cover a newer store"
        );
    }
}
