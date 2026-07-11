// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::FederationAction;
use serde::{Deserialize, Serialize};
use std::path::Path;

const WORKSPACE_FILE: &str = ".aden/workspace.toml";
/// Pre-TOML config path. Auto-migrated on first load, then removed.
const WORKSPACE_FILE_LEGACY: &str = ".aden/workspace.yaml";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default)]
    pub repositories: Vec<Repository>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_: Option<String>,
}

pub fn cmd_federation(action: &FederationAction) -> Result<(), Box<dyn std::error::Error>> {
    let workspace_file = Path::new(WORKSPACE_FILE);
    let mut config = load_workspace_config(workspace_file)?;

    match action {
        FederationAction::List => {
            println!("Aden Workspace Federation");
            println!("=========================");
            if config.repositories.is_empty() {
                println!("No repositories configured.");
                println!("Add one with: aden federation add <path>");
            } else {
                println!("Repositories ({}):", config.repositories.len());
                for (i, repo) in config.repositories.iter().enumerate() {
                    let path_status = if Path::new(&repo.path).exists() {
                        "✓"
                    } else {
                        "✗ (not found)"
                    };
                    println!("  {}. {} ({}) {}", i + 1, repo.name, repo.path, path_status);
                }
            }
        }
        FederationAction::Add { path, name } => {
            let path_str = path.to_string_lossy().to_string();
            let name_str = name.clone().unwrap_or_else(|| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("repo")
                    .to_string()
            });

            if config.repositories.iter().any(|r| r.path == path_str) {
                return Err("Repository already exists in workspace".into());
            }

            config.repositories.push(Repository {
                name: name_str,
                path: path_str,
                ref_: None,
            });

            save_workspace_config(workspace_file, &config)?;
            println!("Added repository to workspace.");
        }
        FederationAction::Remove { name } => {
            let initial_len = config.repositories.len();
            config.repositories.retain(|r| r.name != *name);

            if config.repositories.len() == initial_len {
                return Err(format!("Repository '{}' not found in workspace", name).into());
            }

            save_workspace_config(workspace_file, &config)?;
            println!("Removed repository '{}' from workspace.", name);
        }
        FederationAction::Config => {
            println!("Workspace Configuration");
            println!("=======================");
            if let Some(ws) = &config.workspace {
                println!("Workspace: {}", ws);
            } else {
                println!("Workspace: (not set)");
            }
            println!("Config file: {}", WORKSPACE_FILE);
            println!("Repositories: {} total", config.repositories.len());
        }
    }

    Ok(())
}

fn load_workspace_config(path: &Path) -> Result<WorkspaceConfig, Box<dyn std::error::Error>> {
    // One-time migration: if the pre-TOML `.aden/workspace.yaml` exists and the
    // TOML file does not, convert it, write TOML, and remove the legacy file.
    let legacy = Path::new(WORKSPACE_FILE_LEGACY);
    if !path.exists() && legacy.exists() {
        let config = parse_legacy_yaml(&std::fs::read_to_string(legacy)?);
        save_workspace_config(path, &config)?;
        let _ = std::fs::remove_file(legacy);
        eprintln!("aden: migrated {WORKSPACE_FILE_LEGACY} -> {WORKSPACE_FILE} (one-time).");
        return Ok(config);
    }

    if !path.exists() {
        return Ok(WorkspaceConfig::default());
    }

    let content = std::fs::read_to_string(path)?;
    let config: WorkspaceConfig =
        toml::from_str(&content).map_err(|e| format!("Invalid workspace config: {}", e))?;
    Ok(config)
}

fn save_workspace_config(
    path: &Path,
    config: &WorkspaceConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = toml::to_string(config)?;
    std::fs::write(path, content)?;
    Ok(())
}

/// Minimal reader for the legacy `serde_yaml` block format aden used to emit:
///
/// ```yaml
/// workspace: my-ws        # optional; `null`/absent => None
/// repositories:
/// - name: foo
///   path: ../foo
///   ref_: main            # `null`/absent => None
/// ```
///
/// Hand-rolled so the deprecated `serde_yaml` dependency can be dropped while
/// still auto-migrating existing files. Scoped to the exact shape
/// `save_workspace_config` previously wrote; unrecognised lines are ignored.
fn parse_legacy_yaml(s: &str) -> WorkspaceConfig {
    fn scalar(raw: &str) -> Option<String> {
        let v = raw.trim();
        if v.is_empty() || v == "null" || v == "~" {
            return None;
        }
        Some(v.trim_matches(|c| c == '"' || c == '\'').to_string())
    }

    let mut cfg = WorkspaceConfig::default();
    let mut cur: Option<Repository> = None;
    let flush = |cur: &mut Option<Repository>, cfg: &mut WorkspaceConfig| {
        if let Some(r) = cur.take()
            && (!r.name.is_empty() || !r.path.is_empty())
        {
            cfg.repositories.push(r);
        }
    };

    for raw in s.lines() {
        let line = raw.trim();
        if line.is_empty() || line == "repositories:" {
            continue;
        }
        // A `- ` prefix starts a new repository list item.
        let body = match line.strip_prefix("- ") {
            Some(rest) => {
                flush(&mut cur, &mut cfg);
                cur = Some(Repository {
                    name: String::new(),
                    path: String::new(),
                    ref_: None,
                });
                rest
            }
            None => line,
        };
        let Some((key, val)) = body.split_once(':') else {
            continue;
        };
        match (key.trim(), cur.as_mut()) {
            ("workspace", _) => cfg.workspace = scalar(val),
            ("name", Some(r)) => r.name = scalar(val).unwrap_or_default(),
            ("path", Some(r)) => r.path = scalar(val).unwrap_or_default(),
            ("ref_", Some(r)) => r.ref_ = scalar(val),
            _ => {}
        }
    }
    flush(&mut cur, &mut cfg);
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_yaml_round_trips_to_toml() {
        let yaml = "workspace: my-ws\nrepositories:\n- name: foo\n  path: ../foo\n  ref_: main\n- name: bar\n  path: /abs/bar\n  ref_: null\n";
        let cfg = parse_legacy_yaml(yaml);
        assert_eq!(cfg.workspace.as_deref(), Some("my-ws"));
        assert_eq!(cfg.repositories.len(), 2);
        assert_eq!(cfg.repositories[0].name, "foo");
        assert_eq!(cfg.repositories[0].path, "../foo");
        assert_eq!(cfg.repositories[0].ref_.as_deref(), Some("main"));
        assert_eq!(cfg.repositories[1].name, "bar");
        assert_eq!(cfg.repositories[1].ref_, None);
        // And the converted config must serialize to valid, re-parseable TOML.
        let toml_str = toml::to_string(&cfg).expect("serialize toml");
        let back: WorkspaceConfig = toml::from_str(&toml_str).expect("parse toml");
        assert_eq!(back.repositories.len(), 2);
        assert_eq!(back.workspace.as_deref(), Some("my-ws"));
    }

    #[test]
    fn empty_legacy_is_empty_config() {
        let cfg = parse_legacy_yaml("");
        assert!(cfg.workspace.is_none());
        assert!(cfg.repositories.is_empty());
    }
}
