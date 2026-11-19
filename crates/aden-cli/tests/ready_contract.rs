// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture {
    _root: tempfile::TempDir,
    project: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new(broken_reference: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let data = root.path().join("data");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname=\"ready-contract-fixture\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
        )
        .unwrap();
        std::fs::write(project.join("src/lib.rs"), "pub fn noop() {}\n").unwrap();
        let document = if broken_reference {
            "[[root]]\n= Root\n\n<<missing-ready-contract-anchor>>\n"
        } else {
            "[[root]]\n= Root\n"
        };
        std::fs::write(project.join("README.adoc"), document).unwrap();
        std::fs::write(project.join("NOTICE.md"), "Fixture accreditation.\n").unwrap();
        Self {
            _root: root,
            project,
            data,
        }
    }

    fn ready(&self, fix: bool, human: bool) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aden"));
        if !human {
            command.arg("-j");
        }
        command.arg("ready").arg(&self.project);
        if fix {
            command.arg("--fix");
        }
        if human {
            command.arg("--human");
        }
        command.env("ADEN_DATA_DIR", &self.data).output().unwrap()
    }

    fn user_bytes(&self) -> Vec<Vec<u8>> {
        ["Cargo.toml", "src/lib.rs", "README.adoc", "NOTICE.md"]
            .iter()
            .map(|file| std::fs::read(self.project.join(file)).unwrap())
            .collect()
    }
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "Expected one JSON envelope: {error}\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn step<'a>(document: &'a Value, name: &str) -> &'a Value {
    document["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["name"] == name)
        .unwrap()
}

fn assert_scope(document: &Value) {
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["command"], "ready");
    assert_eq!(document["assessment_scope"], "local_static_checks");
    assert_eq!(document["graph_refresh_requested"], true);
    assert_eq!(
        document["unverified"],
        serde_json::json!(["runtime_behavior", "project_tests", "external_tool_gates"])
    );
    assert_eq!(document["steps"].as_array().unwrap().len(), 5);
    for step in document["steps"].as_array().unwrap() {
        assert!(
            step["scope"]
                .as_str()
                .is_some_and(|scope| !scope.is_empty())
        );
        assert!(
            step["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty())
        );
    }
}

#[test]
fn clean_ready_reports_bounded_scope_and_preserves_user_files() {
    let fixture = Fixture::new(false);
    let before = fixture.user_bytes();
    let output = fixture.ready(false, false);
    let document = json(&output);
    assert!(output.status.success(), "{document}");
    assert_scope(&document);
    assert_eq!(document["ok"], true);
    assert_eq!(document["source_repairs_requested"], false);
    assert_eq!(document["heal_repair_attempted"], false);
    assert!(document["heal_repairs"].is_null());
    assert!(document["repair_failure"].is_null());
    assert_eq!(
        step(&document, "check refs")["evidence_kind"],
        "structural_invariant"
    );
    assert_eq!(step(&document, "lint")["evidence_kind"], "heuristic_signal");
    assert_eq!(
        step(&document, "owasp audit")["evidence_kind"],
        "heuristic_signal"
    );
    for step in document["steps"].as_array().unwrap() {
        assert_eq!(step["status"], "passed");
        assert_eq!(step["passed"], true);
    }
    assert_eq!(fixture.user_bytes(), before);
    let output = fixture.ready(false, true);
    assert!(output.status.success());
    let human = String::from_utf8(output.stdout).unwrap();
    assert!(human.contains("Outcome: clean") || human.contains("Outcome: passed_with_findings"));
    assert!(human.contains("Not evaluated: runtime behavior, project tests, external-tool gates."));
    assert!(
        human.contains("Local static checks passed")
            || human.contains("Local blocking checks passed")
    );
    assert!(!human.contains("commit-ready"));
    assert_eq!(fixture.user_bytes(), before);
}

#[test]
fn unresolved_reference_fails_its_check_and_skips_remaining_checks() {
    let fixture = Fixture::new(true);
    let before = fixture.user_bytes();
    let output = fixture.ready(false, false);
    let document = json(&output);
    assert!(!output.status.success());
    assert_scope(&document);
    assert_eq!(document["ok"], false);
    assert_eq!(document["outcome"], "blocked");
    assert_eq!(step(&document, "gen")["status"], "passed");
    assert_eq!(step(&document, "lint")["status"], "passed");
    assert_eq!(step(&document, "check refs")["status"], "failed");
    for name in ["heal drift scan", "owasp audit"] {
        let skipped = step(&document, name);
        assert_eq!(skipped["status"], "skipped");
        assert_eq!(skipped["passed"], false);
        assert!(skipped["reason"].as_str().unwrap().contains("check refs"));
    }
    assert_eq!(document["heal_repair_attempted"], false);
    assert_eq!(fixture.user_bytes(), before);
    let output = fixture.ready(false, true);
    assert!(!output.status.success());
    let human = String::from_utf8(output.stdout).unwrap();
    assert!(human.contains("SKIP"));
    assert!(human.contains("Local static checks blocked"));
    assert!(!human.contains("commit-ready"));
}

#[test]
fn explicit_fix_reports_attempts_without_extra_json_or_unrequested_changes() {
    let fixture = Fixture::new(false);
    let before = fixture.user_bytes();
    let output = fixture.ready(true, false);
    let document = json(&output);
    assert!(output.status.success(), "{document}");
    assert_scope(&document);
    assert_eq!(document["source_repairs_requested"], true);
    assert_eq!(document["heal_repair_attempted"], true);
    assert!(document["heal_repairs"].is_object());
    assert_eq!(document["heal_repairs"]["failed"], 0);
    assert_eq!(document["heal_repairs"]["conflicts"], 0);
    assert!(document["repair_failure"].is_null());
    assert_eq!(step(&document, "heal drift scan")["status"], "passed");
    assert_eq!(fixture.user_bytes(), before);
}
