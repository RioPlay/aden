// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// OEWN corpus-grounding dry-run — the deterministic WSD gate (measurement harness,
// #[ignore]d, writes NOTHING to .aden/store).
//
// The real-OEWN extraction (`scripts/oewn_to_triples.py`) showed that an unscoped
// synonym import is a MIX of genuine bridges (store->keep, search->lookup) and
// wrong-sense noise (edge->sharpness, vector->virus, node->knob, secret->arcanum):
// Gonzalo's WSD penalty, on aden's own vocabulary. This harness applies the fix that
// needs no WSD model — CORPUS-GROUNDING: keep a dictionary edge only when BOTH endpoints
// appear in aden's real corpus vocabulary, tokenized by aden's OWN tokenizer (so stemming
// and stop-words match the index exactly). "sharpness"/"virus"/"arcanum" are in no symbol
// card, so those edges drop deterministically; "keep"/"lookup"/"node" survive. The output
// is the FILTERED edge set that a (sign-off-gated) producer would actually write.
//
// Inputs: the real gen'd store (default = aden repo; ADEN_REAL_CORPUS overrides) supplies
// the corpus vocab; the flat OEWN triples (ADEN_OEWN_TRIPLES, default
// ~/.cache/aden/dict/oewn-triples-flat.json) are the candidate edges.
// Run: cargo test -p aden-cli --test oewn_grounding -- --include-ignored --nocapture

use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn corpus() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("ADEN_REAL_CORPUS") {
        let p = PathBuf::from(d);
        return p.is_dir().then_some(p);
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.is_dir().then_some(root)
}

fn triples_path() -> PathBuf {
    std::env::var("ADEN_OEWN_TRIPLES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".cache/aden/dict/oewn-triples-flat.json")
        })
}

/// Mirror of `index_text` in `aden-cli/src/util.rs`: the exact text the index tokenizes.
fn index_text(doc: &aden_core::Document) -> String {
    aden_emit::emit_document(doc)
        .lines()
        .filter(|l| !l.trim_start().starts_with(":last-verified:"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The corpus vocabulary: every token aden's tokenizer emits across all store cards —
/// the SAME token space the BM25 index is built over.
fn corpus_vocab(repo: &Path) -> HashSet<String> {
    use aden_store::{GraphStorage, Storage};
    let mut vocab = HashSet::new();
    let root = aden_paths::resolve_root(repo);
    let (store_path, _) = aden_paths::resolve_read_store(&root);
    let Some(store_str) = store_path.to_str() else {
        return vocab;
    };
    let Ok(storage) = Storage::open_existing(store_str) else {
        return vocab;
    };
    let Ok(docs) = storage.get_all_documents() else {
        return vocab;
    };
    for doc in docs.values() {
        for tok in aden_index::tokenize(&index_text(doc)) {
            vocab.insert(tok);
        }
    }
    vocab
}

/// A phrase is grounded iff it yields at least one token and EVERY token is in the
/// corpus vocab (tokenize already lowercases, splits compounds, drops stop-words, stems —
/// so this matches the index's own view of the word).
fn grounded(phrase: &str, vocab: &HashSet<String>) -> bool {
    let toks = aden_index::tokenize(phrase);
    !toks.is_empty() && toks.iter().all(|t| vocab.contains(t))
}

#[test]
#[ignore = "deterministic WSD gate dry-run; reads gen'd store + OEWN triples; writes nothing"]
fn oewn_grounding_report() {
    let Some(repo) = corpus() else {
        eprintln!("SKIP: corpus dir not found (set ADEN_REAL_CORPUS)");
        return;
    };
    let tpath = triples_path();
    let Ok(raw) = std::fs::read_to_string(&tpath) else {
        eprintln!(
            "SKIP: OEWN triples not found at {} — run scripts/oewn_to_triples.py first",
            tpath.display()
        );
        return;
    };
    // Flat form: [[subject, edge, object, pos], ...].
    let triples: Vec<Vec<String>> =
        serde_json::from_str(&raw).expect("flat OEWN triples JSON parse");

    let vocab = corpus_vocab(&repo);
    if vocab.is_empty() {
        eprintln!(
            "SKIP: empty corpus vocab — run `aden gen` at {}",
            repo.display()
        );
        return;
    }

    println!("\n=== OEWN corpus-grounding (deterministic WSD gate, DRY RUN) ===");
    println!(
        "Corpus vocab: {} tokens | candidate OEWN edges: {}",
        vocab.len(),
        triples.len()
    );

    let mut kept: Vec<(&str, &str, &str)> = Vec::new();
    let mut dropped: Vec<(&str, &str, &str)> = Vec::new();
    for t in &triples {
        if t.len() < 3 {
            continue;
        }
        let (s, e, o) = (t[0].as_str(), t[1].as_str(), t[2].as_str());
        if grounded(s, &vocab) && grounded(o, &vocab) {
            kept.push((s, e, o));
        } else {
            dropped.push((s, e, o));
        }
    }

    let by_edge = |set: &[(&str, &str, &str)], edge: &str| -> usize {
        set.iter().filter(|(_, e, _)| *e == edge).count()
    };
    println!(
        "\n  KEPT {} / {}  (SynonymOf {}, IsA {}, PartOf {})   <- the edges a producer would write",
        kept.len(),
        triples.len(),
        by_edge(&kept, "SynonymOf"),
        by_edge(&kept, "IsA"),
        by_edge(&kept, "PartOf"),
    );
    println!(
        "  DROPPED {}  (wrong-sense / non-corpus endpoints — the WSD noise, filtered deterministically)",
        dropped.len()
    );

    println!("\n  -- surviving edges (grounded both endpoints) --");
    for (s, e, o) in &kept {
        println!("      {s:<14} --{e:<9}--> {o}");
    }

    println!("\n  -- sample of dropped noise (endpoint absent from the corpus) --");
    for (s, e, o) in dropped.iter().take(18) {
        let culprit = if !grounded(o, &vocab) { o } else { s };
        println!("      {s:<14} --{e:<9}--> {o:<18} (dropped: '{culprit}' not in corpus)");
    }

    assert!(!vocab.is_empty(), "no corpus vocab");
    assert!(
        kept.len() < triples.len(),
        "grounding kept everything — the filter is not discriminating"
    );
}
