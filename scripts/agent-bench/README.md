# Paired agent benchmark

This pilot measures whether Aden changes the outcome of repository-information
tasks, not only whether it compresses retrieved text. The same Codex model runs
each pinned task twice:

* `baseline` uses conventional `rg`/file/Git navigation and must not invoke Aden.
* `aden` follows the selected, explicitly versioned protocol below.

The default `--protocol legacy-retrieval-v1` preserves the historical deterministic
retrieval experiment: exact ask-first command, fixed strict budget, immediate
synthesis, and one first-candidate recovery for symbol-resolution errors. This
measures that prescribed retrieval workflow; it cannot measure free tool choice.

Opt in with `--protocol agent-usage-v1` for a neutral investigation handoff. It
allows direct symbol navigation, exact source inspection, evidence-based follow-up,
and unresolved ambiguity. It specifies no first tool or example invocation. Precise
behavior and security claims require source inspection, and graph-derived inference
must remain distinguishable from inspected facts. Both protocols remain read-only.
The frozen `tasks.json` is shared and unchanged; add new scenarios in separate corpora.

Both conditions are read-only and return the same structured answer/evidence
shape. The harness records tool trajectories, provider-reported usage when
available, wall time, required-fact recall, evidence recall, forbidden claims,
grounded completion, and whether each run obeyed its assigned navigation method.
Codex user configuration is disabled during runs so a globally pinned MCP server
cannot leak context across repositories; Aden-enhanced runs use the CLI with an
explicit project argument.

`--transport cli` remains the default. For `agent-usage-v1`, command and fixture
engines also accept `--transport mcp`. That neutral prompt permits the configured
isolated server, with the same evidence requirements and no first-tool hint.
The external adapter must isolate MCP to the assigned repository. Native Codex
runs remain CLI-only. Transport is recorded and mixed transports cannot be pooled.

## Validate the corpus without model calls

```bash
python3 scripts/agent_bench.py --dry-run
python3 scripts/agent_bench.py --protocol agent-usage-v1 --dry-run
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

Add `--model <model>` to request a model, and repeat `--model-setting KEY=VALUE`
for explicit configuration overrides (for example, `model_reasoning_effort='"low"'`).
Codex receives each override through `-c`; external adapters receive the exact
list through their environment and must apply or reject it. Unobserved effective
model/settings remain null in the report rather than being inferred from requests.
A complete corpus pass is 28 runs, including a misspelled-symbol recovery task:

```bash
python3 scripts/agent_bench.py --runs 1 --json results.json --md results.md
python3 scripts/agent_bench.py --protocol agent-usage-v1 --runs 1 --json usage-v1.json --md usage-v1.md
```

Use `--runs 3` for comparison-quality results.

JSON report schema 2 records protocol, exact per-run prompts and SHA-256 hashes,
answer schema/version/hash, requested model/settings, harness and corpus hashes,
source revisions, tracked dirty patches, untracked-file hashes, and resolved
binary hashes. Missing binaries have explicit unknown identities. Set
`ADEN_BENCH_BIN` to pin the Aden executable. External adapter command and directly
named file arguments are fingerprinted; transitive dependencies are not captured.
Repository state is captured before the read-only trials. Reports from the two
protocols have distinct labels and cannot be pooled by the aggregate function.

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
* `ADEN_BENCH_EVENTS_FILE`
* `ADEN_BENCH_REPOSITORY`
* `ADEN_BENCH_CONDITION`
* `ADEN_BENCH_PROVIDER`
* `ADEN_BENCH_MODEL`
* `ADEN_BENCH_PROTOCOL`
* `ADEN_BENCH_TRANSPORT`
* `ADEN_BENCH_MODEL_SETTINGS` (JSON array of explicit `KEY=VALUE` overrides)

It must write the schema-conforming JSON answer to `ADEN_BENCH_ANSWER_FILE`, or
print that JSON on stdout. It may write a JSON array of `{tool, name, command}`
records to `ADEN_BENCH_TRAJECTORY_FILE` (maximum 1,000 records; strings are
bounded). Aden-condition runs without a reported Aden invocation remain
method-noncompliant rather than receiving assumed credit.

Adapters may write `{"events": [...], "complete": true}` to `ADEN_BENCH_EVENTS_FILE`.
Events use `item.started` / `item.completed` with a shared `item.id`, and either
`type: "command_execution"` with command/exit_code/aggregated_output or
`type: "mcp_tool_call"` with server/tool/arguments/result. Explicit failures use
`error.kind` of `shell`, `transport`, `protocol`, or `tool`. Adapter events and
trajectories are always labelled `adapter_self_reported`; supplying `complete`
does not promote their provenance to observed.

Native events are retained for replay. `telemetry_from_events` correlates starts
and completions, deduplicates identical IDs/records, and counts only the supplied
observer scope. Unknown event shapes, missing IDs/completions, absent output,
and incomplete captures produce null metrics rather than assumed zero. MCP
results without an explicit `isError` remain conservatively unknown for outcome
classification (a valid `needs_narrowing` state is counted separately). Output
bytes describe UTF-8 text content or structured payload, not transport framing;
mixed media output remains unknown. Exact repeated requests following a failed
matching request are labelled inferred retries, not claimed agent intent.
Tool-call means include only complete observed telemetry, with their contributing
run count reported separately. They never cover unobserved host edits/actions.

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

## Supervised agent-usage pilot

`agent-usage-scenarios.json` defines five separate cases, each repeated in three
fresh sessions. It does not modify the locked retrieval corpus. Review answers
against `agent-usage-rubric.json`; pattern scores do not establish semantic
correctness. Freeze the exact prompts, fixture source, binary hashes, startup
instructions and schemas before execution. Keep first answers separate from
reviewer-assisted corrections.

`scripts/agent_bench_probe.py` observes an isolated MCP server over stdio. Its
`--config` manifest contains `binary` entries for `aden.exe` and `aden-mcp.exe`
(each with `path`) and a `runs` mapping. Each run defines `project`, `data`,
`scenario`, and `skip_auto_gen`. Example invocation:

```powershell
python scripts/agent_bench_probe.py --config target/agent-usage-pilot/runs.json --run known-1 --tool locate --arg symbol=preserve_cli_output_for_mcp
```

The observer appends correlated raw events to the run's `events.jsonl`. Use
`--log setup-events.jsonl` or `--log evaluator-events.jsonl` to keep setup and
review calls outside model measurements. `source` reads an exact relative-file
range of at most 160 lines. `tests` runs locked Cargo tests only for the coding
scenario. The latter is a supervised permission variant; the ordinary benchmark
runner remains read-only. Fixture isolation is enforced by task instructions
and post-run source comparison, not by an OS sandbox.

Replay a completed observer log with
`telemetry_from_events(events, complete=True, source="stdio_observer")`. Mark
incomplete captures accordingly. Each adapter request starts a fresh server, so
latency includes initialization; this does not measure persistent connections.
Host commands and edits outside the adapter are unobserved, not zero.

The [2026-09-19 pilot report](agent-usage-pilot-2026-09-19.json) retains per-run
outcomes and frozen identities for 15 baseline sessions and 8 matched follow-up
sessions. The [canonical execution record](../../docs/roadmap/packets/DX-102-mcp-agent-contract.adoc)
explains the decision and limitations. Raw local evidence is intentionally kept
under ignored `target/agent-usage-pilot/`, not committed as build output.
