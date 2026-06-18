// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Concept graph — the population layer (RioPlay's design): nodes correspond to parts of
// speech, and their relationships/overlap are graphed by the model from how they're used in
// the corpus. (measurement harness, #[ignore]d, needs --features dense, writes a JSON
// artifact; does NOT touch .aden/store.)
//
// Each corpus concept (a distinctive term) becomes a node with:
//   * a PART OF SPEECH (from OEWN; code terms with no entry are tagged `?`), and
//   * a CONTEXT-CENTROID embedding — the model (bge) averaged over the cards the concept
//     appears in. That centroid is the concept's *sense in THIS corpus*, so the relationships
//     are sense-correct by construction: `cluster` lands near `community`/`louvain` here, not
//     near `flock` like the generic dictionary. This is the graph-context-gated meaning the
//     WSD wall demanded — the model makes the connections, grounded in usage.
//
// Relationships = mutual-kNN over the concept centroids (A↔B only if each is in the other's
// top-k): a `SimilarTo` overlap edge between concepts the model finds semantically close.
//
// Run: cargo test -p aden-cli --features dense --test concept_graph -- --include-ignored --nocapture

#![cfg_attr(not(feature = "dense"), allow(dead_code))]

use std::path::{Path, PathBuf};

const MIN_DF: usize = 4;
const MAX_DF_FRAC: f64 = 0.15;
const KNN: usize = 8;

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
        // EVAL HYGIENE: exclude the test harnesses themselves — they pair each probe query with
        // its gold symbol name in one card, which LEAKS the answer key into the concept graph.
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

/// Part of speech for a term from the OEWN wordset (first meaning's `speech_part`); `?` when
/// the term has no dictionary entry (most code-specific identifiers).
fn pos_of(term: &str, wordset: &serde_json::Value) -> String {
    wordset
        .get(term)
        .and_then(|e| e.get("meanings"))
        .and_then(|m| m.as_array())
        .and_then(|a| a.first())
        .and_then(|m| m.get("speech_part"))
        .and_then(|s| s.as_str())
        .map(|s| s.chars().next().unwrap_or('?').to_string())
        .unwrap_or_else(|| "?".to_string())
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

#[test]
#[ignore = "concept graph population layer (needs --features dense); writes a JSON artifact, not .aden/store"]
fn concept_graph_report() {
    #[cfg(not(feature = "dense"))]
    {
        eprintln!("SKIP: rebuild with --features dense");
    }
    #[cfg(feature = "dense")]
    {
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
        let n = cards.len();

        // The model embeds every card (parallel — the production path).
        let card_vecs: Vec<Vec<f32>> = {
            use aden_index::EmbeddingProvider;
            cards.par_iter().map(|(_, _, t)| emb.embed(t)).collect()
        };
        let dim = card_vecs.first().map_or(0, |v| v.len());

        // Postings: concept term -> cards it appears in (df-gated, word-like).
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

        // Each concept's CONTEXT-CENTROID: the model's view of its meaning, averaged over the
        // cards it's used in — sense-grounded in THIS corpus.
        let concepts: Vec<&str> = {
            let mut c: Vec<&str> = postings.keys().copied().collect();
            c.sort();
            c
        };
        let centroid: HashMap<&str, Vec<f32>> = concepts
            .par_iter()
            .map(|&c| {
                let cards_c = &postings[c];
                let mut mean = vec![0.0f32; dim];
                for &i in cards_c {
                    for (m, &x) in mean.iter_mut().zip(&card_vecs[i]) {
                        *m += x;
                    }
                }
                let k = cards_c.len() as f32;
                for m in &mut mean {
                    *m /= k;
                }
                (c, normalize(mean))
            })
            .collect();

        // POS tags from OEWN.
        let wordset: serde_json::Value = std::fs::read_to_string(
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".cache/aden/dict/oewn-triples.json"),
        )
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);
        let pos: HashMap<&str, String> =
            concepts.iter().map(|&c| (c, pos_of(c, &wordset))).collect();

        // top-KNN neighbours per concept (by the model's context similarity).
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

        // Mutual-kNN SimilarTo edges (A↔B only if each is in the other's top-k).
        let mut edges = 0usize;
        for &c in &concepts {
            for (o, _) in &topk[c] {
                if c < *o && topk[o].iter().any(|(x, _)| x == &c) {
                    edges += 1;
                }
            }
        }

        let pos_name = |p: &str| match p {
            "n" => "noun",
            "v" => "verb",
            "a" | "s" => "adj",
            "r" => "adv",
            _ => "?",
        };

        println!("\n=== Concept graph — POS-tagged, model-graphed relationships ===");
        println!(
            "Concept nodes: {} | mutual-SimilarTo edges: {} | {n} cards, dim {dim}",
            concepts.len(),
            edges
        );
        println!(
            "\n-- sample concepts: POS + the relationships the model graphed (corpus-grounded) --"
        );
        let seeds = [
            "cluster", "store", "secret", "vector", "merge", "rank", "orphan", "token", "neighbor",
            "fuse", "anchor", "embed",
        ];
        for s in seeds {
            match topk.get(s) {
                Some(nbrs) => {
                    let shown: Vec<String> = nbrs
                        .iter()
                        .take(KNN)
                        .map(|(o, sim)| format!("{o}·{}({sim:.2})", pos_name(&pos[o])))
                        .collect();
                    println!("  {s:<10} [{}]  → {}", pos_name(&pos[s]), shown.join(", "));
                }
                None => println!("  {s:<10} (below df threshold / absent)"),
            }
        }

        // --- Payoff: retrieval THROUGH the concept graph (does the bridge close the gap?) ---
        // Walk each query term to its sense-correct concept neighbours and add them to the
        // query before re-embedding — the dense match then lands nearer the gold symbol.
        use aden_index::EmbeddingProvider;
        let card_norm: Vec<Vec<f32>> = card_vecs.iter().map(|v| normalize(v.clone())).collect();
        let anchors: Vec<&str> = cards.iter().map(|(a, _, _)| a.as_str()).collect();
        let rank_gold = |q: &str, accept: &[&str]| -> Option<usize> {
            let qv = normalize(emb.embed(q));
            let mut sims: Vec<(usize, f32)> = card_norm
                .iter()
                .enumerate()
                .map(|(i, v)| (i, cosine(&qv, v)))
                .collect();
            sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            sims.iter()
                .position(|(i, _)| accept.iter().any(|t| anchors[*i].contains(t)))
                .map(|p| p + 1)
        };
        // Expand a query with the model-graphed neighbours of each of its terms (top-3 each).
        let expand = |q: &str| -> String {
            let mut extra: Vec<&str> = Vec::new();
            for t in aden_index::tokenize(q) {
                if let Some(nbrs) = topk.get(t.as_str()) {
                    for (o, _) in nbrs.iter().take(3) {
                        if !extra.contains(o) {
                            extra.push(o);
                        }
                    }
                }
            }
            format!("{q} {}", extra.join(" "))
        };
        // RERANK (the right consumer): keep dense's order, then boost top-20 candidates that
        // actually CONTAIN the query terms' concept-graph neighbours — a lexical-overlap signal
        // with no embedding drift. The opposite of expansion: add the relations to the SCORE,
        // not to the query text.
        let rerank_gold = |q: &str, accept: &[&str]| -> Option<usize> {
            let qv = normalize(emb.embed(q));
            let mut sims: Vec<(usize, f32)> = card_norm
                .iter()
                .enumerate()
                .map(|(i, v)| (i, cosine(&qv, v)))
                .collect();
            sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            let mut nbr: Vec<&str> = Vec::new();
            for t in aden_index::tokenize(q) {
                if let Some(ns) = topk.get(t.as_str()) {
                    for (o, _) in ns.iter().take(KNN) {
                        if !nbr.contains(o) {
                            nbr.push(o);
                        }
                    }
                }
            }
            let head_n = 20.min(sims.len());
            let maxd = sims.first().map(|x| x.1).unwrap_or(1.0).max(1e-9);
            let mut head: Vec<(usize, f32)> = sims[..head_n]
                .iter()
                .map(|&(i, s)| {
                    let toks = &cards[i].1;
                    let overlap =
                        nbr.iter().filter(|w| toks.iter().any(|t| t == *w)).count() as f32;
                    (i, s / maxd + 0.5 * overlap)
                })
                .collect();
            head.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            head.iter()
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
        ];
        let np = probes.len();
        let (mut d5, mut c5, mut r5, mut o5) = (0usize, 0usize, 0usize, 0usize);
        let (mut dm, mut cm, mut rm, mut om) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for (q, accept, oracle) in probes {
            for (r, hits, mrr) in [
                (rank_gold(q, accept), &mut d5, &mut dm),
                (rank_gold(&expand(q), accept), &mut c5, &mut cm),
                (rerank_gold(q, accept), &mut r5, &mut rm),
                (
                    rank_gold(&format!("{q} {oracle}"), accept),
                    &mut o5,
                    &mut om,
                ),
            ] {
                if let Some(rk) = r {
                    if rk <= 5 {
                        *hits += 1;
                    }
                    *mrr += 1.0 / rk as f64;
                }
            }
        }
        println!("\n  retrieval THROUGH the concept graph (R@5 / MRR, {np} probes):");
        println!(
            "    DENSE            R@5 {d5}/{np}   MRR {:.3}",
            dm / np as f64
        );
        println!(
            "    + CONCEPT-GRAPH  R@5 {c5}/{np}   MRR {:.3}  (expansion — wrong consumer)",
            cm / np as f64
        );
        println!(
            "    + CONCEPT-RERANK R@5 {r5}/{np}   MRR {:.3}  (rerank — right consumer)",
            rm / np as f64
        );
        println!(
            "    + ORACLE         R@5 {o5}/{np}   MRR {:.3}",
            om / np as f64
        );

        // Write the graph artifact (nodes + edges) — NOT to .aden/store.
        let nodes_json: Vec<serde_json::Value> = concepts
            .iter()
            .map(|&c| serde_json::json!({"concept": c, "pos": pos_name(&pos[c])}))
            .collect();
        let out = PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join(".cache/aden/dict/concept-graph.json");
        let _ = std::fs::write(
            &out,
            serde_json::to_string(&serde_json::json!({"nodes": nodes_json, "edge_count": edges}))
                .unwrap_or_default(),
        );
        println!(
            "\nWrote concept graph ({} nodes) -> {}",
            concepts.len(),
            out.display()
        );
        println!("(.aden/store untouched.)");

        assert!(!concepts.is_empty(), "no concepts");
    }
}
