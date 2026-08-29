/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under the MIT Licence; see LICENSE at the repository root.
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
void plx_crash_scan_maps_file(const char *path, unsigned long pc, unsigned long lr) {
    int m = open(path, O_RDONLY);
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

    struct plx_sbuf b = { rec_buf, 0, sizeof rec_buf };
    b.n = plx_fmt_signal(rec_buf, sizeof rec_buf, sig, signame(sig), addr, pc, lr);
    emit(&b);
#if defined(__arm__)
    emit_regs(&c->uc_mcontext);
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
     * So the honest summary is that this app's crash evidence is its OWN fault event, plus SAM's
     * status. That is an argument for the tracer being good, not for turning cores back on.
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

void plx_crash_install(int ev_fd, int cr_fd) {
    struct sigaction sa;
    /* The sinks first, THEN `sigaction`. Ordering, not style: after the last line of this function
     * a signal can arrive at any instant, and the handler must find descriptors it can write to
     * rather than having to open one — which is the thing a fault inside the allocator makes
     * impossible. */
    event_fd = ev_fd;
    crash_fd = cr_fd;
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

