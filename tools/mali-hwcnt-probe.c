/*
 * Standalone userspace probe for the legacy Midgard r12p0 vinstr reader.
 *
 * This is deliberately not linked into PlxNative.  It proves the kernel ABI and mapping contract
 * independently before the in-process Rust sampler is enabled.  No gator module, sysfs write,
 * clock/power-policy change, or privileged kernel operation is involved.
 */

#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

typedef uint16_t u16;
typedef uint32_t u32;
typedef int32_t s32;
typedef uint64_t u64;

#define UK_FUNC_ID 512u
#define KBASE_FUNC_SET_FLAGS (UK_FUNC_ID + 18u)
#define KBASE_FUNC_HWCNT_READER_SETUP (UK_FUNC_ID + 36u)
#define BASE_CONTEXT_SYSTEM_MONITOR_SUBMIT_DISABLED (1u << 1)

/*
 * 1280 bytes per dump and 16 buffers produce a 20480-byte mapping: exactly five 4096-byte pages
 * on the target.  Four buffers, used by the older /home/root probe, produce a 5120-byte nominal
 * length and do not satisfy the reader's page-granular mmap contract.
 */
#define BUFFER_COUNT 16u
#define EXPECTED_API_VERSION 1u
#define EXPECTED_HW_VERSION 5u
#define EXPECTED_DUMP_SIZE 1280u
#define EXPECTED_WORDS (EXPECTED_DUMP_SIZE / sizeof(u32))

union uk_header {
    u32 id;
    u32 ret;
    u64 sizer;
};

struct uku_version_check_args {
    union uk_header header;
    u16 major;
    u16 minor;
    u32 padding;
};

struct kbase_uk_set_flags {
    union uk_header header;
    u32 create_flags;
    u32 padding;
};

struct kbase_uk_hwcnt_reader_setup {
    union uk_header header;
    u32 buffer_count;
    u32 jm_bm;
    u32 shader_bm;
    u32 tiler_bm;
    u32 mmu_l2_bm;
    s32 fd;
};

#define LEGACY_IOCTL(type) _IOWR(0, 0, type)

#define KBASE_HWCNT_READER 0xBE

struct kbase_hwcnt_reader_metadata {
    u64 timestamp;
    u32 event_id;
    u32 buffer_idx;
};

#define KBASE_HWCNT_READER_GET_HWVER _IOR(KBASE_HWCNT_READER, 0x00, u32)
#define KBASE_HWCNT_READER_GET_BUFFER_SIZE _IOR(KBASE_HWCNT_READER, 0x01, u32)
#define KBASE_HWCNT_READER_DUMP _IOW(KBASE_HWCNT_READER, 0x10, u32)
#define KBASE_HWCNT_READER_CLEAR _IOW(KBASE_HWCNT_READER, 0x11, u32)
#define KBASE_HWCNT_READER_GET_BUFFER \
    _IOR(KBASE_HWCNT_READER, 0x20, struct kbase_hwcnt_reader_metadata)
#define KBASE_HWCNT_READER_PUT_BUFFER \
    _IOW(KBASE_HWCNT_READER, 0x21, struct kbase_hwcnt_reader_metadata)
#define KBASE_HWCNT_READER_GET_API_VERSION _IOW(KBASE_HWCNT_READER, 0xFF, u32)

static void fail(const char *what)
{
    fprintf(stderr, "%s: %s (errno=%d)\n", what, strerror(errno), errno);
    exit(EXIT_FAILURE);
}

static void require_uk(int fd, unsigned long request, void *arg, const char *name)
{
    if (ioctl(fd, request, arg) < 0)
        fail(name);
}

static void take_sample(int reader, const uint8_t *mapping, u32 dump_size, u32 *out)
{
    u32 ignored = 0;
    struct pollfd pfd;
    struct kbase_hwcnt_reader_metadata meta;

    if (ioctl(reader, KBASE_HWCNT_READER_DUMP, &ignored) < 0)
        fail("HWCNT_READER_DUMP");

    memset(&pfd, 0, sizeof(pfd));
    pfd.fd = reader;
    pfd.events = POLLIN;
    int ready = poll(&pfd, 1, 2000);
    if (ready < 0)
        fail("poll HWCNT reader");
    if (ready == 0) {
        fputs("poll HWCNT reader: timed out\n", stderr);
        exit(EXIT_FAILURE);
    }

    memset(&meta, 0, sizeof(meta));
    if (ioctl(reader, KBASE_HWCNT_READER_GET_BUFFER, &meta) < 0)
        fail("HWCNT_READER_GET_BUFFER");
    if (meta.buffer_idx >= BUFFER_COUNT) {
        fprintf(stderr, "invalid reader buffer index: %" PRIu32 "\n", meta.buffer_idx);
        exit(EXIT_FAILURE);
    }

    memcpy(out, mapping + (size_t)meta.buffer_idx * dump_size, dump_size);
    if (ioctl(reader, KBASE_HWCNT_READER_PUT_BUFFER, &meta) < 0)
        fail("HWCNT_READER_PUT_BUFFER");

    printf("sample timestamp=%" PRIu64 " event=%" PRIu32 " slot=%" PRIu32 "\n",
           meta.timestamp, meta.event_id, meta.buffer_idx);
}

static void print_nonzero(const u32 *words)
{
    size_t nonzero = 0;
    for (size_t i = 0; i < EXPECTED_WORDS; ++i) {
        if (words[i] != 0) {
            printf("  raw[%3zu]=0x%08" PRIx32 " (%" PRIu32 ")\n",
                   i, words[i], words[i]);
            ++nonzero;
        }
    }
    printf("nonzero=%zu/%zu\n", nonzero, (size_t)EXPECTED_WORDS);
}

int main(int argc, char **argv)
{
    unsigned int samples = 1;
    if (argc == 2) {
        char *end = NULL;
        unsigned long parsed = strtoul(argv[1], &end, 10);
        if (!end || *end != '\0' || parsed == 0 || parsed > 1000) {
            fprintf(stderr, "usage: %s [sample-count:1..1000]\n", argv[0]);
            return EXIT_FAILURE;
        }
        samples = (unsigned int)parsed;
    } else if (argc != 1) {
        fprintf(stderr, "usage: %s [sample-count:1..1000]\n", argv[0]);
        return EXIT_FAILURE;
    }

    int mali = open("/dev/mali0", O_RDWR | O_CLOEXEC);
    if (mali < 0)
        fail("open /dev/mali0");

    struct uku_version_check_args version;
    memset(&version, 0, sizeof(version));
    version.header.id = 0;
    version.major = 10;
    version.minor = 2;
    require_uk(mali, LEGACY_IOCTL(struct uku_version_check_args), &version,
               "UK version check");
    if (version.header.ret != 0 || version.major != 10 || version.minor != 2) {
        fprintf(stderr, "unexpected UK ABI: ret=%" PRIu32 " version=%" PRIu16 ".%" PRIu16 "\n",
                version.header.ret, version.major, version.minor);
        return EXIT_FAILURE;
    }

    struct kbase_uk_set_flags flags;
    memset(&flags, 0, sizeof(flags));
    flags.header.id = KBASE_FUNC_SET_FLAGS;
    flags.create_flags = BASE_CONTEXT_SYSTEM_MONITOR_SUBMIT_DISABLED;
    require_uk(mali, LEGACY_IOCTL(struct kbase_uk_set_flags), &flags, "SET_FLAGS");
    if (flags.header.ret != 0) {
        fprintf(stderr, "SET_FLAGS returned ret=%" PRIu32 "\n", flags.header.ret);
        return EXIT_FAILURE;
    }

    struct kbase_uk_hwcnt_reader_setup setup;
    memset(&setup, 0, sizeof(setup));
    setup.header.id = KBASE_FUNC_HWCNT_READER_SETUP;
    setup.buffer_count = BUFFER_COUNT;
    setup.jm_bm = UINT32_MAX;
    setup.shader_bm = UINT32_MAX;
    setup.tiler_bm = UINT32_MAX;
    setup.mmu_l2_bm = UINT32_MAX;
    setup.fd = -1;
    require_uk(mali, LEGACY_IOCTL(struct kbase_uk_hwcnt_reader_setup), &setup,
               "HWCNT_READER_SETUP");
    if (setup.header.ret != 0 || setup.fd < 0) {
        fprintf(stderr, "HWCNT_READER_SETUP returned ret=%" PRIu32 " fd=%" PRId32 "\n",
                setup.header.ret, setup.fd);
        return EXIT_FAILURE;
    }
    int reader = setup.fd;

    u32 api = 0;
    u32 hwver = 0;
    u32 dump_size = 0;
    if (ioctl(reader, KBASE_HWCNT_READER_GET_API_VERSION, &api) < 0)
        fail("HWCNT_READER_GET_API_VERSION");
    if (ioctl(reader, KBASE_HWCNT_READER_GET_HWVER, &hwver) < 0)
        fail("HWCNT_READER_GET_HWVER");
    if (ioctl(reader, KBASE_HWCNT_READER_GET_BUFFER_SIZE, &dump_size) < 0)
        fail("HWCNT_READER_GET_BUFFER_SIZE");
    if (api != EXPECTED_API_VERSION || hwver != EXPECTED_HW_VERSION ||
        dump_size != EXPECTED_DUMP_SIZE) {
        fprintf(stderr, "unsupported reader contract: api=%" PRIu32 " hwver=%" PRIu32
                        " dump=%" PRIu32 "\n", api, hwver, dump_size);
        return EXIT_FAILURE;
    }

    long page_size = sysconf(_SC_PAGESIZE);
    if (page_size <= 0)
        fail("sysconf(_SC_PAGESIZE)");
    size_t map_size = (size_t)dump_size * BUFFER_COUNT;
    if (map_size % (size_t)page_size != 0) {
        fprintf(stderr, "reader mmap is not page aligned: %zu bytes, page=%ld\n",
                map_size, page_size);
        return EXIT_FAILURE;
    }

    uint8_t *mapping = mmap(NULL, map_size, PROT_READ, MAP_SHARED, reader, 0);
    if (mapping == MAP_FAILED)
        fail("mmap HWCNT reader");

    printf("reader api=%" PRIu32 " hwver=%" PRIu32 " dump=%" PRIu32
           " buffers=%u map=%zu page=%ld\n",
           api, hwver, dump_size, BUFFER_COUNT, map_size, page_size);

    u32 words[EXPECTED_WORDS];
    memset(words, 0, sizeof(words));
    for (unsigned int i = 0; i < samples; ++i) {
        take_sample(reader, mapping, dump_size, words);
        print_nonzero(words);
        if (i + 1 < samples)
            usleep(100000);
    }

    if (munmap(mapping, map_size) < 0)
        fail("munmap HWCNT reader");
    close(reader);
    close(mali);
    return EXIT_SUCCESS;
}
