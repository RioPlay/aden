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
        self.json_with_ray_threads(args, None)
    }

    fn json_with_ray_threads(&self, args: &[&str], ray_threads: Option<&str>) -> serde_json::Value {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aden"));
        command
            .args(args)
            .current_dir(&self.project)
            .env("ADEN_DATA_DIR", &self.data)
            .env("ADEN_SKIP_AUTO_GEN", "1");
        if let Some(threads) = ray_threads {
            command.env("RAYON_NUM_THREADS", threads);
        }
        let output = command.output().unwrap();
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
fn grep_preserves_lines_and_rejects_late_invalid_utf8() {
    let fixture = Fixture::new("streamed");
    let source = fixture.project.join("src/large.rs");
    let mut bytes = "// no match here\r\n".repeat(9000).into_bytes();
    bytes.extend_from_slice("// marker marker\r\n".as_bytes());
    bytes.extend_from_slice("日".repeat(30_000).as_bytes());
    bytes.extend_from_slice("marker\n// marker\r".as_bytes());
    std::fs::write(&source, &bytes).unwrap();

    let result = fixture.json(&["grep", "marker", "src/large.rs"]);
    assert_eq!(result["total"], 3);
    let hits = result["matches"].as_array().unwrap();
    assert_eq!(hits[0]["line"], 9001);
    assert_eq!(hits[0]["text"], "// marker marker");
    assert_eq!(hits[1]["line"], 9002);
    assert_eq!(hits[1]["match_start_byte"], 90_000);
    assert_eq!(hits[2]["line"], 9003);
    assert_eq!(hits[2]["text"], "// marker\r");

    bytes.push(0xff);
    std::fs::write(source, bytes).unwrap();
    let result = fixture.json(&["grep", "marker", "src/large.rs"]);
    assert_eq!(result["total"], 0);
    assert_eq!(result["matches"], serde_json::json!([]));
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
        .env("ADEN_SKIP_AUTO_GEN", "1")
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
            "pub fn scoped_needle() {{}}\npub fn use_scoped() {{\n scoped_needle();\n scoped_needle();\n}}\npub fn long_body() {{ /* {} */ }}\n",
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
        assert_eq!(scoped["total"], 3, "scope {scope}: {scoped}");
        assert_eq!(scoped["matches"][0]["file"], "nested/src/lib.rs");
        assert_eq!(scoped["matches"][0]["anchor"], anchor);
    }
    let limited = fixture.json(&["grep", "needle", "nested", "--symbol-only", "--limit", "1"]);
    assert_eq!(limited["total"], 3);
    assert_eq!(limited["returned"], 1);
    assert_eq!(limited["matches"].as_array().unwrap().len(), 1);

    let clipped = fixture.json(&["grep", "long_body", "nested/src/lib.rs", "--symbol-only"]);
    assert_eq!(clipped["total"], 1);
    assert!(clipped["matches"][0]["anchor"].is_string());
    assert_eq!(clipped["matches"][0]["text_truncated"], true);
    assert_eq!(clipped["next_actions"][0]["tool"], "locate");
    assert!(
        clipped["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|action| action["tool"] != "understand"),
        "a clipped indexed file hit must not cycle back to understand"
    );
}

#[test]
fn exact_unique_symbol_hit_offers_caller_query_without_guessing() {
    let fixture = Fixture::new("caller-continuation");
    std::fs::write(
        fixture.project.join("src/lib.rs"),
        "pub fn recallableComposerPrompt() {}\npub fn uses_it() { recallableComposerPrompt(); }\n",
    )
    .unwrap();
    let generated = fixture.run(&["gen", "."]);
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );

    let exact = fixture.json(&["grep", "recallableComposerPrompt"]);
    assert_eq!(exact["total"], 2);
    assert_eq!(exact["matches"][0]["symbol"], "recallableComposerPrompt");
    let actions = exact["next_actions"].as_array().unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0]["tool"], "query");
    assert_eq!(
        actions[0]["arguments"]["backlinks"],
        exact["matches"][0]["anchor"]
    );
    assert_eq!(actions[0]["cli"]["args"][0], "query");
    assert_eq!(actions[0]["arguments"]["depth"], 1);
    assert_eq!(actions[0]["arguments"]["max_results"], 16);
    let human = fixture.run(&["--human", "grep", "recallableComposerPrompt"]);
    assert!(human.status.success());
    let human_stdout = String::from_utf8_lossy(&human.stdout);
    assert!(human_stdout.contains("Find direct callers and references"));
    assert!(!human_stdout.contains("expand a hit into full context"));

    let clue = fixture.json(&["grep", "uses_it() {"]);
    assert_eq!(clue["next_actions"][0]["tool"], "understand");
    assert_eq!(
        clue["next_actions"][0]["arguments"]["symbol"],
        clue["matches"][0]["anchor"]
    );

    std::fs::write(
        fixture.project.join("src/invoke.rs"),
        "fn another() { recallableComposerPrompt(); external_missing(); }\n",
    )
    .unwrap();
    let invoked = fixture.json(&["grep", "recallableComposerPrompt", "src/invoke.rs"]);
    assert_eq!(invoked["next_actions"][0]["tool"], "locate");
    assert_eq!(
        invoked["next_actions"][0]["arguments"]["symbol"],
        "recallableComposerPrompt"
    );
    let unresolved = fixture.json(&["grep", "external_missing", "src/invoke.rs"]);
    let follow = |action: &serde_json::Value| {
        let args: Vec<_> = action["cli"]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap())
            .collect();
        fixture.json(&args)
    };
    let located = follow(&unresolved["next_actions"][0]);
    assert_eq!(located["next_actions"][0]["tool"], "grep");
    let broader = follow(&located["next_actions"][0]);
    assert_eq!(broader["total"], 1);
    assert_ne!(broader["next_actions"][0]["tool"], "locate");
    std::fs::remove_file(fixture.project.join("src/invoke.rs")).unwrap();

    let substring = fixture.json(&["grep", "recallableComposer"]);
    assert_eq!(substring["total"], 2);
    assert!(substring["next_actions"].as_array().unwrap().is_empty());

    let folded = fixture.json(&["grep", "recallablecomposerprompt", "--ignore-case"]);
    assert_eq!(folded["total"], 2);
    assert!(folded["next_actions"].as_array().unwrap().is_empty());

    std::fs::write(
        fixture.project.join("src/other.rs"),
        "pub fn recallableComposerPrompt() {}\n",
    )
    .unwrap();
    let regenerated = fixture.run(&["gen", "."]);
    assert!(
        regenerated.status.success(),
        "{}",
        String::from_utf8_lossy(&regenerated.stderr)
    );
    let ambiguous = fixture.json(&["grep", "recallableComposerPrompt"]);
    assert_eq!(ambiguous["total"], 3);
    let limited = fixture.json(&["grep", "recallableComposerPrompt", "--limit", "1"]);
    assert_eq!(limited["returned"], 1);
    assert_eq!(limited["next_actions"][0]["tool"], "locate");
    assert_eq!(ambiguous["next_actions"][0]["tool"], "locate");
    assert_eq!(
        ambiguous["next_actions"][0]["arguments"]["symbol"],
        "recallableComposerPrompt"
    );
}

#[test]
fn clipped_exact_symbol_suggests_locate_and_punctuation_never_guesses() {
    let fixture = Fixture::new("caller-clipped");
    let long_body = "x".repeat(800);
    std::fs::write(
        fixture.project.join("src/lib.rs"),
        format!("pub fn recallableComposerPrompt() {{ let _ = \"{long_body}\"; }}\n"),
    )
    .unwrap();
    let generated = fixture.run(&["gen", "."]);
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );

    let clipped = fixture.json(&["grep", "recallableComposerPrompt"]);
    assert_eq!(clipped["total"], 1);
    assert_eq!(clipped["matches"][0]["text_truncated"], true);
    assert_eq!(clipped["next_actions"][0]["tool"], "locate");

    for pattern in ["", "recallableComposerPrompt()", "recallable::Composer"] {
        let value = fixture.json(&["grep", pattern]);
        assert!(
            value["next_actions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|action| action["tool"] != "locate" && action["tool"] != "query"),
            "punctuated/empty input must not synthesize a symbol pivot: {pattern:?}"
        );
    }
}

#[test]
fn batched_collection_keeps_global_unicode_path_and_line_order() {
    let fixture = Fixture::new("batch-order");
    for path in [
        "src/\u{03a9}.rs",
        "src/file-0002.rs",
        "src/file-0001.rs",
        "src/file-0000.rs",
    ] {
        std::fs::write(fixture.project.join(path), "chunk_needle\nchunk_needle\n").unwrap();
    }

    // Two worker threads force two file batches. The first batch has fewer
    // matches than the page, so a cutoff must not be applied yet.
    let all = fixture.json_with_ray_threads(&["grep", "chunk_needle", "--limit", "10"], Some("2"));
    assert_eq!(all["total"], 8);
    assert_eq!(all["returned"], 8);
    let all_hits = all["matches"].as_array().unwrap();
    for (index, file) in [
        "src/file-0000.rs",
        "src/file-0001.rs",
        "src/file-0002.rs",
        "src/\u{03a9}.rs",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(all_hits[index * 2]["file"], *file);
        assert_eq!(all_hits[index * 2]["line"], 1);
        assert_eq!(all_hits[index * 2 + 1]["file"], *file);
        assert_eq!(all_hits[index * 2 + 1]["line"], 2);
    }

    let top_three =
        fixture.json_with_ray_threads(&["grep", "chunk_needle", "--limit", "3"], Some("2"));
    assert_eq!(top_three["total"], 8);
    assert_eq!(top_three["returned"], 3);
    assert_eq!(top_three["matches"][0]["file"], "src/file-0000.rs");
    assert_eq!(top_three["matches"][2]["file"], "src/file-0001.rs");
    assert_eq!(top_three["matches"][2]["line"], 1);

    let aliased_scope = fixture.json_with_ray_threads(
        &["grep", "chunk_needle", "src/../src", "--limit", "10"],
        Some("2"),
    );
    assert_eq!(aliased_scope["matches"], all["matches"]);
    assert_eq!(aliased_scope["total"], all["total"]);
}
