<!--
Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Aden prebuilt release bundle

This archive is self-contained: it installs the adjacent prebuilt `aden` and
`aden-mcp` executables and does not require Rust, Cargo, or the Aden source tree.

## Install

On Linux or macOS:

```sh
./install.sh
# custom location or replacement of an existing install:
./install.sh --install-dir "$HOME/bin" --force
```

On Windows PowerShell:

```powershell
.\install.ps1
.\install.ps1 -InstallDir "$HOME\bin" -Force
```

The defaults are `~/.local/bin` on Unix and `%LOCALAPPDATA%\Aden\bin` on
Windows. The installers do not edit `PATH`, shell profiles, MCP configuration,
or project files. Run `aden mcp install` afterward if desired.

Both installers verify `SHA256SUMS` before copying. The Unix installer refuses
to install if the manifest is missing or neither `sha256sum` nor `shasum` is
available. The Windows installer uses PowerShell's built-in `Get-FileHash` and
refuses malformed, incomplete, duplicate, or unexpected manifest entries. To
verify the downloaded archive itself, compare it with the adjacent `.sha256`
file on the GitHub release page.

Existing binaries are never overwritten unless `--force` / `-Force` is given.
Uninstall with `./install.sh --uninstall` or `.\install.ps1 -Uninstall`, passing
the same custom install directory if one was used. Uninstall preserves graph
stores and model caches.
