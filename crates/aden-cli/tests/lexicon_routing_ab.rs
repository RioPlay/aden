// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Live-lexicon routing A/B — does the OEWN overlay store actually lift retrieval, and
// does grounding the expansion fix the WSD noise? (measurement harness, #[ignore]d,
// reads two stores, writes nothing.)
//
// First pass found naive live-lexicon expansion lifts routing only +1/12 (vs hand-oracle
// +7/12): expanding ALL senses of every query word drowns the right synonym in noise
// (store→shop/depot, vector→transmitter) — Gonzalo's WSD penalty at QUERY time. The fix
// the data points to: GROUND the expansion to the corpus vocab. Store the full lexicon at
// ingest, but at query time only expand with synonyms the corpus actually uses
// (store→put/save survive; store→shop/depot vanish). Grounding belongs at query-expansion,
// not ingest. This harness measures all four arms head-to-head.
//
// Arms over the SAME real cards + SAME probes:
//   * BM25              — baseline.
//   * BM25 + NAIVE      — query + all-sense SynonymOf neighbours from the overlay.
//   * BM25 + GROUNDED   — query + only neighbours whose lemma is in the corpus vocab.
//   * BM25 + ORACLE     — query + hand-authored correct-sense expansion (upper bound).
// Decisions: (grounded - bm25) is the real deployable lift; (oracle - grounded) is the
// residual gap (domain synonymy OEWN lacks — louvain/community — + lemma-match misses).
//
// Inputs: project store (cards) + ADEN_LEXICON_STORE (default ~/.cache/aden/lexicon).
// Run: cargo test -p aden-cli --test lexicon_routing_ab -- --include-ignored --nocapture

use aden_core::EdgeType;
use aden_index::Index;
use aden_store::{GraphStorage, Storage};
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

fn lexicon_path() -> PathBuf {
    std::env::var("ADEN_LEXICON_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache/aden/lexicon")
        })
}

fn index_text(doc: &aden_core::Document) -> String {
    aden_emit::emit_document(doc)
        .lines()
        .filter(|l| !l.trim_start().starts_with(":last-verified:"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build the BM25 index over the project store's real symbol cards, and return the corpus
/// vocabulary (every token the index sees) for query-time grounding.
fn build_index(repo: &Path) -> Option<(Index, usize, HashSet<String>)> {
    let root = aden_paths::resolve_root(repo);
    let (store_path, _) = aden_paths::resolve_read_store(&root);
    let storage = Storage::open_existing(store_path.to_str()?).ok()?;
    let docs = storage.get_all_documents().ok()?;

    let mut vocab = HashSet::new();
    let mut entries: Vec<(PathBuf, String)> = Vec::new();
    for d in docs.values() {
        let text = index_text(d);
        for tok in aden_index::tokenize(&text) {
            vocab.insert(tok);
        }
        let p = d
            .attributes
            .get("source_file")
            .cloned()
            .unwrap_or_else(|| d.anchor.clone());
        entries.push((PathBuf::from(p), text));
    }
    if entries.is_empty() {
        return None;
    }
    entries.sort_by(|a, b| a.1.cmp(&b.1));
    let n = entries.len();
    let mut index = Index::default();
    index.ingest(entries);
    index.finalize();
    Some((index, n, vocab))
}

fn lex_anchor(source: &str, lemma: &str) -> String {
    format!("aden://term/{source}/{lemma}")
}

fn moby_lexicon_path() -> PathBuf {
    std::env::var("ADEN_MOBY_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache/aden/moby")
        })
}

/// Union of two grounded synonym lists (merged-union arm: agreement >= 1).
fn union_syns(a: &[String], b: &[String]) -> Vec<String> {
    let mut out = a.to_vec();
    for x in b {
        if !out.contains(x) {
            out.push(x.clone());
        }
    }
    out
}

/// Cross-source agreement (merged-agree2 arm): a lemma attested by BOTH sources.
fn agree2_syns(a: &[String], b: &[String]) -> Vec<String> {
    a.iter().filter(|x| b.contains(x)).cloned().collect()
}

/// Content words of a query for lexicon lookup: lowercased, alphabetic, length >= 3.
fn query_words(q: &str) -> Vec<String> {
    q.split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| w.len() >= 3)
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Naive singular fallback so plural query words can still hit base-form OEWN lemmas.
fn singular(w: &str) -> Option<String> {
    (w.ends_with('s') && w.len() > 3).then(|| w[..w.len() - 1].to_string())
}

/// A lemma is grounded iff aden's tokenizer maps it entirely into the corpus vocab.
fn grounded(lemma: &str, vocab: &HashSet<String>) -> bool {
    let toks = aden_index::tokenize(lemma);
    !toks.is_empty() && toks.iter().all(|t| vocab.contains(t))
}

/// Expand a query with SynonymOf neighbours from the LIVE lexicon store. Draws from the
/// FULL neighbour list (so the right sense isn't lost to an early cap); when `vocab` is
/// `Some`, keeps only corpus-grounded lemmas. Caps the KEPT count per word.
fn expand(
    lex: &Storage,
    source: &str,
    query: &str,
    vocab: Option<&HashSet<String>>,
    cap: usize,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in query_words(query) {
        for cand in std::iter::once(w.clone()).chain(singular(&w)) {
            let Ok(edges) = lex.get_outgoing_edges(&lex_anchor(source, &cand)) else {
                continue;
            };
            let mut added = 0;
            for (tgt, et) in edges {
                if !matches!(et, EdgeType::SynonymOf) {
                    continue;
                }
                let lemma = tgt.rsplit('/').next().unwrap_or(&tgt).to_string();
                if lemma.contains(' ') || out.contains(&lemma) {
                    continue;
                }
                if let Some(v) = vocab
                    && !grounded(&lemma, v)
                {
                    continue;
                }
                out.push(lemma);
                added += 1;
                if added >= cap {
                    break;
                }
            }
        }
    }
    out
}

struct Probe {
    query: &'static str,
    accept: &'static [&'static str],
    expand: &'static str, // hand-authored oracle
}

fn probes() -> Vec<Probe> {
    vec![
        Probe {
            query: "store a batch of relationships between nodes in one operation",
            accept: &["put_edges_bulk"],
            expand: "append bulk typed edges deduplicate",
        },
        Probe {
            query: "group the graph into clusters of tightly connected nodes",
            accept: &["detect_communities"],
            expand: "community detection louvain modularity",
        },
        Probe {
            query: "blend two ranked result lists into a single ordering",
            accept: &["rrf_fuse"],
            expand: "reciprocal rank fusion combine rankings",
        },
        Probe {
            query: "how aligned are two embedding vectors",
            accept: &["cosine_similarity"],
            expand: "cosine similarity vector",
        },
        Probe {
            query: "fewest single character edits to turn one word into another",
            accept: &["levenshtein_distance"],
            expand: "levenshtein edit distance",
        },
        Probe {
            query: "figure out which definition a function call points to",
            accept: &["resolve_callee"],
            expand: "resolve callee definition anchor",
        },
        Probe {
            query: "decide what category of question the user is asking",
            accept: &["classify_intent"],
            expand: "classify intent query category",
        },
        Probe {
            query: "detect a leaked password or api key inside text",
            accept: &["content_has_high_confidence_secret"],
            expand: "secret credential api key detection",
        },
        Probe {
            query: "collect the nodes surrounding a starting symbol up to some depth",
            accept: &["build_neighborhood"],
            expand: "neighborhood traversal depth graph",
        },
        Probe {
            query: "find everything that points at a given node",
            accept: &["get_incoming_edges"],
            expand: "incoming edges backlinks callers references",
        },
        Probe {
            query: "how many tokens were avoided versus reading whole files",
            accept: &["SavingsEstimate"],
            expand: "savings estimate tokens baseline bytes",
        },
        Probe {
            query: "anchors in the graph that nothing else references",
            accept: &["scan_orphans"],
            expand: "scan orphan anchors unreferenced dangling",
        },
    ]
}

/// Prose/NL probe set: each query uses a SYNONYM of a word in a real doc heading, so
/// only lexicon-aided expansion can route it. This is the fair test of the dict substrate
/// (prose), complementing the code-symbol `probes()`. Gold = a distinctive substring of the
/// real `aden://doc/...` heading anchor in this repo's store.
fn prose_probes() -> Vec<Probe> {
    vec![
        Probe {
            query: "fused dense sparse retrieval",
            accept: &["hybrid-retrieval"],
            expand: "hybrid retrieval dense sparse fusion",
        },
        Probe {
            query: "three-way reconciliation strategy",
            accept: &["three-way-merge"],
            expand: "three way merge reconcile",
        },
        Probe {
            query: "fundamental argument design rationale",
            accept: &["core-thesis"],
            expand: "core thesis argument",
        },
        Probe {
            query: "worldwide command flags",
            accept: &["global-options"],
            expand: "global options flags",
        },
        Probe {
            query: "hidden credential detection",
            accept: &["secret-scanning"],
            expand: "secret scanning credential",
        },
        Probe {
            query: "context budget savings measurement",
            accept: &["token-efficiency"],
            expand: "token efficiency savings",
        },
        Probe {
            query: "impact scope prior to rewrite",
            accept: &["blast-radius"],
            expand: "blast radius impact",
        },
        Probe {
            query: "how the question answering operates",
            accept: &["ask--works"],
            expand: "ask works question answering",
        },
        Probe {
            query: "auto-repairing drift contracts",
            accept: &["self-healing"],
            expand: "self healing contracts drift",
        },
        Probe {
            query: "restructure code safely",
            accept: &["refactor-with-confidence"],
            expand: "refactor confidence restructure",
        },
    ]
}

/// Run every arm over one probe set and print an R@1 table. Arms: BASELINE, OEWN-grounded,
/// MOBY-grounded, MERGED-UNION (oewn ∪ moby), MERGED-AGREE2 (oewn ∩ moby = cross-source
/// agreement), ORACLE. Moby-dependent arms degrade to baseline when the Moby store is absent.
fn run_arms(
    index: &Index,
    lex: &Storage,
    moby: Option<&Storage>,
    vocab: &HashSet<String>,
    probes: &[Probe],
    label: &str,
) {
    let n = probes.len();
    let (mut b, mut oewn, mut mob, mut uni, mut ag2, mut orc) = (0, 0, 0, 0, 0, 0);
    println!("\n=== {label} probe set ({n} probes) ===");
    for p in probes {
        let oewn_g = expand(lex, "oewn", p.query, Some(vocab), 8);
        let moby_g = moby
            .map(|m| expand(m, "moby", p.query, Some(vocab), 8))
            .unwrap_or_default();
        let union = union_syns(&oewn_g, &moby_g);
        let agree = agree2_syns(&oewn_g, &moby_g);
        let q = |syns: &[String]| {
            if syns.is_empty() {
                top(index, p.query)
            } else {
                top(index, &format!("{} {}", p.query, syns.join(" ")))
            }
        };
        let (bx, ox, mx, ux, ax, rx) = (
            hit(&top(index, p.query), p.accept),
            hit(&q(&oewn_g), p.accept),
            hit(&q(&moby_g), p.accept),
            hit(&q(&union), p.accept),
            hit(&q(&agree), p.accept),
            hit(&top(index, &format!("{} {}", p.query, p.expand)), p.accept),
        );
        b += bx as usize;
        oewn += ox as usize;
        mob += mx as usize;
        uni += ux as usize;
        ag2 += ax as usize;
        orc += rx as usize;
        let m = |ok: bool| if ok { "OK " } else { "-- " };
        println!(
            "  base {} oewn {} moby {} union {} agr2 {} oracle {}  q: {}",
            m(bx),
            m(ox),
            m(mx),
            m(ux),
            m(ax),
            m(rx),
            p.query
        );
    }
    println!("  -- {label} R@1 --");
    println!("    BASELINE        {b}/{n}");
    println!("    OEWN-grounded   {oewn}/{n}   (lift {:+})", oewn as i64 - b as i64);
    println!("    MOBY-grounded   {mob}/{n}   (lift {:+})", mob as i64 - b as i64);
    println!("    MERGED-UNION    {uni}/{n}   (lift {:+})", uni as i64 - b as i64);
    println!("    MERGED-AGREE2   {ag2}/{n}   (lift {:+})", ag2 as i64 - b as i64);
    println!("    ORACLE          {orc}/{n}   (upper bound)");
}

fn top(index: &Index, q: &str) -> Option<String> {
    index.query(q).into_iter().next().map(|r| r.anchor)
}

fn hit(anchor: &Option<String>, accept: &[&str]) -> bool {
    anchor
        .as_deref()
        .is_some_and(|a| accept.iter().any(|t| a.contains(t)))
}

#[test]
#[ignore = "live-lexicon routing A/B; reads project + lexicon stores; writes nothing"]
fn lexicon_routing_report() {
    let Some(repo) = corpus() else {
        eprintln!("SKIP: corpus dir not found");
        return;
    };
    let Some((index, n_cards, vocab)) = build_index(&repo) else {
        eprintln!("SKIP: no project store cards — run `aden gen`");
        return;
    };
    let Ok(lex) = Storage::open_existing(lexicon_path().to_str().unwrap_or_default()) else {
        eprintln!("SKIP: lexicon store not built — run build_lexicon_store first");
        return;
    };

    let probes = probes();
    let n = probes.len();
    println!(
        "\n=== Live-lexicon routing A/B ({n_cards} cards, {} vocab, {n} probes) ===",
        vocab.len()
    );

    let moby = Storage::open_existing(moby_lexicon_path().to_str().unwrap_or_default()).ok();
    if moby.is_none() {
        eprintln!(
            "NOTE: Moby store absent (run build_moby_store) — Moby/union/agree arms show as baseline"
        );
    }

    run_arms(&index, &lex, moby.as_ref(), &vocab, &probes, "CODE");
    run_arms(&index, &lex, moby.as_ref(), &vocab, &prose_probes(), "PROSE");

    assert!(n_cards > 0, "no cards");
}
