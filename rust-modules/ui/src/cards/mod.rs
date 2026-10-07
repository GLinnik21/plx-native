//! `plx_ui::cards` — the shared card sections (shared-card-sections plan, layer L1).
//!
//! [`Shelf`] (one horizontal strip) and [`Grid`] (the six-column poster grid) own everything a card
//! section does that is not the screen's content: the focus pop, the let-go of the tile that lost
//! focus, scrolling to keep focus visible, the caption band, drawing the focused tile last, culling,
//! hit-map stops, `Focusable` placement, paging requests and idle. A screen supplies its content
//! through [`CardSource`] and reacts to the one [`CardEvent`] [`Shelf::on`] / [`Grid::on`] returns;
//! it no longer writes pop, press, stop, restore or paging code.
//!
//! The contract, fixed once for every section:
//!
//! - **One entry.** `on(ev, cx, src, fx)` is fed EVERY `ScreenEvent` the screen receives. It
//!   consumes Tick and FocusMoved and reports Activate / Hold / Want; a screen that skips the call
//!   gets no motion at all, which is visible, not subtle. It never says "handled": a screen still
//!   observes `FocusMoved` itself for whatever else it keeps.
//! - **Elem-keyed.** The section remembers no focus. Each call it reads the ENGINE's focus
//!   (`cx.focus.current`, filtered to its entry) and resolves the element to an index through
//!   [`CardSource::index_of`], so a landing that reorders content cannot leave it naming another
//!   item. Events carry `H::Elem`. (The pop springs are still per position — owner decision 2 is
//!   open — so a tick that finds the focused element at another index with NO `FocusMoved`, a
//!   content landing, MOVES the spring to the new index, snaps the cell it left to rest and shifts
//!   the scroll by the index delta so the tile does not move on screen — [`Landed`] reports it.
//!   A non-deliberate `FocusMoved` adopts the new card whole and snaps the old one to rest; only a
//!   deliberate move lets the old tile go over frames. At most one tile reads lifted in any frame,
//!   including the one between the landing and the tick.)
//! - **The pop rule.** A tile the engine reports focused that no deliberate move (`By::Dir` /
//!   `By::Pointer`) announced draws at FULL focus scale at once — a restore or reconcile is adopted
//!   whole, a page returns exactly as it was left. An unfocused tile draws at rest (or the live
//!   let-go of the one that just lost focus). A deliberate move grows from rest.
//!   A shelf whose page is still dissolving in is [`dormant`](Shelf::dormant): nothing is lifted
//!   under the fade, and the first awake tick grows the focused card from rest.
//! - **One frame convention.** A [`SectionFrame`]'s `y` is screen space; the painter handed to
//!   `draw` may carry any translate (see [`SectionFrame`]).
//! - **Press comes from the frame**, `cx.press` / `f.press` through `PressRead::dip()`, never the
//!   thread-local. [`Shelf::place`] / [`Grid::place`] answer the live drawn rect and the stop the
//!   draw registers is the same value; `rest_rect` is the settled focus-scaled rect.
//! - **Idle.** Springs report their own motion; a settled section reports none and parks at exact
//!   rest. No call allocates except the focused tile's label, as before.
//!
//! The L0 primitives (`card_row`, `poster_grid`) stay public until the last migration PR; the
//! `cards` gate in `ci/check-deps.sh` stops `screens/` growing new direct uses of them.
//! Tier 1 of the conformance suite (`tests.rs`) runs the same seven cases the real screens run
//! (`conformance`, Tier 2) against both components on `FixtureHost`.

use std::ops::Range;

use plx_machine::machine::{Cx, EntryId, FocusRead, Host, Measure};

use crate::card_row::TileLabel;
use crate::widgets::Art;
use crate::{Painter, Rect};

mod grid;
mod shelf;
mod stack;

pub use grid::{Grid, GridSpec, ScrollMode};
pub use shelf::Shelf;
pub use stack::{Kind, SectionSpec, Stack, StackEvent, StackMemory, StackPage, StackView};

#[cfg(any(test, feature = "test-support"))]
pub mod conformance;
#[cfg(test)]
mod tests;

/// The read-only content a section draws. The screen implements it over its own store view; the
/// component never owns or copies content.
pub trait CardSource<H: Host> {
    fn len(&self) -> usize;
    /// The engine element of card `i`.
    fn elem(&self, i: usize) -> H::Elem;
    /// The card showing `e`, if it is in this source — the inverse of [`elem`](Self::elem).
    fn index_of(&self, e: &H::Elem) -> Option<usize>;
    fn art(&self, i: usize) -> Art<'_>;
    /// The block drawn under the FOCUSED card (asked for that one card per frame).
    fn label(&self, i: usize) -> TileLabel;
    /// The amber resume fraction of card `i`, if it is partly watched.
    fn progress(&self, _i: usize) -> Option<f32> {
        None
    }
    /// Anything drawn on EVERY card after its body (a persistent poster label, a cast name).
    /// `tile` is where the card was drawn, press and pop included.
    fn overlay(&self, _p: Painter, _i: usize, _tile: &Tile, _measure: &dyn Measure) {}
    /// Whether card `i`'s content is present to paint. A card that is not (a slot of a paged
    /// listing whose page has not landed) is skipped by `draw`; its stop still registers.
    fn loaded(&self, _i: usize) -> bool {
        true
    }
    /// Whether a hold on card `i` is a [`CardEvent::Hold`].
    fn holdable(&self, _i: usize) -> bool {
        true
    }
    /// Whether the store has more cards beyond `len()`; only then are [`CardEvent::Want`]s emitted.
    fn more(&self) -> bool {
        false
    }
}

/// Where one card was drawn, handed to [`CardSource::overlay`].
#[derive(Clone, Copy, Debug)]
pub struct Tile {
    pub rect: Rect,
    /// The scale `rect` was built from (pop and press), the one the card's treatment derives from.
    pub scale: f32,
    pub radius: f32,
    pub focused: bool,
}

/// What a section reports back from [`Shelf::on`] / [`Grid::on`]. One at most per call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardEvent<E> {
    /// A press committed on the focused card.
    Activate(E),
    /// A hold on the focused card, which [`CardSource::holdable`] allows.
    Hold(E),
    /// The visible window (plus look-ahead) reaches past `len`: the source needs cards up to
    /// `range.end`. Emitted from Tick, once per `(len, end)`.
    Want(Range<usize>),
}

/// Where a [`Shelf`] sits for one call. `y` is the top of its tiles in SCREEN space, the one
/// convention `place`, `draw`, `record_stops` and `redraw_focused` all read: `place` answers it
/// as is, and the draw calls map it into the painter they are given through that painter's own
/// translate (`Painter::dy`), so a painter carrying a page offset or a block origin paints, and
/// registers the stops of, the very rects `place` reports. `clip` is the stops' own clip.
#[derive(Clone, Copy, Debug)]
pub struct SectionFrame {
    pub y: f32,
    pub clip: Rect,
}

/// What the `FocusMoved` events since a section's last tick told it about the card focus went to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Seen {
    /// None arrived: a focused card that is not where the section last had it is the SAME element
    /// moved by a content landing.
    Nothing,
    /// A deliberate move (`By::Dir` / `By::Pointer`) handed focus to this card: it grows from rest.
    Deliberate(usize),
    /// A restore, a reconcile or a seat: adopted whole.
    Other,
}

impl Seen {
    pub(crate) fn of(by: &crate::screen::By, arrived: usize) -> Self {
        if matches!(by, crate::screen::By::Dir | crate::screen::By::Pointer) { Seen::Deliberate(arrived) } else { Seen::Other }
    }
}

/// The focused element's card index changed because the CONTENT did (an insert or a reorder
/// before it), not because focus moved: the section carried its pop to `to`, snapped the tile it
/// left to rest and shifted its scroll so the tile stayed where it was on screen. Readable through
/// [`Shelf::landed`] / [`Grid::landed`] for the tick that did it, so an owner that scrolls a page
/// around the section (a later `Stack`) can shift by the same amount.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Landed {
    pub from: usize,
    pub to: usize,
}

/// The index of the engine-focused card in `src`, when focus is in `entry`'s page and on this
/// source. The one place a section reads focus.
pub(crate) fn focused_index<H: Host, S: CardSource<H>>(
    focus: &FocusRead<H::Elem>,
    entry: EntryId,
    src: &S,
) -> Option<usize> {
    let key = focus.current.filter(|key| key.entry == entry)?;
    src.index_of(&key.elem)
}

/// The paging rule shared by both sections: ask for cards up to `end` once per `(len, end)`.
pub(crate) fn want(asked: &mut Option<(usize, usize)>, len: usize, end: usize, more: bool) -> Option<Range<usize>> {
    if !more || end <= len || *asked == Some((len, end)) {
        return None;
    }
    *asked = Some((len, end));
    Some(len..end)
}

/// The scale a card paints at: the pop for the focused card, times the frame's press dip.
#[inline]
pub(crate) fn press_scale(pop: f32, focused: bool, cx: &Cx<'_, impl Host>) -> f32 {
    if focused { pop * cx.press.dip() } else { pop }
}

/// `r` (screen space) in painter `p`'s space: the painter's own translate undone, so a rect drawn
/// or registered through `p` lands where `r` says on the screen.
#[inline]
pub(crate) fn to_local(p: Painter, r: Rect) -> Rect {
    Rect::new(r.x, r.y - p.dy(), r.w, r.h)
}
