---
name: crash-triage
description: >
  Diagnose the app dying on the TV — a crash, a SIGSEGV/SIGILL/SIGABRT, a black screen,
  the app vanishing mid-run, a harness case failing with "no line found", or the app
  refusing to relaunch. Symbolizes the crash tracer's raw program counter to a function
  (or file:line), routes by signal to the right evidence, and rules out the impostors
  that look like crashes but aren't: a sleeping TV, a stale deployed binary, and SAM's
  stale "running" state. Use it before reading any log by hand.
---

# Crash triage on the TV

The C tracer (`src/main.c`) catches the fatal signals, writes what it knows, then
**re-raises to `SIG_DFL`** so the OS crash daemon still captures a real backtrace. Per
crash it emits:

```
*** SIGNAL 11 (SIGSEGV) addr=0x6bcf pc=0xf5a4dd08 lr=0x0
at:  <the /proc/self/maps line containing pc or lr>    -- which library faulted
bin: <the maps line for our own executable>            -- our load base, for addr2line
```

Turning `pc` into a source location needs `pc - load_base` fed to `addr2line`. Nothing in
the repo did that arithmetic until `tools/crash-report.sh`.

All paths are relative to the repo root. The TV address comes from the `Makefile`
(override with `TV=…`); no addresses or credentials live in this skill.

## Run this first

```bash
tools/crash-report.sh            # evidence + symbolize the most recent crash
tools/crash-report.sh --all      # every crash in the persistent log
tools/crash-report.sh --collect  # evidence bundle only, no symbolization
```

It checks reachability, compares the local and deployed binary md5, checks codegen,
prints the crash block, symbolizes `pc`/`lr` **only when they fall inside our mapping**,
and then dumps stderr, the SAM exit status and the crash-daemon reports.

## Rule out the impostors before believing a crash

1. **Is the TV awake?** A sleeping TV makes *every* log assertion fail as "no line
   found" — a whole FPS suite reported `0 fps samples` on five scenes for this reason,
   which reads exactly like a total regression. Wake it (`wake-tv` skill) and re-run
   before triaging anything.
2. **Is the deployed binary the one you built?** A standby can truncate an scp mid-deploy.
   The driver md5-compares; on mismatch, symbolized addresses are meaningless.
3. **Did it actually crash?** SAM keeps stale "running" state after a hard kill, so a
   launch can be a silent no-op relaunch. Check the SAM `exit_status` in the driver's
   output: a clean exit shows `exit_status: 0`; `768` is `exit(3)`; a signal death shows
   up as `WIFSIGNALED` in the low byte.

## Read the right log

| Log | Lifetime |
|---|---|
| `/tmp/plxnative-crash.log` | **append-only, survives the relaunch** — read this after a crash+restart |
| `/tmp/plxnative-events.log` | truncated at every launch — after a relaunch it is already gone |
| `/tmp/plxnative-stderr.log` | where Rust panics print |

## Route by signal

- **SIGSEGV / SIGBUS (11 / 7)** — bad pointer or a wrong struct offset. If the `at:` line
  shows the PC inside a **TV shared library**, there are no symbols for it and it is
  almost never their bug: it is a bad argument or a wrong offset on the call path into
  that library. Go to the `bind-tv-lib-abi` skill and re-verify every offset on that path.
  If the PC is inside our binary, the symbolized frame is the site.
- **SIGILL (4)** — suspect *codegen*, not logic. The default ARMv6 codegen emits a CP15
  barrier that is undefined on this SoC. The driver prints the CP15 instruction count
  (must be `0`) and the CPU arch tag (must be `v7`); a stale hand-built staticlib is the
  usual cause. No log will tell you this.
- **SIGABRT / SIGTRAP (6 / 5)** — usually a **Rust panic** crossing the FFI boundary. The
  PC is inside `abort()` and is worthless; the evidence is `/tmp/plxnative-stderr.log`.
- **No signal at all, app just gone** — not a caught crash. Check the SAM exit status, an
  OOM/memchute kill, or a deliberate close.

## Symbolization: what you get

The release build has **no DWARF**, so `addr2line` resolves the **function name** only
(`plex_run at ??:?`). That is usually enough to route.

For **file:line**, rebuild with debug info:

```bash
make DEBUG=1                       # C frames resolve immediately
touch rust-modules/src/lib.rs && make DEBUG=1   # also force the Rust rebuild
make deploy                        # then reproduce
```

`make` does not track `RUSTFLAGS` changes, so the Rust staticlib will not rebuild on its
own — hence the `touch`. Verified output after that: `plex_run at rust-modules/src/app.rs:248`,
`main at src/main.c:92`. Same codegen, larger binary; deploy it only while chasing a crash.

## Gotchas

- **pmlog's wall clock is hours off on this TV.** Correlate by monotonic uptime (the
  bracketed number in `/var/log/messages`) or the app's own `SDL_GetTicks` stamps — never
  by time of day.
- **`lr=0x0` is normal** for a signal delivered asynchronously; only `pc` is meaningful then.
- **The crash daemon's reports** (`/var/log/reports/librdx/`) are the real backtraces and
  exist *because* the tracer re-raises. The driver lists them; pull one when the single
  frame isn't enough.
- **Older notes say `/tmp/poc-*`.** The app was renamed; every path is `/tmp/plxnative-*` now.
- **A guard-page allocator exists** for memory-corruption hunts (`src/gpdebug.c`, never in
  the normal build) — reach for it when a SIGSEGV moves around between runs.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `TV … is unreachable` | Wake it (`wake-tv` skill). Re-run whatever failed *before* concluding anything. |
| `MISMATCH` on the md5 | The TV isn't running your build. `make deploy` and reproduce. |
| Crash log empty but the app died | Not a caught signal — read the SAM exit status and stderr sections of the driver output. |
| `?? ??:0` for an address in our binary | Release build without DWARF. Rebuild with `DEBUG=1` (see above). |
| Every frame says "outside our binary" | The fault is in a TV library; the `at:` line names it. Treat as an ABI/argument bug. |
