// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! MCP server entry point for Aden.
//!
//! Usage:
//!   aden-mcp /path/to/project
//!
//! Communicates via JSON-RPC over stdio per the MCP specification.

use std::env;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = env::args().collect();
    let project_dir = args.get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().expect("cannot get current directory"));

    // Crash isolation: catch panics from request handlers so a single
    // malformed input or deserializer bug does not kill the MCP server.
    let result = std::panic::catch_unwind(|| {
        if let Err(e) = aden_mcp::serve(&project_dir) {
            eprintln!("aden-mcp: fatal error: {}", e);
            std::process::exit(1);
        }
    });

    if result.is_err() {
        eprintln!("aden-mcp: panic caught. Exiting gracefully.");
        std::process::exit(1);
    }
}
