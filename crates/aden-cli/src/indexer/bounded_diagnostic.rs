// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Ignored unit diagnostic, absent from shipped debug and release CLI builds.

use super::bounded;
use aden_store::{GraphStorage, Storage};
use serde_json::Value;

fn canonical(value: &Value, root: &str) -> String {
    match value {
        Value::Object(object) => {
            let sorted: std::collections::BTreeMap<_, _> = object
                .iter()
                .filter(|(key, _)| !matches!(key.as_str(), "last-verified" | "last_verified"))
                .map(|(key, value)| (key, canonical(value, root)))
                .collect();
            serde_json::to_string(&sorted).unwrap()
        }
        Value::Array(values) => serde_json::to_string(
            &values
                .iter()
                .map(|value| canonical(value, root))
                .collect::<Vec<_>>(),
        )
        .unwrap(),
        Value::String(value) => serde_json::to_string(
            &value
                .replace(root, "$ROOT")
                .replace(&root.replace('/', "\\"), "$ROOT"),
        )
        .unwrap(),
        other => other.to_string(),
    }
}

#[test]
#[ignore = "controlled full-generation measurement, requires an owned prepared fixture"]
fn bounded_gen_release_diagnostic() {
    let policy = std::env::var("ADEN_BENCH_POLICY").expect("ADEN_BENCH_POLICY");
    let limits = match policy.as_str() {
        "legacy" => (usize::MAX, u64::MAX),
        "bounded" => (bounded::SOURCE_FILES, bounded::SOURCE_BYTES),
        _ => panic!("policy must be legacy or bounded"),
    };
    let root =
        std::path::PathBuf::from(std::env::var_os("ADEN_BENCH_ROOT").expect("ADEN_BENCH_ROOT"))
            .canonicalize()
            .unwrap();
    assert!(
        root.join(".aden-bounded-bench").is_file(),
        "owned fixture marker required"
    );
    assert!(
        std::env::var_os("ADEN_DATA_DIR").is_some(),
        "isolated ADEN_DATA_DIR required"
    );
    assert!(
        std::env::var_os("ADEN_STORE").is_none(),
        "ADEN_STORE must be unset"
    );
    let started = std::time::Instant::now();
    bounded::with_limits(limits, || super::cmd_gen_opts(&root, true, false, false)).unwrap();
    let generation_ms = started.elapsed().as_secs_f64() * 1000.0;

    // Read one document at a time so digest acquisition does not recreate the
    // all-document peak under measurement. Retain only compact hashes.
    let store = Storage::open_existing(aden_paths::store_dir(&root).to_str().unwrap()).unwrap();
    let normalized_root = root.to_string_lossy().replace('\\', "/");
    let mut document_hashes = Vec::new();
    for anchor in store.get_all_anchors().unwrap() {
        let document = store.get_document(&anchor).unwrap().expect("stored anchor");
        let text = canonical(&serde_json::to_value(document).unwrap(), &normalized_root);
        document_hashes.push((anchor, aden_core::stable_hash(text.as_bytes())));
    }
    document_hashes.sort();
    let edges = store.get_all_edges().unwrap();
    let mut edge_hashes: Vec<_> = edges
        .into_iter()
        .map(|edge| {
            let text = canonical(&serde_json::to_value(edge).unwrap(), &normalized_root);
            aden_core::stable_hash(text.as_bytes())
        })
        .collect();
    edge_hashes.sort();
    let document_digest = aden_core::stable_hash(&serde_json::to_vec(&document_hashes).unwrap());
    let edge_digest = aden_core::stable_hash(&serde_json::to_vec(&edge_hashes).unwrap());
    println!(
        "{}",
        serde_json::json!({
            "type": "bounded_gen_diagnostic",
            "policy": policy,
            "generation_ms": generation_ms,
            "documents": document_hashes.len(),
            "edges": edge_hashes.len(),
            "document_digest": document_digest,
            "edge_digest": edge_digest,
        })
    );
}

#[test]
fn canonical_digest_ignores_declared_volatility_and_detects_changed_content() {
    let first =
        serde_json::json!({"last-verified": "old", "source_file": "/owned/a.rs", "body": "one"});
    let same =
        serde_json::json!({"body": "one", "last-verified": "new", "source_file": "/owned/a.rs"});
    let changed = serde_json::json!({"body": "two", "source_file": "/owned/a.rs"});
    assert_eq!(canonical(&first, "/owned"), canonical(&same, "/owned"));
    assert_ne!(canonical(&first, "/owned"), canonical(&changed, "/owned"));
}
