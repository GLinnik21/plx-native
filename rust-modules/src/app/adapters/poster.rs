//! The application's SOURCE half of image caching (restructure spec §10): interning
//! `(server, path, w, h, png)` into an opaque [`PosterKey`] (a slot index), the transcode request
//! path and its token, the fetch + decode workers, the disk tier (`imgcache`), the prefetch gate
//! and the slot LRU. It delivers a decoded image as `tex::PosterReady` and holds NO texture: GL
//! residency is the library's [`crate::ui::tex::TexCache`], reached through `ui::tex`'s free
//! functions, and the seam between the two halves is the key. The library never names this
//! module; it sees it as the [`tex::Source`] installed at [`init`].
//!
//! A slot's lifecycle: EMPTY → WANT (claimed by a draw's miss or a prefetch) → LOADING (a worker
//! fetches + decodes off the lock) → DECODED (pixels waiting on the main thread) → READY (the
//! pixels were handed to the cache by [`drain_decoded`]; the cache uploads them in PREPARE),
//! EVICTED (the render cache could not keep that key resident, through rejection or pressure), or
//! FAILED / RETRY (a transient fetch parked under bounded backoff). EVICTED does NOT re-arm on
//! a prefetch: [`lookup`]'s `P_EVICTED` branch answers a `Touch::Warm` probe with dormancy before
//! the cooldown gate is even reached, so a background prefetch walking past an evicted key never
//! revives it. Only a `Touch::Draw` probe re-arms the slot — so a poster or backdrop that lost
//! residency off-screen does not quietly reappear before something asks to show it again; the pop
//! that eviction caused stays on screen until the draw that wants the key re-requests it. The
//! source cannot offer another upload from retained pixels because ownership moved to the render
//! cache and retaining a second CPU copy of the entire 44 MiB GL pool would defeat the memory
//! ceiling; a later demand therefore pays a full network refetch, decode and upload, not a disk
//! read — `imgcache`'s disk tier holds only plex.tv avatars (`classify` answers `None` for an
//! ordinary server-relative transcode path; `class_of` matches only a plex.tv avatar URL), so a
//! poster, backdrop or hero logo has no disk fallback to land on. That real cost is why re-arming
//! is rate-limited rather than free: it is the reason the residency thrash guard, the 250 ms–8 s
//! cooldown backoff and the Draw-only gate above exist at all. A READY slot recycled by
//! [`victim`] frees its cache entry on the way out.
//!
//! Rust port of the old src/posters.c; rewritten on std::sync (a `Mutex<Store>` + `Condvar` +
//! two `task::spawn` workers). The decoded-pixel pointer is stored as an address (usize) so the
//! shared `Store` stays `Send`.
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
//! ~140 bytes (an ordinary relative thumb) to ~177 (an absolute headshot — see [`built_key`])
//! into a fixed [`PT_KEYLEN`]-byte array (see [`Pslot::key`]), and a `u16` compare is also the
//! cheaper half of the per-frame identity scan, so it goes first.
use crate::img;
use crate::plex::ServerId;
use crate::ui::machine::PosterKey;
use crate::ui::tex::{self, Decoded, PosterError, PosterReady, Tex, Uploader, Warm};
use std::os::raw::{c_int, c_uchar, c_uint};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

// Per-frame GL-upload counters for the frame-drop detector (app/run.rs): each upload the cache
// performs through `GfxUploader` is a synchronous glTexImage2D on the main thread — the prime
// suspect for scroll judder. `take_upload_stats` reads-and-resets (call once per frame).
static UP_CT: AtomicU32 = AtomicU32::new(0);
static UP_PX: AtomicU64 = AtomicU64::new(0);
/// (uploads, total pixels) since the last call; resets both. Main-thread, once per frame.
pub(crate) fn take_upload_stats() -> (u32, u64) {
    (
        UP_CT.swap(0, Ordering::Relaxed),
        UP_PX.swap(0, Ordering::Relaxed),
    )
}

const PT_CAP: usize = 64;
/// Bytes a slot's key array holds, NUL included — see [`Pslot::key`] for the cliff at the end of
/// it, and [`KEY_MAX`] for the gate that keeps requests off it.
const PT_KEYLEN: usize = 256;
/// The longest built path that survives the round trip into a slot intact. Derived from
/// [`PT_KEYLEN`] rather than written as a literal, so the gate cannot drift from the array it
/// gates.
const KEY_MAX: usize = PT_KEYLEN - 1;

/// Can this key ever resolve to a picture? The store's ONE precondition, and it lives here for the
/// same reason [`victim`] and [`idle_of`] were split out: it was duplicated across the entry
/// points, none of those copies could be tested (reaching them drags in `gfx::delete_tex`, which
/// no host test binary links), and the array it protects belongs to [`lookup`], not to whoever
/// built the string.
///
/// **Empty** means no request could be built — an unknown server, or an item the server gave no
/// art for (see [`poster_key`]).
///
/// **Longer than [`KEY_MAX`]** means [`set_key`] would TRUNCATE it, after which the stored key can
/// never equal the probe that built it: every frame misses, claims a fresh slot and evicts a real
/// poster — the whole store thrashing over one tile. `poster_key` refuses to hand such a path out,
/// but it is not the only thing that can reach `lookup` (the three entry points take a raw
/// `*const c_char`), so the check that matters is the one at the array.
///
/// Either way the answer is the same: no slot is claimed, and the tile draws its skeleton.
fn is_fetchable(key: &str) -> bool {
    !key.is_empty() && key.len() <= KEY_MAX
}
const P_EMPTY: c_int = 0;
const P_WANT: c_int = 1;
const P_LOADING: c_int = 2;
const P_DECODED: c_int = 3;
/// The pixels were handed to the render cache; whether they are UPLOADED yet is the cache's.
const P_READY: c_int = 5;
const P_FAILED: c_int = 6;
/// The render cache could not keep this key resident, through rejection or pressure. No pixels
/// are retained here and no fetch starts until the key is demanded again; see the module
/// lifecycle note for why.
const P_EVICTED: c_int = 8;
/// A fetch that failed for a reason that can change — the address race's plaintext window, a
/// refused or timed-out connect, a 5xx — parked until [`Pslot::retry_at`]. Settled for the LRU
/// like `P_FAILED`, but a DRAW that finds it due puts it back to `P_WANT`. `P_FAILED` is kept for
/// an HTTP answer that is not transient, or bytes the decoder could not read.
const P_RETRY: c_int = 7;

/// Speculation may occupy one worker, never both. That leaves capacity for a visible miss that
/// arrives after the prefetch began; workers cannot interrupt a fetch already off-lock.
const PREFETCH_OUTSTANDING_MAX: usize = 1;

/// How a store lookup treats the slot it lands on. The difference IS the prefetch's safety story.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Touch {
    /// A draw: bump the LRU clock and stamp the frame (evict-protect — a slot this frame asked for
    /// must never be a victim, or a full screen of tiles would evict each other).
    Draw,
    /// A prefetch: neither. See [`poster_warm`].
    Warm,
}

// `Warm` — what a prefetch did — is the library's `ui::tex::Warm` now (re-exported through
// the `use` above): the prefetch loops in `ui/` read it without naming this module.

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
    pw: c_int,
    ph: c_int,
    px: usize, // decoded RGBA ptr as address (0 = none) — keeps Pslot Send
    state: c_int,
    /// A draw has asked for this key. Workers serve visible work before speculative warms.
    visible: bool,
    use_: c_uint,  // LRU clock
    gen: c_uint,   // bumped on eviction; stale-decode guard
    frame: c_uint, // last frame poster_get touched it (evict-protect)
    /// `P_RETRY` only: the app-clock tick (`app::clock::now`, ms) the next attempt may go at.
    /// `None` = parked by the worker and not yet scheduled — the next DRAW on the main thread
    /// sets it, because the clock is the loop's and a worker may not read it.
    retry_at: Option<u32>,
    /// The due deadline already requested its one present. Without this latch an off-screen retry
    /// would invalidate every loop iteration forever because no draw visits it to re-queue it.
    retry_wake_sent: bool,
    /// Transient failures in a row for THIS key — the backoff's exponent; cleared on a claim.
    attempts: u8,
    /// The app-clock tick of this slot's most recent READY→EVICTED transition (residency
    /// pressure, [`PosterSource::unresident`] — never a fresh claim). `None` until the first
    /// eviction. Compared against [`EVICT_THRASH_WINDOW_MS`] on the NEXT eviction to tell an
    /// isolated, ordinary LRU turnover from a continuation of an ongoing thrash episode; see
    /// [`evict_was_rapid`].
    evicted_at: Option<u32>,
    /// Consecutive RAPID re-evictions (see [`evict_was_rapid`]) — the residency thrash guard's
    /// exponent, mirroring `attempts` above but for byte-pressure churn rather than a transient
    /// fetch failure. Reset to 0 the instant an eviction is NOT rapid, so an ordinary, occasional
    /// LRU turnover never accumulates backoff it does not deserve.
    evict_attempts: u8,
    /// The app-clock tick before which [`lookup`] refuses to re-arm this EVICTED slot — `None`
    /// means no cooldown is owed. Set only once `evict_attempts` shows an ongoing thrash episode:
    /// the FIRST eviction of a key always re-arms on its very next Draw probe, which is the
    /// recovery [`lookup`]'s P_EVICTED branch exists for and the case
    /// `a_ready_source_hit_recovers_after_its_texture_is_evicted` pins. See [`evict_backoff`].
    evict_cooldown_until: Option<u32>,
    /// Mirrors `retry_wake_sent`, for the same reason: latches that THIS `evict_cooldown_until`
    /// deadline has already requested its one present, so [`invalidate_due_evictions`] does not
    /// invalidate every frame for an off-screen slot nothing draws. Reset whenever a fresh
    /// cooldown is armed ([`PosterSource::unresident`]) or the slot's eviction history is
    /// otherwise cleared (recovery, recycle) — a new deadline is something to wake for again.
    evict_wake_sent: bool,
}
impl Pslot {
    const ZERO: Pslot = Pslot {
        key: [0; PT_KEYLEN],
        srv: ServerId::UNSET,
        pw: 0,
        ph: 0,
        px: 0,
        state: P_EMPTY,
        visible: false,
        use_: 0,
        gen: 0,
        frame: 0,
        retry_at: None,
        retry_wake_sent: false,
        attempts: 0,
        evicted_at: None,
        evict_attempts: 0,
        evict_cooldown_until: None,
        evict_wake_sent: false,
    };
}

/// How long a slot waits after its `n`th transient failure: 1 s, 2 s, 4 s, 8 s, 16 s, then 30 s
/// for good. The first step is short on purpose — the failure this exists for is the boot's
/// plaintext window, which is over within a second — and the cap keeps a server that is really
/// down from being asked more than twice a minute per tile.
fn retry_backoff(attempts: u8) -> Duration {
    let secs = 1u64 << attempts.saturating_sub(1).min(5);
    Duration::from_secs(secs.min(30))
}

/// Is a `P_RETRY` slot's wait over? An unscheduled one is not (it has no wait yet); the tick
/// comparison wraps, like every `app::clock` comparison.
fn retry_due(s: &Pslot, now: u32) -> bool {
    s.state == P_RETRY && s.retry_at.is_some_and(|t| now.wrapping_sub(t) < u32::MAX / 2)
}

/// **The residency thrash guard (Codex P1 review on PR #182, `lookup`'s P_EVICTED branch).**
/// A `Touch::Warm` probe never reaches this guard at all — it is turned away before the branch
/// even looks at the cooldown (see the branch itself) — so what is left for this guard to bound is
/// narrower than the original review reads: a working set that genuinely cannot fit under
/// [`tex::TEX_RESIDENT_BYTES_MAX`] still has the render cache evict key A to make room, a DRAW of A
/// re-arm it, A's eventual upload evict key B, a DRAW of B re-arm IT, and so on with no bound —
/// continuous fetch/decode/upload every frame instead of settling, driven entirely by on-screen
/// demand rather than off-screen prefetch.
///
/// The guard is two pure decisions, both keyed off wall-clock ticks so they cost nothing under the
/// store lock and are host-testable without a real elapsed second (mirrors [`retry_due`] and the
/// residency log's own throttle):
///
/// - [`evict_was_rapid`]: was the PREVIOUS eviction of this key recent enough that this new one is
///   a continuation of an ongoing churn episode, rather than an isolated, ordinary LRU turnover?
///   Ten seconds comfortably exceeds one fetch+decode+upload round trip (the window this guard has
///   to see through), so a slot that settles for that long always gets treated as fresh again —
///   the window is sized against the round trip, not against how a person actually browses, so a
///   user who leaves a screen and comes back inside ten seconds can still land inside an ongoing
///   episode and pay whatever cooldown was already running, with no sustained overload involved.
/// - [`evict_backoff`]: given consecutive rapid re-evictions, how long does the NEXT re-arm wait?
///   Deliberately its own schedule rather than a reuse of [`retry_backoff`] — that one paces
///   retries against a server that may be down, capped at 30 s; this one paces retries against a
///   cache that is merely full, and a screen whose working set has genuinely settled should feel
///   that within a few seconds, not thirty.
///
/// **What the bound actually guarantees.** A key's FIRST eviction always has `evicted_at == None`
/// beforehand, so [`evict_was_rapid`] answers `false`, `evict_attempts` stays 0, and no cooldown is
/// set at all — the ordinary one-eviction-then-recovery case this PR exists to fix re-arms on the
/// very next Draw probe, exactly as before. Only a key that is evicted AGAIN inside the thrash
/// window — proof that something is still churning it — pays a cooldown, and that cooldown is
/// bounded ([`evict_backoff`] caps at 8 s): a Draw probe of the key is guaranteed another turn to
/// ATTEMPT recovery within that bound. That is narrower than it may sound — it is eligibility for
/// another attempt, not a promise the attempt loads successfully, and not durable residency: under
/// sustained pressure the texture can be evicted again the instant it lands.
const EVICT_THRASH_WINDOW_MS: u32 = 10_000;

/// Pure half of the guard: given the tick of a key's previous eviction (`None` before its first),
/// does a new eviction at `now` land inside the thrash window?
fn evict_was_rapid(prev: Option<u32>, now: u32) -> bool {
    prev.is_some_and(|t| now.wrapping_sub(t) < EVICT_THRASH_WINDOW_MS)
}

/// How long the NEXT re-arm of a key waits after its `n`th consecutive RAPID re-eviction (see
/// [`evict_was_rapid`]): 250 ms, 500 ms, 1 s, 2 s, 4 s, then 8 s for good. `n == 0` never reaches
/// this — see the guard's module doc for why a key's first eviction sets no cooldown at all.
fn evict_backoff(evict_attempts: u8) -> Duration {
    let millis = 250u64 << evict_attempts.saturating_sub(1).min(5);
    Duration::from_millis(millis.min(8_000))
}

/// Has a P_EVICTED slot's cooldown cleared? Wrap-safe like [`retry_due`]; `None` (no cooldown was
/// ever set) is always due.
fn evict_cooldown_due(cooldown_until: Option<u32>, now: u32) -> bool {
    match cooldown_until {
        None => true,
        Some(t) => now.wrapping_sub(t) < u32::MAX / 2,
    }
}

/// Is an EVICTED slot's cooldown deadline due for its one wake — as opposed to [`evict_cooldown_due`],
/// which also answers `true` for a slot that never had a cooldown at all? A slot with no deadline has
/// nothing to expire and needs no proactive wake — its very next Draw probe already re-arms it, same
/// as today. Only a slot an [`evict_backoff`] deadline is ticking against needs [`invalidate_due_evictions`]
/// to ask for the frame that lets it recover on a quiet screen; mirrors [`retry_due`]'s wrap-safe shape.
fn evict_wake_due(s: &Pslot, now: u32) -> bool {
    s.state == P_EVICTED && s.evict_cooldown_until.is_some_and(|t| now.wrapping_sub(t) < u32::MAX / 2)
}

/// Wake the present gate once a scheduled retry's main-thread deadline arrives. The loop calls this
/// even while a settled screen skips draws; the draw it requests is what probes the slot again and
/// moves it back to `P_WANT`.
fn invalidate_due_retries(slots: &mut [Pslot; PT_CAP], now: u32) {
    let mut due = false;
    for s in slots.iter_mut().filter(|s| !s.retry_wake_sent && retry_due(s, now)) {
        s.retry_wake_sent = true; // latch the whole batch; one frame probes every visible slot
        due = true;
    }
    if due {
        crate::ui::idle::invalidate();
    }
}

/// The eviction-cooldown twin of [`invalidate_due_retries`], and the fix for the gap the
/// adjudication on PR #182 flagged: nothing previously asked the screen to draw again when a
/// cooldown deadline passed, so on a quiet screen a slot recovered only when something ELSE
/// happened to trigger a frame — later than its own deadline, sometimes much later. Same shape,
/// same reason for the `evict_wake_sent` latch: without it, an expired cooldown on a slot nothing
/// draws would invalidate every loop iteration forever, undoing the idle behaviour the app depends
/// on. The wake only ever asks for one present; it does not itself re-arm anything — re-arming
/// stays Draw-only, through `lookup`'s P_EVICTED branch, exactly as before.
fn invalidate_due_evictions(slots: &mut [Pslot; PT_CAP], now: u32) {
    let mut due = false;
    for s in slots.iter_mut().filter(|s| !s.evict_wake_sent && evict_wake_due(s, now)) {
        s.evict_wake_sent = true;
        due = true;
    }
    if due {
        crate::ui::idle::invalidate();
    }
}

/// Park one transient result and request the draw that assigns its main-thread deadline.
fn park_retry(s: &mut Pslot) {
    s.attempts = s.attempts.saturating_add(1);
    s.retry_at = None;
    s.retry_wake_sent = false;
    s.state = P_RETRY;
    crate::ui::idle::invalidate();
}

/// Does a failed fetch's outcome deserve another try? Transport failure and the statuses whose
/// conditions can clear (401/403 after a re-point or token refresh, 408, 429 and 5xx) are
/// transient. Other completed HTTP answers are final: retrying a redirect we deliberately do not
/// follow, an unsupported method/media type, or a missing item can never change this request.
/// Bytes that arrived but did not decode are the decoder's verdict and final too.
fn is_transient(outcome: &crate::plex::ArtFetch) -> bool {
    match outcome {
        crate::plex::ArtFetch::Bytes(_) => false,
        crate::plex::ArtFetch::Status(s) => matches!(s, 401 | 403 | 408 | 429 | 500..=599),
        crate::plex::ArtFetch::NoResponse => true,
    }
}

/// Does this slot hold the art `srv` was asked for under `key`?
///
/// The store's whole identity rule, extracted because it is the one thing a second server can
/// break invisibly: with the key alone, server B's card is served from A's slot — same texture,
/// wrong picture, and the wrong token on the fetch that filled it. The `u16` compare goes first
/// because this runs for every slot of every probe of every visible tile, and it is the half that
/// can reject without touching the [`PT_KEYLEN`]-byte array.
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
            clock: 0,
            frame: 0,
            quit: false,
            workers: Vec::new(),
        }
    }
}

static STORE: Mutex<Store> = Mutex::new(Store::new());
static CV: Condvar = Condvar::new();

fn store() -> MutexGuard<'static, Store> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
}

fn key_bytes(s: &Pslot) -> &[u8] {
    crate::cbuf::as_bytes(&s.key)
}
fn set_key(s: &mut Pslot, key: &str) {
    crate::cbuf::set_bytes(&mut s.key, key);
}

// (server, path, w, h, png) → built transcode path, memoised. resolve_tex_wh_on re-derives the key for
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

/// The memo is a SECOND crate global, and it outlives `plex::reset_servers_for_test`. Entries are
/// keyed `(slot, w, h, png)` and every test re-registers at slot 0, so without this a test is
/// served the previous test's path for the same request box. That the key tests pass at all rests
/// on `Client::new` drawing a process-unique `token_gen` from a monotone sequence — true today,
/// incidental, and exactly the kind of thing a later simplification (per-server counters from 0)
/// would break in a way nothing in this module would explain. Reset it explicitly instead.
#[cfg(test)]
fn reset_key_memo() {
    unsafe { *std::ptr::addr_of_mut!(KEY_MEMO) = None };
}

/// The built request path for `(srv, path, w, h, png)` — the store key — memoised. `None` for an
/// unknown server, an empty path, or a path the slot array cannot hold ([`KEY_MAX`]: a path that
/// cannot survive the round trip is worse than no path, since the stored key would never equal
/// the probe that built it and every frame would claim a fresh slot and evict a real poster).
/// MAIN thread; borrows the memo, so the key is consumed before the next call.
///
/// Headroom today, worth stating so a future shape can be judged against it: a headshot key
/// measures ~177 bytes of the 255 (54 fixed + 89 encoded URL + 34 of token), an ordinary relative
/// thumb ~140.
fn built_key(srv: ServerId, path: &str, w: c_int, h: c_int, png: bool) -> Option<&'static str> {
    if path.is_empty() {
        return None;
    }
    let c = crate::plex::client_for(srv)?;
    // SAFETY: main-thread only (every caller is a draw path), and the borrow is consumed by the
    // caller before the memo can be touched again; the `'static` is that discipline, not a fact.
    let memo = unsafe {
        (*std::ptr::addr_of_mut!(KEY_MEMO)).get_or_insert_with(|| KeyMemo {
            map: std::collections::HashMap::new(),
        })
    };
    let s = memo.get_or_build(srv.raw(), path, w, h, png, c.token_gen(), || {
        // Supersampled simulator renders (`surface::render_scale`, 1 on a television) ask the
        // server for the pixels they will draw; the store key stays the logical box.
        let n = crate::surface::render_scale() as i64;
        c.image_transcode_path(path, w as i64 * n, h as i64 * n, png)
    });
    if s.len() > KEY_MAX {
        warn_key_refused(s.len());
        return None;
    }
    Some(s)
}

/// A refused key logs ONCE per process, with both ceilings and the length that missed them.
///
/// The latch is the whole design. This is a per-tile, per-frame path and the memo means a refusal
/// REPEATS every frame, so an unlatched log would bury `/tmp/plxnative-events.log` — but silence
/// is the failure `paths.rs` was fixed for (a font fell through to DroidSans while `init_text`
/// still logged `ok=1`), and a silent refusal here is a tile that is a skeleton forever with
/// nothing in the one file an issue report is asked for. Once is enough to name the cause.
fn warn_key_refused(len: usize) {
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::Relaxed) {
        crate::log(&format!(
            "posters: REFUSED art key of {len} bytes (slot holds {KEY_MAX}) - tile stays a skeleton"
        ));
    }
}

/// Process-wide counts of the two halves of issue #107's fix: a `P_READY` slot the render cache
/// could no longer keep resident (`PosterSource::unresident` demoting it to `P_EVICTED`), and a
/// later demand re-arming that dormant slot back to `P_WANT` (the `P_EVICTED` branch of
/// [`lookup`]). Both increment on every real transition, never on a call that finds the slot
/// already past it, so the two numbers are an exact tally, not a sample.
static RESIDENCY_LOST: AtomicU64 = AtomicU64::new(0);
static RESIDENCY_REARMED: AtomicU64 = AtomicU64::new(0);

/// The interval throttle's clock, and the `(lost, rearmed)` pair the LAST emitted line actually
/// reported — the second half is what lets [`log_residency_settled`] tell "nothing changed since
/// we last said so" from "the totals moved and the throttle window swallowed it".
struct ResidencyLog {
    at: Option<std::time::Instant>,
    lost: u64,
    rearmed: u64,
}
static RESIDENCY_LOG: Mutex<ResidencyLog> = Mutex::new(ResidencyLog { at: None, lost: 0, rearmed: 0 });

/// Pure half of the interval throttle in [`log_residency`] — is a transition-triggered line due,
/// given the instant of the last one actually written (`None` before the first) and now? Split
/// out so the throttle's edges (first call always due, the instant it reopens) are host-testable
/// without a real Mutex or a real elapsed second.
fn interval_due(last: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    match last {
        None => true,
        Some(t) => now.duration_since(t) >= Duration::from_secs(1),
    }
}

/// The only place these two counts reach the event log, for the same reason issue #107 itself was
/// invisible: the route heartbeat's `evicted_hot=` counts HOT evictions only (a key wanted again in
/// the same frame), so on-device verification of the fix could report "I did not see a skeleton"
/// and nothing more — never whether eviction fired at all, or whether a fired eviction was
/// followed by a working re-arm. Those are two different failures a screenshot cannot tell apart,
/// and this line is what lets a future field report on #107 tell them apart after the fact.
///
/// Throttled for the reason [`warn_key_refused`] gives for its own latch: `unresident` and the
/// re-arm branch are both per-tile, per-frame paths, and a fast scroll evicts dozens of keys a
/// second, so a line per transition would bury `/tmp/plxnative-events.log`. Unlike that latch this
/// does not go silent after one line — a long session should keep producing fresh readings — so it
/// is a minimum-interval throttle (about one second, monotonic so a skewed wall clock never lies
/// about it) rather than a one-shot. The first call in a process is let through unthrottled, so a
/// short session still leaves evidence.
///
/// **The throttle bounds the LINE, not the truth**: the two atomics above are incremented on
/// every real transition regardless of whether a call here emits, so the TOTALS are always exact
/// — but a lone eviction whose re-arm lands inside the same one-second window used to leave the
/// re-arm permanently unwritten, because nothing ever asked again. A device session that captured
/// exactly `lost=1 rearmed=0` because the fix's own recovery path completed in under a second is
/// indistinguishable, on that reading alone, from a re-arm that never fires at all — which is the
/// one distinction this instrument exists to make. [`log_residency_settled`] is the other half:
/// called once a frame, it bypasses this interval outright and emits whenever the totals have
/// moved since the last line, which is what guarantees the final state always lands even when
/// every individual transition inside a burst was throttled away.
fn log_residency() {
    let lost = RESIDENCY_LOST.load(Ordering::Relaxed);
    let rearmed = RESIDENCY_REARMED.load(Ordering::Relaxed);
    let now = std::time::Instant::now();
    let mut st = RESIDENCY_LOG.lock().unwrap_or_else(|e| e.into_inner());
    if !interval_due(st.at, now) {
        return;
    }
    st.at = Some(now);
    st.lost = lost;
    st.rearmed = rearmed;
    drop(st);
    crate::log(&format!("posters: residency lost={lost} rearmed={rearmed}"));
}

/// The settle half of the instrument (see [`log_residency`]'s doc for the gap it closes): called
/// once a frame from [`begin_frame`], unconditionally, for every screen — not from
/// `PosterSource::idle`, which reaches this module only through `screens/home/mod.rs`'s prefetch
/// gate (`prefetch_armed(..) && source_idle()`) and would therefore never fire for an eviction
/// burst on Detail or a library grid, nor for a Home frame where prefetch happens to be disarmed.
/// `begin_frame` already runs first, every frame, on the main thread, regardless of screen — the
/// same unconditional seam `invalidate_due_retries` uses for its own end-of-frame housekeeping.
///
/// Emits only when the store this frame found QUIET (nothing `P_WANT`/`P_LOADING`/`P_DECODED`)
/// AND the totals moved since the last line actually written: a settle event with nothing new to
/// say costs nothing, and re-checking every frame is cheap because the caller already pays for
/// the same slot scan. Bypasses [`interval_due`] outright — a settle event is by definition rare
/// enough (bounded by bursts, not by transitions) that it does not need the throttle bursts do.
fn log_residency_settled(store_idle_this_frame: bool) {
    if !store_idle_this_frame {
        return;
    }
    let lost = RESIDENCY_LOST.load(Ordering::Relaxed);
    let rearmed = RESIDENCY_REARMED.load(Ordering::Relaxed);
    let mut st = RESIDENCY_LOG.lock().unwrap_or_else(|e| e.into_inner());
    if st.lost == lost && st.rearmed == rearmed {
        return;
    }
    st.at = Some(std::time::Instant::now());
    st.lost = lost;
    st.rearmed = rearmed;
    drop(st);
    crate::log(&format!("posters: residency lost={lost} rearmed={rearmed}"));
}

/// Test-visible read of the two totals above, so a test grades the counters through the same
/// atomics [`log_residency`] reads rather than a reimplementation of them.
#[cfg(test)]
fn residency_counts_for_test() -> (u64, u64) {
    (
        RESIDENCY_LOST.load(Ordering::Relaxed),
        RESIDENCY_REARMED.load(Ordering::Relaxed),
    )
}

/// Test-visible read of the `(lost, rearmed)` pair the last emitted line actually reported, so a
/// test can grade [`log_residency_settled`]'s "did the log line's content change" decision
/// directly, the same way [`residency_counts_for_test`] grades the underlying totals.
#[cfg(test)]
fn residency_last_emitted_for_test() -> (u64, u64) {
    let st = RESIDENCY_LOG.lock().unwrap_or_else(|e| e.into_inner());
    (st.lost, st.rearmed)
}

/// Force the interval throttle's clock back to "no line written yet", so a test's first call
/// through [`log_residency`] is deterministically unthrottled regardless of what an earlier test
/// (serialized the same way, through [`crate::testlock::serial`]) wrote a moment before. Leaves
/// the process-wide totals alone — those are graded by delta, exactly as [`reset_key_memo`]
/// leaves `plex::reset_servers_for_test` to the registry it resets.
#[cfg(test)]
fn reset_residency_log_for_test() {
    let mut st = RESIDENCY_LOG.lock().unwrap_or_else(|e| e.into_inner());
    st.at = None;
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
fn logo_key(srv: ServerId, rk: &str) -> Option<&'static str> {
    if rk.is_empty() {
        return None;
    }
    built_key(srv, &format!("/library/metadata/{rk}/clearLogo"), LOGO_REQ_W, LOGO_REQ_H, true)
}

/// The prefetch twin of [`logo_src`] — starts the same fetch, takes no texture and no LRU
/// protection. A hero page whose logo misses draws its title as HERO TEXT and then pops to the
/// logotype the moment it lands; warming kills that pop the same way it kills the backdrop's.
fn logo_warm(srv: ServerId, rk: &str) -> Warm {
    logo_key(srv, rk)
        .map(|k| lookup(srv, k, Touch::Warm).1)
        .unwrap_or(Warm::Known)
}

/// An item's clearLogo (transparent PNG) as a cache key once its pixels are in — `None` while
/// pending OR when the item has no logo (the store cannot tell those two apart — which is why the
/// text→logo swap is still a cut, see [`crate::ui::hero_logo`]). `ui::tex::logo_src` adds the
/// texture and its TRUE PIXEL SIZE.
///
/// The ONE clearLogo resolve (home hero, detail hero, detail compact title all draw through it).
/// How big it is DRAWN is a UI decision and lives in [`crate::ui::hero_logo::fit`]: this layer used
/// to take a `max_w`×`max_h` box and contain-fit into it, which is how the same mark ended up three
/// sizes in one app — the store never owned layout policy, it only looked as if it did.
///
/// NB it claims a slot even for an item that HAS no logo — the 404 lands as `P_FAILED` and holds it
/// — so every hero page costs two of the store's [`PT_CAP`] slots, backdrop plus logo.
fn logo_probe(srv: ServerId, rk: &str) -> Option<PosterKey> {
    lookup(srv, logo_key(srv, rk)?, Touch::Draw).0
}

/// The slot a miss claims, as a PURE function of what the store looks like: the first EMPTY, else
/// the least-recently-used SETTLED (`P_READY`/`P_EVICTED`/`P_FAILED`/`P_RETRY`) slot the current
/// frame has not touched, else `None` — "everything is either in flight or on screen; skip this
/// request".
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
        if (s.state == P_READY || s.state == P_FAILED || s.state == P_RETRY || s.state == P_EVICTED)
            && s.frame != frame
            && s.use_ < oldest
        {
            oldest = s.use_;
            pick = Some(i);
        }
    }
    pick
}

/// What one store probe found: the slot's key once its pixels have been handed to the render
/// cache (READY), else `None` while the slot is empty, in flight, or failed. The texture and its
/// decoded size are the cache's answer for that key.
type Hit = Option<PosterKey>;

/// MAIN thread. The one store lookup behind [`poster_get`], [`poster_get_wh`] and [`poster_warm`]:
/// hit → the READY texture + its size; miss → claim a slot and enqueue the fetch, IF the request is
/// admitted. Three things decide, and `touch` is only the first: `touch` itself (LRU bookkeeping on
/// every hit, and whether a P_EVICTED slot is eligible to recover at all), the speculation bound
/// [`warm_admissible`] for a Warm, and the drawn card's placement scope
/// ([`crate::ui::card_motion::declines_request`]) for a Draw. Unknown or moving card
/// placement declines new work; featured images outside that scope remain eligible.
/// A refusal claims no source slot; a follow-up present samples placement again.
fn lookup(srv: ServerId, key_s: &str, touch: Touch) -> (Hit, Warm) {
    // The array's own precondition, checked before a slot can be claimed for a key that could
    // never match its probe again. `Warm::Known` (not `Full`) so a prefetch loop walks on to the
    // next candidate instead of retiring this frame's one warm here.
    if !is_fetchable(key_s) {
        return (None, Warm::Known);
    }
    // A scoped card DRAW may not START work with unknown or fast placement, at
    // EVERY point work begins — the fresh miss far below, but equally the two re-arm transitions
    // inside the matching-slot loop. Those matter more than the miss rather than less: a fast
    // scroll is exactly what evicts textures, so scrolling back across the same rows finds
    // P_EVICTED slots, and re-arming each one is a full fetch, decode and upload that the gate
    // would never see if it only guarded the miss. A HIT is deliberately untouched: art that is
    // already resident still draws and still takes its LRU touch, so declining never blanks a
    // tile that had its picture.
    let decline = touch == Touch::Draw && crate::ui::card_motion::declines_request();
    let mut g = store();
    // hit?
    for i in 0..PT_CAP {
        if slot_matches(&g.slots[i], srv, key_s.as_bytes()) {
            if touch == Touch::Draw {
                g.clock = g.clock.wrapping_add(1);
                let (c, f) = (g.clock, g.frame);
                g.slots[i].use_ = c;
                g.slots[i].frame = f;
                // Promotion is monotone: a warm never demotes a key a draw is waiting on. If it
                // is still queued, the next worker rescan will now choose it before speculation.
                g.slots[i].visible = true;
                if g.slots[i].state == P_WANT { CV.notify_one(); }
            }
            // a parked transient failure is SCHEDULED by the first draw that finds it, and goes
            // back in the queue by the first draw after its wait — on a DRAW only, so a tile
            // nobody is looking at does not keep dialling a server that is down
            if touch == Touch::Draw && g.slots[i].state == P_RETRY {
                let now = crate::app::clock::now();
                if g.slots[i].retry_at.is_none() {
                    let wait = retry_backoff(g.slots[i].attempts).as_millis() as u32;
                    g.slots[i].retry_at = Some(now.wrapping_add(wait));
                    g.slots[i].retry_wake_sent = false;
                } else if retry_due(&g.slots[i], now) {
                    if decline {
                        crate::ui::card_motion::deferred();
                        return (None, Warm::Known);
                    }
                    g.slots[i].state = P_WANT;
                    g.slots[i].retry_at = None;
                    g.slots[i].retry_wake_sent = false;
                    drop(g);
                    CV.notify_one();
                    return (None, Warm::Known);
                }
            }
            // Render-cache pressure is not source demand: its callback parks the slot in EVICTED,
            // and mere retention in the source LRU schedules no work. Only a Draw probe may even
            // consider re-arming an existing EVICTED slot; Warm leaves it dormant unconditionally,
            // below, before either of the DRAW-only mechanisms that follow ever run. This prevents
            // speculative resurrection of that slot, but does not prevent cycling when the drawn
            // working set exceeds the residency budget — that remaining DRAW-vs-DRAW case is what
            // the cooldown gate right after this one bounds (see `evict_was_rapid`'s doc): a key's
            // first eviction always re-arms on its very next draw (no cooldown was set for it),
            // but a key that is being re-evicted rapidly — genuine byte pressure the store cannot
            // resolve by itself — waits out a bounded backoff instead of re-arming on every single
            // draw, which is what let two over-budget on-screen keys evict each other forever.
            if g.slots[i].state == P_EVICTED {
                if touch == Touch::Warm {
                    return (None, Warm::Known);
                }
                let now = crate::app::clock::now();
                if !evict_cooldown_due(g.slots[i].evict_cooldown_until, now) {
                    // Still cooling down from a rapid re-eviction. The demand is real and is not
                    // lost — the next probe after the cooldown clears re-arms exactly as usual —
                    // but honoring THIS one would undo the rate limit the cooldown exists to
                    // enforce. It does not stop the cycle: under sustained pressure the key can be
                    // evicted and cooled down again the moment it lands, indefinitely — it only
                    // paces how often that cycle is allowed to turn.
                    return (None, Warm::Known);
                }
                if decline {
                    crate::ui::card_motion::deferred();
                    // The same deferral the cooldown just made, for the same reason: the slot
                    // stays EVICTED and the first draw at a settled speed re-arms it as usual.
                    return (None, Warm::Known);
                }
                g.slots[i].state = P_WANT;
                g.slots[i].evict_wake_sent = false;
                drop(g);
                RESIDENCY_REARMED.fetch_add(1, Ordering::Relaxed);
                log_residency();
                CV.notify_one();
                return (None, Warm::Known);
            }
            let hit = (g.slots[i].state == P_READY).then_some(PosterKey(i as u32));
            return (hit, Warm::Known);
        }
    }
    // Admit speculation only into a quiet visible queue, and never enough of it to occupy both
    // workers. `Full` tells the caller this frame's one prefetch attempt is spent without claiming
    // a slot.
    if touch == Touch::Warm && !warm_admissible(&g.slots) {
        return (None, Warm::Full);
    }
    // A MISS while the document is scrolling fast claims nothing at all — no slot, no worker,
    // no fetch, no decode, no upload. The tile draws its placeholder and asks again next frame;
    // the demand is deferred, never dropped, exactly like the eviction cooldown above. Art
    // resolves as the view slows, which is also the reference clients' idiom.
    //
    // **Why the request is declined outright rather than merely paced.** Every weaker bound was
    // measured on the dev set against a 1000-movie mock library (`fps:library-scroll`, panel
    // off, two runs each, dropped frames after warmup):
    //
    // |                                                     | drops      | median fps |
    // |-----------------------------------------------------|------------|------------|
    // | a9d4e8a2, no gate                                    | 90, 97     | 55         |
    // | slot + cache caps 64 -> 160                          | 226, 231   | 50         |
    // | rate-limit the uploads after decode                  | 172, 156   | 53, 55     |
    // | allow ONE visible request in flight while scrolling  | 204, 196   | 55, 54     |
    // | **decline the request outright**                     | **21, 23** | **60**     |
    //
    // One in flight is the informative row: it paced the pipeline perfectly — `budget=` read
    // 15-18 admitted and ZERO refused, against the ungated 15-18 admitted and 11-15 refused —
    // and still cost twice the baseline's dropped frames. Pacing the work does not make it
    // affordable. The two downstream bounds were worse than doing nothing at all, because by
    // then the fetch and the decode are already paid for and the refused pixels only accumulate
    // in `TexCache::pending` at 375 KB each. The request is the last point at which this work
    // can still be declined for free.
    if decline {
        crate::ui::card_motion::deferred();
        return (None, Warm::Full);
    }
    // miss: prefer EMPTY, else LRU-evict a settled slot not used this frame
    let idx = match victim(&g.slots, g.frame) {
        Some(i) => i,
        None => return (None, Warm::Full), // all visible: skip
    };
    let (was_ready, old_px) = (g.slots[idx].state == P_READY, g.slots[idx].px);
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
        s.px = 0;
        s.gen = s.gen.wrapping_add(1);
        set_key(s, key_s);
        s.srv = srv; // captured HERE, on the main thread — the worker asks no one which server
        s.state = P_WANT;
        s.visible = touch == Touch::Draw;
        s.use_ = use_;
        s.frame = frame;
        s.pw = 0;
        s.ph = 0;
        s.retry_at = None;
        s.retry_wake_sent = false;
        s.attempts = 0;
        // A recycled slot is a DIFFERENT key's identity now — the eviction-thrash history above
        // belongs to whatever image used to live here, not to this one.
        s.evicted_at = None;
        s.evict_attempts = 0;
        s.evict_cooldown_until = None;
        s.evict_wake_sent = false;
    }
    drop(g);
    // free the evicted resources off-lock (this is the GL/main thread): the cache's texture for
    // the recycled slot, and pixels a worker decoded that nobody drained
    if was_ready {
        tex::free(PosterKey(idx as u32), &mut GfxUploader);
    }
    if old_px != 0 {
        img::img_free(old_px as *mut c_uchar);
    }
    CV.notify_one();
    (None, Warm::Claimed)
}

/// The [`tex::Source`] this module is to the library: one value, installed at [`init`].
struct PosterSource;
static SOURCE: PosterSource = PosterSource;

impl tex::Source for PosterSource {
    fn probe(&self, srv: u16, path: &str, w: i32, h: i32, png: bool) -> Option<PosterKey> {
        let srv = ServerId::from_raw(srv);
        lookup(srv, built_key(srv, path, w, h, png)?, Touch::Draw).0
    }
    /// The prefetch: start the fetch, take no key, take no LRU protection. Two deliberate
    /// differences from a draw's probe, and together they are why a prefetch can share a 64-slot
    /// store with everything on screen: it never stamps `frame` (no evict-protection) and never
    /// bumps `use_` (LRU age 0, permanently the first victim). A key that cannot be built answers
    /// `Known`, so a prefetch loop walks on to the next candidate instead of retiring this
    /// frame's one warm on a key that can never resolve.
    fn warm(&self, srv: u16, path: &str, w: i32, h: i32, png: bool) -> Warm {
        let srv = ServerId::from_raw(srv);
        match built_key(srv, path, w, h, png) {
            Some(k) => lookup(srv, k, Touch::Warm).1,
            None => Warm::Known,
        }
    }
    fn logo(&self, srv: u16, rk: &str) -> Option<PosterKey> {
        logo_probe(ServerId::from_raw(srv), rk)
    }
    fn logo_warm(&self, srv: u16, rk: &str) -> Warm {
        logo_warm(ServerId::from_raw(srv), rk)
    }
    fn unresident(&self, key: PosterKey) {
        let mut g = store();
        let mut lost = false;
        if let Some(s) = g.slots.get_mut(key.0 as usize) {
            // READY is the only state whose truth depends on render-cache residency. The cache
            // calls this synchronously for both a rejected decoded result and pressure eviction,
            // before application code can probe or recycle the key. Thus the source never answers
            // READY for a key the cache could not make resident.
            if s.state == P_READY {
                s.state = P_EVICTED;
                lost = true;
                // The thrash guard's bookkeeping (see `evict_was_rapid`'s doc): a rapid re-eviction
                // (this key was evicted before, inside the thrash window) escalates the backoff
                // `lookup`'s P_EVICTED branch will honor on the next probe; an isolated one leaves
                // no cooldown at all, so the very next Draw probe re-arms it as before.
                let now = crate::app::clock::now();
                s.evict_attempts = if evict_was_rapid(s.evicted_at, now) {
                    s.evict_attempts.saturating_add(1)
                } else {
                    0
                };
                s.evicted_at = Some(now);
                s.evict_cooldown_until = (s.evict_attempts > 0)
                    .then(|| now.wrapping_add(evict_backoff(s.evict_attempts).as_millis() as u32));
                // A fresh eviction is a fresh deadline (or none at all) to wake for — whatever an
                // earlier cooldown's expiry already latched no longer applies.
                s.evict_wake_sent = false;
            }
        }
        drop(g);
        if lost {
            RESIDENCY_LOST.fetch_add(1, Ordering::Relaxed);
            log_residency();
        }
    }
    fn idle(&self) -> bool {
        store_idle()
    }
}

/// Pure half of [`store_idle`] — split so the gate is host-testable without mutating the store
/// singleton.
fn idle_of(slots: &[Pslot; PT_CAP]) -> bool {
    !slots
        .iter()
        .any(|s| matches!(s.state, P_WANT | P_LOADING | P_DECODED))
}

/// The next queued slot a worker claims. Kept pure so queue ordering is host-testable.
fn next_wanted(slots: &[Pslot; PT_CAP]) -> Option<usize> {
    #[cfg(test)]
    if std::env::var_os("PLX_TEST_FIFO_POSTER_QUEUE").is_some() {
        return (0..PT_CAP).find(|&i| slots[i].state == P_WANT);
    }
    (0..PT_CAP)
        .find(|&i| slots[i].state == P_WANT && slots[i].visible)
        .or_else(|| (0..PT_CAP).find(|&i| slots[i].state == P_WANT))
}

#[inline]
fn outstanding(s: &Pslot) -> bool {
    matches!(s.state, P_WANT | P_LOADING | P_DECODED)
}

/// Whether one speculative claim can enter without starving the visible set.
fn warm_admissible(slots: &[Pslot; PT_CAP]) -> bool {
    if slots.iter().any(|s| outstanding(s) && s.visible) {
        return false;
    }
    slots.iter().filter(|s| outstanding(s) && !s.visible).count() < PREFETCH_OUTSTANDING_MAX
}

/// MAIN thread. Is the store QUIET — nothing wanted, fetching, or waiting to upload? Hero warming
/// uses this conservative whole-pipeline gate. Other speculative warms (an up-next episode
/// thumbnail, a hero logo) are bounded separately by [`warm_admissible`] and ordered by
/// [`next_wanted`].
///
/// Cost: one 64-slot scan per frame under the mutex — the draw already does dozens.
fn store_idle() -> bool {
    idle_of(&store().slots)
}

// (The old standalone `poster_wh` — a second lock + 64-slot key scan to read two ints about a slot
// the caller had just probed — is gone: `lookup` hands the size back with the texture, so a
// cover-fitted tile now costs exactly what a stretched one does. Both of its callers, `logo_src` and
// `ui::widgets::resolve_tex_wh`, go through `poster_get_wh`.)

/// MAIN thread, once per frame, first: a new frame — nothing is "touched" yet (evict-protection
/// is per frame, see [`victim`]). Also where the residency instrument's settle snapshot is
/// checked (see [`log_residency_settled`]'s doc for why THIS seam and not `PosterSource::idle`):
/// the slot scan below is the same one [`idle_of`] would run, so this is the store's one
/// once-a-frame, screen-agnostic read of whether it is quiet.
pub(crate) fn begin_frame() {
    let now = crate::app::clock::now();
    let mut g = store();
    g.frame = g.frame.wrapping_add(1);
    invalidate_due_retries(&mut g.slots, now);
    invalidate_due_evictions(&mut g.slots, now);
    let settled = idle_of(&g.slots);
    drop(g);
    log_residency_settled(settled);
}

/// MAIN thread, once per frame (§3.3 step 3, the adapter's results): every slot a worker has
/// DECODED hands its pixels to the render cache as one `PosterReady` and becomes READY. No GL
/// here — the cache uploads in PREPARE ([`prepare`]). The pixels are copied once out of the
/// decoder's C allocation into the owned `Decoded` (a 250x375 poster is ~375 KB, tens of
/// microseconds) so the library owns what it uploads.
pub(crate) fn drain_decoded() {
    loop {
        let (idx, px, w, h) = {
            let mut g = store();
            let Some(i) = (0..PT_CAP).find(|&i| g.slots[i].state == P_DECODED) else {
                break;
            };
            let s = &mut g.slots[i];
            let (px, w, h) = (s.px, s.pw, s.ph);
            s.px = 0;
            s.state = P_READY;
            (i, px, w, h)
        };
        let key = PosterKey(idx as u32);
        let result = if px != 0 && w > 0 && h > 0 && w <= u16::MAX as c_int && h <= u16::MAX as c_int {
            let n = (w as usize) * (h as usize) * 4;
            // SAFETY: the worker decoded exactly w*h RGBA bytes at `px` (img::img_decode_rgba's
            // contract) and handed the pointer over under the lock; it is freed right below.
            let rgba: Box<[u8]> = unsafe { std::slice::from_raw_parts(px as *const u8, n) }.into();
            Ok(Decoded {
                w: w as u16,
                h: h as u16,
                rgba,
            })
        } else {
            Err(PosterError::Decode)
        };
        if px != 0 {
            img::img_free(px as *mut c_uchar);
        }
        tex::accept(PosterReady { key, result });
    }
}

/// The GL half behind the cache, on the main thread: `img::img_upload_rgba` (a synchronous
/// glTexImage2D, counted for the frame-drop detector), `gfx::warm_tex` (resident NOW rather than
/// on the next draw that samples it — see that fn for the 116 ms frame it moves out of the draw)
/// and `gfx::delete_tex`.
pub(crate) struct GfxUploader;

impl Uploader for GfxUploader {
    fn upload(&mut self, d: &Decoded) -> Tex {
        let id = img::img_upload_rgba(d.rgba.as_ptr(), d.w as c_int, d.h as c_int);
        UP_CT.fetch_add(1, Ordering::Relaxed);
        UP_PX.fetch_add((d.w as u64) * (d.h as u64), Ordering::Relaxed);
        Tex {
            id,
            w: d.w,
            h: d.h,
        }
    }
    fn warm(&mut self, t: Tex) {
        if t.id != 0 {
            crate::gfx::warm_tex(t.id);
        }
    }
    fn free(&mut self, t: Tex) {
        if t.id != 0 {
            crate::gfx::delete_tex(t.id);
        }
    }
}

/// MAIN/GL thread, once per PRESENTING frame (§3.3 step 9): upload what the cache holds pending,
/// under the frame budget — the `Poster` class for an ordinary poster, the solo `Residency` class
/// for a backdrop or a hero logo (`ui::tex`'s `RESIDENCY_BYTES`).
///
/// **The frame this runs on has already been decided to present**, because the budget's
/// queued-work term is the second half of that decision (`app/run.rs`, §3.3 step 8). So what
/// `idle::invalidate` marks here is not this frame but the NEXT one, and it is still worth its
/// frame: a landed texture is DISCRETE damage, which is exactly what `idle::present_dirty`'s
/// readers — the glass hosts deciding whether to re-snapshot the page under them — are asking
/// about, and no spring reports it. The cache's own `Provenance::Resource` note is the same
/// statement to the phase-2 `Present` machine.
pub(crate) fn prepare(
    b: &mut crate::ui::frame::Budget,
    present: &mut crate::ui::machine::PresentHandle<'_>,
    now_us: impl Fn() -> u64,
) -> usize {
    let n = tex::prepare(b, &mut GfxUploader, present, now_us);
    if n > 0 {
        crate::ui::idle::invalidate();
    }
    n
}

/// Why the worker got no bytes to decode. Three arms rather than one flag because they send the
/// reader to three different places: an unresolved server is registry state, no usable response is
/// the network or the token, and a 2xx with nothing in it is an answer that arrived and was empty.
#[derive(Clone, Copy)]
enum ArtFail {
    /// `plex::client_for` answered `None` — the slot names a server the registry does not resolve
    /// (never registered, or below the floor a sign-out raised). No request was made.
    NoServer,
    /// The request produced no usable response, and this layer cannot take that apart any further:
    /// `stream::http_open` returns -1 — and so `http_get` returns `None` — for a refused or
    /// timed-out connect, a peer that closes mid-header, AND any status outside 200–299 alike.
    ///
    /// So this arm covers a 401 from a revoked token and a 404 for art that does not exist with
    /// one word, and the second of those is not always a fault: [`logo_src`] asks for a `clearLogo`
    /// an item may not have and reads the refusal as "there is none" (its own note — "the 404 lands
    /// as `P_FAILED` and holds it"). A library holding one such item spends this arm's single line
    /// on it. Read the line as "at least one art request on this server came back with nothing",
    /// which is exactly what it says.
    NoResponse,
    /// A 2xx that carried zero bytes: something answered, and the answer held no picture. Split
    /// from [`ArtFail::NoResponse`] because the request plainly reached a server that accepted it,
    /// which rules out the address, the route and the token in one line.
    Empty,
}

impl ArtFail {
    /// The half of the log line that names the cause. Prose rather than a code, because the reader
    /// is whoever opened `/tmp/plxnative-events.log` after being told artwork does not load.
    fn why(self) -> &'static str {
        match self {
            ArtFail::NoServer => {
                "the registry does not resolve this server (never registered, or revoked)"
            }
            ArtFail::NoResponse => {
                "no usable response - refused/timed-out connect, or a status outside 2xx"
            }
            ArtFail::Empty => "the server answered 2xx with an empty body",
        }
    }
}

/// A fetch that produced no bytes logs ONCE per (cause, server), then goes quiet.
///
/// **Why it is logged at all.** `img::img_decode_rgba` reports a decode that failed (`img:
/// decode-none …`), but that runs only once bytes have ARRIVED; the ways of arriving with NONE are
/// this module's to speak for. And what it does about them is otherwise invisible: a permanent
/// answer parks at `P_FAILED`, while a transient one parks at `P_RETRY` until a visible draw's
/// bounded backoff expires. A transport layer can only speak for one request; the STORE's final-or-
/// retry decision is this layer's to say — and a screen of skeletons with nothing in the event log
/// naming them is the silence `paths.rs` was fixed for, where a font fell through to DroidSans
/// while `init_text` still logged `ok=1`.
///
/// **Why it is latched**, exactly as [`warn_key_refused`] is: this is a per-SLOT path and a grid
/// claims dozens of them at once, so one line per failure would be dozens per screen, and a log
/// that scrolls itself away is as useless as a silent one.
///
/// **Why the SERVER is half the key.** With a friend's share registered beside our own, the
/// interesting failure is the asymmetric one — ours answers and the share does not, because it is
/// asleep or its token was revoked — and a cause-only latch would spend its single line on
/// whichever failed first and hide the other for the rest of the process. [`crate::plex::MAX_SERVERS`]
/// is 16, so the extra dimension is one `u32` of bits per slot.
///
/// **What is deliberately NOT in the line: the key.** It is a `/photo/:/transcode?…` path ending in
/// `&X-Plex-Token=…` (see [`poster_key`], and the test that pins the token to the end of it), and
/// `crate::redact_tokens`' own doc states the policy that backstop exists to make redundant — no
/// call site formats a URL into a log line in the first place. The server is named by its registry
/// SLOT NUMBER, the handle `plex: server slot N registered at …` already prints, and by nothing
/// else: not the address, not the machine identifier, not the friendly name (which defaults to the
/// owner's hostname). `app/diagnostics.rs`'s module doc argues the whole rule for the on-screen read-out.
fn warn_fetch_failed(srv: ServerId, cause: ArtFail) {
    // One word per server slot, one BIT per cause. Indexing by the SERVER (clamped) is what keeps
    // the index in range by construction rather than by assumption: `ServerId::UNSET` is
    // `u16::MAX`, and the three store entry points take a raw key from any caller, so a slot's id
    // is not guaranteed to name a registry entry. Everything that does not name one shares the
    // last word.
    const NWORD: usize = crate::plex::MAX_SERVERS + 1;
    static LOGGED: [AtomicU32; NWORD] = [const { AtomicU32::new(0) }; NWORD];
    let bit = 1u32 << cause as u32;
    let word = &LOGGED[(srv.raw() as usize).min(crate::plex::MAX_SERVERS)];
    // fetch_or, not load-then-store: this loop runs on more than one thread (`posters_init` spawns
    // two), so two workers can reach the same cause for the same server at once and only
    // the one that flipped the bit may write the line.
    if word.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
        crate::log(&format!(
            "posters: art fetch FAILED on server {} - {} (permanent responses are final; transient failures retry with backoff while the tile is on screen; further ones like this are silent)",
            srv.raw(),
            cause.why()
        ));
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
                if let Some(i) = next_wanted(&g.slots) {
                    break i;
                }
                let res = CV.wait_timeout(g, Duration::from_millis(50));
                g = res
                    .map(|(guard, _)| guard)
                    .unwrap_or_else(|e| e.into_inner().0);
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
        //
        // The three ways to reach the decode with NOTHING are split apart only to be REPORTED —
        // `warn_fetch_failed` carries the argument for why this module owes the log a line. Nothing
        // about the outcome moves: each of them yields the same null pointer the folded `_ =>` arm
        // did, and the state transition below is untouched.
        let mut w = 0i32;
        let mut h = 0i32;
        // A DURABLE class of art (today: profile avatars, which the server proxies from plex.tv)
        // also lives in `crate::imgcache`, and the FILE WINS whenever there is one: the key
        // carries plex.tv's own cache-buster, so a cached file is current for its key by
        // construction and there is nothing a fetch could refresh. Going to the network first
        // was the 2026-09-06 picker with no faces: offline, the proxy's request to plex.tv
        // hangs for the whole transfer budget, both workers sat on avatars, and the tiles
        // stayed skeletons until long after the pick. On a miss the fetch runs as before, and
        // the bytes are written only AFTER they decode — a proxy answering 2xx with something
        // that is not the picture must never overwrite a good file.
        let disk = crate::imgcache::classify(&key_s);
        let cache_gen = crate::imgcache::generation();
        let mut fetched: Option<Vec<u8>> = None;
        let mut px = std::ptr::null_mut();
        // A stale cached file is refreshed AFTER the picture is published (below), and only
        // into the file — never on this worker's critical path, and never as a texture swap.
        let mut refresh_after: Option<crate::imgcache::DiskKey> = None;
        if let Some(k) = &disk {
            if let Some(b) = crate::imgcache::read(k) {
                px = img::img_decode_rgba(b.as_ptr(), b.len() as c_int, &mut w, &mut h);
                if px.is_null() {
                    // a file that no longer decodes is retired, and the fetch below replaces it
                    crate::imgcache::remove(k);
                } else if crate::imgcache::age(k).is_some_and(|a| a > crate::imgcache::REFRESH_AFTER)
                    && crate::plex::account::plex_tv_recently_reachable()
                {
                    refresh_after = Some(k.clone());
                }
            }
        }
        // `transient` is what a null `px` means: park and retry (P_RETRY), or give up (P_FAILED).
        let (px, transient) = if !px.is_null() {
            (px, false)
        } else {
            match crate::plex::client_for(srv) {
                Some(c) => match c.fetch_built_outcome(&key_s) {
                    // bytes arrived: from here on a failure is the decoder's, and `img.rs` logs it
                    crate::plex::ArtFetch::Bytes(b) if !b.is_empty() => {
                        let px =
                            img::img_decode_rgba(b.as_ptr(), b.len() as c_int, &mut w, &mut h);
                        if !px.is_null() && disk.is_some() {
                            fetched = Some(b);
                        }
                        (px, false)
                    }
                    crate::plex::ArtFetch::Bytes(_) => {
                        warn_fetch_failed(srv, ArtFail::Empty);
                        (std::ptr::null_mut(), true)
                    }
                    outcome @ crate::plex::ArtFetch::Status(_) => {
                        warn_fetch_failed(srv, ArtFail::NoResponse);
                        (std::ptr::null_mut(), is_transient(&outcome))
                    }
                    crate::plex::ArtFetch::NoResponse => {
                        warn_fetch_failed(srv, ArtFail::NoResponse);
                        (std::ptr::null_mut(), true)
                    }
                },
                // no client for this id YET — a re-point in progress publishes the new one a
                // moment later, which is the same window the plaintext refusal opens
                None => {
                    warn_fetch_failed(srv, ArtFail::NoServer);
                    (std::ptr::null_mut(), true)
                }
            }
        };

        if let (Some(k), Some(b)) = (&disk, &fetched) {
            // gated on the generation read before the fetch: a sign-out in between means these
            // bytes belong to an account that has left, and they are dropped
            crate::imgcache::write_at(cache_gen, k, b);
        }

        let mut g = store();
        let s = &mut g.slots[idx];
        if s.gen == gen && s.state == P_LOADING {
            if !px.is_null() {
                s.px = px as usize;
                s.pw = w;
                s.ph = h;
                s.state = P_DECODED;
            } else if transient {
                park_retry(s);
            } else {
                s.state = P_FAILED;
            }
        } else if !px.is_null() {
            drop(g);
            img::img_free(px); // recycled while we worked: discard
        }

        // The cached picture is on its way to the screen; now, and only now, look again at a
        // file old enough to deserve it, on a link plex.tv has answered on recently. A refresh
        // must never cost a face: the fetch feeds the FILE (next boot shows it), the texture
        // already published stands, and a fetch that brings nothing changes nothing. This worker
        // is busy for the round trip, which is the price of not holding a third thread for it.
        if let Some(k) = refresh_after {
            if let Some(b) = crate::plex::client_for(srv).and_then(|c| c.fetch_built(&key_s)) {
                if !b.is_empty() {
                    let mut fw = 0i32;
                    let mut fh = 0i32;
                    let fresh = img::img_decode_rgba(b.as_ptr(), b.len() as c_int, &mut fw, &mut fh);
                    if !fresh.is_null() {
                        img::img_free(fresh);
                        crate::imgcache::write_at(cache_gen, &k, &b);
                    }
                }
            }
        }
    }
}

/// Spawn the poster workers. No config is threaded in: each request carries its own server
/// (the slot's [`Pslot::srv`]), and the address behind it comes from the registry
/// (`crate::plex::install` or a `register` must have run before a fetch can resolve).
pub(crate) fn init() {
    tex::install(&SOURCE);
    {
        let mut g = store();
        g.quit = false;
        g.slots = [Pslot::ZERO; PT_CAP];
        g.workers.clear();
    }
    // filter_map, not map: a refused worker is one fewer decoder, not a dead app. Artwork degrades
    // to whatever the survivors can fetch (and to nothing at all if both are refused).
    let handles: Vec<JoinHandle<()>> = (0..2)
        .filter_map(|_| crate::task::spawn("poster", poster_worker))
        .collect();
    store().workers = handles;
}

pub(crate) fn shutdown() {
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
    // free pending decodes, then every resident texture (main thread for GL); workers are joined
    let mut to_free = Vec::with_capacity(PT_CAP);
    {
        let mut g = store();
        for i in 0..PT_CAP {
            to_free.push(g.slots[i].px);
            g.slots[i] = Pslot::ZERO;
        }
    }
    for px in to_free {
        if px != 0 {
            img::img_free(px as *mut c_uchar);
        }
    }
    tex::shutdown(&mut GfxUploader);
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
    const HEADSHOT: &str =
        "https://metadata-static.plex.tv/a/people/0123456789abcdef0123456789abcdef.jpg";
    /// The same, encoded — every `:` and `/` gone, which is the whole reason the PMS photo
    /// transcoder can be handed a third-party URL as a query VALUE.
    const HEADSHOT_ENC: &str =
        "https%3A%2F%2Fmetadata-static.plex.tv%2Fa%2Fpeople%2F0123456789abcdef0123456789abcdef.jpg";

    /// A registry holding exactly one server, emptied again on the way out. The reset on DROP is
    /// the load-bearing half: an owned `BrowseStore::pump` adopts every registered slot as a source
    /// and spawns a discovery worker for it, so a server left behind here would have another module's tests
    /// dialling a dead loopback port on a background thread.
    struct Fresh(#[allow(dead_code)] crate::testlock::Serial);
    impl Drop for Fresh {
        fn drop(&mut self) {
            crate::plex::reset_servers_for_test();
            reset_key_memo();
        }
    }
    /// `register_for_test`, not the public `register`: the latter resolves the device id through
    /// `session::load`, which mints and PERSISTS a uuid on a host that has no session file.
    fn one_server() -> (Fresh, ServerId, &'static str) {
        let g = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        reset_key_memo();
        let tok = "tok-poster-test";
        let sid = crate::plex::register_for_test(
            "poster-test",
            "127.0.0.1",
            32400,
            tok,
            "cid-poster-test",
        );
        (Fresh(g), sid, tok)
    }

    /// Build a key through the real [`built_key`] — the same call `lookup` makes for a draw —
    /// so these grade the string a draw actually receives, not a reimplementation of it.
    fn key_for(srv: ServerId, src: &str, w: c_int, h: c_int, png: c_int) -> String {
        built_key(srv, src, w, h, png != 0).unwrap_or("").to_owned()
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

        assert!(
            k.starts_with("/photo/:/transcode?width=300&height=300&minSize=1&url="),
            "wrong request shape: {k}"
        );
        assert!(
            k.contains(&format!("url={HEADSHOT_ENC}&")),
            "the absolute URL is not encoded whole: {k}"
        );
        assert!(
            !k.contains("://"),
            "a raw scheme separator would truncate the url parameter: {k}"
        );
        assert!(
            k.ends_with(&format!("&X-Plex-Token={tok}")),
            "the token belongs to the OUTER request: {k}"
        );

        // The headroom the doc quotes, pinned. A headshot key is the longest shape the store holds
        // today; if a future one crosses KEY_MAX it is refused rather than truncated (below), so
        // this failing means artwork silently became skeletons, not that anything corrupted.
        assert!(
            k.len() <= KEY_MAX,
            "a headshot key must fit a slot: {} bytes",
            k.len()
        );
    }

    /// The other half of "keep the existing path untouched": a PMS-relative key is encoded by the
    /// same rule and comes out the same shape it always did. Both kinds reach the transcoder
    /// through one code path, and this is what says so.
    #[test]
    fn a_relative_thumb_still_takes_the_old_path() {
        let (_g, sid, _) = one_server();
        let k = key_for(sid, "/library/metadata/42/thumb/1778526065", 250, 375, 0);
        assert!(
            k.starts_with("/photo/:/transcode?width=250&height=375&minSize=1&url="),
            "wrong request shape: {k}"
        );
        assert!(
            k.contains("url=%2Flibrary%2Fmetadata%2F42%2Fthumb%2F1778526065&"),
            "relative key mis-encoded: {k}"
        );

        // png=1 is the clearLogo flavour — the one thing that changes the shape — and it must not
        // have moved either.
        let logo = key_for(
            sid,
            "/library/metadata/42/clearLogo",
            LOGO_REQ_W,
            LOGO_REQ_H,
            1,
        );
        assert!(
            logo.contains("&format=png&"),
            "the transparent flavour lost its format: {logo}"
        );
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
        assert_eq!(
            key_for(sid, "", 300, 300, 0),
            "",
            "an empty source must not become a request"
        );
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
        assert_eq!(
            exact.len(),
            KEY_MAX,
            "the fixture must land exactly on the boundary"
        );
        let mut s = Pslot::ZERO;
        set_key(&mut s, &exact);
        assert_eq!(
            key_bytes(&s),
            exact.as_bytes(),
            "a key the gate accepts must survive a slot intact"
        );

        let over = key_for(sid, &"a".repeat(KEY_MAX - overhead + 1), 300, 300, 0);
        assert_eq!(
            over, "",
            "one byte past what a slot holds must be refused, not handed out"
        );

        // …because this is what handing it out would have meant. Not a claim about `set_key`, a
        // claim about `lookup`: the probe it compares against is the FULL string.
        let would_be = "b".repeat(KEY_MAX + 1);
        set_key(&mut s, &would_be);
        assert_ne!(
            key_bytes(&s),
            would_be.as_bytes(),
            "a truncated slot can never match its own probe"
        );
    }

    /// The store's precondition, asserted where it actually lives.
    ///
    /// `poster_key` refusing is only half the guarantee: [`lookup`] takes a `&str` from three
    /// `pub(crate)` entry points that each accept a raw `*const c_char`, so a key built by any
    /// other route reaches the 256-byte array without passing the producer's gate. Testing
    /// [`is_fetchable`] directly is the whole reason it was split out — `lookup` itself cannot be
    /// called from a host test binary (it reaches `gfx::delete_tex`, and nothing here links GL).
    #[test]
    fn only_a_key_that_survives_a_slot_is_fetchable() {
        assert!(!is_fetchable(""), "an empty key names no request");
        assert!(
            is_fetchable("/photo/:/transcode?url=x"),
            "an ordinary key is fetchable"
        );
        assert!(
            is_fetchable(&"a".repeat(KEY_MAX)),
            "exactly what a slot holds is still fetchable"
        );
        assert!(
            !is_fetchable(&"a".repeat(KEY_MAX + 1)),
            "one byte past the array must never claim a slot"
        );
    }

    /// A settled, drawable slot: READY, last touched on frame `frame` with LRU age `use_`.
    fn ready(use_: c_uint, frame: c_uint) -> Pslot {
        Pslot {
            state: P_READY,
            use_,
            frame,
            ..Pslot::ZERO
        }
    }

    /// A rejected decode used to leave the source in READY while the cache remembered the key as
    /// failed. No later probe could re-arm either half, so the tile drew its skeleton forever.
    #[test]
    fn a_rejected_decode_cannot_leave_the_real_source_claiming_ready() {
        let (_fresh, sid, _) = one_server();
        tex::install(&SOURCE);
        tex::reset_for_test(16);
        let path = key_for(sid, "/library/metadata/42/thumb", 2, 2, 0);
        {
            let mut g = store();
            g.slots = [Pslot::ZERO; PT_CAP];
            let slot = &mut g.slots[0];
            slot.srv = sid;
            set_key(slot, &path);
            slot.state = P_DECODED;
            slot.px = 0;
            slot.pw = 0;
            slot.ph = 0;
        }

        drain_decoded();

        assert_eq!(
            store().slots[0].state,
            P_EVICTED,
            "a cache rejection must synchronously revoke the source's READY claim"
        );
    }

    /// The whole product loop, using the installed [`PosterSource`], the thread-local product
    /// cache and the global poster store: resident → byte-pressure release → dormant EVICTED → a
    /// real DRAW probe → WANT + condition-variable wake → worker publication → resident again. A
    /// warm probe would leave the slot dormant (see
    /// [`a_warm_probe_of_an_evicted_slot_leaves_it_dormant`]) — this exercises the recovery path
    /// that still works, a real draw.
    #[test]
    fn a_ready_source_hit_recovers_after_its_texture_is_evicted() {
        struct StubUp {
            next: u32,
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
            fn warm(&mut self, _: Tex) {}
            fn free(&mut self, _: Tex) {}
        }

        const BYTES: usize = 16;
        const SRC: &str = "/library/metadata/42/thumb";
        let (_fresh, srv, _) = one_server();
        tex::install(&SOURCE);
        tex::reset_for_test(BYTES);
        let path = key_for(srv, SRC, 2, 2, 0);
        {
            let mut g = store();
            g.slots = [Pslot::ZERO; PT_CAP];
            g.quit = false;
            let slot = &mut g.slots[0];
            slot.state = P_READY;
            slot.srv = srv;
            set_key(slot, &path);
        }

        let decoded = |key| PosterReady {
            key,
            result: Ok(Decoded {
                w: 2,
                h: 2,
                rgba: vec![0; BYTES].into_boxed_slice(),
            }),
        };
        tex::accept(decoded(PosterKey(0)));
        let mut budget = crate::ui::frame::Budget::new();
        budget.begin_frame(0);
        let mut present = crate::ui::present::Present::new();
        let mut present_handle = crate::ui::machine::PresentHandle::of(&mut present);
        let mut uploader = StubUp { next: 0 };
        assert_eq!(
            tex::prepare(&mut budget, &mut uploader, &mut present_handle, || 0),
            1
        );
        assert_ne!(tex::resolve_on(srv.raw(), SRC, 2, 2, false), 0);

        tex::accept(decoded(PosterKey(1)));
        budget.begin_frame(20_000);
        let mut present_handle = crate::ui::machine::PresentHandle::of(&mut present);
        assert_eq!(
            tex::prepare(&mut budget, &mut uploader, &mut present_handle, || 20_000),
            1,
            "the second upload must evict the first by bytes through the product wrapper"
        );
        assert_eq!(store().slots[0].state, P_EVICTED);

        let (armed_tx, armed_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let g = store();
            armed_tx.send(()).unwrap();
            let (mut g, timeout) = CV
                .wait_timeout_while(g, Duration::from_secs(1), |s| s.slots[0].state != P_WANT)
                .unwrap_or_else(|e| e.into_inner());
            assert!(!timeout.timed_out(), "the draw probe must wake a waiting poster worker");
            let slot = &mut g.slots[0];
            slot.state = P_LOADING;
            drop(g);

            unsafe extern "C" {
                fn malloc(size: usize) -> *mut std::ffi::c_void;
            }
            let px = unsafe { malloc(BYTES) as *mut c_uchar };
            assert!(!px.is_null());
            unsafe { std::ptr::write_bytes(px, 0, BYTES) };
            let mut g = store();
            let slot = &mut g.slots[0];
            slot.px = px as usize;
            slot.pw = 2;
            slot.ph = 2;
            slot.state = P_DECODED;
        });
        armed_rx.recv().unwrap();
        assert_eq!(
            tex::resolve_on(srv.raw(), SRC, 2, 2, false),
            0,
            "the re-armed slot has no texture yet - a draw re-arms the dormant source slot"
        );
        worker.join().unwrap();
        assert_eq!(store().slots[0].state, P_DECODED, "the woken worker published pixels");

        drain_decoded();
        budget.begin_frame(40_000);
        let mut present_handle = crate::ui::machine::PresentHandle::of(&mut present);
        assert_eq!(
            tex::prepare(&mut budget, &mut uploader, &mut present_handle, || 40_000),
            1
        );
        assert_ne!(
            tex::resolve_on(srv.raw(), SRC, 2, 2, false),
            0,
            "the same source key becomes resident again"
        );

        tex::shutdown(&mut uploader);
        store().slots = [Pslot::ZERO; PT_CAP];
    }

    /// A DRAW that misses while the document is scrolling fast claims no slot at all, and the
    /// very same draw claims one the moment the document settles. The demand is deferred, never
    /// dropped: nothing about the miss is recorded, so the next frame simply asks again.
    #[test]
    fn a_fast_scroll_declines_a_visible_miss_and_a_settle_takes_it() {
        let (_fresh, sid, _) = one_server();
        let path = key_for(sid, "/library/metadata/4242/thumb", 2, 2, 0);
        store().slots = [Pslot::ZERO; PT_CAP];

        let motion = crate::ui::card_motion::Scope::moving_for_test();
        assert!(crate::ui::card_motion::declines_request(), "the fixture is a fast scroll");
        let (hit, warm) = lookup(sid, &path, Touch::Draw);
        assert_eq!(hit, None, "a fast-scrolling miss resolves to no poster");
        assert_eq!(warm, Warm::Full, "and reports the attempt spent");
        assert!(store().slots.iter().all(|s| s.state == P_EMPTY),
            "no slot is claimed, so no worker, fetch, decode or upload follows");

        // The settle is the whole difference — same key, same store, same call.
        drop(motion);
        assert!(!crate::ui::card_motion::declines_request());
        let (_, warm) = lookup(sid, &path, Touch::Draw);
        assert_eq!(warm, Warm::Claimed, "a settled document claims the slot");
        assert_eq!(store().slots[0].state, P_WANT, "and queues it for a worker");
        assert!(store().slots[0].visible, "as visible demand, not speculation");

        store().slots = [Pslot::ZERO; PT_CAP];
    }

    /// Exercise the source-facing half of the product card primitive, not a
    /// hand-entered admission scope. GPU composition is the real-TV obligation.
    /// A future position term in any screen reaches this same transformed centre.
    #[test]
    fn the_card_renderer_scopes_every_art_variant_and_unknown_misses_converge() {
        let (_fresh, sid, _) = one_server();
        tex::install(&SOURCE);
        tex::reset_for_test(16 * 1024 * 1024);
        store().slots = [Pslot::ZERO; PT_CAP];
        let rect = crate::ui::Rect::new(0.0, 0.0, 250.0, 375.0);
        let mut item = crate::pms::PmsMovie::default();
        item.sid = sid;
        item.thumb = "/library/metadata/42/thumb".into();
        item.still = "/library/metadata/42/still".into();
        crate::ui::widgets::resolve_card_art(crate::ui::Painter::recording(), rect,
            &crate::ui::widgets::Art::Poster(Some(&item)));
        assert!(store().slots.iter().all(|s| s.state == P_EMPTY), "text prewarming must not start poster work");
        for frame in 0..3 {
            crate::ui::card_motion::begin_frame(frame * 16);
            crate::ui::idle::frame_begin(0.016);
            crate::ui::idle::take_local_damage();
            let painter = crate::ui::Painter::root().translate(if frame == 0 { 0.0 } else { 80.0 }, 0.0);
            for art in [crate::ui::widgets::Art::Poster(Some(&item)), crate::ui::widgets::Art::Still(Some(&item)),
                crate::ui::widgets::Art::Thumb { sid, key: "/test-thumb", res: (250, 375) },
                crate::ui::widgets::Art::Person { sid, key: "/test-person", res: (250, 250) }] {
                crate::ui::widgets::resolve_card_art(painter, rect, &art);
            }
            if frame < 2 {
                assert!(store().slots.iter().all(|s| s.state == P_EMPTY), "unknown/moving cards must claim no slots");
                assert!(crate::ui::idle::take_local_damage() > 0, "a refused unknown needs a follow-up present");
            } else {
                assert_eq!(store().slots.iter().filter(|s| s.state == P_WANT).count(), 4, "all four settled variants must queue art");
            }
        }
        assert!(!crate::ui::card_motion::declines_request(), "hero work after a card is outside its scope");
        store().slots = [Pslot::ZERO; PT_CAP];
    }

    #[test]
    fn a_motion_declined_retry_wakes_only_when_due() {
        let (_fresh, sid, _) = one_server();
        let path = key_for(sid, "/library/metadata/4242/thumb", 2, 2, 0);
        {
            let mut g = store();
            g.slots = [Pslot::ZERO; PT_CAP];
            let slot = &mut g.slots[0];
            slot.srv = sid;
            set_key(slot, &path);
            slot.state = P_RETRY;
            slot.retry_at = Some(crate::app::clock::now().wrapping_add(30_000));
        }
        let motion = crate::ui::card_motion::Scope::moving_for_test();
        crate::ui::idle::take_local_damage();
        lookup(sid, &path, Touch::Draw);
        assert_eq!(crate::ui::idle::take_local_damage(), 0, "a future retry must let the present gate rest");
        store().slots[0].retry_at = Some(0);
        lookup(sid, &path, Touch::Draw);
        assert!(crate::ui::idle::take_local_damage() > 0, "an actually refused rearm needs a follow-up draw");
        assert_eq!(store().slots[0].state, P_RETRY);
        drop(motion);
        lookup(sid, &path, Touch::Draw);
        assert_eq!(store().slots[0].state, P_WANT);
        store().slots = [Pslot::ZERO; PT_CAP];
    }

    /// **P2 (Codex review on PR #187).** The fresh-miss gate above is the LEAST important of the
    /// three places a draw starts poster work, because a fast scroll is the very thing that
    /// evicts textures: scroll down hard, and the rows behind you lose residency; scroll back,
    /// and every tile you pass finds its own slot parked at `P_EVICTED` with no cooldown set
    /// (a first eviction never cools). Re-arming each one is a full fetch, decode and upload —
    /// the work the gate exists to refuse — reached through a branch that returns long before
    /// the miss path. The deferral is the cooldown's, not a drop: the slot stays `P_EVICTED`
    /// and the first settled draw re-arms it exactly as it always did.
    #[test]
    fn a_fast_scroll_declines_to_re_arm_an_evicted_slot() {
        let (_fresh, sid, _) = one_server();
        let path = key_for(sid, "/library/metadata/4243/thumb", 2, 2, 0);
        {
            let mut g = store();
            g.slots = [Pslot::ZERO; PT_CAP];
            let slot = &mut g.slots[0];
            slot.srv = sid;
            set_key(slot, &path);
            slot.state = P_EVICTED;
            // No eviction history, so the cooldown gate would admit this re-arm. That isolates
            // the scroll gate: nothing else in this branch is refusing.
            slot.evict_cooldown_until = None;
        }
        let (_, rearmed0) = residency_counts_for_test();

        let motion = crate::ui::card_motion::Scope::moving_for_test();
        let (hit, warm) = lookup(sid, &path, Touch::Draw);
        assert_eq!(hit, None, "an evicted slot has no pixels to hand back");
        assert_eq!(warm, Warm::Known, "the slot is known, merely left dormant");
        assert_eq!(
            store().slots[0].state,
            P_EVICTED,
            "a fast scroll must not re-arm an evicted slot: that is a whole fetch and upload"
        );
        let (_, rearmed1) = residency_counts_for_test();
        assert_eq!(rearmed1, rearmed0, "and must not count as a re-arm");

        drop(motion);
        let (_, _) = lookup(sid, &path, Touch::Draw);
        assert_eq!(store().slots[0].state, P_WANT, "a settled draw re-arms it exactly as before");
        let (_, rearmed2) = residency_counts_for_test();
        assert_eq!(rearmed2, rearmed0 + 1, "and the settle is what the counter records");

        store().slots = [Pslot::ZERO; PT_CAP];
    }

    /// The same narrowing for the other existing-slot re-arm: a `P_RETRY` slot whose backoff has
    /// expired goes back in the queue on the next DRAW, and that draw is subject to the same
    /// admission as any other. The wait is not restarted and the attempt count is untouched — the
    /// slot simply stays parked and due, so the first settled draw takes it.
    #[test]
    fn a_fast_scroll_declines_to_re_queue_a_due_retry() {
        let (_fresh, sid, _) = one_server();
        let path = key_for(sid, "/library/metadata/4244/thumb", 2, 2, 0);
        {
            let mut g = store();
            g.slots = [Pslot::ZERO; PT_CAP];
            let slot = &mut g.slots[0];
            slot.srv = sid;
            set_key(slot, &path);
            slot.state = P_RETRY;
            slot.attempts = 1;
            // Scheduled at tick 0, so it is due for any plausible reading of the app clock.
            slot.retry_at = Some(0);
        }

        let motion = crate::ui::card_motion::Scope::moving_for_test();
        let (hit, _) = lookup(sid, &path, Touch::Draw);
        assert_eq!(hit, None, "a parked retry has no pixels");
        assert_eq!(
            store().slots[0].state,
            P_RETRY,
            "a fast scroll must not re-queue a due retry"
        );
        assert_eq!(
            store().slots[0].retry_at,
            Some(0),
            "and must not reschedule it: the wait already elapsed, it is the draw that was refused"
        );
        assert_eq!(store().slots[0].attempts, 1, "a refusal is not an attempt");

        drop(motion);
        let (_, _) = lookup(sid, &path, Touch::Draw);
        assert_eq!(store().slots[0].state, P_WANT, "a settled draw re-queues it");
        assert_eq!(store().slots[0].retry_at, None, "clearing the elapsed wait");

        store().slots = [Pslot::ZERO; PT_CAP];
    }

    /// The two counters behind `posters: residency lost=… rearmed=…` (issue #107): the route
    /// heartbeat's `evicted_hot=` cannot distinguish "eviction never fired" from "eviction fired
    /// and the re-arm worked", so these are graded directly rather than through the log line's
    /// throttle. `one_server` takes [`crate::testlock::serial`], which is what keeps this test's
    /// deltas exact against the other tests in this file that drive real eviction through the
    /// installed cache (`a_rejected_decode_cannot_leave_the_real_source_claiming_ready`,
    /// `a_ready_source_hit_recovers_after_its_texture_is_evicted`).
    #[test]
    fn residency_transitions_are_counted_exactly_once_each() {
        use crate::ui::tex::Source as _;

        let (_fresh, sid, _) = one_server();
        let path = key_for(sid, "/library/metadata/42/thumb", 2, 2, 0);
        {
            let mut g = store();
            g.slots = [Pslot::ZERO; PT_CAP];
            let slot = &mut g.slots[0];
            slot.srv = sid;
            set_key(slot, &path);
            slot.state = P_READY;
        }
        let (lost0, rearmed0) = residency_counts_for_test();

        SOURCE.unresident(PosterKey(0));
        assert_eq!(store().slots[0].state, P_EVICTED, "unresident demotes a READY slot");
        let (lost1, rearmed1) = residency_counts_for_test();
        assert_eq!(lost1, lost0 + 1, "a real READY->EVICTED transition must count exactly once");
        assert_eq!(rearmed1, rearmed0, "unresident never touches the re-arm counter");

        // A slot already EVICTED has no READY truth left to revoke - must not count again.
        SOURCE.unresident(PosterKey(0));
        let (lost2, _) = residency_counts_for_test();
        assert_eq!(lost2, lost1, "a slot that was already EVICTED must not be counted twice");

        let (hit, _) = lookup(sid, &path, Touch::Draw);
        assert_eq!(hit, None, "a freshly re-armed slot has no pixels yet");
        assert_eq!(store().slots[0].state, P_WANT, "lookup re-arms the dormant slot");
        let (_, rearmed2) = residency_counts_for_test();
        assert_eq!(rearmed2, rearmed1 + 1, "the EVICTED->WANT re-arm must count exactly once");

        store().slots = [Pslot::ZERO; PT_CAP];
    }

    /// **P1 (Codex review on PR #182, `lookup`'s P_EVICTED branch, poster.rs:564).** Without the
    /// thrash guard, every draw that finds a key EVICTED moves it straight back to `P_WANT`
    /// unconditionally, with no memory of how recently it was evicted before. A working set that
    /// genuinely cannot fit under the render cache's byte budget then has this key evicted,
    /// re-armed, evicted again, re-armed again — forever, once frames keep drawing it — instead of
    /// settling. This drives exactly that: a slot evicted and (synthetically, via the same
    /// `SOURCE.unresident` callback the render cache calls under real byte pressure) re-evicted in
    /// rapid succession, every cycle immediately followed by a draw. Pre-fix, EVERY cycle rearms —
    /// no bound at all. Post-fix, only the first (uncooled) eviction rearms; the rest are refused
    /// until their cooldown clears, which real time never does inside a tight loop.
    #[test]
    fn a_key_evicted_and_reevicted_in_rapid_succession_does_not_rearm_without_bound() {
        use crate::ui::tex::Source as _;

        let (_fresh, sid, _) = one_server();
        let path = key_for(sid, "/library/metadata/42/thumb", 2, 2, 0);
        {
            let mut g = store();
            g.slots = [Pslot::ZERO; PT_CAP];
            let slot = &mut g.slots[0];
            slot.srv = sid;
            set_key(slot, &path);
            slot.state = P_READY;
        }
        let (_, rearmed0) = residency_counts_for_test();

        const CYCLES: u32 = 20;
        let mut rearms = 0u32;
        for _ in 0..CYCLES {
            // Stand-in for the render cache's own byte-pressure eviction (real pressure needs a
            // live GL cache no host test links; this callback IS the seam it calls through).
            SOURCE.unresident(PosterKey(0));
            let (hit, _) = lookup(sid, &path, Touch::Draw);
            assert_eq!(hit, None, "an evicted slot never has pixels the same probe that finds it");
            if store().slots[0].state == P_WANT {
                rearms += 1;
                // Simulate the fetch/decode/upload this rearm triggers completing immediately —
                // the fast round trip a thrash needs, not the slow one a network stall would add.
                store().slots[0].state = P_READY;
            }
        }
        let (_, rearmed_final) = residency_counts_for_test();
        assert_eq!(
            rearmed_final - rearmed0,
            rearms as u64,
            "the rearm counter must track exactly the transitions this loop drove"
        );

        assert!(
            rearms < CYCLES,
            "a key evicted and re-evicted in rapid succession must eventually back off instead \
             of rearming every single cycle (got {rearms} rearms out of {CYCLES} cycles - with \
             no bound this is the P1 thrash: continuous fetch/decode/upload instead of settling)"
        );
        assert_eq!(
            rearms, 1,
            "the FIRST eviction must still rearm instantly (the bug PR #182 itself fixes); every \
             eviction after it lands inside the thrash window and must be cooled down"
        );

        store().slots = [Pslot::ZERO; PT_CAP];
    }

    /// The pure decisions behind the residency thrash guard (see `evict_was_rapid`'s module doc):
    /// a key's first-ever eviction is never rapid (nothing to compare against), one that lands
    /// inside the thrash window is, and one that lands outside it is treated as fresh again - the
    /// guard must not let a key that settles for a while accumulate backoff it no longer deserves.
    #[test]
    fn evict_was_rapid_only_flags_a_reeviction_inside_the_thrash_window() {
        assert!(!evict_was_rapid(None, 0), "a key's first-ever eviction has nothing to compare");
        assert!(
            evict_was_rapid(Some(0), EVICT_THRASH_WINDOW_MS - 1),
            "one millisecond inside the window is still rapid"
        );
        assert!(
            !evict_was_rapid(Some(0), EVICT_THRASH_WINDOW_MS),
            "the window's own edge is no longer rapid"
        );
    }

    /// The pure decision behind the thrash guard's cooldown gate: no cooldown ever set is always
    /// due (the ordinary, non-thrashing case), and a set cooldown is due only once its deadline has
    /// actually passed - mirrors [`retry_due`]'s own wrap-safe "has this deadline passed" shape.
    #[test]
    fn evict_cooldown_due_only_after_its_own_deadline() {
        assert!(evict_cooldown_due(None, 0), "no cooldown was ever set - never gated");
        assert!(!evict_cooldown_due(Some(1_000), 999), "one millisecond short is still cooling");
        assert!(evict_cooldown_due(Some(1_000), 1_000), "the deadline itself has cleared");
    }

    /// A second half of the same PR #182 P1: [`evict_cooldown_due_only_after_its_own_deadline`]
    /// and [`a_key_evicted_and_reevicted_in_rapid_succession_does_not_rearm_without_bound`] bound
    /// how OFTEN a DRAW may re-arm an evicted key; this proves a `Touch::Warm` probe of the same
    /// key may never re-arm it AT ALL, regardless of cooldown state — resurrecting an evicted key
    /// speculatively is exactly what let an off-screen prefetch alternate evict/refetch/evict
    /// against on-screen demand (Home's recurring backdrop prefetch, named in the review). The
    /// slot here has no eviction history (`evict_cooldown_until` is `None`, i.e. never cooling), so
    /// this isolates the touch-type gate from the cooldown gate: a warm probe is refused even when
    /// nothing else would have refused it. The second half proves the fix is a narrowing, not a
    /// removal: a real draw of the same slot still re-arms.
    #[test]
    fn a_warm_probe_of_an_evicted_slot_leaves_it_dormant() {
        let (_fresh, sid, _) = one_server();
        let path = key_for(sid, "/library/metadata/42/thumb", 2, 2, 0);
        {
            let mut g = store();
            g.slots = [Pslot::ZERO; PT_CAP];
            let slot = &mut g.slots[0];
            slot.srv = sid;
            set_key(slot, &path);
            slot.state = P_EVICTED;
        }
        let (_, rearmed0) = residency_counts_for_test();

        let (hit, warm) = lookup(sid, &path, Touch::Warm);
        assert_eq!(hit, None, "an evicted slot has no pixels to hand back");
        assert_eq!(warm, Warm::Known, "a warm probe must not claim a slot it leaves dormant");
        assert_eq!(
            store().slots[0].state,
            P_EVICTED,
            "a warm probe must not re-arm an evicted slot"
        );
        let (_, rearmed1) = residency_counts_for_test();
        assert_eq!(rearmed1, rearmed0, "a warm probe must not increment the re-arm counter");

        // The same slot, probed by a real draw, still re-arms - the fix narrows WHICH touch
        // re-arms an evicted slot, it does not remove re-arming itself.
        let (hit, _) = lookup(sid, &path, Touch::Draw);
        assert_eq!(hit, None, "a freshly re-armed slot has no pixels yet");
        assert_eq!(store().slots[0].state, P_WANT, "a draw probe still re-arms the dormant slot");
        let (_, rearmed2) = residency_counts_for_test();
        assert_eq!(
            rearmed2,
            rearmed0 + 1,
            "the draw's EVICTED->WANT re-arm must count exactly once"
        );

        store().slots = [Pslot::ZERO; PT_CAP];
    }

    /// **Deterministic deadline progression (adjudication on PR #182, item 2).** The tight-loop
    /// test above ([`a_key_evicted_and_reevicted_in_rapid_succession_does_not_rearm_without_bound`])
    /// never lets real time move — the host clock stub is frozen — so it proves the FIRST cooldown
    /// blocks a rearm but nothing about what happens once that cooldown's own deadline arrives,
    /// which is the schedule's whole point. This drives the app clock explicitly instead
    /// (`crate::app::clock::set_replay`, the same seam the recorded-replay driver uses), so a
    /// `Touch::Warm` probe can be checked on both sides of the deadline: still dormant a tick
    /// before it, and — because Warm is refused unconditionally, before the cooldown gate is even
    /// reached — still dormant a tick after it too.
    #[test]
    fn a_touch_warm_probe_of_a_cooling_slot_stays_dormant_before_and_after_expiry() {
        let (_fresh, sid, _) = one_server();
        let path = key_for(sid, "/library/metadata/42/thumb", 2, 2, 0);
        {
            let mut g = store();
            g.slots = [Pslot::ZERO; PT_CAP];
            let slot = &mut g.slots[0];
            slot.srv = sid;
            set_key(slot, &path);
            slot.state = P_EVICTED;
            slot.evict_attempts = 1;
            slot.evicted_at = Some(0);
            slot.evict_cooldown_until = Some(250); // evict_backoff(1)
        }

        crate::app::clock::set_replay(100); // before the deadline
        let (hit, warm) = lookup(sid, &path, Touch::Warm);
        assert_eq!(hit, None);
        assert_eq!(warm, Warm::Known);
        assert_eq!(store().slots[0].state, P_EVICTED, "a warm probe stays dormant before expiry");

        crate::app::clock::set_replay(250); // exactly at the deadline
        let (hit, warm) = lookup(sid, &path, Touch::Warm);
        assert_eq!(hit, None);
        assert_eq!(warm, Warm::Known);
        assert_eq!(
            store().slots[0].state,
            P_EVICTED,
            "a warm probe stays dormant even once the cooldown has cleared - only a Draw probe \
             may recover an EVICTED slot"
        );

        store().slots = [Pslot::ZERO; PT_CAP];
    }

    /// The Draw-probe half of the same deadline: refused one tick short, honored at the deadline
    /// itself - mirrors [`evict_cooldown_due_only_after_its_own_deadline`]'s edges, but through
    /// `lookup` end to end (the re-arm counter, the state transition) rather than the pure
    /// predicate alone.
    #[test]
    fn a_touch_draw_probe_only_rearms_once_the_cooldown_deadline_arrives() {
        let (_fresh, sid, _) = one_server();
        let path = key_for(sid, "/library/metadata/42/thumb", 2, 2, 0);
        {
            let mut g = store();
            g.slots = [Pslot::ZERO; PT_CAP];
            let slot = &mut g.slots[0];
            slot.srv = sid;
            set_key(slot, &path);
            slot.state = P_EVICTED;
            slot.evict_attempts = 1;
            slot.evicted_at = Some(0);
            slot.evict_cooldown_until = Some(250);
        }
        let (_, rearmed0) = residency_counts_for_test();

        crate::app::clock::set_replay(249);
        let (hit, _) = lookup(sid, &path, Touch::Draw);
        assert_eq!(hit, None);
        assert_eq!(
            store().slots[0].state,
            P_EVICTED,
            "one millisecond short of the deadline must still refuse to rearm"
        );
        let (_, rearmed1) = residency_counts_for_test();
        assert_eq!(rearmed1, rearmed0, "a refused rearm must not count");

        crate::app::clock::set_replay(250);
        let (hit, _) = lookup(sid, &path, Touch::Draw);
        assert_eq!(hit, None, "a freshly rearmed slot has no pixels yet");
        assert_eq!(store().slots[0].state, P_WANT, "the deadline itself rearms it");
        let (_, rearmed2) = residency_counts_for_test();
        assert_eq!(rearmed2, rearmed0 + 1, "the rearm must count exactly once");

        store().slots = [Pslot::ZERO; PT_CAP];
    }

    /// The escalation the tight-loop test cannot see: driving the app clock forward by exactly
    /// each deadline (never one tick less, per [`a_touch_draw_probe_only_rearms_once_the_cooldown_deadline_arrives`])
    /// and completing the recovery immediately each time - the fast round trip a thrash needs -
    /// must walk [`evict_backoff`]'s own schedule (250ms, 500ms, 1s, 2s, 4s, 8s) and then hold at
    /// its 8s cap, exactly like [`the_retry_backoff_doubles_from_one_second_and_caps`] proves for
    /// the unrelated transient-fetch schedule.
    #[test]
    fn repeated_completed_recoveries_escalate_the_cooldown_to_its_cap() {
        use crate::ui::tex::Source as _;

        let (_fresh, sid, _) = one_server();
        let path = key_for(sid, "/library/metadata/42/thumb", 2, 2, 0);
        let mut now: u32 = 0;
        crate::app::clock::set_replay(now);
        {
            let mut g = store();
            g.slots = [Pslot::ZERO; PT_CAP];
            let slot = &mut g.slots[0];
            slot.srv = sid;
            set_key(slot, &path);
            slot.state = P_READY;
        }

        // A fresh key's FIRST eviction never cools (`evicted_at` was `None`) - it rearms on the
        // very next Draw probe, the ordinary one-eviction-then-recovery case. This primes
        // `evicted_at` so the NEXT eviction (immediately after, same tick) is the first the
        // thrash guard can see as a continuation.
        SOURCE.unresident(PosterKey(0));
        assert_eq!(
            store().slots[0].evict_cooldown_until,
            None,
            "a key's first-ever eviction never cools"
        );
        let (hit, _) = lookup(sid, &path, Touch::Draw);
        assert_eq!(hit, None);
        assert_eq!(store().slots[0].state, P_WANT);
        store().slots[0].state = P_READY;

        let schedule_ms: [u32; 8] = [250, 500, 1_000, 2_000, 4_000, 8_000, 8_000, 8_000];
        for &wait in schedule_ms.iter() {
            SOURCE.unresident(PosterKey(0)); // rapid: inside the thrash window every time
            let cooldown = store().slots[0]
                .evict_cooldown_until
                .expect("a rapid re-eviction always sets a cooldown");
            assert_eq!(
                cooldown,
                now.wrapping_add(wait),
                "the escalation schedule must reach {wait}ms and cap there"
            );

            crate::app::clock::set_replay(now.wrapping_add(wait) - 1);
            let (hit, _) = lookup(sid, &path, Touch::Draw);
            assert_eq!(hit, None);
            assert_eq!(
                store().slots[0].state,
                P_EVICTED,
                "one tick short of {wait}ms must still refuse to rearm"
            );

            now = now.wrapping_add(wait);
            crate::app::clock::set_replay(now);
            let (hit, _) = lookup(sid, &path, Touch::Draw);
            assert_eq!(hit, None);
            assert_eq!(store().slots[0].state, P_WANT, "the deadline itself rearms it");
            store().slots[0].state = P_READY; // the fast recovery a thrash needs
        }

        store().slots = [Pslot::ZERO; PT_CAP];
    }

    /// The other side of [`evict_was_rapid_only_flags_a_reeviction_inside_the_thrash_window`]:
    /// mid-escalation, a gap past [`EVICT_THRASH_WINDOW_MS`] before the NEXT eviction must reset
    /// `evict_attempts` to 0 and set no cooldown at all - the key is treated exactly like one
    /// evicted for the first time, because as far as the guard can tell, it was: nothing has
    /// churned it in ten whole seconds.
    #[test]
    fn an_eviction_gap_past_the_thrash_window_resets_the_escalation() {
        use crate::ui::tex::Source as _;

        let (_fresh, sid, _) = one_server();
        let path = key_for(sid, "/library/metadata/42/thumb", 2, 2, 0);
        crate::app::clock::set_replay(0);
        {
            let mut g = store();
            g.slots = [Pslot::ZERO; PT_CAP];
            let slot = &mut g.slots[0];
            slot.srv = sid;
            set_key(slot, &path);
            slot.state = P_READY;
            slot.evicted_at = Some(0);
            slot.evict_attempts = 5; // mid-escalation from an earlier thrash episode
        }

        // The window's own edge - not one millisecond less - counts as settled.
        crate::app::clock::set_replay(EVICT_THRASH_WINDOW_MS);
        SOURCE.unresident(PosterKey(0));
        {
            let g = store();
            assert_eq!(g.slots[0].evict_attempts, 0, "a settled gap must clear the escalation");
            assert_eq!(
                g.slots[0].evict_cooldown_until, None,
                "a reset episode's first eviction never cools"
            );
        }

        let (hit, _) = lookup(sid, &path, Touch::Draw);
        assert_eq!(hit, None);
        assert_eq!(
            store().slots[0].state,
            P_WANT,
            "the reset key rearms on its very next Draw probe, exactly like a fresh key"
        );

        store().slots = [Pslot::ZERO; PT_CAP];
    }

    /// The pure decision behind [`invalidate_due_evictions`]: only a slot an active cooldown
    /// deadline is ticking against is ever due for a wake - a slot with no cooldown (`None`) has
    /// nothing to expire and needs none, since its very next Draw probe already re-arms it: a
    /// proactive wake for it would just be [`invalidate_due_retries`]'s own P_RETRY continuous-
    /// invalidation bug, reintroduced for P_EVICTED. Mirrors [`a_retry_slot_is_due_when_its_wait_is_over`]'s
    /// shape, including the wrap.
    #[test]
    fn an_evicted_slot_is_due_for_a_wake_only_once_its_cooldown_deadline_arrives() {
        let mut s = Pslot {
            state: P_EVICTED,
            ..Pslot::ZERO
        };
        assert!(!evict_wake_due(&s, 1_000), "no cooldown at all = nothing to wake for");
        s.evict_cooldown_until = Some(6_000);
        assert!(!evict_wake_due(&s, 1_000));
        assert!(evict_wake_due(&s, 6_000));
        assert!(evict_wake_due(&s, 9_000));
        s.evict_cooldown_until = Some(u32::MAX - 100);
        assert!(evict_wake_due(&s, 50), "the tick wraps like every app::clock comparison");
        s.state = P_READY;
        assert!(!evict_wake_due(&s, 60_000), "only an EVICTED slot can be due for this wake");
    }

    /// **Requirement 1's wiring, end to end: exactly one redraw request per expiry, not one per
    /// frame.** Mirrors [`a_due_parked_retry_invalidates_the_frame_gate`] exactly, because
    /// [`invalidate_due_evictions`] is the identical mechanism applied to `P_EVICTED` instead of
    /// `P_RETRY`, for the identical reason: without the `evict_wake_sent` latch, an expired,
    /// off-screen EVICTED slot would invalidate every loop iteration forever, since nothing draws
    /// it to consume the wake — undoing the idle behaviour the app depends on.
    #[test]
    fn a_due_evicted_slot_invalidates_the_frame_gate_exactly_once() {
        let _g = crate::testlock::serial();
        crate::ui::idle::reset_for_test();
        let mut slots = [Pslot::ZERO; PT_CAP];
        slots[3] = Pslot {
            state: P_EVICTED,
            evict_cooldown_until: Some(2_000),
            ..Pslot::ZERO
        };
        slots[4] = Pslot {
            state: P_EVICTED,
            evict_cooldown_until: Some(2_000),
            ..Pslot::ZERO
        };

        invalidate_due_evictions(&mut slots, 1_999);
        assert_eq!(
            crate::ui::idle::take_local_damage(),
            0,
            "a cooling slot must leave the screen settled before its deadline"
        );

        invalidate_due_evictions(&mut slots, 2_000);
        assert_eq!(
            crate::ui::idle::take_local_damage(),
            1,
            "the deadline must wake exactly one draw that can probe the slot again"
        );

        invalidate_due_evictions(&mut slots, 2_001);
        assert_eq!(
            crate::ui::idle::take_local_damage(),
            0,
            "an off-screen expired slot must not hold the present gate awake - the latch makes \
             this ONE redraw request, not a request every frame"
        );
    }

    /// The pure decision behind [`log_residency`]'s interval throttle: due before any line has
    /// ever been written, not due an instant after one was, due again once a full second has
    /// passed. Split out and tested the same way [`idle_of`] is, so the edges do not depend on a
    /// real elapsed second or a real Mutex.
    #[test]
    fn the_interval_throttle_only_waits_out_its_own_window() {
        let t0 = std::time::Instant::now();
        assert!(interval_due(None, t0), "the first line in a process is never throttled");
        assert!(
            !interval_due(Some(t0), t0),
            "immediately after a line, the same instant must not be due again"
        );
        assert!(
            !interval_due(Some(t0), t0 + Duration::from_millis(999)),
            "one millisecond short of the window must still be throttled"
        );
        assert!(
            interval_due(Some(t0), t0 + Duration::from_secs(1)),
            "a full second later the window has reopened"
        );
    }

    /// Reproduces the exact field defect this fix closes (Codex review on PR #182): a device
    /// session that read `lost=1 rearmed=0`, `lost=5 rearmed=0`, `lost=13 rearmed=0` and nothing
    /// else was reported as "the re-arm branch never fires" — but every one of those lines was
    /// written by an EVICTION, and [`log_residency`]'s interval throttle can swallow a re-arm
    /// that lands inside the same one-second window as the eviction that preceded it, which is
    /// exactly the ordinary one-eviction-then-recovery case. This drives that sequence directly:
    /// an eviction (unthrottled, since it is the first line) immediately followed by its re-arm
    /// (throttled, since under a second has passed) must leave `rearmed` stale in the last
    /// emitted line — proving the gap is real — and then a settled frame
    /// ([`log_residency_settled`], as [`begin_frame`] calls it every frame regardless of screen)
    /// must catch the totals up, because that path does not consult the interval at all.
    #[test]
    fn a_rearm_inside_the_throttle_window_still_reaches_the_log_once_the_store_settles() {
        use crate::ui::tex::Source as _;

        let (_fresh, sid, _) = one_server();
        reset_residency_log_for_test();
        let path = key_for(sid, "/library/metadata/42/thumb", 2, 2, 0);
        {
            let mut g = store();
            g.slots = [Pslot::ZERO; PT_CAP];
            let slot = &mut g.slots[0];
            slot.srv = sid;
            set_key(slot, &path);
            slot.state = P_READY;
        }

        // The eviction: the first line in the (just reset) throttle window, so it writes.
        SOURCE.unresident(PosterKey(0));
        let (lost1, rearmed1) = residency_counts_for_test();
        assert_eq!(
            residency_last_emitted_for_test(),
            (lost1, rearmed1),
            "the unthrottled first line must report the eviction it just recorded"
        );

        // The re-arm, immediately after: real time between these two calls is microseconds, so
        // the interval throttle finds the window still open and suppresses the line.
        let (hit, _) = lookup(sid, &path, Touch::Draw);
        assert_eq!(hit, None, "a freshly re-armed slot has no pixels yet");
        let (lost2, rearmed2) = residency_counts_for_test();
        assert_eq!(rearmed2, rearmed1 + 1, "the re-arm itself is never missed - only the LINE is");
        assert_eq!(
            residency_last_emitted_for_test(),
            (lost1, rearmed1),
            "the throttled re-arm must leave the last WRITTEN line stale - this is the bug"
        );

        // The store is quiet (P_WANT, not fetching or decoding) - a settled frame must catch the
        // line up regardless of the interval, because the totals moved since the last line.
        log_residency_settled(true);
        assert_eq!(
            residency_last_emitted_for_test(),
            (lost2, rearmed2),
            "a settle snapshot must report the re-arm even though the interval throttle just \
             suppressed it"
        );

        store().slots = [Pslot::ZERO; PT_CAP];
    }

    /// A settle snapshot must never write a line when nothing changed - `log_residency_settled`
    /// runs every frame the store is quiet, and most quiet frames follow another quiet frame.
    #[test]
    fn a_settled_frame_with_nothing_new_writes_no_second_line() {
        let _g = crate::testlock::serial();
        reset_residency_log_for_test();
        let (lost, rearmed) = residency_counts_for_test();
        {
            let mut st = RESIDENCY_LOG.lock().unwrap_or_else(|e| e.into_inner());
            st.at = Some(std::time::Instant::now());
            st.lost = lost;
            st.rearmed = rearmed;
        }
        // Nothing transitioned since - the recorded pair already matches the live totals, so this
        // must be a no-op (observable only through the pair staying put; a real double-write
        // would be indistinguishable here, but `log_residency_settled`'s own equality check is
        // what `residency_last_emitted_for_test` is pinning).
        log_residency_settled(true);
        assert_eq!(residency_last_emitted_for_test(), (lost, rearmed));
    }

    /// The LRU's two clauses, in order: an EMPTY slot is always preferred (a fresh store must fill
    /// before it evicts anything), and only once there is none does the oldest SETTLED slot go.
    #[test]
    fn victim_prefers_empty_then_oldest_settled() {
        let empty = [Pslot::ZERO; PT_CAP];
        assert_eq!(
            victim(&empty, 7),
            Some(0),
            "an untouched store fills from the front"
        );

        let mut one_hole = [ready(100, 1); PT_CAP];
        one_hole[40] = Pslot::ZERO;
        assert_eq!(
            victim(&one_hole, 7),
            Some(40),
            "an empty slot beats every settled one"
        );

        let mut full = [ready(100, 1); PT_CAP];
        full[17].use_ = 3; // the oldest
        full[52].use_ = 9;
        assert_eq!(
            victim(&full, 7),
            Some(17),
            "with nothing empty, the least recently used goes"
        );

        // FAILED is settled too — a 404 must not pin a slot forever.
        let mut failed = [ready(100, 1); PT_CAP];
        failed[8] = Pslot {
            state: P_FAILED,
            use_: 1,
            frame: 1,
            ..Pslot::ZERO
        };
        assert_eq!(victim(&failed, 7), Some(8));
        // …and so is a parked RETRY: a tile that scrolled away must not hold its slot
        failed[8].state = P_RETRY;
        assert_eq!(victim(&failed, 7), Some(8));
    }

    /// **A transient failure is parked, not burnt** (#107): the boot's plaintext window, a refused
    /// connect or a 5xx used to mark a poster `P_FAILED`, and `lookup` then answered "no art"
    /// for that key for as long as the slot lived — so a cover that missed once stayed a skeleton
    /// for the session while the debug build (which allows plaintext tokens) showed it. Only a
    /// status that will not change is final.
    #[test]
    fn only_a_final_status_is_final() {
        use crate::plex::ArtFetch;
        for s in [301, 400, 404, 405, 410, 414, 415, 422] {
            assert!(!is_transient(&ArtFetch::Status(s)), "{s} will not change");
        }
        for s in [401, 403, 408, 429, 500, 502, 503, 504] {
            assert!(is_transient(&ArtFetch::Status(s)), "{s} can change");
        }
        assert!(is_transient(&ArtFetch::NoResponse), "no response is the boot window's shape");
        assert!(!is_transient(&ArtFetch::Bytes(vec![1])), "bytes are the decoder's to judge");
    }

    /// The wait doubles from one second and caps at thirty: quick enough that the plaintext
    /// window costs one second, bounded enough that a dead server is not hammered.
    #[test]
    fn the_retry_backoff_doubles_from_one_second_and_caps() {
        let secs: Vec<u64> = (1..=8).map(|n| retry_backoff(n).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(retry_backoff(0).as_secs(), 1);
    }

    /// Only a RETRY slot whose scheduled wait is over is due: an unscheduled one is not (the
    /// next draw schedules it), the tick comparison wraps, and FAILED is never due.
    #[test]
    fn a_retry_slot_is_due_when_its_wait_is_over() {
        let mut s = Pslot {
            state: P_RETRY,
            ..Pslot::ZERO
        };
        assert!(!retry_due(&s, 1_000), "unscheduled = not yet");
        s.retry_at = Some(6_000);
        assert!(!retry_due(&s, 1_000));
        assert!(retry_due(&s, 6_000));
        assert!(retry_due(&s, 9_000));
        s.retry_at = Some(u32::MAX - 100);
        assert!(retry_due(&s, 50), "the tick wraps like every app::clock comparison");
        s.state = P_FAILED;
        assert!(!retry_due(&s, 60_000), "FAILED is never due");
    }

    /// A parked retry is clock-driven state on a settled screen. The loop still runs while the
    /// present gate rests, so the adapter must invalidate that gate when the deadline arrives;
    /// otherwise no draw probes the slot again and it remains parked until an unrelated frame.
    #[test]
    fn a_due_parked_retry_invalidates_the_frame_gate() {
        let _g = crate::testlock::serial();
        crate::ui::idle::reset_for_test();
        let mut slots = [Pslot::ZERO; PT_CAP];
        slots[3] = Pslot {
            state: P_RETRY,
            retry_at: Some(2_000),
            ..Pslot::ZERO
        };
        slots[4] = Pslot {
            state: P_RETRY,
            retry_at: Some(2_000),
            ..Pslot::ZERO
        };

        invalidate_due_retries(&mut slots, 1_999);
        assert_eq!(
            crate::ui::idle::take_local_damage(),
            0,
            "a parked retry must leave the screen settled before its deadline"
        );

        invalidate_due_retries(&mut slots, 2_000);
        assert_eq!(
            crate::ui::idle::take_local_damage(),
            1,
            "the deadline must wake a draw that can re-queue the retry"
        );

        invalidate_due_retries(&mut slots, 2_001);
        assert_eq!(
            crate::ui::idle::take_local_damage(),
            0,
            "an off-screen due slot must not hold the present gate awake"
        );
    }

    /// The worker cannot schedule against the loop clock, but its transition to `P_RETRY` must
    /// wake one draw to do so. Otherwise a settled screen waits for the keepalive before the
    /// advertised one-second backoff even begins.
    #[test]
    fn a_newly_parked_retry_wakes_the_draw_that_schedules_it() {
        let _g = crate::testlock::serial();
        crate::ui::idle::reset_for_test();
        let mut s = Pslot {
            state: P_LOADING,
            attempts: 2,
            ..Pslot::ZERO
        };

        park_retry(&mut s);

        assert_eq!(s.state, P_RETRY);
        assert_eq!(s.attempts, 3);
        assert_eq!(s.retry_at, None, "only a draw may read the loop clock and schedule");
        assert_eq!(crate::ui::idle::take_local_damage(), 1);
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
        assert_eq!(
            victim(&slots, f),
            Some(31),
            "the prefetched slot is the cheapest to throw away"
        );

        let drawn = [ready(5, f); PT_CAP];
        assert_eq!(
            victim(&drawn, f),
            None,
            "a frame that drew every slot claims nothing"
        );

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
        for st in [P_WANT, P_LOADING, P_DECODED] {
            let slots = [Pslot {
                state: st,
                use_: 0,
                frame: 0,
                ..Pslot::ZERO
            }; PT_CAP];
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
        let mut s = Pslot {
            state: P_READY,
            srv: a,
            ..Pslot::ZERO
        };
        set_key(&mut s, KEY);

        assert!(
            slot_matches(&s, a, KEY.as_bytes()),
            "the server that asked for it"
        );
        assert!(
            !slot_matches(&s, b, KEY.as_bytes()),
            "the same path on another server is another slot"
        );
        assert!(
            !slot_matches(&s, ServerId::UNSET, KEY.as_bytes()),
            "and an unknown server matches nothing"
        );
        assert!(!slot_matches(
            &s,
            a,
            b"/photo/:/transcode?width=250&height=375&minSize=1&url=%2Fother"
        ));

        // eviction only flips the state — the key bytes and the server stay behind, so an EMPTY
        // slot must be rejected before either is even compared
        let evicted = Pslot {
            state: P_EMPTY,
            ..s
        };
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
        let mut m = KeyMemo {
            map: std::collections::HashMap::new(),
        };
        let builds = std::cell::Cell::new(0u32);
        let key = |m: &mut KeyMemo, srv: u16, w: c_int, gen: u32, built: &str| -> String {
            m.get_or_build(srv, PATH, w, 375, false, gen, || {
                builds.set(builds.get() + 1);
                built.to_owned()
            })
            .to_owned()
        };

        assert_eq!(key(&mut m, 0, 250, 7, "A?token=a"), "A?token=a");
        assert_eq!(
            key(&mut m, 0, 250, 7, "never built"),
            "A?token=a",
            "a hit does not rebuild"
        );
        assert_eq!(builds.get(), 1);

        assert_eq!(
            key(&mut m, 1, 250, 9, "B?token=b"),
            "B?token=b",
            "server B builds its own"
        );
        assert_eq!(
            key(&mut m, 0, 250, 7, "never built"),
            "A?token=a",
            "…and did not displace A's"
        );
        assert_eq!(builds.get(), 2);

        // a profile switch on A: A's entry is rebuilt at the new generation, B's stands
        assert_eq!(key(&mut m, 0, 250, 8, "A?token=a2"), "A?token=a2");
        assert_eq!(
            key(&mut m, 1, 250, 9, "never built"),
            "B?token=b",
            "B's generation never moved"
        );
        assert_eq!(builds.get(), 3);
        assert_eq!(
            key(&mut m, 0, 250, 8, "never built"),
            "A?token=a2",
            "the rebuild replaced, not appended"
        );
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
        assert!(
            idle_of(&[Pslot::ZERO; PT_CAP]),
            "an untouched store is idle"
        );
        for st in [P_READY, P_EVICTED, P_FAILED] {
            let slots = [Pslot {
                state: st,
                ..Pslot::ZERO
            }; PT_CAP];
            assert!(
                idle_of(&slots),
                "settled state {st} is not work in progress"
            );
        }
        for st in [P_WANT, P_LOADING, P_DECODED] {
            let mut slots = [Pslot {
                state: P_READY,
                ..Pslot::ZERO
            }; PT_CAP];
            slots[63] = Pslot {
                state: st,
                ..Pslot::ZERO
            };
            assert!(
                !idle_of(&slots),
                "one slot in state {st} is enough to hold the gate shut"
            );
        }
    }

    #[test]
    fn a_visible_claim_preempts_an_older_prefetch_backlog() {
        let mut slots = [Pslot::ZERO; PT_CAP];
        for slot in slots.iter_mut().take(12) {
            slot.state = P_WANT;
            slot.visible = false;
        }
        slots[47].state = P_WANT;
        slots[47].visible = true;

        assert_eq!(next_wanted(&slots), Some(47),
            "a tile being drawn must outrank every queued lookahead claim");
    }

    #[test]
    fn prefetch_admission_leaves_one_worker_for_visible_work() {
        let mut slots = [Pslot::ZERO; PT_CAP];
        assert!(warm_admissible(&slots), "an idle queue may look ahead");

        slots[9].state = P_LOADING;
        slots[9].visible = false;
        assert!(!warm_admissible(&slots),
            "one speculative fetch spends the entire prefetch allowance");

        slots[9] = Pslot::ZERO;
        slots[41].state = P_WANT;
        slots[41].visible = true;
        assert!(!warm_admissible(&slots),
            "visible work closes prefetch admission regardless of slot index");
    }
}
