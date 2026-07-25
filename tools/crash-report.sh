#!/usr/bin/env bash
#
# crash-report.sh — collect and symbolize the app's last on-device crash.
#
#   crash-report.sh              collect evidence + symbolize the most recent crash
#   crash-report.sh --all        symbolize every crash in the persistent log
#   crash-report.sh --collect    evidence bundle only (no symbolization)
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
#   /tmp/plxnative-crash.log   append-only, SURVIVES the relaunch  <- read this one
#   /tmp/plxnative-events.log  truncated at every launch           <- already gone
#
# Config: TV host from $TV, else the Makefile's TV default. Nothing about the network
# or any credential is stored here. See .claude/skills/crash-triage/SKILL.md.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
: "${WEBOS_SDK:=$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot}"
ADDR2LINE="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-addr2line"
READELF="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-readelf"
OBJDUMP="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-objdump"
BIN="$REPO/pkg/plxnative"
APPDIR=/media/developer/apps/usr/palm/applications/com.beb.plxnative

tv_host() {
  [ -n "${TV:-}" ] && { echo "$TV"; return; }
  make -C "$REPO" -pn 2>/dev/null | sed -n 's/^TV *= *//p' | head -1
}
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=8)
HOST="$(tv_host)"
tv() { ssh "${SSH_OPTS[@]}" "root@$HOST" "$@" 2>/dev/null; }

mode="${1:-last}"

hr() { printf '%s\n' "------------------------------------------------------------"; }

# ---- 0. is the TV even up? a sleeping TV mimics every failure ----------------
if ! tv true; then
  echo "TV $HOST is unreachable — wake it first (.claude/skills/wake-tv/wake-tv.sh)."
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
  echo "  on-TV $tv_md5"
  if [ "$local_md5" != "$tv_md5" ]; then
    echo "  *** MISMATCH — addresses below CANNOT be symbolized against this local build."
    echo "      Redeploy (make deploy) and reproduce, or fetch the deployed binary."
  else
    echo "  match — symbolization is valid"
  fi
else
  echo "  no local pkg/plxnative — run make first"
fi

# ---- 2. codegen sanity (the SIGILL branch) ----------------------------------
if [ -f "$BIN" ] && [ -x "$OBJDUMP" ]; then
  cp15=$("$OBJDUMP" -d "$BIN" 2>/dev/null | grep -c 'mcr.*15.*c7, c10' || true)
  arch=$("$READELF" -A "$BIN" 2>/dev/null | sed -n 's/.*Tag_CPU_arch: *//p' | head -1)
  echo "  CPU arch tag: ${arch:-?}   ARMv6 CP15 barriers: $cp15 (must be 0 — nonzero SIGILLs on this SoC)"
fi

hr
# ---- 3. the crash log (append-only; survives the relaunch) -------------------
echo "== /tmp/plxnative-crash.log"
log="$(tv 'cat /tmp/plxnative-crash.log 2>/dev/null')"
if [ -z "$log" ]; then
  echo "  empty or absent — no traced crash since the log was last cleared."
  echo "  If the app vanished anyway, it was not a caught signal: check the SAM exit status"
  echo "  and /tmp/plxnative-stderr.log (a Rust panic aborts via SIGABRT and prints there)."
else
  if [ "$mode" = "--all" ]; then echo "$log"; else echo "$log" | awk '/\*\*\* SIGNAL/{buf=""} {buf=buf $0 "\n"} END{printf "%s", buf}'; fi
fi

[ "$mode" = "--collect" ] && { hr; echo "== stderr tail"; tv 'tail -30 /tmp/plxnative-stderr.log 2>/dev/null'; exit 0; }

# ---- 4. symbolize ------------------------------------------------------------
hr
echo "== symbolization"
if [ -z "$log" ]; then
  echo "  (nothing to symbolize)"
else
  # take the last trace block; pull pc/lr and the load base from the bin: maps line
  block="$(echo "$log" | awk '/\*\*\* SIGNAL/{buf=""} {buf=buf $0 "\n"} END{printf "%s", buf}')"
  sig=$(echo "$block"  | sed -n 's/.*\*\*\* SIGNAL \([0-9]*\) (\([A-Z]*\)).*/\1 \2/p' | head -1)
  pc=$(echo "$block"   | sed -n 's/.*pc=0x\([0-9a-f]*\).*/\1/p' | head -1)
  lr=$(echo "$block"   | sed -n 's/.*lr=0x\([0-9a-f]*\).*/\1/p' | head -1)
  at=$(echo "$block"   | sed -n 's/^at: *//p' | head -1)
  base=$(echo "$block" | sed -n 's/^bin: *\([0-9a-f]*\)-.*/\1/p' | head -1)
  # the executable's text mapping (first bin: line) bounds what we can symbolize
  top=$(echo "$block"  | sed -n 's/^bin: *[0-9a-f]*-\([0-9a-f]*\).*/\1/p' | head -1)
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
    echo "  (cannot symbolize: need the bin: maps line, pkg/plxnative and the NDK addr2line)"
  fi

  case "${sig%% *}" in
    4)  echo "  SIGILL: suspect codegen, not logic — check the CP15 count above and rebuild." ;;
    6)  echo "  SIGABRT: usually a Rust panic crossing the FFI boundary. The PC is inside"
        echo "           abort() and is worthless — read /tmp/plxnative-stderr.log instead." ;;
    11|7) echo "  SIGSEGV/SIGBUS: bad pointer or a wrong struct offset — the symbolized line"
        echo "           above is the site; verify every offset on that path." ;;
  esac
fi

# ---- 5. corroborating evidence ----------------------------------------------
hr
echo "== stderr tail (Rust panics land here)"
tv 'tail -20 /tmp/plxnative-stderr.log 2>/dev/null' || echo "  (none)"
hr
echo "== SAM exit status (768 = exit(3); a signal death shows WIFSIGNALED in the low byte)"
tv 'grep -a exit_status /var/log/messages 2>/dev/null | tail -5' || echo "  (none)"
hr
echo "== crash daemon reports (real backtraces, thanks to the re-raise)"
tv 'ls -lt /var/log/reports/librdx/ 2>/dev/null | head -5' || echo "  (none)"
echo
echo "Correlate by monotonic uptime, never wall clock — pmlog's clock is hours off on this TV."
