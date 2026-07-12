# Ciabatta installer for Windows — downloads the right prebuilt binary and adds
# it to your PATH. Works on x86_64 (and ARM64 via x86_64 emulation).
#
#   irm https://forsyth-creations.github.io/Ciabatta/install.ps1 | iex
#
# Options (environment variables):
#   CIABATTA_INSTALL_DIR   where to install (default: %LOCALAPPDATA%\Programs\ciabatta)
#   CIABATTA_VERSION       pin a version, e.g. 0.1.15 (default: latest release)

$ErrorActionPreference = "Stop"

$repo = "Forsyth-Creations/Ciabatta"
$asset = "ciabatta-windows-x86_64.zip"

# Resolve download URL. GitHub serves the newest release's asset from /latest/,
# so no API call is needed to always fetch the current version.
if ($env:CIABATTA_VERSION) {
    $version = $env:CIABATTA_VERSION.TrimStart("v")
    $url = "https://github.com/$repo/releases/download/v$version/$asset"
} else {
    $url = "https://github.com/$repo/releases/latest/download/$asset"
}

$installDir = if ($env:CIABATTA_INSTALL_DIR) {
    $env:CIABATTA_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA "Programs\ciabatta"
}

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("ciabatta-" + [System.Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
    Write-Host "downloading $asset ..."
    $zip = Join-Path $tmp $asset
    Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing

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
