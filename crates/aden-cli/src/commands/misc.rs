use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

use crate::types::{OwaspFinding, OwaspSeverity};
use crate::util::quick_health_score;

/// OWASP-style security audit: scan source for vulnerabilities.
pub fn cmd_audit(
    path: &Path,
    lang_filter: Option<&str>,
    format: &str,
    strict: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut findings: Vec<OwaspFinding> = Vec::new();

    // Determine which languages to scan
    let scan_all = lang_filter.is_none();
    let want_lang = lang_filter.map(|s| s.to_lowercase());

    // Extensions mapped to language IDs
    let lang_exts: Vec<(&str, &str)> = vec![
        ("rs", "rust"), ("py", "python"), ("go", "go"),
        ("js", "ts"), ("ts", "ts"), ("jsx", "ts"), ("tsx", "ts"),
        ("php", "php"), ("java", "java"), ("cpp", "cpp"), ("c", "c"),
        ("h", "c"), ("hpp", "cpp"),
    ];

    // Collect source files
    let mut files = Vec::new();
    if path.is_file() {
        files.push(path.to_path_buf());
    } else {
        for entry in walkdir::WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if !p.is_file() { continue; }
            if let Some(ext) = p.extension().and_then(|e| e.to_str())
                && let Some(l) = lang_exts.iter().find(|(e, _)| *e == ext.to_lowercase())
                    && (scan_all || want_lang.as_deref() == Some(l.1)) {
                        files.push(p.to_path_buf());
                    }
        }
    }

    type OwaspPattern = (Regex, Option<&'static str>, &'static str, &'static str, OwaspSeverity, &'static str, &'static str);

    // Build pattern table
    static OWASP_PATTERNS: OnceLock<Vec<OwaspPattern>> = OnceLock::new();
    let patterns = OWASP_PATTERNS.get_or_init(|| {
        vec![
            // A03 - Injection: eval / exec / Function (JavaScript / Python / Ruby)
            (Regex::new(r"(?i)\beval\s*\(").unwrap(),                          Some("ts"),   "A03", "Injection",        OwaspSeverity::Critical,
             "Untrusted input passed to eval()",                               "Avoid eval(); use JSON.parse() or safe parsers."),
            (Regex::new(r"(?i)\bexec\s*\(").unwrap(),                         Some("python"), "A03", "Injection",     OwaspSeverity::Critical,
             "Use of exec() on untrusted data",                                "Remove exec(); validate all input with allow-lists."),
            (Regex::new(r"(?i)\bFunction\s*\(").unwrap(),                      Some("ts"),   "A03", "Injection",        OwaspSeverity::Critical,
             "Dynamic function creation from strings",                         "Avoid Function(); use static function definitions."),

            // A03 - SQL Injection: string-concat in SQL-like strings
            (Regex::new(r#"(?i)(SELECT|INSERT|UPDATE|DELETE|DROP)\s+[^;]*(\+|\$\{|\{|\{|%s|%d)"#).unwrap(), None, "A03", "SQL Injection", OwaspSeverity::High,
             "SQL built via string concatenation or interpolation",           "Use parameterized queries / prepared statements."),

            // A03 - Command Injection
            (Regex::new(r#"(?i)(os\.system|subprocess\.call|subprocess\.run|subprocess\.Popen)\s*\([^)]*(shell\s*=\s*True)"#).unwrap(), Some("python"), "A03", "Command Injection", OwaspSeverity::High,
             "Command execution via shell=True or string formatting",          "Pass arguments as lists (not shell strings) and validate."),
            (Regex::new(r#"(?i)child_process\.(exec|execSync)\s*\([^)]*\+[^)]*\)"#).unwrap(), Some("ts"), "A03", "Command Injection", OwaspSeverity::High,
             "Node child_process.exec with string concatenation",             "Use child_process.execFile or spawn with argument arrays."),
            (Regex::new(r#"(?i)\.arg\s*\(\s*format!"#).unwrap(),                  Some("rust"), "A03", "Command Injection", OwaspSeverity::Medium,
             "Command arguments built with format!",                            "Use separate .arg() calls; never interpolate user data."),

            // A04 - Insecure Design: pickle / yaml.load
            (Regex::new(r#"(?i)\bpickle\.loads?\s*\("#).unwrap(),               Some("python"), "A04", "Insecure Deserialization", OwaspSeverity::Critical,
             "Deserialization of untrusted data with pickle",                   "Use JSON or MessagePack; never unpickle untrusted input."),
            (Regex::new(r#"(?i)\byaml\.load\s*\("#).unwrap(),                  Some("python"), "A04", "Insecure Deserialization", OwaspSeverity::High,
             "yaml.load() is unsafe; yaml.safe_load() should be used",         "Replace yaml.load() with yaml.safe_load()."),

            // A05 - Security Misconfiguration
            (Regex::new(r#"(?i)(DEBUG\s*=\s*True|debug:\s*true|APP_DEBUG\s*=\s*true)"#).unwrap(), None, "A05", "Security Misconfiguration", OwaspSeverity::Medium,
             "Debug mode enabled in production-like code",                      "Set DEBUG=False/False in production; read from env vars."),
            (Regex::new(r#"(?i)(CORS_ORIGIN_ALLOW_ALL|Access-Control-Allow-Origin\s*:\s*\*)"#).unwrap(), None, "A05", "Security Misconfiguration", OwaspSeverity::Medium,
             "Permissive CORS wildcard allows any origin",                     "Restrict origins to an allowed list in production."),

            // A07 - ID & Auth Failures / Cryptographic Failures
            (Regex::new(r#"(?i)(md5|sha1)\s*\("#).unwrap(),                    None, "A07", "Cryptographic Failure",     OwaspSeverity::Medium,
             "Weak hash algorithm (MD5 or SHA1) detected",                     "Use SHA-256+ or Argon2 for passwords, Blake3 for checksums."),
            (Regex::new(r#"(?i)(password|passwd|pwd|secret|token|api_key)\s*=\s*['\"][^'\"]+['\"]"#).unwrap(), None, "A07", "Hardcoded Secret", OwaspSeverity::High,
             "Possible hardcoded credential in source",                         "Load secrets from environment variables or a vault."),
            (Regex::new(r#"(?i)(DISABLE_SSL_VERIFICATION|tls_verify\s*=\s*false|verify\s*:\s*false)"#).unwrap(), None, "A07", "Insecure Transport", OwaspSeverity::High,
             "TLS/SSL certificate verification disabled",                       "Never disable TLS verification in production."),

            // A08 - Software & Data Integrity Failures
            (Regex::new(r#"(?i)InsecureRequestWarning|urllib3\.disable_warnings|warnings\.filterwarnings\s*\([^)]*ignore"#).unwrap(), Some("python"), "A08", "Integrity Failure", OwaspSeverity::Low,
             "Security warnings suppressed",                                     "Handle warnings properly; do not blanket-ignore them."),

            // A09 - Security Logging Failures
            (Regex::new(r#"(?i)catch\s*\{[^}]*\}|catch\s*\([^)]*\)\s*\{[^}]*\}|except\s*[^:]+:\s*pass|except:\s*pass"#).unwrap(), None, "A09", "Logging Failure", OwaspSeverity::Medium,
             "Empty catch / except block swallows errors silently",              "Log exceptions before suppressing; never silently pass."),

            // A10 - SSRF
            (Regex::new(r#"(?i)(http\.Get|http\.Post|fetch\s*\(|reqwest::get|axios\.|curl_exec)\s*\([^)]*(req\.|[a-zA-Z_]*(request|params|body|input|user))"#).unwrap(), None, "A10", "SSRF", OwaspSeverity::High,
             "HTTP request built directly from user input",                      "Validate and sanitize URLs against an allow-list."),

            // Extra - Memory Safety
            (Regex::new(r#"(?i)\bunsafe\s*\{"#).unwrap(),                         Some("rust"), "A04", "Memory Safety",          OwaspSeverity::Medium,
             "unsafe block detected",                                           "Minimize unsafe; document invariants and get review."),
            (Regex::new(r#"(?i)\bunsafe\s*fn\b"#).unwrap(),                       Some("rust"), "A04", "Memory Safety",          OwaspSeverity::Medium,
             "unsafe function detected",                                        "Require audit for every unsafe fn; prefer safe APIs."),

            // Extra - Raw pointers (C/C++ / Rust)
            (Regex::new(r#"(?i)\bgets\s*\("#).unwrap(),                           None, "A03", "Buffer Overflow",         OwaspSeverity::Critical,
             "gets() is unsafe and removed in C11",                             "Use fgets() or getline() with length limits."),
            (Regex::new(r#"(?i)\bstrcpy\s*\("#).unwrap(),                         None, "A03", "Buffer Overflow",         OwaspSeverity::High,
             "strcpy() can overflow; use strncpy or strlcpy",                  "Replace strcpy with strncpy/strlcpy."),
            (Regex::new(r#"(?i)\bstrcat\s*\("#).unwrap(),                         None, "A03", "Buffer Overflow",         OwaspSeverity::High,
             "strcat() can overflow; use strncat",                              "Replace strcat with strncat/strlcat."),
        ]
    });

    // Scan files
    let mut total_scanned = 0usize;
    for file in &files {
        total_scanned += 1;

        // Skip documentation directories — they contain example vulnerability strings
        let file_str = file.to_string_lossy();
        if file_str.contains("/.agent/") || file_str.contains("/docs/") {
            continue;
        }

        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        let lang = lang_exts.iter().find(|(e, _)| *e == ext).map(|(_, l)| *l);

        for (line_no, line) in text.lines().enumerate() {
            let trimmed = line.trim();

            // Skip comment lines and string literals that contain patterns
            if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("*")
                || trimmed.starts_with("##") || trimmed.starts_with("#") {
                continue;
            }

            // Skip lines that define regex patterns or are clearly string literals
            if trimmed.starts_with('"') || trimmed.starts_with('\'') || trimmed.contains("Regex::new") {
                continue;
            }
            // Skip debug output statements (println!, eprintln!, log::*, format!) - not actual SQL
            if (trimmed.contains("println!") || trimmed.contains("eprintln!") || trimmed.contains("format!"))
                && (trimmed.contains("SELECT") || trimmed.contains("INSERT") || trimmed.contains("UPDATE")
                    || trimmed.contains("DELETE") || trimmed.contains("DROP")) {
                    continue; // These are debug strings, not SQL queries
                }

            for (re, pat_lang, owasp_id, category, severity, desc, fix) in patterns.iter() {
                if let Some(pl) = pat_lang
                    && Some(*pl) != lang { continue; }
                if re.is_match(line) {
                    findings.push(OwaspFinding {
                        owasp_id,
                        category,
                        severity: *severity,
                        file: file.clone(),
                        line: line_no + 1,
                        snippet: line.trim().to_string(),
                        description: desc,
                        remediation: fix,
                    });
                }
            }
        }
    }

    // Output
    let is_json = format == "json";
    let is_adoc = format == "adoc";

    if findings.is_empty() {
        if is_json {
            println!("{{\"findings\": [], \"summary\": {{\"total\": 0, \"critical\": 0, \"high\": 0, \"medium\": 0, \"low\": 0, \"info\": 0, \"scanned\": {total_scanned}}}}}");
        } else if is_adoc {
            println!("= OWASP Security Audit\n:date: {}\n\n== Summary\n\n| Severity | Count\n| Critical | 0\n| High     | 0\n| Medium   | 0\n| Low      | 0\n| Info     | 0\n\n_{total_scanned} files scanned. No findings._\n", aden_core::rfc3339_now().split('T').next().unwrap_or(""));
        } else {
            println!("  No OWASP coding vulnerabilities found in {total_scanned} file(s).");
        }
        return Ok(());
    }

    // Sort by severity descending
    findings.sort_by_key(|b| std::cmp::Reverse(b.severity));

    let counts = |sev: OwaspSeverity| findings.iter().filter(|f| f.severity == sev).count();
    let crit = counts(OwaspSeverity::Critical);
    let high = counts(OwaspSeverity::High);
    let med  = counts(OwaspSeverity::Medium);
    let low  = counts(OwaspSeverity::Low);
    let info = counts(OwaspSeverity::Info);

    if is_json {
        println!("{{");
        println!("  \"findings\": [");
        for (i, f) in findings.iter().enumerate() {
            let comma = if i + 1 < findings.len() { "," } else { "" };
            println!("    {{");
            println!("      \"owasp_id\": \"{}\"," , f.owasp_id);
            println!("      \"category\": \"{}\"," , f.category);
            println!("      \"severity\": \"{}\"," , f.severity);
            println!("      \"file\": \"{}\"," , f.file.display());
            println!("      \"line\": {}," , f.line);
            println!("      \"snippet\": \"{}\"," , f.snippet.replace('"', "\\\""));
            println!("      \"description\": \"{}\"," , f.description.replace('"', "\\\""));
            println!("      \"remediation\": \"{}\"" , f.remediation.replace('"', "\\\""));
            println!("    }}{comma}");
        }
        println!("  ],");
        println!("  \"summary\": {{");
        println!("    \"total\": {}, \"critical\": {}, \"high\": {}, \"medium\": {}, \"low\": {}, \"info\": {}, \"scanned\": {}",
            findings.len(), crit, high, med, low, info, total_scanned);
        println!("  }}");
        println!("}}");
    } else if is_adoc {
        let header = format!("= OWASP Security Audit Report\n:date: {}\n:toc: auto\n\n== Summary\n\n| Severity | Count\n| Critical | {crit}\n| High     | {high}\n| Medium   | {med}\n| Low      | {low}\n| Info     | {info}\n\n_{total_scanned} files scanned._\n\n== Findings\n",
            aden_core::rfc3339_now().split('T').next().unwrap_or(""));
        print!("{header}");
        for f in &findings {
            println!("=== [{} {}] {}:{}\n\n`{}`\n\n*Description:* {}\n\n*Remediation:* {}\n", f.severity, f.owasp_id, f.file.display(), f.line, f.snippet, f.description, f.remediation);
        }
    } else {
        println!("  === OWASP Security Audit Findings ===");
        println!("  {} file(s) scanned | {} total finding(s)", total_scanned, findings.len());
        println!("  Severity counts: CRIT={crit} HIGH={high} MED={med} LOW={low} INFO={info}");
        println!();
        for f in &findings {
            println!("  [{}] {} | {}:{}\n    Code: {}\n    {}\n    Fix: {}\n", f.severity, f.owasp_id, f.file.display(), f.line, f.snippet, f.description, f.remediation);
        }
    }

    if strict && (crit > 0 || high > 0) {
        return Err(format!("{} critical/high OWASP finding(s) detected (strict mode)", crit + high).into());
    }
    Ok(())
}

pub fn run_project_tests(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let has_cargo = path.join("Cargo.toml").exists();
    let has_go_mod = path.join("go.mod").exists();
    let has_pkg_json = path.join("package.json").exists();
    let has_pyproject = path.join("pyproject.toml").exists();
    let has_setup_py = path.join("setup.py").exists();
    let has_reqs = path.join("requirements.txt").exists();

    if has_cargo {
        let output = std::process::Command::new("cargo")
            .args(["test", "--workspace", "--quiet"])
            .current_dir(path)
            .output()?;
        if !output.status.success() {
            return Err(format!("cargo test failed:\n{}", String::from_utf8_lossy(&output.stderr)).into());
        }
        return Ok(());
    }

    if has_go_mod {
        let output = std::process::Command::new("go")
            .args(["test", "./..."])
            .current_dir(path)
            .output()?;
        if !output.status.success() {
            return Err(format!("go test failed:\n{}", String::from_utf8_lossy(&output.stderr)).into());
        }
        return Ok(());
    }

    if has_pkg_json {
        let runner = if std::process::Command::new("npm").arg("--version").output().is_ok() {
            "npm"
        } else if std::process::Command::new("yarn").arg("--version").output().is_ok() {
            "yarn"
        } else if std::process::Command::new("pnpm").arg("--version").output().is_ok() {
            "pnpm"
        } else {
            return Err("No JS package manager found (npm/yarn/pnpm)".into());
        };
        let output = std::process::Command::new(runner)
            .args(["test"])
            .current_dir(path)
            .output()?;
        if !output.status.success() {
            return Err(format!("{} test failed:\n{}", runner, String::from_utf8_lossy(&output.stderr)).into());
        }
        return Ok(());
    }

    if has_pyproject || has_setup_py || has_reqs {
        let output = std::process::Command::new("pytest")
            .args(["-q"])
            .current_dir(path)
            .output()?;
        if output.status.success() {
            return Ok(());
        }
        let output = std::process::Command::new("python")
            .args(["-m", "pytest", "-q"])
            .current_dir(path)
            .output()?;
        if output.status.success() {
            return Ok(());
        }
        return Err("Python tests failed or no test runner found (tried pytest, python -m pytest)".into());
    }

    Err("No recognized test framework found (checked Cargo.toml, go.mod, package.json, pyproject.toml, setup.py, requirements.txt)".into())
}

pub fn cmd_ci_check(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut exit_code = 0i32;
    let mut warnings = Vec::new();
    let green = "\x1b[0;32m";
    let red = "\x1b[0;31m";
    let yellow = "\x1b[1;33m";
    let reset = "\x1b[0m";

    macro_rules! gate {
        ($name:expr, $cmd:expr) => {{
            println!("[CI] Running: {} ...", $name);
            match $cmd {
                Ok(_) => println!("{}[CI] PASS: {}{}", green, $name, reset),
                Err(e) => {
                    println!("{}[CI] FAIL: {} — {}{}", red, $name, e, reset);
                    exit_code = 1;
                }
            }
        }};
    }

    macro_rules! warn {
        ($name:expr, $cmd:expr) => {{
            println!("[CI] Checking: {} ...", $name);
            match $cmd {
                Ok(()) => println!("{}[CI] OK:   {}{}", green, $name, reset),
                Err(e) => {
                    println!("{}[CI] WARN: {} — {}{}", yellow, $name, e, reset);
                    warnings.push(format!("{}: {}", $name, e));
                }
            }
        }};
    }

    // ── BLOCKING GATES ────────────────────────────────────
    gate!("aden check", {
        if !path.is_dir() { Err("not a directory".into()) }
        else { crate::util::perform_check(path).map(|_| ()) }
    });

    gate!("constitutional firewall", {
        // Verify bootstrap constitution exists and is valid
        let constitution_path = path.join(".aden/constitution.adoc");
        if !constitution_path.exists() {
            Err("Missing .aden/constitution.adoc — run 'aden init' or create bootstrap constitution".into())
        } else {
            // Validate constitution can be parsed
            aden_policy::PolicyEngine::load_bootstrap(path)
                .map(|_| ())
                .map_err(|e| -> Box<dyn std::error::Error> { format!("Invalid bootstrap constitution: {}", e).into() })
        }
    });

    gate!("tests", {
        run_project_tests(path)
    });

    gate!("aden lint", {
        crate::commands::cmd_lint(path, "Error", false, false)
    });

    gate!("secret scan", {
        use aden_core::filter::AdenFilter;
        use regex::Regex;
        use std::sync::OnceLock;

        static SECRET_PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
        let patterns = SECRET_PATTERNS.get_or_init(|| {
            vec![
                (Regex::new(r"-----BEGIN (RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----").unwrap(), "private key"),
                (Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(), "AWS access key"),
                (Regex::new(r"ghp_[a-zA-Z0-9]{36}").unwrap(), "GitHub token"),
                (Regex::new(r"gho_[a-zA-Z0-9]{36}").unwrap(), "GitHub OAuth"),
                (Regex::new(r"\b[0-9a-zA-Z]{32,64}\b").unwrap(), "long hex secret (possible API key)"),
                (Regex::new(r#"api[_-]?key\s*=\s*['\"][^'\"]{8,}['\"]"#).unwrap(), "API key assignment"),
                (Regex::new(r#"password\s*=\s*['\"][^'\"]{4,}['\"]"#).unwrap(), "hardcoded password"),
                (Regex::new(r#"secret\s*=\s*['\"][^'\"]{8,}['\"]"#).unwrap(), "hardcoded secret"),
                (Regex::new(r#"token\s*=\s*['\"][^'\"]{8,}['\"]"#).unwrap(), "hardcoded token"),
                (Regex::new(r"eyJ[a-zA-Z0-9_-]*\.eyJ[a-zA-Z0-9_-]*\.[a-zA-Z0-9_-]*").unwrap(), "JWT token"),
                (Regex::new(r"bearer\s+[a-zA-Z0-9_\-\.]{20,}").unwrap(), "Bearer token"),
                (Regex::new(r"mongodb(\+srv)?://[^:]+:[^@]+@").unwrap(), "MongoDB connection string"),
                (Regex::new(r"postgres(ql)?://[^:]+:[^@]+@").unwrap(), "PostgreSQL connection string"),
                (Regex::new(r"mysql://[^:]+:[^@]+@").unwrap(), "MySQL connection string"),
                (Regex::new(r"redis://:[^@]+@").unwrap(), "Redis connection string"),
                (Regex::new(r"\.env\.[a-zA-Z]+\s*\n").unwrap(), "env file"),
                (Regex::new(r"DATABASE_URL\s*=\s*").unwrap(), "DATABASE_URL"),
                (Regex::new(r"sk-[a-zA-Z0-9]{48,}").unwrap(), "OpenAI/sk key"),
            ]
        });

        let non_text_exts: std::collections::HashSet<&str> = [
            "png", "jpg", "jpeg", "gif", "svg", "ico", "bmp",
            "pdf", "zip", "tar", "gz", "bz2", "xz", "7z", "rar",
            "mp3", "mp4", "avi", "mov", "mkv", "wav", "flac",
            "wasm", "so", "dll", "dylib", "exe", "bin", "o", "a",
            "ttf", "otf", "woff", "woff2", "eot", "jpg", "mp3", "mp4",
        ].iter().copied().collect();

        const MAX_SCAN_SIZE: u64 = 1024 * 1024;
        let mut found = 0;
        let filter = AdenFilter::from_directory(path);

        for entry in walkdir::WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if !p.is_file() { continue; }
            if let Ok(rel) = p.strip_prefix(path)
                && filter.should_skip(rel) { continue; }
            // Exclude cache and generated files from secret scan
            let rel_path = p.strip_prefix(path).unwrap_or(p.as_ref());
            if rel_path.starts_with(".aden/cache") { continue; }
            if rel_path.file_name().is_some_and(|n| n == "Cargo.lock") { continue; }
            if rel_path.file_name().is_some_and(|n| n == "cache-index.json") { continue; }
            if let Some(ext) = p.extension().and_then(|e| e.to_str())
                && non_text_exts.contains(ext.to_lowercase().as_str()) { continue; }
            if let Ok(meta) = std::fs::metadata(p)
                && meta.len() > MAX_SCAN_SIZE { continue; }
            if let Ok(text) = std::fs::read_to_string(p) {
                for (re, name) in patterns {
                    for cap in re.find_iter(&text) {
                        let line_start = text[..cap.start()].rfind('\n').map(|i| i + 1).unwrap_or(0);
                        let line_end = text[cap.end()..].find('\n').map(|i| cap.end() + i).unwrap_or(text.len());
                        let line = &text[line_start..line_end];
                        // Skip pattern definitions and test files
                        if line.contains("Regex::new") { continue; }
                        if rel_path.starts_with("tools/") { continue; }
                        if rel_path.to_string_lossy().contains("/tests/") { continue; }
                        if *name == "env file" {
                            let trimmed = line.trim();
                            if trimmed.starts_with(".env") || trimmed.starts_with("*.env") {
                                continue;
                            }
                        }
                        let snippet = &text[cap.start().saturating_sub(20)..(cap.end() + 20).min(text.len())];
                        println!("  {}Secret ({}) in {}: ...{}...{}", red, name, p.display(), snippet.replace('\n', " "), reset);
                        found += 1;
                    }
                }
            }
        }

        if found > 0 {
            Err(Box::<dyn std::error::Error>::from(format!("{} secret pattern(s) detected", found)))
        } else {
            Ok(())
        }
    });

    gate!("accreditation check", {
        if path.join("Cargo.lock").exists() && !path.join("NOTICE.md").exists() {
            Err(Box::<dyn std::error::Error>::from("NOTICE.md missing. Run 'aden licenses --out NOTICE.md'.".to_string()))
        } else {
            Ok(())
        }
    });

    gate!("owasp audit", {
        cmd_audit(path, None, "text", true)
    });

    gate!("merge conflict markers", {
        use aden_core::filter::AdenFilter;
        let mut found = 0;
        let filter = AdenFilter::from_directory(path);
        for entry in walkdir::WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if !p.is_file() { continue; }
            if let Ok(rel) = p.strip_prefix(path)
                && filter.should_skip(rel) { continue; }
            if let Ok(text) = std::fs::read_to_string(p) {
                for line in text.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("<<<<<<< ") || trimmed.starts_with(">>>>>>> ") || trimmed == "=======" {
                        println!("  {}Merge conflict marker in {}: {}{}", red, p.display(), trimmed, reset);
                        found += 1;
                    }
                }
            }
        }
        if found > 0 {
            Err(Box::<dyn std::error::Error>::from(format!("{} merge conflict marker(s) detected", found)))
        } else {
            Ok(())
        }
    });

    warn!("insecure protocol", {
        use aden_core::filter::AdenFilter;
        let mut found = 0;
        let insecure_re = Regex::new(r"(?i)http://\S+").unwrap();
        let skip_exts: std::collections::HashSet<&str> = ["lock", "adoc", "md", "txt", "svg", "html", "xml"].iter().copied().collect();
        let filter = AdenFilter::from_directory(path);
        for entry in walkdir::WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if !p.is_file() { continue; }
            if let Ok(rel) = p.strip_prefix(path)
                && filter.should_skip(rel) { continue; }
            if let Some(ext) = p.extension().and_then(|e| e.to_str())
                && skip_exts.contains(ext) { continue; }
            if let Ok(text) = std::fs::read_to_string(p) {
                for line in text.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("//") || trimmed.starts_with("#") || trimmed.starts_with("<!--") {
                        continue;
                    }
                    if line.contains("Regex::new") || line.contains("xmlns=") {
                        continue;
                    }
                    if insecure_re.is_match(line) {
                        println!("  {}Insecure http:// URL in {}: {}{}", red, p.display(), line.trim(), reset);
                        found += 1;
                    }
                }
            }
        }
        if found > 0 {
            Err(Box::<dyn std::error::Error>::from(format!("{} insecure http:// URL(s) detected", found)))
        } else {
            Ok(())
        }
    });

    // ── WARNING GATES ─────────────────────────────────────
    warn!("cargo clippy", {
        if !path.join("Cargo.toml").exists() {
            Ok(())
        } else {
            let output = std::process::Command::new("cargo")
                .args(["clippy", "--workspace", "--", "-W", "clippy::unwrap_used", "-W", "clippy::expect_used", "-W", "clippy::panic"])
                .current_dir(path)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()?;
            if output.status.success() {
                Ok(())
            } else {
                Err(Box::<dyn std::error::Error>::from(
                    format!("cargo clippy found issues:\n{}", String::from_utf8_lossy(&output.stderr))
                ))
            }
        }
    });

    warn!("cargo audit", {
        if !path.join("Cargo.toml").exists() {
            Ok(())
        } else {
            let output = std::process::Command::new("cargo")
                .args(["audit"])
                .current_dir(path)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()?;
            if output.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("not found") || stderr.contains("No such file") {
                    Err(Box::<dyn std::error::Error>::from("cargo audit not installed. Install with: cargo install cargo-audit".to_string()))
                } else {
                    Err(Box::<dyn std::error::Error>::from(
                        format!("cargo audit found vulnerabilities:\n{}", stderr)
                    ))
                }
            }
        }
    });

    warn!("contract freshness", {
        use aden_heal::{Scanner, generate};
        let scanner = Scanner::new(path);
        let events = scanner.scan()?;
        let report = generate(events.clone(), path);
        let critical_count = events.iter().filter(|e| {
            matches!(e, aden_heal::DriftEvent::BrokenReference { .. }
                | aden_heal::DriftEvent::OrphanAnchor { .. }
                | aden_heal::DriftEvent::SignatureMismatch { .. })
        }).count();
        if critical_count > 0 {
            Err(Box::<dyn std::error::Error>::from(format!("{} critical drift events (broken refs, orphans, signature mismatch)", critical_count)))
        } else if report.overall_score < 0.99 {
            Err(Box::<dyn std::error::Error>::from(format!("Health score: {:.2} — contracts need regeneration (run 'aden gen' on modified files)", report.overall_score)))
        } else {
            Ok(())
        }
    });

    // ── Final Verdict ─────────────────────────────────────
    if !warnings.is_empty() {
        println!("\n{}[CI] WARNINGS (non-blocking):{}", yellow, reset);
        for w in &warnings {
            println!("  ⚠ {}", w);
        }
        println!("  Run 'aden gen <file>' on modified source to clear.\n");
    }

    if exit_code != 0 {
        println!("\n{}[CI] GATES BLOCKED — Fix errors above before committing.{}", red, reset);
        std::process::exit(exit_code);
    }
    println!("\n{}[CI] ALL GATES PASSED — Ready to commit.{}", green, reset);
    Ok(())
}

pub fn cmd_doctor(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    println!("Aden Doctor — Environment Diagnostics");
    println!("═══════════════════════════════════════\n");

    let mut issues = Vec::new();

    // Tool availability
    for tool in &["rustc", "cargo", "git"] {
        if std::process::Command::new(tool).arg("--version").output().is_ok() {
            println!("✓ {} found", tool);
        } else {
            println!("✗ {} NOT FOUND", tool);
            issues.push(format!("{} not in PATH", tool));
        }
    }

    // Aden binary
    if std::process::Command::new("aden").arg("--version").output().is_ok() {
        println!("✓ aden CLI found in PATH");
    } else {
        println!("✗ aden CLI NOT in PATH (build or install: cargo install --path crates/aden-cli)");
        issues.push("aden CLI not in PATH".to_string());
    }

    // Signing keys
    let key_dir = dirs::home_dir().unwrap_or_default().join(".aden").join("keys");
    if key_dir.join("aden-sign.pub").exists() {
        println!("✓ Signing public key: {}", key_dir.join("aden-sign.pub").display());
    } else {
        println!("⚠ No signing key found. Generate with:");
        println!("    mkdir -p ~/.aden/keys && cd ~/.aden/keys");
        println!("    ssh-keygen -t ed25519 -C 'aden-sign' -N '' -f aden-sign");
        issues.push("No ~/.aden/keys/aden-sign.pub".to_string());
    }

    // Repo health
    println!("\n— Repo Health —");
    if path.join(".agent").is_dir() {
        println!("✓ .agent/ directory present");
    } else {
        println!("✗ .agent/ MISSING — run 'aden init' in this repo");
        issues.push("No .agent/ directory".to_string());
    }

    if path.join(".adenignore").exists() {
        println!("✓ .adenignore present");
    } else {
        println!("⚠ .adenignore missing — using built-in defaults");
    }

    if path.join("NOTICE.md").exists() {
        println!("✓ NOTICE.md present — accreditation is tracked");
    } else {
        println!("⚠ NOTICE.md missing — run 'aden licenses --out NOTICE.md'");
        issues.push("No NOTICE.md — third-party attribution not tracked".to_string());
    }

    // Quick heal score
    println!("\n— Quick Scan —");
    if let Ok(score) = quick_health_score(path) {
        const EPSILON: f64 = 0.01;
        if (1.0 - score).abs() < EPSILON {
            println!("✓ Health Score: {:.2}/1.00", score);
        } else {
            println!("⚠ Health Score: {:.2}/1.00 (run 'aden heal .' to see drift)", score);
            issues.push(format!("Health score {:.2} (target 1.00)", score));
        }
    }

    // Self-documenting docs check
    println!("\n— Self-Documenting Docs —");
    let self_docs = [
        ("docs/module-aden-cli.adoc", "CLI reference + troubleshooting"),
        ("docs/getting-started.adoc", "Quick start guide"),
        (".agent/onboarding.adoc", "Agent onboarding"),
    ];

    let mut found_self_docs = 0;
    for (doc_path, desc) in &self_docs {
        if path.join(doc_path).exists() {
            println!("✓ {} ({})", doc_path, desc);
            found_self_docs += 1;
        } else {
            println!("✗ {} MISSING ({})", doc_path, desc);
            issues.push(format!("Missing: {}", doc_path));
        }
    }

    if found_self_docs >= 2 {
        println!("✓ Sufficient self-documenting docs for AI agents");
    } else {
        println!("⚠ Run 'aden init' to scaffold self-documenting docs");
    }

    println!("\n═══════════════════════════════════════");
    if issues.is_empty() {
        println!("All diagnostics passed. Environment is healthy.");
    } else {
        println!("{} issue(s) found:", issues.len());
        for i in &issues {
            println!("  - {}", i);
        }
    }
    Ok(())
}

pub fn cmd_review(path: &Path, budget: usize) -> Result<(), Box<dyn std::error::Error>> {
    use aden_propose::list;

    println!("Aden Semantic Review Engine (Budget: {} tokens)", budget);
    println!("================================================");

    if !path.join(".aden").join("proposals").exists() {
        println!("No proposals directory found. Run \"aden heal --scan . --propose\" first.");
        return Ok(());
    }

    let proposals = list(path)?;
    let low_confidence: Vec<_> = proposals.iter()
        .filter(|p| p.confidence < 0.85)
        .collect();

    if low_confidence.is_empty() {
        println!("No low-confidence proposals found. All drift detected is auto-applyable.");
        return Ok(());
    }

    println!("Reviewing {} low-confidence proposals...\n", low_confidence.len());

    let estimated_tokens = low_confidence.len() * 100;
    println!("Estimated review cost: ~{} tokens (budget: {})", estimated_tokens, budget);

    if estimated_tokens > budget {
        println!("WARNING: Review exceeds budget. Showing first {} proposals.", budget / 100);
    }

    let show_count = (budget / 100).min(low_confidence.len());
    for (i, proposal) in low_confidence.iter().take(show_count).enumerate() {
        println!("\n{}. Proposal {} (confidence: {:.2})", i + 1, proposal.id, proposal.confidence);
        println!("   Target: {}", proposal.target_path.display());
        println!("   Drift Type: {}", proposal.drift_type);
        println!("   Rationale: {}", proposal.rationale.lines().next().unwrap_or("(none)"));
    }

    if show_count < low_confidence.len() {
        println!("\n... and {} more proposals (increase --budget to see all)", low_confidence.len() - show_count);
    }

    println!("\nReview each proposal file in .aden/proposals/ before applying.");
    Ok(())
}

pub fn cmd_review_since(path: &Path, budget: usize, since: &str) -> Result<(), Box<dyn std::error::Error>> {
    use aden_heal::Scanner;

    println!("Reviewing changes since '{}' with budget {} tokens", since, budget);

    let output = std::process::Command::new("git")
        .args(["diff", "--name-only", "--", since])
        .current_dir(path)
        .output()?;

    let changed = String::from_utf8_lossy(&output.stdout);
    let files: Vec<&str> = changed.lines().filter(|l| !l.is_empty()).collect();

    if files.is_empty() {
        println!("No files changed since {}.", since);
        return Ok(());
    }

    println!("Files changed since '{}': {} files", since, files.len());
    for f in &files {
        println!("  - {}", f);
    }

    println!("\nRunning targeted drift scan...");
    let scanner = Scanner::new(path);
    let all_events = scanner.scan()?;

    let relevant_events: Vec<_> = all_events.into_iter()
        .filter(|e| {
            let target = match e {
                aden_heal::DriftEvent::StaleHash { target_path, .. } => target_path,
                aden_heal::DriftEvent::SignatureMismatch { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::MissingContract { source_path, .. } => source_path,
                aden_heal::DriftEvent::OrphanAnchor { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::BrokenReference { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::DeadLink { contract_path, .. } => contract_path,
                aden_heal::DriftEvent::MarkdownDrift { md_path, .. } => md_path,
                aden_heal::DriftEvent::StaleMarkdown { md_path, .. } => md_path,
                aden_heal::DriftEvent::MissingMarkdownTemplate { md_path, .. } => md_path,
            };
            files.iter().any(|f| target.contains(f))
        })
        .collect();

    if relevant_events.is_empty() {
        println!("No drift detected in changed files.");
        return Ok(());
    }

    println!("Found {} drift events in changed files.", relevant_events.len());

    let show_count = (budget / 100).min(relevant_events.len());
    for (i, event) in relevant_events.iter().take(show_count).enumerate() {
        println!("  {}. {:?}", i + 1, event);
    }
    if show_count < relevant_events.len() {
        println!("  ... and {} more (increase --budget)", relevant_events.len() - show_count);
    }

    Ok(())
}

pub fn cmd_licenses(
    repo_path: &Path,
    out: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let lock_path = repo_path.join("Cargo.lock");
    if !lock_path.exists() {
        return Err(format!(
            "Cargo.lock not found at {}. Run 'cargo generate-lockfile' first.",
            lock_path.display()
        )
        .into());
    }

    let content = std::fs::read_to_string(&lock_path)?;
    let mut packages: Vec<(String, String)> = Vec::new();
    let mut current_name: Option<String> = None;
    let mut is_aden_crate = true;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[[package]]") {
            is_aden_crate = true;
            current_name = None;
        } else if trimmed.starts_with("name = ") {
            let name = trimmed
                .trim_start_matches("name = ")
                .trim_matches('"')
                .to_string();
            if !name.starts_with("aden") && name != "aden_py" {
                is_aden_crate = false;
            }
            current_name = Some(name);
        } else if trimmed.starts_with("version = ") && !is_aden_crate
            && let Some(name) = current_name.clone() {
                let version = trimmed
                    .trim_start_matches("version = ")
                    .trim_matches('"')
                    .to_string();
                packages.push((name, version));
            }
    }

    packages.sort_by(|a, b| a.0.cmp(&b.0));
    packages.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);

    let mut markdown = String::new();
    markdown.push_str("# Third-Party Dependencies\n\n");
    markdown.push_str("This project uses the following open-source packages.\n");
    markdown.push_str("Generated by `aden licenses`.\n");
    markdown.push_str("For full license texts, see the respective package repositories or `Cargo.lock`.\n\n");
    markdown.push_str("| Package | Version |\n");
    markdown.push_str("|---------|---------|\n");
    for (name, version) in &packages {
        markdown.push_str(&format!("| {} | {} |\n", name, version));
    }
    markdown.push('\n');
    markdown.push_str("## Attribution\n\n");
    markdown.push_str("All third-party packages are used in accordance with their respective licenses.\n");
    markdown.push_str("No proprietary code is bundled or modified without explicit permission.\n");
    markdown.push_str("\n---\nGenerated by Aden.\n");

    if let Some(out_path) = out {
        std::fs::write(out_path, &markdown)?;
        println!("Wrote third-party attribution to {}", out_path.display());
    } else {
        println!("{}", markdown);
    }

    Ok(())
}

pub fn cmd_emergency(
    path: &Path,
    reason: &str,
    ttl: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let ttl_seconds = match ttl {
        "1h" => 3600,
        "24h" => 86400,
        "7d" => 604800,
        _ => return Err(format!("Invalid TTL '{}': use 1h, 24h, or 7d", ttl).into()),
    };

    let aden_dir = path.join(".aden");
    if !aden_dir.exists() {
        std::fs::create_dir_all(&aden_dir)?;
    }

    let timestamp = chrono::Utc::now();
    let expires_at = timestamp + chrono::Duration::seconds(ttl_seconds);
    let tag = format!("emergency-{}", timestamp.format("%Y%m%d-%H%M%S"));

    let audit_log_path = aden_dir.join("emergency-audit.log");
    let audit_entry = format!(
        "[{}] EMERGENCY OVERRIDE created: reason='{}', expires={}, tag={}\n",
        timestamp.to_rfc3339(),
        reason,
        expires_at.to_rfc3339(),
        tag
    );

    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_log_path)?;
    use std::io::Write;
    audit_file.write_all(audit_entry.as_bytes())?;

    let emergency_path = aden_dir.join("emergency-overrides.adoc");
    let content = format!(
        "[[{}]]\n= Emergency Override\n\n[override#{}]\n----\nEmergency override: all Forbid directives downgraded to Warn.\nExpires: {}\nReason: {}\n----\n",
        tag,
        tag,
        expires_at.to_rfc3339(),
        reason
    );

    std::fs::write(&emergency_path, content)?;

    println!("[{}] EMERGENCY OVERRIDE created", tag);
    println!("  Reason: {}", reason);
    println!("  Expires: {}", expires_at.to_rfc3339());
    println!("  File: {}", emergency_path.display());
    println!("  Audit: {}", audit_log_path.display());

    Ok(())
}

pub fn cmd_suggest(intent: &str) -> Result<(), Box<dyn std::error::Error>> {
    let intent_lower = intent.to_lowercase();

    let suggestions = vec![
        (vec!["generate", "doc", "contract", "parse", "extract"], "gen", "aden gen . --auto", "Generate contracts from source code"),
        (vec!["search", "find", "look"], "search", "aden search '<query>'", "Search for text in contracts"),
        (vec!["list", "show", "all", "anchors", "contracts"], "list", "aden list .", "List all anchors in the graph"),
        (vec!["ask", "question", "explain", "how", "what"], "ask", "aden ask '<question>'", "Ask a natural language question"),
        (vec!["fix", "heal", "drift", "stale", "update"], "heal", "aden heal . --fix", "Auto-fix stale contracts"),
        (vec!["check", "validate", "reference", "link"], "check", "aden check .", "Validate all cross-references"),
        (vec!["graph", "depend", "neighbor", "related"], "graph", "aden graph --from <anchor> --depth 2", "Show graph neighborhood"),
        (vec!["assemble", "context", "prompt", "token"], "asm", "aden asm --from <anchor> --budget 4096", "Assemble context within token budget"),
        (vec!["locate", "symbol", "function", "where"], "locate", "aden locate --symbol <name> .", "Find symbol definition"),
        (vec!["init", "scaffold", "setup"], "init", "aden init", "Scaffold .agent/ templates"),
        (vec!["watch", "auto", "regenerate"], "watch", "aden watch .", "Watch for changes and auto-regenerate"),
        (vec!["clean", "gc", "garbage", "orphan"], "gc", "aden heal . --gc", "Garbage collect orphaned contracts"),
        (vec!["doctor", "diagnose", "health", "check environment"], "doctor", "aden doctor .", "Check environment health"),
    ];

    let mut matches: Vec<_> = suggestions.iter()
        .filter(|(keywords, _, _, _)| keywords.iter().any(|k| intent_lower.contains(k)))
        .collect();

    matches.sort_by_key(|a| std::cmp::Reverse(a.0.len()));

    println!("Aden Suggestion for: \"{}\"", intent);
    println!("====================");
    println!();

    if matches.is_empty() {
        println!("No exact match found. Try:");
        println!("  aden gen . --auto         # Generate contracts");
        println!("  aden search '<query>'    # Search contracts");
        println!("  aden ask '<question>'    # Ask a question");
        println!("  aden list .              # List all anchors");
        println!("  aden heal . --fix         # Fix drift");
    } else {
        println!("Try one of these commands:\n");
        for (_, cmd, example, desc) in &matches {
            println!("  {}: {}", cmd, desc);
            println!("    Example: {}\n", example);
        }
    }

    Ok(())
}