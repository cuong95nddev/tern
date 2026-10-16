# Build a release tern.exe and its installer (Inno Setup) for a GitHub release.
#   scripts/release-windows.ps1 [-OutDir DIR] [-Exe tern.exe]
# OutDir defaults to target\dist. With -Exe the installer packs that
# tern.exe (the signed one, in the release workflow) instead of building one.
# Prints the installer's path. Needs Rust (x86_64-pc-windows-msvc) and Inno
# Setup 6 (`iscc` on PATH, or in its default folder).
param([string]$OutDir = "", [string]$Exe = "")
$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
if (-not $OutDir) { $OutDir = Join-Path $Root "target\dist" }
$Version = (Select-String -Path (Join-Path $Root "Cargo.toml") -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches[0].Groups[1].Value

if (-not $Exe) {
    cargo build --release -p tern-app --target x86_64-pc-windows-msvc --manifest-path (Join-Path $Root "Cargo.toml") | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    $Exe = Join-Path $Root "target\x86_64-pc-windows-msvc\release\tern-app.exe"
}

$Stage = Join-Path ([System.IO.Path]::GetTempPath()) ("tern-release-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $Stage | Out-Null
try {
    Copy-Item $Exe (Join-Path $Stage "tern.exe")

    $Iscc = (Get-Command iscc -ErrorAction SilentlyContinue).Source
    # Not on PATH: where an install for all users, or for this one, puts it.
    if (-not $Iscc) {
        $Iscc = @(
            (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6\ISCC.exe"),
            (Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe")
        ) | Where-Object { Test-Path $_ } | Select-Object -First 1
    }
    if (-not $Iscc) { throw "Inno Setup 6 (iscc) not found" }
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
    $OutDir = (Resolve-Path $OutDir).Path
    & $Iscc /Q "/DVersion=$Version" "/DNumVersion=$(($Version -split '[-+]')[0])" "/DStageDir=$Stage" "/DOutputDir=$OutDir" (Join-Path $Root "packaging\windows\tern.iss") | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "iscc failed" }
} finally {
    Remove-Item -Recurse -Force $Stage -ErrorAction SilentlyContinue
}
Join-Path $OutDir "Tern-$Version-windows-x64-setup.exe"
