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

enum StartupAction {
    Serve {
        project_pin: Option<PathBuf>,
        surface: Option<String>,
    },
    Version,
}

/// Parse the intentionally small standalone-server CLI.  This must happen
/// before the stdio transport starts: setup guides and package managers use
/// `aden-mcp --version` as a non-interactive installation smoke test.
fn parse_startup_args(args: impl IntoIterator<Item = String>) -> StartupAction {
    let mut project_pin = None;
    let mut surface = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--version" || arg == "-V" {
            return StartupAction::Version;
        }
        if arg == "--surface" {
            if let Some(level) = args.next() {
                surface = Some(level);
            }
        } else if let Some(level) = arg.strip_prefix("--surface=") {
            surface = Some(level.to_string());
        } else if !arg.starts_with('-') && project_pin.is_none() {
            project_pin = Some(PathBuf::from(arg));
        }
        // Unknown flags are ignored for forward-compatibility.
    }
    StartupAction::Serve {
        project_pin,
        surface,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let StartupAction::Serve {
        project_pin,
        surface,
    } = parse_startup_args(env::args().skip(1))
    else {
        println!("aden-mcp {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    };

    if let Some(surface) = surface {
        set_surface(&surface);
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

#[cfg(test)]
mod tests {
    use super::{StartupAction, parse_startup_args};
    use std::path::PathBuf;

    #[test]
    fn version_is_a_non_interactive_startup_action() {
        assert!(matches!(
            parse_startup_args(["--version".to_string()]),
            StartupAction::Version
        ));
        assert!(matches!(
            parse_startup_args(["-V".to_string()]),
            StartupAction::Version
        ));
    }

    #[test]
    fn surface_and_project_pin_remain_supported() {
        let StartupAction::Serve {
            project_pin,
            surface,
        } = parse_startup_args(["--surface=standard".to_string(), "/tmp/project".to_string()])
        else {
            panic!("ordinary server arguments must serve");
        };
        assert_eq!(surface.as_deref(), Some("standard"));
        assert_eq!(project_pin, Some(PathBuf::from("/tmp/project")));
    }
}
