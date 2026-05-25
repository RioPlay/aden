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
//! Path filter: `.adenignore` and `.adenallow` support.

use std::path::Path;

/// Smart built-in exclusion patterns that apply when no `.adenignore` exists.
pub const BUILT_IN_IGNORES: &[&str] = &[
    "target/",
    ".git/",
    ".agent/",
    ".aden/",
    "contracts/",
    "node_modules/",
    "vendor/",
    ".vscode/",
    ".idea/",
    ".cargo/",
    "*.swp",
    "*.swo",
    ".DS_Store",
    "Thumbs.db",
    "*.tmp",
    "*.temp",
];

/// A compiled path filter combining `.adenignore` and `.adenallow` rules.
#[derive(Debug, Clone)]
pub struct AdenFilter {
    ignore_patterns: Vec<GlobRule>,
    allow_patterns: Vec<GlobRule>,
}

impl AdenFilter {
    /// Load filter from project root. If `.adenignore` does not exist, use built-in defaults.
    pub fn from_directory(dir: &Path) -> Self {
        let ignore_file = dir.join(".adenignore");
        let allow_file = dir.join(".adenallow");

        let ignore_lines = if ignore_file.exists() {
            read_lines(&ignore_file).unwrap_or_default()
        } else {
            BUILT_IN_IGNORES.iter().map(|s| s.to_string()).collect()
        };

        let allow_lines = if allow_file.exists() {
            read_lines(&allow_file).unwrap_or_default()
        } else {
            Vec::new()
        };

        Self {
            ignore_patterns: compile_rules(&ignore_lines),
            allow_patterns: compile_rules(&allow_lines),
        }
    }

    /// Determine if a path relative to the project root should be skipped.
    pub fn should_skip(&self, relative: &Path) -> bool {
        let rel_str = relative.to_string_lossy();
        let matched_ignore = self.ignore_patterns.iter().any(|r| r.matches(&rel_str));
        if !matched_ignore {
            return false;
        }
        // If allowed, don't skip
        let matched_allow = self.allow_patterns.iter().any(|r| r.matches(&rel_str));
        !matched_allow
    }
}

/// A single glob-like rule. Supports `*` (any chars) and `**/` (any depth).
#[derive(Debug, Clone)]
struct GlobRule {
    pattern: String,
    is_dir_rule: bool,
}

impl GlobRule {
    fn matches(&self, path: &str) -> bool {
        if self.is_dir_rule {
            // Directory rules match both "foo/" and "foo"
            let trimmed = self.pattern.trim_end_matches('/');
            path == trimmed || path.starts_with(&(trimmed.to_string() + "/"))
        } else {
            match_glob(path, &self.pattern)
        }
    }
}

fn compile_rules(lines: &[String]) -> Vec<GlobRule> {
    lines
        .iter()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            let is_dir = trimmed.ends_with('/');
            let pat = trimmed.trim_end_matches('/').trim_start_matches('/').to_string();
            Some(GlobRule {
                pattern: pat,
                is_dir_rule: is_dir,
            })
        })
        .collect()
}

fn match_glob(path: &str, pattern: &str) -> bool {
    // Very simple glob: only handles * wildcards
    if !pattern.contains('*') {
        return path == pattern;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 2 {
        let prefix = parts[0];
        let suffix = parts[1];
        return path.starts_with(prefix) && path.ends_with(suffix) && path.len() >= prefix.len() + suffix.len();
    }
    // Fallback: exact match on base name for simple patterns
    if let Some(name) = Path::new(path).file_name().and_then(|n| n.to_str()) {
        return name == pattern.trim_start_matches("*/");
    }
    false
}

fn read_lines(path: &Path) -> std::io::Result<Vec<String>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text.lines().map(|s| s.to_string()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_skips_target() {
        let filter = AdenFilter::from_directory(Path::new("/tmp/nonexistent"));
        assert!(filter.should_skip(Path::new("target/debug/incremental")));
        assert!(filter.should_skip(Path::new(".git/hooks/pre-commit")));
        assert!(filter.should_skip(Path::new("node_modules/lodash")));
        assert!(!filter.should_skip(Path::new("src/lib.rs")));
        assert!(!filter.should_skip(Path::new("docs/context.adoc")));
    }

    #[test]
    fn test_allow_overrides_ignore() {
        let filter = AdenFilter {
            ignore_patterns: compile_rules(&["target/".to_string()]),
            allow_patterns: compile_rules(&["target/custom-tool/".to_string()]),
        };
        assert!(filter.should_skip(Path::new("target/debug")));
        assert!(!filter.should_skip(Path::new("target/custom-tool/main.rs")));
    }
}
