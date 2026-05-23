@{
    RootModule           = 'Aden.psm1'
    ModuleVersion        = '0.1.0'
    GUID                 = '616210fb-5e4a-4328-9f2e-ed6c020112ba'
    Author               = 'Aden Architect'
    Description          = 'PowerShell adapter module for the Aden Rust CLI.'
    FunctionsToExport    = @(
        'Invoke-AdenGenerate',
        'Invoke-AdenAssemble',
        'Test-AdenIntegrity'
    )
    CmdletsToExport      = @()
    VariablesToExport    = @()
    AliasesToExport      = @()
    PrivateData          = @{
        PSData = @{
        }
    }
}
