#!/usr/bin/env bash
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Guard maintainer identity so a past legal-name form cannot re-enter git
# metadata or tracked files.
#
# What this checks (without embedding any personal legal name in-tree):
#   1. Cargo workspace authors stay RioPlay-only.
#   2. Commit author/committer names never use the old "Alias (Legal Name)"
#      parenthetical form (`RioPlay (`…).
#   3. Optional private patterns from env or a gitignored local file — never
#      commit those patterns.
#
# Usage:
#   scripts/check-identity-hygiene.sh              # tree + recent reachable tips
#   scripts/check-identity-hygiene.sh RANGE        # also scan git log RANGE
#   IDENTITY_FORBIDDEN_FILE=.identity-blocklist \
#     scripts/check-identity-hygiene.sh
#
# RANGE examples: origin/main..HEAD   HEAD~20..HEAD   --all
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "${ROOT}" ]]; then
  echo "check-identity-hygiene: not inside a git work tree" >&2
  exit 1
fi
cd "$ROOT"

bad=0
range="${1:-}"

fail() {
  echo "check-identity-hygiene: $*" >&2
  bad=1
}

# ── 1. Workspace authors contract ──────────────────────────────────────────
expected_authors='authors = ["RioPlay <rioplay@rioplay.dev>"]'
if ! grep -Fq "$expected_authors" Cargo.toml; then
  fail "Cargo.toml workspace authors must be exactly: ${expected_authors}"
fi

# ── 2. Commit metadata: forbid "RioPlay (…)" parenthetical expansion ───────
# Historical rewrites collapsed maintainer identity to plain "RioPlay". The
# parenthetical form was the poison vector; reject it on any scanned range.
scan_commit_names() {
  local rev_args=("$@")
  local name
  while IFS= read -r name; do
    [[ -z "$name" ]] && continue
    if [[ "$name" == "RioPlay ("* ]]; then
      # Do not echo the author string — it may contain a private legal name
      # and would land in CI logs. Inspect locally with git log --format='%an'.
      fail "forbidden author/committer form (parenthetical expansion; details omitted)"
    fi
  done < <(git log "${rev_args[@]}" --format='%an%n%cn' 2>/dev/null || true)
}

if [[ -n "$range" ]]; then
  # shellcheck disable=SC2086
  scan_commit_names $range
else
  # Default: every currently reachable tip (cheap; full --all is fine on aden).
  scan_commit_names --all
fi

# ── 3. Optional private content blocklist (never committed) ────────────────
# Patterns are one per line; empty lines and # comments ignored.
# Prefer env IDENTITY_FORBIDDEN_FILE, else .identity-blocklist if present.
blocklist_file="${IDENTITY_FORBIDDEN_FILE:-}"
if [[ -z "$blocklist_file" && -f .identity-blocklist ]]; then
  blocklist_file=".identity-blocklist"
fi

if [[ -n "$blocklist_file" ]]; then
  if [[ ! -f "$blocklist_file" ]]; then
    fail "IDENTITY_FORBIDDEN_FILE not found: ${blocklist_file}"
  else
    while IFS= read -r raw || [[ -n "$raw" ]]; do
      pattern="${raw%%#*}"
      pattern="$(echo "$pattern" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
      [[ -z "$pattern" ]] && continue
      # git grep defaults to tracked files only (never .git/target/ignored).
      if git grep -I -i -F -n -- "$pattern" >/dev/null 2>&1; then
        fail "tracked tree matches private identity blocklist pattern (file kept private; not printed)"
        # Paths only — never echo the pattern (may be a private legal name).
        git grep -I -i -F -l -- "$pattern" 2>/dev/null \
          | sed 's/^/  hit: /' >&2 || true
      fi
    done < "$blocklist_file"
  fi
fi

# Optional env: newline-separated patterns (e.g. from a CI secret). Never log them.
if [[ -n "${IDENTITY_FORBIDDEN_PATTERNS:-}" ]]; then
  while IFS= read -r pattern || [[ -n "$pattern" ]]; do
    pattern="$(echo "$pattern" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
    [[ -z "$pattern" ]] && continue
    if git grep -I -i -F -n -- "$pattern" >/dev/null 2>&1; then
      fail "tracked tree matches IDENTITY_FORBIDDEN_PATTERNS entry (value not printed)"
      git grep -I -i -F -l -- "$pattern" 2>/dev/null \
        | sed 's/^/  hit: /' >&2 || true
    fi
  done <<< "$IDENTITY_FORBIDDEN_PATTERNS"
fi

if [[ "$bad" -ne 0 ]]; then
  echo "check-identity-hygiene: FAILED — maintainer identity must stay RioPlay-only." >&2
  echo "  set git user.name=RioPlay and user.email=rioplay@rioplay.dev" >&2
  echo "  do not reintroduce parenthetical legal names in author fields" >&2
  exit 1
fi

echo "check-identity-hygiene: ok"
