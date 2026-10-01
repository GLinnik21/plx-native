#!/usr/bin/env bash
#
# tv-sched-trace.sh -- capture a kernel scheduler/IRQ trace from the television, then put it back.
#
#   tools/tv-sched-trace.sh [--secs 20] [--out FILE.gz]
#
# Runs on the HOST and drives the set through tools/tv-ssh (root, busybox sh). It takes the
# television's lock like every other device tool (tools/tv-lock.sh), so run it inside your own
# session. Start it, then do the thing under test (open the menu, scrub, ...) while it records.
#
# What it does on the set (kernel 4.4.84 aarch64: tracepoints and event tracing, NO function tracer,
# NO SCHEDSTATS, no perf, no trace-cmd; tracefs is supported but not mounted):
#   1. mounts tracefs at /sys/kernel/tracing only if it is not already mounted;
#   2. sets trace_clock=mono (so event times are the app's CLOCK_MONOTONIC, the clock FRAMEDROP's
#      `mono=`/`wait_at=` use) and a per-CPU buffer sized for the duration;
#   3. enables sched/sched_switch, sched/sched_wakeup, irq/irq_handler_entry, irq/irq_handler_exit
#      and every event category whose name matches mali|kbase|gpu;
#   4. tracing_on=1, sleeps --secs, tracing_on=0;
#   5. streams the `trace` text back, gzipped on the set when it has gzip, else on the host.
# Whatever happens after step 1 begins (error, ^C), a trap restores it: events off, the previous
# buffer size, clock and tracing_on, the buffer cleared, and tracefs unmounted if THIS run mounted
# it. If somebody else already has events enabled the run refuses rather than disturb them.
#
# Read the result with tools/analyze-sched-trace.py. The vsync interrupt is named `osd_irq` here.
# Nothing identifying is printed: tools/tv-ssh keeps the address out of every line.
set -uo pipefail

T=/sys/kernel/tracing
secs=20
out=""

usage() { sed -n '3,4p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --secs) [ $# -ge 2 ] || usage; secs="$2"; shift 2 ;;
    --secs=*) secs="${1#*=}"; shift ;;
    --out) [ $# -ge 2 ] || usage; out="$2"; shift 2 ;;
    --out=*) out="${1#*=}"; shift ;;
    -h|--help) usage ;;
    *) echo "tv-sched-trace: unknown argument: $1" >&2; usage ;;
  esac
done
case "$secs" in ''|*[!0-9]*) echo "tv-sched-trace: --secs needs a whole number of seconds" >&2; exit 2 ;; esac
[ "$secs" -ge 1 ] && [ "$secs" -le 120 ] || { echo "tv-sched-trace: --secs must be 1..120" >&2; exit 2; }
[ -n "$out" ] || out="tv-sched-trace-$(date +%Y%m%d-%H%M%S).gz"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TVSSH="$HERE/tv-ssh"
tv() { "$TVSSH" ssh tv "$@"; }

# The television's lock, taken the way capture-screen.sh takes it (it needs the address in $TV; a
# linked worktree has no .tv-host of its own, so the Makefile is asked for the main checkout's).
TV_HOST="${TV_HOST:-$(cat "$HERE/../.tv-host" 2>/dev/null || true)}"
[ -n "$TV_HOST" ] || TV_HOST="$(make -s -C "$HERE/.." print-tv 2>/dev/null | head -1)"
[ -n "$TV_HOST" ] || { echo "tv-sched-trace: no TV configured (.tv-host or TV_HOST)" >&2; exit 1; }
TV="$TV_HOST" "$HERE/tv-lock.sh" require --quiet --why "tv-sched-trace.sh" || exit 1

# Per-CPU buffer: ~0.5 MB/s/CPU of sched+irq events, so 512 KB per second of run, within
# 2..16 MB so a long run cannot take much of the set's memory (4 CPUs).
buf_kb=$(( secs * 512 ))
[ "$buf_kb" -ge 2048 ] || buf_kb=2048
[ "$buf_kb" -le 16384 ] || buf_kb=16384

# The on-set half, one busybox-sh script run as `sh -s -- MODE ARGS...` over ssh. The event list
# lives here once so that enabling and disabling cannot drift apart.
IFS= read -r -d '' REMOTE <<'SH'
T=/sys/kernel/tracing
events() {   # events 0|1 : switch every event this tool uses on or off
  for d in $T/events/sched/sched_switch $T/events/sched/sched_wakeup \
           $T/events/irq/irq_handler_entry $T/events/irq/irq_handler_exit \
           $T/events/*mali* $T/events/*kbase* $T/events/*gpu*; do
    [ -e "$d/enable" ] && echo "$1" > "$d/enable"
    for e in "$d"/*/enable; do [ -e "$e" ] && echo "$1" > "$e"; done
  done
}
mode=$1; shift
case "$mode" in
  mount)   # mount [only if absent]; print whether this call did
    if [ -e $T/trace_clock ]; then echo mounted=0
    elif mount -t tracefs nodev $T 2>/dev/null; then echo mounted=1
    else echo err=tracefs-cannot-be-mounted; fi ;;
  setup)   # setup BUF_KB : print the previous state, then apply ours
    if [ -n "$(cat $T/set_event 2>/dev/null)" ]; then echo err=events-already-enabled; exit 0; fi
    echo "clock=$(sed -n 's/.*\[\([a-z_-]*\)\].*/\1/p' $T/trace_clock)"
    set -- "$1" $(cat $T/buffer_size_kb); echo "buf=$2"
    echo "on=$(cat $T/tracing_on)"
    echo 0 > $T/tracing_on
    echo mono > $T/trace_clock
    echo "$1" > $T/buffer_size_kb
    echo > $T/trace
    events 1
    [ -e $T/events/sched/sched_switch/enable ] || echo err=no-sched-events
    echo ready ;;
  run)     # run SECS
    echo 1 > $T/tracing_on; sleep "$1"; echo 0 > $T/tracing_on ;;
  hasgzip) command -v gzip >/dev/null 2>&1 && echo yes || echo no ;;
  restore) # restore MOUNTED CLOCK BUF ON
    echo 0 > $T/tracing_on
    events 0
    echo > $T/trace
    [ -n "$2" ] && echo "$2" > $T/trace_clock
    [ -n "$3" ] && echo "$3" > $T/buffer_size_kb
    [ -n "$4" ] && echo "$4" > $T/tracing_on
    [ "$1" = 1 ] && umount $T
    exit 0 ;;
esac
SH
remote() { printf '%s\n' "$REMOTE" | tv sh -s -- "$@"; }
val() { sed -n "s/^$1=//p" <<<"$2" | head -1; }

# ---- restore: runs on every exit once the set may have been touched --------------------------
touched=0; mounted=0; prev_clock=""; prev_buf=""; prev_on=""
restore() {
  [ "$touched" = 1 ] || return 0
  touched=0
  remote restore "$mounted" "$prev_clock" "$prev_buf" "$prev_on" >/dev/null 2>&1 \
    || echo "tv-sched-trace: WARNING: the restore did not complete; check tracefs on the set" >&2
}
trap restore EXIT
trap 'exit 130' INT TERM HUP

fail() { echo "tv-sched-trace: $1" >&2; exit 1; }

touched=1
r="$(remote mount)" || fail "the television is unreachable"
[ -z "$(val err "$r")" ] || fail "$(val err "$r")"
mounted="$(val mounted "$r")"

r="$(remote setup "$buf_kb")"
# Only values that look like values reach the restore command line.
for k in clock buf on; do
  v="$(val "$k" "$r")"
  case "$v" in *[!A-Za-z0-9_-]*) v="" ;; esac
  case "$k" in clock) prev_clock="$v" ;; buf) prev_buf="$v" ;; on) prev_on="$v" ;; esac
done
[ -z "$(val err "$r")" ] || fail "$(val err "$r")"
grep -q '^ready$' <<<"$r" || fail "setup did not complete"

echo "tv-sched-trace: recording ${secs} s (per-CPU buffer ${buf_kb} KB)..." >&2
remote run "$secs" || fail "the run was interrupted"

gz="$(remote hasgzip)"
tmp="$out.part"
if [ "$gz" = yes ]; then
  tv "cat $T/trace | gzip -1" > "$tmp" || { rm -f "$tmp"; fail "could not read the trace"; }
else
  tv "cat $T/trace" | gzip -1 > "$tmp" || { rm -f "$tmp"; fail "could not read the trace"; }
fi
mv "$tmp" "$out"
echo "tv-sched-trace: wrote $out ($(wc -c < "$out" | tr -d ' ') bytes, $(gzip -dc "$out" | grep -c '^ *[^# ]') lines)" >&2
