use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Incremental generation cache: maps contract file path → metadata.
#[derive(Default, Serialize, Deserialize)]
pub struct GenCache {
    pub entries: HashMap<String, GenCacheEntry>,
}

#[derive(Serialize, Deserialize)]
pub struct GenCacheEntry {
    pub source_mtime: u64,
    pub source_path: String,
}

/// Intent classification for natural-language queries.
#[derive(Debug)]
pub enum QueryIntent {
    Debug,    // "Why does X fail?"
    Usage,    // "How do I use X?"
    Explain,  // "What does X do?"
    Refactor, // "Refactor X"
    Impact,   // "What depends on X?"
    List,     // "list all modules", "show me all functions"
    Compare,  // "compare X and Y"
    Count,    // "how many tests", "count the functions"
    General,  // default
}

/// Severity of an OWASP finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OwaspSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl std::fmt::Display for OwaspSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OwaspSeverity::Info => write!(f, "INFO"),
            OwaspSeverity::Low => write!(f, "LOW"),
            OwaspSeverity::Medium => write!(f, "MED"),
            OwaspSeverity::High => write!(f, "HIGH"),
            OwaspSeverity::Critical => write!(f, "CRIT"),
        }
    }
}

/// A single OWASP-style finding.
pub struct OwaspFinding {
    pub owasp_id: &'static str,
    pub category: &'static str,
    pub severity: OwaspSeverity,
    pub file: PathBuf,
    pub line: usize,
    pub snippet: String,
    pub description: &'static str,
    pub remediation: &'static str,
}

/// Anchor pattern priorities for query resolution.
/// Higher values = higher priority when selecting from search results.
#[derive(Clone, Copy, Debug)]
pub enum AnchorPattern {
    Module,  // mod-* = 100
    Adr,     // adr-* = 90
    Plan,    // plan-* = 80
    UseCase, // use-case-* = 70
    Agent,   // agent-* = 60
    Readme,  // readme = 10
    Generic, // default = 50
}

impl AnchorPattern {
    pub fn priority(&self) -> i32 {
        match self {
            AnchorPattern::Module => 100,
            AnchorPattern::Adr => 90,
            AnchorPattern::Plan => 80,
            AnchorPattern::UseCase => 70,
            AnchorPattern::Agent => 60,
            AnchorPattern::Generic => 50,
            AnchorPattern::Readme => 10,
        }
    }

    pub fn from_anchor(anchor: &str) -> Self {
        if anchor.starts_with("mod-") {
            AnchorPattern::Module
        } else if anchor.starts_with("adr-") {
            AnchorPattern::Adr
        } else if anchor.starts_with("plan-") {
            AnchorPattern::Plan
        } else if anchor.starts_with("use-case-") {
            AnchorPattern::UseCase
        } else if anchor.starts_with("agent-") {
            AnchorPattern::Agent
        } else if anchor == "readme" {
            AnchorPattern::Readme
        } else {
            AnchorPattern::Generic
        }
    }
}

/// Static alias map: query terms → preferred anchors.
/// Used to resolve fuzzy queries like "graph module" → "mod-aden-graph".
/// Order matters: more specific terms should be checked first.
pub fn get_anchor_aliases() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();

    // === ADEN CORE (most specific first) ===
    m.insert("graph", "mod-aden-graph");
    m.insert("graphs", "mod-aden-graph");
    m.insert("node", "mod-aden-graph");
    m.insert("edge", "mod-aden-graph");
    m.insert("asm", "mod-aden-asm");
    m.insert("assembly", "mod-aden-asm");
    m.insert("assemble", "mod-aden-asm");
    m.insert("assembler", "mod-aden-asm");
    m.insert("context", "mod-aden-asm");
    m.insert("tokens", "mod-aden-asm");
    m.insert("budget", "mod-aden-asm");
    m.insert("prompt", "mod-aden-asm");
    m.insert("parse", "mod-aden-parse");
    m.insert("parsing", "mod-aden-parse");
    m.insert("parser", "mod-aden-parse");
    m.insert("emit", "mod-aden-emit");
    m.insert("emitter", "mod-aden-emit");
    m.insert("render", "mod-aden-emit");
    m.insert("rendering", "mod-aden-emit");
    m.insert("output", "mod-aden-emit");
    m.insert("index", "mod-aden-index");
    m.insert("search", "mod-aden-index");
    m.insert("query", "mod-aden-index");
    m.insert("core", "mod-aden-core");
    m.insert("schema", "mod-aden-core");
    m.insert("document", "mod-aden-core");
    m.insert("contract", "mod-aden-core");
    m.insert("anchor", "mod-aden-core");
    m.insert("reference", "mod-aden-core");
    m.insert("heal", "mod-aden-heal");
    m.insert("drift", "mod-aden-heal");
    m.insert("propose", "mod-aden-propose");
    m.insert("patch", "mod-aden-propose");
    m.insert("mcp", "mod-aden-mcp");
    m.insert("lsp", "mod-aden-lsp");
    m.insert("cli", "mod-aden-cli");
    m.insert("command", "mod-aden-cli");
    m.insert("py", "mod-aden-py");
    m.insert("python", "mod-aden-py");
    m.insert("policy", "mod-aden-policy");
    m.insert("directive", "mod-aden-policy");
    m.insert("governance", "mod-aden-policy");
    m.insert("telemetry", "mod-aden-telemetry");
    m.insert("trace", "mod-aden-telemetry");
    m.insert("metrics", "mod-aden-telemetry");

    // === DETERMINISMS & SEMANTIC ===
    m.insert("determinism", "determinisms");
    m.insert("determinisms", "determinisms");
    m.insert("semantic", "determinisms");
    m.insert("boolean", "determinisms");
    m.insert("true", "determinisms");
    m.insert("false", "determinisms");
    m.insert("time", "determinisms");
    m.insert("midnight", "determinisms");
    m.insert("noon", "determinisms");
    m.insert("number", "determinisms");
    m.insert("numbers", "determinisms");
    m.insert("month", "determinisms");
    m.insert("months", "determinisms");
    m.insert("may", "determinisms");
    m.insert("january", "determinisms");

    // === MODULE ALIASES (exact module names) ===
    m.insert("module-aden-index", "aden://module/aden-index/lib.rs#tokenize");
    m.insert("module-aden-graph", "aden://module/aden-graph/lib.rs#graph");
    m.insert("module-aden-core", "aden://module/aden-core/lib.rs#Document");
    m.insert("module-aden-parse", "aden://module/aden-parse/lib.rs#parse_file");
    m.insert("module-aden-cli", "aden://module/aden-cli/main.rs#main");

    // === ADEN ACTIONS ===
    m.insert("lint", "mod-aden-cli");
    m.insert("linting", "mod-aden-cli");
    m.insert("generate", "mod-aden-cli");
    m.insert("generation", "mod-aden-cli");
    m.insert("gen", "mod-aden-cli");
    m.insert("check", "mod-aden-cli");
    m.insert("validate", "mod-aden-cli");
    m.insert("validation", "mod-aden-cli");
    m.insert("audit", "mod-aden-cli");
    m.insert("security", "mod-aden-policy");

    // === GENERIC SOFTWARE TERMS ===

    // Parsing & Serialization
    m.insert("serialize", "mod-aden-emit");
    m.insert("serialization", "mod-aden-emit");
    m.insert("deserialize", "mod-aden-emit");
    m.insert("encode", "mod-aden-emit");
    m.insert("decode", "mod-aden-emit");
    m.insert("ast", "mod-aden-parse");
    m.insert("syntax", "mod-aden-parse");
    m.insert("tokenize", "mod-aden-parse");
    m.insert("tokenizer", "mod-aden-parse");

    // Data & Types
    m.insert("model", "mod-aden-core");
    m.insert("models", "mod-aden-core");
    m.insert("type", "mod-aden-core");
    m.insert("types", "mod-aden-core");
    m.insert("struct", "mod-aden-core");
    m.insert("structs", "mod-aden-core");
    m.insert("enum", "mod-aden-core");
    m.insert("enums", "mod-aden-core");
    m.insert("trait", "mod-aden-core");
    m.insert("interface", "mod-aden-core");
    m.insert("class", "mod-aden-core");
    m.insert("database", "mod-aden-graph"); // graph often handles persistence
    m.insert("db", "mod-aden-graph");
    m.insert("persistence", "mod-aden-graph");
    m.insert("cache", "mod-aden-graph");
    m.insert("caching", "mod-aden-graph");
    m.insert("storage", "mod-aden-graph");

    // HTTP & Networking
    m.insert("http", "mod-aden-cli");
    m.insert("https", "mod-aden-cli");
    m.insert("request", "mod-aden-cli");
    m.insert("response", "mod-aden-cli");
    m.insert("rest", "mod-aden-cli");
    m.insert("api", "mod-aden-cli");
    m.insert("endpoint", "mod-aden-cli");
    m.insert("route", "mod-aden-cli");
    m.insert("router", "mod-aden-cli");
    m.insert("server", "mod-aden-cli");
    m.insert("client", "mod-aden-cli");

    // Testing
    m.insert("test", "mod-aden-cli");
    m.insert("testing", "mod-aden-cli");
    m.insert("spec", "mod-aden-cli");
    m.insert("specs", "mod-aden-cli");
    m.insert("mock", "mod-aden-cli");
    m.insert("stub", "mod-aden-cli");
    m.insert("fixture", "mod-aden-cli");

    // Authentication & Security
    m.insert("auth", "mod-aden-policy");
    m.insert("authentication", "mod-aden-policy");
    m.insert("login", "mod-aden-policy");
    m.insert("permission", "mod-aden-policy");
    m.insert("permissions", "mod-aden-policy");
    m.insert("authorization", "mod-aden-policy");
    m.insert("role", "mod-aden-policy");
    m.insert("roles", "mod-aden-policy");
    m.insert("access", "mod-aden-policy");
    m.insert("encrypt", "mod-aden-policy");
    m.insert("encryption", "mod-aden-policy");
    m.insert("decrypt", "mod-aden-policy");
    m.insert("hash", "mod-aden-policy");
    m.insert("hashing", "mod-aden-policy");
    m.insert("secret", "mod-aden-policy");
    m.insert("secrets", "mod-aden-policy");
    m.insert("token", "mod-aden-policy");
    m.insert("tokens", "mod-aden-policy");

    // Errors & Handling
    m.insert("error", "mod-aden-core");
    m.insert("errors", "mod-aden-core");
    m.insert("exception", "mod-aden-core");
    m.insert("exception", "mod-aden-core");
    m.insert("fail", "mod-aden-core");
    m.insert("failure", "mod-aden-core");
    m.insert("panic", "mod-aden-core");
    m.insert("crash", "mod-aden-core");
    m.insert("debug", "mod-aden-cli");
    m.insert("debugging", "mod-aden-cli");
    m.insert("trace", "mod-aden-core");

    // Configuration
    m.insert("config", "mod-aden-cli");
    m.insert("configuration", "mod-aden-cli");
    m.insert("settings", "mod-aden-cli");
    m.insert("option", "mod-aden-cli");
    m.insert("options", "mod-aden-cli");
    m.insert("flag", "mod-aden-cli");
    m.insert("flags", "mod-aden-cli");
    m.insert("env", "mod-aden-cli");
    m.insert("environment", "mod-aden-cli");

    // Async & Concurrency
    m.insert("async", "mod-aden-core");
    m.insert("await", "mod-aden-core");
    m.insert("thread", "mod-aden-core");
    m.insert("threads", "mod-aden-core");
    m.insert("parallel", "mod-aden-core");
    m.insert("parallelism", "mod-aden-core");
    m.insert("concurrent", "mod-aden-core");
    m.insert("concurrency", "mod-aden-core");
    m.insert("future", "mod-aden-core");
    m.insert("futures", "mod-aden-core");
    m.insert("promise", "mod-aden-core");
    m.insert("channel", "mod-aden-core");

    // Performance
    m.insert("performance", "mod-aden-core");
    m.insert("optimize", "mod-aden-core");
    m.insert("optimization", "mod-aden-core");
    m.insert("benchmark", "mod-aden-cli");
    m.insert("bench", "mod-aden-cli");
    m.insert("profiling", "mod-aden-cli");
    m.insert("profile", "mod-aden-cli");
    m.insert("memory", "mod-aden-core");
    m.insert("leak", "mod-aden-core");
    m.insert("gc", "mod-aden-core");
    m.insert("garbage", "mod-aden-core");

    // Common Actions
    m.insert("create", "mod-aden-core");
    m.insert("new", "mod-aden-core");
    m.insert("init", "mod-aden-cli");
    m.insert("initialize", "mod-aden-cli");
    m.insert("setup", "mod-aden-cli");
    m.insert("build", "mod-aden-cli");
    m.insert("compile", "mod-aden-cli");
    m.insert("install", "mod-aden-cli");
    m.insert("run", "mod-aden-cli");
    m.insert("execute", "mod-aden-cli");
    m.insert("start", "mod-aden-cli");
    m.insert("stop", "mod-aden-cli");
    m.insert("update", "mod-aden-core");
    m.insert("modify", "mod-aden-core");
    m.insert("change", "mod-aden-core");
    m.insert("patch", "mod-aden-propose");
    m.insert("delete", "mod-aden-core");
    m.insert("remove", "mod-aden-core");
    m.insert("read", "mod-aden-index");
    m.insert("write", "mod-aden-emit");
    m.insert("load", "mod-aden-graph");
    m.insert("save", "mod-aden-graph");
    m.insert("fetch", "mod-aden-cli");

    // State Management
    m.insert("state", "mod-aden-core");
    m.insert("store", "mod-aden-graph");
    m.insert("redux", "mod-aden-core");
    m.insert("flux", "mod-aden-core");

    // Logging & Monitoring
    m.insert("log", "mod-aden-telemetry");
    m.insert("logging", "mod-aden-telemetry");
    m.insert("logger", "mod-aden-telemetry");
    m.insert("monitor", "mod-aden-telemetry");
    m.insert("monitoring", "mod-aden-telemetry");
    m.insert("alert", "mod-aden-telemetry");
    m.insert("alerts", "mod-aden-telemetry");

    // Documentation & Contracts
    m.insert("doc", "mod-aden-emit");
    m.insert("docs", "mod-aden-emit");
    m.insert("documentation", "mod-aden-emit");
    m.insert("contract", "mod-aden-core");
    m.insert("contracts", "mod-aden-core");
    m.insert("readme", "mod-aden-core");
    m.insert("guide", "mod-aden-core");
    m.insert("tutorial", "mod-aden-core");
    m.insert("example", "mod-aden-core");
    m.insert("examples", "mod-aden-core");

    // AI & LLM
    m.insert("ai", "mod-aden-asm");
    m.insert("llm", "mod-aden-asm");
    m.insert("gpt", "mod-aden-asm");
    m.insert("model", "mod-aden-asm");
    m.insert("completion", "mod-aden-asm");
    m.insert("embedding", "mod-aden-index");

    // Federation & Multi-repo
    m.insert("federation", "mod-aden-cli");
    m.insert("multi-repo", "mod-aden-cli");
    m.insert("monorepo", "mod-aden-cli");
    m.insert("repository", "mod-aden-cli");
    m.insert("repo", "mod-aden-cli");

    m
}
