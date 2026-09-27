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
