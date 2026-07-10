#!/usr/bin/env bash
# Reproducible DX-103 journey probe. Set FULL_INSTALL=1 to measure the literal
# checkout build/install -> first answer path into an isolated install directory.
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ADEN_BIN="${ADEN_BIN:-aden}"
SOURCE_COMMIT="$(git -C "$PROJECT_ROOT" rev-parse HEAD 2>/dev/null || printf unknown)"
SOURCE_TREE="dirty"
git -C "$PROJECT_ROOT" diff --quiet && git -C "$PROJECT_ROOT" diff --cached --quiet && SOURCE_TREE="clean"
repo="$(mktemp -d)"
trap 'rm -rf "$repo"' EXIT
mkdir -p "$repo/src"
printf '%s\n' 'pub fn entry_point() {}' > "$repo/src/lib.rs"
git -C "$repo" init -q
git -C "$repo" config user.email golden@example.invalid
git -C "$repo" config user.name Golden
git -C "$repo" add . && git -C "$repo" commit -qm fixture
PROJECT_HEAD="$(git -C "$repo" rev-parse HEAD)"
started_at="$(date -u +%FT%TZ)"
start_ns="$(date +%s%N)"
if [[ "${FULL_INSTALL:-0}" == "1" ]]; then
  install_dir="$repo/installed-bin"
  INSTALL_DIR="$install_dir" PROJECT_ROOT="$PROJECT_ROOT" bash "$PROJECT_ROOT/install.sh" --yes >/dev/null
  ADEN_BIN="$install_dir/aden"
  "$install_dir/aden-mcp" --version >/dev/null
fi
"$ADEN_BIN" ask "Where is entry_point?" "$repo" --strict --budget 512 >/dev/null
"$ADEN_BIN" understand entry_point "$repo" --json >/dev/null
"$ADEN_BIN" check "$repo" --severity Forbid >/dev/null
elapsed_ms=$(( ($(date +%s%N) - start_ns) / 1000000 ))
binary_sha256="$(sha256sum "$ADEN_BIN" | awk '{print $1}')"
printf 'golden_journey_started_at=%s elapsed_ms=%s commands=3 binary=%s version=%s source_commit=%s source_tree=%s project_head=%s binary_sha256=%s full_install=%s\n' \
  "$started_at" "$elapsed_ms" "$ADEN_BIN" "$("$ADEN_BIN" --version | head -n1)" "$SOURCE_COMMIT" "$SOURCE_TREE" "$PROJECT_HEAD" "$binary_sha256" "${FULL_INSTALL:-0}"
