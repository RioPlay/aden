# Agent Manifest: Aden

> **For AI agents** (Claude, Codex, Cursor, OpenCode, etc.) working on this repository.
> Use the **aden MCP tools** to understand and navigate the code — not raw
> `grep`/`find`/`cat`-walking. Every aden result is tagged with its enclosing symbol,
> which is the anchor you feed back into the graph.

## The graph is fresh by construction (shell and MCP)

Read tools (`ask`, `search`, `grep`, `locate`, `query`, `asm`, `understand`) auto-reindex
when the working tree changed. You do **not** need `gen` in the normal edit loop.
Shell and MCP share this contract. Only run `gen` after large *external* changes —
cloning a new repo, a big merge, or generated code appearing outside your edits.

**Freshness contract:** Explore tools may answer from the last snapshot while a refresh
runs and label `freshness` / `index_stale` in JSON. Blast-radius tools (`understand`,
`impact-diff`) wait briefly for an in-flight refresh. Silent gen never queues behind a
writer for minutes — it fail-opens so agents are not frozen. Escape hatch:
`ADEN_SKIP_AUTO_GEN=1` freezes the index (CI / offline). See
`docs/ai-integration.adoc`.

**Workspace:** `aden-mcp` auto-detects the open project (MCP Roots / host workspace env).
Do not manually re-point the MCP server at a project path for normal multi-repo use.

**Concurrent reads:** `gen` publishes `graph.snapshot` (ADR-011) so multiple readers
load the snapshot instead of opening fjall. Writers single-flight on `store.lock`;
`aden status` shows the active holder and snapshot path.

> **Subagents:** MCP instructions are not inherited automatically — tell spawned agents
> to use aden tools first; they do **not** need a separate freshness protocol.

## Which tool, when

| Goal | Tool |
| --- | --- |
| Structure-aware content search (returns enclosing symbol = anchor) | `grep "pattern"` |
| Natural-language question over the code/docs | `ask "how does X work?"` |
| Keyword retrieval of relevant context | `search "keywords"` |
| Find a symbol's definition + call sites | `locate --symbol <name>` |
| Assemble a token-budgeted context bundle around an anchor | `asm <anchor>` |
| Blast radius — what references this (before a refactor) | `query --backlinks <anchor>` |
| Blast radius — downstream reach | `query --impact <anchor>` |
| Walk the graph N hops from an anchor | `query --from <anchor> --depth 2` |

**Canonical flow:** `grep` to find the structure → take the enclosing symbol it returns
→ feed that anchor to `asm`/`query` to traverse → `ask` for an explanation.

## Validate, heal, test

- `check . --severity Forbid` — validate `<<anchor>>` refs (fails only on critical)
- `heal` — detect drift and resync contracts with the code
- `diagnose` — deterministic knowledge-graph diagnostics
- `lint .` — lint all languages
- `test .` — run tests
- `ready .` — fast pre-commit gate (gen + lint + check + heal drift + audit; aden-only, no external tools)
- `ci-check .` — full CI gate before push (adds external tools on top of `ready`)

## Conventions

- **Never hand-edit the knowledge graph** — rebuild with `gen`
- **Never commit `.aden/`** — build artifact, in `.gitignore`
- **Never ignore test failures**
- This repo uses **AsciiDoc** (`.adoc`): `[[anchors]]` precede titles, `<<anchor>>`
  cross-references must resolve. Run `check . --severity Forbid` after editing `.adoc`.

> Running aden from the shell instead of the MCP tools? Prefix each command with `aden`
> (e.g. `aden grep "pattern"`). `path` defaults to the project directory.

<!-- BEGIN aden:guidance (managed by `aden init` — edit outside this block) -->
## Using aden

Use the **aden** MCP tools (or `aden <cmd>` on the shell) to navigate this
codebase — not raw `grep`/`find`. Every aden result is tagged with its enclosing
symbol, which is the anchor you feed back into the graph.

**The graph is fresh by construction** on shell and MCP — read tools auto-reindex
changed files. JSON may include `freshness` / `index_stale` when answering from a
snapshot during refresh. Only run `gen` after large *external* changes.

| Goal | Tool |
| --- | --- |
| Structure-aware search (returns enclosing symbol = anchor) | `grep "pattern"` |
| Natural-language question over the code | `ask "how does X work?"` |
| Find a symbol's definition + call sites | `locate --symbol <name>` |
| Token-budgeted context bundle around an anchor | `asm <anchor>` |
| Blast radius — what references this | `query --backlinks <anchor>` |
| Blast radius — downstream reach | `query --impact <anchor>` |

**Flow:** `grep` → take the enclosing symbol → `asm`/`query` to traverse → `ask`
to explain. Validate with `check . --severity Forbid`; resync drift with `heal`.

See `.agent/aden-guide.adoc` for the full reference.
<!-- END aden:guidance -->
