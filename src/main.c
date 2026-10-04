/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository
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
#include <sys/stat.h>   /* fchmod — the log sinks are created 0640, see open_fd_log */
#include <errno.h>      /* open_fd_log tells a missing sink (create it) from a refused one */

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
 * The answer comes from Rust (`plx_runtime_path`, rust-modules/base/src/paths.rs) rather than from a
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
 * Two static buffers are alternated per call. No current call site holds two results at once, so
 * this is cheap insurance for a future one rather than a live hazard. */
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

static int open_fd_log(const char *path, int flags);

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
 * Mode 0640, like the other two sinks — see `open_fd_log` for why group read and why that is
 * acceptable. Every line Rust appends passes `eventlog::scrub::scrub_local` first, so the server name,
 * LAN addresses, Plex GUIDs, search queries and household names this file once held in the clear
 * are rewritten before they reach the disk. */
static FILE *open_event_log(void) {
    int fd = open_fd_log(runtime_path("plxnative-events.log"), O_TRUNC | O_APPEND);
    return fd >= 0 ? fdopen(fd, "a") : NULL;
}

/* The three log sinks — events, crash, stderr — are all opened here, and all end up **0640**:
 * owner read/write, the app's own group (gid 5000 on the television) read, nothing for anyone else.
 * They have to be opened this way rather than with `fopen`, which creates 0666 & ~umask and gave
 * two of them mode 0644 on the television (measured 2026-08-12, installing the v0.3.0 package):
 * `/tmp` here is the SHARED system /tmp, mode 1777 in both jail profiles, so 0644 means every
 * co-resident process can read them.
 *
 * Why group read, when these were 0600. A Developer Mode television that is not rooted gives its
 * owner exactly one tool, webOS Dev Manager, and that reads files as an unprivileged user that is
 * in the app's group but is not the app's uid. At 0600 none of the app's logs could be fetched, so
 * a bug report was impossible without root. 0640 is what `plxnative-diag.log` has always shipped
 * (rust-modules/platform/src/storage/diagnostics.rs); "other" still gets nothing.
 *
 * Why that is acceptable now. The 0600 was the fix for a world-readable log, and the file's CONTENT
 * has since been fixed at the source: every event-log line Rust writes passes
 * `eventlog::scrub::scrub_local` (credentials, hosts, bare addresses, Plex GUIDs, search queries and
 * household names) before the write, call sites that logged titles or dialogue were fixed, and
 * the `no_log_call_site_interpolates_viewing_content` test keeps them fixed.
 *
 * What is NOT closed, stated plainly. Another native app on the same television that runs in
 * gid 5000 can read these three files. The crash log is a faulting PC, a signal, one
 * /proc/self/maps line (where THIS binary is mapped) and the Rust panic text. stderr is whatever
 * aborts, panics, GL info logs and the TV's own libraries print. The Rust panic hook scrubs the
 * panic text it appends to the crash log (`app::boot`), but stderr does not pass through
 * `scrub_local`, and neither do the tracer's records or the C-side event-log lines in
 * `src/starfish.c` (pointers, library paths, pipeline ids, Starfish replies). Reading the producers (2026-10-04,
 * docs/distribution.md section 4) found no credential-bearing path into any of them: libcurl is
 * never put in verbose mode, every libavformat open uses a custom AVIO with no URL, and the
 * production panic sites that were read format fixed text. But that is an audit of the code that
 * exists, not a mechanism; a future `unwrap()` on a value that embeds a URL would reach stderr
 * verbatim, and the crash log unless the scrubber happens to recognise it. Keep credentials out of
 * anything that can be printed to stderr.
 *
 * The fixed path lives in shared `/tmp`, so opening is also a security boundary: `O_NOFOLLOW`
 * rejects symlinks; `fstat` on the OPENED descriptor requires a regular file owned by this app uid
 * with exactly ONE link; only after that check may `fchmod` or a requested truncate touch the
 * inode. The `fchmod` is what makes the mode independent of the umask and corrects an append
 * target that survived from a 0600 release.
 *
 * `st_nlink == 1` is the hard-link half of the boundary. A co-resident app in gid 5000 that can
 * link names in the sticky `/tmp` can `link(2)` one of OUR 0600 files (the `auth.json` session
 * fallback lives in the same runtime directory) onto a sink name when `fs.protected_hardlinks` is
 * off: the open then lands on an inode that is a regular file and ours, so every other check
 * passes, and the `fchmod` would publish it to the group while a requested truncate would empty
 * it. A sink with a second name is therefore somebody else's inode and is refused, untouched.
 *
 * An existing file is opened WITHOUT `O_CREAT`; only a name that does not exist is created, with
 * `O_EXCL`, so the `fchmod` below runs on an inode this process just made or on a single-link one it
 * already owned, and never on whatever happened to be at the name. */
static int open_fd_log(const char *path, int flags) {
    const int truncate = flags & O_TRUNC;
    const int base = (flags & ~O_TRUNC) | O_WRONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK;
    int fd = open(path, base);
    if (fd < 0 && errno == ENOENT) {
        fd = open(path, base | O_CREAT | O_EXCL, 0640);
        if (fd < 0 && errno == EEXIST) fd = open(path, base); /* lost a race to a creator: re-check it */
    }
    if (fd < 0) return -1;
    struct stat st;
    if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode) || st.st_uid != geteuid() || st.st_nlink != 1) {
        close(fd);
        return -1;
    }
    if (fchmod(fd, 0640) != 0 || (truncate && ftruncate(fd, 0) != 0)) {
        close(fd);
        return -1;
    }
    return fd;
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
    int event_fd = open_fd_log(runtime_path("plxnative-events.log"), O_APPEND);
    int crash_fd = open_fd_log(runtime_path("plxnative-crash.log"), O_APPEND); /* append: keep prior crashes across relaunches */
    /* Written while allocation and ELF parsing are safe. The signal path then needs only numbers,
     * while the next launch can still pair a record with THIS executable after a deploy. */
    plx_crash_write_image_marker(crash_fd);
    /* stderr is redirected from the SAME verified descriptor. Re-opening by path with `freopen`
     * would reintroduce the symlink race that `open_fd_log` just closed. */
    int stderr_fd = open_fd_log(runtime_path("plxnative-stderr.log"), O_TRUNC | O_APPEND);
    if (stderr_fd >= 0) {
        if (dup2(stderr_fd, STDERR_FILENO) >= 0)
            fcntl(STDERR_FILENO, F_SETFD, FD_CLOEXEC);
        if (stderr_fd != STDERR_FILENO) close(stderr_fd);
    }
    plx_crash_install(event_fd, crash_fd);
    /* request BACK key delivery from the webOS access policy (before SDL init) */
    setenv("SDL_WEBOS_ACCESS_POLICY_KEYS_BACK", "true", 1);
    return plex_run(PMS_HOST, PMS_PORT);
}
