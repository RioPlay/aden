# Agent Manifest: Aden

> **For AI agents** (Claude, Codex, Cursor, OpenCode, etc.) working on this repository.

## Quick Start

1. **Read `.agent/onboarding.adoc`** first
2. Check `.agent/session.adoc` for active sessions
3. Review `.agent/constraints.adoc` for hard rules

## Important: AsciiDoc Format

This repository uses **AsciiDoc** (`.adoc`), not Markdown.
- Files declare `[[anchors]]` before titles
- References use `<<anchor>>` (resolvable cross-references)
- Run `aden check . --severity Forbid` after editing `.adoc` files

## Essential Commands

### Before Changing Code
```bash
aden ask --from <anchor> "What breaks if I change X?"
aden query --from <anchor> --depth 2
aden locate --symbol <symbol>
```

### After Writing Code
```bash
aden gen <file>           # Generate contract
aden check . --severity Forbid  # Validate refs (fails only on critical)
```

### Testing & Linting
```bash
aden lint .               # Lint all languages
aden test .               # Run tests
aden ci-check .           # Full CI gates before commit
```

## Conventions

- **Never hand-edit the knowledge graph** — rebuild with `aden gen`
- **Never commit `.aden/`** — the build artifact (in `.gitignore`)
- **Never ignore test failures**
- **Append your session** to `.agent/session.adoc` before finishing
- **Never suppress warnings** without `// SAFETY:` or `// REVIEW:`

## Architecture

| Crate | Responsibility |
| --- | --- |
| `aden-core` | Schema: Document, Block, Edge, Symbol, three-way merge |
| `aden-parse` | Language routers & AST extraction (Rust, Python, Go, TS/JS, C#, Java, Kotlin, PHP, Ruby, +305 generic) |
| `aden-emit` | Deterministic AsciiDoc emitter |
| `aden-graph` | DiGraph, cycle detection, typed edges, integrity |
| `aden-asm` | Context assembly: BFS traversal, token budgeting |
| `aden-index` | Full-text search with fuzzy matching |
| `aden-heal` | Drift detection & health scoring |
| `aden-propose` | Patch generation & proposals |
| `aden-mcp` | MCP server (OpenCode, Claude, Cursor, Zed, Windsurf) |
| `aden-cli` | Binary (`aden`) |
| `aden-policy` | Constitutional directives, precedence |
| `aden-api` | REST/gRPC types |
| `aden-attest` | Attestation primitives |
| `aden-store` | fjall-backed (LSM-tree) graph store (store-first persistence) |
| `aden-diagnose` | Deterministic knowledge-graph diagnostics |
| `aden-lsp` | Language Server Protocol integration |
| `aden-simulate` | Change-impact simulation |
| `aden-telemetry` | Local telemetry primitives |

## Documentation

| Document | Purpose |
| --- | --- |
| `docs/module-aden-cli.adoc` | CLI reference |
| `docs/getting-started.adoc` | Quick start guide |
| `docs/architecture.adoc` | System architecture |
| `docs/context.adoc` | Glossary, conventions |
| `docs/plan-phase0.adoc` | Foundation roadmap |
| `docs/adr-*.adoc` | Architecture decisions |
| `docs/use-cases.adoc` | Non-software use cases |

---

*Your next step: `.agent/onboarding.adoc`*
== Modules

See: <<mod-aden-core>>, <<mod-aden-cli>>, <<mod-aden-graph>>
