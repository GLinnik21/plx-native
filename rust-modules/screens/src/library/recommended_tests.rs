//! The Recommended page: its harness (shared with `shelf_paging_tests.rs`) and the tests of what is
//! specific to a page that is only hub rows: server order, empty hubs dropped, the first seat, BACK,
//! and a read-out that fits in every shipped language.
use super::*;
use plx_machine::machine::{FocusRead, Host, InputOwner, PressRead, Tick};
use plx_ui::fixture::{FixtureArg, FixtureMeasure};
use plx_ui::focus::{FocusEngine, Outcome};

pub(super) struct HostFixture;
#[derive(Clone, Copy)]
pub(super) struct Views<'a> {
    listing: plx_data::stores::browse::ListingView<'a>,
    directory: plx_data::stores::browse::DirectoryView<'a>,
    hubs: plx_data::stores::browse::HubsView<'a>,
}
impl Host for HostFixture {
    type Arg = FixtureArg;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = Views<'a>;
    type Init = FixtureArg;
    type Memory = PageMemory;
}
impl LibraryLike for HostFixture {
    fn listing<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::ListingView<'a> { cx.views.listing }
    fn directory<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::DirectoryView<'a> { cx.views.directory }
    fn section_hubs<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::HubsView<'a> { cx.views.hubs }
}
pub(super) const ENTRY: EntryId = EntryId(81);
pub(super) const OWNER: InputOwner = InputOwner::Entry(ENTRY);

pub(super) struct Fixture {
    pub(super) stores: Option<plx_data::stores::Stores>,
    pub(super) listing: plx_data::stores::browse::ListingSnapshot,
    pub(super) directory: plx_data::stores::browse::DirectorySnapshot,
    pub(super) hubs: plx_data::stores::browse::HubsSnapshot,
    measure: FixtureMeasure,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let sid = plx_plex::plex::ServerId::from_raw(0);
        let listing = plx_data::browse::view::ListingSnapshot::fixture(sid, Vec::new(), Vec::new());
        let directory = plx_data::browse::view::DirectorySnapshot::fixture(1, 0, vec![
            plx_data::browse::view::SectionView { sid: Some(sid), key: 1, kind: SecKind::Movie,
                row: plx_data::browse::SrcRow { section: 0, title: "Cinema".into(), pinned: true, current: true, ..Default::default() } }]);
        Self { stores: None, listing, directory, hubs: plx_data::stores::browse::HubsSnapshot::empty_for_test(), measure: FixtureMeasure }
    }
    /// A library whose server published `titles` as hubs (`count` cards each), in that order.
    pub(super) fn shelves(titles: &[&str], count: usize) -> Self {
        let mut fixture = Self::new();
        let stores = plx_data::stores::Stores::default();
        stores.browse.borrow_mut().seed_two_source_table_for_test();
        stores.capture_browse(&mut fixture.directory);
        stores.browse_run_for_test(BrowseCmd::SetCur(0));
        {
            let mut browse = stores.browse.borrow_mut();
            browse.seed_items_for_test(120);
            browse.seed_shelves_for_test(0, titles, count);
        }
        let publication = stores.capture_browse(&mut fixture.directory);
        fixture.listing = publication.listing;
        fixture.hubs = publication.section_hubs;
        fixture.stores = Some(stores);
        fixture
    }
    /// A library whose server published the hubs `rows` name (id, key, title), `per_row` cards each,
    /// every one pageable (so the store may give its cards up), the first window of a listing of 500.
    pub(super) fn named_shelves(rows: &[(String, String, String)], per_row: usize) -> Self {
        let mut fixture = Self::new();
        let stores = plx_data::stores::Stores::default();
        stores.browse.borrow_mut().seed_two_source_table_for_test();
        stores.capture_browse(&mut fixture.directory);
        stores.browse_run_for_test(BrowseCmd::SetCur(0));
        {
            let mut browse = stores.browse.borrow_mut();
            browse.seed_items_for_test(120);
            let named: Vec<(&str, &str, &str)> = rows.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str())).collect();
            browse.seed_named_shelves_for_test(0, &named, per_row);
            browse.seed_paging_for_test(0, 500);
        }
        fixture.stores = Some(stores);
        fixture.republish();
        fixture
    }
    /// Capture what the store publishes now, as the dispatcher does between frames.
    pub(super) fn republish(&mut self) {
        let publication = self.stores.as_ref().expect("a fixture with stores").capture_browse(&mut self.directory);
        self.listing = publication.listing;
        self.hubs = publication.section_hubs;
    }
    pub(super) fn cx(&self, focus: Option<FocusKey<u32>>) -> Cx<'_, HostFixture> {
        Cx { views: Views { listing: self.listing.view(), directory: self.directory.view(), hubs: self.hubs.view() },
            tick: Tick::default(), measure: &self.measure, focus: FocusRead { current: focus, ..Default::default() },
            press: PressRead::default(), owner: OWNER }
    }
    pub(super) fn screen(&self) -> RecommendedScreen {
        let mut page = RecommendedScreen::new(ENTRY, InstanceId(19), SecKind::Movie);
        page.sync(&self.cx(None));
        page
    }
}

pub(super) fn deliver(page: &mut RecommendedScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture, event: ScreenEvent<HostFixture>) -> usize {
    let mut output = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.step(&event, &fixture.cx(engine.current(OWNER)),
        &mut Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present));
    let mut remembers = 0;
    for effect in output {
        if let Fx::Remember { group, elem } = effect.fx {
            remembers += 1;
            engine.remember_projected(ENTRY, group, elem);
        }
    }
    remembers
}

pub(super) fn direction(page: &mut RecommendedScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture, dir: Dir) -> usize {
    let mut links = Vec::new();
    <RecommendedScreen as Screen<HostFixture>>::links(page, &mut links);
    let outcome = engine.move_dir(OWNER, page, &links, dir, &fixture.cx(engine.current(OWNER)));
    if let Outcome::Moved { from, to, by } = outcome {
        deliver(page, engine, fixture, ScreenEvent::FocusMoved { from, to, by })
    } else { 0 }
}

fn effects_of(page: &mut RecommendedScreen, fixture: &Fixture, engine: &FocusEngine<u32>, event: ScreenEvent<HostFixture>) -> Vec<Fx<HostFixture>> {
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.step(&event, &fixture.cx(engine.current(OWNER)),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    out.into_iter().map(|effect| effect.fx).collect()
}

#[test]
fn the_page_lists_the_servers_hubs_in_admin_order() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::shelves(&["movie.inprogress.1", "movie.recentlyadded.1", "movie.genre.1"], 6);
    let page = fixture.screen();
    let published: Vec<_> = fixture.hubs.view().shelves().iter().map(|shelf| shelf.id.clone()).collect();
    let shown: Vec<_> = page.shelves.iter().map(|row| row.id.clone()).collect();
    assert_eq!(shown, published, "the page's row i is the store's shelf i");
    assert_eq!(page.readout, Readout::Rows);
}

#[test]
fn a_library_with_no_hubs_reads_empty_not_loading() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::shelves(&[], 0);
    let page = fixture.screen();
    assert!(page.shelves.is_empty());
    assert_eq!(page.readout, Readout::Empty);
}

#[test]
fn the_first_tick_seats_focus_on_the_first_row_once() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::shelves(&["movie.inprogress.1", "movie.recentlyadded.1"], 6);
    let mut page = fixture.screen();
    let engine = FocusEngine::new();
    let seats = |fx: &[Fx<HostFixture>]| fx.iter().filter(|fx| matches!(fx,
        Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { .. }))))).count();
    let first = effects_of(&mut page, &fixture, &engine, ScreenEvent::Tick(Tick { ms: 9000, dt_us: 16_667 }));
    assert_eq!(seats(&first), 1);
    let second = effects_of(&mut page, &fixture, &engine, ScreenEvent::Tick(Tick { ms: 9017, dt_us: 16_667 }));
    assert_eq!(seats(&second), 0, "the seat is made once");
}

/// BACK pops an unscrolled page whatever has focus: the return state captured at the Recommended
/// press puts focus on that control again. (A scrolled page returns to its top first.)
#[test]
fn back_pops_the_page() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::shelves(&["movie.inprogress.1"], 6);
    let mut page = fixture.screen();
    let engine = FocusEngine::new();
    let back = ScreenEvent::Input(plx_machine::machine::InputEvent {
        at: Tick::default(), source: plx_machine::machine::Source::Script,
        kind: plx_machine::machine::InputKind::Key { key: plx_machine::machine::Key::Back, sym: 0, wcode: 0,
            edge: plx_machine::machine::Edge::Down, at_edge: false },
    });
    let fx = effects_of(&mut page, &fixture, &engine, back);
    assert!(fx.iter().any(|fx| matches!(fx, Fx::App(AppFx::Content(ContentReq::Back)))), "BACK is the page's own pop");
}

/// The page's only string fits its band in every shipped language: one line within the panel.
#[test]
fn the_empty_read_out_fits_in_every_shipped_language() {
    use plx_base::fontcov::advances::ShippedMeasure;
    let _guard = plx_base::testlock::serial();
    for language in plx_platform::i18n::SHIPPED {
        let _lang = plx_platform::i18n::language_on_this_thread_for_test(language);
        let text = plx_platform::i18n::msg::recommended_page_empty();
        assert!(!text.is_empty(), "{}: empty string", language.tag());
        let width = plx_machine::machine::Measure::width(&ShippedMeasure, &std::ffi::CString::new(text).unwrap(), plx_ui::theme::size::BODY, false);
        assert!(width <= plx_ui::widgets::StatusOverlay::REASON_W * 1.6,
            "{}: {text:?} is {width:.0} px wide, past what the read-out wraps in two lines", language.tag());
    }
}

/// Mixed, non-representable pitches: the run's prefix sums must be the very bits a straight
/// `pitches[..i].iter().sum()` gives, or every shelf origin would drift a ulp from before.
#[test]
fn shelf_run_origins_are_bit_identical_to_the_straight_sum() {
    let pitches: Vec<f32> = (0..170).map(|i| 301.37 + (i % 7) as f32 * 0.113 + (i % 3) as f32 * 59.9).collect();
    let run = ShelfRun::of(&pitches);
    for i in 0..=pitches.len() + 2 {
        let straight = pitches[..i.min(pitches.len())].iter().sum::<f32>();
        // (an empty prefix sums to -0.0 in std and starts at 0.0 here: equal)
        assert_eq!(run.origin(i), straight, "origin {i}");
        if i > 0 { assert_eq!(run.origin(i).to_bits(), straight.to_bits(), "origin {i}"); }
    }
    assert_eq!(run.height().to_bits(), pitches.iter().sum::<f32>().to_bits());
    assert_eq!(run.pitch(5), pitches[5]);
    assert_eq!(run.pitch(999), plx_ui::consts::ROW_PITCH);
}

/// **A shelf keeps the shared air under its band**: the pitch is the fixed part every shelf column is
/// built from plus the band, so the heading of the next shelf rests where `card_row::room_for` counts
/// it, whatever the band's size and expansion, and a landscape row is the same pitch under a shorter tile.
#[test]
fn shelf_pitch_keeps_the_shared_air() {
    for band in [ui_cards::LEGACY_BAND_H, 0.0, 80.0, 112.0] {
        for expanded in [0.0, 0.5, 1.0] {
            let want = ui_cards::ROW_PITCH_FIXED + ui_cards::under_band_for(band, expanded);
            assert!((shelf_pitch(false, band, expanded) - want).abs() < 1e-3, "band {band} expanded {expanded}");
            let shorter = RowStyle::HOME.h - RowStyle::EPISODE.h;
            assert!((shelf_pitch(true, band, expanded) - (want - shorter)).abs() < 1e-3, "landscape, band {band} expanded {expanded}");
        }
    }
    assert_eq!(shelf_pitch(false, ui_cards::LEGACY_BAND_H, 1.0), plx_ui::consts::ROW_PITCH);
}

#[test]
fn shelf_run_window_holds_exactly_the_shelves_meeting_the_span() {
    let pitches: Vec<f32> = (0..50).map(|i| 300.0 + (i % 4) as f32 * 40.0).collect();
    let run = ShelfRun::of(&pitches);
    assert_eq!(ShelfRun::default().window(0.0, 1.0e6), 0..0);
    assert_eq!(run.window(-1.0e6, -500.0), 0..0, "above the run");
    assert_eq!(run.window(1.0e9, 2.0e9), 50..50, "below the run");
    for (top, bottom) in [(0.0, 1080.0), (5000.0, 6080.0), (123.4, 456.7), (14000.0, 20000.0)] {
        let window = run.window(top, bottom);
        for i in 0..50 {
            let meets = run.origin(i) < bottom && run.origin(i) + pitches[i] > top;
            if meets { assert!(window.contains(&i), "{i} meets {top}..{bottom} but is outside {window:?}"); }
            if !window.contains(&i) { assert!(!meets); }
        }
        assert!(bottom - top > 2000.0 || window.len() < 8, "{window:?}");
    }
}

/// **The page's title row is the Collections page's crumb**, "<Library> › Recommended": the library
/// name is the link back at 65% white and regular weight, the page name stands on the selected
/// plate; the bold "Recommended · Movies" heading is gone.
#[test]
fn the_title_row_is_the_crumb_with_the_library_as_the_link_back() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::shelves(&["movie.inprogress.1", "movie.recentlyadded.1"], 6);
    let page = fixture.screen();
    let engine = FocusEngine::new();
    let cx = fixture.cx(engine.current(OWNER));
    let runs = plx_ui::draw_census::capture_runs(|| {
        let frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
        page.draw_title(&frame);
    });
    let at = |text: &str| runs.iter().find(|r| r.text == text).unwrap_or_else(|| panic!("{text:?} drawn: {:?}", runs.iter().map(|r| &r.text).collect::<Vec<_>>()));
    let library = at("Movies");
    assert_eq!(library.ink, plx_ui::theme::CONTROL_TEXT_REST, "the link is 65% white");
    assert!(!library.bold);
    assert!((library.at.x - plx_ui::page_header::TEXT_X0).abs() < 0.5, "the root starts at x 96, as on the Collections page");
    let page_name = at(plx_platform::i18n::msg::library_header_recommended());
    assert!(!page_name.bold, "no bold heading");
    assert!(page_name.at.x > library.at.x + library.at.w, "the page name follows the library and the chevron");
    assert!(runs.iter().all(|r| !r.text.contains('\u{b7}') || r.text.len() > 3), "the old 'Recommended · Movies' joined heading is gone");
}

/// The crumb's root is the one stop above the rows: UP from the first row reaches it, DOWN returns,
/// and OK on it is the page's Back.
#[test]
fn up_from_the_first_row_reaches_the_crumb_and_ok_on_it_goes_back() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::shelves(&["movie.inprogress.1", "movie.recentlyadded.1"], 6);
    let mut page = fixture.screen();
    let mut engine = FocusEngine::new();
    let first = page.shelves[0].elems[0];
    engine.set(OWNER, FocusKey { entry: ENTRY, elem: first }, Some(page.shelves[0].group), plx_ui::screen::By::Restore);
    direction(&mut page, &mut engine, &fixture, Dir::Up);
    assert_eq!(engine.current(OWNER), Some(FocusKey { entry: ENTRY, elem: CRUMB_ELEM }), "UP lands on the crumb root");
    let fx = effects_of(&mut page, &fixture, &engine, ScreenEvent::Activate(CRUMB_ELEM));
    assert!(fx.iter().any(|fx| matches!(fx, Fx::App(AppFx::Content(ContentReq::Back)))), "OK on the link is Back");
    direction(&mut page, &mut engine, &fixture, Dir::Down);
    assert_eq!(engine.current(OWNER).map(|k| k.elem), Some(first), "DOWN returns to the first row");
}

#[test]
fn the_first_shelf_starts_at_the_grid_top_under_the_title_row() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::shelves(&["movie.inprogress.1"], 6);
    let page = fixture.screen();
    assert_eq!(shelf_y(&page.run, 0, 0.0) - TITLE_DY, 244.0, "the first heading starts on the library page's grid top");
}

/// **The first shelf's heading never reaches into the title row.** The breadcrumb row is the header
/// row (148..204); the heading's drawn text must start clear of it (the grid convention: content at
/// y 244), on the Recommended page at rest.
#[test]
fn the_first_shelf_heading_starts_clear_of_the_title_row() {
    use plx_ui::screen::Screen;
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::shelves(&["movie.inprogress.1", "movie.recentlyadded.1"], 6);
    let mut page = fixture.screen();
    let engine = FocusEngine::new();
    let cx = fixture.cx(engine.current(OWNER));
    let sources = std::rc::Rc::new(std::cell::RefCell::new(plx_ui::frame::backdrop::Sources::default()));
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = plx_ui::frame::backdrop::discover(sources.clone());
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::root());
        page.draw(&mut frame);
    }
    let paints: Vec<(String, [f32; 4])> = sources.borrow().paints.iter().map(|p| (p.kind().to_owned(), p.placed())).collect();
    let title_row_bottom = plx_ui::page_header::TOP + plx_ui::page_header::HEIGHT;
    let heading = paints.iter().filter(|(k, r)| k == "text" && r[1] > title_row_bottom - 20.0)
        .map(|(_, r)| r[1]).fold(f32::INFINITY, f32::min);
    // Clear of the row (204) AND its focus pill stood out by its outset, with air to spare.
    let pill_bottom = title_row_bottom + plx_ui::widgets::PILL_FOCUS_OUTSET;
    assert!(heading >= title_row_bottom + 24.0 && heading >= pill_bottom + 24.0, "the first heading's text starts at {heading}");
    assert_eq!(heading, 244.0, "…on the grid's content line");
}

/// **The crumb row is built once per key, not once per question.** The page asks for the row in its
/// focus questions, its placement, its motion and its paint, many times a frame; it is built when
/// the library's name (or the locale) changes and held otherwise, by the one rule the Library's and
/// the Watchlist's rows are held by (`plx_ui::page_header::SharedRow`).
#[test]
fn the_crumb_row_is_built_once_per_key_not_once_per_question() {
    use plx_ui::screen::{At, Focusable};
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::shelves(&["movie.inprogress.1"], 6);
    let page = fixture.screen();
    let ask = |fixture: &Fixture| {
        let cx = fixture.cx(None);
        let frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
        page.draw_title(&frame);
        Focusable::<HostFixture>::place(&page, &CRUMB_ELEM, &cx, At::Drawn).expect("the crumb root is a stop");
        Focusable::<HostFixture>::reconcile(&page, FocusKey { entry: ENTRY, elem: CRUMB_ELEM }, &cx);
    };
    ask(&fixture);
    let built = page.probe_header_builds();
    assert_eq!(built, 1, "the first question built the row");
    for _ in 0..60 { ask(&fixture); }
    assert_eq!(page.probe_header_builds(), built, "sixty frames of questions and paint build no row");
    let sid = plx_plex::plex::ServerId::from_raw(0);
    fixture.directory = plx_data::browse::view::DirectorySnapshot::fixture(1, 0, vec![
        plx_data::browse::view::SectionView { sid: Some(sid), key: 1, kind: SecKind::Movie,
            row: plx_data::browse::SrcRow { section: 0, title: "Renamed".into(), pinned: true, current: true, ..Default::default() } }]);
    ask(&fixture);
    assert_eq!(page.probe_header_builds(), built + 1, "the library's new name builds the row once");
    for _ in 0..10 { ask(&fixture); }
    assert_eq!(page.probe_header_builds(), built + 1, "and the questions after it build none");
}
