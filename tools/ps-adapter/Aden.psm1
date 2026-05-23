function Invoke-AdenGenerate {
    <#
    .SYNOPSIS
        Generates an output using the Aden CLI.

    .DESCRIPTION
        Wraps the `aden gen` command, generating artefacts from the specified path.

    .PARAMETER Path
        The path to the source or template file/directory.

    .PARAMETER OutDir
        Optional output directory for generated files.

    .EXAMPLE
        Invoke-AdenGenerate -Path ./templates/main.md -OutDir ./output
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,

        [Parameter(Mandatory = $false)]
        [string]$OutDir
    )

    $arguments = @('gen', $Path)

    if ($PSBoundParameters.ContainsKey('OutDir')) {
        $arguments += '--out-dir'
        $arguments += $OutDir
    }

    & aden @arguments
}

function Invoke-AdenAssemble {
    <#
    .SYNOPSIS
        Assembles components using the Aden CLI.

    .DESCRIPTION
        Wraps the `aden asm` command, assembling from a source path.

    .PARAMETER From
        The source path to assemble from.

    .PARAMETER Path
        The target path for the assembled output.

    .PARAMETER Depth
        Optional recursion depth.

    .PARAMETER Budget
        Optional budget limit.

    .EXAMPLE
        Invoke-AdenAssemble -From ./src -Path ./dist -Depth 3 -Budget 500
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$From,

        [Parameter(Mandatory = $true)]
        [string]$Path,

        [Parameter(Mandatory = $false)]
        [int]$Depth,

        [Parameter(Mandatory = $false)]
        [int]$Budget
    )

    $arguments = @('asm', '--from', $From, '--path', $Path)

    if ($PSBoundParameters.ContainsKey('Depth')) {
        $arguments += '--depth'
        $arguments += $Depth
    }

    if ($PSBoundParameters.ContainsKey('Budget')) {
        $arguments += '--budget'
        $arguments += $Budget
    }

    & aden @arguments
}

function Test-AdenIntegrity {
    <#
    .SYNOPSIS
        Tests the integrity of a path using the Aden CLI.

    .DESCRIPTION
        Wraps the `aden check` command. Returns $true if the check passes
        (exit code 0), otherwise returns $false and captures ERROR output.

    .PARAMETER Path
        The path to validate.

    .EXAMPLE
        $valid = Test-AdenIntegrity -Path ./src
        if (-not $valid) { Write-Error "Integrity check failed." }
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $tmpErr = [System.IO.Path]::GetTempFileName()

    try {
        $process = Start-Process -FilePath "aden" -ArgumentList "check", $Path `
            -Wait -PassThru -RedirectStandardError $tmpErr -WindowStyle Hidden

        if ($process.ExitCode -ne 0) {
            $errorOutput = Get-Content -Path $tmpErr -Raw -ErrorAction SilentlyContinue
            if ($errorOutput) {
                Write-Error "Aden check failed with the following error:`n$errorOutput"
            }
            return $false
        }

        return $true
    }
    finally {
        Remove-Item -Path $tmpErr -Force -ErrorAction SilentlyContinue
    }
}
