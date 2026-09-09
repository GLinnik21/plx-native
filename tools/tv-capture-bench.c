/* tv-capture-bench — measure the firmware planes before building a screen recorder.
 *
 * This standalone ARM diagnostic is never linked into PlxNative or included in an install.
 *
 *   vtm [frames] [display|scaler]
 *       Keep LG's video-frame-output device open, wait its real vsync, and read rotating DMA
 *       descriptors. No plane is mmap'd or copied. This proves the video-plane tap only.
 *
 *   osd [iterations]
 *       Read current OSD framebuffer descriptors through DILE_GAL. This measures descriptor
 *       access only; it does not copy the graphics plane.
 *
 *   venc [frames] [width height] [bitrate_kbps]
 *       Feed synthetic NV12 frames to the firmware H.264 encoder and measure sustained latency.
 *       This proves encoder capacity and accepted dimensions, not video/OSD composition.
 *
 *   stream [frames] [port] [width height] [bitrate_kbps]
 *       Map the real VTM NV12 planes read-only, feed them to the firmware H.264 encoder, and
 *       serve the elementary stream over TCP. The peer receives raw H.264, with no file/container.
 *
 * Build: make tv-capture-bench
 *
 * ABI evidence (LG 49SM9000PLA, webOS 4.5 firmware dated 2025-08-27):
 * tvservice build-id a7dd4292c430b69b22c1e2278eee8794d31c4f27, ARM32 EABI5;
 * libdile_vt.so.0 build-id ab115ec43e101532e8a81a11892efdc9d245380d.
 *
 * Layouts come from that tvservice's own DWARF for dile_vt.h:229 and dile_gal.h:169.
 * Keep the assertions: a wrong layout lets a firmware library write into arbitrary memory.
 */

/* This is a Linux-only ARM firmware diagnostic, so pull in the full glibc surface rather than
 * relying on the compiler's default dialect: O_CLOEXEC (fcntl.h) and mmap64/off64_t (sys/mman.h)
 * are GNU/LFS extensions glibc hides under strict ISO C. macOS's headers expose these
 * unconditionally regardless of -std=, which is why this only broke `make check` on the Linux CI
 * runner and not on the dev Mac — see tools/test_tv_capture_bench.py's `-std=c11` harness build.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <arpa/inet.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

enum {
    DILE_VT_SCALER_OUTPUT = 0,
    DILE_VT_DISPLAY_OUTPUT = 1,
    MAX_VTM_BUFFERS = 16,
    MAX_VTM_PLANES = 4,
};

struct dile_vt_fb_capability {
    uint32_t num_buffers;
    uint32_t num_planes;
};

struct dile_vt_rect {
    uint16_t x;
    uint16_t y;
    uint16_t width;
    uint16_t height;
};

struct dile_vt_limitation {
    struct dile_vt_rect max_resolution;
    uint8_t left_top_align;
    uint8_t input_deinterlace;
    uint8_t display_deinterlace;
    uint8_t scale_up;
    uint32_t scale_up_limit_width;
    uint32_t scale_up_limit_height;
    uint8_t scale_down;
    uint8_t padding[3];
    uint32_t scale_down_limit_width;
    uint32_t scale_down_limit_height;
};

struct dile_vt_fb_property {
    uint32_t pixel_format;
    uint32_t stride;
    uint32_t width;
    uint32_t height;
    uint32_t **physical_addresses;
};

struct dile_gal_palette {
    uint32_t palette[256];
    uint32_t length;
};

struct dile_gal_surface {
    uint32_t offset;
    uint32_t physical_address;
    uint16_t pitch;
    uint16_t bpp;
    uint16_t width;
    uint16_t height;
    uint32_t pixel_format;
    struct dile_gal_palette palette_info;
    uint32_t property;
    uint32_t vendor_data;
};

_Static_assert(sizeof(struct dile_vt_fb_capability) == 0x08,
               "DILE_VT framebuffer capability ABI changed");
_Static_assert(sizeof(struct dile_vt_rect) == 0x08, "DILE_VT rect ABI changed");
_Static_assert(sizeof(struct dile_vt_limitation) == 0x20,
               "DILE_VT limitation ABI changed");
_Static_assert(sizeof(struct dile_vt_fb_property) == 0x14,
               "DILE_VT framebuffer property ABI changed");
_Static_assert(sizeof(struct dile_gal_palette) == 0x404,
               "DILE_GAL palette ABI changed");
_Static_assert(sizeof(struct dile_gal_surface) == 0x420,
               "DILE_GAL surface ABI changed");

typedef int (*noarg_fn)(void);
typedef void *(*vt_create_fn)(uint32_t window);
typedef int (*vt_destroy_fn)(void *handle);
typedef int (*vt_start_fn)(uint32_t *lock_mode);
typedef int (*vt_set_dump_fn)(void *handle, uint32_t location);
typedef int (*vt_get_capability_fn)(void *handle, struct dile_vt_fb_capability *out);
typedef int (*vt_get_limitation_fn)(void *handle, struct dile_vt_limitation *out);
typedef int (*vt_set_region_fn)(void *handle, uint32_t location,
                                const struct dile_vt_rect *region);
typedef int (*vt_get_rate_fn)(void *handle, uint32_t *out_rate);
typedef int (*vt_wait_vsync_fn)(void *handle);
typedef int (*vt_get_all_fn)(void *handle, const struct dile_vt_fb_capability *capability,
                             struct dile_vt_fb_property *out);
typedef int (*vt_get_current_fn)(void *handle, struct dile_vt_fb_property *out,
                                 uint32_t *out_index);

struct vt_api {
    noarg_fn init;
    noarg_fn finalize;
    vt_create_fn create;
    vt_destroy_fn destroy;
    vt_start_fn start;
    noarg_fn stop;
    vt_set_dump_fn set_dump;
    vt_get_capability_fn get_capability;
    vt_get_limitation_fn get_limitation;
    vt_set_region_fn set_region;
    vt_get_rate_fn get_rate;
    vt_wait_vsync_fn wait_vsync;
    vt_get_all_fn get_all;
    vt_get_current_fn get_current;
};

typedef int (*gal_get_list_fn)(struct dile_gal_surface **out, uint32_t *out_count);

struct gal_api {
    noarg_fn init;
    noarg_fn finalize;
    gal_get_list_fn get_list;
};

enum {
    VENC_PIXEL_FORMAT_NV12 = 1,
    VENC_CODEC_H264 = 0,
    VENC_FRAMERATE_60P = 2,
    VENC_FRAME_BANK = 4,
};

/* Recovered from HAL_VENC_CODEC_Create/Encode in libhal_lg115x.so.2.0.1. The create function
 * copies these eight words in order into the KADP create request. Encode reads three plane
 * pointers at +12, their lengths at +24, and writes a 12-byte packet descriptor. */
struct venc_config {
    uint32_t pixel_format;
    uint32_t codec;
    uint32_t width;
    uint32_t height;
    uint32_t framerate;
    uint32_t target_bitrate_kbps;
    uint32_t gop;
    uint32_t qp;
};

struct venc_input {
    uint32_t reserved[3];
    void *planes[3];
    uint32_t sizes[3];
};

struct venc_output {
    void *data;
    uint32_t size;
    uint32_t keyframe;
};

_Static_assert(sizeof(struct venc_config) == 0x20, "HAL VENC config ABI changed");
_Static_assert(sizeof(struct venc_input) == 0x24, "HAL VENC input ABI changed");
_Static_assert(sizeof(struct venc_output) == 0x0c, "HAL VENC output ABI changed");

typedef int (*venc_create_fn)(const struct venc_config *config, void **out_context);
typedef int (*venc_encode_fn)(void *context, const struct venc_input *input,
                              struct venc_output *output);
typedef int (*venc_destroy_fn)(void *context);
typedef int (*venc_release_fn)(struct venc_output *output);

struct venc_api {
    venc_create_fn create;
    venc_encode_fn encode;
    venc_destroy_fn destroy;
    venc_release_fn release;
};

static volatile sig_atomic_t interrupted;

static void on_signal(int sig)
{
    (void)sig;
    interrupted = 1;
}

static uint64_t now_ns(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        fprintf(stderr, "clock_gettime: %s\n", strerror(errno));
        exit(EXIT_FAILURE);
    }
    return (uint64_t)ts.tv_sec * UINT64_C(1000000000) + (uint64_t)ts.tv_nsec;
}

static int sleep_until_ns(uint64_t deadline_ns)
{
    struct timespec deadline;
    int rc;
    deadline.tv_sec = (time_t)(deadline_ns / UINT64_C(1000000000));
    deadline.tv_nsec = (long)(deadline_ns % UINT64_C(1000000000));
    do {
        rc = clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &deadline, NULL);
    } while (rc == EINTR && !interrupted);
    return rc;
}

static int cmp_u64(const void *a, const void *b)
{
    uint64_t x = *(const uint64_t *)a;
    uint64_t y = *(const uint64_t *)b;
    return (x > y) - (x < y);
}

static double percentile_ms(uint64_t *sorted, uint32_t n, uint32_t percentile)
{
    uint64_t rank = ((uint64_t)n * percentile + 99) / 100;
    uint32_t index = rank ? (uint32_t)(rank - 1) : 0;
    if (index >= n)
        index = n - 1;
    return sorted[index] / 1000000.0;
}

static int bind_symbol(void *library, const char *name, void *out, size_t out_size)
{
    void *symbol;
    const char *error;

    dlerror();
    symbol = dlsym(library, name);
    error = dlerror();
    if (error || !symbol) {
        fprintf(stderr, "dlsym(%s): %s\n", name, error ? error : "missing symbol");
        return -1;
    }
    if (out_size != sizeof symbol) {
        fprintf(stderr, "dlsym(%s): function pointer size mismatch\n", name);
        return -1;
    }
    memcpy(out, &symbol, sizeof symbol);
    return 0;
}

#define BIND(lib, object, member, name) \
    do { \
        if (bind_symbol((lib), (name), &(object).member, sizeof((object).member)) != 0) \
            return -1; \
    } while (0)

static void *open_library_flags(const char *const *candidates, int flags)
{
    const char *const *candidate;
    for (candidate = candidates; *candidate; ++candidate) {
        void *library = dlopen(*candidate, flags);
        if (library) {
            printf("firmware: loaded %s\n", *candidate);
            return library;
        }
    }
    fprintf(stderr, "dlopen: %s\n", dlerror());
    return NULL;
}

static void *open_library(const char *const *candidates)
{
    return open_library_flags(candidates, RTLD_NOW | RTLD_LOCAL);
}

static int preload_pmlog(void)
{
    static const char *const candidates[] = {
        "libPmLogLib.so.3", "libPmLogLib.so.3.3.0", NULL
    };
    static void *library;
    const char *const *candidate;

    if (library)
        return 0;
    for (candidate = candidates; *candidate; ++candidate) {
        library = dlopen(*candidate, RTLD_NOW | RTLD_GLOBAL);
        if (library)
            return 0;
    }
    fprintf(stderr, "dlopen PmLog dependency: %s\n", dlerror());
    return -1;
}

static int load_vt(void **out_library, struct vt_api *api)
{
    static const char *const candidates[] = {
        "libdile_vt.so.0", "libdile_vt.so.0.1.0", NULL
    };
    void *library;
    /* libdile_vt uses _PmLogMsgKV without declaring libPmLogLib in DT_NEEDED. tvservice already
     * has it in the global namespace; a standalone probe must reproduce that loader state. */
    if (preload_pmlog() != 0)
        return -1;
    library = open_library(candidates);
    if (!library)
        return -1;
    memset(api, 0, sizeof *api);
    BIND(library, *api, init, "DILE_VT_Init");
    BIND(library, *api, finalize, "DILE_VT_Finalize");
    BIND(library, *api, create, "DILE_VT_Create");
    BIND(library, *api, destroy, "DILE_VT_Destroy");
    BIND(library, *api, start, "DILE_VT_Start");
    BIND(library, *api, stop, "DILE_VT_Stop");
    BIND(library, *api, set_dump, "DILE_VT_SetVideoFrameOutputDeviceDumpLocation");
    BIND(library, *api, get_capability, "DILE_VT_GetVideoFrameBufferCapability");
    BIND(library, *api, get_limitation, "DILE_VT_GetVideoFrameOutputDeviceLimitation");
    BIND(library, *api, set_region, "DILE_VT_SetVideoFrameOutputDeviceOutputRegion");
    BIND(library, *api, get_rate, "DILE_VT_GetVideoFrameOutputDeviceFramerate");
    BIND(library, *api, wait_vsync, "DILE_VT_WaitVsync");
    BIND(library, *api, get_all, "DILE_VT_GetAllVideoFrameBufferProperty");
    BIND(library, *api, get_current, "DILE_VT_GetCurrentVideoFrameBufferProperty");
    *out_library = library;
    return 0;
}

static int load_gal(void **out_library, struct gal_api *api)
{
    static const char *const candidates[] = {
        "libdile_gal.so.1", "libdile_gal.so.1.0.2", NULL
    };
    void *library = open_library(candidates);
    if (!library)
        return -1;
    memset(api, 0, sizeof *api);
    BIND(library, *api, init, "DILE_GAL_Init");
    BIND(library, *api, finalize, "DILE_GAL_Finalize");
    BIND(library, *api, get_list, "DILE_GAL_GetFrameBufferList");
    *out_library = library;
    return 0;
}

static int load_venc(void **out_library, struct venc_api *api)
{
    static const char *const candidates[] = {
        "libhal_lg115x.so.2", "libhal_lg115x.so.2.0.1", NULL
    };
    void *library;
    if (preload_pmlog() != 0)
        return -1;
    /* libhal_lg115x is a monolith with unresolved references for unrelated TV subsystems.
     * tvservice supplies all of them globally; this probe needs only the VENC branch. */
    library = open_library_flags(candidates, RTLD_LAZY | RTLD_LOCAL);
    if (!library)
        return -1;
    memset(api, 0, sizeof *api);
    BIND(library, *api, create, "HAL_VENC_CODEC_Create");
    BIND(library, *api, encode, "HAL_VENC_CODEC_Encode");
    BIND(library, *api, destroy, "HAL_VENC_CODEC_Destroy");
    BIND(library, *api, release, "HAL_VENC_CODEC_ReleaseFrameData");
    *out_library = library;
    return 0;
}

static const char *pixel_format_name(uint32_t format)
{
    static const char *const names[] = {
        "YUV420P", "YUV420SP", "YUV420I", "YUV422P", "YUV422SP", "YUV422I",
        "YUV444P", "YUV444SP", "YUV444I", "RGB", "ARGB"
    };
    return format < sizeof names / sizeof names[0] ? names[format] : "unknown";
}

static uint32_t parse_count(const char *text, uint32_t default_value, uint32_t max_value)
{
    char *end = NULL;
    unsigned long value;
    if (!text)
        return default_value;
    errno = 0;
    value = strtoul(text, &end, 10);
    if (errno || !end || *end || value == 0 || value > max_value)
        return 0;
    return (uint32_t)value;
}

struct mapped_plane {
    void *mapping;
    size_t mapping_size;
    unsigned char *data;
};

static int map_physical_plane(int mem_fd, uint32_t physical_address, size_t data_size,
                              struct mapped_plane *out)
{
    long page_size = sysconf(_SC_PAGESIZE);
    uint64_t page_mask;
    uint64_t page_base;
    size_t page_offset;
    size_t mapping_size;
    void *mapping;

    memset(out, 0, sizeof *out);
    if (page_size <= 0 || ((unsigned long)page_size & ((unsigned long)page_size - 1)) != 0) {
        fputs("invalid system page size\n", stderr);
        return -1;
    }
    if (physical_address == 0 || physical_address == UINT32_MAX || data_size == 0) {
        fprintf(stderr, "invalid physical plane: address=%08" PRIx32 " size=%zu\n",
                physical_address, data_size);
        return -1;
    }
    page_mask = (uint64_t)page_size - 1;
    page_base = (uint64_t)physical_address & ~page_mask;
    page_offset = (size_t)((uint64_t)physical_address - page_base);
    if (data_size > SIZE_MAX - page_offset) {
        fputs("physical plane mapping size overflow\n", stderr);
        return -1;
    }
    mapping_size = page_offset + data_size;
    mapping = mmap64(NULL, mapping_size, PROT_READ, MAP_SHARED, mem_fd, (off64_t)page_base);
    if (mapping == MAP_FAILED) {
        fprintf(stderr, "mmap64 physical plane %08" PRIx32 ": %s\n",
                physical_address, strerror(errno));
        return -1;
    }
    out->mapping = mapping;
    out->mapping_size = mapping_size;
    out->data = (unsigned char *)mapping + page_offset;
    return 0;
}

static void unmap_physical_plane(struct mapped_plane *plane)
{
    if (plane->mapping && munmap(plane->mapping, plane->mapping_size) != 0)
        fprintf(stderr, "warning: munmap physical plane: %s\n", strerror(errno));
    memset(plane, 0, sizeof *plane);
}

static int accept_stream_client(uint16_t port, int *out_listener)
{
    struct sockaddr_in address;
    struct timeval timeout;
    int one = 1;
    int listener;
    int client;

    *out_listener = -1;
    listener = socket(AF_INET, SOCK_STREAM, 0);
    if (listener < 0) {
        fprintf(stderr, "socket: %s\n", strerror(errno));
        return -1;
    }
    setsockopt(listener, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    memset(&address, 0, sizeof address);
    address.sin_family = AF_INET;
    address.sin_port = htons(port);
    address.sin_addr.s_addr = htonl(INADDR_ANY);
    if (bind(listener, (const struct sockaddr *)&address, sizeof address) != 0 ||
        listen(listener, 1) != 0) {
        fprintf(stderr, "bind/listen on :%" PRIu16 ": %s\n", port, strerror(errno));
        close(listener);
        return -1;
    }
    printf("stream: listening on :%" PRIu16 " for one raw H.264 client\n", port);
    fflush(stdout);
    do {
        client = accept(listener, NULL, NULL);
    } while (client < 0 && errno == EINTR && !interrupted);
    if (client < 0) {
        fprintf(stderr, "accept: %s\n", strerror(errno));
        close(listener);
        return -1;
    }
    setsockopt(client, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
    timeout.tv_sec = 2;
    timeout.tv_usec = 0;
    setsockopt(client, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof timeout);
    *out_listener = listener;
    puts("stream: client connected");
    return client;
}

static int send_all_fd(int fd, const void *data, size_t size)
{
    const unsigned char *bytes = data;
    size_t offset = 0;
    while (offset < size) {
        ssize_t sent = send(fd, bytes + offset, size - offset, MSG_NOSIGNAL);
        if (sent < 0 && errno == EINTR)
            continue;
        if (sent <= 0)
            return -1;
        offset += (size_t)sent;
    }
    return 0;
}

static int packet_has_annex_b_start_code(const unsigned char *data, size_t size)
{
    size_t i;
    size_t limit = size < 64 ? size : 64;
    for (i = 0; i + 3 < limit; ++i) {
        if (data[i] == 0 && data[i + 1] == 0 &&
            (data[i + 2] == 1 || (data[i + 2] == 0 && data[i + 3] == 1)))
            return 1;
    }
    return 0;
}

static void print_plane_sample(const char *name, const unsigned char *data, size_t size)
{
    uint64_t sum = 0;
    unsigned min_value = 255;
    unsigned max_value = 0;
    size_t i;
    size_t shown = size < 8 ? size : 8;

    for (i = 0; i < size; ++i) {
        unsigned value = data[i];
        if (value < min_value)
            min_value = value;
        if (value > max_value)
            max_value = value;
        sum += value;
    }
    printf("stream: %s bytes=%zu min=%u max=%u mean=%.2f head=", name, size,
           min_value, max_value, size ? (double)sum / size : 0.0);
    for (i = 0; i < shown; ++i)
        printf("%02x", data[i]);
    putchar('\n');
}

struct osd_overlay {
    struct gal_api api;
    struct dile_gal_surface surface;
    struct mapped_plane pixels;
    void *library;
    uint16_t *x_map;
    uint16_t *y_map;
    unsigned char *y;
    unsigned char *u;
    unsigned char *v;
    unsigned char *a;
    unsigned char *uv_a;
    uint32_t content_hash;
    uint32_t output_width;
    uint32_t output_height;
    uint32_t min_x;
    uint32_t min_y;
    uint32_t max_x;
    uint32_t max_y;
    int initialized;
    int cache_valid;
    int has_pixels;
};

static unsigned char clamp_byte(int value)
{
    if (value < 0)
        return 0;
    if (value > 255)
        return 255;
    return (unsigned char)value;
}

static unsigned char blend_byte(unsigned char background, unsigned char foreground,
                                unsigned alpha)
{
    unsigned value = foreground * alpha + background * (255U - alpha) + 128U;
    return (unsigned char)((value + (value >> 8)) >> 8);
}

static uint32_t hash_osd_pixels(const struct osd_overlay *osd)
{
    uint32_t hash = UINT32_C(2166136261);
    uint32_t y;
    size_t row_size = (size_t)osd->surface.width * 4;
    for (y = 0; y < osd->surface.height; ++y) {
        const unsigned char *row = osd->pixels.data + (size_t)y * osd->surface.pitch;
        size_t x;
        for (x = 0; x < row_size; ++x) {
            hash ^= row[x];
            hash *= UINT32_C(16777619);
        }
    }
    return hash;
}

static int update_osd_cache(struct osd_overlay *osd)
{
    uint32_t hash;
    uint32_t x;
    uint32_t y;

    /* The first UI prototype deliberately freezes one OSD snapshot for the stream. Re-hashing
     * 324 KiB every output frame cost more than the hardware encoder on this 32-bit process;
     * production live UI needs the compositor's dirty/vsync signal, not blind polling. */
    if (osd->cache_valid)
        return 0;
    hash = hash_osd_pixels(osd);
    osd->content_hash = hash;
    osd->cache_valid = 1;
    osd->has_pixels = 0;
    osd->min_x = osd->output_width;
    osd->min_y = osd->output_height;
    osd->max_x = 0;
    osd->max_y = 0;
    for (y = 0; y < osd->output_height; ++y) {
        const unsigned char *row = osd->pixels.data +
                                   (size_t)osd->y_map[y] * osd->surface.pitch;
        for (x = 0; x < osd->output_width; ++x) {
            const unsigned char *pixel = row + (size_t)osd->x_map[x] * 4;
            size_t index = (size_t)y * osd->output_width + x;
            int blue = pixel[0];
            int green = pixel[1];
            int red = pixel[2];
            unsigned alpha = pixel[3];
            osd->y[index] = clamp_byte(16 + ((66 * red + 129 * green + 25 * blue + 128) >> 8));
            osd->a[index] = (unsigned char)alpha;
            if (alpha != 0) {
                if (!osd->has_pixels || x < osd->min_x)
                    osd->min_x = x;
                if (!osd->has_pixels || y < osd->min_y)
                    osd->min_y = y;
                if (!osd->has_pixels || x > osd->max_x)
                    osd->max_x = x;
                if (!osd->has_pixels || y > osd->max_y)
                    osd->max_y = y;
                osd->has_pixels = 1;
            }
        }
    }
    for (y = 0; y < osd->output_height; y += 2) {
        uint32_t sample_y = y + 1 < osd->output_height ? y + 1 : y;
        const unsigned char *row = osd->pixels.data +
                                   (size_t)osd->y_map[sample_y] * osd->surface.pitch;
        for (x = 0; x < osd->output_width; x += 2) {
            uint32_t sample_x = x + 1 < osd->output_width ? x + 1 : x;
            const unsigned char *pixel = row + (size_t)osd->x_map[sample_x] * 4;
            size_t index = (size_t)(y / 2) * (osd->output_width / 2) + x / 2;
            int blue = pixel[0];
            int green = pixel[1];
            int red = pixel[2];
            osd->u[index] = clamp_byte(128 + ((-38 * red - 74 * green + 112 * blue + 128) >> 8));
            osd->v[index] = clamp_byte(128 + ((112 * red - 94 * green - 18 * blue + 128) >> 8));
            osd->uv_a[index] = pixel[3];
        }
    }
    return 1;
}

static int setup_osd_overlay(int mem_fd, uint32_t output_width, uint32_t output_height,
                             struct osd_overlay *osd)
{
    struct dile_gal_surface *surfaces = NULL;
    uint32_t count = 0;
    uint32_t selected = UINT32_MAX;
    uint32_t i;
    size_t output_pixels;
    size_t chroma_samples;

    memset(osd, 0, sizeof *osd);
    if (load_gal(&osd->library, &osd->api) != 0)
        goto fail;
    if (osd->api.init() != 0) {
        fputs("DILE_GAL_Init failed for stream-ui\n", stderr);
        goto fail;
    }
    osd->initialized = 1;
    if (osd->api.get_list(&surfaces, &count) != 0 || !surfaces || count == 0) {
        fputs("DILE_GAL_GetFrameBufferList failed for stream-ui\n", stderr);
        goto fail;
    }
    for (i = 0; i < count; ++i) {
        if (surfaces[i].pixel_format == 0 && surfaces[i].bpp == 32 &&
            surfaces[i].width != 0 && surfaces[i].height != 0 &&
            surfaces[i].pitch >= surfaces[i].width * 4U) {
            selected = i;
            break;
        }
    }
    if (selected == UINT32_MAX) {
        fputs("stream-ui found no ARGB8888 OSD surface\n", stderr);
        goto fail;
    }
    osd->surface = surfaces[selected];
    free(surfaces);
    surfaces = NULL;
    if (osd->surface.width > 4096 || osd->surface.height > 4096) {
        fputs("stream-ui OSD dimensions are implausible\n", stderr);
        goto fail;
    }
    if (map_physical_plane(mem_fd, osd->surface.physical_address,
                           (size_t)osd->surface.pitch * osd->surface.height,
                           &osd->pixels) != 0)
        goto fail;
    output_pixels = (size_t)output_width * output_height;
    chroma_samples = output_pixels / 4;
    osd->output_width = output_width;
    osd->output_height = output_height;
    osd->x_map = malloc((size_t)output_width * sizeof *osd->x_map);
    osd->y_map = malloc((size_t)output_height * sizeof *osd->y_map);
    osd->y = malloc(output_pixels);
    osd->u = malloc(chroma_samples);
    osd->v = malloc(chroma_samples);
    osd->a = malloc(output_pixels);
    osd->uv_a = malloc(chroma_samples);
    if (!osd->x_map || !osd->y_map || !osd->y || !osd->u || !osd->v || !osd->a ||
        !osd->uv_a) {
        fputs("stream-ui cache allocation failed\n", stderr);
        goto fail;
    }
    for (i = 0; i < output_width; ++i)
        osd->x_map[i] = (uint16_t)(((uint64_t)i * osd->surface.width) / output_width);
    for (i = 0; i < output_height; ++i)
        osd->y_map[i] = (uint16_t)(((uint64_t)i * osd->surface.height) / output_height);
    printf("stream-ui: OSD ARGB8888 %" PRIu16 "x%" PRIu16 " pitch=%" PRIu16
           " physical=%08" PRIx32 " offset=%08" PRIx32 "\n",
           osd->surface.width, osd->surface.height, osd->surface.pitch,
           osd->surface.physical_address, osd->surface.offset);
    return 0;

fail:
    free(surfaces);
    return -1;
}

static void destroy_osd_overlay(struct osd_overlay *osd)
{
    free(osd->uv_a);
    free(osd->a);
    free(osd->v);
    free(osd->u);
    free(osd->y);
    free(osd->y_map);
    free(osd->x_map);
    unmap_physical_plane(&osd->pixels);
    if (osd->initialized && osd->api.finalize && osd->api.finalize() != 0)
        fputs("warning: DILE_GAL_Finalize failed after stream-ui\n", stderr);
    if (osd->library)
        dlclose(osd->library);
    memset(osd, 0, sizeof *osd);
}

static int composite_osd_nv12(struct osd_overlay *osd, unsigned char *nv12,
                              uint32_t width, uint32_t height)
{
    uint32_t min_x;
    uint32_t min_y;
    uint32_t max_x;
    uint32_t max_y;
    uint32_t x;
    uint32_t y;
    unsigned char *uv = nv12 + (size_t)width * height;
    int changed = update_osd_cache(osd);

    if (!osd->has_pixels)
        return changed;
    min_x = osd->min_x;
    min_y = osd->min_y;
    max_x = osd->max_x + 1;
    max_y = osd->max_y + 1;

    for (y = min_y; y < max_y; ++y) {
        size_t target_row = (size_t)y * width;
        for (x = min_x; x < max_x; ++x) {
            size_t source = target_row + x;
            unsigned alpha = osd->a[source];
            if (alpha == 255)
                nv12[target_row + x] = osd->y[source];
            else if (alpha != 0)
                nv12[target_row + x] = blend_byte(nv12[target_row + x], osd->y[source], alpha);
        }
    }
    min_x &= ~1U;
    min_y &= ~1U;
    max_x = (max_x + 1U) & ~1U;
    max_y = (max_y + 1U) & ~1U;
    if (max_x > width)
        max_x = width;
    if (max_y > height)
        max_y = height;
    for (y = min_y; y < max_y; y += 2) {
        size_t target_row = (size_t)(y / 2) * width;
        size_t source_row = (size_t)(y / 2) * (width / 2);
        for (x = min_x; x < max_x; x += 2) {
            size_t source = source_row + x / 2;
            size_t target = target_row + x;
            unsigned alpha = osd->uv_a[source];
            if (alpha == 255) {
                uv[target] = osd->u[source];
                uv[target + 1] = osd->v[source];
            } else if (alpha != 0) {
                uv[target] = blend_byte(uv[target], osd->u[source], alpha);
                uv[target + 1] = blend_byte(uv[target + 1], osd->v[source], alpha);
            }
        }
    }
    return changed;
}

static int bench_vtm(uint32_t frames, uint32_t dump_location,
                     uint32_t requested_width, uint32_t requested_height)
{
    struct vt_api api;
    struct dile_vt_fb_capability capability;
    struct dile_vt_limitation limitation;
    struct dile_vt_fb_property property;
    uint32_t plane_addresses[MAX_VTM_PLANES];
    uint32_t *one_buffer_planes = plane_addresses;
    uint64_t *samples = NULL;
    uint32_t seen_indices = 0;
    uint32_t transitions = 0;
    uint32_t driver_rate = 0;
    uint32_t current_index = 0;
    uint32_t previous_index = UINT32_MAX;
    uint32_t lock_mode = 1;
    uint32_t completed = 0;
    uint32_t i;
    uint64_t started_ns = 0;
    uint64_t ended_ns = 0;
    void *library = NULL;
    void *handle = NULL;
    int initialized = 0;
    int started = 0;
    int rc = EXIT_FAILURE;

    memset(&capability, 0, sizeof capability);
    memset(&limitation, 0, sizeof limitation);
    memset(&property, 0, sizeof property);
    memset(plane_addresses, 0, sizeof plane_addresses);
    property.physical_addresses = &one_buffer_planes;

    if (load_vt(&library, &api) != 0)
        goto done;
    if (api.init() != 0) {
        fputs("DILE_VT_Init failed\n", stderr);
        goto done;
    }
    initialized = 1;
    handle = api.create(0);
    if (!handle) {
        fputs("DILE_VT_Create(window=0) failed\n", stderr);
        goto done;
    }
    if (api.start(&lock_mode) != 0) {
        fputs("DILE_VT_Start failed (capture resource busy?)\n", stderr);
        goto done;
    }
    started = 1;
    if (api.set_dump(handle, dump_location) != 0) {
        fputs("DILE_VT_SetVideoFrameOutputDeviceDumpLocation failed\n", stderr);
        goto done;
    }
    if (api.get_limitation(handle, &limitation) != 0) {
        fputs("DILE_VT_GetVideoFrameOutputDeviceLimitation failed\n", stderr);
        goto done;
    }
    printf("vtm: max=%" PRIu16 "x%" PRIu16 " align_lt=%u"
           " scale_up=%u up_limit=%" PRIu32 "x%" PRIu32
           " scale_down=%u down_limit=%" PRIu32 "x%" PRIu32 "\n",
           limitation.max_resolution.width, limitation.max_resolution.height,
           limitation.left_top_align, limitation.scale_up,
           limitation.scale_up_limit_width, limitation.scale_up_limit_height,
           limitation.scale_down, limitation.scale_down_limit_width,
           limitation.scale_down_limit_height);
    if (requested_width || requested_height) {
        struct dile_vt_rect region;
        if (!requested_width || !requested_height ||
            requested_width > UINT16_MAX || requested_height > UINT16_MAX) {
            fputs("invalid requested VTM region\n", stderr);
            goto done;
        }
        memset(&region, 0, sizeof region);
        region.width = (uint16_t)requested_width;
        region.height = (uint16_t)requested_height;
        if (api.set_region(handle, dump_location, &region) != 0) {
            fprintf(stderr, "DILE_VT_SetVideoFrameOutputDeviceOutputRegion(%" PRIu32
                            "x%" PRIu32 ") failed\n",
                    requested_width, requested_height);
            goto done;
        }
    }
    if (api.get_capability(handle, &capability) != 0) {
        fputs("DILE_VT_GetVideoFrameBufferCapability failed\n", stderr);
        goto done;
    }
    if (capability.num_buffers == 0 || capability.num_buffers > MAX_VTM_BUFFERS ||
        capability.num_planes == 0 || capability.num_planes > MAX_VTM_PLANES) {
        fprintf(stderr, "unsupported VTM capability: buffers=%" PRIu32 " planes=%" PRIu32 "\n",
                capability.num_buffers, capability.num_planes);
        goto done;
    }
    if (api.get_rate(handle, &driver_rate) != 0) {
        fputs("DILE_VT_GetVideoFrameOutputDeviceFramerate failed\n", stderr);
        goto done;
    }
    samples = calloc(frames, sizeof *samples);
    if (!samples) {
        fputs("calloc samples failed\n", stderr);
        goto done;
    }
    printf("vtm: dump=%s requested=%" PRIu32 " driver_rate=%" PRIu32
           " buffers=%" PRIu32 " planes=%" PRIu32 " lock_mode=%" PRIu32 "\n",
           dump_location == DILE_VT_DISPLAY_OUTPUT ? "display" : "scaler", frames,
           driver_rate, capability.num_buffers, capability.num_planes, lock_mode);

    /* The first interrupt can already be pending, so exclude it from cadence statistics. */
    if (api.wait_vsync(handle) != 0 || api.get_current(handle, &property, &current_index) != 0) {
        fputs("VTM warmup failed (is a non-secure video plane active?)\n", stderr);
        goto done;
    }
    printf("vtm: frame=%" PRIu32 "x%" PRIu32 " stride=%" PRIu32 " format=%s(%" PRIu32 ")\n",
           property.width, property.height, property.stride,
           pixel_format_name(property.pixel_format), property.pixel_format);
    previous_index = current_index;
    seen_indices |= UINT32_C(1) << current_index;

    started_ns = now_ns();
    for (i = 0; i < frames && !interrupted; ++i) {
        uint64_t before = now_ns();
        if (api.wait_vsync(handle) != 0 ||
            api.get_current(handle, &property, &current_index) != 0) {
            fprintf(stderr, "VTM sample failed at frame %" PRIu32 "\n", i);
            goto done;
        }
        samples[i] = now_ns() - before;
        if (current_index >= capability.num_buffers) {
            fprintf(stderr, "VTM returned invalid buffer index %" PRIu32 "/%" PRIu32 "\n",
                    current_index, capability.num_buffers);
            goto done;
        }
        seen_indices |= UINT32_C(1) << current_index;
        if (previous_index != UINT32_MAX && previous_index != current_index)
            ++transitions;
        previous_index = current_index;
        ++completed;
    }
    ended_ns = now_ns();
    if (completed == 0) {
        fputs("VTM produced no samples\n", stderr);
        goto done;
    }

    qsort(samples, completed, sizeof *samples, cmp_u64);
    {
        double seconds = (ended_ns - started_ns) / 1000000000.0;
        double irq_hz = completed / seconds;
        double buffer_fps = transitions / seconds;
        unsigned unique = 0;
        uint32_t mask = seen_indices;
        while (mask) {
            unique += mask & 1U;
            mask >>= 1;
        }
        printf("RESULT vtm waits=%" PRIu32 " elapsed=%.3fs irq_hz=%.2f buffer_fps=%.2f"
               " wait_p50=%.3fms wait_p95=%.3fms wait_max=%.3fms"
               " buffer_transitions=%" PRIu32 " unique_buffers=%u\n",
               completed, seconds, irq_hz, buffer_fps, percentile_ms(samples, completed, 50),
               percentile_ms(samples, completed, 95),
               samples[completed - 1] / 1000000.0, transitions, unique);
        printf("VERDICT vtm %s target=60 new_buffers_per_sec floor=55\n",
               buffer_fps >= 55.0 ? "PASS" : "FAIL");
    }
    rc = interrupted ? 130 : EXIT_SUCCESS;

done:
    free(samples);
    if (started && api.stop() != 0) {
        fputs("warning: DILE_VT_Stop failed\n", stderr);
        rc = EXIT_FAILURE;
    }
    if (handle && api.destroy(handle) != 0) {
        fputs("warning: DILE_VT_Destroy failed\n", stderr);
        rc = EXIT_FAILURE;
    }
    if (initialized && api.finalize() != 0) {
        fputs("warning: DILE_VT_Finalize failed\n", stderr);
        rc = EXIT_FAILURE;
    }
    if (library)
        dlclose(library);
    return rc;
}

static int bench_osd(uint32_t iterations)
{
    struct gal_api api;
    uint64_t *samples = NULL;
    uint32_t completed = 0;
    uint32_t last_address[2] = {0, 0};
    uint32_t address_changes = 0;
    uint32_t first_count = 0;
    uint32_t i;
    uint64_t started_ns = 0;
    uint64_t ended_ns = 0;
    void *library = NULL;
    int initialized = 0;
    int rc = EXIT_FAILURE;

    if (load_gal(&library, &api) != 0)
        goto done;
    if (api.init() != 0) {
        fputs("DILE_GAL_Init failed\n", stderr);
        goto done;
    }
    initialized = 1;
    samples = calloc(iterations, sizeof *samples);
    if (!samples) {
        fputs("calloc samples failed\n", stderr);
        goto done;
    }

    started_ns = now_ns();
    for (i = 0; i < iterations && !interrupted; ++i) {
        struct dile_gal_surface *surfaces = NULL;
        uint32_t count = 0;
        uint64_t before = now_ns();
        if (api.get_list(&surfaces, &count) != 0) {
            fprintf(stderr, "DILE_GAL_GetFrameBufferList failed at iteration %" PRIu32 "\n", i);
            free(surfaces);
            goto done;
        }
        samples[i] = now_ns() - before;
        if (!surfaces || count == 0 || count > 2) {
            fprintf(stderr, "unsupported OSD list: ptr=%p count=%" PRIu32 "\n",
                    (void *)surfaces, count);
            free(surfaces);
            goto done;
        }
        if (i == 0) {
            uint32_t j;
            first_count = count;
            printf("osd: surfaces=%" PRIu32 " requested=%" PRIu32 "\n", count, iterations);
            for (j = 0; j < count; ++j) {
                printf("osd[%" PRIu32 "]: %" PRIu16 "x%" PRIu16
                       " pitch=%" PRIu16 " bpp=%" PRIu16 " format=%" PRIu32 "\n",
                       j, surfaces[j].width, surfaces[j].height, surfaces[j].pitch,
                       surfaces[j].bpp, surfaces[j].pixel_format);
                last_address[j] = surfaces[j].physical_address;
            }
        } else {
            uint32_t j;
            if (count != first_count) {
                fprintf(stderr, "OSD surface count changed: %" PRIu32 " -> %" PRIu32 "\n",
                        first_count, count);
                free(surfaces);
                goto done;
            }
            for (j = 0; j < count; ++j) {
                if (last_address[j] != surfaces[j].physical_address) {
                    ++address_changes;
                    last_address[j] = surfaces[j].physical_address;
                }
            }
        }
        free(surfaces);
        ++completed;
    }
    ended_ns = now_ns();
    if (completed == 0) {
        fputs("OSD produced no samples\n", stderr);
        goto done;
    }
    qsort(samples, completed, sizeof *samples, cmp_u64);
    {
        double seconds = (ended_ns - started_ns) / 1000000000.0;
        double calls = completed / seconds;
        printf("RESULT osd iterations=%" PRIu32 " elapsed=%.3fs calls_per_sec=%.1f"
               " call_p50=%.3fms call_p95=%.3fms call_max=%.3fms address_changes=%" PRIu32 "\n",
               completed, seconds, calls, percentile_ms(samples, completed, 50),
               percentile_ms(samples, completed, 95),
               samples[completed - 1] / 1000000.0, address_changes);
        printf("VERDICT osd %s target=60 descriptor_reads_per_sec\n",
               calls >= 60.0 ? "PASS" : "FAIL");
    }
    rc = interrupted ? 130 : EXIT_SUCCESS;

done:
    free(samples);
    if (initialized && api.finalize() != 0) {
        fputs("warning: DILE_GAL_Finalize failed\n", stderr);
        rc = EXIT_FAILURE;
    }
    if (library)
        dlclose(library);
    return rc;
}

static void fill_nv12(unsigned char *frame, uint32_t width, uint32_t height, uint32_t phase)
{
    uint32_t x;
    uint32_t y;
    size_t luma_size = (size_t)width * height;
    for (y = 0; y < height; ++y) {
        for (x = 0; x < width; ++x) {
            uint32_t checker = ((x + phase * 7) / 16) ^ ((y + phase * 5) / 16);
            frame[(size_t)y * width + x] = checker & 1U ? 220 : 32;
        }
    }
    memset(frame + luma_size, 128, luma_size / 2);
}

static int bench_venc(uint32_t frames, uint32_t width, uint32_t height,
                      uint32_t bitrate_kbps)
{
    struct venc_api api;
    struct venc_config config;
    struct venc_input input;
    struct venc_output output = {0};
    unsigned char *bank[VENC_FRAME_BANK] = {NULL, NULL, NULL, NULL};
    unsigned char dummy = 0;
    uint64_t *samples = NULL;
    uint64_t bytes = 0;
    uint32_t keyframes = 0;
    uint32_t completed = 0;
    uint32_t i;
    size_t luma_size;
    size_t frame_size;
    uint64_t started_ns;
    uint64_t ended_ns;
    void *library = NULL;
    void *context = NULL;
    int call_rc;
    /* Keep the process status failed until all requested work has completed. */
    int rc = EXIT_FAILURE;

    if ((width & 1U) || (height & 1U) || width > 3840 || height > 2160) {
        fputs("venc requires even dimensions no larger than 3840x2160\n", stderr);
        return EXIT_FAILURE;
    }
    luma_size = (size_t)width * height;
    if (luma_size > SIZE_MAX - luma_size / 2) {
        fputs("venc frame size overflow\n", stderr);
        return EXIT_FAILURE;
    }
    frame_size = luma_size + luma_size / 2;

    if (load_venc(&library, &api) != 0)
        goto done;
    memset(&config, 0, sizeof config);
    config.pixel_format = VENC_PIXEL_FORMAT_NV12;
    config.codec = VENC_CODEC_H264;
    config.width = width;
    config.height = height;
    config.framerate = VENC_FRAMERATE_60P;
    config.target_bitrate_kbps = bitrate_kbps;
    config.gop = 60;
    config.qp = 24;
    printf("venc: config=NV12/H264 %" PRIu32 "x%" PRIu32
           "@60 bitrate=%" PRIu32 "kbps gop=%" PRIu32 " qp=%" PRIu32
           " frames=%" PRIu32 "\n",
           width, height, bitrate_kbps, config.gop, config.qp, frames);
    call_rc = api.create(&config, &context);
    if (call_rc != 0 || !context) {
        fprintf(stderr, "HAL_VENC_CODEC_Create failed: rc=%d context=%p\n", call_rc, context);
        goto done;
    }
    for (i = 0; i < VENC_FRAME_BANK; ++i) {
        bank[i] = malloc(frame_size);
        if (!bank[i]) {
            fputs("malloc VENC frame bank failed\n", stderr);
            goto done;
        }
        fill_nv12(bank[i], width, height, i);
    }
    samples = calloc(frames, sizeof *samples);
    if (!samples) {
        fputs("calloc VENC samples failed\n", stderr);
        goto done;
    }

    /* Prime one frame so setup and the first sequence header are outside the sustained result. */
    memset(&input, 0, sizeof input);
    memset(&output, 0, sizeof output);
    input.planes[0] = bank[0];
    input.planes[1] = bank[0] + luma_size;
    input.planes[2] = &dummy;
    input.sizes[0] = (uint32_t)luma_size;
    input.sizes[1] = (uint32_t)(luma_size / 2);
    if (api.encode(context, &input, &output) != 0 || !output.data || output.size == 0) {
        fputs("HAL_VENC_CODEC_Encode warmup failed\n", stderr);
        goto done;
    }
    call_rc = api.release(&output);
    memset(&output, 0, sizeof output);
    if (call_rc != 0) {
        fputs("HAL_VENC_CODEC_ReleaseFrameData warmup failed\n", stderr);
        goto done;
    }

    started_ns = now_ns();
    for (i = 0; i < frames && !interrupted; ++i) {
        uint64_t before;
        uint32_t slot = i % VENC_FRAME_BANK;
        memset(&input, 0, sizeof input);
        memset(&output, 0, sizeof output);
        input.planes[0] = bank[slot];
        input.planes[1] = bank[slot] + luma_size;
        input.planes[2] = &dummy;
        input.sizes[0] = (uint32_t)luma_size;
        input.sizes[1] = (uint32_t)(luma_size / 2);
        before = now_ns();
        if (api.encode(context, &input, &output) != 0 || !output.data || output.size == 0) {
            fprintf(stderr, "HAL_VENC_CODEC_Encode failed at frame %" PRIu32 "\n", i);
            goto done;
        }
        samples[i] = now_ns() - before;
        bytes += output.size;
        keyframes += output.keyframe != 0;
        ++completed;
        call_rc = api.release(&output);
        memset(&output, 0, sizeof output);
        if (call_rc != 0) {
            fprintf(stderr, "HAL_VENC_CODEC_ReleaseFrameData failed at frame %" PRIu32 "\n", i);
            goto done;
        }
    }
    ended_ns = now_ns();
    if (completed == 0) {
        fputs("VENC produced no samples\n", stderr);
        goto done;
    }
    qsort(samples, completed, sizeof *samples, cmp_u64);
    {
        double seconds = (ended_ns - started_ns) / 1000000000.0;
        double fps = completed / seconds;
        double mbps = bytes * 8.0 / seconds / 1000000.0;
        printf("RESULT venc frames=%" PRIu32 " elapsed=%.3fs fps=%.2f"
               " encode_p50=%.3fms encode_p95=%.3fms encode_max=%.3fms"
               " bytes=%" PRIu64 " bitrate=%.2fMbps keyframes=%" PRIu32 "\n",
               completed, seconds, fps, percentile_ms(samples, completed, 50),
               percentile_ms(samples, completed, 95),
               samples[completed - 1] / 1000000.0, bytes, mbps, keyframes);
        printf("VERDICT venc %s target=60 encoded_frames_per_sec floor=55\n",
               fps >= 55.0 ? "PASS" : "FAIL");
    }
    rc = interrupted ? 130 : EXIT_SUCCESS;

done:
    if (output.data)
        api.release(&output);
    for (i = 0; i < VENC_FRAME_BANK; ++i)
        free(bank[i]);
    free(samples);
    if (context && api.destroy(context) != 0) {
        fputs("warning: HAL_VENC_CODEC_Destroy failed\n", stderr);
        rc = EXIT_FAILURE;
    }
    if (library)
        dlclose(library);
    return rc;
}

static int bench_stream(uint32_t frames, uint16_t port, uint32_t width, uint32_t height,
                        uint32_t bitrate_kbps, int include_ui)
{
    struct vt_api vt = {0};
    struct venc_api venc = {0};
    struct dile_vt_fb_capability capability = {0};
    struct dile_vt_fb_property all_property = {0};
    struct dile_vt_fb_property current_property = {0};
    struct dile_vt_rect region = {0, 0, (uint16_t)width, (uint16_t)height};
    struct venc_config config = {0};
    struct venc_input input;
    struct venc_output output = {0};
    struct osd_overlay osd = {0};
    struct mapped_plane mappings[MAX_VTM_BUFFERS][2];
    uint32_t addresses[MAX_VTM_BUFFERS][MAX_VTM_PLANES];
    uint32_t *address_rows[MAX_VTM_BUFFERS];
    uint32_t current_addresses[MAX_VTM_PLANES];
    uint32_t *current_address_row = current_addresses;
    unsigned char dummy = 0;
    uint64_t *encode_samples = NULL;
    uint64_t *send_samples = NULL;
    uint64_t *compose_samples = NULL;
    uint64_t bytes = 0;
    uint64_t started_ns = 0;
    uint64_t ended_ns = 0;
    uint64_t next_frame_ns = 0;
    size_t luma_size = 0;
    size_t chroma_size = 0;
    uint32_t driver_rate = 0;
    uint32_t current_index = 0;
    uint32_t previous_index = UINT32_MAX;
    uint32_t buffer_transitions = 0;
    uint32_t lock_mode = 1;
    uint32_t completed = 0;
    uint32_t keyframes = 0;
    uint32_t osd_changes = 0;
    uint32_t i;
    uint32_t plane;
    void *vt_library = NULL;
    void *venc_library = NULL;
    void *vt_handle = NULL;
    void *venc_context = NULL;
    int mem_fd = -1;
    int listener = -1;
    int client = -1;
    int vt_initialized = 0;
    int vt_started = 0;
    int call_rc;
    /* Firmware call success must never turn a later early exit into success. */
    int rc = EXIT_FAILURE;
    unsigned char *composited_frame = NULL;

    memset(mappings, 0, sizeof mappings);
    memset(addresses, 0, sizeof addresses);
    memset(current_addresses, 0, sizeof current_addresses);
    if ((width & 1U) || (height & 1U) || width == 0 || height == 0 ||
        width > UINT16_MAX || height > UINT16_MAX) {
        fputs("stream requires nonzero even dimensions within the VTM uint16 ABI\n", stderr);
        return EXIT_FAILURE;
    }
    for (i = 0; i < MAX_VTM_BUFFERS; ++i)
        address_rows[i] = addresses[i];
    all_property.physical_addresses = address_rows;
    current_property.physical_addresses = &current_address_row;

    if (load_vt(&vt_library, &vt) != 0)
        goto done;
    if (vt.init() != 0) {
        fputs("DILE_VT_Init failed\n", stderr);
        goto done;
    }
    vt_initialized = 1;
    vt_handle = vt.create(0);
    if (!vt_handle) {
        fputs("DILE_VT_Create(window=0) failed\n", stderr);
        goto done;
    }
    if (vt.start(&lock_mode) != 0) {
        fputs("DILE_VT_Start failed (capture resource busy?)\n", stderr);
        goto done;
    }
    vt_started = 1;
    /* tvservice's own source-video path selects SCALER_OUTPUT. DISPLAY_OUTPUT can be a blank
     * post-composition slot even while its ring advances at the video's cadence. */
    if (vt.set_dump(vt_handle, DILE_VT_SCALER_OUTPUT) != 0 ||
        vt.set_region(vt_handle, DILE_VT_SCALER_OUTPUT, &region) != 0 ||
        vt.get_capability(vt_handle, &capability) != 0 ||
        vt.get_rate(vt_handle, &driver_rate) != 0) {
        fputs("VTM stream setup failed\n", stderr);
        goto done;
    }
    if (capability.num_buffers == 0 || capability.num_buffers > MAX_VTM_BUFFERS ||
        capability.num_planes != 2) {
        fprintf(stderr, "stream requires 1..%d buffers and exactly two planes; got %" PRIu32
                        "/%" PRIu32 "\n",
                MAX_VTM_BUFFERS, capability.num_buffers, capability.num_planes);
        goto done;
    }
    if (vt.wait_vsync(vt_handle) != 0 ||
        vt.get_all(vt_handle, &capability, &all_property) != 0) {
        fputs("DILE_VT_GetAllVideoFrameBufferProperty failed\n", stderr);
        goto done;
    }
    if (all_property.pixel_format != VENC_PIXEL_FORMAT_NV12 ||
        all_property.width != width || all_property.height != height ||
        all_property.stride != width) {
        fprintf(stderr, "stream needs tightly packed NV12 %" PRIu32 "x%" PRIu32
                        "; got %s %" PRIu32 "x%" PRIu32 " stride=%" PRIu32 "\n",
                width, height, pixel_format_name(all_property.pixel_format),
                all_property.width, all_property.height, all_property.stride);
        goto done;
    }
    luma_size = (size_t)all_property.stride * all_property.height;
    chroma_size = (size_t)all_property.stride * (all_property.height / 2);
    if (luma_size > UINT32_MAX || chroma_size > UINT32_MAX) {
        fputs("VTM planes exceed the VENC uint32 size ABI\n", stderr);
        goto done;
    }
    mem_fd = open("/dev/mem", O_RDONLY | O_CLOEXEC);
    if (mem_fd < 0) {
        fprintf(stderr, "open /dev/mem: %s\n", strerror(errno));
        goto done;
    }
    for (i = 0; i < capability.num_buffers; ++i) {
        if (map_physical_plane(mem_fd, addresses[i][0], luma_size, &mappings[i][0]) != 0 ||
            map_physical_plane(mem_fd, addresses[i][1], chroma_size, &mappings[i][1]) != 0)
            goto done;
        printf("stream: buffer[%" PRIu32 "] Y=%08" PRIx32 " UV=%08" PRIx32 "\n",
               i, addresses[i][0], addresses[i][1]);
    }
    if (include_ui) {
        if (setup_osd_overlay(mem_fd, width, height, &osd) != 0)
            goto done;
        composited_frame = malloc(luma_size + chroma_size);
        if (!composited_frame) {
            fputs("stream-ui frame allocation failed\n", stderr);
            goto done;
        }
    }
    printf("stream: VTM NV12 %" PRIu32 "x%" PRIu32 " stride=%" PRIu32
           " buffers=%" PRIu32 " driver_rate=%" PRIu32 "\n",
           all_property.width, all_property.height, all_property.stride,
           capability.num_buffers, driver_rate);

    if (load_venc(&venc_library, &venc) != 0)
        goto done;
    config.pixel_format = VENC_PIXEL_FORMAT_NV12;
    config.codec = VENC_CODEC_H264;
    config.width = width;
    config.height = height;
    config.framerate = VENC_FRAMERATE_60P;
    config.target_bitrate_kbps = bitrate_kbps;
    config.gop = 60;
    config.qp = 24;
    if (venc.create(&config, &venc_context) != 0 || !venc_context) {
        fputs("HAL_VENC_CODEC_Create failed for VTM stream\n", stderr);
        goto done;
    }
    encode_samples = calloc(frames, sizeof *encode_samples);
    send_samples = calloc(frames, sizeof *send_samples);
    if (include_ui)
        compose_samples = calloc(frames, sizeof *compose_samples);
    if (!encode_samples || !send_samples || (include_ui && !compose_samples)) {
        fputs("calloc stream samples failed\n", stderr);
        goto done;
    }
    client = accept_stream_client(port, &listener);
    if (client < 0)
        goto done;

    started_ns = now_ns();
    next_frame_ns = started_ns;
    while (completed < frames && !interrupted) {
        uint64_t before;
        if (completed != 0) {
            next_frame_ns += UINT64_C(1000000000) / 60;
            if (sleep_until_ns(next_frame_ns) != 0 && !interrupted) {
                fputs("stream cadence sleep failed\n", stderr);
                goto done;
            }
        }
        /* SCALER_OUTPUT has no continuing WaitVsync notifications on this firmware. Sample its
         * current DMA surface on a monotonic 60 Hz clock; HAL VENC copies it synchronously. */
        if (vt.get_current(vt_handle, &current_property, &current_index) != 0) {
            fputs("VTM stream frame query failed\n", stderr);
            goto done;
        }
        if (current_index >= capability.num_buffers) {
            fprintf(stderr, "VTM stream returned invalid buffer index %" PRIu32 "\n",
                    current_index);
            goto done;
        }
        if (previous_index != UINT32_MAX && current_index != previous_index)
            ++buffer_transitions;
        previous_index = current_index;
        if (current_property.pixel_format != all_property.pixel_format ||
            current_property.stride != all_property.stride ||
            current_property.width != all_property.width ||
            current_property.height != all_property.height ||
            current_addresses[0] != addresses[current_index][0] ||
            current_addresses[1] != addresses[current_index][1]) {
            fputs("VTM stream descriptor changed after mappings were established\n", stderr);
            goto done;
        }

        memset(&input, 0, sizeof input);
        memset(&output, 0, sizeof output);
        if (include_ui) {
            before = now_ns();
            memcpy(composited_frame, mappings[current_index][0].data, luma_size);
            memcpy(composited_frame + luma_size, mappings[current_index][1].data, chroma_size);
            osd_changes += composite_osd_nv12(&osd, composited_frame, width, height) != 0;
            compose_samples[completed] = now_ns() - before;
            if (completed == 0) {
                printf("stream-ui: first OSD active=%d bbox=%" PRIu32 ",%" PRIu32
                       "..%" PRIu32 ",%" PRIu32 "\n",
                       osd.has_pixels, osd.min_x, osd.min_y, osd.max_x, osd.max_y);
            }
            input.planes[0] = composited_frame;
            input.planes[1] = composited_frame + luma_size;
        } else {
            input.planes[0] = mappings[current_index][0].data;
            input.planes[1] = mappings[current_index][1].data;
        }
        input.planes[2] = &dummy;
        input.sizes[0] = (uint32_t)luma_size;
        input.sizes[1] = (uint32_t)chroma_size;
        if (completed == 0) {
            print_plane_sample("Y", input.planes[0], luma_size);
            print_plane_sample("UV", input.planes[1], chroma_size);
        }
        before = now_ns();
        if (venc.encode(venc_context, &input, &output) != 0 ||
            !output.data || output.size == 0) {
            fprintf(stderr, "HAL_VENC_CODEC_Encode failed at stream frame %" PRIu32 "\n",
                    completed);
            goto done;
        }
        encode_samples[completed] = now_ns() - before;
        if (completed == 0) {
            const unsigned char *head = output.data;
            size_t shown = output.size < 8 ? output.size : 8;
            printf("stream: first packet=%" PRIu32 " bytes head=", output.size);
            for (i = 0; i < shown; ++i)
                printf("%02x", head[i]);
            putchar('\n');
            if (!packet_has_annex_b_start_code(output.data, output.size)) {
                fputs("first VENC packet is not Annex-B H.264; refusing an ambiguous raw stream\n",
                      stderr);
                goto done;
            }
        }
        before = now_ns();
        if (send_all_fd(client, output.data, output.size) != 0) {
            fprintf(stderr, "stream send failed at frame %" PRIu32 ": %s\n",
                    completed, strerror(errno));
            goto done;
        }
        send_samples[completed] = now_ns() - before;
        bytes += output.size;
        keyframes += output.keyframe != 0;
        ++completed;
        call_rc = venc.release(&output);
        memset(&output, 0, sizeof output);
        if (call_rc != 0) {
            fputs("HAL_VENC_CODEC_ReleaseFrameData failed during stream\n", stderr);
            goto done;
        }
    }
    ended_ns = now_ns();
    if (completed == 0) {
        fputs("stream produced no frames\n", stderr);
        goto done;
    }
    qsort(encode_samples, completed, sizeof *encode_samples, cmp_u64);
    qsort(send_samples, completed, sizeof *send_samples, cmp_u64);
    {
        double seconds = (ended_ns - started_ns) / 1000000000.0;
        double fps = completed / seconds;
        double mbps = bytes * 8.0 / seconds / 1000000.0;
        printf("RESULT stream frames=%" PRIu32 " elapsed=%.3fs fps=%.2f bitrate=%.2fMbps"
               " encode_p50=%.3fms encode_p95=%.3fms encode_max=%.3fms"
               " send_p50=%.3fms send_p95=%.3fms send_max=%.3fms"
               " bytes=%" PRIu64 " keyframes=%" PRIu32 " buffer_transitions=%" PRIu32 "\n",
               completed, seconds, fps, mbps,
               percentile_ms(encode_samples, completed, 50),
               percentile_ms(encode_samples, completed, 95),
               encode_samples[completed - 1] / 1000000.0,
               percentile_ms(send_samples, completed, 50),
               percentile_ms(send_samples, completed, 95),
               send_samples[completed - 1] / 1000000.0, bytes, keyframes, buffer_transitions);
        if (include_ui) {
            qsort(compose_samples, completed, sizeof *compose_samples, cmp_u64);
            printf("RESULT stream-ui compose_p50=%.3fms compose_p95=%.3fms compose_max=%.3fms"
                   " osd_changes=%" PRIu32 "\n",
                   percentile_ms(compose_samples, completed, 50),
                   percentile_ms(compose_samples, completed, 95),
                   compose_samples[completed - 1] / 1000000.0, osd_changes);
        }
        printf("VERDICT stream %s target=60 delivered_frames_per_sec floor=55\n",
               fps >= 55.0 ? "PASS" : "FAIL");
    }
    rc = interrupted ? 130 : EXIT_SUCCESS;

done:
    if (output.data && venc.release)
        venc.release(&output);
    if (client >= 0)
        close(client);
    if (listener >= 0)
        close(listener);
    free(compose_samples);
    free(send_samples);
    free(encode_samples);
    free(composited_frame);
    if (venc_context && venc.destroy && venc.destroy(venc_context) != 0) {
        fputs("warning: HAL_VENC_CODEC_Destroy failed after stream\n", stderr);
        rc = EXIT_FAILURE;
    }
    for (i = 0; i < MAX_VTM_BUFFERS; ++i) {
        for (plane = 0; plane < 2; ++plane)
            unmap_physical_plane(&mappings[i][plane]);
    }
    destroy_osd_overlay(&osd);
    if (mem_fd >= 0)
        close(mem_fd);
    if (venc_library)
        dlclose(venc_library);
    if (vt_started && vt.stop() != 0) {
        fputs("warning: DILE_VT_Stop failed after stream\n", stderr);
        rc = EXIT_FAILURE;
    }
    if (vt_handle && vt.destroy(vt_handle) != 0) {
        fputs("warning: DILE_VT_Destroy failed after stream\n", stderr);
        rc = EXIT_FAILURE;
    }
    if (vt_initialized && vt.finalize() != 0) {
        fputs("warning: DILE_VT_Finalize failed after stream\n", stderr);
        rc = EXIT_FAILURE;
    }
    if (vt_library)
        dlclose(vt_library);
    return rc;
}

static int dump_osd_alpha(const char *path)
{
    struct osd_overlay osd = {0};
    unsigned char *row = NULL;
    FILE *file = NULL;
    uint32_t x;
    uint32_t y;
    int mem_fd = -1;
    int rc = EXIT_FAILURE;

    mem_fd = open("/dev/mem", O_RDONLY | O_CLOEXEC);
    if (mem_fd < 0) {
        fprintf(stderr, "open /dev/mem: %s\n", strerror(errno));
        goto done;
    }
    /* The output dimensions only size unused scale maps here; the PGM is always the surface's
     * native geometry, recorded in its own header. */
    if (setup_osd_overlay(mem_fd, 384, 216, &osd) != 0)
        goto done;
    row = malloc(osd.surface.width);
    if (!row) {
        fputs("osd-alpha row allocation failed\n", stderr);
        goto done;
    }
    file = fopen(path, "wb");
    if (!file) {
        fprintf(stderr, "open %s: %s\n", path, strerror(errno));
        goto done;
    }
    if (fprintf(file, "P5\n%" PRIu16 " %" PRIu16 "\n255\n",
                osd.surface.width, osd.surface.height) < 0)
        goto write_failed;
    for (y = 0; y < osd.surface.height; ++y) {
        const unsigned char *source = osd.pixels.data + (size_t)y * osd.surface.pitch;
        for (x = 0; x < osd.surface.width; ++x)
            row[x] = source[(size_t)x * 4 + 3];
        if (fwrite(row, 1, osd.surface.width, file) != osd.surface.width)
            goto write_failed;
    }
    if (fclose(file) != 0) {
        file = NULL;
        goto write_failed;
    }
    file = NULL;
    printf("RESULT osd-alpha %" PRIu16 "x%" PRIu16 " -> %s\n",
           osd.surface.width, osd.surface.height, path);
    rc = EXIT_SUCCESS;
    goto done;

write_failed:
    fprintf(stderr, "write %s: %s\n", path, strerror(errno));

done:
    if (file)
        fclose(file);
    free(row);
    destroy_osd_overlay(&osd);
    if (mem_fd >= 0)
        close(mem_fd);
    return rc;
}

static void usage(const char *program)
{
    fprintf(stderr,
            "usage:\n"
            "  %s vtm [frames:1..3600] [display|scaler] [width height]\n"
            "  %s osd [iterations:1..100000]\n"
            "  %s venc [frames:1..3600] [width height] [bitrate_kbps]\n"
            "  %s stream [frames:1..3600] [port] [width height] [bitrate_kbps]\n"
            "  %s stream-ui [frames:1..3600] [port] [width height] [bitrate_kbps]\n"
            "  %s osd-alpha <output.pgm>\n",
            program, program, program, program, program, program);
}

int main(int argc, char **argv)
{
    struct sigaction action;
    uint32_t count;

    memset(&action, 0, sizeof action);
    action.sa_handler = on_signal;
    sigemptyset(&action.sa_mask);
    sigaction(SIGINT, &action, NULL);
    sigaction(SIGTERM, &action, NULL);

    if (argc < 2) {
        usage(argv[0]);
        return EXIT_FAILURE;
    }
    if (strcmp(argv[1], "vtm") == 0) {
        uint32_t location = DILE_VT_DISPLAY_OUTPUT;
        uint32_t width = 0;
        uint32_t height = 0;
        count = parse_count(argc > 2 ? argv[2] : NULL, 300, 3600);
        if (!count || argc == 5 || argc > 6) {
            usage(argv[0]);
            return EXIT_FAILURE;
        }
        if (argc >= 4) {
            if (strcmp(argv[3], "display") == 0)
                location = DILE_VT_DISPLAY_OUTPUT;
            else if (strcmp(argv[3], "scaler") == 0)
                location = DILE_VT_SCALER_OUTPUT;
            else {
                usage(argv[0]);
                return EXIT_FAILURE;
            }
        }
        if (argc == 6) {
            width = parse_count(argv[4], 0, UINT16_MAX);
            height = parse_count(argv[5], 0, UINT16_MAX);
            if (!width || !height) {
                usage(argv[0]);
                return EXIT_FAILURE;
            }
        }
        return bench_vtm(count, location, width, height);
    }
    if (strcmp(argv[1], "osd") == 0) {
        count = parse_count(argc > 2 ? argv[2] : NULL, 1000, 100000);
        if (!count || argc > 3) {
            usage(argv[0]);
            return EXIT_FAILURE;
        }
        return bench_osd(count);
    }
    if (strcmp(argv[1], "venc") == 0) {
        uint32_t width = 1920;
        uint32_t height = 1080;
        uint32_t bitrate = 12000;
        count = parse_count(argc > 2 ? argv[2] : NULL, 120, 3600);
        if (!count || argc == 4 || argc > 6) {
            usage(argv[0]);
            return EXIT_FAILURE;
        }
        if (argc >= 5) {
            width = parse_count(argv[3], 0, 3840);
            height = parse_count(argv[4], 0, 2160);
            if (!width || !height) {
                usage(argv[0]);
                return EXIT_FAILURE;
            }
        }
        if (argc == 6) {
            bitrate = parse_count(argv[5], 0, 100000);
            if (!bitrate) {
                usage(argv[0]);
                return EXIT_FAILURE;
            }
        }
        return bench_venc(count, width, height, bitrate);
    }
    if (strcmp(argv[1], "stream") == 0) {
        uint32_t port = 8920;
        uint32_t width = 1280;
        uint32_t height = 720;
        uint32_t bitrate = 8000;
        count = parse_count(argc > 2 ? argv[2] : NULL, 300, 3600);
        if (!count || argc == 5 || argc > 7) {
            usage(argv[0]);
            return EXIT_FAILURE;
        }
        if (argc >= 4) {
            port = parse_count(argv[3], 0, UINT16_MAX);
            if (!port) {
                usage(argv[0]);
                return EXIT_FAILURE;
            }
        }
        if (argc >= 6) {
            width = parse_count(argv[4], 0, UINT16_MAX);
            height = parse_count(argv[5], 0, UINT16_MAX);
            if (!width || !height) {
                usage(argv[0]);
                return EXIT_FAILURE;
            }
        }
        if (argc == 7) {
            bitrate = parse_count(argv[6], 0, 100000);
            if (!bitrate) {
                usage(argv[0]);
                return EXIT_FAILURE;
            }
        }
        return bench_stream(count, (uint16_t)port, width, height, bitrate, 0);
    }
    if (strcmp(argv[1], "stream-ui") == 0) {
        uint32_t port = 8920;
        uint32_t width = 1280;
        uint32_t height = 720;
        uint32_t bitrate = 8000;
        count = parse_count(argc > 2 ? argv[2] : NULL, 300, 3600);
        if (!count || argc == 5 || argc > 7) {
            usage(argv[0]);
            return EXIT_FAILURE;
        }
        if (argc >= 4) {
            port = parse_count(argv[3], 0, UINT16_MAX);
            if (!port) {
                usage(argv[0]);
                return EXIT_FAILURE;
            }
        }
        if (argc >= 6) {
            width = parse_count(argv[4], 0, UINT16_MAX);
            height = parse_count(argv[5], 0, UINT16_MAX);
            if (!width || !height) {
                usage(argv[0]);
                return EXIT_FAILURE;
            }
        }
        if (argc == 7) {
            bitrate = parse_count(argv[6], 0, 100000);
            if (!bitrate) {
                usage(argv[0]);
                return EXIT_FAILURE;
            }
        }
        return bench_stream(count, (uint16_t)port, width, height, bitrate, 1);
    }
    if (strcmp(argv[1], "osd-alpha") == 0) {
        if (argc != 3) {
            usage(argv[0]);
            return EXIT_FAILURE;
        }
        return dump_osd_alpha(argv[2]);
    }
    usage(argv[0]);
    return EXIT_FAILURE;
}
