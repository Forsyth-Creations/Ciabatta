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
#   -Unattended       don't ask anything; if there's no Git Bash (which runs
#                     workflow steps), download a private PortableGit
#
# The equivalent environment variables still work; an explicit flag wins:
#   CIABATTA_INSTALL_DIR   where to install
#   CIABATTA_VERSION       pin a version (default: latest release)
#   CIABATTA_UNATTENDED    any value: same as -Unattended
param(
    [string]$Version = $env:CIABATTA_VERSION,
    [string]$Dir = $env:CIABATTA_INSTALL_DIR,
    [switch]$List,
    [switch]$Unattended
)

$ErrorActionPreference = "Stop"

# --- Git Bash -----------------------------------------------------------------
# ciabatta runs every workflow step with a POSIX shell, which on Windows means
# Git Bash. Look for one the same way ciabatta does; if there's none, offer to
# install Git for Windows (winget) or fetch a private PortableGit into
# <install dir>\git, where ciabatta looks first.
$portableGitVersion = "2.56.1"
$portableGitUrl = "https://github.com/git-for-windows/git/releases/download/v$portableGitVersion.windows.1/PortableGit-$portableGitVersion-64-bit.7z.exe"
$portableGitSha256 = "9dd04a608ceb5f6ab32a73709e0ecb159d211da71b697c45ff81f6dd0c6b49c7"

function Find-GitBash([string]$installDir) {
    if ($env:CIABATTA_SHELL) { return $env:CIABATTA_SHELL }
    $candidates = @(Join-Path $installDir "git\bin\bash.exe")
    $git = Get-Command git -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($git) {
        $dir = Split-Path -Parent $git.Source
        foreach ($up in 1..2) {
            $dir = Split-Path -Parent $dir
            if ($dir) { $candidates += Join-Path $dir "bin\bash.exe" }
        }
    }
    foreach ($root in @($env:ProgramFiles, $env:ProgramW6432, ${env:ProgramFiles(x86)})) {
        if ($root) { $candidates += Join-Path $root "Git\bin\bash.exe" }
    }
    if ($env:LOCALAPPDATA) { $candidates += Join-Path $env:LOCALAPPDATA "Programs\Git\bin\bash.exe" }
    foreach ($c in $candidates) {
        if (Test-Path -PathType Leaf $c) { return $c }
    }
    return $null
}

function Install-PortableGit([string]$installDir) {
    $dest = Join-Path $installDir "git"
    $sfx = Join-Path ([System.IO.Path]::GetTempPath()) ("PortableGit-" + [System.Guid]::NewGuid() + ".7z.exe")
    try {
        Write-Host "downloading PortableGit $portableGitVersion (~60 MB) ..."
        Invoke-WebRequest -Uri $portableGitUrl -OutFile $sfx -UseBasicParsing
        $hash = (Get-FileHash -Algorithm SHA256 $sfx).Hash
        if ($hash -ne $portableGitSha256) {
            throw "PortableGit download failed its checksum (got $hash) - not installing it"
        }
        if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
        Write-Host "unpacking into $dest ..."
        $p = Start-Process -FilePath $sfx -ArgumentList @("-o`"$dest`"", "-y") -Wait -PassThru -WindowStyle Hidden
        if ($p.ExitCode -ne 0) { throw "PortableGit failed to unpack (exit $($p.ExitCode))" }
    } finally {
        Remove-Item -Force $sfx -ErrorAction SilentlyContinue
    }
}

function Confirm-GitBash([string]$installDir, [bool]$unattended) {
    $found = Find-GitBash $installDir
    if ($found) {
        Write-Host "shell:     $found"
        return
    }

    Write-Host ""
    Write-Host "ciabatta runs workflow steps with Git Bash, and there's none on this machine."
    $hasWinget = [bool](Get-Command winget -ErrorAction SilentlyContinue)
    $choice = "p"
    if (-not $unattended -and -not [Console]::IsInputRedirected) {
        Write-Host "  [P] download a private PortableGit into $installDir\git (default)"
        if ($hasWinget) { Write-Host "  [W] install Git for Windows with winget" }
        Write-Host "  [S] skip - I'll install Git myself"
        $answer = Read-Host "choice"
        if ($answer -and $answer.Trim()) { $choice = $answer.Trim().Substring(0, 1).ToLower() }
    }

    try {
        switch ($choice) {
            "w" {
                if (-not $hasWinget) { throw "winget isn't available" }
                winget install --id Git.Git -e --source winget
                if ($LASTEXITCODE -ne 0) { throw "winget exited with $LASTEXITCODE" }
            }
            "s" {
                Write-Host "skipped: install Git for Windows (https://git-scm.com/download/win) before running a workflow."
                return
            }
            default { Install-PortableGit $installDir }
        }
    } catch {
        Write-Host "warning: $_"
        Write-Host "ciabatta is installed, but workflows won't run until Git Bash is: install Git for Windows, or rerun this installer."
        return
    }

    $found = Find-GitBash $installDir
    if ($found) {
        Write-Host "shell:     $found"
    } else {
        Write-Host "warning: Git Bash still wasn't found; set CIABATTA_SHELL to the full path of a bash.exe."
    }
}

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

    # `oven` is a shorter name for the same binary. Symlinks need admin rights on
    # Windows, so it's a .cmd shim that forwards to ciabatta.exe beside it. Never
    # replace an oven.cmd that isn't ours.
    $shim = Join-Path $installDir "oven.cmd"
    $shimBody = "@`"%~dp0ciabatta.exe`" %*"
    if (-not (Test-Path $shim)) {
        Set-Content -Path $shim -Value $shimBody -Encoding ASCII
        Write-Host "alias:     $shim"
    } elseif ((Get-Content -Raw $shim).Trim() -ne $shimBody) {
        Write-Host "note: $shim already exists and isn't ciabatta's - left it alone, so no 'oven' alias"
    }
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

Confirm-GitBash $installDir ($Unattended.IsPresent -or [bool]$env:CIABATTA_UNATTENDED)
