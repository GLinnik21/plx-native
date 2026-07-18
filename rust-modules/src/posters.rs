//! Rust port of src/posters.c — async poster/artwork texture store (posters.h).
//! Same C ABI. Rewritten on std::sync instead of the C's pthread globals: a
//! Mutex<Store> + Condvar + std::thread workers. The decoded-pixel pointer is
//! stored as an address (usize) so the shared Store stays Send. GL calls
//! (upload/delete) run only on the main thread (poster_pump / poster_get);
//! workers only fetch (Rust http_get) + decode (Rust img) off the lock.
use crate::{img, stream};
use std::os::raw::{c_char, c_int, c_uchar, c_uint};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

// Per-frame GL-upload counters for the frame-drop detector (app.rs): each poster_pump upload is a
// synchronous glTexImage2D on the main thread — the prime suspect for scroll judder. `take_upload_stats`
// reads-and-resets (call once per frame).
static UP_CT: AtomicU32 = AtomicU32::new(0);
static UP_PX: AtomicU64 = AtomicU64::new(0);
/// (uploads, total pixels) since the last call; resets both. Main-thread, once per frame.
pub(crate) fn take_upload_stats() -> (u32, u64) {
    (UP_CT.swap(0, Ordering::Relaxed), UP_PX.swap(0, Ordering::Relaxed))
}

const PT_CAP: usize = 64;
const P_EMPTY: c_int = 0;
const P_WANT: c_int = 1;
const P_LOADING: c_int = 2;
const P_DECODED: c_int = 3;
const P_UPLOADING: c_int = 4;
const P_READY: c_int = 5;
const P_FAILED: c_int = 6;

#[derive(Clone, Copy)]
struct Pslot {
    key: [u8; 256], // full /photo/:/transcode request path = store key
    tex: c_uint,
    pw: c_int,
    ph: c_int,
    px: usize, // decoded RGBA ptr as address (0 = none) — keeps Pslot Send
    state: c_int,
    use_: c_uint, // LRU clock
    gen: c_uint,  // bumped on eviction; stale-decode guard
    frame: c_uint, // last frame poster_get touched it (evict-protect)
}
impl Pslot {
    const ZERO: Pslot = Pslot { key: [0; 256], tex: 0, pw: 0, ph: 0, px: 0, state: P_EMPTY, use_: 0, gen: 0, frame: 0 };
}

struct Store {
    slots: [Pslot; PT_CAP],
    clock: c_uint,
    frame: c_uint,
    quit: bool,
    workers: Vec<JoinHandle<()>>,
}
impl Store {
    const fn new() -> Store {
        Store {
            slots: [Pslot::ZERO; PT_CAP],
            clock: 0, frame: 0, quit: false,
            workers: Vec::new(),
        }
    }
}

static STORE: Mutex<Store> = Mutex::new(Store::new());
static CV: Condvar = Condvar::new();

fn store() -> MutexGuard<'static, Store> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
}

unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

/// delete a poster texture on the GL/main thread (gfx owns the GL bindings)
fn gl_delete(t: c_uint) {
    crate::gfx::delete_tex(t);
}

fn key_bytes(s: &Pslot) -> &[u8] {
    crate::cbuf::as_bytes(&s.key)
}
fn set_key(s: &mut Pslot, key: &str) {
    crate::cbuf::set_bytes(&mut s.key, key);
}

// (path, w, h, png) → built transcode path, memoised. resolve_tex re-derives the key for every
// visible art tile every frame (~25-40 tiles × 60fps); the QueryBuilder + token clone behind
// image_transcode_path was ~15k heap allocs/sec of steady-state waste for keys that never change.
// MAIN-thread only (all poster_key callers are draw paths). Invalidated when the client token
// changes (profile switch — the token is baked into the path).
struct KeyMemo {
    token_gen: u32,
    map: std::collections::HashMap<(String, c_int, c_int, bool), String>,
}
static mut KEY_MEMO: Option<KeyMemo> = None;

/// Build the transcode request path (also the store key). png=1 -> transparent clearLogo.
/// The path (and token) come from the typed client's `image_transcode_path` — the one place
/// that assembles a /photo/:/transcode request — so this stays the LRU key AND the fetch path.
pub(crate) fn poster_key(dst: *mut c_char, cap: usize, src_path: *const c_char, w: c_int, h: c_int, png: c_int) {
    if dst.is_null() || cap == 0 {
        return;
    }
    unsafe {
        let memo = (*std::ptr::addr_of_mut!(KEY_MEMO)).get_or_insert_with(|| KeyMemo {
            token_gen: crate::plex::client().token_gen(),
            map: std::collections::HashMap::new(),
        });
        let gen = crate::plex::client().token_gen();
        if memo.token_gen != gen || memo.map.len() > 1024 {
            memo.token_gen = gen;
            memo.map.clear();
        }
        let k = (cstr(src_path), w, h, png != 0);
        let s = memo
            .map
            .entry(k)
            .or_insert_with_key(|k| crate::plex::client().image_transcode_path(&k.0, w as i64, h as i64, png != 0));
        let out = std::slice::from_raw_parts_mut(dst as *mut u8, cap);
        let b = s.as_bytes();
        let n = b.len().min(cap - 1);
        out[..n].copy_from_slice(&b[..n]);
        out[n] = 0;
    }
}

/// Resolve an item's clearLogo (transparent PNG) to a GL texture, aspect-fit into `max_w`×`max_h`.
/// `Some((tex, w, h))` once loaded; `None` while pending or when the item has no logo. The ONE
/// clearLogo resolve (home hero, detail hero, detail compact title all draw through it).
pub(crate) fn logo_tex(rk: &str, max_w: f32, max_h: f32) -> Option<(c_uint, f32, f32)> {
    if rk.is_empty() {
        return None;
    }
    let lpath = std::ffi::CString::new(format!("/library/metadata/{rk}/clearLogo")).ok()?;
    let mut key = [0u8; 352];
    poster_key(key.as_mut_ptr() as *mut c_char, key.len(), lpath.as_ptr(), 600, 240, 1);
    let tex = poster_get(key.as_ptr() as *const c_char);
    let (mut lw, mut lh) = (0i32, 0i32);
    poster_wh(key.as_ptr() as *const c_char, &mut lw, &mut lh);
    if tex == 0 || lw <= 0 || lh <= 0 {
        return None;
    }
    let s = (max_h / lh as f32).min(max_w / lw as f32);
    Some((tex, lw as f32 * s, lh as f32 * s))
}

/// MAIN thread. READY texture for key, else 0 (claim a slot + enqueue fetch on miss).
pub(crate) fn poster_get(key: *const c_char) -> c_uint {
    if key.is_null() {
        return 0;
    }
    let key_s = unsafe { cstr(key) };
    let mut g = store();
    // hit?
    for i in 0..PT_CAP {
        if g.slots[i].state != P_EMPTY && key_bytes(&g.slots[i]) == key_s.as_bytes() {
            g.clock = g.clock.wrapping_add(1);
            let (c, f) = (g.clock, g.frame);
            g.slots[i].use_ = c;
            g.slots[i].frame = f;
            return if g.slots[i].state == P_READY { g.slots[i].tex } else { 0 };
        }
    }
    // miss: prefer EMPTY, else LRU-evict a settled slot not used this frame
    let mut slot = None;
    for i in 0..PT_CAP {
        if g.slots[i].state == P_EMPTY {
            slot = Some(i);
            break;
        }
    }
    if slot.is_none() {
        let mut oldest = c_uint::MAX;
        for i in 0..PT_CAP {
            let s = &g.slots[i];
            if (s.state == P_READY || s.state == P_FAILED) && s.frame != g.frame && s.use_ < oldest {
                oldest = s.use_;
                slot = Some(i);
            }
        }
    }
    let idx = match slot {
        Some(i) => i,
        None => return 0, // all visible: skip
    };
    let (old_tex, old_px) = (g.slots[idx].tex, g.slots[idx].px);
    g.clock = g.clock.wrapping_add(1);
    let (c, f) = (g.clock, g.frame);
    {
        let s = &mut g.slots[idx];
        s.tex = 0;
        s.px = 0;
        s.gen = s.gen.wrapping_add(1);
        set_key(s, &key_s);
        s.state = P_WANT;
        s.use_ = c;
        s.frame = f;
        s.pw = 0;
        s.ph = 0;
    }
    drop(g);
    // free the evicted resources off-lock (poster_get is the GL/main thread)
    if old_tex != 0 {
        gl_delete(old_tex);
    }
    if old_px != 0 {
        img::img_free(old_px as *mut c_uchar);
    }
    CV.notify_one();
    0
}

/// pixel dims of a READY texture for key (0,0 if not ready). Does NOT trigger a fetch.
pub(crate) fn poster_wh(key: *const c_char, w: *mut c_int, h: *mut c_int) {
    unsafe {
        if !w.is_null() {
            *w = 0;
        }
        if !h.is_null() {
            *h = 0;
        }
        if key.is_null() {
            return;
        }
        let key_s = cstr(key);
        let g = store();
        for i in 0..PT_CAP {
            if g.slots[i].state == P_READY && key_bytes(&g.slots[i]) == key_s.as_bytes() {
                if !w.is_null() {
                    *w = g.slots[i].pw;
                }
                if !h.is_null() {
                    *h = g.slots[i].ph;
                }
                break;
            }
        }
    }
}

/// MAIN/GL thread, once per frame BEFORE drawing. Uploads up to `budget` decoded slots.
pub(crate) fn poster_pump(budget: c_int) {
    {
        let mut g = store();
        g.frame = g.frame.wrapping_add(1); // new frame: nothing "touched" yet
    }
    for _ in 0..budget {
        let (idx, px, w, h, gen) = {
            let mut g = store();
            let mut found = None;
            for i in 0..PT_CAP {
                if g.slots[i].state == P_DECODED {
                    found = Some(i);
                    break;
                }
            }
            let idx = match found {
                Some(i) => i,
                None => break,
            };
            let s = &mut g.slots[idx];
            let (px, w, h, gen) = (s.px, s.pw, s.ph, s.gen);
            s.px = 0;
            s.state = P_UPLOADING;
            (idx, px, w, h, gen)
        };
        // GL upload off the lock (synchronous glTexImage2D — counted for the frame-drop detector)
        let t = img::img_upload_rgba(px as *const c_uchar, w, h);
        UP_CT.fetch_add(1, Ordering::Relaxed);
        UP_PX.fetch_add((w.max(0) as u64) * (h.max(0) as u64), Ordering::Relaxed);
        img::img_free(px as *mut c_uchar);

        let stale = {
            let mut g = store();
            let s = &mut g.slots[idx];
            if s.gen == gen && s.state == P_UPLOADING {
                s.tex = t;
                s.state = if t != 0 { P_READY } else { P_FAILED };
                0
            } else {
                t // slot recycled mid-upload: drop this texture
            }
        };
        if stale != 0 {
            gl_delete(stale);
        }
    }
}

/// BACKGROUND worker: claim a P_WANT slot, fetch+decode off-lock, publish P_DECODED.
fn poster_worker() {
    loop {
        let (idx, key_s, gen) = {
            let mut g = store();
            let idx = loop {
                if g.quit {
                    return;
                }
                let mut found = None;
                for i in 0..PT_CAP {
                    if g.slots[i].state == P_WANT {
                        found = Some(i);
                        break;
                    }
                }
                if let Some(i) = found {
                    break i;
                }
                let res = CV.wait_timeout(g, Duration::from_millis(50));
                g = res.map(|(guard, _)| guard).unwrap_or_else(|e| e.into_inner().0);
            };
            let key_s = String::from_utf8_lossy(key_bytes(&g.slots[idx])).into_owned();
            let sgen = g.slots[idx].gen;
            g.slots[idx].state = P_LOADING;
            (idx, key_s, sgen)
        };

        // fetch (Rust http_get boxes the stream off-stack) + decode — no GL here. host/port
        // come from the typed client singleton (the key_s path already carries the token).
        let c = crate::plex::client();
        let mut w = 0i32;
        let mut h = 0i32;
        let px = match stream::http_get(c.host(), c.port(), &key_s, None) {
            Some(b) if !b.is_empty() => img::img_decode_rgba(b.as_ptr(), b.len() as c_int, &mut w, &mut h),
            _ => std::ptr::null_mut(),
        };

        let mut g = store();
        let s = &mut g.slots[idx];
        if s.gen == gen && s.state == P_LOADING {
            if !px.is_null() {
                s.px = px as usize;
                s.pw = w;
                s.ph = h;
                s.state = P_DECODED;
            } else {
                s.state = P_FAILED;
            }
        } else if !px.is_null() {
            drop(g);
            img::img_free(px); // recycled while we worked: discard
        }
    }
}

/// Spawn the poster workers. Host/port/token are read from the typed client singleton
/// (`crate::plex::install` must run first), so no config is threaded in here.
pub(crate) fn posters_init() {
    {
        let mut g = store();
        g.quit = false;
        g.slots = [Pslot::ZERO; PT_CAP];
        g.workers.clear();
    }
    let handles: Vec<JoinHandle<()>> = (0..2).map(|_| std::thread::spawn(poster_worker)).collect();
    store().workers = handles;
}

pub(crate) fn posters_shutdown() {
    {
        let mut g = store();
        g.quit = true;
    }
    CV.notify_all();
    let handles = std::mem::take(&mut store().workers);
    for h in handles {
        let _ = h.join();
    }
    // free textures + pending decodes (main thread for GL); workers are joined
    let mut to_free = Vec::with_capacity(PT_CAP);
    {
        let mut g = store();
        for i in 0..PT_CAP {
            to_free.push((g.slots[i].tex, g.slots[i].px));
            g.slots[i] = Pslot::ZERO;
        }
    }
    for (tex, px) in to_free {
        if tex != 0 {
            gl_delete(tex);
        }
        if px != 0 {
            img::img_free(px as *mut c_uchar);
        }
    }
}
