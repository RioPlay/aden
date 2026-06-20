// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Storage performance benchmarks.
//!
//! Two measurements:
//! 1. `store_open_drop` — open an existing fjall store and immediately drop it.
//!    This was the 250ms floor per command with fjall 2.11 (monitor thread
//!    unconditionally slept before checking the stop signal).  With fjall 3.1+
//!    teardown uses a flume channel wake and completes in single-digit µs.
//!
//! 2. `get_all_edges_single_scan` vs `get_all_edges_per_type` — loading every
//!    edge from the store.  The old bridge loop called `get_edges_by_type` once
//!    per EdgeType variant (32 full scans of the edges partition).  The new
//!    `get_all_edges` implementation does it in one pass.

use aden_core::{Document, EdgeType, NodeType};
use aden_store::{GraphStorage, Storage};
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use std::collections::HashMap;
use std::path::PathBuf;

fn temp_store_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("aden_bench_store_{label}"))
}

fn seed_store(path: &str, n_docs: usize, edges_per_type: usize) {
    let storage = Storage::new(path).expect("create store");

    // Insert documents.
    for i in 0..n_docs {
        let doc = Document {
            anchor: format!("anchor-{i}"),
            node_type: NodeType::Function,
            attributes: HashMap::new(),
            blocks: vec![],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };
        storage.put_document(&doc).expect("put doc");
    }

    // Insert a handful of edges of every type so get_all_edges has real work.
    let anchors: Vec<String> = (0..n_docs).map(|i| format!("anchor-{i}")).collect();
    let mut bulk: Vec<(String, String, EdgeType)> = Vec::new();
    for et in &EdgeType::ALL {
        for j in 0..edges_per_type {
            let src = &anchors[j % n_docs];
            let dst = &anchors[(j + 1) % n_docs];
            bulk.push((src.clone(), dst.clone(), *et));
        }
    }
    storage.put_edges_bulk(&bulk).expect("put edges");
    storage.flush().expect("flush");
}

fn bench_store_open_drop(c: &mut Criterion) {
    let path = temp_store_path("open_drop");
    let path_str = path.to_string_lossy().to_string();

    // Seed once so open_existing finds a real store.
    if !path.exists() {
        seed_store(&path_str, 50, 3);
    }

    c.bench_function("store_open_drop", |b| {
        b.iter(|| {
            let s = Storage::open_existing(black_box(&path_str)).expect("open");
            drop(black_box(s));
        })
    });
}

fn bench_get_all_edges(c: &mut Criterion) {
    let path = temp_store_path("get_all_edges");
    let path_str = path.to_string_lossy().to_string();

    if !path.exists() {
        // 200 docs, 5 edges per type → 160 total edges across 32 types.
        seed_store(&path_str, 200, 5);
    }

    let storage = Storage::open_existing(&path_str).expect("open");

    let mut g = c.benchmark_group("edge_load");

    // New: single scan bucketing by type embedded in the key.
    g.bench_function("single_scan", |b| {
        b.iter(|| {
            let edges = storage.get_all_edges().expect("get_all_edges");
            black_box(edges);
        })
    });

    // Old: one full scan per EdgeType variant (32 scans total).
    g.bench_with_input(
        BenchmarkId::new("per_type_loop", EdgeType::ALL.len()),
        &EdgeType::ALL.len(),
        |b, _| {
            b.iter(|| {
                let mut edges = Vec::new();
                for et in &EdgeType::ALL {
                    for (src, dst) in storage.get_edges_by_type(et).expect("get_edges_by_type") {
                        edges.push((src, dst, *et));
                    }
                }
                black_box(edges);
            })
        },
    );

    g.finish();
}

criterion_group!(benches, bench_store_open_drop, bench_get_all_edges);
criterion_main!(benches);
