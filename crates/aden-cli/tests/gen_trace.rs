// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

fn project() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname='gen-trace'\nversion='0.1.0'\n",
    )
    .unwrap();
    std::fs::write(root.path().join("lib.rs"), "pub fn generation_probe() {}\n").unwrap();
    root
}

fn run(root: &Path, data: &Path, args: &[&str], enabled: bool, machine: bool) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(args)
        .current_dir(root)
        .env("ADEN_DATA_DIR", data)
        .env("ADEN_GEN_TRACE", if enabled { "1" } else { "0" })
        .env("ADEN_READ_TRACE", if enabled { "1" } else { "0" })
        .env("ADEN_MCP_MACHINE_ERRORS", if machine { "1" } else { "0" })
        .env("ADEN_MCP_VERSION", env!("CARGO_PKG_VERSION"))
        .env_remove("ADEN_STORE")
        .env_remove("ADEN_SKIP_AUTO_GEN")
        .output()
        .unwrap()
}

fn traces(output: &Output, trace_type: &str) -> Vec<Value> {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value["type"] == trace_type)
        .collect()
}

#[test]
fn explicit_generation_trace_preserves_stdout_and_reports_publication() {
    let root = project();
    let plain_data = tempfile::tempdir().unwrap();
    let traced_data = tempfile::tempdir().unwrap();
    let plain = run(
        root.path(),
        plain_data.path(),
        &["gen", ".", "--quiet"],
        false,
        false,
    );
    let traced = run(
        root.path(),
        traced_data.path(),
        &["gen", ".", "--quiet"],
        true,
        false,
    );
    assert!(plain.status.success());
    assert!(
        traced.status.success(),
        "{}",
        String::from_utf8_lossy(&traced.stderr)
    );
    assert_eq!(plain.stdout, traced.stdout);
    assert!(traces(&plain, "aden_gen_trace").is_empty());
    let reports = traces(&traced, "aden_gen_trace");
    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert_eq!(report["outcome"], "completed");
    assert_eq!(report["counts"]["snapshot_published"], 1);
    assert_eq!(report["counts"]["freshness_manifest_published"], 1);
    assert_eq!(report["counts"]["reindexed_files"], 2);
    for stage in [
        "initial_fingerprint",
        "discovery",
        "parse_prepare",
        "persist_prune",
        "linking",
        "store_close",
        "snapshot_publish",
    ] {
        assert!(
            report["stages"][stage]["elapsed_us"].as_u64().is_some(),
            "{stage}"
        );
    }
    let text = report.to_string();
    assert!(!text.contains("generation_probe"));
    assert!(!text.contains("lib.rs"));
    assert!(!text.contains(root.path().to_string_lossy().as_ref()));
}

#[test]
fn cold_read_reports_refresh_and_warm_read_does_not_regenerate() {
    let root = project();
    let data = tempfile::tempdir().unwrap();
    let args = [
        "query",
        "--from",
        "generation_probe",
        "--depth",
        "0",
        "--max-results",
        "1",
    ];
    for expected_refreshes in [1, 0] {
        let output = run(root.path(), data.path(), &args, true, false);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // The detached worker does not write to the reader's stderr.
        assert!(traces(&output, "aden_gen_trace").is_empty());
        let reports = traces(&output, "aden_read_trace");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0]["stages"]["refresh"]["calls"], expected_refreshes);
        let evidence: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(evidence["context_receipt"]["freshness"], "current");
        assert_eq!(evidence["returned"], 1);
        assert!(
            evidence["items"][0]["anchor"]
                .as_str()
                .is_some_and(|anchor| anchor.ends_with("#generation_probe")),
            "{evidence}"
        );
    }
}

#[test]
fn generation_error_stderr_is_unchanged() {
    let root = project();
    let data = tempfile::tempdir().unwrap();
    let args = ["gen", "missing-directory"];
    let plain = run(root.path(), data.path(), &args, false, true);
    let failed = run(root.path(), data.path(), &args, true, true);
    assert!(!failed.status.success());
    assert!(!plain.status.success());
    assert!(traces(&failed, "aden_gen_trace").is_empty());
    assert_eq!(failed.stderr, plain.stderr);
}
