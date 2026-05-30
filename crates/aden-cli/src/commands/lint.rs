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

pub fn cmd_lint(
    path: &Path,
    severity: &str,
    fix: bool,
    json: bool,
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
                let new = content
                    .replace(".to_string().to_string()", ".to_string()")
                    .replace(".to_owned().to_string()", ".to_owned()")
                    .replace(".to_string().to_owned()", ".to_string()");
                if new != content && std::fs::write(file, new).is_ok() {
                    fixed_files += 1;
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

    let line_results = match ext {
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

    for mut r in line_results {
        r.file = path.to_string_lossy().to_string();
        results.push(r);
    }

    results
}

fn lint_rust_line(line: &str, line_num: usize) -> Vec<LintResult> {
    let mut results = Vec::new();

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
    if line.contains(".clone()") && (line.contains("i32") || line.contains("bool") || line.contains("char") || line.contains("f32") || line.contains("f64")) {
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
    if line.contains(".to_string().to_string()")
        || line.contains(".to_owned().to_string()")
        || line.contains(".to_string().to_owned()")
    {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find(".to_string()").or_else(|| line.find(".to_owned()")).unwrap_or(0) + 1,
            severity: LintSeverity::Warn,
            rule: "redundant_to_string".to_string(),
            message: "Redundant chained conversion — the value is already owned".to_string(),
        });
    }

    // NEW: Debug println left in code
    if line.contains("println!") && (line.contains("\"") && !line.contains("logger") && !line.contains("log::")) {
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
    if line.contains("print(") && !line.contains("#") && !line.contains("logger") && !line.contains("logging") {
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
    if line.contains("list = ") || line.contains("dict = ") || line.contains("str = ") || line.contains("int = ") || line.contains("type = ") {
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
    if line.contains(" = ") && line.contains(" == ") && !line.contains("===") && !line.contains("//") {
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
    if (line.contains("console.info(") || line.contains("console.warn(") || line.contains("console.error("))
        && !line.contains("logger") && !line.contains("log.") {
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
            message: "process.exit() found - avoid in production, prefer throwing errors".to_string(),
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
            message: "ASCII graph detected. Use Mermaid diagrams in [mermaid] code blocks instead.".to_string(),
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
