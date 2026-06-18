#!/usr/bin/env bash
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

# Aden installer — a guided walkthrough.
#
# Every step says WHAT it changes, WHERE, and WHY before it does anything, and
# every change to a file you own (shell profile, editor configs, AGENTS.md) asks
# first. Defaults are chosen so pressing Enter through the walkthrough yields a
# complete, working setup.
#
#   ./install.sh               guided interactive install (the default)
#   ./install.sh --yes         accept every default, no prompts
#   ./install.sh --minimal     binaries + PATH only (skip MCP/AGENTS.md steps)
#   ./install.sh --dense       build with local hybrid (dense) search support
#   ./install.sh --uninstall   guided removal
#
# Environment overrides: INSTALL_DIR (default ~/.local/bin), PROJECT_ROOT
# (default: this checkout), ADEN_DENSE=1 (same as --dense).
#
# Non-interactive runs (no TTY, e.g. piped from curl or CI) perform only the
# self-contained steps — build, copy binaries — and PRINT instructions for the
# steps that would edit your files, instead of editing them unprompted.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INVOKE_DIR="$(pwd)" # captured before any cd — used as the AGENTS.md default
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
PROJECT_ROOT="${PROJECT_ROOT:-$SCRIPT_DIR}"

YES=0
MINIMAL=0
DENSE="${ADEN_DENSE:-0}"
UNINSTALL=0
for arg in "$@"; do
    case "$arg" in
        -y | --yes) YES=1 ;;
        --minimal) MINIMAL=1 ;;
        --dense) DENSE=1 ;;
        --uninstall) UNINSTALL=1 ;;
        -h | --help)
            sed -n '5,23p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "Unknown option: $arg (try --help)" >&2
            exit 2
            ;;
    esac
done

# No TTY → never prompt, never edit user-owned files.
TTY=0
[ -t 0 ] && TTY=1
if [ "$TTY" = "0" ]; then
    YES=1
fi

# ---------------------------------------------------------------- helpers ---
STEP_N=0
step() {
    STEP_N=$((STEP_N + 1))
    echo ""
    echo "── Step $STEP_N: $1"
}
note() { echo "   $*"; }
ok() { echo "   ✓ $*"; }
skip() { echo "   – $*"; }

# ask_yn "question" default(y|n) → 0 yes / 1 no. --yes takes the default.
ask_yn() {
    local q="$1" def="${2:-y}" hint reply
    if [ "$YES" = "1" ] || [ "$TTY" = "0" ]; then
        [ "$def" = "y" ] && return 0 || return 1
    fi
    [ "$def" = "y" ] && hint="[Y/n]" || hint="[y/N]"
    printf "   %s %s " "$q" "$hint"
    read -r reply || reply=""
    case "${reply:-$def}" in
        y | Y | yes | YES) return 0 ;;
        *) return 1 ;;
    esac
}

# Shell profile for PATH guidance.
SHELL_NAME="${SHELL##*/}"
case "$SHELL_NAME" in
    bash) PROFILE="$HOME/.bashrc" ;;
    zsh) PROFILE="$HOME/.zshrc" ;;
    fish) PROFILE="$HOME/.config/fish/config.fish" ;;
    *) PROFILE="$HOME/.profile" ;;
esac
if [ "$SHELL_NAME" = "fish" ]; then
    PATH_LINE="set -gx PATH $INSTALL_DIR \$PATH"
else
    PATH_LINE="export PATH=\"$INSTALL_DIR:\$PATH\""
fi

in_path() { echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; }

# -------------------------------------------------------------- uninstall ---
if [ "$UNINSTALL" = "1" ]; then
    echo "═══ Aden Uninstaller ═══"
    echo ""
    echo "This removes what install.sh put on this machine, and tells you about"
    echo "anything it leaves behind (so nothing disappears without your say-so)."

    step "Unregister the MCP server from AI platforms"
    note "Why first: 'aden mcp uninstall' needs the aden binary that is about to be removed."
    if [ -x "$INSTALL_DIR/aden" ]; then
        "$INSTALL_DIR/aden" mcp list 2>/dev/null || true
        if ask_yn "Remove aden from the configured platforms above?" y; then
            "$INSTALL_DIR/aden" mcp uninstall 2>/dev/null ||
                note "(mcp uninstall reported an issue — check 'aden mcp list' manually)"
        fi
    else
        skip "no aden binary at $INSTALL_DIR — skipping MCP unregistration"
    fi

    step "Remove binaries from $INSTALL_DIR"
    for bin in aden aden-mcp; do
        if [ -e "$INSTALL_DIR/$bin" ]; then
            rm -f "$INSTALL_DIR/$bin" && ok "removed $INSTALL_DIR/$bin"
        else
            skip "$INSTALL_DIR/$bin not present"
        fi
    done

    step "What stays (remove manually if you want a clean slate)"
    note "• PATH line in $PROFILE — added only if you said yes during install:"
    note "    $PATH_LINE"
    note "• Graph stores (per-user, OUTSIDE your repos): ~/.local/share/aden/"
    note "  Safe to delete; they are rebuilt from source by the next 'aden gen'."
    note "• Dense-search model (if fetched): ~/.cache/aden-models/"
    note "• 'aden' blocks in any AGENTS.md you seeded — delete the marked block."
    echo ""
    echo "Done."
    exit 0
fi

# ---------------------------------------------------------------- install ---
echo "═══ Aden Installer ═══"
if [ "$TTY" = "0" ]; then
    echo "(non-interactive: building + copying binaries only; printing"
    echo " instructions for every step that would edit your files)"
elif [ "$YES" = "1" ]; then
    echo "(--yes: accepting every default without prompting)"
fi

# 1 ─ Preflight ───────────────────────────────────────────────────────────────
step "Preflight — what this will do"
command -v cargo >/dev/null 2>&1 || {
    echo "   ✗ cargo not found. Install Rust first: https://rustup.rs" >&2
    exit 1
}
EXISTING=""
if command -v aden >/dev/null 2>&1; then
    EXISTING="$(aden --version 2>/dev/null || true)"
fi
note "Plan (each step explains itself and asks before touching your files):"
note "  1. build release binaries from $PROJECT_ROOT"
[ "$DENSE" = "1" ] && note "     … with dense/hybrid search compiled in (--dense)"
note "  2. copy 'aden' + 'aden-mcp' into $INSTALL_DIR  (user-local, no sudo)"
note "  3. make sure $INSTALL_DIR is in your PATH       (asks before editing $PROFILE)"
if [ "$MINIMAL" = "0" ]; then
    note "  4. register the MCP server with your AI tools   (asks; per-platform)"
    note "  5. optionally seed AGENTS.md guidance in a project of yours"
fi
note "  …. verify, then print a summary with an undo line for every change."
note ""
note "Where your data will live (for orientation, nothing is created yet):"
note "  • graph stores: per-user under ~/.local/share/aden/ — your repos are"
note "    never written to except an ignorable .aden/ folder when you opt in."
[ -n "$EXISTING" ] && note "Existing install detected: $EXISTING — it will be replaced in place."
if ! ask_yn "Proceed?" y; then
    echo "Aborted — nothing was changed."
    exit 0
fi

# 2 ─ Build ───────────────────────────────────────────────────────────────────
step "Build release binaries"
cd "$PROJECT_ROOT"
# Aden's own checkout: activate the repo's git hooks so local work runs the same
# checks as CI. core.hooksPath=git-hooks enables BOTH hooks in that directory: the
# pre-push CI gate (build, test, aden ci-check) and the pre-commit impact gate
# (aden impact-diff --staged: blast radius + tests to run, informational, never
# blocks). Configures THIS repo only, nothing global.
if git -C "$PROJECT_ROOT" rev-parse --git-dir >/dev/null 2>&1; then
    git -C "$PROJECT_ROOT" config core.hooksPath git-hooks
    ok "git hooks activated for this checkout (pre-push CI gate + pre-commit impact gate; repo-local)"
fi
if [ "$DENSE" = "1" ]; then
    note "Building WITH dense/hybrid search: adds the tract + bge embedding stack."
    note "The MCP server spawns this same binary, so hybrid search turns on for both."
    cargo build --release -p aden-cli --features dense 2>&1 | tail -3
    cargo build --release -p aden-mcp 2>&1 | tail -3
else
    note "Building the standard binaries (lexical search; --dense adds hybrid)."
    cargo build --release -p aden-cli -p aden-mcp 2>&1 | tail -3
fi
ok "built target/release/{aden, aden-mcp}"

# 3 ─ Copy binaries ───────────────────────────────────────────────────────────
step "Install binaries → $INSTALL_DIR"
note "Why here: user-local bin dir — no sudo, no system files, trivially undoable."
mkdir -p "$INSTALL_DIR"
# `install`, not `cp`: it unlinks the destination first, so replacing a binary
# that is currently running (e.g. aden-mcp held open by an editor session)
# succeeds with a fresh inode instead of failing with ETXTBSY. The running
# process keeps the old inode until it restarts.
install -m 755 "$PROJECT_ROOT/target/release/aden" "$INSTALL_DIR/aden"
install -m 755 "$PROJECT_ROOT/target/release/aden-mcp" "$INSTALL_DIR/aden-mcp"
ok "aden      → $INSTALL_DIR/aden       (the CLI)"
ok "aden-mcp  → $INSTALL_DIR/aden-mcp   (the MCP server your AI tools spawn)"

# 4 ─ PATH ────────────────────────────────────────────────────────────────────
step "PATH"
if in_path; then
    ok "$INSTALL_DIR is already in your PATH — nothing to do."
else
    note "$INSTALL_DIR is NOT in your PATH. To fix it, this exact line:"
    note "    $PATH_LINE"
    note "would be appended to: $PROFILE"
    if [ "$TTY" = "0" ]; then
        skip "non-interactive: NOT editing $PROFILE — add the line above yourself."
    elif ask_yn "Append it now?" y; then
        printf '%s\n' "$PATH_LINE" >>"$PROFILE"
        ok "added — run 'source $PROFILE' (or open a new shell) to activate"
    else
        skip "skipped — add the line above to your shell profile when ready."
    fi
fi

# 5 ─ MCP registration ────────────────────────────────────────────────────────
if [ "$MINIMAL" = "0" ]; then
    step "Register the MCP server with your AI tools"
    note "MCP is how Claude Code / opencode / Cursor / Zed etc. call aden directly"
    note "(ask, grep, understand, impact … as native tools). Registration only"
    note "writes the aden entry into each platform's own MCP config file — shown"
    note "below — and 'aden mcp uninstall' removes exactly that entry."
    echo ""
    "$INSTALL_DIR/aden" mcp list 2>/dev/null || note "(could not query platforms)"
    echo ""
    note "Default: register for the DETECTED platforms only (✓ column above)."
    if [ "$TTY" = "0" ]; then
        skip "non-interactive: run 'aden mcp install' yourself to register."
    elif ask_yn "Register aden with the detected platforms?" y; then
        "$INSTALL_DIR/aden" mcp install ||
            note "(some platforms failed — 'aden mcp list' shows the current state)"
        note "Restart any open editor/agent sessions to pick up the new server."
    else
        skip "skipped — 'aden mcp install [--platform <name>]' does this any time."
    fi
fi

# 6 ─ Dense model (only when --dense) ─────────────────────────────────────────
if [ "$DENSE" = "1" ]; then
    step "Dense-search model"
    note "Hybrid search needs the bge-small embedding model (one-time download)"
    note "fetched to ~/.cache/aden-models/ — outside every project."
    if [ "$TTY" = "0" ]; then
        skip "non-interactive: run scripts/fetch-bge-model.sh to fetch it."
    elif ask_yn "Fetch it now?" y; then
        "$PROJECT_ROOT/scripts/fetch-bge-model.sh" || note "(fetch failed — rerun scripts/fetch-bge-model.sh)"
    else
        skip "skipped — scripts/fetch-bge-model.sh fetches it any time."
    fi
fi

# 7 ─ AGENTS.md guidance ──────────────────────────────────────────────────────
if [ "$MINIMAL" = "0" ] && [ "$TTY" = "1" ]; then
    step "Seed AGENTS.md guidance in a project (optional)"
    note "Adds a clearly-marked, append-only block telling AI agents to use aden's"
    note "graph tools instead of raw grep in that repo (ADR-004). Idempotent: it"
    note "only ever rewrites its own block; delete the block to opt out."
    DEFAULT_TARGET="$INVOKE_DIR"
    # Don't default to aden's own checkout — that AGENTS.md is hand-maintained.
    [ "$INVOKE_DIR" = "$PROJECT_ROOT" ] && DEFAULT_TARGET=""
    if [ "$YES" = "1" ]; then
        # --yes must not guess a repo to edit; this one stays explicitly manual.
        skip "--yes: skipped (run 'aden agents-md <project>' for each repo you want)"
    else
        if [ -n "$DEFAULT_TARGET" ]; then
            printf "   Project path [%s], or 'n' to skip: " "$DEFAULT_TARGET"
        else
            printf "   Project path (blank to skip): "
        fi
        read -r REPLY_TARGET || REPLY_TARGET=""
        TARGET=""
        case "$REPLY_TARGET" in
            "") TARGET="$DEFAULT_TARGET" ;;
            n | N) TARGET="" ;;
            *) TARGET="$REPLY_TARGET" ;;
        esac
        if [ -n "$TARGET" ]; then
            if [ -d "$TARGET" ]; then
                "$INSTALL_DIR/aden" agents-md "$TARGET" || echo "   (skipped: aden agents-md failed)"
            else
                echo "   (skipped: '$TARGET' is not a directory)"
            fi
        fi
    fi
fi

# 8 ─ Verify + summary ────────────────────────────────────────────────────────
step "Verify"
if VERSION_OUT="$("$INSTALL_DIR/aden" --version 2>&1)"; then
    ok "$VERSION_OUT runs from $INSTALL_DIR"
else
    note "✗ '$INSTALL_DIR/aden --version' failed — something is wrong: $VERSION_OUT"
fi

echo ""
echo "═══ Installed — and how to undo each piece ═══"
echo "  aden, aden-mcp → $INSTALL_DIR/            undo: ./install.sh --uninstall"
if ! in_path; then
    echo "  PATH           → see step above            undo: remove the line from $PROFILE"
fi
echo "  MCP entries    → each platform's config    undo: aden mcp uninstall"
echo "  graph stores   → ~/.local/share/aden/      (created on first 'gen'; safe to delete, rebuilt from source)"
echo ""
echo "First steps in any repo:"
echo "  aden gen .          index it (or just run a query — reads auto-index)"
echo "  aden ask \"how does X work\""
echo "  aden view           explore the graph in your browser"
echo "  aden doctor         check the environment end-to-end"
