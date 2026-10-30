//! Tier 1 of the card conformance suite: the eight `conformance` cases run against [`Shelf`] and
//! [`Grid`] on `FixtureHost`, plus focused tests for the pop rule, the geometry, the events and
//! idle. No expected-failure list: every case passes on both components.
use super::conformance::{self, CardHarness, Landing, Mount, Nb, Outcome};
use super::{CardEvent, CardSource, Grid, GridSpec, PageEdge, ScrollMode, SectionFrame, Shelf};
use crate::card_row::{RowStyle, TileLabel};
use crate::fixture::{FixtureHost, FixtureMeasure, FixtureView, FixtureViews};
use crate::poster_grid;
use crate::screen::{At, By, Dir, DrawFrame, Placed, ScreenEvent, Step};
use crate::widgets::Art;
use crate::{Painter, Rect, Spring};
use plx_machine::machine::{
    Canon, Cx, Effects, Enter, EntryId, FocusKey, FocusRead, InputOwner, InstanceId, MachineId, PressId, PressRead, Tick,
};
use plx_machine::present::Present;

const ENTRY: EntryId = EntryId(9);
const SHELF_AT: SectionFrame = SectionFrame::new(300.0, Rect::FULL);
const GRID: GridSpec = GridSpec::new(520.0, 96.0);
const MS: u32 = 16;
/// Four narrow columns from x = 160 in a document the owner scrolls: nothing about it is the
/// Collection page's grid.
const EXT_STYLE: RowStyle = RowStyle { w: 200.0, h: 300.0, gap: 36.0, ..poster_grid::STYLE };
const EXT: GridSpec = GridSpec::new(520.0, 96.0).columns(4, EXT_STYLE, 160.0).external();

type Cx9<'a> = Cx<'a, FixtureHost>;

/// The frame context every rig hands a section: `view` as the store, the tick at `ms`, press
/// `press` on the focused card, the engine's focus on `focus`, entry `ENTRY` the owner.
fn cx9(view: &FixtureView, ms: u32, press: f32, focus: Option<FocusKey<u32>>) -> Cx9<'_> {
    cx9_pressed(view, ms, press, focus, focus)
}

/// [`cx9`] with the press owned by `pressed`, which is not `focus` once a press was abandoned.
fn cx9_pressed(view: &FixtureView, ms: u32, press: f32, focus: Option<FocusKey<u32>>, pressed: Option<FocusKey<u32>>) -> Cx9<'_> {
    Cx {
        views: FixtureViews { store: view },
        tick: Tick { ms, dt_us: 16_667 },
        measure: &FixtureMeasure,
        press: PressRead { scale: press, owner: pressed, ..Default::default() },
        focus: FocusRead { current: focus, ..Default::default() },
        owner: InputOwner::Entry(ENTRY),
    }
}

struct Cards {
    elems: Vec<u32>,
    more: bool,
    /// The store's landing count for the row ([`CardSource::page_epoch`]).
    epoch: u32,
    /// The list goes on before the first card (the window slid off its start).
    before: bool,
    /// The screen rect each card was really painted at, as the overlay hook saw it.
    drawn: std::cell::RefCell<Vec<(u32, Rect)>>,
    /// The same paints in call order with the alpha each was painted at.
    painted: std::cell::RefCell<Vec<(u32, Rect, f32)>>,
    /// The same paints with the `Tile::focused` the overlay was told (a state's wash reads it).
    focused: std::cell::RefCell<Vec<(u32, bool)>>,
    /// Cards from this index on are not interned yet: `elem` answers the shared header id `0`
    /// (`Members::elem`'s `unwrap_or(HEADER_ELEM)`) and `index_of` knows no such element.
    unindexed_from: usize,
    /// The cards of a paged listing whose page is not present (`CardSource::loaded`).
    unloaded: std::ops::Range<usize>,
    /// Where the held window starts in the listing it is a window of ([`CardSource::global`]).
    first: usize,
    /// Every card carries a two-line caption (the Under posters setting's title over its details).
    captioned: bool,
    sort_caption: bool,
    /// ...with the title alone (the 80 px Under posters band, no details line).
    title_only: bool,
    /// ...with the details line alone (the stored default: title off, Year · Length).
    details_only: bool,
    /// How many times the section asked for a card's element ([`CardSource::elem`]): what a hash of
    /// a card's identity costs the section, as a test reads it.
    elem_calls: std::cell::Cell<usize>,
}

/// The census tag [`Cards::overlay`] notes after each tile it is handed.
const TILE_PAINTED: u64 = 9999;

impl Cards {
    fn new(elems: Vec<u32>, more: bool) -> Self {
        Self { elems, more, epoch: 0, before: false, drawn: Default::default(), painted: Default::default(), focused: Default::default(), unindexed_from: usize::MAX, unloaded: 0..0, first: 0, captioned: false, sort_caption: false, title_only: false, details_only: false, elem_calls: Default::default() }
    }
}

impl CardSource<FixtureHost> for Cards {
    fn len(&self) -> usize {
        self.elems.len()
    }
    fn global(&self, i: usize) -> usize {
        self.first + i
    }
    fn elem(&self, i: usize) -> u32 {
        self.elem_calls.set(self.elem_calls.get() + 1);
        if i >= self.unindexed_from { 0 } else { self.elems[i] }
    }
    fn index_of(&self, e: &u32) -> Option<usize> {
        self.elems.iter().take(self.unindexed_from).position(|x| x == e)
    }
    fn art(&self, _i: usize) -> Art<'_> {
        Art::Poster(None)
    }
    fn label(&self, i: usize) -> TileLabel {
        let mut label = if self.captioned {
            TileLabel::from_lines([
                (!self.details_only).then(|| format!("Title {}", self.elems[i])),
                (!self.title_only).then(|| "2005 \u{b7} 1h 40m".into()),
            ])
        } else {
            TileLabel::default()
        };
        if self.sort_caption { label.value = Some(crate::card_row::SortValue { text: "100%".into(), ..Default::default() }); }
        label
    }
    fn more(&self) -> bool {
        self.more
    }
    fn page_epoch(&self) -> u32 {
        self.epoch
    }
    fn loaded(&self, i: usize) -> bool {
        !self.unloaded.contains(&i)
    }
    fn more_before(&self) -> bool {
        self.before
    }
    fn overlay(&self, p: Painter, i: usize, tile: &super::Tile, _measure: &dyn plx_machine::machine::Measure) {
        // a tile is on screen: a marker in the draw census, so a test reads the order of paint
        crate::draw_census::note(TILE_PAINTED, p.to_screen(tile.rect).1);
        self.drawn.borrow_mut().push((self.elems[i], p.to_screen(tile.rect).1));
        self.painted.borrow_mut().push((self.elems[i], p.to_screen(tile.rect).1, p.opacity()));
        self.focused.borrow_mut().push((self.elems[i], tile.focused));
    }
}

/// The two components behind one test surface; `at` is the shelf's frame, ignored by the grid.
trait Section {
    fn new() -> Self;
    fn on(&mut self, ev: &ScreenEvent<FixtureHost>, cx: &Cx9<'_>, src: &Cards, fx: &mut Effects<'_, FixtureHost>)
        -> Option<CardEvent<u32>>;
    fn place(&self, cx: &Cx9<'_>, src: &Cards, e: u32, how: At) -> Option<Placed>;
    fn scale_of(&self, cx: &Cx9<'_>, src: &Cards, e: u32) -> Option<f32>;
    fn stops(&self, f: &mut DrawFrame<'_, '_, FixtureHost>, src: &Cards);
    fn draw(&self, f: &mut DrawFrame<'_, '_, FixtureHost>, p: Painter, src: &Cards);
    fn redraw(&self, f: &mut DrawFrame<'_, '_, FixtureHost>, src: &Cards, focus: Option<FocusKey<u32>>);
    fn neighbour(&self, src: &Cards, key: FocusKey<u32>, dir: Dir) -> Step<u32>;
    fn focus_scale() -> f32;
    fn columns() -> Option<usize>;
    fn scroll(&self) -> f32;
    fn landed(&self) -> Option<super::Landed>;
    fn restore_scroll(&mut self, scroll: f32, n: usize);
    fn write(&self, c: &mut Canon);
}

impl Section for Shelf {
    fn new() -> Self {
        Shelf::new(ENTRY, &RowStyle::HOME)
    }
    fn on(&mut self, ev: &ScreenEvent<FixtureHost>, cx: &Cx9<'_>, src: &Cards, fx: &mut Effects<'_, FixtureHost>)
        -> Option<CardEvent<u32>> {
        Shelf::on(self, ev, cx, src, fx)
    }
    fn place(&self, cx: &Cx9<'_>, src: &Cards, e: u32, how: At) -> Option<Placed> {
        Shelf::place(self, cx, src, &e, SHELF_AT, how)
    }
    fn scale_of(&self, cx: &Cx9<'_>, src: &Cards, e: u32) -> Option<f32> {
        Shelf::scale_of(self, cx, src, &e)
    }
    fn stops(&self, f: &mut DrawFrame<'_, '_, FixtureHost>, src: &Cards) {
        Shelf::record_stops(self, f, f.painter, src, SHELF_AT)
    }
    fn draw(&self, f: &mut DrawFrame<'_, '_, FixtureHost>, p: Painter, src: &Cards) {
        Shelf::draw(self, f, p, src, SHELF_AT)
    }
    fn redraw(&self, f: &mut DrawFrame<'_, '_, FixtureHost>, src: &Cards, focus: Option<FocusKey<u32>>) {
        Shelf::redraw_focused(self, f, f.painter, src, SHELF_AT, focus)
    }
    fn neighbour(&self, src: &Cards, key: FocusKey<u32>, dir: Dir) -> Step<u32> {
        Shelf::neighbour(self, src, key, dir)
    }
    fn focus_scale() -> f32 {
        RowStyle::HOME.focus_scale
    }
    fn columns() -> Option<usize> {
        None
    }
    fn scroll(&self) -> f32 {
        Shelf::scroll(self)
    }
    fn landed(&self) -> Option<super::Landed> {
        Shelf::landed(self)
    }
    fn restore_scroll(&mut self, scroll: f32, n: usize) {
        Shelf::restore_scroll(self, scroll, n)
    }
    fn write(&self, c: &mut Canon) {
        Shelf::write(self, c)
    }
}

impl Section for Grid {
    fn new() -> Self {
        Grid::new(ENTRY, GRID)
    }
    fn on(&mut self, ev: &ScreenEvent<FixtureHost>, cx: &Cx9<'_>, src: &Cards, fx: &mut Effects<'_, FixtureHost>)
        -> Option<CardEvent<u32>> {
        Grid::on(self, ev, cx, src, fx)
    }
    fn place(&self, cx: &Cx9<'_>, src: &Cards, e: u32, how: At) -> Option<Placed> {
        Grid::place(self, cx, src, &e, how)
    }
    fn scale_of(&self, cx: &Cx9<'_>, src: &Cards, e: u32) -> Option<f32> {
        Grid::scale_of(self, cx, src, &e)
    }
    fn stops(&self, f: &mut DrawFrame<'_, '_, FixtureHost>, src: &Cards) {
        Grid::record_stops(self, f, f.painter, src)
    }
    fn draw(&self, f: &mut DrawFrame<'_, '_, FixtureHost>, p: Painter, src: &Cards) {
        Grid::draw(self, f, p, src)
    }
    fn redraw(&self, f: &mut DrawFrame<'_, '_, FixtureHost>, src: &Cards, focus: Option<FocusKey<u32>>) {
        Grid::redraw_focused(self, f, f.painter, src, focus)
    }
    fn neighbour(&self, src: &Cards, key: FocusKey<u32>, dir: Dir) -> Step<u32> {
        Grid::neighbour(self, src, key, dir)
    }
    fn focus_scale() -> f32 {
        poster_grid::STYLE.focus_scale
    }
    fn columns() -> Option<usize> {
        Some(poster_grid::COLS)
    }
    fn scroll(&self) -> f32 {
        Grid::scroll(self)
    }
    fn landed(&self) -> Option<super::Landed> {
        Grid::landed(self)
    }
    fn restore_scroll(&mut self, scroll: f32, _n: usize) {
        Grid::restore_scroll(self, scroll)
    }
    fn write(&self, c: &mut Canon) {
        Grid::write(self, c)
    }
}

/// A [`Grid`] in [`ScrollMode::External`] with [`EXT`]'s geometry, plus the owner's half of the
/// contract: it hands the grid its page before each event and, after a tick, applies the landing
/// shift and then takes the scroll the grid wants.
struct ExtGrid {
    grid: Grid,
    page: f32,
}

impl Section for ExtGrid {
    fn new() -> Self {
        Self { grid: Grid::new(ENTRY, EXT), page: 0.0 }
    }
    fn on(&mut self, ev: &ScreenEvent<FixtureHost>, cx: &Cx9<'_>, src: &Cards, fx: &mut Effects<'_, FixtureHost>)
        -> Option<CardEvent<u32>> {
        self.grid.set_page(520.0, self.page);
        let out = self.grid.on(ev, cx, src, fx);
        if matches!(ev, ScreenEvent::Tick(_)) {
            self.page += self.grid.landed_shift();
            if let Some(want) = self.grid.reveal_target() {
                self.page = want;
            }
            self.grid.set_page(520.0, self.page);
        }
        out
    }
    fn place(&self, cx: &Cx9<'_>, src: &Cards, e: u32, how: At) -> Option<Placed> {
        self.grid.place(cx, src, &e, how)
    }
    fn scale_of(&self, cx: &Cx9<'_>, src: &Cards, e: u32) -> Option<f32> {
        self.grid.scale_of(cx, src, &e)
    }
    fn stops(&self, f: &mut DrawFrame<'_, '_, FixtureHost>, src: &Cards) {
        self.grid.record_stops(f, f.painter, src)
    }
    fn draw(&self, f: &mut DrawFrame<'_, '_, FixtureHost>, p: Painter, src: &Cards) {
        self.grid.draw(f, p, src)
    }
    fn redraw(&self, f: &mut DrawFrame<'_, '_, FixtureHost>, src: &Cards, focus: Option<FocusKey<u32>>) {
        self.grid.redraw_focused(f, f.painter, src, focus)
    }
    fn neighbour(&self, src: &Cards, key: FocusKey<u32>, dir: Dir) -> Step<u32> {
        self.grid.neighbour(src, key, dir)
    }
    fn focus_scale() -> f32 {
        EXT_STYLE.focus_scale
    }
    fn columns() -> Option<usize> {
        Some(EXT.cols)
    }
    fn scroll(&self) -> f32 {
        self.page
    }
    fn landed(&self) -> Option<super::Landed> {
        self.grid.landed()
    }
    fn restore_scroll(&mut self, scroll: f32, _n: usize) {
        self.page = scroll;
        self.grid.set_page(520.0, scroll);
    }
    fn write(&self, c: &mut Canon) {
        self.grid.write(c)
    }
}

struct Rig<S: Section> {
    sect: S,
    src: Cards,
    view: FixtureView,
    focus: Option<FocusKey<u32>>,
    press: f32,
    /// The card the press dip belongs to when that is not the focused one (an abandoned press).
    pressed: Option<FocusKey<u32>>,
    ms: u32,
}

impl<S: Section> Rig<S> {
    fn new(n: usize) -> Self {
        Self {
            sect: S::new(),
            src: Cards::new((0..n as u32).map(|i| 100 + i).collect(), false),
            view: FixtureView::default(),
            focus: None,
            press: 1.0,
            pressed: None,
            ms: 0,
        }
    }

    fn cx(&self) -> Cx9<'_> {
        self.cx_with(self.press)
    }

    fn cx_with(&self, press: f32) -> Cx9<'_> {
        cx9_pressed(&self.view, self.ms, press, self.focus, self.pressed.or(self.focus))
    }

    /// Step one event through the section: the event it reported and whether the step moved.
    fn feed(&mut self, ev: ScreenEvent<FixtureHost>) -> (Option<CardEvent<u32>>, bool) {
        let mut present = Present::new();
        let mut out = Vec::new();
        let mut reported = None;
        let (_, moving) = plx_machine::idle::scoped_motion(|| {
            let cx = cx9_pressed(&self.view, self.ms, self.press, self.focus, self.pressed.or(self.focus));
            let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(9)), &mut present);
            reported = self.sect.on(&ev, &cx, &self.src, &mut fx);
        });
        (reported, moving || present.page_moving())
    }

    fn key(&self, elem: u32) -> FocusKey<u32> {
        FocusKey { entry: ENTRY, elem }
    }

    fn land_focus(&mut self, elem: u32, by: By) {
        let from = self.focus;
        self.focus = Some(self.key(elem));
        self.feed(ScreenEvent::FocusMoved { from, to: self.key(elem), by });
    }

    fn run(&mut self, frames: u32) -> bool {
        let mut moved = false;
        for _ in 0..frames {
            self.ms += MS;
            moved |= self.feed(ScreenEvent::Tick(Tick { ms: self.ms, dt_us: 16_667 })).1;
        }
        moved
    }

    /// The stops the draw registers at `press` (`record_stops`, which `draw` ends with; painting
    /// itself needs the GL context a host test does not have).
    fn stops(&self, press: f32) -> Vec<crate::screen::Stop<u32>> {
        let cx = self.cx_with(press);
        let mut f = DrawFrame::new(&cx, Painter::root());
        self.sect.stops(&mut f, &self.src);
        f.stops().to_vec()
    }

    /// Run `draw` for real, painting through a recording painter that carries a page scroll of
    /// `dy` (the screen rect each card was painted at, as the overlay hook saw it), and the stops
    /// `record_stops` registers through a painting one with the same scroll (a recording painter
    /// refuses stops, so the two cannot come out of one pass).
    fn drawn_and_stops(&self, press: f32, dy: f32) -> (Vec<(u32, Rect)>, Vec<crate::screen::Stop<u32>>) {
        let cx = self.cx_with(press);
        let mut f = DrawFrame::new(&cx, Painter::recording().translate(0.0, dy));
        self.src.drawn.borrow_mut().clear();
        let p = f.painter;
        self.sect.draw(&mut f, p, &self.src);
        let drawn = self.src.drawn.take();
        let mut f = DrawFrame::new(&cx, Painter::root().translate(0.0, dy));
        self.sect.stops(&mut f, &self.src);
        (drawn, f.stops().to_vec())
    }

    fn reconcile_after_removal(&self, at: usize) -> u32 {
        self.src.elems[at.min(self.src.elems.len() - 1)]
    }
}

impl<S: Section + 'static> CardHarness for Rig<S> {
    fn cards(&self) -> Vec<u32> {
        self.src.elems.clone()
    }
    fn focused(&self) -> Option<u32> {
        self.focus.map(|k| k.elem)
    }
    fn focus(&mut self, elem: u32, by: By) {
        self.land_focus(elem, by);
    }
    fn tick(&mut self, frames: u32) -> bool {
        self.run(frames)
    }
    fn cover(&mut self) {
        self.feed(ScreenEvent::Cover);
    }
    fn uncover(&mut self) {
        self.feed(ScreenEvent::Uncover);
    }
    fn set_press(&mut self, scale: f32) {
        self.press = scale;
    }
    fn neighbour(&self, elem: u32, dir: Dir) -> Nb {
        match self.sect.neighbour(&self.src, self.key(elem), dir) {
            Step::Move(k) => Nb::To(k.elem),
            Step::Edge => Nb::Edge,
        }
    }
    fn place(&self, elem: u32, at: At) -> Option<Placed> {
        self.sect.place(&self.cx(), &self.src, elem, at)
    }
    /// The rect the draw REALLY paints the card at (the overlay hook's), not the stop's.
    fn drawn_rect(&self, elem: u32, press: f32) -> Option<Rect> {
        self.drawn_and_stops(press, 0.0).0.into_iter().find(|&(e, _)| e == elem).map(|(_, r)| r)
    }
    fn scale(&self, elem: u32) -> Option<f32> {
        self.sect.scale_of(&self.cx(), &self.src, elem)
    }
    fn focus_scale(&self) -> f32 {
        S::focus_scale()
    }
    fn canon(&self) -> u64 {
        let mut c = Canon::new();
        self.sect.write(&mut c);
        c.finish()
    }
    fn identity(&self, elem: u32) -> String {
        format!("item{elem}")
    }
    fn scroll(&self) -> Option<f32> {
        Some(self.sect.scroll())
    }
    fn columns(&self) -> Option<usize> {
        S::columns()
    }
    fn landing(&mut self, l: Landing) -> Result<(), &'static str> {
        let focused = self.focus.ok_or("nothing focused")?.elem;
        let at = self.src.index_of(&focused).ok_or("focus is not in the source")?;
        match l {
            Landing::Reorder => self.src.elems.swap(0, 2),
            Landing::InsertAbove => self.src.elems.insert(0, 900),
            Landing::RemoveFocused => {
                self.src.elems.remove(at);
            }
        }
        // what the engine does on a landing: reconcile the focused element
        if self.src.index_of(&focused).is_none() {
            let now = self.reconcile_after_removal(at);
            self.land_focus(now, By::Reconcile);
        }
        Ok(())
    }
    fn memory_roundtrip(&mut self) -> Result<Box<dyn CardHarness>, &'static str> {
        let want = self.focus.ok_or("nothing focused")?.elem;
        let mut fresh = Rig::<S>::new(self.src.elems.len());
        fresh.sect.restore_scroll(self.sect.scroll(), fresh.src.elems.len());
        fresh.land_focus(want, By::Restore);
        Ok(Box::new(fresh))
    }
}

fn mount_shelf(n: usize) -> Box<dyn CardHarness> {
    Box::new(Rig::<Shelf>::new(n))
}
fn mount_grid(n: usize) -> Box<dyn CardHarness> {
    Box::new(Rig::<Grid>::new(n))
}
fn mount_external_grid(n: usize) -> Box<dyn CardHarness> {
    Box::new(Rig::<ExtGrid>::new(n))
}

#[test]
fn the_eight_conformance_cases_pass_on_shelf_and_grid() {
    let table: [(&'static str, Mount); 3] =
        [("shelf", mount_shelf), ("grid", mount_grid), ("external", mount_external_grid)];
    let matrix = conformance::run_all(&table);
    for (s, c, o) in &matrix {
        eprintln!("{s:6} {c:18} {o:?}");
    }
    assert_eq!(matrix.len(), 24);
    let bad: Vec<_> = matrix.iter().filter(|(_, _, o)| *o != Outcome::Pass).collect();
    assert!(bad.is_empty(), "{bad:#?}");
}

// ---- the pop rule ------------------------------------------------------------------------

fn settled<S: Section>(n: usize) -> Rig<S> {
    let mut r = Rig::<S>::new(n);
    r.run(2);
    r
}

/// An engine focus no `FocusMoved` announced draws at FULL scale before any tick has seen it (the
/// frame a restore or a seat lands on), an unfocused tile at rest.
fn unannounced_focus_is_drawn_whole<S: Section>() {
    let mut r = settled::<S>(8);
    let full = S::focus_scale();
    r.focus = Some(r.key(100));
    assert_eq!(r.sect.scale_of(&r.cx(), &r.src, 100), Some(full), "adopted whole, before the tick");
    assert_eq!(r.sect.scale_of(&r.cx(), &r.src, 101), Some(1.0), "an unfocused tile is at rest");
    r.run(1);
    assert_eq!(r.sect.scale_of(&r.cx(), &r.src, 100), Some(full), "…and the tick keeps it");
    // the pop of the seat the engine moves with no deliberate key, by every non-deliberate cause
    for by in [By::Restore, By::Reconcile] {
        r.land_focus(103, by);
        r.run(1);
        assert_eq!(r.sect.scale_of(&r.cx(), &r.src, 103), Some(full), "{by:?} arrives whole");
    }
}
#[test]
fn shelf_draws_an_unannounced_focus_whole() {
    unannounced_focus_is_drawn_whole::<Shelf>();
}
#[test]
fn grid_draws_an_unannounced_focus_whole() {
    unannounced_focus_is_drawn_whole::<Grid>();
}

/// A deliberate move grows from rest over frames, the tile it left lets go over frames, both settle.
fn a_move_grows_from_rest<S: Section>() {
    let mut r = settled::<S>(8);
    let full = S::focus_scale();
    r.land_focus(100, By::Restore);
    r.run(200);
    for by in [By::Dir, By::Pointer] {
        let (from, to) = if by == By::Dir { (100, 101) } else { (101, 100) };
        r.land_focus(to, by);
        // before the tick the new tile reads rest, not full: the move armed it
        assert!(r.sect.scale_of(&r.cx(), &r.src, to).unwrap() < full - 0.02, "{by:?} starts from rest");
        r.run(1);
        let (new, old) = (r.sect.scale_of(&r.cx(), &r.src, to).unwrap(), r.sect.scale_of(&r.cx(), &r.src, from).unwrap());
        assert!(new > 1.0 && new < full, "{by:?}: new {new}");
        assert!(old > 1.0 && old < full, "{by:?}: old {old}");
        r.run(200);
        assert!((r.sect.scale_of(&r.cx(), &r.src, to).unwrap() - full).abs() < 0.002);
        assert!((r.sect.scale_of(&r.cx(), &r.src, from).unwrap() - 1.0).abs() < 0.002);
    }
}
#[test]
fn shelf_grows_from_rest_on_a_move() {
    a_move_grows_from_rest::<Shelf>();
}
#[test]
fn grid_grows_from_rest_on_a_move() {
    a_move_grows_from_rest::<Grid>();
}

/// How many tiles read lifted (above rest) right now.
fn lifted<S: Section>(r: &Rig<S>) -> usize {
    r.src.elems.iter().filter(|&&e| r.sect.scale_of(&r.cx(), &r.src, e).unwrap() > 1.01).count()
}

/// A content landing that moves the focused element to another index (no `FocusMoved`: it is the
/// SAME element) carries its pop with it: one lifted tile in every frame, nothing lets go, and the
/// tile stays where it was on screen. `insert` cards land before it (a whole row for the grid, so
/// its column holds).
fn a_landing_carries_the_pop_with_the_focused_elem<S: Section>(insert: usize) {
    let mut r = settled::<S>(40);
    r.land_focus(112, By::Restore);
    r.run(240);
    let full = S::focus_scale();
    let before = r.sect.place(&r.cx(), &r.src, 112, At::Drawn).unwrap().rect;
    for k in 0..insert {
        r.src.elems.insert(0, 900 + k as u32);
    }
    assert_eq!(lifted(&r), 1, "before the tick the landing reads ONE lifted tile");
    for frame in 0..2 {
        r.run(1);
        let want = (frame == 0).then_some(super::Landed { from: 12, to: 12 + insert });
        assert_eq!(r.sect.landed(), want, "the tick that carried the pop reports it, the next one does not");
        for &e in r.src.elems.iter().filter(|&&e| e != 112) {
            let s = r.sect.scale_of(&r.cx(), &r.src, e).unwrap();
            assert!((s - 1.0).abs() < 0.0005, "frame {frame}: elem {e} is at {s}, not at rest");
        }
        let s = r.sect.scale_of(&r.cx(), &r.src, 112).unwrap();
        assert!((s - full).abs() < 0.002, "frame {frame}: the focused elem is at {s}, not {full}");
        let now = r.sect.place(&r.cx(), &r.src, 112, At::Drawn).unwrap().rect;
        assert!((now.x - before.x).abs() < 0.5 && (now.y - before.y).abs() < 0.5 && (now.w - before.w).abs() < 0.5,
            "frame {frame}: the focused tile jumped from {before:?} to {now:?}");
    }
    r.run(240);
    let now = r.sect.place(&r.cx(), &r.src, 112, At::Drawn).unwrap().rect;
    assert!((now.x - before.x).abs() < 0.5 && (now.y - before.y).abs() < 0.5, "settled at {now:?}, was {before:?}");
}
#[test]
fn shelf_landing_carries_the_pop_and_the_scroll() {
    a_landing_carries_the_pop_with_the_focused_elem::<Shelf>(1);
}
#[test]
fn grid_landing_carries_the_pop_and_the_scroll() {
    a_landing_carries_the_pop_with_the_focused_elem::<Grid>(poster_grid::COLS);
}

/// A paged row's window slides (its first `SLIDE` cards go, as many arrive at the end) between the
/// same two ticks as a key moves focus one card on, in either order. The cards that stay must
/// stay where they were on screen: the scroll is rebased with them in that tick, and the focused
/// card is one step from the one focus left, not a window away.
fn a_slide_under_a_moving_focus_keeps_the_row_in_place(move_first: bool) {
    const SLIDE: usize = 12;
    let mut r = settled::<Shelf>(24);
    r.land_focus(100 + 20, By::Restore);
    r.run(240);
    r.land_focus(100 + 21, By::Dir);
    r.run(3);
    let x = |r: &Rig<Shelf>, elem: u32| Section::place(&r.sect, &r.cx(), &r.src, elem, At::SpringTarget).unwrap().rect.cx();
    let pitch = RowStyle::HOME.w + RowStyle::HOME.gap;
    let (stayed, focused) = (100 + 19, 100 + 22);
    let before = x(&r, stayed);
    let slide = |r: &mut Rig<Shelf>| {
        r.src.elems.drain(..SLIDE);
        r.src.elems.extend(900..900 + SLIDE as u32);
    };
    if move_first {
        r.land_focus(focused, By::Dir);
        slide(&mut r);
    } else {
        slide(&mut r);
        r.land_focus(focused, By::Dir);
    }
    r.run(1);
    // one tick of a row already following a key: a fraction of a card, never a window
    let moved = x(&r, stayed) - before;
    assert!(moved.abs() < pitch / 2.0, "a card that stayed in the window jumped {moved} px in the landing tick");
    assert!((x(&r, focused) - x(&r, stayed) - 3.0 * pitch).abs() < 0.5, "the focused card is three cards on from it");
    // riding the trailing edge as any step of the walk does, the scroll one key behind it
    assert!(x(&r, focused) > crate::consts::SCR_W / 2.0 && x(&r, focused) < crate::consts::SCR_W + pitch,
        "and still at the trailing edge: {}", x(&r, focused));
    assert_eq!(lifted(&r), 2, "the card focus left lets go while the new one grows");
    assert!(Section::scale_of(&r.sect, &r.cx(), &r.src, 100 + 21).unwrap() > 1.01, "the let-go followed its card");
    r.run(240);
    assert_eq!(lifted(&r), 1);
}
#[test]
fn shelf_slide_after_a_move_in_one_tick_keeps_the_row_in_place() {
    a_slide_under_a_moving_focus_keeps_the_row_in_place(true);
}
#[test]
fn shelf_slide_before_a_move_in_one_tick_keeps_the_row_in_place() {
    a_slide_under_a_moving_focus_keeps_the_row_in_place(false);
}

/// The requirement under every landing rule: focus never moves under the user, whatever the order
/// of the events and however far the cards moved. `insert` cards arrive before the focused one
/// (a whole number of rows for a grid) between the same two ticks as the engine hands focus on, by
/// every cause that can: a key, the pointer, a restore, a reconcile naming another card, a
/// reconcile naming the same card again. A card that stayed must stay where it is drawn, and the
/// focused one must end on the screen. `far` is more than any search distance a section once had.
fn a_shift_under_a_focus_change_keeps_the_view_in_place<S: Section>(n: usize, insert_rows: usize) {
    let cols = S::columns().unwrap_or(1);
    let at = if cols > 1 { 2 * cols + 1 } else { 20 };
    let insert = insert_rows * cols;
    let causes = [(By::Dir, false), (By::Pointer, false), (By::Restore, false), (By::Reconcile, false), (By::Reconcile, true)];
    for (by, same) in causes {
        for move_first in [true, false] {
            let mut r = settled::<S>(n);
            r.land_focus(100 + at as u32, By::Restore);
            r.run(240);
            let stayed = 100 + at as u32 - 1;
            let target = if same { 100 + at as u32 } else { 100 + at as u32 + 1 };
            let before = r.sect.place(&r.cx(), &r.src, stayed, At::Drawn).unwrap().rect;
            let slide = |r: &mut Rig<S>| {
                for k in 0..insert {
                    r.src.elems.insert(0, 900 + k as u32);
                }
            };
            if move_first {
                r.land_focus(target, by);
                slide(&mut r);
            } else {
                slide(&mut r);
                r.land_focus(target, by);
            }
            let why = format!("{by:?} same={same} move_first={move_first} insert={insert}");
            r.run(1);
            // an owner that scrolls a page around the section (the external grid) hears of it
            assert_eq!(r.sect.landed(), Some(super::Landed { from: at, to: at + insert }), "{why}: the landing is reported");
            let now = r.sect.place(&r.cx(), &r.src, stayed, At::Drawn).unwrap().rect;
            assert!((now.x - before.x).abs() < 100.0 && (now.y - before.y).abs() < 100.0,
                "{why}: a card that stayed jumped from {before:?} to {now:?} in the landing tick");
            // the pop followed the cards too: a key lets the card it left go while the new one
            // grows; a restore or reconcile adopts its card whole and nothing else is lifted
            let scale = |r: &Rig<S>, e: u32| r.sect.scale_of(&r.cx(), &r.src, e).unwrap();
            // (cells past the shelf's spring array share one spring: no let-go to follow there)
            let own_cells = cols > 1 || at + insert + 1 < crate::card_row::MAX_ROW_ITEMS;
            if matches!(by, By::Dir | By::Pointer) && own_cells {
                assert!(scale(&r, 100 + at as u32) > 1.01, "{why}: the let-go followed its card");
                assert!(scale(&r, target) > 1.0 && scale(&r, target) < S::focus_scale() - 0.005, "{why}: the new card grows from rest");
                assert_eq!(r.src.elems.iter().filter(|&&e| scale(&r, e) > 1.0005).count(), 2, "{why}: only those two");
            } else if !matches!(by, By::Dir | By::Pointer) {
                assert_eq!(lifted(&r), 1, "{why}: one card is lifted");
                assert!((scale(&r, target) - S::focus_scale()).abs() < 0.002, "{why}: the adopted card is whole");
            }
            r.run(240);
            let on = r.sect.place(&r.cx(), &r.src, target, At::Drawn).unwrap().rect;
            assert!(on.cx() > 0.0 && on.cx() < crate::consts::SCR_W && on.cy() > 0.0 && on.cy() < crate::consts::SCR_H,
                "{why}: the focused card ended off screen at {on:?}");
            assert_eq!(lifted(&r), 1, "{why}: one card is lifted once everything settles");
        }
    }
}
#[test]
fn shelf_keeps_its_view_when_cards_arrive_under_any_focus_change() {
    a_shift_under_a_focus_change_keeps_the_view_in_place::<Shelf>(40, 12);
}
#[test]
fn shelf_carries_its_let_go_when_cards_arrive_under_a_key() {
    // two cards in: the focused pair stays inside the shelf's spring array
    a_shift_under_a_focus_change_keeps_the_view_in_place::<Shelf>(40, 2);
}
#[test]
fn shelf_keeps_its_view_when_cards_arrive_far_under_any_focus_change() {
    // 150 cards before it: farther than the search distance the first fix gave up at
    a_shift_under_a_focus_change_keeps_the_view_in_place::<Shelf>(400, 150);
}
#[test]
fn grid_keeps_its_view_when_cards_arrive_under_any_focus_change() {
    a_shift_under_a_focus_change_keeps_the_view_in_place::<Grid>(120, 3);
    a_shift_under_a_focus_change_keeps_the_view_in_place::<Grid>(900, 30);
}
#[test]
fn external_grid_keeps_its_view_when_cards_arrive_under_any_focus_change() {
    a_shift_under_a_focus_change_keeps_the_view_in_place::<ExtGrid>(120, 3);
    a_shift_under_a_focus_change_keeps_the_view_in_place::<ExtGrid>(900, 30);
}

/// The farthest a `n`-card HOME shelf can scroll.
fn home_max_scroll(n: usize) -> f32 {
    let sty = &RowStyle::HOME;
    (n as f32 * (sty.w + sty.gap) - sty.gap - (crate::consts::SCR_W - 2.0 * sty.margin_x)).max(0.0)
}

/// Move `elem` to index `to` of the source (a landing that reorders).
fn put_at(r: &mut Rig<Shelf>, elem: u32, to: usize) {
    r.src.elems.retain(|&e| e != elem);
    r.src.elems.insert(to, elem);
}

/// A landing that moves the focused card to the head of a row sitting at scroll 0 would shift the
/// scroll to -2 pitches (a blank band on the left, then a glide back): the scroll never leaves
/// `[0, max]` on any frame.
#[test]
fn shelf_landing_to_the_head_never_overscrolls() {
    let mut r = settled::<Shelf>(40);
    r.land_focus(102, By::Restore);
    r.run(240);
    assert_eq!(r.sect.scroll(), 0.0, "card 2 is on screen, the row did not scroll");
    put_at(&mut r, 102, 0);
    let max = home_max_scroll(40);
    for frame in 0..240 {
        r.run(1);
        let sx = r.sect.scroll();
        assert!((0.0..=max).contains(&sx), "frame {frame}: scroll {sx} left [0, {max}]");
    }
}

/// An insert above the focus on a row that fits entirely cannot be honoured at all: the scroll
/// stays exactly at 0 with no spring.
#[test]
fn shelf_landing_on_a_row_that_fits_stays_at_zero() {
    let mut r = settled::<Shelf>(3);
    r.land_focus(101, By::Restore);
    r.run(240);
    r.src.elems.insert(0, 900);
    for frame in 0..240 {
        r.run(1);
        assert_eq!(r.sect.scroll(), 0.0, "frame {frame}: a row that fits scrolled");
    }
}

/// Where the shift IS honourable (a landing mid-row) the tile keeps its screen x exactly, and the
/// scroll moves by the whole shift.
#[test]
fn shelf_landing_mid_row_keeps_the_tile_exactly() {
    let mut r = settled::<Shelf>(40);
    r.land_focus(112, By::Restore);
    r.run(240);
    let (before, sx) = (Section::place(&r.sect, &r.cx(), &r.src, 112, At::Drawn).unwrap().rect, Section::scroll(&r.sect));
    assert!(sx > 0.0 && sx < home_max_scroll(40) - 400.0, "a mid-row scroll, got {sx}");
    r.src.elems.insert(0, 900);
    for frame in 0..2 {
        r.run(1);
        let now = Section::place(&r.sect, &r.cx(), &r.src, 112, At::Drawn).unwrap().rect;
        assert!((now.x - before.x).abs() < 0.01, "frame {frame}: x {} -> {}", before.x, now.x);
    }
    assert!((r.sect.scroll() - sx - (RowStyle::HOME.w + RowStyle::HOME.gap)).abs() < 0.01);
}

/// A restore or reconcile arrival is adopted whole and the tile it leaves goes straight to rest —
/// it was not "let go" by anyone — with ONE lifted tile even on the frame before the tick.
fn a_non_deliberate_arrival_rests_the_old_tile<S: Section>() {
    let mut r = settled::<S>(8);
    r.land_focus(101, By::Restore);
    r.run(240);
    for by in [By::Reconcile, By::Restore] {
        let (from, to) = if by == By::Reconcile { (101, 104) } else { (104, 101) };
        r.land_focus(to, by);
        assert_eq!(lifted(&r), 1, "{by:?}: one lifted tile before the tick");
        r.run(1);
        assert!((r.sect.scale_of(&r.cx(), &r.src, from).unwrap() - 1.0).abs() < 0.0005, "{by:?}: the old tile lets go");
        assert!((r.sect.scale_of(&r.cx(), &r.src, to).unwrap() - S::focus_scale()).abs() < 0.002);
    }
}
#[test]
fn shelf_rests_the_old_tile_on_a_non_deliberate_arrival() {
    a_non_deliberate_arrival_rests_the_old_tile::<Shelf>();
}
#[test]
fn grid_rests_the_old_tile_on_a_non_deliberate_arrival() {
    a_non_deliberate_arrival_rests_the_old_tile::<Grid>();
}

/// A focused element that LEAVES the source in a landing (it moved to another section) with no
/// `FocusMoved` away from it: nobody let it go, so the tile now at its index is not left lifted and
/// shrinking over frames; every tile is at rest on the frame of the tick. A deliberate move out
/// (a `FocusMoved` whose `from` is in this section) still lets the old tile go over frames.
#[test]
fn shelf_rests_a_focused_tile_whose_element_left_the_source() {
    let mut r = settled::<Shelf>(8);
    r.land_focus(101, By::Restore);
    r.run(240);
    r.src.elems.retain(|&e| e != 101);
    r.run(1);
    for &e in &r.src.elems {
        let s = r.sect.scale_of(&r.cx(), &r.src, &e).unwrap();
        assert!((s - 1.0).abs() < 0.0005, "elem {e} is at {s} after the focused element left, not at rest");
    }

    let mut r = settled::<Shelf>(8);
    r.land_focus(101, By::Restore);
    r.run(240);
    let away = FocusKey { entry: EntryId(77), elem: 5 };
    let from = r.focus;
    r.focus = Some(away);
    r.feed(ScreenEvent::FocusMoved { from, to: away, by: By::Dir });
    r.run(1);
    let s = r.sect.scale_of(&r.cx(), &r.src, &101).unwrap();
    assert!(s > 1.0005 && s < RowStyle::HOME.focus_scale - 0.0005, "a deliberate move out lets the tile go over frames, at {s}");
}

/// Focus in another entry (a menu's, another page's) is not this section's focus.
fn focus_in_another_entry_is_ignored<S: Section>() {
    let mut r = settled::<S>(8);
    r.focus = Some(FocusKey { entry: EntryId(77), elem: 100 });
    assert_eq!(r.sect.scale_of(&r.cx(), &r.src, 100), Some(1.0));
    let (ev, _) = r.feed(ScreenEvent::PressCommit(PressId(1)));
    assert_eq!(ev, None);
}
#[test]
fn shelf_ignores_another_entrys_focus() {
    focus_in_another_entry_is_ignored::<Shelf>();
}
#[test]
fn grid_ignores_another_entrys_focus() {
    focus_in_another_entry_is_ignored::<Grid>();
}

// ---- geometry and draw --------------------------------------------------------------------

/// The registered stop IS the placed rect at every press, and `rest_rect` the settled focus-scaled
/// rect; an unfocused tile's stop is its rest rect.
fn stops_equal_placement<S: Section>() {
    let mut r = settled::<S>(8);
    r.land_focus(101, By::Restore);
    r.run(200);
    let full = S::focus_scale();
    for press in [1.0, 0.96, 0.918] {
        let stops = r.stops(press);
        assert!(stops.len() >= 4, "stops were recorded");
        for s in &stops {
            let placed = r.sect.place(&r.cx_with(press), &r.src, s.key.elem, At::Drawn).unwrap();
            assert!((placed.rect.x - s.rect.x).abs() < 0.5 && (placed.rect.w - s.rect.w).abs() < 0.5
                && (placed.rect.y - s.rect.y).abs() < 0.5, "press {press} elem {}: {:?} vs {:?}", s.key.elem, placed.rect, s.rect);
            assert!((placed.rest_rect.w - s.rest_rect.w).abs() < 0.5);
        }
        let focused = stops.iter().find(|s| s.key.elem == 101).unwrap();
        let unfocused = stops.iter().find(|s| s.key.elem == 102).unwrap();
        assert!((focused.rect.w - focused.rest_rect.w / full * full * press).abs() < 0.5,
            "the focused stop is the settled rect dipped by the press");
        assert!((unfocused.rect.w - unfocused.rest_rect.w / full).abs() < 0.5, "an unfocused tile rests");
    }
}
#[test]
fn shelf_stops_equal_placement_at_every_press() {
    stops_equal_placement::<Shelf>();
}
#[test]
fn grid_stops_equal_placement_at_every_press() {
    stops_equal_placement::<Grid>();
}

/// What the draw really paints, the stop it registers and what `place` answers are one rect, at
/// every press and under a page scroll: `SectionFrame::y` / the grid's `top` are SCREEN space and
/// the painter's own translate is undone, so a scrolled page's painter and `place` agree.
fn drawn_stop_and_place_agree<S: Section>() {
    let mut r = settled::<S>(8);
    r.land_focus(101, By::Restore);
    r.run(200);
    for dy in [0.0f32, -200.0] {
        for press in [1.0, 0.96, 0.918] {
            let (drawn, stops) = r.drawn_and_stops(press, dy);
            assert!(drawn.len() >= 3 && stops.len() >= 3, "dy {dy}: drew {} registered {}", drawn.len(), stops.len());
            assert!(drawn.iter().any(|&(e, _)| e == 101), "the focused tile was painted");
            for (e, rect) in drawn {
                let stop = stops.iter().find(|s| s.key.elem == e).expect("a painted tile registers a stop").rect;
                let placed = r.sect.place(&r.cx_with(press), &r.src, e, At::Drawn).unwrap().rect;
                for (what, other) in [("stop", stop), ("place", placed)] {
                    assert!((rect.x - other.x).abs() < 0.5 && (rect.y - other.y).abs() < 0.5
                        && (rect.w - other.w).abs() < 0.5 && (rect.h - other.h).abs() < 0.5,
                        "dy {dy} press {press} elem {e}: painted {rect:?} vs {what} {other:?}");
                }
            }
        }
    }
}
#[test]
fn shelf_paints_the_rect_it_registers_and_places() {
    drawn_stop_and_place_agree::<Shelf>();
}
#[test]
fn grid_paints_the_rect_it_registers_and_places() {
    drawn_stop_and_place_agree::<Grid>();
}

/// A shelf paints and registers only tiles on the axis; a grid only the rows the scroll can show.
#[test]
fn off_axis_tiles_register_no_stops() {
    let mut shelf = settled::<Shelf>(60);
    let n = shelf.stops(1.0).len();
    assert!(n > 0 && n < 20, "the shelf registered {n} of 60 stops");
    shelf.land_focus(100, By::Restore);
    let mut grid = settled::<Grid>(600);
    grid.land_focus(100, By::Restore);
    let n = grid.stops(1.0).len();
    assert!(n > 0 && n < 60, "the grid registered {n} of 600 stops");
}

/// The opener redraw paints the focused tile alone and nothing for an element not in the source.
#[test]
fn the_opener_redraw_paints_only_a_known_element() {
    fn run<S: Section>() {
        let mut r = settled::<S>(8);
        r.land_focus(100, By::Restore);
        r.run(60);
        let cx = r.cx();
        let mut f = DrawFrame::new(&cx, Painter::recording());
        r.sect.redraw(&mut f, &r.src, Some(r.key(100)));
        r.sect.redraw(&mut f, &r.src, Some(FocusKey { entry: ENTRY, elem: 9999 }));
        r.sect.redraw(&mut f, &r.src, Some(FocusKey { entry: EntryId(1), elem: 100 }));
        r.sect.redraw(&mut f, &r.src, None);
    }
    run::<Shelf>();
    run::<Grid>();
}

/// **The element a surface lifts is OUT of the page pass and in the lift, in both sections** — the
/// doubled-title guard. The page pass (a frame carrying `lifted`) paints every card but the lifted
/// one and says so (`popover::lift_owns`, which silences the press spring's page damage); the opener
/// lift's own frame (no `lifted`) paints exactly that card. Between them the card is drawn ONCE.
#[test]
fn the_lifted_card_is_left_out_of_the_page_pass_and_drawn_by_the_lift() {
    fn run<S: Section>() {
        let mut r = settled::<S>(8);
        r.land_focus(101, By::Restore);
        r.run(200);
        let cx = r.cx();
        let ids = |drawn: Vec<(u32, Rect)>| drawn.into_iter().map(|(e, _)| e).collect::<Vec<_>>();

        crate::popover::set_lift_owns(false);
        let mut page = DrawFrame::new(&cx, Painter::recording());
        page.lifted = Some(r.key(101));
        let p = page.painter;
        r.src.drawn.borrow_mut().clear();
        r.sect.draw(&mut page, p, &r.src);
        let page_cards = ids(r.src.drawn.take());
        assert!(page_cards.len() >= 3 && !page_cards.contains(&101), "page pass left the lifted card out: {page_cards:?}");
        assert!(crate::popover::lift_owns(), "…and said so");

        let mut lift = DrawFrame::new(&cx, Painter::recording());
        r.sect.redraw(&mut lift, &r.src, Some(r.key(101)));
        assert_eq!(ids(r.src.drawn.take()), vec![101], "the lift is the card's only copy");

        // A frame with nothing lifted (every other moment) draws it as ever, and claims nothing.
        crate::popover::set_lift_owns(false);
        let mut plain = DrawFrame::new(&cx, Painter::recording());
        let p = plain.painter;
        r.sect.draw(&mut plain, p, &r.src);
        assert!(ids(r.src.drawn.take()).contains(&101));
        assert!(!crate::popover::lift_owns());
    }
    run::<Shelf>();
    run::<Grid>();
}

/// With focus out of the grid it scrolls home by default (Collection's header sits above it); a
/// spec that says otherwise leaves the scroll where it is for a page with other focus zones.
#[test]
fn a_grid_scrolls_home_when_unfocused_only_if_its_spec_says_so() {
    for (home, hold) in [(true, false), (false, true)] {
        let mut r = settled::<Grid>(60);
        r.sect = Grid::new(ENTRY, GridSpec { home_when_unfocused: home, ..GRID });
        r.land_focus(130, By::Restore);
        r.run(300);
        let at = r.sect.scroll();
        assert!(at > 100.0, "the focused row scrolled into view: {at}");
        r.focus = None;
        r.run(300);
        if hold {
            assert!((r.sect.scroll() - at).abs() < 0.5, "the scroll stayed at {at}, now {}", r.sect.scroll());
        } else {
            assert!(r.sect.scroll().abs() < 0.5, "the scroll went home, now {}", r.sect.scroll());
        }
    }
}

// ---- events ---------------------------------------------------------------------------------

fn press_events<S: Section>() {
    let mut r = settled::<S>(8);
    assert_eq!(r.feed(ScreenEvent::PressCommit(PressId(1))).0, None, "nothing focused, nothing to activate");
    r.land_focus(102, By::Restore);
    assert_eq!(r.feed(ScreenEvent::PressCommit(PressId(1))).0, Some(CardEvent::Activate(102)));
    assert_eq!(r.feed(ScreenEvent::PressHold(PressId(1))).0, Some(CardEvent::Hold(102)));
    // a landing between focus and press cannot name the wrong item: the elem is resolved at press time
    r.src.elems.insert(0, 900);
    assert_eq!(r.feed(ScreenEvent::PressCommit(PressId(2))).0, Some(CardEvent::Activate(102)));
}
#[test]
fn shelf_reports_activate_and_hold_by_elem() {
    press_events::<Shelf>();
}
#[test]
fn grid_reports_activate_and_hold_by_elem() {
    press_events::<Grid>();
}

#[test]
fn a_source_can_refuse_a_hold() {
    struct NoHold(Cards);
    impl CardSource<FixtureHost> for NoHold {
        fn len(&self) -> usize { self.0.len() }
        fn elem(&self, i: usize) -> u32 { self.0.elem(i) }
        fn index_of(&self, e: &u32) -> Option<usize> { self.0.index_of(e) }
        fn art(&self, i: usize) -> Art<'_> { self.0.art(i) }
        fn label(&self, i: usize) -> TileLabel { self.0.label(i) }
        fn holdable(&self, _i: usize) -> bool { false }
    }
    let r = settled::<Shelf>(4);
    let src = NoHold(Cards::new(r.src.elems.clone(), false));
    let mut shelf = Shelf::new(ENTRY, &RowStyle::HOME);
    let mut r2 = r;
    r2.focus = Some(r2.key(100));
    let mut present = Present::new();
    let mut out = Vec::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(9)), &mut present);
    assert_eq!(shelf.on(&ScreenEvent::PressHold(PressId(1)), &r2.cx(), &src, &mut fx), None);
}

/// Scrolling toward the tail asks the source for more, once per `(len, end)`; a source with nothing
/// more never hears it.
fn want_events<S: Section>() {
    let mut r = settled::<S>(30);
    r.src.more = true;
    r.land_focus(100, By::Restore);
    assert_eq!(r.feed(ScreenEvent::Tick(Tick { ms: 1000, dt_us: 16_667 })).0, None, "the head asks for nothing");
    r.land_focus(129, By::Dir);
    let first = r.feed(ScreenEvent::Tick(Tick { ms: 1016, dt_us: 16_667 })).0;
    let Some(CardEvent::Want(range)) = first else { panic!("expected Want at the tail, got {first:?}") };
    assert_eq!(range.start, 30);
    assert!(range.end > 30);
    assert_eq!(r.feed(ScreenEvent::Tick(Tick { ms: 1032, dt_us: 16_667 })).0, None, "asked once");
    r.src.elems.extend(200..230);
    let again = r.feed(ScreenEvent::Tick(Tick { ms: 1048, dt_us: 16_667 })).0;
    assert!(again.is_none() || matches!(again, Some(CardEvent::Want(_))), "a landing may ask again");
    let mut done = settled::<S>(30);
    done.land_focus(129, By::Restore);
    assert_eq!(done.feed(ScreenEvent::Tick(Tick { ms: 1000, dt_us: 16_667 })).0, None, "no more, no Want");
}
#[test]
fn shelf_wants_more_at_the_tail() {
    want_events::<Shelf>();
}
#[test]
fn grid_wants_more_at_the_tail() {
    want_events::<Grid>();
}

/// The `Want`s a grid reports over `frames` frames at 60 Hz, each with the second it came at.
fn grid_asks(r: &mut Rig<Grid>, frames: u32) -> Vec<(f32, std::ops::Range<usize>)> {
    let mut asks = Vec::new();
    for _ in 0..frames {
        r.ms += MS;
        if let (Some(CardEvent::Want(range)), _) = r.feed(ScreenEvent::Tick(Tick { ms: r.ms, dt_us: 16_667 })) {
            asks.push((r.ms as f32 / 1000.0, range));
        }
    }
    asks
}

fn gaps(asks: &[(f32, std::ops::Range<usize>)]) -> Vec<f32> {
    asks.windows(2).map(|w| w[1].0 - w[0].0).collect()
}

#[test]
fn a_grid_repeats_an_unanswered_tail_ask_on_the_capped_ladder() {
    let mut r = settled::<Grid>(30);
    r.src.more = true;
    r.land_focus(129, By::Dir);
    grid_asks(&mut r, 60 * 3); // the scroll settling moves the look-ahead, each a new ask
    r.src.epoch += 1; // a landing that grew nothing: the next ask is fresh
    let asks = grid_asks(&mut r, 60 * 150);
    assert!(asks.len() >= 8, "the ask keeps being repeated with no key: {asks:?}");
    let g = gaps(&asks);
    assert!(g.windows(2).take(5).all(|w| w[1] > w[0] * 1.8), "the interval climbs: {g:?}");
    assert!(g.iter().all(|g| *g < 30.2), "and never passes the 30 s cap: {g:?}");
    assert!((g[g.len() - 1] - 30.0).abs() < 0.1, "the cap repeats while the user stays: {g:?}");
    // a landing starts it over, at once
    r.src.elems.extend(200..230);
    r.land_focus(229, By::Dir);
    assert!(!grid_asks(&mut r, 2).is_empty(), "a longer listing is asked at once");
    // nothing more: it stops
    r.src.more = false;
    assert!(grid_asks(&mut r, 60 * 40).is_empty(), "no more cards, no ask");
}

#[test]
fn a_grid_repeats_the_ask_for_an_unloaded_card_in_view_until_it_loads() {
    let mut r = settled::<Grid>(300);
    r.src.unloaded = 6..12; // a page that failed or was evicted, inside the visible rows
    let asks = grid_asks(&mut r, 60 * 20);
    assert!(asks.len() >= 5, "the hole is asked for again and again: {asks:?}");
    assert!(asks.iter().all(|(_, a)| a.start == 6 && a.end > 6), "the ask names the hole: {asks:?}");
    assert!(gaps(&asks).iter().all(|g| *g < 30.2));
    r.src.unloaded = 0..0; // it landed
    assert!(grid_asks(&mut r, 60 * 40).is_empty(), "loaded, the asking stops");
    // a hole out of sight is nobody's business until it is scrolled to
    r.src.unloaded = 250..260;
    assert!(grid_asks(&mut r, 60 * 20).is_empty(), "off screen, nothing is asked");
}

#[test]
fn a_grid_asks_for_the_unloaded_card_a_failed_page_scrolled_back_into_view_left() {
    let mut r = settled::<Grid>(300);
    r.src.unloaded = 100..160;
    r.land_focus(200, By::Dir);
    let asks = grid_asks(&mut r, 60 * 3);
    assert!(!asks.is_empty() && asks.iter().all(|(_, a)| a.start >= 96), "focus on the hole asks for it: {asks:?}");
    r.land_focus(399, By::Dir);
    r.src.unloaded = 0..0;
    assert!(grid_asks(&mut r, 60 * 10).is_empty(), "elsewhere and loaded, quiet");
}

// ---- the page edges (Home's paging rule, lifted) ---------------------------------------------

/// One frame of an owner's loop for its row: the tick, then `page_ask` on what the tick asked for.
fn page_frame(r: &mut Rig<Shelf>, offset: usize, active: bool) -> Option<PageEdge> {
    r.ms += MS;
    let (want, _) = r.feed(ScreenEvent::Tick(Tick { ms: r.ms, dt_us: 16_667 }));
    let cx = cx9_pressed(&r.view, r.ms, r.press, r.focus, r.pressed.or(r.focus));
    r.sect.page_ask(&cx, &r.src, matches!(want, Some(CardEvent::Want(_))), offset, active)
}

#[test]
fn page_ask_asks_once_per_window_at_the_trailing_edge() {
    let mut r = settled::<Shelf>(30);
    r.src.more = true;
    r.land_focus(129, By::Dir);
    assert_eq!(page_frame(&mut r, 0, true), Some(PageEdge::After), "the first tick near the tail asks");
    assert_eq!(page_frame(&mut r, 0, true), None, "the same window is asked once");
    r.sect.reset_page_requests();
    assert_eq!(page_frame(&mut r, 0, true), Some(PageEdge::After), "a reset (focus left the row) asks again");
}

/// What the owner calls each frame before `page_ask`: the edge whose ask to withdraw.
fn cancel_frame(r: &mut Rig<Shelf>, offset: usize, active: bool) -> Option<PageEdge> {
    let cx = cx9_pressed(&r.view, r.ms, r.press, r.focus, r.pressed.or(r.focus));
    r.sect.page_cancel(&cx, &r.src, offset, active)
}

#[test]
fn page_cancel_withdraws_an_ask_once_focus_has_left_its_edge() {
    let mut r = settled::<Shelf>(30);
    r.src.more = true;
    assert_eq!(cancel_frame(&mut r, 0, true), None, "nothing was asked");
    r.land_focus(129, By::Dir);
    assert_eq!(page_frame(&mut r, 0, true), Some(PageEdge::After));
    r.land_focus(124, By::Dir);
    assert_eq!(cancel_frame(&mut r, 0, true), None, "still inside the zone: the ask stands");
    r.land_focus(123, By::Dir);
    assert_eq!(cancel_frame(&mut r, 0, true), Some(PageEdge::After), "the first card out of the zone withdraws it");
    assert_eq!(cancel_frame(&mut r, 0, true), None, "once");
    r.land_focus(129, By::Dir);
    assert_eq!(page_frame(&mut r, 0, true), Some(PageEdge::After), "and the edge asks again on return");
}

#[test]
fn page_cancel_withdraws_the_leading_ask_and_one_for_a_row_no_longer_shown() {
    let mut r = settled::<Shelf>(30);
    r.src.more = true;
    r.land_focus(102, By::Dir);
    assert_eq!(page_frame(&mut r, 24, true), Some(PageEdge::Before));
    r.land_focus(106, By::Dir);
    assert_eq!(cancel_frame(&mut r, 24, true), Some(PageEdge::Before));
    assert_eq!(page_frame(&mut r, 24, true), None);
    r.land_focus(102, By::Dir);
    assert_eq!(page_frame(&mut r, 24, true), Some(PageEdge::Before));
    assert_eq!(cancel_frame(&mut r, 24, false), Some(PageEdge::Before), "the owner stopped showing the row");
}

#[test]
fn page_ask_is_silent_in_the_middle_of_the_row() {
    let mut r = settled::<Shelf>(30);
    r.src.more = true;
    r.land_focus(110, By::Dir);
    for _ in 0..5 {
        assert_eq!(page_frame(&mut r, 24, true), None, "neither edge is near at card 10");
    }
}

#[test]
fn page_ask_asks_the_leading_edge_only_when_the_source_has_cards_before() {
    let mut r = settled::<Shelf>(30);
    r.src.more = true;
    r.land_focus(102, By::Dir);
    assert_eq!(page_frame(&mut r, 0, true), None, "nothing before the window: no leading ask");
    assert_eq!(page_frame(&mut r, 24, true), Some(PageEdge::Before), "cards before: ask once");
    assert_eq!(page_frame(&mut r, 24, true), None, "the same leading window is asked once");
    r.land_focus(112, By::Dir);
    assert_eq!(page_frame(&mut r, 24, true), None, "leaving the edge admits a new request");
    r.land_focus(102, By::Dir);
    assert_eq!(page_frame(&mut r, 24, true), Some(PageEdge::Before), "returning to the edge asks again");
}

#[test]
fn page_ask_asks_nothing_while_the_row_is_not_shown() {
    let mut r = settled::<Shelf>(30);
    r.src.more = true;
    r.land_focus(129, By::Dir);
    assert_eq!(page_frame(&mut r, 0, false), None, "an inactive row asks for no page");
}

#[test]
fn page_ask_yields_at_most_one_edge_a_frame() {
    // ten cards, focus on the sixth: near both edges. The leading ask wins, and the trailing one is
    // held while the row shows cards before it (Home's rule: `before` suppresses the forward ask).
    let mut r = settled::<Shelf>(10);
    r.src.more = true;
    r.land_focus(105, By::Dir);
    assert_eq!(page_frame(&mut r, 24, true), Some(PageEdge::Before));
    // one more card lands: the tail now wants a page too, and the leading latch still holds the frame
    r.src.elems.push(110);
    assert_eq!(page_frame(&mut r, 24, true), None, "the trailing ask waits behind the leading one");
}

/// Seconds between the repeats of an ask that nothing answered, frame by frame at 60 Hz.
fn repeat_gaps(r: &mut Rig<Shelf>, offset: usize, frames: usize) -> Vec<f32> {
    let (mut gaps, mut since) = (Vec::new(), r.ms);
    for _ in 0..frames {
        if page_frame(r, offset, true).is_some() {
            gaps.push((r.ms - since) as f32 / 1000.0);
            since = r.ms;
        }
    }
    gaps
}

#[test]
fn an_unanswered_trailing_ask_is_repeated_on_a_ladder_that_caps_while_focus_stands_there() {
    let mut r = settled::<Shelf>(30);
    r.src.more = true;
    r.land_focus(129, By::Dir);
    // the scroll settling moves the look-ahead `end`, which is a new ask each time: let it settle
    repeat_gaps(&mut r, 0, 180);
    r.sect.reset_page_requests();
    assert_eq!(page_frame(&mut r, 0, true), Some(PageEdge::After));
    // two minutes standing at the edge, no key, nothing landing
    let gaps = repeat_gaps(&mut r, 0, 120 * 60);
    assert!(gaps.len() >= 8, "the ask keeps being repeated: {gaps:?}");
    assert!((gaps[0] - 0.25).abs() < 0.05, "the first repeat is quick: {gaps:?}");
    assert!(gaps.windows(2).take(6).all(|w| w[1] > w[0] * 1.8), "the interval climbs a rung each time: {gaps:?}");
    assert!(gaps.iter().all(|g| *g < 30.2), "and never passes the 30 s cap: {gaps:?}");
    assert!((gaps[gaps.len() - 1] - 30.0).abs() < 0.1, "the cap repeats for as long as the user stays: {gaps:?}");
    // leaving the zone withdraws it and ends the repeats
    r.land_focus(110, By::Dir);
    assert_eq!(cancel_frame(&mut r, 0, true), Some(PageEdge::After));
    assert!(repeat_gaps(&mut r, 0, 60 * 40).is_empty(), "no repeats once focus has left the edge");
}

#[test]
fn an_unanswered_leading_ask_is_repeated_and_a_fresh_window_starts_the_ladder_over() {
    let mut r = settled::<Shelf>(30);
    r.src.more = true;
    r.land_focus(102, By::Dir);
    assert_eq!(page_frame(&mut r, 24, true), Some(PageEdge::Before));
    let gaps = repeat_gaps(&mut r, 24, 60 * 10);
    assert!(gaps.len() >= 4 && (gaps[0] - 0.25).abs() < 0.05, "{gaps:?}");
    // a landing that moves the window (another offset) is a new ask, quick again
    assert_eq!(page_frame(&mut r, 12, true), Some(PageEdge::Before));
    let gaps = repeat_gaps(&mut r, 12, 60);
    assert!((gaps[0] - 0.25).abs() < 0.05, "a new window starts at the first rung: {gaps:?}");
}

#[test]
fn no_repeat_is_sent_when_the_source_has_nothing_more_there() {
    let mut r = settled::<Shelf>(30);
    r.src.more = true;
    r.land_focus(129, By::Dir);
    assert_eq!(page_frame(&mut r, 0, true), Some(PageEdge::After));
    r.src.more = false;
    assert!(repeat_gaps(&mut r, 0, 60 * 60).is_empty(), "the end of the listing ends the asking");
}

#[test]
fn a_landing_that_changed_nothing_the_shelf_sees_restarts_the_ladder_and_is_asked_at_once() {
    let mut r = settled::<Shelf>(30);
    r.src.more = true;
    r.land_focus(129, By::Dir);
    repeat_gaps(&mut r, 0, 180);
    r.sect.reset_page_requests();
    assert_eq!(page_frame(&mut r, 0, true), Some(PageEdge::After));
    // the ask goes unanswered long enough to climb the ladder to 4 s
    let climbed = repeat_gaps(&mut r, 0, 60 * 9);
    assert!(climbed.len() >= 4 && climbed[climbed.len() - 1] >= 3.9, "{climbed:?}");
    // a landing: same len, same more, only the store's count moved
    r.src.epoch += 1;
    let mut frames = 0;
    while page_frame(&mut r, 0, true).is_none() { frames += 1; assert!(frames < 3, "asked at once, not after a rung"); }
    // and the ladder starts over: the repeat after it is the first rung
    let gaps = repeat_gaps(&mut r, 0, 60 * 3);
    assert!((gaps[0] - 0.25).abs() < 0.05, "{gaps:?}");
}

// ---- the loading edge: placeholders where the list goes on ------------------------------------

/// How many skeleton cards the shelf paints that are not real cards: every card draws through the
/// shared tile (all of this source's art is a skeleton), and only real cards reach the overlay hook.
fn pending_cells(r: &Rig<Shelf>) -> usize {
    let cx = r.cx();
    r.src.drawn.borrow_mut().clear();
    let (_, frame) = crate::placeholder::capture_declared(|| {
        let f = DrawFrame::new(&cx, Painter::root());
        let p = f.painter;
        r.sect.paint(&f, p, &r.src, SHELF_AT);
    });
    let real = r.src.drawn.borrow().len();
    (frame.of(crate::placeholder::Reason::CardSkeleton) as usize).saturating_sub(real)
}

/// Most placeholders a row can paint past one edge: a screenful of cells, and one for the cell
/// that straddles the screen's far side.
fn pending_bound() -> usize {
    (crate::consts::SCR_W / (RowStyle::HOME.w + RowStyle::HOME.gap)).ceil() as usize + 1
}

#[test]
fn a_row_with_more_past_its_tail_paints_placeholders_there_and_one_without_paints_none() {
    let mut r = settled::<Shelf>(30);
    r.land_focus(129, By::Restore);
    r.run(240);
    assert_eq!(pending_cells(&r), 0, "the list ends here: nothing past the last card");
    r.src.more = true;
    let n = pending_cells(&r);
    assert!(n >= 1 && n <= pending_bound(), "placeholders past the tail, bounded to a screenful: {n}");
}

#[test]
fn a_short_row_that_goes_on_fills_the_viewport_with_placeholders() {
    let mut r = settled::<Shelf>(3);
    r.src.more = true;
    r.land_focus(100, By::Restore);
    r.run(60);
    let n = pending_cells(&r);
    // three cards leave 1920 - 96 - 3 * 280 = 984 px: three whole cells and part of a fourth
    assert!(n >= 4 && n <= pending_bound(), "the empty viewport is filled: {n}");
}

#[test]
fn a_row_with_cards_before_its_head_paints_placeholders_there_and_one_without_paints_none() {
    let mut r = settled::<Shelf>(30);
    r.land_focus(100, By::Restore);
    r.run(240);
    assert_eq!(pending_cells(&r), 0, "nothing before the first card: no placeholder");
    r.src.before = true;
    let n = pending_cells(&r);
    assert!(n >= 1 && n <= pending_bound(), "placeholders before the head, bounded: {n}");
    // a row too short to fill the screen has room for both edges' cells at once
    let mut short = settled::<Shelf>(2);
    short.land_focus(100, By::Restore);
    short.run(60);
    short.src.before = true;
    let one = pending_cells(&short);
    short.src.more = true;
    assert!(pending_cells(&short) > one, "and both edges at once paint both");
}

/// The placeholders past a row's tail are a promise: a row that paints one in view asks for the
/// page that fills it whether or not focus is in the row, and a row whose cards reach past the
/// screen asks only as focus nears its end (Detail's Related row, cut to one card under a
/// collection, stood with a screenful of skeletons until somebody focused it).
#[test]
fn a_row_with_placeholders_in_view_asks_for_its_page_without_focus() {
    let mut short = settled::<Shelf>(2);
    short.src.more = true;
    assert!(pending_cells(&short) > 0, "the row paints the cells past its two cards");
    assert_eq!(short.focus, None, "nobody has focused it");
    assert_eq!(page_frame(&mut short, 0, true), Some(PageEdge::After), "it asks for them");
    assert_eq!(cancel_frame(&mut short, 0, true), None, "and the ask stands while they are in view");
    let mut long = settled::<Shelf>(30);
    long.src.more = true;
    assert!((0..60).all(|_| page_frame(&mut long, 0, true).is_none()), "no placeholder in view, no focus: no ask");
}

/// The card a step from `from` goes to, `None` at the row's edge.
fn step_to(r: &Rig<Shelf>, from: u32, dir: Dir) -> Option<u32> {
    match r.sect.neighbour(&r.src, r.key(from), dir) {
        Step::Move(k) => Some(k.elem),
        Step::Edge => None,
    }
}

#[test]
fn focus_never_lands_on_a_placeholder() {
    let mut r = settled::<Shelf>(10);
    r.src.more = true;
    r.src.before = true;
    r.land_focus(109, By::Restore);
    r.run(240);
    for stop in r.stops(1.0) {
        assert!(r.src.elems.contains(&stop.key.elem), "a stop for no card: {}", stop.key.elem);
    }
    assert_eq!(step_to(&r, 109, Dir::Right), None, "Right at the last real card is the edge");
    assert_eq!(step_to(&r, 100, Dir::Left), None, "and Left at the first");
}

#[test]
fn a_landing_page_takes_the_placeholders_cells_with_the_row_in_place() {
    let mut r = settled::<Shelf>(30);
    r.src.more = true;
    r.land_focus(129, By::Restore);
    r.run(240);
    let x = |r: &Rig<Shelf>, e: u32| Section::place(&r.sect, &r.cx(), &r.src, e, At::Drawn).unwrap().rect.x;
    let before = x(&r, 129);
    assert_eq!(step_to(&r, 129, Dir::Right), None, "held at the edge until the page lands");
    r.src.elems.extend(200..212);
    r.run(1);
    // (a pixel of the focus pop's spring settling, not a re-seat)
    assert!((x(&r, 129) - before).abs() < 2.0, "the focused card did not move: {} -> {}", before, x(&r, 129));
    assert_eq!(step_to(&r, 129, Dir::Right), Some(200),
        "and the next repeat of the held key steps onto the first card of the page");
    r.src.more = false;
    assert_eq!(pending_cells(&r), 0, "the list ended with that page: no placeholders");
}

#[test]
fn placeholders_are_not_pooled_and_a_long_row_paints_the_same_few() {
    let mut r = settled::<Shelf>(30);
    r.src.more = true;
    r.land_focus(129, By::Restore);
    r.run(240);
    let (pooled, few) = (r.sect.pool_len(), pending_cells(&r));
    r.src.elems = (100..1100).collect();
    r.land_focus(1099, By::Restore);
    r.run(240);
    assert!(r.sect.pool_len() <= pooled.max(2) + 1, "no element per placeholder: {}", r.sect.pool_len());
    assert!(pending_cells(&r) <= few.max(pending_bound()), "the count is the screen's, not the list's");
}

// ---- idle ------------------------------------------------------------------------------------

/// Motion is reported while a pop runs and nothing is reported once everything settles, with focus
/// held and after focus leaves.
fn goes_quiet<S: Section>() {
    let mut r = settled::<S>(8);
    r.land_focus(100, By::Dir);
    assert!(r.run(1), "a pop reports motion");
    r.run(300);
    assert!(!r.run(1), "a settled section is quiet with focus on it");
    r.focus = None;
    r.run(300);
    assert!(!r.run(1), "…and quiet once focus has left");
}
#[test]
fn shelf_goes_quiet_at_rest() {
    goes_quiet::<Shelf>();
}
#[test]
fn grid_goes_quiet_at_rest() {
    goes_quiet::<Grid>();
}

// ---- generalised geometry and the external scroll mode -----------------------------------------

/// Tick the grid alone (no owner half), one frame.
fn tick_grid_alone(r: &mut Rig<ExtGrid>) {
    r.ms += MS;
    let cx = cx9(&r.view, r.ms, r.press, r.focus);
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(9)), &mut present);
    r.sect.grid.on(&ScreenEvent::Tick(Tick { ms: r.ms, dt_us: 16_667 }), &cx, &r.src, &mut fx);
}

/// `GridSpec::new` is the six columns of `poster_grid::STYLE` from `MARGIN_X`: Collection's call
/// site gets exactly the geometry the free functions describe.
#[test]
fn the_default_spec_is_the_collection_geometry() {
    let mut r = settled::<Grid>(30);
    r.land_focus(100, By::Restore);
    r.run(240);
    let bands = poster_grid::settled(Some(0));
    for (i, e) in [(0usize, 100u32), (5, 105), (13, 113)] {
        let at = r.sect.place(&r.cx(), &r.src, &e, At::SpringTarget).unwrap().rest_rect;
        let want = poster_grid::cell(i, 520.0, r.sect.scroll(), &bands).scaled(poster_grid::STYLE.focus_scale);
        assert_eq!(at, want, "card {i}");
    }
}

/// Columns, card size and left edge are the spec's: cells sit on its pitch from its edge, the
/// D-pad wraps at its column count, the group registers it, and only the visible rows draw.
#[test]
fn a_grid_takes_its_columns_style_and_left_edge_from_its_spec() {
    let mut r = settled::<ExtGrid>(600);
    r.land_focus(100, By::Restore);
    r.run(2);
    let g = |e: u32| r.sect.place(&r.cx(), &r.src, e, At::SpringTarget).unwrap().rest_rect;
    let (c0, c1, r1, r2) = (g(100), g(101), g(104), g(108));
    assert!((c0.cx() - (160.0 + EXT_STYLE.w / 2.0)).abs() < 0.01, "column 0 starts at the left edge");
    assert!((c1.cx() - c0.cx() - (EXT_STYLE.w + EXT_STYLE.gap)).abs() < 0.01, "the next column is one card + gap over");
    assert!((r1.cx() - c0.cx()).abs() < 0.01, "the fifth card wraps to column 0 of the next row");
    let pitch = EXT_STYLE.h + crate::card_row::LABEL_BAND_COLLAPSED + crate::consts::UNDER_LABEL_AIR;
    assert!(((r2.y + r2.h / 2.0) - (r1.y + r1.h / 2.0) - pitch).abs() < 0.01, "rows are one style pitch apart");
    assert!(matches!(r.sect.grid.neighbour(&r.src, r.key(100), Dir::Down), Step::Move(k) if k == r.key(104)));
    assert!(matches!(r.sect.grid.neighbour(&r.src, r.key(103), Dir::Right), Step::Edge), "the last column is the fourth");
    match r.sect.grid.group_spec(plx_machine::machine::GroupId(1), 600).kind {
        crate::screen::GroupKind::Grid { cols, .. } => assert_eq!(cols, 4),
        _ => panic!("a grid registers a Grid group"),
    }
    let n = r.stops(1.0).len();
    assert!(n > 0 && n < 60, "the grid registered {n} of 600 stops");
}

/// In external mode the grid never moves a scroll of its own: ticks leave the owner's page where
/// it is, `reveal_target` is the scroll the focused row wants (and `None` with focus elsewhere),
/// and `set_page` is what moves the cells.
#[test]
fn an_external_grid_leaves_the_page_scroll_to_its_owner() {
    assert_eq!(EXT.scroll, ScrollMode::External);
    assert!(!EXT.home_when_unfocused, "an externally scrolled grid never homes on its own");
    let mut r = Rig::<ExtGrid>::new(80);
    r.land_focus(100, By::Restore);
    tick_grid_alone(&mut r);
    assert_eq!(r.sect.grid.reveal_target(), Some(0.0), "row 0 wants the head");
    r.land_focus(160, By::Dir);
    for _ in 0..60 {
        tick_grid_alone(&mut r);
    }
    assert_eq!(r.sect.grid.scroll(), 0.0, "the grid did not scroll itself");
    let want = r.sect.grid.reveal_target().expect("a focused row wants a scroll");
    assert!(want > 100.0, "row 15 of 4 columns is below the fold: {want}");
    let g = poster_grid::Geom::of(EXT.cols, EXT.left, &EXT_STYLE);
    assert_eq!(want, poster_grid::snap_row_in(&g, 0.0, 15, 80, 520.0, 96.0));
    let before = r.sect.grid.place(&r.cx(), &r.src, &160, At::SpringTarget).unwrap().rest_rect;
    r.sect.grid.set_page(520.0, want);
    let after = r.sect.grid.place(&r.cx(), &r.src, &160, At::SpringTarget).unwrap().rest_rect;
    assert!((before.y - after.y - want).abs() < 0.01, "set_page moves the cells by the page scroll");
    assert_eq!(r.sect.grid.scroll(), want);
    r.focus = None;
    tick_grid_alone(&mut r);
    assert_eq!(r.sect.grid.reveal_target(), None, "focus elsewhere wants nothing");
    assert_eq!(r.sect.grid.scroll(), want, "and does not scroll the page home");
}

/// A landing that moves the focused element reports the document shift its row moved by instead
/// of shifting a scroll the grid does not own; applying it keeps the tile where it was.
#[test]
fn an_external_grid_reports_the_landing_shift() {
    let mut r = Rig::<ExtGrid>::new(40);
    r.land_focus(110, By::Restore);
    r.run(240);
    let before = r.sect.grid.place(&r.cx(), &r.src, &110, At::Drawn).unwrap().rect;
    for k in 0..EXT.cols {
        r.src.elems.insert(0, 900 + k as u32);
    }
    tick_grid_alone(&mut r);
    let pitch = EXT_STYLE.h + crate::card_row::LABEL_BAND_COLLAPSED + crate::consts::UNDER_LABEL_AIR;
    assert_eq!(r.sect.grid.landed(), Some(super::Landed { from: 10, to: 10 + EXT.cols }));
    assert!((r.sect.grid.landed_shift() - pitch).abs() < 0.01, "one row moved down: {}", r.sect.grid.landed_shift());
    assert_eq!(r.sect.grid.scroll(), r.sect.page, "the grid's scroll is the owner's, untouched");
    r.sect.grid.set_page(520.0, r.sect.page + r.sect.grid.landed_shift());
    let now = r.sect.grid.place(&r.cx(), &r.src, &110, At::Drawn).unwrap().rect;
    assert!((now.y - before.y).abs() < 0.5, "the tile stayed put once the owner applied the shift: {before:?} -> {now:?}");
    tick_grid_alone(&mut r);
    assert_eq!(r.sect.grid.landed_shift(), 0.0, "and the next tick reports nothing");
}

/// The other direction: rows removed above the focused element move it to an EARLIER row. The
/// owner settles the band on the new row before the tick (`settle_band`), and the shift must still
/// be the rows' pitch exactly, not the pitch less the band the new row has opened.
#[test]
fn an_external_grid_reports_the_landing_shift_for_an_earlier_row() {
    let mut r = Rig::<ExtGrid>::new(40);
    r.land_focus(110, By::Restore);
    r.run(240);
    let before = r.sect.grid.place(&r.cx(), &r.src, &110, At::Drawn).unwrap().rect;
    r.src.elems.drain(0..EXT.cols);
    r.sect.grid.settle_band(Some(1));
    tick_grid_alone(&mut r);
    let pitch = EXT_STYLE.h + crate::card_row::LABEL_BAND_COLLAPSED + crate::consts::UNDER_LABEL_AIR;
    assert_eq!(r.sect.grid.landed(), Some(super::Landed { from: 10, to: 10 - EXT.cols }));
    assert!((r.sect.grid.landed_shift() + pitch).abs() < 0.01, "one row moved up: {}", r.sect.grid.landed_shift());
    r.sect.grid.set_page(520.0, r.sect.page + r.sect.grid.landed_shift());
    let now = r.sect.grid.place(&r.cx(), &r.src, &110, At::Drawn).unwrap().rect;
    assert!((now.y - before.y).abs() < 0.5, "the tile stayed put once the owner applied the shift: {before:?} -> {now:?}");
}

/// `Shelf::head` is the first card's settled, unpopped rect: the one `place` answers for it while
/// it is not focused.
#[test]
fn shelf_head_is_the_first_cards_unpopped_slot() {
    let mut r = settled::<Shelf>(8);
    r.focus = None;
    r.run(300);
    let placed = r.place(100, At::SpringTarget).unwrap();
    assert_eq!(r.sect.head(SHELF_AT), placed.rect);
    assert_eq!(placed.rect.y, SHELF_AT.y);
}

/// A dormant shelf (its page still dissolving in) holds every card at rest, whatever focus does,
/// and the first awake tick starts the focused card's pop FROM REST, as a deliberate move does,
/// even for an arrival that would otherwise be adopted whole. Nothing is lifted meanwhile.
#[test]
fn a_dormant_shelf_keeps_its_cards_at_rest_and_wakes_growing_from_rest() {
    let mut r = settled::<Shelf>(8);
    let full = RowStyle::HOME.focus_scale;
    r.sect.dormant(true);
    r.land_focus(102, By::Restore);
    for _ in 0..30 {
        r.run(1);
        assert_eq!(r.scale(102), Some(1.0), "no lift under the fade");
        assert_eq!(lifted(&r), 0);
    }
    assert!(!r.run(1), "a dormant shelf at rest is quiet");
    r.sect.dormant(false);
    r.run(1);
    let first = r.scale(102).unwrap();
    assert!(first > 1.0 && first < full - 0.02, "the pop starts from rest, not whole: {first}");
    r.run(200);
    assert!((r.scale(102).unwrap() - full).abs() < 0.002);
}

/// The restore rule survives idle dormant ticks: a restore announced AFTER the shelf slept, on the
/// tick the page wakes, is adopted whole, while a deliberate arrival after the same ticks still
/// grows from rest.
#[test]
fn a_restore_after_dormant_ticks_arrives_popped_and_a_deliberate_move_still_grows() {
    let full = RowStyle::HOME.focus_scale;
    let mut restored = settled::<Shelf>(8);
    restored.sect.dormant(true);
    restored.run(3);
    restored.sect.dormant(false);
    restored.land_focus(102, By::Restore);
    restored.run(1);
    assert_eq!(restored.scale(102), Some(full), "a restore arrives already popped");

    let mut dived = settled::<Shelf>(8);
    dived.sect.dormant(true);
    dived.run(3);
    dived.sect.dormant(false);
    dived.land_focus(102, By::Dir);
    dived.run(1);
    let first = dived.scale(102).unwrap();
    assert!(first > 1.0 && first < full - 0.02, "a deliberate arrival grows from rest: {first}");
}

/// The grid's twin of the shelf's dormant rule (the Library and the Collections page arrive in
/// their first poster under a dissolve): nothing is lifted meanwhile, whatever focus does, and the
/// first awake tick starts the focused card's pop FROM REST on the standard spring, even for an
/// arrival that would otherwise be adopted whole.
#[test]
fn a_dormant_grid_keeps_its_cards_at_rest_and_wakes_growing_from_rest() {
    let mut r = settled::<Grid>(8);
    let full = RowStyle::HOME.focus_scale;
    r.sect.dormant(true);
    r.land_focus(102, By::Restore);
    for _ in 0..30 {
        r.run(1);
        assert_eq!(r.scale(102), Some(1.0), "no lift under the fade");
        assert_eq!(lifted(&r), 0);
    }
    r.sect.dormant(false);
    r.run(1);
    let first = r.scale(102).unwrap();
    assert!(first > 1.0 && first < full - 0.02, "the pop starts from rest, not whole: {first}");
    r.run(200);
    assert!((r.scale(102).unwrap() - full).abs() < 0.002);
    assert_eq!(lifted(&r), 1);
}

/// The restore rule survives idle dormant ticks on a grid too: a restore announced AFTER the grid
/// slept, on the tick the page wakes, is adopted whole, while a deliberate arrival still grows.
#[test]
fn a_grid_restore_after_dormant_ticks_arrives_popped_and_a_deliberate_move_still_grows() {
    let full = RowStyle::HOME.focus_scale;
    let mut restored = settled::<Grid>(8);
    restored.sect.dormant(true);
    restored.run(3);
    restored.sect.dormant(false);
    restored.land_focus(102, By::Restore);
    restored.run(1);
    assert_eq!(restored.scale(102), Some(full), "a restore arrives already popped");

    let mut dived = settled::<Grid>(8);
    dived.sect.dormant(true);
    dived.run(3);
    dived.sect.dormant(false);
    dived.land_focus(102, By::Dir);
    dived.run(1);
    let first = dived.scale(102).unwrap();
    assert!(first > 1.0 && first < full - 0.02, "a deliberate arrival grows from rest: {first}");
}

/// A captioned grid of four rows, card 100 (the first) focused and settled lifted, the way a page
/// is left when a Detail is opened from it.
fn left_lifted() -> Rig<Grid> {
    let mut r = Rig::<Grid>::new(4 * poster_grid::COLS);
    r.src.captioned = true;
    r.sect.set_caption_policy(Some(crate::caption_policy::CaptionPolicy {
        title: true,
        details: crate::caption_policy::DetailsKind::YearLength,
    }));
    r.run(2);
    r.land_focus(100, By::Dir);
    r.run(200);
    assert!(r.scale(100).unwrap() > 1.05, "premise: the card is left lifted");
    r
}

/// The caption alpha of card 100 on the frame the grid last drew.
fn caption_of_100(r: &Rig<Grid>) -> f32 {
    let cx = r.cx();
    let f = DrawFrame::new(&cx, Painter::root());
    r.sect.caption_probe(&f, f.painter, &r.src).iter().filter(|(i, _)| *i == 0).map(|(_, a)| *a).sum()
}

/// **A page that returns is not put to sleep.** BACK from a Detail runs under the dip's image, which
/// the owner reports as dormant; a seat put at rest there and grown again moves the row under its
/// caption band by the band's height and back. A grid fed `Enter(Restored)` draws the seat the
/// engine restored lifted with its caption open from the first frame, through every dormant tick,
/// and keeps it when the owner wakes it. Both a grid that slept while covered and a fresh one (the
/// stack let the page go and rebuilt it) come back that way.
#[test]
fn a_grid_that_returns_under_a_held_image_keeps_its_seat_lifted_and_its_caption_open() {
    let full = RowStyle::HOME.focus_scale;
    for covered in [true, false] {
        let mut r = if covered {
            // the page slept under the image of the push that covered it, then went unticked
            let mut r = left_lifted();
            r.sect.dormant(true);
            r.run(3);
            assert_eq!(r.scale(100), Some(1.0), "premise: it slept");
            r.focus = None;
            r
        } else {
            Rig::<Grid>::new(4 * poster_grid::COLS)
        };
        if !covered {
            r.src.captioned = true;
            r.sect.set_caption_policy(Some(crate::caption_policy::CaptionPolicy {
                title: true,
                details: crate::caption_policy::DetailsKind::YearLength,
            }));
        }
        r.sect.dormant(true);
        r.feed(ScreenEvent::Enter(Enter::Restored));
        r.land_focus(100, By::Restore);
        assert_eq!(r.scale(100), Some(full), "covered={covered}: whole on the first frame, before any tick");
        for frame in 0..30 {
            r.run(1);
            assert_eq!(r.scale(100), Some(full), "covered={covered}: lifted under the image, frame {frame}");
        }
        assert!((caption_of_100(&r) - 1.0).abs() < 0.01, "covered={covered}: its caption is open under the image: {}", caption_of_100(&r));
        r.sect.dormant(false);
        for frame in 0..30 {
            r.run(1);
            assert_eq!(r.scale(100), Some(full), "covered={covered}: still lifted once awake, frame {frame}");
            assert!((caption_of_100(&r) - 1.0).abs() < 0.01, "covered={covered}: caption kept once awake, frame {frame}");
        }
    }
}

/// **A return leaves no mark.** `Enter(Restored)` reaches a live page too (a modal's dismissal), with
/// no image after it: the first tick that reports awake ends the return, so a later dissolve puts
/// the seat to rest and the lift grows on the awake tick, as on a page that never returned.
#[test]
fn a_grid_that_returned_with_no_image_sleeps_through_the_next_dissolve() {
    let full = RowStyle::HOME.focus_scale;
    let mut r = left_lifted();
    r.focus = None;
    r.feed(ScreenEvent::Enter(Enter::Restored));
    r.land_focus(100, By::Restore);
    r.run(2);
    assert!((r.scale(100).unwrap() - full).abs() < 1e-4, "premise: it came back lifted");
    r.sect.dormant(true);
    r.run(3);
    assert_eq!(r.scale(100), Some(1.0), "a later dissolve puts the seat to rest");
    assert_eq!(lifted(&r), 0);
    r.sect.dormant(false);
    r.run(1);
    let first = r.scale(100).unwrap();
    assert!(first > 1.0 && first < full - 0.02, "and the lift grows from rest once the page is in: {first}");
}

/// **The reader moving ends the return.** A key pressed while the image of the way back is still up
/// is slept through like an entry's: nothing is lifted or opened under the image (the move would
/// hold it), and the new seat grows from rest once the owner wakes the grid.
#[test]
fn a_deliberate_move_ends_a_grids_return_and_is_slept_through() {
    let full = RowStyle::HOME.focus_scale;
    let mut r = left_lifted();
    r.focus = None;
    r.sect.dormant(true);
    r.feed(ScreenEvent::Enter(Enter::Restored));
    r.land_focus(100, By::Restore);
    r.run(3);
    assert!((r.scale(100).unwrap() - full).abs() < 1e-4, "premise: it came back lifted");
    r.land_focus(101, By::Dir);
    for frame in 0..10 {
        r.run(1);
        assert_eq!(r.scale(100), Some(1.0), "the seat it left is at rest, frame {frame}");
        assert_eq!(r.scale(101), Some(1.0), "the seat it moved to is at rest, frame {frame}");
    }
    assert!(!r.run(1), "nothing moves under the image");
    r.sect.dormant(false);
    r.run(1);
    let first = r.scale(101).unwrap();
    assert!(first > 1.0 && first < full - 0.02, "the new seat grows from rest once the page is in: {first}");
}

/// A grid that returned lifted under a held image, then had focus leave its cards: Up from the
/// first row onto a header control (a deliberate move that names no card of the grid), or the
/// BACK that walks there (a seat the page makes for itself, announced as a restore). Elem 7 is on
/// the page and not a card of the grid.
fn returned_then_focus_leaves(by: By) -> Rig<Grid> {
    let mut r = left_lifted();
    r.focus = None;
    r.sect.dormant(true);
    r.feed(ScreenEvent::Enter(Enter::Restored));
    r.land_focus(100, By::Restore);
    r.run(3);
    assert!((r.scale(100).unwrap() - RowStyle::HOME.focus_scale).abs() < 1e-4, "premise: it came back lifted");
    assert!((caption_of_100(&r) - 1.0).abs() < 0.01, "premise: its caption is open");
    r.land_focus(7, by);
    r
}

/// **Leaving the grid under the image of the way back waits for nothing.** The seat the page kept
/// lifted lets go and its caption band shuts at once, like an entry's: a band left closing on its
/// spring (and a pop letting go on its own) is layout motion, and the image waits for it.
#[test]
fn a_grid_that_returned_sleeps_through_focus_leaving_it() {
    for by in [By::Dir, By::Pointer, By::Restore] {
        let mut r = returned_then_focus_leaves(by);
        for frame in 0..8 {
            assert!(!r.run(1), "{by:?}: something moves under the image, frame {frame}");
            assert_eq!(r.scale(100), Some(1.0), "{by:?}: the seat it left is at rest, frame {frame}");
            assert_eq!(caption_of_100(&r), 0.0, "{by:?}: a caption is left closing under the image, frame {frame}");
        }
        assert_eq!(lifted(&r), 0, "{by:?}");
    }
}

/// **A landing is adopted whole under the image, a returning grid included.** A card that left or
/// arrived while the page was covered (the item watched, a refetch) must not play its ghost and its
/// slides under the image that hides them: the animation would hold the image to its bound and
/// then be cut.
#[test]
fn a_grid_that_returned_adopts_a_landing_whole_under_the_image() {
    let mut r = animated_grid(40, 100, GRID);
    r.focus = None;
    r.sect.dormant(true);
    r.run(2);
    r.src.elems.remove(11);
    r.src.elems.insert(13, 900);
    r.feed(ScreenEvent::Enter(Enter::Restored));
    r.land_focus(100, By::Restore);
    for frame in 0..4 {
        r.run(1);
        assert!(r.sect.ghost_probe().0.is_empty(), "a ghost plays under the image, frame {frame}");
    }
    r.sect.dormant(false);
    r.run(2);
    let n = r.src.elems.len();
    assert_eq!(r.sect.landing_probe(n, 13), ((0.0, 0.0), 1.0));
    assert_eq!(r.sect.landing_probe(n, 10), ((0.0, 0.0), 1.0));
    assert!(r.sect.ghost_probe().0.is_empty());
}

/// **A layout built before the first tick sizes the document from the returning seat's band.**
/// `settle_band` is the owner's way to adopt a restored focus row's caption band at once (the
/// Library's `relayout`, which runs on `Enter(Restored)` while the grid still reads the dormant
/// flag of the dip that covered the page): a returning grid takes it, a sleeping one does not.
#[test]
fn settle_band_opens_a_returning_grids_band_but_not_a_sleeping_ones() {
    let open = |r: &Rig<Grid>| r.sect.band_geometry().iter().any(|band| band.expansion > 0.99);
    let mut sleeping = left_lifted();
    sleeping.focus = None;
    sleeping.sect.dormant(true);
    sleeping.run(3);
    sleeping.sect.settle_band(Some(0));
    assert!(!open(&sleeping), "a sleeping grid's band stays shut");

    let mut returning = left_lifted();
    returning.focus = None;
    returning.sect.dormant(true);
    returning.run(3);
    assert!(!open(&returning), "premise: the covered grid's band is shut");
    returning.feed(ScreenEvent::Enter(Enter::Restored));
    returning.sect.settle_band(Some(0));
    assert!(open(&returning), "a returning grid's band opens before any tick");
}

/// A lifted grid card that goes dormant lets go and the grid keeps no lift while it sleeps.
#[test]
fn a_grid_that_goes_dormant_lets_its_lifted_card_go() {
    let mut r = settled::<Grid>(8);
    r.land_focus(101, By::Dir);
    r.run(200);
    assert!(r.scale(101).unwrap() > 1.05);
    r.sect.dormant(true);
    r.run(2);
    assert_eq!(r.scale(101), Some(1.0));
    assert_eq!(lifted(&r), 0);
}

/// **A dormant seat's caption opens with the lift, not under the dissolve.** A caption is its
/// band's content (a line is drawn once the room under its card holds it and never beyond the
/// band's expansion, `card_row::line_reveal_for`), and a dormant grid keeps the focused tile at
/// rest: with the band settled open, the details line stood at full strength under a tile that had
/// not begun to lift (Library arrival, frames 19-25 of 46 measured in the simulator). The band of a
/// seat nobody has lifted yet is shut; it opens on the same frames the lift runs, a Down's way, and
/// the title comes in once the room holds it.
#[test]
fn a_dormant_seats_caption_opens_with_its_lift_and_never_before_it() {
    let cols = poster_grid::COLS;
    let full = RowStyle::HOME.focus_scale;
    let mut r = Rig::<Grid>::new(4 * cols);
    r.src.captioned = true;
    r.sect.set_caption_policy(Some(crate::caption_policy::CaptionPolicy {
        title: true,
        details: crate::caption_policy::DetailsKind::YearLength,
    }));
    r.run(2);
    // the caption of card 0 on the frame the grid last drew
    let caption = |r: &Rig<Grid>| -> f32 {
        let cx = r.cx();
        let f = DrawFrame::new(&cx, Painter::root());
        r.sect.caption_probe(&f, f.painter, &r.src).iter().filter(|(i, _)| *i == 0).map(|(_, a)| *a).sum()
    };
    r.sect.dormant(true);
    r.land_focus(100, By::Restore);
    for frame in 0..30 {
        r.run(1);
        assert_eq!(r.scale(100), Some(1.0), "no lift under the fade, frame {frame}");
        assert_eq!(caption(&r), 0.0, "a caption under a tile that has not lifted, frame {frame}");
    }
    r.sect.dormant(false);
    // (caption alpha, lift progress 0..1) on each awake frame
    let trace: Vec<(f32, f32)> = (0..80).map(|_| {
        r.run(1);
        (caption(&r), (r.scale(100).unwrap() - 1.0) / (full - 1.0))
    }).collect();
    let early: Vec<_> = trace.iter().enumerate().filter(|(_, (alpha, lift))| *alpha > 0.0 && *lift <= 0.0).collect();
    assert!(early.is_empty(), "a caption on a frame the tile is still at rest: {early:?}");
    let (first, lift) = trace[0];
    assert!(first == 0.0 && lift > 0.0 && lift < 0.5, "the lift starts from nothing and the caption waits for its room: caption {first}, lift {lift}");
    assert!(trace.iter().any(|&(alpha, lift)| alpha > 0.0 && lift < 1.0), "the title comes in while the tile is still lifting: {trace:?}");
    assert!(trace.windows(2).all(|w| w[1].0 >= w[0].0 - 1e-4), "the caption does not open monotonically: {trace:?}");
    assert!((trace[79].0 - 1.0).abs() < 0.01 && (trace[79].1 - 1.0).abs() < 0.01, "caption and lift both end full: {:?}", trace[79]);
}

/// **Captions are painted beneath the tiles (CONFIRM judgment 1).** A caption used to paint with its
/// card: the focused card's after the whole page of tiles, so an arriving details line was over the
/// poster tops of the row below it (fixc-after-down1: at n=17 the line sat over the next row's
/// poster top at ~50% of the band's opening). Draw order only, no timing: every caption of the
/// frame is painted before any tile.
#[test]
fn captions_are_painted_beneath_every_tile() {
    let policy = crate::caption_policy::CaptionPolicy {
        title: true,
        details: crate::caption_policy::DetailsKind::YearLength,
    };
    let mut r = Rig::<Grid>::new(4 * poster_grid::COLS);
    r.src.captioned = true;
    r.sect.set_caption_policy(Some(policy));
    r.run(2);
    r.land_focus(100, By::Dir);
    r.run(200);
    let cx = r.cx();
    let tile = r.sect.place(&cx, &r.src, &100, At::Drawn).expect("the focused card").rect;
    let log = crate::draw_census::capture(|| {
        let mut f = DrawFrame::new(&cx, Painter::recording());
        r.sect.draw(&mut f, Painter::recording(), &r.src);
    });
    // the caption's text runs: in the focused card's column, in the band under it
    let band = tile.y + tile.h..tile.y + tile.h + crate::caption_policy::band_h(&policy);
    let captions: Vec<usize> = log.iter().enumerate()
        .filter(|(_, (tag, at))| *tag == 100 && band.contains(&at.y) && at.x >= tile.x - 1.0 && at.x < tile.x + tile.w)
        .map(|(k, _)| k).collect();
    assert!(captions.len() >= 2, "the title and the details line are on screen: {log:?}");
    let first_tile = log.iter().position(|(tag, _)| *tag == TILE_PAINTED).expect("the grid paints tiles");
    assert!(captions.iter().all(|&k| k < first_tile),
        "every caption run (log positions {captions:?}) comes before the first tile primitive ({first_tile})");
}

/// A lifted card that goes dormant lets go over frames and ends parked exactly at rest.
#[test]
fn a_shelf_that_goes_dormant_lets_its_lifted_card_go() {
    let mut r = settled::<Shelf>(8);
    r.land_focus(101, By::Dir);
    r.run(200);
    assert!(r.scale(101).unwrap() > 1.05);
    r.sect.dormant(true);
    r.run(300);
    assert_eq!(r.scale(101), Some(1.0));
    assert!(r.sect.at_rest(), "parked exactly at rest, so an owner may stop stepping it");
    assert!(!r.run(1));
}

/// The page image a test sets must not outlive it (`PAGE_IMAGE_HELD` is a thread-local the
/// dispatcher sets and a section reads on its next tick).
struct ImageHeld;
impl ImageHeld {
    fn set() -> Self {
        crate::containers::transition::set_page_image_held(true);
        ImageHeld
    }
}
impl Drop for ImageHeld {
    fn drop(&mut self) {
        crate::containers::transition::set_page_image_held(false);
    }
}

/// **Nothing steps under an image that hides it.** A shelf that is dormant while the page is a held
/// image (a route dip's Out and In halves, the hold after it) parks at exact rest on its FIRST
/// tick, as a grid put to sleep does (a section that RETURNS is not put to sleep, below): the card that
/// was lifted, the caption band and the heading's lift are all closed at once, with no spring left
/// moving to hold the image past the dissolve.
#[test]
fn a_dormant_shelf_under_a_held_page_image_parks_on_its_first_tick() {
    let mut r = settled::<Shelf>(8);
    r.land_focus(101, By::Dir);
    r.run(200);
    let full = RowStyle::HOME.focus_scale;
    assert!(r.scale(101).unwrap() > full - 0.002, "premise: the card is whole in its lift");
    assert!(r.sect.under_band() > crate::card_row::under_band(0.0) + 1.0, "premise: the focused card's caption band is open");
    let _held = ImageHeld::set();
    r.sect.dormant(true);
    r.run(1);
    assert_eq!(r.scale(101), Some(1.0), "the lifted card is at rest on the first tick under the image");
    assert_eq!(r.sect.heading_lift(), 0.0, "and the heading it lifted is down");
    assert_eq!(r.sect.under_band(), crate::card_row::under_band(0.0), "and the caption band it opened is shut");
    assert!(r.sect.at_rest(), "no spring is left moving");
    assert!(!r.run(1), "and the shelf reports no motion to hold the image");
}

/// **A shelf whose let-go was under way when the image went up finishes it at once.** The page
/// that is covered by a pushed screen freezes its shelves mid-ease; the first tick it runs again
/// is under the BACK dip's image, which hides the rest of the ease, so the springs jump to rest
/// there instead of stepping on under it (the Recommended page's return from a card's Detail held
/// the image ~25 frames past the dissolve and then snapped).
#[test]
fn a_dormant_shelf_mid_ease_jumps_to_rest_when_the_page_image_goes_up() {
    let mut r = settled::<Shelf>(8);
    r.land_focus(101, By::Dir);
    r.run(200);
    r.sect.dormant(true);
    r.run(3);
    let eased = r.scale(101).unwrap();
    assert!(eased > 1.0 + 1e-3 && eased < RowStyle::HOME.focus_scale - 1e-3, "premise: mid-ease on a visible page: {eased}");
    let _held = ImageHeld::set();
    r.run(1);
    assert_eq!(r.scale(101), Some(1.0), "the ease is finished under the image");
    assert!(r.sect.at_rest());
    assert!(!r.run(1));
}

/// **On a page that can be seen, dormant still eases** (Home's dive lets its shelves go while the
/// page stands in view): only a held image hides the ease.
#[test]
fn a_dormant_shelf_on_a_visible_page_still_eases_to_rest() {
    let mut r = settled::<Shelf>(8);
    r.land_focus(101, By::Dir);
    r.run(200);
    let full = RowStyle::HOME.focus_scale;
    r.sect.dormant(true);
    r.run(1);
    let first = r.scale(101).unwrap();
    assert!(first > 1.0 + 1e-3 && first < full, "one tick eases, it does not cut: {first}");
    assert!(!r.sect.at_rest());
    assert!(r.run(1), "and it reports its motion");
    r.run(300);
    assert_eq!(r.scale(101), Some(1.0));
    assert!(r.sect.at_rest());
}

/// A shelf of eight cards with card 101 focused and settled lifted, the way a page is left when a
/// Detail is opened from it.
fn shelf_left_lifted() -> Rig<Shelf> {
    let mut r = settled::<Shelf>(8);
    r.land_focus(101, By::Dir);
    r.run(200);
    assert!(r.scale(101).unwrap() > RowStyle::HOME.focus_scale - 0.002, "premise: the card is left lifted");
    r
}

/// The shelf's caption band is open under its lifted card, and so is the lift it gives its heading.
fn band_open(r: &Rig<Shelf>) -> bool {
    r.sect.under_band() > crate::card_row::under_band(0.0) + 1.0 && r.sect.heading_lift() > 0.0
}

/// **A page that returns is not put to sleep, a shelf's too.** BACK from a Detail runs under the
/// dip's image, which the owner reports as dormant; a seat put at rest there and grown again moves
/// the shelf under its caption band by the band's height and back (the Recommended page's next
/// shelf). A shelf fed `Enter(Restored)` draws the seat the engine restored lifted, its band open
/// and its heading raised, from the first frame, through every dormant tick under the image, and
/// keeps it when the owner wakes it. Both a shelf that slept while covered and a fresh one (the
/// stack let the page go and rebuilt it) come back that way, the band and the heading with the seat
/// on the restoring event itself: the page the dip captures that frame is laid out from them.
#[test]
fn a_shelf_that_returns_under_a_held_image_keeps_its_seat_lifted_and_its_band_open() {
    let full = RowStyle::HOME.focus_scale;
    for covered in [true, false] {
        let mut r = if covered {
            // the page slept under the image of the push that covered it, then went unticked
            let mut r = shelf_left_lifted();
            let _push = ImageHeld::set();
            r.sect.dormant(true);
            r.run(3);
            assert_eq!(r.scale(101), Some(1.0), "premise: it slept");
            r.focus = None;
            r
        } else {
            settled::<Shelf>(8)
        };
        let held = ImageHeld::set();
        r.sect.dormant(true);
        r.feed(ScreenEvent::Enter(Enter::Restored));
        r.land_focus(101, By::Restore);
        assert_eq!(r.scale(101), Some(full), "covered={covered}: whole on the first frame, before any tick");
        assert!(band_open(&r), "covered={covered}: its band and heading are open on the first frame too, for the layout read before any tick");
        for frame in 0..30 {
            r.run(1);
            assert_eq!(r.scale(101), Some(full), "covered={covered}: lifted under the image, frame {frame}");
        }
        assert!(band_open(&r), "covered={covered}: its caption band is open and its heading raised under the image");
        assert!(!r.run(1), "covered={covered}: a seat already where it goes reports no motion to hold the image");
        drop(held);
        r.sect.dormant(false);
        for frame in 0..30 {
            r.run(1);
            assert_eq!(r.scale(101), Some(full), "covered={covered}: still lifted once awake, frame {frame}");
            assert!(band_open(&r), "covered={covered}: band kept once awake, frame {frame}");
        }
    }
}

/// **A scroll glide the push froze ends at once when the page returns.** A seat past the shelf's edge
/// scrolls into view on the scroll spring, and an OK before it ends covers the page with the glide
/// half played. The seat's shelf is stepped on the way back, so it would finish the glide under the
/// BACK dip's image: layout motion, which holds the image past its dissolve and then jumps the row
/// sideways. The scroll settles with the band and the heading on the restoring event, so the page
/// the dip captures is the one that stays.
#[test]
fn a_shelf_that_returns_with_a_scroll_glide_unfinished_ends_the_glide_before_the_image_holds() {
    let seat = 119;
    let end = {
        let mut left = settled::<Shelf>(20);
        left.land_focus(seat, By::Dir);
        left.run(300);
        left.scroll().unwrap()
    };
    assert!(end > 100.0, "premise: the seat is past the edge, so the row scrolls: {end}");
    let mut r = settled::<Shelf>(20);
    r.land_focus(seat, By::Dir);
    r.run(5);
    let frozen = r.scroll().unwrap();
    assert!(frozen > 1.0 && frozen < end - 20.0, "premise: the push froze the glide half played: {frozen} of {end}");
    let push = ImageHeld::set();
    r.sect.dormant(true);
    r.run(3);
    assert_eq!(r.scroll(), Some(frozen), "premise: the covered page keeps its scroll");
    r.focus = None;
    drop(push);

    let _held = ImageHeld::set();
    r.sect.dormant(true);
    r.feed(ScreenEvent::Enter(Enter::Restored));
    r.land_focus(seat, By::Restore);
    let landed = r.scroll().unwrap();
    assert!((landed - end).abs() < 0.5, "the glide ends on the restoring event, before any tick: {landed} of {end}");
    assert!(r.sect.under_band() > crate::card_row::under_band(0.0) + 1.0, "its caption band is open");
    let raised = crate::card_row::heading_clearance(Some(19), &RowStyle::HOME, landed);
    assert_eq!(r.sect.heading_lift(), raised, "and the heading is raised for the scroll it stands at");
    for frame in 0..30 {
        r.run(1);
        assert_eq!(r.scroll(), Some(landed), "the row stands still under the image, frame {frame}");
        assert_eq!(r.sect.heading_lift(), raised, "and so does its heading, frame {frame}");
    }
    assert!(!r.run(1), "a glide already ended reports no motion to hold the image");
}

/// **A return leaves no mark.** `Enter(Restored)` reaches a live page too (a modal's dismissal), with
/// no image after it: the first tick that reports awake ends the return, so a later dissolve puts
/// the seat to rest and the lift grows on the awake tick, as on a page that never returned.
#[test]
fn a_shelf_that_returned_with_no_image_sleeps_through_the_next_dissolve() {
    let full = RowStyle::HOME.focus_scale;
    let mut r = shelf_left_lifted();
    r.focus = None;
    r.feed(ScreenEvent::Enter(Enter::Restored));
    r.land_focus(101, By::Restore);
    r.run(2);
    assert!((r.scale(101).unwrap() - full).abs() < 1e-4, "premise: it came back lifted");
    r.sect.dormant(true);
    r.run(300);
    assert_eq!(r.scale(101), Some(1.0), "a later dissolve puts the seat to rest");
    assert_eq!(lifted(&r), 0);
    r.sect.dormant(false);
    r.run(1);
    let first = r.scale(101).unwrap();
    assert!(first > 1.0 && first < full - 0.02, "and the lift grows from rest once the page is in: {first}");
}

/// **The reader moving ends the return.** A key pressed while the image of the way back is still up
/// is slept through like an entry's: nothing is lifted or opened under the image (the move would
/// hold it), and the new seat grows from rest once the owner wakes the shelf.
#[test]
fn a_deliberate_move_ends_a_shelfs_return_and_is_slept_through() {
    let full = RowStyle::HOME.focus_scale;
    let mut r = shelf_left_lifted();
    r.focus = None;
    let held = ImageHeld::set();
    r.sect.dormant(true);
    r.feed(ScreenEvent::Enter(Enter::Restored));
    r.land_focus(101, By::Restore);
    r.run(3);
    assert!((r.scale(101).unwrap() - full).abs() < 1e-4, "premise: it came back lifted");
    r.land_focus(102, By::Dir);
    r.run(1);
    assert_eq!(r.scale(101), Some(1.0), "the seat it left is at rest at once");
    assert_eq!(r.scale(102), Some(1.0), "the seat it moved to is at rest");
    assert!(!band_open(&r) && r.sect.at_rest(), "nothing is left open or moving under the image");
    assert!(!r.run(1), "nothing moves under the image");
    drop(held);
    r.sect.dormant(false);
    r.run(1);
    let first = r.scale(102).unwrap();
    assert!(first > 1.0 && first < full - 0.02, "the new seat grows from rest once the page is in: {first}");
}

/// **Leaving the shelf under the image of the way back waits for nothing.** A shelf that returned
/// lifted, then had focus leave its cards: Up onto a header control or another shelf (a deliberate
/// move that names no card of this shelf), or the BACK that walks there (a seat the page makes for
/// itself, announced as a restore). The seat lets go and its band shuts at once, like an entry's:
/// a band left closing on its spring is layout motion, and the image waits for it. Elem 7 is on the
/// page and not a card of the shelf. The return ends with the departure, whichever way focus left:
/// a card of the shelf that focus lands on afterwards, with no fresh `Enter`, is slept through.
#[test]
fn a_shelf_that_returned_sleeps_through_focus_leaving_it() {
    for by in [By::Dir, By::Pointer, By::Restore] {
        let mut r = shelf_left_lifted();
        r.focus = None;
        let _held = ImageHeld::set();
        r.sect.dormant(true);
        r.feed(ScreenEvent::Enter(Enter::Restored));
        r.land_focus(101, By::Restore);
        r.run(3);
        assert!(band_open(&r), "{by:?}: premise: it came back with its band open");
        r.land_focus(7, by);
        r.run(1);
        assert_eq!(r.scale(101), Some(1.0), "{by:?}: the seat it left is at rest at once");
        assert!(!band_open(&r) && r.sect.at_rest(), "{by:?}: a band is left closing under the image");
        assert!(!r.run(1), "{by:?}: something moves under the image");
        assert_eq!(lifted(&r), 0, "{by:?}");
        r.land_focus(102, By::Restore);
        r.run(1);
        assert_eq!(r.scale(102), Some(1.0), "{by:?}: the return ended when focus left, so a later seat is slept through");
        assert!(!band_open(&r) && r.sect.at_rest(), "{by:?}: and its band stays shut");
        assert!(!r.run(1), "{by:?}: and nothing moves under the image");
    }
}

/// **A return no move announces wakes at its lift too.** The engine's focus can stay on the card
/// while the page is covered, so the page comes back with `Enter(Restored)` and no `FocusMoved`. What
/// the shelf slept through is stale then as well: the first tick adopts the seat at full pop as it
/// does any focus no deliberate move announced, and does not grow it from rest (the wake of an
/// entry's dissolve).
#[test]
fn a_shelf_that_returns_with_no_restoring_move_wakes_at_its_lift_and_not_from_rest() {
    let full = RowStyle::HOME.focus_scale;
    let mut r = shelf_left_lifted();
    let _held = ImageHeld::set();
    r.sect.dormant(true);
    r.run(3);
    assert_eq!(r.scale(101), Some(1.0), "premise: it slept, with focus still on the card");
    r.feed(ScreenEvent::Enter(Enter::Restored));
    for frame in 0..5 {
        r.run(1);
        assert_eq!(r.scale(101), Some(full), "adopted whole, not grown from rest, frame {frame}");
    }
}

/// **Only the shelf that holds the seat returns lifted.** The Recommended page feeds every shelf
/// `Enter(Restored)`; a shelf the focus is not on (its lift was let go before the page was covered
/// and its ease froze there) is still finished at once under the image, as any dormant shelf is:
/// stepping it on would hold the image past the dissolve.
#[test]
fn a_returning_shelf_that_holds_no_seat_is_still_put_to_rest_under_the_image() {
    let mut r = shelf_left_lifted();
    r.sect.dormant(true);
    r.run(3);
    let eased = r.scale(101).unwrap();
    assert!(eased > 1.0 + 1e-3 && eased < RowStyle::HOME.focus_scale - 1e-3, "premise: mid-ease on a visible page: {eased}");
    r.focus = None;
    let _held = ImageHeld::set();
    r.feed(ScreenEvent::Enter(Enter::Restored));
    r.run(1);
    assert_eq!(r.scale(101), Some(1.0), "the ease is finished under the image");
    assert!(r.sect.at_rest());
    assert!(!r.run(1), "and the shelf reports no motion to hold the image");
}

/// **A landing is adopted whole under the image, a returning shelf included.** A card that left or
/// arrived while the page was covered (the item watched, a refetch) must not play its ghost and its
/// slides under the image that hides them: the animation would hold the image to its bound and
/// then be cut.
#[test]
fn a_shelf_that_returned_adopts_a_landing_whole_under_the_image() {
    let mut r = landing_rig();
    r.focus = None;
    let held = ImageHeld::set();
    r.sect.dormant(true);
    r.run(2);
    r.src.elems.remove(11);
    r.src.elems.insert(13, 900);
    r.feed(ScreenEvent::Enter(Enter::Restored));
    r.land_focus(112, By::Restore);
    for frame in 0..4 {
        r.run(1);
        assert!(r.sect.ghost_probe().0.is_empty(), "a ghost plays under the image, frame {frame}");
        assert_eq!(r.scale(112), Some(RowStyle::HOME.focus_scale), "the seat stays lifted through the landing, frame {frame}");
    }
    drop(held);
    r.sect.dormant(false);
    r.run(2);
    let n = r.src.elems.len();
    assert_eq!(r.sect.landing_probe(n, 13), (0.0, 1.0));
    assert_eq!(r.sect.landing_probe(n, 10), (0.0, 1.0));
    assert!(r.sect.ghost_probe().0.is_empty());
}

/// `cull_margin` keeps painting and registering a card whose left edge is just past the screen's.
#[test]
fn a_cull_margin_keeps_a_card_just_off_the_left_edge() {
    let mut plain = settled::<Shelf>(60);
    let mut margined = settled::<Shelf>(60);
    margined.sect = Shelf::new(ENTRY, &RowStyle::HOME).cull_margin(400.0);
    for r in [&mut plain, &mut margined] {
        r.land_focus(130, By::Restore);
        r.run(300);
    }
    let (a, b) = (plain.stops(1.0).len(), margined.stops(1.0).len());
    assert!(b > a, "a 400 px margin registers cards the plain shelf culls: {a} vs {b}");
}

/// `paint` draws exactly what `draw` does without registering stops.
#[test]
fn paint_draws_the_cards_and_registers_no_stops() {
    let mut r = settled::<Shelf>(8);
    r.land_focus(101, By::Restore);
    r.run(2);
    let cx = r.cx();
    let f = DrawFrame::new(&cx, Painter::recording());
    r.src.drawn.borrow_mut().clear();
    let p = f.painter;
    r.sect.paint(&f, p, &r.src, SHELF_AT);
    assert!(!r.src.drawn.borrow().is_empty());
    assert!(f.stops().is_empty());
}

/// An owner whose column count changes at run time (the Library's episode listing) hands the grid
/// the new columns; the cells follow and the focused tile's pop and caption band carry over
/// instead of being corrupted or restarted.
#[test]
fn set_columns_regrids_the_cells_and_keeps_the_pop_and_bands() {
    let mut r = settled::<ExtGrid>(600);
    r.land_focus(105, By::Dir);
    r.run(240);
    let canon = |r: &Rig<ExtGrid>| { let mut c = Canon::new(); r.sect.write(&mut c); c.finish() };
    let before = canon(&r);
    let pop = r.sect.scale_of(&r.cx(), &r.src, 105).unwrap();
    assert!(pop > 1.0, "the focused tile is lifted");
    let narrow = RowStyle { w: 150.0, h: 225.0, gap: 20.0, ..EXT_STYLE };
    r.sect.grid.set_columns(6, narrow, 100.0);
    assert_eq!(canon(&r), before, "the springs are untouched by a geometry change");
    assert_eq!(r.sect.scale_of(&r.cx(), &r.src, 105), Some(pop));
    let cell = |r: &Rig<ExtGrid>, e: u32| r.sect.place(&r.cx(), &r.src, e, At::SpringTarget).unwrap().rest_rect;
    let (c0, c1, c6) = (cell(&r, 100), cell(&r, 101), cell(&r, 106));
    assert!((c1.cx() - c0.cx() - (narrow.w + narrow.gap)).abs() < 0.01, "columns are the new width apart");
    assert!((c6.cx() - c0.cx()).abs() < 0.01, "the seventh card wraps to column 0 of the next row");
    assert!(matches!(r.sect.grid.neighbour(&r.src, r.key(100), Dir::Down), Step::Move(k) if k == r.key(106)));
    match r.sect.grid.group_spec(plx_machine::machine::GroupId(1), 600).kind {
        crate::screen::GroupKind::Grid { cols, .. } => assert_eq!(cols, 6),
        _ => panic!("a grid registers a Grid group"),
    }
}

/// `settle_band` opens a row's caption band at once; it never cuts short a band `on` is opening,
/// and `reset_bands` closes them.
#[test]
fn settle_band_opens_settled_and_reset_bands_closes() {
    let mut r = settled::<ExtGrid>(80);
    r.sect.grid.settle_band(Some(3));
    let open = |r: &Rig<ExtGrid>, row: usize| r.sect.grid.band_geometry().iter().find(|b| b.row == row).map(|b| b.expansion);
    assert_eq!(open(&r, 3), Some(1.0));
    r.land_focus(100 + 4 * EXT.cols as u32, By::Dir);
    r.run(1);
    let mid = open(&r, 4).unwrap();
    assert!(mid > 0.0 && mid < 1.0, "a deliberate move opens its band over frames: {mid}");
    r.sect.grid.settle_band(Some(4));
    assert_eq!(open(&r, 4), Some(mid), "settling the row already focused changes nothing");
    r.sect.grid.reset_bands();
    assert!(r.sect.grid.band_geometry().iter().all(|b| b.row == usize::MAX), "no band survives a reset");
}

/// A source whose card 1 has not loaded.
struct Holey(Cards);
impl CardSource<FixtureHost> for Holey {
    fn len(&self) -> usize { self.0.len() }
    fn elem(&self, i: usize) -> u32 { self.0.elem(i) }
    fn index_of(&self, e: &u32) -> Option<usize> { self.0.index_of(e) }
    fn art(&self, i: usize) -> Art<'_> { self.0.art(i) }
    fn label(&self, i: usize) -> TileLabel { self.0.label(i) }
    fn overlay(&self, p: Painter, i: usize, tile: &super::Tile, m: &dyn plx_machine::machine::Measure) {
        self.0.overlay(p, i, tile, m)
    }
    fn loaded(&self, i: usize) -> bool { i != 1 }
}

/// A card the source has not loaded is not painted, but its stop still registers: the Library's
/// listing is paged, and a slot whose page has not landed draws nothing.
#[test]
fn an_unloaded_card_is_not_painted_but_keeps_its_stop() {
    let r = settled::<ExtGrid>(8);
    let src = Holey(Cards::new(r.src.elems.clone(), false));
    let cx = r.cx();
    let mut f = DrawFrame::new(&cx, Painter::recording());
    r.sect.grid.draw(&mut f, Painter::recording(), &src);
    let painted: Vec<u32> = src.0.drawn.borrow().iter().map(|&(e, _)| e).collect();
    assert!(painted.contains(&100) && !painted.contains(&101), "card 1 is skipped: {painted:?}");
    let mut f = DrawFrame::new(&cx, Painter::root());
    r.sect.grid.record_stops(&mut f, Painter::root(), &src);
    assert!(f.stops().iter().any(|s| s.key.elem == 101), "…but its stop is registered");
}


/// The same for a shelf: a card the source has not loaded (a hub item not published yet) is not
/// painted, its stop still registers, and the focused one is skipped too.
#[test]
fn a_shelf_does_not_paint_an_unloaded_card() {
    let mut r = settled::<Shelf>(8);
    r.land_focus(101, By::Restore);
    r.run(60);
    let src = Holey(Cards::new(r.src.elems.clone(), false));
    let cx = r.cx();
    let mut f = DrawFrame::new(&cx, Painter::recording());
    Shelf::draw(&r.sect, &mut f, Painter::recording(), &src, SHELF_AT);
    let painted: Vec<u32> = src.0.drawn.borrow().iter().map(|&(e, _)| e).collect();
    assert!(painted.contains(&100) && !painted.contains(&101), "card 1 (focused, unloaded) is skipped: {painted:?}");
    let mut f = DrawFrame::new(&cx, Painter::root());
    Shelf::record_stops(&r.sect, &mut f, Painter::root(), &src, SHELF_AT);
    assert!(f.stops().iter().any(|s| s.key.elem == 101), "…but its stop is registered");
}

/// In external mode the grid's canon omits the scroll fields (the owner writes its own) and the
/// owner can write the bands and the pop on their own, in its own order.
#[test]
fn an_external_grid_writes_no_scroll_of_its_own() {
    let mut r = settled::<ExtGrid>(40);
    r.land_focus(105, By::Dir);
    r.run(3);
    let mut whole = Canon::new();
    r.sect.grid.write(&mut whole);
    let mut parts = Canon::new();
    r.sect.grid.write_bands(&mut parts);
    r.sect.grid.write_pop(&mut parts);
    assert_eq!(whole.finish(), parts.finish(), "bands then pop, and no scroll");
    let mut own = Canon::new();
    Grid::new(ENTRY, GridSpec::new(520.0, 96.0)).write(&mut own);
    let mut ext = Canon::new();
    Grid::new(ENTRY, GridSpec::new(520.0, 96.0).external()).write(&mut ext);
    assert_ne!(own.finish(), ext.finish(), "a self-scrolling grid does write its scroll");
}

// ---- Stack (L2) ----------------------------------------------------------------------------

mod stack {
    use super::*;
    use crate::cards::{Kind, SectionSpec, Stack, StackEvent, StackPage};
    use crate::card_row;
    use crate::screen::Focusable;
    use plx_machine::machine::GroupId;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Sec { Head, Aside, Row, Items, Status }

    const HEAD_ELEM: u32 = 1;
    const STATUS_ELEM: u32 = 2;
    const ASIDE_ELEM: u32 = 3;
    const HEAD_H: f32 = 400.0;
    const ROW_STYLE: &RowStyle = &card_row::RowStyle::HOME;
    const COLS_SPEC: GridSpec = GridSpec::new(0.0, 96.0);
    const RESTING_SPEC: GridSpec = GridSpec::resting(0.0);

    /// A page of a header, a shelf, a grid and a Retry-like overlay, over `Cards`.
    struct Page {
        row: Cards,
        items: Cards,
        revision: u64,
        status: bool,
        pending: bool,
        /// A zero-height focusable that reveals the header's block (`reveal_with`).
        aside: bool,
        /// A header tall enough that the shelf below it needs the page scrolled to be on screen.
        tall: bool,
        /// The grid's rows REST ([`GridSpec::resting`]) instead of snapping to an edge.
        rest: bool,
    }

    impl CardSource<FixtureHost> for &Cards {
        fn len(&self) -> usize { self.elems.len() }
        fn elem(&self, i: usize) -> u32 { self.elems[i] }
        fn index_of(&self, e: &u32) -> Option<usize> { self.elems.iter().position(|x| x == e) }
        fn art(&self, _i: usize) -> Art<'_> { Art::Poster(None) }
        fn label(&self, _i: usize) -> TileLabel { TileLabel::default() }
        fn more(&self) -> bool { self.more }
    }

    impl StackPage<FixtureHost> for Page {
        type Key = Sec;
        type Cards<'a> = &'a Cards;
        fn revision(&self, _cx: &Cx9<'_>) -> u64 { self.revision }
        fn pending(&self, _cx: &Cx9<'_>, _want: &u32) -> bool { self.pending }
        fn sections(&self, _cx: &Cx9<'_>, out: &mut Vec<SectionSpec<Sec>>) {
            // the page's own ids, none of them a position; the grid is listed first, as a page
            // whose first-press seat is its members would have it
            out.push(SectionSpec::new(Sec::Head, Kind::Custom { height: if self.tall { 700.0 } else { HEAD_H }, focusable: true }, GroupId(7)).ranked(1));
            if self.aside {
                out.push(SectionSpec::new(Sec::Aside, Kind::Custom { height: 0.0, focusable: true }, GroupId(6)).ranked(1));
            }
            out.push(SectionSpec::new(Sec::Row, Kind::Shelf { style: ROW_STYLE, heading: 60.0 }, GroupId(3)).ranked(1));
            out.push(SectionSpec::new(Sec::Items, Kind::Grid { spec: if self.rest { RESTING_SPEC } else { COLS_SPEC } }, GroupId(0)));
            if self.status {
                out.push(SectionSpec::new(Sec::Status, Kind::Overlay { rect: Rect::new(96.0, 460.0, 400.0, 80.0), focusable: true }, GroupId(9)).ranked(2));
            }
        }
        fn fallback(&self, _cx: &Cx9<'_>, out: &mut Vec<Sec>) { out.extend([Sec::Items, Sec::Row, Sec::Status, Sec::Head]) }
        fn cards<'a>(&'a self, _cx: &'a Cx9<'_>, k: Sec) -> Option<&'a Cards> {
            match k { Sec::Row => Some(&self.row), Sec::Items => Some(&self.items), _ => None }
        }
        fn elem_of(&self, k: Sec) -> Option<u32> {
            match k { Sec::Head => Some(HEAD_ELEM), Sec::Aside => Some(ASIDE_ELEM), Sec::Status => Some(STATUS_ELEM), _ => None }
        }
        fn reveal_with(&self, k: Sec) -> Option<Sec> { (k == Sec::Aside).then_some(Sec::Head) }
        fn focus_rect(&self, _cx: &Cx9<'_>, k: Sec, section: Rect) -> Rect {
            if k == Sec::Head { Rect::new(360.0, section.y + 96.0, 1344.0, 300.0) } else { section }
        }
    }

    /// A `Stack` over a page `P`, fed events and ticks with the engine's focus kept in step.
    struct PageRig<K, P> {
        stack: Stack<K>,
        page: P,
        view: FixtureView,
        focus: Option<FocusKey<u32>>,
        ms: u32,
    }

    type Rig = PageRig<Sec, Page>;
    type Mrig = PageRig<Msec, Multi>;

    fn rig(row: usize, items: usize) -> Rig {
        rig_with(row, items, false)
    }

    /// [`rig`] with the grid's spec chosen up front: a grid keeps the spec it was built with.
    fn rig_with(row: usize, items: usize, rest: bool) -> Rig {
        let ids = |base: u32, n: usize| (0..n as u32).map(|i| base + i).collect::<Vec<_>>();
        let mut r = PageRig {
            stack: Stack::new(ENTRY),
            page: Page { row: Cards::new(ids(100, row), false), items: Cards::new(ids(1000, items), false), revision: 1, status: false, pending: false, aside: false, tall: false, rest },
            view: FixtureView::default(),
            focus: None,
            ms: 0,
        };
        r.run(1);
        r
    }

    impl<K: Copy + Eq, P: StackPage<FixtureHost, Key = K>> PageRig<K, P> {
        fn cx(&self) -> Cx9<'_> {
            cx9(&self.view, self.ms, 1.0, self.focus)
        }
        fn feed(&mut self, ev: ScreenEvent<FixtureHost>) -> (Option<StackEvent<K, u32>>, bool) {
            let mut present = Present::new();
            let mut out = Vec::new();
            let mut got = None;
            let (_, moving) = plx_machine::idle::scoped_motion(|| {
                let cx = cx9(&self.view, self.ms, 1.0, self.focus);
                let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(9)), &mut present);
                got = self.stack.on(&self.page, &ev, &cx, &mut fx);
            });
            (got, moving || present.page_moving())
        }
        fn go(&mut self, elem: u32, by: By) {
            let from = self.focus;
            let to = FocusKey { entry: ENTRY, elem };
            self.focus = Some(to);
            self.feed(ScreenEvent::FocusMoved { from, to, by });
        }
        fn run(&mut self, frames: u32) -> bool {
            let mut moved = false;
            for _ in 0..frames {
                self.ms += MS;
                moved |= self.feed(ScreenEvent::Tick(Tick { ms: self.ms, dt_us: 16_667 })).1;
            }
            moved
        }
        fn place(&self, elem: u32) -> Option<Placed> {
            self.stack.view(&self.page).place(&elem, &self.cx(), At::Drawn)
        }
        fn reconcile(&self, want: u32) -> u32 {
            let key = FocusKey { entry: ENTRY, elem: want };
            self.stack.view(&self.page).reconcile(key, &self.cx()).elem
        }
        fn groups(&self) -> Vec<GroupSpec> {
            let mut out = Vec::new();
            self.stack.view(&self.page).groups(&self.cx(), &mut out);
            out
        }
        fn step(&self, elem: u32, dir: Dir) -> Step<u32> {
            self.stack.view(&self.page).neighbour(FocusKey { entry: ENTRY, elem }, dir, &self.cx())
        }
    }

    /// A page of a header and a RESTING grid of `items` cards (no shelf, so the first row stands at
    /// the header's foot, y 400, when the page is at the top).
    fn resting_rig(items: usize) -> Rig {
        rig_with(0, items, true)
    }

    /// Where the poster of `elem` stands on screen now: the drawn rect of a card with no focus pop.
    fn drawn_y(r: &Rig, elem: u32) -> f32 {
        r.place(elem).expect("a placed card").rect.y
    }

    /// **A resting grid puts the focused row where the first row stands** (the Library's rule,
    /// `poster_grid::rest_scroll`): focus a card of row `n` and the page rests with that row's
    /// posters at the y the first row had at scroll 0, whatever the row. Measured on the row's
    /// FIRST card while focus is on its second (the focus pop raises the focused card's rect). A
    /// default grid snaps to its edge instead (its first snap puts row 0 on the edge, so row 1
    /// stands a pitch under it), which the same walk pins as unchanged.
    #[test]
    fn a_resting_grid_rests_every_focused_row_where_the_first_row_stands() {
        let mut r = resting_rig(60);
        let first = drawn_y(&r, 1000);
        assert_eq!(first, HEAD_H, "premise: the first row stands at the header's foot");
        for row in [1u32, 2, 4, 7] {
            r.go(1000 + row * 6 + 1, By::Dir);
            r.run(300);
            let y = drawn_y(&r, 1000 + row * 6);
            assert!((y - first).abs() < 0.5, "row {row} rests at {first}, not {y} (scroll {})", r.stack.scroll());
        }
        r.go(1001, By::Dir);
        r.run(300);
        assert!(r.stack.scroll().abs() < 0.5, "row 0 rests at the head: {}", r.stack.scroll());

        let mut snapping = rig(0, 60);
        snapping.go(1007, By::Dir);
        snapping.run(300);
        let pitch = RowStyle::HOME.h + card_row::LABEL_BAND_COLLAPSED + crate::consts::UNDER_LABEL_AIR;
        assert!((drawn_y(&snapping, 1006) - (96.0 + pitch)).abs() < 0.5,
            "a default grid is unchanged: it rests on its first snap (row 0 on the edge, y 96), so row 1 stands a pitch under it: {}",
            drawn_y(&snapping, 1006));
    }

    /// **The document's end holds the last rows lower**: a row whose rest would scroll past the end
    /// of the page stands lower than the line, with its posters whole above the foot margin, and
    /// the scroll is the page's own furthest (`Stack::max_scroll`, measured at the settled bands).
    #[test]
    fn a_resting_grid_is_held_by_the_documents_end() {
        let mut r = resting_rig(60);
        let first = drawn_y(&r, 1000);
        let last_row = 9u32;
        r.go(1000 + last_row * 6 + 1, By::Dir);
        r.run(300);
        let y = drawn_y(&r, 1000 + last_row * 6);
        let pitch = RowStyle::HOME.h + card_row::LABEL_BAND_COLLAPSED + crate::consts::UNDER_LABEL_AIR;
        assert!(r.stack.scroll() < last_row as f32 * pitch, "the end holds the scroll below the unclamped rest: {}", r.stack.scroll());
        assert!(y > first + 0.5, "so the last row stands lower than the first row's line: {y}");
        assert!((r.stack.scroll() - r.stack.max_scroll(&r.page, &r.cx())).abs() < 0.5, "at the page's furthest scroll");
        assert!(y + RowStyle::HOME.h <= crate::consts::SCR_H - crate::consts::MARGIN_Y + 0.5, "with its posters above the foot margin");
    }

    /// **The end is measured against the SETTLED caption band**, not the one still opening: the
    /// target a focus move names on the first frame is the one the page comes to rest at, so the
    /// clamp does not chase the band for as long as it animates.
    #[test]
    fn a_resting_grids_target_is_the_destination_from_the_first_frame() {
        let mut r = resting_rig(60);
        r.go(1000 + 9 * 6 + 1, By::Dir);
        r.run(1);
        let first = r.stack.target();
        r.run(300);
        assert!(first > 0.0, "setup: the last row wants a scroll: {first}");
        assert_eq!(r.stack.target(), first, "the end was the settled one from the first frame");
        assert!((r.stack.scroll() - first).abs() < 0.5);
    }

    /// **The row above the rested one stays drawn**, under the page's chrome, as on the Library: a
    /// resting grid culls only what is wholly off the screen's top (a snapping grid culls the row
    /// over the edge).
    #[test]
    fn a_resting_grid_keeps_the_row_above_drawn() {
        let mut r = resting_rig(60);
        r.go(1000 + 4 * 6 + 1, By::Dir);
        r.run(300);
        let painted = r.stack.grid(Sec::Items).expect("the grid").painted(Painter::recording(), 60);
        assert!(painted.contains(&(3 * 6)), "the row above (cards 18..24) is painted: {painted:?}");
    }

    #[test]
    fn a_section_that_names_a_block_reveals_that_block_not_its_own_sliver() {
        let mut r = rig(6, 60);
        r.page.aside = true;
        r.page.tall = true;
        r.page.revision += 1;
        r.run(1);
        r.go(100, By::Restore);
        r.run(2);
        r.go(1030, By::Dir);
        r.run(120);
        assert!(r.stack.scroll() > 100.0, "setup: the page is scrolled down: {}", r.stack.scroll());
        r.go(ASIDE_ELEM, By::Dir);
        r.run(240);
        assert!(r.stack.scroll().abs() < 0.5, "the aside sits in the header's block: the page goes home, not to the aside's own top: {}", r.stack.scroll());
    }

    #[test]
    fn the_scroll_target_is_measured_against_the_settled_caption_bands() {
        let mut r = rig(6, 24);
        r.page.tall = true;
        r.page.revision += 1;
        r.run(1);
        r.go(100, By::Dir);
        r.run(1);
        let first = r.stack.target();
        r.run(240);
        assert!(first > 0.0, "setup: the shelf below a tall header needs the page scrolled: {first}");
        assert_eq!(r.stack.target(), first, "the target was the destination from the first frame, not a chase of the opening band");
        assert!((r.stack.scroll() - first).abs() < 0.5);
    }

    #[test]
    fn sections_run_again_only_when_the_revision_moves() {
        let mut r = rig(6, 24);
        let built = r.stack.rebuilds;
        r.run(5);
        r.go(1005, By::Dir);
        r.run(20);
        assert_eq!(r.stack.rebuilds, built, "ticks, focus moves and settling never rebuild the layout");
        r.page.revision += 1;
        r.run(1);
        assert_eq!(r.stack.rebuilds, built + 1, "a content revision rebuilds it once");
    }

    #[test]
    fn the_layout_stacks_header_shelf_and_grid_in_document_order() {
        let r = rig(6, 24);
        let view = r.stack.view(&r.page);
        let head = view.place(&HEAD_ELEM, &r.cx(), At::Drawn).unwrap().rect;
        let row = view.place(&100, &r.cx(), At::Drawn).unwrap().rect;
        let grid = view.place(&1000, &r.cx(), At::Drawn).unwrap().rect;
        assert_eq!(head.y, 96.0, "the page's own focus rect: the section top plus its inset");
        assert_eq!(row.y, HEAD_H + 60.0, "a shelf's tiles sit under the header and its heading");
        assert!(grid.y > row.y + ROW_STYLE.h, "the grid starts after the shelf: {grid:?} vs {row:?}");
        let mut groups = Vec::new();
        view.groups(&r.cx(), &mut groups);
        assert_eq!(groups.len(), 3, "header, shelf, grid");
        assert_eq!(r.stack.group(Sec::Items), Some(GroupId(0)));
    }

    /// A section's group id is the page's own and is listed in the page's rank: not its position
    /// in the document, and not shifted by which other sections exist.
    #[test]
    fn group_ids_and_their_order_are_the_pages_not_positions() {
        let mut r = rig(6, 24);
        let ids = |r: &Rig| {
            let mut groups = Vec::new();
            r.stack.view(&r.page).groups(&r.cx(), &mut groups);
            groups.iter().map(|g| g.id.0).collect::<Vec<_>>()
        };
        assert_eq!(ids(&r), [0, 7, 3], "grid first (rank), then header, then shelf");
        assert_eq!((r.stack.group(Sec::Head), r.stack.group(Sec::Row)), (Some(GroupId(7)), Some(GroupId(3))));
        r.page.aside = true;
        r.page.status = true;
        r.page.revision += 1;
        r.run(1);
        assert_eq!(ids(&r), [0, 7, 6, 3, 9]);
        assert_eq!((r.stack.group(Sec::Head), r.stack.group(Sec::Row), r.stack.group(Sec::Items)),
            (Some(GroupId(7)), Some(GroupId(3)), Some(GroupId(0))), "a section keeps its id as others come and go");
        let view = r.stack.view(&r.page);
        let cx = r.cx();
        let key = |g: u32| view.seat(GroupId(g), crate::screen::Placed { rect: Rect::FULL, rest_rect: Rect::FULL, clip: Rect::FULL, index: None }, &cx).elem;
        assert!((1000..1024).contains(&key(0)) && (100..106).contains(&key(3)), "a card of the section the id names");
        assert_eq!((key(7), key(9)), (HEAD_ELEM, STATUS_ELEM), "seat finds the section by its id");
        assert_eq!(view.group_of(&1000, &cx), Some(GroupId(0)));
        assert_eq!(view.group_of(&STATUS_ELEM, &cx), Some(GroupId(9)));
    }

    /// A group the page no longer lists degrades to the page's fallback; it never panics.
    #[test]
    fn seating_into_an_unlisted_group_degrades_to_the_fallback() {
        let r = rig(6, 24);
        let view = r.stack.view(&r.page);
        let from = crate::screen::Placed { rect: Rect::FULL, rest_rect: Rect::FULL, clip: Rect::FULL, index: None };
        assert_eq!(view.seat(GroupId(42), from, &r.cx()).elem, 1000, "the first of the fallback order");
        let mut empty = rig(0, 0);
        empty.page.pending = false;
        let view = empty.stack.view(&empty.page);
        assert_eq!(view.seat(GroupId(3), from, &empty.cx()).elem, HEAD_ELEM, "an emptied shelf's group: the header");
    }

    /// Two paging sections ask in one tick: both asks reach the page on a later tick, whatever
    /// other events arrive between (the page reads a `Want` only on a tick).
    #[test]
    fn a_second_paging_section_is_not_dropped_by_an_event_that_is_not_a_tick() {
        let mut r = rig(3, 3);
        r.page.row.more = true;
        r.page.items.more = true;
        let tick = |r: &mut Rig| { r.ms += MS; r.feed(ScreenEvent::Tick(Tick { ms: r.ms, dt_us: 16_667 })).0 };
        let mut seen = Vec::new();
        for _ in 0..4 {
            if let Some(StackEvent::Card(k, CardEvent::Want(_))) = tick(&mut r) { seen.push(k); }
            // a non-tick event between ticks must neither deliver nor lose a queued ask
            let (got, _) = r.feed(ScreenEvent::Cover);
            assert!(got.is_none(), "a Want is only ever reported on a tick: {got:?}");
        }
        assert!(seen.contains(&Sec::Row) && seen.contains(&Sec::Items), "both sections' asks arrive: {seen:?}");
    }

    #[test]
    fn focus_on_a_deep_grid_row_scrolls_the_page_and_the_header_scrolls_it_home() {
        let mut r = rig(6, 60);
        r.go(1000, By::Restore);
        r.run(2);
        r.go(1030, By::Dir);
        r.run(120);
        assert!(r.stack.scroll() > 100.0, "the page followed the focused row: {}", r.stack.scroll());
        let at = r.place(1030).unwrap().rect;
        assert!(at.y >= 0.0 && at.y + at.h <= SCR_H_F, "and the row is on screen: {at:?}");
        r.go(HEAD_ELEM, By::Dir);
        r.run(240);
        assert!(r.stack.scroll().abs() < 0.5, "focus on the header brings the page home: {}", r.stack.scroll());
    }

    /// The pointer's sideways glide (`InputKind::Pan`) moves the stack's shelf under the pointer,
    /// and only that shelf; a `Pan` over the grid below moves nothing.
    #[test]
    fn a_sideways_glide_moves_the_shelf_under_the_pointer() {
        let pan = |y: f32| ScreenEvent::Input(plx_machine::machine::InputEvent {
            at: Default::default(),
            source: plx_machine::machine::Source::Script,
            kind: plx_machine::machine::InputKind::Pan { dx: 30.0, x: 1900.0, y },
        });
        let mut r = rig(24, 60);
        r.go(100, By::Restore);
        r.run(2);
        let before = r.place(100).unwrap().rect;
        let grid_card = r.place(1000).unwrap().rect;
        for _ in 0..10 { r.feed(pan(before.cy())); }
        r.run(60);
        let after = r.place(100).unwrap().rect;
        assert!((before.x - after.x - 300.0).abs() < 0.5, "the shelf under the pointer glides 300 px: {before:?} -> {after:?}");
        r.feed(pan(grid_card.cy()));
        assert_eq!(r.place(1000).unwrap().rect.x, grid_card.x, "the grid does not move sideways");
    }

    /// The pointer's edge glide (a fractional wheel every frame) moves a stack page itself: the
    /// page follows the wheel and stays where it put it instead of the tick pulling it back to the
    /// focused row; a hover moves focus and not the page, and a key's move brings it back.
    #[test]
    fn a_wheel_glides_the_page_and_it_stays_until_focus_moves() {
        let wheel = |dy: f32| ScreenEvent::Input(plx_machine::machine::InputEvent {
            at: Default::default(),
            source: plx_machine::machine::Source::Script,
            kind: plx_machine::machine::InputKind::Wheel { dy },
        });
        let mut r = rig(6, 60);
        r.go(1000, By::Restore);
        r.run(2);
        r.go(1042, By::Dir);
        r.run(120);
        let deep = r.stack.scroll();
        assert!(deep > 400.0, "the page followed the focused row: {deep}");
        let mut last = deep;
        for _ in 0..30 {
            r.feed(wheel(0.04));
            r.run(1);
            assert!(r.stack.scroll() < last, "every frame of the glide moves the page up");
            last = r.stack.scroll();
        }
        r.run(120);
        let glided = r.stack.scroll();
        assert!((glided - (deep - 1.2 * crate::consts::ROW_PITCH)).abs() < 1.0, "1.2 rows up: {deep} -> {glided}");
        r.go(1043, By::Pointer);
        r.run(120);
        assert!((r.stack.scroll() - glided).abs() < 0.5, "a hover moves focus, not the page: {}", r.stack.scroll());
        r.go(1042, By::Dir);
        r.run(120);
        assert!((r.stack.scroll() - deep).abs() < 1.0, "a key's move reveals the focused row again: {}", r.stack.scroll());
    }

    #[test]
    fn a_page_stays_put_when_focus_leaves_it_unless_it_asks_to_go_home() {
        let scrolled = |home: bool| {
            let mut r = rig(6, 60);
            r.stack = Stack::new(ENTRY).home_when_unfocused(home);
            r.run(1);
            r.go(1000, By::Restore);
            r.run(2);
            r.go(1030, By::Dir);
            r.run(120);
            let deep = r.stack.scroll();
            assert!(deep > 100.0, "the page followed the focused row: {deep}");
            r.focus = None;
            r.run(240);
            (deep, r.stack.scroll(), r)
        };
        let (deep, after, _) = scrolled(false);
        assert!((after - deep).abs() < 0.5, "by default the page stays where it was: {deep} -> {after}");
        let (_, after, mut r) = scrolled(true);
        assert!(after.abs() < 0.5, "with home_when_unfocused the page scrolls home: {after}");
        r.page.status = true;
        r.page.revision += 1;
        r.run(1);
        r.go(1030, By::Dir);
        r.run(120);
        assert!(r.stack.scroll() > 100.0);
        r.go(STATUS_ELEM, By::Dir);
        r.run(240);
        assert!(r.stack.scroll().abs() < 0.5, "an out-of-flow overlay held counts as unfocused: {}", r.stack.scroll());
    }

    const SCR_H_F: f32 = crate::consts::SCR_H;

    #[test]
    fn stops_are_the_rects_placement_answers_at_any_scroll() {
        let mut r = rig(6, 60);
        r.go(1000, By::Restore);
        r.go(1031, By::Dir);
        r.run(60);
        let cx = r.cx();
        let mut f = DrawFrame::new(&cx, Painter::root());
        r.stack.view(&r.page).record_stops(&mut f);
        let stops = f.stops().to_vec();
        assert!(!stops.is_empty());
        for s in stops {
            let placed = r.place(s.key.elem).unwrap().rect;
            assert!((placed.y - s.rect.y).abs() < 0.01 && (placed.x - s.rect.x).abs() < 0.01, "{}: {placed:?} vs {:?}", s.key.elem, s.rect);
        }
    }

    #[test]
    fn events_press_hold_and_paging_come_back_keyed_by_section() {
        let mut r = rig(6, 20);
        r.page.items.more = true;
        r.go(1002, By::Dir);
        assert_eq!(r.feed(ScreenEvent::PressCommit(PressId(1))).0, Some(StackEvent::Card(Sec::Items, CardEvent::Activate(1002))));
        assert_eq!(r.feed(ScreenEvent::PressHold(PressId(1))).0, Some(StackEvent::Card(Sec::Items, CardEvent::Hold(1002))));
        r.go(102, By::Dir);
        assert_eq!(r.feed(ScreenEvent::PressCommit(PressId(1))).0, Some(StackEvent::Card(Sec::Row, CardEvent::Activate(102))));
        assert_eq!(r.feed(ScreenEvent::Activate(HEAD_ELEM)).0, Some(StackEvent::Press(Sec::Head)));
        r.go(1015, By::Dir);
        let want = (0..3).find_map(|_| match r.feed(ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 })).0 {
            Some(StackEvent::Card(Sec::Items, CardEvent::Want(range))) => Some(range),
            _ => None,
        });
        assert!(want.is_some_and(|w| w.start == 20), "focus near the tail asks for more cards");
    }

    #[test]
    fn reconcile_keeps_identity_then_clamps_in_the_section_then_falls_back() {
        let mut r = rig(6, 12);
        r.go(1005, By::Dir);
        r.run(2);
        assert_eq!(r.reconcile(1005), 1005, "a shown element is kept");
        r.page.items.elems.retain(|&e| e != 1005);
        r.page.revision += 1;
        r.run(1);
        assert_eq!(r.reconcile(1005), 1006, "the same position in the same section");
        r.page.items.elems.truncate(5);
        assert_eq!(r.reconcile(1005), 1004, "clamped when the section got shorter");
        r.page.items.elems.clear();
        r.page.revision += 1;
        r.run(1);
        assert_eq!(r.reconcile(1005), 100, "an emptied section falls back to the page's order");
        r.page.pending = true;
        assert_eq!(r.reconcile(1005), 1005, "a page still loading keeps the wanted focus");
    }

    #[test]
    fn a_landing_above_the_focus_shifts_the_page_so_the_tile_stays_put() {
        let mut r = rig(6, 60);
        r.go(1000, By::Restore);
        r.go(1020, By::Dir);
        r.run(240);
        let was = r.place(1020).unwrap().rect;
        r.page.items.elems.splice(0..0, (0..6).map(|i| 900 + i));
        r.page.revision += 1;
        r.run(1);
        let is = r.place(1020).unwrap().rect;
        assert!((is.y - was.y).abs() < 0.5, "a row inserted above the focus moves nothing on screen: {was:?} -> {is:?}");
    }

    #[test]
    fn a_settled_page_is_quiet_and_its_canon_repeats() {
        let mut r = rig(6, 60);
        r.go(1000, By::Restore);
        r.go(1020, By::Dir);
        assert!(r.run(10), "a moving page reports motion");
        r.run(400);
        assert!(!r.run(3), "a settled one reports none");
        let canon = |r: &Rig| { let mut c = Canon::new(); r.stack.write(&mut c); c.finish() };
        let first = canon(&r);
        r.run(5);
        assert_eq!(canon(&r), first, "and the same events leave the same bytes");
    }

    #[test]
    fn memory_restores_the_scroll_and_the_shelf_viewport() {
        let mut r = rig(40, 60);
        r.go(1000, By::Restore);
        r.go(1030, By::Dir);
        r.run(200);
        r.go(139, By::Dir);
        r.run(200);
        let m = r.stack.memory();
        let mut fresh = rig(40, 60);
        fresh.stack.restore(&m);
        fresh.go(139, By::Restore);
        fresh.run(1);
        assert_eq!(fresh.stack.memory(), m, "scroll and shelf viewport come back whole");
        assert!(fresh.place(139).is_some_and(|p| p.rect.x >= 0.0 && p.rect.x < crate::consts::SCR_W));
    }

    // ---- the extensions a page that owns its vertical model asks for ---------------------------

    use crate::screen::{AxisMask, ElemKind, EdgeRule, GroupKind, GroupSpec, Seat};

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Msec { Field, List, Clear, Shelf, More(u8) }

    const FIELD_ELEM: u32 = 1;
    const CLEAR_ELEM: u32 = 2;
    const LIST_BASE: u32 = 50;
    const FIELD_H: f32 = 300.0;
    const ROW_H: f32 = 72.0;
    const SHELF_MARGIN: f32 = 600.0;
    const FLOOR: Rect = Rect { x: 0.0, y: 90.0, w: crate::consts::SCR_W, h: SCR_H_F - 90.0 };

    /// A field, a list of `rows` focusable rows (one section, one column group), a Clear button in
    /// a zero-height section after it, and a shelf: the shape of a page whose own list lives in a
    /// `Custom` and whose rows are seated and linked by the page.
    struct Multi {
        rows: u32,
        shelf: Cards,
        revision: u64,
        foot: f32,
        /// Further shelves after `Shelf`, each at the default reveal margin.
        more: Vec<Cards>,
        /// What `recover` answers (`None`: the trait's default).
        recover_to: Option<u32>,
        /// `Shelf`'s group extent is its own head tile rather than the page's width.
        narrow: bool,
    }

    impl StackPage<FixtureHost> for Multi {
        type Key = Msec;
        type Cards<'a> = &'a Cards;
        fn revision(&self, _cx: &Cx9<'_>) -> u64 { self.revision }
        fn sections(&self, _cx: &Cx9<'_>, out: &mut Vec<SectionSpec<Msec>>) {
            out.push(SectionSpec::new(Msec::Field, Kind::Custom { height: FIELD_H, focusable: true }, GroupId(10)));
            if self.rows > 0 {
                out.push(SectionSpec::new(Msec::List, Kind::Custom { height: self.rows as f32 * ROW_H, focusable: true }, GroupId(11)));
                out.push(SectionSpec::new(Msec::Clear, Kind::Custom { height: 0.0, focusable: true }, GroupId(12)));
            }
            out.push(
                SectionSpec::new(Msec::Shelf, Kind::Shelf { style: ROW_STYLE, heading: 60.0 }, GroupId(13))
                    .seated(Seat::Projected)
                    .edges([EdgeRule::Geometric, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop])
                    .of_kind(ElemKind::Bare),
            );
            for n in 0..self.more.len() as u8 {
                out.push(SectionSpec::new(Msec::More(n), Kind::Shelf { style: ROW_STYLE, heading: 60.0 }, GroupId(20 + n as u32)));
            }
        }
        fn fallback(&self, _cx: &Cx9<'_>, out: &mut Vec<Msec>) { out.extend([Msec::Field]) }
        fn cards<'a>(&'a self, _cx: &'a Cx9<'_>, k: Msec) -> Option<&'a Cards> {
            match k { Msec::Shelf => Some(&self.shelf), Msec::More(n) => self.more.get(n as usize), _ => None }
        }
        fn recover(&self, _cx: &Cx9<'_>, _want: &u32) -> Option<u32> { self.recover_to }
        fn wide_extent(&self, k: Msec) -> bool { !(self.narrow && k == Msec::Shelf) }
        fn elem_of(&self, k: Msec) -> Option<u32> {
            match k { Msec::Field => Some(FIELD_ELEM), Msec::Clear => Some(CLEAR_ELEM), _ => None }
        }
        fn plain_len(&self, k: Msec) -> usize {
            match k { Msec::List => self.rows as usize, Msec::Field | Msec::Clear => 1, Msec::Shelf | Msec::More(_) => 0 }
        }
        fn plain_elem(&self, k: Msec, n: usize) -> Option<u32> {
            match k { Msec::List => (n < self.rows as usize).then_some(LIST_BASE + n as u32), _ => (n == 0).then(|| self.elem_of(k)).flatten() }
        }
        fn plain_step(&self, k: Msec, at: usize, dir: Dir) -> Option<usize> {
            match (k, dir) {
                (Msec::List, Dir::Up) => at.checked_sub(1),
                (Msec::List, Dir::Down) => Some(at + 1),
                _ => None,
            }
        }
        fn element_rect(&self, _cx: &Cx9<'_>, k: Msec, n: usize, section: Rect) -> Rect {
            match k {
                Msec::Field => Rect::new(96.0, section.y + 138.0, 1000.0, 80.0),
                Msec::List => Rect::new(96.0, section.y + n as f32 * ROW_H, 820.0, ROW_H),
                _ => Rect::new(96.0, section.y, 200.0, 60.0),
            }
        }
        fn plain_group(&self, _cx: &Cx9<'_>, k: Msec, id: GroupId, extent: Rect) -> GroupSpec {
            GroupSpec {
                id,
                kind: if k == Msec::List { GroupKind::Column } else { GroupKind::Free },
                seat: if k == Msec::List { Seat::Remembered } else { Seat::First },
                reachable: AxisMask::BOTH,
                edge: [EdgeRule::Stop; 4],
                extent,
                len: self.plain_len(k),
                elem: ElemKind::Bare,
            }
        }
        fn seat_override(&self, _cx: &Cx9<'_>, k: Msec, _from: Placed) -> Option<u32> {
            // the page's own answer for the list: its LAST row, whatever the section would say
            (k == Msec::List).then(|| LIST_BASE + self.rows - 1)
        }
        fn reveal_with(&self, k: Msec) -> Option<Msec> { matches!(k, Msec::List | Msec::Clear).then_some(Msec::Field) }
        fn reveal_margin(&self, k: Msec) -> f32 { if k == Msec::Shelf { SHELF_MARGIN } else { crate::consts::MARGIN_Y } }
        fn shelf_foot(&self, _cx: &Cx9<'_>, k: Msec) -> f32 { if k == Msec::Shelf { self.foot } else { 0.0 } }
    }

    fn mrig(rows: u32, shelf: usize, build: impl FnOnce(Stack<Msec>) -> Stack<Msec>) -> Mrig {
        let mut r = PageRig {
            stack: build(Stack::new(ENTRY)),
            page: Multi { rows, shelf: Cards::new((0..shelf as u32).map(|i| 100 + i).collect(), false), revision: 1, foot: 0.0, more: Vec::new(), recover_to: None, narrow: false },
            view: FixtureView::default(),
            focus: None,
            ms: 0,
        };
        r.feed(ScreenEvent::Mount);
        r.run(1);
        r
    }

    fn moved(s: Step<u32>) -> Option<u32> {
        match s { Step::Move(k) => Some(k.elem), _ => None }
    }

    #[test]
    fn a_custom_section_holds_several_elements_and_walks_them_by_the_pages_rule() {
        let r = mrig(3, 4, |s| s);
        assert_eq!(r.step(LIST_BASE, Dir::Down).pipe(moved), Some(LIST_BASE + 1));
        assert_eq!(r.step(LIST_BASE + 1, Dir::Down).pipe(moved), Some(LIST_BASE + 2));
        assert_eq!(r.step(LIST_BASE + 2, Dir::Down).pipe(moved), None, "past the last element is the section's edge");
        assert_eq!(r.step(LIST_BASE + 2, Dir::Up).pipe(moved), Some(LIST_BASE + 1));
        assert_eq!(r.step(LIST_BASE, Dir::Up).pipe(moved), None);
        assert_eq!(r.step(LIST_BASE, Dir::Left).pipe(moved), None, "no rule, no step");
        assert_eq!(r.step(FIELD_ELEM, Dir::Down).pipe(moved), None, "a single element keeps the default edge");
        let g = r.groups();
        let list = g.iter().find(|g| g.id == GroupId(11)).unwrap();
        assert!(list.len == 3 && matches!(list.kind, GroupKind::Column));
        assert!(g.iter().any(|g| g.id == GroupId(12)), "the zero-height button is its own group");
    }

    trait Pipe: Sized { fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T { f(self) } }
    impl<T> Pipe for T {}

    #[test]
    fn every_element_of_a_custom_section_is_placed_in_its_own_rect_with_its_index() {
        let r = mrig(3, 4, |s| s);
        for n in 0..3u32 {
            let p = r.place(LIST_BASE + n).unwrap();
            assert_eq!(p.index, Some(n));
            assert_eq!(p.rect, Rect::new(96.0, FIELD_H + n as f32 * ROW_H, 820.0, ROW_H));
            assert_eq!(p.rect, p.rest_rect);
        }
        let clear = r.place(CLEAR_ELEM).unwrap();
        assert_eq!(clear.rect.y, FIELD_H + 3.0 * ROW_H, "the zero-height section sits where the list ends");
        assert!(r.place(LIST_BASE + 3).is_none(), "an element the section does not hold is not placed");
        assert_eq!(r.stack.view(&r.page).group_of(&(LIST_BASE + 2), &r.cx()), Some(GroupId(11)));
    }

    #[test]
    fn a_page_reconciles_a_vanished_list_row_to_the_same_position_clamped() {
        let mut r = mrig(4, 4, |s| s);
        r.go(LIST_BASE + 3, By::Dir);
        r.run(2);
        r.page.rows = 2;
        r.page.revision += 1;
        r.run(1);
        let got = r.stack.view(&r.page).reconcile(FocusKey { entry: ENTRY, elem: LIST_BASE + 3 }, &r.cx());
        assert_eq!(got.elem, LIST_BASE + 1, "the last row that is left, in the section focus was in");
    }

    #[test]
    fn a_section_can_replace_its_groups_seat_edges_and_press_kind() {
        let r = mrig(2, 4, |s| s);
        let g = r.groups();
        let shelf = g.iter().find(|g| g.id == GroupId(13)).unwrap();
        assert_eq!(shelf.seat, Seat::Projected);
        assert_eq!(shelf.edge, [EdgeRule::Geometric, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop]);
        assert_eq!(shelf.elem, ElemKind::Bare);
        let field = g.iter().find(|g| g.id == GroupId(10)).unwrap();
        assert_eq!((field.seat, field.edge), (Seat::First, [EdgeRule::Stop; 4]), "an untouched section keeps its kind's own");
    }

    #[test]
    fn the_page_may_answer_a_seat_before_the_section_does() {
        let r = mrig(3, 4, |s| s);
        let from = r.place(FIELD_ELEM).unwrap();
        let seated = r.stack.view(&r.page).seat(GroupId(11), from, &r.cx());
        assert_eq!(seated.elem, LIST_BASE + 2);
        let shelf = r.stack.view(&r.page).seat(GroupId(13), from, &r.cx());
        assert!((100..104).contains(&shelf.elem), "a section the page has no answer for seats as before");
    }

    #[test]
    fn a_page_that_reveals_on_move_keeps_a_scroll_it_set_itself_until_focus_moves() {
        let mut r = mrig(0, 40, |s| s.reveal_on_move(true));
        r.go(FIELD_ELEM, By::Dir);
        r.run(60);
        assert_eq!(r.stack.scroll(), 0.0);
        r.stack.scroll_to(200.0);
        r.run(120);
        assert!((r.stack.scroll() - 200.0).abs() < 0.5, "ticks do not take a scroll the page set: {}", r.stack.scroll());
        // the field (300) and the shelf's block (heading + tiles + the focused label band) fit one
        // screen: nothing to scroll to, so the move's own recomputation is 0, not the 200 set
        assert_eq!(r.stack.max_scroll(&r.page, &r.cx()), 0.0, "setup: a short page does not scroll");
        r.go(100, By::Dir);
        assert_eq!(r.stack.target(), 0.0, "a move recomputes the target (it was 200)");
        r.run(120);
        assert!(r.stack.scroll().abs() < 0.01, "and the page comes home: {}", r.stack.scroll());
        let mut r = mrig(30, 40, |s| s.reveal_on_move(true));
        let max = r.stack.max_scroll(&r.page, &r.cx());
        let content = FIELD_H + 30.0 * ROW_H + 60.0 + ROW_STYLE.h + card_row::under_band(0.0);
        assert_eq!(max, content - (SCR_H_F - crate::consts::MARGIN_Y), "the content's end at the screen's foot");
        assert!(max > 0.0);
        r.stack.jump_to(max);
        assert_eq!((r.stack.scroll(), r.stack.target()), (max, max));
        r.go(FIELD_ELEM, By::Dir);
        assert_eq!(r.stack.target(), 0.0, "the field's block is the page's top");
        r.focus = Some(FocusKey { entry: EntryId(77), elem: 5 });
        r.stack.scroll_to(50.0);
        r.feed(ScreenEvent::FocusMoved { from: None, to: r.focus.unwrap(), by: By::Dir });
        assert_eq!(r.stack.target(), 0.0, "a move to an element the page does not show sends it home");
    }

    #[test]
    fn a_shelf_is_brought_up_to_the_pages_own_margin() {
        let mut r = mrig(30, 40, |s| s);
        let top = FIELD_H + 30.0 * ROW_H;
        let max = r.stack.max_scroll(&r.page, &r.cx());
        assert!(max > top - SHELF_MARGIN && max < top - crate::consts::MARGIN_Y, "setup: only the page's margin pulls the page up: {max}");
        r.stack.jump_to(max);
        r.go(100, By::Dir);
        r.run(1);
        assert_eq!(r.stack.target(), top - SHELF_MARGIN, "the page's margin, not the default {}", crate::consts::MARGIN_Y);
    }

    #[test]
    fn a_shelfs_foot_is_part_of_its_block_and_of_the_reveal() {
        let at = |foot: f32| {
            let mut r = mrig(30, 40, |s| s);
            r.page.foot = foot;
            r.page.revision += 1;
            r.go(FIELD_ELEM, By::Dir);
            r.run(1);
            r.go(100, By::Dir);
            r.run(1);
            (r.stack.target(), r.stack.max_scroll(&r.page, &r.cx()))
        };
        let top = FIELD_H + 30.0 * ROW_H;
        // focus is on the shelf, so its caption band is open in the settled measure
        let block = |foot: f32| 60.0 + ROW_STYLE.h + card_row::under_band(1.0) + foot;
        let max = |foot: f32| top + block(foot) - (SCR_H_F - crate::consts::MARGIN_Y);
        let reveal = |foot: f32| {
            card_row::reveal(0.0, top + block(foot) - (SCR_H_F - crate::consts::MARGIN_Y), top - SHELF_MARGIN, max(foot))
        };
        let (bare_target, bare_max) = at(0.0);
        let (footed_target, footed_max) = at(22.0);
        assert_eq!(bare_max, max(0.0), "the content's end at the screen's foot");
        assert_eq!(footed_max, max(22.0));
        assert_eq!(footed_max - bare_max, 22.0, "the block is that much taller, so the page ends that much lower");
        assert_eq!(bare_target, reveal(0.0), "the reveal of the bare block");
        assert_eq!(footed_target, reveal(22.0), "the reveal of the block with its foot");
        assert!(max(0.0) > 0.0 && reveal(22.0) <= max(22.0), "setup: the page scrolls");
    }

    /// **A caption line stops short of the heading DRAWN under it, past a shelf with nothing to
    /// draw.** A shelf, an empty shelf and a third whose heading rises with its focused card: the
    /// empty one takes no room, so what stands under the first shelf's caption is the third's heading
    /// (`SectionFrame::below_rise`), and no line of the first ends below that heading's drawn top
    /// through a Down onto the third and an Up back.
    #[test]
    fn a_caption_keeps_clear_of_the_next_shelf_that_draws_past_an_empty_one() {
        let mut r = mrig(0, 6, |s| s);
        r.page.shelf.captioned = true;
        // the label band's air under the first shelf, as a page that stacks its shelves keeps it
        r.page.foot = crate::consts::UNDER_LABEL_AIR;
        r.page.more = vec![Cards::new(Vec::new(), false), Cards::new((200..206).collect(), false)];
        r.page.revision += 1;
        r.go(100, By::Restore);
        r.run(60);
        let (mut worst, mut lines) = (f32::MIN, 0);
        for frame in 0..130 {
            match frame {
                0 => r.go(200, By::Dir),
                60 => r.go(100, By::Dir),
                _ => {}
            }
            r.run(1);
            let cx = r.cx();
            let f = DrawFrame::new(&cx, Painter::root());
            let (first, third) = (Msec::Shelf, Msec::More(1));
            let above = r.stack.shelf_frame(&r.page, &cx, first).expect("the first shelf");
            let heading = r.stack.shelf_frame(&r.page, &cx, third).expect("the third shelf").y - 60.0
                - r.stack.shelf(third).expect("the third shelf").heading_lift();
            let drawn = r.stack.shelf(first).expect("the first shelf").caption_lines_probe(&f, &r.page.shelf, above);
            lines += drawn.len();
            worst = drawn.iter().fold(worst, |worst, &(_, _, bottom, _)| worst.max(bottom - heading));
        }
        assert!(lines > 0, "the walk saw no caption line");
        assert!(worst <= 0.0, "a caption line of the first shelf ends {worst:.1} px below the drawn top of the third shelf's heading");
    }

    /// **The caption rule counts `UNDER_LABEL_AIR` under a shelf** (`card_row::room_for`), so a page
    /// whose shelf keeps less than that while another shelf stands below it would have a line drawn over
    /// that shelf's heading, and a `Stack` refuses it rather than lay it out wrong: the default
    /// `shelf_foot` (0) is for the LAST shelf only.
    #[test]
    #[should_panic(expected = "UNDER_LABEL_AIR")]
    fn a_shelf_keeping_less_air_than_the_caption_rule_counts_is_refused() {
        let mut r = mrig(0, 6, |s| s);
        r.page.more = vec![Cards::new((200..206).collect(), false)];
        r.page.revision += 1;
        r.go(100, By::Restore);
        r.run(1);
        let cx = r.cx();
        let _ = r.stack.shelf_frame(&r.page, &cx, Msec::Shelf);
    }

    /// The settled scroll that shows shelf `k` of [`walk_rig`] (0 = `Shelf`, then `More(k - 1)`)
    /// from scroll `cur`, worked from the fixture's geometry.
    fn walk_want(k: usize, cur: f32) -> f32 {
        let (closed, open) = (60.0 + ROW_STYLE.h + card_row::under_band(0.0), 60.0 + ROW_STYLE.h + card_row::under_band(1.0));
        let top = FIELD_H + k as f32 * closed;
        let max = FIELD_H + 5.0 * closed + (open - closed) - (SCR_H_F - crate::consts::MARGIN_Y);
        let margin = if k == 0 { SHELF_MARGIN } else { crate::consts::MARGIN_Y };
        card_row::reveal(cur, top + open - (SCR_H_F - crate::consts::MARGIN_Y), top - margin, max)
    }

    /// A field and five shelves, focus on the field, no tick since.
    fn walk_rig() -> Mrig {
        let mut r = mrig(0, 10, |s| s.reveal_on_move(true));
        r.page.more = (0..4).map(|n| Cards::new((0..10).map(|i| 200 + 100 * n + i).collect(), false)).collect();
        r.page.revision += 1;
        r.go(FIELD_ELEM, By::Dir);
        r
    }

    #[test]
    fn a_reveal_on_move_is_measured_from_where_the_page_is_headed_not_where_it_is() {
        let mut r = walk_rig();
        let mut target = 0.0;
        for (k, elem) in [100, 200, 300, 400, 500].into_iter().enumerate() {
            r.go(elem, By::Dir);
            target = walk_want(k, target);
            assert_eq!(r.stack.target(), target, "shelf {k} from the previous target");
        }
        assert_eq!(r.stack.scroll(), 0.0, "setup: no tick has run, so the spring has not left home");
        r.go(400, By::Dir);
        let from_target = walk_want(3, target);
        assert_eq!(r.stack.target(), from_target, "Up measures from the target the Down walk left");
        assert_ne!(from_target, walk_want(3, r.stack.scroll()), "setup: measuring from the live position would differ");
    }

    #[test]
    fn a_page_may_name_where_focus_recovers_to_and_otherwise_the_stack_clamps() {
        let recovered = |to: Option<u32>| {
            let mut r = mrig(4, 4, |s| s);
            r.go(LIST_BASE + 3, By::Dir);
            r.run(2);
            r.page.rows = 2;
            r.page.revision += 1;
            r.page.recover_to = to;
            r.run(1);
            r.stack.view(&r.page).reconcile(FocusKey { entry: ENTRY, elem: LIST_BASE + 3 }, &r.cx()).elem
        };
        assert_eq!(recovered(None), LIST_BASE + 1, "no override: the same position, clamped");
        assert_eq!(recovered(Some(FIELD_ELEM)), FIELD_ELEM, "the page's answer decides");
    }

    #[test]
    fn a_shelfs_group_spans_the_page_unless_the_page_says_it_is_its_head_tile() {
        let extent = |narrow: bool| {
            let mut r = mrig(0, 8, |s| s);
            r.page.narrow = narrow;
            r.page.revision += 1;
            r.run(1);
            r.groups().into_iter().find(|g| g.id == GroupId(13)).unwrap().extent
        };
        let (wide, own) = (extent(false), extent(true));
        assert_eq!((wide.x, wide.w), (own.x, crate::consts::SCR_W - 2.0 * own.x), "the default spans the page's width");
        assert!(own.w < wide.w && (own.y, own.h) == (wide.y, wide.h), "the head tile alone: {own:?} vs {wide:?}");
    }

    #[test]
    fn a_clipped_page_clips_every_placement_and_defaults_to_none() {
        let r = mrig(2, 4, |s| s.clipped(FLOOR));
        for elem in [FIELD_ELEM, LIST_BASE, CLEAR_ELEM, 100] {
            assert_eq!(r.place(elem).unwrap().clip, FLOOR, "elem {elem}");
        }
        let plain = mrig(2, 4, |s| s);
        for elem in [FIELD_ELEM, LIST_BASE, CLEAR_ELEM, 100] {
            assert_eq!(plain.place(elem).unwrap().clip, Rect::FULL, "elem {elem}");
        }
    }
}

/// A source whose screen paints no focus on it (Home before the dive reaches its shelves).
struct Veiled(Cards);

impl CardSource<FixtureHost> for Veiled {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn elem(&self, i: usize) -> u32 {
        self.0.elem(i)
    }
    fn index_of(&self, e: &u32) -> Option<usize> {
        self.0.index_of(e)
    }
    fn focus_index(&self, _e: &u32) -> Option<usize> {
        None
    }
    fn art(&self, i: usize) -> Art<'_> {
        self.0.art(i)
    }
    fn label(&self, i: usize) -> TileLabel {
        self.0.label(i)
    }
}

#[test]
fn a_source_can_answer_which_card_the_picture_shows_focused() {
    let mut r = Rig::<Shelf>::new(8);
    r.run(2);
    r.land_focus(103, By::Restore);
    r.run(60);
    let veiled = Veiled(Cards::new((0..8u32).map(|i| 100 + i).collect(), false));
    let popped = r.sect.place(&r.cx(), &r.src, &103, SHELF_AT, At::Drawn).unwrap().rect;
    assert!((popped.w - RowStyle::HOME.w * RowStyle::HOME.focus_scale).abs() < 0.5, "popped under the engine's focus");
    // The engine still holds the card, the picture does not: the shelf lets it go.
    let mut present = Present::new();
    let mut out = Vec::new();
    for _ in 0..120 {
        r.ms += MS;
        let cx = cx9(&r.view, r.ms, r.press, r.focus);
        let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(9)), &mut present);
        r.sect.on(&ScreenEvent::Tick(cx.tick), &cx, &veiled, &mut fx);
    }
    let seen = r.sect.place(&r.cx(), &veiled, &103, SHELF_AT, At::Drawn).unwrap().rect;
    assert!((seen.w - RowStyle::HOME.w).abs() < 0.01, "so the card is drawn at rest: {}", seen.w);
}

/// `paint_resting` leaves the focused card out and `paint_focused` draws it alone, so a screen
/// can draw every shelf's others first and the focused card last.
#[test]
fn the_focused_card_paints_apart_from_the_rest() {
    let mut r = settled::<Shelf>(8);
    r.land_focus(101, By::Dir);
    r.run(60);
    let cx = r.cx();
    let f = DrawFrame::new(&cx, Painter::recording());
    let p = f.painter;
    r.src.drawn.borrow_mut().clear();
    r.sect.paint_resting(&f, p, &r.src, SHELF_AT);
    let rest: Vec<u32> = r.src.drawn.take().into_iter().map(|(e, _)| e).collect();
    assert!(!rest.is_empty() && !rest.contains(&101), "the others, not the focused card: {rest:?}");
    r.sect.paint_focused(&f, p, &r.src, SHELF_AT);
    let last: Vec<u32> = r.src.drawn.take().into_iter().map(|(e, _)| e).collect();
    assert_eq!(last, vec![101], "the focused card alone");
}

// ---- the element-keyed pop pool (owner decision 2) ------------------------------

/// Focus 100 settled, a deliberate move to 101, three frames: 100 is mid-shrink. Then `insert`
/// cards land ahead of both, so 100 stands `insert` cells further on.
fn mid_shrink_then_a_landing<S: Section>(insert: usize) -> (Rig<S>, f32) {
    let mut r = settled::<S>(40);
    r.land_focus(100, By::Restore);
    r.run(240);
    r.land_focus(101, By::Dir);
    r.run(3);
    let before = r.sect.scale_of(&r.cx(), &r.src, 100).unwrap();
    assert!(before > 1.02 && before < S::focus_scale() - 0.01, "100 is mid-shrink at {before}");
    for k in 0..insert {
        r.src.elems.insert(0, 900 + k as u32);
    }
    r.run(1);
    (r, before)
}

fn a_mid_shrink_card_keeps_its_scale_across_a_landing<S: Section>(insert: usize) {
    let (r, before) = mid_shrink_then_a_landing::<S>(insert);
    let now = r.sect.scale_of(&r.cx(), &r.src, 100).unwrap();
    assert!(now > 1.01 && now <= before && before - now < 0.03, "100 went from {before} to {now}");
    for k in 0..insert {
        assert_eq!(r.sect.scale_of(&r.cx(), &r.src, 900 + k as u32), Some(1.0), "the new card at rest");
    }
    let full = S::focus_scale();
    let focused = r.sect.scale_of(&r.cx(), &r.src, 101).unwrap();
    assert!(focused > 1.0 && focused < full, "the focused card is still growing, not {focused}");
}
#[test]
fn shelf_keeps_a_mid_shrink_scale_across_a_landing() {
    a_mid_shrink_card_keeps_its_scale_across_a_landing::<Shelf>(1);
}
#[test]
fn grid_keeps_a_mid_shrink_scale_across_a_landing() {
    a_mid_shrink_card_keeps_its_scale_across_a_landing::<Grid>(poster_grid::COLS);
}

/// The element that shrinks goes on shrinking and settles at rest wherever the landing put it, and
/// a card removed mid-shrink takes its let-go with it.
#[test]
fn shelf_pool_lets_go_to_rest_and_drops_a_removed_card() {
    let (mut r, _) = mid_shrink_then_a_landing::<Shelf>(2);
    r.run(240);
    for &e in &r.src.elems {
        let want = if e == 101 { Shelf::focus_scale() } else { 1.0 };
        assert!((r.sect.scale_of(&r.cx(), &r.src, &e).unwrap() - want).abs() < 0.002, "elem {e} settles");
    }
    assert_eq!(r.sect.pool_len(), 1, "only the focused card is held once everything else settled");
    let (mut r, _) = mid_shrink_then_a_landing::<Shelf>(0);
    r.src.elems.retain(|&e| e != 100);
    r.run(1);
    assert!(r.sect.pool_len() <= 1, "the removed card's entry is gone");
    assert_eq!(lifted(&r), 1, "only the focused card reads lifted");
}

/// A long fast walk back and forth never holds more than the pool's capacity.
#[test]
fn shelf_pool_never_exceeds_its_capacity() {
    let mut r = settled::<Shelf>(40);
    r.land_focus(100, By::Restore);
    let mut at = 0usize;
    let mut most = 0;
    for step in 0..400usize {
        let to = if (step / 30) % 2 == 0 { at + 1 } else { at.saturating_sub(1) }.min(39);
        if to != at {
            r.land_focus(100 + to as u32, By::Dir);
            at = to;
        }
        // a different key-repeat period each lap, down to a press every frame
        r.run(1 + (step as u32 % 3));
        if step % 50 == 7 {
            r.src.elems.insert(0, 1000 + step as u32);
            at += 1;
        }
        most = most.max(r.sect.pool_len());
        assert!(r.sect.pool_len() <= super::pool::CAP, "step {step}: {} held", r.sect.pool_len());
    }
    assert_eq!(most, super::pool::CAP, "the walk filled the pool");
}

/// A frame drawn after the source changed but before the next tick already shows the shrinking
/// element at its shrink scale and the card now in its old cell at rest: no frame reverses.
fn a_draw_between_the_insert_and_the_tick_does_not_reverse<S: Section>(insert: usize) {
    let mut r = settled::<S>(40);
    r.land_focus(100, By::Restore);
    r.run(240);
    r.land_focus(101, By::Dir);
    r.run(3);
    let before = r.sect.scale_of(&r.cx(), &r.src, 100).unwrap();
    assert!(before > 1.02 && before < S::focus_scale() - 0.01, "100 is mid-shrink at {before}");
    for k in 0..insert {
        r.src.elems.insert(0, 900 + k as u32);
    }
    // drawn now, before any tick has seen the new source
    assert_eq!(r.sect.scale_of(&r.cx(), &r.src, 100), Some(before), "100 keeps its shrink in the first frame");
    for k in 0..insert {
        assert_eq!(r.sect.scale_of(&r.cx(), &r.src, 900 + k as u32), Some(1.0), "the new card is at rest in the first frame");
    }
    r.run(1);
    let now = r.sect.scale_of(&r.cx(), &r.src, 100).unwrap();
    assert!(now <= before && before - now < 0.03, "and the tick continues it: {before} to {now}");
}
#[test]
fn shelf_pool_draws_no_reversal_between_an_insert_and_the_tick() {
    a_draw_between_the_insert_and_the_tick_does_not_reverse::<Shelf>(1);
}
#[test]
fn grid_pool_draws_no_reversal_between_an_insert_and_the_tick() {
    a_draw_between_the_insert_and_the_tick_does_not_reverse::<Grid>(poster_grid::COLS);
}

/// A card pushed past the last cell with its own spring (a row longer than `MAX_ROW_ITEMS`) by a
/// landing keeps its pop: the pool cannot find it below the cap, and must leave the spring for the
/// landing rule to carry.
#[test]
fn shelf_pool_carries_a_focused_card_pushed_past_the_spring_array() {
    let last = crate::card_row::MAX_ROW_ITEMS - 1;
    let mut r = settled::<Shelf>(40);
    let focused = 100 + last as u32;
    r.land_focus(focused, By::Dir);
    r.run(3);
    let before = r.sect.scale_of(&r.cx(), &r.src, &focused).unwrap();
    assert!(before > 1.005 && before < Shelf::focus_scale() - 0.01, "the focused card is mid-grow at {before}");
    r.src.elems.insert(0, 900);
    r.run(1);
    let now = r.sect.scale_of(&r.cx(), &r.src, &focused).unwrap();
    assert!(now >= before - 0.001, "the focused card kept its pop across the landing: {before} to {now}");
}

/// A key-repeat walk, a press every frame, with the pool full.
fn full_pool_walk() -> (Rig<Shelf>, usize) {
    let mut r = settled::<Shelf>(40);
    r.land_focus(100, By::Restore);
    let mut most = 0;
    for k in 1..=14usize {
        r.land_focus(100 + k as u32, By::Dir);
        r.run(1);
        assert!(r.sect.pool_holds(k), "step {k}: the focused card is held ({} held)", r.sect.pool_len());
        most = most.max(r.sect.pool_len());
    }
    (r, most)
}

/// The focused card is admitted first: a pool full of let-gos still holds it.
#[test]
fn shelf_pool_always_holds_the_focused_card() {
    let (_, most) = full_pool_walk();
    assert_eq!(most, super::pool::CAP, "the walk filled the pool");
}

/// A landing with the pool full keeps the focused card's pop.
#[test]
fn shelf_pool_full_and_a_landing_keeps_the_focused_pop() {
    let (mut r, _) = full_pool_walk();
    assert_eq!(r.sect.pool_len(), super::pool::CAP);
    let before = r.sect.scale_of(&r.cx(), &r.src, &114).unwrap();
    assert!(before > 1.0 && before < Shelf::focus_scale() - 0.01, "114 is mid-grow at {before}");
    r.src.elems.insert(0, 900);
    r.run(1);
    let now = r.sect.scale_of(&r.cx(), &r.src, &114).unwrap();
    assert!(now >= before - 0.001, "114 kept its pop: {before} to {now}");
    assert!(r.sect.pool_len() <= super::pool::CAP);
}

/// The grid looks for a let-go's element within a window of where it was: one that a landing
/// moved further than that lets go (a Library of thousands pays no scan).
#[test]
fn grid_pool_drops_a_let_go_pushed_beyond_the_search_window() {
    let mut r = settled::<Grid>(600);
    r.land_focus(100, By::Restore);
    r.run(240);
    r.land_focus(101, By::Dir);
    r.run(3);
    assert!(r.sect.scale_of(&r.cx(), &r.src, &100).unwrap() > 1.02);
    let rows = super::pool::SEARCH.div_ceil(poster_grid::COLS) + 2;
    for k in 0..rows * poster_grid::COLS {
        r.src.elems.insert(0, 10_000 + k as u32);
    }
    r.run(1);
    assert_eq!(r.sect.scale_of(&r.cx(), &r.src, &100), Some(1.0), "100 is out of the window: its let-go is dropped");
}

/// **A held key on a grid lets EVERY released card go on a spring of its own.** `path` is the
/// walk of the focus, one `By::Dir` press every `period` frames (a let-go lasts ~28 frames, so a
/// short period leaves several in flight at once). Each card is shadowed by the free-running
/// spring it must be: grown toward `focus_scale` while focused, let go toward rest after, landed by
/// `Spring::land`, and picked up where it is if focus comes back to it before it has landed. No
/// frame may draw a card more than the land epsilon from that spring, which also bounds the
/// frame-to-frame jump of a released card: a press that replaced the let-go of the card still
/// shrinking snapped it to rest in one frame (a Library grid's 3.8-5.4 px per edge). Returns the
/// most cards letting go at once and how long a card walked past lets go, in frames.
fn every_released_grid_card_runs_its_own_let_go(path: &[u32], period: u32) -> (usize, u32) {
    let style = poster_grid::STYLE;
    let dt = 16_667.0 / 1_000_000.0;
    let mut r = settled::<Grid>(600);
    r.land_focus(path[0], By::Restore);
    r.run(240);
    // (element, the spring it must be, whether it has landed)
    let mut cards: Vec<(u32, Spring, bool)> = vec![(path[0], Spring::at(style.focus_scale), false)];
    let mut focused = path[0];
    let mut next = 1;
    let mut most_in_flight = 0;
    // how long a card walked past lets go: a lone spring grown for the `period` frames focus stayed
    // on it, to its landing
    let life = {
        let mut lone = Spring::at(1.0);
        for _ in 0..period {
            lone.step(style.focus_scale, style.k_scale, dt);
        }
        (1..).find(|_| { lone.step(1.0, style.k_scale, dt); lone.land(1.0, dt) }).unwrap()
    };
    for frame in 0..path.len() as u32 * period + 200 {
        if next < path.len() && frame % period == 0 {
            focused = path[next];
            next += 1;
            r.land_focus(focused, By::Dir);
            match cards.iter_mut().find(|c| c.0 == focused) {
                // a card still letting go takes up where its let-go is
                Some(c) if !c.2 => {}
                Some(c) => *c = (focused, Spring::at(1.0), false),
                None => cards.push((focused, Spring::at(1.0), false)),
            }
        }
        r.run(1);
        most_in_flight = most_in_flight.max(cards.iter().filter(|c| !c.2 && c.0 != focused).count());
        for c in cards.iter_mut() {
            let drawn = r.scale(c.0).unwrap();
            if !c.2 {
                c.1.step(if c.0 == focused { style.focus_scale } else { 1.0 }, style.k_scale, dt);
                c.2 = c.0 != focused && c.1.land(1.0, dt);
            }
            // the grid holds a let-go for exactly the cards still letting go
            let held = r.sect.letting_go(r.src.index_of(&c.0).unwrap());
            assert_eq!(held, !c.2 && c.0 != focused, "frame {frame}: card {}: the grid holds its let-go while it is in flight, and no longer", c.0);
            let jump = (drawn - c.1.pos).abs();
            let epsilon = crate::LAND_REL * (1.0 + c.1.pos.max(1.0));
            assert!(
                jump <= epsilon,
                "frame {frame}: card {} is drawn at {drawn}, {jump} from the spring it runs on ({}), past the land epsilon {epsilon}: \
                 a width step of {} px on a {} px card; the focus is on {focused}",
                c.0, c.1.pos, jump * poster_grid::STYLE.w, poster_grid::STYLE.w,
            );
        }
    }
    assert!(cards.iter().all(|c| c.2 || c.0 == focused), "every released card landed");
    assert!(most_in_flight >= 2.min(path.len() - 1), "the walk left several let-gos in flight at once: {most_in_flight}");
    for c in cards.iter().filter(|c| c.0 != focused) {
        assert_eq!(r.scale(c.0), Some(1.0), "card {} lands exactly at rest", c.0);
    }
    (most_in_flight, life)
}

/// A held Right: ten presses, three frames apart, so up to nine let-gos are in flight.
#[test]
fn grid_held_key_every_released_card_runs_its_own_let_go() {
    every_released_grid_card_runs_its_own_let_go(&(100..110).collect::<Vec<_>>(), 3);
}

/// A press every frame: as many let-gos in flight as presses fit in one let-go's life, which is more
/// than a few. Nothing is capped and no card is retired early.
#[test]
fn grid_held_key_a_press_every_frame_snaps_no_released_card() {
    let path: Vec<u32> = (100..140).collect();
    let (most, life) = every_released_grid_card_runs_its_own_let_go(&path, 1);
    let presses = path.len() as u32 - 1;
    assert!(most as u32 + 1 >= life.min(presses), "{most} let-gos were in flight at once, a let-go lasts {life} frames and {presses} presses were made");
}

/// A held Right turned into a held Left: the card focus comes back to is still letting go, and
/// grows from where it is rather than from rest.
#[test]
fn grid_held_key_turned_around_picks_each_let_go_up_where_it_is() {
    let there: Vec<u32> = (100..106).collect();
    let back: Vec<u32> = (100..105).rev().collect();
    every_released_grid_card_runs_its_own_let_go(&[there, back].concat(), 3);
}

/// Several let-gos in flight when cards land ahead of them: every one follows its element, none
/// jumps, and a frame drawn before the tick has seen the landing already shows each where it went.
#[test]
fn grid_pool_follows_every_let_go_through_a_landing() {
    let mut r = settled::<Grid>(600);
    r.land_focus(100, By::Restore);
    r.run(240);
    for e in 101..106 {
        r.land_focus(e, By::Dir);
        r.run(3);
    }
    let flying: Vec<u32> = (100..105).collect();
    let before: Vec<f32> = flying.iter().map(|&e| r.scale(e).unwrap()).collect();
    assert!(before.iter().filter(|&&s| s > 1.01).count() >= 3, "several are still letting go: {before:?}");
    for k in 0..poster_grid::COLS {
        r.src.elems.insert(0, 900 + k as u32);
    }
    for (&e, &was) in flying.iter().zip(&before) {
        assert_eq!(r.scale(e), Some(was), "{e} keeps its scale in the first frame");
    }
    r.run(1);
    for (&e, &was) in flying.iter().zip(&before) {
        let now = r.scale(e).unwrap();
        assert!(now <= was && was - now < 0.03, "{e} went from {was} to {now}");
        // the tick moved the let-go to the cell its element stands in now: the draw reads cells
        let at = r.src.index_of(&e).unwrap();
        assert!(r.sect.letting_go(at), "{e}: its let-go is in the cell {at} it stands in");
    }
    r.run(240);
    for &e in &flying {
        assert_eq!(r.scale(e), Some(1.0), "{e} settles at rest wherever the landing put it");
    }
}

/// **A press made over cards that shifted since the last tick still lets the card it left go from
/// where it was, and grows the card it names from rest.** `shift` changes the cards between the last
/// tick and the press (a landing, the same frame as the key); `from` held the pop (full) and `to` is
/// named by the press. The pop's old cell and the index `to` now has can be the SAME cell: the let-go of
/// `from` must not be taken back by it. Both are checked against the free-running springs, in the frame
/// drawn before the tick has seen the shift and the frame after, and through to rest.
fn a_press_over_cards_that_shifted_lets_the_card_it_left_go(shift: impl FnOnce(&mut Vec<u32>), from: u32, to: u32) {
    let style = poster_grid::STYLE;
    let dt = 16_667.0 / 1_000_000.0;
    let mut r = settled::<Grid>(600);
    r.land_focus(from, By::Restore);
    r.run(240);
    let full = r.scale(from).unwrap();
    assert!((full - style.focus_scale).abs() < 1e-3, "{from} is popped whole at {full}");
    let was = r.src.index_of(&from).unwrap();
    shift(&mut r.src.elems);
    let (now_from, now_to) = (r.src.index_of(&from).unwrap(), r.src.index_of(&to).unwrap());
    assert!(now_from != was && now_to == was, "the shift put {to} in the cell {from} popped in ({was}): {from} {was} -> {now_from}, {to} -> {now_to}");
    r.land_focus(to, By::Dir);
    let near = |a: f32, b: f32| (a - b).abs() <= 1e-4;
    let (mut let_go, mut grow) = (Spring::at(full), Spring::at(1.0));
    let drawn = |r: &Rig<Grid>, e: u32| r.scale(e).unwrap();
    for frame in 0..=1u32 {
        if frame == 1 {
            r.run(1);
            let_go.step(1.0, style.k_scale, dt);
            grow.step(style.focus_scale, style.k_scale, dt);
        }
        let (a, b) = (drawn(&r, from), drawn(&r, to));
        let which = if frame == 0 { "before the tick" } else { "after the tick" };
        assert!(near(a, let_go.pos), "{which}: {from} lets go from {full}: it is drawn at {a}, the spring is at {}", let_go.pos);
        assert!(near(b, grow.pos), "{which}: {to} grows from rest: it is drawn at {b}, the spring is at {}", grow.pos);
    }
    assert!(drawn(&r, from) > 1.01, "{from} is still far from rest");
    r.run(240);
    assert_eq!(drawn(&r, from), 1.0, "{from} lands at rest");
    assert!(near(drawn(&r, to), style.focus_scale), "{to} is popped");
}

/// A card removed ahead of the focus, then Right in the same frame: the card after the focus now
/// has the focused card's old index.
#[test]
fn grid_a_press_after_a_card_was_removed_ahead_lets_the_card_it_left_go() {
    a_press_over_cards_that_shifted_lets_the_card_it_left_go(|cards| { cards.remove(50); }, 205, 206);
}

/// A card inserted ahead of the focus, then Left in the same frame: the card before the focus now
/// has the focused card's old index.
#[test]
fn grid_a_press_after_a_card_was_inserted_ahead_lets_the_card_it_left_go() {
    a_press_over_cards_that_shifted_lets_the_card_it_left_go(|cards| cards.insert(50, 900), 205, 204);
}

/// A frame must not hash a card it draws because other cards are letting go: it verifies each
/// let-go's own card once per pass (the draw, the stops) and resolves every other card by its cell.
/// A source change no tick has seen yet is the one frame that does hash every card (the draw
/// between an insert and the tick), and it has its own tests.
#[test]
fn grid_draw_hashes_no_drawn_card_while_cards_let_go() {
    let mut r = settled::<Grid>(600);
    // one row of the grid (cards 96..=101): the page does not scroll while they are walked
    r.land_focus(196, By::Restore);
    r.run(240);
    let source_asks = |r: &Rig<Grid>| {
        let before = r.src.elem_calls.get();
        r.drawn_and_stops(1.0, 0.0);
        r.src.elem_calls.get() - before
    };
    for e in 197..202 {
        r.land_focus(e, By::Dir);
        r.run(3);
    }
    let letting_go = (96..101).filter(|&cell| r.sect.letting_go(cell)).count();
    assert!(letting_go >= 3, "several cards are letting go: {letting_go}");
    let during = source_asks(&r);
    r.run(240);
    let at_rest = source_asks(&r);
    assert!(at_rest > 20, "the frame asks the source about the cards it draws ({at_rest})");
    assert!(
        during <= at_rest + 2 * letting_go,
        "a frame with {letting_go} cards letting go asked the source {during} times against {at_rest} at rest: more than \
         one question per let-go for each of its two passes",
    );
}

/// One frame of an abandoned press: the press dip and the drawn width of the pressed card and of
/// its neighbour, each relative to a card at rest.
struct AbandonRow {
    ms: u32,
    what: &'static str,
    dip: f32,
    pressed: f32,
    neighbour: f32,
    /// The press reported motion this frame (what keeps frames coming and the page repainting).
    motion: bool,
}

/// OK goes down on card 100 and is held for 350 ms (the `collection-tap` scene's cycle), then Right
/// moves focus to 101. The dispatcher abandons the press on that move (`Press::cancel`); the press
/// dip stays with the card that was pressed while it springs back, so the pressed card is the
/// press's owner after focus has left it. Real `Press`, real section, one 16 ms frame at a time.
fn abandoned_press_trace<S: Section + 'static>() -> Vec<AbandonRow> {
    crate::popover::set_lift_owns(false);
    let mut r = Rig::<S>::new(6);
    r.land_focus(100, By::Dir);
    r.run(120);
    let rest = r.drawn_rect(101, 1.0).unwrap().w;
    let mut press = crate::press::Press::new();
    let row = |r: &Rig<S>, ms: u32, what, dip, motion| {
        let width = |elem| r.drawn_rect(elem, dip).unwrap().w / rest;
        AbandonRow { ms, what, dip, pressed: width(100), neighbour: width(101), motion }
    };
    let t0 = r.ms;
    let mut rows = vec![row(&r, 0, "idle", 1.0, false)];
    r.pressed = r.focus;
    press.begin(r.ms);
    let mut what = "ok-down";
    for n in 1..=45 {
        if n == 22 {
            press.cancel();
            r.land_focus(101, By::Dir);
            what = "RIGHT";
        }
        r.ms += MS;
        let (_, motion) = plx_machine::idle::scoped_motion(|| press.tick(r.ms, MS as f32 / 1000.0));
        r.feed(ScreenEvent::Tick(Tick { ms: r.ms, dt_us: 16_667 }));
        let dip = if press.is_active() { press.scale() } else { 1.0 };
        rows.push(row(&r, r.ms - t0, what, dip, motion));
        what = "";
    }
    rows
}

/// Navigating away from a held press must not make any card jump: the pressed card springs back
/// from its dip (the underdamped release) while the neighbour takes the ordinary focus pop with no
/// dip. Neither card's drawn width may change by more than 3 % between two consecutive frames
/// (the ordinary per-frame animation step is under 1 %; the bug was +8.6 % / -7.9 % in one frame).
fn abandoned_press_never_jumps<S: Section + 'static>() {
    let rows = abandoned_press_trace::<S>();
    println!("  ms  event    press  pressed  neighbour  motion");
    for r in &rows {
        println!("{:4}  {:8} {:.3}  {:.3}    {:.3}      {}", r.ms, r.what, r.dip, r.pressed, r.neighbour, r.motion);
    }
    for pair in rows.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        for (name, before, after) in [("pressed card (100)", a.pressed, b.pressed), ("neighbour (101)", a.neighbour, b.neighbour)] {
            assert!((after / before - 1.0).abs() < 0.03,
                "{name} jumped {:+.1} % in ONE frame at {} ms ({before:.3} -> {after:.3})", (after / before - 1.0) * 100.0, b.ms);
        }
    }
    // The spring-back runs on a card that no longer has focus: it must keep reporting motion for as
    // long as the dip is visibly moving, or the pressed card would freeze mid-spring on the page.
    for r in rows.iter().skip_while(|r| r.what != "RIGHT").skip(1).filter(|r| (r.dip - 1.0).abs() > 0.03) {
        assert!(r.motion, "the spring-back at {} ms (press {:.3}) reported no motion", r.ms, r.dip);
    }
    let last = rows.last().unwrap();
    assert!((last.pressed - 1.0).abs() < 0.01, "the pressed card settles at its rest size, not a dip");
    assert!((last.neighbour - rows[0].pressed).abs() < 0.01, "the neighbour settles at the focus pop with no dip");
}

#[test]
fn shelf_abandoned_press_never_jumps() {
    abandoned_press_never_jumps::<Shelf>();
}

#[test]
fn grid_abandoned_press_never_jumps() {
    abandoned_press_never_jumps::<Grid>();
}

/// A press owned by the shared header id (`Collection`/`Person` header press) must dip NO member
/// card, including the ones with no interned element yet whose `elem(i)` falls back to that id.
/// The owner resolves once per pass through `index_of`, like focus, so the fallback never matches.
fn a_header_press_dips_no_unindexed_member<S: Section + 'static>() {
    let mut r = Rig::<S>::new(6);
    r.src.unindexed_from = 3;
    r.pressed = Some(FocusKey { entry: ENTRY, elem: 0 });
    let rects = |r: &Rig<S>, press| r.stops(press).into_iter().map(|s| s.rect).collect::<Vec<_>>();
    assert_eq!(rects(&r, 0.9), rects(&r, 1.0), "no card dips for a press owned by the header id");
    r.pressed = Some(FocusKey { entry: ENTRY, elem: 101 });
    let (dipped, rest) = (rects(&r, 0.9), rects(&r, 1.0));
    assert!(dipped[1].w < rest[1].w * 0.95, "the indexed owner card still dips");
    assert_eq!(dipped[2], rest[2], "only the owner dips");
}

#[test]
fn shelf_header_press_dips_no_unindexed_member() {
    a_header_press_dips_no_unindexed_member::<Shelf>();
}

#[test]
fn grid_header_press_dips_no_unindexed_member() {
    a_header_press_dips_no_unindexed_member::<Grid>();
}

/// The focus read-out of a shelf that holds a slid window names the card by its place in the
/// whole listing (`cdg`), not by its slot in the window (`cdi`): the two were the same number
/// on every shelf once, and a reach graded on it could never pass the window length.
#[cfg(feature = "devtriggers")]
#[test]
fn a_shelfs_probe_reports_the_listing_position_apart_from_the_window_slot() {
    crate::card_probe::arm_for_test();
    let mut src = Cards::new((100..112).collect(), true);
    for (first, want) in [(0, 11), (12, 23)] {
        src.first = first;
        crate::card_probe::clear();
        super::shelf::note_focused(Painter::root(), Rect::new(10.0, 20.0, 200.0, 300.0), 11, &src);
        let seen = crate::card_probe::peek().expect("the focused card was noted");
        assert_eq!((seen.index, seen.len, seen.global), (11, 12, want), "window starting at {first}");
    }
}

// ---- animated landings (`landing.rs`) ----------------------------------------------------

/// A forty-card row with landings animated, focus on card 112 (index 12) and everything at rest.
fn landing_rig() -> Rig<Shelf> {
    landing_rig_at(112)
}

/// [`landing_rig`] with focus on `elem` instead.
fn landing_rig_at(elem: u32) -> Rig<Shelf> {
    crate::containers::transition::set_page_image_held(false);
    let mut r = Rig::<Shelf>::new(40);
    r.sect = Shelf::new(ENTRY, &RowStyle::HOME).animate_landings(true);
    r.run(2);
    r.land_focus(elem, By::Restore);
    r.run(240);
    r
}

fn drawn_x(r: &Rig<Shelf>, e: u32) -> f32 {
    r.sect.place(&r.cx(), &r.src, &e, SHELF_AT, At::Drawn).unwrap().rect.x
}

/// The ghosts of a landing, painted from a source holding `elems` (one per ghost, in the order
/// `Shelf::ghost_keys` names them) through a recording painter: the removed cards the section
/// really drew, with their screen rects.
fn ghost_paint(r: &Rig<Shelf>, elems: &[u32]) -> Vec<(u32, Rect)> {
    let cx = r.cx();
    let f = DrawFrame::new(&cx, Painter::recording());
    let ghosts = Cards::new(elems.to_vec(), false);
    r.sect.paint_ghosts(&f, f.painter, &ghosts, SHELF_AT);
    ghosts.drawn.take()
}

/// Every card the section draws now (the live source, not the ghosts), with its screen rect.
fn drawn_cards(r: &Rig<Shelf>) -> Vec<(u32, Rect)> {
    r.drawn_and_stops(1.0, 0.0).0
}

fn pitch() -> f32 {
    RowStyle::HOME.w + RowStyle::HOME.gap
}

/// A removed card is a ghost drawn from the PREVIOUS source at the slot it stood in on screen,
/// fading out at rest, and the shelf is quiet again once it is gone.
#[test]
fn a_removed_card_fades_out_as_a_ghost_of_the_previous_source() {
    let mut r = landing_rig();
    let was = r.sect.place(&r.cx(), &r.src, &111, SHELF_AT, At::Drawn).unwrap().rect;
    r.src.elems.remove(11);
    r.run(1);
    let (ghosts, a0) = r.sect.ghost_probe();
    assert_eq!(ghosts, vec![11], "the card's index in the previous source");
    assert!(a0 < 1.0 && a0 > 0.0, "fading already, not gone: {a0}");
    let drawn = ghost_paint(&r, &[111]);
    let (e, rect) = drawn.iter().copied().find(|&(e, _)| e == 111).expect("the ghost is painted from the previous source");
    assert_eq!(e, 111);
    assert!((rect.x - was.x).abs() < 0.5 && (rect.w - was.w).abs() < 0.01, "at the slot it stood in, at rest: {was:?} {rect:?}");
    let mut last = a0;
    for _ in 0..8 {
        r.run(1);
        let (_, a) = r.sect.ghost_probe();
        assert!(a < last, "fades monotonically: {a} after {last}");
        last = a;
    }
    r.run(600);
    assert!(r.sect.ghost_probe().0.is_empty(), "gone once faded");
    assert!(ghost_paint(&r, &[111]).is_empty(), "nothing painted after");
    assert!(!r.sect.landing_pending() && !r.run(1), "and the shelf is quiet");
}

/// An inserted card fades in from nothing; the others do not fade.
#[test]
fn an_inserted_card_fades_in() {
    let mut r = landing_rig();
    r.src.elems.insert(13, 900);
    r.run(1);
    let n = r.src.elems.len();
    let (_, a0) = r.sect.landing_probe(n, 13);
    assert!(a0 > 0.0 && a0 < 0.5, "starts from nothing: {a0}");
    assert_eq!(r.sect.landing_probe(n, 12).1, 1.0, "a card that was there does not fade");
    let mut last = a0;
    for _ in 0..12 {
        r.run(1);
        let (_, a) = r.sect.landing_probe(n, 13);
        assert!(a > last, "fades in monotonically");
        last = a;
    }
    r.run(600);
    assert_eq!(r.sect.landing_probe(n, 13), (0.0, 1.0));
    assert!(!r.sect.landing_pending() && !r.run(1), "and the shelf is quiet");
}

/// The cards that stay start where they stood on screen and slide to their new slot, offset
/// decaying to 0; the focused tile's rect never changes, its pop never drops and at most one tile
/// is lifted in any frame.
#[test]
fn survivors_slide_and_the_focused_tile_stays_put() {
    let mut r = landing_rig();
    let focus_at = r.sect.place(&r.cx(), &r.src, &112, SHELF_AT, At::Drawn).unwrap().rect;
    let left_was = drawn_x(&r, 110);
    let right_was = drawn_x(&r, 114);
    r.src.elems.remove(11);
    r.run(1);
    let full = RowStyle::HOME.focus_scale;
    let mut prev_off = f32::INFINITY;
    for frame in 0..90 {
        let now = r.sect.place(&r.cx(), &r.src, &112, SHELF_AT, At::Drawn).unwrap().rect;
        assert!((now.x - focus_at.x).abs() < 0.5 && (now.w - focus_at.w).abs() < 0.01, "frame {frame}: the focused rect moved: {focus_at:?} -> {now:?}");
        assert_eq!(r.scale(112), Some(full), "frame {frame}: the pop dropped");
        assert!(lifted(&r) <= 1, "frame {frame}: more than one tile lifted");
        let off = r.sect.landing_probe(r.src.elems.len(), 10).0.abs();
        assert!(off <= prev_off + 1e-3, "the offset decays: {off} after {prev_off}");
        prev_off = off;
        r.run(1);
    }
    assert!((drawn_x(&r, 114) - right_was).abs() < 0.5, "a card right of the focus never moved on screen");
    assert!((drawn_x(&r, 110) - left_was - pitch()).abs() < 0.5, "the card left of the gap closed it: {left_was} -> {}", drawn_x(&r, 110));
    assert_eq!(r.sect.landing_probe(r.src.elems.len(), 10).0, 0.0, "settled at exactly its slot");
}

/// Right after the landing the sliding card is still where it stood, the stop it registers is the
/// rect it is drawn at, and the slide is a real distance (a pitch, not a nudge).
#[test]
fn a_sliding_card_starts_where_it_stood_and_its_stop_follows() {
    let mut r = landing_rig();
    let was = drawn_x(&r, 110);
    r.src.elems.remove(11);
    r.run(1);
    let first = drawn_x(&r, 110);
    assert!((first - was).abs() < 0.25 * pitch(), "starts from where it stood: {was} -> {first}");
    let at = r.stops(1.0).into_iter().find(|s| s.key.elem == 110).unwrap().rect.x;
    assert!((at - first).abs() < 0.01, "the stop is the drawn rect: {at} vs {first}");
    r.run(1);
    assert!(drawn_x(&r, 110) > first, "and moves toward its slot");
}

/// The first appearance of a shelf's cards is not a landing, and neither is a row of placeholders
/// becoming cards: nothing is there to change.
#[test]
fn nothing_animates_on_the_first_load() {
    crate::containers::transition::set_page_image_held(false);
    let mut r = Rig::<Shelf>::new(0);
    r.sect = Shelf::new(ENTRY, &RowStyle::HOME).animate_landings(true);
    r.run(2);
    r.src.elems = (100..108).collect();
    r.run(1);
    for i in 0..8 {
        assert_eq!(r.sect.landing_probe(8, i), (0.0, 1.0));
    }
    assert!(r.sect.ghost_probe().0.is_empty() && !r.sect.landing_pending());

    let mut p = Rig::<Shelf>::new(8);
    p.sect = Shelf::new(ENTRY, &RowStyle::HOME).animate_landings(true);
    p.src.unloaded = 0..8;
    p.run(2);
    p.src.elems = (200..208).collect();
    p.src.unloaded = 0..0;
    p.run(1);
    for i in 0..8 {
        assert_eq!(p.sect.landing_probe(8, i), (0.0, 1.0), "placeholders landing as cards cut");
    }
    assert!(p.sect.ghost_probe().0.is_empty());
}

/// A section that did not ask for animated landings cuts, as it always did.
#[test]
fn landings_are_off_unless_asked_for() {
    let mut r = settled::<Shelf>(40);
    r.land_focus(112, By::Restore);
    r.run(240);
    r.src.elems.remove(11);
    r.run(1);
    assert!(r.sect.ghost_probe().0.is_empty());
    assert_eq!(r.sect.landing_probe(r.src.elems.len(), 10), (0.0, 1.0));
}

/// A dormant shelf draws no ghosts and snaps: the change is adopted whole, nothing plays after
/// it wakes either.
#[test]
fn a_dormant_shelf_snaps_a_landing() {
    let mut r = landing_rig();
    r.sect.dormant(true);
    r.src.elems.remove(11);
    r.src.elems.insert(13, 900);
    r.run(3);
    assert!(r.sect.ghost_probe().0.is_empty());
    r.sect.dormant(false);
    r.run(2);
    let n = r.src.elems.len();
    assert_eq!(r.sect.landing_probe(n, 13), (0.0, 1.0));
    assert_eq!(r.sect.landing_probe(n, 10), (0.0, 1.0));
    assert!(r.sect.ghost_probe().0.is_empty());
}

/// A landing that arrives while the page is a held image starts at once, so the first live frame
/// after the release shows the START state (the old slots, a full-alpha ghost), and it does not
/// step until the image is gone.
#[test]
fn a_landing_under_a_held_page_image_shows_its_start_state_on_release() {
    let mut r = landing_rig();
    crate::containers::transition::set_page_image_held(true);
    r.src.elems.remove(11);
    r.run(6);
    let n = r.src.elems.len();
    assert_eq!(r.sect.ghost_probe(), (vec![11], 1.0), "begun, and not stepped under the image");
    let slid = r.sect.landing_probe(n, 10).0;
    assert!((slid + pitch()).abs() < 0.5, "the card left of the gap still stands at its old slot: {slid}");
    // the frame painted between the release and the next tick
    crate::containers::transition::set_page_image_held(false);
    assert_eq!(r.sect.ghost_probe(), (vec![11], 1.0));
    assert_eq!(r.sect.landing_probe(n, 10).0, slid);
    let ghost = ghost_paint(&r, &[111]);
    assert_eq!(ghost.len(), 1, "the ghost is painted at full alpha");
    r.run(1);
    assert!(r.sect.ghost_probe().1 < 1.0 && r.sect.landing_probe(n, 10).0 > slid, "and it plays from the first live tick");
    r.run(600);
    assert!(!r.sect.landing_pending() && !r.run(1), "and the shelf is quiet");
}

/// When the focused card is the one removed its ghost starts at the scale it was drawn at (no
/// one-frame drop from lifted to rest) and eases to rest while it fades; the card the screen then
/// seats focus on (the slot it held, `clamp_slot`) is adopted whole, and no more than one LIVE tile
/// is lifted in any frame.
#[test]
fn a_removed_focused_card_eases_from_its_lift_and_focus_is_reseated_by_the_slot_rule() {
    let mut r = landing_rig();
    let full = RowStyle::HOME.focus_scale;
    r.src.elems.remove(12);
    r.run(1);
    assert_eq!(r.sect.ghost_probe().0, vec![12]);
    let first = r.sect.ghost_scales()[0];
    assert!(first > 1.0 + 0.5 * (full - 1.0) && first <= full, "starts at the lift it was drawn at, not at rest: {first}");
    let ghost = ghost_paint(&r, &[112]).iter().find(|&&(e, _)| e == 112).expect("the focused card's ghost is painted").1;
    assert!(ghost.w > RowStyle::HOME.w * 1.05, "drawn lifted, not collapsed to rest: {ghost:?}");
    let mut last = first;
    for _ in 0..8 {
        r.run(1);
        let s = r.sect.ghost_scales()[0];
        assert!(s < last && s >= 1.0, "eases to rest: {s} after {last}");
        last = s;
    }
    r.run(600);
    assert!(r.sect.ghost_probe().0.is_empty());
    // the steady state holds one lifted tile
    let mut r = landing_rig();
    r.src.elems.remove(12);
    r.run(1);
    let slot = super::clamp_slot(12, r.src.elems.len()).unwrap();
    let to = r.src.elems[slot];
    r.land_focus(to, By::Reconcile);
    for _ in 0..30 {
        r.run(1);
        assert!(lifted(&r) <= 1);
    }
    assert_eq!(r.scale(to), Some(full), "adopted whole");
}

/// A second landing while the first still fades does not restart it: the ghost already fading goes
/// on from its alpha and stays where it stood on screen, and the new one starts full.
#[test]
fn a_second_landing_carries_the_live_ghost_and_gives_each_card_its_own_alpha() {
    let mut r = landing_rig();
    r.src.elems.remove(11);
    r.run(4);
    let a1 = r.sect.ghost_alphas()[0];
    let x1 = ghost_paint(&r, &[111])[0].1.x;
    r.src.elems.remove(10);
    r.run(1);
    let al = r.sect.ghost_alphas();
    assert_eq!(r.sect.ghost_probe().0.len(), 2, "both fade");
    assert!(al[0] < a1 && a1 < 1.0, "the first goes on from where it was: {a1} -> {}", al[0]);
    assert!(al.iter().any(|&a| a > 0.9), "the second starts full: {al:?}");
    let x2 = ghost_paint(&r, &[111, 110]).iter().find(|&&(e, _)| e == 111).unwrap().1.x;
    assert!((x1 - x2).abs() < 1.0, "the first ghost stays where it stood on screen: {x1} -> {x2}");
    // an arrival mid-fade keeps its own alpha when another card arrives
    let mut r = landing_rig();
    r.src.elems.insert(13, 900);
    r.run(4);
    let n = r.src.elems.len();
    let a900 = r.sect.landing_probe(n, 13).1;
    r.src.elems.remove(11);
    r.run(1);
    let n = r.src.elems.len();
    let now = r.sect.landing_probe(n, 12).1;
    assert!(now >= a900 && now < 1.0, "the first arrival goes on from its alpha, not from nothing: {a900} -> {now}");
    assert!(!r.sect.ghost_probe().0.is_empty(), "and the removed card fades on its own");
}

/// A slide of the window the source holds (its offset or its page epoch moved) is adopted whole:
/// the deck's window moving is not a card leaving.
#[test]
fn a_window_slide_is_adopted_without_animating() {
    let mut r = landing_rig();
    r.src.elems.drain(0..3);
    r.src.first = 3;
    r.run(1);
    assert!(r.sect.ghost_probe().0.is_empty() && !r.sect.landing_pending(), "an offset change cuts");
    let mut r = landing_rig();
    r.src.elems.remove(11);
    r.src.epoch += 1;
    r.run(1);
    assert!(r.sect.ghost_probe().0.is_empty() && !r.sect.landing_pending(), "a page landing cuts");
    // and a plain landing after it still plays
    r.src.elems.remove(11);
    r.run(1);
    assert!(r.sect.landing_pending());
}

// ---- no cap on the cards a landing diffs ----

/// A forty-card row, focus on index 30: a card removed at 31 is a ghost, the cards after it slide
/// (none fades), and nothing is drawn twice.
#[test]
fn a_landing_deep_in_a_long_row_plays() {
    let mut r = landing_rig_at(130);
    r.src.elems.remove(31);
    r.run(1);
    let n = r.src.elems.len();
    assert_eq!(r.sect.ghost_probe().0, vec![31], "the card past the old 24-card limit fades");
    for i in 30..n {
        assert_eq!(r.sect.landing_probe(n, i).1, 1.0, "card {i} slides, it does not fade");
    }
    assert!(r.sect.landing_probe(n, 31).0.abs() > 1.0, "the card after the gap slides");
    let drawn = drawn_cards(&r);
    let mut seen = std::collections::HashSet::new();
    assert!(drawn.iter().all(|&(e, _)| seen.insert(e)), "no card is drawn twice");
    assert!(ghost_paint(&r, &[131]).iter().all(|&(e, _)| e == 131) && !drawn.iter().any(|&(e, _)| e == 131));
    r.run(600);
    assert!(!r.sect.landing_pending());
}

/// A card inserted before the old limit does not turn the cards after it into fades or cuts.
#[test]
fn an_insert_before_the_old_limit_slides_the_cards_past_it() {
    let mut r = landing_rig_at(122);
    r.src.elems.insert(23, 900);
    r.run(1);
    let n = r.src.elems.len();
    assert!(r.sect.ghost_probe().0.is_empty(), "nothing leaves");
    assert!(r.sect.landing_probe(n, 23).1 < 1.0, "the new card fades in");
    for i in (0..n).filter(|&i| i != 23) {
        assert_eq!(r.sect.landing_probe(n, i).1, 1.0, "card {i} does not fade");
    }
    for i in [24] {
        let off = r.sect.landing_probe(n, i).0;
        assert!(off < -0.5 * pitch() && off > -pitch(), "card {i}, past the old limit, slides one slot: {off}");
    }
    let drawn = drawn_cards(&r);
    let mut seen = std::collections::HashSet::new();
    assert!(drawn.iter().all(|&(e, _)| seen.insert(e)), "no card is drawn twice");
}

/// A change that is off-axis at both ends has no spring, no fade and no ghost: it cuts where nobody
/// can see it, and the cards on screen do not move.
#[test]
fn an_off_axis_change_creates_nothing_to_animate() {
    let mut r = landing_rig();
    let before = drawn_cards(&r);
    r.src.elems.remove(35);
    r.run(1);
    assert!(r.sect.ghost_probe().0.is_empty() && !r.sect.landing_pending());
    assert_eq!(drawn_cards(&r), before);
}

// ---- animated landings on the grid (`landing.rs`, DS4) ------------------------------------

use crate::consts::{K_SCALE, K_SCROLL};

/// `x0` after `frames` ticks of a critically damped spring at stiffness `k`, from rest: the
/// closed form the springs integrate (so a test names the constant a landing runs on).
fn critical(x0: f32, k: f32, frames: u32) -> f32 {
    let w = k.sqrt();
    let t = frames as f32 * 16_667e-6;
    x0 * (1.0 + w * t) * (-w * t).exp()
}

fn animated_grid(n: usize, focus: u32, spec: GridSpec) -> Rig<Grid> {
    crate::containers::transition::set_page_image_held(false);
    let mut r = Rig::<Grid>::new(n);
    r.sect = Grid::new(ENTRY, spec.animate_landings(true));
    r.run(2);
    r.land_focus(focus, By::Restore);
    r.run(240);
    r
}

/// The engine's answer to the focused card leaving: focus goes to the card now at its index.
fn remove_focused_grid(r: &mut Rig<Grid>, at: usize) {
    r.src.elems.remove(at);
    let next = r.reconcile_after_removal(at);
    r.land_focus(next, By::Reconcile);
}

fn grid_ghost_paint(g: &Grid, cx: &Cx9<'_>, elems: &[u32]) -> Vec<(u32, Rect)> {
    let f = DrawFrame::new(cx, Painter::recording());
    let ghosts = Cards::new(elems.to_vec(), false);
    g.paint_ghosts(&f, f.painter, &ghosts);
    ghosts.drawn.take()
}

fn settled_rect(r: &Rig<Grid>, e: u32) -> Rect {
    <Rig<Grid> as CardHarness>::place(r, e, At::SpringTarget).unwrap().rect
}

fn centre(r: Rect) -> (f32, f32) {
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

/// The focused card leaves: it is a ghost, fading where it stood on `K_SCALE`, easing from its
/// lifted scale to rest, and the grid is quiet once it is gone.
#[test]
fn a_grid_ghost_fades_in_its_cell_on_k_scale() {
    let mut r = animated_grid(40, 108, GRID);
    let was = <Rig<Grid> as CardHarness>::place(&r, 108, At::Drawn).unwrap().rect;
    remove_focused_grid(&mut r, 8);
    r.run(1);
    // the frame the landing begins in draws the start state: the ghost at full alpha, no step
    assert!((r.sect.ghost_probe().1 - 1.0).abs() < 1e-6, "the first frame is the frame before");
    r.run(1);
    let (ghosts, a) = r.sect.ghost_probe();
    assert_eq!(ghosts, vec![8], "the card's index in the previous source");
    assert!((a - critical(1.0, K_SCALE, 1)).abs() < 2e-3, "fades on K_SCALE: {a}");
    let lift = r.sect.ghost_scales()[0];
    assert!(lift > 1.0 && lift <= poster_grid::STYLE.focus_scale + 1e-4, "a focused ghost eases from its lift: {lift}");
    let cx = r.cx();
    let drawn = grid_ghost_paint(&r.sect, &cx, &[108]);
    let (_, rect) = *drawn.iter().find(|&&(e, _)| e == 108).expect("the ghost is painted from the card the owner kept");
    let (c, w) = (centre(rect), centre(was));
    assert!((c.0 - w.0).abs() < 0.5 && (c.1 - w.1).abs() < 0.5, "in the cell it stood in: {was:?} {rect:?}");
    let mut last = a;
    for f in 2..=9 {
        r.run(1);
        let (_, a) = r.sect.ghost_probe();
        assert!(a < last && (a - critical(1.0, K_SCALE, f)).abs() < 2e-3, "frame {f}: monotone, on K_SCALE: {a}");
        last = a;
    }
    r.run(600);
    assert!(r.sect.ghost_probe().0.is_empty() && grid_ghost_paint(&r.sect, &r.cx(), &[108]).is_empty(), "gone once faded");
    assert!(!r.sect.landing_pending() && !r.sect.has_ghosts() && !r.run(1), "and the grid is quiet");
}

/// **A ghost the opener lift carries is drawn once, by the lift, at its lift and full alpha**: the
/// page pass (`lifted` names it) leaves it out and says so, the lift's frame (`lifting`) paints it
/// alone, and a frame with neither paints it as ever. Under the menu's veil it holds where it
/// stood, lifted and opaque.
#[test]
fn a_lifted_ghost_is_left_out_of_the_page_pass_and_drawn_by_the_lift() {
    use crate::containers::modal::set_veil;
    use crate::popover::{lift_owns, set_lift_owns};
    let mut r = animated_grid(40, 108, GRID);
    set_veil(1.0);
    let was = <Rig<Grid> as CardHarness>::place(&r, 108, At::Drawn).unwrap().rect;
    r.src.elems.remove(10);
    remove_focused_grid(&mut r, 8);
    r.run(3);
    let order: Vec<u32> = r.sect.ghost_keys().map(|k| [108, 110].into_iter().find(|&e| super::elem_key(&e) == k).unwrap()).collect();
    assert_eq!(order.len(), 2, "premise: two ghosts");
    assert!(r.sect.departing::<FixtureHost>(r.key(108)), "the removed card is departing");
    assert!(!r.sect.departing::<FixtureHost>(r.key(109)), "a card on the page is not");
    let cx = r.cx();
    let ghosts = Cards::new(order, false);
    set_lift_owns(false);
    let mut page = DrawFrame::new(&cx, Painter::recording());
    page.lifted = Some(r.key(108));
    r.sect.paint_ghosts(&page, page.painter, &ghosts);
    let drawn: Vec<u32> = ghosts.drawn.take().into_iter().map(|(e, _)| e).collect();
    assert_eq!(drawn, vec![110], "the page pass leaves the lifted ghost out, and only it");
    assert!(lift_owns(), "…and says the lift owns it");
    let mut lift = DrawFrame::new(&cx, Painter::recording());
    lift.lifting = Some(r.key(108));
    ghosts.painted.borrow_mut().clear();
    r.sect.paint_ghosts(&lift, lift.painter, &ghosts);
    let painted = ghosts.painted.take();
    assert_eq!(painted.len(), 1, "the lift paints that ghost alone: {painted:?}");
    let (e, rect, a) = painted[0];
    assert!(e == 108 && rect_close(rect, was) && a == 1.0, "where it stood, lifted, opaque: {rect:?} {was:?} {a}");
    let plain = DrawFrame::new(&cx, Painter::recording());
    ghosts.focused.borrow_mut().clear();
    r.sect.paint_ghosts(&plain, plain.painter, &ghosts);
    assert!(ghosts.drawn.take().iter().any(|&(e, _)| e == 108), "a frame with no lift paints it as ever");
    // the focused card goes as the focused card: its state's treatment (a wash) does not snap to
    // the unfocused one on the frame it leaves; a card that was not focused goes as it stood
    let mut marks = ghosts.focused.take();
    marks.sort();
    assert_eq!(marks, vec![(108, true), (110, false)], "each ghost keeps the focus it had");
    set_veil(0.0);
    set_lift_owns(false);
    r.run(600);
    assert!(!r.sect.departing::<FixtureHost>(r.key(108)), "gone once faded");
}

/// Every card the grid paints this frame, in call order, with its alpha.
fn grid_paints(r: &Rig<Grid>) -> Vec<(u32, Rect, f32)> {
    let cx = r.cx();
    let mut f = DrawFrame::new(&cx, Painter::recording());
    r.src.painted.borrow_mut().clear();
    let p = f.painter;
    r.sect.draw(&mut f, p, &r.src);
    r.src.painted.take()
}

/// Survivors that keep their row slide along it on `K_SCROLL`, all on the same frame (no
/// stagger), and never leave it. The card that crosses the line break does NOT fly diagonally
/// over the row between: it fades out where it stood and in at its new slot, each copy on
/// `K_SCALE`, the two alphas complementary, and it is never painted anywhere in between.
#[test]
fn a_card_that_changes_row_crossfades_and_the_rest_slide_along_their_rows() {
    let mut r = animated_grid(40, 108, GRID);
    let watch = [110u32, 111, 113, 117];
    let wrap = 112u32;
    let old: Vec<Rect> = watch.iter().map(|&e| settled_rect(&r, e)).collect();
    let wrap_old = settled_rect(&r, wrap);
    remove_focused_grid(&mut r, 8);
    let mut series = Vec::new();
    let mut paints = Vec::new();
    // the frame the landing begins in draws the start state (a grid does not step on it)
    r.run(1);
    for _ in 1..=40 {
        r.run(1);
        series.push(watch.iter().map(|&e| settled_rect(&r, e)).collect::<Vec<_>>());
        paints.push(grid_paints(&r));
    }
    r.run(600);
    let new: Vec<Rect> = watch.iter().map(|&e| settled_rect(&r, e)).collect();
    let wrap_new = settled_rect(&r, wrap);
    assert!(wrap_old.y - wrap_new.y > 100.0 && wrap_new.x - wrap_old.x > poster_grid::STYLE.w,
        "premise: 112 goes from the start of the next row to the end of this one: {wrap_old:?} -> {wrap_new:?}");
    for (f, rects) in series.iter().enumerate() {
        let want = critical(1.0, K_SCROLL, f as u32 + 1);
        for (j, &e) in watch.iter().enumerate() {
            let (sx, py) = (old[j].x - new[j].x, rects[j].y - new[j].y);
            assert!(py.abs() < 0.01, "frame {}: card {e} left its row: {py}", f + 1);
            assert!((old[j].y - new[j].y).abs() < 0.01, "premise: card {e} keeps its row");
            if sx.abs() > 0.5 {
                let px = rects[j].x - new[j].x;
                assert!((px / sx - want).abs() < 3e-3, "frame {}: card {e} x is off the shared K_SCROLL curve: {} vs {want}", f + 1, px / sx);
            }
        }
        // the crossing card: exactly an out copy where it stood and an in copy at its slot
        let copies: Vec<_> = paints[f].iter().filter(|p| p.0 == wrap).collect();
        let fade_in = 1.0 - critical(1.0, K_SCALE, f as u32 + 1);
        let at = |want: Rect| copies.iter().find(|c| rect_close(c.1, want)).map(|c| c.2);
        let (out, inn) = (at(wrap_old), at(wrap_new));
        assert!(copies.iter().all(|c| rect_close(c.1, wrap_old) || rect_close(c.1, wrap_new)),
            "frame {}: 112 painted somewhere between its two slots: {copies:?}", f + 1);
        let inn = inn.unwrap_or_else(|| panic!("frame {}: no in copy: {copies:?}", f + 1));
        assert!((inn - fade_in).abs() < 3e-3, "frame {}: the in copy is off K_SCALE: {inn} vs {fade_in}", f + 1);
        if 1.0 - inn > 1e-3 {
            let out = out.unwrap_or_else(|| panic!("frame {}: no out copy: {copies:?}", f + 1));
            assert!((out + inn - 1.0).abs() < 1e-4, "frame {}: out {out} + in {inn} is not 1", f + 1);
        }
        assert_eq!(r.sect.landing_probe(r.src.elems.len(), 11).0, (0.0, 0.0), "the crossing card never slides");
    }
    assert!(!r.sect.landing_pending() && !r.run(1));
    assert_eq!(grid_paints(&r).iter().filter(|p| p.0 == wrap).count(), 1, "one copy once settled");
}

/// Z-order of a remove: the leaving copy is painted before (under) every card that slides, and the
/// focused card last (on top); the owner's ghost goes under all of them (`paint_ghosts` runs
/// before `draw`).
#[test]
fn a_remove_paints_the_leaving_copy_under_the_sliding_cards_and_the_focus_on_top() {
    let mut r = animated_grid(40, 108, GRID);
    remove_focused_grid(&mut r, 8);
    r.run(3);
    let order: Vec<u32> = grid_paints(&r).iter().map(|p| p.0).collect();
    let first = |e: u32| order.iter().position(|&x| x == e).unwrap();
    let out = first(112);
    for e in [110u32, 111, 113, 114] {
        assert!(out < first(e), "the leaving copy of 112 is over sliding card {e}: {order:?}");
    }
    assert_eq!(order.last(), Some(&109), "the focused card is painted last: {order:?}");
}

/// The focused card leaves from the end of its row: the card focus reconciles to comes up from the
/// next row, so the focused card itself crossfades: its leaving copy is a plain tile at rest under
/// the others, its arriving copy the focused one, painted last, rising from rest.
#[test]
fn a_focused_card_that_changes_row_crossfades_on_top() {
    let mut r = animated_grid(40, 111, GRID);
    let from = settled_rect(&r, 112);
    let to = centre(<Rig<Grid> as CardHarness>::place(&r, 111, At::Drawn).unwrap().rect);
    remove_focused_grid(&mut r, 11);
    assert_eq!(r.focus.map(|k| k.elem), Some(112));
    let mut last = 0.0;
    for frame in 0..30 {
        r.run(1);
        let paints = grid_paints(&r);
        let mine: Vec<_> = paints.iter().enumerate().filter(|(_, p)| p.0 == 112).collect();
        for (_, p) in &mine {
            let c = centre(p.1);
            assert!(rect_close(p.1, from) || ((c.0 - to.0).abs() < 0.5 && (c.1 - to.1).abs() < 0.5),
                "frame {frame}: 112 painted between its two slots: {:?}", p.1);
        }
        let (top, inn) = *mine.last().unwrap();
        assert_eq!(top, paints.len() - 1, "frame {frame}: the arriving focused copy is on top");
        assert!(inn.2 >= last, "frame {frame}: fades in monotonically");
        last = inn.2;
        if inn.2 < 0.999 {
            let (_, out) = mine.first().filter(|(j, _)| *j != top).expect("a leaving copy");
            assert!(rect_close(out.1, from), "frame {frame}: the leaving copy stands where 112 was at rest: {:?} {from:?}", out.1);
            assert!((out.2 + inn.2 - 1.0).abs() < 1e-4);
        }
        assert!(lifted(&r) <= 1, "frame {frame}: more than one tile lifted");
    }
}

/// **A landing a menu's choice started waits for the menu.** While a surface veils the page
/// (opening, open, or closing above `CLOSING_GONE`) nothing of the landing moves or reports motion:
/// the ghost stands lifted at full alpha, the survivors where they were, the takeover at rest. On the
/// first frame after the menu has let go every spring starts on one frame: no beat.
#[test]
fn a_landing_waits_for_the_menu_to_leave_then_moves_at_once_on_the_first_frame_after() {
    use crate::containers::modal::{set_veil, CLOSING_GONE};
    let mut r = animated_grid(40, 108, GRID);
    let stood = settled_rect(&r, 109);
    set_veil(1.0);
    remove_focused_grid(&mut r, 8);
    assert!(!r.run(1), "the landing's first frame under the menu reports no motion");
    let n = r.src.elems.len();
    let start = r.sect.landing_probe(n, 8);
    assert!(start.0 .0 > 1.0, "premise: 109 has a slide to make: {start:?}");
    let mut fade = 1.0f32;
    for frame in 0..40 {
        // the menu's fade-out, down to just above the veil's edge
        if frame >= 10 {
            fade = (fade * 0.9).max(CLOSING_GONE + 1e-3);
            set_veil(fade);
        }
        let moved = r.run(1);
        assert!(!moved, "frame {frame}: the waiting landing reported motion under the menu");
        assert_eq!(r.sect.landing_probe(n, 8), start, "frame {frame}: 109 moved under the menu");
        assert_eq!(r.sect.ghost_probe().1, 1.0, "frame {frame}: the ghost faded under the menu");
        assert_eq!(r.scale(109), Some(1.0), "frame {frame}: the takeover rose under the menu");
        let drawn = <Rig<Grid> as CardHarness>::place(&r, 109, At::Drawn).unwrap().rect;
        assert!(rect_close(drawn, stood), "frame {frame}: 109 is not where it stood: {drawn:?} {stood:?}");
    }
    set_veil(CLOSING_GONE * 0.5);
    r.run(1);
    let ((x, _), _) = r.sect.landing_probe(n, 8);
    assert!(x < start.0 .0, "the slide starts on the first frame after the menu let go");
    assert!(r.sect.ghost_probe().1 < 1.0, "the ghost fades on the same frame");
    let s = r.scale(109).unwrap();
    assert!(s > 1.0, "and the takeover rises on it: {s}");
    set_veil(0.0);
    r.run(600);
    assert!((r.scale(109).unwrap() - poster_grid::STYLE.focus_scale).abs() < 1e-4);
    assert!(!r.sect.landing_pending() && !r.run(1));
}

/// The app reconciles focus a frame AFTER the card leaves (the landing's first tick sees the focused
/// element gone, so no focus): the takeover still rises from rest with the landing, never popping
/// to full the frame focus comes back (`--trigger watchlist=` Remove render, 2026-10-16).
#[test]
fn a_takeover_reconciled_a_frame_late_still_rises_from_rest_with_the_landing() {
    use crate::containers::modal::set_veil;
    let mut r = animated_grid(40, 108, GRID);
    set_veil(1.0);
    r.src.elems.remove(8);
    r.run(1);
    assert!(r.scale(109).unwrap() < 1.0 + 1e-4, "the frame before focus is reconciled: 109 is not lifted");
    let next = r.reconcile_after_removal(8);
    r.land_focus(next, By::Reconcile);
    assert_eq!(r.focus.map(|k| k.elem), Some(109));
    for frame in 0..20 {
        r.run(1);
        assert_eq!(r.scale(109), Some(1.0), "frame {frame}: the late-reconciled takeover is lifted under the menu");
        assert!(lifted(&r) <= 1, "frame {frame}: more than one tile lifted");
    }
    set_veil(0.0);
    r.run(600);
    assert!((r.scale(109).unwrap() - poster_grid::STYLE.focus_scale).abs() < 1e-4, "and it ends lifted");
}

/// A landing no menu veiled starts at once: the frame it begins in draws the start state and the
/// next one moves, with no beat between.
#[test]
fn a_landing_with_no_menu_starts_at_once() {
    crate::containers::modal::set_veil(0.0);
    let mut r = animated_grid(40, 108, GRID);
    let n0 = r.src.elems.len();
    let before = r.sect.landing_probe(n0, 9);
    remove_focused_grid(&mut r, 8);
    r.run(1);
    assert!(r.sect.ghost_probe().1 == 1.0, "the first frame draws the start state");
    r.run(1);
    let ((x, _), _) = r.sect.landing_probe(r.src.elems.len(), 8);
    assert!(before.0 == (0.0, 0.0) && x > 0.0 && x < settled_rect(&r, 110).x - settled_rect(&r, 109).x, "moved on frame 1: {x}");
    assert!((r.sect.ghost_probe().1 - critical(1.0, K_SCALE, 1)).abs() < 2e-3);
}

/// The next card is the focus: it takes the removed card's cell rising from rest as the ghost
/// sinks (one lifted tile in every frame, the takeover's pop on `K_SCALE`), and its stop is the
/// rect it is drawn at.
#[test]
fn focus_moves_to_the_next_card_and_it_slides_into_the_cell() {
    let mut r = animated_grid(40, 108, GRID);
    let cell = <Rig<Grid> as CardHarness>::place(&r, 108, At::Drawn).unwrap().rect;
    let from = <Rig<Grid> as CardHarness>::place(&r, 109, At::Drawn).unwrap().rect;
    remove_focused_grid(&mut r, 8);
    assert_eq!(r.focus.map(|k| k.elem), Some(109));
    let full = poster_grid::STYLE.focus_scale;
    for frame in 0..90 {
        r.run(1);
        let want = full - (full - 1.0) * critical(1.0, K_SCALE, frame + 1);
        let s = r.scale(109).unwrap();
        assert!((s - want).abs() < 2e-3, "frame {frame}: the takeover rises from rest on K_SCALE: {s} vs {want}");
        assert!(lifted(&r) <= 1, "frame {frame}: more than one tile lifted");
        let (drawn, stops) = r.drawn_and_stops(1.0, 0.0);
        let d = drawn.iter().find(|&&(e, _)| e == 109).map(|&(_, d)| d).expect("the next card is drawn");
        let st = stops.iter().find(|s| s.key.elem == 109).expect("and registered").rect;
        assert!(rect_close(d, st), "frame {frame}: drawn {d:?} is not its stop {st:?}");
        if frame == 0 {
            assert!(d.x > cell.x + 0.5 * (from.x - cell.x), "still near where it stood: {d:?}");
        }
    }
    let end = <Rig<Grid> as CardHarness>::place(&r, 109, At::Drawn).unwrap().rect;
    assert!(rect_close(end, cell), "settled in the removed card's cell: {end:?} {cell:?}");
}

fn rect_close(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() < 0.5 && (a.y - b.y).abs() < 0.5 && (a.w - b.w).abs() < 0.5 && (a.h - b.h).abs() < 0.5
}

/// A card with no end on screen cuts (no spring, no ghost), and one on screen plays: there is no
/// cap, only the screen.
#[test]
fn a_grid_snaps_what_is_off_screen_and_animates_what_is_not() {
    let mut r = animated_grid(200, 108, GRID);
    remove_focused_grid(&mut r, 8);
    r.run(1);
    let n = r.src.elems.len();
    assert_ne!(r.sect.landing_probe(n, 9).0, (0.0, 0.0), "a card on screen springs");
    let ((_, dy), a) = r.sect.landing_probe(n, 11);
    assert!(dy == 0.0 && a < 1.0, "one from the next row crossfades across the break: {dy} {a}");
    for i in [150, 199 - 1] {
        assert_eq!(r.sect.landing_probe(n, i).0, (0.0, 0.0), "card {i} is off screen at both ends and cuts");
    }
    r.run(600);
    assert!(!r.sect.landing_pending());
    // a card that leaves off screen plays nothing at all
    let before = r.drawn_and_stops(1.0, 0.0).0;
    r.src.elems.remove(150);
    r.run(1);
    assert!(r.sect.ghost_probe().0.is_empty() && !r.sect.landing_pending(), "nothing to animate");
    assert_eq!(r.drawn_and_stops(1.0, 0.0).0, before, "and what is on screen does not move");
}

/// A card that arrives fades in and the cards after it slide; a grid that did not ask plays nothing.
#[test]
fn an_inserted_card_fades_in_on_the_grid_and_an_unanimated_grid_cuts() {
    let mut r = animated_grid(40, 108, GRID);
    r.src.elems.insert(10, 900);
    r.run(1);
    let n = r.src.elems.len();
    assert_eq!(r.sect.landing_probe(n, 10).1, 0.0, "the first frame is the frame before: nothing yet");
    // the cards that were at 10.. slide right through the new card's cell: it appears when they
    // have left it
    let mut a = 0.0;
    for _ in 0..120 {
        r.run(1);
        a = r.sect.landing_probe(n, 10).1;
        if a > 0.0 {
            break;
        }
    }
    assert!(a > 0.0 && a < 0.5, "fades in from nothing: {a}");
    assert_eq!(r.sect.landing_probe(n, 9).1, 1.0);
    r.run(600);
    assert!(!r.sect.landing_pending());
    let mut r = settled::<Grid>(40);
    r.land_focus(108, By::Restore);
    r.run(240);
    remove_focused_grid(&mut r, 8);
    r.run(1);
    assert!(!r.sect.landing_pending() && !r.sect.has_ghosts() && r.sect.ghost_keys().next().is_none(), "off by default");
}

/// A removal BEFORE the focus that moves it up a row shifts the page to keep the focused tile put;
/// the ghost stays where the card stood ON SCREEN (the cards beside it spring to the shifted page,
/// the ghost fades where it was), in the grid's own scroll and under an owner's.
fn a_ghost_holds_its_screen_place_when_the_landing_shifts_the_page<S: Section>(mut r: Rig<S>, grid: fn(&Rig<S>) -> &Grid)
where
    Rig<S>: CardHarness,
{
    let stood = r.place(111, At::SpringTarget).unwrap().rect;
    r.src.elems.remove(11);
    let shifted = r.scroll();
    for frame in 0..30 {
        r.run(1);
        let cx = r.cx();
        let drawn = grid_ghost_paint(grid(&r), &cx, &[111]);
        let (_, ghost) = *drawn.first().expect("the ghost is painted");
        assert!(rect_close(ghost, stood), "frame {frame}: the ghost left its place: {stood:?} -> {ghost:?} (scroll {shifted:?} -> {:?})", r.scroll());
    }
}
#[test]
fn a_grid_ghost_stays_in_its_row_when_the_page_shifts() {
    crate::containers::transition::set_page_image_held(false);
    let r = animated_grid(40, 112, GRID);
    a_ghost_holds_its_screen_place_when_the_landing_shifts_the_page(r, |r| &r.sect);
}
#[test]
fn an_external_grid_ghost_stays_in_its_row_when_the_page_shifts() {
    crate::containers::transition::set_page_image_held(false);
    let mut r = Rig::<ExtGrid>::new(40);
    r.sect = ExtGrid { grid: Grid::new(ENTRY, EXT.animate_landings(true)), page: 0.0 };
    r.run(2);
    r.land_focus(112, By::Restore);
    r.run(240);
    a_ghost_holds_its_screen_place_when_the_landing_shifts_the_page(r, |r| &r.sect.grid);
}

fn mount_animated_shelf(n: usize) -> Box<dyn CardHarness> {
    let mut r = Rig::<Shelf>::new(n);
    r.sect = Shelf::new(ENTRY, &RowStyle::HOME).animate_landings(true);
    Box::new(r)
}
fn mount_animated_grid(n: usize) -> Box<dyn CardHarness> {
    let mut r = Rig::<Grid>::new(n);
    r.sect = Grid::new(ENTRY, GRID.animate_landings(true));
    Box::new(r)
}
fn mount_animated_external_grid(n: usize) -> Box<dyn CardHarness> {
    let mut r = Rig::<ExtGrid>::new(n);
    r.sect = ExtGrid { grid: Grid::new(ENTRY, EXT.animate_landings(true)), page: 0.0 };
    Box::new(r)
}

/// Every conformance case still passes with landings animated, and so does the removal contract.
#[test]
fn the_conformance_cases_and_the_removal_contract_pass_with_landings_animated() {
    let table: [(&'static str, Mount); 3] = [
        ("shelf", mount_animated_shelf),
        ("grid", mount_animated_grid),
        ("external", mount_animated_external_grid),
    ];
    let matrix = conformance::run_all(&table);
    let bad: Vec<_> = matrix.iter().filter(|(_, _, o)| *o != Outcome::Pass).collect();
    assert!(bad.is_empty(), "{bad:#?}");
    let _guard = plx_base::testlock::serial();
    for (screen, mount) in table {
        assert_eq!(conformance::removal_focuses_next(mount), Outcome::Pass, "{screen}");
    }
}

// ---- the caption band --------------------------------------------------------------------

/// What one grid row's caption and caption band did on one frame.
#[derive(Clone, Copy, Debug)]
struct RowFrame {
    /// The caption's painted alpha (0 when no caption of the row is on screen).
    alpha: f32,
    /// How open the row's band is, 0..1: the spring the layout reads for the row's gap.
    open: f32,
}

/// The block a source puts under a card for `policy`: the lines the policy shows, and no others
/// (the title, the details line, or both); `None` is a section that follows no policy, whose cards
/// carry both lines.
fn shape_captions(src: &mut Cards, policy: Option<crate::caption_policy::CaptionPolicy>) {
    src.captioned = true;
    if let Some(policy) = policy {
        src.title_only = policy.details == crate::caption_policy::DetailsKind::None;
        src.details_only = !policy.title;
    }
}

const BOTH_LINES: crate::caption_policy::CaptionPolicy =
    crate::caption_policy::CaptionPolicy { title: true, details: crate::caption_policy::DetailsKind::YearLength };
const TITLE_ONLY: crate::caption_policy::CaptionPolicy =
    crate::caption_policy::CaptionPolicy { title: true, details: crate::caption_policy::DetailsKind::None };

/// A grid of `rows` rows of captioned cards under `policy`, focus seated on card 0 with its band open.
fn captioned_grid(rows: usize, policy: Option<crate::caption_policy::CaptionPolicy>) -> Rig<Grid> {
    let mut r = Rig::<Grid>::new(rows * poster_grid::COLS);
    shape_captions(&mut r.src, policy);
    r.sect.set_caption_policy(policy);
    r.land_focus(100, By::Restore);
    r.run(40);
    r
}

/// A six-column grid under the two-line Under posters policy (a 112 px band) with focus seated on
/// card 0, its band open. Frame by frame, `downs` are the frames a Down lands on (`FocusMoved` by
/// `Dir`, then the tick), over `frames` frames; the result is, per row, what its caption and its
/// band did on each frame: sample 0 is the settled frame before the first move, sample `n + 1` the
/// frame `n` drew.
fn caption_trace(rows: usize, downs: &[usize], frames: usize) -> Vec<Vec<RowFrame>> {
    let cols = poster_grid::COLS;
    let mut r = captioned_grid(rows, Some(BOTH_LINES));
    let mut trace = vec![Vec::new(); rows];
    let mut sample = |r: &Rig<Grid>| {
        let cx = r.cx();
        let f = DrawFrame::new(&cx, Painter::root());
        let painted = r.sect.caption_probe(&f, f.painter, &r.src);
        let bands = r.sect.band_geometry();
        for (row, series) in trace.iter_mut().enumerate() {
            series.push(RowFrame {
                alpha: painted.iter().filter(|(i, _)| i / cols == row).map(|(_, a)| a).sum(),
                open: bands.iter().find(|b| b.row == row).map_or(0.0, |b| b.expansion),
            });
        }
    };
    // the settled frame the first move starts from: sample 0
    sample(&r);
    let mut at = 0;
    for frame in 0..frames {
        if downs.contains(&frame) {
            at += 1;
            r.land_focus(100 + (at * cols) as u32, By::Dir);
        }
        r.run(1);
        sample(&r);
    }
    trace
}

/// **A caption line is never drawn where the tile below it is DRAWN.** Frame by frame through a
/// Down, a Down and an Up on a grid under `policy`, no caption line with ink (alpha > 0) reaches the
/// top of the tile directly below it AS IT IS DRAWN (`Grid::place(.., At::Drawn)`: its live pop and
/// press folded in): not the arriving caption while the band opens (the next row is still rising to
/// its place under it), not the departing one while the band closes (the rising row slides up under
/// it), and not either while the tile below is popped: on a Down the tile under the departing
/// caption is the newly focused card growing to its pop, up to 17 px above its cell, and on an Up
/// the tile under the arriving caption is the released card still letting go; tiles are drawn over
/// captions. Panics with the worst overlap in px, and when no line was ever on screen (a walk that
/// observes nothing proves nothing). Returns how many lines it saw with a card letting go directly
/// under them, so a walk that is meant to leave one there can say it did.
fn assert_captions_clear_of_the_tile_below(policy: crate::caption_policy::CaptionPolicy, walk: &[(usize, i32)]) -> usize {
    let cols = poster_grid::COLS;
    let rows = 6;
    let mut r = captioned_grid(rows, Some(policy));
    let mut at = 0i32;
    let mut worst = (f32::MIN, String::new());
    let (mut observed, mut over_a_let_go) = (0, 0);
    for frame in 0..100 {
        if let Some(&(_, dir)) = walk.iter().find(|(f, _)| *f == frame) {
            at += dir;
            r.land_focus(100 + (at as usize * cols) as u32, By::Dir);
        }
        r.run(1);
        let cx = r.cx();
        let f = DrawFrame::new(&cx, Painter::root());
        for (card, _top, bottom, alpha) in r.sect.caption_lines_probe(&f, f.painter, &r.src) {
            observed += 1;
            if card + cols >= rows * cols {
                continue;
            }
            let below = r.place(r.src.elems[card + cols], At::Drawn).expect("the tile below a caption is painted").rect;
            over_a_let_go += usize::from(r.sect.letting_go(card + cols));
            let over = bottom - below.y;
            if over > worst.0 {
                worst = (over, format!("frame {frame}: the caption line of card {card} (alpha {alpha:.2}) ends {over:.1} px \
                    below the top of the tile under it (drawn at y {:.1})", below.y));
            }
        }
    }
    assert!(observed > 0, "{policy:?}: no caption line was ever on screen: the walk observed nothing");
    assert!(worst.0 <= 0.0, "{policy:?}: worst overlap {:.1} px: {}", worst.0, worst.1);
    over_a_let_go
}

const WALK: [(usize, i32); 3] = [(0, 1), (30, 1), (60, -1)];

/// A held Down then a held Up, a press every three frames: every press lands while the card the
/// last one released is still letting go, so the tile under a departing caption (Down) and under an
/// arriving one (Up) is a let-go in flight, with others still shrinking beside it.
const HELD_WALK: [(usize, i32); 9] = [(0, 1), (3, 1), (6, 1), (9, 1), (12, 1), (18, -1), (21, -1), (24, -1), (27, -1)];

/// The 112 px band (title over details), the 80 px band (title alone) and the stored DEFAULT (title
/// off, details Year · Length: one details line in an 80 px band, which is what almost every install
/// sees).
#[test]
fn no_caption_line_is_drawn_where_the_tile_below_is() {
    for policy in [BOTH_LINES, TITLE_ONLY, crate::caption_policy::CaptionPolicy::DEFAULT] {
        assert_captions_clear_of_the_tile_below(policy, &WALK);
    }
}

/// **A caption line stops at the DRAWN top of a tile that is a let-go in a held walk.** The same rule
/// as above on a walk whose presses outrun the let-gos: the tile directly below a line is a released
/// card still shrinking from its pop (drawn above its rest top, over the caption), and the grid holds
/// several let-gos at once (each card released by a held key keeps its own spring to the landing). The
/// line reads that tile's scale from the one resolution the draw paints with
/// (`Grid::rise_below` -> `drawn_rect` -> `pop_in`), so a resolution that left the let-gos out would
/// stop the line at the tile's REST top and put it under the tile's pop.
#[test]
fn no_caption_line_is_drawn_where_a_tile_letting_go_below_it_is() {
    for policy in [BOTH_LINES, TITLE_ONLY, crate::caption_policy::CaptionPolicy::DEFAULT] {
        let over_a_let_go = assert_captions_clear_of_the_tile_below(policy, &HELD_WALK);
        assert!(over_a_let_go > 0, "{policy:?}: the held walk never left a let-go under a caption line: it proves nothing");
    }
}

/// The stored DEFAULT's one details line comes in once and stays in: the room holds its box only
/// part-way through the move, and its alpha never falls while the card it hangs under is arriving (a
/// Down, and an Up, where the released card under it is still letting go of its pop).
#[test]
fn the_default_policy_brings_its_line_in_monotonically() {
    let mut r = captioned_grid(6, Some(crate::caption_policy::CaptionPolicy::DEFAULT));
    // (the focus the move lands on, the card whose line arrives)
    for (to, arriving) in [(106u32, 6usize), (100, 0)] {
        r.land_focus(to, By::Dir);
        let mut alphas = Vec::new();
        for _ in 0..90 {
            r.run(1);
            let cx = r.cx();
            let f = DrawFrame::new(&cx, Painter::root());
            let line = r.sect.caption_lines_probe(&f, f.painter, &r.src).into_iter()
                .filter(|l| l.0 == arriving).map(|l| l.3).fold(0.0, f32::max);
            alphas.push(line);
        }
        assert!(alphas.iter().any(|&a| a > 0.0), "move to {to}: the arriving line never came in: {alphas:?}");
        assert!(alphas.windows(2).all(|w| w[1] >= w[0] - 0.0001),
            "move to {to}: the arriving line dimmed after it came in: {alphas:?}");
        assert!(*alphas.last().unwrap() > 0.999, "move to {to}: it did not settle whole: {alphas:?}");
    }
}

/// What a walk of focus between two stacked shelves saw.
struct ShelfWalk {
    /// the worst px a drawn caption line of the shelf above reaches below the top of the next
    /// shelf's heading AS DRAWN (the lift of the shelf below folded in), and the frame it was on
    under_heading: (f32, String),
    /// the same against the top of the next shelf's focused card as drawn (its pop)
    under_card: (f32, String),
    /// per frame, the alphas of the lines the shelf above drew
    alphas: Vec<Vec<f32>>,
}

impl ShelfWalk {
    /// Caption lines (with ink) the shelf above drew over the whole walk.
    fn lines(&self) -> usize {
        self.alphas.iter().map(Vec::len).sum()
    }
}

/// **Two stacked shelves, the real [`Shelf::draw`] decision, mid-move.** Shelf `A` (cards 100..) sits
/// over shelf `B` (cards 200..) the way Home stacks rows: B's tiles stand
/// `ROW_PITCH_FIXED + A.under_band()` below A's, B's heading `TITLE_DY + CARD_DY` above its tiles
/// less its live `heading_lift()`, which A is handed as `SectionFrame::below_rise` (what the layout
/// owners do). Focus goes Down from A to B at frame 0, back Up at frame 60; each frame A's caption
/// lines are read from the label its focused card is drawn with ([`Shelf::caption_lines_probe`])
/// and measured against B's drawn heading and B's focused card's drawn top. `rise` is what A is
/// told about the heading below (a test passes 0 to show what an owner that supplies nothing gets).
fn shelf_walk(policy: Option<crate::caption_policy::CaptionPolicy>, rise: impl Fn(&Shelf) -> f32) -> ShelfWalk {
    use crate::consts::{CARD_DY, TITLE_DY};
    let mut a = Rig::<Shelf>::new(6);
    let mut b = Rig::<Shelf>::new(6);
    b.src = Cards::new((200..206).collect(), false);
    for r in [&mut a, &mut b] {
        r.sect.set_caption_policy(policy);
        shape_captions(&mut r.src, policy);
    }
    let to = |a: &mut Rig<Shelf>, b: &mut Rig<Shelf>, elem: u32, by: By| {
        a.land_focus(elem, by);
        b.land_focus(elem, by);
    };
    to(&mut a, &mut b, 100, By::Restore);
    a.run(60);
    b.run(60);
    let mut walk = ShelfWalk { under_heading: (f32::MIN, String::new()), under_card: (f32::MIN, String::new()), alphas: Vec::new() };
    for frame in 0..130 {
        match frame {
            0 => to(&mut a, &mut b, 200, By::Dir),
            60 => to(&mut a, &mut b, 100, By::Dir),
            _ => {}
        }
        a.run(1);
        b.run(1);
        let (y_a, lift) = (SHELF_AT.y, b.sect.heading_lift());
        let y_b = y_a + super::ROW_PITCH_FIXED + a.sect.under_band();
        let at_a = SectionFrame { below_rise: rise(&b.sect), ..SectionFrame::new(y_a, Rect::FULL) };
        let at_b = SectionFrame::new(y_b, Rect::FULL);
        let heading = y_b - CARD_DY - TITLE_DY - lift;
        let (cx_a, cx_b) = (a.cx(), b.cx());
        let card = Shelf::place(&b.sect, &cx_b, &b.src, &200, at_b, At::Drawn).expect("B's first card").rect.y;
        let f = DrawFrame::new(&cx_a, Painter::root());
        let lines = a.sect.caption_lines_probe(&f, &a.src, at_a);
        walk.alphas.push(lines.iter().map(|l| l.3).collect());
        for (_, _, bottom, alpha) in lines {
            for (limit, name, worst) in [(heading, "heading", &mut walk.under_heading), (card, "focused card", &mut walk.under_card)] {
                let over = bottom - limit;
                if over > worst.0 {
                    *worst = (over, format!("frame {frame}: a caption line (alpha {alpha:.2}) ends {over:.1} px below the \
                        top of the next shelf's {name} as drawn ({limit:.1}; its heading lift {lift:.1})"));
                }
            }
        }
    }
    walk
}

/// **A shelf's caption line is never drawn where the next shelf's heading is drawn.** At the 112 and
/// 80 px bands, through a Down onto the shelf below and an Up back: the heading of the shelf that
/// takes the focus rises by its lift while the departing caption is still on screen, so the room is
/// measured to that drawn heading (`SectionFrame::below_rise`), not to where the heading rests.
#[test]
fn a_shelf_caption_line_never_reaches_the_next_shelfs_heading_or_popped_card() {
    for (name, policy) in [("112", BOTH_LINES), ("80, title", TITLE_ONLY), ("80, default", crate::caption_policy::CaptionPolicy::DEFAULT)] {
        let walk = shelf_walk(Some(policy), Shelf::heading_lift);
        assert!(walk.lines() > 0, "{name}: the walk saw no caption line");
        assert!(walk.under_heading.0 <= 0.0, "{name}: {}", walk.under_heading.1);
        assert!(walk.under_card.0 <= 0.0, "{name}: {}", walk.under_card.1);
    }
}

/// An owner that tells the shelf nothing about the heading below (`below_rise` 0) is the break this
/// guards: the walk must see a line under the lifted heading, or it measures nothing.
#[test]
fn a_shelf_told_nothing_about_the_heading_below_does_reach_it() {
    let walk = shelf_walk(Some(BOTH_LINES), |_| 0.0);
    assert!(walk.under_heading.0 > 0.0, "the heading lift never took any room: {}", walk.under_heading.1);
}

/// A band of 0 px (title off, no details) draws nothing at all, though the source still offers a
/// block: the room never holds a line's box, and a block folded over no lines is not a pass.
#[test]
fn a_shelf_with_a_band_of_zero_draws_no_caption_line() {
    let none = crate::caption_policy::CaptionPolicy { title: false, details: crate::caption_policy::DetailsKind::None };
    let mut a = Rig::<Shelf>::new(6);
    a.sect.set_caption_policy(Some(none));
    a.src.captioned = true;
    assert!(a.src.label(0).lines().iter().flatten().count() > 0, "the source offers a block");
    a.land_focus(100, By::Restore);
    for frame in 0..100 {
        a.run(1);
        let cx = a.cx();
        let f = DrawFrame::new(&cx, Painter::root());
        let lines = a.sect.caption_lines_probe(&f, &a.src, SHELF_AT);
        assert!(lines.is_empty(), "frame {frame}: a band of 0 px drew {lines:?}");
    }
}

/// The legacy 114 px band (a shelf that follows no policy: a landscape still row, a headshot row)
/// keeps its two-line block: the title comes in before the details line under it, both are whole
/// once the band is open, and neither reaches the heading below on the way.
#[test]
fn the_legacy_band_reveals_its_whole_block_and_keeps_clear_of_the_heading() {
    let walk = shelf_walk(None, Shelf::heading_lift);
    assert!(walk.lines() > 0, "the walk saw no caption line");
    assert!(walk.under_heading.0 <= 0.0, "{}", walk.under_heading.1);
    assert!(walk.under_card.0 <= 0.0, "{}", walk.under_card.1);
    let first_two = walk.alphas.iter().position(|a| a.len() == 2).expect("both lines are on screen at some frame");
    let first_one = walk.alphas.iter().position(|a| a.len() == 1).expect("the title arrives alone first");
    assert!(first_one < first_two, "the title ({first_one}) comes in before the details line ({first_two})");
    let settled = walk.alphas.last().unwrap(); // 70 frames after the Up back onto A
    assert!(settled.len() == 2 && settled.iter().all(|&a| a > 0.999), "an open band shows its whole block: {settled:?}");
}

/// **A caption is the band's content: it follows the band it sits in.** A Down closes the
/// departing row's band and opens the arriving row's over the same few frames, and the caption of
/// each follows ITS band, never a cut and never a caption over the row under it
/// (`worst_caption_overlap`):
///
/// * the departing caption was cut in one frame (full on the frame before, gone on the next) while
///   its gap was still closing, because only the FOCUSED card drew one;
/// * the arriving caption stayed off until the gap was nearly open (a curve solved against the
///   next shelf's heading, which a grid has no use for), so for the middle of the move no caption
///   was on screen at all.
///
/// Read through `caption_probe`, the same decision `draw` paints, so the culling a row meets as it
/// scrolls off the top is part of what is asserted.
fn assert_captions_follow_their_bands(trace: &[Vec<RowFrame>]) {
    for (row, series) in trace.iter().enumerate() {
        for (t, w) in series.windows(2).enumerate() {
            let jump = (w[1].alpha - w[0].alpha).abs();
            assert!(jump <= 0.5, "row {row}: the caption jumped {jump:.2} between frames {t} and {} \
                (alpha {:.2} -> {:.2}, band open {:.2} -> {:.2})", t + 1, w[0].alpha, w[1].alpha, w[0].open, w[1].open);
        }
        for (t, w) in series.iter().enumerate() {
            assert!(w.open < 0.5 || w.alpha > 0.0,
                "row {row} frame {t}: its band is {:.2} open and its caption is not on screen", w.open);
            assert!(w.open > 0.0 || w.alpha == 0.0, "row {row} frame {t}: a caption under a closed band");
            assert!(w.alpha <= w.open + 0.001, "row {row} frame {t}: the caption {:.2} is past its band's {:.2}", w.alpha, w.open);
        }
    }
    // never a frame mid-move with no caption anywhere while any gap is more than half open
    for t in 0..trace[0].len() {
        let open = trace.iter().map(|s| s[t].open).fold(0.0, f32::max);
        let shown = trace.iter().map(|s| s[t].alpha).fold(0.0, f32::max);
        assert!(open <= 0.5 || shown > 0.0, "frame {t}: a gap is {open:.2} open and no row shows a caption");
    }
}

#[test]
fn a_down_fades_the_departing_caption_out_and_the_arriving_one_in_with_their_bands() {
    let trace = caption_trace(4, &[0], 60);
    // the band the move leaves was open and showing its whole caption; it ends closed and bare
    assert!(trace[0][0].open > 0.99 && trace[0][0].alpha > 0.99, "the move starts from an open band: {:?}", trace[0][0]);
    assert!(trace[1].last().unwrap().alpha > 0.99, "the arriving caption settles whole: {:?}", trace[1].last());
    assert!(trace[0].last().unwrap().alpha < 0.01, "the departing caption is gone once its band is closed");
    assert_captions_follow_their_bands(&trace);
}

#[test]
fn a_fast_repeat_of_downs_never_cuts_a_caption() {
    // six Downs seven frames apart, a row each: bands are opened and closed before any settles
    let trace = caption_trace(8, &[0, 7, 14, 21, 28, 35], 90);
    assert_captions_follow_their_bands(&trace);
}

/// **One hand-over, from the key frame.** The departing caption starts to leave on the key frame
/// (a line's alpha is never past its band's progress, and the room under it, less the pop of the
/// card rising into it, takes it away as that room goes), and the arriving one is drawn only once
/// the room under it holds the title's box (`card_row::line_reveal_for`): never before the band has
/// opened that far, never past the band's own progress, and it DOES arrive.
#[test]
fn a_down_hands_the_caption_over_from_the_key_frame_and_the_arrival_waits_for_its_room() {
    let trace = caption_trace(4, &[0], 30);
    // sample 1 is the frame the key lands on: the departing caption is already leaving
    assert!(trace[0][1].alpha < 0.99, "the hand-over does not start on the key frame: out {:?}", trace[0][1]);
    let title_box = TileLabel::title("t").lines()[0].unwrap().bottom;
    // the arriving caption does arrive (a walk that never drew it would pass every check below)
    assert!(trace[1].iter().any(|f| f.alpha > 0.0), "the arriving caption never appeared: {:?}", trace[1]);
    for t in 0..=29 {
        let (out, inn) = (trace[0][t], trace[1][t]);
        assert!(out.alpha <= out.open + 0.001,
            "frame {t}: the departing caption {:.3} is past its band's progress {:.3}", out.alpha, out.open);
        if inn.alpha > 0.0 {
            assert!(crate::card_row::room_for(112.0, inn.open, 0.0) >= title_box,
                "frame {t}: the arriving caption is drawn with the band {:.2} open, before the room holds its title", inn.open);
        }
    }
}

/// **A shelf follows the Under posters policy for the room it reserves and the caption it
/// reveals, and a shelf that follows none keeps the legacy two-line band.** The focused shelf's band
/// settles at `under_band_for(band_h(policy), 1)` (0 / 80 / 112 px of caption: the room below the
/// cards), the row below it is carried down by `band_open_for`, and the caption block is fully
/// revealed at the end of the open; a `None` policy is the legacy 114 px band, as a landscape still row
/// or a headshot row (Search's Episode and Person rows, Detail's Extras) reads it.
#[test]
fn a_shelf_reserves_and_reveals_the_band_its_caption_policy_asks_for() {
    use crate::caption_policy::{band_h, CaptionPolicy, DetailsKind};
    let settled_under_band = |policy: Option<CaptionPolicy>| {
        let mut r = Rig::<Shelf>::new(6);
        r.sect.set_caption_policy(policy);
        r.land_focus(101, By::Dir);
        r.run(240);
        // the block the policy puts under a card: its title and/or its details line
        shape_captions(&mut r.src, policy);
        let whole = r.src.label(0).revealed(r.sect.band_h(), r.sect.band_expand(), 0.0);
        (r.sect.under_band(), r.sect.band_h(), whole.lines().iter().flatten().map(|l| l.alpha).fold(1.0, f32::min))
    };
    for (title, details, tall) in [(false, DetailsKind::None, 0.0), (false, DetailsKind::Genre, 80.0),
        (true, DetailsKind::YearLength, 112.0), (true, DetailsKind::None, 80.0)] {
        let policy = CaptionPolicy { title, details };
        let (under, h, reveal) = settled_under_band(Some(policy));
        assert_eq!(h, tall, "title {title}, details {details:?}: the shelf's band is the policy's");
        assert_eq!(h, band_h(&policy));
        assert!((under - crate::card_row::under_band_for(h, 1.0)).abs() < 0.01, "title {title}, details {details:?}: reserved {under}");
        assert!(reveal > 0.999, "the open band shows its whole caption block ({reveal})");
        assert!((crate::card_row::band_open_for(h) - (under - crate::card_row::LABEL_BAND_COLLAPSED)).abs() < 0.01,
            "the row below is carried down by what the band opened");
    }
    let (under, h, reveal) = settled_under_band(None);
    assert_eq!(h, crate::card_row::LEGACY_BAND_H, "a shelf that follows no policy keeps the legacy band");
    assert!(reveal > 0.999, "the open legacy band shows its whole two-line block ({reveal})");
    assert!((under - crate::card_row::under_band(1.0)).abs() < 0.01);
    assert!((crate::card_row::band_open_for(h) - crate::card_row::BAND_OPEN).abs() < 0.01, "the legacy band opens by BAND_OPEN");
}

/// A grid given `None` is back on the legacy band (a grid of landscape stills is off the policy): it
/// reserves [`card_row::LEGACY_BAND_H`], not the stored setting's 80 / 40 px.
#[test]
fn a_grid_taken_off_the_policy_keeps_the_legacy_band() {
    use crate::caption_policy::{band_h, CaptionPolicy, DetailsKind};
    let mut r = Rig::<Grid>::new(2 * poster_grid::COLS);
    let silent = CaptionPolicy { title: false, details: DetailsKind::None };
    r.sect.set_caption_policy(Some(silent));
    assert_eq!(r.sect.band_h(), band_h(&silent));
    r.sect.set_caption_policy(None);
    assert_eq!(r.sect.caption_policy(), None);
    assert_eq!(r.sect.band_h(), crate::card_row::LEGACY_BAND_H);
}

#[test]
fn the_hold_band_speaks_once_per_slack_and_never_leaves_the_view_short_of_its_margin() {
    use super::{hold_band, HoldBand, HOLD_SLACK};
    let len = 40;
    let mut band = HoldBand::default();
    let mut sent = Vec::new();
    // a walk down the whole page, one row at a time, three rows in view
    for top in 0..len - 2 {
        let seen = (top, top + 2);
        if let Some(b) = band.next(seen, len) { sent.push(b); }
        let held = sent.last().copied().expect("the first step sent a band");
        let need = hold_band(seen, len);
        assert!(held.0 <= need.0 && held.1 >= need.1, "row {top}: {held:?} must cover {need:?}");
    }
    // the band reaches HOLD_MARGIN + HOLD_SLACK past the view and is sent again once the view needs
    // more than the margin of it: one band per HOLD_SLACK + 1 rows
    assert!(sent.len() <= (len - 2).div_ceil(HOLD_SLACK + 1), "one band per {} rows: {sent:?}", HOLD_SLACK + 1);
    assert_eq!(sent.last().map(|b| b.1), Some(len - 1), "the walk reached the last row");
    // the same rows again: nothing to say; a forgotten band is said again
    assert_eq!(band.next((len - 3, len - 1), len), None);
    band.forget();
    assert!(band.next((len - 3, len - 1), len).is_some());
    // a list that grew under the view needs rows the old band did not reach
    assert!(band.next((len - 3, len - 1), len + 5).is_some());
}

/// A store as a screen sees it: it keeps the band it was last sent as the shared cut leaves it.
#[derive(Default)]
struct KeptByTheStore {
    held: Option<(usize, usize)>,
    sends: usize,
}

impl KeptByTheStore {
    /// One frame of a screen: tell the band what is in view and send what it answers.
    fn frame(&mut self, band: &mut super::HoldBand, seen: (usize, usize), len: usize) {
        if let Some(sent) = band.next(seen, len) {
            self.held = Some(plx_base::hold::clamp_hold(sent));
            self.sends += 1;
        }
    }

    fn covers(&self, (lo, hi): (usize, usize)) -> bool {
        self.held.is_some_and(|(held_lo, held_hi)| held_lo <= lo && held_hi >= hi)
    }
}

/// The Recommended page's BACK to the top from rows 20..23 of a 40-row page: the focused row 0 is
/// folded into the rows in view (`seen = (0, 23)`), a span no store keeps, and the view then glides
/// up. Whatever the store dropped of rows 0..4 on the first frame has to be asked for again as the
/// span shortens, so that when the glide arrives (and on every frame the rows in view and their
/// margin fit the store at all) they are held. Judged against the band it asked for, the band called
/// rows 0..4 covered, said nothing more, and the top of the page stayed placeholders.
#[test]
fn a_back_to_the_top_from_deep_in_the_page_gets_the_top_rows_held_by_the_time_it_arrives() {
    use super::{hold_band, HoldBand};
    let len = 40;
    let (mut band, mut store) = (HoldBand::default(), KeptByTheStore::default());
    for top in 18..=22 {
        store.frame(&mut band, (top, top + 3), len);
        assert!(store.covers(hold_band((top, top + 3), len)), "standing at row {top}");
    }
    // BACK: focus seats on row 0; the view glides from row 20 to the top, a row a frame
    let mut fitting_frames = 0;
    for top in (0..=20).rev() {
        let seen = (0, top + 3);
        store.frame(&mut band, seen, len);
        let need = hold_band(seen, len);
        if need.1 - need.0 < plx_base::hold::HOLD_ROWS {
            fitting_frames += 1;
            assert!(store.covers(need), "view at row {top}: the store kept {:?}, the view needs {need:?}", store.held);
        }
    }
    assert!(fitting_frames > 5, "premise: the glide has frames the store can hold the view on ({fitting_frames})");
    assert!(store.covers(hold_band((0, 3), len)), "the glide arrived on rows the store kept {:?}", store.held);
    // at rest there is nothing more to say
    let sends = store.sends;
    for _ in 0..10 { store.frame(&mut band, (0, 3), len); }
    assert_eq!(store.sends, sends, "a rested view does not resend");
}

/// A span no band could hold: the first send is all the store can do, and the same view on the
/// following frames sends nothing, not the same cut every frame.
#[test]
fn a_view_too_tall_for_the_store_is_not_sent_the_same_cut_again() {
    use super::HoldBand;
    let len = 60;
    let (mut band, mut store) = (HoldBand::default(), KeptByTheStore::default());
    for _ in 0..20 { store.frame(&mut band, (0, 30), len); }
    assert_eq!(store.sends, 1, "one band, kept as far as the store can");
    assert_eq!(store.held.map(|(lo, hi)| hi - lo + 1), Some(plx_base::hold::HOLD_ROWS));
    // a taller view that the store would keep the same way is still not sent again
    for _ in 0..20 { store.frame(&mut band, (0, 31), len); }
    assert_eq!(store.sends, 1, "the same cut, once");
    // one that moves the cut is
    store.frame(&mut band, (10, 50), len);
    assert_eq!(store.sends, 2);
}

/// The rule the hold band relies on, stated against the constants: the rows in view plus the margin
/// and slack each side fit what the store keeps (also a compile-time assertion beside the constants).
#[test]
fn the_rows_in_view_and_their_hold_reach_fit_the_rows_the_store_keeps() {
    use super::{HOLD_MARGIN, HOLD_SLACK, ROWS_IN_VIEW_MAX};
    assert!(ROWS_IN_VIEW_MAX + 2 * (HOLD_MARGIN + HOLD_SLACK) <= plx_base::hold::HOLD_ROWS);
    // and the premise of ROWS_IN_VIEW_MAX: the fixed pitch of a row, whole rows that fit, plus two cut
    let pitch = crate::card_row::ROW_PITCH_FIXED;
    assert!(ROWS_IN_VIEW_MAX as f32 * pitch >= crate::consts::SCR_H + pitch, "{ROWS_IN_VIEW_MAX} rows of {pitch}px");
}

#[test]
fn value_lines_draw_on_every_visible_card_and_space_the_rows_for_them() {
    let _guard = plx_base::testlock::serial();
    let mut r = Rig::<Grid>::new(30);
    r.src.sort_caption = true;
    r.sect.set_value_line(true);
    // Focus in row 2: rows 0 and 1 are collapsed, so their pitch is the card and the 60 px row gap.
    r.land_focus(112, By::Dir); r.run(120);
    let cx = r.cx();
    let y = |r: &Rig<Grid>, elem: u32| r.sect.place(&r.cx(), &r.src, &elem, At::SpringTarget).unwrap().rest_rect.y;
    assert!((y(&r, 106) - y(&r, 100) - (GRID.style.h + crate::card_row::SORT_VALUE_ROW_GAP)).abs() < 0.5,
        "collapsed rows are 60 px apart while values show");
    // The focused row's band is open: the row below it sits SORT_VALUE_OPEN further down.
    assert!((y(&r, 119) - y(&r, 113) - (GRID.style.h + crate::card_row::SORT_VALUE_ROW_GAP + crate::card_row::SORT_VALUE_OPEN)).abs() < 0.5,
        "the focused row opens 26 px");
    let runs = crate::draw_census::capture_runs(|| {
        let mut frame = DrawFrame::new(&cx, Painter::recording());
        let p = frame.painter;
        r.sect.draw(&mut frame, p, &r.src);
    });
    let values: Vec<_> = runs.iter().filter(|run| run.text == "100%" && run.ink[3] > 0.5).collect();
    assert!(values.iter().all(|run| run.size == crate::theme::size::CAPTION), "never smaller type");
    assert_eq!(values.iter().filter(|run| run.bold).count(), 1, "exactly the lifted card's value is bold");
    assert!(values.iter().filter(|run| !run.bold).count() >= 5, "every other visible card shows its value at rest");
    assert!(runs.iter().all(|run| !run.text.starts_with("Title")), "no identity block while values show");
    r.sect.set_value_line(false);
    r.run(120);
    let cx = r.cx(); let f = DrawFrame::new(&cx, Painter::recording());
    assert!(r.sect.caption_probe(&f, f.painter, &r.src).iter().all(|(index, _)| *index == 12),
        "turning values off restores the focused-only caption band");
}

/// The simulator measured the settled focused value at 217/255 where TEXT_PRIMARY is 247: the grid
/// drew every value BEFORE every tile, and the lifted card's drop shadow (folded into its composite
/// quad, 64 px past the poster) fell over the line. A value is painted over every tile.
#[test]
fn every_value_is_painted_after_every_tile_so_no_shadow_falls_on_it() {
    let _guard = plx_base::testlock::serial();
    let mut r = Rig::<Grid>::new(30);
    r.src.sort_caption = true;
    r.sect.set_value_line(true);
    r.land_focus(112, By::Dir); r.run(120);
    let cx = r.cx();
    let draw = || {
        let mut frame = DrawFrame::new(&cx, Painter::recording());
        let p = frame.painter;
        r.sect.draw(&mut frame, p, &r.src);
    };
    let census = crate::draw_census::capture(draw);
    let last_tile = census.iter().rposition(|(tag, _)| *tag == TILE_PAINTED).expect("tiles are painted");
    let first_value = census.iter().position(|(tag, _)| *tag == 100).expect("values are painted");
    assert!(first_value > last_tile, "a value line goes over every tile (tile {last_tile}, value {first_value})");
    // and the settled focused card's value is the whole of TEXT_PRIMARY, bold
    let runs = crate::draw_census::capture_runs(draw);
    let bold: Vec<_> = runs.iter().filter(|run| run.bold && run.ink[0] > 0.0).collect();
    assert_eq!(bold.len(), 1, "exactly the lifted card's value is bold");
    // (the pop spring settles within a millionth of its target: an alpha well inside one 8-bit step)
    let ink = bold[0].ink;
    assert_eq!(ink[..3], crate::theme::TEXT_PRIMARY[..3]);
    assert!((ink[3] - 1.0).abs() < 0.5 / 255.0, "{ink:?}");
    // the opener's redraw of the lifted card keeps the same order
    let hold = crate::draw_census::capture(|| {
        let mut frame = DrawFrame::new(&cx, Painter::recording());
        r.sect.redraw(&mut frame, &r.src, Some(r.key(112)));
    });
    let tile = hold.iter().position(|(tag, _)| *tag == TILE_PAINTED).expect("the lifted tile");
    assert!(hold.iter().position(|(tag, _)| *tag == 100).is_some_and(|value| value > tile), "{hold:?}");
}

#[test]
fn a_value_line_drops_its_plus_then_its_mark_and_never_elides() {
    let _guard = plx_base::testlock::serial();
    let mark: &'static [crate::widgets::MarkLayer] = &[(crate::icons::Icon::Crowd, crate::theme::RATING_AUDIENCE)];
    let rect = Rect::new(100.0, 100.0, 250.0, 375.0);
    let drawn = |style: &crate::card_row::RowStyle, value: &crate::card_row::SortValue, focused: bool| {
        crate::draw_census::capture_runs(|| crate::card_row::draw_sort_value(
            Painter::recording(), rect, if focused { style.focus_scale } else { 1.0 }, style, value, &FixtureMeasure, focused))
    };
    let value = crate::card_row::SortValue { text: "4K".into(), plus: true, mark, missing: false };
    let wide = crate::card_row::RowStyle { w: 250.0, ..GRID.style };
    // FixtureMeasure is size/2 per byte: "4K" is 24 px, "4K" + NBSP + "+1" is 24 + 12 + 24 + ...
    let full = format!("4K{}", crate::card_row::SORT_VALUE_PLUS);
    assert!(drawn(&wide, &value, false).iter().any(|run| run.text == full), "fits whole when there is room");
    let tight = crate::card_row::RowStyle { w: 100.0, ..GRID.style };
    let runs = drawn(&tight, &value, false);
    assert!(runs.iter().any(|run| run.text == "4K") && !runs.iter().any(|run| run.text.contains("+1")), "+1 goes first");
    // The glyphs the same call hands the painter: a dropped mark is the absence of its icon, which the words alone cannot show.
    let marks = |style: &crate::card_row::RowStyle, value: &crate::card_row::SortValue| {
        crate::draw_census::capture_icons(|| crate::card_row::draw_sort_value(
            Painter::recording(), rect, 1.0, style, value, &FixtureMeasure, false))
    };
    assert_eq!(marks(&wide, &value).len(), mark.len(), "with room for everything the mark is drawn");
    assert_eq!(marks(&tight, &value).len(), mark.len(), "the mark outlasts the +1 (24 px of words and 52 of mark hang fit 100)");
    let bare = crate::card_row::RowStyle { w: 60.0, ..GRID.style };
    let marked = crate::card_row::SortValue { text: "4K".into(), mark, ..Default::default() };
    let runs = drawn(&bare, &marked, false);
    assert!(runs.iter().any(|run| run.text == "4K"), "the words are drawn whole at 60 px");
    assert!(marks(&bare, &marked).is_empty(), "the mark goes before the words are cut: no icon is painted at 60 px (24 + 52 > 60)");
    let long = crate::card_row::SortValue { text: "Yesterday".into(), ..Default::default() };
    let runs = drawn(&wide, &long, false);
    assert!(runs.iter().all(|run| !run.text.contains('\u{2026}')) && runs.iter().any(|run| run.text == "Yesterday"));
    // The number stays on the poster axis whatever hangs beside it.
    let axis = rect.cx();
    let at = drawn(&wide, &crate::card_row::SortValue { text: "98%".into(), mark, ..Default::default() }, false);
    assert!(at.iter().all(|run| run.align == 1 && (run.at.x - axis).abs() < 0.01), "centred on the axis: {at:?}");
    // A missing value is an en dash at 50% white, regular even on the focused card.
    let dash = crate::card_row::SortValue { text: "\u{2013}".into(), missing: true, ..Default::default() };
    for focused in [false, true] {
        let runs = drawn(&wide, &dash, focused);
        assert!(runs.iter().all(|run| !run.bold) && runs.iter().any(|run| run.text == "\u{2013}"), "focused={focused}");
    }
}

#[test]
fn a_value_line_grid_borrows_labels_only_after_culling() {
    struct CachedCards {
        cards: Cards,
        label: TileLabel,
        reads: std::cell::RefCell<Vec<usize>>,
    }
    impl CardSource<FixtureHost> for CachedCards {
        fn len(&self) -> usize { self.cards.len() }
        fn elem(&self, i: usize) -> u32 { self.cards.elem(i) }
        fn index_of(&self, e: &u32) -> Option<usize> { self.cards.index_of(e) }
        fn art(&self, _: usize) -> Art<'_> { Art::Poster(None) }
        fn label(&self, _: usize) -> TileLabel { panic!("persistent painting must borrow the cached label") }
        fn with_label<R>(&self, i: usize, read: impl FnOnce(&TileLabel) -> R) -> R {
            self.reads.borrow_mut().push(i);
            read(&self.label)
        }
    }
    let _guard = plx_base::testlock::serial();
    let sources = std::rc::Rc::new(std::cell::RefCell::new(crate::frame::backdrop::Sources::default()));
    sources.borrow_mut().begin(vec![]);
    let _discovery = crate::frame::backdrop::discover(sources);
    let view = FixtureView::default();
    let cx = cx9(&view, 0, 1.0, None);
    let painter = Painter::root();
    let frame = DrawFrame::new(&cx, painter);
    let mut grid = Grid::new(ENTRY, GRID.columns(GRID.cols, GRID.style, 5000.0));
    grid.set_value_line(true);
    let src = CachedCards { cards: Cards::new((100..130).collect(), false),
        label: TileLabel { value: Some(crate::card_row::SortValue { text: "98%".into(), ..Default::default() }), ..TileLabel::titled("Cached", "1997") },
        reads: Default::default() };
    assert!(grid.caption_probe(&frame, painter, &src).is_empty());
    assert!(src.reads.borrow().is_empty(), "culled tiles must not ask for words");
    grid.set_columns(GRID.cols, GRID.style, crate::consts::MARGIN_X);
    assert!(!grid.caption_probe(&frame, painter, &src).is_empty());
    assert!(!src.reads.borrow().is_empty(), "visible captions use the borrowed path");
}
