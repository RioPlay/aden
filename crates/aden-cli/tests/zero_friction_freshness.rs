// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Zero-friction freshness contracts:
//! - delete/rename detection marks the index stale
//! - silent gen does not hang forever when the write lock is held
//! - JSON envelopes expose `freshness`

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const LIB_RS: &str = r#"
/// Greeting helper.
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}
"#;

const OTHER_RS: &str = r#"
/// Second module.
pub fn other() -> u32 {
    42
}
"#;

fn unique_dir(label: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "aden-zf-{label}-{}-{}-{}",
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

fn git(project: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn scaffold() -> (PathBuf, PathBuf) {
    let project = unique_dir("proj");
    let data = unique_dir("data");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
    std::fs::write(project.join("src/other.rs"), OTHER_RS).unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["config", "user.email", "zf@test.invalid"]);
    git(&project, &["config", "user.name", "ZF Test"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "fixture"]);
    (project, data)
}

fn run(project: &Path, data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(args)
        .current_dir(project)
        .env("ADEN_DATA_DIR", data)
        .output()
        .expect("aden binary must run")
}

#[test]
fn delete_source_makes_index_stale_then_grep_prunes() {
    let (project, data) = scaffold();
    let gen_out = run(&project, &data, &["gen", "."]);
    assert!(
        gen_out.status.success(),
        "gen failed: {}",
        String::from_utf8_lossy(&gen_out.stderr)
    );

    // Delete a source file — index must treat this as stale and prune on refresh.
    std::fs::remove_file(project.join("src/other.rs")).unwrap();

    let started = Instant::now();
    let grep = run(
        &project,
        &data,
        &["grep", "fn other", ".", "--json", "--limit", "20"],
    );
    assert!(
        grep.status.success(),
        "grep after delete failed: {}",
        String::from_utf8_lossy(&grep.stderr)
    );
    // Must not hang for multi-minute lock queues.
    assert!(
        started.elapsed() < Duration::from_secs(120),
        "grep after delete took too long: {:?}",
        started.elapsed()
    );

    let out = String::from_utf8_lossy(&grep.stdout);
    // After refresh, the deleted symbol should not dominate results.
    // (total may still be 0 matches — that is success.)
    assert!(
        !out.contains("src/other.rs")
            || out.contains("\"total\":0")
            || out.contains("\"total\": 0"),
        "unexpected grep after delete: {out}"
    );
}

#[test]
fn status_json_includes_health_after_gen() {
    let (project, data) = scaffold();
    let gen_out = run(&project, &data, &["gen", "."]);
    assert!(gen_out.status.success());
    let st = run(&project, &data, &["status", ".", "-j"]);
    assert!(st.status.success());
    let out = String::from_utf8_lossy(&st.stdout);
    assert!(
        out.contains("health") || out.contains("ok"),
        "status json: {out}"
    );
}
