# Aden: A Dense Referential Context Compiler

**Aden** transforms codebases into traversable knowledge graphs, making the structure of understanding explicit, machine-readable, and queryable by both humans and AI agents.

## The Problem

Large language models are capable of sophisticated reasoning, but they are constrained by a finite context window. When an AI agent is dropped into a codebase of 100,000+ lines, it faces the same problem a human faces: **information overload**. The agent does not know which 10 files out of 500 are relevant to the task at hand. It does not know that changing `Database::connect()` will break `QueueWorker::drain()`. It has no mental map of the system.

## What Aden Does

Aden compiles source code, documentation, notes, and plans into a **knowledge graph** where:

- Every function, module, and decision becomes a **node**
- Every relationship (imports, calls, constraints, justifications) becomes a **typed edge**
- You can ask questions like "what depends on this function?" or "what is the blast radius of changing this module?"

```
Source Code → Aden Pipeline → Knowledge Graph → Context for AI
```

## What Aden Replaces

| Replaces | Why |
|----------|-----|
| Static analysis tools (clippy, Semgrep) | Aden finds *semantic relationships*, not bugs |
| Documentation generators (Rustdoc, Javadoc) | Aden produces *machine-navigable* context, not HTML |
|grep + manual file hunting | Aden lets you query by intent, not keywords |
| Scrolling through READMEs | Aden assembles exactly the context you need |

## Quick Start

```bash
# Install (builds release, copies to ~/.local/bin, adds to PATH)
./install.sh

# Initialize your project (optional — read commands auto-build the index)
cd your-project
aden init

# Compile the whole codebase into the knowledge graph
aden gen . --auto

# Ask a natural-language question — returns dense, connected context
aden ask "How does login work?"

# Structure-aware search: every match tagged with its enclosing symbol
aden grep "hash_password"

# Find a symbol's definition with an exact line number
aden locate --symbol login

# Blast radius before a refactor — who depends on this symbol?
aden query --backlinks hash_password

# Assemble a module (or symbol) overview within a token budget
aden asm --from mod-<crate> --depth 1
```

The graph is **fresh by construction**: read commands (`ask`/`asm`/`query`/
`locate`/`grep`) detect changed source and re-index it automatically, so you
rarely need to run `gen` by hand.

## Core Commands

| Command | Purpose |
|---------|---------|
| `aden gen` | Compile source into the knowledge graph (symbols, call edges, docs) |
| `aden ask` | Natural-language question → dense, graph-traversed context |
| `aden grep` | Structure-aware search — every hit tagged with its enclosing symbol |
| `aden asm` | Assemble context from an anchor within a token budget |
| `aden query` | Graph queries: `--from`, `--backlinks` (callers), `--impact` |
| `aden locate` | Find symbol definitions with exact line numbers |
| `aden check` | Validate referential integrity |
| `aden lint` | Fast, language-agnostic heuristic checks |
| `aden heal` | Detect drift between code and contracts |

## Why AsciiDoc?

- **Human-readable** — open any `.adoc` file and understand it
- **Machine-parseable** — regular grammar, no complex toolchains
- **Version-control-friendly** — diffs cleanly in Git
- **Referential by default** — the `<<anchor>>` syntax builds the graph naturally

## Supported Languages

Aden is language-agnostic: `aden gen` discovers and parses **every** file type it
has a grammar for — not just whichever build manifest happens to be present — and
indexes Markdown/AsciiDoc documentation alongside code.

- **Deep extraction** (call graph, signatures, doc comments): Rust, Python, Go,
  TypeScript/JavaScript, Java, C#, C, Ruby, PHP, Kotlin, PowerShell.
- **Generic extraction** (symbols + structure): 305+ further languages via
  tree-sitter.

Grammars are compiled into the binary at build time (see `.cargo/config.toml` /
`TSLP_LANGUAGES`), so parsing works fully offline — no runtime downloads.

## Documentation

- [Getting Started](docs/getting-started.adoc) — 10-minute intro
- [Philosophy](docs/philosophy.adoc) — Why Aden exists and what it solves
- [Architecture](docs/architecture.adoc) — Technical deep-dive
- [AI Integration](docs/ai-integration.adoc) — Using Aden with AI agents
- [User Guide](docs/user-guide.adoc) — Daily workflow reference

## Acknowledgments

Aden's entire premise — that documentation can be a **plain-text, regular,
referential, scriptable language** rather than prose locked in a binary format —
rests on the people who invented and stewarded AsciiDoc:

- **Stuart Rackham**, who created AsciiDoc in 2002. The original insight — that a
  document could be readable text with a regular grammar, cross-references
  (`<<anchor>>`), includes, attributes, and conditionals — is exactly what lets
  Aden treat docs as a queryable graph instead of opaque files. That idea is
  load-bearing for this whole project.
- **Dan Allen** and the **Asciidoctor** project (with the **AsciiDoc Working
  Group** at the Eclipse Foundation), who carried AsciiDoc forward into a
  maintained processor and a real language specification.

Aden also stands on the shoulders of:

- **Max Brunsfeld** and the **tree-sitter** project — incremental parsing for
  300+ languages, the engine behind Aden's symbol/call extraction.
- **Andrew Gallant (BurntSushi)** — the `regex`, `ignore`, and `grep-*` crates
  (the reusable core of ripgrep) that power Aden's structure-aware `grep`.
- **fjall** (LSM-tree storage), **petgraph** (graph data structures), and
  **rmcp** (the Model Context Protocol SDK).

Full third-party license attribution is generated by `aden licenses` (see
`NOTICE.md`).

## The Name

**A Dense Referential Context Compiler** — Every token is load-bearing. Every edge is typed. Every anchor resolves.

---

*Aden is designed for the future of software development: hybrid teams of humans and AI agents working together.*