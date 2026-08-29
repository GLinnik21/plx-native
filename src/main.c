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
#include "crashfmt.h"  /* the PURE half of the tracer, so ci/test_crashfmt.py can grade it */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <ucontext.h>
#include <unistd.h>
#include <sys/resource.h>
#include <fcntl.h>
#include <sys/stat.h>   /* fchmod — the log sinks are created 0600, see open_log_0600 */
#include <errno.h>      /* the crash handler saves/restores it around its syscalls */

FILE *elogf = NULL;   /* shared event/diagnostic log (extern in app.h); used by the
                       * crash handler here and by the starfish.c seam. Opened "w" each
                       * launch, so it is TRUNCATED on relaunch — do not rely on it to
                       * survive a crash+relaunch (that is what the crash log and `crash_fd` are for). */
/* The crash log has no `FILE *` any more, only the raw `crash_fd` below: its ONLY writer is the
 * signal handler, and stdio is not usable there. See the tracer's comment block. */

extern int plex_run(const char *pms_host, int pms_port);  /* Rust app core (no creds — session or /tmp/plxnative-token) */

/* ---------------------------------------------------------------------------------------------
 * CRASH TRACER — and the whole reason this stays C.
 *
 * It logs the faulting PC/LR, the registers around them, and the /proc/self/maps line(s) that
 * contain them, so triage can say WHICH module faulted (ours, LG's closed libraries, our
 * `dlopen`'d FFmpeg/curl/ACB) before anything is symbolized. `tools/crash-report.sh` and the
 * `crash-triage` skill read exactly what is written below.
 *
 * # It must be async-signal-safe, and until 2026-08-29 it was NOT
 *
 * The previous version called `fprintf`, `fopen`, `fgets`, `sscanf`, `strstr`, `fclose` and
 * `fflush`. None of those is on POSIX's async-signal-safe list, and the reason is exactly the
 * situation this handler runs in: stdio takes a per-stream lock and allocates, so a fault that
 * happened *inside* `malloc` or while another thread held that lock can deadlock or fault again
 * here — losing the report, in the crash class most worth having one for. It "worked" every time
 * it was used because the crashes it was used on were ordinary bad pointers.
 *
 * So everything below is `open`/`read`/`write`/`close` and hand-rolled formatting, all of which
 * ARE on the list. The output is still ASCII and still the same shape: it is read by a shell
 * script, by that skill and by a human, and a binary record would be no safer to write and worse
 * to read. Two static buffers are used and reused — they are BSS, so they cost nothing at boot and
 * cannot fail to be there when the handler needs them, which is the property that matters.
 *
 * # What it does NOT try to do
 *
 * There is no backtrace. `backtrace()` is not async-signal-safe, ARM unwinding out of a signal
 * handler commonly stops at `gsignal()`, and deferring it does not help because by then the stack
 * is gone. Two frames plus the registers plus the faulting module is a FAULT EVENT, not a
 * backtrace, and the honest name is used in the docs for the same reason it is used here.
 *
 * No load-bias arithmetic either: `pkg/plxnative` is `ET_EXEC` (`readelf -h`, `Type: EXEC`), so
 * the PC already IS the link-time address and `crash-report.sh` subtracts the mapping base only
 * to sanity-check that the address falls inside our own text. `dl_iterate_phdr` would answer a
 * question this binary does not ask.
 * ------------------------------------------------------------------------------------------- */
static const char *signame(int sig) {
    switch (sig) {
        case SIGSEGV: return "SIGSEGV";
        case SIGABRT: return "SIGABRT";  /* incl. a Rust panic that crosses an FFI boundary */
        case SIGBUS:  return "SIGBUS";
        case SIGILL:  return "SIGILL";
        case SIGTRAP: return "SIGTRAP";
        default:      return "SIG?";
    }
}

/* The two raw sinks, captured at open time. `fileno()` is not on the safe list and there is no
 * reason to need it: the fds are recorded when the streams are created, before `sigaction` makes
 * the handler reachable, so a signal can never find them half-initialised.
 *
 * `sig_atomic_t` because they are read from signal context; -1 means "not open", and every write
 * below checks. */
static volatile sig_atomic_t crash_fd = -1;   /* plxnative-crash.log — append-only, survives a relaunch */
static volatile sig_atomic_t event_fd = -1;   /* plxnative-events.log — this session only */

/* write(2) until it is all out, or until it stops making progress. Partial writes are real on a
 * file that hit a full filesystem, which is a state a television reaches. */
static void s_write(int fd, const char *p, size_t n) {
    if (fd < 0) return;
    while (n) {
        ssize_t w = write(fd, p, n);
        if (w > 0) { p += (size_t)w; n -= (size_t)w; continue; }
        if (w < 0 && errno == EINTR) continue;
        break;
    }
}

/* Both sinks get every line, so the two logs tell the same story: the event log is where the rest
 * of the session is, and the crash log is the half that survives the relaunch. */
static void emit(const struct plx_sbuf *b) {
    s_write((int)event_fd, b->p, b->n);
    s_write((int)crash_fd, b->p, b->n);
}

/* Static, because signal context has no business allocating and the stack is the one resource a
 * SIGSEGV may have just exhausted. */
static char maps_chunk[4096];
static char maps_line[512];
static char rec_buf[1024];

/* One /proc/self/maps line: emit it as `at:` when it contains the PC or the LR, and as `bin:`
 * when it is our own executable's mapping (which is what gives triage the load base). */
static void emit_map_line(const char *line, size_t n, unsigned long pc, unsigned long lr) {
    int kind = plx_map_line_kind(line, n, pc, lr);
    struct plx_sbuf b = { rec_buf, 0, sizeof rec_buf };
    if (kind & PLX_MAP_AT) {
        plx_s_str(&b, "at: ");
        for (size_t i = 0; i < n; i++) plx_s_ch(&b, line[i]);
        emit(&b);
        b.n = 0;
    }
    if (kind & PLX_MAP_BIN) {
        plx_s_str(&b, "bin: ");
        for (size_t i = 0; i < n; i++) plx_s_ch(&b, line[i]);
        emit(&b);
    }
}

/* Read /proc/self/maps with raw read(2) and scan it a line at a time in place.
 *
 * Chunked rather than slurped: this file is tens of kilobytes on a set with the video pipeline up
 * (the app peaks at 31 threads, each with a stack mapping), it has no size to `stat`, and a static
 * buffer big enough for the worst case is memory this process holds for its whole life to use once
 * at death. A 4 KiB window with a carried partial line costs the same syscalls and is bounded. */
static void scan_maps(unsigned long pc, unsigned long lr) {
    int m = open("/proc/self/maps", O_RDONLY);
    if (m < 0) return;
    size_t held = 0;
    for (;;) {
        ssize_t r = read(m, maps_chunk, sizeof maps_chunk);
        if (r < 0) { if (errno == EINTR) continue; break; }
        if (r == 0) break;
        for (ssize_t i = 0; i < r; i++) {
            char c = maps_chunk[i];
            /* An over-long line is TRUNCATED rather than dropped: the fields that matter (the
             * range, and the path's tail) are worth having even without the middle. */
            if (held < sizeof maps_line) maps_line[held++] = c;
            if (c != '\n') continue;
            emit_map_line(maps_line, held, pc, lr);
            held = 0;
        }
    }
    if (held) emit_map_line(maps_line, held, pc, lr);
    close(m);
}

/* The registers, on their own `reg:` line.
 *
 * A second line rather than more fields on the first, because `crash-report.sh` reads `pc=` and
 * `lr=` off the block with a `head -1` sed and every tool that has ever parsed this file expects
 * the first line's shape. Neither `pc` nor `lr` is repeated here, so there is nothing for that
 * `head -1` to pick up by mistake.
 *
 * Why more than PC and LR at all: with no backtrace, the registers ARE the evidence. On ARM the
 * first four arguments live in r0-r3 at the call, `fp`/`sp` bound the frame a stack scan would
 * walk if two frames ever prove insufficient, and a `cpsr` with the T bit set says the fault was
 * in Thumb code — which changes how the address is read. */
#if defined(__arm__)
static void emit_regs(const struct sigcontext *c) {
    static const char *const NAMES[] = { "r0", "r1", "r2", "r3", "r4", "r5",
                                         "r6", "r7", "r8", "r9", "r10" };
    const unsigned long regs[] = { c->arm_r0, c->arm_r1, c->arm_r2, c->arm_r3,
                                   c->arm_r4, c->arm_r5, c->arm_r6, c->arm_r7,
                                   c->arm_r8, c->arm_r9, c->arm_r10 };
    struct plx_sbuf b = { rec_buf, 0, sizeof rec_buf };
    plx_s_str(&b, "reg: sp=");  plx_s_hex(&b, c->arm_sp);
    plx_s_str(&b, " fp=");      plx_s_hex(&b, c->arm_fp);
    plx_s_str(&b, " ip=");      plx_s_hex(&b, c->arm_ip);
    plx_s_str(&b, " cpsr=");    plx_s_hex(&b, c->arm_cpsr);
    for (unsigned i = 0; i < sizeof regs / sizeof *regs; i++) {
        plx_s_ch(&b, ' ');
        plx_s_str(&b, NAMES[i]);
        plx_s_ch(&b, '=');
        plx_s_hex(&b, regs[i]);
    }
    plx_s_ch(&b, '\n');
    emit(&b);
}
#endif

static void crash_handler(int sig, siginfo_t *si, void *uc) {
    /* Saved and restored around the body: every syscall below can set it, and a handler that
     * returns having clobbered `errno` corrupts whatever the interrupted code was about to read.
     * We do not return — the re-raise kills us — but the rule holds regardless of that. */
    int saved_errno = errno;
    unsigned long pc = 0, lr = 0;
    ucontext_t *c = (ucontext_t *)uc;
#if defined(__arm__)
    pc = (unsigned long)c->uc_mcontext.arm_pc;
    lr = (unsigned long)c->uc_mcontext.arm_lr;
#else
    (void)c;
#endif
    unsigned long addr = si ? (unsigned long)si->si_addr : 0;

    struct plx_sbuf b = { rec_buf, 0, sizeof rec_buf };
    b.n = plx_fmt_signal(rec_buf, sizeof rec_buf, sig, signame(sig), addr, pc, lr);
    emit(&b);
#if defined(__arm__)
    emit_regs(&c->uc_mcontext);
#endif
    scan_maps(pc, lr);

    /* Re-raise with the DEFAULT disposition so the signal actually kills us: the kernel dumps core
     * (where cores are enabled — see the RLIMIT_CORE note below), webOS crashd/librdx captures a
     * full symbolicated backtrace in /var/log/reports/librdx/, and the parent (SAM) sees a real
     * signal crash (WIFSIGNALED) instead of a clean exit. The old `_exit(3)` hid every crash from
     * the system tracer. */
    errno = saved_errno;
    signal(sig, SIG_DFL);
    raise(sig);
    _exit(128 + sig);   /* only reached if raise() somehow returns */
}

static void install_crash_tracer(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = crash_handler;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGABRT, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);
    sigaction(SIGILL, &sa, NULL);    /* UBSan/overflow traps fire SIGILL/SIGTRAP */
    sigaction(SIGTRAP, &sa, NULL);

    /* Ignore SIGPIPE for the whole process. A Rust program gets this from std::rt::init, but
       our main() is C and calls plex_run() directly, so that never runs — leaving the DEFAULT
       disposition (terminate). stream.rs sends the PMS request with flags 0, so a server that
       closes between connect and write (PMS restart, transcoder session reaped, keep-alive
       race) would kill the app outright, with no crash-log line: the tracer above handles only
       SEGV/ABRT/BUS/ILL/TRAP. capture.rs already dodges this per-call with MSG_NOSIGNAL
       ("SIGPIPE would kill the app", capture.rs); this covers every other socket in one line. */
    signal(SIGPIPE, SIG_IGN);

#ifndef PLX_DEBUG
    /* No core dumps in a shipping build — this is the other half of the re-raise above, and
       without it that design is actively hostile to the user's TV. The jail sets RLIMIT_CORE
       to INFINITY and /proc/sys/kernel/core_pattern is the bare string "core", i.e. relative
       to cwd = our own app directory. Measured on the dev TV: a 209,965,056-byte core from a
       single crash, sitting on /dev/mmcblk0p53 — 615.6 MB TOTAL, SHARED with every app the
       user has installed, and nothing on the device ever cleans it. Two crashes fill it.
       webosbrew's repository rule 3 ("be considerate to users' TV") is squarely about this.

       What this costs: the kernel's core file, and only that. The tracer above still writes the
       faulting PC and its /proc/self/maps line to the append-only crash log, which is what
       tools/crash-report.sh symbolizes and what every triage in this project has actually used;
       the re-raise still gives SAM a real WIFSIGNALED exit. `make DEBUG=1` keeps cores (and
       DWARF) for the rare case that wants a full post-mortem. */
    setrlimit(RLIMIT_CORE, &(struct rlimit){ 0, 0 });
#endif
}

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
    (void)argc; (void)argv;
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
    event_fd = open_fd_0600(runtime_path("plxnative-events.log"), O_APPEND);
    crash_fd = open_fd_0600(runtime_path("plxnative-crash.log"), O_APPEND); /* append: keep prior crashes across relaunches */
    /* stderr is REPLACED, so it must go through freopen — but create the file at 0600 first, and
     * freopen's "a" then reuses that inode rather than making a fresh 0644 one. Two calls to
     * runtime_path(), which alternates buffers, so they cannot alias even though the first result
     * is dead by the time the second is taken. */
    { FILE *s = open_log_0600(runtime_path("plxnative-stderr.log"), O_TRUNC); if (s) fclose(s); }
    freopen(runtime_path("plxnative-stderr.log"), "a", stderr); /* capture abort/assert text */
    install_crash_tracer();
    /* request BACK key delivery from the webOS access policy (before SDL init) */
    setenv("SDL_WEBOS_ACCESS_POLICY_KEYS_BACK", "true", 1);
    return plex_run(PMS_HOST, PMS_PORT);
}
