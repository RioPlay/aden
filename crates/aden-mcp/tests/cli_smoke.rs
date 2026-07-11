// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Standalone-binary smoke tests.  The MCP server normally owns stdio, so
//! these commands must stay non-interactive for installers and client setup.

use std::process::Command;

const AI_INTEGRATION_GUIDE: &str = include_str!("../../../docs/ai-integration.adoc");

#[test]
fn version_exits_before_starting_the_stdio_transport() {
    let output = Command::new(env!("CARGO_BIN_EXE_aden-mcp"))
        .arg("--version")
        .output()
        .expect("run aden-mcp --version");

    assert!(
        output.status.success(),
        "aden-mcp --version failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("aden-mcp {}", env!("CARGO_PKG_VERSION"))
    );
    assert!(
        output.stderr.is_empty(),
        "version must not initialize MCP: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn documented_installation_smoke_command_is_executable_and_golden() {
    let documented = "`aden-mcp --version`";
    assert!(
        AI_INTEGRATION_GUIDE.contains(documented),
        "the executable smoke command must remain documented verbatim"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_aden-mcp"))
        .arg("--version")
        .output()
        .expect("execute the documented aden-mcp --version example");
    assert!(output.status.success());

    let expected = format!("aden-mcp {}\n", env!("CARGO_PKG_VERSION"));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    assert!(output.stderr.is_empty());
}
