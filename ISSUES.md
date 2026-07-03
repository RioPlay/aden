# Aden Issues - Future Work

## 2026-06-07 — `build_snippet` panics on non-ASCII (char-boundary slice)

- **FIXED — `aden search`/`ask` panicked on any doc containing a multi-byte
  UTF-8 character near a snippet boundary.** `build_snippet` in
  `crates/aden-index/src/lib.rs` truncated a long match line with a *byte*
  slice: `format!("{}...", &snippet[..200])`. Rust's `str` indexing panics when
  byte index 200 falls in the middle of a multi-byte char, so a snippet whose
  byte 200 landed inside e.g. `→` (U+2192, 3 bytes), an em-dash, or any accented
  letter crashed the whole query instead of returning results. Hit in practice
  while indexing prose containing arrows.
  - **Fix shipped:** truncate by chars, not bytes —
    `snippet.chars().take(200).collect::<String>()`. The 200 limit is cosmetic,
    so char-vs-byte length is immaterial. Two regression tests added
    (`build_snippet_truncates_multibyte_without_panicking`,
    `build_snippet_short_line_unchanged`); the first straddles byte 200 with `→`
    and would have panicked under the old slice.
  - **Audit confirmed clean:** this was the only `&str[..N]` byte-slice on
    source/user text in `aden-index` (grep verified).

## 2026-06-04 — Dependency hygiene + propose/heal/emit dedup

A `cargo machete` audit (verified by source grep) found ten unused dependencies.
All were removed; `cargo check --workspace` stays green and machete now reports
clean. Removed: `aden-graph` (`blake3`, `serde_json`), `aden-parse` (`fnv`,
`rayon`), `aden-mcp` (`serde`), `aden-diagnose` (`tempfile` dev-dep), and three
internal workspace edges — `aden-heal → aden-emit`, `aden-propose → aden-emit`,
`aden-propose → aden-heal`.

- **RESOLVED (by deletion) — `aden-propose` carried a dead parallel
  proposal-rendering path.** Investigation (fan-out read + grep) showed the
  "duplication" was not live-but-duplicated code: `patch.rs` (`generate_patch` +
  a local 3-variant `DriftEvent` + `table_to_asciidoc`), `stub.rs`
  (`generate_stub`/`write_stub`/`emit_table`), `store::apply`, and `ProposeError`
  had **zero callers** anywhere, and the crate has no tests. The original note's
  plan — "consume `aden_heal::DriftEvent` and render via `aden_emit`" — was also
  infeasible: `aden_heal::DriftEvent` (9 variants, path-keyed) flattens
  signatures to `Vec<String>` and carries no `aden_core::Table`, so it cannot
  back the current-vs-proposed Table patch the local `DriftEvent` was built for;
  the two enums are different layers (detect vs. render), not duplicates. And
  `aden_emit` exposes no public `Table`→AsciiDoc renderer (its `emit_table` is
  private). The CLI's real path (`aden-cli/src/commands/heal.rs::generate_proposal`)
  hand-builds `Proposal` structs from `aden_heal::DriftEvent` and uses only
  `aden_propose::{Proposal, ProposalStatus, persist, load, list}`. So the honest
  fix was to **delete** the dead path, which also removed the triplicated
  table-emitter (the surviving copy is `aden_emit`'s private one). `aden-propose`
  is now a pure-std crate (no `aden-core`/`thiserror` deps). If richer proposals
  are ever wanted, build them against today's `aden_heal::DriftEvent` in
  `generate_proposal` (currently hand-rolls `patch_asciidoc` with `writeln!`),
  with tests — not by reviving the stale fork.

## 2026-05-30 — MCP rewrite phase 1+2 (contract + structured output)

The MCP server is a thin director that maps each tool 1:1 to a CLI subcommand
and returns its stdout. Two classes of defect made agent calls fail or return
junk; both are fixed:

- **FIXED — MCP↔CLI flag drift.** The MCP `TOOLS` table declared flags the CLI
  does not accept, so clap rejected them or they were silently dropped. Affected
  `kickoff` (`brief`→`--name`), `new` (`lang` was positional, is `--lang`),
  `workflow` (`output`→`--out`, template positional), `gen` (5 nonexistent flags
  removed), `asm` (`--anchor` removed; the anchor is `--from`), `diagnose`
  (`severity`/`fix` removed; `path` is `--path`), `locate` (`json` removed — see
  below), `test` (`json`/`unlimited` removed), `session` (`status` no longer
  required), and the subcommand-based `mcp`/`federation`. A build-time
  parity test (`crates/aden-cli/tests/mcp_flag_parity.rs`) now asserts every
  MCP-emittable flag is accepted by the CLI, so drift fails the build.
- **FIXED — generated artifacts polluted grep/index.** A bare `.aden/` ignore
  rule only matched at the repo root, so per-crate caches at
  `crates/<x>/.aden/cache/index-cache.json` were walked. Directory ignore rules
  now match a bare name at any depth (gitignore semantics), so `aden grep` no
  longer drowns real hits in cache noise.
- **ADDED — structured output for read tools.** `grep`, `search`, and `list`
  now emit a JSON envelope (`{total, returned, truncated[, offset], …}`) under
  `--json`/`-j`, and the MCP requests it automatically — agents read counts and
  truncation as data instead of parsing human tables or the "... and N more"
  footer.

### Still open
- ~~`ask` can route to a thin stub anchor (a bare module declaration → ~10 tokens)
  with no "result too thin, broaden" fallback. Phase 3.~~ **FIXED** — `ask` now
  detects assembled output < 150 tokens and falls back to `mod-project`.

## 2026-05-29 — Graph connectivity & density fixes (validation pass)

A validation pass against the docs (run on the aden repo itself and on an
external Python repo, github.com/pallets/click) found that the core promise —
"graph the whole codebase so an LLM understands it in one shot" — was broken,
and fixed the root causes:

- **FIXED — Edges silently dropped for any symbol/doc anchor.** `aden-store`'s
  `edge_key` packed `edge:{src}:{dst}:{type}` with `:`, but symbol anchors
  contain `:` (`aden://module/...`). `get_edges_by_type` required exactly 3
  colon-split parts, so it dropped every edge touching a symbol — the entire
  graph was disconnected. Switched the field separator to ASCII Unit Separator
  (`KEY_SEP`, 0x1F).
- **FIXED — gen wrote zero edges + module nodes never stored.** `cmd_gen` now
  runs `link_store_edges`: persists `mod-<crate>`/`mod-project` nodes plus
  module↔symbol containment (Documents/PartOf) and `Calls` edges resolved from
  `edge::calls[...]`. `aden asm/query --from mod-<crate>` now returns the whole
  module; orphans dropped from ~1600 to ~640 (remainder are doc headings).
- **FIXED — asm/ask emitted a random fuzzy-matched symbol** when the anchor was
  not found. `resolve_anchor` now does exact + unambiguous `#suffix` resolution
  (bare `assemble` works) and hard-errors on miss/ambiguous; `ask` falls back to
  `mod-project`, never an arbitrary node.
- **FIXED — token budget ignored (density).** `estimate_tokens` counted only
  alphanumeric words, under-counting code ~2x; `ask` dumped 28 KB under a 4096
  budget. Switched to the documented bytes/4 heuristic; aligned intent depths to
  the docs (Explain 5→2 etc.); removed `PartOf` from auto-traversal so a symbol
  no longer climbs to its module hub and pulls in every sibling.
- **FIXED — per-node boilerplate.** Stripped the repeated tree-sitter provenance
  NOTE, the generic `module:: This symbol is part of the parent module.` line,
  and dangling empty `Relationships:` headers from llm-mode output.

### Still open (lower priority)
- ~~Doc-heading anchors (`aden://doc/...`) remain orphans — not linked to the
  symbols/sections they document.~~ **FIXED** (Phase 5) — `link_store_edges` now emits
  `Contains`/`PartOf` edges from doc-heading sections to their file representative;
  see `doc_heading_sections_gain_file_containment_edges` in `indexer/link.rs`.
- `aden heal` / `aden check` flood stdout (hundreds of events, repeated file
  lists) — context-hostile for an agent; should summarize/cap.
- ~~`aden list --filter "mod-*"` glob returns 0 even when matches exist.~~ **FIXED** —
  `--filter` now uses glob matching for patterns containing `*`/`?`; plain strings
  still use substring match for backward compatibility.
- `locate --caller-of` is implemented: it lists a symbol's callers by walking
  incoming `Calls` edges in the graph (run `gen` first to populate the call
  graph), enriched with each caller's `file:line`. The CLI `locate` still ignores
  the global `-j` — only `--format json` works — but the MCP `locate` tool no
  longer exposes a `json` arg, so agents route JSON via `format=json`.
- MCP server (ISSUES item 4 below): likely the debug `println!`s in
  `aden-cli/src/mcp.rs` corrupting the stdio JSON-RPC stream (flagged by
  `aden lint`).
- Symbol signature rendering still emits `param_x:_str: Unknown` and a `name:`
  that duplicates the node title — minor remaining density noise.

## Test Results Summary (v0.1.0 — historical snapshot)

> Historical snapshot. A later polyglot sweep added TypeScript (parsed via
> `tree-sitter-language-pack`, same path as the languages below). Treat the
> table as a point-in-time record, not the current support matrix.

| Language | init | gen | query | check | Status |
|-----------|------|-----|-------|-------|--------|
| **Rust** | ✅ | ✅ | ✅ | ✅ | Working |
| **Go** | ✅ | ✅ | ✅ | ✅ | Module path parsed from go.mod ("unknown" only when no go.mod found) |
| **JavaScript** | ✅ | ✅ | ✅ | ✅ | Duplicate store entries (cosmetic) |
| **TypeScript** | ✅ | ✅ | ✅ | ✅ | Added in later polyglot sweep |
| **Python** | ✅ | ✅ | ✅ | ✅ | Working (uses tree-sitter-language-pack) |

## Confirmed Issues

### 1. JavaScript Duplicate Contracts
**Severity:** Low
**Description:** JavaScript files produce duplicate contract entries during "Stored" phase, but final list deduplicates correctly.
**Root Cause:** TypeScriptExtractor is being called twice or storing twice.
**Example:** `index.js#util` appears twice in "Stored 4 contracts" but only once in list.
**Status:** Low priority - cosmetic issue.

### 2. No Persistent Active Project Setting
**Severity:** Medium
**Description:** Users must specify `--project` flag on every command or change directories manually.
**Status:** Partial fix - `--project` flag implemented. Persistent setting not yet implemented.

### 3. Source Required for Contracts
**Severity:** Medium
**Description:** Contracts cannot be generated without source files present. No way to define a "virtual" project structure purely from contracts.
**Status:** Not implemented - requires design work.

## Resolved / Not Issues

### Python Parsing - WORKING ✅
Python files ARE being parsed correctly via tree-sitter-language-pack. The earlier report of Python not working was incorrect. Functions, classes, and methods are extracted properly.

### JavaScript Duplicate - Cosmetic Only
The duplicates appear during "Stored" phase but don't affect final anchor list. Low priority.

## Priority Order
1. Persistent active project setting
2. JavaScript duplicate store entries
3. Virtual project structure support