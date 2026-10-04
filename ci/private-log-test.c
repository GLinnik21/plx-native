/* Host regression test for the boot shim's fixed-name log sinks: the open discipline (no symlink,
 * regular file, ours, exactly one link) and the MODE — 0640, owner rw and the app's own group r, never anything for
 * "other" — on the real three sinks `main()` opens, under a restrictive umask. */
#define main plx_boot_main
#include "../src/main.c"
#undef main

#include <errno.h>
#include <limits.h>

int plex_run(const char *host, int port) { (void)host; (void)port; return 0; }
int plx_sentry_spool_external(const char *path) { (void)path; return 0; }
void plx_crash_write_image_marker(int fd) { (void)fd; }
void plx_crash_install(int event_fd, int crash_fd) { (void)event_fd; (void)crash_fd; }

/* The runtime root `main()` resolves its sinks under. The shipped resolver is Rust; here it is a
 * scratch directory, so booting the shim never touches a real /tmp/plxnative-*.log. */
static const char *runtime_root = "/tmp";
int plx_runtime_path(const char *name, char *out, size_t cap) {
    return snprintf(out, cap, "%s/%s", runtime_root, name) > 0;
}

static int failures;

static void expect(int yes, const char *what) {
    if (!yes) {
        fprintf(stderr, "FAIL: %s\n", what);
        failures++;
    }
}

/* The three sinks main() opens, by their runtime names. */
static const char *const sinks[] = {
    "plxnative-events.log", "plxnative-crash.log", "plxnative-stderr.log",
};

/* Run the REAL boot shim — `plx_boot_main`, i.e. `main()` — once, keeping the test's own stderr:
 * `main()` dup2()s the stderr log over fd 2, which would otherwise swallow every later FAIL line. */
static void boot_shim_once(void) {
    int saved = dup(STDERR_FILENO);
    char arg0[] = "plxnative";
    char *argv[] = { arg0, NULL };
    expect(saved >= 0, "stderr saved");
    expect(plx_boot_main(1, argv) == 0, "boot shim returned");
    if (saved >= 0) {
        dup2(saved, STDERR_FILENO);
        close(saved);
    }
    if (elogf) { fclose(elogf); elogf = NULL; }
}

static void expect_sink_modes(const char *dir, const char *when) {
    for (size_t i = 0; i < sizeof sinks / sizeof sinks[0]; i++) {
        char path[PATH_MAX], what[PATH_MAX + 96];
        struct stat st;
        snprintf(path, sizeof path, "%s/%s", dir, sinks[i]);
        snprintf(what, sizeof what, "%s: %s is a regular file", when, sinks[i]);
        expect(stat(path, &st) == 0 && S_ISREG(st.st_mode), what);
        snprintf(what, sizeof what, "%s: %s is 0640 (got %o)", when, sinks[i], (unsigned)(st.st_mode & 07777));
        expect((st.st_mode & 07777) == 0640, what);
        /* No gid assertion, on purpose: the group is the kernel's choice (the process's egid on
         * Linux, where /tmp is not setgid — gid 5000 on the television — but the DIRECTORY's group
         * on a BSD/macOS host), and nothing here chowns. The mode is the contract. */
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
    int fd = open_fd_log(sink, O_TRUNC);
    expect(fd < 0, "symlink sink refused");
    if (fd >= 0) close(fd);
    char got[16] = {0};
    v = open(victim, O_RDONLY);
    expect(v >= 0 && read(v, got, sizeof got) == 9, "victim still readable");
    if (v >= 0) close(v);
    expect(strcmp(got, "unchanged") == 0, "symlink target was not truncated");

    /* The hard-link confused deputy: a co-resident app in the shared gid links one of OUR 0600
     * files (the auth.json session fallback sits in the same runtime directory) onto a sink name.
     * The open lands on an inode that is a regular file and ours, so only st_nlink tells it from a
     * real sink -- and the fchmod/ftruncate that follow must never touch it. */
    struct stat st;
    unlink(sink);
    expect(chmod(victim, 0600) == 0, "victim reset to 0600");
    expect(link(victim, sink) == 0, "attacker hard link created");
    fd = open_fd_log(sink, O_TRUNC | O_APPEND);
    expect(fd < 0, "hard-linked sink refused");
    if (fd >= 0) close(fd);
    memset(got, 0, sizeof got);
    v = open(victim, O_RDONLY);
    expect(v >= 0 && read(v, got, sizeof got) == 9, "linked file still readable, not truncated");
    if (v >= 0) close(v);
    expect(strcmp(got, "unchanged") == 0, "hard-link target was not truncated");
    expect(stat(victim, &st) == 0 && (st.st_mode & 07777) == 0600,
           "hard-link target was not chmod'ed (still 0600)");
    expect(st.st_nlink == 2, "the refusal removed no name");
    fd = open_fd_log(sink, O_APPEND);
    expect(fd < 0, "hard-linked sink refused when appending too");
    if (fd >= 0) close(fd);
    expect(stat(victim, &st) == 0 && (st.st_mode & 07777) == 0600,
           "hard-link target still 0600 after the append open");
    /* Once the second name is gone it is an ordinary single-link survivor again, and the upgrade
     * correction (0600 from the previous release -> 0640) still runs. */
    unlink(sink);
    fd = open_fd_log(victim, O_APPEND);
    expect(fd >= 0, "a single-link survivor is accepted");
    if (fd >= 0) close(fd);
    expect(stat(victim, &st) == 0 && (st.st_mode & 07777) == 0640,
           "a single-link survivor is corrected to 0640");

    unlink(sink);
    fd = open(sink, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    expect(fd >= 0, "permissive owned file created");
    if (fd >= 0) close(fd);
    fd = open_fd_log(sink, O_APPEND);
    expect(fd >= 0, "owned regular sink accepted");
    if (fd >= 0) close(fd);
    expect(stat(sink, &st) == 0 && (st.st_mode & 07777) == 0640,
           "accepted sink forced to 0640");
    unlink(sink);
    unlink(victim);

    /* The sinks themselves, through `main()`. umask 077 is the realistic hostile case: it masks
     * the `open(2)` creation mode down to 0600 (or, for 0777, to nothing), so ONLY the explicit
     * fchmod can give a fresh file 0640. */
    runtime_root = dir;
    mode_t previous_umask = umask(077);
    boot_shim_once();
    expect_sink_modes(dir, "fresh create under umask 077");

    /* An install upgraded from the 0600 release finds its three files at 0600 (the events and
     * stderr logs are truncated at boot, the crash log is appended to): the existing fchmod has to
     * correct them, or the upgrade would leave the logs unreadable until /tmp was cleared. */
    for (size_t i = 0; i < sizeof sinks / sizeof sinks[0]; i++) {
        char path[PATH_MAX];
        snprintf(path, sizeof path, "%s/%s", dir, sinks[i]);
        expect(chmod(path, 0600) == 0, "log reset to the previous release's 0600");
    }
    boot_shim_once();
    expect_sink_modes(dir, "upgrade from 0600");

    umask(0777);
    for (size_t i = 0; i < sizeof sinks / sizeof sinks[0]; i++) {
        char path[PATH_MAX];
        snprintf(path, sizeof path, "%s/%s", dir, sinks[i]);
        expect(unlink(path) == 0, "log removed for a fresh create");
    }
    boot_shim_once();
    expect_sink_modes(dir, "fresh create under umask 777");
    umask(previous_umask);

    for (size_t i = 0; i < sizeof sinks / sizeof sinks[0]; i++) {
        char path[PATH_MAX];
        snprintf(path, sizeof path, "%s/%s", dir, sinks[i]);
        unlink(path);
    }
    rmdir(dir);
    if (failures) return 1;
    puts("private-log: ok");
    return 0;
}
