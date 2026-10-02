#!/usr/bin/env bash
#
# tv-sched-trace.sh -- capture a kernel scheduler/IRQ trace from the television, then put it back.
#
#   tools/tv-sched-trace.sh [--secs 6] [--tid N] [--read-timeout 90] [--out FILE.gz]
#
# Runs on the HOST and drives the set through tools/tv-ssh (root, busybox sh). It takes the
# television's lock like every other device tool (tools/tv-lock.sh), so run it inside your own
# session. Start it, then do the thing under test (open the menu, scrub, ...) while it records.
#
# What it does on the set (kernel 4.4.84 aarch64: tracepoints and event tracing, NO function tracer,
# NO SCHEDSTATS, no perf, no trace-cmd; tracefs is supported but not mounted):
#   1. mounts tracefs at /sys/kernel/tracing only if it is not already mounted;
#   (Observed on the television, 4.4.84: the string filter `name != "arch_timer"` is rejected, so the
#   irq-number fallback is what runs; raw_syscalls `common_pid == N` is accepted; the per-CPU `stats`
#   `entries:` line reaches 0 when drained; 433,702 events read back well inside the 90 s bound.
#   An 8 s run at the 32 MB cap overran CPU 0 only (5,644 of 166,017 events; CPU 0 carries about
#   twice the others'), so the default `--secs 6` is the loss-free choice.)
#   2. sets trace_clock=mono (so event times are the app's CLOCK_MONOTONIC, the clock FRAMEDROP's
#      `mono=`/`wait_at=` use) and a per-CPU buffer sized for ~60k events/s with headroom, capped
#      at 32 MB across all CPUs (the set has ~45 MB free): about 8 s of events on average, but CPU 0 fills first, hence the 6 s default;
#   3. enables sched/sched_switch, sched/sched_wakeup, irq/irq_handler_entry, irq/irq_handler_exit
#      (minus the arch timer, which fires on every CPU every few ms and is pure noise) and every
#      event category whose name matches mali|kbase|gpu; with --tid N also raw_syscalls
#      sys_enter/sys_exit filtered to that thread, so the analyser can name the blocking syscall;
#   4. tracing_on=1, sleeps --secs, tracing_on=0, reports the buffer's entries/overrun;
#   5. drains `trace_pipe` (the consuming reader: `trace` re-walks the ring on every read and took
#      minutes for 3 s of data), gzipped on the set when it has gzip, else on the host. The drain
#      ends when the buffer is empty, and the host stops it after --read-timeout seconds whatever
#      happens (killing the reader on the set so its gzip still finishes cleanly): the file is then a VALID but PARTIAL gzip, stderr says how many seconds it covers,
#      and the exit status is 3. ^C during the read-back does the same (exit 130).
# Whatever happens after step 1 begins (error, ^C), a trap restores it: events off, filters
# cleared, any reader left on the set killed, the previous buffer size, clock and tracing_on, the
# buffer cleared, and tracefs unmounted if THIS run mounted it. If somebody else already has events
# enabled the run refuses rather than disturb them.
#
# Read the result with tools/analyze-sched-trace.py. The vsync interrupt is named `osd_irq` here.
# Nothing identifying is printed: tools/tv-ssh keeps the address out of every line.
#
# Test seam (tools/test_tv_sched_trace.py only): with TV_SCHED_TRACE_TEST=1, TV_SCHED_TRACE_TVSSH is
# the tv-ssh stand-in, TV_SCHED_TRACE_T the tracefs directory and TV_SCHED_TRACE_PIDF the reader
# pid file, and the lock is not taken.
set -uo pipefail

T=/sys/kernel/tracing
PIDF=/tmp/tv-sched-trace.pids
secs=6
tid=""
read_timeout=90
out=""
TEST="${TV_SCHED_TRACE_TEST:-0}"
if [ "$TEST" = 1 ]; then
  T="${TV_SCHED_TRACE_T:?}"; PIDF="${TV_SCHED_TRACE_PIDF:?}"
fi

usage() { sed -n '3p;5p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --secs) [ $# -ge 2 ] || usage; secs="$2"; shift 2 ;;
    --secs=*) secs="${1#*=}"; shift ;;
    --tid) [ $# -ge 2 ] || usage; tid="$2"; shift 2 ;;
    --tid=*) tid="${1#*=}"; shift ;;
    --read-timeout) [ $# -ge 2 ] || usage; read_timeout="$2"; shift 2 ;;
    --read-timeout=*) read_timeout="${1#*=}"; shift ;;
    --out) [ $# -ge 2 ] || usage; out="$2"; shift 2 ;;
    --out=*) out="${1#*=}"; shift ;;
    -h|--help) usage ;;
    *) echo "tv-sched-trace: unknown argument: $1" >&2; usage ;;
  esac
done
case "$secs" in ''|*[!0-9]*) echo "tv-sched-trace: --secs needs a whole number of seconds" >&2; exit 2 ;; esac
[ "$secs" -ge 1 ] && [ "$secs" -le 120 ] || { echo "tv-sched-trace: --secs must be 1..120" >&2; exit 2; }
case "$read_timeout" in ''|*[!0-9]*) echo "tv-sched-trace: --read-timeout needs a whole number of seconds" >&2; exit 2 ;; esac
[ "$read_timeout" -ge 1 ] || { echo "tv-sched-trace: --read-timeout must be >= 1" >&2; exit 2; }
case "$tid" in *[!0-9]*) echo "tv-sched-trace: --tid needs a thread id" >&2; exit 2 ;; esac
[ -n "$out" ] || out="tv-sched-trace-$(date +%Y%m%d-%H%M%S).gz"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TVSSH="$HERE/tv-ssh"
[ "$TEST" = 1 ] && TVSSH="${TV_SCHED_TRACE_TVSSH:?}"
tv() { "$TVSSH" ssh tv "$@"; }

if [ "$TEST" != 1 ]; then
  # The television's lock, taken the way capture-screen.sh takes it (it needs the address in $TV; a
  # linked worktree has no .tv-host of its own, so the Makefile is asked for the main checkout's).
  TV_HOST="${TV_HOST:-$(cat "$HERE/../.tv-host" 2>/dev/null || true)}"
  [ -n "$TV_HOST" ] || TV_HOST="$(make -s -C "$HERE/.." print-tv 2>/dev/null | head -1)"
  [ -n "$TV_HOST" ] || { echo "tv-sched-trace: no TV configured (.tv-host or TV_HOST)" >&2; exit 1; }
  TV="$TV_HOST" "$HERE/tv-lock.sh" require --quiet --why "tv-sched-trace.sh" || exit 1
fi

# Ring sizing. ~60k events/s across all CPUs measured (about 66k with the arch timer, now filtered
# out), ~64 bytes an event in the ring, +25% headroom for an uneven CPU split. Total ring memory is
# capped at 32 MB (the set has ~45 MB free); buffer_size_kb is PER CPU, so the set divides by its
# CPU count. Past the cap the run still works but the ring covers only cap_secs seconds.
ev_per_s=60000
ev_bytes=64
want_kb=$(( secs * ev_per_s * ev_bytes * 5 / 4 / 1024 ))
cap_kb=32768
cap_secs=$(( cap_kb * 1024 / (ev_per_s * ev_bytes) ))

# The on-set half, one busybox-sh script run as `sh -s -- MODE ARGS...` over ssh, preceded by the
# host's `T=` and `PIDF=` lines. The event list lives here once so that enabling and disabling
# cannot drift apart.
IFS= read -r -d '' REMOTE <<'SH'
events() {   # events 0|1 : switch every event this tool uses on or off (and clear filters on off)
  for d in $T/events/sched/sched_switch $T/events/sched/sched_wakeup \
           $T/events/irq/irq_handler_entry $T/events/irq/irq_handler_exit \
           $T/events/raw_syscalls/sys_enter $T/events/raw_syscalls/sys_exit \
           $T/events/*mali* $T/events/*kbase* $T/events/*gpu*; do
    [ -e "$d/enable" ] && echo "$1" > "$d/enable"
    [ "$1" = 0 ] && [ -e "$d/filter" ] && echo 0 > "$d/filter" 2>/dev/null
    for e in "$d"/*/enable; do [ -e "$e" ] && echo "$1" > "$e"; done
  done
}
sumstat() {  # sumstat NAME : the sum of a per_cpu/cpu*/stats counter
  n=0
  for v in $(sed -n "s/^$1: *//p" $T/per_cpu/cpu*/stats 2>/dev/null); do n=$((n + v)); done
  echo $n
}
killreaders() {   # the drain shell and its `cat trace_pipe`, recorded by `drain`
  if [ -f "$PIDF" ]; then
    for p in $(cat "$PIDF"); do
      case "$p" in ''|*[!0-9]*) continue ;; esac
      # Where /proc says what the pid is, only kill ours (a recycled pid is not).
      if [ -r /proc/$p/cmdline ]; then
        tr '\0' ' ' < /proc/$p/cmdline | grep -q 'trace_pipe\|drain' || continue
      fi
      kill $p 2>/dev/null
    done
    rm -f "$PIDF"
  fi
}
mode=$1; shift
case "$mode" in
  mount)   # mount [only if absent]; print whether this call did
    if [ -e $T/trace_clock ]; then echo mounted=0
    elif mount -t tracefs nodev $T 2>/dev/null; then echo mounted=1
    else echo err=tracefs-cannot-be-mounted; fi ;;
  setup)   # setup WANT_KB CAP_KB TID : print the previous state, then apply ours
    if [ -n "$(cat $T/set_event 2>/dev/null)" ]; then echo err=events-already-enabled; exit 0; fi
    echo "clock=$(sed -n 's/.*\[\([a-z_-]*\)\].*/\1/p' $T/trace_clock)"
    echo "buf=$(cat $T/buffer_size_kb)"
    echo "on=$(cat $T/tracing_on)"
    ncpu=$(ls -d $T/per_cpu/cpu* 2>/dev/null | wc -l); [ "$ncpu" -ge 1 ] || ncpu=1
    per=$(($1 / ncpu)); cap=$(($2 / ncpu))
    capped=0; [ "$per" -le "$cap" ] || { per=$cap; capped=1; }
    [ "$per" -ge 1024 ] || per=1024
    echo "ncpu=$ncpu"; echo "percpu_kb=$per"; echo "capped=$capped"
    echo 0 > $T/tracing_on
    echo mono > $T/trace_clock
    echo "$per" > $T/buffer_size_kb
    echo > $T/trace
    # The arch timer ticks on every CPU every few ms and is pure noise. Prefer the string filter on
    # the handler's name; if this kernel rejects it, filter on the irq number(s) /proc/interrupts
    # lists for it. Either way every other interrupt (osd_irq, the Mali ones) is kept.
    ff=name
    for e in entry exit; do
      echo 'name != "arch_timer"' > $T/events/irq/irq_handler_$e/filter 2>/dev/null || ff=""
    done
    if [ -z "$ff" ]; then
      expr=""
      for n in $(sed -n 's/^ *\([0-9][0-9]*\):.*arch_timer.*/\1/p' /proc/interrupts 2>/dev/null); do
        expr="${expr:+$expr && }irq != $n"
      done
      ff=none
      if [ -n "$expr" ]; then
        ff=irq
        for e in entry exit; do
          echo "$expr" > $T/events/irq/irq_handler_$e/filter 2>/dev/null || ff=none
        done
      fi
    fi
    echo "irqfilter=$ff"
    events 1
    if [ -n "$3" ]; then
      for e in enter exit; do
        echo "common_pid == $3" > $T/events/raw_syscalls/sys_$e/filter 2>/dev/null || echo err=syscall-filter
        echo 1 > $T/events/raw_syscalls/sys_$e/enable 2>/dev/null || echo err=no-syscall-events
      done
    fi
    [ -e $T/events/sched/sched_switch/enable ] || echo err=no-sched-events
    echo ready ;;
  run)     # run SECS
    echo 1 > $T/tracing_on; sleep "$1"; echo 0 > $T/tracing_on ;;
  stats)   # the ring's state before the drain, so an overrun is reported rather than silent
    echo "entries=$(sumstat entries)"; echo "overrun=$(sumstat overrun)"
    for f in $T/per_cpu/cpu*/stats; do
      c=${f%/stats}; c=${c##*/}
      echo "cpu=$c $(sed -n 's/^overrun: */overrun=/p' $f | head -1) $(sed -n 's/^entries: */entries=/p' $f | head -1)"
    done ;;
  hasgzip) command -v gzip >/dev/null 2>&1 && echo yes || echo no ;;
  drain)   # drain BOUND : trace_pipe to stdout until the ring is empty (or BOUND seconds)
    cat $T/trace_pipe & rp=$!
    echo "$$ $rp" > "$PIDF"
    idle=0; waited=0
    while [ "$waited" -lt "$1" ]; do
      if [ "$(sumstat entries)" -eq 0 ]; then
        idle=$((idle + 1)); [ "$idle" -lt 2 ] || break
      else idle=0; fi
      sleep 1; waited=$((waited + 1))
    done
    kill $rp 2>/dev/null; wait $rp 2>/dev/null; rm -f "$PIDF"
    exit 0 ;;
  stopreader) killreaders ;;   # end a drain early: its gzip then sees EOF and flushes a clean stream
  restore) # restore MOUNTED CLOCK BUF ON
    echo 0 > $T/tracing_on
    killreaders
    events 0
    echo > $T/trace
    [ -n "$2" ] && echo "$2" > $T/trace_clock
    [ -n "$3" ] && echo "$3" > $T/buffer_size_kb
    [ -n "$4" ] && echo "$4" > $T/tracing_on
    [ "$1" = 1 ] && umount $T
    exit 0 ;;
esac
SH
remote_script() { printf 'T=%s\nPIDF=%s\n%s\n' "$T" "$PIDF" "$REMOTE"; }
remote() { remote_script | tv sh -s -- "$@"; }
val() { sed -n "s/^$1=//p" <<<"$2" | head -1; }

# ---- restore: runs on every exit once the set may have been touched --------------------------
touched=0; mounted=0; prev_clock=""; prev_buf=""; prev_on=""
reading=0; rpid=""; partial=0; rz=no; tmp="$out.part"

dec() { if [ "$rz" = yes ]; then gzip -dc "$1" 2>/dev/null; else cat "$1"; fi; }
# Seconds of trace in a gzip (last event time minus first), from the `ts:` field.
covered() {
  gzip -dc "$1" 2>/dev/null | awk '{ for (i = 1; i <= NF && i < 8; i++) if ($i ~ /^[0-9]+\.[0-9]+:$/) {
      t = $i + 0; if (!n++) a = t; b = t; break } } END { printf "%.1f", n ? b - a : 0 }'
}
# Stop the reader if it is still going and turn what arrived into a valid gzip at $out. Runs on the
# normal path and from the exit trap, so a ^C leaves a usable (partial) file too.
finalize() {
  [ "$reading" = 1 ] || return 0
  reading=0
  if kill -0 "$rpid" 2>/dev/null; then
    partial=1
    # Ask the set to end the drain (its gzip then flushes and finishes cleanly); only a reader that
    # ignores that for ~10 s is killed from here, which can lose the last gzip block.
    remote stopreader >/dev/null 2>&1
    for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$rpid" 2>/dev/null || break; sleep 1; done
    kill "$rpid" 2>/dev/null
  fi
  { wait "$rpid"; } 2>/dev/null; rc=$?
  [ "$rc" -eq 0 ] || partial=1
  if [ "$partial" = 1 ]; then
    dec "$tmp" | sed '$d' | gzip -1 > "$out"   # drop the final, possibly cut, line
  elif [ "$rz" = yes ]; then mv "$tmp" "$out"
  else gzip -1 < "$tmp" > "$out"
  fi
  rm -f "$tmp"
}
restore() {
  finalize
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

r="$(remote setup "$want_kb" "$cap_kb" "$tid")"
# Only values that look like values reach the restore command line.
for k in clock buf on; do
  v="$(val "$k" "$r")"
  case "$v" in *[!A-Za-z0-9_-]*) v="" ;; esac
  case "$k" in clock) prev_clock="$v" ;; buf) prev_buf="$v" ;; on) prev_on="$v" ;; esac
done
[ -z "$(val err "$r")" ] || fail "$(val err "$r")"
grep -q '^ready$' <<<"$r" || fail "setup did not complete"

per_kb="$(val percpu_kb "$r")"; ncpu="$(val ncpu "$r")"
echo "tv-sched-trace: recording ${secs} s (per-CPU buffer ${per_kb} KB x ${ncpu} CPUs, arch timer irq filter: $(val irqfilter "$r")${tid:+, syscalls of tid $tid})..." >&2
if [ "$(val capped "$r")" = 1 ]; then
  echo "tv-sched-trace: WARNING: the ring is capped at $((cap_kb / 1024)) MB, about ${cap_secs} s of events; a ${secs} s run will overrun it" >&2
fi
if [ "$(val irqfilter "$r")" = none ]; then
  echo "tv-sched-trace: WARNING: the arch timer could not be filtered; expect extra irq noise and a faster-filling ring" >&2
fi
remote run "$secs" || fail "the run was interrupted"

st="$(remote stats)" || fail "could not read the buffer stats"
echo "tv-sched-trace: buffer holds $(val entries "$st") events, $(val overrun "$st") overwritten before they could be read" >&2
if [ "$(val overrun "$st")" != 0 ]; then
  echo "tv-sched-trace: WARNING: the ring OVERRAN; the oldest events are gone (per CPU below). Use a shorter --secs." >&2
  grep '^cpu=' <<<"$st" | sed 's/^/tv-sched-trace:   /' >&2
fi

[ "$(remote hasgzip)" = yes ] && rz=yes
bound=$(( read_timeout + 10 ))   # the set's own stop, never the one that normally ends the read
cmd="sh -s -- drain $bound"; [ "$rz" = yes ] && cmd="$cmd | gzip -1"
echo "tv-sched-trace: reading the trace back (up to ${read_timeout} s)..." >&2
reading=1
"$TVSSH" ssh tv "$cmd" < <(remote_script) > "$tmp" &
rpid=$!
SECONDS=0
while kill -0 "$rpid" 2>/dev/null; do
  if [ "$SECONDS" -ge "$read_timeout" ]; then partial=1; break; fi
  sleep 1
done
finalize
if ! [ -s "$out" ] || ! gzip -t "$out" 2>/dev/null; then fail "could not read the trace"; fi
lines="$(gzip -dc "$out" | grep -c '^ *[^# ]')"
if [ "$partial" = 1 ]; then
  echo "tv-sched-trace: PARTIAL trace: the read-back was stopped after ${SECONDS} s; $out holds $lines lines covering $(covered "$out") s of the ${secs} s recorded" >&2
  exit 3
fi
echo "tv-sched-trace: wrote $out ($(wc -c < "$out" | tr -d ' ') bytes, $lines lines, $(covered "$out") s)" >&2
