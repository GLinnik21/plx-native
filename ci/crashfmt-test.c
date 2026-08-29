/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under the MIT Licence; see LICENSE at the repository root.
 *
 * crashfmt-test.c — host test for `src/crashfmt.h`, the pure half of the crash tracer.
 * Built and run by `make check` with the HOST compiler; it links nothing and needs no television.
 *
 * # Why this exists
 *
 * The tracer itself can only be graded on a television: it runs in signal context on ARM, and
 * nothing on this Mac can fault an ARM process. But the part of it that has actually been WRONG
 * has never been the syscalls — it has been the parsing, and a wrong `bin:` line is silent:
 * `tools/crash-report.sh` subtracts that mapping's base from the PC and answers with a confident
 * wrong function rather than failing. Its twin is recorded in that script, on the app id, because
 * `com.beb.plxnative` is a PREFIX of `com.beb.plxnative.debug`.
 *
 * **Written by watching it fail.** `plx_names_our_binary` was reverted to the bare substring test
 * before this file was trusted, and exactly one assertion below went red — `plxnative-sim`. That
 * result is itself the finding: it disproved the justification `src/main.c` had carried since the
 * tracer was written (that a substring test would also match `libturbojpeg.so.0` beside the
 * binary), because the needle carries a slash and the directory component is `/com.beb.plxnative`.
 * The libturbojpeg case is still asserted below, but as what it really is — a line that is not our
 * binary for a reason that has nothing to do with the separator — and `crashfmt.h`'s comment now
 * says which cases the separator actually buys.
 *
 * # What it cannot see
 *
 * The host compiler, not the NDK's GCC 12; a 64-bit `unsigned long`, not the target's 32-bit one.
 * Neither matters for what is asserted here — every function under test is pure C99 over values
 * the caller supplies — but it does mean this proves the LOGIC and never the codegen. The
 * cross-build's own `-Wall -Wextra -Werror` covers the second half.
 */
#include "crashfmt.h"

#include <stdio.h>
#include <string.h>

static int failures = 0;

static void eq_str(const char *what, const char *got, const char *want) {
    if (strcmp(got, want) == 0) return;
    fprintf(stderr, "FAIL %s\n  got  %s\n  want %s\n", what, got, want);
    failures++;
}

static void eq_int(const char *what, long got, long want) {
    if (got == want) return;
    fprintf(stderr, "FAIL %s: got %ld, want %ld\n", what, got, want);
    failures++;
}

/* Format one value through the real `plx_s_hex` and return it as a C string. */
static const char *hex(unsigned long v) {
    static char out[64];
    struct plx_sbuf b = { out, 0, sizeof out - 1 };
    plx_s_hex(&b, v);
    out[b.n] = 0;
    return out;
}

static const char *dec(unsigned long v) {
    static char out[64];
    struct plx_sbuf b = { out, 0, sizeof out - 1 };
    plx_s_dec(&b, v);
    out[b.n] = 0;
    return out;
}

static int kind(const char *line, unsigned long pc, unsigned long lr) {
    return plx_map_line_kind(line, strlen(line), pc, lr);
}

int main(void) {
    /* ---- the numbers `crash-report.sh` parses ------------------------------------------- */
    /* Zero must be `0x0` and not the bare prefix — a null `si_addr` is the single most common
     * fault address there is, so the do-while that produces this is load-bearing. */
    eq_str("hex(0)", hex(0), "0x0");
    eq_str("hex(1)", hex(1), "0x1");
    /* No zero padding and lowercase: the sed in crash-report.sh is `pc=0x\\([0-9a-f]*\\)`, and
     * addr2line takes the result verbatim. */
    eq_str("hex(0xb6f2a1c4)", hex(0xb6f2a1c4UL), "0xb6f2a1c4");
    eq_str("hex(0xdeadbeef)", hex(0xdeadbeefUL), "0xdeadbeef");
    eq_str("dec(0)", dec(0), "0");
    eq_str("dec(11)", dec(11), "11");

    /* ---- the record's first line, byte for byte ------------------------------------------ */
    /* Every tool that reads this file reads THIS line, so it is pinned exactly rather than by
     * substring. The leading newline separates one crash from the last in the append-only log. */
    {
        char out[256];
        size_t n = plx_fmt_signal(out, sizeof out - 1, 11, "SIGSEGV", 0, 0xb6f2a1c4UL, 0xb6f2a0e8UL);
        out[n] = 0;
        eq_str("signal line",
               out,
               "\n*** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0xb6f2a1c4 lr=0xb6f2a0e8\n");
    }
    /* Truncation is silent and must not overrun. The buffer here cannot hold the whole line; the
     * only assertion that matters is that nothing past `cap` was written, which the canary
     * checks. */
    {
        char out[32];
        memset(out, '#', sizeof out);
        size_t n = plx_fmt_signal(out, 16, 11, "SIGSEGV", 0, 0xb6f2a1c4UL, 0xb6f2a0e8UL);
        eq_int("truncated length", (long)n, 16);
        eq_int("nothing written past cap", out[16], '#');
    }

    /* ---- what a /proc/self/maps line means ----------------------------------------------- */
    /* Real lines, in the shape the kernel writes them and with this project's real install paths.
     * `pc` sits inside the executable's text mapping, which is the ordinary case. */
    static const char BIN[] =
        "00010000-0097f000 r-xp 00000000 b3:35 12345 "
        "/media/developer/apps/usr/palm/applications/com.beb.plxnative/plxnative\n";
    /* Deployed beside the binary, inside a directory whose own name ends in `.plxnative`. NOT a
     * trap, in the end: the needle is `/plxnative` and this path's slash is followed by `c`, so
     * even a bare substring test rejects it. Kept because it is the shape main.c's comment named
     * for years, and a case that pins "this is not our binary" is worth having whichever argument
     * makes it true. */
    static const char JPEG[] =
        "b6a00000-b6a3c000 r-xp 00000000 b3:35 12346 "
        "/media/developer/apps/usr/palm/applications/com.beb.plxnative/libturbojpeg.so.0\n";
    /* One of the TV's own libraries: `at:` when it contains the fault, never `bin:`. */
    static const char LGLIB[] =
        "b5000000-b5240000 r-xp 00000000 b3:02 999 /usr/lib/libplayerAPIs.so\n";

    eq_int("our binary, containing the pc", kind(BIN, 0x00020000UL, 0), PLX_MAP_AT | PLX_MAP_BIN);
    eq_int("our binary, not containing it", kind(BIN, 0xb5100000UL, 0), PLX_MAP_BIN);
    eq_int("a library beside the binary is NOT the binary", kind(JPEG, 0x00020000UL, 0), 0);
    /* THE case the separator actually buys, and the only assertion that went red when the token
     * test was reverted to a substring one. `plxnative-sim` is the host simulator and
     * `plxnative.new` is what `make deploy` scp's before renaming it over the running binary;
     * neither is mapped on a television today, which is what makes this a test rather than a
     * hope — nothing else would notice if the guard were dropped. */
    eq_int("plxnative.new is a different name",
           kind("00010000-00020000 r-xp 0 0:0 1 /media/developer/apps/usr/palm/applications/"
                "com.beb.plxnative/plxnative.new\n", 0, 0), 0);
    eq_int("a library beside it, containing the pc", kind(JPEG, 0xb6a10000UL, 0), PLX_MAP_AT);
    eq_int("a TV library containing the pc", kind(LGLIB, 0xb5100000UL, 0), PLX_MAP_AT);

    /* The `(deleted)` form, and the other half of what the separator buys. `make deploy` writes
     * `plxnative.new` and renames it over the running binary (the ETXTBSY dance), so the kernel
     * appends this on every development iteration — the space-terminated spelling is the NORMAL
     * one while iterating, and a test for `"/plxnative\n"` alone would fail to find our own binary
     * exactly when it is being worked on. */
    static const char DELETED[] =
        "00010000-0097f000 r-xp 00000000 b3:35 12345 "
        "/media/developer/apps/usr/palm/applications/com.beb.plxnative/plxnative (deleted)\n";
    eq_int("the (deleted) form is still our binary", kind(DELETED, 0, 0), PLX_MAP_BIN);

    /* And the flavoured install, whose directory name has `.debug` after the token — the prefix
     * trap from the other side. Still our binary: both installs' executables are named
     * `plxnative`, and it is `crash-report.sh` that decides WHICH install a `bin:` line belongs
     * to, by matching the app id in the path. This test pins that division of labour. */
    static const char DEBUG_BIN[] =
        "00010000-0097f000 r-xp 00000000 b3:35 12347 "
        "/media/developer/apps/usr/palm/applications/com.beb.plxnative.debug/plxnative\n";
    eq_int("the debug install's binary is also a bin: line", kind(DEBUG_BIN, 0, 0), PLX_MAP_BIN);

    /* The LR is checked as well as the PC: on a jump through a bad function pointer the PC is
     * garbage and the link register is the only thing naming a real module. */
    eq_int("matched by the lr alone", kind(LGLIB, 0xffffffffUL, 0xb5100000UL), PLX_MAP_AT);

    /* Half-open, at both ends. `hi` is the first address NOT in the mapping; a closed test would
     * attribute a fault to the library below the one that owns it. */
    eq_int("lo is inside", kind(LGLIB, 0xb5000000UL, 0), PLX_MAP_AT);
    eq_int("hi is outside", kind(LGLIB, 0xb5240000UL, 0), 0);

    /* A line with no range says nothing — rather than parsing to a zero-length range at zero that
     * then "contains" a null PC, which would put an `at:` line on every crash with a null address
     * and point it at whatever garbage followed. */
    eq_int("a line with no range", kind("not a maps line at all\n", 0, 0), 0);
    eq_int("a truncated range", kind("00010000-\n", 0, 0), 0);
    eq_int("an empty line", kind("\n", 0, 0), 0);

    eq_int("plxnative-sim is a different name", kind("00010000-00020000 r-xp 0 0:0 1 /x/plxnative-sim\n", 0, 0), 0);

    if (failures) {
        fprintf(stderr, "crashfmt: %d assertion(s) failed\n", failures);
        return 1;
    }
    printf("crashfmt: ok\n");
    return 0;
}
