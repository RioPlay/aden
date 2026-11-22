// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::{Value, json};
use std::fs;
use std::process::{Command, Output, Stdio};

const CLAIM: &str = "docs/guide.adoc";
const EVIDENCE: &str = "src/check.rs";
const ORIGINAL_EVIDENCE: &str = "fn check() -> bool {\n    true\n}\n";

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("docs")).unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join(CLAIM), "An authored claim\n").unwrap();
        fs::write(dir.path().join(EVIDENCE), ORIGINAL_EVIDENCE).unwrap();
        Self { dir }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aden"));
        command
            .current_dir(self.dir.path())
            .env("ADEN_DATA_DIR", self.dir.path().join("data"))
            .env_remove("ADEN_STORE");
        command
    }

    fn source_hash(&self, file: &str) -> String {
        let output = self.command().args(["read", file]).output().unwrap();
        successful_json(output)["source"]["source_hash"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn manifest(&self, claim_path: &str, claim_start: u32) {
        self.manifest_with_evidence(claim_path, claim_start, &[(2, "supports")]);
    }

    fn manifest_with_evidence(&self, claim_path: &str, claim_start: u32, evidence: &[(u32, &str)]) {
        let claim_hash = self.source_hash(CLAIM);
        let evidence_hash = self.source_hash(EVIDENCE);
        let mut manifest = format!(
            r#"version = 1

[[findings]]
id = "sample"

[findings.claim]
file = "{claim_path}"
start_line = {claim_start}
end_line = 1
source_hash = "{claim_hash}"
"#
        );

        for (line, relation) in evidence {
            manifest.push_str(&format!(
                r#"
[[findings.evidence]]
file = "{EVIDENCE}"
start_line = {line}
end_line = {line}
source_hash = "{evidence_hash}"
relation = "{relation}"
"#
            ));
        }

        fs::write(self.dir.path().join("aden-findings.toml"), manifest).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command()
            .arg("findings")
            .arg(self.dir.path())
            .args(args)
            .output()
            .unwrap()
    }

    fn findings(&self, args: &[&str]) -> Value {
        successful_json(self.run(args))
    }

    fn write_evidence(&self, text: &str) {
        fs::write(self.dir.path().join(EVIDENCE), text).unwrap();
    }
}

fn successful_json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn checked_findings(
    fixture: &Fixture,
    scope: &[&str],
    status: i32,
    matched: u32,
    needs_review: u32,
    contradictions: u32,
) -> Value {
    let mut args = vec!["--check"];
    args.extend(scope);
    let output = fixture.run(&args);
    assert_eq!(
        output.status.code(),
        Some(status),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report.as_object_mut().unwrap().remove("gate"),
        Some(json!({
            "ok": status == 0,
            "matched": matched,
            "needs_review": needs_review,
            "contradictions": contradictions,
        }))
    );
    report
}

#[test]
fn checks_current_findings_and_fails_an_empty_selection() {
    let fixture = Fixture::new();
    fixture.manifest(CLAIM, 1);

    let inspection = fixture.findings(&[]);
    assert_eq!(inspection["findings"][0]["state"], "current");
    assert_eq!(checked_findings(&fixture, &[], 0, 1, 0, 0), inspection);

    for (file, line) in [(CLAIM, "1"), (EVIDENCE, "2")] {
        let scope = ["--file", file, "--line", line];
        assert_eq!(
            checked_findings(&fixture, &scope, 0, 1, 0, 0),
            fixture.findings(&scope)
        );
    }

    let empty_scope = ["--file", EVIDENCE, "--line", "3"];
    let empty = checked_findings(&fixture, &empty_scope, 1, 0, 0, 0);
    assert_eq!(empty["findings"], json!([]));
    assert_eq!(empty, fixture.findings(&empty_scope));

    let output = fixture.run(&["--check", "--human"]);
    assert_eq!(output.status.code(), Some(0));
    let human = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        human.lines().last(),
        Some("Findings check passed (1 matched, 0 need review, 0 with contradictions).")
    );
}

#[test]
fn check_preserves_verdict_when_stdout_closes() {
    let fixture = Fixture::new();
    fixture.manifest(CLAIM, 1);

    for (args, expected) in [(&[][..], 0), (&["--file", "missing.adoc"][..], 1)] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let output = fixture
            .command()
            .arg("findings")
            .args(args)
            .arg("--check")
            .stdout(Stdio::from(writer))
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn check_fails_on_changed_evidence_and_recovers_on_restore() {
    let fixture = Fixture::new();
    fixture.manifest(CLAIM, 1);

    fixture.write_evidence("fn check() -> bool {\n    false\n}\n");
    let changed = checked_findings(&fixture, &[], 1, 1, 1, 0);
    assert_eq!(changed["findings"][0]["state"], "needs_review");
    assert_eq!(changed, fixture.findings(&[]));

    let output = fixture.run(&["--check", "--human"]);
    assert_eq!(output.status.code(), Some(1));
    let human = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        human.lines().last(),
        Some("Findings check failed (1 matched, 1 need review, 0 with contradictions).")
    );

    fixture.write_evidence(ORIGINAL_EVIDENCE);
    let restored = checked_findings(&fixture, &[], 0, 1, 0, 0);
    assert_eq!(restored["findings"][0]["state"], "current");
}

#[test]
fn check_counts_current_contradictions_once_per_finding() {
    let fixture = Fixture::new();
    fixture.manifest_with_evidence(CLAIM, 1, &[(2, "contradicts"), (3, "contradicts")]);

    let report = checked_findings(&fixture, &[], 1, 1, 0, 1);
    let finding = &report["findings"][0];
    assert_eq!(finding["state"], "current");
    let evidence = finding["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 2);
    for span in evidence {
        assert_eq!(span["relation"], "contradicts");
        assert_eq!(span["state"], "current");
    }
    assert_eq!(report, fixture.findings(&[]));
}

#[test]
fn looks_up_both_ends_without_swapping_the_claim_and_evidence() {
    let fixture = Fixture::new();
    fixture.manifest(CLAIM, 1);

    let report = fixture.findings(&[]);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["provenance"], "authored");
    assert_eq!(report["edge_direction"], "evidence_to_claim");
    let findings = report["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1);
    let finding = &findings[0];
    assert_eq!(finding["id"], "sample");
    assert_eq!(finding["state"], "current");
    assert_eq!(finding["claim"]["file"], CLAIM);
    assert_eq!(finding["claim"]["start_line"], 1);
    assert_eq!(finding["claim"]["end_line"], 1);
    assert_eq!(finding["claim"]["state"], "current");
    let evidence = finding["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0]["file"], EVIDENCE);
    assert_eq!(evidence[0]["start_line"], 2);
    assert_eq!(evidence[0]["end_line"], 2);
    assert_eq!(evidence[0]["state"], "current");
    assert_eq!(evidence[0]["relation"], "supports");

    for (file, line) in [(CLAIM, "1"), (EVIDENCE, "2")] {
        let filtered = fixture.findings(&["--file", file, "--line", line]);
        assert_eq!(filtered["findings"], report["findings"]);
    }
    let filtered = fixture.findings(&["--file", EVIDENCE]);
    assert_eq!(filtered["findings"], report["findings"]);
    let unrelated = fixture.findings(&["--file", EVIDENCE, "--line", "3"]);
    assert_eq!(unrelated["findings"], json!([]));
    let human = fixture.run(&["--human", "--file", EVIDENCE]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("Claim: docs/guide.adoc:1-1 [current]"));
    assert!(human.contains("supports claim: src/check.rs:2-2 [current]"));
    assert!(!fixture.dir.path().join("data").exists());
    assert!(!fixture.dir.path().join(".aden").exists());
}

#[test]
fn reports_source_changes_and_recovers_when_the_original_bytes_return() {
    let fixture = Fixture::new();
    fixture.manifest(CLAIM, 1);
    let original = fixture.findings(&[]);
    assert_eq!(original["findings"][0]["state"], "current");

    fixture.write_evidence("fn check() -> bool {\n    false\n}\n");
    let changed = fixture.findings(&["--file", EVIDENCE, "--line", "2"]);
    let finding = &changed["findings"][0];
    assert_eq!(finding["id"], "sample");
    assert_eq!(finding["state"], "needs_review");
    assert_eq!(finding["claim"]["state"], "current");
    assert_eq!(finding["evidence"][0]["state"], "changed");

    fixture.write_evidence("Check\n");
    let shortened = fixture.findings(&[]);
    assert_eq!(shortened["findings"][0]["state"], "needs_review");
    assert_eq!(
        shortened["findings"][0]["evidence"][0]["state"],
        "range_unavailable"
    );

    fs::remove_file(fixture.dir.path().join(EVIDENCE)).unwrap();
    let missing = fixture.findings(&[]);
    assert_eq!(missing["findings"][0]["state"], "needs_review");
    assert_eq!(
        missing["findings"][0]["evidence"][0]["state"],
        "unavailable"
    );

    fixture.write_evidence(ORIGINAL_EVIDENCE);
    assert_eq!(fixture.findings(&[]), original);
}

#[test]
fn respects_source_exclusions() {
    let fixture = Fixture::new();
    fixture.manifest(CLAIM, 1);
    assert_eq!(fixture.findings(&[])["findings"][0]["state"], "current");

    fs::write(
        fixture.dir.path().join(".adenignore"),
        format!("{EVIDENCE}\n"),
    )
    .unwrap();
    let report = fixture.findings(&[]);
    assert_eq!(report["findings"][0]["state"], "needs_review");
    assert_eq!(report["findings"][0]["evidence"][0]["state"], "unavailable");
}

#[test]
fn treats_nested_checkouts_as_out_of_scope() {
    let fixture = Fixture::new();
    fixture.manifest(CLAIM, 1);
    assert_eq!(fixture.findings(&[])["findings"][0]["state"], "current");

    fs::create_dir(fixture.dir.path().join("src/.git")).unwrap();
    let report = fixture.findings(&[]);
    assert_eq!(report["findings"][0]["state"], "needs_review");
    assert_eq!(report["findings"][0]["evidence"][0]["state"], "unavailable");
}

#[test]
fn rejects_parent_directory_paths_and_zero_based_spans() {
    let fixture = Fixture::new();
    for (claim_path, claim_start) in [("../outside.adoc", 1), (CLAIM, 0)] {
        fixture.manifest(claim_path, claim_start);
        let output = fixture.run(&[]);
        assert!(
            !output.status.success(),
            "accepted claim span {claim_path}:{claim_start}"
        );
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}
