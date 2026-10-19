// The Collections page as a mounted screen: it asks the store for the section's collections
// (type 18), wears the crumb instead of the library header, captions its tiles with the member
// count, and hands the section back to the full grid when left.
use super::*;
use plx_data::browse::LibraryType;
use plx_machine::machine::{Edge, InputEvent, InputKind, Key, Source, Tick};

/// What `AppArg::LibraryCollections` mounts (`registry.rs`).
fn collections_page() -> LibraryScreen {
    LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Movie).with_listing(Listing::Collections)
}

fn run(page: &mut LibraryScreen, fixture: &Fixture, event: ScreenEvent<HostFixture>) -> Vec<Fx<HostFixture>> {
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.step(&event, &fixture.cx(None), &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    out.into_iter().map(|effect| effect.fx).collect()
}

fn back_key() -> ScreenEvent<HostFixture> {
    ScreenEvent::Input(InputEvent {
        at: Tick::default(), source: Source::Script,
        kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
    })
}

/// The fixture's films listing with `n` collection rows is what the store holds once it has
/// switched to type 18.
fn as_collections(n: usize) -> Fixture {
    let mut fixture = Fixture::new();
    let sid = plx_plex::plex::ServerId::from_raw(0);
    let rows = (0..n).map(|i| plx_data::pms::PmsMovie {
        sid, rk: format!("{}", 50_001 + i), title: format!("Collection {i}"), subtype: Some("movie".into()),
        kind: plx_data::pms::KIND_COLLECTION, child_count: 24, smart: i == 0, ..Default::default()
    }).collect();
    fixture.listing = fixture.listing.with_library_type(LibraryType::Collections).with_total(n).with_page(0, rows);
    fixture
}

fn staged_type(page: &LibraryScreen) -> Option<LibraryType> {
    match page.pending.grid() { Some((_, GridAction::LibraryType(kind))) => Some(*kind), _ => None }
}

/// The defect: the page mounted the plain grid and never asked the store for type 18, so it listed
/// the same films as the page beneath it. Without the request in `follow_listing_type` this fails.
#[test]
fn mounting_the_collections_page_asks_the_store_for_the_collections_listing() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let mut page = collections_page();
    run(&mut page, &fixture, ScreenEvent::Mount);
    assert_eq!(page.listing(), Listing::Collections);
    assert_eq!(staged_type(&page), Some(LibraryType::Collections), "the section is switched to type 18");
    // Asked once: a further store change before the answer lands does not stage it again.
    page.pending.cancel();
    run(&mut page, &fixture, ScreenEvent::Mount);
    assert_eq!(staged_type(&page), None);
}

/// The page borrows the section's store listing, so on mount the store still lists the page
/// beneath it (the films). Until its own answer lands the page shows none of that: an empty grid
/// under the loading read-out, parked dark so the request commits at once, and one fade-in when
/// the collections exist. (`app::bridge::library_collections_arrival_tests` is the same rule
/// through the route dip.)
#[test]
fn the_page_shows_none_of_the_films_while_the_collections_are_in_flight() {
    let _guard = plx_base::testlock::serial();
    let films = Fixture::new();
    let mut page = collections_page();
    let mut ms = 0;
    let mut frame = |page: &mut LibraryScreen, fixture: &Fixture| {
        ms += 16;
        run(page, fixture, ScreenEvent::Tick(Tick { ms, dt_us: 16_000 }));
        page.probe_content()
    };
    run(&mut page, &films, ScreenEvent::Mount);
    let (page_alpha, grid_alpha, loading, _, tiles) = page.probe_content();
    assert_eq!((tiles, loading), (0, true), "the mount frame holds no film: alpha {}", page_alpha * grid_alpha);
    for i in 0..30 {
        let (page_alpha, grid_alpha, loading, _, tiles) = frame(&mut page, &films);
        assert!(tiles == 0 && loading, "frame {i} drew {tiles} films at alpha {}", page_alpha * grid_alpha);
    }
    let landed = as_collections(3);
    let mut shown = Vec::new();
    for _ in 0..30 {
        let (page_alpha, grid_alpha, loading, _, tiles) = frame(&mut page, &landed);
        if tiles > 0 && page_alpha * grid_alpha > 0.0 { shown.push(tiles); }
        assert!(!loading || tiles == 0, "the read-out cleared before the tiles were there");
    }
    assert!(!shown.is_empty() && shown.iter().all(|n| *n == 3), "only the collections were drawn: {shown:?}");
    let (page_alpha, grid_alpha, loading, _, tiles) = page.probe_content();
    assert!(page_alpha * grid_alpha >= 0.99 && !loading && tiles == 3);
}

#[test]
fn the_page_that_listed_collections_is_not_asked_again() {
    let _guard = plx_base::testlock::serial();
    let fixture = as_collections(3);
    let mut page = collections_page();
    run(&mut page, &fixture, ScreenEvent::Mount);
    assert_eq!(page.listed(), LibraryType::Collections);
    assert_eq!(staged_type(&page), None);
}

#[test]
fn the_header_is_the_crumb_not_the_library_header() {
    let _guard = plx_base::testlock::serial();
    let fixture = as_collections(3);
    let mut page = collections_page();
    run(&mut page, &fixture, ScreenEvent::Mount);
    assert!(page.libraries.is_empty(), "no library names on the page");
    assert_eq!(page.header_elems(), [toolbar::CRUMB_ROOT], "the crumb's root is the one stop; PMS declares one sort");
    for gone in [COLLECTIONS, RECOMMENDED, SORTFILTER, MORE] {
        assert!(!page.is_header_elem(gone), "{gone} is not on the Collections page's row");
    }
    let header = page.header.as_ref().expect("the row is built");
    assert!(header.plate(toolbar::CRUMB_ROOT).is_some_and(|plate| plate.x < 100.0 && plate.h > 0.0));
    assert!(header.plate(SORTFILTER).is_none());
}

#[test]
fn the_crumb_names_the_library_the_header_strip_names() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = as_collections(3);
    fixture.directory = plx_data::browse::view::DirectorySnapshot::fixture(1, 0, vec![
        plx_data::browse::view::SectionView { sid: Some(plx_plex::plex::ServerId::from_raw(0)), key: 1,
            kind: SecKind::Movie, row: plx_data::browse::SrcRow { section: 0, title: "Movies".into(),
                pinned: true, current: true, ..Default::default() } }]);
    let mut page = collections_page();
    run(&mut page, &fixture, ScreenEvent::Mount);
    let (crumb, _) = page.header.as_ref().and_then(|h| h.crumb.as_ref()).expect("the crumb");
    assert_eq!(crumb.root, "Movies", "the library's own name, not a hard-coded 'Films'");
}

#[test]
fn a_tile_is_captioned_with_its_member_count_and_a_smart_one_is_marked() {
    let _guard = plx_base::testlock::serial();
    let _lang = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::from_tag("en"));
    let fixture = as_collections(2);
    let view = fixture.listing.view();
    let item = view.item(0).expect("the first collection");
    let label = parts::collection_label(item);
    assert_eq!(label.caption.as_ref().map(|c| c.to_str().unwrap()), Some("24 films"));
    assert!(item.smart && !view.item(1).unwrap().smart);
}

#[test]
fn ok_on_the_crumb_root_and_back_pop_the_page() {
    let _guard = plx_base::testlock::serial();
    let fixture = as_collections(3);
    let mut page = collections_page();
    run(&mut page, &fixture, ScreenEvent::Mount);
    let pops = |fx: &[Fx<HostFixture>]| fx.iter().filter(|fx| matches!(fx, Fx::App(AppFx::Content(crate::registry::ContentReq::Back)))).count();
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.activate(toolbar::CRUMB_ROOT, false, &fixture.cx(None), &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    let fx: Vec<_> = out.into_iter().map(|effect| effect.fx).collect();
    assert_eq!(pops(&fx), 1, "the root link returns to the library page");
    // With no focus on the page (the nav holds it) BACK leaves the page, not the library.
    assert_eq!(pops(&run(&mut page, &fixture, back_key())), 1);
}

/// The type is a loan from the page beneath: the full grid, on becoming the top page, puts the
/// section back (also what heals a section a killed run left on collections).
#[test]
fn the_full_grid_puts_the_section_back_on_its_type() {
    let _guard = plx_base::testlock::serial();
    let fixture = as_collections(3);
    let mut page = fixture.screen();
    assert_eq!(page.listing(), Listing::All);
    run(&mut page, &fixture, ScreenEvent::Uncover);
    assert_eq!(staged_type(&page), Some(LibraryType::Primary));
    // While a page is pushed over it, it leaves the loan alone.
    let mut buried = fixture.screen();
    run(&mut buried, &fixture, ScreenEvent::WillLeave(plx_machine::machine::Leave::Deeper));
    buried.pending.cancel();
    run(&mut buried, &fixture, ScreenEvent::StoreChanged(plx_data::stores::StoreId::Browse.ord(), 1));
    assert_eq!(staged_type(&buried), None);
}
