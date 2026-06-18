// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Consolidated lever comparison on a CLEAN corpus (test cards excluded — no answer-key leak).
// Measures every retrieval lever at once over the expanded probe set so the deltas are trusted:
//   DENSE          — bge cosine baseline
//   CONCEPT        — rerank by direct concept-graph neighbours (correlation-weighted)
//   MH-MAX         — multi-hop depth-2, node weight = BEST path (correlation product * decay)
//   MH-ACCUM       — multi-hop depth-2, node weight = ACCUMULATED path mass (RioPlay's idea:
//                    let path multiplicity/convergence set the weight, not a single path)
//   OEWN           — concept neighbours UNION typed OEWN lexicon neighbours (the dictionary lever)
//   OEWN+MH        — multi-hop UNION OEWN (do the two biggest clean levers COMPOUND?)
//   ORACLE         — hand-authored expansion (upper bound)
// (measurement harness, #[ignore]d, needs --features dense; reads project store + lexicon;
// writes nothing; .aden/store untouched.)
//
// Run: cargo test -p aden-cli --features dense --test retrieval_levers_ab -- --include-ignored --nocapture

#![cfg_attr(not(feature = "dense"), allow(dead_code))]

use std::path::{Path, PathBuf};

const MIN_DF: usize = 4;
const MAX_DF_FRAC: f64 = 0.15;
const KNN: usize = 4; // concept-graph neighbours per node
const HEAD: usize = 20; // rerank depth
const GAMMA: f64 = 0.5; // per-hop decay
const FLOOR: f64 = 0.05; // abandon a path below this weight
const DEPTH: usize = 2; // multi-hop depth (saturates here on this graph)

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

/// Per-card (anchor, token set, indexed text). EVAL HYGIENE: excludes the test harnesses —
/// they pair each probe query with its gold symbol name in one card, leaking the answer key.
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

/// Union neighbour-weight lists, keeping the max weight per stem.
fn union(parts: &[&[(String, f64)]]) -> Vec<(String, f64)> {
    use std::collections::HashMap;
    let mut m: HashMap<&str, f64> = HashMap::new();
    for part in parts {
        for (s, w) in part.iter() {
            let e = m.entry(s.as_str()).or_insert(0.0);
            *e = e.max(*w);
        }
    }
    m.into_iter().map(|(s, w)| (s.to_string(), w)).collect()
}

/// (dense top1-top2 margin, dense sims, OEWN∪multi-hop neighbours, gold) per probe.
type GatedRow = (
    f32,
    Vec<(usize, f32)>,
    Vec<(String, f64)>,
    &'static [&'static str],
);

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
            "    {label:<10} R@1 {:>2}/{n}  R@5 {:>2}/{n}  MRR {:.3}",
            self.r1,
            self.r5,
            self.mrr / n as f64
        )
    }
}

#[test]
#[ignore = "consolidated retrieval lever comparison (needs --features dense); clean corpus; writes nothing"]
fn retrieval_levers_report() {
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

        let card_vecs: Vec<Vec<f32>> = cards.par_iter().map(|(_, _, t)| emb.embed(t)).collect();
        let card_norm: Vec<Vec<f32>> = card_vecs.iter().map(|v| normalize(v.clone())).collect();
        let dim = card_vecs.first().map_or(0, |v| v.len());
        let anchors: Vec<&str> = cards.iter().map(|(a, _, _)| a.as_str()).collect();

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

        // Multi-hop BFS. `accumulate`: final node weight is SUM of arriving path mass (rewards
        // convergence) vs MAX single path. Propagation to the next hop always uses the best path.
        let expand = |qtoks: &[String], depth: usize, accumulate: bool| -> Vec<(String, f64)> {
            let mut best: HashMap<&str, f64> = HashMap::new();
            let mut frontier: Vec<(&str, f64)> = Vec::new();
            for qt in qtoks {
                if let Some((k, _)) = topk.get_key_value(qt.as_str()) {
                    frontier.push((*k, 1.0));
                }
            }
            for hop in 1..=depth {
                let mult = if hop == 1 { 1.0 } else { GAMMA };
                let mut next: HashMap<&str, f64> = HashMap::new();
                for &(node, w) in &frontier {
                    let Some(ns) = topk.get(node) else { continue };
                    for &(nbr, corr) in ns {
                        let nw = w * corr as f64 * mult;
                        if nw < FLOOR || qtoks.iter().any(|q| q.as_str() == nbr) {
                            continue;
                        }
                        best.entry(nbr)
                            .and_modify(|e| *e = if accumulate { *e + nw } else { e.max(nw) })
                            .or_insert(nw);
                        next.entry(nbr).and_modify(|e| *e = e.max(nw)).or_insert(nw);
                    }
                }
                if next.is_empty() {
                    break;
                }
                frontier = next.into_iter().collect();
            }
            best.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
        };

        // OEWN typed neighbours that are corpus terms; weight = graph correlation to the query.
        let etw = |et: &EdgeType| -> bool {
            matches!(et, EdgeType::IsA | EdgeType::PartOf | EdgeType::SynonymOf)
        };
        let oewn_nbrs = |qtoks: &[String], qv: &[f32]| -> Vec<(String, f64)> {
            let mut m: HashMap<String, f64> = HashMap::new();
            if let Some(l) = &lex {
                for qt in qtoks {
                    let Ok(edges) = l.get_outgoing_edges(&format!("aden://term/oewn/{qt}")) else {
                        continue;
                    };
                    for (tgt, et) in edges {
                        if !etw(&et) {
                            continue;
                        }
                        let lemma = tgt.rsplit('/').next().unwrap_or(&tgt);
                        let Some(stem) = aden_index::tokenize(lemma).into_iter().next() else {
                            continue;
                        };
                        let Some(cv) = centroid.get(stem.as_str()) else {
                            continue;
                        };
                        let w = cosine(qv, cv) as f64;
                        let e = m.entry(stem).or_insert(0.0);
                        *e = e.max(w);
                    }
                }
            }
            m.into_iter().collect()
        };

        let rerank =
            |sims: &[(usize, f32)], nbrs: &[(String, f64)], accept: &[&str]| -> Option<usize> {
                let head_n = HEAD.min(sims.len());
                let maxd = sims.first().map(|x| x.1).unwrap_or(1.0).max(1e-9) as f64;
                let mut h: Vec<(usize, f64)> = sims[..head_n]
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
                    .chain(sims[head_n..].iter().map(|x| x.0))
                    .position(|i| accept.iter().any(|t| anchors[i].contains(t)))
                    .map(|p| p + 1)
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
            // --- expanded set (mined via aden, gold symbols verified, queries de-leaked) ---
            (
                "remove formatting noise from a documentation string so an LLM receives only semantic content with no structural overhead",
                &["strip_asciidoc_markup"],
                "markup tables delimiters anchor llm",
            ),
            (
                "gather a subgraph around a starting node into a text prompt and return the list of included node identifiers in visit order",
                &["assemble_with_anchors"],
                "bfs neighborhood context budget traversal",
            ),
            (
                "find relevant symbols by combining keyword ranking with vector similarity and merging the two result lists",
                &["hybrid_query"],
                "bm25 dense rrf retrieval fuse ranking",
            ),
            (
                "run a neural encoder over every indexed contract and persist the resulting vectors for future similarity lookups",
                &["embed_documents"],
                "bge onnx corpus incremental provider vectors",
            ),
            (
                "produce the canonical form of a contract used for fingerprinting and encoding by dropping the provenance attributes that change on every run",
                &["stable_embed_text"],
                "last-verified span source_hash projection",
            ),
            (
                "expand a single search term into all its equivalent canonical representations such as numbers months ordinals and booleans",
                &["SemanticNormalizer"],
                "canonical bm25 temporal ordinal synonym",
            ),
            (
                "break a camelCase or snake-case identifier into its component lowercase words",
                &["split_subtokens"],
                "separator identifier components word humps",
            ),
            (
                "check whether a query word lines up with an identifier's word edges rather than appearing only as a raw interior substring",
                &["token_boundary_match"],
                "edge subword camelcase prefix",
            ),
            (
                "perform a three-way merge of a freshly-parsed symbol against the stored base and the human-intent overlay to produce a conflict-free result",
                &["reconcile_contract"],
                "ground base working overlay three-way merge",
            ),
            (
                "determine whether a pending three-way reconciliation has no outstanding conflicts and is safe to apply automatically",
                &["is_clean"],
                "conflict-free auto-apply actions",
            ),
            (
                "read a region-tagged AsciiDoc text into a structured in-memory representation of generated and human blocks",
                &["parse_contract"],
                "region block asciidoc strict permissive",
            ),
            (
                "serialize the in-memory form of generated and human regions back to region-tagged AsciiDoc for storage",
                &["emit_contract_document"],
                "region block asciidoc serializer canonical",
            ),
            (
                "fingerprint a file's contents with line endings normalised so identical content yields the same value on Windows and Linux",
                &["hash_source"],
                "crlf lf normalization change-detection drift",
            ),
            (
                "load a knowledge-graph node from raw bytes and reconstruct its line-range metadata from stored attributes when the struct field is absent",
                &["deserialize_document"],
                "rehydrate source_span postcard attributes",
            ),
            (
                "drop the redundant callee-listing block from a knowledge node before persisting it on disk to keep its size down",
                &["slim_doc_for_store"],
                "callee listing block size redundant",
            ),
            (
                "remove the absolute host path prefix from a document's path attribute so no username or home directory leaks into stored or model-visible context",
                &["sanitize_source_file"],
                "absolute path prefix strip security context",
            ),
            (
                "show which symbols transitively depend on code touched by the current git working-tree changes together with covering tests",
                &["cmd_impact_diff"],
                "blast radius dependents git transitive tests",
            ),
            (
                "remove from the graph store all nodes whose originating file no longer exists on disk",
                &["cmd_heal_gc"],
                "orphaned stale node prune sweep deleted",
            ),
            (
                "create a structured fix suggestion for a detected contract drift event optionally using the three-way merge engine when the anchor is in the store",
                &["generate_proposal"],
                "drift event fix suggestion anchor",
            ),
            (
                "overwrite the content-fingerprint line in a contract file with the current value to resolve a drift warning",
                &["apply_stale_hash"],
                "source_hash fingerprint line overwrite drift",
            ),
            (
                "write a new generated region block into a source file that lacks its corresponding documentation node",
                &["apply_missing_contract"],
                "absent block region documentation node",
            ),
            (
                "given a categorized query goal return the set of graph relationship kinds most relevant to traverse",
                &["edge_types_for_intent"],
                "queryintent traversal relationship category",
            ),
            (
                "given a categorized query goal return the maximum number of hops to traverse during context assembly",
                &["depth_for_intent"],
                "queryintent traversal hops budget maximum",
            ),
            (
                "given a categorized query goal return which AST node kinds to include when building the context window",
                &["block_filter_for_intent"],
                "queryintent blockkind admonition paragraph",
            ),
            (
                "run a cheap file-modification sweep and if any source changed silently re-index just those files before serving a read command",
                &["ensure_fresh"],
                "mtime sweep incremental reindex stale",
            ),
            (
                "given a free-text description of what a user wants to do print the aden subcommand that best matches",
                &["cmd_suggest"],
                "subcommand recommendation free-text",
            ),
            (
                "access an already-built key-value store at a path returning an error rather than creating one when the directory is absent",
                &["open_existing"],
                "lsm fjall read-only absent notfound",
            ),
            (
                "a struct wrapping an ONNX runtime session and tokenizer that turns text strings into dense float vectors",
                &["TractEmbedder"],
                "onnx runtime tokenizer inference dense float32",
            ),
        ];
        let np = probes.len();

        // SELF-AUDIT of the dataset: a probe is INVALID if it leaks its answer (a >=4-char
        // sub-token of the gold appears in the query) or is DEAD (no card anchor holds the gold).
        // Either silently corrupts the eval, so report both before trusting the numbers.
        let (mut leaks, mut dead) = (0usize, 0usize);
        for &(q, accept, _) in probes {
            let qset: std::collections::HashSet<String> =
                aden_index::tokenize(q).into_iter().collect();
            for a in accept {
                let spaced = a.replace("::", " ").replace('_', " ");
                let shared: Vec<String> = aden_index::tokenize(&spaced)
                    .into_iter()
                    .filter(|t| t.len() >= 4 && qset.contains(t))
                    .collect();
                if !shared.is_empty() {
                    leaks += 1;
                    println!("  LEAK  {a}: query shares {shared:?}");
                }
                if !anchors.iter().any(|an| an.contains(a)) {
                    dead += 1;
                    println!("  DEAD  {a}: no card anchor contains it");
                }
            }
        }
        println!("  dataset audit: {leaks} leak(s), {dead} dead gold(s) over {np} probes");

        let rank_in = |sorted: &[(usize, f32)], accept: &[&str]| -> Option<usize> {
            sorted
                .iter()
                .position(|(i, _)| accept.iter().any(|t| anchors[*i].contains(t)))
                .map(|p| p + 1)
        };

        // Pseudo-relevance feedback (Rocchio in embedding space) — a NATURALLY generated query
        // expansion: pull the query vector toward its own top-k dense results (each weighted by
        // its dense score, so confident hits dominate), then re-retrieve over ALL cards. No
        // dictionary, no graph; the corpus's own response IS the expansion. Targets the oracle's
        // "add the right domain terms" effect automatically.
        let prf = |qv: &[f32],
                   sims: &[(usize, f32)],
                   accept: &[&str],
                   alpha: f64,
                   k: usize|
         -> Option<usize> {
            let mut q1 = qv.to_vec();
            for &(i, s) in sims.iter().take(k) {
                let w = alpha as f32 * s; // weight feedback by the doc's own dense score
                for (a, &b) in q1.iter_mut().zip(&card_norm[i]) {
                    *a += w * b;
                }
            }
            let q1 = normalize(q1);
            let mut s: Vec<(usize, f32)> = card_norm
                .iter()
                .enumerate()
                .map(|(i, v)| (i, cosine(&q1, v)))
                .collect();
            s.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            rank_in(&s, accept)
        };

        let (mut dense_m, mut concept_m, mut mhmax_m, mut mhaccum_m) = (
            Metrics::default(),
            Metrics::default(),
            Metrics::default(),
            Metrics::default(),
        );
        let (mut oewn_m, mut compound_m, mut oracle_m) =
            (Metrics::default(), Metrics::default(), Metrics::default());
        // Per-probe rows for the data-derived confidence-gate pass (see GatedRow).
        let mut gated: Vec<GatedRow> = Vec::new();
        let (mut prf1_m, mut prf2_m, mut prf3_m) =
            (Metrics::default(), Metrics::default(), Metrics::default());

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
            let nbr_concept = expand(&qtoks, 1, false);
            let nbr_mhmax = expand(&qtoks, DEPTH, false);
            let nbr_mhaccum = expand(&qtoks, DEPTH, true);
            let nbr_oewn = oewn_nbrs(&qtoks, &qv);

            concept_m.add(rerank(&sims, &nbr_concept, accept));
            mhmax_m.add(rerank(&sims, &nbr_mhmax, accept));
            mhaccum_m.add(rerank(&sims, &nbr_mhaccum, accept));
            oewn_m.add(rerank(&sims, &union(&[&nbr_concept, &nbr_oewn]), accept));
            compound_m.add(rerank(&sims, &union(&[&nbr_mhmax, &nbr_oewn]), accept));

            let margin = sims[0].1 - sims.get(1).map_or(0.0, |x| x.1);
            gated.push((
                margin,
                sims.clone(),
                union(&[&nbr_mhmax, &nbr_oewn]),
                accept,
            ));

            prf1_m.add(prf(&qv, &sims, accept, 0.5, 5));
            prf2_m.add(prf(&qv, &sims, accept, 1.0, 5));
            prf3_m.add(prf(&qv, &sims, accept, 1.0, 10));
        }

        // Confidence-gated rerank — gate DERIVED FROM THE DATA, not a constant. Confidence =
        // dense top-1/top-2 margin; the cutoff is the MEDIAN margin across the query set, so
        // "uncertain" is defined by the distribution itself, not a hand-set threshold.
        let mut margins: Vec<f32> = gated.iter().map(|g| g.0).collect();
        margins.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = margins.get(margins.len() / 2).copied().unwrap_or(0.0);
        let (mut hard_m, mut soft_m) = (Metrics::default(), Metrics::default());
        let mut gated_n = 0usize;
        for (margin, sims, nbrs, accept) in &gated {
            if *margin < median {
                gated_n += 1;
                hard_m.add(rerank(sims, nbrs, accept)); // rerank only the uncertain queries
            } else {
                hard_m.add(rank_in(sims, accept)); // trust dense when it is confident
            }
            let scale = (1.0 - (*margin as f64) / (median as f64 + 1e-9)).max(0.0);
            let faded: Vec<(String, f64)> =
                nbrs.iter().map(|(s, w)| (s.clone(), w * scale)).collect();
            soft_m.add(rerank(sims, &faded, accept)); // boost faded continuously by confidence
        }

        println!(
            "\n=== Retrieval levers — CLEAN corpus ({np} probes, {n} cards, lexicon {}) ===",
            if lex.is_some() { "LIVE" } else { "ABSENT" }
        );
        println!("{}", dense_m.line("DENSE", np));
        println!("{}", concept_m.line("CONCEPT", np));
        println!("{}", mhmax_m.line("MH-MAX", np));
        println!("{}", mhaccum_m.line("MH-ACCUM", np));
        println!("{}", oewn_m.line("OEWN", np));
        println!("{}", compound_m.line("OEWN+MH", np));
        println!("{}", oracle_m.line("ORACLE", np));
        println!(
            "{}   [gated {gated_n}/{np}, median margin {median:.3}]",
            hard_m.line("GATE-HARD", np)
        );
        println!("{}", soft_m.line("GATE-SOFT", np));
        println!("{}", prf1_m.line("PRF .5/5", np));
        println!("{}", prf2_m.line("PRF 1/5", np));
        println!("{}", prf3_m.line("PRF 1/10", np));

        assert!(!probes.is_empty(), "no probes");
    }
}
