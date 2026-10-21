[CmdletBinding()]
param(
    [string]$Cargo = "cargo",
    [string]$RustToolchain = "1.88.0",
    [string]$Go = "go",
    [string]$GoFmt = "gofmt",
    [string]$Python = "python",
    [string]$Npm = "npm"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
if (Test-Path Variable:PSNativeCommandUseErrorActionPreference) {
    $PSNativeCommandUseErrorActionPreference = $false
}

$RepositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
[string[]]$CargoPrefix = @(if ($RustToolchain) { "+$RustToolchain" })

function Invoke-External {
    param(
        [Parameter(Mandatory = $true)][string]$Command,
        [Parameter(Mandatory = $true)][string[]]$CommandArguments,
        [string]$WorkingDirectory = $RepositoryRoot
    )

    Push-Location $WorkingDirectory
    try {
        & $Command @CommandArguments
        if ($LASTEXITCODE -ne 0) {
            throw "Command failed with exit code ${LASTEXITCODE}: $Command $($CommandArguments -join ' ')"
        }
    }
    finally {
        Pop-Location
    }
}

function Invoke-Cargo {
    param([Parameter(Mandatory = $true)][string[]]$CommandArguments)
    Invoke-External $Cargo ($CargoPrefix + $CommandArguments)
}

function Assert-DirectoryEqual {
    param(
        [Parameter(Mandatory = $true)][string]$Expected,
        [Parameter(Mandatory = $true)][string]$Actual
    )

    $expectedFiles = @(Get-ChildItem -Path $Expected -File -Recurse | ForEach-Object {
        [IO.Path]::GetRelativePath($Expected, $_.FullName)
    } | Sort-Object)
    $actualFiles = @(Get-ChildItem -Path $Actual -File -Recurse | ForEach-Object {
        [IO.Path]::GetRelativePath($Actual, $_.FullName)
    } | Sort-Object)
    if (($expectedFiles -join "`n") -cne ($actualFiles -join "`n")) {
        throw "Generated schema file set differs from schemas/nexus.cua.v1"
    }
    foreach ($relative in $expectedFiles) {
        $expectedHash = (Get-FileHash -Algorithm SHA256 (Join-Path $Expected $relative)).Hash
        $actualHash = (Get-FileHash -Algorithm SHA256 (Join-Path $Actual $relative)).Hash
        if ($expectedHash -cne $actualHash) {
            throw "Generated schema differs: $relative"
        }
    }
}

Push-Location $RepositoryRoot
$schemaDirectory = $null
$previousPythonPath = $env:PYTHONPATH
try {
    Write-Host "== Rust workspace =="
    Invoke-Cargo @("fmt", "--all", "--", "--check")
    Invoke-Cargo @("clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings")
    Invoke-Cargo @("test", "--workspace", "--locked")

    Write-Host "== Generated contracts and documentation =="
    $schemaDirectory = Join-Path ([IO.Path]::GetTempPath()) ("nexuscua-schema-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $schemaDirectory | Out-Null
    Invoke-Cargo @("run", "--quiet", "--locked", "--package", "nexus-cua", "--", "schema", "--output-dir", $schemaDirectory)
    Assert-DirectoryEqual (Join-Path $RepositoryRoot "schemas/nexus.cua.v1") $schemaDirectory
    Invoke-External $Python @("scripts/generate_skills.py", "--check")
    Invoke-External $Python @("scripts/check_markdown_links.py")
    Invoke-External $Python @("scripts/check_workflow_actions.py")

    Write-Host "== Release helpers and native harness =="
    Invoke-External $Python @(
        "-m", "compileall", "-q",
        "scripts/release_candidate.py", "scripts/test_release_candidate.py",
        "scripts/package_channels.py", "scripts/test_package_channels.py",
        "scripts/check_workflow_actions.py", "scripts/test_workflow_actions.py"
    )
    Invoke-External $Python @("-m", "unittest", "discover", "-s", "scripts", "-p", "test_*.py", "-v")
    Invoke-Cargo @("fmt", "--manifest-path", "tools/native-harness/Cargo.toml", "--", "--check")
    Invoke-Cargo @("clippy", "--manifest-path", "tools/native-harness/Cargo.toml", "--all-targets", "--locked", "--", "-D", "warnings")
    Invoke-Cargo @("test", "--manifest-path", "tools/native-harness/Cargo.toml", "--locked")

    Write-Host "== Official SDKs =="
    $unformattedGoFiles = @(& $GoFmt -l sdk/go)
    if ($LASTEXITCODE -ne 0) {
        throw "gofmt failed with exit code $LASTEXITCODE"
    }
    if ($unformattedGoFiles.Count -ne 0) {
        throw "Go files require formatting: $($unformattedGoFiles -join ', ')"
    }
    Invoke-External $Go @("mod", "verify") (Join-Path $RepositoryRoot "sdk/go")
    Invoke-External $Go @("vet", "./...") (Join-Path $RepositoryRoot "sdk/go")
    Invoke-External $Go @("test", "./...") (Join-Path $RepositoryRoot "sdk/go")

    $env:PYTHONPATH = Join-Path $RepositoryRoot "sdk/python/src"
    Invoke-External $Python @("-m", "compileall", "-q", "sdk/python/src", "sdk/python/tests", "sdk/python/examples")
    Invoke-External $Python @("-m", "unittest", "discover", "-s", "sdk/python/tests", "-v")
    Invoke-External $Npm @("--prefix", "sdk/typescript", "ci", "--ignore-scripts")
    Invoke-External $Npm @("--prefix", "sdk/typescript", "run", "check")

    Write-Host "Windows source quality gate passed."
}
finally {
    $env:PYTHONPATH = $previousPythonPath
    if ($null -ne $schemaDirectory -and (Test-Path -LiteralPath $schemaDirectory)) {
        Remove-Item -LiteralPath $schemaDirectory -Recurse -Force
    }
    Pop-Location
}
