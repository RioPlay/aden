// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_graph::graph::AdenGraph;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn bench_graph_construction(c: &mut Criterion) {
    let tmp = std::env::temp_dir().join("aden_bench_graph");
    std::fs::create_dir_all(&tmp).unwrap();

    let main_doc = r#"[[module-main]]
= Main Module

See <<module-helper>>.
"#;
    std::fs::write(tmp.join("main.adoc"), main_doc).unwrap();

    let helper_doc = r#"[[module-helper]]
= Helper Module
"#;
    std::fs::write(tmp.join("helper.adoc"), helper_doc).unwrap();

    c.bench_function("graph_construction", |b| {
        b.iter(|| {
            let graph = AdenGraph::build_from_directory(black_box(&tmp)).unwrap();
            black_box(graph);
        })
    });
}

criterion_group!(benches, bench_graph_construction);
criterion_main!(benches);
