/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository root.
 *
 * crashtrace.c — the fatal-signal tracer's I/O half: the descriptors, `write(2)`, the chunked read
 * of `/proc/self/maps`, and the handler itself. `src/crashfmt.h` holds the pure half (formatting
 * and the maps-line decision), which `ci/crashfmt-test.c` grades on its own.
 *
 * # Why this is a separate translation unit
 *
 * So that the SIGNAL PATH can be tested too. It lifted out of `main.c` on 2026-08-29 for exactly
 * one reason: `ci/crashtrace-test.c` links THIS FILE and nothing else, installs the handler onto a
 * temporary file, faults a child process on purpose and then asserts both halves of the contract —
 * that a record was written, and that the child still died of the original signal, which is what
 * the re-raise buys — a real `WIFSIGNALED` status for SAM, and NOT a crashd backtrace; see the note
 * on the re-raise itself, which measured that. Neither half is decidable from `main.c`, which drags
 * in the whole app.
 *
 * What that host test cannot see is stated where it lives, but the short version belongs here too:
 * on a Mac there is no `/proc/self/maps`, so `scan_maps` returns immediately and the `at:`/`bin:`
 * lines this file exists to produce are never exercised. Their DECISION is graded by the pure test,
 * their PLUMBING only on a television.
 */
#include "crashtrace.h"
#include "crashfmt.h"

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <unistd.h>
/* ARM Linux only. macOS declares `ucontext_t` behind `_XOPEN_SOURCE` and calls the routines
 * deprecated, and the host test has no use for it: the register block below is the only reader and
 * it is already `#if defined(__arm__)`. Including it unconditionally would mean the test had to be
 * compiled with different feature macros from the shipping build, which is how a host test starts
 * grading a different translation than the one that ships. */
#if defined(__arm__)
#include <ucontext.h>
#endif
#include <stddef.h>
#include <string.h>   /* memset, in install only — NOT in the handler */
#include <sys/resource.h>
#include <time.h>     /* time(): async-signal-safe, used by the handler for the record's time: line */
#if defined(__linux__)
#include <sys/prctl.h>
#include <sys/syscall.h>
#endif

/* The process phase, stored by Rust (`plx_base::task::watchdog::plx_crash_phase`, a `#[no_mangle]`
 * static like `plx_freeze_seq`, so this file links without Rust and Rust without this file). The
 * handler only reads it. */
extern volatile unsigned int plx_crash_phase;
/* CLOCK_MONOTONIC at install, for the record's uptime. -1 until `plx_crash_install` ran. */
static volatile long install_mono_secs = -1;

/* ---------------------------------------------------------------------------------------------
 * The tracer proper.
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
        case SIGFPE:  return "SIGFPE";
        case SIGILL:  return "SIGILL";
        case SIGSYS:  return "SIGSYS";
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

/* Static, because signal context has no business allocating and the stack is the one resource a
 * SIGSEGV may have just exhausted.
 *
 * **Two sets, one per handler.** The fatal handler and the freeze probe's handler can both be in
 * flight at once: a freeze record is being written on the main thread while another thread
 * crashes. Sharing one set would let the crash's lines interleave with, or be clobbered by, the
 * freeze's, and the crash is the record that must be whole. */
struct bufs {
    char chunk[4096];
    char line[512];
    char rec[1024];
};
static struct bufs crash_bufs;
static struct bufs freeze_bufs;

/* Set on entry to the fatal handler and never cleared: the process is dying. The freeze probe
 * checks it before every line it writes, because a probe interrupting a thread while ANOTHER
 * thread crashes must not put its lines into the middle of the crash's record. */
static volatile sig_atomic_t crashing;

/* Both sinks get every line, so the two logs tell the same story: the event log is where the rest
 * of the session is, and the crash log is the half that survives the relaunch. A freeze line
 * (written from the freeze buffers) is dropped once a crash has begun. */
static void emit(const struct plx_sbuf *b) {
    if (crashing && b->p == freeze_bufs.rec) return;
    s_write((int)event_fd, b->p, b->n);
    s_write((int)crash_fd, b->p, b->n);
}

/* One /proc/self/maps line: emit it as `at:` when it contains the PC or the LR, and as `bin:`
 * when it is our own executable's mapping (which is what gives triage the load base). */
static void emit_map_line(struct bufs *B, const char *line, size_t n,
                          const unsigned long *addrs, size_t n_addrs) {
    int kind = plx_map_line_kind_n(line, n, addrs, n_addrs);
    struct plx_sbuf b = { B->rec, 0, sizeof B->rec };
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

/* Read a maps file with raw read(2) and scan it a line at a time in place.
 *
 * The PATH is a parameter, and that is the only concession this file makes to being testable: the
 * handler passes `/proc/self/maps`, and `ci/crashtrace-test.c` passes a fixture. It matters because
 * this reader is the newest thing here and its failure is silent — a partial line dropped at a
 * chunk boundary, or a final line without a trailing newline, costs exactly the `bin:` line that
 * every symbolication downstream depends on, and the record still looks well-formed without it.
 * There is no host on which `/proc/self/maps` could exercise those cases deliberately.
 *
 * Chunked rather than slurped: this file is tens of kilobytes on a set with the video pipeline up
 * (the app peaks at 31 threads, each with a stack mapping), it has no size to `stat`, and a static
 * buffer big enough for the worst case is memory this process holds for its whole life to use once
 * at death. A 4 KiB window with a carried partial line costs the same syscalls and is bounded. */

/* Feed every line of the maps-shaped file at `path` to `each(ctx, line, len)`, a chunk at a time.
 * The shared reader under the `at:`/`bin:` scan, the freeze probe's executable-range collection
 * and the install-time stack-bounds read. Async-signal-safe: `open`/`read`/`close` and the
 * caller's static buffers. */
static void for_each_map_line(struct bufs *B, const char *path,
                              void (*each)(void *ctx, const char *line, size_t n), void *ctx) {
    int m = open(path, O_RDONLY);
    if (m < 0) return;
    size_t held = 0;
    for (;;) {
        ssize_t r = read(m, B->chunk, sizeof B->chunk);
        if (r < 0) { if (errno == EINTR) continue; break; }
        if (r == 0) break;
        for (ssize_t i = 0; i < r; i++) {
            char c = B->chunk[i];
            /* An over-long line is TRUNCATED rather than dropped: the fields that matter (the
             * range, and the path's tail) are worth having even without the middle. */
            if (held < sizeof B->line) B->line[held++] = c;
            if (c != '\n') continue;
            each(ctx, B->line, held);
            held = 0;
        }
    }
    if (held) each(ctx, B->line, held);
    close(m);
}

struct at_scan { struct bufs *B; const unsigned long *addrs; size_t n; };

static void at_scan_line(void *ctx, const char *line, size_t n) {
    struct at_scan *a = ctx;
    emit_map_line(a->B, line, n, a->addrs, a->n);
}

static void scan_maps(struct bufs *B, const char *path, const unsigned long *addrs, size_t n) {
    struct at_scan a = { B, addrs, n };
    for_each_map_line(B, path, at_scan_line, &a);
}

void plx_crash_scan_maps_file(const char *path, unsigned long pc, unsigned long lr) {
    const unsigned long addrs[2] = { pc, lr };
    scan_maps(&crash_bufs, path, addrs, 2);
}

void plx_freeze_scan_maps_file(const char *path, const unsigned long *addrs, size_t n) {
    scan_maps(&freeze_bufs, path, addrs, n);
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
static void emit_regs(struct bufs *B, const struct sigcontext *c) {
    static const char *const NAMES[] = { "r0", "r1", "r2", "r3", "r4", "r5",
                                         "r6", "r7", "r8", "r9", "r10" };
    const unsigned long regs[] = { c->arm_r0, c->arm_r1, c->arm_r2, c->arm_r3,
                                   c->arm_r4, c->arm_r5, c->arm_r6, c->arm_r7,
                                   c->arm_r8, c->arm_r9, c->arm_r10 };
    struct plx_sbuf b = { B->rec, 0, sizeof B->rec };
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
    crashing = 1;
    /* Saved and restored around the body: every syscall below can set it, and a handler that
     * returns having clobbered `errno` corrupts whatever the interrupted code was about to read.
     * We do not return — the re-raise kills us — but the rule holds regardless of that. */
    int saved_errno = errno;
    unsigned long pc = 0, lr = 0;
#if defined(__arm__)
    ucontext_t *c = (ucontext_t *)uc;
    pc = (unsigned long)c->uc_mcontext.arm_pc;
    lr = (unsigned long)c->uc_mcontext.arm_lr;
#else
    /* Off the target the fault context is not read at all — see the include guard above. The
     * record still carries `pc=0x0 lr=0x0`, which is what lets `ci/crashtrace-test.c` assert the
     * FIELDS are present without pretending it measured them. */
    (void)uc;
#endif
    unsigned long addr = si ? (unsigned long)si->si_addr : 0;

    struct plx_sbuf b = { crash_bufs.rec, 0, sizeof crash_bufs.rec };
    b.n = plx_fmt_signal(crash_bufs.rec, sizeof crash_bufs.rec, sig, signame(sig), addr, pc, lr);
    emit(&b);
    /* The crash's wall-clock second, on its own line under the signal line (see `plx_fmt_time`).
     * `time()` is on POSIX's async-signal-safe list: it reads the clock and takes no lock and
     * allocates nothing (it was not measured on the television's glibc, which is the one thing a
     * host test cannot show, but nothing in its contract can block). Wall clock, not
     * monotonic, on purpose: this is the time Sentry is told, and monotonic seconds mean nothing
     * to it. A set whose clock is wrong records a wrong time, which the importer's window check
     * (`crashreport::crash_timestamp`) drops when it is absurd; a failed call (-1) writes no line
     * at all rather than a made-up one. */
    time_t now = time(NULL);
    if (now > 0) {
        b.n = plx_fmt_time(crash_bufs.rec, sizeof crash_bufs.rec, (unsigned long)now);
        emit(&b);
    }
    /* Which thread, and how far the process had got. `gettid` and `prctl(PR_GET_NAME)` are plain
     * syscalls and `clock_gettime(CLOCK_MONOTONIC)` is a vDSO read or one, so all of it is
     * async-signal-safe and formats into the same static buffer. Off Linux (the host test on a Mac)
     * there is no gettid: the pid stands in, which also reads as the main thread. */
    {
        unsigned long tid = (unsigned long)getpid();
        char tname[16] = {0};
#if defined(__linux__)
        tid = (unsigned long)syscall(SYS_gettid);
        prctl(PR_GET_NAME, (unsigned long)tname, 0, 0, 0);
#endif
        b.n = plx_fmt_thread(crash_bufs.rec, sizeof crash_bufs.rec, tid, tid == (unsigned long)getpid(), tname);
        emit(&b);
        struct timespec ts;
        long up = 0;
        if (install_mono_secs >= 0 && clock_gettime(CLOCK_MONOTONIC, &ts) == 0 && ts.tv_sec >= install_mono_secs)
            up = (long)ts.tv_sec - install_mono_secs;
        b.n = plx_fmt_phase(crash_bufs.rec, sizeof crash_bufs.rec, plx_crash_phase, (unsigned long)up);
        emit(&b);
    }
#if defined(__arm__)
    emit_regs(&crash_bufs, &c->uc_mcontext);
#endif
    plx_crash_scan_maps_file("/proc/self/maps", pc, lr);

    /* Re-raise with the DEFAULT disposition so the signal actually kills us, and the parent (SAM)
     * sees a real signal crash — `exit_status: 11` for a SIGSEGV, device-verified 2026-08-29 —
     * instead of a clean exit. The old `_exit(3)` hid every crash from the system tracer.
     *
     * **It does NOT get us a crashd backtrace, and this comment used to say it did.** Measured on
     * the dev set: two deliberate SIGSEGVs produced the WIFSIGNALED status above and NO report in
     * `/var/log/reports/librdx/`. The reason is structural — `/proc/sys/kernel/core_pattern` on
     * this firmware is the bare string `core`, i.e. the kernel writes a core FILE into the process
     * cwd and the report chain starts from that file, while `setrlimit(RLIMIT_CORE, 0)` below means
     * no core is ever written. The two are mutually exclusive, and suppressing cores is the right
     * choice: the partition is 615.6 MB shared with every app the user has installed and had
     * 125.9 MB free when this was measured, against a ~200 MB core.
     *
     * (Not proven all the way: writing a core to confirm the chain would have needed ~200 MB that
     * was not there, so the core→report link is inference from `core_pattern` rather than a
     * measurement. What IS measured is that we get no report.)
     *
     * So the honest summary of the always-local/system evidence is this app's OWN fault event plus
     * SAM's status. With consent and a configured endpoint, Sentry Native may additionally preserve
     * a bounded multi-thread report for every successfully captured thread. That is an argument
     * for the tracer being good, not for turning cores back on.
     *
     * # THE UNBLOCK IS THE WHOLE THING, and without it none of the paragraph above was true
     *
     * `sigaction` without `SA_NODEFER` adds the delivered signal to the thread's mask for the
     * duration of the handler. So `raise(sig)` merely marks it PENDING, returns 0, and control
     * falls through to the `_exit` below — which is a CLEAN EXIT with status `128+sig`. No core, no
     * crashd report, and `WIFEXITED` rather than `WIFSIGNALED` for the parent.
     *
     * That is what this code did from the day the re-raise was written (2026-07-09) until
     * 2026-08-29, with the line below commented "only reached if raise() somehow returns" — it was
     * reached EVERY time. The commit that added it was fixing exactly this failure in its earlier
     * form (a bare `_exit(3)` that "hid every crash from the system tracer") and it swapped one
     * clean exit for another, more plausible-looking one. Nothing could have noticed: the tracer's
     * own log looks identical either way, and `128+11 = 139` is a perfectly ordinary-looking
     * status. It was found by `ci/crashtrace-test.c` faulting a process ON PURPOSE and asking the
     * one question no log can answer — *how did it die?*
     *
     * `sigprocmask`, `sigemptyset` and `sigaddset` are all on POSIX's async-signal-safe list.
     * Unblocking HERE rather than passing `SA_NODEFER` at install time is deliberate: with the
     * flag, a fault inside the handler itself would re-enter it and recurse. By this line the
     * record is already written, so there is nothing left to protect. */
    errno = saved_errno;
    signal(sig, SIG_DFL);
    sigset_t one;
    sigemptyset(&one);
    sigaddset(&one, sig);
    sigprocmask(SIG_UNBLOCK, &one, NULL);
    raise(sig);
    _exit(128 + sig);   /* genuinely unreachable now — see above for when it was not */
}

/* ---------------------------------------------------------------------------------------------
 * The freeze probe.
 *
 * The Rust main-loop watchdog (`plx_base::task::watchdog`) sees the main thread make no loop
 * progress for `FREEZE_THRESHOLD_MS` while the app is in the foreground, and sends it
 * `PLX_FREEZE_SIGNAL` with `pthread_kill`. This handler runs ON the frozen thread, writes what that
 * thread was doing as a `*** FREEZE` record (`plx_fmt_freeze`) and RETURNS, so the thread carries on
 * exactly where it was. The process is never terminated here.
 *
 * # Why SIGUSR2
 *
 * Nothing in this repository installs a handler for it, and the pinned sentry-native 0.17.4 source
 * uses no SIGUSR at all: its backends hook the synchronous fault signals, and its one other signal,
 * `SIGRTMIN + 4` for its experimental app-hang walker, is off unless
 * `sentry_options_set_enable_app_hang_tracking` is called, which this app never does. This
 * tracer's own list is SEGV/ABRT/BUS/FPE/ILL/SYS/TRAP, and glibc's NPTL owns real-time signals 32
 * and 33. SDL2 and LG's closed libraries were NOT audited for a SIGUSR2 handler. The Rust side
 * (`probe_main_thread`) refuses to send unless the installed handler is in this very binary
 * (`handler_is_ours`: a `sigaction` query, then `dladdr` on the handler and on its own code,
 * compared by the object they live in), so a library that took the signal after install is never
 * signalled; `si_pid` below and the number Rust arms in `plx_freeze_seq` keep any other SIGUSR2
 * out of the record.
 *
 * # What it does to the interrupted thread
 *
 * `SA_RESTART` restarts what the kernel may restart, but `poll`, `epoll_wait`, `nanosleep` and
 * `futex` waits with a timeout return `EINTR` regardless. A frozen main thread is usually IN one
 * of those, so the probe can make one blocking call in LG's closed code return early once per
 * freeze. The Rust side bounds that: at most `MAX_FREEZE_PROBES` per process.
 *
 * # Async-signal-safety, and the ways this could turn a freeze into a crash
 *
 * Only `open`/`read`/`write`/`close`/`time`/`getpid` and hand formatting, as in the fatal handler,
 * with their own buffers (a crash on another thread must not share them). Every function is called
 * once at install so no lazy-binding resolver runs in the handler: a thread frozen inside `dlopen`
 * holds the loader lock, and a first-use resolve here would wait on it forever. The frame walk
 * dereferences only `[sp, main_stack_hi)` (see `plx_fp_walk`), the bound being checked rather than
 * the pointer trusted. A probe from any other process is ignored (`si_pid`).
 * ------------------------------------------------------------------------------------------- */
static volatile int freeze_busy;      /* one freeze record at a time */
static volatile int freeze_written;   /* records started this process, against the backstop */
static unsigned long main_stack_hi;   /* end of the main thread's [stack], 0 = unknown, no walk */
static struct plx_range exec_ranges[PLX_MAX_EXEC_RANGES];
static size_t n_exec_ranges;

static void collect_exec_range(void *ctx, const char *line, size_t n) {
    (void)ctx;
    if (n_exec_ranges < PLX_MAX_EXEC_RANGES &&
        plx_map_line_exec(line, n, &exec_ranges[n_exec_ranges]))
        n_exec_ranges++;
}

static void find_stack_end(void *ctx, const char *line, size_t n) {
    unsigned long hi;
    if (plx_map_line_stack_end(line, n, *(unsigned long *)ctx, &hi)) main_stack_hi = hi;
}

/* The deepest the main thread's live stack may be for the walk to trust `[sp, hi)`: the main
 * thread's stack rlimit is 8 MiB on this firmware, so a gap wider than this means `sp` is not on
 * the stack `hi` belongs to, and the span between them may be unmapped. */
#define PLX_MAX_WALK_SPAN (8ul * 1024 * 1024)

static void freeze_handler(int sig, siginfo_t *si, void *uc) {
    (void)sig;
#ifdef __linux__
    /* A probe is `pthread_kill` from this process, which Linux reports as our own pid. A signal
     * sent by another process (or by `kill` from a shell) is not ours to answer. */
    if (!si || si->si_pid != getpid()) return;
#else
    (void)si;
#endif
    /* Rust arms the probe's number in `plx_freeze_seq` right before it signals, and it is taken
     * back to 0 here whatever happens next, so one arming answers one signal. A SIGUSR2 that finds
     * none is not a probe (nothing of ours sent it): it records nothing, and the record numbers
     * stay Rust's. */
    unsigned int seq = __sync_lock_test_and_set(&plx_freeze_seq, 0);
    /* A probe that lands while a fatal-signal handler is running (Sentry's native backend waits up
     * to 10 s for its daemon, in the crashing thread) would write a FREEZE record into the middle
     * of a crash. Neither that handler nor ours uses SA_NODEFER, so the fatal signal stays in the
     * blocked set for as long as one runs; `uc_sigmask` is the mask the probe interrupted. */
    {
        static const int fatal[] = {SIGSEGV, SIGABRT, SIGBUS, SIGFPE, SIGILL, SIGSYS, SIGTRAP};
        const sigset_t *interrupted = &((const ucontext_t *)uc)->uc_sigmask;
        for (size_t i = 0; i < sizeof fatal / sizeof fatal[0]; i++)
            if (sigismember(interrupted, fatal[i]) == 1) return;
    }
    if (crashing) return;
    if (__sync_lock_test_and_set(&freeze_busy, 1)) return;
    if (!seq) { __sync_lock_release(&freeze_busy); return; }
    int saved_errno = errno;
    if (++freeze_written <= PLX_MAX_FREEZE_RECORDS) {
        unsigned long pc = 0, lr = 0, sp = 0, fp = 0;
#if defined(__arm__)
        ucontext_t *c = (ucontext_t *)uc;
        pc = (unsigned long)c->uc_mcontext.arm_pc;
        lr = (unsigned long)c->uc_mcontext.arm_lr;
        sp = (unsigned long)c->uc_mcontext.arm_sp;
        fp = (unsigned long)c->uc_mcontext.arm_fp;
#else
        (void)uc;   /* off the target the interrupted context is not read; see the fatal handler */
#endif
        struct bufs *B = &freeze_bufs;
        struct plx_sbuf b = { B->rec, 0, sizeof B->rec };
        b.n = plx_fmt_freeze(B->rec, sizeof B->rec, (unsigned long)seq, pc, lr);
        emit(&b);
        time_t now = time(NULL);
        if (now > 0) {
            b.n = plx_fmt_time(B->rec, sizeof B->rec, (unsigned long)now);
            emit(&b);
        }
#if defined(__arm__)
        emit_regs(B, &c->uc_mcontext);
#endif
        unsigned long addrs[PLX_MAX_FREEZE_ADDRS];
        size_t na = 0, nf = 0;
        addrs[na++] = pc;
        addrs[na++] = lr;
        if (sp && fp && main_stack_hi && sp < main_stack_hi &&
            main_stack_hi - sp <= PLX_MAX_WALK_SPAN) {
            n_exec_ranges = 0;
            for_each_map_line(B, "/proc/self/maps", collect_exec_range, NULL);
            nf = plx_fp_walk(sp, fp, main_stack_hi, exec_ranges, n_exec_ranges,
                             addrs + na, PLX_MAX_FREEZE_FRAMES);
            for (size_t i = 0; i < nf; i += 8) {
                size_t len = 0;
                plx_fmt_frames(B->rec, sizeof B->rec, addrs + na, nf, i, 8, &len);
                b.n = len;
                emit(&b);
            }
            na += nf;
        }
        scan_maps(B, "/proc/self/maps", addrs, na);
    }
    errno = saved_errno;
    __sync_lock_release(&freeze_busy);
}

void plx_crash_install(int ev_fd, int cr_fd) {
    struct sigaction sa;
    /* The sinks first, THEN `sigaction`. Ordering, not style: after the last line of this function
     * a signal can arrive at any instant, and the handler must find descriptors it can write to
     * rather than having to open one — which is the thing a fault inside the allocator makes
     * impossible. */
    event_fd = ev_fd;
    crash_fd = cr_fd;
    /* Bind `time` now. The libc symbols are resolved lazily (the link has no `-z now`), so the
     * handler's first call would otherwise run the dynamic linker's resolver in the middle of a
     * crash. One call here resolves it at startup; the handler's own use is unchanged and
     * `time()` stays async-signal-safe. */
    (void)time(NULL);
    {
        struct timespec ts;
        if (clock_gettime(CLOCK_MONOTONIC, &ts) == 0) install_mono_secs = (long)ts.tv_sec;
#if defined(__linux__)
        (void)syscall(SYS_gettid);
        char nm[16];
        prctl(PR_GET_NAME, (unsigned long)nm, 0, 0, 0);   /* bind both before a fault needs them */
#endif
    }
    /* The freeze probe's own first-use bindings, for the reason above and one more: it runs on a
     * thread that is by definition stuck, possibly inside `dlopen` holding the loader lock. Each
     * libc entry it uses is called once here, and the stack bound is read while this is still the
     * main thread's own frame (`&sa` is on its stack). */
    (void)getpid();
    (void)sigismember(&(sigset_t){0}, SIGSEGV);
    if (write(-1, "", 0) < 0) { /* EBADF is the point: the symbol is bound, nothing was written */ }
    {
        unsigned long here = (unsigned long)&sa;
        for_each_map_line(&freeze_bufs, "/proc/self/maps", find_stack_end, &here);
    }
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = freeze_handler;
    sa.sa_flags = SA_SIGINFO | SA_RESTART;
    {
        /* A library constructor that already took SIGUSR2 owns it: decline rather than replace it
         * (the watchdog also refuses to probe a handler that is not this binary's own). The two
         * dispositions nobody owns are replaced: the default, and `SIG_IGN`, which a parent that
         * ignored the signal hands down across `exec` and which would swallow every probe. */
        struct sigaction prior;
        if (sigaction(SIGUSR2, NULL, &prior) == 0 &&
            (prior.sa_handler == SIG_DFL || prior.sa_handler == SIG_IGN))
            sigaction(SIGUSR2, &sa, NULL);
    }
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = crash_handler;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGABRT, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);
    sigaction(SIGFPE, &sa, NULL);
    sigaction(SIGILL, &sa, NULL);    /* UBSan/overflow traps fire SIGILL/SIGTRAP */
    sigaction(SIGSYS, &sa, NULL);
    sigaction(SIGTRAP, &sa, NULL);

    /* Ignore SIGPIPE for the whole process. A Rust program gets this from std::rt::init, but
       our main() is C and calls plex_run() directly, so that never runs — leaving the DEFAULT
       disposition (terminate). stream.rs sends the PMS request with flags 0, so a server that
       closes between connect and write (PMS restart, transcoder session reaped, keep-alive
       race) would kill the app outright, with no crash-log line: the tracer above handles only
       SEGV/ABRT/BUS/FPE/ILL/SYS/TRAP. capture.rs already dodges this per-call with MSG_NOSIGNAL
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
