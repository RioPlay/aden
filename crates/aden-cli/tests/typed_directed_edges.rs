// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The keystone — make the graph TYPED and DIRECTED (RioPlay's "fit language as a whole").
// Today the concept graph is one untyped, undirected `SimilarTo` edge per kNN pair. Language
// isn't: hypernymy is directed and asymmetric (`cluster` IsA `grouping`, not the reverse), and
// it only holds between compatible parts of speech. This harness builds the first typed/directed
// layer over the SAME corpus concepts, from the OEWN lexicon overlay:
//   * IsA  / PartOf  — DIRECTED (source -> target), noun-gated (no hypernymy for adj/verb here),
//   * SynonymOf      — UNDIRECTED, same-POS,
// and every edge is gated by graph context-correlation (cosine of the two corpus-sense centroids
// >= TAU) — the WSD gate: a wrong-sense dictionary suggestion correlates weakly with the concept
// in THIS corpus and is dropped. Demonstrates the three gates firing (POS, relevance, WSD), the
// asymmetry of the directed edges, and writes the typed edges out as findings.
// (measurement harness, #[ignore]d, needs --features dense; reads project store + lexicon;
// writes a JSON artifact; does NOT touch .aden/store.)
//
// Run: cargo test -p aden-cli --features dense --test typed_directed_edges -- --include-ignored --nocapture

#![cfg_attr(not(feature = "dense"), allow(dead_code))]

use std::path::{Path, PathBuf};

const MIN_DF: usize = 4;
const MAX_DF_FRAC: f64 = 0.15;
const TAU: f32 = 0.90; // WSD gate: keep an edge only if the two corpus-sense centroids correlate >= TAU

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

/// Per-card (token set, indexed text) for the whole store.
fn load_cards(repo: &Path) -> Vec<(Vec<String>, String)> {
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
        .map(|d| {
            let text = index_text(d);
            (aden_index::tokenize(&text), text)
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

/// Part of speech for a term from the OEWN wordset (`n`/`v`/`a`/`s`/`r`); `?` when absent.
fn pos_of(term: &str, wordset: &serde_json::Value) -> char {
    wordset
        .get(term)
        .and_then(|e| e.get("meanings"))
        .and_then(|m| m.as_array())
        .and_then(|a| a.first())
        .and_then(|m| m.get("speech_part"))
        .and_then(|s| s.as_str())
        .and_then(|s| s.chars().next())
        .unwrap_or('?')
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
#[ignore = "typed/directed edge producer (needs --features dense); reads project store + lexicon; writes a JSON artifact"]
fn typed_directed_edges_report() {
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
        let Some(lex) = Storage::open_existing(lexicon_path().to_str().unwrap_or("")).ok() else {
            eprintln!("SKIP: lexicon store not found (run build_lexicon_store)");
            return;
        };
        let n = cards.len();

        let card_vecs: Vec<Vec<f32>> = cards.par_iter().map(|(_, t)| emb.embed(t)).collect();
        let dim = card_vecs.first().map_or(0, |v| v.len());

        let max_df = (MAX_DF_FRAC * n as f64) as usize;
        let mut postings: HashMap<&str, Vec<usize>> = HashMap::new();
        for (ci, (toks, _)) in cards.iter().enumerate() {
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

        // POS for every concept (from the OEWN wordset).
        let wordset: serde_json::Value = std::fs::read_to_string(
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".cache/aden/dict/oewn-triples.json"),
        )
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);
        let pos: HashMap<&str, char> = concepts.iter().map(|&c| (c, pos_of(c, &wordset))).collect();
        let mut pos_hist: HashMap<char, usize> = HashMap::new();
        for &p in pos.values() {
            *pos_hist.entry(p).or_default() += 1;
        }

        // Per-relation gate counters: (offered, dropped_pos, dropped_relevance, dropped_wsd, kept).
        #[derive(Default)]
        struct Gate {
            offered: usize,
            pos: usize,
            relevance: usize,
            wsd: usize,
            kept: usize,
        }
        let mut isa = Gate::default();
        let mut part = Gate::default();
        let mut syn = Gate::default();

        struct TEdge {
            src: String,
            rel: &'static str,
            directed: bool,
            tgt: String,
            w: f32,
        }
        let mut edges: Vec<TEdge> = Vec::new();
        // For an O(1) directionality check later.
        let mut isa_pairs: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();

        for &c in &concepts {
            let Ok(out) = lex.get_outgoing_edges(&format!("aden://term/oewn/{c}")) else {
                continue;
            };
            let cp = pos[c];
            let cv = &centroid[c];
            for (tgt_anchor, et) in out {
                // route only the typed lexical relations we model here
                let (gate, rel, directed, noun_src) = match et {
                    EdgeType::IsA => (&mut isa, "IsA", true, true),
                    EdgeType::PartOf => (&mut part, "PartOf", true, true),
                    EdgeType::SynonymOf => (&mut syn, "SynonymOf", false, false),
                    _ => continue,
                };
                gate.offered += 1;

                // POS gate: IsA/PartOf only from a noun source (no hypernymy for adj/verb here).
                if noun_src && cp != 'n' {
                    gate.pos += 1;
                    continue;
                }
                let tgt_lemma = tgt_anchor.rsplit('/').next().unwrap_or(&tgt_anchor);
                let Some(tgt) = aden_index::tokenize(tgt_lemma).into_iter().next() else {
                    continue;
                };
                // Relevance gate: target must be a corpus concept (else it can't be part of the
                // graph over this corpus); SynonymOf additionally must share POS.
                let Some(tv) = centroid.get(tgt.as_str()) else {
                    gate.relevance += 1;
                    continue;
                };
                if tgt.as_str() == c {
                    gate.relevance += 1;
                    continue;
                }
                // POS gate on the TARGET: a noun's hypernym/holonym must be a noun (IsA/PartOf);
                // a synonym must share the source POS. Without this, a lemma's verb-sense edges
                // leak into its noun node ("question" IsA "ask").
                let tp = pos.get(tgt.as_str()).copied().unwrap_or('?');
                if (noun_src && tp != 'n') || (!directed && tp != cp) {
                    gate.pos += 1;
                    continue;
                }
                // WSD gate: the two corpus-sense centroids must correlate.
                let w = cosine(cv, tv);
                if w < TAU {
                    gate.wsd += 1;
                    continue;
                }
                gate.kept += 1;
                if rel == "IsA" {
                    isa_pairs.insert((c.to_string(), tgt.clone()));
                }
                edges.push(TEdge {
                    src: c.to_string(),
                    rel,
                    directed,
                    tgt,
                    w,
                });
            }
        }

        // Direction proof: how many kept IsA edges have their REVERSE also asserted (should be ~0
        // — hypernymy is asymmetric).
        let isa_reversed = isa_pairs
            .iter()
            .filter(|(a, b)| isa_pairs.contains(&(b.clone(), a.clone())))
            .count();

        println!(
            "\n=== Typed/directed edge producer ({} concepts, {n} cards, TAU {TAU}) ===",
            concepts.len()
        );
        let mut ph: Vec<(char, usize)> = pos_hist.into_iter().collect();
        ph.sort_by_key(|&(_, k)| std::cmp::Reverse(k));
        let pn = |p: char| match p {
            'n' => "noun",
            'v' => "verb",
            'a' | 's' => "adj",
            'r' => "adv",
            _ => "none",
        };
        println!(
            "  concept POS: {}",
            ph.iter()
                .map(|(p, k)| format!("{}={k}", pn(*p)))
                .collect::<Vec<_>>()
                .join("  ")
        );
        println!("\n  edge gates (offered -> kept), each gate shown:");
        let row = |label: &str, dir: &str, g: &Gate| {
            println!(
                "    {label:<10} [{dir:<10}] offered {:>5}  -POS {:>5}  -off-corpus {:>5}  -WSD {:>5}  => KEPT {:>4}",
                g.offered, g.pos, g.relevance, g.wsd, g.kept
            );
        };
        row("IsA", "DIRECTED", &isa);
        row("PartOf", "DIRECTED", &part);
        row("SynonymOf", "undirected", &syn);

        println!(
            "\n  POS-gating PROOF: {} IsA + {} PartOf candidates dropped because the source concept was not a noun.",
            isa.pos, part.pos
        );
        println!(
            "  DIRECTION PROOF: {} kept IsA edges; {isa_reversed} have their reverse also asserted (asymmetric => want ~0).",
            isa.kept
        );

        // Sample directed IsA edges (highest correlation first).
        let mut isa_edges: Vec<&TEdge> = edges.iter().filter(|e| e.rel == "IsA").collect();
        isa_edges.sort_by(|a, b| b.w.partial_cmp(&a.w).unwrap());
        println!("\n  sample DIRECTED IsA edges (concept --IsA--> hypernym, by correlation):");
        for e in isa_edges.iter().take(12) {
            println!("    {} --IsA--> {}  ({:.2})", e.src, e.tgt, e.w);
        }

        // Write the findings: the typed/directed edges with graph-correlation weights.
        let edges_json: Vec<serde_json::Value> = edges
            .iter()
            .map(|e| {
                serde_json::json!({
                    "src": e.src, "rel": e.rel, "directed": e.directed, "tgt": e.tgt,
                    "w": (e.w * 1000.0).round() / 1000.0
                })
            })
            .collect();
        let out = PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join(".cache/aden/dict/typed-directed-edges.json");
        let _ = std::fs::write(
            &out,
            serde_json::to_string(&serde_json::json!({
                "note": "POS-typed, WSD-gated semantic edges over corpus concepts; IsA/PartOf directed, SynonymOf undirected",
                "tau": TAU,
                "kept": {"IsA": isa.kept, "PartOf": part.kept, "SynonymOf": syn.kept},
                "edges": edges_json,
            }))
            .unwrap_or_default(),
        );
        println!(
            "\n  findings -> {}  ({} typed edges: {} IsA, {} PartOf, {} SynonymOf)",
            out.display(),
            edges.len(),
            isa.kept,
            part.kept,
            syn.kept
        );

        assert!(!concepts.is_empty(), "no concepts");
    }
}
