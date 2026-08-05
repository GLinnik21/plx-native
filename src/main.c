/* PlxNative — an unofficial native Plex client for LG webOS.
 * Copyright © 2026 Gleb Linnik. Licensed under the MIT Licence; see LICENSE at the repository
 * root, and THIRD-PARTY-NOTICES.md for the components this links or redistributes.
 * Not affiliated with, endorsed by, or sponsored by Plex GmbH or LG Electronics.
 *
 * plxnative — native webOS Plex client. BOOT SHIM only: the app core is Rust
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
#include <sys/resource.h>
#include <fcntl.h>

FILE *elogf = NULL;   /* shared event/diagnostic log (extern in app.h); used by the
                       * crash handler here and by the starfish.c seam. Opened "w" each
                       * launch, so it is TRUNCATED on relaunch — do not rely on it to
                       * survive a crash+relaunch (that is what clogf is for). */
static FILE *clogf = NULL;  /* persistent crash log: opened "a", never truncated, so a
                             * crash tracer survives the next relaunch (plxnative-crash.log). */

extern int plex_run(const char *pms_host, int pms_port);  /* Rust app core (no creds — session or /tmp/plxnative-token) */

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
            /* our load base, for addr2line. Match the executable ONLY: the app dir is
             * itself named ...com.beb.plxnative/, so a bare substring test also matches
             * libraries deployed beside the binary (libturbojpeg.so.0). */
            if (strstr(line, "/plxnative\n") || strstr(line, "/plxnative "))
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

    /* Ignore SIGPIPE for the whole process. A Rust program gets this from std::rt::init, but
       our main() is C and calls plex_run() directly, so that never runs — leaving the DEFAULT
       disposition (terminate). stream.rs sends the PMS request with flags 0, so a server that
       closes between connect and write (PMS restart, transcoder session reaped, keep-alive
       race) would kill the app outright, with no crash-log line: the tracer above handles only
       SEGV/ABRT/BUS/ILL/TRAP. capture.rs already dodges this per-call with MSG_NOSIGNAL
       ("SIGPIPE would kill the app", capture.rs); this covers every other socket in one line. */
    signal(SIGPIPE, SIG_IGN);

#ifndef PLX_DEBUG
    /* No core dumps in a shipping build — this is the other half of the re-raise above, and
       without it that design is actively hostile to the user's TV. The jail sets RLIMIT_CORE
       to INFINITY and /proc/sys/kernel/core_pattern is the bare string "core", i.e. relative
       to cwd = our own app directory. Measured on the dev TV: a 209,965,056-byte core from a
       single crash, sitting on /dev/mmcblk0p53 — 615.6 MB TOTAL, SHARED with every app the
       user has installed, and nothing on the device ever cleans it. Two crashes fill it.
       webosbrew's repository rule 3 ("be considerate to users' TV") is squarely about this.

       What this costs: the kernel's core file, and only that. The tracer above still writes the
       faulting PC and its /proc/self/maps line to the append-only crash log, which is what
       tools/crash-report.sh symbolizes and what every triage in this project has actually used;
       the re-raise still gives SAM a real WIFSIGNALED exit. `make DEBUG=1` keeps cores (and
       DWARF) for the rare case that wants a full post-mortem. */
    setrlimit(RLIMIT_CORE, &(struct rlimit){ 0, 0 });
#endif
}

/* Open the event log TRUNCATED (fresh each launch, as `make run` and tests/run.py both assume)
 * but in APPEND mode, so every write lands at end-of-file.
 *
 * Both halves matter, and it used to be `fopen(…, "w")`, which gets only the first. A "w" stream
 * carries its own file offset, while Rust's `crate::log` opens the same path with O_APPEND — so
 * the two writers do not see each other's output and the C side writes back over lines the Rust
 * side has already appended. That is not theoretical: it ate the first two characters of the very
 * first Rust log line at boot ("surface: …" arrived as "rface: …"), which is exactly the kind of
 * corruption that makes a log untrustworthy at the moment you most need it — and it silently ate
 * whole lines whenever the C side wrote more than Rust had.
 *
 * Mode 0600 because this file records the server name, the LAN address, Plex Home profile names
 * and episode titles, and /tmp is world-readable. */
static FILE *open_event_log(void) {
    int fd = open("/tmp/plxnative-events.log", O_WRONLY | O_CREAT | O_TRUNC | O_APPEND, 0600);
    return fd >= 0 ? fdopen(fd, "a") : NULL;
}

int main(int argc, char **argv) {
    (void)argc; (void)argv;
    elogf = open_event_log();
    clogf = fopen("/tmp/plxnative-crash.log", "a");     /* append: keep prior crashes across relaunches */
    freopen("/tmp/plxnative-stderr.log", "w", stderr); /* capture abort/assert text */
    install_crash_tracer();
    /* request BACK key delivery from the webOS access policy (before SDL init) */
    setenv("SDL_WEBOS_ACCESS_POLICY_KEYS_BACK", "true", 1);
    return plex_run(PMS_HOST, PMS_PORT);
}
