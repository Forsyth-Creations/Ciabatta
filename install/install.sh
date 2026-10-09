#!/usr/bin/env bash
# Installs ciabatta to a directory on your PATH.
# Usage: ./install.sh [INSTALL_DIR]   (default: /usr/local/bin)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
INSTALL_DIR="${1:-/usr/local/bin}"

if [ ! -d "$INSTALL_DIR" ]; then
    echo "error: install directory does not exist: $INSTALL_DIR" >&2
    exit 1
fi

if [ -w "$INSTALL_DIR" ]; then
    install -m 755 "$SCRIPT_DIR/ciabatta" "$INSTALL_DIR/ciabatta"
else
    echo "Note: $INSTALL_DIR is not writable by the current user — trying sudo"
    sudo install -m 755 "$SCRIPT_DIR/ciabatta" "$INSTALL_DIR/ciabatta"
fi

echo "installed: $INSTALL_DIR/ciabatta"

# `oven` is a shorter name for the same binary: a relative symlink, so it
# follows ciabatta through later updates. Never replace an `oven` that isn't ours.
LINK="$INSTALL_DIR/oven"
if [ -L "$LINK" ] && [ "$(readlink "$LINK")" = "ciabatta" ]; then
    :
elif [ -e "$LINK" ] || [ -L "$LINK" ]; then
    echo "note: $LINK already exists and isn't ciabatta — left it alone, so no 'oven' alias"
elif [ -w "$INSTALL_DIR" ]; then
    ln -s ciabatta "$LINK" && echo "alias:     $LINK -> ciabatta"
else
    sudo ln -s ciabatta "$LINK" && echo "alias:     $LINK -> ciabatta"
fi
