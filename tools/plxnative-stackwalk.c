/*
 * plxnative-stackwalk — remote ARM stack samples for a live process.
 *
 * This is a developer helper, never part of the application package.  Run it as root on the TV.
 * It attaches to one Linux LWP at a time, unwinds through the same libunwind ptrace accessors as
 * the shipped Sentry crash daemon, prints JSONL, and detaches before moving to the next LWP.
 * The host-side tools/plxnative-sample wrapper owns deployment, PID/build identity, aggregation,
 * symbolization, TV locking, and cleanup.
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <inttypes.h>
#include <limits.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include <libunwind-ptrace.h>
#include <libunwind.h>

#ifndef __WALL
#define __WALL 0x40000000
#endif
#ifndef PTRACE_SEIZE
#define PTRACE_SEIZE 0x4206
#endif
#ifndef PTRACE_INTERRUPT
#define PTRACE_INTERRUPT 0x4207
#endif
#ifndef PTRACE_EVENT_STOP
#define PTRACE_EVENT_STOP 128
#endif

#define MAX_TIDS 512
#define MAX_FRAMES_LIMIT 128
#define TEXT_LEN 256

struct thread_info {
    char comm[TEXT_LEN];
    char state;
    char wchan[TEXT_LEN];
    unsigned long long utime;
    unsigned long long stime;
};

static void usage(const char *argv0)
{
    fprintf(stderr,
        "usage: %s --pid PID [--all-threads] [--samples N] "
        "[--interval-ms N] [--max-frames N]\n",
        argv0);
}

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

static void json_string(const char *text)
{
    putchar('"');
    for (const unsigned char *p = (const unsigned char *)text; *p; ++p) {
        switch (*p) {
        case '"': fputs("\\\"", stdout); break;
        case '\\': fputs("\\\\", stdout); break;
        case '\b': fputs("\\b", stdout); break;
        case '\f': fputs("\\f", stdout); break;
        case '\n': fputs("\\n", stdout); break;
        case '\r': fputs("\\r", stdout); break;
        case '\t': fputs("\\t", stdout); break;
        default:
            if (*p < 0x20) {
                printf("\\u%04x", (unsigned int)*p);
            } else {
                putchar((int)*p);
            }
        }
    }
    putchar('"');
}

static void trim_line(char *text)
{
    size_t n = strlen(text);
    while (n > 0 && (text[n - 1] == '\n' || text[n - 1] == '\r')) {
        text[--n] = '\0';
    }
}

static bool read_small_file(const char *path, char *out, size_t out_len)
{
    FILE *f = fopen(path, "r");
    if (!f) {
        out[0] = '\0';
        return false;
    }
    bool ok = fgets(out, (int)out_len, f) != NULL;
    fclose(f);
    if (!ok) {
        out[0] = '\0';
        return false;
    }
    trim_line(out);
    return true;
}

static void read_thread_info(pid_t pid, pid_t tid, struct thread_info *info)
{
    memset(info, 0, sizeof(*info));
    info->state = '?';
    char path[96];
    snprintf(path, sizeof(path), "/proc/%d/task/%d/comm", (int)pid, (int)tid);
    read_small_file(path, info->comm, sizeof(info->comm));
    snprintf(path, sizeof(path), "/proc/%d/task/%d/wchan", (int)pid, (int)tid);
    read_small_file(path, info->wchan, sizeof(info->wchan));

    snprintf(path, sizeof(path), "/proc/%d/task/%d/stat", (int)pid, (int)tid);
    char stat_line[2048];
    if (!read_small_file(path, stat_line, sizeof(stat_line))) {
        return;
    }
    char *after_name = strrchr(stat_line, ')');
    if (!after_name || after_name[1] != ' ') {
        return;
    }
    char *save = NULL;
    char *tok = strtok_r(after_name + 2, " ", &save);
    int index = 0;
    while (tok) {
        if (index == 0) {
            info->state = tok[0];
        } else if (index == 11) {
            info->utime = strtoull(tok, NULL, 10);
        } else if (index == 12) {
            info->stime = strtoull(tok, NULL, 10);
            break;
        }
        ++index;
        tok = strtok_r(NULL, " ", &save);
    }
}

static int compare_pid(const void *left, const void *right)
{
    const pid_t a = *(const pid_t *)left;
    const pid_t b = *(const pid_t *)right;
    return (a > b) - (a < b);
}

static size_t enumerate_tids(pid_t pid, pid_t *out, size_t cap)
{
    char path[64];
    snprintf(path, sizeof(path), "/proc/%d/task", (int)pid);
    DIR *dir = opendir(path);
    if (!dir) {
        return 0;
    }
    size_t count = 0;
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL && count < cap) {
        if (!isdigit((unsigned char)entry->d_name[0])) {
            continue;
        }
        long value;
        if (parse_long(entry->d_name, 1, INT_MAX, &value)) {
            out[count++] = (pid_t)value;
        }
    }
    closedir(dir);
    qsort(out, count, sizeof(out[0]), compare_pid);
    return count;
}

enum attach_result {
    ATTACH_REFUSED = 0,
    ATTACH_STOPPED = 1,
    ATTACH_UNRESOLVED = -1,
};

static bool detach_thread(pid_t tid)
{
    for (;;) {
        if (ptrace(PTRACE_DETACH, tid, NULL, NULL) == 0 || errno == ESRCH) {
            return true;
        }
        if (errno != EINTR) {
            return false;
        }
    }
}

static enum attach_result attach_and_wait(pid_t tid, char *error, size_t error_len)
{
    // PTRACE_ATTACH sends SIGSTOP. If a D-state thread does not enter ptrace-stop before our
    // timeout, PTRACE_DETACH is illegal and that late SIGSTOP can freeze it after the sampler has
    // moved on. SEIZE + INTERRUPT requests a ptrace stop without injecting a process signal; on
    // an unresolved wait the helper exits immediately, and tracer death drops the relationship.
    if (ptrace(PTRACE_SEIZE, tid, NULL, NULL) != 0) {
        snprintf(error, error_len, "ptrace seize: %s", strerror(errno));
        return ATTACH_REFUSED;
    }
    if (ptrace(PTRACE_INTERRUPT, tid, NULL, NULL) != 0) {
        snprintf(error, error_len, "ptrace interrupt: %s", strerror(errno));
        return ATTACH_UNRESOLVED;
    }
    int status = 0;
    for (int retry = 0; retry < 100; ++retry) {
        pid_t got = waitpid(tid, &status, __WALL | WNOHANG);
        if (got == tid) {
            const unsigned int event = (unsigned int)status >> 16;
            const int stop_signal = WIFSTOPPED(status) ? WSTOPSIG(status) : 0;
            // Only consume the synthetic stop PTRACE_INTERRUPT requested. A real
            // signal-delivery-stop or an existing group-stop racing it belongs to the target;
            // detaching with signal 0 would swallow/change that event. Treat it as unresolved
            // and exit the tracer, whose automatic detach preserves the target's stop state.
            if (WIFSTOPPED(status) && stop_signal == SIGTRAP
                && event == PTRACE_EVENT_STOP) {
                return ATTACH_STOPPED;
            }
            snprintf(error, error_len, "unexpected stop status=%d signal=%d event=%u",
                status, stop_signal, event);
            return ATTACH_UNRESOLVED;
        }
        if (got < 0) {
            snprintf(error, error_len, "waitpid: %s", strerror(errno));
            return ATTACH_UNRESOLVED;
        }
        sleep_ms(10);
    }
    snprintf(error, error_len, "stop timeout");
    return ATTACH_UNRESOLVED;
}

static void print_registers(unw_cursor_t *cursor)
{
    bool first = true;
    putchar('{');
    for (unw_regnum_t reg = 0; reg <= UNW_REG_LAST; ++reg) {
        const char *name = unw_regname(reg);
        unw_word_t value = 0;
        if (!name || strcmp(name, "???") == 0 || unw_get_reg(cursor, reg, &value) < 0) {
            continue;
        }
        if (!first) {
            putchar(',');
        }
        first = false;
        json_string(name);
        printf(":%" PRIu64, (uint64_t)value);
    }
    putchar('}');
}

static int sample_thread(pid_t pid, pid_t tid, unsigned int sample,
    unsigned int max_frames)
{
    struct thread_info info;
    read_thread_info(pid, tid, &info);
    char error[TEXT_LEN] = "";
    enum attach_result attached = attach_and_wait(tid, error, sizeof(error));
    if (attached != ATTACH_STOPPED) {
        printf("{\"type\":\"thread\",\"sample\":%u,\"tid\":%d,\"comm\":",
            sample, (int)tid);
        json_string(info.comm);
        printf(",\"state\":\"%c\",\"wchan\":", info.state);
        json_string(info.wchan);
        fputs(",\"error\":", stdout);
        json_string(error);
        fputs(",\"frames\":[]}\n", stdout);
        fflush(stdout);
        // UNRESOLVED means SEIZE succeeded but no legal detach point was observed. The caller
        // must exit the tracer now; continuing would leave that relationship alive while later
        // samples make the failure look recoverable.
        return attached == ATTACH_UNRESOLVED ? -1 : 0;
    }

    unw_addr_space_t address_space = unw_create_addr_space(&_UPT_accessors, 0);
    void *upt = address_space ? _UPT_create(tid) : NULL;
    unw_cursor_t cursor;
    bool cursor_ok = address_space && upt && unw_init_remote(&cursor, address_space, upt) == 0;

    printf("{\"type\":\"thread\",\"sample\":%u,\"tid\":%d,\"comm\":",
        sample, (int)tid);
    json_string(info.comm);
    printf(",\"state\":\"%c\",\"wchan\":", info.state);
    json_string(info.wchan);
    printf(",\"utime\":%llu,\"stime\":%llu", info.utime, info.stime);

    if (!cursor_ok) {
        fputs(",\"error\":\"unw_init_remote\",\"registers\":{},\"frames\":[]", stdout);
    } else {
        fputs(",\"registers\":", stdout);
        print_registers(&cursor);
        fputs(",\"frames\":[", stdout);
        bool first = true;
        for (unsigned int frame = 0; frame < max_frames; ++frame) {
            unw_word_t ip = 0;
            if (unw_get_reg(&cursor, UNW_REG_IP, &ip) < 0 || ip == 0) {
                break;
            }
            char symbol[TEXT_LEN] = "";
            unw_word_t offset = 0;
            int named = unw_get_proc_name(&cursor, symbol, sizeof(symbol), &offset);
            if (!first) {
                putchar(',');
            }
            first = false;
            printf("{\"ip\":%" PRIu64 ",\"symbol\":", (uint64_t)ip);
            json_string((named == 0 || named == -UNW_ENOMEM) ? symbol : "");
            printf(",\"offset\":%" PRIu64 "}",
                (uint64_t)((named == 0 || named == -UNW_ENOMEM) ? offset : 0));
            if (unw_step(&cursor) <= 0) {
                break;
            }
        }
        putchar(']');
    }
    fputs("}\n", stdout);
    fflush(stdout);

    if (upt) {
        _UPT_destroy(upt);
    }
    if (address_space) {
        unw_destroy_addr_space(address_space);
    }
    if (!detach_thread(tid)) {
        return -1;
    }
    return cursor_ok ? 1 : 0;
}

int main(int argc, char **argv)
{
    pid_t pid = 0;
    bool all_threads = false;
    long samples = 1;
    long interval_ms = 0;
    long max_frames = 64;

    for (int i = 1; i < argc; ++i) {
        long value;
        if (strcmp(argv[i], "--pid") == 0 && i + 1 < argc
            && parse_long(argv[++i], 1, INT_MAX, &value)) {
            pid = (pid_t)value;
        } else if (strcmp(argv[i], "--all-threads") == 0) {
            all_threads = true;
        } else if (strcmp(argv[i], "--samples") == 0 && i + 1 < argc
            && parse_long(argv[++i], 1, 10000, &samples)) {
        } else if (strcmp(argv[i], "--interval-ms") == 0 && i + 1 < argc
            && parse_long(argv[++i], 0, 60000, &interval_ms)) {
        } else if (strcmp(argv[i], "--max-frames") == 0 && i + 1 < argc
            && parse_long(argv[++i], 1, MAX_FRAMES_LIMIT, &max_frames)) {
        } else {
            usage(argv[0]);
            return 2;
        }
    }
    if (pid <= 0) {
        usage(argv[0]);
        return 2;
    }

    printf("{\"type\":\"info\",\"pid\":%d,\"samples\":%ld,"
           "\"interval_ms\":%ld,\"all_threads\":%s,\"max_frames\":%ld}\n",
        (int)pid, samples, interval_ms, all_threads ? "true" : "false", max_frames);
    fflush(stdout);

    bool any = false;
    for (long sample = 0; sample < samples; ++sample) {
        printf("{\"type\":\"sample\",\"sample\":%ld,\"monotonic_ns\":%" PRIu64 "}\n",
            sample, monotonic_ns());
        if (all_threads) {
            pid_t tids[MAX_TIDS];
            size_t count = enumerate_tids(pid, tids, MAX_TIDS);
            for (size_t i = 0; i < count; ++i) {
                int sampled = sample_thread(pid, tids[i], (unsigned int)sample,
                    (unsigned int)max_frames);
                if (sampled < 0) {
                    return 1;
                }
                any |= sampled > 0;
            }
        } else {
            int sampled = sample_thread(pid, pid, (unsigned int)sample,
                (unsigned int)max_frames);
            if (sampled < 0) {
                return 1;
            }
            any |= sampled > 0;
        }
        if (sample + 1 < samples && interval_ms > 0) {
            sleep_ms((unsigned int)interval_ms);
        }
    }
    return any ? 0 : 1;
}
