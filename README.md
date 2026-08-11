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

For a frictionless install, download the prebuilt archive matching your platform
from [GitHub Releases](https://github.com/RioPlay/aden/releases), verify its
checksum, extract it, and run the bundled `install.sh` or `install.ps1`. See
[Releasing and prebuilt binaries](docs/releasing.adoc). No Rust toolchain is
required.

The repository-root installer below is the source-build path for contributors and
requires Rust 1.90 or newer.

```bash
# Contributor install (builds release, copies to ~/.local/bin, adds to PATH)
./install.sh

cd your-project

# Bounded map: exact symbols and affected line ranges, no source bodies
aden tree --human --symbols .   # scope DIR if a very large repo is truncated

# Structure-aware evidence, canonical location, then bounded comprehension
aden grep "hash_password"
aden locate --symbol login
aden understand --human Database::connect .

# One bounded conceptual question (broad audits fail with needs_narrowing)
aden ask --human "Where is session authentication enforced?"

# Blast radius before a refactor — unique natural names resolve directly
aden query --impact Database::connect

# Expose the same focused navigation tools to an AI client
aden mcp install --platform claude
```

Windows contributors can run the equivalent source installer from PowerShell:

```powershell
.\install.ps1
# Existing binaries require -Force; remove only the binaries with -Uninstall.
```

Both source installers stage and smoke-check the `aden`/`aden-mcp` pair before
replacement and restore the previous pair if installation fails. Set
`ADEN_INSTALL_DIR` (or pass `-InstallDir` on PowerShell) for a custom location.

The graph is **fresh by construction**: reads detect changed source and update
the per-user store automatically. Normal use creates no `.aden` or other
Aden-specific project files; `init`, explicit `gen`, templates, and governance
are optional.

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

Two retrieval levers route by what the text is: a corpus-derived **PPMI rerank** for
code (MRR 0.216 → 0.289) and grounded **OEWN synonym expansion** for prose (R@1
1/42 → 41/42; end-to-end 0/15 → 15/15). Auto-gating is **off by default** (net-neutral to
negative on natural multi-word queries over external repos); opt in with `ADEN_LEXICON_ON`
(routed by query shape + corpus substrate), or force a single lever with `ADEN_LEXICON_EXPAND` /
`ADEN_PPMI_RERANK`. Once opted in, `ADEN_LEXICON_OFF` force-disables. Grounded and corpus-gated,
so it no-ops where it would not help. See [`docs/retrieval-levers.adoc`](docs/retrieval-levers.adoc).

## Core Commands

CLI reads are JSON-first because Aden is primarily consumed by agents and LLM
tooling. Use `--human` when you want clean terminal prose or tables; the legacy
`--json` flag remains accepted for compatibility.

```bash
aden ask "How does authentication work?"          # versioned JSON envelope
aden ask --human "How does authentication work?"  # clean context for a person
```

| Command | Purpose |
|---------|---------|
| `aden tree --symbols` | Bounded symbol and line-range map; scope to a subtree when truncated |
| `aden grep` | Structure-aware evidence; each hit includes its enclosing symbol |
| `aden locate` | Resolve names to canonical anchors and source ranges |
| `aden understand` | Definition, callers, downstream impact, and bounded context |
| `aden ask` | One bounded conceptual question; unsafe broad shapes fail small |
| `aden query` | Explicit backlinks, impact, and graph traversal |
| `aden asm` | Bounded context around a unique symbol name or canonical anchor |
| `aden impact-diff` | Map a change to affected symbols and downstream risk |

`aden --help` shows this focused product surface. Existing setup, governance,
visualization, and generic wrapper commands remain available through the
categorized `aden commands` compatibility catalog. Prefer native project test,
lint, audit, and CI tools.

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
  included). The default binary statically bundles the deep-language set plus
  common config/web/data grammars; builds can opt into all 305 grammars with
  `TSLP_LANGUAGES=all`. Add entries to `ext_to_language_pack_id` in
  `crates/aden-parse/src/router.rs` to expose more.

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
