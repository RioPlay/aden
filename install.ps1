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
    [string]$InstallDir = $(
        if ($env:ADEN_INSTALL_DIR) { $env:ADEN_INSTALL_DIR }
        elseif (($env:OS -eq 'Windows_NT') -and $env:LOCALAPPDATA) { Join-Path $env:LOCALAPPDATA "Aden\bin" }
        else { Join-Path $HOME ".local/bin" }
    ),
    [switch]$Force,
    [switch]$Uninstall
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

# Windows PowerShell 5.1 does not expose PowerShell Core's $IsWindows
# automatic variable. OS is present on every supported Windows host and keeps
# this source-build installer runnable under both 5.1 and newer PowerShell.
$ExeExt = if ($env:OS -eq 'Windows_NT') { ".exe" } else { "" }
$Names = @("aden$ExeExt", "aden-mcp$ExeExt")

if ($Uninstall) {
    foreach ($Name in $Names) {
        Remove-Item -LiteralPath (Join-Path $InstallDir $Name) -Force -ErrorAction SilentlyContinue
    }
    Write-Host "Removed Aden binaries from $InstallDir"
    Write-Host "User data and MCP configuration were preserved."
    exit 0
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
    # Windows PowerShell 5.1 wraps a native program's stderr as error records.
    # Cargo writes ordinary progress there, so ErrorActionPreference=Stop would
    # abort on the first "Compiling" line even while the build is succeeding.
    $SavedErrorActionPreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        cargo build --locked --release -p aden-cli -p aden-mcp 2>&1 | ForEach-Object { Write-Host $_ }
        $BuildExitCode = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $SavedErrorActionPreference
    }
    if ($BuildExitCode -ne 0) { Die "Build failed" }
}
finally {
    Pop-Location
}

# Resolve Cargo's target directory instead of assuming the repository-local
# default. This honors CARGO_TARGET_DIR and paths containing spaces.
$ManifestPath = Join-Path $PROJECT_ROOT "Cargo.toml"
$Metadata = cargo metadata --locked --manifest-path $ManifestPath --format-version 1 --no-deps | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { Die "Could not resolve Cargo target directory" }
$BinaryDir = Join-Path $Metadata.target_directory "release"
$Sources = @{}
foreach ($Name in $Names) {
    $Source = Join-Path $BinaryDir $Name
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        Die "Build failed: binary not found at $Source"
    }
    $Sources[$Name] = $Source
}

New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
foreach ($Name in $Names) {
    $Destination = Join-Path $InstallDir $Name
    if ((Test-Path -LiteralPath $Destination) -and -not $Force) {
        Die "$Destination already exists; rerun with -Force to replace both binaries."
    }
}

# Stage and smoke both binaries before changing either live destination. If any
# later move or smoke check fails, restore the previous pair.
$Stage = Join-Path $InstallDir (".aden-install-" + [Guid]::NewGuid().ToString("N"))
$CommitStarted = $false
$Previous = @{}
New-Item -ItemType Directory -Path $Stage | Out-Null
try {
    foreach ($Name in $Names) {
        Copy-Item -LiteralPath $Sources[$Name] -Destination (Join-Path $Stage $Name)
    }
    foreach ($Name in $Names) {
        & (Join-Path $Stage $Name) --version
        if ($LASTEXITCODE -ne 0) { throw "Staged $Name smoke check failed." }
    }

    $CommitStarted = $true
    foreach ($Name in $Names) {
        $Destination = Join-Path $InstallDir $Name
        if (Test-Path -LiteralPath $Destination) {
            $Backup = Join-Path $Stage ("backup-" + $Name)
            Move-Item -LiteralPath $Destination -Destination $Backup
            $Previous[$Name] = $Backup
        }
    }
    foreach ($Name in $Names) {
        Move-Item -LiteralPath (Join-Path $Stage $Name) -Destination (Join-Path $InstallDir $Name)
    }
    foreach ($Name in $Names) {
        & (Join-Path $InstallDir $Name) --version
        if ($LASTEXITCODE -ne 0) { throw "Installed $Name smoke check failed." }
    }
} catch {
    if ($CommitStarted) {
        foreach ($Name in $Names) {
            Remove-Item -LiteralPath (Join-Path $InstallDir $Name) -Force -ErrorAction SilentlyContinue
            if ($Previous.ContainsKey($Name)) {
                Move-Item -LiteralPath $Previous[$Name] -Destination (Join-Path $InstallDir $Name) -Force
            }
        }
        Write-Error "Install failed; restored the previous Aden binary pair. $($_.Exception.Message)"
    }
    throw
} finally {
    Remove-Item -LiteralPath $Stage -Recurse -Force -ErrorAction SilentlyContinue
}

$Version = & (Join-Path $InstallDir "aden$ExeExt") --version 2>$null
Banner -Text "Installed"
Write-Host "  aden     -> $(Join-Path $InstallDir "aden$ExeExt")"
Write-Host "  aden-mcp -> $(Join-Path $InstallDir "aden-mcp$ExeExt")"
Write-Host "  Version: $Version"
Write-Host ""
$PathEntries = @($env:PATH -split [IO.Path]::PathSeparator)
if ($PathEntries -notcontains $InstallDir) {
    Write-Host "Add $InstallDir to your PATH to invoke Aden from any directory."
}
Write-Host "Uninstall: .\install.ps1 -InstallDir `"$InstallDir`" -Uninstall"
Write-Host ""
Write-Host "Next steps (no init or project files required):"
Write-Host "  1. Run: aden tree --human --symbols ."
Write-Host "  2. Run: aden grep known_symbol"
