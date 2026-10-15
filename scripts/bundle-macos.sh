#!/bin/sh
# Build a release Tern.app into OUT_DIR (default: target/bundle).
#   scripts/bundle-macos.sh [OUT_DIR]
# TERN_UNIVERSAL=1 builds arm64 and x86_64 and joins them into one binary
# (needs `rustup target add aarch64-apple-darwin x86_64-apple-darwin`).
# TERN_SIGN_ID signs it for distribution (see below).
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:-"$ROOT/target/bundle"}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
APP="$STAGE/Tern.app"
# The menu bar agent: a helper app of its own (own bundle id, no Dock icon),
# so a running agent isn't taken for Tern by LaunchServices.
HELPER="$APP/Contents/Helpers/Tern Menu Bar.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$HELPER/Contents/MacOS"

if [ "${TERN_UNIVERSAL:-}" = 1 ]; then
    for t in aarch64-apple-darwin x86_64-apple-darwin; do
        cargo build --release -p tern-app -p tern-menubar --target "$t" --manifest-path "$ROOT/Cargo.toml"
    done
    lipo -create -output "$APP/Contents/MacOS/tern" \
        "$ROOT/target/aarch64-apple-darwin/release/tern-app" \
        "$ROOT/target/x86_64-apple-darwin/release/tern-app"
    lipo -create -output "$HELPER/Contents/MacOS/tern-menubar" \
        "$ROOT/target/aarch64-apple-darwin/release/tern-menubar" \
        "$ROOT/target/x86_64-apple-darwin/release/tern-menubar"
else
    cargo build --release -p tern-app -p tern-menubar --manifest-path "$ROOT/Cargo.toml"
    cp "$ROOT/target/release/tern-app" "$APP/Contents/MacOS/tern"
    cp "$ROOT/target/release/tern-menubar" "$HELPER/Contents/MacOS/tern-menubar"
fi

# Icons come from assets/icon/source.png via scripts/gen-icons.sh. With
# Xcode, compile the Icon Composer icon (Assets.car, native on macOS 26+,
# plus a matching .icns for older systems); else (or if this Xcode can't
# read .icon) ship the plain .icns. TERN_ICON_XCODE (an Xcode.app) runs
# actool from that Xcode only, e.g. Xcode 26 on a runner whose default is
# older; TERN_REQUIRE_ICON=1 fails instead of falling back.
ICON_DEV=${TERN_ICON_XCODE:+"$TERN_ICON_XCODE/Contents/Developer"}
if ! { env ${ICON_DEV:+DEVELOPER_DIR="$ICON_DEV"} xcrun --find actool >"$STAGE/actool.log" 2>&1 &&
    env ${ICON_DEV:+DEVELOPER_DIR="$ICON_DEV"} xcrun actool "$ROOT/assets/icon/macos/Tern.icon" \
        --compile "$APP/Contents/Resources" \
        --app-icon Tern --platform macosx --target-device mac --minimum-deployment-target 12.0 \
        --output-partial-info-plist "$STAGE/icon.plist" --output-format human-readable-text \
        >>"$STAGE/actool.log" 2>&1 &&
    [ -f "$APP/Contents/Resources/Assets.car" ] && [ -f "$APP/Contents/Resources/Tern.icns" ]; }; then
    if [ "${TERN_REQUIRE_ICON:-}" = 1 ]; then
        echo "error: actool couldn't compile assets/icon/macos/Tern.icon (needs Xcode 26+):" >&2
        cat "$STAGE/actool.log" >&2
        exit 1
    fi
    cp "$ROOT/assets/icon/macos/Tern.icns" "$APP/Contents/Resources/Tern.icns"
fi
cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Tern</string>
    <key>CFBundleDisplayName</key><string>Tern</string>
    <key>CFBundleIdentifier</key><string>dev.tern</string>
    <key>CFBundleExecutable</key><string>tern</string>
    <key>CFBundleIconFile</key><string>Tern</string>
    <key>CFBundleIconName</key><string>Tern</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key><string>Folder</string>
            <key>CFBundleTypeRole</key><string>Viewer</string>
            <key>LSHandlerRank</key><string>None</string>
            <key>LSItemContentTypes</key><array><string>public.folder</string></array>
        </dict>
    </array>
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key><string>dev.tern.session</string>
            <key>CFBundleURLSchemes</key><array><string>tern</string></array>
        </dict>
    </array>
</dict>
</plist>
EOF
cat > "$HELPER/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Tern Menu Bar</string>
    <key>CFBundleIdentifier</key><string>dev.tern.menubar</string>
    <key>CFBundleExecutable</key><string>tern-menubar</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>LSUIElement</key><true/>
</dict>
</plist>
EOF
# With TERN_SIGN_ID (a "Developer ID Application: ..." identity) sign for
# distribution: hardened runtime and a secure timestamp, as notarization
# requires. Else an ad-hoc signature, so Gatekeeper and notifications treat
# it as one app. The helper is signed first: the outer signature seals it.
if [ -n "${TERN_SIGN_ID:-}" ]; then
    codesign --force --options runtime --timestamp --sign "$TERN_SIGN_ID" "$HELPER"
    codesign --force --options runtime --timestamp --sign "$TERN_SIGN_ID" "$APP"
else
    codesign --force --sign - "$HELPER" >/dev/null 2>&1 || true
    codesign --force --sign - "$APP" >/dev/null 2>&1 || true
fi

mkdir -p "$OUT"
rm -rf "$OUT/Tern.app"
cp -R "$APP" "$OUT/Tern.app"
echo "$OUT/Tern.app"
