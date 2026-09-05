#!/bin/bash
#
# Builds a release PairDrop.app and installs it to /Applications.
#
#   ./install.sh                 universal release build, install, launch
#   ./install.sh --no-launch     install without starting it
#   ./install.sh --to ~/Applications
#
# Pass SIGN_IDENTITY to sign with a Developer ID instead of ad-hoc; see build.sh.
set -euo pipefail

cd "$(dirname "$0")"

DEST_DIR="/Applications"
LAUNCH=1

while [ $# -gt 0 ]; do
    case "$1" in
        --to) DEST_DIR="${2:?--to needs a directory}"; shift 2 ;;
        --no-launch) LAUNCH=0; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

DEST_DIR="${DEST_DIR/#\~/$HOME}"
APP_NAME="PairDrop"
DEST="$DEST_DIR/$APP_NAME.app"

./build.sh release universal

BIN_PATH="$(swift build -c release --arch arm64 --arch x86_64 --show-bin-path)"
BUILT="$BIN_PATH/$APP_NAME.app"

if [ ! -d "$BUILT" ]; then
    echo "error: build did not produce $BUILT" >&2
    exit 1
fi

# A running copy can't be replaced cleanly.
if pgrep -f "$DEST/Contents/MacOS/PairDropApp" >/dev/null 2>&1; then
    echo "==> Quitting the running copy"
    pkill -f "$DEST/Contents/MacOS/PairDropApp" || true
    sleep 1
fi

echo "==> Installing to $DEST"
mkdir -p "$DEST_DIR"

if [ -d "$DEST" ] && [ ! -w "$DEST" ]; then
    echo "    (needs admin to replace the existing copy)"
    sudo rm -rf "$DEST"
    sudo cp -R "$BUILT" "$DEST"
    sudo chown -R "$(id -u):$(id -g)" "$DEST"
elif [ ! -w "$DEST_DIR" ]; then
    echo "    (needs admin to write to $DEST_DIR)"
    sudo cp -R "$BUILT" "$DEST"
    sudo chown -R "$(id -u):$(id -g)" "$DEST"
else
    rm -rf "$DEST"
    cp -R "$BUILT" "$DEST"
fi

# Locally built code is never quarantined, but a copy that has travelled might be.
# `xattr` has no recursive flag on macOS 26, so walk it.
find "$DEST" -exec xattr -d com.apple.quarantine {} \; 2>/dev/null || true

# Nudge Launch Services so the icon and name appear straight away.
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
    -f "$DEST" >/dev/null 2>&1 || true

echo "==> Verifying"
codesign --verify --deep --strict "$DEST" && echo "    signature ok"
echo "    $(lipo -archs "$DEST/Contents/MacOS/PairDropApp" 2>/dev/null || echo "?")"

if [ "$LAUNCH" -eq 1 ]; then
    echo "==> Launching"
    open "$DEST"
fi

echo
echo "Installed: $DEST"
echo "PairDrop lives in the menu bar — look for the paper plane."
