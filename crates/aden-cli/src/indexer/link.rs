// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_store::GraphStorage;
use std::collections::HashMap;

/// The per-kind edge record slices `link_store_edges` writes, bundled into one
/// argument. Every record is the same shape (anchor -> target list); the field
/// name picks the EdgeType. `cochange` carries git co-change pairs and
/// `test_anchors` the set of test-symbol anchors (callers whose `Calls` are
/// reclassified as `Tests`).
pub(crate) struct EdgeRecords<'a> {
    pub(crate) calls: &'a [(String, Vec<String>)],
    pub(crate) uses: &'a [(String, Vec<String>)],
    pub(crate) refs: &'a [(String, Vec<String>)],
    pub(crate) implements: &'a [(String, Vec<String>)],
    pub(crate) mutates: &'a [(String, Vec<String>)],
    pub(crate) member_of: &'a [(String, Vec<String>)],
    pub(crate) mentions: &'a [(String, Vec<String>)],
    pub(crate) supersedes: &'a [(String, Vec<String>)],
    pub(crate) demonstrates: &'a [(String, Vec<String>)],
    pub(crate) terms: &'a [(String, Vec<String>)],
    pub(crate) cochange: &'a [crate::types::CochangePair],
    pub(crate) test_anchors: &'a std::collections::HashSet<String>,
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
pub(crate) fn link_include_edges<S: GraphStorage>(
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

/// True for heading/sectanchors section nodes (`…/h2slug`, `…/_slug`).
fn is_doc_heading_section(anchor: &str) -> bool {
    let Some(rest) = anchor.strip_prefix("aden://doc/") else {
        return false;
    };
    if rest.contains('#') {
        return false;
    }
    let Some(fragment) = rest.rsplit('/').next() else {
        return false;
    };
    if fragment.starts_with('_') && fragment.len() > 1 {
        return true;
    }
    let Some(level_digits) = fragment.strip_prefix('h') else {
        return false;
    };
    let (digits, slug) = level_digits.split_at(
        level_digits
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .count(),
    );
    !digits.is_empty() && !slug.is_empty()
}

/// Numeric heading level from `h<level><slug>` fragments; `None` for `_slug` forms.
fn doc_heading_level(anchor: &str) -> Option<u8> {
    let fragment = anchor.rsplit('/').next()?;
    let numbered = fragment.strip_prefix('h')?;
    let level_str: String = numbered
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    level_str.parse().ok()
}

/// Pick the file-level representative for heading containment edges.
fn doc_file_representative<'a>(cands: &[&'a str], file_key: &str) -> Option<&'a str> {
    let in_file: Vec<&str> = cands
        .iter()
        .copied()
        .filter(|a| doc_anchor_file(a) == Some(file_key))
        .collect();
    if in_file.is_empty() {
        return None;
    }
    // Inline `[[frag]]` declarations are the canonical file hub when present.
    if let Some(decl) = in_file.iter().copied().find(|a| a.contains('#')) {
        return Some(decl);
    }
    // Otherwise the shallowest heading section (usually h1) anchors the file.
    in_file
        .iter()
        .copied()
        .min_by_key(|a| doc_heading_level(a).unwrap_or(u8::MAX))
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
pub(crate) struct CalleeStats {
    pub(crate) resolved: usize,
    pub(crate) unresolved: usize,
    pub(crate) ambiguous: usize,
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
/// - Member-of: `edge::member_of[Type]` records (impl methods) become
///   type --Contains--> method edges (reversed from the emit direction).
/// - Mentions (Wave 2): backtick prose mentions (`doc_mentions` records)
///   become doc --Mentions--> symbol edges, only when the name resolves to
///   exactly ONE code symbol.
/// - Demonstrates (Wave 2): `symbol_references` records on doc code listings
///   become listing --Demonstrates--> symbol edges, same unambiguous-only rule.
pub(crate) fn link_store_edges<S: GraphStorage>(
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
        member_of: member_of_records,
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

    // Member-of edges: a Rust impl method is a member of its type, stored REVERSED as
    // `type —Contains→ method` so a type reaches the methods defined in its separate
    // impl blocks (a Python class nests its methods; a Rust type does not). Exact
    // resolution only, mirroring mutates.
    for (anchor, targets) in member_of_records {
        for t in targets {
            if let Some(target) = resolve_exact(t, anchor, &name_index)
                && target != anchor.as_str()
            {
                edges.push((target.to_string(), anchor.clone(), EdgeType::Contains));
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

    // Doc-heading containment: section nodes (`h<level>slug`, `_slug`) gain
    // PartOf/Contains edges to their file's representative so they are no
    // longer zero-edge orphans in graph traversals and health scoring.
    for anchor in &anchors {
        if !is_doc_heading_section(anchor) {
            continue;
        }
        let Some(file_key) = doc_anchor_file(anchor) else {
            continue;
        };
        let Some(stem) = std::path::Path::new(file_key)
            .file_stem()
            .and_then(|s| s.to_str())
        else {
            continue;
        };
        let Some(cands) = doc_file_index.get(stem) else {
            continue;
        };
        let Some(rep) = doc_file_representative(cands, file_key) else {
            continue;
        };
        if rep == anchor.as_str() {
            continue;
        }
        edges.push((rep.to_string(), anchor.clone(), EdgeType::Contains));
        edges.push((anchor.clone(), rep.to_string(), EdgeType::PartOf));
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
#[cfg(test)]
mod link_tests {
    use super::*;
    use aden_store::Storage;
    use std::collections::HashMap;

    use crate::util::{extract_doc_includes, extract_doc_refs, extract_uses};

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
                member_of: &[],
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
                member_of: &[],
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
                member_of: &[],
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
                member_of: &[],
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
                member_of: &[],
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
                member_of: &[],
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
            !out
                .iter()
                .any(|(t, et)| t == heading_sorts_first && *et == EdgeType::RelatesTo),
            "the derived heading slug must not shadow the explicit declaration via RelatesTo; got {out:?}"
        );
    }

    #[test]
    fn doc_heading_sections_gain_file_containment_edges() {
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
        let file_hub = "aden://doc/p/guide.adoc/h1guide";
        let section = "aden://doc/p/guide.adoc/h2setup";
        for a in [file_hub, section] {
            storage.put_document(&mk(a)).unwrap();
        }
        storage.flush().unwrap();

        link_store_edges(
            &storage,
            EdgeRecords {
                calls: &[],
                uses: &[],
                refs: &[],
                implements: &[],
                mutates: &[],
                member_of: &[],
                mentions: &[],
                supersedes: &[],
                demonstrates: &[],
                terms: &[],
                cochange: &[],
                test_anchors: &std::collections::HashSet::new(),
            },
        )
        .unwrap();

        let out = storage.get_outgoing_edges(section).unwrap();
        assert!(
            out.contains(&(file_hub.to_string(), EdgeType::PartOf)),
            "heading section must PartOf its file representative; got {out:?}"
        );
        let hub_out = storage.get_outgoing_edges(file_hub).unwrap();
        assert!(
            hub_out.contains(&(section.to_string(), EdgeType::Contains)),
            "file representative must Contains its sections; got {hub_out:?}"
        );
    }

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
