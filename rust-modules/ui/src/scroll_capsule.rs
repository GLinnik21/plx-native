//! **The scroll capsule and the top fade** — what a long page leaves on screen when its header and
//! the app's top chrome have tucked away: a small capsule that keeps your place ("Films 1,284"),
//! and a fade of the page's own ground colour at the top edge so rows leave under it instead of
//! being cut off.
//!
//! **The fade is eased, and reaches through the capsule.** Its alpha falls along a smoothstep, not a
//! straight line: a linear ramp breaks slope where it ends, and over saturated poster art that
//! break reads as a bright band at y 72. It is [`FADE_H`] deep on its own; while the capsule shows
//! it reaches [`FADE_BELOW_CAPSULE`] past the capsule's own measured bottom edge, so the capsule's
//! lower half does not sit on full-strength art (the capsule's rect is the one source of that
//! extent, there is no second literal). The deeper profile arrives with the capsule's alpha, so the
//! strips stand still and only their alpha moves. The UI has no gradient primitive with an eased
//! ramp; [`Painter::rect`] is the two-stop gradient, so the ease is laid the way the hero scrim and
//! the skeleton do it: abutting [`FADE_STEP`] px strips whose stops are the curve at their edges
//! ([`fade_strips`]); the chord error is at most about half a percent of alpha.
//!
//! **Live liquid glass, the material the top navigation pill wears.** The capsule is a
//! `Glass::DYNAMIC_BACKDROP` surface: the same standing container (`GlassRim::Standing`,
//! `Material::UltraThin`, the track's own rim and lit top edge as its `GlassFace`) the tab track and
//! the hold hint wear, so it refracts the rows scrolling under it. The nav's own solved face
//! (`GlassPlan::tab_face`) is not reachable from a page's frame and goes stale while the bar is
//! tucked, which is exactly when this shows, so it takes the hold hint's fixed face
//! ([`hold_hint::glass_face`]). Where the renderer refuses the backdrop it is the flat track
//! material. (This replaces the frozen-ground form of 2026-10-17: the owner asked for the nav's
//! glass here explicitly.)
//!
//! **The whole name fits.** The capsule is as wide as the name and the count need, never shortened
//! with an ellipsis.
//!
//! **It can carry the sort** ("Films 1,284 · Date viewed", the Library's board): an optional
//! criterion piece after the count and a dot. The library and the criterion are in the primary ink,
//! the count and the dot at 50% white ([`theme::COUNT_FILTERED_INK`], the header read-out's quiet
//! ink: the board's (165,150,144) over the warm ground, where `TEXT_SECONDARY` read cool and bright);
//! all four are BODY regular, [`PIECE_GAP`] (12) apart
//! inside the same [`PAD_L`] / [`PAD_R`], so the capsule's width follows its content. A page that
//! passes no criterion draws the plain capsule exactly as before (a bold name and a CAPTION count,
//! [`NAME_COUNT_GAP`] apart).
//!
//! Pure geometry and colour live here and are tested without a frame; the choice of WHEN to show
//! them (`scroll_chrome`) is owned by the Library (and its Collections page) and the Watchlist.
use std::ffi::CStr;

use plx_machine::machine::Measure;

use crate::hstack::{Align, HStack};
use crate::{theme, widgets, Painter, Rect};

/// Left edge, top edge and height of the capsule: the board's x 96, y 54, 64 tall. The width is the
/// text's ([`width`]): the whole name and the count, never cut.
pub const X: f32 = 96.0;
pub const Y: f32 = 54.0;
pub const H: f32 = 64.0;
/// A full capsule: the corner radius is half the height.
pub const RADIUS: f32 = H * 0.5;
/// Room the text keeps from the capsule's left end, and from its right end.
pub const PAD_L: f32 = 28.0;
pub const PAD_R: f32 = 28.0;
/// Space between the name and the count of a plain capsule.
pub const NAME_COUNT_GAP: f32 = 10.0;
/// Space between the pieces of a capsule that carries the sort: the board's 12.
pub const PIECE_GAP: f32 = 12.0;
/// Height of the fade at the page's top edge, painted in the ambient ground colour, never black.
pub const FADE_H: f32 = 72.0;
/// How far the fade reaches below the capsule's bottom edge while the capsule shows: the capsule's
/// bottom (118) plus this is the fade's depth, about 130.
pub const FADE_BELOW_CAPSULE: f32 = 12.0;
/// Height of one strip of the fade: the knots the smoothstep is sampled at. 72 is twelve of them.
pub const FADE_STEP: f32 = 6.0;

/// The dot between the count and the criterion.
const DOT: &CStr = c"\u{b7}";

/// The most pieces a capsule has: the name, the count, the dot and the criterion.
const PIECES: usize = 4;

/// One text run of the capsule: what it says, its type and its ink.
#[derive(Clone, Copy)]
struct Run<'a> {
    text: &'a CStr,
    size: i32,
    bold: bool,
    ink: [f32; 4],
}

/// The capsule's runs left to right (the first `len` of `list`; a fixed array, because a capsule is
/// built every frame it is up) and the gap between them. Without a criterion: the name (BODY bold)
/// and the count (CAPTION regular, secondary). With one, the board's four pieces: the name, the
/// count, the dot and the criterion, all BODY regular, the count and the dot at 50% white.
#[derive(Clone, Copy)]
struct Runs<'a> {
    list: [Run<'a>; PIECES],
    len: usize,
    gap: f32,
}

impl<'a> Runs<'a> {
    fn of(name: &'a CStr, count: &'a CStr, criterion: Option<&'a CStr>) -> Self {
        let run = |text, ink| Run { text, size: theme::size::BODY, bold: false, ink };
        match criterion {
            None => {
                let name = Run { text: name, size: theme::size::BODY, bold: true, ink: theme::TEXT_PRIMARY };
                let count = Run { text: count, size: theme::size::CAPTION, bold: false, ink: theme::TEXT_SECONDARY };
                Self { list: [name, count, count, count], len: 2, gap: NAME_COUNT_GAP }
            }
            Some(criterion) => Self {
                list: [
                    run(name, theme::TEXT_PRIMARY),
                    run(count, theme::COUNT_FILTERED_INK),
                    run(DOT, theme::COUNT_FILTERED_INK),
                    run(criterion, theme::TEXT_PRIMARY),
                ],
                len: PIECES,
                gap: PIECE_GAP,
            },
        }
    }

    fn as_slice(&self) -> &[Run<'a>] { &self.list[..self.len] }
}

/// **A capsule, measured once**: its runs, their advances and its rectangle. The page asks for the
/// rectangle (the top fade reaches through it) and then draws, so the runs are measured here and
/// nowhere else; nothing is heap-allocated. Built where it is read, every frame the capsule is up.
pub struct Capsule<'a> {
    runs: Runs<'a>,
    /// The advances of `runs` (the first `runs.len`): the capsule's one text run, which an
    /// [`HStack`] spaces `runs.gap` apart.
    widths: [f32; PIECES],
    rect: Rect,
}

impl<'a> Capsule<'a> {
    /// The capsule for `name` and `count`, and the `criterion` when it carries the sort: left
    /// padding, the runs and the gaps between them, right padding.
    pub fn new(name: &'a CStr, count: &'a CStr, criterion: Option<&'a CStr>, measure: &dyn Measure) -> Self {
        let runs = Runs::of(name, count, criterion);
        let mut widths = [0.0; PIECES];
        for (width, run) in widths.iter_mut().zip(runs.as_slice()) {
            *width = measure.width(run.text, run.size, run.bold);
        }
        let text = HStack::widths(&widths[..runs.len], runs.gap, Align::Leading(0.0)).width();
        Self { runs, widths, rect: Rect::new(X, Y, PAD_L + text + PAD_R, H) }
    }

    /// The capsule's rectangle: [`X`], [`Y`], the width its runs need, [`H`].
    pub fn rect(&self) -> Rect { self.rect }

    /// The capsule and its text at `alpha` (the tuck: 0 draws nothing), in the nav pill's live
    /// glass, exactly as wide as its runs and its paddings.
    pub fn draw(&self, p: Painter, alpha: f32) {
        if alpha <= 0.0 {
            return;
        }
        let p = p.alpha(alpha);
        let r = self.rect;
        let glass = widgets::Glass::DYNAMIC_BACKDROP.backdrop(
            p,
            r,
            0.0,
            RADIUS,
            [1.0, 1.0, 1.0, alpha],
            plx_gfx::gfx::GlassRim::Standing,
            widgets::chip_face(crate::hold_hint::glass_face(), alpha),
            theme::Material::UltraThin,
        );
        if !glass {
            let boost = theme::GLASS_RIM_LIGHT[3] - theme::GLASS_RIM[3];
            p.rect_rimmed(r, RADIUS, theme::TAB_TRACK_TOP, theme::TAB_TRACK_BOT, theme::GLASS_RIM, boost);
        }
        let cy = r.y + r.h * 0.5;
        let stack = HStack::widths(&self.widths[..self.runs.len], self.runs.gap, Align::Leading(r.x + PAD_L));
        for (i, run) in self.runs.as_slice().iter().enumerate() {
            let x = stack.span(i).map_or(r.x + PAD_L, |s| s.0);
            let y = plx_gfx::text::text_vcenter_y(run.size, 1, cy);
            p.text(run.text.as_ptr(), x, y, run.size, run.ink, 0, i32::from(run.bold));
        }
    }
}

/// The capsule's width for `name` and `count`, and the `criterion` when it carries the sort ([`Capsule::new`]).
pub fn width(name: &CStr, count: &CStr, criterion: Option<&CStr>, measure: &dyn Measure) -> f32 {
    rect(name, count, criterion, measure).w
}

/// The ground's alpha `y` px down the page's top edge: `1 - smoothstep(y / depth)`, opaque at the
/// very edge and clear at `depth`, with no slope at either end. `wide` (0..=1, the capsule's alpha)
/// moves the depth from [`FADE_H`] to `reach` by crossfading the two curves, so the strips stay put.
pub fn fade_alpha(y: f32, reach: f32, wide: f32) -> f32 {
    let edge = |depth: f32| {
        let t = (y / depth).clamp(0.0, 1.0);
        1.0 - t * t * (3.0 - 2.0 * t)
    };
    let near = edge(FADE_H);
    near + (edge(reach.max(FADE_H)) - near) * wide.clamp(0.0, 1.0)
}

/// The fade's depth while a capsule at `capsule` shows: [`FADE_BELOW_CAPSULE`] below its bottom edge,
/// never less than [`FADE_H`].
pub fn fade_reach(capsule: Rect) -> f32 {
    (capsule.y + capsule.h + FADE_BELOW_CAPSULE).max(FADE_H)
}

/// The fade as abutting strips, top to bottom: `(y, h, top stop, bottom stop)`, every stop the
/// ground colour at [`fade_alpha`] of the strip's edge. `capsule` is the capsule's measured rect and
/// its alpha while it shows (`None`: [`FADE_H`] deep). Strips that are clear at both edges are not
/// produced.
pub fn fade_strips(ground: [f32; 3], capsule: Option<(Rect, f32)>) -> impl Iterator<Item = (f32, f32, [f32; 4], [f32; 4])> {
    let (reach, wide) = capsule.map_or((FADE_H, 0.0), |(r, a)| (fade_reach(r), a));
    let depth = if wide > 0.0 { reach } else { FADE_H };
    let n = (depth / FADE_STEP).ceil() as usize;
    let colour = move |y: f32| [ground[0], ground[1], ground[2], fade_alpha(y, reach, wide)];
    (0..n).filter_map(move |k| {
        let (y0, y1) = (k as f32 * FADE_STEP, ((k + 1) as f32 * FADE_STEP).min(depth));
        let (top, bottom) = (colour(y0), colour(y1));
        (top[3] > 0.0 || bottom[3] > 0.0).then_some((y0, y1 - y0, top, bottom))
    })
}

/// The top-edge fade over a `width` wide page, at `alpha` (the tuck: 0 draws nothing). `capsule` is
/// the capsule's measured rect and its alpha while it is drawn, `None` for a page that wears none.
pub fn draw_top_fade(p: Painter, width: f32, ground: [f32; 3], alpha: f32, capsule: Option<(Rect, f32)>) {
    if alpha <= 0.0 {
        return;
    }
    let p = p.alpha(alpha);
    for (y, h, top, bottom) in fade_strips(ground, capsule) {
        p.rect(Rect::new(0.0, y, width, h), 0.0, top, bottom, 0.0);
    }
}

/// The capsule's rectangle for `name`, `count` and the `criterion` it carries, if any. A page that
/// also draws the capsule builds one [`Capsule`] and asks it for both.
pub fn rect(name: &CStr, count: &CStr, criterion: Option<&CStr>, measure: &dyn Measure) -> Rect {
    Capsule::new(name, count, criterion, measure).rect()
}

#[cfg(test)]
#[path = "scroll_capsule_tests.rs"]
mod tests;
