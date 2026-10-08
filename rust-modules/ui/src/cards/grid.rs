//! [`Grid`] — the poster grid with a collapsing caption band under the focused row: six
//! `poster_grid::STYLE` columns by default, any column count / style / left edge through
//! [`GridSpec`] (`poster_grid`'s `_in` functions are its geometry). The Collection page is its first
//! adopter; [`ScrollMode::External`] serves a grid inside a document another owner scrolls.
//!
//! It owns (unless external) the scroll, the caption bands and the focus pop ([`GridBands`], [`GridPop`]) and runs
//! the same arithmetic for draw, stops, `Focusable` placement and paging, so the rect a card is
//! drawn at, the stop it registers and the rect `place` answers are one value.

use plx_machine::machine::{Canon, Cx, Effects, EntryId, FocusKey, GroupId, Host, ScreenEvent};
use plx_machine::present::{PresentEvent, Provenance};

use super::{CardEvent, CardSource, Landed, Seen, Tile};
use crate::card_row;
use crate::consts::{K_SCROLL, MARGIN_X, SCR_H};
use crate::card_row::RowStyle;
use super::pool::{Drawn, ShrinkKey};
use crate::poster_grid::{self, Geom, GridBand, GridBands, GridPop, COLS, STYLE};
use crate::screen::{
    Activate, At, AxisMask, By, DrawFrame, Dir, EdgeRule, ElemKind, GroupKind, GroupSpec, Hover, Placed, Seat, Step,
    Stop,
};
use crate::{Painter, Rect, Spring};

/// Who owns the grid's vertical scroll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollMode {
    /// The grid springs its own scroll toward the row it snaps to (the Collection page).
    Own,
    /// The grid sits in a document whose page scroll another owner drives (the Library's All grid, a later
    /// `Stack`): the grid never steps or snaps a scroll of its own. The owner hands it the page
    /// with [`Grid::set_page`] before every `on` / `draw` / `place` call, reads the scroll the grid
    /// WANTS for its focused row from [`Grid::reveal_target`], and the document shift a content
    /// landing needs from [`Grid::landed_shift`]. `top` is then the first row's top in DOCUMENT
    /// space and `scroll` the owner's document offset.
    External,
}

/// The grid's geometry and scroll policy. [`GridSpec::new`] is the Collection page's six columns
/// of `poster_grid::STYLE` from `MARGIN_X`; [`columns`](Self::columns) and
/// [`external`](Self::external) generalise it.
#[derive(Clone, Copy)]
pub struct GridSpec {
    /// Top of the first row's posters in document space.
    pub top: f32,
    /// The content edge a scrolled row snaps to (`poster_grid::snap_row`), and the line rows above
    /// the snapped one are culled against.
    pub edge: f32,
    /// Whether the grid scrolls home when focus is not in it (a page with a header above the grid
    /// wants that). `false` leaves the scroll where it is for a page whose other focus zones must
    /// not scroll the grid away.
    pub home_when_unfocused: bool,
    /// Columns per row.
    pub cols: usize,
    /// The card treatment and size of every cell (`w`, `h`, `gap`, pop and scroll constants).
    pub style: RowStyle,
    /// The x of column 0's card.
    pub left: f32,
    pub scroll: ScrollMode,
}

impl GridSpec {
    /// A grid that scrolls home when focus leaves it.
    pub const fn new(top: f32, edge: f32) -> Self {
        Self { top, edge, home_when_unfocused: true, cols: COLS, style: STYLE, left: MARGIN_X, scroll: ScrollMode::Own }
    }

    /// `cols` columns of `style` cards starting at x = `left` (a page with an alphabet rail, or
    /// four episode stills across).
    pub const fn columns(self, cols: usize, style: RowStyle, left: f32) -> Self {
        Self { cols, style, left, ..self }
    }

    /// A grid whose page scroll another owner drives ([`ScrollMode::External`]); it also stays
    /// where it is when focus is elsewhere on the page.
    pub const fn external(self) -> Self {
        Self { scroll: ScrollMode::External, home_when_unfocused: false, ..self }
    }

    fn geom(&self) -> Geom { Geom::of(self.cols, self.left, &self.style) }
}

pub struct Grid {
    entry: EntryId,
    spec: GridSpec,
    scroll: Spring,
    target: f32,
    bands: GridBands,
    pop: GridPop,
    /// Which element the running let-go belongs to: the element-keyed pool (`pool.rs`), idle unless the dev switch is on.
    shrink: ShrinkKey,
    seen: Seen,
    landed: Option<Landed>,
    /// External mode: the document shift the last tick's landing needs (see [`Grid::landed_shift`]).
    shift: f32,
    /// The scroll the focused row wants (see [`Grid::reveal_target`]).
    reveal: Option<f32>,
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
            shrink: ShrinkKey::new(),
            seen: Seen::Nothing,
            landed: None,
            shift: 0.0,
            reveal: None,
            ahead: spec.cols * 2,
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
                if let Some(a) = arrived {
                    self.seen = Seen::of(by, a);
                }
                self.bands.focus(arrived.map(|i| i / self.spec.cols), deliberate);
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
        self.landed = None;
        self.shift = 0.0;
        let pooled = super::pool::pop_pool();
        if pooled {
            self.shrink.follow(&mut self.pop, src);
        }
        let prev = self.pop.cell();
        // where the previously focused cell was on screen, before any band moves
        let was = prev.map(|p| self.cell(p, &self.bands.geometry()).y);
        // A focus the reader did not move (a restore, a landing) adopts its band settled.
        self.bands.focus(focus.map(|i| i / self.spec.cols), false);
        if let Some(i) = focus {
            match (self.seen, prev) {
                (Seen::Deliberate(a), _) if a == i => {}
                // no FocusMoved, another index: the same element, moved by a content landing; the
                // scroll shifts by what its row moved so the tile stays where it was on screen
                (Seen::Nothing, Some(p)) if p != i => {
                    self.pop.relocate(i);
                    match self.spec.scroll {
                        ScrollMode::Own => if let Some(was) = was {
                            self.scroll.pos += self.cell(i, &self.bands.geometry()).y - was;
                        },
                        // The owner may have opened the new row's band before this tick, so `was`
                        // would be measured under it: the rows' pitch is the shift, bands aside.
                        ScrollMode::External => {
                            let rows = (i / self.spec.cols) as f32 - (p / self.spec.cols) as f32;
                            self.shift = rows * self.spec.geom().pitch();
                        }
                    }
                    self.landed = Some(Landed { from: p, to: i });
                }
                _ if prev != Some(i) => self.pop.adopt(i, &self.spec.style),
                _ => {}
            }
        }
        self.seen = Seen::Nothing;
        let home = if self.spec.home_when_unfocused { 0.0 } else { self.target };
        let wanted = focus.map(|i| {
            poster_grid::snap_row_in(&self.spec.geom(), self.scroll.pos + self.shift, i / self.spec.cols, src.len(), self.spec.top, self.spec.edge)
        });
        self.reveal = wanted;
        self.bands.tick(self.spec.style.k_scroll, dt);
        self.pop.tick(focus, &self.spec.style, dt);
        if pooled {
            self.shrink.note(&self.pop, src);
        }
        if self.spec.scroll == ScrollMode::External {
            return;
        }
        self.target = wanted.unwrap_or(home);
        self.scroll.step(self.target, K_SCROLL, dt);
        if (self.scroll.pos - self.target).abs() > 0.25 || self.scroll.vel.abs() > 0.5 {
            fx.note(PresentEvent::Motion);
        }
    }

    /// The paging rule: the last row the scroll shows, or the focused card's look-ahead.
    fn want<H: Host, S: CardSource<H>>(&mut self, cx: &Cx<'_, H>, src: &S) -> Option<CardEvent<H::Elem>> {
        let window = ((self.scroll.pos + SCR_H - self.spec.top) / self.spec.geom().pitch()).ceil().max(0.0) as usize * self.spec.cols;
        let focus = super::focused_index(&cx.focus, self.entry, src).map_or(0, |i| i + 1 + self.ahead);
        super::want(&mut self.asked, src.len(), window.max(focus), src.more()).map(CardEvent::Want)
    }

    /// The landing the last tick carried the focused card's pop through (see [`Landed`]).
    pub fn landed(&self) -> Option<Landed> {
        self.landed
    }

    /// External mode: the document shift (pixels, positive = down) the last tick's [`Landed`] needs
    /// so the focused tile stays where it was on screen; the owner applies it to its page scroll.
    /// Zero in [`ScrollMode::Own`], where the grid shifts its own scroll, and when nothing landed.
    pub fn landed_shift(&self) -> f32 {
        self.shift
    }

    /// The scroll the last tick wanted for the focused row (`poster_grid::snap_row_in` against the
    /// current scroll), `None` while focus is not in the grid. An external owner may take it or
    /// apply its own reveal rule; the self-scrolling grid springs to it.
    pub fn reveal_target(&self) -> Option<f32> {
        self.reveal
    }

    /// External mode: hand the grid the page it is drawn on — the first row's top in document
    /// space and the owner's document scroll. A no-op in [`ScrollMode::Own`].
    pub fn set_page(&mut self, top: f32, scroll: f32) {
        if self.spec.scroll == ScrollMode::External {
            self.spec.top = top;
            self.scroll.jump(scroll);
            self.target = scroll;
        }
    }

    /// A grid whose column count, card style or left edge changes at run time (the Library's
    /// episode listing is four stills across where its posters are six; both fit the same band).
    /// Only the geometry changes: the pop and the caption bands are per position and carry over.
    pub fn set_columns(&mut self, cols: usize, style: RowStyle, left: f32) {
        self.spec.cols = cols;
        self.spec.style = style;
        self.spec.left = left;
    }

    /// Open `row`'s caption band at once, settled (and close the rest), for an owner that sizes
    /// its document from the bands before the next tick would (a restored page's first layout).
    /// A no-op when `row` is already the focused row, so it never cuts short a band `on` is
    /// animating.
    pub fn settle_band(&mut self, row: Option<usize>) {
        self.bands.focus(row, false);
    }

    /// Forget the focus pop's cell and any pending move: a fresh content set whose old indexes mean
    /// nothing, so the next tick adopts the focused element whole instead of reading it as the
    /// same element landing at a new index. For an owner that was not ticking while its content
    /// was replaced (a covered page).
    pub fn forget_pop(&mut self) {
        self.pop = GridPop::new();
        self.seen = Seen::Nothing;
    }

    /// The caption bands closed and settled: a fresh content set whose old rows mean nothing.
    pub fn reset_bands(&mut self) {
        self.bands = GridBands::new();
    }

    /// The grid's height for `len` cards with its caption bands as they are now: the span from the
    /// first row's top to the end of the last row's band, the pitch's air included, as `poster_grid::max_scroll_in` counts it (what a `Stack` lays the next section
    /// after, and bounds its scroll by).
    pub fn height(&self, len: usize) -> f32 {
        poster_grid::row_top_in(&self.spec.geom(), len.div_ceil(self.spec.cols), 0.0, &self.bands.geometry())
    }

    /// The cards the scroll can show out of `len`: what `draw` and `record_stops` touch.
    pub fn window(&self, len: usize) -> std::ops::Range<usize> {
        poster_grid::visible_in(&self.spec.geom(), len, self.spec.top, self.scroll.pos)
    }

    /// The caption-band geometry (the owner's page layout reads the document height from it).
    pub fn band_geometry(&self) -> [GridBand; poster_grid::MAX_GRID_BANDS] {
        self.bands.geometry()
    }

    /// The pop of card `i` given the engine's focus (`GridPop::scale`'s rule: a focused card the
    /// grid was not told about is FULL, the one that lost focus lets go, the rest are at rest).
    fn pop<H: Host, S: CardSource<H>>(&self, src: &S, i: usize, focus: Option<usize>) -> f32 {
        // the element-keyed pool: a source change no tick has seen yet is resolved by element
        if super::pool::pop_pool() {
            match self.shrink.drawn(&self.pop, src, i, focus == Some(i)) {
                Some(Drawn::Cell(c)) => return self.pop.scale(c, false, &self.spec.style),
                Some(Drawn::Rest) => return 1.0,
                None => {}
            }
        }
        self.pop.scale(i, focus == Some(i), &self.spec.style)
    }

    /// The live pop of `elem` (no press).
    pub fn scale_of<H: Host, S: CardSource<H>>(&self, cx: &Cx<'_, H>, src: &S, elem: &H::Elem) -> Option<f32> {
        let i = src.index_of(elem)?;
        Some(self.pop(src, i, super::focused_index(&cx.focus, self.entry, src)))
    }

    fn cell(&self, i: usize, bands: &[GridBand]) -> Rect {
        poster_grid::cell_in(&self.spec.geom(), i, self.spec.top, self.scroll.pos, bands)
    }

    /// Whether card `i`'s row rests wholly above the content edge: the row over the one a snapped
    /// scroll put on the edge, which would show its last few pixels there.
    fn above_edge(&self, i: usize, bands: &[GridBand]) -> bool {
        self.cell(i, bands).y + self.spec.style.h <= self.spec.edge - (self.spec.geom().pitch() - self.spec.style.h) + 0.5
    }

    /// Where `elem` is: the LIVE drawn rect (pop and press folded in) for `At::Drawn`, the settled
    /// one for `At::SpringTarget`; `rest_rect` is the settled focus-scaled rect either way.
    pub fn place<H: Host, S: CardSource<H>>(&self, cx: &Cx<'_, H>, src: &S, elem: &H::Elem, how: At) -> Option<Placed> {
        let i = src.index_of(elem)?;
        let focus = super::focused_index(&cx.focus, self.entry, src);
        let cell = self.cell(i, &self.bands.geometry());
        let s = match how {
            At::Drawn => super::press_scale(self.pop(src, i, focus), focus == Some(i), cx),
            At::SpringTarget => if focus == Some(i) { self.spec.style.focus_scale } else { 1.0 },
        };
        Some(Placed { rect: cell.scaled(s), rest_rect: cell.scaled(self.spec.style.focus_scale), clip: Rect::FULL, index: Some(i as u32) })
    }

    /// Draw the grid into `p` (page alpha already applied) and register its stops: non-focused
    /// cards first, the focused one last; only the cards the scroll can show are touched.
    pub fn draw<H: Host, S: CardSource<H>>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, src: &S) {
        let focus = super::focused_index(&f.focus, self.entry, src);
        let bands = self.bands.geometry();
        for i in self.window(src.len()) {
            if focus == Some(i) || self.above_edge(i, &bands) {
                continue;
            }
            self.draw_card(f, p, src, i, self.pop(src, i, focus), false, &bands);
        }
        if let Some(i) = focus.filter(|&i| i < src.len()) {
            self.draw_card(f, p, src, i, super::press_scale(self.pop(src, i, focus), true, f.cx), true, &bands);
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
        for i in self.window(src.len()) {
            let s = super::press_scale(self.pop(src, i, focus), focus == Some(i), f.cx);
            let cell = super::to_local(p, self.cell(i, &bands));
            f.stop(p, Stop {
                key: FocusKey { entry: self.entry, elem: src.elem(i) },
                rect: cell.scaled(s),
                rest_rect: cell.scaled(self.spec.style.focus_scale),
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
        let s = super::press_scale(self.pop(src, i, Some(i)), true, f.cx);
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
        if !src.loaded(i) {
            return;
        }
        let rect = super::to_local(p, self.cell(i, bands)).scaled(s);
        if !card_row::paint_visible(p, rect, s, focused) {
            return;
        }
        if focused {
            let row = i / self.spec.cols;
            let open = bands.iter().find(|band| band.row == row).map_or(0.0, |band| band.expansion);
            let label = src.label(i).revealed(card_row::band_reveal(open));
            card_row::draw_focused(p, src.art(i), rect, s, &self.spec.style, src.progress(i), &label, f.measure);
        } else {
            card_row::draw_tile(p, src.art(i), rect, s, &self.spec.style, src.progress(i));
        }
        src.overlay(p, i, &Tile { rect, scale: s, radius: self.spec.style.tile_radius(rect, s), focused }, f.measure);
    }

    /// The `Focusable` neighbour of `key`: down from above a short last row lands on its last card.
    pub fn neighbour<H: Host, S: CardSource<H>>(&self, src: &S, key: FocusKey<H::Elem>, dir: Dir) -> Step<H::Elem> {
        let Some(i) = src.index_of(&key.elem) else { return Step::Edge };
        poster_grid::neighbour(i, src.len(), self.spec.cols, dir)
            .map_or(Step::Edge, |j| Step::Move(FocusKey { entry: self.entry, elem: src.elem(j) }))
    }

    /// The first-row card a vertical move into the grid lands on: the column nearest `from`.
    pub fn seat<H: Host, S: CardSource<H>>(&self, src: &S, from: Placed) -> Option<FocusKey<H::Elem>> {
        let n = src.len();
        if n == 0 {
            return None;
        }
        let bands = self.bands.geometry();
        let col = (0..self.spec.cols).min_by(|&a, &b| {
            let d = |c: usize| (self.cell(c, &bands).cx() - from.rect.cx()).abs();
            d(a).total_cmp(&d(b))
        })?;
        Some(FocusKey { entry: self.entry, elem: src.elem(col.min(n - 1)) })
    }

    /// The group this grid registers with the focus engine.
    pub fn group_spec(&self, id: GroupId, len: usize) -> GroupSpec {
        GroupSpec {
            id,
            kind: GroupKind::Grid { cols: self.spec.cols, holes: &[] },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: Rect::new(
                self.spec.left,
                self.spec.top - self.scroll.pos,
                self.spec.cols as f32 * (self.spec.style.w + self.spec.style.gap) - self.spec.style.gap,
                self.spec.style.h,
            ),
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

    /// The grid's canonical state. In [`ScrollMode::External`] the scroll fields are the owner's
    /// (it writes its own), so they are left out; an owner that orders its canon differently
    /// writes [`write_bands`](Self::write_bands) and [`write_pop`](Self::write_pop) itself.
    pub fn write(&self, c: &mut Canon) {
        if self.spec.scroll == ScrollMode::Own {
            c.f32(self.scroll.pos).f32(self.scroll.vel).f32(self.target);
        }
        self.write_bands(c);
        self.write_pop(c);
    }

    /// The caption bands' canonical state.
    pub fn write_bands(&self, c: &mut Canon) {
        self.bands.write(c);
    }

    /// The focus pop's and let-go's canonical state.
    pub fn write_pop(&self, c: &mut Canon) {
        self.pop.write(c);
    }
}
