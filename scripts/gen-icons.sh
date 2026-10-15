#!/bin/sh
# Generate every app icon size from assets/icon/source.png (a macOS-style
# rounded square on transparency). Needs ImageMagick 7 (`magick`) and, for
# the .icns, macOS `iconutil`.
#   scripts/gen-icons.sh
# Outputs under assets/icon/: macos/ (Tern.icns, Tern.icon), ios/ (AppIcon.appiconset),
# android/ (res/ for an Android project, plus the Play Store icon), web/.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIR="$ROOT/assets/icon"
SRC="$DIR/source.png"
# The tile color, used to fill full-bleed icons and adaptive backgrounds.
BG="#0b0b0b"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# The rounded tile, cropped to its edges.
magick "$SRC" -trim +repage "$TMP/tile.png"
# macOS master: tile at Apple's 824/1024 grid, centered on 1024.
magick "$TMP/tile.png" -resize 824x824 -background none -gravity center -extent 1024x1024 "$TMP/mac.png"
# The glyph alone on transparency, in the tile's frame: brightness becomes
# alpha (so the motion trails fade out as on the tile), and the tile's light
# rim is masked off.
magick "$TMP/tile.png" -resize 1024x1024! -background "$BG" -flatten "$TMP/tile-flat.png"
magick -size 1024x1024 xc:black -fill white -draw "rectangle 40,200 983,860" -blur 0x8 "$TMP/inner.png"
magick "$TMP/tile-flat.png" \( +clone -colorspace gray -level 8%,85% "$TMP/inner.png" -compose multiply -composite \) \
    -alpha off -compose copy-opacity -composite "$TMP/glyph.png"
# Full-bleed square (iOS, Play Store): the glyph on the tile color.
magick -size 1024x1024 xc:"$BG" "$TMP/glyph.png" -compose over -composite -alpha off "$TMP/full.png"
magick "$TMP/glyph.png" -fill white -colorize 100 "$TMP/glyph-white.png"

png() { # png SRC SIZE OUT
    mkdir -p "$(dirname "$3")"
    magick "$1" -resize "$2x$2" -strip "$3"
}
# On a SIZE canvas, the glyph scaled to SCALE (fraction) of it, centered.
padded() { # padded SRC SIZE SCALE BACKGROUND OUT
    inner=$(awk "BEGIN { printf \"%d\", $2 * $3 }")
    mkdir -p "$(dirname "$5")"
    magick "$1" -resize "${inner}x${inner}" -background "$4" -gravity center -extent "$2x$2" -strip "$5"
}

# macOS
rm -rf "$DIR/macos" && mkdir -p "$DIR/macos"
ICONSET="$TMP/Tern.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
    png "$TMP/mac.png" "$s" "$ICONSET/icon_${s}x${s}.png"
    png "$TMP/mac.png" $((s * 2)) "$ICONSET/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$ICONSET" -o "$DIR/macos/Tern.icns"
cp "$TMP/mac.png" "$DIR/macos/icon-1024.png"
# Icon Composer source for macOS 26+, which shrinks plain .icns artwork that
# isn't the exact system shape into a gray tile. The system draws the shape
# and glass; this supplies the fill and the glyph. scripts/install-macos.sh
# compiles it with actool.
mkdir -p "$DIR/macos/Tern.icon/Assets"
cp "$TMP/glyph.png" "$DIR/macos/Tern.icon/Assets/glyph.png"
fill=$(magick xc:"$BG" -format "%[fx:r],%[fx:g],%[fx:b]" info: | awk -F, '{ printf "%.5f,%.5f,%.5f", $1, $2, $3 }')
cat > "$DIR/macos/Tern.icon/icon.json" <<JSON
{
  "fill" : { "solid" : "srgb:$fill,1.00000" },
  "groups" : [
    {
      "layers" : [ { "image-name" : "glyph.png", "name" : "glyph" } ],
      "shadow" : { "kind" : "neutral", "opacity" : 0.5 },
      "translucency" : { "enabled" : false, "value" : 0.5 }
    }
  ],
  "supported-platforms" : { "circles" : [ "watchOS" ], "squares" : "shared" }
}
JSON

# iOS: single 1024 sizes (Xcode 14+), with the iOS 18 dark and tinted looks.
IOS="$DIR/ios/AppIcon.appiconset"
rm -rf "$DIR/ios" && mkdir -p "$IOS"
cp "$TMP/full.png" "$IOS/AppIcon-1024.png"
cp "$TMP/glyph.png" "$IOS/AppIcon-1024-dark.png"
magick "$TMP/full.png" -colorspace gray -alpha off "$IOS/AppIcon-1024-tinted.png"
cat > "$IOS/Contents.json" <<'JSON'
{
  "images" : [
    { "filename" : "AppIcon-1024.png", "idiom" : "universal", "platform" : "ios", "size" : "1024x1024" },
    {
      "appearances" : [ { "appearance" : "luminosity", "value" : "dark" } ],
      "filename" : "AppIcon-1024-dark.png", "idiom" : "universal", "platform" : "ios", "size" : "1024x1024"
    },
    {
      "appearances" : [ { "appearance" : "luminosity", "value" : "tinted" } ],
      "filename" : "AppIcon-1024-tinted.png", "idiom" : "universal", "platform" : "ios", "size" : "1024x1024"
    }
  ],
  "info" : { "author" : "xcode", "version" : 1 }
}
JSON

# Android: legacy and round launcher icons, adaptive layers (108dp, glyph
# inside the 66dp safe zone), Android 13 monochrome, Play Store 512.
AND="$DIR/android"
rm -rf "$AND" && mkdir -p "$AND/res/mipmap-anydpi-v26" "$AND/res/values"
magick "$TMP/full.png" \( +clone -alpha transparent -fill white -draw "circle 512,512 512,0" \) \
    -compose copy-opacity -composite "$TMP/round.png"
for d in mdpi:1 hdpi:1.5 xhdpi:2 xxhdpi:3 xxxhdpi:4; do
    name=${d%%:*} f=${d#*:}
    launcher=$(awk "BEGIN { printf \"%d\", 48 * $f }")
    layer=$(awk "BEGIN { printf \"%d\", 108 * $f }")
    out="$AND/res/mipmap-$name"
    padded "$TMP/mac.png" "$launcher" 1.0 none "$out/ic_launcher.png"
    png "$TMP/round.png" "$launcher" "$out/ic_launcher_round.png"
    padded "$TMP/glyph.png" "$layer" 0.70 none "$out/ic_launcher_foreground.png"
    padded "$TMP/glyph-white.png" "$layer" 0.70 none "$out/ic_launcher_monochrome.png"
done
for n in ic_launcher ic_launcher_round; do
    cat > "$AND/res/mipmap-anydpi-v26/$n.xml" <<'XML'
<?xml version="1.0" encoding="utf-8"?>
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
    <background android:drawable="@color/ic_launcher_background" />
    <foreground android:drawable="@mipmap/ic_launcher_foreground" />
    <monochrome android:drawable="@mipmap/ic_launcher_monochrome" />
</adaptive-icon>
XML
done
cat > "$AND/res/values/ic_launcher_background.xml" <<XML
<?xml version="1.0" encoding="utf-8"?>
<resources>
    <color name="ic_launcher_background">$BG</color>
</resources>
XML
png "$TMP/full.png" 512 "$AND/play-store-512.png"

# Web: favicons, Apple touch icon, PWA icons (the maskable one keeps the
# glyph inside the 80% safe circle).
WEB="$DIR/web"
rm -rf "$WEB" && mkdir -p "$WEB"
png "$TMP/mac.png" 16 "$WEB/favicon-16.png"
png "$TMP/mac.png" 32 "$WEB/favicon-32.png"
magick "$TMP/mac.png" -define icon:auto-resize=48,32,16 "$WEB/favicon.ico"
png "$TMP/full.png" 180 "$WEB/apple-touch-icon.png"
png "$TMP/mac.png" 192 "$WEB/icon-192.png"
png "$TMP/mac.png" 512 "$WEB/icon-512.png"
padded "$TMP/glyph.png" 512 0.70 "$BG" "$WEB/icon-maskable-512.png"

echo "Icons written to $DIR"
