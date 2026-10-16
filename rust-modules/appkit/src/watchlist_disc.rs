//! **The Watchlist bookmark disc, shared by the heroes that carry it** (Detail's action row and
//! Home's; docs/watchlist-plan.md §1.8). The face a membership wears, the glyph of each face, and the
//! flip between them are values: a screen owns the membership read, the spring that steps the flip
//! and the paint call, and shares everything else from here so the two discs cannot drift.
//!
//! Pure functions of their arguments, no state.

use plx_data::catalog::Membership;
use plx_ui::Rect;

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
}
