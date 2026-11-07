/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository root.
 *
 * exit-interpose-lib.c — the "firmware library" `ci/exit-interpose-test.c` loads: a shared object
 * that registers a finalizer and then calls libc `exit`, the way the platform's SDL did on its
 * `SDLTimer` thread in PLX-NATIVE-1P. Built `-shared -fPIC` and opened with `dlopen` BY PATH, never
 * linked into the test: a shared object on the link line would change what the linker exports from
 * the executable, and the test is about what the DYNAMIC LINKER does with this object's `exit`.
 *
 * Its `exit` goes through this object's own PLT, so it binds to the executable's `exit` if the
 * executable exports one (`src/exit.c`) and to libc's otherwise. The finalizer is the tell:
 * libc's `exit` runs it and ends with 42; the interposed one never reaches it.
 */
#include <stdlib.h>
#include <unistd.h>

/* A finalizer a library registered earlier: libc's `exit` would run it; ours must not. */
static void finalizer(void) {
    _exit(42);
}

void exit_from_library(int code) {
    atexit(finalizer);
    exit(code);
}
