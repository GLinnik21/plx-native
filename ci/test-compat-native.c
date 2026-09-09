/* Build only on Linux: compare safe scalar keys in this same process. */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include <assert.h>
#include <errno.h>
#include <sys/auxv.h>
unsigned long plx_getauxval(unsigned long);
int main(void)
{
    /* HWCAP is intentionally excluded: libc may normalize raw capabilities. */
    const unsigned long keys[] = {AT_PAGESZ, AT_PHENT, AT_PHNUM, 26, 51};
    for (unsigned i = 0; i < sizeof(keys)/sizeof(keys[0]); ++i) {
        errno = 0;
        unsigned long native = getauxval(keys[i]);
        int native_error = errno;
        errno = 0;
        assert(plx_getauxval(keys[i]) == native);
        assert(errno == native_error);
    }
    return 0;
}
