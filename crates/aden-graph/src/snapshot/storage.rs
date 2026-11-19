// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stored-document serialization using the unchanged snapshot v1 map layout.

use super::*;
use serde::ser::{Error, SerializeMap};
use std::cell::RefCell;

struct StoredDocuments<'a, S> {
    storage: &'a S,
    anchors: Vec<String>,
    error: RefCell<Option<StoreError>>,
    // Graph nodes use the embedded document anchor. Normally it equals the
    // store key, so only mismatches need additional storage for diagnostics.
    mismatches: Option<RefCell<Vec<(usize, String)>>>,
}

impl<S: GraphStorage> serde::Serialize for StoredDocuments<'_, S> {
    fn serialize<T: serde::Serializer>(&self, serializer: T) -> Result<T::Ok, T::Error> {
        let mut map = serializer.serialize_map(Some(self.anchors.len()))?;
        for (index, anchor) in self.anchors.iter().enumerate() {
            let document = self
                .storage
                .get_document(anchor)
                .and_then(|doc| doc.ok_or_else(|| StoreError::NotFound(anchor.clone())))
                .map_err(|error| {
                    let message = error.to_string();
                    *self.error.borrow_mut() = Some(error);
                    T::Error::custom(message)
                })?;
            // Only this document is alive while its map entry is serialized.
            map.serialize_entry(anchor, &document)?;
            if let Some(mismatches) = &self.mismatches
                && document.anchor != *anchor
            {
                mismatches.borrow_mut().push((index, document.anchor));
            }
        }
        map.end()
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
    let mut anchors: Vec<_> = storage.get_all_anchors()?.into_iter().collect();
    anchors.sort_unstable();
    let edges = storage.get_all_edges()?;
    let payload = StoredPayload {
        docs: StoredDocuments {
            storage,
            anchors,
            error: RefCell::new(None),
            mismatches: report_orphans.then(RefCell::default),
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
    let Some(mismatches) = payload.docs.mismatches else {
        return Ok(OrphanSummary::default());
    };
    let mut anchors = payload.docs.anchors;
    let mismatches = mismatches.into_inner();
    if !mismatches.is_empty() {
        for (index, embedded) in mismatches {
            anchors[index] = embedded;
        }
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
