#!/bin/bash
#
# Builds and installs the Flatpak for the current user.
#
#   ./flatpak/build.sh              build and install
#   ./flatpak/build.sh --bundle     also write pairdrop.flatpak, a single shareable file
#
# The runtimes it needs are installed on first run, which is a couple of gigabytes.
# flatpak-builder picks the right freedesktop base for the GNOME runtime itself — GNOME
# 50 sits on 25.08 — so don't second-guess the rust extension version.
set -euo pipefail

cd "$(dirname "$0")/.."
APP_ID="app.pairdrop.Linux"
BUNDLE=0
[ "${1:-}" = "--bundle" ] && BUNDLE=1

command -v flatpak-builder >/dev/null || {
    echo "error: flatpak-builder is not installed" >&2
    exit 1
}

# --install-deps-from needs the remote in the *user* installation, even when the
# runtimes themselves are already installed system-wide.
flatpak remote-add --if-not-exists --user \
    flathub https://dl.flathub.org/repo/flathub.flatpakrepo

echo "==> Building"
flatpak-builder --force-clean --user --install-deps-from=flathub \
    --repo=.flatpak-repo --install build-dir "flatpak/$APP_ID.yml"

if [ "$BUNDLE" -eq 1 ]; then
    echo "==> Bundling"
    flatpak build-bundle .flatpak-repo pairdrop.flatpak "$APP_ID" --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo
    echo "    wrote $(pwd)/pairdrop.flatpak"
fi

echo
echo "Installed. Run it with:  flatpak run $APP_ID"
