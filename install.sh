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
#   ./install.sh --surface=LVL bake the MCP tool surface into the registration:
#                              essential (default, 6 tools) | standard (15) | full (36)
#   ./install.sh --uninstall   guided removal
#
# Environment overrides: INSTALL_DIR or ADEN_INSTALL_DIR (default ~/.local/bin),
# PROJECT_ROOT (default: this checkout), ADEN_DENSE=1 (same as --dense),
# ADEN_MCP_SURFACE (same as --surface=).
#
# Non-interactive runs (no TTY, e.g. piped from curl or CI) perform only the
# self-contained steps — build, copy binaries — and PRINT instructions for the
# steps that would edit your files, instead of editing them unprompted.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INVOKE_DIR="$(pwd)" # captured before any cd — used as the AGENTS.md default
INSTALL_DIR="${INSTALL_DIR:-${ADEN_INSTALL_DIR:-$HOME/.local/bin}}"
PROJECT_ROOT="${PROJECT_ROOT:-$SCRIPT_DIR}"

YES=0
MINIMAL=0
DENSE="${ADEN_DENSE:-0}"
# Tool surface baked into the MCP registration: essential (default) | standard |
# full. Empty means "ask (interactive) or use the server default essential".
SURFACE="${ADEN_MCP_SURFACE:-}"
UNINSTALL=0
for arg in "$@"; do
    case "$arg" in
        -y | --yes) YES=1 ;;
        --minimal) MINIMAL=1 ;;
        --dense) DENSE=1 ;;
        --surface=*) SURFACE="${arg#*=}" ;;
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

# Resolve $SURFACE (the MCP tool surface). Honors a preset value (--surface=… or
# $ADEN_MCP_SURFACE); otherwise prompts interactively, defaulting to essential.
choose_surface() {
    case "$SURFACE" in
        essential | standard | full) return 0 ;;
        "") : ;;
        *)
            note "Ignoring unknown surface '$SURFACE' (expected essential|standard|full)."
            SURFACE=""
            ;;
    esac
    if [ "$YES" = "1" ] || [ "$TTY" = "0" ]; then
        SURFACE="essential"
        return 0
    fi
    note "Tool surface — how many aden tools your AI tools see by default:"
    note "  essential   6 tools  find -> comprehend -> blast-radius   [default]"
    note "  standard   15 tools  + impact-diff, list, test, lint, audit, diagnose, …"
    note "  full       36 tools  + build / setup / admin tooling"
    note "(hidden tools also require explicit surface opt-in; busy servers fail fast instead of queueing.)"
    printf "   Surface [essential/standard/full]: "
    read -r reply || reply=""
    case "$(printf '%s' "$reply" | tr '[:upper:]' '[:lower:]')" in
        standard | 2) SURFACE="standard" ;;
        full | 3) SURFACE="full" ;;
        *) SURFACE="essential" ;;
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
    PATH_LINE="set -gx PATH \"$INSTALL_DIR\" \$PATH"
else
    PATH_LINE="export PATH=\"$INSTALL_DIR:\$PATH\""
fi

in_path() { echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; }
active_aden() { command -v aden 2>/dev/null || true; }
installed_aden_is_active() { [ "$(active_aden)" = "$INSTALL_DIR/aden" ]; }
install_hint() {
    local active
    active="$(active_aden)"
    note "'$INSTALL_DIR/aden' was installed, but PATH resolves '$active' first."
    note "To replace the active user-local installation, rerun with:"
    note "    INSTALL_DIR=$(dirname "$active") ./install.sh --yes"
    note "Or place $INSTALL_DIR before $(dirname "$active") in your shell PATH."
}

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
# Hooks change repository behavior, so they are always explicit opt-in — even
# for `--yes` and non-interactive installs. The installer itself must never
# surprise a newcomer by changing how their next commit or push behaves.
if git -C "$PROJECT_ROOT" rev-parse --git-dir >/dev/null 2>&1; then
    note "Optional: activate this checkout's repo-local pre-push CI hook."
    if ask_yn "Activate git hooks for this checkout?" n; then
        git -C "$PROJECT_ROOT" config core.hooksPath git-hooks
        ok "git hooks activated for this checkout (pre-push CI gate; repo-local setting)"
    else
        skip "git hooks unchanged — opt in later: git config core.hooksPath git-hooks"
    fi
    if [ "$TTY" = "1" ] && [ -f "$PROJECT_ROOT/tools/git-hooks/pre-commit" ]; then
        note "Optional: install the pre-commit hook (secret scan + aden check + test)."
        note "Runs on every commit — heavier than pre-push but catches issues earlier."
        if ask_yn "Install pre-commit hook into .git/hooks/?" n; then
            mkdir -p "$PROJECT_ROOT/.git/hooks"
            cp "$PROJECT_ROOT/tools/git-hooks/pre-commit" "$PROJECT_ROOT/.git/hooks/pre-commit"
            chmod +x "$PROJECT_ROOT/.git/hooks/pre-commit"
            ok "pre-commit hook installed (also: make install-hooks)"
        else
            skip "skipped — run 'make install-hooks' any time."
        fi
    fi
fi
if [ -z "${ADEN_BUILD_REVISION:-}" ]; then
    ADEN_BUILD_REVISION="$(git --git-dir="$PROJECT_ROOT/.git" rev-parse --short=12 HEAD 2>/dev/null || printf unknown)"
fi
if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
    ADEN_BUILD_STATE="reproducible"
elif git --git-dir="$PROJECT_ROOT/.git" --work-tree="$PROJECT_ROOT" status --porcelain --untracked-files=no 2>/dev/null | grep -q .; then
    ADEN_BUILD_STATE="dirty"
else
    ADEN_BUILD_STATE="clean"
fi
export ADEN_BUILD_REVISION ADEN_BUILD_STATE
note "Build identity: $ADEN_BUILD_REVISION ($ADEN_BUILD_STATE); no timestamp embedded."
if [ "$DENSE" = "1" ]; then
    note "Building WITH dense/hybrid search: adds the tract + bge embedding stack."
    note "The MCP server spawns this same binary, so hybrid search turns on for both."
    cargo build --locked --release -p aden-cli --features dense 2>&1 | tail -3
    cargo build --locked --release -p aden-mcp 2>&1 | tail -3
else
    note "Building the standard binaries (lexical search; --dense adds hybrid)."
    cargo build --locked --release -p aden-cli -p aden-mcp 2>&1 | tail -3
fi

TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
case "$TARGET_DIR" in
    /*) ;;
    *) TARGET_DIR="$PROJECT_ROOT/$TARGET_DIR" ;;
esac
RELEASE_DIR="$TARGET_DIR/release"
[ -x "$RELEASE_DIR/aden" ] || { note "✗ built aden not found at $RELEASE_DIR/aden"; exit 1; }
[ -x "$RELEASE_DIR/aden-mcp" ] || { note "✗ built aden-mcp not found at $RELEASE_DIR/aden-mcp"; exit 1; }
ok "built $RELEASE_DIR/{aden, aden-mcp}"

# 3 ─ Copy binaries ───────────────────────────────────────────────────────────
step "Install binaries → $INSTALL_DIR"
note "Why here: user-local bin dir — no sudo, no system files, trivially undoable."
mkdir -p "$INSTALL_DIR"
STAGE="$(mktemp -d "$INSTALL_DIR/.aden-install.XXXXXX")"
COMMIT_STARTED=0
SUCCESS=0
HAD_ADEN=0
HAD_MCP=0
rollback_install() {
    status=$?
    if [ "$SUCCESS" != "1" ] && [ "$COMMIT_STARTED" = "1" ]; then
        rm -f "$INSTALL_DIR/aden" "$INSTALL_DIR/aden-mcp"
        [ "$HAD_ADEN" = "1" ] && mv -f "$STAGE/backup-aden" "$INSTALL_DIR/aden"
        [ "$HAD_MCP" = "1" ] && mv -f "$STAGE/backup-aden-mcp" "$INSTALL_DIR/aden-mcp"
        note "Install failed; restored the previous Aden binary pair."
    fi
    rm -rf "$STAGE"
    return "$status"
}
trap rollback_install EXIT

# Stage and smoke the complete pair before changing either live destination.
# Moving the staged files into place also replaces running binaries with fresh
# inodes; existing processes keep the old inode until they restart.
install -m 755 "$RELEASE_DIR/aden" "$STAGE/aden"
install -m 755 "$RELEASE_DIR/aden-mcp" "$STAGE/aden-mcp"
"$STAGE/aden" --version >/dev/null
"$STAGE/aden-mcp" --version >/dev/null
COMMIT_STARTED=1
if [ -e "$INSTALL_DIR/aden" ]; then mv "$INSTALL_DIR/aden" "$STAGE/backup-aden"; HAD_ADEN=1; fi
if [ -e "$INSTALL_DIR/aden-mcp" ]; then mv "$INSTALL_DIR/aden-mcp" "$STAGE/backup-aden-mcp"; HAD_MCP=1; fi
mv "$STAGE/aden" "$INSTALL_DIR/aden"
mv "$STAGE/aden-mcp" "$INSTALL_DIR/aden-mcp"
"$INSTALL_DIR/aden" --version >/dev/null
"$INSTALL_DIR/aden-mcp" --version >/dev/null
SUCCESS=1
rm -rf "$STAGE"
trap - EXIT
ok "aden      → $INSTALL_DIR/aden       (the CLI)"
ok "aden-mcp  → $INSTALL_DIR/aden-mcp   (the MCP server your AI tools spawn)"
if [ -n "$EXISTING" ]; then
    note "IMPORTANT: running MCP/editor/agent processes still hold the previous binary."
    note "Restart those sessions after install so they load this updated aden-mcp."
fi

# 4 ─ PATH ────────────────────────────────────────────────────────────────────
step "PATH"
if ! in_path; then
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
elif installed_aden_is_active; then
    ok "$INSTALL_DIR is first for aden in your PATH — nothing to do."
else
    install_hint
    if [ "$TTY" = "0" ]; then
        skip "non-interactive: NOT editing $PROFILE — use one of the remedies above."
    elif ask_yn "Put $INSTALL_DIR before the existing aden directory in $PROFILE?" y; then
        printf '%s\n' "$PATH_LINE" >>"$PROFILE"
        ok "added — open a new shell so aden resolves to $INSTALL_DIR/aden"
    else
        skip "PATH unchanged — this shell will continue using $(active_aden)."
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
        if [ -n "$SURFACE" ]; then
            skip "non-interactive: run 'aden mcp install --surface $SURFACE' to register."
        else
            skip "non-interactive: run 'aden mcp install [--surface standard|full]' to register."
        fi
    elif ask_yn "Register aden with the detected platforms?" y; then
        choose_surface
        "$INSTALL_DIR/aden" mcp install --surface "$SURFACE" ||
            note "(some platforms failed — 'aden mcp list' shows the current state)"
        note "Surface: $SURFACE — re-run 'aden mcp install --surface <level>' to change it."
        note "Restart any open editor/agent sessions to pick up the new server."
    else
        skip "skipped — 'aden mcp install [--surface <level>]' does this any time."
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
    case "$VERSION_OUT" in
        *"Build:"*"Formats:"*) ok "installed binary reports build + format identity" ;;
        *) note "✗ installed binary did not report build/format identity" ;;
    esac
    if installed_aden_is_active; then
        ok "$VERSION_OUT runs from $INSTALL_DIR"
    else
        note "✓ $VERSION_OUT is installed at $INSTALL_DIR"
        install_hint
    fi
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
echo "  graph stores   → ~/.local/share/aden/      (created by the first read; safe to delete)"
if [ -n "$EXISTING" ]; then
    echo "  restart        → open MCP/editor/agent sessions (they keep the old process until restart)"
fi
echo ""
echo "First steps in any repo (no init or project files required):"
echo "  aden tree --human --symbols .   compact symbol + line-range map"
echo "  aden grep \"known_symbol\"       structure-aware evidence"
echo "  aden mcp install --platform <client>   expose the focused tools to an LLM"
