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

## Where Aden Fits

Aden **complements** your existing tools — it maps the structure of a codebase,
it does not find bugs or render HTML.

| Instead of / alongside | What Aden adds |
|------------------------|----------------|
| Static analysis tools (clippy, Semgrep) | Aden finds *semantic relationships and blast radius*, not bugs — keep clippy/Semgrep for correctness; use Aden to navigate the graph |
| Documentation generators (Rustdoc, Javadoc) | Aden produces *machine-navigable* context for LLMs, not HTML |
| grep + manual file hunting | Aden lets you query by intent and relationship, with every hit tagged by its enclosing symbol |
| Scrolling through READMEs | Aden assembles exactly the connected context you need, within a token budget |

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

# Find a symbol's definition AND its real aden:// anchor
aden locate --symbol login

# One-shot symbol comprehension (replaces locate + backlinks + impact + asm)
aden understand Database::connect .

# Blast radius before a refactor — who depends on this symbol?
# query/asm take a full aden:// anchor (from locate/grep/list), not a bare name:
aden query --backlinks "aden://module/<crate>/<module-doc>.adoc#code_block_3"

# Assemble a module (or symbol) overview within a token budget
aden asm --from "aden://module/<crate>/<module-doc>.adoc#code_block_3" --depth 1

# Before every commit (fast, aden-only gates)
aden ready .

# Visualise the graph — text (mermaid/dot/json) or an interactive offline browser view
aden viz --mode communities --format mermaid     # text diagram for a PR/README
aden view                                         # whole graph in the browser, with
                                                  # git-history replay (on by default)

# Expose Aden's commands to your AI client as MCP tools
aden mcp install --platform claude   # see docs/mcp-intro.md
```

The graph is **fresh by construction**: read commands (`ask`/`asm`/`query`/
`locate`/`grep`) detect changed source and re-index it automatically, so you
rarely need to run `gen` by hand.

## Hybrid (dense) search — optional

By default `search`/`ask` use BM25 (lexical) ranking over the graph. The optional
`dense` feature adds **local semantic embeddings** fused with BM25 via Reciprocal
Rank Fusion, which improves natural-language queries (it finds code by *meaning*,
not just shared terms). It stays fully offline and deterministic — a pure-Rust
ONNX model (`tract` + BAAI/bge-small-en-v1.5, MIT), no network at query time.

```bash
# One-time: fetch the embedding model into ~/.cache/aden-models (the only step
# that touches the network; aden itself never does). ~127 MB.
scripts/fetch-bge-model.sh

# Build aden with hybrid search enabled
cargo build -p aden-cli --features dense
```

With the feature off (the default), nothing changes and no extra dependencies are
built. Air-gapped? Place `model.onnx` + `tokenizer.json` from BAAI/bge-small-en-v1.5
into the cache dir by hand instead of running the script.

### Dual-substrate levers (opt-in)

Two further retrieval levers route by what the text is: a corpus-derived **PPMI rerank**
for code (MRR 0.216 → 0.289) and grounded **OEWN synonym expansion** for prose (R@1
1/42 → 41/42). `ADEN_LEXICON_AUTO=1` auto-gates both on the detected query shape and
corpus substrate; `ADEN_LEXICON_EXPAND` / `ADEN_PPMI_RERANK` force one on. All off by
default. See [`docs/retrieval-levers.adoc`](docs/retrieval-levers.adoc).

## Core Commands

| Command | Purpose |
|---------|---------|
| `aden gen` | Compile source into the knowledge graph (symbols, call edges, docs) |
| `aden ask` | Natural-language question → dense, graph-traversed context |
| `aden understand` | One-shot symbol comprehension: definition + callers + impact + context |
| `aden grep` | Structure-aware search — every hit tagged with its enclosing symbol |
| `aden asm` | Assemble context from an anchor within a token budget |
| `aden query` | Graph queries: `--from`, `--backlinks` (callers), `--impact` |
| `aden locate` | Find symbol definitions with exact line numbers |
| `aden check` | Validate referential integrity |
| `aden lint` | Fast, language-agnostic heuristic checks; `--dead-code` for graph-based detection |
| `aden heal` | Detect drift between code and contracts |
| `aden ready` | Fast pre-commit gate: gen → lint → check → heal drift → audit |
| `aden sync` | Reconcile store after merges or file deletions (gen + check + heal with gc) |
| `aden ci-check` | Full CI gate suite including external tools; use before push |
| `aden view` | Interactive graph viewer in the browser — offline, with git-history replay |
| `aden timeline` | Time-travel file viewer: bake every git version of a file into a self-contained offline HTML page with client-side diff |

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
  TypeScript/JavaScript, Java, C#, C, Ruby, PHP, Kotlin.
- **Generic extraction** (symbols + structure, no call edges): ~113 further languages
  wired via `ext_to_language_pack_id` in `router.rs` (.ps1/.psm1/.psd1 PowerShell
  included); 305+ grammars available in the bundled pack — add entries to
  `ext_to_language_pack_id` in `crates/aden-parse/src/router.rs` to expose more.

Grammars are compiled into the binary at build time (see `.cargo/config.toml` /
`TSLP_LANGUAGES`), so parsing works fully offline — no runtime downloads.

## Performance

Early self-run measurements on Aden's own repository (244 files) and external corpora:

- **Edge-extraction F1: 0.915** [measured] — micro-precision 0.946, micro-recall 0.886 on a 79-edge polyglot ground-truth fixture.
- **~10× token savings overall** vs a grep-and-read agent [measured, chars/4 proxy]; up to >100× for symbol and structure lookups; ~4–5× for open-ended conceptual questions.
- **Hybrid retrieval beats BM25 on every corpus tested** — R@1 gains of 0.06–0.14 across Go, Rust, C#, Python, TypeScript, and two larger corpora (Linux kernel subset, create-t3-app).
- Energy savings vs LLM inference are estimated (not instrumented): see full methodology and caveats in [docs/benchmarks.adoc](docs/benchmarks.adoc).

See [docs/benchmarks.adoc](docs/benchmarks.adoc) for full numbers, methodology, and all caveats.

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

Aden also stands on the shoulders of the wider open-source Rust ecosystem and
the **many authors, maintainers, and contributors** behind the projects it
builds on. Several are load-bearing:

*Parsing & search*

- **Max Brunsfeld**, the **tree-sitter** project, and the numerous per-language
  grammar authors (bundled via `tree-sitter-language-pack`) whose work makes
  symbol and call extraction possible across 300+ languages.
- **Andrew Gallant (BurntSushi)** and contributors — the `regex` (with
  `aho-corasick`/`memchr`) and `walkdir` crates behind Aden's structure-aware
  `grep`, lint, and file discovery.

*Storage, graph & data*

- the **fjall** project (LSM-tree storage), **petgraph** (graph data
  structures), and the **serde** community (**David Tolnay** and contributors) —
  with `postcard`, `serde_json`, `serde_yaml`, `toml`, `blake3`, `fnv`, and
  `uuid`.

*CLI, async & protocol*

- the **clap**, **rayon**, **Tokio**, and **tower-lsp** teams; **ctrlc**,
  **notify**, **ureq**, **dirs**, **chrono**; **anyhow**/**thiserror**
  (David Tolnay and contributors); and **rmcp** — the Model Context Protocol
  SDK from **Anthropic** and the MCP community.

These names are illustrative, not exhaustive, and many of these projects have
multiple owners. The **complete and authoritative attribution for every one of
Aden's 350+ direct and transitive dependencies — each with its license — lives
in `NOTICE.md`** (regenerate with `aden licenses`). If you maintain a project
Aden depends on and feel under-credited, that is an oversight we want to
correct — please open an issue and we will fix it.

### Third-party reference material

The `research/` tree contains documentation that Aden *parses and queries*, not
code it compiles or links — e.g. a secure-coding knowledge base summarizing
OWASP and MITRE CWE guidance. This material is under its own third-party
licenses: OWASP material under CC BY-SA 4.0 and CC BY 3.0; MITRE CWE content
under the MITRE CWE Terms of Use (a separate, non-Creative-Commons instrument).
Content is kept segregated from Aden's AGPL-3.0 source and never embedded in
any binary. Full citations, required notices, and trademark/non-endorsement
statements are in `research/secure-coding/SOURCES.md` and `research/README.md`.
"OWASP" is a trademark of the OWASP Foundation; "CWE" is a trademark of MITRE
Corporation. Their use here is nominative and implies no affiliation or
endorsement by the OWASP Foundation or MITRE Corporation.

## The Name

**A Dense Referential Context Compiler** — Every token is load-bearing. Every edge is typed. Every anchor resolves.

---

*Aden is designed for the future of software development: hybrid teams of humans and AI agents working together.*