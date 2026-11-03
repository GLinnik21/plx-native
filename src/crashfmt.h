/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository root.
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
static inline int plx_map_line_kind_n(const char *line, size_t n,
                                      const unsigned long *addrs, size_t n_addrs) {
    unsigned long lo = 0, hi = 0;
    const char *p = plx_parse_hex(line, &lo);
    int kind = 0;
    if (!p || *p != '-' || !plx_parse_hex(p + 1, &hi)) return 0;
    /* Half-open, and `lr` is checked as well as `pc`: on ARM the link register is the return
     * address of the frame that faulted, so on a jump through a bad function pointer the PC is
     * garbage and the LR is the only thing that names a real module. A fault passes those two;
     * a freeze passes them and every frame its walk found. */
    for (size_t i = 0; i < n_addrs; i++)
        if (addrs[i] >= lo && addrs[i] < hi) { kind |= PLX_MAP_AT; break; }
    if (plx_names_our_binary(line, n)) kind |= PLX_MAP_BIN;
    return kind;
}

/* A fault's form of the above: the two addresses it knows. */
static inline int plx_map_line_kind(const char *line, size_t n, unsigned long pc, unsigned long lr) {
    const unsigned long a[2] = { pc, lr };
    return plx_map_line_kind_n(line, n, a, 2);
}

/* The record's FIRST line, which is the one every tool parses:
 *
 *     *** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0xb6f2a1c4 lr=0xb6f2a0e8
 *
 * Returns the length written. `tools/crash-report.sh` reads the signal number and name out of it
 * with one sed and the two addresses with two more, all `head -1` over the block — which is why
 * the lines below it (`time:`, `reg:`) deliberately repeat neither `pc=` nor `lr=`. */
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

/* The record's SECOND line: the wall-clock second the crash was written, as Unix seconds.
 *
 *     time: 1750000000
 *
 * Its own line rather than another field on the first, for the reason `reg:` is: every tool that
 * has parsed this file reads the first line's shape (`crash-report.sh` takes `pc=` and `lr=` off
 * the block by sed). The Rust importer (`crashreport::parse`) reads this back as the fallback
 * event's `timestamp`, which is what lets Sentry show WHEN the crash happened and not when the
 * next launch found the log. A record without it (an older build's) is simply sent without one.
 * Same bounded, no-allocation discipline as `plx_fmt_signal`. */
static inline size_t plx_fmt_time(char *out, size_t cap, unsigned long secs) {
    struct plx_sbuf b = { out, 0, cap };
    plx_s_str(&b, "time: ");
    plx_s_dec(&b, secs);
    plx_s_ch(&b, '\n');
    return b.n;
}

/* The record's thread line: WHICH thread took the fault.
 *
 *     thread: tid=4242 main=1 name=plxnative
 *
 * `tid` is `gettid()`, `main` is 1 when it equals the process id (the main thread), and `name` is
 * the kernel's `comm` (`prctl(PR_GET_NAME)`, at most 15 bytes) keeping only `[A-Za-z0-9_.-]`: any
 * other byte becomes `?`, so a name set by a library cannot put a space, a newline or a second
 * record into the log. An empty name is written as `?`. Thread names are the application's and its
 * libraries' own identifiers, never user data. The line sits under `time:` and repeats neither
 * `pc=` nor `lr=`, like `time:` and `reg:`; `crashreport::parse_thread` reads it back. */
static inline size_t plx_fmt_thread(char *out, size_t cap, unsigned long tid, int is_main,
                                    const char *name) {
    struct plx_sbuf b = { out, 0, cap };
    plx_s_str(&b, "thread: tid=");
    plx_s_dec(&b, tid);
    plx_s_str(&b, " main=");
    plx_s_ch(&b, is_main ? '1' : '0');
    plx_s_str(&b, " name=");
    size_t k = 0;
    for (; name && name[k] && k < 15; k++) {
        char c = name[k];
        int ok = (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') ||
                 c == '_' || c == '.' || c == '-';
        plx_s_ch(&b, ok ? c : '?');
    }
    if (k == 0) plx_s_ch(&b, '?');
    plx_s_ch(&b, '\n');
    return b.n;
}

/* The process phase values Rust stores in `plx_crash_phase` (`plx_base::task::watchdog`). */
#define PLX_PHASE_RUNNING  0u   /* the main loop, or before it */
#define PLX_PHASE_SHUTDOWN 1u   /* the loop has ended and the teardown is running */
#define PLX_PHASE_EXITING  2u   /* the teardown is over; the process is ending (exit_now) */

/* The record's phase line: how far the process had got, and how long it had lived.
 *
 *     phase: running uptime=57
 *
 * `uptime` is whole seconds of CLOCK_MONOTONIC since `plx_crash_install`. An unknown phase value
 * is written as `running`'s neighbour `unknown` rather than guessed. */
static inline size_t plx_fmt_phase(char *out, size_t cap, unsigned int phase, unsigned long uptime) {
    struct plx_sbuf b = { out, 0, cap };
    plx_s_str(&b, "phase: ");
    plx_s_str(&b, phase == PLX_PHASE_RUNNING ? "running" :
                  phase == PLX_PHASE_SHUTDOWN ? "shutdown" :
                  phase == PLX_PHASE_EXITING ? "exiting" : "unknown");
    plx_s_str(&b, " uptime=");
    plx_s_dec(&b, uptime);
    plx_s_ch(&b, '\n');
    return b.n;
}

/* ---------------------------------------------------------------------------------------------
 * The FREEZE record: what the main thread was doing when the watchdog saw it stand still.
 *
 * A freeze is not a death, so it has its own header (`*** FREEZE`, never `*** SIGNAL`) and every
 * tool that scans the log for faults keeps skipping it. It is written by the freeze probe's signal
 * handler on the FROZEN thread (`src/crashtrace.c`), and its lines mirror a fault's: the header
 * carries `pc=` and `lr=` (and nothing else of the registers), `time:` the wall-clock second, `reg:`
 * the registers, `at:`/`bin:` the mappings, and one thing a fault does not have, `frames:`.
 * ------------------------------------------------------------------------------------------- */

/* One executable mapping, as the frame walk needs it: `[lo, hi)`. */
struct plx_range { unsigned long lo, hi; };

/* The most executable ranges the walk remembers. A television's app has far fewer than this
 * (the binary, libc, the loader and about thirty libraries), and a range beyond the last slot is
 * dropped: a frame the walk then cannot vouch for ends the walk early, which loses frames and
 * nothing else. */
#define PLX_MAX_EXEC_RANGES 96
/* The most frames one freeze record walks. */
#define PLX_MAX_FREEZE_FRAMES 24
/* The most addresses one freeze record asks the maps scan about: pc, lr and the walked frames. */
#define PLX_MAX_FREEZE_ADDRS (PLX_MAX_FREEZE_FRAMES + 2)

/* If `line` is a `lo-hi perms ...` maps line whose permissions include `x`, store its range. */
static inline int plx_map_line_exec(const char *line, size_t n, struct plx_range *out) {
    unsigned long lo = 0, hi = 0;
    const char *p = plx_parse_hex(line, &lo);
    if (!p || *p != '-') return 0;
    p = plx_parse_hex(p + 1, &hi);
    if (!p || *p != ' ' || hi <= lo) return 0;
    /* "r-xp": four permission characters after the space, the third is execute. */
    if ((size_t)(p + 1 - line) + 4 > n) return 0;
    if (p[3] != 'x') return 0;
    out->lo = lo;
    out->hi = hi;
    return 1;
}

static inline int plx_in_ranges(const struct plx_range *r, size_t nr, unsigned long a) {
    for (size_t i = 0; i < nr; i++)
        if (a >= r[i].lo && a < r[i].hi) return 1;
    return 0;
}

/* If `line` is the `[stack]` mapping and it contains `addr`, store its end. The freeze walk's
 * upper bound: read once at install, on the thread whose stack it is, so the handler never has to
 * ask the kernel which stack a frame pointer belongs to. */
static inline int plx_map_line_stack_end(const char *line, size_t n, unsigned long addr,
                                         unsigned long *hi_out) {
    static const char TAG[] = "[stack]";
    const size_t t = sizeof TAG - 1;
    unsigned long lo = 0, hi = 0;
    const char *p = plx_parse_hex(line, &lo);
    if (!p || *p != '-' || !plx_parse_hex(p + 1, &hi)) return 0;
    while (n && (line[n - 1] == '\n' || line[n - 1] == ' ')) n--;
    if (n < t) return 0;
    for (size_t i = 0; i < t; i++)
        if (line[n - t + i] != TAG[i]) return 0;
    if (addr < lo || addr >= hi) return 0;
    *hi_out = hi;
    return 1;
}

/* Walk the frame-pointer chain from `fp`, writing up to `max` return addresses to `out`.
 *
 * **It only ever dereferences addresses inside `[sp, hi)`**, the interrupted thread's own stack
 * (`hi` is the end of the main thread's `[stack]` mapping, read at install): a fault in the walk
 * would turn a freeze into a crash, so the bound is what is checked, never the pointer's luck.
 * A frame pointer must be word-aligned, inside the window and STRICTLY above the previous one, so
 * the walk terminates and cannot loop on a corrupt chain.
 *
 * **Two frame layouts, because this binary has both.** `force-frame-pointers` makes LLVM (every
 * Rust frame) keep r11 pointing at the saved r11 with the return address one word above it;
 * GCC's ARM-mode frames (the C sources, `-fno-omit-frame-pointer`) keep r11 pointing at the saved
 * return address with the caller's r11 one word BELOW. A chain through Rust and C alternates, and a
 * frame is accepted only when its return address lies in an executable mapping (`exec`), which a
 * stack address or a stale word never does, so trying LLVM's shape first and GCC's second cannot
 * confuse the two. The return address's Thumb bit is cleared. A frame whose layout fits neither
 * ends the walk. The host test grades both layouts on a synthetic stack; that the ARM binary's
 * real frames take those shapes is a claim only a television can show. */
static inline size_t plx_fp_walk(unsigned long sp, unsigned long fp, unsigned long hi,
                                 const struct plx_range *exec, size_t nexec,
                                 unsigned long *out, size_t max) {
    const unsigned long w = sizeof(unsigned long);
    size_t n = 0;
    unsigned long cur = fp;
    while (n < max) {
        /* Written without `cur + 2 * w`: an r11 of 0xFFFFFFF8 (an ordinary value in code with no frame
         * pointer) would wrap that sum to 0 and pass the bound. */
        if (cur % w || cur < sp || cur - sp < w || cur >= hi || hi - cur < 2 * w) break;
        const unsigned long *f = (const unsigned long *)cur;
        unsigned long prev, ret;
        /* A caller's fp is 0 (the chain's end, which a thread's first frame leaves) or strictly
         * above this one and inside the window. */
#define PLX_FP_OK(p) ((p) == 0 || ((p) > cur && (p) < hi))
        if (plx_in_ranges(exec, nexec, f[1]) && PLX_FP_OK(f[0])) {
            prev = f[0]; ret = f[1];                       /* LLVM: [fp]=prev, [fp+w]=ret */
        } else if (plx_in_ranges(exec, nexec, f[0]) && PLX_FP_OK(f[-1])) {
            prev = f[-1]; ret = f[0];                      /* GCC:  [fp]=ret, [fp-w]=prev */
        } else {
            break;
        }
#undef PLX_FP_OK
        out[n++] = ret & ~1ul;
        if (prev == 0) break;
        cur = prev;
    }
    return n;
}

/* The freeze record's FIRST line:
 *
 *     *** FREEZE seq=1 pc=0xb6f2a1c4 lr=0xb6f2a0e8
 *
 * `seq` counts this process's freeze probes from 1; the watchdog's recovery line
 * (`freeze-end: seq=N bucket=...`) names the same number, which is how a recovery finds its
 * record. Like `plx_fmt_signal`, it repeats `pc=`/`lr=` on no later line. */
static inline size_t plx_fmt_freeze(char *out, size_t cap, unsigned long seq,
                                    unsigned long pc, unsigned long lr) {
    struct plx_sbuf b = { out, 0, cap };
    plx_s_str(&b, "\n*** FREEZE seq=");
    plx_s_dec(&b, seq);
    plx_s_str(&b, " pc=");
    plx_s_hex(&b, pc);
    plx_s_str(&b, " lr=");
    plx_s_hex(&b, lr);
    plx_s_ch(&b, '\n');
    return b.n;
}

/* `frames: 0x.. 0x.. ...` — up to `per_line` of the walked return addresses, nearest first,
 * starting at index `from`. Writes the line's length to `*len` and returns how many addresses it
 * held (the caller loops until all are out). */
static inline size_t plx_fmt_frames(char *out, size_t cap, const unsigned long *f, size_t n,
                                    size_t from, size_t per_line, size_t *len) {
    struct plx_sbuf b = { out, 0, cap };
    size_t i = from;
    plx_s_str(&b, "frames:");
    for (; i < n && i - from < per_line; i++) {
        plx_s_ch(&b, ' ');
        plx_s_hex(&b, f[i]);
    }
    plx_s_ch(&b, '\n');
    *len = b.n;
    return i - from;
}

#endif /* PLX_CRASHFMT_H */
