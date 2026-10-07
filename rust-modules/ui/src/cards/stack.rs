//! [`Stack`] — the vertical page of card sections (shared-card-sections plan, layer L2).
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
//! - **Cost.** `sections` runs again only when [`StackPage::revision`] moves, so a page puts
//!   everything its section list depends on (status, a truncated summary, the item count) in it.
//! - **Reconcile.** The focused element is kept when a section still shows it; else a page still
//!   [`pending`](StackPage::pending) keeps the wanted focus; else the same position clamped in the
//!   section it was in; else the first focusable of [`StackPage::fallback`].
//! - **Motion canon.** [`Stack::write`] is the page's motion state: the scroll's spring and every
//!   section's pop, bands and scroll.

use std::marker::PhantomData;

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
    /// `focusable` it is one focus element ([`StackPage::elem_of`]) that reports [`StackEvent::Press`].
    Custom { height: f32, focusable: bool },
    /// Out of flow at a fixed screen rect: a status read-out, a Retry action.
    Overlay { rect: Rect, focusable: bool },
}

#[derive(Clone, Copy)]
pub struct SectionSpec<K> {
    pub key: K,
    pub kind: Kind,
}

/// What [`Stack::on`] reports. One at most per call (further ones are queued for the next).
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
    fn sections(&self, cx: &Cx<'_, H>, out: &mut Vec<SectionSpec<Self::Key>>);
    /// The page-level order focus falls back through when its section empties.
    fn fallback(&self, cx: &Cx<'_, H>, out: &mut Vec<Self::Key>);
    /// The cards of a `Shelf` / `Grid` section; `None` while it has no content.
    fn cards<'a>(&'a self, cx: &'a Cx<'_, H>, k: Self::Key) -> Option<Self::Cards<'a>>;
    /// The one engine element of a focusable `Custom` / `Overlay` section.
    fn elem_of(&self, _k: Self::Key) -> Option<H::Elem> {
        None
    }
    /// The focus rect (and hit target) of a focusable `Custom` / `Overlay` section, given the
    /// section's own rect on screen.
    fn focus_rect(&self, _cx: &Cx<'_, H>, _k: Self::Key, section: Rect) -> Rect {
        section
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

pub struct Stack<K, E = u32> {
    entry: EntryId,
    specs: Vec<SectionSpec<K>>,
    bodies: Vec<Body>,
    revision: Option<u64>,
    scroll: Spring,
    target: f32,
    /// Scroll home when focus is not in the page's flow ([`Stack::home_when_unfocused`]).
    home: bool,
    /// The section and position focus was last on, for the reconcile rule.
    last: Option<(K, usize)>,
    queue: Vec<(K, CardEvent<u32>)>,
    restore: Option<StackMemory<K>>,
    /// How many times `sections` ran (the cost case's counter).
    #[cfg(test)]
    pub(crate) rebuilds: u32,
    _elem: PhantomData<E>,
}

fn blank<K: Copy>() -> Vec<SectionSpec<K>> {
    Vec::new()
}

impl<K: Copy + Eq, E> Stack<K, E> {
    pub fn new(entry: EntryId) -> Self {
        Self {
            entry,
            specs: blank(),
            bodies: Vec::new(),
            revision: None,
            scroll: Spring::at(0.0),
            target: 0.0,
            home: false,
            last: None,
            queue: Vec::new(),
            restore: None,
            #[cfg(test)]
            rebuilds: 0,
            _elem: PhantomData,
        }
    }

    /// Scroll the page home (0) whenever focus is not on a section in the flow: nothing of this
    /// page focused (a menu over it, the pointer gone) or an out-of-flow [`Kind::Overlay`] held.
    /// Off by default: a page whose focus is elsewhere then stays where it is.
    pub fn home_when_unfocused(mut self, on: bool) -> Self {
        self.home = on;
        self
    }

    pub fn scroll(&self) -> f32 {
        self.scroll.pos
    }

    #[cfg(test)]
    pub(crate) fn target(&self) -> f32 {
        self.target
    }

    /// The focus group of section `k` (its position in the current layout).
    pub fn group(&self, k: K) -> Option<GroupId> {
        self.index(k).map(|i| GroupId(i as u32))
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
        c.f32(self.scroll.pos).f32(self.scroll.vel).f32(self.target).seq(self.bodies.len());
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
        self.specs = specs;
    }

    fn len_of<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, i: usize) -> usize {
        p.cards(cx, self.specs[i].key).map_or(0, |c| c.len())
    }

    /// Section `i`'s height in the flow.
    fn height<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, i: usize) -> f32 {
        match (&self.specs[i].kind, &self.bodies[i]) {
            (Kind::Shelf { style, heading }, Body::Shelf(s)) => {
                if self.len_of(p, cx, i) == 0 { 0.0 } else { heading + style.h + s.under_band() }
            }
            (Kind::Grid { .. }, Body::Grid(g)) => g.height(self.len_of(p, cx, i)),
            (Kind::Custom { height, .. }, _) => *height,
            _ => 0.0,
        }
    }

    /// Section `i`'s top in document space.
    fn top<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, i: usize) -> f32 {
        (0..i).map(|j| self.height(p, cx, j)).sum()
    }

    /// Hand every grid the page: its top in document space and the scroll.
    fn sync_pages<H: Host, P: StackPage<H, Key = K>>(&mut self, p: &P, cx: &Cx<'_, H>) {
        for i in 0..self.bodies.len() {
            if matches!(self.bodies[i], Body::Grid(_)) {
                let top = self.top(p, cx, i);
                if let Body::Grid(g) = &mut self.bodies[i] {
                    g.set_page(top, self.scroll.pos);
                }
            }
        }
    }

    /// The section showing `elem`.
    fn owner<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, elem: &H::Elem) -> Option<usize> {
        (0..self.specs.len()).find(|&i| self.shows(p, cx, i, elem))
    }

    fn shows<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, i: usize, elem: &H::Elem) -> bool {
        let k = self.specs[i].key;
        match self.specs[i].kind {
            Kind::Shelf { .. } | Kind::Grid { .. } => p.cards(cx, k).is_some_and(|c| c.index_of(elem).is_some()),
            Kind::Custom { focusable, .. } | Kind::Overlay { focusable, .. } => focusable && p.elem_of(k) == Some(*elem),
        }
    }

    fn focused<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>) -> Option<(usize, usize)> {
        let key = cx.focus.current.filter(|k| k.entry == self.entry)?;
        let i = self.owner(p, cx, &key.elem)?;
        let at = p.cards(cx, self.specs[i].key).and_then(|c| c.index_of(&key.elem)).unwrap_or(0);
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
        SectionFrame { y: self.top(p, cx, i) - self.scroll.pos + heading, clip: Rect::FULL }
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
            if let Some(c) = got {
                if reported.is_none() { reported = Some(StackEvent::Card(k, c)); } else { self.queue.push((k, c)); }
            }
        }
        match ev {
            ScreenEvent::Tick(t) => self.tick(t.dt(), p, cx, fx),
            ScreenEvent::Activate(e) => {
                reported = reported.or_else(|| {
                    let i = self.owner(p, cx, e)?;
                    matches!(self.bodies[i], Body::Plain).then(|| StackEvent::Press(self.specs[i].key))
                });
            }
            _ => {}
        }
        if reported.is_none() && !self.queue.is_empty() {
            let (k, c) = self.queue.remove(0);
            reported = Some(StackEvent::Card(k, c));
        }
        reported
    }

    fn tick<H: Host<Elem = u32>, P: StackPage<H, Key = K>>(&mut self, dt: f32, p: &P, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let focus = self.focused(p, cx);
        if let Some((i, at)) = focus {
            self.last = Some((self.specs[i].key, at));
        }
        // a content landing moved the focused row: the document follows so the tile stays put
        let shift: f32 = self.bodies.iter().map(|b| if let Body::Grid(g) = b { g.landed_shift() } else { 0.0 }).sum();
        if shift != 0.0 {
            self.scroll.pos += shift;
            self.target += shift;
        }
        match focus {
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
                if self.len_of(p, cx, n) == 0 { 0.0 } else { heading + style.h + card_row::under_band((n == focus) as i32 as f32) }
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
        let max = ((0..self.specs.len()).map(h).sum::<f32>() - (SCR_H - MARGIN_Y)).max(0.0);
        let hi = if matches!(self.specs[j].kind, Kind::Shelf { .. }) { top - MARGIN_Y } else { top };
        card_row::reveal(self.scroll.pos, top + height - (SCR_H - MARGIN_Y), hi, max)
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

    fn place_in<H: Host, P: StackPage<H, Key = K>>(&self, p: &P, cx: &Cx<'_, H>, i: usize, elem: &H::Elem, how: At) -> Option<Placed> {
        let k = self.specs[i].key;
        match &self.bodies[i] {
            Body::Shelf(s) => s.place(cx, &p.cards(cx, k)?, elem, self.frame(p, cx, i), how),
            Body::Grid(g) => g.place(cx, &p.cards(cx, k)?, elem, how),
            Body::Plain => {
                let rect = p.focus_rect(cx, k, self.rect(p, cx, i));
                (p.elem_of(k).as_ref() == Some(elem)).then_some(Placed { rect, rest_rect: rect, clip: Rect::FULL, index: Some(0) })
            }
        }
    }

    /// The page's view: `Focusable` and `Part`.
    pub fn view<'a, P>(&'a self, p: &'a P) -> StackView<'a, K, E, P> {
        StackView { stack: self, page: p }
    }
}

pub struct StackView<'a, K, E, P> {
    stack: &'a Stack<K, E>,
    page: &'a P,
}

impl<K: Copy + Eq, E, P> StackView<'_, K, E, P> {
    /// The stops of every section, registered exactly as [`paint`](Self::paint) ends each section
    /// with them (the very rects it draws, in the same z order), without painting: a host test
    /// has no GL context, and a page's harness reads the rects through this.
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
        if let (true, Some(elem)) = (focusable, page.elem_of(k)) {
            let rect = page.focus_rect(f.cx, k, r);
            // a control wholly off the screen can be neither pointed at nor allowed to sit under a
            // card in the hit map's z order
            if !crate::on_axis(rect.y, rect.h, SCR_H, 0.0) {
                return;
            }
            f.stop(
                f.painter,
                Stop {
                    key: FocusKey { entry: s.entry, elem },
                    rect,
                    rest_rect: rect,
                    clip: Rect::FULL,
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
    }
}

impl<K: Copy + Eq, E, H: Host, P: StackPage<H, Key = K>> Part<H> for StackView<'_, K, E, P> {
    fn prepare(&mut self, _b: &mut crate::frame::Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>, _rect: Rect) {
        self.paint(f);
    }
}

impl<K: Copy + Eq, E, P> StackView<'_, K, E, P> {
    /// Element `n` (clamped) of section `i`, or its one element when it is a focusable plain one.
    fn nth<H: Host>(&self, cx: &Cx<'_, H>, i: usize, n: usize) -> Option<H::Elem>
    where
        P: StackPage<H, Key = K>,
    {
        let (s, p) = (self.stack, self.page);
        let k = s.specs[i].key;
        match s.specs[i].kind {
            Kind::Shelf { .. } | Kind::Grid { .. } => {
                let c = p.cards(cx, k).filter(|c| c.len() > 0)?;
                Some(c.elem(n.min(c.len() - 1)))
            }
            Kind::Custom { focusable, .. } | Kind::Overlay { focusable, .. } => p.elem_of(k).filter(|_| focusable),
        }
    }

    /// The first focusable of the page's fallback order.
    fn fallback_key<H: Host>(&self, cx: &Cx<'_, H>) -> Option<FocusKey<H::Elem>>
    where
        P: StackPage<H, Key = K>,
    {
        let mut order = Vec::new();
        self.page.fallback(cx, &mut order);
        order
            .into_iter()
            .find_map(|k| self.stack.index(k).and_then(|i| self.nth(cx, i, 0)))
            .map(|elem| FocusKey { entry: self.stack.entry, elem })
    }
}

impl<K: Copy + Eq, E, H: Host, P: StackPage<H, Key = K>> Focusable<H> for StackView<'_, K, E, P> {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        let (s, p) = (self.stack, self.page);
        for i in 0..s.specs.len() {
            let (k, id) = (s.specs[i].key, GroupId(i as u32));
            match (&s.specs[i].kind, &s.bodies[i]) {
                (Kind::Shelf { .. }, Body::Shelf(sh)) => {
                    let Some(src) = p.cards(cx, k).filter(|c| c.len() > 0) else { continue };
                    let h = sh.head(s.frame(p, cx, i));
                    out.push(sh.group_spec(id, src.len(), Rect::new(h.x, h.y, SCR_W - 2.0 * h.x, h.h)));
                }
                (Kind::Grid { .. }, Body::Grid(g)) => {
                    if let Some(src) = p.cards(cx, k).filter(|c| c.len() > 0) {
                        out.push(g.group_spec(id, src.len()));
                    }
                }
                (Kind::Custom { focusable: true, .. } | Kind::Overlay { focusable: true, .. }, _) => {
                    if p.elem_of(k).is_some() {
                        out.push(p.plain_group(cx, k, id, p.focus_rect(cx, k, s.rect(p, cx, i))));
                    }
                }
                _ => {}
            }
        }
    }

    fn group_of(&self, key: &H::Elem, cx: &Cx<'_, H>) -> Option<GroupId> {
        self.stack.owner(self.page, cx, key).map(|i| GroupId(i as u32))
    }

    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, cx: &Cx<'_, H>) -> Step<H::Elem> {
        let (s, p) = (self.stack, self.page);
        let Some(i) = s.owner(p, cx, &key.elem) else { return Step::Edge };
        let Some(src) = p.cards(cx, s.specs[i].key) else { return Step::Edge };
        match &s.bodies[i] {
            Body::Shelf(sh) => sh.neighbour(&src, key, dir),
            Body::Grid(g) => g.neighbour(&src, key, dir),
            Body::Plain => Step::Edge,
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
        if p.pending(cx, &want.elem) {
            return want;
        }
        if let Some(elem) = s.last.and_then(|(k, n)| s.index(k).and_then(|i| self.nth(cx, i, n))) {
            return at(elem);
        }
        self.fallback_key(cx).unwrap_or(want)
    }

    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let (s, p) = (self.stack, self.page);
        let i = (g.0 as usize).min(s.specs.len().saturating_sub(1));
        let at = |elem| FocusKey { entry: s.entry, elem };
        let k = s.specs[i].key;
        let seated = match &s.bodies[i] {
            Body::Shelf(sh) => p.cards(cx, k).and_then(|c| sh.seat(&c, from)),
            Body::Grid(gr) => p.cards(cx, k).and_then(|c| gr.seat(&c, from)),
            Body::Plain => p.elem_of(k).map(at),
        };
        // `groups` lists only a section that has an element, and the engine seats only into a
        // listed group; a page that emptied in between falls back through the reconcile order
        seated.or_else(|| self.fallback_key(cx)).expect("Stack::seat into a group the page no longer lists")
    }
}
