//! [`Grid`] — the six-column poster grid with a collapsing caption band under the focused row
//! (`poster_grid`'s free functions are its geometry; the Collection page is its first adopter).
//!
//! It owns the scroll, the caption bands and the focus pop ([`GridBands`], [`GridPop`]) and runs
//! the same arithmetic for draw, stops, `Focusable` placement and paging, so the rect a card is
//! drawn at, the stop it registers and the rect `place` answers are one value.

use plx_machine::machine::{Canon, Cx, Effects, EntryId, FocusKey, GroupId, Host, ScreenEvent};
use plx_machine::present::{PresentEvent, Provenance};

use super::{CardEvent, CardSource, Tile};
use crate::card_row;
use crate::consts::{CARD_H, K_SCROLL, MARGIN_X, SCR_H, SCR_W};
use crate::poster_grid::{self, GridBand, GridBands, GridPop, COLS, STYLE};
use crate::screen::{
    Activate, At, AxisMask, By, DrawFrame, Dir, EdgeRule, ElemKind, GroupKind, GroupSpec, Hover, Placed, Seat, Step,
    Stop,
};
use crate::{Painter, Rect, Spring};

/// Cards past the focused one the source is asked for: two rows.
const LOOK_AHEAD: usize = COLS * 2;

/// The grid's vertical geometry.
#[derive(Clone, Copy, Debug)]
pub struct GridSpec {
    /// Top of the first row's posters in document space.
    pub top: f32,
    /// The content edge a scrolled row snaps to (`poster_grid::snap_row`), and the line rows above
    /// the snapped one are culled against.
    pub edge: f32,
}

pub struct Grid {
    entry: EntryId,
    spec: GridSpec,
    scroll: Spring,
    target: f32,
    bands: GridBands,
    pop: GridPop,
    ahead: usize,
    asked: Option<(usize, usize)>,
}

impl Grid {
    pub fn new(entry: EntryId, spec: GridSpec) -> Self {
        Self {
            entry,
            spec,
            scroll: Spring::at(0.0),
            target: 0.0,
            bands: GridBands::new(),
            pop: GridPop::new(),
            ahead: LOOK_AHEAD,
            asked: None,
        }
    }

    /// Cards beyond the focused one a [`CardEvent::Want`] asks for (default two rows).
    pub fn look_ahead(mut self, n: usize) -> Self {
        self.ahead = n;
        self
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
                self.tick(t.dt(), cx, src, fx);
                self.want(cx, src)
            }
            ScreenEvent::FocusMoved { from, to, by } => {
                let arrived = (to.entry == self.entry).then(|| src.index_of(&to.elem)).flatten();
                let left = from.filter(|k| k.entry == self.entry).and_then(|k| src.index_of(&k.elem));
                // A deliberate move is the only focus change the eye should see travel; a restore
                // or a reconcile is adopted whole by the next tick.
                let deliberate = matches!(by, By::Dir | By::Pointer);
                self.bands.focus(arrived.map(|i| i / COLS), deliberate);
                if let Some(i) = arrived.filter(|_| deliberate) {
                    self.pop.arm(i);
                }
                if arrived.is_some() || left.is_some() {
                    fx.invalidate(Provenance::Input);
                }
                None
            }
            ScreenEvent::PressCommit(_) => self.focused_elem(cx, src).map(|(_, e)| CardEvent::Activate(e)),
            ScreenEvent::PressHold(_) => self
                .focused_elem(cx, src)
                .filter(|&(i, _)| src.holdable(i))
                .map(|(_, e)| CardEvent::Hold(e)),
            _ => None,
        }
    }

    fn focused_elem<H: Host, S: CardSource<H>>(&self, cx: &Cx<'_, H>, src: &S) -> Option<(usize, H::Elem)> {
        super::focused_index(&cx.focus, self.entry, src).map(|i| (i, src.elem(i)))
    }

    fn tick<H: Host, S: CardSource<H>>(&mut self, dt: f32, cx: &Cx<'_, H>, src: &S, fx: &mut Effects<'_, H>) {
        let focus = super::focused_index(&cx.focus, self.entry, src);
        self.target = focus.map_or(0.0, |i| {
            poster_grid::snap_row(self.scroll.pos, i / COLS, src.len(), self.spec.top, self.spec.edge)
        });
        // A focus the reader did not move (a restore, a landing) adopts its band settled.
        self.bands.focus(focus.map(|i| i / COLS), false);
        self.bands.tick(STYLE.k_scroll, dt);
        self.pop.tick(focus, &STYLE, dt);
        self.scroll.step(self.target, K_SCROLL, dt);
        if (self.scroll.pos - self.target).abs() > 0.25 || self.scroll.vel.abs() > 0.5 {
            fx.note(PresentEvent::Motion);
        }
    }

    /// The paging rule: the last row the scroll shows, or the focused card's look-ahead.
    fn want<H: Host, S: CardSource<H>>(&mut self, cx: &Cx<'_, H>, src: &S) -> Option<CardEvent<H::Elem>> {
        let window = ((self.scroll.pos + SCR_H - self.spec.top) / poster_grid::ROW_PITCH).ceil().max(0.0) as usize * COLS;
        let focus = super::focused_index(&cx.focus, self.entry, src).map_or(0, |i| i + 1 + self.ahead);
        super::want(&mut self.asked, src.len(), window.max(focus), src.more()).map(CardEvent::Want)
    }

    /// The pop of card `i` given the engine's focus (`GridPop::scale`'s rule: a focused card the
    /// grid was not told about is FULL, the one that lost focus lets go, the rest are at rest).
    fn pop(&self, i: usize, focus: Option<usize>) -> f32 {
        self.pop.scale(i, focus == Some(i), &STYLE)
    }

    /// The live pop of `elem` (no press).
    pub fn scale_of<H: Host, S: CardSource<H>>(&self, cx: &Cx<'_, H>, src: &S, elem: &H::Elem) -> Option<f32> {
        let i = src.index_of(elem)?;
        Some(self.pop(i, super::focused_index(&cx.focus, self.entry, src)))
    }

    fn cell(&self, i: usize, bands: &[GridBand]) -> Rect {
        poster_grid::cell(i, self.spec.top, self.scroll.pos, bands)
    }

    /// Whether card `i`'s row rests wholly above the content edge: the row over the one a snapped
    /// scroll put on the edge, which would show its last few pixels there.
    fn above_edge(&self, i: usize, bands: &[GridBand]) -> bool {
        self.cell(i, bands).y + CARD_H <= self.spec.edge - (poster_grid::ROW_PITCH - CARD_H) + 0.5
    }

    /// Where `elem` is: the LIVE drawn rect (pop and press folded in) for `At::Drawn`, the settled
    /// one for `At::SpringTarget`; `rest_rect` is the settled focus-scaled rect either way.
    pub fn place<H: Host, S: CardSource<H>>(&self, cx: &Cx<'_, H>, src: &S, elem: &H::Elem, how: At) -> Option<Placed> {
        let i = src.index_of(elem)?;
        let focus = super::focused_index(&cx.focus, self.entry, src);
        let cell = self.cell(i, &self.bands.geometry());
        let s = match how {
            At::Drawn => super::press_scale(self.pop(i, focus), focus == Some(i), cx),
            At::SpringTarget => if focus == Some(i) { STYLE.focus_scale } else { 1.0 },
        };
        Some(Placed { rect: cell.scaled(s), rest_rect: cell.scaled(STYLE.focus_scale), clip: Rect::FULL, index: Some(i as u32) })
    }

    /// Draw the grid into `p` (page alpha already applied) and register its stops: non-focused
    /// cards first, the focused one last; only the cards the scroll can show are touched.
    pub fn draw<H: Host, S: CardSource<H>>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, src: &S) {
        let focus = super::focused_index(&f.focus, self.entry, src);
        let bands = self.bands.geometry();
        let window = poster_grid::visible(src.len(), self.spec.top, self.scroll.pos);
        for i in window.clone() {
            if focus == Some(i) || self.above_edge(i, &bands) {
                continue;
            }
            self.draw_card(f, p, src, i, self.pop(i, focus), false, &bands);
        }
        if let Some(i) = focus.filter(|&i| i < src.len()) {
            self.draw_card(f, p, src, i, super::press_scale(self.pop(i, focus), true, f.cx), true, &bands);
        }
        self.record_stops(f, p, src);
    }

    /// Register the stops of the cards the scroll can show: each is the rect [`draw`](Self::draw) paints.
    pub fn record_stops<H: Host, S: CardSource<H>>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, src: &S) {
        if !f.records_stops() {
            return;
        }
        let focus = super::focused_index(&f.focus, self.entry, src);
        let bands = self.bands.geometry();
        for i in poster_grid::visible(src.len(), self.spec.top, self.scroll.pos) {
            let s = super::press_scale(self.pop(i, focus), focus == Some(i), f.cx);
            let cell = self.cell(i, &bands);
            f.stop(p, Stop {
                key: FocusKey { entry: self.entry, elem: src.elem(i) },
                rect: cell.scaled(s),
                rest_rect: cell.scaled(STYLE.focus_scale),
                clip: Rect::FULL,
                hover: Hover::Focus,
                activate: Activate::Press,
            });
        }
    }

    /// The opener redraw: card `focus` drawn alone, popped and captioned exactly as in-page.
    /// Nothing is painted for an element not in this source.
    pub fn redraw_focused<H: Host, S: CardSource<H>>(
        &self,
        f: &mut DrawFrame<'_, '_, H>,
        p: Painter,
        src: &S,
        focus: Option<FocusKey<H::Elem>>,
    ) {
        let Some(i) = focus.filter(|k| k.entry == self.entry).and_then(|k| src.index_of(&k.elem)) else { return };
        let s = super::press_scale(self.pop(i, Some(i)), true, f.cx);
        self.draw_card(f, p, src, i, s, true, &self.bands.geometry());
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_card<H: Host, S: CardSource<H>>(
        &self,
        f: &DrawFrame<'_, '_, H>,
        p: Painter,
        src: &S,
        i: usize,
        s: f32,
        focused: bool,
        bands: &[GridBand],
    ) {
        let rect = self.cell(i, bands).scaled(s);
        if !card_row::paint_visible(p, rect, s, focused) {
            return;
        }
        if focused {
            let row = i / COLS;
            let open = bands.iter().find(|band| band.row == row).map_or(0.0, |band| band.expansion);
            let label = src.label(i).revealed(card_row::band_reveal(open));
            card_row::draw_focused(p, src.art(i), rect, s, &STYLE, src.progress(i), &label, f.measure);
        } else {
            card_row::draw_tile(p, src.art(i), rect, s, &STYLE, src.progress(i));
        }
        src.overlay(p, i, &Tile { rect, scale: s, radius: STYLE.tile_radius(rect, s), focused }, f.measure);
    }

    /// The `Focusable` neighbour of `key`: down from above a short last row lands on its last card.
    pub fn neighbour<H: Host, S: CardSource<H>>(&self, src: &S, key: FocusKey<H::Elem>, dir: Dir) -> Step<H::Elem> {
        let Some(i) = src.index_of(&key.elem) else { return Step::Edge };
        poster_grid::neighbour(i, src.len(), COLS, dir)
            .map_or(Step::Edge, |j| Step::Move(FocusKey { entry: self.entry, elem: src.elem(j) }))
    }

    /// The first-row card a vertical move into the grid lands on: the column nearest `from`.
    pub fn seat<H: Host, S: CardSource<H>>(&self, src: &S, from: Placed) -> Option<FocusKey<H::Elem>> {
        let n = src.len();
        if n == 0 {
            return None;
        }
        let bands = self.bands.geometry();
        let col = (0..COLS).min_by(|&a, &b| {
            let d = |c: usize| (self.cell(c, &bands).cx() - from.rect.cx()).abs();
            d(a).total_cmp(&d(b))
        })?;
        Some(FocusKey { entry: self.entry, elem: src.elem(col.min(n - 1)) })
    }

    /// The group this grid registers with the focus engine.
    pub fn group_spec(&self, id: GroupId, len: usize) -> GroupSpec {
        GroupSpec {
            id,
            kind: GroupKind::Grid { cols: COLS, holes: &[] },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: Rect::new(MARGIN_X, self.spec.top - self.scroll.pos, SCR_W - 2.0 * MARGIN_X, CARD_H),
            len,
            elem: ElemKind::Card,
        }
    }

    pub fn scroll(&self) -> f32 {
        self.scroll.pos
    }

    /// Restore a saved viewport without gliding to it.
    pub fn restore_scroll(&mut self, scroll: f32) {
        self.scroll.jump(scroll);
        self.target = scroll;
    }

    pub fn write(&self, c: &mut Canon) {
        c.f32(self.scroll.pos).f32(self.scroll.vel).f32(self.target);
        self.bands.write(c);
        self.pop.write(c);
    }
}
