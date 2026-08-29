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
 *   3. the process still DIES OF THE ORIGINAL SIGNAL. That is the re-raise to `SIG_DFL`, and it is
 *      the whole reason webOS's crashd/librdx still captures a real backtrace and SAM still sees a
 *      `WIFSIGNALED` exit. It has no other witness: a handler that quietly `_exit`ed would look
 *      identical in the log and would silently disable the platform's own crash reporting — which
 *      is exactly what this app did before the re-raise was added, and nothing noticed.
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

    if (failures) {
        fprintf(stderr, "crashtrace: %d assertion(s) failed\n", failures);
        return 1;
    }
    printf("crashtrace: ok (5 deliberate crashes, records written, all re-raised)\n");
    return 0;
}
