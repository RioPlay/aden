<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Aden 0.4.4 release readiness

## PR review status

The working-tree candidate is based on `2172e0e35668f69af50cc43d37acb88ee9add297`.
The locked dependency checks now pass locally. Review and the hosted gates
below still need to run on a committed candidate.

- **Product scope.** Bounded source reads and graph analysis, incremental and
  background generation, context retrieval and MCP recovery, CLI and MCP
  pairing, portable bundle upgrades, and parser, LSP, editor and viewer fixes.
  See the [0.4.4 changelog](../CHANGELOG.md#044) for the user-facing changes.
- **Latest paired evidence.** The backport passed 458 targeted CLI/MCP tests
  and four SDK time-formatting tests on Rust 1.100 Windows GNU LLVM. The
  matching binaries installed from one verified archive and returned exact
  source over both MCP entry points after the build target was removed.
  The preceding build passed 499 tests and the installed Rust and Go lookup
  smoke. WSL AlmaLinux 10 exercised shell, installer, and package checks.
  The native Linux candidate remains unverified.
- **Security gate.** The original lock failed `RUSTSEC-2026-0543` because RMCP
  pulled in unmaintained Chrono. The checkout now backports the change from
  [RMCP PR #1474](https://github.com/modelcontextprotocol/rust-sdk/pull/1474).
  The resolved locked graph has no Chrono. The all-feature audit passes
  advisories, bans, licenses and sources. The vendored SDK base was checked
  separately against all four RMCP records in RustSec. See
  [vendor provenance](../vendor/README.adoc).
- **Hosted gates.** Run Lean CI and Quality Gate on the PR and merged commit.
  Run the release dry run against the reviewed source and validate the installed
  archives on all five targets. The Windows GNU LLVM test does not cover the
  shipped MSVC binary or exact Rust 1.90 runtime. Real editor and model-client
  coverage also remains separate from the stdio acceptance.
- **Scope of publication.** This release covers GitHub binary bundles. The
  vendored Cargo overrides, including the RMCP backport, do not carry over to
  registry consumers. Individual crate publication needs separate resolution
  and verification.
- **Commit scope.** Include source, tests, reviewed vendor inputs, and release
  documentation. Exclude local `.agent/session.md` and `.agent/session.adoc`
  working changes, `.tmp/` evidence, machine policy files, stores and binaries.
  Stage explicitly and inspect the index before submitting a PR.

## Earlier validation records

The following records describe earlier revisions and individual validation
passes. Use them to trace changes and measurements to their conditions.

Local baseline verification before the version bump and final rendering changes:
979 Rust tests passed, 15 intentionally ignored; 79 Python and 19 viewer tests
passed. The separate LSP target passed with no tests. Clippy, formatting,
AsciiDoc, NOTICE, roadmap, and RustSec checks passed. `ready` passed with advisory
findings; it is not a clean security audit. These baseline results must not be
substituted for the final candidate checks recorded in the PR and GitHub CI.

New benchmark/evidence source headers were corrected after independent review.
Local continuity records, exploratory review notes, raw `.tmp` reports, generated
stores and build artifacts are not part of the release source change.

Viewer methods and limitations are in [viewer-performance.adoc](../docs/viewer-performance.adoc).
Native desktop fixture checks covered Simple search/filter/Back and 2D global
search/no-results/Enter selection. They do not establish touch behavior, frame
rate, universal scalability, or performance on other operating systems.

## Local reliability refresh - 2026-11-15

This is an uncommitted working-tree refresh based on `2172e0e35668f69af50cc43d37acb88ee9add297`,
not a published or approved release. Earlier working-tree changes are preserved.

- Generic AST traversal and declarator-name extraction are iterative. Regression
  coverage preserves order/spans and exercises a 10,000-operator expression.
- LSP acquires at most one graph per request and releases document locks before IO.
  Graphs are rebuilt between requests so external edits and root changes stay visible.
- Editor startup preserves PATH lookup, waits for workspace trust, and reports
  actionable failures. Tests are included in the quality workflow.
- Hybrid retrieval fuses complete lexical/dense rankings before materializing
  snippets. Public all-result APIs, fuzzy duplicates and snippet precedence remain.
- `aden store clean [DIR] --scope cache|graph|all` previews by default. `--apply`
  removes allowlisted generated data under writer/generation exclusion, preserving
  intent, models, source and persistent locks. Shared overrides and redirects fail
  closed; portable relocation is supported.
- Standalone MCP now configures its surface before constructing Tokio worker
  threads. Current Cargo test artifact layouts are supported alongside older ones.

Native Rust 1.100.0 GNU LLVM workspace library/binary tests: **849 passed, two
intentionally ignored**, including LSP. Rust 1.90 linked cleanup acceptance:
**four passed**, covering preview, actual deletion, active writer refusal,
portable relocation, malformed metadata, Windows junction refusal and automatic
rebuild. Python harnesses: **99 passed**. Viewer tests: **23 passed**. Editor
compilation and five mocked startup/resolution tests passed on Node 22.23.5.
All five standalone MCP wire tests passed, including argv surface precedence and
documented recovery from cold background generation. Strict Rust 1.90 Clippy
passes every workspace target and feature. Two existing compiler warnings in the
vendored Liquid Core dependency remain outside workspace lint enforcement.
These extension tests require Node 22.15 or newer as a development tool; they do
not raise the VS Code 1.85 runtime support minimum.

RustSec scan: zero known vulnerabilities and warnings across 417 locked packages.
The locked npm audit also reports zero findings. Independent source review found
and resolved the MCP startup-order issue and portable-cleanup regression. The
heuristic source scanner still flags reviewed unsafe/FFI and test patterns; its
zero high/critical result is not a claim that every source path is vulnerability-free.

Matched retrieval timing and final source identities are recorded in
`docs/reliability-validation.json`. The benchmark compares complete equal outputs
against a frozen eager implementation using a deterministic four-dimensional
provider and a 2,000-document synthetic corpus. Debug-profile timings do not
establish production latency, real-model inference gains, or application-wide gains.
The latest native run shows only a small timing difference, which is insufficient
to claim a reliable production speedup.

Before publication, run the exact committed candidate through the platform,
shipped-binary and quality gates. Real VS Code 1.85/current host acceptance remains
separate from compilation and mocked tests. The Liquid Core workspace patch does
not propagate to crates.io consumers; resolve that packaging gate or obtain an
upstream fix before crate publication. No tag, push, package or release is made
by this refresh.

The optimized 0.4.4 local candidate is now installed for CLI use, together with
its matching standalone MCP binary. Installed source lookup, Rust/Python grammar
loading, MCP surface selection, path confinement, cleanup and regeneration smoke
checks passed again after the Cargo build tree was removed. One previous 0.4.1
installation pair is retained as a rollback fallback. This local activation does
not publish a release or establish that already-running client processes reloaded.

The user's T3 restart activated the canonical 0.4.4 installation in its OpenCode
provider. An actual model request then exposed a gateway incompatibility with
the root-level `anyOf` in Aden's `locate` schema. The schema now uses plain object
properties and documents the required selector; runtime argument checks still
reject missing or invalid selectors. All 120 MCP library tests and strict Rust
1.90 MCP Clippy checks passed after this fix. The enhanced installed-binary check
rejects the previous schema, establishing the negative case. The earlier 849-test
workspace result predates this schema-only fix; it is not a rerun of the final tree.
The rebuilt optimized pair passed schema, grammar, confinement and cleanup smoke
checks both before and after Cargo cleanup. A fresh T3-managed OpenCode request
successfully called `aden_read` and `aden_locate`, resolving `cmd_store_clean` with
current graph freshness. The new installation is active; publication remains gated.

### Rust inline symbols and assertion relationships

The local 0.4.4 installation now indexes inline Rust module functions, types and
methods with scoped identities, preserves their source ranges, and links actual
inline test functions to the code they exercise. Common assertion expressions
retain call relationships and closure shadowing through bounded parsing. Graph
logic version 18 invalidates earlier caches automatically.

Final affected checks: 27 Rust parser tests executed on Rust 1.90, strict Rust
1.90 all-target Clippy for CLI/parser, 25 unchanged-linker tests, and two native
release CLI integration tests passed. Assertion-based fixtures failed against
the earlier installation and passed after repair, including warm generation.
Installed Rust/Python grammar, MCP schema, confinement and cleanup smoke checks
passed after Cargo cleanup.

The original self-graph failure is now covered by real acceptance: natural-name
lookup finds the inline MCP schema test at `lib.rs:5461`; backlinks to
`validate_args` contain that test with both `Calls` and `Tests`. Source hashes
are verified. T3-managed OpenCode repeated this through actual MCP locate, query
and read calls with current graph freshness. Detailed source identities and
results are in `docs/reliability-validation.json`.

This is conservative static analysis. Arbitrary macros and `matches!` remain
opaque; supported assertion parsing is limited to eight passes and 1 MiB per
body. Aliased/cross-file bindings and custom crate-root layouts retain existing
limitations. Earlier full-workspace results predate this repair and do not
substitute for exact committed-candidate release gates.

### C declarator and indirect-call correctness

The local 0.4.4 candidate with graph logic version 19 distinguishes function
declarations from pointer variables, initialized objects and callback arrays.
Functions returning pointers, annotated prototypes and mixed declarations remain
discoverable. Known global object invocations retain an `Indirect callee` table
instead of feeding the direct-call compatibility reader.

A native release CLI regression checks a global callback with an unrelated
same-named static function in a second C file. It preserves the real direct edge,
rejects the false callback edge, and repeats the check after warm generation.
The pinned Linux v6.12 source slice also verifies automatic migration from a v18
graph: the false `late_time_init` function and its boot edge disappear without
source edits, while real boot/read relationships and verified source remain.

No general macro expansion or parameter/local callback resolution is claimed.
The unchanged standalone MCP bridge is reused from its verified installation.
Evidence and exact source identities are recorded in
`docs/reliability-validation.json`; publication remains gated separately.

## Validation ownership and test relevance

The required shipped-binary status owns one complete default workspace test run
(including LSP), the release build, and release-profile product/MCP smoke tests.
The Quality Gate owns MSRV/all-target checks, opt-in features, lint, pure
viewer/benchmark harnesses, dependency/license checks, and documentation validation.
The native OS matrix owns filesystem, CLI/MCP, concurrency, and installer contracts.
Repeated full debug suites and repeated discovery-selected tests were removed.

Local fixes use the smallest checks that can establish their behavior and failure
mode. A final full suite belongs to the exact PR candidate; documentation-only or
workflow-routing edits do not automatically require another full local rebuild.
Review assertions when goals change: retain current observable contracts and
demonstrated failure scenarios, update intentionally changed compatibility
requirements, and replace incidental status wording or internal helper counts.
Historical measurement protocols remain opt-in evidence, rather than mandatory
execution on every change. Check success does not establish browser performance.

## Historical 0.4.1 readiness — 2026-09-19

Local release preparation is complete for workspace version 0.4.1. At the time
of this review, no tag or GitHub release existed. The exact committed candidate
must pass GitHub's platform and release workflows before publication.

## Prepared

- Preserve the five tracked legacy `.aden` files. Ignore new generated state,
  nested Cargo outputs, Python caches, and local release outputs. Policy migration
  is separate work requiring compatibility tests.
- Preserve vendored JavaScript bytes on Windows checkout and check their hashes
  in CI. A fresh checkout with `core.autocrlf=true` passed checksum verification.
- Audit all workspace dependencies and optional features. Update rustls to
  0.23.45 for RUSTSEC-2026-0285; the full locked cargo-deny audit passes.
- Preserve maintained legal notices and regenerate the 404-package dependency
  table from locked metadata. CI checks notice drift and Python license headers.
- Bundle the root LICENSE and NOTICE, verify exact copies, and include source
  tag/archive URLs in the manifest. Those URLs become valid when the matching
  release tag is published.
- Publish portable pilot evidence paths and explicitly labelled hash records.
  Original frozen evidence is retained locally; integrity values are unchanged.

## Validation

- Workspace Rust tests before the final provenance fix: 884 passed, 15 ignored,
  zero failures. Explicit aden-lsp testing passed (no tests defined).
- Formatting, clippy with warnings denied, all-targets checking, and all-features
  checking passed before the final provenance fix.
- Python: 64 tests passed. Viewer: 11 tests passed. Roadmap: 15 packets validated.
- License metadata, source headers, dependency audit, NOTICE drift, shell syntax,
  identity hygiene, and release-bundle fixture checks passed.
- Release-profile CLI and MCP builds passed. The real Windows archive passed
  manifest, checksum, and license-copy checks. Installation into a path with
  spaces passed; installed binaries successfully located Rust and Go symbols.
- All three targeted provenance tests passed, including rejection of credential
  fields, non-hex values, mixed fields, and non-JSON source.
- Final `aden ci-check .` passed: all eight blocking gates passed, including
  tests and secret scanning; clippy, policy, and freshness checks passed.
  Zero blocking findings; the sole advisory is the unavailable optional
  cargo-audit executable, covered separately by the successful cargo-deny audit.

Local evidence is retained under ignored `target/release-readiness/`, including
`final-ci-check.log`, `integrity-tests.log`, `final-python.log`,
`final-release-build.log`, `windows-smoke.log`, and the tested Windows archive
under `dist/`. These are local validation artifacts, not published releases.

## Limits

Local validation uses Windows and Rust 1.96; it does not replace the CI toolchain
and cross-platform matrix. Existing ignored tests were not silently counted as
passes. The source audit reports five medium heuristic findings, no high or
critical findings. The optional cargo-audit executable is unavailable locally;
cargo-deny separately completed the full RustSec dependency audit.

The Luna pilot establishes useful bounded-task evidence, not general reliability:
precise explanations and broad security conclusions still need independent review.


## Functionality improvements - 2026-11-15

Local 0.4.4 graph logic v20 adds explicit semantic analysis limits, persisted
extractor scope, structured include deduplication, scoped C callback handling,
and opt-in project macro API mappings. Mapped logical names retain original
source spans and provenance, without ABI, preprocessing or body-call claims,
and cannot become automatic call-link targets. Configuration changes refresh
unchanged C sources and prune disabled mappings. See
[analysis-limits.adoc](../docs/analysis-limits.adoc).

The sampled Cargo guard rejects output redirects, stops/reaps builds on monitor
failures, checks final growth even after fast success, and preserves a free-space
reserve. Project instructions now use it with the coordinator's remaining budget;
it is not an OS quota. See [build-storage.adoc](../docs/build-storage.adoc).

Final native Rust 1.100 release checks passed: 491 affected crate unit tests,
three CLI graph regressions, 17 Python guard tests, formatting and the AsciiDoc
Forbid gate. Rust 1.90 strict all-target Clippy passed for the three changed
crates. The Rust 1.90 GNU runtime test process crashed before tests on this
machine; runtime compatibility must still pass release CI on supported targets.
The seven-file pinned Linux v6.12 slice and Aden self-graph checks passed, as did
installed MCP wire/source-confinement/cleanup checks. These are bounded checks,
not a full-kernel or full-project security certification. Independent review
findings were fixed. Exact source identities and limitations are in
`docs/reliability-validation.json` under `functionality_improvements`.

The canonical local installation is 0.4.4 with graph logic v20. Build targets and
temporary binary copies were removed; the current project graph remains for
normal navigation. Publication and exact-commit GitHub release gates remain with
the maintainer. No commit, push, tag, or public release was made.

## Bounded generation and parser metadata - 2026-11-17

The latest local 0.4.4 build uses generation logic v21, CLI SHA256
`5bb3a4b1e598b03cbf43862c50b0208ff26b72ac243db4c8dfff62bcb6bf5a43`.
It reuses metadata from each supplied parser input, persists sorted parse
batches, and streams stored documents into the unchanged v1 snapshot fields.
All edge families remain. Bare relative filenames now use the same project
store as directory generation, including interrupted-owner reconciliation.

On the fixed 8,774,891-byte cold-generation fixture, three runs per CLI build
showed median peak working set falling from 135,446,528 to 94,752,768 bytes
and process CPU time from 5.5 to 2.90625 seconds. Median wall time was essentially
unchanged, with a candidate outlier; no reliable latency gain is established.
The separate same-executable policy diagnostic preserved 8,258 documents,
24,706 edges and normalized digests. See
[bounded-generation-performance.adoc](../docs/bounded-generation-performance.adoc)
for scopes, ranges, method and limitations.

Final affected native release checks passed 525 unit and six integration tests,
with three diagnostics intentionally ignored. Native debug passed six bounded
generation, six lifecycle and seven merge tests. The 27 Python storage-guard
tests passed. Strict Rust 1.90 workspace/all-target/all-feature Clippy passed on
the final production code; a later test-only source-count expectation was
corrected to include Cargo.toml. Installed CLI/MCP smoke and the pinned seven-file
Linux slice passed. These do not replace platform or release CI.

An earlier package-selection switch duplicated grammar outputs and crossed the
task storage cap before the sampled guard reacted. The target was removed and
the free-space reserve remained intact. Build identity v2 now rejects incompatible,
unmarked or corrupt targets and unsupported build switches before Cargo starts.
Configurations were subsequently validated in one target at a time. Monitoring
remains sampled, rather than an OS-enforced quota.

One old executable remains necessary for already-running MCP processes and can
be removed after those processes exit. The existing MCP bridge is unchanged.
Exact identities, resource reports and final cleanup state are recorded under
`bounded_generation_2026_11_17` in `docs/reliability-validation.json`.
No commit, push, tag or release was published. The maintainer retains publication
and exact-commit platform, shipped-binary and quality release gates.
