/* stream.h — minimal blocking HTTP/1.1 GET client over a raw TCP socket.
 * No libcurl / no DNS: PMS is a numeric IP:port on the LAN. Format-independent —
 * the demuxer (TS or MKV) reads bytes via http_read(); segmented HLS just opens
 * one http_stream per segment. Implementation in stream.c (relies on
 * -D_GNU_SOURCE for strcasestr). */
#ifndef PLEXPOC_STREAM_H
#define PLEXPOC_STREAM_H

typedef struct {
    int  fd;
    unsigned char buf[65536];   /* leftover-after-headers + read buffer */
    int  blen, bpos;            /* valid bytes in buf, current read cursor */
    long long content_length;   /* -1 if unknown (chunked/close-delimited) */
    long long consumed;         /* body bytes handed to caller so far */
    int  status;                /* HTTP status code, 0 on failure */
    int  chunked;               /* Transfer-Encoding: chunked */
    long long chunk_left;       /* bytes left in the current chunk (chunked mode) */
} http_stream;

/* Connect to ip:port and send GET path. Consumes response headers; leaves the
 * body ready for http_read(). Returns 0 on 2xx, -1 otherwise (fd closed).
 * extra may be NULL or extra request headers each ending in "\r\n". */
int  http_open(http_stream *hs, const char *ip, int port,
               const char *path, const char *extra);
/* Read up to n body bytes into dst. Returns >0 bytes, 0 at EOF, -1 on error.
 * Handles Content-Length / connection-close AND Transfer-Encoding: chunked. */
int  http_read(http_stream *hs, unsigned char *dst, int n);
void http_close(http_stream *hs);

#endif /* PLEXPOC_STREAM_H */
