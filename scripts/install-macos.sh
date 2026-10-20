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

# macOS keeps privacy permissions per signing identity: a different one
# (say Developer ID after Apple Development) means granting them again.
# Turning a permission back on in System Settings keeps the old identity's
# record, so it only holds until the next build: reset it, then grant anew.
req() { codesign -d -r- "$1" 2>/dev/null | sed -n 's/^designated => //p'; }
OLD_REQ=$(req "$APP_DIR/Tern.app")
if [ -n "$OLD_REQ" ] && [ "$OLD_REQ" != "$(req "$APP")" ]; then
    echo "warning: signed by a different identity than the installed Tern; macOS will ask for its permissions again" >&2
    echo "  clear the old grants first, or they never stick:" >&2
    echo "  for s in ScreenCapture Accessibility SystemPolicyAllFiles; do tccutil reset \$s dev.tern; done" >&2
fi

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

# A running Tern, and above all its background daemon, keep running the old
# code (the daemon even outlives Tern). Quit both and open the new one; tabs
# come back (Claude Code ones resume their conversation). Detached, since
# this script may run in one of the terminals that end. TERN_KEEP_RUNNING=1
# skips it.
# Not pgrep: it skips its own ancestors, and the daemon may be one.
RESTART='
exe=$1
# Pids of $exe run as the UI (no arguments or a folder) or as "server".
pids() {
    ps -axo pid=,args= | awk -v exe="$exe" -v kind="$1" "{
        pid = \$1; \$1 = \"\"; sub(/^ /, \"\")
        if (index(\$0, exe) != 1) next
        rest = substr(\$0, length(exe) + 1)
        if (kind == \"server\" ? rest == \" --server\" : (rest == \"\" || rest ~ /^ [^-]/)) print pid
    }"
}
[ "$2" = check ] && { [ -n "$(pids ui)$(pids server)" ]; exit; }
if [ -n "$(pids ui)" ]; then
    osascript -e "quit app \"Tern\"" || true
    for _ in $(seq 50); do [ -n "$(pids ui)" ] || break; sleep 0.2; done
    reopen=1
fi
p=$(pids server); [ -n "$p" ] && kill -TERM $p
for _ in $(seq 25); do [ -n "$(pids server)" ] || break; sleep 0.2; done
p=$(pids server); [ -n "$p" ] && kill -KILL $p
[ -n "${reopen:-}" ] && open "$2"
'
EXE="$APP_DIR/Tern.app/Contents/MacOS/tern"
if [ -z "${TERN_KEEP_RUNNING:-}" ] && sh -c "$RESTART" sh "$EXE" check; then
    echo "Restarting Tern and its background terminals…"
    nohup sh -c "$RESTART" sh "$EXE" "$APP_DIR/Tern.app" </dev/null >/dev/null 2>&1 &
fi
