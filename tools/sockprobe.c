/* sockprobe — the socket semantics the HOST test suite cannot answer.
 *
 * `cargo test` runs on macOS; the app runs on this TV's Linux 3.10/glibc 2.12 ARM. On the two
 * questions that decide where `stream.rs::http_open` publishes its fd, the platforms DISAGREE —
 * verified, not assumed: on Darwin, `shutdown(2)` on a socket still mid-handshake returns 0 AND
 * makes `connect_timeout` report SUCCESS on a socket that never connected. A host test asserting
 * Linux behaviour here would be worse than no test.
 *
 * So this asks the target directly:
 *
 *   A. Can shutdown(2) abort a handshake in progress?
 *      If YES, publishing the fd at socket() (as the async plan called for) buys an interruptible
 *      connect. If NO, the connect window is uninterruptible anyway, it is already bounded by
 *      CONNECT_TIMEOUT_MS = 2000, and the fd should be published AFTER connect — which removes
 *      the whole "a dead descriptor is armed in the token" failure class that made the first
 *      attempt at this dangerous.
 *
 *   B. Does shutdown(2) wake a peer already blocked in recv(2)?
 *      The entire single-closer protocol rests on this. It is asserted on the host by
 *      `shutdown_wakes_a_reader_that_is_already_blocked_in_recv`; this confirms the same on the
 *      kernel that ships.
 *
 * Build (host):  make sockprobe
 * Run (TV):      ./sockprobe          (no arguments, no app state touched, nothing written)
 *
 * MEASURED on the LG 49SM9000PLA (webOS 4.5, 2026-07-28):
 *
 *   A. shutdown -> rv=0, and connect_timeout(1200ms) returned -1 after 200ms.
 *      => it DOES abort a handshake in progress. Not what Linux is documented to do (ENOTCONN),
 *         and the opposite of what I assumed before running this. So `http_open` publishes at
 *         socket(), and the whole open — handshake included — is interruptible.
 *   B. recv woken at 200ms reporting EOF.
 *      => the single-closer interrupt protocol holds on this kernel too.
 *
 * The Darwin contrast that made this probe necessary is worth keeping: there, the same shutdown
 * returns 0 AND makes connect_timeout report SUCCESS on a socket that never connected. A host
 * test asserting either platform's behaviour would have been actively misleading.
 */
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

static long now_ms(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1000 + t.tv_nsec / 1000000;
}

static struct sockaddr_in addr_of(const char *ip, unsigned short port)
{
    struct sockaddr_in sa;
    memset(&sa, 0, sizeof sa);
    sa.sin_family = AF_INET;
    sa.sin_port = htons(port);
    sa.sin_addr.s_addr = inet_addr(ip);
    return sa;
}

/* Mirrors stream.rs::connect_timeout exactly: non-blocking connect, poll(POLLOUT), then SO_ERROR
 * for the real verdict. Testing anything else would be testing the wrong code. */
static int connect_timeout(int fd, const struct sockaddr_in *sa, int timeout_ms)
{
    int flags = fcntl(fd, F_GETFL, 0);
    struct pollfd pfd;
    int err = 0, r;
    socklen_t elen = sizeof err;

    if (flags < 0 || fcntl(fd, F_SETFL, flags | O_NONBLOCK) < 0) {
        return -1;
    }
    if (connect(fd, (const struct sockaddr *)sa, sizeof *sa) == 0) {
        fcntl(fd, F_SETFL, flags);
        return 0;
    }
    if (errno != EINPROGRESS) {
        fcntl(fd, F_SETFL, flags);
        return -1;
    }
    pfd.fd = fd;
    pfd.events = POLLOUT;
    pfd.revents = 0;
    r = poll(&pfd, 1, timeout_ms);
    fcntl(fd, F_SETFL, flags);
    if (r <= 0) {
        return -1; /* timed out or failed */
    }
    if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &err, &elen) < 0 || err != 0) {
        return -1;
    }
    return 0;
}

struct shot {
    int fd;
    int delay_ms;
    int rv;
    int err;
};

static void *shooter(void *arg)
{
    struct shot *s = arg;
    struct timespec ts = { s->delay_ms / 1000, (long)(s->delay_ms % 1000) * 1000000L };
    nanosleep(&ts, NULL);
    errno = 0;
    s->rv = shutdown(s->fd, SHUT_RDWR);
    s->err = errno;
    return NULL;
}

/* A: shutdown() against a handshake that can never complete (TEST-NET-1, RFC 5737). */
static void probe_connect_interrupt(void)
{
    struct sockaddr_in sa = addr_of("192.0.2.1", 80);
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    struct shot s = { fd, 200, 0, 0 };
    pthread_t th;
    long t0, waited;
    int rv;

    printf("A. can shutdown(2) abort a connect in progress?\n");
    if (fd < 0) {
        printf("   socket() failed: %s\n", strerror(errno));
        return;
    }
    pthread_create(&th, NULL, shooter, &s);
    t0 = now_ms();
    rv = connect_timeout(fd, &sa, 1200);
    waited = now_ms() - t0;
    pthread_join(th, NULL);
    close(fd);

    printf("   shutdown -> rv=%d errno=%d (%s)\n", s.rv, s.err, s.rv ? strerror(s.err) : "-");
    printf("   connect_timeout(1200ms) -> rv=%d after %ldms\n", rv, waited);
    if (waited < 1000) {
        printf("   => ABORTED the handshake. Publishing the fd before connect WOULD buy an\n"
               "      interruptible connect — revisit the http_open design note.\n");
    } else {
        printf("   => NOT abortable: the handshake ran its full deadline. The connect window is\n"
               "      uninterruptible and already bounded, so publish the fd AFTER connect.\n");
    }
    if (rv == 0) {
        printf("   !! and connect_timeout reported SUCCESS on a socket that never connected.\n");
    }
}

/* B: shutdown() against a peer already parked in recv(). */
static void probe_recv_wakeup(void)
{
    struct sockaddr_in any = addr_of("127.0.0.1", 0), peer;
    socklen_t plen = sizeof peer;
    int srv = socket(AF_INET, SOCK_STREAM, 0);
    int cli, acc, rv;
    char b[16];
    long t0, waited;
    struct shot s;
    pthread_t th;

    printf("\nB. does shutdown(2) wake a peer already blocked in recv(2)?\n");
    if (srv < 0 || bind(srv, (struct sockaddr *)&any, sizeof any) != 0 || listen(srv, 1) != 0) {
        printf("   loopback setup failed: %s\n", strerror(errno));
        return;
    }
    getsockname(srv, (struct sockaddr *)&peer, &plen);
    cli = socket(AF_INET, SOCK_STREAM, 0);
    peer.sin_addr.s_addr = inet_addr("127.0.0.1");
    if (connect_timeout(cli, &peer, 2000) != 0) {
        printf("   loopback connect failed\n");
        return;
    }
    acc = accept(srv, NULL, NULL); /* held open; nothing is ever sent on it */

    s.fd = cli;
    s.delay_ms = 200;
    pthread_create(&th, NULL, shooter, &s);
    t0 = now_ms();
    rv = (int)recv(cli, b, sizeof b, 0); /* parks here until the shutdown lands */
    waited = now_ms() - t0;
    pthread_join(th, NULL);
    close(acc);
    close(cli);
    close(srv);

    printf("   recv -> rv=%d after %ldms (shutdown fired at ~200ms)\n", rv, waited);
    printf("   => %s\n", (rv == 0 && waited < 2000)
           ? "woken, reports EOF. The single-closer interrupt protocol holds here."
           : "NOT woken as expected — http_shutdown cannot interrupt a reader on this kernel.");
}

int main(void)
{
    printf("sockprobe: uname/kernel semantics for stream.rs::http_open\n\n");
    probe_connect_interrupt();
    probe_recv_wakeup();
    return 0;
}
