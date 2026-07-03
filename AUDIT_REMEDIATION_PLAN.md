# Aden Structural Remediation Plan (Gstack-Reviewed)

**Date**: 2026-07-02 (updated 2026-07-03)  
**Source**: Original gstack audit + `/plan-eng-review` + `/plan-devex-review` (2026-07-02)  
**Status**: **Golden state reached** — Phases 0–6 shipped on `main` (2026-07-03).
ADR-011 read snapshot + writer-queue UX shipped on `wip/merge-engine-phase2-snapshot`
(2026-07-03).

---

## Locked Decisions (2026-07-02)

| Decision | Choice |
|----------|--------|
| Indexer extraction home | **Strangler first:** `aden-cli/src/indexer/` module; new crate (`aden-compile`) deferred until stable 2+ weeks |
| Crate naming | **Never `aden-index`** — that crate is BM25/dense search (~3,200 LOC). Optional future crate: `aden-compile` |
| Policy enforcement | **Advisory first:** `diagnose`/`lint`/`check` surface violations; no `gen` blocking until 0.3.0 |
| DX scope (pre-refactor) | **Core agent DX:** scoped `ADEN_SKIP_AUTO_GEN`, heal/check JSON summaries, stale hints, store-lock UX |

---

## Executive Summary

Aden is in **good health** (tests green, `aden check --severity Forbid` passes, `aden ready` + `ci-check` green). Structural concentration in `generate.rs` has been **resolved** via the strangler indexer; docs and MCP/runtime behavior are aligned.

**Shipped (all phases):**
- Phase 0 docs alignment (ADR-010, architecture, security, AGENTS)
- Phase 1 MCP `ADEN_SKIP_AUTO_GEN` commit
- Phase 2A scoped skip + stale hints
- Phase 2B check/heal/status JSON for MCP
- Phase 2C store lock UX
- Phase 3 advisory policy wiring
- Phase 4 strangler indexer (`indexer/{link,merge,fresh,gen}.rs`; `generate.rs` 15 LOC)
- Phase 5 doc-heading edges, install-hooks prompt, parser claims update, CHANGELOG
- Phase 6 verification gates + MCP golden-path CI step
- ADR-011 read snapshot (`graph.snapshot`) + writer-queue UX (visible `store.lock`
  wait, fjall open retry, `aden status` diagnostics)

---

## Current vs Target Architecture

```
TODAY (achieved)               NEXT
────────────────               ────
aden-cli (thin shell)          query.rs strangler (deferred)
  commands/*.rs wrappers
  indexer/ link merge fresh gen
aden-parse ──┐                 aden-parse (canonical, M1 branch)
aden-graph/parser.rs (dup) ──┘
aden-policy (advisory + ci)    optional enforce mode (0.3.0)
```

---

## Phase Status

| Phase | Status |
|-------|--------|
| 0 — Docs | **Done** |
| 1 — MCP auto-gen | **Done** |
| 2A — Scoped skip + stale hints | **Done** |
| 2B — check/heal/status JSON | **Done** |
| 2C — Store lock UX | **Done** |
| 3 — Advisory policy | **Done** |
| 4 — Strangler indexer | **Done** |
| 5 — Graph connectivity + polish | **Done** |
| 6 — Verification | **Done** |
| ADR-011 — Read snapshot + writer UX | **Done** (branch `wip/merge-engine-phase2-snapshot`) |

---

## Success Metrics

| Metric | Target | Actual |
|--------|--------|--------|
| `generate.rs` LOC | <400 | **15** |
| MCP `check` output | <2KB typical | ~1.1KB |
| Policy | All constitution blocks in `diagnose` | Advisory wired |
| Tests | PR-0 equivalence green | **2/2 pass** |
| Gates | `aden ready` + `ci-check` green | **PASS** |

---

## Deferred (not blocking golden state)

- M1 parse unification (`fix/parse-canonical`) — after indexer stable 2 weeks
- M3 BTree neighbor determinism (`fix/graph-determinism`)
- `query.rs` strangler (~3,200 LOC)
- Policy `enforce` mode (0.3.0)
- New `aden-compile` crate

## Branch inventory (2026-07-03 — clean state)

**Local:** only `main`. Worktrees and stash cleared. Tag
`backup/pre-branch-integration-2026-07-03` marks pre-PPR-merge `main`.

| Branch | Status |
|--------|--------|
| `origin/main` | **Ship from here** — golden state + gather-select/`--select` + batch gen + M16 alternates + PPR |
| `origin/chore/dep-currency-backup` | Dep bumps (criterion/toml); skip tokenizers bump — main uses kitoken |
| `origin/feat/lexical-overlay-perfbase` | Diverged eval branch; largely superseded — cherry-pick only if needed |
| `origin/feat/vocab-mismatch-evals` | Diverged assembly eval harnesses — optional rebase later |
| `origin/backup/dense-kitoken-stash` | Archived WIP (kitoken landed on main) |

**Merged into `main` (2026-07-03):** `origin/perf/store-batch-ingest` production commits
(gather-then-select, `--select`, batch fjall ingest, understand alternates, MCP parity).

**Merged / deleted (2026-07-03):** `feat/asm-ppr-ordering`, `feat/coxn-directional-prereqs`,
`integration/all-fixes`, `fix/audit-phase1-hygiene` (superseded or landed on `main`).

**Merge policy:** no wholesale merges of diverged branches. Rebase + CI + focused PR.

---

## GSTACK REVIEW REPORT

| Run | Skill | Status | Findings |
|-----|-------|--------|----------|
| 1 | plan-eng-review | PASS | Strangler over new crate; never `aden-index`; PR-0 equivalence test; M1 separate; M5 advisory |
| 2 | plan-devex-review | PASS | MCP UX improvements; scope ADEN_SKIP_AUTO_GEN; JSON summaries; protect understand→impact-diff loop |

**VERDICT:** Plan approved and executed. Golden state reached 2026-07-03.

---

*Living document. Re-prime with aden after large refactors.*