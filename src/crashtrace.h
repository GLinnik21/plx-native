/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under the MIT Licence; see LICENSE at the repository root.
 *
 * crashtrace.h — install the fatal-signal tracer. One entry point, called once from `main`.
 */
#ifndef PLX_CRASHTRACE_H
#define PLX_CRASHTRACE_H

/* Arm the handler on SIGSEGV/SIGABRT/SIGBUS/SIGFPE/SIGILL/SIGSYS/SIGTRAP, ignore SIGPIPE
 * process-wide, and (in a non-PLX_DEBUG build) suppress core dumps.
 *
 * `event_fd` and `crash_fd` are the two sinks the record is written to, raw and already open — the
 * caller opens them BEFORE calling this, which is what guarantees a signal can never arrive at
 * code that must open something first. Either may be -1; a record still goes to the other.
 *
 * Call once. Calling it twice is harmless but pointless.
 */
void plx_crash_install(int event_fd, int crash_fd);

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

#endif /* PLX_CRASHTRACE_H */
