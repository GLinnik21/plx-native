/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under the MIT Licence; see LICENSE at the repository root.
 *
 * crashtrace-test.c — **actually crash a process, on purpose, and read what the tracer wrote.**
 * Built and run by `make check` with the HOST compiler, linking `src/crashtrace.c` and nothing
 * else. It needs no television, no NDK and no SDL.
 *
 * # What this grades that `ci/crashfmt-test.c` cannot
 *
 * That one tests pure functions: given this string, what does the tracer DECIDE. This one tests
 * the signal path itself, which is the half that has never been proven by anything but use:
 *
 *   1. a real fatal signal reaches the handler at all;
 *   2. a record is actually WRITTEN, to both descriptors, in the shape the tools parse;
 *   3. the process still DIES OF THE ORIGINAL SIGNAL. That is the re-raise to `SIG_DFL`, and what
 *      it buys is SAM seeing a `WIFSIGNALED` exit — NOT a crashd backtrace, which this firmware
 *      never produces for us: `core_pattern` is the bare string `core` and `RLIMIT_CORE` is 0, so
 *      no core is written and the report chain never starts (measured, twice, with deliberate
 *      SIGSEGVs and an empty `/var/log/reports/librdx/`). This comment claimed the backtrace as its
 *      own rationale, which made an empty librdx directory read as a regression rather than as the
 *      expected result. The status has no other witness: a handler that quietly `_exit`ed would
 *      look identical in the log — which is exactly what this app did for seven weeks, unnoticed.
 *   4. a genuine memory fault, not just `raise()`. The null write below is the real thing: the
 *      kernel raises SIGSEGV from a faulting instruction, so the PC in the record is a real
 *      faulting PC and `si_addr` is the address that could not be touched.
 *
 * Each case runs in a FORKED CHILD, because the subject of the test is a process that dies. The
 * parent reads the exit status and the file afterwards.
 *
 * # What it CANNOT see, stated plainly
 *
 * **`/proc/self/maps` does not exist on macOS**, so `scan_maps` returns immediately and the
 * `at:`/`bin:` lines are never produced here. Their DECISION is graded by the pure test; their
 * plumbing is graded only on a television, and the assertions below are conditional on the file
 * existing so this test tells the truth on either host.
 *
 * The registers are likewise Linux/ARM-only (`#if defined(__arm__)` in the tracer), so no `reg:`
 * line appears on this Mac. This test asserts the lines that DO cross both platforms and says so
 * rather than pretending.
 */
#include "crashtrace.h"

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static int failures = 0;

static void fail(const char *what, const char *detail) {
    fprintf(stderr, "FAIL %s: %s\n", what, detail);
    failures++;
}

static void expect(const char *what, int cond, const char *detail) {
    if (!cond) fail(what, detail);
}

/* How the child is made to die. `FAULT` is the important one — a genuine memory fault rather than
 * a self-sent signal, so the handler sees a real faulting PC. */
enum how { FAULT, RAISE };

/* Fork, install the tracer onto `path`, die the requested way, and return the child's wait status.
 *
 * The child re-opens the sink itself: descriptors survive `fork` and the parent must not hold a
 * writable one, or the two would interleave. */
static int crash_child(const char *path, int sig, enum how how) {
    pid_t pid = fork();
    if (pid < 0) {
        fail("fork", strerror(errno));
        return -1;
    }
    if (pid == 0) {
        int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_APPEND, 0600);
        /* BOTH sinks, deliberately the same file: the tracer writes each line to both, so one file
         * receiving the record twice also proves neither descriptor is being dropped. */
        plx_crash_install(fd, fd);
        if (how == FAULT) {
            /* A real null dereference. `volatile` so no compiler decides this is undefined and
             * therefore deletable — which they do, and then the test would prove nothing. */
            volatile int *p = (volatile int *)0;
            *p = 1;
        } else {
            raise(sig);
        }
        /* Only reached if the signal did not kill us — which is itself the failure, and 97 is a
         * value no signal death can produce. */
        _exit(97);
    }
    int status = 0;
    while (waitpid(pid, &status, 0) < 0) { /* EINTR */ }
    return status;
}

static char *slurp(const char *path, size_t *len) {
    static char buf[64 * 1024];
    int fd = open(path, O_RDONLY);
    if (fd < 0) { *len = 0; buf[0] = 0; return buf; }
    ssize_t n = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (n < 0) n = 0;
    buf[n] = 0;
    *len = (size_t)n;
    return buf;
}

/* How many times `needle` occurs in `hay`. */
static int count(const char *hay, const char *needle) {
    int n = 0;
    size_t l = strlen(needle);
    for (const char *p = hay; (p = strstr(p, needle)) != NULL; p += l) n++;
    return n;
}

static void one_case(const char *name, int sig, const char *signame, enum how how) {
    char path[256];
    snprintf(path, sizeof path, "/tmp/plx-crashtrace-test-%d-%d.log", (int)getpid(), sig);
    unlink(path);

    int status = crash_child(path, sig, how);

    /* (3) THE RE-RAISE. The child must have died OF THE SIGNAL, not exited. */
    if (WIFEXITED(status)) {
        char d[128];
        snprintf(d, sizeof d, "child exited %d instead of dying of %s — the re-raise is gone",
                 WEXITSTATUS(status), signame);
        fail(name, d);
    } else {
        expect(name, WIFSIGNALED(status), "child neither exited nor was signalled");
        if (WIFSIGNALED(status) && WTERMSIG(status) != sig) {
            char d[128];
            snprintf(d, sizeof d, "child died of signal %d, expected %d (%s)",
                     WTERMSIG(status), sig, signame);
            fail(name, d);
        }
    }

    /* (2) THE RECORD. Written twice, once per descriptor. */
    size_t len = 0;
    const char *log = slurp(path, &len);
    char want[64];
    snprintf(want, sizeof want, "*** SIGNAL %d (%s) addr=0x", sig, signame);
    int n = count(log, want);
    if (n != 2) {
        char d[512];
        snprintf(d, sizeof d, "expected the record on BOTH descriptors (2 copies of \"%s\"), got %d.\n"
                              "  log was: %.300s", want, n, log);
        fail(name, d);
    }
    expect(name, count(log, " pc=0x") == 2, "no pc= field");
    expect(name, count(log, " lr=0x") == 2, "no lr= field");

    /* (1)+(4) A REAL FAULT carries a real address. `raise()` cannot: the kernel fills `si_addr`
     * only for a genuine memory fault, so this assertion is scoped to the case that produces one. */
    if (how == FAULT) {
        expect(name, count(log, "(SIGSEGV) addr=0x0 ") == 2,
               "a null dereference should report addr=0x0 — si_addr did not reach the record");
    }

    /* The `at:`/`bin:` lines need /proc/self/maps, which macOS does not have. Assert them where
     * they are producible and say nothing where they are not, rather than skipping silently in
     * both directions. */
    if (access("/proc/self/maps", R_OK) == 0) {
        expect(name, count(log, "\nbin: ") >= 2, "no bin: line — the maps scan produced no load base");
    }

    unlink(path);
}

/* ---- the maps scan, against fixtures -----------------------------------------------------------
 *
 * The reader is chunked — 4 KiB at a time with a carried partial line — and every way that breaks
 * is SILENT: a line lost at a chunk boundary, or a final line with no trailing newline, costs the
 * `bin:` line, and the record still looks perfectly well-formed without it while every
 * symbolication downstream fails. `/proc/self/maps` cannot be made to have those shapes on purpose,
 * and on this Mac it does not exist at all, which is why the scan takes its path as a parameter.
 */

/* Run the scan over `content` and return what was written.
 *
 * Assertions below count `"at: "` and `"bin: "` WITHOUT a leading newline, because the first line
 * of a scan has nothing before it — counting `"\nat: "` silently misses it, which is how the first
 * version of these cases failed against perfectly correct code. Neither string can occur inside a
 * mapping path in these fixtures. */
static const char *scan(const char *content, unsigned long pc, unsigned long lr) {
    static char out[64 * 1024];
    char maps[256], sink[256];
    snprintf(maps, sizeof maps, "/tmp/plx-crashtrace-maps-%d", (int)getpid());
    snprintf(sink, sizeof sink, "/tmp/plx-crashtrace-sink-%d", (int)getpid());

    int mf = open(maps, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    (void)!write(mf, content, strlen(content));
    close(mf);

    int sf = open(sink, O_WRONLY | O_CREAT | O_TRUNC | O_APPEND, 0600);
    /* One sink, not two: the duplicate-write property is already asserted by the crash cases, and
     * counting occurrences here would double every number for no extra evidence. */
    plx_crash_install(sf, -1);
    plx_crash_scan_maps_file(maps, pc, lr);
    close(sf);

    size_t len = 0;
    const char *got = slurp(sink, &len);
    memcpy(out, got, len < sizeof out ? len + 1 : sizeof out - 1);
    out[sizeof out - 1] = 0;
    unlink(maps);
    unlink(sink);
    return out;
}

/* A maps line of the given size, for the mapping named. `pad` inflates the pathname so a fixture
 * can be pushed past a chunk boundary. */
static void map_line(char *out, size_t cap, const char *lo, const char *hi, const char *path, int pad) {
    char pads[4096];
    int n = pad < (int)sizeof pads - 1 ? pad : (int)sizeof pads - 1;
    memset(pads, 'x', (size_t)n);
    pads[n] = 0;
    snprintf(out, cap, "%s-%s r-xp 00000000 b3:35 12345 /media/developer/apps/%s%s\n", lo, hi, pads, path);
}

static const char *OURS = "com.beb.plxnative/plxnative";

static void maps_cases(void) {
    /* The ordinary case, first: one of our own mappings containing the PC. It must produce BOTH
     * lines from the one mapping — `at:` because it holds the fault and `bin:` because it is us. */
    {
        char line[512];
        map_line(line, sizeof line, "00010000", "0097f000", OURS, 0);
        const char *got = scan(line, 0x20000, 0);
        expect("maps: our mapping holding the pc", count(got, "at: ") == 1, "no at: line");
        expect("maps: our mapping holding the pc", count(got, "bin: ") == 1, "no bin: line");
    }

    /* **A LINE STRADDLING THE 4 KiB CHUNK BOUNDARY.** The whole reason the reader carries a partial
     * line. Padding pushes our own mapping so it begins before the boundary and ends after it; a
     * reader that restarted per chunk would lose it entirely and the crash would be unsymbolicatable
     * with nothing in the record to say why. */
    {
        static char buf[16 * 1024];
        size_t n = 0;
        /* filler mappings up to just short of 4096 bytes */
        while (n < 4096 - 60) {
            char line[512];
            map_line(line, sizeof line, "b5000000", "b5001000", "other/libfiller.so", 0);
            size_t l = strlen(line);
            if (n + l > sizeof buf - 600) break;
            memcpy(buf + n, line, l);
            n += l;
        }
        char ours[512];
        map_line(ours, sizeof ours, "00010000", "0097f000", OURS, 0);
        memcpy(buf + n, ours, strlen(ours));
        n += strlen(ours);
        buf[n] = 0;
        /* the boundary really is inside our line */
        expect("maps: fixture actually straddles a chunk", n > 4096 && n - strlen(ours) < 4096,
               "the fixture does not cross 4096 — the test would prove nothing");
        const char *got = scan(buf, 0x20000, 0);
        expect("maps: a line straddling the 4 KiB boundary survives", count(got, "bin: ") == 1,
               "the bin: line was lost at the chunk boundary");
        expect("maps: a line straddling the 4 KiB boundary survives", count(got, "at: ") == 1,
               "the at: line was lost at the chunk boundary");
    }

    /* A final line with NO trailing newline. The kernel always terminates them, but a truncated
     * read does not, and the reader has an explicit tail flush for it. */
    {
        char line[512];
        map_line(line, sizeof line, "00010000", "0097f000", OURS, 0);
        line[strlen(line) - 1] = 0;   /* drop the \n */
        const char *got = scan(line, 0x20000, 0);
        expect("maps: a final line with no newline is still emitted", count(got, "bin: ") == 1,
               "the unterminated tail was dropped");
    }

    /* A line LONGER than the 512-byte line buffer is truncated, not dropped: the range at the
     * front is the part that decides `at:`, and losing the whole line would lose that too. */
    {
        char line[4096];
        map_line(line, sizeof line, "b5000000", "b5240000", "other/libhuge.so", 900);
        const char *got = scan(line, 0xb5100000UL, 0);
        expect("maps: an over-long line is truncated, not dropped", count(got, "at: ") == 1,
               "a line longer than the buffer lost its at: classification");
    }

    /* A mapping that is neither ours nor holding the fault says nothing at all. */
    {
        char line[512];
        map_line(line, sizeof line, "b5000000", "b5240000", "other/libidle.so", 0);
        const char *got = scan(line, 0x20000, 0);
        expect("maps: an unrelated mapping is silent", got[0] == 0, "it emitted something");
    }

    /* An absent file is not a crash inside the crash handler. */
    {
        char sink[256];
        snprintf(sink, sizeof sink, "/tmp/plx-crashtrace-sink-none-%d", (int)getpid());
        int sf = open(sink, O_WRONLY | O_CREAT | O_TRUNC | O_APPEND, 0600);
        plx_crash_install(sf, -1);
        plx_crash_scan_maps_file("/nonexistent/maps", 0x20000, 0);
        close(sf);
        size_t len = 0;
        slurp(sink, &len);
        expect("maps: an unreadable file writes nothing and does not fault", len == 0, "it wrote something");
        unlink(sink);
    }
}

int main(void) {
    /* A genuine memory fault first: it is the only case that exercises the kernel-raised path, a
     * real faulting PC and a real si_addr. */
    one_case("SIGSEGV (real null dereference)", SIGSEGV, "SIGSEGV", FAULT);

    /* Then every other signal the tracer arms, by `raise`. These prove the handler is installed on
     * each one and that each re-raises — `sigaction` is called five times and a missing line there
     * would be invisible until the day that signal actually fired. */
    one_case("SIGABRT", SIGABRT, "SIGABRT", RAISE);
    one_case("SIGBUS",  SIGBUS,  "SIGBUS",  RAISE);
    one_case("SIGILL",  SIGILL,  "SIGILL",  RAISE);
    one_case("SIGTRAP", SIGTRAP, "SIGTRAP", RAISE);

    /* …and then the maps scan, which the crashes above cannot reach on a host with no /proc. */
    maps_cases();

    if (failures) {
        fprintf(stderr, "crashtrace: %d assertion(s) failed\n", failures);
        return 1;
    }
    printf("crashtrace: ok (5 deliberate crashes re-raised; maps scan graded against fixtures)\n");
    return 0;
}
