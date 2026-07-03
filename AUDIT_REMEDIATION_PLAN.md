# Aden Structural Remediation Plan (Gstack-Reviewed)

**Date**: 2026-07-02 (updated 2026-07-03)  
**Source**: Original gstack audit + `/plan-eng-review` + `/plan-devex-review` (2026-07-02)  
**Status**: **Golden state reached** — Phases 0–6 shipped on `main` (2026-07-03).

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

## Branch inventory (2026-07-03)

All local feature branches backed up to `origin/*`. Tag
`backup/pre-branch-integration-2026-07-03` marks `main` before any future merges.

| Branch | Status | Action |
|--------|--------|--------|
| `origin/main` | **Current** — golden state + `index_stale` JSON | Ship from here |
| `origin/feat/asm-ppr-ordering` | 2 unique commits (PPR); cherry-pick conflicts on `aden-graph` | Rebase + PR when ready |
| `origin/feat/coxn-directional-prereqs` | Largely **superseded** on `main` (G4 lock, config, scope gate) | Archive / delete after diff audit |
| `origin/integration/all-fixes` | Largely **superseded** on `main` (MCP surface tiers) | Archive / delete after diff audit |
| `origin/chore/dep-currency-backup` | 4 dep bumps; independent | PR when convenient |
| `origin/feat/lexical-overlay-perfbase` | **Diverged** (+78/−77 vs `main`); core levers already via PRs #38–40 | Do not merge wholesale; cherry-pick if needed |
| `origin/feat/vocab-mismatch-evals` | **Diverged** (+59/−77); `--select` assembly evals | Rebase + PR as separate effort |
| `origin/perf/store-batch-ingest` | **Diverged** (+69/−77); batch ingest + benches | Rebase + PR as separate effort |
| `origin/fix/audit-phase1-hygiene` | **Stale** (0 ahead); precursor to golden-state indexer | Safe to delete |

**Merge policy:** no wholesale merges of diverged branches into `main`. Integrate via
rebase + CI + focused PR per branch. PPR attempted 2026-07-03 — conflict in
`crates/aden-graph/src/lib.rs`; aborted to keep `main` green.

---

## GSTACK REVIEW REPORT

| Run | Skill | Status | Findings |
|-----|-------|--------|----------|
| 1 | plan-eng-review | PASS | Strangler over new crate; never `aden-index`; PR-0 equivalence test; M1 separate; M5 advisory |
| 2 | plan-devex-review | PASS | MCP UX improvements; scope ADEN_SKIP_AUTO_GEN; JSON summaries; protect understand→impact-diff loop |

**VERDICT:** Plan approved and executed. Golden state reached 2026-07-03.

---

*Living document. Re-prime with aden after large refactors.*