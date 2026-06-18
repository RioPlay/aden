// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Graph-derived expansion routing A/B — does the corpus's OWN learned relationships lift
// retrieval where the dictionary couldn't? (measurement harness, #[ignore]d, reads the
// project store, writes nothing.)
//
// The lexicon A/B (lexicon_routing_ab) found automatic OEWN expansion — naive AND
// corpus-grounded — lifts routing only +1/12: dictionary synonyms are polysemous and
// domain-blind. This harness tests the redirect: expand each query word with its top-PPMI
// CORPUS neighbours computed on the fly (cluster→louvain, score→bm25+dense). These are
// sense-correct by construction (derived from usage) and domain-specific (exactly what
// OEWN lacks). Two fixes vs the first PPMI pass: query words are TOKENIZED to the index's
// stemmed key space (so `merge`/`resolve` aren't "absent"), and candidate neighbours are
// CLEANED (no hex hashes, file paths, or signature fragments).
//
// Arms over the SAME real cards + SAME probes as lexicon_routing_ab:
//   * BM25            — baseline.
//   * BM25 + GRAPH    — query + top-PPMI corpus neighbours of each query word.
//   * BM25 + ORACLE   — query + hand-authored correct-sense expansion (upper bound).
// Decisions: (graph - bm25) is the deployable lift; (oracle - graph) is the residual.
//
// Run: cargo test -p aden-cli --test graph_expansion_ab -- --include-ignored --nocapture

use aden_index::Index;
use aden_store::{GraphStorage, Storage};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const MIN_DF: usize = 3;
const MAX_DF_FRAC: f64 = 0.20;
const MIN_CO: usize = 3;
const CAP: usize = 5; // top-PPMI neighbours appended per query word

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

/// A term is a usable expansion candidate iff it is word-like: alphanumeric with at least
/// one letter, short, and not a long hex hash. Drops the content-hash / file-path /
/// signature-fragment tokens that polluted the first PPMI pass.
fn clean(t: &str) -> bool {
    let len_ok = (2..=18).contains(&t.len());
    let alnum = t.chars().all(|c| c.is_ascii_alphanumeric());
    let has_alpha = t.chars().any(|c| c.is_ascii_alphabetic());
    let hex_hash = t.len() >= 8 && t.chars().all(|c| c.is_ascii_hexdigit());
    len_ok && alnum && has_alpha && !hex_hash
}

/// Build the BM25 index and the per-card token sets (the term↔card graph PPMI runs over).
fn load(repo: &Path) -> Option<(Index, Vec<HashSet<String>>)> {
    let root = aden_paths::resolve_root(repo);
    let (store_path, _) = aden_paths::resolve_read_store(&root);
    let storage = Storage::open_existing(store_path.to_str()?).ok()?;
    let docs = storage.get_all_documents().ok()?;

    let mut entries: Vec<(PathBuf, String)> = Vec::new();
    let mut cards: Vec<HashSet<String>> = Vec::new();
    for d in docs.values() {
        let text = index_text(d);
        let toks: HashSet<String> = aden_index::tokenize(&text).into_iter().collect();
        let p = d
            .attributes
            .get("source_file")
            .cloned()
            .unwrap_or_else(|| d.anchor.clone());
        entries.push((PathBuf::from(p), text));
        cards.push(toks);
    }
    if entries.is_empty() {
        return None;
    }
    entries.sort_by(|a, b| a.1.cmp(&b.1));
    let mut index = Index::default();
    index.ingest(entries);
    index.finalize();
    Some((index, cards))
}

fn query_words(q: &str) -> Vec<String> {
    q.split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| w.len() >= 3)
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Expand a query with each query word's top-PPMI corpus neighbours. Query words are
/// tokenized to the index's key space first (consistent stemming), then their PPMI
/// neighbours (clean, df/co-thresholded) are ranked and the top `CAP` appended.
fn graph_expand<'a>(
    cards: &'a [HashSet<String>],
    postings: &HashMap<&'a str, Vec<usize>>,
    n: usize,
    max_df: usize,
    query: &str,
    cap: usize,
) -> Vec<String> {
    let df = |t: &str| postings.get(t).map_or(0, |v| v.len());
    let mut out: Vec<String> = Vec::new();
    for raw in query_words(query) {
        for w in aden_index::tokenize(&raw) {
            let Some(cards_w) = postings.get(w.as_str()) else {
                continue;
            };
            let df_w = cards_w.len();
            if !(MIN_DF..=max_df).contains(&df_w) {
                continue;
            }
            let mut co: HashMap<&str, usize> = HashMap::new();
            for &ci in cards_w {
                for t in &cards[ci] {
                    if t.as_str() != w {
                        *co.entry(t.as_str()).or_default() += 1;
                    }
                }
            }
            let mut scored: Vec<(&str, f64)> = co
                .into_iter()
                .filter(|&(t, c)| c >= MIN_CO && (MIN_DF..=max_df).contains(&df(t)) && clean(t))
                .filter_map(|(t, c)| {
                    let pmi = ((c * n) as f64 / (df_w * df(t)) as f64).log2();
                    (pmi > 0.0).then_some((t, pmi))
                })
                .collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(b.0)));
            for (t, _) in scored.into_iter().take(cap) {
                let s = t.to_string();
                if !out.contains(&s) {
                    out.push(s);
                }
            }
        }
    }
    out
}

/// Targeted expansion: append only the top-`cap` PPMI neighbours of the SINGLE rarest
/// (lowest-df, most distinctive) query word — the likely key concept. Mimics the oracle:
/// expand one concept precisely, not every word broadly.
fn targeted_expand<'a>(
    cards: &'a [HashSet<String>],
    postings: &HashMap<&'a str, Vec<usize>>,
    n: usize,
    max_df: usize,
    query: &str,
    cap: usize,
) -> Vec<String> {
    let df = |t: &str| postings.get(t).map_or(0, |v| v.len());
    let key = query_words(query)
        .into_iter()
        .flat_map(|raw| aden_index::tokenize(&raw))
        .filter(|w| (MIN_DF..=max_df).contains(&df(w)))
        .min_by(|a, b| df(a).cmp(&df(b)).then(a.cmp(b)));
    let Some(w) = key else {
        return Vec::new();
    };
    let df_w = df(&w);
    let Some(cards_w) = postings.get(w.as_str()) else {
        return Vec::new();
    };
    let mut co: HashMap<&str, usize> = HashMap::new();
    for &ci in cards_w {
        for t in &cards[ci] {
            if t.as_str() != w {
                *co.entry(t.as_str()).or_default() += 1;
            }
        }
    }
    let mut scored: Vec<(&str, f64)> = co
        .into_iter()
        .filter(|&(t, c)| c >= MIN_CO && (MIN_DF..=max_df).contains(&df(t)) && clean(t))
        .filter_map(|(t, c)| {
            let pmi = ((c * n) as f64 / (df_w * df(t)) as f64).log2();
            (pmi > 0.0).then_some((t, pmi))
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(b.0)));
    scored
        .into_iter()
        .take(cap)
        .map(|(t, _)| t.to_string())
        .collect()
}

struct Probe {
    query: &'static str,
    accept: &'static [&'static str],
    expand: &'static str,
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

fn top(index: &Index, q: &str) -> Option<String> {
    index.query(q).into_iter().next().map(|r| r.anchor)
}

fn hit(anchor: &Option<String>, accept: &[&str]) -> bool {
    anchor
        .as_deref()
        .is_some_and(|a| accept.iter().any(|t| a.contains(t)))
}

#[test]
#[ignore = "graph-derived expansion routing A/B; reads project store; writes nothing"]
fn graph_expansion_report() {
    let Some(repo) = corpus() else {
        eprintln!("SKIP: corpus dir not found");
        return;
    };
    let Some((index, cards)) = load(&repo) else {
        eprintln!("SKIP: no project store cards — run `aden gen`");
        return;
    };
    let n = cards.len();
    let mut postings: HashMap<&str, Vec<usize>> = HashMap::new();
    for (ci, toks) in cards.iter().enumerate() {
        for t in toks {
            postings.entry(t.as_str()).or_default().push(ci);
        }
    }
    let max_df = (MAX_DF_FRAC * n as f64) as usize;

    let probes = probes();
    let np = probes.len();
    println!("\n=== Graph-derived expansion routing A/B ({n} cards, {np} probes) ===");

    let (mut b, mut g, mut t, mut o) = (0usize, 0usize, 0usize, 0usize);
    for p in &probes {
        let gx = graph_expand(&cards, &postings, n, max_df, p.query, CAP);
        let gt = targeted_expand(&cards, &postings, n, max_df, p.query, 3);

        let b_ok = hit(&top(&index, p.query), p.accept);
        let g_ok = hit(
            &top(&index, &format!("{} {}", p.query, gx.join(" "))),
            p.accept,
        );
        let t_ok = hit(
            &top(&index, &format!("{} {}", p.query, gt.join(" "))),
            p.accept,
        );
        let o_ok = hit(&top(&index, &format!("{} {}", p.query, p.expand)), p.accept);
        b += b_ok as usize;
        g += g_ok as usize;
        t += t_ok as usize;
        o += o_ok as usize;

        let mark = |ok: bool| if ok { "OK  " } else { "MISS" };
        println!(
            "  bm25 {} | bulk {} | targeted {} | oracle {}   q: {}",
            mark(b_ok),
            mark(g_ok),
            mark(t_ok),
            mark(o_ok),
            p.query
        );
        println!("        targeted added: {gt:?}");
    }

    println!("\n  routing R@1:");
    println!("    BM25                  {b}/{np}");
    println!("    BM25 + GRAPH-BULK     {g}/{np}   (every query word, top-{CAP} PPMI)");
    println!("    BM25 + GRAPH-TARGETED {t}/{np}   (rarest word only, top-3 PPMI)");
    println!("    BM25 + ORACLE         {o}/{np}   (hand-authored upper bound)");
    println!(
        "\n  targeted lift (targeted - bm25):   {}",
        t as i64 - b as i64
    );
    println!(
        "  ceiling gap   (oracle - targeted): {}",
        o as i64 - t as i64
    );

    assert!(n > 0, "no cards");
    assert!(
        t >= b,
        "targeted expansion routed worse than baseline ({t} < {b})"
    );
}
