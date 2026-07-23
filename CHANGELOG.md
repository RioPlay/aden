<!-- Copyright (c) 2026 RioPlay <rioplay@rioplay.dev> -->
<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Changelog

All notable changes to aden are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/); this project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.3.1] - 2026-07-12

### Fixed
- **Local release-bundle validation** — the documented maintainer command now
  packages and verifies the archive for the workspace version, rather than an
  obsolete fixed version.

## [0.3.0] - 2026-07-10

This release makes successful CLI reads JSON-first, adds explicit `--human`
rendering, improves deterministic context retrieval and prose support, and
hardens graph freshness and lookup performance. See
`docs/migrations/0.3.0.adoc` before upgrading terminal-output consumers.

### Added
- **Read snapshot for concurrent graph loads** (2026-07-03) — ADR-011: `gen`
  publishes `graph.snapshot` (Postcard docs + edges) beside the per-user store;
  `try_load` reads the snapshot when fresh, avoiding fjall `Locked` errors under
  multi-agent workloads. See `docs/adr-011-read-snapshot.adoc`.
- **Writer queue UX** (2026-07-03) — ADR-011 Phase 2: `gen` waits up to 10m on
  `store.lock` with holder `pid`/age notes; `Storage::new_with_retry` retries fjall
  `Locked` opens while readers finish; `aden status` shows active writer + snapshot.
- **Query-aware gather-then-select assembly** (2026-07-03) — `AssemblyOptions.relevance_select`
  (gated, default off) gathers the reachable neighborhood then selects to budget by
  blended relevance and structural proximity. `aden asm --select` activates it when relevance
  is available; an off-topic safety gate (`relevance_confidence`) falls back to the
  structural walk when cross-query calibration is low. MCP `asm` tool accepts `select`.
- **Rust type→method containment** (2026-07-03) — impl methods emit `member_of` edges,
  stored reversed as `Type —Contains→ method`, so Rust type hubs reach their impl-block
  methods in assembly and blast-radius traversal.
- **`understand` duplicate-symbol alternates** (2026-07-03) — when a bare symbol name
  resolves to one anchor but others share the same `#symbol` suffix, `understand` surfaces
  them in text (`## Other definitions`) and JSON (`alternates`).
- **Batch fjall doc ingest** (2026-07-03) — `gen` commits each file's slimmed documents
  and base snapshots in one atomic `put_documents_bulk` batch (fewer journal appends;
  doc and base never diverge on crash).
- **MCP stale-index JSON signaling** (2026-07-03) — read-tool JSON envelopes now
  include `index_stale` (and `stale_hint` when true) so agents detect graph
  staleness without parsing shell `NOTE:` lines. Applied to `grep`, `search`,
  `list`, `locate`, `understand`, `query`, `impact-diff`, and `communities`.
  Bare-array JSON outputs are wrapped as `{"index_stale": …, "items": …}`.

### Changed
- **JSON-first CLI reads** (2026-07-10) — read commands now emit structured JSON
  by default for agent/LLM consumers; `--human` explicitly selects clean terminal
  prose or tables, while `--json` remains a compatibility flag. `ask` and `asm`
  now return versioned JSON envelopes, and `--strict` caps the complete serialized
  envelope. Mutating `heal --propose/--fix/--gc` paths retain their established
  execution output so selecting a renderer can never skip the requested action.
- **Docs synced with ADR-011 read snapshot** (2026-07-03) — `architecture.adoc`,
  `commands.adoc`, `ai-integration.adoc`, `adr-010`, `AGENTS.md`, and `SECURITY.md`
  document `graph.snapshot`, writer-queue UX, and `aden status` diagnostics.
- **Docs synced with assembly + indexer state** (2026-07-03) — `commands.adoc`,
  `ai-integration.adoc`, `architecture.adoc`, and `index.adoc` updated for `asm --select`,
  `understand` alternates, kitoken (not HuggingFace `tokenizers`), and `ADEN_LEXICON_ON`
  opt-in gating. ADR-010 context reflects Phase 1 indexer extraction as shipped.
- **MCP freshness docs aligned with Phase 2A** (2026-07-03) — ADR-010,
  `ai-integration.adoc`, `AGENTS.md`, and `SECURITY.md` now document that
  `ADEN_SKIP_AUTO_GEN` is scoped to read tools only (write tools refresh without
  skip). `ISSUES.md` marks doc-heading orphan linkage as fixed (Phase 5).

- **Structural remediation (Phases 0–6)** (2026-07-02) — ADR-010 strangler indexer
  (`crates/aden-cli/src/indexer/{link,merge,fresh,gen}.rs`); `generate.rs` thinned
  to a re-export shell. MCP `ADEN_SKIP_AUTO_GEN` scoped to read tools; check/heal/
  status JSON summaries for agents; advisory policy wiring (`policy_violations[]`);
  PR-0 `store_directory_equivalence` gate; doc-heading `Contains`/`PartOf` edges;
  `make install-hooks` + `install.sh` pre-commit prompt; MCP golden-path CI step.
- **AsciiDoc indexing hygiene** (2026-07-01) — preprocessor moved to `aden-parse`
  (`asciidoc_preprocess.rs`) and wired into the active gen path (`include::`,
  `ifdef`/`ifndef`, `leveloffset`, tagged includes). Shared-attribute shallow
  scan, `[#id]` block anchors, file-level `xref:` refs, and `[[id]]`+heading
  dedup land in extraction. Fragment-only glossaries (`[[id]]Term::` on every
  line, Hugo frontmatter) now emit `aden://term/` nodes and `DefinesTerm` edges.
  `check` strips monospace/passthrough spans before path-link scanning so
  illustrative `` `xref:file.adoc#frag` `` examples do not false-flag.
  `GEN_LOGIC_VERSION` bumped **4 → 5** (one-time regen required).
- **Dual-substrate retrieval levers, on by default** (2026-06-19): `search`/`ask` route by
  detected text, a corpus-derived PPMI rerank for code (MRR 0.216 to 0.289) and grounded OEWN
  synonym expansion for prose (BM25 R@1 1/42 to 41/42; end-to-end 0/15 to 15/15). Auto-gating
  is enabled by default after an OFF vs ON bench confirmed it never regresses; disable with
  `ADEN_LEXICON_OFF`, or force a single lever with `ADEN_LEXICON_EXPAND` / `ADEN_PPMI_RERANK`.
  Both levers are grounded and corpus-gated, so they no-op where they would not help (the prose
  lever needs the OEWN store). Validated by the lexical ablations (dictionaries dilute code,
  dense captures only half of prose synonymy); ConceptNet excluded (CC-BY-SA). The MCP server
  inherits the levers via the CLI subprocess. See `docs/retrieval-levers.adoc`.
- **Query-aware assembly + MMR context selection** (2026-06-16) — `ask` now
  spends its token budget on *distinct, query-relevant* context. The assembly
  frontier is ordered by query relevance (the hybrid BM25+dense scores that
  already route the seed), and near-duplicate neighbors are pruned via MMR
  (lexical token-Jaccard, τ=0.8) before they consume budget — freeing it for
  diverse context. Both are deterministic (no per-query LLM), preserving aden's
  model-invariance. Motivated by a cross-corpus measurement: the 4096 default
  captures only ~25% of a richly-connected symbol's neighborhood (90% recall
  knee >8192), so *selection beats a bigger budget*; an MMR headroom probe found
  τ=0.8 reshapes 42–100% of rich hubs. Also lands the
  `aden-graph::personalized_pagerank` (PPR) primitive — available but not yet
  wired into `ask`, held pending a multi-hop assembly eval — and `#[ignore]d`
  measurement harnesses (`budget_sweep`, `assembly_ab`, `mmr_headroom`).
- **Viewer 3D: galaxies, bubble enclosures & grade punch** (2026-06-10) —
  group-gravity now works in all three axes, condensing each module into its
  own colored star cluster; each cluster is wrapped in a *bubble enclosure*
  (`e`) — a translucent, faintly-breathing membrane sphere sized to its
  members (cloned from live meshes, raycast-transparent so it never steals a
  click), recomputed as the layout drifts. Every orb gains an *aura*: a
  cloned, enlarged, translucent shell glowing in its own color that follows
  silhouette and breathing. The aurora/OLED grade was mixed too dark in 3D —
  link opacity 0.28→0.5 and ambient dim 0.42→0.6 (trails 0.85) so the wisps
  actually shimmer. The letterbox tour bars are REMOVED (full-frame tours;
  the resize wasn't worth the cinema).
- **Viewer 3D: never-lost guardrails + living matter** (2026-06-10) —
  *Guardrails*: the layout's bounding sphere (recomputed as physics cools)
  now clamps max zoom-out, the orbit pivot glides back inside the graph when
  a pan releases outside it, and an off-screen watchdog re-fits after ~2s of
  the graph being out of frame or behind the camera ("recentred — you
  drifted off the graph") — you can wander, you can't stay lost. *Living
  matter*: the sealed bundle hides THREE, but live scene objects don't —
  node materials gain emissive self-glow (orbs are lit from within on the
  true-black field), silhouettes are sculpted per kind by mutating mesh
  scales (docs flatten into tablets, Terms stretch into crystals, code stays
  spherical — matching the 2D shape grammar), scene lighting is rebalanced
  for more modelling, and the focused orb breathes (slow 6% swell, restored
  on blur, off under reduced motion).
- **Viewer: verbosity presets, OLED grade & aurora wisps** (2026-06-10) —
  *Verbosity presets (`v`, persisted per browser)*: one key cycles three ways
  of working — `focus` (still and quiet: no autopilot/streams/idle spin/flyby
  cards, minimal render in 2D), `map` (calm structure: envelopes + cards,
  nothing moves on its own — 2D default), `live` (the show: streams, idle
  orbit, auto-tour on open — 3D default). The 3D opening auto-tour now only
  fires in `live`. *OLED grade*: true-black backgrounds in both views (the
  void disappears on OLED; every photon is data) with the palette pushed to
  maximum chroma. *Aurora wisps*: links curve gently (2D 0.18 / 3D 0.25) and
  their ambient color is the blended hue of their two endpoints, so
  connection fields shimmer in gradients across regions instead of reading
  as straight gray wires; the edge-TYPE color still appears on attended
  links where it's information. *Fix*: letterbox bars no longer swallow the
  top chrome — header, search, status pill and hint ride the bars in and
  out with the same easing.
- **Viewer: flyby code cards + the cosmos grade** (2026-06-10) — the
  "stay in touch with the code" layer. *Flyby cards (3D)*: the exporter now
  embeds each node's real first lines (read from its stored span at export
  time; first sentence for docs/terms; capped at 1,500 nodes so kernel-scale
  exports stay lean), and a glass card floats beside whatever orb the camera
  is attending to — focus, list preview, every hop of a travel ride, every
  tour stop — showing the actual code as you pass. *Color grade*: dimming is
  now hue-preserving (colors darken, never gray out) and 3D links carry
  their source module's color — the web reads as colored circuitry; trails
  brighten in the same hue. *Cosmos*: a twinkling CSS starfield sits behind
  both views (3D renderer clears transparent), and in 2D every module gets a
  slowly-breathing nebula field behind its cluster that flares when a stream
  enters it. *Tour cinema*: letterbox bars during tours; travel rides gained
  a speed ramp (accelerate through the middle hops), alternating subtle
  dutch roll, and a decaying landing impulse on arrival. All motion remains
  off under `prefers-reduced-motion`.
- **Viewer: envelopes, streams & cinematic camera** (2026-06-10) — the
  containment + motion layer. *Module envelopes (2D, `e`)*: a group-gravity
  force pulls each module's symbols into a coherent cluster and a soft
  smoothed-hull membrane is drawn around it, labelled with the module name and
  aggregate mass ("aden-cli · 12.4k loc") — the "big idea" orb whose size
  EMERGES from its contents instead of being styled. *Ambient streams (both,
  `b` in 3D)*: signals walk 3–6 hop paths emitting particles; every crossed
  link joins a slow-fading trail (long-standing connections) and arrival
  occasionally triggers a section pulse — a synchronized burst across that
  module's internal links (2D: the envelope itself glows). *Cinematic camera
  (3D)*: all programmatic moves route through one tween-aware core; between
  moves a slight off-axis drift (three slow sines, amplitude scaled to camera
  distance) makes every shot breathe; the neighbour-list glance cam stands
  behind your current node looking down the synapse at whatever each
  connection points at, and search-travel is now a mach-speed chase cam that
  barrels hop-by-hop down the shortest path, strobing each crossed synapse,
  with only the destination getting the full focus treatment. Everything
  yields instantly to drag/click/search and stays still under
  `prefers-reduced-motion`.
- **Viewer: living-graph pass** (2026-06-10) — the eye-candy-with-purpose
  layer, both dimensions. *Content-mass sizing*: orbs scale with what's
  actually inside — real lines of code per symbol (`loc`, from stored spans),
  word counts for docs/terms (`words`), symbol counts for community spheres,
  connectivity only as fallback; mass shows in tooltips and panels. *Vivid
  generative palette*: golden-angle hue stepping replaces the fixed pastel
  set — maximally distinct, saturated colors for any number of groups.
  *Neighbour-walk strobe*: scrolling/arrow-walking the connection list keeps
  your current node anchored, expands the hovered neighbour's own ring, and
  fires a particle volley along the connecting edge — the correlation is
  drawn, not implied. 3D additions: *tour mode* (`t`, and auto after the
  opening full-graph sweep) — an ambient autopilot that wanders connections,
  leaps to new areas by pulling back first, dwells a random beat, and keeps
  peripheral synapses firing around you; any drag/click/search snaps control
  back instantly. *Thought-travel*: with a node focused, picking a search
  result hops the camera along the SHORTEST PATH edge-by-edge with synapse
  strobes — you watch how two thoughts connect instead of teleporting.
  Ember-drift particles run only on attended links; everything respects
  `prefers-reduced-motion` (no autopilot, no particles, jump cuts).
- **`aden view --3d` — the orbital view** (2026-06-10) — a slow-rotating 3D
  spatial picture of the project (vendored `3d-force-graph` v1.80.0, MIT,
  bundles three.js; pinned + sha256'd like the 2D library; fully offline).
  Deliberately split-purpose: 3D is for orientation and wonder (orbit, focus
  glide, keyboard walk), 2D stays the analytical view (lenses, replay,
  filters); `--3d --replay` is rejected with that explanation. Auto-rotation
  pauses the moment you drag, never starts under `prefers-reduced-motion`.
- **Viewer: pivot lenses + salience-first rendering** (2026-06-10) — the
  blast/reach/connectivity buttons no longer rebuild the graph (which
  restarted the force simulation at full energy — the "aggressive bounce");
  they are now a pure FILTER over the current view: every node keeps its
  position, the camera glides to the surviving subset, a dismissible pill
  (top centre, ✕/Esc) names the active lens, and the depth slider dials it
  live. New keys: `x`/`r`/`c` apply the lenses to the focused node, `m`
  toggles minimal render (no glow/pulse painting — the cheap clean mode),
  Esc escalates one layer per press (help → menu → lens → focus). Rendering
  now follows attention the way recall does — you don't see every connection
  your brain makes, only the ones about what you're attending to: ambient
  links are quiet (and structural/advisory types Contains/PartOf/
  AssociatedWith/Mentions start off in views >150 nodes; chips re-enable),
  while a hovered/focused node always reveals its FULL edge set regardless of
  filters; arrows and labels appear only on the focused neighborhood.
  Physics calmed globally (pre-settled warmup, heavier damping) so nothing
  explodes on load; stray background clicks no longer yank the camera
  (double-click/`f` re-fits); `prefers-reduced-motion` disables pulse/spin
  animation and makes camera moves jump cuts; always-on chrome trimmed (kind
  legend lives in the `?` overlay, controls dim until hovered).
- **Guided installer walkthrough** (2026-06-10) — `install.sh` rewritten around
  three principles: every step states what it changes, where, and why before
  acting; every edit to a user-owned file (shell profile, MCP configs,
  AGENTS.md) asks first — the old script appended to the shell profile
  unprompted; and the end-state is undoable, with a per-item undo line in the
  final summary plus a guided `--uninstall` mode (which unregisters MCP before
  removing the binary it needs). New coverage: MCP registration is now part of
  install (shows `aden mcp list`, registers detected platforms on confirm) and
  `--dense` offers the bge model fetch inline. Modes: interactive walkthrough
  (default), `--yes` (defaults, no prompts — but never guesses a repo for
  AGENTS.md), `--minimal` (binaries+PATH only), and non-TTY runs (curl/CI)
  perform only self-contained steps and print instructions for the rest.
  `INSTALL_DIR`/`PROJECT_ROOT`/`ADEN_DENSE` env overrides unchanged.
- **Wave 3 typed edges — `Supersedes`, `Justifies`, `AssociatedWith`** (2026-06-10)
  — the episodic layer: the graph starts recording *story* (what replaced what,
  why code exists, what changes together), not just structure. Same ADR-007
  emitter+consumer+eval policy, eval-first (`wave3_edges.rs`, 7 evals).
  `Supersedes`: a prose cross-reference on a line with supersede language becomes
  a directed NEW→OLD edge — both phrasings converge ("Superseded by <<new>>" in
  the old doc, "supersedes <<old>>" in the new one); parsers fill a
  `doc_supersedes` attribute (`by:`/`of:` direction prefix), resolution stays
  doc-side and format-neutral. `Justifies`: an ADR's unambiguous mention of a
  symbol co-emits ADR→code Justifies alongside Mentions (the
  Tests-alongside-Calls reclassification pattern) — "why is this here" becomes a
  one-hop traversal. `AssociatedWith`: module-level git co-change (last 1000
  non-merge commits, >20-file bulk commits skipped, ≥3 co-changes) emits
  bidirectional Hebbian association edges between file-level nodes (synthesized
  on demand, same pattern as `mod-*` hubs) — "what else usually changes with
  this" now answers the forgotten-file class of review miss; deliberately NOT in
  `impact_edge_types` (advisory association must never inflate a blast radius).
  Census on aden's own repo: AssociatedWith 284, Justifies 53, Supersedes 5
  (including the real ADR-003↔ADR-005 supersession) — live edge types 12 → 15.
  `GEN_LOGIC_VERSION` 3 → 4.
- **`aden viz/view --scope <subdir>` + `--resolution γ`** (2026-06-10) — the
  kernel-scale escape hatches. `--scope net/` restricts slices and community
  detection to the subtree's SUBGRAPH (detection runs on the scoped graph, not
  post-filtered whole-project clusters); an unmatched scope is an error, not an
  empty diagram. `--resolution` exposes the Louvain γ that already threaded
  through `detect_communities` (higher = finer clusters; 2.0–5.0 useful on very
  large graphs). Declared on the MCP `viz` tool (flag-parity test enforced).
- **Directory-based module naming for manifest-less languages** (2026-06-10) —
  `infer_project_name` gains layered fallbacks: language manifest (unchanged) →
  top-level directory under the VCS root (`mm/page_alloc.c` → `mm`,
  `net/core/sock.c` → `net`) → the file's own directory → `unknown`. Fixes the
  Linux-kernel failure where every one of 1.19M anchors collapsed into a single
  `aden://module/unknown/…` group, making community labels and module colors
  meaningless for C/Makefile trees.
- **ADR node typing for flat layouts** — `detect_node_type` now recognizes
  `docs/adr-001.adoc`-style file stems (and `.md` ADRs), not just `/adr/`
  directories; `validate_typed_edges` no longer classifies the decision edges
  (`Justifies`/`Supersedes`/`Amends`/`Verifies`) as code edges forbidden to ADR
  nodes — they are exactly what ADR nodes exist to emit.
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
