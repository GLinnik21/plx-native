//! **A landscape still's one-line footer**: status text on the left, a mark on the right, on the
//! 24 px line 12 px above the card's bottom edge (`Continue watching and episode footer` handoff).
//!
//! It replaces the two-line `label` / `sub` pair where a card has no show name to carry (the
//! Continue Watching card, whose title is on its art, and the detail page's episode strip, which is
//! inside one season of one show). The pair stays as [`crate::widgets::still_line`] for the shelves
//! that need the show's name; [`crate::widgets::still_overlay`] chooses between them.
//!
//! * **In progress**: `S2 E4 · 38m left` (a film: `38m left`) on the left, the amber progress bar
//!   (100 px, shrinking to a 64 px minimum, 4 px high, radius 2) on the right.
//! * **Watched** (the strip): the length on the left, a muted 22 px tick on the right.
//! * **Not started**: the length alone.
//! * **The focus cue**, Continue Watching only: a white play glyph when OK plays, an info glyph when
//!   OK opens details, at the LEFT of the line on the focused card. Its 28 px slot is collapsed at
//!   rest and opens with the card's own focus pop (`Tile::pop`, the pop spring's factor, never a
//!   timer of its own): the text moves right by the slot as the pop rises.
//!
//! **The fit rule is one pure function**, [`footer_geometry`], of the measured widths and that focus
//! progress. Room is `inner - cue - label - 12`, the label being the tag (when shown) and the time
//! text; the bar takes `min(100, room)`, so the 12 px gap between the text group and the bar holds
//! at every progress value. The tag is kept while the bar still gets 64 px; when it would not, the
//! TAG goes (the time unit never does): at rest if the rest layout is already short of 64, else
//! during the same focus transition (it slides out under the cue slot's edge, fading, as the text
//! group closes up) and back in when focus leaves.
//!
//! The geometry is judged on the card's RESTING inner width (`388` = 420 - 2 x 16): the drawn rect
//! is larger while the card is popped, which only ever adds air, and a rule that read the drawn width
//! would flip the tag's decision mid-transition.
//!
//! There is no right-to-left mirroring here: no RTL language ships, and the shared layout layer has
//! no mirroring mechanism to reuse (`hstack` places left to right; only a text run knows its own
//! paragraph direction), so the geometry is expressed in leading-edge offsets and a mirror would be
//! one transform at the draw, but none is built.

#[allow(unused_imports)]
use crate::{theme, Painter, Rect};
use plx_machine::machine::Measure;

/// The line's bottom stands this far above the card's bottom edge.
pub const FOOTER_RISE: f32 = 12.0;
/// The line's height: the cap band is centred in it, and the bar and the marks on its centre.
pub const FOOTER_LINE_H: f32 = 24.0;
/// The air between the text group and the mark at its right.
pub const FOOTER_GAP: f32 = 12.0;
/// The focus cue's slot: a 20 px glyph and 8 px of air (`widgets::STILL_GLYPH_D` + `STILL_LINE_GAP`).
pub const FOOTER_CUE_SLOT: f32 = 28.0;
/// The progress bar at rest, and the least it shrinks to before the tag gives way.
pub const FOOTER_BAR_W: f32 = 100.0;
pub const FOOTER_BAR_MIN_W: f32 = 64.0;
pub const FOOTER_BAR_H: f32 = 4.0;
pub const FOOTER_BAR_RAD: f32 = 2.0;
/// The watched tick's box.
pub const FOOTER_TICK_D: f32 = 22.0;
/// The resting inner width the fit rule is judged on: the episode tile's 420 less both insets.
pub const FOOTER_INNER_W: f32 = crate::card_row::RowStyle::EPISODE.w - 2.0 * crate::widgets::STILL_LINE_INSET;

/// What OK does on a Continue Watching card, as the cue that promises it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue {
    /// OK plays (`DeckPress::Play`): a white play glyph.
    Play,
    /// OK opens the page (`DeckPress::Details`): an info glyph.
    Info,
}

/// What stands at the line's right end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Right {
    None,
    /// The amber progress bar, the played fraction 0..1.
    Bar(f32),
    /// The muted tick of a finished episode.
    Tick,
}

/// One footer's content. `tag` is the episode tag with its trailing dot (`"S2 E4 ·"`, empty for a
/// film or on the strip), `status` the time text, and `focus` the card's pop factor 0..1.
#[derive(Clone, Copy, Debug)]
pub struct Footer<'a> {
    pub tag: &'a str,
    pub status: &'a str,
    pub right: Right,
    pub cue: Option<Cue>,
    pub focus: f32,
}

/// The measured widths [`footer_geometry`] decides from. `tag_w` includes the space after the tag
/// (0 when there is no tag).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FooterFit {
    pub tag_w: f32,
    pub status_w: f32,
    /// The cue slot the line may open: [`FOOTER_CUE_SLOT`], or 0 on a card with no cue.
    pub cue_w: f32,
}

/// The draw geometry of one footer, in offsets from the line's leading inset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FooterGeo {
    /// How far the visible text group stands right of the inset: the cue slot's opened width.
    pub text_dx: f32,
    /// The tag's alpha. 1 when it is kept, 0 when it is gone.
    pub tag_alpha: f32,
    /// Where the tag run is drawn relative to the text group's left edge: 0 while it is whole, then
    /// sliding left (under a clip at that edge) as it fades, so the time text stays glued to what
    /// remains and no gap opens.
    pub tag_dx: f32,
    /// The progress bar's width: 100 at most, 64 at least while the tag decision holds.
    pub bar_w: f32,
}

/// **The fit rule**, pure: where the text goes, whether the tag shows and how wide the bar is, for
/// an inner width `inner_w`, the measured `fit` and the focus progress `focus` (0 rest, 1 open).
pub fn footer_geometry(_inner_w: f32, _fit: FooterFit, _focus: f32) -> FooterGeo {
    FooterGeo { text_dx: 0.0, tag_alpha: 1.0, tag_dx: 0.0, bar_w: FOOTER_BAR_W }
}

/// Draw `footer` on the line at the bottom of `card` (the DRAWN rect, so the line rides the focus
/// pop). `label` is the optional show name that stands above the line (a card whose title is not on
/// its art); see [`crate::widgets::still_overlay`].
pub fn draw_footer(_p: Painter, _card: Rect, _label: &str, _f: &Footer<'_>, _measure: &dyn Measure) {}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 0.001;

    fn fit(tag_w: f32, status_w: f32, cue: bool) -> FooterFit {
        FooterFit { tag_w, status_w, cue_w: if cue { FOOTER_CUE_SLOT } else { 0.0 } }
    }

    /// The text group's end and the bar's start, from the geometry.
    fn gap(inner: f32, f: FooterFit, g: FooterGeo) -> f32 {
        let end = g.text_dx + f.tag_w * g.tag_alpha + f.status_w;
        (inner - g.bar_w) - end
    }

    #[test]
    fn a_roomy_line_keeps_the_tag_and_the_full_bar_at_every_progress() {
        let f = fit(70.0, 110.0, true);
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            let g = footer_geometry(FOOTER_INNER_W, f, t);
            assert_eq!(g.tag_alpha, 1.0, "t={t}");
            assert_eq!(g.bar_w, FOOTER_BAR_W, "t={t}");
            assert!((g.text_dx - FOOTER_CUE_SLOT * t).abs() < EPS, "the text moves by exactly the cue slot: t={t}");
        }
    }

    #[test]
    fn at_rest_the_bar_is_min_of_100_and_the_room() {
        // room = 388 - 250 - 12 = 126 -> 100; room = 388 - 290 - 12 = 86 -> 86
        for (label, want) in [(250.0, 100.0), (290.0, 86.0)] {
            let g = footer_geometry(FOOTER_INNER_W, fit(60.0, label - 60.0, true), 0.0);
            assert!((g.bar_w - want).abs() < EPS, "{label}: {}", g.bar_w);
            assert_eq!(g.tag_alpha, 1.0);
        }
    }

    #[test]
    fn the_bar_shrinks_toward_the_focus_room_as_the_cue_opens_and_never_below_64() {
        // rest room = 388 - 280 - 12 = 96; focus room = 96 - 28 = 68 >= 64: the tag stays
        let f = fit(70.0, 210.0, true);
        let (rest, open) = (footer_geometry(FOOTER_INNER_W, f, 0.0), footer_geometry(FOOTER_INNER_W, f, 1.0));
        assert!((rest.bar_w - 96.0).abs() < EPS && (open.bar_w - 68.0).abs() < EPS, "{rest:?} {open:?}");
        assert_eq!((rest.tag_alpha, open.tag_alpha), (1.0, 1.0));
        let mut last = f32::MAX;
        for i in 0..=20 {
            let g = footer_geometry(FOOTER_INNER_W, f, i as f32 / 20.0);
            assert!(g.bar_w <= last + EPS && g.bar_w >= FOOTER_BAR_MIN_W, "{g:?}");
            last = g.bar_w;
        }
    }

    #[test]
    fn the_tag_goes_at_rest_when_even_the_rest_layout_is_short_of_64() {
        // room = 388 - 330 - 12 = 46 < 64
        let f = fit(100.0, 230.0, true);
        for i in 0..=10 {
            let g = footer_geometry(FOOTER_INNER_W, f, i as f32 / 10.0);
            assert_eq!(g.tag_alpha, 0.0);
            assert!(g.bar_w >= FOOTER_BAR_MIN_W, "{g:?}");
        }
    }

    #[test]
    fn the_tag_fades_during_the_focus_transition_when_only_the_open_cue_pushes_it_out() {
        // rest room = 388 - 300 - 12 = 76 >= 64; focus room = 48 < 64
        let f = fit(100.0, 200.0, true);
        let at = |t| footer_geometry(FOOTER_INNER_W, f, t);
        assert_eq!(at(0.0).tag_alpha, 1.0);
        assert_eq!(at(1.0).tag_alpha, 0.0);
        let mut last = 1.0;
        for i in 0..=20 {
            let g = at(i as f32 / 20.0);
            assert!(g.tag_alpha <= last + EPS, "it fades out monotonically, and so back in on release");
            assert!(g.bar_w >= FOOTER_BAR_MIN_W - EPS, "{g:?}");
            last = g.tag_alpha;
        }
        // the tag slides out under the text group's left edge, so the time text never leaves a hole
        let mid = at(0.5);
        assert!(mid.tag_dx < 0.0 && at(0.0).tag_dx == 0.0 && at(1.0).tag_dx == -f.tag_w);
    }

    #[test]
    fn the_twelve_px_gap_holds_at_every_progress() {
        for (tag_w, status_w) in [(0.0, 90.0), (60.0, 120.0), (70.0, 210.0), (100.0, 200.0), (100.0, 230.0), (120.0, 269.0)] {
            let f = fit(tag_w, status_w, true);
            for i in 0..=40 {
                let g = footer_geometry(FOOTER_INNER_W, f, i as f32 / 40.0);
                assert!(gap(FOOTER_INNER_W, f, g) >= FOOTER_GAP - EPS, "{f:?} {g:?}: gap {}", gap(FOOTER_INNER_W, f, g));
            }
        }
    }

    #[test]
    fn a_card_with_no_cue_never_moves_its_text() {
        let f = fit(60.0, 120.0, false);
        for i in 0..=4 {
            let g = footer_geometry(FOOTER_INNER_W, f, i as f32 / 4.0);
            assert_eq!(g.text_dx, FOOTER_CUE_SLOT * 0.0);
        }
    }
}
