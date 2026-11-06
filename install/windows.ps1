# Installs naTiVeFS on Windows for the current user, no administrator rights needed:
#   irm https://raw.githubusercontent.com/TomasekValla/naTiVeFS/main/install/windows.ps1 | iex
$ErrorActionPreference = "Stop"
$repo = "TomasekValla/naTiVeFS"

$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }
# the newest release, pre-releases included
$release = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases?per_page=1")[0]
$name = "naTiVeFS-windows-$arch-setup.exe"
$asset = $release.assets | Where-Object name -eq $name
if (-not $asset) { throw "No $name in $($release.tag_name)" }

$dir = Join-Path ([IO.Path]::GetTempPath()) "nativefs-install-$PID"
New-Item -ItemType Directory -Force $dir | Out-Null
try {
    $setup = Join-Path $dir $name
    Write-Host "Downloading naTiVeFS $($release.tag_name)"
    Invoke-WebRequest $asset.browser_download_url -OutFile $setup -UseBasicParsing
    if (Get-Command minisign -ErrorAction SilentlyContinue) {
        Invoke-WebRequest "$($asset.browser_download_url).minisig" -OutFile "$setup.minisig" -UseBasicParsing
        minisign -Vm $setup -P "RWSQa45JZ1TSMKJ+gVjv+fsOf40z6FcRFMCMz2CplMxl40yszLg0URRu" | Out-Null
        if ($LASTEXITCODE) { throw "The signature does not match, nothing was installed." }
        Write-Host "Signature verified."
    }
    Start-Process $setup -ArgumentList "/S" -Wait
    Write-Host "naTiVeFS $($release.tag_name) is installed. Find it in the Start menu."
} finally {
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
}
