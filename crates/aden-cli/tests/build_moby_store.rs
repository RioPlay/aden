// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Build the Moby thesaurus overlay store (a WRITE -- to a DEDICATED store,
// never the project's `.aden/store`).
//
// Moby is a public-domain English thesaurus. The converter
// (`scripts/moby_to_triples.py`) emits flat triples as
// [subj, label, obj, pos] or [subj, label, obj, pos, source] 5-tuples.
// Moby only emits SynonymOf but the same parse_edge mapping is kept for
// forward-compatibility (e.g. an extended Moby that adds AntonymOf).
//
// Lemmas are anchored under `aden://term/moby/<lemma>` so they cannot
// collide with OEWN (`aden://term/oewn/...`) or code (`aden://module/...`)
// anchors. In the unified overlay these two namespaces are eventually
// cross-linked by a router; each source keeps its own store.
//
// This builder is idempotent: it wipes the Moby store dir and rebuilds, so
// re-runs never accrete stale edges. `.aden/store` is never opened.
//
// Inputs:
//   ADEN_MOBY_TRIPLES (default ~/.cache/aden/dict/moby-triples-flat.json)
//     Array of [subj, label, obj, pos] or [subj, label, obj, pos, source].
// Output:
//   ADEN_MOBY_STORE (default ~/.cache/aden/moby).
//
// Run: cargo test -p aden-cli --test build_moby_store -- --include-ignored --nocapture

use aden_core::EdgeType;
use aden_store::{GraphStorage, Storage};
use std::path::PathBuf;

fn home_join(rest: &str) -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest)
}

fn moby_store_path() -> PathBuf {
    std::env::var("ADEN_MOBY_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_join(".cache/aden/moby"))
}

fn moby_triples_path() -> PathBuf {
    std::env::var("ADEN_MOBY_TRIPLES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_join(".cache/aden/dict/moby-triples-flat.json"))
}

/// Map a converter edge label onto aden's `EdgeType`. Unknown labels are skipped.
/// Moby currently emits only SynonymOf; the full table is kept for safety.
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

/// Moby anchor scheme -- `aden://term/moby/<lemma>`.
fn anchor(lemma: &str) -> String {
    format!("aden://term/moby/{lemma}")
}

#[test]
#[ignore = "WRITES the dedicated Moby store (~/.cache/aden/moby); never touches .aden/store"]
fn build_moby_store() {
    let tpath = moby_triples_path();
    let Ok(raw) = std::fs::read_to_string(&tpath) else {
        eprintln!(
            "SKIP: Moby triples not found at {} -- run `python3 scripts/moby_to_triples.py` first",
            tpath.display()
        );
        return;
    };
    let triples: Vec<Vec<String>> =
        serde_json::from_str(&raw).expect("Moby flat triples JSON parse");

    // Accept [subj, label, obj, pos] (4-tuple) OR [subj, label, obj, pos, source] (5-tuple).
    // Only indices 0, 1, 2 are used for the graph edge; pos/source are ignored here.
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

    // Idempotent rebuild: wipe then recreate the Moby store dir.
    // The project `.aden/store` is never opened.
    let store_path = moby_store_path();
    let _ = std::fs::remove_dir_all(&store_path);
    std::fs::create_dir_all(&store_path).expect("create Moby store dir");
    let store_str = store_path.to_str().expect("Moby store path is valid UTF-8");
    let store = Storage::new(store_str).expect("create Moby store");

    println!("\n=== Build Moby thesaurus overlay store (dedicated; .aden/store untouched) ===");
    println!("Source triples : {} ({})", triples.len(), tpath.display());
    println!("Mapped edges   : {}", edges.len());
    println!("Moby store     : {}", store_path.display());

    store.put_edges_bulk(&edges).expect("bulk-write Moby edges");

    // Read back counts to verify the write landed.
    let count = |et: EdgeType| store.get_edges_by_type(&et).map(|v| v.len()).unwrap_or(0);
    let syn = count(EdgeType::SynonymOf);
    let isa = count(EdgeType::IsA);
    let part = count(EdgeType::PartOf);
    let persisted = syn + isa + part;

    println!("\n  Persisted (read back from the store):");
    println!("    SynonymOf {syn}");
    println!("    IsA       {isa}");
    println!("    PartOf    {part}");
    println!(
        "    total     {persisted}  (written {}, delta = deduped triples)",
        edges.len()
    );

    // Spot-check `aden://term/moby/merge` -- a word present in the Moby thesaurus.
    let sample_anchor = anchor("merge");
    let sample = store.get_outgoing_edges(&sample_anchor).unwrap_or_default();
    println!(
        "\n  Spot-check  {sample_anchor} -> {} outgoing edge(s):",
        sample.len()
    );
    for (tgt, et) in sample.iter().take(8) {
        println!("      --{et:?}--> {tgt}");
    }

    println!("\n  Moby lexicon is now a live, queryable graph. .aden/store was not opened.");

    assert!(!edges.is_empty(), "no edges mapped from Moby triples");
    assert!(persisted > 0, "nothing persisted -- the write did not land");
    assert!(
        persisted <= edges.len(),
        "persisted ({persisted}) exceeds written ({}) -- dedup invariant violated",
        edges.len()
    );
}
