#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
static unsigned long fixture[8194];
static size_t fixture_len, position, chunk = SIZE_MAX;
static int opens, closes, reads, open_error, read_error, close_error;
static int open_interrupt, read_interrupt, cancellation, delayed_error;
static atomic_int in_read, release_read;
static int fake_open(const char *path, int flags)
{
    assert(strcmp(path, "/proc/self/auxv") == 0);
    assert(flags == (O_RDONLY | O_CLOEXEC));
    ++opens;
    if (open_interrupt) { open_interrupt = 0; errno = EINTR; return -1; }
    if (open_error) { errno = open_error; return -1; }
    return 42;
}
static ssize_t fake_read(int fd, void *dst, size_t size)
{
    assert(fd == 42);
    int previous;
    assert(pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, &previous) == 0);
    assert(previous == PTHREAD_CANCEL_DISABLE);
    ++reads;
    if (cancellation && reads == 1) {
        atomic_store(&in_read, 1);
        while (!atomic_load(&release_read)) usleep(100);
        pthread_testcancel(); /* Must not cancel while owning the fd. */
    }
    if (read_interrupt) { read_interrupt = 0; errno = EINTR; return -1; }
    if (read_error || (delayed_error && position >= 2 * sizeof(long))) {
        errno = read_error ? read_error : ENOSPC; return -1;
    }
    if (size > chunk) size = chunk;
    if (size > fixture_len - position) size = fixture_len - position;
    memcpy(dst, (unsigned char *)fixture + position, size);
    position += size;
    return (ssize_t)size;
}
static int fake_close(int fd)
{
    assert(fd == 42);
    ++closes;
    if (close_error) { errno = close_error; return -1; }
    return 0;
}
#define PLX_AUXV_HOST_TEST
#define PLX_AUXV_OPEN fake_open
#define PLX_AUXV_READ fake_read
#define PLX_AUXV_CLOSE fake_close
#ifndef AUX_SOURCE
#define AUX_SOURCE "../src/compat/getauxval.c"
#endif
#include AUX_SOURCE
static void expect(unsigned long key, unsigned long value, int expected_errno)
{
    errno = EDOM;
    assert(plx_getauxval(key) == value);
    assert(errno == expected_errno);
}
static void *concurrent(void *unused)
{
    (void)unused;
    for (int i = 0; i < 10000; ++i) expect(16, 123, EDOM);
    return NULL;
}
static void *cancelled(void *unused)
{
    (void)unused;
    expect(16, 123, EDOM);
    pthread_testcancel();
    return NULL;
}
int main(int argc, char **argv)
{
    assert(argc == 2);
    const char *test = argv[1];
    unsigned long initial[] = {16, 123, 26, 0, 51, ULONG_MAX, 0, 0};
    memcpy(fixture, initial, sizeof(initial));
    fixture_len = sizeof(initial);
    int failure = 0;
    if (!strcmp(test, "empty")) { fixture_len = 0; failure = EIO; }
    else if (!strcmp(test, "partial")) { fixture_len -= 1; failure = EIO; }
    else if (!strcmp(test, "unterminated")) { fixture_len -= 2 * sizeof(long); failure = EIO; }
    else if (!strcmp(test, "open-error")) { open_error = EACCES; failure = EACCES; }
    else if (!strcmp(test, "late-read-error")) { delayed_error = 1; failure = ENOSPC; }
    else if (!strcmp(test, "read-error")) { read_error = EBADF; failure = EBADF; }
    else if (!strcmp(test, "close-error")) { close_error = EINTR; failure = EINTR; }
    else if (!strcmp(test, "primary-error")) { read_error = EFAULT; close_error = EINTR; failure = EFAULT; }
    else if (!strcmp(test, "short")) chunk = 3;
    else if (!strcmp(test, "eintr")) { open_interrupt = 1; read_interrupt = 1; chunk = 1; }
    else if (!strcmp(test, "long") || !strcmp(test, "bound") || !strcmp(test, "over-bound")) {
        size_t count = !strcmp(test, "long") ? 1000 : 4095;
        if (!strcmp(test, "over-bound")) { count = 4096; failure = E2BIG; }
        for (size_t i = 0; i < count; ++i) { fixture[2*i] = 100 + i; fixture[2*i+1] = i; }
        fixture[2*(count-1)] = 16; fixture[2*(count-1)+1] = 123;
        fixture[2*count] = 0; fixture[2*count+1] = 0;
        fixture_len = (count+1)*2*sizeof(long);
    } else if (!strcmp(test, "concurrent")) {
        pthread_t threads[8];
        for (int i = 0; i < 8; ++i) assert(pthread_create(&threads[i], NULL, concurrent, NULL) == 0);
        for (int i = 0; i < 8; ++i) assert(pthread_join(threads[i], NULL) == 0);
        assert(opens == 1 && closes == 1);
        return 0;
    } else if (!strcmp(test, "cancel")) {
        cancellation = 1;
        pthread_t thread;
        assert(pthread_create(&thread, NULL, cancelled, NULL) == 0);
        while (!atomic_load(&in_read)) usleep(100);
        assert(pthread_cancel(thread) == 0);
        atomic_store(&release_read, 1);
        void *result;
        assert(pthread_join(thread, &result) == 0);
        assert(result == PTHREAD_CANCELED);
        assert(opens == 1 && closes == 1);
        expect(16, 123, EDOM);
        return 0;
    } else if (!strcmp(test, "disabled")) {
        int old_state, restored;
        assert(pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, &old_state) == 0);
        expect(16, 123, EDOM);
        assert(pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, &restored) == 0);
        assert(restored == PTHREAD_CANCEL_DISABLE);
        assert(pthread_setcancelstate(old_state, NULL) == 0);
    } else assert(!strcmp(test, "valid"));
    expect(16, failure ? 0 : 123, failure ? failure : EDOM);
    int cancellation_state;
    assert(pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, &cancellation_state) == 0);
    assert(cancellation_state == PTHREAD_CANCEL_ENABLE);
    assert(pthread_setcancelstate(cancellation_state, NULL) == 0);
    int saved_opens = opens, saved_reads = reads, saved_closes = closes;
    /* Proc access and bytes can change after initialization, without changing results. */
    memset(fixture, 0, sizeof(fixture));
    open_error = ENOENT; read_error = EIO;
    expect(16, failure ? 0 : 123, failure ? failure : EDOM);
    assert(opens == saved_opens && reads == saved_reads && closes == saved_closes);
    assert(closes == (!strcmp(test, "open-error") ? 0 : 1));
    if (!failure) {
        expect(0, 0, ENOENT);
        expect(99999, 0, ENOENT);
        if (!strcmp(test, "valid") || !strcmp(test, "short") || !strcmp(test, "eintr")) {
            expect(26, 0, EDOM);
            expect(51, ULONG_MAX, EDOM);
        }
    }
    puts(test);
    return 0;
}
