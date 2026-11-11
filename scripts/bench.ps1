# Mesure de l'impact de Clipper : processeur, carte graphique et RAM de Clipper (et de ses ffmpeg /
# PresentMon), plus, si un jeu est donné, les temps d'image du jeu relevés par PresentMon.
#
# Usage (PowerShell en administrateur pour la partie PresentMon) :
#   .\scripts\bench.ps1 -Seconds 60 -Label "clipper-arrete"
#   .\scripts\bench.ps1 -Seconds 60 -Label "buffer-actif" -Game "VALORANT-Win64-Shipping.exe"
# Faire trois relevés dans la même scène du jeu : Clipper arrêté (clipping désactivé), buffer actif, REC en
# cours. Les résultats s'ajoutent à scripts\bench-results.csv pour comparer avant / après.
param(
    [int]$Seconds = 60,
    [string]$Label = "mesure",
    [string]$Game = ""
)
$ErrorActionPreference = "SilentlyContinue"
$names = "clipper", "ffmpeg", "presentmon"
$cores = [Environment]::ProcessorCount
$pmCsv = Join-Path $env:TEMP "clipper-bench-pm.csv"
$pm = $null
if ($Game) {
    $exe = Join-Path $PSScriptRoot "..\installer\presentmon.exe"
    if (-not (Test-Path $exe)) { $exe = "presentmon" }
    Remove-Item $pmCsv -ErrorAction SilentlyContinue
    $pm = Start-Process $exe -ArgumentList "--process_name $Game --timed $Seconds --terminate_after_timed --output_file `"$pmCsv`" --no_console_stats --session_name ClipperBench --stop_existing_session" -PassThru -WindowStyle Hidden
}
$t0 = Get-Process $names | ForEach-Object { [pscustomobject]@{ Id = $_.Id; Cpu = $_.TotalProcessorTime.TotalSeconds } }
$ram = @(); $gpu = @()
for ($i = 0; $i -lt $Seconds; $i++) {
    Start-Sleep 1
    $ps = Get-Process $names
    $ram += ($ps | Measure-Object WorkingSet64 -Sum).Sum / 1MB
    $ids = $ps.Id
    $g = (Get-Counter '\GPU Engine(*)\Utilization Percentage').CounterSamples |
        Where-Object { $_.InstanceName -match '^pid_(\d+)_' -and $ids -contains [int]$Matches[1] } |
        Measure-Object CookedValue -Sum
    $gpu += $g.Sum
}
$t1 = Get-Process $names
$cpu = 0
foreach ($p in $t1) {
    $o = $t0 | Where-Object Id -eq $p.Id
    $start = if ($o) { $o.Cpu } else { 0 }
    $cpu += $p.TotalProcessorTime.TotalSeconds - $start
}
$cpuPct = $cpu / $Seconds / $cores * 100
$row = [ordered]@{
    date = (Get-Date -Format "yyyy-MM-dd HH:mm:ss"); label = $Label
    cpu_pct_machine = [math]::Round($cpuPct, 2)
    gpu_pct_moy = [math]::Round(($gpu | Measure-Object -Average).Average, 2)
    ram_mo_moy = [math]::Round(($ram | Measure-Object -Average).Average, 0)
    fps_moy = ""; frametime_moy_ms = ""; frametime_p99_ms = ""; low1_fps = ""
}
if ($pm) {
    $pm.WaitForExit()
    $rows = Import-Csv $pmCsv
    $col = if ($rows[0].PSObject.Properties.Name -contains "MsBetweenDisplayChange") { "MsBetweenDisplayChange" } elseif ($rows[0].PSObject.Properties.Name -contains "DisplayedTime") { "DisplayedTime" } else { "MsBetweenPresents" }
    $ft = $rows | ForEach-Object { $_.$col -as [double] } | Where-Object { $_ -gt 0 } | Sort-Object
    if ($ft.Count -gt 20) {
        $avg = ($ft | Measure-Object -Average).Average
        $p99 = $ft[[int][math]::Floor($ft.Count * 0.99) - 1]
        $worst = $ft | Select-Object -Last ([math]::Ceiling($ft.Count / 100))
        $row.fps_moy = [math]::Round(1000 / $avg, 1)
        $row.frametime_moy_ms = [math]::Round($avg, 2)
        $row.frametime_p99_ms = [math]::Round($p99, 2)
        $row.low1_fps = [math]::Round(1000 / ($worst | Measure-Object -Average).Average, 1)
    }
}
$out = Join-Path $PSScriptRoot "bench-results.csv"
[pscustomobject]$row | Export-Csv $out -Append -NoTypeInformation -Encoding UTF8
[pscustomobject]$row | Format-List
