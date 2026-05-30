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
///
/// These defaults are deliberately *polyglot*: Aden compiles context for any
/// codebase, so we exclude the build-output and dependency directories of every
/// common ecosystem — not just the one Aden itself is written in. Anything
/// project-specific belongs in a user-authored `.adenignore`, never here.
pub const BUILT_IN_IGNORES: &[&str] = &[
    // Aden's own artifacts and version control
    ".git/",
    ".hg/",
    ".svn/",
    ".agent/",
    ".aden/",
    // NOTE: do NOT hard-code "contracts/" here — it is aden's *own* output dir
    // name, but it is also *source* in other ecosystems (Solidity/Vyper smart
    // contracts live in `contracts/`). aden's actual output lives under `.aden/`
    // (already skipped). Skipping bare `contracts/` would silently drop the
    // entire codebase of any smart-contract repo — an aden-centric assumption
    // that breaks language-agnosticism.
    // Editor / OS debris
    ".vscode/",
    ".idea/",
    ".vs/",
    "*.swp",
    "*.swo",
    ".DS_Store",
    "Thumbs.db",
    "*.tmp",
    "*.temp",
    // Generic build output (shared across many ecosystems).
    // NOTE: deliberately NOT excluding "bin/" or "packages/" — they are build
    // output in some ecosystems but *source* in others (e.g. `packages/<name>`
    // in npm/pnpm/lerna monorepos, which discovery treats as modules).
    "build/",
    "dist/",
    "out/",
    "obj/",
    // Rust / Cargo
    "target/",
    ".cargo/",
    // Node / JS / TS
    "node_modules/",
    ".next/",
    ".nuxt/",
    ".svelte-kit/",
    "bower_components/",
    // Python
    "__pycache__/",
    ".venv/",
    "venv/",
    ".tox/",
    ".mypy_cache/",
    ".pytest_cache/",
    ".ruff_cache/",
    "*.egg-info/",
    // Go / vendored dependencies
    "vendor/",
    // JVM (Gradle / Maven)
    ".gradle/",
    // Ruby / Bundler
    ".bundle/",
    // Misc caches and coverage
    ".cache/",
    "coverage/",
];

/// Universal credential/secret detection. This is a security floor applied at
/// the **indexing** boundary (`gen` → the persistent store), so secret material
/// never enters the knowledge graph where `ask`/`asm` would assemble it into LLM
/// context. It deliberately does NOT gate ephemeral, targeted search (`grep`,
/// `locate`, `audit`) — you must be able to *find* secrets to fix them, and a
/// search result is scoped to the query rather than persisted and re-served.
/// Matching is by filename / extension / parent directory — never repo layout —
/// so it stays language- and project-agnostic.
pub fn is_secret_path(relative: &Path) -> bool {
    let rel = relative.to_string_lossy().replace('\\', "/");

    // Any path segment that is a well-known credential directory.
    const SECRET_DIRS: &[&str] = &[
        ".ssh", ".gnupg", ".aws", ".azure", ".kube", "secrets", "secret",
    ];
    if rel.split('/').any(|seg| SECRET_DIRS.contains(&seg)) {
        return true;
    }

    let name = relative
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    // Exact credential filenames.
    const SECRET_NAMES: &[&str] = &[
        ".env", ".envrc", ".npmrc", ".pypirc", ".netrc", "_netrc", ".htpasswd",
        "credentials", "credentials.json", "credentials.toml", "credentials.yml",
        "id_rsa", "id_dsa", "id_ecdsa", "id_ed25519",
    ];
    if SECRET_NAMES.contains(&name.as_str()) {
        return true;
    }
    // dotenv variants (.env.local, .env.production, …) and GCP service accounts.
    if name.starts_with(".env.")
        || (name.starts_with("service-account") && name.ends_with(".json"))
    {
        return true;
    }

    // Private-key / keystore / encrypted-secret extensions.
    const SECRET_EXTS: &[&str] = &[
        "pem", "key", "p8", "p12", "pfx", "keystore", "jks", "kdbx", "ppk", "asc", "gpg", "pgp",
    ];
    if let Some(ext) = relative.extension().and_then(|e| e.to_str()) {
        if SECRET_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
            return true;
        }
    }
    false
}

/// A compiled path filter combining `.adenignore` and `.adenallow` rules.
#[derive(Debug, Clone)]
pub struct AdenFilter {
    ignore_patterns: Vec<GlobRule>,
    allow_patterns: Vec<GlobRule>,
}

impl AdenFilter {
    pub fn pattern_count(&self) -> usize {
        self.ignore_patterns.len()
    }

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
        // Normalize separators so Windows backslash paths match slash patterns.
        let rel_str = relative.to_string_lossy().replace('\\', "/");
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
            let trimmed = self.pattern.trim_end_matches('/');
            let matches_dir = path == trimmed || path.starts_with(&(trimmed.to_string() + "/"));
            if matches_dir {
                return true;
            }
            if let Some(without_dot) = trimmed.strip_prefix('.') {
                path == without_dot || path.starts_with(&(without_dot.to_string() + "/"))
            } else {
                false
            }
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
            let pat = trimmed
                .trim_end_matches('/')
                .trim_start_matches('/')
                .to_string();
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
        return path.starts_with(prefix)
            && path.ends_with(suffix)
            && path.len() >= prefix.len() + suffix.len();
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
        assert!(filter.should_skip(Path::new("agent/file.adoc")));
        assert!(filter.should_skip(Path::new(".agent/file.adoc")));
    }

    #[test]
    fn test_builtin_skips_are_polyglot() {
        let filter = AdenFilter::from_directory(Path::new("/tmp/nonexistent"));
        // Build/dependency dirs from many ecosystems are excluded by default.
        assert!(filter.should_skip(Path::new("__pycache__/mod.cpython-312.pyc")));
        assert!(filter.should_skip(Path::new(".venv/lib/python3.12/site.py")));
        assert!(filter.should_skip(Path::new("vendor/golang.org/x/foo.go")));
        assert!(filter.should_skip(Path::new(".gradle/caches/modules")));
        assert!(filter.should_skip(Path::new("dist/bundle.js")));
        assert!(filter.should_skip(Path::new("obj/Debug/App.dll")));
        // …but real source in any language is kept, including monorepo
        // `packages/` and `bin/` which are source dirs in some ecosystems.
        assert!(!filter.should_skip(Path::new("app/main.py")));
        assert!(!filter.should_skip(Path::new("packages/api/src/index.ts")));
        assert!(!filter.should_skip(Path::new("bin/console.rb")));
        assert!(!filter.should_skip(Path::new("cmd/server/main.go")));
    }

    #[test]
    fn test_dotfile_pattern_from_ignore() {
        let filter = AdenFilter {
            ignore_patterns: compile_rules(&[".agent/".to_string()]),
            allow_patterns: Vec::new(),
        };
        assert!(filter.should_skip(Path::new("agent/file.adoc")), "Should match agent/");
        assert!(filter.should_skip(Path::new("agent/templates/foo.adoc")), "Should match agent/templates/");
        assert!(!filter.should_skip(Path::new("src/main.rs")));
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
