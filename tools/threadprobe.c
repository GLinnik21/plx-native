/* threadprobe — what actually stops this TV from creating another thread, and at what count?
 *
 * `std::thread::spawn` panics by unwrapping `Builder::spawn`
 * (library/std/src/thread/functions.rs: `Builder::new().spawn(f).expect("failed to spawn thread")`),
 * and the crate's `task.rs` exists to turn that into a return value. The obvious follow-up — is
 * the refusal reachable at all on this hardware? — was answered from /proc arithmetic
 * (RLIMIT_NPROC 3746 vs 31 threads at playback peak). This probe answers it by measurement
 * instead: drop to the app's uid, spawn with the same stack size Rust uses, and keep going until
 * pthread_create says no.
 *
 * Build (host):  make threadprobe
 * Run (TV, as root so the setuid works):
 *     ./threadprobe [stack_kb] [uid] [cap]
 *   stack_kb  2048 = Rust's default thread stack; 256 = crate::task::spawn_small.  (default 2048)
 *   uid       drop to this uid before spawning, so RLIMIT_NPROC is counted the way the jailed
 *             app experiences it.  0 = stay root.                                  (default 6910)
 *   cap       stop after this many successes, so a probe on a live TV stays bounded.(default 4096)
 *
 * The threads park in pause() and are never joined — the process exits as soon as the answer is
 * known, and they die with it.  Nothing is written to disk and no app state is touched.
 *
 * MEASURED on the LG 49SM9000PLA (webOS 4.5, 2026-07-28), uid 6910, app closed:
 *
 *   stack     refused at    VmSize there   binding limit
 *   2048 kB   2043 threads  4188 MB        RLIMIT_AS (4294967295 = the full AArch32 4 GB space)
 *    256 kB   3745 threads   963 MB        RLIMIT_NPROC (3746, exactly)
 *
 * Both refusals are EAGAIN (11), which is what `std::thread::spawn` unwraps into a panic.  So the
 * failure is real and reproducible — and needs 2043 threads against the app's 31 at playback peak
 * (VmSize 363 MB, ~11x address-space headroom).  RSS cost is trivial either way: ~12 kB/thread,
 * 31 MB at 3746 threads.
 *
 * Two things worth keeping from this: which limit binds depends on the stack size, and the
 * crossover is between 256 kB and 2 MB — so `task::spawn_small`'s 256 kB is not a micro-
 * optimisation, it is the difference between spending address space and spending thread slots.
 *
 * `threadprobe time [stack_kb] [iters]` answers the other half: what does spawning COST, which is
 * what decides whether a thread-per-request is affordable from a 16600 us frame budget.  Same TV,
 * 300 iterations of create+join:
 *
 *   256 kB   mean 58us  median 45us  p95 125us  max  753us
 *  2048 kB   mean 82us  median 50us  p95 182us  max 1830us
 *
 * The app pays only the create half on the SDL loop (nothing joins these), so ~0.3% of a frame in
 * the typical case.  Note the tail: a 2 MB stack can cost 1830 us — 11% of a frame — where 256 kB
 * tops out at 753 us.  A third reason the loop-initiated workers use the small stack.
 */
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <time.h>
#include <unistd.h>

static void *park(void *arg)
{
    (void)arg;
    for (;;) {
        pause(); /* no CPU, no allocation — the thread exists and does nothing else */
    }
    return NULL;
}

/* One /proc/self/status field, or -1. Cheaper than pulling in a parser for three numbers. */
static long status_kb(const char *key)
{
    FILE *f = fopen("/proc/self/status", "r");
    char line[256];
    long v = -1;
    if (!f) {
        return -1;
    }
    while (fgets(line, sizeof line, f)) {
        if (strncmp(line, key, strlen(key)) == 0) {
            v = strtol(line + strlen(key) + 1, NULL, 10);
            break;
        }
    }
    fclose(f);
    return v;
}

static void report_limits(const char *when)
{
    struct rlimit rl;
    printf("[%s] uid=%d  Threads=%ld  VmSize=%ldkB  VmRSS=%ldkB\n",
           when, (int)getuid(), status_kb("Threads"), status_kb("VmSize"), status_kb("VmRSS"));
    if (getrlimit(RLIMIT_NPROC, &rl) == 0) {
        printf("[%s] RLIMIT_NPROC soft=%lu hard=%lu\n", when,
               (unsigned long)rl.rlim_cur, (unsigned long)rl.rlim_max);
    }
    if (getrlimit(RLIMIT_AS, &rl) == 0) {
        printf("[%s] RLIMIT_AS   soft=%lu hard=%lu\n", when,
               (unsigned long)rl.rlim_cur, (unsigned long)rl.rlim_max);
    }
}

static void *exit_now(void *arg)
{
    (void)arg;
    return NULL;
}

static int cmp_long(const void *a, const void *b)
{
    long x = *(const long *)a, y = *(const long *)b;
    return (x > y) - (x < y);
}

/* How long does ONE create+join cost? This is the number that decides whether spawning per
 * request is affordable from the SDL loop, whose whole frame budget is 16600us. */
static void timing_mode(size_t stack_kb, long n)
{
    pthread_attr_t attr;
    long *us = malloc((size_t)n * sizeof *us);
    long total = 0;

    if (!us) {
        printf("timing: out of memory\n");
        return;
    }
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, stack_kb * 1024);

    for (long i = 0; i < n; i++) {
        struct timespec a, b;
        pthread_t t;
        clock_gettime(CLOCK_MONOTONIC, &a);
        if (pthread_create(&t, &attr, exit_now, NULL) != 0) {
            printf("timing: refused at iteration %ld\n", i);
            free(us);
            return;
        }
        pthread_join(t, NULL);
        clock_gettime(CLOCK_MONOTONIC, &b);
        us[i] = (b.tv_sec - a.tv_sec) * 1000000 + (b.tv_nsec - a.tv_nsec) / 1000;
        total += us[i];
    }
    qsort(us, (size_t)n, sizeof *us, cmp_long);
    printf("create+join x%ld @ %zukB stack: mean=%ldus median=%ldus p95=%ldus max=%ldus\n",
           n, stack_kb, total / n, us[n / 2], us[(n * 95) / 100], us[n - 1]);
    printf("  (one 60fps frame is 16600us)\n");
    free(us);
}

int main(int argc, char **argv)
{
    /* timing mode: ./threadprobe time [stack_kb] [iterations] */
    if (argc > 1 && strcmp(argv[1], "time") == 0) {
        size_t stack_kb = (argc > 2) ? (size_t)strtoul(argv[2], NULL, 10) : 256;
        long   iters    = (argc > 3) ? strtol(argv[3], NULL, 10)          : 200;
        timing_mode(stack_kb, iters);
        return 0;
    }

    size_t stack_kb = (argc > 1) ? (size_t)strtoul(argv[1], NULL, 10) : 2048;
    uid_t  uid      = (argc > 2) ? (uid_t)strtoul(argv[2], NULL, 10)  : 6910;
    long   cap      = (argc > 3) ? strtol(argv[3], NULL, 10)          : 4096;

    printf("threadprobe: stack=%zukB target_uid=%u cap=%ld\n", stack_kb, (unsigned)uid, cap);
    report_limits("before");

    /* Drop to the app's uid so the per-uid RLIMIT_NPROC tally is the one the app lives under.
     * setgid first — after setuid the privilege to change groups is gone. */
    if (uid != 0) {
        if (setgid((gid_t)uid) != 0 || setuid(uid) != 0) {
            printf("threadprobe: could not drop to uid %u (%s) — staying root, RLIMIT_NPROC "
                   "tally will NOT match the app's\n", (unsigned)uid, strerror(errno));
        }
    }
    report_limits("as-app");

    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, stack_kb * 1024);
    pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);

    long n = 0;
    int rc = 0;
    while (n < cap) {
        pthread_t t;
        rc = pthread_create(&t, &attr, park, NULL);
        if (rc != 0) {
            break;
        }
        n++;
        /* progress, sparse enough not to dominate the run */
        if (n % 256 == 0) {
            printf("  ... %ld threads, VmSize=%ldkB VmRSS=%ldkB\n",
                   n, status_kb("VmSize"), status_kb("VmRSS"));
            fflush(stdout);
        }
    }

    if (rc != 0) {
        /* pthread_create returns the error code; it does NOT set errno. */
        printf("REFUSED after %ld threads: pthread_create -> %d (%s)\n", n, rc, strerror(rc));
    } else {
        printf("NO REFUSAL: hit the %ld cap with every spawn succeeding\n", cap);
    }
    report_limits("at-end");
    /* Exit without joining: the parked threads go with the process. */
    return 0;
}
