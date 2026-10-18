#!/bin/sh
# Dev loop: run a debug Tern next to the installed one, and rebuild and
# relaunch it whenever a source file changes (a failed build keeps the old
# one running). On macOS it runs as "Tern Dev" (scripts/dev-app-macos.sh);
# on Windows (Git Bash) it runs a copy, as a running .exe can't be relinked.
# It leaves the installed app's saved session alone, and its control socket
# is target/dev.sock (Unix only, see crates/tern-app/src/control.rs).
#   scripts/dev.sh [FOLDER]     default: this repo
set -u

ROOT=$(cd "$(dirname "$0")/.." && pwd)
FOLDER=$(cd "${1:-$ROOT}" && pwd)
cd "$ROOT"

# Newest mtime of the sources and manifests.
# GNU stat (Linux, Git Bash) takes -c, BSD stat (macOS) -f.
if stat -c %Y . >/dev/null 2>&1; then MTIME='-c %Y'; else MTIME='-f %m'; fi
stamp() { find crates \( -name '*.rs' -o -name Cargo.toml \) -exec stat $MTIME {} + | sort -n | tail -1; }

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
            case $(uname) in
            Darwin)
                # As "Tern Dev", with privacy permissions of its own.
                PID=$(scripts/dev-app-macos.sh "$FOLDER") || PID=
                ;;
            MINGW* | MSYS* | CYGWIN*)
                # From a copy: the next build relinks tern-app.exe.
                # A tern-dev.exe still running (e.g. left by an earlier
                # dev.sh) locks the copy: say so rather than run the old one.
                if cp target/debug/tern-app.exe target/debug/tern-dev.exe; then
                    target/debug/tern-dev.exe "$FOLDER" &
                    PID=$!
                else
                    echo "dev: can't replace tern-dev.exe: is another one running?"
                    PID=
                fi
                ;;
            *)
                # A relative path: socket paths are limited to ~100 bytes.
                TERN_CONTROL=target/dev.sock target/debug/tern-app "$FOLDER" &
                PID=$!
                ;;
            esac
            echo "dev: Tern running (pid $PID)"
        else
            echo "dev: build failed, keeping the running Tern"
        fi
    fi
    sleep 1
done
