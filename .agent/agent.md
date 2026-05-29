# Aden Quick Reference

**Aden is this project's knowledge graph and CI system.** Use it before and after every change.

## Why Use Aden?

- **Token-dense context** — sip tokens, don't gulp. Get exactly what you need.
- **Validates cross-references** — every `<<ref>>` resolves to a real `[[anchor]]`
- **Self-heals drift** — detects when code changes but docs don't
- **Graph traversal** — understand relationships, not just files

## Quick Wins for Agents

### Understanding Code (Start Here)

```bash
# Best for general questions - finds relevant anchor automatically
aden ask "How does authentication work?"

# Pin to a specific module for more precise context
aden ask "How does parsing work?" --from mod-aden-parse

# Get raw context without filtering (for debugging)
aden asm --from <anchor> --depth 3 --format aden
```

**Pro tip:** `aden ask` routes to an anchor, then does BFS traversal with token budgeting. Use `--from` when you know the module.

### Finding Things Fast

```bash
# Full-text search across all contracts
aden search "parse_file"

# List all symbols in a module
aden list --filter "mod-aden-parse"

# Find where a function is defined and called
aden locate --symbol parse_file
```

### Understanding Relationships

```bash
# Query the graph - what depends on this?
aden query --backlinks mod-aden-core

# Or what does it depend on?
aden query --from mod-aden-core --depth 2
```

## Always Run Before Commit

```bash
aden ci-check .
```

This runs: check → heal → lint → test → audit → clippy

## Common Commands

| Task | Command |
|------|---------|
| Understand code | `aden ask "How does X work?"` |
| Find dependencies | `aden query --backlinks <anchor>` |
| Check for breakage | `aden check .` |
| Fix drift | `aden heal . --fix` |
| Project health | `aden status .` |
| Get precise context | `aden asm --from <anchor> --depth 2` |
| Find symbols | `aden locate --symbol foo` |

## Token Budgeting

- Default budget: 4096 tokens (~3KB)
- Use `--budget` to adjust: `--budget 8192` for more context
- Depth matters more than budget for relevance
- `aden ask` selects depth automatically based on intent

## This Project Uses

- **AsciiDoc** (`.adoc`) — don't convert to Markdown
- **Contracts** in `contracts/` — regenerated, don't edit manually
- **Graph edges** — `Uses`, `Calls`, `Documents`, `PartOf`, `IsA`, etc.

## Agent Session

Before starting work: check `.agent/session.adoc` for active sessions.
After finishing: append your session to `.agent/session.adoc`.

## MCP Integration

For direct tool access via MCP:
```bash
aden mcp install --platform opencode  # or claude, cursor, etc.
```