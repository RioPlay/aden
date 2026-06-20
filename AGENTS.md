# Agent Manifest: Aden

> **For AI agents** (Claude, Codex, Cursor, OpenCode, etc.) working on this repository.
> Use the **aden MCP tools** to understand and navigate the code — not raw
> `grep`/`find`/`cat`-walking. Every aden result is tagged with its enclosing symbol,
> which is the anchor you feed back into the graph.

## The graph is fresh by construction

Read tools (`ask`, `search`, `grep`, `locate`, `query`, `asm`) auto-reindex any file
that changed since the last run. You do **not** need to run `gen` before a session or
after your own edits. Only run `gen` after large *external* changes — cloning a new
repo, a big merge, or generated code appearing outside your edits.

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

**The graph is fresh by construction.** Read tools (`ask`, `search`, `grep`,
`locate`, `query`, `asm`) auto-reindex any file changed since the last run. You do
**not** need `gen` before a session or after your own edits — only after large
*external* changes (cloning, a big merge, generated code).

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
