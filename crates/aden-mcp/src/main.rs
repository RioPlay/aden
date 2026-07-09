// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! MCP server entry point for Aden.
//!
//! Usage:
//!   aden-mcp [--surface essential|standard|full] [/optional/pin/project]
//!
//! Prefer **no** project path: the server auto-detects the open workspace via
//! the MCP Roots protocol (and common host env vars). Pass an absolute path only
//! to pin a single project (escape hatch).
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
    let mut project_pin: Option<PathBuf> = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--surface" {
            if let Some(level) = args.next() {
                set_surface(&level);
            }
        } else if let Some(level) = arg.strip_prefix("--surface=") {
            set_surface(level);
        } else if !arg.starts_with('-') && project_pin.is_none() {
            project_pin = Some(PathBuf::from(arg));
        }
        // Unknown flags are ignored for forward-compatibility.
    }

    // Explicit pin via argv or ADEN_PROJECT; otherwise start from cwd and
    // re-resolve from MCP Roots / workspace env on each tool call.
    let pinned = project_pin.is_some() || env::var_os("ADEN_PROJECT").is_some();
    let project_dir = project_pin
        .or_else(|| env::var_os("ADEN_PROJECT").map(PathBuf::from))
        .unwrap_or_else(|| env::current_dir().expect("cannot get current directory"));

    aden_mcp::serve_with_options(project_dir, pinned).await
}

/// Export the requested surface so `requested_surface()` picks it up. Done once at
/// startup before any worker thread is spawned, so the `set_var` is sound.
fn set_surface(level: &str) {
    // SAFETY: single-threaded process startup; nothing else reads the env yet.
    unsafe { env::set_var("ADEN_MCP_SURFACE", level) };
}
