# Aden Punch List — 2026-05-30

Goals in scope: **(1) docs accurate · (2) all functions work, esp. MCP.**
(Security audit deliberately deferred — out of scope for this list.)

Derived from a functional validation sweep (every CLI command run end-to-end;
mutating commands in scratch dirs) + a real JSON-RPC wire test of `aden-mcp`.
Each item is backed by an observed run, not inference. Effort: S ≤1h · M few h · L day+ · XL multi-day feature.

---

## Goal status at a glance

- **Docs (goal 1):** Largely done this session — 10 docs corrected against code + verified (commits `0ef247a`, `98562e7`, `08f0101`). Remaining: stale `.agent/audit-2026-05-30.md` snapshot (see Docs Remaining).
- **MCP (goal 2):** ✅ **Accurate & sound.** Wire test PASS on all 6 steps; description/timeout fixes landed (`720e4ac`). See MCP section.
- **Functions (goal 2):** Mixed — most work; a cluster of real bugs below.

---

## Functional punch list

### HIGH — real bugs / broken features

- [x] **H1 · Deletion pruning (`gen`)** — DONE. gen now prunes on re-index: GenCacheEntry records each file's anchor set; on re-parse, anchors absent from the fresh set are `delete_node`'d (cascading edges both directions); on `--auto`, anchors of vanished files are pruned too. WorkItem restructured so an emptied file (zero symbols) is still seen by the prune. Guards: never touch `mod-*` hubs or nodes lacking source_file. Verified: symbol-delete, file-delete, hub-survival, idempotent, stale caller_of all correct.
- [x] **H2 · `heal --gc`** — DONE. Rewritten from the legacy `.adoc`-walk no-op to a store sweep: enumerates stored docs, removes any whose `source_file` is gone from the project AND missing on disk, via `delete_node`. This is the authoritative GC that catches deletions gen never saw. Verified: removes orphaned node after file deleted without a re-gen; preserves hubs + live symbols; idempotent; no-op on clean project.
- [ ] **H3 · `lint --fix` corrupts source** — M. **Data-safety bug.** `--fix` does blind textual `.replace(".to_string().to_string()", ".to_string()")` etc. with no AST/awareness, so it mangles any file containing those patterns inside **string literals or comments** — it corrupted aden's own `lint.rs` detection literals during this sweep (reverted). On a pristine repo it reports 0 files (the "no-op" symptom), but on files that legitimately contain the patterns it silently breaks them. Fix: gate replacement to actual code tokens, or drop the textual `--fix` until it's AST-based.
- [ ] **H4 · `heal --fix` makes drift worse / noisy** — M. Exit 0 but spams "Skipping OrphanAnchor { … }: low confidence" and does not improve the health score. Fix: suppress/aggregate low-confidence skips; confirm it actually applies the high-confidence fixes (StaleHash/MissingContract/SignatureMismatch).
- [x] **H5 · `test --json` emits the human banner, not JSON** — DONE (`a` commit). Root cause: the `Test` clap struct had no json field and `cmd_test` ignored the global `-j`. Added a `json` param + structured envelope `{scanned_path, discovered, ran, passed, failed, results|tests}`; also added `test` to the MCP `structured_output_flags` allow-list so the MCP `test` tool returns parseable JSON. Verified over CLI + MCP wire.
- [x] **H6 · `query-adq node|incoming|outgoing`** — NOT A BUG (false alarm). The sweep agent invoked it as TWO args (`query-adq node <anchor>`); `script` is a single positional ADQ expression, so `node` became the script and `<anchor>` a (nonexistent) DIR. Called correctly — `query-adq "node(<valid-anchor>)"` with DIR defaulting to `.` — it returns proper JSON. At most a help-text clarity nit. Downgraded; no fix needed.
- [ ] **H7 · `complete` is an explicit LLM stub** — XL (feature). Scans + previews the prompt only ("full LLM integration would go here", `complete.rs:103`); never calls a model. MCP description already corrected to say so (`720e4ac`). Real fix is the LLM integration — defer unless prioritized.
- [x] **H8 · `regen`/`gen --quiet` print no summary** — DONE. Root cause: `quiet` was overloaded — `ensure_fresh` (the silent refresh-on-read path) and user `regen`/`--quiet` both passed `quiet=true`, so the "summary only" line was suppressed for everyone. Split **silent** (new `cmd_gen_silent`, used by `ensure_fresh`) from **quiet** (suppress per-file lines, keep summary). Now `regen` → "Stored N contracts. Skipped M"; read commands stay silent. Verified.

### MEDIUM

- [ ] **M1 · `watch --graph-sync` is a no-op** — M. Flag accepted, prints "Graph sync enabled…", but the graph is never updated (known TODO in `query.rs`). Fix: wire the incremental graph rebuild, or reject the flag as unimplemented.
- [x] **M2 · `heal --apply <bad-id>` exit code** — NOT A BUG (false alarm). Verified: `aden heal . --apply nonexistent_id_123` returns **exit=1** with `Error: "Failed to load proposal '...': No such file or directory"`. That is the correct non-zero-on-failure behavior. No fix needed.
- [ ] **M3 · `check`/`status` orphan-count noise** — S. 991 orphans reported as a warning floods output / drags health to 0/100; many are intentional metadata docs. Fix: classify metadata anchors as non-orphans or summarize.
- [ ] **M4 · `ask` JSON path** — S. Human mode works (resolves intent → anchor → context). Verify `-j/--json` produces a clean envelope (human mode confirmed; JSON unconfirmed).
- [ ] **M5 · `status --verbose` unused** — S. Flag accepted, no effect (doc already corrected). Fix: honor it or drop it.

### LOW / confirmed working (no action)

- **Work as documented:** `gen` (file + `--auto`), `sync`, `search`, `grep`, `locate --symbol`, **`locate --caller-of`** (real callers, confirmed), `list`, `query --from/--backlinks/--impact`, `query-adq where`, `asm` (`--budget` respected), `lint`, `lint --json`, `test --list`, `init`, `new`, `workflow`, `session`, `audit` (no OWASP findings in 92 files), `diagnose`, `review`, `kickoff`, `emergency` (`--ttl` honored), `federation` (list/add/config).

---

## MCP server — ✅ done (goal 2)

Wire test (real JSON-RPC 2.0 over stdio, not a `</dev/null` probe): **PASS on all 6 steps.**
- `initialize` → serverInfo + tools capability ✓
- `tools/list` → exactly 33 tools ✓
- `tools/call` grep/locate → well-formed structured-JSON content ✓
- unknown tool → `-32602` graceful error ✓
- missing required arg → `isError:true` usage text, no panic ✓
- no crash/hang; clean stderr throughout ✓

Fixes landed (`720e4ac`): `complete`/`watch`/`ci-check` descriptions corrected to match code; `run_aden_command` wrapped in a 120s timeout so blocking tools (`watch`, `heal --watch`) or a runaway `gen` fail cleanly instead of hanging the stream. `mcp_flag_parity` + 9 unit tests pass.

---

## Docs remaining (goal 1)

- [ ] `.agent/audit-2026-05-30.md` — point-in-time snapshot now overtaken: ~16 stale findings (MCP flag drift resolved, `--caller-of` implemented, `exec_where` wired, removed deps) + ~15 line-number drifts. Cheapest fix: a top-of-file "Phase-1 fixes landed" note; or strike the resolved findings. Low priority (internal working doc, not user-facing).

---

## Recommended execution order

**Phase A — quick wins (S, ~half day):** H5 `test --json`, H8 `regen` output, H6 `query-adq` DIR default, M2 `heal --apply` exit code.

**Phase B — the core feature you flagged (M/L):** H1 + H2 deletion pruning — make `gen` prune removed symbols on re-index and `heal --gc` actually GC store nodes+edges. Highest-value correctness fix; eliminates the stale-data class entirely.

**Phase C — safety + behavior (M):** H3 `lint --fix` corruption (data-safety), H4 `heal --fix` behavior, M1 `watch --graph-sync`, M3 orphan-count noise.

**Deferred features:** H7 `complete` LLM integration (XL), M4/M5 polish.

**Out of scope:** security audit (deferred by request).

---

### Process notes
- A functional agent ran `aden lint . --fix` against the real repo and H3 corrupted `lint.rs` (caught + reverted; tree clean). All future mutating validation must run in scratch dirs only.
- Concurrent sweep agents running `gen`/`heal` against the SAME `.aden/store` corrupted it (`FjallError: Storage(Unrecoverable)`) — the store is single-writer. Rebuilt with a clean single-process `gen . --auto`. Never run multiple mutating aden processes against one store.

### Progress (2026-05-30)
- **Phase A COMPLETE:** H5 ✅ (real fix), H8 ✅ (real fix), H6 ✅ (false alarm), M2 ✅ (false alarm).
- **Phase B COMPLETE:** H1 ✅ + H2 ✅ — deletion pruning. Store layer also got two latent-bug fixes en route: `delete_edge` matched on edge-type only (over-deleted adjacency entries), and `delete_document` didn't cascade edges (added `delete_node`); `put_edges_bulk` now dedups. The drift class (stale symbols/edges/caller_of lingering until a full `.aden` wipe) is closed.
- Testing gotcha: `cli_tests` invoke `aden` from PATH, so always reinstall (`cp target/release/aden ~/.local/bin/`) before `cargo test` or a stale binary fails `test_ask_returns_context` spuriously.
- 3 of the sweep's findings were false alarms (H6, M2, plus petgraph/SignatureMismatch in the doc pass) — agent verdicts skew pessimistic; verify each against the live binary before fixing.
- **Next: Phase C** — H3 lint --fix data-safety, H4 heal --fix, M1 watch --graph-sync, M3 orphan noise.
