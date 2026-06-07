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

## Caveats

- Ground truth is single-target: each query has exactly one expected file. Real code
  retrieval often has several acceptable answers, so these recall numbers are a
  **lower bound** on usefulness.
- Numbers are self-run on these corpora; treat as indicative, not a standardized
  benchmark. Grow the query sets and add corpora to harden them.
- Hybrid runs one `aden` process per query, each loading the embedding model, so the
  hybrid sweep is slow — fine for an eval, not a latency measurement.
