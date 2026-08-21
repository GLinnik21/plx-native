#!/usr/bin/env bash
#
# crash-report.sh — collect and symbolize the app's last on-device crash.
#
#   crash-report.sh              collect evidence + symbolize the most recent crash
#   crash-report.sh --all        symbolize every crash in the persistent log
#   crash-report.sh --collect    evidence bundle only (no symbolization)
#
#   --flavor <f>                 which INSTALL to triage: debug (default) | stable. Two builds
#                                live on the one television with their own app ids and their own
#                                runtime roots, so they have their own crash logs; this picks one.
#
# The C tracer (src/main.c) writes, per crash:
#     *** SIGNAL <n> (<name>) addr=0x… pc=0x… lr=0x…
#     at:  <the /proc/self/maps line containing pc or lr>     (which library faulted)
#     bin: <the maps line for our own binary>                 (our load base)
# and then re-raises to SIG_DFL so the OS crash daemon still captures a real backtrace.
# Turning pc into a source line needs `pc - load_base` fed to addr2line — this script is
# the only thing in the repo that does that arithmetic.
#
# TWO LOGS, and the difference matters after a relaunch:
#   <runtime root>/plxnative-crash.log   append-only, SURVIVES the relaunch  <- read this one
#   <runtime root>/plxnative-events.log  truncated at every launch           <- already gone
# The NAMES are the same for both installs; only the root differs — `/tmp` for the stable
# install, `/tmp/<app id>` for a flavoured one. Ask for the one you mean with
# `make -s print-rundir FLAVOR=<f>`; this script derives CRASHLOG/STDERRLOG from it below.
# Do not carry an absolute path out of here into a by-hand `cat`: at the default flavour
# `/tmp/plxnative-crash.log` is the OTHER install's log, it exists, it is append-only, and it
# will hand you a perfectly plausible crash that has nothing to do with the build you are
# triaging.
#
# Config: TV host, app id, install directory and runtime root all come from `make -s print-…`.
# Nothing about the network or any credential is stored here.
# See .claude/skills/crash-triage/SKILL.md.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# --flavor is stripped out of the argv before the mode is read, so it can go either side of
# --all/--collect. Every path below depends on it: the crash log, the stderr tail, the app
# directory the `bin:` line has to match, and which app id SAM's exit status is filtered to.
FLAVOR=""
_argv=()
while [ $# -gt 0 ]; do
  case "$1" in
    --flavor)   FLAVOR="${2:-}"; shift 2 ;;
    --flavor=*) FLAVOR="${1#*=}"; shift ;;
    *)          _argv+=("$1"); shift ;;
  esac
done
# bash 3.2 + `set -u`: "${arr[@]}" on an EMPTY array is an unbound-variable error, and the
# no-argument invocation (the common one) is exactly that case.
set -- ${_argv[@]+"${_argv[@]}"}

# WHICH INSTALL — asked for, never restated. The app directory used to be a literal here, which
# was one install's path hardcoded into a tool that now has two to choose between. An unknown
# FLAVOR is a parse-time $(error) in the Makefile, so a typo stops here.
: "${FLAVOR:=$(make -s -C "$REPO" print-flavor)}"
{ read -r FLAVOR; read -r APPID; read -r APPDIR; read -r RUNDIR; } < <(
  make -s -C "$REPO" FLAVOR="$FLAVOR" print-flavor print-appid print-appdir print-rundir
)
# On a bad flavour make has already printed the exact complaint; do not restate it wrongly —
# the failed `read` above left FLAVOR empty, so echoing it back here would name nothing.
[ -n "${RUNDIR:-}" ] || { echo "cannot resolve the flavour above from $REPO/Makefile" >&2; exit 2; }
# These two have no `print-` target of their own, and want none: the log NAMES are unchanged
# across flavours — only the directory they sit in moved — so the runtime root is the whole story.
CRASHLOG="$RUNDIR/plxnative-crash.log"
STDERRLOG="$RUNDIR/plxnative-stderr.log"

: "${WEBOS_SDK:=$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot}"
ADDR2LINE="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-addr2line"
READELF="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-readelf"
OBJDUMP="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-objdump"
BIN="$REPO/pkg/plxnative"

tv_host() {
  [ -n "${TV:-}" ] && { echo "$TV"; return; }
  # `print-tv` is a real recipe echoing the EXPANDED value. This used to be `make -pn`, which was
  # broken on every checkout that keeps the address in `.tv-host`: `-p` prints a recursive
  # variable's UNEXPANDED DEFINITION, so HOST became the literal string
  # `$(strip $(shell cat .tv-host 2>/dev/null))`, every ssh failed on an invalid hostname, and this
  # script — the one you reach for immediately after a crash — reported the television unreachable
  # while it sat there answering. Same trap tools/tv-session.sh documents; this was its twin.
  make -s -C "$REPO" print-tv 2>/dev/null
}
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=8)
HOST="$(tv_host)"
tv() { ssh "${SSH_OPTS[@]}" "root@$HOST" "$@" 2>/dev/null; }

mode="${1:-last}"

hr() { printf '%s\n' "------------------------------------------------------------"; }

# ---- 0. is the TV even up? a sleeping TV mimics every failure ----------------
echo "== triaging $APPID [$FLAVOR] on ${HOST:-<no TV address>}"
if ! tv true; then
  echo "TV ${HOST:-<no TV address>} is unreachable — wake it first (.claude/skills/wake-tv/wake-tv.sh)."
  echo "NOTE: a sleeping TV makes every log assertion fail as 'no line found'; that is"
  echo "      not a crash. Re-run whatever failed after waking before triaging."
  exit 2
fi

# ---- 1. is the deployed binary the one we can symbolize against? -------------
echo "== binary identity"
if [ -f "$BIN" ]; then
  local_md5=$(md5 -q "$BIN" 2>/dev/null || md5sum "$BIN" | cut -d' ' -f1)
  tv_md5=$(tv "md5sum $APPDIR/plxnative" | cut -d' ' -f1)
  echo "  local $local_md5"
  echo "  on-TV $tv_md5   ($APPDIR/plxnative)"
  if [ "$local_md5" != "$tv_md5" ]; then
    echo "  *** MISMATCH — addresses below CANNOT be symbolized against this local build."
    echo "      Redeploy (make FLAVOR=$FLAVOR deploy) and reproduce, or fetch the deployed binary."
  else
    echo "  match — symbolization is valid"
  fi
else
  echo "  no local pkg/plxnative — run make first"
fi

# ---- 2. codegen sanity (the SIGILL branch) ----------------------------------
# The legacy ARMv6 CP15 memory barriers — `mcr p15, 0, Rd, c7, c10, {4,5}` (DSB/DMB) and
# `c7, c5, {4}` (ISB) — are UNDEFINED on this TV's ARMv8 A53 and SIGILL on the first one
# executed. The Makefile's `-C target-cpu=cortex-a9` (and the unpinned -mcpu on the C side)
# exist solely to make the compilers emit the dedicated `dmb`/`isb` instead; see the long
# note in the Makefile. This count is the ONLY automated guard on that, so it must never
# answer a quiet "0" when it did not actually look — hence the loud CANNOT CHECK branches.
#
# The pattern must match objdump's REAL spelling. GNU objdump prints the coprocessor
# registers as `cr7, cr10` and braces the opc2:
#     ee070fba 	mcr	15, 0, r0, cr7, cr10, {5}
# This used to grep for `c7, c10`, which the `r` in `cr7` makes unmatchable — so for the
# whole life of the check it reported "0 found" on every build, present bug or not
# (verified 2026-07-29 by assembling the three barriers and disassembling them). `cr?`
# also accepts the bare-`c7` spelling other binutils versions print.
CP15_RE='mcr[[:space:]].*[[:space:]]cr?7, (cr?10|cr?5, [{]4[}])'
echo "== codegen sanity (the SIGILL branch)"
if [ ! -f "$BIN" ]; then
  echo "  *** CANNOT CHECK — no local pkg/plxnative. Run make, then re-run this. ***"
elif [ ! -x "$OBJDUMP" ]; then
  echo "  *** CANNOT CHECK — no NDK objdump at $OBJDUMP (see the setup-environment skill). ***"
else
  dis="$("$OBJDUMP" -d "$BIN" 2>/dev/null)"
  if [ -z "$dis" ]; then
    echo "  *** CANNOT CHECK — objdump produced no disassembly for $BIN. ***"
  else
    cp15=$(printf '%s\n' "$dis" | grep -cE "$CP15_RE" || true)
    arch=$("$READELF" -A "$BIN" 2>/dev/null | sed -n 's/.*Tag_CPU_arch: *//p' | head -1)
    echo "  CPU arch tag: ${arch:-?}"
    if [ "${cp15:-0}" -eq 0 ]; then
      echo "  ARMv6 CP15 barriers: 0 — clean (this build cannot SIGILL on that)"
    else
      echo "  *** ARMv6 CP15 barriers: $cp15 — this binary WILL SIGILL on the A53. ***"
      echo "      Something dropped -C target-cpu=cortex-a9 (RUST_ENV) or pinned an ARMv6 -mcpu."
      printf '%s\n' "$dis" | grep -E "$CP15_RE" | head -3 | sed 's/^/      /'
    fi
  fi
fi

hr
# ---- 3. the crash log (append-only; survives the relaunch) -------------------
echo "== $CRASHLOG"
log="$(tv "cat $CRASHLOG 2>/dev/null")"
if [ -z "$log" ]; then
  echo "  empty or absent — no traced crash since the log was last cleared."
  echo "  If the app vanished anyway, it was not a caught signal: check the SAM exit status"
  echo "  and $STDERRLOG (a Rust panic aborts via SIGABRT and prints there)."
  echo "  NB this log is per-install: a crash in the other flavour is in ITS runtime root."
else
  if [ "$mode" = "--all" ]; then echo "$log"; else echo "$log" | awk '/\*\*\* SIGNAL/{buf=""} {buf=buf $0 "\n"} END{printf "%s", buf}'; fi
fi

[ "$mode" = "--collect" ] && { hr; echo "== stderr tail"; tv "tail -30 $STDERRLOG 2>/dev/null"; exit 0; }

# ---- 4. symbolize ------------------------------------------------------------
hr
echo "== symbolization ($APPID [$FLAVOR], against $BIN)"
if [ -z "$log" ]; then
  echo "  (nothing to symbolize)"
else
  # take the last trace block; pull pc/lr and the load base from the bin: maps line
  block="$(echo "$log" | awk '/\*\*\* SIGNAL/{buf=""} {buf=buf $0 "\n"} END{printf "%s", buf}')"
  sig=$(echo "$block"  | sed -n 's/.*\*\*\* SIGNAL \([0-9]*\) (\([A-Z]*\)).*/\1 \2/p' | head -1)
  pc=$(echo "$block"   | sed -n 's/.*pc=0x\([0-9a-f]*\).*/\1/p' | head -1)
  lr=$(echo "$block"   | sed -n 's/.*lr=0x\([0-9a-f]*\).*/\1/p' | head -1)
  at=$(echo "$block"   | sed -n 's/^at: *//p' | head -1)
  # WHICH `bin:` line. The tracer emits one per maps line naming our executable, and that path
  # carries the app directory — so the match has to be ANCHORED on `/<id>/`. A bare substring test
  # would not do: `com.beb.plxnative` is a PREFIX of `com.beb.plxnative.debug`, so triaging the
  # stable install would happily accept the debug install's load base and hand addr2line an offset
  # into the wrong binary — which does not fail, it answers with a confident wrong function.
  # (src/main.c's tracer documents the same trap for the /plxnative name itself.)
  binline=$(echo "$block" | grep '^bin: ' | grep -F "/$APPID/" | tail -1)
  if [ -z "$binline" ] && echo "$block" | grep -q '^bin: '; then
    echo "  *** this crash's bin: line does not name $APPID:"
    echo "$block" | grep '^bin: ' | tail -1 | sed 's/^/      /'
    echo "      Symbolizing it against $BIN would be meaningless. Re-run with the --flavor that"
    echo "      wrote it, or clear the log if it is a leftover from an earlier install."
  fi
  base=$(echo "$binline" | sed -n 's/^bin: *\([0-9a-f]*\)-.*/\1/p' | head -1)
  # the executable's text mapping bounds what we can symbolize
  top=$(echo "$binline"  | sed -n 's/^bin: *[0-9a-f]*-\([0-9a-f]*\).*/\1/p' | head -1)
  echo "  signal : ${sig:-?}"
  echo "  faulted in: ${at:-<no maps line — pc outside every mapping>}"

  if echo "$at" | grep -q '\.so'; then
    echo "  -> the PC is inside a TV shared library, not our binary. There are no symbols"
    echo "     for it: treat this as a bad argument / wrong struct offset on the call path"
    echo "     into that library (see the bind-tv-lib-abi skill), not a bug in their code."
  fi
  if [ -n "$base" ] && [ -n "$pc" ] && [ -f "$BIN" ] && [ -x "$ADDR2LINE" ]; then
    for reg in pc lr; do
      val=$([ "$reg" = pc ] && echo "$pc" || echo "$lr")
      [ -n "$val" ] || continue
      # addr2line is only meaningful for an address inside OUR text mapping; anything
      # else (libc, a TV .so, lr=0) would silently produce a confident "?? ??:0"
      if ! python3 -c "import sys; a=int('$val',16); sys.exit(0 if int('$base',16)<=a<int('${top:-0}',16) else 1)" 2>/dev/null; then
        echo "  $reg 0x$val -> outside our binary (see the 'faulted in' line above)"
        continue
      fi
      off=$(python3 -c "print(hex(int('$val',16) - int('$base',16)))")
      line=$("$ADDR2LINE" -f -C -p -e "$BIN" "$off" 2>/dev/null)
      echo "  $reg 0x$val  (base 0x$base, offset $off) -> $line"
    done
  else
    echo "  (cannot symbolize: need a bin: maps line for $APPID, pkg/plxnative and the NDK addr2line)"
  fi

  case "${sig%% *}" in
    4)  echo "  SIGILL: suspect codegen, not logic — check the CP15 count above and rebuild." ;;
    6)  echo "  SIGABRT: usually a Rust panic crossing the FFI boundary. The PC is inside"
        echo "           abort() and is worthless — read $STDERRLOG instead." ;;
    11|7) echo "  SIGSEGV/SIGBUS: bad pointer or a wrong struct offset — the symbolized line"
        echo "           above is the site; verify every offset on that path." ;;
  esac
fi

# ---- 5. corroborating evidence ----------------------------------------------
hr
echo "== stderr tail (Rust panics land here: $STDERRLOG)"
tv "tail -20 $STDERRLOG 2>/dev/null" || echo "  (none)"
hr
echo "== SAM exit status for $APPID (768 = exit(3); a signal death shows WIFSIGNALED in the low byte)"
# Filtered to this install, and anchored on the RIGHT: /var/log/messages carries both apps' lines,
# and `com.beb.plxnative` is a prefix of `com.beb.plxnative.debug`, so a plain grep for the stable
# id also returns the debug id's deaths — the other install's crash reported as this one's.
# The dots are escaped because they are regex metacharacters, not separators, to grep.
APPID_RE=$(printf '%s' "$APPID" | sed 's/\./\\./g')
sam=$(tv "grep -a exit_status /var/log/messages 2>/dev/null | grep -aE '$APPID_RE([^.a-zA-Z0-9]|\$)' | tail -5")
[ -n "$sam" ] && printf '%s\n' "$sam" || echo "  (none naming $APPID)"
hr
echo "== crash daemon reports (real backtraces, thanks to the re-raise)"
tv 'ls -lt /var/log/reports/librdx/ 2>/dev/null | head -5' || echo "  (none)"
echo
echo "Correlate by monotonic uptime, never wall clock — pmlog's clock is hours off on this TV."
echo "Everything above is $APPID [$FLAVOR]; pass --flavor to triage the other install."
