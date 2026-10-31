//! Uniform portrait-grid geometry and caption motion shared by the grids of the application.
//!
//! [`GridBands`] is the collapsing caption band every grid row uses (lifted from the Library's
//! All grid, which still owns its own rail-aware layout and reads the bands through
//! [`GridBand`]): only the focused row reserves `card_row::UNDER_LABEL_H` under its posters, and
//! rows below it move down by what that row grows. The default-geometry free functions (test-only now that
//! `cards::Grid` computes its own) are the six-column grid
//! of a page without an alphabet rail (the Collection page) — row pitch, band-aware cell
//! placement, culling window and reveal arithmetic, so draw, focus placement, scrolling and
//! paging all describe the same cells. [`neighbour`], [`settled`] and [`growth_before`] are
//! column-count agnostic, and the Library's rail-aware layout uses them too.

use crate::card_row::{self, RowStyle, LABEL_BAND_COLLAPSED};
#[cfg(test)]
use crate::card_row::UNDER_LABEL_H;
#[cfg(test)]
use crate::consts::CARD_H;
use crate::consts::{CARD_W, MARGIN_X, MARGIN_Y, SCR_H, SCR_W, UNDER_LABEL_AIR};
use crate::{Rect, Spring};

pub const COLS: usize = 6;
pub const GAP: f32 = (SCR_W - 2.0 * MARGIN_X - COLS as f32 * CARD_W) / (COLS as f32 - 1.0);
pub const STYLE: RowStyle = RowStyle { gap: GAP, ..RowStyle::HOME };
/// A COLLAPSED row's pitch; a row whose band is open adds its share of `card_row::BAND_OPEN`.
#[cfg(test)]
pub const ROW_PITCH: f32 = CARD_H + LABEL_BAND_COLLAPSED + UNDER_LABEL_AIR;

/// Only the focused and closing bands are represented, independent of catalog size.
/// Sixteen spans more row moves than a complete spring at normal remote repeat speed.
pub const MAX_GRID_BANDS: usize = 16;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridBand {
    pub row: usize,
    pub expansion: f32,
}
impl GridBand {
    pub const CLOSED: Self = Self { row: usize::MAX, expansion: 0.0 };
}

/// Sparse caption motion: the focused row and rows still closing behind it. A catalog with
/// ten thousand rows costs exactly as much as a short one; no per-frame allocation or row walk.
pub struct GridBands {
    focus: Option<usize>,
    slots: [Slot; MAX_GRID_BANDS],
    /// What the focused caption band shows (CONTRACT FREEZE: P3 sizes the band from it); `None`
    /// keeps the legacy two-line band, as the grid that owns these bands does. A pref input, not
    /// motion: it is not in [`write`](Self::write).
    caption_policy: Option<crate::caption_policy::CaptionPolicy>,
}

/// One band: the row it opens under, the card of that row whose caption it carries (known once the
/// row has held focus on a tick, so a band that closes behind a focus move still knows whose
/// caption leaves with it) and the spring that opens and closes it.
#[derive(Clone, Copy)]
struct Slot {
    row: Option<usize>,
    card: Option<usize>,
    spring: Spring,
}

impl Slot {
    const EMPTY: Self = Self { row: None, card: None, spring: Spring::at(0.0) };
}

impl GridBands {
    pub fn new() -> Self {
        Self {
            focus: None,
            slots: [Slot::EMPTY; MAX_GRID_BANDS],
            caption_policy: Some(crate::caption_policy::CaptionPolicy::DEFAULT),
        }
    }

    pub fn set_caption_policy(&mut self, policy: Option<crate::caption_policy::CaptionPolicy>) {
        self.caption_policy = policy;
    }

    pub fn caption_policy(&self) -> Option<crate::caption_policy::CaptionPolicy> {
        self.caption_policy
    }

    /// The focused band's height under this grid's caption policy (0 / 80 / 112), what
    /// [`growth_before_for`] and the other `_for` geometry take.
    pub fn band_h(&self) -> f32 {
        self.caption_policy.as_ref().map_or(card_row::LEGACY_BAND_H, crate::caption_policy::band_h)
    }

    /// The row whose band is open (or opening).
    pub fn focused(&self) -> Option<usize> {
        self.focus
    }

    pub fn focus(&mut self, row: Option<usize>, animate: bool) {
        if self.focus == row { return; }
        self.focus = row;
        if !animate {
            self.slots = [Slot::EMPTY; MAX_GRID_BANDS];
            if let Some(row) = row { self.slots[0] = Slot { row: Some(row), card: None, spring: Spring::at(1.0) }; }
            return;
        }
        let Some(row) = row else { return };
        if self.slots.iter().any(|s| s.row == Some(row)) { return; }
        // A stream faster than remote repeat can fill the bounded pool. Retire the smallest
        // closing band, never the focused row; normal input leaves several spare slots.
        let at = self.slots.iter().position(|s| s.row.is_none()).unwrap_or_else(|| {
            self.slots.iter().enumerate().min_by(|(_, a), (_, b)| a.spring.pos.total_cmp(&b.spring.pos))
                .map_or(0, |(at, _)| at)
        });
        self.slots[at] = Slot { row: Some(row), card: None, spring: Spring::at(0.0) };
    }

    /// Close every band at once and name no focused row: what was opening and what was closing
    /// behind a move are gone, with no spring left to step. [`focus`](Self::focus)`(None, false)`
    /// cannot say it when the focus has already left (`None`): it returns early, and the band a
    /// move off the grid left closing goes on closing.
    pub fn shut(&mut self) {
        self.focus = None;
        self.slots = [Slot::EMPTY; MAX_GRID_BANDS];
    }

    /// Note which card of the focused row `row` the focus is on, so the band, once it closes behind
    /// a move, still knows whose caption it carries ([`closing`](Self::closing)). A card outside the
    /// focused row is not noted.
    pub fn note_card(&mut self, row: usize, card: usize) {
        if self.focus != Some(row) { return; }
        if let Some(slot) = self.slots.iter_mut().find(|s| s.row == Some(row)) { slot.card = Some(card); }
    }

    /// The bands closing behind a focus move, as `(row, card, expansion)`: each is the caption of
    /// `card` (the one the row held focus on) drawn by its band's own expansion
    /// ([`card_row::line_reveal_for`] decides which of its lines the room still holds). The focused row's band is
    /// not among them; a band with no card noted (a row that never held a tick) or fully shut is not.
    pub fn closing(&self) -> impl Iterator<Item = (usize, usize, f32)> + '_ {
        self.slots.iter().filter_map(|s| {
            let (row, card) = (s.row?, s.card?);
            (self.focus != Some(row) && s.spring.pos > 0.0).then_some((row, card, s.spring.pos))
        })
    }

    pub fn tick(&mut self, k: f32, dt: f32) {
        for slot in &mut self.slots {
            let Some(r) = slot.row else { continue };
            let target = f32::from(self.focus == Some(r));
            slot.spring.step(target, k, dt);
            if target == 0.0 && slot.spring.pos.abs() < 1.0e-5 && slot.spring.vel.abs() < 1.0e-4 { *slot = Slot::EMPTY; }
        }
    }

    pub fn geometry(&self) -> [GridBand; MAX_GRID_BANDS] {
        self.slots.map(|s| s.row.map_or(GridBand::CLOSED,
            |row| GridBand { row, expansion: s.spring.pos }))
    }

    pub fn write(&self, c: &mut plx_machine::machine::Canon) {
        c.option(self.focus, |c, row| { c.u32(row as u32); });
        c.seq(self.slots.iter().filter(|s| s.row.is_some()).count());
        for s in &self.slots {
            if let Some(row) = s.row {
                c.u32(row as u32).f32(s.spring.pos).f32(s.spring.vel);
                c.option(s.card, |c, card| { c.u32(card as u32); });
            }
        }
    }
}

/// **The focus pop of a poster grid** — the spring that carries the focused cell toward
/// `RowStyle::focus_scale` and the springs that let the cells that lost focus back down, the
/// grid's counterpart of a shelf's `RowMotion` per-tile springs. The Library's All grid and the
/// Collection page both read their cells' scale from here, so neither draws `focus_scale` as a
/// step.
///
/// It only starts from REST when a deliberate MOVE arms it ([`arm`](Self::arm), from the
/// `FocusMoved` arm for `By::Dir` / `By::Pointer`); every other way a cell becomes focused — a
/// restore, a reconcile, a command seating the cursor — is adopted at full scale by
/// [`tick`](Self::tick), because a page coming back from a Detail push must land exactly as it
/// was left. Inputs and the `FocusMoved` they deliver run BEFORE the screen's `Tick`, so a D-pad
/// step has already armed the pop by the time `tick` steps it on from 1.0.
///
/// **Every card that lost focus lets go on a spring of its own** ([`LetGo`]), the way a shelf's
/// cell does (`CardRow` keeps one spring per cell, and lands each by [`Spring::land`]). A held key
/// presses faster than a let-go lasts, so several are in flight at once and a press adds one to
/// them; none is replaced. A shelf's springs are an array indexed by cell because a shelf has at
/// most `MAX_ROW_ITEMS` of them; a grid has thousands of cells, so its let-gos are SPARSE: only
/// the cards still off rest hold one (an entry leaves the moment it lands), a settled grid holds
/// none and pays for none, and the count is whatever the presses of the last ~30 frames left in
/// flight — no cap, no eviction.
///
/// **A card is its element, not its cell.** The pop knows which [`Card`] it is on and a let-go is
/// born with the card it was released from, so no let-go is ever without its element: a press that
/// finds the cards shifted since the last tick (a landing and a key in one frame) hands the card it
/// left a let-go that follows that card, and picks up the spring of the card it names, by element,
/// whatever index the shift gave either. The cells a let-go carries are where the last tick saw its
/// element (`cards::pool::follow_let_gos` keeps them so).
///
/// A hold menu changes nothing here: the menu is its own input scope, the page keeps its focus,
/// and the settled pop is what the opener redraw reads ([`scale`](Self::scale)).
pub struct GridPop {
    pop: Pop,
    /// The cards letting go, one entry each and never two on a cell nor on the popped one, oldest
    /// release first. An entry is removed by the shared landing rule
    /// ([`Spring::land`](crate::Spring::land)), so a settled grid pays for no letting-go card.
    let_gos: Vec<LetGo>,
}

/// A card of a grid: the cell it stands in and which element it is (`cards::pool::key`, a number
/// because a section is not generic over the host's element).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Card {
    pub cell: usize,
    pub key: u64,
}

/// The focused card and the spring that grows it.
struct Pop {
    /// `None`: the pop belongs to no card.
    card: Option<Card>,
    spring: Spring,
}

impl Pop {
    const NONE: Self = Self { card: None, spring: Spring::at(1.0) };
}

/// One card letting go: which card it is (so a content landing carries the let-go with it,
/// `cards/pool.rs`) and the spring that brings it back to 1.0.
struct LetGo {
    card: Card,
    spring: Spring,
}

impl GridPop {
    pub const fn new() -> Self {
        Self { pop: Pop::NONE, let_gos: Vec::new() }
    }

    /// Forget the pop and every let-go (a fresh content set whose cells mean nothing), keeping the
    /// room the let-go list has grown to: a grid that wakes from dormancy or is handed new content
    /// does not allocate again for the presses it already had room for.
    pub fn reset(&mut self) {
        self.pop = Pop::NONE;
        self.let_gos.clear();
    }

    /// Hand the pop's card to the let-gos: it was released, so it goes on from where its spring is.
    fn release(&mut self) {
        if let Some(card) = self.pop.card {
            // a card is on one spring: the pop's is the live one
            self.take(card.key);
            self.let_gos.push(LetGo { card, spring: self.pop.spring });
        }
    }

    /// Take the let-go of the element `key` out of the running, if it has one: its spring.
    fn take(&mut self, key: u64) -> Option<Spring> {
        let at = self.let_gos.iter().position(|l| l.card.key == key)?;
        Some(self.let_gos.remove(at).spring)
    }

    /// Arm the pop: `card` grows from REST, or from where its let-go has got to when it is a card
    /// still letting go (focus came back to it before it landed: one spring goes on, as a shelf's
    /// cell does, and nothing jumps). Whatever held the pop before starts its own let-go.
    pub fn arm(&mut self, card: Card) {
        self.release();
        let spring = self.take(card.key).unwrap_or(Spring::at(1.0));
        self.pop = Pop { card: Some(card), spring };
    }

    /// Whether the card in `cell` is letting go.
    #[cfg(test)]
    pub(crate) fn letting_go(&self, cell: usize) -> bool {
        self.let_gos.iter().any(|l| l.card.cell == cell)
    }

    /// Every let-go follows its element: `locate(cell, key)` says where the element is now,
    /// `Some(cell)` when it did not move, `None` when it left (its let-go goes with it). A locate
    /// reads the source and the let-go it is asked about, nothing of the others, so the moves are
    /// made in place: two let-gos that swap cells (a reorder) are not confused with each other.
    pub(crate) fn follow_let_gos(&mut self, mut locate: impl FnMut(usize, u64) -> Option<usize>) {
        self.let_gos.retain_mut(|l| match locate(l.card.cell, l.card.key) {
            Some(to) => {
                l.card.cell = to;
                true
            }
            None => false,
        });
    }

    /// Every card letting go, oldest release first: where the last tick saw each and which element
    /// it is, what a draw reads to put each card at its own let-go's scale.
    pub(crate) fn let_gos(&self) -> impl Iterator<Item = Card> + '_ {
        self.let_gos.iter().map(|l| l.card)
    }

    /// The cell the pop currently belongs to.
    pub fn cell(&self) -> Option<usize> {
        self.pop.card.map(|c| c.cell)
    }

    /// The focused element moved to cell `to` because the CONTENT changed (a landing): the pop
    /// spring, velocity and all, goes with it. (The card is the same element, so it has no let-go
    /// to drop: a let-go and the pop never share a card.)
    pub fn relocate(&mut self, to: usize) {
        if let Some(card) = &mut self.pop.card {
            card.cell = to;
        }
    }

    /// The focused `card` took over a cell whose card left lifted (a landing's ghost carries that
    /// lift down while it fades): its pop starts from REST and grows with the landing, so the two
    /// never stand lifted side by side. Nothing lets go — the card that held the pop is the ghost,
    /// which eases itself.
    pub fn rise(&mut self, card: Card) {
        self.pop = Pop { card: Some(card), spring: Spring::at(1.0) };
        self.take(card.key);
    }

    /// The focused card left as a ghost and focus has not been reconciled yet: the pop belongs to
    /// no card and nothing lets go (the card now in that cell was never lifted; the ghost eases
    /// itself).
    pub fn vacate(&mut self) {
        self.pop = Pop::NONE;
    }

    /// `card` is focused with no deliberate move (a restore, a reconcile): FULL scale at once
    /// and the card it left does not let go, the cell it left is simply at rest. (The cards still
    /// letting go from earlier moves go on to rest on their own springs.)
    pub fn adopt(&mut self, card: Card, style: &RowStyle) {
        self.take(card.key);
        self.pop = Pop { card: Some(card), spring: Spring::at(style.focus_scale) };
    }

    /// Advance the pop one tick; `focused` is the focused card, if focus is in the grid.
    pub fn tick(&mut self, focused: Option<Card>, style: &RowStyle, dt: f32) {
        if focused.map(|c| c.cell) != self.cell() {
            self.release();
            match focused {
                Some(card) => self.adopt(card, style),
                None => self.pop = Pop::NONE,
            }
        } else {
            // the card standing in the popped cell is the popped one (a refresh may have put another there)
            self.pop.card = focused;
        }
        self.pop.spring.step(if self.pop.card.is_some() { style.focus_scale } else { 1.0 }, style.k_scale, dt);
        // Each let-go ends by the landing rule a shelf's released tile ends by (`CardRow::update`):
        // within `LAND_REL` of rest AND no longer travelling, put exactly at rest.
        self.let_gos.retain_mut(|l| {
            l.spring.step(1.0, style.k_scale, dt);
            !l.spring.land(1.0, dt)
        });
    }

    /// The focused cell's LIVE scale: the pop spring's position while the pop belongs to this
    /// cell, and FULL scale when it does not — a focus this grid has not been told about yet (a
    /// restore, a reconcile, a seat that landed after this frame's tick) lands finished rather
    /// than animating, so drawing it at rest would be a one-frame collapse.
    pub fn pop_scale(&self, index: usize, style: &RowStyle) -> f32 {
        if self.cell() == Some(index) { self.pop.spring.pos } else { style.focus_scale }
    }

    /// **The one scale a cell draws at — for its RECT and for its TREATMENT, always together**
    /// (`card_row::draw_tile`/`draw_focused` derive the radius and shadow ramp from it): the live
    /// pop while `focused`, the live let-go for a cell that lost focus and has not landed, rest for
    /// every other. The press is the caller's to multiply in.
    pub fn scale(&self, index: usize, focused: bool, style: &RowStyle) -> f32 {
        if focused { return self.pop_scale(index, style) }
        self.let_gos.iter().find(|l| l.card.cell == index).map_or(1.0, |l| l.spring.pos)
    }

    /// Canonical animation state: a spring mid-flight decides the next frames even when two grids
    /// draw the same rects. The pop (its cell and spring), then the let-gos in flight as a counted
    /// sequence, oldest release first, each a cell and a spring — so the canon is self-delimiting
    /// and a grid with none writes the empty sequence. A card's element is the source's, not the
    /// grid's, and is not written.
    pub fn write(&self, c: &mut plx_machine::machine::Canon) {
        c.option(self.cell(), |c, i| { c.u32(i as u32); });
        c.f32(self.pop.spring.pos).f32(self.pop.spring.vel);
        c.seq(self.let_gos.len());
        for l in &self.let_gos {
            c.u32(l.card.cell as u32).f32(l.spring.pos).f32(l.spring.vel);
        }
    }
}

impl Default for GridPop {
    fn default() -> Self { Self::new() }
}

/// The SETTLED bands of a grid whose focus is `row` — every reveal and scroll bound is computed
/// from where the bands are going, never from their live springs (`card_row::settled_top`'s
/// argument).
pub fn settled(row: Option<usize>) -> [GridBand; 1] {
    [row.map_or(GridBand::CLOSED, |row| GridBand { row, expansion: 1.0 })]
}

/// A grid's column geometry: the free functions below are this with [`Geom::DEFAULT`] (the six
/// columns of a page without an alphabet rail); a page with another column count, card size or
/// left edge (the Library's rail-aware grid) passes its own to the `_in` variants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geom {
    pub cols: usize,
    /// The x of column 0's card.
    pub left: f32,
    pub w: f32,
    pub h: f32,
    pub gap: f32,
    /// The grid shows a sort value under every poster ([`card_row::SortValue`]): a collapsed row
    /// keeps [`card_row::SORT_VALUE_ROW_GAP`] under its posters instead of 40 px.
    pub value_line: bool,
}

impl Geom {
    #[cfg(test)]
    pub const DEFAULT: Self = Self { cols: COLS, left: MARGIN_X, w: CARD_W, h: CARD_H, gap: GAP, value_line: false };

    pub const fn of(cols: usize, left: f32, style: &RowStyle) -> Self {
        Self { cols, left, w: style.w, h: style.h, gap: style.gap, value_line: false }
    }

    /// A COLLAPSED row's pitch.
    pub fn pitch(&self) -> f32 {
        self.h + if self.value_line { card_row::SORT_VALUE_ROW_GAP }
            else { LABEL_BAND_COLLAPSED + UNDER_LABEL_AIR }
    }

    /// How far under a card's unscaled bottom the focused row's caption reaches for a band of
    /// `band_h`: what the page must keep on screen, and what paging counts back from.
    fn under_reach(&self, band_h: f32) -> f32 {
        if self.value_line { card_row::SORT_VALUE_REACH } else { shown_band(band_h) }
    }
}

#[cfg(test)]
pub fn rows(len: usize) -> usize { len.div_ceil(COLS) }

/// How far every open band above row `row` pushes it down.
pub fn growth_before(row: usize, bands: &[GridBand]) -> f32 {
    growth_before_for(card_row::LEGACY_BAND_H, row, bands)
}

/// [`growth_before`] for bands of `band_h` ([`crate::caption_policy::band_h`]): 40 px per open
/// one-line band, 72 px per two-line one, nothing when the policy shows nothing.
pub fn growth_before_for(band_h: f32, row: usize, bands: &[GridBand]) -> f32 {
    bands.iter().filter(|band| band.row < row)
        .map(|band| card_row::under_band_for(band_h, band.expansion) - LABEL_BAND_COLLAPSED).sum()
}

/// Row `row`'s top in document space (before scroll), with every open band above it counted.
#[cfg(test)]
pub fn row_top(row: usize, top: f32, bands: &[GridBand]) -> f32 { row_top_in(&Geom::DEFAULT, row, top, bands) }

pub fn row_top_in(g: &Geom, row: usize, top: f32, bands: &[GridBand]) -> f32 {
    row_top_in_for(card_row::LEGACY_BAND_H, g, row, top, bands)
}

/// [`row_top_in`] for bands of `band_h`.
pub fn row_top_in_for(band_h: f32, g: &Geom, row: usize, top: f32, bands: &[GridBand]) -> f32 {
    top + row as f32 * g.pitch() + growth_before_for(band_h, row, bands)
}

#[cfg(test)]
pub fn cell(index: usize, top: f32, scroll: f32, bands: &[GridBand]) -> Rect {
    cell_in(&Geom::DEFAULT, index, top, scroll, bands)
}

pub fn cell_in(g: &Geom, index: usize, top: f32, scroll: f32, bands: &[GridBand]) -> Rect {
    cell_in_for(card_row::LEGACY_BAND_H, g, index, top, scroll, bands)
}

/// [`cell_in`] for bands of `band_h`.
pub fn cell_in_for(band_h: f32, g: &Geom, index: usize, top: f32, scroll: f32, bands: &[GridBand]) -> Rect {
    let col = index % g.cols;
    Rect::new(g.left + col as f32 * (g.w + g.gap),
        row_top_in_for(band_h, g, index / g.cols, top, bands) - scroll, g.w, g.h)
}

#[cfg(test)]
pub fn max_scroll(len: usize, top: f32, bands: &[GridBand]) -> f32 {
    max_scroll_in(&Geom::DEFAULT, len, top, bands)
}

#[cfg(test)]
pub fn max_scroll_in(g: &Geom, len: usize, top: f32, bands: &[GridBand]) -> f32 {
    (row_top_in(g, len.div_ceil(g.cols), top, bands) - (SCR_H - MARGIN_Y)).max(0.0)
}

/// The scroll that shows row `row` focused — its posters AND its open caption band above the
/// bottom margin — computed from the settled bands of that focus.
#[cfg(test)]
pub fn reveal_row(current: f32, row: usize, len: usize, top: f32) -> f32 {
    let g = Geom::DEFAULT;
    let bands = settled(Some(row));
    let row_top = row_top_in(&g, row, top, &bands);
    card_row::reveal(current,
        row_top + CARD_H + UNDER_LABEL_H - (SCR_H - MARGIN_Y),
        row_top - MARGIN_Y,
        max_scroll(len, top, &bands))
}

/// The scroll that shows row `row` focused on a page whose rows SNAP to a content edge (the
/// Collection page, `Collections.dc.html` C2): the minimal reveal of the row's posters and open
/// caption band, rounded UP to the scroll that puts some row's top exactly at `edge`, so the page
/// never rests with a row cut by the edge. Row 0 reveals the document's head (scroll 0), as the
/// Library's `row_reveal(0)` does; no rounding passes `row` itself, so the focused row always shows.
#[cfg(test)]
pub fn snap_row(current: f32, row: usize, len: usize, top: f32, edge: f32) -> f32 {
    snap_row_in(&Geom::DEFAULT, current, row, len, top, edge)
}

pub fn snap_row_in(g: &Geom, current: f32, row: usize, len: usize, top: f32, edge: f32) -> f32 {
    snap_row_in_for(card_row::LEGACY_BAND_H, g, current, row, len, top, edge)
}

/// [`snap_row_in`] for bands of `band_h`: the focused row shows whole with its open band.
pub fn snap_row_in_for(band_h: f32, g: &Geom, current: f32, row: usize, len: usize, top: f32, edge: f32) -> f32 {
    if row == 0 || len == 0 { return 0.0; }
    let bands = settled(Some(row));
    let focused_top = row_top_in_for(band_h, g, row, top, &bands);
    let min = card_row::reveal(current, focused_top + g.h + g.under_reach(band_h) - (SCR_H - MARGIN_Y),
        focused_top - edge, f32::INFINITY);
    if min <= 0.0 { return 0.0; }
    (0..=row).map(|k| row_top_in_for(band_h, g, k, top, &bands) - edge).find(|&at| at >= min - 0.5)
        .unwrap_or(focused_top - edge)
}

/// **The rest rule: a focused row rests where the first row does.** The scroll that puts row `row`'s
/// poster top at the first row's resting y: `row` pitches down, plus `growth_before`, the growth of
/// every open band above it (none for the row the focus itself opened). Row 0 rests at the head,
/// scroll 0. The document's end holds it at `max`: the last rows of a list cannot scroll past the
/// end, so they rest lower. A row never rests above the line.
///
/// The Library's `Layout::row_reveal` (its grid, and the Collections page that is the same layout)
/// and a [`Grid`](crate::cards::Grid) built with `GridSpec::resting` (the Watchlist) both land here,
/// so the line (244 on those pages) is one rule, not two.
pub fn rest_scroll(row: usize, pitch: f32, growth_before: f32, max: f32) -> f32 {
    if row == 0 { return 0.0; }
    (row as f32 * pitch + growth_before).min(max).max(0.0)
}

/// The label block a focused row shows under its posters for a band of `band_h`: the band less the
/// air [`UNDER_LABEL_AIR`] below it ([`UNDER_LABEL_H`] for the legacy band), never negative.
fn shown_band(band_h: f32) -> f32 {
    (card_row::open_gap(band_h) - UNDER_LABEL_AIR).max(0.0)
}

/// The D-pad neighbour of card `index` in a `len`-card grid of `cols` columns. Down from above a
/// short last row lands on its last card rather than stopping where no card sits directly below.
pub fn neighbour(index: usize, len: usize, cols: usize, dir: crate::screen::Dir) -> Option<usize> {
    use crate::screen::Dir;
    let (row, col) = (index / cols, index % cols);
    match dir {
        Dir::Left => col.checked_sub(1).map(|c| row * cols + c),
        Dir::Right => (col + 1 < cols).then_some(index + 1),
        Dir::Up => row.checked_sub(1).map(|r| r * cols + col),
        Dir::Down => ((row + 1) * cols < len).then(|| ((row + 1) * cols + col).min(len - 1)),
    }
    .filter(|&i| i < len)
}

/// The members that can touch the screen at `scroll` — one row wider on each side than the
/// collapsed pitch alone says, so an open band shifting rows down never culls a visible one.
#[cfg(test)]
pub fn visible(len: usize, top: f32, scroll: f32) -> std::ops::Range<usize> {
    visible_in(&Geom::DEFAULT, len, top, scroll)
}

pub fn visible_in(g: &Geom, len: usize, top: f32, scroll: f32) -> std::ops::Range<usize> {
    visible_in_for(card_row::LEGACY_BAND_H, g, len, top, scroll)
}

/// [`visible_in`] for bands of `band_h`.
pub fn visible_in_for(band_h: f32, g: &Geom, len: usize, top: f32, scroll: f32) -> std::ops::Range<usize> {
    if len == 0 { return 0..0; }
    let pitch = g.pitch();
    let first = ((scroll - top - g.h - g.under_reach(band_h)) / pitch).floor().max(0.0) as usize;
    let last = ((scroll + SCR_H - top) / pitch).ceil().max(0.0) as usize + 1;
    first.saturating_mul(g.cols)..last.saturating_mul(g.cols).min(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_portraits_keep_existing_card_size_and_share_one_pitch() {
        let a = cell(0, 400.0, 0.0, &[]);
        let b = cell(5, 400.0, 0.0, &[]);
        let next = cell(6, 400.0, 0.0, &[]);
        assert_eq!((a.w, a.h), (CARD_W, CARD_H));
        assert!(b.x + b.w <= SCR_W - MARGIN_X + 0.01);
        assert_eq!(next.x, a.x);
        assert!((next.y - a.y - ROW_PITCH).abs() < 0.01);
    }

    /// C2: a row focused below the first scrolls the page so a row's top rests exactly on the
    /// content edge — the row above the focused one, when both fit — and never cuts a row; row 0
    /// shows the head again.
    #[test]
    fn a_snapped_page_rests_a_row_on_the_content_edge() {
        let (top, edge, len) = (520.0, 96.0, 18);
        let at = snap_row(0.0, 1, len, top, edge);
        assert_eq!(cell(0, top, at, &settled(Some(1))).y, edge, "row 0 rests on the edge");
        let focused = cell(6, top, at, &settled(Some(1)));
        assert!(focused.y + CARD_H + UNDER_LABEL_H <= SCR_H - MARGIN_Y, "and the focused row shows whole");
        let at2 = snap_row(at, 2, len, top, edge);
        assert_eq!(cell(6, top, at2, &settled(Some(2))).y, edge, "down again: the next row takes the edge");
        assert_eq!(snap_row(at2, 1, len, top, edge), at2, "up to a row already on screen: no move");
        assert_eq!(snap_row(at2, 0, len, top, edge), 0.0, "row 0 reveals the head");
    }

    /// The focused row's caption band opens and pushes only the rows below it; a revealed
    /// row's posters AND caption clear the bottom margin, the last row's included.
    #[test]
    fn an_open_band_moves_only_the_rows_below_and_the_revealed_caption_fits() {
        let bands = settled(Some(1));
        assert_eq!(cell(6, 400.0, 0.0, &bands).y, cell(6, 400.0, 0.0, &[]).y, "the focused row stays");
        assert!((cell(12, 400.0, 0.0, &bands).y - cell(12, 400.0, 0.0, &[]).y - card_row::BAND_OPEN).abs() < 0.01);
        let len = 40;
        for row in [0, 3, rows(len) - 1] {
            let scroll = reveal_row(0.0, row, len, 400.0);
            let r = cell(row * COLS, 400.0, scroll, &settled(Some(row)));
            assert!(r.y + CARD_H + UNDER_LABEL_H <= SCR_H - MARGIN_Y + 0.01, "row {row} caption clipped");
            assert!(r.y >= MARGIN_Y - 0.01, "row {row} pushed off the top");
        }
    }

    #[test]
    fn all_caption_bands_open_with_shared_motion_and_stop_requesting_frames_at_rest() {
        let mut bands = GridBands::new();
        bands.focus(Some(0), false);
        bands.focus(Some(1), true);
        let k = RowStyle::HOME.k_scroll;
        let (_, moving) = plx_machine::idle::scoped_motion(|| bands.tick(k, 1.0 / 60.0));
        assert!(moving, "caption motion keeps the presenter awake");
        let geometry = bands.geometry();
        let opened = geometry.iter().find(|b| b.row == 1).unwrap().expansion;
        let closing = geometry.iter().find(|b| b.row == 0).unwrap().expansion;
        assert!(opened > 0.0 && opened < 1.0 && closing > 0.0 && closing < 1.0);
        assert!((opened + closing - 1.0).abs() < 0.0001);
        for _ in 0..120 { bands.tick(k, 1.0 / 60.0); }
        let (_, moving) = plx_machine::idle::scoped_motion(|| bands.tick(k, 1.0 / 60.0));
        assert!(!moving, "a settled grid lets idle suppression sleep");
        assert_eq!(bands.slots.iter().filter(|s| s.row.is_some()).count(), 1);
        let settled = bands.geometry().iter().find(|b| b.row == 1).unwrap().expansion;
        let whole = card_row::TileLabel::titled("t", "d").revealed(112.0, settled, 0.0).lines();
        assert!(whole.iter().flatten().all(|l| l.alpha > 0.999), "a settled band shows its whole caption");
        bands.focus(None, true);
        for _ in 0..120 { bands.tick(k, 1.0 / 60.0); }
        assert!(bands.slots.iter().all(|s| s.row.is_none()));
    }

    /// The alpha a one-line title is drawn with under a `band` px band at expansion `e`.
    fn title_alpha(band: f32, e: f32) -> f32 {
        card_row::TileLabel::title("t").revealed(band, e, 0.0).lines()[0].unwrap().alpha
    }

    /// **A focus move always has a title on screen at the 112 px band, and dims once at the 80 px
    /// one.** The arriving row's title is drawn once the room under it holds its box and the
    /// departing row's until the room that held it is gone ([`card_row::line_reveal_for`]); the two
    /// bands of a move sum to 1 (`e` and `1 - e`). At 112 the two windows overlap (the title's box
    /// is held from e = .28 on the way in and until e = .72 on the way out), so one of them is
    /// always drawn. At 80 the room holds the box exactly at e = .5 for both rows: the departing
    /// title is gone as the arriving one begins, so the crossing reads as a dip to nothing at that
    /// frame (and a dim title for the few percent of the move either side), not as a title over
    /// the row under it.
    #[test]
    fn a_focus_move_hands_the_title_over_without_a_blank_at_112_and_dips_once_at_80() {
        let shown = |band: f32, e: f32| title_alpha(band, e).max(title_alpha(band, 1.0 - e));
        for step in 0..=1000 {
            let e = step as f32 / 1000.0;
            assert!(shown(112.0, e) > 0.0, "112 px band: no title on screen at e {e}");
        }
        assert!(shown(80.0, 0.5) < 0.001, "80 px band: the crossing holds neither title");
        let dim: Vec<u32> = (0..=1000).filter(|&k| shown(80.0, k as f32 / 1000.0) < 0.5).collect();
        let (lo, hi) = (dim[0], *dim.last().unwrap());
        assert!(dim.len() as u32 == hi - lo + 1 && hi - lo < 200,
            "80 px band: the dim window is one short run around e = .5 ({lo}..{hi} of 1000)");
    }

    /// The bands' canon is the focused row, then a counted sequence of the open bands, each its row,
    /// its spring and the card whose caption it carries (`None` until the row has held a tick).
    /// Pinned from literals, in the order `LibraryGrid::SHAPE` declares
    /// (`slots:[(row:u32,sp:..,card:Option<u32>)]`), so a dropped, reordered or undeclared field
    /// fails here; the replay anchors (`make check-replay`) are what hold the bytes of a whole page.
    #[test]
    fn the_bands_canon_is_the_focus_then_a_counted_sequence_of_row_spring_and_card() {
        use plx_machine::machine::Canon;
        let written = |bands: &GridBands| { let mut c = Canon::new(); bands.write(&mut c); c.finish() };
        let expect = |focus: Option<usize>, slots: &[(usize, f32, f32, Option<usize>)]| {
            let mut c = Canon::new();
            c.option(focus, |c, row| { c.u32(row as u32); });
            c.seq(slots.len());
            for &(row, pos, vel, card) in slots {
                c.u32(row as u32).f32(pos).f32(vel);
                c.option(card, |c, card| { c.u32(card as u32); });
            }
            c.finish()
        };
        let mut bands = GridBands::new();
        assert_eq!(written(&bands), expect(None, &[]), "no focus, no band: the empty sequence");
        bands.focus(Some(0), false);
        assert_eq!(written(&bands), expect(Some(0), &[(0, 1.0, 0.0, None)]), "an open band no card has been noted on");
        bands.note_card(0, 7);
        assert_eq!(written(&bands), expect(Some(0), &[(0, 1.0, 0.0, Some(7))]), "the band carries its card");
        bands.focus(Some(1), true);
        assert_eq!(
            written(&bands),
            expect(Some(1), &[(0, 1.0, 0.0, Some(7)), (1, 0.0, 0.0, None)]),
            "the closing band keeps its card and the opening one has none yet",
        );
    }

    /// The card in `cell`, an element that is its cell number (nothing here moves the cards).
    fn at(cell: usize) -> Card {
        Card { cell, key: cell as u64 }
    }

    /// The pop is armed from rest by a deliberate move, adopted whole when focus arrives
    /// unannounced, lets the old cell back down and sleeps once settled.
    #[test]
    fn the_focus_pop_grows_from_rest_adopts_a_seat_whole_and_settles() {
        let style = STYLE;
        let dt = 1.0 / 60.0;
        let mut pop = GridPop::new();
        assert_eq!(pop.scale(3, true, &style), style.focus_scale, "an unannounced focus draws whole");
        pop.tick(Some(at(3)), &style, dt);
        assert_eq!(pop.scale(3, true, &style), style.focus_scale, "…and the tick adopts it unanimated");
        pop.arm(at(4));
        pop.tick(Some(at(4)), &style, dt);
        let (new, old) = (pop.scale(4, true, &style), pop.scale(3, false, &style));
        assert!(new > 1.0 && new < style.focus_scale, "the new cell grows from rest: {new}");
        assert!(old > 1.0 && old < style.focus_scale, "the old cell lets go: {old}");
        for _ in 0..120 { pop.tick(Some(at(4)), &style, dt); }
        assert!((pop.scale(4, true, &style) - style.focus_scale).abs() < 0.001);
        assert_eq!(pop.scale(3, false, &style), 1.0);
        let (_, moving) = plx_machine::idle::scoped_motion(|| pop.tick(Some(at(4)), &style, dt));
        assert!(!moving, "a settled grid lets idle suppression sleep");
    }

    /// **A released grid card lands by the one shared rule** (`Spring::land`, the rule a shelf's
    /// `CardRow::update` uses): the let-go is judged finished only once the spring is within `LAND_REL`
    /// of rest AND no longer travelling, so the frame the tile leaves the let-go list steps its rect,
    /// radius and shadow by no more than that epsilon.
    ///
    /// The reference is the same spring stepped alone, so the test also pins that nothing but the
    /// landing differs: the let-go runs the same constants toward the same target.
    #[test]
    fn a_released_grid_card_lands_by_the_shared_rule_and_never_snaps_while_moving() {
        let style = STYLE;
        let dt = 1.0 / 60.0;
        let mut pop = GridPop::new();
        pop.tick(Some(at(3)), &style, dt); // seated whole
        pop.arm(at(4));
        let mut reference = Spring::at(style.focus_scale);
        let mut landed = false;
        for frame in 1..=600 {
            let was_letting_go = pop.letting_go(3);
            pop.tick(Some(at(4)), &style, dt);
            reference.step(1.0, style.k_scale, dt);
            let drawn = pop.scale(3, false, &style);
            if was_letting_go && !pop.letting_go(3) {
                let epsilon = crate::LAND_REL * (1.0 + reference.pos.max(1.0));
                assert!(
                    (drawn - reference.pos).abs() <= epsilon,
                    "frame {frame}: the let-go left the list at {} while still travelling ({}/frame), a {} step to {drawn} against the land epsilon {epsilon}",
                    reference.pos,
                    reference.vel * dt,
                    (drawn - reference.pos).abs(),
                );
                assert_eq!(drawn, 1.0, "it lands exactly at rest");
                landed = true;
                break;
            }
            assert_eq!(drawn, reference.pos, "frame {frame}: the let-go is the unchanged spring until it lands");
        }
        assert!(landed, "the let-go never landed");
    }

    /// **A press never replaces a let-go**: each card released by a held key keeps its own spring to
    /// the landing, whatever the presses do to the others, and a focus that comes back to one still
    /// letting go takes up its spring where it is.
    #[test]
    fn a_press_adds_a_let_go_and_replaces_none() {
        let style = STYLE;
        let dt = 1.0 / 60.0;
        let mut pop = GridPop::new();
        pop.tick(Some(at(3)), &style, dt); // seated whole
        let mut reference = [Spring::at(style.focus_scale), Spring::at(1.0), Spring::at(1.0), Spring::at(1.0)];
        for (n, cell) in [4usize, 5, 6].into_iter().enumerate() {
            pop.arm(at(cell));
            for _ in 0..3 {
                pop.tick(Some(at(cell)), &style, dt);
                for (c, sp) in reference.iter_mut().enumerate().take(n + 2) {
                    sp.step(if c == n + 1 { style.focus_scale } else { 1.0 }, style.k_scale, dt);
                }
            }
        }
        for c in 3..=5 {
            assert!(pop.letting_go(c), "cell {c} is still letting go");
            assert_eq!(pop.scale(c, false, &style), reference[c - 3].pos, "cell {c} runs its own spring");
        }
        assert!(!pop.letting_go(6), "the focused cell is the pop, not a let-go");
        // focus comes back to 5 while it is letting go: it grows from where it is, with its velocity
        let was = pop.scale(5, false, &style);
        pop.arm(at(5));
        assert_eq!(pop.scale(5, true, &style), was, "no jump on the press");
        assert!(!pop.letting_go(5) && pop.letting_go(6), "5 is the pop again and 6 let go");
        // a seat adopted whole does not stop the others
        pop.adopt(at(9), &style);
        assert!(pop.letting_go(3) && pop.letting_go(4) && pop.letting_go(6), "the let-gos in flight go on");
        for _ in 0..120 { pop.tick(Some(at(9)), &style, dt); }
        assert!((3..=6).all(|c| !pop.letting_go(c)), "and each lands");
        assert!((3..=6).all(|c| pop.scale(c, false, &style) == 1.0), "exactly at rest");
    }

    /// **A let-go is born with the element its card is.** The pop knows which element it is on, even
    /// when the cards changed since it was armed: a press names the new card by element, so the
    /// card it leaves lets go as itself and the card it names takes up ITS let-go, whatever cell
    /// the other stood in. A card re-keyed in its cell (a placeholder whose content landed) keeps
    /// the pop's spring and lets go under its new key.
    #[test]
    fn a_let_go_is_born_with_its_element_and_a_pick_up_is_by_element() {
        let style = STYLE;
        let dt = 1.0 / 60.0;
        let mut pop = GridPop::new();
        pop.tick(Some(Card { cell: 5, key: 50 }), &style, dt);
        // the cards shifted: the element 60 now stands in the cell 5 the pop was armed in
        pop.arm(Card { cell: 5, key: 60 });
        let gone: Vec<_> = pop.let_gos().collect();
        assert_eq!(gone, [Card { cell: 5, key: 50 }], "the card left lets go as ITSELF, not as the cell's new content");
        assert_eq!(pop.pop.spring.pos, 1.0, "and the card named grows from rest, not from the let-go in its cell");
        // 60 lets go, then focus comes back to 50 while it is letting go: 50's spring, by element
        pop.arm(Card { cell: 6, key: 70 });
        assert!(pop.letting_go(5));
        let was = pop.scale(5, false, &style);
        pop.arm(Card { cell: 4, key: 50 });
        assert_eq!(pop.pop.spring.pos, was, "50 takes up its own spring");
        assert_eq!(pop.let_gos().map(|c| c.key).collect::<Vec<_>>(), [60, 70], "and nothing else's");
        // re-keyed in its cell: the pop carries on and lets go under the new key
        pop.tick(Some(Card { cell: 4, key: 51 }), &style, dt);
        pop.arm(Card { cell: 9, key: 90 });
        assert!(pop.let_gos().any(|c| c == Card { cell: 4, key: 51 }), "the released card is the one now standing in the cell");
    }

    /// The canon is the pop's cell and spring, then a counted sequence of (cell, spring) let-gos in
    /// flight, oldest release first; a grid with none writes the empty sequence. Pinned from
    /// literals, not from the fields the writer reads, so a reordered, dropped or doubled field
    /// fails here; the replay anchors (`make check-replay`) are what hold the bytes of a whole page.
    #[test]
    fn the_canon_is_the_pop_then_a_counted_sequence_of_let_gos() {
        use plx_machine::machine::Canon;
        let style = STYLE;
        let dt = 1.0 / 60.0;
        let written = |pop: &GridPop| { let mut c = Canon::new(); pop.write(&mut c); c.finish() };
        let expect = |pop: (Option<usize>, f32, f32), let_gos: &[(usize, f32, f32)]| {
            let mut c = Canon::new();
            c.option(pop.0, |c, i| { c.u32(i as u32); });
            c.f32(pop.1).f32(pop.2);
            c.seq(let_gos.len());
            for &(cell, pos, vel) in let_gos { c.u32(cell as u32).f32(pos).f32(vel); }
            c.finish()
        };
        let mut pop = GridPop::new();
        assert_eq!(written(&pop), expect((None, 1.0, 0.0), &[]), "no pop, no let-go: the empty sequence, no phantom slot");
        pop.tick(Some(at(3)), &style, dt);
        pop.arm(at(4));
        let whole = style.focus_scale;
        assert_eq!(written(&pop), expect((Some(4), 1.0, 0.0), &[(3, whole, 0.0)]), "the pop, then the card it let go");
        pop.arm(at(5));
        assert_eq!(
            written(&pop),
            expect((Some(5), 1.0, 0.0), &[(3, whole, 0.0), (4, 1.0, 0.0)]),
            "a second let-go adds an element after the first",
        );
        for _ in 0..240 { pop.tick(Some(at(5)), &style, dt); }
        let (pos, vel) = (pop.pop.spring.pos, pop.pop.spring.vel);
        assert_eq!(written(&pop), expect((Some(5), pos, vel), &[]), "landed let-gos leave the empty sequence");
    }

    /// **The canon is self-delimiting.** The grid's canon is followed by other state in the same
    /// stream (the bands, in a Library page's `GridPart::write`), so two different grids must not
    /// hash alike just because the bytes after one read as another let-go. One let-go followed by a
    /// continuation shaped like a let-go is NOT two let-gos.
    #[test]
    fn a_grid_pop_canon_does_not_run_into_the_state_after_it() {
        use plx_machine::machine::Canon;
        let style = STYLE;
        let dt = 1.0 / 60.0;
        // focus on 5, with 3 letting go
        let mut one = GridPop::new();
        one.tick(Some(at(3)), &style, dt);
        one.arm(at(5));
        // focus on 5, with 3 and 4 letting go
        let mut two = GridPop::new();
        two.tick(Some(at(3)), &style, dt);
        two.arm(at(4));
        two.arm(at(5));
        let mut a = Canon::new();
        one.write(&mut a);
        // the continuation: a cell and a spring at rest, which is cell 4's let-go in `two`
        a.option(Some(4usize), |c, i| { c.u32(i as u32); }).f32(1.0).f32(0.0);
        let mut b = Canon::new();
        two.write(&mut b);
        assert_ne!(a.finish(), b.finish(), "one let-go and a let-go-shaped continuation is not two let-gos");
    }

    #[test]
    fn rapid_all_row_moves_keep_animation_bounded_and_preserve_the_focused_band() {
        let mut bands = GridBands::new();
        bands.focus(Some(0), false);
        for row in 1..200 {
            bands.focus(Some(row), true);
            bands.tick(RowStyle::EPISODE.k_scroll, 1.0 / 240.0);
            assert!(bands.slots.iter().any(|s| s.row == Some(row)));
            assert!(bands.slots.iter().filter(|s| s.row.is_some()).count() <= MAX_GRID_BANDS);
        }
        for _ in 0..120 { bands.tick(RowStyle::EPISODE.k_scroll, 1.0 / 60.0); }
        assert_eq!(bands.slots.iter().filter(|s| s.row.is_some()).count(), 1);
        assert!(bands.geometry().iter().find(|b| b.row == 199).unwrap().expansion > 0.999);
        let before = bands.geometry();
        bands.focus(Some(199), true);
        assert_eq!(bands.geometry(), before, "moving horizontally does not close the same row");
    }
}
