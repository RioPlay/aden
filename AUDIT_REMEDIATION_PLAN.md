# Aden Full gstack Audit Remediation Plan

**Date**: 2026-06-30  
**Source**: Full parallel gstack team audit (7 specialized subagents + direct gate runs + aden-first investigation).  
**Status**: Phase 1-3 committed on branch `fix/audit-phase1-hygiene`. Phase 4 investigation active.

**Completed & Committed:**
- Phase 1 (Hygiene): L1 lint fixes (debug prints + clones) committed as fix(lint).
- Phase 1 (Docs): L8 qualified unsafe/ADEN_STORE claims committed as docs.
- Phase 2 (CI): fmt/clippy enforcement + hooks targets committed as ci.
- Phase 3 (MCP): env hardening committed as fix(mcp).
- All gates green before each commit (fmt, clippy -D, aden check, tests).
- aden-first for all discovery.

**Current:** 4 atomic commits on branch. aden lint 0 Rust warnings | full ws clippy clean | aden check PASS.

Remaining: complete Phase 1 parser/docs if any, L2 benches, full Phase 4 (H1 generate refactor + M1-M3), Phase 5-6.

## Executive Summary

A comprehensive audit using the project's own aden tools (plus cargo/clippy/tests) found the project in **good overall health**:

- All tests green.
- `aden check --severity Forbid`, `aden ready`, `aden ci-check` pass.
- Strong invariants around referential integrity, merge safety (human regions preserved), path confinement, secret filtering, and store isolation (ADR-003).
- Excellent fixture-driven integration tests on critical paths (parse_contract ↔ emit roundtrips, MergeProposal, graph construction, store recovery).
- Parser hardening and docs anchor discipline are solid.

**Key problem areas** (prioritized):

**HIGH**
- Heavy concentration of the write/indexing/merge pipeline inside `crates/aden-cli/src/commands/generate.rs` (god module). This is the highest-blast-radius item.

**MEDIUM** (multiple)
- Duplicate document parsing/extraction logic between aden-parse and aden-graph.
- CLI crate pulls almost the entire workspace (god crate).
- Inconsistent dedup / multigraph handling and determinism in AdenGraph paths.
- MCP child process inherits the full host environment (no allowlist).
- Policy/constitution engine exists but is only partially wired into hot paths.
- CI does not enforce `cargo fmt -- --check` or `cargo clippy -- -D warnings` (only manual + aden gates).

**LOW / INFO** (many actionable but lower risk)
- aden lint warnings (dbg!, clones).
- Benches not executed in CI.
- Actionable orphans reported by `aden check`.
- Pre-commit hook has activation friction.
- Parser "300+ languages" claim exceeds wired support.
- Occasional fjall lock contention under concurrent MCP + CLI use.
- Documentation claims need minor qualification ("zero unsafe in application code", policy enforcement depth).

All findings have evidence in specific files/anchors from the subagent reports.

## Guiding Principles for Remediation

1. **aden-first always** (per AGENTS.md):
   - Before reading or changing any symbol: `aden grep`, `aden locate --symbol`, `aden understand <symbol>`, `aden asm <anchor>`, `aden query --backlinks=<anchor> --impact=<anchor>`.
   - Re-run after large external changes only if needed; the graph stays fresh.
2. **Respect hard boundaries**:
   - Ask the user before touching:
     - `crates/aden-core/src/contract.rs`
     - `.aden/store` or any LSM/fjall internals
     - `crates/aden-store/src/fjall_store.rs` (except via public trait)
3. **Rust + project conventions**:
   - New branch per logical change.
   - `cargo fmt --all && cargo clippy --workspace -- -D warnings` before every commit.
   - `cargo test --workspace` must stay green.
   - Atomic commits. Conventional commits (`fix:`, `refactor:`, `docs:`, `chore:`).
   - Integration tests + fixtures preferred over mocks.
   - Red-green-refactor: adjust or add failing test first when behavior changes.
4. **Parallel where possible**:
   - Independent items can be worked by subagents or separate branches.
   - Use `spawn_subagent` for exploration/refactor planning on isolated areas.
5. **Verification at every phase**:
   - Run `aden check . --severity Forbid`, `aden ready .`, `aden ci-check .`, full `cargo test`, fmt+clippy.
   - Re-run a focused gstack-style audit after major phases.

## Prioritized Issue List

### HIGH
| ID | Issue | Primary Locations | Impact | Blast Radius |
|----|-------|-------------------|--------|--------------|
| H1 | Write path concentration | `crates/aden-cli/src/commands/generate.rs` (cmd_gen, link_store_edges, reconcile, indexing) | Hard to evolve indexing independently; divergence risk between gen and asm/ask/heal/diagnose | Very high (touches store, heal, merge, graph construction) |

### MEDIUM
| ID | Issue | Primary Locations | Impact |
|----|-------|-------------------|--------|
| M1 | Duplicate parsing | aden-parse (router, extractors) vs aden-graph (parser.rs, cache.rs) + generate | Inconsistent anchors/refs/edges between write and read paths |
| M2 | CLI god crate | `crates/aden-cli/Cargo.toml`, `src/commands/mod.rs`, generate.rs etc. | Fat binary; hard to use core crates standalone |
| M3 | AdenGraph dedup + determinism | `crates/aden-graph/src/graph.rs` (add_node, add_edge*, build_*), cache.rs, traverse.rs | Inconsistent multigraph behavior; order-dependent results; DuplicateAnchor never enforced |
| M4 | MCP env inheritance | `crates/aden-mcp/src/lib.rs` (`run_aden_command`) | Child sees all host env (model keys, secrets) |
| M5 | Partial policy wiring | `crates/aden-policy/src/lib.rs`, limited call sites in gen/check/lint | "Constitutional" Forbid directives not strongly enforced in hot paths |
| M6 | Missing fmt/clippy in CI | `.github/workflows/ci.yml`, Makefile | Local CLAUDE.md rules not enforced in CI |

### LOW / INFO
| ID | Issue | Locations | Notes |
|----|-------|-----------|-------|
| L1 | aden lint warnings | heal.rs:759 (dbg!), query.rs (multiple dbg!), review.rs, clones | Non-blocking today |
| L2 | Benches not CI-gated | `benches/`, ci.yml | Useful but "early" per docs/benchmarks.adoc |
| L3 | Actionable orphans | `aden check` output (52) | Run `aden heal . --gc` or document expected set |
| L4 | Pre-commit hook friction | `tools/git-hooks/pre-commit`, git config | pre-push is active; pre-commit requires manual install |
| L5 | Parser breadth claim | docs/architecture.adoc, README, aden-parse/router.rs | Actual support is curated + generic fallback |
| L6 | CSV extractor quality | `crates/aden-parse/src/csv.rs` | Naive split; document or replace |
| L7 | Store contention | fjall "Locked" during mixed MCP/CLI use | Operational; improve observability? |
| L8 | Docs qualification | security-model.adoc, SECURITY.md | "zero unsafe", policy depth, ADEN_STORE power-user path |

## Phased Remediation Plan

### Phase 0 — Foundations (low risk, ½ day)
- [ ] Create and commit this plan (`AUDIT_REMEDIATION_PLAN.md`).
- [ ] Run baseline:
  ```bash
  cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings
  cargo test --workspace -- --quiet
  ./target/release/aden check . --severity Forbid
  ./target/release/aden ready .
  ./target/release/aden ci-check .
  ```
- [ ] Use aden to prime understanding of key symbols (see "Investigation Commands" below).
- [ ] Decide tracking: GitHub issues, or `.agent/` notes, or both. Create one issue per phase or per HIGH/MED item.
- [ ] Branch strategy: `fix/audit-h1-generate-refactor`, `fix/audit-m4-mcp-env`, etc. Never commit directly to main.

**Investigation commands (run these first for every item):**
```bash
./target/release/aden understand <symbol>
./target/release/aden locate --symbol <name>
./target/release/aden grep "<pattern>" --regex
./target/release/aden query --backlinks "<anchor>" --impact "<anchor>"
./target/release/aden asm --from "<anchor>"
```

### Phase 1 — Hygiene & Quick Wins (parallel-friendly, 1–2 days)
- L1 (dbg! + clones)
  - Use `aden grep "dbg!"` (scoped) and `aden understand` on the enclosing functions.
  - Replace with `eprintln!` + tracing or remove (most are debug leftovers).
  - Locations reported: `heal.rs`, `query.rs`, `review.rs`.
- L3 (orphans)
  - Run `aden heal . --gc --dry-run` (or propose) and decide on GC vs documentation.
- L4 (hooks)
  - Add `make install-hooks` or improve `install.sh` to symlink/copy pre-commit.
- L8 (docs)
  - Qualify claims in `docs/security-model.adoc` and `SECURITY.md`:
    - "No `unsafe` in application/library code (test and MCP startup env hacks are the only exceptions)".
    - Note current depth of PolicyEngine integration.
  - Add stronger warning for `ADEN_STORE` override.
- L5 / L6 (parser claims + CSV)
  - Update architecture.adoc and commands.adoc to reflect actual breadth.
  - Either improve CSV or add clear limitation + test.

**Verification**: `aden lint .` (target 0 warnings or documented), `aden check`, full test.

### Phase 2 — CI & DX Enforcement (½–1 day)
- Edit `.github/workflows/ci.yml`:
  - Add after "Setup Rust":
    ```yaml
    - name: Format check
      run: cargo fmt --all -- --check
    - name: Clippy
      run: cargo clippy --workspace -- -D warnings
    ```
  - Consider adding `cargo run -p aden-cli -- ci-check .` (already partially present via ready in some jobs).
- Update Makefile `ci` target to include fmt + clippy.
- Optional: make `aden ready` or `aden ci-check` invoke fmt/clippy internally for local parity.
- Add a scheduled workflow or job for `cargo bench` (or `scripts/bench.py`) on main.

**Verification**: PRs will now fail on fmt/clippy.

### Phase 3 — Security & MCP Hardening (1 day, some overlap with Phase 1)
- M4 (env inheritance)
  - In `crates/aden-mcp/src/lib.rs`:
    - Modify `run_aden_command` to start with `.env_clear()`.
    - Pass explicit allowlist: `ADEN_*`, `PATH`, `HOME` (minimal), any required for git/dirs.
  - Add unit test.
  - Update docs/ai-integration and security-model.
- M5 (policy wiring)
  - Use aden to find current callers of `PolicyEngine`, `evaluate`, `Directive`.
  - Identify high-value insertion points (e.g., before indexing secrets or certain imports).
  - Start with non-breaking: surface more via `aden diagnose` or `lint` if not already.
  - Consider making some Forbid checks block `gen` (behind flag initially?).
- Review `aden-mcp` surface tiers and timeouts (already good).

**Verification**: `aden audit`, manual MCP test, updated security docs.

### Phase 4 — Structural Refactors (highest effort, multiple PRs)
**H1 + M1 + M2 + M3** — do not rush. Investigation started.

**Progress so far**:
- `aden grep "fn cmd_" crates/aden-cli/src/commands/generate.rs` surfaced:
  - cmd_gen (1421)
  - cmd_gen_opts
  - cmd_gen_silent
  - cmd_gen_inner (1535) — the core
- `aden understand "cmd_gen_inner"`:
  - Backlinks to cmd_gen*, ADRs, our plan.
  - **Downstream impact: 185 nodes** (store put_document/base, paths (root/key/guard), index, graph (build/add/edges), emit, core (contract/parse/reconcile/merge), util, etc.). Confirms extreme blast radius — must extract carefully with tests first.
- `aden understand "link_store_edges"`: at 782, 44 impacts (store put_edges, graph add, many resolvers like resolve_callee in same file).

**Sub-plan for H1 (generate concentration)**:
1. `aden understand generate` + `aden asm` on the command + key functions (`cmd_gen_inner`, `link_store_edges`, work item processing, etc.).
2. `aden query --impact` on the generate module to see all downstream.
3. Identify extractable pieces:
   - Symbol emission + edge linking logic.
   - Reconcile / base snapshot handling.
   - Store write coordination.
4. Propose new boundary, e.g.:
   - Move core to `aden-index` crate (or `aden-cli/src/indexer` module first).
   - Expose `index_paths(...) -> Result<GraphStats>` or similar behind a clean trait.
5. Incremental:
   - First PR: extract pure functions + add tests that compare old vs new path.
   - Second: wire generate to call the new API.
   - Keep store writes going through the public `GraphStorage` trait where possible.
6. Update `aden-graph` / `aden-index` docs and architecture.adoc.

**M1 (duplicate parsing)**:
- Decide canonical path for AsciiDoc/Markdown contract extraction.
- Either move aden-graph's `parser.rs` logic into aden-parse or make aden-parse the source for both gen and `build_from_*`.
- Heavy use of `aden grep` + `aden understand` on `ParsedDocument`, `extract_code_references`, `collect_anchors`.

**M2 (CLI god crate)**:
- After H1 extraction, reduce direct dependencies in `aden-cli/Cargo.toml`.
- Consider thin `aden-app` or re-export facade from the root `aden` crate.
- Goal: make `aden-core`, `aden-graph`, `aden-index` more usable standalone.

**M3 (AdenGraph)**:
- `aden understand AdenGraph` (use CLI `aden` or direct source after locating).
- In `graph.rs`:
  - `add_node`: now returns `Result<NodeIndex, GraphError>`, checks for duplicate anchor and returns `Err(DuplicateAnchor)` (restored enforcement).
  - Updated callers in build_from_* and test helpers (let _ = since dups shouldn't occur in normal construction).
  - `add_edge`: now dedupes on (pair, edge value) matching add_edge_by_anchor (was only contains_edge on pair).
- Update CLI paths (query, locate, impact_diff, asm traverse) to use `get_backlinks` / cache where appropriate.
- Add or strengthen tests for multigraph + backlinks + build_from_directory vs build_from_storage equivalence.

Completed: dedup enforcement in add_node + multigraph consistency in add_edge.

**Approach for all of Phase 4**:
- Small PRs with red tests first.
- Use `aden impact-diff` (once available on the branch) on the diff itself.
- Keep `aden check --Forbid` green at all times.

### Phase 5 — Parser & Claims Alignment + Final Polish
- Expand `GENERIC_PACK_EXTENSIONS` or document the exact set.
- Improve or gate CSV (or mark as "best-effort").
- Add more adversarial parser tests (deep nesting, malformed [[ ]], large files).
- Address any remaining L items not covered earlier.

### Phase 6 — Verification & Closeout
- Full `cargo test --workspace`.
- `cargo fmt && cargo clippy -- -D warnings`.
- `aden gen . && aden check . --severity Forbid && aden ready . && aden ci-check .`.
- Re-run a lightweight gstack audit (or at minimum the Testing/CI/Architecture agents) and compare findings.
- Update CHANGELOG.md, relevant ADRs if behavior changed.
- Clean up any temporary ignored tests or measurement harnesses if they are now stable.
- Close tracking issues with references to the PRs.

## Execution & Tooling Notes

- **For every change**:
  1. `aden grep "pattern"` or `aden understand "SymbolName"`.
  2. `aden asm <anchor>` or `aden query --backlinks`.
  3. Read only the minimal neighborhood.
  4. Write/adjust test first (red).
  5. Implement (green).
  6. fmt + clippy.
  7. Run relevant `aden` gates + `cargo test -p <crate>`.

- **Parallel work**:
  - Phase 1 items are largely independent.
  - CI changes (Phase 2) can happen early.
  - Security items (Phase 3) can start while structural work is planned.

- **Risks & Mitigations**:
  - H1 refactor has highest risk of breaking gen/heal/merge fidelity → use base snapshots, roundtrip tests, and the existing `merge_engine_integration.rs` + `contract_roundtrip_tests.rs` heavily.
  - Parser changes can shift anchors → must keep `aden check` green.
  - Do not touch `contract.rs` internals without explicit approval.

- **Success Metrics**:
  - Zero HIGH findings in a follow-up audit.
  - All MEDIUM items either fixed or explicitly accepted with rationale in the plan.
  - CI now enforces fmt + clippy.
  - `aden ready` and `aden ci-check` remain the local "single command" gate and stay green.
  - Health score from `aden doctor` / `aden check` improves or is documented.

## Open Questions / User Decisions Needed

- How aggressively to wire PolicyEngine (M5)? Blocking gen on Forbid vs advisory?
- Should `aden lint` become blocking in `ready`/`ci-check`?
- Preferred long-term home for the extracted indexer (new crate vs inside aden-index vs aden-graph)?
- Any items we decide to accept as "won't fix" (e.g. certain orphans, bench scheduling)?
- Permission to explore changes in `aden-core/src/contract.rs` for deeper policy integration?

---

**Next step recommendation**: Start with Phase 0 + Phase 1 in parallel on two branches. Use `aden understand` on the generate module and key MCP functions as the very first investigation step for the structural items.

Run `cargo fmt --all && cargo clippy --workspace -- -D warnings && cargo test --workspace` after every significant step.

This plan is itself a living document — update it as work progresses and re-prime with aden after large refactors.