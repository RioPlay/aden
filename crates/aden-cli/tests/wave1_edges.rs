// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wave 1 graph-type activation evals (graph-type-roadmap.adoc):
//!
//! 1. `Tests` edges — `impact-diff` must list the tests covering a changed
//!    symbol in an always-on `affected_tests` section.
//! 2. `Implements` edges — the blast radius of a change to a trait must reach
//!    the implementors' methods (calls through trait objects no longer
//!    silently truncate impact).
//!
//! These run the freshly-built binary (`CARGO_BIN_EXE_aden`) against a
//! hermetic git fixture, with the store isolated via a per-test
//! `ADEN_DATA_DIR`, so they are offline and deterministic.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Fixture source: a trait with two implementors, a caller through the trait
/// object, a plain helper, and a test (under `tests/`) covering the helper.
const GREETER_RS: &str = r#"
pub trait Greeter {
    fn greet(&self) -> String;
}

pub struct English;

impl Greeter for English {
    fn greet(&self) -> String {
        make_greeting("hello")
    }
}

pub struct French;

impl Greeter for French {
    fn greet(&self) -> String {
        make_greeting("bonjour")
    }
}

pub fn greet_all(g: &dyn Greeter) -> String {
    g.greet()
}

pub fn make_greeting(word: &str) -> String {
    format!("{word}!")
}
"#;

const GREETER_TEST_RS: &str = r#"
#[test]
fn test_make_greeting() {
    let got = make_greeting("hi");
    assert_eq!(got, "hi!");
}
"#;

fn unique_dir(label: &str) -> PathBuf {
    // pid+nanos alone collides on macOS (µs clock granularity) when parallel
    // test threads enter in the same tick; the counter disambiguates.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "aden-wave1-{label}-{}-{}-{}",
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

/// Scaffold the fixture project as a committed git repo and return
/// (project_dir, data_dir). The data dir isolates the aden store per test.
fn scaffold() -> (PathBuf, PathBuf) {
    let project = unique_dir("proj");
    let data = unique_dir("data");
    std::fs::write(project.join("greeter.rs"), GREETER_RS).unwrap();
    std::fs::create_dir_all(project.join("tests")).unwrap();
    std::fs::write(project.join("tests/greeter_test.rs"), GREETER_TEST_RS).unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["config", "user.email", "wave1@test.invalid"]);
    git(&project, &["config", "user.name", "Wave1 Test"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "fixture"]);
    (project, data)
}

/// Run `aden impact-diff --json <project>` with the isolated data dir and
/// return the parsed JSON output.
fn impact_diff_json(project: &Path, data: &Path) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["impact-diff", "--json"])
        .arg(project)
        .env("ADEN_DATA_DIR", data)
        .output()
        .expect("aden binary must run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "impact-diff failed.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("impact-diff --json must emit JSON ({e}); got: {stdout}"))
}

fn anchors_of(value: &serde_json::Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("JSON must carry an '{key}' array; got: {value}"))
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect()
}

/// Eval (Tests edges): changing a symbol with a known covering test must list
/// that test in the always-on `affected_tests` output.
#[test]
fn impact_diff_lists_affected_tests_for_changed_symbol() {
    let (project, data) = scaffold();
    // Change the body of make_greeting (a symbol covered by test_make_greeting).
    let changed = GREETER_RS.replace(
        "format!(\"{word}!\")",
        "let w = word.trim();\n    format!(\"{w}!\")",
    );
    assert_ne!(changed, GREETER_RS, "fixture edit must apply");
    std::fs::write(project.join("greeter.rs"), changed).unwrap();

    let json = impact_diff_json(&project, &data);
    let affected = anchors_of(&json, "affected_tests");
    assert!(
        affected.iter().any(|a| a.ends_with("#test_make_greeting")),
        "affected_tests must include the covering test test_make_greeting; got: {affected:?}\nfull: {json}"
    );
}

/// Eval (Tests edges, negative): a change to a symbol no test exercises must
/// not claim test coverage.
#[test]
fn impact_diff_affected_tests_empty_when_uncovered() {
    let (project, data) = scaffold();
    // greet_all is called by nothing and tested by nothing.
    let changed = GREETER_RS.replace("g.greet()", "let r = g.greet();\n    r");
    assert_ne!(changed, GREETER_RS, "fixture edit must apply");
    std::fs::write(project.join("greeter.rs"), changed).unwrap();

    let json = impact_diff_json(&project, &data);
    let affected = anchors_of(&json, "affected_tests");
    assert!(
        affected.is_empty(),
        "no test covers greet_all; affected_tests must be empty, got: {affected:?}"
    );
}

/// Eval (Implements edges): the blast set of a change to the trait must
/// include the implementors' methods. Before Wave 1 this reported only the
/// direct `Uses` dependents (silent truncation across polymorphism).
#[test]
fn trait_change_blast_includes_implementor_methods() {
    let (project, data) = scaffold();
    // Change a line INSIDE the trait body so the touched symbol is the trait.
    let changed = GREETER_RS.replace(
        "fn greet(&self) -> String;",
        "/// Render a greeting.\n    fn greet(&self) -> String;",
    );
    assert_ne!(changed, GREETER_RS, "fixture edit must apply");
    std::fs::write(project.join("greeter.rs"), changed).unwrap();

    let json = impact_diff_json(&project, &data);
    let touched: Vec<String> = json["touched"]
        .as_array()
        .expect("touched array")
        .iter()
        .map(|t| t["anchor"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        touched.iter().any(|a| a.ends_with("#Greeter")),
        "the edited line lies inside the trait; touched must include Greeter, got: {touched:?}"
    );
    let impacted = anchors_of(&json, "impacted");
    for method in ["#English::greet", "#French::greet"] {
        assert!(
            impacted.iter().any(|a| a.ends_with(method)),
            "blast set of the trait must reach implementor method {method}; got: {impacted:?}\nfull: {json}"
        );
    }
}
