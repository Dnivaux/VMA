# Builds the Windows package of VMA: a folder that runs by double-click, nothing to install
# (docs/specs/packaging-windows.md). Run by .github/workflows/windows-package.yml after
# `cargo build --release -p vma`, GStreamer installed (.github/actions/gstreamer-windows).
#
#   pwsh packaging/windows/bundle.ps1 -Out dist\VMA
#
# Layout (GStreamer's own, so that it finds its plugins relative to its DLLs):
#   Ouvrir VMA.cmd, Test Windows - 40 min.cmd, LISEZMOI.txt
#   bin\       vma.exe, launch.ps1, the DLLs vma.exe and the plugins import, build.txt
#   lib\gstreamer-1.0\        plugins whose licence is not GPL and that load
#   libexec\gstreamer-1.0\    gst-plugin-scanner.exe
#   Licences\, Observation\ (the partner's checklist and the picture of VMA)
# No GPL in the package (BRIEF §2.6): GPL plugins are left out by their declared licence, and
# the build fails if a DLL of a known GPL library is pulled in anyway.
param([Parameter(Mandatory)][string]$Out)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$here = $PSScriptRoot
$gst = $env:GSTREAMER_1_0_ROOT_MSVC_X86_64
if (-not $gst) { throw 'GSTREAMER_1_0_ROOT_MSVC_X86_64 is not set' }
$gst = $gst.TrimEnd('\')
$vma = Join-Path $repo 'target\release\vma.exe'
if (-not (Test-Path $vma)) { throw "$vma not found: cargo build --release -p vma first" }
$inspect = Join-Path $gst 'bin\gst-inspect-1.0.exe'

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$dumpbin = & $vswhere -latest -products * -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe' |
    Select-Object -First 1
if (-not $dumpbin) { throw 'dumpbin.exe not found (Visual Studio C++ tools)' }

if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
$bin = (New-Item -ItemType Directory -Force (Join-Path $Out 'bin')).FullName
$plugins = (New-Item -ItemType Directory -Force (Join-Path $Out 'lib\gstreamer-1.0')).FullName
$libexec = (New-Item -ItemType Directory -Force (Join-Path $Out 'libexec\gstreamer-1.0')).FullName
$licences = (New-Item -ItemType Directory -Force (Join-Path $Out 'Licences')).FullName
$observation = (New-Item -ItemType Directory -Force (Join-Path $Out 'Observation\images')).FullName

# --- Plugins: every one that loads and whose licence allows it.
function Test-Licence([string]$Licence) {
    if ($Licence -match 'LGPL') { return $true }
    if ($Licence -match 'GPL|Proprietary|unknown') { return $false }
    return $Licence -match 'BSD|MIT|MPL|Apache|X11|ISC|Zlib'
}
# The first capture of $Pattern in $Lines, or $null.
function First-Match($Lines, [string]$Pattern) {
    $m = $Lines | Select-String $Pattern | Select-Object -First 1
    if ($m) { $m.Matches[0].Groups[1].Value } else { $null }
}
$kept = @()
$left = @()
foreach ($file in Get-ChildItem (Join-Path $gst 'lib\gstreamer-1.0') -Filter '*.dll') {
    $info = & $inspect $file.FullName 2>$null
    $name = First-Match $info '^\s+Name\s+(\S+)'
    $licence = First-Match $info '^\s+License\s+(.+)$'
    if (-not $licence) {
        $left += "$($file.Name) : ne se charge pas"
    } elseif (Test-Licence $licence) {
        Copy-Item $file.FullName $plugins
        $kept += [pscustomobject]@{ Plugin = $name; File = $file.Name; Licence = $licence.Trim() }
    } else {
        $left += "$($file.Name) ($name) : licence $($licence.Trim())"
    }
}
Copy-Item (Join-Path $gst 'libexec\gstreamer-1.0\gst-plugin-scanner.exe') $libexec
Copy-Item $vma $bin

# --- DLLs: the closure of the imports of vma.exe, the scanner and the plugins, from
# GStreamer's bin; the Visual C++ runtime from System32 (redistributable); the rest is Windows.
function Get-Imports([string]$File) {
    & $dumpbin /nologo /dependents $File |
        Select-String '^\s{4}(\S+\.dll)\s*$' | ForEach-Object { $_.Matches[0].Groups[1].Value }
}
$vcRuntime = '^(vcruntime|msvcp|concrt|vccorlib)\d.*\.dll$'
$system32 = [Environment]::SystemDirectory
$seen = @{}
$queue = [System.Collections.Generic.Queue[string]]::new()
$queue.Enqueue((Join-Path $bin 'vma.exe'))
$queue.Enqueue((Join-Path $libexec 'gst-plugin-scanner.exe'))
Get-ChildItem $plugins -Filter '*.dll' | ForEach-Object { $queue.Enqueue($_.FullName) }
while ($queue.Count -gt 0) {
    foreach ($dll in Get-Imports $queue.Dequeue()) {
        $key = $dll.ToLowerInvariant()
        if ($seen.ContainsKey($key)) { continue }
        $seen[$key] = $true
        $fromGst = Join-Path $gst "bin\$dll"
        if (Test-Path $fromGst) {
            Copy-Item $fromGst $bin
            $queue.Enqueue((Join-Path $bin (Split-Path $fromGst -Leaf)))
        } elseif ($dll -match $vcRuntime -and (Test-Path (Join-Path $system32 $dll))) {
            Copy-Item (Join-Path $system32 $dll) $bin
        }
    }
}
$gpl = Get-ChildItem $bin, $plugins -Filter '*.dll' |
    Where-Object { $_.Name -match 'x264|x265|a52|mpeg2|dvdread|dvdnav|faad|sidplay|xvid' }
if ($gpl) { throw "GPL library in the package: $($gpl.Name -join ', ')" }

# --- Launchers, notes, licences, the partner's checklist.
function Copy-WithBom([string]$From, [string]$To) {
    Get-Content -Raw -Encoding utf8 $From | Set-Content -NoNewline -Encoding utf8BOM $To
}
Copy-WithBom (Join-Path $here 'launch.ps1') (Join-Path $bin 'launch.ps1')
Copy-WithBom (Join-Path $here 'LISEZMOI.txt') (Join-Path $Out 'LISEZMOI.txt')
Copy-Item (Join-Path $here 'Ouvrir VMA.cmd'), (Join-Path $here 'Test Windows - 40 min.cmd') $Out
Copy-Item (Join-Path $repo 'docs\manual-tests\observation-reference-interface.md') (Split-Path $observation)
Copy-Item (Join-Path $repo 'docs\manual-tests\images\vma-interface-2026-11-11.png') $observation
Copy-Item (Join-Path $repo 'LICENSE-MIT') (Join-Path $licences 'VMA-MIT.txt')
Copy-Item (Join-Path $repo 'LICENSE-APACHE') (Join-Path $licences 'VMA-Apache-2.0.txt')
Copy-Item (Join-Path $here 'LGPL-2.1.txt'), (Join-Path $here 'MPL-2.0.txt') $licences
if (Test-Path (Join-Path $gst 'share\licenses')) {
    Copy-Item -Recurse (Join-Path $gst 'share\licenses') (Join-Path $licences 'GStreamer')
}

$version = First-Match (Get-Content (Join-Path $repo 'Cargo.toml')) '^version\s*=\s*"(.+)"'
$commit = (git -C $repo rev-parse --short HEAD)
"VMA $version, commit $commit, GStreamer $env:GST_WIN_VERSION, construit le $(Get-Date -Format yyyy-MM-dd)" |
    Set-Content -Encoding utf8 (Join-Path $bin 'build.txt')

$notice = @(
    "VMA $version (commit $commit) : MIT ou Apache-2.0 (VMA-MIT.txt, VMA-Apache-2.0.txt).",
    'Composants Rust et leurs licences : fenêtre « À propos » de VMA ; liste complète :',
    '`cargo deny list` dans les sources de VMA.',
    '',
    "GStreamer $env:GST_WIN_VERSION (paquets officiels MSVC x86_64, non modifiés), lié",
    'dynamiquement : LGPL-2.1 ou ultérieure (LGPL-2.1.txt) pour l''essentiel. Sources :',
    "https://gstreamer.freedesktop.org/src/ (version $env:GST_WIN_VERSION). Les bibliothèques",
    'de bin\ et lib\gstreamer-1.0\ peuvent être remplacées par d''autres versions compatibles.',
    'Aucun plugin sous GPL n''est inclus. Plugin NDI® (gst-plugin-ndi, dans vma.exe) : MPL-2.0',
    '(MPL-2.0.txt) ; le runtime NDI® n''est pas fourni. NDI® is a registered trademark of Vizrt NDI AB.',
    'Runtime Visual C++ (vcruntime, msvcp) : fichiers redistribuables de Microsoft.',
    '',
    'Plugins GStreamer inclus (nom, fichier, licence déclarée) :'
)
$notice += $kept | Sort-Object Plugin | ForEach-Object { "  $($_.Plugin)  $($_.File)  $($_.Licence)" }
$notice += '', 'Plugins laissés de côté :'
$notice += $left | Sort-Object | ForEach-Object { "  $_" }
$notice | Set-Content -Encoding utf8BOM (Join-Path $licences 'LICENCES.txt')

$size = (Get-ChildItem -Recurse $Out | Measure-Object Length -Sum).Sum / 1MB
Write-Host ("Package: {0} plugins kept, {1} left out, {2} DLLs in bin, {3:N0} MiB" -f
    $kept.Count, $left.Count, (Get-ChildItem $bin -Filter '*.dll').Count, $size)
$left | Sort-Object | ForEach-Object { Write-Host "  left out: $_" }
