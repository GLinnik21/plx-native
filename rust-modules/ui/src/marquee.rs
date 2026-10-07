//! **The focused run's marquee** — one looping glide for a single line of text that does not fit
//! its window: a beat of rest so the couch can start reading, a slow glide left, a gap, then the
//! same run re-entering from the right, forever while it holds focus.
//!
//! Three users, one implementation (`ui/src/CLAUDE.md` rule 4): a focused tile's title under its
//! card (`card_row`, [`TITLE`]), a focused cast headshot's name and role (the detail page's cast
//! shelf, [`TITLE`], through [`Marquee::glide`] like the title), and a focused menu row's label
//! (`table`, [`ROW`]) — a downloaded subtitle's release name is routinely three times the width
//! of the track menu. They differ only in the rest beat, and each keeps its OWN [`Clock`],
//! because a popover menu over a shelf draws both focused runs in one frame and a shared clock
//! would restart on every draw.
//!
//! Draw TWO copies at `x - offset` and `x - offset + text_w + GAP` (the follower), both clipped to
//! the window — the follower is what makes the wrap seamless instead of a visible pop back to the
//! start: by the time `offset` reaches the full travel distance the follower has arrived exactly
//! where the primary run started, so the loop boundary is invisible.

use crate::{Painter, Rect};
use std::cell::{Cell, RefCell};

/// Glide speed, in px/s. Chosen to be readable from a couch rather than merely legible paused — a
/// scrolling news-ticker speed blurs on a TV's own motion smoothing.
pub const SPEED: f32 = 40.0;
/// Air between the outgoing run's tail and its follower's head — enough that the two never read as
/// one run with a repeated word running into itself.
pub const GAP: f32 = 60.0;

/// One marquee's timing. Everything but the rest beat is shared ([`SPEED`], [`GAP`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marquee {
    /// How long the run rests, left edge in the window, before it starts gliding — and again at
    /// the start of every loop.
    pub hold_ms: f32,
}

/// A focused tile's title: long enough to read the opening words of an ordinary title before it
/// moves.
pub const TITLE: Marquee = Marquee { hold_ms: 1000.0 };
/// A focused menu row's label. Longer than [`TITLE`]: a menu row is read while deciding, and its
/// opening words (a release's title and year) are usually the part that tells two rows apart, so
/// the run stays put for a couple of seconds before it scrolls to show the rest.
pub const ROW: Marquee = Marquee { hold_ms: 2000.0 };

impl Marquee {
    /// The horizontal offset at `t_ms` since this run became (or stayed) focused, for a run
    /// `text_w` px wide inside a `budget` px window. A pure function of elapsed time, so it is
    /// trivially testable and resettable — the caller just starts `t_ms` back at zero. `0` for the
    /// first [`Self::hold_ms`], then a glide at [`SPEED`] until the run plus [`GAP`] of air has
    /// fully passed, then loops. A run that fits (`text_w <= budget`) is inert at every `t_ms`.
    pub fn x(&self, t_ms: f32, text_w: f32, budget: f32) -> f32 {
        if text_w <= budget {
            return 0.0;
        }
        let t = t_ms.max(0.0) % self.period(text_w);
        if t < self.hold_ms {
            0.0
        } else {
            (t - self.hold_ms) / 1000.0 * SPEED
        }
    }

    /// Is the run actually GLIDING at `t_ms` — the frame that must keep [`plx_machine::idle`]
    /// awake with an invalidate? `false` during the rest beat and whenever the run fits. A separate
    /// question from [`Self::x`] rather than "moved since last frame": two resting frames draw
    /// nothing different, which is exactly what must NOT report as motion.
    pub fn moving(&self, t_ms: f32, text_w: f32, budget: f32) -> bool {
        if text_w <= budget {
            return false;
        }
        t_ms.max(0.0) % self.period(text_w) >= self.hold_ms
    }

    /// One full cycle in ms — the rest beat plus the glide that carries the run and its [`GAP`]
    /// fully past. The ONE place the period is spelled.
    pub fn period(&self, text_w: f32) -> f32 {
        self.hold_ms + (text_w + GAP) / SPEED * 1000.0
    }

    /// Fold the `f64` clock onto one period BEFORE it becomes an `f32`. An `f32` of a six-day-old
    /// clock (~2^29 ms) only moves in 64 ms steps — a 2.5 px judder at [`SPEED`] rather than a
    /// glide (Codex review, 2026-09-02). A phase is never larger than one period, so it is exact.
    pub fn phase(&self, t_ms: f64, text_w: f32, budget: f32) -> f32 {
        if text_w <= budget {
            return 0.0;
        }
        (t_ms.max(0.0) % self.period(text_w) as f64) as f32
    }

    /// Keep the present gate honest for a run drawn at `t_ms` that does NOT fit: the clock
    /// advances by [`plx_machine::idle::now_ms`] on DRAWN frames only, so the rest beat has to buy
    /// its own frames or it never ends — a focused overflowing title was reproduced sitting clipped
    /// and motionless at 4 s and again at 7 s of focus (sim, 2026-09-02). `wake` (a frame, no claim
    /// that pixels changed) for the hold; `invalidate` for the glide, where they do.
    pub fn report(&self, t_ms: f32, text_w: f32, budget: f32) {
        if self.moving(t_ms, text_w, budget) {
            plx_machine::idle::invalidate();
        } else {
            plx_machine::idle::wake();
        }
    }

    /// **Paint one overflowing run as a marquee** — the ONE draw every user of this module goes
    /// through, so a poster's title and a headshot's name and role cannot drift apart.
    ///
    /// `key` is the run's text (the [`Clock`]'s identity), `run_w` its drawn width, `window` the
    /// clip it glides inside (taller than the glyphs by the caller's descender allowance; its
    /// width is the budget). `paint(dx)` draws the run shifted by `dx` from where it rests; it is
    /// called twice, for the run and for its follower [`GAP`] behind. Call it only for a run that
    /// does NOT fit (`run_w > window.w`) — a fitting focused run should
    /// [`Clock::release`] instead, which is the caller's branch to take.
    pub fn glide(
        &self,
        clock: &Clock,
        p: Painter,
        key: &str,
        run_w: f32,
        window: Rect,
        paint: impl Fn(f32),
    ) {
        let t_ms = self.phase(clock.read(key), run_w, window.w);
        self.report(t_ms, run_w, window.w);
        let off = self.x(t_ms, run_w, window.w);
        p.clip(window);
        paint(-off);
        paint(-off + run_w + GAP);
        p.clip_clear();
    }
}

/// Which run owns a marquee, and since when. One focused run per user, so one clock per user —
/// keyed by the drawn TEXT, because the callers have no uniform notion of item identity; an
/// identical run on a genuinely different item keeps running rather than resetting, which reads
/// the same either way. Lives in a `thread_local!` at its user.
pub struct Clock {
    key: RefCell<String>,
    /// The [`plx_machine::idle::now_ms`] reading when `key` last changed. Elapsed time is
    /// `now_ms().wrapping_sub(this)` — `draw` gets no `Tick`, and a `wrapping_sub` of two absolute
    /// readings is drift-free without an accumulator.
    start_ms: Cell<u32>,
}

impl Clock {
    pub const fn new() -> Self {
        Clock { key: RefCell::new(String::new()), start_ms: Cell::new(0) }
    }

    /// Advance (or restart, when `text` is a different run) the clock, returning its value in ms.
    /// Call at most once per frame per user — the focused run is drawn exactly once.
    pub fn read(&self, text: &str) -> f64 {
        let now = plx_machine::idle::now_ms();
        let changed = {
            let mut k = self.key.borrow_mut();
            if k.as_str() == text {
                false
            } else {
                k.clear();
                k.push_str(text);
                true
            }
        };
        if changed {
            self.start_ms.set(now);
            0.0
        } else {
            now.wrapping_sub(self.start_ms.get()) as f64
        }
    }

    /// Nothing overflowing is focused: the next overflowing run starts from its rest beat rather
    /// than resuming mid-glide.
    pub fn release(&self) {
        self.key.borrow_mut().clear();
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row's rest beat is longer than a title's, and everything else — speed, gap, the loop —
    /// is the same arithmetic.
    #[test]
    fn a_row_rests_longer_than_a_title_then_glides_at_the_same_speed() {
        let (w, budget) = (500.0, 200.0);
        assert!(ROW.hold_ms > TITLE.hold_ms);
        assert_eq!(ROW.x(TITLE.hold_ms + 500.0, w, budget), 0.0, "a row is still resting");
        assert!(TITLE.x(TITLE.hold_ms + 500.0, w, budget) > 0.0, "a title is already gliding");
        assert_eq!(
            ROW.x(ROW.hold_ms + 500.0, w, budget),
            TITLE.x(TITLE.hold_ms + 500.0, w, budget),
            "same speed once moving"
        );
        assert!(!ROW.moving(ROW.hold_ms - 1.0, w, budget) && ROW.moving(ROW.hold_ms + 1.0, w, budget));
        assert_eq!(ROW.x(ROW.period(w) + 10.0, w, budget), ROW.x(10.0, w, budget), "loops");
    }

    /// A fitting run never moves and never reports.
    #[test]
    fn a_fitting_run_is_inert() {
        for m in [TITLE, ROW] {
            assert_eq!(m.x(1e6, 100.0, 300.0), 0.0);
            assert!(!m.moving(1e6, 100.0, 300.0));
            assert_eq!(m.phase(1e6, 100.0, 300.0), 0.0);
        }
    }

    /// Two clocks are independent: one user restarting never restarts the other.
    #[test]
    fn two_clocks_never_restart_each_other() {
        let (a, b) = (Clock::new(), Clock::new());
        assert_eq!(a.read("Alpha"), 0.0);
        assert_eq!(b.read("Beta"), 0.0);
        let _ = b.read("Gamma");
        assert_eq!(a.key.borrow().as_str(), "Alpha", "b's change never touched a's key");
        a.release();
        assert_eq!(a.read("Alpha"), 0.0, "a released clock starts the run from rest");
    }
}
