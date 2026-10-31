//! [`Grid`] — a poster grid with focused captions, or fixed captions on every visible tile: six
//! `poster_grid::STYLE` columns by default, any column count / style / left edge through
//! [`GridSpec`] (`poster_grid`'s `_in` functions are its geometry). The Collection page is its first
//! adopter; [`ScrollMode::External`] serves a grid inside a document another owner scrolls.
//!
//! It owns (unless external) the scroll, the caption bands and the focus pop ([`GridBands`], [`GridPop`]) and runs
//! the same arithmetic for draw, stops, `Focusable` placement and paging, so the rect a card is
//! drawn at, the stop it registers and the rect `place` answers are one value.

use plx_machine::machine::{Canon, Cx, Effects, Enter, EntryId, FocusKey, GroupId, Host, ScreenEvent};
use plx_machine::present::{PresentEvent, Provenance};

use super::landing::{Landing, Occupancy, Scene};
use super::{CardEvent, CardSource, Landed, Retry, Seen, Tile};
use crate::card_row;
use crate::consts::{K_SCROLL, MARGIN_X, SCR_H};
use crate::card_row::RowStyle;
use super::pool::{self, Drawn, LetGoCards};
use crate::poster_grid::{self, Card, Geom, GridBand, GridBands, GridPop, COLS, MAX_GRID_BANDS, STYLE};
use crate::screen::{
    Activate, At, AxisMask, By, DrawFrame, Dir, EdgeRule, ElemKind, GroupKind, GroupSpec, Hover, Placed, Seat, Step,
    Stop,
};
use crate::{Painter, Rect, Spring};

/// Where a card stands in [`Grid::draw`]'s back-to-front order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawRole {
    /// The copy a card that changed row leaves where it stood (fading out).
    Leaving,
    /// A card at rest in its slot.
    Still,
    /// A new card (or one that changed row) fading in at its slot, under every card in flight.
    Arriving,
    /// A survivor sliding to its slot.
    Moving,
    /// The focused card.
    Focus,
}

/// Who owns the grid's vertical scroll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollMode {
    /// The grid springs its own scroll toward the row it snaps to. No screen runs it: every
    /// shipped grid is [`ScrollMode::External`], under the Library's scroll or a `Stack`'s.
    Own,
    /// The grid sits in a document whose page scroll another owner drives (the Library's All grid, every grid
    /// section of a `Stack`): the grid never steps or snaps a scroll of its own. The owner hands it the page
    /// with [`Grid::set_page`] before every `on` / `draw` / `place` call, reads the scroll the grid
    /// WANTS for its focused row from [`Grid::reveal_target`] ([`Grid::rest_target`] for a
    /// [`resting`](GridSpec::resting) grid), and the document shift a content
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
    /// the snapped one are culled against. A [`resting`](Self::resting) grid snaps to no edge: its
    /// edge is the screen's top, which culls only a row wholly off it.
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
    /// Play a content change no focus move announced instead of cutting it (see
    /// [`animate_landings`](Self::animate_landings)).
    pub landings: bool,
    /// Rows REST rather than snap (see [`resting`](Self::resting)).
    pub rests: bool,
    /// Every visible cell carries its sort value line ([`card_row::SortValue`]) and the grid's
    /// rows are [`card_row::SORT_VALUE_ROW_GAP`] apart, opening [`card_row::SORT_VALUE_OPEN`] on focus.
    pub value_line: bool,
}

impl GridSpec {
    /// A grid that scrolls home when focus leaves it.
    pub const fn new(top: f32, edge: f32) -> Self {
        Self { top, edge, home_when_unfocused: true, cols: COLS, style: STYLE, left: MARGIN_X, scroll: ScrollMode::Own, landings: false, rests: false, value_line: false }
    }

    /// **A grid whose rows rest where the first row does** (the Library's rule, `poster_grid::rest_scroll`):
    /// a focused row below the first comes to rest with its poster top at the first row's resting
    /// y (`top` at scroll 0), instead of snapping to an edge ([`new`](Self::new)). The document's end
    /// holds the last rows lower; the owner applies it ([`Grid::rest_target`], which a `Stack`
    /// calls with its own end), so this is a [`ScrollMode::External`] grid's rule. Rows above the
    /// rested one stay drawn, under the page's chrome, as on the Library: nothing is culled that
    /// the screen still shows, so the edge is the screen's top (0).
    pub const fn resting(top: f32) -> Self {
        Self { rests: true, ..Self::new(top, 0.0) }
    }

    /// The distance between two COLLAPSED rows' tops: what a page whose chrome tucks once the first
    /// row has scrolled off (`scroll_chrome::Handoff::step`'s `gone_at`) is told that distance is.
    pub fn pitch(&self) -> f32 {
        self.geom().pitch()
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

    /// Play a content landing that no move announced (`landing.rs`): a removed card fades out where
    /// it stood on `style.k_scale` ([`Grid::paint_ghosts`] paints it from the card the owner kept),
    /// every card that keeps its row slides along it on `style.k_scroll`, all together (no
    /// stagger), and a card that crosses a line break crossfades on `style.k_scale` (out where it
    /// stood, in at its new cell) instead of flying over the row between. Cards with no end on
    /// screen cut. Focus is not this spring's: the engine's reconcile has already put it on the
    /// card that took the removed one's index (the next card), whose pop rises from rest as the
    /// removed card's ghost sinks. Nothing starts while a menu veils the page: the Remove lands on
    /// the first feed after the veil drops below `CLOSING_GONE` (`landing.rs`); a menu that lets the
    /// page reflow (`Screen::lets_host_reflow`) veils nothing. The frame a landing begins in draws
    /// the start state, and the draw order is leaving copies, still cards, cards fading in, movers by the
    /// length of their way, the focused card. Off by default: a grid whose owner keeps no removed cards for
    /// the ghosts still gets the slide.
    pub const fn animate_landings(self, on: bool) -> Self {
        Self { landings: on, ..self }
    }

    fn geom(&self) -> Geom {
        Geom { value_line: self.value_line, ..Geom::of(self.cols, self.left, &self.style) }
    }
}

/// What [`Grid::card_paint`] decides for one card (see there).
struct CardPaint {
    rect: Rect,
    p: Painter,
    label: Option<super::TileLabel>,
}

pub struct Grid {
    entry: EntryId,
    spec: GridSpec,
    scroll: Spring,
    target: f32,
    bands: GridBands,
    /// The bands as the last frame drew them, kept from the moment a focus the reader did not move
    /// settled them on another row until the landing has measured against them (`play_landing`).
    bands_was: Option<[GridBand; MAX_GRID_BANDS]>,
    /// The focus pop and every let-go in flight, each keyed to its element (`pool.rs`).
    pop: GridPop,
    seen: Seen,
    /// The element the last tick had focused (`pool::key`): what tells a landing that moved the
    /// cards from a focus that moved over them, as the shelf's.
    anchor: Option<(usize, u64)>,
    landed: Option<Landed>,
    /// External mode: the document shift the last tick's landing needs (see [`Grid::landed_shift`]).
    shift: f32,
    /// The focused row and the scroll it wants (see [`Grid::reveal_target`]).
    reveal: Option<(usize, f32)>,
    ahead: usize,
    asked: Option<(usize, usize)>,
    /// The first unloaded card in view the latest hole ask named.
    hole: Option<usize>,
    /// An ask is out that nothing has answered: it is repeated on the [`Retry`] ladder.
    out: bool,
    retry: Retry,
    /// The source's [`CardSource::page_epoch`] at the last tick.
    epoch: u32,
    /// The animated landing of inserted and removed cards (`landing.rs`); off unless the spec asks.
    landing: Landing,
    /// What the focused caption band shows (the Library's Under posters setting); `None` keeps the
    /// legacy 114 px band every grid without the setting opens to.
    caption_policy: Option<crate::caption_policy::CaptionPolicy>,
    /// The owner reports the page unseen, dissolving in or shown as a held image
    /// ([`dormant`](Grid::dormant)). The grid is then asleep (no card is lifted yet) unless the
    /// page is `returning` ([`asleep`](Grid::asleep)).
    dormant: bool,
    /// The last tick ran asleep (a returning grid ticks dormant and sleeps through none), so the
    /// next awake one starts the focused card's pop from rest.
    slept: bool,
    /// The page is RETURNING: it was restored (`Enter(Restored)`: BACK onto it, a modal's dismissal,
    /// its own pill pressed again), so its seat comes back as it was left and is never put to sleep
    /// ([`dormant`](Grid::dormant)). It ends on the first tick the owner reports awake, at the first
    /// deliberate move and when focus lands anywhere but a card of this grid: the reader has moved
    /// on, and the move is slept through like an entry's. Transient, like `dormant` and `slept`: not
    /// part of the canon.
    returning: bool,
    /// The owner announced that the focus it just seated is an arrival ([`arrive`](Grid::arrive)),
    /// not a restore: the next `FocusMoved` onto this grid grows from rest. Cleared by the tick.
    arriving: bool,
}

impl Grid {
    pub fn new(entry: EntryId, spec: GridSpec) -> Self {
        Self {
            entry,
            spec,
            scroll: Spring::at(0.0),
            target: 0.0,
            bands: GridBands::new(),
            bands_was: None,
            pop: GridPop::new(),
            seen: Seen::Nothing,
            anchor: None,
            landed: None,
            shift: 0.0,
            reveal: None,
            ahead: spec.cols * 2,
            asked: None,
            hole: None,
            out: false,
            retry: Retry::new(),
            epoch: 0,
            landing: {
                let mut l = Landing::new();
                l.set_enabled(spec.landings);
                l.hold_first_frame(true);
                l
            },
            caption_policy: None,
            dormant: false,
            slept: false,
            returning: false,
            arriving: false,
        }
    }

    /// The owner is seating this grid's focus as the page's own ARRIVAL (its first poster, once the
    /// cards have landed or the page's head has moved): the `FocusMoved` it raises grows from rest
    /// on the standard lift spring, its caption band opening with it, as a deliberate move's does
    /// and as the dormant grid's waking does, instead of being adopted whole like a restore (a
    /// seat the reader left). One `FocusMoved` takes it; the tick after it clears what it left.
    pub fn arrive(&mut self) {
        self.arriving = true;
    }

    /// Hold every card at rest while the owner's page is still dissolving in (the twin of
    /// [`Shelf::dormant`](super::Shelf::dormant)): a lifted tile under a fade plays its selection
    /// where nobody can see it. The grid keeps no pop meanwhile; the first awake tick starts the
    /// focused card's pop from rest, as a deliberate move does (a restore or reconcile announced
    /// since the last dormant tick is still adopted whole). Set it before each [`on`](Self::on).
    ///
    /// **A page that returns is not put to sleep.** A grid fed `Enter(Restored)` keeps its seat
    /// lifted and its caption band open, as it was left, under the very fade or held image the
    /// owner reports (BACK from a Detail runs under its dip's image): a card put at rest and grown
    /// again there moves the row below it by the band's height, which reads as a bounce. The return
    /// is the grid's own state, so the owner says nothing more than this flag: it ends on the first
    /// tick that reports awake, at the first deliberate move and when focus leaves the grid's cards
    /// (that move is slept through, and no band is left closing under the image). Landings are
    /// adopted whole while dormant, a returning grid included: nothing plays under the image.
    pub fn dormant(&mut self, held: bool) {
        self.dormant = held;
    }

    /// Whether the grid keeps no lift now: the owner says the page is unseen and the page is not
    /// returning.
    fn asleep(&self) -> bool {
        self.dormant && !self.returning
    }

    /// Size and caption the focused band by `policy` (the Library's Under posters setting), or with
    /// `None` keep the legacy two-line band: a grid of LANDSCAPE stills (the Library's Episodes
    /// listing) is not a poster surface and is off the policy, as a landscape [`Shelf`](super::Shelf)
    /// is. A grid never given one keeps the legacy band.
    pub fn set_caption_policy(&mut self, policy: Option<crate::caption_policy::CaptionPolicy>) {
        self.caption_policy = policy;
        self.bands.set_caption_policy(policy);
    }

    /// The policy the grid's band follows, `None` while it is on the legacy band.
    pub fn caption_policy(&self) -> Option<crate::caption_policy::CaptionPolicy> {
        self.caption_policy
    }

    /// Draw every tile's sort value line, and space the rows for it.
    pub fn set_value_line(&mut self, on: bool) { self.spec.value_line = on; }

    /// The focused band's height: [`card_row::SORT_VALUE_BAND_H`] while value lines show, else the
    /// policy's (0 / 80 / 112) or the legacy band.
    pub fn band_h(&self) -> f32 {
        if self.spec.value_line { return card_row::SORT_VALUE_BAND_H; }
        self.caption_policy.as_ref().map_or(card_row::LEGACY_BAND_H, crate::caption_policy::band_h)
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
                self.tick(t.dt(), cx, src, fx);
                self.want(cx, src)
            }
            ScreenEvent::FocusMoved { from, to, by } => {
                let arrived = (to.entry == self.entry).then(|| src.index_of(&to.elem)).flatten();
                let left = from.filter(|k| k.entry == self.entry).and_then(|k| src.index_of(&k.elem));
                // A deliberate move is the only focus change the eye should see travel; a restore
                // or a reconcile is adopted whole by the next tick.
                let arrival = arrived.is_some() && std::mem::take(&mut self.arriving);
                let deliberate = arrival || matches!(by, By::Dir | By::Pointer);
                // the reader has moved on from what the page returned with: a deliberate move, or
                // focus gone from the grid's cards (a header control, a BACK that walks to one)
                if deliberate || arrived.is_none() {
                    self.returning = false;
                }
                if let Some(a) = arrived {
                    self.seen = if arrival { Seen::Deliberate(a) } else { Seen::of(by, a) };
                }
                self.focus_band(arrived.map(|i| i / self.spec.cols), deliberate);
                if let Some(cell) = arrived.filter(|_| deliberate) {
                    // the element is the one the key named, whatever index it has: the card focus
                    // left lets go as its own, and a let-go of the card named is taken up by
                    // element, even when the cards shifted since the last tick (a landing and a key
                    // in one frame can give the card named the cell the card left stood in)
                    self.pop.arm(Card { cell, key: pool::key(&to.elem) });
                }
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
        // the focused card and which element it is, hashed once: the pop is seated on it and the
        // next tick's `slid` starts from it
        let anchor = pool::anchor_of(src, focus);
        let focus_card = anchor.map(|(cell, key)| Card { cell, key });
        self.landed = None;
        self.shift = 0.0;
        let sy0 = self.scroll.pos;
        if !self.dormant {
            self.returning = false;
        }
        let awake = !self.asleep();
        self.arriving = false;
        if self.asleep() {
            // nothing is lifted under the fade, and whatever was lets go like an unfocused grid's
            self.slept = true;
            self.seen = Seen::Nothing;
            self.pop.reset();
            // A caption is its band's content (its lines are drawn only as the band opens), so the band of a seat
            // nobody has lifted is shut too: it opens with the lift below, not under the fade. Shut
            // outright and not through `focus_band`, which would keep the open layout for the
            // landing to measure from, and a landing is not what wakes. `shut` also drops a band a
            // move off the grid left closing (a returning grid's, put to sleep by that move).
            self.bands.shut();
        } else if std::mem::take(&mut self.slept) {
            // an arrival announced since the last dormant tick (a restore, a reconcile) keeps its
            // own rule; only a focus no event told the grid about wakes growing from rest, its
            // caption band opening on the spring the lift runs on, as a Down's does
            if let (Some(i), Some(card), Seen::Nothing) = (focus, focus_card, self.seen) {
                self.pop.arm(card);
                self.seen = Seen::Deliberate(i);
                self.focus_band(Some(i / self.spec.cols), true);
            }
        }
        // what the focused card was drawn at before this tick lets it go: if the landing removes
        // it, its ghost starts there
        let lifted = self.pop.cell().map(|p| (p, self.pop.pop_scale(p, &self.spec.style)));
        // A landing and a focus change between the same two ticks (a key over cards that moved, a
        // restore or reconcile landing with a page): the change alone would leave the scroll where
        // the cards WERE. The card focus was on is carried to where the source now shows it first,
        // pop and scroll, and the change then starts from there (the shelf's rule, `pool::slid`).
        let slid = focus.filter(|_| self.seen.announced()).and_then(|i| pool::slid(src, self.anchor).map(|s| (i, s)));
        pool::follow_let_gos(&mut self.pop, src, slid.map(|(_, moved)| moved));
        let mut prev = self.pop.cell();
        // where the previously focused cell was on screen, before any band moves (a deliberate move
        // has already armed the pop on its new cell: the anchor is the cell focus left)
        let was = slid.map(|(_, (p, _))| p).or(prev).map(|p| self.cell(p, &self.bands.geometry()).y);
        // A focus the reader did not move (a restore, a landing) adopts its band settled.
        // No focus is settled AFTER the landing has diffed: while the focused card leaves as a ghost
        // (focus is off it for a frame, until the app reconciles), its row's caption band stays as
        // drawn, or the cards below would jump up under the menu that chose the removal.
        if focus.is_some() && awake {
            self.focus_band(focus.map(|i| i / self.spec.cols), false);
        }
        if let Some((i, (p, q))) = slid {
            self.carry(p, q, was);
            prev = Some(q);
            if let Seen::Deliberate(_) = self.seen {
                // the pop stays on the card the move named, by the index it has in the cards that
                // stand now (the let-go the move started followed its card above: it was born with
                // its element)
                self.pop.relocate(i);
                self.seen = Seen::Deliberate(i);
            }
        }
        if let Some(card) = focus_card.filter(|_| awake) {
            let i = card.cell;
            match (self.seen, prev) {
                (Seen::Deliberate(a), _) if a == i => {}
                // no FocusMoved, another index: the same element, moved by a content landing
                (Seen::Nothing, Some(p)) if p != i => self.carry(p, i, was),
                _ if prev != Some(i) => self.pop.adopt(card, &self.spec.style),
                _ => {}
            }
        }
        self.seen = Seen::Nothing;
        self.anchor = anchor;
        // a page the owner reports unseen adopts a landing whole, a returning one included: nothing
        // plays under the fade or the image
        if self.dormant {
            self.landing.snap(src);
        } else {
            self.play_landing(src, sy0, lifted, dt);
        }
        if focus.is_none() && !self.landing.has_takeover() {
            self.focus_band(None, false);
        }
        // the focused card left as a ghost: the card focus reconciled to rises from rest as it sinks.
        // The app reconciles focus a frame after the card leaves: until it does the pop belongs to
        // no card (the ghost carries the lift), and the takeover waits for the focus.
        if self.landing.has_takeover() {
            match focus_card {
                Some(card) => {
                    self.landing.take_takeover();
                    self.pop.rise(card);
                }
                None => self.pop.vacate(),
            }
        }
        let home = if self.spec.home_when_unfocused { 0.0 } else { self.target };
        let wanted = focus.map(|i| {
            let row = i / self.spec.cols;
            let at = if self.spec.rests {
                self.rest_scroll(row, f32::INFINITY)
            } else {
                poster_grid::snap_row_in_for(self.band_h(), &self.spec.geom(), self.scroll.pos + self.shift, row, src.len(), self.spec.top, self.spec.edge)
            };
            (row, at)
        });
        self.reveal = wanted;
        if let Some(i) = focus {
            self.bands.note_card(i / self.spec.cols, i);
        }
        self.bands.tick(self.spec.style.k_scroll, dt);
        // the takeover's rise starts with the landing's springs, not under the menu that chose it
        if !(self.landing.waiting() && focus.is_some() && focus == self.pop.cell()) {
            self.pop.tick(focus_card.filter(|_| awake), &self.spec.style, dt);
        }
        if self.spec.scroll == ScrollMode::External {
            return;
        }
        self.target = wanted.map_or(home, |(_, at)| at);
        self.scroll.step(self.target, K_SCROLL, dt);
        if (self.scroll.pos - self.target).abs() > 0.25 || self.scroll.vel.abs() > 0.5 {
            fx.note(PresentEvent::Motion);
        }
    }

    /// Move the focused row of the caption bands. A band the reader did not animate settles at once;
    /// the layout it drew is kept for the landing to measure from, so the cards the band moved start where
    /// they stood and the move plays on the landing's spring (`play_landing`).
    fn focus_band(&mut self, row: Option<usize>, animate: bool) {
        if !animate && self.bands.focused() != row {
            self.bands_was.get_or_insert_with(|| self.bands.geometry());
        }
        self.bands.focus(row, animate);
    }

    /// Diff the cards against the ones last adopted and step what the change plays. `sy0` is the
    /// scroll before the landing rule moved it (own mode); an external owner's move is `self.shift`.
    /// Slots are DOCUMENT points under the caption bands as they stand now; where a card stood is
    /// measured under the bands the last frame DREW ([`bands_was`](Self::bands_was): a focus the reader did not
    /// move settled the caption band on another row), so the band's move plays on the landing's
    /// spring with the cards. Whether a card crossed a line break is judged under the bands that stand now.
    fn play_landing<H: Host, S: CardSource<H>>(&mut self, src: &S, sy0: f32, lifted: Option<(usize, f32)>, dt: f32) {
        let scroll_moved = match self.spec.scroll {
            ScrollMode::Own => self.scroll.pos - sy0,
            ScrollMode::External => self.shift,
        };
        // Whether a card is on screen is judged where the document stands AFTER this landing's move:
        // an external owner applies `self.shift` once the grid has ticked, so the scroll it handed
        // us is the one before it, and a row the move carries across the top edge (a ghost standing
        // partly over it) would otherwise be judged a whole shift off screen and cut instead of fading.
        let scroll = match self.spec.scroll {
            ScrollMode::Own => self.scroll.pos,
            ScrollMode::External => self.scroll.pos + self.shift,
        };
        let (geom, top, h, band_h) = (self.spec.geom(), self.spec.top, self.spec.style.h, self.band_h());
        let bands = self.bands.geometry();
        let slot = |i: usize| {
            let r = poster_grid::cell_in_for(band_h, &geom, i, top, 0.0, &bands);
            (r.x, r.y)
        };
        let on = |_: f32, y: f32| crate::on_axis(y - scroll, h, SCR_H, 0.0);
        // the layout the last frame drew: the caption band may have settled on another row since
        let drawn = self.bands_was.take();
        let was = |i: usize| {
            let r = poster_grid::cell_in_for(band_h, &geom, i, top, 0.0, drawn.as_ref().unwrap_or(&bands));
            (r.x, r.y)
        };
        let scene = Scene { shift: (0.0, scroll_moved), slot: &slot, was: &was, on: &on, lifted };
        let began = self.landing.observe(src, &scene);
        if drawn.is_some() && !began {
            self.landing.rebase(src.len(), &scene);
        }
        let size = {
            let r = poster_grid::cell_in_for(band_h, &geom, 0, top, 0.0, &bands);
            (r.w, r.h)
        };
        let occupy = Occupancy { slot: &slot, size };
        self.landing.step(dt, (self.spec.style.k_scroll, self.spec.style.k_scale), Some(&occupy));
    }

    /// The focused element moved from cell `p` to cell `q` because the CONTENT changed: its pop goes
    /// with it and the scroll shifts by what its row moved so the tile stays where it was on
    /// screen (`was`: where cell `p` stood before any band moved).
    fn carry(&mut self, p: usize, q: usize, was: Option<f32>) {
        self.pop.relocate(q);
        match self.spec.scroll {
            ScrollMode::Own => if let Some(was) = was {
                self.scroll.pos += self.cell(q, &self.bands.geometry()).y - was;
            },
            // The owner may have opened the new row's band before this tick, so `was`
            // would be measured under it: the rows' pitch is the shift, bands aside.
            ScrollMode::External => {
                let rows = (q / self.spec.cols) as f32 - (p / self.spec.cols) as f32;
                self.shift = rows * self.spec.geom().pitch();
            }
        }
        self.landed = Some(Landed { from: p, to: q });
    }

    /// The paging rule: the last row the scroll shows, or the focused card's look-ahead, asked for
    /// once per `(len, end)`; and any card in view (or focused) whose page is not present, asked
    /// for once per first hole. An ask nothing answers is repeated on the [`Retry`] ladder for as
    /// long as the need stands, so a read that failed, was refused or was withdrawn does not wait
    /// for a key press; a landing (the length, the first hole or [`CardSource::page_epoch`]
    /// moving) answers it and the next ask goes at once. The hole scan is the visible cells at
    /// most, a screenful.
    fn want<H: Host, S: CardSource<H>>(&mut self, cx: &Cx<'_, H>, src: &S) -> Option<CardEvent<H::Elem>> {
        let pitch = self.spec.geom().pitch();
        let cols = self.spec.cols;
        let window = ((self.scroll.pos + SCR_H - self.spec.top) / pitch).ceil().max(0.0) as usize * cols;
        let first = ((self.scroll.pos - self.spec.top) / pitch).floor().max(0.0) as usize * cols;
        let at = super::focused_index(&cx.focus, self.entry, src);
        let focus = at.map_or(0, |i| i + 1 + self.ahead);
        let (len, more) = (src.len(), src.more());
        let end = window.max(focus);
        let epoch = src.page_epoch();
        if epoch != self.epoch {
            self.epoch = epoch;
            self.asked = None;
            self.hole = None;
            self.retry.reset();
            self.out = false;
        }
        let shown = window.min(len);
        let hole = (first..shown).chain(at).find(|&i| i < len && src.unread(i));
        if hole.is_none() && !(more && end > len) {
            self.hole = None;
            self.out = false;
            self.retry.reset();
            return None;
        }
        let due = self.out && self.retry.due();
        let mut fresh = false;
        let mut ask = None;
        if let Some(h) = hole {
            if self.hole != Some(h) || due {
                fresh = self.hole != Some(h);
                self.hole = Some(h);
                ask = Some(h..shown.max(h + 1));
            }
        } else {
            self.hole = None;
        }
        if ask.is_none() {
            let was = self.asked;
            ask = super::want(&mut self.asked, len, end, more, due);
            fresh = self.asked != was;
        }
        if ask.is_some() {
            if fresh { self.retry.mark_fresh(); }
            self.retry.sent();
            self.out = true;
        }
        ask.map(CardEvent::Want)
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

    /// External mode: the shift ([`landed_shift`](Self::landed_shift)) the NEXT tick will report for
    /// the cards as they stand now. An owner that sets its scroll target between two ticks against
    /// a document that has already landed holds that target in the coordinates its scroll is still
    /// in: it takes this off, so the tick's rebase of both does not apply the landing twice.
    pub fn pending_shift<H: Host, S: CardSource<H>>(&self, cx: &Cx<'_, H>, src: &S) -> f32 {
        if self.spec.scroll != ScrollMode::External || super::focused_index(&cx.focus, self.entry, src).is_none() {
            return 0.0;
        }
        pool::slid(src, self.anchor).map_or(0.0, |(p, q)| {
            ((q / self.spec.cols) as f32 - (p / self.spec.cols) as f32) * self.spec.geom().pitch()
        })
    }

    /// The scroll the last tick wanted for the focused row (`poster_grid::snap_row_in_for` against
    /// the current scroll; for a [`resting`](GridSpec::resting) grid `rest_scroll` with no document
    /// end, which [`rest_target`](Self::rest_target) holds to the owner's), `None` while focus is not
    /// in the grid. An external owner may take it or apply its own reveal rule; the self-scrolling
    /// grid springs to it.
    pub fn reveal_target(&self) -> Option<f32> {
        self.reveal.map(|(_, at)| at)
    }

    /// Whether the grid's rows rest where the first row does ([`GridSpec::resting`]).
    pub fn rests(&self) -> bool {
        self.spec.rests
    }

    /// A resting grid's scroll for its focused row, held to a document that ends at `end` (the
    /// owner's furthest scroll with this focus): `poster_grid::rest_scroll`, the Library's rule.
    /// `None` while focus is not in the grid.
    pub fn rest_target(&self, end: f32) -> Option<f32> {
        self.reveal.map(|(row, _)| self.rest_scroll(row, end))
    }

    fn rest_scroll(&self, row: usize, end: f32) -> f32 {
        let growth = poster_grid::growth_before_for(self.band_h(), row, &poster_grid::settled(Some(row)));
        poster_grid::rest_scroll(row, self.spec.geom().pitch(), growth, end)
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
    /// Only the geometry changes: the content is untouched, so each card keeps its pop spring and the caption bands carry over.
    pub fn set_columns(&mut self, cols: usize, style: RowStyle, left: f32) {
        self.spec.cols = cols;
        self.spec.style = style;
        self.spec.left = left;
    }

    /// Open `row`'s caption band at once, settled (and close the rest), for an owner that sizes
    /// its document from the bands before the next tick would (a restored page's first layout).
    /// A no-op when `row` is already the focused row, so it never cuts short a band `on` is
    /// animating, and while the grid is asleep ([`dormant`](Self::dormant) and not returning): a seat
    /// nobody has lifted has no band open under it, and its caption with it.
    pub fn settle_band(&mut self, row: Option<usize>) {
        if !self.asleep() {
            self.bands.focus(row, false);
        }
    }

    /// Forget the focus pop's cell and any pending move: a fresh content set whose old indexes mean
    /// nothing, so the next tick adopts the focused element whole instead of reading it as the
    /// same element landing at a new index. For an owner that was not ticking while its content
    /// was replaced (a covered page).
    pub fn forget_pop(&mut self) {
        self.pop.reset();
        self.seen = Seen::Nothing;
    }

    /// The caption bands closed and settled: a fresh content set whose old rows mean nothing.
    pub fn reset_bands(&mut self) {
        self.bands = GridBands::new();
        self.bands.set_caption_policy(self.caption_policy);
    }

    /// The grid's height for `len` cards with its caption bands as they are now: the span from the
    /// first row's top to the end of the last row's band, the pitch's air included, as `poster_grid::max_scroll_in` counts it (what a `Stack` lays the next section
    /// after, and bounds its scroll by).
    pub fn height(&self, len: usize) -> f32 {
        poster_grid::row_top_in_for(self.band_h(), &self.spec.geom(), len.div_ceil(self.spec.cols), 0.0, &self.bands.geometry())
    }

    /// [`height`](Self::height) at the bands' DESTINATION: the focused row's caption band open when
    /// `focused` (this grid holds the page's focus) and every other closed, wherever the springs are
    /// now. A scroll bound computed against the live bands chases a moving goal for as long as
    /// they animate (the same rule as the shelf's `Stack::settled_height`).
    pub fn settled_height(&self, len: usize, focused: bool) -> f32 {
        let bands = poster_grid::settled(self.bands.focused().filter(|_| focused));
        poster_grid::row_top_in_for(self.band_h(), &self.spec.geom(), len.div_ceil(self.spec.cols), 0.0, &bands)
    }

    /// The cards the scroll can show out of `len`: what `draw` and `record_stops` touch.
    pub fn window(&self, len: usize) -> std::ops::Range<usize> {
        poster_grid::visible_in_for(self.band_h(), &self.spec.geom(), len, self.spec.top, self.scroll.pos)
    }

    /// The run of cards [`draw`](Self::draw) actually paints out of `len`: [`window`](Self::window)
    /// minus the cards the primitive culls (`card_row::paint_visible`, at rest scale) and the row
    /// resting over the content edge. `0..0` when none. How a screen tells the cards it is NOT
    /// drawing (the rows to prefetch) from the ones it is, without naming the L0 test.
    pub fn painted(&self, p: Painter, len: usize) -> std::ops::Range<usize> {
        let bands = self.bands.geometry();
        let window = self.window(len);
        // The test is pure, so the two ends are found from their own side: only the culled cards at
        // each edge are tested, not the whole window.
        let shown = |&i: &usize| {
            !self.above_edge(i, &bands) && card_row::paint_visible(p, super::to_local(p, self.cell(i, &bands)), 1.0, false)
        };
        let Some(first) = window.clone().find(shown) else { return 0..0 };
        first..window.rev().find(shown).unwrap_or(first) + 1
    }

    /// The caption-band geometry (the owner's page layout reads the document height from it).
    pub fn band_geometry(&self) -> [GridBand; poster_grid::MAX_GRID_BANDS] {
        self.bands.geometry()
    }

    /// The pop of card `i` given the engine's focus (`GridPop::scale`'s rule: a focused card the
    /// grid was not told about is FULL, the ones that lost focus let go, the rest are at rest). A
    /// pass over many cards asks [`pool::let_go_cards`] once and calls [`pop_in`](Self::pop_in).
    fn pop<H: Host, S: CardSource<H>>(&self, src: &S, i: usize, focus: Option<usize>) -> f32 {
        self.pop_in(pool::let_go_cards(&self.pop, src), src, i, focus)
    }

    /// [`pop`](Self::pop) with the way this frame finds the let-gos' cards already decided.
    fn pop_in<H: Host, S: CardSource<H>>(&self, cards: LetGoCards, src: &S, i: usize, focus: Option<usize>) -> f32 {
        // a page asleep (unseen, and not returning) lifts nothing; a returning one keeps its seat
        // lifted through the fade or the image
        let focus = focus.filter(|_| !self.asleep());
        // the element-aware pool: a source change no tick has seen yet is resolved by element
        if cards == LetGoCards::ByElement {
            match pool::let_go_drawn(&self.pop, src, i, focus == Some(i)) {
                Some(Drawn::Cell(c)) => return self.pop.scale(c, false, &self.spec.style),
                Some(Drawn::Rest) => return 1.0,
                None => {}
            }
        }
        // The takeover is owed but the pop has no cell: the card focus reconciled to rests until the
        // landing's first step raises it, never the "not told yet" full scale (that read as the
        // new card jumping to the pop under the menu that chose the removal).
        if focus == Some(i) && self.pop.cell() != Some(i) && self.landing.has_takeover() {
            return 1.0;
        }
        self.pop.scale(i, focus == Some(i), &self.spec.style)
    }

    /// The live pop of `elem` (no press).
    pub fn scale_of<H: Host, S: CardSource<H>>(&self, cx: &Cx<'_, H>, src: &S, elem: &H::Elem) -> Option<f32> {
        let i = src.index_of(elem)?;
        Some(self.pop(src, i, super::focused_index(&cx.focus, self.entry, src)))
    }

    fn cell(&self, i: usize, bands: &[GridBand]) -> Rect {
        poster_grid::cell_in_for(self.band_h(), &self.spec.geom(), i, self.spec.top, self.scroll.pos, bands)
    }

    /// Card `i`'s settled (unpopped) cell of `n` cards with a landing's slide folded in: where the
    /// card is drawn and registered while it springs to [`cell`](Self::cell).
    fn slot(&self, i: usize, n: usize, bands: &[GridBand]) -> Rect {
        let (dx, dy) = self.landing.offset_xy(n, i);
        let r = self.cell(i, bands);
        Rect::new(r.x + dx, r.y + dy, r.w, r.h)
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
        let bands = self.bands.geometry();
        let cell = self.slot(i, src.len(), &bands);
        let rect = match how {
            At::Drawn => self.drawn_rect(pool::let_go_cards(&self.pop, src), cx, src, i, &bands),
            At::SpringTarget => cell.scaled(if focus == Some(i) { self.spec.style.focus_scale } else { 1.0 }),
        };
        Some(Placed { rect, rest_rect: cell.scaled(self.spec.style.focus_scale), clip: Rect::FULL, index: Some(i as u32) })
    }

    /// Card `i` as it is DRAWN now: its slot (a landing's slide folded in) scaled by its live pop
    /// and the press dip, the rect [`place`](Self::place) answers for `At::Drawn`. The pop is the
    /// draw's own ([`pop_in`](Self::pop_in) with the pass's `cards`), so a let-go still shrinking is
    /// read the way the frame paints it, even over a source no tick has seen yet.
    fn drawn_rect<H: Host, S: CardSource<H>>(&self, cards: LetGoCards, cx: &Cx<'_, H>, src: &S, i: usize, bands: &[GridBand]) -> Rect {
        let focus = super::focused_index(&cx.focus, self.entry, src);
        let pressed = super::pressed_index(cx, self.entry, src);
        self.slot(i, src.len(), bands).scaled(super::press_scale(self.pop_in(cards, src, i, focus), i, pressed, cx))
    }

    /// **How far the tile directly below card `i` is drawn above its cell**, px: the live pop of the
    /// card that takes (or lets go of) the focus under a caption, which is drawn OVER it. The one
    /// number the grid hands the caption rule ([`card_row::line_reveal_for`]) so a line stops where
    /// that tile's drawn top is, not where its cell rests. 0 for a card in the last row.
    ///
    /// The tile below may be a card focus has just left and that is still shrinking (a let-go: a held
    /// Down or Right leaves one under every caption it passes), which is drawn above its rest top too.
    /// Its scale is the draw's own, read through [`drawn_rect`](Self::drawn_rect) with the pass's
    /// `cards`: never a second computation of what the frame paints.
    fn rise_below<H: Host, S: CardSource<H>>(&self, cards: LetGoCards, cx: &Cx<'_, H>, src: &S, i: usize, bands: &[GridBand]) -> f32 {
        let below = i + self.spec.cols;
        if below >= src.len() {
            return 0.0;
        }
        self.cell(below, bands).y - self.drawn_rect(cards, cx, src, below, bands).y
    }

    /// Draw the grid into `p` (page alpha already applied) and register its stops. Back to front:
    /// the leaving copies of the cards a landing moved to another row (over the owner's ghosts,
    /// which it paints before this), every caption (beneath every tile, so a line still arriving
    /// never covers the poster tops of the row under it), the non-focused cards, the focused one,
    /// then every sort-value line above all of them; only the cards the scroll can show are touched.
    pub fn draw<H: Host, S: CardSource<H>>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, src: &S) {
        let focus = super::focused_index(&f.focus, self.entry, src);
        let pressed = super::pressed_index(f.cx, self.entry, src);
        let bands = self.bands.geometry();
        self.draw_leaving(f, p, src, &bands);
        let plan = self.draw_plan(src.len(), focus, &bands);
        let cards = pool::let_go_cards(&self.pop, src);
        // What each card is painted with, decided once: the captions and the tiles are two passes
        // over the same decisions.
        let paints: Vec<_> = plan.into_iter().map(|(role, i)| {
            let s = super::press_scale(self.pop_in(cards, src, i, focus), i, pressed, f.cx);
            let focused = role == DrawRole::Focus;
            (i, s, focused, self.card_paint(f, p, src, cards, i, s, focused, &bands))
        }).collect();
        // The focused card paints last, over its neighbours, but it is the card the reader is
        // looking at: it asks the source for its poster FIRST, so it is the first one served
        // (`card_row::lead_focused`). It still resolves once; the draw below reuses the answer.
        let lead = paints.last().filter(|&&(.., focused, _)| focused).and_then(|(i, s, _, paint)| {
            let CardPaint { rect, p, .. } = paint.as_ref()?;
            Some(card_row::lead_focused(*p, &src.art(*i), *rect, *s))
        });
        // Captions BENEATH the tiles: every caption of the frame is painted before any tile, so a
        // line that is still arriving (the band opening under the focused row) never paints over
        // the poster tops of the row below it. Draw order only; the band's timing is its own.
        for (i, s, focused, paint) in &paints {
            if let Some(paint) = paint {
                self.draw_caption(f, src, *i, paint, *s, *focused);
            }
        }
        for (i, s, focused, paint) in &paints {
            if let Some(paint) = paint {
                self.draw_card(f, src, *i, *s, *focused, paint, lead.filter(|_| *focused));
            }
        }
        // A value line is the OPPOSITE of a band: it has a fixed place under its poster (never over
        // the row below: `SORT_VALUE_REACH` is inside the 60 px row gap), so it goes ABOVE every
        // tile. The lifted card's drop shadow is folded into its composite quad, which reaches 64 px
        // past the poster: a value drawn before the tiles sat UNDER that shadow and read at 86% of
        // its ink.
        for (i, s, focused, paint) in &paints {
            if let Some(paint) = paint {
                self.draw_value(f, src, *i, paint, *s, *focused);
            }
        }
        self.record_stops(f, p, src);
    }

    /// The cards [`draw`](Self::draw) paints after the leaving copies, back to front: still cards,
    /// cards fading in, movers by the length of their way, the focused card.
    fn draw_plan(&self, n: usize, focus: Option<usize>, bands: &[GridBand]) -> Vec<(DrawRole, usize)> {
        let mut plan = Vec::new();
        let mut moving: Vec<(f32, usize)> = Vec::new();
        let mut arriving = Vec::new();
        for i in self.window(n) {
            if focus == Some(i) || self.above_edge(i, bands) {
                continue;
            }
            match self.landing.travel(n, i) {
                Some(t) => moving.push((t, i)),
                None if self.landing.alpha(n, i) < 1.0 => arriving.push((DrawRole::Arriving, i)),
                None => plan.push((DrawRole::Still, i)),
            }
        }
        // a card still fading in is under every card in flight, which it may be partly behind
        plan.extend(arriving);
        // the longest way (old slot to new, fixed when the landing began) draws on top of the shorter
        moving.sort_by(|a, b| a.0.total_cmp(&b.0));
        plan.extend(moving.into_iter().map(|(_, i)| (DrawRole::Moving, i)));
        if let Some(i) = focus.filter(|&i| i < n) {
            plan.push((DrawRole::Focus, i));
        }
        plan
    }

    /// Test support: what [`draw`](Self::draw) paints, back to front, as `(role, index, rect,
    /// alpha)`: the leaving copies first, then the cards in the plan's order. The rect is the
    /// card's cell with a landing's slide folded in (no pop), the alpha its arrival fade (a
    /// leaving copy's, its fade-out).
    #[cfg(any(test, feature = "test-support"))]
    pub fn plan_probe(&self, n: usize, focus: Option<usize>) -> Vec<(DrawRole, usize, Rect, f32)> {
        let bands = self.bands.geometry();
        let mut out: Vec<_> = self.leaving_rects(n).into_iter().map(|(i, r, a)| (DrawRole::Leaving, i, r, a)).collect();
        out.extend(self.draw_plan(n, focus, &bands).into_iter().map(|(role, i)| {
            (role, i, self.slot(i, n, &bands), self.landing.alpha(n, i))
        }));
        out
    }

    /// Test support: the ghosts a landing is fading out, `(key, rect, alpha)`: the cell each stood
    /// in, held where it stood on screen, at its pop scale.
    #[cfg(any(test, feature = "test-support"))]
    pub fn ghost_slots(&self) -> Vec<(u64, Rect, f32)> {
        let bands = self.bands.geometry();
        self.landing.ghosts().iter().map(|g| {
            let c = self.cell(g.was, &bands);
            (g.key, Rect::new(c.x + g.shift.0, c.y + g.shift.1, c.w, c.h).scaled(g.lift.pos), g.alpha.pos.clamp(0.0, 1.0))
        }).collect()
    }

    /// Register the stops of the cards the scroll can show: each is the rect [`draw`](Self::draw) paints.
    pub fn record_stops<H: Host, S: CardSource<H>>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, src: &S) {
        if !f.records_stops() {
            return;
        }
        let focus = super::focused_index(&f.focus, self.entry, src);
        let pressed = super::pressed_index(f.cx, self.entry, src);
        let bands = self.bands.geometry();
        let cards = pool::let_go_cards(&self.pop, src);
        for i in self.window(src.len()) {
            let s = super::press_scale(self.pop_in(cards, src, i, focus), i, pressed, f.cx);
            let cell = super::to_local(p, self.slot(i, src.len(), &bands));
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
        let pressed = super::pressed_index(f.cx, self.entry, src);
        let cards = pool::let_go_cards(&self.pop, src);
        let s = super::press_scale(self.pop_in(cards, src, i, Some(i)), i, pressed, f.cx);
        let Some(paint) = self.card_paint(f, p, src, cards, i, s, true, &self.bands.geometry()) else { return };
        self.draw_caption(f, src, i, &paint, s, true);
        self.draw_card(f, src, i, s, true, &paint, None);
        self.draw_value(f, src, i, &paint, s, true);
    }

    /// What card `i` is painted with, decided apart from the GL calls so that [`draw_card`](Self::draw_card)
    /// and the host test that reads the captions ([`caption_probe`](Self::caption_probe)) cannot
    /// disagree about what is on screen: the cell it fills, the painter it is drawn with (an arriving
    /// card's fade applied) and the caption block under it with the band it hangs in in `reveal`. `None` when
    /// the card is not painted at all: not loaded, lifted into a menu, or culled.
    #[allow(clippy::too_many_arguments)]
    fn card_paint<H: Host, S: CardSource<H>>(
        &self,
        f: &DrawFrame<'_, '_, H>,
        p: Painter,
        src: &S,
        cards: LetGoCards,
        i: usize,
        s: f32,
        focused: bool,
        bands: &[GridBand],
    ) -> Option<CardPaint> {
        if !src.loaded(i) {
            return None;
        }
        if super::lifted_out(f, self.entry, src, i) {
            return None;
        }
        // Cull before asking the source for any words. A value line has a fixed reach;
        // a focused/closing legacy caption reserves the same room whether its words are empty.
        let open = if focused {
            Some(bands.iter().find(|band| band.row == i / self.spec.cols).map_or(0.0, |band| band.expansion))
        } else {
            self.bands.closing().find(|&(row, card, _)| card == i && row == i / self.spec.cols)
                .map(|(_, _, open)| open)
        };
        let rect = super::to_local(p, self.slot(i, src.len(), bands)).scaled(s);
        let visible = if self.spec.value_line {
            card_row::paint_visible_with_label_h(p, rect, s, card_row::SORT_VALUE_REACH)
        } else { card_row::paint_visible(p, rect, s, open.is_some()) };
        if !visible { return None; }
        // A value line's words stay in the source cache until draw_caption borrows them. Legacy bands
        // retain their per-frame reveal, including the focused ghost's held caption.
        let label = if self.spec.value_line { None } else {
            open.map(|open| {
                let label = if focused && self.caption_held() { super::TileLabel::default() } else { src.label(i) };
                label.revealed(self.band_h(), open, self.rise_below(cards, f.cx, src, i, bands))
            })
        };
        // an arriving card fades in
        let p = match self.landing.alpha(src.len(), i) {
            a if a < 1.0 => p.alpha(a),
            _ => p,
        };
        Some(CardPaint { rect, p, label })
    }

    /// Hands `read` the label card `i` carries, `None` when it carries none: with value lines
    /// the source's own cached label (every card has one), otherwise the one `paint` holds
    /// (the focused card's, or the closing band's of a card the focus left).
    fn with_caption<H: Host, S: CardSource<H>, R>(
        &self, src: &S, i: usize, paint: &CardPaint, read: impl FnOnce(&super::TileLabel) -> R,
    ) -> Option<R> {
        if self.spec.value_line { Some(src.with_label(i, read)) }
        else { paint.label.as_ref().map(read) }
    }

    /// The caption block under card `paint` ([`with_caption`](Self::with_caption)'s label), painted
    /// ahead of every tile ([`draw`](Self::draw)). `focused` marks the lifted card: it opens the
    /// `devtriggers` card probe. A grid in value-line mode paints no block here
    /// ([`draw_value`](Self::draw_value) paints its line after the tiles).
    fn draw_caption<H: Host, S: CardSource<H>>(
        &self, f: &DrawFrame<'_, '_, H>, src: &S, i: usize, paint: &CardPaint, s: f32, focused: bool,
    ) {
        #[cfg(feature = "devtriggers")]
        if focused { crate::card_probe::begin_card(); }
        #[cfg(not(feature = "devtriggers"))]
        let _ = focused;
        if self.spec.value_line { return; }
        self.with_caption(src, i, paint, |label| {
            card_row::draw_label(paint.p, paint.rect, s, &self.spec.style, label, f.measure);
        });
    }

    /// The sort value line under card `paint` (the source's cached label), painted AFTER every
    /// tile ([`draw`](Self::draw)): `focused` marks the lifted card, whose value is drawn bold.
    fn draw_value<H: Host, S: CardSource<H>>(
        &self, f: &DrawFrame<'_, '_, H>, src: &S, i: usize, paint: &CardPaint, s: f32, focused: bool,
    ) {
        if !self.spec.value_line { return; }
        src.with_label(i, |label| {
            if let Some(value) = &label.value {
                card_row::draw_sort_value(paint.p, paint.rect, s, &self.spec.style, value, f.measure, focused);
            }
        });
    }

    /// The tile of card `i`, drawn after the captions: the art, its lift, its resume bar and the
    /// source's overlay.
    #[allow(clippy::too_many_arguments)]
    fn draw_card<H: Host, S: CardSource<H>>(
        &self,
        f: &DrawFrame<'_, '_, H>,
        src: &S,
        i: usize,
        s: f32,
        focused: bool,
        paint: &CardPaint,
        led: Option<crate::widgets::ArtImage>,
    ) {
        let (rect, p) = (paint.rect, paint.p);
        if focused {
            #[cfg(feature = "devtriggers")]
            crate::card_probe::begin_art();
            card_row::draw_focused_tile_led(p, src.art(i), rect, s, &self.spec.style, src.progress(i), led);
            #[cfg(feature = "devtriggers")]
            if !p.is_recording() {
                let at = p.to_screen(rect).0;
                let held = self.window(src.len());
                crate::card_probe::focused(at.x, at.y, at.w, i.saturating_sub(held.start), held.len(), i);
            }
        } else {
            card_row::draw_tile(p, src.art(i), rect, s, &self.spec.style, src.progress(i));
        }
        src.overlay(p, i, &Tile { rect, scale: s, radius: self.spec.style.tile_radius(rect, s), focused }, f.measure);
    }

    /// Test support: the captions [`draw`](Self::draw) puts on screen now, `(card index, alpha of its
    /// most visible line)`,
    /// read from the same decision the draw paints (`card_paint`: loaded, lifted, culled, arriving
    /// fade and all), so a caption that is cut or late on screen is cut or late here.
    #[cfg(any(test, feature = "test-support"))]
    pub fn caption_probe<H: Host, S: CardSource<H>>(&self, f: &DrawFrame<'_, '_, H>, p: Painter, src: &S) -> Vec<(usize, f32)> {
        let focus = super::focused_index(&f.focus, self.entry, src);
        let pressed = super::pressed_index(f.cx, self.entry, src);
        let bands = self.bands.geometry();
        let cards = pool::let_go_cards(&self.pop, src);
        self.draw_plan(src.len(), focus, &bands).into_iter().filter_map(|(role, i)| {
            let s = super::press_scale(self.pop_in(cards, src, i, focus), i, pressed, f.cx);
            let paint = self.card_paint(f, p, src, cards, i, s, role == DrawRole::Focus, &bands)?;
            self.with_caption(src, i, &paint, |label| {
                (label.title.is_some() || label.caption.is_some()).then(|| {
                    (i, label.lines().iter().flatten().map(|l| l.alpha).fold(0.0, f32::max) * paint.p.opacity())
                })
            }).flatten()
        }).collect()
    }

    /// Test support: every caption LINE [`draw`](Self::draw) puts on screen now, `(card index, top y,
    /// bottom y, alpha)` in the grid's own space, from the same decision the draw paints
    /// (`card_paint`) and the same boxes ([`TileLabel::lines`](card_row::TileLabel::lines)): a line
    /// that is not drawn (alpha 0) is not listed, so a test reads exactly the ink a frame carries.
    #[cfg(any(test, feature = "test-support"))]
    pub fn caption_lines_probe<H: Host, S: CardSource<H>>(&self, f: &DrawFrame<'_, '_, H>, p: Painter, src: &S) -> Vec<(usize, f32, f32, f32)> {
        let focus = super::focused_index(&f.focus, self.entry, src);
        let pressed = super::pressed_index(f.cx, self.entry, src);
        let bands = self.bands.geometry();
        let cards = pool::let_go_cards(&self.pop, src);
        let mut out = Vec::new();
        for (role, i) in self.draw_plan(src.len(), focus, &bands) {
            let s = super::press_scale(self.pop_in(cards, src, i, focus), i, pressed, f.cx);
            let Some(paint) = self.card_paint(f, p, src, cards, i, s, role == DrawRole::Focus, &bands) else { continue };
            self.with_caption(src, i, &paint, |label| {
                let bottom = card_row::unscaled_bottom(paint.rect, s);
                for line in label.lines().into_iter().flatten().filter(|l| l.alpha * paint.p.opacity() > 0.0) {
                    out.push((i, bottom + line.top, bottom + line.bottom, line.alpha * paint.p.opacity()));
                }
            });
        }
        out
    }

    /// The focused card left as a ghost and the landing has not made its first moving step (the menu's
    /// veil, a held image, or the frame it began in is still being drawn as the start state): the
    /// caption is still the ghost's, and the card focus reconciled to draws none yet. It changes on the
    /// frame the springs first step, with [`landing_waiting`](Self::landing_waiting)'s other readers
    /// (the header count).
    fn caption_held(&self) -> bool {
        self.landing_waiting() && self.landing.ghosts().iter().any(|g| g.focused)
    }

    /// Test support: the element of the focused card's ghost whose caption is on screen now (the
    /// caption the reader reads under the focus), if the ghost still holds it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn ghost_caption(&self) -> Option<u64> {
        self.landing.ghosts().iter().find(|g| g.focused && self.caption_held()).map(|g| g.key)
    }

    /// The cards a landing moved to another row, fading out where they stood (at rest, never
    /// captioned or registered: the card itself is fading in at its new slot).
    fn draw_leaving<H: Host, S: CardSource<H>>(&self, f: &DrawFrame<'_, '_, H>, p: Painter, src: &S, bands: &[GridBand]) {
        for (i, (dx, dy), a) in self.landing.leaving(src.len()) {
            if a <= 0.0 || !src.loaded(i) || super::lifted_out(f, self.entry, src, i) {
                continue;
            }
            let cell = self.cell(i, bands);
            let rect = super::to_local(p, Rect::new(cell.x + dx, cell.y + dy, cell.w, cell.h));
            if !card_row::paint_visible(p, rect, 1.0, false) {
                continue;
            }
            let pr = p.alpha(a);
            card_row::draw_tile(pr, src.art(i), rect, 1.0, &self.spec.style, src.progress(i));
            src.overlay(pr, i, &Tile { rect, scale: 1.0, radius: self.spec.style.tile_radius(rect, 1.0), focused: false }, f.measure);
        }
    }

    /// The leaving copies a landing draws now, `(index, rect, alpha)`: where each card that changed
    /// row stood on screen, fading out.
    pub fn leaving_rects(&self, n: usize) -> Vec<(usize, Rect, f32)> {
        let bands = self.bands.geometry();
        self.landing
            .leaving(n)
            .map(|(i, (dx, dy), a)| {
                let c = self.cell(i, &bands);
                (i, Rect::new(c.x + dx, c.y + dy, c.w, c.h), a)
            })
            .collect()
    }

    /// A landing is playing: an owner that keeps the cards it removed for
    /// [`paint_ghosts`](Self::paint_ghosts) keeps them while this holds.
    pub fn landing_pending(&self) -> bool {
        self.landing.pending()
    }

    /// A landing is playing but has not stepped yet: the menu's veil has not yet dropped below
    /// `CLOSING_GONE`, the page is a held image, or the frame it began in is still the frame before
    /// it (the springs step from the next). An owner pairs its own change with the landing's first step.
    pub fn landing_waiting(&self) -> bool {
        self.landing.waiting() || self.landing.unstepped()
    }

    /// Whether the card in `cell` is letting go.
    #[cfg(test)]
    pub(crate) fn letting_go(&self, cell: usize) -> bool {
        self.pop.letting_go(cell)
    }

    /// The scale card `focus` is drawn at now (`None`, or a grid asleep: rest): what a viewer sees
    /// of the lift, with the let-gos of the cards focus left out.
    #[cfg(any(test, feature = "test-support"))]
    pub fn pop_probe(&self, focus: Option<usize>) -> f32 {
        focus.filter(|_| !self.asleep()).map_or(1.0, |i| self.pop.pop_scale(i, &self.spec.style))
    }

    /// Card `i`'s settled (unpopped) cell as the grid stands now, the caption bands folded in: where
    /// a test reads how much room the focused row opened under its cards.
    #[cfg(any(test, feature = "test-support"))]
    pub fn cell_probe(&self, i: usize) -> Rect {
        self.cell(i, &self.bands.geometry())
    }

    /// What a landing is doing to card `i` of `n` right now: `((x, y) offset px, alpha)`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn landing_probe(&self, n: usize, i: usize) -> ((f32, f32), f32) {
        (self.landing.offset_xy(n, i), self.landing.alpha(n, i))
    }

    /// The ghosts a landing is fading out, as the indexes they had before it, and the most opaque
    /// one's alpha.
    #[cfg(any(test, feature = "test-support"))]
    pub fn ghost_probe(&self) -> (Vec<usize>, f32) {
        let a = self.landing.ghosts().iter().map(|g| g.alpha.pos.clamp(0.0, 1.0)).fold(0.0, f32::max);
        (self.landing.ghosts().iter().map(|g| g.was).collect(), a)
    }

    /// Each ghost's pop scale right now, in [`ghost_keys`](Self::ghost_keys) order.
    #[cfg(any(test, feature = "test-support"))]
    pub fn ghost_scales(&self) -> Vec<f32> {
        self.landing.ghosts().iter().map(|g| g.lift.pos).collect()
    }

    /// The elements ([`elem_key`](super::elem_key)) of the cards a landing is fading out, in the
    /// order [`paint_ghosts`](Self::paint_ghosts) reads its ghost source by.
    pub fn ghost_keys(&self) -> impl Iterator<Item = u64> + '_ {
        self.landing.ghosts().iter().map(|g| g.key)
    }

    /// Whether `key`'s card left this grid and is still fading as a landing ghost: an opener that
    /// is departing ([`Screen::opener_departing`](crate::screen::Screen::opener_departing)).
    pub fn departing<H: Host>(&self, key: FocusKey<H::Elem>) -> bool {
        key.entry == self.entry && {
            let k = super::elem_key(&key.elem);
            self.landing.ghosts().iter().any(|g| g.key == k && g.alpha.pos > 0.0)
        }
    }

    /// Whether [`paint_ghosts`](Self::paint_ghosts) has a card to draw this frame.
    pub fn has_ghosts(&self) -> bool {
        self.landing.has_ghosts()
    }

    /// The cards a landing removed, fading out in the cell they stood in (held in place on screen
    /// when the scroll moved to keep focus where it was): paint them BEFORE [`draw`](Self::draw), so
    /// the cards that slid over their cells cover them. `ghosts` holds the removed cards in
    /// [`ghost_keys`](Self::ghost_keys) order (only `art`, `progress`, `loaded`, `overlay` and, for
    /// the focused card's ghost, `label` are read; a ghost is never registered as a stop, and is
    /// captioned only while the landing waits to step, so the caption stays as drawn; one that was the focused card eases
    /// from its lifted scale to rest while it fades). Nothing is drawn when no landing is playing,
    /// and only what is on screen.
    pub fn paint_ghosts<H: Host, G: CardSource<H> + ?Sized>(&self, f: &DrawFrame<'_, '_, H>, p: Painter, ghosts: &G) {
        let bands = self.bands.geometry();
        // A ghost the opener lift carries is drawn by the lift alone (`DrawFrame::lifting`) and left
        // out of the page pass (`DrawFrame::lifted`), as a lifted card is.
        let key = |k: Option<FocusKey<H::Elem>>| k.filter(|k| k.entry == self.entry).map(|k| super::elem_key(&k.elem));
        let (lifted, lifting) = (key(f.lifted), key(f.lifting));
        for (j, g) in self.landing.ghosts().iter().enumerate().filter(|(j, _)| *j < ghosts.len() && ghosts.loaded(*j)) {
            let a = g.alpha.pos.clamp(0.0, 1.0);
            if a <= 0.0 || lifting.is_some_and(|k| k != g.key) {
                continue;
            }
            if lifted == Some(g.key) {
                crate::popover::set_lift_owns(true);
                continue;
            }
            let cell = self.cell(g.was, &bands);
            let s = g.lift.pos;
            let rect = super::to_local(p, Rect::new(cell.x + g.shift.0, cell.y + g.shift.1, cell.w, cell.h)).scaled(s);
            if !card_row::paint_visible(p, rect, s, false) {
                continue;
            }
            let pr = p.alpha(a);
            card_row::draw_tile(pr, ghosts.art(j), rect, s, &self.spec.style, ghosts.progress(j));
            let tile = Tile { rect, scale: s, radius: self.spec.style.tile_radius(rect, s), focused: g.focused };
            ghosts.overlay(pr, j, &tile, f.measure);
            // The focused card's caption stays as drawn until the landing steps: from then on it is
            // the new focus card's, which moves with the focus.
            if g.focused && self.landing_waiting() {
                let open = bands.iter().find(|band| band.row == g.was / self.spec.cols).map_or(0.0, |band| band.expansion);
                // nothing below it lifts while the landing waits: the card focus reconciled to rests
                // until the landing's first step (`pop`), so the ghost's caption sees its row at rest
                let label = ghosts.label(j).revealed(self.band_h(), open, 0.0);
                card_row::draw_label(pr, rect, s, &self.spec.style, &label, f.measure);
            }
        }
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

    /// The focus pop's and every let-go's canonical state.
    pub fn write_pop(&self, c: &mut Canon) {
        self.pop.write(c);
    }
}
