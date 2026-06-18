// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Dumps the CLEAN eval corpus (cards + out-edges) and the shared probe set to JSON for the GPU
// sidecar (scripts/gpu_eval.py), which runs the same bge model on CUDA so recipe iteration is
// seconds instead of minutes. No embedding here (CPU-only, fast); same /tests/ leak filter as
// the harnesses so the dumped corpus matches what they evaluate. #[ignore]d; writes to
// ~/.cache/aden/dict/eval/ (NOT .aden/store).
//
// Run: cargo test -p aden-cli --test dump_corpus -- --include-ignored --nocapture

mod common;

use std::path::PathBuf;

fn index_text(doc: &aden_core::Document) -> String {
    aden_emit::emit_document(doc)
        .lines()
        .filter(|l| !l.trim_start().starts_with(":last-verified:"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
#[ignore = "dump corpus+edges+probes to JSON for the GPU sidecar (writes ~/.cache/aden/dict/eval)"]
fn dump_corpus() {
    use aden_store::{GraphStorage, Storage};

    let root = aden_paths::resolve_root(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let (store_path, _) = aden_paths::resolve_read_store(&root);
    let Some(storage) = store_path
        .to_str()
        .and_then(|s| Storage::open_existing(s).ok())
    else {
        eprintln!("SKIP: no project store");
        return;
    };
    let Ok(docs) = storage.get_all_documents() else {
        eprintln!("SKIP: cannot read documents");
        return;
    };
    let cards: Vec<(String, String)> = docs
        .values()
        .filter(|d| {
            !d.anchor.contains("/tests/")
                && !d
                    .attributes
                    .get("source_file")
                    .is_some_and(|s| s.contains("/tests/"))
        })
        .map(|d| (d.anchor.clone(), index_text(d)))
        .collect();
    let set: std::collections::HashSet<&str> = cards.iter().map(|(a, _)| a.as_str()).collect();

    let mut edges: Vec<(String, String, String)> = Vec::new();
    for (a, _) in &cards {
        if let Ok(out) = storage.get_outgoing_edges(a) {
            for (tgt, et) in out {
                if set.contains(tgt.as_str()) {
                    edges.push((a.clone(), tgt, format!("{et:?}")));
                }
            }
        }
    }

    let dir =
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache/aden/dict/eval");
    std::fs::create_dir_all(&dir).ok();
    let cj: Vec<serde_json::Value> = cards
        .iter()
        .map(|(a, t)| serde_json::json!([a, t]))
        .collect();
    let ej: Vec<serde_json::Value> = edges
        .iter()
        .map(|(s, t, e)| serde_json::json!([s, t, e]))
        .collect();
    let pj: Vec<serde_json::Value> = common::PROBES
        .iter()
        .map(|(q, acc, exp)| serde_json::json!([q, acc, exp]))
        .collect();
    std::fs::write(
        dir.join("cards.json"),
        serde_json::to_string(&cj).unwrap_or_default(),
    )
    .ok();
    std::fs::write(
        dir.join("edges.json"),
        serde_json::to_string(&ej).unwrap_or_default(),
    )
    .ok();
    std::fs::write(
        dir.join("probes.json"),
        serde_json::to_string(&pj).unwrap_or_default(),
    )
    .ok();
    println!(
        "dumped {} cards, {} edges, {} probes -> {}",
        cards.len(),
        edges.len(),
        common::PROBES.len(),
        dir.display()
    );
}
