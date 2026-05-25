use std::path::Path;
use serde::{Deserialize, Serialize};

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
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Aden Universal Test Runner");
    println!("===========================");
    println!("Scanning: {}", path.display());
    println!();

    let test_scope = scope.map(TestScope::from_str);
    let mut all_tests = discover_tests(path)?;
    
    if let Some(s) = test_scope {
        all_tests.retain(|t| t.scope == s);
    }
    
    if let Some(f) = filter {
        all_tests.retain(|t| t.name.to_lowercase().contains(&f.to_lowercase()));
    }
    
    println!("Discovered {} tests", all_tests.len());
    
    for (i, test) in all_tests.iter().enumerate() {
        let scope_str = match test.scope {
            TestScope::Unit => "unit",
            TestScope::Integration => "integration",
        };
        println!("  {}. [{}] {} ({}:{})", i + 1, scope_str, test.name, test.file, test.line);
    }
    
    if list_only {
        return Ok(());
    }
    
    println!();
    println!("Running tests...");
    println!();
    
    let results = run_tests(&all_tests)?;
    
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.iter().filter(|r| !r.passed).count();
    
    println!("Results: {} passed, {} failed", passed, failed);
    
    for result in &results {
        if !result.passed {
            println!("  FAIL: {} - {}", result.file, result.message.as_deref().unwrap_or("unknown error"));
        }
    }
    
    if failed > 0 {
        std::process::exit(1);
    }
    
    Ok(())
}

fn discover_tests(path: &Path) -> Result<Vec<TestInfo>, Box<dyn std::error::Error>> {
    let mut tests = Vec::new();
    
    let extensions = ["rs", "py", "go", "ts", "tsx", "js", "jsx", "java", "cs", "rb", "php"];
    
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
        
        if is_excluded(&entry_path) {
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

fn is_excluded(path: &Path) -> bool {
    let exclusions = ["target", ".git", "node_modules", ".cargo", ".rustup", "dist", "build"];
    
    path.components().any(|c| {
        c.as_os_str().to_str()
            .map(|s| exclusions.contains(&s))
            .unwrap_or(false)
    })
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
        let name = trimmed.trim_start_matches("async ").trim_start_matches("def ").split('(').next().unwrap_or("");
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
    
    if trimmed.contains("test(") || trimmed.contains("it(") || trimmed.contains("describe(") {
        if !trimmed.starts_with("//") && !trimmed.starts_with("*") {
            return Some(TestInfo {
                name: format!("test at line {}", line_num + 1),
                file: file.to_string(),
                line: line_num + 1,
                scope: TestScope::Unit,
                language: "typescript".to_string(),
            });
        }
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
    
    if trimmed.starts_with("func Test") && (trimmed.contains("(t *testing.T)") || trimmed.contains("(t testing.T)")) {
        let name = trimmed.trim_start_matches("func ").split('(').next().unwrap_or("");
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
        let name = trimmed.trim_start_matches("def ").split('_').next().unwrap_or("");
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
    let mut results = Vec::new();
    
    for test in tests {
        let result = run_single_test(test)?;
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
    let output = std::process::Command::new("cargo")
        .args(["test", "--", "--nocapture"])
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
    let output = std::process::Command::new("python")
        .args(["-m", "pytest", "-v"])
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
    let output = std::process::Command::new("npm")
        .args(["test", "--", "--passWithNoTests"])
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
    let output = std::process::Command::new("go")
        .args(["test", "-v", "./..."])
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
    let output = std::process::Command::new("mvn")
        .args(["test"])
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
    let output = std::process::Command::new("dotnet")
        .args(["test"])
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
    let output = std::process::Command::new("rake")
        .arg("test")
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
    let output = std::process::Command::new("phpunit")
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