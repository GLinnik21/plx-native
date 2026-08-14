//! Rust port of src/posters.c — async poster/artwork texture store (posters.h).
//! Same C ABI. Rewritten on std::sync instead of the C's pthread globals: a
//! Mutex<Store> + Condvar + std::thread workers. The decoded-pixel pointer is
//! stored as an address (usize) so the shared Store stays Send. GL calls
//! (upload/delete) run only on the main thread (poster_pump / poster_get);
//! workers only fetch (via the typed client) + decode (Rust img) off the lock.
//!
//! ## Every entry point names a SERVER, and none of them assumes one
//!
//! Artwork is per-server in the strongest sense: `/photo/:/transcode?url=…` is a path on ONE
//! PMS, its `url=` is that server's own thumb key, and the token baked into it is that server's
//! grant. With a friend's shared server merged into one Home, "the current server" is the wrong
//! host for half the tiles on screen — so a [`ServerId`] rides every call, sits in the slot as
//! the other half of its identity, and is what the worker dials. Nothing in this file reads
//! `plex::client()`; a caller that genuinely means "the server I am browsing" says so by passing
//! `plex::current_server()`, at the call site, where it is visible.
//!
//! The server is a FIELD on the slot, not a prefix on the key, deliberately: keys already run
//! ~135 bytes into a fixed 256-byte array (see [`Pslot::key`]), and a `u16` compare is also the
//! cheaper half of the per-frame identity scan, so it goes first.
use crate::img;
use crate::plex::ServerId;
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
/// Bytes a slot's key array holds, NUL included — see [`Pslot::key`] for the cliff at the end of
/// it, and [`KEY_MAX`] for the gate that keeps requests off it.
const PT_KEYLEN: usize = 256;
/// The longest built path that survives the round trip into a slot intact. Derived from
/// [`PT_KEYLEN`] rather than written as a literal, so the gate cannot drift from the array it
/// gates.
const KEY_MAX: usize = PT_KEYLEN - 1;
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
    // Full /photo/:/transcode request path = store key. NB set_key TRUNCATES to KEY_MAX bytes +
    // NUL: a key at/over PT_KEYLEN would never match its probe again (the text.rs glyph cache had
    // this exact bug at 96 — fixed with klen there), so every frame would miss, claim a fresh slot
    // and evict a real poster — the whole store thrashing over one tile. `poster_key` now REFUSES
    // to hand out a path this cannot hold (see [`KEY_MAX`]), which turns that into one skeleton
    // tile; if the shape ever routinely needs more room, key this the klen way instead of raising
    // the number.
    key: [u8; PT_KEYLEN],
    /// Which server this art was asked of — the OTHER half of the slot's identity, and what the
    /// worker dials. Two servers can hand out the same `/photo/:/transcode?url=/library/metadata/
    /// 42/thumb/…` path (their rating keys are both server-local integers from 1), so a key alone
    /// names a slot only while there is one server.
    srv: ServerId,
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
    const ZERO: Pslot = Pslot {
        key: [0; PT_KEYLEN],
        srv: ServerId::UNSET,
        tex: 0,
        pw: 0,
        ph: 0,
        px: 0,
        state: P_EMPTY,
        use_: 0,
        gen: 0,
        frame: 0,
    };
}

/// Does this slot hold the art `srv` was asked for under `key`?
///
/// The store's whole identity rule, extracted because it is the one thing a second server can
/// break invisibly: with the key alone, server B's card is served from A's slot — same texture,
/// wrong picture, and the wrong token on the fetch that filled it. The `u16` compare goes first
/// because this runs for every slot of every probe of every visible tile, and it is the half that
/// can reject without touching the 256-byte array.
fn slot_matches(s: &Pslot, srv: ServerId, key: &[u8]) -> bool {
    s.state != P_EMPTY && s.srv == srv && key_bytes(s) == key
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

// (server, path, w, h, png) → built transcode path, memoised. resolve_tex re-derives the key for
// every visible art tile every frame (~25-40 tiles × 60fps); the QueryBuilder + token clone behind
// image_transcode_path was ~15k heap allocs/sec of steady-state waste for keys that never change.
// MAIN-thread only (all poster_key callers are draw paths).
//
// The SERVER is part of the key and the token generation is part of the VALUE. Both matter, and
// neither is bookkeeping: the built path carries a token, so keying without the server would let
// server B's card be built from A's memoised, token-bearing path — a request to B carrying A's
// grant, which is a 401 at best. And the generation was one number for the whole memo, flushed
// when "the" token changed; with a table of servers there is no such number, so each entry
// remembers the generation it was built at and is rebuilt when THAT server's token moves.
/// The part of a memo key that is `Copy`. The PATH is the map's key instead, one level in, so a
/// lookup can borrow it.
///
/// `HashMap` takes its key by value in `entry`, so a flat `(u16, String, …)` key allocated a
/// `String` on every call — **hit or miss**. That is this module's hottest path: `poster_key` runs
/// once per visible tile per frame plus the hero art and logo, so at ~25–40 tiles × 60 fps it was
/// ~1,500–2,400 needless heap allocations a second on a 32-bit A53, on top of the `cstr` that
/// already built the same string. Nesting keeps the hit path allocation-free (`get` borrows a
/// `&str`) and pays for the owned key only on a real miss.
type MemoDims = (u16, c_int, c_int, bool);
struct KeyMemo {
    map: std::collections::HashMap<MemoDims, std::collections::HashMap<String, (u32, String)>>,
}
/// Entries kept before the memo is emptied wholesale. A ceiling, not a working set: a screen's
/// live keys are dozens, and this only bounds a session that browses thousands of tiles.
const MEMO_CAP: usize = 1024;

impl KeyMemo {
    /// The memo's whole rule, as a pure function of what it holds — so the two ways it can serve
    /// the wrong path are host-testable without a registry, a socket or a GL context: an entry
    /// belongs to ONE server, and it survives only as long as the token generation it was built
    /// at. `build` is called exactly when neither holds.
    fn get_or_build(
        &mut self,
        srv: u16,
        path: &str,
        w: c_int,
        h: c_int,
        png: bool,
        gen: u32,
        build: impl FnOnce() -> String,
    ) -> &str {
        let by_path = self.map.entry((srv, w, h, png)).or_default();
        // Bound the working set the same way the flat map did, one dimension bucket at a time.
        if by_path.len() > MEMO_CAP {
            by_path.clear();
        }
        // HIT: borrow the path, allocate nothing. This is the frame-rate path.
        if by_path.get(path).is_some_and(|e| e.0 == gen) {
            return &by_path[path].1;
        }
        // MISS (or a moved token): now the owned key is worth paying for.
        by_path.insert(path.to_owned(), (gen, build()));
        &by_path[path].1
    }
}
static mut KEY_MEMO: Option<KeyMemo> = None;

/// Build `srv`'s transcode request path (also the store key). png=1 -> transparent clearLogo.
/// The path (and token) come from that server's own `image_transcode_path` — the one place that
/// assembles a /photo/:/transcode request — so this stays the LRU key AND the fetch path.
///
/// ## `src_path` may be RELATIVE or ABSOLUTE, and both take this one route
///
/// Most art is a PMS-relative key (`/library/metadata/42/thumb/1778526065`). Search's `Directory[]`
/// results are the exception: an `actor` row's `thumb` is an **absolute
/// `https://metadata-static.plex.tv/…jpg`**, a different host entirely — and `stream.rs` has
/// neither DNS nor TLS, so the app can never dial it. It does not have to. `image_transcode_path`
/// percent-encodes whatever it is given into `url=` (`enc` is RFC3986-unreserved passthrough, so
/// `:` and `/` both become `%XX`), the request still goes to our own PMS, and **the server does
/// the TLS on our behalf**. Verified live against PMS 1.43.3 (2026-08-14): a headshot URL encoded
/// into `url=` answers `200 image/jpeg`, exactly the requested 300×300. `docs/pms-api.md` §2 says
/// the same thing about the detail page's cast row and adds the rule this doc is here to keep —
/// **never invent another image route for them**. There is deliberately no second code path: the
/// two shapes differ only in how long the encoded value is.
///
/// ## Three sources get an EMPTY key, which every store entry point reads as "nothing to fetch"
///
/// The alternative is a claimed slot for a request that can never resolve — a tile that stays a
/// skeleton forever while holding one of [`PT_CAP`].
///
///  * **A server the registry does not know**: no address to dial, no token to sign with.
///  * **An empty `src_path`**, which used to fall through and build a fetchable-LOOKING
///    `…&url=&X-Plex-Token=…`. This server answers that **404 `text/html`** (measured), so each one
///    burnt a slot as `P_FAILED` until the LRU walked back to it. A rarity worth ignoring while
///    every caller was `ui::widgets::resolve_tex_on`, which refuses an empty path at the call site
///    — but SEARCH makes it the normal case for a WHOLE SHELF, because a `/hubs/search`
///    `collection` row carries no `thumb` at all (nor a `ratingKey` — see `search::TagHit::thumb`).
///  * **A built path longer than a slot can key** (see [`KEY_MAX`]) — the cliff absolute URLs
///    brought within sight, and the worst of the three if it is let through.
pub(crate) fn poster_key(srv: ServerId, dst: *mut c_char, cap: usize, src_path: *const c_char, w: c_int, h: c_int, png: c_int) {
    if dst.is_null() || cap == 0 {
        return;
    }
    let Some(c) = crate::plex::client_for(srv) else {
        unsafe { crate::cbuf::set(dst, cap, "") };
        return;
    };
    unsafe {
        let path = cstr(src_path);
        if path.is_empty() {
            crate::cbuf::set(dst, cap, "");
            return;
        }
        let memo = (*std::ptr::addr_of_mut!(KEY_MEMO)).get_or_insert_with(|| KeyMemo { map: std::collections::HashMap::new() });
        let s = memo.get_or_build(srv.raw(), &path, w, h, png != 0, c.token_gen(), || {
            c.image_transcode_path(&path, w as i64, h as i64, png != 0)
        });
        // A path the slot array cannot hold is worse than no path: `set_key` would truncate it,
        // the truncated key would never equal the probe that built it, and every frame would claim
        // a fresh slot and evict a real poster. Refusing hands back the same skeleton an unknown
        // server gets. Headroom today is comfortable and worth stating so a future shape can be
        // judged against it: a headshot key measures ~177 bytes of the 255 (54 fixed + 89 encoded
        // URL + 34 of token), an ordinary relative thumb ~140.
        crate::cbuf::set(dst, cap, if s.len() <= KEY_MAX { s } else { "" });
    }
}

/// The clearLogo transcode request box (was two bare literals inside the old `logo_tex`).
/// `minSize=1` means COVER, not fit, so a 1:1 source comes back ~600×600 and a 5:1 one ~1200×240 —
/// both comfortably above anything [`crate::ui::hero_logo`] draws (900×268 worst case), so a hero
/// logo is always a downscale. Mirrored in `img.rs`'s decode-budget table; change both together.
const LOGO_REQ_W: c_int = 600;
const LOGO_REQ_H: c_int = 240;

/// The ONE clearLogo store key ([`LOGO_REQ_W`]×[`LOGO_REQ_H`] transparent PNG). Its own fn because
/// the PREFETCH must warm the exact key the draw will later resolve — `(server, path, w, h, png)`
/// IS the store key, so a warm at a different size (or against a different server) is a different
/// slot and buys nothing. `None` for an item with no ratingKey.
fn logo_key(srv: ServerId, rk: &str) -> Option<[u8; 352]> {
    if rk.is_empty() {
        return None;
    }
    let lpath = std::ffi::CString::new(format!("/library/metadata/{rk}/clearLogo")).ok()?;
    let mut key = [0u8; 352];
    poster_key(srv, key.as_mut_ptr() as *mut c_char, key.len(), lpath.as_ptr(), LOGO_REQ_W, LOGO_REQ_H, 1);
    Some(key)
}

/// The prefetch twin of [`logo_src`] — starts the same fetch, takes no texture and no LRU
/// protection. A hero page whose logo misses draws its title as HERO TEXT and then pops to the
/// logotype the moment it lands; warming kills that pop the same way it kills the backdrop's.
pub(crate) fn logo_warm(srv: ServerId, rk: &str) -> Warm {
    logo_key(srv, rk).map(|k| poster_warm(srv, k.as_ptr() as *const c_char)).unwrap_or(Warm::Known)
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
pub(crate) fn logo_src(srv: ServerId, rk: &str) -> Option<(c_uint, f32, f32)> {
    let key = logo_key(srv, rk)?;
    let (tex, lw, lh) = poster_get_wh(srv, key.as_ptr() as *const c_char);
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
fn lookup(srv: ServerId, key_s: &str, touch: Touch) -> (Hit, Warm) {
    let mut g = store();
    // hit?
    for i in 0..PT_CAP {
        if slot_matches(&g.slots[i], srv, key_s.as_bytes()) {
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
        s.srv = srv; // captured HERE, on the main thread — the worker asks no one which server
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

/// MAIN thread. READY texture for `srv`'s `key`, else 0 (claim a slot + enqueue fetch on miss).
pub(crate) fn poster_get(srv: ServerId, key: *const c_char) -> c_uint {
    poster_get_wh(srv, key).0
}

/// [`poster_get`] plus the texture's DECODED pixel size — `(0, 0, 0)` until it is READY. Art that
/// must be FIT or COVERED into its frame (a clearLogo, a hero backdrop) needs the source aspect, and
/// the store is the only thing that knows it. Same one probe as `poster_get`: the size rides along.
pub(crate) fn poster_get_wh(srv: ServerId, key: *const c_char) -> Hit {
    if key.is_null() {
        return (0, 0, 0);
    }
    let key_s = unsafe { cstr(key) };
    if key_s.is_empty() {
        return (0, 0, 0); // no path was built for this server — see [`poster_key`]
    }
    lookup(srv, &key_s, Touch::Draw).0
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
pub(crate) fn poster_warm(srv: ServerId, key: *const c_char) -> Warm {
    if key.is_null() {
        return Warm::Known;
    }
    let key_s = unsafe { cstr(key) };
    if key_s.is_empty() {
        // Nothing to enqueue and nothing spent: `Known` is what lets the caller's prefetch loop
        // walk on to the next candidate instead of retiring this frame's one warm on a key that
        // can never resolve.
        return Warm::Known;
    }
    lookup(srv, &key_s, Touch::Warm).1
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
        let (idx, key_s, srv, gen) = {
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
            let (srv, sgen) = (g.slots[idx].srv, g.slots[idx].gen);
            g.slots[idx].state = P_LOADING;
            (idx, key_s, srv, sgen)
        };

        // Fetch + decode off the lock — no GL here.
        //
        // The address is THIS SLOT's server, resolved from the id the main thread wrote when it
        // claimed the slot; the worker never asks what the current server is, because with two
        // registered it is the wrong host for half the tiles on screen. And the fetch goes
        // through `fetch_built`, not `get_bytes`: `key_s` IS the store key and already ends in
        // that server's token, so the ordinary path would append a second one. (Going through
        // the client at all is the point — `crate::stream` used to be called from here, behind
        // the back of the layer whose module doc claims to be the only code that touches it.)
        let mut w = 0i32;
        let mut h = 0i32;
        let body = crate::plex::client_for(srv).and_then(|c| c.fetch_built(&key_s));
        let px = match body {
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

/// Spawn the poster workers. No config is threaded in: each request carries its own server
/// (the slot's [`Pslot::srv`]), and the address behind it comes from the registry
/// (`crate::plex::install` or a `register` must have run before a fetch can resolve).
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
    //! The pure half of the store: which slot a miss claims, whether the store is quiet, and what
    //! [`poster_key`] builds.
    //!
    //! The LRU tests are ordinary parallel ones — each drives a LOCAL `[Pslot; PT_CAP]` array
    //! rather than the `STORE` singleton, deliberately: the singleton's eviction path frees a GL
    //! texture and no test binary on this host links GL. That boundary is exactly why [`victim`]
    //! and [`idle_of`] were split out of [`lookup`]/[`store_idle`] in the first place.
    //!
    //! The key-building tests are the exception and take [`crate::testlock::serial`]: they need a
    //! server in the registry, which is a crate global. They still touch no socket and no GL —
    //! `poster_key` only formats a string.
    use super::*;

    /// A synthetic headshot URL of the SHAPE the server really returns for a `/hubs/search`
    /// `actor` row: absolute, on Plex's metadata CDN, with a 32-hex digest. A made-up digest
    /// rather than a captured one, so nothing here is a record of who is in anyone's library —
    /// the LENGTH is what the encoding and the [`KEY_MAX`] headroom are graded on, and it matches.
    const HEADSHOT: &str = "https://metadata-static.plex.tv/a/people/0123456789abcdef0123456789abcdef.jpg";
    /// The same, encoded — every `:` and `/` gone, which is the whole reason the PMS photo
    /// transcoder can be handed a third-party URL as a query VALUE.
    const HEADSHOT_ENC: &str =
        "https%3A%2F%2Fmetadata-static.plex.tv%2Fa%2Fpeople%2F0123456789abcdef0123456789abcdef.jpg";

    /// A registry holding exactly one server, emptied again on the way out. The reset on DROP is
    /// the load-bearing half: `browse::pump` adopts every registered slot as a source and spawns a
    /// discovery worker for it, so a server left behind here would have another module's tests
    /// dialling a dead loopback port on a background thread.
    struct Fresh(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
    impl Drop for Fresh {
        fn drop(&mut self) {
            crate::plex::reset_servers_for_test();
        }
    }
    /// `register_for_test`, not the public `register`: the latter resolves the device id through
    /// `session::load`, which mints and PERSISTS a uuid on a host that has no session file.
    fn one_server() -> (Fresh, ServerId, &'static str) {
        let g = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let tok = "tok-poster-test";
        let sid = crate::plex::register_for_test("poster-test", "127.0.0.1", 32400, tok, "cid-poster-test");
        (Fresh(g), sid, tok)
    }

    /// Build a key through the real [`poster_key`], into the same 352-byte buffer
    /// `ui::widgets::tex_key` uses — so these grade the string a draw actually receives, not a
    /// reimplementation of it.
    fn key_for(srv: ServerId, src: &str, w: c_int, h: c_int, png: c_int) -> String {
        let src_c = std::ffi::CString::new(src).expect("test source paths carry no NUL");
        let mut buf = [0u8; 352];
        poster_key(srv, buf.as_mut_ptr() as *mut c_char, buf.len(), src_c.as_ptr(), w, h, png);
        String::from_utf8_lossy(crate::cbuf::as_bytes(&buf)).into_owned()
    }

    /// **Search's `Directory[]` half, and the reason this file needed no second image route.** An
    /// `actor` result's `thumb` is an absolute `https://metadata-static.plex.tv/…` URL on a host
    /// `stream.rs` can never reach — it has neither DNS nor TLS. It does not need to: the URL goes
    /// in as the `url=` VALUE, percent-encoded whole, and the request still goes to our own PMS,
    /// which fetches it over TLS on our behalf. Verified live against PMS 1.43.3 (2026-08-14):
    /// `200 image/jpeg`, exactly the requested 300×300.
    ///
    /// The assertions that matter are that nothing survives raw — a bare `://` or `?` in a query
    /// value would end the `url` parameter early and hand the server a truncated address — and
    /// that the token still lands at the end, on the outer request rather than inside `url=`.
    #[test]
    fn an_absolute_headshot_is_percent_encoded_into_the_url_parameter() {
        let (_g, sid, tok) = one_server();
        let k = key_for(sid, HEADSHOT, 300, 300, 0);

        assert!(k.starts_with("/photo/:/transcode?width=300&height=300&minSize=1&url="), "wrong request shape: {k}");
        assert!(k.contains(&format!("url={HEADSHOT_ENC}&")), "the absolute URL is not encoded whole: {k}");
        assert!(!k.contains("://"), "a raw scheme separator would truncate the url parameter: {k}");
        assert!(k.ends_with(&format!("&X-Plex-Token={tok}")), "the token belongs to the OUTER request: {k}");

        // The headroom the doc quotes, pinned. A headshot key is the longest shape the store holds
        // today; if a future one crosses KEY_MAX it is refused rather than truncated (below), so
        // this failing means artwork silently became skeletons, not that anything corrupted.
        assert!(k.len() <= KEY_MAX, "a headshot key must fit a slot: {} bytes", k.len());
    }

    /// The other half of "keep the existing path untouched": a PMS-relative key is encoded by the
    /// same rule and comes out the same shape it always did. Both kinds reach the transcoder
    /// through one code path, and this is what says so.
    #[test]
    fn a_relative_thumb_still_takes_the_old_path() {
        let (_g, sid, _) = one_server();
        let k = key_for(sid, "/library/metadata/42/thumb/1778526065", 250, 375, 0);
        assert!(k.starts_with("/photo/:/transcode?width=250&height=375&minSize=1&url="), "wrong request shape: {k}");
        assert!(k.contains("url=%2Flibrary%2Fmetadata%2F42%2Fthumb%2F1778526065&"), "relative key mis-encoded: {k}");

        // png=1 is the clearLogo flavour — the one thing that changes the shape — and it must not
        // have moved either.
        let logo = key_for(sid, "/library/metadata/42/clearLogo", LOGO_REQ_W, LOGO_REQ_H, 1);
        assert!(logo.contains("&format=png&"), "the transparent flavour lost its format: {logo}");
    }

    /// **A `/hubs/search` `collection` row carries no `thumb` at all** (verified live — no
    /// `ratingKey` either; only `key` and a tag `id`), so an empty source stops being a rarity and
    /// becomes a whole shelf of them. It must produce NO request: `…&url=&X-Plex-Token=…` is a
    /// `404 text/html` from this server (measured), and every one of them would burn a slot as
    /// `P_FAILED` until the LRU walked back to it.
    ///
    /// The empty key is the store's existing "nothing to fetch" convention — the same answer an
    /// unregistered server gets — so the tile draws its skeleton face and no prefetch budget is
    /// spent on it.
    ///
    /// It asserts only the BUILDER, and that limit is the host boundary rather than an oversight:
    /// calling `poster_get_wh`/`poster_warm` here — even on the empty key they return early for —
    /// makes `lookup` reachable from the test binary, and through it `gfx::delete_tex`. The link
    /// then fails with `Undefined symbols … _glDeleteTextures`, because `-dead_strip` was the only
    /// reason this suite got away without a GL context at all. Their empty-key early exits are
    /// asserted in prose at their definitions instead.
    #[test]
    fn an_empty_thumb_yields_no_request_at_all() {
        let (_g, sid, _) = one_server();
        assert_eq!(key_for(sid, "", 300, 300, 0), "", "an empty source must not become a request");
    }

    /// The cliff absolute URLs brought within sight, and the reason [`poster_key`] gates on
    /// [`KEY_MAX`] instead of trusting the caller's 352-byte buffer.
    ///
    /// `set_key` truncates into [`PT_KEYLEN`]. A truncated key never equals the probe that built
    /// it, so the slot can never be found again: every frame misses, claims a fresh slot and
    /// evicts a real poster — the whole store thrashing over one tile, which is far worse than the
    /// tile simply not loading. The sizes are DERIVED from a probe build rather than written down,
    /// so this keeps grading the real boundary if the request shape ever changes.
    #[test]
    fn a_path_too_long_for_a_slot_is_refused_rather_than_truncated() {
        let (_g, sid, _) = one_server();
        // Everything around the source, measured: build with a one-character source and subtract
        // it. The filler is alphanumeric, so it passes through `enc` one byte per byte.
        let overhead = key_for(sid, "x", 300, 300, 0).len() - 1;

        let exact = key_for(sid, &"a".repeat(KEY_MAX - overhead), 300, 300, 0);
        assert_eq!(exact.len(), KEY_MAX, "the fixture must land exactly on the boundary");
        let mut s = Pslot::ZERO;
        set_key(&mut s, &exact);
        assert_eq!(key_bytes(&s), exact.as_bytes(), "a key the gate accepts must survive a slot intact");

        let over = key_for(sid, &"a".repeat(KEY_MAX - overhead + 1), 300, 300, 0);
        assert_eq!(over, "", "one byte past what a slot holds must be refused, not handed out");

        // …because this is what handing it out would have meant. Not a claim about `set_key`, a
        // claim about `lookup`: the probe it compares against is the FULL string.
        let would_be = "b".repeat(KEY_MAX + 1);
        set_key(&mut s, &would_be);
        assert_ne!(key_bytes(&s), would_be.as_bytes(), "a truncated slot can never match its own probe");
    }

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

    /// **The two-server identity rule.** Rating keys are server-local integers from 1, so our own
    /// server and a friend's share hand out the SAME `/photo/:/transcode?url=/library/metadata/
    /// 42/thumb/…` path for two different films. With the key alone deciding a hit, the second
    /// one drawn shows the first one's picture — from a slot that was filled using the other
    /// server's token.
    #[test]
    fn two_servers_asking_for_the_same_art_path_do_not_share_a_slot() {
        const KEY: &str = "/photo/:/transcode?width=250&height=375&minSize=1&url=%2Flibrary%2Fmetadata%2F42%2Fthumb%2F1";
        let (a, b) = (ServerId::from_raw(0), ServerId::from_raw(1));
        let mut s = Pslot { state: P_READY, srv: a, ..Pslot::ZERO };
        set_key(&mut s, KEY);

        assert!(slot_matches(&s, a, KEY.as_bytes()), "the server that asked for it");
        assert!(!slot_matches(&s, b, KEY.as_bytes()), "the same path on another server is another slot");
        assert!(!slot_matches(&s, ServerId::UNSET, KEY.as_bytes()), "and an unknown server matches nothing");
        assert!(!slot_matches(&s, a, b"/photo/:/transcode?width=250&height=375&minSize=1&url=%2Fother"));

        // eviction only flips the state — the key bytes and the server stay behind, so an EMPTY
        // slot must be rejected before either is even compared
        let evicted = Pslot { state: P_EMPTY, ..s };
        assert!(!slot_matches(&evicted, a, KEY.as_bytes()));
    }

    /// The memo's twin of the rule above, plus the generation half. Both are about serving a path
    /// that carries a TOKEN: keyed without the server, server B's card is built from A's memoised
    /// path — B's host asked with A's grant, which is a 401 — and with one shared generation
    /// number, A's profile switch either flushes B's entries or (worse, once `client()` answers
    /// with a different server) reports that nothing changed at all.
    #[test]
    fn a_memo_entry_belongs_to_one_server_and_one_token_generation() {
        const PATH: &str = "/library/metadata/42/thumb/1755000000";
        let mut m = KeyMemo { map: std::collections::HashMap::new() };
        let builds = std::cell::Cell::new(0u32);
        let key = |m: &mut KeyMemo, srv: u16, w: c_int, gen: u32, built: &str| -> String {
            m.get_or_build(srv, PATH, w, 375, false, gen, || {
                builds.set(builds.get() + 1);
                built.to_owned()
            })
            .to_owned()
        };

        assert_eq!(key(&mut m, 0, 250, 7, "A?token=a"), "A?token=a");
        assert_eq!(key(&mut m, 0, 250, 7, "never built"), "A?token=a", "a hit does not rebuild");
        assert_eq!(builds.get(), 1);

        assert_eq!(key(&mut m, 1, 250, 9, "B?token=b"), "B?token=b", "server B builds its own");
        assert_eq!(key(&mut m, 0, 250, 7, "never built"), "A?token=a", "…and did not displace A's");
        assert_eq!(builds.get(), 2);

        // a profile switch on A: A's entry is rebuilt at the new generation, B's stands
        assert_eq!(key(&mut m, 0, 250, 8, "A?token=a2"), "A?token=a2");
        assert_eq!(key(&mut m, 1, 250, 9, "never built"), "B?token=b", "B's generation never moved");
        assert_eq!(builds.get(), 3);
        assert_eq!(key(&mut m, 0, 250, 8, "never built"), "A?token=a2", "the rebuild replaced, not appended");
        assert_eq!(builds.get(), 3);

        // the request box is still part of the key — a warm at one size and a draw at another are
        // two different store slots, so they must be two different entries here too
        assert_eq!(key(&mut m, 0, 1280, 8, "A-backdrop"), "A-backdrop");
        assert_eq!(key(&mut m, 0, 250, 8, "never built"), "A?token=a2");
        assert_eq!(builds.get(), 4);
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
