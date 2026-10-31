//! The Tier 2 card-conformance harness for the Watchlist page (`cards_conformance_tests.rs`). A
//! sibling of `page`, so it reads the page through the `pub(crate)` test accessors the screen
//! exposes; it also builds the rows the page tests share.
use plx_data::watchlist::model::{Kind, ServerMatch, TileState, WatchRow};
use plx_machine::machine::{
    Cx, Effects, EntryId, FocusKey, FocusRead, Host, InputOwner, Machine, PressRead, Tick,
};
use plx_ui::cards::conformance::{CardHarness, Landing, Nb};
use plx_ui::fixture::FixtureMeasure;
use plx_ui::screen::{At, By, Dir, Focusable, Placed, Screen, ScreenEvent, Step};
use plx_ui::Rect;

use super::options::WatchlistOptionsState;
use super::page::{WatchTile, WatchlistIn, WatchlistScreen};
use crate::registry::{AppFx, AppMsg, PageMemory};

/// A host with no stores: the page reads its rows from itself.
pub(crate) struct WatchHost;
impl Host for WatchHost {
    type Arg = super::super::family::SettingsPage;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = ();
    type Init = super::super::family::NoInit;
    type Memory = PageMemory;
}

pub(crate) const ENTRY: EntryId = EntryId(9);

/// A row titled `<i>`, a movie of 2000 + i.
pub(crate) fn row(i: usize) -> WatchRow {
    WatchRow {
        guid: format!("plex://movie/g{i}"),
        hex: [0; 12],
        kind: Kind::Movie,
        title: i.to_string(),
        title_sort: String::new(),
        year: Some(2000 + i as i32),
        added_at: None,
        release: None,
        rating: None,
        released: true,
        order: i as u32,
        thumb: String::new(),
    }
}

pub(crate) fn copy(i: usize) -> ServerMatch {
    ServerMatch::new(plx_plex::plex::ServerId::from_raw(0), 100 + i as i64)
}

/// An available, matched tile.
pub(crate) fn tile(i: usize) -> WatchTile {
    WatchTile { row: row(i), state: TileState::Available, copy: Some(copy(i)) }
}

pub(crate) fn tile_in(i: usize, state: TileState) -> WatchTile {
    let copy = matches!(state, TileState::Available | TileState::ComingSoonMatched | TileState::ServerOffline)
        .then(|| copy(i));
    WatchTile { row: row(i), state, copy }
}

pub(crate) fn tiles(n: usize) -> Vec<WatchTile> {
    (0..n).map(tile).collect()
}

/// The page, its focus and its clock, stepped the way the host steps a mounted screen.
pub(crate) struct Harness {
    pub(crate) screen: WatchlistScreen,
    pub(crate) focus: Option<FocusKey<u32>>,
    press: f32,
    ms: u32,
    n: usize,
}

pub(crate) fn mount(n: usize) -> Box<dyn CardHarness> {
    Box::new(Harness::new(tiles(n)))
}

impl Harness {
    pub(crate) fn new(rows: Vec<WatchTile>) -> Self {
        let n = rows.len();
        let screen = WatchlistScreen::new(rows, WatchlistOptionsState::default()).at_entry(ENTRY);
        let mut h = Self { screen, focus: None, press: 1.0, ms: 0, n };
        // the stack lays its sections out on the first event it sees
        h.step(ScreenEvent::Tick(Tick { ms: 0, dt_us: 16_667 }));
        h
    }

    pub(crate) fn cx(&self) -> Cx<'_, WatchHost> {
        Cx {
            views: (),
            tick: Tick { ms: self.ms, dt_us: 16_667 },
            measure: &FixtureMeasure,
            press: PressRead { scale: self.press, owner: self.focus, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() },
            owner: InputOwner::Entry(ENTRY),
        }
    }

    /// Draw the screen once through a recording painter. The draw runs inside a discovery walk
    /// (`draw_discovered`) because the page's frame clear would otherwise reach GL.
    pub(crate) fn draw(&mut self) {
        self.draw_discovered();
    }

    /// Draw the screen once inside a backdrop DISCOVERY walk with a recording painter: every
    /// `Painter` primitive is recorded away, so what reaches the returned sources is only what the
    /// page laid down outside its painter (`gfx::frame_clear`). No GL call is made.
    pub(crate) fn draw_discovered(&mut self) -> std::rc::Rc<std::cell::RefCell<plx_gfx::gfx::backdrop::Sources>> {
        use plx_ui::screen::Screen;
        let sources = std::rc::Rc::new(std::cell::RefCell::new(plx_gfx::gfx::backdrop::Sources::default()));
        let cx = Cx {
            views: (),
            tick: Tick { ms: self.ms, dt_us: 16_667 },
            measure: &FixtureMeasure,
            press: PressRead { scale: self.press, owner: self.focus, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() },
            owner: InputOwner::Entry(ENTRY),
        };
        {
            let _discovery = plx_ui::frame::backdrop::discover(sources.clone());
            let mut f = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
            Screen::<WatchHost>::draw(&mut self.screen, &mut f);
        }
        sources
    }

    /// The paints one descriptive draw declares (`frame::backdrop::discover`: primitives are
    /// recorded, GL is never reached), glass and rects included, as the Library's tests read a page.
    pub(crate) fn draw_paints(&mut self) -> Vec<std::rc::Rc<plx_gfx::gfx::backdrop::Paint>> {
        let sources = std::rc::Rc::new(std::cell::RefCell::new(plx_gfx::gfx::backdrop::Sources::default()));
        sources.borrow_mut().begin(vec![]);
        let cx = Cx {
            views: (),
            tick: Tick { ms: self.ms, dt_us: 16_667 },
            measure: &FixtureMeasure,
            press: PressRead { scale: self.press, owner: self.focus, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() },
            owner: InputOwner::Entry(ENTRY),
        };
        {
            let _discovery = plx_ui::frame::backdrop::discover(sources.clone());
            let mut f = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::root());
            Screen::<WatchHost>::draw(&mut self.screen, &mut f);
        }
        let paints = sources.borrow().paints.clone();
        paints
    }

    /// Every text run the page's draw declares, as `(string, screen rect)` (`plx_ui::draw_census::capture_text`).
    pub(crate) fn draw_texts(&mut self) -> Vec<(String, Rect)> {
        let sources = std::rc::Rc::new(std::cell::RefCell::new(plx_gfx::gfx::backdrop::Sources::default()));
        let cx = Cx {
            views: (),
            tick: Tick { ms: self.ms, dt_us: 16_667 },
            measure: &FixtureMeasure,
            press: PressRead { scale: self.press, owner: self.focus, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() },
            owner: InputOwner::Entry(ENTRY),
        };
        let screen = &mut self.screen;
        plx_ui::draw_census::capture_text(|| {
            let _discovery = plx_ui::frame::backdrop::discover(sources.clone());
            let mut f = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
            Screen::<WatchHost>::draw(screen, &mut f);
        })
    }

    /// Every text run the page's own pass declares with its style and ink, inside a discovery walk
    /// like [`Self::draw`] (the page's frame clear would otherwise reach GL).
    pub(crate) fn draw_runs(&mut self) -> Vec<plx_ui::draw_census::Run> {
        let sources = std::rc::Rc::new(std::cell::RefCell::new(plx_gfx::gfx::backdrop::Sources::default()));
        let cx = Cx {
            views: (),
            tick: Tick { ms: self.ms, dt_us: 16_667 },
            measure: &FixtureMeasure,
            press: PressRead { scale: self.press, owner: self.focus, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() },
            owner: InputOwner::Entry(ENTRY),
        };
        let screen = &mut self.screen;
        plx_ui::draw_census::capture_runs(|| {
            let _discovery = plx_ui::frame::backdrop::discover(sources.clone());
            let mut f = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
            Screen::<WatchHost>::draw(screen, &mut f);
        })
    }

    /// Every primitive the page's draw declares, as `(tag, screen rect)` (`plx_ui::draw_census`),
    /// drawn with `lifted` as the element a surface over the page has lifted out of it
    /// (`DrawFrame::lifted`, the dispatcher's `lifted_opener`).
    pub(crate) fn draw_census(&mut self, lifted: Option<FocusKey<u32>>) -> Vec<(u64, Rect)> {
        let sources = std::rc::Rc::new(std::cell::RefCell::new(plx_gfx::gfx::backdrop::Sources::default()));
        let cx = Cx {
            views: (),
            tick: Tick { ms: self.ms, dt_us: 16_667 },
            measure: &FixtureMeasure,
            press: PressRead { scale: self.press, owner: self.focus, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() },
            owner: InputOwner::Entry(ENTRY),
        };
        let screen = &mut self.screen;
        plx_ui::draw_census::capture(|| {
            let _discovery = plx_ui::frame::backdrop::discover(sources.clone());
            let mut f = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
            f.lifted = lifted;
            Screen::<WatchHost>::draw(screen, &mut f);
        })
    }

    /// The opener lift's own frame for `focus` (`Screen::redraw_focused`), through a recording
    /// painter inside a discovery walk.
    pub(crate) fn redraw_lift(&mut self, focus: Option<FocusKey<u32>>) {
        use plx_ui::screen::Screen;
        let sources = std::rc::Rc::new(std::cell::RefCell::new(plx_gfx::gfx::backdrop::Sources::default()));
        let cx = self.cx();
        let _discovery = plx_ui::frame::backdrop::discover(sources);
        let mut f = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
        Screen::<WatchHost>::redraw_focused(&self.screen, &mut f, focus);
    }

    /// The opener lift's frame for `focus`, with what it declared: its primitives as `(tag, screen
    /// rect)` and its text runs with their ink. A recording painter OUTSIDE a discovery walk, which
    /// is the only place a run's ink is noted.
    pub(crate) fn lift_census(&mut self, focus: Option<FocusKey<u32>>) -> (Vec<(u64, Rect)>, Vec<plx_ui::draw_census::Run>) {
        use plx_ui::screen::Screen;
        let cx = self.cx();
        let mut f = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
        let mut prims = Vec::new();
        let runs = plx_ui::draw_census::capture_runs(|| {
            prims = plx_ui::draw_census::capture(|| Screen::<WatchHost>::redraw_focused(&self.screen, &mut f, focus));
        });
        (prims, runs)
    }

    /// Step one event; returns the effects it queued and whether the step reported page motion.
    pub(crate) fn step_fx(&mut self, ev: ScreenEvent<WatchHost>) -> (Vec<plx_machine::machine::Stamped<WatchHost>>, bool) {
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let (_, moving) = plx_machine::idle::scoped_motion(|| {
            let cx = Cx {
                views: (),
                tick: Tick { ms: self.ms, dt_us: 16_667 },
                measure: &FixtureMeasure,
                press: PressRead { scale: self.press, owner: self.focus, ..Default::default() },
                focus: FocusRead { current: self.focus, ..Default::default() },
                owner: InputOwner::Entry(ENTRY),
            };
            let mut fx = Effects::new(
                &mut out,
                plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(9)),
                &mut present,
            );
            Machine::<WatchHost>::step(&mut self.screen, &ev, &cx, &mut fx);
        });
        (out, moving || present.page_moving())
    }

    pub(crate) fn step(&mut self, ev: ScreenEvent<WatchHost>) -> bool {
        self.step_fx(ev).1
    }

    pub(crate) fn key(&self, elem: u32) -> FocusKey<u32> {
        FocusKey { entry: ENTRY, elem }
    }

    pub(crate) fn focus_index(&mut self, i: usize) {
        let elem = self.screen.elem_at(i).expect("a tile at that index");
        self.focus_elem(elem, By::Dir);
    }

    pub(crate) fn focus_elem(&mut self, elem: u32, by: By) {
        let from = self.focus;
        self.focus = Some(self.key(elem));
        self.step(ScreenEvent::FocusMoved { from, to: self.key(elem), by });
    }

    pub(crate) fn run(&mut self, frames: u32) -> bool {
        let mut moved = false;
        for _ in 0..frames {
            self.ms += 16;
            moved |= self.step(ScreenEvent::Tick(Tick { ms: self.ms, dt_us: 16_667 }));
        }
        moved
    }

    pub(crate) fn reconcile(&mut self) {
        if let Some(want) = self.focus {
            let now = Focusable::<WatchHost>::reconcile(&self.screen, want, &self.cx());
            if now != want {
                self.focus_elem(now.elem, By::Reconcile);
            }
        }
    }
}

impl CardHarness for Harness {
    fn cards(&self) -> Vec<u32> {
        (0..self.screen.tiles().len()).filter_map(|i| self.screen.elem_at(i)).collect()
    }
    fn focused(&self) -> Option<u32> {
        self.focus.map(|k| k.elem)
    }
    fn focus(&mut self, elem: u32, by: By) {
        self.focus_elem(elem, by);
    }
    fn tick(&mut self, frames: u32) -> bool {
        self.run(frames)
    }
    fn cover(&mut self) {
        self.step(ScreenEvent::Cover);
    }
    fn uncover(&mut self) {
        self.step(ScreenEvent::Uncover);
    }
    fn set_press(&mut self, scale: f32) {
        self.press = scale;
    }
    fn neighbour(&self, elem: u32, dir: Dir) -> Nb {
        match Focusable::<WatchHost>::neighbour(&self.screen, self.key(elem), dir, &self.cx()) {
            Step::Move(k) => Nb::To(k.elem),
            Step::Edge => Nb::Edge,
        }
    }
    fn place(&self, elem: u32, at: At) -> Option<Placed> {
        Focusable::<WatchHost>::place(&self.screen, &elem, &self.cx(), at)
    }
    /// The rect the grid registers as `elem`'s stop at `press` (`Grid::record_stops`, which uses
    /// the very rect `draw` paints; painting itself needs the GL context a host test lacks).
    fn drawn_rect(&self, elem: u32, press: f32) -> Option<Rect> {
        let cx = Cx { press: PressRead { scale: press, owner: self.focus, ..Default::default() }, ..self.cx() };
        let mut f = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::root());
        self.screen.record_stops(&mut f);
        f.stops().iter().find(|stop| stop.key.elem == elem).map(|stop| stop.rect)
    }
    fn scale(&self, elem: u32) -> Option<f32> {
        self.screen.scale_of(&self.cx(), &elem)
    }
    fn focus_scale(&self) -> f32 {
        plx_ui::cards::GRID_STYLE.focus_scale
    }
    fn canon(&self) -> u64 {
        plx_machine::machine::LogicalState::hash(&self.screen)
    }
    fn identity(&self, elem: u32) -> String {
        self.screen
            .index_of(elem)
            .map(|i| self.screen.tiles()[i].row.guid.clone())
            .unwrap_or_default()
    }
    fn scroll(&self) -> Option<f32> {
        Some(self.screen.scroll())
    }
    fn columns(&self) -> Option<usize> {
        Some(plx_ui::cards::GRID_COLS)
    }
    fn landing(&mut self, l: Landing) -> Result<(), &'static str> {
        let focused = self.focus.map(|k| k.elem);
        let at = focused.and_then(|e| self.screen.index_of(e)).ok_or("nothing focused")?;
        let mut rows = self.screen.tiles().to_vec();
        match l {
            Landing::Reorder => rows.swap(0, 2),
            Landing::InsertAbove => rows.insert(0, tile(9_000)),
            Landing::RemoveFocused => {
                let guid = rows[at].row.guid.clone();
                self.screen.input(WatchlistIn::Removed(guid));
                self.run(1);
                self.reconcile();
                return Ok(());
            }
        }
        self.screen.input(WatchlistIn::Rows(rows));
        self.run(1);
        self.reconcile();
        Ok(())
    }
    fn memory_roundtrip(&mut self) -> Result<Box<dyn CardHarness>, &'static str> {
        let _ = (Screen::<WatchHost>::memory_at(&self.screen, self.focus), self.n);
        Err("PageMemory has no Watchlist arm")
    }
}
