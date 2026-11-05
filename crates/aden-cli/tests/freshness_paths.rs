// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn run(root: &Path, data: &Path, pinned: Option<&Path>, args: &[&str]) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aden"));
    command
        .args(args)
        .current_dir(root)
        .env("ADEN_DATA_DIR", data)
        .env_remove("ADEN_SKIP_AUTO_GEN")
        .env_remove("ADEN_STORE");
    if let Some(pinned) = pinned {
        command.env("ADEN_STORE", pinned);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn project(root: &Path, symbol: &str) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='path-fixture'\nversion='0.1.0'\n",
    )
    .unwrap();
    std::fs::write(root.join("src/lib.rs"), format!("pub fn {symbol}() {{}}\n")).unwrap();
}

#[test]
fn nested_scopes_keep_distinct_checkouts_isolated() {
    let fixture = tempfile::tempdir().unwrap();
    let data = fixture.path().join("data");
    for (directory, own, other) in [
        ("a", "checkout_a", "checkout_b"),
        ("b", "checkout_b", "checkout_a"),
    ] {
        let root = fixture.path().join(directory);
        project(&root, own);
        let found = run(
            &root.join("src"),
            &data,
            None,
            &["locate", "--symbol", own, "--json"],
        );
        assert_eq!(found["total"], 1);
        assert_eq!(found["match_kind"], "definition");
        assert_eq!(found["context_receipt"]["freshness"], "current");
        let absent = run(&root, &data, None, &["locate", "--symbol", other, "--json"]);
        // Similar names can produce fuzzy suggestions; they must never resolve
        // as the other checkout's definition.
        assert_ne!(absent["match_kind"], "definition", "{absent}");
    }
}

#[test]
fn missing_pinned_store_falls_back_to_legacy_without_refreshing() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("repo");
    let data = fixture.path().join("data");
    project(&root, "legacy_symbol");
    let initial = run(
        &root,
        &data,
        None,
        &["locate", "--symbol", "legacy_symbol", "--json"],
    );
    assert_eq!(initial["total"], 1);
    let projects = std::fs::read_dir(data.join("projects")).unwrap();
    let project_dir = projects.into_iter().next().unwrap().unwrap().path();
    let legacy = root.join(".aden/store");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::rename(project_dir.join("store"), &legacy).unwrap();
    let missing = fixture.path().join("missing-pinned-store");
    let observed = run(
        &root.join("src"),
        &data,
        Some(&missing),
        &["locate", "--symbol", "legacy_symbol", "--json"],
    );
    assert_eq!(observed["total"], 1);
    assert_eq!(observed["context_receipt"]["freshness"], "current");
    assert_eq!(observed["context_receipt"]["refresh_cause"], "none");
    assert!(!missing.exists());
    assert!(legacy.is_dir());
}

#[test]
fn existing_central_and_pinned_stores_take_precedence() {
    let fixture = tempfile::tempdir().unwrap();
    for pinned in [false, true] {
        let case = fixture
            .path()
            .join(if pinned { "pinned" } else { "central" });
        let root = case.join("repo");
        let data = case.join("data");
        let pin = case.join("pinned-store");
        let override_path = pinned.then_some(pin.as_path());
        project(&root, "preferred_symbol");
        run(
            &root,
            &data,
            override_path,
            &["locate", "--symbol", "preferred_symbol", "--json"],
        );
        let legacy = root.join(".aden/store");
        std::fs::create_dir_all(&legacy).unwrap();
        let central = std::fs::read_dir(data.join("projects"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("store");
        if pinned {
            std::fs::create_dir_all(&central).unwrap();
        }
        let observed = run(
            &root.join("src"),
            &data,
            override_path,
            &["locate", "--symbol", "preferred_symbol", "--json"],
        );
        assert_eq!(observed["total"], 1);
        assert_eq!(observed["context_receipt"]["freshness"], "current");
        assert_eq!(observed["context_receipt"]["refresh_cause"], "none");
        // The compatibility probe would materialize Fjall files if it opened
        // either lower-priority empty directory.
        assert_eq!(std::fs::read_dir(&legacy).unwrap().count(), 0);
        if pinned {
            assert_eq!(std::fs::read_dir(&central).unwrap().count(), 0);
        }
    }
}
