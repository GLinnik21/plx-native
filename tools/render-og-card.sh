#!/usr/bin/env bash
# Render site/og/card.html to site/media/og-card.jpg, the 1200x630 link-preview image that
# plxnative.com advertises through its og:image / twitter:image tags.
#
# Needs any headless Chromium: set CHROME, or have Google Chrome, Chromium, or a Playwright
# browser cache installed, plus `sips` (macOS) or ImageMagick for the JPEG step. The card is a
# JPEG because WhatsApp drops preview images much over 300 KB; a PNG of it is ~700 KB.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/site/media/og-card.jpg"
tmp="$(mktemp -t og-card.XXXXXX).png"
trap 'rm -f "$tmp"' EXIT

find_chrome() {
  if [ -n "${CHROME:-}" ]; then echo "$CHROME"; return; fi
  local c
  for c in \
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    "/Applications/Chromium.app/Contents/MacOS/Chromium" \
    "$(command -v chromium 2>/dev/null || true)" \
    "$(command -v google-chrome 2>/dev/null || true)" \
    "$HOME"/Library/Caches/ms-playwright/chromium_headless_shell-*/chrome-headless-shell-*/chrome-headless-shell \
    "$HOME"/.cache/ms-playwright/chromium_headless_shell-*/chrome-headless-shell-*/chrome-headless-shell \
    /opt/pw-browsers/chromium*/chrome-linux/chrome; do
    if [ -n "$c" ] && [ -x "$c" ]; then echo "$c"; return; fi
  done
  echo "render-og-card: no headless Chromium found; set CHROME=/path/to/chrome" >&2
  exit 1
}

chrome="$(find_chrome)"
"$chrome" --headless --disable-gpu --hide-scrollbars --allow-file-access-from-files \
  --force-device-scale-factor=1 --window-size=1200,630 --virtual-time-budget=3000 \
  --screenshot="$tmp" "file://$root/site/og/card.html" >/dev/null 2>&1
if command -v sips >/dev/null 2>&1; then
  sips -s format jpeg -s formatOptions 86 "$tmp" --out "$out" >/dev/null
else
  magick "$tmp" -quality 86 "$out"
fi
file "$out"
