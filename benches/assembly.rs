// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_asm::traverse::{AssemblyOptions, assemble};
use aden_graph::graph::AdenGraph;
use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn bench_assembly(c: &mut Criterion) {
    let tmp = std::env::temp_dir().join("aden_bench_assembly");
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

    let graph = AdenGraph::build_from_directory(&tmp).unwrap();
    let opts = AssemblyOptions {
        start_anchor: "module-main".to_string(),
        max_depth: 3,
        token_budget: 8192,
        edge_types: vec![],
        block_filter: Vec::new(),
        include_tags: Vec::new(),
        exclude_tags: Vec::new(),
        attributes: Vec::new(),
        llm_mode: true,
        hydrate_root: None,
    };

    c.bench_function("assembly", |b| {
        b.iter(|| {
            let result = assemble(black_box(&graph), black_box(&opts)).unwrap();
            black_box(result);
        })
    });
}

criterion_group!(benches, bench_assembly);
criterion_main!(benches);
