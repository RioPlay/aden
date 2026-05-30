// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
//! Contract test: every flag the MCP server can emit must be accepted by the
//! `aden` CLI. The MCP layer (`aden-mcp`) translates JSON tool calls into CLI
//! flags and shells out to this binary; if a declared arg drifts from the CLI's
//! clap surface, clap rejects it (or silently ignores it) and the agent's call
//! fails opaquely. This test pins the two together so drift is caught at build
//! time rather than in a live agent session.

use std::process::Command;

/// Global clap args (defined on the top-level `Cli`, `global = true`) are
/// accepted by every subcommand. They may or may not be echoed in each
/// subcommand's `--help`, so we never flag them as drift.
const GLOBAL_FLAGS: &[&str] = &["--json", "--unlimited", "--verbose", "--project"];

#[test]
fn mcp_declared_flags_are_accepted_by_cli() {
    let bin = env!("CARGO_BIN_EXE_aden");
    let mut failures: Vec<String> = Vec::new();

    for (tool, args) in aden_mcp::tool_arg_specs() {
        let out = Command::new(bin)
            .args([tool, "--help"])
            .output()
            .unwrap_or_else(|e| panic!("failed to run `aden {tool} --help`: {e}"));

        // `--help` exits 0 and prints to stdout for every real subcommand.
        let help = String::from_utf8_lossy(&out.stdout);
        if help.trim().is_empty() {
            failures.push(format!(
                "{tool}: `aden {tool} --help` produced no help (unknown subcommand?). stderr: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
            continue;
        }

        for (arg, _ty) in args {
            // Positional args are not flags — clap accepts them by position.
            if aden_mcp::arg_is_positional(tool, arg) {
                continue;
            }
            let flag = format!("--{}", arg.replace('_', "-"));
            if GLOBAL_FLAGS.contains(&flag.as_str()) {
                continue;
            }
            if !help.contains(&flag) {
                failures.push(format!(
                    "{tool}: MCP can emit `{flag}` but `aden {tool} --help` does not list it"
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "MCP\u{2194}CLI flag drift detected ({} issue(s)):\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}
