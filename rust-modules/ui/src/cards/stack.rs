//! [`Stack`] — the vertical page of card sections (layer L2: pages built from the L1 sections).
//!
//! A page is a header of some kind, then [`Shelf`]s and [`Grid`]s, then a status read-out. The page
//! implements [`StackPage`] (what its sections are, in order, and what each shows) and feeds
//! every event to [`Stack::on`]; the `Stack` owns what the sections share: the document layout,
//! the reveal scroll, the groups and their seats, placement, reconcile (the one focus-recovery
//! rule), landing anchoring and the memory a Back restores. It is a `Part`, not a `Screen`: a
//! hero or any other bespoke block is a [`Kind::Custom`] section the page draws itself.
//!
//! - **Layout.** Sections stack in document space from y = 0 in the order [`StackPage::sections`]
//!   gives, each as tall as its [`Kind`] says (a grid's height follows its live caption bands).
//!   An [`Kind::Overlay`] is out of flow at a screen rect. The `Stack` owns the one scroll; every
//!   [`Grid`] runs in [`ScrollMode::External`] and is handed the page before each call, so the
//!   grid's reveal rule, landing shift and bands are used as they are.
//! - **Groups.** Each section carries the focus group the page names it by ([`SectionSpec::group`])
//!   and where that group is listed ([`SectionSpec::rank`]): ids are stable per section kind, never
//!   positions, because the engine keys its cursors by `(entry, group)` and a fresh mount targets
//!   `GroupId(0)`.
//! - **A page that owns its vertical model.** The defaults above are Collection's and Person's. A
//!   page may instead set [`SectionSpec::seated`] / [`edges`](SectionSpec::edges) /
//!   [`of_kind`](SectionSpec::of_kind) on a section's group, answer a seat itself
//!   ([`StackPage::seat_override`]), hold a list in one `Custom` section, clip its placements
//!   ([`Stack::clipped`]), and drive the scroll on `FocusMoved` rather than every tick
//!   ([`Stack::reveal_on_move`], [`Stack::scroll_to`], [`Stack::jump_to`]).
//! - **Cost.** `sections` runs again only when [`StackPage::revision`] moves, so a page puts
//!   everything its section list depends on (status, a truncated summary, the item count) in it.
//! - **Reconcile.** The focused element is kept when a section still shows it; else a page still
//!   [`pending`](StackPage::pending) keeps the wanted focus; else the same position clamped in the
//!   section it was in; else the first focusable of [`StackPage::fallback`].
//! - **Motion canon.** [`Stack::write`] is the page's motion state: the scroll's spring and every
//!   section's pop, bands and scroll.

use plx_machine::machine::{Canon, Cx, Effects, EntryId, FocusKey, GroupId, Host, ScreenEvent};
use plx_machine::present::PresentEvent;

use super::{CardEvent, CardSource, Grid, GridSpec, SectionFrame, Shelf};
use crate::card_row::{self, RowStyle};
use crate::consts::{K_SCROLL, MARGIN_Y, SCR_H, SCR_W};
use crate::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Hover, Part,
    Placed, Seat, Step, Stop,
};
use crate::{Rect, Spring};

/// What one section of a page is.
#[derive(Clone, Copy)]
pub enum Kind {
    /// A horizontal strip under a `heading`-pixel heading the page draws ([`StackPage::draw_heading`]).
    Shelf { style: &'static RowStyle, heading: f32 },
    /// A grid. The `Stack` makes it [`external`](GridSpec::external) and sets its top.
    Grid { spec: GridSpec },
    /// A block the page draws ([`StackPage::custom_draw`]): `height` tall in the flow. With
    /// `focusable` it is one focus element ([`StackPage::elem_of`]) that reports [`StackEvent::Press`],
    /// or [`StackPage::plain_len`] of them (a list, a button row) that the page walks and rects
    /// itself ([`plain_step`](StackPage::plain_step), [`element_rect`](StackPage::element_rect)).
    Custom { height: f32, focusable: bool },
    /// Out of flow at a fixed screen rect: a status read-out, a Retry action.
    Overlay { rect: Rect, focusable: bool },
}

/// One section of a page: what it is and the focus group the PAGE names it by.
///
/// The group id is the page's, not a position: the engine remembers a cursor per
/// `(EntryId, GroupId)` and a fresh mount targets `GroupId(0)`, so a section keeps its id however
/// many other sections exist. Ids must be unique among the sections of a page.
#[derive(Clone, Copy)]
pub struct SectionSpec<K> {
    pub key: K,
    pub kind: Kind,
    pub group: GroupId,
    /// Where this section's group stands in [`Focusable::groups`] (ascending; ties keep document
    /// order). The engine seats the first non-empty group of that order when nothing is focused.
    pub rank: u32,
    /// The parts of this section's group a page that owns the vertical model (explicit links
    /// between rows, a seat projected from where the move came from) replaces; `None` keeps the
    /// kind's own. Set through [`seated`](Self::seated), [`edges`](Self::edges) and
    /// [`of_kind`](Self::of_kind).
    seat: Option<Seat>,
    edge: Option<[EdgeRule; 4]>,
    elem: Option<ElemKind>,
}

impl<K> SectionSpec<K> {
    fn apply(&self, g: &mut GroupSpec) {
        g.seat = self.seat.unwrap_or(g.seat);
        g.edge = self.edge.unwrap_or(g.edge);
        g.elem = self.elem.unwrap_or(g.elem);
    }

    /// A section in document order, its group listed in document order too.
    pub fn new(key: K, kind: Kind, group: GroupId) -> Self {
        Self { key, kind, group, rank: 0, seat: None, edge: None, elem: None }
    }

    /// Seat this section's group by `seat` instead of its kind's rule.
    pub fn seated(mut self, seat: Seat) -> Self {
        self.seat = Some(seat);
        self
    }

    /// Give this section's group `edge` (up, down, left, right) instead of its kind's rules.
    pub fn edges(mut self, edge: [EdgeRule; 4]) -> Self {
        self.edge = Some(edge);
        self
    }

    /// Give this section's elements `elem`'s press behaviour instead of its kind's.
    pub fn of_kind(mut self, elem: ElemKind) -> Self {
        self.elem = Some(elem);
        self
    }

    /// List this section's group at `rank` instead of in document order.
    pub fn ranked(mut self, rank: u32) -> Self {
        self.rank = rank;
        self
    }
}

/// What [`Stack::on`] reports. One at most per call; a `Want` beyond the first of a tick waits for
/// the next tick (a `Want` is only ever read on a tick), so none is lost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StackEvent<K, E> {
    Card(K, CardEvent<E>),
    /// A focusable `Custom` / `Overlay` section was activated.
    Press(K),
}

/// The page a [`Stack`] lays out.
pub trait StackPage<H: Host> {
    type Key: Copy + Eq;
    type Cards<'a>: CardSource<H>
    where
        Self: 'a;

    /// A counter that moves whenever anything [`sections`](Self::sections) or a section's length
    /// reads moves (a content landing, a status change, a measured summary).
    fn revision(&self, cx: &Cx<'_, H>) -> u64;
    /// Whether the store is still delivering what `want` named, so the wanted focus is kept.
    fn pending(&self, _cx: &Cx<'_, H>, _want: &H::Elem) -> bool {
        false
    }
    /// Where focus goes when `want` is no longer shown, where the page keeps its own record of
    /// where each element was (a slot in its group) and the position the `Stack` last saw focus at
    /// is not it. `None` leaves it to the `Stack`.
    fn recover(&self, _cx: &Cx<'_, H>, _want: &H::Elem) -> Option<H::Elem> {
        None
    }
    fn sections(&self, cx: &Cx<'_, H>, out: &mut Vec<SectionSpec<Self::Key>>);
    /// The page-level order focus falls back through when its section empties.
    fn fallback(&self, cx: &Cx<'_, H>, out: &mut Vec<Self::Key>);
    /// The cards of a `Shelf` / `Grid` section; `None` while it has no content.
    fn cards<'a>(&'a self, cx: &'a Cx<'_, H>, k: Self::Key) -> Option<Self::Cards<'a>>;
    /// Whether a held OK on card `elem` of section `k` opens an item menu: the hold hint
    /// ([`Stack::hold_hint`]) is shown only on a card that answers `true`. A page whose cards can
    /// open something else on a hold (a collection hit opens its page) says so here; a page whose
    /// cards are catalog items answers with the app's own predicate, `item_menu::has_actions`,
    /// the one the bridge declines a menu with.
    fn card_has_menu(&self, _cx: &Cx<'_, H>, _k: Self::Key, _elem: &H::Elem) -> bool {
        true
    }
    /// The one engine element of a focusable `Custom` / `Overlay` section.
    fn elem_of(&self, _k: Self::Key) -> Option<H::Elem> {
        None
    }
    /// How many engine elements a focusable `Custom` / `Overlay` section holds (a list, a button
    /// row). One when [`elem_of`](Self::elem_of) names one.
    fn plain_len(&self, k: Self::Key) -> usize {
        self.elem_of(k).is_some() as usize
    }
    /// Element `n` of a focusable `Custom` / `Overlay` section, in the order
    /// [`plain_step`](Self::plain_step) walks.
    fn plain_elem(&self, k: Self::Key, n: usize) -> Option<H::Elem> {
        if n == 0 { self.elem_of(k) } else { None }
    }
    /// Where a move `dir` from element `at` of a multi-element section ends inside it, as an index
    /// into [`plain_elem`](Self::plain_elem); `None` is the section's edge (its group's
    /// [`EdgeRule`]s and the page's links decide what is beyond).
    fn plain_step(&self, _k: Self::Key, _at: usize, _dir: Dir) -> Option<usize> {
        None
    }
    /// The focus rect (and hit target) of a focusable `Custom` / `Overlay` section, given the
    /// section's own rect on screen.
    fn focus_rect(&self, _cx: &Cx<'_, H>, _k: Self::Key, section: Rect) -> Rect {
        section
    }
    /// The focus rect of element `n` of a multi-element section; one element is
    /// [`focus_rect`](Self::focus_rect).
    fn element_rect(&self, cx: &Cx<'_, H>, k: Self::Key, _n: usize, section: Rect) -> Rect {
        self.focus_rect(cx, k, section)
    }
    /// The element a group is entered on, where the page has its own answer for section `k`
    /// (any kind) before the section's: `from` is the placement the move came from. `None` leaves
    /// it to the section.
    fn seat_override(&self, _cx: &Cx<'_, H>, _k: Self::Key, _from: Placed) -> Option<H::Elem> {
        None
    }
    /// How far above a shelf's heading the page's top edge sits when the scroll reveals it from
    /// below (the margin its block is brought up to).
    fn reveal_margin(&self, _k: Self::Key) -> f32 {
        MARGIN_Y
    }
    /// Air the page keeps under a shelf's block, counted in the block's height (the scroll reveals
    /// it with the block): a page whose shelves are stacked with the label band's air under each.
    fn shelf_foot(&self, _k: Self::Key) -> f32 {
        0.0
    }
    /// Whether a shelf's group extent spans the page's width (the default) or is the shelf's own
    /// head tile.
    fn wide_extent(&self, _k: Self::Key) -> bool {
        true
    }
    /// The focus group of a focusable `Custom` / `Overlay` section.
    fn plain_group(&self, _cx: &Cx<'_, H>, _k: Self::Key, id: GroupId, extent: Rect) -> GroupSpec {
        GroupSpec {
            id,
            kind: GroupKind::Free,
            seat: Seat::First,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent,
            len: 1,
            elem: ElemKind::Bare,
        }
    }
    /// When focus is on section `k`, the scroll reveals section `j`'s block instead: a focusable
    /// `Custom` that is part of a larger block (a header band holding two controls) names the block.
    fn reveal_with(&self, _k: Self::Key) -> Option<Self::Key> {
        None
    }
    /// A shelf's heading in `r` (screen space); `lift` is how far it must rise, live, to clear the
    /// focused tile.
    fn draw_heading(&self, _k: Self::Key, _f: &mut DrawFrame<'_, '_, H>, _r: Rect, _lift: f32) {}
    /// A `Custom` / `Overlay` section drawn in `r` (screen space); `scroll` is the page's.
    fn custom_draw(&self, _k: Self::Key, _f: &mut DrawFrame<'_, '_, H>, _r: Rect, _scroll: f32) {}
}

enum Body {
    Shelf(Shelf),
    Grid(Grid),
    Plain,
}

/// What a Back restores: the page scroll and each shelf's.
#[derive(Clone, Debug, PartialEq)]
pub struct StackMemory<K> {
    pub scroll: f32,
    pub shelves: Vec<(K, f32)>,
}

pub struct Stack<K> {
    entry: EntryId,
    specs: Vec<SectionSpec<K>>,
    bodies: Vec<Body>,
    revision: Option<u64>,
    scroll: Spring,
    target: f32,
    /// Scroll home when focus is not in the page's flow ([`Stack::home_when_unfocused`]).
    home: bool,
    /// The reveal target moves only on `FocusMoved`, not every tick ([`Stack::reveal_on_move`]).
    on_move: bool,
    /// The clip every section's placement carries ([`Stack::clipped`]).
    clip: Rect,
    /// The section and position focus was last on, for the reconcile rule.
    last: Option<(K, usize)>,
    /// `Want`s a tick could not report yet, oldest first. Only `Want`s wait here: an activation or
    /// a hold comes from the one focused section and is reported at once.
    queue: Vec<(K, std::ops::Range<usize>)>,
    /// Indices into `specs` in [`SectionSpec::rank`] order.
    order: Vec<usize>,
    /// Section tops in document space, refreshed with every pass over the sections' heights.
    tops: Vec<f32>,
    /// The element focus was last on: the last answer of a `seat` nothing else can answer.
    last_elem: u32,
    restore: Option<StackMemory<K>>,
    /// The standing "Hold OK for options" hint, on the pages that opted in
    /// ([`Stack::hold_hint`]). Paint-only: never written to the canon.
    hint: Option<crate::hold_hint::HoldHint>,
    /// How many times `sections` ran (the cost case's counter).
    #[cfg(test)]
    pub(crate) rebuilds: u32,
}

impl<K: Copy + Eq> Stack<K> {
    pub fn new(entry: EntryId) -> Self {
        Self {
            entry,
            specs: Vec::new(),
            bodies: Vec::new(),
            revision: None,
            scroll: Spring::at(0.0),
            target: 0.0,
            home: false,
            on_move: false,
            clip: Rect::FULL,
            last: None,
            queue: Vec::new(),
            order: Vec::new(),
            tops: Vec::new(),
            last_elem: 0,
            restore: None,
            hint: None,
            #[cfg(test)]
            rebuilds: 0,
        }
    }

    /// Scroll the page home (0) whenever focus is not on a section in the flow: nothing of this
    /// page focused (a menu over it, the pointer gone) or an out-of-flow [`Kind::Overlay`] held.
    /// Off by default: a page whose focus is elsewhere then stays where it is.
    pub fn home_when_unfocused(mut self, on: bool) -> Self {
        self.home = on;
        self
    }

    /// Recompute the reveal target when focus MOVES (and leave it between moves) instead of on
    /// every tick: a page that also sets the scroll itself ([`scroll_to`](Self::scroll_to): a
    /// wheel, a panel that parks the page) keeps its setting until focus moves again. A move to
    /// an element no section of the page shows sends the page home.
    pub fn reveal_on_move(mut self, on: bool) -> Self {
        self.on_move = on;
        self
    }

    /// Clip every section's placement to `clip` (default: none): a page painted under a standing
    /// bar whose hits the bar owns.
    pub fn clipped(mut self, clip: Rect) -> Self {
        self.clip = clip;
        self
    }

    /// Teach the hold on this page: a standing "Hold OK for options" hint over the cards, on the
    /// rarer cadence of a non-Home screen of `kind` (`HoldHint::once_per_run`). The stack steps it
    /// on its `Tick`, answers its two screen-specific questions itself ([`hint_input`](Self::hint_input))
    /// and draws it last in [`StackView::paint`]. For a page whose card holds open the item menu.
    pub fn hold_hint(mut self, kind: crate::hold_hint::Kind) -> Self {
        self.hint = Some(crate::hold_hint::HoldHint::once_per_run(kind));
        self
    }

    /// Draw the hold hint, if the page opted in: a standing note, never a target (no stop, no hit
    /// rect), over everything the page painted. [`StackView::paint`] ends with it; a page that
    /// paints its sections itself calls it last.
    pub fn draw_hold_hint(&self, p: crate::Painter, measure: &dyn plx_machine::machine::Measure) {
        if let Some(h) = &self.hint {
            h.draw(p, measure);
        }
    }

    /// Whether the hold hint ([`hold_hint`](Self::hold_hint)) is on screen.
    pub fn hint_visible(&self) -> bool {
        self.hint.as_ref().is_some_and(|h| h.visible())
    }

    pub fn scroll(&self) -> f32 {
        self.scroll.pos
    }

    /// Move the page to `y` (glides there). Under [`reveal_on_move`](Self::reveal_on_move) it
    /// stays until focus moves.
    pub fn scroll_to(&mut self, y: f32) {
        self.target = y;
    }

    /// Put the page at `y` at once, without gliding.
    pub fn jump_to(&mut self, y: f32) {
        self.scroll.jump(y);
        self.target = y;
    }

    /// The furthest the page scrolls: its content's end (settled bands) at the screen's foot.
    pub fn max_scroll<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>) -> f32 {
        self.max_with_focus(p, cx, self.focused(p, cx).map_or(usize::MAX, |(i, _)| i))
    }

    /// [`max_scroll`](Self::max_scroll) with focus on section `focus` (its bands the open ones).
    fn max_with_focus<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, focus: usize) -> f32 {
        let h = |n: usize| self.settled_height(p, cx, n, focus);
        ((0..self.specs.len()).map(h).sum::<f32>() - (SCR_H - MARGIN_Y)).max(0.0)
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    /// The scroll spring and its target, for a page that writes its own canon around them.
    pub fn write_scroll(&self, c: &mut Canon) {
        c.f32(self.scroll.pos).f32(self.scroll.vel).f32(self.target);
    }

    /// Shelf `k`'s state (`None` while the layout has no such shelf).
    pub fn shelf(&self, k: K) -> Option<&Shelf> {
        match self.bodies.get(self.index(k)?) {
            Some(Body::Shelf(s)) => Some(s),
            _ => None,
        }
    }

    /// Where shelf `k`'s tiles sit now.
    pub fn shelf_frame<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, k: K) -> Option<SectionFrame> {
        Some(self.frame(p, cx, self.index(k)?))
    }

    /// The focus group the page named section `k` (`None` while the layout has no such section).
    pub fn group(&self, k: K) -> Option<GroupId> {
        self.index(k).map(|i| self.specs[i].group)
    }

    fn index_of_group(&self, g: GroupId) -> Option<usize> {
        self.specs.iter().position(|s| s.group == g)
    }

    fn index(&self, k: K) -> Option<usize> {
        self.specs.iter().position(|s| s.key == k)
    }

    pub fn memory(&self) -> StackMemory<K> {
        let shelves = self
            .specs
            .iter()
            .zip(&self.bodies)
            .filter_map(|(s, b)| if let Body::Shelf(sh) = b { Some((s.key, sh.scroll())) } else { None })
            .collect();
        StackMemory { scroll: self.scroll.pos, shelves }
    }

    /// Adopt a saved viewport without gliding to it; applied by the next `on`, once the sections
    /// it names exist.
    pub fn restore(&mut self, m: &StackMemory<K>) {
        self.scroll.jump(m.scroll);
        self.target = m.scroll;
        self.restore = Some(m.clone());
    }

    /// The page's motion state (see the module doc).
    pub fn write(&self, c: &mut Canon) {
        self.write_scroll(c);
        c.seq(self.bodies.len());
        for b in &self.bodies {
            match b {
                Body::Shelf(s) => s.write(c),
                Body::Grid(g) => {
                    g.write_bands(c);
                    g.write_pop(c);
                }
                Body::Plain => {}
            }
        }
    }

    // ---- layout ----------------------------------------------------------------------------

    fn refresh<H: Host, P: StackPage<H, Key = K>>(&mut self, p: &P, cx: &Cx<'_, H>) {
        let rev = p.revision(cx);
        if self.revision == Some(rev) {
            return;
        }
        self.revision = Some(rev);
        #[cfg(test)]
        {
            self.rebuilds += 1;
        }
        let mut specs = Vec::new();
        p.sections(cx, &mut specs);
        let mut old: Vec<Option<(K, Body)>> =
            std::mem::take(&mut self.specs).into_iter().zip(std::mem::take(&mut self.bodies)).map(|(s, b)| Some((s.key, b))).collect();
        self.bodies = specs
            .iter()
            .map(|s| {
                let kept = old.iter_mut().find(|o| o.as_ref().is_some_and(|(k, _)| *k == s.key)).and_then(Option::take);
                match (s.kind, kept) {
                    (Kind::Shelf { .. }, Some((_, b @ Body::Shelf(_)))) => b,
                    (Kind::Grid { .. }, Some((_, b @ Body::Grid(_)))) => b,
                    (Kind::Shelf { style, .. }, _) => Body::Shelf(Shelf::new(self.entry, style)),
                    (Kind::Grid { spec }, _) => Body::Grid(Grid::new(self.entry, spec.external())),
                    _ => Body::Plain,
                }
            })
            .collect();
        self.order.clear();
        self.order.extend(0..specs.len());
        self.order.sort_by_key(|&i| specs[i].rank);
        self.queue.retain(|(k, _)| specs.iter().any(|s| s.key == *k));
        self.tops.reserve(specs.len());
        self.queue.reserve(specs.len());
        self.specs = specs;
    }

    fn len_of<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, i: usize) -> usize {
        p.cards(cx, self.specs[i].key).map_or(0, |c| c.len())
    }

    /// Section `i`'s height in the flow.
    fn height<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, i: usize) -> f32 {
        match (&self.specs[i].kind, &self.bodies[i]) {
            (Kind::Shelf { style, heading }, Body::Shelf(s)) => {
                if self.len_of(p, cx, i) == 0 { 0.0 } else { heading + style.h + s.under_band() + p.shelf_foot(self.specs[i].key) }
            }
            (Kind::Grid { .. }, Body::Grid(g)) => g.height(self.len_of(p, cx, i)),
            (Kind::Custom { height, .. }, _) => *height,
            _ => 0.0,
        }
    }

    /// Section `i`'s top in document space: the cached one, else (before the first pass) summed.
    fn top<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, i: usize) -> f32 {
        match self.tops.get(i) {
            Some(t) if self.tops.len() == self.specs.len() => *t,
            _ => (0..i).map(|j| self.height(p, cx, j)).sum(),
        }
    }

    /// Re-measure every section's top (one pass, no allocation once the buffer has grown).
    fn retop<H: Host, P: StackPage<H, Key = K>>(&mut self, p: &P, cx: &Cx<'_, H>) {
        let mut tops = std::mem::take(&mut self.tops);
        tops.clear();
        let mut y = 0.0;
        for i in 0..self.specs.len() {
            tops.push(y);
            y += self.height(p, cx, i);
        }
        self.tops = tops;
    }

    /// Hand every grid the page: its top in document space and the scroll.
    fn sync_pages<H: Host, P: StackPage<H, Key = K>>(&mut self, p: &P, cx: &Cx<'_, H>) {
        self.retop(p, cx);
        for i in 0..self.bodies.len() {
            if let Body::Grid(g) = &mut self.bodies[i] {
                g.set_page(self.tops[i], self.scroll.pos);
            }
        }
    }

    /// Index of `elem` among the elements of plain section `i` (`None`: not one of them).
    fn plain_index<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, i: usize, elem: &H::Elem) -> Option<usize> {
        let k = self.specs[i].key;
        (0..p.plain_len(k)).find(|&n| p.plain_elem(k, n).as_ref() == Some(elem))
    }

    /// The section showing `elem`.
    fn owner<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, elem: &H::Elem) -> Option<usize> {
        (0..self.specs.len()).find(|&i| self.shows(p, cx, i, elem))
    }

    fn shows<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, i: usize, elem: &H::Elem) -> bool {
        let k = self.specs[i].key;
        match self.specs[i].kind {
            Kind::Shelf { .. } | Kind::Grid { .. } => p.cards(cx, k).is_some_and(|c| c.index_of(elem).is_some()),
            Kind::Custom { focusable, .. } | Kind::Overlay { focusable, .. } => focusable && self.plain_index(p, i, elem).is_some(),
        }
    }

    fn focused<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>) -> Option<(usize, usize)> {
        let key = cx.focus.current.filter(|k| k.entry == self.entry)?;
        let i = self.owner(p, cx, &key.elem)?;
        let at = match self.specs[i].kind {
            Kind::Custom { .. } | Kind::Overlay { .. } => self.plain_index(p, i, &key.elem),
            _ => p.cards(cx, self.specs[i].key).and_then(|c| c.index_of(&key.elem)),
        }
        .unwrap_or(0);
        Some((i, at))
    }

    /// Section `i`'s rect on screen (a plain section; a shelf's block).
    fn rect<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, i: usize) -> Rect {
        match self.specs[i].kind {
            Kind::Overlay { rect, .. } => rect,
            _ => Rect::new(0.0, self.top(p, cx, i) - self.scroll.pos, SCR_W, self.height(p, cx, i)),
        }
    }

    fn frame<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, i: usize) -> SectionFrame {
        let heading = if let Kind::Shelf { heading, .. } = self.specs[i].kind { heading } else { 0.0 };
        SectionFrame { y: self.top(p, cx, i) - self.scroll.pos + heading, clip: self.clip }
    }

    // ---- events ----------------------------------------------------------------------------

    /// The one entry: feed it EVERY event the screen receives. It consumes Tick, FocusMoved and
    /// the activations, and says "handled" never: a page still observes `FocusMoved` for whatever
    /// else it keeps.
    pub fn on<H: Host<Elem = u32>, P: StackPage<H, Key = K>>(
        &mut self,
        p: &P,
        ev: &ScreenEvent<H>,
        cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>,
    ) -> Option<StackEvent<K, u32>> {
        self.refresh(p, cx);
        if let Some(m) = self.restore.take() {
            for (k, scroll) in &m.shelves {
                if let Some(i) = self.index(*k) {
                    let n = self.len_of(p, cx, i);
                    if let Body::Shelf(s) = &mut self.bodies[i] {
                        s.restore_scroll(*scroll, n);
                    }
                }
            }
        }
        self.sync_pages(p, cx);
        let mut reported = None;
        for i in 0..self.bodies.len() {
            let k = self.specs[i].key;
            let Some(src) = p.cards(cx, k) else { continue };
            let got = match &mut self.bodies[i] {
                Body::Shelf(s) => s.on(ev, cx, &src, fx),
                Body::Grid(g) => g.on(ev, cx, &src, fx),
                Body::Plain => None,
            };
            match got {
                Some(CardEvent::Want(r)) => self.queue.push((k, r)),
                Some(c) => reported = Some(StackEvent::Card(k, c)),
                None => {}
            }
        }
        match ev {
            ScreenEvent::Tick(t) => {
                self.tick(t.dt(), p, cx, fx);
                if let Some(h) = &self.hint {
                    let input = if h.wants_input(cx.press.held_ms.is_some()) {
                        self.hint_input(p, cx)
                    } else {
                        crate::hold_hint::HintInput::default()
                    };
                    if let Some(h) = &mut self.hint {
                        h.step(input, t.ms, t.dt(), &mut |ev| fx.note(ev));
                    }
                }
                if reported.is_none() && !self.queue.is_empty() {
                    let (k, r) = self.queue.remove(0);
                    reported = Some(StackEvent::Card(k, CardEvent::Want(r)));
                }
            }
            ScreenEvent::FocusMoved { to, .. } if self.on_move => self.reveal_on(p, cx, *to),
            ScreenEvent::Activate(e) => {
                reported = reported.or_else(|| {
                    let i = self.owner(p, cx, e)?;
                    matches!(self.bodies[i], Body::Plain).then(|| StackEvent::Press(self.specs[i].key))
                });
            }
            _ => {}
        }
        if !matches!(ev, ScreenEvent::Tick(_)) {
            self.sync_pages(p, cx);
        }
        reported
    }

    /// Under [`reveal_on_move`](Self::reveal_on_move): bring the focused element's block into view
    /// now, as a `FocusMoved` to it would (a page whose sections changed under a standing focus).
    pub fn reveal<H: Host<Elem = u32>, P: StackPage<H, Key = K>>(&mut self, p: &P, cx: &Cx<'_, H>) {
        if let Some(key) = cx.focus.current {
            self.reveal_on(p, cx, key);
        }
    }

    fn reveal_on<H: Host, P: StackPage<H, Key = K>>(&mut self, p: &P, cx: &Cx<'_, H>, to: FocusKey<H::Elem>) {
        self.refresh(p, cx);
        self.target = self.owner(p, cx, &to.elem).filter(|_| to.entry == self.entry).map_or(0.0, |i| self.wanted(p, cx, i));
    }

    fn tick<H: Host<Elem = u32>, P: StackPage<H, Key = K>>(&mut self, dt: f32, p: &P, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let focus = self.focused(p, cx);
        if let Some((i, at)) = focus {
            self.last = Some((self.specs[i].key, at));
            if let Some(key) = cx.focus.current {
                self.last_elem = key.elem;
            }
        }
        // a content landing moved the focused row: the document follows so the tile stays put
        let shift: f32 = self.bodies.iter().map(|b| if let Body::Grid(g) = b { g.landed_shift() } else { 0.0 }).sum();
        if shift != 0.0 {
            self.scroll.pos += shift;
            self.target += shift;
        }
        match focus {
            _ if self.on_move => {}
            Some((i, _)) if !matches!(self.specs[i].kind, Kind::Overlay { .. }) => self.target = self.wanted(p, cx, i),
            _ if self.home => self.target = 0.0,
            _ => {}
        }
        self.scroll.step(self.target, K_SCROLL, dt);
        if (self.scroll.pos - self.target).abs() > 0.25 || self.scroll.vel.abs() > 0.5 {
            fx.note(PresentEvent::Motion);
        }
        self.sync_pages(p, cx);
    }

    /// Section `n`'s height with focus on section `focus`, measured at its DESTINATION: a shelf's
    /// caption band open when focused and closed otherwise, wherever its spring is now. A scroll
    /// target computed against the live bands chases a moving goal for as long as they animate.
    fn settled_height<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, n: usize, focus: usize) -> f32 {
        match &self.specs[n].kind {
            Kind::Shelf { style, heading } => {
                if self.len_of(p, cx, n) == 0 {
                    0.0
                } else {
                    heading + style.h + card_row::under_band((n == focus) as i32 as f32) + p.shelf_foot(self.specs[n].key)
                }
            }
            _ => self.height(p, cx, n),
        }
    }

    /// The scroll that shows section `i` focused: the grid's own reveal rule, the minimal reveal
    /// of a shelf's block (against its settled bands) or of a plain section (or the block the page
    /// names, [`StackPage::reveal_with`]) otherwise.
    fn wanted<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, i: usize) -> f32 {
        if let Body::Grid(g) = &self.bodies[i] {
            return g.reveal_target().unwrap_or(self.target);
        }
        if matches!(self.specs[i].kind, Kind::Overlay { .. }) {
            return self.target;
        }
        let j = p.reveal_with(self.specs[i].key).and_then(|k| self.index(k)).unwrap_or(i);
        let h = |n: usize| self.settled_height(p, cx, n, i);
        let (top, height) = ((0..j).map(h).sum::<f32>(), h(j));
        let max = self.max_with_focus(p, cx, i);
        let hi = if matches!(self.specs[j].kind, Kind::Shelf { .. }) { top - p.reveal_margin(self.specs[j].key) } else { top };
        // a page that sets the target itself reveals from where it is headed, not where it is
        let from = if self.on_move { self.target } else { self.scroll.pos };
        card_row::reveal(from, top + height - (SCR_H - MARGIN_Y), hi, max)
    }

    // ---- placement -------------------------------------------------------------------------

    /// Where the focused card is, with its section: the opener's and the page's own handle on it.
    pub fn focused_card<H: Host<Elem = u32>, P: StackPage<H, Key = K>>(
        &self,
        p: &P,
        cx: &Cx<'_, H>,
        how: At,
    ) -> Option<(K, Placed)> {
        let key = cx.focus.current.filter(|k| k.entry == self.entry)?;
        let i = self.owner(p, cx, &key.elem)?;
        if matches!(self.bodies[i], Body::Plain) {
            return None;
        }
        Some((self.specs[i].key, self.place_in(p, cx, i, &key.elem, how)?))
    }

    /// The hold hint's two screen-specific answers for a `Stack` page (`hold_hint`, "Adopting it"):
    /// the CARD focus rests on (a `Plain` section's control, another page's focus, a menu or modal
    /// over the page — then focus is not this entry's — all answer `None`), and whether nothing
    /// under it is still moving: the page scroll at its target, and the card drawn where it comes
    /// to rest (the shelf's glide and the focus pop). ([`HintInput::for_placed`](crate::hold_hint::HintInput::for_placed)
    /// owns the rule, pinned clocks included.) The press clock is passed straight through.
    fn hint_input<H: Host<Elem = u32>, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>) -> crate::hold_hint::HintInput {
        let Some(key) = cx.focus.current.filter(|k| k.entry == self.entry) else {
            return crate::hold_hint::HintInput::default();
        };
        let Some((_, at)) = self.focused_card(p, cx, At::Drawn).filter(|(sec, _)| p.card_has_menu(cx, *sec, &key.elem))
        else {
            return crate::hold_hint::HintInput::default();
        };
        crate::hold_hint::HintInput::for_placed(key.elem, at, (self.scroll.pos - self.target).abs() < 0.5, cx.press.held_ms)
    }

    fn place_in<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, i: usize, elem: &H::Elem, how: At) -> Option<Placed> {
        let k = self.specs[i].key;
        match &self.bodies[i] {
            Body::Shelf(s) => s.place(cx, &p.cards(cx, k)?, elem, self.frame(p, cx, i), how),
            Body::Grid(g) => g.place(cx, &p.cards(cx, k)?, elem, how),
            Body::Plain => {
                let n = self.plain_index(p, i, elem)?;
                let rect = p.element_rect(cx, k, n, self.rect(p, cx, i));
                Some(Placed { rect, rest_rect: rect, clip: self.clip, index: Some(n as u32) })
            }
        }
    }

    /// The page's view: `Focusable` and `Part`.
    pub fn view<'a, P>(&'a self, p: &'a P) -> StackView<'a, K, P> {
        StackView { stack: self, page: p }
    }
}

pub struct StackView<'a, K, P> {
    stack: &'a Stack<K>,
    page: &'a P,
}

impl<K: Copy + Eq, P> StackView<'_, K, P> {
    /// The stops of every section, registered as [`paint`](Self::paint) ends each section with
    /// them (the very rects it draws, in the same z order), without painting: a host test has no
    /// GL context, and a page's harness reads the rects through this. Paint culls a shelf wholly
    /// off the screen, so it registers no stops for it; this registers them all.
    pub fn record_stops<H: Host>(&self, f: &mut DrawFrame<'_, '_, H>)
    where
        P: StackPage<H, Key = K>,
    {
        let s = self.stack;
        for i in 0..s.specs.len() {
            let k = s.specs[i].key;
            match (&s.specs[i].kind, &s.bodies[i]) {
                (Kind::Shelf { .. }, Body::Shelf(sh)) => if let Some(src) = self.page.cards(f.cx, k) { sh.record_stops(f, f.painter, &src, s.frame(self.page, f.cx, i)) },
                (Kind::Grid { .. }, Body::Grid(g)) => if let Some(src) = self.page.cards(f.cx, k) { g.record_stops(f, f.painter, &src) },
                (Kind::Custom { focusable, .. } | Kind::Overlay { focusable, .. }, _) => self.plain_stop(f, i, *focusable),
                _ => {}
            }
        }
    }

    /// The stop of section `i` when it is a focusable `Custom` / `Overlay`.
    fn plain_stop<H: Host>(&self, f: &mut DrawFrame<'_, '_, H>, i: usize, focusable: bool)
    where
        P: StackPage<H, Key = K>,
    {
        let (s, page) = (self.stack, self.page);
        let (k, r) = (s.specs[i].key, s.rect(page, f.cx, i));
        if !focusable {
            return;
        }
        for n in 0..page.plain_len(k) {
            let Some(elem) = page.plain_elem(k, n) else { continue };
            let rect = page.element_rect(f.cx, k, n, r);
            // a control wholly off the screen can be neither pointed at nor allowed to sit under a
            // card in the hit map's z order
            if !crate::on_axis(rect.y, rect.h, SCR_H, 0.0) {
                continue;
            }
            f.stop(
                f.painter,
                Stop {
                    key: FocusKey { entry: s.entry, elem },
                    rect,
                    rest_rect: rect,
                    clip: s.clip,
                    hover: Hover::Focus,
                    activate: Activate::Direct,
                },
            );
        }
    }

    /// The opener redraw: card `focus` drawn alone, popped and captioned exactly as in-page, over
    /// whatever covers the page. Nothing is painted for an element no card section shows.
    pub fn redraw_focused<H: Host>(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<FocusKey<H::Elem>>)
    where
        P: StackPage<H, Key = K>,
    {
        let (s, page) = (self.stack, self.page);
        let Some(key) = focus.filter(|k| k.entry == s.entry) else { return };
        let Some(i) = s.owner(page, f.cx, &key.elem) else { return };
        let Some(src) = page.cards(f.cx, s.specs[i].key) else { return };
        let p = f.painter.alpha(f.page_alpha);
        match &s.bodies[i] {
            Body::Shelf(sh) => sh.redraw_focused(f, p, &src, s.frame(page, f.cx, i), focus),
            Body::Grid(g) => g.redraw_focused(f, p, &src, focus),
            Body::Plain => {}
        }
    }

    /// The live pop of card `elem` (no press), in whichever card section shows it.
    pub fn scale_of<H: Host>(&self, cx: &Cx<'_, H>, elem: &H::Elem) -> Option<f32>
    where
        P: StackPage<H, Key = K>,
    {
        let (s, page) = (self.stack, self.page);
        let i = s.owner(page, cx, elem)?;
        let src = page.cards(cx, s.specs[i].key)?;
        match &s.bodies[i] {
            Body::Shelf(sh) => sh.scale_of(cx, &src, elem),
            Body::Grid(g) => g.scale_of(cx, &src, elem),
            Body::Plain => None,
        }
    }

    pub fn paint<H: Host>(&self, f: &mut DrawFrame<'_, '_, H>)
    where
        P: StackPage<H, Key = K>,
    {
        let (s, page) = (self.stack, self.page);
        let p = f.painter.alpha(f.page_alpha);
        for i in 0..s.specs.len() {
            let k = s.specs[i].key;
            let r = s.rect(page, f.cx, i);
            match (&s.specs[i].kind, &s.bodies[i]) {
                (Kind::Shelf { .. }, Body::Shelf(sh)) => {
                    if r.h <= 0.0 || r.y >= SCR_H || r.y + r.h <= 0.0 {
                        continue;
                    }
                    let Some(src) = page.cards(f.cx, k) else { continue };
                    page.draw_heading(k, f, r, sh.heading_lift());
                    sh.draw(f, p, &src, s.frame(page, f.cx, i));
                }
                (Kind::Grid { .. }, Body::Grid(g)) => {
                    if let Some(src) = page.cards(f.cx, k) {
                        g.draw(f, p, &src);
                    }
                }
                (Kind::Custom { focusable, .. } | Kind::Overlay { focusable, .. }, _) => {
                    page.custom_draw(k, f, r, s.scroll.pos);
                    self.plain_stop(f, i, *focusable);
                }
                _ => {}
            }
        }
        s.draw_hold_hint(p, f.measure);
    }
}

impl<K: Copy + Eq, H: Host<Elem = u32>, P: StackPage<H, Key = K>> Part<H> for StackView<'_, K, P> {
    fn prepare(&mut self, _b: &mut crate::frame::Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>, _rect: Rect) {
        self.paint(f);
    }
}

impl<K: Copy + Eq, P> StackView<'_, K, P> {
    /// Element `n` (clamped) of section `i`, or its one element when it is a focusable plain one.
    fn nth<H: Host>(&self, cx: &Cx<'_, H>, i: usize, n: usize) -> Option<H::Elem>
    where
        P: StackPage<H, Key = K>,
    {
        let (s, p) = (self.stack, self.page);
        let k = s.specs[i].key;
        match s.specs[i].kind {
            Kind::Shelf { .. } | Kind::Grid { .. } => {
                let c = p.cards(cx, k)?;
                Some(c.elem(super::clamp_slot(n, c.len())?))
            }
            Kind::Custom { focusable, .. } | Kind::Overlay { focusable, .. } => {
                p.plain_elem(k, super::clamp_slot(n, p.plain_len(k))?).filter(|_| focusable)
            }
        }
    }

    /// The first focusable of the page's fallback order; when nothing is focusable, the element of
    /// the LAST section of that order that has one (the page's home, e.g. its header), focusable
    /// or not.
    fn fallback_key<H: Host>(&self, cx: &Cx<'_, H>) -> Option<FocusKey<H::Elem>>
    where
        P: StackPage<H, Key = K>,
    {
        let mut order = Vec::new();
        self.page.fallback(cx, &mut order);
        order
            .iter()
            .find_map(|&k| self.stack.index(k).and_then(|i| self.nth(cx, i, 0)))
            .or_else(|| order.iter().rev().find_map(|&k| self.page.plain_elem(k, 0)))
            .map(|elem| FocusKey { entry: self.stack.entry, elem })
    }
}

impl<K: Copy + Eq, H: Host<Elem = u32>, P: StackPage<H, Key = K>> Focusable<H> for StackView<'_, K, P> {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        let (s, p) = (self.stack, self.page);
        for &i in &s.order {
            let (k, id) = (s.specs[i].key, s.specs[i].group);
            match (&s.specs[i].kind, &s.bodies[i]) {
                (Kind::Shelf { .. }, Body::Shelf(sh)) => {
                    let Some(src) = p.cards(cx, k).filter(|c| c.len() > 0) else { continue };
                    let h = sh.head(s.frame(p, cx, i));
                    let extent = if p.wide_extent(k) { Rect::new(h.x, h.y, SCR_W - 2.0 * h.x, h.h) } else { h };
                    let mut g = sh.group_spec(id, src.len(), extent);
                    s.specs[i].apply(&mut g);
                    out.push(g);
                }
                (Kind::Grid { .. }, Body::Grid(g)) => {
                    if let Some(src) = p.cards(cx, k).filter(|c| c.len() > 0) {
                        let mut spec = g.group_spec(id, src.len());
                        s.specs[i].apply(&mut spec);
                        out.push(spec);
                    }
                }
                (Kind::Custom { focusable: true, .. } | Kind::Overlay { focusable: true, .. }, _) => {
                    if p.plain_len(k) > 0 {
                        let mut g = p.plain_group(cx, k, id, p.focus_rect(cx, k, s.rect(p, cx, i)));
                        s.specs[i].apply(&mut g);
                        out.push(g);
                    }
                }
                _ => {}
            }
        }
    }

    fn group_of(&self, key: &H::Elem, cx: &Cx<'_, H>) -> Option<GroupId> {
        self.stack.owner(self.page, cx, key).map(|i| self.stack.specs[i].group)
    }

    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, cx: &Cx<'_, H>) -> Step<H::Elem> {
        let (s, p) = (self.stack, self.page);
        let Some(i) = s.owner(p, cx, &key.elem) else { return Step::Edge };
        let src = p.cards(cx, s.specs[i].key);
        match (&s.bodies[i], src) {
            (Body::Shelf(sh), Some(src)) => sh.neighbour(&src, key, dir),
            (Body::Grid(g), Some(src)) => g.neighbour(&src, key, dir),
            (Body::Plain, _) => {
                let k = s.specs[i].key;
                let step = s.plain_index(p, i, &key.elem).and_then(|at| p.plain_step(k, at, dir));
                step.and_then(|n| p.plain_elem(k, n)).map_or(Step::Edge, |elem| Step::Move(FocusKey { entry: s.entry, elem }))
            }
            _ => Step::Edge,
        }
    }

    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        let i = self.stack.owner(self.page, cx, key)?;
        self.stack.place_in(self.page, cx, i, key, at)
    }

    fn reconcile(&self, want: FocusKey<H::Elem>, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let (s, p) = (self.stack, self.page);
        let at = |elem| FocusKey { entry: s.entry, elem };
        if s.owner(p, cx, &want.elem).is_some() {
            return at(want.elem);
        }
        // not laid out yet (no event has reached the page): nothing can be judged gone, so the
        // wanted focus stands until the first layout
        if s.revision.is_none() {
            return want;
        }
        if p.pending(cx, &want.elem) {
            return want;
        }
        if let Some(elem) = p.recover(cx, &want.elem) {
            return at(elem);
        }
        if let Some(elem) = s.last.and_then(|(k, n)| s.index(k).and_then(|i| self.nth(cx, i, n))) {
            return at(elem);
        }
        self.fallback_key(cx).unwrap_or(want)
    }

    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let (s, p) = (self.stack, self.page);
        let at = |elem| FocusKey { entry: s.entry, elem };
        let seated = s.index_of_group(g).and_then(|i| {
            let k = s.specs[i].key;
            if let Some(elem) = p.seat_override(cx, k, from) {
                return Some(at(elem));
            }
            match &s.bodies[i] {
                Body::Shelf(sh) => p.cards(cx, k).and_then(|c| sh.seat(&c, from)),
                Body::Grid(gr) => p.cards(cx, k).and_then(|c| gr.seat(&c, from)),
                Body::Plain => p.plain_elem(k, 0).map(at),
            }
        });
        // `groups` lists only a section that has an element, and the engine seats only into a
        // listed group; a group the page no longer lists (it emptied in between) degrades to the
        // page's fallback, then to any element it shows, then to where focus last was
        seated
            .or_else(|| self.fallback_key(cx))
            .or_else(|| (0..s.specs.len()).find_map(|i| self.nth(cx, i, 0)).map(at))
            .unwrap_or(FocusKey { entry: s.entry, elem: s.last_elem })
    }
}
