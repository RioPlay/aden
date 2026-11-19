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
const GLOBAL_FLAGS: &[&str] = &["--json", "--human", "--unlimited", "--verbose", "--project"];

/// `--require-fresh` is parsed globally by clap, so it appears in every
/// subcommand's help. It is intentionally an MCP *read-surface* capability:
/// write/admin tools must not advertise a freshness promise that they cannot
/// fulfill. Keep it out of the generic global list so the forward parity test
/// still verifies every MCP read tool that exposes it.
const READ_SURFACE_ONLY_GLOBAL_FLAG: &str = "--require-fresh";

/// CLI flags deliberately NOT exposed over MCP: `(CLI command, flag, reason)`.
/// Every entry must carry a reason — an empty list is the healthy state.
/// (Positional CLI args are out of scope here: the reverse check enumerates
/// `--long` flags only; positionals are covered by the forward direction.)
const REVERSE_EXEMPT: &[(&str, &str, &str)] = &[
    (
        "tree",
        "--files",
        "MCP selects this mode through the existing symbols=false argument",
    ),
    (
        "heal",
        "--watch",
        "long-running file-watch daemon; always trips the MCP request/response timeout",
    ),
    (
        "gen",
        "--auto",
        "MCP gen is background-only; auto discovery has no background JobStatus contract",
    ),
    (
        "gen",
        "--quiet",
        "MCP gen returns JobStatus rather than per-file or summary generation output",
    ),
    (
        "gen",
        "--propose",
        "proposal mode is unsupported on the MCP background generation surface",
    ),
    (
        "gen",
        "--force-regen",
        "force regeneration is unsupported on the MCP background generation surface",
    ),
    (
        "gen",
        "--background",
        "MCP gen always injects this flag to ensure generation outlives the request",
    ),
];

/// Hidden command handlers that are implementation details, never MCP routes.
const INTERNAL_CLI_COMMANDS: &[(&str, &str)] = &[(
    "__gen-worker",
    "owned background worker launched by generation_job; not a caller-facing command",
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
    (
        "commands",
        "human-facing catalog of hidden compatibility commands; MCP already has tool discovery",
    ),
    ("overlay", "interactive authoring flow (opens an editor)"),
    (
        "agents-md",
        "one-time repo setup step (init --agents-md covers the MCP path)",
    ),
    (
        "model",
        "opt-in operator setup (model-fetch feature); downloads the embedding model over the network, not a request/response retrieval tool",
    ),
    ("help", "clap built-in"),
    (
        "scope",
        "advanced scoping for impact gates and agent task manifests (produces files consumed by impact-diff --scope); primarily a human/CI workflow, not general agent retrieval over MCP",
    ),
    (
        "config",
        "local aden configuration get/set (.aden/config.toml); host-specific operator tool, not useful or safe to surface over MCP",
    ),
];

/// Extract clap positional labels from `Arguments:` (e.g. `[DIR]`,
/// `<QUESTION>`).  This is intentionally separate from the flag parser: MCP
/// must preserve both the *existence* and the order-sensitive nature of CLI
/// positionals.
fn positional_labels_in_help(help: &str) -> Vec<String> {
    let mut labels = Vec::new();
    let mut in_arguments = false;
    for line in help.lines() {
        if line.trim() == "Arguments:" {
            in_arguments = true;
            continue;
        }
        if !in_arguments {
            continue;
        }
        if line.trim() == "Options:" {
            break;
        }
        let label = line.split_whitespace().next().unwrap_or_default();
        if !label.is_empty() {
            labels.push(
                label
                    .trim_matches(|c| c == '[' || c == ']' || c == '<' || c == '>' || c == '.')
                    .to_ascii_lowercase(),
            );
        }
    }
    labels
}

fn cli_help(bin: &str, tool: &str) -> String {
    let mut args = cli_command_path_for_mcp(tool);
    args.push("--help");
    let out = Command::new(bin)
        .args(&args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run `aden {}`: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "`aden {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Resolve only the three MCP generation names whose CLI command differs from
/// their ToolSpec name. Every other tool remains a direct top-level mapping.
fn cli_command_path_for_mcp(tool: &str) -> Vec<&str> {
    match tool {
        "generation_status" => vec!["jobs", "status"],
        "generation_start" => vec!["gen"],
        "generation_cancel" => vec!["jobs", "cancel"],
        _ => vec![tool],
    }
}

/// Extract every clap-rendered default which belongs to an MCP-exposed
/// argument. This is the reverse of `mcp_schema_defaults_are_documented...`:
/// a new CLI default must not silently remain invisible to schema-aware agents.
fn cli_defaults_in_help(tool: &str, help: &str, exposed: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut defaults = Vec::new();
    for line in help.lines() {
        let Some(default_start) = line.find("[default: ") else {
            continue;
        };
        let value_start = default_start + "[default: ".len();
        let Some(value_end) = line[value_start..].find(']') else {
            continue;
        };
        let rendered = line[value_start..value_start + value_end].to_string();

        let flag_arg = line[..default_start]
            .split_whitespace()
            .find(|token| token.starts_with("--"))
            .map(|token| token.trim_start_matches('-').replace('-', "_"));
        let positional_arg = if flag_arg.is_none() {
            let label = line
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_matches(|c| matches!(c, '[' | ']' | '<' | '>' | '.'))
                .to_ascii_lowercase();
            match label.as_str() {
                "dir" => Some("path".to_string()),
                _ => None,
            }
        } else {
            None
        };
        let Some(arg) = flag_arg.or(positional_arg) else {
            continue;
        };
        if exposed.iter().any(|(name, _)| *name == arg) {
            defaults.push((arg, rendered));
        } else {
            panic!("{tool}: CLI default for unrecognized MCP argument `{arg}`: {line}");
        }
    }
    defaults
}

/// MCP generation_start maps to the CLI's variadic `gen [PATH]...`. The CLI
/// deliberately omits a clap-rendered default for that Vec, then resolves an
/// empty path list to the current project directory before starting work.
fn mapped_cli_default_matches(tool: &str, arg: &str, default: &str, help: &str) -> bool {
    tool == "generation_start"
        && arg == "path"
        && default == "."
        && cli_command_path_for_mcp(tool) == ["gen"]
        && positional_labels_in_help(help)
            .iter()
            .any(|label| label == "path")
        && !help.contains("[default: .]")
}

#[test]
fn mcp_declared_flags_are_accepted_by_cli() {
    let bin = env!("CARGO_BIN_EXE_aden");
    let mut failures: Vec<String> = Vec::new();

    for (tool, args) in aden_mcp::tool_arg_specs() {
        let mut help_args = cli_command_path_for_mcp(tool);
        help_args.push("--help");
        let out = Command::new(bin)
            .args(&help_args)
            .output()
            .unwrap_or_else(|e| panic!("failed to run `aden {}`: {e}", help_args.join(" ")));

        // `--help` exits 0 and prints to stdout for every real subcommand.
        let help = String::from_utf8_lossy(&out.stdout);
        if help.trim().is_empty() {
            failures.push(format!(
                "{tool}: `aden {}` produced no help (unknown subcommand?). stderr: {}",
                help_args.join(" "),
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

/// Positionals are where the two transports are easiest to accidentally
/// desynchronize: MCP uses named JSON fields while clap binds by order.  Each
/// MCP positional must therefore be visible in clap's Arguments section.
#[test]
fn mcp_positionals_are_visible_in_cli_arguments() {
    let bin = env!("CARGO_BIN_EXE_aden");
    let mut failures = Vec::new();
    for (tool, args) in aden_mcp::tool_arg_specs() {
        let labels = positional_labels_in_help(&cli_help(bin, tool));
        for (arg, _) in args {
            // clap renders a nested command dispatcher in `Commands:` rather
            // than `Arguments:`. `mcp_enum_values_exist_in_cli_help` below
            // pins each advertised action to that help, so this is not an
            // untested exception.
            let command_dispatch = matches!((tool, *arg), ("federation" | "mcp", "action"));
            if aden_mcp::arg_is_positional(tool, arg)
                && !command_dispatch
                && !labels.iter().any(|label| label == &arg.replace('_', "-"))
                // clap calls the `from` CLI positional ANCHOR for asm-like
                // commands; only fields declared positional reach this branch.
                && !labels.iter().any(|label| label == "dir" && *arg == "path")
            {
                failures.push(format!(
                    "{tool}.{arg}: MCP declares a positional but clap help has Arguments: {labels:?}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "MCP→CLI positional drift:\n  {}",
        failures.join("\n  ")
    );
}

/// The only MCP-required fields must correspond to required clap positionals
/// or flags. This catches the damaging direction where a schema makes an
/// optional CLI input impossible for an agent to omit.
#[test]
fn mcp_required_args_are_required_by_cli_usage() {
    let bin = env!("CARGO_BIN_EXE_aden");
    let mut failures = Vec::new();
    for (tool, _) in aden_mcp::tool_arg_specs() {
        let help = cli_help(bin, tool);
        let usage = help
            .lines()
            .find(|line| line.trim_start().starts_with("Usage:"))
            .unwrap_or_default();
        for arg in aden_mcp::tool_required_args(tool) {
            let expected = if aden_mcp::arg_is_positional(tool, arg) {
                format!("<{arg}").to_ascii_uppercase()
            } else {
                format!("--{} <", arg.replace('_', "-"))
            };
            if !usage
                .to_ascii_lowercase()
                .contains(&expected.to_ascii_lowercase())
            {
                failures.push(format!(
                    "{tool}.{arg}: MCP marks required but CLI usage is `{usage}`"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "MCP requiredness drift:\n  {}",
        failures.join("\n  ")
    );
}

/// Schema defaults are user-facing promises. Every MCP default must still be
/// rendered by clap, including the shared project path default.
#[test]
fn mcp_schema_defaults_are_documented_by_cli_help() {
    let bin = env!("CARGO_BIN_EXE_aden");
    let mut failures = Vec::new();
    for (tool, args) in aden_mcp::tool_arg_specs() {
        let help = cli_help(bin, tool);
        for (arg, _) in args {
            let Some(default) = aden_mcp::tool_arg_default(tool, arg) else {
                continue;
            };
            let rendered = default
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| default.to_string());
            let documented = if aden_mcp::tool_arg_default_is_transport_override(tool, arg) {
                help.contains(&format!("MCP default: {rendered}"))
            } else if mapped_cli_default_matches(tool, arg, &rendered, &help) {
                true
            } else {
                help.contains(&format!("[default: {rendered}]"))
            };
            if !documented {
                failures.push(format!(
                    "{tool}.{arg}: MCP default `{rendered}` is absent from CLI help"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "MCP default drift:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn every_mcp_exposed_cli_default_is_declared_by_mcp() {
    let bin = env!("CARGO_BIN_EXE_aden");
    let mut failures = Vec::new();
    for (tool, args) in aden_mcp::tool_arg_specs() {
        for (arg, cli_default) in cli_defaults_in_help(tool, &cli_help(bin, tool), args) {
            if aden_mcp::tool_arg_default_is_transport_override(tool, &arg) {
                continue;
            }
            let mcp_default = aden_mcp::tool_arg_default(tool, &arg).map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string())
            });
            if mcp_default.as_deref() != Some(cli_default.as_str()) {
                failures.push(format!(
                    "{tool}.{arg}: CLI default `{cli_default}`, MCP default {mcp_default:?}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "CLI→MCP default drift:\n  {}",
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
        let help = cli_help(bin, tool);
        let cli_command = cli_command_path_for_mcp(tool)
            .last()
            .copied()
            .unwrap_or(tool);

        for flag in long_flags_in_help(&help) {
            if flag == "--help" || GLOBAL_FLAGS.contains(&flag.as_str()) {
                continue;
            }
            if flag == READ_SURFACE_ONLY_GLOBAL_FLAG {
                continue;
            }
            let arg_name = flag.trim_start_matches("--").replace('-', "_");
            if args.iter().any(|(a, _ty)| *a == arg_name) {
                continue;
            }
            if REVERSE_EXEMPT
                .iter()
                .any(|(command, f, _why)| *command == cli_command && *f == flag)
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

#[test]
fn require_fresh_is_exposed_only_on_authoritative_read_tools() {
    for (tool, _) in aden_mcp::tool_arg_specs() {
        let declared = aden_mcp::tool_advertises_authoritative_freshness(tool);
        assert_eq!(
            declared,
            aden_mcp::supports_authoritative_freshness(tool),
            "{tool}: MCP schema drifted from the authoritative read-surface classification"
        );
    }
    assert!(aden_mcp::supports_authoritative_freshness("grep"));
    assert!(!aden_mcp::supports_authoritative_freshness("gen"));
    assert!(!aden_mcp::supports_authoritative_freshness(
        "generation_status"
    ));
    assert!(!aden_mcp::tool_advertises_authoritative_freshness(
        "generation_status"
    ));
    assert!(!aden_mcp::supports_authoritative_freshness("audit"));
}

#[test]
fn generation_status_does_not_advertise_or_accept_require_fresh() {
    let args = serde_json::json!({"require_fresh": true})
        .as_object()
        .unwrap()
        .clone();
    assert!(!aden_mcp::tool_advertises_authoritative_freshness(
        "generation_status"
    ));
    assert!(aden_mcp::prepare_cli_args_for_mcp("generation_status", &args).is_err());
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
        let help = cli_help(bin, tool);
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
fn cli_short_flags_remain_available_for_common_workflows() {
    let bin = env!("CARGO_BIN_EXE_aden");
    let expectations: &[(&str, &[&str])] = &[
        ("gen", &["-a", "-q", "-P", "-f"]),
        ("check", &["-m"]),
        ("complete", &["-n", "-M"]),
        ("lint", &["-f", "-j", "-d", "-p"]),
        ("test", &["-s", "-f", "-l"]),
    ];
    for (tool, flags) in expectations {
        let help = cli_help(bin, tool);
        for flag in *flags {
            assert!(
                help.contains(flag),
                "{tool} help should expose {flag}: {help}"
            );
        }
    }
}

#[test]
fn cli_commands_without_mcp_tools_are_expressly_exempt() {
    let bin = env!("CARGO_BIN_EXE_aden");
    let out = Command::new(bin)
        .arg("--help")
        .output()
        .unwrap_or_else(|e| panic!("failed to run `aden --help`: {e}"));
    let help = String::from_utf8_lossy(&out.stdout);

    let commands = top_level_commands(&help);
    // Top-level help is intentionally focused; optional compatibility commands
    // live in `aden commands` and remain covered by flag/default parity tests.
    assert!(
        commands.len() >= 10,
        "focused command parser looks broken; got: {commands:?}"
    );

    let tools: Vec<&str> = aden_mcp::tool_arg_specs()
        .iter()
        .map(|(name, _)| *name)
        .collect();

    let mut failures: Vec<String> = Vec::new();
    for cmd in &commands {
        let has_tool = tools.iter().any(|tool| {
            cli_command_path_for_mcp(tool)
                .first()
                .is_some_and(|command| *command == cmd.as_str())
        });
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
    // Hidden compatibility commands and explicit nested generation routes do
    // not appear as MCP tool names, so validate their mapped CLI help paths.
    for tool in &tools {
        let mut args = cli_command_path_for_mcp(tool);
        args.push("--help");
        let exists = Command::new(bin)
            .args(&args)
            .output()
            .is_ok_and(|output| output.status.success());
        if !exists {
            failures.push(format!(
                "MCP tool `{tool}` has no matching callable `aden {}` route",
                cli_command_path_for_mcp(tool).join(" ")
            ));
        }
    }

    for (internal, reason) in INTERNAL_CLI_COMMANDS {
        assert!(
            !reason.trim().is_empty(),
            "{internal} needs an internal-command reason"
        );
        if tools
            .iter()
            .any(|tool| cli_command_path_for_mcp(tool).contains(internal))
        {
            failures.push(format!(
                "internal CLI helper `{internal}` must not be an MCP route"
            ));
        }
        let exists = Command::new(bin)
            .args([*internal, "--help"])
            .output()
            .is_ok_and(|output| output.status.success());
        if !exists {
            failures.push(format!(
                "expected hidden internal CLI helper `{internal}` is unavailable"
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

#[test]
fn generation_tools_map_to_expected_cli_help_paths() {
    let bin = env!("CARGO_BIN_EXE_aden");
    assert_eq!(
        cli_command_path_for_mcp("generation_status"),
        ["jobs", "status"]
    );
    assert_eq!(cli_command_path_for_mcp("generation_start"), ["gen"]);
    assert_eq!(
        cli_command_path_for_mcp("generation_cancel"),
        ["jobs", "cancel"]
    );
    assert!(cli_help(bin, "generation_start").contains("--background"));
    assert!(cli_help(bin, "generation_status").contains("status"));
    assert!(cli_help(bin, "generation_cancel").contains("--job-id"));
    let gen_help = cli_help(bin, "generation_start");
    assert!(mapped_cli_default_matches(
        "generation_start",
        "path",
        ".",
        &gen_help,
    ));
}
