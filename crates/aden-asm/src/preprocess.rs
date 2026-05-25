// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// Original author and maintainer: RioPlay
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
use regex::Regex;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

static INCLUDE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^include::([^\[]+)\[(.*)\]\s*$").expect("static regex should compile")
});
static IFDEF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^ifdef::([^\[]+)\[\]\s*$").expect("static regex should compile")
});
static IFNDEF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^ifndef::([^\[]+)\[\]\s*$").expect("static regex should compile")
});
static IFEVAL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^ifeval::\[([^\]]+)\]\s*$").expect("static regex should compile")
});
static ENDIF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^endif::\[\]\s*$").expect("static regex should compile")
});
static HEADING_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(=+)\s+(.+)$").expect("static regex should compile")
});
static INLINE_ATTR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{([^}]+)\}").expect("static regex should compile")
});

#[derive(Debug, thiserror::Error)]
pub enum PreprocessError {
    #[error("IO error on {path}: {message}")]
    Io { path: String, message: String },
    #[error("recursive include detected: {path}")]
    RecursiveInclude { path: String },
    #[error("invalid line range: {spec}")]
    InvalidLineRange { spec: String },
    #[error("missing include file: {path}")]
    MissingInclude { path: String },
    #[error("include path escapes trusted root: {path}")]
    PathTraversal { path: String },
}

/// Preprocess a single AsciiDoc file, resolving `include::[]` directives
/// and evaluating conditionals. Returns the flat AsciiDoc text.
pub fn preprocess(
    path: &Path,
    attrs: &HashMap<String, String>,
    visited: &mut Vec<PathBuf>,
    level_offset: usize,
) -> Result<String, PreprocessError> {
    let canon = path.canonicalize().map_err(|e| PreprocessError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;

    if visited.contains(&canon) {
        return Err(PreprocessError::RecursiveInclude {
            path: path.display().to_string(),
        });
    }

    let mut file = std::fs::File::open(path).map_err(|e| PreprocessError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    let mut raw = String::new();
    file.read_to_string(&mut raw).map_err(|e| PreprocessError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;

    // Establish a security boundary: includes may not escape the directory
    // containing the *original* document being preprocessed.
    let base_dir = canon
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_default()
                .canonicalize()
                .unwrap_or_default()
        });

    visited.push(canon.clone());
    let result = process_text(&raw, path, &base_dir, attrs, visited, level_offset);
    visited.pop();
    result
}

fn process_text(
    raw: &str,
    current_path: &Path,
    base_dir: &Path,
    attrs: &HashMap<String, String>,
    _visited: &mut Vec<PathBuf>,
    level_offset: usize,
) -> Result<String, PreprocessError> {
    let local_attrs = attrs.clone();
    let mut output = Vec::new();
    let mut skip_depth = 0usize; // number of nested conditionals to skip
    let mut skip_stack = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();

        // Conditionals
        if let Some(cap) = IFDEF_RE.captures(trimmed) {
            let attr = cap[1].to_string();
            let active = local_attrs.contains_key(&attr);
            if active {
                skip_stack.push(false);
            } else {
                skip_depth += 1;
                skip_stack.push(true);
            }
            continue;
        }
        if let Some(cap) = IFNDEF_RE.captures(trimmed) {
            let attr = cap[1].to_string();
            let active = !local_attrs.contains_key(&attr);
            if active {
                skip_stack.push(false);
            } else {
                skip_depth += 1;
                skip_stack.push(true);
            }
            continue;
        }
        if let Some(cap) = IFEVAL_RE.captures(trimmed) {
            let expr = cap[1].to_string();
            let active = eval_ifeval(&expr, &local_attrs);
            if active {
                skip_stack.push(false);
            } else {
                skip_depth += 1;
                skip_stack.push(true);
            }
            continue;
        }
        if ENDIF_RE.is_match(trimmed) {
            if let Some(was_skipping) = skip_stack.pop()
                && was_skipping {
                    skip_depth = skip_depth.saturating_sub(1);
                }
            continue;
        }

        if skip_depth > 0 {
            continue;
        }

        // Includes
        if let Some(cap) = INCLUDE_RE.captures(trimmed) {
            let rel_path = cap[1].trim().to_string();
            let attr_str = cap[2].trim();
            let base = current_path.parent().unwrap_or(Path::new("."));
            let inc_path = base.join(&rel_path);

            if !inc_path.exists() {
                return Err(PreprocessError::MissingInclude {
                    path: inc_path.display().to_string(),
                });
            }

            let inc_canon = inc_path.canonicalize().map_err(|e| PreprocessError::Io {
                path: inc_path.display().to_string(),
                message: e.to_string(),
            })?;

            if !inc_canon.starts_with(base_dir) {
                return Err(PreprocessError::PathTraversal {
                    path: inc_path.display().to_string(),
                });
            }

            let mut tag = None;
            let mut lines_spec: Option<String> = None;
            let mut new_leveloff = 0i32;
            for part in attr_str.split(';') {
                let part = part.trim();
                if let Some(v) = part.strip_prefix("tags=") {
                    tag = Some(v.trim_matches('"').to_string());
                } else if let Some(v) = part.strip_prefix("lines=") {
                    lines_spec = Some(v.trim_matches('"').to_string());
                } else if let Some(v) = part.strip_prefix("leveloffset=")
                    && let Ok(n) = v.trim_matches('"').parse::<i32>() {
                        new_leveloff = n;
                    }
            }

            let mut inc_text = std::fs::read_to_string(&inc_canon).map_err(|e| PreprocessError::Io {
                path: inc_canon.display().to_string(),
                message: e.to_string(),
            })?;

            // Apply lines filter
            if let Some(spec) = lines_spec {
                inc_text = filter_lines(&inc_text, &spec)?;
            }

            // Apply tags filter
            if let Some(t) = tag {
                inc_text = filter_tags(&inc_text, &t)?;
            }

            // Recursively preprocess
            let sub = process_text(
                &inc_text,
                &inc_canon,
                base_dir,
                &local_attrs,
                _visited,
                (level_offset as i32 + new_leveloff).max(0) as usize,
            )?;

            // Apply level offset to headings
            let adjusted = adjust_headings(&sub, level_offset as i32 + new_leveloff);
            output.push(adjusted);
            continue;
        }

        // Apply level offset to headings in current document too
        if let Some(cap) = HEADING_RE.captures(trimmed) {
            let level = cap[1].len();
            let new_level = (level + level_offset).min(6);
            let title = &cap[2];
            let prefix = "=".repeat(new_level);
            output.push(format!("{prefix} {title}"));
            continue;
        }

        // Resolve inline attribute references {key}
        let resolved = resolve_inline_attrs(line, &local_attrs);
        output.push(resolved);
    }

    Ok(output.join("\n"))
}

fn eval_ifeval(expr: &str, attrs: &HashMap<String, String>) -> bool {
    let trimmed = expr.trim();
    let operators = ["<=", ">=", "==", "!=", "<", ">"];
    for op in &operators {
        if let Some(pos) = trimmed.find(op) {
            let left = trimmed[..pos].trim();
            let right = trimmed[pos + op.len()..].trim();
            let left_val = if left.starts_with('{') && left.ends_with('}') {
                let attr_name = &left[1..left.len() - 1];
                attrs.get(attr_name).map(|s| s.as_str()).unwrap_or("")
            } else {
                left
            };
            let right = right.trim_matches('"');
            return match *op {
                "==" => left_val == right,
                "!=" => left_val != right,
                "<" | ">" | "<=" | ">=" => {
                    let l_num = left_val.parse::<f64>();
                    let r_num = right.parse::<f64>();
                    match (l_num, r_num) {
                        (Ok(l), Ok(r)) => match *op {
                            "<" => l < r,
                            ">" => l > r,
                            "<=" => l <= r,
                            ">=" => l >= r,
                            _ => false,
                        },
                        _ => false,
                    }
                }
                _ => false,
            };
        }
    }
    false
}

fn filter_lines(text: &str, spec: &str) -> Result<String, PreprocessError> {
    let lines: Vec<&str> = text.lines().collect();
    let mut result = Vec::new();
    for range_str in spec.split(',') {
        let range_str = range_str.trim();
        if let Some(pos) = range_str.find("..") {
            let start_str = range_str[..pos].trim();
            let end_str = range_str[pos + 2..].trim();
            let start: usize = start_str.parse().map_err(|_| PreprocessError::InvalidLineRange { spec: spec.to_string() })?;
            let end: usize = if end_str.is_empty() {
                lines.len()
            } else {
                end_str.parse().map_err(|_| PreprocessError::InvalidLineRange { spec: spec.to_string() })?
            };
            let start_idx = start.saturating_sub(1);
            let end_idx = end.min(lines.len());
            for line in &lines[start_idx..end_idx] {
                result.push(line.to_string());
            }
        } else {
            let n: usize = range_str.parse().map_err(|_| PreprocessError::InvalidLineRange { spec: spec.to_string() })?;
            if n > 0 && n <= lines.len() {
                result.push(lines[n - 1].to_string());
            }
        }
    }
    Ok(result.join("\n"))
}

fn filter_tags(text: &str, tag_name: &str) -> Result<String, PreprocessError> {
    let start_tag = format!("// tag::{tag_name}[]");
    let end_tag = format!("// end::{tag_name}[]");
    let mut result = Vec::new();
    let mut in_tag = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == start_tag {
            in_tag = true;
            continue;
        }
        if trimmed == end_tag {
            in_tag = false;
            continue;
        }
        if in_tag {
            result.push(line.to_string());
        }
    }
    Ok(result.join("\n"))
}

fn adjust_headings(text: &str, offset: i32) -> String {
    text.lines()
        .map(|line| {
            let trimmed = line.trim();
            if let Some(cap) = HEADING_RE.captures(trimmed) {
                let level = cap[1].len();
                let new_level = (level as i32 + offset).clamp(1, 6) as usize;
                let title = &cap[2];
                format!("{} {}", "=".repeat(new_level), title)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn resolve_inline_attrs(line: &str, attrs: &HashMap<String, String>) -> String {
    let mut result = line.to_string();
    for cap in INLINE_ATTR_RE.captures_iter(line) {
        let key = &cap[1];
        if let Some(val) = attrs.get(key) {
            result = result.replace(&cap[0], val);
        }
    }
    result
}
