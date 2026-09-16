/*
 * verify-fanotify-privsep.c — prove the core design assumption for
 * least-privilege fsmon.
 *
 * CLAIM UNDER TEST
 *   The kernel's "this fanotify group is unprivileged" flag (FANOTIFY_UNPRIV)
 *   is a property of the GROUP OBJECT, not of the process that reads it.
 *   Therefore a privileged component can create the group and hand the fd to
 *   an unprivileged long-running daemon, and the daemon still receives real
 *   PIDs for events caused by other processes.
 *
 *   See fs/notify/fanotify/fanotify_user.c:
 *     :1426   internal_flags |= FANOTIFY_UNPRIV   (only when !capable(CAP_SYS_ADMIN))
 *     :1497   group->fanotify_data.flags = flags | internal_flags
 *     :682    if (FAN_GROUP_FLAG(group, FANOTIFY_UNPRIV) &&
 *              task_tgid(current) != event->pid) metadata.pid = 0;
 *
 *   The pid gate reads the GROUP flag, and read() has no capability check,
 *   so an fd passed over SCM_RIGHTS should carry the privileged status.
 *
 * WHY THIS MATTERS
 *   If CONFIRMED -> fsmon needs CAP_SYS_ADMIN for one syscall at startup only;
 *               the daemon can run with zero capabilities (better than gsr,
 *               whose privileged helper must stay resident).
 *   If REFUTED -> the privileged component must itself read the event stream,
 *               i.e. a resident privileged broker is unavoidable.
 *
 * HOW TO RUN (needs root exactly once):
 *     gcc -O2 -o verify-fanotify-privsep verify-fanotify-privsep.c
 *     sudo ./verify-fanotify-privsep 1000
 *
 * EXIT: 0 = claim confirmed, 1 = claim refuted, 2 = setup error.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <linux/fanotify.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/fanotify.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#define TESTDIR "/tmp/fanotify-privsep-test"
#define POLL_BUDGET 10 /* × 500ms = 5s max */

static void show_caps(const char *who) {
    FILE *f = fopen("/proc/self/status", "r");
    char line[256];
    printf("  [%s] uid=%d euid=%d ", who, getuid(), geteuid());
    while (f && fgets(line, sizeof(line), f))
        if (!strncmp(line, "CapEff:", 7)) { printf("%s", line); break; }
    if (f) fclose(f);
}

/* Drop to the target uid/gid. Order matters: groups, then gid, then uid. */
static int drop_to(uid_t uid, gid_t gid) {
    if (setgroups(0, NULL) != 0) return -1;
    if (setgid(gid) != 0) return -1;
    if (setuid(uid) != 0) return -1;
    if (setuid(0) == 0) return -1; /* must NOT be able to regain root */
    return 0;
}

/* _exit() does NOT flush stdio buffers, so a piped run would silently lose
 * every diagnostic printed by the child. Always leave children through this. */
static void child_exit(int code) {
    fflush(NULL);
    _exit(code);
}

/* Create a file, reporting success. A silent failure here would fake a
 * REFUTED verdict, so the caller must always surface this. */
static int make_file(const char *name) {
    char path[256];
    snprintf(path, sizeof(path), TESTDIR "/%s", name);
    FILE *f = fopen(path, "w");
    if (!f) {
        fprintf(stderr, "  !! writer could not create %s: %s\n", path, strerror(errno));
        return -1;
    }
    fputs("payload\n", f);
    fclose(f);
    return 0;
}

int main(int argc, char **argv) {
    /* Keep output ordered and non-duplicated regardless of whether stdout
     * is a tty or a pipe. */
    setvbuf(stdout, NULL, _IOLBF, 0);

    if (geteuid() != 0) {
        fprintf(stderr, "error: must start as root (this is the privileged component)\n");
        return 2;
    }
    uid_t uid = argc > 1 ? (uid_t)atoi(argv[1]) : 1000;
    gid_t gid = uid;
    {
        char cmd[128];
        snprintf(cmd, sizeof(cmd), "id -g %u", (unsigned)uid);
        FILE *p = popen(cmd, "r");
        if (p) { unsigned g; if (fscanf(p, "%u", &g) == 1) gid = (gid_t)g; pclose(p); }
    }

    printf("=== fanotify privilege-separation verification ===\n");
    printf("target unprivileged uid=%u gid=%u\n", (unsigned)uid, (unsigned)gid);
    show_caps("privileged component (this process)");

    /* ---- 1. privileged component creates the group and the marks ---- */
    system("rm -rf " TESTDIR "; mkdir -p " TESTDIR);
    /* The unprivileged writer must be able to create files here, otherwise it
     * fails silently and the test reports a bogus REFUTED. */
    if (chown(TESTDIR, uid, gid) != 0 || chmod(TESTDIR, 0777) != 0) {
        perror("chown/chmod " TESTDIR);
        return 2;
    }

    int fan = fanotify_init(FAN_CLOEXEC | FAN_NONBLOCK | FAN_CLASS_NOTIF |
                                FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME,
                            O_RDONLY | O_CLOEXEC);
    if (fan < 0) { perror("fanotify_init"); return 2; }
    if (fanotify_mark(fan, FAN_MARK_ADD | FAN_MARK_ONLYDIR,
                      FAN_CREATE | FAN_CLOSE_WRITE, AT_FDCWD, TESTDIR) < 0) {
        perror("fanotify_mark"); return 2;
    }
    printf("\n[1] privileged group created on %s (fd %d)\n", TESTDIR, fan);

    /* Sanity: confirm this group really is privileged. FAN_UNLIMITED_MARKS is
     * in FANOTIFY_ADMIN_INIT_FLAGS, so it succeeds only with CAP_SYS_ADMIN. */
    {
        int probe = fanotify_init(FAN_CLOEXEC | FAN_CLASS_NOTIF | FAN_REPORT_FID |
                                      FAN_UNLIMITED_MARKS, O_RDONLY | O_CLOEXEC);
        printf("    group is privileged (FAN_UNLIMITED_MARKS accepted): %s\n",
               probe >= 0 ? "YES" : "NO -- aborting");
        if (probe >= 0) close(probe);
        if (probe < 0) return 2;
    }

    /* ---- 2. fork the unprivileged reader ("the daemon") ---- */
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0, sv) != 0) {
        perror("socketpair"); return 2;
    }

    fflush(NULL);
    pid_t reader = fork();
    if (reader == 0) {
        close(sv[0]);
        /* Close the inherited fanotify fd: the child must obtain it ONLY by
         * fd-passing, otherwise the test proves nothing. */
        close(fan);
        if (drop_to(uid, gid) != 0) { perror("drop_to"); child_exit(2); }
        show_caps("unprivileged reader (the daemon)");

        char cbuf[CMSG_SPACE(sizeof(int))];
        char dbuf[2 * sizeof(pid_t)];
        struct iovec io = { .iov_base = dbuf, .iov_len = sizeof(dbuf) };
        struct msghdr mh = { 0 };
        mh.msg_iov = &io; mh.msg_iovlen = 1;
        mh.msg_control = cbuf; mh.msg_controllen = sizeof(cbuf);
        if (recvmsg(sv[1], &mh, 0) <= 0) { perror("recvmsg"); child_exit(2); }

        int got_fd = -1;
        for (struct cmsghdr *c = CMSG_FIRSTHDR(&mh); c; c = CMSG_NXTHDR(&mh, c))
            if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS)
                memcpy(&got_fd, CMSG_DATA(c), sizeof(int));
        if (got_fd < 0) { fprintf(stderr, "no fd received\n"); child_exit(2); }

        pid_t want[2];
        memcpy(want, dbuf, sizeof(want));
        printf("    received fanotify fd %d\n"
               "    expecting: root-writer pid=%d, unpriv-writer pid=%d\n",
               got_fd, want[0], want[1]);

        /* ---- 3. read events as an unprivileged process ---- */
        int saw_root = 0, saw_unpriv = 0, saw_any = 0;
        for (int i = 0; i < POLL_BUDGET && !(saw_root && saw_unpriv); i++) {
            struct pollfd p = { .fd = got_fd, .events = POLLIN };
            if (poll(&p, 1, 500) <= 0) continue;
            char buf[16384] __attribute__((aligned(8)));
            ssize_t n = read(got_fd, buf, sizeof(buf));
            if (n <= 0) continue;
            struct fanotify_event_metadata *md = (void *)buf;
            while (FAN_EVENT_OK(md, n)) {
                const char *tag = md->pid == 0        ? "  <-- PID SUPPRESSED"
                                  : md->pid == want[0] ? "  <-- root-writer, CORRECT"
                                  : md->pid == want[1] ? "  <-- unpriv-writer, CORRECT"
                                                       : "  <-- (self/other)";
                printf("    reader sees: mask=0x%-4llx pid=%-7d%s\n",
                       (unsigned long long)md->mask, md->pid, tag);
                saw_any++;
                if (md->pid == want[0]) saw_root = 1;
                if (md->pid == want[1]) saw_unpriv = 1;
                if (md->fd >= 0) close(md->fd);
                md = FAN_EVENT_NEXT(md, n);
            }
        }
        if (!saw_any)
            printf("    reader saw NO events at all (writer failed?)\n");
        child_exit(saw_unpriv ? 0 : 1);
    }

    /* ---- parent: fork the unprivileged writer, then hand over the fd ---- */
    close(sv[1]);

    fflush(NULL);
    pid_t writer = fork();
    if (writer == 0) {
        close(sv[0]);
        close(fan);
        if (drop_to(uid, gid) != 0) child_exit(2);
        child_exit(make_file("written-by-unprivileged-writer.txt") == 0 ? 0 : 3);
    }

    /* Also cause an event as root, so we get a signal even if the unprivileged
     * writer has trouble. */
    make_file("written-by-root.txt");

    {
        pid_t want[2] = { getpid(), writer };
        char cbuf[CMSG_SPACE(sizeof(int))];
        struct iovec io = { .iov_base = want, .iov_len = sizeof(want) };
        struct msghdr mh = { 0 };
        mh.msg_iov = &io; mh.msg_iovlen = 1;
        mh.msg_control = cbuf; mh.msg_controllen = sizeof(cbuf);
        struct cmsghdr *c = CMSG_FIRSTHDR(&mh);
        c->cmsg_level = SOL_SOCKET; c->cmsg_type = SCM_RIGHTS;
        c->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(c), &fan, sizeof(int));
        if (sendmsg(sv[0], &mh, 0) < 0) { perror("sendmsg"); return 2; }
    }

    int st_w, st_r;
    waitpid(writer, &st_w, 0);
    waitpid(reader, &st_r, 0);

    printf("\n--- harness self-check (so a failure can't be blamed on the kernel) ---\n");
    printf("  unprivileged writer exit status : %d%s\n",
           WIFEXITED(st_w) ? WEXITSTATUS(st_w) : -1,
           (WIFEXITED(st_w) && WEXITSTATUS(st_w) == 0)
               ? "  (file created OK)"
               : "  <-- WRITER FAILED, result is not meaningful");
    printf("  files actually created          : ");
    fflush(stdout);
    system("ls -1 " TESTDIR " 2>/dev/null | tr '\\n' ' '; echo");

    printf("\n=== RESULT ===\n");
    int ok = WIFEXITED(st_r) && WEXITSTATUS(st_r) == 0;
    if (ok) {
        printf("CONFIRMED: an unprivileged reader holding only a passed-in fanotify\n"
               "           fd received the TRUE pid of an event caused by ANOTHER\n"
               "           process (the unprivileged writer).\n\n"
               "=> fsmon needs CAP_SYS_ADMIN for fanotify_init() only. After the fds\n"
               "   exist, the daemon can drop every capability and still do full\n"
               "   process attribution. No resident privileged helper required.\n");
    } else {
        printf("REFUTED / INCONCLUSIVE: the unprivileged reader did not receive the\n"
               "         real pid of the unprivileged writer's event.\n"
               "         Check the harness self-check above first -- if the writer\n"
               "         failed, this run proves nothing about the kernel.\n");
    }
    printf("\ncleanup: rm -rf %s\n", TESTDIR);
    return ok ? 0 : 1;
}
