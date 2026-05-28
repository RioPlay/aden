#!/usr/bin/env bash
# Aden install script — copies release binaries to ~/.local/bin
# and ensures the directory is in PATH.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
PROJECT_ROOT="${PROJECT_ROOT:-$SCRIPT_DIR}"

echo "=== Aden Installer ==="
echo ""

# Build release binaries
echo "Building release binaries..."
cd "$PROJECT_ROOT"
cargo build --release -p aden-cli -p aden-mcp 2>&1 | tail -3

# Ensure install directory exists
mkdir -p "$INSTALL_DIR"

# Copy binaries
echo "Installing binaries to $INSTALL_DIR ..."
cp "$PROJECT_ROOT/target/release/aden" "$INSTALL_DIR/"
cp "$PROJECT_ROOT/target/release/aden-mcp" "$INSTALL_DIR/"
chmod +x "$INSTALL_DIR/aden" "$INSTALL_DIR/aden-mcp"

# Detect shell profile
SHELL_NAME="${SHELL##*/}"
case "$SHELL_NAME" in
    bash) PROFILE="$HOME/.bashrc" ;;
    zsh)  PROFILE="$HOME/.zshrc" ;;
    fish) PROFILE="$HOME/.config/fish/config.fish" ;;
    *)    PROFILE="$HOME/.profile" ;;
esac

# Check if install dir is already in PATH
if echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
    echo ""
    echo "$INSTALL_DIR is already in your PATH."
else
    echo ""
    echo "$INSTALL_DIR is NOT in your PATH. Adding it to $PROFILE ..."

    # Determine export line format
    if [ "$SHELL_NAME" = "fish" ]; then
        echo "set -gx PATH $INSTALL_DIR \$PATH" >> "$PROFILE"
    else
        echo "export PATH=\"$INSTALL_DIR:\$PATH\"" >> "$PROFILE"
    fi

    echo ""
    echo "Added export to $PROFILE"
    echo "Run this to activate:"
    echo "  source $PROFILE"
fi

echo ""
echo "=== Installed ==="
echo "  aden      -> $INSTALL_DIR/aden"
echo "  aden-mcp  -> $INSTALL_DIR/aden-mcp"
echo ""
echo "Verify:"
echo "  which aden"
echo "  aden --version"
echo ""
