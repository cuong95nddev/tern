# Build Tern from this checkout and open its installer; you click through it.
#   scripts/install-windows.ps1
# The installer closes Tern; its background terminals keep running, and the
# new Tern asks at launch whether to restart them on the new version.
$ErrorActionPreference = "Stop"

$Installer = & (Join-Path $PSScriptRoot "release-windows.ps1") | Select-Object -Last 1
if (-not $Installer -or -not (Test-Path $Installer)) { throw "no installer was built" }
Write-Host "Opening $Installer"
Start-Process -FilePath $Installer
