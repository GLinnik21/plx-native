/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under the MIT Licence; see LICENSE at the repository root.
 *
 * crashtrace.h — install the fatal-signal tracer. One entry point, called once from `main`.
 */
#ifndef PLX_CRASHTRACE_H
#define PLX_CRASHTRACE_H

/* Arm the handler on SIGSEGV/SIGABRT/SIGBUS/SIGILL/SIGTRAP, ignore SIGPIPE process-wide, and (in a
 * non-PLX_DEBUG build) suppress core dumps.
 *
 * `event_fd` and `crash_fd` are the two sinks the record is written to, raw and already open — the
 * caller opens them BEFORE calling this, which is what guarantees a signal can never arrive at
 * code that must open something first. Either may be -1; a record still goes to the other.
 *
 * Call once. Calling it twice is harmless but pointless.
 */
void plx_crash_install(int event_fd, int crash_fd);

#endif /* PLX_CRASHTRACE_H */
