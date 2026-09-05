#!/bin/bash
#
# Builds PairDrop.app from the SwiftPM executable.
#
# SwiftPM produces a bare Mach-O; a menu bar app needs a real bundle for LSUIElement,
# the bundle identifier, and notification entitlements. This assembles one, drops
# WebRTC.framework into Contents/Frameworks, and ad-hoc signs the result.
#
#   ./build.sh              debug build
#   ./build.sh release      release build
#   ./build.sh release universal
#
# VERSION and BUILD_NUMBER override what goes in the bundle's Info.plist; package.sh
# uses them to stamp a release. Without them the checked-in values are used.
set -euo pipefail

cd "$(dirname "$0")"

CONFIG="${1:-debug}"
ARCHS="${2:-native}"
APP_NAME="PairDrop"
EXECUTABLE="PairDropApp"
BUILD_ARGS=(-c "$CONFIG")

if [ "$ARCHS" = "universal" ]; then
    BUILD_ARGS+=(--arch arm64 --arch x86_64)
fi

echo "==> Building ($CONFIG, $ARCHS)"
swift build "${BUILD_ARGS[@]}" \
    -Xlinker -rpath -Xlinker '@executable_path/../Frameworks'

BIN_PATH="$(swift build "${BUILD_ARGS[@]}" --show-bin-path)"
APP="$BIN_PATH/$APP_NAME.app"

echo "==> Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks" "$APP/Contents/Resources"

cp "$BIN_PATH/$EXECUTABLE" "$APP/Contents/MacOS/$EXECUTABLE"
cp Resources/Info.plist "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"

PLB=/usr/libexec/PlistBuddy
if [ -n "${VERSION:-}" ]; then
    "$PLB" -c "Set :CFBundleShortVersionString $VERSION" "$APP/Contents/Info.plist"
fi
if [ -n "${BUILD_NUMBER:-}" ]; then
    "$PLB" -c "Set :CFBundleVersion $BUILD_NUMBER" "$APP/Contents/Info.plist"
fi
echo "==> Version $("$PLB" -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist") ($("$PLB" -c 'Print :CFBundleVersion' "$APP/Contents/Info.plist"))"

if [ ! -f Resources/PairDrop.icns ]; then
    echo "==> Generating icon"
    swift Scripts/make-icon.swift
fi
cp Resources/PairDrop.icns "$APP/Contents/Resources/PairDrop.icns"

# The xcframework lands in .build/artifacts once `swift package resolve` has run.
WEBRTC_FRAMEWORK="$(find .build/artifacts ../shared/PairDropKit/.build/artifacts \
    -type d -name 'WebRTC.framework' -path '*macos*' 2>/dev/null | head -1)"

if [ -z "$WEBRTC_FRAMEWORK" ]; then
    echo "error: WebRTC.framework not found. Run 'swift package resolve' first." >&2
    exit 1
fi

echo "==> Embedding $(basename "$(dirname "$WEBRTC_FRAMEWORK")")/WebRTC.framework"
cp -R "$WEBRTC_FRAMEWORK" "$APP/Contents/Frameworks/"

# The binary already has an rpath into .build; that path won't exist on another machine.
install_name_tool -add_rpath '@executable_path/../Frameworks' \
    "$APP/Contents/MacOS/$EXECUTABLE" 2>/dev/null || true

# Set SIGN_IDENTITY to a Developer ID to produce a distributable, notarizable build.
# The default is an ad-hoc signature, which is fine locally.
IDENTITY="${SIGN_IDENTITY:--}"
SIGN_FLAGS=(--force --sign "$IDENTITY")

if [ "$IDENTITY" = "-" ]; then
    # Ad-hoc signatures can't be timestamped, and skipping it keeps offline builds fast.
    SIGN_FLAGS+=(--timestamp=none)
else
    # The hardened runtime requires every embedded library to carry the *same* Team ID.
    # Ad-hoc signatures have none, so dyld rejects the framework — enable it only for
    # a real identity.
    #
    # The secure timestamp is not optional here: notarization rejects a signature
    # without one ("The signature does not include a secure timestamp").
    SIGN_FLAGS+=(--options runtime --timestamp)
fi

echo "==> Signing as ${IDENTITY}"
# Nested code is signed before the bundle that contains it.
codesign "${SIGN_FLAGS[@]}" "$APP/Contents/Frameworks/WebRTC.framework/Versions/A"
codesign "${SIGN_FLAGS[@]}" --entitlements Resources/PairDrop.entitlements "$APP"

echo "==> Verifying"
codesign --verify --deep --strict "$APP" && echo "signature ok"

echo
echo "Built: $APP"
echo "Run:   open \"$APP\""
