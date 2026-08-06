#!/usr/bin/env bash
# Build Aden into an isolated Cargo target, copy only the shipped binaries,
# delete the build tree, then prove bundled Rust and Go grammars still parse.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/aden-release-portability.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

BUILD="$WORK/build"
STAGE="$WORK/installed"
FIXTURE="$WORK/fixture"
DATA="$WORK/data"
mkdir -p "$STAGE" "$FIXTURE"

printf '%s\n' "[portability] isolated release build"
(
    cd "$ROOT"
    CARGO_TARGET_DIR="$BUILD" TSLP_LINK_MODE=static \
        cargo build --locked --release -p aden-cli -p aden-mcp
)
install -m 755 "$BUILD/release/aden" "$STAGE/aden"
install -m 755 "$BUILD/release/aden-mcp" "$STAGE/aden-mcp"
rm -rf "$BUILD"
test ! -e "$BUILD"

printf 'pub fn rust_release_smoke() {}\n' >"$FIXTURE/lib.rs"
printf 'package smoke\nfunc GoReleaseSmoke() {}\n' >"$FIXTURE/smoke.go"

"$STAGE/aden" --version >/dev/null
"$STAGE/aden-mcp" --version >/dev/null
ADEN_DATA_DIR="$DATA" "$STAGE/aden" gen --human --verbose "$FIXTURE" \
    >"$WORK/gen.log" 2>&1
if grep -q 'Parse failed' "$WORK/gen.log"; then
    cat "$WORK/gen.log" >&2
    echo "[portability] installed binary attempted to load a grammar from the deleted build tree" >&2
    exit 1
fi

ADEN_DATA_DIR="$DATA" "$STAGE/aden" locate --symbol rust_release_smoke "$FIXTURE" \
    >"$WORK/rust.json"
ADEN_DATA_DIR="$DATA" "$STAGE/aden" locate --symbol GoReleaseSmoke "$FIXTURE" \
    >"$WORK/go.json"
python3 - "$WORK" <<'PY'
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
for language, symbol in (("rust", "rust_release_smoke"), ("go", "GoReleaseSmoke")):
    response = json.loads((root / f"{language}.json").read_text(encoding="utf-8"))
    items = response.get("items", [])
    assert items, f"{language} grammar did not index {symbol}: {response}"
    assert any(symbol in item.get("anchor", "") for item in items), response
PY

printf '%s\n' "[portability] PASS: copied binaries parse Rust and Go after build tree deletion"
