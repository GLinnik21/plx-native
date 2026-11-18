/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository root.
 *
 * access-interpose-test.c — **does an `access()` made by a shared library answer the GPU driver's
 * four debug-switch paths "absent" without the kernel, and every other path exactly as the kernel
 * does?** Built and run by `make check` (`check-c-unit`) with the HOST compiler on Linux; it needs
 * no television, no NDK and no Rust.
 *
 * # What it grades
 *
 * `src/access.c` defines libc `access` in the executable and the link exports it, so that the
 * `access` of a library (`libmali`, which probes `/tmp/API_PRINT` and three siblings at the top of
 * every GL call) binds there. This test makes that call for real: `ci/access-interpose-lib.c` is
 * built as a shared object and opened BY PATH (argv[1]), and every call below is made through its
 * `access_from_library`, whose `access` goes through that object's PLT. A call from this
 * executable would bind to the executable's own definition whatever the linker exported, so it
 * would prove nothing about the library's binding; the test makes none.
 *
 * Built twice by the Makefile. The four probe paths are real files for the duration of the run
 * (created here, removed at exit and on any signal that ends the run, the program's own faults
 * included; a file that was already there is left alone, and is not removed). Runs take turns: the
 * paths are fixed, so a lock on `/tmp` keeps two runs at once from removing each other's files.
 *
 *   `interposed` (src/access.c linked, the link's own ACCESS_INTERPOSER_LDFLAG): the library's
 *   `access` of each probe path, with the file existing, is -1 with `ENOENT`, for `F_OK` and for
 *   `R_OK|W_OK|X_OK`. Everything else is the kernel's own answer, compared with the raw system call
 *   made here: an existing file with `F_OK` and with `R_OK|W_OK` (a read-only file and one with no
 *   permission bits as well, which answer differently for a root and for any other user, hence the
 *   comparison with the kernel rather than a fixed number), a missing path, the empty path, an
 *   invalid mode, a null path (`EFAULT`), a file named `API_PRINT` in some other directory (the
 *   match is the whole path, never the base name; the kernel's answer there is 0), and
 *   `/tmp/API_PRINT/` (a probe path plus a slash; the kernel answers `ENOTDIR` for a file, and a
 *   match by prefix would answer `ENOENT`).
 *
 *   `control` (src/access.c NOT linked): the same library's `access` of each probe path returns 0,
 *   because libc answers and the file exists. That is what proves this test can fail, and that it is
 *   `src/access.c` that changes the result.
 *
 * # What it cannot see
 *
 * Whether the shipped ARM binary exports `access` is `ci/check-elf.sh`'s question, asked of the
 * artifact; whether the real `libmali` binds through its PLT is only a television's. A path added
 * to the table in `src/access.c` needs a line in `probes` below, which is the only list this test
 * has (it grades what it lists). Linux only: Darwin's two-level namespace binds a dylib's `access` to
 * libSystem whatever the executable defines, so the Makefile skips this test there.
 */
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

/* The paths the driver probes, as `src/access.c`'s table has them. */
static const char *const probes[] = {
    "/tmp/API_PRINT",
    "/tmp/SKIP_PRINT",
    "/tmp/SHADER_PRINT",
    "/tmp/GM_FPS_PRINT",
};
#define N_PROBES (sizeof probes / sizeof probes[0])

/* 1 for a probe file this run created (and so removes), 0 for one that was already there. */
static int created[N_PROBES];

/* The scratch directory and the files in it, removed by `cleanup`. */
static char dir[4096];
static char path_present[4400], path_readonly[4400], path_noperm[4400], path_basename[4400];

static int (*library_access)(const char *, int);

/* Only `unlink` and `rmdir`, so it is safe to run from the fatal-signal handler too. */
static void cleanup(void) {
    for (unsigned i = 0; i < N_PROBES; i++)
        if (created[i]) unlink(probes[i]);
    if (dir[0] == '\0') return;
    unlink(path_present);
    unlink(path_readonly);
    unlink(path_noperm);
    unlink(path_basename);
    rmdir(dir);
}

/* Runs take turns. The probe paths are fixed (they are the driver's), so two runs at once, which
 * `ci/test_check_collisions.py` makes on purpose and two checkouts' `make check` can, would delete
 * each other's probe files under a leg that needs them. The lock is on `/tmp` itself, the directory
 * the probe files are in: it needs no file of its own to leave behind, and the kernel drops it when
 * the process ends. Best effort: a filesystem that cannot lock a directory leaves the runs
 * unserialised, with a warning. */
static void take_turn(void) {
    int fd = open("/tmp", O_RDONLY | O_DIRECTORY);
    if (fd < 0 || flock(fd, LOCK_EX) != 0)
        fprintf(stderr, "access-interpose: warning: cannot lock /tmp; a concurrent run may remove the probe files\n");
}

static void on_signal(int sig) {
    cleanup();
    signal(sig, SIG_DFL);
    raise(sig);
}

static void fail(const char *what, const char *path, int mode) __attribute__((noreturn));
static void fail(const char *what, const char *path, int mode) {
    fprintf(stderr, "access-interpose: FAIL: %s (path \"%s\", mode %d, errno %d)\n", what,
            path ? path : "(null)", mode, errno);
    exit(1);
}

/* What the kernel answers: the raw system call glibc's `access` makes, made from this executable
 * (which is not a call through any library's PLT, so it is not interposed). */
static long kernel_access(const char *path, int mode) {
#if defined(SYS_access)
    return syscall(SYS_access, path, (long)mode);
#else
    return syscall(SYS_faccessat, (long)AT_FDCWD, path, (long)mode);
#endif
}

static void make_file(const char *path, mode_t perms) {
    int fd = open(path, O_WRONLY | O_CREAT | O_EXCL, perms);
    if (fd < 0) fail("cannot create a scratch file", path, 0);
    close(fd);
    /* `open`'s mode is masked by the umask; the permission bits are the point of two of them. */
    if (chmod(path, perms) != 0) fail("cannot set a scratch file's permissions", path, 0);
}

/* The library's `access` must be exactly the kernel's: the same return and, on failure, the same
 * errno. */
static void expect_kernel_answer(const char *path, int mode) {
    errno = 0;
    long want = kernel_access(path, mode);
    int want_errno = errno;
    errno = 0;
    int got = library_access(path, mode);
    int got_errno = errno;
    if (got != (int)want) fail("the library's access() differs from the kernel's return", path, mode);
    if (want < 0 && got_errno != want_errno) {
        fprintf(stderr, "access-interpose: kernel errno %d\n", want_errno);
        fail("the library's access() differs from the kernel's errno", path, mode);
    }
}

int main(int argc, char **argv) {
    if (argc != 3 || (strcmp(argv[2], "interposed") != 0 && strcmp(argv[2], "control") != 0)) {
        fprintf(stderr, "usage: %s <libaccesscaller.so> <interposed|control>\n", argv[0]);
        return 2;
    }
    int interposed = strcmp(argv[2], "interposed") == 0;
    alarm(30); /* a hang fails the test instead of the build */
    atexit(cleanup);
    /* Every signal that would end the run, the program's own faults included: the probe files are
     * removed whatever the way out is (SIGKILL aside, which nothing can catch). */
    static const int fatal[] = {SIGINT, SIGTERM, SIGHUP, SIGALRM, SIGSEGV, SIGBUS, SIGABRT, SIGFPE, SIGILL};
    for (unsigned i = 0; i < sizeof fatal / sizeof fatal[0]; i++) signal(fatal[i], on_signal);
    take_turn();

    void *handle = dlopen(argv[1], RTLD_NOW);
    if (!handle) {
        fprintf(stderr, "dlopen %s: %s\n", argv[1], dlerror());
        return 98;
    }
    library_access = (int (*)(const char *, int))dlsym(handle, "access_from_library");
    if (!library_access) {
        fprintf(stderr, "dlsym access_from_library: %s\n", dlerror());
        return 98;
    }

    /* The probe files exist, whoever made them. A path that is already taken is left as it is; one
     * that is taken by something that does not resolve (a dangling link) cannot stand for a file
     * the driver would find, so the test cannot say anything and refuses to. */
    for (unsigned i = 0; i < N_PROBES; i++) {
        int fd = open(probes[i], O_WRONLY | O_CREAT | O_EXCL, 0600);
        if (fd >= 0) {
            close(fd);
            created[i] = 1;
        } else if (errno != EEXIST) {
            fail("cannot create a probe file", probes[i], 0);
        }
        struct stat st;
        if (stat(probes[i], &st) != 0) fail("the probe path exists but does not resolve to a file", probes[i], 0);
    }

    if (!interposed) {
        for (unsigned i = 0; i < N_PROBES; i++) {
            errno = 0;
            if (library_access(probes[i], F_OK) != 0)
                fail("control: libc's access() did not see the probe file (the test cannot discriminate)", probes[i], F_OK);
        }
        printf("access-interpose (control): the library's access() of the %u existing probe files returned 0, as expected\n",
               (unsigned)N_PROBES);
        fflush(stdout);
        return 0;
    }

    for (unsigned i = 0; i < N_PROBES; i++) {
        static const int modes[] = {F_OK, R_OK | W_OK | X_OK};
        for (unsigned m = 0; m < sizeof modes / sizeof modes[0]; m++) {
            errno = 0;
            int r = library_access(probes[i], modes[m]);
            if (r != -1 || errno != ENOENT)
                fail("a probe path that exists must still be answered -1/ENOENT", probes[i], modes[m]);
        }
    }

    const char *tmp = getenv("TMPDIR");
    snprintf(dir, sizeof dir, "%s/plx-access-test.XXXXXX", tmp && *tmp ? tmp : "/tmp");
    if (!mkdtemp(dir)) {
        dir[0] = '\0';
        fail("mkdtemp failed", "", 0);
    }
    snprintf(path_present, sizeof path_present, "%s/present", dir);
    snprintf(path_readonly, sizeof path_readonly, "%s/readonly", dir);
    snprintf(path_noperm, sizeof path_noperm, "%s/noperm", dir);
    snprintf(path_basename, sizeof path_basename, "%s/API_PRINT", dir);
    make_file(path_present, 0600);
    make_file(path_readonly, 0444);
    make_file(path_noperm, 0000);
    make_file(path_basename, 0600);

    /* An existing file: 0 for F_OK; the other modes are whatever the kernel says for this user. */
    errno = 0;
    if (library_access(path_present, F_OK) != 0) fail("an existing file must be accessible", path_present, F_OK);
    expect_kernel_answer(path_present, F_OK);
    expect_kernel_answer(path_present, R_OK | W_OK);
    expect_kernel_answer(path_present, X_OK);
    expect_kernel_answer(path_readonly, F_OK);
    expect_kernel_answer(path_readonly, R_OK | W_OK);
    expect_kernel_answer(path_noperm, R_OK);
    expect_kernel_answer(path_noperm, W_OK);

    /* Missing paths: -1/ENOENT from the kernel, not from the table. */
    errno = 0;
    if (library_access("/tmp/plx-access-test-no-such-file", F_OK) != -1 || errno != ENOENT)
        fail("a missing path must be -1/ENOENT", "/tmp/plx-access-test-no-such-file", F_OK);
    expect_kernel_answer("/tmp/plx-access-test-no-such-file", F_OK);
    expect_kernel_answer("", F_OK);
    expect_kernel_answer(path_present, 0x40); /* not a valid mode: the kernel's EINVAL, not ours */
    expect_kernel_answer(NULL, F_OK);         /* EFAULT, as glibc's access would say */

    /* The match is the whole path. A file with a probe's base name elsewhere exists (0)... */
    errno = 0;
    if (library_access(path_basename, F_OK) != 0)
        fail("a file named like a probe, in another directory, must be accessible", path_basename, F_OK);
    expect_kernel_answer(path_basename, F_OK);
    /* ...and a probe path with a slash after it is the kernel's: a regular file is not a directory. */
    errno = 0;
    if (library_access("/tmp/API_PRINT/", F_OK) != -1 || errno != ENOTDIR)
        fail("a probe path plus a slash must reach the kernel (ENOTDIR), not match by prefix", "/tmp/API_PRINT/", F_OK);
    expect_kernel_answer("/tmp/API_PRINT/", F_OK);

    printf("access-interpose (interposed): the library's access() answered the %u probe paths -1/ENOENT and every other path as the kernel does\n",
           (unsigned)N_PROBES);
    fflush(stdout);
    return 0;
}
