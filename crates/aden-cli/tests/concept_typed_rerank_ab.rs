// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The synthesis — THE GRAPH CREATES THE WEIGHTS (RioPlay's model). Every edge's weight is the
// context-centroid CORRELATION the graph computed between its endpoints, not a hardcoded
// per-type constant. Two terms used in similar contexts here correlate strongly -> strong edge;
// the correlation *is* the weight. Then we write those derived weights out as the findings.
//
// We union two relationship sources into one reranker, both weighted by graph correlation:
//   * concept-graph neighbours  (weight = cosine(centroid[term], centroid[neighbour])), PLUS
//   * OEWN typed neighbours that are corpus terms (weight = cosine(query, centroid[term])) — the
//     dictionary proposes the EDGE, the graph decides its WEIGHT and so disambiguates it: a
//     wrong-sense suggestion correlates weakly with the query context and is down-weighted to
//     near-zero automatically. Graph-context-gated WSD as a soft weight, not a hard gate.
//
// Arms (rank-of-gold): DENSE, CONCEPT, +OEWN-CORR (the model), +OEWN-TYPE (hardcoded weights —
// the comparison that tests the model), +OEWN-CORR>=floor, ORACLE.
// Findings written to ~/.cache/aden/dict/concept-graph-weighted.json (NOT .aden/store).
//
// Run: cargo test -p aden-cli --features dense --test concept_typed_rerank_ab -- --include-ignored --nocapture

#![cfg_attr(not(feature = "dense"), allow(dead_code))]

use std::path::{Path, PathBuf};

const MIN_DF: usize = 4;
const MAX_DF_FRAC: f64 = 0.15;
const KNN: usize = 4; // concept-graph neighbours per term (sweep winner)
const HEAD: usize = 20; // rerank depth (sweep winner)
const FLOOR: f32 = 0.90; // soft-gate floor for the +OEWN-CORR>=floor arm

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

/// Per-card (anchor, token set, indexed text) for the whole store.
fn load_cards(repo: &Path) -> Vec<(String, Vec<String>, String)> {
    use aden_store::{GraphStorage, Storage};
    let root = aden_paths::resolve_root(repo);
    let (store_path, _) = aden_paths::resolve_read_store(&root);
    let Some(s) = store_path.to_str() else {
        return Vec::new();
    };
    let Ok(storage) = Storage::open_existing(s) else {
        return Vec::new();
    };
    let Ok(docs) = storage.get_all_documents() else {
        return Vec::new();
    };
    docs.values()
        // EVAL HYGIENE: exclude the test harnesses themselves — they contain the probe queries
        // and gold symbol names verbatim, which would pollute retrieval (self-referential cards).
        .filter(|d| {
            !d.anchor.contains("/tests/")
                && !d
                    .attributes
                    .get("source_file")
                    .is_some_and(|s| s.contains("/tests/"))
        })
        .map(|d| {
            let text = index_text(d);
            (d.anchor.clone(), aden_index::tokenize(&text), text)
        })
        .collect()
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

fn lexicon_path() -> PathBuf {
    std::env::var("ADEN_LEXICON_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache/aden/lexicon")
        })
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    for x in &mut v {
        *x /= norm;
    }
    v
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[derive(Default, Clone)]
struct Metrics {
    r1: usize,
    r5: usize,
    mrr: f64,
}
impl Metrics {
    fn add(&mut self, rank: Option<usize>) {
        if let Some(r) = rank {
            self.r1 += (r == 1) as usize;
            self.r5 += (r <= 5) as usize;
            self.mrr += 1.0 / r as f64;
        }
    }
    fn line(&self, label: &str, n: usize) -> String {
        format!(
            "    {label:<20} R@1 {}/{n}  R@5 {}/{n}  MRR {:.3}",
            self.r1,
            self.r5,
            self.mrr / n as f64
        )
    }
}

#[test]
#[ignore = "concept+typed rerank synthesis (needs --features dense); reads project store + lexicon; writes a JSON artifact"]
fn concept_typed_rerank_report() {
    #[cfg(not(feature = "dense"))]
    {
        eprintln!("SKIP: rebuild with --features dense");
    }
    #[cfg(feature = "dense")]
    {
        use aden_core::EdgeType;
        use aden_index::EmbeddingProvider;
        use aden_store::{GraphStorage, Storage};
        use rayon::prelude::*;
        use std::collections::HashMap;

        let Some(repo) = corpus() else {
            eprintln!("SKIP: corpus dir not found");
            return;
        };
        let cards = load_cards(&repo);
        if cards.is_empty() {
            eprintln!("SKIP: no store cards");
            return;
        }
        let Some(emb) = load_embedder() else {
            eprintln!("SKIP: bge model not found");
            return;
        };
        let lex = Storage::open_existing(lexicon_path().to_str().unwrap_or("")).ok();
        let n = cards.len();

        // Embed every card (parallel). Raw vecs feed the centroid; normalized feed the cosine.
        let card_vecs: Vec<Vec<f32>> = cards.par_iter().map(|(_, _, t)| emb.embed(t)).collect();
        let card_norm: Vec<Vec<f32>> = card_vecs.iter().map(|v| normalize(v.clone())).collect();
        let dim = card_vecs.first().map_or(0, |v| v.len());
        let anchors: Vec<&str> = cards.iter().map(|(a, _, _)| a.as_str()).collect();

        // Postings: concept term -> cards (df-gated, word-like).
        let max_df = (MAX_DF_FRAC * n as f64) as usize;
        let mut postings: HashMap<&str, Vec<usize>> = HashMap::new();
        for (ci, (_, toks, _)) in cards.iter().enumerate() {
            for t in toks {
                let len_ok = (3..=18).contains(&t.len());
                let wordish = t.chars().all(|c| c.is_ascii_alphanumeric())
                    && t.chars().any(|c| c.is_ascii_alphabetic());
                let hex = t.len() >= 8 && t.chars().all(|c| c.is_ascii_hexdigit());
                if len_ok && wordish && !hex {
                    postings.entry(t.as_str()).or_default().push(ci);
                }
            }
        }
        postings.retain(|_, v| (MIN_DF..=max_df).contains(&v.len()));

        let concepts: Vec<&str> = {
            let mut c: Vec<&str> = postings.keys().copied().collect();
            c.sort();
            c
        };
        // Context-centroid per concept: its sense IN this corpus.
        let centroid: HashMap<&str, Vec<f32>> = concepts
            .par_iter()
            .map(|&c| {
                let cc = &postings[c];
                let mut mean = vec![0.0f32; dim];
                for &i in cc {
                    for (m, &x) in mean.iter_mut().zip(&card_vecs[i]) {
                        *m += x;
                    }
                }
                let k = cc.len() as f32;
                for m in &mut mean {
                    *m /= k;
                }
                (c, normalize(mean))
            })
            .collect();
        // Top-KNN concept-graph neighbours per concept, WITH the correlation (= the weight).
        let topk: HashMap<&str, Vec<(&str, f32)>> = concepts
            .par_iter()
            .map(|&c| {
                let cv = &centroid[c];
                let mut sims: Vec<(&str, f32)> = concepts
                    .iter()
                    .filter(|&&o| o != c)
                    .map(|&o| (o, cosine(cv, &centroid[o])))
                    .collect();
                sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                sims.truncate(KNN);
                (c, sims)
            })
            .collect();

        // Hardcoded EdgeType weights (aden-core::activation_weight) — used ONLY by the
        // +OEWN-TYPE comparison arm, to test whether graph-derived weights beat them.
        let etw = |et: &EdgeType| -> f64 {
            match et {
                EdgeType::IsA => 1.0,
                EdgeType::PartOf => 0.9,
                EdgeType::SynonymOf => 0.8,
                _ => 0.0,
            }
        };

        let probes: &[(&str, &[&str], &str)] = &[
            (
                "store a batch of relationships between nodes in one operation",
                &["put_edges_bulk"],
                "append bulk typed edges deduplicate",
            ),
            (
                "group the graph into clusters of tightly connected nodes",
                &["detect_communities"],
                "community detection louvain modularity",
            ),
            (
                "blend two ranked result lists into a single ordering",
                &["rrf_fuse"],
                "reciprocal rank fusion combine rankings",
            ),
            (
                "how aligned are two embedding vectors",
                &["cosine_similarity"],
                "cosine similarity vector",
            ),
            (
                "fewest single character edits to turn one word into another",
                &["levenshtein_distance"],
                "levenshtein edit distance",
            ),
            (
                "figure out which definition a function call points to",
                &["resolve_callee"],
                "resolve callee definition anchor",
            ),
            (
                "decide what category of question the user is asking",
                &["classify_intent"],
                "classify intent query category",
            ),
            (
                "detect a leaked password or api key inside text",
                &["content_has_high_confidence_secret"],
                "secret credential api key detection",
            ),
            (
                "collect the nodes surrounding a starting symbol up to some depth",
                &["build_neighborhood"],
                "neighborhood traversal depth graph",
            ),
            (
                "find everything that points at a given node",
                &["get_incoming_edges"],
                "incoming edges backlinks callers references",
            ),
            (
                "how many tokens were avoided versus reading whole files",
                &["SavingsEstimate"],
                "savings estimate tokens baseline bytes",
            ),
            (
                "anchors in the graph that nothing else references",
                &["scan_orphans"],
                "scan orphan anchors unreferenced dangling",
            ),
        ];
        let np = probes.len();

        let rank_in = |sorted: &[(usize, f32)], accept: &[&str]| -> Option<usize> {
            sorted
                .iter()
                .position(|(i, _)| accept.iter().any(|t| anchors[*i].contains(t)))
                .map(|p| p + 1)
        };

        // Per probe: dense scores; concept neighbours (stem, correlation); corpus-term OEWN
        // neighbours (stem, hardcoded-type-weight, correlation-to-query); accept; dense/oracle.
        struct Pre {
            sims: Vec<(usize, f32)>,
            concept: Vec<(String, f64)>,
            oewn: Vec<(String, f64, f32)>,
            accept: Vec<String>,
        }
        let mut pre: Vec<Pre> = Vec::new();
        let mut dense_m = Metrics::default();
        let mut oracle_m = Metrics::default();
        let mut oewn_raw = 0usize;

        for &(q, accept, oracle) in probes {
            let qv = normalize(emb.embed(q));
            let mut sims: Vec<(usize, f32)> = card_norm
                .iter()
                .enumerate()
                .map(|(i, v)| (i, cosine(&qv, v)))
                .collect();
            sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            dense_m.add(rank_in(&sims, accept));

            let ovec = normalize(emb.embed(&format!("{q} {oracle}")));
            let mut os: Vec<(usize, f32)> = card_norm
                .iter()
                .enumerate()
                .map(|(i, v)| (i, cosine(&ovec, v)))
                .collect();
            os.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            oracle_m.add(rank_in(&os, accept));

            let qtoks = aden_index::tokenize(q);
            // concept-graph neighbours (union over query terms), weight = correlation.
            let mut concept: Vec<(String, f64)> = Vec::new();
            for qt in &qtoks {
                if let Some(ns) = topk.get(qt.as_str()) {
                    for (o, corr) in ns {
                        if !concept.iter().any(|(x, _)| x == o) {
                            concept.push(((*o).to_string(), *corr as f64));
                        }
                    }
                }
            }
            // OEWN typed neighbours that are corpus terms; keep type weight AND query correlation.
            let mut oewn_map: HashMap<String, (f64, f32)> = HashMap::new();
            if let Some(l) = &lex {
                for qt in &qtoks {
                    let Ok(edges) = l.get_outgoing_edges(&format!("aden://term/oewn/{qt}")) else {
                        continue;
                    };
                    for (tgt, et) in edges {
                        let w = etw(&et);
                        if w == 0.0 {
                            continue;
                        }
                        let lemma = tgt.rsplit('/').next().unwrap_or(&tgt);
                        let Some(stem) = aden_index::tokenize(lemma).into_iter().next() else {
                            continue;
                        };
                        let Some(cv) = centroid.get(stem.as_str()) else {
                            continue;
                        };
                        let ctx = cosine(&qv, cv);
                        let e = oewn_map.entry(stem).or_insert((0.0, ctx));
                        e.0 = e.0.max(w);
                    }
                }
            }
            oewn_raw += oewn_map.len();
            let oewn: Vec<(String, f64, f32)> =
                oewn_map.into_iter().map(|(s, (w, c))| (s, w, c)).collect();

            pre.push(Pre {
                sims,
                concept,
                oewn,
                accept: accept.iter().map(|s| s.to_string()).collect(),
            });
        }

        // Rerank the dense top-HEAD by weighted overlap with a neighbour set.
        let rerank = |p: &Pre, nbrs: &[(String, f64)]| -> Option<usize> {
            let head_n = HEAD.min(p.sims.len());
            let maxd = p.sims.first().map(|x| x.1).unwrap_or(1.0).max(1e-9) as f64;
            let mut h: Vec<(usize, f64)> = p.sims[..head_n]
                .iter()
                .map(|&(i, s)| {
                    let toks = &cards[i].1;
                    let boost: f64 = nbrs
                        .iter()
                        .filter(|(stem, _)| toks.iter().any(|t| t == stem))
                        .map(|(_, w)| *w)
                        .sum();
                    (i, s as f64 / maxd + boost)
                })
                .collect();
            h.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            h.iter()
                .map(|x| x.0)
                .chain(p.sims[head_n..].iter().map(|x| x.0))
                .position(|i| p.accept.iter().any(|t| anchors[i].contains(t)))
                .map(|pos| pos + 1)
        };

        // Neighbour-weight builders. `oewn_mode`: None=concept only; Some((use_corr, floor)).
        let nbrs_for = |p: &Pre, oewn_mode: Option<(bool, f32)>| -> Vec<(String, f64)> {
            let mut m: HashMap<String, f64> = HashMap::new();
            for (c, corr) in &p.concept {
                let e = m.entry(c.clone()).or_insert(0.0);
                *e = (*e).max(*corr); // concept edges: weight = graph correlation
            }
            if let Some((use_corr, floor)) = oewn_mode {
                for (stem, tw, ctx) in &p.oewn {
                    if *ctx < floor {
                        continue;
                    }
                    let w = if use_corr { *ctx as f64 } else { *tw };
                    let e = m.entry(stem.clone()).or_insert(0.0);
                    *e = (*e).max(w);
                }
            }
            m.into_iter().collect()
        };

        let mut concept_m = Metrics::default();
        let mut corr_m = Metrics::default();
        let mut type_m = Metrics::default();
        let mut floor_m = Metrics::default();
        let mut kept_floor = 0usize;
        for p in &pre {
            concept_m.add(rerank(p, &nbrs_for(p, None)));
            corr_m.add(rerank(p, &nbrs_for(p, Some((true, -1.0))))); // graph correlation, all
            type_m.add(rerank(p, &nbrs_for(p, Some((false, -1.0))))); // hardcoded type weight
            floor_m.add(rerank(p, &nbrs_for(p, Some((true, FLOOR))))); // correlation + soft gate
            kept_floor += p.oewn.iter().filter(|(_, _, c)| *c >= FLOOR).count();
        }

        // Write the findings: the redefined relationships with their GRAPH-DERIVED weights.
        let edges_json: Vec<serde_json::Value> = {
            let mut v: Vec<serde_json::Value> = concepts
                .iter()
                .map(|&c| {
                    let nbrs: Vec<serde_json::Value> = topk[c]
                        .iter()
                        .map(|(o, w)| serde_json::json!([o, (*w * 1000.0).round() / 1000.0]))
                        .collect();
                    serde_json::json!({"term": c, "neighbours": nbrs})
                })
                .collect();
            v.sort_by(|a, b| a["term"].as_str().cmp(&b["term"].as_str()));
            v
        };
        let out = PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join(".cache/aden/dict/concept-graph-weighted.json");
        let _ = std::fs::write(
            &out,
            serde_json::to_string(&serde_json::json!({
                "note": "edges with graph-derived correlation weights (the findings, not the dictionary's)",
                "weight": "cosine(centroid[term], centroid[neighbour])",
                "concepts": concepts.len(),
                "edges": edges_json,
            }))
            .unwrap_or_default(),
        );

        println!(
            "\n=== Concept + typed-OEWN rerank — graph-derived weights ({np} probes, {n} cards) ==="
        );
        println!(
            "  lexicon: {}   corpus-term OEWN neighbours: {oewn_raw} total ({:.1}/probe)",
            if lex.is_some() { "LIVE" } else { "ABSENT" },
            oewn_raw as f64 / np as f64
        );
        println!("\n  rank-of-gold:");
        println!("{}", dense_m.line("DENSE", np));
        println!("{}", concept_m.line("CONCEPT", np));
        println!(
            "{}   <- the model (graph sets the weight)",
            corr_m.line("+OEWN-CORR", np)
        );
        println!(
            "{}   <- hardcoded type weights",
            type_m.line("+OEWN-TYPE", np)
        );
        println!(
            "{}   [kept {kept_floor} OEWN]",
            floor_m.line(&format!("+OEWN-CORR>={FLOOR:.2}"), np)
        );
        println!("{}", oracle_m.line("ORACLE", np));
        println!(
            "\n  findings (weighted edges) -> {}  ({} concepts)",
            out.display(),
            concepts.len()
        );

        assert!(!pre.is_empty(), "no probes");
    }
}
