#!/usr/bin/env python3
"""Run the actual benchmark bodies with host fakes for firmware and device I/O.

This grades exit/cleanup control flow, not ARM ABI or capture quality. The ARM
build retains all layout assertions; pointer-bearing layouts differ on the host.
"""
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

SOURCE = Path(__file__).with_name("tv-capture-bench.c")


def function(source, name):
    match = re.search(r"^static [^;{]*\b" + name + r"\([^;{]*\{.*?^}\n", source, re.M | re.S)
    if not match:
        raise AssertionError("missing benchmark function: " + name)
    return match.group()


MOCKS = r'''
static const char *fault;
static int encodes, releases, sends, allocations, destroys;
static int fail(const char *name) { return strcmp(fault, name) == 0; }
static void *test_malloc(size_t n) {
    if (++allocations == 2 && fail("allocation")) return NULL;
    return malloc(n);
}
static void *test_calloc(size_t n, size_t s) {
    if (fail("samples")) return NULL;
    return calloc(n, s);
}
#define malloc test_malloc
#define calloc test_calloc
static int create(const struct venc_config *c, void **out) {
    (void)c;
    if (fail("create")) return -9;
    *out = (void *)1;
    return 0;
}
static int encode(void *c, const struct venc_input *in, struct venc_output *out) {
    static unsigned char packet[] = {0, 0, 0, 1, 0x65};
    (void)c; (void)in;
    ++encodes;
    if ((fail("warmup") && encodes == 1) || (fail("encode") && encodes == 3)) return -9;
    if (fail("empty") && encodes == 3) return 0;
    out->data = packet; out->size = sizeof packet; out->keyframe = 1;
    if (fail("interrupt") && encodes == 2) interrupted = 1;
    return 0;
}
static int release(struct venc_output *out) {
    if (!out->data) abort();
    ++releases;
    return (fail("release") && releases == 2) ? -9 : 0;
}
static int destroy(void *c) { (void)c; ++destroys; return fail("destroy") ? -9 : 0; }
static int load_venc(void **lib, struct venc_api *api) {
    *lib = NULL;
    *api = (struct venc_api){create, encode, destroy, release};
    return 0;
}
static int ok(void) { return 0; }
static void *vt_create(uint32_t w) { (void)w; return (void *)1; }
static int vt_destroy(void *h) { (void)h; return 0; }
static int vt_start(uint32_t *m) { (void)m; return 0; }
static int vt_dump(void *h, uint32_t l) { (void)h; (void)l; return 0; }
static int vt_region(void *h, uint32_t l, const struct dile_vt_rect *r) {
    (void)h; (void)l; (void)r; return 0;
}
static int vt_cap(void *h, struct dile_vt_fb_capability *c) {
    (void)h; *c = (struct dile_vt_fb_capability){1, 2}; return 0;
}
static int vt_rate(void *h, uint32_t *r) { (void)h; *r = 60; return 0; }
static int vt_wait(void *h) { (void)h; return 0; }
static void property(struct dile_vt_fb_property *p) {
    p->pixel_format = VENC_PIXEL_FORMAT_NV12; p->stride = p->width = p->height = 2;
    p->physical_addresses[0][0] = 4096; p->physical_addresses[0][1] = 8192;
}
static int vt_all(void *h, const struct dile_vt_fb_capability *c, struct dile_vt_fb_property *p) {
    (void)h; (void)c; property(p); return 0;
}
static int vt_current(void *h, struct dile_vt_fb_property *p, uint32_t *i) {
    (void)h;
    if (fail("query") && encodes == 2) return -9;
    property(p); *i = 0; return 0;
}
static int load_vt(void **lib, struct vt_api *api) {
    *lib = NULL;
    *api = (struct vt_api){.init=ok, .finalize=ok, .create=vt_create, .destroy=vt_destroy,
        .start=vt_start, .stop=ok, .set_dump=vt_dump, .get_capability=vt_cap,
        .set_region=vt_region, .get_rate=vt_rate, .wait_vsync=vt_wait,
        .get_all=vt_all, .get_current=vt_current};
    return 0;
}
static int test_open(const char *p, int f) { (void)p; (void)f; return 100; }
static int test_close(int fd) { (void)fd; return 0; }
#define open test_open
#define close test_close
static int map_physical_plane(int fd, uint32_t p, size_t n, struct mapped_plane *out) {
    static unsigned char pixels[4] = {128, 128, 128, 128};
    (void)fd; (void)p; (void)n; out->data = pixels; return 0;
}
static void unmap_physical_plane(struct mapped_plane *p) { (void)p; }
static int setup_osd_overlay(int fd, uint32_t w, uint32_t h, struct osd_overlay *o) {
    (void)fd; (void)w; (void)h; (void)o; return 0;
}
static void destroy_osd_overlay(struct osd_overlay *o) { (void)o; }
static int composite_osd_nv12(struct osd_overlay *o, unsigned char *p, uint32_t w, uint32_t h) {
    (void)o; (void)p; (void)w; (void)h; return 0;
}
static int accept_stream_client(uint16_t p, int *listener) { (void)p; *listener = 101; return 102; }
static int send_all_fd(int fd, const void *p, size_t n) {
    (void)fd; (void)p; (void)n;
    return (fail("send") && ++sends == 2) ? -1 : 0;
}
static uint64_t now_ns(void) { static uint64_t n; return n += 1000000; }
static int sleep_until_ns(uint64_t n) { (void)n; return 0; }
'''


class BenchmarkExit(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory(prefix="plx-capture-test-")
        cls.addClassCleanup(cls.tmp.cleanup)
        src = SOURCE.read_text()
        types = src[src.index("enum {"):src.index("static void on_signal")]
        # Only host-pointer-dependent assertions are inapplicable here.
        for name in ("dile_vt_fb_property", "venc_input", "venc_output"):
            types = re.sub(r"_Static_assert\(sizeof\(struct " + name + r"\).*?;", "", types, flags=re.S)
        types += src[src.index("struct mapped_plane {"):src.index("static int map_physical_plane")]
        types += src[src.index("struct osd_overlay {"):src.index("static unsigned char clamp_byte")]
        headers = "\n".join("#include <" + h + ">" for h in (
            "stdint.h", "stdio.h", "stdlib.h", "string.h", "signal.h", "inttypes.h", "errno.h", "fcntl.h", "dlfcn.h"))
        # This harness never #includes the source file -- it assembles a fresh
        # translation unit from extracted fragments, so the source's own
        # `#ifndef _GNU_SOURCE` preamble (tv-capture-bench.c) never reaches this
        # compile. A feature-test macro must precede the FIRST libc header
        # (glibc's bits/fcntl-linux.h gates O_CLOEXEC on it), so it has to be
        # defined here, ahead of `headers`, not in the source file.
        code = "#define _GNU_SOURCE\n" + headers + "\n" + types + "\n" + MOCKS + "\n"
        for name in ("cmp_u64", "percentile_ms", "pixel_format_name", "fill_nv12", "print_plane_sample",
                     "packet_has_annex_b_start_code", "bench_venc", "bench_stream"):
            code += function(src, name)
        code += r'''
int main(int argc, char **argv) {
    if (argc != 3) return 99;
    fault = argv[2];
    int rc = strcmp(argv[1], "venc") == 0 ? bench_venc(4, 2, 2, 1000)
        : bench_stream(4, 8920, 2, 2, 1000, strcmp(argv[1], "stream-ui") == 0);
    fprintf(stderr, "cleanup destroys=%d releases=%d\n", destroys, releases);
    return rc;
}
'''
        path = Path(cls.tmp.name) / "test.c"
        path.write_text(code)
        cls.binary = path.with_suffix("")
        subprocess.run([os.environ.get("CC", "cc"), "-std=c11", "-Wall", "-Wextra", "-Werror",
                        str(path), "-o", str(cls.binary)], check=True)

    def run_case(self, mode, fault, status):
        result = subprocess.run([str(self.binary), mode, fault], capture_output=True, text=True)
        self.assertEqual(result.returncode, status, result.stdout + result.stderr)
        self.assertIn("cleanup destroys=" + ("0" if fault == "create" else "1"), result.stderr)

    def test_venc_failures(self):
        for fault in ("create", "allocation", "samples", "warmup", "encode", "empty", "release", "destroy"):
            with self.subTest(fault=fault):
                self.run_case("venc", fault, 1)

    def test_stream_failures_after_successful_frames(self):
        for mode in ("stream", "stream-ui"):
            for fault in ("create", "samples", "warmup", "encode", "empty", "send", "query", "release", "destroy"):
                with self.subTest(mode=mode, fault=fault):
                    self.run_case(mode, fault, 1)

    def test_success_and_interruption(self):
        for mode in ("venc", "stream", "stream-ui"):
            for fault, status in (("none", 0), ("interrupt", 130)):
                with self.subTest(mode=mode, fault=fault):
                    self.run_case(mode, fault, status)


if __name__ == "__main__":
    unittest.main()
