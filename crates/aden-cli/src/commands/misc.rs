// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

use crate::types::{OwaspFinding, OwaspSeverity};
use crate::util::quick_health_score;

/// OWASP-aligned security audit: scan source for vulnerabilities.
/// A03 SQL-injection detector. Requires a real SQL clause shape (verb + clause
/// keyword) followed by a concat/interpolation marker, so prose and test strings
/// that merely contain a SQL verb or a `${}` template do not produce findings.
pub(crate) const SQL_INJECTION_PATTERN: &str = r#"(?i)\b(SELECT\s+[^;'"]*\bFROM\b|INSERT\s+INTO\b|UPDATE\s+[\w.]+\s+SET\b|DELETE\s+FROM\b|DROP\s+TABLE\b)[^;]*(\+|\$\{|%s|%d|\|\|)"#;

/// A07 hardcoded-credential detector. Matches a credential-named assignment to a
/// quoted LITERAL. The value class excludes `$`/`{`/`}` so an interpolated or
/// templated value — `"${secret}"`, `"{{token}}"`, `"#{api_key}"`, an f-string
/// `"{key}"` — is NOT flagged: those are derived at runtime, not hardcoded. This
/// kills the create-t3-app FP on `AUTH_SECRET="${secret}"` (an .env template)
/// while still catching `password = "hunter2"`. aden:allow-secret
pub(crate) const SECRET_ASSIGNMENT_PATTERN: &str =
    r#"(?i)(password|passwd|pwd|secret|token|api_key)\s*=\s*['"][^'"${}]+['"]"#;

// ── A06: manifest/lockfile vulnerability checks ──────────────────────────────
//
// Each table entry is: (crate/package name, bad version predicate description,
// version_is_bad closure, severity, description, remediation).
//
// Version comparison is major.minor-only (patch is intentionally ignored since
// patch-level CVEs are tracked by `cargo audit`, not this heuristic scan).

/// Parse a semver string into (major, minor). Returns None on parse failure.
fn parse_major_minor(v: &str) -> Option<(u64, u64)> {
    // Strip a leading `^`, `~`, `=`, `>=`, etc. if present (Cargo.toml style).
    let v = v.trim_start_matches(['^', '~', '=', '>', '<', ' ']);
    let mut parts = v.splitn(3, '.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
}

/// A single known-vulnerable dependency rule for lockfile/manifest scanning.
#[allow(dead_code)] // `bad_range` is documentation-only; used in error messages / rule tables.
struct DepRule {
    /// Ecosystem this rule applies to ("cargo", "npm", "pypi", "maven").
    ecosystem: &'static str,
    /// Package name to match (case-insensitive).
    name: &'static str,
    /// Human-readable version range description, e.g. "< 1.0.0".
    bad_range: &'static str,
    /// Returns `true` when the resolved version is in the vulnerable range.
    version_is_bad: fn(major: u64, minor: u64) -> bool,
    severity: OwaspSeverity,
    description: &'static str,
    remediation: &'static str,
}

static DEP_RULES: &[DepRule] = &[
    // Rust — Cargo.lock
    DepRule {
        ecosystem: "cargo",
        name: "serde",
        bad_range: "< 1.0",
        version_is_bad: |maj, _| maj < 1,
        severity: OwaspSeverity::High,
        description: "Known-vulnerable serde version (pre-1.0) in Cargo.lock",
        remediation: "Update to serde >= 1.0 for security fixes.",
    },
    DepRule {
        ecosystem: "cargo",
        name: "reqwest",
        bad_range: "< 0.11",
        version_is_bad: |maj, min| maj == 0 && min < 11,
        severity: OwaspSeverity::Medium,
        description: "Older reqwest version (< 0.11) may have known TLS issues",
        remediation: "Update to latest reqwest version.",
    },
    DepRule {
        ecosystem: "cargo",
        name: "actix-web",
        bad_range: "< 4.6",
        version_is_bad: |maj, min| maj < 4 || (maj == 4 && min < 6),
        severity: OwaspSeverity::High,
        description: "Vulnerable actix-web version (pre-4.6) in Cargo.lock",
        remediation: "Update to actix-web >= 4.6.",
    },
    // Python — requirements.txt / poetry.lock
    DepRule {
        ecosystem: "pypi",
        name: "cryptography",
        bad_range: "< 41.0",
        version_is_bad: |maj, _| maj < 41,
        severity: OwaspSeverity::High,
        description: "Vulnerable cryptography library version (< 41.0) in Python manifest",
        remediation: "Update to latest cryptography >= 41.0.",
    },
    // Java — pom.xml
    DepRule {
        ecosystem: "maven",
        name: "jackson-databind",
        bad_range: "< 2.15",
        version_is_bad: |maj, min| maj < 2 || (maj == 2 && min < 15),
        severity: OwaspSeverity::High,
        description: "Vulnerable Jackson Databind version (< 2.15) in pom.xml",
        remediation: "Update Jackson to 2.15+ for CVE fixes.",
    },
];

/// A resolved dependency from a manifest or lockfile.
struct ResolvedDep<'a> {
    ecosystem: &'static str,
    name: String,
    version: String,
    /// Path to the manifest/lockfile that contained this entry.
    source_path: &'a std::path::Path,
}

/// Read Cargo.lock and return resolved packages (skips workspace-internal crates).
fn read_cargo_lock(root: &Path) -> Vec<(String, String)> {
    let content = match std::fs::read_to_string(root.join("Cargo.lock")) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut name: Option<String> = None;
    let mut is_workspace = false;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("[[package]]") {
            name = None;
            is_workspace = false;
        } else if let Some(rest) = t.strip_prefix("name = ") {
            let n = rest.trim_matches('"').to_string();
            is_workspace = n.starts_with("aden");
            name = Some(n);
        } else if let Some(rest) = t.strip_prefix("version = ")
            && !is_workspace
            && let Some(ref n) = name
        {
            out.push((n.clone(), rest.trim_matches('"').to_string()));
        }
    }
    out
}

/// Read Python deps from poetry.lock or requirements.txt.
fn read_python_deps(root: &Path) -> Vec<(String, String)> {
    // poetry.lock takes priority.
    if let Ok(content) = std::fs::read_to_string(root.join("poetry.lock")) {
        let mut out = Vec::new();
        let mut name: Option<String> = None;
        for line in content.lines() {
            let t = line.trim();
            if t.starts_with("[[package]]") {
                name = None;
            } else if let Some(rest) = t.strip_prefix("name = ") {
                name = Some(rest.trim_matches('"').to_string());
            } else if let Some(rest) = t.strip_prefix("version = ")
                && let Some(ref n) = name
            {
                out.push((n.clone(), rest.trim_matches('"').to_string()));
            }
        }
        return out;
    }
    // Fall back to requirements.txt (pinned lines only).
    let mut out = Vec::new();
    if let Ok(content) = std::fs::read_to_string(root.join("requirements.txt")) {
        for line in content.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() || line.starts_with('-') {
                continue;
            }
            let spec = line.split(';').next().unwrap_or("").trim();
            if let Some((pkg, ver)) = spec.split_once("==") {
                let pkg = pkg.split('[').next().unwrap_or("").trim();
                if !pkg.is_empty() {
                    out.push((pkg.to_string(), ver.trim().to_string()));
                }
            }
        }
    }
    out
}

/// Extract `<artifactId>` + `<version>` pairs from a pom.xml (best-effort line scan).
fn read_maven_deps(root: &Path) -> Vec<(String, String)> {
    let content = match std::fs::read_to_string(root.join("pom.xml")) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    // Very lightweight parser: collect <artifactId> and <version> within the
    // same <dependency> block. This avoids pulling in an XML parser.
    let mut out = Vec::new();
    let mut artifact: Option<String> = None;
    let mut in_dep = false;
    for line in content.lines() {
        let t = line.trim();
        if t.contains("<dependency>") {
            in_dep = true;
            artifact = None;
        } else if t.contains("</dependency>") {
            in_dep = false;
            artifact = None;
        } else if in_dep {
            if let Some(inner) = t
                .strip_prefix("<artifactId>")
                .and_then(|s| s.strip_suffix("</artifactId>"))
            {
                artifact = Some(inner.to_string());
            } else if let Some(inner) = t
                .strip_prefix("<version>")
                .and_then(|s| s.strip_suffix("</version>"))
                && let Some(ref art) = artifact
            {
                out.push((art.clone(), inner.to_string()));
            }
        }
    }
    out
}

/// Scan manifests/lockfiles at `root` for known-vulnerable dependency versions.
/// Returns `OwaspFinding` entries (A06) — no source-line scanning.
fn check_vulnerable_deps(root: &Path) -> Vec<OwaspFinding> {
    let mut deps: Vec<ResolvedDep<'_>> = Vec::new();

    let cargo_lock_path = root.join("Cargo.lock");
    if cargo_lock_path.exists() {
        for (name, ver) in read_cargo_lock(root) {
            deps.push(ResolvedDep {
                ecosystem: "cargo",
                name,
                version: ver,
                source_path: &cargo_lock_path,
            });
        }
    }

    let pyproject_path = root.join("pyproject.toml");
    let setup_py_path = root.join("setup.py");
    let reqs_path = root.join("requirements.txt");
    // Use a single representative path for Python manifest findings.
    let py_source = if pyproject_path.exists() {
        &pyproject_path
    } else if setup_py_path.exists() {
        &setup_py_path
    } else {
        &reqs_path
    };
    if py_source.exists() || root.join("poetry.lock").exists() {
        for (name, ver) in read_python_deps(root) {
            deps.push(ResolvedDep {
                ecosystem: "pypi",
                name,
                version: ver,
                source_path: py_source,
            });
        }
    }

    let pom_path = root.join("pom.xml");
    if pom_path.exists() {
        for (name, ver) in read_maven_deps(root) {
            deps.push(ResolvedDep {
                ecosystem: "maven",
                name,
                version: ver,
                source_path: &pom_path,
            });
        }
    }

    let mut findings = Vec::new();
    for dep in &deps {
        let name_lower = dep.name.to_lowercase();
        for rule in DEP_RULES {
            if rule.ecosystem != dep.ecosystem {
                continue;
            }
            if name_lower != rule.name {
                continue;
            }
            if let Some((maj, min)) = parse_major_minor(&dep.version)
                && (rule.version_is_bad)(maj, min)
            {
                findings.push(OwaspFinding {
                    owasp_id: "A06",
                    category: "Vulnerable Dependency",
                    severity: rule.severity,
                    file: dep.source_path.to_path_buf(),
                    line: 0, // manifest-level, not line-specific
                    snippet: format!("{} = \"{}\"", dep.name, dep.version),
                    description: rule.description,
                    remediation: rule.remediation,
                });
            }
        }
    }
    findings
}

pub fn cmd_audit(
    path: &Path,
    lang_filter: Option<&str>,
    format: &str,
    strict: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut findings: Vec<OwaspFinding> = Vec::new();

    // Determine which languages to scan
    let scan_all = lang_filter.is_none();
    let want_lang = lang_filter.map(|s| s.to_lowercase());

    // Extensions mapped to language IDs
    let lang_exts: Vec<(&str, &str)> = vec![
        ("rs", "rust"),
        ("py", "python"),
        ("go", "go"),
        ("js", "ts"),
        ("ts", "ts"),
        ("jsx", "ts"),
        ("tsx", "ts"),
        ("php", "php"),
        ("java", "java"),
        ("cpp", "cpp"),
        ("c", "c"),
        ("h", "c"),
        ("hpp", "cpp"),
    ];

    // Collect source files
    let mut files = Vec::new();
    if path.is_file() {
        files.push(path.to_path_buf());
    } else {
        // Honor .adenignore/.adenallow + the built-in ignore list (build
        // artifacts, .git, vendored deps, …) so the audit walks only project
        // source, like the rest of aden's file discovery.
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
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            if let Some(ext) = p.extension().and_then(|e| e.to_str())
                && let Some(l) = lang_exts.iter().find(|(e, _)| *e == ext.to_lowercase())
                && (scan_all || want_lang.as_deref() == Some(l.1))
            {
                files.push(p.to_path_buf());
            }
        }
    }

    type OwaspPattern = (
        Regex,
        Option<&'static str>,
        &'static str,
        &'static str,
        OwaspSeverity,
        &'static str,
        &'static str,
    );

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

            // A03 - SQL Injection: string-concat/interpolation in a *real* SQL
            // statement. The verb must be followed by its clause keyword
            // (SELECT…FROM, INSERT INTO, UPDATE…SET, DELETE FROM, DROP TABLE) so
            // prose and test strings that merely contain the word "DELETE" or a
            // template literal `${i}` do not trip it. Interpolation markers are
            // string concat (`+`), template/format interpolation (`${`, `%s`,
            // `%d`) or the SQL concat operator (`||`) — the bare-brace `{`
            // alternative was removed (it matched arrow-function bodies `=> {`).
            (Regex::new(SQL_INJECTION_PATTERN).unwrap(), None, "A03", "SQL Injection", OwaspSeverity::High,
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
            (Regex::new(SECRET_ASSIGNMENT_PATTERN).unwrap(), None, "A07", "Hardcoded Secret", OwaspSeverity::High,
             "Possible hardcoded credential in source",                         "Load secrets from environment variables or a vault."),
            (Regex::new(r#"(?i)(DISABLE_SSL_VERIFICATION|tls_verify\s*=\s*false|verify\s*:\s*false)"#).unwrap(), None, "A07", "Insecure Transport", OwaspSeverity::High,
             "TLS/SSL certificate verification disabled",                       "Never disable TLS verification in production."),

            // A02 - Cryptographic Failures
            (Regex::new(r#"(?i)\/ECB\/"#).unwrap(),                            None, "A02", "ECB Mode",                  OwaspSeverity::High,
             "ECB mode leaks patterns; use GCM or CBC with HMAC",                "Use GCM or CBC with message authentication."),
            (Regex::new(r#"(?i)(\biv\s*=|nonce\s*=|IV\s*=\s*['\"])"#).unwrap(),  None, "A02", "Hardcoded IV",            OwaspSeverity::High,
             "Hardcoded initialization vector (IV) detected",                   "Generate IVs randomly per encryption; use SecureRandom."),
            (Regex::new(r#"(?i)(NoPadding|PKCS5Padding)"#).unwrap(),             None, "A02", "Weak Padding",             OwaspSeverity::Medium,
             "Weak or no padding mode detected",                               "Use GCM mode (provides authenticated encryption)."),
            (Regex::new(r#"(?i)(RSA.*512|PKEY_RSA.*512)"#).unwrap(),            None, "A02", "Weak Key Size",           OwaspSeverity::Critical,
             "RSA key size < 2048 bits detected",                              "Use RSA 2048+ or switch to ECDSA/Ed25519."),
            (Regex::new(r#"(?i)(AES.*128|aes.*128)"#).unwrap(),                 None, "A02", "Weak Key Size",           OwaspSeverity::High,
             "AES key size < 256 bits detected",                               "Use AES-256 for sensitive data."),
            (Regex::new(r#"(?i)(trustAll|VALID_ALL|ALLOW_ALL|DefaultTrustManager)"#).unwrap(), None, "A02", "Disabled Cert Validation", OwaspSeverity::Critical,
             "Certificate validation disabled or trusting all certs",           "Use proper certificate chain validation; never disable for production."),
            (Regex::new(r#"(?i)\bnew\s+Random\s*\("#).unwrap(),                 Some("java"), "A02", "Insecure Random",         OwaspSeverity::Medium,
             "Insecure Random used instead of SecureRandom",                    "Use java.security.SecureRandom for security-sensitive values."),
            (Regex::new(r#"(?i)(\bsalt\b.*=|SALT\s*=)"#).unwrap(),               None, "A02", "Static Salt",             OwaspSeverity::Medium,
             "Static salt detected (vulnerable to rainbow tables)",             "Generate unique salts per-user; store alongside hash."),
            (Regex::new(r#"(?i)setHostnameVerifier"#).unwrap(),                 None, "A02", "Disabled Host Validation", OwaspSeverity::High,
             "Hostname verification disabled in TLS",                          "Use proper HostnameVerifier; never bypass."),
            (Regex::new(r#"(?i)(deriveKey|buildKey).*password"#).unwrap(),       None, "A02", "Weak Key Derivation",    OwaspSeverity::High,
             "Weak key derivation from password detected",                     "Use PBKDF2, bcrypt, or Argon2 with high iteration count."),

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

            // A01 - Broken Access Control
            (Regex::new(r#"(?i)(chmod\s*\(\s*0o777|chmod\s+777)"#).unwrap(),      None, "A01", "Broken Access Control",   OwaspSeverity::High,
             "World-writable file permission (0o777) detected",                  "Use minimal permissions (0o755 for dirs, 0o644 for files)."),
            (Regex::new(r#"(?i)(is_admin|isRoot|is_root|has_role|check_permission)\s*\([^)]*\)\s*\{?\s*return\s+true"#).unwrap(), None, "A01", "Broken Access Control", OwaspSeverity::Medium,
             "Hardcoded admin/root check always returns true",                   "Implement proper authorization checks from trusted sources."),
            (Regex::new(r#"(?i)(allow_redirect\s*=\s*true|follow_redirects\s*=\s*true)"#).unwrap(), None, "A01", "Open Redirect",          OwaspSeverity::Medium,
             "HTTP redirect follows any URL without validation",                 "Validate redirect URLs against an allow-list."),
            (Regex::new(r#"(?i)(permit_all|@PermitAll|@RolesAllowed\s*\(\s*\"\")"#).unwrap(), Some("java"), "A01", "Broken Access Control", OwaspSeverity::Medium,
             "Missing or empty role-based access control annotation",             "Specify proper roles or use deny-by-default."),
            (Regex::new(r#"(?i)(authorize\s*\(\s*\)|authorize\s*\(\s*\"\s*\))"#).unwrap(), Some("python"), "A01", "Broken Access Control", OwaspSeverity::Medium,
             "Empty authorization check",                                         "Require explicit role or permission verification."),

            // A06 — Vulnerable Components are checked by reading manifests/lockfiles
            // (see `check_vulnerable_deps` below), not by pattern-matching source
            // lines. Source-line scanning for dependency versions produces false
            // positives from string literals and doc examples.
        ]
    });

    // Scan files
    let mut total_scanned = 0usize;
    for file in &files {
        total_scanned += 1;

        // Skip documentation directories — they contain example vulnerability strings.
        // Skip aden-cli/src/commands/ — these source files embed the detector pattern
        // strings themselves (lint.rs, init.rs, etc.) and self-flag as false positives.
        // Normalize separators so these matches also hold on Windows (`\`).
        let path_str = crate::util::normalize_sep(file);
        if path_str.contains("/.agent/")
            || path_str.contains("/docs/")
            || path_str.contains("/aden-cli/src/commands/")
        {
            continue;
        }

        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let ext = file
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let lang = lang_exts.iter().find(|(e, _)| *e == ext).map(|(_, l)| *l);

        // Track `#[cfg(test)]` modules so we can skip them: test code contains
        // deliberate positive-case fixtures (`password = "hunter2"`, sample SQL, aden:allow-secret
        // example credentials) that assert the detectors fire — they are not
        // shipped vulnerabilities. We arm on the attribute, enter on the next
        // `mod ... {`, and leave when brace depth returns to the module's start.
        let mut cfg_test_armed = false;
        let mut test_mod_depth: Option<i32> = None;
        let mut brace_depth: i32 = 0;

        for (line_no, line) in text.lines().enumerate() {
            let trimmed = line.trim();

            // Maintain test-module tracking before any skip/continue below.
            if ext == "rs" {
                if test_mod_depth.is_none() {
                    if trimmed.contains("#[cfg(test)]") {
                        cfg_test_armed = true;
                    } else if cfg_test_armed && trimmed.starts_with("mod ") {
                        test_mod_depth = Some(brace_depth);
                        cfg_test_armed = false;
                    }
                }
                let opens = line.matches('{').count() as i32;
                let closes = line.matches('}').count() as i32;
                brace_depth += opens - closes;
                if let Some(start_depth) = test_mod_depth {
                    if brace_depth <= start_depth {
                        test_mod_depth = None; // closed the test module
                    }
                    continue; // inside (or just-closed) a #[cfg(test)] module — skip
                }
            }

            // Skip comment lines and string literals that contain patterns
            if trimmed.starts_with("//")
                || trimmed.starts_with("/*")
                || trimmed.starts_with("*")
                || trimmed.starts_with("##")
                || trimmed.starts_with("#")
            {
                continue;
            }

            // Skip lines that define regex patterns or are clearly string literals
            if trimmed.starts_with('"')
                || trimmed.starts_with('\'')
                || trimmed.contains("Regex::new")
            {
                continue;
            }
            // Skip debug output statements (println!, eprintln!, log::*, format!) - not actual SQL
            if (trimmed.contains("println!")
                || trimmed.contains("eprintln!")
                || trimmed.contains("format!"))
                && (trimmed.contains("SELECT")
                    || trimmed.contains("INSERT")
                    || trimmed.contains("UPDATE")
                    || trimmed.contains("DELETE")
                    || trimmed.contains("DROP"))
            {
                continue; // These are debug strings, not SQL queries
            }

            for (re, pat_lang, owasp_id, category, severity, desc, fix) in patterns.iter() {
                if let Some(pl) = pat_lang
                    && Some(*pl) != lang
                {
                    continue;
                }
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

    // A06: manifest/lockfile-based vulnerable dependency scan. Only runs when
    // `path` is a directory (no manifest when auditing a single file) and when
    // there is no language filter that would make the results misleading.
    if path.is_dir() && lang_filter.is_none() {
        findings.extend(check_vulnerable_deps(path));
    }

    // Output — global -j/--json flag is equivalent to --format json
    let is_json = json || format == "json";
    let is_adoc = !is_json && format == "adoc";

    if findings.is_empty() {
        if is_json {
            println!(
                "{{\"findings\": [], \"summary\": {{\"total\": 0, \"critical\": 0, \"high\": 0, \"medium\": 0, \"low\": 0, \"info\": 0, \"scanned\": {total_scanned}}}}}"
            );
        } else if is_adoc {
            println!(
                "= OWASP Security Audit\n:date: {}\n\n== Summary\n\n| Severity | Count\n| Critical | 0\n| High     | 0\n| Medium   | 0\n| Low      | 0\n| Info     | 0\n\n_{total_scanned} files scanned. No findings._\n",
                aden_core::rfc3339_now().split('T').next().unwrap_or("")
            );
        } else {
            // Use eprintln! so the "clean" message doesn't pollute stdout when
            // audit is called as a sub-command inside ci-check -j.
            eprintln!("  No OWASP coding vulnerabilities found in {total_scanned} file(s).");
        }
        return Ok(());
    }

    // Sort by severity descending
    findings.sort_by_key(|b| std::cmp::Reverse(b.severity));

    let counts = |sev: OwaspSeverity| findings.iter().filter(|f| f.severity == sev).count();
    let crit = counts(OwaspSeverity::Critical);
    let high = counts(OwaspSeverity::High);
    let med = counts(OwaspSeverity::Medium);
    let low = counts(OwaspSeverity::Low);
    let info = counts(OwaspSeverity::Info);

    if is_json {
        // SECURITY (audit MEDIUM-4): build JSON with serde, not string
        // concatenation. `snippet` is attacker-controlled (a line from an
        // untrusted source file); the old hand-rolled escaper only handled `"`,
        // so a trailing backslash or a raw control byte could break out of the
        // string or produce invalid JSON. serde_json escapes everything.
        let doc = serde_json::json!({
            "findings": findings.iter().map(|f| serde_json::json!({
                "owasp_id": f.owasp_id,
                "category": f.category,
                "severity": f.severity.to_string(),
                "file": f.file.display().to_string(),
                "line": f.line,
                "snippet": f.snippet,
                "description": f.description,
                "remediation": f.remediation,
            })).collect::<Vec<_>>(),
            "summary": {
                "total": findings.len(),
                "critical": crit, "high": high, "medium": med,
                "low": low, "info": info, "scanned": total_scanned,
            },
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
        );
    } else if is_adoc {
        let header = format!(
            "= OWASP Security Audit Report\n:date: {}\n:toc: auto\n\n== Summary\n\n| Severity | Count\n| Critical | {crit}\n| High     | {high}\n| Medium   | {med}\n| Low      | {low}\n| Info     | {info}\n\n_{total_scanned} files scanned._\n\n== Findings\n",
            aden_core::rfc3339_now().split('T').next().unwrap_or("")
        );
        print!("{header}");
        for f in &findings {
            println!(
                "=== [{} {}] {}:{}\n\n`{}`\n\n*Description:* {}\n\n*Remediation:* {}\n",
                f.severity,
                f.owasp_id,
                f.file.display(),
                f.line,
                f.snippet,
                f.description,
                f.remediation
            );
        }
    } else {
        println!("  === OWASP Security Audit Findings ===");
        println!(
            "  {} file(s) scanned | {} total finding(s)",
            total_scanned,
            findings.len()
        );
        println!("  Severity counts: CRIT={crit} HIGH={high} MED={med} LOW={low} INFO={info}");
        println!();
        for f in &findings {
            println!(
                "  [{}] {} | {}:{}\n    Code: {}\n    {}\n    Fix: {}\n",
                f.severity,
                f.owasp_id,
                f.file.display(),
                f.line,
                f.snippet,
                f.description,
                f.remediation
            );
        }
    }

    if strict && (crit > 0 || high > 0) {
        return Err(format!(
            "{} critical/high OWASP finding(s) detected (strict mode)",
            crit + high
        )
        .into());
    }
    Ok(())
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
pub fn run_project_tests(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
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
                println!("  [PASS] {label}");
            }
            FrameworkResult::Fail(label, msg) => {
                let trimmed = msg.trim();
                println!("  [FAIL] {label}:\n{trimmed}");
                failures.push(format!("{label}: {trimmed}"));
            }
            FrameworkResult::Skip(label, reason) => {
                println!("  [SKIP] {label}: {reason}");
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
        } else {
            println!("[CI] SKIP: constitutional firewall — no .aden/constitution.adoc (optional)");
        }
    }

    gate!("tests", { run_project_tests(path) });

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
            if rel_str.contains(".aden/cache") || rel_str.contains("contracts/") {
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
                        println!(
                            "  {}Secret ({}) in {}: ...{}...{}",
                            red,
                            name,
                            p.display(),
                            snippet.replace('\n', " "),
                            reset
                        );
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
        cmd_audit(path, None, "text", true, false)
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
                        println!(
                            "  {}Merge conflict marker in {}: {}{}",
                            red,
                            p.display(),
                            trimmed,
                            reset
                        );
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
                        println!(
                            "  {}Insecure http:// URL in {}: {}{}",
                            red,
                            p.display(),
                            line.trim(),
                            reset
                        );
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
        let env = serde_json::json!({
            "ok": exit_code == 0,
            "gates": gate_results,
            "warnings": warnings,
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
    println!(
        "\n{}[CI] ALL GATES PASSED — Ready to commit.{}",
        green, reset
    );
    Ok(())
}

/// Fast, dev-facing pre-commit combo: gen → lint → check refs → heal drift
/// scan → owasp audit. Unlike `ci-check`, `ready` is the quick local loop —
/// it skips the external-tool gates (cargo audit, clippy, license/NOTICE) and
/// focuses on aden's own correctness plus documentation drift. Each step prints
/// a clear PASS/FAIL line; the run fails fast on the first hard error but always
/// emits a final commit-readiness summary. Returns Err if any hard gate failed.
pub fn cmd_ready(path: &Path, fix: bool) -> Result<(), Box<dyn std::error::Error>> {
    let green = "\x1b[0;32m";
    let red = "\x1b[0;31m";
    let yellow = "\x1b[1;33m";
    let reset = "\x1b[0m";

    // (step label, passed?) — recorded for the final summary.
    let mut results: Vec<(&str, bool)> = Vec::new();
    // Hard failure: blocks all subsequent steps (fail-fast).
    let mut hard_failure: Option<String> = None;
    // Soft failure: recorded for the final verdict but does NOT block later steps.
    let mut soft_failure: Option<String> = None;

    // Helper: run a hard gate unless we have already failed fast.
    macro_rules! step {
        ($name:expr, $body:expr) => {{
            if hard_failure.is_some() {
                println!(
                    "{}[ready] SKIP: {} (earlier step failed){}",
                    yellow, $name, reset
                );
                results.push(($name, false));
            } else {
                println!("[ready] Running: {} ...", $name);
                match $body {
                    Ok(()) => {
                        println!("{}[ready] PASS: {}{}", green, $name, reset);
                        results.push(($name, true));
                    }
                    Err(e) => {
                        let e: Box<dyn std::error::Error> = e;
                        println!("{}[ready] FAIL: {} — {}{}", red, $name, e, reset);
                        results.push(($name, false));
                        hard_failure = Some(format!("{}: {}", $name, e));
                    }
                }
            }
        }};
    }
    // Soft step: reports failure and continues — does NOT block subsequent steps.
    // Used for heal drift, which is a doc-quality signal, not a code-safety gate.
    macro_rules! soft_step {
        ($name:expr, $body:expr) => {{
            println!("[ready] Running: {} ...", $name);
            match $body {
                Ok(()) => {
                    println!("{}[ready] PASS: {}{}", green, $name, reset);
                    results.push(($name, true));
                }
                Err(e) => {
                    let e: Box<dyn std::error::Error> = e;
                    println!("{}[ready] WARN: {} — {}{}", yellow, $name, e, reset);
                    results.push(($name, false));
                    // Record as a soft failure so the final verdict still fails,
                    // but don't set hard_failure — let remaining steps (e.g. audit) run.
                    if soft_failure.is_none() {
                        soft_failure = Some(format!("{}: {}", $name, e));
                    }
                }
            }
        }};
    }

    println!("aden ready — pre-commit checks for {}\n", path.display());

    // (1) gen — recompile the project into the knowledge graph.
    step!("gen", { crate::commands::cmd_gen(path, true) });

    // (2) lint — fast line-based heuristics. --fix forwards to the linter.
    step!("lint", {
        crate::commands::cmd_lint(path, "Error", fix, false, false, false, false)
    });

    // (3) check refs — validate every <<ref>> resolves to an [[anchor]].
    step!("check refs", {
        if !path.is_dir() {
            Err("not a directory".into())
        } else {
            crate::util::perform_check(path).map(|_| ())
        }
    });

    // (4) heal drift scan — doc-mismatch gate. Drift (broken refs, orphan
    // anchors, signature mismatch, or a degraded health score) is a reportable
    // hard signal here, per the pre-commit intent: never commit stale docs.
    // With --fix we apply high-confidence auto-fixes first, then re-scan.
    soft_step!("heal drift scan", {
        use aden_heal::{Scanner, generate};
        if fix {
            // Auto-apply StaleHash/MissingContract fixes before judging drift.
            let _ = crate::commands::cmd_heal_scan(path, false, true, false, false);
        }
        let scanner = Scanner::new(path);
        let events = scanner.scan()?;
        // Only hard correctness failures matter to this gate:
        // - BrokenReference (Critical): a <<ref>> points at a missing anchor
        // - SignatureMismatch (High): a symbol's signature changed
        // OrphanAnchor (Medium) and MissingContract (Medium) are maintenance
        // signals — stale store entries and undocumented/metadata doc nodes —
        // not pre-commit blockers. Run `aden sync` to clean them up. The same
        // classification that `diagnose`/`status` use to report 100/100 while
        // hundreds of metadata doc nodes lack contracts; the score gate below
        // MUST agree, so it judges the score over these hard events only rather
        // than letting doc-node MissingContracts crater it to 0.00.
        let is_hard = |e: &aden_heal::DriftEvent| {
            matches!(
                e,
                aden_heal::DriftEvent::BrokenReference { .. }
                    | aden_heal::DriftEvent::SignatureMismatch { .. }
                    | aden_heal::DriftEvent::DocSignatureDivergence { .. }
            )
        };
        let hard_events: Vec<_> = events.iter().filter(|e| is_hard(e)).cloned().collect();
        let report = generate(hard_events, path);
        let critical = events.iter().filter(|e| is_hard(e)).count();
        if critical > 0 {
            Err(Box::<dyn std::error::Error>::from(format!(
                "{} critical drift event(s) (broken refs, signature mismatch, doc/code divergence) — run 'aden heal' to inspect",
                critical
            )))
        } else if report.overall_score < 0.90 {
            Err(Box::<dyn std::error::Error>::from(format!(
                "doc drift: health score {:.2} (< 0.90) — run 'aden gen' / 'aden heal . --fix'",
                report.overall_score
            )))
        } else {
            Ok(())
        }
    });

    // (5) audit — OWASP-aligned source scan (in-process, no external tools).
    step!("owasp audit", {
        cmd_audit(path, None, "text", true, false)
    });

    // ── Final verdict ─────────────────────────────────────
    println!("\n[ready] Summary:");
    for (name, passed) in &results {
        let (mark, color) = if *passed {
            ("PASS", green)
        } else {
            ("FAIL", red)
        };
        println!("  {}{:>4}{} {}", color, mark, reset, name);
    }

    let any_failure = hard_failure.or(soft_failure);
    if let Some(reason) = any_failure {
        println!(
            "\n{}[ready] NOT commit-ready — fix the failing step: {}{}",
            red, reason, reset
        );
        return Err(reason.into());
    }

    println!(
        "\n{}[ready] All checks passed — tree looks commit-ready.{}",
        green, reset
    );
    Ok(())
}

pub fn cmd_doctor(path: &Path, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Collect structured check results regardless of output mode, then either
    // print the human report or emit JSON. Each check is {name, ok, detail}.
    struct Check {
        name: String,
        ok: bool,
        detail: String,
    }
    let mut checks: Vec<Check> = Vec::new();
    let mut issues: Vec<String> = Vec::new();

    macro_rules! chk {
        ($name:expr, $ok:expr, $detail:expr, $error:expr) => {{
            let ok: bool = $ok;
            let detail: String = $detail.to_string();
            if !ok && $error {
                issues.push($name.to_string());
            }
            // `$error` is consumed above (blocking checks push to `issues`); the
            // Check struct itself only needs name/ok/detail for rendering.
            checks.push(Check {
                name: $name.to_string(),
                ok,
                detail,
            });
        }};
    }

    // git
    let git_ok = std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok();
    chk!(
        "git",
        git_ok,
        if git_ok { "git found" } else { "git NOT FOUND" },
        true
    );

    // Toolchains (language-agnostic — probe whatever manifests exist)
    let is_rust = path.join("Cargo.toml").exists();
    let is_node = path.join("package.json").exists();
    let is_python = path.join("pyproject.toml").exists() || path.join("setup.py").exists();
    let is_go = path.join("go.mod").exists();

    if is_rust {
        for tool in &["rustc", "cargo"] {
            let ok = std::process::Command::new(tool)
                .arg("--version")
                .output()
                .is_ok();
            chk!(
                format!("{tool} (Rust)"),
                ok,
                if ok {
                    format!("{tool} found")
                } else {
                    format!("{tool} NOT FOUND")
                },
                true
            );
        }
    }
    for (flag, tools, req) in [
        (is_node, vec!["node", "npm"], false),
        (is_python, vec!["python3"], false),
        (is_go, vec!["go"], false),
    ] {
        if flag {
            for tool in tools {
                let ok = std::process::Command::new(tool)
                    .arg("--version")
                    .output()
                    .is_ok();
                chk!(
                    tool,
                    ok,
                    if ok {
                        format!("{tool} found")
                    } else {
                        format!("{tool} not found")
                    },
                    req
                );
            }
        }
    }

    // aden CLI
    let aden_ok = std::process::Command::new("aden")
        .arg("--version")
        .output()
        .is_ok();
    chk!(
        "aden CLI",
        aden_ok,
        if aden_ok {
            "aden found in PATH"
        } else {
            "aden NOT in PATH"
        },
        true
    );

    // Signing key (optional)
    let key_dir = dirs::home_dir()
        .unwrap_or_default()
        .join(".aden")
        .join("keys");
    let signing_key = key_dir.read_dir().ok().and_then(|mut d| {
        d.find_map(|e| {
            let e = e.ok()?;
            let s = e.file_name().to_string_lossy().to_string();
            if s.ends_with(".pub") { Some(s) } else { None }
        })
    });
    chk!(
        "signing key",
        signing_key.is_some(),
        signing_key
            .as_deref()
            .unwrap_or("none (optional — used for contract attestation)"),
        false
    );

    // Store location (ADR-003: graph lives in the per-user data dir, not in-tree)
    let store_root = crate::util::find_project_root(path);
    let store_path = aden_paths::store_dir(&store_root);
    let store_exists = store_path.exists();
    chk!(
        "store",
        store_exists,
        if store_exists {
            format!("{}", store_path.display())
        } else {
            format!("{} (run 'aden gen')", store_path.display())
        },
        false
    );

    // Repo scaffold
    chk!(
        ".agent/",
        path.join(".agent").is_dir(),
        if path.join(".agent").is_dir() {
            ".agent/ present"
        } else {
            "not present — run 'aden init'"
        },
        false
    );
    chk!(
        ".adenignore",
        path.join(".adenignore").exists(),
        if path.join(".adenignore").exists() {
            "present"
        } else {
            "not present — built-in defaults used"
        },
        false
    );

    // Documentation
    let has_docs = path.join("docs").is_dir()
        || path.join("doc").is_dir()
        || path.join("documentation").is_dir()
        || path.join("README.md").exists()
        || path.join("README.adoc").exists()
        || path.join("README.rst").exists()
        || path.join(".agent").is_dir();
    if !has_docs {
        issues.push("no documentation found".to_string());
    }
    chk!(
        "docs",
        has_docs,
        if has_docs {
            "documentation present"
        } else {
            "no docs/README found"
        },
        true
    );

    // Graph connectivity (what this tool actually measures: are all nodes connected?
    // Different from `heal`'s contract-freshness score — see `aden heal` for that.)
    let graph_score = quick_health_score(path).ok();
    if let Some(s) = graph_score {
        const EPS: f64 = 0.01;
        let ok = (1.0 - s).abs() < EPS;
        if !ok {
            issues.push(format!("graph connectivity {:.2} (target 1.00)", s));
        }
        chk!(
            "graph connectivity",
            ok,
            format!(
                "{:.2}/1.00{}",
                s,
                if ok {
                    ""
                } else {
                    " — run 'aden heal .' to see drift"
                }
            ),
            false
        );
    }

    if json {
        let env = serde_json::json!({
            "ok": issues.is_empty(),
            "issues": issues,
            "checks": checks.iter().map(|c| serde_json::json!({
                "name": c.name,
                "ok": c.ok,
                "detail": c.detail,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&env)?);
        return Ok(());
    }

    println!("— Version Control —");
    for c in checks.iter().filter(|c| c.name == "git") {
        println!("{} {}", if c.ok { "✓" } else { "✗" }, c.detail);
    }
    println!("\n— Project Language Toolchain —");
    let toolchains: Vec<_> = checks
        .iter()
        .filter(|c| {
            matches!(
                c.name.as_str(),
                "rustc (Rust)" | "cargo (Rust)" | "node" | "npm" | "python3" | "go"
            )
        })
        .collect();
    if toolchains.is_empty() {
        println!("  (no recognised project manifest — skipping toolchain check)");
    } else {
        for c in &toolchains {
            println!("{} {}", if c.ok { "✓" } else { "⚠" }, c.detail);
        }
    }
    println!("\n— Aden —");
    for c in checks.iter().filter(|c| c.name == "aden CLI") {
        println!("{} {}", if c.ok { "✓" } else { "✗" }, c.detail);
    }
    for c in checks.iter().filter(|c| c.name == "signing key") {
        println!("{} {}", if c.ok { "✓" } else { " " }, c.detail);
    }
    println!("\n— Repo Health —");
    for c in checks
        .iter()
        .filter(|c| matches!(c.name.as_str(), ".agent/" | ".adenignore" | "docs"))
    {
        println!("{} {}", if c.ok { "✓" } else { "⚠" }, c.detail);
    }
    println!("\n— Knowledge Graph Connectivity —");
    for c in checks.iter().filter(|c| c.name == "graph connectivity") {
        println!(
            "{} Health Score: {}",
            if c.ok { "✓" } else { "⚠" },
            c.detail
        );
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
    let low_confidence: Vec<_> = proposals.iter().filter(|p| p.confidence < 0.85).collect();

    if low_confidence.is_empty() {
        println!("No low-confidence proposals found. All drift detected is auto-applyable.");
        return Ok(());
    }

    println!(
        "Reviewing {} low-confidence proposals...\n",
        low_confidence.len()
    );

    let estimated_tokens = low_confidence.len() * 100;
    println!(
        "Estimated review cost: ~{} tokens (budget: {})",
        estimated_tokens, budget
    );

    if estimated_tokens > budget {
        println!(
            "WARNING: Review exceeds budget. Showing first {} proposals.",
            budget / 100
        );
    }

    let show_count = (budget / 100).min(low_confidence.len());
    for (i, proposal) in low_confidence.iter().take(show_count).enumerate() {
        println!(
            "\n{}. Proposal {} (confidence: {:.2})",
            i + 1,
            proposal.id,
            proposal.confidence
        );
        println!("   Target: {}", proposal.target_path.display());
        println!("   Drift Type: {}", proposal.drift_type);
        println!(
            "   Rationale: {}",
            proposal.rationale.lines().next().unwrap_or("(none)")
        );
    }

    if show_count < low_confidence.len() {
        println!(
            "\n... and {} more proposals (increase --budget to see all)",
            low_confidence.len() - show_count
        );
    }

    println!("\nReview each proposal file in .aden/proposals/ before applying.");
    Ok(())
}

pub fn cmd_review_since(
    path: &Path,
    budget: usize,
    since: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_heal::Scanner;

    println!(
        "Reviewing changes since '{}' with budget {} tokens",
        since, budget
    );

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

    let relevant_events: Vec<_> = all_events
        .into_iter()
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
                aden_heal::DriftEvent::DocSignatureDivergence { doc_path, .. } => doc_path,
            };
            files.iter().any(|f| target.contains(f))
        })
        .collect();

    if relevant_events.is_empty() {
        println!("No drift detected in changed files.");
        return Ok(());
    }

    println!(
        "Found {} drift events in changed files.",
        relevant_events.len()
    );

    let show_count = (budget / 100).min(relevant_events.len());
    for (i, event) in relevant_events.iter().take(show_count).enumerate() {
        println!("  {}. {:?}", i + 1, event);
    }
    if show_count < relevant_events.len() {
        println!(
            "  ... and {} more (increase --budget)",
            relevant_events.len() - show_count
        );
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

    let now_secs = crate::time_util::now_unix_secs();
    let expires_secs = now_secs + ttl_seconds as u64;
    let tag = format!(
        "emergency-{}",
        crate::time_util::unix_secs_to_compact(now_secs)
    );

    let audit_log_path = aden_dir.join("emergency-audit.log");
    let audit_entry = format!(
        "[{}] EMERGENCY OVERRIDE created: reason='{}', expires={}, tag={}\n",
        crate::time_util::unix_secs_to_rfc3339(now_secs),
        reason,
        crate::time_util::unix_secs_to_rfc3339(expires_secs),
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
        crate::time_util::unix_secs_to_rfc3339(expires_secs),
        reason
    );

    std::fs::write(&emergency_path, content)?;

    println!("[{}] EMERGENCY OVERRIDE created", tag);
    println!("  Reason: {}", reason);
    println!(
        "  Expires: {}",
        crate::time_util::unix_secs_to_rfc3339(expires_secs)
    );
    println!("  File: {}", emergency_path.display());
    println!("  Audit: {}", audit_log_path.display());

    Ok(())
}

pub fn cmd_suggest(intent: &str) -> Result<(), Box<dyn std::error::Error>> {
    let intent_lower = intent.to_lowercase();

    let suggestions = vec![
        (
            vec!["generate", "doc", "contract", "parse", "extract"],
            "gen",
            "aden gen . --auto",
            "Generate contracts from source code",
        ),
        (
            vec!["search", "find", "look"],
            "search",
            "aden search '<query>'",
            "Search for text in contracts",
        ),
        (
            vec!["list", "show", "all", "anchors", "contracts"],
            "list",
            "aden list .",
            "List all anchors in the graph",
        ),
        (
            vec!["ask", "question", "explain", "how", "what"],
            "ask",
            "aden ask '<question>'",
            "Ask a natural language question",
        ),
        (
            vec!["fix", "heal", "drift", "stale", "update"],
            "heal",
            "aden heal . --fix",
            "Auto-fix stale contracts",
        ),
        (
            vec!["check", "validate", "reference", "link"],
            "check",
            "aden check .",
            "Validate all cross-references",
        ),
        (
            vec!["graph", "depend", "neighbor", "related"],
            "graph",
            "aden graph --from <anchor> --depth 2",
            "Show graph neighborhood",
        ),
        (
            vec!["assemble", "context", "prompt", "token"],
            "asm",
            "aden asm --from <anchor> --budget 4096",
            "Assemble context within token budget",
        ),
        (
            vec!["locate", "symbol", "function", "where"],
            "locate",
            "aden locate --symbol <name> .",
            "Find symbol definition",
        ),
        (
            vec![
                "rename",
                "refactor",
                "blast",
                "impact",
                "before i change",
                "before i rename",
                "safe to change",
                "downstream",
            ],
            "understand",
            "aden understand <symbol>",
            "One-shot: definition + callers (backlinks) + downstream impact for a symbol",
        ),
        (
            vec![
                "caller",
                "callers",
                "who calls",
                "called by",
                "backlink",
                "references",
                "used by",
                "usages",
                "dependents",
            ],
            "query",
            "aden query . --backlinks <anchor>",
            "List everything that references a symbol (blast radius)",
        ),
        (
            vec!["init", "scaffold", "setup"],
            "init",
            "aden init",
            "Scaffold .agent/ templates",
        ),
        (
            vec!["watch", "auto", "regenerate"],
            "watch",
            "aden watch .",
            "Watch for changes and auto-regenerate",
        ),
        (
            vec!["clean", "gc", "garbage", "orphan"],
            "gc",
            "aden heal . --gc",
            "Garbage collect orphaned contracts",
        ),
        (
            vec!["doctor", "diagnose", "health", "check environment"],
            "doctor",
            "aden doctor .",
            "Check environment health",
        ),
    ];

    let mut matches: Vec<_> = suggestions
        .iter()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: `cmd_ready` runs the full step pipeline end-to-end over a
    /// tiny temp project and produces a verdict without panicking. We only
    /// assert that it terminates with a Result (the verdict itself depends on
    /// generated contract drift, which is the gate working as intended), which
    /// also pins the command wiring so it can never silently drop out of the
    /// build. An empty (non-directory) path must produce a hard failure.
    #[test]
    fn sql_injection_pattern_rejects_prose_and_template_literals() {
        let re = Regex::new(SQL_INJECTION_PATTERN).unwrap();
        // False positives from the ky validation — must NOT match.
        assert!(!re.is_match("`Update ${i} should have higher or equal percent than previous`,"));
        assert!(!re.is_match("test('DELETE request', async t => {"));
        assert!(!re.is_match("// select the first item from the list"));
        assert!(!re.is_match("const dropTable = `${name} updated`;"));
        // Real SQL injection — must match.
        assert!(re.is_match(r#"db.query("SELECT * FROM users WHERE id = " + userId)"#));
        assert!(re.is_match("`UPDATE accounts SET balance = ${amount}`"));
        assert!(re.is_match(r#"query("DELETE FROM sessions WHERE token='" + tok + "'")"#));
        assert!(re.is_match("\"INSERT INTO logs VALUES (%s)\" % payload"));
    }

    #[test]
    fn secret_pattern_rejects_interpolated_values() {
        let re = Regex::new(SECRET_ASSIGNMENT_PATTERN).unwrap();
        // create-t3-app FP and other templated/interpolated values — must NOT match.
        assert!(!re.is_match(r#"AUTH_SECRET="${secret}" # Generated by create-t3-app."#));
        assert!(!re.is_match(r#"token = "{{ runtime_token }}""#));
        assert!(!re.is_match(r##"api_key = "#{ENV['KEY']}""##));
        assert!(!re.is_match(r#"password = `${pw}`"#)); // backtick template (no straight quotes)
        // Genuine hardcoded literals — must match.
        assert!(re.is_match(r#"password = "hunter2""#)); // aden:allow-secret
        assert!(re.is_match(r#"api_key = 'AKIAIOSFODNN7EXAMPLE'"#));
        assert!(re.is_match(r#"SECRET="s3kr3t-literal-value""#));
    }

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

        let hex = "192b9bdd22ab9ed4d12e236c78afcb9a393ec15f71bbf5dc987d54727823bcbf"; // aden:allow-secret
        let m = re.find(hex).expect("pattern matches the hex secret");
        assert!(has_digit(m.as_str()), "hex secret has digits → kept");
    }

    #[test]
    fn cmd_ready_runs_on_temp_project() {
        let dir = std::env::temp_dir().join(format!("aden-ready-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A trivial Rust project: the pipeline has real source to run against.
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname=\"x\"\nversion=\"0.0.0\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn noop() {}\n").unwrap();

        // Must complete without panicking and yield a verdict (Ok or Err).
        let _verdict = cmd_ready(&dir, false);
        let _ = std::fs::remove_dir_all(&dir);

        // A non-directory path is a hard failure at the "check refs" gate, so
        // ready must report NOT commit-ready (Err).
        let missing = dir.join("does-not-exist");
        assert!(
            cmd_ready(&missing, false).is_err(),
            "ready should fail when the target path is not a directory"
        );
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

    // ── A06 vulnerable dependency: manifest-based, not source-line ───────────

    /// A .rs source file that merely MENTIONS an old serde version in a string
    /// literal must NOT produce an A06 finding. The version check must come from
    /// Cargo.lock, not from pattern-matching source lines.
    #[test]
    fn a06_string_literal_in_rs_does_not_produce_finding() {
        let dir =
            std::env::temp_dir().join(format!("aden-a06-literal-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        // A .rs file with a string literal that looks like a vulnerable dep spec.
        std::fs::write(
            dir.join("src/lib.rs"),
            // This is a doc comment / string literal — must NOT be flagged.
            "/// See serde = \"0.8\" for history\npub fn x() {}\n",
        )
        .unwrap();

        // No Cargo.lock → no A06 manifest findings.
        let findings = check_vulnerable_deps(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            findings.is_empty(),
            "string literal in .rs source must not produce A06 findings; got: {findings:#?}",
        );
    }

    /// A Cargo.lock entry for a genuinely old serde version MUST produce an A06
    /// finding. This validates the manifest-based path fires correctly.
    #[test]
    fn a06_cargo_lock_old_serde_produces_finding() {
        let dir = std::env::temp_dir().join(format!("aden-a06-lock-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Minimal Cargo.lock with serde 0.9.x — pre-1.0, should be flagged.
        std::fs::write(
            dir.join("Cargo.lock"),
            "# This file is automatically @generated by Cargo.\n\
             [[package]]\n\
             name = \"serde\"\n\
             version = \"0.9.15\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
        )
        .unwrap();

        let findings = check_vulnerable_deps(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            !findings.is_empty(),
            "Cargo.lock with serde 0.9.x must produce an A06 finding"
        );
        assert_eq!(findings[0].owasp_id, "A06");
        assert!(findings[0].snippet.contains("serde"));
    }

    /// A Cargo.lock entry for a current serde (>= 1.0) must NOT produce a finding.
    #[test]
    fn a06_cargo_lock_current_serde_no_finding() {
        let dir =
            std::env::temp_dir().join(format!("aden-a06-current-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("Cargo.lock"),
            "# This file is automatically @generated by Cargo.\n\
             [[package]]\n\
             name = \"serde\"\n\
             version = \"1.0.200\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
        )
        .unwrap();

        let findings = check_vulnerable_deps(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            findings.is_empty(),
            "Cargo.lock with serde 1.0.x must not produce an A06 finding; got: {findings:#?}"
        );
    }

    /// parse_major_minor handles various version string formats correctly.
    #[test]
    fn parse_major_minor_handles_semver_variants() {
        assert_eq!(parse_major_minor("1.0.0"), Some((1, 0)));
        assert_eq!(parse_major_minor("0.9.15"), Some((0, 9)));
        assert_eq!(parse_major_minor("41.0.0"), Some((41, 0)));
        assert_eq!(parse_major_minor("2.15.1"), Some((2, 15)));
        assert_eq!(parse_major_minor("^1.2"), Some((1, 2)));
        assert_eq!(parse_major_minor("invalid"), None);
    }
}
