<!-- Copyright (c) 2026 RioPlay <rioplay@rioplay.dev> -->
<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Changelog

All notable changes to aden are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/); this project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

Post-0.2.0 work on main — not yet tagged.

### Added
- **Term nodes + `DefinesTerm` edges** (2026-06-10) — glossaries become graph
  citizens, completing Wave 2. Glossary entries (AsciiDoc `Name:: def` description
  lists — including the `[[anchor]]Name::` multi-line idiom — and Markdown
  `- **Term**: def` bullets) become `aden://term/<project>/<slug>` nodes
  (`NodeType::Term`) with section→term `DefinesTerm` edges. Gated to
  glossary-titled sections/documents so ordinary description lists stay prose. A
  term's definition links to the code it names through the existing
  unambiguous-only Mentions channel. Census: 14 Term nodes on aden's own repo —
  12 live edge types; `ask "what is reciprocal rank fusion"` on the research KB
  returns the glossary definition verbatim.
- **Wave 2 typed edges — `Mentions`, `Demonstrates`** (2026-06-10) — two new live
  edge types under the same ADR-007 emitter+consumer+eval policy. `Mentions`:
  unmarked prose that names a symbol in backticks gets a doc→code edge, deliberately
  weaker than `Documents` so intentional contracts stay undiluted; extraction is
  per-format (markdown + asciidoc fill a `doc_mentions` attribute, fence-aware like
  `doc_refs`), resolution is format-neutral and links only names ≥4 chars resolving
  to exactly ONE code anchor. `Demonstrates`: a doc code listing that references a
  symbol links listing→code, turning `code_block_*` anchors from orphan noise into
  "show me a working example of X" answers — a new language-neutral call-token scan
  feeds the existing `symbol_references` attribute. Consumers: `ask` traverses both
  (Demonstrates on usage/explain intents; a symbol's demonstrating listings fold
  into assembly alongside its callers), `query --edge-type mentions|demonstrates`.
  Census on aden's own repo: Mentions 172, Demonstrates 12 — live edge types 9 → 11.
  Term nodes + `DefinesTerm` (the glossary half of the roadmap item) are deferred.
- **`aden viz --mode reach`** (2026-06-10) — the outgoing dependencies view (what
  the anchor relies on), matching `query --impact` semantics, split out of the old
  mislabeled `blast` (see Fixed).
- **`aden view --editor auto`** (2026-06-10, new default) — "open in editor" links
  now probe PATH for code/codium/cursor/zed/idea and emit a URI scheme that actually
  has a handler on the machine. The hardcoded `vscode://` default went nowhere on
  VSCodium/Cursor/Zed-only machines (the OS silently drops a click on an
  unregistered scheme). Explicit `--editor` values are unchanged.
- **Wave 1 typed edges — `Tests`, `Implements`, `Mutates`** — three new live edge
  types (each requires an emitter, a consumer, and an eval gate before activation —
  the ADR-007 policy applied). Census on aden's own repo: Tests 138, Implements 75,
  Mutates 16, deterministic across regens. `impact-diff` gains an always-on
  `affected_tests` section (`--json affected_tests` array): test-source anchors with
  `Tests` edges into the touched symbols or their blast radius, so you see which test
  files cover the changed code before you push. `Implements` blast radius now reaches
  implementor methods — previously a trait's blast radius was silently truncated at
  the trait level without descending into its `impl` blocks.
- **Prose graph references — `RelatesTo` edges (ADR-006)** — cross-document
  `<<anchor>>`, `xref:file#fragment`, and markdown `[text](#heading)` references
  become bidirectional `RelatesTo` edges on `gen`. The design makes prose a
  first-class graph citizen: edges derived from refs the author explicitly wrote, not
  inferred mentions — so check's false-positive rate on real docs drops to zero. On
  the aden docs repo itself, this went from 58 unresolved refs → 0. `query --backlinks`
  now reaches prose nodes; `GEN_LOGIC_VERSION` bumped to 3 so mtime-skipped docs
  re-emit their refs on upgrade.
- **`aden ask --explain`** — routing transparency flag: prints the top anchor
  candidates with scores and token patterns, the tiebreak decision, intent
  classification, overview signal, and any fallback swap. Useful for diagnosing
  why a question routes where it does and for writing eval harness assertions.
- **`ask` conceptual routing** — broad questions ("What is Aden?", philosophy,
  architecture, getting started) now route to curated prose/entry-doc anchors via
  `RelatesTo`/`Documents` in-degree rather than implementation symbols. Root causes
  were AnchorPattern collapsing every `#`-anchor to `Symbol`, a token-split on
  non-alphanumerics fragmenting multi-word doc anchors, and a bare-token fallback
  hijacking correctly-chosen doc anchors to hubs. Golden-set accuracy 5/10 → 10/10;
  byte-identical output on the three narrow-scoped controls.
- **`ask` density/immediacy** — `ask` was routing correctly but assembling almost
  nothing: a 4115-token budget could return 22 tokens (0.5% density). Three-layer fix:
  (F1) render fixes — signature tables get a fallback name, leading docstrings and
  signature tables are exempt from intent block-filters, callee tables capped at 12
  so hubs don't burn budget on scaffolding; (F2) budget-aware source-span hydration
  packs included nodes with their actual source lines when budget allows (source hash
  verified; stale store → a note, never stale source), callers fold in as a depth-1
  frontier; (F3) three-rung escalation ladder for underfull anchors (re-render without
  intent filter → fold callers → depth +1) fires *before* community broadening, which
  now ranks members by BM25 query-relevance instead of degree. Eval gates: mean
  density ≥ 0.50, per-query floor ≥ 0.15, immediacy ≥ 12/15, budget-honesty 15/15,
  anchor-hit ≥ 12/15.
- **MCP ↔ CLI parity** — `viz` tool added to MCP surface; reverse parity tests
  ensure every CLI capability has an MCP counterpart and vice versa, so the two
  surfaces can't silently diverge.
- **ADR-005** (`docs/adr-005-footprint-opt-in-tracking.adoc`) — accepted via PR #18.
  Decides: only `.aden/` at the project root (folds `.agent/`, `.adenignore`);
  `.aden/config.toml` as the single config file; opt-in tracking (`git.track = false`
  by default, via a generated `.aden/.gitignore` that never edits the repo-root
  `.gitignore`). Resolves the ADR-003/AGENTS.md contradiction: "`.aden/` is ignored"
  is now the default truth; "intent travels with the repo" is the opt-in. Implementation
  unblocked by acceptance.
- **ADR-006** (`docs/adr-006-prose-graph-references.adoc`) — design record for
  prose as a first-class graph citizen (the RelatesTo edge work above).
- **ADR-007** (`docs/adr-007-typed-edge-model.adoc`) — typed-edge activation
  policy: an edge type is real only when it has an emitter, a consumer, and an eval
  gate. Documents the wave-1 activation and the blast-radius = transitive-dependents
  decision.

### Changed
- **Impact/intent filters no longer name emitter-less edge types** (2026-06-10) —
  `Constrains` and `Invokes` were removed from `impact_edge_types` and every ask
  intent edge set (ADR-007 §1: a filter naming a type with zero live edges filters
  nothing while reading like coverage). Behaviorally a no-op today; the enum
  variants and `--edge-type` parser entries remain, to be re-added to filters WITH
  an emitter. `query --impact` and `understand` now use the one shared set —
  `understand`'s local copy had silently drifted (missing `Implements`/`Mutates`,
  truncating its impact view at trait boundaries `query --impact` crossed).
- **`extract_code_references` deduplicated** (2026-06-10) — the markdown and
  asciidoc parsers each carried a byte-identical 116-line copy; now one shared
  implementation in `aden-parse::extractor`.

### Fixed
- **viz/view positional trap** (2026-06-10) — a directory path in the ANCHOR slot
  (`aden viz <path> --mode graph`, `aden view .`) now errors with guidance instead
  of silently visualizing the current working directory (the modes that ignore
  ANCHOR made the misplaced path invisible). Regression test included.
- **`viz --mode blast` traversed the wrong direction** (2026-06-10) — it claimed to
  mirror `impact-diff` but BFS'd outgoing edges, showing the anchor's *dependencies*
  under a *blast radius* label — the same inversion fixed in impact-diff (ADR-007
  §2), one surface over. `blast` now walks incoming impact edges (the dependents at
  risk if the anchor changes); rendered edges keep their stored caller→callee
  orientation in every mode. The impact edge SET is now defined once
  (`util::impact_edge_types`) and shared by impact-diff and viz so the two cannot
  drift again. Regression evals in `tests/viz_direction.rs`; `aden view`'s blast
  lens inherits the fix (it reuses the same slices).
- **Graph bridge silently dropped edges of unlisted types** (2026-06-10) — the
  store load path enumerated edge types from a local hand-written list, so edges of
  any newer type were written to the store but never loaded into the graph (Wave
  2's Mentions/Demonstrates surfaced this; it is exactly the drift class ADR-007 §1
  warns about). `EdgeType::ALL` is now the single canonical variant list and the
  bridge uses it.
- **`impact-diff` blast radius direction** — the blast radius was traversing
  *dependencies* (what the changed code calls out to) instead of *dependents* (what
  calls the changed code). This is a correctness bug in a shipped feature: "what
  breaks if I change this" is the reverse direction. Traversal now walks incoming
  edges via the impact-edge set, matching `query --impact` semantics.
- **`ask` honesty gate** — the ambiguous-match alternates loop appended separator
  (`"\n\n---\n\n"`) and header (`"// alternate (ambiguous match): …"`) overhead to
  the assembled body without charging those bytes against the effective budget, so
  multi-alternate responses could silently exceed `effective_budget` and fail the
  honesty check. Replaced the static `per_alt` slice with per-iteration remaining
  that pre-subtracts separator+header cost before each `assemble_seed` call; breaks
  early when fewer than 32 tokens remain.
- **`ask` test-anchor self-suppression** — `is_test_anchor` falsely fired on
  `#is_test_anchor` because the bare `"test_"` marker matched the symbol-name
  fragment. Narrowed to `"/test_"` (path-component form) so detection only fires on
  path segments (e.g. `/tests/`, `test_utils.py`), not symbol names after `#`.

### Legal
- **CLA v1.0** (`CLA.md`) — Contributor License Agreement adversarially reviewed
  (§5 covenant-not-condition confirmed). `CONTRIBUTING.md` and `LICENSE` aligned.

---

## [0.2.0] — 2026-06-09

The interactive graph + reproducible-retrieval release. New features, no breaking
CLI changes. Tag `v0.2.0` to trigger the release build (`release.yml`).

### Added
- **`aden viz`** — export a graph slice as a text diagram (Mermaid / Graphviz DOT /
  AsciiDoc / JSON). Modes: `blast`, `connectivity`, `communities`, and `graph` (the
  whole-graph view model: per-node `{anchor,label,group,community,kind,degree,file,line}`
  + every typed edge). `--full` emits the graph uncapped.
- **`aden view`** — interactive, **offline** browser graph (self-contained HTML, no
  CDN/network): community drill-down, full **git-history replay**, level-of-detail
  scaling, search, density/depth controls, edge-type filters, subsystem colouring,
  open-in-editor (`vscode://`-style), and a **synapse mode** (`b`) that animates the
  graph like a neural net. On by default (the `view` feature); disable with
  `--no-default-features`.
- **M6 benchmark infrastructure** — `scripts/gen_queries.py` (commit→single-file query
  authoring), `scripts/bench.py` (multi-corpus runner emitting a comparable JSON +
  markdown report), and a 5-repo / 5-language real-corpus eval (Go, Rust, C#, Python,
  TypeScript) showing **hybrid ≥ BM25 on every repo**.

### Changed
- **`EdgeType::Contains`** — split containment off `Documents`. `Documents` now means
  *only* prose→code; module/project containment is `Contains` (+ the `PartOf` inverse).
  Frees the code↔prose signal that was 2:1 buried under containment mirrors.
- `aden view --max` (replay history depth) defaults to `0` (entire history).
- Vendored `*.min.js` / `*.min.css` are no longer indexed (removed a synthetic
  `mod-unknown` mega-hub from the self-graph).

### Fixed
- **Reproducible retrieval** — `aden gen`/`search` are now deterministic run-to-run.
  Five root causes fixed: a wall-clock `:last-verified:` timestamp in the indexed
  text, two `HashMap`-iteration orders (`collect_store_entries`, `ingest` dedup), the
  parallel store-write anchor-collision race, and `emit_document` attribute order.
- `detect_communities` determinism (canonical node order before Louvain).
- **`aden view` density slider during replay** — the `idVisible()` reveal gate was
  short-circuiting the density filter in `growMode`; density now composes with the
  reveal gate so the slider is live during replay (bypassed only at 100%).
- **`aden view` panel pointer-event leak** — clicking or hovering the side panel was
  selecting nodes beneath it. `#panel` now has `z-index 8`; an `overUI` flag
  (set on `pointerenter`/`leave` of panel, controls, search, replay bar, filter,
  and action buttons) guards all `onNodeHover`/`onClick` paths.

### Security
- `aden view` hardened against `<script>` injection — target-codebase data inlined
  into the page is escaped (`</`→`<\/`) so a symbol/doc containing `</script>` cannot
  break out when viewing an untrusted repo.
- `mcp serve --http` no longer advertises POST endpoints it doesn't implement; it is a
  health/discovery server, with tool access over the supported stdio transport.

### Legal
- Reproduced the full **force-graph MIT** copyright + permission notice in `NOTICE.md`
  (the bundle is vendored into the binary). Added source-repo license attribution for
  the benchmark corpus.

---

_Earlier history (hybrid retrieval, community detection, `impact-diff`, the eval
harness, parser upgrades) predates this changelog — see `~/Projects/aden-devlog`._
