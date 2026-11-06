//! **The narrative column's sentence that follows focus** (Settings v2, `Sv2-6-Spec`: "Description
//! follows focus"). A route page whose table rows carry no sub-line states each row's sentence in
//! a FIXED spot of the left column instead - [`FOLLOW_LINES`] lines at the reading size, hung
//! [`theme::space::MD`] under the title - and that sentence cross-fades as focus moves.
//!
//! Two shared pieces, both opt-in for [`crate::table_screen::Header`] so every other caller draws
//! exactly what it drew:
//!
//! * [`FollowCopy`] - the cross-fade. The screen names the sentence the focused row wants
//!   ([`FollowCopy::follow`]: a PURE function of the focused row key and nothing else; a row with
//!   no sentence of its own passes the page's), steps it beside its table ([`FollowCopy::update`])
//!   and lends it to the `Header`. The fade is a [`Spring`] on the shared appear stiffness
//!   ([`crate::popover::K_APPEAR`]), the motion every entry on the page runs on; the springs report
//!   their own motion to the idle gate, so the fade needs no invalidation of its own. It is
//!   decoration: it is never part of a page's canonical state.
//!
//!   **Why it is not [`crate::xfade::Xfade`]** (the shared content cross-fade, `ui/CLAUDE.md` rule
//!   4). `Xfade` is a fade OUT, a data swap at the floor, a fade IN: a millisecond ramp whose hard
//!   deadline gates a state commit, for content that is WIPED under the screen. A followed sentence
//!   commits nothing and wipes nothing: both strings are in hand, so the old one leaves while the
//!   new one arrives, SIMULTANEOUSLY, on a spring (which also lets a key held down retarget the fade
//!   mid-flight without a restart, [`FollowCopy::follow`]). Putting that behind `Xfade` would mean
//!   a dip to nothing between two sentences the board shows overlapping.
//! * [`PictureSlot`] - the display-only picture under the sentence ([`crate::route_screen::
//!   RouteLayout::picture_slot`]), which contributes no focus or hit stop.

use crate::text_view::TextView;
use crate::{theme, Painter, Rect, Spring};
use plx_machine::machine::Measure;

/// The most lines a followed sentence may take (the board: "up to 5 lines"). A page's copy fits
/// them in every shipped language - the screen's `fit_report` test grades it.
pub const FOLLOW_LINES: usize = 5;

/// A sentence that cross-fades to whatever focus asks for.
pub struct FollowCopy {
    shown: String,
    /// The sentence being faded away from; empty once the fade has arrived.
    left: String,
    /// 0 = the fade just began (all `left`), 1 = arrived (all `shown`).
    arrive: Spring,
}

impl FollowCopy {
    /// A page that opens on `text` shows it at once, with nothing fading.
    pub fn new(text: &str) -> Self {
        Self { shown: text.to_owned(), left: String::new(), arrive: Spring::at(1.0) }
    }

    /// Ask for `text`. A sentence already shown (or already being faded to) changes nothing; any
    /// other starts the fade from what is on screen. Returns whether a fade began.
    pub fn follow(&mut self, text: &str) -> bool {
        if self.shown == text {
            return false;
        }
        // Mid-fade, the sentence nearer to arriving stays as the one fading out, so a held key
        // never stacks three sentences; the one nearly gone is dropped.
        if self.arrive.pos >= 0.5 || self.left.is_empty() {
            self.left = std::mem::take(&mut self.shown);
        } else {
            self.shown.clear();
        }
        self.shown.push_str(text);
        self.arrive = Spring::at(0.0);
        true
    }

    /// The sentence being shown (the target of the fade).
    pub fn text(&self) -> &str {
        &self.shown
    }

    /// Step the fade one frame.
    pub fn update(&mut self, dt: f32) {
        if self.arrive.pos < 1.0 || !self.left.is_empty() {
            self.arrive.step(1.0, crate::popover::K_APPEAR, dt);
            if self.arrive.land(1.0, dt) {
                self.left.clear();
            }
        }
    }

    /// How far the shown sentence has come, 0..1.
    pub fn arrival(&self) -> f32 {
        self.arrive.pos.clamp(0.0, 1.0)
    }

    /// Draw the sentence(s) in `frame` at `size`: the leaving one at the opacity the arrival has
    /// taken from it, the shown one at the opacity it has gained. The same view the plain copy is
    /// drawn with (`size`, a leading of `size + XS`, the reading ink), capped at [`FOLLOW_LINES`].
    pub fn draw(&self, p: Painter, frame: Rect, size: std::os::raw::c_int) {
        let a = self.arrival();
        if !self.left.is_empty() && a < 1.0 {
            TextView::new(&self.left, size, theme::TEXT_READING)
                .leading(size as f32 + theme::space::XS)
                .max_lines(FOLLOW_LINES)
                .draw(p.alpha(1.0 - a), frame);
        }
        if !self.shown.is_empty() {
            TextView::new(&self.shown, size, theme::TEXT_READING)
                .leading(size as f32 + theme::space::XS)
                .max_lines(FOLLOW_LINES)
                .draw(p.alpha(if self.left.is_empty() { 1.0 } else { a }), frame);
        }
    }
}

/// Display-only art in a route page's picture slot. It is never a focus or hit stop, so it wears
/// no ring, tick or label (the board: "so it never reads as a control").
pub trait PictureSlot {
    /// Paint into `slot` ([`crate::route_screen::RouteLayout::picture_slot`]).
    fn draw(&self, p: Painter, slot: Rect, measure: &dyn Measure);
}

/// A fan of poster cards, the real tile ([`crate::widgets::card`]) at rest: the first card full
/// size on the slot's left, each next one a step right and a step smaller, drawn back to front so
/// the first overlaps the rest.
///
/// **Art only.** The picture never reads as a control, so no card of the fan wears the poster's
/// state language (the watched tick, the resume bar's fact): [`PosterFan::art`] hands the shared
/// tile the row's facts with that state cleared, and the tile's own art path does the rest.
pub struct PosterFan<'a> {
    pub arts: Vec<crate::widgets::Art<'a>>,
}

impl<'a> PosterFan<'a> {
    /// The art the shared tile is handed for card `i`: a poster's facts with no watch state, so
    /// `widgets::poster_mark` is `None` and the tile draws no tick. Other variants carry no state.
    pub fn art(&self, i: usize) -> crate::widgets::Art<'a> {
        Self::stateless(self.arts[i])
    }

    /// `art` with no watch state: the rule [`PosterFan::art`] applies, for a picture that draws
    /// its tiles itself (the Under posters picture, whose tile of a kind with no poster is not art).
    pub fn stateless(art: crate::widgets::Art<'a>) -> crate::widgets::Art<'a> {
        use crate::widgets::Art;
        match art {
            Art::Poster(Some(f)) => Art::Poster(Some(crate::tile::TileFacts { watched: false, resume: None, ..f })),
            other => other,
        }
    }
}

impl PictureSlot for PosterFan<'_> {
    fn draw(&self, p: Painter, slot: Rect, _measure: &dyn Measure) {
        let n = self.arts.len();
        if n == 0 {
            return;
        }
        // A poster is 2:3. The front card fills the slot's height less the steps the others hang
        // behind it; each card behind is smaller by FAN_SHRINK and offset right by its step.
        let step = (slot.w * FAN_STEP).min(slot.w);
        let front_w = ((slot.w - step * (n as f32 - 1.0)).min(slot.h * 2.0 / 3.0)).max(0.0);
        for i in (0..n).rev() {
            let k = 1.0 - FAN_SHRINK * i as f32;
            let (w, h) = (front_w * k, front_w * k * 1.5);
            let x = slot.x + step * i as f32;
            // cards behind sit on the front card's floor
            let y = slot.y + front_w * 1.5 - h;
            crate::widgets::card(p.alpha(1.0 - FAN_FADE * i as f32), Rect::new(x, y, w, h), self.art(i), theme::CARD_RING_RAD, false, 1.0, 0.0);
        }
    }
}

/// A landscape (16:9) picture filling the slot's width: the HUD gallery's and a HUD package's
/// preview. Still art, or the shared [`crate::widgets::ArtCycle`] when the owner has several
/// frames of one scene. No frame, ring or label: it is a picture, not a control.
///
/// This replaced the header's older landscape slot after the narrative (`Header::preview`, its
/// own frame and height rule), which only the HUD pages used.
pub struct LandscapePicture<'a> {
    pub art: Option<crate::widgets::Art<'a>>,
    pub cycle: Option<&'a crate::widgets::ArtCycle>,
}

impl LandscapePicture<'_> {
    /// The picture's rect in `slot`: its full width, 16:9, as tall as the slot leaves.
    pub fn rect(slot: Rect) -> Rect {
        Rect::new(slot.x, slot.y, slot.w, (slot.w * 9.0 / 16.0).min(slot.h).max(0.0))
    }
}

impl PictureSlot for LandscapePicture<'_> {
    fn draw(&self, p: Painter, slot: Rect, _measure: &dyn Measure) {
        let rect = Self::rect(slot);
        // a slot too short for any picture draws nothing, as the old landscape slot did
        if rect.h <= 0.0 {
            return;
        }
        if let Some(cycle) = self.cycle.filter(|c| !c.is_empty()) {
            cycle.draw(p, rect, theme::CARD_RING_RAD);
        } else if let Some(art) = self.art {
            crate::widgets::card(p, rect, art, theme::CARD_RING_RAD, false, 1.0, 0.0);
        }
    }
}

/// Horizontal step between fanned cards, as a share of the slot's width.
const FAN_STEP: f32 = 0.18;
/// How much smaller each card behind the front one is.
const FAN_SHRINK: f32 = 0.08;
/// How much dimmer each card behind the front one is.
const FAN_FADE: f32 = 0.18;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_opens_on_its_sentence_and_following_it_changes_nothing() {
        let mut c = FollowCopy::new("page");
        assert!(!c.follow("page"));
        assert_eq!(c.arrival(), 1.0);
        assert_eq!(c.text(), "page");
    }

    #[test]
    fn following_a_new_sentence_fades_from_the_old_one_and_arrives() {
        let mut c = FollowCopy::new("page");
        assert!(c.follow("row"));
        assert_eq!(c.text(), "row");
        assert_eq!(c.arrival(), 0.0);
        for _ in 0..600 {
            c.update(1.0 / 60.0);
        }
        assert_eq!(c.arrival(), 1.0, "the shared appear spring arrives");
        assert!(c.left.is_empty(), "nothing left to fade out once arrived");
        assert!(!c.follow("row"), "the sentence already shown is not a change");
    }

    /// One frame of a fade, judged the way the present gate judges it: `frame_begin` forgets the
    /// last frame's motion, `update` steps the springs, and the springs report their own motion.
    fn step_frame(c: &mut FollowCopy) -> bool {
        plx_machine::idle::frame_begin(1.0 / 60.0);
        c.update(1.0 / 60.0);
        plx_machine::idle::page_moving()
    }

    /// A fade that is running reports motion every frame (the gate is spring-instrumented, so it
    /// stays on springs), and a settled one goes quiet (`ui/CLAUDE.md` rule 4).
    #[test]
    fn a_running_fade_reports_animating_each_frame() {
        let _g = plx_base::testlock::serial();
        let mut c = FollowCopy::new("page");
        assert!(c.follow("row"));
        for f in 0..4 {
            assert!(step_frame(&mut c), "frame {f} of a running fade must report motion");
            assert!(c.arrival() < 1.0, "frame {f} is still mid-fade");
        }
    }

    #[test]
    fn a_settled_copy_goes_quiet() {
        let _g = plx_base::testlock::serial();
        let mut c = FollowCopy::new("page");
        c.follow("row");
        for _ in 0..600 {
            step_frame(&mut c);
        }
        for f in 0..8 {
            assert!(!step_frame(&mut c), "frame {f} after arriving must not hold the panel awake");
        }
        let mut fresh = FollowCopy::new("page");
        for f in 0..8 {
            assert!(!step_frame(&mut fresh), "a page that never followed anything is at rest too (frame {f})");
        }
    }

    #[test]
    fn the_fan_is_art_only_whatever_the_rows_watch_state() {
        use crate::tile::{Resume, TileFacts};
        use crate::widgets::{poster_mark, Art, PosterMark};
        let finished = TileFacts { watched: true, unwatched: false, thumb: "/t", ..Default::default() };
        let mid = TileFacts { resume: Some(Resume { frac: 0.3, left_ms: 1 }), thumb: "/t", ..Default::default() };
        assert_eq!(poster_mark(&finished), PosterMark::Watched, "the row alone would wear a tick");
        let fan = PosterFan { arts: vec![Art::Poster(Some(finished)), Art::Poster(Some(mid)), Art::Poster(None)] };
        for i in 0..3 {
            match fan.art(i) {
                Art::Poster(Some(f)) => {
                    assert_eq!(poster_mark(&f), PosterMark::None, "card {i} wears no tick or bar");
                    assert_eq!(f.thumb, "/t", "the art itself is untouched");
                }
                Art::Poster(None) => assert_eq!(i, 2),
                _ => panic!("the fan only holds posters here"),
            }
        }
    }

    #[test]
    fn a_held_key_never_stacks_more_than_two_sentences() {
        let mut c = FollowCopy::new("a");
        c.follow("b");
        c.update(1.0 / 60.0);
        c.follow("c");
        c.update(1.0 / 60.0);
        c.follow("d");
        assert_eq!(c.text(), "d");
        assert!(c.left == "a" || c.left == "b" || c.left == "c");
        assert_eq!(c.arrival(), 0.0);
    }

    /// **The landscape picture is 16:9 at the slot's width**, the board's 660 x 371 at the picture slot, and draws
    /// nothing (rather than a sliver) when the slot is too short for a picture.
    #[test]
    fn a_landscape_picture_is_sixteen_by_nine_at_the_slots_width() {
        use crate::route_screen::RouteLayout;
        let slot = RouteLayout::settings_page().picture_slot();
        let r = LandscapePicture::rect(slot);
        assert_eq!((r.x, r.y, r.w), (slot.x, slot.y, slot.w));
        assert!((r.w - 660.0).abs() < 0.5, "the board's width: {r:?}");
        assert!((r.h - r.w * 9.0 / 16.0).abs() < 0.01, "16:9: {r:?}");
        assert!(r.y + r.h <= slot.y + slot.h, "inside the slot");
        let short = LandscapePicture::rect(Rect::new(0.0, 0.0, 660.0, 100.0));
        assert_eq!(short.h, 100.0, "never taller than the slot leaves");
    }
}
