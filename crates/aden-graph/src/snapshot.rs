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
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
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

#[derive(serde::Deserialize)]
struct SnapshotPayload {
    docs: HashMap<String, Document>,
    edges: Vec<(String, String, EdgeType)>,
}

#[derive(serde::Serialize)]
struct SnapshotPayloadRef<'a> {
    docs: &'a HashMap<String, Document>,
    edges: &'a [(String, String, EdgeType)],
}

/// Serialize docs + edges into the ADR-011 v1 wire format without cloning the
/// complete graph. At kernel scale, cloning here doubled the largest live data
/// structures immediately before allocating the encoded snapshot bytes.
pub fn encode_snapshot(
    docs: &HashMap<String, Document>,
    edges: &[(String, String, EdgeType)],
) -> Result<Vec<u8>, SnapshotError> {
    let payload = SnapshotPayloadRef { docs, edges };
    let body = postcard::to_allocvec(&payload).map_err(|e| SnapshotError::Encode(e.to_string()))?;
    let mut out = Vec::with_capacity(MAGIC.len() + 4 + body.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&SNAPSHOT_VERSION.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

const HEADER_LEN: usize = MAGIC.len() + 4;
const INITIAL_DECODE_SCRATCH: usize = 64 * 1024;

fn validate_header(header: &[u8]) -> Result<(), SnapshotError> {
    if header.len() < HEADER_LEN {
        return Err(SnapshotError::Incompatible("truncated header".into()));
    }
    if &header[..MAGIC.len()] != MAGIC {
        return Err(SnapshotError::Incompatible("bad magic".into()));
    }
    let version = u32::from_le_bytes(header[8..12].try_into().unwrap());
    if version != SNAPSHOT_VERSION {
        return Err(SnapshotError::Incompatible(format!(
            "unsupported snapshot version {version} (expected {SNAPSHOT_VERSION})"
        )));
    }
    Ok(())
}

/// Decode snapshot bytes already held by a caller. Returns `(docs, edges)`.
pub fn decode_snapshot(bytes: &[u8]) -> Result<SnapshotData, SnapshotError> {
    validate_header(bytes)?;
    let payload: SnapshotPayload = postcard::from_bytes(&bytes[HEADER_LEN..])
        .map_err(|e| SnapshotError::Decode(e.to_string()))?;
    Ok((payload.docs, payload.edges))
}

fn decode_snapshot_reader<R: Read + Seek>(
    mut reader: R,
    file_len: u64,
    initial_scratch: usize,
) -> Result<SnapshotData, SnapshotError> {
    let mut header = [0_u8; HEADER_LEN];
    match reader.read_exact(&mut header) {
        Ok(()) => validate_header(&header)?,
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            return Err(SnapshotError::Incompatible("truncated header".into()));
        }
        Err(error) => return Err(SnapshotError::Io(error)),
    }

    let payload_len = file_len.saturating_sub(HEADER_LEN as u64);
    let payload_len: usize = payload_len.try_into().map_err(|_| {
        SnapshotError::Incompatible("snapshot is too large for this platform".into())
    })?;
    let mut scratch_len = payload_len.clamp(1, initial_scratch.max(1));

    loop {
        reader.seek(SeekFrom::Start(HEADER_LEN as u64))?;
        let mut scratch = vec![0_u8; scratch_len];
        match postcard::from_io::<SnapshotPayload, _>((&mut reader, &mut scratch)) {
            Ok((payload, _)) => return Ok((payload.docs, payload.edges)),
            Err(postcard::Error::DeserializeUnexpectedEnd) if scratch_len < payload_len => {
                // Postcard's IO flavor uses caller scratch for temporary string/
                // byte borrows. Owned fields reuse it, so normal snapshots stay
                // at 64 KiB; a single larger field retries with bounded growth.
                scratch_len = scratch_len.saturating_mul(2).min(payload_len);
            }
            Err(error) => return Err(SnapshotError::Decode(error.to_string())),
        }
    }
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

fn temporary_path(dest: &Path) -> PathBuf {
    dest.with_extension("snapshot.tmp")
}

fn write_snapshot_to_temp(
    dest: &Path,
    docs: &HashMap<String, Document>,
    edges: &[(String, String, EdgeType)],
) -> Result<(), SnapshotError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(temporary_path(dest))?;
    let mut writer = BufWriter::new(file);
    writer.write_all(MAGIC)?;
    writer.write_all(&SNAPSHOT_VERSION.to_le_bytes())?;
    let payload = SnapshotPayloadRef { docs, edges };
    let mut writer =
        postcard::to_io(&payload, writer).map_err(|e| SnapshotError::Encode(e.to_string()))?;
    writer.flush()?;
    Ok(())
}

/// Serialize a snapshot into `dest.tmp` without retaining its encoded bytes.
///
/// Call [`publish_prepared`] after the storage handle is dropped so a final
/// fjall flush cannot make the on-disk store newer than the snapshot file.
pub fn prepare_from_storage<S: GraphStorage>(
    dest: &Path,
    storage: &S,
) -> Result<(), SnapshotError> {
    let (docs, edges) = GraphBridge::load_from_storage(storage)?;
    write_snapshot_to_temp(dest, &docs, &edges)
}

/// Atomically rename a successfully prepared snapshot into place.
pub fn publish_prepared(dest: &Path) -> Result<(), SnapshotError> {
    let tmp = temporary_path(dest);
    // Preparing happens while Fjall is still open; refresh the temporary file's
    // mtime after that handle has dropped so the renamed snapshot covers its
    // final flush.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&tmp)?
        .set_modified(SystemTime::now())?;
    std::fs::rename(tmp, dest)?;
    Ok(())
}

/// Load docs + edges from storage and publish to `dest`.
pub fn publish_from_storage<S: GraphStorage>(
    dest: &Path,
    storage: &S,
) -> Result<(), SnapshotError> {
    prepare_from_storage(dest, storage)?;
    publish_prepared(dest)
}

/// Read a snapshot file without retaining its complete encoded byte stream.
pub fn read_snapshot_file(path: &Path) -> Result<SnapshotData, SnapshotError> {
    let file = std::fs::File::open(path)?;
    let file_len = file.metadata()?.len();
    decode_snapshot_reader(BufReader::new(file), file_len, INITIAL_DECODE_SCRATCH)
}

/// Load docs + edges from the last published read snapshot when present.
///
/// **Zero-friction policy (ADR-011 amend):** prefer a non-empty snapshot even
/// when the live store is newer (interrupted gen / concurrent writer). Serving
/// the last consistent snapshot avoids fjall `Locked` under multi-agent load.
/// Callers that need store-exact data open fjall after this returns `None`.
///
/// Returns `None` when the snapshot is missing or empty.
pub fn try_read_fresh(root: &Path) -> Option<SnapshotData> {
    let snapshot_path = aden_paths::graph_snapshot_file(root);
    if !snapshot_path.is_file() {
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
    fn streamed_snapshot_matches_v1_wire_format() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("graph.snapshot");
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

        let expected = encode_snapshot(&docs, &edges).unwrap();
        write_snapshot_to_temp(&dest, &docs, &edges).unwrap();
        assert_eq!(std::fs::read(temporary_path(&dest)).unwrap(), expected);
        publish_prepared(&dest).unwrap();
        assert_eq!(read_snapshot_file(&dest).unwrap().1, edges);
    }

    #[test]
    fn streaming_reader_grows_scratch_for_one_large_owned_field() {
        let mut attributes = HashMap::new();
        attributes.insert("body".into(), "x".repeat(200_000));
        let mut docs = HashMap::new();
        docs.insert(
            "aden://module/foo.adoc#large".into(),
            Document {
                anchor: "aden://module/foo.adoc#large".into(),
                node_type: NodeType::Function,
                attributes,
                blocks: vec![],
                source_span: None,
                metadata: None,
                confidence: 1.0,
            },
        );
        let bytes = encode_snapshot(&docs, &[]).unwrap();
        let (decoded, _) =
            decode_snapshot_reader(std::io::Cursor::new(&bytes), bytes.len() as u64, 8).unwrap();
        assert_eq!(
            decoded["aden://module/foo.adoc#large"].attributes["body"].len(),
            200_000
        );
    }

    #[test]
    fn streaming_reader_preserves_header_and_corruption_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.snapshot");

        std::fs::write(&path, b"ADENSNAP").unwrap();
        assert!(matches!(
            read_snapshot_file(&path),
            Err(SnapshotError::Incompatible(message)) if message == "truncated header"
        ));

        let mut bad_magic = [0_u8; HEADER_LEN];
        bad_magic[8..12].copy_from_slice(&SNAPSHOT_VERSION.to_le_bytes());
        std::fs::write(&path, bad_magic).unwrap();
        assert!(matches!(
            read_snapshot_file(&path),
            Err(SnapshotError::Incompatible(message)) if message == "bad magic"
        ));

        let mut unsupported = Vec::from(MAGIC.as_slice());
        unsupported.extend_from_slice(&(SNAPSHOT_VERSION + 1).to_le_bytes());
        std::fs::write(&path, unsupported).unwrap();
        assert!(matches!(
            read_snapshot_file(&path),
            Err(SnapshotError::Incompatible(message)) if message.contains("unsupported snapshot version")
        ));

        let mut valid = encode_snapshot(&HashMap::new(), &[]).unwrap();
        valid.pop();
        std::fs::write(&path, valid).unwrap();
        assert!(matches!(
            read_snapshot_file(&path),
            Err(SnapshotError::Decode(_))
        ));
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
