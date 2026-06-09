<!-- Copyright (c) 2026 RioPlay <rioplay@rioplay.dev> -->
<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Changelog

All notable changes to aden are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/); this project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

The interactive graph + reproducible-retrieval release. (Version bump pending:
recommend **0.2.0** — new features, no breaking CLI changes; `release.yml` tags
trigger the build.)

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
