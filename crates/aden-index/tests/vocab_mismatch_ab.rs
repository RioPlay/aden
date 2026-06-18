// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Vocabulary-mismatch A/B (NOT a committed gate — a measurement harness).
//
// Question: does explicit synonym/hypernym expansion add measurable routing lift
// over aden's existing BM25 (+ dense RRF) stack, on the short-keyword regime where
// query terms do NOT lexically overlap the target's terms? This is the open
// empirical question the aden-semantic-retrieval findings brief flags as unsettled
// (red-team #4: "RRF may already capture the lexical signal") — the number that
// decides whether the WordNet→edge importer (which writes to .aden/store) is worth
// building BEFORE anything touches the store.
//
// Three arms over the SAME paragraph index and the SAME queries:
//   * BM25            — the baseline (index.query).
//   * BM25 + ORACLE   — a hand-authored, correct-sense synonym/hypernym expansion
//                       appended at query time. This is Gonzalo et al. 1998's
//                       *oracle disambiguation* condition: the UPPER BOUND of what a
//                       lexical-semantic edge layer could achieve. If even the oracle
//                       does not beat BM25, the importer is not worth building.
//   * HYBRID          — BM25 + dense (bge-small) via RRF (index.hybrid_query); shows
//                       how much of the oracle's headroom dense already recovers.
//
// The decision number is `oracle - hybrid`: the routing headroom NET of what dense
// already captures. Reports routing R@1 (top hit's source doc is acceptable) per arm
// plus the BM25 miss list (the addressable set a graph layer would target).
//
// Vocabulary-mismatch query set: each query is authored to share little surface
// vocabulary with its target doc, so the retriever must bridge the gap. Small,
// hand-authored, illustrative — the RELATIVE arm comparison is the signal, not an
// absolute leaderboard.
//
// Default corpus: ~/Projects/AI Research/docs (override ADEN_PROSE_CORPUS).
// Run: cargo test -p aden-index --features dense --test vocab_mismatch_ab -- --include-ignored --nocapture

use aden_index::Index;
use std::path::{Path, PathBuf};

fn corpus_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("ADEN_PROSE_CORPUS") {
        let p = PathBuf::from(d);
        return p.is_dir().then_some(p);
    }
    let home = std::env::var("HOME").ok()?;
    let p = PathBuf::from(home).join("Projects/AI Research/docs");
    p.is_dir().then_some(p)
}

fn collect_adoc(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_adoc(&p, out);
            } else if p.extension().and_then(|x| x.to_str()) == Some("adoc") {
                out.push(p);
            }
        }
    }
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Blank-line-delimited non-empty paragraphs (the PlainTextExtractor unit).
fn paragraphs(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n")
        .split("\n\n")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Source doc stem from a paragraph anchor (`name`, `name__pN`).
fn doc_of(anchor: &str) -> &str {
    anchor.split("__").next().unwrap_or(anchor)
}

/// One vocabulary-mismatch probe.
struct Probe {
    /// The query, deliberately using different words than the target doc.
    query: &'static str,
    /// Acceptable source-doc stems (routing is correct if the #1 hit is one of these).
    accept: &'static [&'static str],
    /// Oracle (correct-sense) synonym/hypernym terms appended in the ORACLE arm.
    expand: &'static str,
    /// The lexical-semantic relation that *would* bridge query→doc (self-documenting;
    /// this is the eval set a future WordNet→edge importer must reproduce).
    bridge: &'static str,
}

fn probes() -> Vec<Probe> {
    vec![
        Probe {
            query: "fake passage to improve retrieval",
            accept: &["rag-architectures"],
            expand: "hypothetical document generated",
            bridge: "fake->hypothetical, passage->document (synonym)",
        },
        Probe {
            query: "trigger another lookup midway through generation",
            accept: &["rag-architectures"],
            expand: "adaptive iterative retrieval active",
            bridge: "trigger lookup->adaptive/iterative retrieval (synonym/hypernym)",
        },
        Probe {
            query: "shrink vectors cheaply on a cpu",
            accept: &["quantization-pareto", "embedding-evaluation-efficiency"],
            expand: "compress quantize quantization int8",
            bridge: "shrink->compress/quantize (synonym)",
        },
        Probe {
            query: "the narrow cone of raw transformer vectors",
            accept: &["representation-geometry"],
            expand: "anisotropy isotropy cosine degeneration",
            bridge: "narrow cone->anisotropy (synonym)",
        },
        Probe {
            query: "words that mean the same thing for expanding a search",
            accept: &["lexical-semantic-resources"],
            expand: "synonym synset wordnet query expansion",
            bridge: "same meaning->synonym/synset (definition)",
        },
        Probe {
            query: "a kind-of hierarchy over word meanings",
            accept: &["parts-of-speech-relations", "lexical-semantic-resources"],
            expand: "hypernymy hyponymy is-a taxonomy",
            bridge: "kind-of hierarchy->hypernymy (synonym)",
        },
        Probe {
            query: "opposite-meaning word pairs",
            accept: &["parts-of-speech-relations", "lexical-semantic-resources"],
            expand: "antonym antonymy opposition",
            bridge: "opposite-meaning->antonym (synonym)",
        },
        Probe {
            query: "shorten embedding length without retraining the model",
            accept: &["representation-geometry", "embedding-evaluation-efficiency"],
            expand: "matryoshka truncation nested dimension",
            bridge: "shorten length->matryoshka truncation (synonym/hypernym)",
        },
        Probe {
            query: "learn sentence vectors with no labels",
            accept: &["embedding-models", "representation-geometry"],
            expand: "unsupervised self-supervised contrastive simcse",
            bridge: "no labels->unsupervised/self-supervised (synonym)",
        },
        Probe {
            query: "where keyword search beats neural retrieval off-distribution",
            accept: &["dense-retrieval", "embedding-evaluation-efficiency"],
            expand: "bm25 beir distribution shift out-of-domain",
            bridge: "keyword search->bm25, off-distribution->distribution shift/BEIR",
        },
        Probe {
            query: "matching plain-English questions to source-code names",
            accept: &["code-embeddings", "lexical-semantic-resources"],
            expand: "identifier vocabulary mismatch code search",
            bridge: "plain-English<->code names->identifier vocabulary mismatch",
        },
        Probe {
            query: "scoring candidates a second time after the first pass",
            accept: &["rag-architectures", "dense-retrieval"],
            expand: "rerank reranker cross-encoder two-stage",
            bridge: "score a second time->rerank (synonym)",
        },
    ]
}

/// Append oracle expansion terms to a query (lexical query expansion).
fn expand_query(query: &str, expand: &str) -> String {
    format!("{query} {expand}")
}

/// Build a paragraph-granularity index of the corpus (the shipped Note unit).
/// Returns the index and the paragraph count.
fn build_para_index() -> Option<(Index, usize)> {
    let dir = corpus_dir()?;
    let mut files = Vec::new();
    collect_adoc(&dir, &mut files);
    files.sort();
    if files.is_empty() {
        return None;
    }

    let mut entries = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let name = stem(f);
        for (i, para) in paragraphs(&text).into_iter().enumerate() {
            let anchor = format!("{name}__p{i}");
            entries.push((
                PathBuf::from(format!("{anchor}.adoc")),
                format!("[[{anchor}]]\n{para}\n"),
            ));
        }
    }
    let n_paras = entries.len();
    let mut index = Index::default();
    index.ingest(entries);
    index.finalize();
    Some((index, n_paras))
}

#[cfg(feature = "dense")]
fn load_embedder() -> Option<aden_index::TractEmbedder> {
    let dir = std::env::var("ADEN_BGE_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".cache/aden-models/bge-small-en-v1.5")
        });
    if !dir.join("model.onnx").exists() {
        return None;
    }
    aden_index::TractEmbedder::from_dir(&dir).ok()
}

/// Top-1 source doc for a query under BM25 (the only arm available without dense).
fn route_bm25(index: &Index, query: &str) -> Option<String> {
    index
        .query(query)
        .into_iter()
        .next()
        .map(|r| doc_of(&r.anchor).to_string())
}

#[test]
#[ignore = "measurement harness, not a CI gate; reads an external prose corpus"]
fn vocab_mismatch_report() {
    // `index` is mutated only in the dense `embed_documents` path below.
    #[cfg_attr(not(feature = "dense"), allow(unused_mut))]
    let Some((mut index, n_paras)) = build_para_index() else {
        eprintln!("SKIP: prose corpus not found (set ADEN_PROSE_CORPUS)");
        return;
    };
    let probes = probes();
    let n = probes.len();

    #[cfg(feature = "dense")]
    let embedder = load_embedder();
    #[cfg(feature = "dense")]
    if let Some(e) = &embedder {
        index.embed_documents(e);
    }

    // HYBRID top-1 source doc (only meaningful with the dense feature + a model).
    let route_hybrid = |index: &Index, query: &str| -> Option<String> {
        #[cfg(feature = "dense")]
        if let Some(e) = &embedder {
            return index
                .hybrid_query(query, e)
                .into_iter()
                .next()
                .map(|r| doc_of(&r.anchor).to_string());
        }
        let _ = (index, query);
        None
    };
    let hybrid_available = cfg!(feature = "dense") && {
        #[cfg(feature = "dense")]
        {
            embedder.is_some()
        }
        #[cfg(not(feature = "dense"))]
        {
            false
        }
    };

    println!("\n=== Vocabulary-mismatch A/B (real prose corpus) ===");
    println!(
        "Corpus: {} paragraphs | {} probes | hybrid arm: {}",
        n_paras,
        n,
        if hybrid_available {
            "ON (bge model)"
        } else {
            "OFF (no model / no dense feature)"
        }
    );

    let in_accept = |doc: &Option<String>, accept: &[&str]| -> bool {
        doc.as_deref().is_some_and(|d| accept.contains(&d))
    };

    let (mut bm25_hits, mut oracle_hits, mut hybrid_hits) = (0usize, 0usize, 0usize);
    let mut bm25_misses: Vec<&str> = Vec::new();

    for p in &probes {
        let bm25 = route_bm25(&index, p.query);
        let oracle = route_bm25(&index, &expand_query(p.query, p.expand));
        let hybrid = if hybrid_available {
            route_hybrid(&index, p.query)
        } else {
            None
        };

        let b_ok = in_accept(&bm25, p.accept);
        let o_ok = in_accept(&oracle, p.accept);
        let h_ok = in_accept(&hybrid, p.accept);

        bm25_hits += b_ok as usize;
        oracle_hits += o_ok as usize;
        hybrid_hits += h_ok as usize;
        if !b_ok {
            bm25_misses.push(p.query);
        }

        let mark = |ok: bool| if ok { "OK  " } else { "MISS" };
        let hyb = if hybrid_available { mark(h_ok) } else { "--  " };
        println!(
            "  bm25 {} | oracle {} | hybrid {}   q: {}\n        (bridge: {})",
            mark(b_ok),
            mark(o_ok),
            hyb,
            p.query,
            p.bridge
        );
    }

    println!("\n  routing R@1:");
    println!("    BM25            {bm25_hits}/{n}");
    println!("    BM25 + ORACLE   {oracle_hits}/{n}   (lexical-semantic expansion, upper bound)");
    if hybrid_available {
        println!("    HYBRID (RRF)    {hybrid_hits}/{n}   (BM25 + dense)");
        println!(
            "\n  headroom for a graph layer (oracle - bm25):   {} probe(s)",
            oracle_hits as i64 - bm25_hits as i64
        );
        println!(
            "  DECISION number  (oracle - hybrid):           {} probe(s)  \
             <- net of what dense already captures",
            oracle_hits as i64 - hybrid_hits as i64
        );
    } else {
        println!(
            "\n  headroom for a graph layer (oracle - bm25):   {} probe(s)",
            oracle_hits as i64 - bm25_hits as i64
        );
        println!("  (run with --features dense + a bge model for the HYBRID decision number)");
    }

    if !bm25_misses.is_empty() {
        println!("\n  BM25 miss list (the addressable set a semantic-graph layer targets):");
        for q in &bm25_misses {
            println!("    - {q}");
        }
    }

    // Sanity only — this harness EXPOSES the gap, it does not gate on closing it.
    // Assert the corpus indexed and every arm ran over the full probe set.
    assert!(n_paras > 0, "corpus produced zero paragraphs");
    assert!(
        oracle_hits >= bm25_hits,
        "oracle expansion routed WORSE than baseline ({oracle_hits} < {bm25_hits}); \
         the hand-authored expansions are mis-sensed — review the probe set"
    );
}
