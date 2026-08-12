// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Fail if a tuned regression dataset changes without an explicit lock update.
//!
//! Ports `scripts/verify_regression_lock.py` so Lean/Quality CI need no Python
//! for this gate.

use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

#[test]
fn frozen_regression_datasets_match_lock() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root");
    let lock_path = root.join("scripts/regression-lock.json");
    let lock_raw = fs::read_to_string(&lock_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", lock_path.display()));
    let lock: serde_json::Value =
        serde_json::from_str(&lock_raw).expect("regression-lock.json is JSON");
    let files = lock["files"]
        .as_object()
        .expect("regression-lock.json must have a files object");

    let mut failures = Vec::new();
    for (relative, expected_value) in files {
        let expected = expected_value
            .as_str()
            .unwrap_or_else(|| panic!("hash for {relative} must be a string"));
        let path = root.join(relative);
        let actual = if path.is_file() {
            let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            // Locks are computed on the git-canonical LF form. Windows checkouts
            // with core.autocrlf may present CRLF on disk; normalize before hash.
            hex_sha256(&normalize_lf(&bytes))
        } else {
            "missing".to_string()
        };
        if actual != expected {
            failures.push(format!("{relative}: expected {expected}, got {actual}"));
        }
    }

    assert!(
        failures.is_empty(),
        "Regression lock mismatch:\n{}",
        failures.join("\n")
    );
}

fn normalize_lf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                out.push(b'\n');
                i += 2;
            } else {
                out.push(b'\n');
                i += 1;
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}
