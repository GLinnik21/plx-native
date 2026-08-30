/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under the MIT Licence; see LICENSE at the repository
 * root, and THIRD-PARTY-NOTICES.md for the components this links or redistributes.
 * Not affiliated with, endorsed by, or sponsored by Plex GmbH or LG Electronics.
 *
 * plxnative — native webOS Plex client. BOOT SHIM only: the app core is Rust
 * plex_run() (rust-modules). This file stays C for the genuinely low-level
 * bootstrap that must run before any Rust executes — the async-signal-safe crash
 * tracer, the event-log handle, stderr capture, and process bring-up. Everything
 * else (SDL, the event loop, input, playback orchestration, draw) is Rust. */
#include "app.h"
#include "crashtrace.h"  /* the fatal-signal tracer — its own TU, so ci/crashtrace-test.c can fault it */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/stat.h>   /* fchmod — the log sinks are created 0600, see open_log_0600 */

FILE *elogf = NULL;   /* shared event/diagnostic log (extern in app.h); used by the
                       * crash handler here and by the starfish.c seam. Opened "w" each
                       * launch, so it is TRUNCATED on relaunch — do not rely on it to
                       * survive a crash+relaunch (that is what the crash log and `crash_fd` are for). */
/* The crash log has no `FILE *` any more, only a raw descriptor handed to `plx_crash_install`: its
 * ONLY writer is the signal handler, and stdio is not usable there. See `src/crashtrace.c`. */

extern int plex_run(const char *pms_host, int pms_port);  /* Rust app core (no creds — session or /tmp/plxnative-token) */
extern int plx_sentry_spool_external(const char *path); /* Sentry daemon's spool-only re-entry */
extern void plx_crash_write_image_marker(int fd); /* identify this binary in the append-only log */

/* Where this INSTALL's runtime files live — `/tmp/plxnative-events.log` for the app users get,
 * `/tmp/com.beb.plxnative.debug/plxnative-events.log` for a developer build installed beside it.
 *
 * The answer comes from Rust (`plx_runtime_path`, rust-modules/src/paths.rs) rather than from a
 * literal here, and that is the whole point: this file opens three logs before a single line of
 * Rust runs, and `crate::log` and the panic hook then append to two of them. Two definitions of
 * "where" — one in C, one in Rust — is the split-brain paths.rs documents at length: main.c would
 * truncate a log at /tmp while Rust wrote to another, and the crash report built afterwards would
 * be missing whichever half you did not look at. One resolver, asked twice, cannot disagree.
 *
 * Safe to call this early: the resolver touches nothing but its own lazily-initialised path and
 * must not log (it would re-enter that initialisation). If it ever fails — a path longer than the
 * buffer — the literal below keeps the behaviour every release so far has had, which is the right
 * fallback because the app users get is exactly the one whose root IS `/tmp`.
 *
 * Two static buffers, alternated per call. No CURRENT call site holds two results at once — the
 * stderr pair completes its `open_log_0600(...); fclose(...)` before the `freopen` beside it is
 * evaluated — so this is cheap insurance for a future one that does, not a hazard being averted.
 * Said plainly because the first version of this comment claimed the collision was live, which is
 * the kind of invented justification that makes a reader distrust the rest of the file. */
extern int plx_runtime_path(const char *name, char *out, size_t cap);

static const char *runtime_path(const char *name) {
    static char buf[2][256];
    static int slot = 0;
    char *b = buf[slot];
    slot ^= 1;
    if (plx_runtime_path(name, b, sizeof buf[0])) return b;
    snprintf(b, sizeof buf[0], "/tmp/%s", name);
    return b;
}

/* Open the event log TRUNCATED (fresh each launch, as `make run` and tests/run.py both assume)
 * but in APPEND mode, so every write lands at end-of-file.
 *
 * Both halves matter, and it used to be `fopen(…, "w")`, which gets only the first. A "w" stream
 * carries its own file offset, while Rust's `crate::log` opens the same path with O_APPEND — so
 * the two writers do not see each other's output and the C side writes back over lines the Rust
 * side has already appended. That is not theoretical: it ate the first two characters of the very
 * first Rust log line at boot ("surface: …" arrived as "rface: …"), which is exactly the kind of
 * corruption that makes a log untrustworthy at the moment you most need it — and it silently ate
 * whole lines whenever the C side wrote more than Rust had.
 *
 * Mode 0600 because this file records the server name, the LAN address, Plex Home profile names
 * and episode titles, and /tmp is world-readable. */
static FILE *open_event_log(void) {
    int fd = open(runtime_path("plxnative-events.log"), O_WRONLY | O_CREAT | O_TRUNC | O_APPEND, 0600);
    return fd >= 0 ? fdopen(fd, "a") : NULL;
}

/* The other two log sinks, at the SAME 0600 — and they have to be opened this way rather than with
 * `fopen`, which creates 0666 & ~umask and gave both files mode 0644 on the television (measured
 * 2026-08-12, installing the v0.3.0 package). `/tmp` here is the SHARED system /tmp, mode 1777 in
 * both jail profiles, so 0644 means every co-resident process can read them. Neither is known to
 * carry a credential today — the crash log is a faulting PC and a /proc/self/maps line, stderr is
 * whatever aborts print — but "what this file happens to contain" is the wrong thing to depend on,
 * and it is the exact shape of this project's earlier token-in-a-world-readable-log incident.
 *
 * `open` + `fdopen` rather than `fopen` + `chmod`: a chmod after the fact leaves a window in which
 * the file exists at 0644, and on a relaunch the crash log ALREADY exists, where O_CREAT's mode is
 * ignored — hence the explicit `fchmod` on the existing file. */
static int open_fd_0600(const char *path, int flags) {
    int fd = open(path, flags | O_WRONLY | O_CREAT, 0600);
    if (fd < 0) return -1;
    fchmod(fd, 0600); /* an append target that survived a previous run keeps its old mode */
    return fd;
}

static FILE *open_log_0600(const char *path, int flags) {
    int fd = open_fd_0600(path, flags);
    return fd >= 0 ? fdopen(fd, (flags & O_APPEND) ? "a" : "w") : NULL;
}

int main(int argc, char **argv) {
    /* Sentry Native launches this executable with exactly one envelope path after the crashed
     * process is already beyond saving. Recognise and move it BEFORE opening (and truncating) any
     * ordinary app log, starting SDL, or arming another handler. A lookalike argument returns 0 and
     * proceeds as a normal launch; Rust validates the exact SDK directory, UUID and file type. */
    if (argc == 2 && plx_sentry_spool_external(argv[1])) return 0;
    elogf = open_event_log();
    /* The crash handler's own descriptors, both opened BEFORE `install_crash_tracer` arms the
     * handler — so a signal can never reach code that has to open something first, which is the
     * one thing a fault inside the allocator would make impossible.
     *
     * A SEPARATE fd on the event log rather than `fileno(elogf)`, and the reason is ordering
     * rather than tidiness: the `FILE *` carries a buffer, and a raw write through the same
     * descriptor would jump the queue past anything still sitting in it. Two O_APPEND descriptors
     * on one file each land at end-of-file per write, so the handler's lines follow whatever the
     * stream has already flushed and nothing is interleaved mid-line. (Every `elogf` writer in
     * `src/starfish.c` does `fflush` immediately today, so there is nothing pending in practice —
     * but a crash tracer must not depend on that continuing to be true.) */
    int event_fd = open_fd_0600(runtime_path("plxnative-events.log"), O_APPEND);
    int crash_fd = open_fd_0600(runtime_path("plxnative-crash.log"), O_APPEND); /* append: keep prior crashes across relaunches */
    /* Written while allocation and ELF parsing are safe. The signal path then needs only numbers,
     * while the next launch can still pair a record with THIS executable after a deploy. */
    plx_crash_write_image_marker(crash_fd);
    /* stderr is REPLACED, so it must go through freopen — but create the file at 0600 first, and
     * freopen's "a" then reuses that inode rather than making a fresh 0644 one. Two calls to
     * runtime_path(), which alternates buffers, so they cannot alias even though the first result
     * is dead by the time the second is taken. */
    { FILE *s = open_log_0600(runtime_path("plxnative-stderr.log"), O_TRUNC); if (s) fclose(s); }
    freopen(runtime_path("plxnative-stderr.log"), "a", stderr); /* capture abort/assert text */
    plx_crash_install(event_fd, crash_fd);
    /* request BACK key delivery from the webOS access policy (before SDL init) */
    setenv("SDL_WEBOS_ACCESS_POLICY_KEYS_BACK", "true", 1);
    return plex_run(PMS_HOST, PMS_PORT);
}
