// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Code-corpus vocabulary-mismatch A/B (NOT a committed gate — a measurement harness).
//
// The prose sibling (vocab_mismatch_ab.rs) found that synonym expansion's marginal
// lift over the existing BM25+dense+RRF stack was small on a doc corpus (+1/12). The
// findings brief (aden-semantic-retrieval.adoc) argues the decision should be made on
// CODE, where CoREB (2605.04615) predicts a LARGER dense collapse on short queries.
// This harness ports the same three arms to a code corpus of real aden symbol cards.
//
// WHY CODE IS A CLEANER ISOLATION OF LEVER B (dictionary-populated SynonymOf edges):
// aden's tokenizer ALREADY splits compound identifiers (`put_edges_bulk` -> put,
// edges, bulk; verified: the `tokenize` doc says it splits on underscore/dot/camelCase).
// So identifier *splitting* (lever C) is already done by ingest. The residual
// NL->code gap on a split corpus is therefore the *synonym* gap ("save" vs the
// symbol's "put", "combine" vs "fuse", "cluster" vs "community"): exactly what a
// WordNet/OEWN-populated SynonymOf layer would close. This arm measures that and
// nothing else.
//
// CORPUS: real aden symbol names + a faithful one-line role gloss standing in for the
// signature+doc-comment text the dense path actually embeds. Symbol NAMES and roles
// are grounded in aden source read this session; the gloss wording is hand-authored.
// Small + illustrative — the RELATIVE arm comparison is the signal, not a leaderboard.
//
// Three arms (identical structure to the prose harness):
//   * BM25          — index.query (the tokenizer already split the identifiers).
//   * BM25 + ORACLE — query + hand-authored, correct-sense synonym expansion. Gonzalo
//                     et al. 1998's *oracle* upper bound for a SynonymOf edge layer.
//   * HYBRID        — index.hybrid_query (BM25 + bge-small dense via RRF).
// Decision number = oracle - hybrid (lift net of what dense already recovers).
//
// Run: cargo test -p aden-index --features dense --test code_vocab_mismatch_ab -- --include-ignored --nocapture

use aden_index::Index;
use std::path::PathBuf;

/// A real aden symbol card: `anchor` is the symbol, `gloss` is the indexed text
/// (signature-ish + role), containing the identifier's own tokens plus role words.
struct Card {
    anchor: &'static str,
    gloss: &'static str,
}

/// ~18 real aden symbols (names/roles grounded in source read this session) as the
/// retrieval corpus. The gloss deliberately uses each symbol's OWN vocabulary so the
/// synonym-mismatch queries below have a genuine gap to bridge.
fn cards() -> Vec<Card> {
    vec![
        Card {
            anchor: "aden-store::put_edges_bulk",
            gloss: "fn put_edges_bulk: append a batch of typed edges to the fjall store, deduplicating existing triples.",
        },
        Card {
            anchor: "aden-index::rrf_fuse",
            gloss: "fn rrf_fuse: reciprocal rank fusion of the BM25 and dense rankings with constant k=60.",
        },
        Card {
            anchor: "aden-graph::detect_communities",
            gloss: "fn detect_communities: deterministic Louvain modularity community detection over the graph.",
        },
        Card {
            anchor: "aden-graph::validate_typed_edges",
            gloss: "fn validate_typed_edges: confirm each typed edge is allowed between its endpoint node types.",
        },
        Card {
            anchor: "aden-index::resolve_callee",
            gloss: "fn resolve_callee: resolve a callee name to its definition anchor using locality heuristics.",
        },
        Card {
            anchor: "aden-index::scan_orphans",
            gloss: "fn scan_orphans: scan the graph for orphan anchors that no document references.",
        },
        Card {
            anchor: "aden-index::classify_orphans",
            gloss: "fn classify_orphans: sort orphan anchors into expected metadata versus actionable orphans.",
        },
        Card {
            anchor: "aden-index::Index::ingest",
            gloss: "fn ingest: parse and tokenize documents into the inverted index, splitting compound identifiers.",
        },
        Card {
            anchor: "aden-index::bm25_score",
            gloss: "fn bm25_score: term frequency times inverse document frequency relevance for one token.",
        },
        Card {
            anchor: "aden-index::tokenize",
            gloss: "fn tokenize: split text into search terms on underscore, dot, and camelCase boundaries.",
        },
        Card {
            anchor: "aden-core::EdgeType::activation_weight",
            gloss: "fn activation_weight: the per edge type traversal weight used when walking the graph.",
        },
        Card {
            anchor: "aden-core::EdgeType::is_semantic",
            gloss: "fn is_semantic: whether an edge type is a lexical-semantic relation such as SynonymOf.",
        },
        Card {
            anchor: "aden-generate::link_include_edges",
            gloss: "fn link_include_edges: a post pass that writes include and containment edges after parsing.",
        },
        Card {
            anchor: "aden-index::embed_documents",
            gloss: "fn embed_documents: run the bge encoder over every indexed document and cache the vectors.",
        },
        Card {
            anchor: "aden-index::hybrid_query",
            gloss: "fn hybrid_query: fuse the BM25 and dense vector result lists with reciprocal rank fusion.",
        },
        Card {
            anchor: "aden-index::impact_traversal",
            gloss: "fn impact_traversal: walk downstream from a symbol to compute its blast radius of reachable nodes.",
        },
        Card {
            anchor: "aden-index::backlinks_query",
            gloss: "fn backlinks_query: every call site or reference that points at a given symbol anchor.",
        },
        Card {
            anchor: "aden-parse::make_anchor",
            gloss: "fn make_anchor: build a stable anchor string from a crate, file path, and symbol name.",
        },
    ]
}

/// One synonym-mismatch probe over the code corpus.
struct Probe {
    /// Short, NL-ish query whose content words are SYNONYMS of the target's tokens,
    /// not the tokens themselves (the gap a SynonymOf layer would bridge).
    query: &'static str,
    /// Acceptable target symbol anchor(s).
    accept: &'static [&'static str],
    /// Oracle (correct-sense) synonym terms appended in the ORACLE arm.
    expand: &'static str,
    /// The synonym relation that would bridge query->symbol (self-documenting).
    bridge: &'static str,
}

fn probes() -> Vec<Probe> {
    vec![
        Probe {
            query: "save many relationships in one write",
            accept: &["aden-store::put_edges_bulk"],
            expand: "append insert edges bulk batch",
            bridge: "save->append/put, relationships->edges, one write->bulk/batch",
        },
        Probe {
            query: "blend two ordered hit lists into one",
            accept: &["aden-index::rrf_fuse", "aden-index::hybrid_query"],
            expand: "fuse merge reciprocal rank fusion rankings",
            bridge: "blend->fuse, ordered hit lists->rankings",
        },
        Probe {
            query: "cluster the graph into groups",
            accept: &["aden-graph::detect_communities"],
            expand: "community detection louvain modularity",
            bridge: "cluster->community detection, groups->communities",
        },
        Probe {
            query: "ensure links are legal between vertex categories",
            accept: &["aden-graph::validate_typed_edges"],
            expand: "validate typed edges allowed node types",
            bridge: "ensure->validate, links->edges, vertex categories->node types",
        },
        Probe {
            query: "figure out where a function name is defined",
            accept: &["aden-index::resolve_callee"],
            expand: "resolve callee definition anchor",
            bridge: "figure out->resolve, function name->callee",
        },
        Probe {
            query: "anchors that nothing points to",
            accept: &["aden-index::scan_orphans"],
            expand: "orphan dangling unreferenced",
            bridge: "nothing points to->orphan/unreferenced",
        },
        Probe {
            query: "how strongly to follow each link kind when walking",
            accept: &["aden-core::EdgeType::activation_weight"],
            expand: "activation weight edge type traversal",
            bridge: "strongly->weight, link kind->edge type, walking->traversal",
        },
        Probe {
            query: "run the neural encoder over indexed text and store vectors",
            accept: &["aden-index::embed_documents"],
            expand: "embed bge encoder dense vectors cache",
            bridge: "neural encoder->bge embedder, store vectors->cache",
        },
        Probe {
            query: "everything that breaks if I change a symbol",
            accept: &["aden-index::impact_traversal"],
            expand: "impact downstream blast radius reachable",
            bridge: "breaks if I change->impact/blast radius, downstream",
        },
        Probe {
            query: "incoming callers of a symbol",
            accept: &["aden-index::backlinks_query"],
            expand: "backlinks call sites references pointing",
            bridge: "incoming callers->backlinks/call sites",
        },
        Probe {
            query: "divide a snake case name into pieces",
            accept: &["aden-index::tokenize", "aden-index::Index::ingest"],
            expand: "tokenize split identifier terms camelcase",
            bridge: "divide->split, pieces->tokens/terms",
        },
        Probe {
            query: "edge kinds that mean the same thing",
            accept: &["aden-core::EdgeType::is_semantic"],
            expand: "semantic synonym lexical relation synonymof",
            bridge: "mean the same thing->semantic/synonym relation",
        },
    ]
}

/// Append oracle expansion terms to a query (lexical query expansion).
fn expand_query(query: &str, expand: &str) -> String {
    format!("{query} {expand}")
}

fn build_index() -> Index {
    let entries: Vec<(PathBuf, String)> = cards()
        .into_iter()
        .map(|c| {
            (
                PathBuf::from(format!("{}.adoc", c.anchor.replace([':', '<', '>'], "_"))),
                format!("[[{}]]\n{}\n", c.anchor, c.gloss),
            )
        })
        .collect();
    let mut index = Index::default();
    index.ingest(entries);
    index.finalize();
    index
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

/// Top-1 anchor for a query under BM25.
fn top_bm25(index: &Index, query: &str) -> Option<String> {
    index.query(query).into_iter().next().map(|r| r.anchor)
}

#[test]
#[ignore = "measurement harness, not a CI gate"]
fn code_vocab_mismatch_report() {
    // `index` is mutated only in the dense `embed_documents` path below.
    #[cfg_attr(not(feature = "dense"), allow(unused_mut))]
    let mut index = build_index();
    let probes = probes();
    let n = probes.len();
    let n_cards = cards().len();

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

    let hit = |anchor: &Option<String>, accept: &[&str]| -> bool {
        anchor.as_deref().is_some_and(|a| accept.contains(&a))
    };

    println!("\n=== Code-corpus vocabulary-mismatch A/B (real aden symbol cards) ===");
    println!(
        "Corpus: {} symbol cards | {} synonym-mismatch probes | hybrid arm: {}",
        n_cards,
        n,
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

    // Sanity only — this harness EXPOSES the gap, it does not gate on closing it.
    assert!(n_cards > 0, "no symbol cards");
    assert!(
        o_hits >= b_hits,
        "oracle expansion routed WORSE than baseline ({o_hits} < {b_hits}); \
         the hand-authored expansions are mis-sensed — review the probe set"
    );
}
