<#
  Builds a release of JOJO Server: the installer, its update signature and the latest.json file
  that installed copies read to find new versions.

  Usage (from the project folder):
    .\scripts\build-release.ps1 -Version 1.1.1 -Notes "What is new" `
        -DownloadBase "https://github.com/OWNER/REPO/releases/download/v1.1.1" `
        -UpdateUrl    "https://github.com/OWNER/REPO/releases/latest/download/latest.json"

  Optional: -CertThumbprint <thumbprint of your code-signing certificate> to sign the installer
  (this is what removes the Windows SmartScreen "unknown publisher" warning).

  The update-signing key lives OUTSIDE the project, in %USERPROFILE%\.jojo-signing.
  Back that folder up: without it, installed copies can never be updated again.
#>
param(
  [string]$Version = "",
  [string]$Notes = "",
  [string]$DownloadBase = "",
  [string]$UpdateUrl = "",
  [string]$CertThumbprint = ""
)
$ErrorActionPreference = "Stop"
$project = Split-Path $PSScriptRoot -Parent
Set-Location $project

# 1. version
$confPath = Join-Path $project "src-tauri\tauri.conf.json"
if ($Version) {
  # Write as UTF-8 WITHOUT a byte-order mark: Node and Cargo choke on one.
  $utf8 = New-Object System.Text.UTF8Encoding($false)
  function Set-Version($path, $pattern, $replacement) {
    $text = [System.IO.File]::ReadAllText($path)
    [System.IO.File]::WriteAllText($path, [regex]::Replace($text, $pattern, $replacement, 'Multiline'), $utf8)
  }
  Set-Version $confPath '"version": "[^"]+"' "`"version`": `"$Version`""
  Set-Version (Join-Path $project "package.json") '"version": "[^"]+"' "`"version`": `"$Version`""
  Set-Version (Join-Path $project "src-tauri\Cargo.toml") '^version = "[^"]+"' "version = `"$Version`""
}
$Version = (Get-Content $confPath -Raw | ConvertFrom-Json).version
Write-Host "Building JOJO Server $Version"

# 2. keys and addresses
$keyDir = Join-Path $env:USERPROFILE ".jojo-signing"
if (-not (Test-Path (Join-Path $keyDir "updater.key"))) { throw "Update-signing key not found in $keyDir" }
$env:TAURI_SIGNING_PRIVATE_KEY = Join-Path $keyDir "updater.key"
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = (Get-Content (Join-Path $keyDir "updater.pass") -Raw).Trim()
if ($UpdateUrl) { $env:JOJO_UPDATE_URL = $UpdateUrl }   # baked into the app: where it looks for updates

# 3. build
$extra = @()
if ($CertThumbprint) {
  $cfg = @{ bundle = @{ windows = @{ certificateThumbprint = $CertThumbprint; timestampUrl = "http://timestamp.digicert.com" } } } | ConvertTo-Json -Depth 5 -Compress
  $extra = @("--config", $cfg)
}
npm install
npm run tauri build -- @extra
if ($LASTEXITCODE -ne 0) { throw "Build failed" }

# 4. collect the release files
$nsis = Join-Path $project "src-tauri\target\release\bundle\nsis"
$exe = Get-ChildItem $nsis -Filter "*_${Version}_x64-setup.exe" | Select-Object -First 1
if (-not $exe) { throw "Installer for $Version not found in $nsis" }
$sig = "$($exe.FullName).sig"
if (-not (Test-Path $sig)) { throw "Signature file is missing: $sig" }
$out = Join-Path $project "release\$Version"
New-Item -ItemType Directory -Force $out | Out-Null
$name = "JOJO-Server_${Version}_x64-setup.exe"      # no spaces: safe in download links
Copy-Item $exe.FullName (Join-Path $out $name) -Force
Copy-Item $sig (Join-Path $out "$name.sig") -Force

$url = if ($DownloadBase) { "$($DownloadBase.TrimEnd('/'))/$name" } else { "REPLACE-WITH-THE-DOWNLOAD-LINK-OF/$name" }
$latest = [ordered]@{
  version  = $Version
  notes    = $Notes
  pub_date = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
  platforms = [ordered]@{
    "windows-x86_64" = [ordered]@{ signature = (Get-Content $sig -Raw).Trim(); url = $url }
  }
}
$latest | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $out "latest.json") -Encoding utf8
Write-Host ""
Write-Host "Done. Upload these files to your release:"
Get-ChildItem $out | ForEach-Object { Write-Host "  $($_.FullName)" }
if (-not $CertThumbprint) { Write-Host "Note: the installer is not code-signed, so Windows SmartScreen will warn on other PCs." }
