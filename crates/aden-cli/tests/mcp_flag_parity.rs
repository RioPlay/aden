// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Contract tests pinning the MCP tool registry to the `aden` CLI surface, in
//! BOTH directions:
//!
//! 1. MCP → CLI (`mcp_declared_flags_are_accepted_by_cli`): every flag the MCP
//!    server can emit must be accepted by the CLI — otherwise clap rejects it
//!    and the agent's call fails opaquely.
//! 2. CLI → MCP (`cli_flags_are_declared_on_mcp_tools`): every long flag a CLI
//!    subcommand exposes must be declared on its MCP tool spec (or be expressly
//!    exempted) — otherwise capability silently drifts off the MCP surface.
//! 3. Command coverage (`cli_commands_without_mcp_tools_are_expressly_exempt`):
//!    every top-level CLI command must have an MCP tool or a conscious
//!    exemption, so a new command can't ship un-surfaced by accident.

use std::process::Command;

/// Global clap args (defined on the top-level `Cli`, `global = true`) are
/// accepted by every subcommand and echoed in every subcommand's `--help`.
/// They are never per-tool drift in either direction: the MCP injects `--json`
/// itself where a tool has a JSON envelope (`structured_output_flags`), and
/// `--project` is meaningless when the server already pins `current_dir` to
/// its confined project root.
const GLOBAL_FLAGS: &[&str] = &["--json", "--unlimited", "--verbose", "--project"];

/// CLI flags deliberately NOT exposed over MCP: `(tool, flag, reason)`.
/// Every entry must carry a reason — an empty list is the healthy state.
/// (Positional CLI args are out of scope here: the reverse check enumerates
/// `--long` flags only; positionals are covered by the forward direction.)
const REVERSE_EXEMPT: &[(&str, &str, &str)] = &[(
    "heal",
    "--watch",
    "long-running file-watch daemon; always trips the MCP request/response timeout",
)];

/// Top-level CLI commands with NO MCP tool, each with the reason it is kept
/// off the MCP surface. A new CLI command fails the coverage test until it
/// either gets a ToolSpec in `aden-mcp` or a conscious entry here.
const COMMANDS_WITHOUT_MCP_TOOL: &[(&str, &str)] = &[
    (
        "view",
        "interactive browser viewer — long-running, not request/response",
    ),
    (
        "timeline",
        "interactive browser file-history viewer (like view); writes HTML + opens a browser, not request/response",
    ),
    (
        "watch",
        "long-running file-watch daemon; would always hit the MCP timeout",
    ),
    (
        "store",
        "host-level store admin (path/list/prune/migrate); operator-only",
    ),
    (
        "suggest",
        "meta command recommender; an MCP agent picks tools from the registry",
    ),
    ("overlay", "interactive authoring flow (opens an editor)"),
    (
        "agents-md",
        "one-time repo setup step (init --agents-md covers the MCP path)",
    ),
    ("help", "clap built-in"),
];

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

/// Extract the long flags (`--foo-bar`) DEFINED in a clap `--help` output.
/// Only flag-definition lines are considered (trimmed line starts with `-`),
/// so flags merely mentioned inside help prose (e.g. "git diff --cached") are
/// not picked up.
fn long_flags_in_help(help: &str) -> Vec<String> {
    let mut flags = Vec::new();
    let mut in_options = false;
    for line in help.lines() {
        // clap renders an `Options:` header (sometimes after `Arguments:`).
        if line.trim() == "Options:" {
            in_options = true;
            continue;
        }
        if !in_options {
            continue;
        }
        let t = line.trim_start();
        // Definition lines look like `--regex …` or `-i, --ignore-case …`.
        if !t.starts_with('-') {
            continue;
        }
        if let Some(idx) = t.find("--") {
            let name: String = t[idx + 2..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            if !name.is_empty() {
                flags.push(format!("--{name}"));
            }
        }
    }
    flags.sort();
    flags.dedup();
    flags
}

/// Reverse direction: every long flag a CLI subcommand defines must be
/// declared on its MCP tool spec, unless expressly exempted. Catches the
/// drift class where a new CLI flag ships but never reaches the MCP surface
/// (the audit found six such tools: ask/communities/gen/init/session/licenses).
#[test]
fn cli_flags_are_declared_on_mcp_tools() {
    let bin = env!("CARGO_BIN_EXE_aden");
    let mut failures: Vec<String> = Vec::new();

    for (tool, args) in aden_mcp::tool_arg_specs() {
        let out = Command::new(bin)
            .args([tool, "--help"])
            .output()
            .unwrap_or_else(|e| panic!("failed to run `aden {tool} --help`: {e}"));
        let help = String::from_utf8_lossy(&out.stdout);

        for flag in long_flags_in_help(&help) {
            if flag == "--help" || GLOBAL_FLAGS.contains(&flag.as_str()) {
                continue;
            }
            let arg_name = flag.trim_start_matches("--").replace('-', "_");
            if args.iter().any(|(a, _ty)| *a == arg_name) {
                continue;
            }
            if REVERSE_EXEMPT
                .iter()
                .any(|(t, f, _why)| *t == tool && *f == flag)
            {
                continue;
            }
            failures.push(format!(
                "{tool}: CLI defines `{flag}` but the MCP tool spec does not declare \
                 `{arg_name}` (add it to the ToolSpec, or exempt it in REVERSE_EXEMPT \
                 with a reason)"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "CLI\u{2192}MCP flag drift detected ({} issue(s)):\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

/// Enum drift: every value the MCP schema pins in an `enum` (check/lint
/// severity, viz mode, asm/audit/etc. format, ask intent, search doc_type, …)
/// must still appear in that command's `--help`. Catches the harmful direction —
/// a value renamed or removed in the CLI while the MCP enum keeps offering it.
/// (It does NOT force completeness when the CLI *adds* a value: enums are
/// client-side hints, never server-enforced, so a lagging enum only under-offers
/// — it can never mis-route a call.)
#[test]
fn mcp_enum_values_exist_in_cli_help() {
    let bin = env!("CARGO_BIN_EXE_aden");
    let mut failures: Vec<String> = Vec::new();

    for (tool, arg, values) in aden_mcp::tool_arg_enums() {
        let out = Command::new(bin)
            .args([tool, "--help"])
            .output()
            .unwrap_or_else(|e| panic!("failed to run `aden {tool} --help`: {e}"));
        let help = String::from_utf8_lossy(&out.stdout);
        for v in values {
            if !help.contains(v) {
                failures.push(format!(
                    "{tool}.{arg}: MCP enum offers `{v}` but `aden {tool} --help` no longer \
                     lists it (update arg_enum in aden-mcp, or the CLI)"
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "MCP enum/CLI drift detected ({} issue(s)):\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

/// Parse the top-level command names out of `aden --help`'s `Commands:` block.
fn top_level_commands(help: &str) -> Vec<String> {
    let mut cmds = Vec::new();
    let mut in_commands = false;
    for line in help.lines() {
        if line.trim() == "Commands:" {
            in_commands = true;
            continue;
        }
        if !in_commands {
            continue;
        }
        // The block ends at the next unindented header (e.g. `Options:`).
        if !line.starts_with(' ') {
            if !line.trim().is_empty() {
                break;
            }
            continue;
        }
        if let Some(first) = line.split_whitespace().next() {
            cmds.push(first.trim_end_matches(',').to_string());
        }
    }
    cmds
}

/// Coverage: the set of CLI commands WITHOUT an MCP tool must exactly match
/// the conscious exemption list. A new CLI command therefore fails this test
/// until someone adds a ToolSpec or an explicit exemption with a reason.
#[test]
fn cli_commands_without_mcp_tools_are_expressly_exempt() {
    let bin = env!("CARGO_BIN_EXE_aden");
    let out = Command::new(bin)
        .arg("--help")
        .output()
        .unwrap_or_else(|e| panic!("failed to run `aden --help`: {e}"));
    let help = String::from_utf8_lossy(&out.stdout);

    let commands = top_level_commands(&help);
    assert!(
        commands.len() > 20,
        "suspiciously few commands parsed from `aden --help` — parser broken? got: {commands:?}"
    );

    let tools: Vec<&str> = aden_mcp::tool_arg_specs()
        .iter()
        .map(|(name, _)| *name)
        .collect();

    let mut failures: Vec<String> = Vec::new();
    for cmd in &commands {
        let has_tool = tools.contains(&cmd.as_str());
        let exempt = COMMANDS_WITHOUT_MCP_TOOL.iter().any(|(c, _)| c == cmd);
        match (has_tool, exempt) {
            (false, false) => failures.push(format!(
                "`aden {cmd}` has no MCP tool: add a ToolSpec in aden-mcp, or add it \
                 to COMMANDS_WITHOUT_MCP_TOOL with a reason"
            )),
            (true, true) => failures.push(format!(
                "`{cmd}` is both an MCP tool and exempted — remove the stale \
                 COMMANDS_WITHOUT_MCP_TOOL entry"
            )),
            _ => {}
        }
    }
    // Every MCP tool must correspond to a real CLI command (catches a tool
    // outliving a removed/renamed command). Feature-gated exempt commands
    // (view/watch) may legitimately be absent from --help, so only tools are
    // checked, not exemptions.
    for tool in &tools {
        if !commands.iter().any(|c| c == tool) {
            failures.push(format!(
                "MCP tool `{tool}` has no matching `aden {tool}` command in `aden --help`"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "MCP tool coverage drift ({} issue(s)):\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}
