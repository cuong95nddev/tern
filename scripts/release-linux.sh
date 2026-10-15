#!/bin/sh
# Build a release tern binary and pack it with its desktop entry, icons and
# install script for a GitHub release.
#   scripts/release-linux.sh [OUT_DIR]      default: target/dist
# Prints the archive's path. Needs the GPUI build dependencies (see README).
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:-"$ROOT/target/dist"}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
ARCH=$(uname -m)

cargo build --release -p tern-app --manifest-path "$ROOT/Cargo.toml" >&2

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
DIR="$STAGE/Tern-$VERSION"
mkdir -p "$DIR/icons"
cp "$ROOT/target/release/tern-app" "$DIR/tern"
cp "$ROOT/packaging/linux/dev.tern.Tern.desktop" "$ROOT/packaging/linux/install.sh" "$ROOT/LICENSE" "$DIR/"
cp "$ROOT"/assets/icon/linux/tern-*.png "$DIR/icons/"

mkdir -p "$OUT"
TAR="$(cd "$OUT" && pwd)/Tern-$VERSION-linux-$ARCH.tar.gz"
tar -C "$STAGE" -czf "$TAR" "Tern-$VERSION"
echo "$TAR"
