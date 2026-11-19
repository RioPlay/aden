// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! A single decoded snapshot retained only by an explicitly enabled read worker.
//! File byte identity supplements, and never replaces, command freshness checks.

use super::{INITIAL_DECODE_SCRATCH, SnapshotData, decode_snapshot_reader};
use crate::cache::{
    AnchorResolution, AnchorResolutionAnalysis, analyze_anchor_list, resolve_anchor_from_list,
    symbols::PreparedSymbols,
};
use aden_core::EdgeType;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

// Eligibility limits, not a claim about decoded heap size. Only one snapshot is
// retained. Larger projects keep the existing selective store/snapshot path.
pub(super) const MAX_ENCODED_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RECORDS: usize = 100_000;
type Adjacency = HashMap<String, Vec<(String, EdgeType)>>;

pub(crate) struct SharedSnapshot {
    pub data: SnapshotData,
    anchors: OnceLock<Vec<String>>,
    incoming: OnceLock<Adjacency>,
    outgoing: OnceLock<Adjacency>,
    symbols: OnceLock<PreparedSymbols>,
}

impl SharedSnapshot {
    pub(crate) fn new(data: SnapshotData) -> Self {
        Self {
            data,
            anchors: OnceLock::new(),
            incoming: OnceLock::new(),
            outgoing: OnceLock::new(),
            symbols: OnceLock::new(),
        }
    }

    pub fn anchors(&self) -> &[String] {
        self.anchors
            .get_or_init(|| self.data.0.keys().cloned().collect())
    }

    pub fn analyze_anchor(&self, symbol: &str) -> AnchorResolutionAnalysis {
        self.analyze_anchor_using(
            symbol,
            std::env::var("ADEN_SYMBOL_LOOKUP_CACHE").as_deref() != Ok("0"),
        )
    }

    fn analyze_anchor_using(&self, symbol: &str, prepared: bool) -> AnchorResolutionAnalysis {
        if prepared {
            self.symbols
                .get_or_init(|| PreparedSymbols::new(self.anchors()))
                .analyze(symbol, self.anchors(), |anchor| {
                    self.data.0.contains_key(anchor)
                })
        } else {
            analyze_anchor_list(symbol, self.anchors())
        }
    }

    pub fn resolve_anchor(&self, symbol: &str) -> AnchorResolution {
        self.resolve_anchor_using(
            symbol,
            std::env::var("ADEN_SYMBOL_LOOKUP_CACHE").as_deref() != Ok("0"),
        )
    }

    fn resolve_anchor_using(&self, symbol: &str, prepared: bool) -> AnchorResolution {
        if prepared {
            if let Some(anchor) =
                PreparedSymbols::exact_anchor(symbol, |anchor| self.data.0.contains_key(anchor))
            {
                return AnchorResolution::Exact(anchor);
            }
            self.symbols
                .get_or_init(|| PreparedSymbols::new(self.anchors()))
                .resolve(symbol, self.anchors(), |anchor| {
                    self.data.0.contains_key(anchor)
                })
        } else {
            resolve_anchor_from_list(symbol, self.anchors())
        }
    }

    pub fn adjacency(&self, direction: crate::Direction) -> &Adjacency {
        let cell = match direction {
            crate::Direction::Incoming => &self.incoming,
            crate::Direction::Outgoing => &self.outgoing,
        };
        cell.get_or_init(|| {
            let mut result: Adjacency = HashMap::new();
            for (src, dst, edge) in &self.data.1 {
                let (key, neighbor) = match direction {
                    crate::Direction::Incoming => (dst, src),
                    crate::Direction::Outgoing => (src, dst),
                };
                result
                    .entry(key.clone())
                    .or_default()
                    .push((neighbor.clone(), *edge));
            }
            result
        })
    }
}

struct Entry {
    root: PathBuf,
    path: PathBuf,
    digest: [u8; 32],
    snapshot: Arc<SharedSnapshot>,
}

#[derive(Default)]
struct SessionCache {
    entry: Option<Entry>,
}

thread_local! {
    static CACHE: RefCell<Option<SessionCache>> = const { RefCell::new(None) };
}

/// Enable one thread-local read-worker cache. Ordinary CLI commands remain
/// unchanged. Disabling immediately releases retained documents and indexes.
pub fn set_session_cache_enabled(enabled: bool) {
    CACHE.with(|cache| *cache.borrow_mut() = enabled.then(SessionCache::default));
}

pub fn session_cache_enabled() -> bool {
    CACHE.with(|cache| cache.borrow().is_some())
}

pub(crate) fn try_read_session(root: &Path) -> Option<Arc<SharedSnapshot>> {
    CACHE.with(|cache| cache.borrow_mut().as_mut()?.load(root))
}

fn digest_file(file: &mut File) -> io::Result<[u8; 32]> {
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_ENCODED_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot exceeds cache eligibility",
            ));
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize().into())
}

impl SessionCache {
    fn load(&mut self, root: &Path) -> Option<Arc<SharedSnapshot>> {
        self.load_file(root, &aden_paths::graph_snapshot_file(root))
    }

    fn load_file(&mut self, root: &Path, path: &Path) -> Option<Arc<SharedSnapshot>> {
        let result = self.load_checked(root, path);
        if result.is_none() {
            self.entry = None;
        }
        result
    }

    fn load_checked(&mut self, root: &Path, path: &Path) -> Option<Arc<SharedSnapshot>> {
        let root = root.canonicalize().ok()?;
        let path = path.canonicalize().ok()?;
        let mut file = File::open(&path).ok()?;
        let len = file.metadata().ok()?.len();
        if len > MAX_ENCODED_BYTES {
            return None;
        }
        let digest = digest_file(&mut file).ok()?;
        if let Some(entry) = &self.entry
            && entry.root == root
            && entry.path == path
            && entry.digest == digest
        {
            return Some(Arc::clone(&entry.snapshot));
        }
        // Do not retain an old publication while decoding its replacement.
        self.entry = None;
        file.seek(SeekFrom::Start(0)).ok()?;
        let data =
            decode_snapshot_reader(BufReader::new(&mut file), len, INITIAL_DECODE_SCRATCH).ok()?;
        if data.0.is_empty() || data.0.len().saturating_add(data.1.len()) > MAX_RECORDS {
            return None;
        }
        // Atomic publishers leave this handle stable. Also reject in-place
        // rewrites during decoding rather than retaining mixed evidence.
        if digest_file(&mut file).ok()? != digest {
            return None;
        }
        let snapshot = Arc::new(SharedSnapshot::new(data));
        self.entry = Some(Entry {
            root,
            path,
            digest,
            snapshot: Arc::clone(&snapshot),
        });
        Some(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aden_core::{Document, NodeType};

    fn data(name: &str) -> SnapshotData {
        let doc = Document {
            anchor: name.into(),
            node_type: NodeType::Function,
            attributes: HashMap::new(),
            blocks: vec![],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };
        (
            HashMap::from([(name.into(), doc)]),
            vec![(name.into(), name.into(), EdgeType::Calls)],
        )
    }

    // Keep fixtures inside temporary directories, without environment mutation.
    fn root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn publish(root: &Path, name: &str) -> PathBuf {
        let path = root.join("graph.snapshot");
        let (docs, edges) = data(name);
        super::super::publish_bytes(
            &path,
            &super::super::encode_snapshot(&docs, &edges).unwrap(),
        )
        .unwrap();
        path
    }

    #[test]
    fn reuses_identical_bytes_and_indexes_but_replaces_changed_publication() {
        let root = root();
        let path = publish(root.path(), "before");
        let mut cache = SessionCache::default();
        let first = cache.load_file(root.path(), &path).unwrap();
        let anchors = first.anchors().as_ptr();
        let incoming = first.adjacency(crate::Direction::Incoming) as *const _;
        first.analyze_anchor_using("before", true);
        let symbols = first.symbols.get().unwrap() as *const _;
        let second = cache.load_file(root.path(), &path).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(anchors, second.anchors().as_ptr());
        assert_eq!(symbols, second.symbols.get().unwrap() as *const _);
        assert_eq!(
            incoming,
            second.adjacency(crate::Direction::Incoming) as *const _
        );
        let old_time = path.metadata().unwrap().modified().unwrap();
        publish(root.path(), "afterr");
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old_time))
            .unwrap();
        let changed = cache.load_file(root.path(), &path).unwrap();
        assert!(!Arc::ptr_eq(&first, &changed));
        assert!(changed.symbols.get().is_none());
        assert_eq!(
            changed.resolve_anchor_using("afterr", true),
            AnchorResolution::Exact("afterr".into())
        );
        assert!(matches!(
            changed.resolve_anchor_using("before", true),
            AnchorResolution::NotFound { .. }
        ));
        assert_eq!(
            first.resolve_anchor_using("before", true),
            AnchorResolution::Exact("before".into())
        );
        assert!(changed.data.0.contains_key("afterr"));
        assert!(!changed.data.0.contains_key("before"));
        assert!(first.data.0.contains_key("before")); // pinned requests stay immutable
    }

    #[test]
    fn symbol_lookup_switch_preserves_reference_and_only_prepares_enabled_natural_reads() {
        let anchor = "aden://module/demo/lib.rs#Handler<T>::handle";
        let snapshot = SharedSnapshot::new(data(anchor));
        let expected = analyze_anchor_list("handle", snapshot.anchors());
        assert!(snapshot.symbols.get().is_none());
        assert_eq!(snapshot.analyze_anchor_using("handle", false), expected);
        assert_eq!(
            snapshot.resolve_anchor_using("handle", false),
            expected.resolution
        );
        assert!(
            snapshot.symbols.get().is_none(),
            "diagnostic-off reads initialized symbol tables"
        );
        assert_eq!(
            snapshot.resolve_anchor_using(anchor, true),
            AnchorResolution::Exact(anchor.into())
        );
        assert!(
            snapshot.symbols.get().is_none(),
            "canonical exact lookup should use existing document keys"
        );
        assert_eq!(
            snapshot.resolve_anchor_using("demo/lib.rs#Handler<T>::handle", true),
            AnchorResolution::Exact(anchor.into())
        );
        assert!(
            snapshot.symbols.get().is_none(),
            "canonical alias lookup should use existing document keys"
        );
        assert_eq!(snapshot.analyze_anchor_using("handle", true), expected);
        let prepared = snapshot.symbols.get().unwrap() as *const _;
        assert_eq!(
            snapshot.resolve_anchor_using("handle", true),
            expected.resolution
        );
        assert_eq!(
            snapshot.analyze_anchor_using("Handler", true),
            analyze_anchor_list("Handler", snapshot.anchors())
        );
        assert_eq!(prepared, snapshot.symbols.get().unwrap() as *const _);
        assert_eq!(snapshot.analyze_anchor_using("handle", false), expected);
        assert_eq!(prepared, snapshot.symbols.get().unwrap() as *const _);
    }

    #[test]
    fn missing_corrupt_and_oversized_files_evict_retained_state() {
        let root = root();
        let path = publish(root.path(), "present");
        let mut cache = SessionCache::default();
        cache.load_file(root.path(), &path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(cache.load_file(root.path(), &path).is_none());
        assert!(cache.entry.is_none());
        publish(root.path(), "present");
        cache.load_file(root.path(), &path).unwrap();
        std::fs::write(&path, b"broken").unwrap();
        assert!(cache.load_file(root.path(), &path).is_none());
        assert!(cache.entry.is_none());
        publish(root.path(), "present");
        cache.load_file(root.path(), &path).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_ENCODED_BYTES + 1)
            .unwrap();
        assert!(cache.load_file(root.path(), &path).is_none());
        assert!(cache.entry.is_none());
    }

    #[test]
    fn roots_are_isolated_and_worker_cache_can_be_disabled() {
        let a = root();
        let b = root();
        let a_path = publish(a.path(), "aaa");
        let b_path = publish(b.path(), "bbb");
        let mut cache = SessionCache::default();
        let first = cache.load_file(a.path(), &a_path).unwrap();
        let second = cache.load_file(b.path(), &b_path).unwrap();
        assert!(second.data.0.contains_key("bbb"));
        assert!(!second.data.0.contains_key("aaa"));
        assert_eq!(Arc::strong_count(&first), 1);
        assert!(try_read_session(a.path()).is_none());
        set_session_cache_enabled(true);
        CACHE.with(|cache| assert!(cache.borrow().is_some()));
        set_session_cache_enabled(false);
        assert!(try_read_session(a.path()).is_none());
    }
}
