# Contributing to Aden

## Developer Certificate of Origin (DCO)

By contributing to this project, you agree that your contributions are made under the terms of the GNU Affero General Public License v3.0 (AGPL-3.0-or-later).

All commits must include a `Signed-off-by` line in the commit message:

```
Signed-off-by: Your Legal Name <your@email.com>
```

This line attests that you have the right to submit the code under the AGPL license and that you agree to the Developer Certificate of Origin: https://developercertificate.org/

## Copyright

Project copyright is held by RioPlay <rioplay@rioplay.dev>.

Contributions are received under the AGPL license. The BDFL (RioPlay) retains the right to manage dual-licensing arrangements.

## Process

1. Read `.agent/onboarding.adoc` before starting work.
2. Run `aden check .` before submitting.
3. Run `cargo clippy --workspace` to catch style issues.
4. Ensure all `<<refs>>` resolve to existing `[[anchors]]`.
5. Include a `Signed-off-by` line on every commit.
6. Open a pull request with a clear description of changes.

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

### Automatic Pre-Commit Hooks (Recommended)

Run `aden init` to scaffold sample git hooks under `.aden/hooks/`, then install
them for automatic validation:

```bash
# Install pre-commit hook
cp .aden/hooks/pre-commit .git/hooks/

# Install pre-push hook (optional, more thorough)
cp .aden/hooks/pre-push .git/hooks/
```

Now `git commit` automatically runs `aden ci-check .` and `git push` runs full validation.

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
`.cargo/config.toml` pins `TSLP_LANGUAGES`, which compiles those grammars
*statically into the binary* at build time (sources are fetched once during
`cargo build`, which has network):

```toml
# .cargo/config.toml
[env]
TSLP_LANGUAGES = "rust,python,go,typescript,tsx,javascript,c,...,swift"
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

== Modules

See: <<mod-aden-core>>, <<mod-aden-cli>>, <<mod-aden-graph>>
