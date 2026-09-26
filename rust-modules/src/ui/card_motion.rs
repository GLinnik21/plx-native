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
//!
//! This is render history, never LogicalState. Two reused vectors retain only the
//! previous/current drawn frame; walking a 1000-item document does not retain 1000
//! cards. Capacity follows peak simultaneous draws, with no fixed-cap eviction that
//! could keep a visible card perpetually unknown. After warm-up frames allocate nothing.
use crate::ui::Rect;
use std::cell::{Cell, RefCell};

const MAX_SPEED: f32 = 120.0;
const MAX_SAMPLE_MS: u32 = 250;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict { Unknown, Moving, Settled }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    /// The borrowed row/path distinguishes two catalog placements of the same art.
    pub owner: usize,
    pub asset: u64,
}

#[derive(Clone, Copy)]
struct Sample { id: Identity, occurrence: usize, x: f32, y: f32, ms: u32 }

#[derive(Default)]
pub(crate) struct History {
    frame: u64,
    drawn_frame: u64,
    previous: Vec<Sample>,
    current: Vec<Sample>,
}
impl History {
    pub(crate) fn begin(&mut self) { self.frame = self.frame.wrapping_add(1); }

    pub(crate) fn observe(&mut self, id: Identity, rect: Rect, ms: u32) -> Verdict {
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
                else if speed > MAX_SPEED { Verdict::Moving }
                else { Verdict::Settled }
            });
        self.current.push(Sample { id, occurrence, x, y, ms });
        verdict
    }
}

thread_local! {
    static HISTORY: RefCell<History> = RefCell::new(History::default());
    static FRAME_MS: Cell<u32> = const { Cell::new(0) };
    static ADMISSION: Cell<Option<Verdict>> = const { Cell::new(None) };
}

/// The actual loop timestamp (or replay timestamp), before spring integration
/// clamps its dt. Pixel speed must not inherit the spring's 50ms stall clamp.
pub(crate) fn begin_frame(ms: u32) {
    FRAME_MS.with(|clock| clock.set(ms));
    HISTORY.with(|h| h.borrow_mut().begin());
}
pub(super) fn frame_ms() -> u32 { FRAME_MS.with(Cell::get) }

/// A card owns every art probe until its draw returns, including early returns.
/// Calls outside this scope (hero art, logos, warm requests) have no card-motion gate.
pub(crate) struct Scope(Option<Verdict>);
impl Scope {
    pub(crate) fn card(id: Identity, rect: Rect) -> Self {
        let v = HISTORY.with(|h| h.borrow_mut().observe(id, rect, frame_ms()));
        Self::enter(v)
    }
    fn enter(v: Verdict) -> Self { Self(ADMISSION.with(|s| s.replace(Some(v)))) }
    #[cfg(test)]
    pub(crate) fn moving_for_test() -> Self { Self::enter(Verdict::Moving) }
}
impl Drop for Scope {
    fn drop(&mut self) { ADMISSION.with(|s| s.set(self.0)); }
}

pub(crate) fn verdict() -> Option<Verdict> { ADMISSION.with(|s| s.get()) }
pub(crate) fn declines_request() -> bool {
    matches!(verdict(), Some(Verdict::Unknown | Verdict::Moving))
}

/// A declined miss has no worker whose completion could wake it. The next sample
/// MUST present, including an unknown card first appearing on an otherwise idle page.
pub(crate) fn deferred() { crate::ui::idle::invalidate(); }

#[cfg(test)]
mod tests {
    use super::*;
    const ID: Identity = Identity { owner: 1, asset: 7 };
    fn sample(h: &mut History, x: f32, y: f32, ms: u32) -> Verdict {
        h.begin(); h.observe(ID, Rect::new(x, y, 250.0, 375.0), ms)
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
        let _guard = crate::testlock::serial();
        assert!(!declines_request());
        {
            let _scope = Scope::moving_for_test();
            assert!(declines_request());
            let _inner = Scope::enter(Verdict::Settled);
            assert!(!declines_request());
        }
        assert!(!declines_request(), "hero/heading work outside card() has no motion scope");
        crate::ui::idle::take_local_damage();
        deferred();
        assert!(crate::ui::idle::take_local_damage() > 0, "a declined miss must request its next observation");
    }
}
