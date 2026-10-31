//! [`Shelf`] — one horizontal strip of cards (a person's filmography shelf, a related row).
//!
//! It wraps the L0 [`CardRow`] (a spring per cell plus the scroll and label-band springs) and adds
//! what the row deliberately leaves to its caller: reading focus from the engine, the pop rule,
//! the draw loop with its cull and stops, `Focusable` placement and paging.

use plx_machine::machine::{
    Canon, Cx, Effects, Enter, EntryId, FocusKey, GroupId, Host, ScreenEvent,
};
use plx_machine::present::Provenance;

use super::landing::{Landing, Scene};
use super::pool::{self, Drawn, RowPool};
use super::{CardEvent, CardSource, Landed, Retry, SectionFrame, Seen, Tile};
use crate::card_row::{self, CardRow, RowStyle, TileLabel};
use crate::consts::SCR_W;
use crate::screen::{
    Activate, At, AxisMask, By, DrawFrame, Dir, EdgeRule, ElemKind, GroupKind, GroupSpec, Placed, Seat, Step,
    Stop,
};
use crate::widgets::Art;
use crate::{Painter, Rect};

/// Cards past the last visible one the source is asked for.
const LOOK_AHEAD: usize = 6;

/// Cards the shelf keeps requested beyond the painted ones, in the direction of travel: a bound
/// on WORK (the walk and the requests it can make), not on what the row can reach. About one
/// screenful, so a held key on a slow server finds the next few cards already on their way. The
/// poster source decides how many of them may be in flight (`PREFETCH_OUTSTANDING_MAX`).
pub(super) const WARM_AHEAD_CARDS: usize = 8;

/// Cards from either end of a row inside which focus counts as near that edge, for
/// [`Shelf::page_ask`].
pub const PAGE_EDGE_CARDS: usize = 6;

/// The edge of a row focus is near, when the owner should ask for the neighbouring page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageEdge {
    Before,
    After,
}

/// The focus read-out for card `i` of `src`, drawn at `rect`: its slot in the held window and its
/// place in the whole listing the window is part of (`plxnative-focusx`).
#[cfg(feature = "devtriggers")]
pub(super) fn note_focused<H: Host, S: CardSource<H>>(pr: Painter, rect: Rect, i: usize, src: &S) {
    let at = pr.to_screen(rect).0;
    crate::card_probe::focused(at.x, at.y, at.w, i, src.len(), src.global(i));
}

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
    asked: Option<(usize, usize)>,
    asked_before: Option<usize>,
    /// The edge a page ask handed to the owner is out for, until it lands or is cancelled
    /// ([`Shelf::page_cancel`]).
    in_flight: Option<PageEdge>,
    /// When the ask out is repeated if nothing answered it ([`Retry`]).
    retry: Retry,
    /// The source's landing count when the shelf last looked ([`CardSource::page_epoch`]).
    epoch: u32,
    /// Cards at the row's start that are not part of the window (a pinned head the window's offset
    /// does not count): the leading edge's zone starts after them ([`Shelf::set_head`]).
    head: usize,
    /// The element the last tick had focused (`pool::key`), in the cell the row recorded: what
    /// tells a landing that moved the cards from a focus that moved over them.
    anchor: Option<(usize, u64)>,
    /// How far past the screen edge a card still paints and registers a stop (default 0).
    margin: f32,
    /// The page is still dissolving in ([`dormant`](Shelf::dormant)): no card is lifted yet, unless
    /// the shelf is `returning` with the seat ([`asleep`](Shelf::asleep)).
    dormant: bool,
    /// The last tick ran asleep, so the next awake one starts the focused card's pop from rest.
    slept: bool,
    /// The page is RETURNING: it was restored (`Enter(Restored)`: BACK onto it, a modal's dismissal),
    /// so the seat the engine restores comes back as it was left and is not put to sleep
    /// ([`dormant`](Shelf::dormant)). It ends on the first tick the owner reports awake, at the first
    /// deliberate move and when focus lands anywhere but a card of this shelf: the reader has moved
    /// on, and the move is slept through like an entry's. Transient, like `dormant` and `slept`.
    returning: bool,
    /// The element-keyed pop pool (`pool.rs`).
    pool: RowPool,
    /// Where the artwork warm-ahead stands ([`warm_ahead`](Shelf::warm_ahead)).
    warm: Warm,
    /// The animated landing of inserted and removed cards (`landing.rs`); off unless the owner
    /// asks ([`animate_landings`](Shelf::animate_landings)).
    landing: Landing,
}

/// Render history of the shelf's artwork warm-ahead: never canonised, restored or compared.
#[derive(Clone, Copy)]
struct Warm {
    /// The focused index the last tick saw, which names the direction of travel.
    last: Option<usize>,
    /// Travelling toward higher indexes (a shelf starts that way).
    forward: bool,
    /// The walk already made for one view of the row: `(which, scanned)`. `scanned` cards from the
    /// nearest are held by the source, so the next tick resumes there instead of asking again.
    walk: Option<(WalkKey, usize)>,
}

/// What a walk is a walk of: the same key means the same cards in the same order.
#[derive(Clone, Copy, PartialEq, Eq)]
struct WalkKey {
    edge: usize,
    forward: bool,
    len: usize,
    /// The element at the edge: a window that slid under an unchanged edge index changes it.
    head: u64,
}

impl Shelf {
    pub const fn new(entry: EntryId, style: &'static RowStyle) -> Self {
        Self { entry, style, row: CardRow::new(), seen: Seen::Nothing, landed: None, left: false, asked: None, asked_before: None, in_flight: None, retry: Retry::new(), epoch: 0, head: 0, anchor: None,
            margin: 0.0, dormant: false, slept: false, returning: false, pool: RowPool::new(),
            warm: Warm { last: None, forward: true, walk: None }, landing: Landing::new() }
    }

    /// Play a content landing no focus move announced over frames (`landing.rs`): a removed card
    /// fades out where it stood ([`paint_ghosts`](Self::paint_ghosts)), the cards after it slide
    /// into the gap and an inserted card fades in. The focused tile is held at its slot by the
    /// landing rule. Off by default: a section that keeps no removed cards for the ghosts still gets
    /// the slide and the fade-in, but its removed cards would cut.
    pub const fn animate_landings(mut self, on: bool) -> Self {
        self.landing.set_enabled(on);
        self
    }

    /// A landing is playing: an owner that keeps the cards it removed for
    /// [`paint_ghosts`](Self::paint_ghosts) keeps them while this holds.
    pub fn landing_pending(&self) -> bool {
        self.landing.pending()
    }

    /// What a landing is doing to card `i` of `n` right now: `(x offset px, alpha)`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn landing_probe(&self, n: usize, i: usize) -> (f32, f32) {
        (self.landing.offset(n, i), self.landing.alpha(n, i))
    }

    /// The ghosts a landing is fading out, as the indexes they had before it, and the most opaque
    /// one's alpha.
    #[cfg(any(test, feature = "test-support"))]
    pub fn ghost_probe(&self) -> (Vec<usize>, f32) {
        let a = self.landing.ghosts().iter().map(|g| g.alpha.pos.clamp(0.0, 1.0)).fold(0.0, f32::max);
        (self.landing.ghosts().iter().map(|g| g.was).collect(), a)
    }

    /// Each ghost's alpha right now, in [`ghost_keys`](Self::ghost_keys) order.
    #[cfg(any(test, feature = "test-support"))]
    pub fn ghost_alphas(&self) -> Vec<f32> {
        self.landing.ghosts().iter().map(|g| g.alpha.pos.clamp(0.0, 1.0)).collect()
    }

    /// Each ghost's pop scale right now, in [`ghost_keys`](Self::ghost_keys) order.
    #[cfg(any(test, feature = "test-support"))]
    pub fn ghost_scales(&self) -> Vec<f32> {
        self.landing.ghosts().iter().map(|g| g.lift.pos).collect()
    }

    /// The elements ([`elem_key`](super::elem_key)) of the cards a landing is fading out, in the
    /// order [`paint_ghosts`](Self::paint_ghosts) reads its ghost source by: the owner looks each
    /// removed card up and hands the lot back.
    pub fn ghost_keys(&self) -> impl Iterator<Item = u64> + '_ {
        self.landing.ghosts().iter().map(|g| g.key)
    }

    /// Whether [`paint_ghosts`](Self::paint_ghosts) has a card to draw this frame.
    pub fn has_ghosts(&self) -> bool {
        self.landing.has_ghosts()
    }

    /// Cards this far past the screen's leading (left) edge still paint (a popped card's shadow
    /// reaches onto the screen from beyond it) and register their stops; the trailing edge is not
    /// widened. Default 0.
    pub const fn cull_margin(mut self, px: f32) -> Self {
        self.margin = px;
        self
    }

    /// Hold every card at rest while the owner's page is still dissolving in: a lifted tile under
    /// a fade plays its selection where nobody can see it. The shelf keeps no pop meanwhile; the
    /// first awake tick starts the focused card's pop from rest, as a deliberate move does (a restore
    /// or reconcile announced since the last dormant tick is still adopted whole). Set it before each
    /// [`on`](Self::on).
    ///
    /// What was lifted lets go in one of two ways, by whether the page can be seen. On a page in
    /// view (Home's shelves while its dive has not reached them, the Recommended page's while its
    /// shelves dissolve in) the springs EASE to rest like an unfocused shelf's and park once they
    /// settle. While the page is a held transition image
    /// ([`page_image_held`](crate::containers::transition::page_image_held): a dip's Out and In
    /// halves and the hold after it) nothing on screen can show the ease, and stepping it would hold
    /// the image past its dissolve (a page that is covered freezes its shelves mid-ease and resumes
    /// them under the BACK dip's image), so the springs and the caption band jump to rest on the
    /// first tick, as a [`Grid`](super::Grid) put to sleep by [`Grid::dormant`](super::Grid::dormant)
    /// does. The scroll is kept either way.
    ///
    /// **The seat of a page that returns is not put to sleep.** A shelf fed `Enter(Restored)` keeps
    /// the card the engine restores focus to lifted and its caption band open, as it was left, under
    /// the very fade or held image the owner reports (BACK from a Detail runs under its dip's
    /// image): a card put at rest and grown again there moves the shelf below it by the band's
    /// height, which reads as a bounce. Only the shelf that holds the seat keeps it; a page holds
    /// many shelves and every one is fed the `Enter`, and the rest are put to rest as any dormant
    /// shelf is. The return is the shelf's own state, so the owner says nothing more than this flag:
    /// it ends on the first tick that reports awake, at the first deliberate move and when focus
    /// lands anywhere but a card of this shelf (that move is slept through, and no band is left
    /// closing under the image). A landing is adopted whole while dormant, a returning shelf
    /// included, and so is a scroll glide the covered page froze under way: nothing plays under the
    /// image.
    pub fn dormant(&mut self, held: bool) {
        self.dormant = held;
    }

    /// Whether the shelf keeps no lift now: the owner says the page is unseen, and the shelf is not
    /// returning with the seat (`seated`: the focused card, when focus is on one of this shelf's).
    fn asleep(&self, seated: Option<usize>) -> bool {
        self.dormant && !(self.returning && seated.is_some())
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
                self.retry.tick(t.ms);
                self.observe_landing(src);
                self.tick(t.dt(), cx, src);
                if self.landing.held() {
                    // a landing waits under a held page image: a frame must follow its release
                    fx.invalidate(Provenance::Lifecycle);
                }
                self.warm_ahead(cx, src);
                self.want(cx, src)
            }
            ScreenEvent::FocusMoved { from, to, by } => {
                let here = |e: &H::Elem| src.index_of(e);
                let arrived = (to.entry == self.entry).then(|| here(&to.elem)).flatten();
                // the reader has moved on from what the page returned with: a deliberate move, or
                // focus gone from this shelf's cards (a header control, another shelf)
                if arrived.is_none() || matches!(by, By::Dir | By::Pointer) {
                    self.returning = false;
                }
                if let Some(a) = arrived {
                    self.seen = Seen::of(by, a);
                    // a key's move along the row brings it back to its focus; a hover does not
                    if *by != By::Pointer {
                        self.row.unfree();
                    }
                    if self.returning && self.dormant {
                        // The seat returns to a row the page shut when it covered it (a shelf put to
                        // rest under the push's image): its scroll, band and heading settle with it,
                        // now, so the layout the owner reads on this very event (the page drawn this
                        // frame is the one the dip captures) already has them, not a tick after, and
                        // no glide plays on under the image.
                        self.row.settle_seat(a, src.len(), self.style);
                    }
                }
                let left = from.filter(|k| k.entry == self.entry).and_then(|k| here(&k.elem));
                self.left |= left.is_some();
                if arrived.is_some() || left.is_some() {
                    fx.invalidate(Provenance::Input);
                }
                None
            }
            ScreenEvent::Enter(Enter::Restored) => {
                // what the covered page slept through is stale: its seat comes back as it was left
                self.returning = true;
                self.slept = false;
                None
            }
            ScreenEvent::PressCommit(_) => {
                self.focused_elem(cx, src).map(|(_, e)| CardEvent::Activate(e))
            }
            ScreenEvent::PressHold(_) => self
                .focused_elem(cx, src)
                .filter(|&(i, _)| src.holdable(i))
                .map(|(_, e)| CardEvent::Hold(e)),
            // A click on one of the row's cards brings the row back to its focus (the card).
            ScreenEvent::Input(input) => {
                if let plx_machine::machine::InputKind::Click { hit: Some(hit), .. } = &input.kind {
                    if src.index_of(hit).is_some() {
                        self.row.unfree();
                        fx.invalidate(Provenance::Input);
                    }
                }
                None
            }
            // A covering menu changes nothing here: the page keeps its focus and the settled pop is
            // what the opener redraw reads.
            _ => None,
        }
    }

    fn focused_elem<H: Host, S: CardSource<H>>(&self, cx: &Cx<'_, H>, src: &S) -> Option<(usize, H::Elem)> {
        super::focused_index(&cx.focus, self.entry, src).map(|i| (i, src.elem(i)))
    }

    fn tick<H: Host, S: CardSource<H>>(&mut self, dt: f32, cx: &Cx<'_, H>, src: &S) {
        let seated = super::focused_index(&cx.focus, self.entry, src);
        if !self.dormant {
            self.returning = false;
        }
        let asleep = self.asleep(seated);
        let focus = seated.filter(|_| !asleep);
        self.landed = None;
        let sx0 = self.row.scroll_x();
        // what the focused card was drawn at before this tick lets it go: if the landing removes
        // it, its ghost starts there
        let lifted = self.prev().map(|p| (p, self.row.scale(p)));
        if asleep {
            // nothing is lifted under the fade, and whatever was lets go like an unfocused shelf's
            self.slept = true;
            self.pool.clear();
            self.seen = Seen::Nothing;
            self.left = false;
            self.anchor = None;
            self.landing.snap(src);
            if !self.row.at_exact_rest() {
                if crate::containers::transition::page_image_held() {
                    // an image hides the page: nothing may step under it (it would hold the image
                    // past its dissolve), so what was lifted is at rest now
                    self.row.snap_to_rest();
                } else {
                    self.row.update(src.len(), None, self.style, dt);
                    self.row.park();
                }
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
        let carried = self.pool.follow(&mut self.row, src, focus);
        // A landing and a focus change between the same two ticks (a held key over a paged row, the
        // window sliding under it; a restore or reconcile landing with a page): the change alone
        // would leave the scroll where the OLD window had it, a window away from every card that
        // stayed. The card focus was on is carried to where the source now shows it, scroll and
        // all, at any distance, and the change then starts from there.
        if let (Some(i), true, Some((p, q))) = (focus, self.seen.announced(), pool::slid(src, self.anchor)) {
            let dx = (q as f32 - p as f32) * self.pitch();
            if self.pool.holds(q) {
                self.row.shift_scroll(dx);
            } else {
                self.row.relocate(p, q, dx);
            }
            self.row.clamp_scroll(src.len(), self.style);
            self.row.refocus(q);
            self.landed = Some(Landed { from: p, to: q });
            // a deliberate move was announced by the index it had in whichever window stood then
            if matches!(self.seen, Seen::Deliberate(_)) {
                self.seen = Seen::Deliberate(i);
            }
        }
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
        // after the landing rule above: the offsets are taken against the focused card's own move
        let (scroll, w, margin) = (self.row.scroll_x(), self.style.w, self.margin);
        let on = |x: f32, _: f32| crate::on_axis(x - scroll, w, SCR_W, margin);
        let (pitch, margin_x) = (self.pitch(), self.style.margin_x);
        let slot = |i: usize| (margin_x + i as f32 * pitch, 0.0);
        if self.dormant {
            // a page the owner reports unseen adopts a landing whole, a returning one included:
            // nothing plays under the image
            self.landing.snap(src);
        } else {
            self.landing.observe(src, &Scene { shift: (scroll - sx0, 0.0), slot: &slot, was: &slot, on: &on, lifted });
            self.landing.step(dt, (self.style.k_scroll, self.style.k_scale), None);
        }
        if focus.is_some() || !self.row.at_exact_rest() {
            self.row.update(src.len(), focus, self.style, dt);
            if focus.is_none() {
                self.row.park();
            }
        }
        self.pool.admit(&self.row, src, focus);
        self.anchor = pool::anchor_of(src, focus);
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

    pub fn has_page_request(&self) -> bool {
        self.asked.is_some() || self.asked_before.is_some() || self.in_flight.is_some()
    }

    /// Release paging demand when the owner leaves this row.
    pub fn reset_page_requests(&mut self) {
        self.asked = None;
        self.asked_before = None;
        self.retry.reset();
    }

    /// A page landed for the row since the last look, whatever it changed: the ask that was out is
    /// answered, so the next one goes out at once and the ladder starts over. Without this a landing
    /// that moved nothing the shelf sees (a rescan of known keys) would wait out a rung of the ladder
    /// as if the store had ignored it. Each landing admits one ask, so the pace is the store's own.
    fn observe_landing<H: Host, S: CardSource<H>>(&mut self, src: &S) {
        let epoch = src.page_epoch();
        if epoch != self.epoch {
            self.epoch = epoch;
            self.asked = None;
            self.asked_before = None;
        }
    }

    /// The owner withdrew the ask itself (it left the row): there is nothing for
    /// [`page_cancel`](Self::page_cancel) to withdraw. A landing does not call this: the ask it
    /// answered is simply no longer in the store, and a late cancel for it is a no-op there.
    pub fn drop_page_flight(&mut self) {
        self.in_flight = None;
        self.retry.reset();
    }

    /// The row leads with `head` cards that no window slides over (Detail's Related head): the leading
    /// edge's zone is the first [`PAGE_EDGE_CARDS`] cards after them, and focus on them counts too.
    pub fn set_head(&mut self, head: usize) {
        self.head = head;
    }

    /// Request an earlier window while its leading edge remains active: once per window, and again
    /// when the ask has stood unanswered for a rung of the [`Retry`] ladder (the store may have
    /// refused it for a reason that has passed, or given the page up). Leaving the edge or landing
    /// another offset admits a new request.
    pub fn want_before(&mut self, offset: usize, active: bool) -> bool {
        if !active {
            self.asked_before = None;
            return false;
        }
        if self.asked_before == Some(offset) {
            return self.in_flight == Some(PageEdge::Before) && self.retry.due();
        }
        self.asked_before = Some(offset);
        self.retry.mark_fresh();
        true
    }

    /// The page the owner should ask for this tick, if focus is near one edge of the row (or, with
    /// focus elsewhere, the row paints a pending cell in view, [`pending_in_view`](Self::pending_in_view)): one per
    /// window per edge, and the same one again each time it has stood unanswered for a rung of the
    /// [`Retry`](super::Retry) ladder. `wanted` is whether this tick's [`on`](Self::on) returned a
    /// [`CardEvent::Want`](super::CardEvent::Want) (the trailing edge's latch lives there), `offset`
    /// is the window the source shows (`0` when it has nothing before it), and `active` is whether the
    /// owner shows the row at all. The owner maps the edge to its own store command, and asks
    /// [`reset_page_requests`](Self::reset_page_requests) when focus leaves the row.
    pub fn page_ask<H: Host, S: CardSource<H>>(
        &mut self,
        cx: &Cx<'_, H>,
        src: &S,
        wanted: bool,
        offset: usize,
        active: bool,
    ) -> Option<PageEdge> {
        self.observe_landing(src);
        let focus = self.engine_focus(cx, src);
        let before = self.near(PageEdge::Before, focus, src, offset);
        let edge = if self.want_before(offset, active && before) {
            Some(PageEdge::Before)
        } else {
            let after = self.near(PageEdge::After, focus, src, offset);
            (active && wanted && after && !before).then_some(PageEdge::After)
        };
        if edge.is_some() {
            self.in_flight = edge;
            self.retry.sent();
        }
        edge
    }

    /// The engine's focus as an index into the source, not the picture's: a hero dive keeps the card
    /// it came from lifted, and the paging follows the card the engine holds.
    fn engine_focus<H: Host, S: CardSource<H>>(&self, cx: &Cx<'_, H>, src: &S) -> Option<usize> {
        cx.focus.current.filter(|k| k.entry == self.entry).and_then(|k| src.index_of(&k.elem))
    }

    /// Whether the row is inside the zone of `edge` that asks for a page there: focus index `focus`
    /// is near that edge, or (the trailing edge, focus elsewhere) the row paints a pending cell in
    /// view ([`pending_in_view`](Self::pending_in_view)).
    fn near<H: Host, S: CardSource<H>>(&self, edge: PageEdge, focus: Option<usize>, src: &S, offset: usize) -> bool {
        match edge {
            PageEdge::Before => offset > 0 && focus.is_some_and(|i| i < self.head.saturating_add(PAGE_EDGE_CARDS)),
            PageEdge::After => match focus {
                Some(i) => i.saturating_add(PAGE_EDGE_CARDS) >= src.len(),
                None => self.pending_in_view(src),
            },
        }
    }

    /// Whether the cell past the last card falls within the screen's WIDTH at the row's scroll while
    /// the list goes on there ([`CardSource::more_after`]): the row paints the skeleton of a card
    /// there ([`paint_pending`](Self::paint_pending)), and that skeleton is a promise the row keeps
    /// by asking for the page, focused or not. A row whose cards are shorter than the screen would
    /// otherwise stand on screen with a screenful of skeletons until somebody focused it (Detail's
    /// Related head, cut short when a member movie's collection takes its previews). Horizontal
    /// only: the shelf does not know where its owner places it vertically, so a short row below
    /// the fold asks too (one page, on the page's open). A row whose cards reach past the screen
    /// has no such cell and still asks only as focus nears its end.
    fn pending_in_view<H: Host, S: CardSource<H>>(&self, src: &S) -> bool {
        let x = self.style.margin_x + src.len() as f32 * self.pitch() - self.row.scroll_x();
        src.more_after() && crate::on_axis(x, self.style.w, SCR_W, self.margin)
    }

    /// The edge whose page ask the owner must withdraw this tick, if any: one handed out by
    /// [`page_ask`](Self::page_ask) that has not landed while focus has left that edge's zone (or
    /// the owner stopped showing the row). Call it every frame before `page_ask`, with the same
    /// `offset` and `active`, and cancel the owner's store ask for the edge it names.
    ///
    /// A page slides the window by half of it, keeping the half on focus's side of the edge, so a
    /// landing is only safe while focus is still in that half. An ask is sent with focus at most
    /// [`PAGE_EDGE_CARDS`] from the edge, which leaves that many presses of margin; withdrawing it on
    /// the first press out of the zone keeps a landing that is already on its way (the owner acts on a
    /// view one landing old, and the cancel takes a frame to reach the store) well inside the margin.
    /// The row asks again when focus returns to the edge. An ask sent with focus out of the row
    /// ([`pending_in_view`](Self::pending_in_view)) stands while focus stays out and the cell is on
    /// the axis; it is withdrawn if focus enters the row farther than [`PAGE_EDGE_CARDS`] from its
    /// end, and asked again as focus nears the end. The margin argument covers only the focused kind.
    pub fn page_cancel<H: Host, S: CardSource<H>>(&mut self, cx: &Cx<'_, H>, src: &S, offset: usize, active: bool)
        -> Option<PageEdge> {
        let edge = self.in_flight?;
        let focus = self.engine_focus(cx, src);
        if active && self.near(edge, focus, src, offset) { return None; }
        self.reset_page_requests();
        self.in_flight = None;
        Some(edge)
    }

    /// The paging rule: the window's last card plus look-ahead (or the focused card's, if further).
    fn want<H: Host, S: CardSource<H>>(&mut self, cx: &Cx<'_, H>, src: &S) -> Option<CardEvent<H::Elem>> {
        let pitch = self.style.w + self.style.gap;
        let window = ((self.row.scroll_x() + SCR_W - self.style.margin_x) / pitch).ceil().max(0.0) as usize;
        let focus = super::focused_index(&cx.focus, self.entry, src).map_or(0, |i| i + 1);
        let end = window.max(focus).saturating_add(LOOK_AHEAD);
        let retry = self.in_flight == Some(PageEdge::After) && self.retry.due();
        let was = self.asked;
        let ask = super::want(&mut self.asked, src.len(), end, src.more(), retry);
        if ask.is_some() && self.asked != was { self.retry.mark_fresh(); }
        ask.map(CardEvent::Want)
    }

    /// Request the artwork of the cards the focus is travelling toward, before they scroll in.
    ///
    /// A card asks for its art when it is first drawn on-axis, so a held key on a slow server
    /// outruns the fetches and shows skeletons. This asks [`WARM_AHEAD_CARDS`] cards beyond the
    /// painted ones in the direction of travel (the cards a window slide just landed there
    /// included) through the Library's mechanism, `tex::warm_ahead_on`: the source admits the
    /// request only while no drawn card is waiting and one speculative fetch is out, and claims
    /// a slot only from outside the cards most recently drawn. So the visible cards' own requests
    /// are never queued behind this, and nothing on screen is evicted for it.
    ///
    /// Bounded work: one claim per tick, nearest card first, at most `2 * WARM_AHEAD_CARDS`
    /// lookups, and none at all once the walk for this view of the row is complete.
    fn warm_ahead<H: Host, S: CardSource<H>>(&mut self, cx: &Cx<'_, H>, src: &S) {
        let seated = super::focused_index(&cx.focus, self.entry, src);
        let focus = seated.filter(|&i| !self.asleep(seated) && i < src.len());
        let Some(focus) = focus else {
            self.warm = Warm { last: None, forward: self.warm.forward, walk: None };
            return;
        };
        // A landing moves the index under an unmoved focus: it says nothing about direction.
        if let (Some(last), None) = (self.warm.last, self.landed) {
            if last != focus {
                self.warm.forward = focus > last;
            }
        }
        self.warm.last = Some(focus);
        let n = src.len();
        let forward = self.warm.forward;
        let pitch = self.pitch();
        let sx = self.row.scroll_x();
        // the first card past the painted run on each side (a card is painted while any of it shows)
        let first_shown = ((sx - self.style.margin_x + self.style.gap) / pitch).floor().max(0.0) as usize;
        let past_shown = ((sx + SCR_W - self.style.margin_x) / pitch).ceil().max(0.0) as usize;
        // Cards between the painted run and the focus are about to scroll in too (the scroll
        // lags a held key), so the walk starts at the painted edge, not at the focus.
        let (edge, lag) = if forward {
            (past_shown, (focus + 1).saturating_sub(past_shown))
        } else {
            (first_shown, first_shown.saturating_sub(focus))
        };
        let span = lag + WARM_AHEAD_CARDS;
        let span = span.min(2 * WARM_AHEAD_CARDS);
        let head = if edge < n { pool::key(&src.elem(edge)) } else { 0 };
        let key = WalkKey { edge, forward, len: n, head };
        let from = match self.warm.walk {
            Some((k, scanned)) if k == key => scanned,
            _ => 0,
        };
        for at in from..span {
            // nearest first: forward walks up from the edge, backward walks down from just behind it
            let Some(i) = (if forward { Some(edge + at) } else { edge.checked_sub(at + 1) }).filter(|&i| i < n) else {
                break;
            };
            if !src.loaded(i) {
                continue;
            }
            let Some((srv, path, w, h)) = crate::widgets::card_art_request(&src.art(i)) else { continue };
            if crate::tex::warm_ahead_on(srv, path, w, h, false) != crate::tex::Warm::Known {
                // claimed (the next tick finds it held and goes on) or refused (try again there)
                self.warm.walk = Some((key, at));
                return;
            }
        }
        self.warm.walk = Some((key, span));
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
        if focus.is_some_and(|_| !self.asleep(focus)) {
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
        let Some(j) = focus.filter(|_| !self.asleep(focus)) else { return self.row.scale(i) };
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

    /// Card `i`'s settled (unpopped) rect in SCREEN space, a landing's slide folded in (`n` cards).
    fn slot(&self, i: usize, n: usize, at: SectionFrame, sx: f32) -> Rect {
        let r = card_row::tile_rect(i, self.style.margin_x, self.pitch(), sx, at.y, (self.style.w, self.style.h));
        Rect::new(r.x + self.landing.offset(n, i), r.y, r.w, r.h)
    }

    /// The settled, unpopped rect of the shelf's first card in SCREEN space: where the strip
    /// starts, for a screen that registers the shelf's group extent there.
    pub fn head(&self, at: SectionFrame) -> Rect {
        self.slot(0, 0, at, self.row.scroll_x())
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
        let pressed = super::pressed_index(cx, self.entry, src);
        let slot = self.slot(i, src.len(), at, self.row.scroll_x());
        let s = match how {
            At::Drawn => super::press_scale(self.pop(src, i, focus), i, pressed, cx),
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
        let pressed = super::pressed_index(f.cx, self.entry, src);
        let sx = self.row.scroll_x();
        let pr = p.translate(-sx, 0.0);
        let visible = |i: usize| crate::on_axis(self.slot(i, n, at, sx).x, self.style.w, SCR_W, self.margin);
        for i in (0..n).filter(|&i| focus != Some(i) && visible(i)) {
            let s = super::press_scale(self.pop(src, i, focus), i, pressed, f.cx);
            self.draw_card(f, pr, src, i, at, s, false);
        }
        self.paint_pending(pr, src, at, sx);
    }

    /// The cells past an edge the list goes on beyond ([`CardSource::more_after`] /
    /// [`more_before`](CardSource::more_before)) hold the skeleton of a card whose poster has not
    /// loaded, so a row that is still loading does not look finished. They are cells of the same
    /// grid the cards sit in: the page that lands takes them over in place. Nothing registers a
    /// stop for them, so focus cannot land on one, and nothing is requested for them or kept in
    /// the pool; a screenful of cells at most is drawn, and only the ones on screen.
    fn paint_pending<H: Host, S: CardSource<H>>(&self, pr: Painter, src: &S, at: SectionFrame, sx: f32) {
        let (trailing, leading) = (src.more_after(), src.more_before());
        if !trailing && !leading {
            return;
        }
        let pitch = self.pitch();
        let cells = (SCR_W / pitch).ceil() as usize + 1;
        let n = src.len();
        let y = at.y - pr.dy();
        let cell = |index: f32| {
            let rect = Rect::new(self.style.margin_x + index * pitch, y, self.style.w, self.style.h);
            if crate::on_axis(rect.x - sx, rect.w, SCR_W, self.margin) {
                card_row::draw_tile(pr, Art::Poster(None), rect, 1.0, self.style, None);
            }
        };
        if trailing {
            (0..cells).for_each(|k| cell((n + k) as f32));
        }
        if leading {
            (1..=cells).for_each(|k| cell(-(k as f32)));
        }
    }

    /// The cards a landing removed, fading out where they stood: paint them BEFORE
    /// [`paint_resting`](Self::paint_resting), so the cards that slid in over their slots cover
    /// them. `ghosts` holds the removed cards in [`ghost_keys`](Self::ghost_keys) order (only `art`,
    /// `progress`, `loaded` and `overlay` are read; a ghost is never captioned or registered as a
    /// stop; one that was the focused card eases from its lifted scale to rest while it fades).
    /// Nothing is drawn when no landing is playing, and only what is on-axis.
    pub fn paint_ghosts<H: Host, G: CardSource<H> + ?Sized>(
        &self, f: &DrawFrame<'_, '_, H>, p: Painter, ghosts: &G, at: SectionFrame,
    ) {
        if self.dormant {
            return;
        }
        let sx = self.row.scroll_x();
        let pr = p.translate(-sx, 0.0);
        for (j, g) in self.landing.ghosts().iter().enumerate().filter(|(j, _)| *j < ghosts.len() && ghosts.loaded(*j)) {
            let a = g.alpha.pos.clamp(0.0, 1.0);
            let x = self.style.margin_x + g.was as f32 * self.pitch() + g.shift.0;
            if a <= 0.0 || !crate::on_axis(x - sx, self.style.w, SCR_W, self.margin) {
                continue;
            }
            let s = g.lift.pos;
            let pr = pr.alpha(a);
            let rect = Rect::new(x, at.y - pr.dy(), self.style.w, self.style.h).scaled(s);
            card_row::draw_tile(pr, ghosts.art(j), rect, s, self.style, ghosts.progress(j));
            let tile = Tile { rect, scale: s, radius: self.style.tile_radius(rect, s), focused: g.focused };
            ghosts.overlay(pr, j, &tile, f.measure);
        }
    }

    /// The focused card, painted whether or not it is on-axis.
    pub fn paint_focused<H: Host, S: CardSource<H>>(&self, f: &DrawFrame<'_, '_, H>, p: Painter, src: &S, at: SectionFrame) {
        let focus = super::focused_index(&f.focus, self.entry, src);
        let pressed = super::pressed_index(f.cx, self.entry, src);
        let pr = p.translate(-self.row.scroll_x(), 0.0);
        if let Some(i) = focus.filter(|&i| i < src.len()) {
            let s = super::press_scale(self.pop(src, i, focus), i, pressed, f.cx);
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
        let pressed = super::pressed_index(f.cx, self.entry, src);
        let sx = self.row.scroll_x();
        let n = src.len();
        for i in (0..n).filter(|&i| crate::on_axis(self.slot(i, n, at, sx).x, self.style.w, SCR_W, self.margin)) {
            let s = super::press_scale(self.pop(src, i, focus), i, pressed, f.cx);
            let slot = super::to_local(p, self.slot(i, n, at, sx));
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
        let pressed = super::pressed_index(f.cx, self.entry, src);
        let s = super::press_scale(self.pop(src, i, Some(i)), i, pressed, f.cx);
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
        if !src.loaded(i) {
            return;
        }
        if super::lifted_out(f, self.entry, src, i) {
            return;
        }
        let unscrolled = self.slot(i, src.len(), SectionFrame { y: at.y - pr.dy(), ..at }, 0.0);
        let rect = unscrolled.scaled(s);
        // an arriving card fades in
        let pr = match self.landing.alpha(src.len(), i) {
            a if a < 1.0 => pr.alpha(a),
            _ => pr,
        };
        if focused {
            let label = self.focused_label(src, i, at);
            #[cfg(feature = "devtriggers")]
            crate::card_probe::begin_card();
            card_row::draw_focused(pr, src.art(i), rect, s, self.style, src.progress(i), &label, f.measure);
            #[cfg(feature = "devtriggers")]
            if !pr.is_recording() {
                note_focused(pr, rect, i, src);
            }
        } else {
            card_row::draw_tile(pr, src.art(i), rect, s, self.style, src.progress(i));
        }
        let tile = Tile { rect, scale: s, radius: self.style.tile_radius(rect, s), focused };
        src.overlay(pr, i, &tile, f.measure);
    }

    /// The label block the focused card `i` is drawn with: its source's label, revealed as far as
    /// the band has opened and the room under it holds each line ([`card_row::line_reveal_for`],
    /// the heading below it as `at.below_rise` says it is drawn), placed for where the tile settles.
    /// What [`draw_card`](Self::draw_card) paints and [`caption_lines_probe`](Self::caption_lines_probe)
    /// reads, so a test sees the ink the frame carries.
    fn focused_label<H: Host, S: CardSource<H>>(&self, src: &S, i: usize, at: SectionFrame) -> TileLabel {
        self.row.reveal(src.label(i), at.below_rise)
            .settling(self.row.settle_lag(src.len(), i, self.style))
    }

    /// Test support: every caption LINE [`draw`](Self::draw) puts on screen now for the focused card,
    /// `(card index, top y, bottom y, alpha)` in screen space for a painter with no translate and
    /// no scroll (the tests'), from the same label and boxes the draw paints
    /// ([`TileLabel::lines`](card_row::TileLabel::lines)): a line that is not drawn (alpha 0) is
    /// not listed.
    #[cfg(any(test, feature = "test-support"))]
    pub fn caption_lines_probe<H: Host, S: CardSource<H>>(&self, f: &DrawFrame<'_, '_, H>, src: &S, at: SectionFrame) -> Vec<(usize, f32, f32, f32)> {
        let Some(i) = super::focused_index(&f.focus, self.entry, src).filter(|&i| i < src.len() && src.loaded(i)) else {
            return Vec::new();
        };
        let pressed = super::pressed_index(f.cx, self.entry, src);
        let s = super::press_scale(self.pop(src, i, Some(i)), i, pressed, f.cx);
        let bottom = card_row::unscaled_bottom(self.slot(i, src.len(), at, 0.0).scaled(s), s);
        self.focused_label(src, i, at).lines().into_iter().flatten().filter(|l| l.alpha > 0.0)
            .map(|l| (i, bottom + l.top, bottom + l.bottom, l.alpha)).collect()
    }

    /// The `Focusable` neighbour of `key` inside the shelf: left and right by index, the edge
    /// otherwise (the screen's group edge rules take it from there).
    pub fn neighbour<H: Host, S: CardSource<H>>(&self, src: &S, key: FocusKey<H::Elem>, dir: Dir) -> Step<H::Elem> {
        let Some(i) = src.index_of(&key.elem) else { return Step::Edge };
        let to = match dir {
            Dir::Left => i.checked_sub(1).filter(|_| !src.gap_before(i)),
            Dir::Right => Some(i + 1).filter(|&j| j < src.len() && !src.gap_before(j)),
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
        self.row.at_exact_rest() && !self.landing.pending()
    }

    /// The scroll spring's velocity, for a diagnostic witness.
    #[cfg(feature = "devtriggers")]
    pub fn scroll_velocity(&self) -> f32 {
        self.row.scroll_velocity()
    }

    /// **The pointer's sideways glide** (`InputKind::Pan`) over this row: move it by `dx` px, held
    /// to its `n` cards, and leave it there until a key moves along it or a click lands on one of
    /// its cards ([`CardRow::glide`]).
    pub fn glide(&mut self, dx: f32, n: usize) {
        self.row.glide(dx, n, self.style);
    }

    /// Release a row the pointer's glide holds: it follows its focused card again.
    pub fn unfree(&mut self) {
        self.row.unfree();
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

    /// Follow the Under posters policy (`Some`) or the legacy two-line band (`None`, the default):
    /// the band this shelf reserves, and how much of the caption block shows, follow its height
    /// ([`crate::caption_policy::band_h`]). Only the focused shelf opens it.
    pub fn set_caption_policy(&mut self, policy: Option<crate::caption_policy::CaptionPolicy>) {
        if self.row.caption_policy() != policy {
            self.row.set_caption_policy(policy);
        }
    }

    /// The focused band's whole height under this shelf's policy ([`CardRow::band_h`]).
    pub fn band_h(&self) -> f32 {
        self.row.band_h()
    }

    /// The live label-band expansion, 0 collapsed to 1 focused: what a screen that lays its column
    /// out from the band's TRAVEL (`card_row::BAND_OPEN`, or `band_open_for(band_h)` under a policy, times this) reads.
    pub fn band_expand(&self) -> f32 {
        self.row.band_expand()
    }

    pub fn write(&self, c: &mut Canon) {
        self.row.write_motion(c);
    }
}
