#!/usr/bin/env bash
# tests/player_shots.sh — the player HUD and its four panels, on the simulator, as PNGs.
#
# The screens phase 9 moves are the ones no host test can look at: the transport, the track menu,
# the Info card, the Chapters strip and the `…` popover. This boots `plxnative-sim` straight into a
# real playback — `plxnative-playurl` at a Range-capable `tests/serve_fixtures.py`, plus
# `plxnative-clocksink` so the seam accepts and discards the access units and a presentation clock
# advances — drives the transport with the remote FIFO and writes one shot per panel. The same
# script run against two commits produces two comparable sets.
#
#   CLIP=/tmp/plxfix/clip.mp4 tests/player_shots.sh /tmp/shots-mine
#
# The clip is any h264/aac mp4; generate one with
#   ffmpeg -f lavfi -i testsrc=size=1920x1080:rate=24:duration=120 \
#          -f lavfi -i sine=frequency=440:duration=120 \
#          -c:v libx264 -preset ultrafast -pix_fmt yuv420p -c:a aac -shortest clip.mp4
#
# Needs `make sim`. The video PLANE is empty here by construction (nothing decodes, and the wayland
# overlay is webOS-only), which is exactly right for HUD layout and wrong for anything about the
# picture.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SIM_BIN="${SIM_BIN:-$ROOT/rust-modules/target-sim/debug/plxnative-sim}"
OUT="${1:-/tmp/plxnative-player-shots}"
CLIP="${CLIP:-/tmp/plxfix/clip.mp4}"
MEDIA_PORT="${MEDIA_PORT:-8021}"
PMS_PORT="${PMS_PORT:-32497}"
STEP="${STEP:-0.9}"

[ -f "$CLIP" ] || { echo "no clip at $CLIP (see the header)"; exit 2; }

rm -rf "$OUT"; mkdir -p "$OUT"
D="$OUT/root"; mkdir -p "$D"

python3 "$ROOT/tests/serve_fixtures.py" --root "$(dirname "$CLIP")" --port "$MEDIA_PORT" \
  > "$OUT/media.log" 2>&1 &
MEDIA_PID=$!
# …and a synthetic PMS, only so the boot lands on HOME. The autoplay arm that reads `playurl` is
# skipped on `Route::Login`, so a boot with no session never reaches the player at all.
python3 "$ROOT/tests/mock_pms.py" --port "$PMS_PORT" --seed 1 > "$OUT/pms.log" 2>&1 &
PMS_PID=$!
cleanup() {
  [ -n "${SIM_PID:-}" ] && kill "$SIM_PID" 2>/dev/null || true
  kill "$MEDIA_PID" 2>/dev/null || true
  kill "$PMS_PID" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT
sleep 1

printf 'synthetic-token' > "$D/plxnative-token"
touch "$D/plxnative-noidle" "$D/plxnative-clocksink"
printf '{"url":"http://127.0.0.1:%s/%s","vcodec":"h264","acodec":"aac","fps":24.0}' \
  "$MEDIA_PORT" "$(basename "$CLIP")" > "$D/plxnative-playurl"

PLXNATIVE_RUNTIME_DIR="$D" PLXNATIVE_APP_DIR="$ROOT/pkg" PLXNATIVE_WIN=1920x1080 \
  "$SIM_BIN" 127.0.0.1 "$PMS_PORT" > "$D/sim.out" 2>&1 &
SIM_PID=$!

LOG="$D/plxnative-events.log"
for _ in $(seq 1 200); do
  [ -f "$LOG" ] && grep -q 'route=player' "$LOG" && break
  kill -0 "$SIM_PID" 2>/dev/null || break
  sleep 0.2
done
sleep 5

exec 3<> "$D/plxnative-remote"
send() { for t in "$@"; do printf '%s ' "$t" >&3; sleep "$STEP"; done; }
shot() { printf 'shot ' >&3; sleep 2; mv "$D"/shot-*.png "$OUT/$1.png" 2>/dev/null || true; }

# The bare transport, focus at rest on the scrubber. LEFT/RIGHT is a scrub, so a bare `right`
# raises it without changing what the row holds.
send right
shot hud

# Row 1 is the control row: the Subtitles / Audio / … discs.
send up
send ok; shot menu; send back
send right ok; shot menu-audio; send back
send right ok; shot more; send back

# Row 2 is the tab strip under the discs: Info, then Chapters.
send down down
send ok; shot info; send back
send right ok; shot chapters; send back

# …and a transport key WITH a panel up: the panel must stay and the film must pause (issue 28).
send left ok
sleep 1
printf 'pause ' >&3; sleep 1
shot info-paused
send back

exec 3>&-
sleep 1
kill "$SIM_PID" 2>/dev/null || true; wait "$SIM_PID" 2>/dev/null || true; SIM_PID=""
echo "shots in $OUT:"
ls -1 "$OUT"/*.png
