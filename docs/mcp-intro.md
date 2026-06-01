# Aden over MCP

This is the practical guide to running Aden as a [Model Context
Protocol](https://modelcontextprotocol.io) server so an AI client (Claude,
Cursor, Codex, Zed, Windsurf, opencode, …) can query your codebase graph
directly — no copy-pasting CLI output.

For the full tool/prompt reference and the recommended agent system prompt, see
[`ai-integration.adoc`](ai-integration.adoc).

## What the Aden MCP server is

`aden-mcp` is a **thin director**: it exposes Aden's CLI as MCP tools, where
each tool maps **1:1 to an `aden` subcommand** (~35 tools — `grep`, `ask`,
`understand`, `asm`, `query`, `locate`, `gen`, `heal`, `check`, `ready`,
`sync`, `list`, and so on). There is no separate logic path — calling the
`grep` tool runs the same code as `aden grep` on the command line, so anything
documented for the CLI applies verbatim to the MCP tools.

The server operates on the **target project you point it at** — nothing it
returns is specific to Aden's own source. Every result is derived from the
repository you serve.

## Prerequisites

The MCP server is a separate binary, `aden-mcp`. The repo's `./install.sh`
builds and installs **both** `aden` and `aden-mcp` to `~/.local/bin`:

```bash
./install.sh
aden --version
aden-mcp --version
```

> Note: `cargo install --path crates/aden-cli` installs only the `aden` CLI and
> **omits** `aden-mcp`. Use `./install.sh` (or build `crates/aden-mcp`
> explicitly) if you want the MCP server.

## Install into a client

```bash
aden mcp install --platform <platform>
```

Supported `<platform>` values (confirm with `aden mcp install --help`):

`claude`, `cursor`, `codex`, `zed`, `windsurf`, `opencode`

Useful flags:

- `--project <PATH>` — pin the project directory the server serves (defaults to
  the current directory).
- `--binary <PATH>` — point at a specific `aden-mcp` binary if it is not on
  `PATH`.
- `--dry-run` — print what would be written without touching any config.
- `--all` — install for every supported platform, not just detected ones.

Related subcommands: `aden mcp list` (show platforms and their install status),
`aden mcp uninstall --platform <platform>`, and `aden mcp serve` (run an HTTP
endpoint for CI/agent integration).

## Verify it works

1. Confirm the binary is reachable: `aden-mcp --version`.
2. Check the client config was written: `aden mcp list`.
3. **Fully restart your AI client** so it loads the new server.
4. In the client, confirm the `aden` tools appear (e.g. `grep`, `ask`, `asm`,
   `query`, `locate`). Ask it to run `grep` for a known string and confirm the
   matches come back tagged with their enclosing symbol.

If the tools do not appear, see [`troubleshooting.adoc`](troubleshooting.adoc).

## The key tools

**Understand (start here):**

- `understand` — one-shot symbol comprehension: resolves a bare name to its
  anchor, returns definition location, backlinks (callers), downstream impact,
  and an assembled context block. Replaces the manual `locate` → `query
  --backlinks` → `query --impact` → `asm` chain. Use this before touching any
  existing symbol.

**Read / navigate:**

- `ask` — natural-language question → dense, token-budgeted, graph-traversed
  context. Best first tool for exploratory questions.
- `grep` — structure-aware search; every match is tagged with its enclosing
  symbol. The enclosing-symbol name is your starting point for resolving an
  anchor.
- `locate` — find a symbol's definition **and its real `aden://…` anchors**.
- `list` — page through the anchors in the graph.
- `asm` — assemble context from a specific anchor within a token budget.
- `query` — graph traversal: `from` (outgoing edges), `backlinks` (who
  references this — blast radius), `impact` (downstream transitive reach).

**Anchors, not bare names.** `asm`/`query` take a full `aden://…` anchor (or a
module alias like `mod-<crate>`), not a bare symbol name — a bare name returns
`Anchor not found or ambiguous`. The flow is always: `grep`/`locate`/`list` to
get the `aden://…` anchor, then feed that anchor to `asm`/`query`. Or use
`understand` to skip all of this.

**Pre-commit / CI:**

- `ready` — fast pre-commit gate: gen + lint + check + heal drift + audit.
  Aden-only, no external tools. **Use before every commit.**
- `ci-check` — full gate suite including external tools (clippy, cargo audit,
  licenses). Use before push to remote.
- `sync` — reconcile the store after large merges or file deletions (gen +
  check + heal with gc). Not a routine pre-commit step — use `ready` for that.

**Maintain:**

- `gen` — compile source into the graph.
- `heal` — detect and propose fixes for drift between code and contracts.
- `check` — validate referential integrity (no broken refs, no duplicate
  anchors).
- `lint` — heuristic source checks; add `dead_code=true` to flag unreferenced
  symbols via the knowledge graph.

### You rarely need to call `gen`

The read tools (`ask`, `asm`, `query`, `locate`, `grep`) are **fresh by
construction**: they detect source files that changed since the last run and
re-index them automatically before answering. So you do *not* run `gen` before
each query. Only call `gen` after large external changes — cloning a new repo, a
big merge, or generated code appearing outside the agent's own edits.

## Security boundary

- **Path confinement.** Tools operate inside the served project directory; the
  `path`/`--project` argument defaults to that directory and the server does not
  reach outside it.
- **Timeout.** Long-running tools are bounded by a 120-second guard so a single
  call cannot hang the client indefinitely.
- **`watch` is terminal-only.** The continuous file-watch / live-reindex mode is
  a long-running daemon: it is listed as a tool but **is not usable over MCP**
  (the call will time out) — run `aden watch` in a terminal if you want it, and
  use `gen`/`sync` for one-shot updates.

## See also

- [`ai-integration.adoc`](ai-integration.adoc) — full tool and prompt
  reference, plus the recommended agent system prompt.
- [`getting-started.adoc`](getting-started.adoc) — the anchor-resolution
  workflow (`grep`/`locate` → `aden://…` anchor → `asm`/`query`).
- [`troubleshooting.adoc`](troubleshooting.adoc) — when the server or tools do
  not show up.
