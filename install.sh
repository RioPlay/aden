#!/usr/bin/env bash
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

# Aden install script — copies release binaries to ~/.local/bin
# and ensures the directory is in PATH.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INVOKE_DIR="$(pwd)"   # capture before we cd into the project root
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
PROJECT_ROOT="${PROJECT_ROOT:-$SCRIPT_DIR}"

echo "=== Aden Installer ==="
echo ""

# Build release binaries.
# Opt into local hybrid (dense) search with `ADEN_DENSE=1 ./install.sh` (or pass
# `--dense`). It adds the tract + bge embedding stack to the `aden` binary; the
# MCP server spawns this same binary, so enabling it here turns on hybrid search
# for BOTH the CLI and MCP. Fetch the model afterwards: scripts/fetch-bge-model.sh
cd "$PROJECT_ROOT"

# Activate the repo's git hooks (pre-push CI gate) so code can't be pushed
# without passing the checks. No-op outside a git checkout.
if git -C "$PROJECT_ROOT" rev-parse --git-dir >/dev/null 2>&1; then
  git -C "$PROJECT_ROOT" config core.hooksPath git-hooks
  echo "Git hooks activated (pre-push CI gate)."
fi

DENSE="${ADEN_DENSE:-0}"
[ "${1:-}" = "--dense" ] && DENSE=1
if [ "$DENSE" = "1" ]; then
  echo "Building release binaries (with dense/hybrid search)..."
  cargo build --release -p aden-cli --features dense 2>&1 | tail -3
  cargo build --release -p aden-mcp 2>&1 | tail -3
else
  echo "Building release binaries..."
  cargo build --release -p aden-cli -p aden-mcp 2>&1 | tail -3
fi

# Ensure install directory exists
mkdir -p "$INSTALL_DIR"

# Copy binaries.
# Use `install`, not `cp`: it unlinks the destination first, so replacing a
# binary that is currently running (e.g. the aden-mcp server held open by an
# editor/agent session) succeeds with a fresh inode instead of failing with
# "Text file busy" (ETXTBSY). The running process keeps the old inode until it
# restarts; the new copy is what the next launch picks up.
echo "Installing binaries to $INSTALL_DIR ..."
install -m 755 "$PROJECT_ROOT/target/release/aden" "$INSTALL_DIR/aden"
install -m 755 "$PROJECT_ROOT/target/release/aden-mcp" "$INSTALL_DIR/aden-mcp"

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

# Offer to seed the append-only aden usage block into a project's AGENTS.md so
# AI agents use aden by default (ADR-004). Append-only and idempotent — it only
# ever touches its own marked block. Skipped automatically on non-interactive
# installs (e.g. piped from curl) so it never blocks or edits a file unprompted.
if [ -t 0 ]; then
    echo ""
    DEFAULT_TARGET="$INVOKE_DIR"
    # Don't default to aden's own checkout — that AGENTS.md is hand-maintained.
    if [ "$INVOKE_DIR" = "$PROJECT_ROOT" ]; then
        DEFAULT_TARGET=""
    fi
    echo "Add aden usage guidance to a project's AGENTS.md? (helps AI agents use aden)"
    if [ -n "$DEFAULT_TARGET" ]; then
        printf "  Project path [%s], or 'n' to skip: " "$DEFAULT_TARGET"
    else
        printf "  Project path (blank to skip): "
    fi
    read -r REPLY_TARGET || REPLY_TARGET=""

    # Resolve the answer: empty reply takes the default; 'n'/'N' always skips.
    TARGET=""
    case "$REPLY_TARGET" in
        "")    TARGET="$DEFAULT_TARGET" ;;
        n|N)   TARGET="" ;;
        *)     TARGET="$REPLY_TARGET" ;;
    esac

    if [ -n "$TARGET" ]; then
        if [ -d "$TARGET" ]; then
            "$INSTALL_DIR/aden" agents-md "$TARGET" || echo "  (skipped: aden agents-md failed)"
        else
            echo "  (skipped: '$TARGET' is not a directory)"
        fi
    fi
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
