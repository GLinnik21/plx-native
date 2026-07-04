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
                       * crash handler here and by the starfish.c seam */

extern int plex_run(const char *pms_host, int pms_port, const char *pms_token);  /* Rust app core */

/* crash tracer: log the faulting PC + the /proc/self/maps line containing it, so
 * we can tell which library (libplayerAPIs, gstreamer, ours) faulted. Runs in a
 * signal handler (must stay minimal/async-signal-safe), which is why it stays C. */
static void crash_handler(int sig, siginfo_t *si, void *uc) {
    unsigned long pc = 0, lr = 0;
    ucontext_t *c = (ucontext_t *)uc;
#if defined(__arm__)
    pc = (unsigned long)c->uc_mcontext.arm_pc;
    lr = (unsigned long)c->uc_mcontext.arm_lr;
#endif
    if (elogf) {
        fprintf(elogf, "\n*** SIGNAL %d addr=%p pc=0x%lx lr=0x%lx\n", sig,
                si ? si->si_addr : 0, pc, lr);
        FILE *m = fopen("/proc/self/maps", "r");
        if (m) {
            char line[256];
            while (fgets(line, sizeof line, m)) {
                unsigned long lo = 0, hi = 0;
                if (sscanf(line, "%lx-%lx", &lo, &hi) != 2) continue;
                if ((pc >= lo && pc < hi) || (lr >= lo && lr < hi))
                    fprintf(elogf, "at: %s", line);
                if (strstr(line, "plexpoc"))      /* our load base, for addr2line */
                    fprintf(elogf, "bin: %s", line);
            }
            fclose(m);
        }
        fflush(elogf);
    }
    _exit(3);
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
    freopen("/tmp/poc-stderr.log", "w", stderr); /* capture abort/assert text */
    install_crash_tracer();
    /* request BACK key delivery from the webOS access policy (before SDL init) */
    setenv("SDL_WEBOS_ACCESS_POLICY_KEYS_BACK", "true", 1);
    return plex_run(PMS_HOST, PMS_PORT, PMS_TOKEN);
}
