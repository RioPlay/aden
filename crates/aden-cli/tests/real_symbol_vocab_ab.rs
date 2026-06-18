// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Real-symbol vocabulary-mismatch A/B — THE CONFIRMATION GATE (measurement harness,
// #[ignore]d, reads a real gen'd store).
//
// WHY THIS EXISTS: the code-corpus oracle A/B (`aden-index/tests/code_vocab_mismatch_ab.rs`)
// found that hand-authored synonym expansion beat the existing BM25+dense+RRF stack by
// +3/12 (25 pp) on code symbol cards — the number that flips the call toward building a
// dictionary-populated `SynonymOf` edge layer. But that result rests on 18 HAND-GLOSSED
// cards whose text the author wrote, which the research brief flags as "a soft ceiling,
// not a guarantee." Before any store-writing importer is built, the decision must be
// reproduced on REAL `aden gen` symbol cards — the actual signature+contract text the
// retrieval path indexes, which NO ONE hand-tuned to the queries.
//
// WHAT CHANGES vs. the toy harness: the corpus is loaded from a real fjall store via the
// SAME path production retrieval uses — `collect_store_entries` + `index_text` in
// `aden-cli/src/util.rs` (replicated here; that function is private to the binary crate).
// Each card's indexed text is `aden_emit::emit_document(stored_contract)`, i.e. exactly
// what `aden ask`/`aden search` see. The only authored inputs are the probe QUERIES and
// their gold target symbols — and those are deliberately phrased in SYNONYMS the real
// card does not contain, so the retriever must bridge the gap.
//
// HONEST RESIDUAL CAVEAT: the queries here are authored by someone who has seen aden's
// symbols, so this closes overfit risk #1 (tuned card text) but only mitigates risk #2
// (queries drawn from the symbol's own vocabulary) by discipline. The STRONGEST run
// points `ADEN_REAL_CORPUS` at an independently-authored external repo that has been
// `aden gen`'d (e.g. a Flask/Django checkout) with queries written from general API
// knowledge. This same harness shape serves both; only the probe set is aden-specific.
//
// Three arms over the SAME real index and SAME queries (identical to the toy harness):
//   * BM25            — index.query (the tokenizer already split the identifiers).
//   * BM25 + ORACLE   — query + correct-sense synonym expansion. Gonzalo et al. 1998's
//                       *oracle* upper bound for a SynonymOf layer.
//   * HYBRID          — index.hybrid_query (BM25 + bge-small dense via RRF).
// DECISION number = oracle - hybrid (lift NET of what dense already recovers). The gate
// passes the "build it" bar if this stays materially positive (toy corpus: +3/12).
//
// Requires `aden gen <repo>` to have populated the store first (the read auto-reindexes
// source, but the symbol cards are store-first — `Index::from_directory` alone won't see
// them). Default corpus: the aden repo itself (its store is always populated).
// Run: cargo test -p aden-cli --features dense --test real_symbol_vocab_ab -- --include-ignored --nocapture

use aden_index::Index;
use std::path::PathBuf;

/// The gen'd repo whose store supplies the symbol cards. `ADEN_REAL_CORPUS` overrides;
/// default is the aden workspace root (its own store is guaranteed populated).
fn corpus() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("ADEN_REAL_CORPUS") {
        let p = PathBuf::from(d);
        return p.is_dir().then_some(p);
    }
    // crates/aden-cli -> workspace root.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.is_dir().then_some(root)
}

/// Emit a stored `Document` as the search-index text, with the volatile
/// `:last-verified:` timestamp dropped — a faithful mirror of `index_text` in
/// `aden-cli/src/util.rs` (the production indexer). Kept in sync by hand because that
/// function is private to the binary crate; if it changes, change this too.
fn index_text(doc: &aden_core::Document) -> String {
    aden_emit::emit_document(doc)
        .lines()
        .filter(|l| !l.trim_start().starts_with(":last-verified:"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Load every symbol card from the repo's fjall store as `(synthetic_path, indexed_text)`
/// entries — a faithful mirror of `collect_store_entries` in `aden-cli/src/util.rs`,
/// including its deterministic (anchor, source_file) sort so the index build (and any
/// anchor-collision winner) is reproducible. Empty when no store exists yet.
fn load_real_cards(repo: &std::path::Path) -> Vec<(PathBuf, String)> {
    use aden_store::{GraphStorage, Storage};

    let root = aden_paths::resolve_root(repo);
    let (store_path, _) = aden_paths::resolve_read_store(&root);
    if !store_path.is_dir() {
        return Vec::new();
    }
    let Some(store_str) = store_path.to_str() else {
        return Vec::new();
    };
    let Ok(storage) = Storage::open_existing(store_str) else {
        return Vec::new();
    };
    let Ok(docs) = storage.get_all_documents() else {
        return Vec::new();
    };
    let mut docs: Vec<_> = docs.into_values().collect();
    docs.sort_by(|a, b| {
        a.anchor.cmp(&b.anchor).then_with(|| {
            a.attributes
                .get("source_file")
                .cmp(&b.attributes.get("source_file"))
        })
    });
    docs.into_iter()
        .map(|doc| {
            let synthetic = doc
                .attributes
                .get("source_file")
                .cloned()
                .unwrap_or_else(|| doc.anchor.clone());
            (PathBuf::from(synthetic), index_text(&doc))
        })
        .collect()
}

/// Build the BM25 index over the real store cards. Returns `(index, card_count)`, or
/// `None` when the store is absent/empty (the harness then SKIPs rather than failing).
fn build_index(repo: &std::path::Path) -> Option<(Index, usize)> {
    let cards = load_real_cards(repo);
    if cards.is_empty() {
        return None;
    }
    let n = cards.len();
    let mut index = Index::default();
    index.ingest(cards);
    index.finalize();
    Some((index, n))
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

/// One synonym-mismatch probe over the real symbol corpus.
struct Probe {
    /// NL-ish query phrased in SYNONYMS of the target's tokens, not the tokens
    /// themselves — the gap a `SynonymOf` layer would bridge.
    query: &'static str,
    /// Distinctive symbol-name substring(s); routing is correct when the #1 hit's
    /// anchor contains one of these. (Anchors are full `aden://…#symbol` URIs, so a
    /// distinctive name uniquely identifies the card regardless of crate/path.)
    accept: &'static [&'static str],
    /// Oracle (correct-sense) expansion appended in the ORACLE arm — the symbol's own
    /// vocabulary, i.e. the synonym edge a WordNet/OEWN importer would have to assert.
    expand: &'static str,
    /// The bridge the expansion stands in for (self-documenting; this is the eval set
    /// a real importer must reproduce).
    bridge: &'static str,
}

/// 12 probes against real aden symbols (names/roles grounded in source read this
/// session; the indexed card text is whatever `aden gen` emitted, NOT authored here).
fn probes() -> Vec<Probe> {
    vec![
        Probe {
            query: "store a batch of relationships between nodes in one operation",
            accept: &["put_edges_bulk"],
            expand: "append bulk typed edges deduplicate",
            bridge: "relationships->edges, one operation->bulk",
        },
        Probe {
            query: "group the graph into clusters of tightly connected nodes",
            accept: &["detect_communities"],
            expand: "community detection louvain modularity",
            bridge: "group into clusters->community detection",
        },
        Probe {
            query: "blend two ranked result lists into a single ordering",
            accept: &["rrf_fuse"],
            expand: "reciprocal rank fusion combine rankings",
            bridge: "blend ranked lists->reciprocal rank fusion",
        },
        Probe {
            query: "how aligned are two embedding vectors",
            accept: &["cosine_similarity"],
            expand: "cosine similarity vector",
            bridge: "how aligned->cosine similarity",
        },
        Probe {
            query: "fewest single character edits to turn one word into another",
            accept: &["levenshtein_distance"],
            expand: "levenshtein edit distance",
            bridge: "single character edits->levenshtein edit distance",
        },
        Probe {
            query: "figure out which definition a function call points to",
            accept: &["resolve_callee"],
            expand: "resolve callee definition anchor",
            bridge: "function call points to->resolve callee definition",
        },
        Probe {
            query: "decide what category of question the user is asking",
            accept: &["classify_intent"],
            expand: "classify intent query category",
            bridge: "category of question->classify intent",
        },
        Probe {
            query: "detect a leaked password or api key inside text",
            accept: &["content_has_high_confidence_secret"],
            expand: "secret credential api key detection",
            bridge: "leaked password/api key->secret/credential detection",
        },
        Probe {
            query: "collect the nodes surrounding a starting symbol up to some depth",
            accept: &["build_neighborhood"],
            expand: "neighborhood traversal depth graph",
            bridge: "nodes surrounding a start->neighborhood traversal",
        },
        Probe {
            query: "find everything that points at a given node",
            accept: &["get_incoming_edges"],
            expand: "incoming edges backlinks callers references",
            bridge: "points at a node->incoming edges/backlinks",
        },
        Probe {
            query: "how many tokens were avoided versus reading whole files",
            accept: &["SavingsEstimate"],
            expand: "savings estimate tokens baseline bytes",
            bridge: "tokens avoided->savings estimate",
        },
        Probe {
            query: "anchors in the graph that nothing else references",
            accept: &["scan_orphans"],
            expand: "scan orphan anchors unreferenced dangling",
            bridge: "nothing references->orphan/unreferenced",
        },
    ]
}

/// Append oracle expansion terms to a query (lexical query expansion).
fn expand_query(query: &str, expand: &str) -> String {
    format!("{query} {expand}")
}

/// Top-1 anchor for a query under BM25.
fn top_bm25(index: &Index, query: &str) -> Option<String> {
    index.query(query).into_iter().next().map(|r| r.anchor)
}

/// True when the #1 hit's anchor carries any accepted symbol name.
fn hit(anchor: &Option<String>, accept: &[&str]) -> bool {
    anchor
        .as_deref()
        .is_some_and(|a| accept.iter().any(|t| a.contains(t)))
}

#[test]
#[ignore = "confirmation gate; reads a real gen'd store (set ADEN_REAL_CORPUS, or uses the aden repo)"]
fn real_symbol_vocab_report() {
    let Some(repo) = corpus() else {
        eprintln!("SKIP: corpus dir not found (set ADEN_REAL_CORPUS)");
        return;
    };
    // `index` is mutated only in the dense `embed_documents` path below.
    #[cfg_attr(not(feature = "dense"), allow(unused_mut))]
    let Some((mut index, n_cards)) = build_index(&repo) else {
        eprintln!(
            "SKIP: no store cards at {} — run `aden gen` there first",
            repo.display()
        );
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

    let top_hybrid = |index: &Index, query: &str| -> Option<String> {
        #[cfg(feature = "dense")]
        if let Some(e) = &embedder {
            return index
                .hybrid_query(query, e)
                .into_iter()
                .next()
                .map(|r| r.anchor);
        }
        let _ = (index, query);
        None
    };
    let hybrid_on = {
        #[cfg(feature = "dense")]
        {
            embedder.is_some()
        }
        #[cfg(not(feature = "dense"))]
        {
            false
        }
    };

    println!("\n=== Real-symbol vocabulary-mismatch A/B (CONFIRMATION GATE) ===");
    println!("Corpus: {}", repo.display());
    println!(
        "Store cards: {n_cards} | {n} synonym-mismatch probes | hybrid arm: {}",
        if hybrid_on {
            "ON (bge model)"
        } else {
            "OFF (no model / no dense feature)"
        }
    );

    let (mut b_hits, mut o_hits, mut h_hits) = (0usize, 0usize, 0usize);
    let mut bm25_misses: Vec<&str> = Vec::new();

    for p in &probes {
        let bm25 = top_bm25(&index, p.query);
        let oracle = top_bm25(&index, &expand_query(p.query, p.expand));
        let hybrid = if hybrid_on {
            top_hybrid(&index, p.query)
        } else {
            None
        };

        let b_ok = hit(&bm25, p.accept);
        let o_ok = hit(&oracle, p.accept);
        let h_ok = hit(&hybrid, p.accept);
        b_hits += b_ok as usize;
        o_hits += o_ok as usize;
        h_hits += h_ok as usize;
        if !b_ok {
            bm25_misses.push(p.query);
        }

        let mark = |ok: bool| if ok { "OK  " } else { "MISS" };
        let hyb = if hybrid_on { mark(h_ok) } else { "--  " };
        println!(
            "  bm25 {} | oracle {} | hybrid {}   q: {}\n        (bridge: {})",
            mark(b_ok),
            mark(o_ok),
            hyb,
            p.query,
            p.bridge
        );
    }

    println!("\n  routing R@1 (exact symbol):");
    println!("    BM25            {b_hits}/{n}");
    println!("    BM25 + ORACLE   {o_hits}/{n}   (SynonymOf-layer upper bound)");
    if hybrid_on {
        println!("    HYBRID (RRF)    {h_hits}/{n}   (BM25 + dense)");
        println!(
            "\n  headroom for a SynonymOf layer (oracle - bm25):   {} probe(s)",
            o_hits as i64 - b_hits as i64
        );
        println!(
            "  DECISION number  (oracle - hybrid):               {} probe(s)  \
             <- net of what dense already captures",
            o_hits as i64 - h_hits as i64
        );
    } else {
        println!(
            "\n  headroom for a SynonymOf layer (oracle - bm25):   {} probe(s)",
            o_hits as i64 - b_hits as i64
        );
        println!("  (run with --features dense + a bge model for the HYBRID decision number)");
    }

    if !bm25_misses.is_empty() {
        println!("\n  BM25 miss list (the synonym-gap set a SynonymOf layer targets):");
        for q in &bm25_misses {
            println!("    - {q}");
        }
    }

    // Sanity only — this harness EXPOSES the gap, it does not gate CI on closing it.
    assert!(n_cards > 0, "no store cards loaded");
    assert!(
        o_hits >= b_hits,
        "oracle expansion routed WORSE than baseline ({o_hits} < {b_hits}) — the \
         hand-authored expansions are mis-sensed (Gonzalo's penalty); review the probe set"
    );
}
