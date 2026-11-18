#!/usr/bin/env bash
#
# tv-session.sh — bring the app up on the TV in a known state, drive it, hand the TV back.
#
#   tv-session.sh up [opts]      wake -> deploy-if-stale -> clear triggers -> arm -> launch -> verify
#   tv-session.sh status         re-assert an existing session without disturbing it
#   tv-session.sh key <token>…   send key tokens (down, ok, back, play, pause, stop, …); never `focus:`,
#                                `press:` or `state:` (the three verbs below are their one door)
#   tv-session.sh click <x> <y>  click at authored 1920x1080 coords
#   tv-session.sh focus <id>     move focus to the element an accessibility id names (`nav.watchlist`)
#   tv-session.sh press <id> [--rk <ratingKey>]
#                                focus that element and press OK; an element that acts on a title it names
#                                (the list is `SUBJECT_BEARING` in tools/a11y.py) needs --rk: see "BY ID" below
#   tv-session.sh state [--out FILE]
#                                what is on screen, by id: the focused id, the groups, every element
#   tv-session.sh shot [out.png] grab the panel (video plane included) via the capture service
#   tv-session.sh log [pattern]  fetch the on-device event log, optionally grepped
#   tv-session.sh screen off|on  blank the PANEL while the app keeps running (see below)
#   tv-session.sh sound off|on|status
#                                mute/unmute the TELEVISION, independent of the panel (see below)
#   tv-session.sh background [APPID]|foreground|fg
#                                send the app to the background / bring it back (see below)
#   tv-session.sh wan off [TTL]|on|status
#                                cut the TELEVISION's route to the internet, LAN intact (see below)
#   tv-session.sh down           hand the TV back: strip automation, close the app (no relaunch)
#
# Options (accepted before OR after the subcommand, because every one of them needs it):
#   --flavor <f>      which INSTALL to drive: debug (default) | stable. Two builds live side by
#                     side on the one television, with their own app ids, their own install
#                     directories and their own runtime roots; this picks one, and the deploy,
#                     the close, the launch, the triggers and the log all follow it.
#
# `up` options:
#   --screen <name>   home (default) | profiles | library[=N] | detail=<rk> | collection=<rk> | person=<movie rk>
#                     | player=<rk> | login | account | itemmenu
#   --server <slot>   open detail=/player= on this registered Plex server slot instead of the
#                     current one; boots through the signed-in stored roster so secondary slots
#                     exist (an already-armed plxnative-servers also survives with --keep)
#   --guest           boot as tests/manifest.local.json's managed TEST USER, never the owner.
#                     Reuses run.py's own identity resolution (fetch_managed_user_token, the
#                     plex.tv shared_servers lookup) rather than re-deriving it, so the two
#                     never disagree about what "guest" means. REFUSES (exit 1) rather than
#                     falling through to the owner when no manifest.local.json/test_user is
#                     configured — see the 2026-09-10 postmortem below `cmd_up`'s definition:
#                     a caller asked for a guest, got the owner silently, and a real playback
#                     wrote progress into the household account.
#   --owner           boot as the household account (the config.local.h owner token). This is
#                     also what a bare `up` does with neither flag — spelling it out is for a
#                     script that wants the choice to be visible in its own command line.
#   --mock            with --guest, use only tests/mock_pms.py at the configured PMS endpoint.
#                     Verifies its synthetic identity; never uses an account token or contacts plex.tv.
#   --dry-run         resolve and print the identity `up` would use (including a real --guest
#                     token lookup, which touches plex.tv but never the TV) and the shape of the
#                     commands it would run, then exit 0 without contacting the television at all.
#   --stream[=PORT]   also start tools/stream-screen.py for a live browser view (default 8909)
#                     STREAM_RES=480x270 makes mpeg encode ~4x cheaper (see the skill)
#   --remote[=PORT]   like --stream, but ALSO publish an authenticated, D-pad-only page over an
#                     HTTPS tunnel so the TV can be watched and driven from a PHONE, off-network
#                     (default dpad port 8908). Prints a URL + generated password; `down` revokes
#                     both. Needs cloudflared (brew install cloudflared). See the skill for why
#                     this is a tunnel and never a router port forward.
#   --no-token        boot with no injected token (exercises the QR sign-in flow, or the
#                     who's-watching picker for a stored session — a THIRD identity, distinct
#                     from both --guest and --owner)
#   --arm NAME[=VAL]  also arm trigger plxnative-NAME (repeatable), written with the screen's own
#                     triggers BEFORE the launch, which is the only time a boot trigger is read.
#                     For a scene's extras, e.g. `--arm menu=1 --arm submenuosc=900 --arm framedrop=25`
#   --mock-plextv=PORT
#                     sign the TV in through tests/mock_pms.py's own plex.tv (the mock must be
#                     serving on THIS host at PORT, started with `--host 0.0.0.0 --advertise-ip
#                     <this host's LAN IPv4> --authorize-after 2`, so its resources row names a
#                     LAN address the set can reach). Debug install only. Arms
#                     `plxnative-plextv=http://127.0.0.1:PORT` and opens the reverse ssh tunnel
#                     that makes that loopback address real ON THE DEVICE (the trigger accepts only
#                     loopback, deliberately). Implies --no-token; pair it with `--screen login`:
#                     the QR sign-in authorizes itself on the 2nd poll and the mock's server arrives
#                     OWNED, so the owned-only rules (server updates) run unmodified. Refuses
#                     --guest/--owner, any screen but login, --server, --keep, and a mock whose
#                     identity is not the synthetic one. The
#                     tunnel lives in `.tv-plextv.pid`; `down` (and the next `up`) closes it.
#   --keep            do not clear existing triggers first (rarely what you want)
#
# SCREEN OFF is a panel state, not an app state. `luna://com.webos.service.tvpower/power/
# turnOffScreen` blanks the picture with the set still on, the app still running and playback
# still decoding — it does NOT deliver the SDL background events (`0x103`/`0x104`) that make
# `app.rs` suspend the buffer-feed and drop to Home, which is the failure worth ruling out before
# using it: a suspended feed reads as a total ABR regression rather than as an obviously blank
# screen. Device-confirmed on webOS 4.10.0 (2026-08-27), and the whole P1/P1b device corpus was
# taken this way. `power/turnOnScreen` is the reverse; both are on this firmware's api-permissions
# list, unlike the newer-webOS `power/turnOff` the wake-tv skill records as `Unknown method` here.
#
# Screen off is only the backlight, and it is fine for every measurement tier — ABR, transaction
# cost, anything read out of the event log, and the fps scenes too (`tests/run.py --fps --mock`
# blanks it by default). Nothing in the app reads the panel state, so the app keeps presenting at full rate:
# `plx_machine::idle` keys on the UI's own changes ("nothing on screen changed, so don't repaint
# it"), not on the physical panel. Measured on the dev TV 2026-10-24 (Home `grid-pong`, release
# build): panel off 73 frames/cycle at 59.9 fps with 9 missed refreshes in cycles 2-60; panel on
# 73 frames/cycle at 59.6-59.9 fps with 8, 13 and 14. It saves the panel over long runs and costs
# nothing.
# `shot` (the DISPLAY compositor read) works with the panel off: docs/measurements/
# tv-session-7-2026-09-10.md ran `screen off` first and got a 1920x1080 DISPLAY capture with real
# pixel values (opener p95 246.9, row-mate 167.0), and docs/measurements/
# library-all-types-and-fps-2026-09-26.md checked native captures of the final card rendering with
# the panel off. NOT on record: the live capture stream (`/tmp/plxnative-capture`, the app's own
# GLES frames) with the panel off; check the first frame instead of assuming either way.
#
# SOUND is the television's own mute, separate from the panel above and from playback: it silences
# whatever the set would otherwise put out, panel on or off, app running or not.
# `luna://com.webos.service.audio/setMuted` flips the flag and `com.webos.service.audio/getVolume`
# reads it back. Both calls were exercised by hand on the set on 2026-09-19 — `setMuted` returned
# success and an independent `getVolume` then reported `muted:true` — but the SUBCOMMAND around
# them is host-tested only and has never run against a television. Whoever uses it first: watch
# what the set actually does and record it. `off`/`on` call `setMuted` and then RE-READ `getVolume` to
# confirm, rather than trusting `setMuted`'s own `returnValue` — the same discipline `ensure_binary`
# already uses for a deploy. `status` only reads `getVolume`. Like `screen`, this drives the set and
# takes the TV lock.
#
# THIS IS THE SANCTIONED PATH for muting the television. Before it existed, the only ways to
# silence a run were the physical remote or a raw `luna-send` reached through
# `PLX_TV_LOCK_BYPASS=1` around the lock guard — neither belongs in an automated lane, and the
# bypass in particular is meant for a human who knows the set is theirs, not for routine muting. No
# lane needs it for this: `tv-session.sh sound off` is the tool.
#
# It does NOT restore sound at teardown — `down` does not call it — because a lane that muted for
# its own reasons is the only one that knows when unmuting is correct; restoring automatically
# would fight a human who muted the set on purpose before handing it to a lane.
#
# BY ID, NOT BY WALK: `focus`, `press` and `state` are the FIFO's accessibility-id tokens (`focus:<id>`,
# `press:<id>[@<ratingKey>]`, `state:<nonce>`; debug builds only, `devtriggers`). Ids are place-based dot paths
# (`nav.watchlist`, `home.hero.play`, `detail.hero.play`, `watchlist.grid.card.3`); `state` lists the real ones.
# Prefer them to `key` walks, which are for tests whose subject is travel. Never press OK on a title whose
# ratingKey you have not read: `press` refuses, without `--rk`, any id whose element ACTS ON a title it names (it
# plays, restarts, finishes or retries it, marks it watched or unwatched, bookmarks it, acts on the title a menu was
# opened for, or opens that title's own page or menu where the title can change under the press: a card, a credit,
# the Home carousel's Info). Navigation is not the test: only a control whose target the page fixes (Go to Show, Also
# available, the Play Version opener, More, a panel opener, a seek) is pressed bare. The list of those ids is
# `SUBJECT_BEARING` in tools/a11y.py, read off the code (`a11y_subject_bearing` below is the same list), and the
# `--rk` is the ratingKey of the title the press acts on, which `state` prints as the element's subject (on a started
# show's Mark watched it is the on-deck episode, on a player's Next the queue neighbour, not the page's or the playing
# item's). The app
# refuses the press when the element now there carries another subject (`a11y: refused press <id> subject_mismatch
# ...` in the event log). `key` refuses a
# `focus:`, `press:` or `state:` word, so there is ONE door per action, and a word with whitespace in it is
# refused (the app would cut it into two tokens, the second past every check here).
# A `state` snapshot holds titles and rating keys: it is for you, not for an issue or a recording.
#
# THE TV LOCK: every subcommand that DRIVES the set (up, key, click, focus, press, state, shot, down) requires the
# television's lock and refuses when another lane holds it; `status`, `log` and `fg` are read-only
# and only name the holder. Take one for a whole session rather than letting each command take its own
# 10-minute implicit lease — the gap between two of your own commands is exactly where another
# job lands:  tools/tv-lock.sh acquire --why "…"  …  tools/tv-lock.sh release.
#
# WHY THIS EXISTS: every on-device task needs the same fragile ritual, and each step fails
# silently in its own way — a sleeping TV makes every assertion read as a regression, a
# stale binary means you are testing yesterday's build, a leftover trigger silently changes
# which screen you land on AND suppresses the who's-watching picker, and SAM keeps stale
# "running" state so a launch without a close-first is a no-op — and there are now TWO installs
# on the one television, so every step also has to say which of them it meant, or the ticks below
# are green against the other app's log. This asserts each step instead of assuming it.
# See .agents/skills/tv-session/SKILL.md.
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
# `print-tv` in the batched query below.
#
# APPPORT is the app's capture listener and belongs to the install like everything else here:
# 8910 for the shipped app, 8911 for a flavoured one, so two installs cannot fight over one
# socket (`capture::default_port()` is the same rule, and ci/flavor.py --selftest cross-checks
# them). It is read once and used TWICE below — the content of the `plxnative-capture` trigger
# and the streamer's `--app-port` — because those two must be the same number and a literal in
# either place is how the picture ends up on one install while the keys go to the other.
# ONE invocation. `print-flavor` comes back first and answers the Makefile's own default when
# nothing set FLAVOR, so the pre-fetch that used to stand above this line — a whole second Makefile
# parse to learn a value the composed query already returns — is gone. `${FLAVOR:+…}` passes the
# assignment only when there is one, which is what lets make answer with its default.
{ read -r FLAVOR; read -r APPID; read -r APPDIR; read -r RUNDIR; read -r EVENTLOG; read -r APPPORT; read -r HOST; } < <(
  make -s -C "$REPO" ${FLAVOR:+FLAVOR="$FLAVOR"} \
       print-flavor print-appid print-appdir print-rundir print-eventlog print-appport print-tv
)
# On a bad flavour make has already printed the exact complaint; do not restate it wrongly —
# the failed `read` above left FLAVOR empty, so echoing it back here would name nothing.
# Test the LAST value read, not a middle one: a short answer (an older Makefile missing a goal)
# fills the earlier variables and leaves only the tail empty. That tail is `print-tv` now.
[ -n "${HOST:-}" ] || [ "${1:-}" = selftest ] || {
  echo "cannot resolve the flavour above from $REPO/Makefile" >&2; exit 2;
}
# The app mkfifos this at boot inside its own runtime root; the NAME is unchanged across flavours,
# only the directory moved.
REMOTE_FIFO="$RUNDIR/plxnative-remote"

STREAM_PID_FILE="$REPO/.tv-stream.pid"
# --remote's three extra processes. Each gets a pid file so `down` revokes the published URL and
# the password even if this shell is long gone — a tunnel that outlives the session it was opened
# for is the one failure mode here that is silent AND outward-facing.
DPAD_PID_FILE="$REPO/.tv-dpad.pid"
TUNNEL_PID_FILE="$REPO/.tv-tunnel.pid"
# --mock-plextv's reverse ssh tunnel (device 127.0.0.1:PORT -> this host's mock). Not outward-facing,
# but it is a long-lived process holding a port on the set, so `down` closes it like the rest.
PLEXTV_PID_FILE="$REPO/.tv-plextv.pid"
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
  for f in "$PLEXTV_PID_FILE" "$TUNNEL_PID_FILE" "$DPAD_PID_FILE" "$STREAM_PID_FILE"; do
    [ -f "$f" ] && { kill "$(cat "$f")" 2>/dev/null; rm -f "$f"; }
  done
  pkill -f 'stream-screen.py --port' 2>/dev/null
  pkill -f 'remote-dpad.py --port'   2>/dev/null
  pkill -f 'cloudflared tunnel --url' 2>/dev/null
  rm -f "$REMOTE_URL_FILE" "$DPAD_PASS_FILE"
  return 0
}

# HOST came back from the batched query above, as `print-tv`. This used to be a local `tv_host()`
# that read `$TV`, then `.tv-host`, then SCRAPED `^TV *=` out of the Makefile with sed — and that
# last branch was already dead: the Makefile ships `TV ?= $(strip $(shell tools/tv-config.sh host …))`, which
# the pattern cannot match. Asking make is also the only correct way; `make -pn` prints a recursive
# variable's DEFINITION, so HOST became the literal `$(strip $(shell cat .tv-host ...))` text, every
# ssh failed with "hostname contains invalid characters", and `up` reported "TV unreachable" for a
# television that was awake and answering. `tools/crash-report.sh` was moved onto `print-tv`, and the
# wake-tv skill onto `tools/tv-config.sh` (the resolver `print-tv` itself shells out to); this was the
# last copy.
# Every ssh goes through tools/tv-ssh: the key first, `sshpass` only if the set refuses it, a fast
# failure if it is unreachable, and neither the address nor the password on any line it prints.
tv()  { PLX_TV_ADDR="$HOST" "$REPO/tools/tv-ssh" ssh tv "$@"; }
tvq() { PLX_TV_ADDR="$HOST" "$REPO/tools/tv-ssh" ssh tv "$@" 2>/dev/null; }

# `pidof plxnative` matched BOTH installs the moment a second flavour landed: the binaries are
# both named `plxnative`, and it hands back two pids in an order busybox does not promise. `fuser`
# on the resolved install's own binary is INODE-scoped, so it answers for exactly the app this
# invocation is driving. Keep only the digits — busybox prints bare pids, other fusers prefix the
# path (which has none), so this normalises both to a plain space-separated list.
app_pids() {
  tvq "fuser $APPDIR/plxnative" | tr -cs '0-9' ' ' | sed 's/^ *//; s/ *$//'
}

# ------------------------------------------------------------ the TV lock ----
# One television, no OS-level mutex: two jobs on it do not fail cleanly, they produce plausible
# WRONG data (an fps number measured while somebody else's binary was deployed underneath, a
# capture of a screen the other job navigated away from). Every subcommand here that DRIVES the
# set goes through the lock; the three read-only ones (log, status, fg) only name the holder. tools/tv-lock.sh is the
# mechanism and .agents/skills/tv-lock/SKILL.md is the workflow.
LOCKTOOL="$REPO/tools/tv-lock.sh"
require_lock() {  # $1 = what this session is for, for the holder read-out
  [ -x "$LOCKTOOL" ] || return 0
  TV="$HOST" "$LOCKTOOL" require --quiet --why "$1" || exit 1
}
advise_lock() {   # read-only: never takes the lock, never refuses — only names the holder
  [ -x "$LOCKTOOL" ] || return 0
  TV="$HOST" "$LOCKTOOL" require --advisory --quiet --why "$1" || true
}

ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$*"; }
info() { printf '  · %s\n' "$*"; }

# ---------------------------------------------------------------- wake -------
ensure_awake() {
  if tvq true; then ok "TV reachable"; return 0; fi
  info "TV asleep — waking"
  "$REPO/.agents/skills/wake-tv/wake-tv.sh" >/dev/null 2>&1
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
is_md5_hash() {
  [ "${#1}" -eq 32 ] || return 1
  case "$1" in
    *[!0-9a-fA-F]*) return 1 ;;
  esac
}

local_binary_hash() {
  local raw hash
  if raw=$(md5 -q "$REPO/pkg/plxnative" 2>/dev/null); then
    :
  else
    raw=$(md5sum "$REPO/pkg/plxnative" 2>/dev/null) || return 1
  fi
  hash=${raw%%[[:space:]]*}
  [ -z "$hash" ] && { printf '\n'; return 0; }
  is_md5_hash "$hash" || return 1
  printf '%s\n' "$hash"
}

remote_binary_hash() {
  local raw hash
  raw=$(tvq "md5sum $APPDIR/plxnative") || return 1
  hash=${raw%%[[:space:]]*}
  [ -z "$hash" ] && { printf '\n'; return 0; }
  is_md5_hash "$hash" || return 1
  printf '%s\n' "$hash"
}

ensure_binary() {
  [ -f "$REPO/pkg/plxnative" ] || { bad "no pkg/plxnative — run make"; return 1; }
  local l t presence
  if ! l=$(local_binary_hash); then
    bad "could not read local binary hash"
    return 1
  fi
  [ -n "$l" ] || { bad "local binary hash is empty"; return 1; }
  # A registered install can have lost its executable. Only a successful, exact response
  # from a searchable/readable app directory establishes absence; transport failures and
  # dangling symlinks must not authorize a repair deploy.
  if ! presence=$(tvq "cd '$APPDIR' && [ -r . ] && [ -x . ] || exit 1
if [ -e plxnative ] || [ -L plxnative ]; then printf 'present\\n'; else printf 'missing\\n'; fi"); then
    bad "could not determine deployed binary presence"
    return 1
  fi
  case "$presence" in
    present)
      if ! t=$(remote_binary_hash); then
        bad "could not read deployed binary hash"
        return 1
      fi
      [ -n "$t" ] || { bad "deployed binary hash is empty"; return 1; }
      ;;
    missing) t=""; info "deployed binary is missing — repair required" ;;
    *) bad "invalid deployed binary presence response"; return 1 ;;
  esac
  # This compares BYTES, and `pkg/plxnative` is a path that every flavour and both configurations
  # write — so a match says "these are the bytes on my disk right now", never "this is the install
  # I asked for". That second question is settled by assert_install, on the app's own boot line.
  if [ "$l" = "$t" ]; then ok "deployed binary matches local build"; return 0; fi
  info "binary differs or is missing — deploying to $APPID"
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
  if ! l=$(local_binary_hash); then
    bad "could not re-read local binary hash after deploy"
    return 1
  fi
  [ -n "$l" ] || { bad "local binary hash is empty after deploy"; return 1; }
  if ! t=$(remote_binary_hash); then
    bad "could not re-read deployed binary hash"
    return 1
  fi
  [ -n "$t" ] || { bad "deployed binary hash is empty after deploy"; return 1; }
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

# guest identity: reuse tests/run.py's OWN resolution instead of re-deriving the plex.tv call.
# `--print-test-token` is that path exposed standalone (tests/manifest.local.json's `test_user`
# -> fetch_managed_user_token, the same owner-token + shared_servers lookup run.py already makes
# for the identical reason -- keeping test playback off the real account's watch history).
#
# On success sets GUEST_TOKEN and returns 0. On failure sets GUEST_ERROR to run.py's own reason
# (no manifest.local.json, no test_user block, the managed user has no shared_servers entry, …)
# and returns 1 -- the caller MUST refuse rather than fall back to push_token, which is exactly
# the 2026-09-10 defect this replaced: `--guest` printed a warning and booted as the owner anyway,
# and a real playback wrote progress into the household account (clearing it needed
# /:/unscrobble, which also reset that item's viewCount — real data loss, not a test artifact).
GUEST_TOKEN=""
GUEST_ERROR=""
resolve_guest_token() {
  GUEST_TOKEN=""; GUEST_ERROR=""
  local errfile tok rc
  errfile=$(mktemp 2>/dev/null) || errfile=/dev/null
  if [ "${mock:-0}" = 1 ]; then
    tok=$(python3 "$REPO/tools/mock-guest.py" "$REPO/src/config.local.h" 2>"$errfile")
  else
  tok=$(cd "$REPO/tests" && python3 run.py --print-test-token 2>"$errfile")
  fi
  rc=$?
  if [ $rc -ne 0 ] || [ -z "$tok" ]; then
    GUEST_ERROR=$(cat "$errfile" 2>/dev/null)
    [ -n "$GUEST_ERROR" ] || GUEST_ERROR="tests/run.py --print-test-token exited $rc with no output"
    [ "$errfile" = /dev/null ] || rm -f "$errfile"
    return 1
  fi
  [ "$errfile" = /dev/null ] || rm -f "$errfile"
  GUEST_TOKEN="$tok"
}

# Decide the Plex identity this boot will use, and do it BEFORE a single trigger is armed or the
# television is touched — so --dry-run can print exactly the decision a real `up` would make, and
# so an unresolvable --guest fails before the TV lock is even taken instead of after. Reads the
# CALLER's (cmd_up's, or cmd_selftest's) locals guest/owner/no_token/server_set/server_slot by
# Bash's dynamic scope — the same convention configure_direct_screen above already uses — and sets
# the caller's identity_desc/push_guest/push_owner the same way.
#
# Returns 1 ONLY for an unresolvable --guest, and the caller MUST treat that as fatal: falling
# through to the owner from here is the exact 2026-09-10 defect this function replaced (see
# resolve_guest_token's doc above).
resolve_identity() {
  identity_desc=""; push_guest=0; push_owner=0
  if [ "$no_token" = 1 ]; then
    if [ "$server_set" = 1 ]; then
      identity_desc="stored session — restoring the signed-in multi-server roster for slot $server_slot"
    else
      identity_desc="stored session / picker — boots as a real user would (picker, or QR if $APPID has no session)"
    fi
    return 0
  fi
  if [ "$guest" = 1 ]; then
    if resolve_guest_token; then
      push_guest=1
      if [ "${mock:-0}" = 1 ]; then
        identity_desc="guest — synthetic mock PMS (no Plex account or account token)"
      else
      identity_desc="guest — the manifest's managed test user (token via tests/run.py, value not printed)"
      fi
      return 0
    fi
    bad "cannot resolve a guest identity: $GUEST_ERROR"
    bad "refusing to boot — falling through to the owner here is the exact defect this guard replaced"
    info "alternative: tests/run.py --server --filter <case>   (drives the harness AS the managed test user)"
    info "alternative: tv-session.sh up --owner                (boot as the household account, explicitly)"
    return 1
  fi
  identity_desc="owner (household account) — the config.local.h token"
  push_owner=1
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

# The heartbeat carries TWO words since UI-restructure phase 10: the PAGE as `route=` and the top
# `ModalStack` surface's own `Screen::name` as ` overlay=`. A surface that used to be a route of
# its own — the account sheet, the press-and-hold card menu — therefore prints
# `route=home overlay=account`, never the retired `route=account`.
#
# Reading only the page word is not merely imprecise here, it is unable to fail: every one of
# those surfaces sits over Home, so `route=home` is already true the instant the app boots, and a
# trigger the app REFUSED would be graded as having arrived. So a caller that names an overlay
# gets both halves checked. ` overlay=none` (what the player prints with nothing up) normalises
# to "no overlay" so a caller naming only a page still matches it.
assert_route() {
  local want="$1" want_overlay="${2:-}" seen seen_route seen_overlay
  seen=$(tvq "grep -oE 'route=[a-z]+( overlay=[a-z]+)?' $EVENTLOG 2>/dev/null | tail -1")
  if [ -z "$seen" ]; then
    bad "no route= heartbeat in $EVENTLOG yet (app booting, or it died)"
    info "if it died: tools/crash-report.sh --flavor $FLAVOR"
    return 1
  fi
  info "reached ${seen}"
  [ -z "$want" ] && return 0
  seen_route="${seen%% *}"
  case "$seen" in
    *" overlay="*) seen_overlay="${seen##* overlay=}" ;;
    *)             seen_overlay="" ;;
  esac
  [ "$seen_overlay" = none ] && seen_overlay=""
  # A caller naming ONLY a page does not care what sits over it — `--screen detail=<rk>` with a
  # panel trigger armed beside it is a boot that arrived, not a boot that missed. The overlay is
  # compared only when it was asked for.
  if [ "$seen_route" = "route=$want" ] \
     && { [ -z "$want_overlay" ] || [ "$seen_overlay" = "$want_overlay" ]; }; then
    ok "on the requested screen"; return 0
  fi
  if [ -n "$want_overlay" ]; then
    bad "wanted route=$want overlay=$want_overlay, got $seen"
  else
    bad "wanted route=$want, got $seen"
  fi
  return 1
}

# Resolve everything implied by `--server` in one testable place. Bash's dynamic local scope is
# intentional here: cmd_up and cmd_selftest each own these names, and this helper fills that
# caller-owned state without globals or Bash-4 namerefs (the host still ships Bash 3.2).
configure_direct_screen() {
  [ "$server_set" = 1 ] || return 0
  [ -n "$server_slot" ] || { bad "--server needs a registry slot"; return 2; }
  case "$server_slot" in
    *[!0-9]*) bad "--server must be a numeric registry slot, got: $server_slot"; return 2 ;;
  esac
  # The app logs ServerId's numeric value, so accept only the one spelling that can round-trip
  # through that identity marker. `01` used to arm slot 1 successfully and then make this command
  # reject its own `server=1 start` proof because it was waiting for `server=01 start`.
  case "$server_slot" in
    0|[1-9]|[1-9][0-9]*) ;;
    *) bad "--server must use canonical decimal (no leading zero), got: $server_slot"; return 2 ;;
  esac
  case "$screen" in
    detail=*|collection=*|player=*) ;;
    *) bad "--server applies only to --screen detail=<rk>, collection=<rk>, or player=<rk>"; return 2 ;;
  esac
  direct_kind="${screen%%=*}"
  direct_rk="${screen#*=}"
  case "$direct_rk" in
    ''|*[!0-9]*) bad "a server-qualified ratingKey must be numeric, got: $direct_rk"; return 2 ;;
  esac

  # A singular dev token installs only the compiled primary PMS. The signed-in session, on the
  # other hand, restores its persisted multi-server roster before the direct trigger fires. Make
  # that the supported path automatically; requiring an undocumented `--no-token` was exactly the
  # reason `--server 1` could otherwise select a slot the same command had just erased.
  no_token=1
  case "$direct_kind" in
    player) direct_marker="plxnative-play: rk=$direct_rk server=$server_slot start" ;;
    detail) direct_marker="plxnative-detail: rk=$direct_rk server=$server_slot start" ;;
  esac
}

# Pure decision core for the bounded direct-screen waiter below. Keeping this separate lets the
# host selftest exhaust the meaningful states without sleeping or touching the television.
direct_poll_state() {
  local marker_seen="$1" route_seen="$2" refused_seen="$3" alive="$4"
  [ "$refused_seen" = 1 ] && { printf '%s\n' refused; return; }
  [ "$alive" = 1 ] || { printf '%s\n' dead; return; }
  if [ "$marker_seen" = 1 ] && [ "$route_seen" = 1 ]; then
    printf '%s\n' ready
  else
    printf '%s\n' wait
  fi
}

# Direct item triggers deliberately perform a synchronous metadata fetch before they route: two
# PMS API calls for a movie and up to five for a show. Each API call has the documented 25-second
# total deadline, so an 8-second launch snapshot cannot distinguish a slow healthy remote PMS from
# a rejected trigger. Poll through the structural 5*25-second upper bound plus launch slack, while
# still returning immediately on an explicit refusal or process death. This is tooling patience,
# not a playback/ABR heuristic.
await_direct_screen() {
  local marker="$1" want_route="$2" max_wait_s=130 waited=0 snapshot=""
  local marker_seen=0 route_seen=0 refused_seen=0 alive=0 state=""
  while [ "$waited" -le "$max_wait_s" ]; do
    snapshot=$(tvq "
      if grep -F '$marker' $EVENTLOG >/dev/null 2>&1; then echo marker=1; fi
      if grep -F 'plxnative-$direct_kind: refused:' $EVENTLOG >/dev/null 2>&1; then echo refused=1; fi
      grep -oE 'route=[a-z]+' $EVENTLOG 2>/dev/null | tail -1
      if fuser $APPDIR/plxnative >/dev/null 2>&1; then echo alive=1; fi
    ")
    marker_seen=0; route_seen=0; refused_seen=0; alive=0
    printf '%s\n' "$snapshot" | grep -qx 'marker=1' && marker_seen=1
    printf '%s\n' "$snapshot" | grep -qx "route=$want_route" && route_seen=1
    printf '%s\n' "$snapshot" | grep -qx 'refused=1' && refused_seen=1
    printf '%s\n' "$snapshot" | grep -qx 'alive=1' && alive=1
    state=$(direct_poll_state "$marker_seen" "$route_seen" "$refused_seen" "$alive")
    case "$state" in
      ready)
        ok "direct identity confirmed ($direct_kind rk=$direct_rk server=$server_slot)"
        ok "on the requested screen (route=$want_route)"
        return 0
        ;;
      refused)
        bad "$direct_kind rk=$direct_rk on server $server_slot was explicitly refused"
        info "see the plxnative-$direct_kind: refused line in $EVENTLOG"
        return 1
        ;;
      dead)
        bad "$APPID died while opening $direct_kind rk=$direct_rk on server $server_slot"
        info "check: tools/crash-report.sh --flavor $FLAVOR"
        return 1
        ;;
    esac
    [ "$waited" -eq "$max_wait_s" ] && break
    sleep 1
    waited=$((waited + 1))
  done
  bad "direct screen did not become ready within ${max_wait_s}s"
  [ "$marker_seen" = 1 ] || info "missing identity: $marker"
  [ "$route_seen" = 1 ] || info "last route: $(printf '%s\n' "$snapshot" | grep '^route=' | tail -1)"
  return 1
}

# `up --mock-plextv=PORT`: prove the host side is tests/mock_pms.py (its synthetic identity, the
# same two answers tools/mock-guest.py demands) and then open the reverse tunnel that makes
# 127.0.0.1:PORT on the DEVICE reach it. The host-side check is what refuses a real server on that
# port; the device-side probe only proves the tunnel carries.
open_mock_plextv() {
  local port="$1" ident user
  ident=$(curl -s -m 5 -H 'Accept: application/json' "http://127.0.0.1:$port/identity" 2>/dev/null)
  user=$(curl -s -m 5 -H 'Accept: application/json' "http://127.0.0.1:$port/api/v2/user" 2>/dev/null)
  case "$ident" in *1.41.0.0000-synthetic*) ;; *) bad "nothing synthetic answers on 127.0.0.1:$port — start tests/mock_pms.py first"; return 1 ;; esac
  case "$user" in *'"demo-user"'*) ;; *) bad "127.0.0.1:$port is not the mock's plex.tv"; return 1 ;; esac
  [ -f "$PLEXTV_PID_FILE" ] && { kill "$(cat "$PLEXTV_PID_FILE")" 2>/dev/null; rm -f "$PLEXTV_PID_FILE"; }
  (nohup env PLX_TV_ADDR="$HOST" "$REPO/tools/tv-ssh" ssh -N -o ExitOnForwardFailure=yes \
      -o ServerAliveInterval=15 -R "$port:127.0.0.1:$port" tv \
      > /dev/null 2>&1 </dev/null & echo $! > "$PLEXTV_PID_FILE") ; disown 2>/dev/null || true
  local i
  for i in 1 2 3 4 5 6 7 8; do
    sleep 1
    if tv "curl -s -m 3 http://127.0.0.1:$port/identity" 2>/dev/null | grep -q 1.41.0.0000-synthetic; then
      ok "mock plex.tv reachable on the device at 127.0.0.1:$port (reverse tunnel, pid file .tv-plextv.pid)"
      return 0
    fi
  done
  bad "the reverse tunnel did not come up (the device cannot reach the mock on 127.0.0.1:$port)"
  return 1
}

# ------------------------------------------------------------ commands -------
cmd_up() {
  local screen=home guest=0 mock=0 owner=0 dry_run=0 stream="" no_token=0 keep=0 remote="" server_slot="" server_set=0
  local direct_kind="" direct_rk="" direct_marker="" extra_arm=() mock_plextv=""
  local identity_desc="" push_guest=0 push_owner=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --screen) screen="$2"; shift 2 ;;
      --screen=*) screen="${1#*=}"; shift ;;
      --server) [ $# -ge 2 ] || { bad "--server needs a registry slot"; exit 2; }
                server_slot="$2"; server_set=1; shift 2 ;;
      --server=*) server_slot="${1#*=}"; server_set=1; shift ;;
      --guest) guest=1; shift ;;
      --mock) mock=1; shift ;;
      --owner) owner=1; shift ;;
      --dry-run) dry_run=1; shift ;;
      --stream) stream=8909; shift ;;
      --stream=*) stream="${1#*=}"; shift ;;
      --remote) remote=8908; shift ;;
      --remote=*) remote="${1#*=}"; shift ;;
      --no-token) no_token=1; shift ;;
      --keep) keep=1; shift ;;
      --mock-plextv=*) mock_plextv="${1#*=}"; shift ;;
      --arm) [ $# -ge 2 ] || { bad "--arm needs NAME[=VALUE]"; exit 2; }
             extra_arm+=("$2"); shift 2 ;;
      --arm=*) extra_arm+=("${1#*=}"); shift ;;
      *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
  done
  if [ "$mock" = 1 ] && [ "$guest" != 1 ]; then
    bad "--mock requires --guest (synthetic identity only)"; exit 2
  fi
  if [ "$mock" = 1 ] && [ "$FLAVOR" != debug ]; then
    bad "--mock requires the debug install"; exit 2
  fi
  if [ "$guest" = 1 ] && [ "$owner" = 1 ]; then
    echo "--guest and --owner are mutually exclusive" >&2; exit 2
  fi
  if [ "$guest" = 1 ] && [ "$no_token" = 1 ]; then
    echo "--guest and --no-token are mutually exclusive: --no-token boots the stored session (or the who's-watching picker), which is a THIRD identity, distinct from both --guest and --owner" >&2
    exit 2
  fi
  if [ -n "$mock_plextv" ]; then
    case "$mock_plextv" in ''|*[!0-9]*) bad "--mock-plextv=PORT needs a port number"; exit 2 ;; esac
    if [ "$guest" = 1 ] || [ "$owner" = 1 ]; then
      bad "--mock-plextv signs in through the mock's plex.tv; it cannot be combined with --guest or --owner"; exit 2
    fi
    if [ "$FLAVOR" != debug ]; then bad "--mock-plextv requires the debug install"; exit 2; fi
    # The mock identity is pinned only by the QR sign-in on a boot with no stored session. Any
    # other screen, --server (stored roster) or --keep (triggers left by an earlier boot) would
    # reach the owner's stored session beside a mock plex.tv, so each is refused, not dropped.
    if [ "$screen" != login ]; then bad "--mock-plextv requires --screen login (the mock sign-in is the only boot it pins)"; exit 2; fi
    if [ "$server_set" = 1 ]; then bad "--mock-plextv cannot be combined with --server (that boots the stored roster)"; exit 2; fi
    if [ "$keep" = 1 ]; then bad "--mock-plextv cannot be combined with --keep (leftover triggers could select another identity)"; exit 2; fi
    no_token=1
    extra_arm+=("plextv=http://127.0.0.1:$mock_plextv")
    extra_arm+=("discover=http://127.0.0.1:$mock_plextv")
  fi
  configure_direct_screen || exit 2
  # --remote is --stream plus a front door: there is nothing to publish without a stream, so it
  # turns one on rather than making the caller remember to pass both.
  if [ -n "$remote" ]; then
    [ -z "$stream" ] && stream=8909
    if ! command -v cloudflared >/dev/null 2>&1; then
      bad "--remote needs cloudflared (brew install cloudflared)"; exit 1
    fi
  fi

  # screen -> triggers. Triggers are read ONCE at boot, so they must all be in place before the
  # launch below; anything live goes through the FIFO afterwards. Computed here, BEFORE the
  # television is touched at all, so identity resolution (next) and --dry-run (after that) can
  # both see the final, fully-resolved no_token — including the two screens that force it
  # themselves (profiles, and --server via configure_direct_screen above).
  local files=() want_route="" want_overlay=""
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
    # The account sheet and the card menu are SURFACES on the shared ModalStack since
    # UI-restructure phase 10, not routes: the heartbeat prints the host page as `route=` and
    # the surface's own `Screen::name` as ` overlay=`. Both halves are named, because the host
    # page alone is Home — true from the first heartbeat of any boot — so naming only it would
    # grade a refused trigger as a success. `tests/manifest.json` re-keyed its `home-acct-glass`
    # and `item-menu` scenes the same way and for the same reason.
    account)   files+=("plxnative-acct="); want_route=home; want_overlay=account ;;
    # the press-and-hold card menu: the trigger snaps into the grid and holds the focused
    # card for us, because a real hold is a live gesture no boot trigger can express
    itemmenu)  files+=("plxnative-itemmenu="); want_route=home; want_overlay=itemmenu ;;
    library)   files+=("plxnative-library="); want_route=library ;;
    library=*) files+=("plxnative-library=${screen#*=}"); want_route=library ;;
    detail=*)  files+=("plxnative-detail=${screen#*=}"); want_route=detail ;;
    collection=*) files+=("plxnative-collection=${screen#*=}"); want_route=collection ;;
    # the person page has no boot trigger of its own — it is REACHED, by opening a movie's
    # detail page, walking focus down to Cast & Crew (a movie's second section) and pressing
    # OK on the first headshot. So the rk here is the MOVIE's, not the person's.
    person=*)  files+=("plxnative-detail=${screen#*=}" "plxnative-detailsec=1" "plxnative-detailok=")
               want_route=person ;;
    player=*)  files+=("plxnative-play=${screen#*=}"); want_route=player ;;
    *) echo "unknown --screen: $screen" >&2; exit 2 ;;
  esac
  [ "$server_set" = 1 ] && files+=("plxnative-server=$server_slot")
  # `--arm`: a scene's own extra triggers, e.g. menu=1 + the oscillator + framedrop. A bare NAME is
  # armed empty (`touch`), matching how the screen triggers above are written.
  local a
  for a in ${extra_arm[@]+"${extra_arm[@]}"}; do
    case "$a" in *=*) files+=("plxnative-$a") ;; *) files+=("plxnative-$a=") ;; esac
  done
  # capture trigger is DIAG-exempt: arming the live view must not suppress the picker.
  # The content is the port to listen on, and it is written EXPLICITLY rather than left empty
  # (which would fall through to the app's own default) so that the number the app binds and the
  # number the streamer dials are the one variable resolved at the top of this file.
  [ -n "$stream" ] && files+=("plxnative-capture=$APPPORT")

  # A caller who typed --guest gets a REFUSAL, not a silently different identity, the moment the
  # chosen screen/server turns out to force the stored-session boot instead (profiles; any
  # --server slot, via configure_direct_screen above). Silently downgrading here would be the same
  # shape of bug this whole change exists to close, just moved one option combination sideways.
  if [ "$guest" = 1 ] && [ "$no_token" = 1 ]; then
    bad "--guest cannot be combined with --screen $screen (server_set=$server_set): that combination requires the stored-session boot (no injected token), which is a different identity from the managed test user"
    exit 1
  fi

  # Identity is decided HERE — before a single trigger is armed, before the TV lock, before the
  # television is even woken — so it can be said out loud before booting into it, and so
  # --dry-run never has to touch the set to answer "what would this do". See the 2026-09-10
  # postmortem on resolve_guest_token above: --guest used to print a warning and then push the
  # OWNER's token anyway, and a real playback wrote progress into the household Plex account.
  resolve_identity || exit 1
  info "identity: $identity_desc"

  if [ "$dry_run" = 1 ]; then
    echo "== DRY RUN ($screen) on $APPID [$FLAVOR] — nothing below touches the television"
    if [ ${#files[@]} -gt 0 ]; then
      for f in "${files[@]}"; do
        local dn="${f%%=*}" dv="${f#*=}"
        if [ "$f" = "$dn=" ] || [ -z "$dv" ]; then echo "  would run: touch $RUNDIR/$dn"
        else echo "  would run: printf '%s' '<redacted>' > $RUNDIR/$dn"; fi
      done
    else
      echo "  (no boot triggers for this screen)"
    fi
    if [ "$push_guest" = 1 ]; then
      echo "  would run: printf '%s' '<guest token, not printed>' | tools/tv-ssh ssh tv 'cat > $RUNDIR/plxnative-token'"
    elif [ "$push_owner" = 1 ]; then
      echo "  would run: printf '%s' '<owner token from src/config.local.h, not printed>' | tools/tv-ssh ssh tv 'cat > $RUNDIR/plxnative-token'"
    else
      echo "  would inject: nothing ($identity_desc)"
    fi
    echo "  would run: make -C $REPO FLAVOR=$FLAVOR kill"
    echo "  would run: tools/tv-ssh ssh tv luna-send -i luna://com.webos.applicationManager/launch '{\"id\":\"$APPID\"}'"
    echo "  would then require: route=${want_route:-<any>}${want_overlay:+ overlay=$want_overlay}"
    echo "== dry run complete — identity: $identity_desc"
    return 0
  fi

  # Before the relaunch, never after — see stop_viewers.
  stop_viewers

  echo "== bringing up the TV session ($screen) on $APPID [$FLAVOR]"
  # FIRST, before anything is closed, cleared or deployed. `up` kills the running app and wipes
  # every trigger under the runtime root — if another lane is mid-run, that is its session, and
  # everything after this point would be measuring a television two jobs are steering.
  require_lock "tv-session up --screen $screen [$FLAVOR]"
  ensure_awake  || exit 1
  ensure_installed || exit 1
  ensure_rundir || exit 1
  ensure_binary || exit 1
  [ "$keep" = 1 ] || clear_triggers

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

  if [ "$push_guest" = 1 ]; then
    printf '%s' "$GUEST_TOKEN" | tv "cat > $RUNDIR/plxnative-token" \
      && ok "guest token injected (value not printed)" \
      || { bad "failed to inject the guest token"; exit 1; }
  elif [ "$push_owner" = 1 ]; then
    push_token || info "continuing without a token — expect the QR sign-in screen"
  fi

  if [ -n "$mock_plextv" ]; then open_mock_plextv "$mock_plextv" || exit 1; fi

  PREV_PIDS=$(app_pids)
  relaunch
  assert_running || { info "check: tools/crash-report.sh --flavor $FLAVOR"; exit 1; }
  if ! assert_install; then
    [ "$server_set" = 1 ] && exit 1
  fi
  if [ "$server_set" = 1 ]; then
    await_direct_screen "$direct_marker" "$want_route" || exit 1
  else
    assert_route "$want_route" "$want_overlay" || true
  fi

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

# `fifo_write` / `fifo_send`: any ONE-WORD token reaches the remote shell as ONE quoted word; a token with
# whitespace in it (any the app cuts a line at: see `fifo_token_ok`) is refused, and nothing is sent for a
# refusal. Round trip through `sh -c`, the way the set's shell reads it. Then the doors in front of it: `key`
# refuses the three by-id tokens and judges every word before sending any, while `click`, `focus` and `press`
# still send through `send_tokens` (no television: `tv` and `require_lock` are stubbed in a subshell).
selftest_fifo() {
  local REMOTE_FIFO probe word stmt got sent_to_tv=0 sent_arg="" _real_tv marker
  probe=$(mktemp "${TMPDIR:-/tmp}/plx-fifo-selftest.XXXXXX") || { bad "no scratch file for the FIFO quoting check"; return 1; }
  REMOTE_FIFO=$probe
  # one-word tokens, injection attempts spelled without a space (a space is refused below), and non-ASCII text
  # that is NOT whitespace: Cyrillic, an accented letter, a curly apostrophe, an em dash and an ellipsis
  # (U+2019, U+2014, U+2026 share their lead bytes with the Unicode spaces `fifo_token_ok` refuses)
  for word in "txt:it's" "txt:';echo\${IFS}pwned>&2;'" "'" "''" 'a"b' 'a$HOME`id`b;&|()<>' 'x\y\n' "txt:a+b" "txt:*" \
              "back" "ck:10,20" "txt:привет" "txt:caf$(printf '\303\251')" "txt:it’s—a…b"; do
    stmt=$(fifo_write "$word") || { bad "fifo_write refused a one-word token: $word"; rm -f "$probe"; return 1; }
    : > "$probe"
    sh -c "$stmt" 2>/dev/null
    got=$(cat "$probe")
    [ "$got" = "$word" ] || { bad "the FIFO got '$got' for the word '$word'"; rm -f "$probe"; return 1; }
  done
  rm -f "$probe"
  # A token is ONE word: the app cuts a line at ANY whitespace, so each of these would be two tokens, the second
  # (a title's press with no subject) past every check in this file. The multi-byte ones are U+0085, U+00A0,
  # U+1680, U+2000, U+200A, U+2028, U+2029, U+202F, U+205F and U+3000.
  local refused=(
    $'back\nok' "back ok" $'back\tok' $'back\rok' $'back\vok' $'back\fok' " back" "back " "txt:a b"
    "back press:home.hero.play" "ok press:home.shelf.2.card.5" $'back\n press:x'
    $'back\xc2\x85press:x' $'back\xc2\xa0press:x' $'back\xe1\x9a\x80press:x' $'back\xe2\x80\x80press:x'
    $'back\xe2\x80\x8apress:x' $'back\xe2\x80\xa8press:x' $'back\xe2\x80\xa9press:x' $'back\xe2\x80\xafpress:x'
    $'back\xe2\x81\x9fpress:x' $'back\xe3\x80\x80press:x'
  )
  for word in "${refused[@]}"; do
    if fifo_write "$word" >/dev/null 2>&1; then bad "fifo_write accepted a token with whitespace in it: $(printf '%s' "$word" | od -An -c | tr -s ' ' | head -c 60)"; return 1; fi
  done
  # fifo_send: a refusal sends nothing, a word is sent as one ssh command carrying the escaped quote.
  _real_tv=$(declare -f tv)
  tv() { sent_to_tv=1; sent_arg=$1; }
  for word in "${refused[@]}"; do
    fifo_send "$word" >/dev/null 2>&1; got=$?
    [ "$got" = 2 ] && [ "$sent_to_tv" = 0 ] || { eval "$_real_tv"; bad "a token with whitespace was sent (status $got)"; return 1; }
  done
  fifo_send "txt:it's" >/dev/null 2>&1; got=$?
  eval "$_real_tv"
  [ "$got" = 0 ] && [ "$sent_to_tv" = 1 ] || { bad "an ordinary word was not sent (status $got)"; return 1; }
  case "$sent_arg" in
    *"'txt:it'\\''s'"*) ;;
    *) bad "the apostrophe was not escaped for the remote shell: $sent_arg"; return 1 ;;
  esac
  ok "tv-session FIFO quoting (any one-word token is sent as one remote word; a token with whitespace is not sent)"

  # ---- `key` is not a second door for the by-id tokens. `key_word_ok` is the predicate (returns 2, so it can be
  # tested here: `cmd_key` exits).
  for word in down ok back "ck:10,20" "txt:a+b" "txt:press:x" "pat:x" "shot" "hang:1000" "focused" "pressed:x" "stated"; do
    key_word_ok "$word" >/dev/null 2>&1 || { bad "key refused the key token: $word"; return 1; }
  done
  for word in "focus:nav.watchlist" "press:home.continue.card.0" "press:home.hero.play" "press:home.shelf.2.card.5@1234" \
              "press:nav.watchlist" "state:abc" "focus:" "press:" "state:"; do
    if key_word_ok "$word" >/dev/null 2>&1; then bad "key accepted the by-id token: $word"; return 1; fi
  done
  got=$(key_word_ok "press:home.continue.card.0" 2>&1 || true)   # (not a pipe into grep: the refusal's status 2 fails a pipefail pipeline)
  case "$got" in
    *"use the press verb"*) ;;
    *) bad "key's refusal does not point to the press verb: $got"; return 1 ;;
  esac
  # ... and every door still sends what it should, and a refusal sends NOTHING (not even the words in front of
  # the bad one).
  selftest_key_doors || return 1
  ok "tv-session key door (focus:/press:/state: only through their verbs; a refusal sends nothing)"
}

# The usage text ends on the header's last option row (`--keep`) and holds every option whole: it began as a line
# count that stopped inside `--remote` once the list outgrew it.
selftest_usage() {
  local out last
  out=$(usage) || { bad "usage: the header's end marker (# SCREEN OFF is a panel state) is gone"; return 1; }
  case "$(printf '%s\n' "$out" | sed -n 1p)" in
    "tv-session.sh "*) ;;
    *) bad "usage does not start at the header's title"; return 1 ;;
  esac
  last=$(printf '%s\n' "$out" | tail -n 1)
  case "$last" in
    "  --keep "*) ;;
    *) bad "usage ends at '$last', not at the header's last option row (--keep; if an option was added after it, move this check)"; return 1 ;;
  esac
  printf '%s\n' "$out" | grep -q 'never a router port forward' || { bad "usage stops inside the --remote entry"; return 1; }
  ok "tv-session usage text (the whole command and option list, ending on --keep)"
}

# Run one door (`cmd_key`, `cmd_press`, ...) in a subshell, the set and its lock stubbed out: each `tv` call is one
# line in `$1`. Prints "<exit status> <tokens sent>". `exit` inside a door only ends the subshell.
selftest_door() {  # $1 = scratch file, then the door and its words
  local marker=$1 rc
  shift
  : > "$marker"
  ( tv() { printf '%s\n' "$1" >> "$marker"; }; require_lock() { :; }; info() { :; }; "$@" ) >/dev/null 2>&1; rc=$?
  printf '%s %s' "$rc" "$(grep -c . "$marker")"
}

selftest_key_doors() {
  local marker rc=0
  marker=$(mktemp "${TMPDIR:-/tmp}/plx-key-selftest.XXXXXX") || { bad "no scratch file for the key check"; return 1; }
  selftest_key_doors_in "$marker" || rc=1
  rm -f "$marker"
  return "$rc"
}

selftest_key_doors_in() {  # $1 = scratch file
  local m=$1
  # a refusal: status 2 and nothing sent
  local -a refused=(
    "cmd_key press:home.continue.card.0" "cmd_key focus:nav.watchlist" "cmd_key state:abc"
    "cmd_key back press:home.continue.card.0" "cmd_key press:home.hero.play back"
    "cmd_press home.hero.play" "cmd_press home.hero.info" "cmd_press home.hero.watchlist"
    "cmd_press itemmenu.play" "cmd_press itemmenu.watchlist_remove" "cmd_press itemmenu.version.0"
    "cmd_press detail.hero.play" "cmd_press detail.hero.restart" "cmd_press detail.hero.trailer"
    "cmd_press detail.hero.mark_watched" "cmd_press detail.hero.mark_unwatched" "cmd_press detail.hero.watchlist"
    "cmd_press home.shelf.2.card.5" "cmd_press home.hero.play --rk" "cmd_press home.hero.play --rk 1+2"
    "cmd_press versions.row.1" "cmd_press player.control.next" "cmd_press player.control.previous"
    "cmd_press player.control.next_episode" "cmd_press player.control.from_beginning" "cmd_press player.control.skip"
    "cmd_press player.control.5" "cmd_press player.section.0" "cmd_press player.failure.try_again"
    "cmd_press player.failure.play_automatically" "cmd_press more.fold.next" "cmd_press more.fold.from_beginning"
    "cmd_press more.rung.auto" "cmd_press menu.audio.0" "cmd_press menu.sub.1" "cmd_press menu.off"
    "cmd_press menu.forced" "cmd_press menu.hit.2"
  )
  local door
  for door in "${refused[@]}"; do
    # shellcheck disable=SC2086  # the words are spelled in the entry, none holds a space
    [ "$(selftest_door "$m" $door)" = "2 0" ] || { bad "$door was not refused with nothing sent ($(selftest_door "$m" $door))"; return 1; }
  done
  # one WORD with whitespace in it (a space, an NBSP) is refused before the `back` ahead of it is sent
  [ "$(selftest_door "$m" cmd_key back "ok press:home.continue.card.0")" = "2 0" ] || { bad "key sent a word with a space in it"; return 1; }
  [ "$(selftest_door "$m" cmd_key back $'ok\xc2\xa0press:x')" = "2 0" ] || { bad "key sent a word with an NBSP in it"; return 1; }
  [ "$(selftest_door "$m" cmd_key ok "back
press:x")" = "2 0" ] || { bad "key sent a word with a newline in it"; return 1; }
  # the doors that stay open: key tokens through `key`/`click`, and the ids through their verbs
  [ "$(selftest_door "$m" cmd_key back ok)" = "0 2" ] || { bad "key did not send two key tokens"; return 1; }
  [ "$(selftest_door "$m" cmd_click 10 20)" = "0 1" ] && grep -q "'ck:10,20'" "$m" || { bad "click did not send ck:10,20"; return 1; }
  [ "$(selftest_door "$m" cmd_focus nav.watchlist)" = "0 1" ] && grep -q "'focus:nav.watchlist'" "$m" || { bad "focus did not send its token"; return 1; }
  [ "$(selftest_door "$m" cmd_press home.hero.play --rk 12)" = "0 1" ] && grep -q "'press:home.hero.play@12'" "$m" || { bad "press --rk did not send press:<id>@<rk>"; return 1; }
  [ "$(selftest_door "$m" cmd_press itemmenu.watchlist_remove --rk 7)" = "0 1" ] && grep -q "'press:itemmenu.watchlist_remove@7'" "$m" || { bad "an item-menu press naming its title was not sent"; return 1; }
  [ "$(selftest_door "$m" cmd_press home.hero.action)" = "0 1" ] && grep -q "'press:home.hero.action'" "$m" || { bad "a bare press of the failure read-out's retry (no title) was not sent"; return 1; }
  [ "$(selftest_door "$m" cmd_press detail.hero.mark_watched --rk 200622)" = "0 1" ] && grep -q "'press:detail.hero.mark_watched@200622'" "$m" || { bad "a Detail hero press naming its title was not sent"; return 1; }
  # a Detail hero control whose target the page fixes (it only navigates or opens a menu) names no title, so a bare press of it goes out
  [ "$(selftest_door "$m" cmd_press detail.hero.go_to_show)" = "0 1" ] && grep -q "'press:detail.hero.go_to_show'" "$m" || { bad "a bare press of the Detail hero's Go to Show (it only navigates) was not sent"; return 1; }
  return 0
}

# Host-only regression for the command contract above. It deliberately performs no TV I/O and is
# run by `make check`; keeping it in this script proves the same Bash-3 code cmd_up calls rather
# than a Python reimplementation of the option semantics.
cmd_selftest() {
  local screen=player=5469 server_set=1 server_slot=1 no_token=0
  local direct_kind="" direct_rk="" direct_marker=""
  configure_direct_screen >/dev/null || { bad "direct-screen selftest setup failed"; return 1; }
  [ "$no_token" = 1 ] || { bad "--server did not select the stored-roster boot"; return 1; }
  [ "$direct_marker" = "plxnative-play: rk=5469 server=1 start" ] || {
    bad "wrong player identity marker: $direct_marker"; return 1;
  }

  screen=detail=42; server_slot=0; no_token=0; direct_kind=""; direct_rk=""; direct_marker=""
  configure_direct_screen >/dev/null || { bad "detail selftest setup failed"; return 1; }
  [ "$direct_marker" = "plxnative-detail: rk=42 server=0 start" ] || {
    bad "wrong detail identity marker: $direct_marker"; return 1;
  }

  screen=home
  if configure_direct_screen >/dev/null 2>&1; then
    bad "--server incorrectly accepted a non-item screen"; return 1
  fi
  screen=player=not-a-rating-key
  if configure_direct_screen >/dev/null 2>&1; then
    bad "--server incorrectly accepted a nonnumeric ratingKey"; return 1
  fi
  screen=player=42; server_slot=01
  if configure_direct_screen >/dev/null 2>&1; then
    bad "--server incorrectly accepted a noncanonical slot"; return 1
  fi

  [ "$(direct_poll_state 1 1 0 1)" = ready ] || {
    bad "direct waiter did not accept marker + route"; return 1;
  }
  [ "$(direct_poll_state 0 1 0 1)" = wait ] || {
    bad "direct waiter accepted a route without item identity"; return 1;
  }
  [ "$(direct_poll_state 1 0 0 1)" = wait ] || {
    bad "direct waiter accepted item identity before its route"; return 1;
  }
  [ "$(direct_poll_state 1 1 1 1)" = refused ] || {
    bad "direct waiter ignored an explicit refusal"; return 1;
  }
  [ "$(direct_poll_state 1 1 0 0)" = dead ] || {
    bad "direct waiter ignored process death"; return 1;
  }
  ok "tv-session direct-screen contract"

  # ---- by accessibility id: the grammar, the card guard and the nonce match. Pure: nothing is sent.
  local aid
  for aid in nav.watchlist home.hero.play home.shelf.2.card.5 watchlist.grid.card.0 under-posters.row.3 \
             library.rail.12 itemmenu.version.0 detail.hero.mark_watched; do
    a11y_id_ok "$aid" || { bad "a11y_id_ok refused the well-formed id $aid"; return 1; }
  done
  for aid in "" home nav. .nav nav..x Nav.watchlist nav.Watchlist nav.watch-list nav.05 nav.-1 "nav.a b" "nav.a;b" \
             "nav.x'y" 'nav.$HOME' 'nav.a[0]' nav.0x1 under-posters.row-3 "nav.watchlist
nav.home"; do
    ! a11y_id_ok "$aid" || { bad "a11y_id_ok accepted a malformed id: $aid"; return 1; }
  done
  [ "$(a11y_token focus nav.watchlist)" = "focus:nav.watchlist" ] || { bad "focus token"; return 1; }
  [ "$(a11y_token press detail.hero.go_to_show)" = "press:detail.hero.go_to_show" ] || { bad "press token"; return 1; }
  [ "$(a11y_token press home.shelf.2.card.5 1234)" = "press:home.shelf.2.card.5@1234" ] || { bad "press-with-subject token"; return 1; }
  [ "$(a11y_token press catalog.primary 5d7768ab)" = "press:catalog.primary@5d7768ab" ] || { bad "press subject on the catalog pill"; return 1; }
  # Every id whose press acts on a title it names is pressed by name (`SUBJECT_BEARING`): a card, a Detail episode
  # (OK on a still PLAYS it), a season tab, a Filmography credit, the catalog pill, a Home hero Play, Info or
  # Watchlist button (the carousel advances by itself, so a bare press acts on whichever title is showing), a Detail
  # hero control that plays or writes (Play, Restart, Trailer, Mark watched, Mark unwatched, Watchlist: each acts on
  # the ratingKey its subject names), an item-menu row (it acts on the title the hold opened), a Play Version row, a
  # player control that starts or restarts a title (Previous, Next, Next Episode, From beginning, Skip, an id spelled
  # by position, an Info card button, a failure retry, a More fold). Without --rk each is refused; with it, each is
  # sent as press:<id>@<rk>.
  for aid in home.shelf.2.card.5 home.continue.card.0 watchlist.grid.card.3 detail.episodes.still.0 \
             detail.episodes.text.0 detail.episodes.still.3 detail.season.1 filmography.row.2 catalog.primary \
             home.hero.play home.hero.info home.hero.watchlist detail.hero.play detail.hero.restart detail.hero.trailer \
             detail.hero.mark_watched detail.hero.mark_unwatched detail.hero.watchlist \
             itemmenu.play itemmenu.watchlist_remove itemmenu.version.0 versions.row.0 versions.row.2 \
             player.control.next player.control.previous player.control.next_episode player.control.from_beginning \
             player.control.skip player.control.5 player.section.0 player.section.1 player.failure.try_again \
             player.failure.play_automatically more.fold.previous more.fold.next more.fold.from_beginning more.rung.auto \
             more.rung.p720 menu.audio.0 menu.sub.1 menu.off menu.forced menu.hit.2; do
    if a11y_token press "$aid" >/dev/null 2>&1; then bad "a press of $aid without --rk was accepted"; return 1; fi
    [ "$(a11y_token press "$aid" 4321)" = "press:$aid@4321" ] || { bad "press of $aid with --rk was not sent as press:<id>@<rk>"; return 1; }
    [ "$(a11y_token focus "$aid")" = "focus:$aid" ] || { bad "focus of $aid was refused"; return 1; }
  done
  # ... and the ones that start or write no title still press with none (nothing is named by `--rk` there): the
  # Detail hero's controls whose target the page fixes (`go_to_show`, `alt`, `version`, `more`: they navigate or open a
  # menu or a panel), `home.hero.action` (the failure read-out's retry), which acts on no title, and the player's controls that open a
  # panel, seek, pause or leave (tests/player_shots.sh presses its panel openers bare) are pressed bare.
  for aid in detail.about.card detail.about.languages detail.hero.go_to_show detail.hero.alt detail.hero.version \
             detail.hero.more under-posters.row.3 filmography.tab.2 nav.watchlist home.hero.action \
             player.control.subtitles player.control.audio player.control.more player.control.play_pause \
             player.control.seek_back player.control.watch player.control.watch_credits player.tab.info \
             player.tab.chapters player.chapter.0 player.scrub player.failure.change_quality player.failure.back \
             more.quality more.speed more.stats more.rate.1500 more.fold.go_to_detail menu.boost menu.loudness menu.timing menu.style \
             menu.search menu.field.size menu.retry menu.lang.0; do
    [ "$(a11y_token press "$aid")" = "press:$aid" ] || { bad "press of $aid needlessly wanted --rk"; return 1; }
  done
  for aid in "1 2" "12'34" '12@3' '12$x' "12;x" ""; do
    if a11y_token press watchlist.grid.card.3 "$aid" >/dev/null 2>&1; then bad "an unsafe --rk was accepted: $aid"; return 1; fi
  done
  if a11y_token focus nav.watchlist 123 >/dev/null 2>&1; then bad "focus accepted a ratingKey"; return 1; fi
  if a11y_token press "nav.Bad" >/dev/null 2>&1; then bad "press accepted a malformed id"; return 1; fi
  a11y_answers abc '{"schema":1,"nonce":"abc","elements":[]}' || { bad "a11y_answers missed its own nonce"; return 1; }
  a11y_answers abc '{ "nonce" : "abc" }' || { bad "a11y_answers is whitespace-sensitive"; return 1; }
  if a11y_answers abc '{"nonce":"abd"}'; then bad "a11y_answers took another request's file"; return 1; fi
  if a11y_answers abc '{"label":"abc"}'; then bad "a11y_answers matched a value that is not the nonce"; return 1; fi
  aid=$(a11y_nonce)
  [ "${#aid}" -eq 16 ] || { bad "a11y_nonce is not 16 characters"; return 1; }
  ok "tv-session by-id contract (grammar, a title's element needs --rk, nonce match)"
  selftest_fifo || return 1
  selftest_usage || return 1

  # ---- identity: `--guest` must resolve the real managed-user token or REFUSE, and must never
  # fall through to the owner's — the 2026-09-10 TV session 5 defect (a warning printed, then the
  # owner's token pushed anyway; a real playback wrote progress into the household Plex account,
  # and clearing it needed /:/unscrobble, which also reset that item's viewCount).
  #
  # Stubs resolve_guest_token so this never shells out to python3/plex.tv. A Bash function
  # definition is GLOBAL, not scoped to this one — cmd_selftest itself runs as a side effect of
  # simply `source`ing this script with "selftest" as $1 (see the dispatch `case` at the bottom of
  # the file), which is exactly what ci/test_tv_session.py's own harness does to reach OTHER
  # functions in this file without touching the TV. Leaving the stub installed would then silently
  # answer every LATER call to resolve_guest_token in that same sourced shell, in that test file's
  # own process — so it is captured and restored around this block rather than left in place.
  local identity_desc="" push_guest=0 push_owner=0
  local guest=1 owner=0 no_token=0 server_set=0 server_slot=""
  local _guest_stub_ok=1
  local _real_resolve_guest_token; _real_resolve_guest_token=$(declare -f resolve_guest_token)
  resolve_guest_token() {
    if [ "$_guest_stub_ok" = 1 ]; then GUEST_TOKEN="stub-token"; GUEST_ERROR=""; return 0
    else GUEST_TOKEN=""; GUEST_ERROR="stubbed failure"; return 1
    fi
  }

  local _identity_rc=0
  resolve_identity || { bad "resolve_identity failed on a resolvable guest"; _identity_rc=1; }
  if [ "$_identity_rc" = 0 ]; then
    { [ "$push_guest" = 1 ] && [ "$push_owner" = 0 ] && [ "$GUEST_TOKEN" = "stub-token" ]; } || {
      bad "a resolvable guest did not resolve cleanly (push_guest=$push_guest push_owner=$push_owner)"
      _identity_rc=1
    }
  fi

  if [ "$_identity_rc" = 0 ]; then
    _guest_stub_ok=0
    if resolve_identity; then
      bad "resolve_identity must FAIL when the guest cannot be resolved"; _identity_rc=1
    elif ! { [ "$push_guest" = 0 ] && [ "$push_owner" = 0 ]; }; then
      bad "an UNRESOLVABLE guest left push_guest=$push_guest push_owner=$push_owner -- this is the exact silent-owner-fallback defect"
      _identity_rc=1
    fi
  fi

  if [ "$_identity_rc" = 0 ]; then
    guest=0; owner=1
    resolve_identity
    [ "$push_owner" = 1 ] || { bad "--owner did not resolve to the owner identity"; _identity_rc=1; }
  fi

  if [ "$_identity_rc" = 0 ]; then
    guest=0; owner=0; no_token=0
    resolve_identity
    [ "$push_owner" = 1 ] || {
      bad "the DEFAULT (neither --guest nor --owner) must resolve to the owner identity"; _identity_rc=1
    }
  fi

  if [ "$_identity_rc" = 0 ]; then
    guest=0; owner=0; no_token=1
    resolve_identity
    { [ "$push_guest" = 0 ] && [ "$push_owner" = 0 ]; } || {
      bad "--no-token must inject no identity at all"; _identity_rc=1
    }
  fi

  if [ "$_identity_rc" = 0 ]; then
    _guest_stub_ok=1
    local mock_dry
    mock_dry=$(cmd_up --guest --mock --dry-run 2>&1) || _identity_rc=1
    case "$mock_dry" in
      *"identity: guest — synthetic mock PMS"*) ;;
      *) bad "mock guest dry run did not select a synthetic identity"; _identity_rc=1 ;;
    esac
    if (cmd_up --mock --dry-run) >/dev/null 2>&1; then
      bad "mock without guest must refuse"; _identity_rc=1
    fi
    if (FLAVOR=stable; cmd_up --guest --mock --dry-run) >/dev/null 2>&1; then
      bad "mock on stable must refuse"; _identity_rc=1
    fi
  fi

  # Restore the REAL resolve_guest_token unconditionally, whichever branch above set _identity_rc.
  eval "$_real_resolve_guest_token"
  [ "$_identity_rc" = 0 ] || return 1
  ok "tv-session identity contract (guest resolves-or-refuses, never falls back to owner)"
}

cmd_status() {
  echo "== session status: $APPID [$FLAVOR]"
  advise_lock "tv-session status"
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

# One token into the remote FIFO. The app drains it each frame; the write is time-boxed so a FIFO
# with no reader (app not running) cannot wedge this shell. `fifo_write` single-quotes the token for the
# remote shell the POSIX way (`'` becomes `'\''`) and refuses a token with whitespace in it, so ANY one-word
# token is safe here: an apostrophe in a `txt:` token included (`txt:` spells a space `+`). `focus`, `press` and
# `state` additionally validate their ids, subjects and nonce against the app's grammar (`a11y_id_ok`,
# `a11y_subject_ok`) before they get here.
#
# A TOKEN IS ONE WORD. The app cuts the FIFO into tokens at ANY whitespace: `rust-modules/src/remote.rs` finds
# the end of the complete run at ASCII whitespace (space, tab, newline, CR, FF) and then `split_whitespace`
# cuts it at every Unicode White_Space (VT, U+0085, U+00A0, U+2000-200A, U+2028/9, U+202F, U+205F, U+3000, ...).
# So `key $'back press:<card>'` would write TWO tokens, the second a title's press with no subject named, past
# every check in this file. Refusing only a newline left that door open; every one of those separators is
# refused now, and the word is judged as BYTES (`LC_ALL=C`), so the multi-byte ones are matched in any locale.
fifo_token_ok() {  # $1 = one token; says why not (stderr) and returns 2 for a word the app would read as several
  local LC_ALL=C
  case "$1" in
    *[$' \t\n\r\v\f']*|*$'\xc2\x85'*|*$'\xc2\xa0'*|*$'\xe1\x9a\x80'*|*$'\xe3\x80\x80'*|\
    *$'\xe2\x80'[$'\x80\x81\x82\x83\x84\x85\x86\x87\x88\x89\x8a\xa8\xa9\xaf']*|*$'\xe2\x81\x9f'*)
      echo "tv-session: a FIFO token is ONE word with no whitespace in it (the app reads a space, tab, newline or any other whitespace as the end of a token, so a second word would be a second token, past every check here)" >&2
      return 2 ;;
  esac
  return 0
}

fifo_write() {  # $1 = one token; prints the one remote statement that writes it to the FIFO, or returns 2
  local sq="'" esc
  fifo_token_ok "$1" || return 2
  esc=${1//$sq/$sq\\$sq$sq}
  printf '%s' "printf '%s\n' '$esc' > $REMOTE_FIFO"
}

# Returns 2 (and sends nothing) for a word `fifo_write` refuses, else 0: the remote `kill` that ends the
# time box exits non-zero whenever the write finished, so its status says nothing about the send.
fifo_send() {  # $1 = one token
  local stmt
  stmt=$(fifo_write "$1") || return 2
  tv "($stmt) & P=\$!; sleep 2; kill \$P 2>/dev/null" 2>/dev/null
  return 0
}

# `key` is for KEYS and the like (`down`, `ok`, `ck:<x>,<y>`, `txt:...`): never for the by-id tokens, which have
# ONE door each, `focus`, `press` and `state`, because those are where an id is checked against the grammar and a
# title's press is made to name the title (`--rk`). Returns 2 (and says why) for a word that starts one.
key_word_ok() {  # $1 = one word given to `key`
  case "$1" in
    focus:*|press:*|state:*)
      echo "tv-session: key does not send '${1%%:*}:' tokens: use the ${1%%:*} verb (focus <id>, press <id> [--rk <ratingKey>], state), which checks the id and makes a title's press name the title" >&2
      return 2 ;;
  esac
  return 0
}

# Every word is judged BEFORE any is sent (and before the lock is asked for): `key back "ok press:x"` must not
# send `back` and then refuse. The sender behind `key`, `click`, `focus` and `press`; exits 2 on a refusal.
send_tokens() {  # $@ = tokens, in order
  local t
  for t in "$@"; do fifo_write "$t" >/dev/null || exit 2; done
  require_lock "tv-session key"
  for t in "$@"; do
    fifo_send "$t" || exit 2
    info "sent $t"
  done
}

cmd_key() {
  [ $# -gt 0 ] || { echo "usage: tv-session.sh key <token>...   (key tokens: down ok back ... ; focus/press/state have their own verbs)" >&2; exit 2; }
  local t
  for t in "$@"; do key_word_ok "$t" || exit 2; done
  send_tokens "$@"
}

cmd_click() {
  [ $# -eq 2 ] || { echo "usage: tv-session.sh click <x> <y>   (authored 1920x1080)" >&2; exit 2; }
  send_tokens "ck:$1,$2"
}

# ----------------------------------------------------- by accessibility id ----
# `focus:<id>`, `press:<id>[@<subject>]` and `state:<nonce>` are FIFO tokens of a `devtriggers` build
# (rust-modules/ui/src/a11y.rs, dispatch/by_id.rs, src/app/a11y_state.rs). The checks below are the
# grammar's own, so a malformed id stops HERE rather than as a refusal line on the set. (Quoting for the
# remote shell is `fifo_write`'s, for every word `key` and `click` send as well, and so is the refusal of a word
# with whitespace in it; `key` itself refuses these three tokens: see `key_word_ok`.)
#
#   id       <ns>.<path>: ns [a-z][a-z0-9_-]*, then one or more segments, each a name [a-z][a-z0-9_]*
#            or a canonical decimal index (0, or digits with no leading zero)
#   subject  a ratingKey (or a catalog hex id): [A-Za-z0-9_-]+, a subset of what the app accepts
A11Y_ID_RE='^[a-z][a-z0-9_-]*(\.([a-z][a-z0-9_]*|0|[1-9][0-9]*))+$'
A11Y_SUBJECT_RE='^[A-Za-z0-9_-]+$'
A11Y_STATE_TRIES=24     # `state` polls the runtime root this many times, a half second apart

a11y_id_ok() {  # $1 = a full accessibility id. LC_ALL=C: [a-z] must not follow a locale's collation.
  case "$1" in *$'\n'*) return 1 ;; esac
  printf '%s\n' "$1" | LC_ALL=C grep -Eq "$A11Y_ID_RE"
}

a11y_subject_ok() {  # $1 = a ratingKey / subject
  case "$1" in *$'\n'*) return 1 ;; esac
  printf '%s\n' "$1" | LC_ALL=C grep -Eq "$A11Y_SUBJECT_RE"
}

# The token for focusing or pressing an id; says why not (stderr) and returns 2 when it must not be sent.
#   a11y_token focus <id>            -> focus:<id>
#   a11y_token press <id> [<rk>]     -> press:<id> or press:<id>@<rk>
# An element whose subject is a title is pressed BY NAME (`--rk`): the real-account rule's own guard (never
# press OK on a title whose ratingKey you have not read), and the app compares it against the subject of the
# element it finds there. `press` refuses without it; `focus` moves nothing that is account-affecting, so it
# takes none. Which ids those are is `a11y_subject_bearing`.
#
# THE LIST IS READ OFF THE CODE (`git grep -n with_subject rust-modules`), one entry per production
# `A11y::with_subject` call SITE, and is the same list as `SUBJECT_BEARING` in tools/a11y.py, which carries the table of
# what each entry is and why (a test holds the two equal; another holds the number of calls per file equal to the
# sites it was derived from, and another the set of files that construct a title-acting message equal to the files it
# covers). THE RULE (stated in full at `SUBJECT_BEARING`): an id is in it when its press ACTS ON a title it names, which
# includes opening that title's own page or menu where the title can change under the press (a card, a credit, the Home
# carousel's Info); only a control whose target the page fixes is pressed bare. A bare `*.row.*` would also catch `under-posters.row.3`, so the namespaced ones are spelled
# out, and the player's controls are spelled by word (`player.control.subtitles` opens a panel and stays bare) with
# the positional spelling `player.control.[0-9]*` guarded whole, and the player's menus' rows by kind (a folded command
# that starts a title and `more.rung.*`, the quality rungs, which re-resolve it; the Tracks menu's track picks and
# subtitle-search results, which write to the title on the server; `more.speed|stats` and the rest stay bare). The rows of a Catalog item menu have no ratingKey,
# so `press` cannot name them and refuses them (a Watchlist title no server holds: the safe side).
a11y_subject_bearing() {  # $1 = a full accessibility id
  case "$1" in
    *.card.*|detail.episodes.*|detail.season.*|filmography.row.*|catalog.primary|home.hero.play|home.hero.info|home.hero.watchlist|detail.hero.play|detail.hero.restart|detail.hero.trailer|detail.hero.mark_watched|detail.hero.mark_unwatched|detail.hero.watchlist|itemmenu.*|versions.row.*|player.control.previous|player.control.next|player.control.next_episode|player.control.from_beginning|player.control.skip|player.control.[0-9]*|player.section.*|player.failure.try_again|player.failure.play_automatically|more.fold.previous|more.fold.next|more.fold.from_beginning|more.rung.*|menu.audio.*|menu.sub.*|menu.off|menu.forced|menu.hit.*) return 0 ;;
  esac
  return 1
}

a11y_token() {
  local act="$1" id="${2:-}" rk="${3:-}"
  a11y_id_ok "$id" || { echo "tv-session: not an accessibility id: '$id' (want <ns>.<segment>..., e.g. nav.watchlist)" >&2; return 2; }
  case "$act" in
    focus)
      [ -z "$rk" ] || { echo "tv-session: focus takes no ratingKey" >&2; return 2; }
      printf 'focus:%s\n' "$id" ;;
    press)
      if a11y_subject_bearing "$id"; then
        [ -n "$rk" ] || {
          echo "tv-session: press $id: pressing this element acts on a title it names (the ids are SUBJECT_BEARING in tools/a11y.py), so name the title with --rk <ratingKey>, the ratingKey of the title the press acts on (read it from \`state\`'s subject for the element, or the log, first)" >&2
          return 2; }
      fi
      if [ -n "$rk" ]; then
        a11y_subject_ok "$rk" || { echo "tv-session: --rk wants a ratingKey of letters, digits, _ and -" >&2; return 2; }
        printf 'press:%s@%s\n' "$id" "$rk"
      else
        printf 'press:%s\n' "$id"
      fi ;;
    *) echo "tv-session: internal: a11y_token focus|press" >&2; return 2 ;;
  esac
}

cmd_focus() {
  [ $# -eq 1 ] || { echo "usage: tv-session.sh focus <id>   (e.g. nav.watchlist)" >&2; exit 2; }
  local tok; tok=$(a11y_token focus "$1") || exit 2
  send_tokens "$tok"
}

cmd_press() {
  local id="" rk="" usage="usage: tv-session.sh press <id> [--rk <ratingKey>]   (a title's element needs --rk)"
  while [ $# -gt 0 ]; do
    case "$1" in
      --rk)   [ $# -ge 2 ] || { echo "$usage" >&2; exit 2; }
              rk="$2"; shift 2 ;;
      --rk=*) rk="${1#*=}"; shift ;;
      -*)     echo "$usage" >&2; exit 2 ;;
      *)      [ -z "$id" ] || { echo "$usage" >&2; exit 2; }
              id="$1"; shift ;;
    esac
  done
  [ -n "$id" ] || { echo "$usage" >&2; exit 2; }
  local tok; tok=$(a11y_token press "$id" "$rk") || exit 2
  send_tokens "$tok"
}

# A nonce in the app's grammar ([A-Za-z0-9_-]{1,32}): 16 hex digits.
a11y_nonce() { od -An -N8 -tx1 /dev/urandom | tr -d ' \n'; }

# Does the state file text `$2` answer the request that carried nonce `$1`? The file is compact JSON,
# so the member reads `"nonce":"<nonce>"`; a stale file names another nonce and does not match.
a11y_answers() { printf '%s' "$2" | grep -Eq "\"nonce\" *: *\"$1\""; }

# Ask the app what is on screen: write `state:<nonce>`, then read `$RUNDIR/plxnative-a11y.json` over ssh
# until it names that nonce (a bounded wait: the app is not running, was built without devtriggers, or is
# stalled). Prints the snapshot, or writes it to FILE (owner-only) and prints one line about it.
cmd_state() {
  local out="" usage="usage: tv-session.sh state [--out FILE]"
  while [ $# -gt 0 ]; do
    case "$1" in
      --out)   [ $# -ge 2 ] && [ -n "$2" ] || { echo "$usage" >&2; exit 2; }
               out="$2"; shift 2 ;;
      --out=*) out="${1#*=}"; [ -n "$out" ] || { echo "$usage" >&2; exit 2; }; shift ;;
      *)       echo "$usage" >&2; exit 2 ;;
    esac
  done
  require_lock "tv-session state"
  local nonce doc i
  nonce=$(a11y_nonce)
  fifo_send "state:$nonce"
  for i in $(seq 1 "$A11Y_STATE_TRIES"); do
    doc=$(tvq "cat $RUNDIR/plxnative-a11y.json")
    if [ -n "$doc" ] && a11y_answers "$nonce" "$doc"; then
      if [ -n "$out" ]; then
        ( umask 077; printf '%s\n' "$doc" > "$out" ) || { bad "cannot write $out"; return 1; }
        ok "state:$nonce answered; $(printf '%s' "$doc" | wc -c | tr -d ' ') bytes written to $out"
      else
        printf '%s\n' "$doc"
      fi
      return 0
    fi
    sleep 0.5
  done
  bad "no snapshot named state:$nonce appeared in $RUNDIR (app not running, a RELEASE=1 build, or its loop is stalled)"
  return 1
}

cmd_shot() {
  # A capture is a MEASUREMENT, not a peek: a shot taken during another lane's session is a
  # picture of a screen that lane navigated to, and nothing about the image says so.
  require_lock "tv-session shot"
  local out="${1:-$REPO/tv-shot.png}"
  "$REPO/tools/capture-screen.sh" "$out" DISPLAY
}

cmd_log() {
  advise_lock "tv-session log"
  local pat="${1:-}"
  if [ -n "$pat" ]; then tvq "grep -E '$pat' $EVENTLOG"
  else tvq "cat $EVENTLOG"; fi
}

cmd_down() {
  echo "== handing the TV back"
  # `down` still needs the lock: it CLOSES the app and clears every trigger, which is the most
  # destructive thing in this file if another lane is mid-run. Handing the television back to a
  # human is `down` followed by `tools/tv-lock.sh release` — the skill spells that pair out.
  require_lock "tv-session down"
  local had_url=0; [ -f "$REMOTE_URL_FILE" ] && had_url=1
  stop_viewers
  ok "streamer stopped"
  # Say it out loud. A published URL is the one thing here that outlives the terminal, so
  # "it is gone now" has to be visible in the handback and not merely true.
  [ "$had_url" = 1 ] && ok "remote URL revoked (tunnel closed, password discarded)"
  ensure_awake || exit 1
  ensure_rundir || exit 1
  clear_triggers                      # strips token/autoplay/capture/everything
  # Close the tested app and leave the television as it is: the owner does not want the debug
  # app reopened behind them after a test. Triggers are already cleared, so the next launch
  # (by anyone) is an ordinary interactive boot. `make kill` closes THIS flavour by app id.
  make -C "$REPO" FLAVOR="$FLAVOR" kill >/dev/null 2>&1
  ok "app closed ($APPID); TV left as is"
  echo "== TV is yours"
}

# Blank or restore the PANEL, leaving the app and playback untouched. See the header: it is only
# the backlight, so the app presents the same either way (`shot` works with it dark; see the header).
cmd_screen() {
  local want="${1:-}"
  case "$want" in
    off) method=turnOffScreen ;;
    on)  method=turnOnScreen ;;
    *) echo "usage: tv-session.sh screen off|on" >&2; exit 2 ;;
  esac
  # Driving the set, so it takes the lock like every other command that does.
  require_lock "tv-session screen $want"
  ensure_awake || exit 1
  # `luna-send` silently no-ops without a controlling TTY -- the house `script -qc` wrapper, same
  # as every other luna call against this television.
  # The reply is pretty-printed multi-line JSON whose exact spacing is not ours to rely on, so
  # match it with grep on the collapsed text rather than a shell glob over embedded newlines --
  # the glob form silently took the failure branch on a reply that was in fact successful.
  local reply flat
  reply=$(tv "script -qc \"luna-send -n 1 -f luna://com.webos.service.tvpower/power/$method '{}'\" /dev/null" 2>/dev/null)
  flat=$(printf '%s' "$reply" | tr -d '\r\n' | tr -s ' ')
  if printf '%s' "$flat" | grep -q '"returnValue": *true'; then
    ok "panel $want ($(printf '%s' "$flat" | sed -n 's/.*"state": *"\([^"]*\)".*/\1/p'))"
  elif [ "$want" = off ] && printf '%s' "$flat" | grep -q '"errorCode": *"-101"'; then
    # -101 "The current state must be 'Active'" means the panel is ALREADY off. Asking for a state
    # the set is already in is a no-op, not a failure -- and reporting it as one makes every script
    # that re-asserts screen-off after a relaunch look broken. Only for `off`: a -101 answering
    # `on` would say "panel already on" about a panel the reply calls dark, and skip a real
    # turn-on, so it falls through to the refusal below.
    ok "panel already $want"
  elif [ "$want" = on ] && printf '%s' "$flat" | grep -q '"errorCode": *"-102"' \
    && printf '%s' "$flat" | grep -q '"state": *"Active"'; then
    # The mirror: -102 "The current state must be 'Screen Off'" with state Active means the panel is
    # ALREADY lit (`tests/run.py --fps --panel on` asks for `screen on` and would refuse to run on a
    # lit set). Any other state -- "Active Standby" -- is a set that is NOT lit, so it stays a refusal.
    ok "panel already on"
  else
    bad "screen $want refused: $flat"; return 1
  fi
}

# Send the app to the BACKGROUND and bring it back: the lifecycle edges (SDL WILL/DID enter
# background `0x103`/`0x104`, WILL/DID enter foreground `0x105`/`0x106`) as the OS delivers them.
# On this webOS 4.5 set the LG launcher/Home/Settings are OVERLAYS and deliver NO edge (see the
# skill), so `background` launches a real full-screen app through the application manager instead
# (default `org.webosbrew.hbchannel`, override with an argument or PLX_BG_APP) and asserts it is now
# the foreground app. `foreground` launches THIS flavour's own id again -- the same call `up` makes,
# but with NO close-first, so the pid must be unchanged (a changed pid means the app died, which is
# a finding, not a relaunch) -- and asserts the app is the foreground app again. No setting is
# changed and nothing is injected; `fg` only reads the foreground app (read-only: advisory lock, no lease,
# and an asleep set is never woken: it reports that nothing was read, which is not "idle").
fg_app() {
  tvq "script -qc \"luna-send -n 1 -f luna://com.webos.applicationManager/getForegroundAppInfo '{}'\" /dev/null" \
    | tr -d '\r\n' | tr -s ' ' | sed -En 's/.*"appId": *"([^"]*)".*/\1/p'
}
launch_app() {  # $1 = app id. -i keeps the subscriber open for the launch to take (see relaunch).
  tv "luna-send -i luna://com.webos.applicationManager/launch '{\"id\":\"$1\"}' >/dev/null 2>&1 & \
      LP=\$!; sleep 5; kill \$LP 2>/dev/null" >/dev/null 2>&1
}
cmd_lifecycle() {
  local what="${1:-}" target pid before after
  case "$what" in
    fg)
      # The one lifecycle word that is a pure read, and tv-lock-guard.py lets it through with no
      # lease for that reason: so it must stay one. `ensure_awake` would send Wake-on-LAN to a
      # sleeping set (a mutation, and the exact thing a person in the next room notices); a set
      # that does not answer is left alone and the line says so: nothing was read and no wake was
      # sent (tv-lock.sh's idle_verdict calls an unread foreground UNKNOWN unless the panel reads Active
      # Standby, and `fg` never reads the panel, so it says "nothing read", not "idle").
      # advise_lock is `tv-lock.sh require --advisory`: it only names a holder (and renews a lease
      # THIS lane already holds), takes nothing and wakes nothing.
      tvq true || { info "TV unreachable or asleep (nothing read, no wake sent)"; return 0; }
      advise_lock "tv-session fg"
      ok "TV reachable"; info "foreground app: $(fg_app)"; return 0 ;;
    background) target="${2:-${PLX_BG_APP:-org.webosbrew.hbchannel}}" ;;
    foreground) target="$APPID" ;;
    *) echo "usage: tv-session.sh background [APPID] | foreground | fg" >&2; exit 2 ;;
  esac
  require_lock "tv-session $what"
  ensure_awake || exit 1
  pid=$(app_pids)
  [ -n "$pid" ] || { bad "$APPID is not running; nothing to $what"; return 1; }
  before=$(fg_app)
  if [ "$what" = background ] && [ "$before" != "$APPID" ]; then
    bad "$APPID is not the foreground app ($before); refusing to launch over someone else's screen"; return 1
  fi
  info "foreground before: $before; launching $target (pid $pid)"
  launch_app "$target"
  after=$(fg_app)
  if [ "$after" != "$target" ]; then
    bad "foreground is '$after', not $target: the launch did not take"; return 1
  fi
  if [ "$(app_pids)" != "$pid" ]; then
    bad "app pid changed ($pid -> $(app_pids)): the app died or restarted instead of $what"; return 1
  fi
  ok "$what: $target is the foreground app; $APPID still pid $pid"
}

# Mute/unmute the TELEVISION's own audio (see SOUND in the header), or just read the flag back.
# `off`/`on` call setMuted and then RE-READ getVolume to confirm rather than trusting setMuted's
# own returnValue -- the same discipline ensure_binary already uses for a deploy; `status` only
# reads getVolume. This never restores sound on its own -- see the header for why.
cmd_sound() {
  local want="${1:-}" muted=""
  case "$want" in
    off) muted=true ;;
    on)  muted=false ;;
    status) ;;
    *) echo "usage: tv-session.sh sound off|on|status" >&2; exit 2 ;;
  esac
  # Driving the set (off/on) takes the lock like `screen`; `status` still reaches the television
  # over ssh, so it goes through the same advisory-only check `status`/`log` use elsewhere in this
  # file rather than either refusing outright or pretending it never touched the set.
  if [ "$want" = status ]; then advise_lock "tv-session sound status"
  else require_lock "tv-session sound $want"; fi
  ensure_awake || exit 1

  if [ "$want" != status ]; then
    # `luna-send` silently no-ops without a controlling TTY -- the house `script -qc` wrapper,
    # same as every other luna call against this television.
    local reply flat
    reply=$(tv "script -qc \"luna-send -n 1 -f luna://com.webos.service.audio/setMuted '{\\\"muted\\\":$muted}'\" /dev/null" 2>/dev/null)
    flat=$(printf '%s' "$reply" | tr -d '\r\n' | tr -s ' ')
    if ! printf '%s' "$flat" | grep -q '"returnValue": *true'; then
      bad "sound $want refused: $flat"; return 1
    fi
  fi

  # The reply is pretty-printed multi-line JSON whose exact spacing is not ours to rely on, so
  # match it with grep/sed on the collapsed text rather than a shell glob over embedded newlines --
  # see cmd_screen's own note on the same trap.
  local vreply vflat seen_muted vol
  vreply=$(tv "script -qc \"luna-send -n 1 -f luna://com.webos.service.audio/getVolume '{}'\" /dev/null" 2>/dev/null)
  vflat=$(printf '%s' "$vreply" | tr -d '\r\n' | tr -s ' ')
  if ! printf '%s' "$vflat" | grep -q '"returnValue": *true'; then
    bad "sound $want: getVolume refused: $vflat"; return 1
  fi
  # `-E` (extended regex) rather than a BRE `\(true\|false\)`: BSD sed (the macOS host this is
  # developed on) does not support `\|` alternation inside a BRE group, so that spelling silently
  # matched nothing at all -- caught by this file's own host test, never on the set.
  seen_muted=$(printf '%s' "$vflat" | sed -En 's/.*"muted": *(true|false).*/\1/p')
  vol=$(printf '%s' "$vflat" | sed -n 's/.*"volume": *\([0-9]*\).*/\1/p')
  case "$want" in
    off)
      [ "$seen_muted" = true ] || {
        bad "setMuted true did not stick -- getVolume reports muted=${seen_muted:-unknown}"; return 1
      }
      ok "sound off (muted, volume ${vol:-?})"
      ;;
    on)
      [ "$seen_muted" = false ] || {
        bad "setMuted false did not stick -- getVolume reports muted=${seen_muted:-unknown}"; return 1
      }
      ok "sound on (unmuted, volume ${vol:-?})"
      ;;
    status)
      info "sound: muted=${seen_muted:-unknown} volume=${vol:-?}"
      ;;
  esac
}

# ------------------------------------------------------------ the WAN cut ----
# "Offline mode" is a household whose LAN is up and whose uplink is down. Nothing on a desk can
# take the router's uplink away for ONE device deterministically, so this does it on the set
# itself, in two halves because the firmware only has one of the two tools (probed 2026-09-05):
#   * v4: a netfilter chain on OUTPUT (iptables 1.6, filter table loaded) REJECTS every packet
#     not bound for the LAN, plus every DNS query anywhere — the router would otherwise still
#     resolve `plex.direct` through its own uplink, which is the half of "offline" that matters;
#   * v6: there is NO ip6tables filter table (`ip6_tables` is not in the kernel's modules), so
#     the cut is an `unreachable 2000::/3` route — more specific than either default route, so
#     no metric contest with connman's or the RA's, and less specific than the on-link /64s, so
#     LAN v6 keeps routing and neighbour discovery is untouched. DNS is v4 here (connman's
#     resolv.conf names the router's v4 address), so the v4 chain already covers it.
# REJECT / unreachable rather than DROP, so a dead destination fails at once instead of burning a
# connect budget: the case graded is "the app reaches the LAN server", not "how long a timeout takes".
#
# FAIL-SAFE BY CONSTRUCTION. `off` writes the restore script ON THE SET and starts a watchdog
# there (`nohup sh -c 'sleep TTL; sh restore'`), so a harness that dies, a Mac that sleeps and an
# ssh that drops all end with the television back online without anybody's help. `on` runs the
# same restore and kills the watchdog. Idempotent both ways. The marker is outside the
# `plxnative-*` prefix, like the lock, so it neither suppresses the picker nor gets swept.
#
# Two things this cannot claim, stated so nobody grades them from it: a REJECT is not what a
# real router does with its uplink down (that is usually silence), and the jail's resolver view
# is the app's business — read `net: curl rc=6` / `nowan` lines in the app's OWN log, never this
# script's status, to say what the app saw.
WAN_MARK=/tmp/plx-wan-off
WAN_RESTORE=/tmp/plx-wan-restore.sh
cmd_wan() {
  local want="${1:-}" ttl="${2:-900}"
  case "$want" in off|on|status) ;; *) echo "usage: tv-session.sh wan off [TTL_SECONDS]|on|status" >&2; exit 2 ;; esac
  case "$ttl" in ''|*[!0-9]*) echo "wan off: TTL must be seconds" >&2; exit 2 ;; esac
  if [ "$want" = status ]; then advise_lock "tv-session wan status"; else require_lock "tv-session wan $want"; fi
  ensure_awake || exit 1
  case "$want" in
    off)
      # The restore script FIRST, so a watchdog can never exist without the thing it runs.
      tv "cat > $WAN_RESTORE" <<'RESTORE'
#!/bin/sh
# written by tools/tv-session.sh wan off — puts the television's uplink back
while iptables -D OUTPUT -j PLXWAN 2>/dev/null; do :; done
iptables -F PLXWAN 2>/dev/null; iptables -X PLXWAN 2>/dev/null
while ip -6 route del unreachable 2000::/3 2>/dev/null; do :; done
[ -f /tmp/plx-wan-off.wd ] && kill "$(cat /tmp/plx-wan-off.wd)" 2>/dev/null
rm -f /tmp/plx-wan-off /tmp/plx-wan-off.wd
RESTORE
      if ! tv "sh -s $ttl" <<'CUT'
set -e
ttl="$1"
sh /tmp/plx-wan-restore.sh >/dev/null 2>&1 || true   # idempotent: start from a clean chain
lan4=$(ip -4 route show dev eth0 scope link | awk '/\// {print $1; exit}')
iptables -N PLXWAN
iptables -A PLXWAN -p udp --dport 53 -j REJECT
iptables -A PLXWAN -p tcp --dport 53 -j REJECT
iptables -A PLXWAN -d 127.0.0.0/8 -j ACCEPT
[ -n "$lan4" ] && iptables -A PLXWAN -d "$lan4" -j ACCEPT
iptables -A PLXWAN -d 224.0.0.0/4 -j ACCEPT
iptables -A PLXWAN -j REJECT --reject-with icmp-net-unreachable
# The watchdog BEFORE the hook: if anything after this line fails, the set still comes back.
date +%s > /tmp/plx-wan-off
nohup sh -c "sleep $ttl; sh /tmp/plx-wan-restore.sh" >/dev/null 2>&1 &
echo $! > /tmp/plx-wan-off.wd
iptables -I OUTPUT -j PLXWAN
ip -6 route add unreachable 2000::/3
# …and prove it took, from the set's own tables, or fail this command.
[ "$(iptables -S OUTPUT | grep -c PLXWAN)" = 1 ] || { echo "v4 chain not hooked" >&2; exit 3; }
ip -6 route show | grep -q '^unreachable 2000::/3' || { echo "v6 route not added" >&2; exit 3; }
echo "lan4=$lan4 ttl=$ttl"
CUT
      then
        bad "WAN cut FAILED on the television — restoring whatever half took"
        tv "sh $WAN_RESTORE" >/dev/null 2>&1 || true
        exit 1
      fi
      ok "WAN cut on the television (LAN + loopback open, DNS refused, public v6 unreachable); auto-restores in ${ttl}s"
      cmd_wan status
      ;;
    on)
      tv "[ -f $WAN_RESTORE ] && sh $WAN_RESTORE; while iptables -D OUTPUT -j PLXWAN 2>/dev/null; do :; done; iptables -F PLXWAN 2>/dev/null; iptables -X PLXWAN 2>/dev/null; while ip -6 route del unreachable 2000::/3 2>/dev/null; do :; done; rm -f $WAN_MARK $WAN_MARK.wd; true"
      ok "WAN restored"
      cmd_wan status
      ;;
    status)
      local chain since pms
      chain=$(tvq "iptables -S OUTPUT 2>/dev/null | grep -c PLXWAN; ip -6 route show 2>/dev/null | grep -c '^unreachable 2000::/3'" | tr '\n' '/')
      since=$(tvq "cat $WAN_MARK 2>/dev/null")
      if [ -n "$since" ]; then
        info "WAN: CUT since epoch $since (v4 chain / v6 unreachable route armed: ${chain%/}); watchdog pid $(tvq "cat $WAN_MARK.wd 2>/dev/null")"
      else
        info "WAN: open (v4 chain / v6 unreachable route armed: ${chain%/})"
      fi
      # Measured from the SET, which is the only place the answer is about: the PMS on the Mac
      # over the LAN, and a public name through the router's resolver. The PMS host comes from
      # the gitignored config, when this checkout has one; a lane without it skips that line.
      pms=${PMS_HOST:-$(sed -n 's/.*PMS_HOST *"\([^"]*\)".*/\1/p' "$REPO/src/config.local.h" 2>/dev/null)}
      [ -n "$pms" ] && info "from the TV: LAN PMS /identity -> $(tvq "wget -q -T 5 -O - http://$pms:32400/identity >/dev/null 2>&1 && echo ok || echo FAIL")"
      info "from the TV: resolve plex.tv -> $(tvq "nslookup plex.tv >/dev/null 2>&1 && echo ok || echo FAIL")"
      ;;
  esac
}

# The text an unknown subcommand prints: the header's command and option list, from line 3 down to the paragraph
# that opens the prose (`# SCREEN OFF is a panel state`), found by THAT LINE and not by a line count. The count it
# replaced (`sed -n '3,60p'`) went stale each time the list grew, and the text stopped in the middle of `--remote`;
# `selftest_usage` holds the end.
usage() {
  local text
  text=$(awk 'NR < 3 { next } /^# SCREEN OFF is a panel state/ { found = 1; exit } { print } END { exit !found }' \
         "${BASH_SOURCE[0]}") || { echo "tv-session: internal: the header's usage text has no end marker (# SCREEN OFF ...)" >&2; return 1; }
  printf '%s\n' "$text" | sed 's/^# \{0,1\}//'
}

case "${1:-}" in
  selftest) cmd_selftest ;;
  wan)    shift; cmd_wan "$@" ;;
  up)     shift; cmd_up "$@" ;;
  screen) shift; cmd_screen "$@" ;;
  sound)  shift; cmd_sound "$@" ;;
  background|foreground|fg) cmd_lifecycle "$@" ;;
  status) shift; cmd_status ;;
  key)    shift; cmd_key "$@" ;;
  click)  shift; cmd_click "$@" ;;
  focus)  shift; cmd_focus "$@" ;;
  press)  shift; cmd_press "$@" ;;
  state)  shift; cmd_state "$@" ;;
  shot)   shift; cmd_shot "$@" ;;
  log)    shift; cmd_log "$@" ;;
  down)   shift; cmd_down ;;
  *) usage; exit 2 ;;
esac
