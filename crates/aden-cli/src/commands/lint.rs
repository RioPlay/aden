// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_core::NodeType;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintResult {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub severity: LintSeverity,
    pub rule: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LintSeverity {
    Suggest,
    Warn,
    Error,
}

impl LintSeverity {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "error" => LintSeverity::Error,
            "warn" => LintSeverity::Warn,
            _ => LintSeverity::Suggest,
        }
    }

    pub fn weight(&self) -> f64 {
        match self {
            LintSeverity::Error => 1.0,
            LintSeverity::Warn => 0.5,
            LintSeverity::Suggest => 0.25,
        }
    }
}

/// Minimal description of a graph symbol, used for the dead-code filter so the
/// decision logic can be unit-tested without building a real graph.
#[derive(Debug, Clone)]
struct SymbolInfo {
    /// Graph anchor (also serves as the symbol's identifier here).
    anchor: String,
    node_type: NodeType,
    incoming: usize,
}

/// Anchor patterns that legitimately have no incoming edges and must never be
/// reported as dead code. This mirrors the `is_expected_metadata` closure in
/// `util::perform_check` (kept in sync deliberately — that one is a private
/// closure, so it cannot be reused directly) plus public-entry-point names.
fn is_expected_or_public_anchor(anchor: &str) -> bool {
    // Synthetic / metadata anchors that never have callers by design.
    anchor.starts_with("aden://doc/")
        || anchor.starts_with("adr-")
        || anchor.starts_with("plan-")
        || anchor.starts_with("use-case-")
        || anchor.starts_with("agent-")
        || anchor == "readme"
        // Public API / entry points: `main`, or any anchor ending in `::main`
        // / `-main` (language-agnostic entry-point naming).
        || anchor == "main"
        || anchor.ends_with("::main")
        || anchor.ends_with("-main")
}

/// Decide whether a symbol should be flagged as dead code.
///
/// A symbol is dead-code-worthy when it is a real code symbol
/// (`Function`/`Type`), has zero incoming edges, and is not a synthetic `mod-`
/// anchor. When `include_public` is false, expected-metadata anchors and public
/// API / entry points (e.g. `main`) are also skipped.
fn is_dead_code(sym: &SymbolInfo, include_public: bool) -> bool {
    if sym.incoming > 0 {
        return false;
    }
    // Only flag concrete code symbols — modules, ADRs, plans, etc. are not code.
    if !matches!(sym.node_type, NodeType::Function | NodeType::Type) {
        return false;
    }
    // Always skip synthetic module anchors.
    if sym.anchor.starts_with("mod-") {
        return false;
    }
    if !include_public && is_expected_or_public_anchor(&sym.anchor) {
        return false;
    }
    true
}

/// Build dead-code lint findings from the project's knowledge graph: any code
/// symbol with zero incoming edges (use Direction::Incoming, like the
/// `query --backlinks` traversal) is potentially unreferenced.
fn lint_dead_code(
    path: &Path,
    include_public: bool,
) -> Result<Vec<LintResult>, Box<dyn std::error::Error>> {
    let graph = aden_graph::cache::build_from_directory_cached(path)?;
    let mut results = Vec::new();
    for node_idx in graph.graph.node_indices() {
        let node = &graph.graph[node_idx];
        let incoming = graph
            .graph
            .neighbors_directed(node_idx, aden_graph::Direction::Incoming)
            .count();
        let sym = SymbolInfo {
            anchor: node.doc.anchor.clone(),
            node_type: node.doc.node_type.clone(),
            incoming,
        };
        if !is_dead_code(&sym, include_public) {
            continue;
        }
        let (file, line) = match &node.doc.source_span {
            Some(span) => (span.file.clone(), span.start_line),
            None => (sym.anchor.clone(), 1),
        };
        results.push(LintResult {
            file,
            line,
            column: 1,
            severity: LintSeverity::Suggest,
            rule: "dead-code".to_string(),
            message: format!(
                "symbol '{}' has no incoming references (potentially dead code)",
                sym.anchor
            ),
        });
    }
    Ok(results)
}

pub fn cmd_lint(
    path: &Path,
    severity: &str,
    fix: bool,
    json: bool,
    dead_code: bool,
    include_public: bool,
    // When true, emit no report at all (used by ci-check, which only consumes the
    // Ok/Err result and builds its own JSON envelope). Distinct from `json`, which
    // selects the *format* of the standalone report — so `json` can always emit
    // valid JSON (`[]` on no findings) without polluting a parent's stdout.
    quiet: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let min_severity = LintSeverity::from_str(severity);

    // The banner is human chrome — keep it off stdout in --json mode so the
    // output is valid JSON for programmatic consumers.
    if !json && !quiet {
        println!("Aden Universal Linter");
        println!("=====================");
        println!("Scanning: {}", path.display());
        println!();
    }

    let _results: Vec<LintResult> = Vec::new();

    let sources = discover_source_files(path)?;

    // Parallel: lint all source files
    let all_results: Vec<_> = sources
        .par_iter()
        .filter_map(|src_path| {
            let content = std::fs::read_to_string(src_path).ok()?;
            let ext = src_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let file_results = lint_file(src_path, &content, ext);
            if file_results.is_empty() {
                None
            } else {
                Some(file_results)
            }
        })
        .flatten()
        .collect();

    let mut results = all_results;

    // Graph-based dead-code detection. Merged into the line-based findings here
    // so `--json` and `--severity` filtering apply to it automatically.
    if dead_code {
        results.extend(lint_dead_code(path, include_public)?);
    }

    // Constitution Warn directives (Phase 3 advisory policy).
    let policy = aden_policy::audit_policy(path);
    for v in &policy.violations {
        if v.severity.eq_ignore_ascii_case("warn") {
            results.push(LintResult {
                file: ".aden/constitution.adoc".to_string(),
                line: 0,
                column: 0,
                severity: LintSeverity::Warn,
                rule: "policy-constitution".to_string(),
                message: v.message.clone(),
            });
        }
    }

    results.sort_by_key(|r| r.file.clone());

    let filtered: Vec<_> = results
        .into_iter()
        .filter(|r| r.severity.weight() >= min_severity.weight())
        .collect();

    let error_count = filtered
        .iter()
        .filter(|r| r.severity == LintSeverity::Error)
        .count();
    let warn_count = filtered
        .iter()
        .filter(|r| r.severity == LintSeverity::Warn)
        .count();
    let suggest_count = filtered
        .iter()
        .filter(|r| r.severity == LintSeverity::Suggest)
        .count();

    if quiet {
        // No report — ci-check uses only the Ok/Err result.
    } else if json {
        // Always valid JSON, including `[]` when there are no findings, so a
        // standalone `aden lint --json | jq` never sees empty/invalid stdout.
        println!("{}", serde_json::to_string_pretty(&filtered)?);
    } else {
        if filtered.is_empty() {
            println!("No lint issues found.");
        } else {
            println!(
                "Issues found: {} error, {} warning, {} suggestion",
                error_count, warn_count, suggest_count
            );
            println!();

            for result in &filtered {
                let severity_str = match result.severity {
                    LintSeverity::Error => "ERROR",
                    LintSeverity::Warn => "WARN",
                    LintSeverity::Suggest => "SUGGEST",
                };
                println!(
                    "{}:{}:{} [{}] {}: {}",
                    result.file,
                    result.line,
                    result.column,
                    severity_str,
                    result.rule,
                    result.message
                );
            }
        }
    }

    // --fix: apply the safe, unambiguous auto-fixes (collapsing redundant
    // chained conversions) and report honestly on what still needs manual work,
    // instead of silently doing nothing.
    if fix {
        use std::collections::BTreeSet;
        let fixable_files: BTreeSet<&str> = filtered
            .iter()
            .filter(|r| r.rule == "redundant_to_string")
            .map(|r| r.file.as_str())
            .collect();
        let mut fixed_files = 0usize;
        for file in &fixable_files {
            if let Ok(content) = std::fs::read_to_string(file) {
                let mut changed = false;
                let mut out_lines: Vec<String> = Vec::with_capacity(content.lines().count());
                for line in content.lines() {
                    // Only collapse on a line whose *code* (string/comment text
                    // blanked) actually contains the redundant chain. This is
                    // what stops --fix from rewriting the pattern where it
                    // appears inside a string literal or comment — the blind
                    // whole-file replace used to corrupt such files (including
                    // aden's own lint rules).
                    let code = code_only(line);
                    let has_redundant = code.contains(".to_string().to_string()")
                        || code.contains(".to_owned().to_string()")
                        || code.contains(".to_string().to_owned()");
                    if has_redundant {
                        let fixed = line
                            .replace(".to_string().to_string()", ".to_string()")
                            .replace(".to_owned().to_string()", ".to_owned()")
                            .replace(".to_string().to_owned()", ".to_string()");
                        if fixed != line {
                            changed = true;
                        }
                        out_lines.push(fixed);
                    } else {
                        out_lines.push(line.to_string());
                    }
                }
                if changed {
                    // Preserve a trailing newline if the original had one.
                    let mut new = out_lines.join("\n");
                    if content.ends_with('\n') {
                        new.push('\n');
                    }
                    if std::fs::write(file, new).is_ok() {
                        fixed_files += 1;
                    }
                }
            }
        }
        let manual = filtered
            .iter()
            .filter(|r| r.rule != "redundant_to_string")
            .count();
        if !json {
            println!(
                "\n--fix: auto-fixed redundant Rust conversions in {} file(s); {} issue(s) require manual review.",
                fixed_files, manual
            );
        }
        return Ok(());
    }

    if error_count > 0 {
        return Err(format!("{} lint error(s) found", error_count).into());
    }

    Ok(())
}

fn discover_source_files(
    path: &Path,
) -> Result<Vec<std::path::PathBuf>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();

    let extensions = [
        "rs", "py", "go", "ts", "tsx", "js", "jsx", "java", "cs", "rb", "php", "c", "h", "cpp",
        "kt",
    ];

    // Use the shared path filter (built-in ignores + `.adenignore`) rather than a
    // hand-rolled exclusion list, so lint prunes exactly what gen/audit/check
    // prune — including agent-runtime dirs like `.claude/worktrees/`. Pruning at
    // the directory level (filter_entry) also avoids descending into them.
    let filter = aden_core::filter::AdenFilter::from_directory(path);
    for entry in walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            e.path()
                .strip_prefix(path)
                .map(|rel| rel.as_os_str().is_empty() || !filter.should_skip(rel))
                .unwrap_or(true)
        })
        .filter_map(|e| e.ok())
    {
        let entry_path = entry.path();
        if !entry_path.is_file() {
            continue;
        }

        let ext = entry_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        if !extensions.contains(&ext) {
            continue;
        }

        files.push(entry_path.to_path_buf());
    }

    Ok(files)
}

fn lint_file(path: &Path, content: &str, ext: &str) -> Vec<LintResult> {
    let mut results = Vec::new();

    // Stateful flag for adoc: are we currently inside a mermaid fenced block?
    // Threaded across lines so multi-line mermaid blocks are excluded from the
    // ascii_graph rule (it is per-line otherwise and only caught the first line).
    let mut mermaid = MermaidState::default();
    for (line_num, line) in content.lines().enumerate() {
        let line_results = apply_lint_rules(path, line, line_num + 1, ext, &mut mermaid);
        results.extend(line_results);
    }

    results
}

/// Byte offset of a standalone macro call `name` (e.g. `println!`) in `line`,
/// requiring the match to not be preceded by an identifier character. This keeps
/// `println!` from matching inside `eprintln!` (and `print!` inside `eprint!`),
/// which is what caused the `debug_print` rule to flag every stderr line.
fn find_macro(line: &str, name: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(rel) = line[from..].find(name) {
        let idx = from + rel;
        let prev_is_ident = idx > 0 && {
            let c = bytes[idx - 1];
            c.is_ascii_alphanumeric() || c == b'_'
        };
        if !prev_is_ident {
            return Some(idx);
        }
        from = idx + name.len();
    }
    None
}

/// True if `needle` occurs in `hay` as a whole word — not preceded or followed
/// by an identifier character (`[A-Za-z0-9_]`). Used so type-name / keyword
/// rules don't fire on substrings (e.g. `i32` inside `parse_i32`).
fn word_in(hay: &str, needle: &str) -> bool {
    let bytes = hay.as_bytes();
    let mut from = 0;
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    while let Some(rel) = hay[from..].find(needle) {
        let idx = from + rel;
        let prev_ok = idx == 0 || !is_ident(bytes[idx - 1]);
        let end = idx + needle.len();
        let next_ok = end >= bytes.len() || !is_ident(bytes[end]);
        if prev_ok && next_ok {
            return true;
        }
        from = idx + needle.len();
    }
    false
}

/// Byte offset of a loose-equality operator (`==` or `!=`) in `code`, excluding
/// the strict forms (`===`, `!==`) and the assignment/comparison operators
/// `=`, `<=`, `>=`. Returns `None` when only strict/other operators are present.
fn find_loose_equality(code: &str) -> Option<usize> {
    let b = code.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        // `==` not part of `===`, and not `!==`/`<=`/`>=` (those don't start `==`).
        if b[i] == b'=' && b[i + 1] == b'=' {
            let third_eq = i + 2 < b.len() && b[i + 2] == b'=';
            let prev_bang = i > 0 && b[i - 1] == b'!';
            // `===` -> skip; `!==` would have been caught as `!=` below, but the
            // `==` here is preceded by `!`, so it's `!==` strict — skip too.
            if !third_eq && !prev_bang {
                return Some(i);
            }
            i += 2;
            continue;
        }
        // `!=` not part of `!==`.
        if b[i] == b'!' && b[i + 1] == b'=' {
            let third_eq = i + 2 < b.len() && b[i + 2] == b'=';
            if !third_eq {
                return Some(i);
            }
            i += 3;
            continue;
        }
        i += 1;
    }
    None
}

/// True if the line is a comment in the given language. Code-level rules
/// (security, style) must never fire on prose inside a comment — e.g. the word
/// `eval(` in a Python `#` comment is not an eval call.
fn is_comment_line(trimmed: &str, ext: &str) -> bool {
    if trimmed.is_empty() {
        return false;
    }
    match ext {
        "py" | "rb" => trimmed.starts_with('#'),
        "rs" | "go" | "java" | "kt" | "cs" | "ts" | "tsx" | "js" | "jsx" | "php" | "c" | "h"
        | "cpp" => {
            trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*')
        }
        "adoc" | "aden" => trimmed.starts_with("//"),
        _ => false,
    }
}

fn apply_lint_rules(
    path: &Path,
    line: &str,
    line_num: usize,
    ext: &str,
    mermaid: &mut MermaidState,
) -> Vec<LintResult> {
    let mut results = Vec::new();

    let _path_str = path.to_string_lossy();

    // Skip comment-only lines for every language.
    if is_comment_line(line.trim(), ext) {
        return results;
    }

    let mut line_results = match ext {
        "rs" => lint_rust_line(line, line_num),
        "py" => lint_python_line(line, line_num),
        "ts" | "tsx" | "js" | "jsx" => lint_typescript_line(line, line_num),
        "go" => lint_go_line(line, line_num),
        "java" | "kt" => lint_java_line(line, line_num),
        "cs" => lint_csharp_line(line, line_num),
        "rb" => lint_ruby_line(line, line_num),
        "php" => lint_php_line(line, line_num),
        "adoc" | "aden" => lint_adoc_line(line, line_num, mermaid),
        _ => vec![],
    };

    // Language-agnostic secure-coding rules (ADR-002): a high-confidence subset
    // of the secure-coding constitution, enforced across all source languages.
    // Skipped for doc/markup extensions, which have no executable surface.
    if !matches!(ext, "adoc" | "aden") {
        line_results.extend(lint_secure_coding_line(line, line_num, ext));
    }

    for mut r in line_results {
        r.file = path.to_string_lossy().to_string();
        results.push(r);
    }

    results
}

/// Machine-checkable subset of the secure-coding constitution
/// (`.agent/secure-coding.adoc`, ADR-002). Each rule is tagged with the
/// constitution anchor it enforces, and is deliberately HIGH-CONFIDENCE —
/// matched only against real code (string/comment text blanked via `code_only`)
/// and only on patterns that are almost always genuine violations — so this
/// does not flood a scan with false positives.
fn lint_secure_coding_line(line: &str, line_num: usize, ext: &str) -> Vec<LintResult> {
    let mut results = Vec::new();
    // `code_only` blanks string literals and `//` comments. For languages that
    // use `#` line comments (Python, Ruby, PHP-hash), also drop a trailing `#`
    // comment first so e.g. `run([...])  # shell=True` is not flagged. Done on
    // the already-string-blanked text so a `#` inside a string is not mistaken
    // for a comment.
    let blanked = code_only(line);
    let code = if matches!(ext, "py" | "rb") {
        match blanked.find('#') {
            Some(i) => blanked[..i].to_string(),
            None => blanked,
        }
    } else {
        blanked
    };

    // sc-data-is-data: spawning a subprocess through a shell interprets data as
    // a command (CWE-78). The high-confidence signals differ per language.
    let shell_exec: Option<&str> = match ext {
        "py" => code
            .contains("shell=True")
            .then_some("subprocess called with shell=True — pass an argument list instead"),
        // Match the RAW line: the signal is the shell name *inside* the string
        // literal, which `code_only` would blank. Require the `Command::new(`
        // call in code so a mere mention of "sh" in a string elsewhere is safe.
        "rs" => (code.contains("Command::new(")
            && (line.contains("Command::new(\"sh\")") || line.contains("Command::new(\"bash\")")))
        .then_some("spawning a shell — pass program + args as an argv vector instead"),
        // NOTE: no JS/TS rule here. The high-confidence single-line signal would
        // be `child_process.exec(`, but real code splits the require/import from
        // the call across lines, and bare `.exec(` collides with RegExp.exec —
        // both unacceptable for a line-based linter. Omitted rather than noisy.
        // Ruby/PHP: the call (`system(`, `shell_exec(`) is code and survives
        // code_only, but the danger signal (string interpolation `#{`, or a PHP
        // `$var`) lives INSIDE the string literal — so check it on the raw line.
        "rb" => (code.contains("system(") && (line.contains("#{") || line.contains("\" +")))
            .then_some("shell command built from interpolated input — use an argv array form"),
        "php" => ((code.contains("shell_exec(") || code.contains("system(")) && line.contains('$'))
            .then_some("shell command with a variable — use escapeshellarg or a safe API"),
        _ => None,
    };
    if let Some(msg) = shell_exec {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: 1,
            severity: LintSeverity::Warn,
            rule: "sc-data-is-data".to_string(),
            message: format!("{msg} (secure-coding: sc-data-is-data)"),
        });
    }

    // sc-no-secret-ingest: an obviously hard-coded provider credential in source
    // (AWS/GitHub/OpenAI key shapes). Mirrors the gen indexing screen. NOTE:
    // scan the RAW line, not `code` — a hard-coded secret lives *inside* a
    // string literal, which `code_only` blanks, so checking `code` would miss it.
    if aden_core::filter::content_has_high_confidence_secret(line) {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: 1,
            severity: LintSeverity::Error,
            rule: "sc-no-secret-ingest".to_string(),
            message: "hard-coded credential token in source — move it to a secret store (secure-coding: sc-no-secret-ingest)".to_string(),
        });
    }

    results
}

/// Return `line` with string-literal contents and any trailing `//` comment
/// blanked out (replaced by spaces, preserving column positions), so that a
/// textual pattern match only fires on genuine *code* — not on a pattern that
/// happens to appear inside a string literal or a comment.
///
/// This is a deliberately small, single-line scanner (the linter is line-based
/// by design); it does not span multi-line strings, but it correctly handles
/// the common cases that caused false positives — e.g. aden's own lint rules
/// contain the literal `".to_string().to_string()"` as a string, which must
/// never be flagged or rewritten.
pub(crate) fn code_only(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];

        // Line comment — drop the rest.
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            break;
        }

        // Raw string literal: `r"…"`, `r#"…"#`, `r##"…"##`, … The body may
        // contain unescaped `"`; it ends at a quote followed by the same number
        // of `#`. Only treat `r` as a prefix when it starts a token (the
        // previous char is not an identifier char), so `error`/`for` don't match.
        if c == 'r' && (i == 0 || !is_ident(chars[i - 1])) {
            let mut j = i + 1;
            let mut hashes = 0;
            while chars.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if chars.get(j) == Some(&'"') {
                // Blank the opening delimiter (r, #…, ").
                for _ in i..=j {
                    out.push(' ');
                }
                let mut k = j + 1;
                while k < chars.len() {
                    if chars[k] == '"' && (0..hashes).all(|h| chars.get(k + 1 + h) == Some(&'#')) {
                        for _ in 0..=hashes {
                            out.push(' '); // closing quote + hashes
                        }
                        k += 1 + hashes;
                        break;
                    }
                    out.push(' ');
                    k += 1;
                }
                i = k;
                continue;
            }
        }

        // Regular string literal with `\` escapes.
        if c == '"' {
            out.push(' ');
            i += 1;
            let mut escaped = false;
            while i < chars.len() {
                let ch = chars[i];
                out.push(' ');
                i += 1;
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    break;
                }
            }
            continue;
        }

        out.push(c);
        i += 1;
    }
    out
}

fn lint_rust_line(line: &str, line_num: usize) -> Vec<LintResult> {
    let mut results = Vec::new();
    // Code with string/comment text blanked out, for patterns that must only
    // match real code (avoids flagging a pattern quoted inside a string).
    let code = code_only(line);

    // Pattern detection runs against `code` (string/comment text blanked by
    // code_only above) so a pattern quoted inside a string literal — e.g. the
    // linter's own rule definitions like `line.contains("unsafe fn")` — is not
    // flagged as real usage.
    if code.contains("unsafe fn") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: code.find("unsafe fn").unwrap_or(0) + 1,
            severity: LintSeverity::Warn,
            rule: "unsafe_fn".to_string(),
            message: "Usage of unsafe fn - review for memory safety".to_string(),
        });
    }

    if code.contains("unwrap()") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: code.find("unwrap()").unwrap_or(0) + 1,
            severity: LintSeverity::Suggest,
            rule: "unwrap_used".to_string(),
            message: "unwrap() can panic - consider using ? or expect() with context".to_string(),
        });
    }

    if code.contains("todo!()") || code.contains("unimplemented!()") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: 1,
            severity: LintSeverity::Warn,
            rule: "todo_in_code".to_string(),
            message: "TODO or unimplemented in code - should be resolved before production"
                .to_string(),
        });
    }

    // NEW: Unnecessary clone on Copy types. Match only against real code, and
    // require the Copy type name to appear as a whole word (so `parse_i32` /
    // `bool_flag` / a mention in a comment don't trigger it).
    let copy_type = |ty: &str| word_in(&code, ty);
    if code.contains(".clone()")
        && (copy_type("i32")
            || copy_type("u32")
            || copy_type("i64")
            || copy_type("u64")
            || copy_type("usize")
            || copy_type("isize")
            || copy_type("bool")
            || copy_type("char")
            || copy_type("f32")
            || copy_type("f64"))
    {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: code.find(".clone()").unwrap_or(0) + 1,
            severity: LintSeverity::Warn,
            rule: "unnecessary_clone".to_string(),
            message: "Unnecessary .clone() on Copy type - use direct assignment".to_string(),
        });
    }

    // Redundant conversion: a `.to_string()` chained onto something already
    // owned (`.to_string().to_string()`, `.to_owned().to_string()`) is an
    // unambiguous no-op. The old rule fired on any line containing both
    // "String" and ".to_string()", which false-flagged legitimate
    // `let s: String = x.to_string()` (&str -> String) conversions.
    if code.contains(".to_string().to_string()")
        || code.contains(".to_owned().to_string()")
        || code.contains(".to_string().to_owned()")
    {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: code
                .find(".to_string()")
                .or_else(|| code.find(".to_owned()"))
                .unwrap_or(0)
                + 1,
            severity: LintSeverity::Warn,
            rule: "redundant_to_string".to_string(),
            message: "Redundant chained conversion — the value is already owned".to_string(),
        });
    }

    // Debug print left in code. Only flag prints that actually look like
    // debugging: the `dbg!` macro, or a `print!`/`println!` using a debug
    // formatter (`{:?}`/`{:#?}`). A bare `println!("…")` is a CLI program's
    // legitimate product and `eprintln!` is diagnostic output, so neither is
    // flagged. (The previous rule matched the `println!` substring, which also
    // fired on every `eprintln!` and on all user-facing CLI output.)
    // Detect the macro on `code` so a quoted macro name (e.g. the linter's own
    // `find_macro(line, "dbg!")`) is not mistaken for a real invocation. The
    // `{:?}` check stays on `line` because the debug formatter legitimately
    // lives inside the format string, which code_only blanks out.
    let has_print_macro =
        find_macro(&code, "println!").is_some() || find_macro(&code, "print!").is_some();
    let uses_debug_fmt = line.contains("{:?}") || line.contains("{:#?}");
    let has_dbg_macro = find_macro(&code, "dbg!").is_some();
    if has_dbg_macro || (has_print_macro && uses_debug_fmt) {
        let column = find_macro(&code, "dbg!")
            .or_else(|| find_macro(&code, "println!"))
            .or_else(|| find_macro(&code, "print!"))
            .map(|i| i + 1)
            .unwrap_or(1);
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column,
            severity: LintSeverity::Warn,
            rule: "debug_print".to_string(),
            message: "Debug print left in code (dbg!/{:?}) — remove before release".to_string(),
        });
    }

    // unwrap_or_else with a closure body that is clearly a Default construction.
    // The previous rule fired on EVERY `unwrap_or_else(|| ...)`, but the
    // `unwrap_or_default()` rewrite is only valid when the closure returns the
    // type's `Default` value. Narrow to high-confidence default constructions so
    // the suggestion is actually correct.
    if let Some(start) = code
        .find("unwrap_or_else(||")
        .or_else(|| code.find("unwrap_or_else(|_|"))
    {
        // Body of the closure: everything after the second `|`.
        let after = &code[start..];
        let body = after
            .split_once("||")
            .map(|(_, b)| b)
            .or_else(|| after.split_once("|_|").map(|(_, b)| b))
            .unwrap_or("");
        let body_trim = body.trim_start();
        let is_default_body = body_trim.starts_with("Vec::new()")
            || body_trim.starts_with("String::new()")
            || body_trim.starts_with("HashMap::new()")
            || body_trim.starts_with("HashSet::new()")
            || body_trim.starts_with("BTreeMap::new()")
            || body_trim.starts_with("Default::default()")
            || body_trim.starts_with("vec![]")
            || body_trim.starts_with("0)")
            || body_trim.starts_with("0,")
            || body_trim == "0"
            || body_trim.starts_with("0 ")
            || body_trim.starts_with("\"\"");
        if is_default_body {
            results.push(LintResult {
                file: String::new(),
                line: line_num,
                column: start + 1,
                severity: LintSeverity::Suggest,
                rule: "unwrap_or_default".to_string(),
                message: "Closure returns a Default value - use unwrap_or_default()".to_string(),
            });
        }
    }

    results
}

fn lint_python_line(line: &str, line_num: usize) -> Vec<LintResult> {
    let mut results = Vec::new();
    // Code with string literals / `//`-comments blanked. (Python uses `#`
    // comments; comment-only lines are already filtered upstream, and a trailing
    // `#` comment after code is rare enough for these line rules.)
    let code = code_only(line);

    if line.contains("eval(") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("eval(").unwrap_or(0) + 1,
            severity: LintSeverity::Error,
            rule: "eval_usage".to_string(),
            message: "eval() is a security risk - avoid untrusted input".to_string(),
        });
    }

    if line.contains("exec(") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("exec(").unwrap_or(0) + 1,
            severity: LintSeverity::Error,
            rule: "exec_usage".to_string(),
            message: "exec() is a security risk - validate all input".to_string(),
        });
    }

    // A secret is being logged only when a print/log call AND a secret-ish word
    // are on the same line. The prior rule used broken || precedence and fired on
    // ANY line containing "secret" or "token" (e.g. a docstring, a parameter
    // definition, or a dict) regardless of whether there was a print call.
    let is_print_call = line.contains("print(")
        || line.contains("println!(")
        || line.contains("log::")
        || line.contains("logging.")
        || line.contains("logger.");
    let has_secret_word = line.contains("password")
        || line.contains("secret")
        || line.contains("token")
        || line.contains("api_key");
    if is_print_call && has_secret_word {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: 1,
            severity: LintSeverity::Error,
            rule: "secret_log".to_string(),
            message: "Potential secret being printed — use a logging library with redaction"
                .to_string(),
        });
    }

    // NEW: from module import * (wildcard imports)
    if line.contains("from ") && line.contains(" import *") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: 1,
            severity: LintSeverity::Warn,
            rule: "wildcard_import".to_string(),
            message: "Wildcard import can pollute namespace - import explicitly".to_string(),
        });
    }

    // NEW: bare except
    if line.contains("except:") && !line.contains("except ") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("except:").unwrap_or(0) + 1,
            severity: LintSeverity::Warn,
            rule: "bare_except".to_string(),
            message: "Bare except catches all exceptions - specify exception type".to_string(),
        });
    }

    // NEW: print statements (debug leftovers)
    if line.contains("print(")
        && !line.contains("#")
        && !line.contains("logger")
        && !line.contains("logging")
    {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("print(").unwrap_or(0) + 1,
            severity: LintSeverity::Suggest,
            rule: "debug_print".to_string(),
            message: "print() statement found - use logging in production".to_string(),
        });
    }

    // NEW: == None instead of is None
    if line.contains("== None") || line.contains("==None") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("==").unwrap_or(0) + 1,
            severity: LintSeverity::Suggest,
            rule: "comparison_none".to_string(),
            message: "Use 'is None' instead of '== None' for None comparison".to_string(),
        });
    }

    // NEW: shadowing built-in. Only when the built-in name is assigned as a
    // bare local (whole word, not a member access like `self.str = ...` and not
    // inside a string literal — handled by `code_only` + the `.` guard).
    let shadows_builtin = ["list", "dict", "str", "int", "type"].iter().any(|name| {
        let pat = format!("{name} = ");
        let mut from = 0;
        while let Some(rel) = code[from..].find(&pat) {
            let idx = from + rel;
            let prev = code[..idx].chars().next_back();
            let prev_is_ident_or_dot =
                matches!(prev, Some(c) if c.is_alphanumeric() || c == '_' || c == '.');
            if !prev_is_ident_or_dot {
                return true;
            }
            from = idx + pat.len();
        }
        false
    });
    if shadows_builtin {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: 1,
            severity: LintSeverity::Warn,
            rule: "shadow_builtin".to_string(),
            message: "Shadowing built-in type name - use different variable name".to_string(),
        });
    }

    results
}

fn lint_typescript_line(line: &str, line_num: usize) -> Vec<LintResult> {
    let mut results = Vec::new();
    // Code with string literals / `//` comments blanked, for rules that must
    // only fire on real code (not on a pattern quoted in a string).
    let code = code_only(line);

    if line.contains("eval(") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("eval(").unwrap_or(0) + 1,
            severity: LintSeverity::Error,
            rule: "eval_usage".to_string(),
            message: "eval() is a security risk - use JSON.parse or safe parsers".to_string(),
        });
    }

    // `any` used as a TYPE annotation, not a substring of an identifier like
    // `company` / `getAnyResult`. Require `any` as a whole word appearing in a
    // type position: `: any`, `as any`, `<any`, `any>`, `any[]`, `any,`, `any)`,
    // `any;`, `any |`/`| any`. Checked against code (strings/comments blanked).
    let any_as_type = word_in(&code, "any")
        && (code.contains(": any")
            || code.contains("as any")
            || code.contains("<any")
            || code.contains("any>")
            || code.contains("any[]")
            || code.contains("any,")
            || code.contains("any)")
            || code.contains("any;")
            || code.contains("any |")
            || code.contains("| any"));
    if any_as_type {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: code.find("any").unwrap_or(0) + 1,
            severity: LintSeverity::Suggest,
            rule: "any_type".to_string(),
            message: "Using 'any' type loses type safety - consider using generic or unknown"
                .to_string(),
        });
    }

    // NEW: console.log debugging
    if line.contains("console.log(") && !line.contains("//") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("console.log(").unwrap_or(0) + 1,
            severity: LintSeverity::Suggest,
            rule: "console_log".to_string(),
            message: "console.log() found - remove in production or use logger".to_string(),
        });
    }

    // NEW: == / != instead of === / !== (loose equality). Detect a genuine
    // loose-equality operator in code, excluding the strict `===` / `!==` forms.
    let loose_eq_col = find_loose_equality(&code);
    if let Some(col) = loose_eq_col {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: col + 1,
            severity: LintSeverity::Suggest,
            rule: "loose_equality".to_string(),
            message: "Use === instead of == for strict equality".to_string(),
        });
    }

    // NEW: var instead of let/const
    if line.contains("var ") && !line.contains("//") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("var ").unwrap_or(0) + 1,
            severity: LintSeverity::Suggest,
            rule: "var_usage".to_string(),
            message: "Use 'let' or 'const' instead of 'var' for block scoping".to_string(),
        });
    }

    // NEW: require instead of import
    if line.contains("require(") && !line.contains("//") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("require(").unwrap_or(0) + 1,
            severity: LintSeverity::Suggest,
            rule: "require_usage".to_string(),
            message: "Use ES6 import instead of require()".to_string(),
        });
    }

    // NEW: trailing console.info/warn/error
    if (line.contains("console.info(")
        || line.contains("console.warn(")
        || line.contains("console.error("))
        && !line.contains("logger")
        && !line.contains("log.")
    {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: 1,
            severity: LintSeverity::Suggest,
            rule: "console_output".to_string(),
            message: "Console output found - consider using structured logger".to_string(),
        });
    }

    // NEW: process.exit in non-test code
    if line.contains("process.exit(") && !line.contains("test") && !line.contains("__tests__") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("process.exit(").unwrap_or(0) + 1,
            severity: LintSeverity::Warn,
            rule: "process_exit".to_string(),
            message: "process.exit() found - avoid in production, prefer throwing errors"
                .to_string(),
        });
    }

    results
}

fn lint_go_line(line: &str, line_num: usize) -> Vec<LintResult> {
    let mut results = Vec::new();

    if line.contains("panic(") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("panic(").unwrap_or(0) + 1,
            severity: LintSeverity::Warn,
            rule: "panic_in_code".to_string(),
            message: "panic() will crash the program - consider returning error".to_string(),
        });
    }

    // NEW: fmt.Printf debugging
    if line.contains("fmt.Printf(") || line.contains("fmt.Println(") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("fmt.Print").unwrap_or(0) + 1,
            severity: LintSeverity::Suggest,
            rule: "debug_print".to_string(),
            message: "fmt.Print() found - use logging package in production".to_string(),
        });
    }

    // NEW: log.Printf debugging
    if line.contains("log.Printf(") || line.contains("log.Println(") {
        // This is acceptable for logging, but flag if it looks like debug
    }

    // NEW: ignoring error return value
    if line.contains("_ = ") && (line.contains("err") || line.contains("Error")) {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: 1,
            severity: LintSeverity::Warn,
            rule: "ignored_error".to_string(),
            message: "Ignoring error return value - handle or explicitly ignore with _".to_string(),
        });
    }

    // NEW: fmt.Errorf without wrapping
    if line.contains("fmt.Errorf(\"") && !line.contains("%w") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("fmt.Errorf").unwrap_or(0) + 1,
            severity: LintSeverity::Suggest,
            rule: "error_wrap".to_string(),
            message: "Consider using %w to wrap error for better error chains".to_string(),
        });
    }

    // NEW: http.ListenAndServe without error handling
    if line.contains("http.ListenAndServe(") && !line.contains("if err") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("http.ListenAndServe").unwrap_or(0) + 1,
            severity: LintSeverity::Warn,
            rule: "server_error".to_string(),
            message: "ListenAndServe called without error handling".to_string(),
        });
    }

    results
}

fn lint_java_line(line: &str, line_num: usize) -> Vec<LintResult> {
    let mut results = Vec::new();

    if line.contains("System.out.println") && (line.contains("password") || line.contains("secret"))
    {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: 1,
            severity: LintSeverity::Error,
            rule: "secret_log".to_string(),
            message: "Potential secret being printed".to_string(),
        });
    }

    results
}

fn lint_csharp_line(line: &str, line_num: usize) -> Vec<LintResult> {
    let mut results = Vec::new();
    let code = code_only(line);

    // Console.WriteLine left in code (debug output).
    if code.contains("Console.WriteLine(") || code.contains("Console.Write(") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: code.find("Console.Write").unwrap_or(0) + 1,
            severity: LintSeverity::Suggest,
            rule: "debug_print".to_string(),
            message: "Console.Write* found - use a logging framework in production".to_string(),
        });
    }

    // Empty catch block swallows exceptions.
    if code.contains("catch") && code.replace(' ', "").contains("catch{}") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: code.find("catch").unwrap_or(0) + 1,
            severity: LintSeverity::Warn,
            rule: "empty_catch".to_string(),
            message: "Empty catch block swallows exceptions - handle or log them".to_string(),
        });
    }

    results
}

fn lint_ruby_line(line: &str, line_num: usize) -> Vec<LintResult> {
    let mut results = Vec::new();

    if line.contains("eval(") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("eval(").unwrap_or(0) + 1,
            severity: LintSeverity::Error,
            rule: "eval_usage".to_string(),
            message: "eval() is a security risk".to_string(),
        });
    }

    results
}

fn lint_php_line(line: &str, line_num: usize) -> Vec<LintResult> {
    let mut results = Vec::new();

    if line.contains("eval(") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("eval(").unwrap_or(0) + 1,
            severity: LintSeverity::Error,
            rule: "eval_usage".to_string(),
            message: "eval() is a severe security risk".to_string(),
        });
    }

    results
}

/// Stateful tracker for adoc `[mermaid]` fenced blocks, threaded line-by-line
/// through [`lint_adoc_line`]. A `[mermaid]` attribute line arms the next
/// `----` fence to open a mermaid block; the matching `----` closes it. Plain
/// (non-mermaid) `----` listing blocks are ignored so they don't suppress the
/// ascii_graph rule. This type is local to this module.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum MermaidState {
    /// Not in or near a mermaid block.
    #[default]
    Outside,
    /// Saw `[mermaid]`; the next `----` fence opens the block.
    Armed,
    /// Inside an open mermaid fenced block.
    Inside,
}

/// `mermaid` tracks, across calls (one per line), whether the current line is
/// inside a `[mermaid]` fenced block. This makes mermaid exclusion stateful so a
/// multi-line mermaid diagram is never flagged as an ASCII graph.
fn lint_adoc_line(line: &str, line_num: usize, mermaid: &mut MermaidState) -> Vec<LintResult> {
    let mut results = Vec::new();
    let trimmed = line.trim();

    // A `[mermaid]` / `[source,mermaid]` attribute line arms the next fence.
    if trimmed.starts_with("[mermaid") || trimmed.contains("source,mermaid") {
        *mermaid = MermaidState::Armed;
        return results;
    }
    if trimmed.starts_with("----") {
        *mermaid = match *mermaid {
            MermaidState::Armed => MermaidState::Inside, // opening fence
            MermaidState::Inside => MermaidState::Outside, // closing fence
            MermaidState::Outside => MermaidState::Outside, // unrelated listing
        };
        return results;
    }

    // Rule: No ASCII art graphs - must use Mermaid
    // Box-drawing characters that indicate ASCII graphs
    let ascii_graph_chars = ['│', '├', '└', '┌', '┐', '┘', '┼', '╭', '╮', '╯', '╰', '═'];
    let has_ascii_graph = ascii_graph_chars.iter().any(|c| line.contains(*c));

    // Allow if inside a mermaid fenced block (stateful) or if the line itself is
    // a mermaid diagram-type declaration.
    let is_mermaid_decl = trimmed.starts_with("flowchart")
        || trimmed.starts_with("graph")
        || trimmed.starts_with("pie")
        || trimmed.starts_with("sequenceDiagram")
        || trimmed.starts_with("classDiagram")
        || trimmed.starts_with("stateDiagram");
    let is_in_mermaid = *mermaid == MermaidState::Inside || is_mermaid_decl;

    if has_ascii_graph && !is_in_mermaid && line.len() > 10 {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: 1,
            severity: LintSeverity::Error,
            rule: "ascii_graph".to_string(),
            message: "ASCII graph detected. Use Mermaid diagrams in [mermaid] code blocks instead."
                .to_string(),
        });
    }

    // Rule: References must use <<anchor>> format
    if line.contains("link:") && !line.contains("<<") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("link:").unwrap_or(0) + 1,
            severity: LintSeverity::Suggest,
            rule: "adoc_ref_style".to_string(),
            message: "Use <<anchor>> references instead of link: URLs".to_string(),
        });
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(line: &str, ext: &str) -> Vec<String> {
        lint_secure_coding_line(line, 1, ext)
            .into_iter()
            .map(|r| r.rule)
            .collect()
    }

    #[test]
    fn secure_coding_flags_real_violations() {
        // sc-data-is-data — shell exec signals per language.
        assert!(
            rules("subprocess.run(cmd, shell=True)", "py").contains(&"sc-data-is-data".to_string())
        );
        assert!(
            rules("Command::new(\"sh\").arg(\"-c\")", "rs")
                .contains(&"sc-data-is-data".to_string())
        );
        assert!(
            rules("system(\"rm #{user_path}\")", "rb").contains(&"sc-data-is-data".to_string())
        );
        // sc-no-secret-ingest — hard-coded credential in a string literal.
        // Non-EXAMPLE body: an `EXAMPLE` suffix marks a documentation
        // placeholder, which the detector deliberately ignores.
        let aws_key_code = ["let k = \"", "AKIA", "IOSFODNN7REALKEY\";"].concat();
        assert!(rules(&aws_key_code, "rs").contains(&"sc-no-secret-ingest".to_string()));
    }

    #[test]
    fn secure_coding_no_false_positives() {
        // shell=True only in a trailing comment must not flag.
        assert!(
            !rules("subprocess.run([\"ls\"])  # not shell=True", "py")
                .contains(&"sc-data-is-data".to_string())
        );
        // `sk`/`AKIA` as identifiers/words, not credential tokens.
        assert!(rules("let sk = total; let akia = 1;", "rs").is_empty());
        // safe argv-form subprocess is fine.
        assert!(rules("subprocess.run([\"git\", \"status\"])", "py").is_empty());
        // a regex exec in JS must not be flagged (we ship no JS shell rule).
        assert!(rules("const m = /a/.exec(input);", "js").is_empty());
    }

    #[test]
    fn code_only_blanks_strings_and_comments() {
        // Pattern inside a string literal is not code.
        assert!(
            !code_only(r#"line.contains(".to_string().to_string()")"#)
                .contains(".to_string().to_string()")
        );
        // Pattern inside a line comment is not code.
        assert!(
            !code_only("// foo .to_string().to_string() bar").contains(".to_string().to_string()")
        );
        // Genuine code is preserved.
        assert!(
            code_only(r#"let x = y.to_string().to_string();"#).contains(".to_string().to_string()")
        );
        // Escaped quote inside a string does not prematurely end it.
        assert!(
            !code_only(r#"let s = "a \" .to_string().to_string()";"#)
                .contains(".to_string().to_string()")
        );
    }

    #[test]
    fn code_only_blanks_raw_string_literals() {
        // Pattern inside a hashed raw string is not code (regression: the linter
        // flagged its own `r#"…unsafe fn…"#` / `r#"…dbg!…"#` test fixtures).
        let blanked = code_only(r##"let l = r#"unsafe fn dbg!() {:?} todo!()"#;"##);
        assert!(!blanked.contains("unsafe fn"), "got: {blanked}");
        assert!(!blanked.contains("dbg!"), "got: {blanked}");
        assert!(!blanked.contains("todo!()"), "got: {blanked}");
        // The surrounding real code survives.
        assert!(blanked.contains("let l ="), "got: {blanked}");
        // Non-hashed raw string too.
        assert!(!code_only(r##"x(r"unsafe fn")"##).contains("unsafe fn"));
        // `r` as part of an identifier must NOT trigger raw-string handling.
        assert!(code_only("for x in y { foo() }").contains("for x in y"));
        assert!(code_only("let error = bar();").contains("let error = bar()"));
    }

    #[test]
    fn redundant_to_string_not_flagged_inside_string_literal() {
        // Regression: aden's own lint rules contain this pattern as a string;
        // it must never be flagged (which previously led --fix to corrupt the
        // file).
        let line = r#"    line.contains(".to_string().to_string()")"#;
        let results = lint_rust_line(line, 1);
        assert!(
            !results.iter().any(|r| r.rule == "redundant_to_string"),
            "pattern in a string literal must not be flagged: {results:?}"
        );
    }

    #[test]
    fn redundant_to_string_flagged_in_real_code() {
        let line = r#"    let x = y.to_string().to_string();"#;
        let results = lint_rust_line(line, 1);
        assert!(
            results.iter().any(|r| r.rule == "redundant_to_string"),
            "genuine redundant conversion should be flagged"
        );
    }

    fn sym(anchor: &str, node_type: NodeType, incoming: usize) -> SymbolInfo {
        SymbolInfo {
            anchor: anchor.to_string(),
            node_type,
            incoming,
        }
    }

    #[test]
    fn dead_code_flags_unreferenced_function() {
        assert!(is_dead_code(
            &sym("orphan_fn", NodeType::Function, 0),
            false
        ));
        assert!(is_dead_code(&sym("OrphanType", NodeType::Type, 0), false));
    }

    #[test]
    fn dead_code_skips_referenced_function() {
        assert!(!is_dead_code(&sym("used_fn", NodeType::Function, 2), false));
    }

    #[test]
    fn dead_code_skips_synthetic_module_anchors() {
        assert!(!is_dead_code(&sym("mod-foo", NodeType::Function, 0), false));
        // Modules themselves are never code symbols.
        assert!(!is_dead_code(&sym("foo", NodeType::Module, 0), false));
    }

    #[test]
    fn dead_code_skips_public_entry_points_by_default() {
        let main = sym("main", NodeType::Function, 0);
        assert!(!is_dead_code(&main, false));
        // With --include-public, entry points ARE flagged.
        assert!(is_dead_code(&main, true));
        // Qualified entry-point names are treated the same.
        assert!(!is_dead_code(
            &sym("app::main", NodeType::Function, 0),
            false
        ));
    }

    // ---- helpers for per-language line rules ----
    fn rust_rules(line: &str) -> Vec<String> {
        lint_rust_line(line, 1)
            .into_iter()
            .map(|r| r.rule)
            .collect()
    }
    fn py_rules(line: &str) -> Vec<String> {
        lint_python_line(line, 1)
            .into_iter()
            .map(|r| r.rule)
            .collect()
    }
    fn ts_rules(line: &str) -> Vec<String> {
        lint_typescript_line(line, 1)
            .into_iter()
            .map(|r| r.rule)
            .collect()
    }
    fn cs_rules(line: &str) -> Vec<String> {
        lint_csharp_line(line, 1)
            .into_iter()
            .map(|r| r.rule)
            .collect()
    }
    fn has(v: &[String], r: &str) -> bool {
        v.iter().any(|x| x == r)
    }

    // ---------- Rust: unnecessary_clone ----------
    #[test]
    fn unnecessary_clone_true_and_false_positives() {
        // true positive: clone on a whole-word Copy type
        assert!(has(
            &rust_rules("let x: i32 = y.clone();"),
            "unnecessary_clone"
        ));
        assert!(has(
            &rust_rules("let b: bool = flag.clone();"),
            "unnecessary_clone"
        ));
        // false positive: `i32` only as a substring of an identifier
        assert!(!has(
            &rust_rules("let v = parse_i32(s).clone();"),
            "unnecessary_clone"
        ));
        // false positive: type name only inside a comment
        assert!(!has(
            &rust_rules("let s = name.clone(); // returns i32 later"),
            "unnecessary_clone"
        ));
        // false positive: no copy type at all
        assert!(!has(
            &rust_rules("let s = name.clone();"),
            "unnecessary_clone"
        ));
    }

    // ---------- Rust: unwrap_or_default ----------
    #[test]
    fn unwrap_or_default_narrowed() {
        // true positive: closure returns a Default construction
        assert!(has(
            &rust_rules("let v = opt.unwrap_or_else(|| Vec::new());"),
            "unwrap_or_default"
        ));
        assert!(has(
            &rust_rules("let s = opt.unwrap_or_else(|| String::new());"),
            "unwrap_or_default"
        ));
        assert!(has(
            &rust_rules("let n = opt.unwrap_or_else(|| 0);"),
            "unwrap_or_default"
        ));
        // false positive: closure returns a non-default value
        assert!(!has(
            &rust_rules("let v = opt.unwrap_or_else(|| compute_fallback());"),
            "unwrap_or_default"
        ));
        assert!(!has(
            &rust_rules("let v = opt.unwrap_or_else(|_| other.clone());"),
            "unwrap_or_default"
        ));
    }

    // ---------- Python: shadow_builtin ----------
    #[test]
    fn shadow_builtin_true_and_false_positives() {
        assert!(has(&py_rules("list = []"), "shadow_builtin"));
        assert!(has(&py_rules("str = get_name()"), "shadow_builtin"));
        // false positive: member assignment, not a bare local
        assert!(!has(&py_rules("self.str = value"), "shadow_builtin"));
        assert!(!has(&py_rules("obj.list = []"), "shadow_builtin"));
        // false positive: inside a string literal
        assert!(!has(&py_rules("msg = \"str = value\""), "shadow_builtin"));
    }

    // ---------- Python: other rules still fire ----------
    #[test]
    fn python_basic_rules() {
        assert!(has(&py_rules("x = eval(user_input)"), "eval_usage"));
        assert!(has(&py_rules("if x == None:"), "comparison_none"));
        assert!(has(&py_rules("from os import *"), "wildcard_import"));
    }

    // ---------- TS: any_type ----------
    #[test]
    fn any_type_true_and_false_positives() {
        assert!(has(&ts_rules("function f(x: any) {}"), "any_type"));
        assert!(has(&ts_rules("const v = data as any;"), "any_type"));
        assert!(has(&ts_rules("let xs: any[] = [];"), "any_type"));
        // false positive: `any` as substring of an identifier
        assert!(!has(
            &ts_rules("const company: string = getCompany();"),
            "any_type"
        ));
        assert!(!has(&ts_rules("const r = getAnyResult();"), "any_type"));
        // false positive: pattern inside a string
        assert!(!has(&ts_rules("const s: string = \": any\";"), "any_type"));
    }

    // ---------- TS: loose_equality ----------
    #[test]
    fn loose_equality_true_and_false_positives() {
        assert!(has(&ts_rules("if (a == b) {}"), "loose_equality"));
        assert!(has(&ts_rules("if (a != b) {}"), "loose_equality"));
        // false positive: strict equality
        assert!(!has(&ts_rules("if (a === b) {}"), "loose_equality"));
        assert!(!has(&ts_rules("if (a !== b) {}"), "loose_equality"));
        // false positive: plain assignment
        assert!(!has(&ts_rules("const x = b;"), "loose_equality"));
        // false positive: `==` inside a string
        assert!(!has(&ts_rules("const s = \"a == b\";"), "loose_equality"));
    }

    // ---------- TS: other rules ----------
    #[test]
    fn typescript_basic_rules() {
        assert!(has(&ts_rules("console.log(x);"), "console_log"));
        assert!(has(&ts_rules("var x = 1;"), "var_usage"));
    }

    // ---------- C# ----------
    #[test]
    fn csharp_rules() {
        assert!(has(&cs_rules("Console.WriteLine(x);"), "debug_print"));
        assert!(has(&cs_rules("try { f(); } catch { }"), "empty_catch"));
        // false positive: WriteLine inside a string is not code
        assert!(!has(
            &cs_rules("var s = \"Console.WriteLine(x)\";"),
            "debug_print"
        ));
        assert!(cs_rules("int x = 1;").is_empty());
    }

    // ---------- adoc: stateful mermaid ----------
    fn adoc_lines(lines: &[&str]) -> Vec<String> {
        let mut st = MermaidState::default();
        let mut out = Vec::new();
        for (i, l) in lines.iter().enumerate() {
            for r in lint_adoc_line(l, i + 1, &mut st) {
                out.push(r.rule);
            }
        }
        out
    }

    #[test]
    fn adoc_mermaid_block_is_stateful() {
        // A multi-line mermaid fenced block must NOT be flagged as ascii_graph.
        let block = [
            "[mermaid]",
            "----",
            "graph TD",
            "  A --> B",
            "  ├── child",
            "  └── leaf",
            "----",
        ];
        assert!(!has(&adoc_lines(&block), "ascii_graph"));

        // The SAME box-drawing content outside a mermaid block IS flagged.
        let raw = ["Some prose here", "  ├── child node goes here longer"];
        assert!(has(&adoc_lines(&raw), "ascii_graph"));
    }

    #[test]
    fn adoc_plain_listing_block_does_not_suppress() {
        // A non-mermaid `----` listing block must not arm mermaid state, so an
        // ascii graph after it is still flagged.
        let lines = [
            "----",
            "some code",
            "----",
            "  ├── this is an ascii graph line that is long",
        ];
        assert!(has(&adoc_lines(&lines), "ascii_graph"));
    }

    #[test]
    fn dead_code_skips_expected_metadata_anchors() {
        // Metadata anchors legitimately have no incoming edges. They are also
        // not Function/Type, but the anchor guard is the relevant one here.
        assert!(!is_dead_code(&sym("adr-001", NodeType::Function, 0), false));
        assert!(!is_dead_code(&sym("plan-x", NodeType::Function, 0), false));
        // Override with --include-public.
        assert!(is_dead_code(&sym("adr-001", NodeType::Function, 0), true));
    }
}
