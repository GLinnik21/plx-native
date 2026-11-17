#!/usr/bin/env bash
# tests/player_shots.sh — the player HUD and its four panels, on the simulator, as PNGs.
#
# The screens phase 9 moves are the ones no host test can look at: the transport, the track menu,
# the Info card, the Chapters strip and the `…` popover. This boots `plxnative-sim` straight into a
# real playback — `plxnative-playurl` at a Range-capable `tests/serve_fixtures.py`, plus
# `plxnative-clocksink` so the seam accepts and discards the access units and a presentation clock
# advances — drives the transport with the remote FIFO (each control pressed by its accessibility
# id, `press:player.control.subtitles`, not walked to) and writes one shot per panel. The same
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
# …and a synthetic PMS, so the boot has a session and lands on HOME, as a viewer's does. (A boot
# with no session reaches the player too: `playurl` also fires from the sign-in page.)
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

# A control is pressed BY ID (`press:<id>`, tools/a11y.py), not walked to with direction keys: the
# player's ids are `player.control.<command>`, `player.tab.info|chapters` and `player.scrub`, and an id
# names the disc or tab whatever the row holds. A refusal (`a11y: refused press ...` in the event log:
# the HUD is not up, the id names nothing on this clip) presses nothing, so `press` says so and returns
# 1: the shot that would follow is of a state the script did not reach. The HUD must be UP to be
# pressed, and it lingers HUD_LINGER_MS (4.5 s, plx_appkit::player_hud::LINGER_MS) after the input
# that raised it: the shot-then-press gap here (STEP + 2 s = 2.9 s) fits, so a slower STEP can make
# the first press refuse.
refusals() { local n; n=$(grep -c 'a11y: refused' "$LOG" 2>/dev/null || true); echo "${n:-0}"; }
press() {
  local before after; before=$(refusals)
  printf 'press:player.%s ' "$1" >&3; sleep "$STEP"
  after=$(refusals)
  [ "$after" -eq "$before" ] && return 0
  grep 'a11y: refused' "$LOG" | tail -n "$((after - before))" >&2
  return 1
}
must_press() { press "$1" || { echo "player_shots: the app refused press:player.$1 (is the HUD up?)" >&2; exit 1; }; }

# The bare transport, focus at rest on the scrubber. LEFT/RIGHT is a scrub, so a bare `right`
# raises it without changing what the row holds (a key: the raise is the subject).
send right
shot hud

# Row 1 is the control row: the Subtitles / Audio / … discs.
must_press control.subtitles; shot menu; send back
must_press control.audio; shot menu-audio; send back
must_press control.more; shot more; send back

# Row 2 is the tab strip under the discs: Info, then Chapters (a clip without chapters draws no
# Chapters tab: its press is refused and that shot is skipped).
must_press tab.info; shot info; send back
if press tab.chapters; then shot chapters; send back; else echo "no Chapters tab on $CLIP: no chapters shot"; fi

# …and a transport key WITH a panel up: the panel must stay and the film must pause (issue 28).
must_press tab.info
sleep 1
printf 'pause ' >&3; sleep 1
shot info-paused
send back

exec 3>&-
sleep 1
kill "$SIM_PID" 2>/dev/null || true; wait "$SIM_PID" 2>/dev/null || true; SIM_PID=""
echo "shots in $OUT:"
ls -1 "$OUT"/*.png
