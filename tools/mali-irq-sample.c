/* Passive Mali activity sampler for the rooted development TV.
 *
 * Prints timestamped snapshots of the Mali/GPU rows from /proc/interrupts.  It changes no driver
 * state and is never packaged with PlxNative.  The harness normalizes deltas per second and keeps
 * each IRQ separate; this helper deliberately does not pretend a global interrupt is process- or
 * phase-attributable.
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <ctype.h>
#include <errno.h>
#include <inttypes.h>
#include <limits.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static bool parse_long(const char *text, long lo, long hi, long *out)
{
    char *end = NULL;
    errno = 0;
    long value = strtol(text, &end, 10);
    if (errno != 0 || !end || *end != '\0' || value < lo || value > hi) {
        return false;
    }
    *out = value;
    return true;
}

static uint64_t monotonic_ns(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0;
    }
    return (uint64_t)ts.tv_sec * UINT64_C(1000000000) + (uint64_t)ts.tv_nsec;
}

static void sleep_ms(unsigned int value)
{
    struct timespec requested = {
        .tv_sec = value / 1000,
        .tv_nsec = (long)(value % 1000) * 1000000L,
    };
    while (nanosleep(&requested, &requested) != 0 && errno == EINTR) {
    }
}

static bool contains_folded(const char *text, const char *needle)
{
    const size_t n = strlen(needle);
    for (const char *at = text; *at; ++at) {
        size_t i = 0;
        while (i < n && at[i]
            && tolower((unsigned char)at[i]) == tolower((unsigned char)needle[i])) {
            ++i;
        }
        if (i == n) {
            return true;
        }
    }
    return false;
}

int main(int argc, char **argv)
{
    long samples = 1;
    long interval_ms = 100;
    if (argc != 3 || !parse_long(argv[1], 1, 100000, &samples)
        || !parse_long(argv[2], 1, 60000, &interval_ms)) {
        fprintf(stderr, "usage: %s SAMPLE_COUNT INTERVAL_MS\n", argv[0]);
        return 2;
    }

    bool found = false;
    for (long sample = 0; sample < samples; ++sample) {
        FILE *source = fopen("/proc/interrupts", "r");
        if (!source) {
            perror("open /proc/interrupts");
            return 1;
        }
        printf("@@sample %ld %" PRIu64 "\n", sample, monotonic_ns());
        char line[1024];
        while (fgets(line, sizeof(line), source)) {
            if (contains_folded(line, "mali") || contains_folded(line, "gpu")) {
                fputs(line, stdout);
                found = true;
            }
        }
        fclose(source);
        fflush(stdout);
        if (sample + 1 < samples) {
            sleep_ms((unsigned int)interval_ms);
        }
    }
    if (!found) {
        fputs("no Mali/GPU IRQ rows found in /proc/interrupts\n", stderr);
        return 1;
    }
    return 0;
}
