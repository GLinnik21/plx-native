/* Host regression test for the boot shim's fixed-name log sinks. */
#define main plx_boot_main
#include "../src/main.c"
#undef main

#include <errno.h>
#include <limits.h>

int plex_run(const char *host, int port) { (void)host; (void)port; return 0; }
int plx_sentry_spool_external(const char *path) { (void)path; return 0; }
void plx_crash_write_image_marker(int fd) { (void)fd; }
void plx_crash_install(int event_fd, int crash_fd) { (void)event_fd; (void)crash_fd; }
int plx_runtime_path(const char *name, char *out, size_t cap) {
    return snprintf(out, cap, "/tmp/%s", name) > 0;
}

static int failures;

static void expect(int yes, const char *what) {
    if (!yes) {
        fprintf(stderr, "FAIL: %s\n", what);
        failures++;
    }
}

int main(void) {
    char dir[] = "/tmp/plx-private-log-test.XXXXXX";
    expect(mkdtemp(dir) != NULL, "scratch directory");
    char victim[PATH_MAX], sink[PATH_MAX];
    snprintf(victim, sizeof victim, "%s/victim", dir);
    snprintf(sink, sizeof sink, "%s/sink", dir);

    int v = open(victim, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    expect(v >= 0, "victim created");
    if (v >= 0) { expect(write(v, "unchanged", 9) == 9, "victim seeded"); close(v); }
    expect(symlink(victim, sink) == 0, "attacker symlink created");
    int fd = open_fd_0600(sink, O_TRUNC);
    expect(fd < 0, "symlink sink refused");
    if (fd >= 0) close(fd);
    char got[16] = {0};
    v = open(victim, O_RDONLY);
    expect(v >= 0 && read(v, got, sizeof got) == 9, "victim still readable");
    if (v >= 0) close(v);
    expect(strcmp(got, "unchanged") == 0, "symlink target was not truncated");

    unlink(sink);
    fd = open(sink, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    expect(fd >= 0, "permissive owned file created");
    if (fd >= 0) close(fd);
    fd = open_fd_0600(sink, O_APPEND);
    expect(fd >= 0, "owned regular sink accepted");
    if (fd >= 0) close(fd);
    struct stat st;
    expect(stat(sink, &st) == 0 && (st.st_mode & 0777) == 0600,
           "accepted sink forced to 0600");

    unlink(sink);
    unlink(victim);
    rmdir(dir);
    if (failures) return 1;
    puts("private-log: ok");
    return 0;
}
