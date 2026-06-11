// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Prose cross-reference graph edges (ADR: `<<anchor>>` is a GLOBAL reference;
//! the aden graph is the router):
//!
//! 1. A post referencing `<<_term>>` declared INLINE in another file
//!    (`[[_term]]Term::` — the real-world description-list form) must gain
//!    bidirectional `RelatesTo` edges to the declaring doc node after gen, so
//!    `query --backlinks` on the glossary term reaches the post.
//! 2. `aden check` must accept inline anchor declarations and must not flag
//!    `<<x>>` examples inside `----` listing blocks.
//!
//! These run the freshly-built binary (`CARGO_BIN_EXE_aden`) against a
//! hermetic fixture, with the store isolated via a per-test `ADEN_DATA_DIR`,
//! so they are offline and deterministic (style of `wave1_edges.rs`).

use std::path::{Path, PathBuf};
use std::process::Command;

const GLOSSARY_ADOC: &str = "\
= Glossary

[[_term]]Term::
The canonical definition of the term.

[[_other]]Other term::
A second definition that references <<_term>> itself.
";

const POST_ADOC: &str = "\
= A Post

== Intro

This section references <<_term>> and a labeled <<_other,the other term>>.

----
a listing showing <<not_a_ref>> and [[not_an_anchor]] literally
----

The fence above must neither declare anchors nor count as references.
";

fn unique_dir(label: &str) -> PathBuf {
    // pid+nanos alone collides on macOS (µs clock granularity) when parallel
    // test threads enter in the same tick; the counter disambiguates.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "aden-prosexref-{label}-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn scaffold() -> (PathBuf, PathBuf) {
    let project = unique_dir("proj");
    let data = unique_dir("data");
    std::fs::write(project.join("glossary.adoc"), GLOSSARY_ADOC).unwrap();
    std::fs::write(project.join("post.adoc"), POST_ADOC).unwrap();
    (project, data)
}

fn aden(project: &Path, data: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(args)
        .arg(project)
        .env("ADEN_DATA_DIR", data)
        .output()
        .expect("aden binary must run");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

/// Backlink anchors for `target` as reported by `query --backlinks`.
fn backlinks(project: &Path, data: &Path, target: &str) -> Vec<String> {
    let (ok, text) = aden(project, data, &["query", "--backlinks", target]);
    assert!(ok, "query --backlinks {target} failed:\n{text}");
    let json: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("query --backlinks must emit JSON ({e}); got: {text}"));
    json.as_array()
        .unwrap_or_else(|| panic!("backlinks output must be an array; got {json}"))
        .iter()
        .map(|v| v["anchor"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Cross-document prose ref → bidirectional RelatesTo edges, and a clean check.
#[test]
fn prose_ref_builds_backlinked_edges_and_check_is_clean() {
    let (project, data) = scaffold();

    let (ok, gen_out) = aden(&project, &data, &["gen"]);
    assert!(ok, "gen failed:\n{gen_out}");

    // Backlinks on the glossary term (declared INLINE) must include the
    // referencing post section. The fixture has no manifest, so the project
    // segment is the directory name (the manifest-less naming fallback —
    // formerly a global "unknown" bucket).
    let proj = project.file_name().unwrap().to_string_lossy().to_string();
    let term = format!("aden://doc/{proj}/glossary.adoc#_term");
    let back = backlinks(&project, &data, &term);
    assert!(
        back.iter().any(|a| a.contains("post.adoc")),
        "backlinks on {term} must reach the referencing post node; got {back:?}"
    );

    // Bidirectional: the post section's backlinks include the glossary term
    // (the term's RelatesTo edge points back at the post).
    let post = back
        .iter()
        .find(|a| a.contains("post.adoc"))
        .unwrap()
        .clone();
    let reverse = backlinks(&project, &data, &post);
    assert!(
        reverse.contains(&term),
        "RelatesTo must be bidirectional; backlinks of {post} were {reverse:?}"
    );

    // The labeled form `<<_other,label>>` links too.
    let other_back = backlinks(
        &project,
        &data,
        &format!("aden://doc/{proj}/glossary.adoc#_other"),
    );
    assert!(
        other_back.iter().any(|a| a.contains("post.adoc")),
        "labeled <<_other,label>> must also produce the edge; got {other_back:?}"
    );

    // The listing-block examples produce neither anchors nor refs.
    let (_, check_out) = aden(&project, &data, &["check"]);
    assert!(
        check_out.contains("All <<refs>> resolve."),
        "check must be clean — inline [[_term]] declarations resolve and \
         fenced <<not_a_ref>> is not a reference; got:\n{check_out}"
    );
    assert!(
        !check_out.contains("unresolved"),
        "no unresolved refs expected; got:\n{check_out}"
    );
}
