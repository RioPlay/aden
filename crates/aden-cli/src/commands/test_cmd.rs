// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestInfo {
    pub name: String,
    pub file: String,
    pub line: usize,
    pub scope: TestScope,
    pub language: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestScope {
    Unit,
    Integration,
}

impl TestScope {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "integration" => TestScope::Integration,
            _ => TestScope::Unit,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub name: String,
    pub file: String,
    pub passed: bool,
    pub duration_ms: u64,
    pub message: Option<String>,
}

pub fn cmd_test(
    path: &Path,
    scope: Option<&str>,
    filter: Option<&str>,
    list_only: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !json {
        println!("Aden Universal Test Runner");
        println!("===========================");
        println!("Scanning: {}", path.display());
        println!();
    }

    let test_scope = scope.map(TestScope::from_str);
    let mut all_tests = discover_tests(path)?;

    if let Some(s) = test_scope {
        all_tests.retain(|t| t.scope == s);
    }

    if let Some(f) = filter {
        all_tests.retain(|t| t.name.to_lowercase().contains(&f.to_lowercase()));
    }

    // --list (or -j --list): report the discovered tests without running them.
    if list_only {
        if json {
            let envelope = serde_json::json!({
                "scanned_path": path.to_string_lossy(),
                "discovered": all_tests.len(),
                "ran": false,
                "tests": all_tests,
            });
            println!("{}", serde_json::to_string_pretty(&envelope)?);
            return Ok(());
        }
        println!("Discovered {} tests", all_tests.len());
        print_test_list(&all_tests);
        return Ok(());
    }

    // Count distinct language suites that will be run.
    let suite_count = {
        use std::collections::BTreeSet;
        all_tests
            .iter()
            .map(|t| &t.language)
            .collect::<BTreeSet<_>>()
            .len()
    };

    if !json {
        println!(
            "Discovered {} test(s) across {} language suite(s)",
            all_tests.len(),
            suite_count
        );
        print_test_list(&all_tests);
        println!();
        println!("Running {} suite(s)...", suite_count);
        println!();
    }

    let results = run_tests(&all_tests)?;
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.iter().filter(|r| !r.passed).count();

    if json {
        let envelope = serde_json::json!({
            "scanned_path": path.to_string_lossy(),
            "discovered": all_tests.len(),
            "suites": suite_count,
            "ran": true,
            "suites_passed": passed,
            "suites_failed": failed,
            "results": results,
        });
        println!("{}", serde_json::to_string_pretty(&envelope)?);
    } else {
        println!(
            "Results: {} suite(s) passed, {} suite(s) failed",
            passed, failed
        );
        for result in &results {
            if result.passed {
                println!("  PASS: {}", result.name);
            } else {
                println!(
                    "  FAIL: {} - {}",
                    result.name,
                    result.message.as_deref().unwrap_or("unknown error")
                );
            }
        }
    }

    if failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// Print the discovered tests as a numbered human list.
fn print_test_list(all_tests: &[TestInfo]) {
    for (i, test) in all_tests.iter().enumerate() {
        let scope_str = match test.scope {
            TestScope::Unit => "unit",
            TestScope::Integration => "integration",
        };
        println!(
            "  {}. [{}] {} ({}:{})",
            i + 1,
            scope_str,
            test.name,
            test.file,
            test.line
        );
    }
}

fn discover_tests(path: &Path) -> Result<Vec<TestInfo>, Box<dyn std::error::Error>> {
    let mut tests = Vec::new();

    let extensions = [
        "rs", "py", "go", "ts", "tsx", "js", "jsx", "java", "cs", "rb", "php",
    ];

    // Use the shared path filter (built-in ignores + `.adenignore`) so the test
    // runner prunes exactly what gen/audit/lint prune — including Rust toolchain
    // dirs, agent-runtime dirs like `.claude/worktrees/`, and any project-specific
    // `.adenignore` entries. Pruning at the directory level (`filter_entry`) avoids
    // descending into excluded subtrees entirely.
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

        let content = match std::fs::read_to_string(entry_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let file_tests = discover_file_tests(entry_path, &content, ext);
        tests.extend(file_tests);
    }

    Ok(tests)
}

/// Walk up from `start` until a file named `manifest` is found, returning the
/// directory that contains it.  Returns `None` if the filesystem root is reached
/// without a match.
fn find_manifest_dir(start: &Path, manifest: &str) -> Option<PathBuf> {
    let mut dir = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    loop {
        if dir.join(manifest).exists() {
            return Some(dir);
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => return None,
        }
    }
}

/// Walk up from `start` until any file with the given extension is found in a
/// directory, returning that directory.  Used for manifests whose names are
/// not fixed (e.g. `*.sln`, `*.csproj`).  Returns `None` if the filesystem
/// root is reached without a match.
fn find_manifest_dir_by_ext(start: &Path, ext: &str) -> Option<PathBuf> {
    let mut dir = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    loop {
        let found = std::fs::read_dir(&dir).ok()?.any(|entry| {
            entry.ok().and_then(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x == ext)
            }) == Some(true)
        });
        if found {
            return Some(dir);
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => return None,
        }
    }
}

fn discover_file_tests(path: &Path, content: &str, ext: &str) -> Vec<TestInfo> {
    let file_str = path.to_string_lossy().to_string();
    let mut tests = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        let test_info = match ext {
            "rs" => discover_rust_test(line, line_num, &file_str),
            "py" => discover_python_test(line, line_num, &file_str),
            "ts" | "tsx" | "js" | "jsx" => discover_typescript_test(line, line_num, &file_str),
            "go" => discover_go_test(line, line_num, &file_str),
            "java" => discover_java_test(line, line_num, &file_str),
            "cs" => discover_csharp_test(line, line_num, &file_str),
            "rb" => discover_ruby_test(line, line_num, &file_str),
            "php" => discover_php_test(line, line_num, &file_str),
            _ => None,
        };

        if let Some(t) = test_info {
            tests.push(t);
        }
    }

    tests
}

fn discover_rust_test(line: &str, line_num: usize, file: &str) -> Option<TestInfo> {
    let trimmed = line.trim();

    if trimmed.starts_with("#[test]") {
        return Some(TestInfo {
            name: format!("test at line {}", line_num + 1),
            file: file.to_string(),
            line: line_num + 1,
            scope: TestScope::Unit,
            language: "rust".to_string(),
        });
    }

    if trimmed.starts_with("#[cfg(test)]") {
        return Some(TestInfo {
            name: format!("test module at line {}", line_num + 1),
            file: file.to_string(),
            line: line_num + 1,
            scope: TestScope::Unit,
            language: "rust".to_string(),
        });
    }

    if trimmed.starts_with("#[tokio::test]") || trimmed.starts_with("#[actix_rt::test]") {
        return Some(TestInfo {
            name: format!("async test at line {}", line_num + 1),
            file: file.to_string(),
            line: line_num + 1,
            scope: TestScope::Unit,
            language: "rust".to_string(),
        });
    }

    None
}

fn discover_python_test(line: &str, line_num: usize, file: &str) -> Option<TestInfo> {
    let trimmed = line.trim();

    if trimmed.starts_with("def test_") || trimmed.starts_with("async def test_") {
        let name = trimmed
            .trim_start_matches("async ")
            .trim_start_matches("def ")
            .split('(')
            .next()
            .unwrap_or("");
        return Some(TestInfo {
            name: name.to_string(),
            file: file.to_string(),
            line: line_num + 1,
            scope: TestScope::Unit,
            language: "python".to_string(),
        });
    }

    if trimmed.starts_with("class Test") && !trimmed.contains(":") {
        return Some(TestInfo {
            name: trimmed.trim_end_matches(':').to_string(),
            file: file.to_string(),
            line: line_num + 1,
            scope: TestScope::Unit,
            language: "python".to_string(),
        });
    }

    None
}

fn discover_typescript_test(line: &str, line_num: usize, file: &str) -> Option<TestInfo> {
    let trimmed = line.trim();

    if (trimmed.contains("test(") || trimmed.contains("it(") || trimmed.contains("describe("))
        && !trimmed.starts_with("//")
        && !trimmed.starts_with("*")
    {
        return Some(TestInfo {
            name: format!("test at line {}", line_num + 1),
            file: file.to_string(),
            line: line_num + 1,
            scope: TestScope::Unit,
            language: "typescript".to_string(),
        });
    }

    if trimmed.contains("@Test") || trimmed.contains("@test") {
        return Some(TestInfo {
            name: format!("test at line {}", line_num + 1),
            file: file.to_string(),
            line: line_num + 1,
            scope: TestScope::Unit,
            language: "typescript".to_string(),
        });
    }

    None
}

fn discover_go_test(line: &str, line_num: usize, file: &str) -> Option<TestInfo> {
    let trimmed = line.trim();

    if trimmed.starts_with("func Test")
        && (trimmed.contains("(t *testing.T)") || trimmed.contains("(t testing.T)"))
    {
        let name = trimmed
            .trim_start_matches("func ")
            .split('(')
            .next()
            .unwrap_or("");
        return Some(TestInfo {
            name: name.to_string(),
            file: file.to_string(),
            line: line_num + 1,
            scope: TestScope::Unit,
            language: "go".to_string(),
        });
    }

    if trimmed.starts_with("func Benchmark") {
        return Some(TestInfo {
            name: format!("benchmark at line {}", line_num + 1),
            file: file.to_string(),
            line: line_num + 1,
            scope: TestScope::Unit,
            language: "go".to_string(),
        });
    }

    None
}

fn discover_java_test(line: &str, line_num: usize, file: &str) -> Option<TestInfo> {
    let trimmed = line.trim();

    if trimmed.contains("@Test") {
        return Some(TestInfo {
            name: format!("test at line {}", line_num + 1),
            file: file.to_string(),
            line: line_num + 1,
            scope: TestScope::Unit,
            language: "java".to_string(),
        });
    }

    if trimmed.starts_with("@ParameterizedTest") || trimmed.starts_with("@RepeatedTest") {
        return Some(TestInfo {
            name: format!("parameterized test at line {}", line_num + 1),
            file: file.to_string(),
            line: line_num + 1,
            scope: TestScope::Unit,
            language: "java".to_string(),
        });
    }

    None
}

fn discover_csharp_test(line: &str, line_num: usize, file: &str) -> Option<TestInfo> {
    let trimmed = line.trim();

    if trimmed.contains("[Fact]") || trimmed.contains("[Theory]") || trimmed.contains("[Test]") {
        return Some(TestInfo {
            name: format!("test at line {}", line_num + 1),
            file: file.to_string(),
            line: line_num + 1,
            scope: TestScope::Unit,
            language: "csharp".to_string(),
        });
    }

    None
}

fn discover_ruby_test(line: &str, line_num: usize, file: &str) -> Option<TestInfo> {
    let trimmed = line.trim();

    if trimmed.starts_with("def test_") {
        let name = trimmed
            .trim_start_matches("def ")
            .split('_')
            .next()
            .unwrap_or("");
        return Some(TestInfo {
            name: format!("test_{}", name),
            file: file.to_string(),
            line: line_num + 1,
            scope: TestScope::Unit,
            language: "ruby".to_string(),
        });
    }

    None
}

fn discover_php_test(line: &str, line_num: usize, file: &str) -> Option<TestInfo> {
    let trimmed = line.trim();

    if trimmed.starts_with("public function test") || trimmed.starts_with("function test") {
        let name = trimmed.split_whitespace().last().unwrap_or("");
        return Some(TestInfo {
            name: name.to_string(),
            file: file.to_string(),
            line: line_num + 1,
            scope: TestScope::Unit,
            language: "php".to_string(),
        });
    }

    None
}

fn run_tests(tests: &[TestInfo]) -> Result<Vec<TestResult>, Box<dyn std::error::Error>> {
    // Run each language's test suite ONCE, not once per discovered test function.
    // The previous loop called `run_single_test` for every TestInfo, which ran
    // `cargo test` / `pytest` / … N times for a project with N discovered tests —
    // the N-fold problem. Group by language and invoke the runner once per group.
    use std::collections::BTreeMap;
    let mut by_lang: BTreeMap<String, &TestInfo> = BTreeMap::new();
    for t in tests {
        by_lang.entry(t.language.clone()).or_insert(t);
    }
    let mut results = Vec::new();
    for (lang, representative) in &by_lang {
        let mut result = run_single_test(representative)?;
        result.name = lang.clone();
        results.push(result);
    }
    Ok(results)
}

fn run_single_test(test: &TestInfo) -> Result<TestResult, Box<dyn std::error::Error>> {
    let start = std::time::Instant::now();

    let result = match test.language.as_str() {
        "rust" => run_rust_test(test),
        "python" => run_python_test(test),
        "typescript" => run_typescript_test(test),
        "go" => run_go_test(test),
        "java" => run_java_test(test),
        "csharp" => run_csharp_test(test),
        "ruby" => run_ruby_test(test),
        "php" => run_php_test(test),
        _ => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: true,
            duration_ms: 0,
            message: Some("Unknown language, skipped".to_string()),
        }),
    };

    let duration = start.elapsed().as_millis() as u64;
    let mut r = result?;
    r.duration_ms = duration;
    Ok(r)
}

fn run_rust_test(test: &TestInfo) -> Result<TestResult, Box<dyn std::error::Error>> {
    // `cargo test` must run from the directory that contains `Cargo.toml`.  Walk up
    // from the test file to find the workspace or crate root.
    let test_path = Path::new(&test.file);
    let dir = find_manifest_dir(test_path, "Cargo.toml")
        .unwrap_or_else(|| test_path.parent().unwrap_or(Path::new(".")).to_path_buf());

    let output = std::process::Command::new("cargo")
        .args(["test", "--", "--nocapture"])
        .current_dir(&dir)
        .output();

    match output {
        Ok(o) => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: o.status.success(),
            duration_ms: 0,
            message: if !o.status.success() {
                Some(String::from_utf8_lossy(&o.stderr).to_string())
            } else {
                None
            },
        }),
        Err(e) => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: false,
            duration_ms: 0,
            message: Some(format!("Failed to run cargo test: {}", e)),
        }),
    }
}

fn run_python_test(test: &TestInfo) -> Result<TestResult, Box<dyn std::error::Error>> {
    // pytest must run from the project root (where pyproject.toml / setup.py /
    // requirements.txt lives) so that its rootdir detection is correct.  Try each
    // manifest in precedence order; fall back to the file's parent directory.
    let test_path = Path::new(&test.file);
    let dir = find_manifest_dir(test_path, "pyproject.toml")
        .or_else(|| find_manifest_dir(test_path, "setup.py"))
        .or_else(|| find_manifest_dir(test_path, "requirements.txt"))
        .unwrap_or_else(|| test_path.parent().unwrap_or(Path::new(".")).to_path_buf());

    let output = std::process::Command::new("python")
        .args(["-m", "pytest", "-v"])
        .current_dir(&dir)
        .output();

    match output {
        Ok(o) => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: o.status.success(),
            duration_ms: 0,
            message: if !o.status.success() {
                Some(String::from_utf8_lossy(&o.stderr).to_string())
            } else {
                None
            },
        }),
        Err(e) => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: false,
            duration_ms: 0,
            message: Some(format!("Failed to run pytest: {}", e)),
        }),
    }
}

fn run_typescript_test(test: &TestInfo) -> Result<TestResult, Box<dyn std::error::Error>> {
    // npm/yarn/pnpm must run from the directory containing `package.json`.
    let test_path = Path::new(&test.file);
    let dir = find_manifest_dir(test_path, "package.json")
        .unwrap_or_else(|| test_path.parent().unwrap_or(Path::new(".")).to_path_buf());

    let output = std::process::Command::new("npm")
        .args(["test", "--", "--passWithNoTests"])
        .current_dir(&dir)
        .output();

    match output {
        Ok(o) => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: o.status.success(),
            duration_ms: 0,
            message: if !o.status.success() {
                Some(String::from_utf8_lossy(&o.stderr).to_string())
            } else {
                None
            },
        }),
        Err(e) => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: false,
            duration_ms: 0,
            message: Some(format!("Failed to run npm test: {}", e)),
        }),
    }
}

fn run_go_test(test: &TestInfo) -> Result<TestResult, Box<dyn std::error::Error>> {
    // `go test ./...` must run from the module root (where `go.mod` lives).
    let test_path = Path::new(&test.file);
    let dir = find_manifest_dir(test_path, "go.mod")
        .unwrap_or_else(|| test_path.parent().unwrap_or(Path::new(".")).to_path_buf());

    let output = std::process::Command::new("go")
        .args(["test", "-v", "./..."])
        .current_dir(&dir)
        .output();

    match output {
        Ok(o) => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: o.status.success(),
            duration_ms: 0,
            message: if !o.status.success() {
                Some(String::from_utf8_lossy(&o.stderr).to_string())
            } else {
                None
            },
        }),
        Err(e) => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: false,
            duration_ms: 0,
            message: Some(format!("Failed to run go test: {}", e)),
        }),
    }
}

fn run_java_test(test: &TestInfo) -> Result<TestResult, Box<dyn std::error::Error>> {
    // Maven must run from the directory containing `pom.xml`.
    let test_path = Path::new(&test.file);
    let dir = find_manifest_dir(test_path, "pom.xml")
        .unwrap_or_else(|| test_path.parent().unwrap_or(Path::new(".")).to_path_buf());

    let output = std::process::Command::new("mvn")
        .args(["test"])
        .current_dir(&dir)
        .output();

    match output {
        Ok(o) => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: o.status.success(),
            duration_ms: 0,
            message: if !o.status.success() {
                Some(String::from_utf8_lossy(&o.stderr).to_string())
            } else {
                None
            },
        }),
        Err(e) => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: false,
            duration_ms: 0,
            message: Some(format!("Failed to run mvn test: {}", e)),
        }),
    }
}

fn run_csharp_test(test: &TestInfo) -> Result<TestResult, Box<dyn std::error::Error>> {
    // `dotnet test` must run from the directory containing a `.sln` or `.csproj`.
    // Try `.sln` first (solution root runs all projects); fall back to `.csproj`.
    let test_path = Path::new(&test.file);
    let dir = find_manifest_dir_by_ext(test_path, "sln")
        .or_else(|| find_manifest_dir_by_ext(test_path, "csproj"))
        .unwrap_or_else(|| test_path.parent().unwrap_or(Path::new(".")).to_path_buf());

    let output = std::process::Command::new("dotnet")
        .args(["test"])
        .current_dir(&dir)
        .output();

    match output {
        Ok(o) => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: o.status.success(),
            duration_ms: 0,
            message: if !o.status.success() {
                Some(String::from_utf8_lossy(&o.stderr).to_string())
            } else {
                None
            },
        }),
        Err(e) => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: false,
            duration_ms: 0,
            message: Some(format!("Failed to run dotnet test: {}", e)),
        }),
    }
}

fn run_ruby_test(test: &TestInfo) -> Result<TestResult, Box<dyn std::error::Error>> {
    // `rake test` must run from the directory containing `Rakefile` or `Gemfile`.
    let test_path = Path::new(&test.file);
    let dir = find_manifest_dir(test_path, "Rakefile")
        .or_else(|| find_manifest_dir(test_path, "Gemfile"))
        .unwrap_or_else(|| test_path.parent().unwrap_or(Path::new(".")).to_path_buf());

    let output = std::process::Command::new("rake")
        .arg("test")
        .current_dir(&dir)
        .output();

    match output {
        Ok(o) => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: o.status.success(),
            duration_ms: 0,
            message: if !o.status.success() {
                Some(String::from_utf8_lossy(&o.stderr).to_string())
            } else {
                None
            },
        }),
        Err(e) => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: false,
            duration_ms: 0,
            message: Some(format!("Failed to run rake test: {}", e)),
        }),
    }
}

fn run_php_test(test: &TestInfo) -> Result<TestResult, Box<dyn std::error::Error>> {
    // PHPUnit must run from the directory containing `phpunit.xml` or
    // `composer.json` (where it auto-discovers the config).
    let test_path = Path::new(&test.file);
    let dir = find_manifest_dir(test_path, "phpunit.xml")
        .or_else(|| find_manifest_dir(test_path, "phpunit.xml.dist"))
        .or_else(|| find_manifest_dir(test_path, "composer.json"))
        .unwrap_or_else(|| test_path.parent().unwrap_or(Path::new(".")).to_path_buf());

    let output = std::process::Command::new("phpunit")
        .current_dir(&dir)
        .output();

    match output {
        Ok(o) => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: o.status.success(),
            duration_ms: 0,
            message: if !o.status.success() {
                Some(String::from_utf8_lossy(&o.stderr).to_string())
            } else {
                None
            },
        }),
        Err(e) => Ok(TestResult {
            name: test.name.clone(),
            file: test.file.clone(),
            passed: false,
            duration_ms: 0,
            message: Some(format!("Failed to run phpunit: {}", e)),
        }),
    }
}
