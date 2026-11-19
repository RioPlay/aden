// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_store::{GraphStorage, Storage};
use serde_json::Value;
use std::process::{Command, Output};

struct Fixture {
    root: tempfile::TempDir,
    data: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            root: tempfile::tempdir().unwrap(),
            data: tempfile::tempdir().unwrap(),
        };
        std::fs::write(
            fixture.root.path().join("Cargo.toml"),
            "[package]\nname='bounded-fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        for i in 0..66 {
            fixture.write(i, &format!("member_{i:02}"));
        }
        fixture
    }

    fn write(&self, file: usize, symbol: &str) {
        self.write_source(file, &format!("pub fn {symbol}() {{}}\n"));
    }

    fn write_source(&self, file: usize, source: &str) {
        std::fs::write(self.root.path().join(format!("f{file:02}.rs")), source).unwrap();
    }

    fn run_gen(&self, fail_after: Option<&str>) -> Output {
        self.run_gen_path(".", fail_after)
    }

    fn run_gen_path(&self, path: &str, fail_after: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aden"));
        command
            .args(["gen", path, "--quiet"])
            .current_dir(self.root.path())
            .env("ADEN_DATA_DIR", self.data.path())
            .env("ADEN_GEN_TRACE", "1")
            .env_remove("ADEN_STORE")
            .env_remove("ADEN_SKIP_AUTO_GEN")
            .env_remove("ADEN_TEST_FAIL_AFTER_FILE_WRITE");
        if let Some(path) = fail_after {
            command.env("ADEN_TEST_FAIL_AFTER_FILE_WRITE", path);
        }
        command.output().unwrap()
    }

    fn read_json(&self, args: &[&str], skip_auto_gen: bool) -> Value {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aden"));
        command
            .args(args)
            .arg("--json")
            .current_dir(self.root.path())
            .env("ADEN_DATA_DIR", self.data.path())
            .env_remove("ADEN_STORE")
            .env_remove("ADEN_SKIP_AUTO_GEN")
            .env_remove("ADEN_TEST_FAIL_AFTER_FILE_WRITE");
        if skip_auto_gen {
            command.env("ADEN_SKIP_AUTO_GEN", "1");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn anchors(&self) -> Vec<String> {
        let project = std::fs::read_dir(self.data.path().join("projects"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let storage = Storage::open_existing(project.join("store").to_str().unwrap()).unwrap();
        storage.get_all_documents().unwrap().into_keys().collect()
    }

    fn edges(&self) -> std::collections::BTreeSet<(String, String, String)> {
        let project = std::fs::read_dir(self.data.path().join("projects"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let storage = Storage::open_existing(project.join("store").to_str().unwrap()).unwrap();
        storage
            .get_all_edges()
            .unwrap()
            .into_iter()
            .map(|(source, target, kind)| (source, target, format!("{kind:?}")))
            .collect()
    }
}

fn report(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|value| value["type"] == "aden_gen_trace")
        .expect("generation trace")
}

fn has(anchors: &[String], symbol: &str) -> bool {
    anchors
        .iter()
        .any(|anchor| anchor.ends_with(&format!("#{symbol}")))
}

fn anchor(anchors: &[String], symbol: &str) -> String {
    anchors
        .iter()
        .find(|anchor| anchor.ends_with(&format!("#{symbol}")))
        .unwrap_or_else(|| panic!("missing symbol {symbol}"))
        .clone()
}

#[test]
fn changed_caller_relinks_edges_without_losing_untouched_calls_or_type_uses() {
    fn populated_fixture() -> Fixture {
        let fixture = Fixture::new();
        fixture.write_source(
            0,
            "pub struct Marker;\npub fn bar(_: Marker) {}\npub fn foo(_: Marker) { bar(Marker); }\n",
        );
        fixture.write_source(1, "pub fn untouched() { bar(crate::Marker); }\n");
        fixture.write_source(2, "#[test]\nfn checks_bar() { bar(crate::Marker); }\n");
        fixture
    }

    let fixture = populated_fixture();
    report(&fixture.run_gen(None));
    let initial_anchors = fixture.anchors();
    let foo = anchor(&initial_anchors, "foo");
    let bar = anchor(&initial_anchors, "bar");
    let untouched = anchor(&initial_anchors, "untouched");
    let checks_bar = anchor(&initial_anchors, "checks_bar");
    let marker = anchor(&initial_anchors, "Marker");
    let initial_edges = fixture.edges();
    assert!(initial_edges.contains(&(foo.clone(), bar.clone(), "Calls".into())));

    fixture.write_source(
        0,
        "pub struct Marker;\npub fn bar(_: Marker) {}\npub fn foo(_: Marker) {}\n",
    );
    report(&fixture.run_gen(None));

    let edges = fixture.edges();
    assert!(!edges.contains(&(foo.clone(), bar.clone(), "Calls".into())));
    assert!(edges.contains(&(untouched.clone(), bar.clone(), "Calls".into())));
    assert!(edges.contains(&(checks_bar.clone(), bar.clone(), "Calls".into())));
    assert!(edges.contains(&(checks_bar.clone(), bar.clone(), "Tests".into())));
    assert!(edges.contains(&(foo.clone(), marker.clone(), "Uses".into())));

    let clean = populated_fixture();
    clean.write_source(
        0,
        "pub struct Marker;\npub fn bar(_: Marker) {}\npub fn foo(_: Marker) {}\n",
    );
    report(&clean.run_gen(None));
    assert_eq!(
        edges,
        clean.edges(),
        "incremental graph must match clean generation"
    );
}

#[test]
fn bounded_generation_drains_batches_and_preserves_warm_and_deleted_sources() {
    let fixture = Fixture::new();
    let cold = report(&fixture.run_gen(None));
    assert_eq!(cold["counts"]["parse_batches"], 3);
    assert_eq!(cold["counts"]["recovery_journal_writes"], 3);
    assert_eq!(cold["counts"]["peak_parse_batch_files"], 32);
    assert!(
        cold["counts"]["peak_parse_batch_snapshot_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    let anchors = fixture.anchors();
    for i in 0..66 {
        assert!(
            has(&anchors, &format!("member_{i:02}")),
            "missing source {i}"
        );
    }
    let warm = report(&fixture.run_gen(None));
    assert_eq!(warm["counts"]["reindexed_files"].as_u64().unwrap_or(0), 0);
    assert_eq!(warm["counts"]["peak_parse_batch_documents"], 0);
    assert_eq!(warm["counts"]["recovery_journal_writes"], 0);
    fixture.write(0, "edited_first_batch");
    let edited = report(&fixture.run_gen(None));
    assert_eq!(edited["counts"]["reindexed_files"], 1);
    assert_eq!(edited["counts"]["recovery_journal_writes"], 1);
    std::fs::remove_file(fixture.root.path().join("f01.rs")).unwrap();
    fixture.write(65, "replacement_last_batch");
    report(&fixture.run_gen(None));
    let anchors = fixture.anchors();
    assert!(!has(&anchors, "member_01"));
    assert!(!has(&anchors, "member_65"));
    assert!(has(&anchors, "replacement_last_batch"));
    assert!(has(&anchors, "edited_first_batch"));
}

#[test]
fn deleted_sources_are_journaled_even_without_a_parse_batch() {
    let fixture = Fixture::new();
    report(&fixture.run_gen(None));
    for i in 0..66 {
        std::fs::remove_file(fixture.root.path().join(format!("f{i:02}.rs"))).unwrap();
    }
    std::fs::remove_file(fixture.root.path().join("Cargo.toml")).unwrap();
    let preview = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["gen", ".", "--propose", "--quiet"])
        .current_dir(fixture.root.path())
        .env("ADEN_DATA_DIR", fixture.data.path())
        .env_remove("ADEN_STORE")
        .output()
        .unwrap();
    assert!(preview.status.success());
    assert!(has(&fixture.anchors(), "member_65"));
    let deleted = report(&fixture.run_gen(None));
    assert_eq!(deleted["counts"]["parse_batches"], 0);
    assert_eq!(deleted["counts"]["recovery_journal_writes"], 1);
    assert_eq!(deleted["counts"]["pruned_symbols"], 66);
    assert_eq!(deleted["counts"]["freshness_manifest_published"], 1);
    assert!(!has(&fixture.anchors(), "member_65"));
}

#[cfg(debug_assertions)]
#[test]
fn empty_project_recovers_an_interrupted_first_generation() {
    let fixture = Fixture {
        root: tempfile::tempdir().unwrap(),
        data: tempfile::tempdir().unwrap(),
    };
    assert!(fixture.run_gen(None).status.success());
    assert!(!fixture.data.path().join("projects").exists());
    fixture.write(0, "interrupted_only_symbol");
    let interrupted = fixture.run_gen(Some("f00.rs"));
    assert!(!interrupted.status.success());
    assert!(has(&fixture.anchors(), "interrupted_only_symbol"));
    std::fs::remove_file(fixture.root.path().join("f00.rs")).unwrap();
    let recovered = report(&fixture.run_gen(None));
    assert_eq!(recovered["counts"]["pruned_symbols"], 1);
    assert_eq!(recovered["counts"]["freshness_manifest_published"], 1);
    assert!(!has(&fixture.anchors(), "interrupted_only_symbol"));
}

#[test]
fn empty_project_preserves_colliding_symbols_in_a_shared_store() {
    let lab = tempfile::tempdir().unwrap();
    let store = lab.path().join("shared");
    let data = lab.path().join("data");
    let a = lab.path().join("a");
    let b = lab.path().join("b");
    let generate = |root: &std::path::Path| {
        let output = Command::new(env!("CARGO_BIN_EXE_aden"))
            .args(["gen", ".", "--quiet"])
            .current_dir(root)
            .env("ADEN_STORE", &store)
            .env("ADEN_DATA_DIR", &data)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    for root in [&a, &b] {
        std::fs::create_dir(root).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='shared-crate'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(
            root.join("lib.rs"),
            "pub fn shared() {}\npub fn caller() { shared(); }",
        )
        .unwrap();
        generate(root);
    }
    let read_store = || {
        let storage = Storage::open_existing(store.to_str().unwrap()).unwrap();
        (
            storage.get_all_anchors().unwrap(),
            storage.get_all_edges().unwrap(),
        )
    };
    let (anchors, edges) = read_store();
    assert!(edges.iter().any(|(src, dst, kind)| src.ends_with("#caller")
        && dst.ends_with("#shared")
        && *kind == aden_core::EdgeType::Calls));
    std::fs::remove_file(a.join("lib.rs")).unwrap();
    std::fs::remove_file(a.join("Cargo.toml")).unwrap();
    generate(&a);
    let (after, after_edges) = read_store();
    assert_eq!(after, anchors);
    assert_eq!(after_edges, edges);
}

#[cfg(debug_assertions)]
#[test]
fn failure_in_first_batch_recovers_without_losing_later_batch_owners() {
    let fixture = Fixture::new();
    report(&fixture.run_gen(None));
    fixture.write(0, "replacement_first_batch");
    fixture.write(65, "replacement_last_batch");
    let failed = fixture.run_gen(Some("f00.rs"));
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("injected failure"));
    report(&fixture.run_gen(None));
    let anchors = fixture.anchors();
    assert!(!has(&anchors, "member_00"));
    assert!(!has(&anchors, "member_65"));
    assert!(has(&anchors, "replacement_first_batch"));
    assert!(has(&anchors, "replacement_last_batch"));
    assert!(has(&anchors, "member_32"));
    assert!(has(&anchors, "member_64"));
}

#[cfg(debug_assertions)]
#[test]
fn repeated_interruption_retains_later_batch_recovery_after_source_reverts() {
    let fixture = Fixture::new();
    report(&fixture.run_gen(None));
    fixture.write(32, "interrupted_later_batch");
    let failed = fixture.run_gen(Some("f32.rs"));
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("injected failure"));
    assert!(has(&fixture.anchors(), "interrupted_later_batch"));

    // Reverting to the last persisted digest must not cause the third attempt
    // to skip this source after an earlier batch fails on the second attempt.
    fixture.write(32, "member_32");
    fixture.write(0, "interrupted_first_batch");
    let failed = fixture.run_gen(Some("f00.rs"));
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("injected failure"));
    report(&fixture.run_gen(None));
    let anchors = fixture.anchors();
    assert!(!has(&anchors, "interrupted_later_batch"));
    assert!(has(&anchors, "member_32"));
    assert!(has(&anchors, "interrupted_first_batch"));
    assert!(has(&anchors, "member_65"));
}

#[cfg(debug_assertions)]
#[test]
fn single_file_request_reconciles_other_interrupted_owners_before_certifying_root() {
    let fixture = Fixture::new();
    report(&fixture.run_gen(None));
    fixture.write(32, "transient_other_owner");
    let failed = fixture.run_gen(Some("f32.rs"));
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("injected failure"));
    assert!(has(&fixture.anchors(), "transient_other_owner"));
    fixture.write(32, "member_32");
    std::fs::remove_file(fixture.root.path().join("f33.rs")).unwrap();
    let recovered = report(&fixture.run_gen_path("f00.rs", None));
    // The 65 remaining Rust sources and Cargo.toml are all discoverable inputs.
    assert_eq!(recovered["counts"]["source_files"], 66);
    assert_eq!(recovered["counts"]["parse_batches"], 3);
    assert_eq!(recovered["counts"]["freshness_manifest_published"], 1);
    assert_eq!(
        std::fs::read_dir(fixture.data.path().join("projects"))
            .unwrap()
            .count(),
        1,
        "bare filename and directory requests must use the same project store"
    );
    let anchors = fixture.anchors();
    assert!(!has(&anchors, "transient_other_owner"));
    assert!(has(&anchors, "member_32"));
    assert!(!has(&anchors, "member_33"));
    let read = fixture.read_json(&["search", "member_32", "."], true);
    assert_eq!(read["context_receipt"]["freshness"], "current");
    assert!(read.to_string().contains("#member_32"));
}

#[test]
fn ordinary_single_file_request_does_not_certify_other_changed_sources() {
    let fixture = Fixture::new();
    report(&fixture.run_gen(None));
    fixture.write(32, "changed_outside_partial_scope");
    let partial = report(&fixture.run_gen_path("f00.rs", None));
    assert_eq!(partial["counts"]["source_files"], 1);
    assert_eq!(partial["counts"]["freshness_manifest_published"], 0);
    let anchors = fixture.anchors();
    assert!(has(&anchors, "member_32"));
    assert!(!has(&anchors, "changed_outside_partial_scope"));
    let search = ["search", "changed_outside_partial_scope", "."];
    let first = fixture.read_json(&search, false);
    // Explore may serve the old snapshot while its background refresh finishes.
    // Wait for that job before searching again so a second job cannot mask a failure.
    match first["context_receipt"]["freshness"].as_str() {
        Some("lagging") => assert_eq!(first["index_stale"], true),
        Some("current") => assert_eq!(first["index_stale"], false),
        _ => panic!("Unexpected initial freshness: {first}"),
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let job = fixture.read_json(&["jobs", "status", "."], true);
        match job["state"].as_str() {
            Some("completed") => break,
            Some("queued" | "running") => assert!(std::time::Instant::now() < deadline, "{job}"),
            _ => panic!("Refresh did not complete: {job}"),
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let read = fixture.read_json(&search, true);
    assert_eq!(read["context_receipt"]["freshness"], "current");
    assert!(read.to_string().contains("#changed_outside_partial_scope"));
    let anchors = fixture.anchors();
    assert!(!has(&anchors, "member_32"));
    assert!(has(&anchors, "changed_outside_partial_scope"));
}

#[test]
fn incremental_generation_preserves_edges_for_a_partial_shared_caller_anchor() {
    let fixture = Fixture::new();
    for i in 0..66 {
        std::fs::remove_file(fixture.root.path().join(format!("f{i:02}.rs"))).unwrap();
    }
    for (dir, helper) in [("a", "AOnly"), ("z", "ZOnly")] {
        let module = fixture.root.path().join(dir);
        std::fs::create_dir(&module).unwrap();
        std::fs::write(
            module.join("go.mod"),
            "module example.invalid/collision\n\ngo 1.22\n",
        )
        .unwrap();
        std::fs::write(
            module.join("shared.go"),
            format!("package example\nfunc Caller() {{ {helper}() }}\nfunc {helper}() {{}}\n"),
        )
        .unwrap();
    }

    report(&fixture.run_gen(None));
    let anchors = fixture.anchors();
    let caller = anchor(&anchors, "Caller");
    let a_only = anchor(&anchors, "AOnly");
    let z_only = anchor(&anchors, "ZOnly");
    assert_eq!(
        anchors
            .iter()
            .filter(|item| item.ends_with("#Caller"))
            .count(),
        1,
        "fixture must produce one shared caller anchor"
    );
    let initial_edges = fixture.edges();
    assert!(initial_edges.contains(&(caller.clone(), a_only.clone(), "Calls".into())));
    assert!(initial_edges.contains(&(caller.clone(), z_only.clone(), "Calls".into())));

    let a_source = fixture.root.path().join("a/shared.go");
    let mut updated = std::fs::read_to_string(&a_source).unwrap();
    updated.insert_str(0, "// unrelated comment change\n");
    std::fs::write(a_source, updated).unwrap();
    report(&fixture.run_gen(None));

    let edges = fixture.edges();
    assert!(edges.contains(&(caller.clone(), a_only, "Calls".into())));
    assert!(edges.contains(&(caller.clone(), z_only.clone(), "Calls".into())));

    std::fs::remove_file(fixture.root.path().join("a/shared.go")).unwrap();
    report(&fixture.run_gen(None));
    assert!(fixture.anchors().contains(&caller));
    assert!(fixture.edges().contains(&(caller, z_only, "Calls".into())));
}

#[test]
fn overlay_collision_notices_match_across_batch_boundaries() {
    fn notices(filler_files: usize) -> (String, String) {
        let fixture = Fixture::new();
        for i in filler_files..66 {
            std::fs::remove_file(fixture.root.path().join(format!("f{i:02}.rs"))).unwrap();
        }
        for dir in ["a", "z"] {
            std::fs::create_dir(fixture.root.path().join(dir)).unwrap();
            // Both nested modules deliberately have the same module identity
            // and module-relative source path, creating a real anchor collision.
            std::fs::write(
                fixture.root.path().join(dir).join("go.mod"),
                "module example.invalid/collision\n\ngo 1.22\n",
            )
            .unwrap();
            std::fs::write(
                fixture.root.path().join(dir).join("shared.go"),
                "package example\n// Collision returns the original integer value.\nfunc Collision() int { return 1 }\n",
            )
            .unwrap();
        }
        let collision_document = |dir: &str| {
            let path = fixture.root.path().join(dir).join("shared.go");
            let source = std::fs::read_to_string(&path).unwrap();
            aden_parse::parse_file(&path, &source)
                .unwrap()
                .into_iter()
                .find(|doc| doc.anchor.ends_with("#Collision"))
                .expect("fixture collision symbol")
        };
        let original = collision_document("a");
        assert_eq!(original.anchor, collision_document("z").anchor);
        report(&fixture.run_gen(None));
        let anchors = fixture.anchors();
        let collision: Vec<_> = anchors
            .iter()
            .filter(|a| a.ends_with("#Collision"))
            .collect();
        assert_eq!(
            collision.len(),
            1,
            "fixture must exercise an anchor collision"
        );
        let anchor = collision[0];
        let slug: String = anchor
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let overlays = fixture.root.path().join(".aden/overlays");
        std::fs::create_dir_all(&overlays).unwrap();
        std::fs::write(
            overlays.join(format!("{slug}.adoc")),
            format!(":anchor: {anchor}\n\n[human#{anchor}]\n----\nPreserve this decision.\n----\n"),
        )
        .unwrap();
        for dir in ["a", "z"] {
            std::fs::write(
                fixture.root.path().join(dir).join("shared.go"),
                "package example\n// Collision returns the updated string value.\nfunc Collision() string { return \"updated\" }\n",
            )
            .unwrap();
        }
        // The overlay guards the first generated region. Source-hash changes
        // alone do not alter that region, so prove this fixture does.
        let original_contract = aden_core::contract::ContractDocument::from_document(&original);
        let updated_contract =
            aden_core::contract::ContractDocument::from_document(&collision_document("a"));
        assert_eq!(
            original_contract.blocks[0].tag.as_deref(),
            Some(anchor.as_str())
        );
        assert_ne!(
            original_contract.blocks[0].content, updated_contract.blocks[0].content,
            "fixture must change the rendered unit guarded by its overlay"
        );
        let output = fixture.run_gen(None);
        let telemetry = report(&output);
        assert_eq!(
            telemetry["counts"]["parse_batches"],
            if filler_files == 0 { 1 } else { 3 }
        );
        let summary = String::from_utf8_lossy(&output.stdout)
            .lines()
            .find(|line| line.contains("annotated symbol(s) changed"))
            .expect("guarded change notice")
            .to_string();
        // Overlay notices contain comment-only review instructions, rather than
        // the metadata required by aden_propose::list's proposal parser.
        let notice = std::fs::read_to_string(
            fixture
                .root
                .path()
                .join(".aden/proposals")
                .join(format!("overlay-review-{slug}.patch.adoc")),
        )
        .expect("persisted overlay notice");
        assert!(notice.starts_with(&format!("// Overlay review notice for {anchor}\n")));
        assert!(notice.contains(&format!(
            "// CHANGED: Generated block '{anchor}' changed but a durable (human/agent) block with the same tag exists in working"
        )));
        assert!(notice.contains("The generated layer was updated and your overlay was preserved."));
        assert!(notice.contains(&format!(".aden/overlays/{slug}.adoc")));
        (summary, notice)
    }
    let same_batch = notices(0);
    let different_batches = notices(66);
    assert!(same_batch.0.starts_with("2 annotated symbol(s) changed"));
    assert_eq!(same_batch, different_batches);
}
