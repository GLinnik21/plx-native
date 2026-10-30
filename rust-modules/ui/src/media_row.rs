//! **MediaRow** — one wide row of a media LIST: a poster at the leading edge, a title, up to
//! [`MAX_LINES`] lines under it, a status chip at the trailing edge and, while the title is on its
//! way, the amber progress band along the poster's foot. The Seerr tab's request list is the first
//! caller (docs/seerr.md); any list whose row is a title with a state reads the same way.
//!
//! It exists because nothing else draws a row with art: a `table::Row` is a form or menu row at
//! form height, and its poster slot was retired for the filmography list, where a 54 x 81 chip
//! repeated two hundred times read as grey rectangles. A request list is a page of at most a few
//! dozen titles, so its poster is drawn at [`POSTER_W`] x [`POSTER_H`], large enough to be the
//! picture.
//!
//! The vocabulary is the app's own, so the row adds no mark of its own:
//!
//! - **Focus** is the table's: the [`theme::ACCENT`] plate (`table::PILL_RAD`) under the whole row,
//!   the title in [`theme::ACCENT_INK`] and the lines one step back ([`theme::ROW_VALUE_INK_ON`]).
//!   At rest the title is [`theme::TEXT_PRIMARY`] and the lines [`theme::TEXT_SECONDARY`].
//! - **Progress** is the poster's amber band (`card_row::resume_bar`), the mark that means "how far
//!   along" on every card; the row never draws a second bar of its own.
//! - **The chip** is a [`widgets::badge`] outlined in the row's ink, keyed to the plate under it.
//!
//! **Who owns the text** (the fit rule, `ui/CLAUDE.md`): the title is the server's and elides; a
//! line is the caller's to compose and elides too, because it carries server words (a user's name,
//! a release title); the chip is the app's and must fit, which the caller's `fit` test checks
//! against [`CHIP_MAX_W`].
//!
//! Text stands by its cap band, stacked by [`VStack`] and centred on the row; nothing here places a
//! line by a guessed y.
use std::ffi::CString;

use plx_machine::machine::Measure;

use crate::hstack::Item;
use crate::label::{HAlign, Label, VAlign};
use crate::placeholder::{self, Reason};
use crate::vstack::{Anchor, VStack};
use crate::widgets::{self, BadgeStyle};
use crate::{consts, theme, Painter, Rect};

/// The poster, 2:3.
pub const POSTER_W: f32 = 100.0;
pub const POSTER_H: f32 = 150.0;
/// The poster's corners: a card's ring radius at this width.
pub const POSTER_RAD: f32 = theme::CARD_RING_RAD * POSTER_W / consts::CARD_W;
/// Air between the plate's edge and the poster, and above and below it.
pub const PAD: f32 = theme::space::SM;
/// One row: the poster and the air above and below it.
pub const ROW_H: f32 = POSTER_H + 2.0 * PAD;
/// Air between the poster and the text, and between the text and the chip.
pub const GUTTER: f32 = theme::space::MD;
/// How many lines a row carries under its title.
pub const MAX_LINES: usize = 3;
/// The widest chip a row makes room for: an app-owned chip must fit inside it in every shipped
/// language (the caller's fit test).
pub const CHIP_MAX_W: f32 = 320.0;

/// The type rungs: the title at BODY, bold; the lines at LABEL.
const TITLE_SIZE: i32 = theme::size::BODY;
const LINE_SIZE: i32 = theme::size::LABEL;
/// Air between the title's cap band and the first line's, and between lines.
const LINE_GAP: f32 = theme::space::SM;

/// The poster's source: a server whose photo proxy fetches it (the raw `ServerId`, as `tex` takes
/// it) and the URL. `None` on the row is a title with no poster at all, drawn as an absent face.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Poster<'a> {
    pub server: u16,
    pub url: &'a str,
}

/// One row, as the caller composed it. Borrowed for one draw.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MediaRow<'a> {
    /// Server text: elides.
    pub title: &'a str,
    /// The lines under the title, top to bottom; an empty one takes no room.
    pub lines: [&'a str; MAX_LINES],
    /// App text at the trailing edge (a request's state); empty draws no chip.
    pub chip: &'a str,
    /// 0..1 along the poster's foot, while something is on its way.
    pub progress: Option<f32>,
    pub poster: Option<Poster<'a>>,
    pub focused: bool,
}

impl MediaRow<'_> {
    /// The poster's rect in a row drawn in `r`.
    pub fn poster_rect(r: Rect) -> Rect {
        Rect::new(r.x + PAD, r.y + (r.h - POSTER_H) * 0.5, POSTER_W, POSTER_H)
    }

    /// The text column's left edge and its width before the chip, in a row drawn in `r`.
    pub fn text_span(&self, r: Rect, measure: &dyn Measure) -> (f32, f32) {
        let x = r.x + PAD + POSTER_W + GUTTER;
        let chip = self.chip_w(measure);
        let right = r.x + r.w - PAD - if chip > 0.0 { chip + GUTTER } else { 0.0 };
        (x, (right - x).max(0.0))
    }

    /// The chip's drawn width, 0 for none.
    pub fn chip_w(&self, measure: &dyn Measure) -> f32 {
        if self.chip.is_empty() { 0.0 } else { widgets::badge_w(self.chip, None, measure) }
    }

    /// The cap bands of the title and every present line, centred on the row: `(y, h)` each, `None`
    /// for a line that is empty.
    pub fn bands(&self, r: Rect, measure: &dyn Measure) -> [Option<(f32, f32)>; 1 + MAX_LINES] {
        let mut items = [Item::new(measure.cap_h(TITLE_SIZE)); 1 + MAX_LINES];
        for (i, line) in self.lines.iter().enumerate() {
            items[1 + i] = if line.is_empty() { Item::absent() } else { Item::new(measure.cap_h(LINE_SIZE)) };
        }
        let stack = VStack::new(&items, LINE_GAP, Anchor::Centre(r.y + r.h * 0.5));
        std::array::from_fn(|i| stack.span(i))
    }

    /// Paint the row in `r` (screen space): the plate when focused, the poster and its band, the
    /// text, the chip.
    pub fn draw(&self, p: Painter, r: Rect, measure: &dyn Measure) {
        if self.focused {
            p.rrect(r, crate::table::PILL_RAD, crate::table::PILL_RAD, theme::ACCENT);
        }
        self.draw_poster(p, Self::poster_rect(r));
        let (title_ink, line_ink) = if self.focused {
            (theme::ACCENT_INK, theme::ROW_VALUE_INK_ON)
        } else {
            (theme::TEXT_PRIMARY, theme::TEXT_SECONDARY)
        };
        let (x, w) = self.text_span(r, measure);
        let bands = self.bands(r, measure);
        let runs = std::iter::once((self.title, TITLE_SIZE, true, title_ink))
            .chain(self.lines.iter().map(|line| (*line, LINE_SIZE, false, line_ink)));
        for ((text, size, bold, ink), band) in runs.zip(bands) {
            let Some((y, h)) = band else { continue };
            let fitted = plx_machine::machine::elide_by(text, w, false, |s| measure.width_str(s, size, bold));
            let Ok(c) = CString::new(fitted) else { continue };
            let label = Label::new(c.as_ptr(), size, ink).h(HAlign::Left).v(VAlign::CapTop);
            if bold { label.bold() } else { label }.draw(p, Rect::new(x, y, w, h));
        }
        if !self.chip.is_empty() {
            // The chip centres on the title's line; its interior knocks out what is under it (the
            // plate, else the page), as a table row's chip does.
            let cy = bands[0].map_or(r.y + r.h * 0.5, |(y, h)| y + h * 0.5);
            let (border, bg) = if self.focused {
                (theme::ACCENT_INK, theme::ACCENT)
            } else {
                (theme::OVERLAY_BORDER, theme::SURFACE_APP)
            };
            let ink = title_ink;
            let x = r.x + r.w - PAD - self.chip_w(measure);
            widgets::badge(p, x, cy, self.chip, None, BadgeStyle::Outlined { col: ink, border, bg }, measure);
        }
    }

    fn draw_poster(&self, p: Painter, rect: Rect) {
        let Some(poster) = self.poster else {
            // placeholder-exempt: a title the server has no poster for; nothing will arrive.
            p.rrect(rect, POSTER_RAD, POSTER_RAD, theme::CARD_ABSENT);
            return self.draw_band(p, rect);
        };
        let (tex, _, _) = widgets::resolve_tex_wh_on(poster.server, poster.url, consts::CARD_W as i32,
            consts::CARD_H as i32, 0);
        if tex == 0 {
            placeholder::skeleton(p, Reason::CardSkeleton, poster.url, rect, POSTER_RAD);
        } else {
            p.tex_uv(tex, plx_gfx::gfx::UV_FULL, rect, POSTER_RAD, theme::TINT_WHITE);
        }
        self.draw_band(p, rect);
    }

    fn draw_band(&self, p: Painter, rect: Rect) {
        if let Some(frac) = self.progress {
            crate::card_row::resume_bar(p, rect, frac, POSTER_RAD);
        }
    }
}

#[cfg(test)]
#[path = "media_row_tests.rs"]
mod tests;
