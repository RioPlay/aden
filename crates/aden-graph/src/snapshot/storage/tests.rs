// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use aden_core::{Block, DocumentMetadata, NodeType, SourceSpan};
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};

struct GeneratedStorage {
    count: usize,
    reads: AtomicUsize,
    fail_at: Option<usize>,
    missing: bool,
    embedded_anchors: HashMap<String, String>,
    edges: Option<Vec<(String, String, EdgeType)>>,
}

impl GeneratedStorage {
    fn new(count: usize) -> Self {
        Self {
            count,
            reads: AtomicUsize::new(0),
            fail_at: None,
            missing: false,
            embedded_anchors: HashMap::new(),
            edges: None,
        }
    }

    fn document(anchor: &str) -> Document {
        let index: usize = anchor.parse().unwrap();
        let types = [
            NodeType::Module,
            NodeType::Function,
            NodeType::Type,
            NodeType::Script,
            NodeType::Adr,
            NodeType::Runbook,
            NodeType::Plan,
            NodeType::Context,
            NodeType::Manifest,
            NodeType::Spec,
            NodeType::Note,
            NodeType::Term,
        ];
        Document {
            anchor: anchor.into(),
            node_type: types[index % types.len()].clone(),
            attributes: HashMap::from([("body".into(), format!("{anchor}:{}", "x".repeat(8192)))]),
            blocks: vec![
                Block::Paragraph("first".into()),
                Block::Listing {
                    language: Some("rust".into()),
                    code: "second".into(),
                },
            ],
            source_span: Some(SourceSpan {
                file: "src/example.rs".into(),
                start_line: 2,
                end_line: 4,
                start_byte: 10,
                end_byte: 64,
            }),
            metadata: Some(DocumentMetadata {
                author: Some("author".into()),
                ..Default::default()
            }),
            confidence: 0.75,
        }
    }
}

// Deliberately generate each document at the individual-read boundary. Bulk
// reads panic so a regression to GraphBridge loading cannot pass these tests.
impl GraphStorage for GeneratedStorage {
    fn get_document(&self, anchor: &str) -> Result<Option<Document>, StoreError> {
        let read = self.reads.fetch_add(1, Ordering::SeqCst);
        if self.fail_at == Some(read) {
            return if self.missing {
                Ok(None)
            } else {
                Err(StoreError::Io("injected read failure".into()))
            };
        }
        let mut doc = Self::document(anchor);
        if let Some(embedded) = self.embedded_anchors.get(anchor) {
            doc.anchor.clone_from(embedded);
        }
        Ok(Some(doc))
    }
    fn get_all_documents(&self) -> Result<HashMap<String, Document>, StoreError> {
        panic!("snapshot publication must not materialize all documents")
    }
    fn get_all_anchors(&self) -> Result<HashSet<String>, StoreError> {
        Ok((0..self.count).map(|n| format!("{n:06}")).collect())
    }
    fn get_all_edges(&self) -> Result<Vec<(String, String, EdgeType)>, StoreError> {
        if let Some(edges) = &self.edges {
            return Ok(edges.clone());
        }
        Ok(if self.count > 1 {
            vec![("000000".into(), "000001".into(), EdgeType::Calls)]
        } else {
            vec![]
        })
    }
    fn put_document(&self, _: &Document) -> Result<(), StoreError> {
        unimplemented!()
    }
    fn delete_document(&self, _: &str) -> Result<(), StoreError> {
        unimplemented!()
    }
    fn put_base_snapshot(&self, _: &str, _: &str) -> Result<(), StoreError> {
        unimplemented!()
    }
    fn get_base_snapshot(&self, _: &str) -> Result<Option<String>, StoreError> {
        unimplemented!()
    }
    fn delete_base_snapshot(&self, _: &str) -> Result<(), StoreError> {
        unimplemented!()
    }
    fn get_outgoing_edges(&self, _: &str) -> Result<Vec<(String, EdgeType)>, StoreError> {
        unimplemented!()
    }
    fn get_incoming_edges(&self, _: &str) -> Result<Vec<(String, EdgeType)>, StoreError> {
        unimplemented!()
    }
    fn put_edge(&self, _: &str, _: &str, _: EdgeType) -> Result<(), StoreError> {
        unimplemented!()
    }
    fn delete_edge(&self, _: &str, _: &str, _: &EdgeType) -> Result<(), StoreError> {
        unimplemented!()
    }
    fn get_edges_by_type(&self, _: &EdgeType) -> Result<Vec<(String, String)>, StoreError> {
        unimplemented!()
    }
    fn edge_exists(&self, _: &str, _: &str, _: &EdgeType) -> Result<bool, StoreError> {
        unimplemented!()
    }
    fn get_meta(&self, _: &str) -> Result<Option<String>, StoreError> {
        unimplemented!()
    }
    fn put_meta(&self, _: &str, _: &str) -> Result<(), StoreError> {
        unimplemented!()
    }
    fn bfs(
        &self,
        _: &str,
        _: usize,
        _: Option<&EdgeType>,
    ) -> Result<Vec<(String, String)>, StoreError> {
        unimplemented!()
    }
    fn neighborhood(
        &self,
        _: &str,
        _: usize,
    ) -> Result<HashMap<String, Vec<(String, EdgeType)>>, StoreError> {
        unimplemented!()
    }
    fn count_nodes(&self) -> Result<usize, StoreError> {
        Ok(self.count)
    }
    fn count_edges(&self) -> Result<usize, StoreError> {
        unimplemented!()
    }
    fn clear(&self) -> Result<(), StoreError> {
        unimplemented!()
    }
    fn flush(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

#[test]
fn orphan_report_uses_embedded_endpoints_and_keeps_snapshot_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("graph.snapshot");
    let mut storage = GeneratedStorage::new(14);
    for key in ["000002", "000009"] {
        storage.embedded_anchors.insert(key.into(), "alias".into());
    }
    storage.edges = Some(vec![
        ("000000".into(), "000000".into(), EdgeType::Uses),
        ("000001".into(), "alias".into(), EdgeType::Calls),
        ("000001".into(), "alias".into(), EdgeType::Tests),
        ("000005".into(), "000001".into(), EdgeType::Mentions),
        ("000003".into(), "000009".into(), EdgeType::Calls),
        ("000002".into(), "000004".into(), EdgeType::Calls),
    ]);
    prepare_from_storage(&dest, &storage).unwrap();
    let quiet_bytes = std::fs::read(temporary_path(&dest)).unwrap();
    let report = prepare_from_storage_with_orphans(&dest, &storage).unwrap();
    assert_eq!(report.total, 9);
    assert_eq!(
        report.examples,
        ["000003", "000004", "000006", "000007", "000008"]
    );
    assert_eq!(storage.reads.load(Ordering::SeqCst), storage.count * 2);
    assert_eq!(std::fs::read(temporary_path(&dest)).unwrap(), quiet_bytes);
}

#[test]
fn stored_documents_match_legacy_v1_facts_without_bulk_reads() {
    for count in [0, 1, 256] {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("graph.snapshot");
        let storage = GeneratedStorage::new(count);
        prepare_from_storage(&dest, &storage).unwrap();
        assert!(!dest.exists());
        assert_eq!(storage.reads.load(Ordering::SeqCst), count);
        let bytes = std::fs::read(temporary_path(&dest)).unwrap();
        let expected_docs = (0..count)
            .map(|n| {
                let anchor = format!("{n:06}");
                (anchor.clone(), GeneratedStorage::document(&anchor))
            })
            .collect();
        let legacy = encode_snapshot(&expected_docs, &storage.get_all_edges().unwrap()).unwrap();
        assert_eq!(
            decode_snapshot(&bytes).unwrap(),
            decode_snapshot(&legacy).unwrap()
        );
        publish_prepared(&dest).unwrap();
        assert_eq!(
            read_snapshot_file(&dest).unwrap(),
            decode_snapshot(&legacy).unwrap()
        );
    }
}

#[test]
fn failed_document_reads_preserve_published_snapshot_and_recover() {
    for missing in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("graph.snapshot");
        publish_from_storage(&dest, &GeneratedStorage::new(1)).unwrap();
        let published = std::fs::read(&dest).unwrap();
        let storage = GeneratedStorage {
            fail_at: Some(1),
            missing,
            ..GeneratedStorage::new(3)
        };
        let error = publish_from_storage(&dest, &storage).unwrap_err();
        if missing {
            assert!(
                matches!(error, SnapshotError::Store(StoreError::NotFound(anchor)) if anchor == "000001")
            );
        } else {
            assert!(
                matches!(error, SnapshotError::Store(StoreError::Io(message)) if message == "injected read failure")
            );
        }
        assert_eq!(storage.reads.load(Ordering::SeqCst), 2);
        assert_eq!(std::fs::read(&dest).unwrap(), published);
        assert!(!temporary_path(&dest).exists());
        publish_from_storage(&dest, &GeneratedStorage::new(3)).unwrap();
        assert_eq!(read_snapshot_file(&dest).unwrap().0.len(), 3);
    }
}

#[test]
fn streamed_snapshot_remains_preferred_with_newer_live_store() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".aden")).unwrap();
    assert_eq!(
        aden_paths::resolve_root(dir.path()),
        dir.path().canonicalize().unwrap()
    );
    aden_paths::write_storage_config(
        dir.path(),
        Some(&aden_paths::StorageConfig {
            portable: true,
            data_dir: None,
        }),
    )
    .unwrap();
    let dest = aden_paths::graph_snapshot_file(dir.path());
    publish_from_storage(&dest, &GeneratedStorage::new(2)).unwrap();
    let newer_store = dest.parent().unwrap().join("store/newer");
    std::fs::create_dir_all(newer_store.parent().unwrap()).unwrap();
    std::fs::write(&newer_store, b"newer live state").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&newer_store)
        .unwrap()
        .set_modified(SystemTime::now() + std::time::Duration::from_secs(2))
        .unwrap();
    assert!(!snapshot_covers_store(&dest, newer_store.parent().unwrap()));
    assert_eq!(try_read_fresh(dir.path()).unwrap().0.len(), 2);
}

#[test]
fn stored_snapshot_matches_fjall_bulk_read_including_rehydrated_spans() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("graph.snapshot");
    let storage = aden_store::Storage::new(dir.path().join("store").to_str().unwrap()).unwrap();
    for n in 0..12 {
        let mut document = GeneratedStorage::document(&format!("{n:06}"));
        let span = document.source_span.take().unwrap();
        document.attributes.extend(span.to_attributes());
        storage.put_document(&document).unwrap();
    }
    storage
        .put_edge("000000", "000001", EdgeType::Calls)
        .unwrap();
    let legacy = encode_snapshot(
        &storage.get_all_documents().unwrap(),
        &storage.get_all_edges().unwrap(),
    )
    .unwrap();
    prepare_from_storage(&dest, &storage).unwrap();
    drop(storage);
    publish_prepared(&dest).unwrap();
    assert_eq!(
        read_snapshot_file(&dest).unwrap(),
        decode_snapshot(&legacy).unwrap()
    );
}

#[test]
fn temporary_open_failure_does_not_remove_existing_path_or_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("graph.snapshot");
    publish_from_storage(&dest, &GeneratedStorage::new(1)).unwrap();
    let published = std::fs::read(&dest).unwrap();
    std::fs::create_dir(temporary_path(&dest)).unwrap();
    assert!(matches!(
        prepare_from_storage(&dest, &GeneratedStorage::new(2)),
        Err(SnapshotError::Io(_))
    ));
    assert!(temporary_path(&dest).is_dir());
    assert_eq!(std::fs::read(&dest).unwrap(), published);
}
