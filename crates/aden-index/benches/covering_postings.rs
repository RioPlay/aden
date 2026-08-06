// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! A/B experiment for string postings versus numeric covering postings.
//!
//! This intentionally does not change the production `Index`. It isolates the
//! representation/scoring hypothesis first; promotion requires a repeated win
//! plus the existing retrieval quality gates.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hint::black_box;

const K1: f64 = 1.5;
const B: f64 = 0.75;

#[derive(Clone, Serialize, Deserialize)]
struct SourceDoc {
    anchor: String,
    path: String,
    text: String,
    len: u32,
    terms: Vec<(String, u32)>,
}

#[derive(Serialize, Deserialize)]
struct StringLayout {
    inverted: HashMap<String, Vec<(String, u32)>>,
    doc_lengths: HashMap<String, u32>,
    #[serde(rename = "anchor_paths")]
    paths: HashMap<String, String>,
    #[serde(rename = "anchor_text")]
    texts: HashMap<String, String>,
    #[serde(rename = "avg_doc_length")]
    avg_len: f64,
}

/// Tuple encoding is deliberate: JSON objects repeat field names for every
/// posting and regressed small-cache load time. A fixed-shape tuple is compact
/// today and maps directly to a future fixed-width binary record.
#[derive(Clone, Copy, Serialize, Deserialize)]
struct Posting(u32, u32, u32);

#[derive(Serialize, Deserialize)]
struct DocRecord {
    anchor: String,
    path: String,
    text: String,
}

#[derive(Serialize, Deserialize)]
struct CoveringLayout {
    inverted: HashMap<String, Vec<Posting>>,
    documents: Vec<DocRecord>,
    avg_len: f64,
}

fn corpus(doc_count: usize, terms_per_doc: usize, vocabulary: usize) -> Vec<SourceDoc> {
    (0..doc_count)
        .map(|doc| {
            let mut frequencies = HashMap::<String, u32>::new();
            // Deterministic spread with overlap and repeated terms.
            let mut state = (doc as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
            for _ in 0..terms_per_doc {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let term = format!("term_{:05}", (state as usize) % vocabulary);
                *frequencies.entry(term).or_default() += 1;
            }
            let text = frequencies
                .keys()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            SourceDoc {
                anchor: format!(
                    "aden://module/synthetic/src/module_{:05}.rs#symbol_{doc:05}",
                    doc % 500
                ),
                path: format!("src/module_{:05}.rs", doc % 500),
                text,
                len: terms_per_doc as u32,
                terms: frequencies.into_iter().collect(),
            }
        })
        .collect()
}

fn build_string(docs: &[SourceDoc]) -> StringLayout {
    let mut layout = StringLayout {
        inverted: HashMap::new(),
        doc_lengths: HashMap::new(),
        paths: HashMap::new(),
        texts: HashMap::new(),
        avg_len: docs.iter().map(|d| d.len as usize).sum::<usize>() as f64 / docs.len() as f64,
    };
    for doc in docs {
        layout.doc_lengths.insert(doc.anchor.clone(), doc.len);
        layout.paths.insert(doc.anchor.clone(), doc.path.clone());
        layout.texts.insert(doc.anchor.clone(), doc.text.clone());
        for (term, tf) in &doc.terms {
            layout
                .inverted
                .entry(term.clone())
                .or_default()
                .push((doc.anchor.clone(), *tf));
        }
    }
    layout
}

fn covering_from_string(source: &StringLayout) -> CoveringLayout {
    let mut anchors: Vec<_> = source.doc_lengths.keys().cloned().collect();
    anchors.sort();
    let ids: HashMap<_, _> = anchors
        .iter()
        .enumerate()
        .map(|(id, anchor)| (anchor.as_str(), id as u32))
        .collect();
    let documents = anchors
        .iter()
        .map(|anchor| DocRecord {
            anchor: anchor.clone(),
            path: source.paths.get(anchor).cloned().unwrap_or_default(),
            text: source.texts.get(anchor).cloned().unwrap_or_default(),
        })
        .collect();
    let inverted = source
        .inverted
        .iter()
        .map(|(term, postings)| {
            let compact = postings
                .iter()
                .filter_map(|(anchor, tf)| {
                    Some(Posting(
                        *ids.get(anchor.as_str())?,
                        *tf,
                        source.doc_lengths.get(anchor).copied().unwrap_or(1),
                    ))
                })
                .collect();
            (term.clone(), compact)
        })
        .collect();
    CoveringLayout {
        inverted,
        documents,
        avg_len: source.avg_len,
    }
}

fn build_covering(docs: &[SourceDoc]) -> CoveringLayout {
    let mut layout = CoveringLayout {
        inverted: HashMap::new(),
        documents: Vec::with_capacity(docs.len()),
        avg_len: docs.iter().map(|d| d.len as usize).sum::<usize>() as f64 / docs.len() as f64,
    };
    for (doc_id, doc) in docs.iter().enumerate() {
        layout.documents.push(DocRecord {
            anchor: doc.anchor.clone(),
            path: doc.path.clone(),
            text: doc.text.clone(),
        });
        for (term, tf) in &doc.terms {
            layout
                .inverted
                .entry(term.clone())
                .or_default()
                .push(Posting(doc_id as u32, *tf, doc.len));
        }
    }
    layout
}

fn bm25(tf: u32, doc_len: u32, avg_len: f64, idf: f64) -> f64 {
    let tf = tf as f64;
    let normalized = (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * doc_len as f64 / avg_len));
    idf * normalized
}

fn score_string(index: &StringLayout, terms: &[String]) -> Vec<(String, f64)> {
    // Match the production scorer exactly: today `n` is the sum of posting-list
    // lengths, not the document count. This experiment changes representation,
    // not ranking semantics; correcting that denominator is a separate decision.
    let n = index.inverted.values().map(Vec::len).sum::<usize>() as f64;
    let mut scores = HashMap::<String, f64>::new();
    for term in terms {
        let Some(postings) = index.inverted.get(term) else {
            continue;
        };
        let df = postings.len() as f64;
        let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln().max(0.0);
        for (anchor, tf) in postings {
            let len = index.doc_lengths.get(anchor).copied().unwrap_or(1);
            *scores.entry(anchor.clone()).or_default() += bm25(*tf, len, index.avg_len, idf);
        }
    }
    let mut ranked: Vec<_> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}

fn score_covering(index: &CoveringLayout, terms: &[String]) -> Vec<(String, f64)> {
    let n = index.inverted.values().map(Vec::len).sum::<usize>() as f64;
    let mut scores = HashMap::<u32, f64>::new();
    for term in terms {
        let Some(postings) = index.inverted.get(term) else {
            continue;
        };
        let df = postings.len() as f64;
        let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln().max(0.0);
        for posting in postings {
            *scores.entry(posting.0).or_default() += bm25(posting.1, posting.2, index.avg_len, idf);
        }
    }
    let mut ranked: Vec<_> = scores
        .into_iter()
        .map(|(id, score)| (index.documents[id as usize].anchor.clone(), score))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}

fn estimated_postings_heap_string(index: &StringLayout) -> usize {
    index
        .inverted
        .values()
        .flatten()
        .map(|(anchor, _)| std::mem::size_of::<(String, u32)>() + anchor.capacity())
        .sum()
}

fn estimated_postings_heap_covering(index: &CoveringLayout) -> usize {
    index
        .inverted
        .values()
        .map(|v| v.capacity() * std::mem::size_of::<Posting>())
        .sum()
}

fn bench_real_cache(c: &mut Criterion, path: &std::path::Path) {
    let bytes = std::fs::read(path).expect("read ADEN_POSTINGS_CACHE");
    let string: StringLayout =
        serde_json::from_slice(&bytes).expect("parse production index cache");
    let covering = covering_from_string(&string);
    let mut terms: Vec<_> = string
        .inverted
        .iter()
        .filter(|(_, postings)| postings.len() >= 10)
        .map(|(term, _)| term.clone())
        .collect();
    terms.sort();
    let query: Vec<_> = terms.into_iter().take(6).collect();
    assert_eq!(query.len(), 6, "real cache needs six nontrivial terms");

    let old_rank = score_string(&string, &query);
    let new_rank = score_covering(&covering, &query);
    assert_eq!(old_rank.len(), new_rank.len());
    for (old, new) in old_rank.iter().zip(&new_rank) {
        assert_eq!(old.0, new.0, "real-cache ranking changed");
        assert_eq!(old.1.to_bits(), new.1.to_bits(), "real-cache score changed");
    }

    // Compare only equivalent lexical fields. Production caches may also carry
    // dense embeddings; dropping those from only one arm would fake a win.
    let baseline_json = serde_json::to_vec(&string).unwrap();
    let compact_json = serde_json::to_vec(&covering).unwrap();
    let compact_roundtrip: CoveringLayout = serde_json::from_slice(&compact_json).unwrap();
    let roundtrip_rank = score_covering(&compact_roundtrip, &query);
    assert_eq!(new_rank.len(), roundtrip_rank.len());
    for (before, after) in new_rank.iter().zip(&roundtrip_rank) {
        assert_eq!(before.0, after.0, "compact cache roundtrip changed ranking");
        assert_eq!(
            before.1.to_bits(),
            after.1.to_bits(),
            "compact cache roundtrip changed score"
        );
    }
    eprintln!(
        "real-cache covering report: source={} docs={} postings={} old_json={} new_json={} ratio={:.3} old_postings_heap_est={} new_postings_heap_est={} heap_ratio={:.3}",
        path.display(),
        string.doc_lengths.len(),
        string.inverted.values().map(Vec::len).sum::<usize>(),
        baseline_json.len(),
        compact_json.len(),
        compact_json.len() as f64 / baseline_json.len() as f64,
        estimated_postings_heap_string(&string),
        estimated_postings_heap_covering(&covering),
        estimated_postings_heap_covering(&covering) as f64
            / estimated_postings_heap_string(&string) as f64,
    );

    let mut scoring = c.benchmark_group("real_cache_bm25_posting_layout");
    scoring.bench_function("anchor_string_plus_length_lookup", |b| {
        b.iter(|| score_string(black_box(&string), black_box(&query)))
    });
    scoring.bench_function("document_id_covering_posting", |b| {
        b.iter(|| score_covering(black_box(&covering), black_box(&query)))
    });
    scoring.finish();

    let mut conversion = c.benchmark_group("real_cache_covering_conversion");
    conversion.bench_function("from_anchor_string_layout", |b| {
        b.iter(|| covering_from_string(black_box(&string)))
    });
    conversion.finish();

    let mut load = c.benchmark_group("real_cache_json_load");
    load.bench_function("anchor_string", |b| {
        b.iter(|| serde_json::from_slice::<StringLayout>(black_box(&baseline_json)).unwrap())
    });
    load.bench_function("document_id_covering", |b| {
        b.iter(|| serde_json::from_slice::<CoveringLayout>(black_box(&compact_json)).unwrap())
    });
    load.finish();
}

fn bench(c: &mut Criterion) {
    if let Some(path) = std::env::var_os("ADEN_POSTINGS_CACHE") {
        bench_real_cache(c, std::path::Path::new(&path));
        return;
    }

    let docs = corpus(5_000, 120, 8_000);
    let string = build_string(&docs);
    let covering = build_covering(&docs);
    let query: Vec<String> = docs[123]
        .terms
        .iter()
        .take(6)
        .map(|(t, _)| t.clone())
        .collect();

    let old_rank = score_string(&string, &query);
    let new_rank = score_covering(&covering, &query);
    assert_eq!(old_rank.len(), new_rank.len());
    for (old, new) in old_rank.iter().zip(&new_rank) {
        assert_eq!(old.0, new.0, "ranking changed");
        assert_eq!(
            old.1.to_bits(),
            new.1.to_bits(),
            "score changed for {}",
            old.0
        );
    }

    let old_json = serde_json::to_vec(&string).unwrap();
    let new_json = serde_json::to_vec(&covering).unwrap();
    let roundtrip: CoveringLayout = serde_json::from_slice(&new_json).unwrap();
    let roundtrip_rank = score_covering(&roundtrip, &query);
    assert_eq!(new_rank.len(), roundtrip_rank.len());
    for (before, after) in new_rank.iter().zip(&roundtrip_rank) {
        assert_eq!(before.0, after.0, "compact cache roundtrip changed ranking");
        assert_eq!(before.1.to_bits(), after.1.to_bits());
    }
    eprintln!(
        "covering-postings report: postings={} old_json={} new_json={} ratio={:.3} old_postings_heap_est={} new_postings_heap_est={} heap_ratio={:.3}",
        string.inverted.values().map(Vec::len).sum::<usize>(),
        old_json.len(),
        new_json.len(),
        new_json.len() as f64 / old_json.len() as f64,
        estimated_postings_heap_string(&string),
        estimated_postings_heap_covering(&covering),
        estimated_postings_heap_covering(&covering) as f64
            / estimated_postings_heap_string(&string) as f64,
    );

    let mut scoring = c.benchmark_group("bm25_posting_layout");
    scoring.throughput(Throughput::Elements(query.len() as u64));
    scoring.bench_function("anchor_string_plus_length_lookup", |b| {
        b.iter(|| score_string(black_box(&string), black_box(&query)))
    });
    scoring.bench_function("document_id_covering_posting", |b| {
        b.iter(|| score_covering(black_box(&covering), black_box(&query)))
    });
    scoring.finish();

    let mut construction = c.benchmark_group("posting_layout_build");
    construction.bench_with_input(
        BenchmarkId::new("anchor_string", docs.len()),
        &docs,
        |b, docs| b.iter(|| build_string(black_box(docs))),
    );
    construction.bench_with_input(
        BenchmarkId::new("document_id_covering", docs.len()),
        &docs,
        |b, docs| b.iter(|| build_covering(black_box(docs))),
    );
    construction.finish();

    let mut load = c.benchmark_group("posting_layout_json_load");
    load.throughput(Throughput::Bytes(old_json.len() as u64));
    load.bench_function("anchor_string", |b| {
        b.iter(|| serde_json::from_slice::<StringLayout>(black_box(&old_json)).unwrap())
    });
    load.throughput(Throughput::Bytes(new_json.len() as u64));
    load.bench_function("document_id_covering", |b| {
        b.iter(|| serde_json::from_slice::<CoveringLayout>(black_box(&new_json)).unwrap())
    });
    load.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
