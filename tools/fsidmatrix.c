/*
 * fsidmatrix.c — determine the EXACT rule for which paths can share one
 * fanotify group, by brute force.
 *
 * For every ordered pair (first, second): create a FRESH group, mark `first`,
 * then try to mark `second` in the same group. Report OK / EXDEV.
 *
 * This is ground truth for the fsmon design question, independent of my
 * reading of the kernel's weak-fsid logic.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/fanotify.h>
#include <stdio.h>
#include <string.h>
#include <sys/fanotify.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <unistd.h>

static const char *PATHS[] = {
    "/etc",          /* subvol @  (mounted at /)   */
    "/usr",          /* subvol @  (same sb as /)   */
    "/var/log",      /* subvol @log                */
    "/home",         /* subvol @home               */
    "/tmp",          /* tmpfs                      */
    "/dev/shm",      /* tmpfs (different mount)    */
};
#define N (sizeof(PATHS) / sizeof(PATHS[0]))

static int mark_in_new_group(const char *first, const char *second, int *err_out) {
    int fd = fanotify_init(FAN_CLOEXEC | FAN_NONBLOCK | FAN_CLASS_NOTIF |
                               FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME,
                           O_RDONLY | O_CLOEXEC);
    if (fd < 0) { *err_out = errno; return -1; }
    if (fanotify_mark(fd, FAN_MARK_ADD | FAN_MARK_ONLYDIR, FAN_CREATE,
                      AT_FDCWD, first) != 0) {
        *err_out = errno; close(fd); return -1;
    }
    int r = fanotify_mark(fd, FAN_MARK_ADD | FAN_MARK_ONLYDIR, FAN_CREATE,
                          AT_FDCWD, second);
    *err_out = errno;
    close(fd);
    return r;
}

int main(void) {
    printf("=== can these two paths share ONE fanotify group? ===\n\n");

    printf("  fsids:\n");
    for (size_t i = 0; i < N; i++) {
        struct statfs sf; struct stat sb;
        statfs(PATHS[i], &sf); stat(PATHS[i], &sb);
        printf("    %-12s st_dev=%-6lu fsid=%08x%08x\n", PATHS[i],
               (unsigned long)sb.st_dev, (unsigned)sf.f_fsid.__val[0],
               (unsigned)sf.f_fsid.__val[1]);
    }

    printf("\n  matrix (row = FIRST mark, col = SECOND mark in same group):\n\n");
    printf("  %-12s", "FIRST\\2nd");
    for (size_t j = 0; j < N; j++) printf(" %-11s", PATHS[j]);
    printf("\n");

    for (size_t i = 0; i < N; i++) {
        printf("  %-12s", PATHS[i]);
        for (size_t j = 0; j < N; j++) {
            int err = 0;
            int r = mark_in_new_group(PATHS[i], PATHS[j], &err);
            char cell[16];
            if (r == 0) snprintf(cell, sizeof cell, "OK");
            else if (err == EXDEV) snprintf(cell, sizeof cell, "EXDEV");
            else if (err == EACCES) snprintf(cell, sizeof cell, "EACCES");
            else snprintf(cell, sizeof cell, "e%d", err);
            printf(" %-11s", cell);
        }
        printf("\n");
    }

    printf("\n=== interpretation ===\n");
    printf("  OK on the diagonal is trivial. An off-diagonal OK means those two\n"
           "  filesystems can genuinely share one group (strong fsid mixing).\n"
           "  EXDEV means the kernel refuses -> each needs its OWN group.\n");
    return 0;
}
