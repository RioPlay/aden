// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Propagate optional build-identity env vars into the compiler environment.
//!
//! `option_env!("ADEN_BUILD_*")` alone does not always recompile when CI sets
//! those vars on a warm Cargo cache. `cargo:rerun-if-env-changed` +
//! `cargo:rustc-env` make release archives report `Build: <sha> (release)`
//! instead of the local default `dev (source-tree)`.

fn main() {
    println!("cargo:rerun-if-env-changed=ADEN_BUILD_REVISION");
    println!("cargo:rerun-if-env-changed=ADEN_BUILD_STATE");
    if let Ok(revision) = std::env::var("ADEN_BUILD_REVISION") {
        // Keep the banner short and stable (git short SHA style).
        let short = if revision.len() > 12 {
            &revision[..12]
        } else {
            &revision
        };
        println!("cargo:rustc-env=ADEN_BUILD_REVISION={short}");
    }
    if let Ok(state) = std::env::var("ADEN_BUILD_STATE") {
        println!("cargo:rustc-env=ADEN_BUILD_STATE={state}");
    }
}
