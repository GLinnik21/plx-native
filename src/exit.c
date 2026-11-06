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
 * `callbackFunctionHook` interposer in `src/starfish.c`. It hands the status and its own return
 * address to `plx_exit_intercepted` (`rust-modules/base/src/process.rs`), which logs the calling
 * thread and the shared object and offset the return address falls in, then ends the process with
 * `exit_now`. The return address is there because the PLX-NATIVE-1P event shows no frame above
 * `__run_exit_handlers`, so which library called `exit` is otherwise unknown.
 *
 * This is an interposer and not an `atexit` handler because it does not depend on the order the
 * finalizers were registered in (a handler registered once at boot is run AFTER everything
 * registered later), it keeps the caller's real status, and it runs before glibc's
 * `__call_tls_dtors`.
 *
 * The firmware's own SDL2 imports `exit@GLIBC_2.4` through an `R_ARM_JUMP_SLOT` on the inventoried
 * images (webOS 4.x W19P/W19R, 6.x W21O, 9.2 W24G, 11.2 W23O), so an `exit` SDL makes binds to this
 * definition. Whether PLX-NATIVE-1P's call came through a PLT at all is not determined: the event
 * has no frame above `__run_exit_handlers`.
 *
 * The Makefile exports the symbol explicitly so the export does not rest on an implicit rule (the
 * NDK's ld exports the executable's `exit` even without the flag, because libc.so, always on the
 * link line, defines `exit` too) and so `exit` is a `--gc-sections` root. `ci/check-elf.sh` asserts
 * the export on the shipped binary, and `ci/exit-interpose-test.c` proves the chain on a Linux host
 * against a control built without this file.
 *
 * # What it does not cover
 *
 * libc.so.6's own internal callers of `exit` (`error(3)` and `err(3)` with a nonzero status,
 * `__libc_start_main` after `main` returns) call libc's `exit` directly, not through the PLT (the
 * firmware libc has no `exit@` relocation, glibc 2.24-2.39), so they still run the handlers.
 * libpthread's last-thread `exit(0)` on glibc <= 2.31 (webOS 4.x-8.x, before libpthread merged into
 * libc) does go through its PLT and is interposed, which is harmless: the app never ends its main
 * thread with `pthread_exit`. Every exit of the app session itself goes through `exit_now`;
 * `src/main.c` returns 0 from `main` only on the Sentry-spool re-entry
 * (`plx_sentry_spool_external`), which runs libc's own `exit` as before.
 */
#include <stdlib.h>

extern void plx_exit_intercepted(int status, void *caller) __attribute__((noreturn));

__attribute__((noreturn)) void exit(int status) {
    plx_exit_intercepted(status, __builtin_return_address(0));
}
