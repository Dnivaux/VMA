# Launcher of the Windows package (packaging/windows/bundle.ps1), run by the .cmd files at its
# root: VMA with the GStreamer of the package (never an installed one), logs and reports in
# the "Rapports" folder next to it.
#   -Mode Open  : VMA with 4 synthetic inputs, to look at the interface;
#   -Mode Test  : the Windows soaks deferred since Phase 0 (A1 calm 30 min, then A13 hostile
#                 10 min: docs/manual-tests/phase0-soak-hostile.md), one report each;
#   -Mode Smoke : 5 s soak, 720p25, one input (CI check of the package; exit 1 on failure).
# $env:VMA_SOAK_SECS shortens both soaks of -Mode Test (dry run).
param([ValidateSet('Open', 'Test', 'Smoke')][string]$Mode = 'Open')

$ErrorActionPreference = 'Stop'
try { [Console]::OutputEncoding = [Text.Encoding]::UTF8 } catch {}

$bin = $PSScriptRoot
$root = Split-Path $bin -Parent
$vma = Join-Path $bin 'vma.exe'
$reports = Join-Path $root 'Rapports'
New-Item -ItemType Directory -Force $reports | Out-Null
$stamp = Get-Date -Format 'yyyy-MM-dd_HH-mm'
$day = Get-Date -Format 'yyyy-MM-dd'

# GStreamer of the package only: its plugins, its scanner, a registry of its own.
$env:PATH = "$bin;$env:PATH"
$env:GST_PLUGIN_SYSTEM_PATH_1_0 = Join-Path $root 'lib\gstreamer-1.0'
$env:GST_PLUGIN_SCANNER_1_0 = Join-Path $root 'libexec\gstreamer-1.0\gst-plugin-scanner.exe'
$env:GST_PLUGIN_PATH_1_0 = $null
$env:GST_PLUGIN_PATH = $null
$registry = Join-Path $env:LOCALAPPDATA 'VMA'
New-Item -ItemType Directory -Force $registry | Out-Null
$env:GST_REGISTRY_1_0 = Join-Path $registry 'gstreamer-registry.bin'

# Runs vma.exe, its output in Rapports\<name>.log; returns its exit code.
function Invoke-Vma([string]$Name, [string[]]$Arguments) {
    $quoted = $Arguments | ForEach-Object { if ($_ -match '\s') { '"' + $_ + '"' } else { $_ } }
    $p = Start-Process -FilePath $vma -ArgumentList ($quoted -join ' ') -WorkingDirectory $root `
        -NoNewWindow -PassThru `
        -RedirectStandardError (Join-Path $reports "$Name.log") `
        -RedirectStandardOutput (Join-Path $reports "$Name.out.log")
    $null = $p.Handle # keeps the exit code readable after the wait (Windows PowerShell 5.1)
    $p.WaitForExit()
    return $p.ExitCode
}

# The verdict line of a soak report, or why there is none.
function Show-Verdict([string]$Report, [int]$Code) {
    $line = $null
    if (Test-Path $Report) {
        $line = Select-String -Path $Report -Pattern '^\*\*Verdict' -List -Encoding UTF8 |
            Select-Object -First 1
    }
    if ($line) {
        Write-Host ('   ' + $line.Line.Trim('*'))
    } else {
        Write-Host "   Pas de rapport : VMA s'est arrêté avant la fin (code $Code)." -ForegroundColor Yellow
    }
    Write-Host ''
}

# OS, processor, GPUs, screens, power: what the reports do not say.
function Save-Machine {
    $out = Join-Path $reports "machine-$stamp.txt"
    $lines = @("Paquet : " + (Get-Content (Join-Path $bin 'build.txt') -ErrorAction SilentlyContinue))
    try {
        $os = Get-CimInstance Win32_OperatingSystem
        $lines += "Windows : $($os.Caption) $($os.Version) (build $($os.BuildNumber))"
        $lines += 'Processeur : ' + ((Get-CimInstance Win32_Processor).Name -join ' ; ')
        $lines += 'Mémoire : {0:N1} Gio' -f ($os.TotalVisibleMemorySize / 1MB)
        foreach ($g in Get-CimInstance Win32_VideoController) {
            $lines += "GPU : $($g.Name), pilote $($g.DriverVersion), " +
                "$($g.CurrentHorizontalResolution)x$($g.CurrentVerticalResolution) à $($g.CurrentRefreshRate) Hz"
        }
        $battery = Get-CimInstance Win32_Battery
        if ($battery) {
            $plugged = if ($battery.BatteryStatus -eq 2) { 'sur secteur' } else { 'SUR BATTERIE' }
            $lines += "Alimentation : portable, $plugged, $($battery.EstimatedChargeRemaining) %"
        } else {
            $lines += 'Alimentation : secteur (pas de batterie)'
        }
        $plan = (powercfg /getactivescheme) -join ' '
        $lines += "Mode d'alimentation : $plan"
    } catch {
        $lines += "(informations incomplètes : $_)"
    }
    $lines | Set-Content -Encoding UTF8 $out
}

# No sleep, no screen off while this script runs.
function Stop-Sleep {
    try {
        Add-Type -Namespace Vma -Name Power -MemberDefinition `
            '[DllImport("kernel32.dll")] public static extern uint SetThreadExecutionState(uint flags);'
        # ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
        [void][Vma.Power]::SetThreadExecutionState([uint32]2147483651)
    } catch {
        Write-Host "(La mise en veille n'a pas pu être bloquée : la désactiver le temps du test.)"
    }
}

# The package folder in the Explorer, the Rapports folder selected (ready to compress).
function Show-Reports { Start-Process explorer.exe "/select,`"$reports`"" }

function Wait-Enter([string]$What) { [void](Read-Host "   Appuyer sur Entrée pour $What") }

switch ($Mode) {
    'Smoke' {
        $report = Join-Path $reports 'smoke.md'
        $code = Invoke-Vma 'smoke' @('--soak', '5', '--rate', '25', '--resolution', '720p',
            '--inputs', '1', '--no-journal', '--report', $report)
        Write-Host "vma.exe : code $code"
        if (Test-Path $report) { Get-Content -Encoding UTF8 $report | Write-Host }
        Get-Content (Join-Path $reports 'smoke.log') -ErrorAction SilentlyContinue | Select-Object -Last 40 | Write-Host
        # 2 = frames missed: expected on a runner without GPU. The input must have run.
        $ran = (Test-Path $report) -and (Select-String -Path $report -Pattern '^## Inputs' -Quiet)
        if (($code -ne 0 -and $code -ne 2) -or -not $ran) { exit 1 }
        exit 0
    }
    'Open' {
        Write-Host ''
        Write-Host '  VMA s''ouvre, avec 4 mires de test comme inputs.'
        Write-Host '  Ne pas fermer cette fenêtre noire : elle fait tourner VMA.'
        Write-Host '  Pour quitter : fermer la fenêtre de VMA.'
        Write-Host ''
        $code = Invoke-Vma "vma-$stamp" @('--inputs', '4', '--journal-dir', (Join-Path $reports 'journal'))
        if ($code -ne 0) {
            Write-Host "  VMA s'est arrêté sur une erreur (code $code)." -ForegroundColor Yellow
            Write-Host '  Le dossier Rapports va s''ouvrir : l''envoyer, avec une phrase sur ce qui s''est passé.'
            Show-Reports
            Wait-Enter 'fermer'
        }
    }
    'Test' {
        $calm = if ($env:VMA_SOAK_SECS) { $env:VMA_SOAK_SECS } else { '1800' }
        $hostile = if ($env:VMA_SOAK_SECS) { $env:VMA_SOAK_SECS } else { '600' }
        $r1 = Join-Path $reports "$day-phase1-a1-windows.md"
        $r2 = Join-Path $reports "$day-phase0-hostile-windows.md"
        Save-Machine
        Stop-Sleep

        Write-Host ''
        Write-Host '  TEST WINDOWS : VMA ne doit perdre aucune image. Deux parties, 40 minutes en tout.'
        Write-Host '  Portable : brancher le chargeur. Fermer les autres programmes.'
        Write-Host ''
        Write-Host '  PARTIE 1 / 2 : 30 minutes SANS TOUCHER l''ordinateur.'
        Write-Host '  VMA fait des transitions tout seul. Laisser sa fenêtre visible :'
        Write-Host '  ne pas la réduire, ne rien ouvrir par-dessus. Elle se ferme seule à la fin.'
        Write-Host ''
        Wait-Enter 'commencer la partie 1'
        Write-Host '  Partie 1 en cours (30 min)...'
        $c1 = Invoke-Vma "test1-calme-$stamp" @('--soak', $calm, '--rate', '50', '--resolution', '1080p',
            '--inputs', '4', '--no-journal', '--report', $r1)
        Write-Host '  Partie 1 terminée.'
        Show-Verdict $r1 $c1

        Write-Host '  PARTIE 2 / 2 : 10 minutes à MALMENER la fenêtre.'
        Write-Host '  VMA la secoue aussi tout seul (taille, plein écran, réduite) : c''est voulu.'
        Write-Host '  Pendant ces 10 minutes, plusieurs fois :'
        Write-Host '   - tenir la barre de titre et déplacer la fenêtre sans lâcher, 30 secondes ou plus ;'
        Write-Host '   - tirer un bord de la fenêtre pour la redimensionner, 30 secondes ou plus ;'
        Write-Host '   - la réduire puis la rouvrir par la barre des tâches ;'
        Write-Host '   - touches Windows + flèche bas, puis Windows + flèche haut ;'
        Write-Host '   - s''il y a un deuxième écran : y déplacer la fenêtre et revenir.'
        Write-Host '  Ne pas la fermer : elle se ferme seule à la fin.'
        Write-Host ''
        Wait-Enter 'commencer la partie 2'
        Write-Host '  Partie 2 en cours (10 min)...'
        $c2 = Invoke-Vma "test2-fenetre-$stamp" @('--soak', $hostile, '--hostile', '--no-journal', '--report', $r2)
        Write-Host '  Partie 2 terminée.'
        Show-Verdict $r2 $c2

        if (Test-Path $r2) {
            Write-Host '  Trois questions sur la partie 2 (o = oui, n = non) :'
            $moved = Read-Host '   Fenêtre déplacée en tenant la barre de titre ?'
            $resized = Read-Host '   Fenêtre redimensionnée par un bord ?'
            $minimized = Read-Host '   Fenêtre réduite puis rouverte ?'
            $notes = Read-Host '   Une remarque ? (Entrée si rien)'
            @('', '### Réponses de l''opératrice (lanceur du paquet Windows)',
                "- Déplacée par la barre de titre : $moved",
                "- Redimensionnée par un bord : $resized",
                "- Réduite puis rouverte : $minimized",
                "- Remarque : $notes") | Add-Content -Encoding UTF8 $r2
        }

        Write-Host ''
        Write-Host '  C''est fini, merci ! Le dossier Rapports va apparaître, sélectionné :'
        Write-Host '  clic droit dessus > Compresser (ou Envoyer vers > Dossier compressé), puis l''envoyer.'
        Show-Reports
        Wait-Enter 'fermer'
    }
}
