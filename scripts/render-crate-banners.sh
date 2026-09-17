#!/bin/sh
# Renders the crates' README banners, assets/banners/<crate>.png, from assets/banners/banner.html with
# headless Chrome at 2x (2560x800), so they stay sharp on crates.io and GitHub. The crate READMEs link them
# by their raw.githubusercontent.com URL on main, so a changed banner shows once it is merged.
#
#   scripts/render-crate-banners.sh        CHROME=<path> to use another Chrome
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
chrome=${CHROME:-"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"}
[ -x "$chrome" ] || { echo "no Chrome at $chrome; set CHROME" >&2; exit 1; }

for crate in colonizer-harness colonizer-agentd; do
  out="$root/assets/banners/$crate.png"
  "$chrome" --headless --disable-gpu --no-sandbox --hide-scrollbars \
    --force-device-scale-factor=2 --window-size=1280,400 --virtual-time-budget=10000 \
    --screenshot="$out" "file://$root/assets/banners/banner.html?crate=$crate" >/dev/null 2>&1
  printf '%s %s\n' "$crate" "$(sips -g pixelWidth -g pixelHeight "$out" 2>/dev/null | awk '/pixel/ { printf "%s ", $2 }')"
done
