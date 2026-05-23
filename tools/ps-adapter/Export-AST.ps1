param([string]$Path)
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($Path, [ref]$tokens, [ref]$errors)

$functions = $ast.FindAll({ $args[0] -is [System.Management.Automation.Language.FunctionDefinitionAst] }, $true) | ForEach-Object {
    @{
        Name = $_.Name
        Type = "FunctionDefinitionAst"
        Parameters = @($_.Parameters | ForEach-Object { $_.Name.VariablePath.UserName })
        Text = $_.Extent.Text
    }
}

$types = $ast.FindAll({ $args[0] -is [System.Management.Automation.Language.TypeDefinitionAst] }, $true) | ForEach-Object {
    @{
        Name = $_.Name
        Type = "TypeDefinitionAst"
        Members = @($_.Members | ForEach-Object { if ($_ -is [System.Management.Automation.Language.FunctionMemberAst]) { $_.Name } })
        Text = $_.Extent.Text
    }
}

@{
    File = $Path
    Functions = $functions
    Types = $types
} | ConvertTo-Json -Depth 10
