// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::Value;
use std::process::{Command, Output};

fn temp_project(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aden-result-semantics-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn aden(args: &[&std::ffi::OsStr], data_dir: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(args)
        .env("ADEN_DATA_DIR", data_dir)
        .output()
        .unwrap()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid JSON: {e}\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn check_summary(doc: &Value) -> Result<(bool, u64), &'static str> {
    let ok = doc["ok"].as_bool().ok_or("ok must be a boolean")?;
    let counts = doc["counts"]
        .as_object()
        .ok_or("counts must be an object")?;
    if ["errors", "warnings", "info"]
        .into_iter()
        .any(|name| doc.get(name).is_some())
    {
        return Err("errors, warnings, and info must be nested in counts");
    }
    let count = |name, message| counts.get(name).and_then(Value::as_u64).ok_or(message);
    let errors = count("errors", "counts.errors must be an unsigned integer")?;
    count("warnings", "counts.warnings must be an unsigned integer")?;
    count("info", "counts.info must be an unsigned integer")?;
    Ok((ok, errors))
}

fn documented_check_summary() -> Value {
    const MARKER: &str = "[source,json,id=check-json-summary]";
    let mut lines = include_str!("../../../docs/commands.adoc")
        .lines()
        .peekable();
    assert!(
        lines.any(|line| line == MARKER),
        "missing {MARKER} in docs/commands.adoc"
    );
    assert_eq!(
        lines.next(),
        Some("----"),
        "missing opening delimiter for {MARKER}"
    );
    let mut body = String::new();
    while let Some(line) = lines.next_if(|line| *line != "----") {
        body.push_str(line);
        body.push('\n');
    }
    assert_eq!(
        lines.next(),
        Some("----"),
        "missing closing delimiter for {MARKER}"
    );
    assert!(
        !lines.any(|line| line == MARKER),
        "duplicate {MARKER} in docs/commands.adoc"
    );
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("invalid JSON in {MARKER}: {e}"))
}

fn assert_complete_result(doc: &Value) {
    let result = &doc["result"];
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["complete"], true);
    assert!(result.get("outcome").is_some());
    assert!(result.get("result_state").is_some());
    assert!(result.get("graph_health").is_some());
    assert!(result.get("policy_outcome").is_some());
    assert!(result.get("freshness_outcome").is_some());
}

fn valid_project(label: &str) -> std::path::PathBuf {
    let project = temp_project(label);
    std::fs::write(project.join("README.adoc"), "[[root]]\n= Root\n").unwrap();
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"result-semantics-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    std::fs::write(project.join("NOTICE.md"), "Fixture accreditation.\n").unwrap();
    project
}

#[test]
fn audit_json_distinguishes_empty_findings_and_blocked_without_exit_migration() {
    let data = temp_project("data");
    let clean = temp_project("clean");
    std::fs::write(clean.join("safe.py"), "print('safe')\n").unwrap();
    let out = aden(&["-j".as_ref(), "audit".as_ref(), clean.as_os_str()], &data);
    assert!(out.status.success());
    let doc = json(&out);
    assert_eq!(doc["result"]["outcome"], "clean");
    assert_eq!(doc["result"]["result_state"], "empty");
    assert_eq!(doc["result"]["complete"], true);

    let findings = temp_project("findings");
    std::fs::write(findings.join("unsafe.py"), "exec(user_input)\n").unwrap();
    let out = aden(
        &["-j".as_ref(), "audit".as_ref(), findings.as_os_str()],
        &data,
    );
    assert!(out.status.success(), "advisory findings retain exit 0");
    let doc = json(&out);
    assert_eq!(doc["result"]["outcome"], "passed_with_findings");
    assert_eq!(doc["result"]["result_state"], "complete");
    assert!(doc["result"]["advisory_findings"].as_u64().unwrap() > 0);

    let out = aden(
        &[
            "-j".as_ref(),
            "audit".as_ref(),
            findings.as_os_str(),
            "--strict".as_ref(),
        ],
        &data,
    );
    assert!(!out.status.success());
    let doc = json(&out);
    assert_eq!(doc["result"]["outcome"], "blocked");
    assert!(doc["result"]["blocking_findings"].as_u64().unwrap() > 0);
}

#[test]
fn ask_empty_result_keeps_machine_json_contract() {
    let data = temp_project("ask-empty-data");
    let project = valid_project("ask-empty");
    let out = aden(
        &[
            "-j".as_ref(),
            "ask".as_ref(),
            "zzzz_no_such_anchor_7f31".as_ref(),
            project.as_os_str(),
        ],
        &data,
    );
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = json(&out);
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["result_state"], "empty");
    assert_eq!(doc["context"], "");
    assert!(
        doc["recovery"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
}

#[test]
fn check_json_matches_documented_summary() {
    let data = temp_project("check-summary-data");
    let project = valid_project("check-summary-project");
    let out = aden(
        &[
            "check".as_ref(),
            project.as_os_str(),
            "--severity".as_ref(),
            "Forbid".as_ref(),
            "--json".as_ref(),
        ],
        &data,
    );
    assert!(
        out.status.success(),
        "check rejected a valid project: stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = json(&out);
    let actual = check_summary(&doc).unwrap_or_else(|e| panic!("invalid check output: {e}: {doc}"));
    assert_eq!(actual, (true, 0), "successful check summary: {doc}");

    let example = documented_check_summary();
    let documented = check_summary(&example).unwrap_or_else(|e| {
        panic!("invalid check-json-summary in docs/commands.adoc: {e}: {example}")
    });
    assert_eq!(documented, actual, "documented check summary: {example}");
}

#[test]
fn check_json_summary_rejects_flat_counters() {
    let flat = serde_json::json!({"ok": true, "errors": 0, "warnings": 0, "info": 7});
    assert_eq!(check_summary(&flat), Err("counts must be an object"));

    for name in ["errors", "warnings", "info"] {
        let mut mixed =
            serde_json::json!({"ok": true, "counts": {"errors": 0, "warnings": 0, "info": 7}});
        mixed[name] = flat[name].clone();
        assert_eq!(
            check_summary(&mixed),
            Err("errors, warnings, and info must be nested in counts"),
            "accepted top-level {name}: {mixed}"
        );
    }
}

#[test]
fn check_json_keeps_graph_policy_and_freshness_axes_stable() {
    let data = temp_project("check-data");
    let project = temp_project("check-project");
    std::fs::write(
        project.join("README.adoc"),
        "[[root]]\n= Root\n\n<<missing-anchor>>\n",
    )
    .unwrap();
    let out = aden(
        &[
            "-j".as_ref(),
            "check".as_ref(),
            project.as_os_str(),
            "--severity".as_ref(),
            "Forbid".as_ref(),
        ],
        &data,
    );
    assert!(!out.status.success());
    let doc = json(&out);
    let (ok, errors) =
        check_summary(&doc).unwrap_or_else(|e| panic!("invalid blocked check output: {e}: {doc}"));
    assert!(!ok, "blocked check reported ok: {doc}");
    assert!(errors > 0, "blocked check reported no errors: {doc}");
    assert_eq!(doc["result"]["outcome"], "blocked");
    assert_eq!(doc["result"]["graph_health"], "unhealthy");
    assert!(doc["result"].get("policy_outcome").is_some());
    assert_eq!(doc["result"]["freshness_outcome"], "not_evaluated");
    assert_eq!(doc["result"]["complete"], true);
}

#[test]
fn validation_command_matrix_emits_additive_complete_json_without_exit_migration() {
    let data = temp_project("matrix-data");
    let project = valid_project("matrix-project");

    let cases: &[&[&std::ffi::OsStr]] = &[
        &[
            "-j".as_ref(),
            "check".as_ref(),
            project.as_os_str(),
            "--severity".as_ref(),
            "Forbid".as_ref(),
        ],
        &[
            "diagnose".as_ref(),
            "--format".as_ref(),
            "json".as_ref(),
            project.as_os_str(),
        ],
        &["-j".as_ref(), "heal".as_ref(), project.as_os_str()],
        &["-j".as_ref(), "status".as_ref(), project.as_os_str()],
        &["-j".as_ref(), "ci-check".as_ref(), project.as_os_str()],
    ];

    for args in cases {
        let out = aden(args, &data);
        assert!(
            out.status.success(),
            "{args:?} unexpectedly changed exit behavior: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_complete_result(&json(&out));
    }
}

#[test]
fn ready_and_ci_human_verdicts_label_clean_runs_explicitly() {
    let data = temp_project("human-data");
    let project = valid_project("human-project");

    let ci = aden(
        &["--human".as_ref(), "ci-check".as_ref(), project.as_os_str()],
        &data,
    );
    assert!(ci.status.success());
    let ci_stdout = String::from_utf8_lossy(&ci.stdout);
    assert!(
        ci_stdout.contains("[CI] Outcome: clean")
            || ci_stdout.contains("[CI] Outcome: passed_with_findings"),
        "stdout={ci_stdout}"
    );

    let ready = aden(
        &["--human".as_ref(), "ready".as_ref(), project.as_os_str()],
        &data,
    );
    assert!(ready.status.success());
    let ready_stdout = String::from_utf8_lossy(&ready.stdout);
    assert!(
        ready_stdout.contains("[ready] Outcome: clean")
            || ready_stdout.contains("[ready] Outcome: passed_with_findings"),
        "stdout={ready_stdout}"
    );
}

#[test]
fn ci_advisories_remain_zero_exit_but_are_never_rendered_as_clean() {
    let data = temp_project("ci-advisory-data");
    let project = valid_project("ci-advisory-project");
    std::fs::write(
        project.join("src/lib.rs"),
        format!(
            "pub const FIXTURE_URL: &str = \"{}{}\";\n",
            "http", "://example.invalid"
        ),
    )
    .unwrap();

    let json_out = aden(
        &["-j".as_ref(), "ci-check".as_ref(), project.as_os_str()],
        &data,
    );
    assert!(json_out.status.success());
    let doc = json(&json_out);
    assert_eq!(doc["result"]["outcome"], "passed_with_findings");
    assert!(doc["result"]["advisory_findings"].as_u64().unwrap() > 0);

    let human_out = aden(
        &["--human".as_ref(), "ci-check".as_ref(), project.as_os_str()],
        &data,
    );
    assert!(human_out.status.success());
    let stdout = String::from_utf8_lossy(&human_out.stdout);
    assert!(
        stdout.contains("[CI] Outcome: passed_with_findings"),
        "stdout={stdout}"
    );
    assert!(
        !stdout.contains("ALL GATES PASSED — Ready to commit"),
        "stdout={stdout}"
    );
}

#[test]
fn ci_json_keeps_stdout_machine_readable_when_nested_audit_finds_a_vulnerability() {
    let data = temp_project("ci-audit-json-data");
    let project = temp_project("ci-audit-json-project");
    std::fs::write(project.join("README.adoc"), "[[root]]\n= Root\n").unwrap();
    // This is intentionally a failing project: the assertion is about the
    // *single JSON envelope* on stdout, including when a nested hard gate
    // emits its own human security diagnostic.
    std::fs::write(project.join("unsafe.py"), "exec(user_input)\n").unwrap();

    let out = aden(
        &["-j".as_ref(), "ci-check".as_ref(), project.as_os_str()],
        &data,
    );
    assert!(
        !out.status.success(),
        "OWASP finding remains a blocking CI gate"
    );
    let doc = json(&out);
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["result"]["outcome"], "blocked");
    assert!(String::from_utf8_lossy(&out.stderr).contains("OWASP Security Audit Findings"));
}
