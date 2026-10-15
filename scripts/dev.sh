#!/bin/sh
# Dev loop: run a debug Tern next to the installed one, and rebuild and
# relaunch it whenever a source file changes (a failed build keeps the old
# one running). It leaves the installed app's saved session alone, and its
# control socket is target/dev.sock (see crates/tern-app/src/control.rs).
#   scripts/dev.sh [FOLDER]     default: this repo
set -u

ROOT=$(cd "$(dirname "$0")/.." && pwd)
FOLDER=$(cd "${1:-$ROOT}" && pwd)
cd "$ROOT"

# Newest mtime of the sources and manifests.
stamp() { find crates \( -name '*.rs' -o -name Cargo.toml \) -exec stat -f %m {} + | sort -n | tail -1; }

PID=
stop() {
    [ -n "$PID" ] && kill "$PID" 2>/dev/null && wait "$PID" 2>/dev/null
    PID=
}
trap 'stop; exit 0' INT TERM

last=
while :; do
    now=$(stamp)
    if [ "$now" != "$last" ]; then
        last=$now
        if cargo build -p tern-app; then
            stop
            # A relative path: socket paths are limited to ~100 bytes.
            TERN_CONTROL=target/dev.sock target/debug/tern-app "$FOLDER" &
            PID=$!
            echo "dev: Tern running (pid $PID)"
        else
            echo "dev: build failed, keeping the running Tern"
        fi
    fi
    sleep 1
done
