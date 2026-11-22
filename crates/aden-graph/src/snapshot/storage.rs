// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stored-document serialization using the unchanged snapshot v1 map layout.

use super::*;
use serde::ser::{Error, SerializeMap};
use std::cell::{Cell, RefCell};

struct StoredDocuments<'a, S> {
    storage: &'a S,
    error: RefCell<Option<StoreError>>,
    collect_anchors: bool,
    // Graph nodes use the embedded document anchor. Normally it equals the
    // store key in ascending order, so only out-of-order mismatches require a resort.
    anchors: RefCell<Vec<String>>,
    needs_resort: Cell<bool>,
}

impl<S: GraphStorage> serde::Serialize for StoredDocuments<'_, S> {
    fn serialize<T: serde::Serializer>(&self, serializer: T) -> Result<T::Ok, T::Error> {
        struct StreamState<Ser, Map, Err> {
            serializer: Option<Ser>,
            map: Option<Map>,
            err: Option<Err>,
        }

        let state = RefCell::new(StreamState {
            serializer: Some(serializer),
            map: None,
            err: None,
        });

        let res = self.storage.visit_documents_ordered(
            &mut |count| {
                let mut st = state.borrow_mut();
                let Some(s) = st.serializer.take() else {
                    return Err(StoreError::Serialization(
                        "visit_documents_ordered invoked on_count more than once".into(),
                    ));
                };
                match s.serialize_map(Some(count)) {
                    Ok(map) => {
                        st.map = Some(map);
                        if self.collect_anchors {
                            self.anchors.borrow_mut().reserve(count);
                        }
                        Ok(())
                    }
                    Err(e) => {
                        st.err = Some(e);
                        Err(StoreError::Serialization(
                            "snapshot map header serialization aborted".into(),
                        ))
                    }
                }
            },
            &mut |anchor, document| {
                let mut st = state.borrow_mut();
                let Some(map) = st.map.as_mut() else {
                    return Err(StoreError::Serialization(
                        "visit_documents_ordered invoked on_doc before on_count".into(),
                    ));
                };
                if let Err(e) = map.serialize_entry(&anchor, &document) {
                    st.err = Some(e);
                    return Err(StoreError::Serialization(
                        "snapshot document serialization aborted".into(),
                    ));
                }
                if self.collect_anchors {
                    if document.anchor != anchor {
                        self.needs_resort.set(true);
                    }
                    self.anchors.borrow_mut().push(document.anchor);
                }
                Ok(())
            },
        );

        let st = state.into_inner();
        if let Some(e) = st.err {
            return Err(e);
        }
        if let Err(error) = res {
            let message = error.to_string();
            *self.error.borrow_mut() = Some(error);
            return Err(T::Error::custom(message));
        }
        match st.map {
            Some(map) => map.end(),
            None => Err(T::Error::custom(
                "snapshot document stream did not initialize",
            )),
        }
    }
}

#[derive(serde::Serialize)]
#[serde(bound(serialize = ""))]
struct StoredPayload<'a, S: GraphStorage> {
    docs: StoredDocuments<'a, S>,
    edges: &'a [(String, String, EdgeType)],
}

pub(super) fn prepare<S: GraphStorage>(
    dest: &Path,
    storage: &S,
    report_orphans: bool,
) -> Result<OrphanSummary, SnapshotError> {
    let edges = storage.get_all_edges()?;
    let payload = StoredPayload {
        docs: StoredDocuments {
            storage,
            error: RefCell::new(None),
            collect_anchors: report_orphans,
            anchors: RefCell::new(Vec::new()),
            needs_resort: Cell::new(false),
        },
        edges: &edges,
    };
    let result = write_payload_to_temp(dest, &payload);
    // Serde's error type cannot carry a StoreError; restore the original error
    // instead of reporting a missing document or failed store read as encoding.
    if let Some(error) = payload.docs.error.into_inner() {
        return Err(SnapshotError::Store(error));
    }
    result?;
    if !report_orphans {
        return Ok(OrphanSummary::default());
    }
    let mut anchors = payload.docs.anchors.into_inner();
    if payload.docs.needs_resort.get() {
        anchors.sort_unstable();
        anchors.dedup();
    }
    Ok(summarize_orphans(anchors, &edges))
}

fn summarize_orphans(
    sorted_anchors: Vec<String>,
    edges: &[(String, String, EdgeType)],
) -> OrphanSummary {
    let mut connected = vec![false; sorted_anchors.len()];
    for (source, target, _) in edges {
        // A dangling edge is discarded by the graph builder. Its surviving
        // endpoint still needs another valid edge to count as connected.
        if let (Ok(src), Ok(dst)) = (
            sorted_anchors.binary_search(source),
            sorted_anchors.binary_search(target),
        ) {
            connected[src] = true;
            connected[dst] = true;
        }
    }
    let mut isolated = sorted_anchors
        .into_iter()
        .zip(connected)
        .filter_map(|(anchor, linked)| (!linked).then_some(anchor));
    let examples: Vec<_> = isolated.by_ref().take(5).collect();
    OrphanSummary {
        total: examples.len() + isolated.count(),
        examples,
    }
}

#[cfg(test)]
mod tests;
