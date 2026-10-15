#!/bin/sh
# Build a release Tern.app and install it, plus a `tern` command that opens a
# folder as a workspace in the running Tern (launching it, with the last
# session, if needed).
#   scripts/install-macos.sh [APP_DIR] [BIN_DIR]
# Defaults: /Applications and ~/.local/bin.
set -eu

APP_DIR=${1:-/Applications}
BIN_DIR=${2:-"$HOME/.local/bin"}
ROOT=$(cd "$(dirname "$0")/.." && pwd)

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
