# Aden Issues - Future Work

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
- `ask` can route to a thin stub anchor (a bare module declaration → ~10 tokens)
  with no "result too thin, broaden" fallback. Phase 3.

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
- Doc-heading anchors (`aden://doc/...`) remain orphans — not linked to the
  symbols/sections they document.
- `aden heal` / `aden check` flood stdout (hundreds of events, repeated file
  lists) — context-hostile for an agent; should summarize/cap.
- `aden list --filter "mod-*"` glob returns 0 even when matches exist.
- `locate --caller-of` is unimplemented. (Docs corrected 2026-05-30 to the real
  binary flags `--caller-of`/`--show-context`.) The CLI `locate` still ignores
  the global `-j` — only `--format json` works — but the MCP `locate` tool no
  longer exposes a `json` arg, so agents route JSON via `format=json`.
- MCP server (ISSUES item 4 below): likely the debug `println!`s in
  `aden-cli/src/mcp.rs` corrupting the stdio JSON-RPC stream (flagged by
  `aden lint`).
- Symbol signature rendering still emits `param_x:_str: Unknown` and a `name:`
  that duplicates the node title — minor remaining density noise.

## Test Results Summary (v0.1.0 - Current)

| Language | init | gen | query | check | Status |
|-----------|------|-----|-------|-------|--------|
| **Rust** | ✅ | ✅ | ✅ | ✅ | Working |
| **Go** | ✅ | ✅ | ✅ | ✅ | Module path = "unknown" (minor) |
| **JavaScript** | ✅ | ✅ | ✅ | ✅ | Duplicate store entries (cosmetic) |
| **Python** | ✅ | ✅ | ✅ | ✅ | Working (uses tree-sitter-language-pack) |

## Confirmed Issues

### 1. JavaScript Duplicate Contracts
**Severity:** Low
**Description:** JavaScript files produce duplicate contract entries during "Stored" phase, but final list deduplicates correctly.
**Root Cause:** TypeScriptExtractor is being called twice or storing twice.
**Example:** `index.js#util` appears twice in "Stored 4 contracts" but only once in list.
**Status:** Low priority - cosmetic issue.

### 2. Go Module Path Not Resolved
**Severity:** Low  
**Description:** Go files show `aden://module/unknown/main.go#main` instead of extracting the actual module path from `go.mod`.
**Root Cause:** Go module path resolution not implemented or failing silently.
**Expected:** Should show something like `aden://module/example.com/project/main.go#main`
**Status:** Low priority - cosmetic issue.

### 3. No Persistent Active Project Setting
**Severity:** Medium
**Description:** Users must specify `--project` flag on every command or change directories manually.
**Status:** Partial fix - `--project` flag implemented. Persistent setting not yet implemented.

### 4. MCP Server Not Responding — NOT A BUG (2026-05-29)
**Severity:** ~~High~~ resolved / misdiagnosed
**Description:** `aden-mcp` appeared to "exit with connection closed: initialize request" when run directly.
**Root Cause:** That is expected behaviour of a *stdio* JSON-RPC server: with no MCP client on the other end of stdin, there is no `initialize` request to process. It is not a fault.
**Verified working:** Feeding a real MCP handshake over stdin
(`initialize` → `notifications/initialized` → `tools/list` / `tools/call`) returns
correct JSON-RPC responses: protocol negotiation, all 33 tools with schemas, and
a live `tools/call locate` returning results — clean stdout, empty stderr, exit 0.
The `rmcp` SDK owns the stdout framing and `run_aden_command` captures subprocess
output, so nothing pollutes the stream.
**Status:** Works. Use via an MCP client (`aden mcp install --platform <name>`), not by running the binary bare.

### 5. Source Required for Contracts
**Severity:** Medium
**Description:** Contracts cannot be generated without source files present. No way to define a "virtual" project structure purely from contracts.
**Status:** Not implemented - requires design work.

## Resolved / Not Issues

### Python Parsing - WORKING ✅
Python files ARE being parsed correctly via tree-sitter-language-pack. The earlier report of Python not working was incorrect. Functions, classes, and methods are extracted properly.

### JavaScript Duplicate - Cosmetic Only
The duplicates appear during "Stored" phase but don't affect final anchor list. Low priority.

## Priority Order
1. MCP server connectivity (blocks MCP users)
2. Persistent active project setting
3. Go module path resolution
4. JavaScript duplicate store entries
5. Virtual project structure support