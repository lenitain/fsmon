// captest4.c — DECISIVE: with an unprivileged fanotify group, does the kernel
// still report the PID of the process that caused the event?
//
// fsmon's entire value proposition is process attribution, so if the pid comes
// back as 0 the unprivileged mode is useless regardless of everything else.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/fanotify.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/fanotify.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    printf("watcher uid=%d pid=%d\n", getuid(), getpid());

    int fd = fanotify_init(FAN_CLOEXEC | FAN_NONBLOCK | FAN_CLASS_NOTIF |
                               FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME,
                           O_RDONLY | O_CLOEXEC);
    if (fd < 0) { perror("fanotify_init"); return 1; }
    if (fanotify_mark(fd, FAN_MARK_ADD | FAN_MARK_ONLYDIR,
                      FAN_CREATE | FAN_CLOSE_WRITE, AT_FDCWD, "/tmp/fscap") < 0) {
        perror("fanotify_mark");
        return 1;
    }

    /* A DIFFERENT process makes the change — so the reported pid must be its
     * pid, not ours. Use a unique filename to identify the event. */
    char name[128];
    snprintf(name, sizeof(name), "/tmp/fscap/attr-%d.txt", getpid());
    pid_t writer = fork();
    if (writer == 0) {
        FILE *f = fopen(name, "w");
        if (f) { fputs("x\n", f); fclose(f); }
        _exit(0);
    }
    int st;
    waitpid(writer, &st, 0);
    printf("writer pid (expected in event) = %d\n", writer);

    int found = 0;
    for (int i = 0; i < 4 && found < 1; i++) {
        struct pollfd pfd = { .fd = fd, .events = POLLIN };
        if (poll(&pfd, 1, 1500) <= 0) break;
        char buf[16384] __attribute__((aligned(8)));
        ssize_t n = read(fd, buf, sizeof(buf));
        if (n <= 0) break;
        struct fanotify_event_metadata *md = (void *)buf;
        while (FAN_EVENT_OK(md, n)) {
            printf("  event mask=0x%llx  pid=%d  fd=%d  %s\n",
                   (unsigned long long)md->mask, md->pid, md->fd,
                   md->pid == writer     ? "<== CORRECT attribution"
                   : md->pid <= 0        ? "<== PID SUPPRESSED"
                                         : "<== wrong/other pid");
            if (md->fd >= 0) close(md->fd);
            if (md->pid == writer) found = 1;
            md = FAN_EVENT_NEXT(md, n);
        }
    }

    printf("\n%s\n", found
        ? "RESULT: unprivileged fanotify DOES report the causing pid."
        : "RESULT: attribution unavailable/incorrect when unprivileged.");
    return found ? 0 : 2;
}
