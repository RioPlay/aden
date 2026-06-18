// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Dictionary contextualization — DRY RUN (writes NOTHING to .aden/store).
//
// Goal (the north-star the session is driving toward): take a dictionary and turn it
// into a *contextualized* graph — typed lexical-semantic edges between term senses —
// the way `aden overlay` eventually will. This harness proves the *extraction* works
// on real dictionary data BEFORE any store write exists, so the edges can be reviewed
// first. It is the honest, ungated precursor to the (sign-off-gated) importer.
//
// WHAT ALREADY WORKS IN REAL ADEN (the 2026-06-16 probe): a glossary-form doc indexes
// to one `Term` node per entry + a `DefinesTerm` containment edge. What is MISSING is
// the inter-term *relations* — extraction never emits the semantic `EdgeType` variants.
// This dry run fills exactly that gap: dictionary entry -> candidate `EdgeType` triples.
//
// THE DICTIONARY IS THE BEST CASE FOR THIS:
//   * It is sense-structured — each entry separates its senses, each with its own
//     gloss and part of speech — so BUILD-time word-sense disambiguation is largely
//     sidestepped: we import every sense cleanly (the hard query-time WSD is separate).
//   * Parts of speech come straight from the entry (`speech_part`) and *gate* which
//     relations are even eligible (only nouns get genus IsA / PartOf), which is the
//     POS layer doing real work — answering the "types of speech" question directly.
//
// Emits REAL `aden_core::EdgeType` values (not strings) to prove the mapping is concrete.
//
// Source: embedded wordset-schema sample (hermetic), or a real file via ADEN_DICT_JSON
// (wordset format: a JSON object/array of {word, meanings:[{speech_part, def, synonyms}]}).
// Run: cargo test -p aden-index --test dict_contextualize_dryrun -- --include-ignored --nocapture

use aden_core::EdgeType;
use std::collections::BTreeMap;

// ---- wordset-schema input -------------------------------------------------------

#[derive(serde::Deserialize, Default)]
struct Meaning {
    #[serde(default)]
    speech_part: Option<String>,
    #[serde(default)]
    def: String,
    #[serde(default)]
    synonyms: Vec<String>,
}

#[derive(serde::Deserialize, Default)]
struct Entry {
    #[serde(default)]
    word: String,
    #[serde(default)]
    meanings: Vec<Meaning>,
}

/// A small, realistic wordset-format sample. Object form (`word -> entry`) so the same
/// loader path handles real wordset `a.json`..`z.json` files unchanged. Chosen to
/// exercise every extraction rule, incl. a polysemous entry (`stream`) for the
/// sense-scoping demo and POS-gating (adjectives must NOT get a genus IsA).
const SAMPLE: &str = r#"{
  "xanthous":   {"word":"xanthous",  "meanings":[{"speech_part":"adjective","def":"of a yellowish color; see also xanthic","synonyms":["yellowish","yellow"]}]},
  "sedan":      {"word":"sedan",     "meanings":[{"speech_part":"noun","def":"a kind of automobile with a closed body and fixed roof","synonyms":["saloon"]}]},
  "automobile": {"word":"automobile","meanings":[{"speech_part":"noun","def":"a self-propelled passenger vehicle; synonymous with car","synonyms":["car","motorcar"]}]},
  "wheel":      {"word":"wheel",     "meanings":[{"speech_part":"noun","def":"a circular component that is part of a vehicle","synonyms":[]}]},
  "cold":       {"word":"cold",      "meanings":[{"speech_part":"adjective","def":"having a low temperature; the opposite of hot","synonyms":["chilly","frigid"]}]},
  "happy":      {"word":"happy",     "meanings":[{"speech_part":"adjective","def":"feeling or showing joy; the opposite of sad","synonyms":["glad","joyful"]}]},
  "debugger":   {"word":"debugger",  "meanings":[{"speech_part":"noun","def":"a software tool used to debug a program","synonyms":[]}]},
  "stream":     {"word":"stream",    "meanings":[
                    {"speech_part":"noun","def":"a small narrow river","synonyms":["brook","creek"]},
                    {"speech_part":"noun","def":"a continuous flow of data in computing","synonyms":["feed"]},
                    {"speech_part":"verb","def":"to flow continuously in a current","synonyms":["flow","pour"]}]},
  "teach":      {"word":"teach",     "meanings":[{"speech_part":"verb","def":"to impart knowledge to someone; antonym of learn","synonyms":["instruct","educate"]}]}
}"#;

/// Load wordset JSON (array of entries OR object `word -> entry`) into entries.
fn load(src: &str) -> Vec<Entry> {
    let v: serde_json::Value = serde_json::from_str(src).expect("dictionary JSON parse");
    match v {
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .filter_map(|e| serde_json::from_value(e).ok())
            .collect(),
        serde_json::Value::Object(map) => map
            .into_iter()
            .filter_map(|(k, e)| {
                let mut ent: Entry = serde_json::from_value(e).ok()?;
                if ent.word.is_empty() {
                    ent.word = k;
                }
                Some(ent)
            })
            .collect(),
        _ => Vec::new(),
    }
}

// ---- POS layer ------------------------------------------------------------------

/// Canonical part of speech (the "types of speech" layer). Maps the dictionary's
/// free-form `speech_part` onto the open content classes that carry relations.
fn pos(speech_part: &Option<String>) -> &'static str {
    match speech_part
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        s if s.starts_with("noun") => "noun",
        s if s.starts_with("verb") => "verb",
        s if s.starts_with("adj") => "adjective",
        s if s.starts_with("adv") => "adverb",
        "" => "unknown",
        _ => "other",
    }
}

// ---- relation extraction (Tier 1: from the dictionary's own text) ---------------

#[derive(Clone, Copy, PartialEq)]
enum Conf {
    High, // explicit `synonyms` list
    Med,  // explicit phrase ("opposite of X", "synonymous with X")
    Low,  // heuristic parse (genus head, "part of X", "see also X")
}

struct Triple {
    subject: String,
    edge: EdgeType,
    object: String,
    pos: &'static str,
    conf: Conf,
    rule: &'static str,
}

/// Normalize a captured term: drop a leading article, keep the first alphabetic word.
fn canon(s: &str) -> String {
    let mut words = s.split_whitespace();
    let mut first = words.next().unwrap_or("");
    if matches!(first, "a" | "an" | "the") {
        first = words.next().unwrap_or("");
    }
    first
        .chars()
        .take_while(|c| c.is_ascii_alphabetic() || *c == '-')
        .collect::<String>()
        .to_ascii_lowercase()
}

/// First content word after any of `triggers` in `def_lc`, skipping articles.
fn capture_after(def_lc: &str, triggers: &[&str]) -> Option<String> {
    for t in triggers {
        if let Some(pos) = def_lc.find(t) {
            let rest = def_lc[pos + t.len()..].trim_start();
            let rest = rest
                .strip_prefix("a ")
                .or_else(|| rest.strip_prefix("an "))
                .or_else(|| rest.strip_prefix("the "))
                .unwrap_or(rest);
            let w = canon(rest);
            if w.len() > 1 {
                return Some(w);
            }
        }
    }
    None
}

/// Genus (hypernym) head for a NOUN gloss: "a kind of X" -> X, else the head noun of
/// the leading noun phrase ("a small narrow river" -> river). NOUN-ONLY by design —
/// this is the POS gate that keeps adjectives/verbs from acquiring a false IsA.
fn genus(def_lc: &str) -> Option<String> {
    let d = def_lc.trim();
    let d = d
        .strip_prefix("a ")
        .or_else(|| d.strip_prefix("an "))
        .or_else(|| d.strip_prefix("the "))
        .unwrap_or(d);
    for kind in [
        "kind of ",
        "type of ",
        "form of ",
        "sort of ",
        "variety of ",
        "species of ",
        "class of ",
        "genus of ",
        "any of ",
    ] {
        if let Some(rest) = d.strip_prefix(kind) {
            let w = canon(rest);
            if w.len() > 1 {
                return Some(w);
            }
        }
    }
    // Head of the leading noun phrase: words up to the first clause/preposition marker.
    let stops = [" that ", " which ", " used ", " of ", ",", ";", "."];
    let mut end = d.len();
    for s in stops {
        if let Some(i) = d.find(s) {
            end = end.min(i);
        }
    }
    let phrase = d[..end].trim();
    let head = phrase.split_whitespace().last().unwrap_or("");
    let head = canon(head);
    (head.len() > 2).then_some(head)
}

/// Build a triple unless it is empty or a self-loop. (Free helper — avoids a closure
/// holding a mutable borrow of the output vec across the extraction loop.)
fn mk(
    subj: &str,
    edge: EdgeType,
    object: String,
    p: &'static str,
    conf: Conf,
    rule: &'static str,
) -> Option<Triple> {
    (!object.is_empty() && object != subj).then(|| Triple {
        subject: subj.to_string(),
        edge,
        object,
        pos: p,
        conf,
        rule,
    })
}

/// Extract candidate typed edges from one dictionary entry. POS-gated per WordNet's
/// architecture: only nouns get genus IsA / PartOf; antonymy/synonymy apply broadly.
fn extract(entry: &Entry) -> Vec<Triple> {
    let subj = entry.word.to_ascii_lowercase();
    let mut out: Vec<Triple> = Vec::new();

    for m in &entry.meanings {
        let p = pos(&m.speech_part);
        let def_lc = m.def.to_ascii_lowercase();

        // SynonymOf — explicit synonyms list (highest confidence; any POS).
        for s in &m.synonyms {
            out.extend(mk(
                &subj,
                EdgeType::SynonymOf,
                canon(s),
                p,
                Conf::High,
                "synonyms[]",
            ));
        }
        // SynonymOf — from the gloss ("synonymous with X", "also called X").
        if let Some(o) = capture_after(
            &def_lc,
            &["synonymous with ", "also called ", "another word for "],
        ) {
            out.extend(mk(
                &subj,
                EdgeType::SynonymOf,
                o,
                p,
                Conf::Med,
                "\"synonymous with X\"",
            ));
        }
        // AntonymOf — explicit opposition (any POS; lexical).
        if let Some(o) = capture_after(&def_lc, &["opposite of ", "antonym of ", "the reverse of "])
        {
            out.extend(mk(
                &subj,
                EdgeType::AntonymOf,
                o,
                p,
                Conf::Med,
                "\"opposite of X\"",
            ));
        }
        // RelatesTo — navigational cross-references and "used to/for X".
        if let Some(o) = capture_after(&def_lc, &["see also ", "compare ", "related to "]) {
            out.extend(mk(
                &subj,
                EdgeType::RelatesTo,
                o,
                p,
                Conf::Low,
                "\"see also X\"",
            ));
        }
        if let Some(o) = capture_after(&def_lc, &["used to ", "used for "]) {
            out.extend(mk(
                &subj,
                EdgeType::RelatesTo,
                o,
                p,
                Conf::Low,
                "\"used to X\"",
            ));
        }
        // NOUN-GATED structural relations (the POS layer doing real work).
        if p == "noun" {
            if let Some(o) = capture_after(&def_lc, &["part of ", "a member of ", "component of "])
            {
                out.extend(mk(
                    &subj,
                    EdgeType::PartOf,
                    o,
                    p,
                    Conf::Low,
                    "\"part of X\"",
                ));
            }
            if let Some(o) = genus(&def_lc) {
                out.extend(mk(&subj, EdgeType::IsA, o, p, Conf::Low, "genus head"));
            }
        }
    }

    // Dedup identical (edge, object) pairs.
    out.sort_by(|a, b| {
        ename(&a.edge)
            .cmp(ename(&b.edge))
            .then_with(|| a.object.cmp(&b.object))
    });
    out.dedup_by(|a, b| ename(&a.edge) == ename(&b.edge) && a.object == b.object);
    out
}

fn ename(e: &EdgeType) -> &'static str {
    match e {
        EdgeType::SynonymOf => "SynonymOf",
        EdgeType::AntonymOf => "AntonymOf",
        EdgeType::IsA => "IsA",
        EdgeType::PartOf => "PartOf",
        EdgeType::RelatesTo => "RelatesTo",
        _ => "Other",
    }
}

fn conf_label(c: Conf) -> &'static str {
    match c {
        Conf::High => "high (explicit list)",
        Conf::Med => "med  (explicit phrase)",
        Conf::Low => "low  (heuristic parse)",
    }
}

#[test]
#[ignore = "dry-run contextualizer; writes nothing; reads embedded sample or ADEN_DICT_JSON"]
fn dict_contextualize_dryrun() {
    let (src, origin) = match std::env::var("ADEN_DICT_JSON") {
        Ok(path) => match std::fs::read_to_string(&path) {
            Ok(text) => (text, format!("ADEN_DICT_JSON = {path}")),
            Err(e) => {
                eprintln!("SKIP: cannot read ADEN_DICT_JSON ({path}): {e}");
                return;
            }
        },
        Err(_) => (
            SAMPLE.to_string(),
            "embedded wordset-schema sample".to_string(),
        ),
    };

    let entries = load(&src);
    let n_words = entries.len();
    let n_senses: usize = entries.iter().map(|e| e.meanings.len()).sum();

    // POS distribution (the "types of speech" layer).
    let mut pos_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for e in &entries {
        for m in &e.meanings {
            *pos_counts.entry(pos(&m.speech_part)).or_default() += 1;
        }
    }

    let triples: Vec<Triple> = entries.iter().flat_map(extract).collect();

    println!("\n=== Dictionary contextualization — DRY RUN (no store writes) ===");
    println!("Source: {origin}");
    println!("Ingested: {n_words} words, {n_senses} senses");
    println!(
        "(real aden already makes these {n_words} Term nodes + DefinesTerm; this adds the MISSING inter-term edges)"
    );

    println!("\n-- Parts of speech (POS layer; gates which relations are eligible) --");
    for (p, c) in &pos_counts {
        println!("    {p:<10} {c}");
    }

    println!("\n-- Derived typed edges (what `aden overlay` would assert) --");
    let mut by_edge: BTreeMap<&str, Vec<&Triple>> = BTreeMap::new();
    for t in &triples {
        by_edge.entry(ename(&t.edge)).or_default().push(t);
    }
    for (edge, ts) in &by_edge {
        println!("  {edge}  ({} edges):", ts.len());
        for t in ts {
            println!(
                "      {:<11} --{edge}--> {:<11} [{}]  {}  rule: {}",
                t.subject,
                t.object,
                t.pos,
                conf_label(t.conf),
                t.rule,
            );
        }
    }

    // Sense-scoping demo: a polysemous word whose senses get DIFFERENT edges — the
    // dictionary's structure pre-separates them, so we import each sense cleanly.
    let mut by_subject: BTreeMap<&str, Vec<&Triple>> = BTreeMap::new();
    for t in &triples {
        by_subject.entry(t.subject.as_str()).or_default().push(t);
    }
    if let Some(poly) = by_subject
        .iter()
        .find(|(_, ts)| ts.iter().filter(|t| ename(&t.edge) == "IsA").count() > 1)
    {
        println!(
            "\n-- Sense-scoping (the dictionary pre-disambiguates) --\n  '{}' resolves to multiple senses, each with its own hypernym:",
            poly.0
        );
        for t in poly.1.iter().filter(|t| ename(&t.edge) == "IsA") {
            println!("      sense ({}) {} IsA {}", t.pos, t.subject, t.object);
        }
    }

    let assertable = triples.iter().filter(|t| t.conf != Conf::Low).count();
    let review = triples.len() - assertable;
    println!(
        "\nSummary: {} candidate edges across {} edge types from {n_words} words.",
        triples.len(),
        by_edge.len()
    );
    println!(
        "  would assert (high/med): {assertable}   |   heuristic, hold for review (low): {review}"
    );
    println!("DRY RUN: nothing written to .aden/store.\n");

    assert!(n_words > 0, "no dictionary entries parsed");
    assert!(
        !triples.is_empty(),
        "extraction produced zero edges — the contextualizer is not deriving relations"
    );
}
