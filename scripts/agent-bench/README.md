# Paired agent benchmark

This pilot measures whether Aden changes the outcome of repository-information
tasks, not only whether it compresses retrieved text. The same Codex model runs
each pinned task twice:

* `baseline` uses conventional `rg`/file/Git navigation and must not invoke Aden.
* `aden` uses adaptive navigation: direct text search for exact contracts and
  Aden retrieval/graph traversal for symbols, concepts, and relationships.

Both conditions are read-only and return the same structured answer/evidence
shape. The harness records tool trajectories, provider-reported usage when
available, wall time, required-fact recall, evidence recall, forbidden claims,
grounded completion, and whether each run obeyed its assigned navigation method.
Codex user configuration is disabled during runs so a globally pinned MCP server
cannot leak context across repositories; Aden-enhanced runs use the CLI with an
explicit project argument.

## Validate the corpus without model calls

```bash
python3 scripts/agent_bench.py --dry-run
```

Repository paths default to `~/Projects/<name>`. Override them with the
`path_env` variables in `tasks.json`. Revisions are pinned; use
`--allow-revision-mismatch` only for exploratory results.

## Run a small paired trial

```bash
python3 scripts/agent_bench.py \
  --task fjall-optimistic-commit \
  --runs 1 \
  --json /tmp/aden-agent-bench.json \
  --md /tmp/aden-agent-bench.md
```

Add `--model <model>` to pin a Codex model. A complete pilot is 24 model runs:

```bash
python3 scripts/agent_bench.py --runs 1 --json results.json --md results.md
```

Use `--runs 3` for comparison-quality results. The deterministic scorer is a
regression gate, not a prose-quality judge: publish its raw records and manually
or blindly review disputed answers before making product claims.

## Calibrate automatic context tiers

Before changing context-budget thresholds, sweep the committed tasks through
512/1,024/2,048/4,096-token strict retrieval:

```bash
python3 scripts/context_gate_bench.py \
  --aden-bin ./target/release/aden \
  --json /tmp/context-gate.json
```

The report compares the deterministic category/risk policy with the smallest
budget that preserves every required fact and evidence pattern. This retrieval
calibration is cheaper than a full agent run and identifies which tasks need a
paired agent confirmation.

The default `--routing policy` deterministically chooses graph traversal for
relationship/mechanism questions and anchored retrieval for explicit symbols and
normative contracts. Use `--routing ask` to isolate graph-first behavior or
`--routing adaptive` to isolate facet-query expansion, explicit-symbol lookup,
and parallel 128-token candidate probes before final assembly.

The intended product contract is outcome-only: routing, retries, tier expansion,
and candidate arbitration remain internal. Default output should contain one
assembled result or one concise actionable failure. Diagnostic receipts belong
only in structured JSON or an explicit explain mode. Native `aden ask` follows
this contract: normal output is the assembled outcome, while `--explain` restores
routing headers, summaries, and fallback diagnostics.

## Fixture engine

Tests and offline development can use `--engine fixture --fixture-dir DIR`.
Fixture files are named `<task>.<condition>.<run>.json` and contain the same
`answer`/`evidence` object required from Codex.
