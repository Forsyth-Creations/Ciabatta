#!/bin/sh
# Ciabatta installer — downloads the right prebuilt binary for your OS/arch and
# drops it on your PATH. Architecture-agnostic: works on Linux and macOS, on both
# x86_64 and ARM64.
#
#   curl -fsSL https://forsyth-creations.github.io/Ciabatta/install.sh | sh
#
# To pass options through a pipe, `sh` needs `-s --` before them — everything
# after that goes to this script rather than to sh:
#
#   curl -fsSL .../install.sh | sh -s -- --version 0.3.0
#   curl -fsSL .../install.sh | sh -s -- --dir ~/bin
#
# Options:
#   -v, --version VERSION  install this version (e.g. 0.3.0, v0.3.0, or latest)
#   -d, --dir DIR          where to install
#   -l, --list             list the available versions and exit
#   -h, --help             show this and exit
#
# The equivalent environment variables still work, for callers that find them
# easier to set; an explicit flag wins over them:
#   CIABATTA_INSTALL_DIR   where to install (default: /usr/local/bin, else ~/.local/bin)
#   CIABATTA_VERSION       pin a version (default: latest release)
set -eu

REPO="Forsyth-Creations/Ciabatta"
BIN="ciabatta"

say() { printf '%s\n' "$*"; }
err() { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<EOF
Ciabatta installer

  curl -fsSL https://forsyth-creations.github.io/Ciabatta/install.sh | sh
  curl -fsSL https://forsyth-creations.github.io/Ciabatta/install.sh | sh -s -- --version 0.3.0

Options:
  -v, --version VERSION  install this version (e.g. 0.3.0, v0.3.0, or latest)
  -d, --dir DIR          where to install (default: /usr/local/bin, else ~/.local/bin)
  -l, --list             list the available versions and exit
  -h, --help             show this and exit

Note the \`-s --\` when piping into sh: without it, sh reads the flags as its own.
EOF
}

# --- options ---------------------------------------------------------------
# Seeded from the environment so both spellings work; a flag overrides.
version="${CIABATTA_VERSION:-}"
install_dir="${CIABATTA_INSTALL_DIR:-}"
list_only=""

while [ $# -gt 0 ]; do
    case "$1" in
        -v | --version)
            [ $# -ge 2 ] || err "--version needs a value, e.g. --version 0.3.0"
            version="$2"; shift 2 ;;
        --version=*) version="${1#*=}"; shift ;;
        -d | --dir)
            [ $# -ge 2 ] || err "--dir needs a value, e.g. --dir ~/bin"
            install_dir="$2"; shift 2 ;;
        --dir=*) install_dir="${1#*=}"; shift ;;
        -l | --list) list_only=1; shift ;;
        -h | --help) usage; exit 0 ;;
        # A bare version is what people try first; accept it rather than
        # failing on the most natural thing to type.
        v[0-9]* | [0-9]*) version="$1"; shift ;;
        *) err "unknown option '$1'
       Run with --help for the list. When piping into sh, options need
       \`sh -s -- <options>\`." ;;
    esac
done

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

# --- pick a downloader -----------------------------------------------------
if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1" -o "$2"; }
    fetch_stdout() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO "$2" "$1"; }
    fetch_stdout() { wget -qO- "$1"; }
else
    err "need curl or wget to download ciabatta"
fi

# --- list versions ---------------------------------------------------------
# Tag names out of the releases API. Deliberately grep/sed rather than a JSON
# parser: this script must run on a bare container with neither jq nor python.
list_versions() {
    fetch_stdout "https://api.github.com/repos/${REPO}/releases?per_page=100" |
        sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p'
}

if [ -n "$list_only" ]; then
    say "Available versions of ciabatta:"
    versions="$(list_versions || true)"
    if [ -z "$versions" ]; then
        err "couldn't reach the GitHub releases API.
       See https://github.com/${REPO}/releases"
    fi
    printf '%s\n' "$versions" | sed 's/^/  /'
    say ""
    say "Install one with:"
    say "  curl -fsSL https://forsyth-creations.github.io/Ciabatta/install.sh | sh -s -- --version <VERSION>"
    exit 0
fi

# --- resolve download URL --------------------------------------------------
# GitHub serves the newest release's asset from the /latest/ path, so the
# unpinned case needs no API call or JSON parsing.
case "$version" in
    "" | latest | Latest | LATEST)
        version=""
        url="https://github.com/${REPO}/releases/latest/download/${asset}" ;;
    *)
        # Accept "0.3.0" and "v0.3.0" alike; the tags carry the v.
        version="${version#v}"
        case "$version" in
            *[!0-9.]* | "" ) err "'$version' doesn't look like a version.
       Expected something like 0.3.0. Run with --list to see what exists." ;;
        esac
        url="https://github.com/${REPO}/releases/download/v${version}/${asset}" ;;
esac

# --- download + extract to a temp dir --------------------------------------
tmp="$(mktemp -d 2>/dev/null || mktemp -d -t ciabatta)"
trap 'rm -rf "$tmp"' EXIT INT TERM

if [ -n "$version" ]; then
    say "downloading ${asset} (v${version}) …"
else
    say "downloading ${asset} (latest) …"
fi
if ! fetch "$url" "$tmp/$asset" 2>/dev/null; then
    if [ -n "$version" ]; then
        err "no v${version} release for ${os_name}/${arch_name}.
       Run with --list to see the versions that exist:
         curl -fsSL https://forsyth-creations.github.io/Ciabatta/install.sh | sh -s -- --list"
    fi
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

if [ -n "$install_dir" ]; then
    dest="$install_dir"
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
