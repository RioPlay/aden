<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Release readiness — 2026-09-19

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
