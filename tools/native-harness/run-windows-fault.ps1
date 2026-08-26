[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$RepositoryRoot,
    [string]$EvidenceRoot = "",
    [ValidateSet("debug", "release")]
    [string]$BuildProfile = "debug",
    [ValidateRange(500, 10000)]
    [int]$ArmDelayMilliseconds = 3000,
    [ValidateRange(500, 30000)]
    [int]$HangMilliseconds = 5000,
    [switch]$FlatOutput
)

$ErrorActionPreference = "Stop"
$repository = (Resolve-Path $RepositoryRoot).Path
if ([string]::IsNullOrWhiteSpace($EvidenceRoot)) {
    $EvidenceRoot = Join-Path $repository "target\native-evidence\windows-fault"
}
$evidence = if ($FlatOutput) {
    $EvidenceRoot
} else {
    Join-Path $EvidenceRoot ([guid]::NewGuid().ToString("N"))
}
$token = Join-Path $evidence "fault-token"
$artifacts = Join-Path $evidence "fault-artifacts"
$endpoint = "\\.\pipe\nexus-cua-native-fault-$PID"
$sidecar = Join-Path $repository "target\$BuildProfile\nexus-cua.exe"
$harness = Join-Path $repository "tools\native-harness\target\$BuildProfile\nexus-cua-native-harness.exe"
$fixturePath = Join-Path $repository "target\native-fixtures\windows\nexus-cua-native-fixture.exe"

foreach ($required in @($sidecar, $harness, $fixturePath)) {
    if (-not (Test-Path $required -PathType Leaf)) {
        throw "required native probe executable is missing: $required"
    }
}

New-Item -ItemType Directory -Force $evidence, $artifacts | Out-Null
([guid]::NewGuid().ToString("N") + [guid]::NewGuid().ToString("N")) |
    Set-Content -NoNewline $token

$service = $null
$fixture = $null
try {
    $service = Start-Process $sidecar -PassThru -ArgumentList @(
        "serve", "--log-level", "info", "--endpoint", $endpoint,
        "--token-file", $token, "--artifact-root", $artifacts
    ) -RedirectStandardOutput (Join-Path $evidence "fault-service.out.log") `
        -RedirectStandardError (Join-Path $evidence "fault-service.err.log")
    Start-Sleep -Seconds 2
    if ($service.HasExited) {
        throw "sidecar exited during startup"
    }

    $env:NEXUS_CUA_FIXTURE_FAULT_ARM_MS = $ArmDelayMilliseconds.ToString()
    $env:NEXUS_CUA_FIXTURE_FAULT_HANG_MS = $HangMilliseconds.ToString()
    $fixture = Start-Process $fixturePath -PassThru
    Start-Sleep -Seconds 1
    & $harness --endpoint $endpoint --token-file $token fault preflight `
        --arm-delay-ms $ArmDelayMilliseconds |
        Tee-Object (Join-Path $evidence "fault-preflight.json")
    if ($LASTEXITCODE -ne 0) {
        throw "preflight fault probe failed with exit code $LASTEXITCODE"
    }

    if (-not $fixture.HasExited) {
        $fixture.Kill()
        $fixture.WaitForExit()
    }
    $fixture = Start-Process $fixturePath -PassThru
    Start-Sleep -Seconds 1
    & $harness --endpoint $endpoint --token-file $token fault dispatch |
        Tee-Object (Join-Path $evidence "fault-dispatch.json")
    if ($LASTEXITCODE -ne 0) {
        throw "dispatch fault probe failed with exit code $LASTEXITCODE"
    }

    Write-Output "evidence=$evidence"
}
finally {
    Remove-Item Env:NEXUS_CUA_FIXTURE_FAULT_ARM_MS -ErrorAction SilentlyContinue
    Remove-Item Env:NEXUS_CUA_FIXTURE_FAULT_HANG_MS -ErrorAction SilentlyContinue
    if ($fixture -and -not $fixture.HasExited) {
        $fixture.Kill()
        $fixture.WaitForExit()
    }
    if ($service -and -not $service.HasExited) {
        $service.Kill()
        $service.WaitForExit()
    }
}
