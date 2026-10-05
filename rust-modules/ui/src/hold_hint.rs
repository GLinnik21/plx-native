//! `HoldHint` — the standing **"Hold [OK] for options"** capsule (`Home Screen.dc.html`, the design
//! project's `ds-additions/` HoldHint): the one place the app TEACHES that a held OK on a card opens
//! its context menu. The hold has no other visible sign, and the menu behind it is where Mark as
//! Watched, Go to Show and Play from Start live.
//!
//! # Adopting it on a card screen
//!
//! 1. Own ONE [`HoldHint`] on the screen instance (paint-only state: leave it out of the canon
//!    census, like a spinner phase).
//! 2. From the screen's `Tick`, call [`HoldHint::step`] with a [`HintInput::new`], the tick's `ms` and
//!    its `dt`: `focus` is the
//!    stable element id of the CARD focus rests on (`None` for anything that is not a card, and for
//!    a covered page — a menu or modal is open); `settled` is "nothing under that card is still
//!    moving" (scroll, snap, focus glide); `held_ms` is `cx.press.held_ms`, passed straight
//!    through. The screen never divides by the hold time or names `press::LONG_MS`: the
//!    constructor does that.
//! 3. Call [`HoldHint::draw`] LAST in the page draw, over everything the page paints. It is
//!    non-interactive: record no focus stop and no hit rect for it.
//! 4. Nothing else. The item menu's `Mount` already calls [`mark_learned`], so opening a menu from
//!    any card screen retires the hint everywhere; [`step`](HoldHint::step) reads that itself.
//!
//! Host-testing an adopter needs no screen: see this module's `a_screenless_owner_…` test, which
//! drives the widget through exactly those calls.
//!
//! # What it is
//!
//! A dark standing capsule, bottom-centre ([`BOTTOM`] above the frame, [`HEIGHT`] tall), in the tab
//! track's own material, carrying three runs: the word "Hold", a [`key_cap_with`] saying OK, and
//! "for options" — one catalog sentence with the cap as its `{key}` placeholder, so a translation
//! may move it ([`crate::widgets::key_hint_parts`]). It is never pressable.
//!
//! **The cap is also the hold's progress bar.** While OK is held the cap FILLS left to right over the
//! press's own hold time (`press::LONG_MS`, applied by [`HintInput::new`], the only place it is
//! named) and drains back over [`RELEASE_S`] when the key is released early. So the hint that says
//! "hold" is also what shows the hold working.
//!
//! **When it shows is a schedule, and the schedule lives here**, advanced by the owner's `Tick`
//! `dt` — no wall clock, so replay and host tests are deterministic: focus must have RESTED on one
//! card for [`DWELL_MS`] (any move of `focus`, or any frame that is not `settled`, re-arms the dwell
//! from zero and hides it), and the hint then stands for [`LIFE_MS`] and leaves on its own, once per
//! resting place. A held OK shows it regardless of the dwell — that is the fill's moment.
//! `focus == None` shows nothing, and once the viewer has opened a context menu it RETIRES for good
//! ([`learned`]; the design's "until first use" mode — "always" and "off" were design-time toggles
//! and have no setting here).
//!
//! **Motion.** It fades in over ~.36 s and rises [`RISE_PX`] on a slower spring, both critically
//! damped [`Spring`]s, so the present gate ([`plx_machine::idle`]) hears them move and hears them
//! stop: a resting hint (shown or hidden) asks for no frame. The dwell and life counters are TIMERS,
//! not animators — like `Home`'s hero countdown they report nothing while they count (the loop
//! delivers `Tick` to the page whether or not a frame presents) and the fade's first step is what
//! wakes the gate. The fill is the one thing that is neither: it advances every frame it is held or
//! draining, and says so through the `note` callback [`HoldHint::step`] takes.
//!
//! **Material: live glass, with the flat capsule as its fallback.** The capsule is a second
//! standing glass surface beside the top tab track and the profile chip — the same material
//! (`GlassRim::Standing`, `Material::UltraThin`, the track's rim and lit top edge) — and it enters
//! the live-backdrop walk the only legal way: one `Glass::DYNAMIC_BACKDROP.backdrop` call from the
//! normal painter draw. It orchestrates nothing; the frame's layer walk (`ui::frame::backdrop`) owns
//! the source, the occlusion and the damage. Because Home's page layer is not a SHARED band (the
//! chrome layer is), the walk gives it its OWN source entry rather than merging it into the top
//! band's: a surface at the bottom of the screen never drags the top band's single grab out toward
//! full screen (the "glass at the top and another at the bottom is a full-screen blur" law of
//! `docs/glass-hardware-budget.md` §3.1 belongs to the old one-cache design). Its region is the
//! capsule grown `BLUR_MARGIN` and clamped to the panel — about 880 x 204 = 180k px² at the widest
//! shipped language, inside the 300k a MOVING host holds 60 fps under
//! (`the_capsules_own_blur_region_fits_the_budget_and_never_meets_the_top_band` holds it, and
//! holds the two regions apart). When the walk refuses glass this frame (blur latched off, a
//! frozen host, no render target, a source pass) `backdrop` answers `false` and the capsule draws
//! the tab track's flat fallback (`TAB_TRACK_TOP`/`BOT` through `rect_rimmed`), the same fallback the
//! chip takes. The glass fades and rises WITH the capsule — its scrim and rim are faded by the
//! opacity (a `GlassFace` is not cascaded by the painter's alpha, so [`widgets::chip_face`] scales it,
//! as the chip's unfurl does) — and a hidden hint draws nothing, so it declares no surface and costs
//! no capture or composite at rest. The weight is the FIXED [`theme::HINT_GLASS_TOP`]/`BOT`: the
//! track solves its density per frame from a framebuffer readback, and a standing hint does not
//! take one.
//!
//! **Where "learned" lives.** A frame-thread `thread_local`, not the persisted session record (whose
//! format is not this widget's to change): after a relaunch the hint teaches once more until the
//! next menu opens.

use std::ffi::CString;

use plx_machine::machine::Measure;
use plx_machine::present::PresentEvent;

use crate::widgets::{self, CapFace, CapLook, CapMetrics};
use crate::{theme, Painter, Rect, Spring};

/// Bottom edge of the capsule above the bottom of the frame (`--hint-bottom`). The design's 52 is
/// two pixels inside the 54 px safe keep-out ([`crate::consts::MARGIN_Y`]); the capsule is a
/// non-interactive standing note, not content, and the design's number is kept.
pub const BOTTOM: f32 = 52.0;
/// The capsule's height (`--hint-h`).
pub const HEIGHT: f32 = 64.0;
/// Horizontal padding inside the capsule, either end.
const PAD_X: f32 = 30.0;
/// Space between the three runs (the design's `gap: 16`).
const RUN_GAP: f32 = 16.0;
/// How far below its resting place the capsule starts, and returns to when it hides unseen.
pub const RISE_PX: f32 = 14.0;
/// Focus must rest this long on one shelf tile before the hint appears (`--hint-dwell`).
pub const DWELL_MS: u32 = 1500;
/// How long it stands once shown before leaving on its own (`--hint-life`).
pub const LIFE_MS: u32 = 6000;
/// Opacity spring: critically damped, settles to the idle gate's rest band in about .36 s
/// (the design's `opacity .36s ease-out`).
const K_FADE: f32 = 300.0;
/// Rise spring: the slower of the two, about .5 s (`transform .5s` spring).
const K_RISE: f32 = 150.0;
/// An early release drains the cap's fill back to empty in this long (the design's ~.2 s).
pub const RELEASE_S: f32 = 0.2;
/// Below this opacity nothing is drawn and the capsule counts as hidden.
const VISIBLE: f32 = 0.004;

// ---- "has the viewer learned it" -----------------------------------------------------------------
//
// The hint retires for good once a context menu has been opened — from ANY card screen, since the
// lesson is the gesture. It is held for the life of the process on the FRAME THREAD (a
// `thread_local`, the same shape as `idle`'s and `press`'s published snapshots): every writer (the
// item-menu surface mounting) and every reader (Home's tick) runs there, and a host test gets a
// fresh answer per test thread with no lock to forget. It is deliberately NOT persisted: the
// persisted preference store is the session record, whose format and migration are not this
// widget's to change, so after a relaunch the hint teaches once more until the next menu opens.
thread_local! {
    static LEARNED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The viewer has opened a context menu, so the hint has done its job and never shows again.
pub fn learned() -> bool {
    LEARNED.with(|l| l.get())
}

/// Record that a context menu was opened. Idempotent.
pub fn mark_learned() {
    LEARNED.with(|l| l.set(true));
}

/// Forget it — a test's start state, and nothing else.
#[cfg(any(test, feature = "test-support"))]
pub fn reset_learned_for_test() {
    LEARNED.with(|l| l.set(false));
}

/// What the owner tells the hint each frame. Build it with [`HintInput::new`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HintInput {
    /// The card focus is resting on, by the owner's own stable element id; `None` whenever the hint
    /// has no business on screen (focus is not on a card, or a menu or modal covers the page).
    pub focus: Option<u32>,
    /// Nothing under that card is still moving — no scroll, snap or focus glide in flight. The
    /// dwell only runs while this holds.
    pub settled: bool,
    /// How far through the hold a held OK is, 0..=1: `held_ms / press::LONG_MS`, clamped. `None`
    /// when OK is not held on a holdable card press.
    pub hold: Option<f32>,
}

impl HintInput {
    /// `held_ms` is the press machine's own clock (`cx.press.held_ms`), passed through unchanged;
    /// the division by the hold threshold happens here so no screen names `press::LONG_MS`.
    pub fn new(focus: Option<u32>, settled: bool, held_ms: Option<u32>) -> Self {
        let hold = held_ms.map(|ms| (ms as f32 / crate::press::LONG_MS as f32).clamp(0.0, 1.0));
        Self { focus, settled, hold }
    }
}

/// The hint's state: the schedule, the two springs and the cap's fill. Hold one per page.
#[derive(Clone, Copy)]
pub struct HoldHint {
    key: Option<u32>,
    /// The tick clock (`Tick.ms`) at which continuous rest on [`key`](Self::key) began; `None`
    /// while not resting. An absolute reading rather than an accumulated per-frame delta, so a slow
    /// or skipped tick cannot drift the schedule (the same idiom as `motion::Phase`).
    rest_at: Option<u32>,
    /// This resting place has had its [`LIFE_MS`]; it does not come back until focus moves.
    spent: bool,
    fade: Spring,
    rise: Spring,
    /// The cap's fill fraction as drawn: follows the hold exactly while held, drains while not.
    fill: f32,
}

impl Default for HoldHint {
    fn default() -> Self {
        Self::new()
    }
}

impl HoldHint {
    pub const fn new() -> Self {
        Self { key: None, rest_at: None, spent: false, fade: Spring::at(0.0), rise: Spring::at(0.0), fill: 0.0 }
    }

    /// Advance one frame — the owner's whole per-`Tick` job. `note` receives [`PresentEvent::Motion`]
    /// on every frame the fill is moving — the fill is steered by a clock the springs cannot see, and an idle gate that never
    /// heard it would freeze the progress mid-hold.
    pub fn step(&mut self, input: HintInput, now_ms: u32, dt: f32, note: &mut dyn FnMut(PresentEvent)) {
        let focus = input.focus.filter(|_| !learned());
        if focus != self.key {
            self.key = focus;
            self.rest_at = None;
            self.spent = false;
        }
        let mut rested_ms = 0;
        if focus.is_none() || !input.settled {
            self.rest_at = None;
        } else if !self.spent {
            let at = *self.rest_at.get_or_insert(now_ms);
            rested_ms = now_ms.wrapping_sub(at);
            if rested_ms >= DWELL_MS + LIFE_MS {
                self.spent = true;
            }
        }
        // a hold on no card (or under a retired hint) is no hold
        let hold = input.hold.filter(|_| focus.is_some());
        let standing = focus.is_some() && !self.spent && self.rest_at.is_some() && rested_ms >= DWELL_MS;
        let show = hold.is_some() || standing;

        self.fade.step(if show { 1.0 } else { 0.0 }, K_FADE, dt);
        if show {
            self.rise.step(1.0, K_RISE, dt);
        } else if self.fade.pos <= VISIBLE {
            // leaving is a plain fade; the capsule goes back under its resting place unseen
            self.rise.jump(0.0);
        }

        match hold {
            Some(frac) => {
                self.fill = frac.clamp(0.0, 1.0);
                note(PresentEvent::Motion);
            }
            None if self.fill > 0.0 => {
                self.fill = (self.fill - dt / RELEASE_S).max(0.0);
                note(PresentEvent::Motion);
            }
            None => {}
        }
    }

    /// Whether any of it is on screen.
    pub fn visible(&self) -> bool {
        self.fade.pos > VISIBLE
    }

    /// The cap's current fill fraction.
    pub fn fill(&self) -> f32 {
        self.fill
    }

    /// The opacity the capsule is drawn at.
    pub fn opacity(&self) -> f32 {
        self.fade.pos.clamp(0.0, 1.0)
    }

    /// Draw it, centred at the bottom of the screen. Draws nothing while hidden. Non-interactive:
    /// it records no focus stop and no hit rect.
    pub fn draw(&self, p: Painter, measure: &dyn Measure) {
        if !self.visible() {
            return;
        }
        let lay = Layout::resolve(measure, &sentence());
        let lift = (1.0 - self.rise.pos.clamp(0.0, 1.0)) * RISE_PX;
        let r = lay.rect(lift);
        let opacity = self.opacity();
        // One call into the live-backdrop walk; `false` is its refusal and the flat track material
        // is the answer. The face is faded by hand: only the tint rides the painter's cascade.
        let glass = widgets::Glass::DYNAMIC_BACKDROP.backdrop(
            p,
            r,
            lift,
            r.h * 0.5,
            [1.0, 1.0, 1.0, opacity],
            plx_gfx::gfx::GlassRim::Standing,
            widgets::chip_face(glass_face(), opacity),
            theme::Material::UltraThin,
        );
        let p = p.alpha(opacity);
        if !glass {
            let boost = theme::GLASS_RIM_LIGHT[3] - theme::GLASS_RIM[3];
            p.rect_rimmed(
                r,
                r.h * 0.5,
                theme::TAB_TRACK_TOP,
                theme::TAB_TRACK_BOT,
                theme::GLASS_RIM,
                boost,
            );
        }
        let cy = r.cy();
        let sz = theme::size::CAPTION;
        let ty = plx_gfx::text::text_vcenter_y(sz, 0, cy);
        let mut x = r.x + PAD_X;
        if !lay.pre.is_empty() {
            p.text(lay.pre.as_ptr(), x, ty, sz, theme::TEXT_SECONDARY, 0, 0);
            x += lay.pre_w + RUN_GAP;
        }
        widgets::key_cap_with(
            p,
            x,
            cy,
            CapFace::Label(KEY),
            CapLook {
                ring: theme::TEXT_SECONDARY,
                label: theme::TEXT_PRIMARY,
                fill: Some((self.fill, theme::OVERLAY_FOCUS_PILL)),
                metrics: CapMetrics::HINT,
            },
            measure,
        );
        x += lay.cap_w;
        if !lay.post.is_empty() {
            x += lay.post_gap;
            p.text(lay.post.as_ptr(), x, ty, sz, theme::TEXT_SECONDARY, 0, 0);
        }
    }

    /// The capsule's width under `measure` in the current language — for a fit test and for a
    /// caller that must keep clear of it.
    pub fn width(measure: &dyn Measure) -> f32 {
        Layout::resolve(measure, &sentence()).width
    }
}

/// What the capsule wears over its backdrop: the track's rim and lit top edge, at the hint's own
/// fixed scrim weight (`theme::HINT_GLASS_TOP`/`BOT`).
fn glass_face() -> plx_gfx::gfx::GlassFace {
    plx_gfx::gfx::GlassFace {
        scrim_top: theme::HINT_GLASS_TOP,
        scrim_bot: theme::HINT_GLASS_BOT,
        rim: theme::GLASS_RIM,
        rim_lit: theme::GLASS_RIM_LIGHT,
        rim_w: theme::CARD_SHEEN_W,
    }
}

/// The key the cap names.
const KEY: &std::ffi::CStr = c"OK";

/// The catalog sentence, `{key}` held by the object-replacement character.
fn sentence() -> String {
    plx_platform::i18n::msg::widgets_hint_hold_options("\u{fffc}")
}

/// One measured arrangement of the three runs — the single place width is summed, so the capsule
/// the draw paints is the capsule the caller measured.
struct Layout {
    pre: CString,
    post: CString,
    pre_w: f32,
    cap_w: f32,
    /// Space between the cap and the post run: the design's run gap, except none before closing
    /// punctuation, which belongs to the cap's word ("{key}, каб адкрыць меню").
    post_gap: f32,
    width: f32,
}

impl Layout {
    fn resolve(measure: &dyn Measure, message: &str) -> Self {
        let (pre, post) = widgets::key_hint_parts(message);
        // The design spaces the runs by its own 16 px gap; the catalog's own spaces would double it.
        let trim = |s: CString| CString::new(s.to_string_lossy().trim()).unwrap_or_default();
        let (pre, post) = (trim(pre), trim(post));
        let sz = theme::size::CAPTION;
        let pre_w = if pre.is_empty() { 0.0 } else { measure.width(&pre, sz, false) };
        let post_w = if post.is_empty() { 0.0 } else { measure.width(&post, sz, false) };
        let cap_w = widgets::key_cap_w_with(CapFace::Label(KEY), CapMetrics::HINT, measure);
        let post_gap = match post.to_bytes().first() {
            None | Some(b',' | b'.' | b';' | b':' | b'!' | b'?') => 0.0,
            Some(_) => RUN_GAP,
        };
        let pre_gap = if pre.is_empty() { 0.0 } else { RUN_GAP };
        Self { pre, post, pre_w, cap_w, post_gap, width: 2.0 * PAD_X + pre_w + pre_gap + cap_w + post_gap + post_w }
    }

    /// The capsule's rect, `lift` px below its resting place.
    fn rect(&self, lift: f32) -> Rect {
        Rect::new(
            (crate::consts::SCR_W - self.width) * 0.5,
            crate::consts::SCR_H - BOTTOM - HEIGHT + lift,
            self.width,
            HEIGHT,
        )
    }
}

#[cfg(test)]
mod tests {
    //! The schedule is pure arithmetic over `dt`; the motion half touches `plx_machine::idle`'s
    //! per-thread flags, so every test that steps a springs holds `testlock::serial()` like
    //! `xfade`'s. What these cannot say is how the capsule READS: that is a simulator capture, and
    //! the device fps scenes (`fps_floor` for the fade, `fps_ceiling` for the rest) are not run here.
    use super::*;
    use crate::fixture::FixtureMeasure;

    const DT: f32 = 1.0 / 60.0;

    thread_local! {
        /// The tests' tick clock: frames stepped on this thread so far (one test, one thread).
        static FRAME: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    /// `HoldHint::step` at the next 60 Hz frame's `Tick.ms`.
    fn tstep(h: &mut HoldHint, input: HintInput, note: &mut dyn FnMut(PresentEvent)) {
        let n = FRAME.with(|f| {
            f.set(f.get() + 1);
            f.get()
        });
        h.step(input, (n as u64 * 1000 / 60) as u32, DT, note);
    }

    fn rested(focus: u32) -> HintInput {
        HintInput { focus: Some(focus), settled: true, hold: None }
    }

    /// Step `n` frames of `input` through the present gate, counting the `Motion` notes. Going
    /// through the gate every frame matters: its settle frame and sticky damage flag are consumed
    /// by the frames that raise them, as they are in the loop.
    fn run(h: &mut HoldHint, input: HintInput, n: usize) -> usize {
        let mut notes = 0;
        for _ in 0..n {
            plx_machine::idle::note_present(10_000);
            plx_machine::idle::frame_begin(DT);
            tstep(h, input, &mut |ev| {
                notes += 1;
                plx_machine::idle::note(ev)
            });
            plx_machine::idle::should_present(10_016);
        }
        notes
    }

    fn secs(s: f32) -> usize {
        (s * 60.0).round() as usize
    }

    fn fresh() -> (plx_base::testlock::Serial, HoldHint) {
        let g = plx_base::testlock::serial();
        reset_learned_for_test();
        // drain whatever the previous test left on the gate's process-wide damage flag
        frame_asks(&mut HoldHint::new(), HintInput::default());
        (g, HoldHint::new())
    }

    /// One presented-or-not frame of `input`: did the gate ask to present it? The springs report
    /// into the flag `frame_begin` clears, so the tick has to happen BETWEEN the two calls, with a
    /// present just behind us so the 2 s keepalive cannot answer in the hint's place.
    fn frame_asks(h: &mut HoldHint, input: HintInput) -> bool {
        plx_machine::idle::note_present(10_000);
        plx_machine::idle::frame_begin(DT);
        tstep(h, input, &mut |ev| plx_machine::idle::note(ev));
        plx_machine::idle::should_present(10_016)
    }

    #[test]
    fn it_waits_out_the_dwell_then_appears() {
        let (_g, mut h) = fresh();
        run(&mut h, rested(7), secs(1.4));
        assert!(!h.visible(), "not before the dwell");
        run(&mut h, rested(7), secs(0.2) + secs(0.6));
        assert!(h.visible(), "after the dwell the fade is under way");
        assert!(h.opacity() > 0.9, "and settled well within a second");
    }

    #[test]
    fn it_leaves_on_its_own_after_its_life_and_stays_gone() {
        let (_g, mut h) = fresh();
        run(&mut h, rested(7), secs(1.5 + 3.0));
        assert!(h.visible());
        run(&mut h, rested(7), secs(3.0 + 1.5));
        assert!(!h.visible(), "6 s after it appeared it is gone");
        run(&mut h, rested(7), secs(20.0));
        assert!(!h.visible(), "and it does not come back on the same tile");
    }

    #[test]
    fn a_focus_move_hides_it_and_rearms_the_dwell_from_zero() {
        let (_g, mut h) = fresh();
        run(&mut h, rested(7), secs(2.5));
        assert!(h.visible());
        run(&mut h, rested(8), secs(1.0));
        assert!(h.opacity() < 0.2, "the move hides it");
        run(&mut h, rested(8), secs(0.4));
        assert!(h.opacity() < 0.2, "1.4 s after the move it is still waiting out the dwell");
        run(&mut h, rested(8), secs(1.0));
        assert!(h.opacity() > 0.9, "after the full dwell it is back");
        // and a spent hint is revived by a move, not by time
        run(&mut h, rested(8), secs(9.0));
        assert!(!h.visible());
        run(&mut h, rested(9), secs(2.5));
        assert!(h.visible(), "a new tile starts a new life");
    }

    #[test]
    fn scrolling_holds_the_dwell_at_zero() {
        let (_g, mut h) = fresh();
        let moving = HintInput { settled: false, ..rested(7) };
        run(&mut h, moving, secs(5.0));
        assert!(!h.visible(), "never while scrolling");
        run(&mut h, rested(7), secs(1.4));
        assert!(!h.visible(), "the rest begins when the scroll ends");
    }

    #[test]
    fn no_focus_on_a_shelf_tile_shows_nothing() {
        let (_g, mut h) = fresh();
        run(&mut h, HintInput { focus: None, settled: true, hold: None }, secs(10.0));
        assert!(!h.visible(), "the tab strip, a menu or a covered page has no shelf focus");
        run(&mut h, rested(7), secs(2.5));
        assert!(h.visible());
        run(&mut h, HintInput { focus: None, settled: true, hold: None }, secs(1.0));
        assert!(!h.visible(), "a menu opening over it takes it away");
    }

    #[test]
    fn a_held_ok_shows_it_before_the_dwell_and_fills_the_cap() {
        let (_g, mut h) = fresh();
        let held = |f| HintInput { hold: Some(f), ..rested(7) };
        let mut last = 0.0;
        for i in 0..20 {
            tstep(&mut h, held(i as f32 / 30.0), &mut |_| {});
            assert!(h.fill() >= last, "the fill only grows while held");
            last = h.fill();
        }
        assert!(h.visible(), "shown by the hold alone, well inside the dwell");
        assert!((h.fill() - 19.0 / 30.0).abs() < 1e-6, "the fill IS the hold fraction");
        tstep(&mut h, held(2.0), &mut |_| {});
        assert_eq!(h.fill(), 1.0, "clamped");
    }

    #[test]
    fn an_early_release_drains_the_fill_in_about_a_fifth_of_a_second() {
        let (_g, mut h) = fresh();
        tstep(&mut h, HintInput { hold: Some(0.8), ..rested(7) }, &mut |_| {});
        assert!(h.fill() > 0.7);
        run(&mut h, rested(7), secs(0.1));
        assert!(h.fill() > 0.0 && h.fill() < 0.8, "draining");
        run(&mut h, rested(7), secs(0.15));
        assert_eq!(h.fill(), 0.0, "empty again");
    }

    #[test]
    fn a_hold_is_ignored_once_the_hint_has_retired() {
        let (_g, mut h) = fresh();
        mark_learned();
        tstep(&mut h, HintInput { hold: Some(0.5), ..rested(7) }, &mut |_| {});
        assert!(!h.visible());
        assert_eq!(h.fill(), 0.0);
        run(&mut h, rested(7), secs(5.0));
        assert!(!h.visible(), "retired for good");
    }

    #[test]
    fn opening_a_menu_retires_a_standing_hint() {
        let (_g, mut h) = fresh();
        run(&mut h, rested(7), secs(2.5));
        assert!(h.visible());
        mark_learned();
        run(&mut h, rested(7), secs(1.0));
        assert!(!h.visible());
        reset_learned_for_test();
    }

    /// Host half of the animator's two tests (`ui/CLAUDE.md`): it reports while it runs...
    #[test]
    fn a_fading_hint_asks_for_every_frame_of_itself() {
        let (_g, mut h) = fresh();
        run(&mut h, rested(7), secs(1.5) + 1);
        for f in 0..6 {
            assert!(frame_asks(&mut h, rested(7)), "frame {f} of the fade-in must repaint");
        }
        // ...and so does a hold filling the cap, though no spring is moving then
        let mut h = HoldHint::new();
        for f in 0..4 {
            assert!(frame_asks(&mut h, HintInput { hold: Some(f as f32 / 30.0), ..rested(7) }));
        }
    }

    /// ...and goes quiet at rest, shown or hidden — the failure that costs the whole saving.
    #[test]
    fn a_hint_at_rest_asks_for_nothing() {
        let (_g, mut h) = fresh();
        run(&mut h, rested(7), secs(1.4));
        assert!(!frame_asks(&mut h, rested(7)), "hidden and counting the dwell: nothing to repaint");
        run(&mut h, rested(7), secs(2.0));
        assert!(!frame_asks(&mut h, rested(7)), "shown and settled: nothing to repaint");
        run(&mut h, rested(7), secs(5.0));
        run(&mut h, rested(7), secs(2.0));
        assert!(!frame_asks(&mut h, rested(7)), "expired and hidden: nothing to repaint");
        let mut h = HoldHint::new();
        run(&mut h, HintInput::default(), 5);
        assert!(!frame_asks(&mut h, HintInput::default()), "never armed: nothing to repaint");
    }

    /// **Reusability without Home.** A fake owner with no screen, no press machine and no engine
    /// does exactly what the module doc's recipe says an adopter does: own one `HoldHint`, build a
    /// [`HintInput::new`] from three plain facts each tick, `step`, and read `visible()` where a
    /// draw would. It never names `press::LONG_MS`; the hold arrives as raw milliseconds.
    #[test]
    fn a_screenless_owner_drives_dwell_show_hold_fill_and_retire() {
        struct FakeOwner {
            hint: HoldHint,
            focus: Option<u32>,
            settled: bool,
            held_ms: Option<u32>,
        }
        impl FakeOwner {
            fn frames(&mut self, n: usize) {
                for _ in 0..n {
                    let input = HintInput::new(self.focus, self.settled, self.held_ms);
                    tstep(&mut self.hint, input, &mut |ev| plx_machine::idle::note(ev));
                }
            }
        }
        let _g = plx_base::testlock::serial();
        reset_learned_for_test();
        let mut o = FakeOwner { hint: HoldHint::new(), focus: Some(42), settled: true, held_ms: None };

        // dwell, then show
        o.frames(secs(1.4));
        assert!(!o.hint.visible(), "inside the dwell");
        o.frames(secs(1.5));
        assert!(o.hint.visible(), "after it");

        // a hold: raw press milliseconds in, a clamped fraction of the hold time out
        o.held_ms = Some(crate::press::LONG_MS / 4);
        o.frames(2);
        assert!((o.hint.fill() - 0.25).abs() < 1e-6);
        o.held_ms = Some(crate::press::LONG_MS * 3);
        o.frames(1);
        assert_eq!(o.hint.fill(), 1.0, "clamped by the constructor");
        o.held_ms = None;
        o.frames(secs(0.3));
        assert_eq!(o.hint.fill(), 0.0, "released: drained");

        // not a card (or a covered page): gone
        o.focus = None;
        o.frames(secs(1.0));
        assert!(!o.hint.visible());

        // retire: opening any menu marks the lesson learned, and nothing brings it back
        o.focus = Some(42);
        o.frames(secs(3.0));
        assert!(o.hint.visible(), "control: it is showing before the menu opens");
        mark_learned();
        o.frames(secs(1.0));
        assert!(!o.hint.visible());
        o.held_ms = Some(100);
        o.frames(secs(10.0));
        assert!(!o.hint.visible() && o.hint.fill() == 0.0, "retired for good, even under a hold");
        reset_learned_for_test();
    }

    #[test]
    fn the_input_constructor_does_the_hold_division() {
        let long = crate::press::LONG_MS;
        assert_eq!(HintInput::new(Some(1), true, None).hold, None);
        assert_eq!(HintInput::new(Some(1), true, Some(0)).hold, Some(0.0));
        assert_eq!(HintInput::new(Some(1), true, Some(long / 2)).hold, Some(0.5));
        assert_eq!(HintInput::new(Some(1), true, Some(long * 9)).hold, Some(1.0));
    }

    #[test]
    fn the_capsule_stands_bottom_centre_and_the_hold_fill_is_the_only_per_frame_note() {
        let _g = plx_base::testlock::serial();
        let lay = Layout::resolve(&FixtureMeasure, "Hold \u{fffc} for options");
        let r = lay.rect(0.0);
        assert_eq!(r.y + r.h, crate::consts::SCR_H - BOTTOM);
        assert_eq!(r.h, HEIGHT);
        assert!(((r.x + r.w * 0.5) - crate::consts::SCR_W * 0.5).abs() < 1e-3, "centred");
        assert_eq!(lay.pre.to_str().unwrap(), "Hold", "catalog spaces trimmed: the design's gap is the space");
        assert_eq!(lay.post.to_str().unwrap(), "for options");
        let sum = 2.0 * PAD_X + lay.pre_w + lay.cap_w + 2.0 * RUN_GAP
            + FixtureMeasure.width(c"for options", theme::size::CAPTION, false);
        assert!((lay.width - sum).abs() < 1e-3);
    }

    /// **Punctuation that opens the post-cap run sits tight on the cap.** The Belarusian sentence
    /// is "Утрымлівайце {key}, каб адкрыць меню": the comma belongs to the cap's word, so the 16 px
    /// run gap goes BETWEEN words and never before a comma; a post run that starts with a letter, or
    /// a dash that needs its space, keeps the gap.
    #[test]
    fn a_post_run_that_starts_with_punctuation_attaches_to_the_cap() {
        let m = &FixtureMeasure;
        let w = |s: &std::ffi::CStr| m.width(s, theme::size::CAPTION, false);
        let tight = Layout::resolve(m, "Hold \u{fffc}, to open the menu");
        assert_eq!(tight.post.to_str().unwrap(), ", to open the menu");
        assert_eq!(tight.post_gap, 0.0);
        let sum = 2.0 * PAD_X + tight.pre_w + tight.cap_w + RUN_GAP + w(c", to open the menu");
        assert!((tight.width - sum).abs() < 1e-3, "one gap (before the cap), none before the comma");
        let words = Layout::resolve(m, "Hold \u{fffc} for options");
        assert_eq!(words.post_gap, RUN_GAP);
        let dash = Layout::resolve(m, "\u{fffc} \u{2014} back to the library");
        assert_eq!(dash.post_gap, RUN_GAP, "a dash is a word-space mark, not closing punctuation");
    }

    /// **The glass is bounded by the same budget as the rest of the band's glass, and kept apart
    /// from it.** The capsule's own blurred region (itself grown `BLUR_MARGIN`, clamped to the
    /// panel — `gfx::blur_region`, the expression the planner's capture uses) must fit what a
    /// MOVING host holds 60 fps under, at the widest shipped language and at the bottom of its rise
    /// (the rest rect and the drawn rect both lie inside it). And it must not meet the top band's
    /// region: Home's page layer is not a shared band, so they are separate sources, and the
    /// separation is what keeps a bottom surface from dragging the band's one grab toward full screen.
    #[test]
    fn the_capsules_own_blur_region_fits_the_budget_and_never_meets_the_top_band() {
        use plx_base::fontcov::advances::ShippedMeasure;
        let band = {
            let h = crate::widgets::TAB_PILL_H + 2.0 * crate::widgets::TAB_TRACK_PAD;
            let y = crate::widgets::TOP_BAR_Y - crate::widgets::TAB_TRACK_PAD;
            plx_gfx::gfx::blur_region(0.0, y, crate::consts::SCR_W, h)
        };
        let mut widest = 0.0f32;
        for language in plx_platform::i18n::SHIPPED {
            let _guard = plx_platform::i18n::language_on_this_thread_for_test(language);
            let lay = Layout::resolve(&ShippedMeasure, &sentence());
            for lift in [0.0, RISE_PX] {
                let r = lay.rect(lift);
                let reg = plx_gfx::gfx::blur_region(r.x, r.y, r.w, r.h);
                let area = reg[2] * reg[3];
                widest = widest.max(area);
                assert!(
                    area <= plx_gfx::gfx::GLASS_REGION_BUDGET,
                    "{}: the capsule's region is {area:.0} px^2, past the {:.0} a moving host carries",
                    language.tag(),
                    plx_gfx::gfx::GLASS_REGION_BUDGET,
                );
                assert!(
                    reg[1] > band[1] + band[3],
                    "{}: the capsule's region (top {:.0}) meets the top band's (bottom {:.0}): one grab would span the screen",
                    language.tag(),
                    reg[1],
                    band[1] + band[3],
                );
            }
        }
        // the budget the pair spends if both ever refresh in the same frame: stated, not graded
        // against 60 fps (that is a television measurement)
        assert!(widest + band[2] * band[3] > plx_gfx::gfx::GLASS_REGION_BUDGET, "the pair is two grabs, not one: do not price them as a union");
    }

    /// The fixed glass weight is the track's ceiling family, never heavier than the flat capsule it
    /// falls back to, and its stops run lighter-to-heavier like the track's own.
    #[test]
    fn the_glass_scrim_never_runs_heavier_than_the_flat_fallback() {
        let f = glass_face();
        assert!(f.scrim_top[3] < f.scrim_bot[3]);
        assert!(f.scrim_top[3] <= theme::TAB_TRACK_TOP[3] && f.scrim_bot[3] <= theme::TAB_TRACK_BOT[3]);
        assert!(f.scrim_top[3] > theme::TAB_GLASS_TOP[3], "heavier than the track's floor: it cannot solve per frame");
        assert_eq!((f.rim, f.rim_lit), (theme::GLASS_RIM, theme::GLASS_RIM_LIGHT), "the track's own edge");
        let half = widgets::chip_face(f, 0.5);
        assert_eq!(half.scrim_top[3], f.scrim_top[3] * 0.5);
        assert_eq!(half.rim_lit[3], f.rim_lit[3] * 0.5, "the rim fades with the capsule: no bright hairline left behind");
    }

    /// **A hidden hint declares no glass; a visible one declares exactly one, in the page layer.**
    /// Nothing is sampled, captured or composited at rest while it is invisible.
    #[test]
    fn a_hidden_hint_declares_no_surface_and_a_visible_one_declares_one() {
        use crate::frame::backdrop::{self, Sources, Z};
        use std::{cell::RefCell, rc::Rc};
        let (_g, mut h) = fresh();
        // A fresh source table per frame: entries outlive the frame that declared them until the
        // planner prunes them, so "declared nothing" is read off an empty table.
        let declared = |hint: &HoldHint| {
            let sources = Rc::new(RefCell::new(Sources::default()));
            sources.borrow_mut().begin(vec![]);
            let _walk = backdrop::discover(sources.clone());
            let _page = backdrop::layer(Z::page(1), false);
            hint.draw(Painter::root(), &FixtureMeasure);
            let n = sources.borrow().entries.len();
            n
        };
        assert_eq!(declared(&h), 0, "hidden: no surface");
        run(&mut h, rested(7), secs(2.5));
        assert!(h.visible());
        assert_eq!(declared(&h), 1, "visible: one surface");
        run(&mut h, HintInput::default(), secs(1.5));
        assert!(!h.visible());
        assert_eq!(declared(&h), 0, "hidden again: none");
    }

    /// **The capsule is a fit rule like any other app-owned text.** It hugs its content, so "fits"
    /// means: one catalog placeholder in each shipped language, both runs present or deliberately
    /// absent, and the whole capsule inside the safe frame at the shipped size with the device's
    /// whole-pixel advances. It never elides — there is nothing to ellipsize into.
    #[test]
    fn the_capsule_fits_the_safe_frame_in_every_shipped_language() {
        use plx_base::fontcov::advances::ShippedMeasure;
        let mut bad = Vec::new();
        for language in plx_platform::i18n::SHIPPED {
            let _guard = plx_platform::i18n::language_on_this_thread_for_test(language);
            let message = sentence();
            let tag = language.tag();
            if message.matches('\u{fffc}').count() != 1 {
                bad.push(format!("{tag}: needs exactly one key placeholder: {message:?}"));
            }
            let lay = Layout::resolve(&ShippedMeasure, &message);
            if lay.pre.is_empty() && lay.post.is_empty() {
                bad.push(format!("{tag}: the sentence has no words around the cap: {message:?}"));
            }
            if lay.width > crate::consts::SAFE.w {
                bad.push(format!("{tag}: {:.0}px wider than the {:.0}px safe frame", lay.width, crate::consts::SAFE.w));
            }
            assert!(HoldHint::width(&ShippedMeasure) == lay.width, "{tag}: width() and the draw share one layout");
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}
