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

# Initialize your project
cd your-project
aden init

# Generate contracts from source
aden gen --auto .

# Ask questions about your codebase
aden ask "How does authentication work?"

# Find the blast radius before refactoring
aden asm --from mod-auth --depth 2
```

## Core Commands

| Command | Purpose |
|---------|---------|
| `aden gen` | Generate `.adoc` contracts from source code |
| `aden ask` | Ask natural language questions, get graph-traversed context |
| `aden asm` | Assemble context within a token budget |
| `aden check` | Validate referential integrity |
| `aden heal` | Detect and fix drift between code and contracts |
| `aden locate` | Find symbol definitions with exact line numbers |

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

## The Name

**A Dense Referential Context Compiler** — Every token is load-bearing. Every edge is typed. Every anchor resolves.

---

*Aden is designed for the future of software development: hybrid teams of humans and AI agents working together.*