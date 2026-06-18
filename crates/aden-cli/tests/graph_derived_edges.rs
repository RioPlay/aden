// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Graph-derived relationships — let the corpus define meaning, not the dictionary
// (measurement harness, #[ignore]d, writes NOTHING to .aden/store).
//
// The thesis (user, 2026-06-17): don't take the dictionary's word for it — have aden's
// own graph redefine relationships from how terms ACTUALLY connect in the corpus. This
// is descriptive (distributional) semantics vs OEWN's prescriptive (lexicographic) ones:
// "you shall know a word by the company it keeps." For each term we compute its real
// neighbors from the term<->card bipartite graph via PPMI (positive pointwise mutual
// information) over co-occurrence — fully deterministic, MODEL-FREE (no bge in the loop),
// which is aden's identity. First-order co-occurrence yields AssociatedWith (topical
// association: secret~credential); second-order context similarity (the embedding /
// mutual-kNN follow-up) would yield SynonymOf-grade SimilarTo. This harness does the
// first and prints it HEAD-TO-HEAD against OEWN so the domain-vs-generic gap is visible:
// OEWN says node->knob; the graph says node->edge/anchor/traversal.
//
// Inputs: the real gen'd store (default = aden repo; ADEN_REAL_CORPUS overrides) and,
// for the side-by-side, the OEWN wordset JSON (ADEN_OEWN_WORDSET, default
// ~/.cache/aden/dict/oewn-triples.json). Writes graph-derived candidate edges to
// ADEN_GRAPH_EDGES_OUT (default ~/.cache/aden/dict/graph-derived-edges.json).
// Run: cargo test -p aden-cli --test graph_derived_edges -- --include-ignored --nocapture

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const SEEDS: &[&str] = &[
    "secret", "node", "edge", "token", "vector", "score", "rank", "store", "search", "save",
    "merge", "cluster", "group", "split", "resolve", "fetch", "index", "graph", "anchor", "orphan",
    "neighbor", "distance",
];
const MIN_DF: usize = 3; // ignore terms in fewer than 3 cards (noise)
const MAX_DF_FRAC: f64 = 0.20; // ignore terms in >20% of cards (uninformative boilerplate)
const MIN_CO: usize = 3; // a pair must co-occur in >=3 cards to count
const TOPK: usize = 8;

fn corpus() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("ADEN_REAL_CORPUS") {
        let p = PathBuf::from(d);
        return p.is_dir().then_some(p);
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.is_dir().then_some(root)
}

fn index_text(doc: &aden_core::Document) -> String {
    aden_emit::emit_document(doc)
        .lines()
        .filter(|l| !l.trim_start().starts_with(":last-verified:"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Per-card unique token sets — the term<->card bipartite graph the PPMI is computed over.
fn card_token_sets(repo: &Path) -> Vec<HashSet<String>> {
    use aden_store::{GraphStorage, Storage};
    let mut out = Vec::new();
    let root = aden_paths::resolve_root(repo);
    let (store_path, _) = aden_paths::resolve_read_store(&root);
    let Some(store_str) = store_path.to_str() else {
        return out;
    };
    let Ok(storage) = Storage::open_existing(store_str) else {
        return out;
    };
    let Ok(docs) = storage.get_all_documents() else {
        return out;
    };
    for doc in docs.values() {
        let toks: HashSet<String> = aden_index::tokenize(&index_text(doc)).into_iter().collect();
        if !toks.is_empty() {
            out.push(toks);
        }
    }
    out
}

/// OEWN neighbors (synonyms + hypernyms) for `seed`, for the side-by-side. Empty when the
/// dictionary has no entry — itself informative (the graph knows code terms OEWN doesn't).
fn oewn_neighbors(wordset: &serde_json::Value, seed: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(entry) = wordset
        .get(seed)
        .and_then(|e| e.get("meanings"))
        .and_then(|m| m.as_array())
    {
        for meaning in entry {
            for key in ["synonyms", "hypernyms"] {
                if let Some(arr) = meaning.get(key).and_then(|a| a.as_array()) {
                    out.extend(arr.iter().filter_map(|v| v.as_str().map(str::to_string)));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[test]
#[ignore = "graph-derived-relationships dry-run; reads gen'd store; writes nothing to store"]
fn graph_derived_edges_report() {
    let Some(repo) = corpus() else {
        eprintln!("SKIP: corpus dir not found (set ADEN_REAL_CORPUS)");
        return;
    };
    let cards = card_token_sets(&repo);
    let n = cards.len();
    if n == 0 {
        eprintln!(
            "SKIP: no store cards — run `aden gen` at {}",
            repo.display()
        );
        return;
    }

    // Document frequency + postings (term -> card indices).
    let mut postings: HashMap<&str, Vec<usize>> = HashMap::new();
    for (ci, toks) in cards.iter().enumerate() {
        for t in toks {
            postings.entry(t.as_str()).or_default().push(ci);
        }
    }
    let df = |t: &str| postings.get(t).map_or(0, |v| v.len());
    let max_df = (MAX_DF_FRAC * n as f64) as usize;

    let wordset: serde_json::Value = std::env::var("ADEN_OEWN_WORDSET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".cache/aden/dict/oewn-triples.json")
        })
        .pipe(std::fs::read_to_string)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);

    println!("\n=== Graph-derived relationships vs OEWN (PPMI over {n} cards, MODEL-FREE) ===");
    println!("Vocab terms: {} | seeds: {}\n", postings.len(), SEEDS.len());

    let mut edges: Vec<(String, String, f64, usize)> = Vec::new();

    for &seed in SEEDS {
        let Some(seed_cards) = postings.get(seed) else {
            println!("  {seed:<10} (absent from corpus — no symbol card uses it)");
            continue;
        };
        let df_seed = seed_cards.len();

        // Co-occurrence: how many seed-cards each other term shares.
        let mut co: HashMap<&str, usize> = HashMap::new();
        for &ci in seed_cards {
            for t in &cards[ci] {
                if t.as_str() != seed {
                    *co.entry(t.as_str()).or_default() += 1;
                }
            }
        }

        // PPMI rank.
        let mut scored: Vec<(&str, f64, usize)> = co
            .into_iter()
            .filter(|&(t, c)| c >= MIN_CO && (MIN_DF..=max_df).contains(&df(t)))
            .filter_map(|(t, c)| {
                let pmi = ((c * n) as f64 / (df_seed * df(t)) as f64).log2();
                (pmi > 0.0).then_some((t, pmi, c))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap()
                .then(b.2.cmp(&a.2))
                .then(a.0.cmp(b.0))
        });
        scored.truncate(TOPK);

        let graph_str = scored
            .iter()
            .map(|(t, pmi, _)| format!("{t}({pmi:.1})"))
            .collect::<Vec<_>>()
            .join(", ");
        let oewn = oewn_neighbors(&wordset, seed);
        let oewn_str = if oewn.is_empty() {
            "(none)".into()
        } else {
            oewn.join(", ")
        };

        println!("  {seed}");
        println!("    OEWN  : {oewn_str}");
        println!("    GRAPH : {graph_str}");

        for (t, pmi, c) in scored {
            edges.push((seed.to_string(), t.to_string(), pmi, c));
        }
    }

    let out = std::env::var("ADEN_GRAPH_EDGES_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".cache/aden/dict/graph-derived-edges.json")
        });
    let json: Vec<serde_json::Value> = edges
        .iter()
        .map(|(s, t, pmi, c)| {
            serde_json::json!({"subject": s, "edge": "AssociatedWith", "object": t,
                               "ppmi": pmi, "cooccur": c})
        })
        .collect();
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        &out,
        serde_json::to_string_pretty(&json).unwrap_or_default(),
    );

    println!(
        "\n  Graph-derived candidate edges: {} -> {}",
        edges.len(),
        out.display()
    );
    println!(
        "  (AssociatedWith, corpus-derived; SynonymOf-grade SimilarTo is the embedding follow-up)"
    );
    println!("  DRY RUN: nothing written to .aden/store.");

    assert!(n > 0 && !postings.is_empty(), "empty corpus");
    assert!(
        !edges.is_empty(),
        "no graph-derived edges — PPMI thresholds too strict?"
    );
}

/// Tiny pipe helper so the wordset load reads top-to-bottom.
trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}
impl<T> Pipe for T {}
