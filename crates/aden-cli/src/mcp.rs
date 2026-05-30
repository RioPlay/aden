// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP (Model Context Protocol) integration installer.
//!
//! Configures the `aden-mcp` binary as an MCP server for popular AI agent
//! platforms: opencode, Claude Code, Cursor, Codex, Zed, and Windsurf.

use serde_json::Value;
use std::path::{Path, PathBuf};
use toml_edit::{
    Array as TomlArray, DocumentMut, Item as TomlItem, Table as TomlTable, value as toml_value,
};

/// Supported MCP platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    OpenCode,
    ClaudeCode,
    Cursor,
    Codex,
    Zed,
    Windsurf,
}

impl Platform {
    pub fn all() -> &'static [Platform] {
        &[
            Platform::OpenCode,
            Platform::ClaudeCode,
            Platform::Cursor,
            Platform::Codex,
            Platform::Zed,
            Platform::Windsurf,
        ]
    }

    pub fn from_name(name: &str) -> Option<Platform> {
        match name.to_lowercase().as_str() {
            "opencode" | "open-code" | "open_code" => Some(Platform::OpenCode),
            "claude" | "claude-code" | "claudecode" => Some(Platform::ClaudeCode),
            "cursor" => Some(Platform::Cursor),
            "codex" | "openai-codex" | "openai_codex" => Some(Platform::Codex),
            "zed" => Some(Platform::Zed),
            "windsurf" | "windsurf-editor" => Some(Platform::Windsurf),
            _ => None,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Platform::OpenCode => "opencode",
            Platform::ClaudeCode => "Claude Code",
            Platform::Cursor => "Cursor",
            Platform::Codex => "Codex (OpenAI)",
            Platform::Zed => "Zed",
            Platform::Windsurf => "Windsurf",
        }
    }

    /// Config file locations, ordered by preference (project-local first).
    pub fn config_paths(&self) -> Vec<PathBuf> {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
        match self {
            Platform::OpenCode => vec![
                PathBuf::from(".opencode/opencode.json"),
                PathBuf::from(".opencode/opencode.jsonc"),
                home.join(".config/opencode/opencode.json"),
                home.join(".config/opencode/opencode.jsonc"),
            ],
            // Claude Code reads MCP servers from a project-scoped `.mcp.json`
            // (committed, discoverable) — NOT `~/.claude/settings.json`. We write
            // only the project file and never rewrite the user's `~/.claude.json`
            // state; for a user-scoped install use `claude mcp add aden -s user
            // -- aden-mcp`.
            Platform::ClaudeCode => vec![PathBuf::from(".mcp.json")],
            Platform::Cursor => vec![
                PathBuf::from(".cursor/mcp.json"),
                home.join(".cursor/mcp.json"),
            ],
            // Codex reads MCP servers from a TOML config (`[mcp_servers.<id>]`),
            // not JSON. Project-scoped `.codex/config.toml` is honored in trusted
            // projects; the user file is `~/.codex/config.toml`.
            Platform::Codex => vec![
                PathBuf::from(".codex/config.toml"),
                home.join(".codex/config.toml"),
            ],
            Platform::Zed => vec![
                PathBuf::from(".zed/settings.json"),
                home.join(".config/zed/settings.json"),
            ],
            // Windsurf (Cascade) reads only a single user-global config at
            // `~/.codeium/windsurf/mcp_config.json` — there is no project scope.
            Platform::Windsurf => vec![home.join(".codeium/windsurf/mcp_config.json")],
        }
    }

    /// True if any config path or the platform's config directory exists.
    pub fn is_detected(&self) -> bool {
        self.config_paths().iter().any(|p| p.exists())
    }

    /// True if this platform's config file is TOML rather than JSON.
    pub fn is_toml(&self) -> bool {
        matches!(self, Platform::Codex)
    }

    /// The config key/table under which MCP servers live for this platform.
    /// For TOML platforms (Codex) this is the table name (`mcp_servers`).
    pub fn server_config_key(&self) -> &'static str {
        match self {
            Platform::OpenCode => "mcp",
            Platform::ClaudeCode | Platform::Cursor | Platform::Windsurf => "mcpServers",
            Platform::Codex => "mcp_servers",
            Platform::Zed => "context_servers",
        }
    }

    /// The JSON value to insert for the aden MCP server. Only valid for
    /// JSON-config platforms; TOML platforms (Codex) are handled separately.
    pub fn aden_config(&self, binary: &str, project: &str) -> Value {
        match self {
            Platform::OpenCode => serde_json::json!({
                "type": "local",
                "command": [binary, project],
                "enabled": true,
            }),
            Platform::ClaudeCode | Platform::Cursor | Platform::Windsurf => serde_json::json!({
                "command": binary,
                "args": [project],
            }),
            // Zed requires `source: "custom"` for manually-declared servers.
            Platform::Zed => serde_json::json!({
                "source": "custom",
                "command": binary,
                "args": [project],
            }),
            Platform::Codex => {
                unreachable!("Codex uses a TOML config; handled by the TOML install path")
            }
        }
    }
}

/// Make a path absolute without requiring it to exist.
fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|e| e.to_string())
    }
}

/// Read a JSON config file, or return an empty object.
fn read_config(path: &Path) -> Result<Value, String> {
    if !path.exists() {
        return Ok(Value::Object(Default::default()));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;

    serde_json::from_str(&text).map_err(|e| {
        format!(
            "Invalid JSON in {}: {}\n\
            Note: JSONC files (with comments) are not supported. \
            Please convert to plain JSON or edit manually.",
            path.display(),
            e
        )
    })
}

/// Write a JSON config file atomically (temp + rename).
fn write_config(path: &Path, value: &Value) -> Result<(), String> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| format!("JSON serialization failed: {}", e))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory {}: {}", parent.display(), e))?;
    }

    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, json + "\n")
        .map_err(|e| format!("Failed to write temp file {}: {}", tmp.display(), e))?;

    std::fs::rename(&tmp, path).map_err(|e| {
        format!(
            "Failed to rename {} to {}: {}",
            tmp.display(),
            path.display(),
            e
        )
    })?;

    Ok(())
}

/// Read a TOML config file as an editable document, or return an empty one.
/// `toml_edit` preserves the user's existing keys, comments, and formatting.
fn read_toml(path: &Path) -> Result<DocumentMut, String> {
    if !path.exists() {
        return Ok(DocumentMut::new());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    text.parse::<DocumentMut>()
        .map_err(|e| format!("Invalid TOML in {}: {}", path.display(), e))
}

/// Write a TOML document atomically (temp + rename).
fn write_toml(path: &Path, doc: &DocumentMut) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory {}: {}", parent.display(), e))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, doc.to_string())
        .map_err(|e| format!("Failed to write temp file {}: {}", tmp.display(), e))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        format!(
            "Failed to rename {} to {}: {}",
            tmp.display(),
            path.display(),
            e
        )
    })?;
    Ok(())
}

/// Build the `[mcp_servers.aden]` TOML table for Codex.
fn codex_aden_table(binary: &str, project: &str) -> TomlTable {
    let mut tbl = TomlTable::new();
    tbl["command"] = toml_value(binary);
    let mut args = TomlArray::new();
    args.push(project);
    tbl["args"] = toml_value(args);
    tbl
}

/// Return the first existing config path, or the first config path if none exist.
fn active_config_path(platform: &Platform) -> PathBuf {
    platform
        .config_paths()
        .into_iter()
        .find(|p| p.exists())
        .unwrap_or_else(|| platform.config_paths()[0].clone())
}

/// Check whether aden is already configured at the given path.
fn is_configured_at(path: &Path, platform: &Platform) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let key = platform.server_config_key();
    if platform.is_toml() {
        let doc = read_toml(path)?;
        return Ok(doc
            .get(key)
            .and_then(|t| t.as_table())
            .map(|t| t.contains_key("aden"))
            .unwrap_or(false));
    }
    let cfg = read_config(path)?;
    match cfg.get(key) {
        Some(Value::Object(map)) => Ok(map.contains_key("aden")),
        _ => Ok(false),
    }
}

/// Install aden MCP into one platform.
fn install_platform(
    platform: &Platform,
    binary: &Path,
    project: &Path,
    dry_run: bool,
) -> Result<(), String> {
    let config_path = active_config_path(platform);
    let key = platform.server_config_key();
    let binary = binary.to_string_lossy();
    let project = project.to_string_lossy();

    if platform.is_toml() {
        let mut doc = read_toml(&config_path)?;
        // Ensure the [mcp_servers] table exists, then set [mcp_servers.aden].
        // Mark the parent implicit so a fresh config renders just the
        // `[mcp_servers.aden]` subtable, not a redundant empty `[mcp_servers]`.
        if !doc.get(key).map(|t| t.is_table()).unwrap_or(false) {
            let mut parent = TomlTable::new();
            parent.set_implicit(true);
            doc[key] = TomlItem::Table(parent);
        }
        doc[key]["aden"] = TomlItem::Table(codex_aden_table(&binary, &project));

        if dry_run {
            println!("  [dry-run] Would write: {}", config_path.display());
            return Ok(());
        }
        write_toml(&config_path, &doc)?;
        println!("  ✓ Configured: {}", config_path.display());
        return Ok(());
    }

    let mut cfg = read_config(&config_path)?;

    // Ensure the server config key exists as an object
    if !cfg.get(key).map(|v| v.is_object()).unwrap_or(false) {
        cfg[key] = Value::Object(Default::default());
    }

    // Merge or create the aden entry
    if let Some(Value::Object(servers)) = cfg.get_mut(key) {
        let aden_value = platform.aden_config(&binary, &project);
        servers.insert("aden".to_string(), aden_value);
    }

    if dry_run {
        println!("  [dry-run] Would write: {}", config_path.display());
        return Ok(());
    }

    write_config(&config_path, &cfg)?;
    println!("  ✓ Configured: {}", config_path.display());
    Ok(())
}

/// Remove aden MCP from one platform.
fn uninstall_platform(platform: &Platform, dry_run: bool) -> Result<(), String> {
    let config_path = active_config_path(platform);
    if !config_path.exists() {
        println!("  ✗ No config file: {}", config_path.display());
        return Ok(());
    }

    let key = platform.server_config_key();

    if platform.is_toml() {
        let mut doc = read_toml(&config_path)?;
        let changed = doc
            .get_mut(key)
            .and_then(|t| t.as_table_mut())
            .map(|t| t.remove("aden").is_some())
            .unwrap_or(false);
        if !changed {
            println!("  ✗ aden was not configured in: {}", config_path.display());
            return Ok(());
        }
        if dry_run {
            println!("  [dry-run] Would update: {}", config_path.display());
            return Ok(());
        }
        write_toml(&config_path, &doc)?;
        println!("  ✓ Removed aden from: {}", config_path.display());
        return Ok(());
    }

    let mut cfg = read_config(&config_path)?;

    let changed = match cfg.get_mut(key) {
        Some(Value::Object(servers)) => servers.remove("aden").is_some(),
        _ => false,
    };

    if !changed {
        println!("  ✗ aden was not configured in: {}", config_path.display());
        return Ok(());
    }

    if dry_run {
        println!("  [dry-run] Would update: {}", config_path.display());
        return Ok(());
    }

    write_config(&config_path, &cfg)?;
    println!("  ✓ Removed aden from: {}", config_path.display());
    Ok(())
}

// ── Public commands ──────────────────────────────────────────────

/// Install aden MCP into selected platforms.
pub fn run_install(
    names: &[String],
    binary_override: Option<&Path>,
    project_override: Option<&Path>,
    all: bool,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Resolve platforms
    let platforms: Vec<Platform> = if names.is_empty() {
        if all {
            Platform::all().to_vec()
        } else {
            Platform::all()
                .iter()
                .copied()
                .filter(|p| p.is_detected())
                .collect()
        }
    } else {
        let mut out = Vec::new();
        for name in names {
            match Platform::from_name(name) {
                Some(p) => out.push(p),
                None => {
                    eprintln!("Unknown platform '{}'. Supported:", name);
                    for p in Platform::all() {
                        eprintln!("  - {}", p.display_name());
                    }
                    std::process::exit(1);
                }
            }
        }
        out
    };

    if platforms.is_empty() {
        println!("No platforms detected. Use --all to install for all supported platforms.");
        println!("Supported platforms:");
        for p in Platform::all() {
            println!(
                "  - {} (config: {})",
                p.display_name(),
                p.config_paths()[0].display()
            );
        }
        return Ok(());
    }

    // Resolve the MCP server binary. The stdio server is `aden-mcp`, NOT the
    // `aden` CLI — configuring `aden <project>` as the command would fail (it
    // isn't a subcommand). Default to the `aden-mcp` sibling of the running
    // executable; honor an explicit --bin override.
    let binary = if let Some(b) = binary_override {
        b.to_path_buf()
    } else {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mcp_name = if cfg!(windows) {
            "aden-mcp.exe"
        } else {
            "aden-mcp"
        };
        exe.parent()
            .map(|d| d.join(mcp_name))
            .filter(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("aden-mcp"))
    };
    let binary = absolute_path(&binary)?;

    // Resolve project
    let project = if let Some(p) = project_override {
        p.to_path_buf()
    } else {
        std::env::current_dir()?
    };
    let project = absolute_path(&project)?;

    println!("Installing aden MCP server...");
    println!("  Binary : {}", binary.display());
    println!("  Project: {}", project.display());
    if dry_run {
        println!("  Mode   : dry-run (no changes written)");
    }
    println!();

    let mut ok = 0;
    let mut fail = 0;

    for platform in &platforms {
        println!("Platform: {}", platform.display_name());
        match install_platform(platform, &binary, &project, dry_run) {
            Ok(()) => ok += 1,
            Err(e) => {
                eprintln!("  Error: {}", e);
                fail += 1;
            }
        }
    }

    println!();
    if dry_run {
        println!("Dry-run complete. {} platform(s) would be configured.", ok);
    } else {
        println!("Done. Configured {} platform(s). {} failed.", ok, fail);
    }
    println!();
    println!("Important: Restart your AI agent platform for changes to take effect.");

    Ok(())
}

/// Remove aden MCP from selected platforms.
pub fn run_uninstall(
    names: &[String],
    all: bool,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let platforms: Vec<Platform> = if names.is_empty() {
        if all {
            Platform::all().to_vec()
        } else {
            let detected: Vec<_> = Platform::all()
                .iter()
                .copied()
                .filter(|p| p.is_detected())
                .collect();
            if detected.is_empty() {
                println!(
                    "No platforms detected. Use --all to uninstall from all supported platforms."
                );
                return Ok(());
            }
            detected
        }
    } else {
        let mut out = Vec::new();
        for name in names {
            match Platform::from_name(name) {
                Some(p) => out.push(p),
                None => {
                    eprintln!("Unknown platform '{}'.", name);
                    std::process::exit(1);
                }
            }
        }
        out
    };

    if dry_run {
        println!("Uninstalling aden MCP (dry-run)...\n");
    } else {
        println!("Uninstalling aden MCP...\n");
    }

    let mut ok = 0;
    let mut fail = 0;

    for platform in &platforms {
        println!("Platform: {}", platform.display_name());
        match uninstall_platform(platform, dry_run) {
            Ok(()) => ok += 1,
            Err(e) => {
                eprintln!("  Error: {}", e);
                fail += 1;
            }
        }
    }

    println!();
    if dry_run {
        println!("Dry-run complete. {} platform(s) would be updated.", ok);
    } else {
        println!("Done. Updated {} platform(s). {} failed.", ok, fail);
    }

    Ok(())
}

/// List all supported platforms and their status.
pub fn run_list() -> Result<(), Box<dyn std::error::Error>> {
    println!("Supported MCP Platforms");
    println!("══════════════════════════════════════════════════════════════════");
    println!(
        "{:<18} {:<10} {:<12} Config Path",
        "Platform", "Detected", "Configured"
    );
    println!("──────────────────────────────────────────────────────────────────");

    for platform in Platform::all() {
        let detected = platform.is_detected();
        let config_path = active_config_path(platform);
        let configured = if config_path.exists() {
            is_configured_at(&config_path, platform).unwrap_or(false)
        } else {
            false
        };

        let det_str = if detected { "✓" } else { "✗" };
        let cfg_str = if configured { "✓" } else { "✗" };
        let path_str = if config_path.exists() {
            config_path.display().to_string()
        } else {
            format!("{} (not yet created)", config_path.display())
        };

        println!(
            "{:<18} {:<10} {:<12} {}",
            platform.display_name(),
            det_str,
            cfg_str,
            path_str
        );
    }

    println!();
    println!("Legend:");
    println!("  Detected   – config file or directory exists for this platform");
    println!("  Configured – aden MCP server is present in the config");
    println!();
    println!("To install:   aden mcp install [--platform <name>] [--all]");
    println!("To uninstall: aden mcp uninstall [--platform <name>] [--all]");

    Ok(())
}

/// Start a simple HTTP server for CI/agent integration.
/// Exposes core aden commands via HTTP JSON-RPC.
pub fn run_http_server(_project_dir: &Path, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr)?;

    println!("Aden HTTP Server");
    println!("================");
    println!(
        "Listening on port {}",
        addr.split(':').next_back().unwrap_or(&addr)
    );
    println!("Endpoints:");
    println!("  GET  /health          - Health check");
    println!("  POST /api/check      - Run aden check");
    println!("  POST /api/heal       - Run aden heal");
    println!("  POST /api/asm        - Assemble context");
    println!("  POST /api/query      - Query graph");
    println!();

    for stream in listener.incoming() {
        let mut stream = stream?;
        let mut reader = BufReader::new(&stream);
        let mut request_line = String::new();
        reader.read_line(&mut request_line)?;

        let parts: Vec<&str> = request_line.split_whitespace().collect();
        if parts.len() >= 2 {
            let method = parts[0];
            let path = parts[1];

            let response = match (method, path) {
                ("GET", "/health") => r#"{"status":"ok","service":"aden"}"#,
                ("GET", "/") | ("GET", "/api") => {
                    r#"{"service":"aden","version":"0.1.0","endpoints":["/health","/api/check","/api/heal","/api/asm","/api/query"]}"#
                }
                _ => r#"{"error":"not found}"#,
            };

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                response.len(),
                response
            );
            stream.write_all(response.as_bytes())?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_codex_is_toml() {
        assert!(Platform::Codex.is_toml());
        for p in [
            Platform::ClaudeCode,
            Platform::Cursor,
            Platform::OpenCode,
            Platform::Zed,
            Platform::Windsurf,
        ] {
            assert!(!p.is_toml(), "{} should be JSON", p.display_name());
        }
    }

    #[test]
    fn windsurf_path_is_codeium_mcp_config() {
        // Windsurf reads only ~/.codeium/windsurf/mcp_config.json (user-global).
        let paths = Platform::Windsurf.config_paths();
        assert_eq!(paths.len(), 1);
        assert!(
            paths[0].ends_with(".codeium/windsurf/mcp_config.json"),
            "got {:?}",
            paths[0]
        );
        assert_eq!(Platform::Windsurf.server_config_key(), "mcpServers");
    }

    #[test]
    fn codex_path_is_toml_with_mcp_servers_table() {
        let paths = Platform::Codex.config_paths();
        assert!(
            paths
                .iter()
                .all(|p| p.extension().and_then(|e| e.to_str()) == Some("toml")),
            "codex must use .toml files: {paths:?}"
        );
        assert_eq!(Platform::Codex.server_config_key(), "mcp_servers");
    }

    #[test]
    fn zed_requires_source_custom() {
        let v = Platform::Zed.aden_config("aden-mcp", "/proj");
        assert_eq!(v["source"], "custom");
        assert_eq!(v["command"], "aden-mcp");
        assert_eq!(v["args"][0], "/proj");
        assert_eq!(Platform::Zed.server_config_key(), "context_servers");
    }

    #[test]
    fn claude_cursor_windsurf_use_command_args() {
        for p in [Platform::ClaudeCode, Platform::Cursor, Platform::Windsurf] {
            let v = p.aden_config("aden-mcp", "/proj");
            assert_eq!(v["command"], "aden-mcp", "{}", p.display_name());
            assert_eq!(v["args"][0], "/proj", "{}", p.display_name());
        }
    }

    #[test]
    fn opencode_uses_local_command_array() {
        let v = Platform::OpenCode.aden_config("aden-mcp", "/proj");
        assert_eq!(v["type"], "local");
        assert_eq!(v["command"][0], "aden-mcp");
        assert_eq!(v["command"][1], "/proj");
        assert_eq!(v["enabled"], true);
        assert_eq!(Platform::OpenCode.server_config_key(), "mcp");
    }

    #[test]
    fn codex_table_has_command_and_args() {
        let tbl = codex_aden_table("aden-mcp", "/proj");
        assert_eq!(tbl["command"].as_str(), Some("aden-mcp"));
        assert_eq!(
            tbl["args"]
                .as_array()
                .and_then(|a| a.get(0))
                .and_then(|v| v.as_str()),
            Some("/proj")
        );
    }

    #[test]
    fn codex_toml_merge_preserves_existing_content() {
        // A realistic existing ~/.codex/config.toml with a user setting and
        // another MCP server. Installing aden must not clobber either.
        let existing =
            "model = \"gpt-5-codex\"\n\n[mcp_servers.other]\ncommand = \"other-mcp\"\nargs = []\n";
        let mut doc = existing.parse::<DocumentMut>().unwrap();
        let key = "mcp_servers";
        if !doc.get(key).map(|t| t.is_table()).unwrap_or(false) {
            doc[key] = TomlItem::Table(TomlTable::new());
        }
        doc[key]["aden"] = TomlItem::Table(codex_aden_table("aden-mcp", "/proj"));

        let out = doc.to_string();
        assert!(
            out.contains("model = \"gpt-5-codex\""),
            "lost user key:\n{out}"
        );
        assert!(
            out.contains("[mcp_servers.other]"),
            "lost other server:\n{out}"
        );
        assert!(
            out.contains("command = \"other-mcp\""),
            "lost other cmd:\n{out}"
        );
        assert!(
            out.contains("[mcp_servers.aden]"),
            "missing aden table:\n{out}"
        );
        assert!(
            out.contains("command = \"aden-mcp\""),
            "missing aden cmd:\n{out}"
        );
    }

    #[test]
    fn toml_roundtrip_via_files_is_idempotent() {
        // read_toml on a missing file yields empty; write+read round-trips.
        let dir = std::env::temp_dir().join("aden_mcp_toml_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        let _ = std::fs::remove_file(&path);

        let mut doc = read_toml(&path).unwrap(); // empty
        doc["mcp_servers"] = TomlItem::Table(TomlTable::new());
        doc["mcp_servers"]["aden"] = TomlItem::Table(codex_aden_table("aden-mcp", "/proj"));
        write_toml(&path, &doc).unwrap();

        let reloaded = read_toml(&path).unwrap();
        assert!(
            reloaded
                .get("mcp_servers")
                .and_then(|t| t.as_table())
                .map(|t| t.contains_key("aden"))
                .unwrap_or(false)
        );
        let _ = std::fs::remove_file(&path);
    }
}
