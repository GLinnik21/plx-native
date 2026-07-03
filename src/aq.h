/* aq.h — access-unit queue between the demux thread (producer) and the
 * buffer-feed loop (consumer). Format-independent: whatever demuxer runs (TS or
 * MKV) turns the HTTP byte stream into H264 Annex-B access units and calls
 * aq_push(); bufferfeed_pump() pops them and Feed()s them to the pipeline.
 *
 * One producer, one consumer. malloc per AU (only ~24 AUs/s, ~tens of KB each),
 * so no circular-buffer wrap logic and each AU is contiguous for Feed()'s
 * bufferAddr. Backpressure: aq_push blocks while queued bytes exceed AQ_MAX_BYTES,
 * pacing the download to playback. Included by main.c (single TU, all static). */
#ifndef PLEXPOC_AQ_H
#define PLEXPOC_AQ_H

#include <pthread.h>
#include <stdlib.h>
#include <string.h>

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

static void aq_init(au_queue *q) {
    memset(q, 0, sizeof *q);
    pthread_mutex_init(&q->m, NULL);
    pthread_cond_init(&q->not_full, NULL);
    pthread_cond_init(&q->not_empty, NULL);
}

/* Destroy the sync objects (call once the producer/consumer are done). Pairs with
 * aq_init so a re-init on the next playback isn't UB-on-an-initialized-mutex. */
static void aq_destroy(au_queue *q) {
    pthread_mutex_destroy(&q->m);
    pthread_cond_destroy(&q->not_full);
    pthread_cond_destroy(&q->not_empty);
}

/* Producer: append one AU (copies `len` bytes). Blocks while the queue is over
 * AQ_MAX_BYTES unless aborting. Returns 0 on success, -1 if aborting or OOM. */
static int aq_push(au_queue *q, const unsigned char *data, int len,
                   long long pts, int key, int es) {
    au_node *n = (au_node *)malloc(sizeof(au_node) + (size_t)len);
    if (!n) return -1;
    n->next = NULL; n->pts = pts; n->len = len; n->key = key; n->es = es;
    memcpy(n->data, data, (size_t)len);

    pthread_mutex_lock(&q->m);
    while (q->queued_bytes > AQ_MAX_BYTES && !q->abort)
        pthread_cond_wait(&q->not_full, &q->m);
    if (q->abort) { pthread_mutex_unlock(&q->m); free(n); return -1; }
    if (q->tail) q->tail->next = n; else q->head = n;
    q->tail = n;
    q->queued_bytes += len;
    pthread_cond_signal(&q->not_empty);
    pthread_mutex_unlock(&q->m);
    return 0;
}

/* Consumer: pop the next AU (caller frees it), or NULL if empty. Never blocks.
 * *eof_out is set to 1 when the producer has finished and the queue is drained. */
static au_node *aq_pop(au_queue *q, int *eof_out) {
    pthread_mutex_lock(&q->m);
    au_node *n = q->head;
    if (n) {
        q->head = n->next;
        if (!q->head) q->tail = NULL;
        q->queued_bytes -= n->len;
        pthread_cond_signal(&q->not_full);
    }
    if (eof_out) *eof_out = (!q->head && q->eof);
    pthread_mutex_unlock(&q->m);
    return n;
}

static void aq_set_eof(au_queue *q) {
    pthread_mutex_lock(&q->m);
    q->eof = 1;
    pthread_cond_signal(&q->not_empty);
    pthread_mutex_unlock(&q->m);
}

/* Ask the producer to stop blocking and bail (teardown). */
static void aq_abort(au_queue *q) {
    pthread_mutex_lock(&q->m);
    q->abort = 1;
    pthread_cond_broadcast(&q->not_full);
    pthread_cond_broadcast(&q->not_empty);
    pthread_mutex_unlock(&q->m);
}

static long aq_bytes(au_queue *q) {
    pthread_mutex_lock(&q->m);
    long b = q->queued_bytes;
    pthread_mutex_unlock(&q->m);
    return b;
}

#endif /* PLEXPOC_AQ_H */
