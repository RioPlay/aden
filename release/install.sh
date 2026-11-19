#!/usr/bin/env bash
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

# Source-free installer shipped inside Aden release bundles.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_DIR="${ADEN_INSTALL_DIR:-${HOME}/.local/bin}"
FORCE=0
UNINSTALL=0

usage() {
    cat <<'EOF'
Usage: ./install.sh [--install-dir DIR] [--force] [--uninstall]

Copies the adjacent prebuilt aden and aden-mcp binaries. It never builds source
or edits shell profiles. Existing binaries require --force. Set
ADEN_INSTALL_DIR to change the default (~/.local/bin).
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --install-dir) [ "$#" -ge 2 ] || { echo "--install-dir requires a value" >&2; exit 2; }; INSTALL_DIR="$2"; shift 2 ;;
        --force) FORCE=1; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

for name in aden aden-mcp; do
    [ -f "$SCRIPT_DIR/$name" ] || { echo "Missing bundled binary: $SCRIPT_DIR/$name" >&2; exit 1; }
done

if [ "$UNINSTALL" = "1" ]; then
    rm -f "$INSTALL_DIR/aden" "$INSTALL_DIR/aden-mcp"
    echo "Removed Aden binaries from $INSTALL_DIR"
    echo "User data under ~/.local/share/aden and ~/.cache/aden-models was preserved."
    exit 0
fi

[ -f "$SCRIPT_DIR/SHA256SUMS" ] || { echo "Missing SHA256SUMS; refusing unverified install." >&2; exit 1; }
[ "$(wc -l < "$SCRIPT_DIR/SHA256SUMS" | tr -d ' ')" = "2" ] || { echo "SHA256SUMS must contain exactly two entries." >&2; exit 1; }
grep -Eq '^[0-9a-fA-F]{64}  aden$' "$SCRIPT_DIR/SHA256SUMS" || { echo "Missing or malformed aden checksum." >&2; exit 1; }
grep -Eq '^[0-9a-fA-F]{64}  aden-mcp$' "$SCRIPT_DIR/SHA256SUMS" || { echo "Missing or malformed aden-mcp checksum." >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then
    (cd "$SCRIPT_DIR" && sha256sum -c SHA256SUMS)
elif command -v shasum >/dev/null 2>&1; then
    (cd "$SCRIPT_DIR" && shasum -a 256 -c SHA256SUMS)
else
    echo "No SHA-256 verifier found; refusing unverified install." >&2
    exit 1
fi

mkdir -p "$INSTALL_DIR"
for name in aden aden-mcp; do
    destination="$INSTALL_DIR/$name"
    if { [ -e "$destination" ] || [ -L "$destination" ]; } && [ "$FORCE" != "1" ]; then
        echo "$destination already exists; rerun with --force to replace both binaries." >&2
        exit 1
    fi
done

STAGE="$(mktemp -d "$INSTALL_DIR/.aden-install.XXXXXX")"
COMMIT_STARTED=0
SUCCESS=0
BACKED_UP_ADEN=0
BACKED_UP_MCP=0
INSTALLED_ADEN=0
INSTALLED_MCP=0
restore_binary() {
    local name="$1" backed_up="$2" installed="$3"
    local backup="$STAGE/backup-$name" destination="$INSTALL_DIR/$name"
    if [ "$installed" = "1" ] && ! rm -f "$destination"; then
        echo "Could not remove partial install $destination" >&2
    fi
    if [ "$backed_up" = "1" ]; then
        if [ -e "$destination" ] || [ -L "$destination" ] || ! mv "$backup" "$destination"; then
            keep_stage=1
            echo "Could not restore $destination; original backup retained at $backup" >&2
        fi
    fi
}
rollback() {
    local status=$? keep_stage=0
    if [ "$SUCCESS" != "1" ] && [ "$COMMIT_STARTED" = "1" ]; then
        restore_binary aden "$BACKED_UP_ADEN" "$INSTALLED_ADEN"
        restore_binary aden-mcp "$BACKED_UP_MCP" "$INSTALLED_MCP"
    fi
    if [ "$keep_stage" = "1" ]; then
        echo "Recovery files retained in $STAGE" >&2
    else
        rm -rf "$STAGE"
    fi
    return "$status"
}
trap rollback EXIT

cp "$SCRIPT_DIR/aden" "$STAGE/aden"
cp "$SCRIPT_DIR/aden-mcp" "$STAGE/aden-mcp"
chmod 755 "$STAGE/aden" "$STAGE/aden-mcp"
# Both staged binaries must be runnable before either destination changes.
"$STAGE/aden" --version
"$STAGE/aden-mcp" --version

COMMIT_STARTED=1
for name in aden aden-mcp; do
    if [ -d "$INSTALL_DIR/$name" ]; then
        echo "$INSTALL_DIR/$name is a directory; refusing to replace it." >&2
        exit 1
    fi
done
if [ -e "$INSTALL_DIR/aden" ] || [ -L "$INSTALL_DIR/aden" ]; then
    mv "$INSTALL_DIR/aden" "$STAGE/backup-aden"
    BACKED_UP_ADEN=1
fi
if [ -e "$INSTALL_DIR/aden-mcp" ] || [ -L "$INSTALL_DIR/aden-mcp" ]; then
    mv "$INSTALL_DIR/aden-mcp" "$STAGE/backup-aden-mcp"
    BACKED_UP_MCP=1
fi
mv "$STAGE/aden" "$INSTALL_DIR/aden"
INSTALLED_ADEN=1
mv "$STAGE/aden-mcp" "$INSTALL_DIR/aden-mcp"
INSTALLED_MCP=1
"$INSTALL_DIR/aden" --version
"$INSTALL_DIR/aden-mcp" --version
SUCCESS=1
rm -rf "$STAGE"
trap - EXIT
echo "Installed aden and aden-mcp in $INSTALL_DIR"
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo "Add $INSTALL_DIR to PATH to invoke Aden from any directory." ;;
esac
echo "Uninstall: $SCRIPT_DIR/install.sh --install-dir '$INSTALL_DIR' --uninstall"
