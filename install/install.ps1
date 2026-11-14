# Installs ciabatta to a directory on your PATH.
# Usage: .\install.ps1 [-InstallDir <path>] [-Unattended]
#   -InstallDir   where to install (default: %LOCALAPPDATA%\Programs\ciabatta)
#   -Unattended   don't ask anything; if there's no Git Bash, download PortableGit
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA "Programs\ciabatta"),
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

# --- ciabatta -----------------------------------------------------------------
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

Confirm-GitBash $InstallDir ($Unattended.IsPresent -or [bool]$env:CIABATTA_UNATTENDED)
