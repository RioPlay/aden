#!/usr/bin/env bash
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

set -euo pipefail

usage() { echo "Usage: $0 <target> <version> [dist-dir]"; }
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
(cd "$BUNDLE" && sha256sum "aden$EXT" "aden-mcp$EXT" > SHA256SUMS)
{
    echo "Aden v$VERSION"
    echo "Target: $TARGET"
    echo "Files:"
    (cd "$BUNDLE" && find . -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort | sed 's/^/  /')
} > "$BUNDLE/MANIFEST.txt"
# Normalize metadata so identical inputs produce identical archives.
find "$BUNDLE" -exec touch -h -t 198001010000.00 {} +
if [ "$FORMAT" = "zip" ]; then
    (cd "$STAGE" && find "$NAME" -type f -print | LC_ALL=C sort | zip -X -q "$OUTPUT_DIR/$NAME.zip" -@)
    ARTIFACT="$OUTPUT_DIR/$NAME.zip"
else
    tar --sort=name --owner=0 --group=0 --numeric-owner --mtime='1980-01-01 UTC' -C "$STAGE" -cf - "$NAME" | gzip -n > "$OUTPUT_DIR/$NAME.tar.gz"
    ARTIFACT="$OUTPUT_DIR/$NAME.tar.gz"
fi
(cd "$OUTPUT_DIR" && sha256sum "$(basename "$ARTIFACT")" > "$(basename "$ARTIFACT").sha256")
echo "$ARTIFACT"
