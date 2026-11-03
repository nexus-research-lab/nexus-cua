[CmdletBinding()]
param(
    [string]$RepositoryRoot = (Join-Path $PSScriptRoot "../.."),
    [ValidateSet("debug", "release")][string]$BuildProfile = "debug",
    [ValidateRange(30, 600)][int]$TimeoutSeconds = 180,
    [switch]$WindowOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$repository = (Resolve-Path $RepositoryRoot).Path
if ([Diagnostics.Process]::GetCurrentProcess().SessionId -eq 0) {
    throw "Run in the logged-in interactive desktop session, not an SSH/service session."
}
# An active WTS session can still be locked. WGC/UIA need the input desktop.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class CuaValidationDesktop {
    [DllImport("user32.dll", SetLastError=true)]
    static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);
    [DllImport("user32.dll", SetLastError=true, CharSet=CharSet.Unicode)]
    static extern bool GetUserObjectInformationW(IntPtr handle, int index, StringBuilder value, uint size, out uint needed);
    [DllImport("user32.dll")]
    static extern bool CloseDesktop(IntPtr handle);
    public static string Name() {
        var desktop = OpenInputDesktop(0, false, 1);
        if (desktop == IntPtr.Zero) return "unavailable:" + Marshal.GetLastWin32Error();
        try {
            var value = new StringBuilder(256);
            uint needed;
            return GetUserObjectInformationW(desktop, 2, value, 512, out needed)
                ? value.ToString() : "unavailable:" + Marshal.GetLastWin32Error();
        } finally { CloseDesktop(desktop); }
    }
}
'@
$inputDesktop = [CuaValidationDesktop]::Name()
if ($inputDesktop -ne 'Default') {
    throw "Interactive input desktop is not available ($inputDesktop). Unlock the Windows console before native validation."
}
$sidecar = Join-Path $repository "target\$BuildProfile\nexuscua.exe"
$harness = Join-Path $repository "tools\native-harness\target\$BuildProfile\nexus-cua-native-harness.exe"
$fixturePath = Join-Path $repository "target\native-fixtures\windows\nexus-cua-native-fixture.exe"
foreach ($path in @($sidecar, $harness, $fixturePath)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing executable: $path" }
}
$run = [guid]::NewGuid().ToString("N")
$evidence = Join-Path $repository "target\native-evidence\windows-validation\$run"
$artifacts = Join-Path $evidence "artifacts"
$token = Join-Path $evidence "token"
$endpoint = "\\.\pipe\nexuscua-validation-$run"
New-Item -ItemType Directory -Force $evidence, $artifacts | Out-Null
([guid]::NewGuid().ToString("N") + [guid]::NewGuid().ToString("N")) | Set-Content -NoNewline $token
$service = $null
$fixture = $null
$probe = $null
try {
    $service = Start-Process $sidecar -PassThru -WorkingDirectory $repository -ArgumentList @(
        "serve", "--endpoint", $endpoint, "--token-file", "`"$token`"", "--artifact-root", "`"$artifacts`""
    ) -RedirectStandardOutput (Join-Path $evidence "service.out.log") -RedirectStandardError (Join-Path $evidence "service.err.log")
    Start-Sleep -Seconds 2
    if ($service.HasExited) { throw "Runtime exited during startup" }
    $fixture = Start-Process $fixturePath -PassThru -WorkingDirectory $repository
    Start-Sleep -Seconds 2
    $validationMode = if ($WindowOnly) { "--window-only" } else { "--enforce" }
    $probe = Start-Process $harness -PassThru -WorkingDirectory $repository -ArgumentList @(
        "--endpoint", $endpoint, "--token-file", "`"$token`"", "validate", $validationMode
    ) -RedirectStandardOutput (Join-Path $evidence "validation.json") -RedirectStandardError (Join-Path $evidence "validation.err.log")
    if (-not $probe.WaitForExit($TimeoutSeconds * 1000)) { throw "Validation exceeded bounded timeout" }
    if ($probe.ExitCode -ne 0) { throw "Validation failed ($($probe.ExitCode)); evidence=$evidence" }
    if ($WindowOnly) { Write-Output "Window-only diagnostics completed; full acceptance not evaluated; evidence=$evidence" }
    else { Write-Output "Validation passed; evidence=$evidence" }
}
finally {
    foreach ($process in @($probe, $fixture, $service)) {
        if ($null -ne $process -and -not $process.HasExited) {
            $process.Kill()
            if (-not $process.WaitForExit(5000)) {
                Write-Warning "Process $($process.Id) did not exit within cleanup deadline"
            }
        }
    }
    # Keep only metadata reports, never captured desktop pixels or transport authority.
    Remove-Item -LiteralPath $artifacts -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $token -Force -ErrorAction SilentlyContinue
}
