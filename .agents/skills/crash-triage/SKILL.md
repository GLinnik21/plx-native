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

> **Collecting evidence is read-only and needs no lock** (`tools/crash-report.sh`, `tv-session.sh
> log`) — but the moment you RE-RUN to reproduce, take the television's lock first:
> `tools/tv-lock.sh acquire --why "reproduce <crash>"`. See the **`tv-lock`** skill.

There are now two evidence paths. When crash-report consent and a compiled endpoint are both
present, the patched Sentry Native ARM32 handler wakes the shipped `sentry-crash` process. That
process reads the stopped target, walks its frame chain, then suspends the other Linux LWPs with
`ptrace`, captures their registers and walks their frame chains before resuming them. It records
up to 128 frames for the crashing thread plus up to 32 for each captured non-crashing thread,
registers, modules, Linux/webOS versions and build ids. Its transport is disabled; the next
healthy PlxNative launch sanitises the envelope and sends it from the ordinary telemetry spool.
That Sentry event **is a
backtrace** when it contains multiple frames.

The local, always-available fallback is the C tracer (`src/crashtrace.c`). It catches the fatal
signals, writes what it knows, then **re-raises to `SIG_DFL`** so SAM sees a real `WIFSIGNALED`
death — and **not** so a crash daemon
produces a backtrace, which on this firmware it never does (see §"do not go looking for a crashd
backtrace" above; `core_pattern` is the bare string `core` and `RLIMIT_CORE` is 0, so no core is
written and the report chain never starts). Per crash it emits:

```
*** SIGNAL 11 (SIGSEGV) addr=0x6bcf pc=0xf5a4dd08 lr=0x0
reg: sp=0x… fp=0x… ip=0x… cpsr=0x… r0=0x… r1=0x… … r10=0x…
at:  <the /proc/self/maps line containing pc or lr>    -- which library faulted
bin: <the maps line for our own executable>            -- our load base, for addr2line
```

Turning `pc` into a source location needs `pc - load_base` fed to `addr2line`. Nothing in
the repo did that arithmetic until `tools/crash-report.sh`.

**Call the local C record a FAULT EVENT, not a backtrace, when you write it up.** Two frames and the
registers are what an async-signal-safe handler can honestly produce: `backtrace()` is not on the safe list, ARM
unwinding out of a handler commonly stops at `gsignal()`, and deferring does not help because by
then the stack is gone.

**And do not go looking for a crashd backtrace to fill the gap — you will not get one.** Device-
verified 2026-08-29 with a deliberate SIGSEGV: the re-raise gets SAM a real `exit_status: 11`, and
`/var/log/reports/librdx/` stays empty. `core_pattern` here is the bare string `core`, so the report
chain begins with a core FILE, and every build that is not `DEBUG=1` sets `RLIMIT_CORE` to 0 — for
good reason, on a 615.6 MB partition shared with every installed app that had 125.9 MB free. So the
fault event below IS the evidence. Treat an empty librdx directory as expected, not as a clue.

**A log from before 2026-08-29 has no crashd report to go with it, whatever this page says about
the re-raise.** The re-raise was added on 2026-07-09 and did not work: `sigaction` masks the signal
inside its own handler, so `raise()` only marked it pending and the `_exit(128 + sig)` below it ran
— a clean exit, no core, no `/var/log/reports/librdx/` entry, `WIFEXITED` for SAM. So when triaging
an old crash, **read a SAM `exit_status` of 35584 as a SIGSEGV** (`139 << 8`), 34304 as a SIGABRT
(`134 << 8`), and do not conclude from an absent crashd report that the process did not take a
signal. It was found by `ci/crashtrace-test.c`, which faults a process on purpose and asks how it
died — the one question the crash log cannot answer about itself. (The *other* half of that
change, making the handler async-signal-safe, was not a discovery at all: it is finding C3 of
`docs/architecture-review-2026-07-26.md`, filed five weeks earlier and left undone.)

The `reg:` line arrived with the same change (the rewrite that made the handler actually
async-signal-safe — it had been calling `fprintf`/`fopen`/`sscanf`), so an older log will not have
it. **Read it when the PC does not resolve**: r0-r3 are the first four arguments at
the call, `fp`/`sp` bound the frame, and a `cpsr` with bit 5 set says the fault was in Thumb code.

All paths are relative to the repo root. The TV address comes from the `Makefile`
(override with `TV=…`); no addresses or credentials live in this skill.

## First: which install crashed

Two builds can be on this television — `com.beb.plxnative` (stable, what users install) and
`com.beb.plxnative.debug` (the developer build beside it, and the Makefile's **default**). They
have separate app directories, separate runtime roots and therefore **separate crash logs**, so
triaging the wrong one produces a clean bill of health for an app that is dying. Ask the Makefile
rather than typing a path:

```bash
make -s print-appid    FLAVOR=debug     # com.beb.plxnative.debug
make -s print-appdir   FLAVOR=debug     # where the binary and the .so files are
make -s print-rundir   FLAVOR=debug     # /tmp/com.beb.plxnative.debug   (stable: /tmp)
make -s print-eventlog FLAVOR=debug
```

Not `make -p`/`make -pn`: that prints a recursive variable's UNEXPANDED DEFINITION, so `TV` comes
back as the literal `$(strip $(shell cat .tv-host …))` and every ssh built from it fails — which
reads as an unreachable television and sends you to `wake-tv` for a set that is already awake.

## Run this first

```bash
tools/crash-report.sh [--flavor <f>]  # evidence + symbolize the most recent crash
tools/crash-report.sh --all           # every crash in the persistent log
tools/crash-report.sh --collect       # evidence bundle only, no symbolization
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
   The driver md5-compares; on mismatch, symbolized addresses are meaningless. With two
   installs the check is **necessary but no longer sufficient**: `pkg/plxnative` is a path
   every flavour and both configurations write, so a MATCH proves the bytes and not the
   install, and a MISMATCH is at least as likely to mean you are pointed at the other app as
   at a bad scp. Settle it with the `install:` line, not by deploying — see the table below.
   `pidof plxnative` cannot settle it either: both binaries carry that name, so on this
   busybox set it returns two pids in an order nothing promises. Liveness is
   `fuser $(make -s print-appdir FLAVOR=<f>)/plxnative`, which is inode-scoped.
3. **Did it actually crash?** SAM keeps stale "running" state after a hard kill, so a
   launch can be a silent no-op relaunch. Check the SAM `exit_status` in the driver's
   output: a clean exit shows `exit_status: 0`; `768` is `exit(3)`; a signal death shows
   up as `WIFSIGNALED` in the low byte.

## Read the right log

All three live in the crashing install's **runtime root** — `/tmp` for stable, `/tmp/<app id>` for
a flavoured install (`make -s print-rundir FLAVOR=<f>`). The file names are identical in both, so
a path typed from memory reads the wrong app's log without erroring.

| Log | Lifetime |
|---|---|
| `<rundir>/plxnative-crash.log` | **append-only, survives the relaunch** — read this after a crash+restart |
| `<rundir>/plxnative-events.log` | truncated at every launch — after a relaunch it is already gone |
| `<rundir>/plxnative-stderr.log` | where Rust panics print |

**The event log's FIRST line names the install**, before anything can fail, and it is the only
witness in the system that does:

```
install: id=com.beb.plxnative.debug flavour=debug runtime=/tmp/com.beb.plxnative.debug features=dev APPID_env=…
appdir: /media/… (from current_exe)
```

`features=` is `dev` or `release`, which also settles "is this the shipped configuration?" without
a hash.

**The crash log is the one that survives, and it carries no `install:` line** — the tracer runs in
C, before and independently of any of that. What it does carry is the `bin:` maps line, which is
the full path of the faulting executable and therefore names the app directory, i.e. the id. Match
it anchored (`/<id>/`) and it is as good a witness as the event log's; read it as a bare substring
and it is worse than none, for the reason in the gotchas below.

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
  PC is inside `abort()` and is worthless; the evidence is `<rundir>/plxnative-stderr.log`.
- **No signal at all, app just gone** — not a caught crash. Check the SAM exit status, an
  OOM/memchute kill, or a deliberate close.

## Symbolization: what you get

The release build has **no DWARF**, so `addr2line` resolves the **function name** only
(`plex_run at ??:?`). That is usually enough to route.

For **file:line**, rebuild with debug info:

```bash
make DEBUG=1                       # C frames resolve immediately
touch rust-modules/src/lib.rs && make DEBUG=1   # also force the Rust rebuild
make FLAVOR=<f> deploy             # the SAME flavour you are triaging, then reproduce
```

`make` does not track `RUSTFLAGS` changes, so the Rust staticlib will not rebuild on its
own — hence the `touch`. Verified output after that: `plex_run at rust-modules/src/app.rs:248`,
`main at src/main.c:92`. Same codegen, larger binary; deploy it only while chasing a crash.

## Prove the tracer works before reading its silence

An empty `plxnative-crash.log` means one of two things and they are opposite: nothing crashed, or
the recorder is broken. **Two of the three ways this recorder can be broken have actually happened**
— it spent seven weeks re-raising nothing, and before that it `_exit(3)`d — and in both cases the
log looked entirely normal. So when a crash is suspected and the log is empty, fault the app on
purpose and check the recorder end to end.

```bash
tools/tv-lock.sh acquire --why "prove the crash tracer"
make deploy                                     # a devtriggers build; RELEASE=1 compiles this out
RUN=$(make -s print-rundir)
ssh root@"$(make -s print-tv)" "echo segv > $RUN/plxnative-crashtest"
make run RUN_SECS=12                            # it will die at once, on purpose
tools/crash-report.sh                           # the record, symbolized
```

Four things to check, and the third and fourth are the ones no host test can reach:

1. `crashtest: DELIBERATE crash, kind=segv signal=11` in the event log — so nobody later mistakes
   this for the bug they were hunting.
2. `*** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0x… lr=0x…` in the crash log, with a `reg:` line under it.
3. **`at:` and `bin:` lines.** These come from `/proc/self/maps` and exist on no development Mac;
   `ci/crashtrace-test.c` grades the reader against fixtures, but only the set proves it against
   the real file. `bin:` must name the install you deployed to.
4. **A SAM `exit_status` with `WIFSIGNALED` set — and an EMPTY `/var/log/reports/librdx/`.**
   The status is the re-raise and the only thing it buys. The empty directory is the expected
   result, not a clue: no core is written, so no report is ever generated. This step used to demand
   a fresh librdx entry as a PASS criterion, which fails a perfectly healthy tracer and sends you
   hunting a directory that is empty by construction. A status of `35584` means the re-raise
   regressed — that is `(128+11) << 8`, a clean exit wearing a signal's number.

`segv` is a genuine null write, so the PC is a real faulting PC. The other kinds — `abrt`, `bus`,
`ill`, `trap` — go through `raise` and prove each of the five `sigaction` calls took, but their PC
points into `raise` and is worth nothing.

**Clear the trigger afterwards.** `tests/run.py` sweeps `plxnative-*` on exit; `make run` does not,
so a by-hand session leaves it armed and the next launch dies too — which reads exactly like the
app having become unlaunchable.

```bash
ssh root@"$(make -s print-tv)" "rm -f $RUN/plxnative-crashtest"
```

## Gotchas

- **pmlog's wall clock is hours off on this TV.** Correlate by monotonic uptime (the
  bracketed number in `/var/log/messages`) or the app's own `SDL_GetTicks` stamps — never
  by time of day.
- **`lr=0x0` is normal** for a signal delivered asynchronously; only `pc` is meaningful then.
- **There are no system crashd reports to pull.** `/var/log/reports/librdx/` is empty by
  construction and this bullet used to say the opposite — that the re-raise produces real
  backtraces there. Do not confuse LG's absent `crashd` output with the app's shipped
  `sentry-crash` daemon: the latter hands its envelope to the telemetry queue and normally leaves
  no report file behind after a healthy restart. For consent-off/unconfigured builds, use
  `pkg/plxnative.debug` and `addr2line` on the local fault event.
- **Older notes say `/tmp/poc-*`.** The app was renamed; the names are all `plxnative-*` now,
  and they sit in the install's runtime root rather than always in `/tmp`.
- **`com.beb.plxnative` is a PREFIX of `com.beb.plxnative.debug`.** Anything that picks a crash
  block, a maps line or a log by app-directory path must anchor on a delimiter — match `/<id>/`,
  never the bare id — or every stable-id filter silently accepts the debug install's evidence too
  and you symbolize one app's addresses against the other's binary. `src/main.c`'s `bin:` matcher
  documents the same trap one level down: it tests `/plxnative\n` and `/plxnative ` rather than a
  bare substring, because the app directory is itself named `…com.beb.plxnative/` and a loose test
  also matched libraries deployed beside the binary.
- **A guard-page allocator exists** for memory-corruption hunts (`src/gpdebug.c`, never in
  the normal build) — reach for it when a SIGSEGV moves around between runs.
- **`make FLAVOR=<f> uninstall` takes the crash history with it.** It `rm -rf`s that install's
  whole runtime root, and the append-only crash log — the one artifact deliberately built to
  outlive a relaunch — is inside it. Pull anything you still want off the TV first; there is no
  copy anywhere else.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `TV … is unreachable` | Wake it (`wake-tv` skill). Re-run whatever failed *before* concluding anything. |
| `MISMATCH` on the md5 | **Check the flavour before you deploy anything.** With two installs this is routinely "you are looking at the other app", not "the TV has a stale build" — and the old advice here, a bare `make deploy`, would then put your dev build on top of the STABLE install on this skill's own recommendation, destroying a working app to investigate a crash that was never in it. Read the `install:` line of the event log in the runtime root you fetched from, compare it with `make -s print-appid FLAVOR=<f>`, and only once they agree treat the mismatch as a stale or truncated deploy — then `make FLAVOR=<f> deploy` and reproduce. |
| `<appdir> does not exist … not installed` from a deploy | That flavour has never been installed; scp cannot create an app. `make FLAVOR=<f> install` once (`tv-session` skill). |
| Crash log empty but the app died | Not a caught signal — read the SAM exit status and stderr sections of the driver output. |
| `?? ??:0` for an address in our binary | Release build without DWARF. Rebuild with `DEBUG=1` (see above). |
| Every frame says "outside our binary" | The fault is in a TV library; the `at:` line names it. Treat as an ABI/argument bug. |
