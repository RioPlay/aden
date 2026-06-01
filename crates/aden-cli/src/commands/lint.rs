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
) -> Result<(), Box<dyn std::error::Error>> {
    let min_severity = LintSeverity::from_str(severity);

    // The banner is human chrome — keep it off stdout in --json mode so the
    // output is valid JSON for programmatic consumers.
    if !json {
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

    if json {
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
                "\n--fix: auto-fixed redundant conversions in {} file(s); {} issue(s) require manual review.",
                fixed_files, manual
            );
        }
        return Ok(());
    }

    if error_count > 0 {
        std::process::exit(1);
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

    for entry in walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
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

        if is_excluded(entry_path) {
            continue;
        }

        files.push(entry_path.to_path_buf());
    }

    Ok(files)
}

fn is_excluded(path: &Path) -> bool {
    let exclusions = [
        "target",
        ".git",
        "node_modules",
        ".cargo",
        ".rustup",
        "dist",
        "build",
    ];

    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .map(|s| exclusions.contains(&s))
            .unwrap_or(false)
    })
}

fn lint_file(path: &Path, content: &str, ext: &str) -> Vec<LintResult> {
    let mut results = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        let line_results = apply_lint_rules(path, line, line_num + 1, ext);
        results.extend(line_results);
    }

    results
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

fn apply_lint_rules(path: &Path, line: &str, line_num: usize, ext: &str) -> Vec<LintResult> {
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
        "adoc" | "aden" => lint_adoc_line(line, line_num),
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
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut in_str = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_str {
            // Inside a string literal: blank everything until the closing quote.
            if escaped {
                escaped = false;
                out.push(' ');
            } else if c == '\\' {
                escaped = true;
                out.push(' ');
            } else if c == '"' {
                in_str = false;
                out.push(' ');
            } else {
                out.push(' ');
            }
        } else if c == '"' {
            in_str = true;
            out.push(' ');
        } else if c == '/' && chars.peek() == Some(&'/') {
            // Rest of the line is a comment — drop it.
            break;
        } else {
            out.push(c);
        }
    }
    out
}

fn lint_rust_line(line: &str, line_num: usize) -> Vec<LintResult> {
    let mut results = Vec::new();
    // Code with string/comment text blanked out, for patterns that must only
    // match real code (avoids flagging a pattern quoted inside a string).
    let code = code_only(line);

    // Skip lines that are comments or string-literal definitions — the pattern
    // may appear as documentation or as a value being matched, not as real usage.
    let trimmed = line.trim();
    let is_string_literal_line = trimmed.starts_with("//")
        || trimmed.starts_with("///")
        || trimmed.starts_with('"')
        || trimmed.starts_with("r\"")
        || trimmed.starts_with("r#");

    if line.contains("unsafe fn") && !is_string_literal_line {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("unsafe fn").unwrap_or(0) + 1,
            severity: LintSeverity::Warn,
            rule: "unsafe_fn".to_string(),
            message: "Usage of unsafe fn - review for memory safety".to_string(),
        });
    }

    if line.contains("unwrap()") && !is_string_literal_line {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("unwrap()").unwrap_or(0) + 1,
            severity: LintSeverity::Suggest,
            rule: "unwrap_used".to_string(),
            message: "unwrap() can panic - consider using ? or expect() with context".to_string(),
        });
    }

    if (line.contains("todo!()") || line.contains("unimplemented!()")) && !is_string_literal_line {
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

    // NEW: Unnecessary clone on Copy types
    if line.contains(".clone()")
        && (line.contains("i32")
            || line.contains("bool")
            || line.contains("char")
            || line.contains("f32")
            || line.contains("f64"))
    {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find(".clone()").unwrap_or(0) + 1,
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

    // NEW: Debug println left in code
    if line.contains("println!")
        && (line.contains("\"") && !line.contains("logger") && !line.contains("log::"))
    {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("println!").map(|i| i + 1).unwrap_or(1),
            severity: LintSeverity::Warn,
            rule: "debug_print".to_string(),
            message: "Debug println left in code - remove in production".to_string(),
        });
    }

    // NEW: Unused mut
    if line.contains("let mut ") && (line.contains("=") && !line.contains(" = ")) {
        // Check if it's likely unused - pattern: let mut x = something that doesn't get reassigned
    }

    // NEW: unwrap_or_else with default that's cheap
    if line.contains("unwrap_or_else(||") || line.contains("unwrap_or_else(|_|") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("unwrap_or_else").unwrap_or(0) + 1,
            severity: LintSeverity::Suggest,
            rule: "unwrap_or_default".to_string(),
            message: "Consider using unwrap_or_default() for simpler syntax".to_string(),
        });
    }

    results
}

fn lint_python_line(line: &str, line_num: usize) -> Vec<LintResult> {
    let mut results = Vec::new();

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

    if line.contains("print(") && line.contains("password")
        || line.contains("secret")
        || line.contains("token")
    {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: 1,
            severity: LintSeverity::Error,
            rule: "secret_log".to_string(),
            message: "Potential secret being printed - use logging library with redaction"
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

    // NEW: shadowing built-in
    if line.contains("list = ")
        || line.contains("dict = ")
        || line.contains("str = ")
        || line.contains("int = ")
        || line.contains("type = ")
    {
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

    if line.contains("any") && line.contains(": ") && !line.contains("//") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("any").unwrap_or(0) + 1,
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

    // NEW: == instead of === (loose equality)
    if line.contains(" = ")
        && line.contains(" == ")
        && !line.contains("===")
        && !line.contains("//")
    {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find(" == ").unwrap_or(0) + 1,
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

fn lint_csharp_line(_line: &str, _line_num: usize) -> Vec<LintResult> {
    vec![]
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

fn lint_adoc_line(line: &str, line_num: usize) -> Vec<LintResult> {
    let mut results = Vec::new();

    // Rule: No ASCII art graphs - must use Mermaid
    // Box-drawing characters that indicate ASCII graphs
    let ascii_graph_chars = ['│', '├', '└', '┌', '┐', '┘', '┼', '╭', '╮', '╯', '╰', '═'];
    let has_ascii_graph = ascii_graph_chars.iter().any(|c| line.contains(*c));

    // But allow if it's inside a mermaid block
    // This is a simple heuristic - would need more sophisticated parsing for full correctness
    let is_in_mermaid = line.starts_with("----")
        || line.contains("[mermaid")
        || line.starts_with("flowchart")
        || line.starts_with("graph")
        || line.starts_with("pie")
        || line.starts_with("sequenceDiagram")
        || line.starts_with("classDiagram")
        || line.starts_with("stateDiagram");

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
        let aws_key_code = ["let k = \"", "AKIA", "IOSFODNN7EXAMPLE\";"].concat();
        assert!(
            rules(&aws_key_code, "rs").contains(&"sc-no-secret-ingest".to_string())
        );
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
        assert!(is_dead_code(&sym("orphan_fn", NodeType::Function, 0), false));
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
        assert!(!is_dead_code(&sym("app::main", NodeType::Function, 0), false));
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
