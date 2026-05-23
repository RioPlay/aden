# Aden Installer — PowerShell edition
# Supports: Windows PowerShell 5.1+, PowerShell Core 7+, Windows Terminal
# Usage:
#   Invoke-RestMethod -Uri https://rioplay.dev/install.ps1 | Invoke-Expression
#   irm https://rioplay.dev/install.ps1 | iex

param(
    [string]$InstallDir = "$env:USERPROFILE\.local\bin",
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$Repo = "RioPlay/aden"
$ApiUrl = "https://api.github.com/repos/$Repo/releases/latest"

function Banner {
    param([string]$Text)
    Write-Host "`n=== $Text ===" -ForegroundColor Cyan
}

function Die {
    param([string]$Message)
    Write-Host "Error: $Message" -ForegroundColor Red
    exit 1
}

# ── Detect OS ────────────────────────────────────
$Platform = switch ($env:OS) {
    "Windows_NT" { "pc-windows-msvc" }
    default {
        $uname = (uname -s).ToLower()
        switch ($uname) {
            "linux"   { "unknown-linux-gnu" }
            "darwin"  { "apple-darwin" }
            default   { $null }
        }
    }
}

if (-not $Platform) {
    Die "Unsupported OS. Install from source: cargo install --git https://github.com/$Repo"
}

# ── Detect Arch ──────────────────────────────────
$Arch = switch ([System.Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture) {
    ([System.Runtime.InteropServices.Architecture]::X64)     { "x86_64" }
    ([System.Runtime.InteropServices.Architecture]::Arm64)    { "aarch64" }
    ([System.Runtime.InteropServices.Architecture]::X86)     { "i686" }
    ([System.Runtime.InteropServices.Architecture]::Arm)     { "armv7" }
    default {
        # Fallback for older PowerShell
        if ($env:PROCESSOR_ARCHITECTURE -match "64") { "x86_64" }
        elseif ($env:PROCESSOR_ARCHITECTURE -match "86") { "i686" }
        else { $null }
    }
}

if (-not $Arch) {
    Die "Unsupported architecture. Install from source: cargo install --git https://github.com/$Repo"
}

# ── Asset Name ────────────────────────────────────
$Binary = if ($Platform -eq "pc-windows-msvc") { "aden.exe" } else { "aden" }
$Asset = "aden-${Arch}-${Platform}"
$ZipExt = if ($Platform -eq "pc-windows-msvc") { "zip" } else { "tar.gz" }

$DownloadUrl = "https://github.com/$Repo/releases/latest/download/${Asset}.${ZipExt}"

# ── Already Installed? ────────────────────────────
if ((Get-Command aden -ErrorAction SilentlyContinue) -and -not $Force) {
    $Current = & aden --version 2>$null
    Write-Host "Aden is already installed: $Current"
    Write-Host "To force reinstall, run with -Force."
    exit 0
}

# ── Download ──────────────────────────────────────
Banner -Text "Installing Aden"
Write-Host "  Repository : $Repo"
Write-Host "  Asset      : ${Asset}.${ZipExt}"
Write-Host "  Target     : $env:OS / $Arch"
Write-Host "  Install to : $InstallDir"

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

$TmpDir = Join-Path $env:TEMP ([System.Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $TmpDir | Out-Null

try {
    $ArchivePath = Join-Path $TmpDir "archive.${ZipExt}"

    # Prefer modern download methods
    if ($PSVersionTable.PSVersion.Major -ge 7) {
        Invoke-RestMethod -Uri $DownloadUrl -OutFile $ArchivePath
    }
    elseif (Get-Command Invoke-WebRequest -ErrorAction SilentlyContinue) {
        Invoke-WebRequest -Uri $DownloadUrl -OutFile $ArchivePath -UseBasicParsing
    }
    else {
        # Legacy fallback for very old Windows
        $WebClient = New-Object System.Net.WebClient
        $WebClient.DownloadFile($DownloadUrl, $ArchivePath)
    }

    # ── Extract ──────────────────────────────────
    if ($ZipExt -eq "zip") {
        if (Get-Command Expand-Archive -ErrorAction SilentlyContinue) {
            Expand-Archive -Path $ArchivePath -DestinationPath $TmpDir -Force
        } else {
            Die "Expand-Archive cmdlet not available. Please update PowerShell."
        }
    } else {
        if (Get-Command tar -ErrorAction SilentlyContinue) {
            & tar -xzf $ArchivePath -C $TmpDir
            if ($LASTEXITCODE -ne 0) { Die "tar extraction failed." }
        } else {
            Die "tar is required for extraction. Install Git for Windows or use WSL."
        }
    }

    # Find binary
    $Found = Get-ChildItem -Path $TmpDir -Recurse -Filter $Binary | Select-Object -First 1
    if (-not $Found) {
        Die "Could not find '$Binary' in downloaded archive."
    }

    Copy-Item -Path $Found.FullName -Destination (Join-Path $InstallDir $Binary) -Force

    # On Unix, ensure execute bit
    if ($Platform -ne "pc-windows-msvc") {
        chmod +x (Join-Path $InstallDir $Binary)
    }

} finally {
    Remove-Item -Recurse -Force -Path $TmpDir -ErrorAction SilentlyContinue
}

# ── Verify ───────────────────────────────────────
$InstalledPath = Join-Path $InstallDir $Binary
if (Test-Path $InstalledPath) {
    $Version = & $InstalledPath --version 2>$null
    if (-not $Version) { $Version = "unknown" }

    Banner -Text "Success"
    Write-Host "Aden installed to: $InstalledPath"
    Write-Host "Version: $Version"
    Write-Host ""
    Write-Host "Next steps:"
    Write-Host "  1. Ensure $InstallDir is in your PATH"
    Write-Host "  2. Run: aden init       # in any project you want to index"
    Write-Host "  3. Run: aden doctor .   # verify your environment"
    Write-Host ""
    Write-Host "For docs: https://github.com/$Repo"
} else {
    Die "Installation failed: binary not found at $InstalledPath"
}
