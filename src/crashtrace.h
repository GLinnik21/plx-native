/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository root.
 *
 * crashtrace.h — the fatal-signal tracer: `plx_crash_install`, called once from `main`, and
 * `plx_exit_record`, called by the executable's `exit` (`src/exit.c`).
 */
#ifndef PLX_CRASHTRACE_H
#define PLX_CRASHTRACE_H

#include <stddef.h>

/* Arm the handler on SIGSEGV/SIGABRT/SIGBUS/SIGFPE/SIGILL/SIGSYS/SIGTRAP, ignore SIGPIPE
 * process-wide, and (in a non-PLX_DEBUG build) suppress core dumps.
 *
 * It also arms the FREEZE probe on SIGUSR2: the Rust main-loop watchdog `pthread_kill`s the main
 * thread with it when the loop stands still, and the handler writes a `*** FREEZE` record and
 * RETURNS (see `src/crashtrace.c`). Call it on the thread the watchdog will watch: it reads that
 * thread's stack bound.
 *
 * `event_fd` and `crash_fd` are the two sinks the record is written to, raw and already open — the
 * caller opens them BEFORE calling this, which is what guarantees a signal can never arrive at
 * code that must open something first. Either may be -1; a record still goes to the other.
 *
 * Call once. Calling it twice is harmless but pointless.
 */
void plx_crash_install(int event_fd, int crash_fd);

/* The most `*** FREEZE` records the handler writes in one process. A backstop against a stray
 * signal storm: the Rust watchdog sends at most `MAX_FREEZE_PROBES` (four), so this is never the
 * limit a real freeze meets. */
#define PLX_MAX_FREEZE_RECORDS 8

/* Scan a `/proc/self/maps`-shaped file and emit an `at:` line for every mapping containing `pc` or
 * `lr`, and a `bin:` line for every mapping of our own executable. Writes to the descriptors given
 * to [`plx_crash_install`].
 *
 * The handler calls this with `/proc/self/maps`. It is exported, rather than being a static with a
 * hardcoded path, so `ci/crashtrace-test.c` can point it at a fixture — which is the only way to
 * exercise the cases that actually break a chunked reader (a line straddling the 4 KiB boundary, a
 * final line with no newline, a line longer than the buffer) and the only way to exercise ANY of
 * it on a host with no `/proc`. Async-signal-safe, like everything it calls. */
void plx_crash_scan_maps_file(const char *path, unsigned long pc, unsigned long lr);

/* The record of a libc `exit()` made inside the process, and then the process's end: `src/exit.c`
 * (the executable's own definition of `exit`, so that an `exit` made by a shared library lands
 * here) calls this with the status and its own return address (`caller`, the RAW address of the
 * `exit` call), and it never returns.
 *
 * It writes one record to BOTH logs: the event log, and the append-only crash log, which the next
 * launch's importer (`crashreport::parse`) turns into an error report of its own kind (an exit is
 * not a crash, so it is not counted as one). The record is a `*** EXIT status=<n> caller=0x<hex>`
 * header, the `time:` and `thread:` lines of a fault record, and the `/proc/self/maps` line (`at:`)
 * of the mapping that contains the call, which is how a stripped firmware library that called
 * `exit` gets named. Then it ends the process with `status`. Its lines are dropped once a fatal
 * signal's handler has begun, so they never land in the middle of a crash record. It also stores
 * the "exiting" phase in `plx_crash_phase`, so a fault in these last instructions says so.
 *
 * **Async-signal-safe, all of it,** because `exit` can be called from a signal handler: lock-free
 * `__atomic` operations, `syscall(SYS_gettid)` and `prctl`, `time`, `getpid`,
 * `open`/`read`/`write`/`close`, `errno` (read only after a failed `read` or `write`, to retry an
 * interrupted one), `pause`, `syscall(SYS_exit_group)` (`_exit` itself is only the unreachable
 * fallback behind it, and the way out off Linux) and static buffers, and nothing that takes a lock
 * or allocates. Every one of them but `pause` and that `errno` read is bound by
 * `plx_crash_install` ahead of time, so the exit itself runs no lazy PLT resolution. Only the
 * first caller writes a record. A second one (another thread's exit, or this one re-entered from a
 * signal handler) ends the process at once and never waits.
 *
 * **While a fatal signal's handler is running, none of this ends the process:** the caller parks
 * in `pause` and the handler's re-raise ends it, so the crash record is not cut short and SAM sees
 * the signal and not exit status `status`.
 *
 * **What it does not do is flush.** `exit` would flush the C stdio streams, and the Rust-side
 * `exit_now` flushes Rust's stdout and stderr as well; both take locks that the interrupted code
 * may hold, so this does neither. That drops at most a partial line still buffered in Rust's stdout
 * or in a C stdio stream. The event and crash logs are raw descriptor writes with no buffer, and
 * lose nothing.
 *
 * Before [`plx_crash_install`] has run there is no sink and it simply ends the process (nothing is
 * bound ahead of time then, so that exit resolves `syscall` lazily). */
void plx_exit_record(int status, unsigned long caller) __attribute__((noreturn));

/* The freeze probe's number, OWNED BY THE RUST WATCHDOG (`plx_base::task::watchdog`, which defines
 * the symbol): it stores the number right before it signals the main thread and the handler takes
 * it back to 0 and writes it as `seq=`. A SIGUSR2 that finds 0 is not a probe and writes nothing,
 * so the records and the `freeze-end: seq=` lines Rust writes cannot number differently. A host
 * test that links `crashtrace.c` without Rust defines it itself. */
extern volatile unsigned int plx_freeze_seq;

/* [`plx_crash_scan_maps_file`] for a freeze: an `at:` line for every mapping containing ANY of the
 * `n` addresses (pc, lr and the walked frames), using the freeze handler's own buffers. Exported
 * for the same reason, so the fixtures can reach it. */
void plx_freeze_scan_maps_file(const char *path, const unsigned long *addrs, size_t n);

#endif /* PLX_CRASHTRACE_H */
