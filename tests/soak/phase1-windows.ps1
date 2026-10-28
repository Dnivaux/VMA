# Phase 1 acceptance on Windows, in the real application (release build):
#   A1  : 30 min, 1080p50, 4 synthetic 1080p50 sources, calm (leave the machine alone);
#   A13 : 10 min hostile soak deferred from Phase 0 (operator actions: see
#         docs/manual-tests/phase0-soak-hostile.md, section Windows).
# One report per soak in tests/soak/reports/. Exit code 1 if any soak failed.
#
#   powershell -ExecutionPolicy Bypass -File tests\soak\phase1-windows.ps1
#   powershell -ExecutionPolicy Bypass -File tests\soak\phase1-windows.ps1 -Only A13
#   $env:VMA_SOAK_SECS = 60; ...   # short dry run (both soaks)
param([ValidateSet("all", "A1", "A13")][string]$Only = "all")

$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..\..")

# GStreamer (docs/dev-setup.md): default install path unless already set.
if (-not $env:GSTREAMER_1_0_ROOT_MSVC_X86_64) {
    $env:GSTREAMER_1_0_ROOT_MSVC_X86_64 = "C:\Program Files\gstreamer\1.0\msvc_x86_64\"
}
$gst = $env:GSTREAMER_1_0_ROOT_MSVC_X86_64
if (-not (Test-Path (Join-Path $gst "bin\gstreamer-1.0-0.dll"))) {
    Write-Error "GStreamer introuvable dans $gst (voir docs/dev-setup.md)"
}
$env:PATH = (Join-Path $gst "bin") + ";" + $env:PATH
$env:PKG_CONFIG_PATH = Join-Path $gst "lib\pkgconfig"

cargo build --release
if ($LASTEXITCODE -ne 0) { exit 1 }

$day = Get-Date -Format yyyy-MM-dd
$status = 0
function Run-Soak([string]$Name, [int]$Secs, [string[]]$Extra) {
    $report = "tests/soak/reports/$day-$Name.md"
    $soakArgs = @("--soak", $Secs, "--no-journal", "--report", $report) + $Extra
    Write-Host "== $Name ($Secs s) -> $report"
    & .\target\release\vma.exe @soakArgs
    $code = $LASTEXITCODE
    $verdict = Select-String -Path $report -Pattern '^\*\*Verdict' -List -ErrorAction SilentlyContinue
    if ($verdict) { Write-Host $verdict.Line } else { Write-Host "pas de verdict dans $report" }
    if ($code -ne 0 -or -not $verdict) { $script:status = 1 }
}

$a1Secs = if ($env:VMA_SOAK_SECS) { [int]$env:VMA_SOAK_SECS } else { 1800 }
$a13Secs = if ($env:VMA_SOAK_SECS) { [int]$env:VMA_SOAK_SECS } else { 600 }

if ($Only -in "all", "A1") {
    Write-Host "A1 : ne pas toucher la machine pendant 30 min (la fenetre doit rester visible)."
    Run-Soak "phase1-a1-windows" $a1Secs @("--rate", "50", "--resolution", "1080p", "--inputs", "4")
}
if ($Only -in "all", "A13") {
    Write-Host "A13 : soak hostile. Pendant 10 min, malmener la fenetre a la souris"
    Write-Host "      (docs/manual-tests/phase0-soak-hostile.md, section Windows)."
    Read-Host "Entree pour lancer"
    Run-Soak "phase0-hostile-windows" $a13Secs @("--hostile")
}
exit $status
