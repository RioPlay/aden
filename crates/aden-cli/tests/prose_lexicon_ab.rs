// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Dictionary-on-PROSE A/B (measurement harness, #[ignore]d, BM25-only, writes nothing).
//
// The lexical-merge ablation tested dictionaries on CODE retrieval, which is the wrong
// domain: code synonymy is corpus-specific identifier vocabulary, not English dictionary
// synonymy. Dictionaries are for PROSE understanding. This harness gives them their proper
// test: a neutral external prose corpus (Wikipedia intros), in the regime where a dictionary
// should matter most, namely BM25-only (no dense embeddings to already bridge paraphrase).
//
// Fair probe construction (automated, no hand-picking): for each article whose title is a
// concept C, find an OEWN synonym S that is ABSENT from C's article text. The probe is
// (query = S, gold = C). Baseline BM25(S) cannot match C (S is not in it); only a dictionary
// expansion of S (which, by symmetric synonymy, contains C) can bridge S -> C. This is exactly
// the prose-understanding job a dictionary exists for.
//
// Arms (BM25-only): BASELINE, +OEWN, +MOBY, +UNION, +AGREE2 (cross-source), ORACLE (S + C).
// Inputs: ADEN_PROSE_CORPUS (default ~/.cache/aden/prose-eval/corpus), OEWN + Moby stores.
// Run: cargo test -p aden-cli --test prose_lexicon_ab -- --include-ignored --nocapture

use aden_core::EdgeType;
use aden_index::Index;
use aden_store::{GraphStorage, Storage};
use std::collections::HashSet;
use std::path::PathBuf;

fn home_join(rest: &str) -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest)
}

fn corpus_dir() -> PathBuf {
    std::env::var("ADEN_PROSE_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_join(".cache/aden/prose-eval/corpus"))
}

fn store_at(env: &str, default: &str) -> Option<Storage> {
    let p = std::env::var(env)
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_join(default));
    Storage::open_existing(p.to_str().unwrap_or_default()).ok()
}

/// Single-word SynonymOf lemmas of `word` from a source namespace.
fn syns(store: &Storage, source: &str, word: &str) -> Vec<String> {
    store
        .get_outgoing_edges(&format!("aden://term/{source}/{word}"))
        .map(|es| {
            es.into_iter()
                .filter(|(_, et)| matches!(et, EdgeType::SynonymOf))
                .map(|(t, _)| t.rsplit('/').next().unwrap_or(&t).to_string())
                .filter(|l| !l.contains(' ') && l.len() >= 3)
                .collect()
        })
        .unwrap_or_default()
}

/// A lemma is grounded iff it tokenizes entirely into the corpus vocabulary.
fn grounded(lemma: &str, vocab: &HashSet<String>) -> bool {
    let t = aden_index::tokenize(lemma);
    !t.is_empty() && t.iter().all(|x| vocab.contains(x))
}

/// Grounded synonym expansion of `word` from one source (capped).
fn expand(
    store: Option<&Storage>,
    source: &str,
    word: &str,
    vocab: &HashSet<String>,
) -> Vec<String> {
    let Some(s) = store else {
        return Vec::new();
    };
    syns(s, source, word)
        .into_iter()
        .filter(|l| grounded(l, vocab))
        .take(8)
        .collect()
}

fn union(a: &[String], b: &[String]) -> Vec<String> {
    let mut out = a.to_vec();
    for x in b {
        if !out.contains(x) {
            out.push(x.clone());
        }
    }
    out
}

fn agree2(a: &[String], b: &[String]) -> Vec<String> {
    a.iter().filter(|x| b.contains(x)).cloned().collect()
}

fn top(index: &Index, q: &str) -> Option<String> {
    index.query(q).into_iter().next().map(|r| r.anchor)
}

/// Build a BM25 index over the external prose corpus. Each `.txt` becomes one document
/// anchored by its filename stem (the concept). Returns (index, vocab, [(concept, tokens)]).
fn build_index() -> Option<(Index, HashSet<String>, Vec<(String, HashSet<String>)>)> {
    let dir = corpus_dir();
    let rd = std::fs::read_dir(&dir).ok()?;
    let mut entries: Vec<(PathBuf, String)> = Vec::new();
    let mut vocab: HashSet<String> = HashSet::new();
    let mut articles: Vec<(String, HashSet<String>)> = Vec::new();
    for ent in rd.flatten() {
        let path = ent.path();
        if path.extension().and_then(|e| e.to_str()) != Some("txt") {
            continue;
        }
        let Some(slug) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let toks: HashSet<String> = aden_index::tokenize(&text).into_iter().collect();
        vocab.extend(toks.iter().cloned());
        // Wrap as AsciiDoc so the index anchors the article by its concept slug.
        let adoc = format!("[[{slug}]]\n= {slug}\n\n{text}\n");
        entries.push((PathBuf::from(format!("{slug}.adoc")), adoc));
        articles.push((slug, toks));
    }
    if entries.is_empty() {
        return None;
    }
    let mut index = Index::default();
    index.ingest(entries);
    index.finalize();
    Some((index, vocab, articles))
}

#[test]
#[ignore = "dictionary-on-prose A/B; reads external prose corpus + lexicon stores; writes nothing"]
fn prose_lexicon_report() {
    let Some((index, vocab, articles)) = build_index() else {
        eprintln!(
            "SKIP: prose corpus not found at {} (run /tmp/fetch_prose.py)",
            corpus_dir().display()
        );
        return;
    };
    let Some(oewn) = store_at("ADEN_LEXICON_STORE", ".cache/aden/lexicon") else {
        eprintln!("SKIP: OEWN store not built");
        return;
    };
    let moby = store_at("ADEN_MOBY_STORE", ".cache/aden/moby");

    // Auto-construct probes: query = an OEWN synonym S absent from concept C's article.
    let mut probes: Vec<(String, String)> = Vec::new();
    for (c, toks) in &articles {
        if c.contains('_') {
            continue; // multi-word concept has no single OEWN lemma
        }
        for s in syns(&oewn, "oewn", c) {
            if &s == c {
                continue;
            }
            // S must be ABSENT from C's article (so BM25 baseline cannot match C via S).
            if aden_index::tokenize(&s).iter().any(|t| toks.contains(t)) {
                continue;
            }
            probes.push((s, c.clone()));
            break;
        }
    }

    let n = probes.len();
    println!(
        "\n=== Dictionary-on-PROSE A/B ({} articles, {} vocab, {n} synonym-bridge probes, BM25-only) ===",
        articles.len(),
        vocab.len()
    );

    let (mut base, mut oe, mut mo, mut un, mut ag, mut orc) = (0, 0, 0, 0, 0, 0);
    for (s, c) in &probes {
        let oewn_x = expand(Some(&oewn), "oewn", s, &vocab);
        let moby_x = expand(moby.as_ref(), "moby", s, &vocab);
        let union_x = union(&oewn_x, &moby_x);
        let agree_x = agree2(&oewn_x, &moby_x);
        let hitc = |q: &str| {
            top(&index, q)
                .as_deref()
                .is_some_and(|a| a.contains(c.as_str()))
        };
        let q = |xs: &[String]| {
            if xs.is_empty() {
                s.clone()
            } else {
                format!("{s} {}", xs.join(" "))
            }
        };
        let (b, o, m, u, a, r) = (
            hitc(s),
            hitc(&q(&oewn_x)),
            hitc(&q(&moby_x)),
            hitc(&q(&union_x)),
            hitc(&q(&agree_x)),
            hitc(&format!("{s} {c}")),
        );
        base += b as usize;
        oe += o as usize;
        mo += m as usize;
        un += u as usize;
        ag += a as usize;
        orc += r as usize;
        let mk = |x: bool| if x { "OK " } else { "-- " };
        println!(
            "  base {} oewn {} moby {} union {} agr2 {} oracle {}   '{s}' -> {c}",
            mk(b),
            mk(o),
            mk(m),
            mk(u),
            mk(a),
            mk(r)
        );
    }

    println!("\n  -- prose R@1 (synonym-bridge, BM25-only) --");
    println!("    BASELINE   {base}/{n}");
    println!(
        "    +OEWN      {oe}/{n}   (lift {:+})",
        oe as i64 - base as i64
    );
    println!(
        "    +MOBY      {mo}/{n}   (lift {:+})",
        mo as i64 - base as i64
    );
    println!(
        "    +UNION     {un}/{n}   (lift {:+})",
        un as i64 - base as i64
    );
    println!(
        "    +AGREE2    {ag}/{n}   (lift {:+})",
        ag as i64 - base as i64
    );
    println!("    ORACLE     {orc}/{n}   (S + the gold concept word)");

    assert!(n > 0, "no probes constructed");
}
