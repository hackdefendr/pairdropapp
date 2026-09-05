#!/bin/bash
#
# Builds the release artifacts that get attached to a GitHub release:
#
#   dist/PairDrop-<version>-macOS-universal.dmg    what most people download
#   dist/PairDrop-<version>-macOS-universal.zip    for scripted installs
#   dist/SHA256SUMS                                checksums for both
#
#   ./package.sh              version from Resources/Info.plist
#   ./package.sh 0.1.0        stamp this version into the bundle
#
# Signing and notarization are opt-in, and both are needed for a download that
# opens without the user clearing quarantine by hand:
#
#   SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" \
#   NOTARY_PROFILE=pairdrop ./package.sh 0.1.0
#
# NOTARY_PROFILE names a keychain profile made with:
#   xcrun notarytool store-credentials pairdrop --apple-id … --team-id … --password …
set -euo pipefail

cd "$(dirname "$0")"

APP_NAME="PairDrop"
VOLUME_NAME="PairDrop"
DIST="dist"
PLB=/usr/libexec/PlistBuddy

VERSION="${1:-$("$PLB" -c 'Print :CFBundleShortVersionString' Resources/Info.plist)}"
# A monotonic build number keeps macOS from serving a stale cached bundle when two
# releases share a marketing version.
BUILD_NUMBER="${BUILD_NUMBER:-$(date -u +%Y%m%d%H%M)}"
IDENTITY="${SIGN_IDENTITY:--}"

export VERSION BUILD_NUMBER

STEM="$APP_NAME-$VERSION-macOS-universal"
DMG="$DIST/$STEM.dmg"
ZIP="$DIST/$STEM.zip"

echo "==> Packaging $APP_NAME $VERSION (build $BUILD_NUMBER)"

./build.sh release universal

BIN_PATH="$(swift build -c release --arch arm64 --arch x86_64 --show-bin-path)"
APP="$BIN_PATH/$APP_NAME.app"
[ -d "$APP" ] || { echo "error: build did not produce $APP" >&2; exit 1; }

rm -rf "$DIST"
mkdir -p "$DIST"

# --- Notarize the app before it goes into the disk image ------------------------
#
# The DMG is notarized too, but stapling the app itself means the copy that ends up
# in /Applications carries its own ticket and launches even offline.
notarize() {
    local target="$1"
    [ -n "${NOTARY_PROFILE:-}" ] || return 0

    echo "==> Notarizing $(basename "$target")"
    local upload="$target"
    if [ -d "$target" ]; then
        upload="$DIST/.notarize-$(basename "$target").zip"
        ditto -c -k --keepParent "$target" "$upload"
    fi

    xcrun notarytool submit "$upload" --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$target"
    [ "$upload" = "$target" ] || rm -f "$upload"
}

if [ "$IDENTITY" = "-" ]; then
    echo "==> Ad-hoc signed: downloads will be quarantined (see README)"
else
    notarize "$APP"
fi

# --- Disk image -----------------------------------------------------------------

echo "==> Building $DMG"
STAGE="$DIST/.stage"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/$APP_NAME.app"
ln -s /Applications "$STAGE/Applications"

hdiutil create \
    -volname "$VOLUME_NAME" \
    -srcfolder "$STAGE" \
    -fs HFS+ \
    -format UDZO \
    -quiet -ov \
    "$DMG"
rm -rf "$STAGE"

if [ "$IDENTITY" != "-" ]; then
    codesign --force --sign "$IDENTITY" --timestamp "$DMG"
    notarize "$DMG"
fi

# --- Zip ------------------------------------------------------------------------
#
# ditto, not `zip`: it preserves the code signature and the symlinks inside the
# framework, both of which a plain zip mangles.

echo "==> Building $ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"

# --- Checksums ------------------------------------------------------------------

( cd "$DIST" && shasum -a 256 "$STEM.dmg" "$STEM.zip" > SHA256SUMS )

echo
echo "==> Verifying"
codesign --verify --deep --strict "$APP" && echo "    signature ok ($IDENTITY)"
echo "    architectures: $(lipo -archs "$APP/Contents/MacOS/PairDropApp")"
if [ -n "${NOTARY_PROFILE:-}" ]; then
    spctl --assess --type execute --verbose=2 "$APP" 2>&1 | sed 's/^/    /'
fi

echo
echo "Artifacts in $(pwd)/$DIST:"
ls -lh "$DIST" | tail -n +2 | sed 's/^/  /'
echo
cat "$DIST/SHA256SUMS" | sed 's/^/  /'
echo
echo "Attach all three to the GitHub release."
