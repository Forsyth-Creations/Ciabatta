# Ciabatta installer for Windows — downloads the right prebuilt binary and adds
# it to your PATH. Works on x86_64 (and ARM64 via x86_64 emulation).
#
#   irm https://forsyth-creations.github.io/Ciabatta/install.ps1 | iex
#
# `iex` runs the script with no way to pass arguments, so to pin a version turn
# it into a script block and call that instead:
#
#   & ([scriptblock]::Create((irm https://forsyth-creations.github.io/Ciabatta/install.ps1))) -Version 0.3.0
#   & ([scriptblock]::Create((irm https://forsyth-creations.github.io/Ciabatta/install.ps1))) -List
#
# Options:
#   -Version VERSION  install this version (e.g. 0.3.0, v0.3.0, or latest)
#   -Dir DIR          where to install (default: %LOCALAPPDATA%\Programs\ciabatta)
#   -List             list the available versions and exit
#
# The equivalent environment variables still work; an explicit flag wins:
#   CIABATTA_INSTALL_DIR   where to install
#   CIABATTA_VERSION       pin a version (default: latest release)
param(
    [string]$Version = $env:CIABATTA_VERSION,
    [string]$Dir = $env:CIABATTA_INSTALL_DIR,
    [switch]$List
)

$ErrorActionPreference = "Stop"

$repo = "Forsyth-Creations/Ciabatta"
$asset = "ciabatta-windows-x86_64.zip"

if ($List) {
    Write-Host "Available versions of ciabatta:"
    $releases = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases?per_page=100" -UseBasicParsing
    foreach ($r in $releases) { Write-Host "  $($r.tag_name)" }
    Write-Host ""
    Write-Host "Install one with:"
    Write-Host "  & ([scriptblock]::Create((irm https://forsyth-creations.github.io/Ciabatta/install.ps1))) -Version <VERSION>"
    return
}

# Resolve download URL. GitHub serves the newest release's asset from /latest/,
# so the unpinned case needs no API call.
if ($Version -and $Version -notin @("latest", "Latest", "LATEST")) {
    # Accept "0.3.0" and "v0.3.0" alike; the tags carry the v.
    $Version = $Version.TrimStart("v")
    if ($Version -notmatch '^[0-9][0-9.]*$') {
        throw "'$Version' doesn't look like a version. Expected something like 0.3.0. Pass -List to see what exists."
    }
    $url = "https://github.com/$repo/releases/download/v$Version/$asset"
} else {
    $Version = $null
    $url = "https://github.com/$repo/releases/latest/download/$asset"
}

# If ciabatta is already installed and on PATH, update that copy in place
# (unless the user pinned a directory) so we don't leave a stale binary behind.
$existingDir = $null
$existing = Get-Command ciabatta -CommandType Application -ErrorAction SilentlyContinue |
    Select-Object -First 1
if ($existing) {
    $existingDir = Split-Path -Parent $existing.Source
}

$installDir = if ($Dir) {
    $Dir
} elseif ($existingDir) {
    Write-Host "updating existing install at $existingDir ..."
    $existingDir
} else {
    Join-Path $env:LOCALAPPDATA "Programs\ciabatta"
}

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("ciabatta-" + [System.Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
    if ($Version) {
        Write-Host "downloading $asset (v$Version) ..."
    } else {
        Write-Host "downloading $asset (latest) ..."
    }
    $zip = Join-Path $tmp $asset
    try {
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    } catch {
        if ($Version) {
            throw "no v$Version release for windows/x86_64. Pass -List to see the versions that exist."
        }
        throw
    }

    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    $exe = Join-Path $tmp "ciabatta.exe"
    if (-not (Test-Path $exe)) { throw "archive did not contain ciabatta.exe" }

    if (-not (Test-Path $installDir)) {
        New-Item -ItemType Directory -Path $installDir -Force | Out-Null
    }
    $dest = Join-Path $installDir "ciabatta.exe"
    Copy-Item -Force $exe $dest

    # Add the install dir to the user PATH if it isn't already there.
    $userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($userPath -notlike "*$installDir*") {
        [Environment]::SetEnvironmentVariable("PATH", "$userPath;$installDir", "User")
        Write-Host "added $installDir to your PATH (open a new terminal to use ciabatta)"
    }

    Write-Host "installed: $dest"
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
