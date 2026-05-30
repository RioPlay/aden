// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::FederationAction;
use serde::{Deserialize, Serialize};
use std::path::Path;

const WORKSPACE_FILE: &str = ".aden/workspace.yaml";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceConfig {
    pub workspace: Option<String>,
    pub repositories: Vec<Repository>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub name: String,
    pub path: String,
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
    if !path.exists() {
        return Ok(WorkspaceConfig::default());
    }

    let content = std::fs::read_to_string(path)?;
    let config: WorkspaceConfig =
        serde_yaml::from_str(&content).map_err(|e| format!("Invalid workspace config: {}", e))?;
    Ok(config)
}

fn save_workspace_config(
    path: &Path,
    config: &WorkspaceConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = serde_yaml::to_string(config)?;
    std::fs::write(path, content)?;
    Ok(())
}
