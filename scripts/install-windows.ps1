# Build Tern from this checkout and open its installer; you click through it.
#   scripts/install-windows.ps1
# The installer closes Tern and its background terminals before it copies
# the new tern.exe (see packaging/windows/tern.iss).
$ErrorActionPreference = "Stop"

$Installer = & (Join-Path $PSScriptRoot "release-windows.ps1") | Select-Object -Last 1
if (-not $Installer -or -not (Test-Path $Installer)) { throw "no installer was built" }
Write-Host "Opening $Installer"
Start-Process -FilePath $Installer
