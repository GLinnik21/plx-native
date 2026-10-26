#!/usr/bin/env bash
#
# tv-lock.sh — the mutex the television never had.
#
#   tv-lock.sh status                       who holds the set, and is anybody on it unlocked
#   tv-lock.sh acquire --why "<what>"       take it (waits with --wait, steals only when expired)
#   tv-lock.sh renew                        extend the lease you already hold
#   tv-lock.sh release                      hand it back
#   tv-lock.sh require                      assert + renew; what every TV-facing tool calls
#   tv-lock.sh with --why "<what>" -- CMD…  acquire (or inherit yours), run CMD, release what it took even on Ctrl-C (not started on IN USE)
#   tv-lock.sh break [--yes]                steal a lease (names the holder first; refuses a LIVE one without --yes)
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
# See .agents/skills/tv-lock/SKILL.md for the workflow, and docs/agent-reference.md's testing section for what
# the lock does and does not protect (a lease cannot see a human watching a film; the pre-flight
# after an acquire reads what is in front and prints IDLE, `IN USE:` or UNKNOWN, as of that read only).
#
# EXIT CODES. 0 done (an `acquire` on an IN USE set still exits 0 and prints the `IN USE:` line);
# 1 refused (held by another lane, no lease to renew, a live lease `break` will not take, a `break` that
# lost the race, or a `with` whose acquire failed for any reason); 2 `acquire`, `status` or `release` could
# not reach the TV, no TV is configured, `acquire` lost a race, or a usage error (`require` exits 0 for the
# first two on purpose: an unreachable set is not a collision); 3 `with` did not start its command because
# the pre-flight found the set IN USE (a lease it just took is released, one this lane already held stays
# held; the owner says go, then PLX_TV_IN_USE_OK=1). `with` otherwise exits with its command's own status.
#
# ENVIRONMENT. PLX_TV_LOCK_PATH / PLX_TV_LOCK_STATE / PLX_TV_LOCK_LANE / PLX_TV_LOCK_TTL move the lock, the
# lease mirror, the lane and the lease length; PLX_TV_LOCK_BYPASS=1 skips the check (`require`);
# PLX_TV_IN_USE_OK=1 lets `with` and tests/run.py go on an IN USE set, on a lease they take or one this
# lane already holds (only after the owner said go);
# PLX_TV_PREFLIGHT_SECS (default 10) bounds the whole pre-flight read, and PLX_TV_RENEW_SECS (default 240)
# is how often `with` renews while its command runs: both are test seams; PLX_TV_LOCK_LOCAL / PLX_TV_LOCK_LOCAL_PROBE=FILE are the selftest's "there is no television" switch
# and the file whose text stands in for the set's pre-flight reply.
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
SESSIONS=""     # the Claude Code sessions that have used this lane's lease (see lease_write)

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

# The ONE line a person needs when the set is about to be in use: printed (plain, at the start of its
# line, on stdout) when a NEW lease is taken -- `acquire`, `break`, or your own EXPIRED lease re-taken -- and
# never for a live renew. A shell cannot call PushNotification, so the PostToolUse hook
# .claude/hooks/tv-lock-notify.py keys on the leading `HEADS-UP:` and tells the agent to send it. `sane`
# keeps a --why to one line.
heads_up() {  # $1 = why, $2 = lease minutes
  printf 'HEADS-UP: %s, lease %s min\n' "$(sane "${1:-no reason given}")" "$2"
}

# ------------------------------------------------------------------- the TV ---
# $TV, $TV_HOST, then tools/tv-config.sh: this checkout's `.tv-host`, the main checkout's, then the
# per-user `tv-host`. The last two matter here more than anywhere: `.tv-host` is gitignored, so a
# LINKED WORKTREE has none — and worktrees are where the parallel agents live. Without them, the one
# lane most likely to collide is also the only one that cannot ask who holds the set.
resolve_tv() {
  local h="${TV:-${TV_HOST:-}}"
  [ -n "$h" ] || h="$("$REPO/tools/tv-config.sh" host 2>/dev/null)"
  printf '%s' "$(printf '%s' "$h" | tr -d ' \t\n\r')"
}
HOST="$(resolve_tv)"

# Every ssh goes through tools/tv-ssh: this Mac's key first, `sshpass` (webosbrew's published
# dev-mode root password -- the same on every rooted set, so it identifies nobody; the ADDRESS is
# the part that stays out of the repo) only when the set refuses the key, and a fast failure for a
# set that is unreachable. Its stderr is dropped here: callers decide what an unreachable set means.
# SSH_TV_OPTS (an array a caller sets, as preflight_note does with a `local`) rides before the `tv`
# placeholder, where tools/tv-ssh passes ssh options through untouched.
ssh_tv() {
  # PLX_TV_LOCK_LOCAL is the selftest's "there is no television" switch (remote_op runs the device
  # half under a local sh for it too): nothing is asked of the set, and it always answers.
  [ -z "${PLX_TV_LOCK_LOCAL:-}" ] || return 0
  PLX_TV_ADDR="$HOST" "$REPO/tools/tv-ssh" ssh ${SSH_TV_OPTS[@]+"${SSH_TV_OPTS[@]}"} tv "$@" 2>/dev/null
}

# A command with a DEADLINE. macOS has no `timeout(1)`, and nothing under tools/ offers one to share
# (tv-sched-trace.sh's bound is private to its read-back loop), so this is the one.
#   run_bounded SECONDS CMD ARGS…   CMD's stdout is this function's stdout, its status is returned, or 124
#                                   when the deadline came first (the timeout(1) convention); 125 when it
#                                   could not even start. CMD may be a shell function; stdin is /dev/null.
# On the deadline CMD and everything it started (tv-ssh's ssh, sshpass's ssh) are killed from one `ps`
# snapshot, TERM and then KILL. On EVERY path out (normal exit, deadline, INT/TERM/HUP to the caller) the
# watchdog and its `sleep` go too, so nothing outlives the call. CMD's stdout goes to a file, never the
# caller's pipe, so a straggler cannot hold a `$(...)` open. The INT/TERM/HUP trap is the caller's for
# the duration: none of this file's callers sets its own.
_tree_pids() {  # _tree_pids <pid>: that process and every descendant, one pid per line
  [ -n "${1:-}" ] || return 0
  ps -A -o pid= -o ppid= 2>/dev/null | awk -v root="$1" '
    { kid[$2] = kid[$2] " " $1 }
    END { n = 1; q[1] = root
          for (i = 1; i <= n; i++) { print q[i]; m = split(kid[q[i]], c, " "); for (j = 1; j <= m; j++) q[++n] = c[j] } }'
}
_signal_all() {  # _signal_all <signal> <pid>…
  local s="$1" p; shift
  for p in "$@"; do kill "-$s" "$p" 2>/dev/null; done
  return 0
}
run_bounded() {
  local secs="$1" dir pid wd rc hit=0
  shift
  dir="$(mktemp -d "${TMPDIR:-/tmp}/plx-bounded.XXXXXX")" || return 125
  trap '_signal_all KILL $(_tree_pids "${pid:-}") $(_tree_pids "${wd:-}"); rm -rf "$dir"; trap - INT TERM HUP; exit 143' INT TERM HUP
  "$@" >"$dir/out" </dev/null &
  pid=$!
  ( sleep "$secs"
    : > "$dir/hit"
    _tree_pids "$pid" > "$dir/pids"
    _signal_all TERM $(cat "$dir/pids")
    sleep 1
    _signal_all KILL $(cat "$dir/pids") ) >/dev/null 2>&1 &
  wd=$!
  wait "$pid"; rc=$?
  _signal_all TERM $(_tree_pids "$wd")
  wait "$wd" 2>/dev/null
  if [ -e "$dir/hit" ]; then hit=1; _signal_all KILL $(cat "$dir/pids" 2>/dev/null); fi
  trap - INT TERM HUP
  cat "$dir/out" 2>/dev/null
  rm -rf "$dir"
  if [ "$hit" = 1 ] && [ "$rc" != 0 ]; then return 124; fi
  return "$rc"
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
#
# SESSIONS is the one field only this script writes: the Claude Code session ids (from
# $CLAUDE_CODE_SESSION_ID, the id the hook payload carries too, a subagent's being its parent's) that
# took this TOKEN's lease. tv-lock-guard.py reads it when a command names a lane by a script path or
# a cd rather than by the cwd, so session A cannot run /B/tools/... on the strength of B's lease.
# `acquire` and `break` (and so `with`) reset it to the current session, `renew` appends it, and a
# new TOKEN starts it afresh. Unset id, no line; a lease with no line is the owner's terminal's.
cur_session() { printf '%s' "${CLAUDE_CODE_SESSION_ID:-}" | tr -cd 'A-Za-z0-9_-'; }
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
  [ -z "$SESSIONS" ] || printf "SESSIONS='%s'\n" "$SESSIONS" >> "$LEASE"
}
lease_clear() { rm -f "$LEASE"; }
lease_load() {
  TOKEN=""; EXPIRES=0; ACQUIRED=0; VERIFIED=0; SESSIONS=""
  [ -f "$LEASE" ] && . "$LEASE" 2>/dev/null
  FILE_TOKEN="$TOKEN"
  # An inherited token beats the file: inside `with`, or inside anything it spawned, the lease is
  # the parent's and may not have been written for this lane at all.
  [ -n "${PLX_TV_LOCK_TOKEN:-}" ] && TOKEN="$PLX_TV_LOCK_TOKEN"
  [ "$TOKEN" = "$FILE_TOKEN" ] || SESSIONS=""     # the sessions belong to the file's TOKEN
  [ -n "$TOKEN" ]
}
new_token() {
  TOKEN="$( (head -c 12 /dev/urandom 2>/dev/null | od -An -tx1 | tr -d ' \n') )"
  [ -n "$TOKEN" ] || TOKEN="$$-$(now)"
  ACQUIRED_AT="$(now)"
  SESSIONS="$(cur_session)"
}

need_host() {
  [ -n "$HOST" ] && return 0
  bad "no TV configured — put its IP in ~/.config/plxnative/tv-host (or .tv-host), or pass TV=<ip>"
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
  [ -n "$ACT" ] || { bad "no answer from the TV — it is unreachable"; return 2; }
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
      # An expired lease still names its owner until someone else takes it, so the token matches and
      # do_acquire answers `renewed`; but the set may have sat idle past the expiry, so it is a new
      # lease as far as the person is concerned. do_acquire overwrites $ACT, so read it first.
      local was="$ACT"
      SESSIONS="$(cur_session)"     # acquiring is claiming: this session, no longer the last one's
      if do_acquire "$ttl" 0; then
        ok "lock renewed (already yours) — $(fmt_dur $((ttl*60))) from now"
        # An expired lease of your own, re-taken, is a NEW lease to the person, so it gets the pre-flight
        # and the line a new acquire gets (the line last, as below); a live renew prints neither.
        if [ "$was" = expired ]; then preflight_note; heads_up "$WHY" "$ttl"; fi
        exit 0
      fi
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
      heads_up "$WHY" "$ttl"        # last, so a `| tail` keeps it (a LIVE in-place renew above prints none;
                                    # re-taking your own EXPIRED lease prints it from there)
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
      echo "    - if that lease is a corpse: tools/tv-lock.sh break (shows the holder), then break --yes to take it" >&2
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
    "")       bad "no answer from the TV; local lease dropped anyway"; lease_clear; exit 2 ;;
  esac
  exit 0
}

cmd_renew() {
  parse_opts "$@"
  need_host || exit 2
  lease_load || { bad "no lease in this lane to renew"; exit 1; }
  local ttl="${TTL_OVERRIDE:-$TTL_MIN}" me; me="$(cur_session)"
  case " $SESSIONS " in *" $me "*) ;; *) [ -z "$me" ] || SESSIONS="${SESSIONS:+$SESSIONS }$me" ;; esac
  RESP="$(remote_op renew $(( ttl * 60 )) 0)"; parse_resp
  case "$ACT" in
    renewed) lease_write "$(fld expires "$OWNER_BLK")"; ok "lease renewed — $(fmt_dur $((ttl*60))) from now" ;;
    *) bad "cannot renew (act=$ACT) — the lease is no longer yours"; exit 1 ;;
  esac
}

# ------------------------------------------------------------ is anybody watching ---
# The verdict. A lease says no other LOCKED job is on the set; this says whether a PERSON is, from
# the facts one round trip can read: the foreground app, the panel power state and the mute flag.
# Pure, so the selftest runs it on fixtures.
#   idle:   the set in `Active Standby` (nobody is looking, whatever is queued); or one of these in front:
#           our DEBUG app (a lane's, whatever the sound), the PARKING app (`tv-session.sh background` parks
#           the set there between lanes), the LG Home launcher, or live TV (this household has no antenna,
#           so the owner counts live TV as idle, 2026-10-14: not a general rule); or our STABLE app, the
#           household's own client, but only when the mute flag is positively read as true;
#   in use: any other app id in front (a streaming app, an HDMI input), or our stable app with the sound on;
#   unknown: nothing readable or unambiguous (an ambiguous panel reply under an app that would be idle
#           included), or our stable app with the mute flag unreadable. NEVER idle: not knowing is not
#           vouching.
# The panel is NOT evidence of idleness except in standby: `Screen Off` is only the backlight (the owner
# blanks it for tests, and on 2026-10-02 a lane blanked it over somebody's unmuted YouTube). And a normal
# idle set is often UNMUTED (lanes restore volume 12), so sound alone never makes the set IN USE; it only
# decides our own stable app. The app id is compared RAW: an id that is not shaped like one
# (letters, digits, . _ : -) is unknown rather than cleaned into something that might match.
# An ambiguous panel reply (`?`) cannot vouch for idleness, but it must not hide an app in front either: a
# streaming app is IN USE whatever the panel said. So the app verdict is computed first and only an IDLE one
# is demoted to UNKNOWN; IN USE and UNKNOWN stand as they are. Standby is the one panel state that decides.
idle_verdict() {  # idle_verdict <foreground appId> <panel state> <muted: true|false|other> <stable id> <debug id> <parking id>
  local verdict
  if [ "$2" = "Active Standby" ]; then echo "IDLE (panel off: Active Standby)"; return 0; fi
  verdict="$(idle_app_verdict "$@")"
  case "$2:$verdict" in "?:IDLE"*) verdict="UNKNOWN (the panel state was ambiguous)" ;; esac
  echo "$verdict"
}
idle_app_verdict() {  # the same arguments; the panel state is read by idle_verdict, not here
  local LC_ALL=C app="$1" muted="$3" stable="${4:-}" debug="${5:-}" park="${6:-}"
  case "$app" in
    ''|*[!A-Za-z0-9._:-]*) echo "UNKNOWN (the foreground app could not be read)"; return 0 ;;
  esac
  if [ "${#app}" -gt 100 ]; then echo "UNKNOWN (the foreground app could not be read)"; return 0; fi
  if [ -n "$debug" ] && [ "$app" = "$debug" ]; then echo "IDLE (this project's own app: $app)"; return 0; fi
  if [ -n "$park" ] && [ "$app" = "$park" ]; then echo "IDLE (the parking app: $app)"; return 0; fi
  if [ -n "$stable" ] && [ "$app" = "$stable" ]; then
    case "$muted" in
      true)  echo "IDLE (this project's own app: $app, muted)" ;;
      false) echo "IN USE ($app, sound is on)" ;;
      *)     echo "UNKNOWN (our stable app is in front and the mute state could not be read)" ;;
    esac
    return 0
  fi
  case "$app" in
    com.webos.app.home)   echo "IDLE (LG Home launcher)" ;;
    com.webos.app.livetv) echo "IDLE (live TV counts as idle)" ;;
    *)                    echo "IN USE ($app)" ;;
  esac
}

# The parking app: where `tv-session.sh background` leaves the set between lanes, so a normal lane's
# acquire does not read its predecessor's parking as somebody watching. It is that script's own default
# (read from its `PLX_BG_APP:-<id>` line rather than copied here), or PLX_BG_APP when set.
park_app_id() {
  local p="${PLX_BG_APP:-}"
  [ -n "$p" ] || p="$(sed -n 's/.*PLX_BG_APP:-\([A-Za-z0-9._-]*\)}.*/\1/p' "$REPO/tools/tv-session.sh" 2>/dev/null | head -1)"
  printf '%s' "$p"
}

# The ONE line that stops an agent: plain, at the start of its line, on stdout, like heads_up (a
# `warn` would put `  ! ` in front of the marker). .claude/hooks/tv-lock-notify.py keys on the
# leading `IN USE:` and tells the agent to stop and tell the owner. THE CONTRACT, for every reader of
# a pre-flight: an IN USE verdict is exactly one stdout line starting `IN USE:` at column 0 and nothing
# else ever starts a line that way; `acquire` and `status` still exit 0 with it (the lease is kept);
# `with` keys on it to refuse to start its command (exit 3), and tests/run.py does the same.
in_use_line() {  # $1 = the app id
  printf 'IN USE: %s: stop and tell the owner\n' "$1"
}
# The ONE test `with` applies to a pre-flight, whether the lease is one it just took or one this lane
# already held: it reads the pre-flight's stdout on stdin and succeeds when it carries the `IN USE:`
# line and the owner has not said go (PLX_TV_IN_USE_OK=1).
in_use_blocks() { grep -q '^IN USE:' && [ "${PLX_TV_IN_USE_OK:-0}" != 1 ]; }

# One luna reply out of the probe's output, collapsed to a single line (the set's pty adds CRs, and a
# reply may be pretty-printed), the way tv-session.sh collapses one before it matches.
luna_section() {  # luna_section <tag> <probe output>
  printf '%s\n' "$2" | tr -d '\r' \
    | awk -v t="@@LUNA $1" '$0 == t { on = 1; next } /^@@LUNA / { on = 0 } on' \
    | tr '\n' ' ' | tr -s ' '
}

# One key's value out of a collapsed reply, only when it is UNAMBIGUOUS: every occurrence of the key carries
# the same well-formed value (the real getForegroundAppInfo reply repeats its appId in a foregroundAppInfo
# array, and the same value twice is fine). Prints the value (a string without its quotes, or a bare
# true / false / number); "" when the key is absent; "?" when it occurs with different values, or with a
# value that did not parse (half-written, an escaped quote, a key inside an errorText). "?" is not shaped
# like any app id, panel state or flag, so every caller reads it as unknown.
luna_val() {  # luna_val <key> <collapsed reply>
  local n seen vals
  n=$(printf '%s' "$2" | grep -o "\"$1\"" | wc -l | tr -d ' ')
  [ "$n" -gt 0 ] || return 0
  vals=$(printf '%s' "$2" | grep -oE "\"$1\": *(\"[^\"]*\"|true|false|-?[0-9]+)" | sed -E "s/^\"$1\": *//; s/^\"//; s/\"\$//")
  seen=$(printf '%s\n' "$vals" | grep -c .)
  if [ "$seen" = "$n" ] && [ "$(printf '%s\n' "$vals" | sort -u | wc -l | tr -d ' ')" = 1 ]; then
    printf '%s' "$vals" | head -1
  else
    printf '?'
  fi
}
luna_ok() { [ "$(luna_val returnValue "$1")" = true ]; }   # luna_ok <collapsed reply>: it said returnValue true, and only that

# The remote text that prints one tagged luna reply. `luna-send` silently no-ops without a
# controlling TTY, hence the `script -qc` wrapper every other luna call in this repo uses
# (tools/tv-session.sh: fg_app, cmd_screen, cmd_sound).
luna_probe() {  # luna_probe <tag> <service/method>
  printf 'echo "@@LUNA %s"; script -qc "luna-send -n 1 -f luna://%s {}" /dev/null; echo; ' "$1" "$2"
}

# The `IN USE:` line this prints is a contract with its readers: see in_use_line.
# What a probe's reply says, as lines: the app note, the ssh count, then the verdict and the sound.
# Pure (a selftest feeds it fixtures). Line 1 is the fuser answer per install, line 2 the ssh count,
# then one tagged section per luna read. Each luna read is believed only when it says `returnValue`
# true (and only that); a value that is absent, contradictory or malformed reads as unknown.
preflight_report() {  # preflight_report <probe output> <stable app id> <debug app id> <parking app id>
  local raw="$1" apps ssh_n fgr volr powr fg power muted vol verdict inuse shown_muted shown_vol
  apps="$(printf '%s\n' "$raw" | head -1)"
  ssh_n="$(printf '%s\n' "$raw" | sed -n '2p')"
  case "$apps" in
    *[0-9]*) info "note: an app is already running on the set (stable/debug: $apps)" ;;
  esac
  # The count includes THIS command's own connection, so 2+ means somebody else is on the wire —
  # possibly from a machine whose processes you cannot see with `ps`.
  if [ -n "$ssh_n" ] && [ "$ssh_n" -gt 1 ] 2>/dev/null; then
    warn "$ssh_n ssh sessions on the TV (one is mine) — somebody may be driving it unlocked"
  fi
  fgr="$(luna_section fg "$raw")"; volr="$(luna_section vol "$raw")"; powr="$(luna_section power "$raw")"
  fg=""; power=""; muted=""; vol=""
  if luna_ok "$fgr";  then fg="$(luna_val appId "$fgr")"; fi
  if luna_ok "$powr"; then power="$(luna_val state "$powr")"; fi
  if luna_ok "$volr"; then muted="$(luna_val muted "$volr")"; vol="$(luna_val volume "$volr")"; fi
  if [ -n "$muted$vol" ]; then
    case "$muted" in true|false) shown_muted="$muted" ;; *) shown_muted=unknown ;; esac
    case "$vol" in ''|*[!0-9]*) shown_vol='?' ;; *) shown_vol="$vol" ;; esac
    info "sound: muted=$shown_muted volume=$shown_vol"
  fi
  verdict="$(idle_verdict "$fg" "$power" "$muted" "$2" "$3" "${4:-}")"
  case "$verdict" in
    IDLE*)    info "$verdict" ;;
    "IN USE"*) inuse="${verdict#IN USE (}"; in_use_line "${inuse%)}" ;;
    *)        warn "$verdict — nobody vouches that the television is idle; treat it as possibly in use" ;;
  esac
}

# The pre-flight the agent-reference block used to spell by hand. It answers the OTHER half of the
# question the lock cannot: a lease says no other LOCKED job is on the set, and this says whether
# anybody is on it anyway — an older job, a colleague, a human with the remote.
#
# ONE round trip: which of the two installs has a process, the ssh count, and the three luna reads
# the verdict needs (foreground app, volume, power). `getPowerState` and `getVolume` are read-only
# calls; tools/tv-session.sh runs the same two services' other methods, but THIS read of the power
# state has not been run against a set yet, and its `state` strings (`Screen Off`, `Active
# Standby`) are the ones cmd_screen's replies carry. A reply that does not parse is UNKNOWN.
#
# BOUNDED: the whole read has PLX_TV_PREFLIGHT_SECS (default 10) and then it is UNKNOWN, because a luna
# service that never answers would otherwise hang `acquire`, `status` and tests/run.py behind a lease
# that is already written. The deadline is local (run_bounded kills ssh and what it started); the
# keep-alive options only cut a dead network sooner. Neither changes any exit code.
preflight_note() {
  local sd dd sid did out rc secs="${PLX_TV_PREFLIGHT_SECS:-10}"
  local SSH_TV_OPTS=(-o ServerAliveInterval=3 -o ServerAliveCountMax=2)
  case "$secs" in ''|0|*[!0-9]*) secs=10 ;; esac
  # The selftest has no television: nothing is asked of the set. Its PLX_TV_LOCK_LOCAL_PROBE=FILE
  # stands the file's text in for the probe's output, so a case can force any verdict through the
  # real subcommands.
  [ -z "${PLX_TV_LOCK_LOCAL:-}" ] || [ -f "${PLX_TV_LOCK_LOCAL_PROBE:-}" ] || return 0
  { read -r sid; read -r sd; } < <(make -s -C "$REPO" FLAVOR=stable print-appid print-appdir 2>/dev/null)
  { read -r did; read -r dd; } < <(make -s -C "$REPO" FLAVOR=debug  print-appid print-appdir 2>/dev/null)
  if [ -n "${PLX_TV_LOCK_LOCAL:-}" ]; then
    out="$(cat "$PLX_TV_LOCK_LOCAL_PROBE")"
  else
    [ -n "$sd" ] && [ -n "$dd" ] || return 0
    # `fuser` on each install's own binary, never `pidof plxnative`: both binaries carry that name, so
    # a name-scoped test matches BOTH and answers in an order busybox does not promise.
    out="$(run_bounded "$secs" ssh_tv "for d in '$sd' '$dd'; do printf '%s ' \"\$(fuser \$d/plxnative 2>/dev/null || echo NONE)\"; done; echo; netstat -an 2>/dev/null | grep -c 'ESTABLISHED.*:22 \|:22 .*ESTABLISHED'; $(luna_probe fg com.webos.applicationManager/getForegroundAppInfo)$(luna_probe vol com.webos.service.audio/getVolume)$(luna_probe power com.webos.service.tvpower/power/getPowerState)")"
    rc=$?
    if [ "$rc" = 124 ]; then
      warn "UNKNOWN (the pre-flight read timed out after ${secs}s) — nobody vouches that the television is idle; treat it as possibly in use"
      return 0
    fi
  fi
  preflight_report "$out" "$sid" "$did" "$(park_app_id)"
}

cmd_status() {
  parse_opts "$@"
  need_host || exit 2
  if [ -n "${PLX_VERBOSE:-}" ]; then echo "== TV lock: $HOST"; else echo "== TV lock: the TV"; fi
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
  # remote_op writes $TOKEN into its owner record and `set -u` kills it when nothing bound it: with no
  # lease_load here a lane that holds no lease (the one that breaks) got an empty reply, read it as
  # "not held", and skipped the live-lease refusal below. cmd_status binds it the same way.
  lease_load || true
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
  heads_up "break: $WHY" "${TTL_OVERRIDE:-$TTL_MIN}"
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
    [ "$QUIET" = 1 ] || warn "cannot reach the TV to check the lock — continuing"
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
  # free or expired: take the short implicit lease. No HEADS-UP here: tv-lock-guard.py refuses an unleased device
  # command before it gets here, so this branch is normally a script or a human; the exceptions are a lease the TV
  # lost (a reboot) while the local mirror still reads live, and a hand-typed `require`.
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
    # The acquire's stdout is passed through as it comes AND kept: its pre-flight may print `IN USE:`
    # (see in_use_line), and then the command is not started. The lease is released, not kept, because
    # `with` is the one that owns it and nothing would ever come back to release it.
    local acq; acq="$(mktemp "${TMPDIR:-/tmp}/plx-with.XXXXXX")" || exit 2
    if ! ( exec "$0" acquire ${WHY:+--why "$WHY"} ${LABEL:+--as "$LABEL"} \
             ${TTL_OVERRIDE:+--ttl "$TTL_OVERRIDE"} --wait "$WAIT" ) | tee "$acq"; then
      rm -f "$acq"; exit 1
    fi
    lease_load || { rm -f "$acq"; exit 1; }
    if in_use_blocks < "$acq"; then
      rm -f "$acq"
      "$0" release >/dev/null
      echo "tv-lock: with: the pre-flight found the television IN USE (the IN USE: line above), so your command was NOT started and the lease is released. Tell the owner now; once they say go, rerun with PLX_TV_IN_USE_OK=1." >&2
      exit 3
    fi
    rm -f "$acq"
  else
    # A lease this lane already holds was pre-flighted when it was taken, perhaps hours ago, so read the
    # set again before a command with no model in the loop starts. `with` did not take this lease, so on
    # IN USE it leaves it held (the lane that took it had its own reasons) and only refuses to start.
    local pf; pf="$(preflight_note)"
    [ -z "$pf" ] || printf '%s\n' "$pf"
    if in_use_blocks <<< "$pf"; then
      echo "tv-lock: with: the pre-flight found the television IN USE (the IN USE: line above), so your command was NOT started. The lease this lane already held is still held (with did not take it, so it did not release it). Tell the owner now; once they say go, rerun with PLX_TV_IN_USE_OK=1." >&2
      exit 3
    fi
  fi

  # Keep the lease warm for as long as the child runs. Without this a legitimately long job — the
  # 21-case suite, a soak — expires under itself and the next lane steals the television mid-run.
  # Its output is dropped (it prints nothing) so it can never pin the caller's pipe, and the trap
  # kills its whole tree: killing only the subshell orphaned its `sleep 240`, which held a `$(...)`
  # or a `| tail` open for four minutes after the command finished.
  local renewer=0
  if [ "$inherited" = 0 ]; then
    ( while sleep "${PLX_TV_RENEW_SECS:-240}"; do "$0" renew >/dev/null 2>&1 || exit 0; done ) >/dev/null 2>&1 & renewer=$!
  fi
  trap 'rc=$?; [ '"$renewer"' -gt 0 ] && _signal_all TERM $(_tree_pids '"$renewer"'); [ '"$inherited"' = 0 ] && "$0" release >/dev/null; exit $rc' EXIT INT TERM HUP

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

  # The HEADS-UP line (.claude/hooks/tv-lock-notify.py keys on it), through the real subcommands
  # rather than remote_op: one on a NEW lease (acquire, break, your own expired one re-taken), none on a live renew or a refusal. Each
  # call is a child with its own lane, lease directory and lock path under $d, so nothing here can
  # reach the operator's real lease; PLX_TV_LOCK_LOCAL keeps ssh_tv, ensure_awake and
  # preflight_note off the network, and TV is a placeholder so no tv-config lookup happens either.
  local self="${BASH_SOURCE[0]}" out rc
  hu() {  # hu <lane> <tv-lock.sh args…>  ->  $out (stdout+stderr), $rc. HU_SID is the session id
          # the child sees as CLAUDE_CODE_SESSION_ID; unset, it sees none (a run inside a Claude session
          # exports one, and the cases must not depend on it).
    local lane="$1"; shift
    out="$(env -u PLX_TV_LOCK_TOKEN -u PLX_TV_LOCK_INHERITED -u PLX_TV_LOCK_BYPASS \
      -u CLAUDE_CODE_SESSION_ID ${HU_SID:+CLAUDE_CODE_SESSION_ID=$HU_SID} \
      TV=selftest.invalid PLX_TV_LOCK_LOCAL="$d" PLX_TV_LOCK_PATH="$d/headsup.lock" \
      PLX_TV_LOCK_STATE="$d/headsup-state" PLX_TV_LOCK_LANE="$d/lane-$lane" "$self" "$@" 2>&1)"
    rc=$?
  }
  want() {  # want <name> <exit> <HEADS-UP line count> [<the one line, whole>]
    local n line; n=$(printf '%s\n' "$out" | grep -c '^HEADS-UP: ')
    line=$(printf '%s\n' "$out" | grep '^HEADS-UP: ' | head -1)
    if [ "$rc" = "$2" ] && [ "$n" = "$3" ] && { [ -z "${4:-}" ] || [ "$line" = "$4" ]; }; then
      printf '  \033[32m✓\033[0m %s (exit %s, %s HEADS-UP)\n' "$1" "$rc" "$n"
    else
      printf '  \033[31m✗\033[0m %s: want exit %s with %s HEADS-UP%s; got exit %s with %s: %s\n' \
        "$1" "$2" "$3" "${4:+ \"$4\"}" "$rc" "$n" "${line:-none}"
      fails=$((fails+1))
    fi
  }
  hu A acquire --why "heads-up case" --ttl 7
  want "a new acquire prints one HEADS-UP" 0 1 "HEADS-UP: heads-up case, lease 7 min"
  hu A acquire --why "again" --ttl 7
  want "acquiring what you hold is a renew: none" 0 0
  hu A renew --ttl 7
  want "renew: none" 0 0
  hu B acquire --why "intruder"
  want "an acquire the set refuses: none" 1 0
  hu B break --why "case"
  want "a break without --yes is refused while the lease is live: none" 1 0
  hu B break --yes --why "case" --ttl 7
  want "break prints one HEADS-UP" 0 1 "HEADS-UP: break: case, lease 7 min"
  hu B release
  want "release: none" 0 0
  hu A acquire --why "$(printf 'two\nlines "quoted"')" --ttl 7
  want "a --why with a newline and quotes is still ONE line" 0 1 "HEADS-UP: two lines quoted, lease 7 min"
  # An EXPIRED lease of your own is not a live one: the set may have sat idle past it, so taking it
  # again is a new lease and prints the line; a live own lease re-taken stays a silent renew. A
  # negative --ttl writes a lease that has already ended, and A still holds a live one from above.
  hu A acquire --why "shorten" --ttl -1
  want "renewing a live own lease, even to an expired one: none" 0 0
  hu A acquire --why "back after expiry" --ttl 7
  want "re-taking your own EXPIRED lease prints one HEADS-UP" 0 1 "HEADS-UP: back after expiry, lease 7 min"
  hu A acquire --why "live again" --ttl 7
  want "and the next acquire, now live, is a renew: none" 0 0

  # SESSIONS: the one field tv-lock-guard.py reads to decide whether a lane named by a script path or
  # a cd (not the cwd's own) is THIS session's. tools/tv-lock.sh is its only writer, from
  # $CLAUDE_CODE_SESSION_ID: acquire, break and with reset it, renew appends, and a new TOKEN starts
  # it afresh. sess <name> <lane> <expected, "" = no line> reads the lane's mirror file by its LANE.
  sess() {
    local f got n
    f="$(grep -l "^LANE='$d/lane-$2'\$" "$d"/headsup-state/*.lease 2>/dev/null | head -1)"
    got="$(sed -n "s/^SESSIONS='\(.*\)'\$/\1/p" "$f" 2>/dev/null)"
    n=$(grep -c -E "^(TOKEN|EXPIRES|ACQUIRED|VERIFIED|TV|LANE)=" "$f" 2>/dev/null)
    if [ -n "$f" ] && [ "$got" = "$3" ] && [ "${n:-0}" = 6 ]; then
      printf '  \033[32m✓\033[0m %s (SESSIONS=%s)\n' "$1" "${got:-none}"
    else
      printf '  \033[31m✗\033[0m %s: want SESSIONS=%s with the 6 other fields; got %s (%s fields)\n' \
        "$1" "${3:-none}" "${got:-none}" "${n:-0}"
      fails=$((fails+1))
    fi
  }
  HU_SID=sess-1 hu A acquire --ttl 7
  sess "acquiring what you hold records this session" A "sess-1"
  HU_SID=sess-2 hu A renew --ttl 7
  sess "another session's renew appends it" A "sess-1 sess-2"
  HU_SID=sess-2 hu A renew --ttl 7
  sess "a renew by a session already listed adds nothing" A "sess-1 sess-2"
  hu A renew --ttl 7
  sess "a renew with no session id changes nothing" A "sess-1 sess-2"
  HU_SID=sess-3 hu A acquire --ttl 7
  sess "an acquire resets it to the acquiring session" A "sess-3"
  hu A release
  HU_SID=sess-4 hu A acquire --ttl 7
  sess "a new TOKEN starts afresh, nothing carried from the last" A "sess-4"
  HU_SID=sess-5 hu B break --yes --ttl 7
  sess "break resets it to the breaking session" B "sess-5"
  HU_SID=sess-6 hu B renew --ttl 7
  HU_SID=sess-7 hu B break --yes --ttl 7
  sess "a second break resets it again" B "sess-7"
  hu B release
  hu A release
  hu A acquire --ttl 7
  sess "no CLAUDE_CODE_SESSION_ID: no SESSIONS line at all" A ""
  HU_SID=sess-9 hu A renew --ttl 7
  sess "a renew with an id adds it to a lease that had none" A "sess-9"
  hu A acquire --ttl 7
  sess "an acquire with no id clears it" A ""
  HU_SID="s1;'x\$y" hu A acquire --ttl 7
  sess "an id with quotes and shell metacharacters is cut to its safe characters" A "s1xy"

  local got marker sig
  # IS ANYBODY WATCHING: the verdict preflight_note prints, and the note around it, on fixture replies
  # (shaped like the luna replies tools/tv-session.sh already parses; never observed from a set).
  # The app ids are the Makefile's own (print-appid), not literals of ours, and the parking app's is
  # tv-session.sh's own default (park_app_id).
  chk() {  # chk <name> <1 = pass> [<detail printed on failure>]
    if [ "$2" = 1 ]; then printf '  \033[32m✓\033[0m %s\n' "$1"
    else printf '  \033[31m✗\033[0m %s%s\n' "$1" "${3:+: $3}"; fails=$((fails+1)); fi
  }
  local sid did park
  sid="$(make -s -C "$REPO" FLAVOR=stable print-appid 2>/dev/null)"
  did="$(make -s -C "$REPO" FLAVOR=debug print-appid 2>/dev/null)"
  park="$(PLX_BG_APP= park_app_id)"
  if [ -n "$sid" ] && [ -n "$did" ] && [ "$sid" != "$did" ]; then
    printf '  \033[32m✓\033[0m the two install ids come from the Makefile (print-appid)\n'
  else
    printf '  \033[31m✗\033[0m print-appid gave "%s" / "%s"\n' "$sid" "$did"; fails=$((fails+1))
  fi
  case "$park" in
    ''|*[!A-Za-z0-9._-]*) chk "the parking app id is derived from tv-session.sh's PLX_BG_APP default" 0 "got '$park'" ;;
    *) chk "the parking app id is derived from tv-session.sh's PLX_BG_APP default" 1 ;;
  esac
  chk "PLX_BG_APP overrides it, as it does for tv-session.sh" \
    "$([ "$(PLX_BG_APP=my.park.app park_app_id)" = my.park.app ] && echo 1)"
  verdict() {  # verdict <name> <want, the whole line> <foreground appId> <panel state> [<muted>]
    local got; got="$(idle_verdict "$3" "$4" "${5:-}" "$sid" "$did" "$park")"
    if [ "$got" = "$2" ]; then printf '  \033[32m✓\033[0m %s -> %s\n' "$1" "$got"
    else printf '  \033[31m✗\033[0m %s: want "%s", got "%s"\n' "$1" "$2" "$got"; fails=$((fails+1)); fi
  }
  local unk_app="UNKNOWN (the foreground app could not be read)"
  verdict "our debug app in front is idle, sound on" "IDLE (this project's own app: $did)" "$did" Active false
  verdict "our debug app in front is idle, sound unread" "IDLE (this project's own app: $did)" "$did" Active ""
  verdict "our stable app in front, positively muted, is idle" "IDLE (this project's own app: $sid, muted)" "$sid" Active true
  verdict "our stable app in front with the sound on is IN USE" "IN USE ($sid, sound is on)" "$sid" Active false
  verdict "our stable app in front, mute unread, is UNKNOWN" "UNKNOWN (our stable app is in front and the mute state could not be read)" "$sid" Active ""
  verdict "our stable app in front, mute ambiguous, is UNKNOWN" "UNKNOWN (our stable app is in front and the mute state could not be read)" "$sid" Active "?"
  verdict "the parking app is idle, sound on" "IDLE (the parking app: $park)" "$park" Active false
  verdict "the parking app is idle, muted" "IDLE (the parking app: $park)" "$park" Active true
  verdict "LG Home is idle, sound on" "IDLE (LG Home launcher)" com.webos.app.home Active false
  verdict "live TV is idle (this household has no antenna), sound on" "IDLE (live TV counts as idle)" com.webos.app.livetv Active false
  verdict "a streaming app is IN USE" "IN USE (youtube.leanback.v4)" youtube.leanback.v4 Active true
  verdict "an HDMI input is IN USE" "IN USE (com.webos.app.hdmi1)" com.webos.app.hdmi1 Active true
  verdict "a streaming app with the power state unread is IN USE" "IN USE (youtube.leanback.v4)" youtube.leanback.v4 "" true
  verdict "no foreground app is UNKNOWN, never idle" "$unk_app" "" Active
  verdict "neither read is UNKNOWN" "$unk_app" "" ""
  verdict "an app id that is only junk is UNKNOWN" "$unk_app" '!!' Active
  verdict "an app id with an escape and a newline is UNKNOWN, not cut to its safe characters" "$unk_app" "$(printf 'app\033[31m\nxx')" Active
  verdict "an ambiguous foreground read is UNKNOWN" "$unk_app" "?" Active
  verdict "Home plus a stray character is UNKNOWN, not Home" "$unk_app" 'com.webos.app.home!' Active
  verdict "Home plus a space is UNKNOWN, not Home" "$unk_app" 'com.webos.app.home ' Active
  verdict "a prefix of Home is IN USE" "IN USE (com.webos.app.homeX)" com.webos.app.homeX Active true
  verdict "a prefix of our debug id is IN USE" "IN USE ($did.er)" "$did.er" Active true
  verdict "upper case is not Home: IN USE" "IN USE (COM.WEBOS.APP.HOME)" COM.WEBOS.APP.HOME Active true
  verdict "live TV with a suffix is not live TV: IN USE" "IN USE (com.webos.app.livetv:hdmi)" com.webos.app.livetv:hdmi Active true
  verdict "a non-ASCII byte in an id is UNKNOWN" "$unk_app" "com.webos.app.hom$(printf '\303\251')" Active
  verdict "an id too long to be an app id is UNKNOWN" "$unk_app" "$(printf 'a%.0s' $(seq 1 130))" Active
  verdict "Screen Off is only the backlight: a streaming app is still IN USE" "IN USE (youtube.leanback.v4)" youtube.leanback.v4 "Screen Off" false
  verdict "Screen Off with our stable app and the sound on is IN USE" "IN USE ($sid, sound is on)" "$sid" "Screen Off" false
  verdict "Screen Off with the app unread is UNKNOWN" "$unk_app" "" "Screen Off"
  verdict "standby is idle whatever is in front" "IDLE (panel off: Active Standby)" youtube.leanback.v4 "Active Standby" false
  verdict "standby with the app unread is idle" "IDLE (panel off: Active Standby)" "" "Active Standby"
  # An ambiguous panel read ("?") cannot vouch for idleness, but it must not hide an app in front either:
  # the app verdict is computed first, an IDLE becomes UNKNOWN, and IN USE or UNKNOWN stay as they are.
  verdict "an ambiguous panel read does not hide a streaming app: IN USE" "IN USE (youtube.leanback.v4)" youtube.leanback.v4 "?" false
  verdict "an ambiguous panel read does not hide our stable app with the sound on: IN USE" "IN USE ($sid, sound is on)" "$sid" "?" false
  verdict "an ambiguous panel read does not hide an HDMI input: IN USE" "IN USE (com.webos.app.hdmi1)" com.webos.app.hdmi1 "?" true
  verdict "an ambiguous panel read on Home is UNKNOWN, never idle" "UNKNOWN (the panel state was ambiguous)" com.webos.app.home "?" false
  verdict "an ambiguous panel read on live TV is UNKNOWN, never idle" "UNKNOWN (the panel state was ambiguous)" com.webos.app.livetv "?" false
  verdict "an ambiguous panel read on our debug app is UNKNOWN, never idle" "UNKNOWN (the panel state was ambiguous)" "$did" "?" false
  verdict "an ambiguous panel read on the parking app is UNKNOWN, never idle" "UNKNOWN (the panel state was ambiguous)" "$park" "?" true
  verdict "an ambiguous panel read with our stable app muted is UNKNOWN, never idle" "UNKNOWN (the panel state was ambiguous)" "$sid" "?" true
  verdict "an ambiguous panel read with our stable app's mute unread stays the app's UNKNOWN" "UNKNOWN (our stable app is in front and the mute state could not be read)" "$sid" "?" ""
  verdict "an ambiguous panel read with the app unread stays the app's UNKNOWN" "$unk_app" "" "?"
  verdict "standby still wins over an unreadable app" "IDLE (panel off: Active Standby)" "" "Active Standby"
  verdict "a lit panel does not rescue a streaming app" "IN USE (youtube.leanback.v4)" youtube.leanback.v4 "Active" true

  pf() {  # pf <fg reply> <volume reply> <power reply> [<apps line>] -> $out: preflight_report on a fixture probe
    out="$(preflight_report "$(printf '%s\n3\n@@LUNA fg\r\n%s\r\n@@LUNA vol\r\n%s\r\n@@LUNA power\r\n%s\r\n' \
      "${4:-NONE NONE }" "$1" "$2" "$3")" "$sid" "$did" "$park" 2>&1)"
  }
  pfcheck() {  # pfcheck <name> <ok?>   (the failing output is indented: a column-0 IN USE: would trip the notify hook)
    if [ "$2" = 1 ]; then printf '  \033[32m✓\033[0m %s\n' "$1"
    else printf '  \033[31m✗\033[0m %s: got\n%s\n' "$1" "$(printf '%s\n' "$out" | sed 's/^/    /')"; fails=$((fails+1)); fi
  }
  local capture
  capture="$(fails=0; out="$(in_use_line some.app)"; pfcheck "demo failure" 0)"
  chk "a FAILING pfcheck prints no column-0 IN USE: line (the notify hook would fire on it)" \
    "$(printf '%s\n' "$capture" | grep -q '^IN USE:' || { printf '%s\n' "$capture" | grep -q '^    IN USE: some.app' && echo 1; })" "$capture"
  pfclass() {  # the ONE verdict class $out carries: IDLE | IN USE | UNKNOWN
    local c="" n=0
    printf '%s\n' "$out" | grep -q '^IN USE: ' && { c="IN USE"; n=$((n+1)); }
    printf '%s\n' "$out" | grep -q '^  · IDLE' && { c="IDLE"; n=$((n+1)); }
    printf '%s\n' "$out" | grep -q 'UNKNOWN'   && { c="UNKNOWN"; n=$((n+1)); }
    if [ "$n" = 1 ]; then printf '%s' "$c"; else printf 'NOT ONE CLASS (%s)' "$n"; fi
  }
  rcase() {  # rcase <name> <want: IDLE | IN USE | UNKNOWN> <fg reply> <volume reply> <power reply>
    local got; pf "$3" "$4" "$5"; got="$(pfclass)"
    if [ "$got" = "$2" ]; then printf '  \033[32m✓\033[0m %s -> %s\n' "$1" "$got"
    else printf '  \033[31m✗\033[0m %s: want %s, got %s\n%s\n' "$1" "$2" "$got" "$(printf '%s\n' "$out" | sed 's/^/    /')"; fails=$((fails+1)); fi
  }
  local vol_ok='{"returnValue": true, "muted": true, "volume": 12}'
  local vol_on='{"returnValue": true, "muted": false, "volume": 20}'
  local pow_ok='{"returnValue": true, "state": "Active"}'
  fgj()  { printf '{"returnValue": true, "appId": "%s"}' "$1"; }
  powj() { printf '{"returnValue": true, "state": "%s"}' "$1"; }
  # The adversarial replies the review crafted (probe1.sh letters B D F G I J M N O P ... in its table).
  rcase "an empty foreground reply" UNKNOWN "" "$vol_on" "$pow_ok"
  rcase "B: returnValue false with an appId (Home)" UNKNOWN '{"returnValue": false, "appId": "com.webos.app.home", "errorText": "x"}' "$vol_on" "$pow_ok"
  rcase "B2: returnValue false at the top and true in a nested object (Home)" UNKNOWN '{"returnValue": false, "x": {"returnValue": true}, "appId": "com.webos.app.home"}' "$vol_on" "$pow_ok"
  rcase "a power reply that says returnValue both ways does not vouch for standby" "IN USE" "$(fgj youtube.leanback.v4)" "$vol_on" '{"returnValue": true, "state": "Active Standby", "sub": {"returnValue": false}}'
  rcase "C: an error object only" UNKNOWN '{"returnValue": false, "errorCode": -1, "errorText": "Denied"}' "$vol_on" "$pow_ok"
  rcase "D: two DISTINCT appIds, the real one first, Home last" UNKNOWN '{"returnValue": true, "appId": "youtube.leanback.v4", "previous": {"appId": "com.webos.app.home"}}' "$vol_on" "$pow_ok"
  rcase "E: two DISTINCT appIds, Home first" UNKNOWN '{"returnValue": true, "appId": "com.webos.app.home", "x": {"appId": "youtube.leanback.v4"}}' "$vol_on" "$pow_ok"
  rcase "the real shape: an appId repeated by the foregroundAppInfo array (Home) stays accepted" IDLE '{ "returnValue": true, "appId": "com.webos.app.home", "foregroundAppInfo": [ { "appId": "com.webos.app.home", "windowId": "" } ] }' "$vol_on" "$pow_ok"
  rcase "the real shape, the same appId repeated (a streaming app) stays IN USE" "IN USE" '{ "returnValue": true, "appId": "youtube.leanback.v4", "foregroundAppInfo": [ { "appId": "youtube.leanback.v4" } ] }' "$vol_on" "$pow_ok"
  rcase "a truncated second appId is UNKNOWN, not the first one's verdict" UNKNOWN '{"returnValue": true, "appId": "com.webos.app.home", "foregroundAppInfo": [{"appId": "youtu' "$vol_on" "$pow_ok"
  rcase "F: a prefix of Home (homeX)" "IN USE" "$(fgj com.webos.app.homeX)" "$vol_on" "$pow_ok"
  rcase "G: a prefix of ours (plxnative.debugger)" "IN USE" "$(fgj "$sid.debugger")" "$vol_on" "$pow_ok"
  rcase "H: upper-case Home" "IN USE" "$(fgj COM.WEBOS.APP.HOME)" "$vol_on" "$pow_ok"
  rcase "I: Home plus a junk character" UNKNOWN "$(fgj 'com.webos.app.home!')" "$vol_on" "$pow_ok"
  rcase "J: Home plus a space" UNKNOWN "$(fgj 'com.webos.app.home ')" "$vol_on" "$pow_ok"
  rcase "K: live TV with a :hdmi suffix" "IN USE" "$(fgj com.webos.app.livetv:hdmi)" "$vol_on" "$pow_ok"
  rcase "L: a space before the colon is not read" UNKNOWN '{"returnValue": true, "appId" : "youtube.leanback.v4"}' "$vol_on" "$pow_ok"
  rcase "M: the power reply failed and its errorText mentions Screen Off" "IN USE" "$(fgj youtube.leanback.v4)" "$vol_on" '{"returnValue": false, "errorText": "state Screen Off", "state": "Screen Off"}'
  rcase "N: an unrelated power field holds Screen Off" "IN USE" "$(fgj youtube.leanback.v4)" "$vol_on" '{"returnValue": true, "state": "Active", "processing": "Screen Off"}'
  rcase "O: two DISTINCT power states (a nested one last): the streaming app still reads IN USE" "IN USE" "$(fgj youtube.leanback.v4)" "$vol_on" '{"returnValue": true, "state": "Active", "sub": {"state": "Screen Off"}}'
  rcase "O2: a nested Active Standby does not vouch: the streaming app still reads IN USE" "IN USE" "$(fgj youtube.leanback.v4)" "$vol_on" '{"returnValue": true, "state": "Active", "sub": {"state": "Active Standby"}}'
  rcase "O4: two DISTINCT power states with Home in front is UNKNOWN, never idle" UNKNOWN "$(fgj com.webos.app.home)" "$vol_on" '{"returnValue": true, "state": "Active", "sub": {"state": "Active Standby"}}'
  rcase "O3: the same power state repeated is accepted (standby is idle)" IDLE "$(fgj youtube.leanback.v4)" "$vol_on" '{"returnValue": true, "state": "Active Standby", "sub": {"state": "Active Standby"}}'
  rcase "P: returnValue true only inside an errorText" "IN USE" "$(fgj youtube.leanback.v4)" "$vol_on" '{"returnValue": false, "errorText": "\"returnValue\": true", "state": "Screen Off"}'
  rcase "Q: lower-case screen off" "IN USE" "$(fgj youtube.leanback.v4)" "$vol_on" "$(powj 'screen off')"
  rcase "R: Screen Off, Spotify, the sound on" "IN USE" "$(fgj spotify-beehive)" "$vol_on" "$(powj 'Screen Off')"
  rcase "S: Screen Saver" "IN USE" "$(fgj youtube.leanback.v4)" "$vol_on" "$(powj 'Screen Saver')"
  rcase "2026-10-02: YouTube, unmuted, the panel blanked" "IN USE" "$(fgj youtube.leanback.v4)" '{"returnValue": true, "muted": false, "volume": 15}' "$(powj 'Screen Off')"
  rcase "T: an empty appId" UNKNOWN '{"returnValue": true, "appId": ""}' "$vol_on" "$pow_ok"
  rcase "W: an escaped quote in the appId" UNKNOWN '{"returnValue": true, "appId": "a\"b"}' "$vol_on" "$pow_ok"
  rcase "X: a half-written reply" UNKNOWN '{"returnValue": true, "appId": "youtu' "$vol_on" "$pow_ok"
  rcase "a foreground reply with no returnValue at all" UNKNOWN '{"appId": "youtube.leanback.v4"}' "$vol_on" "$pow_ok"
  rcase "standby, whatever is in front, is idle" IDLE "$(fgj youtube.leanback.v4)" "$vol_on" "$(powj 'Active Standby')"
  rcase "our stable app, muted, lit panel" IDLE "$(fgj "$sid")" "$vol_ok" "$pow_ok"
  rcase "our stable app, the sound on, lit panel (a film)" "IN USE" "$(fgj "$sid")" "$vol_on" "$pow_ok"
  rcase "our stable app, the volume reply failed" UNKNOWN "$(fgj "$sid")" '{"returnValue": false, "errorText": "x"}' "$pow_ok"
  rcase "our stable app, the volume reply never came" UNKNOWN "$(fgj "$sid")" "" "$pow_ok"
  rcase "our stable app, two distinct muted values" UNKNOWN "$(fgj "$sid")" '{"returnValue": true, "muted": true, "x": {"muted": false}, "volume": 20}' "$pow_ok"
  rcase "our debug app, the sound on" IDLE "$(fgj "$did")" "$vol_on" "$pow_ok"
  rcase "the parking app (tv-session.sh background), muted" IDLE "$(fgj "$park")" "$vol_ok" "$pow_ok"
  rcase "the parking app, the sound on" IDLE "$(fgj "$park")" "$vol_on" "$pow_ok"
  rcase "an unparseable reply" UNKNOWN "luna-send: not found" "" "garbage"
  rcase "an empty reply of every kind" UNKNOWN "" "" ""

  pf "{\"returnValue\": true, \"appId\": \"youtube.leanback.v4\"}" "$vol_ok" "$pow_ok"
  pfcheck "IN USE: one marker line, at column 0, naming the app, saying to stop" \
    "$([ "$(printf '%s\n' "$out" | grep -c '^IN USE: ')" = 1 ] \
       && printf '%s\n' "$out" | grep -q '^IN USE: youtube.leanback.v4: stop and tell the owner' && echo 1)"
  pfcheck "IN USE: is not also called IDLE or UNKNOWN" \
    "$(printf '%s\n' "$out" | grep -qE 'IDLE|UNKNOWN' || echo 1)"
  pfcheck "the sound state is reported, and does not change the verdict" \
    "$(printf '%s\n' "$out" | grep -q 'sound: muted=true volume=12' && echo 1)"
  pf "{\"returnValue\": true, \"appId\": \"$sid\"}" "$vol_on" "$pow_ok"
  pfcheck "our stable app with the sound on: the marker names the app and the sound" \
    "$(printf '%s\n' "$out" | grep -q "^IN USE: $sid, sound is on: stop and tell the owner" && echo 1)"
  pf "{\"returnValue\": true, \"appId\": \"com.webos.app.home\"}" "$vol_on" "$pow_ok"
  pfcheck "Home: an info line IDLE (...), no IN USE: marker, even with the sound on" \
    "$(printf '%s\n' "$out" | grep -q '^  · IDLE (LG Home launcher)' && ! printf '%s\n' "$out" | grep -q '^IN USE:' && echo 1)"
  pf "{
    \"returnValue\": true,
    \"appId\": \"$did\"
}" "" ""
  pfcheck "a pretty-printed multi-line reply parses, and an unread volume/power prints no sound line" \
    "$(printf '%s\n' "$out" | grep -q 'IDLE (this project.s own app' && ! printf '%s\n' "$out" | grep -q 'sound:' && echo 1)"
  pf "luna-send: not found" "" "garbage"
  pfcheck "an unparseable reply is UNKNOWN and says so, never IDLE, never the marker" \
    "$(printf '%s\n' "$out" | grep -q 'UNKNOWN (the foreground app could not be read)' \
       && ! printf '%s\n' "$out" | grep -qE 'IDLE|^IN USE:' && echo 1)"
  out="$(preflight_report "" "$sid" "$did" "$park" 2>&1)"
  pfcheck "no probe output at all (ssh failed) is UNKNOWN" \
    "$(printf '%s\n' "$out" | grep -q 'UNKNOWN' && ! printf '%s\n' "$out" | grep -qE 'IDLE|^IN USE:' && echo 1)"
  pf "{\"returnValue\": true, \"appId\": \"com.webos.app.home\"}" "$vol_ok" "$pow_ok" "4242 NONE "
  pfcheck "a running app is still noted, as before" \
    "$(printf '%s\n' "$out" | grep -q 'note: an app is already running on the set (stable/debug: 4242 NONE )' && echo 1)"
  out="$(preflight_note 2>&1)"
  pfcheck "preflight_note stays off the network under PLX_TV_LOCK_LOCAL (the selftest has no television)" \
    "$([ -z "$out" ] && echo 1)"

  # THE PRE-FLIGHT HAS A TIME LIMIT. A luna service that never answered used to hang `acquire`,
  # `status` and tests/run.py behind a 45-minute lease. ssh_tv is replaced here by something that never
  # answers; "nothing left behind" is read from the process list by a marker only these cases put in
  # an argv (never a global count: other lanes run their own sleeps).
  local mk mkpat wsecs rsecs t0 el sp hung
  mk=$((200000 + RANDOM)); wsecs=$((900000 + RANDOM))
  mkpat="sleep $mk|plxmark-$mk|plxmark-outer-$mk"        # the argv strings these cases plant, not a bare number
  left() { pgrep -f "$1" 2>/dev/null | tr '\n' ' '; }          # the pids whose argv holds $1
  waitfor() {  # waitfor <tries> <command…>: poll, 0.2 s apart, until the command succeeds
    local k=0 n="$1"; shift
    until "$@"; do k=$((k+1)); [ "$k" -ge "$n" ] && return 1; sleep 0.2; done
  }
  absent() { [ -z "$(left "$1")" ]; }
  present() { [ -n "$(left "$1")" ]; }
  dead() { ! kill -0 "$1" 2>/dev/null; }
  gone() { waitfor 15 absent "$1"; }                          # allowing a killed process a moment to be reaped
  guarded() {  # guarded <tries> <outfile> <command…>: run it with a hard stop of <tries> x 0.2 s; status 99 = it had to be killed
    local n="$1" f="$2" gp; shift 2
    ( "$@" ) > "$f" 2>&1 & gp=$!
    if waitfor "$n" dead "$gp"; then wait "$gp" 2>/dev/null; return $?; fi
    kill -9 "$gp" 2>/dev/null; pkill -f "$mkpat" 2>/dev/null; wait "$gp" 2>/dev/null; return 99
  }
  got="$(run_bounded "$wsecs" printf 'hello')"; rc=$?
  chk "run_bounded: a command that finishes passes its stdout and status through" \
    "$([ "$got" = hello ] && [ "$rc" = 0 ] && echo 1)" "out='$got' rc=$rc"
  chk "run_bounded: ... and its watchdog sleep is not left behind" "$(gone "sleep $wsecs" && echo 1)" "$(left "sleep $wsecs")"
  got="$(run_bounded "$wsecs" sh -c 'echo partial; exit 7')"; rc=$?
  chk "run_bounded: a non-zero status comes back as it was" "$([ "$got" = partial ] && [ "$rc" = 7 ] && echo 1)" "out='$got' rc=$rc"
  t0=$SECONDS
  guarded 40 "$d/deadline.out" run_bounded 1 sh -c "sh -c 'sleep $mk; :' plxmark-$mk; :" plxmark-outer-$mk; rc=$?
  el=$((SECONDS - t0))
  chk "run_bounded: a command that never answers is cut at the deadline, status 124, in a few seconds" \
    "$([ "$rc" = 124 ] && [ "$el" -le 5 ] && echo 1)" "rc=$rc (99 = still running after 8 s) after ${el}s"
  chk "run_bounded: ... with the command AND its children and grandchildren gone" "$(gone "$mkpat" && echo 1)" "$(left "$mkpat")"
  for sig in TERM HUP; do
    ( run_bounded "$wsecs" sh -c "sh -c 'sleep $mk; :' plxmark-$mk; :" plxmark-outer-$mk ) >/dev/null 2>&1 &
    sp=$!
    chk "run_bounded: (setup, $sig) the bounded command is running" "$(waitfor 25 present "$mkpat" && echo 1)"
    kill "-$sig" "$sp" 2>/dev/null; wait "$sp" 2>/dev/null
    chk "run_bounded: $sig to the caller leaves no command, child or watchdog sleep" \
      "$(gone "$mkpat" && gone "sleep $wsecs" && echo 1)" "$(left "$mkpat") $(left "sleep $wsecs")"
  done
  hang_pf() {   # runs in guarded's subshell: ssh_tv is replaced there, and only there
    unset PLX_TV_LOCK_LOCAL
    ssh_tv() { echo "${SSH_TV_OPTS[*]-}" > "$d/ssh-opts"; sh -c "sleep $mk; :" plxmark-$mk; }
    PLX_TV_PREFLIGHT_SECS=1 preflight_note; echo "rc=$?"
  }
  t0=$SECONDS; guarded 40 "$d/hang.out" hang_pf; hung=$?; el=$((SECONDS - t0))
  out="$(cat "$d/hang.out")"
  pfcheck "a hung pre-flight (PLX_TV_PREFLIGHT_SECS=1) returns within a few seconds" \
    "$([ "$hung" != 99 ] && [ "$el" -le 5 ] && echo 1)"
  pfcheck "... says UNKNOWN and that the read timed out, never IDLE, never the marker" \
    "$(printf '%s\n' "$out" | grep -q 'UNKNOWN' && printf '%s\n' "$out" | grep -q 'timed out' \
       && ! printf '%s\n' "$out" | grep -qE 'IDLE|^IN USE:' && echo 1)"
  pfcheck "... and preflight_note itself still returns 0 (acquire and status keep their exit codes)" \
    "$(printf '%s\n' "$out" | grep -qx 'rc=0' && echo 1)"
  chk "... leaving none of its processes behind" "$(gone "$mkpat" && echo 1)" "$(left "$mkpat")"
  pfcheck "... and asked ssh for keep-alives (ServerAliveInterval=3, ServerAliveCountMax=2)" \
    "$(grep -q 'ServerAliveInterval=3' "$d/ssh-opts" 2>/dev/null && grep -q 'ServerAliveCountMax=2' "$d/ssh-opts" && echo 1)"
  mkdir -p "$d/fakerepo/tools"; printf '#!/bin/sh\necho "$*"\n' > "$d/fakerepo/tools/tv-ssh"; chmod +x "$d/fakerepo/tools/tv-ssh"
  got="$( unset PLX_TV_LOCK_LOCAL; REPO="$d/fakerepo"; HOST=x
          SSH_TV_OPTS=(-o ServerAliveInterval=3 -o ServerAliveCountMax=2); ssh_tv true )"
  chk "ssh_tv puts a caller's ssh options before the tv placeholder, where tools/tv-ssh passes them on" \
    "$([ "$got" = "ssh -o ServerAliveInterval=3 -o ServerAliveCountMax=2 tv true" ] && echo 1)" "$got"
  got="$( unset PLX_TV_LOCK_LOCAL; REPO="$d/fakerepo"; HOST=x; ssh_tv true )"
  chk "ssh_tv with no options is the call it always was" "$([ "$got" = "ssh tv true" ] && echo 1)" "$got"

  # THE PRE-FLIGHT SPEAKS THROUGH THE REAL SUBCOMMANDS (PLX_TV_LOCK_LOCAL_PROBE stands a file in for the
  # set's reply): `acquire` keeps exit 0 and prints the IN USE: marker; `with` does not start its command
  # on IN USE; your own EXPIRED lease re-taken is pre-flighted like any new lease, a live renew is not.
  probe_file() {  # probe_file <fg reply> <volume reply> <power reply>
    printf 'NONE NONE \n1\n@@LUNA fg\r\n%s\r\n@@LUNA vol\r\n%s\r\n@@LUNA power\r\n%s\r\n' "$1" "$2" "$3" > "$d/probe.txt"
  }
  inuse_n() { printf '%s\n' "$out" | grep -c '^IN USE: '; }
  hu A release; hu B release
  probe_file "$(fgj youtube.leanback.v4)" "$vol_on" "$pow_ok"
  export PLX_TV_LOCK_LOCAL_PROBE="$d/probe.txt"
  hu A acquire --why "in use" --ttl 7
  chk "acquire on an IN USE set keeps exit 0, one IN USE: marker, one HEADS-UP, the marker first" \
    "$([ "$rc" = 0 ] && [ "$(inuse_n)" = 1 ] && [ "$(printf '%s\n' "$out" | grep -c '^HEADS-UP: ')" = 1 ] \
       && [ "$(printf '%s\n' "$out" | grep -n '^IN USE: ' | cut -d: -f1)" -lt "$(printf '%s\n' "$out" | grep -n '^HEADS-UP: ' | cut -d: -f1)" ] && echo 1)" \
    "rc=$rc: $out"
  hu A release
  marker="$d/child-ran"; rm -f "$marker"
  hu W with --why "in use" -- sh -c ": > '$marker'"
  chk "with: the pre-flight said IN USE, so the command is NOT started" "$([ ! -e "$marker" ] && echo 1)" "rc=$rc"
  chk "with: ... it exits 3 and says to tell the owner and how to proceed (PLX_TV_IN_USE_OK=1)" \
    "$([ "$rc" = 3 ] && [ "$(inuse_n)" = 1 ] && printf '%s\n' "$out" | grep -q 'PLX_TV_IN_USE_OK=1' && echo 1)" "rc=$rc: $out"
  chk "with: ... and the lease it just took is released" "$([ ! -e "$d/headsup.lock" ] && echo 1)"
  PLX_TV_IN_USE_OK=1 hu W with --why "owner said go" -- sh -c ": > '$marker'; exit 5"
  chk "with PLX_TV_IN_USE_OK=1: the command runs, its status is returned, the lease is released after" \
    "$([ -e "$marker" ] && [ "$rc" = 5 ] && [ ! -e "$d/headsup.lock" ] && echo 1)" "rc=$rc"
  probe_file "$(fgj com.webos.app.home)" "$vol_on" "$pow_ok"; rm -f "$marker"
  hu W with --why "idle set" -- sh -c ": > '$marker'; exit 6"
  chk "with on an idle set: the command runs and its status is returned" \
    "$([ -e "$marker" ] && [ "$rc" = 6 ] && [ "$(inuse_n)" = 0 ] && echo 1)" "rc=$rc: $out"
  # A lease this lane ALREADY holds is re-read too: it was pre-flighted when it was taken, maybe hours
  # ago, and the screen may have changed since. `with` did not take it, so on IN USE it refuses (exit 3)
  # and leaves it held; with the override, or on an idle set, it runs the command and still leaves it held.
  held_by_w() { hu W status; printf '%s\n' "$out" | grep -q 'HELD BY THIS LANE' && [ -e "$d/headsup.lock" ]; }
  hu W acquire --why "held for hours" --ttl 7
  probe_file "$(fgj youtube.leanback.v4)" "$vol_on" "$pow_ok"; rm -f "$marker"
  hu W with --why "inherited, in use" -- sh -c ": > '$marker'"
  chk "with under a lease this lane holds: the set is IN USE now, so the command is NOT started" \
    "$([ ! -e "$marker" ] && echo 1)" "rc=$rc"
  chk "with (inherited): ... it exits 3, prints the IN USE: line once, says the lease is still held and how to proceed" \
    "$([ "$rc" = 3 ] && [ "$(inuse_n)" = 1 ] && printf '%s\n' "$out" | grep -q 'still held' \
       && printf '%s\n' "$out" | grep -q 'PLX_TV_IN_USE_OK=1' && echo 1)" "rc=$rc: $out"
  chk "with (inherited): ... and the lease it did not take is still held by this lane" "$(held_by_w && echo 1)" "$out"
  rm -f "$marker"
  PLX_TV_IN_USE_OK=1 hu W with --why "owner said go" -- sh -c ": > '$marker'; exit 5"
  chk "with (inherited) under PLX_TV_IN_USE_OK=1: the command runs, its status comes back, the lease stays held" \
    "$([ -e "$marker" ] && [ "$rc" = 5 ] && [ -e "$d/headsup.lock" ] && echo 1)" "rc=$rc: $out"
  probe_file "$(fgj com.webos.app.home)" "$vol_on" "$pow_ok"; rm -f "$marker"
  hu W with --why "inherited, idle" -- sh -c ": > '$marker'; exit 6"
  chk "with (inherited) on an idle set: the command runs, its status comes back, no IN USE: line, the lease stays held" \
    "$([ -e "$marker" ] && [ "$rc" = 6 ] && [ "$(inuse_n)" = 0 ] && held_by_w && echo 1)" "rc=$rc: $out"
  hu W release
  rsecs=$((800000 + RANDOM))
  export PLX_TV_RENEW_SECS="$rsecs"
  hu W with --why "renewer" -- sh -c "sleep 1"
  unset PLX_TV_RENEW_SECS
  chk "with: the renewer's sleep does not outlive the command (its whole tree is killed)" \
    "$(gone "sleep $rsecs" && echo 1)" "rc=$rc left: $(left "sleep $rsecs")"
  probe_file "$(fgj youtube.leanback.v4)" "$vol_on" "$pow_ok"
  hu D acquire --why "own lease" --ttl 7
  hu D acquire --why "shorten" --ttl -1
  hu D acquire --why "back after expiry" --ttl 7
  chk "re-taking your own EXPIRED lease runs the pre-flight (one IN USE: marker) and still prints one HEADS-UP" \
    "$([ "$rc" = 0 ] && [ "$(inuse_n)" = 1 ] && [ "$(printf '%s\n' "$out" | grep -c '^HEADS-UP: ')" = 1 ] && echo 1)" "rc=$rc: $out"
  hu D acquire --why "live again" --ttl 7
  chk "a LIVE renew stays silent: no pre-flight, no HEADS-UP" \
    "$([ "$rc" = 0 ] && [ "$(inuse_n)" = 0 ] && [ "$(printf '%s\n' "$out" | grep -c '^HEADS-UP: ')" = 0 ] && echo 1)" "rc=$rc: $out"
  hu D release
  unset PLX_TV_LOCK_LOCAL_PROBE
  hu A acquire --why "holder" --ttl 7
  hu B acquire --why "refused"
  chk "the refusal hint says break shows the holder and break --yes takes it" \
    "$([ "$rc" = 1 ] && printf '%s\n' "$out" | grep -q 'break --yes' && echo 1)" "$out"
  hu A release
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
