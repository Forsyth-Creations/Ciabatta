# Installs ciabatta to a directory on your PATH.
# Usage: .\install.ps1 [-InstallDir <path>]   (default: %LOCALAPPDATA%\Programs\ciabatta)
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA "Programs\ciabatta")
)

$ErrorActionPreference = "Stop"
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path

if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}

$dest = Join-Path $InstallDir "ciabatta.exe"
Copy-Item -Force (Join-Path $scriptDir "ciabatta.exe") $dest

$userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($userPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("PATH", "$userPath;$InstallDir", "User")
    Write-Host "Added $InstallDir to your PATH (open a new terminal to use ciabatta)"
}

Write-Host "installed: $dest"

# `oven` is a shorter name for the same binary. Symlinks need admin rights on
# Windows, so it's a .cmd shim that forwards to ciabatta.exe beside it. Never
# replace an oven.cmd that isn't ours.
$shim = Join-Path $InstallDir "oven.cmd"
$shimBody = "@`"%~dp0ciabatta.exe`" %*"
if (-not (Test-Path $shim)) {
    Set-Content -Path $shim -Value $shimBody -Encoding ASCII
    Write-Host "alias:     $shim"
} elseif ((Get-Content -Raw $shim).Trim() -ne $shimBody) {
    Write-Host "note: $shim already exists and isn't ciabatta's - left it alone, so no 'oven' alias"
}
