// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

mod commands;
mod mcp;
mod types;
mod util;

use crate::commands::complete::cmd_complete;
use crate::commands::query::AsmOptions;
use crate::util::find_project_root;

use clap::{Parser, Subcommand, ValueHint};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "aden",
    version = env!("CARGO_PKG_VERSION"),
    // `--version` prints the source offer required by AGPL-3.0 §13: any
    // network-accessible instance (e.g. `aden mcp`) must offer its Corresponding
    // Source. `-V` still prints the bare version.
    long_version = concat!(
        env!("CARGO_PKG_VERSION"),
        "\nLicense: AGPL-3.0-or-later",
        "\nSource:  https://github.com/RioPlay/aden",
        "\nThis is free software with ABSOLUTELY NO WARRANTY. The Corresponding",
        "\nSource for any network-accessible instance is the repository above,",
        "\nas required by AGPL-3.0 section 13."
    ),
    about = "Aden — A Dense Referential Context Compiler"
)]
struct Cli {
    #[arg(long, global = true, help = "Remove all limits (show full results)")]
    unlimited: bool,
    #[arg(short, long, global = true, help = "Output JSON where supported")]
    json: bool,
    #[arg(short, long, global = true, help = "Verbose output")]
    verbose: bool,
    #[arg(
        short = 'p',
        long,
        global = true,
        value_name = "PATH",
        help = "Aden project path (overrides CWD for this run)"
    )]
    project: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scaffold .agent/ templates in target repository
    Init {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        /// Also scaffold secure-coding reference stubs (CWE Top 25 + OWASP SCP
        /// index, with citations) into .agent/secure-coding-refs/
        #[arg(long)]
        with_secure_refs: bool,
        /// Seed a short, append-only aden usage block into the repo-root
        /// AGENTS.md so AI agents use aden by default (ADR-004). Idempotent;
        /// only ever touches its own marked block. Remove the block to opt out.
        #[arg(long)]
        agents_md: bool,
    },
    /// Seed/refresh the append-only aden usage block in a repo-root AGENTS.md,
    /// without the full `init` scaffolding (ADR-004). Idempotent; only ever
    /// touches its own marked block. Remove the block to opt out.
    AgentsMd {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Create a new project from a language template with aden scaffolding
    New {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(long, value_name = "LANG", default_value = "rust")]
        lang: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Author an intent overlay for a symbol: durable [human]/[agent] notes that
    /// survive `aden gen` and are delivered to readers (asm/ask).
    Overlay {
        #[arg(value_name = "ANCHOR", help = "Anchor or bare symbol name to annotate")]
        anchor: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Create a kickoff document for a new initiative (interactive or from a brief)
    Kickoff {
        #[arg(long, value_name = "NAME")]
        name: String,
        #[arg(long, help = "Interactive wizard mode")]
        interactive: bool,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Workflow engine: instantiate templates with substitutions and chain documents
    Workflow {
        #[arg(
            value_name = "TEMPLATE",
            help = "Template to instantiate: kickoff, design, spec, task, adr"
        )]
        template: String,
        #[arg(long, value_name = "FILE", help = "Source document to derive from")]
        from: Option<String>,
        #[arg(long, value_name = "FILE", help = "Output path")]
        out: Option<PathBuf>,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Recompile the whole project into the per-user project store (see `aden store
    /// path`) from scratch: clears the gen/graph caches first, then regenerates. The
    /// full-rebuild counterpart to the incremental `aden gen .` (not a transparent
    /// alias — it always re-stores all).
    Regen {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Compile source into the per-user project store (see `aden store path`). A
    /// directory indexes the whole project; a single file re-indexes just that file.
    /// Store-first: `gen` writes only to that store, never to the working tree.
    Gen {
        #[arg(value_name = "PATH", value_hint = ValueHint::AnyPath)]
        paths: Vec<PathBuf>,
        #[arg(
            long,
            help = "Auto-discover source files for the whole project (default when PATH is a directory)"
        )]
        auto: bool,
        #[arg(long, help = "Suppress per-file output (summary only)")]
        quiet: bool,
        #[arg(
            long,
            help = "Dry-run: reconcile and write conflict proposals without changing the store"
        )]
        propose: bool,
        #[arg(
            long,
            help = "Bypass the merge gate and overwrite the store (may clobber [human]/[agent] overlay collisions)"
        )]
        force_regen: bool,
    },
    /// Verify all <<refs>> resolve to existing [[anchors]]
    Check {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
        #[arg(
            long,
            value_name = "SEVERITY",
            default_value = "Warn",
            help = "Minimum severity to fail: Suggest, Warn, Forbid"
        )]
        severity: String,
    },
    /// Complete incomplete contracts by filling in required documentation
    Complete {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
        #[arg(
            long,
            help = "Dry-run mode (don't actually complete)",
            default_value = "false"
        )]
        dry_run: bool,
        #[arg(
            long,
            value_name = "MODEL",
            help = "LLM model to use (e.g., ollama:llama3)"
        )]
        model: Option<String>,
    },
    /// Lint source files using fast, language-agnostic line-based heuristics
    Lint {
        #[arg(value_name = "PATH", default_value = ".", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
        #[arg(
            long,
            value_name = "SEVERITY",
            default_value = "Warn",
            help = "Minimum severity to report: Suggest, Warn, Error"
        )]
        severity: String,
        #[arg(long, help = "Fix issues where possible")]
        fix: bool,
        #[arg(long, help = "Output JSON format")]
        json: bool,
        #[arg(
            long,
            help = "Flag potentially dead code (symbols with no incoming graph edges)"
        )]
        dead_code: bool,
        #[arg(long, help = "Include public API / entry points in dead-code analysis")]
        include_public: bool,
    },
    /// Discover and run tests across all languages
    Test {
        #[arg(value_name = "PATH", default_value = ".", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
        #[arg(
            long,
            value_name = "SCOPE",
            help = "Test scope: unit, integration, all"
        )]
        scope: Option<String>,
        #[arg(long, help = "Run only these tests (filter)")]
        filter: Option<String>,
        #[arg(long, help = "Don't run tests, only list them")]
        list: bool,
    },
    /// Assemble a context prompt from the knowledge graph
    Asm {
        #[arg(long, value_name = "ANCHOR")]
        from: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(long, value_name = "N", default_value = "3")]
        depth: usize,
        #[arg(long, value_name = "TOKENS", default_value = "8192")]
        budget: usize,
        #[arg(long, value_name = "TYPES")]
        edge_types: Option<String>,
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        #[arg(
            long,
            value_name = "FORMAT",
            default_value = "llm",
            help = "Output format: llm (default, stripped prose for LLMs), adg (compact JSON), aden (raw AsciiDoc)"
        )]
        format: String,
        #[arg(long, help = "Silent mode: skip intro, output only context")]
        silent: bool,
        #[arg(long, help = "Auto mode: adjust budget based on relevance scores")]
        auto: bool,
        #[arg(long, help = "Strict mode: never exceed budget (disable auto-boost)")]
        strict: bool,
        #[arg(long, help = "Inspect: show what would be included without outputting")]
        inspect: bool,
        #[arg(
            long,
            value_name = "TAG",
            help = "Include only content with this tag (can repeat)"
        )]
        include_tag: Vec<String>,
        #[arg(
            long,
            value_name = "TAG",
            help = "Exclude content with this tag (can repeat)"
        )]
        exclude_tag: Vec<String>,
        #[arg(
            long,
            value_name = "ATTR",
            help = "Set attribute for conditional processing (can repeat)"
        )]
        set_attr: Vec<String>,
    },
    /// Query the knowledge graph and emit JSON
    Query {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
        #[arg(long, value_name = "ANCHOR")]
        from: Option<String>,
        #[arg(long, value_name = "TYPE")]
        edge_type: Option<String>,
        #[arg(long, value_name = "N", default_value = "3")]
        depth: usize,
        #[arg(long, value_name = "ANCHOR")]
        backlinks: Option<String>,
        #[arg(long, value_name = "ANCHOR")]
        impact: Option<String>,
        #[arg(
            long,
            value_name = "FORMAT",
            default_value = "json",
            help = "Output format: json, table"
        )]
        format: String,
    },
    /// Execute an Aden Query (.adq) script
    QueryAdq {
        #[arg(
            value_name = "SCRIPT",
            help = "ADQ script: node(anchor), incoming(anchor), outgoing(anchor), where anchor:term"
        )]
        script: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Ask a natural-language question; Aden resolves it to a subgraph and assembles context.
    Ask {
        #[arg(value_name = "QUESTION")]
        question: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(long, value_name = "ANCHOR")]
        from: Option<String>,
        #[arg(long, value_name = "TOKENS", default_value = "4096")]
        budget: usize,
        #[arg(
            long,
            value_name = "MODEL",
            help = "LLM model: ollama:<name>, openai:<name>, or auto"
        )]
        model: Option<String>,
        #[arg(
            long,
            value_name = "INTENT",
            help = "Override intent classification: debug, usage, explain, refactor, impact, list, compare, count, general"
        )]
        intent: Option<String>,
        #[arg(long, value_name = "N", help = "Override the intent's traversal depth")]
        depth: Option<usize>,
        #[arg(
            long,
            value_name = "TYPES",
            help = "Override the intent's edge types (comma-separated)"
        )]
        edge_types: Option<String>,
        #[arg(
            long,
            help = "Strict mode: use --budget as an exact cap (disable the relevance boost)"
        )]
        strict: bool,
        #[arg(
            long,
            help = "Explain routing: top candidates with scores/patterns, the tiebreak decision, intent, overview signal, and any fallback swap"
        )]
        explain: bool,
    },
    /// Search the knowledge graph for documents matching a query
    Search {
        #[arg(value_name = "QUERY")]
        query: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(
            long,
            value_name = "N",
            default_value = "50",
            help = "Limit number of results"
        )]
        limit: usize,
        #[arg(
            long,
            value_name = "N",
            default_value = "0",
            help = "Offset for pagination"
        )]
        offset: usize,
        #[arg(
            long,
            value_name = "TYPE",
            help = "Filter by document type: module, adr, plan, use-case"
        )]
        doc_type: Option<String>,
        #[arg(long, help = "Also include semantic relationship results")]
        semantics: bool,
    },
    /// List all anchors and contracts in the knowledge graph (alias: ls)
    List {
        #[arg(
            long,
            value_name = "PATTERN",
            help = "Filter by pattern (e.g., 'mod-aden-*')"
        )]
        filter: Option<String>,
        #[arg(long, help = "Show detailed information for each anchor")]
        verbose: bool,
        #[arg(long, help = "Show only semantic concept nodes")]
        semantics: bool,
        #[arg(
            long,
            value_name = "N",
            default_value = "50",
            help = "Limit number of results"
        )]
        limit: usize,
        #[arg(
            long,
            value_name = "N",
            default_value = "0",
            help = "Offset for pagination"
        )]
        offset: usize,
        #[arg(long, help = "Show all results (no limit)")]
        unlimited: bool,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Locate a symbol definition or its call sites in the knowledge graph
    /// Structure-aware content search: find a pattern and tag each hit with the
    /// symbol it lives inside (a graph-aware replacement for grep).
    Grep {
        #[arg(
            value_name = "PATTERN",
            help = "Text (or regex with --regex) to search for"
        )]
        pattern: String,
        #[arg(long, help = "Treat PATTERN as a regular expression")]
        regex: bool,
        #[arg(short = 'i', long, help = "Case-insensitive match")]
        ignore_case: bool,
        #[arg(long, help = "Only report matches that fall inside a known symbol")]
        symbol_only: bool,
        #[arg(
            long,
            value_name = "N",
            default_value = "100",
            help = "Limit number of results"
        )]
        limit: usize,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Detect functional communities (clusters of densely-connected symbols)
    Communities {
        #[arg(
            long,
            value_name = "N",
            default_value = "2",
            help = "Only show communities with at least N members (1 includes singletons)"
        )]
        min_size: usize,
        #[arg(
            long,
            value_name = "N",
            default_value = "30",
            help = "Limit number of communities shown"
        )]
        limit: usize,
        #[arg(
            long,
            value_name = "G",
            default_value = "1.0",
            help = "Resolution (>1.0 = more, smaller clusters; 1.0 = standard modularity)"
        )]
        resolution: f64,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Export a graph slice as a text diagram (Mermaid/DOT/JSON) for docs, PRs, or CI.
    Viz {
        #[arg(
            value_name = "ANCHOR",
            help = "Symbol to center blast/reach/connectivity on (name or full aden:// anchor; omit for --mode communities)"
        )]
        anchor: Option<String>,
        #[arg(
            long,
            value_name = "MODE",
            default_value = "blast",
            help = "View: blast (dependents at risk if the anchor changes) | reach (dependencies it relies on) | connectivity (both directions) | communities (clusters)"
        )]
        mode: String,
        #[arg(
            long,
            value_name = "N",
            default_value = "2",
            help = "Max hops to include (blast/connectivity)"
        )]
        depth: usize,
        #[arg(
            long,
            value_name = "FMT",
            default_value = "mermaid",
            help = "Output format: mermaid | dot | asciidoc | json (or use -j for json)"
        )]
        format: String,
        #[arg(
            long,
            help = "For --mode graph: emit the WHOLE graph (no importance cap)"
        )]
        full: bool,
        #[arg(
            long,
            value_name = "SUBDIR",
            help = "Restrict to sources under this project-relative path (e.g. net/ on the kernel) — slices and community detection run on the subtree's subgraph"
        )]
        scope: Option<String>,
        #[arg(
            long,
            value_name = "GAMMA",
            default_value = "1.0",
            help = "Community-detection resolution γ (higher = finer clusters; try 2.0–5.0 on very large graphs)"
        )]
        resolution: f64,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Render a graph slice in the browser — interactive, offline, with git-history replay.
    #[cfg(feature = "view")]
    View {
        #[arg(
            value_name = "ANCHOR",
            help = "Symbol to center on (name or full aden:// anchor; omit for --mode communities)"
        )]
        anchor: Option<String>,
        #[arg(
            long,
            value_name = "MODE",
            default_value = "blast",
            help = "View: blast (downstream impact) | connectivity (both directions) | communities"
        )]
        mode: String,
        #[arg(
            long,
            value_name = "N",
            default_value = "2",
            help = "Max hops to include (blast/connectivity)"
        )]
        depth: usize,
        #[arg(
            long = "3d",
            help = "Orbital 3D view — a slow-rotating spatial picture of the project (2D is the analytical view: lenses, replay, filters)"
        )]
        three_d: bool,
        #[arg(long = "no-open", help = "Write the HTML but do not open a browser")]
        no_open: bool,
        #[arg(
            long,
            value_name = "ED",
            default_value = "auto",
            help = "Editor for 'open in editor' links: auto (detect installed) | vscode | cursor | vscodium | zed | idea | <uri-template with {file}/{line}>"
        )]
        editor: String,
        #[arg(
            long,
            help = "Replay git history — watch the project populate commit-by-commit"
        )]
        replay: bool,
        #[arg(
            long,
            value_name = "N",
            default_value = "0",
            help = "Replay: max commits to include, oldest→newest (0 = entire history)"
        )]
        max: usize,
        #[arg(
            long,
            value_name = "FILE",
            value_hint = ValueHint::FilePath,
            help = "Output HTML path (default: a temp file named after the project)"
        )]
        out: Option<PathBuf>,
        #[arg(
            long,
            value_name = "SUBDIR",
            help = "Restrict to sources under this project-relative path — the viewer shows the subtree's subgraph"
        )]
        scope: Option<String>,
        #[arg(
            long,
            value_name = "GAMMA",
            default_value = "1.0",
            help = "Community-detection resolution γ (higher = finer clusters on very large graphs)"
        )]
        resolution: f64,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Map a git diff to the symbols it touches and report the blast radius
    ImpactDiff {
        #[arg(
            long,
            value_name = "REV",
            help = "Diff against this git ref (e.g. HEAD~1, main) instead of the working tree"
        )]
        since: Option<String>,
        #[arg(long, help = "Analyze staged changes (git diff --cached)")]
        staged: bool,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    Locate {
        #[arg(long, value_name = "SYMBOL", help = "Find definition of this symbol")]
        symbol: Option<String>,
        #[arg(long, value_name = "SYMBOL", help = "Find call sites of this symbol")]
        caller_of: Option<String>,
        #[arg(long, value_name = "FORMAT", default_value = "plain")]
        format: String,
        #[arg(
            long,
            value_name = "N",
            help = "Include N lines of context around symbol"
        )]
        show_context: Option<usize>,
        #[arg(
            long,
            value_name = "N",
            default_value = "50",
            help = "Limit number of results"
        )]
        limit: usize,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Show project health status at a glance
    Status {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Run gen + check + heal (with gc) in sequence to sync everything
    Sync {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(
            long,
            help = "Skip garbage-collection of deleted symbols/files from the store"
        )]
        no_gc: bool,
    },
    /// Watch source files for changes and auto-regenerate contracts
    #[cfg(feature = "watch")]
    Watch {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(
            long,
            help = "Also sync graph in real-time (contracts + graph stay current)"
        )]
        graph_sync: bool,
        #[arg(long, help = "Restore graph from cache on startup for faster sync")]
        restore: bool,
        #[arg(long, help = "Unified sync: run gen + check + heal on each change")]
        sync: bool,
    },
    /// Self-healing documentation engine: scan for drift, propose patches, apply reviewed changes
    Heal {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(long, help = "Generate patch proposals for review")]
        propose: bool,
        #[arg(
            long,
            help = "Auto-fix high-confidence drift: rewrites SignatureMismatch in the store and legacy on-disk StaleHash. Store-resident StaleHash and MissingContract are reported and deferred to `aden gen`"
        )]
        fix: bool,
        #[arg(
            long,
            help = "Garbage collect orphaned contracts (contracts without matching source)"
        )]
        gc: bool,
        #[arg(
            long,
            value_name = "REF",
            help = "Limit scan to files changed since git ref"
        )]
        since: Option<String>,
        #[arg(long, value_name = "ID", help = "Apply a specific proposal by ID")]
        apply: Option<String>,
        #[cfg(feature = "watch")]
        #[arg(
            long,
            value_name = "DIR",
            help = "Watch directory and auto-heal on changes"
        )]
        watch: Option<PathBuf>,
    },
    /// Fast pre-commit combo: gen + lint + check + heal drift scan + audit
    Ready {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(
            long,
            help = "Apply auto-fixes where possible (lint + high-confidence heal)"
        )]
        fix: bool,
    },
    /// One-shot symbol comprehension: definition, backlinks, impact, and assembled context
    Understand {
        /// Symbol name to understand
        symbol: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        /// Token budget for the assembled context block
        #[arg(long, value_name = "TOKENS", default_value = "4000")]
        budget: usize,
        /// Emit a single JSON object instead of a human report
        #[arg(long)]
        json: bool,
    },
    /// Run all local CI gates before committing (check, heal, test, secret-scan)
    CiCheck {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Diagnose the environment: tool versions, repo health, signing keys
    Doctor {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Semantic review: validate low-confidence proposals with token budgeting
    Review {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::AnyPath)]
        path: PathBuf,
        #[arg(
            long,
            value_name = "REF",
            help = "Review only files changed since git ref"
        )]
        since: Option<String>,
        #[arg(long, value_name = "TOKENS", default_value = "2048")]
        budget: usize,
    },
    /// Atomic session lock: append entry to .agent/session.adoc
    Session {
        #[arg(long, value_name = "ID")]
        agent_id: String,
        #[arg(long, value_name = "DESC")]
        task: String,
        #[arg(long, value_name = "FILES")]
        files: Option<String>,
        #[arg(long, value_name = "STATUS")]
        status: Option<String>,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// Generate third-party accreditation report (Cargo, npm, PyPI, Go)
    Licenses {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(
            long,
            value_name = "FILE",
            help = "Write output to file instead of stdout"
        )]
        out: Option<PathBuf>,
        #[arg(
            long,
            help = "Resolve licenses (local install first, registry fallback) and group by license"
        )]
        full: bool,
    },
    /// Multi-repository workspace management
    Federation {
        #[command(subcommand)]
        action: FederationAction,
    },
    /// OWASP-aligned security audit: scan source for vulnerabilities
    Audit {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
        #[arg(
            long,
            value_name = "LANG",
            help = "Filter to a specific language (rust, python, go, ts, php). Default: auto-detect all"
        )]
        lang: Option<String>,
        #[arg(
            long,
            value_name = "FORMAT",
            default_value = "text",
            help = "Output format: text, json, adoc"
        )]
        format: String,
        #[arg(long, help = "Exit non-zero on any finding (default: warnings only)")]
        strict: bool,
    },
    /// MCP (Model Context Protocol) integration management
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Manage per-user, per-project graph stores (ADR-003)
    Store {
        #[command(subcommand)]
        action: StoreAction,
    },
    /// Emergency override: downgrade global Forbid to Warn, auto-expires
    Emergency {
        #[arg(
            long,
            value_name = "REASON",
            help = "Justification for emergency override"
        )]
        reason: String,
        #[arg(
            long,
            value_name = "TTL",
            default_value = "24h",
            help = "Time-to-live: 1h, 24h, 7d"
        )]
        ttl: String,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// AI assistant: describe what you want to do, get the right aden command
    Suggest {
        #[arg(
            value_name = "INTENT",
            help = "What you want to do (e.g., 'generate docs for my code', 'find how X works')"
        )]
        intent: String,
    },
    /// Deterministic diagnostic scanner for knowledge graphs
    Diagnose {
        // Positional DIR for consistency with check/lint/audit/doctor (previously
        // the lone command that required `--path`). `aden diagnose .` now works.
        #[arg(
            value_name = "DIR",
            default_value = ".",
            value_hint = ValueHint::DirPath,
            help = "Directory to scan"
        )]
        path: PathBuf,
        #[arg(
            long,
            value_name = "FORMAT",
            default_value = "text",
            help = "Output format: text, json"
        )]
        format: String,
    },
    /// Time-travel file viewer: bake every historical version of a file into
    /// a self-contained HTML page with client-side comparison.
    #[cfg(feature = "view")]
    Timeline {
        /// File path, repo-relative path, or aden:// anchor URI
        #[arg(value_name = "PATH")]
        path_arg: String,
        /// Oldest ref to include (git ref or hash)
        #[arg(long, value_name = "REF")]
        from: Option<String>,
        /// Newest ref to include (git ref or hash)
        #[arg(long, value_name = "REF")]
        to: Option<String>,
        /// Show a single version at this ref
        #[arg(long, value_name = "REF")]
        at: Option<String>,
        /// Max versions to bake (default: 60; 0 = all)
        #[arg(long, value_name = "N", default_value = "60")]
        max: usize,
        /// Output HTML path (default: temp file)
        #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
        out: Option<PathBuf>,
        /// Write the HTML but do not open a browser
        #[arg(long = "no-open")]
        no_open: bool,
    },
}

#[derive(Subcommand)]
enum McpAction {
    /// Install aden MCP into supported AI platforms
    Install {
        #[arg(
            long,
            value_name = "PLATFORM",
            help = "Target platform: opencode, claude, cursor, codex, zed, windsurf"
        )]
        platform: Option<String>,
        #[arg(long, value_name = "PATH", help = "Path to aden-mcp binary")]
        binary: Option<PathBuf>,
        #[arg(long, value_name = "PATH", help = "Project directory to serve")]
        project: Option<PathBuf>,
        #[arg(
            long,
            value_name = "SCOPE",
            help = "Install scope: user (global) or project (local). Default: user for Claude Code, project otherwise"
        )]
        scope: Option<String>,
        #[arg(long, help = "Install for all platforms, not just detected ones")]
        all: bool,
        #[arg(long, help = "Show what would be done without writing files")]
        dry_run: bool,
    },
    /// Remove aden MCP from supported AI platforms
    Uninstall {
        #[arg(long, value_name = "PLATFORM", help = "Target platform")]
        platform: Option<String>,
        #[arg(
            long,
            value_name = "SCOPE",
            help = "Uninstall scope: user (global) or project (local). Default: user for Claude Code, project otherwise"
        )]
        scope: Option<String>,
        #[arg(long, help = "Remove from all supported platforms")]
        all: bool,
        #[arg(long, help = "Show what would be done without writing files")]
        dry_run: bool,
    },
    /// List supported platforms and their status
    List,
    /// Start HTTP server for CI/agent integration
    Serve {
        #[arg(long, value_name = "PORT", default_value = "3030")]
        port: u16,
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum StoreAction {
    /// Print the resolved store path for the current project
    Path {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
    /// List all per-user project stores and their real roots
    List,
    /// Remove stores whose project root no longer exists on disk
    Prune {
        #[arg(long, help = "Show what would be removed without deleting")]
        dry_run: bool,
    },
    /// Move a legacy in-tree store (.aden/store) to the central location
    Migrate {
        #[arg(value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum FederationAction {
    /// List repositories in the workspace
    List,
    /// Add a repository to the workspace
    Add {
        #[arg(value_name = "PATH")]
        path: PathBuf,
        #[arg(long, value_name = "NAME", help = "Friendly name for the repository")]
        name: Option<String>,
    },
    /// Remove a repository from the workspace
    Remove {
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Show workspace configuration
    Config,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Run all CLI work on a worker thread with a large stack. The OS main-thread
    // stack is ~1 MB on Windows (vs ~8 MB on Linux/macOS), which is too small for
    // clap-derive's monolithic command-tree build (the 44-variant `Commands`
    // enum) at `Cli::parse()` and for the uncapped tree-sitter AST walkers in
    // aden-parse on deeply nested sources. A 64 MB stack clears both on every
    // platform. The thread is named "main" so panic/overflow diagnostics still
    // read `thread 'main'`, and errors are printed here (Display, unquoted) to
    // match the formatting the runtime's `Termination` impl would otherwise give
    // the original `Box<dyn Error>` (which isn't `Send`, so it can't cross join).
    let child = std::thread::Builder::new()
        .name("main".into())
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            real_main().unwrap_or_else(|e| {
                eprintln!("Error: {e}");
                std::process::exit(1);
            })
        })
        .expect("failed to spawn aden worker thread");
    match child.join() {
        Ok(()) => Ok(()),
        Err(_) => std::process::exit(101), // worker thread panicked (already reported)
    }
}

fn real_main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let quiet = !cli.verbose;
    crate::util::quiet::set_quiet(quiet);
    let _unlimited = cli.unlimited;
    let _global_json = cli.json;

    // ADR-003 §6: store *creation* is explicitly authorized when -p/--project
    // was given or the command is `init`. Reads never consult this flag.
    util::set_creation_explicit(
        cli.project.is_some() || matches!(cli.command, Commands::Init { .. }),
    );

    if let Some(ref project_path) = cli.project {
        if project_path.exists() && project_path.is_dir() {
            std::env::set_current_dir(project_path)?;
            eprintln!("Switched to aden project: {}", project_path.display());
            // Persist the resolved root so future calls without --project pick it up.
            let resolved = find_project_root(project_path);
            if let Err(e) = util::write_project_conf(&resolved) {
                eprintln!("Warning: could not write project.conf: {}", e);
            }
        } else {
            eprintln!(
                "Warning: Project path does not exist: {}",
                project_path.display()
            );
        }
    }

    match cli.command {
        Commands::Init {
            path,
            with_secure_refs,
            agents_md,
        } => commands::cmd_init(&path, with_secure_refs, agents_md),
        Commands::AgentsMd { path } => commands::cmd_agents_md(&path),
        Commands::Regen { path } => {
            let root = find_project_root(&path);
            // True from-scratch rebuild. Clearing only the caches (the old
            // behavior) was self-defeating: gen's prune step diffs each file's
            // fresh anchors against the gen-cache to find symbols to remove, so
            // deleting that cache left renamed/removed anchors (e.g. a method
            // requalified by a parser change) orphaned in the store forever.
            // Wipe the whole per-user project dir — store AND caches, all
            // rebuildable per ADR-003 — so the subsequent gen repopulates a
            // pristine store with no stale anchors.
            if std::env::var_os("ADEN_STORE").is_none() {
                let project = aden_paths::project_dir(&root);
                if project.exists()
                    && let Err(e) = std::fs::remove_dir_all(&project)
                {
                    eprintln!(
                        "WARN: regen could not clear {}: {} (falling back to cache clear)",
                        project.display(),
                        e
                    );
                }
            } else {
                // A pinned/shared $ADEN_STORE holds several projects' anchors;
                // wiping it would destroy the others. Clear only this project's
                // caches and warn that stale anchors may persist there.
                eprintln!(
                    "NOTE: $ADEN_STORE is pinned/shared — regen rebuilds without clearing it; \
                     renamed or removed symbols may persist. Unset $ADEN_STORE for a full rebuild."
                );
                let gen_cache = aden_paths::gen_cache_file(&root);
                if gen_cache.exists() {
                    let _ = std::fs::remove_file(&gen_cache);
                }
                let graph_cache = aden_paths::cache_dir(&root);
                if graph_cache.exists() {
                    let _ = std::fs::remove_dir_all(&graph_cache);
                }
                // The embedding cache lives outside cache/ (it must survive a
                // normal gen); regen is the one path that should drop it so dense
                // vectors are recomputed from scratch.
                let emb_cache = aden_paths::embeddings_cache_file(&root);
                if emb_cache.exists() {
                    let _ = std::fs::remove_file(&emb_cache);
                }
            }
            commands::cmd_gen(&path, true)
        }
        Commands::Gen {
            paths,
            auto: _,
            quiet,
            propose,
            force_regen,
        } => {
            let effective_path = if paths.is_empty() {
                std::path::PathBuf::from(".")
            } else {
                paths[0].clone()
            };
            commands::cmd_gen_opts(&effective_path, quiet, propose, force_regen)
        }
        Commands::Check { path, severity } => commands::cmd_check(&path, &severity, cli.json),
        Commands::Complete {
            path,
            dry_run,
            model,
        } => cmd_complete(&path, dry_run, model.as_deref()),
        Commands::Lint {
            path,
            severity,
            fix,
            json,
            dead_code,
            include_public,
        } => commands::cmd_lint(
            &path,
            &severity,
            fix,
            json,
            dead_code,
            include_public,
            false,
        ),
        Commands::Ready { path, fix } => commands::cmd_ready(&path, fix),
        Commands::Understand {
            symbol,
            path,
            budget,
            json,
        } => commands::cmd_understand(&symbol, &path, budget, json),
        Commands::Test {
            path,
            scope,
            filter,
            list,
        } => commands::cmd_test(&path, scope.as_deref(), filter.as_deref(), list, cli.json),
        Commands::Asm {
            from,
            depth,
            budget,
            edge_types,
            out,
            path,
            format: asm_format,
            silent,
            auto,
            strict,
            inspect,
            include_tag,
            exclude_tag,
            set_attr,
        } => {
            let types = edge_types
                .map(|s| util::parse_edge_types_validated(&s))
                .transpose()?
                .unwrap_or_default();
            let (effective_format, effective_budget, effective_auto) = (asm_format, budget, auto);
            commands::cmd_asm(AsmOptions {
                path,
                from,
                depth,
                budget: effective_budget,
                edge_types: types,
                out,
                format: effective_format,
                silent,
                auto: effective_auto,
                strict,
                inspect,
                include_tags: include_tag,
                exclude_tags: exclude_tag,
                attributes: set_attr,
            })
        }
        Commands::Query {
            from,
            edge_type,
            depth,
            backlinks,
            impact,
            format,
            path,
        } => commands::cmd_query(
            &path,
            from.as_deref(),
            edge_type.as_deref(),
            depth,
            backlinks.as_deref(),
            impact.as_deref(),
            &format,
        ),
        Commands::QueryAdq { script, path } => commands::cmd_query_adq(&path, &script),
        Commands::Ask {
            question,
            from,
            budget,
            model,
            path,
            intent,
            depth,
            edge_types,
            strict,
            explain,
        } => {
            let intent_override = intent.map(|s| s.parse()).transpose()?;
            let edge_types_override = edge_types
                .map(|s| util::parse_edge_types_validated(&s))
                .transpose()?;
            commands::cmd_ask(
                &path,
                &question,
                from.as_deref(),
                budget,
                model.as_deref(),
                intent_override,
                depth,
                edge_types_override,
                strict,
                explain,
            )
        }
        Commands::Search {
            query,
            path,
            limit,
            offset,
            doc_type,
            semantics,
        } => {
            let effective_limit = if cli.unlimited { usize::MAX } else { limit };
            commands::cmd_search(
                &path,
                &query,
                effective_limit,
                offset,
                doc_type.as_deref(),
                semantics,
                cli.json,
            )
        }
        Commands::List {
            filter,
            verbose,
            semantics,
            limit,
            offset,
            unlimited,
            path,
        } => {
            let effective_limit = if unlimited || cli.unlimited {
                usize::MAX
            } else {
                limit
            };
            commands::cmd_list(
                &path,
                filter.as_deref(),
                verbose,
                effective_limit,
                offset,
                semantics,
                cli.json,
            )
        }
        Commands::Grep {
            pattern,
            regex,
            ignore_case,
            symbol_only,
            limit,
            path,
        } => {
            let effective_limit = if cli.unlimited { usize::MAX } else { limit };
            commands::cmd_grep(
                &pattern,
                &path,
                regex,
                ignore_case,
                symbol_only,
                effective_limit,
                cli.json,
            )
        }
        Commands::Communities {
            min_size,
            limit,
            resolution,
            path,
        } => {
            let effective_limit = if cli.unlimited { usize::MAX } else { limit };
            commands::cmd_communities(&path, min_size, effective_limit, resolution, cli.json)
        }
        Commands::ImpactDiff {
            since,
            staged,
            path,
        } => commands::cmd_impact_diff(&path, since.as_deref(), staged, cli.json),
        Commands::Viz {
            anchor,
            mode,
            depth,
            format,
            full,
            scope,
            resolution,
            path,
        } => commands::cmd_viz(
            &path,
            anchor.as_deref(),
            depth,
            &format,
            &mode,
            cli.json,
            full,
            scope.as_deref(),
            resolution,
        ),
        #[cfg(feature = "view")]
        Commands::View {
            anchor,
            mode,
            depth,
            three_d,
            no_open,
            out,
            scope,
            resolution,
            path,
            editor,
            replay,
            max,
        } => commands::cmd_view(
            &path,
            anchor.as_deref(),
            &mode,
            depth,
            three_d,
            !no_open,
            out.as_deref(),
            &editor,
            replay,
            max,
            scope.as_deref(),
            resolution,
        ),
        Commands::Locate {
            symbol,
            caller_of,
            format,
            show_context,
            path,
            limit,
        } => {
            let effective_limit = if cli.unlimited { usize::MAX } else { limit };
            commands::cmd_locate(
                &path,
                symbol.as_deref(),
                caller_of.as_deref(),
                &format,
                effective_limit,
                show_context,
                cli.json,
            )
        }
        #[cfg(feature = "watch")]
        Commands::Status { path } => {
            let aden_path = path.join(".aden");

            // Health is a heal-drift metric (stale docs vs. code), separate
            // from orphans. Keep it as the honest drift signal.
            let health = crate::util::quick_health_score(&path).unwrap_or(0.0);
            let health_pct = (health * 100.0).round() as i32;

            // Orphan breakdown via the SAME classifier `check` uses, so status
            // never reports expected metadata docs as scary orphans. Computed once.
            let (expected_n, actionable): (usize, Vec<String>) =
                match aden_graph::cache::build_from_directory_cached(&path) {
                    Ok(g) => {
                        let (expected, actionable) = crate::util::classify_orphans(&g);
                        (expected.len(), actionable)
                    }
                    Err(_) => (0, Vec::new()),
                };

            // Machine-readable for the global `-j/--json` flag (previously ignored).
            if cli.json {
                let env = serde_json::json!({
                    "path": path.display().to_string(),
                    "aden_dir": aden_path.display().to_string(),
                    "store": aden_paths::store_dir(&find_project_root(&path)).display().to_string(),
                    "health_score": health,
                    "health": health_pct,
                    "orphans": {
                        "expected": expected_n,
                        "actionable_count": actionable.len(),
                        "actionable": actionable,
                    },
                });
                println!("{}", serde_json::to_string_pretty(&env)?);
                return Ok(());
            }

            println!("Aden Status: {}", path.display());
            println!("Active .aden: {}", aden_path.display());
            println!(
                "Store: {}",
                aden_paths::store_dir(&find_project_root(&path)).display()
            );
            let emoji = if health >= 0.95 {
                "✅"
            } else if health >= 0.8 {
                "⚠️"
            } else {
                "❌"
            };
            println!("{} Health: {}/100", emoji, health_pct);

            if actionable.is_empty() {
                if expected_n == 0 {
                    println!("✅ No orphan documents");
                } else {
                    println!(
                        "✅ No actionable orphans ({} expected metadata doc(s), which is normal)",
                        expected_n
                    );
                }
            } else {
                println!(
                    "⚠️ {} actionable orphan document(s) (run 'aden heal . --gc' to remove if deleted)",
                    actionable.len()
                );
                if expected_n > 0 {
                    println!("   (plus {} expected metadata doc(s) — normal)", expected_n);
                }
            }

            // Savings summary from the persistent ledger: this session + all-time.
            let repo_root = find_project_root(&path);
            let summary = crate::commands::savings_store::load_summary(&repo_root);
            if summary.all_time.queries == 0 {
                println!("Savings (est.): no queries recorded yet");
            } else {
                use aden_core::savings::humanize_count;
                let line = |label: &str, l: &crate::commands::savings_store::SavingsLedger| {
                    println!(
                        "{label}: {} aden call{} → est. ~{} tool calls + ~{} tokens saved vs grep-and-read",
                        l.queries,
                        if l.queries == 1 { "" } else { "s" },
                        l.tool_calls_saved,
                        humanize_count(l.saved_tokens),
                    );
                };
                println!("Savings estimate (vs grep-and-read) [est.]:");
                line("  This session", &summary.session);
                line("  All-time    ", &summary.all_time);
            }

            Ok(())
        }
        Commands::Sync { path, no_gc } => {
            println!("Running aden sync on {}...", path.display());

            // 1. Generate contracts
            println!("\n[1/3] Generating contracts...");
            if let Err(e) = commands::cmd_gen(&path, true) {
                eprintln!("Gen error: {}", e);
            }

            // 2. Check references
            println!("\n[2/3] Checking references...");
            if let Err(e) = commands::cmd_check(&path, "Warn", false) {
                let msg = format!("{}", e);
                if !msg.contains("ERROR") {
                    println!("Check OK");
                }
            }

            // 3. Heal scan — gc by default so deleted symbols/files are pruned
            // from the store (the orphan/drift these leave behind is exactly
            // what "sync everything" is meant to converge). `--no-gc` opts out.
            let gc = !no_gc;
            if gc {
                println!("\n[3/3] Scanning for drift (with gc)...");
            } else {
                println!("\n[3/3] Scanning for drift...");
            }
            if let Err(e) = commands::cmd_heal_scan(&path, false, false, gc, cli.unlimited) {
                eprintln!("Heal error: {}", e);
            }

            println!("\nSync complete!");
            Ok(())
        }
        Commands::Watch {
            path,
            graph_sync,
            restore,
            sync,
        } => commands::cmd_watch(&path, graph_sync, restore, sync),
        Commands::Heal {
            path,
            propose,
            fix,
            gc,
            since,
            apply,
            watch,
        } => {
            if let Some(id) = apply {
                commands::cmd_heal_apply(&path, &id)
            } else if let Some(watch_path) = watch {
                #[cfg(feature = "watch")]
                {
                    commands::cmd_heal_watch(&watch_path)
                }
                #[cfg(not(feature = "watch"))]
                {
                    Err("watch feature is not enabled in this build".into())
                }
            } else if let Some(ref git_ref) = since {
                commands::cmd_heal_scan_since(&path, propose, git_ref)
            } else {
                commands::cmd_heal_scan(&path, propose, fix, gc, cli.unlimited)
            }
        }
        Commands::CiCheck { path } => commands::cmd_ci_check(&path, cli.json),
        Commands::Doctor { path } => commands::cmd_doctor(&path, cli.json),
        Commands::Review {
            path,
            budget,
            since,
        } => {
            if let Some(ref git_ref) = since {
                commands::cmd_review_since(&path, budget, git_ref)
            } else {
                commands::cmd_review(&path, budget)
            }
        }
        Commands::Session {
            agent_id,
            task,
            files,
            status,
            path,
        } => commands::cmd_session(
            &path,
            &agent_id,
            &task,
            files.as_deref(),
            status.as_deref().unwrap_or("in_progress"),
        ),
        Commands::Licenses { path, out, full } => {
            commands::cmd_licenses(&path, out.as_deref(), full, cli.json)
        }
        Commands::Federation { action } => commands::cmd_federation(&action),
        Commands::Audit {
            path,
            lang,
            format,
            strict,
        } => commands::cmd_audit(&path, lang.as_deref(), &format, strict, cli.json),
        Commands::New { name, lang, path } => commands::cmd_new(&name, &lang, &path),
        Commands::Overlay { anchor, path } => commands::overlay::cmd_overlay(&path, &anchor),
        Commands::Kickoff {
            name,
            interactive,
            path,
        } => commands::cmd_kickoff(&name, interactive, &path),
        Commands::Workflow {
            template,
            from,
            out,
            path,
        } => commands::cmd_workflow(&template, from.as_deref(), out.as_deref(), &path),
        Commands::Mcp { action } => match action {
            McpAction::Install {
                platform,
                binary,
                project,
                scope,
                all,
                dry_run,
            } => {
                let platforms = platform.map_or_else(Vec::new, |p| vec![p]);
                mcp::run_install(
                    &platforms,
                    binary.as_deref(),
                    project.as_deref(),
                    scope.as_deref(),
                    all,
                    dry_run,
                )
            }
            McpAction::Uninstall {
                platform,
                scope,
                all,
                dry_run,
            } => {
                let platforms = platform.map_or_else(Vec::new, |p| vec![p]);
                mcp::run_uninstall(&platforms, scope.as_deref(), all, dry_run)
            }
            McpAction::List => mcp::run_list(),
            McpAction::Serve { port, path } => mcp::run_http_server(&path, port),
        },
        Commands::Store { action } => match action {
            StoreAction::Path { path } => commands::cmd_store_path(&path),
            StoreAction::List => commands::cmd_store_list(),
            StoreAction::Prune { dry_run } => commands::cmd_store_prune(dry_run),
            StoreAction::Migrate { path } => commands::cmd_store_migrate(&path),
        },
        Commands::Emergency { reason, ttl, path } => commands::cmd_emergency(&path, &reason, &ttl),
        Commands::Suggest { intent } => commands::cmd_suggest(&intent),
        Commands::Diagnose { path, format } => {
            let effective_format = if cli.json && format == "text" {
                "json".to_string()
            } else {
                format
            };
            commands::cmd_diagnose(&path, &effective_format)
        }
        #[cfg(feature = "view")]
        Commands::Timeline {
            path_arg,
            from,
            to,
            at,
            max,
            out,
            no_open,
        } => commands::cmd_timeline(
            &path_arg,
            from.as_deref(),
            to.as_deref(),
            at.as_deref(),
            max,
            out.as_deref(),
            !no_open,
        ),
    }
}
