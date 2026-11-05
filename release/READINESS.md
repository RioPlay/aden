<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Release readiness — 0.4.4 candidate

The 0.4.4 candidate combines deterministic source reads and content-coherent
generation, bounded traversal, measured setup reductions, viewer performance
work, and dependency security fixes already on main. Publication is conditional
on resolving material review findings and successful GitHub platform and quality
gates for the exact candidate commit. No 0.4.4 tag or release has been published.

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
