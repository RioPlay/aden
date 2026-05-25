use std::path::Path;
use serde::{Deserialize, Serialize};

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
    
    println!("Aden Universal Linter");
    println!("=====================");
    println!("Scanning: {}", path.display());
    println!();

    let mut results: Vec<LintResult> = Vec::new();
    
    let sources = discover_source_files(path)?;
    
    for src_path in &sources {
        let content = match std::fs::read_to_string(src_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        
        let ext = src_path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        
        let file_results = lint_file(src_path, &content, ext);
        results.extend(file_results);
    }
    
    let filtered: Vec<_> = results.into_iter()
        .filter(|r| r.severity.weight() >= min_severity.weight())
        .collect();
    
    let error_count = filtered.iter().filter(|r| r.severity == LintSeverity::Error).count();
    let warn_count = filtered.iter().filter(|r| r.severity == LintSeverity::Warn).count();
    let suggest_count = filtered.iter().filter(|r| r.severity == LintSeverity::Suggest).count();
    
    if json {
        println!("{}", serde_json::to_string_pretty(&filtered)?);
    } else {
        if filtered.is_empty() {
            println!("No lint issues found.");
        } else {
            println!("Issues found: {} error, {} warning, {} suggestion", 
                error_count, warn_count, suggest_count);
            println!();
            
            for result in &filtered {
                let severity_str = match result.severity {
                    LintSeverity::Error => "ERROR",
                    LintSeverity::Warn => "WARN",
                    LintSeverity::Suggest => "SUGGEST",
                };
                println!("{}:{}:{} [{}] {}: {}", 
                    result.file, result.line, result.column,
                    severity_str, result.rule, result.message);
            }
        }
    }
    
    if error_count > 0 && !fix {
        std::process::exit(1);
    }
    
    Ok(())
}

fn discover_source_files(path: &Path) -> Result<Vec<std::path::PathBuf>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    
    let extensions = ["rs", "py", "go", "ts", "tsx", "js", "jsx", "java", "cs", "rb", "php", "c", "h", "cpp", "kt"];
    
    for entry in walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let entry_path = entry.path();
        if !entry_path.is_file() {
            continue;
        }
        
        let ext = entry_path.extension()
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
    let exclusions = ["target", ".git", "node_modules", ".cargo", ".rustup", "dist", "build"];
    
    path.components().any(|c| {
        c.as_os_str().to_str()
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

fn apply_lint_rules(path: &Path, line: &str, line_num: usize, ext: &str) -> Vec<LintResult> {
    let mut results = Vec::new();
    
    let line_results = match ext {
        "rs" => lint_rust_line(line, line_num),
        "py" => lint_python_line(line, line_num),
        "ts" | "tsx" | "js" | "jsx" => lint_typescript_line(line, line_num),
        "go" => lint_go_line(line, line_num),
        "java" | "kt" => lint_java_line(line, line_num),
        "cs" => lint_csharp_line(line, line_num),
        "rb" => lint_ruby_line(line, line_num),
        "php" => lint_php_line(line, line_num),
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

    if line.contains("unsafe fn") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("unsafe fn").unwrap_or(0) + 1,
            severity: LintSeverity::Warn,
            rule: "unsafe_fn".to_string(),
            message: "Usage of unsafe fn - review for memory safety".to_string(),
        });
    }
    
    if line.contains("unwrap()") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: line.find("unwrap()").unwrap_or(0) + 1,
            severity: LintSeverity::Suggest,
            rule: "unwrap_used".to_string(),
            message: "unwrap() can panic - consider using ? or expect() with context".to_string(),
        });
    }
    
    if line.contains("todo!()") || line.contains("unimplemented!()") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: 1,
            severity: LintSeverity::Warn,
            rule: "todo_in_code".to_string(),
            message: "TODO or unimplemented in code - should be resolved before production".to_string(),
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
    
    if line.contains("print(") && line.contains("password") || line.contains("secret") || line.contains("token") {
        results.push(LintResult {
            file: String::new(),
            line: line_num,
            column: 1,
            severity: LintSeverity::Error,
            rule: "secret_log".to_string(),
            message: "Potential secret being printed - use logging library with redaction".to_string(),
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
            message: "Using 'any' type loses type safety - consider using generic or unknown".to_string(),
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
    
    results
}

fn lint_java_line(line: &str, line_num: usize) -> Vec<LintResult> {
    let mut results = Vec::new();
    
    if line.contains("System.out.println") && (line.contains("password") || line.contains("secret")) {
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