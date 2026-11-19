// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Executable navigation suggestions shared by the CLI and MCP contracts.

use serde_json::{Value, json};
use std::path::Path;

fn quote(argument: &str) -> String {
    if cfg!(windows) {
        format!("'{}'", argument.replace('\'', "''"))
    } else {
        format!("'{}'", argument.replace('\'', "'\"'\"'"))
    }
}

fn action(tool: &str, arguments: Value, reason: &str, args: Vec<String>) -> Option<Value> {
    let command = format!(
        "aden {}",
        args.iter()
            .map(|arg| quote(arg))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let value = json!({
        "tool": tool,
        "arguments": arguments,
        "reason": reason,
        "cli": {"program": "aden", "args": args},
        "command": command,
        "shell": if cfg!(windows) { "powershell" } else { "posix" },
    });
    // Never make an already-bounded response unbounded through recovery text.
    (serde_json::to_vec(&value).ok()?.len() <= 4096).then_some(value)
}

pub fn inspect(anchor: &str, path: &Path, reason: &str) -> Option<Value> {
    let path = path.to_string_lossy();
    action(
        "understand",
        json!({"symbol": anchor, "path": path}),
        reason,
        vec![
            "understand".into(),
            "--".into(),
            anchor.into(),
            path.into_owned(),
        ],
    )
}

/// Incoming references to an exact source anchor, without guessing a symbol.
pub fn backlinks(anchor: &str, path: &Path, reason: &str) -> Option<Value> {
    let path = path.to_string_lossy();
    action(
        "query",
        json!({"backlinks": anchor, "depth": 1, "max_results": 16, "path": path}),
        reason,
        vec![
            "query".into(),
            "-d".into(),
            "1".into(),
            "-b".into(),
            anchor.into(),
            "--max-results".into(),
            "16".into(),
            "--".into(),
            path.into_owned(),
        ],
    )
}

/// Resolve a literal symbol name before choosing one exact anchor.
pub fn locate_symbol(symbol: &str, path: &Path, reason: &str) -> Option<Value> {
    let path = path.to_string_lossy();
    action(
        "locate",
        json!({"symbol": symbol, "path": path}),
        reason,
        vec![
            "locate".into(),
            "-s".into(),
            symbol.into(),
            "--".into(),
            path.into_owned(),
        ],
    )
}

pub fn search(pattern: &str, path: &Path, reason: &str) -> Option<Value> {
    let path = path.to_string_lossy();
    // Grep deliberately rejects likely accidental regexes in literal mode.
    // Escape such inputs so the executable follow-up still searches literally.
    let regex = pattern.contains('|')
        || pattern.contains('\\')
        || pattern.contains(".*")
        || pattern.contains(".+");
    let escaped: String = if regex {
        pattern
            .chars()
            .flat_map(|ch| {
                if "\\.^$|?*+()[]{}".contains(ch) {
                    vec!['\\', ch]
                } else {
                    vec![ch]
                }
            })
            .collect()
    } else {
        pattern.to_string()
    };
    let mut arguments = json!({"pattern": escaped, "path": path});
    let mut args = vec!["grep".into()];
    if regex {
        arguments["regex"] = true.into();
        args.push("--regex".into());
    }
    args.extend(["--".into(), escaped, path.into_owned()]);
    action("grep", arguments, reason, args)
}

pub fn search_results_page(
    query: &str,
    path: &Path,
    limit: usize,
    cursor: Option<&str>,
    doc_type: Option<&str>,
    semantics: bool,
    reason: &str,
) -> Option<Value> {
    let path = path.to_string_lossy();
    let mut arguments = json!({
        "query": query,
        "path": path,
        "limit": limit,
    });
    let mut args = vec!["search".into(), "--limit".into(), limit.to_string()];
    if let Some(cursor) = cursor {
        arguments["cursor"] = cursor.into();
        args.extend(["--cursor".into(), cursor.into()]);
    }
    if let Some(kind) = doc_type {
        arguments["doc_type"] = kind.into();
        args.extend(["--doc-type".into(), kind.into()]);
    }
    if semantics {
        arguments["semantics"] = true.into();
        args.push("--semantics".into());
    }
    args.extend(["--".into(), query.into(), path.into_owned()]);
    action("search", arguments, reason, args)
}

pub fn tree(path: &Path, reason: &str) -> Option<Value> {
    let path = path.to_string_lossy();
    action(
        "tree",
        json!({"path": path}),
        reason,
        vec!["tree".into(), "--".into(), path.into_owned()],
    )
}

pub fn generation_status(path: &Path, reason: &str) -> Option<Value> {
    let path = path.to_string_lossy();
    action(
        "generation_status",
        json!({"path": path}),
        reason,
        vec![
            "jobs".into(),
            "status".into(),
            "--json".into(),
            "--".into(),
            path.into_owned(),
        ],
    )
}

/// A file-list follow-up for `tree --files`. MCP exposes this mode as
/// `symbols: false`, while the executable CLI uses the explicit `--files` flag.
pub fn tree_files(path: &Path, reason: &str) -> Option<Value> {
    let path = path.to_string_lossy();
    action(
        "tree",
        json!({"path": path, "symbols": false}),
        reason,
        vec![
            "tree".into(),
            "--files".into(),
            "--".into(),
            path.into_owned(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_remain_data_in_commands_and_tool_calls() {
        let input = "--help 'quoted' $(echo unsafe) `unsafe`";
        let value = search(input, Path::new("a path"), "Find source").unwrap();
        assert_eq!(value["arguments"]["pattern"], input);
        assert_eq!(value["cli"]["args"][1], "--");
        assert_eq!(value["cli"]["args"][2], input);
        if cfg!(windows) {
            assert!(value["command"].as_str().unwrap().contains("''quoted''"));
        } else {
            assert!(value["command"].as_str().unwrap().contains("'\"'\"'quoted"));
        }
    }

    #[test]
    fn oversized_actions_are_omitted_without_changing_the_target() {
        assert!(inspect(&"x".repeat(4096), Path::new("."), "Inspect candidate").is_none());
    }

    #[test]
    fn file_tree_actions_preserve_cli_mode_and_mcp_arguments() {
        let path = Path::new("folder with 'quote");
        let value = tree_files(path, "Inspect narrower files").unwrap();
        assert_eq!(value["arguments"]["path"], path.to_string_lossy().as_ref());
        assert_eq!(value["arguments"]["symbols"], false);
        assert_eq!(value["cli"]["args"][0], "tree");
        assert_eq!(value["cli"]["args"][1], "--files");
        assert_eq!(value["cli"]["args"][3], path.to_string_lossy().as_ref());
        if cfg!(windows) {
            assert!(value["command"].as_str().unwrap().contains("''quote"));
        } else {
            assert!(value["command"].as_str().unwrap().contains("'\"'\"'quote"));
        }
    }

    #[test]
    fn oversized_file_tree_actions_are_omitted() {
        assert!(tree_files(Path::new(&"x".repeat(4096)), "Inspect narrower files").is_none());
    }

    #[test]
    fn regex_shaped_targets_still_search_for_the_literal_text() {
        let value = search("missing.*symbol|other\\name", Path::new("."), "Find source").unwrap();
        assert_eq!(value["arguments"]["regex"], true);
        assert_eq!(
            value["arguments"]["pattern"],
            "missing\\.\\*symbol\\|other\\\\name"
        );
        assert_eq!(value["cli"]["args"][1], "--regex");
        assert_eq!(value["cli"]["args"][2], "--");
        assert_eq!(value["cli"]["args"][3], value["arguments"]["pattern"]);
    }
}
