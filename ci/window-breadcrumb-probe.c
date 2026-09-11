/* Offline device proof for the sparse window breadcrumbs. Link with the same libsentry.a,
 * libunwind.a and src/sentry_context.c as the app; pass a disposable database and sentry-crash
 * path. The SDK is built with transport=none, and the DSN is deliberately non-routable.
 * This process faults deliberately; it never opens the app's session or reporting database. */
#include <sentry.h>
#include <signal.h>
#include <stdio.h>
#include <sys/resource.h>

void plx_sentry_window_breadcrumb(const char *, int, int, int, int, int, int);

int main(int argc, char **argv)
{
    if (argc != 3) {
        fprintf(stderr, "usage: window-breadcrumb-probe DATABASE HANDLER\n");
        return 2;
    }
    const struct rlimit no_core = {0, 0};
    if (setrlimit(RLIMIT_CORE, &no_core) != 0) {
        return 5;
    }
    sentry_options_t *options = sentry_options_new();
    sentry_options_set_dsn(options, "https://public@example.invalid/1");
    sentry_options_set_database_path(options, argv[1]);
    sentry_options_set_handler_path(options, argv[2]);
    sentry_options_set_external_crash_reporter_path(options, "/bin/true");
    sentry_options_set_release(options, "plxnative-window-probe");
    sentry_options_set_environment(options, "local-verification");
    sentry_options_set_auto_session_tracking(options, 0);
    sentry_options_set_max_breadcrumbs(options, 16);
    sentry_options_set_crash_reporting_mode(options, SENTRY_CRASH_REPORTING_MODE_NATIVE);
    if (sentry_init(options) != 0) {
        return 3;
    }
    for (int i = 0; i < 20; i++) {
        plx_sentry_window_breadcrumb("will_background", 1, -1, -1, -1, -1, -1);
        plx_sentry_window_breadcrumb("did_background", 0, -1, -1, -1, -1, -1);
    }
    plx_sentry_window_breadcrumb("did_foreground", 0, -1, -1, -1, -1, -1);
    plx_sentry_window_breadcrumb("wm_ready", -1, 2, 0, 5, 1, 1);
    plx_sentry_window_breadcrumb("first_frame", 1, -1, -1, -1, -1, -1);
    fputs("DELIBERATE SIGSEGV: offline window breadcrumb probe\n", stderr);
    fflush(stderr);
    raise(SIGSEGV);
    return 4;
}
