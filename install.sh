#!/usr/bin/env bash
# Aden Installer — Architecture-agnostic binary installer
# Usage: curl -fsSL https://raw.githubusercontent.com/RioPlay/aden/main/install.sh | bash

set -euo pipefail

REPO="RioPlay/aden"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
    linux)   PLATFORM="unknown-linux-gnu" ;;
    darwin)  PLATFORM="apple-darwin" ;;
    msys*|cygwin*|mingw*)
        PLATFORM="pc-windows-msvc"
        ;;
    *)
        echo "Unsupported OS: $OS"
        echo "Install from source: cargo install --git https://github.com/$REPO"
        exit 1
        ;;
esac

case "$ARCH" in
    x86_64)  ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *)
        echo "Unsupported architecture: $ARCH"
        echo "Install from source: cargo install --git https://github.com/$REPO"
        exit 1
        ;;
esac

if [[ "$PLATFORM" == "pc-windows-msvc" ]]; then
    BINARY="aden.exe"
else
    BINARY="aden"
fi

ASSET="${BINARY}-${ARCH}-${PLATFORM}"
URL="https://github.com/$REPO/releases/latest/download/$ASSET"

echo "Installing Aden..."
echo "  OS:      $OS"
echo "  Arch:    $ARCH"
echo "  Target:  $ASSET"
echo "  To:      $INSTALL_DIR"

mkdir -p "$INSTALL_DIR"

if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$URL" -o "$INSTALL_DIR/$BINARY"
elif command -v wget >/dev/null 2>&1; then
    wget -q "$URL" -O "$INSTALL_DIR/$BINARY"
else
    echo "Error: curl or wget is required."
    exit 1
fi

chmod +x "$INSTALL_DIR/$BINARY"

echo ""
echo "Aden installed successfully to $INSTALL_DIR/$BINARY"
echo ""
echo "Next steps:"
echo "  1. Ensure $INSTALL_DIR is in your PATH"
echo "  2. Run 'aden init' in any project you want to index"
echo "  3. Run 'aden doctor .' to verify your environment"
echo ""
echo "For full documentation: https://github.com/$REPO"
