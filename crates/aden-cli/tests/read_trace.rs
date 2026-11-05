// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::Value;
use std::process::{Command, Output};

struct Fixture {
    root: tempfile::TempDir,
    data: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname='trace-fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(root.path().join("lib.rs"), "pub fn trace_fixture() {}\n").unwrap();
        Self { root, data }
    }

    fn run(&self, args: &[&str], enabled: bool, machine: bool) -> Output {
        self.run_with_store(args, enabled, machine, None)
    }

    fn run_with_store(
        &self,
        args: &[&str],
        enabled: bool,
        machine: bool,
        store: Option<&std::path::Path>,
    ) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aden"));
        command
            .args(args)
            .current_dir(self.root.path())
            .env("ADEN_DATA_DIR", self.data.path())
            .env("ADEN_READ_TRACE", if enabled { "1" } else { "0" })
            .env("ADEN_MCP_MACHINE_ERRORS", if machine { "1" } else { "0" })
            .env("ADEN_MCP_VERSION", env!("CARGO_PKG_VERSION"))
            .env_remove("ADEN_STORE")
            .env_remove("ADEN_SKIP_AUTO_GEN");
        if let Some(store) = store {
            command.env("ADEN_STORE", store);
        }
        command.output().unwrap()
    }
}

fn trace(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let traces: Vec<_> = String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value["type"] == "aden_read_trace")
        .collect();
    assert_eq!(
        traces.len(),
        1,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    traces.into_iter().next().unwrap()
}

#[test]
fn disabled_trace_leaves_source_output_and_stderr_unchanged() {
    let fixture = Fixture::new();
    let plain = fixture.run(&["read", "lib.rs"], false, false);
    assert!(plain.status.success());
    assert!(plain.stderr.is_empty());
    let enabled = fixture.run(&["read", "lib.rs"], true, false);
    assert_eq!(plain.stdout, enabled.stdout);
    let telemetry = trace(&enabled);
    assert_eq!(telemetry["schema_version"], 1);
    assert_eq!(telemetry["command"], "read");
    assert_eq!(telemetry["counters"]["fingerprint_scans"], 0);
    assert_eq!(telemetry["counters"]["index_cache_misses"], 0);
    assert!(telemetry["total_elapsed_us"].as_u64().is_some());
    let diagnostic = telemetry.to_string();
    assert!(!diagnostic.contains("trace_fixture"));
    assert!(!diagnostic.contains("lib.rs"));
    assert!(!diagnostic.contains(fixture.root.path().to_string_lossy().as_ref()));
}

#[test]
fn warm_graph_reads_report_repeated_fingerprints_and_cache_reuse() {
    let fixture = Fixture::new();
    let first = fixture.run(&["search", "trace_fixture", ".", "--json"], true, false);
    let cold = trace(&first);
    let second = fixture.run(&["search", "trace_fixture", ".", "--json"], true, false);
    let warm = trace(&second);
    let receipt: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(receipt["context_receipt"]["freshness"], "current");
    assert!(cold["stages"]["refresh"]["calls"].as_u64().unwrap() >= 1);
    assert!(cold["counters"]["index_cache_misses"].as_u64().unwrap() >= 1);
    assert_eq!(warm["stages"]["refresh"]["calls"], 0);
    // Stage telemetry must remain valid even when an optimization eliminates
    // or consolidates setup work. Independent freshness observations below
    // are the safety contract, not a fixed number of helper invocations.
    for stage in [
        "root_resolve",
        "store_resolve",
        "store_compatibility",
        "manifest_load",
    ] {
        assert!(warm["stages"][stage]["calls"].as_u64().is_some());
        assert!(warm["stages"][stage]["elapsed_us"].as_u64().is_some());
    }
    assert_eq!(warm["counters"]["compatibility_store_opens"], 0);
    assert!(warm["counters"]["fingerprint_scans"].as_u64().unwrap() >= 2);
    assert!(warm["counters"]["fingerprint_files"].as_u64().unwrap() >= 2);
    assert!(warm["counters"]["fingerprint_bytes"].as_u64().unwrap() >= 2 * 26);
    assert!(warm["counters"]["index_cache_hits"].as_u64().unwrap() >= 1);
    assert!(
        warm["timing_semantics"]
            .as_str()
            .unwrap()
            .contains("must not be summed")
    );
}

#[test]
fn incompatible_markers_keep_engine_classification_and_recovery() {
    let fixture = Fixture::new();
    let first = fixture.run(&["search", "trace_fixture", ".", "--json"], true, false);
    trace(&first);
    let project = std::fs::read_dir(fixture.data.path().join("projects"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(project.join("store/version"), b"FJL\x02").unwrap();
    let repaired = fixture.run(&["search", "trace_fixture", ".", "--json"], true, false);
    let telemetry = trace(&repaired);
    assert_eq!(telemetry["counters"]["compatibility_store_opens"], 1);
    assert!(telemetry["stages"]["refresh"]["calls"].as_u64().unwrap() >= 1);
    let result: Value = serde_json::from_slice(&repaired.stdout).unwrap();
    assert_eq!(result["context_receipt"]["freshness"], "current");
    assert_eq!(
        result["context_receipt"]["refresh_cause"],
        "store_incompatible"
    );
    assert_eq!(
        std::fs::read(project.join("store/version")).unwrap()[..4],
        *b"FJL\x03"
    );
}

#[test]
fn incompatible_pinned_store_is_never_wiped_for_recovery() {
    let fixture = Fixture::new();
    let store = fixture.data.path().join("shared-store");
    let args = ["search", "trace_fixture", ".", "--json"];
    trace(&fixture.run_with_store(&args, true, false, Some(&store)));
    std::fs::write(store.join("version"), b"FJL\x02").unwrap();
    std::fs::write(store.join("shared-sentinel"), b"preserve shared data").unwrap();
    let refused = fixture.run_with_store(&args, true, false, Some(&store));
    // A current search cache can still answer the read after recovery is
    // refused. The contract here is preservation and actionable diagnostics,
    // not that every snapshot/cache-backed read must fail.
    assert!(String::from_utf8_lossy(&refused.stderr).contains("pinned/shared"));
    assert_eq!(std::fs::read(store.join("version")).unwrap(), b"FJL\x02");
    assert_eq!(
        std::fs::read(store.join("shared-sentinel")).unwrap(),
        b"preserve shared data"
    );
}

#[test]
fn tracing_cannot_append_diagnostics_to_machine_errors() {
    let fixture = Fixture::new();
    let output = fixture.run(&["read", "missing.rs"], true, true);
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "source_unavailable");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("aden_read_trace"));
}
