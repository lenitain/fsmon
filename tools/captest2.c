// captest2.c — exhaustive privilege probe for exactly the operations fsmon performs.
//
// Every probe mirrors a real call site in fsmon/src, so the output is a direct
// statement of "what privilege does fsmon actually need".
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/fanotify.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>
#include <linux/netlink.h>
#include <linux/connector.h>
#include <linux/cn_proc.h>
#include <poll.h>
#include <sys/wait.h>

static void ok(const char *what, long ret, const char *note) {
    printf("  %-52s %s%s%s\n", what, ret < 0 ? "FAIL " : "OK   ",
           ret < 0 ? strerror(errno) : "", note ? note : "");
}

static int mkfan(unsigned int flags) {
    return fanotify_init(flags | FAN_CLOEXEC, O_RDONLY | O_CLOEXEC);
}

int main(int argc, char **argv) {
    printf("uid=%d euid=%d\n", getuid(), geteuid());
    FILE *f = fopen("/proc/self/status", "r");
    char line[256];
    while (f && fgets(line, sizeof(line), f))
        if (!strncmp(line, "CapEff:", 7)) fputs(line, stdout);
    if (f) fclose(f);

    /* ── fanotify_init: which flag combinations need CAP_SYS_ADMIN? ── */
    printf("\n[fanotify_init]\n");
    int fd;
    fd = mkfan(FAN_CLASS_NOTIF | FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME);
    ok("CLASS_NOTIF|REPORT_FID|REPORT_DIR_FID|REPORT_NAME", fd,
       "  <- fsmon init.rs:159 exact flags");
    if (fd >= 0) close(fd);

    fd = mkfan(FAN_CLASS_NOTIF | FAN_REPORT_FID);
    ok("CLASS_NOTIF|REPORT_FID", fd, NULL);
    if (fd >= 0) close(fd);

    fd = mkfan(FAN_CLASS_NOTIF | FAN_REPORT_FID | FAN_UNLIMITED_QUEUE);
    ok("CLASS_NOTIF|REPORT_FID|UNLIMITED_QUEUE", fd, "  <- needs CAP_SYS_ADMIN");
    if (fd >= 0) close(fd);

    fd = mkfan(FAN_CLASS_NOTIF | FAN_REPORT_FID | FAN_UNLIMITED_MARKS);
    ok("CLASS_NOTIF|REPORT_FID|UNLIMITED_MARKS", fd, "  <- needs CAP_SYS_ADMIN");
    if (fd >= 0) close(fd);

    fd = mkfan(FAN_CLASS_CONTENT | FAN_REPORT_FID);
    ok("CLASS_CONTENT|REPORT_FID", fd, "  <- permission events");
    if (fd >= 0) close(fd);

    /* ── fanotify_mark: does the target's ownership matter? ── */
    printf("\n[fanotify_mark]\n");
    const char *home = getenv("HOME");
    char own[512];
    snprintf(own, sizeof(own), "%s", home ? home : "/tmp");
    int dfd = open(own, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    ok("open(own dir)", dfd, own);

    fd = mkfan(FAN_CLASS_NOTIF | FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME);
    if (fd >= 0 && dfd >= 0) {
        int r = fanotify_mark(fd, FAN_MARK_ADD | FAN_MARK_ONLYDIR,
                              FAN_CLOSE_WRITE | FAN_ONDIR, dfd, NULL);
        ok("mark(own dir, FAN_MARK_ADD, dirfd)", r, NULL);
    }
    if (fd >= 0) close(fd);
    if (dfd >= 0) close(dfd);

    int root_dfd = open("/usr", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    ok("open(/usr dir)", root_dfd, "  (root-owned)");
    fd = mkfan(FAN_CLASS_NOTIF | FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME);
    if (fd >= 0 && root_dfd >= 0) {
        int r = fanotify_mark(fd, FAN_MARK_ADD | FAN_MARK_ONLYDIR,
                              FAN_CLOSE_WRITE | FAN_ONDIR, root_dfd, NULL);
        ok("mark(/usr root-owned dir, dirfd)", r, "  <- fsmon's core use case");
    }
    if (fd >= 0) close(fd);
    if (root_dfd >= 0) close(root_dfd);

    fd = mkfan(FAN_CLASS_NOTIF | FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME);
    if (fd >= 0) {
        int r = fanotify_mark(fd, FAN_MARK_ADD | FAN_MARK_MOUNT,
                              FAN_CLOSE_WRITE | FAN_ONDIR, AT_FDCWD, "/usr");
        ok("mark(FAN_MARK_MOUNT, /usr)", r, NULL);
    }
    if (fd >= 0) close(fd);

    fd = mkfan(FAN_CLASS_NOTIF | FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME);
    if (fd >= 0) {
        int r = fanotify_mark(fd, FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                              FAN_CLOSE_WRITE | FAN_ONDIR, AT_FDCWD, "/usr");
        ok("mark(FAN_MARK_FILESYSTEM, /usr)", r, "  <- needs CAP_SYS_ADMIN");
    }
    if (fd >= 0) close(fd);

    /* ── the FID -> path resolution chain (fid_parser.rs three-tier) ── */
    printf("\n[fid resolution]\n");
    struct file_handle *fh = calloc(1, sizeof(struct file_handle) + 64);
    fh->handle_bytes = 64;
    int mount_id = 0;
    int r = name_to_handle_at(AT_FDCWD, "/usr", fh, &mount_id, 0);
    ok("name_to_handle_at(/usr) [tier-2 dir_cache fill]", r, NULL);
    if (r == 0) {
        int mfd = open("/", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
        int hfd = mfd >= 0 ? open_by_handle_at(mfd, fh, O_RDONLY | O_CLOEXEC) : -1;
        ok("open_by_handle_at [tier-3 syscall fallback]", hfd,
           "  <- needs CAP_DAC_READ_SEARCH");
        if (hfd >= 0) close(hfd);
        if (mfd >= 0) close(mfd);
    }

    /* ── cn_proc proc connector: bind succeeds, but does it DELIVER? ── */
    printf("\n[proc connector cn_proc]\n");
    int nl = socket(PF_NETLINK, SOCK_DGRAM | SOCK_CLOEXEC, NETLINK_CONNECTOR);
    ok("socket(NETLINK_CONNECTOR)", nl, NULL);
    if (nl >= 0) {
        struct sockaddr_nl sa = { .nl_family = AF_NETLINK, .nl_groups = CN_IDX_PROC };
        r = bind(nl, (struct sockaddr *)&sa, sizeof(sa));
        ok("bind(CN_IDX_PROC multicast group)", r, "  <- needs CAP_NET_ADMIN?");
        if (r == 0) {
            struct {
                struct nlmsghdr nl;
                struct cn_msg cn;
                enum proc_cn_mcast_op op;
            } __attribute__((packed)) msg;
            memset(&msg, 0, sizeof(msg));
            msg.nl.nlmsg_len = sizeof(msg);
            msg.nl.nlmsg_type = NLMSG_DONE;
            msg.nl.nlmsg_pid = 0;
            msg.cn.id.idx = CN_IDX_PROC;
            msg.cn.id.val = CN_VAL_PROC;
            msg.cn.len = sizeof(enum proc_cn_mcast_op);
            msg.op = PROC_CN_MCAST_LISTEN;
            r = send(nl, &msg, sizeof(msg), 0);
            ok("send(PROC_CN_MCAST_LISTEN)", r, NULL);

            // The real test: provoke a fork and see whether an event arrives.
            if (r >= 0) {
                pid_t child = fork();
                if (child == 0) _exit(0);
                if (child > 0) {
                    int status;
                    waitpid(child, &status, 0);
                    struct pollfd pfd = { .fd = nl, .events = POLLIN };
                    int pr = poll(&pfd, 1, 1000);
                    char buf[4096];
                    ssize_t n = pr > 0 ? recv(nl, buf, sizeof(buf), 0) : -1;
                    ok("delivery: recv() after fork()", (pr > 0 && n > 0) ? 1 : -1,
                       (pr > 0 && n > 0) ? "  <- events ARE delivered" : "  <- NO events");
                }
            }
        }
        close(nl);
    }

    /* ── cross-uid /proc attribution ── */
    printf("\n[cross-uid /proc reads]\n");
    int target = argc > 1 ? atoi(argv[1]) : 0;
    for (int pid = 1; pid < 65536; pid++) {
        char st[64], path[80];
        struct stat sb;
        snprintf(st, sizeof(st), "/proc/%d", pid);
        if (stat(st, &sb) != 0 || sb.st_uid != (uid_t)target) continue;
        snprintf(path, sizeof(path), "/proc/%d/cmdline", pid);
        int cfd = open(path, O_RDONLY | O_CLOEXEC);
        char note[96];
        snprintf(note, sizeof(note), "  [pid %d uid %d]", pid, target);
        ok("open(/proc/<pid>/cmdline)", cfd, note);
        if (cfd >= 0) close(cfd);
        snprintf(path, sizeof(path), "/proc/%d/status", pid);
        cfd = open(path, O_RDONLY | O_CLOEXEC);
        ok("open(/proc/<pid>/status)", cfd, note);
        if (cfd >= 0) close(cfd);
        break;
    }
    return 0;
}
