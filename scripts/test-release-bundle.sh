#!/usr/bin/env bash
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

validate_archive() {
    local archive="$1" target="$2" version="${3#v}" name extension bundle
    name="aden-v$version-$target"
    case "$target" in
        *windows*) extension="zip" ;;
        *) extension="tar.gz" ;;
    esac
    [ "$(basename "$archive")" = "$name.$extension" ] || {
        echo "Unexpected archive name: $(basename "$archive") (expected $name.$extension)" >&2
        return 1
    }
    [ -f "$archive" ] || { echo "Archive not found: $archive" >&2; return 1; }
    if [ -f "$archive.sha256" ]; then
        (cd "$(dirname "$archive")" && sha256sum -c "$(basename "$archive").sha256")
    fi
    if [ "$extension" = "zip" ]; then
        command -v unzip >/dev/null || { echo "unzip is required to validate Windows bundles" >&2; return 1; }
        unzip -q "$archive" -d "$TMP/extracted"
    else
        tar -xzf "$archive" -C "$TMP/extracted"
    fi
    bundle="$TMP/extracted/$name"
    [ -d "$bundle" ] || { echo "Archive root $name is missing" >&2; return 1; }
    for file in README.md LICENSE NOTICE.md MANIFEST.txt SHA256SUMS; do
        [ -f "$bundle/$file" ] || { echo "Bundle file missing: $file" >&2; return 1; }
    done
    if [ "$extension" = "zip" ]; then
        for file in aden.exe aden-mcp.exe install.ps1; do
            [ -f "$bundle/$file" ] || { echo "Bundle file missing: $file" >&2; return 1; }
        done
    else
        for file in aden aden-mcp install.sh; do
            [ -f "$bundle/$file" ] || { echo "Bundle file missing: $file" >&2; return 1; }
        done
        [ -x "$bundle/aden" ] && [ -x "$bundle/aden-mcp" ] && [ -x "$bundle/install.sh" ] || {
            echo "Unix bundle executables lack execute permission" >&2; return 1
        }
    fi
    grep -Fqx "Aden v$version" "$bundle/MANIFEST.txt"
    grep -Fqx "Target: $target" "$bundle/MANIFEST.txt"
    (cd "$bundle" && sha256sum -c SHA256SUMS)
    echo "Validated release archive: $(basename "$archive")"
}

if [ "$#" -ne 0 ]; then
    [ "$#" -eq 3 ] || { echo "Usage: $0 [<archive> <target> <version>]" >&2; exit 2; }
    mkdir -p "$TMP/extracted"
    validate_archive "$1" "$2" "$3"
    exit 0
fi

BIN="$TMP/bin"
mkdir -p "$BIN" "$TMP/out-a" "$TMP/out-b"
for name in aden aden-mcp; do
    printf '#!/usr/bin/env sh\necho "%s 0.0.0-test"\n' "$name" > "$BIN/$name"
    chmod +x "$BIN/$name"
done
ADEN_RELEASE_BINARY_DIR="$BIN" "$ROOT/scripts/package-release.sh" x86_64-unknown-linux-gnu 0.0.0-test "$TMP/out-a"
ADEN_RELEASE_BINARY_DIR="$BIN" "$ROOT/scripts/package-release.sh" x86_64-unknown-linux-gnu 0.0.0-test "$TMP/out-b"
cmp "$TMP/out-a/aden-v0.0.0-test-x86_64-unknown-linux-gnu.tar.gz" "$TMP/out-b/aden-v0.0.0-test-x86_64-unknown-linux-gnu.tar.gz"
cmp "$TMP/out-a/aden-v0.0.0-test-x86_64-unknown-linux-gnu.tar.gz.sha256" "$TMP/out-b/aden-v0.0.0-test-x86_64-unknown-linux-gnu.tar.gz.sha256"
mkdir -p "$TMP/extracted"
validate_archive "$TMP/out-a/aden-v0.0.0-test-x86_64-unknown-linux-gnu.tar.gz" x86_64-unknown-linux-gnu 0.0.0-test
mv "$TMP/extracted/aden-v0.0.0-test-x86_64-unknown-linux-gnu" "$TMP/aden-v0.0.0-test-x86_64-unknown-linux-gnu"
BUNDLE="$TMP/aden-v0.0.0-test-x86_64-unknown-linux-gnu"
DEST="$TMP/installed"
"$BUNDLE/install.sh" --install-dir "$DEST"
test "$("$DEST/aden" --version)" = "aden 0.0.0-test"
if "$BUNDLE/install.sh" --install-dir "$DEST" 2>/dev/null; then
    echo "Installer unexpectedly overwrote existing binaries" >&2; exit 1
fi
"$BUNDLE/install.sh" --install-dir "$DEST" --force
# Malformed checksum manifests must fail closed without changing the live pair.
cp "$BUNDLE/SHA256SUMS" "$TMP/good-sums"
printf '%s\n' 'unexpected checksum content' >> "$BUNDLE/SHA256SUMS"
if "$BUNDLE/install.sh" --install-dir "$DEST" --force 2>/dev/null; then
    echo "Installer accepted a malformed checksum manifest" >&2; exit 1
fi
test "$("$DEST/aden" --version)" = "aden 0.0.0-test"
test "$("$DEST/aden-mcp" --version)" = "aden-mcp 0.0.0-test"
cp "$TMP/good-sums" "$BUNDLE/SHA256SUMS"

# Absence of a SHA-256 implementation is a hard failure, never a warning.
NO_HASH_PATH="$TMP/no-hash-path"
mkdir "$NO_HASH_PATH"
for tool in dirname wc tr grep; do ln -s "$(command -v "$tool")" "$NO_HASH_PATH/$tool"; done
if PATH="$NO_HASH_PATH" /bin/bash "$BUNDLE/install.sh" --install-dir "$DEST" --force 2>/dev/null; then
    echo "Installer proceeded without a SHA-256 verifier" >&2; exit 1
fi
test "$("$DEST/aden" --version)" = "aden 0.0.0-test"
test "$("$DEST/aden-mcp" --version)" = "aden-mcp 0.0.0-test"

# A bundle whose second binary fails its staged smoke check must leave the old
# two-binary installation intact (never a mixed-version pair).
cp "$BUNDLE/aden-mcp" "$TMP/good-aden-mcp"
printf '#!/usr/bin/env sh\nexit 17\n' > "$BUNDLE/aden-mcp"
chmod +x "$BUNDLE/aden-mcp"
(cd "$BUNDLE" && sha256sum aden aden-mcp > SHA256SUMS)
if "$BUNDLE/install.sh" --install-dir "$DEST" --force 2>/dev/null; then
    echo "Installer accepted a staged binary that failed its smoke check" >&2; exit 1
fi
test "$("$DEST/aden" --version)" = "aden 0.0.0-test"
test "$("$DEST/aden-mcp" --version)" = "aden-mcp 0.0.0-test"
cp "$TMP/good-aden-mcp" "$BUNDLE/aden-mcp"
cp "$TMP/good-sums" "$BUNDLE/SHA256SUMS"

# PowerShell is not guaranteed locally; statically preserve its fail-closed
# parser and rollback invariants here, while Windows CI executes the installer.
grep -Fq '$Expected.Count -ne 2' "$ROOT/release/install.ps1"
grep -Fq 'Duplicate SHA256SUMS entry' "$ROOT/release/install.ps1"
grep -Fq 'restored the previous Aden binary pair' "$ROOT/release/install.ps1"
grep -Fq 'Staged aden-mcp.exe smoke check failed' "$ROOT/release/install.ps1"
"$BUNDLE/install.sh" --install-dir "$DEST" --uninstall
test ! -e "$DEST/aden" && test ! -e "$DEST/aden-mcp"
echo "Release bundle smoke test passed."
