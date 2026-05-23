#!/usr/bin/env bash
# Aden Installer — POSIX-compliant, architecture-agnostic
# Supports: bash, dash, zsh, busybox ash, Git Bash, WSL
# Usage: curl -fsSL https://rioplay.dev/install | bash
#        wget -qO- https://rioplay.dev/install | bash

set -euo pipefail

REPO="RioPlay/aden"
API_URL="https://api.github.com/repos/$REPO/releases/latest"

# Allow override
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

# ── Helpers ──────────────────────────────────────
banner() { printf '\n=== %s ===\n' "$1"; }
die() { printf 'Error: %s\n' "$1" >&2; exit 1; }

# ── Detect OS ────────────────────────────────────
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
    linux)            PLATFORM="unknown-linux-gnu" ;;
    darwin)           PLATFORM="apple-darwin" ;;
    msys*|cygwin*|mingw*|nt)
                      PLATFORM="pc-windows-msvc" ;;
    freebsd|openbsd|netbsd)
                      PLATFORM="unknown-freebsd" ;; # best-effort fallback
    *) die "Unsupported OS: $OS. Build from source: cargo install --git https://github.com/$REPO" ;;
esac

# ── Detect Arch ──────────────────────────────────
case "$ARCH" in
    x86_64|amd64)          ARCH="x86_64" ;;
    aarch64|arm64)         ARCH="aarch64" ;;
    armv7l)                ARCH="armv7" ;;
    i386|i686)             ARCH="i686" ;;
    *) die "Unsupported architecture: $ARCH. Build from source: cargo install --git https://github.com/$REPO" ;;
esac

# ── Binary Name ──────────────────────────────────
ASSET="aden-${ARCH}-${PLATFORM}"
if [[ "$PLATFORM" == "pc-windows-msvc" ]]; then
    BINARY="aden.exe"
    ZIP_EXT="zip"
else
    BINARY="aden"
    ZIP_EXT="tar.gz"
fi

DOWNLOAD_URL="https://github.com/$REPO/releases/latest/download/${ASSET}.${ZIP_EXT}"

# ── Presence Check ───────────────────────────────
if command -v aden >/dev/null 2>&1; then
    CURRENT=$(aden --version 2>/dev/null || echo "unknown")
    echo "Aden is already installed: $CURRENT"
    echo "To reinstall or upgrade, run:"
    echo "  curl -fsSL https://rioplay.dev/install | bash -s -- --force"
    exit 0
fi

# ── Download ─────────────────────────────────────
banner "Installing Aden"
printf '  Repository : %s\n' "$REPO"
printf '  Asset      : %s\n' "${ASSET}.${ZIP_EXT}"
printf '  Target     : %s/%s\n' "$OS" "$ARCH"
printf '  Install to : %s\n' "$INSTALL_DIR"

mkdir -p "$INSTALL_DIR"

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$DOWNLOAD_URL" -o "$TMPDIR/archive.${ZIP_EXT}"
elif command -v wget >/dev/null 2>&1; then
    wget -q "$DOWNLOAD_URL" -O "$TMPDIR/archive.${ZIP_EXT}"
elif command -v fetch >/dev/null 2>&1; then
    fetch -o "$TMPDIR/archive.${ZIP_EXT}" "$DOWNLOAD_URL"
else
    die "No download tool found (tried: curl, wget, fetch). Please install curl and retry."
fi

# ── Extract ──────────────────────────────────────
if [[ "$ZIP_EXT" == "zip" ]]; then
    if command -v unzip >/dev/null 2>&1; then
        unzip -q "$TMPDIR/archive.${ZIP_EXT}" -d "$TMPDIR"
    else
        die "unzip is required to extract Windows archives."
    fi
else
    if command -v tar >/dev/null 2>&1; then
        tar -xzf "$TMPDIR/archive.${ZIP_EXT}" -C "$TMPDIR"
    else
        die "tar is required to extract archives."
    fi
fi

# Find the binary (may be in a subdirectory depending on archive layout)
FOUND=$(find "$TMPDIR" -type f -name "$BINARY" | head -n1)
if [[ -z "$FOUND" ]]; then
    die "Could not find '$BINARY' in downloaded archive."
fi

cp "$FOUND" "$INSTALL_DIR/$BINARY"
chmod +x "$INSTALL_DIR/$BINARY"

# ── Verify ───────────────────────────────────────
if ! command -v aden >/dev/null 2>&1; then
    printf '\nWarning: %s is not in your PATH.\n' "$INSTALL_DIR"
    printf 'Add this to your shell profile (.bashrc, .zshrc, etc.):\n'
    printf '  export PATH="%s:$PATH"\n\n' "$INSTALL_DIR"
fi

banner "Success"
printf 'Aden installed to: %s/%s\n' "$INSTALL_DIR" "$BINARY"
printf 'Version: %s\n' "$(aden --version 2>/dev/null || echo "unknown")"
printf '\nNext steps:\n'
printf '  1. Ensure %s is in your PATH\n' "$INSTALL_DIR"
printf '  2. Run: aden init       # in any project you want to index\n'
printf '  3. Run: aden doctor .   # verify your environment\n'
printf '\nFor docs: https://github.com/%s\n' "$REPO"
