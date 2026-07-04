/* stream.c — blocking HTTP/1.1 GET over a raw TCP socket (see stream.h).
 * Relies on -D_GNU_SOURCE (Makefile CFLAGS) for strcasestr. */
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <errno.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include "stream.h"

int http_open(http_stream *hs, const char *ip, int port,
              const char *path, const char *extra) {
    memset(hs, 0, sizeof *hs);
    hs->fd = -1;
    hs->content_length = -1;

    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    struct sockaddr_in sa;
    memset(&sa, 0, sizeof sa);
    sa.sin_family = AF_INET;
    sa.sin_port = htons((unsigned short)port);
    if (inet_aton(ip, &sa.sin_addr) == 0) { close(fd); return -1; }
    if (connect(fd, (struct sockaddr *)&sa, sizeof sa) < 0) { close(fd); return -1; }
    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
    /* bound a stalled recv: a blocked read can't be woken by close() from another
     * thread, so cap it so teardown (pthread_join) can never hang indefinitely */
    struct timeval rcvto = { 15, 0 };
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &rcvto, sizeof rcvto);

    /* Send the default wildcard Accept ONLY if the caller didn't set one. The
     * Plex API does content negotiation: a wildcard Accept yields XML, but pms.h's
     * parser needs JSON, so it passes an explicit "Accept: application/json" that
     * the wildcard would otherwise override. Playback/part/photo endpoints ignore
     * Accept and keep the default. */
    int has_accept = extra && strcasestr(extra, "Accept:");
    char req[2048];
    int n = snprintf(req, sizeof req,
                     "GET %s HTTP/1.1\r\n"
                     "Host: %s:%d\r\n"
                     "User-Agent: plexpoc/0.1\r\n"
                     "%s"
                     "%s"
                     "Connection: close\r\n\r\n",
                     path, ip, port,
                     has_accept ? "" : "Accept: */*\r\n",
                     extra ? extra : "");
    for (int off = 0; off < n; ) {
        int w = (int)send(fd, req + off, n - off, 0);
        if (w <= 0) { close(fd); return -1; }
        off += w;
    }

    /* read until end of headers (\r\n\r\n), keeping any body bytes that follow */
    int hdr_end = -1;
    hs->blen = 0;
    while (hdr_end < 0 && hs->blen < (int)sizeof hs->buf - 1) {
        int r = (int)recv(fd, hs->buf + hs->blen, sizeof hs->buf - hs->blen, 0);
        if (r <= 0) { close(fd); return -1; }
        hs->blen += r;
        for (int i = 3; i < hs->blen; i++) {
            if (hs->buf[i-3]=='\r' && hs->buf[i-2]=='\n' &&
                hs->buf[i-1]=='\r' && hs->buf[i]=='\n') { hdr_end = i + 1; break; }
        }
    }
    if (hdr_end < 0) { close(fd); return -1; }

    /* parse status line + a couple of headers (headers are ASCII up to hdr_end) */
    hs->buf[hdr_end < (int)sizeof hs->buf ? hdr_end : (int)sizeof hs->buf - 1] = hs->buf[hdr_end]; /* no-op guard */
    {
        char save = 0;
        /* temporarily NUL-terminate the header block for strstr scans */
        if (hdr_end < (int)sizeof hs->buf) { save = (char)hs->buf[hdr_end]; hs->buf[hdr_end] = 0; }
        const char *h = (const char *)hs->buf;
        if (strncmp(h, "HTTP/1.", 7) == 0) hs->status = atoi(h + 9);
        const char *cl = strcasestr(h, "\r\nContent-Length:");
        if (cl) hs->content_length = strtoll(cl + 17, NULL, 10);
        if (strcasestr(h, "\r\nTransfer-Encoding: chunked")) hs->chunked = 1;
        if (hdr_end < (int)sizeof hs->buf) hs->buf[hdr_end] = (unsigned char)save;
    }

    hs->fd   = fd;
    hs->bpos = hdr_end;   /* first body byte */
    /* blen already includes any body bytes read alongside the headers */

    if (hs->status < 200 || hs->status >= 300) { close(fd); hs->fd = -1; return -1; }
    return 0;
}

/* one raw body byte from the buffered-then-socket stream (for chunk framing) */
static int hs_getb(http_stream *hs, unsigned char *b) {
    if (hs->bpos < hs->blen) { *b = hs->buf[hs->bpos++]; return 1; }
    if (hs->fd < 0) return 0;
    int r = (int)recv(hs->fd, b, 1, 0);
    if (r == 1) return 1;
    if (r == 0) { close(hs->fd); hs->fd = -1; }
    return 0;
}
/* read the next chunk-size line (skips the CRLF trailing the previous chunk and any
 * chunk extensions/whitespace). Returns the chunk data size (0 = last chunk), or -1. */
static long long hs_next_chunk(http_stream *hs) {
    unsigned char b; long long sz = 0; int any = 0;
    do { if (!hs_getb(hs, &b)) return -1; } while (b == '\r' || b == '\n');
    for (;;) {
        int d;
        if (b >= '0' && b <= '9') d = b - '0';
        else if (b >= 'a' && b <= 'f') d = b - 'a' + 10;
        else if (b >= 'A' && b <= 'F') d = b - 'A' + 10;
        else break;
        sz = sz * 16 + d; any = 1;
        if (!hs_getb(hs, &b)) return any ? sz : -1;
    }
    while (b != '\n') { if (!hs_getb(hs, &b)) break; }   /* consume extensions + CRLF */
    return any ? sz : -1;
}

int http_read(http_stream *hs, unsigned char *dst, int n) {
    if (hs->chunked) {
        if (hs->chunk_left <= 0) {
            long long cs = hs_next_chunk(hs);
            if (cs <= 0) { if (hs->fd >= 0) { close(hs->fd); hs->fd = -1; } return 0; }
            hs->chunk_left = cs;
        }
        int want = ((long long)n < hs->chunk_left) ? n : (int)hs->chunk_left;
        int got = 0;
        while (got < want) {
            if (hs->bpos < hs->blen) {
                int avail = hs->blen - hs->bpos;
                int take = (want - got) < avail ? (want - got) : avail;
                memcpy(dst + got, hs->buf + hs->bpos, take); hs->bpos += take; got += take;
            } else if (hs->fd >= 0) {
                int r = (int)recv(hs->fd, dst + got, want - got, 0);
                if (r < 0) { if (errno == EINTR) continue; break; }
                if (r == 0) { close(hs->fd); hs->fd = -1; break; }
                got += r;
            } else break;
        }
        hs->chunk_left -= got;
        hs->consumed += got;
        return got > 0 ? got : (hs->fd < 0 ? 0 : -1);
    }
    if (hs->fd < 0 && hs->bpos >= hs->blen) return 0;
    if (hs->content_length >= 0 && hs->consumed >= hs->content_length) return 0;

    /* serve buffered body first */
    if (hs->bpos < hs->blen) {
        int avail = hs->blen - hs->bpos;
        int take = avail < n ? avail : n;
        memcpy(dst, hs->buf + hs->bpos, take);
        hs->bpos += take;
        hs->consumed += take;
        return take;
    }
    if (hs->fd < 0) return 0;
    int r = (int)recv(hs->fd, dst, n, 0);
    if (r < 0) return (errno == EINTR) ? http_read(hs, dst, n) : -1;
    if (r == 0) { close(hs->fd); hs->fd = -1; return 0; }
    hs->consumed += r;
    return r;
}

void http_close(http_stream *hs) {
    if (hs->fd >= 0) { close(hs->fd); hs->fd = -1; }
}
