/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository root.
 *
 * exit-interpose-test.c — **does an `exit()` made by a shared library, on a thread of its own, end
 * the process through `src/exit.c` with the caller's real status, and leave the record that names
 * the library?** Built and run by `make check` (`check-c-unit`) with the HOST compiler on Linux; it
 * needs no television, no NDK and no Rust.
 *
 * # What it grades
 *
 * `src/exit.c` defines libc `exit` in the executable and the link exports it, so that the `exit` of
 * a firmware library binds there (Sentry PLX-NATIVE-1P: an exit made on SDL's own `SDLTimer`
 * thread ran an exit handler that crashed). It hands the status and its return address to
 * `plx_exit_record` in `src/crashtrace.c`, which writes an async-signal-safe record to the event log
 * and to the crash log, and ends the process. This test makes that call for real, with the REAL `src/crashtrace.c` (only
 * what Rust owns, `plx_crash_phase` and `plx_freeze_seq`, is defined below):
 *
 *   1. the parent creates two temporary files, the event log and the crash log;
 *   2. a forked child installs the crash tracer onto them (`plx_crash_install`), `dlopen`s
 *      `ci/exit-interpose-lib.c`'s object BY PATH (argv[1]), and calls its function from a SECOND
 *      thread named `exit-caller` (the SDLTimer shape), which registers a finalizer that would
 *      `_exit(42)` and then calls `exit(7)` through its own PLT;
 *   3. the parent asserts how the child ended and what it wrote (argv[2] is the mode).
 *
 * Built twice by the Makefile. In `interposed` mode `src/exit.c` is linked and the child must end
 * with 7: the caller's real status survived and the finalizer never ran. BOTH logs must hold the
 * `*** EXIT status=7 caller=0x` header, a `time:` line, a `thread:` line naming `exit-caller`, and
 * an `at:` line naming `libexitcaller.so`, the object that called `exit`, which is the return
 * address resolved through `/proc/self/maps` with no `dladdr`. The event log is this session's
 * story; the crash log is the file the next launch's importer reads (`crashreport::parse`) and
 * turns into an error report of its own kind, so the record has to be there, whole. Neither log
 * holds a `bin:` line: an exit names no load base. In `control` mode `src/exit.c` is NOT linked
 * (`src/crashtrace.c` still is) and the same program must end with 42, because libc's `exit` runs
 * the finalizer: that control is what proves this test can fail, and that it is `src/exit.c` that
 * changes the result.
 *
 * # What it cannot see
 *
 * Whether the shipped ARM binary exports `exit` is `ci/check-elf.sh`'s question, asked of the
 * artifact; whether the Thumb-bit handling is right is only a television's (the host is not ARM).
 * Linux only: Darwin's two-level namespace binds a dylib's `exit` to libSystem whatever the
 * executable defines, and there is no `/proc/self/maps`, so the Makefile skips this test there.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE /* pthread_setname_np */
#endif
#include "crashtrace.h"

#include <dlfcn.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* What Rust owns (`plx_base::task::watchdog`) and `src/crashtrace.c` only reads. */
volatile unsigned int plx_freeze_seq;
volatile unsigned int plx_crash_phase;

/* The status the library's thread passes to `exit`, and so the one an interposed `exit` must keep. */
#define CALLER_STATUS 7
_Static_assert(CALLER_STATUS == 7, "the header the parent looks for spells the status");

/* The object `ci/exit-interpose-lib.c` is built as: the one whose PLT `exit` is called through. */
#define CALLER_OBJECT "libexitcaller.so"

/* The name the calling thread gives itself, which the record's `thread:` line must carry. */
#define CALLER_THREAD "exit-caller"

static void (*library_entry)(int);

/* The "SDLTimer" thread: the library's function never returns. */
static void *second_thread(void *unused) {
    (void)unused;
#if defined(__linux__)
    /* Darwin's `pthread_setname_np` takes the name alone; the test runs on Linux only, and this
     * keeps a Mac's `cc -fsyntax-only` over the file honest. */
    pthread_setname_np(pthread_self(), CALLER_THREAD);
#endif
    library_entry(CALLER_STATUS);
    return NULL;
}

static void run_child(const char *library, int event_fd, int crash_fd) __attribute__((noreturn));
static void run_child(const char *library, int event_fd, int crash_fd) {
    alarm(30); /* a hung exit path fails the test instead of the build */
    plx_crash_install(event_fd, crash_fd);
    void *handle = dlopen(library, RTLD_NOW);
    if (!handle) {
        fprintf(stderr, "dlopen %s: %s\n", library, dlerror());
        _exit(98);
    }
    library_entry = (void (*)(int))dlsym(handle, "exit_from_library");
    if (!library_entry) {
        fprintf(stderr, "dlsym exit_from_library: %s\n", dlerror());
        _exit(98);
    }
    pthread_t thread;
    if (pthread_create(&thread, NULL, second_thread, NULL) != 0) {
        fprintf(stderr, "pthread_create failed\n");
        _exit(98);
    }
    pthread_join(thread, NULL);
    fprintf(stderr, "the library's exit() returned\n");
    _exit(97);
}

/* A temporary file in $TMPDIR (or /tmp): its path in `path`, its descriptor returned, -1 on error. */
static int make_temp(char *path, size_t cap, const char *what) {
    const char *dir = getenv("TMPDIR");
    snprintf(path, cap, "%s/plx-exit-%s.XXXXXX", dir && *dir ? dir : "/tmp", what);
    return mkstemp(path);
}

/* The whole of the file at `path` as a NUL-terminated string in `buf`; its length. */
static size_t slurp(const char *path, char *buf, size_t cap) {
    FILE *f = fopen(path, "rb");
    size_t n = 0;
    if (f) {
        n = fread(buf, 1, cap - 1, f);
        fclose(f);
    }
    buf[n] = 0;
    return n;
}

/* Whether some line of `text` starts with `prefix` and also contains `needle`. */
static int has_line(const char *text, const char *prefix, const char *needle) {
    size_t prefix_len = strlen(prefix);
    for (const char *line = text; *line;) {
        const char *end = strchr(line, '\n');
        size_t len = end ? (size_t)(end - line) : strlen(line);
        if (len >= prefix_len && strncmp(line, prefix, prefix_len) == 0) {
            char copy[1024];
            if (len >= sizeof copy) len = sizeof copy - 1;
            memcpy(copy, line, len);
            copy[len] = 0;
            if (strstr(copy, needle)) return 1;
        }
        if (!end) break;
        line = end + 1;
    }
    return 0;
}

static int fail(const char *what, const char *log) {
    fprintf(stderr, "FAIL: %s\n--- the log in question ---\n%s--- end ---\n", what, log);
    return 1;
}

int main(int argc, char **argv) {
    if (argc != 3 || (strcmp(argv[2], "interposed") != 0 && strcmp(argv[2], "control") != 0)) {
        fprintf(stderr, "usage: %s <library.so> <interposed|control>\n", argv[0]);
        return 2;
    }
    int interposed = strcmp(argv[2], "interposed") == 0;
    int expected = interposed ? CALLER_STATUS : 42;

    char event_path[512], crash_path[512];
    int event_fd = make_temp(event_path, sizeof event_path, "events");
    int crash_fd = make_temp(crash_path, sizeof crash_path, "crash");
    if (event_fd < 0 || crash_fd < 0) {
        perror("mkstemp");
        return 2;
    }

    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return 2;
    }
    if (pid == 0) {
        run_child(argv[1], event_fd, crash_fd);
    }

    int status = 0;
    if (waitpid(pid, &status, 0) != pid) {
        perror("waitpid");
        return 2;
    }
    close(event_fd);
    close(crash_fd);

    char events[16384], crash[16384];
    slurp(event_path, events, sizeof events);
    size_t crash_len = slurp(crash_path, crash, sizeof crash);
    unlink(event_path);
    unlink(crash_path);

    if (!WIFEXITED(status) || WEXITSTATUS(status) != expected) {
        if (WIFEXITED(status)) {
            fprintf(stderr, "FAIL: the child exited with status %d, expected %d (7: interposed, 42: libc's exit ran the finalizer, 97: exit returned, 98: setup failed)\n",
                    WEXITSTATUS(status), expected);
        } else {
            fprintf(stderr, "FAIL: the child did not exit normally (wait status 0x%x), expected status %d\n", status, expected);
        }
        return fail("wrong end", events);
    }

    if (interposed) {
        if (!strstr(events, "*** EXIT status=7 caller=0x"))
            return fail("no `*** EXIT status=7 caller=0x` header in the event log", events);
        if (!has_line(events, "time: ", ""))
            return fail("no `time:` line in the event log", events);
        if (!has_line(events, "thread: ", "name=" CALLER_THREAD))
            return fail("no `thread:` line naming " CALLER_THREAD, events);
        if (!has_line(events, "at: ", CALLER_OBJECT))
            return fail("no `at:` line naming " CALLER_OBJECT, events);
        /* The crash log: the same record. This is what the next launch reports. */
        if (!strstr(crash, "*** EXIT status=7 caller=0x"))
            return fail("no `*** EXIT status=7 caller=0x` header in the crash log", crash);
        if (!has_line(crash, "time: ", ""))
            return fail("no `time:` line in the crash log", crash);
        if (!has_line(crash, "thread: ", "name=" CALLER_THREAD))
            return fail("no `thread:` line naming " CALLER_THREAD " in the crash log", crash);
        if (!has_line(crash, "at: ", CALLER_OBJECT))
            return fail("no `at:` line naming " CALLER_OBJECT " in the crash log", crash);
        if (has_line(events, "bin: ", "") || has_line(crash, "bin: ", ""))
            return fail("a `bin:` line in an exit record (the exit names no load base)", crash);
        if (crash_len == 0)
            return fail("the crash log is empty", crash);
    }

    printf("exit-interpose (%s): the library's exit(%d) ended the child with status %d%s\n",
           argv[2], CALLER_STATUS, expected,
           interposed ? ", with an exit record naming the thread and " CALLER_OBJECT " in both the event log and the crash log" : ", as expected");
    fflush(stdout);
    return 0;
}
