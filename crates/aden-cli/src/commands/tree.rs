// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `aden tree` — a compact, index-aware project outline.
//!
//! Unlike the shell `tree` utility, this view follows Aden's source-discovery
//! policy, so ignored build output and vendored dependencies never bury useful
//! code. Optional symbol expansion uses stored source spans to expose lexical
//! structure beneath a script without dumping its contents.

use crate::commands::grep::{Span, load_symbol_spans};
use crate::util::{discover_indexed_relative_files_scoped, find_project_root};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

// Leave headroom for the JSON/context receipt so agent-facing responses stay
// near 32 KiB instead of consuming a large fraction of a model turn.
// Agent session-starter bound. A whole-repo outline used to dump ~28 KiB /
// 4k symbols into the first `tree` call; that is a map, not a dump, only
// when it stays small. `--unlimited` or a subtree path remains the escape.
const DEFAULT_OUTLINE_BYTES: usize = 12 * 1024;
const DEFAULT_OUTLINE_SYMBOLS: usize = 768;

#[derive(Clone)]
struct Symbol {
    name: String,
    start: usize,
    end: usize,
    is_code: bool,
}

/// Return project-relative indexed paths for a canonical file or directory.
fn discover_tree_scope_files(
    root: &Path,
    scope: &Path,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    if scope.is_dir() {
        return discover_indexed_relative_files_scoped(scope, root);
    }
    if !scope.is_file() {
        return Ok(Vec::new());
    }
    let Some(relative) = aden_paths::relative_to(root, scope) else {
        return Ok(Vec::new());
    };
    let filter = aden_core::filter::AdenFilter::from_directory(root);
    if relative
        .ancestors()
        .any(|path| !path.as_os_str().is_empty() && filter.should_skip(path))
    {
        return Ok(Vec::new());
    }
    let supported = aden_parse::supported_extensions();
    let parseable = crate::util::is_supported_source_file(scope, |ext| supported.contains(&ext));
    let indexed =
        aden_core::filter::FileDisposition::for_path(&relative, false, parseable).is_indexed();
    Ok(indexed.then_some(relative).into_iter().collect())
}

pub fn cmd_tree(
    path: &Path,
    depth: usize,
    symbol_depth: usize,
    symbols_only: bool,
    files_only: bool,
    unlimited: bool,
    json_output: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let root = find_project_root(if path.is_file() {
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
    } else {
        path
    });
    let scope = std::fs::canonicalize(path)
        .map_err(|error| format!("Cannot inspect '{}': {error}", path.display()))?;
    // Windows: git root is often `C:/…` while canonicalize yields `\\?\C:\…`.
    // One RelativeRoot covers that comparison; `is_under` plus `relative_to`
    // would canonicalize both paths a second time.
    let relative_root = aden_paths::RelativeRoot::new(&root);
    let Some(relative_scope) = relative_root.relative_to(&scope) else {
        return Err(format!("Tree scope '{}' is outside project root", path.display()).into());
    };
    super::ensure_fresh(&root);
    let mut files = discover_tree_scope_files(&root, &scope)?;
    if files_only {
        files.sort_unstable();
        files.dedup();
        let outline = files_outline(&files, unlimited);
        let next_actions = if outline.truncated && scope.is_dir() {
            let top_dirs: BTreeSet<_> = files
                .iter()
                .filter_map(|file| file.strip_prefix(&relative_scope).ok())
                .filter_map(|file| {
                    let mut components = file.components();
                    let first = components.next()?;
                    components.next().map(|_| first.as_os_str())
                })
                .collect();
            top_dirs
                .into_iter()
                .take(3)
                .filter_map(|dir| super::next_actions::tree_files(&root.join(&relative_scope).join(dir), "Inspect this narrower file scope; other scopes may also contain omitted results"))
                .collect::<Vec<_>>()
        } else if files.is_empty() && !relative_scope.as_os_str().is_empty() {
            super::next_actions::tree_files(&root, "Browse the repository for discovered files")
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        if json_output {
            let payload = super::augment_read_json(
                &root,
                serde_json::json!({
                    "schema_version": 1,
                    "result_state": if outline.truncated { "truncated" } else { "complete" },
                    "format": outline.format,
                    "scope": if relative_scope.as_os_str().is_empty() { ".".into() } else { relative_scope.to_string_lossy().replace('\\', "/") },
                    "file_count": outline.file_count,
                    "symbol_count": 0,
                    "returned_file_count": outline.returned_file_count,
                    "returned_symbol_count": 0,
                    "truncated": outline.truncated,
                    "next_actions": next_actions,
                    "next_action": outline.truncated.then_some("Rerun tree --files on a project-relative subtree, or use --unlimited explicitly."),
                    "outline": outline.text,
                }),
            );
            crate::read_worker::emit_machine_body(serde_json::to_string(&payload)?);
        } else {
            print!("{}", outline.text);
            super::next_actions::print(&next_actions);
        }
        return Ok(());
    }

    let symbols_only = symbols_only || json_output;
    let spans = load_symbol_spans(&root);
    let mut by_file = BTreeMap::new();
    let mut dirs = BTreeSet::new();
    for relative in files {
        if !symbols_only {
            let parent = relative.parent().unwrap_or(Path::new(""));
            let mut current = PathBuf::new();
            for component in parent.components() {
                current.push(component);
                dirs.insert(current.clone());
            }
        }
        let key = relative.to_string_lossy().replace('\\', "/");
        let symbols = spans
            .get(&key)
            .map(|items| symbols_for(items))
            .unwrap_or_default();
        by_file.insert(relative, symbols);
    }

    // JSON is the CLI default and the only useful agent view. The graphical
    // directory tree is `--human` (and `--human --symbols` for a text outline).
    // Without this, `aden tree` dumps box-drawing art at an LLM even though
    // `-j` / "JSON is the default" is advertised globally.
    if symbols_only {
        let outline = symbol_outline(&by_file, unlimited);
        let next_actions: Vec<_> = if outline.truncated && scope.is_dir() {
            // Follow the same code-file filter as the outline. Config and prose
            // paths sort before source in many projects but cannot reveal the
            // symbol names omitted by a compact file map.
            by_file.iter()
                .filter(|(file, symbols)| !is_prose_file(file) && symbols.iter().any(|symbol| symbol.is_code))
                .filter_map(|(file, _)| file.strip_prefix(&relative_scope).ok())
                .filter_map(|file| file.components().next())
                .map(|component| root.join(&relative_scope).join(component.as_os_str()))
                .collect::<BTreeSet<_>>().into_iter().take(3)
                .filter_map(|scope| super::next_actions::tree(&scope, "Inspect this narrower scope; other scopes may also contain omitted results"))
                .collect()
        } else if by_file.is_empty() && !relative_scope.as_os_str().is_empty() {
            super::next_actions::tree(&root, "Browse the repository for indexed source files")
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        if json_output {
            let payload = super::augment_read_json(
                &root,
                serde_json::json!({
                    "schema_version": 1,
                    "result_state": if outline.truncated { "truncated" } else { "complete" },
                    "format": outline.format,
                    "scope": if relative_scope.as_os_str().is_empty() { ".".into() } else { relative_scope.to_string_lossy().replace('\\', "/") },
                    "file_count": outline.file_count,
                    "symbol_count": outline.symbol_count,
                    "returned_file_count": outline.returned_file_count,
                    "returned_symbol_count": outline.returned_symbol_count,
                    "truncated": outline.truncated,
                    "next_actions": next_actions,
                    "next_action": outline.truncated.then_some(
                        if outline.format == "file-map-v1" {
                            "Rerun tree on a project-relative subtree for symbol names and line ranges, or use --unlimited."
                        } else {
                            "Rerun tree --symbols on a project-relative subtree, or use --unlimited explicitly."
                        }
                    ),
                    "outline": outline.text,
                }),
            );
            crate::read_worker::emit_machine_body(serde_json::to_string(&payload)?);
        } else {
            print!("{}", outline.text);
            super::next_actions::print(&next_actions);
        }
        return Ok(());
    }

    let label = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project");
    println!("{label}/  ({} indexed files)", by_file.len());
    render_dir(Path::new(""), 0, depth, symbol_depth, &dirs, &by_file, "");
    Ok(())
}

struct SymbolOutline {
    text: String,
    format: &'static str,
    file_count: usize,
    symbol_count: usize,
    returned_file_count: usize,
    returned_symbol_count: usize,
    truncated: bool,
}

fn files_outline(files: &[PathBuf], unlimited: bool) -> SymbolOutline {
    use std::fmt::Write as _;

    let byte_limit = if unlimited {
        usize::MAX
    } else {
        DEFAULT_OUTLINE_BYTES.saturating_sub(160)
    };
    let mut body = String::new();
    let mut returned_file_count = 0;
    for file in files {
        let line = file.to_string_lossy().replace('\\', "/");
        if body.len().saturating_add(line.len()).saturating_add(1) > byte_limit {
            break;
        }
        body.push_str(&line);
        body.push('\n');
        returned_file_count += 1;
    }
    let truncated = returned_file_count < files.len();
    let mut text = String::new();
    if truncated {
        let _ = writeln!(
            text,
            "# showing {returned_file_count} of {} discovered files (clipped; use --unlimited or a narrower subtree)",
            files.len()
        );
    } else {
        let _ = writeln!(text, "# {} discovered files", files.len());
    }
    text.push_str(&body);
    SymbolOutline {
        text,
        format: "files-v1",
        file_count: files.len(),
        symbol_count: 0,
        returned_file_count,
        returned_symbol_count: 0,
        truncated,
    }
}

/// Token-lean project map. Grouping by file avoids repeating long paths; exact
/// anchor fragments remain copy/pasteable for `locate`, while line ranges let
/// an agent read only the affected source span. Normal output has a hard bound;
/// callers can scope to a subtree or opt into `--unlimited` for enormous repos.
fn symbol_outline(files: &BTreeMap<PathBuf, Vec<Symbol>>, unlimited: bool) -> SymbolOutline {
    use std::fmt::Write as _;

    let code_files: Vec<_> = files
        .iter()
        .filter(|(file, symbols)| {
            !is_prose_file(file) && symbols.iter().any(|symbol| symbol.is_code)
        })
        .collect();
    let symbol_count: usize = code_files
        .iter()
        .map(|(_, symbols)| symbols.iter().filter(|symbol| symbol.is_code).count())
        .sum();
    let byte_limit = if unlimited {
        usize::MAX
    } else {
        DEFAULT_OUTLINE_BYTES.saturating_sub(160)
    };
    let symbol_limit = if unlimited {
        usize::MAX
    } else {
        DEFAULT_OUTLINE_SYMBOLS
    };
    // A truncated name dump is not a map. When the symbol list will not fit,
    // emit file + count only so the first call stays orienting.
    if !unlimited && symbols_would_exceed(&code_files, symbol_count, byte_limit, symbol_limit) {
        return file_map_outline(&code_files, symbol_count, byte_limit);
    }
    let mut body = String::new();
    let mut returned_file_count = 0;
    let mut returned_symbol_count = 0;
    let mut truncated = false;

    'files: for (file, symbols) in &code_files {
        let file_line = format!("{}:\n", file.to_string_lossy().replace('\\', "/"));
        let body_before_file = body.len();
        if body.len().saturating_add(file_line.len()) > byte_limit {
            truncated = true;
            break;
        }
        body.push_str(&file_line);
        let mut included_from_file = false;
        for symbol in symbols.iter().filter(|symbol| symbol.is_code) {
            if returned_symbol_count >= symbol_limit {
                if !included_from_file {
                    body.truncate(body_before_file);
                }
                truncated = true;
                break 'files;
            }
            let line = format!("{}-{} {}\n", symbol.start, symbol.end, symbol.name);
            if body.len().saturating_add(line.len()) > byte_limit {
                if !included_from_file {
                    body.truncate(body_before_file);
                }
                truncated = true;
                break 'files;
            }
            body.push_str(&line);
            returned_symbol_count += 1;
            if !included_from_file {
                included_from_file = true;
                returned_file_count += 1;
            }
        }
    }

    truncated |= returned_symbol_count < symbol_count;
    let mut text = String::new();
    if truncated {
        let _ = writeln!(
            text,
            "# showing {returned_file_count} of {} code files; {returned_symbol_count} of {symbol_count} symbols (truncated)",
            code_files.len()
        );
    } else {
        let _ = writeln!(
            text,
            "# {} code files; {} symbols",
            code_files.len(),
            symbol_count
        );
    }
    text.push_str(&body);

    SymbolOutline {
        text,
        format: "symbol-outline-v1",
        file_count: code_files.len(),
        symbol_count,
        returned_file_count,
        returned_symbol_count,
        truncated,
    }
}

fn symbols_would_exceed(
    code_files: &[(&PathBuf, &Vec<Symbol>)],
    symbol_count: usize,
    byte_limit: usize,
    symbol_limit: usize,
) -> bool {
    if symbol_count > symbol_limit {
        return true;
    }
    let mut estimated = 80usize;
    for (file, symbols) in code_files {
        estimated = estimated.saturating_add(file.to_string_lossy().len() + 2);
        for symbol in symbols.iter().filter(|symbol| symbol.is_code) {
            estimated = estimated.saturating_add(8 + symbol.name.len());
            if estimated > byte_limit {
                return true;
            }
        }
    }
    false
}

fn file_map_outline(
    code_files: &[(&PathBuf, &Vec<Symbol>)],
    symbol_count: usize,
    byte_limit: usize,
) -> SymbolOutline {
    use std::fmt::Write as _;

    let mut body = String::new();
    let mut returned_file_count = 0;
    for (file, symbols) in code_files {
        let count = symbols.iter().filter(|symbol| symbol.is_code).count();
        let line = format!("{}  ({count})\n", file.to_string_lossy().replace('\\', "/"));
        if body.len().saturating_add(line.len()) > byte_limit {
            break;
        }
        body.push_str(&line);
        returned_file_count += 1;
    }
    let mut text = String::new();
    let _ = writeln!(
        text,
        "# {} code files; {symbol_count} symbols (file map — pass a subtree for names)",
        code_files.len()
    );
    text.push_str(&body);
    SymbolOutline {
        text,
        format: "file-map-v1",
        file_count: code_files.len(),
        symbol_count,
        returned_file_count,
        returned_symbol_count: 0,
        truncated: true,
    }
}

fn is_prose_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "mdx" | "adoc" | "aden" | "rst" | "txt"
            )
        })
}

fn symbols_for(spans: &[Span]) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = spans
        .iter()
        .map(|span| Symbol {
            name: short_anchor(&span.anchor),
            start: span.start,
            end: span.end,
            is_code: span.anchor.starts_with("aden://module/"),
        })
        .collect();
    out.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then(b.end.cmp(&a.end))
            .then(a.name.cmp(&b.name))
    });
    out.dedup_by(|a, b| a.name == b.name && a.start == b.start && a.end == b.end);
    out
}

fn short_anchor(anchor: &str) -> String {
    anchor
        .rsplit('#')
        .next()
        .unwrap_or(anchor)
        .rsplit('/')
        .next()
        .unwrap_or(anchor)
        .to_string()
}

#[allow(clippy::too_many_arguments)]
fn render_dir(
    dir: &Path,
    level: usize,
    max_depth: usize,
    symbol_depth: usize,
    dirs: &BTreeSet<PathBuf>,
    files: &BTreeMap<PathBuf, Vec<Symbol>>,
    prefix: &str,
) {
    let mut entries: Vec<(PathBuf, bool)> = dirs
        .iter()
        .filter(|candidate| candidate.parent() == Some(dir))
        .cloned()
        .map(|path| (path, true))
        .chain(
            files
                .keys()
                .filter(|candidate| candidate.parent() == Some(dir))
                .cloned()
                .map(|path| (path, false)),
        )
        .collect();
    entries.sort();

    for (index, (entry, is_dir)) in entries.iter().enumerate() {
        let last = index + 1 == entries.len();
        let branch = if last { "└─ " } else { "├─ " };
        let continuation = if last { "   " } else { "│  " };
        let name = entry.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        if *is_dir {
            let descendants = files.keys().filter(|file| file.starts_with(entry)).count();
            if level >= max_depth {
                println!("{prefix}{branch}{name}/  ({descendants} indexed files; depth limit)");
            } else {
                println!("{prefix}{branch}{name}/");
                render_dir(
                    entry,
                    level + 1,
                    max_depth,
                    symbol_depth,
                    dirs,
                    files,
                    &format!("{prefix}{continuation}"),
                );
            }
        } else {
            let symbols = &files[entry];
            let suffix = if symbols.is_empty() {
                String::new()
            } else {
                format!("  ({} symbols)", symbols.len())
            };
            println!("{prefix}{branch}{name}{suffix}");
            if symbol_depth > 0 {
                render_symbols(symbols, symbol_depth, &format!("{prefix}{continuation}"));
            }
        }
    }
}

fn render_symbols(symbols: &[Symbol], max_depth: usize, prefix: &str) {
    let mut stack: Vec<usize> = Vec::new();
    for (index, symbol) in symbols.iter().enumerate() {
        while let Some(parent) = stack.last() {
            let enclosing = &symbols[*parent];
            if symbol.start >= enclosing.start && symbol.end <= enclosing.end {
                break;
            }
            stack.pop();
        }
        let depth = stack.len() + 1;
        if depth <= max_depth {
            println!(
                "{prefix}└─ {}  (lines {}–{})",
                symbol.name, symbol.start, symbol.end
            );
        }
        stack.push(index);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Symbol, discover_tree_scope_files, is_prose_file, short_anchor, symbol_outline, symbols_for,
    };
    use crate::commands::grep::Span;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    #[test]
    fn scope_under_root_accepts_verbatim_vs_plain() {
        // Mirrors the Windows product failure: git root vs canonicalize spelling.
        assert!(aden_paths::is_under(
            Path::new(r"C:\Users\me\proj"),
            Path::new(r"\\?\C:\Users\me\proj"),
        ));
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("crates").join("cli");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(aden_paths::is_under(dir.path(), dir.path()));
        assert!(aden_paths::is_under(dir.path(), &nested));
    }

    #[test]
    fn scoped_tree_discovery_matches_files_and_prunes_ignored_ancestors() {
        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join("src");
        let ignored_dir = dir.path().join("ignored");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&ignored_dir).unwrap();
        let source_file = source_dir.join("main.rs");
        let ignored_file = ignored_dir.join("secret.rs");
        let unsupported_file = source_dir.join("payload.bin");
        std::fs::write(&source_file, "fn main() {}\n").unwrap();
        std::fs::write(&ignored_file, "fn hidden() {}\n").unwrap();
        std::fs::write(&unsupported_file, [0, 1, 2]).unwrap();
        std::fs::write(dir.path().join(".adenignore"), "ignored/\n").unwrap();

        let folder_files = discover_tree_scope_files(dir.path(), &source_dir).unwrap();
        let single_file = discover_tree_scope_files(dir.path(), &source_file).unwrap();
        let ignored_scope = discover_tree_scope_files(dir.path(), &ignored_dir).unwrap();
        let ignored_file_scope = discover_tree_scope_files(dir.path(), &ignored_file).unwrap();
        let unsupported_file_scope =
            discover_tree_scope_files(dir.path(), &unsupported_file).unwrap();

        assert_eq!(folder_files, vec![Path::new("src").join("main.rs")]);
        assert_eq!(single_file, folder_files);
        assert!(ignored_scope.is_empty());
        assert!(ignored_file_scope.is_empty());
        assert!(unsupported_file_scope.is_empty());
    }

    #[test]
    fn outline_keeps_exact_copy_pasteable_symbol_names() {
        assert_eq!(short_anchor("aden://module/app#run_server"), "run_server");
        assert_eq!(
            short_anchor("aden://doc/app/README.adoc#h2getting-started"),
            "h2getting-started"
        );
    }

    #[test]
    fn compact_outline_excludes_prose_contracts() {
        assert!(is_prose_file(Path::new("README.md")));
        assert!(is_prose_file(Path::new("docs/guide.adoc")));
        assert!(!is_prose_file(Path::new("src/main.rs")));
        assert!(!is_prose_file(Path::new("cmd/main.go")));
    }

    #[test]
    fn outline_lookup_key_matches_nested_windows_style_paths() {
        // tree joins spans with `/`-normalized keys; load_symbol_spans must
        // produce the same form or nested files (src\lib.rs) vanish on Windows.
        let mut files = BTreeMap::new();
        files.insert(
            PathBuf::from("src").join("lib.rs"),
            vec![Symbol {
                name: "included".into(),
                start: 1,
                end: 1,
                is_code: true,
            }],
        );
        let outline = symbol_outline(&files, true);
        assert_eq!(outline.file_count, 1);
        assert!(outline.text.contains("src/lib.rs:"), "{}", outline.text);
        assert!(outline.text.contains("included"), "{}", outline.text);
    }

    #[test]
    fn symbols_sort_enclosing_spans_before_children() {
        let symbols = symbols_for(&[
            Span {
                anchor: "a#child".into(),
                start: 3,
                end: 4,
            },
            Span {
                anchor: "a#parent".into(),
                start: 1,
                end: 8,
            },
        ]);
        assert_eq!(symbols[0].name, "parent");
        assert_eq!(symbols[1].name, "child");
    }

    #[test]
    fn compact_outline_is_bounded_unless_unlimited() {
        let mut files = BTreeMap::new();
        files.insert(
            PathBuf::from("src/large.rs"),
            (0..4_100)
                .map(|line| Symbol {
                    name: format!("symbol_{line}"),
                    start: line + 1,
                    end: line + 1,
                    is_code: true,
                })
                .collect(),
        );

        let bounded = symbol_outline(&files, false);
        assert!(bounded.truncated);
        assert_eq!(bounded.format, "file-map-v1");
        assert_eq!(bounded.symbol_count, 4_100);
        assert_eq!(bounded.returned_symbol_count, 0);
        assert!(
            bounded.text.contains("src/large.rs  (4100)"),
            "{}",
            bounded.text
        );
        assert!(bounded.text.contains("file map"), "{}", bounded.text);
        assert!(bounded.text.len() <= 12 * 1024);

        let full = symbol_outline(&files, true);
        assert!(!full.truncated);
        assert_eq!(full.returned_symbol_count, 4_100);
    }
}
