#!/bin/sh
# Wrap the debug build in an app of its own and start it:
#   scripts/dev-app-macos.sh [FOLDER]     default: this repo
# macOS keeps privacy permissions (Screen Recording, Accessibility, ...) per
# app identity. A bare target/debug/tern-app has an ad-hoc signature, so each
# build is a new app to macOS; and run from a terminal in Tern.app, macOS
# holds Tern.app responsible for what it does. Here it is "Tern Dev"
# (dev.tern.debug), signed with the same certificate as an installed Tern,
# so its permissions are its own and survive rebuilds, and the installed
# Tern's are never touched. Launched with `open`, so it answers for itself.
# Env: TERN_CONTROL (default target/dev.sock), XDG_CONFIG_HOME,
# TERN_SESSION and TERN_FRAME_TRACE are passed on; TERN_DEV_SIGN_ID picks the identity.
# Prints the app's pid. Build first (`cargo build -p tern-app`).
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
FOLDER=$(cd "${1:-$ROOT}" && pwd)
BIN="$ROOT/target/debug/tern-app"
APP="$ROOT/target/debug/Tern Dev.app"
EXE="$APP/Contents/MacOS/tern"
[ -f "$BIN" ] || { echo "error: no $BIN; run cargo build -p tern-app" >&2; exit 1; }

if [ -z "${TERN_DEV_SIGN_ID:-}" ]; then
    TERN_DEV_SIGN_ID=$(security find-identity -v -p codesigning 2>/dev/null |
        sed -n 's/^ *[0-9]*) \([0-9A-F]\{40\}\) "Apple Development: .*/\1/p' | head -1)
fi

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/assets/icon/macos/Tern.icns" "$APP/Contents/Resources/Tern.icns"
# No URL scheme or document types: tern:// and folders stay the installed Tern's.
cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Tern Dev</string>
    <key>CFBundleDisplayName</key><string>Tern Dev</string>
    <key>CFBundleIdentifier</key><string>dev.tern.debug</string>
    <key>CFBundleExecutable</key><string>tern</string>
    <key>CFBundleIconFile</key><string>Tern</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleVersion</key><string>0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
</dict>
</plist>
EOF

# Re-sign only when the build changed (signing a debug binary takes a while).
if ! cmp -s "$BIN" "$APP/Contents/MacOS/.built" 2>/dev/null || ! codesign -v "$APP" 2>/dev/null; then
    cp "$BIN" "$APP/Contents/MacOS/.built"
    cp "$BIN" "$EXE"
    if [ -n "$TERN_DEV_SIGN_ID" ]; then
        codesign --force --sign "$TERN_DEV_SIGN_ID" "$APP"
    else
        echo "warning: no Apple Development identity; ad-hoc signed, so permissions reset on each build" >&2
        codesign --force --sign - "$APP"
    fi
fi

CONTROL=${TERN_CONTROL:-target/dev.sock}
case $CONTROL in /*) ;; *) CONTROL="$ROOT/$CONTROL" ;; esac
rm -f "$CONTROL"
set -- --env "TERN_CONTROL=$CONTROL"
[ -n "${XDG_CONFIG_HOME:-}" ] && set -- "$@" --env "XDG_CONFIG_HOME=$XDG_CONFIG_HOME"
[ -n "${TERN_SESSION:-}" ] && set -- "$@" --env "TERN_SESSION=$TERN_SESSION"
[ -n "${TERN_FRAME_TRACE:-}" ] && set -- "$@" --env "TERN_FRAME_TRACE=$TERN_FRAME_TRACE"
open -n "$@" -a "$APP" --args "$FOLDER"

# `open` doesn't say which process it started; wait for it to show up.
for _ in $(seq 50); do
    pid=$(ps -axo pid=,args= | awk -v cmd="$EXE $FOLDER" '{ pid = $1; sub(/^ *[0-9]+ /, "") } $0 == cmd { print pid }' | tail -1)
    [ -n "$pid" ] && { echo "$pid"; exit 0; }
    sleep 0.1
done
echo "error: Tern Dev didn't start" >&2
exit 1
