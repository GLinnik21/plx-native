#!/usr/bin/env bash
# Render site/og/social-preview.html to assets/github-social-preview.png, the 1280x640 image for
# GitHub > Settings > Social preview (upload by hand: GitHub has no API for it). Same Chromium
# discovery as render-og-card.sh: set CHROME (and CHROME_ARGS, e.g. --no-sandbox) to override.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$root/assets/github-social-preview.png}"
page="$root/site/og/social-preview.html"

chrome="${CHROME:-}"
if [ -z "$chrome" ]; then
  for c in \
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    "/Applications/Chromium.app/Contents/MacOS/Chromium" \
    "$(command -v chromium 2>/dev/null || true)" \
    "$(command -v google-chrome 2>/dev/null || true)" \
    "$HOME"/Library/Caches/ms-playwright/chromium_headless_shell-*/chrome-headless-shell-*/chrome-headless-shell \
    "$HOME"/.cache/ms-playwright/chromium_headless_shell-*/chrome-headless-shell-*/chrome-headless-shell \
    /opt/pw-browsers/chromium*/chrome-linux/chrome; do
    if [ -n "$c" ] && [ -x "$c" ]; then chrome="$c"; break; fi
  done
fi
[ -n "$chrome" ] || { echo "render-github-social: no headless Chromium found; set CHROME=/path/to/chrome" >&2; exit 1; }

# shellcheck disable=SC2086  # CHROME_ARGS is a word list on purpose.
"$chrome" ${CHROME_ARGS:-} --headless --disable-gpu --hide-scrollbars --allow-file-access-from-files \
  --force-device-scale-factor=1 --window-size=1280,640 --virtual-time-budget=3000 \
  --screenshot="$out" "file://$page" >/dev/null 2>&1
[ -s "$out" ] || { echo "render-github-social: $chrome wrote no screenshot" >&2; exit 1; }
file "$out"
