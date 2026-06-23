// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Build the shared lexical-semantic overlay store from OEWN (a WRITE — but to a
// DEDICATED store, never the project's `.aden/store`).
//
// This is the first step that makes the dictionary graph permanent and queryable
// instead of a dry-run JSON. It reads the full OEWN flat triples
// (`scripts/oewn_to_triples.py --all` → 172k edges) and writes them as typed edges
// (`SynonymOf`/`IsA`/`PartOf`) into a fjall store at a dedicated lexicon path. Lemmas
// are anchored under the `aden://term/oewn/<lemma>` scheme so they never collide with a
// project's code anchors.
//
// SAFETY / why a separate store: the project store is "fresh by construction" — every
// edge derives from source, and gen/heal --gc prune anything that doesn't. Injecting
// 172k sourceless English edges there would either be GC'd as orphans or pollute every
// `ask`/`grep`/`check` on the repo. So the lexicon lives in its OWN store. This builder
// is idempotent: it wipes the lexicon dir and rebuilds, so re-runs never accrete.
// `.aden/store` is never opened or written here.
//
// v1 writes EDGES only (the relational graph). Term `Document` nodes carrying the gloss
// text are the next increment. Retrieval integration (read tools consulting the overlay)
// is deliberately separate — that's the part that needs measurement, not this write.
//
// Inputs: ADEN_OEWN_TRIPLES (default ~/.cache/aden/dict/oewn-triples-flat.json).
// Output: ADEN_LEXICON_STORE (default ~/.cache/aden/lexicon).
// Run: cargo test -p aden-cli --test build_lexicon_store -- --include-ignored --nocapture

use aden_core::EdgeType;
use aden_store::{GraphStorage, Storage};
use std::path::PathBuf;

fn home_join(rest: &str) -> PathBuf {
    PathBuf::from(
        dirs::home_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    )
    .join(rest)
}

fn lexicon_path() -> PathBuf {
    std::env::var("ADEN_LEXICON_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_join(".cache/aden/lexicon"))
}

fn triples_path() -> PathBuf {
    std::env::var("ADEN_OEWN_TRIPLES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_join(".cache/aden/dict/oewn-triples-flat.json"))
}

/// Map the converter's edge label onto aden's `EdgeType`. Unknown labels are skipped.
fn parse_edge(s: &str) -> Option<EdgeType> {
    match s {
        "SynonymOf" => Some(EdgeType::SynonymOf),
        "IsA" => Some(EdgeType::IsA),
        "PartOf" => Some(EdgeType::PartOf),
        "AntonymOf" => Some(EdgeType::AntonymOf),
        "RelatesTo" => Some(EdgeType::RelatesTo),
        _ => None,
    }
}

/// Lexicon anchor scheme — namespaced so it can never collide with a project's code
/// anchors (`aden://module/...`).
fn anchor(lemma: &str) -> String {
    format!("aden://term/oewn/{lemma}")
}

#[test]
#[ignore = "WRITES the dedicated lexicon store (~/.cache/aden/lexicon); never touches .aden/store"]
fn build_lexicon_store() {
    let tpath = triples_path();
    let Ok(raw) = std::fs::read_to_string(&tpath) else {
        eprintln!(
            "SKIP: OEWN triples not found at {} — run `python3 scripts/oewn_to_triples.py --all` first",
            tpath.display()
        );
        return;
    };
    let triples: Vec<Vec<String>> =
        serde_json::from_str(&raw).expect("flat OEWN triples JSON parse");

    // [subject, edge, object, pos] → (anchor(subject), anchor(object), EdgeType).
    let edges: Vec<(String, String, EdgeType)> = triples
        .iter()
        .filter_map(|t| {
            if t.len() < 3 {
                return None;
            }
            let et = parse_edge(&t[1])?;
            Some((anchor(&t[0]), anchor(&t[2]), et))
        })
        .collect();

    // Idempotent rebuild: wipe the dedicated lexicon dir, then create fresh. (Only the
    // resolved lexicon path is touched; the project store is never opened.)
    let lex = lexicon_path();
    let _ = std::fs::remove_dir_all(&lex);
    std::fs::create_dir_all(&lex).expect("create lexicon dir");
    let lex_str = lex.to_str().expect("lexicon path is valid UTF-8");
    let store = Storage::new(lex_str).expect("create lexicon store");

    println!("\n=== Build OEWN lexicon overlay store (dedicated; .aden/store untouched) ===");
    println!("Source triples : {} ({})", triples.len(), tpath.display());
    println!("Mapped edges   : {}", edges.len());
    println!("Lexicon store  : {}", lex.display());

    store
        .put_edges_bulk(&edges)
        .expect("bulk-write lexicon edges");

    // Verify by reading back per type (counts reflect post-dedup persisted edges).
    let count = |et: EdgeType| store.get_edges_by_type(&et).map(|v| v.len()).unwrap_or(0);
    let (syn, isa, part) = (
        count(EdgeType::SynonymOf),
        count(EdgeType::IsA),
        count(EdgeType::PartOf),
    );
    let persisted = syn + isa + part;

    println!("\n  Persisted (read back from the store):");
    println!("    SynonymOf {syn}");
    println!("    IsA       {isa}");
    println!("    PartOf    {part}");
    println!(
        "    total     {persisted}  (written {}, delta = deduped triples)",
        edges.len()
    );

    // Spot-check a known lemma round-trips with its outgoing neighbours.
    let sample_anchor = anchor("merge");
    let sample = store.get_outgoing_edges(&sample_anchor).unwrap_or_default();
    println!(
        "\n  Spot-check  {sample_anchor} → {} outgoing edge(s):",
        sample.len()
    );
    for (tgt, et) in sample.iter().take(8) {
        println!("      --{et:?}--> {tgt}");
    }

    println!("\n  The lexicon is now a live, queryable graph. .aden/store was not opened.");

    assert!(!edges.is_empty(), "no edges mapped from triples");
    assert!(persisted > 0, "nothing persisted — the write did not land");
    assert!(
        persisted <= edges.len(),
        "persisted ({persisted}) exceeds written ({}) — dedup invariant violated",
        edges.len()
    );
}
