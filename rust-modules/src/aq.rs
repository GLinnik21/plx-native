//! One-producer / one-consumer access-unit FIFO with byte-cap backpressure (was
//! src/aq.c). The player consumer allocates an `AuQueue` and passes `&q`; `aq_pop`
//! hands back a malloc'd `AuNode` the caller reads and frees. AuNode uses a flexible
//! array member (`data[]`); AuQueue embeds pthread objects — both repr(C) so the
//! layouts match glibc.
use std::os::raw::{c_int, c_long, c_uchar, c_void};
use std::ptr;

#[repr(C)]
pub struct AuNode {
    next: *mut AuNode,
    pts: i64,
    len: c_int,
    key: c_int,
    es: c_int,
    // flexible array `data[]` follows immediately after `es` (see node_data)
}

#[repr(C)]
pub struct AuQueue {
    head: *mut AuNode,
    tail: *mut AuNode,
    queued_bytes: c_long,
    max_bytes: c_long, // per-queue backpressure cap; the caller chooses it (see aq_new)
    eof: c_int,
    abort: c_int,
    m: libc::pthread_mutex_t,
    not_full: libc::pthread_cond_t,
    not_empty: libc::pthread_cond_t,
}

/// pointer to the au_node's flexible `data[]` (offset = right after `es`)
#[inline]
unsafe fn node_data(n: *mut AuNode) -> *mut u8 {
    (n as *mut u8).add(core::mem::offset_of!(AuNode, es) + core::mem::size_of::<c_int>())
}

/// crate-internal: has the consumer asked the producer to stop? (the demuxer checks this)
#[inline]
pub(crate) unsafe fn aq_is_aborted(q: *const AuQueue) -> bool {
    !q.is_null() && (*q).abort != 0
}

/// crate-internal: read a popped node's fields (es, key, pts, len, data ptr) —
/// the player engine's feed loop needs them without re-exposing the raw offsets.
#[inline]
pub(crate) unsafe fn au_fields(n: *mut AuNode) -> (c_int, c_int, i64, c_int, *const u8) {
    ((*n).es, (*n).key, (*n).pts, (*n).len, node_data(n) as *const u8)
}

/// allocate + initialize a boxed queue with an explicit backpressure byte cap (mirrors
/// `stream::http_stream_boxed`; the two-lane feed gives each ES its own cap).
pub(crate) fn aq_new(cap: c_long) -> Box<AuQueue> {
    let mut q: Box<AuQueue> = Box::new(unsafe { std::mem::zeroed() });
    aq_init_cap(&mut *q, cap);
    q
}

/// initialize an already-allocated queue with an explicit backpressure byte cap.
fn aq_init_cap(q: *mut AuQueue, cap: c_long) {
    if q.is_null() { return; }
    unsafe {
        ptr::write_bytes(q as *mut u8, 0, core::mem::size_of::<AuQueue>());
        (*q).max_bytes = cap;
        libc::pthread_mutex_init(ptr::addr_of_mut!((*q).m), ptr::null());
        libc::pthread_cond_init(ptr::addr_of_mut!((*q).not_full), ptr::null());
        libc::pthread_cond_init(ptr::addr_of_mut!((*q).not_empty), ptr::null());
    }
}

pub(crate) fn aq_destroy(q: *mut AuQueue) {
    if q.is_null() { return; }
    unsafe {
        libc::pthread_mutex_destroy(ptr::addr_of_mut!((*q).m));
        libc::pthread_cond_destroy(ptr::addr_of_mut!((*q).not_full));
        libc::pthread_cond_destroy(ptr::addr_of_mut!((*q).not_empty));
    }
}

/// Producer: append one AU (copies `len` bytes). Blocks over the queue's `max_bytes` cap
/// unless aborting. Returns 0 on success, -1 if aborting or OOM.
pub(crate) fn aq_push(q: *mut AuQueue, data: *const c_uchar, len: c_int,
                          pts: i64, key: c_int, es: c_int) -> c_int {
    if q.is_null() || len < 0 { return -1; }
    unsafe {
        let n = libc::malloc(core::mem::size_of::<AuNode>() + len as usize) as *mut AuNode;
        if n.is_null() { return -1; }
        (*n).next = ptr::null_mut();
        (*n).pts = pts;
        (*n).len = len;
        (*n).key = key;
        (*n).es = es;
        if len > 0 && !data.is_null() {
            ptr::copy_nonoverlapping(data, node_data(n), len as usize);
        }
        libc::pthread_mutex_lock(ptr::addr_of_mut!((*q).m));
        while (*q).queued_bytes > (*q).max_bytes && (*q).abort == 0 {
            libc::pthread_cond_wait(ptr::addr_of_mut!((*q).not_full), ptr::addr_of_mut!((*q).m));
        }
        if (*q).abort != 0 {
            libc::pthread_mutex_unlock(ptr::addr_of_mut!((*q).m));
            libc::free(n as *mut c_void);
            return -1;
        }
        if !(*q).tail.is_null() {
            (*(*q).tail).next = n;
        } else {
            (*q).head = n;
        }
        (*q).tail = n;
        (*q).queued_bytes += len as c_long;
        libc::pthread_cond_signal(ptr::addr_of_mut!((*q).not_empty));
        libc::pthread_mutex_unlock(ptr::addr_of_mut!((*q).m));
        0
    }
}

/// Consumer: pop the next AU (caller frees it) or NULL. Never blocks.
/// *eof_out = 1 when the producer finished and the queue is drained.
pub(crate) fn aq_pop(q: *mut AuQueue, eof_out: *mut c_int) -> *mut AuNode {
    if q.is_null() { return ptr::null_mut(); }
    unsafe {
        libc::pthread_mutex_lock(ptr::addr_of_mut!((*q).m));
        let n = (*q).head;
        if !n.is_null() {
            (*q).head = (*n).next;
            if (*q).head.is_null() {
                (*q).tail = ptr::null_mut();
            }
            (*q).queued_bytes -= (*n).len as c_long;
            libc::pthread_cond_signal(ptr::addr_of_mut!((*q).not_full));
        }
        if !eof_out.is_null() {
            *eof_out = if (*q).head.is_null() && (*q).eof != 0 { 1 } else { 0 };
        }
        libc::pthread_mutex_unlock(ptr::addr_of_mut!((*q).m));
        n
    }
}

pub(crate) fn aq_set_eof(q: *mut AuQueue) {
    if q.is_null() { return; }
    unsafe {
        libc::pthread_mutex_lock(ptr::addr_of_mut!((*q).m));
        (*q).eof = 1;
        libc::pthread_cond_signal(ptr::addr_of_mut!((*q).not_empty));
        libc::pthread_mutex_unlock(ptr::addr_of_mut!((*q).m));
    }
}

pub(crate) fn aq_abort(q: *mut AuQueue) {
    if q.is_null() { return; }
    unsafe {
        libc::pthread_mutex_lock(ptr::addr_of_mut!((*q).m));
        (*q).abort = 1;
        libc::pthread_cond_broadcast(ptr::addr_of_mut!((*q).not_full));
        libc::pthread_cond_broadcast(ptr::addr_of_mut!((*q).not_empty));
        libc::pthread_mutex_unlock(ptr::addr_of_mut!((*q).m));
    }
}

pub(crate) fn aq_bytes(q: *mut AuQueue) -> c_long {
    if q.is_null() { return 0; }
    unsafe {
        libc::pthread_mutex_lock(ptr::addr_of_mut!((*q).m));
        let b = (*q).queued_bytes;
        libc::pthread_mutex_unlock(ptr::addr_of_mut!((*q).m));
        b
    }
}
