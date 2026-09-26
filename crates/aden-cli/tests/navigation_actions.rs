// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    _temp: tempfile::TempDir,
    project: PathBuf,
    elsewhere: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("aden navigation 'scope' ")
            .tempdir()
            .unwrap();
        let project = temp.path().join("project");
        let elsewhere = temp.path().join("elsewhere");
        let data = temp.path().join("data");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let git = Command::new("git")
            .args(["init", "--quiet"])
            .arg(&project)
            .output()
            .unwrap();
        assert!(
            git.status.success(),
            "{}",
            String::from_utf8_lossy(&git.stderr)
        );
        // Replays run outside the project, so an action that forgets its scope
        // cannot accidentally pass by relying on the original working directory.
        std::fs::write(elsewhere.join("unrelated.rs"), "fn outside_scope() {}\n").unwrap();
        Self {
            _temp: temp,
            project,
            elsewhere,
            data,
        }
    }

    fn write(&self, file: &str, content: &str) {
        let path = self.project.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_aden"))
            .args(args)
            .current_dir(&self.elsewhere)
            .env("ADEN_DATA_DIR", &self.data)
            .env("ADEN_SKIP_AUTO_GEN", "1")
            .env("ADEN_MCP_MACHINE_ERRORS", "1")
            .env("ADEN_MCP_VERSION", env!("CARGO_PKG_VERSION"))
            .output()
            .unwrap()
    }

    fn scoped(&self, args: &[&str]) -> Output {
        let path = self.project.to_str().unwrap();
        let mut args = args.to_vec();
        args.push(path);
        self.run(&args)
    }

    fn generate(&self) {
        let output = self.scoped(&["gen"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.scoped(args);
        Self::success_json(output)
    }

    fn success_json(output: Output) -> Value {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "invalid JSON ({error}): {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn error(&self, args: &[&str]) -> Value {
        let output = self.scoped(args);
        assert!(
            !output.status.success(),
            "unexpected success: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        serde_json::from_slice(&output.stderr).unwrap_or_else(|error| {
            panic!(
                "invalid machine error ({error}): {}",
                String::from_utf8_lossy(&output.stderr)
            )
        })
    }

    fn replay(&self, action: &Value) -> Value {
        assert_eq!(action["cli"]["program"], "aden");
        let scope = Path::new(action["arguments"]["path"].as_str().unwrap());
        assert!(scope.is_absolute(), "action depends on cwd: {action}");
        assert!(
            std::fs::canonicalize(scope)
                .unwrap()
                .starts_with(std::fs::canonicalize(&self.project).unwrap()),
            "action escaped fixture: {action}"
        );
        let args: Vec<_> = action["cli"]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap())
            .collect();
        assert!(
            args.contains(&"--"),
            "action must end option parsing: {action}"
        );
        Self::success_json(self.run(&args))
    }
}

fn actions(report: &Value) -> &[Value] {
    let actions = report["next_actions"].as_array().unwrap();
    assert!(!actions.is_empty(), "missing executable recovery: {report}");
    assert!(actions.len() <= 3, "unbounded recovery: {report}");
    actions
}

#[test]
fn ambiguous_assembly_preserves_candidates_and_replays_only_exact_targets() {
    let fixture = Fixture::new();
    fixture.write("a.rs", "fn repeated() { let a = 1; }\n");
    fixture.write("b.rs", "fn repeated() { let b = 2; }\n");
    fixture.generate();

    let error = fixture.error(&["asm", "--from", "repeated"]);
    assert_eq!(error["error"]["code"], "ambiguous_symbol");
    let candidates = error["error"]["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(actions(&error).len(), 2);
    assert!(error["anchor"].is_null());
    assert!(error["documents"].is_null());
    for action in actions(&error) {
        assert_eq!(action["tool"], "understand");
        let chosen = &action["arguments"]["symbol"];
        assert!(
            candidates.contains(chosen),
            "not an exact candidate: {action}"
        );
        assert_ne!(chosen, "repeated");
        let inspected = fixture.replay(action);
        assert_eq!(&inspected["anchor"], chosen);
        assert_eq!(inspected["source"]["state"], "complete");
    }
}

#[test]
fn empty_ask_grep_and_locate_offer_replayable_scoped_recovery() {
    let fixture = Fixture::new();
    fixture.write("sample.rs", "fn sample() {}\n");
    fixture.generate();
    let missing = "zzzxmissingyyy123";
    let ask = fixture.json(&["ask", missing]);
    assert_eq!(ask["result_state"], "empty");
    let grep = fixture.json(&["grep", missing]);
    assert_eq!(grep["total"], 0);
    let locate = fixture.json(&["locate", "--symbol", missing]);
    assert_eq!(locate["resolution"]["state"], "not_found");
    assert_eq!(locate["returned"], 0);
    for report in [&ask, &grep, &locate] {
        for action in actions(report) {
            let replayed = fixture.replay(action);
            match action["tool"].as_str().unwrap() {
                "tree" => assert!(replayed["outline"].as_str().unwrap().contains("sample")),
                "grep" => assert_eq!(replayed["total"], 0),
                unexpected => panic!("unexpected recovery tool: {unexpected}"),
            }
        }
    }
}

#[test]
fn regex_shaped_unresolved_target_replays_as_a_literal_source_search() {
    let fixture = Fixture::new();
    let target = "missing.*symbol|other\\name";
    fixture.write(
        "sample.rs",
        &format!("// {target}\n// missingZZsymbol\n// other\\name\nfn sample() {{}}\n"),
    );
    fixture.generate();
    let error = fixture.error(&["asm", "--from", target]);
    assert_eq!(error["error"]["code"], "anchor_not_found");
    let action = &actions(&error)[0];
    assert_eq!(action["tool"], "grep");
    assert_eq!(action["arguments"]["regex"], true);
    let report = fixture.replay(action);
    assert_eq!(
        report["total"], 1,
        "regex recovery widened the literal search: {report}"
    );
    assert_eq!(report["matches"][0]["text"], format!("// {target}"));
    assert_eq!(report["matches"][0]["file"], "sample.rs");
}

#[test]
fn truncated_tree_actions_narrow_to_subtrees_with_symbols() {
    let fixture = Fixture::new();
    // These entries sort before every code scope. A file map's continuation
    // must be selected from symbol-bearing source, not the unfiltered file list.
    fixture.write(
        ".devcontainer/devcontainer.json",
        "{\"name\":\"development\"}\n",
    );
    fixture.write(".github/workflows/ci.yml", "name: CI\non: push\n");
    fixture.write(".pre-commit-config.yaml", "repos: []\n");
    fixture.write(
        "00_docs/guide.adoc",
        "[[guide]]\n= Guide\n\nProject setup.\n",
    );
    for dir in ["alpha", "beta", "gamma"] {
        let content: String = (0..270)
            .map(|number| format!("fn {dir}_{number:04}() {{}}\n"))
            .collect();
        fixture.write(&format!("{dir}/chunk.rs"), &content);
    }
    fixture.generate();
    let report = fixture.json(&["tree"]);
    assert_eq!(report["truncated"], true);
    assert_eq!(report["format"], "file-map-v1");
    assert!(report["symbol_count"].as_u64().unwrap() >= 810);
    assert_eq!(report["file_count"], 3);
    assert_eq!(actions(&report).len(), 3);
    for action in actions(&report) {
        assert_eq!(action["tool"], "tree");
        let scope = Path::new(action["arguments"]["path"].as_str().unwrap());
        assert!(
            ["alpha", "beta", "gamma"].contains(&scope.file_name().unwrap().to_str().unwrap()),
            "file-map action must reveal actual symbols: {action}"
        );
        assert_ne!(
            std::fs::canonicalize(scope).unwrap(),
            std::fs::canonicalize(&fixture.project).unwrap()
        );
        let narrowed = fixture.replay(action);
        assert_eq!(narrowed["truncated"], false);
        assert!(narrowed["returned_symbol_count"].as_u64().unwrap() >= 270);
        assert_eq!(narrowed["file_count"], 1);
    }
}

#[test]
fn query_direction_metadata_matches_actual_call_relationships() {
    let fixture = Fixture::new();
    fixture.write(
        "calls.rs",
        "fn caller() { target(); }\nfn target() { dependency(); }\nfn dependency() {}\n",
    );
    fixture.generate();
    for (flag, direction, label, present, absent) in [
        (
            "--backlinks",
            "incoming",
            "Used by / potentially affected",
            "#caller",
            "#dependency",
        ),
        (
            "--impact",
            "outgoing",
            "Depends on",
            "#dependency",
            "#caller",
        ),
    ] {
        let report = fixture.json(&["query", flag, "target", "--depth", "1"]);
        assert_eq!(report["relationship"]["direction"], direction);
        assert_eq!(report["relationship"]["label"], label);
        let anchors: Vec<_> = report["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["anchor"].as_str())
            .collect();
        assert!(
            anchors.iter().any(|anchor| anchor.ends_with(present)),
            "{report}"
        );
        assert!(
            !anchors.iter().any(|anchor| anchor.ends_with(absent)),
            "{report}"
        );
        let human = fixture.scoped(&[
            "--human", "query", "--format", "table", flag, "target", "--depth", "1",
        ]);
        assert!(
            human.status.success(),
            "{}",
            String::from_utf8_lossy(&human.stderr)
        );
        assert!(String::from_utf8_lossy(&human.stdout).contains(&format!("{label} ({direction})")));
    }
}
