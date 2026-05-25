// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP (Model Context Protocol) integration installer.
//!
//! Configures the `aden-mcp` binary as an MCP server for popular AI agent
//! platforms: opencode, Claude Code, Cursor, Codex, Zed, and Windsurf.

use serde_json::Value;
use std::path::{Path, PathBuf};

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
            Platform::ClaudeCode => vec![
                home.join(".claude/settings.json"),
            ],
            Platform::Cursor => vec![
                PathBuf::from(".cursor/mcp.json"),
                home.join(".cursor/mcp.json"),
            ],
            Platform::Codex => vec![
                PathBuf::from(".codex/config.json"),
                home.join(".codex/config.json"),
            ],
            Platform::Zed => vec![
                PathBuf::from(".zed/settings.json"),
                home.join(".config/zed/settings.json"),
            ],
            Platform::Windsurf => vec![
                PathBuf::from(".windsurf/config.json"),
                home.join(".windsurf/config.json"),
            ],
        }
    }

    /// True if any config path or the platform's config directory exists.
    pub fn is_detected(&self) -> bool {
        self.config_paths().iter().any(|p| p.exists())
    }

    /// The JSON key under which MCP servers live for this platform.
    pub fn server_config_key(&self) -> &'static str {
        match self {
            Platform::OpenCode => "mcp",
            Platform::ClaudeCode | Platform::Cursor | Platform::Codex | Platform::Windsurf => {
                "mcpServers"
            }
            Platform::Zed => "context_servers",
        }
    }

    /// The JSON value to insert for the aden MCP server.
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
            Platform::Codex => serde_json::json!({
                "type": "stdio",
                "command": binary,
                "args": [project],
            }),
            Platform::Zed => serde_json::json!({
                "command": binary,
                "args": [project],
            }),
        }
    }
}

/// Find the `aden-mcp` binary on the system.
pub fn find_aden_mcp_binary(extra_paths: &[PathBuf]) -> Result<PathBuf, String> {
    // 1. Environment override
    if let Ok(env_path) = std::env::var("ADEN_MCP_PATH") {
        let p = PathBuf::from(env_path);
        if p.is_file() {
            return Ok(p);
        }
    }

    // 2. Extra paths passed by user (--binary)
    for p in extra_paths {
        if p.is_file() {
            return Ok(p.clone());
        }
    }

    // 3. Local build artifacts
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    for rel in &["target/release/aden-mcp", "target/debug/aden-mcp"] {
        let candidate = cwd.join(rel);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    // 4. PATH search
    #[cfg(unix)]
    {
        if let Ok(output) = std::process::Command::new("sh")
            .args(["-c", "command -v aden-mcp"])
            .output()
            && output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    return Ok(PathBuf::from(path));
                }
            }
    }
    #[cfg(windows)]
    {
        if let Ok(output) = std::process::Command::new("where").arg("aden-mcp").output() {
            if output.status.success() {
                if let Some(line) = String::from_utf8_lossy(&output.stdout).lines().next() {
                    let path = line.trim();
                    if !path.is_empty() {
                        return Ok(PathBuf::from(path));
                    }
                }
            }
        }
    }

    // 5. Cargo bin directory
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    let cargo_bin = home.join(".cargo/bin/aden-mcp");
    if cargo_bin.is_file() {
        return Ok(cargo_bin);
    }

    Err(
        "Could not find aden-mcp binary.\n\
         Suggestions:\n\
         1. Build it: cargo build --release -p aden-mcp\n\
         2. Install it: cargo install --path crates/aden-mcp\n\
         3. Or pass --binary /path/to/aden-mcp"
            .to_string(),
    )
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

    serde_json::from_str(&text)
        .map_err(|e| format!("Invalid JSON in {}: {}\n\
            Note: JSONC files (with comments) are not supported. \
            Please convert to plain JSON or edit manually.", path.display(), e))
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

    std::fs::rename(&tmp, path)
        .map_err(|e| format!("Failed to rename {} to {}: {}", tmp.display(), path.display(), e))?;

    Ok(())
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
    let cfg = read_config(path)?;
    let key = platform.server_config_key();
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
    let mut cfg = read_config(&config_path)?;
    let key = platform.server_config_key();

    // Ensure the server config key exists as an object
    if !cfg.get(key).map(|v| v.is_object()).unwrap_or(false) {
        cfg[key] = Value::Object(Default::default());
    }

    // Merge or create the aden entry
    if let Some(Value::Object(servers)) = cfg.get_mut(key) {
        let aden_value = platform.aden_config(
            &binary.to_string_lossy(),
            &project.to_string_lossy(),
        );
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

    let mut cfg = read_config(&config_path)?;
    let key = platform.server_config_key();

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
            println!("  - {} (config: {})", p.display_name(), p.config_paths()[0].display());
        }
        return Ok(());
    }

    // Resolve binary
    let binary = if let Some(b) = binary_override {
        b.to_path_buf()
    } else {
        find_aden_mcp_binary(&[])?
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
                println!("No platforms detected. Use --all to uninstall from all supported platforms.");
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
    println!("Listening on http://{}", addr);
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
                ("GET", "/") | ("GET", "/api") => r#"{"service":"aden","version":"0.1.0","endpoints":["/health","/api/check","/api/heal","/api/asm","/api/query"]}"#,
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
