//! Tier 1 of the card conformance suite: the seven `conformance` cases run against [`Shelf`] and
//! [`Grid`] on `FixtureHost`, plus focused tests for the pop rule, the geometry, the events and
//! idle. No expected-failure list: every case passes on both components.
use super::conformance::{self, CardHarness, Landing, Mount, Nb, Outcome};
use super::{CardEvent, CardSource, Grid, GridSpec, SectionFrame, Shelf};
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

type Cx9<'a> = Cx<'a, FixtureHost>;

struct Cards {
    elems: Vec<u32>,
    more: bool,
    /// The screen rect each card was really painted at, as the overlay hook saw it.
    drawn: std::cell::RefCell<Vec<(u32, Rect)>>,
}

impl Cards {
    fn new(elems: Vec<u32>, more: bool) -> Self {
        Self { elems, more, drawn: Default::default() }
    }
}

impl CardSource<FixtureHost> for Cards {
    fn len(&self) -> usize {
        self.elems.len()
    }
    fn elem(&self, i: usize) -> u32 {
        self.elems[i]
    }
    fn index_of(&self, e: &u32) -> Option<usize> {
        self.elems.iter().position(|x| x == e)
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

struct Rig<S: Section> {
    sect: S,
    src: Cards,
    view: FixtureView,
    focus: Option<FocusKey<u32>>,
    press: f32,
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
            ms: 0,
        }
    }

    fn cx(&self) -> Cx9<'_> {
        self.cx_with(self.press)
    }

    fn cx_with(&self, press: f32) -> Cx9<'_> {
        Cx {
            views: FixtureViews { store: &self.view },
            tick: Tick { ms: self.ms, dt_us: 16_667 },
            measure: &FixtureMeasure,
            press: PressRead { scale: press, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() },
            owner: InputOwner::Entry(ENTRY),
        }
    }

    /// Step one event through the section: the event it reported and whether the step moved.
    fn feed(&mut self, ev: ScreenEvent<FixtureHost>) -> (Option<CardEvent<u32>>, bool) {
        let mut present = Present::new();
        let mut out = Vec::new();
        let mut reported = None;
        let (_, moving) = plx_machine::idle::scoped_motion(|| {
            let cx = Cx {
                views: FixtureViews { store: &self.view },
                tick: Tick { ms: self.ms, dt_us: 16_667 },
                measure: &FixtureMeasure,
                press: PressRead { scale: self.press, ..Default::default() },
                focus: FocusRead { current: self.focus, ..Default::default() },
                owner: InputOwner::Entry(ENTRY),
            };
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

#[test]
fn the_seven_conformance_cases_pass_on_shelf_and_grid() {
    let table: [(&'static str, Mount); 2] = [("shelf", mount_shelf), ("grid", mount_grid)];
    let matrix = conformance::run_all(&table);
    for (s, c, o) in &matrix {
        eprintln!("{s:6} {c:18} {o:?}");
    }
    assert_eq!(matrix.len(), 14);
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
