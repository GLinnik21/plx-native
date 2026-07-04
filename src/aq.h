/* aq.h — access-unit queue between the demux thread (producer) and the
 * buffer-feed loop (consumer). Format-independent: whatever demuxer runs (TS or
 * MKV) turns the HTTP byte stream into H264 Annex-B access units and calls
 * aq_push(); bufferfeed_pump() pops them and Feed()s them to the pipeline.
 *
 * One producer, one consumer. malloc per AU (only ~24 AUs/s, ~tens of KB each),
 * so no circular-buffer wrap logic and each AU is contiguous for Feed()'s
 * bufferAddr. Backpressure: aq_push blocks while queued bytes exceed AQ_MAX_BYTES,
 * pacing the download to playback. Implementation in aq.c. */
#ifndef PLEXPOC_AQ_H
#define PLEXPOC_AQ_H

#include <pthread.h>

#define AQ_MAX_BYTES (6 * 1024 * 1024)   /* ~3s @ 16 Mbps of queued video */

typedef struct au_node {
    struct au_node *next;
    long long pts;      /* ns */
    int len;
    int key;            /* 1 = contains IDR/keyframe */
    int es;             /* 1 = video (Annex-B), 2 = audio (raw frame) */
    unsigned char data[];
} au_node;

typedef struct {
    au_node *head, *tail;   /* FIFO */
    long queued_bytes;
    int  eof;               /* producer done (stream ended) */
    int  abort;             /* consumer/teardown asked producer to stop */
    pthread_mutex_t m;
    pthread_cond_t  not_full;   /* signalled when consumer drains below max */
    pthread_cond_t  not_empty;  /* signalled when producer adds / sets eof */
} au_queue;

void      aq_init(au_queue *q);
void      aq_destroy(au_queue *q);
int       aq_push(au_queue *q, const unsigned char *data, int len,
                  long long pts, int key, int es);
au_node  *aq_pop(au_queue *q, int *eof_out);
void      aq_set_eof(au_queue *q);
void      aq_abort(au_queue *q);
long      aq_bytes(au_queue *q);

#endif /* PLEXPOC_AQ_H */
