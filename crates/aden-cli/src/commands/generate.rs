// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_graph::graph::AdenGraph;
use aden_store::{GraphStorage, Storage};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::Path;

use crate::types::GenCacheEntry;
use crate::util::{
    cochange_pairs, discover_source_files, extract_callees, extract_demonstrates,
    extract_doc_includes, extract_doc_mentions, extract_doc_refs, extract_doc_supersedes,
    extract_doc_terms, extract_edge_macro, extract_uses, find_project_root, load_gen_cache,
    sanitize_source_file, save_gen_cache,
};

/// The per-kind edge record slices `link_store_edges` writes, bundled into one
/// argument. Every record is the same shape (anchor -> target list); the field
/// name picks the EdgeType. `cochange` carries git co-change pairs and
/// `test_anchors` the set of test-symbol anchors (callers whose `Calls` are
/// reclassified as `Tests`).
struct EdgeRecords<'a> {
    calls: &'a [(String, Vec<String>)],
    uses: &'a [(String, Vec<String>)],
    refs: &'a [(String, Vec<String>)],
    implements: &'a [(String, Vec<String>)],
    mutates: &'a [(String, Vec<String>)],
    mentions: &'a [(String, Vec<String>)],
    supersedes: &'a [(String, Vec<String>)],
    demonstrates: &'a [(String, Vec<String>)],
    terms: &'a [(String, Vec<String>)],
    cochange: &'a [crate::types::CochangePair],
    test_anchors: &'a std::collections::HashSet<String>,
}

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
    /// Prose cross-references (`ref:<fragment>` entries from the parser-filled
    /// `doc_refs` attribute — AsciiDoc `<<target>>`/`xref:`, markdown
    /// `[text](#frag)`). Linked as bidirectional `RelatesTo` edges against doc
    /// anchor fragments only.
    refs: Vec<String>,
    /// `include::` document-composition targets (raw file paths from the
    /// `doc_includes` attribute). Linked as directional `Requires` edges.
    includes: Vec<String>,
    /// `edge::implements[Trait::method]` references (trait impls) — linked as
    /// `Implements` edges so blast radius reaches implementors (Wave 1).
    implements: Vec<String>,
    /// `edge::mutates[Type]` references (`&mut self` receivers) — linked as
    /// `Mutates` edges from a method to the type whose state it writes.
    mutates: Vec<String>,
    /// Backtick symbol names from the parser-filled `doc_mentions` attribute
    /// (prose only — the parsers' fence state keeps listings out). Linked as
    /// `Mentions` edges when the name resolves to exactly one code symbol
    /// (Wave 2).
    mentions: Vec<String>,
    /// Supersede-context refs (`<by|of>:ref:<frag>` entries from the
    /// parser-filled `doc_supersedes` attribute — a cross-reference on a line
    /// with supersede language). Linked as directed NEW —Supersedes→ OLD
    /// edges against doc anchor fragments only (Wave 3).
    supersedes: Vec<String>,
    /// `kind:name` entries from the parser-filled `symbol_references`
    /// attribute on doc code listings. Linked as `Demonstrates` edges under
    /// the same unambiguous-only rule (Wave 2).
    demonstrates: Vec<String>,
    /// Full `aden://term/…` anchors from the parser-filled `doc_terms`
    /// attribute on glossary sections. Linked as `DefinesTerm` edges by exact
    /// anchor match (the parser constructed both ends, so no fuzzing).
    defines_terms: Vec<String>,
    /// Whether this symbol's generated document was actually written to the
    /// store. False when the merge gate held it back (overlay conflict) or on a
    /// dry-run — so the summary count reflects real writes, not just processing.
    wrote: bool,
}

/// Work item returned from parallel file processing.
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
        /// Slimmed documents to write to the store. Carried out of the parallel
        /// pass so the actual `put_document` happens *sequentially in sorted
        /// source order* in Phase 2 — otherwise two files sharing a basename
        /// anchor (e.g. `openapi2/helpers.go` + `openapi3/helpers.go` →
        /// `helpers.go#copyURI`) race, and the collision winner (hence the index)
        /// is non-deterministic. Empty on a `--propose` dry-run.
        docs: Vec<aden_core::Document>,
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
fn module_from_anchor(anchor: &str) -> Option<String> {
    let rest = anchor.strip_prefix("aden://module/")?;
    let path = rest.split('#').next()?;
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    // The first segment is always the project/crate name.
    // Previously this used segs[n-2] (penultimate segment), which worked when
    // file_name was a bare basename but broke with project-relative paths like
    // `aden-cli/src/commands/heal.rs` where n-2 would give "commands".
    let name = segs.first().copied()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Resolve `include::` directives (the `doc_includes` channel) into directional
/// `Requires` edges: an including document depends on each file it pulls in.
/// Resolution is file-wise by stem (an include names a file, not a fragment),
/// pointing at that file's representative (first) doc node. Runs as a separate
/// additive pass after `link_store_edges`; `put_edges_bulk` appends, so edges
/// already written are kept.
fn link_include_edges<S: GraphStorage>(
    storage: &S,
    include_records: &[(String, Vec<String>)],
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_core::EdgeType;
    if include_records.is_empty() {
        return Ok(());
    }
    let anchors = storage.get_all_anchors()?;
    // File stem -> declaring doc anchors (sorted/deduped for deterministic picks).
    let mut stem_index: HashMap<&str, Vec<&str>> = HashMap::new();
    for anchor in &anchors {
        if let Some(file_key) = doc_anchor_file(anchor)
            && let Some(stem) = std::path::Path::new(file_key)
                .file_stem()
                .and_then(|s| s.to_str())
        {
            stem_index.entry(stem).or_default().push(anchor.as_str());
        }
    }
    for cands in stem_index.values_mut() {
        cands.sort_unstable();
        cands.dedup();
    }
    let mut edges: Vec<(String, String, EdgeType)> = Vec::new();
    for (referrer, targets) in include_records {
        let ref_file = doc_anchor_file(referrer);
        for target in targets {
            let Some(stem) = std::path::Path::new(target)
                .file_stem()
                .and_then(|s| s.to_str())
            else {
                continue;
            };
            let Some(cands) = stem_index.get(stem) else {
                continue;
            };
            // An include points at ANOTHER document: prefer a candidate in a
            // different file than the referrer; else the first (deterministic).
            let target_anchor = cands
                .iter()
                .copied()
                .find(|a| doc_anchor_file(a) != ref_file)
                .or_else(|| cands.first().copied());
            if let Some(ta) = target_anchor
                && ta != referrer.as_str()
            {
                edges.push((referrer.clone(), ta.to_string(), EdgeType::Requires));
            }
        }
    }
    if !edges.is_empty() {
        storage.put_edges_bulk(&edges)?;
    }
    Ok(())
}

/// True when a doc anchor belongs to an Architecture Decision Record: any
/// path segment of the `aden://doc/…` anchor is `adr` (an `adr/` directory)
/// or starts with `adr-` (an `adr-NNN-…` file or `[[adr-NNN]]` fragment).
/// ADR-ness licenses the `Justifies` reclassification — only decision
/// records justify code; ordinary docs merely mention it.
fn is_adr_doc_anchor(anchor: &str) -> bool {
    let Some(rest) = anchor.strip_prefix("aden://doc/") else {
        return false;
    };
    rest.split(['/', '#'])
        .any(|seg| seg == "adr" || seg.starts_with("adr-"))
}

/// Slim a document before storing it. Drops the `edge::calls[...]` listing
/// block — it is redundant with the `Callee` table for display and is no longer
/// needed for linking (callees are carried out of the parse phase directly), so
/// storing it just bloats the (already large) store on big repos.
/// `pub(crate)`: heal's merge reconciliation re-parses source to build the
/// `ground` layer and must apply the same slimming, or every reconcile sees
/// phantom diffs against the slimmed store/base.
pub(crate) fn slim_doc_for_store(doc: &mut aden_core::Document) {
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
    let caller_crate = module_from_anchor(caller);
    let pick = |cands: &[&'a str]| pick_local(cands, caller_file, caller_crate.as_deref());
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

/// Disambiguate candidate anchors by locality: a unique candidate wins
/// outright; among several, prefer the caller's own file, then its crate;
/// otherwise give up (no edge beats a wrong edge). Extracted from
/// `resolve_callee` so the Wave-1 `Implements`/`Mutates` resolution shares
/// exactly the same tiebreak.
fn pick_local<'a>(
    cands: &[&'a str],
    caller_file: Option<&str>,
    caller_crate: Option<&str>,
) -> Option<&'a str> {
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
            let cc = caller_crate?;
            let same: Vec<&'a str> = many
                .iter()
                .copied()
                .filter(|a| module_from_anchor(a).as_deref() == Some(cc))
                .collect();
            if same.len() == 1 { Some(same[0]) } else { None }
        }
    }
}

/// Minimum name length for the Wave-2 prose-mention / listing-reference
/// channels (mirrors the parsers' extraction floor — short generic names like
/// `new`/`get`/`run` would flood the graph with wrong edges).
const UNAMBIGUOUS_NAME_FLOOR: usize = 4;

/// Strictest resolver in the linker, for the Wave-2 `Mentions`/`Demonstrates`
/// channels: a name links only when it resolves to exactly ONE code symbol.
/// No locality tiebreak — prose and listings have no meaningful "caller
/// locality", so a collision is genuine ambiguity and stays unlinked (the
/// false-positive guard validated in the prose-mention-autolink research).
/// Candidates are filtered to code-backed module anchors first, so a doc
/// heading that happens to share the name cannot create ambiguity.
fn resolve_unambiguous_code<'a>(
    name: &str,
    name_index: &HashMap<&str, Vec<&'a str>>,
) -> Option<&'a str> {
    let lookup = |key: &str| -> Option<&'a str> {
        if key.len() < UNAMBIGUOUS_NAME_FLOOR {
            return None;
        }
        let mut code_cands = name_index
            .get(key)?
            .iter()
            .copied()
            .filter(|a| is_code_module_anchor(a));
        let first = code_cands.next()?;
        // Qualified names index under both the full key and the trailing
        // segment, so the same anchor can appear twice — only a DIFFERENT
        // second anchor is real ambiguity.
        code_cands.all(|a| a == first).then_some(first)
    };
    // Exact name first (`Type::method` is an index key), then the trailing
    // member segment for qualified forms.
    lookup(name).or_else(|| {
        let segment = name.rsplit(['.', ':']).next()?;
        (segment != name).then(|| lookup(segment)).flatten()
    })
}

/// True for module anchors backed by CODE (not prose): doc files also emit
/// `aden://module/...` anchors (e.g. `…/guide.adoc#document`), which must not
/// be Mentions/Demonstrates targets.
fn is_code_module_anchor(anchor: &str) -> bool {
    let Some(rest) = anchor.strip_prefix("aden://module/") else {
        return false;
    };
    let path = rest.split('#').next().unwrap_or(rest);
    let lower = path.to_lowercase();
    !(lower.ends_with(".md")
        || lower.ends_with(".adoc")
        || lower.ends_with(".rst")
        || lower.ends_with(".txt")
        || lower.ends_with(".aden"))
}

/// Exact-name resolution with the locality tiebreak — deliberately NO
/// trailing-segment fallback (unlike `resolve_callee`): for `Implements`/
/// `Mutates` targets a bare-method fallback could forge an edge to an
/// unrelated same-named symbol, which is worse than no edge.
fn resolve_exact<'a>(
    name: &str,
    caller: &str,
    name_index: &HashMap<&str, Vec<&'a str>>,
) -> Option<&'a str> {
    let cands = name_index.get(name)?;
    pick_local(
        cands,
        anchor_file(caller),
        module_from_anchor(caller).as_deref(),
    )
}

/// Resolve an `edge::implements[...]` target, emitted by the parser as
/// `Trait::method` (possibly path-scoped: `path::Trait::method`). Preference
/// order — method-level edges are strictly better for blast radius, but only
/// when the trait method is actually a stored symbol:
/// 1. the full qualified name (`Trait::method`) — today trait-body method
///    declarations are not extracted as symbols, so this is forward-compat;
/// 2. the trait itself (the qualified name minus its `::method` segment);
/// 3. the trait's bare name (last path segment) for scoped references.
///
/// Every step is an EXACT lookup via [`resolve_exact`]; an external trait
/// (`fmt::Display`) simply resolves nowhere and produces no edge.
fn resolve_implements<'a>(
    target: &str,
    caller: &str,
    name_index: &HashMap<&str, Vec<&'a str>>,
) -> Option<&'a str> {
    if let Some(hit) = resolve_exact(target, caller, name_index) {
        return Some(hit);
    }
    let trait_part = &target[..target.rfind("::")?];
    if trait_part.is_empty() {
        return None;
    }
    if let Some(hit) = resolve_exact(trait_part, caller, name_index) {
        return Some(hit);
    }
    let bare = trait_part.rsplit("::").next().unwrap_or(trait_part);
    if bare != trait_part && !bare.is_empty() {
        return resolve_exact(bare, caller, name_index);
    }
    None
}

/// The file portion of a module anchor: `aden://module/<file>#<sym>` → `<file>`
/// (e.g. `aden-parse/rust.rs`). Used to scope ambiguous-callee resolution to the
/// caller's own file. Returns `None` for non-module anchors (docs, etc.).
fn anchor_file(anchor: &str) -> Option<&str> {
    anchor.strip_prefix("aden://module/")?.split('#').next()
}

/// The file portion of a DOC anchor, for both stored shapes:
/// `aden://doc/<proj>/<file>#<frag>` (inline `[[frag]]` declarations) and
/// `aden://doc/<proj>/<file>/<frag>` (heading `h<level><slug>` / sectanchors
/// `_slug` forms) → `<proj>/<file>`. Returns `None` for non-doc anchors.
/// Shared with ask-routing (`query.rs`), which groups in-band prose candidates
/// by file and promotes thin section anchors to their file's canonical anchor.
pub(crate) fn doc_anchor_file(anchor: &str) -> Option<&str> {
    let rest = anchor.strip_prefix("aden://doc/")?;
    if let Some(h) = rest.find('#') {
        return Some(&rest[..h]);
    }
    rest.rfind('/').map(|s| &rest[..s])
}

/// Resolve a prose cross-reference fragment (`ref:<frag>`, prefix already
/// stripped) to a doc anchor — format-neutrally, by EXACT match against the
/// fragment indexes built from doc anchors. Never consults the code-symbol
/// index: `resolve_callee`'s bare-name fallbacks could attach a doc ref to a
/// same-named code symbol, forging a doc→code edge out of prose.
///
/// Two tiers, never mixed (ADR: `<<anchor>>` is a GLOBAL reference to the
/// file that DECLARES `[[anchor]]`):
/// 1. exact anchor ids (`frags`) — inline `[[frag]]` declarations plus the
///    literal heading/sectanchors fragments (`h2slug`, `_slug`);
/// 2. derived bare heading slugs (`slugs`) — consulted only when no exact id
///    exists, so markdown `[text](#my-heading)` still resolves but a derived
///    slug can never shadow an explicit declaration.
///
/// Anchors are globally unique by contract, so a fragment declared in TWO
/// docs is the author's lint problem, not a crash: prefer a declaration in
/// the referrer's own file, else pick deterministically (candidate lists are
/// sorted; the first wins — same spirit as `pick_local`, but a collision
/// still yields an edge rather than dropping the reference).
fn resolve_doc_ref<'a>(
    frag: &str,
    referrer: &str,
    frags: &HashMap<&str, Vec<&'a str>>,
    slugs: &HashMap<&str, Vec<&'a str>>,
) -> Option<&'a str> {
    let cands = frags.get(frag).or_else(|| slugs.get(frag))?;
    match cands.as_slice() {
        [] => None,
        [one] => Some(one),
        many => {
            if let Some(rf) = doc_anchor_file(referrer)
                && let Some(same) = many.iter().find(|a| doc_anchor_file(a) == Some(rf))
            {
                return Some(same);
            }
            Some(many[0])
        }
    }
}

fn resolve_doc_file_ref<'a>(
    target: &str,
    referrer: &str,
    files: &HashMap<String, Vec<&'a str>>,
) -> Option<&'a str> {
    let stem = std::path::Path::new(target)
        .file_stem()
        .and_then(|s| s.to_str())?;
    let cands = files.get(stem)?;
    let ref_file = doc_anchor_file(referrer);
    cands
        .iter()
        .copied()
        .find(|a| doc_anchor_file(a) != ref_file)
        .or_else(|| cands.first().copied())
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
/// - Calls: each resolved callee becomes a `Calls` edge. When the CALLER is a
///   test symbol (`test_anchors`), the same resolved call is ALSO emitted as a
///   `Tests` edge — reclassification, not new analysis (graph-type roadmap
///   Wave 1). The `Calls` edge is kept so call-graph consumers see no change.
/// - Implements: `edge::implements[Trait::method]` records become implementor
///   --Implements--> trait(-method) edges, so the Incoming blast-radius
///   traversal from a changed trait reaches its implementors.
/// - Mutates: `edge::mutates[Type]` records (`&mut self` receivers) become
///   method --Mutates--> parent-type edges.
/// - Mentions (Wave 2): backtick prose mentions (`doc_mentions` records)
///   become doc --Mentions--> symbol edges, only when the name resolves to
///   exactly ONE code symbol.
/// - Demonstrates (Wave 2): `symbol_references` records on doc code listings
///   become listing --Demonstrates--> symbol edges, same unambiguous-only rule.
fn link_store_edges<S: GraphStorage>(
    storage: &S,
    records: EdgeRecords<'_>,
) -> Result<CalleeStats, Box<dyn std::error::Error>> {
    // Destructure back into the original per-kind bindings so the linking body
    // below is unchanged; only the call surface collapsed to one struct.
    let EdgeRecords {
        calls: link_records,
        uses: use_records,
        refs: ref_records,
        implements: impl_records,
        mutates: mutates_records,
        mentions: mention_records,
        supersedes: supersede_records,
        demonstrates: demo_records,
        terms: term_records,
        cochange: cochange_pairs,
        test_anchors,
    } = records;
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
        } else if anchor.starts_with("aden://doc/") {
            // Doc section anchors carry no `#` — they end in `/<fragment>`
            // (e.g. `…/guide.adoc/_configuration` or `…/guide.adoc/h2configuration`).
            // Index by that trailing fragment so an `xref:file.adoc#_configuration`
            // resolves to the section node. Asciidoctor `:sectanchors:` aliases
            // (`_slug`) and aden's own `h<level>slug` form both become resolvable
            // targets this way.
            if let Some(slash) = anchor.rfind('/') {
                let fragment = &anchor[slash + 1..];
                if !fragment.is_empty() {
                    name_index
                        .entry(fragment)
                        .or_default()
                        .push(anchor.as_str());
                }
            }
        }
    }

    // Prose cross-reference indexes: doc anchor FRAGMENT -> declaring doc
    // anchors. Strictly doc-side (never code symbols) so `ref:` records cannot
    // leak into the code-symbol fuzzy path. Two tiers (see `resolve_doc_ref`):
    // - `doc_frag_index` (exact anchor ids): the part after `#` for inline
    //   `[[frag]]` declarations (`aden://doc/<proj>/<file>#<frag>`) and the
    //   part after the last `/` for heading/sectanchors forms
    //   (`…/<file>/h2slug`, `…/<file>/_slug`);
    // - `doc_slug_index` (derived): a heading fragment `h<level><slug>` is
    //   additionally indexed by its bare slug so markdown `[text](#my-heading)`
    //   and AsciiDoc `<<my-heading>>` heading refs resolve — but only as a
    //   FALLBACK, so a derived slug never shadows an explicit `[[anchor]]`
    //   declaration (the ADR's global-anchor contract).
    // Candidate lists are sorted for deterministic collision picks.
    let mut doc_frag_index: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut doc_slug_index: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut doc_file_index: HashMap<String, Vec<&str>> = HashMap::new();
    for anchor in &anchors {
        let Some(rest) = anchor.strip_prefix("aden://doc/") else {
            continue;
        };
        if let Some(file_key) = doc_anchor_file(anchor)
            && let Some(stem) = std::path::Path::new(file_key)
                .file_stem()
                .and_then(|s| s.to_str())
        {
            doc_file_index
                .entry(stem.to_string())
                .or_default()
                .push(anchor.as_str());
        }
        let fragment = match rest.find('#') {
            Some(h) => &rest[h + 1..],
            None => match rest.rfind('/') {
                Some(s) => &rest[s + 1..],
                None => continue,
            },
        };
        // Skip the symbol-linked doc form whose "fragment" embeds a full code
        // anchor (`aden://doc/<file>#aden://module/...`): not a prose target.
        if fragment.is_empty() || fragment.contains("://") {
            continue;
        }
        doc_frag_index
            .entry(fragment)
            .or_default()
            .push(anchor.as_str());
        if !rest.contains('#')
            && let Some(numbered) = fragment.strip_prefix('h')
        {
            let slug = numbered.trim_start_matches(|c: char| c.is_ascii_digit());
            if slug.len() < numbered.len() && !slug.is_empty() {
                doc_slug_index
                    .entry(slug)
                    .or_default()
                    .push(anchor.as_str());
            }
        }
    }
    for cands in doc_frag_index
        .values_mut()
        .chain(doc_slug_index.values_mut())
    {
        cands.sort_unstable();
        cands.dedup();
    }
    for cands in doc_file_index.values_mut() {
        cands.sort_unstable();
        cands.dedup();
    }

    let mut edges: Vec<(String, String, EdgeType)> = Vec::new();
    let mut modules: HashSet<String> = HashSet::new();

    // Containment for every anchor.
    for anchor in &anchors {
        if let Some(module) = module_from_anchor(anchor) {
            let module_anchor = format!("mod-{}", module);
            modules.insert(module);
            edges.push((module_anchor.clone(), anchor.clone(), EdgeType::Contains));
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
                    // Wave 1: a resolved call FROM a test symbol is also a
                    // `Tests` edge (test → tested symbol). Emitted in addition
                    // to — never instead of — the `Calls` edge.
                    if test_anchors.contains(anchor.as_str()) {
                        edges.push((anchor.clone(), target.to_string(), EdgeType::Tests));
                    }
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

    // Implements edges (Wave 1): implementor method —Implements→ trait (or
    // trait method, when that is a stored symbol). Direction matters: the
    // blast-radius traversal walks Incoming edges, so a change to a trait
    // reaches its implementors' methods — closing the silent truncation
    // across trait-object dispatch.
    for (anchor, targets) in impl_records {
        for t in targets {
            if let Some(target) = resolve_implements(t, anchor, &name_index)
                && target != anchor.as_str()
            {
                edges.push((anchor.clone(), target.to_string(), EdgeType::Implements));
            }
        }
    }

    // Mutates edges (Wave 1): a `&mut self` method —Mutates→ its parent type.
    // Exact resolution only — a fuzzy fallback could attach the edge to an
    // unrelated same-named type.
    for (anchor, targets) in mutates_records {
        for t in targets {
            if let Some(target) = resolve_exact(t, anchor, &name_index)
                && target != anchor.as_str()
            {
                edges.push((anchor.clone(), target.to_string(), EdgeType::Mutates));
            }
        }
    }

    // Prose cross-reference edges (`ref:<fragment>` records from the parsers'
    // `<<target>>` / `xref:` / `[text](#frag)` extraction). Resolved ONLY via
    // the doc-fragment index — `resolve_callee` is off-limits here because its
    // bare-name fallbacks would attach a prose ref to a same-named code
    // symbol. Bidirectional so backlinks work (a doc and what it references
    // are mutually reachable).
    for (anchor, refs) in ref_records {
        for r in refs {
            let target = if let Some(frag) = r.strip_prefix("ref:") {
                resolve_doc_ref(frag, anchor, &doc_frag_index, &doc_slug_index)
            } else if let Some(file) = r.strip_prefix("file:") {
                resolve_doc_file_ref(file, anchor, &doc_file_index)
            } else {
                None
            };
            if let Some(target) = target
                && target != anchor.as_str()
            {
                edges.push((anchor.clone(), target.to_string(), EdgeType::RelatesTo));
                edges.push((target.to_string(), anchor.clone(), EdgeType::RelatesTo));
            }
        }
    }

    // Mentions edges (Wave 2): unmarked prose that names a symbol in backticks
    // gets a doc —Mentions→ code edge — deliberately weaker than `Documents`
    // so the intentional-contract signal stays undiluted. Precision over
    // recall: only names that resolve to exactly ONE code symbol link;
    // ambiguous and unknown names stay unlinked rather than guessing.
    for (anchor, names) in mention_records {
        // Justifies (Wave 3): an ADR's mention of a symbol is not casual prose —
        // it is the decision record naming what it decided about. Co-emitted
        // alongside Mentions (the Tests-alongside-Calls reclassification
        // pattern) so "why is this here" becomes a one-hop traversal while
        // Mentions consumers see no change.
        let from_adr = is_adr_doc_anchor(anchor);
        for name in names {
            if let Some(target) = resolve_unambiguous_code(name, &name_index)
                && target != anchor.as_str()
            {
                edges.push((anchor.clone(), target.to_string(), EdgeType::Mentions));
                if from_adr {
                    edges.push((anchor.clone(), target.to_string(), EdgeType::Justifies));
                }
            }
        }
    }

    // Supersedes edges (Wave 3, episodic layer): a cross-reference on a line
    // with supersede language becomes a directed NEW —Supersedes→ OLD edge.
    // The parser recorded which side the enclosing doc is on (`by:` = passive
    // "superseded by X", so the referenced doc is the new one; `of:` = active
    // "supersedes X", so the enclosing doc is). Resolution is doc-side only,
    // same as `ref_records` — and the plain bidirectional RelatesTo from the
    // same line is kept, so backlinks still work; Supersedes adds direction.
    for (anchor, entries) in supersede_records {
        for entry in entries {
            let Some((dir, r)) = entry.split_once(':') else {
                continue;
            };
            let Some(frag) = r.strip_prefix("ref:") else {
                continue;
            };
            if let Some(target) = resolve_doc_ref(frag, anchor, &doc_frag_index, &doc_slug_index)
                && target != anchor.as_str()
            {
                if dir == "by" {
                    edges.push((target.to_string(), anchor.clone(), EdgeType::Supersedes));
                } else {
                    edges.push((anchor.clone(), target.to_string(), EdgeType::Supersedes));
                }
            }
        }
    }

    // Demonstrates edges (Wave 2): a doc code listing that references a symbol
    // demonstrates it — turning `code_block_*` anchors from orphan noise into
    // the answer to "show me a working example of X". Records are `kind:name`
    // entries (declaration scan + neutral call-token scan); the same
    // unambiguous-only rule applies.
    for (anchor, entries) in demo_records {
        for entry in entries {
            let name = entry.split_once(':').map(|(_, n)| n).unwrap_or(entry);
            if let Some(target) = resolve_unambiguous_code(name, &name_index)
                && target != anchor.as_str()
            {
                edges.push((anchor.clone(), target.to_string(), EdgeType::Demonstrates));
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

    // DefinesTerm edges (Wave 2 remainder): glossary section —DefinesTerm→
    // term node. Both anchors were constructed by the same parser pass, so
    // this is exact-match only — a record whose term node is missing from the
    // store (e.g. held back by the merge gate) simply emits no edge.
    for (anchor, targets) in term_records {
        for t in targets {
            if anchor_set.contains(t.as_str()) && t != anchor {
                edges.push((anchor.clone(), t.clone(), EdgeType::DefinesTerm));
            }
        }
    }

    // AssociatedWith edges (Wave 3, episodic layer): file-level git co-change
    // — the Hebbian "files that change together belong together" signal.
    // Pairs were thresholded upstream ([`cochange_pairs`]). The store has no
    // file-level nodes (symbols hang off `mod-<crate>` hubs), so the file
    // node is synthesized here for co-change participants only — the same
    // pattern as the module-hub synthesis below, bounded by the thresholded
    // pair count. Both directions are emitted (the type is bidirectional-feel
    // by definition). Deliberately NOT in `impact_edge_types`: co-change is
    // advisory association, not structural dependency, and must never
    // inflate a blast radius.
    let mut synthesized_files: HashSet<String> = HashSet::new();
    for ((a, fa), (b, fb)) in cochange_pairs {
        for (anchor, file) in [(a, fa), (b, fb)] {
            if !anchor_set.contains(anchor.as_str()) && !synthesized_files.contains(anchor) {
                let mut attrs = HashMap::new();
                if !file.is_empty() {
                    attrs.insert("source_file".to_string(), file.clone());
                }
                let _ = storage.put_document(&Document {
                    anchor: anchor.clone(),
                    node_type: NodeType::Module,
                    attributes: attrs,
                    blocks: vec![Block::Paragraph(format!(
                        "Source file {}. Co-change hub: linked to the files that \
                         historically change together with it.",
                        if file.is_empty() {
                            anchor.as_str()
                        } else {
                            file.as_str()
                        }
                    ))],
                    source_span: None,
                    metadata: None,
                    confidence: 1.0,
                });
                synthesized_files.insert(anchor.clone());
            }
        }
        edges.push((a.clone(), b.clone(), EdgeType::AssociatedWith));
        edges.push((b.clone(), a.clone(), EdgeType::AssociatedWith));
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
        for module in &modules {
            let module_anchor = format!("mod-{}", module);
            if !anchors.contains(&module_anchor) {
                let _ = storage.put_document(&make_module_doc(
                    &module_anchor,
                    &format!(
                        "Module {}. Contains the symbols extracted from its source.",
                        module
                    ),
                ));
            }
            edges.push((
                project.to_string(),
                module_anchor.clone(),
                EdgeType::Contains,
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
/// Rebuild the store from source IF (and only if) it exists on disk but is in a
/// storage-engine format this build cannot read (e.g. after a fjall upgrade).
///
/// Returns `true` when a rebuild was triggered. Unlike [`ensure_fresh`], this
/// does NOT regenerate on mere mtime staleness — it fires solely on the
/// format-mismatch signal. That distinction matters for `heal`, whose whole job
/// is to observe drift between source and the *current* store: a staleness-gen
/// before a heal scan would reconcile the very drift heal is meant to surface,
/// but a store in an unreadable format carries no usable baseline to drift from,
/// so rebuilding it first is correct (and leaves heal a readable store).
pub(crate) fn recover_if_incompatible_store(path: &Path) -> bool {
    let root = find_project_root(path);
    let (store, _) = aden_paths::resolve_read_store(&root);
    if store.exists()
        && let Some(store_str) = store.to_str()
        && matches!(
            Storage::open_existing(store_str),
            Err(aden_store::StoreError::IncompatibleVersion(_))
        )
    {
        // Recovery is best-effort, but its OUTCOME must be honest: return `true`
        // only when the rebuild actually succeeded. A failure here means the
        // store is still unreadable — most concretely a pinned/shared
        // `$ADEN_STORE`, which `cmd_gen_inner` refuses to auto-wipe and so
        // returns `Err`. Returning `true` regardless would make callers
        // (`ensure_fresh`, heal) treat the store as recovered, skip their own
        // logic, and silently degrade to empty results. Surface the error so the
        // user understands why, and return `false` so callers do not assume
        // success.
        return match cmd_gen_silent(&root) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("aden: could not rebuild the incompatible store: {e}");
                false
            }
        };
    }
    false
}

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

    // The store exists but may be unreadable by this build — e.g. it was written
    // by an older storage-engine format and this binary was just upgraded. The
    // mtime freshness check below would see an up-to-date tree and skip the
    // rebuild, leaving the read to open an incompatible store and degrade to
    // empty results. Recover on the format-mismatch signal (and ONLY that), so
    // the read auto-recovers with zero user action.
    if recover_if_incompatible_store(&root) {
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

/// Lock the store for a write phase (G4). Held for the whole of `gen` so two
/// concurrent runs against one store cannot interleave their writes and corrupt
/// it — parallel-agent driving makes that concurrency real. Waits up to 10
/// minutes for a live holder; a dead holder is reclaimed immediately (process
/// liveness on Linux), so this blocks only on a genuinely active gen. The
/// lockfile is a sibling of the store directory.
fn acquire_store_lock(store_path: &Path) -> std::io::Result<aden_core::lock::FileLock> {
    aden_core::lock::FileLock::acquire_timeout(
        store_path.with_extension("lock"),
        std::time::Duration::from_secs(600),
    )
}

#[cfg(test)]
mod store_lock_tests {
    use super::*;

    #[test]
    fn gen_store_lock_is_exclusive_and_releases() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("store");

        let held = acquire_store_lock(&store_path).expect("first gen takes the lock");
        // A second writer on the same store's sibling lockfile is blocked.
        let blocked = aden_core::lock::FileLock::acquire_timeout(
            store_path.with_extension("lock"),
            std::time::Duration::from_millis(100),
        );
        assert_eq!(
            blocked.expect_err("second writer must block").kind(),
            std::io::ErrorKind::WouldBlock
        );

        drop(held);
        // Once the first writer finishes, the store is acquirable again.
        aden_core::lock::FileLock::acquire_timeout(
            store_path.with_extension("lock"),
            std::time::Duration::from_millis(100),
        )
        .expect("lock is free after release");
    }
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
        let mut sources = if path.is_file() {
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
        // G4: serialize concurrent writers. Held until this function returns,
        // covering the whole open/index/flush write phase.
        let _store_lock = acquire_store_lock(&store_path).map_err(|e| {
            format!(
                "another aden gen is writing the store at {}: {e}",
                store_path.display()
            )
        })?;
        let store_str = store_path
            .to_str()
            .expect("Store path should be valid UTF-8");
        // A store that does not exist yet — first gen, a manual `rm -rf store/`,
        // or a `regen` wipe — is empty: its incremental gen-cache, if any, is now
        // stale and must be ignored, or every file would be skipped as
        // "unchanged" against an empty store and the rebuild would silently
        // produce nothing (the recovery-from-deletion trap).
        let mut full_rebuild = force || !store_path.exists();
        let storage = match Storage::new(store_str) {
            Ok(s) => s,
            Err(aden_store::StoreError::IncompatibleVersion(_)) => {
                // The on-disk store was written in an engine format this build
                // cannot read (e.g. a fjall major upgrade). The store is a
                // rebuildable cache (ADR-003), so wipe it and rebuild from
                // source. Scoped strictly to this signal — a generic Io error
                // falls through to the hard-failure arm below and never wipes.
                //
                // SAFETY: a pinned/shared `$ADEN_STORE` may hold several projects'
                // data; never auto-wipe shared state. Surface an actionable error
                // and let the user decide (mirrors `regen`'s pinned-store
                // caution). The default per-project store is safe to wipe.
                if std::env::var_os("ADEN_STORE").is_some() {
                    return Err(format!(
                        "Store at {} is in an incompatible engine format, and $ADEN_STORE \
                         is pinned/shared — refusing to auto-wipe shared state. Unset \
                         $ADEN_STORE for a per-project store, or run `aden regen` to rebuild.",
                        store_path.display()
                    )
                    .into());
                }
                progress!(
                    silent,
                    "Store format changed (engine upgrade) — rebuilding from source."
                );
                let _ = std::fs::remove_dir_all(&store_path);
                full_rebuild = true;
                // The wipe destroyed the WHOLE store, so a single-file `gen` must
                // now repopulate the whole project — otherwise the rebuild would
                // re-index just that one file and leave a near-empty graph.
                if path.is_file() {
                    sources = discover_source_files(&root)?;
                }
                Storage::new(store_str).map_err(|e| {
                    format!("Failed to rebuild store at {}: {}", store_path.display(), e)
                })?
            }
            Err(e) => {
                return Err(
                    format!("Failed to open store at {}: {}", store_path.display(), e).into(),
                );
            }
        };
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
        let mut work_items: Vec<_> = sources
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
                // `--force-regen`/`--propose` and a full rebuild (new/empty/
                // recovered store) must re-examine every file even when its mtime
                // is unchanged: force needs to overwrite, the dry-run needs to
                // audit current state against overlays, and a full rebuild has an
                // empty store whose "unchanged" cache entries no longer reflect
                // any stored contract.
                if !full_rebuild
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

                // Parse documents; the actual store write is deferred to Phase 2
                // (sequential, sorted) so basename-anchor collisions resolve
                // deterministically instead of racing across worker threads.
                let mut emitted = Vec::new();
                let mut conflicts: Vec<(String, aden_core::contract::MergeProposal)> = Vec::new();
                let mut docs_local: Vec<aden_core::Document> = Vec::new();
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
                    let includes = extract_doc_includes(&doc_clone);
                    let implements = extract_edge_macro(&doc_clone, "implements");
                    let mutates = extract_edge_macro(&doc_clone, "mutates");
                    let mentions = extract_doc_mentions(&doc_clone);
                    let supersedes = extract_doc_supersedes(&doc_clone);
                    let demonstrates = extract_demonstrates(&doc_clone);
                    let defines_terms = extract_doc_terms(&doc_clone);
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
                        let overlay = crate::commands::overlay::load_overlay(
                            &root,
                            &doc_clone.anchor,
                        )
                        .ok()
                        .flatten();
                        if let Ok(p) = aden_core::contract::reconcile_anchor(
                            &doc_clone,
                            stored.as_ref(),
                            overlay.as_ref(),
                        ) && !p.is_clean()
                        {
                            conflicts.push((doc_clone.anchor.clone(), p));
                        }
                    }

                    emitted.push(EmittedSymbol {
                        anchor: doc_clone.anchor.clone(),
                        callees,
                        uses,
                        refs,
                        includes,
                        implements,
                        mutates,
                        mentions,
                        supersedes,
                        demonstrates,
                        defines_terms,
                        wrote: write,
                    });
                    // Defer the write — Phase 2 stores these in sorted source order.
                    if write {
                        docs_local.push(doc_clone);
                    }
                }

                // Always report a reindexed file — even with zero symbols — so
                // the prune step can drop anchors a now-empty file used to own.
                Some(WorkItem::Reindexed {
                    cache_key: cache_key.clone(),
                    source_mtime: mtime_secs,
                    source_path: src_path.to_string_lossy().to_string(),
                    symbols: emitted,
                    docs: docs_local,
                    conflicts,
                })
            })
            .collect();

        // Phase 2: Merge parallel results into shared state. Collect compact
        // (anchor, callees) link records so the linker never reloads documents.
        let mut link_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut use_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut ref_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut include_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut impl_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut mutates_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut mention_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut supersede_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut demo_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut term_records: Vec<(String, Vec<String>)> = Vec::new();
        // Anchors whose SOURCE FILE is a test/spec file (conventional path
        // markers, shared with ask-routing's `is_test_result`). Their resolved
        // calls are additionally emitted as `Tests` edges. Lookup-only — never
        // iterated — so it cannot perturb emission order (determinism).
        let mut test_anchors: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Anchors to prune: symbols a reindexed file no longer defines.
        let mut stale_anchors: Vec<String> = Vec::new();
        // Merge conflicts surfaced by the reconcile gate, written as proposals.
        let mut merge_conflicts: Vec<(String, aden_core::contract::MergeProposal)> = Vec::new();
        // Deterministic store writes: order work by source path so that when two
        // files share a basename anchor, the collision winner is the same every run
        // (last sorted source wins) — making the store, and the index built from it,
        // reproducible. The parallel pass deferred every `put_document` to here.
        fn work_key(w: &WorkItem) -> &str {
            match w {
                WorkItem::Reindexed { source_path, .. } => source_path.as_str(),
                WorkItem::Skip => "",
            }
        }
        work_items.sort_by(|a, b| work_key(a).cmp(work_key(b)));
        for item in work_items {
            match item {
                WorkItem::Skip => skipped += 1,
                WorkItem::Reindexed {
                    cache_key,
                    source_mtime,
                    source_path,
                    symbols,
                    docs,
                    conflicts,
                } => {
                    for d in &docs {
                        if let Err(e) = storage.put_document(d) {
                            eprintln!("WARN: Failed to store {}: {}", d.anchor, e);
                            continue;
                        }
                        // Record the canonical contract text as the base snapshot
                        // for three-way merges.  The snapshot is the
                        // `emit_contract_document` output for the exact document
                        // written above (already slimmed by slim_doc_for_store);
                        // `parse_contract` is its exact inverse so the round-trip
                        // is lossless.
                        let snapshot = aden_emit::emit_contract_document(
                            &aden_core::contract::ContractDocument::from_document(d),
                        );
                        if let Err(e) = storage.put_base_snapshot(&d.anchor, &snapshot) {
                            eprintln!(
                                "WARN: Failed to record base snapshot for {}: {}",
                                d.anchor, e
                            );
                        }
                    }
                    merge_conflicts.extend(conflicts);
                    // The module-form anchor flattens the directory, so
                    // test-ness comes from the real (root-relative) source
                    // path — the same rule ask-routing applies at query time.
                    let from_test_file = crate::commands::query::is_test_source_path(&cache_key);
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
                        if !sym.includes.is_empty() {
                            include_records.push((sym.anchor.clone(), sym.includes));
                        }
                        if !sym.uses.is_empty() {
                            use_records.push((sym.anchor.clone(), sym.uses));
                        }
                        if !sym.implements.is_empty() {
                            impl_records.push((sym.anchor.clone(), sym.implements));
                        }
                        if !sym.mutates.is_empty() {
                            mutates_records.push((sym.anchor.clone(), sym.mutates));
                        }
                        if !sym.mentions.is_empty() {
                            mention_records.push((sym.anchor.clone(), sym.mentions));
                        }
                        if !sym.supersedes.is_empty() {
                            supersede_records.push((sym.anchor.clone(), sym.supersedes));
                        }
                        if !sym.demonstrates.is_empty() {
                            demo_records.push((sym.anchor.clone(), sym.demonstrates));
                        }
                        if !sym.defines_terms.is_empty() {
                            term_records.push((sym.anchor.clone(), sym.defines_terms));
                        }
                        if from_test_file {
                            test_anchors.insert(sym.anchor.clone());
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
        let cochange = cochange_pairs(&root, &cache);
        let callee_stats = match link_store_edges(
            &storage,
            EdgeRecords {
                calls: &link_records,
                uses: &use_records,
                refs: &ref_records,
                implements: &impl_records,
                mutates: &mutates_records,
                mentions: &mention_records,
                supersedes: &supersede_records,
                demonstrates: &demo_records,
                terms: &term_records,
                cochange: &cochange,
                test_anchors: &test_anchors,
            },
        ) {
            Ok(stats) => stats,
            Err(e) => {
                eprintln!("WARN: Failed to link graph edges: {}", e);
                CalleeStats::default()
            }
        };

        // Additive second pass: include:: directives -> Requires edges. Kept
        // separate from link_store_edges (which works from anchors only); this
        // resolves file-wise and put_edges_bulk appends, so prior edges persist.
        if let Err(e) = link_include_edges(&storage, &include_records) {
            eprintln!("WARN: Failed to link include edges: {}", e);
        }

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
        // New format: file component is the full project-relative path,
        // so the anchor now looks like aden://module/{project}/{rel/path}#symbol.
        // anchor_file strips "aden://module/" and the "#..." fragment, returning
        // the rest — which is "{project}/{rel/path}".
        assert_eq!(
            anchor_file("aden://module/aden-parse/src/rust.rs#node_text"),
            Some("aden-parse/src/rust.rs")
        );
        // Old bare-basename format still works (backward compat; no '#' or extra '/'):
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

    /// Wave 1 (`Implements`) resolution order: method-level when the trait
    /// method is a stored symbol, else the trait itself, else the bare trait
    /// name of a scoped path — and NEVER a bare-method fallback (which would
    /// forge an edge to an unrelated same-named symbol).
    #[test]
    fn resolve_implements_prefers_method_then_trait_never_bare_method() {
        let caller = "aden://module/c/greeter.rs#English::greet";
        let mut idx: HashMap<&str, Vec<&str>> = HashMap::new();
        idx.insert("Greeter", vec!["aden://module/c/greeter.rs#Greeter"]);
        // Trait method not stored → trait-level fallback.
        assert_eq!(
            resolve_implements("Greeter::greet", caller, &idx),
            Some("aden://module/c/greeter.rs#Greeter")
        );
        // Trait method stored → method-level wins.
        idx.insert(
            "Greeter::greet",
            vec!["aden://module/c/greeter.rs#Greeter::greet"],
        );
        assert_eq!(
            resolve_implements("Greeter::greet", caller, &idx),
            Some("aden://module/c/greeter.rs#Greeter::greet")
        );
        // Scoped reference resolves through the bare trait name.
        let mut idx2: HashMap<&str, Vec<&str>> = HashMap::new();
        idx2.insert("Extractor", vec!["aden://module/c/x.rs#Extractor"]);
        assert_eq!(
            resolve_implements("parse::Extractor::run", caller, &idx2),
            Some("aden://module/c/x.rs#Extractor")
        );
        // External trait (`Display::fmt`): nothing resolves → no edge.
        assert_eq!(
            resolve_implements("Display::fmt", caller, &HashMap::new()),
            None
        );
        // CRITICAL: a same-named method elsewhere must not be picked up.
        let mut idx3: HashMap<&str, Vec<&str>> = HashMap::new();
        idx3.insert("greet", vec!["aden://module/c/other.rs#Other::greet"]);
        assert_eq!(resolve_implements("Greeter::greet", caller, &idx3), None);
    }

    /// Wave 1 (`Tests`): a resolved call whose source anchor is in the
    /// test-anchor set is emitted as BOTH a `Calls` and a `Tests` edge; a
    /// non-test caller emits `Calls` only. Exercised through the real store
    /// so the bulk write path is covered too.
    #[test]
    fn test_callers_emit_tests_edges_alongside_calls() {
        use aden_core::{Block, Document, EdgeType, NodeType};
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path().to_str().unwrap()).unwrap();
        let mk = |anchor: &str| Document {
            anchor: anchor.into(),
            node_type: NodeType::Function,
            attributes: Default::default(),
            blocks: vec![Block::Paragraph("body".into())],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };
        let target = "aden://module/c/lib.rs#target";
        let test_fn = "aden://module/c/parity.rs#test_target";
        let caller = "aden://module/c/lib.rs#caller";
        for a in [target, test_fn, caller] {
            storage.put_document(&mk(a)).unwrap();
        }
        storage.flush().unwrap();

        let link_records = vec![
            (test_fn.to_string(), vec!["target".to_string()]),
            (caller.to_string(), vec!["target".to_string()]),
        ];
        // test-ness comes from the SOURCE PATH at collection time (the anchor
        // alone can hide it), modeled here by membership in the set.
        let test_anchors: std::collections::HashSet<String> =
            std::collections::HashSet::from([test_fn.to_string()]);
        link_store_edges(
            &storage,
            EdgeRecords {
                calls: &link_records,
                uses: &[],
                refs: &[],
                implements: &[],
                mutates: &[],
                mentions: &[],
                supersedes: &[],
                demonstrates: &[],
                terms: &[],
                cochange: &[],
                test_anchors: &test_anchors,
            },
        )
        .unwrap();

        let out_test = storage.get_outgoing_edges(test_fn).unwrap();
        assert!(
            out_test.contains(&(target.to_string(), EdgeType::Calls)),
            "the Calls edge must be kept (reclassify ADDS, never replaces); got {out_test:?}"
        );
        assert!(
            out_test.contains(&(target.to_string(), EdgeType::Tests)),
            "a test source's resolved call must also be a Tests edge; got {out_test:?}"
        );
        let out_caller = storage.get_outgoing_edges(caller).unwrap();
        assert!(
            out_caller.contains(&(target.to_string(), EdgeType::Calls)),
            "non-test caller keeps its Calls edge; got {out_caller:?}"
        );
        assert!(
            !out_caller.iter().any(|(_, et)| *et == EdgeType::Tests),
            "non-test caller must NOT gain a Tests edge; got {out_caller:?}"
        );
    }

    /// Wave 1 (`Implements`/`Mutates`) through the store: impl/mutates
    /// records become edges with the documented direction.
    #[test]
    fn implements_and_mutates_records_become_edges() {
        use aden_core::{Block, Document, EdgeType, NodeType};
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path().to_str().unwrap()).unwrap();
        let mk = |anchor: &str| Document {
            anchor: anchor.into(),
            node_type: NodeType::Type,
            attributes: Default::default(),
            blocks: vec![Block::Paragraph("body".into())],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };
        let trait_a = "aden://module/c/greeter.rs#Greeter";
        let ty = "aden://module/c/greeter.rs#English";
        let method = "aden://module/c/greeter.rs#English::greet";
        for a in [trait_a, ty, method] {
            storage.put_document(&mk(a)).unwrap();
        }
        storage.flush().unwrap();

        let impl_records = vec![(method.to_string(), vec!["Greeter::greet".to_string()])];
        let mutates_records = vec![(method.to_string(), vec!["English".to_string()])];
        link_store_edges(
            &storage,
            EdgeRecords {
                calls: &[],
                uses: &[],
                refs: &[],
                implements: &impl_records,
                mutates: &mutates_records,
                mentions: &[],
                supersedes: &[],
                demonstrates: &[],
                terms: &[],
                cochange: &[],
                test_anchors: &std::collections::HashSet::new(),
            },
        )
        .unwrap();

        let out = storage.get_outgoing_edges(method).unwrap();
        assert!(
            out.contains(&(trait_a.to_string(), EdgeType::Implements)),
            "implementor method —Implements→ trait (trait-level fallback); got {out:?}"
        );
        assert!(
            out.contains(&(ty.to_string(), EdgeType::Mutates)),
            "&mut self method —Mutates→ parent type; got {out:?}"
        );
    }

    /// Prose `ref:` records resolve format-neutrally against DOC anchor
    /// fragments only — bidirectional `RelatesTo` — and must NEVER attach to a
    /// same-named code symbol (the fuzzy code path is off-limits to prose).
    #[test]
    fn include_records_link_requires_edges() {
        use aden_core::{Block, Document, EdgeType, NodeType};
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path().to_str().unwrap()).unwrap();
        let mk = |anchor: &str| Document {
            anchor: anchor.into(),
            node_type: NodeType::Module,
            attributes: Default::default(),
            blocks: vec![Block::Paragraph("body".into())],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };
        // A master doc and a chapter it includes, each a heading-section node.
        let master = "aden://doc/p/master.adoc/h1master";
        let chapter = "aden://doc/p/chapter-one.adoc/h1chapter";
        for a in [master, chapter] {
            storage.put_document(&mk(a)).unwrap();
        }
        storage.flush().unwrap();

        let include_records = vec![(master.to_string(), vec!["chapter-one.adoc".to_string()])];
        link_include_edges(&storage, &include_records).unwrap();

        let out = storage.get_outgoing_edges(master).unwrap();
        assert!(
            out.contains(&(chapter.to_string(), EdgeType::Requires)),
            "master must Requires the included chapter doc node; got {out:?}"
        );
        // Directional: the chapter does not Requires the master.
        let back = storage.get_outgoing_edges(chapter).unwrap();
        assert!(
            !back.iter().any(|(t, _)| t == master),
            "include edge must be directional (master->chapter only); got {back:?}"
        );
    }

    #[test]
    fn prose_ref_records_link_doc_anchors_only() {
        use aden_core::{Block, Document, EdgeType, NodeType};
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path().to_str().unwrap()).unwrap();
        let mk = |anchor: &str| Document {
            anchor: anchor.into(),
            node_type: NodeType::Module,
            attributes: Default::default(),
            blocks: vec![Block::Paragraph("body".into())],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };
        // A glossary term (inline-anchor doc form), a SAME-NAMED code symbol,
        // a heading-form doc node, and the referencing post section.
        let term = "aden://doc/p/glossary.adoc#term";
        let code_term = "aden://module/c/lib.rs#term";
        let heading = "aden://doc/p/guide.md/h2setup-guide";
        let post = "aden://doc/p/post.adoc/h2intro";
        let code_only = "aden://module/c/lib.rs#onlycode";
        for a in [term, code_term, heading, post, code_only] {
            storage.put_document(&mk(a)).unwrap();
        }
        storage.flush().unwrap();

        let ref_records = vec![(
            post.to_string(),
            vec![
                "ref:term".to_string(),
                "ref:setup-guide".to_string(),
                "ref:onlycode".to_string(),
                "ref:missing_anchor".to_string(),
            ],
        )];
        link_store_edges(
            &storage,
            EdgeRecords {
                calls: &[],
                uses: &[],
                refs: &ref_records,
                implements: &[],
                mutates: &[],
                mentions: &[],
                supersedes: &[],
                demonstrates: &[],
                terms: &[],
                cochange: &[],
                test_anchors: &std::collections::HashSet::new(),
            },
        )
        .unwrap();

        let out = storage.get_outgoing_edges(post).unwrap();
        assert!(
            out.contains(&(term.to_string(), EdgeType::RelatesTo)),
            "post must RelatesTo the glossary term doc node; got {out:?}"
        );
        assert!(
            out.contains(&(heading.to_string(), EdgeType::RelatesTo)),
            "a heading slug ref (#setup-guide) must resolve to the h2 doc node; got {out:?}"
        );
        assert!(
            !out.iter().any(|(t, _)| t == code_term || t == code_only),
            "a prose ref must NEVER link to a code symbol; got {out:?}"
        );
        // Bidirectional: backlink edge from the term to the post.
        let back = storage.get_outgoing_edges(term).unwrap();
        assert!(
            back.contains(&(post.to_string(), EdgeType::RelatesTo)),
            "RelatesTo must be emitted both ways; got {back:?}"
        );
    }

    #[test]
    fn file_level_prose_ref_links_representative_doc_node() {
        use aden_core::{Block, Document, EdgeType, NodeType};
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path().to_str().unwrap()).unwrap();
        let mk = |anchor: &str| Document {
            anchor: anchor.into(),
            node_type: NodeType::Module,
            attributes: Default::default(),
            blocks: vec![Block::Paragraph("body".into())],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };
        let post = "aden://doc/p/post.adoc/h2intro";
        let guide = "aden://doc/p/guide.adoc/h1guide";
        let other = "aden://doc/p/other.adoc/h1other";
        for a in [post, guide, other] {
            storage.put_document(&mk(a)).unwrap();
        }
        storage.flush().unwrap();

        let ref_records = vec![(
            post.to_string(),
            vec![
                "file:guide.adoc".to_string(),
                "file:missing.adoc".to_string(),
            ],
        )];
        link_store_edges(
            &storage,
            EdgeRecords {
                calls: &[],
                uses: &[],
                refs: &ref_records,
                implements: &[],
                mutates: &[],
                mentions: &[],
                supersedes: &[],
                demonstrates: &[],
                terms: &[],
                cochange: &[],
                test_anchors: &std::collections::HashSet::new(),
            },
        )
        .unwrap();

        let out = storage.get_outgoing_edges(post).unwrap();
        assert!(
            out.contains(&(guide.to_string(), EdgeType::RelatesTo)),
            "file-level xref should RelatesTo the target file representative; got {out:?}"
        );
        assert!(
            !out.iter().any(|(t, _)| t == other),
            "file-level xref must not link unrelated files; got {out:?}"
        );
        let back = storage.get_outgoing_edges(guide).unwrap();
        assert!(
            back.contains(&(post.to_string(), EdgeType::RelatesTo)),
            "file-level RelatesTo must be bidirectional; got {back:?}"
        );
    }

    /// Anchor collisions (same fragment declared in two docs) prefer the
    /// referrer's own file, else pick deterministically — never drop or crash.
    #[test]
    fn prose_ref_collision_prefers_same_file_then_deterministic() {
        use aden_core::{Block, Document, EdgeType, NodeType};
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path().to_str().unwrap()).unwrap();
        let mk = |anchor: &str| Document {
            anchor: anchor.into(),
            node_type: NodeType::Module,
            attributes: Default::default(),
            blocks: vec![Block::Paragraph("body".into())],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };
        let in_a = "aden://doc/p/a.adoc#dup";
        let in_b = "aden://doc/p/b.adoc#dup";
        let ref_a = "aden://doc/p/a.adoc/h2intro"; // same file as in_a
        let ref_c = "aden://doc/p/c.adoc/h2intro"; // unrelated file
        for a in [in_a, in_b, ref_a, ref_c] {
            storage.put_document(&mk(a)).unwrap();
        }
        storage.flush().unwrap();

        let ref_records = vec![
            (ref_a.to_string(), vec!["ref:dup".to_string()]),
            (ref_c.to_string(), vec!["ref:dup".to_string()]),
        ];
        link_store_edges(
            &storage,
            EdgeRecords {
                calls: &[],
                uses: &[],
                refs: &ref_records,
                implements: &[],
                mutates: &[],
                mentions: &[],
                supersedes: &[],
                demonstrates: &[],
                terms: &[],
                cochange: &[],
                test_anchors: &std::collections::HashSet::new(),
            },
        )
        .unwrap();

        let out_a = storage.get_outgoing_edges(ref_a).unwrap();
        assert!(
            out_a.contains(&(in_a.to_string(), EdgeType::RelatesTo)),
            "same-file declaration must win the collision; got {out_a:?}"
        );
        assert!(
            !out_a.iter().any(|(t, _)| t == in_b),
            "must not also link the foreign duplicate; got {out_a:?}"
        );
        // No locality signal: deterministic pick (first in sorted anchor order).
        let out_c = storage.get_outgoing_edges(ref_c).unwrap();
        assert!(
            out_c.contains(&(in_a.to_string(), EdgeType::RelatesTo)),
            "collision without locality resolves deterministically (sorted first); got {out_c:?}"
        );
    }

    /// ADR contract: `<<anchor>>` resolves to the file that DECLARES
    /// `[[anchor]]`. An explicit declaration must beat a same-named heading
    /// SLUG (derived, tier-2) everywhere — even when the heading sorts first.
    #[test]
    fn explicit_anchor_declaration_beats_heading_slug() {
        use aden_core::{Block, Document, EdgeType, NodeType};
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path().to_str().unwrap()).unwrap();
        let mk = |anchor: &str| Document {
            anchor: anchor.into(),
            node_type: NodeType::Module,
            attributes: Default::default(),
            blocks: vec![Block::Paragraph("body".into())],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };
        // The derived heading slug sorts BEFORE the explicit declaration, and
        // a same-file heading exists too — neither may shadow the declaration.
        let declared = "aden://doc/p/user-guide.adoc#faq";
        let heading_sorts_first = "aden://doc/p/aaa.adoc/h2faq";
        let referrer = "aden://doc/p/aaa.adoc/h2intro"; // same file as the heading!
        for a in [declared, heading_sorts_first, referrer] {
            storage.put_document(&mk(a)).unwrap();
        }
        storage.flush().unwrap();

        let ref_records = vec![(referrer.to_string(), vec!["ref:faq".to_string()])];
        link_store_edges(
            &storage,
            EdgeRecords {
                calls: &[],
                uses: &[],
                refs: &ref_records,
                implements: &[],
                mutates: &[],
                mentions: &[],
                supersedes: &[],
                demonstrates: &[],
                terms: &[],
                cochange: &[],
                test_anchors: &std::collections::HashSet::new(),
            },
        )
        .unwrap();

        let out = storage.get_outgoing_edges(referrer).unwrap();
        assert!(
            out.contains(&(declared.to_string(), EdgeType::RelatesTo)),
            "<<faq>> must resolve to the [[faq]] declaration, not a heading slug; got {out:?}"
        );
        assert!(
            !out.iter().any(|(t, _)| t == heading_sorts_first),
            "the derived heading slug must not shadow the explicit declaration; got {out:?}"
        );
    }

    /// extract_doc_refs reads the parser-filled `doc_refs` attribute (the
    /// per-format extraction channel) — not the document blocks.
    #[test]
    fn extract_doc_refs_reads_doc_refs_attribute() {
        use aden_core::{Block, Document, NodeType};
        let mut attrs = std::collections::HashMap::new();
        attrs.insert(
            "doc_refs".to_string(),
            "ref:_term_b,ref:_term_a,ref:_term_b".to_string(),
        );
        let doc = Document {
            anchor: "aden://doc/p/x.adoc/h2s".into(),
            node_type: NodeType::Module,
            attributes: attrs,
            // Blocks containing `<<not_extracted>>` must be ignored: extraction
            // happens in the parser (fence/backtick-aware), not here.
            blocks: vec![Block::Paragraph("prose <<not_extracted>>".into())],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };
        assert_eq!(
            extract_doc_refs(&doc),
            vec!["ref:_term_a".to_string(), "ref:_term_b".to_string()],
            "sorted, deduped, attribute-sourced"
        );
    }

    #[test]
    fn extract_doc_includes_reads_doc_includes_attribute() {
        use aden_core::{Block, Document, NodeType};
        let mut attrs = std::collections::HashMap::new();
        attrs.insert(
            "doc_includes".to_string(),
            "chapter-two.adoc,chapter-one.adoc,chapter-two.adoc".to_string(),
        );
        let doc = Document {
            anchor: "aden://doc/p/master.adoc/h2overview".into(),
            node_type: NodeType::Module,
            attributes: attrs,
            blocks: vec![Block::Paragraph("body".into())],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };
        assert_eq!(
            extract_doc_includes(&doc),
            vec![
                "chapter-one.adoc".to_string(),
                "chapter-two.adoc".to_string()
            ],
            "sorted, deduped, attribute-sourced"
        );
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
