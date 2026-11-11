# Banc de qualité des réglages NVENC de Clipper : pour chaque réglage, charge du bloc d'encodage (NVENC),
# taille du fichier et score VMAF face à une référence quasi sans perte.
#
# À lancer HORS PARTIE (le calcul VMAF prend beaucoup de processeur), Clipper fermé ou clipping désactivé :
#   .\scripts\quality-bench.ps1                       # enregistre 20 s de l'écran comme référence
#   .\scripts\quality-bench.ps1 -Source "C:\...\ref.mkv"   # ou une référence déjà enregistrée
# Pendant l'enregistrement de la référence, lancer une vidéo de gameplay Valorant en plein écran (mouvements
# rapides) : c'est le cas le plus dur pour l'encodeur. Résultats : scripts\quality-results.csv.
param(
    [string]$Source = "",
    [int]$Seconds = 20,
    [int]$Height = 1080,
    [int]$Fps = 60
)
$ErrorActionPreference = "Stop"
$dir = Join-Path $env:TEMP "clipper-quality"
New-Item -ItemType Directory -Force $dir | Out-Null
if (-not $Source) {
    $Source = Join-Path $dir "ref.mkv"
    Write-Host "Référence : $Seconds s de l'écran, NVENC sans perte..."
    ffmpeg -hide_banner -loglevel error -y -f lavfi -i "ddagrab=output_idx=0:framerate=${Fps}:draw_mouse=0" -t $Seconds `
        -c:v hevc_nvenc -tune lossless $Source
}
# Réglages comparés : les trois profils de Clipper, plus des préréglages plus lourds pour situer le gain.
$cases = [ordered]@{
    "competitif-p1"      = "-c:v h264_nvenc -preset p1 -tune hq -rc vbr -cq 22 -b:v 0 -maxrate 23800k -bufsize 47600k"
    "equilibre-p1-aq"    = "-c:v h264_nvenc -preset p1 -tune hq -rc vbr -cq 20 -b:v 0 -maxrate 28000k -bufsize 56000k -spatial-aq 1"
    "qualite-p2-aq"      = "-c:v h264_nvenc -preset p2 -tune hq -rc vbr -cq 20 -b:v 0 -maxrate 33600k -bufsize 67200k -spatial-aq 1"
    "p4-aq"              = "-c:v h264_nvenc -preset p4 -tune hq -rc vbr -cq 20 -b:v 0 -maxrate 28000k -bufsize 56000k -spatial-aq 1"
    "hevc-competitif-p1" = "-c:v hevc_nvenc -preset p1 -tune hq -rc vbr -cq 23 -b:v 0 -maxrate 15500k -bufsize 31000k"
    "hevc-p1-aq"         = "-c:v hevc_nvenc -preset p1 -tune hq -rc vbr -cq 21 -b:v 0 -maxrate 18200k -bufsize 36400k -spatial-aq 1"
    "av1-p1"             = "-c:v av1_nvenc -preset p1 -tune hq -rc vbr -cq 29 -b:v 0 -maxrate 12000k -bufsize 24000k"
}
$out = Join-Path $PSScriptRoot "quality-results.csv"
foreach ($name in $cases.Keys) {
    $file = Join-Path $dir "$name.mp4"
    # Encodage au rythme réel (-readrate 1 : comme en jeu). Charge du bloc d'encodage relevée chaque seconde
    # par le compteur Windows « VideoEncode » (nvidia-smi -f n'écrivait rien avant d'être arrêté).
    $p = Start-Process ffmpeg -ArgumentList "-hide_banner -loglevel error -y -readrate 1 -t $Seconds -i `"$Source`" $($cases[$name]) -vf scale=-2:$Height -an `"$file`"" -PassThru -NoNewWindow
    $enc = @()
    while (-not $p.HasExited) {
        $s = (Get-Counter '\GPU Engine(*engtype_VideoEncode)\Utilization Percentage' -ErrorAction SilentlyContinue).CounterSamples
        if ($s) { $enc += ($s | Measure-Object CookedValue -Sum).Sum }
    }
    $vmafLog = Join-Path $dir "$name.json"
    $ref = "[1:v]scale=-2:${Height}:flags=lanczos,setpts=PTS-STARTPTS[r];[0:v]setpts=PTS-STARTPTS[d];[d][r]libvmaf=log_fmt=json:log_path='$($vmafLog -replace '\\','/' -replace ':','\:')':n_threads=4"
    ffmpeg -hide_banner -loglevel error -i $file -t $Seconds -i $Source -lavfi $ref -f null - 2>$null
    $vmaf = (Get-Content $vmafLog -Raw | ConvertFrom-Json).pooled_metrics.vmaf
    $row = [pscustomobject]@{
        reglage = $name
        nvenc_moy_pct = [math]::Round(($enc | Measure-Object -Average).Average, 1)
        nvenc_max_pct = ($enc | Measure-Object -Maximum).Maximum
        mo = [math]::Round((Get-Item $file).Length / 1MB, 1)
        vmaf_moy = [math]::Round($vmaf.mean, 2)
        vmaf_1pct = [math]::Round($vmaf.min, 2)
    }
    $row | Export-Csv $out -Append -NoTypeInformation -Encoding UTF8
    $row | Format-Table -AutoSize | Out-String | Write-Host
}
Write-Host "Résultats : $out"
