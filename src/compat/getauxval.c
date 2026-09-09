/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (c) 2026 Gleb Linnik and contributors.
 * Authored from the public ABI contract and Linux man-pages cited there.
 */
/* Bounded Linux auxv fallback, GPL-3.0-or-later, replacing the old NDK libglibc_polyfills.a.
 * Normal-thread use only; no signal-handler/loader reentrancy contract.
 * AT_SECURE is not supported as a basis for security decisions.
 */
#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <pthread.h>
#include <unistd.h>

struct aux_record { unsigned long key, value; };
_Static_assert(sizeof(struct aux_record) == 2 * sizeof(unsigned long), "auxv pair layout");
_Static_assert(offsetof(struct aux_record, value) == sizeof(unsigned long), "auxv value offset");
#ifdef PLX_AUXV_HOST_TEST
#define AUX_ENTRY plx_getauxval
#else
#if !defined(__arm__) || !defined(__linux__)
#error "getauxval fallback is only authorized for ARM32 Linux"
#endif
_Static_assert(sizeof(unsigned long) == 4 && sizeof(void *) == 4, "ARM32 ABI required");
#define AUX_ENTRY getauxval
#endif

#ifndef PLX_AUXV_OPEN
#define PLX_AUXV_OPEN open
#define PLX_AUXV_READ read
#define PLX_AUXV_CLOSE close
#endif
#define AUX_CAPACITY 4096
static struct aux_record aux_records[AUX_CAPACITY];
static size_t aux_count;
static int aux_error;
static pthread_once_t aux_once = PTHREAD_ONCE_INIT;

static void aux_initialize(void)
{
    int fd;
    do { fd = PLX_AUXV_OPEN("/proc/self/auxv", O_RDONLY | O_CLOEXEC); }
    while (fd < 0 && errno == EINTR);
    if (fd < 0) { aux_error = errno; return; }
    int failure = E2BIG;
    for (size_t i = 0; i < AUX_CAPACITY; ++i) {
        unsigned char *bytes = (unsigned char *)&aux_records[i];
        size_t offset = 0;
        while (offset < sizeof(struct aux_record)) {
            ssize_t n = PLX_AUXV_READ(fd, bytes + offset, sizeof(struct aux_record) - offset);
            if (n < 0) {
                if (errno == EINTR) continue;
                failure = errno;
                goto done;
            }
            if (n == 0) { failure = EIO; goto done; }
            offset += (size_t)n;
        }
        if (aux_records[i].key == 0) {
            aux_count = i;
            failure = 0;
            break;
        }
    }
done:
    /* Linux close EINTR consumes the descriptor: never retry it. */
    if (PLX_AUXV_CLOSE(fd) < 0 && failure == 0) failure = errno;
    aux_error = failure;
}

__attribute__((visibility("hidden")))
unsigned long AUX_ENTRY(unsigned long key)
{
    int incoming = errno, old_state;
    int error = pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, &old_state);
    if (error != 0) { errno = error; return 0; }
    error = pthread_once(&aux_once, aux_initialize);
    unsigned long value = 0;
    int result_errno = error ? error : aux_error;
    if (result_errno == 0) {
        result_errno = ENOENT;
        if (key != 0) {
            for (size_t i = 0; i < aux_count; ++i) {
                if (aux_records[i].key == key) {
                    value = aux_records[i].value;
                    result_errno = incoming;
                    break;
                }
            }
        }
    }
    /* No fd remains owned, including when restoring delivers cancellation. */
    (void)pthread_setcancelstate(old_state, NULL);
    errno = result_errno;
    return value;
}
