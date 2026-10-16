[CmdletBinding(DefaultParameterSetName = "Install")]
param(
    [Parameter(Mandatory = $true, ParameterSetName = "Install")]
    [string]$Executable,
    [Parameter(Mandatory = $true, ParameterSetName = "Remove")]
    [switch]$Remove
)

$ErrorActionPreference = "Stop"
$programs = [Environment]::GetFolderPath([Environment+SpecialFolder]::Programs)
if ([string]::IsNullOrWhiteSpace($programs)) {
    throw "the current user's Start Menu Programs directory is unavailable"
}
$fixtureDirectory = Join-Path $programs "Nexus CUA Native Fixture"
$shortcutPath = Join-Path $fixtureDirectory "Nexus CUA Native Fixture.lnk"

if ($Remove) {
    if (Test-Path $shortcutPath -PathType Leaf) {
        Remove-Item -LiteralPath $shortcutPath -Force
    }
    if ((Test-Path $fixtureDirectory -PathType Container) -and
        -not (Get-ChildItem -LiteralPath $fixtureDirectory -Force)) {
        Remove-Item -LiteralPath $fixtureDirectory -Force
    }
    Write-Output $shortcutPath
    exit 0
}

$resolvedExecutable = (Resolve-Path -LiteralPath $Executable).Path
if (-not (Test-Path $resolvedExecutable -PathType Leaf)) {
    throw "fixture executable is not a regular file: $resolvedExecutable"
}
if ([IO.Path]::GetFileName($resolvedExecutable) -ne "nexus-cua-native-fixture.exe") {
    throw "shortcut installer accepts only nexus-cua-native-fixture.exe"
}

New-Item -ItemType Directory -Force $fixtureDirectory | Out-Null
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $resolvedExecutable
$shortcut.WorkingDirectory = [IO.Path]::GetDirectoryName($resolvedExecutable)
$shortcut.Description = "Nexus CUA deterministic native fixture"
$shortcut.Save()

$verified = $shell.CreateShortcut($shortcutPath)
if ((Resolve-Path -LiteralPath $verified.TargetPath).Path -ne $resolvedExecutable) {
    throw "created Start Menu shortcut does not resolve to the exact fixture executable"
}
Write-Output $shortcutPath
