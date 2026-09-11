//! `TexCache<K>` — the library's RENDER-RESOURCE half of image caching (spec §10). It owns GL
//! texture residency and the LRU, `resolve`/`resolve_wh`, `warm`, the upload step under
//! `Budget`'s `Poster` class, and one `Provenance::Resource` invalidate when a texture becomes
//! resident. It never knows what a key denotes: the application's source half
//! (`app/adapters/poster.rs`, phase 3a — interning, the transcode URL, fetch + decode workers,
//! the disk tier) delivers a decoded image as `PosterReady` and the seam is the KEY.
//!
//! **The result handler only ACCEPTS** (`accept`: owned pixels into the pending queue, no GL).
//! **Upload happens in PREPARE** (`prepare`: §3.3 step 9, after `glViewport`, inside the
//! presented frame's GL scope, one `Budget::take` per upload, `warm` on each). A frame that does
//! not present uploads nothing and the queue waits — and it does not wait long, because
//! `note_queued` publishes the queue to the budget before the present decision reads it, so
//! pending work is itself a reason to present. The class of a take is chosen by the decoded byte
//! size ([`RESIDENCY_BYTES`]): a poster is a `Poster` take, a backdrop or a hero logo a
//! `Residency` one, which takes a frame to itself. The pixels are an owned render resource handed
//! over once: they never enter logical state, and the recorder writes only `(key, ok)`.
//!
//! **Residency is bounded by BYTES, not only by the [`CACHE_CAP`] slot count.** The source's 64
//! `Pslot`s cap how many DISTINCT items the cache can be asked to hold, but a poster (≈375 KB), a
//! hero logo (≈1.44 MB) and a backdrop (≈3.7 MB) are not the same weight, so a cache holding 64
//! slots of the heavier two costs tens of megabytes more than 64 posters — ordinary browsing (ten
//! detail pages, each warming a backdrop and a logo, then a shelf of posters) reached ≈68 MB
//! resident with slots still free, which breached `RenderSet` rule (c) (§8.3,
//! `ui/frame/render_set.rs`) on every `make sim` debug build while a release build only logged.
//! [`TEX_RESIDENT_BYTES_MAX`] is the second, independent ceiling `evict_for` enforces: LRU by
//! `last_used`, oldest first, exactly as the count cap already did — the two ceilings share one
//! eviction loop and either can fire first.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;

use super::frame::{Budget, Class};
use super::machine::{PosterKey, PresentHandle};
use super::present::{PresentEvent, Provenance, ResourceKind};

/// What a prefetch did — which is what lets a caller spend exactly ONE key per frame: a prefetch
/// loop walks its candidates and stops at the first `Claimed`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Warm {
    /// the source already holds this key (ready, failed, or in flight) — nothing was enqueued
    Known,
    /// a slot was claimed and the fetch enqueued — this frame's one prefetch is spent
    Claimed,
    /// every slot is in flight or evict-protected; try again on a later frame
    Full,
}

/// The application's SOURCE half of image caching (spec §10), as the library sees it: an
/// interning of `(server, path, w, h, png)` into an opaque [`PosterKey`], a fetch it starts on a
/// miss, and a decoded image it hands back through [`accept`]. `srv` is the server's raw id —
/// the library never names an application type. Installed once at boot ([`install`]).
pub trait Source {
    /// A DRAW's probe: `Some(key)` once the source has handed the cache this key's pixels (the
    /// texture may still be waiting for upload); `None` while empty, in flight or failed. A miss
    /// claims a slot and starts the fetch. Touches the source's LRU.
    fn probe(&self, srv: u16, path: &str, w: i32, h: i32, png: bool) -> Option<PosterKey>;
    /// The prefetch twin: start the fetch, take nothing, protect nothing.
    fn warm(&self, srv: u16, path: &str, w: i32, h: i32, png: bool) -> Warm;
    /// An item's clearLogo, at the source's one logo request box.
    fn logo(&self, srv: u16, rk: &str) -> Option<PosterKey>;
    fn logo_warm(&self, srv: u16, rk: &str) -> Warm;
    /// Nothing wanted, fetching or decoded-but-unaccepted — the prefetch gate.
    fn idle(&self) -> bool;
}

thread_local! {
    /// The render cache, on the GL thread. A named render-cache static (spec §15.2): screens
    /// reach it through the free functions below until they take it from `Cx` (phases 5–8).
    static CACHE: RefCell<TexCache<PosterKey>> =
        RefCell::new(TexCache::with_budget(CACHE_CAP, TEX_RESIDENT_BYTES_MAX));
    static SOURCE: Cell<Option<&'static dyn Source>> = const { Cell::new(None) };
}

/// One entry per source slot: the source's own eviction policy (`free` on recycle) is the one
/// LRU, so the cache never evicts on its own below this — **but see [`TEX_RESIDENT_BYTES_MAX`]**,
/// the second ceiling that fires long before 64 slots do whenever the resident mix skews toward
/// backdrops and hero logos rather than posters.
const CACHE_CAP: usize = 64;

/// **The cache's own residency ceiling — the tex pool's share of `RenderSet` rule (c)'s
/// `RENDER_BYTES_MAX` (§8.3, `ui/frame/render_set.rs`).** Here is the arithmetic, so a future
/// change to either side of it has to be re-derived rather than nudged.
///
/// ```text
///   RENDER_BYTES_MAX (ui/frame/render_set.rs)                    67,108,864 B  (64 MiB)
///   − FRAME_CACHE_BYTES (one Cached-host FrameCache, 1920×1080×4) 8,294,400 B
///   − worst per-screen RenderReport alive alongside the pool      8,294,400 B
///   ────────────────────────────────────────────────────────────────────────
///   = remaining for this pool                                    50,520,064 B  (≈48.18 MiB)
/// ```
///
/// The subtracted per-screen term is the larger of the two `RenderReport`s this file's own
/// screens can produce: the login QR (`screens/login.rs`, 400×400×4 = 640,000 B) and the
/// player's image-subtitle display set (`ui/player_hud.rs`'s `SubtitleBitmaps::bytes`). Neither
/// screen owns a texture from THIS pool (a QR code and a subtitle bitmap are each their own GL
/// resource, not a `ui::tex` key), but both count toward the SAME `RENDER_BYTES_MAX` ceiling this
/// pool shares, so the pool's own budget has to leave them room. The subtitle bound is not a
/// device measurement — `player/shared.rs`'s `SubBitmap` doc notes canvases up to 3840×2160 for
/// some 4K PGS, but an authored display SET (a caption, or a caption plus a sign card) is a
/// fraction of that canvas in every source seen; bounding it at one `FRAME_CACHE_BYTES`-sized
/// render (a full 1920×1080 upload) is generous for that in-practice case and simple to state,
/// while a genuinely pathological single display set is already bounded elsewhere — the decoded
/// store `player/mod.rs::push_subtitle_bitmap` evicts on a 24 MB total that only ONE track's ONE
/// display set is drawn from at a time.
///
/// FrameCache and a subtitle-bitmap render are on different screens (a Cached-host popover vs.
/// the player) but not mutually exclusive within one frame — a track menu or a marker pill is a
/// `Popover`+`TableView` (the DS idiom) served from the shared `FrameCache` while playback is
/// drawing subtitle bitmaps beneath it — so both terms are subtracted together rather than
/// taking their max.
///
/// The remainder (≈48.18 MiB) is rounded DOWN to **44 MiB**, buying ≈4.18 MiB (≈8.7%) of margin
/// against the subtitle bound being an estimate rather than a measurement — the same shape of
/// rounding `RENDER_BYTES_MAX` itself uses (68.8 MiB rounded to 64, ≈7% margin) and for the same
/// reason: a number that is wrong should be wrong SMALL, on the side that asserts early rather
/// than the side that ships a breach.
pub const TEX_RESIDENT_BYTES_MAX: usize = 44 << 20;

/// Decoded bytes above which an upload is a [`Class::Residency`] take rather than a
/// [`Class::Poster`] one (spec §8.1).
///
/// **The split is by PAYLOAD SIZE, because the cost is.** The class is not "what the picture is
/// for" — the cache has never known that — it is how long `glTexImage2D` plus the `warm_tex`
/// that follows it takes, which scales with the texels. The images this app decodes fall in two
/// clear groups: a 250x375 poster is ≈375 KB; a hero logo is requested at 600x240 with
/// `minSize=1`, which COVERS rather than fits, so a 1:1 source decodes to 600x600 ≈1.44 MB; and a
/// 1280x720 backdrop is ≈3.7 MB. 1 MiB sits in the gap, so the threshold is not a tuned number:
/// it is a line drawn through empty space, and every poster-sized image is a poster take.
pub const RESIDENCY_BYTES: usize = 1 << 20;

fn class_for(decoded_bytes: usize) -> Class {
    if decoded_bytes > RESIDENCY_BYTES {
        Class::Residency
    } else {
        Class::Poster
    }
}

/// Install the application's source, once, at boot.
pub fn install(src: &'static dyn Source) {
    SOURCE.with(|s| s.set(Some(src)));
}

fn with_source<T>(f: impl FnOnce(&dyn Source) -> T, absent: T) -> T {
    match SOURCE.with(|s| s.get()) {
        Some(src) => f(src),
        None => absent,
    }
}

/// The renderer's question for art on a named server: the resident texture id, or 0 (the
/// absent-resource rule: draw the placeholder at the final geometry; the request is already
/// in flight).
pub fn resolve_on(srv: u16, path: &str, w: i32, h: i32, png: bool) -> u32 {
    resolve_wh_on(srv, path, w, h, png).0
}

/// [`resolve_on`] plus the decoded pixel size — `(0, 0.0, 0.0)` until resident. Same one probe.
pub fn resolve_wh_on(srv: u16, path: &str, w: i32, h: i32, png: bool) -> (u32, f32, f32) {
    if path.is_empty() {
        return (0, 0.0, 0.0);
    }
    let Some(key) = with_source(|s| s.probe(srv, path, w, h, png), None) else {
        return (0, 0.0, 0.0);
    };
    resolve_key(key)
}

fn resolve_key(key: PosterKey) -> (u32, f32, f32) {
    CACHE.with(|c| match c.borrow_mut().resolve(key) {
        Some(t) => (t.id, t.w as f32, t.h as f32),
        None => (0, 0.0, 0.0),
    })
}

/// The prefetch twin of [`resolve_on`]: same arguments on purpose, so a screen warms EXACTLY the
/// key it will later resolve.
pub fn warm_on(srv: u16, path: &str, w: i32, h: i32, png: bool) -> Warm {
    if path.is_empty() {
        return Warm::Known;
    }
    with_source(|s| s.warm(srv, path, w, h, png), Warm::Known)
}

/// An item's clearLogo as a texture plus its TRUE pixel size; `None` while pending or absent.
pub fn logo_src(srv: u16, rk: &str) -> Option<(u32, f32, f32)> {
    let key = with_source(|s| s.logo(srv, rk), None)?;
    let (id, w, h) = resolve_key(key);
    (id != 0 && w > 0.0 && h > 0.0).then_some((id, w, h))
}

pub fn logo_warm(srv: u16, rk: &str) -> Warm {
    with_source(|s| s.logo_warm(srv, rk), Warm::Known)
}

/// The prefetch gate: the source is quiet AND nothing is waiting for upload.
pub fn source_idle() -> bool {
    with_source(|s| s.idle(), true) && !CACHE.with(|c| c.borrow().has_pending())
}

/// The source's delivery: one decoded image (or a failure) for a key. Touches no GL.
pub fn accept(r: PosterReady<PosterKey>) {
    CACHE.with(|c| c.borrow_mut().accept(r));
}

/// The source recycled a slot: its texture, if resident, is freed now (GL thread).
pub fn free(key: PosterKey, up: &mut dyn Uploader) {
    CACHE.with(|c| c.borrow_mut().free(key, up));
}

/// The upload step, once per frame in the GL scope (§3.3 step 9). Returns how many landed.
pub fn prepare(b: &mut Budget, up: &mut dyn Uploader, present: &mut PresentHandle<'_>, now_us: impl Fn() -> u64) -> usize {
    CACHE.with(|c| c.borrow_mut().prepare(b, up, present, now_us))
}

/// Whether the upload queue holds work — what forces a present (§3.3 step 8).
pub fn has_pending() -> bool {
    CACHE.with(|c| c.borrow().has_pending())
}

/// Publish the upload queue to the frame budget, so §3.3 step 8's `has_queued_work()` is the
/// answer about the REAL queue. The loop calls this once, immediately before the present
/// decision — after the frame's decoded images have been accepted and before anything reads the
/// verdict. Nothing published it until phase 11: `Budget::queued` had no product writer at all,
/// so the term that is supposed to force a present for waiting work was permanently false.
pub fn note_queued(b: &mut Budget) {
    b.note_queued(has_pending());
}

/// The decoded bytes of every resident texture (lane B's `RenderSet` accumulator reads it).
pub fn resident_bytes() -> usize {
    CACHE.with(|c| c.borrow().resident_bytes())
}

/// Free every resident texture (app exit, GL thread).
pub fn shutdown(up: &mut dyn Uploader) {
    CACHE.with(|c| c.borrow_mut().drain_all(up));
}

/// A decoded image: an OWNED render resource.
pub struct Decoded {
    pub w: u16,
    pub h: u16,
    pub rgba: Box<[u8]>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PosterError {
    Fetch,
    Decode,
    Refused,
}

/// The one message the app's poster adapter delivers to `MachineId::Cache`.
pub struct PosterReady<K> {
    pub key: K,
    pub result: Result<Decoded, PosterError>,
}

/// A resident texture as the renderer sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Tex {
    pub id: u32,
    pub w: u16,
    pub h: u16,
}

/// The GL half behind the cache: the real one wraps `gfx::tex_upload`/`warm_tex`; tests stub it.
pub trait Uploader {
    fn upload(&mut self, d: &Decoded) -> Tex;
    /// `gfx::warm_tex`: touch the texture inside the presented frame's GL scope (residency).
    fn warm(&mut self, t: Tex);
    fn free(&mut self, t: Tex);
}

struct Entry {
    tex: Tex,
    last_used: u64,
    /// The decoded bytes this entry accounts for in [`TexCache::bytes`]. Carried on the entry
    /// rather than recomputed from `tex.w * tex.h * 4` at each release, so the accumulator is
    /// conserved by construction: what a release subtracts is exactly what the upload added.
    bytes: usize,
}

pub struct TexCache<K> {
    resident: HashMap<K, Entry>,
    pending: VecDeque<(K, Decoded)>,
    failed: HashSet<K>,
    cap: usize,
    /// The byte ceiling `evict_for` holds residency under, independent of `cap`
    /// ([`TexCache::with_budget`]). `usize::MAX` (via [`TexCache::new`]) means "count-capped
    /// only" — every non-product caller (tests, `ui/fixture.rs`'s host fixture) wants that, since
    /// they size their own decoded payloads and are not grading this ceiling.
    bytes_max: usize,
    clock: u64,
    bytes: usize,
}

impl<K: Copy + Eq + Hash> TexCache<K> {
    /// Count-capped only — see [`TexCache::bytes_max`]'s doc for why that is the right default
    /// off the product path.
    pub fn new(cap: usize) -> Self {
        Self::with_budget(cap, usize::MAX)
    }

    /// The product constructor: bounds residency by BOTH the slot count and total decoded bytes.
    pub fn with_budget(cap: usize, bytes_max: usize) -> Self {
        Self {
            resident: HashMap::new(),
            pending: VecDeque::new(),
            failed: HashSet::new(),
            cap,
            bytes_max,
            clock: 0,
            bytes: 0,
        }
    }

    /// The result handler: moves the pixels into the pending queue and touches no GL.
    pub fn accept(&mut self, r: PosterReady<K>) {
        match r.result {
            Ok(d) => {
                self.failed.remove(&r.key);
                self.pending.push_back((r.key, d));
            }
            Err(_) => {
                self.failed.insert(r.key);
            }
        }
    }

    /// Whether `prepare` has work — `Budget::note_queued` reads it at the top of the frame.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The upload step (§3.3 step 9). `now_us` is read before EVERY take — one clock reading per
    /// admission decision, never one per call. Returns how many textures became resident.
    ///
    /// The class is chosen by the DECODED BYTE SIZE of the image at the head of the queue
    /// ([`RESIDENCY_BYTES`]): an ordinary poster is a `Poster` take, a backdrop or a hero logo is
    /// a `Residency` one, which the budget admits only on a frame of its own. A refusal BREAKS —
    /// the queue is a FIFO and the head is what the frame that presents it needs, so skipping
    /// past a refused item would upload art nobody is waiting for and leave the wait in place.
    ///
    /// Every admitted upload calls `evict_for` (LRU, oldest first) to hold BOTH residency
    /// ceilings: the slot count (`cap`) and the decoded-byte budget (`bytes_max`,
    /// [`TEX_RESIDENT_BYTES_MAX`] on the product cache) — never the slot count alone. A single
    /// decoded image bigger than the whole byte budget is still uploaded: refusing it here would
    /// refuse that poster or backdrop forever, since nothing ever makes it smaller, so it lands
    /// with the rest of the cache evicted around it instead.
    pub fn prepare(
        &mut self,
        b: &mut Budget,
        up: &mut dyn Uploader,
        present: &mut PresentHandle<'_>,
        now_us: impl Fn() -> u64,
    ) -> usize {
        let mut n = 0;
        while let Some((key, d)) = self.pending.front() {
            let key = *key;
            let class = class_for(d.rgba.len());
            if !b.take(class, now_us()) {
                break;
            }
            let (_, d) = self.pending.pop_front().expect("front() was Some");
            let bytes = d.rgba.len();
            // a key already resident is replaced in place and needs no ROOM, but its new pixels
            // still count against the byte budget — `protect` keeps the key being replaced out of
            // its own eviction, so making room for a bigger re-upload never evicts itself.
            let is_new = !self.resident.contains_key(&key);
            let protect = if is_new { None } else { Some(key) };
            self.evict_for(if is_new { 1 } else { 0 }, bytes, protect, up);
            let tex = up.upload(&d);
            up.warm(tex);
            self.clock += 1;
            self.bytes += bytes;
            if let Some(old) = self.resident.insert(
                key,
                Entry {
                    tex,
                    last_used: self.clock,
                    bytes,
                },
            ) {
                // a re-upload of a key that was already resident: the OLD entry's bytes go with
                // its texture
                self.bytes = self.bytes.saturating_sub(old.bytes);
                up.free(old.tex);
            }
            n += 1;
        }
        if n > 0 {
            present.note(PresentEvent::Damage(Provenance::Resource(ResourceKind::Texture)));
        }
        n
    }

    /// Drop one key's residency and any pending pixels for it (the source recycled its slot).
    pub fn free(&mut self, k: K, up: &mut dyn Uploader) {
        if let Some(e) = self.resident.remove(&k) {
            self.bytes = self.bytes.saturating_sub(e.bytes);
            up.free(e.tex);
        }
        self.pending.retain(|(pk, _)| *pk != k);
        self.failed.remove(&k);
    }

    /// Free everything (exit).
    pub fn drain_all(&mut self, up: &mut dyn Uploader) {
        for (_, e) in self.resident.drain() {
            up.free(e.tex);
        }
        self.pending.clear();
        self.failed.clear();
        self.bytes = 0;
    }

    /// Evict LRU-first (`last_used`, oldest first — the one policy both ceilings share) until
    /// EITHER holds: `room` more slots fit under `cap`, or `incoming` more bytes fit under
    /// `bytes_max`. `protect`, when set, is the key about to be re-uploaded in place — it is
    /// excluded from victim selection so a replacement can never evict itself to make room for
    /// its own new pixels; if it is the only entry left and the incoming size alone exceeds the
    /// budget, the loop stops (nothing left to evict) and the caller uploads anyway. A single
    /// image bigger than the whole budget is never refused — see [`TexCache::prepare`]'s doc —
    /// it uploads with the cache emptied around it, `bytes` briefly over `bytes_max` until the
    /// next upload's eviction brings it back down.
    fn evict_for(&mut self, room: usize, incoming: usize, protect: Option<K>, up: &mut dyn Uploader) {
        loop {
            let over_count = self.resident.len() + room > self.cap;
            let over_bytes = self.bytes.saturating_add(incoming) > self.bytes_max;
            if !over_count && !over_bytes {
                break;
            }
            let victim = self
                .resident
                .iter()
                .filter(|(k, _)| protect != Some(**k))
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| *k);
            let Some(k) = victim else { break };
            if let Some(e) = self.resident.remove(&k) {
                self.bytes = self.bytes.saturating_sub(e.bytes);
                up.free(e.tex);
            }
        }
    }

    /// The renderer's question. A hit is a use (LRU); a miss is the absent-resource rule's cue
    /// (§8.2): draw the placeholder at the final geometry and request once.
    pub fn resolve(&mut self, k: K) -> Option<Tex> {
        self.clock += 1;
        let clock = self.clock;
        self.resident.get_mut(&k).map(|e| {
            e.last_used = clock;
            e.tex
        })
    }

    pub fn resolve_wh(&mut self, k: K) -> Option<(u16, u16)> {
        self.resolve(k).map(|t| (t.w, t.h))
    }

    pub fn is_failed(&self, k: K) -> bool {
        self.failed.contains(&k)
    }

    pub fn resident_count(&self) -> usize {
        self.resident.len()
    }

    /// The decoded bytes of every RESIDENT texture.
    pub fn resident_bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::present::Present;

    struct StubUp {
        next: u32,
        freed: Vec<u32>,
        warmed: Vec<u32>,
    }
    impl Uploader for StubUp {
        fn upload(&mut self, d: &Decoded) -> Tex {
            self.next += 1;
            Tex {
                id: self.next,
                w: d.w,
                h: d.h,
            }
        }
        fn warm(&mut self, t: Tex) {
            self.warmed.push(t.id);
        }
        fn free(&mut self, t: Tex) {
            self.freed.push(t.id);
        }
    }

    fn ready(k: u32) -> PosterReady<u32> {
        PosterReady {
            key: k,
            result: Ok(Decoded {
                w: 2,
                h: 2,
                rgba: vec![0; 16].into_boxed_slice(),
            }),
        }
    }

    #[test]
    fn accept_touches_no_gl_and_prepare_uploads_under_the_poster_quota() {
        let mut c: TexCache<u32> = TexCache::new(2);
        let mut up = StubUp {
            next: 0,
            freed: vec![],
            warmed: vec![],
        };
        for k in 1..=4 {
            c.accept(ready(k));
        }
        assert_eq!(up.next, 0, "accept uploads nothing");
        assert!(c.has_pending());
        let mut b = Budget::new();
        b.begin_frame(0);
        let mut present = Present::new();
        let _ = present.take(0);
        let mut ph = PresentHandle(&mut present);
        let n = c.prepare(&mut b, &mut up, &mut ph, || 0);
        assert_eq!(n, 3, "the quota is three per frame");
        assert!(c.has_pending(), "the fourth waits");
        assert_eq!(c.resident_count(), 2, "cap 2: the oldest was evicted");
        assert_eq!(up.freed, vec![1]);
        assert_eq!(up.warmed, vec![1, 2, 3]);
        assert!(present.take(16), "a resident texture is one damage");
        assert!(c.resolve(3).is_some() && c.resolve(1).is_none());
    }

    /// `bytes` is the accumulator lane B's `RenderSet` reads. It is incremented at every upload
    /// and must be decremented on every one of the FOUR ways a texture is released: the source
    /// recycling a slot (`free`), exit (`drain_all`), the LRU (`evict_for`) and a re-upload of a
    /// key that was already resident (`insert` returning the old entry). Two of those four never
    /// decremented, so the number only ever grew.
    #[test]
    fn resident_bytes_are_conserved_across_eviction_and_replacement() {
        let mut c: TexCache<u32> = TexCache::new(2);
        let mut up = StubUp {
            next: 0,
            freed: vec![],
            warmed: vec![],
        };
        let mut present = Present::new();
        let mut b = Budget::new();
        let one = 2 * 2 * 4; // `ready`'s 2x2 RGBA
        // three distinct keys into a cache of two: the third eviction frees the first
        for k in 1..=3 {
            c.accept(ready(k));
        }
        b.begin_frame(0);
        let mut ph = PresentHandle(&mut present);
        assert_eq!(c.prepare(&mut b, &mut up, &mut ph, || 0), 3);
        assert_eq!(c.resident_count(), 2);
        assert_eq!(c.resident_bytes(), 2 * one, "an evicted texture's bytes are released");
        // and the two paths that always decremented
        let resident: Vec<u32> = (1..=3).filter(|k| c.resolve(*k).is_some()).collect();
        c.free(resident[0], &mut up);
        assert_eq!(c.resident_bytes(), one);
        c.drain_all(&mut up);
        assert_eq!(c.resident_bytes(), 0);

        // a key that is uploaded again while already resident: the REPLACED entry's bytes go
        // with its texture. A cache with room, so the LRU is not what is being graded.
        let mut c: TexCache<u32> = TexCache::new(4);
        for k in 1..=2 {
            c.accept(ready(k));
        }
        b.begin_frame(0);
        let mut ph = PresentHandle(&mut present);
        assert_eq!(c.prepare(&mut b, &mut up, &mut ph, || 0), 2);
        assert_eq!(c.resident_bytes(), 2 * one);
        c.accept(ready(1));
        b.begin_frame(0);
        let mut ph = PresentHandle(&mut present);
        assert_eq!(c.prepare(&mut b, &mut up, &mut ph, || 0), 1);
        assert_eq!(c.resident_count(), 2, "the same two keys");
        assert_eq!(c.resident_bytes(), 2 * one, "a replaced texture's bytes are released");
    }

    /// The finding this fix answers: `evict_for` used to evict ONLY on `resident.len() + room >
    /// cap`, so a cache with plenty of free SLOTS but a heavy resident mix (a backdrop, a hero
    /// logo per detail page — see this file's module doc) grew without bound, breaching
    /// `RenderSet` rule (c) on a `make sim` debug build while a release build only logged it. A
    /// cap of 64 slots is far too loose to prove the byte ceiling actually bites, so this cache
    /// is built wide (`cap` = 8, no slot pressure at all) and narrow on bytes: four ~1 MB images,
    /// a budget that fits two. Watched red against the code before this fix: with no `bytes_max`
    /// consulted, all four eventually land and `resident_bytes()` reaches ~4x the budget.
    #[test]
    fn residency_is_bounded_by_bytes_not_only_by_slots() {
        let sized = |k: u32, bytes: usize| PosterReady {
            key: k,
            result: Ok(Decoded {
                w: 1,
                h: 1,
                rgba: vec![0; bytes].into_boxed_slice(),
            }),
        };
        const IMG: usize = 1_000_000; // under RESIDENCY_BYTES, so every upload is a Poster take
        const BUDGET: usize = 2_500_000; // room for two IMGs, not three

        let mut c: TexCache<u32> = TexCache::with_budget(8, BUDGET);
        let mut up = StubUp {
            next: 0,
            freed: vec![],
            warmed: vec![],
        };
        let mut present = Present::new();
        let mut b = Budget::new();

        for k in 1..=4 {
            c.accept(sized(k, IMG));
        }
        b.begin_frame(0);
        let mut ph = PresentHandle(&mut present);
        // the Poster quota is 3/frame: keys 1..3 are admitted by the BUDGET, but the byte
        // ceiling evicts key 1 (oldest) the moment key 3's upload would push past it
        let n = c.prepare(&mut b, &mut up, &mut ph, || 0);
        assert_eq!(n, 3, "the frame budget's poster quota is still three");
        assert!(
            c.resident_bytes() <= BUDGET,
            "resident bytes ({}) must never exceed the byte ceiling ({BUDGET}), regardless of \
             free slots (cap=8, resident_count={})",
            c.resident_bytes(),
            c.resident_count()
        );
        assert!(c.resolve(3).is_some(), "the newest upload is resident");
        assert!(c.has_pending(), "key 4 waited for the byte ceiling, not the slot count");

        // the next frame admits key 4 within quota; the byte ceiling evicts the now-oldest
        // resident (key 2) LRU-first to make room, never touching the newest (key 3)
        b.begin_frame(20_000);
        let mut ph = PresentHandle(&mut present);
        assert_eq!(c.prepare(&mut b, &mut up, &mut ph, || 20_000), 1);
        assert!(
            c.resident_bytes() <= BUDGET,
            "still under budget after the second frame: {}",
            c.resident_bytes()
        );
        assert!(c.resolve(4).is_some(), "the newest upload is resident");
        assert!(c.resolve(3).is_some(), "the previous frame's newest survives — LRU, oldest first");
        assert!(!c.has_pending());
    }

    /// A re-upload of a key that is ALREADY resident needs no room, so it must evict nobody: the
    /// upload step used to call `evict_for(1)` before every insert, so a full cache threw out its
    /// least-recent innocent to make room for a texture that was replacing one in place.
    #[test]
    fn a_replacement_upload_evicts_nothing() {
        let mut c: TexCache<u32> = TexCache::new(2);
        let mut up = StubUp {
            next: 0,
            freed: vec![],
            warmed: vec![],
        };
        let mut present = Present::new();
        let mut b = Budget::new();
        for k in 1..=2 {
            c.accept(ready(k));
        }
        b.begin_frame(0);
        let mut ph = PresentHandle(&mut present);
        assert_eq!(c.prepare(&mut b, &mut up, &mut ph, || 0), 2);
        // key 2 is the least recent: touch key 1 after both landed
        assert!(c.resolve(1).is_some());
        c.accept(ready(1));
        b.begin_frame(0);
        let mut ph = PresentHandle(&mut present);
        assert_eq!(c.prepare(&mut b, &mut up, &mut ph, || 0), 1);
        assert!(c.resolve(2).is_some(), "the innocent least-recent key survived the replacement");
        assert_eq!(c.resident_count(), 2);
        assert_eq!(c.resident_bytes(), 2 * (2 * 2 * 4));
    }

    /// §3.3 step 8's second term. The product's present decision reads `Budget::has_queued_work`,
    /// and the queue it must reflect is THIS one — the pixels `accept` parked for the upload step.
    #[test]
    fn has_queued_work_reflects_the_texture_queue() {
        let mut b = Budget::new();
        b.begin_frame(0);
        note_queued(&mut b);
        assert!(!b.has_queued_work(), "an empty queue does not force a present");
        accept(PosterReady {
            key: PosterKey(0),
            result: Ok(Decoded {
                w: 2,
                h: 2,
                rgba: vec![0; 16].into_boxed_slice(),
            }),
        });
        note_queued(&mut b);
        assert!(b.has_queued_work(), "a pending texture forces the frame that uploads it");
        // leave the thread-local cache as this test found it
        CACHE.with(|c| c.borrow_mut().pending.clear());
        note_queued(&mut b);
        assert!(!b.has_queued_work());
    }

    /// §15.1: admission reads the clock BEFORE EVERY take — one reading per take, never one per
    /// call. A counting closure is the proof: three admitted uploads and the refusal that closed
    /// the window are four takes, so four readings.
    #[test]
    fn budget_admission_reads_the_clock_before_every_take() {
        let mut c: TexCache<u32> = TexCache::new(8);
        let mut up = StubUp {
            next: 0,
            freed: vec![],
            warmed: vec![],
        };
        for k in 1..=5 {
            c.accept(ready(k));
        }
        let reads = std::cell::Cell::new(0u32);
        let mut present = Present::new();
        let mut b = Budget::new();
        b.begin_frame(0);
        let mut ph = PresentHandle(&mut present);
        let n = c.prepare(&mut b, &mut up, &mut ph, || {
            reads.set(reads.get() + 1);
            0
        });
        assert_eq!(n, 3, "the quota is three");
        assert_eq!(reads.get(), 4, "one reading per take: three admitted, one refusal");
    }

    /// The class split is by decoded byte size, and a large image takes the frame to itself.
    #[test]
    fn a_large_image_uploads_on_a_frame_of_its_own() {
        assert_eq!(class_for(375 * 1024), Class::Poster);
        assert_eq!(class_for(1280 * 720 * 4), Class::Residency);

        let big = |k: u32| PosterReady {
            key: k,
            result: Ok(Decoded {
                w: 1280,
                h: 720,
                rgba: vec![0; 1280 * 720 * 4].into_boxed_slice(),
            }),
        };
        let mut c: TexCache<u32> = TexCache::new(8);
        let mut up = StubUp {
            next: 0,
            freed: vec![],
            warmed: vec![],
        };
        let mut present = Present::new();
        let mut b = Budget::new();

        // a backdrop at the head of the queue: admitted alone, the posters behind it wait
        c.accept(big(1));
        c.accept(ready(2));
        c.accept(ready(3));
        b.begin_frame(0);
        let mut ph = PresentHandle(&mut present);
        assert_eq!(c.prepare(&mut b, &mut up, &mut ph, || 0), 1, "the solo frame uploads one");
        assert_eq!(b.solo(), Some(Class::Residency));
        assert!(c.has_pending(), "the posters behind it wait for the next frame");

        // the next frame is ordinary: both posters land
        b.begin_frame(20_000);
        let mut ph = PresentHandle(&mut present);
        assert_eq!(c.prepare(&mut b, &mut up, &mut ph, || 20_000), 2);
        assert_eq!(b.solo(), None);
        assert!(!c.has_pending());

        // a backdrop BEHIND a poster does not jump the queue: the poster goes, the backdrop is
        // refused for not being first, and the frame after gives it its own
        c.accept(ready(4));
        c.accept(big(5));
        b.begin_frame(40_000);
        let mut ph = PresentHandle(&mut present);
        assert_eq!(c.prepare(&mut b, &mut up, &mut ph, || 40_000), 1);
        b.begin_frame(60_000);
        let mut ph = PresentHandle(&mut present);
        assert_eq!(c.prepare(&mut b, &mut up, &mut ph, || 60_000), 1);
        assert_eq!(b.solo(), Some(Class::Residency));
        assert!(!c.has_pending());
    }
}
