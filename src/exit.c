/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository root.
 *
 * exit.c — the executable's own definition of libc `exit`, so that an `exit()` made by a shared
 * library ends the process the way the app's own exits do.
 *
 * # What it is for
 *
 * Every exit the app makes goes through `plx_base::process::exit_now` (flush, then `_exit`): libc's
 * `exit` runs the `atexit` handlers and the libraries' finalizers while worker threads are still
 * inside them (#1115). A firmware library can still call `exit` on a thread of its own. Sentry
 * PLX-NATIVE-1P (0.9.0, webOS TV Lite 4.10, board k5lp) is one: a session backgrounded mid-playback
 * crashed on SDL's own `SDLTimer` thread, which the app never asks for (it calls `SDL_Init(
 * SDL_INIT_VIDEO)` and adds no timer), with `__run_exit_handlers` on the stack and an unsymbolized
 * exit handler under it. Which handler faulted, and why the platform's SDL called `exit`, is not
 * determined; what is known is that no finalizer should run there at all.
 *
 * # How
 *
 * This file defines `exit`, and the link exports it (`-Wl,--export-dynamic-symbol=exit`, the
 * Makefile's `EXIT_INTERPOSER_LDFLAG`), so the `R_ARM_JUMP_SLOT` of every shared object's `exit`
 * binds to this definition instead of libc's. The same mechanism carries the Starfish
 * `callbackFunctionHook` interposer in `src/starfish.c`. It hands the status to
 * `plx_exit_intercepted` (`rust-modules/base/src/process.rs`), which logs the caller and ends the
 * process with `exit_now`. That is why this is an interposer and not an `atexit` handler: it does
 * not depend on the order the finalizers were registered in (a handler registered once at boot is
 * run AFTER everything registered later), it keeps the caller's real status, and it runs before
 * glibc's `__call_tls_dtors`.
 *
 * It binds only because the Makefile exports it: an executable's own symbol is otherwise invisible
 * to the dynamic linker. `ci/check-elf.sh` asserts the export on the shipped binary, and
 * `ci/exit-interpose-test.c` proves the whole chain on a host with and without the flag.
 *
 * # What it does not cover
 *
 * glibc's own callers of `exit` (`error(3)` and `err(3)` with a nonzero status, `__libc_start_main`
 * after `main` returns) reach libc's hidden alias, never the PLT, so they still run the handlers.
 * The app's own `main` does not return into libc: `plex_run` ends through `exit_now`.
 */
#include <stdlib.h>

extern void plx_exit_intercepted(int status) __attribute__((noreturn));

__attribute__((noreturn)) void exit(int status) {
    plx_exit_intercepted(status);
}
