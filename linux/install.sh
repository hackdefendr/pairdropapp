#!/bin/bash
#
# Builds a release binary and installs it for the current user.
#
#   ./install.sh                 → ~/.local (no root needed)
#   ./install.sh --prefix /usr/local
#
# Needs GTK 4 and libadwaita development packages:
#   Debian/Ubuntu/Kali  sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev
#   Fedora              sudo dnf install gcc pkgconf gtk4-devel libadwaita-devel
#   Arch                sudo pacman -S base-devel gtk4 libadwaita
set -euo pipefail

cd "$(dirname "$0")"
PREFIX="${HOME}/.local"

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

for tool in cargo pkg-config; do
    command -v "$tool" >/dev/null || { echo "error: $tool is not installed" >&2; exit 1; }
done
for lib in gtk4 libadwaita-1; do
    pkg-config --exists "$lib" || {
        echo "error: $lib development files are missing — see the header of this script" >&2
        exit 1
    }
done

echo "==> Building"
cargo build --release --package pairdrop-gtk

echo "==> Installing to $PREFIX"
install -Dm755 target/release/pairdrop "$PREFIX/bin/pairdrop"
install -Dm644 crates/pairdrop-gtk/data/app.pairdrop.Linux.desktop \
    "$PREFIX/share/applications/app.pairdrop.Linux.desktop"
install -Dm644 crates/pairdrop-gtk/data/icons/app.pairdrop.Linux.svg \
    "$PREFIX/share/icons/hicolor/scalable/apps/app.pairdrop.Linux.svg"

# So the launcher appears without a re-login.
if command -v update-desktop-database >/dev/null; then
    update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache >/dev/null; then
    gtk-update-icon-cache -qtf "$PREFIX/share/icons/hicolor" 2>/dev/null || true
fi

echo
echo "Installed: $PREFIX/bin/pairdrop"
case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "Note: $PREFIX/bin is not on your PATH." ;;
esac
