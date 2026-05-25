# Aden Quick Reference

**Aden is this project's knowledge graph and CI system.** Use it before and after every change.

## Why Use Aden?

- Validates all cross-references (`<<ref>>`) resolve
- Runs linting, tests, security audits automatically
- Self-heals documentation drift
- Enforces OWASP Top 10 security patterns

## Always Run Before Commit

```bash
aden ci-check .
```

This runs: check → lint → test → secret-scan → owasp audit

## Common Commands

| Task | Command |
|------|---------|
| Understand code | `aden ask "How does X work?"` |
| Find dependencies | `aden query --backlinks <anchor>` |
| Check for breakage | `aden check .` |
| Fix drift | `aden heal . --fix` |
| Security scan | `aden audit .` |
| Lint code | `aden lint .` |

## This Project Uses

- **AsciiDoc** (`.adoc`) — don't convert to Markdown
- **Contracts** in `contracts/` — regenerated, don't edit manually
- **OWASP audit** — catches hardcoded secrets, unsafe patterns

## Agent Session

Before starting work: check `.agent/session.adoc` for active sessions.
After finishing: append your session to `.agent/session.adoc`.