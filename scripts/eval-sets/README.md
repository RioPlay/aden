<!-- Copyright (c) 2026 RioPlay <rioplay@rioplay.dev> -->
<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Real-repo retrieval eval sets

End-to-end retrieval evals that drive the real `aden` binary over external
repositories, measuring the product path a user actually hits (`aden gen` →
`aden search`). Run with [`../eval_corpus.py`](../eval_corpus.py).

These complement the in-process fixture eval (`crates/aden-index/tests/eval_corpus.rs`):
that one is hermetic and runs in CI; these point at large external repos that are
**not vendored**, so you reproduce them by cloning the repo yourself.

A query set is a TSV of `query <TAB> expected_path <TAB> note`, where `expected_path`
is a distinctive substring of the source file that should rank top. The harness keys
on the `:source_file:` path in each result, computes Recall@{1,5,10,20} and MRR@20.

## BM25 vs hybrid

The retrieval mode is decided by which binary you point at:

- **BM25** — the default build: `cargo build --release -p aden-cli`
- **Hybrid** (BM25 + local dense embeddings via RRF) — the dense build:
  `cargo build --release -p aden-cli --features dense` (needs the bge model, see
  `scripts/fetch-bge-model.sh`).

Each binary's `gen` writes the index in its own mode, so pass `--gen` (or `gen` once)
with the matching binary before searching.

## Results (recorded 2026-06-07)

### `t3-cli-src.queries.tsv` — create-t3-app, `cli/src` (70 queries)

| Mode | R@1 | R@5 | R@10 | R@20 | MRR@20 |
|------|-----|-----|------|------|--------|
| BM25 | 0.229 | 0.429 | 0.529 | 0.614 | 0.314 |
| **Hybrid** | **0.371** | **0.643** | **0.700** | **0.700** | **0.482** |

Hybrid lifts R@1 by ~62% relative and R@5 by ~50% relative over BM25 — a clear,
honest win on natural-language code queries.

### `linux-subset.queries.tsv` — Linux kernel subsystems (199 queries)

Scale run over **4,114 real kernel `.c` files** (~134,800 symbols across mm,
kernel, fs, net, block, ipc, security, crypto, lib, init), 199 iconic-file queries.

| Mode | R@1 | R@5 | R@10 | R@20 | MRR@20 |
|------|-----|-----|------|------|--------|
| BM25 | 0.362 | 0.472 | 0.558 | 0.608 | 0.417 |
| **Hybrid** | **0.487** | **0.673** | **0.739** | **0.819** | **0.571** |

Hybrid's lead *widens* at scale: R@1 +35% relative, R@5 +43%, MRR +37%, and misses
halve (78→36 of 199). On a large, unfamiliar C codebase, dense recovers a lot of
queries BM25's exact-term matching misses.

> **Read these as a lower bound, not a leaderboard.** Ground truth is
> *single-target* (exactly one expected file per query), so a result that surfaces
> an equally-valid sibling file counts as a miss — real usefulness is higher than
> the raw recall. The numbers are *self-run on one corpus with labels we authored*:
> indicative of how dense compares to BM25 here, **not** a standardized benchmark,
> and your repo will differ. The point of shipping the harness + query sets is so
> you can measure the BM25-vs-hybrid trade-off **on your own codebase** (see
> Reproduce) rather than trust ours. Retrieval is also file-level routing, not
> end-to-end task success.
>
> **Cost to be aware of.** Hybrid needs a one-time cold embed of the corpus
> (~24 min for this 4,114-file subset on CPU via the pure-Rust `tract` runtime).
> After that it is incremental: a reindex re-embeds only changed symbols (no-op
> reindex +0 / ~1.4s; one-file edit ~2.5s), because embeddings persist in a
> content-addressed cache keyed by each symbol's source hash. Speeding up the cold
> build (smaller model / GPU / a faster runtime) and an ANN index for query latency
> are separate follow-ons.

## Reproduce

### create-t3-app (T3)

```bash
git clone --depth 1 https://github.com/t3-oss/create-t3-app.git /tmp/t3-eval
# This is a code-retrieval eval: exclude doc-markdown so docs don't outrank code.
printf '.changeset/\n*.md\n*.mdx\n' > /tmp/t3-eval/.adenignore

# BM25
python3 scripts/eval_corpus.py --bin ./target/release/aden \
  --repo /tmp/t3-eval --queries scripts/eval-sets/t3-cli-src.queries.tsv --gen
# Hybrid: rebuild with --features dense, then rerun (the --gen re-embeds).
```

The full repo also has a `cli/template/` permutation tree and a multilingual `www/`
docs site; both are poor single-target corpora (many files validly answer one
query), so the published number is scoped to `cli/src`, the CLI's distinct logic.

### Linux kernel (subset)

Full `torvalds/linux` (~60k files) is heavy to gen; this eval uses a subset of
iconic subsystems so `gen` is fast (~24s) and ground truth is authorable:

```bash
git clone --depth 1 https://github.com/torvalds/linux.git /tmp/linux-eval
mkdir -p /tmp/linux-subset
for d in mm kernel fs net block ipc security crypto lib init; do
  mkdir -p "/tmp/linux-subset/$d" && cp -r /tmp/linux-eval/$d/* "/tmp/linux-subset/$d/"
done
python3 scripts/eval_corpus.py --bin ./target/release/aden \
  --repo /tmp/linux-subset --queries scripts/eval-sets/linux-subset.queries.tsv --gen
```

### `kin-openapi.queries.tsv` — getkin/kin-openapi, `openapi3/` (M6 pilot, 2026-06-09)

First repo from the M6 breadth corpus, and the first to use **semi-automated query
authoring** (`../gen_queries.py`: commit-message → single touched file) followed by the
**manual spot-check gate**. 30 candidate labels were generated; spot-check excluded 8
whose commit subject describes a cross-cutting *effect* that lives in another file (not
the touched file) — a **73% spot-check pass rate**, published here next to the number.

```bash
git clone --depth 1000 https://github.com/getkin/kin-openapi.git ~/Projects/eval-repos/kin-openapi
python3 scripts/gen_queries.py --repo ~/Projects/eval-repos/kin-openapi --scope openapi3 \
  --ext .go --max 30 --out scripts/eval-sets/kin-openapi.queries.tsv      # then spot-check by hand
python3 scripts/eval_corpus.py --bin ./target/debug/aden \
  --repo ~/Projects/eval-repos/kin-openapi --queries scripts/eval-sets/kin-openapi.queries.tsv --gen
```

| Set | Mode | R@1 | R@5 | R@10 | R@20 | MRR@20 |
|-----|------|-----|-----|------|------|--------|
| 30 candidates (pre-spot-check) | BM25 | 0.200 | 0.233 | 0.267 | 0.367 | 0.231 |
| 22 validated (spot-checked) | BM25 | 0.273 | 0.318 | 0.364 | 0.500 | 0.314 |
| **22 validated** | **Hybrid** | **0.364** | **0.455** | **0.545** | **0.636** | **0.404** |

Two findings, both honest:

1. **The spot-check gate is load-bearing.** Removing 8 cross-cutting-effect labels lifts BM25
   R@1 0.20 → 0.27 (and R@20 0.37 → 0.50). Raw auto-generated labels are *not* publishable;
   the manual gate (73% pass) is the difference between noise and a real number.
2. **Hybrid beats BM25 on this new corpus too:** R@1 +33% rel (0.273 → 0.364), R@5 +43%,
   R@10 +50%, MRR +29%, misses 11 → 8 — consistent with t3-cli (R@1 +62%) and Linux (+35%).
   The dense lift generalises to a third, independent repo.

Validated BM25 R@1 (0.273) is in line with the t3-cli BM25 baseline (0.229). Hybrid run with
`target/release/aden` (`--features dense`) + bge-small; `gen` re-run per mode (each binary writes
the index in its own mode).

### M6 pilot summary — 3 repos, 3 languages (2026-06-09)

All three via `gen_queries.py` (commit→single-file) + manual spot-check, BM25 (debug bin) vs
Hybrid (`--release --features dense` + bge-small). Validated (spot-checked) sets:

| Repo | Lang | Scope | N | BM25 R@1 / R@20 | Hybrid R@1 / R@20 | Δ R@1 |
|------|------|-------|---|-----------------|-------------------|-------|
| getkin/kin-openapi | Go | `openapi3/` | 22 | 0.273 / 0.500 | **0.364 / 0.636** | +33% |
| rust-lang/rustfmt | Rust | `src/` | 21 | 0.095 / 0.238 | **0.191 / 0.286** | +100% |
| unoplatform/uno | C# | `src/Uno.UI/Controls` | 20 | 0.150 / 0.200 | **0.150 / 0.300** | flat (R@20 +50%) |

**Findings (the pilot's job — surface these before scaling to 24 repos):**

1. **Hybrid ≥ BM25 on every repo** — the dense lift reproduces across Go/Rust/C# (and t3/Linux),
   though magnitude varies. Strongest where BM25 is weakest (rustfmt R@1 +100%).
2. **Absolute recall tracks corpus/method fit, not just aden.** The commit→file query method shines
   when files are named by *domain* (kin-openapi: `schema.go`, `parameter.go` ↔ subjects that name
   the type) and struggles when files are named by *platform/concern* with churny cleanup commits
   (uno Controls: platform renderers + ambiguous `.iOS`/`.Android` variants; rustfmt: behavioural
   subjects ↔ generic `expr.rs`/`chains.rs`). **Sub-tree selection matters** — pick logic-dense,
   domain-named modules; uno Controls was a poor pick (a logic module like `DataBinding` likely fares
   better).
3. **The spot-check gate is non-negotiable and corpus-dependent** — pass rates ran 73% (kin-openapi),
   66% (rustfmt), 67% (uno). Raw auto-labels are never publishable.

## Caveats

- uno used basename matching against an isolated sub-tree copy (6 platform-variant basenames collide →
  slightly lenient for those rows).
- Ground truth is single-target: each query has exactly one expected file. Real code
  retrieval often has several acceptable answers, so these recall numbers are a
  **lower bound** on usefulness.
- Numbers are self-run on these corpora; treat as indicative, not a standardized
  benchmark. Grow the query sets and add corpora to harden them.
- Hybrid runs one `aden` process per query, each loading the embedding model, so the
  hybrid sweep is slow — fine for an eval, not a latency measurement.
