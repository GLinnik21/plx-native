/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository root.
 *
 * exit.c — the executable's own definition of libc `exit`, so that an `exit()` made by a shared
 * library ends the process without running any exit handler, as the app's own exits do.
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
 * address to `plx_exit_record` (`src/crashtrace.c`), which writes one record to the event log and
 * to the append-only crash log (the thread, and the mapping the return address falls in: the
 * shared object that called `exit`) and ends the process without running an exit handler, with
 * the `exit_group` syscall (see below). The next launch reports the
 * crash log's copy as an error event under the crash-report consent (`crashreport::parse`). The
 * return address is there because the PLX-NATIVE-1P event shows no frame above
 * `__run_exit_handlers`, so which library called `exit` is otherwise unknown.
 *
 * **The interposer must be async-signal-safe, because `exit` can be reached from a signal
 * handler** (a library's own, on any thread), where a lock the interrupted code holds is a
 * self-deadlock. So the whole path is the crash tracer's own: the compiler intrinsic
 * `__builtin_return_address`, lock-free `__atomic` operations (the guard that lets only the first
 * exit write a record; a second one ends the process at once), the `gettid` syscall and `prctl`,
 * `time` and `getpid`, `open`/`read`/`write`/`close` over static buffers, `pause` and the
 * `exit_group` syscall (reached as `syscall(SYS_exit_group)`, a libc entry `plx_crash_install` has
 * already bound, so the exit runs no lazy PLT resolution; `_exit` is only the unreachable fallback
 * behind it and the way out off Linux). It allocates nothing, takes no lock and calls neither
 * `dladdr` (which takes the loader's lock) nor Rust (`format!`, the event log's lock, `exit_now`'s
 * stdout and stderr locks). While a fatal signal's handler is running it ends nothing: it parks in
 * `pause`, so a crash record in flight is not cut short and the handler's re-raise ends the
 * process with the signal.
 *
 * **It does not flush.** `exit` flushes the C stdio streams and `exit_now` Rust's stdout and stderr,
 * and each takes a lock the interrupted code may hold. What that drops is at most a partial line
 * still buffered in Rust's stdout or a C stream; the event and crash logs are raw descriptor writes
 * and lose nothing.
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
 * LG's SDL does call `exit` on a thread of its own: every `libsdl2-webos` build in the firmware
 * images, 2.0.0 (W16) to 2.0.14 (W25), carries a "forcetoEXIT" force-quit thread. In 2.0.5-83 (W19P)
 * a callback that also sends `SDL_WINDOWEVENT_CLOSE` starts it; it runs `SDL_Delay(60000)` and then
 * `exit(1)`, and its creation-failure path calls `exit(1)` at once. Those are that build's only two
 * `exit` calls, and neither is in a signal handler. The field crash was on a thread named `SDLTimer`
 * 1.4 s after backgrounding, which matches neither that thread's name nor its delay; the crashing k5lp
 * build is not in the image set, so whether it is the same mechanism is not determined.
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
#include "crashtrace.h"

#include <stdlib.h>

__attribute__((noreturn)) void exit(int status) {
    plx_exit_record(status, (unsigned long)__builtin_return_address(0));
}
