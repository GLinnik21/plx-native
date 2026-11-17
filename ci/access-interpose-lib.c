/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository root.
 *
 * access-interpose-lib.c — the "GPU driver" `ci/access-interpose-test.c` loads: a shared object
 * whose one function calls libc `access`, the way `libmali` does at the top of every GL call.
 * Built `-shared -fPIC` and opened with `dlopen` BY PATH, never linked into the test: a shared
 * object on the link line can change what the linker exports from the executable, and the test is
 * about what the DYNAMIC LINKER does with this object's `access`.
 *
 * Its `access` goes through this object's own PLT, so it binds to the executable's `access` if the
 * executable exports one (`src/access.c`) and to libc's otherwise. A call made from the test
 * executable itself would prove nothing about that binding, which is why this object exists.
 */
#include <unistd.h>

int access_from_library(const char *path, int mode) {
    return access(path, mode);
}
