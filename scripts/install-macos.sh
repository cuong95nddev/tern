#!/bin/sh
# Build a release Tern.app and install it, plus a `tern` command that opens a
# folder as a workspace in the running Tern (launching it, with the last
# session, if needed).
#   scripts/install-macos.sh [APP_DIR] [BIN_DIR]
# Defaults: /Applications and ~/.local/bin.
# Signs with the first "Apple Development" identity in the keychain, if any,
# so privacy permissions (Accessibility, Screen Recording) survive reinstalls;
# set TERN_DEV_SIGN_ID to pick another, or TERN_SIGN_ID for a release signature.
set -eu

APP_DIR=${1:-/Applications}
BIN_DIR=${2:-"$HOME/.local/bin"}
ROOT=$(cd "$(dirname "$0")/.." && pwd)

if [ -z "${TERN_SIGN_ID:-}" ] && [ -z "${TERN_DEV_SIGN_ID:-}" ]; then
    TERN_DEV_SIGN_ID=$(security find-identity -v -p codesigning 2>/dev/null |
        sed -n 's/^ *[0-9]*) \([0-9A-F]\{40\}\) "Apple Development: .*/\1/p' | head -1)
    export TERN_DEV_SIGN_ID
fi

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
APP=$("$ROOT/scripts/bundle-macos.sh" "$STAGE" | tail -1)

mkdir -p "$APP_DIR"
rm -rf "$APP_DIR/Tern.app"
cp -R "$APP" "$APP_DIR/Tern.app"
# Make Finder and the Dock pick up a changed icon.
touch "$APP_DIR/Tern.app"

mkdir -p "$BIN_DIR"
cat > "$BIN_DIR/tern" <<EOF
#!/bin/sh
# Open a folder (default: the current one) as a workspace in Tern.
dir=\$(cd "\${1:-.}" && pwd) || exit 1
exec open -a "$APP_DIR/Tern.app" "\$dir"
EOF
chmod +x "$BIN_DIR/tern"

echo "Installed $APP_DIR/Tern.app and $BIN_DIR/tern"
