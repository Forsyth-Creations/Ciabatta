#!/bin/sh
# Ciabatta installer — downloads the right prebuilt binary for your OS/arch and
# drops it on your PATH. Architecture-agnostic: works on Linux and macOS, on both
# x86_64 and ARM64.
#
#   curl -fsSL https://forsyth-creations.github.io/Ciabatta/install.sh | sh
#
# Options (environment variables):
#   CIABATTA_INSTALL_DIR   where to install (default: /usr/local/bin, else ~/.local/bin)
#   CIABATTA_VERSION       pin a version, e.g. 0.1.15 (default: latest release)
set -eu

REPO="Forsyth-Creations/Ciabatta"
BIN="ciabatta"

say() { printf '%s\n' "$*"; }
err() { printf 'error: %s\n' "$*" >&2; exit 1; }

# --- detect OS -------------------------------------------------------------
os="$(uname -s)"
case "$os" in
    Linux)  os_name="linux" ;;
    Darwin) os_name="macos" ;;
    *) err "unsupported OS '$os'. On Windows, use the PowerShell installer:
       irm https://forsyth-creations.github.io/Ciabatta/install.ps1 | iex" ;;
esac

# --- detect architecture ---------------------------------------------------
arch="$(uname -m)"
case "$arch" in
    x86_64 | amd64)          arch_name="x86_64" ;;
    aarch64 | arm64)         arch_name="aarch64" ;;
    *) err "unsupported architecture '$arch' (need x86_64 or arm64)" ;;
esac

asset="${BIN}-${os_name}-${arch_name}.tar.gz"

# --- resolve download URL --------------------------------------------------
# GitHub serves the newest release's asset from the /latest/ path, so no API
# call or JSON parsing is needed to always fetch the current version.
if [ -n "${CIABATTA_VERSION:-}" ]; then
    version="${CIABATTA_VERSION#v}"
    url="https://github.com/${REPO}/releases/download/v${version}/${asset}"
else
    url="https://github.com/${REPO}/releases/latest/download/${asset}"
fi

# --- pick a downloader -----------------------------------------------------
if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO "$2" "$1"; }
else
    err "need curl or wget to download ciabatta"
fi

# --- download + extract to a temp dir --------------------------------------
tmp="$(mktemp -d 2>/dev/null || mktemp -d -t ciabatta)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading ${asset} …"
if ! fetch "$url" "$tmp/$asset"; then
    err "download failed: $url
       (no release asset for ${os_name}/${arch_name}? check https://github.com/${REPO}/releases)"
fi

tar -xzf "$tmp/$asset" -C "$tmp" || err "failed to extract $asset"
[ -f "$tmp/$BIN" ] || err "archive did not contain the '$BIN' binary"
chmod +x "$tmp/$BIN"

# --- choose an install directory -------------------------------------------
# Prefer a system dir on PATH; fall back to a per-user dir if we can't write
# there (and can't sudo), so the install never needs to fail for permissions.
install_to() {
    dir="$1"
    mkdir -p "$dir" 2>/dev/null || return 1
    if [ -w "$dir" ]; then
        mv -f "$tmp/$BIN" "$dir/$BIN"
    else
        return 1
    fi
}

sudo_install_to() {
    dir="$1"
    command -v sudo >/dev/null 2>&1 || return 1
    say "installing to $dir (needs sudo) …"
    sudo mkdir -p "$dir" && sudo mv -f "$tmp/$BIN" "$dir/$BIN" && sudo chmod 755 "$dir/$BIN"
}

# If ciabatta is already installed on PATH, update that copy in place (unless
# the user pinned a directory) so we don't leave a stale binary shadowing the
# new one from a different location.
existing_dir=""
if command -v "$BIN" >/dev/null 2>&1; then
    existing_dir="$(CDPATH= cd -- "$(dirname -- "$(command -v "$BIN")")" && pwd)"
fi

if [ -n "${CIABATTA_INSTALL_DIR:-}" ]; then
    dest="$CIABATTA_INSTALL_DIR"
    install_to "$dest" || sudo_install_to "$dest" || err "cannot write to $dest"
elif [ -n "$existing_dir" ]; then
    dest="$existing_dir"
    say "updating existing install at $dest …"
    install_to "$dest" || sudo_install_to "$dest" || err "cannot update $dest"
else
    dest="/usr/local/bin"
    if install_to "$dest" || sudo_install_to "$dest"; then
        :
    else
        dest="$HOME/.local/bin"
        say "no access to /usr/local/bin — installing to $dest instead"
        install_to "$dest" || err "cannot write to $dest"
    fi
fi

say "installed: $dest/$BIN"

# --- PATH hint -------------------------------------------------------------
case ":${PATH}:" in
    *":$dest:"*) ;;
    *) say ""
       say "note: $dest is not on your PATH. Add it, e.g.:"
       say "  echo 'export PATH=\"$dest:\$PATH\"' >> ~/.profile && . ~/.profile" ;;
esac

say ""
"$dest/$BIN" --version 2>/dev/null || say "run 'ciabatta --help' to get started"
