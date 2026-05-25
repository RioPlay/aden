# Agent Manifest: Aden

> **For AI agents** (Claude, Codex, Cursor, OpenCode, etc.) working on this repository.

## Stop. Read This First.

This repository uses **AsciiDoc** (`.adoc`) as its native knowledge format — *not Markdown*.
All contracts, plans, templates, agent instructions, and architecture decisions are stored
as structured AsciiDoc so they can cross-reference each other via `[[anchors]]` and `<<refs>>`.

**Do not flatten this into Markdown. Use the AsciiDoc files directly.**

## Agent Onboarding (Read in Order)

1. **`.agent/onboarding.adoc`** — How to safely begin work (session locking, impact analysis,
   validation protocol). This is your canonical quick-start.
2. **`.agent/constraints.adoc`** — Hard negative constraints: never-commit rules, security posture,
   file-system boundaries, and workflow invariants.
3. **`.agent/templates/aden-guide.adoc`** — Full Aden command reference, token economy rules,
   common failure modes, and reference integrity rules.
4. **`.agent/context.adoc`** — Project identity, dependencies, and codebase-specific security
   patterns extracted from prior audits.
5. **`.agent/session.adoc`** — Current active sessions. **Always check this before starting work.**

## Essential Aden CLI Commands

Run these from the project root.

### Before touching existing code
```bash
aden ask --from <anchor> "What breaks if I change X?"
aden graph --depth 2 <anchor>
aden locate <symbol>
aden query --backlinks <anchor>  # Find what references this anchor
```

### After writing or modifying code
```bash
aden gen <file>          # Generate/update contract for modified source
aden gen <file> --format md    # Generate Markdown instead of AsciiDoc
aden check .             # Validate all <<refs>> resolve
```

### Linting and Testing
```bash
aden lint .              # Universal linter (Rust, Python, Go, TS, Java, etc.)
aden lint . --severity error  # Errors only
aden lint . --json      # JSON output for CI
aden test .             # Discover and run tests across all languages
aden test . --list      # List tests without running
```

### Multi-repository Management
```bash
aden federation list     # List repositories in workspace
aden federation add <path>  # Add repository to workspace
aden federation remove <name>  # Remove repository
```

### HTTP Server (for CI/agents)
```bash
aden mcp serve --port 3030  # Start HTTP server for CI integration
```

### Before every commit
```bash
aden ci-check .          # Run all local gates (check, heal, lint, tests, audit)
```

## Working with `.adoc` Files

- Every file declares a `[[unique-anchor]]` immediately before its title.
- Tables use `|===` with header rows.
- `<<anchor>>` creates typed, resolvable references.
- `include::path[]` composes documents into single-source-of-truth assemblies.
- **Always run `aden check .` after editing any `.adoc` file.**

## Conventions

- **Never edit `.adoc` contracts by hand** — regenerate with `aden gen`.
- **Never commit `contracts/` or `.aden/`** — they are build artifacts containing
  source paths and signatures. They are already in `.gitignore`.
- **Never ignore test failures.** If `cargo test --workspace` fails, fix before proceeding.
- **Append your session** to `.agent/session.adoc` before declaring work complete.
- **Never suppress warnings** without a `// SAFETY:` or `// REVIEW:` comment.

## Documentation Index

| Document | Purpose |
| --- | --- |
| `README.adoc` | Human-facing project overview |
| `docs/context.adoc` | Project glossary, conventions |
| `docs/plan-phase0.adoc` … `plan-phase6.adoc` | Phased roadmap |
| `docs/adr-001.adoc`, `adr-002.adoc` | Architecture decisions |
| `docs/module-*.adoc` | Per-crate contract summaries |
| `docs/use-cases.adoc` | Non-software use cases |
| `CONTRIBUTING.md` | DCO sign-off requirements |

## Architecture

| Crate | Responsibility |
| --- | --- |
| `aden-core` | Schema: `Document`, `Block`, `Edge`, `Symbol`, etc. |
| `aden-parse` | Language routers & AST extraction (Rust, Python, Go, TS/JS, C#, Java, Kotlin, PHP, Ruby + 305+ generic) |
| `aden-emit` | Deterministic AsciiDoc emitter |
| `aden-graph` | Referential integrity: DiGraph, cycle detection, typed edges |
| `aden-asm` | Context assembly: BFS traversal, token budgeting |
| `aden-index` | Semantic full-text search |
| `aden-heal` | Drift detection & health scoring |
| `aden-propose` | Patch generation & proposal lifecycle |
| `aden-mcp` | MCP server (Claude, Cursor, Zed, Windsurf) |
| `aden-lsp` | LSP for `.adoc`/`.aden` files |
| `aden-py` | PyO3 Python bindings |
| `aden-cli` | Binary (`aden`) with all commands |

---

*If you are an AI agent reading this, your next step is `.agent/onboarding.adoc`.*
