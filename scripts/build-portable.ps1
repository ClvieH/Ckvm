# Build the Windows portable package (roadmap 5.3).
#
# What it does, in order:
#   1. Frontend build + Tauri release build (no installer bundles).
#   2. Copies mykvm.exe + mykvm-input-helper.exe into the portable tree.
#   3. Ensures portable.ini exists (the portable-mode marker).
#   4. Zips the portable tree.
#   5. Verifies the fresh exe actually contains the startup-wiring log strings
#      — the same grep check used to catch the double-.setup() regression —
#      so a stale or mis-wired exe can never ship silently.
#
# Usage:  powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-portable.ps1
#         powershell ... -SkipBuild   # repackage the existing release exes

param(
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$portableTree = Join-Path $root "portable\MyKVM-Portable-win-x64\MyKVM-Portable"
$zipPath = Join-Path $root "portable\MyKVM-Portable-win-x64.zip"
$releaseDir = Join-Path $root "src-tauri\target\release"
$logMarkers = @(
    "file-clipboard landing dir",
    "discovery signing identity dir",
    "file transfer resuming"
)

Write-Host "== MyKVM portable build ==" -ForegroundColor Cyan

if (-not $SkipBuild) {
    Write-Host "[1/5] npm run tauri:build (release, no installer bundles)" -ForegroundColor Yellow
    npm run tauri:build
    if ($LASTEXITCODE -ne 0) { throw "tauri:build failed" }
} else {
    Write-Host "[1/5] skipping build (-SkipBuild)" -ForegroundColor Yellow
}

$helper = Join-Path $releaseDir "mykvm-input-helper.exe"
$main = Join-Path $releaseDir "mykvm.exe"
if (-not (Test-Path $helper)) {
    Write-Host "      helper missing; building it via cargo" -ForegroundColor Yellow
    cargo build --release --manifest-path (Join-Path $root "src-tauri\Cargo.toml") --bin mykvm-input-helper
    if ($LASTEXITCODE -ne 0) { throw "helper build failed" }
}
if (-not (Test-Path $main)) { throw "mykvm.exe not found in $releaseDir" }

Write-Host "[2/5] copying executables into the portable tree" -ForegroundColor Yellow
New-Item -ItemType Directory -Force -Path $portableTree | Out-Null
Copy-Item $main (Join-Path $portableTree "mykvm.exe") -Force
Copy-Item $helper (Join-Path $portableTree "mykvm-input-helper.exe") -Force

Write-Host "[3/5] portable.ini marker" -ForegroundColor Yellow
$ini = Join-Path $portableTree "portable.ini"
if (-not (Test-Path $ini)) {
    New-Item -ItemType File -Path $ini | Out-Null
}

Write-Host "[4/5] verifying the shipped exe contains the startup-wiring log strings" -ForegroundColor Yellow
$bytes = [System.IO.File]::ReadAllBytes($main)
$text = [System.Text.Encoding]::ASCII.GetString($bytes)
foreach ($marker in $logMarkers) {
    if (-not $text.Contains($marker)) {
        throw "wiring check FAILED: '$marker' missing from mykvm.exe — the build is stale or the single-.setup() wiring regressed"
    }
    Write-Host "      ok: '$marker'" -ForegroundColor Green
}

Write-Host "[5/5] zipping" -ForegroundColor Yellow
if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
Compress-Archive -Path (Join-Path $root "portable\MyKVM-Portable-win-x64") -DestinationPath $zipPath -CompressionLevel Optimal

$zip = Get-Item $zipPath
$exe = Get-Item (Join-Path $portableTree "mykvm.exe")
Write-Host ""
Write-Host "== portable package ready ==" -ForegroundColor Cyan
Write-Host ("  exe:  {0}  (built {1:yyyy-MM-dd HH:mm})" -f $exe.FullName, $exe.LastWriteTime)
Write-Host ("  zip:  {0}  ({1:N1} MB, built {2:yyyy-MM-dd HH:mm})" -f $zip.FullName, ($zip.Length / 1MB), $zip.LastWriteTime)
