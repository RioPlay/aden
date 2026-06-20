// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

use crate::types::{OwaspFinding, OwaspSeverity};

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
