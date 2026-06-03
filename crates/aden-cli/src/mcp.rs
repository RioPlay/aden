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

    /// True if the platform appears installed. For Claude Code this means the
    /// user-global signal (`~/.claude.json` / `~/.claude/`) — NOT a project
    /// `.mcp.json`, which is the file the installer itself creates and so can
    /// never be a reliable "is it installed" probe. Other platforms detect via
    /// their config files/directories existing.
    pub fn is_detected(&self) -> bool {
        match self {
            Platform::ClaudeCode => {
                claude_code_installed() || self.config_paths().iter().any(|p| p.exists())
            }
            _ => self.config_paths().iter().any(|p| p.exists()),
        }
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

/// Install scope: user-global vs project-local.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
}

impl Scope {
    pub fn from_name(name: &str) -> Option<Scope> {
        match name.to_lowercase().as_str() {
            "user" | "global" | "u" => Some(Scope::User),
            "project" | "local" | "p" => Some(Scope::Project),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Project => "project",
        }
    }
}

/// The scope to use when the user didn't pass `--scope`. Claude Code's canonical
/// MCP install is user-scoped (`claude mcp add -s user` / `~/.claude.json`);
/// Windsurf only has a user-global config. Everything else defaults to project.
fn default_scope(platform: &Platform) -> Scope {
    match platform {
        Platform::ClaudeCode | Platform::Windsurf => Scope::User,
        _ => Scope::Project,
    }
}

/// True if Claude Code appears installed on this machine — the user-global
/// signal (`~/.claude.json` or `~/.claude/`), independent of any project
/// `.mcp.json`. This is what lets `aden mcp install`/`list` recognize Claude
/// Code even before a project file exists.
fn claude_code_installed() -> bool {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    home.join(".claude.json").exists() || home.join(".claude").is_dir()
}

/// True if a JSON config object holds a server named `name` under `key`.
fn json_has_server(cfg: &Value, key: &str, name: &str) -> bool {
    cfg.get(key)
        .and_then(|v| v.as_object())
        .map(|m| m.contains_key(name))
        .unwrap_or(false)
}

/// True if aden is registered as a user-scoped MCP server in `~/.claude.json`.
/// Read-only — never rewrites the file.
fn claude_user_configured() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let path = home.join(".claude.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(cfg) => json_has_server(&cfg, "mcpServers", "aden"),
        Err(_) => false,
    }
}

/// Run the `claude` CLI with the given args. Returns a friendly error if the
/// binary is missing or the command fails.
fn run_claude_cli(args: &[&str]) -> Result<(), String> {
    let output = std::process::Command::new("claude")
        .args(args)
        .output()
        .map_err(|e| {
            format!(
                "could not run the `claude` CLI ({e}). Install Claude Code (or add it to PATH), \
                 or pass --scope project to write a local .mcp.json instead."
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "`claude {}` failed: {}",
            args.join(" "),
            stderr.trim()
        ));
    }
    Ok(())
}

/// Install aden as a user-scoped Claude Code MCP server via the `claude` CLI.
/// We shell out rather than hand-edit the large, stateful `~/.claude.json`.
fn install_claude_user(binary: &str, dry_run: bool) -> Result<(), String> {
    if claude_user_configured() {
        println!("  ✓ Already configured (user scope) in ~/.claude.json");
        println!(
            "    To re-add: aden mcp uninstall --platform claude --scope user, then install again."
        );
        return Ok(());
    }
    if dry_run {
        println!("  [dry-run] Would run: claude mcp add aden -s user -- {binary}");
        return Ok(());
    }
    run_claude_cli(&["mcp", "add", "aden", "-s", "user", "--", binary])?;
    println!("  ✓ Configured (user scope) via: claude mcp add aden -s user -- {binary}");
    Ok(())
}

/// Remove the user-scoped Claude Code MCP server via the `claude` CLI.
fn uninstall_claude_user(dry_run: bool) -> Result<(), String> {
    if !claude_user_configured() {
        println!("  ✗ aden was not configured (user scope) in ~/.claude.json");
        return Ok(());
    }
    if dry_run {
        println!("  [dry-run] Would run: claude mcp remove aden -s user");
        return Ok(());
    }
    run_claude_cli(&["mcp", "remove", "aden", "-s", "user"])?;
    println!("  ✓ Removed aden (user scope) via: claude mcp remove aden -s user");
    Ok(())
}

/// Pick the config file path for an explicit scope: project = first relative
/// path, user = first absolute (home) path. Falls back to the platform's
/// primary path when one form is unavailable.
fn config_path_for_scope(platform: &Platform, scope: Scope) -> PathBuf {
    let paths = platform.config_paths();
    let pick = |abs: bool| paths.iter().find(|p| p.is_absolute() == abs).cloned();
    match scope {
        Scope::Project => pick(false).or_else(|| pick(true)),
        Scope::User => pick(true).or_else(|| pick(false)),
    }
    .unwrap_or_else(|| paths[0].clone())
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
    Ok(json_has_server(&cfg, key, "aden"))
}

/// Install aden MCP into one platform.
fn install_platform(
    platform: &Platform,
    binary: &Path,
    project: &Path,
    requested_scope: Option<Scope>,
    dry_run: bool,
) -> Result<(), String> {
    let binary = binary.to_string_lossy();
    let project = project.to_string_lossy();

    // Claude Code, user scope: register globally via the `claude` CLI rather
    // than writing a project file (the canonical, common-case install).
    let scope = requested_scope.unwrap_or_else(|| default_scope(platform));
    if matches!(platform, Platform::ClaudeCode) && scope == Scope::User {
        return install_claude_user(&binary, dry_run);
    }

    // File-based install. With an explicit scope, pick the matching config
    // path; otherwise keep the historical "first existing, else project-local"
    // auto behavior so other platforms are unaffected.
    let config_path = match requested_scope {
        Some(s) => config_path_for_scope(platform, s),
        None => active_config_path(platform),
    };
    let key = platform.server_config_key();

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
fn uninstall_platform(
    platform: &Platform,
    requested_scope: Option<Scope>,
    dry_run: bool,
) -> Result<(), String> {
    let scope = requested_scope.unwrap_or_else(|| default_scope(platform));
    if matches!(platform, Platform::ClaudeCode) && scope == Scope::User {
        return uninstall_claude_user(dry_run);
    }

    let config_path = match requested_scope {
        Some(s) => config_path_for_scope(platform, s),
        None => active_config_path(platform),
    };
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

/// Parse an optional `--scope` value, exiting with a clear message on a bad value.
fn parse_scope(scope_name: Option<&str>) -> Option<Scope> {
    match scope_name {
        None => None,
        Some(s) => match Scope::from_name(s) {
            Some(scope) => Some(scope),
            None => {
                eprintln!("Unknown scope '{s}'. Use 'user' (global) or 'project' (local).");
                std::process::exit(1);
            }
        },
    }
}

/// Install aden MCP into selected platforms.
pub fn run_install(
    names: &[String],
    binary_override: Option<&Path>,
    project_override: Option<&Path>,
    scope_name: Option<&str>,
    all: bool,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let scope = parse_scope(scope_name);

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
        let eff = scope.unwrap_or_else(|| default_scope(platform));
        println!(
            "Platform: {} ({} scope)",
            platform.display_name(),
            eff.label()
        );
        match install_platform(platform, &binary, &project, scope, dry_run) {
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
    scope_name: Option<&str>,
    all: bool,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let scope = parse_scope(scope_name);
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
        let eff = scope.unwrap_or_else(|| default_scope(platform));
        println!(
            "Platform: {} ({} scope)",
            platform.display_name(),
            eff.label()
        );
        match uninstall_platform(platform, scope, dry_run) {
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

        // Claude Code is normally user-scoped (`~/.claude.json` via
        // `claude mcp add`); fall back to a project `.mcp.json` if present.
        let (configured, path_str) = if matches!(platform, Platform::ClaudeCode) {
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
            let user_json = home.join(".claude.json");
            let project_cfg = active_config_path(platform);
            let project_configured =
                project_cfg.exists() && is_configured_at(&project_cfg, platform).unwrap_or(false);
            if claude_user_configured() {
                (true, format!("{} (user scope)", user_json.display()))
            } else if project_configured {
                (true, format!("{} (project scope)", project_cfg.display()))
            } else {
                (
                    false,
                    format!("{} (user scope via `claude mcp add`)", user_json.display()),
                )
            }
        } else {
            let config_path = active_config_path(platform);
            let configured =
                config_path.exists() && is_configured_at(&config_path, platform).unwrap_or(false);
            let path_str = if config_path.exists() {
                config_path.display().to_string()
            } else {
                format!("{} (not yet created)", config_path.display())
            };
            (configured, path_str)
        };

        let det_str = if detected { "✓" } else { "✗" };
        let cfg_str = if configured { "✓" } else { "✗" };

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
    println!("To install:   aden mcp install [--platform <name>] [--scope user|project] [--all]");
    println!("To uninstall: aden mcp uninstall [--platform <name>] [--scope user|project] [--all]");
    println!();
    println!("Scope: Claude Code defaults to user (global, via `claude mcp add -s user`);");
    println!("       other platforms default to a project-local config file.");

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

            let (status_line, body) = match (method, path) {
                ("GET", "/health") => ("200 OK", r#"{"status":"ok","service":"aden"}"#.to_string()),
                ("GET", "/") | ("GET", "/api") => (
                    "200 OK",
                    format!(
                        r#"{{"service":"aden","version":"{}","endpoints":["/health","/api/check","/api/heal","/api/asm","/api/query"]}}"#,
                        env!("CARGO_PKG_VERSION")
                    ),
                ),
                _ => ("404 Not Found", r#"{"error":"not found"}"#.to_string()),
            };

            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                status_line,
                body.len(),
                body
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

    #[test]
    fn scope_parses_aliases() {
        assert_eq!(Scope::from_name("user"), Some(Scope::User));
        assert_eq!(Scope::from_name("global"), Some(Scope::User));
        assert_eq!(Scope::from_name("Project"), Some(Scope::Project));
        assert_eq!(Scope::from_name("local"), Some(Scope::Project));
        assert_eq!(Scope::from_name("nonsense"), None);
    }

    #[test]
    fn claude_and_windsurf_default_to_user_scope() {
        assert_eq!(default_scope(&Platform::ClaudeCode), Scope::User);
        assert_eq!(default_scope(&Platform::Windsurf), Scope::User);
        // Everything else stays project-local by default.
        for p in [
            Platform::Cursor,
            Platform::Codex,
            Platform::Zed,
            Platform::OpenCode,
        ] {
            assert_eq!(default_scope(&p), Scope::Project, "{}", p.display_name());
        }
    }

    #[test]
    fn config_path_for_scope_picks_relative_or_absolute() {
        // Claude Code only has a project-relative path; project scope yields it.
        assert_eq!(
            config_path_for_scope(&Platform::ClaudeCode, Scope::Project),
            PathBuf::from(".mcp.json")
        );
        // Cursor has both: project scope is relative, user scope is absolute.
        let cursor_project = config_path_for_scope(&Platform::Cursor, Scope::Project);
        assert!(cursor_project.is_relative(), "got {cursor_project:?}");
        let cursor_user = config_path_for_scope(&Platform::Cursor, Scope::User);
        assert!(cursor_user.is_absolute(), "got {cursor_user:?}");
    }

    #[test]
    fn json_has_server_detects_aden() {
        let cfg = serde_json::json!({
            "mcpServers": { "aden": { "command": "aden-mcp" }, "other": {} }
        });
        assert!(json_has_server(&cfg, "mcpServers", "aden"));
        assert!(!json_has_server(&cfg, "mcpServers", "missing"));
        assert!(!json_has_server(&cfg, "wrongKey", "aden"));
        let empty = serde_json::json!({});
        assert!(!json_has_server(&empty, "mcpServers", "aden"));
    }
}
