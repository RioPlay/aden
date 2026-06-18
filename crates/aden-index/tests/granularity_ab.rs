// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// One-off granularity A/B (NOT a committed gate — a measurement harness).
//
// Question: how much does chunk granularity + heading structure change prose
// retrieval? Isolates the variables by indexing the SAME real prose three ways
// and running the SAME queries:
//   * doc-level  — one index entry per document (today's .txt behaviour)
//   * para-level — one entry per blank-line paragraph (the paragraph-Note unit)
//   * head-level — paragraph + its heading breadcrumb, the breadcrumb term-boosted
//                  (a crude BM25F: heading terms weighted higher, and a paragraph
//                  inherits its section's keywords even when its body omits them)
//
// Reports routing R@1 (does the top hit come from an acceptable source doc) and
// context density (chars of the #1 result — the immediacy win).
//
// Real corpus, natural queries authored from content knowledge (overfit caveat:
// small, hand-authored set; illustrative, not a leaderboard).
// Default corpus: ~/Projects/AI Research/docs (override ADEN_PROSE_CORPUS).
// Run: cargo test -p aden-index --features dense --test granularity_ab -- --include-ignored --nocapture

use aden_index::Index;
use std::collections::HashMap;
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

/// Recognize a single-line AsciiDoc-style heading paragraph: `== Title` etc.
/// Returns (level, title). This stands in for the typography recognizer the
/// plain-text path would run (Setext underlines, Title-Case lines, numbered
/// headers); here the corpus already marks headings with `=`, so we read those.
fn parse_heading(para: &str) -> Option<(usize, String)> {
    if para.lines().count() != 1 {
        return None;
    }
    let t = para.trim_start();
    let eqs = t.chars().take_while(|c| *c == '=').count();
    if eqs == 0 || eqs > 6 || !t[eqs..].starts_with(' ') {
        return None;
    }
    let title = t[eqs..].trim();
    (!title.is_empty()).then(|| (eqs, title.to_string()))
}

/// Paragraph entries carrying a term-boosted heading breadcrumb.
/// Returns (anchor, indexed_text, body_char_len). Heading-only paragraphs are
/// folded into the breadcrumb rather than indexed as their own nodes.
fn heading_entries(name: &str, text: &str) -> Vec<(String, String, usize)> {
    let mut out = Vec::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut idx = 0usize;
    for para in paragraphs(text) {
        if let Some((level, title)) = parse_heading(&para) {
            stack.retain(|(l, _)| *l < level);
            stack.push((level, title));
            continue;
        }
        let crumb = stack
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        // Breadcrumb repeated x2 ahead of the body — a crude BM25F field boost.
        let indexed = if crumb.is_empty() {
            para.clone()
        } else {
            format!("{crumb} {crumb}\n{para}")
        };
        out.push((format!("{name}__h{idx}"), indexed, para.len()));
        idx += 1;
    }
    out
}

struct Ab {
    doc_index: Index,
    para_index: Index,
    head_index: Index,
    doc_meta: HashMap<String, (String, usize)>,
    para_meta: HashMap<String, (String, usize)>,
    head_meta: HashMap<String, (String, usize)>,
    n_docs: usize,
    n_paras: usize,
}

fn build() -> Option<Ab> {
    let dir = corpus_dir()?;
    let mut files = Vec::new();
    collect_adoc(&dir, &mut files);
    files.sort();
    if files.is_empty() {
        return None;
    }

    let mut doc_entries = Vec::new();
    let mut para_entries = Vec::new();
    let mut head_entries = Vec::new();
    let mut doc_meta = HashMap::new();
    let mut para_meta = HashMap::new();
    let mut head_meta = HashMap::new();

    for f in &files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let name = stem(f);
        // These docs use `==`/`xref:`, not `[[..]]`, so the prepended anchor is
        // the only one parse_adoc resolves — every arm shares the identical parse
        // path, differing only in chunk size / heading boost.
        doc_entries.push((
            PathBuf::from(format!("{name}.adoc")),
            format!("[[{name}]]\n{text}\n"),
        ));
        doc_meta.insert(name.clone(), (name.clone(), text.len()));

        for (i, para) in paragraphs(&text).into_iter().enumerate() {
            let anchor = format!("{name}__p{i}");
            para_entries.push((
                PathBuf::from(format!("{anchor}.adoc")),
                format!("[[{anchor}]]\n{para}\n"),
            ));
            para_meta.insert(anchor, (name.clone(), para.len()));
        }

        for (anchor, indexed, blen) in heading_entries(&name, &text) {
            head_entries.push((
                PathBuf::from(format!("{anchor}.adoc")),
                format!("[[{anchor}]]\n{indexed}\n"),
            ));
            head_meta.insert(anchor, (name.clone(), blen));
        }
    }

    let n_docs = doc_entries.len();
    let n_paras = para_entries.len();

    let mk = |entries| {
        let mut ix = Index::default();
        ix.ingest(entries);
        ix.finalize();
        ix
    };

    Some(Ab {
        doc_index: mk(doc_entries),
        para_index: mk(para_entries),
        head_index: mk(head_entries),
        doc_meta,
        para_meta,
        head_meta,
        n_docs,
        n_paras,
    })
}

struct Q {
    text: &'static str,
    accept: &'static [&'static str],
}

fn queries() -> Vec<Q> {
    vec![
        Q {
            text: "why does cosine similarity fail on raw transformer embeddings",
            accept: &["representation-geometry"],
        },
        Q {
            text: "build a knowledge graph from a corpus for multi hop reasoning",
            accept: &["rag-architectures"],
        },
        Q {
            text: "cheap way to compress vectors on a cpu without much quality loss",
            accept: &["quantization-pareto", "embedding-evaluation-efficiency"],
        },
        Q {
            text: "learn sentence embeddings with no labeled data",
            accept: &["embedding-models", "representation-geometry"],
        },
        Q {
            text: "fast approximate nearest neighbor index that fits in memory",
            accept: &["dense-retrieval"],
        },
        Q {
            text: "trigger another retrieval in the middle of generation",
            accept: &["rag-architectures"],
        },
        Q {
            text: "shrink embedding dimensions at inference without retraining",
            accept: &["representation-geometry", "embedding-evaluation-efficiency"],
        },
        Q {
            text: "does one bit binary quantization break down at very large scale",
            accept: &["quantization-pareto"],
        },
        Q {
            text: "benchmark where bm25 beats dense retrievers out of domain",
            accept: &["dense-retrieval", "embedding-evaluation-efficiency"],
        },
        Q {
            text: "prompt a model to write a fake passage to improve retrieval",
            accept: &["rag-architectures"],
        },
    ]
}

/// Source doc stem from any chunk anchor (`name`, `name__pN`, `name__hN`).
fn doc_of(anchor: &str) -> &str {
    anchor.split("__").next().unwrap_or(anchor)
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

#[test]
#[ignore = "measurement harness, not a CI gate; reads an external prose corpus"]
fn granularity_ab_report() {
    // `ab` is mutated only in the dense `embed_documents` path below.
    #[cfg_attr(not(feature = "dense"), allow(unused_mut))]
    let Some(mut ab) = build() else {
        eprintln!("SKIP: prose corpus not found (set ADEN_PROSE_CORPUS)");
        return;
    };
    let qs = queries();

    #[cfg(feature = "dense")]
    let embedder = load_embedder();
    #[cfg(feature = "dense")]
    if let Some(e) = &embedder {
        ab.doc_index.embed_documents(e);
        ab.para_index.embed_documents(e);
        ab.head_index.embed_documents(e);
    }

    let rank = |index: &Index, q: &str, hybrid: bool| -> Option<String> {
        #[cfg(feature = "dense")]
        if hybrid && let Some(e) = &embedder {
            return index
                .hybrid_query(q, e)
                .into_iter()
                .next()
                .map(|r| r.anchor);
        }
        let _ = hybrid;
        index.query(q).into_iter().next().map(|r| r.anchor)
    };

    println!("\n=== Granularity + heading A/B (real prose corpus) ===");
    println!(
        "Corpus: {} docs, {} paragraphs | {} queries",
        ab.n_docs,
        ab.n_paras,
        qs.len()
    );

    let modes: &[(&str, bool)] = &[
        ("BM25", false),
        #[cfg(feature = "dense")]
        ("HYBRID", true),
    ];

    for (label, use_hybrid) in modes {
        let (mut dh, mut ph, mut hh) = (0usize, 0usize, 0usize);
        let (mut dsz, mut psz, mut hsz) = (0usize, 0usize, 0usize);

        println!("\n----- {label} -----");
        for q in &qs {
            let d = rank(&ab.doc_index, q.text, *use_hybrid);
            let p = rank(&ab.para_index, q.text, *use_hybrid);
            let h = rank(&ab.head_index, q.text, *use_hybrid);

            let d_ok = d
                .as_deref()
                .map(doc_of)
                .is_some_and(|x| q.accept.contains(&x));
            let p_ok = p
                .as_deref()
                .map(doc_of)
                .is_some_and(|x| q.accept.contains(&x));
            let h_ok = h
                .as_deref()
                .map(doc_of)
                .is_some_and(|x| q.accept.contains(&x));

            dh += d_ok as usize;
            ph += p_ok as usize;
            hh += h_ok as usize;
            dsz += d
                .as_deref()
                .and_then(|a| ab.doc_meta.get(a))
                .map_or(0, |(_, s)| *s);
            psz += p
                .as_deref()
                .and_then(|a| ab.para_meta.get(a))
                .map_or(0, |(_, s)| *s);
            hsz += h
                .as_deref()
                .and_then(|a| ab.head_meta.get(a))
                .map_or(0, |(_, s)| *s);

            let mark = |ok: bool| if ok { "OK  " } else { "MISS" };
            println!(
                "  doc {} | para {} | head {}   q: {}",
                mark(d_ok),
                mark(p_ok),
                mark(h_ok),
                q.text
            );
        }

        let n = qs.len();
        println!("  [{label}] routing R@1:  doc {dh}/{n}   para {ph}/{n}   head {hh}/{n}");
        println!(
            "  [{label}] avg #1 size:  doc {} chars   para {} chars   head {} chars",
            dsz / n,
            psz / n,
            hsz / n
        );
    }
}
