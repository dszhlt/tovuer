#!/usr/bin/env bash
# Regenerate the README screenshots in docs/screenshots: the client draws demo
# scenes into HTML (the snapshot example) and headless Chromium takes the
# pictures. Needs chromium and ImageMagick (magick).
set -e
cd "$(dirname "$0")/.."
OUT=docs/screenshots
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
cargo build -q -p tovuer --example snapshot
mkdir -p "$OUT"

shot() {  # <file> <scene> <theme> <columns> <rows>
  local file=$1 scene=$2 theme=$3 cols=$4 rows=$5
  target/debug/examples/snapshot "$scene" "$theme" "$cols" "$rows" "$TMP/raw.html" 2>/dev/null
  # a window: rounded corners, a thin edge and a soft shadow on a transparent page
  sed 's/body{margin:0;background:#000}/body{margin:0;padding:18px 22px 28px;background:transparent}.t{border-radius:12px;overflow:hidden;box-shadow:0 0 0 1px rgba(255,255,255,.08),0 10px 28px rgba(0,0,0,.45)}/' \
    "$TMP/raw.html" > "$TMP/page.html"
  # the font is 15px JetBrains Mono: a cell is 9×20 px
  chromium --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=2 \
    --default-background-color=00000000 --window-size=$((cols * 9 + 44)),$((rows * 20 + 46)) \
    --screenshot="$file" "file://$TMP/page.html" >/dev/null 2>&1
  magick "$file" -strip -define png:compression-level=9 "$file"
}

shot "$OUT/main.png" voice tovuer 120 34
shot "$OUT/palette.png" palette catppuccin 100 28
shot "$OUT/settings.png" settings nord 100 28
for theme in tokyo-night gruvbox everforest sunset; do
  shot "$TMP/theme-$theme.png" voice "$theme" 100 28
done
magick montage "$TMP"/theme-{tokyo-night,gruvbox,everforest,sunset}.png -tile 2x2 -geometry 944x606+0+0 -background none "$TMP/themes.png"
magick "$TMP/themes.png" -dither FloydSteinberg -colors 256 -strip -define png:compression-level=9 "PNG8:$OUT/themes.png"
ls -l "$OUT"
