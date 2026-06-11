// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wave 2 graph-type activation evals (graph-type-roadmap.adoc):
//!
//! 1. `Mentions` edges — unmarked prose that names a symbol in backticks gets
//!    a doc —Mentions→ code edge, deliberately weaker than `Documents` so the
//!    intentional-contract signal stays undiluted.
//! 2. `Demonstrates` edges — a doc code listing that references a symbol gets
//!    listing —Demonstrates→ code, turning `code_block_*` anchors from orphan
//!    noise into "show me an example of X" answers.
//!
//! Polyglot by construction: the same prose/listing fixtures are asserted in
//! BOTH Markdown and AsciiDoc — extraction is per-format (in the parsers),
//! resolution is format-neutral (in `link_store_edges`).
//!
//! Precision guards asserted: a name that is ambiguous across two symbols, or
//! shorter than 4 chars, must NOT link.

use std::path::{Path, PathBuf};
use std::process::Command;

const CHAIN_RS: &str = r#"
pub fn helper_fn(word: &str) -> String {
    format!("{word}!")
}

pub fn caller_fn() -> String {
    helper_fn("hello")
}

pub fn dupe_name() -> u8 { 1 }

pub fn shr() -> u8 { 2 }
"#;

/// Second file defining ANOTHER `dupe_name` so the name is ambiguous.
const OTHER_RS: &str = r#"
pub fn dupe_name() -> u8 { 3 }
"#;

const README_MD: &str = r#"# Guide

The `helper_fn` routine renders a greeting. Ambiguous `dupe_name` and the
short `shr` name must stay unlinked.

## Example

```rust
let s = helper_fn("hi");
```

## Glossary

- **caller_fn**: the entry point of the greeting flow.
"#;

const GUIDE_ADOC: &str = r#"= Adoc Guide

The `caller_fn` entry point drives the greeting flow.

== Example

----
caller_fn();
----

random_thing:: prose dlist outside a glossary section must NOT become a Term.

== Glossary

helper_fn:: The greeting helper, see `helper_fn` in chain.rs.
[[widget-factory]]Widget Factory (WF)::
Makes widgets from greetings.
"#;

fn unique_dir(label: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "aden-wave2-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn git(project: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .expect("git must be available");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn scaffold() -> (PathBuf, PathBuf) {
    let project = unique_dir("proj");
    let data = unique_dir("data");
    std::fs::write(project.join("chain.rs"), CHAIN_RS).unwrap();
    std::fs::write(project.join("other.rs"), OTHER_RS).unwrap();
    std::fs::write(project.join("README.md"), README_MD).unwrap();
    std::fs::write(project.join("GUIDE.adoc"), GUIDE_ADOC).unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["config", "user.email", "wave2@test.invalid"]);
    git(&project, &["config", "user.name", "Wave2 Test"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "fixture"]);
    (project, data)
}

/// Run `aden viz --mode graph --full -j` (the census surface) and return all
/// typed edges as (from_anchor, to_anchor, type).
///
/// Runs WITH `current_dir(project)` and no positional: viz's first positional
/// is ANCHOR, not DIR, so passing the project path as an argument would put
/// it in ANCHOR and silently census the test-runner's cwd instead.
fn census_edges(project: &Path, data: &Path) -> Vec<(String, String, String)> {
    let out = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["viz", "--mode", "graph", "--full", "--format", "json"])
        .current_dir(project)
        .env("ADEN_DATA_DIR", data)
        .output()
        .expect("aden binary must run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "viz --mode graph failed.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("viz --mode graph must emit JSON");
    let by_id: std::collections::HashMap<String, String> = json["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .map(|n| {
            (
                n["id"].as_str().unwrap_or_default().to_string(),
                n["anchor"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    json["edges"]
        .as_array()
        .expect("edges array")
        .iter()
        .map(|e| {
            (
                by_id[e["from"].as_str().unwrap_or_default()].clone(),
                by_id[e["to"].as_str().unwrap_or_default()].clone(),
                e["type"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn edges_of_type<'a>(
    edges: &'a [(String, String, String)],
    ty: &str,
) -> Vec<&'a (String, String, String)> {
    edges.iter().filter(|(_, _, t)| t == ty).collect()
}

/// Eval (Mentions, Markdown): backticked `helper_fn` in README prose must
/// produce doc —Mentions→ helper_fn.
#[test]
fn markdown_prose_mention_links_to_symbol() {
    let (project, data) = scaffold();
    let edges = census_edges(&project, &data);
    let mentions = edges_of_type(&edges, "Mentions");
    assert!(
        mentions.iter().any(|(f, t, _)| f.contains("README.md")
            && !f.contains("code_block")
            && t.ends_with("#helper_fn")),
        "README prose must Mention helper_fn; Mentions edges: {mentions:?}"
    );
}

/// Eval (Mentions, AsciiDoc): backticked `caller_fn` in GUIDE.adoc prose must
/// produce doc —Mentions→ caller_fn (same channel, different format).
#[test]
fn asciidoc_prose_mention_links_to_symbol() {
    let (project, data) = scaffold();
    let edges = census_edges(&project, &data);
    let mentions = edges_of_type(&edges, "Mentions");
    assert!(
        mentions.iter().any(|(f, t, _)| f.contains("GUIDE.adoc")
            && !f.contains("code_block")
            && t.ends_with("#caller_fn")),
        "GUIDE.adoc prose must Mention caller_fn; Mentions edges: {mentions:?}"
    );
}

/// Eval (Mentions, precision guards): an ambiguous name (defined in two files)
/// and a short (<4 chars) name must NOT produce Mentions edges.
#[test]
fn ambiguous_and_short_names_stay_unlinked() {
    let (project, data) = scaffold();
    let edges = census_edges(&project, &data);
    let mentions = edges_of_type(&edges, "Mentions");
    assert!(
        !mentions.iter().any(|(_, t, _)| t.ends_with("#dupe_name")),
        "dupe_name is defined in two files (ambiguous) and must stay unlinked; got: {mentions:?}"
    );
    assert!(
        !mentions.iter().any(|(_, t, _)| t.ends_with("#shr")),
        "shr is under the 4-char floor and must stay unlinked; got: {mentions:?}"
    );
}

/// Eval (Demonstrates, Markdown): the fenced rust listing calling helper_fn
/// must produce code_block —Demonstrates→ helper_fn.
#[test]
fn markdown_listing_demonstrates_symbol() {
    let (project, data) = scaffold();
    let edges = census_edges(&project, &data);
    let demos = edges_of_type(&edges, "Demonstrates");
    assert!(
        demos.iter().any(|(f, t, _)| f.contains("README.md")
            && f.contains("code_block")
            && t.ends_with("#helper_fn")),
        "README listing must Demonstrate helper_fn; Demonstrates edges: {demos:?}"
    );
}

/// All node anchors present in the census.
fn census_anchors(edges_unused: &Path, data: &Path) -> Vec<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["viz", "--mode", "graph", "--full", "--format", "json"])
        .current_dir(edges_unused)
        .env("ADEN_DATA_DIR", data)
        .output()
        .expect("aden binary must run");
    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("census JSON");
    json["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .map(|n| n["anchor"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Eval (Term nodes, AsciiDoc): a glossary dlist entry becomes a Term node
/// (`aden://term/…`) with a doc —DefinesTerm→ term edge; a term whose name is
/// an unambiguous symbol also gets term —Mentions→ code (the definition links
/// back to what it defines). The explicit `[[anchor]]` form keeps its slug.
#[test]
fn asciidoc_glossary_entries_become_term_nodes() {
    let (project, data) = scaffold();
    let edges = census_edges(&project, &data);
    let defines = edges_of_type(&edges, "DefinesTerm");
    assert!(
        defines.iter().any(|(f, t, _)| f.contains("GUIDE.adoc")
            && t.starts_with("aden://term/")
            && t.ends_with("/helper-fn")),
        "glossary section must DefinesTerm the helper_fn term; got: {defines:?}"
    );
    assert!(
        defines
            .iter()
            .any(|(_, t, _)| t.starts_with("aden://term/") && t.ends_with("/widget-factory")),
        "explicit [[widget-factory]] anchor must keep its slug; got: {defines:?}"
    );
    let mentions = edges_of_type(&edges, "Mentions");
    assert!(
        mentions
            .iter()
            .any(|(f, t, _)| f.starts_with("aden://term/")
                && f.ends_with("/helper-fn")
                && t.ends_with("#helper_fn")),
        "term helper_fn must Mention the code symbol it names; got: {mentions:?}"
    );
}

/// Eval (Term nodes, Markdown): `- **term**: def` bullets under a glossary
/// heading become Term nodes with DefinesTerm edges.
#[test]
fn markdown_glossary_entries_become_term_nodes() {
    let (project, data) = scaffold();
    let edges = census_edges(&project, &data);
    let defines = edges_of_type(&edges, "DefinesTerm");
    assert!(
        defines.iter().any(|(f, t, _)| f.contains("README.md")
            && t.starts_with("aden://term/")
            && t.ends_with("/caller-fn")),
        "markdown glossary must DefinesTerm the caller_fn term; got: {defines:?}"
    );
}

/// Eval (Term nodes, negative): a description-list line OUTSIDE a glossary
/// section/document must not become a Term node.
#[test]
fn non_glossary_dlist_is_not_a_term() {
    let (project, data) = scaffold();
    let anchors = census_anchors(&project, &data);
    assert!(
        !anchors
            .iter()
            .any(|a| a.starts_with("aden://term/") && a.contains("random-thing")),
        "dlist entries outside glossary sections must stay plain prose; got: {anchors:?}"
    );
}

/// Eval (Demonstrates, AsciiDoc): the `----` listing calling caller_fn must
/// produce code_block —Demonstrates→ caller_fn.
#[test]
fn asciidoc_listing_demonstrates_symbol() {
    let (project, data) = scaffold();
    let edges = census_edges(&project, &data);
    let demos = edges_of_type(&edges, "Demonstrates");
    assert!(
        demos.iter().any(|(f, t, _)| f.contains("GUIDE.adoc")
            && f.contains("code_block")
            && t.ends_with("#caller_fn")),
        "GUIDE.adoc listing must Demonstrate caller_fn; Demonstrates edges: {demos:?}"
    );
}
