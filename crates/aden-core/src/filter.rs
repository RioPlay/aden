// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Path filter: `.adenignore` and `.adenallow` support.

use memchr::memmem;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;

/// Why a discovered file was not emitted into the graph.
///
/// This is deliberately shared by generation, freshness, and coverage
/// reporting.  A file may be safe to search ephemerally while still being
/// unsafe to persist in the graph; callers must record that distinction rather
/// than silently making the file disappear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileDisposition {
    Indexed,
    Ignored,
    /// Reserved for a future policy that can preserve safe metadata while
    /// proving it does not disclose sensitive paths. Current secret handling
    /// emits `SecretPath`/`SecretContent`, never this variant.
    Redacted,
    SecretPath,
    SecretContent,
    Unsupported,
    InvalidEncoding,
    ParseFailed,
    IoFailed,
}

impl FileDisposition {
    /// Classify a path before it is read. Content and parser outcomes are
    /// classified by the generation pipeline after this initial check.
    pub fn for_path(relative: &Path, ignored: bool, supported: bool) -> Self {
        if ignored {
            Self::Ignored
        } else if is_secret_path(relative) {
            Self::SecretPath
        } else if !supported {
            Self::Unsupported
        } else {
            Self::Indexed
        }
    }

    pub fn for_content(content: &str) -> Self {
        if content_has_high_confidence_secret(content) {
            Self::SecretContent
        } else {
            Self::Indexed
        }
    }

    pub fn is_indexed(self) -> bool {
        self == Self::Indexed
    }
}

/// Smart built-in exclusion patterns. These are always applied; a user
/// `.adenignore` extends (never replaces) them — see [`AdenFilter::from_directory`].
///
/// These defaults are deliberately *polyglot*: Aden compiles context for any
/// codebase, so we exclude the build-output and dependency directories of every
/// common ecosystem — not just the one Aden itself is written in. Anything
/// project-specific belongs in a user-authored `.adenignore`, never here.
pub const BUILT_IN_IGNORES: &[&str] = &[
    // Aden's own artifacts and version control
    ".git/",
    ".hg/",
    ".svn/",
    ".agent/",
    ".aden/",
    // Agent / AI-tooling runtime dirs. These hold local state and, in the case
    // of `.claude/worktrees/`, entire git worktree copies of the repo — indexing
    // them multiplies every symbol and tanks graph health. They are gitignored by
    // convention; never index them.
    ".claude/",
    ".opencode/",
    ".grok/",
    ".cursor/",
    ".codex/",
    ".windsurf/",
    // NOTE: do NOT hard-code "contracts/" here — it is aden's *own* output dir
    // name, but it is also *source* in other ecosystems (Solidity/Vyper smart
    // contracts live in `contracts/`). aden's actual output lives under `.aden/`
    // (already skipped). Skipping bare `contracts/` would silently drop the
    // entire codebase of any smart-contract repo — an aden-centric assumption
    // that breaks language-agnosticism.
    // Editor / OS debris
    ".vscode/",
    ".idea/",
    ".vs/",
    "*.swp",
    "*.swo",
    ".DS_Store",
    "Thumbs.db",
    "*.tmp",
    "*.temp",
    // Generic build output (shared across many ecosystems).
    // NOTE: deliberately NOT excluding "bin/" or "packages/" — they are build
    // output in some ecosystems but *source* in others (e.g. `packages/<name>`
    // in npm/pnpm/lerna monorepos, which discovery treats as modules).
    "build/",
    "dist/",
    "out/",
    "obj/",
    // Rust / Cargo. Cargo accepts alternate target directories (commonly
    // `target-<toolchain>`); these are build residue just like `target/`.
    "target/",
    "target-*/",
    ".cargo/",
    // Node / JS / TS
    "node_modules/",
    ".next/",
    ".nuxt/",
    ".svelte-kit/",
    "bower_components/",
    // Python
    "__pycache__/",
    ".venv/",
    "venv/",
    ".tox/",
    ".mypy_cache/",
    ".pytest_cache/",
    ".ruff_cache/",
    "*.egg-info/",
    // Go / vendored dependencies
    "vendor/",
    // JVM (Gradle / Maven)
    ".gradle/",
    // Ruby / Bundler
    ".bundle/",
    // Misc caches and coverage
    ".cache/",
    "coverage/",
];

fn ignore_fingerprint(patterns: &[&str]) -> u64 {
    // Stable FNV-1a rather than DefaultHasher, whose algorithm is not a durable
    // cache contract. A separator prevents concatenation-equivalent lists from
    // colliding (`["ab", "c"]` versus `["a", "bc"]`).
    let mut hash = 0xcbf29ce484222325_u64;
    for pattern in patterns {
        for byte in pattern.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Stable identity of the built-in path policy. Persist this beside generated
/// artifacts so an upgraded binary never trusts a graph built under old rules.
pub fn built_in_ignore_fingerprint() -> u64 {
    ignore_fingerprint(BUILT_IN_IGNORES)
}

/// Machine-wide exclusion list, applied to every project: `<data root>/ignore`
/// (see [`aden_paths::data_root`]). Same syntax as `.adenignore`.
///
/// This is the escape hatch for trees that cannot be recognised structurally:
/// vendored copies, mirrored reference checkouts, generated trees, or a
/// vendored directory under a name no ecosystem convention predicts. It lives
/// outside the repository, so a project never needs an Aden-specific file.
/// `.adenallow` still wins, which keeps deliberate local intent possible.
pub const MACHINE_IGNORE_FILE: &str = "ignore";

/// Comma, semicolon, or newline separated patterns applied to every project.
/// Overrides [`MACHINE_IGNORE_FILE`] for one-off runs and CI, where writing a
/// user file is not desirable.
pub const EXCLUDES_ENV: &str = "ADEN_EXCLUDES";

/// Policy layers a machine owner supplied for every project.
fn machine_ignore_lines() -> Vec<String> {
    machine_ignore_lines_from(&aden_paths::data_root())
}

fn machine_ignore_lines_from(data_root: &Path) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    if let Some(raw) = std::env::var_os(EXCLUDES_ENV) {
        lines.extend(
            raw.to_string_lossy()
                .split([',', ';', '\n'])
                .map(str::to_string),
        );
    }
    if let Ok(text) = std::fs::read_to_string(data_root.join(MACHINE_IGNORE_FILE)) {
        lines.extend(text.lines().map(str::to_string));
    }
    lines
}

/// Version-control markers that mean "this directory is its own repository".
/// A subdirectory holding one is a submodule, nested clone, or linked worktree:
/// its files belong to another project and are never first-party source.
const NESTED_REPO_MARKERS: &[&str] = &[".git", ".hg", ".svn", ".jj", ".pijul"];

/// Whether `dir` is a separate repository nested inside the project.
///
/// Call this for subdirectories only: the project root itself carries a marker
/// by definition. A `.git` *file* (gitfile) counts, which is what a submodule
/// or worktree checkout uses.
pub fn is_nested_repository(dir: &Path) -> bool {
    NESTED_REPO_MARKERS
        .iter()
        .any(|marker| dir.join(marker).exists())
}

/// Universal credential/secret detection. This is a security floor applied at
/// the **indexing** boundary (`gen` → the persistent store), so secret material
/// never enters the knowledge graph where `ask`/`asm` would assemble it into LLM
/// context. It deliberately does NOT gate ephemeral, targeted search (`grep`,
/// `locate`, `audit`) — you must be able to *find* secrets to fix them, and a
/// search result is scoped to the query rather than persisted and re-served.
/// Matching is by filename / extension / parent directory — never repo layout —
/// so it stays language- and project-agnostic.
pub fn is_secret_path(relative: &Path) -> bool {
    let rel = relative.to_string_lossy().replace('\\', "/");

    // Any path segment that is a well-known credential directory.
    const SECRET_DIRS: &[&str] = &[
        ".ssh", ".gnupg", ".aws", ".azure", ".kube", "secrets", "secret",
    ];
    if rel.split('/').any(|seg| SECRET_DIRS.contains(&seg)) {
        return true;
    }

    let name = relative
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    // Exact credential filenames.
    const SECRET_NAMES: &[&str] = &[
        ".env",
        ".envrc",
        ".npmrc",
        ".pypirc",
        ".netrc",
        "_netrc",
        ".htpasswd",
        "credentials",
        "credentials.json",
        "credentials.toml",
        "credentials.yml",
        "id_rsa",
        "id_dsa",
        "id_ecdsa",
        "id_ed25519",
        "kubeconfig",
        ".dockercfg",
    ];
    if SECRET_NAMES.contains(&name.as_str()) {
        return true;
    }
    // dotenv variants (.env.local, .env.production, …) and GCP service accounts.
    if name.starts_with(".env.") || (name.starts_with("service-account") && name.ends_with(".json"))
    {
        return true;
    }

    // Private-key / keystore / encrypted-secret extensions. Terraform state and
    // tfvars files routinely contain plaintext provider credentials, so they are
    // treated as secrets at the indexing boundary.
    //
    // `.asc` is deliberately ABSENT: it is dual-use — the canonical AsciiDoc extension
    // (Pro Git and many doc/book repos use it) AND a PGP armored-data extension. Blanket
    // skipping it silently dropped entire AsciiDoc corpora from the index (empty graph,
    // green health). A real armored PRIVATE key in a `.asc` is still caught by the content
    // gate (`content_has_high_confidence_secret`), which is the right boundary for a
    // dual-use extension: judge by content, not by name.
    const SECRET_EXTS: &[&str] = &[
        "pem", "key", "p8", "p12", "pfx", "keystore", "jks", "kdbx", "ppk", "gpg", "pgp",
        "tfstate", "tfvars",
    ];
    if let Some(ext) = relative.extension().and_then(|e| e.to_str())
        && SECRET_EXTS.contains(&ext.to_ascii_lowercase().as_str())
    {
        return true;
    }
    false
}

/// The first high-confidence credential found in a file: which rule fired and
/// the 1-based line where the match starts. Callers report this so a skipped
/// file can be explained without echoing the secret itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretMatch {
    pub rule: &'static str,
    pub line: usize,
}

/// Whether `content` holds a high-confidence credential; see
/// [`detect_high_confidence_secret`] for the rules and the matched location.
pub fn content_has_high_confidence_secret(content: &str) -> bool {
    detect_high_confidence_secret(content).is_some()
}

/// Detect high-confidence credential patterns before indexing a file or
/// emitting graph-backed source (CWE-798 / CWE-200).
///
/// This complements [`is_secret_path`] by checking structured token and
/// private-key header patterns inside ordinary source or configuration files.
/// Ephemeral `grep` and `audit` retain access for remediation. Private-key
/// headers are checked first, then provider tokens in table order.
pub fn detect_high_confidence_secret(content: &str) -> Option<SecretMatch> {
    // Provider tokens have a fixed prefix; each kind validates its own body.
    const TOKEN_PREFIXES: [(&[u8], TokenKind); 9] = [
        (b"AKIA", TokenKind::Aws),    // AWS access key id
        (b"ASIA", TokenKind::Aws),    // AWS temporary access key id
        (b"ghp_", TokenKind::GitHub), // GitHub personal access token
        (b"gho_", TokenKind::GitHub), // GitHub OAuth token
        (b"ghs_", TokenKind::GitHub), // GitHub server-to-server token
        (b"ghr_", TokenKind::GitHub), // GitHub refresh token
        (b"xoxb-", TokenKind::Slack), // Slack bot token
        (b"xoxp-", TokenKind::Slack), // Slack user token
        (b"sk-", TokenKind::ApiKey),  // OpenAI and Anthropic keys
    ];
    static FINDERS: LazyLock<[(memmem::Finder<'static>, TokenKind); TOKEN_PREFIXES.len()]> =
        LazyLock::new(|| TOKEN_PREFIXES.map(|(prefix, kind)| (memmem::Finder::new(prefix), kind)));

    let bytes = content.as_bytes();
    let (rule, offset) = find_armored_header(bytes).or_else(|| {
        FINDERS.iter().find_map(|(prefix, kind)| {
            find_prefixed_token(bytes, prefix, *kind).map(|offset| (kind.rule(), offset))
        })
    })?;
    let line = memchr::memchr_iter(b'\n', &bytes[..offset]).count() + 1;
    Some(SecretMatch { rule, line })
}

/// Find a PEM or PGP private-key label and delimiter on a single header line,
/// returning the rule name and the offset of its `BEGIN` marker.
fn find_armored_header(content: &[u8]) -> Option<(&'static str, usize)> {
    const BEGIN: &[u8] = b"-----BEGIN ";
    // Split the private-key labels so the implementation stays indexable.
    const PEM_LABEL: &[u8] = concat!("PRIVATE", " KEY").as_bytes();
    const PGP_LABEL: &[u8] = concat!("PGP PRIVATE", " KEY BLOCK").as_bytes();
    static FINDER: LazyLock<memmem::Finder<'static>> = LazyLock::new(|| memmem::Finder::new(BEGIN));
    FINDER.find_iter(content).find_map(|offset| {
        let tail = &content[offset + BEGIN.len()..];
        let label_end = tail.iter().position(|byte| {
            !byte.is_ascii_uppercase() && !byte.is_ascii_digit() && *byte != b' '
        })?;
        // The first byte outside the allowed label must start the delimiter.
        // This also bounds each label to its own line and avoids scanning the
        // same suffix again when malformed BEGIN markers repeat.
        if !tail[label_end..].starts_with(b"-----") {
            return None;
        }
        let label = &tail[..label_end];
        if label.ends_with(PGP_LABEL) {
            Some(("pgp_private_key", offset))
        } else if label.ends_with(PEM_LABEL) {
            Some(("pem_private_key", offset))
        } else {
            None
        }
    })
}

/// Provider token families. Each one decides which byte may precede its
/// prefix, which bytes form the token body, and when that body is a credential.
#[derive(Debug, Clone, Copy)]
enum TokenKind {
    /// `AKIA`/`ASIA` + exactly 16 `[A-Z0-9]`, standing alone as a word.
    Aws,
    /// `gh?_` + at least 36 `[A-Za-z0-9]`.
    GitHub,
    /// `xox?-` + at least 10 `[A-Za-z0-9_-]`.
    Slack,
    /// `sk-` + at least 20 `[A-Za-z0-9_-]` with a digit or uppercase letter.
    ApiKey,
}

impl TokenKind {
    fn rule(self) -> &'static str {
        match self {
            Self::Aws => "aws_access_key",
            Self::GitHub => "github_token",
            Self::Slack => "slack_token",
            Self::ApiKey => "openai_or_anthropic_key",
        }
    }

    /// Whether `byte` directly before the prefix makes it part of a larger
    /// identifier (`task-…`, `EURASIA…`). Slack and API-key prefixes may
    /// follow `_` (`SLACK_TOKEN_xoxb-…`); kebab-case `-` never starts a token.
    fn joins_left(self, byte: u8) -> bool {
        match self {
            Self::Aws | Self::GitHub => is_word_byte(byte),
            Self::Slack | Self::ApiKey => byte.is_ascii_alphanumeric() || byte == b'-',
        }
    }

    fn is_body_byte(self, byte: u8) -> bool {
        match self {
            Self::Aws => byte.is_ascii_uppercase() || byte.is_ascii_digit(),
            Self::GitHub => byte.is_ascii_alphanumeric(),
            Self::Slack | Self::ApiKey => is_word_byte(byte),
        }
    }

    /// Validate the maximal body run after a prefix; `next` is the byte after it.
    fn accepts(self, body: &[u8], next: Option<u8>) -> bool {
        if body.ends_with(b"EXAMPLE") {
            return false;
        }
        match self {
            Self::Aws => body.len() == 16 && !next.is_some_and(is_word_byte),
            Self::GitHub => body.len() >= 36,
            Self::Slack => body.len() >= 10,
            // Kebab-case identifiers are all lowercase; real keys are not.
            Self::ApiKey => {
                body.len() >= 20
                    && body
                        .iter()
                        .any(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
                    && !["placeholder", "dummy", "fake"].iter().any(|word| {
                        body.windows(word.len())
                            .any(|window| window.eq_ignore_ascii_case(word.as_bytes()))
                    })
            }
        }
    }
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

/// Offset of the first `prefix` occurrence that starts a valid `kind` token.
///
/// A prefix glued to a preceding identifier is skipped one byte at a time, so
/// overlapping or nested prefixes are still tried. A prefix that starts a body
/// run but fails validation skips the whole run: nested prefixes inside it
/// share its end and character set, so they can only be shorter, keep the same
/// `EXAMPLE` suffix, and hold a subset of its digits and uppercase letters. A
/// dummy word anywhere in an API-key run marks the whole run as a placeholder.
fn find_prefixed_token(
    content: &[u8],
    prefix: &memmem::Finder<'_>,
    kind: TokenKind,
) -> Option<usize> {
    let mut start = 0;
    while let Some(found) = prefix.find(&content[start..]) {
        let offset = start + found;
        if offset > 0 && kind.joins_left(content[offset - 1]) {
            start = offset + 1;
            continue;
        }
        let body_start = offset + prefix.needle().len();
        let run_len = content[body_start..]
            .iter()
            .take_while(|byte| kind.is_body_byte(**byte))
            .count();
        let body_end = body_start + run_len;
        if kind.accepts(
            &content[body_start..body_end],
            content.get(body_end).copied(),
        ) {
            return Some(offset);
        }
        start = body_end;
    }
    None
}

/// A compiled path filter combining `.adenignore` and `.adenallow` rules.
#[derive(Debug, Clone)]
pub struct AdenFilter {
    ignore_patterns: Vec<GlobRule>,
    ignore_dirs: HashSet<String>,
    ignore_pattern_count: usize,
    allow_patterns: Vec<GlobRule>,
}

impl AdenFilter {
    pub fn pattern_count(&self) -> usize {
        self.ignore_pattern_count
    }

    /// Load filter from project root.
    ///
    /// Three layers, all additive: the built-in defaults
    /// ([`BUILT_IN_IGNORES`]), the machine-wide [`MACHINE_IGNORE_FILE`] and
    /// [`EXCLUDES_ENV`] policy, then the project's own `.adenignore`. None of
    /// them replaces another, and `.adenallow` can re-include any match. This is
    /// a safety floor: adding one project-specific rule must never silently
    /// re-expose `target/`, `node_modules/`, `.git/`, or the agent-runtime
    /// worktrees the defaults exist to exclude.
    pub fn from_directory(dir: &Path) -> Self {
        Self::from_directory_with(dir, &machine_ignore_lines())
    }

    /// Compose the filter for `dir` plus already-resolved extra patterns.
    /// Tests and callers that resolved policy themselves use this instead of
    /// reading machine state.
    pub fn from_directory_with(dir: &Path, extra: &[String]) -> Self {
        let ignore_file = dir.join(".adenignore");
        let allow_file = dir.join(".adenallow");

        let mut ignore_lines: Vec<String> =
            BUILT_IN_IGNORES.iter().map(|s| s.to_string()).collect();
        ignore_lines.extend(extra.iter().cloned());
        if ignore_file.exists() {
            ignore_lines.extend(read_lines(&ignore_file).unwrap_or_default());
        }

        let allow_lines = if allow_file.exists() {
            read_lines(&allow_file).unwrap_or_default()
        } else {
            Vec::new()
        };

        Self::from_rules(compile_rules(&ignore_lines), compile_rules(&allow_lines))
    }

    fn from_rules(ignore_patterns: Vec<GlobRule>, allow_patterns: Vec<GlobRule>) -> Self {
        let ignore_pattern_count = ignore_patterns.len();
        let mut ignore_dirs = HashSet::new();
        let ignore_patterns = ignore_patterns
            .into_iter()
            .filter_map(|rule| {
                if rule.is_dir_rule && !rule.pattern.contains(['/', '*']) {
                    ignore_dirs.insert(rule.pattern);
                    None
                } else {
                    Some(rule)
                }
            })
            .collect();
        Self {
            ignore_patterns,
            ignore_dirs,
            ignore_pattern_count,
            allow_patterns,
        }
    }

    /// Directory-walk decision for one entry: descend unless policy excludes
    /// the path or the directory is a separate repository nested in the
    /// project. `dir` is `None` for file entries, which carry no marker.
    pub fn should_descend(&self, relative: &Path, dir: Option<&Path>) -> bool {
        !self.should_skip(relative) && !dir.is_some_and(is_nested_repository)
    }

    /// Determine if a path relative to the project root should be skipped.
    pub fn should_skip(&self, relative: &Path) -> bool {
        // Normalize separators so Windows backslash paths match slash patterns.
        let rel_str = relative.to_string_lossy().replace('\\', "/");
        let matched_ignore = rel_str
            .split('/')
            .any(|part| self.ignore_dirs.contains(part))
            || self.ignore_patterns.iter().any(|r| r.matches(&rel_str));
        if !matched_ignore {
            return false;
        }
        // If allowed, don't skip
        let matched_allow = self.allow_patterns.iter().any(|r| r.matches(&rel_str));
        !matched_allow
    }
}

/// A single glob-like rule. Supports `*` (any chars) and `**/` (any depth).
#[derive(Debug, Clone)]
struct GlobRule {
    pattern: String,
    is_dir_rule: bool,
}

impl GlobRule {
    fn matches(&self, path: &str) -> bool {
        if self.is_dir_rule {
            let trimmed = self.pattern.trim_end_matches('/');
            // Dots are literal: `.vs/` is editor state, while `src/vs/`
            // contains the main source tree in VS Code.
            dir_rule_matches(path, trimmed)
        } else {
            match_glob(path, &self.pattern)
        }
    }
}

/// Match a directory ignore rule against a relative path.
///
/// Anchored prefix matching handles root-level and multi-segment rules
/// (`target/custom-tool/`). A bare single-segment name *additionally* matches a
/// directory of that name at ANY depth — gitignore semantics — so `.aden/`
/// prunes a nested `crates/foo/.aden/` (where per-crate caches live), not only a
/// top-level `.aden/`. Without this, generated artifacts leak into grep/index.
fn dir_rule_matches(path: &str, dir: &str) -> bool {
    if path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
    {
        return true;
    }
    if !dir.contains('/') {
        if dir.contains('*') {
            // Wildcards only match ancestor segments, preserving source files
            // such as `target-helper.rs` in the final segment.
            return path.rsplit_once('/').is_some_and(|(parents, _)| {
                parents.split('/').any(|segment| match_glob(segment, dir))
            });
        }
        return path.split('/').any(|segment| segment == dir);
    }
    false
}

fn compile_rules(lines: &[String]) -> Vec<GlobRule> {
    lines
        .iter()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            let is_dir = trimmed.ends_with('/');
            let pat = trimmed
                .trim_end_matches('/')
                .trim_start_matches('/')
                .to_string();
            Some(GlobRule {
                pattern: pat,
                is_dir_rule: is_dir,
            })
        })
        .collect()
}

fn match_glob(path: &str, pattern: &str) -> bool {
    // Very simple glob: only handles * wildcards
    let Some((prefix, suffix)) = pattern.split_once('*') else {
        return path == pattern;
    };
    if !suffix.contains('*') {
        return path.starts_with(prefix)
            && path.ends_with(suffix)
            && path.len() >= prefix.len() + suffix.len();
    }
    // Fallback: exact match on base name for simple patterns
    if let Some(name) = Path::new(path).file_name().and_then(|n| n.to_str()) {
        return name == pattern.trim_start_matches("*/");
    }
    false
}

fn read_lines(path: &Path) -> std::io::Result<Vec<String>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text.lines().map(|s| s.to_string()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_policy_fingerprint_is_stable_and_content_sensitive() {
        assert_eq!(
            built_in_ignore_fingerprint(),
            ignore_fingerprint(BUILT_IN_IGNORES)
        );
        assert_ne!(
            ignore_fingerprint(&["ab", "c"]),
            ignore_fingerprint(&["a", "bc"])
        );
        let mut changed = BUILT_IN_IGNORES.to_vec();
        changed.push("generated-checkout-*/");
        assert_ne!(built_in_ignore_fingerprint(), ignore_fingerprint(&changed));
    }

    #[test]
    fn machine_policy_layers_extend_builtins_and_allow_still_wins() {
        let data_root =
            std::env::temp_dir().join(format!("aden-machine-ignore-{}", std::process::id()));
        std::fs::create_dir_all(&data_root).unwrap();
        std::fs::write(
            data_root.join(MACHINE_IGNORE_FILE),
            "# mirrored trees\n.repos/\ngenerated-*/\n",
        )
        .unwrap();
        let machine = machine_ignore_lines_from(&data_root);
        assert!(machine.iter().any(|p| p == ".repos/"));
        assert!(machine.iter().any(|p| p == "generated-*/"));
        std::fs::remove_dir_all(&data_root).ok();

        // An explicit allow can still re-include one deliberate mirror.
        let project =
            std::env::temp_dir().join(format!("aden-machine-project-{}", std::process::id()));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join(".adenallow"), ".repos/keep-me/\n").unwrap();
        let filter = AdenFilter::from_directory_with(&project, &[".repos/".to_string()]);
        assert!(filter.should_skip(Path::new(".repos/mirror/packages/aws/src/index.ts")));
        assert!(!filter.should_skip(Path::new(".repos/keep-me/src/index.ts")));
        assert!(filter.should_skip(Path::new("node_modules/lodash/index.js")));
        assert!(!filter.should_skip(Path::new("apps/server/src/main.ts")));
        std::fs::remove_dir_all(&project).ok();
    }

    #[test]
    fn nested_repository_markers_prune_descendants() {
        let root = std::env::temp_dir().join(format!("aden-nested-repo-{}", std::process::id()));
        let nested = root.join("libs/dep");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(
            !is_nested_repository(&nested),
            "a plain directory is project source"
        );
        // A submodule or worktree checkout carries a gitfile, not a directory.
        std::fs::write(nested.join(".git"), "gitdir: ../../.git/modules/dep").unwrap();
        assert!(is_nested_repository(&nested));
        std::fs::remove_file(nested.join(".git")).unwrap();
        std::fs::create_dir_all(nested.join(".git")).unwrap();
        assert!(is_nested_repository(&nested));

        let filter = AdenFilter::from_directory_with(&root, &[]);
        assert!(!filter.should_descend(Path::new("libs/dep"), Some(&nested)));
        assert!(filter.should_descend(Path::new("libs"), Some(&root.join("libs"))));
        assert!(filter.should_descend(Path::new("libs/dep/src/main.rs"), None));
        // Pattern policy still applies to directories that are not nested repos.
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        assert!(!filter.should_descend(Path::new("target"), Some(&root.join("target"))));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_builtin_skips_target() {
        let filter = AdenFilter::from_directory(&std::env::temp_dir().join("aden-nonexistent"));
        assert!(filter.should_skip(Path::new("target/debug/incremental")));
        assert!(filter.should_skip(Path::new("target-0.4/debug/deps")));
        assert!(filter.should_skip(Path::new("crates/widget/target-nightly/debug/widget")));
        assert!(!filter.should_skip(Path::new("src/target-helper.rs")));
        assert!(filter.should_skip(Path::new(".git/hooks/pre-commit")));
        assert!(filter.should_skip(Path::new("node_modules/lodash")));
        assert!(!filter.should_skip(Path::new("src/lib.rs")));
        assert!(!filter.should_skip(Path::new("agent/file.adoc")));
        assert!(filter.should_skip(Path::new(".agent/file.adoc")));
        assert!(filter.should_skip(Path::new("src/.vs/cache")));
        assert!(!filter.should_skip(Path::new("src/vs/base/common/lifecycle.ts")));
        assert!(!filter.should_skip(Path::new("src\\vs\\base\\common\\lifecycle.ts")));
    }

    #[test]
    fn test_builtin_skips_are_polyglot() {
        let filter = AdenFilter::from_directory(&std::env::temp_dir().join("aden-nonexistent"));
        // Build/dependency dirs from many ecosystems are excluded by default.
        assert!(filter.should_skip(Path::new("__pycache__/mod.cpython-312.pyc")));
        assert!(filter.should_skip(Path::new(".venv/lib/python3.12/site.py")));
        assert!(filter.should_skip(Path::new("vendor/golang.org/x/foo.go")));
        assert!(filter.should_skip(Path::new(".gradle/caches/modules")));
        assert!(filter.should_skip(Path::new("dist/bundle.js")));
        assert!(filter.should_skip(Path::new("obj/Debug/App.dll")));
        // …but real source in any language is kept, including monorepo
        // `packages/` and `bin/` which are source dirs in some ecosystems.
        assert!(!filter.should_skip(Path::new("app/main.py")));
        assert!(!filter.should_skip(Path::new("packages/api/src/index.ts")));
        assert!(!filter.should_skip(Path::new("bin/console.rb")));
        assert!(!filter.should_skip(Path::new("cmd/server/main.go")));
    }

    #[test]
    fn test_builtin_skips_agent_tooling_runtime() {
        let filter = AdenFilter::from_directory(&std::env::temp_dir().join("aden-nonexistent"));
        // Agent-runtime dirs (and the git-worktree copies inside them) must never
        // be indexed — regression for the .claude/worktrees pollution.
        assert!(filter.should_skip(Path::new(".claude/settings.json")));
        assert!(filter.should_skip(Path::new(
            ".claude/worktrees/agent-abc/crates/aden-core/src/filter.rs"
        )));
        assert!(filter.should_skip(Path::new(".opencode/opencode.jsonc")));
    }

    #[test]
    fn test_adenignore_extends_builtins() {
        // A user `.adenignore` must EXTEND the built-in defaults, not replace
        // them: adding one custom rule cannot silently re-expose target/, .git/,
        // node_modules/, or the agent worktrees.
        let dir = std::env::temp_dir().join(format!("aden-filter-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".adenignore"), "fixtures/\n").unwrap();
        let filter = AdenFilter::from_directory(&dir);
        // The user rule is honored…
        assert!(filter.should_skip(Path::new("fixtures/sample.rs")));
        // …and the built-ins are still in force.
        assert!(filter.should_skip(Path::new("target/debug/foo")));
        assert!(filter.should_skip(Path::new("node_modules/lodash")));
        assert!(filter.should_skip(Path::new(".claude/worktrees/agent-x/src/main.rs")));
        assert!(!filter.should_skip(Path::new("src/lib.rs")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_dotfile_pattern_from_ignore() {
        let filter = AdenFilter::from_rules(compile_rules(&[".agent/".to_string()]), Vec::new());
        assert!(
            !filter.should_skip(Path::new("agent/file.adoc")),
            "Dots in directory rules are literal"
        );
        assert!(
            filter.should_skip(Path::new(".agent/templates/foo.adoc")),
            "Should match .agent/templates/"
        );
        assert!(!filter.should_skip(Path::new("src/main.rs")));
    }

    #[test]
    fn test_dir_rule_matches_nested_dir() {
        // Regression: a bare `.aden/` rule must prune the per-crate caches at
        // `crates/<x>/.aden/...`, not only a top-level `.aden/`. Otherwise the
        // generated index-cache.json leaks into grep/index results.
        let filter = AdenFilter::from_rules(
            compile_rules(&[".aden/".to_string(), "target/".to_string()]),
            Vec::new(),
        );
        assert!(filter.should_skip(Path::new(".aden/store")));
        assert!(filter.should_skip(Path::new("crates/aden-cli/.aden")));
        assert!(filter.should_skip(Path::new("crates/aden-cli/.aden/cache/index-cache.json")));
        assert!(filter.should_skip(Path::new("crates/foo/target/debug/x")));
        // A real source file with no ignored segment is still kept.
        assert!(!filter.should_skip(Path::new("crates/aden-cli/src/main.rs")));
    }

    #[test]
    fn test_allow_overrides_ignore() {
        let filter = AdenFilter::from_rules(
            compile_rules(&["target/".to_string()]),
            compile_rules(&["target/custom-tool/".to_string()]),
        );
        assert!(filter.should_skip(Path::new("target/debug")));
        assert!(!filter.should_skip(Path::new("target/custom-tool/main.rs")));
    }

    #[test]
    fn indexed_ignore_rules_preserve_configured_counts_and_allow_precedence() {
        let ignore = ["target/", "target/", "*.tmp", "# comment", " "].map(str::to_string);
        let allow = ["target/custom-tool/", "target/keep.tmp"].map(str::to_string);
        let filter = AdenFilter::from_rules(compile_rules(&ignore), compile_rules(&allow));
        assert_eq!(filter.pattern_count(), 3);
        assert_eq!(filter.ignore_dirs.len(), 1);
        assert_eq!(filter.ignore_patterns.len(), 1);
        assert!(filter.should_skip(Path::new("nested/target/debug")));
        assert!(!filter.should_skip(Path::new("target/custom-tool/main.rs")));
        assert!(!filter.should_skip(Path::new("target/keep.tmp")));
        assert!(filter.should_skip(Path::new("other.tmp")));
    }

    #[test]
    fn optimized_rules_match_legacy_special_pattern_behavior() {
        // Keep the prior matcher as an oracle. Its deliberately limited glob
        // semantics, including multi-star fallback, must survive optimization.
        fn legacy_glob(path: &str, pattern: &str) -> bool {
            if !pattern.contains('*') {
                return path == pattern;
            }
            let parts: Vec<_> = pattern.split('*').collect();
            if parts.len() == 2 {
                return path.starts_with(parts[0])
                    && path.ends_with(parts[1])
                    && path.len() >= parts[0].len() + parts[1].len();
            }
            Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == pattern.trim_start_matches("*/"))
        }
        fn legacy_rule(rule: &GlobRule, path: &str) -> bool {
            if !rule.is_dir_rule {
                return legacy_glob(path, &rule.pattern);
            }
            let dir = rule.pattern.trim_end_matches('/');
            if path == dir || path.starts_with(&format!("{dir}/")) {
                return true;
            }
            if dir.contains('/') {
                return false;
            }
            let segments: Vec<_> = path.split('/').collect();
            segments.iter().enumerate().any(|(index, segment)| {
                if dir.contains('*') {
                    index + 1 < segments.len() && legacy_glob(segment, dir)
                } else {
                    *segment == dir
                }
            })
        }
        let patterns = [
            "target/",
            "/target/",
            ".vs/",
            "target-*/",
            "*.egg-info/",
            "target/custom/",
            "*/",
            "**/",
            "**/file.rs",
            "a*b*c",
            "*.tmp",
            "*file.rs",
            "file*",
            "/",
            "///",
            "a//",
            "a?b/",
            "[ab]/",
        ];
        let components = [
            "",
            "target",
            "target-fast",
            "target-*",
            "file.rs",
            ".vs",
            "vs",
            "a",
            "a?b",
            "[ab]",
            "x.tmp",
        ];
        for pattern in patterns {
            let rules = compile_rules(&[pattern.to_string()]);
            let filter = AdenFilter::from_rules(rules.clone(), Vec::new());
            for first in components {
                for second in components {
                    for third in components {
                        let path = format!("{first}/{second}/{third}");
                        let expected = legacy_rule(&rules[0], &path);
                        assert_eq!(rules[0].matches(&path), expected, "{pattern:?}: {path:?}");
                        assert_eq!(
                            filter.should_skip(Path::new(&path)),
                            expected,
                            "indexed {pattern:?}: {path:?}"
                        );
                    }
                }
            }
            for path in components {
                assert_eq!(
                    filter.should_skip(Path::new(path)),
                    legacy_rule(&rules[0], path),
                    "{pattern:?}: {path:?}"
                );
            }
        }
    }

    #[test]
    fn content_secret_detects_real_credentials() {
        // Embedded in an ordinary-looking source/config file the path filter misses.
        // Build test tokens via byte arrays to avoid static pattern detection.
        // NOTE: the AWS token body must NOT end in `EXAMPLE` — that suffix marks a
        // documentation placeholder, which the detector deliberately ignores
        // (see `content_secret_ignores_example_placeholders`). Body: IOSFODNN7REALKEY.
        let aws_key = String::from_utf8_lossy(&[
            123, 32, 34, 97, 119, 115, 95, 107, 101, 121, 34, 58, 32, 34, 65, 75, 73, 65, 73, 79,
            83, 70, 79, 68, 78, 78, 55, 82, 69, 65, 76, 75, 69, 89, 34, 32, 125,
        ]);
        assert!(content_has_high_confidence_secret(&aws_key));

        let github_token = String::from_utf8_lossy(&[
            108, 101, 116, 32, 116, 32, 61, 32, 34, 103, 104, 112, 95, 48, 49, 50, 51, 52, 53, 54,
            55, 56, 57, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111,
            112, 113, 114, 115, 116, 117, 118, 119, 120, 121, 122, 34, 59,
        ]);
        assert!(content_has_high_confidence_secret(&github_token));

        let openai_key = String::from_utf8_lossy(&[
            79, 80, 69, 78, 65, 73, 95, 65, 80, 73, 95, 75, 69, 89, 61, 115, 107, 45, 97, 98, 99,
            100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116,
            117, 118, 119, 120, 121, 122, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57,
        ]);
        assert!(content_has_high_confidence_secret(&openai_key));

        let pem_key = String::from_utf8_lossy(&[
            45, 45, 45, 45, 45, 66, 69, 71, 73, 78, 32, 82, 83, 65, 32, 80, 82, 73, 86, 65, 84, 69,
            32, 75, 69, 89, 45, 45, 45, 45, 45, 10, 77, 73, 73, 69, 46, 46, 46, 10, 45, 45, 45, 45,
            45, 69, 78, 68, 32, 82, 83, 65, 32, 80, 82, 73, 86, 65, 84, 69, 32, 75, 69, 89, 45, 45,
            45, 45, 45,
        ]);
        assert!(content_has_high_confidence_secret(&pem_key));
    }

    #[test]
    fn asc_is_dual_use_doc_not_blanket_secret() {
        // `.asc` is the canonical AsciiDoc extension (Pro Git and many book/doc repos),
        // so it must index — NOT be skipped by extension as a PGP file. Other crypto
        // extensions stay protected.
        assert!(!is_secret_path(Path::new(
            "book/03-git-tools/sections/signing.asc"
        )));
        assert!(!is_secret_path(Path::new("docs/intro.asc")));
        assert!(is_secret_path(Path::new("secrets/server.pem")));
        assert!(is_secret_path(Path::new("backup/key.gpg")));

        // A real armored PRIVATE key inside a `.asc` is still caught by the CONTENT gate.
        // Marker built by concat so this source file never holds it contiguously.
        let priv_block = format!(
            "-----BEGIN {}\nlQVYBF...\n-----END {}",
            ["PGP PRIVATE", " KEY BLOCK-----"].concat(),
            ["PGP PRIVATE", " KEY BLOCK-----"].concat()
        );
        assert!(content_has_high_confidence_secret(&priv_block));

        // A PUBLIC key block or detached signature (what a signing chapter shows) is NOT a
        // secret, so the AsciiDoc page still indexes.
        assert!(!content_has_high_confidence_secret(
            "-----BEGIN PGP PUBLIC KEY BLOCK-----\nmQENBF...\n-----END PGP PUBLIC KEY BLOCK-----"
        ));
        assert!(!content_has_high_confidence_secret(
            "-----BEGIN PGP SIGNATURE-----\niQEzBA...\n-----END PGP SIGNATURE-----"
        ));
    }

    #[test]
    fn content_secret_ignores_example_placeholders() {
        // AWS's canonical documentation key `AKIAIOSFODNN7EXAMPLE` (and any
        // token ending in the `EXAMPLE` convention) is a placeholder, not a
        // credential — it must not be flagged. Built via bytes to avoid the
        // file itself tripping the indexing screen.
        let example_key = String::from_utf8_lossy(&[
            123, 32, 34, 97, 119, 115, 95, 107, 101, 121, 34, 58, 32, 34, 65, 75, 73, 65, 73, 79,
            83, 70, 79, 68, 78, 78, 55, 69, 88, 65, 77, 80, 76, 69, 34, 32, 125,
        ]);
        assert!(
            !content_has_high_confidence_secret(&example_key),
            "AWS EXAMPLE placeholder key must not be treated as a real secret"
        );
    }

    #[test]
    fn content_secret_preserves_overlapping_and_nested_token_prefixes() {
        let repeated_examples = format!("{}EXAMPLE", "AKIA".repeat(128));
        assert!(!content_has_high_confidence_secret(&repeated_examples));
        assert!(content_has_high_confidence_secret(&format!(
            "{repeated_examples}éAKIA{}",
            "0".repeat(16)
        )));
        assert!(content_has_high_confidence_secret(&format!(
            "πAKIAKIA{}éEXAMPLE",
            "7".repeat(13)
        )));

        // The outer GitHub prefix is too short, but the nested Slack token
        // reaches its own length threshold.
        assert!(content_has_high_confidence_secret(&format!(
            "ghp_xoxb-{}",
            "z".repeat(10)
        )));
        assert!(!content_has_high_confidence_secret(&format!(
            "ghp_xoxb-{}EXAMPLE",
            "z".repeat(10)
        )));
    }

    #[test]
    fn content_secret_preserves_armored_header_boundaries() {
        let begin = "-----BEGIN ";
        let pem = concat!("PRIVATE", " KEY-----");
        let pgp = concat!("PGP PRIVATE", " KEY BLOCK-----");
        for (header, expected) in [
            (format!("{begin}{pem}"), true),
            (format!("{begin}RSA2{pem}-- trailing text"), true),
            (format!("{begin}  {pgp}\r\n"), true),
            (format!("{begin}RSA\r\n{pem}"), false),
            (format!("{begin}RSA\r{pem}"), false),
            (format!("{begin}RSA\t{pem}"), false),
            (format!("{begin}RSAÉ {pem}"), false),
            (format!("{begin}RSA-{pem}"), false),
            (format!("{begin}bad {pem} RSA {pgp}"), false),
            (format!("{}{pem}", begin.repeat(128)), true),
        ] {
            assert_eq!(
                content_has_high_confidence_secret(&header),
                expected,
                "{header:?}"
            );
        }
    }

    #[test]
    fn content_secret_no_false_positives_on_normal_code() {
        // Must NOT fire on ordinary source — a false positive silently drops a
        // legitimate file from the graph, so the bar is high-confidence only.
        for src in [
            "fn main() { let sk = compute(); println!(\"{sk}\"); }", // `sk` as an identifier
            "// AKIA is an AWS key prefix; this comment mentions it", // prefix w/o a token body
            "let url = \"https://api.github.com/repos/x/y\";",
            "const RETRIES: usize = 3; let total = a + b;",
            "ghp_ token format starts with ghp underscore", // prefix, but no 36-char body
        ] {
            assert!(
                !content_has_high_confidence_secret(src),
                "false positive on: {src:?}"
            );
        }
    }

    #[test]
    fn content_secret_ignores_identifiers_that_contain_token_prefixes() {
        // Regression: production TypeScript was skipped because `sk-` matched
        // inside `task-…`/`ask-…` test ids and `ASIA` matched mid-identifier.
        for src in [
            "<button data-testid=\"task-create-new-session-button\" />".to_owned(),
            "openModal('ask-user-question-modal');".to_owned(),
            "<div className=\"NewTaskSelector-sk-panel\" />".to_owned(),
            "export const ASIA_PACIFIC_REGION_CONFIG = load();".to_owned(),
            "const region = \"EURASIA12345678901234\";".to_owned(),
            "navigate(\"sk-some-long-kebab-case-identifier\");".to_owned(),
            format!("key = \"sk-dummy-{}\"", "A1".repeat(10)),
            format!("key = \"AKIA{}x\"", "Q7".repeat(8)),
            format!("token = x_ghp_{}", "a1B2".repeat(9)),
        ] {
            assert_eq!(
                detect_high_confidence_secret(&src),
                None,
                "false positive on: {src:?}"
            );
        }
        assert_eq!(
            detect_high_confidence_secret(include_str!("filter.rs")),
            None
        );
    }

    #[test]
    fn detect_secret_reports_rule_and_line() {
        // Fixtures are assembled at runtime so this file stays indexable. The
        // preamble holds near-misses the scan must step past on every rule.
        let preamble = "// task-create-new-session-button\n\
                        navigate(\"sk-some-long-kebab-case-identifier\");\n\
                        fn main() {}\n";
        for (extra_lines, secret, rule) in [
            (
                0,
                format!("-----BEGIN RSA {}", concat!("PRIVATE", " KEY-----")),
                "pem_private_key",
            ),
            (
                1,
                format!("-----BEGIN {}", concat!("PGP PRIVATE", " KEY BLOCK-----")),
                "pgp_private_key",
            ),
            (
                2,
                format!("key = \"AKIA{}\"", "Q7".repeat(8)),
                "aws_access_key",
            ),
            (
                3,
                format!("token: ghp_{}", "a1B2".repeat(9)),
                "github_token",
            ),
            (
                4,
                format!("SLACK_BOT=xoxb-{}", "123-abc".repeat(3)),
                "slack_token",
            ),
            (
                5,
                format!("OPENAI_API_KEY=sk-proj-{}", "Ab3".repeat(8)),
                "openai_or_anthropic_key",
            ),
            (
                6,
                format!("ANTHROPIC_API_KEY=\"sk-ant-api03-{}\"", "xY9_".repeat(6)),
                "openai_or_anthropic_key",
            ),
        ] {
            let content = format!(
                "{preamble}{}{secret}\ntrailing();\n",
                "\n".repeat(extra_lines)
            );
            assert_eq!(
                detect_high_confidence_secret(&content),
                Some(SecretMatch {
                    rule,
                    line: 4 + extra_lines,
                }),
                "{content:?}"
            );
        }
    }

    #[test]
    fn disposition_keeps_normal_filter_source_indexable() {
        // Regression for the original AP-102 report: the detector's own
        // implementation must never classify as credential content merely
        // because it describes credential patterns.
        let source = include_str!("filter.rs");
        assert_eq!(
            FileDisposition::for_path(Path::new("crates/aden-core/src/filter.rs"), false, true),
            FileDisposition::Indexed
        );
        assert_eq!(
            FileDisposition::for_content(source),
            FileDisposition::Indexed
        );
    }

    #[test]
    fn disposition_records_path_and_content_secret_separately() {
        assert_eq!(
            FileDisposition::for_path(Path::new("secrets/deploy.rs"), false, true),
            FileDisposition::SecretPath
        );
        let token = String::from_utf8_lossy(&[
            103, 104, 112, 95, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 97, 98, 99, 100, 101, 102,
            103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 117, 118, 119,
            120, 121, 122,
        ]);
        assert_eq!(
            FileDisposition::for_content(&token),
            FileDisposition::SecretContent
        );
    }

    #[test]
    fn disposition_serialization_keeps_redaction_reserved_and_explicit() {
        let encoded = serde_json::to_string(&FileDisposition::Redacted).unwrap();
        assert_eq!(encoded, "\"redacted\"");
        assert_eq!(
            serde_json::from_str::<FileDisposition>(&encoded).unwrap(),
            FileDisposition::Redacted
        );
    }

    #[test]
    fn every_disposition_has_a_stable_wire_value() {
        for disposition in [
            FileDisposition::Indexed,
            FileDisposition::Ignored,
            FileDisposition::Redacted,
            FileDisposition::SecretPath,
            FileDisposition::SecretContent,
            FileDisposition::Unsupported,
            FileDisposition::InvalidEncoding,
            FileDisposition::ParseFailed,
            FileDisposition::IoFailed,
        ] {
            let encoded = serde_json::to_string(&disposition).unwrap();
            assert_eq!(
                serde_json::from_str::<FileDisposition>(&encoded).unwrap(),
                disposition
            );
        }
    }
}
