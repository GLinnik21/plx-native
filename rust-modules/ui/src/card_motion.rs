//! Poster admission belongs to the card's final screen placement, not its animators.
//!
//! `widgets::card` observes the centre AFTER the Painter transform and scopes all
//! of that card's source probes. A new positional term therefore participates without
//! a second reporting call. Heading lift, focus pop and hero/backdrop animation do
//! not move that centre. Featured images outside the card primitive (hero art,
//! logos and the Info panel's single still) have no scrolling tile demand and no
//! card scope; a popover translating its one selected image does not reveal new tiles.
//! Unknown placement defers a miss until the next drawn sample;
//! the source wakes that sample only when it actually declines work (hits stay cheap).
//! A card judged `Moving` (faster than [`MOVING_SPEED`]) is NOT deferred by default: the Library's
//! held-scroll tiles stayed dark under that gate (measured on the TV, 1000-movie mock), and the
//! at-rest lookahead plus an ungated draw shows far fewer of them. The classification is still
//! made, because `card_motion_metrics` and the poster-gate scenes count it; the dev trigger
//! `plxnative-cardspeed=<px/s>` brings the old decline back ([`declines_request`]).
//!
//! The same history carries each card's ART phase ([`ArtPhase`]), which is how the shared card draw
//! tells art that lands after a skeleton (it dissolves in, [`Scope::arrival`]) from art that was
//! there at the card's first draw (it shows whole).
//!
//! **One history advance per card per frame, whatever the number of walks.** The application draws
//! the page several times a frame: the discovery walk, one replay per stale glass source, and the
//! visible walk ([`crate::frame::backdrop::walk_serial`] names each). The FIRST walk of a frame that
//! draws a card advances its history: it reads the previous frame's sample and writes this frame's.
//! Every later walk of that frame REUSES that answer (the verdict, the art phase and how much of the
//! art shows) and records nothing new, so discovery, replay and visible draw the same alpha. A
//! card's identity in the history is `(owner, asset, occurrence)`, and the occurrence counts the
//! draws of that id within the CURRENT walk: two copies of one card inside a walk (the outgoing and
//! incoming surfaces) stay distinct, while the second walk's first draw is the first walk's first
//! draw again. Keying by the draw's place in the whole frame instead let a replay that started
//! mid-dissolve push the visible draw to an occurrence the previous frame never had, which read as
//! "art that was there from the first draw" and showed it whole in one frame (DEBUG-5).
//!
//! This is render history, never LogicalState. Two reused vectors retain only the
//! previous/current drawn frame; walking a 1000-item document does not retain 1000
//! cards. Capacity follows peak simultaneous draws, with no fixed-cap eviction that
//! could keep a visible card perpetually unknown. After warm-up frames allocate nothing.
use crate::Rect;
use std::cell::{Cell, RefCell};

/// The speed (px/s) above which a card is classified `Moving`. A MEASUREMENT, not a gate: by
/// default a `Moving` card still gets its poster work ([`declines_request`]). The dev trigger
/// `plxnative-cardspeed` replaces it ([`moving_limit`]) and turns the classification back into the
/// decline the shipping app used to apply.
const MOVING_SPEED: f32 = 120.0;
const MAX_SAMPLE_MS: u32 = 250;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict { Unknown, Moving, Settled }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The borrowed row/path distinguishes two catalog placements of the same art.
    pub owner: usize,
    pub asset: u64,
}

/// **Where a card's art stood the last frame it was drawn**, carried in the same render history as
/// its placement. It is what lets the shared card draw tell art that arrives LATE (drawn as a
/// skeleton one frame, resident the next) from art that was there when the card first appeared
/// (never a skeleton, so nothing to dissolve over): only the first fades in ([`Scope::arrival`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ArtPhase {
    /// Nothing to dissolve: the art was there from the first draw, or its fade is done.
    #[default]
    Settled,
    /// The card was drawn as a skeleton: whatever lands next is late.
    Waiting,
    /// The art landed at frame time `since` (ms) and is dissolving in over the skeleton.
    Arriving { since: u32 },
}

/// One card's history for one frame. `walk` is the last walk that drew it, which is how a draw
/// counts its own occurrence within its walk; `verdict`, `art` and `shown` are the frame's answer,
/// which a later walk reuses.
#[derive(Clone, Copy)]
struct Sample { id: Identity, occurrence: usize, walk: u64, x: f32, y: f32, ms: u32, verdict: Verdict, art: ArtPhase, shown: f32 }

/// What one draw of a card found in the history ([`History::observe_in`]).
#[derive(Clone, Copy)]
struct Observed {
    verdict: Verdict,
    /// The card's sample this frame.
    slot: usize,
    /// `Some(the art phase the previous frame ended in)` when this draw advanced the history;
    /// `None` when an earlier walk of the frame already had, and this draw reuses its answer.
    advanced_from: Option<ArtPhase>,
}

#[derive(Default)]
pub struct History {
    frame: u64,
    drawn_frame: u64,
    previous: Vec<Sample>,
    current: Vec<Sample>,
}
impl History {
    pub fn begin(&mut self) { self.frame = self.frame.wrapping_add(1); }

    pub fn observe(&mut self, id: Identity, rect: Rect, ms: u32) -> Verdict {
        self.observe_full(id, rect, ms).verdict
    }

    /// [`observe`](Self::observe) plus the slot of the card's sample and whether this draw advanced
    /// the card's history, for [`Scope::card`].
    fn observe_full(&mut self, id: Identity, rect: Rect, ms: u32) -> Observed {
        self.observe_in(crate::frame::backdrop::walk_serial(), id, rect, ms, moving_limit())
    }

    /// [`observe`](Self::observe) against an explicit speed limit (px/s). Only the `Moving`
    /// threshold depends on it: a card with no previous sample is `Unknown` at any limit.
    fn observe_at(&mut self, id: Identity, rect: Rect, ms: u32, limit: f32) -> Verdict {
        self.observe_in(crate::frame::backdrop::walk_serial(), id, rect, ms, limit).verdict
    }

    /// Record where this frame's draw of the card's art ended up, for the next frame to read and
    /// for the frame's later walks to reuse.
    fn set_art(&mut self, slot: usize, art: ArtPhase, shown: f32) {
        if let Some(sample) = self.current.get_mut(slot) { sample.art = art; sample.shown = shown; }
    }

    /// The art phase and visible share the frame's first draw of this sample settled on.
    fn art_of(&self, slot: usize) -> (ArtPhase, f32) {
        self.current.get(slot).map_or((ArtPhase::Settled, 1.0), |s| (s.art, s.shown))
    }

    /// One draw of `id` by walk `walk`. The draw's occurrence is its place among this walk's draws
    /// of the id. If this frame already has a sample for that occurrence (an earlier walk drew the
    /// card) the draw reuses it; otherwise it advances the history from the previous frame's sample.
    /// The reuse stamps `sample.walk` with the reusing walk, so it holds only while no walk draws a
    /// card both before AND after a nested walk (`enter`/`discover`) that also draws it: the outer
    /// walk's earlier draw would no longer count as its own and its later draw would take the same
    /// occurrence again. In production (`app/run.rs`) the visible walk draws nothing before the
    /// discovery walk and the replays, which are leaf scopes.
    fn observe_in(&mut self, walk: u64, id: Identity, rect: Rect, ms: u32, limit: f32) -> Observed {
        if self.drawn_frame != self.frame {
            std::mem::swap(&mut self.previous, &mut self.current);
            self.current.clear();
            self.drawn_frame = self.frame;
        }
        // A single borrowed object may itself be drawn twice (e.g. outgoing/incoming
        // surfaces). Occurrences have independent histories; neither overwrites the
        // other's centre. Their traversal order is the card primitive's placement scope.
        let occurrence = self.current.iter().filter(|p| p.id == id && p.walk == walk).count();
        if let Some(slot) = self.current.iter().position(|p| p.id == id && p.occurrence == occurrence) {
            let sample = &mut self.current[slot];
            sample.walk = walk;
            return Observed { verdict: sample.verdict, slot, advanced_from: None };
        }
        let (x, y) = (rect.cx(), rect.cy());
        let previous = self.previous.iter().find(|p| p.id == id && p.occurrence == occurrence);
        let verdict = previous.map_or(Verdict::Unknown, |old| {
            let dt = ms.wrapping_sub(old.ms);
            if dt == 0 || dt > MAX_SAMPLE_MS || !x.is_finite() || !y.is_finite()
                || !old.x.is_finite() || !old.y.is_finite() {
                return Verdict::Unknown;
            }
            let speed = (x - old.x).abs().max((y - old.y).abs()) * 1000.0 / dt as f32;
            if !speed.is_finite() { Verdict::Unknown }
            else if speed > limit { Verdict::Moving }
            else { Verdict::Settled }
        });
        let art = previous.map_or(ArtPhase::Settled, |old| old.art);
        self.current.push(Sample { id, occurrence, walk, x, y, ms, verdict, art, shown: 1.0 });
        Observed { verdict, slot: self.current.len() - 1, advanced_from: Some(art) }
    }
}

/// **The art phase after this frame's draw, and how much of the art shows**, from the phase the
/// card's previous frame ended in, whether its texture is resident now (`hit`) and the frame time.
/// Pure. A miss is `Waiting` (the skeleton is up and whatever lands is late); the first hit after
/// a miss starts the dissolve ([`xfade::arrived`](crate::xfade::arrived), 0 at its start and 1
/// after [`xfade::IN_MS`](crate::xfade::IN_MS)); a hit with nothing to dissolve over is fully
/// shown. Frame time that goes backwards (a replay rewound) reads as the start.
fn art_after(prev: ArtPhase, hit: bool, now: u32) -> (ArtPhase, f32) {
    if !hit { return (ArtPhase::Waiting, 0.0) }
    match prev {
        ArtPhase::Settled => (ArtPhase::Settled, 1.0),
        ArtPhase::Waiting => (ArtPhase::Arriving { since: now }, 0.0),
        ArtPhase::Arriving { since } => {
            let age = if now >= since { (now - since) as f32 } else { 0.0 };
            if age >= crate::xfade::IN_MS { (ArtPhase::Settled, 1.0) } else { (ArtPhase::Arriving { since }, crate::xfade::arrived(age)) }
        }
    }
}

/// The speed limit the dev trigger `plxnative-cardspeed=<px/s>` imposed, if it did: while it is
/// `Some`, a card faster than it is `Moving` AND declines poster work ([`declines_request`]), which
/// reproduces the gate this app shipped with (`cardspeed=120`) for an A/B on the device. Read ONCE
/// (at the first card drawn, i.e. before any measured frame) so the hot path is an atomic load,
/// never a `/tmp` stat. `devtrig::read` is `None` at COMPILE time without the `devtriggers`
/// feature, so a release build is always `None` and this carries no `#[cfg]` pair.
///
/// This crate's own unit tests never read the host's trigger file (a leftover
/// `/tmp/plxnative-cardspeed` on a dev machine would change every speed they classify): they see
/// no trigger, and [`parse_limit`] is the pure rule they grade.
fn enforced_limit() -> Option<f32> {
    static SEEN: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
    *SEEN.get_or_init(|| {
        #[cfg(test)]
        let text: Option<String> = None;
        #[cfg(not(test))]
        let text = plx_base::devtrig::read("cardspeed");
        let text = text?;
        let limit = parse_limit(&text);
        match limit {
            Some(limit) => plx_base::eventlog::log(&format!(
                "card_motion: moving cards above {limit} px/s decline poster work (plxnative-cardspeed)")),
            None => plx_base::eventlog::log(&format!(
                "card_motion: plxnative-cardspeed {:?} ignored (want px/s > 0); moving cards keep their poster work",
                text.trim())),
        }
        limit
    })
}

/// The speed above which a card is `Moving`: the trigger's limit, else [`MOVING_SPEED`].
fn moving_limit() -> f32 { enforced_limit().unwrap_or(MOVING_SPEED) }

/// A trigger's content as a limit: a finite number > 0 in px/s; anything else is `None` (the
/// trigger is ignored and the default stands).
fn parse_limit(text: &str) -> Option<f32> {
    text.trim().parse::<f32>().ok().filter(|v| v.is_finite() && *v > 0.0)
}

thread_local! {
    static HISTORY: RefCell<History> = RefCell::new(History::default());
    static FRAME_MS: Cell<u32> = const { Cell::new(0) };
    static ADMISSION: Cell<Option<Verdict>> = const { Cell::new(None) };
}
#[cfg(any(test, feature = "test-support"))]
thread_local! { static ENFORCE_FOR_TEST: Cell<Option<bool>> = const { Cell::new(None) }; }

/// The actual loop timestamp (or replay timestamp), before spring integration
/// clamps its dt. Pixel speed must not inherit the spring's 50ms stall clamp.
pub fn begin_frame(ms: u32) {
    FRAME_MS.with(|clock| clock.set(ms));
    HISTORY.with(|h| h.borrow_mut().begin());
}
pub(super) fn frame_ms() -> u32 { FRAME_MS.with(Cell::get) }

/// A card owns every art probe until its draw returns, including early returns.
/// Calls outside this scope (hero art, logos, warm requests) have no card-motion gate.
pub struct Scope(Option<Verdict>, Option<Track>);
/// The card's art history this scope may read: its sample's slot, and the phase the previous frame
/// ended in when this scope is the frame's first draw of the card and so advances it. A scope that
/// does not advance reuses the answer the first walk's draw stored in the slot.
#[derive(Clone, Copy)]
struct Track { slot: usize, advances_from: Option<ArtPhase> }
impl Scope {
    pub fn card(id: Identity, rect: Rect) -> Self {
        let seen = HISTORY.with(|h| {
            let mut h = h.borrow_mut();
            #[cfg(feature = "devtriggers")]
            if h.frame != h.drawn_frame { super::card_motion_metrics::frame(); }
            h.observe_full(id, rect, frame_ms())
        });
        let mut scope = Self::enter(seen.verdict);
        scope.1 = Some(Track { slot: seen.slot, advances_from: seen.advanced_from });
        scope
    }
    fn enter(v: Verdict) -> Self { Self(ADMISSION.with(|s| s.replace(Some(v))), None) }

    /// **How much of the card's art shows this frame, 0..1**: the art-arrival fade of the shared
    /// card draw. `hit` is whether the probe came back with a texture. Art that was there the first
    /// time the card drew (a texture resident before the card was on screen) shows at once; art
    /// that lands after the card drew a skeleton dissolves in over it on the page fade's own
    /// duration and curve, instead of cutting in. A dissolve in flight asks the frame gate for the
    /// next frame (a ms ramp is invisible to the spring instrumentation, as `Xfade::tick` says).
    /// Only the frame's first draw of the card works this out; every later walk of the frame (a
    /// glass source replay, the visible walk) gets the same answer back, so the picture a glass
    /// filters is the picture the panel shows. Call once per scope; a scope that tracks nothing (a
    /// test's, a card with no identity) shows everything.
    pub fn arrival(&self, hit: bool) -> f32 {
        let Some(Track { slot, advances_from }) = self.1 else { return 1.0 };
        let (phase, shown) = match advances_from {
            Some(prev) => {
                let (phase, shown) = art_after(prev, hit, frame_ms());
                HISTORY.with(|h| h.borrow_mut().set_art(slot, phase, shown));
                (phase, shown)
            }
            None => HISTORY.with(|h| h.borrow().art_of(slot)),
        };
        if matches!(phase, ArtPhase::Arriving { .. }) { plx_machine::idle::invalidate(); }
        shown
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn moving_for_test() -> Self { Self::enter(Verdict::Moving) }
    #[cfg(any(test, feature = "test-support"))]
    pub fn unknown_for_test() -> Self { Self::enter(Verdict::Unknown) }
}

/// While the guard lives, a `Moving` card declines ([`declines_request`]) as it does under
/// `plxnative-cardspeed`; the default admits it. The limit itself is untouched.
#[cfg(any(test, feature = "test-support"))]
pub struct EnforceGuard(Option<bool>);
#[cfg(any(test, feature = "test-support"))]
pub fn enforce_for_test() -> EnforceGuard { EnforceGuard(ENFORCE_FOR_TEST.with(|c| c.replace(Some(true)))) }
/// While the guard lives, a `Moving` card is admitted - the shipping default - whatever
/// `/tmp/plxnative-cardspeed` the host happens to hold. A test that asserts the default takes this
/// instead of trusting the latched trigger read, which a leftover file on a dev machine flips.
#[cfg(any(test, feature = "test-support"))]
pub fn default_for_test() -> EnforceGuard { EnforceGuard(ENFORCE_FOR_TEST.with(|c| c.replace(Some(false)))) }
#[cfg(any(test, feature = "test-support"))]
impl Drop for EnforceGuard {
    fn drop(&mut self) { ENFORCE_FOR_TEST.with(|c| c.set(self.0)); }
}
impl Drop for Scope {
    fn drop(&mut self) { ADMISSION.with(|s| s.set(self.0)); }
}

pub fn verdict() -> Option<Verdict> { ADMISSION.with(|s| s.get()) }

/// Whether a `Moving` card declines (the trigger `plxnative-cardspeed` is armed).
fn declines_moving() -> bool {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(forced) = ENFORCE_FOR_TEST.with(Cell::get) { return forced; }
    enforced_limit().is_some()
}

/// Should a card DRAW decline to start art work for this card? An `Unknown` placement always does
/// (one sample is needed to know a speed); a `Moving` one only under `plxnative-cardspeed`.
///
/// **Never in a frame dump** ([`plx_gfx::dump`]): a held repeat samples at `dt == 0`, which is
/// `Unknown` by construction, and a card that scrolled in `Moving` is `Moving` for the same reason
/// on every repeat, so under a frozen clock the verdict can never settle and the declined request
/// would never be made (measured: one `down` on Home held 56,670 iterations on `card-skeleton`).
/// Admitting everything is safe there because the dump draws one deterministic frame at a time
/// and holds until every placeholder is gone. Outside the simulator `armed()` is a constant
/// `false` and this is the verdict test alone.
pub fn declines_request() -> bool {
    declines_in(plx_gfx::dump::armed(), verdict(), declines_moving())
}

/// [`declines_request`] as a pure function of the dump switch, the verdict and whether a `Moving`
/// card declines.
#[inline]
fn declines_in(dump: bool, verdict: Option<Verdict>, moving_declines: bool) -> bool {
    !dump && match verdict {
        Some(Verdict::Unknown) => true,
        Some(Verdict::Moving) => moving_declines,
        Some(Verdict::Settled) | None => false,
    }
}

/// A declined miss has no worker whose completion could wake it. The next sample
/// MUST present, including an unknown card first appearing on an otherwise idle page.
pub fn deferred() { plx_machine::idle::invalidate(); }

#[cfg(test)]
mod tests {
    use super::*;
    const ID: Identity = Identity { owner: 1, asset: 7 };
    fn sample(h: &mut History, x: f32, y: f32, ms: u32) -> Verdict {
        h.begin(); h.observe(ID, Rect::new(x, y, 250.0, 375.0), ms)
    }
    /// A card that moves `px` pixels in one second, judged against `limit`.
    fn step_verdict(px: f32, limit: f32) -> Verdict {
        let mut h = History::default();
        h.begin(); h.observe_at(ID, Rect::new(0.0, 0.0, 250.0, 375.0), 0, limit);
        h.begin(); h.observe_at(ID, Rect::new(px, 0.0, 250.0, 375.0), 200, limit)
    }
    #[test]
    fn the_classification_limit_is_120_and_an_absent_trigger_declines_no_moving_card() {
        assert_eq!(MOVING_SPEED, 120.0);
        // The pure rule, not the latched /tmp read: a leftover trigger file on the host must not
        // change what the default is.
        assert_eq!(parse_limit("junk"), None, "an unusable trigger is ignored and the default stands");
        assert_eq!(parse_limit("480"), Some(480.0));
        assert!(!declines_in(false, Some(Verdict::Moving), false), "no plxnative-cardspeed trigger");
        let _default = default_for_test();
        assert!(!declines_moving());
    }
    #[test]
    fn an_override_moves_only_the_moving_threshold() {
        // 60 px in 200 ms = 300 px/s.
        assert_eq!(step_verdict(60.0, 120.0), Verdict::Moving);
        assert_eq!(step_verdict(60.0, 480.0), Verdict::Settled);
        assert_eq!(step_verdict(0.0, 120.0), Verdict::Settled);
        // A card with no previous sample still declines at any limit.
        let mut h = History::default();
        h.begin();
        assert_eq!(h.observe_at(ID, Rect::new(0.0, 0.0, 250.0, 375.0), 0, 1.0e9), Verdict::Unknown);
    }
    #[test]
    fn limit_parsing_accepts_positive_numbers_and_rejects_everything_else() {
        assert_eq!(parse_limit("480"), Some(480.0));
        assert_eq!(parse_limit(" 300.5\n"), Some(300.5));
        for bad in ["", "abc", "0", "-5", "-0", "nan", "inf", "-inf", "1e999", "12px", "off"] {
            assert_eq!(parse_limit(bad), None, "{bad:?} must leave the default (no moving decline)");
        }
    }
    #[test]
    fn arbitrary_derived_displacement_is_seen_and_settle_admits() {
        let mut h = History::default();
        assert_eq!(sample(&mut h, 0.0, 0.0, 0), Verdict::Unknown);
        // No spring/report API: a product, a band and any new offset are already pixels.
        assert_eq!(sample(&mut h, 4000.0 * 0.02, 617.0 * 0.02 + 8.0, 16), Verdict::Moving);
        assert_eq!(sample(&mut h, 80.0, 20.34, 32), Verdict::Settled);
    }
    #[test]
    fn band_only_motion_and_combined_subthreshold_terms_are_seen() {
        let mut h = History::default();
        sample(&mut h, 0.0, 0.0, 0);
        assert_eq!(sample(&mut h, 0.0, 1.1 + 1.1, 16), Verdict::Moving);
        assert_eq!(sample(&mut h, 0.0, 2.2, 32), Verdict::Settled);
    }
    #[test]
    fn cancelling_motion_and_focus_pop_do_not_defer_stationary_cards() {
        let mut h = History::default();
        sample(&mut h, 0.0, 0.0, 0);
        h.begin();
        let rect = Rect::new(0.0, 60.0 - 60.0, 250.0, 375.0).scaled(1.1);
        assert_eq!(h.observe(ID, rect, 16), Verdict::Settled);
    }
    #[test]
    fn duplicate_art_and_duplicate_owners_have_independent_placements() {
        let mut h = History::default();
        let rects = [Rect::new(0.0, 0.0, 250.0, 375.0), Rect::new(800.0, 600.0, 250.0, 375.0)];
        for frame in 0..3 {
            h.begin();
            for r in rects {
                assert_eq!(h.observe(ID, r, frame * 16), if frame == 0 { Verdict::Unknown } else { Verdict::Settled });
            }
        }
    }
    #[test]
    fn oversized_visible_sets_settle_and_history_does_not_grow_with_the_document() {
        let mut h = History::default();
        for page in 0..10 {
            for pass in 0..2 {
                h.begin();
                for n in 0..513 {
                    let id = Identity { owner: page * 513 + n, asset: 7 };
                    let v = h.observe(id, Rect::new(n as f32, 0.0, 1.0, 1.0), (page * 32 + pass * 16) as u32);
                    assert_eq!(v, if pass == 0 { Verdict::Unknown } else { Verdict::Settled });
                }
                assert!(h.current.len() <= 513 && h.previous.len() <= 513);
            }
        }
    }
    /// The walks of one frame, driven with explicit walk serials (`backdrop::walk_serial`).
    #[test]
    fn a_later_walk_of_the_frame_reuses_the_first_walks_answer_and_records_nothing() {
        let mut h = History::default();
        let rect = Rect::new(0.0, 0.0, 250.0, 375.0);
        let (discovery, replay, visible) = (11, 13, 12);
        for frame in 0..3u32 {
            h.begin();
            let first = h.observe_in(discovery, ID, rect, frame * 16, 120.0);
            assert!(first.advanced_from.is_some(), "the first walk advances the card's history");
            h.set_art(first.slot, ArtPhase::Arriving { since: 5 }, 0.5);
            // The glass replay and the visible walk draw the same card at another place: both
            // take the first walk's answer, and the history holds one sample for the three draws.
            for walk in [replay, visible] {
                let later = h.observe_in(walk, ID, Rect::new(900.0, 900.0, 250.0, 375.0), frame * 16, 120.0);
                assert!(later.advanced_from.is_none(), "frame {frame}: walk {walk} reuses");
                assert_eq!((later.slot, later.verdict), (first.slot, first.verdict));
                assert_eq!(h.art_of(later.slot), (ArtPhase::Arriving { since: 5 }, 0.5));
            }
            assert_eq!(h.current.len(), 1);
            assert_eq!(first.verdict, if frame == 0 { Verdict::Unknown } else { Verdict::Settled },
                "and the answer is the one the previous frame's sample gives");
        }
    }

    #[test]
    fn two_copies_in_one_walk_stay_distinct_across_walks_and_a_card_only_a_later_walk_draws_advances() {
        let mut h = History::default();
        let rect = Rect::new(0.0, 0.0, 250.0, 375.0);
        let other = Identity { owner: 2, asset: 7 };
        for frame in 0..2u32 {
            h.begin();
            let ms = frame * 16;
            let slots: Vec<usize> = [1, 2].into_iter().flat_map(|walk| {
                let a = h.observe_in(walk, ID, rect, ms, 120.0);
                let b = h.observe_in(walk, ID, Rect::new(800.0, 600.0, 250.0, 375.0), ms, 120.0);
                assert_eq!((a.advanced_from.is_some(), b.advanced_from.is_some()), (walk == 1, walk == 1));
                [a.slot, b.slot]
            }).collect();
            assert_eq!(slots, [0, 1, 0, 1], "walk 2's copies are walk 1's copies, in order");
            assert_eq!(h.current.len(), 2);
            let only_later = h.observe_in(2, other, rect, ms, 120.0);
            assert!(only_later.advanced_from.is_some(), "no earlier walk drew it: its history advances here");
            assert_eq!(only_later.verdict, if frame == 0 { Verdict::Unknown } else { Verdict::Settled });
        }
    }

    #[test]
    fn a_nonfinite_previous_axis_cannot_authorize_work_through_f32_max() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut h = History::default();
            assert_eq!(sample(&mut h, bad, 0.0, 0), Verdict::Unknown);
            assert_eq!(sample(&mut h, 0.0, 0.0, 16), Verdict::Unknown);
            assert_eq!(sample(&mut h, 0.0, 0.0, 32), Verdict::Settled);
        }
    }

    #[test]
    fn actual_frame_time_not_clamped_spring_time_owns_pixel_speed() {
        let id = Identity { owner: 987, asset: 123 };
        let rect = Rect::new(0.0, 0.0, 250.0, 375.0);
        begin_frame(0);
        { let _scope = Scope::card(id, rect); assert_eq!(verdict(), Some(Verdict::Unknown)); }
        begin_frame(100); // A 100ms frame moves 10px =100px/s; clamping to50ms would say200.
        { let _scope = Scope::card(id, Rect::new(10.0, 0.0, 250.0, 375.0)); assert_eq!(verdict(), Some(Verdict::Settled)); }
    }
    #[test]
    fn zero_time_stale_and_wrapping_samples_are_explicit() {
        let mut h = History::default();
        sample(&mut h, 0.0, 0.0, 0);
        assert_eq!(sample(&mut h, 0.0, 0.0, 0), Verdict::Unknown);
        assert_eq!(sample(&mut h, 0.0, 0.0, 16), Verdict::Settled);
        assert_eq!(sample(&mut h, 0.0, 0.0, 500), Verdict::Unknown);
        assert_eq!(sample(&mut h, 0.0, 0.0, 516), Verdict::Settled);
        sample(&mut h, 0.0, 0.0, u32::MAX - 8);
        assert_eq!(sample(&mut h, 0.0, 0.0, 7), Verdict::Settled);
    }

    #[test]
    fn scopes_exclude_hero_work_restore_on_exit_and_unknown_misses_wake() {
        let _guard = plx_base::testlock::serial();
        let _enforce = enforce_for_test();
        assert!(!declines_request());
        {
            let _scope = Scope::moving_for_test();
            assert!(declines_request(), "under plxnative-cardspeed a moving card declines");
            let _inner = Scope::enter(Verdict::Settled);
            assert!(!declines_request());
        }
        assert!(!declines_request(), "hero/heading work outside card() has no motion scope");
        plx_machine::idle::take_local_damage();
        deferred();
        assert!(plx_machine::idle::take_local_damage() > 0, "a declined miss must request its next observation");
    }

    #[test]
    fn by_default_only_an_unknown_placement_declines() {
        let _guard = plx_base::testlock::serial();
        let _default = default_for_test();
        {
            let _scope = Scope::moving_for_test();
            assert!(!declines_request(), "a moving card is admitted by default");
            let _unknown = Scope::enter(Verdict::Unknown);
            assert!(declines_request(), "one sample is still needed to know a speed");
        }
        let enforce = enforce_for_test();
        let _scope = Scope::moving_for_test();
        assert!(declines_request());
        drop(enforce);
        assert!(!declines_request(), "the guard restores the default");
    }

    #[test]
    fn a_dump_never_declines_a_draw_for_card_motion() {
        // Held repeats sample at dt == 0, which is `Unknown`; a frozen clock never leaves it.
        for v in [Verdict::Unknown, Verdict::Moving] {
            assert!(declines_in(false, Some(v), true), "outside a dump {v:?} declines (cardspeed armed)");
            assert!(!declines_in(true, Some(v), true), "a dump admits every card draw ({v:?})");
        }
        assert!(declines_in(false, Some(Verdict::Unknown), false) && !declines_in(false, Some(Verdict::Moving), false));
        assert!(!declines_in(false, Some(Verdict::Settled), true) && !declines_in(false, None, true));
    }
}
