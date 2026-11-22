// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::{Path, PathBuf};

/// Windows `canonicalize` yields `\\?\` paths. Navigation actions are replayed
/// as CLI arguments, so the prefix has to be stripped before it is copied into
/// `arguments.path`, `cli.args`, and `command`. Non-Windows and relative paths
/// are left unchanged.
fn display_scope(path: &Path) -> PathBuf {
    aden_paths::strip_verbatim(path)
}

pub(crate) fn inspect(anchor: &str, path: &Path, reason: &str) -> Option<serde_json::Value> {
    let path = display_scope(path);
    aden_mcp::navigation::inspect(anchor, &path, reason)
}

pub(crate) fn backlinks(anchor: &str, path: &Path, reason: &str) -> Option<serde_json::Value> {
    let path = display_scope(path);
    aden_mcp::navigation::backlinks(anchor, &path, reason)
}

pub(crate) fn locate_symbol(symbol: &str, path: &Path, reason: &str) -> Option<serde_json::Value> {
    let path = display_scope(path);
    aden_mcp::navigation::locate_symbol(symbol, &path, reason)
}

pub(crate) fn search(pattern: &str, path: &Path, reason: &str) -> Option<serde_json::Value> {
    let path = display_scope(path);
    aden_mcp::navigation::search(pattern, &path, reason)
}

pub(crate) fn search_results_page(
    query: &str,
    path: &Path,
    limit: usize,
    cursor: Option<&str>,
    doc_type: Option<&str>,
    semantics: bool,
    reason: &str,
) -> Option<serde_json::Value> {
    let path = display_scope(path);
    aden_mcp::navigation::search_results_page(
        query, &path, limit, cursor, doc_type, semantics, reason,
    )
}

pub(crate) fn tree(path: &Path, reason: &str) -> Option<serde_json::Value> {
    let path = display_scope(path);
    aden_mcp::navigation::tree(&path, reason)
}

pub(crate) fn tree_files(path: &Path, reason: &str) -> Option<serde_json::Value> {
    let path = display_scope(path);
    aden_mcp::navigation::tree_files(&path, reason)
}

pub(crate) fn print(actions: &[serde_json::Value]) {
    for action in actions {
        if let (Some(reason), Some(command)) =
            (action["reason"].as_str(), action["command"].as_str())
        {
            println!(
                "Next: {}\n  {}",
                crate::util::sanitize_terminal(reason),
                crate::util::sanitize_terminal(command)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tree;
    use std::path::Path;

    fn path_text(action: &serde_json::Value) -> Vec<String> {
        let mut values = vec![action["arguments"]["path"].as_str().unwrap().to_string()];
        values.extend(
            action["cli"]["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|arg| arg.as_str().unwrap().to_string()),
        );
        values.push(action["command"].as_str().unwrap().to_string());
        values
    }

    #[test]
    fn verbatim_windows_scope_is_a_normal_path() {
        let action = tree(Path::new(r"\\?\C:\repo\src"), "Browse the repository").unwrap();
        let rendered = path_text(&action);
        assert!(
            rendered.iter().all(|value| !value.contains(r"\\?\")),
            "{rendered:?}"
        );
        assert_eq!(action["arguments"]["path"], r"C:\repo\src");
        assert_eq!(action["cli"]["args"][2], r"C:\repo\src");
        assert!(action["command"].as_str().unwrap().contains(r"C:\repo\src"));

        let relative = tree(Path::new("src/lib.rs"), "Browse the repository").unwrap();
        assert_eq!(relative["arguments"]["path"], "src/lib.rs");
        let unix = tree(Path::new("/tmp/repo"), "Browse the repository").unwrap();
        assert_eq!(unix["arguments"]["path"], "/tmp/repo");
    }
}
