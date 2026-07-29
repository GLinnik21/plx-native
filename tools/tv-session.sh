#!/usr/bin/env bash
#
# tv-session.sh — bring the app up on the TV in a known state, drive it, hand the TV back.
#
#   tv-session.sh up [opts]      wake -> deploy-if-stale -> clear triggers -> arm -> launch -> verify
#   tv-session.sh status         re-assert an existing session without disturbing it
#   tv-session.sh key <token>…   send key tokens (down, ok, back, play, pause, stop, …)
#   tv-session.sh click <x> <y>  click at authored 1920x1080 coords
#   tv-session.sh shot [out.png] grab the panel (video plane included) via the capture service
#   tv-session.sh log [pattern]  fetch the on-device event log, optionally grepped
#   tv-session.sh down           hand the TV back: strip automation, relaunch interactive
#
# `up` options:
#   --screen <name>   home (default) | profiles | library[=N] | detail=<rk> | person=<movie rk>
#                     | player=<rk> | login | account | itemmenu
#   --guest           run as the managed test user rather than the owner (default: owner)
#   --stream[=PORT]   also start tools/stream-screen.py for a live browser view (default 8909)
#                     STREAM_RES=480x270 makes mpeg encode ~4x cheaper (see the skill)
#   --no-token        boot with no injected token (exercises the QR sign-in flow)
#   --keep            do not clear existing triggers first (rarely what you want)
#
# WHY THIS EXISTS: every on-device task needs the same fragile ritual, and each step fails
# silently in its own way — a sleeping TV makes every assertion read as a regression, a
# stale binary means you are testing yesterday's build, a leftover trigger silently changes
# which screen you land on AND suppresses the who's-watching picker, and SAM keeps stale
# "running" state so a launch without a close-first is a no-op. This asserts each step
# instead of assuming it. See .claude/skills/tv-session/SKILL.md.
#
# Config: TV host from $TV, else the Makefile's TV default. The PMS token is read from the
# gitignored src/config.local.h at runtime, written straight to the TV in its own ssh
# round-trip, and never printed. Nothing about the network or any credential lives here.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APPDIR=/media/developer/apps/usr/palm/applications/com.beb.plxnative
APPID=com.beb.plxnative
STREAM_PID_FILE="$REPO/.tv-stream.pid"

tv_host() {
  [ -n "${TV:-}" ] && { echo "$TV"; return; }
  make -C "$REPO" -pn 2>/dev/null | sed -n 's/^TV *= *//p' | head -1
}
HOST="$(tv_host)"
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=8)
tv()  { ssh "${SSH_OPTS[@]}" "root@$HOST" "$@"; }
tvq() { ssh "${SSH_OPTS[@]}" "root@$HOST" "$@" 2>/dev/null; }

ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$*"; }
info() { printf '  · %s\n' "$*"; }

# ---------------------------------------------------------------- wake -------
ensure_awake() {
  if tvq true; then ok "TV reachable"; return 0; fi
  info "TV asleep — waking"
  "$REPO/.claude/skills/wake-tv/wake-tv.sh" >/dev/null 2>&1
  if tvq true; then ok "TV woken"; return 0; fi
  bad "TV unreachable (see the wake-tv skill)"; return 1
}

# ------------------------------------------------------------- deploy --------
ensure_binary() {
  [ -f "$REPO/pkg/plxnative" ] || { bad "no pkg/plxnative — run make"; return 1; }
  local l t
  l=$(md5 -q "$REPO/pkg/plxnative" 2>/dev/null || md5sum "$REPO/pkg/plxnative" | cut -d' ' -f1)
  t=$(tvq "md5sum $APPDIR/plxnative" | cut -d' ' -f1)
  if [ "$l" = "$t" ]; then ok "deployed binary matches local build"; return 0; fi
  info "binary differs — deploying"
  make -C "$REPO" deploy >/dev/null 2>&1
  t=$(tvq "md5sum $APPDIR/plxnative" | cut -d' ' -f1)
  # a standby can truncate an scp mid-flight, so verify rather than trust
  [ "$l" = "$t" ] && { ok "deployed + md5 verified"; return 0; }
  bad "deploy did not land (md5 still differs) — TV may have slept mid-scp"; return 1
}

# ------------------------------------------------------------ triggers -------
# GLOB-clear, exactly like tests/run.py: a newly-added app trigger can never bleed in
# from a previous session, and any leftover non-DIAG file also suppresses the picker.
clear_triggers() {
  tv 'for f in /tmp/plxnative-*; do case "$f" in *.log) ;; *) rm -f "$f";; esac; done' 2>/dev/null
  ok "triggers cleared"
}

# token: read host-side, pushed in its own round-trip, never echoed
push_token() {
  local tok
  tok=$(sed -n 's/.*PMS_TOKEN *"\([^"]*\)".*/\1/p' "$REPO/src/config.local.h" 2>/dev/null | head -1)
  [ -n "$tok" ] || { bad "no PMS_TOKEN in src/config.local.h (gitignored) — boot will hit QR sign-in"; return 1; }
  printf '%s' "$tok" | tv "cat > /tmp/plxnative-token" || return 1
  ok "token injected (value not printed)"
}

# ------------------------------------------------------------- launch --------
relaunch() {
  # SAM keeps stale "running" state after a hard kill, so a launch without a close-first
  # is a silent no-op relaunch — `make kill` is the proven close path.
  make -C "$REPO" kill >/dev/null 2>&1
  sleep 2
  # luna-send must STAY SUBSCRIBED (-i) for the launch to take, which means the SSH
  # session has to stay OPEN while it does: backgrounding it and letting ssh return
  # kills the subscriber and the launch silently no-ops (the app keeps running as it
  # was, so everything downstream looks fine while testing the OLD instance).
  tv "rm -f /tmp/plxnative-events.log; \
      luna-send -i luna://com.webos.applicationManager/launch '{\"id\":\"$APPID\"}' >/dev/null 2>&1 & \
      LP=\$!; sleep 8; kill \$LP 2>/dev/null" >/dev/null 2>&1
}

PREV_PID=""
assert_running() {
  local pid; pid=$(tvq 'pidof plxnative')
  if [ -z "$pid" ]; then bad "app is not running after launch"; return 1; fi
  if [ -n "$PREV_PID" ] && [ "$pid" = "$PREV_PID" ]; then
    bad "app pid unchanged ($pid) — the relaunch did NOT take; you would be testing the old instance"
    return 1
  fi
  ok "app running (pid $pid)"; return 0
}

assert_route() {
  local want="$1" seen
  seen=$(tvq "grep -oE 'route=[a-z]+' /tmp/plxnative-events.log 2>/dev/null | tail -1")
  if [ -z "$seen" ]; then
    bad "no route= heartbeat in the event log yet (app booting, or it died — try: tools/crash-report.sh)"
    return 1
  fi
  info "reached ${seen}"
  [ -z "$want" ] && return 0
  [ "$seen" = "route=$want" ] && { ok "on the requested screen"; return 0; }
  bad "wanted route=$want, got $seen"; return 1
}

# ------------------------------------------------------------ commands -------
cmd_up() {
  local screen=home guest=0 stream="" no_token=0 keep=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --screen) screen="$2"; shift 2 ;;
      --screen=*) screen="${1#*=}"; shift ;;
      --guest) guest=1; shift ;;
      --stream) stream=8909; shift ;;
      --stream=*) stream="${1#*=}"; shift ;;
      --no-token) no_token=1; shift ;;
      --keep) keep=1; shift ;;
      *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
  done

  echo "== bringing up the TV session ($screen)"
  ensure_awake  || exit 1
  ensure_binary || exit 1
  [ "$keep" = 1 ] || clear_triggers

  # screen -> triggers. Triggers are read ONCE at boot, so they must all be in place
  # before the launch below; anything live goes through the FIFO afterwards.
  local files=() want_route=""
  case "$screen" in
    home)      want_route=home ;;
    # The picker is what an ORDINARY boot shows: it needs the stored session and NO
    # automation. An injected token suppresses it (token beats session), and
    # plxnative-pickuser forces it only to auto-pick a tile and move straight on — so
    # neither reaches it. Hence: no token, no triggers.
    profiles)  no_token=1; want_route=profiles ;;
    login)     files+=("plxnative-login="); want_route=login ;;
    account)   files+=("plxnative-acct="); want_route=account ;;
    # the press-and-hold card menu: the trigger snaps into the grid and holds the focused
    # card for us, because a real hold is a live gesture no boot trigger can express
    itemmenu)  files+=("plxnative-itemmenu="); want_route=itemmenu ;;
    library)   files+=("plxnative-library="); want_route=library ;;
    library=*) files+=("plxnative-library=${screen#*=}"); want_route=library ;;
    detail=*)  files+=("plxnative-detail=${screen#*=}"); want_route=detail ;;
    # the person page has no boot trigger of its own — it is REACHED, by opening a movie's
    # detail page, walking focus down to Cast & Crew (a movie's second section) and pressing
    # OK on the first headshot. So the rk here is the MOVIE's, not the person's.
    person=*)  files+=("plxnative-detail=${screen#*=}" "plxnative-detailsec=1" "plxnative-detailok=")
               want_route=person ;;
    player=*)  files+=("plxnative-play=${screen#*=}"); want_route=player ;;
    *) echo "unknown --screen: $screen" >&2; exit 2 ;;
  esac
  # capture trigger is DIAG-exempt: arming the live view must not suppress the picker
  [ -n "$stream" ] && files+=("plxnative-capture=8910")

  # NB bash 3.2 (macOS system bash) + `set -u`: "${arr[@]}" on an EMPTY array is an
  # unbound-variable error, so every expansion here is length-guarded. Screens that need
  # no triggers at all (home, profiles) hit exactly that case.
  if [ ${#files[@]} -gt 0 ]; then
    local parts=()
    for f in "${files[@]}"; do
      local name="${f%%=*}" val="${f#*=}"
      if [ "$f" = "$name=" ] || [ -z "$val" ]; then parts+=("touch /tmp/$name")
      else parts+=("printf '%s' '$val' > /tmp/$name"); fi
    done
    tv "$(IFS=';'; echo "${parts[*]}")" 2>/dev/null
    ok "armed: ${files[*]}"
  else
    info "no boot triggers needed for this screen"
  fi

  if [ "$no_token" = 0 ]; then
    if [ "$guest" = 1 ]; then
      info "guest identity: use tests/run.py (it resolves the managed-user token); booting as owner"
    fi
    push_token || info "continuing without a token — expect the QR sign-in screen"
  else
    # with a stored session this lands on the who's-watching picker; only a device with
    # no session at all falls through to QR
    info "no token by request — boots as a real user would (picker, or QR if no session)"
  fi

  PREV_PID=$(tvq 'pidof plxnative')
  relaunch
  assert_running || { info "check: tools/crash-report.sh"; exit 1; }
  assert_route "$want_route" || true

  if [ -n "$stream" ]; then
    pkill -f "stream-screen.py --port $stream" 2>/dev/null
    sleep 1
    # fully detach: without </dev/null the child keeps the caller's stdout pipe open and
    # an interactive shell appears to hang long after this script has finished
    (cd "$REPO" && nohup python3 -u tools/stream-screen.py --port "$stream" --res "${STREAM_RES:-960x540}" \
        > "$REPO/.tv-stream.log" 2>&1 </dev/null & echo $! > "$STREAM_PID_FILE") ; disown 2>/dev/null || true
    sleep 8
    local ver; ver=$(curl -s -m 3 "http://127.0.0.1:$stream/version" 2>/dev/null)
    if [ -n "$ver" ]; then ok "live view: http://127.0.0.1:$stream/  ($ver)"
    else bad "streamer did not answer on :$stream (see .tv-stream.log)"; fi
  fi

  # prove the control path end-to-end rather than assuming it
  if tvq 'test -p /tmp/plxnative-remote'; then
    ok "remote FIFO present (key/click injection ready)"
  else
    bad "no remote FIFO — the app creates it at boot; it may not be fully up yet"
  fi
  echo "== session up"
}

cmd_status() {
  echo "== session status"
  ensure_awake || exit 1
  local pid; pid=$(tvq 'pidof plxnative')
  [ -n "$pid" ] && ok "app running (pid $pid)" || bad "app not running"
  assert_route "" || true
  tvq 'ls /tmp/plxnative-* 2>/dev/null | grep -v "\.log$" | sed "s|/tmp/||"' \
    | while read -r t; do [ -n "$t" ] && info "armed: $t"; done
  local ver
  ver=$(curl -s -m 2 "http://127.0.0.1:8909/version" 2>/dev/null)
  [ -n "$ver" ] && ok "live view up on :8909 ($ver)"
}

cmd_key() {
  [ $# -gt 0 ] || { echo "usage: tv-session.sh key <token>..." >&2; exit 2; }
  # the app drains the FIFO each frame; time-box the write so a FIFO with no reader
  # (app not running) cannot wedge this shell
  for t in "$@"; do
    tv "(printf '%s\n' '$t' > /tmp/plxnative-remote) & P=\$!; sleep 2; kill \$P 2>/dev/null" 2>/dev/null
    info "sent $t"
  done
}

cmd_click() {
  [ $# -eq 2 ] || { echo "usage: tv-session.sh click <x> <y>   (authored 1920x1080)" >&2; exit 2; }
  cmd_key "ck:$1,$2"
}

cmd_shot() {
  local out="${1:-$REPO/tv-shot.png}"
  "$REPO/tools/capture-screen.sh" "$out" DISPLAY
}

cmd_log() {
  local pat="${1:-}"
  if [ -n "$pat" ]; then tvq "grep -E '$pat' /tmp/plxnative-events.log"
  else tvq 'cat /tmp/plxnative-events.log'; fi
}

cmd_down() {
  echo "== handing the TV back"
  if [ -f "$STREAM_PID_FILE" ]; then
    kill "$(cat "$STREAM_PID_FILE")" 2>/dev/null && ok "streamer stopped"
    rm -f "$STREAM_PID_FILE"
  fi
  pkill -f 'stream-screen.py --port' 2>/dev/null
  ensure_awake || exit 1
  clear_triggers                      # strips token/autoplay/capture/everything
  relaunch                            # a real interactive boot: picker or QR, as a user gets
  assert_running && ok "relaunched as a normal interactive session"
  echo "== TV is yours"
}

case "${1:-}" in
  up)     shift; cmd_up "$@" ;;
  status) shift; cmd_status ;;
  key)    shift; cmd_key "$@" ;;
  click)  shift; cmd_click "$@" ;;
  shot)   shift; cmd_shot "$@" ;;
  log)    shift; cmd_log "$@" ;;
  down)   shift; cmd_down ;;
  *) sed -n '3,26p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
