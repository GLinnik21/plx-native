/* tools/logmprobe.c — read (and optionally flip) LG's KADP log MASK, on a RUNNING app.
 *
 * WHY THIS EXISTS. The Dolby Vision display-management path logs the one line that would settle
 * an open investigation — `DOVI_MDAsync_WriteOTTMetaData` line 248, "Write buffer = 0x%x Count =
 * %d PTS = %d DMSize = %d", the key it actually put in the LUT ring — and that line is emitted at
 * KADP log level 2, which is NOT ENABLED on this set. `KADP_LOGM_WriteLog` (libkadaptor.so.2.0.1
 * @0x88c34) gates BITWISE, not by threshold:
 *
 *     if ((1 << (level & 0xff) & rec[0x20] & ~rec[0x24]) == 0) return 0;
 *
 * so a level-3 line being visible says nothing about level 2, and the absence of a level-2 line is
 * NOT evidence that its function did not run. That misreading cost this investigation a day.
 *
 * The mask table is not per-process state: `KADP_LOGM_Open` (@0x876bc) does
 * open("/dev/lg/logm", O_RDWR) -> ioctl(fd, 0x80046101, &size) -> mmap(NULL, size, PROT_READ,
 * MAP_SHARED, fd, 0), so every process shares one kernel-backed table. That is what makes this
 * tool possible: the mask can be read, and flipped, from a SECOND ssh session while the app is
 * playing — no rebuild, no relaunch, no perturbation of the session being measured.
 *
 * Records are 0x54 bytes: char name[0x20]; u32 enable_mask@0x20; u32 hidden_mask@0x24; ...
 *
 * DEFAULT MODE IS READ-ONLY and takes no arguments. Writing requires an explicit `set`/`clear`,
 * because this changes state on a device shared with whatever else is running.
 *
 *   make logmprobe && scp pkg/logmprobe root@TV:/tmp/ && ssh root@TV /tmp/logmprobe
 *   ssh root@TV '/tmp/logmprobe set kad-hdr 2'      # arm level 2
 *   ssh root@TV '/tmp/logmprobe clear kad-hdr 2'    # put it back
 *
 * Delete it from the TV when done — it is a diagnostic, not a deployed file.
 */
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <errno.h>
#include <stdint.h>

#define REC 0x54
#define IOCTL_SIZE 0x80046101u
#define IOCTL_CTL  0xC0286100u

/* KADP_LOGM_BitMaskEnable @0x87e4c / KADP_LOGM_MaskSet @0x87f88 both go through ioctl CTL with a
 * 0x28-byte argument whose first four words are {id, mask, bit, op}. op 3 = enable one bit. The
 * op numbers are read off those two functions; anything else is left alone deliberately. */
struct ctl { uint32_t id, mask, bit, op; unsigned char pad[24]; };

int main(int argc, char **argv) {
    int fd = open("/dev/lg/logm", O_RDWR);
    if (fd < 0) { printf("open /dev/lg/logm: %s\n", strerror(errno)); return 1; }
    unsigned size = 0;
    if (ioctl(fd, IOCTL_SIZE, &size) < 0 || size == 0 || size > (64u << 20)) {
        printf("ioctl size: rv/size bad (%s, size=%u)\n", strerror(errno), size); return 1;
    }
    unsigned char *m = mmap(NULL, size, PROT_READ, MAP_SHARED, fd, 0);
    if (m == MAP_FAILED) { printf("mmap: %s\n", strerror(errno)); return 1; }
    printf("logm table: %u bytes, %u records\n", size, size / REC);

    const char *want = (argc >= 3) ? argv[2] : NULL;
    int found = -1;
    for (unsigned i = 0; i < size / REC; i++) {
        unsigned char *r = m + (size_t)i * REC;
        if (r[0] < 0x20 || r[0] > 0x7e) continue;              /* not a live record */
        char name[0x21]; memcpy(name, r, 0x20); name[0x20] = 0;
        uint32_t en, hid;
        memcpy(&en,  r + 0x20, 4);
        memcpy(&hid, r + 0x24, 4);
        int show = !want || strstr(name, want) != NULL;
        if (want && strstr(name, want)) found = (int)i;
        if (show)
            printf("  id=%-4u %-24s enable=0x%08x hidden=0x%08x  ->  levels on:%s%s%s%s%s%s\n",
                   i, name, en, hid,
                   ((en & ~hid) & (1u<<0)) ? " 0" : "", ((en & ~hid) & (1u<<1)) ? " 1" : "",
                   ((en & ~hid) & (1u<<2)) ? " 2" : "", ((en & ~hid) & (1u<<3)) ? " 3" : "",
                   ((en & ~hid) & (1u<<4)) ? " 4" : "", ((en & ~hid) & (1u<<5)) ? " 5" : "");
    }
    if (argc < 2 || !strcmp(argv[1], "show")) return 0;

    if (argc < 4) { printf("usage: logmprobe [show|set|clear] <name-substring> <bit>\n"); return 2; }
    if (found < 0) { printf("no record matching '%s'\n", want); return 3; }
    struct ctl a; memset(&a, 0, sizeof a);
    a.id = (uint32_t)found;
    a.bit = (uint32_t)atoi(argv[3]);
    if (!strcmp(argv[1], "set")) a.op = 3;                     /* BitMaskEnable */
    else if (!strcmp(argv[1], "clear")) a.op = 4;              /* BitMaskDisable, by symmetry */
    else { printf("unknown verb '%s'\n", argv[1]); return 2; }
    int rv = ioctl(fd, IOCTL_CTL, &a);
    printf("ioctl(CTL) id=%u bit=%u op=%u -> rv=%d (%s)\n", a.id, a.bit, a.op, rv,
           rv < 0 ? strerror(errno) : "ok");
    /* Read the mask BACK — an ioctl that returns 0 having done nothing is the failure mode that
     * would otherwise be reported as success and then quoted. */
    unsigned char *r = m + (size_t)found * REC;
    uint32_t en, hid; memcpy(&en, r + 0x20, 4); memcpy(&hid, r + 0x24, 4);
    printf("after: enable=0x%08x hidden=0x%08x  bit %u effective=%d\n",
           en, hid, a.bit, ((en & ~hid) >> a.bit) & 1);
    return 0;
}
