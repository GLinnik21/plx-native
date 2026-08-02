#!/usr/bin/env bash
#
# capture-screen.sh — grab the LG webOS TV screen to an image file on this Mac.
#
# Uses the TV's AV-framework capture service:
#     luna://com.webos.service.tv.capture/executeOneShot
# which taps the FINAL panel output. Plane coverage depends on the method:
#
#     DISPLAY  (default) : hardware VIDEO overlay plane + GLES/graphics(UI) plane,
#                          composited exactly as shown on screen.  <-- use this
#     VIDEO              : hardware VIDEO overlay plane ONLY (no UI).
#                          Fails with CAPTURE_ERROR_09 "no signal state" when
#                          nothing is decoded onto the video plane — which is a
#                          useful diagnostic in itself.
#     GRAPHIC            : GLES/graphics (UI) plane ONLY (no video).
#
# This is NOT captureCompositorOutput (that method does not exist on this
# webOS 4.5 build); it is the LG "CBE" capture back-end via /usr/sbin/avf.
#
# Usage:
#   ./capture-screen.sh [output_path] [method]
#     output_path : local file to write.  Default: ./tv-capture-<timestamp>.png
#                   Extension picks the format: .png (lossless, native, default),
#                   .jpg/.jpeg (small), .bmp (raw). Any other extension is
#                   captured as PNG and converted locally with `sips`.
#     method      : DISPLAY (default) | VIDEO | GRAPHIC   (see above)
#
# Environment overrides:
#     TV_HOST (default: the gitignored .tv-host)  TV_USER (root)  TV_PASS (alpine)
#     CAP_W (1920)  CAP_H (1080)
#
# Requires: bash, ssh, scp, and (only if no SSH key is installed) sshpass.
#           sips (built into macOS) is used for non-native format conversion.
#
set -euo pipefail

# The TV's address comes from $TV_HOST, else the gitignored .tv-host next to the Makefile (the
# same file `make TV=` falls back to) — the repo carries no home-network address of its own.
TV_HOST="${TV_HOST:-$(cat "$(dirname "$0")/../.tv-host" 2>/dev/null || true)}"
[ -n "$TV_HOST" ] || { echo "no TV configured — put its IP in .tv-host, or set TV_HOST=<ip>" >&2; exit 1; }
TV_USER="${TV_USER:-root}"
TV_PASS="${TV_PASS:-alpine}"
CAP_W="${CAP_W:-1920}"
CAP_H="${CAP_H:-1080}"

OUT="${1:-tv-capture-$(date +%Y%m%d-%H%M%S).png}"
METHOD="${2:-DISPLAY}"

# ---- pick the on-device format from the requested extension --------------------
ext="${OUT##*.}"; ext="$(printf '%s' "$ext" | tr '[:upper:]' '[:lower:]')"
CONVERT=""
case "$ext" in
  jpg|jpeg) DEV_FMT="JPEG"; DEV_EXT="jpg" ;;
  bmp)      DEV_FMT="BMP";  DEV_EXT="bmp" ;;
  png)      DEV_FMT="PNG";  DEV_EXT="png" ;;
  *)        DEV_FMT="PNG";  DEV_EXT="png"; CONVERT="1" ;;   # capture PNG, convert locally
esac
REMOTE="/tmp/capture/cap-$$.${DEV_EXT}"

# ---- SSH/SCP transport: prefer key auth, fall back to sshpass ------------------
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
          -o LogLevel=ERROR -o ConnectTimeout=10)
if ssh "${SSH_OPTS[@]}" -o BatchMode=yes "${TV_USER}@${TV_HOST}" true 2>/dev/null; then
  SSH=(ssh "${SSH_OPTS[@]}"); SCP=(scp "${SSH_OPTS[@]}")
elif command -v sshpass >/dev/null 2>&1; then
  SSH=(sshpass -p "$TV_PASS" ssh "${SSH_OPTS[@]}")
  SCP=(sshpass -p "$TV_PASS" scp "${SSH_OPTS[@]}")
else
  echo "ERROR: cannot auth to ${TV_USER}@${TV_HOST}. Install an SSH key or sshpass." >&2
  exit 1
fi

# ---- run the capture ----------------------------------------------------------
# luna-send only delivers its request reliably under a pseudo-TTY -> ssh -tt.
PAYLOAD="{\"path\":\"${REMOTE}\",\"method\":\"${METHOD}\",\"width\":${CAP_W},\"height\":${CAP_H},\"format\":\"${DEV_FMT}\"}"
RESP=$("${SSH[@]}" -tt "${TV_USER}@${TV_HOST}" \
        "mkdir -p /tmp/capture; luna-send -n 1 'luna://com.webos.service.tv.capture/executeOneShot' '${PAYLOAD}'" \
        2>/dev/null | tr -d '\r')

if ! printf '%s' "$RESP" | grep -q '"returnValue": *true'; then
  echo "ERROR: capture failed (method=$METHOD). Service response:" >&2
  printf '%s\n' "$RESP" >&2
  [ "$METHOD" = "VIDEO" ] && \
    echo "Hint: VIDEO fails with 'no signal state' when nothing is decoded on the video plane." >&2
  exit 1
fi

# ---- pull it back, clean up the device ----------------------------------------
LOCAL_DEV="$OUT"; [ -n "$CONVERT" ] && LOCAL_DEV="${OUT%.*}.png"
"${SCP[@]}" "${TV_USER}@${TV_HOST}:${REMOTE}" "$LOCAL_DEV" >/dev/null 2>&1
"${SSH[@]}" "${TV_USER}@${TV_HOST}" "rm -f ${REMOTE}" >/dev/null 2>&1 || true

# ---- convert if a non-native extension was requested --------------------------
if [ -n "$CONVERT" ]; then
  sips -s format "$ext" "$LOCAL_DEV" --out "$OUT" >/dev/null 2>&1 \
    || { echo "ERROR: sips could not convert to .$ext" >&2; exit 1; }
  rm -f "$LOCAL_DEV"
fi

# ---- validate & report --------------------------------------------------------
[ -s "$OUT" ] || { echo "ERROR: output file is empty: $OUT" >&2; exit 1; }
DIM=$(sips -g pixelWidth -g pixelHeight "$OUT" 2>/dev/null | awk '/pixel/{print $2}' | paste -sd x -)
ABS="$(cd "$(dirname "$OUT")" && pwd)/$(basename "$OUT")"
echo "Captured [method=${METHOD}, ${DIM:-unknown} px] -> ${ABS}"
