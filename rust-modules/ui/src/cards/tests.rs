//! Tier 1 of the card conformance suite: the seven `conformance` cases run against [`Shelf`] and
//! [`Grid`] on `FixtureHost`, plus focused tests for the pop rule, the geometry, the events and
//! idle. No expected-failure list: every case passes on both components.
use super::conformance::{self, CardHarness, Landing, Mount, Nb, Outcome};
use super::{CardEvent, CardSource, Grid, GridSpec, PageEdge, ScrollMode, SectionFrame, Shelf};
use crate::card_row::{RowStyle, TileLabel};
use crate::fixture::{FixtureHost, FixtureMeasure, FixtureView, FixtureViews};
use crate::poster_grid;
use crate::screen::{At, By, Dir, DrawFrame, Placed, ScreenEvent, Step};
use crate::widgets::Art;
use crate::{Painter, Rect};
use plx_machine::machine::{
    Canon, Cx, Effects, EntryId, FocusKey, FocusRead, InputOwner, InstanceId, MachineId, PressId, PressRead, Tick,
};
use plx_machine::present::Present;

const ENTRY: EntryId = EntryId(9);
const SHELF_AT: SectionFrame = SectionFrame { y: 300.0, clip: Rect::FULL };
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
    /// The screen rect each card was really painted at, as the overlay hook saw it.
    drawn: std::cell::RefCell<Vec<(u32, Rect)>>,
    /// Cards from this index on are not interned yet: `elem` answers the shared header id `0`
    /// (`Members::elem`'s `unwrap_or(HEADER_ELEM)`) and `index_of` knows no such element.
    unindexed_from: usize,
}

impl Cards {
    fn new(elems: Vec<u32>, more: bool) -> Self {
        Self { elems, more, drawn: Default::default(), unindexed_from: usize::MAX }
    }
}

impl CardSource<FixtureHost> for Cards {
    fn len(&self) -> usize {
        self.elems.len()
    }
    fn elem(&self, i: usize) -> u32 {
        if i >= self.unindexed_from { 0 } else { self.elems[i] }
    }
    fn index_of(&self, e: &u32) -> Option<usize> {
        self.elems.iter().take(self.unindexed_from).position(|x| x == e)
    }
    fn art(&self, _i: usize) -> Art<'_> {
        Art::Poster(None)
    }
    fn label(&self, _i: usize) -> TileLabel {
        TileLabel::default()
    }
    fn more(&self) -> bool {
        self.more
    }
    fn overlay(&self, p: Painter, i: usize, tile: &super::Tile, _measure: &dyn plx_machine::machine::Measure) {
        self.drawn.borrow_mut().push((self.elems[i], p.to_screen(tile.rect).1));
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
fn the_seven_conformance_cases_pass_on_shelf_and_grid() {
    let table: [(&'static str, Mount); 3] =
        [("shelf", mount_shelf), ("grid", mount_grid), ("external", mount_external_grid)];
    let matrix = conformance::run_all(&table);
    for (s, c, o) in &matrix {
        eprintln!("{s:6} {c:18} {o:?}");
    }
    assert_eq!(matrix.len(), 21);
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
            out.push(SectionSpec::new(Sec::Items, Kind::Grid { spec: COLS_SPEC }, GroupId(0)));
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
        let ids = |base: u32, n: usize| (0..n as u32).map(|i| base + i).collect::<Vec<_>>();
        let mut r = PageRig {
            stack: Stack::new(ENTRY),
            page: Page { row: Cards::new(ids(100, row), false), items: Cards::new(ids(1000, items), false), revision: 1, status: false, pending: false, aside: false, tall: false },
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
        fn shelf_foot(&self, k: Msec) -> f32 { if k == Msec::Shelf { self.foot } else { 0.0 } }
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

/// A source whose screen shows only part of the shelf's scroll (Home's row sweeps in with its dive).
struct Swept(Cards, f32);

impl CardSource<FixtureHost> for Swept {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn elem(&self, i: usize) -> u32 {
        self.0.elem(i)
    }
    fn index_of(&self, e: &u32) -> Option<usize> {
        self.0.index_of(e)
    }
    fn art(&self, i: usize) -> Art<'_> {
        self.0.art(i)
    }
    fn label(&self, i: usize) -> TileLabel {
        self.0.label(i)
    }
    fn overlay(&self, p: Painter, i: usize, tile: &super::Tile, m: &dyn plx_machine::machine::Measure) {
        self.0.overlay(p, i, tile, m)
    }
    fn sweep(&self) -> f32 {
        self.1
    }
}

/// `CardSource::sweep` scales the offset the cards are drawn, placed and registered at, all three
/// the same, and leaves the spring's own scroll alone.
#[test]
fn a_swept_source_shows_a_fraction_of_the_scroll_everywhere() {
    let mut r = settled::<Shelf>(30);
    r.land_focus(120, By::Dir);
    r.run(300);
    let scroll = r.sect.scroll();
    assert!(scroll > 400.0, "the row has an offset to sweep: {scroll}");
    let half = Swept(Cards::new(r.src.elems.clone(), false), 0.5);
    let whole = Swept(Cards::new(r.src.elems.clone(), false), 1.0);
    let x = |src: &Swept| {
        let cx = r.cx();
        r.sect.place(&cx, src, &105, SHELF_AT, At::Drawn).unwrap().rest_rect.x
    };
    assert!((x(&half) - x(&whole) - scroll * 0.5).abs() < 0.01, "placed at half the offset");
    let drawn = |src: &Swept| {
        let cx = r.cx();
        let f = DrawFrame::new(&cx, Painter::recording());
        src.0.drawn.borrow_mut().clear();
        r.sect.paint(&f, f.painter, src, SHELF_AT);
        src.0.drawn.take()
    };
    let (dh, dw) = (drawn(&half), drawn(&whole));
    let at = |d: &[(u32, Rect)], e: u32| d.iter().find(|&&(x, _)| x == e).map(|&(_, r)| r.x);
    let (h, w) = (at(&dh, 120).unwrap(), at(&dw, 120).unwrap());
    assert!((h - w - scroll * 0.5).abs() < 0.01, "the focused card is drawn at half the offset: {h} vs {w}");
    let cx = r.cx();
    let mut f = DrawFrame::new(&cx, Painter::root());
    let p = f.painter;
    r.sect.record_stops(&mut f, p, &half, SHELF_AT);
    let first = f.stops().first().expect("a card is on the swept axis");
    let stop = first.rest_rect.x;
    let placed = r.sect.place(&cx, &half, &first.key.elem, SHELF_AT, At::Drawn).unwrap().rest_rect.x;
    assert!((stop - placed).abs() < 0.01, "the stop is where the draw puts the card");
    assert!((r.sect.scroll() - scroll).abs() < f32::EPSILON, "the spring itself is untouched");
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
