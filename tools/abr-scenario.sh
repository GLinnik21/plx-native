#!/usr/bin/env bash
#
# abr-scenario.sh — run ONE Auto/ABR scenario against the host simulator over a shaped link,
# and print what the controller did with it.
#
#   tools/abr-scenario.sh <name> <item-key> "<leg> [<leg> …]"
#
# where a leg is `<at_s>:<netcond-mode>`, e.g. `0:pass 40:rate:2500 100:rate:16000`.
#
# WHY THIS EXISTS, and why it is not `tests/run.py --server`.
#
# The harness's two `link_profile` cases are the right shape and cannot answer this question.
# `auto_original_squeeze` says so in its own note — "the squeeze is never lifted, deliberately: the
# question is whether Auto ever leaves, not whether it recovers when the link does" — and
# `auto_link_squeeze` lifts the squeeze to `pass`, which on a gigabit LAN is a link hundreds of
# times the source. Neither reaches the régime the device failed in.
#
# **That régime is a RATIO, not a rate.** Auto's Original gate needs
# `conservative_kbps >= vbr_allowance_pm * source`, and `conservative_kbps` is `slow * (1 - unc)`
# with `unc` floored at 200 pm — so admission needs `slow >= 1.350/0.800 = 1.6875 x source`, and
# the interesting band is `1.0 x source < link < 1.69 x source`: fast enough to carry the film,
# too slow for the model to say so. Releasing to `pass` jumps clean over it. The device landed
# inside it (a 25 264 kbps 4K DV source on a ~38 Mbps link, 1.5x) and blinked twice.
#
# So the legs here are written as MULTIPLES of the measured source bitrate, resolved per item.
#
# HOST ONLY, and deliberately. This runs `plxnative-sim` with the clock sink armed, so the whole
# path between the socket and the decoder is real — both AVIO transports, `ff.rs`'s demux, the AU
# queues and their byte-cap backpressure, the feed-ahead throttle, the ABR controller and its
# transactions. Nothing decodes, and nothing measured here is a device measurement: every
# heartbeat carries `sim=1`. What it CAN answer is the only question this scenario asks — how many
# times did the controller change its mind, and what did it settle on.
#
# netcond binds LOOPBACK here, which also sidesteps the macOS application firewall: the prompt
# that silently drops a TV's connections to an ad-hoc python listener never appears for 127.0.0.1.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

NAME="${1:?usage: abr-scenario.sh <name> <item-key> \"<at_s>:<mode> …\"}"
ITEM="${2:?item key from tests/manifest.local.json}"
LEGS="${3:?legs, e.g. \"0:pass 40:rate:2500\"}"
RUN_SECS="${RUN_SECS:-180}"

PROXY_PORT="${PROXY_PORT:-32499}"
DIR="/tmp/abr-sim-$NAME"
LOG="$DIR/plxnative-events.log"
MODEFILE="/tmp/netcond-$NAME.mode"
# Ask the Makefile where it put the binary rather than restating the path: `SIM_TDIR` is
# overridable (agents running several simulators keep separate target dirs) and a literal copy
# here would silently test a stale build from another lane.
SIM_BIN="$(make -s print-simbin 2>/dev/null)"
[ -n "$SIM_BIN" ] && [ -x "$SIM_BIN" ] || SIM_BIN="rust-modules/target-sim/debug/plxnative-sim"
[ -x "$SIM_BIN" ] || { echo "no simulator binary — run \`make sim\`"; exit 1; }

# The rating key and the token are read from gitignored files and NEVER echoed. This repo is
# public and `outbound-guard.py` exists because that convention has already failed once.
RK="$(python3 -c "import json;print(json.load(open('tests/manifest.local.json'))['items']['$ITEM'])" 2>/dev/null)"
[ -n "$RK" ] || { echo "item '$ITEM' not in tests/manifest.local.json"; exit 1; }
TOKEN="$(sed -n 's/.*PMS_TOKEN *"\([^"]*\)".*/\1/p' src/config.local.h | head -1)"
[ -n "$TOKEN" ] || { echo "no PMS_TOKEN in src/config.local.h"; exit 1; }
PMS_HOST="$(sed -n 's/.*PMS_HOST *"\([^"]*\)".*/\1/p' src/config.local.h | head -1)"
PMS_PORT="$(sed -n 's/^#define[ 	]*PMS_PORT[ 	]*\([0-9]*\).*/\1/p' src/config.local.h | head -1)"

# ---- the source bitrate, so a leg can be written as a multiple of it ----------
SRC_KBPS="$(curl -s -m 10 -H 'Accept: application/json' \
    "http://$PMS_HOST:$PMS_PORT/library/metadata/$RK?X-Plex-Token=$TOKEN" \
  | python3 -c "
import json,sys
d=json.load(sys.stdin)['MediaContainer']['Metadata'][0]['Media'][0]
print(int(d.get('bitrate') or 0))
" 2>/dev/null)"
[ "${SRC_KBPS:-0}" -gt 0 ] || { echo "could not read the source bitrate for '$ITEM'"; exit 1; }

# `xN` legs resolve against it; a bare `rate:` or `pass` passes through untouched.
resolve_leg() {
  case "$1" in
    x*) echo "rate:$(python3 -c "print(int($SRC_KBPS * ${1#x}))")" ;;
    *)  echo "$1" ;;
  esac
}

echo "== $NAME :: $ITEM :: source ${SRC_KBPS}kbps =="
echo "   legs:"
for leg in $LEGS; do
  printf '     t=%-4s %s\n' "${leg%%:*}s" "$(resolve_leg "${leg#*:}")"
done

# ---- netcond, on loopback -----------------------------------------------------
rm -f "$MODEFILE"; echo pass > "$MODEFILE"
python3 -u tools/netcond.py --bind 127.0.0.1 --listen "$PROXY_PORT" \
    --target "127.0.0.1:$PMS_PORT" --control "$MODEFILE" \
    > "/tmp/netcond-$NAME.log" 2>&1 &
NETCOND_PID=$!
sleep 1
kill -0 "$NETCOND_PID" 2>/dev/null || { echo "netcond died:"; tail -5 "/tmp/netcond-$NAME.log"; exit 1; }

# ---- the instance root --------------------------------------------------------
rm -rf "$DIR"; mkdir -p "$DIR"
printf '%s' "$TOKEN" > "$DIR/plxnative-token"
: > "$DIR/plxnative-clocksink"   # accept AUs, discard them, advance a real-time clock
: > "$DIR/plxnative-noidle"      # the present gate would otherwise stall a settled screen
: > "$DIR/plxnative-detailplay"  # press Play once the detail page has landed
printf '%s' "$RK" > "$DIR/plxnative-detail"
# A seek is not a link condition, but it is the other half of the state space this tool exists to
# reach: `transcode_seek` REUSES the encoder session id, so the transactions either side of a seek
# are the only ones whose names can collide. `AUTOSEEK` takes the trigger's own grammar verbatim
# (`gap=<ms>` then comma-separated absolute/relative steps) and the first step fires ~12 s after
# the player route is entered, so `gap=` is how you put a seek AFTER a commit rather than before.
[ -n "${AUTOSEEK:-}" ] && printf '%s' "$AUTOSEEK" > "$DIR/plxnative-autoseek"

# **A bare `wait` here HANGS the whole matrix**, and it did: the simulator is an SDL application
# and under `nohup` there is no controlling terminal, so a plain TERM does not always end it — the
# scenario then sat in `wait` forever with its legs already applied and its verdict never printed,
# which reads exactly like a slow run. Escalate instead: TERM, a bounded grace, then KILL, and
# never block on a process that has already been sent both.
cleanup() {
  kill "$SIM_PID" "$NETCOND_PID" 2>/dev/null
  local waited=0
  while [ $waited -lt 20 ] && kill -0 "$SIM_PID" 2>/dev/null; do sleep 0.25; waited=$((waited+1)); done
  kill -9 "$SIM_PID" "$NETCOND_PID" 2>/dev/null
  wait "$SIM_PID" 2>/dev/null || true
  wait "$NETCOND_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

PLXNATIVE_RUNTIME_DIR="$DIR" PLXNATIVE_APP_DIR="$REPO/pkg" PLXNATIVE_WIN=640x360 \
  "$SIM_BIN" 127.0.0.1 "$PROXY_PORT" > "$DIR/sim.stdout" 2>&1 &
SIM_PID=$!

# ---- drive the profile --------------------------------------------------------
# Anchored on the app's FIRST LOG LINE, exactly as `tests/run.py` anchors a `link_profile`: ssh
# start, process start and app start are three different clocks and only the last one is the
# app's own.
START=""
for _ in $(seq 1 60); do
  [ -s "$LOG" ] && { START="$(date +%s)"; break; }
  sleep 0.5
done
[ -n "$START" ] || { echo "the simulator never wrote an event log"; tail -20 "$DIR/sim.stdout"; exit 1; }

for leg in $(echo "$LEGS" | tr ' ' '\n' | sort -t: -k1 -n); do
  at="${leg%%:*}"; mode="$(resolve_leg "${leg#*:}")"
  now=$(( $(date +%s) - START ))
  [ "$at" -gt "$now" ] && sleep $(( at - now ))
  echo "$mode" > "$MODEFILE"
  echo "   [t=$(( $(date +%s) - START ))s] link -> $mode"
  kill -0 "$SIM_PID" 2>/dev/null || { echo "   simulator exited early"; break; }
done

while [ $(( $(date +%s) - START )) -lt "$RUN_SECS" ] && kill -0 "$SIM_PID" 2>/dev/null; do sleep 2; done
cleanup

# ---- what the controller did --------------------------------------------------
echo
echo "-- mode and rung changes ------------------------------------------------"
grep -aE 'route: Auto|auto: |reload_at:|reload_transcode:|abr: (committed|tx |seed|mode chose)|Original probe|abr: source|autoseek:|not produced in time' "$LOG" \
  | sed 's/^/   /' || true
echo
# `grep -c` exits 1 on a count of zero, so `|| echo 0` fires ON TOP of the 0 it already printed
# and the verdict reads "0\n0". `|| true` keeps the count and swallows only the status.
RELOADS=$( { grep -ac 'reload_at: fresh Load\|reload_transcode: fresh Load' "$LOG" || true; } 2>/dev/null)
SWITCHES=$( { grep -ac '^auto: ' "$LOG" || true; } 2>/dev/null)
echo "-- verdict ---------------------------------------------------------------"
echo "   visible reloads (each is a blink): $RELOADS"
echo "   mode-controller decisions:         $SWITCHES"
echo "   final state: $(grep -aE 'abr: steady|abr: committed' "$LOG" | tail -1 | sed 's/^/     /')"
echo "   log: $LOG"
