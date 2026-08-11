# Contributing to Aden

## Developer Certificate of Origin (DCO)

By contributing to this project, you agree that your contributions are licensed to the Maintainer under the terms of the [Contributor License Agreement (CLA)](CLA.md), and that each contribution, as incorporated into a published open release of the Project, is made available to the public under the GNU Affero General Public License v3.0 (AGPL-3.0-or-later) or another FSF/OSI-recognized open source license, as provided in CLA Section 5. To the extent of any conflict between the DCO sign-off and the CLA, the CLA prevails (CLA Section 11).

All commits must include a `Signed-off-by` line in the commit message:

```
Signed-off-by: Your Legal Name <your@email.com>
```

This line attests that you have the right to submit the code under the AGPL license and that you agree to the Developer Certificate of Origin: https://developercertificate.org/

## Copyright and Contributor License Agreement

The Project's principal copyright holder and Maintainer is RioPlay <rioplay@rioplay.dev>; contributors retain copyright in their own contributions, licensed to the Maintainer under the CLA below.

All contributions are made under the terms of the [Contributor License Agreement (CLA)](CLA.md). By contributing, you agree to the CLA, which grants the Maintainer a broad license to your contributions — including the right to relicense them under commercial terms — while you retain ownership of your work. In return, the CLA commits the Maintainer to keeping every contribution, as incorporated into a published open release of the Project, publicly available under the AGPL or another FSF/OSI-recognized open source license (CLA Section 5).

The BDFL, RioPlay, retains the right to manage dual-licensing arrangements for the Project.

## Process

1. Read `.agent/onboarding.adoc` before starting work.
2. Read the [Contributor License Agreement (CLA)](CLA.md).
3. Run `aden check .` before submitting.
4. Run `cargo clippy --workspace` to catch style issues.
5. Ensure all `<<refs>>` resolve to existing `[[anchors]]`.
6. Include a `Signed-off-by` line on every commit.
7. Open a pull request with a clear description of changes, including the exact statement: "I have read and agree to the Contributor License Agreement (CLA) for Aden."

## Ownership, Review, and Protected Changes

The current maintainer and continuity policy are recorded in
[MAINTAINERS.md](MAINTAINERS.md). Do not assign maintainers, reviewers, or
security responders in an issue or pull request without their explicit consent.

For ordinary changes, make the smallest independently reviewable pull request
you can, explain the user-visible outcome, and include the validation you ran.
Avoid mixing generated artifacts, broad refactors, and behavior changes unless
the dependency is necessary and documented.

The following protected changes require documented maintainer review and the
applicable validation before merge:

- public CLI, MCP, graph, store, or result-schema compatibility;
- graph/store migrations or rollback behavior;
- secret handling, path confinement, command execution, authentication, or
  vulnerability remediation; and
- governance, licensing, contributor, or release-policy documents.

The BDFL may approve and merge protected changes after documenting the affected
contract, validation, and recovery or rollback path in the pull request or
issue. The BDFL is the sole approval authority. Independent review is optional
and advisory; it never blocks a sole-maintainer release. An AI agent or tool
acting on the BDFL's direction is not an independent reviewer.

When a change affects a public contract, migration, or security boundary, state
in the pull request:

- the affected contract and compatibility impact;
- the validation and rollback or recovery path;
- any remaining limitations or follow-up work; and
- the issue or ADR that records the durable decision, when applicable.

## Before Every Commit

Run the full CI check locally:

```bash
aden ci-check .
```

This runs blocking gates — `aden check`, project tests, `aden lint`, a secret
scan, an accreditation/attribution check, an OWASP-style `aden audit`, and a
merge-conflict-marker scan — plus warning-only gates (constitutional firewall,
insecure-protocol scan, `cargo clippy`, `cargo audit`, contract freshness). It
does not run `aden heal`.

### Sample Pre-Commit Hooks (Recommended)

Run `aden init --templates` to explicitly scaffold sample git hooks under `.aden/hooks/`. These are
templates: every command in them is commented out, so you must uncomment and
adapt the lines that fit your project before they do anything. Install them by
copying into `.git/hooks/` and making them executable:

```bash
# Install pre-commit hook
cp .aden/hooks/pre-commit .git/hooks/
chmod +x .git/hooks/pre-commit

# Install pre-push hook (optional, more thorough)
cp .aden/hooks/pre-push .git/hooks/
chmod +x .git/hooks/pre-push
```

The sample pre-commit hook suggests running your test command and `aden check .`;
the sample pre-push hook suggests your build/lint commands and `aden gen src/ --auto`.
Edit them to your needs (for example, swap in `aden ci-check .`) so `git commit`
and `git push` run the validation you want.

### Manual Workflow

If you prefer manual control:

```bash
# 1. Generate fresh contracts
aden gen src/ --auto

# 2. Validate graph
aden check .

# 3. Detect drift (optional: --propose to preview first)
aden heal . --fix

# 4. Run tests
cargo test --workspace

# 5. Run linter
cargo clippy --workspace
```

## Language Grammars (Offline Parsing)

Aden parses source via `tree-sitter-language-pack`, which by default *downloads*
grammars at runtime. That would make `aden gen` fail to parse first-class
languages on any offline / air-gapped / locked-down CI box. To avoid that,
`.cargo/config.toml` pins both `TSLP_LANGUAGES` and `TSLP_LINK_MODE=static`,
which compiles those grammars *into the binary* at build time (sources are
fetched once during `cargo build`, which has network). The link mode is required:
the language pack otherwise emits platform-specific shared libraries under
Cargo's temporary build directory:

```toml
# .cargo/config.toml
[env]
TSLP_LANGUAGES = "rust,python,go,typescript,tsx,javascript,c,...,swift"
TSLP_LINK_MODE = "static"
```

- Editing that list changes which languages parse offline. The deep-resolver
  languages (rust, python, go, ts/js, c, java, kotlin, csharp, ruby, php) **must**
  be listed — their resolvers load the grammar from the pack too.
- To bundle every supported grammar (larger binary, much longer build), build
  with `TSLP_LANGUAGES=all cargo build` — an explicit env var overrides the file.
- A normal `cargo build` already picks up `.cargo/config.toml`; no extra step.

## Developer Ritual

Aden templates are embedded in the binary via `include_str!`. When templates change, the binary must be rebuilt and the local `.agent/` workspace re-initialized.

### The Stable Binary Ritual (Do This Every Time Templates Change)

```
# 1. Build a clean release binary
cargo build --workspace --release

# 2. Save it as the stable reference
cp target/release/aden ~/.cargo/bin/aden-stable

# 3. Re-initialize the workspace with the new binary
aden-stable init

# 4. Run all gates
aden-stable ci-check .

# 5. Only then commit
```

### Why This Matters

If you skip step 3, the `.agent/` workspace on disk will contain stale templates that don't match the binary's embedded versions. This causes silent drift that breaks downstream projects and wastes debugging time.
