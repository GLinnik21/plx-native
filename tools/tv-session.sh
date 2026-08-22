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
# Options (accepted before OR after the subcommand, because every one of them needs it):
#   --flavor <f>      which INSTALL to drive: debug (default) | stable. Two builds live side by
#                     side on the one television, with their own app ids, their own install
#                     directories and their own runtime roots; this picks one, and the deploy,
#                     the close, the launch, the triggers and the log all follow it.
#
# `up` options:
#   --screen <name>   home (default) | profiles | library[=N] | detail=<rk> | person=<movie rk>
#                     | player=<rk> | login | account | itemmenu
#   --guest           run as the managed test user rather than the owner (default: owner)
#   --stream[=PORT]   also start tools/stream-screen.py for a live browser view (default 8909)
#                     STREAM_RES=480x270 makes mpeg encode ~4x cheaper (see the skill)
#   --remote[=PORT]   like --stream, but ALSO publish an authenticated, D-pad-only page over an
#                     HTTPS tunnel so the TV can be watched and driven from a PHONE, off-network
#                     (default dpad port 8908). Prints a URL + generated password; `down` revokes
#                     both. Needs cloudflared (brew install cloudflared). See the skill for why
#                     this is a tunnel and never a router port forward.
#   --no-token        boot with no injected token (exercises the QR sign-in flow)
#   --keep            do not clear existing triggers first (rarely what you want)
#
# WHY THIS EXISTS: every on-device task needs the same fragile ritual, and each step fails
# silently in its own way — a sleeping TV makes every assertion read as a regression, a
# stale binary means you are testing yesterday's build, a leftover trigger silently changes
# which screen you land on AND suppresses the who's-watching picker, and SAM keeps stale
# "running" state so a launch without a close-first is a no-op — and there are now TWO installs
# on the one television, so every step also has to say which of them it meant, or the ticks below
# are green against the other app's log. This asserts each step instead of assuming it.
# See .claude/skills/tv-session/SKILL.md.
#
# Config: TV host from $TV, else the Makefile's TV default. The app id, the install directory
# and the runtime root are ASKED FOR (`make -s print-…`), never restated here — see the block
# under REPO for why a literal copy of them is a bug waiting to happen. The PMS token is read
# from the gitignored src/config.local.h at runtime, written straight to the TV in its own ssh
# round-trip, and never printed. Nothing about the network or any credential lives here.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# --flavor is stripped out of the argv HERE, ahead of the dispatch at the bottom, because it is
# not an `up` option — `log` reads a different file, `status` lists a different directory and
# `down` closes a different app id. Threading it through each subcommand's own parser is exactly
# how one of them would keep the default while the rest moved.
FLAVOR=""
_argv=()
while [ $# -gt 0 ]; do
  case "$1" in
    # `shift 2` with only one positional left is a NO-OP that returns 1, and neither script sets
    # -e — so a bare trailing `--flavor` (a shell that ate the value, a tab-completion stop) left
    # $# unchanged and this loop spun at 100% CPU forever, printing nothing. Shift what is
    # actually there instead.
    --flavor)   FLAVOR="${2:-}"; shift; [ $# -gt 0 ] && shift ;;
    --flavor=*) FLAVOR="${1#*=}"; shift ;;
    *)          _argv+=("$1"); shift ;;
  esac
done
# bash 3.2 (macOS system bash) + `set -u`: "${arr[@]}" on an EMPTY array is an unbound-variable
# error, and a bare `tv-session.sh` with no arguments is precisely that case.
set -- ${_argv[@]+"${_argv[@]}"}

# WHICH INSTALL — asked for, never restated. This file used to carry
# `APPDIR=/media/developer/apps/usr/palm/applications/com.beb.plxnative` and a matching `APPID=`
# as literals, which was fine while there was one install and became a second source of truth the
# moment a second one landed: the Makefile derives all of this from FLAVOR, and a copy here would
# go stale silently, pointing the deploy at one app and the log at another.
#
# The default comes from the same place (`print-flavor` with nothing passed IS the Makefile's
# default, today the developer install), then one invocation returns all six in goal order. The
# flavour name is validated for free — an unknown FLAVOR is a parse-time $(error) in the Makefile,
# so a typo stops here instead of resolving to six plausible-looking wrong paths. These
# `print-*` targets are the ONLY supported way to ask;
# `make -p` prints a recursive variable's UNEXPANDED definition, which is the trap documented on
# tv_host below.
#
# APPPORT is the app's capture listener and belongs to the install like everything else here:
# 8910 for the shipped app, 8911 for a flavoured one, so two installs cannot fight over one
# socket (`capture::default_port()` is the same rule, and ci/flavor.py --selftest cross-checks
# them). It is read once and used TWICE below — the content of the `plxnative-capture` trigger
# and the streamer's `--app-port` — because those two must be the same number and a literal in
# either place is how the picture ends up on one install while the keys go to the other.
: "${FLAVOR:=$(make -s -C "$REPO" print-flavor)}"
{ read -r FLAVOR; read -r APPID; read -r APPDIR; read -r RUNDIR; read -r EVENTLOG; read -r APPPORT; } < <(
  make -s -C "$REPO" FLAVOR="$FLAVOR" \
       print-flavor print-appid print-appdir print-rundir print-eventlog print-appport
)
# On a bad flavour make has already printed the exact complaint; do not restate it wrongly —
# the failed `read` above left FLAVOR empty, so echoing it back here would name nothing.
# Test the LAST value read, not a middle one: a short answer (an older Makefile missing a goal)
# fills the earlier variables and leaves only the tail empty.
[ -n "${APPPORT:-}" ] || { echo "cannot resolve the flavour above from $REPO/Makefile" >&2; exit 2; }
# The app mkfifos this at boot inside its own runtime root; the NAME is unchanged across flavours,
# only the directory moved.
REMOTE_FIFO="$RUNDIR/plxnative-remote"

STREAM_PID_FILE="$REPO/.tv-stream.pid"
# --remote's three extra processes. Each gets a pid file so `down` revokes the published URL and
# the password even if this shell is long gone — a tunnel that outlives the session it was opened
# for is the one failure mode here that is silent AND outward-facing.
DPAD_PID_FILE="$REPO/.tv-dpad.pid"
TUNNEL_PID_FILE="$REPO/.tv-tunnel.pid"
REMOTE_URL_FILE="$REPO/.tv-remote-url"
DPAD_PASS_FILE="$REPO/.tv-dpad-pass"

# Stop everything on THIS machine that holds a socket to the app's capture stream.
#
# Order matters and it is the trap that costs an hour: the app serves one capture client per
# connection and does NOT hang up on a dead peer, so a streamer left running across a relaunch
# leaves a stale client on the app. The encoder keeps running (the event log keeps printing
# `venc: N frm ...`), the TS goes to the dead socket, and the new streamer sees ZERO bytes while
# every log line says the pipeline is healthy. So this runs BEFORE the relaunch, not after.
stop_viewers() {
  for f in "$TUNNEL_PID_FILE" "$DPAD_PID_FILE" "$STREAM_PID_FILE"; do
    [ -f "$f" ] && { kill "$(cat "$f")" 2>/dev/null; rm -f "$f"; }
  done
  pkill -f 'stream-screen.py --port' 2>/dev/null
  pkill -f 'remote-dpad.py --port'   2>/dev/null
  pkill -f 'cloudflared tunnel --url' 2>/dev/null
  rm -f "$REMOTE_URL_FILE" "$DPAD_PASS_FILE"
  return 0
}

tv_host() {
  [ -n "${TV:-}" ] && { echo "$TV"; return; }
  # `.tv-host` FIRST, because it is the source the Makefile itself reads (`TV ?= $(strip $(shell cat
  # .tv-host))`). Asking make for the value with `-pn` does NOT work and used to be what this did:
  # `-p` prints the DEFINITION, and a recursive `=` variable's definition is the literal
  # `$(strip $(shell cat .tv-host ...))` text — so HOST became that string, every ssh failed with
  # "hostname contains invalid characters", and `up` reported **"TV unreachable"** on a television
  # that was awake and answering. Precisely the mimicry the header warns about, from the driver.
  [ -f "$REPO/.tv-host" ] && { tr -d '[:space:]' < "$REPO/.tv-host"; return; }
  # a hardcoded `TV = 1.2.3.4` in the Makefile, for a checkout with no .tv-host
  sed -n 's/^TV *[?:]*= *\([0-9a-zA-Z.:_-]\{1,\}\) *$/\1/p' "$REPO/Makefile" | head -1
}
HOST="$(tv_host)"
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=8)
tv()  { ssh "${SSH_OPTS[@]}" "root@$HOST" "$@"; }
tvq() { ssh "${SSH_OPTS[@]}" "root@$HOST" "$@" 2>/dev/null; }

# `pidof plxnative` matched BOTH installs the moment a second flavour landed: the binaries are
# both named `plxnative`, and it hands back two pids in an order busybox does not promise. `fuser`
# on the resolved install's own binary is INODE-scoped, so it answers for exactly the app this
# invocation is driving. Keep only the digits — busybox prints bare pids, other fusers prefix the
# path (which has none), so this normalises both to a plain space-separated list.
app_pids() {
  tvq "fuser $APPDIR/plxnative" | tr -cs '0-9' ' ' | sed 's/^ *//; s/ *$//'
}

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

# ------------------------------------------------------------- rundir --------
# The runtime root has to exist, and it has to be 1777, BEFORE anything writes into it — and the
# chmod is separate from the mkdir on purpose, because the umask masks mkdir's mode and would
# leave it owner-only. Two uids write here and neither can be made to go second: this script arms
# triggers and pushes the token AS ROOT over ssh before the app has ever booted, while the app runs
# jailed under its own uid and creates the event log there. Owner-only locks one of them out, and a
# root-owned event log the app cannot write stays 0 bytes — which every tool in this repo, this one
# included, reports as "no line found", i.e. indistinguishable from a total regression.
#
# On the stable flavour this is /tmp, which already exists 1777; the work is real only for a
# flavoured install, whose root is /tmp/<app id>.
ensure_rundir() {
  if tv "mkdir -p $RUNDIR && chmod 1777 $RUNDIR" 2>/dev/null; then
    ok "runtime root $RUNDIR ready (1777)"; return 0
  fi
  bad "cannot create $RUNDIR on the TV"; return 1
}

# ----------------------------------------------------------- installed -------
# A deploy cannot create an install. `make deploy` scp's into a directory appinstalld already
# registered; a directory SAM knows nothing about is not an app, and one conjured with `mkdir -p`
# would take a deploy, take a binary, and then never launch. Assert it here so the failure names
# the one command that fixes it, rather than surfacing three steps later as a deploy that "did
# not land" — which reads as a flaky scp and invites re-running `up` forever.
ensure_installed() {
  if tvq "test -d $APPDIR"; then ok "$APPID is installed"; return 0; fi
  bad "$APPDIR does not exist on the TV — the $FLAVOR flavour is not installed"
  info "install it once:  make FLAVOR=$FLAVOR install"
  return 1
}

# ------------------------------------------------------------- deploy --------
ensure_binary() {
  [ -f "$REPO/pkg/plxnative" ] || { bad "no pkg/plxnative — run make"; return 1; }
  local l t
  l=$(md5 -q "$REPO/pkg/plxnative" 2>/dev/null || md5sum "$REPO/pkg/plxnative" | cut -d' ' -f1)
  t=$(tvq "md5sum $APPDIR/plxnative" | cut -d' ' -f1)
  # This compares BYTES, and `pkg/plxnative` is a path that every flavour and both configurations
  # write — so a match says "these are the bytes on my disk right now", never "this is the install
  # I asked for". That second question is settled by assert_install, on the app's own boot line.
  if [ "$l" = "$t" ]; then ok "deployed binary matches local build"; return 0; fi
  info "binary differs — deploying to $APPID"
  # THE SAME flavour that was resolved above. A deploy that fell back to the Makefile default
  # would write install A's directory and then launch install B below — SAM's stale-running no-op,
  # after which every assertion here grades the other app's log.
  # CAPTURED, not discarded. `release-guard` refuses a dev build on the stable id and explains
  # itself in three lines including the ALLOW_DEV_ON_STABLE=1 hatch — all of which went to
  # /dev/null, after which the md5 still differed and the operator got two diagnoses that are both
  # wrong for that case ("the TV may have slept", "run make install"). Both fail identically on a
  # refusal, so the refusal has to reach them.
  if ! _deploy_out=$(make -C "$REPO" FLAVOR="$FLAVOR" deploy 2>&1); then
    bad "make FLAVOR=$FLAVOR deploy failed — its own words follow"
    printf '%s\n' "$_deploy_out" | sed 's/^/    /'
    return 1
  fi
  t=$(tvq "md5sum $APPDIR/plxnative" | cut -d' ' -f1)
  # a standby can truncate an scp mid-flight, so verify rather than trust
  [ "$l" = "$t" ] && { ok "deployed + md5 verified"; return 0; }
  # Re-running `up` fixes the standby case and nothing else, so name the other one too — see
  # ensure_installed above, which is the step that has already said so if it applies.
  bad "deploy did not land (md5 still differs) — TV may have slept mid-scp; re-run up"
  info "if $APPDIR is missing, no amount of re-running helps: make FLAVOR=$FLAVOR install"
  return 1
}

# ------------------------------------------------------------ triggers -------
# GLOB-clear, exactly like tests/run.py: a newly-added app trigger can never bleed in
# from a previous session, and any leftover non-DIAG file also suppresses the picker.
clear_triggers() {
  tv "for f in $RUNDIR/plxnative-*; do case \"\$f\" in *.log) ;; *) rm -f \"\$f\";; esac; done" 2>/dev/null
  ok "triggers cleared ($RUNDIR)"
}

# token: read host-side, pushed in its own round-trip, never echoed
push_token() {
  local tok
  tok=$(sed -n 's/.*PMS_TOKEN *"\([^"]*\)".*/\1/p' "$REPO/src/config.local.h" 2>/dev/null | head -1)
  [ -n "$tok" ] || { bad "no PMS_TOKEN in src/config.local.h (gitignored) — boot will hit QR sign-in"; return 1; }
  printf '%s' "$tok" | tv "cat > $RUNDIR/plxnative-token" || return 1
  ok "token injected (value not printed)"
}

# ------------------------------------------------------------- launch --------
relaunch() {
  # SAM keeps stale "running" state after a hard kill, so a launch without a close-first
  # is a silent no-op relaunch — `make kill` is the proven close path.
  # …for THIS flavour: `make kill` closes by app id, so a default-flavour close would leave the
  # app we are about to grade running and shut the other one down instead.
  make -C "$REPO" FLAVOR="$FLAVOR" kill >/dev/null 2>&1
  sleep 2
  # luna-send must STAY SUBSCRIBED (-i) for the launch to take, which means the SSH
  # session has to stay OPEN while it does: backgrounding it and letting ssh return
  # kills the subscriber and the launch silently no-ops (the app keeps running as it
  # was, so everything downstream looks fine while testing the OLD instance).
  tv "rm -f $EVENTLOG; \
      luna-send -i luna://com.webos.applicationManager/launch '{\"id\":\"$APPID\"}' >/dev/null 2>&1 & \
      LP=\$!; sleep 8; kill \$LP 2>/dev/null" >/dev/null 2>&1
}

PREV_PIDS=""
assert_running() {
  local pid; pid=$(app_pids)
  if [ -z "$pid" ]; then bad "$APPID is not running after launch"; return 1; fi
  if [ -n "$PREV_PIDS" ] && [ "$pid" = "$PREV_PIDS" ]; then
    bad "app pid unchanged ($pid) — the relaunch did NOT take; you would be testing the old instance"
    return 1
  fi
  ok "$APPID running (pid $pid)"; return 0
}

# The event log's own first line names the install that wrote it. Check it, because nothing else
# can: both binaries are called `plxnative`, and `pkg/plxnative` is a path every flavour and every
# configuration writes, so the md5 above proves only "some flavour of some configuration". Without
# this, driving the wrong install produces a full page of green ticks against the other app's log.
assert_install() {
  local line seen feat
  line=$(tvq "grep '^install: id=' $EVENTLOG 2>/dev/null | head -1")
  if [ -z "$line" ]; then
    bad "no install: line in $EVENTLOG — that is the FIRST thing the app writes"
    info "so this log is another install's leftover, or the app died before plex_run:"
    info "    tools/crash-report.sh --flavor $FLAVOR"
    return 1
  fi
  seen=${line#*id=}; seen=${seen%% *}
  feat=$(printf '%s' "$line" | sed -n 's/.*features=\([a-z]*\).*/\1/p')
  if [ "$seen" = "$APPID" ]; then ok "log written by $seen (${feat:-?} build)"; return 0; fi
  bad "log was written by $seen, not $APPID — every assertion below would grade the OTHER install"
  return 1
}

assert_route() {
  local want="$1" seen
  seen=$(tvq "grep -oE 'route=[a-z]+' $EVENTLOG 2>/dev/null | tail -1")
  if [ -z "$seen" ]; then
    bad "no route= heartbeat in $EVENTLOG yet (app booting, or it died)"
    info "if it died: tools/crash-report.sh --flavor $FLAVOR"
    return 1
  fi
  info "reached ${seen}"
  [ -z "$want" ] && return 0
  [ "$seen" = "route=$want" ] && { ok "on the requested screen"; return 0; }
  bad "wanted route=$want, got $seen"; return 1
}

# ------------------------------------------------------------ commands -------
cmd_up() {
  local screen=home guest=0 stream="" no_token=0 keep=0 remote=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --screen) screen="$2"; shift 2 ;;
      --screen=*) screen="${1#*=}"; shift ;;
      --guest) guest=1; shift ;;
      --stream) stream=8909; shift ;;
      --stream=*) stream="${1#*=}"; shift ;;
      --remote) remote=8908; shift ;;
      --remote=*) remote="${1#*=}"; shift ;;
      --no-token) no_token=1; shift ;;
      --keep) keep=1; shift ;;
      *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
  done
  # --remote is --stream plus a front door: there is nothing to publish without a stream, so it
  # turns one on rather than making the caller remember to pass both.
  if [ -n "$remote" ]; then
    [ -z "$stream" ] && stream=8909
    if ! command -v cloudflared >/dev/null 2>&1; then
      bad "--remote needs cloudflared (brew install cloudflared)"; exit 1
    fi
  fi
  # Before the relaunch, never after — see stop_viewers.
  stop_viewers

  echo "== bringing up the TV session ($screen) on $APPID [$FLAVOR]"
  ensure_awake  || exit 1
  ensure_installed || exit 1
  ensure_rundir || exit 1
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
    # The session is PER-INSTALL (paths::session_candidates names auth.json for the app id,
    # and the legacy in-app-dir entry is offered only to the shipped install), so this screen
    # is reachable on the flavour that signed itself in and lands on QR on the other. That is
    # the design — two installs are two devices to the account — not a broken picker.
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
  # capture trigger is DIAG-exempt: arming the live view must not suppress the picker.
  # The content is the port to listen on, and it is written EXPLICITLY rather than left empty
  # (which would fall through to the app's own default) so that the number the app binds and the
  # number the streamer dials are the one variable resolved at the top of this file.
  [ -n "$stream" ] && files+=("plxnative-capture=$APPPORT")

  # NB bash 3.2 (macOS system bash) + `set -u`: "${arr[@]}" on an EMPTY array is an
  # unbound-variable error, so every expansion here is length-guarded. Screens that need
  # no triggers at all (home, profiles) hit exactly that case.
  if [ ${#files[@]} -gt 0 ]; then
    local parts=()
    for f in "${files[@]}"; do
      local name="${f%%=*}" val="${f#*=}"
      if [ "$f" = "$name=" ] || [ -z "$val" ]; then parts+=("touch $RUNDIR/$name")
      else parts+=("printf '%s' '$val' > $RUNDIR/$name"); fi
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
    # with a stored session this lands on the who's-watching picker; only an install with
    # no session of its OWN falls through to QR — the file is named for the app id, so the
    # other flavour having signed in does not count
    info "no token by request — boots as a real user would (picker, or QR if $APPID has no session)"
  fi

  PREV_PIDS=$(app_pids)
  relaunch
  assert_running || { info "check: tools/crash-report.sh --flavor $FLAVOR"; exit 1; }
  assert_install || true
  assert_route "$want_route" || true

  if [ -n "$stream" ]; then
    # fully detach: without </dev/null the child keeps the caller's stdout pipe open and
    # an interactive shell appears to hang long after this script has finished.
    # `--source app` rather than the default `auto`: auto silently falls back to the ~3fps luna
    # service capture, which over a tunnel reads as "the app is broken" rather than "the source
    # changed". Better to fail loudly on the fast path than succeed slowly on the slow one.
    # `--runtime-dir` is not optional here even though it has a default: the streamer resolves the
    # app's remote FIFO from it, and its default is the Makefile's FLAVOR, not this session's. Left
    # off, a `--flavor stable` session would show the stable install's picture while dropping every
    # key the browser page sends into the DEBUG install's FIFO — a live view that looks fine and is
    # driving the wrong app.
    # `--app-port` for the same reason on the picture half: the port belongs to the install too,
    # and it is passed rather than left to the streamer's own default so that it is the SAME
    # `$APPPORT` already written into the capture trigger above. Disagreeing halves do not error —
    # the app listens on one port, the streamer dials the other, finds nothing, and `--source app`
    # simply never produces a frame.
    (cd "$REPO" && nohup python3 -u tools/stream-screen.py --port "$stream" --res "${STREAM_RES:-960x540}" \
        --source app --runtime-dir "$RUNDIR" --app-port "$APPPORT" \
        > "$REPO/.tv-stream.log" 2>&1 </dev/null & echo $! > "$STREAM_PID_FILE") ; disown 2>/dev/null || true
    sleep 8
    local ver; ver=$(curl -s -m 3 "http://127.0.0.1:$stream/version" 2>/dev/null)
    # "<ver> <mode> app=<id> runtime=<dir>", the first two fields POSITIONAL. `mode` is `jpeg`
    # until TS has actually flowed (stream-screen's _mode has a 6s window), so a `mpeg` here is
    # the real end-to-end proof that the encoder reached us; `app=` is the streamer's own answer
    # for WHICH install it is watching, which is why the whole line is echoed rather than a field.
    if [ -n "$ver" ]; then ok "live view: http://127.0.0.1:$stream/  ($ver)"
    else bad "streamer did not answer on :$stream (see .tv-stream.log)"; fi
  fi

  if [ -n "$remote" ]; then
    local pw; pw=$(python3 -c 'import secrets;print(secrets.token_urlsafe(12))')
    printf '%s' "$pw" > "$DPAD_PASS_FILE"; chmod 600 "$DPAD_PASS_FILE"
    # `--runtime-dir` for the same reason the streamer gets it, minus the consequence: this
    # process never writes the FIFO, so passing it cannot send a key anywhere wrong. What it
    # buys is the `install:` banner, which remote-dpad only prints when it was told — and this
    # is the session that is driven from a PHONE, off-network, where "which of the two installs
    # am I looking at" is hardest to check and easiest to get wrong.
    (cd "$REPO" && nohup python3 -u tools/remote-dpad.py --port "$remote" --upstream "$stream" \
        --runtime-dir "$RUNDIR" \
        --user tv --password "$pw" > "$REPO/.tv-dpad.log" 2>&1 </dev/null & echo $! > "$DPAD_PID_FILE") ; disown 2>/dev/null || true
    sleep 2
    if ! curl -s -m 3 -o /dev/null "http://127.0.0.1:$remote/"; then
      bad "d-pad front end did not answer on :$remote (see .tv-dpad.log)"; return 1
    fi
    # 401 unauthenticated is the assertion that matters: the tunnel is about to make this
    # reachable from the internet, so "it serves the page" is not the thing to check.
    local code; code=$(curl -s -o /dev/null -w '%{http_code}' -m 3 "http://127.0.0.1:$remote/")
    [ "$code" = "401" ] && ok "d-pad front end up, unauthenticated requests refused (401)" \
                        || bad "front end answered $code unauthenticated — expected 401"
    (cd "$REPO" && nohup cloudflared tunnel --url "http://127.0.0.1:$remote" --no-autoupdate \
        > "$REPO/.tv-tunnel.log" 2>&1 </dev/null & echo $! > "$TUNNEL_PID_FILE") ; disown 2>/dev/null || true
    local url="" i=0
    while [ $i -lt 30 ] && [ -z "$url" ]; do
      url=$(grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' "$REPO/.tv-tunnel.log" 2>/dev/null | head -1)
      [ -z "$url" ] && { sleep 1; i=$((i+1)); }
    done
    if [ -z "$url" ]; then bad "tunnel did not publish a URL (see .tv-tunnel.log)"; return 1; fi
    printf '%s' "$url" > "$REMOTE_URL_FILE"
    ok "remote view: $url"
    echo "     user: tv"
    echo "     pass: $pw"
    echo "     d-pad only; pointer clicks and transport keys are refused at the proxy."
    echo "     \`tv-session.sh down\` revokes this URL."
  fi

  # prove the control path end-to-end rather than assuming it
  if tvq "test -p $REMOTE_FIFO"; then
    ok "remote FIFO present (key/click injection ready)"
  else
    bad "no remote FIFO — the app creates it at boot; it may not be fully up yet"
  fi
  echo "== session up"
}

cmd_status() {
  echo "== session status: $APPID [$FLAVOR]"
  ensure_awake || exit 1
  local pid; pid=$(app_pids)
  [ -n "$pid" ] && ok "app running (pid $pid)" || bad "app not running"
  assert_install || true
  assert_route "" || true
  tvq "ls $RUNDIR/plxnative-* 2>/dev/null | grep -v '\.log\$' | sed 's|.*/||'" \
    | while read -r t; do [ -n "$t" ] && info "armed: $t"; done
  local ver
  ver=$(curl -s -m 2 "http://127.0.0.1:8909/version" 2>/dev/null)
  [ -n "$ver" ] && ok "live view up on :8909 ($ver)"
  # A published URL must be discoverable from a cold shell — otherwise the only way to learn the
  # TV is on the internet is to remember opening it.
  if [ -f "$REMOTE_URL_FILE" ]; then
    ok "remote view PUBLISHED: $(cat "$REMOTE_URL_FILE")"
    [ -f "$DPAD_PASS_FILE" ] && info "user tv / pass $(cat "$DPAD_PASS_FILE")"
    info "reachable from outside this network until \`tv-session.sh down\`"
  fi
}

cmd_key() {
  [ $# -gt 0 ] || { echo "usage: tv-session.sh key <token>..." >&2; exit 2; }
  # the app drains the FIFO each frame; time-box the write so a FIFO with no reader
  # (app not running) cannot wedge this shell
  for t in "$@"; do
    tv "(printf '%s\n' '$t' > $REMOTE_FIFO) & P=\$!; sleep 2; kill \$P 2>/dev/null" 2>/dev/null
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
  if [ -n "$pat" ]; then tvq "grep -E '$pat' $EVENTLOG"
  else tvq "cat $EVENTLOG"; fi
}

cmd_down() {
  echo "== handing the TV back"
  local had_url=0; [ -f "$REMOTE_URL_FILE" ] && had_url=1
  stop_viewers
  ok "streamer stopped"
  # Say it out loud. A published URL is the one thing here that outlives the terminal, so
  # "it is gone now" has to be visible in the handback and not merely true.
  [ "$had_url" = 1 ] && ok "remote URL revoked (tunnel closed, password discarded)"
  ensure_awake || exit 1
  ensure_rundir || exit 1
  clear_triggers                      # strips token/autoplay/capture/everything
  relaunch                            # a real interactive boot: picker or QR, as a user gets
  assert_running && ok "relaunched as a normal interactive session ($APPID)"
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
  *) sed -n '3,31p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
