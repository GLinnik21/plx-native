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

/// How a store lookup treats the slot it lands on. The difference IS the prefetch's safety story.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Touch {
    /// A draw: bump the LRU clock and stamp the frame (evict-protect — a slot this frame asked for
    /// must never be a victim, or a full screen of tiles would evict each other).
    Draw,
    /// A prefetch: neither. See [`poster_warm`].
    Warm,
}

/// What a [`poster_warm`] did — which is what lets a caller spend exactly ONE key per frame: a
/// prefetch loop walks its candidates and stops at the first `Claimed`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Warm {
    /// the store already holds this key (ready, failed, or in flight) — nothing was enqueued
    Known,
    /// a slot was claimed and the fetch enqueued — this frame's one prefetch is spent
    Claimed,
    /// every slot is in flight or evict-protected; try again on a later frame
    Full,
}

#[derive(Clone, Copy)]
struct Pslot {
    // Full /photo/:/transcode request path = store key. NB set_key TRUNCATES to 255 bytes + NUL:
    // a key at/over 256 bytes would never match its probe again (the text.rs glyph cache had this
    // exact bug at 96 — fixed with klen there). Real keys run ~135 bytes today; if the path shape
    // ever grows past ~255, key this the klen way too.
    key: [u8; 256],
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

/// The clearLogo transcode request box (was two bare literals inside the old `logo_tex`).
/// `minSize=1` means COVER, not fit, so a 1:1 source comes back ~600×600 and a 5:1 one ~1200×240 —
/// both comfortably above anything [`crate::ui::hero_logo`] draws (900×268 worst case), so a hero
/// logo is always a downscale. Mirrored in `img.rs`'s decode-budget table; change both together.
const LOGO_REQ_W: c_int = 600;
const LOGO_REQ_H: c_int = 240;

/// The ONE clearLogo store key ([`LOGO_REQ_W`]×[`LOGO_REQ_H`] transparent PNG). Its own fn because
/// the PREFETCH must warm the exact key the draw will later resolve — `(path, w, h, png)` IS the
/// store key, so a warm at a different size is a different slot and buys nothing. `None` for an item
/// with no ratingKey.
fn logo_key(rk: &str) -> Option<[u8; 352]> {
    if rk.is_empty() {
        return None;
    }
    let lpath = std::ffi::CString::new(format!("/library/metadata/{rk}/clearLogo")).ok()?;
    let mut key = [0u8; 352];
    poster_key(key.as_mut_ptr() as *mut c_char, key.len(), lpath.as_ptr(), LOGO_REQ_W, LOGO_REQ_H, 1);
    Some(key)
}

/// The prefetch twin of [`logo_src`] — starts the same fetch, takes no texture and no LRU
/// protection. A hero page whose logo misses draws its title as HERO TEXT and then pops to the
/// logotype the moment it lands; warming kills that pop the same way it kills the backdrop's.
pub(crate) fn logo_warm(rk: &str) -> Warm {
    logo_key(rk).map(|k| poster_warm(k.as_ptr() as *const c_char)).unwrap_or(Warm::Known)
}

/// Resolve an item's clearLogo (transparent PNG) to a GL texture plus its TRUE PIXEL SIZE.
/// `Some((tex, px_w, px_h))` once loaded; `None` while pending OR when the item has no logo (the
/// store cannot tell those two apart — which is why the text→logo swap is still a cut, see
/// [`crate::ui::hero_logo`]).
///
/// The ONE clearLogo resolve (home hero, detail hero, detail compact title all draw through it).
/// How big it is DRAWN is a UI decision and lives in [`crate::ui::hero_logo::fit`]: this layer used
/// to take a `max_w`×`max_h` box and contain-fit into it, which is how the same mark ended up three
/// sizes in one app — the store never owned layout policy, it only looked as if it did.
///
/// NB it claims a slot even for an item that HAS no logo — the 404 lands as `P_FAILED` and holds it
/// — so every hero page costs two of the store's [`PT_CAP`] slots, backdrop plus logo.
pub(crate) fn logo_src(rk: &str) -> Option<(c_uint, f32, f32)> {
    let key = logo_key(rk)?;
    let (tex, lw, lh) = poster_get_wh(key.as_ptr() as *const c_char);
    if tex == 0 || lw <= 0 || lh <= 0 {
        return None;
    }
    Some((tex, lw as f32, lh as f32))
}

/// The slot a miss claims, as a PURE function of what the store looks like: the first EMPTY, else
/// the least-recently-used SETTLED (`P_READY`/`P_FAILED`) slot the current frame has not touched,
/// else `None` — "everything is either in flight or on screen; skip this request".
///
/// Extracted from [`lookup`] because the PREFETCH's entire safety argument is a claim about this
/// function — that a warmed slot (LRU age 0, no frame stamp) is always a more attractive victim than
/// anything a draw touched — and the store around it is not host-testable: eviction frees a GL
/// texture, and no test binary on this host links GL.
///
/// Note the second clause is what bounds a prefetch's depth, and it is sharper than eviction:
/// `P_WANT`/`P_LOADING`/`P_DECODED` are never victims, so a batch of in-flight warms does not merely
/// age the store out, it can make this return `None` for a poster the user is looking at — which is
/// a tile that is never even REQUESTED, not one that arrives late.
fn victim(slots: &[Pslot; PT_CAP], frame: c_uint) -> Option<usize> {
    if let Some(i) = (0..PT_CAP).find(|&i| slots[i].state == P_EMPTY) {
        return Some(i);
    }
    let mut oldest = c_uint::MAX;
    let mut pick = None;
    for i in 0..PT_CAP {
        let s = &slots[i];
        if (s.state == P_READY || s.state == P_FAILED) && s.frame != frame && s.use_ < oldest {
            oldest = s.use_;
            pick = Some(i);
        }
    }
    pick
}

/// What one store probe found: the READY texture and its DECODED pixel size, or `(0, 0, 0)` while
/// the slot is empty, in flight, or failed. One answer rather than two calls because the size is
/// free once the slot is in hand — and this is a per-frame path walked once per visible tile, so a
/// second lock + 64-slot key scan just to read two ints is pure waste.
type Hit = (c_uint, c_int, c_int);

/// MAIN thread. The one store lookup behind [`poster_get`], [`poster_get_wh`] and [`poster_warm`]:
/// hit → the READY texture + its size, miss → claim a slot and enqueue the fetch. `touch` is the ONLY
/// difference between the paths, and it is entirely about the LRU bookkeeping.
fn lookup(key_s: &str, touch: Touch) -> (Hit, Warm) {
    let mut g = store();
    // hit?
    for i in 0..PT_CAP {
        if g.slots[i].state != P_EMPTY && key_bytes(&g.slots[i]) == key_s.as_bytes() {
            if touch == Touch::Draw {
                g.clock = g.clock.wrapping_add(1);
                let (c, f) = (g.clock, g.frame);
                g.slots[i].use_ = c;
                g.slots[i].frame = f;
            }
            let s = &g.slots[i];
            let hit = if s.state == P_READY { (s.tex, s.pw, s.ph) } else { (0, 0, 0) };
            return (hit, Warm::Known);
        }
    }
    // miss: prefer EMPTY, else LRU-evict a settled slot not used this frame
    let idx = match victim(&g.slots, g.frame) {
        Some(i) => i,
        None => return ((0, 0, 0), Warm::Full), // all visible: skip
    };
    let (old_tex, old_px) = (g.slots[idx].tex, g.slots[idx].px);
    let (use_, frame) = match touch {
        Touch::Draw => {
            g.clock = g.clock.wrapping_add(1);
            (g.clock, g.frame)
        }
        // age 0 = older than everything a draw has touched, so a warmed slot is always the FIRST
        // victim of the next miss; frame-1 is by construction never the current frame, so it never
        // carries evict-protection — and unlike a literal 0 it does not depend on `poster_pump`
        // having bumped the frame counter yet (a screen's update runs BEFORE the pump).
        Touch::Warm => (0, g.frame.wrapping_sub(1)),
    };
    {
        let s = &mut g.slots[idx];
        s.tex = 0;
        s.px = 0;
        s.gen = s.gen.wrapping_add(1);
        set_key(s, key_s);
        s.state = P_WANT;
        s.use_ = use_;
        s.frame = frame;
        s.pw = 0;
        s.ph = 0;
    }
    drop(g);
    // free the evicted resources off-lock (this is the GL/main thread)
    if old_tex != 0 {
        gl_delete(old_tex);
    }
    if old_px != 0 {
        img::img_free(old_px as *mut c_uchar);
    }
    CV.notify_one();
    ((0, 0, 0), Warm::Claimed)
}

/// MAIN thread. READY texture for key, else 0 (claim a slot + enqueue fetch on miss).
pub(crate) fn poster_get(key: *const c_char) -> c_uint {
    poster_get_wh(key).0
}

/// [`poster_get`] plus the texture's DECODED pixel size — `(0, 0, 0)` until it is READY. Art that
/// must be FIT or COVERED into its frame (a clearLogo, a hero backdrop) needs the source aspect, and
/// the store is the only thing that knows it. Same one probe as `poster_get`: the size rides along.
pub(crate) fn poster_get_wh(key: *const c_char) -> Hit {
    if key.is_null() {
        return (0, 0, 0);
    }
    let key_s = unsafe { cstr(key) };
    lookup(&key_s, Touch::Draw).0
}

/// MAIN thread. Ensure `key` is being fetched WITHOUT drawing it — the prefetch path (the texture
/// twin of `browse::want`). Returns what it did, never a texture, so no caller can accidentally draw
/// a warmed slot and re-age it behind the store's back.
///
/// Two deliberate differences from [`poster_get`], and together they are why a prefetch can share a
/// 64-slot store with everything on screen:
///  * it never stamps `frame`, so a warmed slot carries NO evict-protection — a visible tile that
///    misses on the very next frame takes the slot straight back;
///  * it never bumps `use_`, so a warmed slot stays at LRU age 0, permanently the first victim.
///
/// Prefetched art is by construction the cheapest thing in the store to throw away.
pub(crate) fn poster_warm(key: *const c_char) -> Warm {
    if key.is_null() {
        return Warm::Known;
    }
    let key_s = unsafe { cstr(key) };
    lookup(&key_s, Touch::Warm).1
}

/// Pure half of [`store_idle`] — split so the gate is host-testable without mutating the store
/// singleton.
fn idle_of(slots: &[Pslot; PT_CAP]) -> bool {
    !slots.iter().any(|s| matches!(s.state, P_WANT | P_LOADING | P_DECODED))
}

/// MAIN thread. Is the store QUIET — nothing wanted, fetching, or waiting to upload? The prefetch
/// gate, and the honest answer to "never compete with a texture the user is waiting on".
///
/// It has to be a GATE rather than a priority because [`poster_worker`] claims the first `P_WANT`
/// slot **by slot index**, and indices come from "first EMPTY, else LRU victim" ([`victim`]) — i.e.
/// arbitrarily. With two workers a prefetch in a low index is picked ahead of a visible poster in a
/// high one. The way to stay off the critical path is not to rank requests, it is to only ISSUE one
/// on a frame where nothing else is outstanding.
///
/// Cost: one 64-slot scan per frame under the mutex — the draw already does dozens.
pub(crate) fn store_idle() -> bool {
    idle_of(&store().slots)
}

// (The old standalone `poster_wh` — a second lock + 64-slot key scan to read two ints about a slot
// the caller had just probed — is gone: `lookup` hands the size back with the texture, so a
// cover-fitted tile now costs exactly what a stretched one does. Both of its callers, `logo_src` and
// `ui::widgets::resolve_tex_wh`, go through `poster_get_wh`.)

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
        } else {
            // A texture just landed on a screen that may have gone idle waiting for it. No spring
            // reports this — the reveal spring is only armed once the tile SEES a texture — so the
            // present gate has to be told, or the poster arrives invisibly and the shelf stays
            // grey until the next keypress. (`ui::idle::invalidate` — see its call-site list.)
            crate::ui::idle::invalidate();
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
    // filter_map, not map: a refused worker is one fewer decoder, not a dead app. Artwork degrades
    // to whatever the survivors can fetch (and to nothing at all if both are refused).
    let handles: Vec<JoinHandle<()>> =
        (0..2).filter_map(|_| crate::task::spawn("poster", poster_worker)).collect();
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
        // these park in `stream::http_get`, whose socket nothing outside the call can reach —
        // so an app exit against a stalled PMS waits out SO_RCVTIMEO here. Measured, not fixed.
        crate::task::join("poster", h);
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

#[cfg(test)]
mod tests {
    //! The pure half of the store: which slot a miss claims, and whether the store is quiet.
    //!
    //! Ordinary parallel tests — every one of these drives a LOCAL `[Pslot; PT_CAP]` array rather
    //! than the `STORE` singleton, deliberately: the singleton's eviction path frees a GL texture
    //! and no test binary on this host links GL. That boundary is exactly why [`victim`] and
    //! [`idle_of`] were split out of [`lookup`]/[`store_idle`] in the first place.
    use super::*;

    /// A settled, drawable slot: READY, last touched on frame `frame` with LRU age `use_`.
    fn ready(use_: c_uint, frame: c_uint) -> Pslot {
        Pslot { state: P_READY, use_, frame, ..Pslot::ZERO }
    }

    /// The LRU's two clauses, in order: an EMPTY slot is always preferred (a fresh store must fill
    /// before it evicts anything), and only once there is none does the oldest SETTLED slot go.
    #[test]
    fn victim_prefers_empty_then_oldest_settled() {
        let empty = [Pslot::ZERO; PT_CAP];
        assert_eq!(victim(&empty, 7), Some(0), "an untouched store fills from the front");

        let mut one_hole = [ready(100, 1); PT_CAP];
        one_hole[40] = Pslot::ZERO;
        assert_eq!(victim(&one_hole, 7), Some(40), "an empty slot beats every settled one");

        let mut full = [ready(100, 1); PT_CAP];
        full[17].use_ = 3; // the oldest
        full[52].use_ = 9;
        assert_eq!(victim(&full, 7), Some(17), "with nothing empty, the least recently used goes");

        // FAILED is settled too — a 404 must not pin a slot forever.
        let mut failed = [ready(100, 1); PT_CAP];
        failed[8] = Pslot { state: P_FAILED, use_: 1, frame: 1, ..Pslot::ZERO };
        assert_eq!(victim(&failed, 7), Some(8));
    }

    /// **The test the whole prefetch rests on.** `Touch::Warm` writes `use_ = 0` and a frame stamp
    /// of `frame - 1`, so a warmed slot is simultaneously the oldest thing in the store and
    /// unprotected — it must lose to every slot this frame drew. And when the frame really has
    /// touched everything, the answer must be `None` ("all visible: skip"), never a slot in use.
    #[test]
    fn a_warmed_slot_is_evicted_before_anything_the_frame_drew() {
        let f: c_uint = 42;
        let mut slots = [ready(c_uint::MAX - 1, f); PT_CAP]; // 64 tiles this frame drew
        slots[31] = ready(0, f.wrapping_sub(1)); // …and one warmed a moment ago
        assert_eq!(victim(&slots, f), Some(31), "the prefetched slot is the cheapest to throw away");

        let drawn = [ready(5, f); PT_CAP];
        assert_eq!(victim(&drawn, f), None, "a frame that drew every slot claims nothing");

        // frame counters wrap (`wrapping_add` in poster_pump), and a warm at frame 0 stamps
        // c_uint::MAX — which must still read as "not this frame" rather than as protection.
        let mut wrapped = [ready(c_uint::MAX - 1, 0); PT_CAP];
        wrapped[2] = ready(0, 0u32.wrapping_sub(1));
        assert_eq!(victim(&wrapped, 0), Some(2));
    }

    /// The sharp edge behind the depth budget: a slot that is fetching is never a victim, so enough
    /// in-flight warms make a miss return `None` — a poster the user is looking at that is never
    /// even REQUESTED, which is worse than one that arrives late. Pinned in code, not just in prose.
    #[test]
    fn an_in_flight_slot_is_never_evicted() {
        for st in [P_WANT, P_LOADING, P_DECODED, P_UPLOADING] {
            let slots = [Pslot { state: st, use_: 0, frame: 0, ..Pslot::ZERO }; PT_CAP];
            assert_eq!(victim(&slots, 7), None, "state {st} must not be evictable");
        }
    }

    /// The prefetch gate sees every stage of a fetch. If this ever loosens, the prefetch quietly
    /// starts competing with the tiles on screen for the two workers.
    #[test]
    fn idle_of_sees_every_stage_of_a_fetch() {
        assert!(idle_of(&[Pslot::ZERO; PT_CAP]), "an untouched store is idle");
        for st in [P_READY, P_FAILED] {
            let slots = [Pslot { state: st, ..Pslot::ZERO }; PT_CAP];
            assert!(idle_of(&slots), "settled state {st} is not work in progress");
        }
        for st in [P_WANT, P_LOADING, P_DECODED] {
            let mut slots = [Pslot { state: P_READY, ..Pslot::ZERO }; PT_CAP];
            slots[63] = Pslot { state: st, ..Pslot::ZERO };
            assert!(!idle_of(&slots), "one slot in state {st} is enough to hold the gate shut");
        }
    }
}
