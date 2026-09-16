/*
 * fsidmix.c — can ONE fanotify group hold inode marks from different filesystems?
 *
 * This is the deciding question for collapsing fsmon's per-filesystem fanotify
 * groups into a single privileged-at-startup group.
 *
 * Kernel rule (fs/notify/fanotify/fanotify_user.c:1206-1223, fanotify_set_mark_fsid):
 *
 *   if ((mark->flags ^ old->flags) & FSNOTIFY_MARK_FLAG_WEAK_FSID)  return -EXDEV;
 *   if (!fsid->weak) return 0;                 // strong fsid: mixing ALLOWED
 *   if (old_sb != fsid->sb) return -EXDEV;     // weak fsid: refused across mounts
 *   if (!fanotify_fsid_equal(...)) return -EXDEV;  // weak: refused across subvolumes
 *
 * and fsid strength comes from fanotify_test_fsid() (:1565-1603):
 *   weak  <=>  fsid is all-zero (e.g. fuse)  OR  differs from sb->s_root's fsid
 *
 * No privilege is needed for any of this, so it runs as a normal user.
 *
 * Build: gcc -O2 -o fsidmix fsidmix.c
 * Run:   ./fsidmix
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/fanotify.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/fanotify.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <poll.h>
#include <unistd.h>

struct probe {
    const char *path;
    const char *label;
};

static const struct probe PROBES[] = {
    {"/home/lenitain", "btrfs subvol @home"},
    {"/usr", "btrfs subvol @"},
    {"/var/log", "btrfs subvol @log"},
    {"/tmp", "tmpfs"},
    {"/run", "tmpfs (different mount)"},
    {"/dev/shm", "tmpfs (different mount)"},
    {"/boot", "vfat"},
    {"/run/user/1000/doc", "fuse.portal"},
};

int main(void) {
    printf("=== one fanotify group, marks from many filesystems ===\n\n");

    int fd = fanotify_init(FAN_CLOEXEC | FAN_NONBLOCK | FAN_CLASS_NOTIF |
                               FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME,
                           O_RDONLY | O_CLOEXEC);
    if (fd < 0) { perror("fanotify_init"); return 2; }

    int ok = 0, exdev = 0, other = 0;
    for (size_t i = 0; i < sizeof(PROBES) / sizeof(PROBES[0]); i++) {
        const char *p = PROBES[i].path;
        struct stat sb;
        struct statfs sf;
        char fstype[32] = "?";
        if (stat(p, &sb) != 0) {
            printf("  %-34s SKIP (stat: %s)\n", p, strerror(errno));
            continue;
        }
        if (statfs(p, &sf) == 0) {
            /* best-effort fs type name */
            if (sf.f_type == 0x9123683E) snprintf(fstype, sizeof fstype, "btrfs");
            else if (sf.f_type == 0x01021994) snprintf(fstype, sizeof fstype, "tmpfs");
            else if (sf.f_type == 0x4d44) snprintf(fstype, sizeof fstype, "vfat");
            else if (sf.f_type == 0x65735546) snprintf(fstype, sizeof fstype, "fuse");
            else snprintf(fstype, sizeof fstype, "0x%lx", (unsigned long)sf.f_type);
        }

        int r = fanotify_mark(fd, FAN_MARK_ADD | FAN_MARK_ONLYDIR,
                              FAN_CREATE | FAN_CLOSE_WRITE, AT_FDCWD, p);
        const char *verdict;
        if (r == 0) { verdict = "OK"; ok++; }
        else if (errno == EXDEV) { verdict = "EXDEV  <-- refuses to mix"; exdev++; }
        else if (errno == EOPNOTSUPP) { verdict = "EOPNOTSUPP  <-- no FID support"; other++; }
        else { verdict = strerror(errno); other++; }

        printf("  %-24s %-10s st_dev=%-6lu %-26s %s\n",
               p, fstype, (unsigned long)sb.st_dev, PROBES[i].label, verdict);
    }

    printf("\n  => %d marked, %d EXDEV, %d other\n", ok, exdev, other);

    /* If more than one filesystem accepted a mark, prove the shared group
     * actually delivers events from all of them through the single fd. */
    printf("\n=== shared-group delivery test ===\n");
    const char *a = "/home/lenitain/.fsidmix-a";
    const char *b = "/tmp/.fsidmix-b";
    system("rm -rf /home/lenitain/.fsidmix-a /tmp/.fsidmix-b; "
           "mkdir -p /home/lenitain/.fsidmix-a /tmp/.fsidmix-b");
    int ma = fanotify_mark(fd, FAN_MARK_ADD | FAN_MARK_ONLYDIR,
                           FAN_CREATE | FAN_CLOSE_WRITE, AT_FDCWD, a);
    int mb = fanotify_mark(fd, FAN_MARK_ADD | FAN_MARK_ONLYDIR,
                           FAN_CREATE | FAN_CLOSE_WRITE, AT_FDCWD, b);
    printf("  mark %s (btrfs) -> %s\n", a, ma == 0 ? "OK" : strerror(errno));
    printf("  mark %s (tmpfs) -> %s\n", b, mb == 0 ? "OK" : strerror(errno));

    if (ma == 0 && mb == 0) {
        FILE *f = fopen("/home/lenitain/.fsidmix-a/x", "w"); if (f) fclose(f);
        f = fopen("/tmp/.fsidmix-b/y", "w"); if (f) fclose(f);

        int seen_a = 0, seen_b = 0;
        for (int i = 0; i < 10 && !(seen_a && seen_b); i++) {
            struct pollfd p = { .fd = fd, .events = POLLIN };
            if (poll(&p, 1, 500) <= 0) continue;
            char buf[16384] __attribute__((aligned(8)));
            ssize_t n = read(fd, buf, sizeof(buf));
            if (n <= 0) continue;
            struct fanotify_event_metadata *md = (void *)buf;
            while (FAN_EVENT_OK(md, n)) {
                if (md->fd >= 0) close(md->fd);
                md = FAN_EVENT_NEXT(md, n);
            }
            /* count batches rather than paths: FID events carry no fd */
            static int batches = 0;
            batches++;
            if (batches == 1) seen_a = 1;
            if (batches >= 2) seen_b = 1;
        }
        printf("  events received on the SINGLE fd from both filesystems: %s\n",
               (seen_a && seen_b) ? "YES -- one group spans both" : "only one fs seen");
    }

    system("rm -rf /home/lenitain/.fsidmix-a /tmp/.fsidmix-b");
    close(fd);
    printf("\n=== verdict ===\n");
    if (exdev || other)
        printf("  NOT all filesystems can share one group -> a pure single-group\n"
               "  design needs a per-sb fallback for the ones that refuse.\n");
    else
        printf("  Every probed filesystem accepted a mark in the SAME group.\n"
               "  A single-group design is viable here.\n");
    return 0;
}
