# Télécharge PresentMon (Intel, licence MIT) dans installer\vendor\ avant de compiler l'installateur.
# Source officielle : https://github.com/GameTechDev/PresentMon/releases
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue' # la barre de progression rend le téléchargement très lent
$version = '2.6.0'
$out = Join-Path $PSScriptRoot 'vendor\presentmon.exe'
New-Item -ItemType Directory -Force (Split-Path $out) | Out-Null
foreach ($try in 1..3) {
    try { Invoke-WebRequest "https://github.com/GameTechDev/PresentMon/releases/download/v$version/PresentMon-$version-x64.exe" -OutFile $out; break }
    catch { if ($try -eq 3) { throw }; Start-Sleep 5 }
}
$sig = Get-AuthenticodeSignature $out
if ($sig.Status -ne 'Valid' -or $sig.SignerCertificate.Subject -notmatch 'O=Intel Corporation') {
    Remove-Item $out
    throw "Signature Intel absente ou invalide : $($sig.Status)"
}
"PresentMon $version prêt : $out"
