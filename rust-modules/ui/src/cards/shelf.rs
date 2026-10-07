//! [`Shelf`] — one horizontal strip of cards (a person's filmography shelf, a related row).
//!
//! It wraps the L0 [`CardRow`] (a spring per cell plus the scroll and label-band springs) and adds
//! what the row deliberately leaves to its caller: reading focus from the engine, the pop rule,
//! the draw loop with its cull and stops, `Focusable` placement and paging.

use plx_machine::machine::{
    Canon, Cx, Effects, EntryId, FocusKey, GroupId, Host, ScreenEvent,
};
use plx_machine::present::Provenance;

use super::{CardEvent, CardSource, SectionFrame, Tile};
use crate::card_row::{self, CardRow, RowStyle};
use crate::consts::SCR_W;
use crate::screen::{
    Activate, At, AxisMask, By, DrawFrame, Dir, EdgeRule, ElemKind, GroupKind, GroupSpec, Hover, Placed, Seat, Step,
    Stop,
};
use crate::{Painter, Rect};

/// Cards past the last visible one the source is asked for.
const LOOK_AHEAD: usize = 6;

pub struct Shelf {
    entry: EntryId,
    style: &'static RowStyle,
    row: CardRow,
    /// The cell a deliberate move (`By::Dir` / `By::Pointer`) just handed focus to: its pop grows
    /// from rest. Cleared by the next tick. Any other focused cell is adopted at full pop.
    armed: Option<usize>,
    ahead: usize,
    asked: Option<(usize, usize)>,
}

impl Shelf {
    pub const fn new(entry: EntryId, style: &'static RowStyle) -> Self {
        Self { entry, style, row: CardRow::new(), armed: None, ahead: LOOK_AHEAD, asked: None }
    }

    /// Cards beyond the visible window a [`CardEvent::Want`] asks for (default 6).
    pub const fn look_ahead(mut self, n: usize) -> Self {
        self.ahead = n;
        self
    }

    pub fn style(&self) -> &'static RowStyle {
        self.style
    }

    /// The one entry: feed it every event the screen receives.
    pub fn on<H: Host, S: CardSource<H>>(
        &mut self,
        ev: &ScreenEvent<H>,
        cx: &Cx<'_, H>,
        src: &S,
        fx: &mut Effects<'_, H>,
    ) -> Option<CardEvent<H::Elem>> {
        match ev {
            ScreenEvent::Tick(t) => {
                self.tick(t.dt(), cx, src);
                self.want(cx, src)
            }
            ScreenEvent::FocusMoved { from, to, by } => {
                let here = |e: &H::Elem| src.index_of(e);
                let arrived = (to.entry == self.entry).then(|| here(&to.elem)).flatten();
                self.armed = arrived.filter(|_| matches!(by, By::Dir | By::Pointer));
                let left = from.filter(|k| k.entry == self.entry).and_then(|k| here(&k.elem));
                if arrived.is_some() || left.is_some() {
                    fx.invalidate(Provenance::Input);
                }
                None
            }
            ScreenEvent::PressCommit(_) => {
                self.focused_elem(cx, src).map(|(_, e)| CardEvent::Activate(e))
            }
            ScreenEvent::PressHold(_) => self
                .focused_elem(cx, src)
                .filter(|&(i, _)| src.holdable(i))
                .map(|(_, e)| CardEvent::Hold(e)),
            // A covering menu changes nothing here: the page keeps its focus and the settled pop is
            // what the opener redraw reads.
            _ => None,
        }
    }

    fn focused_elem<H: Host, S: CardSource<H>>(&self, cx: &Cx<'_, H>, src: &S) -> Option<(usize, H::Elem)> {
        super::focused_index(&cx.focus, self.entry, src).map(|i| (i, src.elem(i)))
    }

    fn tick<H: Host, S: CardSource<H>>(&mut self, dt: f32, cx: &Cx<'_, H>, src: &S) {
        let focus = super::focused_index(&cx.focus, self.entry, src);
        if let Some(i) = focus {
            if self.row.focus() != i as i32 && self.armed != Some(i) {
                self.row.adopt(i, self.style);
            }
        }
        self.armed = None;
        if focus.is_some() || !self.row.at_exact_rest() {
            self.row.update(src.len(), focus, self.style, dt);
            if focus.is_none() {
                self.row.park();
            }
        }
    }

    /// The paging rule: the window's last card plus look-ahead (or the focused card's, if further).
    fn want<H: Host, S: CardSource<H>>(&mut self, cx: &Cx<'_, H>, src: &S) -> Option<CardEvent<H::Elem>> {
        let pitch = self.style.w + self.style.gap;
        let window = ((self.row.scroll_x() + SCR_W - self.style.margin_x) / pitch).ceil().max(0.0) as usize;
        let focus = super::focused_index(&cx.focus, self.entry, src).map_or(0, |i| i + 1);
        let end = window.max(focus).saturating_add(self.ahead);
        super::want(&mut self.asked, src.len(), end, src.more()).map(CardEvent::Want)
    }

    /// The pop scale of card `i` given the engine's focus: the live spring for a card the shelf
    /// has been told about, FULL for a focused card it has not (adopted whole, never a one-frame
    /// collapse), the live let-go or rest for every other.
    fn pop(&self, i: usize, focus: Option<usize>) -> f32 {
        if focus == Some(i) && self.row.focus() != i as i32 && self.armed != Some(i) {
            self.style.focus_scale
        } else {
            self.row.scale(i)
        }
    }

    /// The live pop of `elem` (no press), for tests and the opener's redraw.
    pub fn scale_of<H: Host, S: CardSource<H>>(&self, cx: &Cx<'_, H>, src: &S, elem: &H::Elem) -> Option<f32> {
        let i = src.index_of(elem)?;
        Some(self.pop(i, super::focused_index(&cx.focus, self.entry, src)))
    }

    fn pitch(&self) -> f32 {
        self.style.w + self.style.gap
    }

    /// Card `i`'s settled (unpopped) rect in the section's painter space.
    fn slot(&self, i: usize, at: SectionFrame) -> Rect {
        card_row::tile_rect(i, self.style.margin_x, self.pitch(), self.row.scroll_x(), at.y,
            (self.style.w, self.style.h))
    }

    /// Where `elem` is: the LIVE drawn rect (pop and press folded in) for `At::Drawn`, the settled
    /// one for `At::SpringTarget`; `rest_rect` is the settled focus-scaled rect either way.
    pub fn place<H: Host, S: CardSource<H>>(
        &self,
        cx: &Cx<'_, H>,
        src: &S,
        elem: &H::Elem,
        at: SectionFrame,
        how: At,
    ) -> Option<Placed> {
        let i = src.index_of(elem)?;
        let focus = super::focused_index(&cx.focus, self.entry, src);
        let slot = self.slot(i, at);
        let s = match how {
            At::Drawn => super::press_scale(self.pop(i, focus), focus == Some(i), cx),
            At::SpringTarget => if focus == Some(i) { self.style.focus_scale } else { 1.0 },
        };
        Some(Placed {
            rect: slot.scaled(s),
            rest_rect: slot.scaled(self.style.focus_scale),
            clip: at.clip,
            index: Some(i as u32),
        })
    }

    /// Draw the shelf into `p` (the section's painter: page offset and alpha already applied) and
    /// register its stops: non-focused cards first, the focused one last; only on-axis cards paint
    /// and only they resolve artwork or register a stop.
    pub fn draw<H: Host, S: CardSource<H>>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, src: &S, at: SectionFrame) {
        let n = src.len();
        let focus = super::focused_index(&f.focus, self.entry, src);
        let sx = self.row.scroll_x();
        let pr = p.translate(-sx, 0.0);
        let visible = |i: usize| crate::on_axis(self.slot(i, at).x, self.style.w, SCR_W, 0.0);
        for i in (0..n).filter(|&i| focus != Some(i) && visible(i)) {
            let s = self.pop(i, focus);
            self.draw_card(f, pr, src, i, at, s, false);
        }
        if let Some(i) = focus.filter(|&i| i < n) {
            let s = super::press_scale(self.pop(i, focus), true, f.cx);
            self.draw_card(f, pr, src, i, at, s, true);
        }
        self.record_stops(f, p, src, at);
    }

    /// Register the stops of the on-axis cards: each is the rect [`draw`](Self::draw) paints.
    pub fn record_stops<H: Host, S: CardSource<H>>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, src: &S, at: SectionFrame) {
        if !f.records_stops() {
            return;
        }
        let focus = super::focused_index(&f.focus, self.entry, src);
        for i in (0..src.len()).filter(|&i| crate::on_axis(self.slot(i, at).x, self.style.w, SCR_W, 0.0)) {
            let s = super::press_scale(self.pop(i, focus), focus == Some(i), f.cx);
            let slot = self.slot(i, at);
            f.stop(p, Stop {
                key: FocusKey { entry: self.entry, elem: src.elem(i) },
                rect: slot.scaled(s),
                rest_rect: slot.scaled(self.style.focus_scale),
                clip: at.clip,
                hover: Hover::Focus,
                activate: Activate::Press,
            });
        }
    }

    /// The opener redraw: card `focus` drawn alone, popped and captioned exactly as in-page, over
    /// whatever covers the page. Nothing is painted for an element not in this source.
    pub fn redraw_focused<H: Host, S: CardSource<H>>(
        &self,
        f: &mut DrawFrame<'_, '_, H>,
        p: Painter,
        src: &S,
        at: SectionFrame,
        focus: Option<FocusKey<H::Elem>>,
    ) {
        let Some(i) = focus.filter(|k| k.entry == self.entry).and_then(|k| src.index_of(&k.elem)) else { return };
        let s = super::press_scale(self.pop(i, Some(i)), true, f.cx);
        self.draw_card(f, p.translate(-self.row.scroll_x(), 0.0), src, i, at, s, true);
    }

    /// One card at scale `s` in the scrolled painter `pr`. The rect and the treatment derive from
    /// the same `s`.
    #[allow(clippy::too_many_arguments)]
    fn draw_card<H: Host, S: CardSource<H>>(
        &self,
        f: &DrawFrame<'_, '_, H>,
        pr: Painter,
        src: &S,
        i: usize,
        at: SectionFrame,
        s: f32,
        focused: bool,
    ) {
        let unscrolled = card_row::tile_rect(i, self.style.margin_x, self.pitch(), 0.0, at.y,
            (self.style.w, self.style.h));
        let rect = unscrolled.scaled(s);
        if focused {
            let label = src.label(i).revealed(self.row.band_reveal())
                .settling(self.row.settle_lag(src.len(), i, self.style));
            card_row::draw_focused(pr, src.art(i), rect, s, self.style, src.progress(i), &label, f.measure);
        } else {
            card_row::draw_tile(pr, src.art(i), rect, s, self.style, src.progress(i));
        }
        let tile = Tile { rect, scale: s, radius: self.style.tile_radius(rect, s), focused };
        src.overlay(pr, i, &tile, f.measure);
    }

    /// The `Focusable` neighbour of `key` inside the shelf: left and right by index, the edge
    /// otherwise (the screen's group edge rules take it from there).
    pub fn neighbour<H: Host, S: CardSource<H>>(&self, src: &S, key: FocusKey<H::Elem>, dir: Dir) -> Step<H::Elem> {
        let Some(i) = src.index_of(&key.elem) else { return Step::Edge };
        let to = match dir {
            Dir::Left => i.checked_sub(1),
            Dir::Right => Some(i + 1).filter(|&j| j < src.len()),
            Dir::Up | Dir::Down => None,
        };
        to.map_or(Step::Edge, |j| Step::Move(FocusKey { entry: self.entry, elem: src.elem(j) }))
    }

    /// The card a vertical move into this shelf lands on: the one nearest `from`'s centre.
    pub fn seat<H: Host, S: CardSource<H>>(&self, src: &S, from: Placed) -> Option<FocusKey<H::Elem>> {
        let n = src.len();
        if n == 0 {
            return None;
        }
        let i = card_row::column_near_x(from.rect.cx(), self.style.margin_x, self.pitch(), self.style.w,
            self.row.scroll_x(), n, from.index.unwrap_or(0) as usize);
        Some(FocusKey { entry: self.entry, elem: src.elem(i) })
    }

    /// The group this shelf registers with the focus engine.
    pub fn group_spec(&self, id: GroupId, len: usize, extent: Rect) -> GroupSpec {
        GroupSpec {
            id,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent,
            len,
            elem: ElemKind::Card,
        }
    }

    pub fn scroll(&self) -> f32 {
        self.row.scroll_x()
    }

    /// Restore a saved viewport (`n` is the current card count).
    pub fn restore_scroll(&mut self, scroll: f32, n: usize) {
        self.row.restore_scroll(scroll, n, self.style);
    }

    /// The row's heading lift and caption band, for the caller's section layout.
    pub fn row(&self) -> &CardRow {
        &self.row
    }

    pub fn write(&self, c: &mut Canon) {
        self.row.write_motion(c);
    }
}
