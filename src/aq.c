/* aq.c — one-producer/one-consumer access-unit FIFO with byte-cap backpressure. */
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include "aq.h"

void aq_init(au_queue *q) {
    memset(q, 0, sizeof *q);
    pthread_mutex_init(&q->m, NULL);
    pthread_cond_init(&q->not_full, NULL);
    pthread_cond_init(&q->not_empty, NULL);
}

/* Destroy the sync objects (call once the producer/consumer are done). Pairs with
 * aq_init so a re-init on the next playback isn't UB-on-an-initialized-mutex. */
void aq_destroy(au_queue *q) {
    pthread_mutex_destroy(&q->m);
    pthread_cond_destroy(&q->not_full);
    pthread_cond_destroy(&q->not_empty);
}

/* Producer: append one AU (copies `len` bytes). Blocks while the queue is over
 * AQ_MAX_BYTES unless aborting. Returns 0 on success, -1 if aborting or OOM. */
int aq_push(au_queue *q, const unsigned char *data, int len,
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
au_node *aq_pop(au_queue *q, int *eof_out) {
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

void aq_set_eof(au_queue *q) {
    pthread_mutex_lock(&q->m);
    q->eof = 1;
    pthread_cond_signal(&q->not_empty);
    pthread_mutex_unlock(&q->m);
}

/* Ask the producer to stop blocking and bail (teardown). */
void aq_abort(au_queue *q) {
    pthread_mutex_lock(&q->m);
    q->abort = 1;
    pthread_cond_broadcast(&q->not_full);
    pthread_cond_broadcast(&q->not_empty);
    pthread_mutex_unlock(&q->m);
}

long aq_bytes(au_queue *q) {
    pthread_mutex_lock(&q->m);
    long b = q->queued_bytes;
    pthread_mutex_unlock(&q->m);
    return b;
}
