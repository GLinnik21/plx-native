//! **A landscape still's one-line footer**: status text on the left, a mark on the right, on the
//! 24 px line. The deck places it 8 px below the art and the episode strip insets it 12 px.
//!
//! It replaces the two-line `label` / `sub` pair on the Continue Watching card and the detail page's
//! episode strip (which is inside one season of one show). The pair stays as
//! [`crate::widgets::still_line`] for the shelves that carry the show's name;
//! [`crate::widgets::still_overlay`] chooses between them (a `Continue` card keeps the show name on a
//! line above the footer only for a card whose art carries no title and that did not opt in to the
//! deck's art walk; a deck card's title is in its art: the cover, the composite or, with neither, type).
//!
//! * **In progress**: `S2 E4 · 38m left` (a film: `38m left`) on the left, the amber progress bar
//!   (100 px, shrinking to a 64 px minimum, 6 px high below the art or 4 px high in the strip) on the right.
//! * **Watched** (the strip): the length on the left, a muted 22 px tick on the right.
//! * **Not started**: on the strip the length alone; on a Continue Watching card an episode nobody
//!   has played reads `S2 E1 · New episode` (the started line's tag, then `widgets.card.new_episode`), no bar.
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
//! **Where the line stands is [`Place`]**: [`Place::InArt`] (the detail page's episode strip) on the
//! art's own bottom, 12 px above its edge, and [`Place::Below`] (the Continue Watching row, owner's
//! board CW-13 v2) OFF the art: the line's top [`theme::STILL_FOOTER_DROP`] under the tile's foot, 4 px
//! inset each side, its bar 6 px tall (radius 3), the art above it clean (no scrim, four rounded
//! corners). The fit rule is the same function; only the inner width it is judged on differs.
//!
//! The geometry is judged on the card's RESTING inner width (`388` = 420 - 2 x 16, in the art; 412 below): the drawn rect
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
/// The bar and its radius on the line BELOW the art ([`Place::Below`]).
pub const FOOTER_BELOW_BAR_H: f32 = 6.0;
pub const FOOTER_BELOW_BAR_RAD: f32 = 3.0;
/// The line BELOW the art stands this far in from the tile's own edges.
pub const FOOTER_BELOW_INSET: f32 = 4.0;
/// The room a [`Place::Below`] row spends under its tiles past a poster shelf's standard air
/// ([`crate::card_row::RowStyle::under_tile`]): the line's drop and height, and the heading air under
/// it ([`theme::STILL_FOOTER_AIR`]) less the `space::LG` a poster shelf already leaves.
pub const FOOTER_UNDER_TILE: f32 = theme::STILL_FOOTER_DROP + FOOTER_LINE_H + theme::STILL_FOOTER_AIR - theme::space::LG;
/// The watched tick's box.
pub const FOOTER_TICK_D: f32 = 22.0;
/// The resting inner width the fit rule is judged on: the episode tile's 420 less both insets.
pub const FOOTER_INNER_W: f32 = crate::card_row::RowStyle::EPISODE.w - 2.0 * crate::widgets::STILL_LINE_INSET;
/// The same, for a line BELOW the art: the tile's 420 less its 4 px insets.
pub const FOOTER_BELOW_INNER_W: f32 = crate::card_row::RowStyle::STILL.w - 2.0 * FOOTER_BELOW_INSET;

/// Where a footer line stands against its tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// On the art's own bottom (`12 px above its edge`), inset 16: the episode strip.
    InArt,
    /// Under the art, [`theme::STILL_FOOTER_DROP`] below its foot, inset 4: the Continue Watching row.
    Below,
}

impl Place {
    /// The line's inset from the tile's left and right edges.
    pub const fn inset(self) -> f32 {
        match self {
            Place::InArt => crate::widgets::STILL_LINE_INSET,
            Place::Below => FOOTER_BELOW_INSET,
        }
    }
    /// The resting inner width the fit rule is judged on.
    pub const fn inner_w(self) -> f32 {
        match self {
            Place::InArt => FOOTER_INNER_W,
            Place::Below => FOOTER_BELOW_INNER_W,
        }
    }
    /// The progress bar's height and corner radius.
    pub const fn bar(self) -> (f32, f32) {
        match self {
            Place::InArt => (FOOTER_BAR_H, FOOTER_BAR_RAD),
            Place::Below => (FOOTER_BELOW_BAR_H, FOOTER_BELOW_BAR_RAD),
        }
    }
    /// The line's top edge for a tile drawn at `card` (the DRAWN rect, so the line rides the pop).
    pub fn line_top(self, card: Rect) -> f32 {
        match self {
            Place::InArt => card.y + card.h - FOOTER_RISE - FOOTER_LINE_H,
            Place::Below => card.y + card.h + theme::STILL_FOOTER_DROP,
        }
    }
}

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
    pub place: Place,
}

/// The measured widths [`footer_geometry`] decides from. `tag_w` includes the space after the tag
/// (0 when there is no tag).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FooterFit {
    pub tag_w: f32,
    pub status_w: f32,
    /// The cue slot the line may open: [`FOOTER_CUE_SLOT`], or 0 on a card with no cue.
    pub cue_w: f32,
    /// Whether a progress bar stands at the line's right end. With one, the tag is kept only while
    /// the bar keeps its 64 px and the [`FOOTER_GAP`]; with none (an unstarted episode's
    /// "S2 E1 · New episode") it is kept while the whole text fits the line.
    pub bar: bool,
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

/// The widths [`footer_geometry`] decides from, measured the way the draw lays the line out: the
/// status run in the regular `size::CAPTION` face, and the tag as the advance it adds IN FRONT of the
/// status (`"S2 E4 · 38m left"` less `"38m left"`, so the space after the dot is included and a
/// kerned pair is not double counted). `cue_w` is the slot the line may open, `bar` whether a bar
/// stands at the right end.
pub fn fit_of(tag: &str, status: &str, cue_w: f32, bar: bool, measure: &dyn Measure) -> FooterFit {
    let sz = theme::size::CAPTION;
    let status_w = measure.width_str(status, sz, false);
    let tag_w = if tag.is_empty() {
        0.0
    } else if status.is_empty() {
        measure.width_str(tag, sz, false)
    } else {
        measure.width_str(&format!("{tag} {status}"), sz, false) - status_w
    };
    FooterFit { tag_w, status_w, cue_w, bar }
}

/// **The fit rule**, pure: where the text goes, whether the tag shows and how wide the bar is, for
/// an inner width `inner_w`, the measured `fit` and the focus progress `focus` (0 rest, 1 open).
///
/// The room the bar can take is `inner - cue - (tag shown + status) - 12`. The tag is KEPT while that
/// room is at least 64 (`FOOTER_BAR_MIN_W`): judged at rest, and again with the cue slot fully open.
/// Kept at both: it stays. Gone at rest: it never shows. Kept at rest but not with the cue open:
/// it fades with the focus progress, so the same transition that opens the cue closes the tag up
/// (the room is linear in `focus` between two ends that both clear 64, so the bar never drops below
/// it on the way). Whatever the tag does, the bar is `clamp(room, 0, 100)`, which is what holds the
/// 12 px gap at every progress value.
pub fn footer_geometry(inner_w: f32, fit: FooterFit, focus: f32) -> FooterGeo {
    let t = focus.clamp(0.0, 1.0);
    let full = fit.tag_w + fit.status_w;
    // What the right end must keep: the bar's least width and the air before it, or nothing.
    let right_min = if fit.bar { FOOTER_GAP + FOOTER_BAR_MIN_W } else { 0.0 };
    let keeps = |cue: f32| inner_w - cue - full >= right_min;
    let tag_alpha = if fit.tag_w <= 0.0 || !keeps(0.0) {
        0.0
    } else if keeps(fit.cue_w) {
        1.0
    } else {
        1.0 - t
    };
    let text_dx = fit.cue_w * t;
    let room = inner_w - text_dx - (fit.tag_w * tag_alpha + fit.status_w) - FOOTER_GAP;
    let bar_w = if fit.bar { room.clamp(0.0, FOOTER_BAR_W) } else { 0.0 };
    FooterGeo { text_dx, tag_alpha, tag_dx: -fit.tag_w * (1.0 - tag_alpha), bar_w }
}

/// Draw `footer` on the line at the bottom of `card` (the DRAWN rect, so the line rides the focus
/// pop). `label` is the optional show name that stands above the line (a card whose title is not on
/// its art); see [`crate::widgets::still_overlay`].
pub fn draw_footer(p: Painter, card: Rect, label: &str, f: &Footer<'_>, measure: &dyn Measure) {
    let sz = theme::size::CAPTION;
    let (sct, scb) = plx_gfx::text::text_cap_band(sz, 0);
    let place = f.place;
    let cy = place.line_top(card) + FOOTER_LINE_H * 0.5;
    let sy = cy - (sct + scb) * 0.5;
    // The show's name stands over the line ON the art: a line below the art has no ground for it.
    if !label.is_empty() && place == Place::InArt {
        crate::widgets::still_label_over(p, card, label, cy + (scb - sct) * 0.5, measure);
    }
    let inset = place.inset();
    let (left, right) = (card.x + inset, card.x + card.w - inset);
    let cue_w = if f.cue.is_some() { FOOTER_CUE_SLOT } else { 0.0 };
    let t = if f.cue.is_some() { f.focus.clamp(0.0, 1.0) } else { 0.0 };

    // The cue stands at the leading edge and fades in; the text group makes room for it.
    if let (Some(cue), true) = (f.cue, t > 0.0) {
        let d = crate::widgets::STILL_GLYPH_D;
        let icon = match cue {
            Cue::Play => crate::icons::Icon::Play,
            Cue::Info => crate::icons::Icon::Info,
        };
        crate::icons::draw(p.alpha(t), icon, Rect::new(left, cy - d * 0.5, d, d), theme::TEXT_PRIMARY);
    }

    let fit = fit_of(f.tag, f.status, cue_w, matches!(f.right, Right::Bar(_)), measure);
    let geo = match f.right {
        Right::Bar(_) | Right::None => footer_geometry(place.inner_w(), fit, t),
        // a mark that does not flex: the tag, if there is one, is simply shown
        Right::Tick => FooterGeo { text_dx: cue_w * t, tag_alpha: 1.0, tag_dx: 0.0, bar_w: 0.0 },
    };
    let x_l = left + geo.text_dx;
    let right_limit = match f.right {
        Right::Bar(_) if geo.bar_w > 0.0 => right - geo.bar_w - FOOTER_GAP,
        Right::Tick => right - FOOTER_TICK_D - FOOTER_GAP,
        _ => right,
    };

    if !f.tag.is_empty() && geo.tag_alpha > 0.0 {
        if let Ok(tag) = std::ffi::CString::new(f.tag) {
            // the part of the tag that has slid out is cut at the text group's own edge, so it
            // never runs under the cue. The scope INTERSECTS with a scissor the page already holds
            // (Home's `ClipScope` under its top bar); the bare `p.clip` pair would replace it. A
            // recording painter opens nothing, so the slide is graded by position and alpha.
            let _cut = (geo.tag_alpha < 1.0).then(|| crate::screen::ClipScope::open_in(p, Rect::new(x_l, card.y, card.w, card.h)));
            p.alpha(geo.tag_alpha).text(tag.as_ptr(), x_l + geo.tag_dx, sy, sz, theme::TEXT_SECONDARY, 0, 0);
        }
    }
    if !f.status.is_empty() {
        let x = x_l + fit.tag_w * geo.tag_alpha;
        // half a pixel of tolerance: `footer_geometry` made the bar yield exactly this room, and a
        // card at a fractional x must not shave the time's last glyph for an ulp
        let run = measure.fit_line(f.status, (right_limit - x + 0.5).max(0.0), sz, false);
        p.text(run.as_ptr(), x, sy, sz, theme::TEXT_SECONDARY, 0, 0);
    }

    match f.right {
        Right::Bar(frac) if geo.bar_w > 0.0 => {
            let (bar_h, bar_rad) = place.bar();
            crate::widgets::progress_bar(p, Rect::new(right - geo.bar_w, cy - bar_h * 0.5, geo.bar_w, bar_h), bar_rad, bar_h, frac)
        }
        Right::Tick => crate::icons::draw(
            p,
            crate::icons::Icon::Check,
            Rect::new(right - FOOTER_TICK_D, cy - FOOTER_TICK_D * 0.5, FOOTER_TICK_D, FOOTER_TICK_D),
            theme::TEXT_SECONDARY,
        ),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 0.001;

    fn fit(tag_w: f32, status_w: f32, cue: bool) -> FooterFit {
        FooterFit { tag_w, status_w, cue_w: if cue { FOOTER_CUE_SLOT } else { 0.0 }, bar: true }
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

    // ---- every shipped language, measured with the TV's own advances ---------------------------

    use plx_base::fontcov::advances::ShippedMeasure;
    use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};

    /// The board's drafts for the usual line (S2 E4, 38 min left), by language tag: the whole line,
    /// or for a language whose tag the board drops, the status alone. `ca` and `hu` are not on the
    /// board (drafts of ours, unreviewed) and so carry no expectation. The board's Polish draft
    /// spells the episode `O`; the app's one address spelling (`core.episode`) is `E`, so `pl` here
    /// is `E` and the draft's letter is a native-review question.
    const BOARD_USUAL: [(&str, &str); 10] = [
        ("en", "S2 E4 · 38m left"),
        ("de", "S2 E4 · noch 38 Min."),
        ("fr", "S2 E4 · 38 min restantes"),
        ("es", "T2 E4 · quedan 38 min"),
        ("it", "S2 E4 · 38 min rimasti"),
        ("pt", "T2 E4 · faltam 38 min"),
        ("nl", "S2 A4 · nog 38 min"),
        ("pl", "S2 E4 · zostało 38 min"),
        ("ru", "S2 E4 · осталось 38 мин"),
        ("be", "С2 Э4 · засталося 38 хв"),
    ];

    /// The board's measured widths (board fonts), usual / worst case S12 E24 1h 12m.
    const BOARD_WIDTHS: [(&str, f32, f32); 10] = [
        ("en", 180.0, 248.0),
        ("de", 234.0, 338.0),
        ("fr", 275.0, 349.0),
        ("es", 255.0, 329.0),
        ("it", 246.0, 320.0),
        ("pt", 241.0, 315.0),
        ("nl", 216.0, 290.0),
        ("pl", 256.0, 378.0),
        ("ru", 289.0, 363.0),
        ("be", 280.0, 389.0),
    ];

    /// `(tag, status)` of the usual line and of the worst case, in the thread's language.
    fn cases() -> [(&'static str, String, String); 2] {
        let cx = plx_platform::i18n::current();
        [
            ("usual", crate::fmt::footer_tag_in(cx, 2, 4), crate::fmt::footer_left_minutes_in(cx, 38)),
            ("worst", crate::fmt::footer_tag_in(cx, 12, 24), crate::fmt::footer_left_minutes_in(cx, 72)),
        ]
    }

    #[test]
    fn the_usual_line_reads_as_the_boards_draft_in_every_board_language() {
        for (tag, want) in BOARD_USUAL {
            let language = SHIPPED.into_iter().find(|l| l.tag() == tag).expect(tag);
            let _g = language_on_this_thread_for_test(language);
            let [(_, t, s), _] = cases();
            assert_eq!(format!("{t} {s}"), want, "{tag}");
        }
    }

    /// **The rule holds for every shipped language, at the usual line and the worst case, at every
    /// progress of the focus transition**: the bar never leaves its 64..100 band and the 12 px gap
    /// between the text and the bar never closes. A language added to `SHIPPED` is covered here with
    /// no edit. Run with `--nocapture` for the per-language fit table.
    #[test]
    fn every_shipped_language_fits_at_every_focus_progress() {
        let mut table = String::from("lang case  label_w  tag@rest tag@open  bar@rest bar@open  board_w\n");
        for language in SHIPPED {
            let tag = language.tag();
            let _g = language_on_this_thread_for_test(language);
            for (case, t, s) in cases() {
                let f = fit_of(&t, &s, FOOTER_CUE_SLOT, true, &ShippedMeasure);
                assert!(
                    FOOTER_INNER_W - FOOTER_CUE_SLOT - f.status_w - FOOTER_GAP >= FOOTER_BAR_MIN_W,
                    "{tag} {case}: even with no tag the time text {:.0} px leaves the bar under 64 px with the cue open",
                    f.status_w
                );
                for i in 0..=40 {
                    let g = footer_geometry(FOOTER_INNER_W, f, i as f32 / 40.0);
                    assert!(gap(FOOTER_INNER_W, f, g) >= FOOTER_GAP - EPS, "{tag} {case} {i}/40: {g:?}");
                    assert!(g.bar_w >= FOOTER_BAR_MIN_W - EPS && g.bar_w <= FOOTER_BAR_W + EPS, "{tag} {case} {i}/40: {g:?}");
                    assert!((g.text_dx - FOOTER_CUE_SLOT * i as f32 / 40.0).abs() < EPS, "{tag} {case}: the text moves by the cue slot");
                }
                let (rest, open) = (footer_geometry(FOOTER_INNER_W, f, 0.0), footer_geometry(FOOTER_INNER_W, f, 1.0));
                let board = BOARD_WIDTHS.iter().find(|(l, _, _)| *l == tag).map(|(_, u, w)| if case == "usual" { *u } else { *w });
                table += &format!(
                    "{tag:<4} {case:<6} {:>7.0}  {:<8} {:<8}  {:>8.0} {:>8.0}  {}\n",
                    f.tag_w + f.status_w,
                    if rest.tag_alpha > 0.0 { "kept" } else { "off" },
                    if open.tag_alpha >= 1.0 { "kept" } else { "off" },
                    rest.bar_w,
                    open.bar_w,
                    board.map_or("-".into(), |b| format!("{b:.0}")),
                );
                // the real font and the board's agree to within the board's own rounding of fonts
                if let Some(b) = board {
                    let w = f.tag_w + f.status_w;
                    assert!((w / b - 1.0).abs() < 0.3, "{tag} {case}: measured {w:.0} px vs the board's {b:.0}");
                }
            }
        }
        eprintln!("{table}");
    }

    /// **An unstarted episode's `S12 E24 · New episode`, in every shipped language, at every progress
    /// of the focus transition**: no bar stands at the right end, so the rule is that nothing is
    /// cut. The text group (cue slot opened by the focus, tag if kept, "New episode") is never wider
    /// than the line, the tag goes before the words do (`New episode` is the catalogue's own
    /// `widgets.card.new_episode`, the line's meaning: it is never elided or dropped), and a language
    /// whose words alone do not fit the line with the cue open fails here. Run with `--nocapture` for
    /// which languages keep the tag at rest and with the cue open.
    #[test]
    fn every_shipped_language_fits_a_new_episode_line_at_every_focus_progress() {
        let mut table = String::from("lang  line_w  words_w  tag@rest tag@open  room@rest room@open\n");
        for language in SHIPPED {
            let tag = language.tag();
            let _g = language_on_this_thread_for_test(language);
            let cx = plx_platform::i18n::current();
            let (t, s) = (crate::fmt::footer_tag_in(cx, 12, 24), plx_platform::i18n::msg::widgets_card_new_episode_in(cx).to_owned());
            let f = fit_of(&t, &s, FOOTER_CUE_SLOT, false, &ShippedMeasure);
            assert!(
                FOOTER_INNER_W - FOOTER_CUE_SLOT - f.status_w >= 0.0,
                "{tag}: 'New episode' ({:.0} px) does not fit the line with the cue open even with the tag gone",
                f.status_w
            );
            for i in 0..=40 {
                let g = footer_geometry(FOOTER_INNER_W, f, i as f32 / 40.0);
                let end = g.text_dx + f.tag_w * g.tag_alpha + f.status_w;
                assert!(end <= FOOTER_INNER_W + EPS, "{tag} {i}/40: the text ends at {end:.1} of {FOOTER_INNER_W}: {g:?}");
                assert_eq!(g.bar_w, 0.0, "{tag}: no bar stands on an unstarted card");
                assert!((g.text_dx - FOOTER_CUE_SLOT * i as f32 / 40.0).abs() < EPS, "{tag}: the text moves by the cue slot");
                assert!(g.tag_alpha == 0.0 || g.tag_alpha == 1.0 || (g.tag_alpha - (1.0 - i as f32 / 40.0)).abs() < EPS, "{tag} {i}/40: {g:?}");
            }
            let (rest, open) = (footer_geometry(FOOTER_INNER_W, f, 0.0), footer_geometry(FOOTER_INNER_W, f, 1.0));
            table += &format!(
                "{tag:<5} {:>6.0} {:>8.0}  {:<8} {:<8}  {:>9.0} {:>9.0}\n",
                f.tag_w + f.status_w,
                f.status_w,
                if rest.tag_alpha > 0.0 { "kept" } else { "off" },
                if open.tag_alpha >= 1.0 { "kept" } else { "off" },
                FOOTER_INNER_W - (f.tag_w * rest.tag_alpha + f.status_w),
                FOOTER_INNER_W - FOOTER_CUE_SLOT - (f.tag_w * open.tag_alpha + f.status_w),
            );
        }
        eprintln!("{table}");
    }

    /// Without a bar the tag is kept while the whole text fits the line (cue included) and is judged
    /// the way the bar's rule is: at rest, then with the cue open.
    #[test]
    fn with_no_bar_the_tag_is_kept_while_the_text_fits_the_line() {
        let f = |tag_w: f32, status_w: f32| FooterFit { tag_w, status_w, cue_w: FOOTER_CUE_SLOT, bar: false };
        let kept = footer_geometry(FOOTER_INNER_W, f(70.0, 200.0), 1.0);
        assert_eq!((kept.tag_alpha, kept.bar_w), (1.0, 0.0), "270 + the cue 28 < 388: kept with the cue open");
        // 100 + 270 = 370 fits at rest but not with the 28 px cue open: the tag fades with the focus
        let fades = footer_geometry(FOOTER_INNER_W, f(100.0, 270.0), 0.5);
        assert_eq!(fades.tag_alpha, 0.5);
        assert_eq!(footer_geometry(FOOTER_INNER_W, f(100.0, 270.0), 1.0).tag_alpha, 0.0);
        // 130 + 270 = 400 > 388 even at rest: the tag never shows
        assert_eq!(footer_geometry(FOOTER_INNER_W, f(130.0, 270.0), 0.0).tag_alpha, 0.0);
    }

    // ---- the draw, through the census -----------------------------------------------------------

    fn runs_at(focus: f32, status: &str, tag: &str) -> Vec<crate::draw_census::Run> {
        runs_in(Place::InArt, focus, status, tag)
    }

    fn runs_in(place: Place, focus: f32, status: &str, tag: &str) -> Vec<crate::draw_census::Run> {
        let card = Rect::new(0.0, 0.0, 420.0, 236.0);
        let m = crate::fixture::FixtureMeasure;
        crate::draw_census::capture_runs(|| {
            draw_footer(
                Painter::recording(),
                card,
                "",
                &Footer { tag, status, right: Right::Bar(0.4), cue: Some(Cue::Play), focus, place },
                &m,
            )
        })
    }

    /// **The line BELOW the art** (owner's board CW-13 v2): its top [`theme::STILL_FOOTER_DROP`] under
    /// the tile's foot, the line 24 tall, 4 px in from both edges, the bar 6 px tall with radius 3 and
    /// at its 100 px width. The art keeps its own bottom: nothing of the line is inside the card.
    #[test]
    fn the_line_below_the_art_stands_off_it_with_a_six_px_bar() {
        let _g = plx_base::testlock::serial();
        let card = Rect::new(100.0, 50.0, 420.0, 236.0);
        let foot = card.y + card.h;
        let m = crate::fixture::FixtureMeasure;
        let f = Footer { tag: "", status: "38m left", right: Right::Bar(0.4), cue: None, focus: 0.0, place: Place::Below };
        let bars: Vec<_> = crate::draw_census::capture_inks(|| draw_footer(Painter::recording(), card, "", &f, &m))
            .into_iter()
            .filter(|(tag, r, _)| *tag == 2 && r.h < 20.0)
            .collect();
        assert!(!bars.is_empty(), "the bar was drawn");
        for (_, r, _) in &bars {
            assert_eq!(r.h, 6.0, "6 px tall");
            assert_eq!(r.w, FOOTER_BAR_W, "100 px wide at rest");
            assert_eq!(r.x + r.w, card.x + card.w - 4.0, "its right end stands on the 4 px inset");
            assert_eq!(r.y + r.h * 0.5, foot + theme::STILL_FOOTER_DROP + FOOTER_LINE_H * 0.5, "centred on the line");
            assert!(r.y > foot, "all of it OFF the art: {r:?}");
        }
        assert_eq!(Place::Below.bar(), (6.0, 3.0));
        assert_eq!(Place::Below.line_top(card), foot + 8.0);
        let line = runs_in(Place::Below, 0.0, "38m left", "");
        assert_eq!(line[0].at.x, 4.0, "the text starts on the 4 px inset");
        // and the in-art line is where it always was: 12 px up from the edge, inset 16, a 4 px bar
        assert_eq!(Place::InArt.line_top(card), foot - FOOTER_RISE - FOOTER_LINE_H);
        assert_eq!(Place::InArt.bar(), (4.0, 2.0));
        assert_eq!(runs_in(Place::InArt, 0.0, "38m left", "")[0].at.x, crate::widgets::STILL_LINE_INSET);
        // the room a row spends under its tiles is exactly the line, its drop and the heading's air
        assert_eq!(FOOTER_UNDER_TILE + theme::space::LG, theme::STILL_FOOTER_DROP + FOOTER_LINE_H + theme::STILL_FOOTER_AIR);
        assert_eq!(FOOTER_BELOW_INNER_W, 412.0);
    }

    /// The rule holds on the BELOW line's 412 px for every shipped language, as it does on the 388 px
    /// in-art line: the bar stays in its 64..100 band, the 12 px gap holds, and the unstarted line's
    /// words are never cut, at every progress of the focus transition.
    #[test]
    fn the_line_below_the_art_fits_every_shipped_language_at_every_focus_progress() {
        let inner = Place::Below.inner_w();
        for language in SHIPPED {
            let tag = language.tag();
            let _g = language_on_this_thread_for_test(language);
            for (case, t, s) in cases() {
                let f = fit_of(&t, &s, FOOTER_CUE_SLOT, true, &ShippedMeasure);
                for i in 0..=40 {
                    let g = footer_geometry(inner, f, i as f32 / 40.0);
                    assert!(gap(inner, f, g) >= FOOTER_GAP - EPS, "{tag} {case} {i}/40: {g:?}");
                    assert!(g.bar_w >= FOOTER_BAR_MIN_W - EPS && g.bar_w <= FOOTER_BAR_W + EPS, "{tag} {case} {i}/40: {g:?}");
                }
            }
            let cx = plx_platform::i18n::current();
            let (t, s) = (crate::fmt::footer_tag_in(cx, 12, 24), plx_platform::i18n::msg::widgets_card_new_episode_in(cx).to_owned());
            let f = fit_of(&t, &s, FOOTER_CUE_SLOT, false, &ShippedMeasure);
            for i in 0..=40 {
                let g = footer_geometry(inner, f, i as f32 / 40.0);
                let end = g.text_dx + f.tag_w * g.tag_alpha + f.status_w;
                assert!(end <= inner + EPS, "{tag} {i}/40: the text ends at {end:.1} of {inner}: {g:?}");
            }
        }
    }

    #[test]
    fn the_text_moves_right_by_exactly_the_cue_slot_as_focus_rises() {
        let _g = plx_base::testlock::serial();
        let x = |focus| runs_at(focus, "38m left", "")[0].at.x;
        assert_eq!(x(0.0), crate::widgets::STILL_LINE_INSET);
        assert!((x(1.0) - x(0.0) - FOOTER_CUE_SLOT).abs() < EPS);
        assert!((x(0.5) - x(0.0) - FOOTER_CUE_SLOT * 0.5).abs() < EPS);
    }

    #[test]
    fn a_tag_the_open_cue_pushes_out_fades_and_the_time_closes_up_behind_it() {
        let _g = plx_base::testlock::serial();
        // FixtureMeasure is 12 px a character: this status leaves 76 px of room at rest with the
        // tag and 48 with the cue open
        let status = "abcdefghijklmno";
        let rest = runs_at(0.0, status, "S2 E4 \u{b7}");
        assert_eq!(rest.len(), 2, "tag and status");
        let (tag_w, left) = (rest[1].at.x - rest[0].at.x, rest[0].at.x);
        assert!(tag_w > 0.0);
        let mid = runs_at(0.5, status, "S2 E4 \u{b7}");
        assert_eq!(mid.len(), 2);
        assert_eq!(mid[0].at.x, left + FOOTER_CUE_SLOT * 0.5 - tag_w * 0.5, "the tag slides out as it fades");
        assert!(mid[0].ink[3] < rest[0].ink[3] && mid[0].ink[3] > 0.0, "and fades");
        let open = runs_at(1.0, status, "S2 E4 \u{b7}");
        assert_eq!(open.len(), 1, "the tag is gone");
        assert_eq!(open[0].at.x, left + FOOTER_CUE_SLOT, "the time stands where the tag's slot closed up");
    }

    /// **What a Continue Watching still says** through the one `still_overlay` door: the show's name, then
    /// `S2 E4 · 38m left` and, for a card nobody has started, the same tag and `New episode`, no bar.
    /// These are cards that did not opt in to the deck's art (a frame between a publication and its
    /// projection): the line stands under the art, so the name is set in type on the art instead,
    /// and an opted-in card's own title is the art's (the next test).
    #[test]
    fn a_continue_watching_still_says_the_show_the_tag_and_the_time_left() {
        use crate::tile::{Resume, TileFacts};
        let _g = plx_base::testlock::serial();
        let card = Rect::new(0.0, 0.0, 420.0, 236.0);
        let m = crate::fixture::FixtureMeasure;
        let footer = crate::widgets::StillFooter::Continue { cue: Cue::Info, pop: 0.0 };
        let words = |facts: TileFacts<'_>| {
            crate::draw_census::capture_runs(|| {
                crate::widgets::still_overlay(Painter::recording(), &facts, card, 12.0, footer, &m)
            })
            .into_iter()
            .map(|r| r.text)
            .collect::<Vec<_>>()
        };
        let started = TileFacts {
            show_title: "Severance",
            season_index: 2,
            ep_index: 4,
            resume: Some(Resume { frac: 0.3, left_ms: 38 * 60_000 }),
            ..Default::default()
        };
        assert_eq!(words(started), ["Severance", "S2 E4 \u{b7}", "38m left"]);
        let film = TileFacts { title: "Heat", resume: Some(Resume { frac: 0.3, left_ms: 38 * 60_000 }), ..Default::default() };
        assert_eq!(words(film), ["Heat", "38m left"], "a film has no tag");
        let next_up = TileFacts { show_title: "Severance", season_index: 2, ep_index: 5, unwatched: true, ..Default::default() };
        assert_eq!(
            words(next_up),
            ["Severance", "S2 E5 \u{b7}", "New episode"],
            "nothing to count down: the same tag a started episode wears, then the catalogue's New episode, no bar"
        );
        let unnumbered = TileFacts { show_title: "Severance", unwatched: true, ..Default::default() };
        assert_eq!(words(unnumbered), ["Severance"], "an unstarted card with no address has only its name");
    }

    /// **A deck card's title is drawn ONCE, and in the art**: Plex's cover and our composite carry
    /// it in the picture, so the card draws no title of its own; with neither, the Type rung draws it
    /// in type at the logo slot, and the footer never carries it. A card that did not opt in (its
    /// art is a bare still or poster) keeps the show's name above its footer.
    #[test]
    fn an_opted_in_card_draws_its_title_once_in_type_and_only_when_the_art_carries_none() {
        use crate::tile::{DeckArt, Resume, TileFacts};
        let _g = plx_base::testlock::serial();
        let card = Rect::new(0.0, 0.0, 420.0, 236.0);
        let m = crate::fixture::FixtureMeasure;
        let runs = |facts: TileFacts<'_>, footer: crate::widgets::StillFooter| {
            crate::draw_census::capture_runs(|| crate::widgets::still_overlay(Painter::recording(), &facts, card, 12.0, footer, &m))
        };
        let cont = crate::widgets::StillFooter::Continue { cue: Cue::Info, pop: 0.0 };
        let started = TileFacts {
            show_title: "Severance",
            season_index: 2,
            ep_index: 4,
            resume: Some(Resume { frac: 0.3, left_ms: 38 * 60_000 }),
            ..Default::default()
        };
        // neither a cover nor a composite exists: the title, once, in type, above the footer line
        let bare = TileFacts { deck: Some(DeckArt::default()), ..started };
        let r = runs(bare, cont);
        let titles: Vec<_> = r.iter().filter(|run| run.text == "Severance").collect();
        assert_eq!(titles.len(), 1, "{r:?}");
        assert_eq!(titles[0].size, theme::size::HEADLINE, "set at the Card rung's type size");
        assert!(titles[0].bold);
        assert!(titles[0].at.y + 1.0 < 236.0 - crate::theme::logo::CARD_BOTTOM_INSET + 1.0, "at the logo slot: {:?}", titles[0].at);
        let words: Vec<_> = r.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(&words[1..], ["S2 E4 \u{b7}", "38m left"], "the footer carries the tag and the status only");
        // a cover (or a composite) names the title in the art: the card draws none
        let covered = TileFacts { deck: Some(DeckArt { cover: "/plx/cover/x", card: "" }), ..started };
        let words: Vec<_> = runs(covered, cont).into_iter().map(|run| run.text).collect();
        assert_eq!(words, ["S2 E4 \u{b7}", "38m left"]);
        let composed = TileFacts { deck: Some(DeckArt { cover: "", card: "/plx/card/1/a" }), ..started };
        assert_eq!(runs(composed, cont).len(), 2);
        // the shelf footer follows the same rule: the address, and no show line above it
        let shelf = |facts| runs(facts, crate::widgets::StillFooter::Shelf).into_iter().map(|run| run.text).collect::<Vec<_>>();
        assert_eq!(shelf(covered), ["S2 \u{b7} E4"], "a cover carries the title");
        assert_eq!(shelf(bare), ["Severance", "S2 \u{b7} E4"], "type carries it once");
        assert_eq!(shelf(TileFacts { deck: None, ..started }), ["Severance", "S2 \u{b7} E4"], "a plain still keeps its show line");
        // a film names itself by its own title
        let film = TileFacts { title: "Heat", deck: Some(DeckArt::default()), ..Default::default() };
        assert_eq!(runs(film, cont).iter().filter(|run| run.text == "Heat").count(), 1);
    }

    #[test]
    fn a_card_with_a_tick_keeps_its_text_clear_of_the_tick() {
        let _g = plx_base::testlock::serial();
        let card = Rect::new(0.0, 0.0, 420.0, 236.0);
        let m = crate::fixture::FixtureMeasure;
        let long = "x".repeat(60);
        let runs = crate::draw_census::capture_runs(|| {
            draw_footer(
                Painter::recording(),
                card,
                "",
                &Footer { tag: "", status: &long, right: Right::Tick, cue: None, focus: 0.0, place: Place::InArt },
                &m,
            )
        });
        let r = &runs[0];
        assert!(r.at.x + r.at.w <= 420.0 - 16.0 - FOOTER_TICK_D - FOOTER_GAP + EPS, "{:?}", r.at);
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
