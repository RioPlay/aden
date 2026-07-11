# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

# Aden Installer — PowerShell edition
# Supports: Windows PowerShell 5.1+, PowerShell Core 7+
# Builds from source and installs to user-local directory.
#
# SECURITY: This script requires execution policy changes to run.
# RECOMMENDED: Use -ExecutionPolicy Bypass just for this script, not globally:
#   powershell -ExecutionPolicy Bypass -File .\install.ps1
#
# For ongoing security, set your default policy to RemoteSigned or AllSigned:
#   Set-ExecutionPolicy -Scope CurrentUser -ExecutionPolicy RemoteSigned
# This allows locally-created scripts to run while requiring downloaded
# scripts to be signed by a trusted publisher.

param(
    [string]$InstallDir = "$env:USERPROFILE\.local\bin",
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

$SCRIPT_DIR = Split-Path -Parent $MyInvocation.MyCommand.Path
$PROJECT_ROOT = $SCRIPT_DIR

function Banner {
    param([string]$Text)
    Write-Host "`n=== $Text ===" -ForegroundColor Cyan
}

function Die {
    param([string]$Message)
    Write-Host "Error: $Message" -ForegroundColor Red
    exit 1
}

# Check for Rust
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Die "Rust not found. Install from https://rustup.rs"
}

# Build release binaries
Banner -Text "Building Aden"
Write-Host "  Project: $PROJECT_ROOT"
Write-Host "  Install to: $InstallDir"

Push-Location $PROJECT_ROOT
try {
    cargo build --release -p aden-cli -p aden-mcp 2>&1 | ForEach-Object { Write-Host $_ }
    if ($LASTEXITCODE -ne 0) { Die "Build failed" }
}
finally {
    Pop-Location
}

# Ensure install directory exists
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
}

# Windows binaries carry a .exe extension; PowerShell Core on Unix does not.
$ExeExt = if ($IsWindows -or $env:OS -eq 'Windows_NT') { ".exe" } else { "" }

# Copy binaries
$AdenSrc = Join-Path $PROJECT_ROOT "target/release/aden$ExeExt"
$McpSrc = Join-Path $PROJECT_ROOT "target/release/aden-mcp$ExeExt"

if (-not (Test-Path $AdenSrc)) {
    Die "Build failed: aden binary not found"
}

Copy-Item -Path $AdenSrc -Destination (Join-Path $InstallDir "aden$ExeExt") -Force
if (Test-Path $McpSrc) {
    Copy-Item -Path $McpSrc -Destination (Join-Path $InstallDir "aden-mcp$ExeExt") -Force
}

# Verify
$InstalledPath = Join-Path $InstallDir "aden$ExeExt"
if (Test-Path $InstalledPath) {
    $Version = & $InstalledPath --version 2>$null
    if (-not $Version) { $Version = "unknown" }

    Banner -Text "Installed"
    Write-Host "  aden     -> $InstallDir\aden$ExeExt"
    Write-Host "  Version: $Version"
    Write-Host ""
    Write-Host "Next steps:"
    Write-Host "  1. Add $InstallDir to your PATH"
    Write-Host "  2. Run: aden init       # in any project"
    Write-Host "  3. Run: aden doctor .   # verify environment"
} else {
    Die "Installation failed"
}
