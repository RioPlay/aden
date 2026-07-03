# Security Policy

## Supported Versions

aden is pre-1.0. Security fixes are applied to the latest `0.2.x` release line
only; there is no backporting to older point releases.

| Version | Supported          |
|---------|--------------------|
| 0.2.x   | :white_check_mark: |
| < 0.2   | :x:                |

## Reporting a Vulnerability

**Please do NOT open a public GitHub issue for security vulnerabilities.**

Report privately to the maintainer listed in
[MAINTAINERS.md](MAINTAINERS.md) — currently RioPlay
<rioplay@rioplay.dev>. Include:

- a description of the vulnerability and its impact,
- the affected version / commit,
- reproduction steps or a proof-of-concept, and
- any suggested remediation, if you have one.

### Response window

- **Acknowledgement:** within 7 days of your report.
- **Assessment & triage:** we aim to confirm or reject the issue, with a
  severity assessment, shortly after acknowledgement.
- **Coordinated disclosure:** we will agree a disclosure timeline with you and
  credit you in the release notes unless you ask to remain anonymous. Please
  give us a reasonable window to ship a fix before any public disclosure.

## Threat Model

aden is designed to ingest **untrusted repositories** — it parses, indexes, and
assembles context from source it did not author. Its defenses reflect that:

- **Path confinement.** `include::[]` directives and all MCP path arguments
  (`path`/`out`/`from`) are canonicalized and rejected if they resolve outside
  the project root — no `../../etc/passwd` traversal, no writing outside the
  workspace.
- **argv, not shell.** Commands are spawned with an explicit argument vector and
  a `--` end-of-options terminator, so attacker-controlled values cannot be
  interpreted as flags or injected into a shell.
- **MCP environment isolation.** The MCP server clears the host environment
  before spawning `aden` and re-injects only `ADEN_*`, `PATH`, `HOME`, `USER`,
  and `SHELL`. API keys and tokens from the host process do not leak to child
  commands.
- **Per-file panic isolation.** A malformed or pathological source file cannot
  abort an indexing run; parse failures are isolated per file.
- **Secret screening at index time.** Files are screened by path and by content
  for structured credentials (AWS/GitHub/OpenAI/Slack keys, PEM private-key
  blocks) before they enter the graph, so secrets are kept out of generated
  context.
- **Bounded execution.** MCP tool invocations are bounded by a command timeout
  so a single call cannot block indefinitely.

### MCP freshness note

MCP sets `ADEN_SKIP_AUTO_GEN=1` on **read-only** tool invocations so read tools
do not silently reindex while another writer may hold the store lock. Write tools
(`gen`, `sync`, `ready`) run without skip. This is a concurrency safeguard, not a
security boundary — but agents should know that MCP read results may reflect a
slightly stale graph until an explicit `gen`/`sync` runs. Read-tool JSON envelopes
include `index_stale` (and `stale_hint` when true) for machine-detectable staleness.
See [ADR-010](docs/adr-010-structural-remediation.adoc).

Concurrent read subprocesses on the same project load `graph.snapshot` when fresh
(ADR-011) instead of opening the live fjall store, reducing `FjallError: Locked`
contention under multi-agent use. Writers still serialize on `store.lock`.

For the full threat table, the include-directive rules, and the secret-scanning
layers, see [`docs/security-model.adoc`](docs/security-model.adoc).