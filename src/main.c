/* plexpoc — native webOS Plex client. BOOT SHIM only: the app core is Rust
 * plex_run() (rust-modules). This file stays C for the genuinely low-level
 * bootstrap that must run before any Rust executes — the async-signal-safe crash
 * tracer, the event-log handle, stderr capture, and process bring-up. Everything
 * else (SDL, the event loop, input, playback orchestration, draw) is Rust. */
#include "app.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <ucontext.h>
#include <unistd.h>

FILE *elogf = NULL;   /* shared event/diagnostic log (extern in app.h); used by the
                       * crash handler here and by the starfish.c seam. Opened "w" each
                       * launch, so it is TRUNCATED on relaunch — do not rely on it to
                       * survive a crash+relaunch (that is what clogf is for). */
static FILE *clogf = NULL;  /* persistent crash log: opened "a", never truncated, so a
                             * crash tracer survives the next relaunch (poc-crash.log). */

extern int plex_run(const char *pms_host, int pms_port);  /* Rust app core (no creds — session or /tmp/poc-token) */

/* crash tracer: log the faulting PC + the /proc/self/maps line containing it, so
 * we can tell which library (libplayerAPIs, gstreamer, ours) faulted. Runs in a
 * signal handler (must stay minimal/async-signal-safe), which is why it stays C. */
static const char *signame(int sig) {
    switch (sig) {
        case SIGSEGV: return "SIGSEGV";
        case SIGABRT: return "SIGABRT";  /* incl. a Rust panic that crosses an FFI boundary */
        case SIGBUS:  return "SIGBUS";
        case SIGILL:  return "SIGILL";
        case SIGTRAP: return "SIGTRAP";
        default:      return "SIG?";
    }
}

/* Write the fault PC/LR + the /proc/self/maps line(s) containing them to one log. */
static void write_trace(FILE *f, int sig, void *addr, unsigned long pc, unsigned long lr) {
    if (!f) return;
    fprintf(f, "\n*** SIGNAL %d (%s) addr=%p pc=0x%lx lr=0x%lx\n", sig, signame(sig), addr, pc, lr);
    FILE *m = fopen("/proc/self/maps", "r");
    if (m) {
        char line[256];
        while (fgets(line, sizeof line, m)) {
            unsigned long lo = 0, hi = 0;
            if (sscanf(line, "%lx-%lx", &lo, &hi) != 2) continue;
            if ((pc >= lo && pc < hi) || (lr >= lo && lr < hi))
                fprintf(f, "at: %s", line);
            if (strstr(line, "plexpoc"))      /* our load base, for addr2line */
                fprintf(f, "bin: %s", line);
        }
        fclose(m);
    }
    fflush(f);
}

static void crash_handler(int sig, siginfo_t *si, void *uc) {
    unsigned long pc = 0, lr = 0;
    ucontext_t *c = (ucontext_t *)uc;
#if defined(__arm__)
    pc = (unsigned long)c->uc_mcontext.arm_pc;
    lr = (unsigned long)c->uc_mcontext.arm_lr;
#endif
    void *addr = si ? si->si_addr : 0;
    write_trace(elogf, sig, addr, pc, lr);   /* immediate, this-session log (may be lost on relaunch) */
    write_trace(clogf, sig, addr, pc, lr);   /* persistent, survives the next relaunch */
    /* Re-raise with the DEFAULT disposition so the signal actually kills us: the
     * kernel dumps core and webOS crashd/librdx captures a full symbolicated
     * backtrace (/var/log/reports/librdx/), and the parent (SAM) sees a real
     * signal crash (WIFSIGNALED) instead of a clean exit. The old _exit(3) hid
     * every crash from the system tracer. */
    signal(sig, SIG_DFL);
    raise(sig);
    _exit(128 + sig);   /* only reached if raise() somehow returns */
}

static void install_crash_tracer(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = crash_handler;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGABRT, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);
    sigaction(SIGILL, &sa, NULL);    /* UBSan/overflow traps fire SIGILL/SIGTRAP */
    sigaction(SIGTRAP, &sa, NULL);
}

int main(int argc, char **argv) {
    (void)argc; (void)argv;
    elogf = fopen("/tmp/poc-events.log", "w");
    clogf = fopen("/tmp/poc-crash.log", "a");     /* append: keep prior crashes across relaunches */
    freopen("/tmp/poc-stderr.log", "w", stderr); /* capture abort/assert text */
    install_crash_tracer();
    /* request BACK key delivery from the webOS access policy (before SDL init) */
    setenv("SDL_WEBOS_ACCESS_POLICY_KEYS_BACK", "true", 1);
    return plex_run(PMS_HOST, PMS_PORT);
}
