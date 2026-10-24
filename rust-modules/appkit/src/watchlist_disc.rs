//! **The Watchlist bookmark disc, shared by the heroes that carry it** (Detail's action row and
//! Home's; docs/watchlist-plan.md §1.8). The face a membership wears, the glyph of each face, the
//! flip between them, and the state machine that plays the flip ([`Disc`]) live here so the two
//! discs cannot drift: a screen owns the membership read and the paint call, and asks a [`Disc`]
//! for everything in between.
//!
//! The face tables and the flip geometry are pure functions of their arguments; [`Disc`] is the one
//! stateful type, and the one place the "snap on a first answer, animate on a change" rule is written.

use plx_data::catalog::Membership;
use plx_ui::{Rect, Spring};

/// The two faces the bookmark disc wears. Membership picks one; an UNKNOWN membership (the list has
/// not landed, or plex.tv is unreachable) wears the outline, the Add face — the PUT is idempotent,
/// so a press on it is still allowed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Face {
    Outline,
    Filled,
}

/// The face for `m`.
pub fn face(m: Membership) -> Face {
    if m == Membership::On { Face::Filled } else { Face::Outline }
}

/// The glyph a face draws. The pair share one silhouette (`icons.rs`), so a flip never moves an edge.
pub fn face_icon(f: Face) -> plx_ui::icons::Icon {
    match f {
        Face::Outline => plx_ui::icons::Icon::Bookmark,
        Face::Filled => plx_ui::icons::Icon::BookmarkFill,
    }
}

/// The disc's flip target: 0 for the outline, 1 for the filled face.
pub fn target(m: Membership) -> f32 {
    f32::from(face(m) == Face::Filled)
}

/// How far the ribbon drops at the middle of a flip, px (the board's 3 px). `theme` has no token for
/// a glyph nudge this small — the nearest, `space::XS`, is eight — so the board's value is named here.
pub const RIBBON_DROP: f32 = 3.0;

/// One instant of the flip between the faces, from the face's progress `p` (0 = outline at rest,
/// 1 = filled at rest — the spring the screen steps toward the target face).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FlipFrame {
    /// How much of the filled glyph is revealed, measured up from the glyph's bottom edge (0..1).
    pub fill: f32,
    /// The ribbon's downward offset, px: 0 at both rests, [`RIBBON_DROP`] at the middle.
    pub ribbon_dy: f32,
}

/// The flip's frame at progress `p` (clamped to 0..1). Both rests are exact (`fill` 0 / 1, no
/// offset), so a settled disc is the plain outline or filled icon with nothing left of the animation.
pub fn flip_frame(p: f32) -> FlipFrame {
    let p = p.clamp(0.0, 1.0);
    FlipFrame { fill: p, ribbon_dy: RIBBON_DROP * 4.0 * p * (1.0 - p) }
}

/// The glyph a disc at flip progress `p` draws, and the layered fill when `p` is mid-flip: at either
/// rest the plain face (`None`), only a flip in flight layers the filled glyph over the outline.
pub fn paint(p: f32) -> (plx_ui::icons::Icon, Option<FlipFrame>) {
    let icon = face_icon(if p >= 1.0 { Face::Filled } else { Face::Outline });
    let mid = (p > 0.0 && p < 1.0).then(|| flip_frame(p));
    (icon, mid)
}

/// The part of the glyph's box `frame` reveals the filled face in: bottom-anchored, `fill` of the
/// height. The outline draws under it over the whole box.
pub fn fill_clip(glyph: Rect, frame: FlipFrame) -> Rect {
    let h = glyph.h * frame.fill;
    Rect::new(glyph.x, glyph.y + glyph.h - h, glyph.w, h)
}

/// The glyph's box moved down by the ribbon drop.
pub fn dropped(glyph: Rect, frame: FlipFrame) -> Rect {
    Rect::new(glyph.x, glyph.y + frame.ribbon_dy, glyph.w, glyph.h)
}

/// The speed under which [`Disc::step`] stops calling the flip animating: the threshold Detail's
/// page-moving sum has always judged each of its springs by.
const MOVING_VEL: f32 = 0.01;

/// **The disc's whole flip state:** the membership it wears, the spring that plays the flip toward
/// that membership's face, and whether the user's own press is waiting for its answer. A screen
/// holds one, feeds it the application's membership answers, steps it each frame and paints from
/// [`Self::pos`]; nothing about when to snap and when to animate is left to the screen.
///
/// The flip is stepped at [`plx_ui::widgets::K_DISC_UNFURL`], the hero discs' unfurl stiffness.
#[derive(Clone, Copy, Default)]
pub struct Disc {
    /// The membership the disc is flipping toward; [`target`] is its face.
    m: Membership,
    /// 0 = the outline at rest, 1 = filled.
    flip: Spring,
    /// A press on the disc whose membership change has not arrived yet.
    pressed: bool,
}

impl Disc {
    /// A disc at rest on the face of `m`, no press pending.
    pub fn new(m: Membership) -> Self {
        Self { m, flip: Spring::at(target(m)), pressed: false }
    }

    /// The membership the disc wears.
    pub fn membership(&self) -> Membership {
        self.m
    }

    /// The user pressed the disc: the membership change that answers it is theirs to see. Call it
    /// where the press is turned into the application's Add or Remove, so only a press the
    /// application will answer is recorded.
    pub fn press(&mut self) {
        self.pressed = true;
    }

    /// The application's answer for the title the disc belongs to, and whether it changed the
    /// membership (the caller owes the idle machinery a frame when it did).
    ///
    /// **The rule, written once.** The first answer (the list landing, or the page opening on an
    /// already-listed title) is the disc's REST face, not a flip: it snaps. A change from a known
    /// membership animates. A change that answers the user's own [`Self::press`] is real even out of
    /// `Unknown` (the list unread, or plex.tv unreachable): the Add pressed there is `Unknown` to
    /// `On` too, and snapping it would play no flip while the Remove back out of it drains. A press
    /// is consumed by the change that answers it; an unchanged answer leaves it pending.
    pub fn set(&mut self, m: Membership) -> bool {
        if m == self.m {
            return false;
        }
        if self.m == Membership::Unknown && !self.pressed {
            self.flip = Spring::at(target(m));
        }
        self.pressed = false;
        self.m = m;
        true
    }

    /// The disc now belongs to another title (the hero rotated): rest on the face of `m` with no
    /// press pending, whatever the face was before.
    pub fn reset(&mut self, m: Membership) {
        *self = Self::new(m);
    }

    /// Step the flip toward the membership's face, one frame of `dt` seconds. Returns whether it is
    /// still moving, the flag a screen folds into its own "is anything animating" test.
    ///
    /// Decorative (`idle::decorative`): the flip moves no layout, so it never holds a page image.
    pub fn step(&mut self, dt: f32) -> bool {
        let to = target(self.m);
        plx_machine::idle::decorative(|| self.flip.step(to, plx_ui::widgets::K_DISC_UNFURL, dt));
        // A spring idle has stopped asking frames for can rest a hair short of its target; land it
        // exactly so a settled disc draws the plain face, never a 99.9 % fill clip.
        if plx_machine::idle::settled(self.flip.pos, to, self.flip.vel) {
            self.flip.jump(to);
        }
        self.animating()
    }

    /// Whether the flip is still moving.
    pub fn animating(&self) -> bool {
        self.flip.vel.abs() > MOVING_VEL
    }

    /// The flip's progress: 0 = the outline at rest, 1 = the filled face; what [`paint`] draws from.
    pub fn pos(&self) -> f32 {
        self.flip.pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Outline unless the title is on the list; unknown wears the outline (the Add face).
    #[test]
    fn the_face_table() {
        assert_eq!(face(Membership::Off), Face::Outline);
        assert_eq!(face(Membership::Unknown), Face::Outline);
        assert_eq!(face(Membership::On), Face::Filled);
        assert_eq!(face_icon(Face::Outline), plx_ui::icons::Icon::Bookmark);
        assert_eq!(face_icon(Face::Filled), plx_ui::icons::Icon::BookmarkFill);
        assert_eq!((target(Membership::On), target(Membership::Off), target(Membership::Unknown)), (1.0, 0.0, 0.0));
    }

    /// The flip fills from the bottom, drops the ribbon 3 px at most, and rests exactly at both ends.
    #[test]
    fn the_flip_endpoints_and_the_ribbon_drop() {
        assert_eq!(RIBBON_DROP, 3.0);
        assert_eq!(flip_frame(0.0), FlipFrame { fill: 0.0, ribbon_dy: 0.0 });
        assert_eq!(flip_frame(1.0), FlipFrame { fill: 1.0, ribbon_dy: 0.0 });
        assert_eq!(flip_frame(-4.0), flip_frame(0.0));
        assert_eq!(flip_frame(9.0), flip_frame(1.0));
        let mid = flip_frame(0.5);
        assert_eq!((mid.fill, mid.ribbon_dy), (0.5, RIBBON_DROP));
        let mut last = -1.0;
        for i in 0..=100 {
            let f = flip_frame(i as f32 / 100.0);
            assert!(f.fill >= last, "fill only rises");
            assert!((0.0..=RIBBON_DROP).contains(&f.ribbon_dy));
            last = f.fill;
        }
    }

    #[test]
    fn the_fill_rises_from_the_bottom_edge() {
        let g = Rect::new(10.0, 20.0, 18.0, 24.0);
        let bottom = g.y + g.h;
        for p in [0.0, 0.25, 0.5, 1.0] {
            let c = fill_clip(g, flip_frame(p));
            assert_eq!(c.y + c.h, bottom, "p={p}: anchored to the bottom");
            assert_eq!((c.x, c.w), (g.x, g.w));
            assert_eq!(c.h, g.h * p);
        }
        let d = dropped(g, flip_frame(0.5));
        assert_eq!((d.x, d.y, d.w, d.h), (g.x, g.y + RIBBON_DROP, g.w, g.h));
        assert_eq!(dropped(g, flip_frame(1.0)), g);
    }

    /// A settled disc is the plain face; only the middle of a flip layers the fill.
    #[test]
    fn a_settled_disc_paints_the_plain_face() {
        assert_eq!(paint(0.0), (plx_ui::icons::Icon::Bookmark, None));
        assert_eq!(paint(1.0), (plx_ui::icons::Icon::BookmarkFill, None));
        let (icon, mid) = paint(0.4);
        assert_eq!(icon, plx_ui::icons::Icon::Bookmark);
        assert_eq!(mid, Some(flip_frame(0.4)));
    }

    const DT: f32 = 1.0 / 60.0;

    /// Step `d` for `n` frames from a fresh idle frame each time, collecting the progress it drew.
    fn frames(d: &mut Disc, n: usize) -> Vec<f32> {
        (0..n)
            .map(|_| {
                plx_machine::idle::frame_begin(DT);
                d.step(DT);
                d.pos()
            })
            .collect()
    }

    fn in_flight(trace: &[f32]) -> usize {
        trace.iter().filter(|p| **p > 0.0 && **p < 1.0).count()
    }

    /// The first answer is the rest face: an `Unknown` to `On` with no press (the list landing under
    /// an already-listed title) snaps, and so does the `Unknown` to `Off` (nothing to flip to).
    #[test]
    fn an_unpressed_first_answer_snaps() {
        let _serial = plx_base::testlock::serial();
        let mut d = Disc::new(Membership::Unknown);
        assert_eq!((d.pos(), d.membership()), (0.0, Membership::Unknown));
        assert!(d.set(Membership::On), "the answer changed the membership");
        assert_eq!(d.pos(), 1.0, "snapped onto the filled face before any frame");
        assert_eq!(frames(&mut d, 8), vec![1.0; 8], "and nothing moves afterwards");
        assert!(!d.animating());
        let mut off = Disc::new(Membership::Unknown);
        off.set(Membership::Off);
        assert_eq!(off.pos(), 0.0);
        // An unchanged answer is not a change.
        assert!(!d.set(Membership::On));
    }

    /// An Add pressed from `Unknown` (the list unread) answers `On` at once: that change is the
    /// user's own, so the flip UNFURLS over several frames instead of snapping.
    #[test]
    fn a_pressed_add_from_unknown_unfurls() {
        let _serial = plx_base::testlock::serial();
        let mut d = Disc::new(Membership::Unknown);
        d.press();
        assert!(d.set(Membership::On));
        assert_eq!(d.pos(), 0.0, "the flip starts at the outline");
        let trace = frames(&mut d, 8);
        assert!(in_flight(&trace) >= 5, "the add unfurls over frames: {trace:?}");
        assert!(trace.windows(2).all(|w| w[1] >= w[0]), "and rises: {trace:?}");
        // The press was consumed by that answer: the next Unknown to On, unpressed, is a first answer.
        d.set(Membership::Unknown);
        let _ = frames(&mut d, 240);
        d.set(Membership::On);
        assert_eq!(d.pos(), 1.0, "the unpressed second answer snaps");
    }

    /// A Remove drains: `On` to `Unknown` (the app answers the Remove with the list unread) and `On`
    /// to `Off` both fall over several frames from the filled face, and settle EXACTLY on the outline.
    #[test]
    fn a_remove_drains() {
        let _serial = plx_base::testlock::serial();
        for after in [Membership::Unknown, Membership::Off] {
            let mut d = Disc::new(Membership::On);
            assert_eq!(d.pos(), 1.0);
            d.press();
            assert!(d.set(after));
            assert_eq!(d.pos(), 1.0, "the drain starts at the filled face");
            let trace = frames(&mut d, 8);
            assert!(in_flight(&trace) >= 5, "{after:?}: the remove drains over frames: {trace:?}");
            assert!(trace.windows(2).all(|w| w[1] <= w[0]), "and falls: {trace:?}");
            let _ = frames(&mut d, 240);
            assert_eq!((d.flip.pos, d.flip.vel), (0.0, 0.0), "{after:?}: rests exactly on the outline");
            assert!(!d.animating());
        }
    }

    /// A settled flip lands EXACTLY on its face (not 0.999 drawing a fill clip), and a mid-flip disc
    /// reports that it is animating until then.
    #[test]
    fn a_flip_settles_exactly_and_reports_animating() {
        let _serial = plx_base::testlock::serial();
        let mut d = Disc::new(Membership::Off);
        d.set(Membership::On);
        plx_machine::idle::frame_begin(DT);
        assert!(d.step(DT), "mid-flip");
        for _ in 0..600 {
            plx_machine::idle::frame_begin(DT);
            d.step(DT);
        }
        assert_eq!((d.flip.pos, d.flip.vel), (1.0, 0.0));
        plx_machine::idle::frame_begin(DT);
        assert!(!d.step(DT), "at rest");
    }

    /// The hero rotating to another title rests on that title's face with no flip, and forgets a
    /// press made on the title that left.
    #[test]
    fn a_reset_rests_on_the_new_face_and_forgets_the_press() {
        let _serial = plx_base::testlock::serial();
        let mut d = Disc::new(Membership::Off);
        d.press();
        d.reset(Membership::On);
        assert_eq!((d.pos(), d.membership()), (1.0, Membership::On));
        assert_eq!(frames(&mut d, 4), vec![1.0; 4], "no flip plays");
        // Had the press survived the reset, this first answer would unfurl instead of snapping.
        d.reset(Membership::Unknown);
        d.set(Membership::On);
        assert_eq!(d.pos(), 1.0, "the press did not outlive the title it was made on");
    }
}
