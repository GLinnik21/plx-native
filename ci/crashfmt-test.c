/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository root.
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
    /* The record's second line: the wall-clock second the crash happened, which the Rust importer
     * reads back as the event's `timestamp` (`crashreport::crash_time_record` is the same
     * spelling). Pinned byte for byte because a change here silently strips the time off every
     * fallback event. */
    {
        char out[64];
        size_t n = plx_fmt_time(out, sizeof out - 1, 1750000000UL);
        out[n] = 0;
        eq_str("time line", out, "time: 1750000000\n");
        n = plx_fmt_time(out, sizeof out - 1, 0);
        out[n] = 0;
        eq_str("time line, zero", out, "time: 0\n");
        char small[32];
        memset(small, '#', sizeof small);
        n = plx_fmt_time(small, 8, 1750000000UL);
        eq_int("time line truncated length", (long)n, 8);
        eq_int("time line wrote nothing past cap", small[8], '#');
    }
    /* The thread and phase lines: byte for byte, because `crashreport::parse` reads them back and
     * a drift strips the thread off every fallback event. A name is cut at 15 bytes and a byte
     * outside [A-Za-z0-9_.-] (a space, a newline) can never reach the log as itself. */
    {
        char out[96];
        size_t n = plx_fmt_thread(out, sizeof out - 1, 4242, 1, "plxnative");
        out[n] = 0;
        eq_str("thread line", out, "thread: tid=4242 main=1 name=plxnative\n");
        n = plx_fmt_thread(out, sizeof out - 1, 7, 0, "a b\nc*");
        out[n] = 0;
        eq_str("thread line, hostile name", out, "thread: tid=7 main=0 name=a?b?c?\n");
        n = plx_fmt_thread(out, sizeof out - 1, 7, 0, "");
        out[n] = 0;
        eq_str("thread line, empty name", out, "thread: tid=7 main=0 name=?\n");
        n = plx_fmt_thread(out, sizeof out - 1, 7, 0, "0123456789abcdefXYZ");
        out[n] = 0;
        eq_str("thread line, long name", out, "thread: tid=7 main=0 name=0123456789abcde\n");
        n = plx_fmt_phase(out, sizeof out - 1, PLX_PHASE_RUNNING, 57);
        out[n] = 0;
        eq_str("phase line, running", out, "phase: running uptime=57\n");
        n = plx_fmt_phase(out, sizeof out - 1, PLX_PHASE_SHUTDOWN, 0);
        out[n] = 0;
        eq_str("phase line, shutdown", out, "phase: shutdown uptime=0\n");
        n = plx_fmt_phase(out, sizeof out - 1, PLX_PHASE_EXITING, 3600);
        out[n] = 0;
        eq_str("phase line, exiting", out, "phase: exiting uptime=3600\n");
        n = plx_fmt_phase(out, sizeof out - 1, 9, 1);
        out[n] = 0;
        eq_str("phase line, unknown", out, "phase: unknown uptime=1\n");
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

    /* ---- the FREEZE record (`plx_fmt_freeze` and the frame walk) ---- */
    {
        char out[256];
        size_t n = plx_fmt_freeze(out, sizeof out - 1, 3, 0xb6f2a1c4UL, 0xb6f2a0e8UL);
        out[n] = 0;
        eq_str("freeze header", out, "\n*** FREEZE seq=3 pc=0xb6f2a1c4 lr=0xb6f2a0e8\n");
        /* Never a fault's header: every tool that scans for `*** SIGNAL` must keep skipping it. */
        eq_int("freeze header is not a SIGNAL line", strstr(out, "SIGNAL") == NULL, 1);
        /* Bounded like every formatter here: a short buffer truncates and never overruns. */
        char tiny[8];
        n = plx_fmt_freeze(tiny, sizeof tiny, 1, 0x1UL, 0x2UL);
        eq_int("a short buffer truncates the freeze header", (long)n, (long)sizeof tiny);

        unsigned long fr[10];
        for (unsigned i = 0; i < 10; i++) fr[i] = 0x10000UL + i;
        size_t len = 0;
        size_t put = plx_fmt_frames(out, sizeof out - 1, fr, 10, 0, 4, &len);
        out[len] = 0;
        eq_int("frames line holds per_line addresses", (long)put, 4);
        eq_str("frames line", out, "frames: 0x10000 0x10001 0x10002 0x10003\n");
        put = plx_fmt_frames(out, sizeof out - 1, fr, 10, 8, 4, &len);
        out[len] = 0;
        eq_int("the last frames line holds the remainder", (long)put, 2);
        eq_str("last frames line", out, "frames: 0x10008 0x10009\n");
    }

    /* Executable-range and `[stack]` recognition. */
    {
        struct plx_range r;
        eq_int("an r-xp line is executable",
               plx_map_line_exec("00010000-0097f000 r-xp 00000000 b3:35 1 /x/plxnative\n", 56, &r), 1);
        eq_int("lo of the range", (long)r.lo, 0x10000);
        eq_int("hi of the range", (long)r.hi, 0x97f000);
        const char *rw = "b5000000-b5001000 rw-p 00000000 00:00 0 \n";
        eq_int("rw-p is not executable", plx_map_line_exec(rw, strlen(rw), &r), 0);
        const char *ro = "b5000000-b5001000 r--p 00000000 00:00 0 \n";
        eq_int("r--p is not executable", plx_map_line_exec(ro, strlen(ro), &r), 0);
        const char *junk = "not a maps line\n";
        eq_int("junk is not a range", plx_map_line_exec(junk, strlen(junk), &r), 0);
        const char *trunc = "00010000-0097f000 r-";
        eq_int("a truncated permission field is rejected", plx_map_line_exec(trunc, strlen(trunc), &r), 0);

        unsigned long hi = 0;
        const char *stk = "be800000-bf000000 rw-p 00000000 00:00 0          [stack]\n";
        eq_int("the [stack] line containing the address",
               plx_map_line_stack_end(stk, strlen(stk), 0xbefff000UL, &hi), 1);
        eq_int("its end", (long)hi, 0xbf000000L);
        eq_int("an address outside it", plx_map_line_stack_end(stk, strlen(stk), 0xb0000000UL, &hi), 0);
        const char *heap = "00a00000-00a21000 rw-p 00000000 00:00 0          [heap]\n";
        eq_int("[heap] is not the stack", plx_map_line_stack_end(heap, strlen(heap), 0xa10000UL, &hi), 0);
    }

    /* The frame walk, on a synthetic stack, in BOTH layouts and the ways it must stop. The words
     * are `unsigned long`, so the layout offsets are `sizeof(unsigned long)`; addresses are real
     * pointers into `stack`, which is what the walk dereferences. */
    {
        enum { N = 64 };
        static unsigned long stack[N];
        const unsigned long lo = (unsigned long)&stack[0], hi = (unsigned long)&stack[N];
        const struct plx_range code[] = { { 0x10000, 0x20000 } };
        unsigned long out[8];
        const unsigned long A = 0x10100, B = 0x10200, C = 0x10301;   /* C has the Thumb bit set */
        #define W(i) ((unsigned long)&stack[i])

        /* LLVM layout: [fp] = caller's fp, [fp+w] = return address. Three frames, the last one's
         * saved fp is zero, which ends the chain. */
        memset(stack, 0, sizeof stack);
        stack[10] = W(20); stack[11] = A;
        stack[20] = W(30); stack[21] = B;
        stack[30] = 0;     stack[31] = C;
        size_t n = plx_fp_walk(lo, W(10), hi, code, 1, out, 8);
        eq_int("LLVM layout: three frames", (long)n, 3);
        eq_int("LLVM frame 0", (long)out[0], (long)A);
        eq_int("LLVM frame 1", (long)out[1], (long)B);
        eq_int("the Thumb bit is cleared", (long)out[2], (long)(C & ~1ul));

        /* GCC layout: [fp] = return address, [fp-w] = caller's fp. */
        memset(stack, 0, sizeof stack);
        stack[11] = W(21); stack[12] = A;
        stack[21] = W(31); stack[22] = B;
        stack[31] = 0;     stack[32] = C;
        n = plx_fp_walk(lo, W(12), hi, code, 1, out, 8);
        eq_int("GCC layout: three frames", (long)n, 3);
        eq_int("GCC frame 0", (long)out[0], (long)A);
        eq_int("GCC frame 2", (long)out[2], (long)(C & ~1ul));

        /* A chain that crosses from a Rust frame into a C frame and back. */
        memset(stack, 0, sizeof stack);
        stack[10] = W(21); stack[11] = A;    /* LLVM frame; its saved fp is the GCC frame's fp */
        stack[21] = B;     stack[20] = W(40);/* GCC frame at fp=W(21): [fp]=ret, [fp-w]=caller fp */
        stack[40] = 0;     stack[41] = C;    /* LLVM frame, end of chain */
        n = plx_fp_walk(lo, W(10), hi, code, 1, out, 8);
        eq_int("a mixed chain walks through both layouts", (long)n, 3);
        eq_int("mixed frame 1", (long)out[1], (long)B);

        /* The count is capped. */
        memset(stack, 0, sizeof stack);
        stack[10] = W(20); stack[11] = A;
        stack[20] = W(30); stack[21] = B;
        stack[30] = 0;     stack[31] = C;
        eq_int("max frames caps the walk", (long)plx_fp_walk(lo, W(10), hi, code, 1, out, 2), 2);

        /* It must not touch memory outside [sp, hi): each of these starts at a frame pointer the
         * window excludes, and returns zero frames without dereferencing it. */
        eq_int("fp below sp", (long)plx_fp_walk(W(20), W(10), hi, code, 1, out, 8), 0);
        eq_int("fp at hi", (long)plx_fp_walk(lo, hi, hi, code, 1, out, 8), 0);
        eq_int("fp whose frame record straddles hi", (long)plx_fp_walk(lo, W(N - 1), hi, code, 1, out, 8), 0);
        eq_int("an unaligned fp", (long)plx_fp_walk(lo, W(10) + 1, hi, code, 1, out, 8), 0);
        eq_int("fp zero", (long)plx_fp_walk(lo, 0, hi, code, 1, out, 8), 0);
        /* A frame pointer near the top of the address space must not wrap the bound check. */
        eq_int("fp -w wraps", (long)plx_fp_walk(lo, (unsigned long)0 - sizeof(unsigned long), hi, code, 1, out, 8), 0);
        eq_int("fp -2w wraps", (long)plx_fp_walk(lo, (unsigned long)0 - 2 * sizeof(unsigned long), hi, code, 1, out, 8), 0);

        /* A return address that is not in executable memory is not a frame. */
        memset(stack, 0, sizeof stack);
        stack[10] = W(20); stack[11] = 0x999999;
        eq_int("a return address outside code ends the walk", (long)plx_fp_walk(lo, W(10), hi, code, 1, out, 8), 0);

        /* A chain that does not strictly rise (a self-loop, a step back) terminates. */
        memset(stack, 0, sizeof stack);
        stack[10] = W(10); stack[11] = A;    /* saved fp == fp */
        eq_int("a self-loop does not walk", (long)plx_fp_walk(lo, W(10), hi, code, 1, out, 8), 0);
        memset(stack, 0, sizeof stack);
        stack[20] = W(10); stack[21] = A;    /* saved fp below fp */
        eq_int("a descending chain does not walk", (long)plx_fp_walk(lo, W(20), hi, code, 1, out, 8), 0);
        #undef W
    }

    if (failures) {
        fprintf(stderr, "crashfmt: %d assertion(s) failed\n", failures);
        return 1;
    }
    printf("crashfmt: ok\n");
    return 0;
}
