#!/usr/bin/env bash
#
# tv-lock.sh — the mutex the television never had.
#
#   tv-lock.sh status                       who holds the set, and is anybody on it unlocked
#   tv-lock.sh acquire --why "<what>"       take it (waits with --wait, steals only when expired)
#   tv-lock.sh renew                        extend the lease you already hold
#   tv-lock.sh release                      hand it back
#   tv-lock.sh require                      assert + renew; what every TV-facing tool calls
#   tv-lock.sh with --why "<what>" -- CMD…  acquire, run CMD, release even on Ctrl-C
#   tv-lock.sh break                        steal a live lease (names the holder first)
#   tv-lock.sh selftest                     exercise the whole protocol with NO television
#
# WHY THIS EXISTS. There is one dev set, one app instance on it, and nothing in webOS, ssh or
# this repo enforces one user at a time. Two `tests/run.py` runs, or a run plus a `make deploy`,
# or a capture session plus either, kill each other's app — and the damage is not a clean failure,
# it is PLAUSIBLE WRONG DATA: a bogus timeline_climb, an fps number measured while somebody else's
# binary was being deployed underneath, a capture of a screen the other job navigated away from.
# None of those can be told from a real regression by looking at them. Sequencing by hand is what
# was keeping jobs apart, and "at most one lane at a time" is a rule that is true when written and
# false the moment a second agent starts.
#
# WHERE THE LOCK LIVES: on the TELEVISION, at $REMOTE_LOCK — a directory, because `mkdir` is the
# one create-if-absent primitive that is atomic on every filesystem including this set's busybox
# userland. On the device rather than on a dev Mac because the television is the resource: a
# host-side file cannot see the second worktree, the second machine, or the colleague on the sofa,
# and this project's collisions come from exactly those. It is under /tmp deliberately — a TV
# reboot clears it, which is the one event that also makes every holder's session meaningless.
#
# The name does NOT begin with `plxnative-`, and that is load-bearing rather than cosmetic: for the
# stable install the app's runtime root IS /tmp, and any file there matching that prefix marks the
# boot as automated and suppresses the who's-watching picker (`dev::any_trigger_present`). A lock
# named `plxnative-tv.lock` would silently change which screen the app boots to — for every
# session, including the ones it was taken to protect. It is also outside the `plxnative-*` glob
# that `make run` and `tests/run.py` clear, so a teardown cannot drop somebody's lease.
#
# THE CLOCK IS THE HOST'S. Every timestamp written into the lock comes from the machine taking it,
# never from `date` on the TV: pmlog's wall clock on this set runs about three hours off, so a
# lease minted against it would expire in the past or three hours late. Hosts are NTP-synced to
# each other; the television is not synced to anything that matters here.
#
# See .agents/skills/tv-lock/SKILL.md for the workflow, and CLAUDE.md's testing section for what
# the lock does and does not protect (it cannot see a human watching a film with the remote).
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

REMOTE_LOCK="${PLX_TV_LOCK_PATH:-/tmp/plx-tv.lock}"
STATE_DIR="${PLX_TV_LOCK_STATE:-$HOME/.plxnative/tv-lock}"

# Lease lengths, in minutes. An EXPLICIT acquire is a session and gets the long one; the implicit
# lease `require` takes when nobody holds the set is short on purpose, because nothing will ever
# come back to release it — it exists so a lone `make deploy` still cannot collide, not so a
# forgotten one can hold the television for an hour.
TTL_MIN="${PLX_TV_LOCK_TTL:-45}"
AUTO_TTL_MIN=10

# ---------------------------------------------------------------- identity ---
# The LANE, not the process: a lease belongs to a checkout (this worktree), so every Bash call,
# every `make`, every nested tool in that lane inherits it, and a second worktree on the same Mac
# is a different lane — which is precisely the fleet case that collides today.
LANE="${PLX_TV_LOCK_LANE:-$REPO}"
lane_slug() {
  local h; h=$(printf '%s' "$LANE" | shasum 2>/dev/null | cut -c1-12)
  [ -n "$h" ] || h=$(printf '%s' "$LANE" | cksum | tr -d ' ' | cut -c1-12)
  printf '%s-%s' "$(basename "$LANE" | tr -c 'A-Za-z0-9._-' '_')" "$h"
}
LEASE="$STATE_DIR/$(lane_slug).lease"

now()  { date +%s; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*" >&2; }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$*" >&2; }
info() { printf '  · %s\n' "$*"; }

# Anything that reaches the lock file has to survive being read back by `sed` and re-embedded in a
# shell script, so it is stripped to one harmless line rather than quoted through three layers.
sane() { printf '%s' "${1:-}" | tr '\n\r' '  ' | tr -d "'\"\\\\\`\$" | cut -c1-160; }

fmt_dur() {  # seconds -> "1h 04m" / "7m 12s" / "9s"
  local s="${1:-0}"; [ "$s" -lt 0 ] && s=0
  if   [ "$s" -ge 3600 ]; then printf '%dh %02dm' $((s/3600)) $(((s%3600)/60))
  elif [ "$s" -ge 60 ];   then printf '%dm %02ds' $((s/60)) $((s%60))
  else printf '%ds' "$s"; fi
}

# ------------------------------------------------------------------- the TV ---
# Resolution order ends in a fallback the rest of the repo does not have, and it matters here more
# than anywhere: `.tv-host` is gitignored, so a LINKED WORKTREE has none — and worktrees are where
# the parallel agents live. Without this, the one lane most likely to collide is also the only one
# that cannot ask who holds the set.
resolve_tv() {
  local h="${TV:-${TV_HOST:-}}"
  [ -n "$h" ] || h="$(cat "$REPO/.tv-host" 2>/dev/null)"
  if [ -z "$h" ]; then
    local common; common="$(git -C "$REPO" rev-parse --git-common-dir 2>/dev/null)"
    [ -n "$common" ] && h="$(cat "$common/../.tv-host" 2>/dev/null)"
  fi
  [ -n "$h" ] || h="$(make -s -C "$REPO" print-tv 2>/dev/null | head -1)"
  printf '%s' "$(printf '%s' "$h" | tr -d ' \t\n\r')"
}
HOST="$(resolve_tv)"

SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR
          -o ConnectTimeout=8 -o BatchMode=yes)
# Key auth first (this Mac has one), sshpass second so a machine without the key still works. The
# password is webosbrew's published dev-mode root password — the same on every rooted set, so it
# identifies nobody; the ADDRESS is the part that stays out of the repo.
ssh_tv() {
  ssh "${SSH_OPTS[@]}" "root@$HOST" "$@" 2>/dev/null && return 0
  local rc=$?
  command -v sshpass >/dev/null 2>&1 || return $rc
  sshpass -p alpine ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
      -o LogLevel=ERROR -o ConnectTimeout=8 "root@$HOST" "$@" 2>/dev/null
}

# ------------------------------------------------------- the protocol itself ---
# ONE round trip per operation, and the whole decision is taken ON THE TELEVISION. Reading the
# owner here and writing it back would be a check-then-act with a network in the middle, which is
# not a lock at all — two lanes reading "free" a millisecond apart would both write themselves in.
#
# $1 = acquire | renew | release | status.  Everything else rides in as pre-set shell variables so
# nothing has to be quoted through ssh twice.
remote_op() {
  local mode="$1" ttl_s="$2" force="${3:-0}"
  local exp=$(( $(now) + ttl_s ))
  local owner
  owner="token=$TOKEN
expires=$exp
acquired=${ACQUIRED_AT:-$(now)}
ttl=$ttl_s
user=$(sane "${USER:-unknown}")
host=$(sane "$(hostname -s 2>/dev/null || hostname)")
lane=$(sane "$LANE")
branch=$(sane "$(git -C "$REPO" rev-parse --abbrev-ref HEAD 2>/dev/null)")
label=$(sane "${LABEL:-}")
why=$(sane "${WHY:-}")
pid=$$"

  local script
  script="$(cat <<PLX_HDR
set -u
L='$REMOTE_LOCK'
O="\$L/owner"
NOW=$(now)
TOK='$TOKEN'
MODE='$mode'
FORCE='$force'
OWNER=\$(cat <<'PLX_OWNER'
$owner
PLX_OWNER
)
PLX_HDR
)"
  script="$script
$REMOTE_BODY"

  if [ -n "${PLX_TV_LOCK_LOCAL:-}" ]; then printf '%s\n' "$script" | sh
  else printf '%s\n' "$script" | ssh_tv 'sh -s'; fi
}

# The device half, kept as one string so `selftest` can run the identical text under a local `sh`
# against a temp directory — the protocol is then tested for real, with no television involved.
# POSIX only: this runs under busybox ash on the set.
REMOTE_BODY='
w() { printf "%s\n" "$OWNER" > "$O"; }   # QUOTE the format: bare %s\n loses the backslash to the
                                             # shell and appends a literal "n", which then swallows the
                                             # @@prev delimiter into the last field.
EX=0
if [ "$MODE" = acquire ]; then
  MK=0; mkdir "$L" 2>/dev/null && MK=1
  # A lock directory with no owner file yet is an acquire IN FLIGHT (the two writes cannot be one
  # syscall). Treating that as a corpse would let the second lane steal the first one mid-take, so
  # give it a moment before believing it.
  if [ "$MK" = 0 ] && [ ! -f "$O" ]; then sleep 1; fi
else
  MK=0
  [ -d "$L" ] || MK=2
fi
CT=$(sed -n "s/^token=//p" "$O" 2>/dev/null)
CE=$(sed -n "s/^expires=//p" "$O" 2>/dev/null)
[ -n "$CE" ] || CE=0
case "$MODE" in
  acquire)
    if   [ "$MK" = 1 ];            then ACT=acquired
    elif [ "$CT" = "$TOK" ];       then ACT=renewed
    elif [ "$FORCE" = 1 ];         then ACT=broken
    elif [ "$NOW" -gt "$CE" ];     then ACT=stolen
    else                                ACT=held; EX=1
    fi
    if [ "$ACT" != held ]; then
      [ "$ACT" = acquired ] || cp "$O" "$L/previous" 2>/dev/null
      w
    fi ;;
  renew)
    if   [ "$MK" = 2 ];      then ACT=absent;  EX=1
    elif [ "$CT" = "$TOK" ]; then w; ACT=renewed
    else                          ACT=notmine; EX=1
    fi ;;
  release)
    if   [ "$MK" = 2 ];      then ACT=absent
    elif [ "$CT" = "$TOK" ] || [ "$FORCE" = 1 ]; then rm -rf "$L"; ACT=released
    else                          ACT=notmine; EX=1
    fi ;;
  status)
    if   [ "$MK" = 2 ];        then ACT=free
    elif [ ! -f "$O" ];        then ACT=held
    elif [ "$NOW" -gt "$CE" ]; then ACT=expired
    else                            ACT=held
    fi ;;
esac
echo "act=$ACT"
echo "@@owner"
case "$ACT" in free|absent|released) ;; *) cat "$O" 2>/dev/null ;; esac
echo "@@prev"
case "$ACT" in stolen|broken) cat "$L/previous" 2>/dev/null ;; esac
echo "@@end"
exit $EX
'

# Split one remote reply into $ACT and the two owner blocks.
RESP=""; ACT=""; OWNER_BLK=""; PREV_BLK=""
parse_resp() {
  ACT="$(printf '%s\n' "$RESP" | sed -n 's/^act=//p' | head -1)"
  OWNER_BLK="$(printf '%s\n' "$RESP" | sed -n '/^@@owner$/,/^@@prev$/p' | sed '1d;$d')"
  PREV_BLK="$(printf '%s\n' "$RESP" | sed -n '/^@@prev$/,/^@@end$/p'   | sed '1d;$d')"
}
fld() { printf '%s\n' "$2" | sed -n "s/^$1=//p" | head -1; }

# "gleb@studio [bridge-cse] since 12m, 33m left — why: fps suite" — one line, because it is
# printed by tools that are in the middle of saying something else.
describe() {
  local blk="$1" u h l w lab exp acq n
  u=$(fld user "$blk"); h=$(fld host "$blk"); l=$(fld lane "$blk"); w=$(fld why "$blk")
  lab=$(fld label "$blk"); exp=$(fld expires "$blk"); acq=$(fld acquired "$blk"); n=$(now)
  local who="$u@$h"; [ -n "$lab" ] && who="$who ($lab)"
  local held="" left=""
  [ -n "$acq" ] && held=" held $(fmt_dur $((n - acq)))"
  if [ -n "$exp" ]; then
    if [ "$n" -gt "$exp" ]; then left=", EXPIRED $(fmt_dur $((n - exp))) ago"
    else left=", $(fmt_dur $((exp - n))) left"; fi
  fi
  printf '%s%s%s\n' "$who" "$held" "$left"
  [ -n "$l" ] && printf '    lane: %s\n' "$l"
  [ -n "$w" ] && printf '    why:  %s\n' "$w"
  return 0
}

# ------------------------------------------------------------ the local lease ---
# The token proves the lease is MINE, and it lives in two places: here (so any later shell in this
# lane can prove it) and in the lock on the TV. A hook reads only this file, which is why it costs
# nothing on every Bash call; the television stays the authority, and `require` reconciles them.
lease_write() {
  mkdir -p "$STATE_DIR" 2>/dev/null; chmod 700 "$STATE_DIR" 2>/dev/null
  umask 077
  cat > "$LEASE" <<EOF
TOKEN='$TOKEN'
EXPIRES=$1
ACQUIRED=${ACQUIRED_AT:-$(now)}
VERIFIED=$(now)
TV='$HOST'
LANE='$LANE'
EOF
}
lease_clear() { rm -f "$LEASE"; }
lease_load() {
  TOKEN=""; EXPIRES=0; ACQUIRED=0; VERIFIED=0
  [ -f "$LEASE" ] && . "$LEASE" 2>/dev/null
  FILE_TOKEN="$TOKEN"
  # An inherited token beats the file: inside `with`, or inside anything it spawned, the lease is
  # the parent's and may not have been written for this lane at all.
  [ -n "${PLX_TV_LOCK_TOKEN:-}" ] && TOKEN="$PLX_TV_LOCK_TOKEN"
  [ -n "$TOKEN" ]
}
new_token() {
  TOKEN="$( (head -c 12 /dev/urandom 2>/dev/null | od -An -tx1 | tr -d ' \n') )"
  [ -n "$TOKEN" ] || TOKEN="$$-$(now)"
  ACQUIRED_AT="$(now)"
}

need_host() {
  [ -n "$HOST" ] && return 0
  bad "no TV configured — put its IP in .tv-host, or pass TV=<ip>"
  return 1
}

# `acquire` is the entry point to a device session, so it wakes the set rather than reporting it
# unreachable: the TV drops to standby after a few idle minutes and every automation session starts
# there. Asleep, every assertion downstream reads as a total regression.
ensure_awake() {
  ssh_tv true && return 0
  info "TV not answering — waking (Wake-on-LAN)"
  TV_HOST="$HOST" "$REPO/.agents/skills/wake-tv/wake-tv.sh" >/dev/null 2>&1
  ssh_tv true
}

# ------------------------------------------------------------------ commands ---
WHY=""; LABEL=""; WAIT=0; FORCE=0; QUIET=0; ADVISORY=0; TTL_OVERRIDE=""
parse_opts() {
  ARGS_REST=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --why)   WHY="${2:-}"; shift; [ $# -gt 0 ] && shift ;;
      --why=*) WHY="${1#*=}"; shift ;;
      --as)    LABEL="${2:-}"; shift; [ $# -gt 0 ] && shift ;;
      --as=*)  LABEL="${1#*=}"; shift ;;
      --wait)  WAIT="${2:-0}"; shift; [ $# -gt 0 ] && shift ;;
      --wait=*) WAIT="${1#*=}"; shift ;;
      --ttl)   TTL_OVERRIDE="${2:-}"; shift; [ $# -gt 0 ] && shift ;;
      --ttl=*) TTL_OVERRIDE="${1#*=}"; shift ;;
      --force|--yes) FORCE=1; shift ;;
      --quiet) QUIET=1; shift ;;
      --advisory) ADVISORY=1; shift ;;
      --)      shift; ARGS_REST=("$@"); return 0 ;;
      *)       ARGS_REST+=("$1"); shift ;;
    esac
  done
}

do_acquire() {  # $1 = ttl minutes, $2 = force ; assumes TOKEN set
  local ttl_s=$(( $1 * 60 ))
  RESP="$(remote_op acquire "$ttl_s" "$2")"; parse_resp
  [ -n "$ACT" ] || { bad "no answer from $HOST — the TV is unreachable"; return 2; }
  # VERIFY, always. Stealing an expired lease is a write, not a compare-and-swap, so two lanes
  # deciding to steal in the same second can both write — and the loser has to find out here
  # rather than in an fps number three minutes from now.
  local got; got=$(fld token "$OWNER_BLK")
  case "$ACT" in
    acquired|renewed|stolen|broken)
      if [ "$got" != "$TOKEN" ]; then
        bad "lost a race for the lock — it is now held by:"; describe "$OWNER_BLK" >&2; return 1
      fi
      lease_write "$(fld expires "$OWNER_BLK")"
      return 0 ;;
    held) return 1 ;;
    *)    bad "unexpected lock reply: $ACT"; return 2 ;;
  esac
}

cmd_acquire() {
  parse_opts "$@"
  need_host || exit 2
  local ttl="${TTL_OVERRIDE:-$TTL_MIN}"
  ensure_awake || { bad "TV unreachable — cannot take the lock (see the wake-tv skill)"; exit 2; }

  # Already mine? Renew in place. Two acquires in one lane is the normal shape when a session
  # spans several Bash calls, and it must not be an error, or every wrapper would need to know
  # whether the caller had already taken it.
  if lease_load; then
    RESP="$(remote_op status 0 0)"; parse_resp
    if [ "$(fld token "$OWNER_BLK")" = "$TOKEN" ]; then
      ACQUIRED_AT="$(fld acquired "$OWNER_BLK")"
      do_acquire "$ttl" 0 && { ok "lock renewed (already yours) — $(fmt_dur $((ttl*60))) from now"; exit 0; }
    fi
  fi

  new_token
  local deadline=$(( $(now) + WAIT ))
  while :; do
    if do_acquire "$ttl" "$FORCE"; then
      case "$ACT" in
        stolen) warn "took an EXPIRED lease from:"; describe "$PREV_BLK" >&2 ;;
        broken) warn "BROKE a live lease held by:"; describe "$PREV_BLK" >&2 ;;
      esac
      ok "TV lock held by this lane for $(fmt_dur $((ttl*60)))${WHY:+ — $WHY}"
      info "release it: tools/tv-lock.sh release"
      preflight_note
      exit 0
    fi
    [ "$ACT" = held ] || exit 2
    if [ "$(now)" -ge "$deadline" ]; then
      bad "the TV is held by another lane:"
      describe "$OWNER_BLK" >&2
      echo "" >&2
      echo "  Do NOT work around this. Either:" >&2
      echo "    - wait:      tools/tv-lock.sh acquire --wait 540 --why '…'" >&2
      echo "    - work host-side meanwhile: make check, or the simulator (make sim / ui-sim skill)" >&2
      echo "    - if that lease is a corpse: tools/tv-lock.sh break   (it names the holder first)" >&2
      exit 1
    fi
    info "held by $(fld user "$OWNER_BLK")@$(fld host "$OWNER_BLK") — waiting ($(fmt_dur $((deadline - $(now)))) left)"
    sleep 5
  done
}

cmd_release() {
  parse_opts "$@"
  need_host || exit 2
  if ! lease_load; then info "no lease in this lane — nothing to release"; exit 0; fi
  RESP="$(remote_op release 0 "$FORCE")"; parse_resp
  case "$ACT" in
    released) lease_clear; ok "TV released" ;;
    absent)   lease_clear; info "lock was already gone (TV rebooted, or someone broke it)" ;;
    notmine)  lease_clear
              warn "your lease was gone — the TV is now held by:"; describe "$OWNER_BLK" >&2 ;;
    "")       bad "no answer from $HOST; local lease dropped anyway"; lease_clear; exit 2 ;;
  esac
  exit 0
}

cmd_renew() {
  parse_opts "$@"
  need_host || exit 2
  lease_load || { bad "no lease in this lane to renew"; exit 1; }
  local ttl="${TTL_OVERRIDE:-$TTL_MIN}"
  RESP="$(remote_op renew $(( ttl * 60 )) 0)"; parse_resp
  case "$ACT" in
    renewed) lease_write "$(fld expires "$OWNER_BLK")"; ok "lease renewed — $(fmt_dur $((ttl*60))) from now" ;;
    *) bad "cannot renew (act=$ACT) — the lease is no longer yours"; exit 1 ;;
  esac
}

# The pre-flight the CLAUDE.md block used to spell by hand. It answers the OTHER half of the
# question the lock cannot: a lease says no other LOCKED job is on the set, and this says whether
# anybody is on it anyway — an older job, a colleague, a human with the remote.
preflight_note() {
  local sd dd out
  sd="$(make -s -C "$REPO" FLAVOR=stable print-appdir 2>/dev/null)"
  dd="$(make -s -C "$REPO" FLAVOR=debug  print-appdir 2>/dev/null)"
  [ -n "$sd" ] && [ -n "$dd" ] || return 0
  # ONE round trip for both installs and the ssh count. `fuser` on each install's own binary,
  # never `pidof plxnative`: both binaries carry that name, so a name-scoped test matches BOTH and
  # answers in an order busybox does not promise.
  out="$(ssh_tv "for d in '$sd' '$dd'; do printf '%s ' \"\$(fuser \$d/plxnative 2>/dev/null || echo NONE)\"; done; echo; netstat -an 2>/dev/null | grep -c 'ESTABLISHED.*:22 \|:22 .*ESTABLISHED'")"
  local apps ssh_n
  apps="$(printf '%s\n' "$out" | head -1)"
  ssh_n="$(printf '%s\n' "$out" | sed -n '2p')"
  case "$apps" in
    *[0-9]*) info "note: an app is already running on the set (stable/debug: $apps)" ;;
  esac
  # The count includes THIS command's own connection, so 2+ means somebody else is on the wire —
  # possibly from a machine whose processes you cannot see with `ps`.
  if [ -n "$ssh_n" ] && [ "$ssh_n" -gt 1 ] 2>/dev/null; then
    warn "$ssh_n ssh sessions on the TV (one is mine) — somebody may be driving it unlocked"
  fi
}

cmd_status() {
  parse_opts "$@"
  need_host || exit 2
  echo "== TV lock: $HOST"
  if ! ssh_tv true; then bad "TV unreachable (asleep? see the wake-tv skill)"; exit 2; fi
  lease_load || true
  RESP="$(remote_op status 0 0)"; parse_resp
  case "$ACT" in
    free)    ok "FREE — nobody holds the television" ;;
    expired) warn "EXPIRED lease still on the set (next acquire takes it):"; describe "$OWNER_BLK" ;;
    held)
      if [ -n "${TOKEN:-}" ] && [ "$(fld token "$OWNER_BLK")" = "$TOKEN" ]; then
        ok "HELD BY THIS LANE:"; describe "$OWNER_BLK"
      else
        bad "HELD by another lane:"; describe "$OWNER_BLK"
      fi ;;
    *) bad "no answer from the lock (act=${ACT:-none})" ;;
  esac
  preflight_note
  [ "$ACT" = free ] || [ "$ACT" = expired ] || return 0
}

cmd_break() {
  parse_opts "$@"
  need_host || exit 2
  RESP="$(remote_op status 0 0)"; parse_resp
  if [ "$ACT" = free ]; then info "nothing to break — the lock is free"; exit 0; fi
  echo "breaking a lease held by:" >&2; describe "$OWNER_BLK" >&2
  if [ "$FORCE" != 1 ] && [ "$ACT" = held ]; then
    local exp; exp=$(fld expires "$OWNER_BLK")
    if [ -n "$exp" ] && [ "$(now)" -lt "$exp" ]; then
      bad "that lease is LIVE ($(fmt_dur $((exp - $(now)))) left). If you are sure it is a corpse:"
      echo "    tools/tv-lock.sh break --yes" >&2
      echo "  A live lease usually means a real job is mid-run; breaking it corrupts BOTH." >&2
      exit 1
    fi
  fi
  new_token; WHY="${WHY:-broke a stale lease}"
  do_acquire "${TTL_OVERRIDE:-$TTL_MIN}" 1 || exit 1
  ok "lock broken and taken by this lane"
}

# What every TV-facing tool calls. Three jobs: refuse when somebody else holds the set, keep a
# live session's lease from ageing out under it, and — when nobody holds it at all — take a SHORT
# implicit lease rather than making a lone `make deploy` fail. That last branch is why a one-off
# command still cannot collide with a fleet job, without anybody having typed anything.
cmd_require() {
  parse_opts "$@"
  if [ "${PLX_TV_LOCK_BYPASS:-0}" = 1 ]; then
    warn "PLX_TV_LOCK_BYPASS=1 — TV lock NOT checked (you are on your own for collisions)"; exit 0
  fi
  [ -n "$HOST" ] || exit 0            # no TV configured: the caller will fail on its own terms

  # THE FAST PATH, and the only reason this is affordable as a prerequisite of every TV-facing
  # make recipe: a lease this lane verified against the television less than a minute ago, with
  # minutes still to run, is re-checked from the local file alone — no ssh, no round trip. What it
  # gives up is narrow and deliberate: someone who BREAKS a live lease (`break --yes`, never
  # automatic) can go unnoticed for up to a minute. Expiry cannot hide here, because the local
  # copy carries the same expiry the TV does.
  if lease_load && [ -n "$FILE_TOKEN" ] && [ "$TOKEN" = "$FILE_TOKEN" ] \
     && [ $(( $(now) - ${VERIFIED:-0} )) -lt 60 ] && [ $(( ${EXPIRES:-0} - $(now) )) -gt 120 ]; then
    [ "$QUIET" = 1 ] || ok "TV lock held by this lane"
    exit 0
  fi

  RESP="$(remote_op status 0 0)"; parse_resp
  if [ -z "$ACT" ]; then
    # Unreachable is not a collision — an asleep television has nobody on it either. Let the
    # caller's own command produce the real error instead of a lock complaint about a lock the
    # TV cannot even be asked about.
    [ "$QUIET" = 1 ] || warn "cannot reach $HOST to check the TV lock — continuing"
    exit 0
  fi
  if lease_load && [ "$(fld token "$OWNER_BLK")" = "$TOKEN" ]; then
    # Renew when under half the lease is left, so a long session never expires mid-run and a short
    # command does not pay for an extra round trip it does not need.
    local exp ttl; exp=$(fld expires "$OWNER_BLK"); ttl=$(fld ttl "$OWNER_BLK")
    if [ -n "$exp" ] && [ -n "$ttl" ] && [ $(( exp - $(now) )) -lt $(( ttl / 2 )) ]; then
      remote_op renew "$ttl" 0 >/dev/null && lease_write $(( $(now) + ttl ))
    fi
    [ "$QUIET" = 1 ] || ok "TV lock held by this lane"
    exit 0
  fi
  if [ "$ADVISORY" = 1 ]; then
    # Read-only callers (`tv-session.sh log`, `status`) take nothing and refuse nothing: fetching a
    # log cannot disturb a running session. They still SAY who is on the set, because reading a log
    # that another lane's app is writing is how a session gets graded as your own.
    [ "$ACT" = held ] && { warn "the TV is held by another lane — this is somebody else's session:"; describe "$OWNER_BLK" >&2; }
    exit 0
  fi
  if [ "$ACT" = held ]; then
    bad "REFUSING: the television is held by another lane —"
    describe "$OWNER_BLK" >&2
    echo "  Two jobs on this set do not fail cleanly; they produce plausible WRONG data." >&2
    echo "  Wait for it (tools/tv-lock.sh acquire --wait 540 --why '…'), or work host-side:" >&2
    echo "  make check / make sim (the ui-sim skill runs N simulators at once)." >&2
    exit 1
  fi
  # free or expired: take the short implicit lease.
  new_token; WHY="${WHY:-implicit lease (an unattended TV command)}"; LABEL="${LABEL:-auto}"
  if do_acquire "$AUTO_TTL_MIN" 0; then
    # ALWAYS said out loud, `--quiet` or not: `--quiet` means "do not narrate the lease I already
    # had", and taking a new one is a different event. Silently acquiring would leave an operator
    # wondering for ten minutes why the set is held by a command that has already finished.
    warn "nobody held the TV — took an implicit ${AUTO_TTL_MIN}m lease. For a session, take a real one: tools/tv-lock.sh acquire --why '…'"
    exit 0
  fi
  bad "could not take the TV lock:"; describe "$OWNER_BLK" >&2; exit 1
}

cmd_with() {
  parse_opts "$@"
  set -- ${ARGS_REST[@]+"${ARGS_REST[@]}"}
  [ $# -gt 0 ] || { echo "usage: tv-lock.sh with [--why …] -- <command>…" >&2; exit 2; }
  need_host || exit 2

  # Inheriting rather than nesting. A `with` inside a lane that ALREADY holds the set must not
  # release it on the way out — the outer session would silently lose the television mid-task.
  local inherited=0
  if [ "${PLX_TV_LOCK_INHERITED:-0}" = 1 ]; then inherited=1
  elif lease_load; then
    RESP="$(remote_op status 0 0)"; parse_resp
    [ "$(fld token "$OWNER_BLK")" = "$TOKEN" ] && inherited=1
  fi

  if [ "$inherited" = 0 ]; then
    ( exec "$0" acquire ${WHY:+--why "$WHY"} ${LABEL:+--as "$LABEL"} \
        ${TTL_OVERRIDE:+--ttl "$TTL_OVERRIDE"} --wait "$WAIT" ) || exit 1
    lease_load || exit 1
  fi

  # Keep the lease warm for as long as the child runs. Without this a legitimately long job — the
  # 21-case suite, a soak — expires under itself and the next lane steals the television mid-run.
  local renewer=0
  if [ "$inherited" = 0 ]; then
    ( while sleep 240; do "$0" renew >/dev/null 2>&1 || exit 0; done ) & renewer=$!
  fi
  trap 'rc=$?; [ '"$renewer"' -gt 0 ] && kill '"$renewer"' 2>/dev/null; [ '"$inherited"' = 0 ] && "$0" release >/dev/null; exit $rc' EXIT INT TERM HUP

  PLX_TV_LOCK_TOKEN="$TOKEN" PLX_TV_LOCK_INHERITED=1 "$@"
}

cmd_selftest() {
  # The whole protocol, against a temp directory under a local `sh` — same REMOTE_BODY text the
  # television runs. It cannot prove anything about ssh or busybox, and it proves everything about
  # the decisions: who wins a contended acquire, that a live lease is not stealable, that an
  # expired one is, that release is owner-scoped, that re-acquiring is a renew.
  local d; d="$(mktemp -d)"; trap "rm -rf '$d'" EXIT
  export PLX_TV_LOCK_LOCAL="$d"
  REMOTE_LOCK="$d/plx-tv.lock"
  local fails=0
  t() {  # t <name> <expected act> ; RESP already set
    parse_resp
    if [ "$ACT" = "$2" ]; then printf '  \033[32m✓\033[0m %s (act=%s)\n' "$1" "$ACT"
    else printf '  \033[31m✗\033[0m %s: expected %s, got %s\n' "$1" "$2" "${ACT:-none}"; fails=$((fails+1)); fi
  }
  echo "== tv-lock selftest (no television involved)"
  # Switch lanes wholesale — token AND identity. Carrying one lane's label into the other's
  # writes is how the "previous holder" evidence below ends up naming the wrong one.
  lane() { TOKEN="$1"; LABEL="$2"; WHY="lane $2"; ACQUIRED_AT=$(now); }
  lane aaa A
  RESP="$(remote_op acquire 600 0)";  t "A takes a free lock"                acquired
  RESP="$(remote_op status 0 0)";     t "status sees it held"                held
  lane bbb B
  RESP="$(remote_op acquire 600 0)";  t "B is refused while A's lease lives" held
  RESP="$(remote_op release 0 0)";    t "B cannot release A's lock"          notmine
  lane aaa A
  RESP="$(remote_op acquire 600 0)";  t "A re-acquiring is a renew"          renewed
  # Expire A's lease by writing one that ended in the past, then let B in.
  RESP="$(remote_op acquire -600 0)" ; parse_resp
  lane bbb B
  RESP="$(remote_op acquire 600 0)";  t "B takes an EXPIRED lease"           stolen
  if [ "$(fld label "$PREV_BLK")" = A ]; then printf '  \033[32m✓\033[0m it named the previous holder (A)\n'
  else printf '  \033[31m✗\033[0m previous holder should be A, got %s\n' "$(fld label "$PREV_BLK")"; fails=$((fails+1)); fi
  RESP="$(remote_op renew 600 0)";    t "B renews its own"                   renewed
  lane aaa A
  RESP="$(remote_op renew 600 0)";    t "A cannot renew what it lost"        notmine
  lane ccc C
  RESP="$(remote_op acquire 600 1)";  t "--force breaks a LIVE foreign lease" broken
  RESP="$(remote_op release 0 0)";    t "owner releases"                     released
  RESP="$(remote_op status 0 0)";     t "lock is free again"                 free
  RESP="$(remote_op release 0 0)";    t "releasing nothing is not an error"  absent
  # The partial-acquire window: a lock directory with no owner file is an acquire IN FLIGHT, and
  # the grace path must not read it as a corpse and hand the set to two lanes at once.
  mkdir -p "$REMOTE_LOCK"
  TOKEN=ccc
  RESP="$(remote_op acquire 600 0)";  t "an owner-less lock dir is taken, not a wedge" stolen
  echo ""
  [ "$fails" = 0 ] && { echo "selftest: all good"; return 0; }
  echo "selftest: $fails FAILED"; return 1
}

case "${1:-}" in
  acquire) shift; cmd_acquire "$@" ;;
  release) shift; cmd_release "$@" ;;
  renew)   shift; cmd_renew "$@" ;;
  status)  shift; cmd_status "$@" ;;
  require) shift; cmd_require "$@" ;;
  with)    shift; cmd_with "$@" ;;
  break)   shift; cmd_break "$@" ;;
  selftest) shift; cmd_selftest ;;
  *) sed -n '3,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
