#!/usr/bin/env bash
# tests/focusfp.sh — the FOCUS FINGERPRINT flows, on the simulator, against the synthetic PMS.
#
# Each flow boots `plxnative-sim` into an instance root with `plxnative-focus` armed, drives it
# through the remote FIFO with the same tokens a television session uses, and keeps every
# `focus …` fingerprint line (`crate::focusprobe`, logged only on CHANGE) plus every `route=`
# heartbeat transition the run produced. The result is one `.fp` file per flow under $OUT: the
# (screen x key) characterisation of the app's focus behaviour, taken against data that is pinned
# (`tests/mock_pms.py --seed`) and free of household bytes, so it can be diffed across commits and
# committed as a fixture.
#
# Phase 0 of the UI restructure runs this as a LIVE-SIM SMOKE: a flow passes when the app stayed
# alive and produced at least MIN_LINES fingerprint lines. From phase 2 the flows are replayed
# from the committed recording instead (restructure spec §15.4), and from 3b `--resolve` grades
# the engine's answers against these lines pointwise.
#
# Flow 10 (root BACK at Home / picker / QR) is a television flow — on the simulator a root BACK is
# a log line and nothing else — and is listed here as SKIP so the numbering matches the spec.
#
#   tests/focusfp.sh                    # all flows, own mock PMS on 127.0.0.1:32498
#   tests/focusfp.sh --only 2,8         # a subset
#   tests/focusfp.sh --rec --only 1     # RECORD each flow (plxnative-rec): the recording lands in
#                                       #   $OUT/root-<n>/plxnative-recordings/latest, ready for
#                                       #   `tools/plxnative-rec import <dir> <name>`
#   tests/focusfp.sh --replay --only 1  # REPLAY tests/fixtures/replay/<n>-<name> instead of
#                                       #   sending tokens; a flow passes when the app's own
#                                       #   `replay: done … verdict=SAME` line is produced
#   tests/focusfp.sh --pms 10.0.0.5:32400   # against a real server (fingerprints then carry
#                                            #   real ratingKeys: NOT committable)
#
# Needs `make sim` (the binary) and nothing else. Every flow gets a fresh instance root.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SIM_BIN="${SIM_BIN:-$ROOT/rust-modules/target-sim/debug/plxnative-sim}"
OUT="${OUT:-/tmp/plxnative-focusfp}"
PORT="${MOCK_PORT:-32498}"
SEED="${MOCK_SEED:-1}"
PMS=""
ONLY=""
REC=""
REPLAY=""
MIN_LINES=2
BOOT_WAIT=25     # seconds to wait for the boot marker before giving up on a flow
STEP=0.9         # seconds between tokens — a human D-pad cadence, and past every settle spring
SETTLE=3         # seconds after the last token before the run is read

while [ $# -gt 0 ]; do
  case "$1" in
    --pms) PMS="$2"; shift 2 ;;
    --only) ONLY="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --seed) SEED="$2"; shift 2 ;;
    --rec) REC=1; shift ;;
    --replay) REPLAY=1; shift ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "focusfp: unknown argument $1" >&2; exit 2 ;;
  esac
done

[ -x "$SIM_BIN" ] || { echo "focusfp: no simulator at $SIM_BIN — run \`make sim\` first" >&2; exit 2; }
mkdir -p "$OUT"

MOCK_PID=""
if [ -z "$PMS" ]; then
  python3 "$ROOT/tests/mock_pms.py" --port "$PORT" --seed "$SEED" > "$OUT/mock_pms.log" 2>&1 &
  MOCK_PID=$!
  for _ in $(seq 1 30); do
    grep -q 'serving' "$OUT/mock_pms.log" 2>/dev/null && break
    sleep 0.2
  done
  grep -q 'serving' "$OUT/mock_pms.log" || { echo "focusfp: mock PMS did not start (see $OUT/mock_pms.log)" >&2; exit 2; }
  PMS="127.0.0.1:$PORT"
fi
PMS_HOST="${PMS%%:*}"
PMS_PORT="${PMS##*:}"

SIM_PID=""
cleanup() {
  [ -n "$SIM_PID" ] && kill "$SIM_PID" 2>/dev/null || true
  [ -n "$MOCK_PID" ] && kill "$MOCK_PID" 2>/dev/null || true
}
trap cleanup EXIT

# The synthetic library's ratingKeys are dense and seeded (tests/mock_pms.py): the first movie is
# 1001 and the first show is 2001 — under any seed. A real server needs `--rk`; not offered yet.
RK_MOVIE=1001
RK_SHOW=2001

# run_flow <n> <name> "<triggers: name[=value] …>" "<tokens …>" <boot-marker-regex>
run_flow() {
  local n="$1" name="$2" triggers="$3" tokens="$4" marker="$5"
  local d="$OUT/root-$n"
  local fp="$OUT/$n-$name.fp"
  rm -rf "$d"; mkdir -p "$d"
  printf 'synthetic-token' > "$d/plxnative-token"
  touch "$d/plxnative-focus" "$d/plxnative-noidle"
  [ -n "$REC" ] && touch "$d/plxnative-rec"
  if [ -n "$REPLAY" ]; then
    local fixture="$ROOT/tests/fixtures/replay/$n-$name"
    [ -d "$fixture" ] || { echo "  [SKIP] $n $name: no committed fixture at $fixture"; return 0; }
    printf '%s' "$fixture" > "$d/plxnative-recplay"
  fi
  for t in $triggers; do
    case "$t" in
      *=*) printf '%s' "${t#*=}" > "$d/plxnative-${t%%=*}" ;;
      *)   touch "$d/plxnative-$t" ;;
    esac
  done
  PLXNATIVE_RUNTIME_DIR="$d" PLXNATIVE_APP_DIR="$ROOT/pkg" PLXNATIVE_WIN=1920x1080 \
    "$SIM_BIN" "$PMS_HOST" "$PMS_PORT" > "$d/sim.out" 2>&1 &
  SIM_PID=$!
  local log="$d/plxnative-events.log"
  local ok_boot=0
  for _ in $(seq 1 $((BOOT_WAIT * 5))); do
    if [ -f "$log" ] && grep -qE "$marker" "$log"; then ok_boot=1; break; fi
    kill -0 "$SIM_PID" 2>/dev/null || break
    sleep 0.2
  done
  if [ "$ok_boot" != 1 ]; then
    echo "  [FAIL] $n $name: boot marker /$marker/ never appeared (see $log)"
    kill "$SIM_PID" 2>/dev/null || true; wait "$SIM_PID" 2>/dev/null || true; SIM_PID=""
    return 1
  fi
  if [ -n "$REPLAY" ]; then
    # The recording carries the inputs; the app ends the run itself when the recording does.
    local waited=0
    while kill -0 "$SIM_PID" 2>/dev/null && [ "$waited" -lt 120 ]; do
      grep -q '^replay: REFUSED' "$log" && break
      sleep 1; waited=$((waited+1))
    done
    kill "$SIM_PID" 2>/dev/null || true; wait "$SIM_PID" 2>/dev/null || true; SIM_PID=""
    local verdict
    if verdict=$(grep -E '^replay: REFUSED' "$log" | head -1) && [ -n "$verdict" ]; then
      echo "  [FAIL] $n $name: $verdict"
      return 1
    fi
    verdict=$(grep -E '^replay: done ' "$log" | tail -1 || true)
    grep -E '^replay: (diverge|present|INITIAL)' "$log" | head -20 | sed 's/^/    /'
    if [ -z "$verdict" ]; then
      echo "  [FAIL] $n $name: no \`replay: done\` line after ${waited}s ($log)"; return 1
    fi
    case "$verdict" in
      *verdict=SAME*) echo "  [PASS] $n $name: $verdict"; return 0 ;;
      *) echo "  [FAIL] $n $name: $verdict"; return 1 ;;
    esac
  fi
  sleep 2   # posters and the async landings the first keys must not race
  if [ -n "$tokens" ]; then
    # `<>`: a FIFO opened write-only blocks in open(2) with no reader (ui-sim skill, trap 1).
    exec 3<> "$d/plxnative-remote"
    for tok in $tokens; do
      case "$tok" in
        sleep:*) sleep "${tok#sleep:}" ;;
        *) printf '%s ' "$tok" >&3; sleep "$STEP" ;;
      esac
    done
    exec 3>&-
  fi
  sleep "$SETTLE"
  local alive=1
  kill -0 "$SIM_PID" 2>/dev/null || alive=0
  kill "$SIM_PID" 2>/dev/null || true; wait "$SIM_PID" 2>/dev/null || true; SIM_PID=""
  {
    echo "# focusfp flow $n $name — pms=$PMS_HOST:$PMS_PORT seed=$SEED triggers=[$triggers] tokens=[$tokens]"
    # fingerprints, and every heartbeat whose route word CHANGED — the two lines the harness reads
    awk '/^focus / {print; next}
         match($0, /route=[a-z]+( overlay=[a-z]+)?/) { r=substr($0, RSTART, RLENGTH); if (r != last) { print "hb " r; last=r } }' "$log"
  } > "$fp"
  local lines
  lines=$(grep -c '^focus ' "$fp" || true)
  if [ "$alive" != 1 ]; then
    echo "  [FAIL] $n $name: the app DIED during the flow ($lines fingerprint lines; $d/sim.out)"
    return 1
  fi
  local min_lines="$MIN_LINES"
  # The Settings overlays leave the underlying Home fingerprint unchanged. Their actual
  # traversal is asserted from heartbeat stages below, not from unrelated Home log volume.
  [ "$n" = 6 ] && min_lines=1
  if [ "$lines" -lt "$min_lines" ]; then
    echo "  [FAIL] $n $name: only $lines fingerprint line(s) (need >= $min_lines) — $fp"
    return 1
  fi
  local proof
  if ! proof=$(python3 "$ROOT/tests/focusfp_check.py" "$n" "$fp"); then
    echo "  [FAIL] $n $name: $proof — $fp"
    return 1
  fi
  echo "  [PASS] $n $name: $lines fingerprint lines -> $fp"
  [ -n "$REC" ] && echo "         recording: $d/plxnative-recordings/latest ($(cat "$d"/plxnative-recordings/latest/rec-*.jsonl 2>/dev/null | wc -l | tr -d " ") lines)"
  return 0
}

want() { [ -z "$ONLY" ] || [[ ",$ONLY," == *",$1,"* ]]; }

fails=0; ran=0; skipped=0
echo "=== focusfp: $PMS (seed $SEED), out=$OUT ==="
# 1 boot -> Home -> chip -> grid   (the picker is suppressed by the injected token; a TV flow)
want 1 && { ran=$((ran+1)); run_flow 1 boot-home-chip-grid "" "up left down down down" 'hubs: landed' || fails=$((fails+1)); }
# 2 grid -> Detail -> BACK
want 2 && { ran=$((ran+1)); run_flow 2 grid-detail-back "grid" "down ok sleep:2 back" 'hubs: landed' || fails=$((fails+1)); }
# 3 Home -> Library -> real grid card -> Detail -> BACK to the same Library card. The shot tokens
# retain the Library grid before Detail and after the return; they do not alter navigation. The
# second pre-detail shot is intentional: the first can catch the simulator's route transition.
want 3 && { ran=$((ran+1)); run_flow 3 home-library-detail-back "" "up right right ok sleep:3 down down down sleep:1 shot sleep:2 shot sleep:1 ok sleep:3 back sleep:3 shot" 'hubs: landed' || fails=$((fails+1)); }
# 4 Search with a seeded query -> shelf -> Detail -> BACK   (typing is the television's keyboard;
#   the seed is the only way a headless run reaches a result shelf — docs/search.md)
want 4 && { ran=$((ran+1)); run_flow 4 search-shelf-detail-back "search=s0" "sleep:2 down down ok sleep:2 back" 'route=search' || fails=$((fails+1)); }
# 5 Detail -> Person -> Detail -> BACK -> Person -> BACK -> Detail
want 5 && { ran=$((ran+1)); run_flow 5 detail-person-detail "detail=$RK_MOVIE detailsec=1 detailok" "sleep:2 down ok sleep:2 back sleep:1 back sleep:1 down ok sleep:2 back" 'route=person' || fails=$((fails+1)); }
# 6 chip -> Account -> Settings -> Privacy -> Legal -> document -> BACK x4
# The synthetic token boot has no signed-in account: Favourites is absent, so Privacy is row 0.
# Visit it and toggle its first choice, then return to row 0 and step down once to Legal.
want 6 && { ran=$((ran+1)); run_flow 6 settings-family "settings=root" "sleep:1 ok sleep:1 ok sleep:1 back sleep:1 down ok sleep:1 ok sleep:1 back back back" 'overlay=settings' || fails=$((fails+1)); }
# 7 first-run consent -> onboard -> Home
want 7 && { ran=$((ran+1)); run_flow 7 firstrun-consent-onboard "firstrun" "sleep:1 ok sleep:1 ok sleep:1 down ok sleep:2" 'route=' || fails=$((fails+1)); }
# 8 Detail -> hold on Related -> ItemMenu -> BACK. detailsec is a DOWN count, not a section id:
# the synthetic movie's order is hero -> Cast -> Related -> About.
want 8 && { ran=$((ran+1)); run_flow 8 detail-hold-itemmenu "detail=$RK_MOVIE detailsec=2" "sleep:2 okdown sleep:0.7 okup sleep:1 back" 'route=detail' || fails=$((fails+1)); }
# 9 player (the mock's part bytes are not a film: the run lands on the failure read-out, which is
#   the one screen the simulator reaches for playback — spec §15.4 requires a player line)
want 9 && { ran=$((ran+1)); run_flow 9 player-readout "play=$RK_MOVIE clocksink" "sleep:3 down sleep:1 ok sleep:1 back" 'route=player' || fails=$((fails+1)); }
want 10 && { skipped=$((skipped+1)); echo "  [SKIP] 10 root-back: a television flow (tv-session skill) — the simulator's root BACK is a log line"; }
# 11 pointer clicks on every stop class incl. a clipped one and one under the tab track
want 11 && { ran=$((ran+1)); run_flow 11 pointer-stops "" "ck:960,92 sleep:1 ck:133,92 sleep:1 back sleep:1 ck:220,960 sleep:1 ck:1850,960 sleep:1 ck:960,540" 'hubs: landed' || fails=$((fails+1)); }
# 12 Phase-7 adoption: Filmography owns its own focus scope. A library-matched credit opens Detail, BACK
# restores the Filmography entry and the next BACK dismisses it to its Person host. `nowan`: the
# person page's F_PROFILE/F_CREDITS mailboxes still dial discover.provider.plex.tv for real (the
# `personcredits` seed only stands in for a landing, it does not suppress the fetch `pump` already
# sent before the seed runs), and against the injected token that call 401s — so this flow's
# stability otherwise depends on the internet answering that 401 promptly. `nowan` makes the name
# refuse locally and at once, the same offline reproduction `docs/agent-reference.md` documents.
want 12 && { ran=$((ran+1)); run_flow 12 filmography-detail-return "detail=$RK_MOVIE detailsec=1 detailok filmography personcredits nowan" "sleep:2 down ok sleep:2 back sleep:2 back" 'route=person' || fails=$((fails+1)); }

echo "=== focusfp: $((ran - fails)) passed, $fails failed of $ran, $skipped skipped ==="
[ "$fails" -eq 0 ]
