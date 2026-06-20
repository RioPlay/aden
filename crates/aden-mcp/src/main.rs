// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! MCP server entry point for Aden.
//!
//! Usage:
//!   aden-mcp [/path/to/project] [--surface essential|standard|full]
//!
//! `--surface` (or `--surface=LEVEL`) selects which tools `list_tools` advertises
//! by exporting `ADEN_MCP_SURFACE` before serving — so the MCP client config can
//! pin the surface via args (cross-platform) without relying on an `env` block.
//!
//! Communicates via JSON-RPC over stdio per the MCP specification,
//! using the official `rmcp` Rust SDK.

use std::env;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut project_dir: Option<PathBuf> = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--surface" {
            if let Some(level) = args.next() {
                set_surface(&level);
            }
        } else if let Some(level) = arg.strip_prefix("--surface=") {
            set_surface(level);
        } else if !arg.starts_with('-') && project_dir.is_none() {
            project_dir = Some(PathBuf::from(arg));
        }
        // Unknown flags are ignored for forward-compatibility.
    }
    let project_dir =
        project_dir.unwrap_or_else(|| env::current_dir().expect("cannot get current directory"));

    aden_mcp::serve(project_dir).await
}

/// Export the requested surface so `requested_surface()` picks it up. Done once at
/// startup before any worker thread is spawned, so the `set_var` is sound.
fn set_surface(level: &str) {
    // SAFETY: single-threaded process startup; nothing else reads the env yet.
    unsafe { env::set_var("ADEN_MCP_SURFACE", level) };
}
