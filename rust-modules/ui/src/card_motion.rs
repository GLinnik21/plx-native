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

#[derive(Clone, Copy)]
struct Sample { id: Identity, occurrence: usize, x: f32, y: f32, ms: u32 }

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
        self.observe_at(id, rect, ms, moving_limit())
    }

    /// [`observe`](Self::observe) against an explicit speed limit (px/s). Only the `Moving`
    /// threshold depends on it: a card with no previous sample is `Unknown` at any limit.
    fn observe_at(&mut self, id: Identity, rect: Rect, ms: u32, limit: f32) -> Verdict {
        if self.drawn_frame != self.frame {
            std::mem::swap(&mut self.previous, &mut self.current);
            self.current.clear();
            self.drawn_frame = self.frame;
        }
        // A single borrowed object may itself be drawn twice (e.g. outgoing/incoming
        // surfaces). Occurrences have independent histories; neither overwrites the
        // other's centre. Their traversal order is the card primitive's placement scope.
        let occurrence = self.current.iter().filter(|p| p.id == id).count();
        let (x, y) = (rect.cx(), rect.cy());
        let verdict = self.previous.iter().find(|p| p.id == id && p.occurrence == occurrence)
            .map_or(Verdict::Unknown, |old| {
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
        self.current.push(Sample { id, occurrence, x, y, ms });
        verdict
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
pub struct Scope(Option<Verdict>);
impl Scope {
    pub fn card(id: Identity, rect: Rect) -> Self {
        let v = HISTORY.with(|h| {
            let mut h = h.borrow_mut();
            #[cfg(feature = "devtriggers")]
            if h.frame != h.drawn_frame { super::card_motion_metrics::frame(); }
            h.observe(id, rect, frame_ms())
        });
        Self::enter(v)
    }
    fn enter(v: Verdict) -> Self { Self(ADMISSION.with(|s| s.replace(Some(v)))) }
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
