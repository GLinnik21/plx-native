/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository root.
 *
 * crashtrace.h — install the fatal-signal tracer. One entry point, called once from `main`.
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
