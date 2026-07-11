// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end blast-radius for METHOD calls (`self.method()`), the OO-codebase
//! recall gap. A method's callers must appear in its backlinks:
//!
//! * Rust: `self.flush()` inside `impl Engine` → `Engine::flush` backlinks include
//!   `Engine::run` (the parser now emits `self.flush`, the linker re-qualifies it).
//! * Python: `self.validate()` inside `class User` → `User.validate` backlinks
//!   include `User.create`.
//!
//! Runs the freshly-built binary (`CARGO_BIN_EXE_aden`) over a hermetic git
//! fixture with an isolated `ADEN_DATA_DIR`.

use std::path::{Path, PathBuf};
use std::process::Command;

const ENGINE_RS: &str = r#"
pub struct Engine;

impl Engine {
    pub fn run(&self) {
        self.flush();
    }
    pub fn flush(&self) {}
}
"#;

const USER_PY: &str = "class User:\n\
    \x20   def create(self):\n\
    \x20       self.validate()\n\
    \x20   def validate(self):\n\
    \x20       pass\n";

fn unique_dir(label: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "aden-mcblast-{label}-{}-{}-{}",
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
        .expect("git must be available");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn scaffold(file: &str, contents: &str) -> (PathBuf, PathBuf) {
    let project = unique_dir("proj");
    let data = unique_dir("data");
    std::fs::write(project.join(file), contents).unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["config", "user.email", "mc@test.invalid"]);
    git(&project, &["config", "user.name", "MC Test"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "fixture"]);
    (project, data)
}

/// `aden understand <symbol> -j <project>` → backlink anchor strings.
fn understand_backlinks(project: &Path, data: &Path, symbol: &str) -> Vec<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_aden"))
        .args(["understand", symbol, "--json"])
        .arg(project)
        .env("ADEN_DATA_DIR", data)
        .output()
        .expect("aden binary must run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "understand {symbol} failed.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("understand --json must emit JSON ({e}); got: {stdout}"));
    json["backlinks"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|b| {
                    b.as_str()
                        .map(str::to_string)
                        .or_else(|| b["anchor"].as_str().map(str::to_string))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn rust_self_method_caller_appears_in_callee_backlinks() {
    let (project, data) = scaffold("engine.rs", ENGINE_RS);
    let backlinks = understand_backlinks(&project, &data, "Engine::flush");
    assert!(
        backlinks.iter().any(|a| a.ends_with("Engine::run")),
        "Engine::flush backlinks must include its self-method caller Engine::run; got: {backlinks:?}"
    );
}

#[test]
fn python_self_method_caller_appears_in_callee_backlinks() {
    let (project, data) = scaffold("user.py", USER_PY);
    let backlinks = understand_backlinks(&project, &data, "User.validate");
    assert!(
        backlinks.iter().any(|a| a.ends_with("User.create")),
        "User.validate backlinks must include its self-method caller User.create; got: {backlinks:?}"
    );
}
