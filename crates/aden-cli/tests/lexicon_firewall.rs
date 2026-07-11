// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Legal firewall test: every "imported" edge in the lexicon provenance sidecar
// must come from a source whose licence has been cleared for distribution.
//
// Background: the lexicon overlay imports edges from third-party dictionaries and
// thesauri. Each source carries its own licence terms. This test enforces that
// only sources from the SHIPPABLE_SOURCES allowlist -- each of which has been
// reviewed for AGPL compatibility and freely-redistributable terms -- appear as
// the origin of imported edges in the provenance sidecar.
//
// "derived" edges (kind == "derived") are exempt: they are produced algorithmically
// from cleared inputs and carry no third-party licence obligations of their own.
//
// Inputs:
//   ADEN_LEXICON_PROVENANCE (default: sibling of ADEN_LEXICON_STORE or
//                            ~/.cache/aden/lexicon-provenance.json).
//
// Run: cargo test -p aden-cli --test lexicon_firewall -- --include-ignored --nocapture

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Sources whose licences have been reviewed and cleared for shipping with aden.
/// Expand this list only after a licence review -- never add a source here
/// without confirming its redistribution terms.
const SHIPPABLE_SOURCES: &[&str] = &[
    "oewn",     // Open English WordNet -- CC BY 4.0
    "moby",     // Moby Project -- public domain
    "roget",    // Project Gutenberg Roget's Thesaurus -- public domain
    "webster",  // Webster's 1913 -- public domain
    "wikidata", // Wikidata -- CC0 1.0
    "wordnet",  // Princeton WordNet -- WordNet licence (permissive, redistribution allowed)
];

fn home_join(rest: &str) -> PathBuf {
    PathBuf::from(
        dirs::home_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    )
    .join(rest)
}

fn provenance_path() -> PathBuf {
    if let Ok(p) = std::env::var("ADEN_LEXICON_PROVENANCE") {
        return PathBuf::from(p);
    }
    // Fall back to sibling of the lexicon store directory.
    let store = std::env::var("ADEN_LEXICON_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_join(".cache/aden/lexicon"));
    store
        .parent()
        .map(|p| p.join("lexicon-provenance.json"))
        .unwrap_or_else(|| home_join(".cache/aden/lexicon-provenance.json"))
}

#[test]
#[ignore = "reads the provenance sidecar and enforces the shippable-sources allowlist"]
fn lexicon_firewall() {
    let prov_path = provenance_path();

    let raw = match std::fs::read_to_string(&prov_path) {
        Ok(s) => s,
        Err(_) => {
            eprintln!(
                "SKIP: no provenance sidecar at {} -- run build_lexicon_store first",
                prov_path.display()
            );
            return;
        }
    };

    let sidecar: serde_json::Value =
        serde_json::from_str(&raw).expect("provenance sidecar must be valid JSON");

    let edges = sidecar["edges"]
        .as_object()
        .expect("provenance sidecar must have an 'edges' object");

    let allowlist: HashSet<&str> = SHIPPABLE_SOURCES.iter().copied().collect();

    let mut total_edges: usize = 0;
    let mut imported_count: usize = 0;
    let mut derived_count: usize = 0;
    // Track every source seen across imported edges.
    let mut sources_seen: HashSet<String> = HashSet::new();
    // Collect violations: source -> list of offending edge keys.
    let mut violations: HashMap<String, Vec<String>> = HashMap::new();

    for (edge_key, edge_val) in edges {
        total_edges += 1;
        let kind = edge_val["kind"].as_str().unwrap_or("imported");

        if kind == "derived" {
            derived_count += 1;
            // Derived edges are exempt from the allowlist check.
            continue;
        }

        // Treat anything that is not "derived" as "imported".
        imported_count += 1;

        let sources: Vec<String> = edge_val["sources"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        for source in &sources {
            sources_seen.insert(source.clone());
            if !allowlist.contains(source.as_str()) {
                violations
                    .entry(source.clone())
                    .or_default()
                    .push(edge_key.clone());
            }
        }
    }

    // Print summary regardless of pass/fail so --nocapture gives visibility.
    println!("\n=== Lexicon firewall ===");
    println!("Sidecar        : {}", prov_path.display());
    println!("Total edges    : {total_edges}");
    println!("  imported     : {imported_count}");
    println!("  derived      : {derived_count}");
    let mut seen_sorted: Vec<&String> = sources_seen.iter().collect();
    seen_sorted.sort();
    println!("Sources seen   : {seen_sorted:?}");
    println!("Violations     : {}", violations.len());

    if !violations.is_empty() {
        for (src, keys) in &violations {
            let sample: Vec<&String> = keys.iter().take(5).collect();
            println!(
                "  VIOLATION source={src:?}  ({} edge(s), e.g. {sample:?})",
                keys.len()
            );
        }
    }

    let violation_sources: Vec<&String> = {
        let mut v: Vec<&String> = violations.keys().collect();
        v.sort();
        v
    };
    assert!(
        violations.is_empty(),
        "lexicon firewall: {} non-cleared source(s) found in imported edges: {violation_sources:?}. \
        Add them to SHIPPABLE_SOURCES only after a licence review.",
        violations.len()
    );

    println!("\n  All imported edges originate from cleared sources. Firewall passed.");
}
