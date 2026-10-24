//! `plx_ui::cards` — the shared card sections (layer L1: the sections built on the L0 row and grid primitives).
//!
//! [`Shelf`] (one horizontal strip) and [`Grid`] (the six-column poster grid) own everything a card
//! section does that is not the screen's content: the focus pop, the let-go of every tile that lost
//! focus, scrolling to keep focus visible, the caption band, drawing the focused tile last, culling,
//! hit-map stops, `Focusable` placement, paging requests and idle. A screen supplies its content
//! through [`CardSource`] and reacts to the one [`CardEvent`] [`Shelf::on`] / [`Grid::on`] returns;
//! it writes no pop, stop, restore or paging code of its own (and, for the stack pages, no press
//! code either — see the exceptions under "One entry").
//!
//! The contract, fixed once for every section:
//!
//! - **One entry.** `on(ev, cx, src, fx)` is fed EVERY `ScreenEvent` the screen receives. It
//!   consumes Tick and FocusMoved and reports Activate / Hold / Want; a screen that skips the call
//!   gets no motion at all, which is visible, not subtle. It never says "handled": a screen still
//!   observes `FocusMoved` itself for whatever else it keeps.
//!   Three screens are exceptions to "reports Activate / Hold / Want": Home feeds `Tick` and
//!   `FocusMoved` only and keeps its own press paths, because a Continue Watching press is
//!   account-affecting; Library and Detail do feed every event but discard the returned
//!   [`CardEvent`] and keep their own `Activate` / `PressCommit` / `PressHold` handling (Library's
//!   item menu and review flow, Detail's section-aware activation). The `cards::Stack` pages
//!   (Collection, Person, Search) act on the event.
//! - **Elem-keyed.** The section remembers no focus. Each call it reads the ENGINE's focus
//!   (`cx.focus.current`, filtered to its entry) and resolves the element to an index through
//!   [`CardSource::index_of`], so a landing that reorders content cannot leave it naming another
//!   item. Events carry `H::Elem`. (The pop springs follow their elements — `pool.rs` — so a tick
//!   that finds the focused element at another index with NO `FocusMoved`, a
//!   content landing, MOVES the spring to the new index, snaps the cell it left to rest and shifts
//!   the scroll by the index delta so the tile does not move on screen — [`Landed`] reports it.
//!   A non-deliberate `FocusMoved` adopts the new card whole and snaps the old one to rest; only a
//!   deliberate move lets the old tile go over frames. At most one tile reads lifted in any frame,
//!   including the one between the landing and the tick.)
//! - **Animated landings.** A [`Shelf`] or a [`Grid`] built with [`Shelf::animate_landings`] plays a content change
//!   that no focus move announced (a card leaves, a card arrives) instead of cutting: the removed
//!   card fades out as a ghost the owner paints ([`Shelf::paint_ghosts`], from the cards named by
//!   [`Shelf::ghost_keys`]), the cards that stay slide to their slots (the focused tile is held by
//!   the landing rule, so it does not move unless the scroll's range clamps it), the new ones fade in.
//!   Off by default; first loads, slides of the source's window and [`dormant`](Shelf::dormant)
//!   shelves never animate; the springs are frame-time and count as layout motion. See `landing.rs`.
//!   [`Grid`] plays them too when its [`GridSpec`] says `.animate_landings(true)`: slots are (x, y),
//!   ghosts fade on K_SCALE, survivors spring home on K_SCROLL.
//! - **The pop rule.** A tile the engine reports focused that no deliberate move (`By::Dir` /
//!   `By::Pointer`) announced draws at FULL focus scale at once — a restore or reconcile is adopted
//!   whole, a page returns as it was left (a page that returns dormant, below, regrows its focused
//!   card from rest instead). An unfocused tile draws at rest (or the live
//!   let-go of one that lost focus and has not landed). A deliberate move grows from rest (or from
//!   where its own let-go is, when focus comes back to a tile still letting go).
//!   A shelf or grid whose page is still dissolving in, or is shown as a held transition image, is
//!   [`dormant`](Shelf::dormant) / [`Grid::dormant`]: nothing is lifted under the fade or the image
//!   (a shelf on a page in view eases what was lifted to rest; under a held image it, like a grid,
//!   is at rest on its first tick), and the first awake tick grows the focused card from rest. A
//!   focus the engine restored while the owner slept regrows from rest too, once a dormant tick has
//!   run since (the Recommended page returning from a Detail under the BACK dip's image); only a
//!   restore announced after the last dormant tick, on the tick the page wakes, is adopted whole,
//!   as above. A grid keeps
//!   its caption bands shut meanwhile and the waking tick opens the focused row's on the lift's own
//!   frames: a caption is its band's content (a line is drawn only as the band opens and the room
//!   holds it, `card_row::line_reveal_for`), so it never stands at full strength under a tile that
//!   has not lifted. An owner seating its page's ARRIVAL later (a
//!   listing that answered after the dissolve lands its first poster) says so with
//!   [`Grid::arrive`], and that one `FocusMoved` grows from rest like a deliberate move, band and all.
//! - **One frame convention.** A [`SectionFrame`]'s `y` is screen space; the painter handed to
//!   `draw` may carry any translate (see [`SectionFrame`]). It also says how far the heading under
//!   the shelf is drawn above its place (`below_rise`), which a caption line stops short of.
//! - **Press comes from the frame**, `cx.press` / `f.press` through `PressRead::dip_of()`, never the
//!   thread-local. The dip lands on the card the press OWNS (`PressRead::owner`), focused or not:
//!   an abandoned press springs back on the card that was pressed while its neighbour takes the
//!   ordinary focus pop. [`Shelf::place`] / [`Grid::place`] answer the live drawn rect and the stop the
//!   draw registers is the same value; `rest_rect` is the settled focus-scaled rect.
//! - **Idle.** Springs report their own motion; a settled section reports none and parks at exact
//!   rest. What allocates: the focused tile's label, a landing's diff, a [`Grid`]'s draw plan (built
//!   each frame) and its let-go list, which grows to the most cards a held key has had letting go at
//!   once and is kept across a dormant wake and [`Grid::forget_pop`], so a press or a tick allocates
//!   only when a key first puts more in flight than the grid has seen.
//!
//! The L0 primitives (`card_row`, `poster_grid`) are `pub(crate)`: a screen cannot hand-assemble a
//! shelf or a grid. What a screen legitimately reads from them (the tile style and label types, the
//! row and band geometry its own layout is built from, the heading painters) is re-exported here;
//! the `cards` gate in `ci/check-deps.sh` is the backstop.
//! Tier 1 of the conformance suite (`tests.rs`) runs the same eight cases the real screens run
//! (`conformance`, Tier 2) against both components on `FixtureHost`.

use std::ops::Range;

use plx_machine::machine::{Cx, EntryId, FocusRead, Host, Measure};

use crate::screen::DrawFrame;
use crate::widgets::Art;
use crate::{Painter, Rect};

mod avatars;
mod grid;
mod landing;
mod pool;
mod sample_row;
mod shelf;
mod stack;

pub use avatars::AvatarRow;
// Geometry and painters a screen's own layout reads (L0 `card_row` / `poster_grid`, re-exported so
// those modules can stay `pub(crate)`).
pub use crate::card_row::{
    column_near_x, credit_role_dy, draw_credit_label, draw_heading, draw_heading_c, draw_label, cw_caption, heading_flow, heading_parts, HeadingPart, HEADING_SOURCE_PAD, heading_lift_max, label_band, paint_visible,
    resume_bar, reveal, scroll_into_view, settled_top, settled_top_for, tile_rect, under_band, under_band_for, RowStyle, TileLabel,
    band_open_for, BAND_OPEN, CREDIT_ROLE_LEADING, LABEL_BAND_COLLAPSED, LABEL_EDGE_FADE, LEGACY_BAND_H, MAX_ROW_ITEMS, ROW_PITCH_FIXED, UNDER_LABEL_H,
};
pub use crate::poster_grid::{
    growth_before as grid_growth_before, growth_before_for as grid_growth_before_for, rest_scroll as grid_rest_scroll, settled as grid_settled, GridBand, COLS as GRID_COLS, MAX_GRID_BANDS,
    STYLE as GRID_STYLE,
};
pub use grid::{DrawRole, Grid, GridSpec, ScrollMode};
pub use sample_row::SampleRow;
pub use shelf::{PageEdge, Shelf, PAGE_EDGE_CARDS};
pub use stack::{Kind, SectionSpec, Stack, StackEvent, StackMemory, StackPage, StackView};

#[cfg(any(test, feature = "test-support"))]
pub mod conformance;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod warm_tests;

/// Rows a screen's store holds cards for beyond the ones in view, each side: the least a screen
/// may be left with. [`HoldBand`] asks for [`HOLD_SLACK`] more each side when it speaks.
pub const HOLD_MARGIN: usize = 2;

/// Rows a [`HoldBand`] asks for beyond the [`HOLD_MARGIN`] each time it sends a band, so a press
/// that scrolls a row does not cost the store a new band: it is sent again only once the rows in
/// view have used the slack up. The margin is never smaller than [`HOLD_MARGIN`]; the slack is
/// where the hysteresis lives (not a cap: any row is reachable, the band follows the view).
pub const HOLD_SLACK: usize = 2;

/// The most rows of one shelf column that are on screen at once, partly or whole: the whole rows
/// that fit the panel at the fixed pitch (the focused row's under-label band only adds to it) and
/// the two the edges cut. Geometry, not a list bound: it sizes the rule below.
pub const ROWS_IN_VIEW_MAX: usize = (crate::consts::SCR_H / ROW_PITCH_FIXED) as usize + 2;

// The rule a band relies on: the rows in view plus the margin and slack each side fit in the rows
// a store keeps ([`plx_base::hold::HOLD_ROWS`]). Past it the store keeps LESS than the band asks
// for, and "the rows in view always have [`HOLD_MARGIN`] held beyond them" stops being true.
// (A view that is taller for a moment, a glide whose focused row is far from the rows on screen,
// is the case [`HoldBand`] handles by tracking what the store kept.)
const _: () = assert!(
    ROWS_IN_VIEW_MAX + 2 * (HOLD_MARGIN + HOLD_SLACK) <= plx_base::hold::HOLD_ROWS,
    "the rows in view plus the hold margin and slack each side must fit the rows the store keeps"
);

/// The rows `seen` (the first and last row in view, the focused row folded in) widen to once the
/// [`HOLD_MARGIN`] is added each side, clamped to the `len` rows there are (`len` is at least 1):
/// the band a screen must always have held, whatever it last sent.
pub fn hold_band((lo, hi): (usize, usize), len: usize) -> (usize, usize) {
    (lo.saturating_sub(HOLD_MARGIN), (hi + HOLD_MARGIN).min(len - 1))
}

/// The hold band a screen sends its store (`HubsCmd::Hold`, `LibraryWork::HubHold`), with
/// hysteresis. The store gives the cards of every row outside the band up and reads the ones that
/// enter it again, so a band that follows the view row by row costs a release, a rebuild of the
/// catalog and a read per press. This one speaks only when the rows the store KEPT of the band it
/// last sent no longer cover [`hold_band`] of the rows in view, and then asks for [`HOLD_SLACK`]
/// rows more each side, so a walk down a long page sends one band per [`HOLD_SLACK`] rows. The rows
/// in view have [`HOLD_MARGIN`] held beyond them whenever they fit the store's bound (the rule
/// asserted above); the slack is on top of that, which is what keeps this from being a cap: the
/// band moves with the view and every row stays reachable.
///
/// **What the store kept, not what was asked.** A store keeps at most [`plx_base::hold::HOLD_ROWS`]
/// rows of a band, cut by [`plx_base::hold::clamp_hold`] (the very function the store applies, so
/// there is one cut for every caller). Coverage is judged against that cut: a screen whose focus is
/// far from the rows on screen (BACK to the top while the view glides up from row 20, the focused
/// row folded into `seen`) asks for more rows than are kept, the store keeps the middle of them,
/// and the rows beyond stay released. Judged against the ask, the band would call them covered and
/// say nothing more; judged against the cut, it asks again as the glide shortens the span until the
/// rows in view are held. It is not asked again for a band the store would keep exactly as it keeps
/// the one it has (a span too tall for any band), which would flood the store with the same cut.
///
/// **What a band costs the store.** A walk down a page sends one band per `HOLD_SLACK + 1` rows (the
/// band reaches `HOLD_MARGIN + HOLD_SLACK` past the view and is sent again once the view uses up
/// the slack). The store takes a changed band as its signal to ask again for rows whose reload
/// failed (`stalled`), so a failed row is retried about once per three rows of movement, not once
/// per row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HoldBand {
    /// The rows the store keeps of the band last sent: `clamp_hold(sent)`.
    kept: Option<(usize, usize)>,
}

impl HoldBand {
    /// The band to send for the rows `seen` of a page of `len` rows (`len` is at least 1), or
    /// `None` when the rows the store kept of the one last sent still cover them and their margin
    /// (or when no band could make it keep more than it does).
    pub fn next(&mut self, seen: (usize, usize), len: usize) -> Option<(usize, usize)> {
        let need = hold_band(seen, len);
        if self.kept.is_some_and(|(lo, hi)| lo <= need.0 && hi >= need.1) {
            return None;
        }
        let reach = HOLD_MARGIN + HOLD_SLACK;
        let band = (seen.0.saturating_sub(reach), (seen.1 + reach).min(len - 1));
        let kept = plx_base::hold::clamp_hold(band);
        if self.kept == Some(kept) {
            return None;
        }
        self.kept = Some(kept);
        Some(band)
    }

    /// Forget what was sent: the next [`next`](Self::next) sends (a different list, or a store
    /// that has not been told).
    pub fn forget(&mut self) {
        self.kept = None;
    }
}

/// The one recovery position every card screen uses when the focused card is gone from its
/// section: the slot it held, clamped to the section's new length. `None` when the section is
/// empty, which is the page's cue to use its fallback.
pub fn clamp_slot(slot: usize, len: usize) -> Option<usize> {
    len.checked_sub(1).map(|last| slot.min(last))
}

/// `e`'s identity as the number a landing's ghosts are named by ([`Shelf::ghost_keys`]).
pub fn elem_key<E: std::hash::Hash>(e: &E) -> u64 {
    pool::key(e)
}

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
    /// Whether card `i`'s content is still to be read: its page has not landed, was evicted or
    /// failed. A [`Grid`] asks again for a card in view that is (see [`CardEvent::Want`]); it
    /// differs from `!loaded(i)` only for a source that paints an unread slot as a placeholder
    /// rather than skipping it.
    fn unread(&self, i: usize) -> bool {
        !self.loaded(i)
    }
    /// The card the ENGINE's focused element `e` is shown on, for the pop, the caption and the
    /// opener redraw: [`index_of`](Self::index_of) unless the screen paints a different focus
    /// than the engine holds (Home's hero dive keeps the card it came from lifted until the
    /// shelves are the picture, and shows none while the picture is still the billboard).
    fn focus_index(&self, e: &H::Elem) -> Option<usize> {
        self.index_of(e)
    }
    /// How the hit map treats card `i`'s stop (`Hover::OnlyIfFocused` keeps a partly visible row
    /// from taking the pointer's focus as it passes). A [`Shelf`] reads it; a `Grid` does not.
    fn hover(&self, _i: usize) -> crate::screen::Hover {
        crate::screen::Hover::Focus
    }
    /// Whether cards the source does not hold lie between card `i - 1` and card `i`: a row that
    /// is a fixed head and a sliding window of what follows draws the two side by side while the
    /// window is away from the head. A press does not step across (it would pass every card
    /// between in one move); the screen asks its store to bring the window back instead.
    fn gap_before(&self, _i: usize) -> bool {
        false
    }
    /// Card `i`'s position in the whole listing this source is a window of, whichever window it
    /// holds now (a slid shelf's first card is not listing position 0). Only the focus read-out
    /// (`card_probe`) asks; the default is a source that holds its listing whole.
    fn global(&self, i: usize) -> usize {
        i
    }
    /// Whether a hold on card `i` is a [`CardEvent::Hold`].
    fn holdable(&self, _i: usize) -> bool {
        true
    }
    /// Whether the store has more cards beyond `len()`; only then are [`CardEvent::Want`]s emitted.
    fn more(&self) -> bool {
        false
    }
    /// How many page reads have LANDED for this row, a failed one not counting. A landing can change
    /// nothing the shelf sees (a row that rescans its listing for keys it already knew leaves
    /// `(len, end)` as it was), and the shelf must not read that as an ask nobody answered: when
    /// this moves, the ask that was out is answered, the repeat ladder starts over and the next ask
    /// goes out at once. Defaults to a count that never moves (a source whose landings always change
    /// its length).
    fn page_epoch(&self) -> u32 {
        0
    }
    /// A number that moves whenever the list of cards (their elements, order or length) may have
    /// changed, or `None` for a source that cannot say: an animated landing
    /// ([`Shelf::animate_landings`]) then compares every card each tick instead of only when the
    /// length or this moved.
    fn revision(&self) -> Option<u64> {
        None
    }
    /// Whether the list goes on past the LAST card, for the placeholders a [`Shelf`] draws there
    /// until the page lands. [`more`](Self::more) unless the source gates that on where focus is
    /// (Home and the Library ask for a page only near the edge, but a row that continues
    /// continues whoever is looking).
    fn more_after(&self) -> bool {
        self.more()
    }
    /// Whether the list goes on before the FIRST card (the window slid off its start): the
    /// leading edge's counterpart of [`more_after`](Self::more_after).
    fn more_before(&self) -> bool {
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
    /// The source needs the cards in `range`: the visible window (plus look-ahead) reaches past
    /// `len`, or (in a grid) a card in view is still unread, a range inside `len`. Emitted from
    /// Tick, and again whenever the retry ladder is due while the need stands; an owner forwards
    /// every one.
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
    /// How far what the layout owner puts directly below this shelf's caption band (the next
    /// shelf's heading) is DRAWN above where the layout rests it, in px: the next shelf's
    /// `heading_lift()`. A caption line is drawn only where that thing is not
    /// (`card_row::line_reveal_for`). 0 for a shelf with nothing under it, or one whose neighbour
    /// does not lift. A [`Stack`] reads it off the next section that takes room, so stacked shelves
    /// are adjacent sections with the air under each as its [`StackPage::shelf_foot`], not a `Custom`
    /// gap between two.
    pub below_rise: f32,
}

impl SectionFrame {
    /// A frame whose neighbour below does not lift (nothing under the shelf, or a resting block).
    pub const fn new(y: f32, clip: Rect) -> Self {
        Self { y, clip, below_rise: 0.0 }
    }
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
    /// A focus change was announced since the last tick (anything but [`Seen::Nothing`]).
    pub(crate) fn announced(self) -> bool {
        self != Seen::Nothing
    }

    pub(crate) fn of(by: &crate::screen::By, arrived: usize) -> Self {
        if matches!(by, crate::screen::By::Dir | crate::screen::By::Pointer) { Seen::Deliberate(arrived) } else { Seen::Other }
    }
}

/// The focused element's card index changed because the CONTENT did (an insert or a reorder
/// before it), not because focus moved: the section carried its pop to `to`, snapped the tile it
/// left to rest and shifted its scroll so the tile stayed where it was on screen. Readable through
/// [`Shelf::landed`] / [`Grid::landed`] for the tick that did it, so an owner that scrolls a page
/// around the section (a `Stack`) can shift by the same amount.
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
    src.focus_index(&key.elem)
}

/// **Is card `i` the element a surface lifts out of the dim this frame?** Then the page pass leaves
/// it out (`DrawFrame::lifted`): the opener lift draws the one live copy above the host snapshot,
/// so the snapshot never holds the card and the press spring moving it is not page damage
/// ([`crate::popover::set_lift_owns`]). Said by both card sections' `draw_card`; the lift's own frame
/// carries no `lifted`, so it draws.
pub(crate) fn lifted_out<H: Host, S: CardSource<H>>(
    f: &DrawFrame<'_, '_, H>,
    entry: EntryId,
    src: &S,
    i: usize,
) -> bool {
    f.leaves_out(entry, src.elem(i))
}

/// The paging rule shared by both sections: ask for cards up to `end` once per `(len, end)`, and
/// again when `retry` says the ask has stood unanswered long enough ([`Retry`]).
pub(crate) fn want(asked: &mut Option<(usize, usize)>, len: usize, end: usize, more: bool, retry: bool) -> Option<Range<usize>> {
    if !more || end <= len || (*asked == Some((len, end)) && !retry) {
        return None;
    }
    *asked = Some((len, end));
    Some(len..end)
}

/// Seconds an ask that nobody answered waits before the section asks again, per ask already
/// repeated; the last step repeats for as long as the user stays at the edge. A store may refuse
/// an ask for reasons that pass (a read it withdrew is still out, a thread it could not spawn) or
/// give a page up after its own retries, and no key press follows to ask again: so an ask that has
/// not been answered is repeated, on a ladder that never gives up while the user stands there and
/// never spins (the cap is the stores' own `RETRY_MAX_S`).
const RETRY_LADDER_S: [f32; 8] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 30.0];

/// The repeat timer of a section's page ask. Runs only while an ask is out. Time is the frames'
/// own `Tick.ms`, differenced with `wrapping_sub` the way `motion::Phase` does, so it holds no
/// per-frame delta and reads no clock.
#[derive(Clone, Copy, Debug)]
pub struct Retry {
    /// `Tick.ms` of the latest tick.
    now: u32,
    /// `Tick.ms` the latest ask went out at.
    at: u32,
    n: u8,
    /// The ask the next [`sent`](Self::sent) records is a new one, not a repeat.
    fresh: bool,
}

impl Retry {
    pub const fn new() -> Self {
        Self { now: 0, at: 0, n: 0, fresh: false }
    }

    pub fn tick(&mut self, ms: u32) {
        self.now = ms;
    }

    /// Whether the ask out has gone unanswered for this rung of the ladder.
    pub fn due(&self) -> bool {
        let rung = RETRY_LADDER_S[usize::from(self.n).min(RETRY_LADDER_S.len() - 1)];
        self.now.wrapping_sub(self.at) as f32 >= rung * 1000.0
    }

    pub fn mark_fresh(&mut self) {
        self.fresh = true;
    }

    /// An ask went out: a new one starts the ladder over, a repeat climbs a rung.
    pub fn sent(&mut self) {
        self.n = if std::mem::take(&mut self.fresh) { 0 } else { self.n.saturating_add(1) };
        self.at = self.now;
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

/// The index of the card the press owns in section `entry`, resolved ONCE per pass the way focus is
/// ([`focused_index`]): through [`CardSource::index_of`], so an owner that names no card of this
/// source (another page, an element the source has not interned yet) dips nobody. Comparing indexes
/// instead of `src.elem(i)` per card keeps the pass free of per-card element lookups, and a
/// card whose element falls back to a shared id (a header press) can never match by that fallback.
pub(crate) fn pressed_index<H: Host, S: CardSource<H>>(cx: &Cx<'_, H>, entry: EntryId, src: &S) -> Option<usize> {
    let key = cx.press.owner.filter(|k| k.entry == entry)?;
    src.index_of(&key.elem)
}

/// The scale card `i` paints at: its pop times the frame's press dip IF it is the pressed card
/// (`pressed`, from [`pressed_index`]). The dip follows the press's owner, not focus: an abandoned
/// press springs back on the card that was pressed while its neighbour takes the focus pop, and
/// neither card's size may jump on the frame focus moves.
#[inline]
pub(crate) fn press_scale<H: Host>(pop: f32, i: usize, pressed: Option<usize>, cx: &Cx<'_, H>) -> f32 {
    pop * if pressed == Some(i) { cx.press.dip() } else { 1.0 }
}

/// `r` (screen space) in painter `p`'s space: the painter's own translate undone, so a rect drawn
/// or registered through `p` lands where `r` says on the screen.
#[inline]
pub(crate) fn to_local(p: Painter, r: Rect) -> Rect {
    Rect::new(r.x, r.y - p.dy(), r.w, r.h)
}
