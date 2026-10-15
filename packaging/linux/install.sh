#!/bin/sh
# Install Tern for this user: the `tern` command in ~/.local/bin, its menu
# entry and icons under ~/.local/share. Run it from the unpacked archive;
# run it again to update. `./install.sh --uninstall` removes it.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
BIN=${XDG_BIN_HOME:-"$HOME/.local/bin"}
DATA=${XDG_DATA_HOME:-"$HOME/.local/share"}

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$BIN/tern" "$DATA/applications/dev.tern.Tern.desktop"
    rm -f "$DATA"/icons/hicolor/*/apps/dev.tern.Tern.png
    echo "Tern removed."
    exit 0
fi

mkdir -p "$BIN" "$DATA/applications"
# Replace, don't overwrite in place: a running Tern keeps its old file.
cp "$HERE/tern" "$BIN/.tern.new" && mv -f "$BIN/.tern.new" "$BIN/tern"
chmod 755 "$BIN/tern"
sed "s|^Exec=tern|Exec=$BIN/tern|" "$HERE/dev.tern.Tern.desktop" > "$DATA/applications/dev.tern.Tern.desktop"
for png in "$HERE"/icons/tern-*.png; do
    s=$(basename "$png" .png); s=${s#tern-}
    mkdir -p "$DATA/icons/hicolor/${s}x${s}/apps"
    cp "$png" "$DATA/icons/hicolor/${s}x${s}/apps/dev.tern.Tern.png"
done
command -v update-desktop-database >/dev/null && update-desktop-database "$DATA/applications" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q "$DATA/icons/hicolor" 2>/dev/null || true

echo "Tern installed: $BIN/tern"
case ":$PATH:" in *":$BIN:"*) ;; *) echo "Add $BIN to PATH to run \`tern <folder>\` from a shell." ;; esac
