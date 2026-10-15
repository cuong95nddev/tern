#!/bin/sh
# Build a universal Tern.app and pack it in a DMG (with an Applications link
# to drag it onto) for a GitHub release.
#   scripts/release-macos.sh [OUT_DIR]      default: target/dist
# Prints the DMG's path.
# For a DMG that opens without Gatekeeper warnings, set TERN_SIGN_ID to a
# "Developer ID Application: ..." identity and TERN_NOTARY_PROFILE to a
# notarytool keychain profile (`xcrun notarytool store-credentials`); the DMG
# is then signed, notarized and stapled (TERN_NOTARY_KEYCHAIN: the keychain
# holding that profile, if not the default one).
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:-"$ROOT/target/dist"}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
TERN_UNIVERSAL=1 "$ROOT/scripts/bundle-macos.sh" "$STAGE/dmg" >/dev/null
ln -s /Applications "$STAGE/dmg/Applications"

mkdir -p "$OUT"
DMG="$OUT/Tern-$VERSION-macos-universal.dmg"
rm -f "$DMG"
hdiutil create -volname "Tern $VERSION" -srcfolder "$STAGE/dmg" -fs HFS+ -format UDZO "$DMG" >/dev/null
if [ -n "${TERN_SIGN_ID:-}" ]; then
    codesign --force --timestamp --sign "$TERN_SIGN_ID" "$DMG"
fi
if [ -n "${TERN_NOTARY_PROFILE:-}" ]; then
    xcrun notarytool submit "$DMG" --keychain-profile "$TERN_NOTARY_PROFILE" \
        ${TERN_NOTARY_KEYCHAIN:+--keychain "$TERN_NOTARY_KEYCHAIN"} --wait >&2
    xcrun stapler staple "$DMG" >&2
fi
echo "$DMG"
