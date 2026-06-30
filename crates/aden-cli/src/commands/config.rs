// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runtime configuration backed by `.aden/config.toml`.
//!
//! A small, typed key/value store so tools (and coxn) can read runtime
//! preferences through `aden config get/set` instead of hardcoding them. Keys
//! are dotted paths into nested TOML tables (e.g. `heal.window`). Values are
//! stored typed: integers and booleans are parsed, everything else is a string.
//! Edits go through `toml_edit`, preserving any existing formatting and comments.

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item};

/// The config file for a project root.
fn config_path(root: &Path) -> PathBuf {
    root.join(".aden").join("config.toml")
}

/// Load the config document, or an empty one if the file does not exist.
fn load_doc(path: &Path) -> Result<DocumentMut, Box<dyn std::error::Error>> {
    if path.exists() {
        Ok(std::fs::read_to_string(path)?.parse::<DocumentMut>()?)
    } else {
        Ok(DocumentMut::new())
    }
}

/// Coerce a string to a typed TOML value: integer, then boolean, else string.
fn typed_value(s: &str) -> toml_edit::Value {
    if let Ok(i) = s.parse::<i64>() {
        i.into()
    } else if let Ok(b) = s.parse::<bool>() {
        b.into()
    } else {
        s.into()
    }
}

/// Follow a dotted key to its item, if present.
fn get_dotted<'a>(doc: &'a DocumentMut, key: &str) -> Option<&'a Item> {
    let mut item = doc.as_item();
    for seg in key.split('.') {
        item = item.get(seg)?;
    }
    Some(item)
}

/// Set a dotted key, creating intermediate tables. Errors if an intermediate
/// segment already holds a non-table value.
fn set_dotted(doc: &mut DocumentMut, key: &str, val: &str) -> Result<(), String> {
    let segs: Vec<&str> = key.split('.').collect();
    let (last, parents) = segs.split_last().expect("split never yields empty");
    let mut tbl = doc.as_table_mut();
    for seg in parents {
        let entry = tbl.entry(seg).or_insert(toml_edit::table());
        tbl = entry
            .as_table_mut()
            .ok_or_else(|| format!("config key segment '{seg}' is not a table"))?;
    }
    tbl[last] = toml_edit::value(typed_value(val));
    Ok(())
}

/// Render a scalar item's value for `get`.
fn scalar(item: &Item) -> Option<String> {
    let v = item.as_value()?;
    Some(
        v.as_str()
            .map(str::to_string)
            .unwrap_or_else(|| v.to_string().trim().to_string()),
    )
}

/// `aden config get <key>`: print the value, or error if unset.
pub fn cmd_config_get(root: &Path, key: &str) -> Result<(), Box<dyn std::error::Error>> {
    let doc = load_doc(&config_path(root))?;
    match get_dotted(&doc, key).and_then(scalar) {
        Some(val) => {
            println!("{val}");
            Ok(())
        }
        None => Err(format!("config key '{key}' is not set").into()),
    }
}

/// `aden config set <key> <value>`: write the value to `.aden/config.toml`.
pub fn cmd_config_set(root: &Path, key: &str, val: &str) -> Result<(), Box<dyn std::error::Error>> {
    let path = config_path(root);
    let mut doc = load_doc(&path)?;
    set_dotted(&mut doc, key, val)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, doc.to_string())?;
    println!("set {key} = {val}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_then_get_dotted_roundtrips() {
        let mut doc = DocumentMut::new();
        set_dotted(&mut doc, "heal.window", "512").unwrap();
        set_dotted(&mut doc, "name", "proj").unwrap();
        assert_eq!(
            scalar(get_dotted(&doc, "heal.window").unwrap()).as_deref(),
            Some("512")
        );
        assert_eq!(
            scalar(get_dotted(&doc, "name").unwrap()).as_deref(),
            Some("proj")
        );
        assert!(get_dotted(&doc, "missing").is_none());
    }

    #[test]
    fn values_are_typed() {
        let mut doc = DocumentMut::new();
        set_dotted(&mut doc, "n", "8192").unwrap();
        set_dotted(&mut doc, "flag", "true").unwrap();
        set_dotted(&mut doc, "s", "hello").unwrap();
        // Integers and booleans serialize unquoted; strings are quoted.
        let text = doc.to_string();
        assert!(text.contains("n = 8192"), "{text}");
        assert!(text.contains("flag = true"), "{text}");
        assert!(text.contains("s = \"hello\""), "{text}");
    }

    #[test]
    fn file_roundtrip_via_commands() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        cmd_config_set(root, "heal.window", "256").unwrap();
        // Re-reading through a fresh load proves it persisted.
        let doc = load_doc(&config_path(root)).unwrap();
        assert_eq!(
            scalar(get_dotted(&doc, "heal.window").unwrap()).as_deref(),
            Some("256")
        );
        // A missing key errors.
        assert!(cmd_config_get(root, "nope").is_err());
    }
}
