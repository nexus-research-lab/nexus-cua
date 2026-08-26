$ErrorActionPreference = "Stop"

$repositoryRoot = Resolve-Path (Join-Path $PSScriptRoot "..\..\..")
$project = Join-Path $PSScriptRoot "Nexus.Cua.NativeFixture.csproj"
$output = Join-Path $repositoryRoot "target\native-fixtures\windows"
$configuration = if ($args.Count -gt 0) { $args[0] } else { "Debug" }
$runtimeIdentifier = if ($args.Count -gt 1) { $args[1] } else { "win-x64" }

if ($runtimeIdentifier -notin @("win-x64", "win-arm64")) {
    throw "runtime identifier must be win-x64 or win-arm64"
}

dotnet publish $project `
    --configuration $configuration `
    --runtime $runtimeIdentifier `
    --self-contained true `
    --output $output `
    -p:PublishSingleFile=true
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
Write-Output (Join-Path $output "nexus-cua-native-fixture.exe")
