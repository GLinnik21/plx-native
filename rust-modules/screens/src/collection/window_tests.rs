//! The Collection page keeps keys for the window it can paint or step to, not for the list: a
//! 5,000-member collection walked end to end holds a bounded number of cells and keys, the window
//! sent to the store follows the grid's painted range (so a scroll that does not move focus moves
//! it), and a hole whose page lands keeps the focused slot's index, column and on-screen cell.

use super::tests::{cx, item, set, step, CollectionHost};
use super::*;
use plx_data::stores::collection::CollectionStore;
use plx_machine::machine::{FocusKey, Fx};
use plx_ui::cards::GRID_COLS;
use plx_ui::screen::Step;
use std::ops::Range;

/// The most cells the page may project at once: the painted rows with a row either side, the
/// focus row with its neighbours, the first row and a restore target's rows. Whatever the list.
const CELL_BOUND: usize = 160;

type Sent = (Range<usize>, Option<usize>, Option<usize>);

/// A collection of `all.len()` members read through the store's window protocol: each
/// `Window` the screen sends evicts and refills the pages the way a server that honours paging
/// would, and the engine's `reconcile` runs after every frame, as `Dispatcher` does.
struct Sim {
    store: CollectionStore,
    screen: CollectionScreen,
    all: Vec<PmsMovie>,
    focus: Option<FocusKey<u32>>,
    ms: u32,
    sent: Vec<Sent>,
    /// Whether a `Window` changes the store. Off, the store stays as it is (every page held), so
    /// no landing reaches the page and only the frame's own projection can key a moved view.
    reads: bool,
}

impl Sim {
    fn new(total: usize) -> Self {
        let all: Vec<PmsMovie> = (0..total).map(|i| item(&format!("m{i}"))).collect();
        let mut store = CollectionStore::default();
        store.run(CollectionCmd::Open { target: CollectionTarget { id: set(), want: PAGE_SIZE } });
        store.install_for_test(all.clone(), CollectionStatus::Ready);
        store.edit_for_test(|c| c.evict_for_test(0..3 * PAGE_SIZE, None, None));
        let mut sim = Self { store, screen: CollectionScreen::new(EntryId(9), set()), all, focus: None, ms: 0, sent: Vec::new(), reads: true };
        sim.frame();
        sim
    }

    /// Every page held and the store deaf to windows.
    fn resident(total: usize) -> Self {
        let mut sim = Self::new(total);
        let all = sim.all.clone();
        sim.store.edit_for_test(|c| c.replace_items_for_test(all));
        sim.reads = false;
        sim
    }

    fn events(&mut self, ev: ScreenEvent<CollectionHost>) {
        let out = step(&mut self.screen, ev, &cx(self.store.view(), self.focus));
        self.land(out);
    }

    /// Apply every `Window` among `out` to the store.
    fn land(&mut self, out: Vec<plx_machine::machine::Stamped<CollectionHost>>) {
        for e in out {
            if let Fx::App(AppFx::Store(_, plx_data::stores::StoreCmd::Collection(
                CollectionCmd::Window { wanted, focus, restore }))) = e.fx
            {
                self.sent.push((wanted.clone(), focus, restore));
                if !self.reads { continue; }
                let all = &self.all;
                self.store.edit_for_test(|c| { c.evict_for_test(wanted, focus, restore); c.fill_wanted_for_test(all); });
                // the landing reaches the page as the store's change notice, before any reconcile
                let landed = ScreenEvent::StoreChanged(plx_data::stores::StoreId::Collection.ord(), 0);
                let _ = step(&mut self.screen, landed, &cx(self.store.view(), self.focus));
            }
        }
    }

    /// One frame: the tick, then the engine's reconcile of the focus it holds.
    fn frame(&mut self) {
        self.ms += 16;
        self.events(ScreenEvent::Tick(Tick { ms: self.ms, dt_us: 16_667 }));
        if let Some(from) = self.focus {
            let to = Focusable::<CollectionHost>::reconcile(&self.screen, from, &cx(self.store.view(), self.focus));
            if to != from {
                self.focus = Some(to);
                self.events(ScreenEvent::FocusMoved { from: Some(from), to, by: By::Reconcile });
            }
        }
    }

    fn collection(&self) -> &Collection { self.store.view().current().unwrap() }

    fn focus_on(&mut self, to: FocusKey<u32>, by: By) {
        let from = self.focus.replace(to);
        self.events(ScreenEvent::FocusMoved { from, to, by });
    }

    /// A D-pad press: the grid's neighbour of the focus, as the engine asks it.
    fn press(&mut self, dir: Dir) -> bool {
        let Some(from) = self.focus else { return false };
        match Focusable::<CollectionHost>::neighbour(&self.screen, from, dir, &cx(self.store.view(), self.focus)) {
            Step::Move(to) => { self.focus_on(to, By::Dir); true }
            _ => false,
        }
    }

    fn painted(&self) -> Range<usize> {
        self.screen.stack.grid_window(&self.screen.page, &cx(self.store.view(), self.focus), Sec::Items).unwrap()
    }

    fn index(&self, key: FocusKey<u32>) -> Option<usize> { self.screen.item_index(self.collection(), key.elem) }

    fn centre(&self, elem: u32) -> (f32, f32) {
        let placed = Focusable::<CollectionHost>::place(&self.screen, &elem, &cx(self.store.view(), self.focus), At::Drawn)
            .expect("the cell places");
        (placed.rect.cx(), placed.rect.cy())
    }

    /// Seat focus on the first member, as the engine's first press does.
    fn seat_first(&mut self) {
        let key = FocusKey { entry: EntryId(9), elem: self.screen.elem_at(self.collection(), 0).unwrap() };
        self.focus_on(key, By::Dir);
        self.frame();
    }
}

/// Walk to the last row with Down and answer (peak cells, peak keys, frames), asserting at every
/// step that the key table and the projection are bounded and that every cell the grid can paint
/// or step to holds a key in that frame.
fn walk(total: usize) -> (usize, usize) {
    let mut sim = Sim::new(total);
    sim.seat_first();
    let (mut peak_cells, mut peak_keys) = (0, 0);
    let mut moves = 0;
    while sim.press(Dir::Down) {
        moves += 1;
        for _ in 0..2 { sim.frame(); }
        let (cells, keys) = (sim.screen.page.cells(), sim.screen.page.cards.len());
        assert!(cells <= CELL_BOUND, "{cells} cells projected for {total} members at move {moves}");
        assert!(keys <= 2 * cells + 256, "{keys} keys for a {cells}-cell window at move {moves}");
        peak_cells = peak_cells.max(cells);
        peak_keys = peak_keys.max(keys);
        for i in sim.painted() {
            assert!(sim.screen.page.cell_elem(i).is_some(), "painted cell {i} has no key at move {moves}");
        }
        let at = sim.index(sim.focus.unwrap()).expect("the focused card keeps an index");
        assert_eq!(at, moves * GRID_COLS, "focus moved one row");
        for dir in [Dir::Up, Dir::Down, Dir::Left, Dir::Right] {
            if let Step::Move(to) = Focusable::<CollectionHost>::neighbour(&sim.screen, sim.focus.unwrap(), dir,
                &cx(sim.store.view(), sim.focus)) {
                assert!(sim.index(to).is_some(), "a neighbour {dir:?} of {at} has no index");
            }
        }
    }
    assert_eq!(moves, (total - 1) / GRID_COLS, "the walk reached the last row");
    (peak_cells, peak_keys)
}

#[test]
fn a_5000_member_walk_projects_and_keys_a_bounded_window() {
    let (cells_5000, keys_5000) = walk(5000);
    let (cells_1200, keys_1200) = walk(1200);
    assert!(cells_5000 <= cells_1200 && keys_5000 <= keys_1200 + GRID_COLS,
        "bounds do not grow with the list: {cells_5000}/{keys_5000} vs {cells_1200}/{keys_1200}");
}

#[test]
fn a_scroll_that_does_not_move_focus_moves_the_window_sent_to_the_store() {
    let mut sim = Sim::new(5000);
    sim.seat_first();
    for _ in 0..4 { sim.frame(); }
    let (before, _, _) = sim.sent.last().cloned().expect("a window was sent");
    assert!(before.start < PAGE_SIZE, "at the top the window is the first pages: {before:?}");
    // The view moves 400 rows down; focus stays on the first member.
    sim.screen.stack.jump_to(ROW_PITCH * 400.0);
    sim.sent.clear();
    sim.frame();
    let painted = sim.painted();
    let (wanted, focus, _) = sim.sent.last().cloned().expect("the window moved with the view");
    assert!(painted.start > 2000, "the view is far from focus: {painted:?}");
    assert!(wanted.start <= painted.start && wanted.end >= painted.end,
        "the window {wanted:?} covers what the grid paints {painted:?}");
    assert_eq!(focus, Some(0), "the focused member is still named, so its page is kept");
    for i in painted {
        assert!(sim.screen.page.cell_elem(i).is_some(), "painted cell {i} has a key after the scroll step");
    }
}

#[test]
fn focus_on_a_hole_keeps_its_slot_column_and_cell_when_its_page_lands() {
    let mut sim = Sim::new(5000);
    // pages 50 and 51 are held; slot 3125 is on page 52, which is not
    sim.store.edit_for_test(|c| c.evict_for_test(3000..3000 + 2 * PAGE_SIZE, None, None));
    let hole = FocusKey { entry: EntryId(9), elem: hole_elem(3125) };
    sim.focus = Some(hole);
    sim.screen.stack.jump_to(ROW_PITCH * (3125 / GRID_COLS) as f32);
    sim.events(ScreenEvent::FocusMoved { from: None, to: hole, by: By::Restore });
    // the frame whose tick asks for page 52 (it lands after the tick)
    sim.ms += 16;
    sim.events(ScreenEvent::Tick(Tick { ms: sim.ms, dt_us: 16_667 }));
    assert!(sim.collection().item(3125).is_some(), "the page landed");
    assert_eq!(sim.index(hole), Some(3125), "before the screen sees the landing the hole is slot 3125");

    // the frame that sees it: the engine's reconcile of the held focus runs after the tick
    sim.ms += 16;
    sim.events(ScreenEvent::Tick(Tick { ms: sim.ms, dt_us: 16_667 }));
    let hole_cell = sim.centre(hole.elem);
    let seated = Focusable::<CollectionHost>::reconcile(&sim.screen, hole, &cx(sim.store.view(), sim.focus));
    assert_ne!(seated.elem, hole.elem, "the landed slot is seated on its member's own key");
    assert!(seated.elem < HOLE_ELEM_BASE);
    assert_eq!(sim.index(seated), Some(3125), "the same slot");
    assert_eq!(sim.index(seated).unwrap() % GRID_COLS, 3125 % GRID_COLS, "the same column");
    let member_cell = sim.centre(seated.elem);
    assert!((member_cell.0 - hole_cell.0).abs() < 0.5 && (member_cell.1 - hole_cell.1).abs() < 0.5,
        "the same cell on screen: {hole_cell:?} then {member_cell:?}");
    sim.focus_on(seated, By::Reconcile);
    sim.frame();
    assert_eq!(sim.index(seated), Some(3125));
    assert_eq!(sim.collection().item(3125).map(|m| m.rk.as_str()), Some("m3125"));
    assert_eq!(Focusable::<CollectionHost>::reconcile(&sim.screen, seated, &cx(sim.store.view(), sim.focus)), seated,
        "and it stays seated");
}

#[test]
fn focus_survives_the_eviction_and_refetch_of_its_neighbours() {
    let mut sim = Sim::new(5000);
    sim.seat_first();
    while sim.index(sim.focus.unwrap()).unwrap() < 3000 {
        assert!(sim.press(Dir::Down));
        sim.frame();
    }
    for _ in 0..180 { sim.frame(); }
    let key = sim.focus.unwrap();
    let (at, cell) = (sim.index(key), sim.centre(key.elem));
    assert_eq!(at, Some(3000));
    // every page around it goes (the focus's own page stays, as the store keeps it) ...
    sim.store.edit_for_test(|c| { c.evict_for_test(0..PAGE_SIZE, Some(3000), None); c.fill_wanted_for_test(&sim.all); });
    sim.frame();
    assert_eq!(sim.focus, Some(key), "the focus key is untouched by the eviction");
    assert_eq!(sim.index(key), at);
    // ... and are read again.
    for _ in 0..30 { sim.frame(); }
    assert_eq!(sim.focus, Some(key));
    assert_eq!(sim.index(key), at, "the same slot");
    let after = sim.centre(key.elem);
    assert!((after.0 - cell.0).abs() < 0.5 && (after.1 - cell.1).abs() < 0.5, "the same cell: {cell:?} then {after:?}");
    for neighbour in [2999usize, 3001, 3000 - GRID_COLS, 3000 + GRID_COLS] {
        assert!(sim.collection().item(neighbour).is_some(), "neighbour {neighbour} was read again");
        assert!(sim.screen.page.cell_elem(neighbour).is_some_and(|e| e < HOLE_ELEM_BASE), "and keyed");
    }
}

#[test]
fn a_far_restore_seats_its_target_once_the_target_is_projected() {
    let mut first = Sim::new(5000);
    first.seat_first();
    while first.index(first.focus.unwrap()).unwrap() < 3000 {
        assert!(first.press(Dir::Down));
        first.frame();
    }
    for _ in 0..60 { first.frame(); }
    let key = first.focus.unwrap();
    let PageMemory::Collection(memory) = Screen::<CollectionHost>::memory_at(&first.screen, Some(key)) else { panic!() };
    assert_eq!(memory.focus_index, Some(3000));
    assert!(memory.cards.len() <= 2 * CELL_BOUND + 256, "{} keys saved", memory.cards.len());

    // Back: a new page, its store showing the first pages, and the focus engine holding `key`
    let mut back = Sim::new(5000);
    back.screen = CollectionScreen::new(EntryId(9), set());
    back.focus = Some(key);
    back.events(ScreenEvent::RestoreMemory(PageMemory::Collection(memory)));
    back.events(ScreenEvent::Enter(plx_ui::screen::Enter::Restored));
    let seated = Focusable::<CollectionHost>::reconcile(&back.screen, key, &cx(back.store.view(), back.focus));
    assert_eq!(seated, key, "focus waits on the remembered member");
    for _ in 0..40 { back.frame(); }
    assert_eq!(back.focus, Some(key));
    assert_eq!(back.index(key), Some(3000), "the target is projected and seated");
    assert_eq!(back.collection().item(3000).map(|m| m.rk.as_str()), Some("m3000"));
    assert!(Focusable::<CollectionHost>::place(&back.screen, &key.elem, &cx(back.store.view(), back.focus), At::Drawn).is_some());
}

#[test]
fn every_cell_the_grid_paints_holds_a_key_in_the_frame_its_scroll_moved_the_view() {
    let mut sim = Sim::resident(5000);
    sim.seat_first();
    for _ in 0..4 { sim.frame(); }
    for rows in [300, 40, 700, 5] {
        // a view change no landing follows: the store holds every page already
        sim.screen.stack.jump_to(ROW_PITCH * rows as f32);
        sim.frame();
        let painted = sim.painted();
        assert!(painted.start > 6 || rows == 5, "the view moved: {painted:?}");
        for i in painted.clone() {
            let elem = sim.screen.page.cell_elem(i).unwrap_or_else(|| panic!("painted cell {i} of {painted:?} has no key"));
            assert!(elem < HOLE_ELEM_BASE, "and the member is held, so it is keyed");
        }
        // and every neighbour the focus can step to
        let focus = sim.focus.unwrap();
        for dir in [Dir::Up, Dir::Down, Dir::Left, Dir::Right] {
            if let Step::Move(to) = Focusable::<CollectionHost>::neighbour(&sim.screen, focus, dir, &cx(sim.store.view(), sim.focus)) {
                assert!(sim.index(to).is_some());
            }
        }
    }
}

/// A page the store lost (a failed read, an eviction) under cards in view is asked for again with
/// no key pressed: the window goes to the store again on the grid's ladder, which is what resets
/// the store's frontier budget and re-reads the page, and it stops once the page is back.
#[test]
fn a_hole_under_the_visible_cards_is_asked_for_again_without_a_key() {
    let mut sim = Sim::resident(5000);
    sim.seat_first();
    for _ in 0..60 { sim.press(Dir::Down); for _ in 0..2 { sim.frame(); } }
    for _ in 0..30 { sim.frame(); }
    let before = sim.sent.len();
    let all = sim.all.clone();
    let seen = sim.painted().start;
    let at = sim.index(sim.focus.unwrap()).unwrap();
    // the focused card's page stays (as the store keeps it); the other page in view is lost
    sim.store.edit_for_test(|c| c.evict_for_test(0..60, Some(at), None));
    assert!(seen > 100, "scrolled: {seen}");
    assert!((seen..sim.painted().end).any(|i| sim.collection().item(i).is_none()), "a page in view is gone");
    assert!(sim.collection().item(at).is_some());
    for _ in 0..60 * 12 { sim.frame(); }
    let asked = sim.sent.len() - before;
    assert!(asked >= 4, "the window was sent again {asked} times while the hole stood");
    sim.store.edit_for_test(|c| c.replace_items_for_test(all));
    for _ in 0..30 { sim.frame(); }
    let settled = sim.sent.len();
    for _ in 0..60 * 40 { sim.frame(); }
    assert_eq!(sim.sent.len(), settled, "loaded, it asks no more");
}
