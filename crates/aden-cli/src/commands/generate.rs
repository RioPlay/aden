// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_graph::graph::AdenGraph;
use aden_store::{GraphStorage, Storage};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::Path;

use crate::types::GenCacheEntry;
use crate::util::{
    discover_source_files, find_project_root, load_gen_cache, sanitize_source_file, save_gen_cache,
};

/// One stored symbol plus the compact data the linker needs. Carrying callee
/// names out of the parse phase means linking never has to reload the (huge)
/// document store to rebuild the call graph.
struct EmittedSymbol {
    anchor: String,
    callees: Vec<String>,
    /// `edge::uses[Type]` references — types named in a signature/fields, linked
    /// as `Uses` edges so a type that is used but never *called* is not a false
    /// dead-code candidate.
    uses: Vec<String>,
    /// `<<target>>` cross-references found in the document body (docs link to
    /// other docs / code via these).
    refs: Vec<String>,
    /// Whether this symbol's generated document was actually written to the
    /// store. False when the merge gate held it back (overlay conflict) or on a
    /// dry-run — so the summary count reflects real writes, not just processing.
    wrote: bool,
}

/// Work item returned from parallel file processing.
///
/// `Reindexed` is emitted whenever a file was (re)parsed — even if it produced
/// ZERO symbols (e.g. every function was deleted). That is deliberate: the
/// prune step diffs each reindexed file's fresh anchor set against the set the
/// cache recorded last time, so an emptied file correctly drops its stale
/// symbols. `Skip` is reserved for files whose mtime is unchanged.
enum WorkItem {
    Skip,
    Reindexed {
        cache_key: String,
        source_mtime: u64,
        source_path: String,
        symbols: Vec<EmittedSymbol>,
        /// Anchors whose freshly-generated content collided with durable
        /// `[human]`/`[agent]` overlay intent. Surfaced as proposals; the
        /// stored document is left untouched for these.
        conflicts: Vec<(String, aden_core::contract::MergeProposal)>,
    },
}

/// Emit a progress line unless quiet mode is on.
macro_rules! progress {
    ($quiet:expr, $($arg:tt)*) => {
        if !$quiet { println!($($arg)*); }
    };
}

/// Module name for a symbol anchor of the form
/// `aden://module/<path>/<file>#<symbol>`.
///
/// The module is the directory that immediately contains the file — i.e. the
/// package. Using the *first* path segment was wrong for path-based ecosystems:
/// Go anchors like `aden://module/github.com/spf13/cobra/command.go#Execute`
/// collapsed the entire repo into `mod-github.com`. The last path segment is
/// always the file (`make_anchor` appends `/<file>#<sym>`), so the directory
/// before it is the real package/crate. For aden's own `crate/file.rs` layout
/// this still yields the crate name, so nothing regresses.
fn crate_from_anchor(anchor: &str) -> Option<String> {
    let rest = anchor.strip_prefix("aden://module/")?;
    let path = rest.split('#').next()?;
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let name = match segs.len() {
        0 => return None,
        1 => segs[0],     // file at module root — use it
        n => segs[n - 2], // directory containing the file
    };
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Callee names referenced by a symbol document, for call-graph linking.
/// Reads both the `edge::calls[...]` listing and the `Callee` table so it works
/// regardless of which an extractor emits.
fn extract_callees(doc: &aden_core::Document) -> Vec<String> {
    use aden_core::Block;
    let mut callees = Vec::new();
    for block in &doc.blocks {
        match block {
            Block::Listing { code, .. } => {
                for line in code.lines() {
                    if let Some(rest) = line.trim().strip_prefix("edge::calls[")
                        && let Some(callee) = rest.strip_suffix(']')
                        && !callee.is_empty()
                    {
                        callees.push(callee.to_string());
                    }
                }
            }
            Block::Table(t)
                if t.headers.first().map(|h| h.eq_ignore_ascii_case("callee")) == Some(true) =>
            {
                for row in &t.rows {
                    if let Some(c) = row.first()
                        && !c.is_empty()
                    {
                        callees.push(c.clone());
                    }
                }
            }
            _ => {}
        }
    }
    callees.sort();
    callees.dedup();
    callees
}

/// Type names a symbol `Uses`, read from `edge::uses[...]` listings (emitted by
/// extractors for the types referenced in a signature/fields). Kept separate
/// from callees so they link as `Uses` edges, not `Calls`.
fn extract_uses(doc: &aden_core::Document) -> Vec<String> {
    use aden_core::Block;
    let mut uses = Vec::new();
    for block in &doc.blocks {
        if let Block::Listing { code, .. } = block {
            for line in code.lines() {
                if let Some(rest) = line.trim().strip_prefix("edge::uses[")
                    && let Some(t) = rest.strip_suffix(']')
                    && !t.is_empty()
                {
                    uses.push(t.to_string());
                }
            }
        }
    }
    uses.sort();
    uses.dedup();
    uses
}

/// Append `<<target>>` cross-reference targets found in `text` to `out`.
fn collect_xrefs(text: &str, out: &mut Vec<String>) {
    let mut rest = text;
    while let Some(s) = rest.find("<<") {
        let after = &rest[s + 2..];
        let Some(e) = after.find(">>") else { break };
        let inner = &after[..e];
        let target = inner.split(',').next().unwrap_or(inner).trim();
        if !target.is_empty() && !target.contains('{') {
            out.push(target.to_string());
        }
        rest = &after[e + 2..];
    }
}

/// Cross-references a document makes via `<<target>>` macros in its prose. These
/// become graph edges so documentation is connected to what it references (docs
/// were previously hollow, unlinked islands).
fn extract_doc_refs(doc: &aden_core::Document) -> Vec<String> {
    use aden_core::Block;
    let mut refs = Vec::new();
    for block in &doc.blocks {
        match block {
            Block::Paragraph(t) => collect_xrefs(t, &mut refs),
            Block::Listing { code, .. } => collect_xrefs(code, &mut refs),
            _ => {}
        }
    }
    refs.sort();
    refs.dedup();
    refs
}

/// Slim a document before storing it. Drops the `edge::calls[...]` listing
/// block — it is redundant with the `Callee` table for display and is no longer
/// needed for linking (callees are carried out of the parse phase directly), so
/// storing it just bloats the (already large) store on big repos.
fn slim_doc_for_store(doc: &mut aden_core::Document) {
    use aden_core::Block;
    doc.blocks.retain(|b| {
        let Block::Listing { code, .. } = b else {
            return true;
        };
        // Drop only listings that are purely `edge::` macros.
        !code
            .lines()
            .filter(|l| !l.trim().is_empty())
            .all(|l| l.trim().starts_with("edge::"))
    });
}

/// Resolve a callee string to a single target anchor, or None if unknown.
/// Order: (1) self-receiver calls (`self.foo`, `this.foo`, `$this->foo`,
/// `Self::foo`) resolve exactly to a method on the caller's own enclosing type —
/// the one OOP case with zero ambiguity; (2) the full callee; (3) the trailing
/// segment after the last `.`/`:` so receiver/qualified calls link
/// (`c.ExecuteC` → `ExecuteC`, `click.echo` → `echo`, `Path::new` → `new`).
/// When a name is ambiguous (defined in several
/// places) we disambiguate by locality: prefer a candidate in the caller's own
/// FILE, then in its crate. Most calls are intra-file/intra-crate, so this
/// resolves the common case (e.g. a private `node_text` helper copied into every
/// extractor file) instead of dropping the edge — without guessing across
/// modules, which would forge false edges.
fn resolve_callee<'a>(
    callee: &str,
    caller: &str,
    name_index: &HashMap<&str, Vec<&'a str>>,
) -> Option<&'a str> {
    let caller_file = anchor_file(caller);
    let caller_crate = crate_from_anchor(caller);
    let pick = |cands: &[&'a str]| -> Option<&'a str> {
        match cands {
            [] => None,
            [one] => Some(*one),
            many => {
                // Ambiguous: prefer the caller's own file, then its crate.
                if let Some(cf) = caller_file {
                    let same: Vec<&'a str> = many
                        .iter()
                        .copied()
                        .filter(|a| anchor_file(a) == Some(cf))
                        .collect();
                    if same.len() == 1 {
                        return Some(same[0]);
                    }
                }
                let cc = caller_crate.as_deref()?;
                let same: Vec<&'a str> = many
                    .iter()
                    .copied()
                    .filter(|a| crate_from_anchor(a).as_deref() == Some(cc))
                    .collect();
                if same.len() == 1 { Some(same[0]) } else { None }
            }
        }
    };
    // Self-receiver fast path (exact, zero false-edge risk): a call through the
    // caller's own instance — `self.foo()`, `this.foo()`, `$this->foo()`,
    // `Self::foo()` — provably targets a method of the caller's OWN enclosing
    // type, which we know from the caller anchor. This is precisely the OOP /
    // method-heavy case that base-name matching cannot disambiguate (e.g. a
    // `Command.invoke` call colliding with `OptParseCommand.invoke`). We only
    // handle a direct method (no further `.`/`::`/`->` in the remainder); a
    // field hop like `self.command.run` has an unknown intermediate type.
    if let Some(method) = strip_self_receiver(callee)
        && is_plain_ident(method)
        && let Some(qualified) = enclosing_qualified(caller, method)
        && let Some(t) = name_index.get(qualified.as_str())
        && let Some(r) = pick(t)
    {
        return Some(r);
    }
    if let Some(t) = name_index.get(callee)
        && let Some(r) = pick(t)
    {
        return Some(r);
    }
    let base = callee.rsplit(['.', ':']).next().unwrap_or(callee);
    if base != callee
        && !base.is_empty()
        && let Some(t) = name_index.get(base)
        && let Some(r) = pick(t)
    {
        return Some(r);
    }
    None
}

/// The file portion of a module anchor: `aden://module/<file>#<sym>` → `<file>`
/// (e.g. `aden-parse/rust.rs`). Used to scope ambiguous-callee resolution to the
/// caller's own file. Returns `None` for non-module anchors (docs, etc.).
fn anchor_file(anchor: &str) -> Option<&str> {
    anchor.strip_prefix("aden://module/")?.split('#').next()
}

/// If `callee` is a call through the current instance (`self.x`, `this.x`,
/// `$this->x`, `Self::x`), return the part after the receiver. Returns `None`
/// for everything else, so non-self calls fall through to ordinary resolution.
fn strip_self_receiver(callee: &str) -> Option<&str> {
    const PREFIXES: &[&str] = &["self.", "this.", "self::", "Self::", "$this->", "this->"];
    PREFIXES.iter().find_map(|p| callee.strip_prefix(p))
}

/// True if `s` is a single bare identifier — no member/path separators. Guards
/// the self-receiver path to direct method calls (`self.run`), excluding field
/// hops (`self.inner.run`) whose intermediate type we cannot know.
fn is_plain_ident(s: &str) -> bool {
    !s.is_empty() && !s.contains('.') && !s.contains(':') && !s.contains("->")
}

/// Given a caller anchor and a method name, build the fully-qualified symbol name
/// of that method ON THE CALLER'S OWN ENCLOSING TYPE, preserving the language's
/// member separator so it matches how the type's methods are stored:
/// `…#Command.main` + `invoke` → `Command.invoke` (Python `.`),
/// `…#FjallStorage::open` + `flush` → `FjallStorage::flush` (Rust `::`).
/// Returns `None` when the caller symbol is not itself a method (a free function
/// has no enclosing type, so `self` could not appear anyway).
fn enclosing_qualified(caller: &str, method: &str) -> Option<String> {
    let sym = caller.rsplit('#').next()?;
    let dot = sym.rfind('.');
    let colon = sym.rfind("::");
    let (idx, sep) = match (dot, colon) {
        (Some(d), Some(c)) if d > c => (d, "."),
        (Some(_), Some(c)) => (c, "::"),
        (Some(d), None) => (d, "."),
        (None, Some(c)) => (c, "::"),
        (None, None) => return None,
    };
    let ty = &sym[..idx];
    if ty.is_empty() {
        return None;
    }
    Some(format!("{ty}{sep}{method}"))
}

/// Tally of how the call-site callee references fared during resolution. Counts
/// are per callee reference, not per built edge — self-calls and several callee
/// strings collapsing onto one target are each counted once here, yet build zero
/// or deduped edges. Pure diagnostics: it never changes which edges are built, it
/// just explains where the call graph thins out (a dropped call site is either an
/// `Unresolved` name that matches no stored symbol, or an `Ambiguous` one that
/// matches several and couldn't be disambiguated by locality).
#[derive(Default)]
struct CalleeStats {
    resolved: usize,
    unresolved: usize,
    ambiguous: usize,
}

/// Why a callee did not produce a `Calls` edge — used to attribute drops to the
/// messiness they signal (no such symbol vs. a name shared across modules).
enum DropReason {
    Unresolved,
    Ambiguous,
}

/// Classify a callee that `resolve_callee` declined to link. Mirrors that
/// function's lookup order (full name, then trailing segment) but only inspects
/// candidate *counts*: zero candidates → `Unresolved`, otherwise the locality
/// heuristic gave up on multiple → `Ambiguous`. Cheap (HashMap lookups, no
/// allocation) and never alters resolution.
fn classify_drop(callee: &str, name_index: &HashMap<&str, Vec<&str>>) -> DropReason {
    let count = |name: &str| name_index.get(name).map(|c| c.len()).unwrap_or(0);
    if count(callee) > 0 {
        // The full (qualified) name itself is defined somewhere — a genuine
        // internal name collision the locality heuristic couldn't break.
        return DropReason::Ambiguous;
    }
    // Static path call `Qualifier::base` (Rust/C++/PHP `::`): the qualifier names
    // a *type*. If that type is not a project symbol, the call targets an external
    // type whose method name merely collides with a project name (e.g. `Vec::new`
    // colliding with the 24 internal `new`s) — external, not internal ambiguity.
    if let Some(pos) = callee.rfind("::") {
        let base = &callee[pos + 2..];
        // The immediate type segment directly before the called method.
        let qualifier = callee[..pos].rsplit(['.', ':']).next().unwrap_or("");
        if base.is_empty() || count(base) == 0 {
            return DropReason::Unresolved;
        }
        return if !qualifier.is_empty() && count(qualifier) == 0 {
            DropReason::Unresolved
        } else {
            DropReason::Ambiguous
        };
    }
    // Receiver call `recv.base`: the qualifier is a *value* whose type we can't
    // know statically, so we cannot declare it external — fall back to the base
    // name. A receiver call to a name no project symbol carries is still external.
    let base = callee.rsplit('.').next().unwrap_or(callee);
    if base == callee || base.is_empty() || count(base) == 0 {
        DropReason::Unresolved
    } else {
        DropReason::Ambiguous
    }
}

/// Connect the stored symbols into a traversable graph by persisting edges,
/// with bounded memory so it scales to large repositories.
///
/// Critically this never calls `get_all_documents()` — loading every full
/// document into RAM is what made linking the Linux kernel (a 17 GB store)
/// OOM. Instead it:
/// 1. reads only the anchor *keys* (`get_all_anchors`) to build the name index
///    and the module containment edges, and
/// 2. takes call-site data as compact `(anchor, callees)` records collected
///    during the parse phase.
///
/// All edges are then written with a single `put_edges_bulk` pass (O(E), not the
/// O(N^2) that per-edge writes incur on high-degree module nodes).
///
/// Edges built:
/// - Containment: `mod-<crate>` --Documents--> symbol, symbol --PartOf-->
///   `mod-<crate>`, `mod-project` --Documents--> each module. Module nodes are
///   synthesized here (they otherwise live only in ignored `.adoc` files).
/// - Calls: each resolved callee becomes a `Calls` edge.
fn link_store_edges<S: GraphStorage>(
    storage: &S,
    link_records: &[(String, Vec<String>)],
    use_records: &[(String, Vec<String>)],
    ref_records: &[(String, Vec<String>)],
) -> Result<CalleeStats, Box<dyn std::error::Error>> {
    use aden_core::{Block, Document, EdgeType, NodeType};
    use std::collections::HashSet;

    // Anchor keys only — cheap relative to full documents.
    let anchors = storage.get_all_anchors()?;

    // Short symbol name -> anchors that define it (borrows from `anchors`).
    // Member anchors are qualified as `Type::method` (so same-named methods on
    // different types don't collide in the store). A call site like
    // `filter.should_skip()` yields the BARE callee `should_skip`, which would
    // never match the qualified key — leaving most method calls unresolved. So we
    // also index each member by its trailing `::` segment. Collisions (several
    // types with the same method name) are disambiguated downstream by
    // `resolve_callee`'s locality heuristic (caller's file, then crate); genuinely
    // ambiguous ones stay unlinked rather than producing a wrong edge.
    let mut name_index: HashMap<&str, Vec<&str>> = HashMap::new();
    for anchor in &anchors {
        if let Some(hash) = anchor.rfind('#') {
            let name = &anchor[hash + 1..];
            if !name.is_empty() {
                name_index.entry(name).or_default().push(anchor.as_str());
                // Also index the trailing member segment after the last `::` OR
                // `.` so a bare/receiver call resolves to a qualified method:
                // Rust `Storage::open` and Go/Python `Command.Execute` both also
                // index `open` / `Execute`. Collisions are disambiguated
                // downstream by `resolve_callee`'s locality + self/receiver paths.
                if let Some(method) = name.rsplit(['.', ':']).next()
                    && method != name
                    && !method.is_empty()
                {
                    name_index.entry(method).or_default().push(anchor.as_str());
                }
            }
        }
    }

    let mut edges: Vec<(String, String, EdgeType)> = Vec::new();
    let mut modules: HashSet<String> = HashSet::new();

    // Containment for every anchor.
    for anchor in &anchors {
        if let Some(krate) = crate_from_anchor(anchor) {
            let module_anchor = format!("mod-{}", krate);
            modules.insert(krate);
            edges.push((module_anchor.clone(), anchor.clone(), EdgeType::Documents));
            edges.push((anchor.clone(), module_anchor, EdgeType::PartOf));
        }
    }

    // Call edges from the compact per-symbol records. Tally each callee so the
    // gen summary can flag where the call graph silently thins out.
    let mut callee_stats = CalleeStats::default();
    for (anchor, callees) in link_records {
        for callee in callees {
            match resolve_callee(callee, anchor, &name_index) {
                Some(target) if target != anchor.as_str() => {
                    callee_stats.resolved += 1;
                    edges.push((anchor.clone(), target.to_string(), EdgeType::Calls));
                }
                // A self-call resolves but builds no edge (we skip self-loops);
                // count it as resolved so it isn't mistaken for a dropped edge.
                Some(_) => callee_stats.resolved += 1,
                None => match classify_drop(callee, &name_index) {
                    DropReason::Unresolved => callee_stats.unresolved += 1,
                    DropReason::Ambiguous => callee_stats.ambiguous += 1,
                },
            }
        }
    }

    // Type-usage edges: a symbol whose signature/fields name a stored type
    // `Uses` it. Keeps a type that is used (but never *called*) from looking like
    // dead code in graph-wide queries like `where callers=0`.
    for (anchor, used_types) in use_records {
        for used in used_types {
            if let Some(target) = resolve_callee(used, anchor, &name_index)
                && target != anchor.as_str()
            {
                edges.push((anchor.clone(), target.to_string(), EdgeType::Uses));
            }
        }
    }

    // Cross-reference edges from document `<<target>>` macros. Bidirectional so
    // backlinks work (a doc and what it references are mutually reachable).
    for (anchor, refs) in ref_records {
        for r in refs {
            if let Some(target) = resolve_callee(r, anchor, &name_index)
                && target != anchor.as_str()
            {
                edges.push((anchor.clone(), target.to_string(), EdgeType::RelatesTo));
                edges.push((target.to_string(), anchor.clone(), EdgeType::RelatesTo));
            }
        }
    }

    // Documentation edges: doc nodes whose anchor encodes a code-symbol anchor
    // in the form `aden://doc/<path>#<code-anchor>` (produced by asciidoc
    // `[[code-anchor]]` declarations) are linked to the symbol they document.
    // This makes doc nodes reachable from code traversals and vice-versa.
    let anchor_set: std::collections::HashSet<&str> = anchors.iter().map(|s| s.as_str()).collect();
    for anchor in &anchors {
        let Some(rest) = anchor.strip_prefix("aden://doc/") else {
            continue;
        };
        // The embedded code anchor starts after the first `#` in the `rest`
        // portion.  A plain doc heading like `aden://doc/x/y.md/h1foo` has no
        // `#` in `rest`; a symbol-linked doc like
        // `aden://doc/aden-cli/generate.adoc#aden://module/aden-cli/generate.rs#fn`
        // has one.
        let Some(hash_pos) = rest.find('#') else {
            continue;
        };
        let code_anchor = &rest[hash_pos + 1..];
        if code_anchor.is_empty() || !anchor_set.contains(code_anchor) {
            continue;
        }
        edges.push((anchor.clone(), code_anchor.to_string(), EdgeType::Documents));
        edges.push((code_anchor.to_string(), anchor.clone(), EdgeType::PartOf));
    }

    // Synthesize module nodes + project root, and connect the project to each.
    if !modules.is_empty() {
        let make_module_doc = |anchor: &str, body: &str| Document {
            anchor: anchor.to_string(),
            node_type: NodeType::Module,
            attributes: HashMap::new(),
            blocks: vec![Block::Paragraph(body.to_string())],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };
        let project = "mod-project";
        if !anchors.contains(project) {
            let _ = storage.put_document(&make_module_doc(
                project,
                "Project root. Links to every crate/module in the project.",
            ));
        }
        for krate in &modules {
            let module_anchor = format!("mod-{}", krate);
            if !anchors.contains(&module_anchor) {
                let _ = storage.put_document(&make_module_doc(
                    &module_anchor,
                    &format!(
                        "Module {}. Contains the symbols extracted from its source.",
                        krate
                    ),
                ));
            }
            edges.push((
                project.to_string(),
                module_anchor.clone(),
                EdgeType::Documents,
            ));
            edges.push((module_anchor, project.to_string(), EdgeType::PartOf));
        }
    }

    storage.put_edges_bulk(&edges)?;
    storage.flush()?;
    Ok(callee_stats)
}

/// Ensure the store is up to date with the source before a read command serves
/// from it. This is the "fresh by construction" path: a cheap mtime sweep over
/// the gen-cache, and — only if a source file is new or modified — a quiet
/// incremental `gen` (which skips unchanged files and re-links edges). When
/// nothing changed it is just stat calls, so queries stay fast while never
/// serving stale context. Deletions are intentionally ignored here (they only
/// leave harmless orphans); `aden heal . --gc` reclaims those.
///
/// Best-effort: any error degrades to serving the existing store rather than
/// failing the read.
pub fn ensure_fresh(path: &Path) {
    use std::time::UNIX_EPOCH;

    let root = find_project_root(path);
    // No store yet → build it now. Read commands are store-first, so a fresh
    // project must be indexed on first query (this is what makes asm/ask/locate
    // work without an explicit `aden gen`).
    let (existing_store, _) = aden_paths::resolve_read_store(&root);
    if !existing_store.exists() {
        let _ = cmd_gen_silent(&root);
        return;
    }

    let cache = load_gen_cache(&aden_paths::gen_cache_file(&root));
    let sources = match discover_source_files(&root) {
        Ok(s) => s,
        Err(_) => return,
    };

    // The newest source mtime gen has already seen. Comparing against this —
    // rather than requiring every discovered file to be present in the cache —
    // avoids perpetual staleness from files that are discovered but never
    // cached (e.g. unsupported languages that fail to parse). A file newer than
    // anything gen knew about is genuinely new or modified.
    let newest_known = cache
        .entries
        .values()
        .map(|e| e.source_mtime)
        .max()
        .unwrap_or(0);

    let stale = sources.iter().any(|src| {
        let mtime = src
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        mtime > newest_known
    });

    if stale {
        // Silent incremental regen: re-parses only changed files and re-links
        // edges, without printing anything (this runs transparently on reads).
        let _ = cmd_gen_silent(&root);
    }
}

/// Auto-document a codebase: discover source files, skip unchanged,
/// emit structured contracts to store, and optionally to disk.
/// Compile source into the store.
///
/// Two independent verbosity axes:
/// - `quiet`  — suppress the per-file "Stored <anchor>" progress lines but
///   still print the one-line summary. This is what `--quiet`/`regen` want
///   ("summary only").
/// - `silent` — suppress EVERYTHING, including the summary and parse warnings.
///   This is the transparent refresh-on-read path (`ensure_fresh`), which must
///   never write to stdout/stderr during `ask`/`query`/`grep`/etc.
pub fn cmd_gen(path: &Path, quiet: bool) -> Result<(), Box<dyn std::error::Error>> {
    cmd_gen_inner(path, quiet, false, false, false)
}

/// Persist one review *notice* per guarded change into `.aden/proposals/`,
/// reusing the existing `aden_propose` pipeline. A notice records that a symbol
/// carrying durable overlay intent had its generated content updated, so the
/// author re-reviews the overlay; it is informational (the store was already
/// updated and the overlay preserved). Ids are deterministic
/// (`overlay-review-<sanitized-anchor>`) so the same change overwrites the same
/// file rather than accumulating. Returns the number written.
fn write_merge_proposals(
    root: &Path,
    conflicts: &[(String, aden_core::contract::MergeProposal)],
) -> usize {
    use crate::commands::overlay;
    use std::fmt::Write as _;

    let mut written = 0usize;
    for (anchor, proposal) in conflicts {
        let slug = overlay::sanitize_anchor_filename(anchor);
        let mut patch = String::new();
        let _ = writeln!(patch, "// Overlay review notice for {anchor}");
        for action in &proposal.actions {
            if let aden_core::contract::MergeAction::Conflict { reason, .. } = action {
                let _ = writeln!(patch, "// CHANGED: {reason}");
            }
        }
        let _ = writeln!(
            patch,
            "//\n// The generated layer was updated and your overlay was preserved.\n// Re-check that your intent still holds: .aden/overlays/{slug}.adoc"
        );

        let prop = aden_propose::Proposal {
            id: format!("overlay-review-{slug}"),
            target_path: overlay::overlay_path(root, anchor),
            drift_type: "OverlayReview".to_string(),
            confidence: 0.5,
            status: aden_propose::ProposalStatus::PendingReview,
            rationale: format!(
                "Generated content for {anchor} changed while a durable [human]/[agent] overlay annotates it; store updated, overlay preserved — re-review the annotation."
            ),
            patch_asciidoc: patch,
        };
        if aden_propose::persist(&prop, root).is_ok() {
            written += 1;
        }
    }
    written
}

/// `gen` with the three-way-merge flags exposed on the CLI.
///
/// * `propose` — dry-run: reconcile and write conflict proposals, but never
///   mutate the store.
/// * `force` — bypass the merge gate and overwrite the store unconditionally
///   (emergency escape hatch; can clobber `[human]`/`[agent]` overlay collisions).
pub fn cmd_gen_opts(
    path: &Path,
    quiet: bool,
    propose: bool,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    cmd_gen_inner(path, quiet, false, propose, force)
}

/// Fully-silent variant for the auto-refresh path (see `ensure_fresh`).
pub fn cmd_gen_silent(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    cmd_gen_inner(path, true, true, false, false)
}

fn cmd_gen_inner(
    path: &Path,
    quiet: bool,
    silent: bool,
    propose: bool,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !path.exists() {
        return Err("Path does not exist or is not a file/directory".into());
    }

    // Project root: for a single file, search upward from its directory.
    let search_start = if path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    let root = find_project_root(search_start);

    // Store-first: `gen` writes ONLY to .aden/store. Module hub nodes
    // (mod-project, mod-<crate>) are synthesized into the store by
    // link_store_edges — no .adoc files or contracts/ directory are emitted.
    {
        // A single file re-indexes just itself; a directory indexes the project.
        let sources = if path.is_file() {
            vec![path.to_path_buf()]
        } else {
            discover_source_files(&root)?
        };
        if sources.is_empty() {
            eprintln!(
                "No source files discovered in {}. Is this a supported project?",
                root.display()
            );
            return Ok(());
        }

        // ADR-003: the store now lives in the per-user data dir, keyed per
        // project. Refuse to create one at $HOME / fs-root unless explicit, then
        // migrate any legacy in-tree store before opening the central one.
        aden_paths::guard_creatable_root(&root, crate::util::creation_explicit())?;
        crate::util::migrate_legacy_store(&root);
        let store_path = aden_paths::store_dir(&root);
        if let Some(parent) = store_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create store dir {}: {}", parent.display(), e))?;
        }
        let storage = Storage::new(
            store_path
                .to_str()
                .expect("Store path should be valid UTF-8"),
        )
        .map_err(|e| format!("Failed to open store at {}: {}", store_path.display(), e))?;
        let _ = aden_paths::write_meta(&root);

        let cache_path = aden_paths::gen_cache_file(&root);
        let mut cache = load_gen_cache(&cache_path);
        let mut generated = Vec::new();
        let mut skipped = 0usize;

        // Anchors that have an intent overlay on disk. Computed once: only these
        // symbols can produce a merge conflict, so the gate skips the per-symbol
        // store read + overlay parse for every other symbol. Empty when there is
        // no `.aden/overlays/` directory (the common case → zero overhead).
        let overlay_slugs = crate::commands::overlay::overlay_slugs(&root);

        // Phase 1: Parallel file processing — read, parse, write to store
        let work_items: Vec<_> = sources
            .par_iter()
            .filter_map(|src_path| {
                let src_mtime = src_path
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                let mtime_secs = src_mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                // Check mtime cache — if match, return Skip
                let src_rel = src_path.strip_prefix(&root).unwrap_or(src_path);
                // Security floor: never index credential material into the store
                // (where ask/asm would assemble it into LLM context). Search
                // (grep/locate/audit) can still see these files to fix them.
                if aden_core::filter::is_secret_path(src_rel) {
                    return None;
                }
                let cache_key = src_rel.to_string_lossy().to_string();
                // `--force-regen` and `--propose` must re-examine every file even
                // when its mtime is unchanged: force needs to overwrite, and the
                // dry-run needs to audit current state against overlays.
                if !force
                    && !propose
                    && let Some(e) = cache.entries.get(&cache_key)
                    && e.source_mtime == mtime_secs
                {
                    return Some(WorkItem::Skip);
                }

                // Read source
                let source = match std::fs::read_to_string(src_path) {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::InvalidData => return None,
                    Err(e) => {
                        if !quiet {
                            eprintln!("WARN: Failed to read {}: {}", src_path.display(), e);
                        }
                        return None;
                    }
                };

                // Security floor (content): the filename-based is_secret_path
                // check above misses a credential embedded in an ordinary source
                // or config file. Scan content for high-confidence secret tokens
                // (AWS/GitHub/OpenAI/Slack keys, PEM private keys) and refuse to
                // index such a file into the store, where ask/asm would otherwise
                // assemble it into LLM context (CWE-798/CWE-200).
                if aden_core::filter::content_has_high_confidence_secret(&source) {
                    if !silent {
                        eprintln!(
                            "WARN: Skipping {} — file content matches a credential pattern (not indexed). Add to .adenignore if intentional.",
                            src_rel.display()
                        );
                    }
                    return None;
                }

                // Parse
                let docs = match aden_parse::parse_file(src_path, &source) {
                    Ok(d) => d,
                    Err(aden_core::Error::UnsupportedLanguage(_)) => return None,
                    Err(e) => {
                        if !silent {
                            eprintln!("WARN: Parse failed for {}: {}", src_path.display(), e);
                        }
                        return None;
                    }
                };

                // Write each document to store
                let mut emitted = Vec::new();
                let mut conflicts: Vec<(String, aden_core::contract::MergeProposal)> = Vec::new();
                for doc in docs {
                    let mut doc_clone = doc.clone();
                    sanitize_source_file(&mut doc_clone, &root);

                    // Capture call sites for graph linking before slimming, then
                    // drop the redundant edge:: listing so the store stays compact.
                    // Real containment/Calls edges are built in link_store_edges,
                    // so the old parent-module relationship boilerplate is gone.
                    let callees = extract_callees(&doc_clone);
                    let uses = extract_uses(&doc_clone);
                    let refs = extract_doc_refs(&doc_clone);
                    slim_doc_for_store(&mut doc_clone);

                    // Three-way merge gate. A conflict can only arise when the
                    // symbol has an overlay, so for everything else we skip the
                    // store read + overlay parse entirely (zero overhead in the
                    // common case). `propose` is a dry-run that never writes;
                    // `force` skips the gate (no notices).
                    //
                    // Semantics are *notify*, not block: the generated layer
                    // always updates so the store never drifts from source, and
                    // the overlay's durable intent is preserved (separate file)
                    // and delivered to readers (folded into the read graph). When
                    // a guarded generated unit changes we record a *notice* so the
                    // author re-reviews their overlay. The notice self-clears on
                    // the next run once the generated content settles.
                    let write = !propose;
                    if !force
                        && overlay_slugs.contains(
                            &crate::commands::overlay::sanitize_anchor_filename(&doc_clone.anchor),
                        )
                    {
                        let stored = storage.get_document(&doc_clone.anchor).ok().flatten();
                        let overlay =
                            crate::commands::overlay::load_overlay(&root, &doc_clone.anchor);
                        if let Ok(p) = aden_core::contract::reconcile_anchor(
                            &doc_clone,
                            stored.as_ref(),
                            overlay.as_ref(),
                        ) && !p.is_clean()
                        {
                            conflicts.push((doc_clone.anchor.clone(), p));
                        }
                    }

                    let mut wrote = false;
                    if write {
                        if let Err(e) = storage.put_document(&doc_clone) {
                            eprintln!("WARN: Failed to store {}: {}", doc_clone.anchor, e);
                            continue;
                        }
                        wrote = true;
                        if !quiet {
                            progress!(quiet, "Stored {}", doc_clone.anchor);
                        }
                    }

                    emitted.push(EmittedSymbol {
                        anchor: doc_clone.anchor.clone(),
                        callees,
                        uses,
                        refs,
                        wrote,
                    });
                }

                // Always report a reindexed file — even with zero symbols — so
                // the prune step can drop anchors a now-empty file used to own.
                Some(WorkItem::Reindexed {
                    cache_key: cache_key.clone(),
                    source_mtime: mtime_secs,
                    source_path: src_path.to_string_lossy().to_string(),
                    symbols: emitted,
                    conflicts,
                })
            })
            .collect();

        // Phase 2: Merge parallel results into shared state. Collect compact
        // (anchor, callees) link records so the linker never reloads documents.
        let mut link_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut use_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut ref_records: Vec<(String, Vec<String>)> = Vec::new();
        // Anchors to prune: symbols a reindexed file no longer defines.
        let mut stale_anchors: Vec<String> = Vec::new();
        // Merge conflicts surfaced by the reconcile gate, written as proposals.
        let mut merge_conflicts: Vec<(String, aden_core::contract::MergeProposal)> = Vec::new();
        for item in work_items {
            match item {
                WorkItem::Skip => skipped += 1,
                WorkItem::Reindexed {
                    cache_key,
                    source_mtime,
                    source_path,
                    symbols,
                    conflicts,
                } => {
                    merge_conflicts.extend(conflicts);
                    let fresh: Vec<String> = symbols.iter().map(|s| s.anchor.clone()).collect();
                    // Diff against what this file contributed last time: any
                    // previously-recorded anchor not in the fresh set is a
                    // symbol that was deleted/renamed and must be pruned.
                    if let Some(prev) = cache.entries.get(&cache_key) {
                        for old in &prev.anchors {
                            if !fresh.contains(old) {
                                stale_anchors.push(old.clone());
                            }
                        }
                    }
                    cache.entries.insert(
                        cache_key,
                        GenCacheEntry {
                            source_mtime,
                            source_path,
                            anchors: fresh,
                        },
                    );
                    for sym in symbols {
                        // Count only symbols actually written to the store, so the
                        // summary never claims to have stored a conflict-held doc.
                        if sym.wrote {
                            generated.push(sym.anchor.clone());
                        }
                        if !sym.refs.is_empty() {
                            ref_records.push((sym.anchor.clone(), sym.refs));
                        }
                        if !sym.uses.is_empty() {
                            use_records.push((sym.anchor.clone(), sym.uses));
                        }
                        if !sym.callees.is_empty() {
                            link_records.push((sym.anchor, sym.callees));
                        }
                    }
                }
            }
        }

        // Dry-run: never mutate the store (no prune, link, flush, or cache
        // save). Write conflict proposals and report, then stop.
        if propose {
            let written = write_merge_proposals(&root, &merge_conflicts);
            progress!(
                silent,
                "gen --propose: {} annotated symbol(s) would change → {} review notice(s) in .aden/proposals/. No store changes written.",
                merge_conflicts.len(),
                written
            );
            return Ok(());
        }

        // Case (b): whole-file deletion. On a full-tree gen (NOT a single-file
        // re-index, which only knows about one file), any cache entry whose
        // source file is no longer in the discovered set is gone — prune all
        // anchors it owned and drop the entry.
        if !path.is_file() {
            let live: std::collections::HashSet<String> = sources
                .iter()
                .map(|p| {
                    p.strip_prefix(&root)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .to_string()
                })
                .collect();
            let dead_keys: Vec<String> = cache
                .entries
                .keys()
                .filter(|k| !live.contains(*k))
                .cloned()
                .collect();
            for k in dead_keys {
                if let Some(entry) = cache.entries.remove(&k) {
                    stale_anchors.extend(entry.anchors);
                }
            }
        }

        // Prune stale nodes (deleted symbols / deleted files). delete_node
        // cascades edges in both directions so no dangling reference survives.
        // Guard: never touch synthesized hub nodes (mod-*) — they carry no
        // source_file and are rebuilt by link_store_edges below.
        let mut pruned = 0usize;
        for anchor in &stale_anchors {
            if anchor.starts_with("mod-") {
                continue;
            }
            match storage.delete_node(anchor) {
                Ok(()) => pruned += 1,
                Err(e) => {
                    if !silent {
                        eprintln!("WARN: Failed to prune {}: {}", anchor, e);
                    }
                }
            }
        }

        // Flush store to persist all documents
        storage
            .flush()
            .map_err(|e| format!("Store flush failed: {}", e))?;

        // Connect the graph: persist module<->symbol containment and call edges
        // so the store-first graph used by asm/ask/query is actually traversable.
        let callee_stats =
            match link_store_edges(&storage, &link_records, &use_records, &ref_records) {
                Ok(stats) => stats,
                Err(e) => {
                    eprintln!("WARN: Failed to link graph edges: {}", e);
                    CalleeStats::default()
                }
            };

        save_gen_cache(&cache_path, &cache)?;

        // The summary is "summary only" output: shown under --quiet/regen, but
        // suppressed entirely on the silent refresh-on-read path.
        if pruned > 0 {
            progress!(
                silent,
                "\nStored {} contracts. Skipped {} unchanged files. Pruned {} stale symbol(s).",
                generated.len(),
                skipped,
                pruned
            );
        } else {
            progress!(
                silent,
                "\nStored {} contracts. Skipped {} unchanged files.",
                generated.len(),
                skipped
            );
        }
        if skipped == 0 && generated.len() == sources.len() {
            progress!(
                silent,
                "(All files were skipped — nothing changed since last run)"
            );
        }

        // Notices: a guarded symbol's generated content changed while a durable
        // overlay annotates it. The store was updated (no drift) and the overlay
        // is preserved + delivered; the notice asks the author to re-review.
        if !merge_conflicts.is_empty() {
            let written = write_merge_proposals(&root, &merge_conflicts);
            progress!(
                silent,
                "{} annotated symbol(s) changed → {} review notice(s) in .aden/proposals/ (your overlay intent is preserved; re-check it in .aden/overlays/).",
                merge_conflicts.len(),
                written
            );
        }

        // Call-graph resolution health. Dropped call sites (unresolved/ambiguous
        // callees) are exactly where the graph thins out, so surface the counts
        // on the same summary channel — quiet/regen still shows it, the silent
        // refresh-on-read path stays silent. Only emit when something dropped to
        // keep the clean case uncluttered. Counts are per callee reference, not
        // per built edge (self-calls / collapsed targets inflate `resolved`).
        if callee_stats.unresolved > 0 || callee_stats.ambiguous > 0 {
            progress!(
                silent,
                "Call graph: {} internal calls linked, {} external (stdlib/other crate — no \
                 project symbol), {} polymorphic (name defined in several places, e.g. \
                 new/from/trait methods — left unlinked to avoid false edges).",
                callee_stats.resolved,
                callee_stats.unresolved,
                callee_stats.ambiguous
            );
        }

        // Report orphan symbols using store-first graph build. Suppressed in
        // quiet mode so the transparent refresh-on-read path stays silent.
        if !quiet {
            match AdenGraph::build_from_storage(&storage) {
                Ok(graph) => {
                    let orphans = graph.orphans();
                    if !orphans.is_empty() {
                        eprintln!("\nWARNING: {} orphan symbol(s) detected:", orphans.len());
                        for orphan in orphans.iter().take(5) {
                            eprintln!("  - {}", orphan);
                        }
                        if orphans.len() > 5 {
                            eprintln!("  ... and {} more", orphans.len() - 5);
                        }
                        eprintln!("  Run 'aden heal . --gc' to auto-link or remove orphans");
                    }
                }
                Err(e) => {
                    eprintln!("Note: Could not check for orphans: {}", e);
                }
            }
        }
    }

    // Invalidate caches after generating so the next query rebuilds
    let cache_dir = aden_paths::cache_dir(path);
    if cache_dir.exists() {
        let _ = std::fs::remove_dir_all(&cache_dir);
        let _ = std::fs::create_dir_all(&cache_dir);
    }

    Ok(())
}

#[cfg(test)]
mod link_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn anchor_file_extracts_path() {
        assert_eq!(
            anchor_file("aden://module/aden-parse/rust.rs#node_text"),
            Some("aden-parse/rust.rs")
        );
        assert_eq!(anchor_file("aden://doc/x/y.md/h1foo"), None);
    }

    #[test]
    fn resolve_unique_callee_links() {
        let mut idx: HashMap<&str, Vec<&str>> = HashMap::new();
        idx.insert("foo", vec!["aden://module/c/a.rs#foo"]);
        assert_eq!(
            resolve_callee("foo", "aden://module/c/b.rs#bar", &idx),
            Some("aden://module/c/a.rs#foo")
        );
    }

    #[test]
    fn enclosing_qualified_preserves_member_separator() {
        // Python dot-methods.
        assert_eq!(
            enclosing_qualified("aden://module/p/core.py#Command.main", "invoke"),
            Some("Command.invoke".to_string())
        );
        // Rust path-methods.
        assert_eq!(
            enclosing_qualified("aden://module/c/s.rs#FjallStorage::open", "flush"),
            Some("FjallStorage::flush".to_string())
        );
        // A free function has no enclosing type → None (self can't appear).
        assert_eq!(
            enclosing_qualified("aden://module/c/s.rs#free_fn", "x"),
            None
        );
    }

    #[test]
    fn strip_self_receiver_recognizes_instance_calls() {
        assert_eq!(strip_self_receiver("self.run"), Some("run"));
        assert_eq!(strip_self_receiver("this.run"), Some("run"));
        assert_eq!(strip_self_receiver("$this->run"), Some("run"));
        assert_eq!(strip_self_receiver("Self::run"), Some("run"));
        assert_eq!(strip_self_receiver("other.run"), None);
        assert_eq!(strip_self_receiver("run"), None);
        // Field hop is recognized as self-receiver but rejected by is_plain_ident.
        assert!(!is_plain_ident(
            strip_self_receiver("self.inner.run").unwrap()
        ));
        assert!(is_plain_ident(strip_self_receiver("self.run").unwrap()));
    }

    #[test]
    fn resolve_self_call_targets_callers_own_type() {
        // `Command.invoke` and a test class's `invoke` collide on the base name,
        // so base matching is ambiguous; the self path must pick the caller's own
        // type precisely.
        let mut idx: HashMap<&str, Vec<&str>> = HashMap::new();
        idx.insert(
            "Command.invoke",
            vec!["aden://module/p/core.py#Command.invoke"],
        );
        idx.insert(
            "invoke",
            vec![
                "aden://module/p/core.py#Command.invoke",
                "aden://module/p/test_commands.py#OptParseCommand.invoke",
            ],
        );
        assert_eq!(
            resolve_callee("self.invoke", "aden://module/p/core.py#Command.main", &idx),
            Some("aden://module/p/core.py#Command.invoke")
        );
        // A self-call to a method the enclosing type does NOT define (e.g.
        // inherited) must not be force-linked here; it falls through to base
        // resolution, which stays ambiguous and yields None.
        assert_eq!(
            resolve_callee("self.missing", "aden://module/p/core.py#Command.main", &idx),
            None
        );
    }

    #[test]
    fn ambiguous_callee_prefers_same_file() {
        let mut idx: HashMap<&str, Vec<&str>> = HashMap::new();
        idx.insert(
            "node_text",
            vec![
                "aden://module/aden-parse/rust.rs#node_text",
                "aden://module/aden-parse/tree_sitter_common.rs#node_text",
                "aden://module/aden-cli/x.rs#node_text",
            ],
        );
        // Caller in rust.rs → the rust.rs copy wins (same file), not the shared one.
        assert_eq!(
            resolve_callee(
                "node_text",
                "aden://module/aden-parse/rust.rs#extract_struct",
                &idx
            ),
            Some("aden://module/aden-parse/rust.rs#node_text")
        );
    }

    #[test]
    fn ambiguous_callee_falls_back_to_same_crate_then_gives_up() {
        let mut idx: HashMap<&str, Vec<&str>> = HashMap::new();
        idx.insert(
            "helper",
            vec![
                "aden://module/crate-a/x.rs#helper",
                "aden://module/crate-b/y.rs#helper",
            ],
        );
        // Different file but same crate → same-crate wins.
        assert_eq!(
            resolve_callee("helper", "aden://module/crate-a/z.rs#caller", &idx),
            Some("aden://module/crate-a/x.rs#helper")
        );
        // Caller in a third crate → genuinely ambiguous, do not guess.
        assert_eq!(
            resolve_callee("helper", "aden://module/crate-c/z.rs#caller", &idx),
            None
        );
    }

    #[test]
    fn classify_drop_zero_candidates_is_unresolved() {
        // Empty index: nothing matches the callee at all.
        let idx: HashMap<&str, Vec<&str>> = HashMap::new();
        assert!(
            matches!(classify_drop("nonexistent", &idx), DropReason::Unresolved),
            "a callee with zero candidates must be Unresolved"
        );
        // A name present but unrelated to the callee is still zero candidates.
        let mut idx2: HashMap<&str, Vec<&str>> = HashMap::new();
        idx2.insert(
            "something_else",
            vec!["aden://module/c/a.rs#something_else"],
        );
        assert!(matches!(
            classify_drop("missing", &idx2),
            DropReason::Unresolved
        ));
    }

    #[test]
    fn classify_drop_multiple_full_name_candidates_is_ambiguous() {
        // The full callee name itself resolves to >= 2 candidates.
        let mut idx: HashMap<&str, Vec<&str>> = HashMap::new();
        idx.insert(
            "helper",
            vec![
                "aden://module/crate-a/x.rs#helper",
                "aden://module/crate-b/y.rs#helper",
            ],
        );
        assert!(
            matches!(classify_drop("helper", &idx), DropReason::Ambiguous),
            ">=2 candidates for the full name must be Ambiguous"
        );
    }

    #[test]
    fn classify_drop_trailing_segment_with_multiple_candidates_is_ambiguous() {
        // Full qualified name has no candidates, but its trailing segment
        // (after '.' / ':') matches >= 2 — mirrors resolve_callee's fallback.
        let mut idx: HashMap<&str, Vec<&str>> = HashMap::new();
        idx.insert(
            "node_text",
            vec![
                "aden://module/aden-parse/rust.rs#node_text",
                "aden://module/aden-parse/python.rs#node_text",
            ],
        );
        // Dotted receiver call: `self.node_text` → base `node_text`. The receiver
        // is a value of unknown type, so we cannot call it external — the base
        // collides internally → Ambiguous.
        assert!(
            matches!(classify_drop("self.node_text", &idx), DropReason::Ambiguous),
            "receiver call whose base has >=2 candidates must be Ambiguous"
        );
        // Path-qualified call to a type NOT in the index: `Parser::node_text`
        // where `Parser` is external → the method-name collision is incidental,
        // so this is External (Unresolved), not a true internal ambiguity.
        assert!(
            matches!(
                classify_drop("Parser::node_text", &idx),
                DropReason::Unresolved
            ),
            "static call to an unknown type must be External/Unresolved"
        );
    }

    #[test]
    fn classify_drop_static_call_to_known_project_type_is_ambiguous() {
        // `Parser` IS a project type and `node_text` collides across files → a
        // genuine internal ambiguity the locality heuristic couldn't break.
        let mut idx: HashMap<&str, Vec<&str>> = HashMap::new();
        idx.insert("Parser", vec!["aden://module/aden-parse/rust.rs#Parser"]);
        idx.insert(
            "node_text",
            vec![
                "aden://module/aden-parse/rust.rs#node_text",
                "aden://module/aden-parse/python.rs#node_text",
            ],
        );
        assert!(matches!(
            classify_drop("Parser::node_text", &idx),
            DropReason::Ambiguous
        ));
        // `Vec::new`-style external constructor colliding with project `new`s →
        // External, not ambiguous (the headline reclassification).
        idx.insert(
            "new",
            vec![
                "aden://module/a/x.rs#Foo::new",
                "aden://module/b/y.rs#Bar::new",
            ],
        );
        assert!(matches!(
            classify_drop("Vec::new", &idx),
            DropReason::Unresolved
        ));
    }

    #[test]
    fn extract_uses_reads_edge_uses_listings() {
        use aden_core::{Block, Document, NodeType};
        let doc = Document {
            anchor: "x".into(),
            node_type: NodeType::Function,
            attributes: Default::default(),
            blocks: vec![Block::Listing {
                language: None,
                code: "edge::uses[EmittedSymbol]\nedge::uses[DocumentNode]".into(),
            }],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };
        // sorted + deduped
        assert_eq!(
            extract_uses(&doc),
            vec!["DocumentNode".to_string(), "EmittedSymbol".to_string()]
        );
    }
}
