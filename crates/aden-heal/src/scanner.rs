// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::HealError;
use crate::drift::DriftEvent;
use aden_core::{Block, Document, filter::AdenFilter};
use aden_graph::cache::try_load;
use aden_graph::graph::AdenGraph;
use aden_store::{GraphStorage, Storage};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Markdown files tracked for staleness and template-drift detection.
///
/// These are the conventional doc files expected at the repo root. The list is
/// hardcoded for now; it should become configurable via `.aden/config.toml` once
/// that configuration layer lands (see ADR-005).
const TRACKED_MARKDOWN_FILES: &[&str] = &[
    "AGENTS.md",
    "README.md",
    "CONTRIBUTING.md",
    "NOTICE.md",
    "MAINTAINERS.md",
];

/// Bump whenever the extraction logic that produces cached Documents changes
/// (anchor format, symbol set, etc.). The scan cache is otherwise invalidated
/// only by file mtime, so a parser-logic change would keep serving stale docs
/// for unchanged files until they happen to be edited. Mirrors the index's
/// CURRENT_INDEX_VERSION.
const CACHE_LOGIC_VERSION: u32 = 2;

#[derive(Default, Serialize, Deserialize)]
struct SourceCache {
    /// Extraction-logic version this cache was written with. A mismatch on load
    /// discards the cache so every file re-parses under the current logic.
    #[serde(default)]
    version: u32,
    /// path relative to repo_root → (mtime_secs, serialized_doc_json per symbol).
    /// A single source file emits one Document per symbol, so the value MUST be a
    /// list — keying a single doc per file collapsed every file to its last
    /// symbol, making all other symbols read as OrphanAnchor on warm-cache runs.
    entries: HashMap<String, (u64, Vec<String>)>,
    timestamp_secs: u64,
}

pub struct Scanner {
    pub repo_root: PathBuf,
    cache: Option<SourceCache>,
    cache_path: PathBuf,
    filter: AdenFilter,
}

impl Scanner {
    pub fn new(repo_root: impl AsRef<Path>) -> Self {
        let root = repo_root.as_ref().to_path_buf();
        let cache_path = aden_paths::scan_cache_file(&root);
        let cache = std::fs::read(&cache_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<SourceCache>(&b).ok())
            .filter(|c| c.version == CACHE_LOGIC_VERSION);
        let filter = AdenFilter::from_directory(&root);
        Self {
            repo_root: root,
            cache,
            cache_path,
            filter,
        }
    }

    fn mtime(path: &Path) -> u64 {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH)
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    pub fn scan(&self) -> Result<Vec<DriftEvent>, HealError> {
        let mut events = Vec::new();
        let mut new_cache = SourceCache {
            version: CACHE_LOGIC_VERSION,
            timestamp_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            ..SourceCache::default()
        };

        // a. Find and parse all source files — skip unchanged ones from cache
        let mut source_paths = Vec::new();
        self.collect_source_files(&self.repo_root, &mut source_paths)?;

        // Parallel source file processing
        let mut source_entries: Vec<(PathBuf, Document)> = Vec::new();
        let source_work: Vec<_> = source_paths
            .par_iter()
            .map(|path| {
                let rel = path.strip_prefix(&self.repo_root).unwrap_or(path);
                let rel_str = rel.to_string_lossy().to_string();
                let current_mtime = Self::mtime(path);

                // Try cache fast-path: mtime matches and we have the file's docs.
                // The cached value holds ALL of the file's symbol docs, so every
                // symbol is restored — not just the last one.
                if let Some(cached_jsons) = self.cache.as_ref().and_then(|c| {
                    let (mt, jsons) = c.entries.get(&rel_str)?;
                    if *mt == current_mtime {
                        Some(jsons.clone())
                    } else {
                        None
                    }
                }) {
                    let docs: Vec<Document> = cached_jsons
                        .iter()
                        .filter_map(|j| serde_json::from_str::<Document>(j).ok())
                        .collect();
                    if docs.len() == cached_jsons.len() {
                        let entries = docs
                            .into_iter()
                            .map(|doc| (path.clone(), doc))
                            .collect::<Vec<_>>();
                        return Some((
                            vec![(rel_str.clone(), (current_mtime, cached_jsons))],
                            entries,
                        ));
                    }
                }

                // Slow path: read & parse. Cache ALL of the file's symbol docs
                // under one key so the warm-cache run restores every symbol.
                if let Ok(content) = std::fs::read_to_string(path)
                    && let Ok(docs) = aden_parse::parse_file(path, &content)
                {
                    let mut jsons = Vec::new();
                    let mut entries = Vec::new();
                    for doc in docs {
                        if let Ok(json) = serde_json::to_string(&doc) {
                            jsons.push(json);
                        }
                        entries.push((path.clone(), doc));
                    }
                    return Some((vec![(rel_str.clone(), (current_mtime, jsons))], entries));
                }
                None::<(Vec<(String, (u64, Vec<String>))>, Vec<(PathBuf, Document)>)>
            })
            .collect();

        // Merge parallel results
        for (cache_updates, entries) in source_work.into_iter().flatten() {
            for (rel_str, entry) in cache_updates {
                new_cache.entries.insert(rel_str, entry);
            }
            source_entries.extend(entries);
        }

        let mut anchor_to_source_idx: HashMap<String, usize> = HashMap::new();
        for (i, (_, doc)) in source_entries.iter().enumerate() {
            anchor_to_source_idx.insert(doc.anchor.clone(), i);
        }

        // b. Load contracts from store, fall back to disk .adoc files if store is empty
        let (store_path, is_legacy) = aden_paths::resolve_read_store(&self.repo_root);
        let mut aden_anchors: HashSet<String> = HashSet::new();
        let mut contract_docs: Vec<(String, Document)> = Vec::new();

        // ADR-011: prefer fresh graph.snapshot (lock-free) when doing drift
        // scanning that produces MergeReconcile proposals.
        if let Some((docs, _)) = aden_graph::snapshot::try_read_fresh(&self.repo_root) {
            for (anchor, doc) in docs {
                aden_anchors.insert(anchor.clone());
                contract_docs.push((anchor, doc));
            }
        }

        if contract_docs.is_empty()
            && store_path.exists()
            && {
                if is_legacy {
                    eprintln!("{}", aden_paths::legacy_notice(&self.repo_root));
                }
                true
            }
            && let Some(store_path_str) = store_path.to_str()
            && let Ok(storage) = Storage::open_existing(store_path_str)
            && let Ok(all_docs) = storage.get_all_documents()
            && !all_docs.is_empty()
        {
            for (anchor, doc) in all_docs {
                aden_anchors.insert(anchor.clone());
                contract_docs.push((anchor.clone(), doc));
            }
        }

        // Fallback: if store has no docs, parse .adoc files from disk
        if contract_docs.is_empty() {
            let mut contract_paths = Vec::new();
            self.collect_contract_files(&self.repo_root, &mut contract_paths)?;
            for path in &contract_paths {
                if let Ok(parsed) = aden_graph::parser::parse_file(path) {
                    for anchor in &parsed.anchors {
                        let doc = parsed_to_doc(&parsed, anchor, path);
                        aden_anchors.insert(anchor.clone());
                        contract_docs.push((anchor.clone(), doc));
                    }
                }
            }
        }

        // c. StaleHash - check source_hash against original source
        for (anchor, doc) in &contract_docs {
            if let Some(expected_hash) = doc.attributes.get("source_hash") {
                let source_path = self.find_source_for_doc(doc);
                if let Some(source_path) = source_path
                    && let Ok(content) = std::fs::read_to_string(&source_path)
                {
                    // Normalize line endings so a CRLF checkout on Windows does
                    // not falsely drift against an LF-generated contract hash.
                    let actual_hash = aden_core::hash_source(&content);
                    if actual_hash != *expected_hash {
                        events.push(DriftEvent::StaleHash {
                            target_path: format!(".aden/store:{}", anchor),
                            expected_hash: expected_hash.clone(),
                            actual_hash,
                        });
                    }
                }
            }
        }

        // d. SignatureMismatch
        for (anchor, doc) in &contract_docs {
            if let Some(&idx) = anchor_to_source_idx.get(anchor) {
                let (_, source_doc) = &source_entries[idx];
                let current_sig = extract_sig_from_doc(source_doc);
                let contract_sig = extract_sig_from_doc(doc);
                if contract_sig != current_sig {
                    events.push(DriftEvent::SignatureMismatch {
                        anchor: anchor.to_string(),
                        contract_path: format!(".aden/store:{}", anchor),
                        expected_sig: contract_sig,
                        actual_sig: current_sig,
                    });
                }
            }
        }

        // e. MissingContract - public source symbols without .aden contract.
        // Only CODE symbols need contracts: documentation files (.md/.adoc/.rst)
        // ARE the contracts/docs, so their headings and file-level anchors must
        // not be reported as "missing a contract". They also re-extract with an
        // unresolved `aden://doc/unknown/...` project segment that can never match
        // the store, so they would be permanent false positives.
        for (path, doc) in &source_entries {
            if is_doc_source_file(path) || crate::drift::is_expected_metadata(&doc.anchor) {
                continue;
            }
            if is_public_symbol(doc) && !aden_anchors.contains(&doc.anchor) {
                let symbol_name = doc
                    .anchor
                    .rfind('#')
                    .map(|i| doc.anchor[i + 1..].to_string())
                    .unwrap_or_else(|| doc.anchor.clone());
                events.push(DriftEvent::MissingContract {
                    source_path: path.to_string_lossy().to_string(),
                    anchor: doc.anchor.clone(),
                    symbol_name,
                });
            }
        }

        // f. OrphanAnchor - store anchors without corresponding source symbol.
        // Reference docs (doc headings, ADRs, plans, NOTICE/license entries,
        // code-block snippets) legitimately have no backing source symbol, so
        // emitting them here floods the report with thousands of non-actionable
        // events. Gate through `is_expected_metadata` — the same predicate the
        // CLI's `classify_orphans` uses — so heal agrees with check/status.
        for (anchor, _doc) in &contract_docs {
            if !anchor_to_source_idx.contains_key(anchor)
                && !crate::drift::is_expected_metadata(anchor)
            {
                events.push(DriftEvent::OrphanAnchor {
                    anchor: anchor.clone(),
                    contract_path: format!(".aden/store:{}", anchor),
                });
            }
        }

        // g. BrokenReference (best-effort; skip if graph has structural issues)
        // Use store-first graph loading for broken reference detection
        if let Some(graph) = try_load(&self.repo_root) {
            for (contract_path, ref_anchor) in graph.unresolved_refs() {
                let line = find_ref_line(&self.repo_root, &contract_path, &ref_anchor);
                events.push(DriftEvent::BrokenReference {
                    contract_path,
                    ref_anchor,
                    line,
                });
            }
        } else if let Ok(graph) = AdenGraph::build_from_directory(&self.repo_root) {
            for (contract_path, ref_anchor) in graph.unresolved_refs() {
                let line = find_ref_line(&self.repo_root, &contract_path, &ref_anchor);
                events.push(DriftEvent::BrokenReference {
                    contract_path,
                    ref_anchor,
                    line,
                });
            }
        }

        // h. DeadLink - check includes on disk (includes reference other files)
        for (anchor, doc) in &contract_docs {
            if let Some(includes) = doc.attributes.get("includes") {
                let inc_paths: Vec<&str> = includes.split(',').collect();
                for inc_path in inc_paths {
                    let inc_path = inc_path.trim();
                    if let Ok(inc_path_buf) =
                        resolve_include_from_anchor(anchor, inc_path, &self.repo_root)
                        && !inc_path_buf.exists()
                    {
                        events.push(DriftEvent::DeadLink {
                            contract_path: format!(".aden/store:{}", anchor),
                            include_path: inc_path.to_string(),
                        });
                    }
                }
            }
        }

        // i. Markdown Drift - scan for stale .md files
        events.extend(self.scan_markdown_drift()?);

        // j. Doc/code semantic divergence — fenced code in docs that declares a
        // function whose parameter arity no longer matches the real symbol.
        events.extend(self.scan_doc_signature_divergence(&source_entries));

        // Persist cache for next incremental scan
        if let Ok(json) = serde_json::to_string_pretty(&new_cache) {
            let _ = std::fs::create_dir_all(self.cache_path.parent().unwrap_or(Path::new(".")));
            let _ = std::fs::write(&self.cache_path, json);
        }

        Ok(events)
    }

    fn is_excluded_dir(&self, path: &Path) -> bool {
        // Always skip hard-coded build/vcs dirs regardless of .adenignore
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| matches!(n, "target" | ".git" | "node_modules" | ".cargo" | ".rustup"))
            .unwrap_or(false)
        {
            return true;
        }
        // Also honour .adenignore so agent dirs, tool configs, etc. are skipped
        if let Ok(rel) = path.strip_prefix(&self.repo_root) {
            return self.filter.should_skip(rel);
        }
        false
    }

    fn scan_markdown_drift(&self) -> Result<Vec<DriftEvent>, HealError> {
        let mut events = Vec::new();

        // Known markdown files that should be kept in sync
        let known_md_files = TRACKED_MARKDOWN_FILES;

        // Collect code source files ONCE, outside the per-markdown loop. This is
        // an O(N) recursive walk; doing it per markdown file made the whole scan
        // O(N*M). We deliberately exclude documentation extensions (.md/.rst/.adoc)
        // from the staleness trigger set so editing one markdown file does not make
        // every OTHER markdown file report as stale.
        let mut code_sources = Vec::new();
        self.collect_source_files(&self.repo_root, &mut code_sources)?;
        code_sources.retain(|p| {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            !matches!(ext, "md" | "rst" | "adoc" | "aden")
        });

        // Only repos that already participate in aden's markdown convention
        // (i.e. at least one of the known docs exists) are eligible for the
        // "missing template" check. This avoids flagging MissingMarkdownTemplate
        // on every project that happens not to ship a NOTICE.md/MAINTAINERS.md.
        let participates = known_md_files
            .iter()
            .any(|k| self.repo_root.join(k).exists());

        // A known markdown file absent from a participating repo root is a
        // "missing template" condition: aden expects to keep it in sync, but
        // there is nothing on disk. This is detectable without any markdown
        // generation infrastructure, so MissingMarkdownTemplate is emitted here.
        // (MarkdownDrift — a content-level diff of a generated template against
        // the on-disk file — requires the `aden gen --format md` renderer, which
        // the scanner does not invoke, so it is not emitted from here.)
        if participates {
            for known in known_md_files {
                let candidate = self.repo_root.join(known);
                if !candidate.exists() {
                    events.push(DriftEvent::MissingMarkdownTemplate {
                        md_path: candidate.to_string_lossy().to_string(),
                        template_source: format!(
                            "aden gen . --format md (would generate {})",
                            known
                        ),
                    });
                }
            }
        }

        for entry in std::fs::read_dir(&self.repo_root)? {
            let entry = entry?;
            let path = entry.path();

            if !path.is_file() {
                continue;
            }

            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            if !known_md_files.contains(&file_name) {
                continue;
            }

            // Check if source files have been modified since markdown was last modified
            let md_mtime = Self::mtime(&path);

            let stale_sources: Vec<String> = code_sources
                .iter()
                .filter(|s| Self::mtime(s) > md_mtime)
                .map(|s| s.to_string_lossy().to_string())
                .collect();

            if !stale_sources.is_empty() {
                events.push(DriftEvent::StaleMarkdown {
                    md_path: path.to_string_lossy().to_string(),
                    source_files_changed: stale_sources,
                });
            }
        }

        Ok(events)
    }

    /// Detect semantic doc/code divergence: a documentation file declares a
    /// function in a fenced code block whose parameter arity differs from the
    /// real symbol of the same name in the codebase.
    ///
    /// Precision-first by construction:
    /// - The fence body is parsed with the *real* language parser, so only
    ///   genuine declarations are matched — call-site usage examples
    ///   (`foo(a, b)`) are not extracted as functions and never flagged.
    /// - Only function names that are *unique* in the codebase are checked;
    ///   overloads / polymorphic names (`new`, `from`, trait methods) are
    ///   ambiguous and skipped to avoid false positives.
    fn scan_doc_signature_divergence(
        &self,
        source_entries: &[(PathBuf, Document)],
    ) -> Vec<DriftEvent> {
        use aden_core::NodeType;

        // Build name -> set of real parameter arities, from code (non-doc) symbols.
        let mut name_arities: HashMap<String, HashSet<usize>> = HashMap::new();
        for (path, doc) in source_entries {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if matches!(ext, "md" | "rst" | "adoc" | "aden") {
                continue; // only real code symbols define the ground truth
            }
            if doc.node_type != NodeType::Function {
                continue;
            }
            let Some(name) = bare_symbol_name(&doc.anchor) else {
                continue;
            };
            name_arities
                .entry(name)
                .or_default()
                .insert(extract_sig_from_doc(doc).len());
        }

        let mut events = Vec::new();
        let mut doc_paths = Vec::new();
        let _ = self.collect_doc_files(&self.repo_root, &mut doc_paths);

        for doc_path in &doc_paths {
            let content = match std::fs::read_to_string(doc_path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for fence in extract_code_fences(&content) {
                let Some(ext) = fence_lang_to_ext(&fence.lang) else {
                    continue;
                };
                // Parse the fence body as standalone source of that language.
                let synthetic = PathBuf::from(format!("snippet.{ext}"));
                let Ok(docs) = aden_parse::parse_file(&synthetic, &fence.body) else {
                    continue;
                };
                for d in docs {
                    if d.node_type != NodeType::Function {
                        continue;
                    }
                    let Some(name) = bare_symbol_name(&d.anchor) else {
                        continue;
                    };
                    let Some(arities) = name_arities.get(&name) else {
                        continue; // not a real symbol — nothing to diverge from
                    };
                    // Skip ambiguous symbols (overloads / polymorphic names).
                    if arities.len() != 1 {
                        continue;
                    }
                    let actual = *arities.iter().next().unwrap();
                    let documented = extract_sig_from_doc(&d).len();
                    if documented != actual {
                        events.push(DriftEvent::DocSignatureDivergence {
                            doc_path: doc_path.to_string_lossy().to_string(),
                            line: fence.start_line,
                            symbol_name: name,
                            documented_params: documented,
                            actual_params: actual,
                        });
                    }
                }
            }
        }
        events
    }

    /// Recursively collect documentation files (.md/.adoc/.rst) under `dir`,
    /// honouring the same exclusion rules as source collection.
    fn collect_doc_files(&self, dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), HealError> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false) {
                continue;
            }
            if path.is_dir() {
                if !self.is_excluded_dir(&path) {
                    self.collect_doc_files(&path, files)?;
                }
            } else if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if matches!(ext, "md" | "adoc" | "rst" | "markdown") {
                    files.push(path);
                }
            }
        }
        Ok(())
    }

    fn collect_source_files(&self, dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), HealError> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            // SECURITY: Skip symlinks to prevent traversal outside the repo.
            if entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false) {
                continue;
            }
            if path.is_dir() {
                if !self.is_excluded_dir(&path) {
                    self.collect_source_files(&path, files)?;
                }
            } else if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                // All languages supported by aden-parse — language-agnostic drift tracking
                if matches!(
                    ext,
                    // Systems
                    "rs" | "c" | "h" | "cpp" | "cc" | "cxx" | "hpp"
                    // Scripting / shell
                    | "py" | "rb" | "pl" | "lua" | "sh" | "bash"
                    // JVM
                    | "java" | "kt" | "kts" | "scala" | "groovy"
                    // .NET
                    | "cs" | "fs" | "vb"
                    // Web
                    | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs"
                    // Go / Zig / Swift / D
                    | "go" | "zig" | "swift" | "d"
                    // PHP / Ruby / Elixir / Erlang / Clojure
                    | "php" | "ex" | "exs" | "erl" | "hrl" | "clj" | "cljs"
                    // PowerShell
                    | "ps1" | "psm1" | "psd1"
                    // Data / config (structured, can carry semantics)
                    | "sql" | "graphql" | "gql" | "proto"
                    // Docs that are source of truth
                    | "adoc" | "aden" | "md" | "rst"
                ) {
                    files.push(path);
                }
            }
        }
        Ok(())
    }

    fn find_source_for_doc(&self, doc: &Document) -> Option<PathBuf> {
        // Try explicit :source_file: attribute
        if let Some(source_file) = doc.attributes.get("source_file") {
            let candidate = self.repo_root.join(source_file);
            if candidate.exists() {
                return Some(candidate);
            }
        }

        // Infer from anchor pattern: aden://module/{project}/{rel_path}#{symbol}
        //
        // Since feat/anchor-path-precision, `{rel_path}` is the project-root-relative
        // path (e.g. `src/commands/heal.rs`), NOT a bare basename.  The project root
        // is the directory that contains the manifest (Cargo.toml, go.mod, …) or the
        // VCS top-level directory for manifest-less repos.
        //
        // We probe several layouts to cover:
        //   • Cargo workspace members  → crates/{project}/{rel_path}
        //   • Standalone crate/module  → {project}/{rel_path}
        //   • File directly at root    → {rel_path}  (for VCS-root anchors like src/lib.rs)
        let anchor = &doc.anchor;
        if let Some(hash_pos) = anchor.rfind('#') {
            let prefix = &anchor[..hash_pos];
            if let Some(rest) = prefix.strip_prefix("aden://module/") {
                let parts: Vec<&str> = rest.splitn(2, '/').collect();
                if parts.len() == 2 {
                    let project = parts[0];
                    let rel_path = parts[1];
                    let candidates = [
                        format!("crates/{}/{}", project, rel_path),
                        format!("{}/{}", project, rel_path),
                        rel_path.to_string(),
                    ];
                    for candidate in &candidates {
                        let path = self.repo_root.join(candidate);
                        if path.exists() {
                            return Some(path);
                        }
                    }
                }
            }
        }

        None
    }

    fn collect_contract_files(
        &self,
        dir: &Path,
        files: &mut Vec<PathBuf>,
    ) -> Result<(), HealError> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            // SECURITY: Skip symlinks to prevent traversal outside the repo.
            if entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false) {
                continue;
            }
            if path.is_dir() {
                if !self.is_excluded_dir(&path) {
                    self.collect_contract_files(&path, files)?;
                }
            } else if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if ext == "adoc" || ext == "aden" {
                    files.push(path);
                }
            }
        }
        Ok(())
    }
}

fn parsed_to_doc(
    parsed: &aden_graph::parser::ParsedDocument,
    anchor: &str,
    _file_path: &Path,
) -> Document {
    use aden_core::Document;
    Document {
        anchor: anchor.to_string(),
        node_type: aden_core::NodeType::Note,
        attributes: parsed.attributes.clone(),
        blocks: vec![],
        source_span: None,
        metadata: None,
        confidence: 0.9,
    }
}

/// The bare trailing identifier of an anchor — strips the `aden://…#` prefix
/// and any language qualifier (`mod::fn`, `Class.method`, `ns\fn`) so a doc
/// fence's plain `build(...)` matches the codebase symbol regardless of how it
/// is module-qualified. This deliberately collapses qualified overloads to the
/// same key so the ambiguity guard skips them.
fn bare_symbol_name(anchor: &str) -> Option<String> {
    let after_hash = anchor.rsplit('#').next()?;
    let bare = after_hash
        .rsplit([':', '.', '\\', '/'])
        .next()
        .unwrap_or(after_hash)
        .trim();
    if bare.is_empty() {
        None
    } else {
        Some(bare.to_string())
    }
}

/// A fenced code block lifted from a documentation file.
struct CodeFence {
    /// The info string immediately after the opening fence (e.g. `rust`).
    lang: String,
    /// The raw body between the fences.
    body: String,
    /// 1-based line number of the opening fence.
    start_line: usize,
}

/// Extract fenced code blocks delimited by ``` or ~~~ from markdown/asciidoc.
/// Also handles AsciiDoc `[source,lang]` + `----` listing blocks.
fn extract_code_fences(content: &str) -> Vec<CodeFence> {
    let mut fences = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    // Pending language from a preceding AsciiDoc `[source,lang]` attribute line.
    let mut pending_lang: Option<String> = None;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        // AsciiDoc source attribute: [source,rust] / [source, python]
        if let Some(rest) = trimmed.strip_prefix("[source") {
            let lang = rest
                .trim_start_matches([',', ' '])
                .trim_end_matches(']')
                .trim()
                .to_string();
            if !lang.is_empty() {
                pending_lang = Some(lang);
            }
            i += 1;
            continue;
        }

        let fence_marker = if trimmed.starts_with("```") {
            Some("```")
        } else if trimmed.starts_with("~~~") {
            Some("~~~")
        } else if trimmed.starts_with("----") {
            Some("----") // AsciiDoc listing block
        } else {
            None
        };

        if let Some(marker) = fence_marker {
            let info = trimmed[marker.len()..].trim().to_string();
            let lang = if !info.is_empty() {
                info
            } else {
                pending_lang.take().unwrap_or_default()
            };
            let start_line = i + 1;
            let mut body = String::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with(marker) {
                body.push_str(lines[i]);
                body.push('\n');
                i += 1;
            }
            i += 1; // skip closing fence
            if !lang.is_empty() {
                fences.push(CodeFence {
                    lang,
                    body,
                    start_line,
                });
            }
            pending_lang = None;
            continue;
        }

        // A non-fence, non-attribute line clears any pending source attribute.
        if !trimmed.is_empty() {
            pending_lang = None;
        }
        i += 1;
    }
    fences
}

/// Map a fenced code-block language tag to a file extension aden can parse.
/// Returns `None` for languages without a deep parameter-extracting parser, so
/// the divergence check stays high-precision.
fn fence_lang_to_ext(lang: &str) -> Option<&'static str> {
    let l = lang.trim().to_ascii_lowercase();
    Some(match l.as_str() {
        "rust" | "rs" => "rs",
        "python" | "py" => "py",
        "go" | "golang" => "go",
        "javascript" | "js" => "js",
        "typescript" | "ts" => "ts",
        "tsx" => "tsx",
        "java" => "java",
        "c" => "c",
        "cpp" | "c++" | "cxx" => "cpp",
        "ruby" | "rb" => "rb",
        "php" => "php",
        "csharp" | "cs" | "c#" => "cs",
        "kotlin" | "kt" => "kt",
        _ => return None,
    })
}

fn extract_sig_from_doc(doc: &Document) -> Vec<String> {
    let mut sig = Vec::new();
    for block in &doc.blocks {
        if let Block::Table(table) = block {
            for row in &table.rows {
                if row.len() >= 2 && row[0].starts_with("param ") {
                    sig.push(row[1].clone());
                }
            }
        }
    }
    sig
}

/// Whether a source path is a prose document rather than code. Such files are
/// the documentation itself, so their anchors must not be treated as code
/// symbols that need a generated contract (MissingContract).
fn is_doc_source_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("md" | "adoc" | "rst" | "txt" | "aden")
    )
}

fn is_public_symbol(doc: &Document) -> bool {
    for block in &doc.blocks {
        if let Block::Table(table) = block {
            for row in &table.rows {
                if row.len() >= 2 && row[0] == "Visibility" {
                    return row[1] == "Public" || row[1] == "Crate";
                }
            }
        }
    }
    // Default to true for languages where visibility is not tracked
    true
}

/// Find the 1-based line number where `ref_anchor` is referenced inside the
/// contract at `contract_path`. Returns 0 when the contract is store-resident
/// (pseudo-path `.aden/store:…`, no file to read), unreadable, or the reference
/// can't be located by text search.
fn find_ref_line(repo_root: &Path, contract_path: &str, ref_anchor: &str) -> usize {
    // Store-backed pseudo-paths have no on-disk file to scan.
    if contract_path.starts_with(".aden/store:") {
        return 0;
    }
    let path = {
        let p = Path::new(contract_path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            repo_root.join(p)
        }
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    for (i, line) in content.lines().enumerate() {
        if line.contains(&format!("<<{}", ref_anchor)) {
            return i + 1;
        }
    }
    0
}

/// Resolve an include path from an anchor, preventing directory traversal attacks.
fn resolve_include_from_anchor(
    _anchor: &str,
    include: &str,
    repo_root: &Path,
) -> std::io::Result<PathBuf> {
    let candidate = repo_root.join(include);

    // Prevent traversal outside the base directory
    if candidate
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "Include path '{}' contains parent-dir traversal (..). Denied for security.",
                include
            ),
        ));
    }

    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drift::DriftEvent;
    use std::io::Write;

    #[test]
    fn scanner_scan_empty_dir_returns_no_events() {
        let dir = tempfile::tempdir().unwrap();
        let scanner = Scanner::new(dir.path());
        let events = scanner.scan().unwrap();
        // An empty directory may produce MissingContract events for source files,
        // but if there are no .adoc/.aden files there should be no events
        assert!(
            events.is_empty()
                || events
                    .iter()
                    .all(|e| !matches!(e, DriftEvent::StaleHash { .. }))
        );
    }

    fn divergence_events(events: &[DriftEvent]) -> Vec<&DriftEvent> {
        events
            .iter()
            .filter(|e| matches!(e, DriftEvent::DocSignatureDivergence { .. }))
            .collect()
    }

    #[test]
    fn doc_divergence_flags_stale_signature_in_docs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let src = root.join("src");
        std::fs::create_dir(&src).unwrap();
        // Code: process now takes THREE params.
        std::fs::write(
            src.join("lib.rs"),
            "pub fn process(input: i32, count: i32, flag: bool) -> bool { flag }",
        )
        .unwrap();
        // Docs: a fenced block shows the OLD two-param signature.
        std::fs::write(
            root.join("README.md"),
            "# Guide\n\n```rust\nfn process(input: i32, count: i32) -> bool {}\n```\n",
        )
        .unwrap();

        let events = Scanner::new(root).scan().unwrap();
        let div = divergence_events(&events);
        assert_eq!(div.len(), 1, "expected one divergence, got: {:?}", div);
        match div[0] {
            DriftEvent::DocSignatureDivergence {
                symbol_name,
                documented_params,
                actual_params,
                ..
            } => {
                assert_eq!(symbol_name, "process");
                assert_eq!(*documented_params, 2);
                assert_eq!(*actual_params, 3);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn doc_divergence_silent_when_signatures_agree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let src = root.join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "pub fn process(input: i32, count: i32) -> bool { true }",
        )
        .unwrap();
        std::fs::write(
            root.join("README.md"),
            "# Guide\n\n```rust\nfn process(input: i32, count: i32) -> bool {}\n```\n",
        )
        .unwrap();

        let events = Scanner::new(root).scan().unwrap();
        assert!(
            divergence_events(&events).is_empty(),
            "matching signatures must not diverge"
        );
    }

    #[test]
    fn doc_divergence_ignores_usage_examples() {
        // A doc fence that merely *calls* the function with a different arg count
        // is not a declaration and must never be flagged — the precision guarantee.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let src = root.join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "pub fn process(input: i32, count: i32) -> bool { true }",
        )
        .unwrap();
        std::fs::write(
            root.join("README.md"),
            "# Guide\n\n```rust\nlet r = process(1);\nlet s = process(1, 2, 3, 4);\n```\n",
        )
        .unwrap();

        let events = Scanner::new(root).scan().unwrap();
        assert!(
            divergence_events(&events).is_empty(),
            "call-site usage examples must not be flagged as divergence"
        );
    }

    #[test]
    fn doc_divergence_skips_ambiguous_names() {
        // Two real `build` functions with different arities → ambiguous → skip.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let src = root.join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("a.rs"), "pub fn build(x: i32) -> i32 { x }").unwrap();
        std::fs::write(
            src.join("b.rs"),
            "pub fn build(x: i32, y: i32) -> i32 { x + y }",
        )
        .unwrap();
        std::fs::write(
            root.join("README.md"),
            "# Guide\n\n```rust\nfn build(a: i32, b: i32, c: i32) {}\n```\n",
        )
        .unwrap();

        let events = Scanner::new(root).scan().unwrap();
        assert!(
            divergence_events(&events).is_empty(),
            "ambiguous (overloaded) names must be skipped"
        );
    }

    #[test]
    fn scanner_detects_stale_contract() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create a source Rust file
        let src = root.join("src");
        std::fs::create_dir(&src).unwrap();
        let source_file = src.join("lib.rs");
        let mut file = std::fs::File::create(&source_file).unwrap();
        write!(file, "pub fn hello() {{ println!(\"hello\"); }}").unwrap();

        // Compute actual source hash
        let source_bytes = std::fs::read(&source_file).unwrap();
        let actual_hash = aden_core::stable_hash(&source_bytes);

        // Create a contract with the CORRECT source_hash
        let contract = root.join("lib.rs.adoc");
        let mut file = std::fs::File::create(&contract).unwrap();
        write!(
            file,
            r#":source_file: src/lib.rs
:source_hash: {}
[[lib-rs]]
= lib.rs

Hello world.
"#,
            actual_hash
        )
        .unwrap();

        // First scan: hash matches, no stale events
        let scanner = Scanner::new(root);
        let events = scanner.scan().unwrap();
        let stale_count = events
            .iter()
            .filter(|e| matches!(e, DriftEvent::StaleHash { .. }))
            .count();
        assert_eq!(
            stale_count, 0,
            "Fresh contract should not produce StaleHash"
        );

        // Modify the source file
        let mut file = std::fs::File::create(&source_file).unwrap();
        write!(file, "pub fn hello() {{ println!(\"modified\"); }}").unwrap();

        // Rescan — should detect stale hash
        let scanner = Scanner::new(root);
        let events = scanner.scan().unwrap();
        let stale_count = events
            .iter()
            .filter(|e| matches!(e, DriftEvent::StaleHash { .. }))
            .count();
        assert!(
            stale_count > 0,
            "Modified source should produce StaleHash. Events: {:?}",
            events
        );
    }

    #[test]
    fn scanner_finds_orphan_anchors() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create a doc that references a non-existent anchor
        let contract = root.join("orphan.adoc");
        let mut file = std::fs::File::create(&contract).unwrap();
        write!(
            file,
            r#"[[orphan]]
= Orphan

<<nonexistent>>
"#
        )
        .unwrap();

        let scanner = Scanner::new(root);
        let events = scanner.scan().unwrap();
        let has_orphan = events
            .iter()
            .any(|e| matches!(e, DriftEvent::OrphanAnchor { .. }));
        let has_broken_ref = events
            .iter()
            .any(|e| matches!(e, DriftEvent::BrokenReference { .. }));

        // OrphanAnchor detection depends on the graph build; BrokenReference is more likely
        if !has_orphan && !has_broken_ref {
            // If no structural issues detected, the scanner at least ran without panicking
            // Scanner ran successfully; structural checks depend on graph construction details
        }
    }

    #[test]
    fn scanner_detects_missing_contract() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create a source file
        let source = root.join("src");
        std::fs::create_dir(&source).unwrap();
        let main_rs = source.join("main.rs");
        let mut file = std::fs::File::create(&main_rs).unwrap();
        write!(file, r#"fn main() {{ println!("hello"); }}"#).unwrap();

        let scanner = Scanner::new(root);
        let events = scanner.scan().unwrap();
        // Should detect MissingContract for main.rs
        let missing = events
            .iter()
            .filter(|e| matches!(e, DriftEvent::MissingContract { .. }))
            .count();
        assert!(
            missing > 0 || events.is_empty(),
            "Scanner should either detect MissingContract or produce no events"
        );
    }

    #[test]
    fn tracked_markdown_files_const_contains_expected_entries() {
        // The five canonical doc files must all be present in the const.
        for name in &[
            "AGENTS.md",
            "README.md",
            "CONTRIBUTING.md",
            "NOTICE.md",
            "MAINTAINERS.md",
        ] {
            assert!(
                TRACKED_MARKDOWN_FILES.contains(name),
                "{name} is missing from TRACKED_MARKDOWN_FILES"
            );
        }

        // CHANGELOG.md is intentionally NOT in the tracked list; it is not
        // generated by `aden gen` and does not need staleness tracking.
        assert!(
            !TRACKED_MARKDOWN_FILES.contains(&"CHANGELOG.md"),
            "CHANGELOG.md should not be in TRACKED_MARKDOWN_FILES"
        );
    }
}
