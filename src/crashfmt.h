/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under the MIT Licence; see LICENSE at the repository root.
 *
 * crashfmt.h — the PURE half of the crash tracer: formatting a fault record and deciding what a
 * /proc/self/maps line means. Everything here is a total function over its arguments, touches no
 * global and makes no syscall, which is what lets `ci/test_crashfmt.py` compile and RUN it on the
 * development Mac. `src/main.c` holds the other half — the descriptors, `write(2)`, the chunked
 * read of the maps file and the handler itself.
 *
 * # Why the split exists
 *
 * The tracer runs in signal context on a television, so it can be graded there and nowhere else,
 * and there is no way to make a host test fault an ARM process. But the part of it that has
 * actually been WRONG is not the syscalls — it is the parsing, and the failure mode is a
 * `bin:` line naming the wrong mapping, from which `tools/crash-report.sh` computes an offset and
 * answers with a confident wrong function. That is worse than no answer, it is silent, and it is
 * decidable from a string with no television in the room. Its twin is recorded in that script: the
 * app id `com.beb.plxnative` is a PREFIX of `com.beb.plxnative.debug`, so triaging the stable
 * install would accept the debug install's load base.
 *
 * **A correction, because this file inherited an invented justification and it is exactly the kind
 * that makes a reader distrust the rest.** `src/main.c` carried, for as long as the tracer existed:
 *
 *     the app dir is itself named ...com.beb.plxnative/, so a bare substring test also matches
 *     libraries deployed beside the binary (libturbojpeg.so.0)
 *
 * It does not. The needle is `/plxnative`, with the slash, and the directory component is
 * `/com.beb.plxnative` — slash, then `c`. Measured rather than reasoned: a bare
 * `strstr(line, "/plxnative")` answers no on `…/com.beb.plxnative/libturbojpeg.so.0` and yes on
 * `…/com.beb.plxnative/plxnative`, which is the right answer for the wrong reason. What the
 * separator test actually buys is the two cases below, and the test file asserts those and not the
 * one that was written down.
 *
 * # Everything here must stay async-signal-safe
 *
 * That is a constraint on the CALLER's behalf: these are `static inline`, they are called from a
 * signal handler, and so no function added here may allocate, take a lock, or call anything from
 * stdio. The formatter writes into a caller-owned buffer and stops at its capacity — a truncated
 * record is worth having and an overrun is not, and in signal context there is nothing to report a
 * failure to.
 */
#ifndef PLX_CRASHFMT_H
#define PLX_CRASHFMT_H

#include <stddef.h>

/* A bounded output buffer. `n` is what has been written, `cap` what may be. */
struct plx_sbuf { char *p; size_t n, cap; };

static inline void plx_s_ch(struct plx_sbuf *b, char c) {
    if (b->n < b->cap) b->p[b->n++] = c;
}

static inline void plx_s_str(struct plx_sbuf *b, const char *s) {
    while (*s) plx_s_ch(b, *s++);
}

static inline void plx_s_dec(struct plx_sbuf *b, unsigned long v) {
    char t[24];
    int i = 0;
    do { t[i++] = (char)('0' + (v % 10)); v /= 10; } while (v);
    while (i) plx_s_ch(b, t[--i]);
}

/* Lowercase hex with an `0x` prefix and NO zero padding — the spelling `tools/crash-report.sh`
 * greps for (`pc=0x\([0-9a-f]*\)`) and the one it hands to `addr2line` verbatim. Zero is `0x0`,
 * which the loop below produces because it is do-while: a leading-digit test would emit `0x`. */
static inline void plx_s_hex(struct plx_sbuf *b, unsigned long v) {
    static const char D[] = "0123456789abcdef";
    char t[24];
    int i = 0;
    plx_s_str(b, "0x");
    do { t[i++] = D[v & 0xf]; v >>= 4; } while (v);
    while (i) plx_s_ch(b, t[--i]);
}

/* Parse a hex run, returning the position after it, or NULL if there was none. Accepts either
 * case: the kernel writes lowercase, and depending on somebody else's formatting is how the last
 * parser in this file went wrong. */
static inline const char *plx_parse_hex(const char *s, unsigned long *out) {
    unsigned long v = 0;
    int any = 0;
    for (;; s++) {
        int d;
        if (*s >= '0' && *s <= '9')      d = *s - '0';
        else if (*s >= 'a' && *s <= 'f') d = *s - 'a' + 10;
        else if (*s >= 'A' && *s <= 'F') d = *s - 'A' + 10;
        else break;
        v = v * 16 + (unsigned long)d;
        any = 1;
    }
    *out = v;
    return any ? s : NULL;
}

/* What one /proc/self/maps line is worth saying about. A BITMASK, because a line can be both: the
 * faulting PC is very often inside our own executable's text mapping, and that line is then the
 * `at:` evidence AND the `bin:` load base. */
#define PLX_MAP_AT  1   /* contains the PC or the LR — which module faulted */
#define PLX_MAP_BIN 2   /* is our own executable's mapping — the load base */

/* Does `line` name our executable?
 *
 * A TOKEN test: `/plxnative` has to be followed by end-of-line, a space or the end of the buffer.
 * Two things turn on that separator, and neither is the one main.c used to claim (see the header
 * comment above — the libturbojpeg story is false and was measured false):
 *
 *   * **a sibling whose name merely STARTS with ours.** `plxnative-sim` is the host simulator and
 *     `plxnative.new` is the name `make deploy` scp's to before renaming over the running binary,
 *     and a bare `strstr(line, "/plxnative")` matches both. Neither is mapped into a television's
 *     address space today, which is precisely why this is worth a test rather than a hope: the
 *     cost of the guard is one comparison and the cost of losing it is silent.
 *   * **`(deleted)`**, which the kernel appends once the file has been replaced under a running
 *     process — which `make deploy`'s tmp+mv dance does on every single iteration. So the
 *     space-terminated spelling is the NORMAL one while developing, not an edge case, and a test
 *     for `"/plxnative\n"` alone would fail to find our own binary exactly when it is being
 *     worked on. The old code got this right with two `strstr` needles; this keeps it.
 */
static inline int plx_names_our_binary(const char *line, size_t n) {
    static const char TOK[] = "/plxnative";
    const size_t t = sizeof TOK - 1;
    if (n < t) return 0;
    for (size_t i = 0; i + t <= n; i++) {
        size_t j = 0;
        while (j < t && line[i + j] == TOK[j]) j++;
        if (j < t) continue;
        char c = (i + t < n) ? line[i + t] : '\n';
        if (c == '\n' || c == ' ' || c == '\0') return 1;
    }
    return 0;
}

/* Classify a maps line. Returns 0 for a line that says nothing — including one that does not
 * begin with a `lo-hi` range at all, which is how a truncated read or a header line is rejected
 * rather than parsed into a range of zero that then "contains" a null PC. */
static inline int plx_map_line_kind(const char *line, size_t n, unsigned long pc, unsigned long lr) {
    unsigned long lo = 0, hi = 0;
    const char *p = plx_parse_hex(line, &lo);
    int kind = 0;
    if (!p || *p != '-' || !plx_parse_hex(p + 1, &hi)) return 0;
    /* Half-open, and `lr` is checked as well as `pc`: on ARM the link register is the return
     * address of the frame that faulted, so on a jump through a bad function pointer the PC is
     * garbage and the LR is the only thing that names a real module. */
    if ((pc >= lo && pc < hi) || (lr >= lo && lr < hi)) kind |= PLX_MAP_AT;
    if (plx_names_our_binary(line, n)) kind |= PLX_MAP_BIN;
    return kind;
}

/* The record's FIRST line, which is the one every tool parses:
 *
 *     *** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0xb6f2a1c4 lr=0xb6f2a0e8
 *
 * Returns the length written. `tools/crash-report.sh` reads the signal number and name out of it
 * with one sed and the two addresses with two more, all `head -1` over the block — which is why
 * the register line that follows deliberately repeats neither `pc=` nor `lr=`. */
static inline size_t plx_fmt_signal(char *out, size_t cap, int sig, const char *name,
                                    unsigned long addr, unsigned long pc, unsigned long lr) {
    struct plx_sbuf b = { out, 0, cap };
    plx_s_str(&b, "\n*** SIGNAL ");
    plx_s_dec(&b, (unsigned long)sig);
    plx_s_str(&b, " (");
    plx_s_str(&b, name);
    plx_s_str(&b, ") addr=");
    plx_s_hex(&b, addr);
    plx_s_str(&b, " pc=");
    plx_s_hex(&b, pc);
    plx_s_str(&b, " lr=");
    plx_s_hex(&b, lr);
    plx_s_ch(&b, '\n');
    return b.n;
}

#endif /* PLX_CRASHFMT_H */
