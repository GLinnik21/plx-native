/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository root.
 *
 * exit-interpose-test.c — **does an `exit()` made by a shared library, on a thread of its own, end
 * the process through `src/exit.c` with the caller's real status?** Built and run by `make check`
 * (`check-c-unit`) with the HOST compiler on Linux; it needs no television, no NDK and no Rust.
 *
 * # What it grades
 *
 * `src/exit.c` defines libc `exit` in the executable and the link exports it, so that the `exit` of
 * a firmware library binds there (Sentry PLX-NATIVE-1P: an exit made on SDL's own `SDLTimer`
 * thread ran an exit handler that crashed). This test makes that call for real:
 *
 *   1. a forked child `dlopen`s `ci/exit-interpose-lib.c`'s object BY PATH (argv[1]),
 *   2. calls its function from a SECOND thread (the SDLTimer shape), which registers a finalizer
 *      that would `_exit(42)` and then calls `exit(7)` through its own PLT,
 *   3. and the parent asserts how the child ended (argv[2] is the status it expects).
 *
 * Built twice by the Makefile. With `src/exit.c` linked, the child must end with 7: the call
 * reached `plx_exit_intercepted` (a stub here, `_exit(status)`; the Rust half is
 * `plx_base::process::plx_exit_intercepted`), the caller's real status survived and the finalizer
 * never ran. WITHOUT it, the same program must end with 42, because libc's `exit` runs the
 * finalizer: that control is what proves this test can fail, and that it is `src/exit.c` that
 * changes the result.
 *
 * # What it cannot see
 *
 * Whether the shipped ARM binary exports `exit` is `ci/check-elf.sh`'s question, asked of the
 * artifact. Linux only: Darwin's two-level namespace binds a dylib's `exit` to libSystem whatever
 * the executable defines, so the Makefile skips this test there.
 */
#include <dlfcn.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* The status the library's thread passes to `exit`, and so the one an interposed `exit` must keep. */
#define CALLER_STATUS 7

/* Stands in for the Rust side, `plx_base::process::plx_exit_intercepted`: end the process with the
 * status and run nothing. `src/exit.c` calls this; the control build does not link `src/exit.c`. */
void plx_exit_intercepted(int status) __attribute__((noreturn));
void plx_exit_intercepted(int status) {
    _exit(status);
}

static void (*library_entry)(int);

/* The "SDLTimer" thread: the library's function never returns. */
static void *second_thread(void *unused) {
    (void)unused;
    library_entry(CALLER_STATUS);
    return NULL;
}

static void run_child(const char *library) __attribute__((noreturn));
static void run_child(const char *library) {
    alarm(30); /* a hung exit path fails the test instead of the build */
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

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: %s <library.so> <expected child status>\n", argv[0]);
        return 2;
    }
    int expected = atoi(argv[2]);

    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return 2;
    }
    if (pid == 0) {
        run_child(argv[1]);
    }

    int status = 0;
    if (waitpid(pid, &status, 0) != pid) {
        perror("waitpid");
        return 2;
    }
    if (WIFEXITED(status) && WEXITSTATUS(status) == expected) {
        printf("exit-interpose: the library's exit(%d) ended the child with status %d, as expected\n",
               CALLER_STATUS, expected);
        fflush(stdout);
        return 0;
    }
    if (WIFEXITED(status)) {
        fprintf(stderr, "FAIL: the child exited with status %d, expected %d (7: interposed, 42: libc's exit ran the finalizer)\n",
                WEXITSTATUS(status), expected);
    } else {
        fprintf(stderr, "FAIL: the child did not exit normally (wait status 0x%x), expected status %d\n", status, expected);
    }
    return 1;
}
