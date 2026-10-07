//! The Tier 2 card-conformance harnesses for the Library page (`cards_conformance_tests.rs`): the
//! All grid (a `GridPart` of the poster wall) and the section's hub shelves (`CardRow`s above it).
//! A child module of `library` so it reads the private rect and scale helpers the screen's own
//! tests read. One `Harness` drives either card set; `Set` says which.
use super::*;
use plx_machine::machine::{FocusRead, Host, InputOwner, PressRead, Tick};
use plx_ui::cards::conformance::{CardHarness, Landing, Nb};
use plx_ui::fixture::{FixtureArg, FixtureMeasure};
use plx_ui::screen::By;

struct LibHost;
#[derive(Clone, Copy)]
struct Views<'a> {
    listing: plx_data::stores::browse::ListingView<'a>,
    directory: plx_data::stores::browse::DirectoryView<'a>,
    hubs: plx_data::stores::browse::HubsView<'a>,
}
impl Host for LibHost {
    type Arg = FixtureArg;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = Views<'a>;
    type Init = FixtureArg;
    type Memory = PageMemory;
}
impl LibraryLike for LibHost {
    fn listing<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::ListingView<'a> { cx.views.listing }
    fn directory<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::DirectoryView<'a> { cx.views.directory }
    fn section_hubs<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::HubsView<'a> { cx.views.hubs }
}

const ENTRY: EntryId = EntryId(81);
const INSTANCE: InstanceId = InstanceId(19);

#[derive(Clone, Copy, PartialEq)]
enum Set { Grid, Shelf }

pub(crate) struct Harness {
    set: Set,
    n: usize,
    rks: Vec<String>,
    listing: plx_data::stores::browse::ListingSnapshot,
    directory: plx_data::stores::browse::DirectorySnapshot,
    hubs: plx_data::stores::browse::HubsSnapshot,
    _stores: Option<plx_data::stores::Stores>,
    screen: LibraryScreen,
    focus: Option<FocusKey<u32>>,
    press: f32,
    ms: u32,
}

pub(crate) fn mount_grid(n: usize) -> Box<dyn CardHarness> { Box::new(Harness::new(Set::Grid, n)) }
pub(crate) fn mount_shelves(n: usize) -> Box<dyn CardHarness> { Box::new(Harness::new(Set::Shelf, n)) }

fn listing_of(rks: &[String]) -> plx_data::stores::browse::ListingSnapshot {
    let sid = plx_plex::plex::ServerId::from_raw(0);
    plx_data::browse::view::ListingSnapshot::fixture(sid,
        rks.iter().map(|rk| Some(plx_data::pms::PmsMovie { sid, rk: rk.clone(), title: rk.clone(), ..Default::default() })).collect(),
        vec![("A".into(), rks.len() as i64)])
}

impl Harness {
    fn new(set: Set, n: usize) -> Self {
        let rks: Vec<String> = (0..n).map(|i| format!("m{i}")).collect();
        let sid = plx_plex::plex::ServerId::from_raw(0);
        let mut directory = plx_data::browse::view::DirectorySnapshot::fixture(1, 0, vec![
            plx_data::browse::view::SectionView { sid: Some(sid), key: 1, kind: SecKind::Movie,
                row: plx_data::browse::SrcRow { section: 0, pinned: true, current: true, ..Default::default() } }]);
        let (listing, hubs, stores) = match set {
            Set::Grid => (listing_of(&rks), plx_data::stores::browse::HubsSnapshot::empty_for_test(), None),
            Set::Shelf => {
                // The same seeding `tests::Fixture::shelves` does: one hub shelf of `n` tiles
                // over a (here unused) 120-item grid.
                let stores = plx_data::stores::Stores::default();
                stores.browse.borrow_mut().seed_two_source_table_for_test();
                stores.capture_browse(&mut directory);
                stores.browse_run(BrowseCmd::SetCur(0));
                {
                    let mut browse = stores.browse.borrow_mut();
                    browse.seed_items_for_test(120);
                    browse.seed_shelves_for_test(0, &["movie.recentlyadded.1"], n);
                }
                let publication = stores.capture_browse(&mut directory);
                (publication.listing, publication.section_hubs, Some(stores))
            }
        };
        let mut h = Self { set, n, rks, listing, directory, hubs, _stores: stores,
            screen: LibraryScreen::new(ENTRY, INSTANCE, SecKind::Movie), focus: None, press: 1.0, ms: 0 };
        h.sync();
        h.screen.initial = false;
        h
    }

    fn cx(&self) -> Cx<'_, LibHost> {
        Cx { views: Views { listing: self.listing.view(), directory: self.directory.view(), hubs: self.hubs.view() },
            tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
            press: PressRead { scale: self.press, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) }
    }

    fn sync(&mut self) {
        let cx: Cx<'_, LibHost> = Cx {
            views: Views { listing: self.listing.view(), directory: self.directory.view(), hubs: self.hubs.view() },
            tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
            press: PressRead { scale: self.press, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) };
        self.screen.sync(&cx);
    }

    fn step(&mut self, ev: ScreenEvent<LibHost>) -> bool {
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let (_, moving) = plx_machine::idle::scoped_motion(|| {
            let cx: Cx<'_, LibHost> = Cx {
                views: Views { listing: self.listing.view(), directory: self.directory.view(), hubs: self.hubs.view() },
                tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
                press: PressRead { scale: self.press, ..Default::default() },
                focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) };
            let mut fx = Effects::new(&mut out, MachineId::Instance(INSTANCE), &mut present);
            Machine::<LibHost>::step(&mut self.screen, &ev, &cx, &mut fx);
        });
        moving || present.page_moving()
    }

    fn key(&self, elem: u32) -> FocusKey<u32> { FocusKey { entry: ENTRY, elem } }
    fn is_focused(&self, elem: u32) -> bool { self.focus.map(|k| k.elem) == Some(elem) }

    fn cell(&self, elem: u32) -> Option<(usize, usize)> {
        match self.set {
            Set::Grid => self.screen.pair.detail.index_of(elem).map(|i| (0, i)),
            Set::Shelf => self.screen.shelves.iter().enumerate()
                .find_map(|(r, s)| s.elems.iter().position(|e| *e == elem).map(|c| (r, c))),
        }
    }
}

impl CardHarness for Harness {
    fn cards(&self) -> Vec<u32> {
        match self.set {
            Set::Grid => self.screen.pair.detail.elems.clone(),
            Set::Shelf => self.screen.shelves.first().map(|s| s.elems.clone()).unwrap_or_default(),
        }
    }
    fn focused(&self) -> Option<u32> { self.focus.map(|k| k.elem) }
    fn focus(&mut self, elem: u32, by: By) {
        let from = self.focus;
        self.focus = Some(self.key(elem));
        self.step(ScreenEvent::FocusMoved { from, to: self.key(elem), by });
    }
    fn tick(&mut self, frames: u32) -> bool {
        let mut moved = false;
        for _ in 0..frames {
            self.ms += 16;
            moved |= self.step(ScreenEvent::Tick(Tick { ms: self.ms, dt_us: 16_667 }));
        }
        moved
    }
    fn cover(&mut self) { self.step(ScreenEvent::Cover); }
    fn uncover(&mut self) { self.step(ScreenEvent::Uncover); }
    fn set_press(&mut self, scale: f32) { self.press = scale; }
    fn neighbour(&self, elem: u32, dir: Dir) -> Nb {
        match Focusable::<LibHost>::neighbour(&self.screen, self.key(elem), dir, &self.cx()) {
            Step::Move(k) => Nb::To(k.elem),
            Step::Edge => Nb::Edge,
        }
    }
    fn place(&self, elem: u32, at: At) -> Option<Placed> {
        Focusable::<LibHost>::place(&self.screen, &elem, &self.cx(), at)
    }
    /// What the draw paints: the grid's `rect_at` (live pop x press for the focused cell), or the
    /// shelf's `shelf_rect` (live pop already in it) scaled by the press of the focused tile
    /// (`draw_shelf_tile`).
    fn drawn_rect(&self, elem: u32, press: f32) -> Option<Rect> {
        let (row, col) = self.cell(elem)?;
        let focused = self.is_focused(elem);
        Some(match self.set {
            Set::Grid => self.screen.pair.detail.rect_at(col, focused, press),
            Set::Shelf => {
                let rect = self.screen.shelf_rect(row, col);
                if focused && press > 0.0 { rect.scaled(press) } else { rect }
            }
        })
    }
    fn scale(&self, elem: u32) -> Option<f32> {
        let (row, col) = self.cell(elem)?;
        Some(match self.set {
            // `GridPart::tile_scale` is private to `parts`: its rect is the card rect times it.
            Set::Grid => self.screen.pair.detail.rect_at(col, self.is_focused(elem), 1.0).w / self.screen.layout.card_w(),
            Set::Shelf => self.screen.shelves[row].motion.scale(col),
        })
    }
    fn focus_scale(&self) -> f32 { RowStyle::HOME.focus_scale }
    fn canon(&self) -> u64 {
        let mut c = Canon::new();
        LogicalState::write(&self.screen, &mut c);
        c.finish()
    }
    fn identity(&self, elem: u32) -> String {
        let Some((row, col)) = self.cell(elem) else { return String::new() };
        match self.set {
            Set::Grid => self.listing.view().item(col).map(|m| m.rk.clone()).unwrap_or_default(),
            Set::Shelf => self.hubs.view().shelves().get(row).and_then(|s| s.items.get(col)).map(|m| m.rk.clone()).unwrap_or_default(),
        }
    }
    fn scroll(&self) -> Option<f32> {
        Some(match self.set {
            Set::Grid => self.screen.scroll.pos,
            Set::Shelf => self.screen.shelves.first()?.motion.scroll_x(),
        })
    }
    fn columns(&self) -> Option<usize> { (self.set == Set::Grid).then_some(super::layout::COLS) }
    fn landing(&mut self, l: Landing) -> Result<(), &'static str> {
        if self.set == Set::Shelf {
            return Err("no test seam edits a seeded hub shelf's items (seed_shelves_for_test mints rk from the index)");
        }
        let (_, at) = self.focus.and_then(|k| self.cell(k.elem)).ok_or("nothing focused")?;
        match l {
            Landing::Reorder => self.rks.swap(0, 2),
            Landing::InsertAbove => self.rks.insert(0, "landed".into()),
            Landing::RemoveFocused => { self.rks.remove(at); }
        }
        self.listing = listing_of(&self.rks);
        self.step(ScreenEvent::StoreChanged(plx_data::stores::StoreId::Browse.ord(), 1));
        let want = self.focus.unwrap();
        let now = Focusable::<LibHost>::reconcile(&self.screen, want, &self.cx());
        if now != want { self.focus(now.elem, By::Reconcile); }
        Ok(())
    }
    fn memory_roundtrip(&mut self) -> Result<Box<dyn CardHarness>, &'static str> {
        let PageMemory::Library(mem) = Screen::<LibHost>::memory_at(&self.screen, self.focus) else {
            return Err("memory_at did not return PageMemory::Library");
        };
        let mut fresh = Harness::new(self.set, self.n);
        fresh.screen.restore(&mem);
        fresh.sync();
        fresh.screen.initial = false;
        let want = self.focus.ok_or("nothing focused")?;
        let now = Focusable::<LibHost>::reconcile(&fresh.screen, want, &fresh.cx());
        fresh.focus(now.elem, By::Restore);
        Ok(Box::new(fresh))
    }
}
