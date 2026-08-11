#!/usr/bin/env bash
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

set -euo pipefail

usage() { echo "Usage: $0 <target> <version> [dist-dir]"; }
sha256_files() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$@"
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$@"
    else
        echo "SHA-256 generation requires sha256sum or shasum" >&2
        return 1
    fi
}
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[ "$#" -ge 2 ] && [ "$#" -le 3 ] || { usage >&2; exit 2; }
TARGET="$1"
VERSION="${2#v}"
OUTPUT_DIR="${3:-$ROOT/dist}"
BINARY_DIR="${ADEN_RELEASE_BINARY_DIR:-$ROOT/target/$TARGET/release}"

case "$TARGET" in
    *windows*) EXT=".exe"; FORMAT="zip" ;;
    *) EXT=""; FORMAT="tar.gz" ;;
esac
for name in aden aden-mcp; do
    [ -f "$BINARY_DIR/$name$EXT" ] || { echo "Missing $BINARY_DIR/$name$EXT" >&2; exit 1; }
done

NAME="aden-v$VERSION-$TARGET"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
BUNDLE="$STAGE/$NAME"
mkdir -p "$BUNDLE" "$OUTPUT_DIR"
# The zip command runs from the staging directory for deterministic member
# paths, so keep the caller's output directory absolute across that chdir.
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
cp "$BINARY_DIR/aden$EXT" "$BUNDLE/aden$EXT"
cp "$BINARY_DIR/aden-mcp$EXT" "$BUNDLE/aden-mcp$EXT"
cp "$ROOT/release/README.md" "$BUNDLE/README.md"
cp "$ROOT/LICENSE" "$BUNDLE/LICENSE"
cp "$ROOT/NOTICE.md" "$BUNDLE/NOTICE.md"
if [ "$FORMAT" = "zip" ]; then
    cp "$ROOT/release/install.ps1" "$BUNDLE/install.ps1"
else
    cp "$ROOT/release/install.sh" "$BUNDLE/install.sh"
    chmod 755 "$BUNDLE/aden" "$BUNDLE/aden-mcp" "$BUNDLE/install.sh"
fi
(cd "$BUNDLE" && sha256_files "aden$EXT" "aden-mcp$EXT" > SHA256SUMS)
{
    echo "Aden v$VERSION"
    echo "Target: $TARGET"
    echo "Files:"
    for file in "$BUNDLE"/*; do
        [ -f "$file" ] && basename "$file"
    done | LC_ALL=C sort | sed 's/^/  /'
} > "$BUNDLE/MANIFEST.txt"
# Normalize metadata so identical inputs produce identical archives.
find "$BUNDLE" -exec touch -h -t 198001010000.00 {} +
if [ "$FORMAT" = "zip" ]; then
    (cd "$STAGE" && find "$NAME" -type f -print | LC_ALL=C sort | zip -X -q "$OUTPUT_DIR/$NAME.zip" -@)
    ARTIFACT="$OUTPUT_DIR/$NAME.zip"
else
    if tar --version 2>/dev/null | grep -q 'GNU tar'; then
        tar --sort=name --owner=0 --group=0 --numeric-owner --mtime='1980-01-01 UTC' -C "$STAGE" -cf - "$NAME" | gzip -n > "$OUTPUT_DIR/$NAME.tar.gz"
    else
        # macOS ships bsdtar. File mtimes were normalized above; these flags
        # normalize ownership while the sorted file list fixes member order.
        (cd "$STAGE" && find "$NAME" -print | LC_ALL=C sort | tar --uid 0 --gid 0 --uname root --gname root -cf - -T -) | gzip -n > "$OUTPUT_DIR/$NAME.tar.gz"
    fi
    ARTIFACT="$OUTPUT_DIR/$NAME.tar.gz"
fi
(cd "$OUTPUT_DIR" && sha256_files "$(basename "$ARTIFACT")" > "$(basename "$ARTIFACT").sha256")
echo "$ARTIFACT"
