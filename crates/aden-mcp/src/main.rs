// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! MCP server entry point for Aden.
//!
//! Usage:
//!   aden-mcp /path/to/project
//!
//! Communicates via JSON-RPC over stdio per the MCP specification,
//! using the official `rmcp` Rust SDK.

use std::env;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();
    let project_dir = args
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().expect("cannot get current directory"));

    aden_mcp::serve(project_dir).await
}
