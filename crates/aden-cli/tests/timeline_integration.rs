// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration test for `aden timeline` — builds a two-commit git repo,
//! runs the binary, and asserts the HTML output contains both versions.

use std::path::{Path, PathBuf};
use std::process::Command;

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn unique_dir(label: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "aden-timeline-test-{label}-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git must be available");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Build the path to the aden-cli binary (in target/debug or target/release).
fn aden_bin() -> PathBuf {
    // CARGO_BIN_EXE_aden is set by cargo test when the test is in the same package.
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_aden") {
        return PathBuf::from(p);
    }
    // Walk up from CARGO_MANIFEST_DIR to workspace root.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let workspace = Path::new(&manifest)
        .ancestors()
        .find(|p| p.join("Cargo.lock").exists())
        .unwrap_or(Path::new(&manifest))
        .to_path_buf();
    // Prefer debug build (fastest in CI).
    let debug = workspace.join("target/debug/aden");
    if debug.exists() {
        return debug;
    }
    workspace.join("target/release/aden")
}

#[test]
fn timeline_html_contains_versions() {
    let dir = unique_dir("versions");

    // Initialize a git repo.
    git(&dir, &["init"]);
    git(&dir, &["config", "user.email", "test@test.com"]);
    git(&dir, &["config", "user.name", "Test User"]);

    // First version.
    let file = dir.join("hello.txt");
    std::fs::write(&file, "version one\n").unwrap();
    git(&dir, &["add", "hello.txt"]);
    git(&dir, &["commit", "-m", "first commit"]);

    // Second version.
    std::fs::write(&file, "version two\n").unwrap();
    git(&dir, &["add", "hello.txt"]);
    git(&dir, &["commit", "-m", "second commit"]);

    // Run `aden timeline hello.txt --no-open --out <tmpfile>` in the repo dir.
    let out_html = dir.join("timeline-test.html");
    let bin = aden_bin();

    // Skip if binary not built yet (avoids CI failures when only running unit tests).
    if !bin.exists() {
        eprintln!(
            "SKIP: aden binary not found at {}; run `cargo build -p aden-cli` first",
            bin.display()
        );
        return;
    }

    let status = Command::new(&bin)
        .args([
            "timeline",
            "hello.txt",
            "--no-open",
            "--out",
            out_html.to_str().unwrap(),
        ])
        .current_dir(&dir)
        .status()
        .expect("failed to run aden timeline");

    assert!(status.success(), "aden timeline exited non-zero");
    assert!(out_html.exists(), "HTML output file was not created");

    let html = std::fs::read_to_string(&out_html).unwrap();

    // The page must contain both versions' content.
    assert!(
        html.contains("version one"),
        "HTML must contain 'version one' from first commit"
    );
    assert!(
        html.contains("version two"),
        "HTML must contain 'version two' from second commit"
    );

    // The page must reference the VERSIONS array.
    assert!(
        html.contains("versions") || html.contains("VERSIONS"),
        "HTML must reference VERSIONS array"
    );

    // Must contain commit subjects.
    assert!(
        html.contains("first commit"),
        "HTML must contain 'first commit' subject"
    );
    assert!(
        html.contains("second commit"),
        "HTML must contain 'second commit' subject"
    );

    // Must contain compare UI elements.
    assert!(html.contains("selBase"), "HTML must contain base selector");
    assert!(
        html.contains("selCompare"),
        "HTML must contain compare selector"
    );
    assert!(
        html.contains("Compare to today"),
        "HTML must contain 'Compare to today' button"
    );
}
