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

/// **The Collections page settles like the library's grid.** The page is the library grid in
/// `Listing::Collections` mode, so a focused row below the first rests with its posters where the
/// first row's are at rest (the grid top, y 244), through the same `row_reveal`. Pinned here on the
/// mounted collections page, with focus moved by the real DOWN, so a rule that held for the films
/// grid alone (`scroll_chrome_tests`) could not stand in for it.
#[test]
fn a_focused_collections_row_below_the_first_rests_with_its_posters_at_244() {
    let _guard = plx_base::testlock::serial();
    let fixture = as_collections(60);
    let mut page = collections_page();
    run(&mut page, &fixture, ScreenEvent::Mount);
    page.initial = false;
    let mut engine = FocusEngine::new();
    let first = page.key(page.pair.detail.elem_at(12).unwrap());
    page.relayout(Some(first));
    engine.set(OWNER, first, Some(page.pair.groups_config().detail), By::Restore);
    direction(&mut page, &mut engine, &fixture, Dir::Down);
    let next = engine.current(OWNER).unwrap();
    assert_eq!(page.grid_position(Some(next)), Some((3, 0)), "DOWN reached the fourth row of collections");
    assert!(page.scroll_target > 0.0, "the page scrolled to it");
    assert_eq!(page.target_layout.row_y(3, page.scroll_target), 244.0, "its posters rest at the grid top");
    assert!(page.target_layout.max_scroll() > page.scroll_target, "…and nothing clamped it short");
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
    let header = page.header.row().expect("the row is built");
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
    let header = page.header.row().expect("the row is built");
    let crumb = header.page.spec().crumb.as_ref().expect("the crumb");
    assert_eq!(crumb.root, "Movies", "the library's own name, not a hard-coded 'Films'");
}

#[test]
fn a_tile_is_captioned_with_its_member_count_and_a_smart_one_is_marked() {
    let _guard = plx_base::testlock::serial();
    let _lang = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::from_tag("en"));
    let fixture = as_collections(2);
    let view = fixture.listing.view();
    let item = view.item(0).expect("the first collection");
    let label = parts::grid_label(item, &parts::caption_policy());
    assert_eq!(label.caption.as_ref().map(|c| c.to_str().unwrap()), Some("24 films"));
    assert!(item.smart && !view.item(1).unwrap().smart);
}

/// **S23, the focused caption of a long collection name stays inside its tile.** The name the face
/// cuts is carried by the caption under the lifted tile (with the Title switch off, the one rule
/// puts it on the details line, in front of the member count); it used to glide inside a window two tiles
/// and two gaps wide, so it ran under the next column and was cut hard mid-letter ("...Open Movi")
/// at the window's edge. Now the window is the lifted tile, the run dissolves over the last
/// [`plx_ui::cards::LABEL_EDGE_FADE`] px before the edge, and the shared marquee glides it (a run and its follower).
#[test]
fn a_long_collection_name_is_held_to_its_lifted_tile_with_a_fade_and_marquees() {
    use plx_machine::machine::Measure as _;
    let _guard = plx_base::testlock::serial();
    let _lang = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::from_tag("en"));
    plx_machine::idle::frame_begin(1.0 / 60.0);
    let name = "The Complete Blender Foundation Open Movie Projects: Every Short Film and Making-Of, Extended Cuts, \
        Commentary Tracks, Concept Art and the Complete Behind-the-Scenes Production Diaries, Box Set";
    let item = plx_data::pms::PmsMovie {
        sid: plx_plex::plex::ServerId::from_raw(0), rk: "50001".into(), title: name.into(),
        subtype: Some("movie".into()), kind: plx_data::pms::KIND_COLLECTION, child_count: 2,
        thumb: "/library/collections/50001/composite/1700000000?width=400".into(), ..Default::default()
    };
    let label = parts::grid_label(&item, &plx_ui::caption_policy::CaptionPolicy::DEFAULT);
    let line = format!("{name} \u{b7} 2 films");
    assert!(label.title.is_none(), "the Title switch is off: the name rides the details line");
    assert_eq!(label.caption.as_ref().map(|c| c.to_str().unwrap()), Some(line.as_str()), "the cut name is carried by the caption");
    let sty = layout::Layout::new(false, 10_000).style();
    let s = sty.focus_scale;
    let lifted = Rect::new(plx_ui::consts::MARGIN_X, 300.0, sty.w, sty.h).scaled(s);
    let runs = plx_ui::draw_census::capture_runs(|| {
        plx_ui::cards::draw_label(plx_ui::Painter::recording(), lifted, s, &sty, &label, &plx_ui::fixture::FixtureMeasure);
    });
    let run_w = plx_ui::fixture::FixtureMeasure
        .width(label.caption.as_ref().unwrap(), plx_ui::theme::size::CAPTION, false);
    assert!(run_w > lifted.w, "the name is wider than the tile it names ({run_w} vs {})", lifted.w);
    let title: Vec<_> = runs.iter().filter(|r| r.text == line && r.ink[3] > 0.5).collect();
    assert_eq!(title.len(), 2, "a run and its follower: the shared marquee glides it ({runs:?})");
    for run in &title {
        let window = run.clip.expect("the glide is clipped to a window");
        assert!(window.x >= lifted.x - 0.5 && window.x + window.w <= lifted.x + lifted.w + 0.5,
            "the window {window:?} stays inside the lifted tile {lifted:?}");
    }
    let window = title[0].clip.unwrap();
    assert!((title[1].at.x - title[0].at.x - (run_w + plx_ui::marquee::GAP)).abs() < 0.5, "the follower is one gap behind");
    let edge = window.x + window.w;
    let (from, to) = title[0].fade.expect("the run dissolves into the clip edge instead of being cut hard");
    assert!((to - edge).abs() < 0.5 && (to - from - plx_ui::cards::LABEL_EDGE_FADE).abs() < 0.5,
        "a {} px dissolve ending on the window's right edge {edge}: {from}..{to}", plx_ui::cards::LABEL_EDGE_FADE);
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
