//! The **paged prose viewport** the read-only alert panels share: a column of paragraphs that a
//! viewer pages through with UP/DOWN, a feathered top and bottom edge, and the [`widgets::scroll_rail`]
//! beside it. The person page's biography (`screens::person_bio`) and the Detail About panel's
//! synopsis (`screens::about_panel`) both read through it, so the two cannot disagree about the
//! step, the feather, the tail under the last line or the rail.
//!
//! **The scroll is a [`Spring`]** chasing a page's resting position, so `gfx::spring` reports it to
//! the present gate and nothing here integrates milliseconds of its own. A page is a scroll POSITION
//! (`(page-1)` whole steps, the last pinned to the end of the travel), not a screenful: the step is
//! smaller than the viewport, so consecutive pages overlap and the eye keeps an anchor.
//!
//! **Layout is memoised by `TextView`'s own wrap cache**, keyed by content and width, so the flow
//! is laid out once per content/width change however many frames the spring takes to settle.
//! Paragraphs off the viewport are culled with [`crate::on_axis`] rather than merely clipped.
//!
//! **It does not clamp the page against the page count on a key press**, and that is load-bearing:
//! knowing the count means measuring wrapped prose, which reaches `plx_gfx::text` and turns a key
//! handler into a link error in the host suite. [`ProseScroll::step_page`] moves the cursor and
//! [`ProseScroll::follow`] (on the tick) pulls it back before anything reads it.

use crate::consts::{K_SCROLL, SDLK_DOWN, SDLK_UP};
use crate::text_view::TextView;
use crate::{theme, widgets, Painter, Rect, Spring};
use plx_machine::machine::Canon;
use std::os::raw::c_uint;

/// Air between paragraphs. A block gap, one `space` rung.
pub const PARA_GAP: f32 = theme::space::MD;
/// The design's trailing 40px spacer under the last paragraph: air at the END OF THE TRAVEL, so the
/// last line does not rest flush on the clip with the bottom feather already off.
pub const BODY_TAIL: f32 = theme::space::LG;
/// The dissolve band at each end of the viewport.
pub const FEATHER: f32 = theme::alert::FEATHER;
/// Air between the prose column and the rail beside it.
pub const RAIL_GAP: f32 = theme::space::MD;

/// The prose column's width inside a content column of `content_w`: the rail is part of the reading
/// block's frame, so the text may not flow under it.
pub fn text_w(content_w: f32) -> f32 {
    (content_w - widgets::RAIL_W - RAIL_GAP).max(1.0)
}

/// How far to the next resting place, as a function of the reader's line pitch: five lines. The
/// design's bio number (200 at a 40 pitch), kept as a line count so a panel with a different pitch
/// still pages in whole lines.
pub const fn step_for(lead: f32) -> f32 {
    5.0 * lead
}

/// **The paging arithmetic, pure**: how many pages `content_h` of prose makes in a `view_h`
/// viewport stepping `step` px, and the furthest the block may scroll.
///
/// `pages` is "how many distinct resting places are there", one more than the number of whole steps
/// the travel contains, and the LAST page is pinned to `max_scroll` rather than to
/// `(pages-1) * step`: the final step is a short one, and a rail that stopped short of its track's
/// end while the prose had visibly run out would be lying. Content that fits is one page with no
/// travel, which makes the rail and both feather edges disappear together.
pub fn paging(content_h: f32, view_h: f32, step: f32) -> (f32, usize) {
    let max_scroll = (content_h - view_h).max(0.0);
    if max_scroll <= 0.0 || step <= 0.0 {
        return (0.0, 1);
    }
    (max_scroll, (max_scroll / step).ceil() as usize + 1)
}

/// The scroll offset page `page` rests at — `(page-1)` whole steps, clamped to the travel. Pure.
pub fn scroll_at(page: usize, max_scroll: f32, step: f32) -> f32 {
    ((page.max(1) - 1) as f32 * step).min(max_scroll).max(0.0)
}

/// One paragraph's view. Built in ONE place so its measure and its draw cannot disagree about the
/// rung, the ink or the leading.
fn para_view(text: &str, lead: f32) -> TextView<'_> {
    TextView::new(text, theme::size::BODY, theme::TEXT_READING).h(theme::alert::TEXT_ALIGN).leading(lead)
}

/// Total flowed height of `paras` wrapped to `w` at pitch `lead`, with [`PARA_GAP`] between them and
/// the [`BODY_TAIL`] under the last. Zero for no paragraphs.
pub fn content_h(paras: &[&str], w: f32, lead: f32) -> f32 {
    if paras.is_empty() {
        return 0.0;
    }
    let mut h = 0.0;
    for (i, para) in paras.iter().enumerate() {
        if i > 0 {
            h += PARA_GAP;
        }
        h += para_view(para, lead).measure_h(w);
    }
    h + BODY_TAIL
}

/// The scrolling prose: every paragraph stacked at `-scroll`, inside a hard scissor at `view`.
///
/// **The clip is the hard cut; the DISSOLVE is the text's own glyphs fading**, via
/// [`TextView::edge_fade`], against the surface behind them — nothing is painted over the glass.
/// Each edge appears only when there is prose on the far side of it. Set and clear of the clip are
/// paired in this one function, because the scissor is global GL state.
pub fn draw(p: Painter, paras: &[&str], lead: f32, view: Rect, scroll: f32, max_scroll: f32) {
    if paras.is_empty() {
        return;
    }
    let w = view.w;
    let top = (scroll > 0.5).then_some((view.y, view.y + FEATHER));
    let bot = (scroll < max_scroll - 0.5).then_some((view.y + view.h - FEATHER, view.y + view.h));
    p.clip(view);
    let mut y = view.y - scroll;
    for para in paras {
        let v = para_view(para, lead).edge_fade(top, bot);
        let h = v.measure_h(w);
        if crate::on_axis(y - view.y, h, view.h, 0.0) {
            v.draw(p, Rect::new(view.x, y, w, 0.0));
        }
        y += h + PARA_GAP;
    }
    p.clip_clear();
}

/// The rail beside the viewport, flush to the content column's right edge `content_right`.
pub fn draw_rail(p: Painter, view: Rect, content_right: f32, page: usize, pages: usize) {
    widgets::scroll_rail(p, Rect::new(content_right - widgets::RAIL_W, view.y, widgets::RAIL_W, view.h), page, pages);
}

/// The cursor and the spring that chases it — the state both panels record.
pub struct ProseScroll {
    /// The current page, 1-based. The spring chases [`scroll_at`] of it, rather than the page being
    /// derived from the scroll: paging is the input, and a spring still travelling must not be read
    /// back as a different page half way there.
    pub page: usize,
    pub scroll: Spring,
}

impl Default for ProseScroll {
    fn default() -> Self {
        Self::new()
    }
}

impl ProseScroll {
    pub fn new() -> Self {
        Self { page: 1, scroll: Spring::at(0.0) }
    }

    /// UP/DOWN page the viewport; any other key is inert. Returns whether the cursor moved. May run
    /// one past the last page for a single frame — [`Self::follow`] pulls it back. `probe` names the
    /// spring for the animation probe.
    pub fn step_page(&mut self, sym: c_uint) -> bool {
        let next = match sym {
            SDLK_UP => self.page.saturating_sub(1).max(1),
            SDLK_DOWN => self.page + 1,
            _ => self.page,
        };
        let moved = next != self.page;
        self.page = next;
        moved
    }

    /// One tick: re-clamp the page to the content as it now stands (the store can land longer or
    /// shorter prose while the panel is open), then spring toward that page's resting offset.
    pub fn follow(&mut self, dt: f32, max_scroll: f32, pages: usize, step: f32, probe: &'static str) {
        self.page = self.page.clamp(1, pages);
        let want = scroll_at(self.page, max_scroll, step);
        self.scroll.step(want, K_SCROLL, dt);
        crate::anim::probe(probe, self.scroll.pos, self.scroll.vel, want, dt);
    }

    /// The offset to draw at: the spring, held inside the travel.
    pub fn offset(&self, max_scroll: f32) -> f32 {
        self.scroll.pos.clamp(0.0, max_scroll)
    }

    pub fn write(&self, c: &mut Canon) {
        c.u64(self.page as u64).f32(self.scroll.pos).f32(self.scroll.vel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_is_five_lines() {
        assert_eq!(step_for(40.0), 200.0);
    }

    #[test]
    fn follow_pulls_an_overshot_page_back_and_settles_on_the_end() {
        let (max, pages) = paging(500.0, 400.0, 200.0);
        let mut s = ProseScroll::new();
        s.step_page(SDLK_DOWN);
        s.step_page(SDLK_DOWN);
        s.step_page(SDLK_DOWN);
        for _ in 0..600 {
            s.follow(1.0 / 60.0, max, pages, 200.0, "test.scroll");
        }
        assert_eq!(s.page, pages);
        assert!((s.offset(max) - max).abs() < 0.5);
    }
}
