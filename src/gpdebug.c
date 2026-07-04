/* gpdebug.c — ElectricFence-style guard-page allocator (DEBUG BUILD ONLY, not
 * part of the normal build). Every allocation is placed flush against a trailing
 * PROT_NONE guard page, so a heap overflow faults on the exact instruction doing
 * the overflowing write — in our code, with our symbols — instead of corrupting
 * an adjacent object that crashes frames later somewhere else. Interposes the
 * process-wide allocator. Enormous memory + syscall cost; only for pinpointing
 * the modular-split heap-corruption bug. */
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>

#define GP_PAGE  4096u
#define GP_MAGIC ((size_t)0xE0F0BEEFu)

/* Header lives in the 4 pointer-sized slots just below the returned object
 * (obj is 16-aligned; we reserve 32 bytes so it fits on both 32- and 64-bit):
 *   [-1]=base  [-2]=maplen  [-3]=objsize  [-4]=magic */
static void *gp_alloc(size_t n) {
    if (n == 0) n = 1;
    size_t need   = (n + 15u) & ~(size_t)15u;
    size_t datasz = (need + 32u + GP_PAGE - 1u) & ~(size_t)(GP_PAGE - 1u);
    size_t maplen = datasz + GP_PAGE;
    unsigned char *base = mmap(0, maplen, PROT_READ | PROT_WRITE,
                               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (base == (unsigned char *)MAP_FAILED) return 0;
    mprotect(base + datasz, GP_PAGE, PROT_NONE);   /* overflow past obj -> SIGSEGV */
    unsigned char *obj = base + datasz - need;     /* object END == guard boundary */
    ((void   **)obj)[-1] = base;
    ((size_t  *)obj)[-2] = maplen;
    ((size_t  *)obj)[-3] = need;
    ((size_t  *)obj)[-4] = GP_MAGIC;
    return obj;
}
static int gp_mine(void *p, size_t *objsize, size_t *maplen, void **base) {
    if (!p || ((uintptr_t)p & 15u)) return 0;
    unsigned char *obj = p;
    if (((size_t *)obj)[-4] != GP_MAGIC) return 0;
    if (objsize) *objsize = ((size_t *)obj)[-3];
    if (maplen)  *maplen  = ((size_t *)obj)[-2];
    if (base)    *base    = ((void  **)obj)[-1];
    return 1;
}
void free(void *p) {
    size_t maplen; void *base;
    if (gp_mine(p, 0, &maplen, &base)) munmap(base, maplen);
    /* not ours (pre-init/uninterposed alloc): leak rather than corrupt/crash */
}
void *malloc(size_t n) { return gp_alloc(n); }
void *calloc(size_t a, size_t b) {
    size_t n = a * b;
    if (a && n / a != b) return 0;         /* size overflow */
    return gp_alloc(n);                     /* mmap already zero-fills */
}
void *realloc(void *p, size_t n) {
    if (!p) return gp_alloc(n);
    if (n == 0) { free(p); return 0; }
    size_t old = 0; gp_mine(p, &old, 0, 0);
    void *np = gp_alloc(n);
    if (np && old) memcpy(np, p, old < n ? old : n);
    free(p);
    return np;
}
void *memalign(size_t a, size_t n)  { (void)a; return gp_alloc(n); }
void *aligned_alloc(size_t a, size_t n) { (void)a; return gp_alloc(n); }
void *valloc(size_t n) { return gp_alloc(n); }
int posix_memalign(void **out, size_t a, size_t n) {
    (void)a; void *p = gp_alloc(n); if (!p) return 12; *out = p; return 0;
}
