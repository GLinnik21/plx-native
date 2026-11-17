/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository root.
 *
 * access.c — the executable's own definition of libc `access`, so that the GPU driver's per-call
 * probes for its debug-switch files are answered in user space instead of costing a failing path
 * lookup in the kernel on every GL call.
 *
 * # What it is for
 *
 * On a webOS 5.6.2 television (SoC rtd288o, Mali-G31, Realtek's `libmali.so.1.0` r11p0, firmware
 * 04.64.00) the GPU driver calls `access("/tmp/API_PRINT", F_OK)` at the top of every one of its
 * exported `gl*` functions (468 of them), unconditionally. It also calls `access("/tmp/SKIP_PRINT")`
 * in `glDrawArrays`, `glDrawElements` and `eglSwapBuffers` (when that file exists the draw or the
 * swap is skipped), `access("/tmp/GM_FPS_PRINT")` once per swap and `access("/tmp/SHADER_PRINT")`
 * from `glUseProgram`. Nothing gates them: there is no environment variable and no configuration.
 *
 * On that kernel a failing `access` of such a path costs about 8.5 us (a syscall that takes no path
 * costs about 0.3 us), and scrolling Home makes roughly 850-950 GL calls per frame, so some 7-8 ms
 * of every frame is kernel time spent answering "does this debug file exist" for files that never
 * do. The cause was found with an `LD_PRELOAD` shim that answered exactly these paths with `ENOENT`
 * and made no syscall.
 *
 * Measured with this change on that set (2026-11-17; debug flavour, release profile, launcher
 * start, the Home grid scroll scene against the mock server, 55 s per run, runs interleaved control,
 * fix, control with an extra fix run; the control is `origin/main` without this file): frames slower
 * than 18 ms per run, 1638 and 1608 (control) against 54 and 57 (fix); the median per-second frame
 * 95th percentile, 31 ms against 18 ms; frames rendered in 55 s, 1644 and 1615 against 2268 and
 * 2440. Under `strace` the unfixed build made 63,553 `access("/tmp/API_PRINT")` calls in 25 s of
 * Home; the fixed build made none of the four probe paths while rendering the same scene.
 *
 * Other firmware: a sweep of 21 LG firmware images (webOS 3.4 to 11.2) found the per-GL-call probe
 * only in the Realtek k6lp webOS 5 driver (the set above runs a byte-identical `libmali`). The
 * Realtek k5lp webOS 4.5 driver probes `/tmp/GM_FPS_PRINT` once per swap, which this table also
 * answers. The GPU drivers of the other images (LG's own SoCs, MStar/MediaTek, and Realtek k2l, k8lp
 * and k24n) make no such probe, so there this definition is never reached with one of these paths
 * and costs a few byte compares per `access` call.
 *
 * # How
 *
 * This file defines `access`, and the link exports it (`-Wl,--export-dynamic-symbol=access`, the
 * Makefile's `ACCESS_INTERPOSER_LDFLAG`), so the `R_ARM_JUMP_SLOT` of every shared object's `access`
 * binds to this definition instead of libc's. That is the mechanism `src/exit.c` uses for `exit`
 * and `src/starfish.c` for the Starfish `callbackFunctionHook`. `libmali` imports `access@GLIBC_2.4`
 * through its PLT on that image, which is what makes the export reach it.
 *
 * The call is answered here, with `errno = ENOENT` and a return of -1 and no syscall, when the path
 * is exactly one of `probe_paths` below. Every other path gets the kernel's own answer from the
 * raw system call, the same one glibc's `access` makes (`access`, or `faccessat(AT_FDCWD, ...)` on
 * an ABI that has no `access` syscall), with no `dlsym` and no `RTLD_NEXT`: the result and `errno`
 * are the kernel's. So the one behaviour that changes is for four paths nothing in the app wants:
 * a file that really exists at one of them is ignored too, which includes the driver's own
 * `SKIP_PRINT` debug switch (drawing and swapping are never skipped) and means the driver's other
 * debug switches cannot be turned on by creating these files while the app runs.
 * A non-NULL invalid pointer now faults in user space (the `/tmp/` prefix compare reads it), where
 * glibc's `access` would return `EFAULT`; NULL still reaches the kernel and gets `EFAULT`.
 *
 * Past the `/tmp/` directory the match is on the whole name, never on a prefix of it or a case-folded
 * one: `/tmp/API_PRINT/`, `/tmp/api_print` and `/tmp/API_PRINT_x` all reach the kernel. The table is the only place the paths are written; a
 * firmware sweep that finds another probe path under `/tmp/` adds one line to it (and one to the list
 * in `ci/access-interpose-test.c`, which grades the list it holds). Every entry must start with
 * `/tmp/`, because that prefix is checked before the table is looked at; a probe elsewhere needs
 * that check generalised first.
 *
 * **The interposer must be async-signal-safe**, because `access` is on POSIX's list of functions a
 * signal handler may call and a driver's own handler can reach it on any thread. It allocates
 * nothing, takes no lock and touches no stdio: the path is compared with small hand-written loops
 * (not `strcmp`, which is a libc call of its own), the `/tmp/` prefix first, so an ordinary path
 * costs a few byte compares and never reaches the table, and the only libc entries it reaches are
 * `syscall` and the `errno` accessor. Both are already bound before any signal can matter:
 * `plx_crash_install` (`src/crashtrace.c`) calls `syscall(SYS_gettid)` once in `main`, before SDL is
 * initialised and so before any GL call, so the lazy-binding resolver has already run for it. The
 * `errno` write is a call to `__errno_location` through the executable's PLT, and
 * `plx_crash_install` binds that slot too, with a store through `errno`, so neither this write nor
 * the crash handlers' reads of `errno` can be the first call and run the resolver inside a signal
 * handler.
 *
 * It is Linux's only: the system call numbers are Linux's, and a Darwin executable's two-level
 * namespace would not bind a dylib's `access` here anyway, so on any other platform this file
 * defines nothing (the Makefile's host test of it is skipped there as well).
 *
 * The Makefile exports the symbol explicitly so the export does not rest on an implicit linker rule
 * and so `access` is a `--gc-sections` root.
 * `ci/check-elf.sh` asserts the export on the shipped binary, and `ci/access-interpose-test.c`
 * proves the chain on a Linux host against a control built without this file.
 *
 * # What it does not cover
 *
 * A call that does not go through a PLT slot the executable can preempt keeps its own answer: the
 * calls `libc.so.6` makes to its own `access` internally, and a driver that issues the system call
 * directly. Neither is how `libmali` reaches `access` on the measured image.
 */
#if defined(__linux__)

#include <errno.h>
#include <fcntl.h>
#include <sys/syscall.h>
#include <unistd.h>

/* The directory every probe path is in, checked before the table is. */
#define PROBE_DIR "/tmp/"
#define PROBE_DIR_LEN (sizeof PROBE_DIR - 1)

/* The paths the driver probes (`API_PRINT` on every GL call, the other three less often) and that
 * are therefore answered "absent" without a system call. Whole paths, compared exactly; each starts
 * with PROBE_DIR. */
static const char *const probe_paths[] = {
    "/tmp/API_PRINT",
    "/tmp/SKIP_PRINT",
    "/tmp/SHADER_PRINT",
    "/tmp/GM_FPS_PRINT",
};

/* Whether `path` begins with PROBE_DIR. A path shorter than that differs at its terminator, which
 * the loop never reads past. */
static int in_probe_dir(const char *path) {
    for (unsigned i = 0; i < PROBE_DIR_LEN; i++)
        if (path[i] != PROBE_DIR[i]) return 0;
    return 1;
}

/* Equal as NUL-terminated strings. It stops at the first differing byte, and neither string is
 * read past its terminator. */
static int same_path(const char *a, const char *b) {
    while (*a == *b) {
        if (*a == '\0') return 1;
        a++;
        b++;
    }
    return 0;
}

int access(const char *path, int mode) {
    /* glibc's prototype promises a non-null `path`, so the compiler is entitled to drop a null
     * check. The kernel answers a null path with EFAULT, as glibc's `access` does, and so does this:
     * the empty asm hides the promise from the optimizer. */
    __asm__("" : "+r"(path));
    if (path != NULL && in_probe_dir(path)) {
        for (unsigned i = 0; i < sizeof probe_paths / sizeof probe_paths[0]; i++) {
            if (same_path(path + PROBE_DIR_LEN, probe_paths[i] + PROBE_DIR_LEN)) {
                errno = ENOENT;
                return -1;
            }
        }
    }
#if defined(SYS_access)
    return (int)syscall(SYS_access, path, (long)mode);
#else
    return (int)syscall(SYS_faccessat, (long)AT_FDCWD, path, (long)mode);
#endif
}

#endif /* __linux__ */
