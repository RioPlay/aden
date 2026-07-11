# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

[CmdletBinding()]
param(
    [string]$InstallDir = $(if ($env:ADEN_INSTALL_DIR) { $env:ADEN_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Aden\bin" }),
    [switch]$Force,
    [switch]$Uninstall
)
$ErrorActionPreference = "Stop"
$BundleDir = $PSScriptRoot
$Names = @("aden.exe", "aden-mcp.exe")

if ($Uninstall) {
    foreach ($Name in $Names) { Remove-Item -LiteralPath (Join-Path $InstallDir $Name) -Force -ErrorAction SilentlyContinue }
    Write-Host "Removed Aden binaries from $InstallDir"
    Write-Host "User data was preserved."
    exit 0
}

foreach ($Name in $Names) {
    $Source = Join-Path $BundleDir $Name
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) { throw "Missing bundled binary: $Source" }
}

$Sums = Join-Path $BundleDir "SHA256SUMS"
if (-not (Test-Path -LiteralPath $Sums -PathType Leaf)) { throw "Missing SHA256SUMS; refusing unverified install." }
$Expected = @{}
foreach ($Line in @(Get-Content -LiteralPath $Sums)) {
    if ($Line -notmatch '^([0-9a-fA-F]{64})  (aden(?:-mcp)?\.exe)$') {
        throw "Malformed or unexpected SHA256SUMS entry."
    }
    $FileName = $Matches[2]
    if ($Expected.ContainsKey($FileName)) { throw "Duplicate SHA256SUMS entry for $FileName." }
    $Expected[$FileName] = $Matches[1]
}
foreach ($Name in $Names) {
    if (-not $Expected.ContainsKey($Name)) { throw "Missing SHA256SUMS entry for $Name." }
    $Actual = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $BundleDir $Name)).Hash
    if ($Actual -ne $Expected[$Name]) { throw "SHA-256 verification failed for $Name." }
}
if ($Expected.Count -ne 2) { throw "SHA256SUMS must contain exactly two entries." }
Write-Host "Bundle checksums verified."

New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
foreach ($Name in $Names) {
    $Destination = Join-Path $InstallDir $Name
    if ((Test-Path -LiteralPath $Destination) -and -not $Force) {
        throw "$Destination already exists; rerun with -Force to replace both binaries."
    }
}
$Stage = Join-Path $InstallDir (".aden-install-" + [Guid]::NewGuid().ToString("N"))
$CommitStarted = $false
$Previous = @{}
New-Item -ItemType Directory -Path $Stage | Out-Null
try {
    foreach ($Name in $Names) {
        $Staged = Join-Path $Stage $Name
        Copy-Item -LiteralPath (Join-Path $BundleDir $Name) -Destination $Staged
    }
    # Both staged binaries must be runnable before either destination changes.
    & (Join-Path $Stage "aden.exe") --version
    if ($LASTEXITCODE -ne 0) { throw "Staged aden.exe smoke check failed." }
    & (Join-Path $Stage "aden-mcp.exe") --version
    if ($LASTEXITCODE -ne 0) { throw "Staged aden-mcp.exe smoke check failed." }

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
    & (Join-Path $InstallDir "aden.exe") --version
    if ($LASTEXITCODE -ne 0) { throw "Installed aden.exe smoke check failed." }
    & (Join-Path $InstallDir "aden-mcp.exe") --version
    if ($LASTEXITCODE -ne 0) { throw "Installed aden-mcp.exe smoke check failed." }
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
Write-Host "Installed aden and aden-mcp in $InstallDir"
Write-Host "Add that directory to your user PATH if needed."
Write-Host "Uninstall: .\install.ps1 -InstallDir `"$InstallDir`" -Uninstall"
