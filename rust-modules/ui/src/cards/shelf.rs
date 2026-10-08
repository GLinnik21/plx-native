//! [`Shelf`] — one horizontal strip of cards (a person's filmography shelf, a related row).
//!
//! It wraps the L0 [`CardRow`] (a spring per cell plus the scroll and label-band springs) and adds
//! what the row deliberately leaves to its caller: reading focus from the engine, the pop rule,
//! the draw loop with its cull and stops, `Focusable` placement and paging.

use plx_machine::machine::{
    Canon, Cx, Effects, EntryId, FocusKey, GroupId, Host, ScreenEvent,
};
use plx_machine::present::Provenance;

use super::pool::{Drawn, RowPool};
use super::{CardEvent, CardSource, Landed, SectionFrame, Seen, Tile};
use crate::card_row::{self, CardRow, RowStyle};
use crate::consts::SCR_W;
use crate::screen::{
    Activate, At, AxisMask, DrawFrame, Dir, EdgeRule, ElemKind, GroupKind, GroupSpec, Placed, Seat, Step,
    Stop,
};
use crate::{Painter, Rect};

/// Cards past the last visible one the source is asked for.
const LOOK_AHEAD: usize = 6;

pub struct Shelf {
    entry: EntryId,
    style: &'static RowStyle,
    row: CardRow,
    /// What the `FocusMoved`s since the last tick said: a deliberate move (`By::Dir` /
    /// `By::Pointer`) grows its cell from rest, any other arrival is adopted at full pop, and none
    /// at all means a changed focused index is a content landing. Cleared by the tick.
    seen: Seen,
    /// The landing the last tick carried the focused pop through, if there was one.
    landed: Option<Landed>,
    /// A `FocusMoved` since the last tick took focus off a card of this shelf: a deliberate way
    /// out, which lets that tile go over frames. Without one, a focused card that is no longer in
    /// the source left it in a content landing (it joined another section), and nobody lets go.
    left: bool,
    ahead: usize,
    asked: Option<(usize, usize)>,
    /// How far past the screen edge a card still paints and registers a stop (default 0).
    margin: f32,
    /// The page is still dissolving in ([`dormant`](Shelf::dormant)): no card is lifted yet.
    dormant: bool,
    /// The last tick ran dormant, so the next awake one starts the focused card's pop from rest.
    slept: bool,
    /// The element-keyed pop pool, empty unless the dev switch is on (`pool.rs`).
    pool: RowPool,
}

impl Shelf {
    pub const fn new(entry: EntryId, style: &'static RowStyle) -> Self {
        Self { entry, style, row: CardRow::new(), seen: Seen::Nothing, landed: None, left: false, ahead: LOOK_AHEAD, asked: None,
            margin: 0.0, dormant: false, slept: false, pool: RowPool::new() }
    }

    /// Cards this far past the screen's leading (left) edge still paint (a popped card's shadow
    /// reaches onto the screen from beyond it) and register their stops; the trailing edge is not
    /// widened. Default 0.
    pub const fn cull_margin(mut self, px: f32) -> Self {
        self.margin = px;
        self
    }

    /// Hold every card at rest while the owner's page is still dissolving in: a lifted tile under
    /// a fade plays its selection where nobody can see it. The shelf keeps no pop meanwhile (the
    /// springs ease to rest like an unfocused shelf's); the first awake tick starts the focused
    /// card's pop from rest, as a deliberate move does (a restore or reconcile announced since the last
    /// dormant tick is still adopted whole). Set it before each [`on`](Self::on).
    pub fn dormant(&mut self, held: bool) {
        self.dormant = held;
    }

    /// Cards beyond the visible window a [`CardEvent::Want`] asks for (default 6).
    pub const fn look_ahead(mut self, n: usize) -> Self {
        self.ahead = n;
        self
    }

    pub fn style(&self) -> &'static RowStyle {
        self.style
    }

    /// The y the screen last laid the shelf out at (kept in the motion canon).
    pub fn base_y(&self) -> f32 {
        self.row.base_y
    }

    pub fn set_base_y(&mut self, y: f32) {
        self.row.base_y = y;
    }

    /// How far card `i`'s caption block still trails the scroll (`n` cards in the shelf).
    pub fn settle_lag(&self, n: usize, i: usize) -> f32 {
        self.row.settle_lag(n, i, self.style)
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
                if let Some(a) = arrived {
                    self.seen = Seen::of(by, a);
                }
                let left = from.filter(|k| k.entry == self.entry).and_then(|k| here(&k.elem));
                self.left |= left.is_some();
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
        let focus = super::focused_index(&cx.focus, self.entry, src)
            .filter(|_| !self.dormant);
        self.landed = None;
        if self.dormant {
            // nothing is lifted under the fade, and whatever was lets go like an unfocused shelf's
            self.slept = true;
            self.pool.clear();
            self.seen = Seen::Nothing;
            self.left = false;
            if !self.row.at_exact_rest() {
                self.row.update(src.len(), None, self.style, dt);
                self.row.park();
            }
            return;
        }
        if std::mem::take(&mut self.slept) {
            // an arrival announced since the last dormant tick (a restore, a reconcile) keeps its
            // own rule; only a focus no event told the shelf about wakes growing from rest
            if let (Some(i), Seen::Nothing) = (focus, self.seen) {
                self.seen = Seen::Deliberate(i);
            }
        }
        // the element-keyed pool: springs follow their elements before anything reads a cell
        let pooled = super::pool::pop_pool();
        let carried = if pooled {
            self.pool.follow(&mut self.row, src, focus)
        } else {
            self.pool.clear();
            false
        };
        if let Some(i) = focus {
            let prev = self.prev();
            match (self.seen, prev) {
                // a deliberate move: grows from rest, and the tile it left lets go over frames
                (Seen::Deliberate(a), _) if a == i => {}
                // no FocusMoved, another index: the same element, moved by a content landing
                (Seen::Nothing, Some(p)) if p != i => {
                    let dx = (i as f32 - p as f32) * self.pitch();
                    if carried {
                        self.row.shift_scroll(dx);
                    } else {
                        self.row.relocate(p, i, dx);
                    }
                    self.row.clamp_scroll(src.len(), self.style);
                    self.landed = Some(Landed { from: p, to: i });
                }
                // a restore, a reconcile, a seat: adopted whole, the old tile straight to rest
                _ if prev != Some(i) => {
                    if let Some(p) = prev {
                        self.row.rest(p);
                    }
                    self.row.adopt(i, self.style);
                }
                _ => {}
            }
        }
        // the focused element left the source (a landing moved it to another section): the tile
        // now at its index must not be left lifted, shrinking over frames
        if let (None, Some(p), false) = (focus, self.prev(), self.left) {
            self.row.rest(p);
        }
        self.seen = Seen::Nothing;
        self.left = false;
        if focus.is_some() || !self.row.at_exact_rest() {
            self.row.update(src.len(), focus, self.style, dt);
            if focus.is_none() {
                self.row.park();
            }
        }
        if pooled {
            self.pool.admit(&self.row, src, focus);
        }
    }

    /// Is the pop of cell `cell` held in the element-keyed pool?
    #[cfg(test)]
    pub(crate) fn pool_holds(&self, cell: usize) -> bool {
        self.pool.holds(cell)
    }

    /// Held pops in the element-keyed pool.
    #[cfg(test)]
    pub(crate) fn pool_len(&self) -> usize {
        self.pool.len()
    }

    /// The paging rule: the window's last card plus look-ahead (or the focused card's, if further).
    fn want<H: Host, S: CardSource<H>>(&mut self, cx: &Cx<'_, H>, src: &S) -> Option<CardEvent<H::Elem>> {
        let pitch = self.style.w + self.style.gap;
        let window = ((self.row.scroll_x() + SCR_W - self.style.margin_x) / pitch).ceil().max(0.0) as usize;
        let focus = super::focused_index(&cx.focus, self.entry, src).map_or(0, |i| i + 1);
        let end = window.max(focus).saturating_add(self.ahead);
        super::want(&mut self.asked, src.len(), end, src.more()).map(CardEvent::Want)
    }

    /// The cell the row last recorded as focused.
    fn prev(&self) -> Option<usize> {
        usize::try_from(self.row.focus()).ok()
    }

    /// The landing the last tick carried the focused card's pop through (see [`Landed`]).
    pub fn landed(&self) -> Option<Landed> {
        self.landed
    }

    /// The pop scale of card `i` given the engine's focus, exactly what the next tick will have
    /// made of it, so no frame shows two lifted tiles: the live spring for a card the shelf has
    /// been told about (a deliberate move grows from rest, the tile it left lets go), the landed
    /// element's spring at its new index with the old one at rest, FULL for a focused card adopted
    /// whole (never a one-frame collapse) with the one it left at rest.
    fn pop<H: Host, S: CardSource<H>>(&self, src: &S, i: usize, focus: Option<usize>) -> f32 {
        // the element-keyed pool: a source change no tick has seen yet is resolved by element
        if super::pool::pop_pool() && focus.is_some_and(|_| !self.dormant) {
            match self.pool.drawn(src, i, focus == Some(i)) {
                Some(Drawn::Cell(c)) => return self.row.scale(c),
                Some(Drawn::Rest) => return 1.0,
                None => {}
            }
        }
        self.pop_at(i, focus)
    }

    /// [`pop`](Self::pop) by position alone: what the draw paints when the tick has seen the source.
    fn pop_at(&self, i: usize, focus: Option<usize>) -> f32 {
        let Some(j) = focus.filter(|_| !self.dormant) else { return self.row.scale(i) };
        let prev = self.prev();
        if prev == Some(j) || self.seen == Seen::Deliberate(j) {
            return self.row.scale(i);
        }
        let live = match (self.seen, prev) {
            (Seen::Nothing, Some(p)) => self.row.scale(p),
            _ => self.style.focus_scale,
        };
        if i == j {
            live
        } else if prev == Some(i) {
            1.0
        } else {
            self.row.scale(i)
        }
    }

    /// The live pop of `elem` (no press), for tests and the opener's redraw.
    pub fn scale_of<H: Host, S: CardSource<H>>(&self, cx: &Cx<'_, H>, src: &S, elem: &H::Elem) -> Option<f32> {
        let i = src.index_of(elem)?;
        Some(self.pop(src, i, super::focused_index(&cx.focus, self.entry, src)))
    }

    fn pitch(&self) -> f32 {
        self.style.w + self.style.gap
    }

    /// Card `i`'s settled (unpopped) rect in SCREEN space.
    fn slot(&self, i: usize, at: SectionFrame, sx: f32) -> Rect {
        card_row::tile_rect(i, self.style.margin_x, self.pitch(), sx, at.y, (self.style.w, self.style.h))
    }

    /// The scroll the row is drawn at: the spring's, scaled by [`CardSource::sweep`].
    fn drawn_scroll<H: Host, S: CardSource<H>>(&self, src: &S) -> f32 {
        self.row.scroll_x() * src.sweep()
    }

    /// The settled, unpopped rect of the shelf's first card in SCREEN space: where the strip
    /// starts, for a screen that registers the shelf's group extent there.
    pub fn head(&self, at: SectionFrame) -> Rect {
        self.slot(0, at, self.row.scroll_x())
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
        let slot = self.slot(i, at, self.drawn_scroll(src));
        let s = match how {
            At::Drawn => super::press_scale(self.pop(src, i, focus), focus == Some(i), cx),
            At::SpringTarget => if focus == Some(i) { self.style.focus_scale } else { 1.0 },
        };
        Some(Placed {
            rect: slot.scaled(s),
            rest_rect: slot.scaled(self.style.focus_scale),
            clip: at.clip,
            index: Some(i as u32),
        })
    }

    /// Draw the shelf into `p` (alpha already applied; any translate it carries is undone, see
    /// [`SectionFrame`]) and register its stops: [`paint`](Self::paint), then
    /// [`record_stops`](Self::record_stops).
    pub fn draw<H: Host, S: CardSource<H>>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, src: &S, at: SectionFrame) {
        self.paint(f, p, src, at);
        self.record_stops(f, p, src, at);
    }

    /// Paint the cards without registering stops ([`paint_resting`](Self::paint_resting), then
    /// [`paint_focused`](Self::paint_focused)), for a screen that records its page's stops in
    /// its own order.
    pub fn paint<H: Host, S: CardSource<H>>(&self, f: &DrawFrame<'_, '_, H>, p: Painter, src: &S, at: SectionFrame) {
        self.paint_resting(f, p, src, at);
        self.paint_focused(f, p, src, at);
    }

    /// The on-axis cards but the focused one (only on-axis cards paint and resolve artwork), for a
    /// screen that draws the focused card of ALL its shelves after every shelf's others, so its
    /// glow overlaps the neighbouring shelves.
    pub fn paint_resting<H: Host, S: CardSource<H>>(&self, f: &DrawFrame<'_, '_, H>, p: Painter, src: &S, at: SectionFrame) {
        let n = src.len();
        let focus = super::focused_index(&f.focus, self.entry, src);
        let sx = self.drawn_scroll(src);
        let pr = p.translate(-sx, 0.0);
        let visible = |i: usize| crate::on_axis(self.slot(i, at, sx).x, self.style.w, SCR_W, self.margin);
        for i in (0..n).filter(|&i| focus != Some(i) && visible(i)) {
            let s = self.pop(src, i, focus);
            self.draw_card(f, pr, src, i, at, s, false);
        }
    }

    /// The focused card, painted whether or not it is on-axis.
    pub fn paint_focused<H: Host, S: CardSource<H>>(&self, f: &DrawFrame<'_, '_, H>, p: Painter, src: &S, at: SectionFrame) {
        let focus = super::focused_index(&f.focus, self.entry, src);
        let pr = p.translate(-self.drawn_scroll(src), 0.0);
        if let Some(i) = focus.filter(|&i| i < src.len()) {
            let s = super::press_scale(self.pop(src, i, focus), true, f.cx);
            self.draw_card(f, pr, src, i, at, s, true);
        }
    }

    /// Register the stops of the on-axis cards: each is the rect [`draw`](Self::draw) paints. Unlike
    /// [`paint`](Self::paint), which always paints the focused card (even past the cull), there is no
    /// focused exception here: a focused card off the axis registers no stop.
    pub fn record_stops<H: Host, S: CardSource<H>>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, src: &S, at: SectionFrame) {
        if !f.records_stops() {
            return;
        }
        let focus = super::focused_index(&f.focus, self.entry, src);
        let sx = self.drawn_scroll(src);
        for i in (0..src.len()).filter(|&i| crate::on_axis(self.slot(i, at, sx).x, self.style.w, SCR_W, self.margin)) {
            let s = super::press_scale(self.pop(src, i, focus), focus == Some(i), f.cx);
            let slot = super::to_local(p, self.slot(i, at, sx));
            f.stop(p, Stop {
                key: FocusKey { entry: self.entry, elem: src.elem(i) },
                rect: slot.scaled(s),
                rest_rect: slot.scaled(self.style.focus_scale),
                clip: at.clip,
                hover: src.hover(i),
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
        let s = super::press_scale(self.pop(src, i, Some(i)), true, f.cx);
        self.draw_card(f, p.translate(-self.drawn_scroll(src), 0.0), src, i, at, s, true);
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
        if !src.loaded(i) {
            return;
        }
        let unscrolled = card_row::tile_rect(i, self.style.margin_x, self.pitch(), 0.0, at.y - pr.dy(),
            (self.style.w, self.style.h));
        let rect = unscrolled.scaled(s);
        if focused {
            let label = src.label(i).revealed(self.row.band_reveal())
                .settling(self.row.settle_lag(src.len(), i, self.style) * src.sweep());
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

    /// Card `i`'s pop as the draw paints it before any press, given the index of the focused card
    /// (the focus rule and [`dormant`](Self::dormant) included): what a probe reads between frames.
    #[cfg(any(test, feature = "test-support"))]
    pub fn drawn_pop(&self, i: usize, focus: Option<usize>) -> f32 {
        self.pop_at(i, focus)
    }

    /// Every spring is parked exactly at rest: nothing here needs stepping.
    #[cfg(any(test, feature = "test-support"))]
    pub fn at_rest(&self) -> bool {
        self.row.at_exact_rest()
    }

    /// The scroll spring's velocity, for a diagnostic witness.
    #[cfg(feature = "devtriggers")]
    pub fn scroll_velocity(&self) -> f32 {
        self.row.scroll_velocity()
    }

    /// Restore a saved viewport (`n` is the current card count).
    pub fn restore_scroll(&mut self, scroll: f32, n: usize) {
        self.row.restore_scroll(scroll, n, self.style);
    }

    /// How far the shelf's heading must rise, live, to clear the focused tile.
    pub fn heading_lift(&self) -> f32 {
        self.row.lift()
    }

    /// The caption band the shelf reserves under its tiles right now, for the caller's layout.
    pub fn under_band(&self) -> f32 {
        self.row.under_band()
    }

    /// The live label-band expansion, 0 collapsed to 1 focused: what a screen that lays its column
    /// out from the band's TRAVEL (`card_row::BAND_OPEN` times this) reads.
    pub fn band_expand(&self) -> f32 {
        self.row.band_expand()
    }

    pub fn write(&self, c: &mut Canon) {
        self.row.write_motion(c);
    }
}
