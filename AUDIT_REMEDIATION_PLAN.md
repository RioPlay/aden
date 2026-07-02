# Aden Structural Remediation Plan (Gstack-Reviewed)

**Date**: 2026-07-02 (updated from 2026-06-30 audit)  
**Source**: Original gstack audit + `/plan-eng-review` + `/plan-devex-review` (2026-07-02)  
**Status**: Phases 1–3 **shipped on `main`**. Phase 0 docs **done** (2026-07-02). Phase 1 in-flight work pending. Phase 4 strangler refactor next.

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

Aden is in **good health** (tests green, `aden check --severity Forbid` passes, strong integration tests). The main debt is **structural concentration** in `aden-cli` god modules and **honesty gaps** between docs and MCP/runtime behavior.

**Shipped (mark done):**
- Phase 1 hygiene (L1 lint, L8 docs partial)
- Phase 2 CI (fmt + clippy in `.github/workflows/ci.yml`)
- Phase 3 M4 (MCP `env_clear()` + allowlist in `crates/aden-mcp/src/lib.rs`)
- M3 dedup (`AdenGraph::add_node` DuplicateAnchor, `add_edge` consistency)
- Partial H1 extractions (`cochange_pairs`, `extract_*` helpers → `util.rs`)

**In flight (uncommitted on `main`):**
- `ADEN_SKIP_AUTO_GEN` in `generate.rs` + unit tests
- MCP sets `ADEN_SKIP_AUTO_GEN=1` globally (needs scoping per Phase 2A)

**Highest risk:** `generate.rs` (~2,770 LOC, `cmd_gen_inner` ~1,400 LOC, **185 downstream nodes**)

---

## Current vs Target Architecture

```
TODAY                          TARGET
─────                          ──────
aden-cli (god crate)           aden-cli (thin shell)
  generate.rs 2770 LOC           commands/*.rs wrappers
  query.rs 3205 LOC            aden-cli/src/indexer/
aden-parse ──┐                   link.rs, merge.rs, fresh.rs, gen.rs
aden-graph/parser.rs (dup) ──┘ aden-parse (canonical)
aden-policy (ci bootstrap)     aden-policy (diagnose+lint+check)
```

**Hard boundaries:** `contract.rs`, `fjall_store.rs` internals — no touch without approval.

---

## Prioritized Issue List (updated)

### HIGH
| ID | Issue | Location | Status |
|----|-------|----------|--------|
| H1 | Write path concentration | `generate.rs` | **Active** — strangler to `indexer/` |

### MEDIUM
| ID | Issue | Status |
|----|-------|--------|
| M1 | Duplicate parsing (aden-parse vs aden-graph/parser.rs) | **Separate branch** after H1 PR-5 |
| M2 | CLI god crate | Partially addressed by H1; `query.rs` next |
| M3 | AdenGraph dedup + determinism | Dedup **done**; BTree ordering deferred |
| M4 | MCP env inheritance | **Done** |
| M5 | Partial policy wiring | **Phase 3** — advisory only |
| M6 | fmt/clippy in CI | **Done** |

### LOW / INFO
| ID | Issue | Phase |
|----|-------|-------|
| L3 | Orphan docs (59 info) | Phase 5 |
| L4 | Pre-commit hook friction | Phase 5 |
| L5/L6 | Parser claims + CSV | Phase 0 + 5 |
| L7 | Store lock contention | Phase 2C |
| L8 | Docs qualification | **Phase 0** |

---

## Phased Remediation Plan

### Phase 0 — Documentation Alignment (~1 day) `[DONE 2026-07-02]`

Docs only. No behavior change.

**New:**
- [x] `docs/adr-010-structural-remediation.adoc` — strangler indexer, advisory policy, MCP freshness model

**Update:**
- [x] `docs/architecture.adoc` — add `[[known-limitations]]`, `[[target-architecture]]`; qualify Constitutional Governance; fix language tiers; update crate map
- [x] `docs/index.adoc` — link ADR-010 (also ADR-008, ADR-009)
- [x] `SECURITY.md` — support `0.2.x`; MCP env + `ADEN_SKIP_AUTO_GEN`
- [x] `docs/security-model.adoc` — MCP env row; policy advisory depth
- [x] `docs/ai-integration.adoc` — MCP freshness subsection
- [x] `AGENTS.md` — align "fresh by construction" with MCP behavior

**Gate:** `aden check . --severity Forbid`

---

### Phase 1 — Complete In-Flight Work (~½ day) `[DONE 2026-07-02]`

- [x] Commit `ADEN_SKIP_AUTO_GEN` + tests (`generate.rs`)
- [x] Commit MCP `env_clear()` (already on main; verify `ADEN_SKIP_AUTO_GEN` scoping plan in Phase 2A)

**PR:** `fix/mcp-auto-gen-suppression`

---

### Phase 2 — Core Agent DX (~3–5 days)

Per gstack DX review (MCP UX scored 5/10; magical moment = `understand` → `impact-diff`).

**2A — Scoped `ADEN_SKIP_AUTO_GEN`** `[DONE 2026-07-02]`
- [x] Set skip only for **read** MCP tools (`Effect::Read` in `aden-mcp`)
- [x] Write tools (gen, ready, sync, heal, …) run without skip
- [x] Stale sentinel: text `index_stale=true` hint via `StaleHintGuard` (JSON field deferred to 2B)

**2B — Heal/Check JSON summaries**
- [ ] Add check/heal/status to MCP `structured_output_flags()`
- [ ] Schema: `{ok, counts, top_issues[], truncated}`
- [ ] Text mode: summary line + `--max-issues 20` for MCP
- [ ] Target: MCP `check` <2KB on aden self-repo

**2C — Store lock UX (L7)**
- [ ] Clear error: "store locked — another aden process holds the lock"

**Deferred to Phase 5:** doc-heading edges, install-hooks, status in Essential MCP surface

---

### Phase 3 — Advisory Policy Wiring (~2–3 days)

- [ ] `PolicyEngine` in `aden diagnose` — report unwired `[constitution]` blocks
- [ ] `Warn` directives surfaced in `aden lint`
- [ ] `policy_violations[]` in `check --json`
- [ ] Document `ADEN_POLICY_MODE=advisory` (default); enforce deferred to 0.3.0
- [ ] **Do not** block `gen` in this phase

---

### Phase 4 — Strangler Indexer (~2–4 weeks)

**PR-0 (gate — before any extraction):**
- [ ] Equivalence test: `build_from_storage` vs `build_from_directory` on fixture

| PR | Work | Tests |
|----|------|-------|
| 4-1 | Extract `link_store_edges` + resolvers → `indexer/link.rs` | wave1/2/3_edges |
| 4-2 | Extract `slim_doc`, `write_merge_proposals` → `indexer/merge.rs` | merge_engine_integration |
| 4-3 | Decouple `heal.rs` from `generate::slim_doc` | heal tests |
| 4-4 | Extract `ensure_fresh`, `recover_*` → `indexer/fresh.rs` | fresh_tests.rs |
| 4-5 | Extract `cmd_gen_inner` → `indexer/gen.rs`; `generate.rs` <400 LOC | gen_base_snapshot |

**Module layout:**
```
crates/aden-cli/src/indexer/
  mod.rs, link.rs, merge.rs, fresh.rs, gen.rs
```

**M1 parse unification:** separate branch `fix/parse-canonical` — merge only after 4-5 stable 2 weeks.

**M3 determinism:** `fix/graph-determinism` — BTree neighbor ordering.

**Guardrail:** `understand` → `impact-diff` loop must not regress (Aden's magical moment).

---

### Phase 5 — Graph Connectivity + Polish (~1 week)

- [ ] Link doc-heading anchors in `link_store_edges`
- [ ] `make install-hooks` + `install.sh` prompt (L4)
- [ ] Parser claims (L5/L6), CSV best-effort note
- [ ] `CHANGELOG.md` for 0.2.x

---

### Phase 6 — Verification (~1 day)

```bash
cargo fmt --all -- --check
cargo clippy --workspace --exclude aden-lsp -- -D warnings
cargo test --workspace --exclude aden-lsp
aden gen . && aden check . --severity Forbid
aden ready . && aden ci-check .
```

- [ ] MCP golden path CI: `grep → understand → impact-diff → check --json`
- [ ] Lightweight gstack re-audit
- [ ] Close tracking issues

---

## PR Dependency Graph

```
Phase0 (docs) ──┬──> Phase1 (MCP flag commit)
                └──> Phase2 (agent DX) ──> Phase3 (policy advisory)
                              │
                              v
                    Phase4 PR-0 (equivalence test)
                              │
                              v
                    Phase4 PR-1..5 (indexer strangler)
                              │
              M1 parse spike ─┘ (parallel, merge later)
                              v
                         Phase5 → Phase6
```

---

## Success Metrics

| Metric | Target |
|--------|--------|
| `generate.rs` LOC | <400 after Phase 4 |
| MCP `check` output | <2KB typical |
| Policy | All constitution blocks in `diagnose` |
| Tests | PR-0 equivalence green |
| Gates | `aden ready` + `ci-check` green throughout |

---

## NOT in Scope

- `contract.rs` internals
- Policy blocking `gen` by default (0.3.0)
- New `aden-compile` crate in initial Phase 4
- Deleting `aden-graph/parser.rs` in same PR as indexer move
- LSP ship/deprecate decision

---

## Investigation Commands (aden-first)

```bash
aden understand <symbol>
aden locate --symbol <name>
aden grep "<pattern>" --regex
aden query --backlinks "<anchor>" --impact "<anchor>"
aden impact-diff --since HEAD~1
```

---

## GSTACK REVIEW REPORT

| Run | Skill | Status | Findings |
|-----|-------|--------|----------|
| 1 | plan-eng-review | PASS | Strangler over new crate; never `aden-index`; PR-0 equivalence test; M1 separate; M5 advisory; Phases 1-3 already shipped |
| 2 | plan-devex-review | PASS | MCP UX 5/10; scope ADEN_SKIP_AUTO_GEN to read tools; JSON summaries for check/heal; protect understand→impact-diff loop |

**VERDICT:** Plan approved with locked decisions. Execute Phase 0 docs first, then Phase 1 commit, then Phase 2 agent DX.

**UNRESOLVED DECISIONS:** None — indexer home, policy mode, and DX scope locked 2026-07-02.

---

*Living document. Update after each phase. Re-prime with aden after large refactors.*