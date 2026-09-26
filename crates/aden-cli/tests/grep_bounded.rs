// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::process::Command;

struct Fixture {
    root: std::path::PathBuf,
    project: std::path::PathBuf,
    data: std::path::PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("aden-grep-{name}-{}", std::process::id()));
        let project = root.join("project");
        let data = root.join("data");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname='grep-fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        Self {
            root,
            project,
            data,
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_aden"))
            .args(args)
            .current_dir(&self.project)
            .env("ADEN_DATA_DIR", &self.data)
            .env("ADEN_SKIP_AUTO_GEN", "1")
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn grep_limit_bounds_retained_results_but_keeps_exact_total() {
    let root = std::env::temp_dir().join(format!("aden-grep-bounded-{}", std::process::id()));
    let data = root.join("data");
    let project = root.join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='grep-bounded'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    let lines = (0..100)
        .map(|i| format!("// bounded_match {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(project.join("src/a.rs"), &lines).unwrap();
    std::fs::write(project.join("src/b.rs"), &lines).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["-j", "grep", "bounded_match", "--limit", "3"])
        .arg(&project)
        .env("ADEN_DATA_DIR", &data)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["total"], 200);
    assert_eq!(value["returned"], 3);
    assert_eq!(value["truncated"], true);
    assert_eq!(value["matches"].as_array().unwrap().len(), 3);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn nonexistent_scope_fails_instead_of_searching_the_project() {
    let fixture = Fixture::new("invalid-scope");
    std::fs::write(fixture.project.join("src/lib.rs"), "fn needle() {}\n").unwrap();
    let output = fixture.run(&["grep", "needle", "src/missing"]);
    assert!(!output.status.success());
    let error = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        error.contains("invalid_scope") || error.contains("cannot search"),
        "{error}"
    );
    assert!(
        !error.contains("\"matches\""),
        "invalid scope must never return project hits: {error}"
    );
}

#[test]
fn minified_unicode_preview_contains_match_and_exact_original_offsets() {
    let fixture = Fixture::new("minified-preview");
    let line = format!("{}Needle{}", "🌱".repeat(100_000), "文".repeat(100_000));
    std::fs::write(fixture.project.join("src/asset.js"), &line).unwrap();
    for extra in ["--ignore-case", "--regex"] {
        let pattern = if extra == "--regex" {
            "N[e]+dle"
        } else {
            "needle"
        };
        let value = fixture.json(&["grep", pattern, "src/asset.js", extra]);
        assert_eq!(value["total"], 1);
        assert_eq!(value["returned"], 1);
        assert_eq!(value["truncated"], false);
        assert_eq!(value["text_truncated"], true);
        let hit = &value["matches"][0];
        assert_eq!(hit["text_truncated"], true);
        let start = hit["text_start_byte"].as_u64().unwrap() as usize;
        let end = hit["text_end_byte"].as_u64().unwrap() as usize;
        assert_eq!(&line[start..end], hit["text"].as_str().unwrap());
        assert!(end - start <= 512);
        assert!(hit["text"].as_str().unwrap().contains("Needle"));
        assert_eq!(hit["match_start_byte"], 400_000);
        assert_eq!(hit["match_end_byte"], 400_006);
        assert_eq!(hit["line_bytes"], line.len());
        assert!(
            value["next_actions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|action| action["tool"] != "understand"),
            "clipped file searches must not cycle back to understand"
        );
    }
}

#[test]
fn serialized_response_cap_preserves_counts_even_when_limit_is_large() {
    let fixture = Fixture::new("response-cap");
    let line = format!("// needle{}\n", "\"\\\t".repeat(200));
    std::fs::write(fixture.project.join("src/lib.rs"), line.repeat(1000)).unwrap();
    let output = fixture.run(&["grep", "needle", "--limit", "10000"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.len() <= 64 * 1024,
        "{} bytes",
        output.stdout.len()
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let returned = value["returned"].as_u64().unwrap();
    assert!(returned > 0 && returned < 1000);
    assert_eq!(value["matches"].as_array().unwrap().len() as u64, returned);
    assert_eq!(value["total"], 1000);
    assert_eq!(value["truncated"], true);
    assert_eq!(value["response_bytes_truncated"], true);
    assert_eq!(value["limit_truncated"], false);
    let actions = value["next_actions"].as_array().unwrap();
    assert!(!actions.is_empty());
    assert!(
        actions
            .iter()
            .all(|action| action["arguments"]["path"] != ".")
    );
}

#[test]
fn zero_limit_retains_exact_count_and_empty_search_offers_browsing() {
    let fixture = Fixture::new("empty");
    std::fs::write(fixture.project.join("src/lib.rs"), "fn needle() {}\n").unwrap();
    let zero = fixture.json(&["grep", "needle", "--limit", "0"]);
    assert_eq!(zero["total"], 1);
    assert_eq!(zero["returned"], 0);
    assert_eq!(zero["truncated"], true);
    assert_eq!(zero["limit_truncated"], true);
    assert_eq!(zero["response_bytes_truncated"], false);
    let empty = fixture.json(&["grep", "absent", "src/lib.rs"]);
    assert_eq!(empty["total"], 0);
    assert_eq!(empty["truncated"], false);
    assert_eq!(empty["next_actions"][0]["tool"], "tree");
}

#[test]
fn file_and_subtree_scopes_keep_repository_anchor_identity() {
    let fixture = Fixture::new("anchor-scope");
    let initialized = Command::new("git")
        .args(["init", "--quiet"])
        .arg(&fixture.project)
        .output()
        .unwrap();
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    std::fs::create_dir_all(fixture.project.join("nested/src")).unwrap();
    std::fs::write(
        fixture.project.join("nested/Cargo.toml"),
        "[package]\nname='nested'\nversion='0.1.0'\n",
    )
    .unwrap();
    std::fs::write(
        fixture.project.join("nested/src/lib.rs"),
        format!(
            "pub fn scoped_needle() {{}}\npub fn long_body() {{ /* {} */ }}\n",
            "🌱".repeat(300)
        ),
    )
    .unwrap();
    std::fs::write(
        fixture.project.join("src/lib.rs"),
        "pub fn outside_needle() {}\n",
    )
    .unwrap();
    let generated = fixture.run(&["gen", "."]);
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let broad = fixture.json(&["grep", "scoped_needle", "."]);
    let anchor = broad["matches"][0]["anchor"]
        .as_str()
        .expect("indexed function anchor");
    for scope in ["nested", "nested/src/lib.rs"] {
        let scoped = fixture.json(&["grep", "needle", scope, "--symbol-only"]);
        assert_eq!(scoped["total"], 1, "scope {scope}: {scoped}");
        assert_eq!(scoped["matches"][0]["file"], "nested/src/lib.rs");
        assert_eq!(scoped["matches"][0]["anchor"], anchor);
    }
    let clipped = fixture.json(&["grep", "long_body", "nested/src/lib.rs", "--symbol-only"]);
    assert_eq!(clipped["total"], 1);
    assert!(clipped["matches"][0]["anchor"].is_string());
    assert_eq!(clipped["matches"][0]["text_truncated"], true);
    assert_eq!(clipped["next_actions"][0]["tool"], "tree");
    assert!(
        clipped["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|action| action["tool"] != "understand"),
        "a clipped indexed file hit must not cycle back to understand"
    );
}
