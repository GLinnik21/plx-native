#!/usr/bin/env bash
# Render site/og/card.html to site/media/og-card.jpg, the 1200x630 link-preview image that
# plxnative.com advertises through its og:image / twitter:image tags.
#
#   tools/render-og-card.sh                           # the card as committed (docs/screenshots/home.jpg)
#   tools/render-og-card.sh --shot IMG --out FILE     # another screenshot, somewhere else
#
# `make screenshots` runs the second form with the home figure it has just rendered, so the card
# always shows the current app (tests/screenshots/scenes.json, the `home` scene's `card` output).
#
# Needs any headless Chromium: set CHROME, or have Google Chrome, Chromium, or a Playwright
# browser cache installed, plus `sips` (macOS) or ImageMagick for the JPEG step. The card is a
# JPEG because WhatsApp drops preview images much over 300 KB; a PNG of it is ~700 KB.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/site/media/og-card.jpg"
shot=""
while [ $# -gt 0 ]; do
  case "$1" in
    --shot) shot="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    *) echo "render-og-card: unknown argument $1" >&2; exit 2 ;;
  esac
done

tmp="$(mktemp -t og-card.XXXXXX).png"
# The page is rendered from site/og/ so its relative font, logo and figure URLs resolve; with
# --shot it is a copy beside the original whose --shot variable names that file instead.
page="$root/site/og/card.html"
copy=""
trap 'rm -f "$tmp" ${copy:+"$copy"}' EXIT
if [ -n "$shot" ]; then
  [ -f "$shot" ] || { echo "render-og-card: no such screenshot: $shot" >&2; exit 1; }
  shot="$(cd "$(dirname "$shot")" && pwd)/$(basename "$shot")"
  copy="$(mktemp "$root/site/og/.card-XXXXXX")"
  mv "$copy" "$copy.html"  # Chromium sniffs the page type from its name
  copy="$copy.html"
  sed "s#</head>#<style>:root { --shot: url(\"file://$shot\"); }</style></head>#" "$page" > "$copy"
  page="$copy"
fi

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
  --screenshot="$tmp" "file://$page" >/dev/null 2>&1
[ -s "$tmp" ] || { echo "render-og-card: $chrome wrote no screenshot" >&2; exit 1; }
if command -v sips >/dev/null 2>&1; then
  sips -s format jpeg -s formatOptions 86 "$tmp" --out "$out" >/dev/null
else
  magick "$tmp" -quality 86 "$out"
fi
file "$out"
