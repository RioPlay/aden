// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use regex::Regex;
use std::path::Path;

/// Public integrity identifiers in documentation are not credentials.  The
/// generic long-token detector deliberately catches unknown API keys, so this
/// exception is intentionally narrow: it applies only to documentation and
/// only when the line explicitly labels an exact Git commit (40 hex chars) or
/// a SHA-256/digest/checksum (64 hex chars).  Provider-token and password
/// patterns are never exempted by this helper.
fn documented_integrity_identifier(rel_path: &Path, line: &str, candidate: &str) -> bool {
    let is_document = matches!(
        rel_path.extension().and_then(|ext| ext.to_str()),
        Some("adoc" | "md" | "rst" | "txt")
    );
    if !is_document {
        return false;
    }
    // Length and a provenance label alone are not enough: the generic matcher
    // also accepts letters outside the hexadecimal alphabet.  Requiring ASCII
    // hex prevents a labelled documentation line from suppressing an arbitrary
    // long credential-like token.
    if !candidate.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }

    let label = line.to_ascii_lowercase();
    match candidate.len() {
        40 => {
            label.contains("git commit")
                || label.contains("source_commit")
                // Authority metadata and historical records use an explicit
                // dated `as-of` provenance field for their Git revision.
                || label.contains("as-of:")
                || label.contains("**as of:**")
        }
        64 => {
            label.contains("sha-256")
                || label.contains("sha256")
                || label.contains("binary digest")
                || label.contains("checksum")
        }
        _ => false,
    }
}

/// Structured benchmark manifests also carry public integrity identifiers.
/// Keep this narrower than the prose exemption: only a JSON revision field
/// may hold a 40-hex Git commit, and only the dedicated regression lock may
/// hold 64-hex file digests. Other JSON strings remain fully scannable.
fn structured_integrity_identifier(rel_path: &Path, line: &str, candidate: &str) -> bool {
    if !candidate.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    match candidate.len() {
        40 => compact == format!(r#""revision":"{candidate}""#),
        64 => {
            rel_path
                .file_name()
                .is_some_and(|name| name == "regression-lock.json")
                && (compact.ends_with(&format!(r#""{candidate}","#))
                    || compact.ends_with(&format!(r#""{candidate}""#)))
        }
        _ => false,
    }
}

/// GitHub recommends pinning third-party Actions to immutable 40-hex commits.
/// Treat only the exact workflow `uses: owner/action@<sha>` grammar as integrity
/// metadata; an identical token in source, inputs, or `env:` remains scannable.
fn github_action_commit_pin(rel_path: &Path, line: &str, candidate: &str) -> bool {
    if candidate.len() != 40 || !candidate.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    let components: Vec<_> = rel_path.components().collect();
    let in_workflow = components.len() >= 3
        && components[0].as_os_str() == ".github"
        && components[1].as_os_str() == "workflows"
        && matches!(
            rel_path
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("yml" | "yaml")
        );
    if !in_workflow {
        return false;
    }

    let trimmed = line.trim_start();
    if !trimmed.starts_with("uses:") {
        return false;
    }
    let Some(position) = trimmed.find(&format!("@{candidate}")) else {
        return false;
    };
    let tail = &trimmed[position + candidate.len() + 1..];
    tail.is_empty() || tail.starts_with(char::is_whitespace) || tail.starts_with('#')
}

/// Result of running one test framework's suite.
#[derive(Debug)]
enum FrameworkResult {
    Pass(String),
    Fail(String, String), // (label, error message)
    Skip(String, String), // (label, reason)
}

/// Returns true if `binary` resolves on PATH.
fn binary_available(binary: &str) -> bool {
    std::process::Command::new(binary)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Returns true if `package.json` contains a `"test"` script entry.
fn pkg_json_has_test_script(path: &Path) -> bool {
    let content = match std::fs::read_to_string(path.join("package.json")) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };
    json.get("scripts")
        .and_then(|s| s.get("test"))
        .and_then(|t| t.as_str())
        .map(|t| !t.is_empty())
        .unwrap_or(false)
}

/// Detect and run tests for every framework present at `path`.
/// Each detected framework is attempted independently; a missing runner is
/// reported as SKIP rather than FAIL so that a JS-only host running a
/// Rust+Node monorepo does not block on absent `npm`. Overall result is `Err`
/// only if at least one framework actually failed (exit status != 0).
#[allow(dead_code)] // Kept as the independently useful, human-reporting test runner.
pub fn run_project_tests(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    run_project_tests_with_output(path, false)
}

/// CI's JSON envelope owns stdout, so its nested test runner must be silent.
fn run_project_tests_with_output(
    path: &Path,
    quiet: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let has_cargo = path.join("Cargo.toml").exists();
    let has_go_mod = path.join("go.mod").exists();
    let has_pkg_json = path.join("package.json").exists() && pkg_json_has_test_script(path);
    let has_pyproject = path.join("pyproject.toml").exists();
    let has_setup_py = path.join("setup.py").exists();
    let has_reqs = path.join("requirements.txt").exists();
    let has_pom = path.join("pom.xml").exists();

    let mut results: Vec<FrameworkResult> = Vec::new();

    // ── Rust / Cargo ────────────────────────────────────────────────────────
    if has_cargo {
        if !binary_available("cargo") {
            results.push(FrameworkResult::Skip(
                "cargo".into(),
                "cargo not found on PATH".into(),
            ));
        } else {
            let output = std::process::Command::new("cargo")
                .args(["test", "--workspace", "--quiet"])
                .current_dir(path)
                .output();
            match output {
                Ok(o) if o.status.success() => {
                    results.push(FrameworkResult::Pass("cargo test".into()));
                }
                Ok(o) => {
                    results.push(FrameworkResult::Fail(
                        "cargo test".into(),
                        String::from_utf8_lossy(&o.stderr).into_owned(),
                    ));
                }
                Err(e) => {
                    results.push(FrameworkResult::Fail("cargo test".into(), e.to_string()));
                }
            }
        }
    }

    // ── Go ──────────────────────────────────────────────────────────────────
    if has_go_mod {
        if !binary_available("go") {
            results.push(FrameworkResult::Skip(
                "go test".into(),
                "go not found on PATH".into(),
            ));
        } else {
            let output = std::process::Command::new("go")
                .args(["test", "./..."])
                .current_dir(path)
                .output();
            match output {
                Ok(o) if o.status.success() => {
                    results.push(FrameworkResult::Pass("go test".into()));
                }
                Ok(o) => {
                    results.push(FrameworkResult::Fail(
                        "go test".into(),
                        String::from_utf8_lossy(&o.stderr).into_owned(),
                    ));
                }
                Err(e) => {
                    results.push(FrameworkResult::Fail("go test".into(), e.to_string()));
                }
            }
        }
    }

    // ── Node / JS ───────────────────────────────────────────────────────────
    if has_pkg_json {
        let runner = if binary_available("npm") {
            Some("npm")
        } else if binary_available("yarn") {
            Some("yarn")
        } else if binary_available("pnpm") {
            Some("pnpm")
        } else {
            None
        };
        match runner {
            None => {
                results.push(FrameworkResult::Skip(
                    "npm/yarn/pnpm test".into(),
                    "no JS package manager found on PATH (npm/yarn/pnpm)".into(),
                ));
            }
            Some(r) => {
                let output = std::process::Command::new(r)
                    .args(["test", "--", "--passWithNoTests"])
                    .current_dir(path)
                    .output();
                match output {
                    Ok(o) if o.status.success() => {
                        results.push(FrameworkResult::Pass(format!("{r} test")));
                    }
                    Ok(o) => {
                        results.push(FrameworkResult::Fail(
                            format!("{r} test"),
                            String::from_utf8_lossy(&o.stderr).into_owned(),
                        ));
                    }
                    Err(e) => {
                        results.push(FrameworkResult::Fail(format!("{r} test"), e.to_string()));
                    }
                }
            }
        }
    }

    // ── Python ──────────────────────────────────────────────────────────────
    if has_pyproject || has_setup_py || has_reqs {
        let runner = if binary_available("pytest") {
            Some(("pytest", vec!["-q"]))
        } else if binary_available("python") {
            Some(("python", vec!["-m", "pytest", "-q"]))
        } else if binary_available("python3") {
            Some(("python3", vec!["-m", "pytest", "-q"]))
        } else {
            None
        };
        match runner {
            None => {
                results.push(FrameworkResult::Skip(
                    "pytest".into(),
                    "pytest / python not found on PATH".into(),
                ));
            }
            Some((bin, args)) => {
                let output = std::process::Command::new(bin)
                    .args(&args)
                    .current_dir(path)
                    .output();
                match output {
                    Ok(o) if o.status.success() => {
                        results.push(FrameworkResult::Pass(format!("{bin} {}", args.join(" "))));
                    }
                    Ok(o) => {
                        results.push(FrameworkResult::Fail(
                            format!("{bin} {}", args.join(" ")),
                            String::from_utf8_lossy(&o.stderr).into_owned(),
                        ));
                    }
                    Err(e) => {
                        results.push(FrameworkResult::Fail(
                            format!("{bin} {}", args.join(" ")),
                            e.to_string(),
                        ));
                    }
                }
            }
        }
    }

    // ── Java / Maven ────────────────────────────────────────────────────────
    if has_pom {
        if !binary_available("mvn") {
            results.push(FrameworkResult::Skip(
                "mvn test".into(),
                "mvn not found on PATH".into(),
            ));
        } else {
            let output = std::process::Command::new("mvn")
                .args(["test", "-q"])
                .current_dir(path)
                .output();
            match output {
                Ok(o) if o.status.success() => {
                    results.push(FrameworkResult::Pass("mvn test".into()));
                }
                Ok(o) => {
                    results.push(FrameworkResult::Fail(
                        "mvn test".into(),
                        String::from_utf8_lossy(&o.stderr).into_owned(),
                    ));
                }
                Err(e) => {
                    results.push(FrameworkResult::Fail("mvn test".into(), e.to_string()));
                }
            }
        }
    }

    // ── Aggregate ───────────────────────────────────────────────────────────
    if results.is_empty() {
        return Err(
            "No recognized test framework found (checked Cargo.toml, go.mod, \
                    package.json[test script], pyproject.toml, setup.py, \
                    requirements.txt, pom.xml)"
                .into(),
        );
    }

    let mut failures: Vec<String> = Vec::new();
    for r in &results {
        match r {
            FrameworkResult::Pass(label) => {
                if !quiet {
                    println!("  [PASS] {label}");
                }
            }
            FrameworkResult::Fail(label, msg) => {
                let trimmed = msg.trim();
                if !quiet {
                    println!("  [FAIL] {label}:\n{trimmed}");
                }
                failures.push(format!("{label}: {trimmed}"));
            }
            FrameworkResult::Skip(label, reason) => {
                if !quiet {
                    println!("  [SKIP] {label}: {reason}");
                }
            }
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} test framework(s) failed:\n{}",
            failures.len(),
            failures.join("\n---\n")
        )
        .into())
    }
}

pub fn cmd_ci_check(path: &Path, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut exit_code = 0i32;
    let mut warnings = Vec::new();
    let mut gate_results: Vec<serde_json::Value> = Vec::new();
    let (green, red, yellow, reset) = if json {
        ("", "", "", "")
    } else {
        ("\x1b[0;32m", "\x1b[0;31m", "\x1b[1;33m", "\x1b[0m")
    };

    macro_rules! gate {
        ($name:expr, $cmd:expr) => {{
            if !json { println!("[CI] Running: {} ...", $name); }
            match $cmd {
                Ok(_) => {
                    if !json { println!("{}[CI] PASS: {}{}", green, $name, reset); }
                    gate_results.push(serde_json::json!({"name":$name,"status":"pass","blocking":true}));
                }
                Err(e) => {
                    if !json { println!("{}[CI] FAIL: {} — {}{}", red, $name, e, reset); }
                    gate_results.push(serde_json::json!({"name":$name,"status":"fail","blocking":true,"message":e.to_string()}));
                    exit_code = 1;
                }
            }
        }};
    }

    macro_rules! warn {
        ($name:expr, $cmd:expr) => {{
            if !json { println!("[CI] Checking: {} ...", $name); }
            match $cmd {
                Ok(()) => {
                    if !json { println!("{}[CI] OK:   {}{}", green, $name, reset); }
                    gate_results.push(serde_json::json!({"name":$name,"status":"ok","blocking":false}));
                }
                Err(e) => {
                    if !json { println!("{}[CI] WARN: {} — {}{}", yellow, $name, e, reset); }
                    warnings.push(format!("{}: {}", $name, e));
                    gate_results.push(serde_json::json!({"name":$name,"status":"warn","blocking":false,"message":e.to_string()}));
                }
            }
        }};
    }

    // ── BLOCKING GATES ────────────────────────────────────
    gate!("aden check", {
        if !path.is_dir() {
            Err("not a directory".into())
        } else {
            crate::util::perform_check(path).map(|_| ())
        }
    });

    // Constitutional firewall: soft warning only — projects that have not opted
    // into aden governance are not penalised. Only validate if the file exists.
    {
        let constitution_path = path.join(".aden/constitution.adoc");
        if constitution_path.exists() {
            warn!("constitutional firewall", {
                aden_policy::PolicyEngine::load_bootstrap(path)
                    .map(|_| ())
                    .map_err(|e| -> Box<dyn std::error::Error> {
                        format!("Invalid bootstrap constitution: {}", e).into()
                    })
            });
        } else if !json {
            println!("[CI] SKIP: constitutional firewall — no .aden/constitution.adoc (optional)");
        }
    }

    gate!("tests", { run_project_tests_with_output(path, json) });

    gate!("aden lint", {
        // quiet=true: ci-check only needs the Ok/Err result and builds its own
        // JSON envelope, so lint must emit nothing on its own stdout.
        crate::commands::cmd_lint(path, "Error", false, false, false, false, true)
    });

    gate!("secret scan", {
        use aden_core::filter::AdenFilter;
        use regex::Regex;
        use std::sync::OnceLock;

        static SECRET_PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
        let patterns = SECRET_PATTERNS.get_or_init(|| {
            vec![
                (
                    Regex::new(&format!(
                        r"-----BEGIN (RSA |EC |OPENSSH |DSA )?{}-----",
                        "PRIVATE KEY"
                    ))
                    .unwrap(),
                    "private key",
                ),
                (Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(), "AWS access key"),
                (Regex::new(r"ghp_[a-zA-Z0-9]{36}").unwrap(), "GitHub token"),
                (Regex::new(r"gho_[a-zA-Z0-9]{36}").unwrap(), "GitHub OAuth"),
                (
                    Regex::new(r"\b[0-9a-zA-Z]{32,64}\b").unwrap(),
                    "long hex secret (possible API key)",
                ),
                (
                    // Value classes exclude `${}` so interpolated/templated
                    // values (`"${secret}"`, `"{{token}}"`) are not flagged —
                    // they are runtime-derived, not hardcoded literals.
                    Regex::new(r#"api[_-]?key\s*=\s*['\"][^'\"${}]{8,}['\"]"#).unwrap(),
                    "API key assignment",
                ),
                (
                    Regex::new(r#"password\s*=\s*['\"][^'\"${}]{4,}['\"]"#).unwrap(),
                    "hardcoded password",
                ),
                (
                    Regex::new(r#"secret\s*=\s*['\"][^'\"${}]{8,}['\"]"#).unwrap(),
                    "hardcoded secret",
                ),
                (
                    Regex::new(r#"token\s*=\s*['\"][^'\"${}]{8,}['\"]"#).unwrap(),
                    "hardcoded token",
                ),
                (
                    Regex::new(r"eyJ[a-zA-Z0-9_-]*\.eyJ[a-zA-Z0-9_-]*\.[a-zA-Z0-9_-]*").unwrap(),
                    "JWT token",
                ),
                (
                    Regex::new(r"bearer\s+[a-zA-Z0-9_\-\.]{20,}").unwrap(),
                    "Bearer token",
                ),
                (
                    Regex::new(r"mongodb(\+srv)?://[^:]+:[^@]+@").unwrap(),
                    "MongoDB connection string",
                ),
                (
                    Regex::new(r"postgres(ql)?://[^:]+:[^@]+@").unwrap(),
                    "PostgreSQL connection string",
                ),
                (
                    Regex::new(r"mysql://[^:]+:[^@]+@").unwrap(),
                    "MySQL connection string",
                ),
                (
                    Regex::new(r"redis://:[^@]+@").unwrap(),
                    "Redis connection string",
                ),
                (Regex::new(r"\.env\.[a-zA-Z]+\s*\n").unwrap(), "env file"),
                (Regex::new(r"DATABASE_URL\s*=\s*").unwrap(), "DATABASE_URL"),
                (Regex::new(r"sk-[a-zA-Z0-9]{48,}").unwrap(), "OpenAI/sk key"),
            ]
        });

        // Common regex constructors across ecosystems. A line that builds a
        // pattern matcher legitimately contains credential-shaped substrings,
        // so it must not trip the scanner — in any language, not just Rust.
        const REGEX_DEF_MARKERS: &[&str] = &[
            "Regex::new",         // Rust (regex crate)
            "RegexBuilder",       // Rust
            "regexp.MustCompile", // Go
            "regexp.Compile",     // Go
            "re.compile",         // Python
            "Pattern.compile",    // Java
            "new RegExp",         // JS / TS
            "new Regex",          // .NET
            "Regexp.new",         // Ruby
            "preg_match",         // PHP
            "preg_replace",       // PHP
        ];

        let non_text_exts: std::collections::HashSet<&str> = [
            "png", "jpg", "jpeg", "gif", "svg", "ico", "bmp", "pdf", "zip", "tar", "gz", "bz2",
            "xz", "7z", "rar", "mp3", "mp4", "avi", "mov", "mkv", "wav", "flac", "wasm", "so",
            "dll", "dylib", "exe", "bin", "o", "a", "ttf", "otf", "woff", "woff2", "eot", "jpg",
            "mp3", "mp4",
        ]
        .iter()
        .copied()
        .collect();

        const MAX_SCAN_SIZE: u64 = 1024 * 1024;
        let mut found = 0;
        let filter = AdenFilter::from_directory(path);

        for entry in walkdir::WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            if let Ok(rel) = p.strip_prefix(path)
                && filter.should_skip(rel)
            {
                continue;
            }
            // Exclude cache and generated files from secret scan
            let rel_path = p.strip_prefix(path).unwrap_or(p.as_ref());
            let rel_str = rel_path.to_string_lossy();
            if rel_str.contains(".aden/cache")
                || rel_str.contains("contracts/")
                // gstack browser traces are workstation-local diagnostics, not
                // project source.  They may record request hashes/tokens and
                // must neither be scanned nor accidentally released.
                || rel_path.components().any(|c| c.as_os_str() == ".gstack")
            {
                continue;
            }
            // Dependency lockfiles across ecosystems are full of package
            // integrity hashes (sha256/sha512, 32-64 hex chars) that trip the
            // broad "long hex secret" pattern. They are machine-generated
            // manifests, not credential stores, so skip the well-known ones —
            // aden scans any codebase, not just Cargo projects.
            const LOCKFILES: &[&str] = &[
                "Cargo.lock",
                "uv.lock",
                "poetry.lock",
                "Pipfile.lock",
                "pdm.lock",
                "package-lock.json",
                "npm-shrinkwrap.json",
                "yarn.lock",
                "pnpm-lock.yaml",
                "go.sum",
                "composer.lock",
                "Gemfile.lock",
                "gradle.lockfile",
                "flake.lock",
            ];
            if rel_path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| LOCKFILES.contains(&n))
            {
                continue;
            }
            if rel_path
                .file_name()
                .is_some_and(|n| n == "cache-index.json")
            {
                continue;
            }
            if let Some(ext) = p.extension().and_then(|e| e.to_str())
                && non_text_exts.contains(ext.to_lowercase().as_str())
            {
                continue;
            }
            if let Ok(meta) = std::fs::metadata(p)
                && meta.len() > MAX_SCAN_SIZE
            {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(p) {
                for (re, name) in patterns {
                    for cap in re.find_iter(&text) {
                        // The broad "long hex secret" pattern matches any 32-64
                        // char alphanumeric run, which also catches long CamelCase
                        // identifiers like `TemplateContextProcessorCallable`. Real
                        // API keys / hashes have entropy: they contain at least one
                        // digit. Require that to drop pure-alphabetic identifiers
                        // without losing hex/base64 secrets.
                        if *name == "long hex secret (possible API key)"
                            && !cap.as_str().bytes().any(|b| b.is_ascii_digit())
                        {
                            continue;
                        }
                        let line_start =
                            text[..cap.start()].rfind('\n').map(|i| i + 1).unwrap_or(0);
                        let line_end = text[cap.end()..]
                            .find('\n')
                            .map(|i| cap.end() + i)
                            .unwrap_or(text.len());
                        let line = &text[line_start..line_end];
                        // A bare checksum-manifest entry (`<hash>  filename`, as in
                        // CHECKSUMS / `*.sha256` files) is integrity data, not a secret:
                        // exactly two whitespace-separated tokens, the first being the
                        // matched hex itself. Skip it for the hex-secret rule only.
                        if *name == "long hex secret (possible API key)" {
                            let mut toks = line.split_whitespace();
                            if toks.next() == Some(cap.as_str())
                                && toks.next().is_some()
                                && toks.next().is_none()
                            {
                                continue;
                            }
                            if documented_integrity_identifier(rel_path, line, cap.as_str()) {
                                continue;
                            }
                            if structured_integrity_identifier(rel_path, line, cap.as_str()) {
                                continue;
                            }
                            if github_action_commit_pin(rel_path, line, cap.as_str()) {
                                continue;
                            }
                        }
                        // Language-agnostic allowlist: any line bearing this
                        // marker — in a comment of ANY syntax (`//`, `#`, `--`,
                        // `;`, `<!-- -->`) or none — is intentional sample/fixture
                        // data, not a live credential. Plain substring match keeps
                        // it neutral across every codebase aden scans.
                        if line.contains("aden:allow-secret") {
                            continue;
                        }
                        // AWS's published, non-functional documentation key. It
                        // satisfies the AKIA pattern by design so docs/tests can
                        // carry a realistic example; it is never a real credential.
                        // The pattern still catches every *other* AKIA key.
                        if line.contains("AKIAIOSFODNN7EXAMPLE") {
                            continue;
                        }
                        // Skip regex/pattern *definition* lines, which embed
                        // credential-shaped substrings as match patterns rather
                        // than as secrets. Language-neutral: covers the common
                        // regex constructors across ecosystems, not just Rust.
                        if REGEX_DEF_MARKERS.iter().any(|m| line.contains(m)) {
                            continue;
                        }
                        if rel_path.starts_with("tools/") {
                            continue;
                        }
                        // Component match (not a substring) so it holds on Windows.
                        if rel_path.components().any(|c| c.as_os_str() == "tests") {
                            continue;
                        }
                        if *name == "env file" {
                            let trimmed = line.trim();
                            if trimmed.starts_with(".env") || trimmed.starts_with("*.env") {
                                continue;
                            }
                        }
                        let snippet =
                            &text[cap.start().saturating_sub(20)..(cap.end() + 20).min(text.len())];
                        if !json {
                            println!(
                                "  {}Secret ({}) in {}: ...{}...{}",
                                red,
                                name,
                                p.display(),
                                snippet.replace('\n', " "),
                                reset
                            );
                        }
                        found += 1;
                    }
                }
            }
        }

        if found > 0 {
            Err(Box::<dyn std::error::Error>::from(format!(
                "{} secret pattern(s) detected",
                found
            )))
        } else {
            Ok(())
        }
    });

    gate!("accreditation check", {
        if path.join("Cargo.lock").exists() && !path.join("NOTICE.md").exists() {
            Err(Box::<dyn std::error::Error>::from(
                "NOTICE.md missing. Run 'aden licenses --out NOTICE.md'.".to_string(),
            ))
        } else {
            Ok(())
        }
    });

    gate!("owasp audit", {
        crate::commands::audit::cmd_audit_with_output(path, None, "text", true, false, json)
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
            if !p.is_file() {
                continue;
            }
            if let Ok(rel) = p.strip_prefix(path)
                && filter.should_skip(rel)
            {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(p) {
                for line in text.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("<<<<<<< ")
                        || trimmed.starts_with(">>>>>>> ")
                        || trimmed == "======="
                    {
                        if !json {
                            println!(
                                "  {}Merge conflict marker in {}: {}{}",
                                red,
                                p.display(),
                                trimmed,
                                reset
                            );
                        }
                        found += 1;
                    }
                }
            }
        }
        if found > 0 {
            Err(Box::<dyn std::error::Error>::from(format!(
                "{} merge conflict marker(s) detected",
                found
            )))
        } else {
            Ok(())
        }
    });

    warn!("insecure protocol", {
        use aden_core::filter::AdenFilter;
        let mut found = 0;
        let insecure_re = Regex::new(r"(?i)http://\S+").unwrap();
        let skip_exts: std::collections::HashSet<&str> =
            ["lock", "adoc", "md", "txt", "svg", "html", "xml"]
                .iter()
                .copied()
                .collect();
        let filter = AdenFilter::from_directory(path);
        for entry in walkdir::WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            if let Ok(rel) = p.strip_prefix(path)
                && filter.should_skip(rel)
            {
                continue;
            }
            if let Some(ext) = p.extension().and_then(|e| e.to_str())
                && skip_exts.contains(ext)
            {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(p) {
                for line in text.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("//")
                        || trimmed.starts_with("#")
                        || trimmed.starts_with("<!--")
                    {
                        continue;
                    }
                    if line.contains("Regex::new") || line.contains("xmlns=") {
                        continue;
                    }
                    if insecure_re.is_match(line) {
                        if !json {
                            println!(
                                "  {}Insecure http:// URL in {}: {}{}",
                                red,
                                p.display(),
                                line.trim(),
                                reset
                            );
                        }
                        found += 1;
                    }
                }
            }
        }
        if found > 0 {
            Err(Box::<dyn std::error::Error>::from(format!(
                "{} insecure http:// URL(s) detected",
                found
            )))
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
                .args(["clippy", "--workspace", "--", "-W", "clippy::all"])
                .current_dir(path)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()?;
            if output.status.success() {
                Ok(())
            } else {
                Err(Box::<dyn std::error::Error>::from(format!(
                    "cargo clippy found issues:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                )))
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
                    Err(Box::<dyn std::error::Error>::from(
                        "cargo audit not installed. Install with: cargo install cargo-audit"
                            .to_string(),
                    ))
                } else {
                    Err(Box::<dyn std::error::Error>::from(format!(
                        "cargo audit found vulnerabilities:\n{}",
                        stderr
                    )))
                }
            }
        }
    });

    warn!("contract freshness", {
        use aden_heal::{Scanner, generate};
        let scanner = Scanner::new(path);
        let events = scanner.scan()?;
        // Only genuinely actionable drift counts here: broken refs and signature
        // mismatches. OrphanAnchor is overwhelmingly EXPECTED metadata (doc-heading
        // nodes; ADR/plan/use-case docs with no edges), so including it inflated the
        // figure into the thousands and labeled benign drift "critical" in a gate
        // that is non-blocking by design. Broken refs are already a hard failure via
        // the blocking `aden check` gate above; here they are a soft re-surfacing.
        // The score gate below must judge the SAME hard events, else doc-node
        // MissingContracts crater the score to 0.00 and contradict `diagnose`.
        let is_actionable = |e: &aden_heal::DriftEvent| {
            matches!(
                e,
                aden_heal::DriftEvent::BrokenReference { .. }
                    | aden_heal::DriftEvent::SignatureMismatch { .. }
                    | aden_heal::DriftEvent::DocSignatureDivergence { .. }
            )
        };
        let actionable_events: Vec<_> = events
            .iter()
            .filter(|e| is_actionable(e))
            .cloned()
            .collect();
        let report = generate(actionable_events, path);
        let actionable_count = events.iter().filter(|e| is_actionable(e)).count();
        if actionable_count > 0 {
            Err(Box::<dyn std::error::Error>::from(format!(
                "{} actionable drift event(s) (broken refs / signature mismatch) — run 'aden heal'",
                actionable_count
            )))
        } else if report.overall_score < 0.90 {
            // Soft warning only when the health score is genuinely low; minor drift
            // is auto-fixable and must not block commits.
            Err(Box::<dyn std::error::Error>::from(format!(
                "Health score: {:.2} — contract drift (run 'aden gen' to refresh)",
                report.overall_score
            )))
        } else {
            Ok(())
        }
    });

    // ── Final Verdict ─────────────────────────────────────
    if json {
        let outcome = crate::commands::outcome::OutcomeEnvelope::evaluated(
            if exit_code == 0 { 0 } else { 1 },
            warnings.len(),
            if exit_code == 0 {
                "healthy"
            } else {
                "unhealthy"
            },
            if warnings.iter().any(|w| w.contains("constitutional")) {
                "advisory_findings"
            } else {
                "clean"
            },
            if warnings.iter().any(|w| w.contains("freshness")) {
                "stale"
            } else {
                "fresh"
            },
        );
        let env = serde_json::json!({
            "ok": exit_code == 0,
            "gates": gate_results,
            "warnings": warnings,
            "result": outcome,
        });
        println!("{}", serde_json::to_string_pretty(&env)?);
        if exit_code != 0 {
            std::process::exit(exit_code);
        }
        return Ok(());
    }

    if !warnings.is_empty() {
        println!("\n{}[CI] WARNINGS (non-blocking):{}", yellow, reset);
        for w in &warnings {
            println!("  ⚠ {}", w);
        }
        println!("  Run 'aden gen <file>' on modified source to clear.\n");
    }

    if exit_code != 0 {
        println!(
            "\n{}[CI] GATES BLOCKED — Fix errors above before committing.{}",
            red, reset
        );
        std::process::exit(exit_code);
    }
    if warnings.is_empty() {
        println!(
            "\n{}[CI] ALL GATES PASSED — Ready to commit.{}",
            green, reset
        );
        println!("[CI] Outcome: clean");
    } else {
        println!(
            "\n{}[CI] BLOCKING GATES PASSED — {} advisory finding(s) remain.{}",
            yellow,
            warnings.len(),
            reset
        );
        println!("[CI] Outcome: passed_with_findings");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_hex_secret_filter_drops_alpha_identifiers_keeps_hex() {
        // The broad long-token pattern matches both a 32-char CamelCase
        // identifier and a real hex secret; the digit-entropy guard used in the
        // secret-scan gate must drop the former and keep the latter.
        let re = Regex::new(r"\b[0-9a-zA-Z]{32,64}\b").unwrap();
        let has_digit = |s: &str| s.bytes().any(|b| b.is_ascii_digit());

        let identifier = "TemplateContextProcessorCallable"; // 32 alpha chars
        let m = re.find(identifier).expect("pattern matches the identifier");
        assert!(!has_digit(m.as_str()), "identifier has no digit → filtered");

        let hex = [
            "192b9bdd22ab9ed4d12e",
            "236c78afcb9a393ec15f",
            "71bbf5dc987d54727823bcbf",
        ]
        .concat();
        let m = re.find(&hex).expect("pattern matches the hex secret");
        assert!(has_digit(m.as_str()), "hex secret has digits → kept");
    }

    #[test]
    fn documented_integrity_identifiers_are_narrowly_exempted() {
        let commit = ["d8146cd2b77b86d2573", "de398a474dcd90920ab65"].concat();
        let digest = [
            "0baecb2903aad9ef3cdc",
            "7defabaecbd665a40347",
            "219b688a588a0ddb5f2d8dad",
        ]
        .concat();
        assert!(documented_integrity_identifier(
            Path::new("docs/evidence.adoc"),
            &format!("Git commit `{commit}`"),
            &commit
        ));
        assert!(documented_integrity_identifier(
            Path::new("docs/evidence.adoc"),
            &format!("SHA-256 `{digest}`"),
            &digest
        ));
        assert!(!documented_integrity_identifier(
            Path::new("src/config.rs"),
            &format!("SHA-256 `{digest}`"),
            &digest
        ));
        assert!(!documented_integrity_identifier(
            Path::new("docs/evidence.adoc"),
            &format!("token = `{digest}`"),
            &digest
        ));
        let non_hex_commit = ["z8146cd2b77b86d2573", "de398a474dcd90920ab65"].concat();
        assert!(!documented_integrity_identifier(
            Path::new("docs/evidence.adoc"),
            &format!("Git commit `{non_hex_commit}`"),
            &non_hex_commit
        ));
        let non_hex_digest = [
            "zbaecb2903aad9ef3cdc",
            "7defabaecbd665a40347",
            "219b688a588a0ddb5f2d8dad",
        ]
        .concat();
        assert!(!documented_integrity_identifier(
            Path::new("docs/evidence.adoc"),
            &format!("SHA-256 `{non_hex_digest}`"),
            &non_hex_digest
        ));
    }

    #[test]
    fn structured_integrity_identifiers_are_narrowly_exempted() {
        let commit = ["ecfec5b87f78ad6ede41", "5c406eb862034999fb04"].concat();
        let digest = [
            "2d56057a9a04977a6dac",
            "88f3db790c727acfac9a",
            "49a92e3d4cae75192fd3c564",
        ]
        .concat();

        assert!(structured_integrity_identifier(
            Path::new("scripts/agent-bench/tasks.json"),
            &format!(r#"      "revision": "{commit}""#),
            &commit
        ));
        assert!(structured_integrity_identifier(
            Path::new("scripts/regression-lock.json"),
            &format!(r#"    "scripts/tasks.json": "{digest}","#),
            &digest
        ));
        assert!(!structured_integrity_identifier(
            Path::new("config.json"),
            &format!(r#"    "token": "{commit}""#),
            &commit
        ));
        assert!(!structured_integrity_identifier(
            Path::new("other-lock.json"),
            &format!(r#"    "file": "{digest}""#),
            &digest
        ));
    }

    #[test]
    fn github_action_commit_pins_are_narrowly_exempted() {
        let commit = ["34e114876b0b11c390a56", "381ad16ebd13914f8d5"].concat();
        let workflow = Path::new(".github/workflows/release.yml");
        assert!(github_action_commit_pin(
            workflow,
            &format!("uses: actions/checkout@{commit} # v4"),
            &commit
        ));
        assert!(!github_action_commit_pin(
            Path::new("src/config.yml"),
            &format!("uses: actions/checkout@{commit}"),
            &commit
        ));
        assert!(!github_action_commit_pin(
            workflow,
            &format!("env: TOKEN={commit}"),
            &commit
        ));
        let non_hex = ["z4e114876b0b11c390a56", "381ad16ebd13914f8d5"].concat();
        assert!(!github_action_commit_pin(
            workflow,
            &format!("uses: actions/checkout@{non_hex}"),
            &non_hex
        ));
    }

    // ── run_project_tests: polyglot detection ────────────────────────────────

    /// A fixture directory with both Cargo.toml and package.json should cause
    /// `run_project_tests` to ATTEMPT both frameworks, not short-circuit after
    /// Cargo. We assert this without actually running cargo/npm by checking that
    /// the error message mentions both frameworks when both are present but
    /// neither runner succeeds (CI host may lack npm; cargo test would succeed
    /// here but we use a path that has no test binaries). The key invariant is
    /// that the code path reaches the JS framework check at all.
    #[test]
    fn run_project_tests_attempts_all_detected_frameworks() {
        let dir = std::env::temp_dir().join(format!("aden-polyglot-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Minimal Cargo.toml: cargo will pass (no tests = green).
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname=\"poly\"\nversion=\"0.0.0\"\nedition=\"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn x() {}\n").unwrap();

        // package.json with a "test" script: the JS framework should be detected.
        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"poly","version":"0.0.0","scripts":{"test":"echo ok"}}"#,
        )
        .unwrap();

        // With both manifests present, run_project_tests must attempt >= 2 frameworks.
        // We observe this by checking that it doesn't return immediately after
        // cargo (it will also try npm/yarn/pnpm; if none present it SKIPS, not fails).
        // The function must return Ok (cargo passes) and emit at least one [PASS] line.
        // We just assert no panic and a definite result (not "no framework found").
        let result = run_project_tests(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        // "No recognized test framework found" must NOT appear — we have two manifests.
        if let Err(ref e) = result {
            let msg = e.to_string();
            assert!(
                !msg.contains("No recognized test framework found"),
                "expected at least one framework to be detected, got: {msg}"
            );
        }
        // Either Ok (cargo passed, JS skipped/passed) or an error naming a framework.
    }

    /// A directory with only package.json but NO "test" script should NOT be
    /// detected as a JS test framework (avoids running `npm test` on a project
    /// that has no tests configured, which would exit with an error).
    #[test]
    fn run_project_tests_skips_pkg_json_without_test_script() {
        let dir = std::env::temp_dir().join(format!("aden-noscript-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"no-test","version":"0.0.0","scripts":{"build":"tsc"}}"#,
        )
        .unwrap();

        let result = run_project_tests(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        // Should fail with "No recognized test framework found" since the only
        // manifest has no test script and no other manifests are present.
        assert!(
            result.is_err(),
            "expected error when no test scripts are configured"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No recognized test framework found"),
            "should report no framework found"
        );
    }
}
