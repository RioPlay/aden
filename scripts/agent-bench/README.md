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

Add `--model <model>` to pin a Codex model. A complete pilot is 28 model runs, including a misspelled-symbol journey that
measures whether agents follow Aden's deterministic suggestions without treating
them as automatically selected traversal targets:

```bash
python3 scripts/agent_bench.py --runs 1 --json results.json --md results.md
```

Use `--runs 3` for comparison-quality results.

## Multiple providers

Use the provider-neutral command engine for Anthropic, OpenAI, Google, local,
or future agent CLIs through a small adapter executable:

```bash
python3 scripts/agent_bench.py \
  --engine command \
  --agent-command './provider-adapter' \
  --provider anthropic \
  --model claude-example \
  --runs 1
```

The command is parsed directly into an argument vector and is never evaluated
by a shell. The harness prompt requires read-only work, but the external adapter
must enforce its provider's read-only sandbox. It runs in the selected repository
and receives:

* `ADEN_BENCH_PROMPT_FILE`
* `ADEN_BENCH_SCHEMA_FILE`
* `ADEN_BENCH_ANSWER_FILE`
* `ADEN_BENCH_TRAJECTORY_FILE`
* `ADEN_BENCH_REPOSITORY`
* `ADEN_BENCH_CONDITION`
* `ADEN_BENCH_PROVIDER`
* `ADEN_BENCH_MODEL`

It must write the schema-conforming JSON answer to `ADEN_BENCH_ANSWER_FILE`, or
print that JSON on stdout. It may write a JSON array of `{tool, name, command}`
records to `ADEN_BENCH_TRAJECTORY_FILE` (maximum 1,000 records; strings are
bounded). Aden-condition runs without a reported Aden invocation remain
method-noncompliant rather than receiving assumed credit.

Keep authentication, sandboxing, and provider SDKs in the external adapter; the
committed harness remains provider-neutral. Adapter trajectories are
self-reported evidence, not a security boundary, so published comparisons should
include the adapter and raw records. Reports record engine, provider, and model
so results cannot be accidentally conflated. The deterministic scorer is a
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
