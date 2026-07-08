// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Compounding — does reranking the FUSION-FIXED base beat reranking pure dense?
// (measurement harness, #[ignore]d, needs --features dense, writes nothing.)
//
// Two independent wins this session: the fusion fix (dense-weighted RRF 1:5, base MRR 0.182
// vs pure dense 0.146) and the L2 reranker (rerank pure dense → 0.203). They act at different
// stages — fusion makes the base, the reranker reorders the base's top-K — so they should
// stack. This measures whether they do: rerank the RRF-1:5 base and compare to reranking pure
// dense. If the rerank lift carries onto the better base, that's the compounding the
// iterate-the-loop plan predicts; if not, the two are redundant and we pick one.
//
// Arms: DENSE · HYBRID(RRF 1:5) · RERANK→DENSE · RERANK→HYBRID · ORACLE. (rerank weight 2.0,
// window 100, L2 signal = PPMI + OEWN agreement.)
// Run: cargo test -p aden-cli --features dense --test compound_ab -- --include-ignored --nocapture

#![cfg_attr(not(feature = "dense"), allow(dead_code))]

use aden_core::EdgeType;
use aden_index::{Index, SearchResult};
use aden_store::{GraphStorage, Storage};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

type TermMap = HashMap<String, HashSet<String>>;

const MIN_DF: usize = 3;
const MAX_DF_FRAC: f64 = 0.20;
const TOPK: usize = 100;

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

fn load(repo: &Path) -> Option<(Index, TermMap, TermMap)> {
    let root = aden_paths::resolve_root(repo);
    let (store_path, _) = aden_paths::resolve_read_store(&root);
    let storage = Storage::open_existing(store_path.to_str()?).ok()?;
    let docs = storage.get_all_documents().ok()?;
    let mut entries: Vec<(PathBuf, String)> = Vec::new();
    let mut card_tokens: TermMap = HashMap::new();
    let mut postings: TermMap = HashMap::new();
    for d in docs.values() {
        let text = index_text(d);
        let toks: HashSet<String> = aden_index::tokenize(&text).into_iter().collect();
        for t in &toks {
            postings
                .entry(t.clone())
                .or_default()
                .insert(d.anchor.clone());
        }
        card_tokens.insert(d.anchor.clone(), toks);
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
    let mut index = Index::default();
    index.ingest(entries);
    index.finalize();
    Some((index, card_tokens, postings))
}

#[cfg(feature = "dense")]
fn load_embedder() -> Option<aden_index::TractEmbedder> {
    let dir = std::env::var("ADEN_BGE_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(
                dirs::home_dir()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            )
            .join(".cache/aden-models/bge-small-en-v1.5")
        });
    if !dir.join("model.onnx").exists() {
        return None;
    }
    aden_index::TractEmbedder::from_dir(&dir).ok()
}

fn lexicon_path() -> PathBuf {
    std::env::var("ADEN_LEXICON_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(
                dirs::home_dir()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            )
            .join(".cache/aden/lexicon")
        })
}

fn moby_lexicon_path() -> PathBuf {
    std::env::var("ADEN_MOBY_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(
                dirs::home_dir()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            )
            .join(".cache/aden/moby")
        })
}

/// Per-query-term SynonymOf neighbours from a given source namespace (oewn|moby|...).
fn build_syn_from(
    lex: Option<&Storage>,
    source: &str,
    qts: &[String],
) -> HashMap<String, HashSet<String>> {
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    let Some(l) = lex else {
        return out;
    };
    for qt in qts {
        if let Ok(edges) = l.get_outgoing_edges(&format!("aden://term/{source}/{qt}")) {
            let s: HashSet<String> = edges
                .into_iter()
                .filter(|(_, et)| matches!(et, EdgeType::SynonymOf))
                .map(|(tgt, _)| tgt.rsplit('/').next().unwrap_or(&tgt).to_string())
                .collect();
            if !s.is_empty() {
                out.insert(qt.clone(), s);
            }
        }
    }
    out
}

/// Per-term union of two syn maps (merged-union arm: agreement >= 1).
fn union_maps(
    a: &HashMap<String, HashSet<String>>,
    b: &HashMap<String, HashSet<String>>,
) -> HashMap<String, HashSet<String>> {
    let mut out = a.clone();
    for (k, v) in b {
        out.entry(k.clone()).or_default().extend(v.iter().cloned());
    }
    out
}

/// Per-term cross-source agreement (merged-agree2 arm): only lemmas in BOTH sources.
fn agree2_maps(
    a: &HashMap<String, HashSet<String>>,
    b: &HashMap<String, HashSet<String>>,
) -> HashMap<String, HashSet<String>> {
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    for (k, va) in a {
        if let Some(vb) = b.get(k) {
            let inter: HashSet<String> = va.iter().filter(|x| vb.contains(*x)).cloned().collect();
            if !inter.is_empty() {
                out.insert(k.clone(), inter);
            }
        }
    }
    out
}

fn ppmi(a: &str, b: &str, postings: &TermMap, n: usize) -> f64 {
    let (Some(pa), Some(pb)) = (postings.get(a), postings.get(b)) else {
        return 0.0;
    };
    let (small, big) = if pa.len() <= pb.len() {
        (pa, pb)
    } else {
        (pb, pa)
    };
    let co = small.iter().filter(|x| big.contains(*x)).count();
    if co == 0 {
        return 0.0;
    }
    ((co as f64 * n as f64) / (pa.len() as f64 * pb.len() as f64))
        .log2()
        .max(0.0)
}

fn rel_score(
    qts: &[String],
    card: &HashSet<String>,
    postings: &TermMap,
    n: usize,
    max_df: usize,
    syn: &HashMap<String, HashSet<String>>,
) -> f64 {
    let df = |t: &str| postings.get(t).map_or(0, |s| s.len());
    let ok = |t: &str| (MIN_DF..=max_df).contains(&df(t));
    qts.iter()
        .filter(|qt| ok(qt))
        .map(|qt| {
            let qsyn = syn.get(qt);
            card.iter()
                .filter(|ct| ok(ct))
                .map(|ct| {
                    let p = ppmi(qt, ct, postings, n);
                    let o = if qsyn.is_some_and(|s| s.contains(ct)) {
                        3.0
                    } else {
                        0.0
                    };
                    p + o
                })
                .fold(0.0_f64, f64::max)
        })
        .sum()
}

/// Weighted RRF (k=60) of two ranked lists → fused anchor order (the fusion fix).
fn rrf(bm25: &[SearchResult], dense: &[SearchResult], w_bm: f64, w_de: f64) -> Vec<String> {
    const K: f64 = 60.0;
    let mut score: HashMap<&str, f64> = HashMap::new();
    for (i, r) in bm25.iter().enumerate() {
        *score.entry(r.anchor.as_str()).or_default() += w_bm / (K + (i + 1) as f64);
    }
    for (i, r) in dense.iter().enumerate() {
        *score.entry(r.anchor.as_str()).or_default() += w_de / (K + (i + 1) as f64);
    }
    let mut v: Vec<(&str, f64)> = score.into_iter().collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(b.0)));
    v.into_iter().map(|(a, _)| a.to_string()).collect()
}

/// Rerank a base ranked anchor list by `base_rank_score + w_rel * normalized_rel` over its
/// top-`rels.len()`, leaving the tail in base order.
fn rerank_list(base: &[String], rels: &[f64], w_rel: f64) -> Vec<String> {
    let max_rel = rels.iter().cloned().fold(0.0_f64, f64::max).max(1e-9);
    let mut head: Vec<(usize, f64)> = (0..rels.len())
        .map(|i| (i, 1.0 / (i + 1) as f64 + w_rel * rels[i] / max_rel))
        .collect();
    head.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    head.into_iter()
        .map(|(i, _)| base[i].clone())
        .chain(base.iter().skip(rels.len()).cloned())
        .collect()
}

fn rank_anchors(list: &[String], accept: &[&str]) -> Option<usize> {
    list.iter()
        .position(|a| accept.iter().any(|t| a.contains(t)))
        .map(|p| p + 1)
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
            query: "anchors in the graph that nothing else references",
            accept: &["scan_orphans"],
            expand: "scan orphan anchors unreferenced dangling",
        },
    ]
}

/// Prose/NL probes: each query uses a SYNONYM of a real doc heading word, so only
/// lexicon-aided reranking can lift the gold doc anchor. The fair test of the dict substrate.
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
            query: "context budget assembly measurement",
            accept: &["token-efficiency"],
            expand: "token efficiency assembly budget",
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

#[derive(Default)]
struct Metrics {
    r1: usize,
    r5: usize,
    r10: usize,
    mrr: f64,
}
impl Metrics {
    fn add(&mut self, rank: Option<usize>) {
        if let Some(r) = rank {
            self.r1 += (r == 1) as usize;
            self.r5 += (r <= 5) as usize;
            self.r10 += (r <= 10) as usize;
            self.mrr += 1.0 / r as f64;
        }
    }
    fn line(&self, label: &str, n: usize) -> String {
        format!(
            "    {label:<16} R@1 {}/{n}   R@5 {}/{n}   R@10 {}/{n}   MRR {:.3}",
            self.r1,
            self.r5,
            self.r10,
            self.mrr / n as f64
        )
    }
}

#[test]
#[ignore = "compounding A/B (needs --features dense); reads project store; writes nothing"]
fn compound_report() {
    #[cfg(not(feature = "dense"))]
    {
        eprintln!("SKIP: rebuild with --features dense");
    }
    #[cfg(feature = "dense")]
    {
        let Some(repo) = corpus() else {
            eprintln!("SKIP: corpus dir not found");
            return;
        };
        let Some((mut index, card_tokens, postings)) = load(&repo) else {
            eprintln!("SKIP: no project store cards");
            return;
        };
        let Some(e) = load_embedder() else {
            eprintln!("SKIP: bge model not found");
            return;
        };
        index.embed_documents(&e);

        let n_cards = card_tokens.len();
        let max_df = (MAX_DF_FRAC * n_cards as f64) as usize;
        let lex = Storage::open_existing(lexicon_path().to_str().unwrap_or_default()).ok();
        let probes = probes();
        let n = probes.len();
        println!("\n=== Compounding ({n_cards} cards, {n} probes) ===");

        let moby = Storage::open_existing(moby_lexicon_path().to_str().unwrap_or_default()).ok();
        if moby.is_none() {
            eprintln!("NOTE: Moby store absent — Moby/union/agree arms equal PPMI-only");
        }
        let empty: HashSet<String> = HashSet::new();

        // One pass over a probe set: all arms rerank the SAME hybrid base, differing only in
        // the relation signal. RR+PPMI is derived-only (corpus co-occurrence); RR+OEWN/MOBY add
        // an imported dict; RR+UNION/AGREE2 combine the two dicts (agree2 = cross-source).
        let run = |probes: &[Probe], label: &str| {
            let n = probes.len();
            let (mut md, mut mh, mut der, mut roewn, mut rmoby, mut runi, mut ragr, mut orc) = (
                Metrics::default(),
                Metrics::default(),
                Metrics::default(),
                Metrics::default(),
                Metrics::default(),
                Metrics::default(),
                Metrics::default(),
                Metrics::default(),
            );
            for p in probes {
                let bm = index.query(p.query);
                let de = index.dense_query(p.query, &e);
                let qts: Vec<String> = aden_index::tokenize(p.query);
                let syn_oewn = build_syn_from(lex.as_ref(), "oewn", &qts);
                let syn_moby = build_syn_from(moby.as_ref(), "moby", &qts);
                let syn_union = union_maps(&syn_oewn, &syn_moby);
                let syn_agree = agree2_maps(&syn_oewn, &syn_moby);
                let none_syn: HashMap<String, HashSet<String>> = HashMap::new();

                let dense_base: Vec<String> = de.iter().map(|r| r.anchor.clone()).collect();
                let hybrid_base: Vec<String> = rrf(&bm, &de, 1.0, 5.0);
                let rels_for = |syn: &HashMap<String, HashSet<String>>| -> Vec<f64> {
                    hybrid_base
                        .iter()
                        .take(TOPK)
                        .map(|a| {
                            let c = card_tokens.get(a).unwrap_or(&empty);
                            rel_score(&qts, c, &postings, n_cards, max_df, syn)
                        })
                        .collect()
                };

                md.add(rank_anchors(&dense_base, p.accept));
                mh.add(rank_anchors(&hybrid_base, p.accept));
                der.add(rank_anchors(
                    &rerank_list(&hybrid_base, &rels_for(&none_syn), 2.0),
                    p.accept,
                ));
                roewn.add(rank_anchors(
                    &rerank_list(&hybrid_base, &rels_for(&syn_oewn), 2.0),
                    p.accept,
                ));
                rmoby.add(rank_anchors(
                    &rerank_list(&hybrid_base, &rels_for(&syn_moby), 2.0),
                    p.accept,
                ));
                runi.add(rank_anchors(
                    &rerank_list(&hybrid_base, &rels_for(&syn_union), 2.0),
                    p.accept,
                ));
                ragr.add(rank_anchors(
                    &rerank_list(&hybrid_base, &rels_for(&syn_agree), 2.0),
                    p.accept,
                ));
                orc.add(rank_anchors(
                    &index
                        .query(&format!("{} {}", p.query, p.expand))
                        .iter()
                        .map(|r| r.anchor.clone())
                        .collect::<Vec<_>>(),
                    p.accept,
                ));
            }

            println!("\n  === {label} ({n} probes) rank-of-gold ===");
            println!("{}", md.line("DENSE", n));
            println!("{}", mh.line("HYBRID 1:5", n));
            println!("{}", der.line("RR+PPMI(deriv)", n));
            println!("{}", roewn.line("RR+OEWN", n));
            println!("{}", rmoby.line("RR+MOBY", n));
            println!("{}", runi.line("RR+UNION", n));
            println!("{}", ragr.line("RR+AGREE2", n));
            println!("{}", orc.line("ORACLE", n));
        };

        run(&probes, "CODE");
        run(&prose_probes(), "PROSE");

        assert!(n_cards > 0, "no cards");
    }
}
