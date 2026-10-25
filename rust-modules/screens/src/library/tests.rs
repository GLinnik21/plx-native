use super::*;
use plx_ui::fixture::FixtureMeasure;
use plx_ui::focus::{FocusEngine, Outcome};
use plx_machine::machine::{Host, InputOwner, FocusRead, PressRead, Tick};
use plx_ui::screen::ScreenArg;

#[derive(Clone)]
pub(super) struct Arg;
impl LogicalState for Arg {
    fn write(&self, _: &mut Canon) {}
    fn probe(&self, _: &mut String) {}
}
impl ScreenArg for Arg {
    fn chrome(&self) -> plx_machine::machine::Chrome { plx_machine::machine::Chrome::None }
    fn id(&self) -> plx_machine::machine::ScreenId { plx_machine::machine::ScreenId(1) }
    fn title(&self) -> Option<&str> { None }
    fn same_instance(&self, _: &Self) -> bool { true }
}
pub(super) struct HostFixture;
#[derive(Clone, Copy)]
pub(super) struct Views<'a> {
    listing: plx_data::stores::browse::ListingView<'a>,
    directory: plx_data::stores::browse::DirectoryView<'a>,
    hubs: plx_data::stores::browse::HubsView<'a>,
}
impl Host for HostFixture {
    type Arg = Arg;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = Views<'a>;
    type Init = Arg;
    type Memory = PageMemory;
}
impl LibraryLike for HostFixture {
    fn listing<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::ListingView<'a> { cx.views.listing }
    fn directory<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::DirectoryView<'a> { cx.views.directory }
    fn section_hubs<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::HubsView<'a> { cx.views.hubs }
    /// The fixture's libraries all hold collections, and their probe has said so: the Collections
    /// control is on the row only with a known count, and these tests are about a row that has it.
    fn collections_count(_cx: &Cx<'_, Self>, _section: usize) -> Option<Result<u32, crate::registry::Forbidden>> { Some(Ok(4)) }
}
const ENTRY: EntryId = EntryId(81);
const OWNER: InputOwner = InputOwner::Entry(ENTRY);

/// Return memory is captured several times per frame. A 1,200-item catalog must share its
/// unchanged keys across those snapshots, while later reconciliation cannot mutate a saved
/// return position or its canonical state.
#[test]
fn page_memory_shares_1200_keys_and_preserves_older_snapshots() {
    let _guard = plx_base::testlock::serial();
    let sid = plx_plex::plex::ServerId::from_raw(1);
    let section = LibrarySectionIdentity { sid, key: 7 };
    let mut page = LibraryScreen::new(ENTRY, InstanceId(20), SecKind::Movie);
    for index in 0..1200 {
        page.keys.register(LibraryIdentity::Grid {
            section: section.clone(), sid, rk: format!("fixture-{index}"),
        }, GRID_GROUP, index);
    }
    let original = page.page_memory();
    let original_hash = PageMemory::Library(original.clone()).hash();
    assert_eq!(original.keys.len(), 1200);
    for frame in 0..60 {
        page.scroll.jump(frame as f32);
        let next = page.page_memory();
        assert_eq!(original.keys.as_ptr(), next.keys.as_ptr(),
            "scroll frame {frame} must not copy the unchanged catalog into return memory");
        let forwarded = next.clone();
        assert_eq!(next.keys.as_ptr(), forwarded.keys.as_ptr(),
            "forwarding return memory must not copy the catalog either");
    }

    let key = &original.keys[17];
    let elem = key.elem;
    let identity = key.identity.clone();
    assert_eq!(page.keys.register(identity.clone(), GRID_GROUP, 17), elem);
    page.keys.update_last_place(elem, GRID_GROUP, 17);
    assert_eq!(page.page_memory().keys.as_ptr(), original.keys.as_ptr(),
        "unchanged reconciliation must not detach the shared snapshot");

    assert_eq!(page.keys.register(identity, GroupId(90), 23), elem);
    let moved = page.page_memory();
    assert_ne!(moved.keys.as_ptr(), original.keys.as_ptr());
    assert_eq!(moved.keys[17].last_index, 23);
    assert_eq!(original.keys[17].last_index, 17);
    page.keys.update_last_place(elem, GroupId(91), 31);
    let moved_again = page.page_memory();
    assert_eq!(moved_again.keys[17].last_index, 31);
    assert_eq!(moved.keys[17].last_index, 23, "a previous reconciliation stays immutable");

    let added = page.keys.register(LibraryIdentity::Grid {
        section: section.clone(), sid, rk: "fixture-new".into(),
    }, GRID_GROUP, 1200);
    let grown = page.page_memory();
    assert_eq!(grown.keys.len(), 1201);
    assert_eq!(moved_again.keys.len(), 1200, "appending must not grow an older snapshot");
    assert_ne!(added, elem);

    let mut restored = KeyRegistry::restore(&original);
    let restored_memory = restored.remember(original.section.clone(), original.scroll, Vec::new());
    assert_eq!(restored_memory.keys.as_ptr(), original.keys.as_ptr(),
        "remounting reuses the immutable key snapshot");
    assert_eq!(restored.last_place(elem), Some((GRID_GROUP, 17)));
    restored.update_last_place(elem, GroupId(92), 41);
    assert_eq!(restored.last_place(elem), Some((GroupId(92), 41)));
    assert_eq!(original.keys[17].last_index, 17);
    assert_eq!(PageMemory::Library(original.clone()).hash(), original_hash,
        "neither live reconciliation nor remount mutation may change saved canonical memory");
}

/// The key at a grid slot that may lie outside the projected windows (a far seat projects it).
pub(super) fn deep(page: &mut LibraryScreen, fixture: &Fixture, index: usize) -> u32 {
    page.pair.detail.elem_for(&fixture.cx(None), &mut page.keys, index, COLS).expect("a slot of the listing")
}

#[test]
fn all_grid_caption_band_restores_with_the_saved_viewport() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    let elem = deep(&mut page, &fixture, 35);
    let key = page.key(elem);
    page.relayout(Some(key));
    page.scroll_target = page.target_layout.row_reveal(5);
    page.scroll.jump(page.scroll_target);
    page.relayout(Some(key));
    let before = page.place(&key.elem, &fixture.cx(Some(key)), At::Drawn).unwrap();
    let PageMemory::Library(memory) = <LibraryScreen as Screen<HostFixture>>::memory(&page) else { panic!() };
    let mut restored = LibraryScreen::new(ENTRY, InstanceId(20), SecKind::Movie);
    restored.restore(&memory);
    restored.sync(&fixture.cx(Some(key)));
    let after = restored.place(&key.elem, &fixture.cx(Some(key)), At::Drawn).unwrap();
    assert_eq!(restored.layout.row_expansion(5), 1.0);
    assert_eq!(restored.layout.row_expansion(4), 0.0);
    assert_eq!(restored.scroll.pos, page.scroll.pos);
    assert_eq!([after.rect.x, after.rect.y, after.rect.w, after.rect.h],
        [before.rect.x, before.rect.y, before.rect.w, before.rect.h]);
    assert_eq!(restored.layout.doc_h(), page.layout.doc_h());
}

#[test]
fn saved_last_all_row_opens_before_the_bookmark_scroll_is_clamped() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let saved_scroll = fixture.screen().layout.with_grid_focus(Some(5)).max_scroll();
    fixture.listing = fixture.listing.clone().with_cursor(plx_data::stores::browse::Cursor {
        at: plx_data::stores::browse::CursorAt::SlotIndex(35), scroll: saved_scroll,
    });
    let mut page = fixture.screen();
    let mut output = Vec::new();
    let mut present = plx_machine::present::Present::new();
    assert!(page.seed_cursor(&fixture.cx(None),
        &mut Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present)));
    assert_eq!(page.scroll.pos, saved_scroll);
    assert_eq!(page.target_layout.row_expansion(5), 1.0);
    assert_eq!(page.layout.row_expansion(5), 1.0);
    assert_eq!(page.restore_scroll, Some(saved_scroll));
}

#[test]
fn all_grid_moves_open_only_the_destination_band_and_use_settled_reveal() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    let first = page.key(page.pair.detail.elem_at(12).unwrap());
    page.relayout(Some(first));
    engine.set(OWNER, first, Some(page.pair.groups_config().detail), By::Restore);
    direction(&mut page, &mut engine, &fixture, Dir::Down);
    let next = engine.current(OWNER).unwrap();
    assert_eq!(page.grid_position(Some(next)), Some((3, 0)));
    assert_eq!(page.layout.row_expansion(2), 1.0, "outgoing row starts closing from its live size");
    assert_eq!(page.layout.row_expansion(3), 0.0, "incoming row starts compact");
    assert_eq!(page.target_layout.row_expansion(2), 0.0);
    assert_eq!(page.target_layout.row_expansion(3), 1.0);
    assert_eq!(page.scroll_target, page.target_layout.row_reveal(3));
    let target = page.place(&next.elem, &fixture.cx(Some(next)), At::SpringTarget).unwrap();
    assert_eq!(target.rest_rect.cy(), page.target_layout.row_y(3, page.scroll_target)
        + page.target_layout.card_h() * 0.5);
    let (lo, hi) = page.target_layout.visible_rows(page.scroll_target);
    assert!((lo..hi).contains(&3));
}

#[test]
fn grid_paint_window_keeps_cards_above_the_centered_tab_track() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    let layout = page.layout;
    let index = 2 * COLS;
    let y = plx_ui::widgets::TOP_BAR_BOTTOM - plx_ui::consts::CARD_H - 8.0;
    let scroll = layout.row_y(2, 0.0) - y;
    page.pair.detail.set_geometry(layout, scroll, layout, scroll);
    let rect = page.pair.detail.rect_at(&fixture.cx(None), index);
    assert!(rect.y + rect.h > 0.0 && rect.y + rect.h < plx_ui::widgets::TOP_BAR_BOTTOM);
    let window = page.pair.detail.window();
    assert!(window.contains(&index), "the real paint iterator must include this visible card");

    // The paint iterator conservatively retains overscan rows, but never the entire catalog.
    // Far outside that window, the same card must be excluded on either side of the screen.
    for y in [-2.0 * SCR_H, 2.0 * SCR_H] {
        let scroll = layout.row_y(2, 0.0) - y;
        page.pair.detail.set_geometry(layout, scroll, layout, scroll);
        let window = page.pair.detail.window();
        assert!(!window.contains(&index), "far-offscreen card at {y} must not enter paint iteration");
    }
}

#[test]
fn duplicate_across_pages_keeps_full_projection_recovery_metadata() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let sid = plx_plex::plex::ServerId::from_raw(0);
    let movie = |i| plx_data::pms::PmsMovie { sid, rk: format!("duplicate-test-{i}"), ..Default::default() };
    let mut items = (0..120).map(movie).collect::<Vec<_>>();
    items[85] = items[5].clone();
    fixture.listing = plx_data::browse::view::ListingSnapshot::fixture(sid,
        items.iter().cloned().map(Some).collect(), Vec::new()).with_total(10_000);
    let mut partial = fixture.screen();
    let mut full = fixture.screen();
    let duplicate = partial.pair.detail.elem_at(5).unwrap();
    let group = partial.pair.groups_config().detail;
    let mut observations = Vec::new();
    let grid_hash = |page: &LibraryScreen| {
        let mut canon = Canon::new();
        page.pair.detail.write(&mut canon);
        canon.finish()
    };
    let mut canonical = Vec::new();
    // First change only unrelated data on the lower page; then remove both copies together.
    // A second pass removes only the higher copy, leaving the lower unchanged.
    for remove_high_only in [false, true] {
        fixture.listing = fixture.listing.clone().with_page(0, items[..60].to_vec())
            .with_page(60, items[60..].to_vec());
        partial.sync(&fixture.cx(None));
        full.pair.detail.clear_projection();
        full.sync(&fixture.cx(None));
        fixture.listing = fixture.listing.clone().with_page(0, vec![movie(1000)]);
        partial.pair.detail.reset_publication_ops();
        partial.sync(&fixture.cx(None));
        full.pair.detail.clear_projection();
        full.sync(&fixture.cx(None));
        // The pass visits the wanted window (five rows of six), exactly, not the 60-slot listing.
        assert_eq!(partial.pair.detail.publication_ops().0, 30);
        assert!(partial.pair.detail.publication_ops().1 <= 120);
        canonical.push((grid_hash(&partial), grid_hash(&full)));
        observations.push((partial.keys.last_place(duplicate), full.keys.last_place(duplicate)));
        assert_eq!(partial.pair.detail.index_of(duplicate), Some(5));
        if remove_high_only {
            fixture.listing = fixture.listing.clone().with_page(85, vec![movie(85)]);
        } else {
            fixture.listing = fixture.listing.clone().with_page(5, vec![movie(1005)])
                .with_page(85, vec![movie(85)]);
        }
        partial.sync(&fixture.cx(None));
        full.pair.detail.clear_projection();
        full.sync(&fixture.cx(None));
        canonical.push((grid_hash(&partial), grid_hash(&full)));
        observations.push((partial.keys.last_place(duplicate), full.keys.last_place(duplicate)));
        observations.push((partial.pair.detail.fallback_for(duplicate).map(|e| (group, e as usize)),
            full.pair.detail.fallback_for(duplicate).map(|e| (group, e as usize))));
        if remove_high_only {
            fixture.listing = fixture.listing.clone().with_page(5, vec![movie(1005)]);
            partial.sync(&fixture.cx(None));
            full.pair.detail.clear_projection();
            full.sync(&fixture.cx(None));
            canonical.push((grid_hash(&partial), grid_hash(&full)));
            assert_eq!(partial.keys.last_place(duplicate), Some((group, 5)));
            assert_eq!(partial.pair.detail.fallback_for(duplicate), partial.pair.detail.elem_at(5));
        }
    }
    eprintln!("duplicate recovery partial/full: {observations:?}");
    assert!(observations.iter().all(|(partial, full)| partial == full),
        "partial publication must preserve full projection's last occurrence and tombstone fallback: {observations:?}");
    assert!(canonical.iter().all(|(partial, full)| partial == full),
        "ordered projection and known metadata must equal full projection: {canonical:?}");
}

#[test]
fn large_listing_publication_work_is_bounded_by_the_window_not_the_listing() {
    let _guard = plx_base::testlock::serial();
    const TOTAL: usize = 10_000;
    const PAGE: usize = 60;
    let mut fixture = Fixture::new();
    let sid = plx_plex::plex::ServerId::from_raw(0);
    let movies = |start: usize| (start..start + PAGE).map(|i| plx_data::pms::PmsMovie {
        sid, rk: format!("large-{i}"), title: format!("Large {i}"), ..Default::default()
    }).collect::<Vec<_>>();
    fixture.listing = plx_data::browse::view::ListingSnapshot::fixture(
        sid, movies(0).into_iter().map(Some).collect(), Vec::new()).with_total(TOTAL);

    let mut page = fixture.screen();
    let (initial_slots, initial_known) = page.pair.detail.publication_ops();
    let initial_registry = page.keys.register_probes();
    page.pair.detail.reset_publication_ops();
    page.keys.reset_register_probes();

    fixture.listing = fixture.listing.clone().with_page(PAGE, movies(PAGE));
    page.sync(&fixture.cx(None));
    let (next_slots, next_known) = page.pair.detail.publication_ops();
    let next_registry = page.keys.register_probes();
    eprintln!("publication-ops initial slots={initial_slots} known={initial_known} registry={initial_registry}; next slots={next_slots} known={next_known} registry={next_registry}");

    assert!(initial_slots <= 64 && initial_known <= 64 && initial_registry <= 128
        && next_slots <= 64 && next_known <= 64 && next_registry <= 128,
        "publication work is the wanted window, never the listing: initial slots={initial_slots} known={initial_known} registry={initial_registry}; next slots={next_slots} known={next_known} registry={next_registry}");
}

#[test]
fn derived_grid_indexes_survive_reorder_truncation_clear_and_restore() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let original = fixture.listing.clone();
    let mut page = fixture.screen();
    let stable = page.pair.detail.elem_at(5).unwrap();
    let removed = deep(&mut page, &fixture, 30);

    let mut reordered = (0..36).map(|i| original.view().item(i).unwrap().clone()).collect::<Vec<_>>();
    reordered.swap(5, 17);
    fixture.listing = fixture.listing.clone().with_page(0, reordered);
    page.sync(&fixture.cx(None));
    assert_eq!(page.pair.detail.elem_at(17), Some(stable));
    assert_eq!(page.pair.detail.index_of(stable), Some(17), "a same-query reorder follows the stable item key");

    fixture.listing = fixture.listing.clone().with_total(10);
    page.sync(&fixture.cx(None));
    assert_eq!(page.pair.detail.index_of(stable), None);
    assert_eq!(page.pair.detail.index_of(removed), None);
    assert_eq!(page.pair.detail.fallback_for(stable), page.pair.detail.elem_at(9));
    assert_eq!(page.pair.detail.fallback_for(removed), page.pair.detail.elem_at(9),
        "a truncated item falls back through its last published slot");

    let memory = page.page_memory();
    fixture.listing = plx_data::browse::view::ListingSnapshot::absent();
    page.sync(&fixture.cx(None));
    assert!(page.pair.detail.total() == 0);
    assert_eq!(page.pair.detail.index_of(stable), None);

    fixture.listing = original.with_total(10);
    let mut restored = LibraryScreen::new(ENTRY, InstanceId(20), SecKind::Movie);
    restored.restore(&memory);
    restored.sync(&fixture.cx(None));
    assert_eq!(restored.pair.detail.elem_at(5), Some(stable),
        "restore rebuilds lookup indexes and reuses the stable item key");
    assert_eq!(restored.pair.detail.index_of(stable), Some(5));
    assert_eq!(restored.pair.detail.fallback_for(removed), restored.pair.detail.elem_at(9));
}

#[test]
fn down_from_a_missing_final_row_column_clamps_to_the_last_item() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let sid = plx_plex::plex::ServerId::from_raw(0);
    fixture.listing = plx_data::browse::view::ListingSnapshot::fixture(sid,
        (0..8).map(|i| Some(plx_data::pms::PmsMovie { sid, rk: format!("{i}"), ..Default::default() })).collect(),
        Vec::new());
    let mut page = fixture.screen();
    let mut engine = FocusEngine::new();
    let key = page.key(page.pair.detail.elem_at(5).unwrap());
    engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
    direction(&mut page, &mut engine, &fixture, Dir::Down);
    assert_eq!(engine.current(OWNER), Some(page.key(page.pair.detail.elem_at(7).unwrap())));
    direction(&mut page, &mut engine, &fixture, Dir::Down);
    assert_eq!(engine.current(OWNER), Some(page.key(page.pair.detail.elem_at(7).unwrap())), "the last row remains an edge");
}

#[test]
fn owned_rail_keeps_the_fixed_grid_top_origin_and_short_window() {
    let _guard = plx_base::testlock::serial();
    for n in [9, 40] {
        let mut fixture = Fixture::new();
        let sid = plx_plex::plex::ServerId::from_raw(0);
        fixture.listing = plx_data::browse::view::ListingSnapshot::fixture(sid,
            (0..36).map(|i| Some(plx_data::pms::PmsMovie { sid, rk: format!("{i}"), ..Default::default() })).collect(),
            (0..n).map(|i| (format!("{i}"), 1)).collect());
        let page = fixture.screen();
        let mut groups = Vec::new();
        page.pair.master.groups(&fixture.cx(Some(page.key(page.pair.detail.elem_at(0).unwrap()))), &mut groups);
        let rail = groups.first().expect("the fixture has a real rail").extent;
        // a short alphabet starts at the grid's top; a long one climbs to the board's 152 first
        assert_eq!(rail.y, if n == 9 { 244.0 } else { 152.0 }, "the rail is not attached to the scrolling first grid row");
        if n == 9 { assert_eq!(rail.h, 9.0 * layout::RAIL_PITCH); }
        else { assert!(rail.h < n as f32 * layout::rail_pitch(n), "an alphabet too long even squeezed scrolls"); }
    }
}

#[test]
fn the_rail_lists_the_whole_alphabet_and_letters_the_server_omitted_are_empty() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let sid = plx_plex::plex::ServerId::from_raw(0);
    // PMS answers only the letters something starts with.
    fixture.listing = plx_data::browse::view::ListingSnapshot::fixture(sid,
        (0..6).map(|i| Some(plx_data::pms::PmsMovie { sid, rk: format!("{i}"), ..Default::default() })).collect(),
        vec![("B".into(), 2), ("C".into(), 1), ("E".into(), 3)]);
    let page = fixture.screen();
    let rail = &page.pair.master;
    assert_eq!(rail.elems.len(), 26, "A to Z, not the three the server listed");
    let empty: Vec<usize> = (0..26).filter(|i| rail.letter_empty(*i)).collect();
    assert_eq!(empty.len(), 23, "every letter but B, C and E is dimmed and skipped by focus");
    assert!(!empty.contains(&1) && !empty.contains(&2) && !empty.contains(&4));
    assert_eq!(rail.letter_for_item(0), 1);
    assert_eq!(rail.letter_for_item(2), 2);
    assert_eq!(rail.letter_for_item(3), 4, "the jump offsets are the prefix sums: E starts at item 3");
    let (_, _, h, max) = layout::rail_geom(26);
    assert_eq!(max, 0.0);
    let mut groups = Vec::new();
    page.pair.master.groups(&fixture.cx(Some(page.key(page.pair.detail.elem_at(0).unwrap()))), &mut groups);
    let extent = groups.first().expect("the rail is a group").extent;
    assert!((extent.h - h).abs() < 0.01 && extent.y + extent.h < 1080.0, "the whole alphabet stands on screen");
}

#[test]
fn retry_stop_matches_the_shared_measured_status_action_with_and_without_reason() {
    let _guard = plx_base::testlock::serial();
    for owner in ["", "friend"] {
        let mut fixture = Fixture::new();
        fixture.listing = plx_data::stores::browse::ListingSnapshot::empty_for_test();
        fixture.directory = plx_data::browse::view::DirectorySnapshot::fixture_source(4, plx_plex::plex::ServerId::from_raw(7),
            plx_data::browse::SrcGroup { name: "Cinema server".into(), handle: owner.into(),
                state: plx_data::browse::SourceState::Unreachable, tier: None }, SecFetch::Failed);
        let page = fixture.screen();
        let cx = fixture.cx(Some(page.key(RETRY)));
        let (caption, reason) = page.status_text(&cx);
        assert_eq!(reason.is_some(), !owner.is_empty());
        let mut overlay = plx_ui::widgets::StatusOverlay::new(page.status_frame(), &caption,
            plx_ui::widgets::StatusKind::Failed).page(plx_ui::icons::Icon::ServerBadgeMinus).action(c"Try again");
        if let Some(reason) = &reason { overlay = overlay.reason(reason); }
        let expected = overlay.action_frame_measured(cx.measure).unwrap();
        for at in [At::Drawn, At::SpringTarget] {
            let actual = page.place(&RETRY, &cx, at).unwrap().rect;
            assert_eq!([actual.x, actual.y, actual.w, actual.h], [expected.x, expected.y, expected.w, expected.h],
                "reason={owner:?}, at={at:?}");
        }
    }
}

/// **A failed source names a wrong clock when key mode cannot help** (`net::keypin::blocked_for`): the
/// clock reason and glyph take the slot, ahead of the "shared by" line and behind an offered
/// plaintext server's reason. No fact is byte-for-byte today's read-out, the reason moves the row,
/// and the *Try again* stop is the pill the same frame draws.
#[test]
fn a_failed_source_names_a_wrong_clock_when_key_mode_cannot_help() {
    use plx_net::net::keypin::{self, Blocked};
    use plx_ui::icons::Icon;
    use plx_ui::widgets::StatusOverlay;
    let _guard = plx_base::testlock::serial();
    plx_plex::plex::grant::reset_for_test();
    let key = keypin::key_of("library-clock.invalid", 32400);
    let _scoped = keypin::Scoped::watch_machine("library-clock-machine", &key);
    plx_plex::plex::reset_servers_for_test();
    let failed_sid = plx_plex::plex::register_pinned_with_client_id("library-clock-machine",
        &plx_plex::plex::Origin::http("192.168.1.51", 32400), "", None, "client", Default::default());
    // `tick_damaged`: a fact appearing or clearing under a static Failed read-out moves the
    // action row, so the tick must damage the frame.
    let tick = tick_damaged;
    let read = |page: &LibraryScreen, fixture: &Fixture| {
        let cx = fixture.cx(Some(page.key(RETRY)));
        let (caption, reason) = page.status_text(&cx);
        let overlay = page.status_overlay(&cx, &caption, reason.as_deref());
        let (shown, glyph) = (overlay.reason.map(|r| r.to_owned()), overlay.glyph);
        let row = overlay.action_frame_measured(cx.measure).unwrap();
        let stop = page.place(&RETRY, &cx, At::Drawn).unwrap().rect;
        assert_eq!([stop.x, stop.y, stop.w, stop.h], [row.x, row.y, row.w, row.h], "the hit rect is the drawn pill");
        (shown, glyph, row, caption)
    };
    for owner in ["", "friend"] {
        let fixture = Fixture::failed_source(failed_sid, owner);
        let mut page = fixture.screen();
        tick(&mut page, &fixture);
        let (shown, glyph, plain_row, caption) = read(&page, &fixture);
        let shared = plx_platform::i18n::msg::browse_library_shared_unreachable("friend");
        assert_eq!(shown.as_ref().and_then(|r| r.to_str().ok()), (!owner.is_empty()).then_some(shared.as_str()),
            "no fact: today's read-out");
        assert_eq!(glyph, Some(Icon::ServerBadgeMinus));

        keypin::strict_failure(&key, 60, Some(10));
        let (stale, ..) = read(&page, &fixture);
        assert_eq!(stale, shown, "the held fact does not change between ticks");
        assert!(tick(&mut page, &fixture), "a fact appearing under a static read-out damages the frame");
        let (shown, glyph, row, clock_caption) = read(&page, &fixture);
        assert_eq!(shown.as_deref(), Some(plx_platform::i18n::msg::browse_clock_no_key_c()), "owner {owner:?}");
        assert_eq!(glyph, Some(Icon::ClockBadgeAlert));
        assert_eq!(clock_caption, caption, "the verdict is unchanged");
        assert!(row.y >= plain_row.y);
        if owner.is_empty() {
            assert!(row.y > plain_row.y, "the reason moves the row");
        }
        assert!(row.y >= StatusOverlay::FULL_ANCHOR_TOP);

        keypin::key_changed(&key);
        assert!(tick(&mut page, &fixture));
        assert_eq!(page.clock.blocked(), Some(Blocked::KeyChanged));
        let (shown, ..) = read(&page, &fixture);
        assert_eq!(shown.as_deref(), Some(plx_platform::i18n::msg::browse_clock_key_changed_c()));

        keypin::strict_established(&key);
        assert!(tick(&mut page, &fixture), "…and one clearing does too");
        let (shown, glyph, ..) = read(&page, &fixture);
        assert_eq!(shown.as_ref().and_then(|r| r.to_str().ok()), (!owner.is_empty()).then_some(shared.as_str()),
            "the fact cleared: the shared-by line is back");
        assert_eq!(glyph, Some(Icon::ServerBadgeMinus));
    }

    // An offered plaintext server's reason outranks the clock's, glyph included.
    use plx_plex::plex::session::PlaintextChoice;
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_pinned_with_client_id("lan-machine", &plx_plex::plex::Origin::http("192.168.1.50", 32400), "", None, "client", Default::default());
    let fixture = Fixture::failed_source(sid, "friend");
    let mut page = fixture.screen();
    let lan_key = keypin::key_of("192.168.1.50", 32400);
    let _lan = keypin::Scoped::watch_machine("lan-machine", &lan_key);
    keypin::strict_failure(&lan_key, 60, Some(10));
    plx_plex::plex::grant::offered(plx_plex::plex::grant::scope(),
        plx_plex::plex::grant::PlaintextVerdict { machine_id: "lan-machine".into(), name: "nas".into(), shared_by: String::new(),
            eligibility: plx_plex::plex::probe::PlaintextEligibility::Eligible, choice: PlaintextChoice::Undecided });
    tick(&mut page, &fixture);
    assert!(page.clock.blocked().is_some(), "the clock fact stands");
    let (shown, glyph, ..) = read(&page, &fixture);
    assert!(shown.as_ref().and_then(|r| r.to_str().ok()).is_some_and(|r| r.contains("Select Connect")), "{shown:?}");
    assert_eq!(glyph, Some(Icon::ServerBadgeMinus));
    plx_plex::plex::grant::reset_for_test();
}

/// **A fact about ANOTHER server does not colour this source's read-out** (scenario B): the
/// Library asks `net::keypin::blocked_for` about the failed source's own machine, so a second bound
/// server's expired certificate leaves it alone, and a fact for the source's own machine shows.
#[test]
fn a_failed_source_ignores_a_clock_fact_about_another_server() {
    use plx_net::net::keypin;
    use plx_ui::icons::Icon;
    let _guard = plx_base::testlock::serial();
    plx_plex::plex::grant::reset_for_test();
    plx_plex::plex::reset_servers_for_test();
    let here = keypin::key_of("library-here.invalid", 32400);
    let elsewhere = keypin::key_of("library-elsewhere.invalid", 32400);
    let _here = keypin::Scoped::watch_machine("library-here-machine", &here);
    let _elsewhere = keypin::Scoped::watch_machine("library-elsewhere-machine", &elsewhere);
    let sid = plx_plex::plex::register_pinned_with_client_id("library-here-machine",
        &plx_plex::plex::Origin::http("192.168.1.52", 32400), "", None, "client", Default::default());
    let fixture = Fixture::failed_source(sid, "");
    let mut page = fixture.screen();
    let tick = |page: &mut LibraryScreen| tick_damaged(page, &fixture);
    let shown = |page: &LibraryScreen| {
        let cx = fixture.cx(Some(page.key(RETRY)));
        let (caption, reason) = page.status_text(&cx);
        let overlay = page.status_overlay(&cx, &caption, reason.as_deref());
        (overlay.reason.map(|r| r.to_owned()), overlay.glyph)
    };
    tick(&mut page);
    keypin::strict_failure(&elsewhere, 60, Some(10));
    keypin::key_changed(&elsewhere);
    tick(&mut page);
    assert_eq!(page.clock.blocked(), None, "another server's fact is not this source's");
    assert_eq!(shown(&page), (None, Some(Icon::ServerBadgeMinus)), "today's read-out");

    keypin::strict_failure(&here, 60, Some(10));
    tick(&mut page);
    assert_eq!(shown(&page), (Some(plx_platform::i18n::msg::browse_clock_no_key_c().to_owned()), Some(Icon::ClockBadgeAlert)));
}

/// **A failed Library section and a failed Home stand on ONE line** (owner, 2026-09-19: the
/// Library's read-out sat ~y 830 while Home's was near the centre). The Library's verdict hangs
/// from `StatusOverlay::FULL_ANCHOR_TOP` exactly where Home's failed hub read-out puts its own
/// (`screens/home`'s `status_overlay`: the full frame, `.page()`, *Try again*, no reason); its own
/// server's read-out carries no reason, so its *Try again* is Home's too, word for word, while a
/// borrowed source's reason stacks the row lower by the reason slot and the read-out's drop, on the same column.
#[test]
fn a_failed_library_section_and_a_failed_home_share_the_verdict_and_the_row() {
    use plx_ui::widgets::{StatusKind, StatusOverlay};
    let _guard = plx_base::testlock::serial();
    let home = StatusOverlay::new(Rect::FULL, c"Can\u{2019}t reach your Plex server", StatusKind::Failed)
        .page(plx_ui::icons::Icon::ServerBadgeMinus)
        .action(c"Try again");
    for owner in ["", "friend"] {
        let mut fixture = Fixture::new();
        fixture.listing = plx_data::stores::browse::ListingSnapshot::empty_for_test();
        fixture.directory = plx_data::browse::view::DirectorySnapshot::fixture_source(4, plx_plex::plex::ServerId::from_raw(7),
            plx_data::browse::SrcGroup { name: "Cinema server".into(), handle: owner.into(),
                state: plx_data::browse::SourceState::Unreachable, tier: None }, SecFetch::Failed);
        let page = fixture.screen();
        let cx = fixture.cx(Some(page.key(RETRY)));
        let (caption, reason) = page.status_text(&cx);
        if owner.is_empty() {
            assert_eq!(caption.to_str().unwrap(), "Can\u{2019}t reach your Plex server");
        }
        let library = page.status_overlay(&cx, &caption, reason.as_deref());
        let (lv, hv) = (library.verdict_band_measured(cx.measure), home.verdict_band_measured(cx.measure));
        assert_eq!(lv.y, hv.y, "owner={owner:?}: the verdicts share a line");
        assert_eq!(lv.y, StatusOverlay::FULL_ANCHOR_TOP);
        let lr = page.place(&RETRY, &cx, At::Drawn).unwrap().rect;
        let hr = home.action_frame_measured(cx.measure).unwrap();
        assert_eq!(lr.cx(), hr.cx(), "owner={owner:?}: one column");
        if owner.is_empty() {
            assert_eq!(lr.y, hr.y, "own server: the rows share a line");
        } else {
            assert!(lr.y > hr.y, "a borrowed source's reason stacks its row under it");
        }
    }
}

#[test]
fn a_fully_discovered_missing_kind_finishes_its_fade_and_has_no_foreign_grid() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.directory = plx_data::browse::view::DirectorySnapshot::fixture(1, 0, vec![
        plx_data::browse::view::SectionView { sid: Some(plx_plex::plex::ServerId::from_raw(0)), key: 1,
            kind: SecKind::Movie, row: plx_data::browse::SrcRow { section: 0, title: "Cinema".into(),
                pinned: true, current: true, ..Default::default() } }]);
    let mut page = LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Show);
    page.page_fade.mount();
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    for i in 0..40 {
        page.step(&ScreenEvent::Tick(Tick { ms: i * 20, dt_us: 20_000 }), &fixture.cx(None),
            &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    }
    assert_eq!(page.readout, Readout::Empty);
    assert_eq!(page.page_fade.alpha(), 1.0, "a terminal absent type must not hold the page at black");
    assert_eq!(page.kind, SecKind::Show, "the requested type is not silently replaced by Movies");
    assert_eq!(page.status_text(&fixture.cx(None)).0.to_str().unwrap(), "Nothing here matches");
    assert!(!out.iter().any(|effect| matches!(&effect.fx, Fx::App(AppFx::Store(StoreId::Browse,
        StoreCmd::Browse(BrowseCmd::Addressed { work: LibraryWork::Want { .. }, .. }))))),
        "waiting for Shows cannot page through the retained Movies listing");
    let mut groups = Vec::new();
    page.groups(&fixture.cx(None), &mut groups);
    assert!(!groups.iter().any(|g| g.id == page.pair.groups_config().detail));
    assert!(page.focused_item(Some(page.key(page.keys.keys().first().map_or(0, |k| k.elem))), &fixture.cx(None)).is_none());
}

#[test]
fn owned_card_stops_clip_pointer_hits_and_hold_the_engine_item() {
    use plx_ui::hit::PointerKind;
    use plx_ui::input::{InputMachine, PressEvent};
    use plx_machine::machine::{PressArm, PressFrom};
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    page.initial = false;
    page.scroll.jump(400.0);
    page.scroll_target = 400.0;
    let first = page.key(page.pair.detail.elem_at(0).unwrap());
    let chosen = page.key(page.pair.detail.elem_at(1).unwrap());
    page.relayout(Some(first));
    let cx = fixture.cx(Some(first));
    let mut draw = DrawFrame::new(&cx, plx_ui::Painter::root());
    page.record_stops(&mut draw);
    let stops = draw.into_stops();
    let stop = *stops.iter().find(|stop| stop.key == chosen).unwrap();
    let mut input = InputMachine::new();
    input.engine.set(OWNER, first, Some(page.pair.groups_config().detail), By::Restore);
    input.hit.fill(stops);
    input.hit.swap();
    assert!(stop.rect.y < 0.0, "the fixture contains a genuinely clipped card");
    assert!(input.hit.resolve(Some(ENTRY), PointerKind::Click, stop.rect.cx(), stop.rect.y + 1.0, Some(first)).miss);
    let y = stop.clip.y + 10.0;
    assert!(stop.rect.contains(stop.rect.cx(), y));
    let resolved = input.hit.resolve(Some(ENTRY), PointerKind::Click, stop.rect.cx(), y, Some(first));
    assert_eq!(resolved.focus, Some(chosen));
    assert_eq!(resolved.activate, Some((chosen, plx_ui::screen::Activate::Press)));
    input.engine.set(OWNER, chosen, page.group_of(&chosen.elem, &cx), By::Pointer);
    input.arm(PressArm { key: chosen, from: PressFrom::Pointer, holdable: true }, MachineId::Instance(InstanceId(19)), 0);
    let held = input.tick(plx_ui::press::LONG_MS + 1, 0.016);
    assert!(matches!(held.as_slice(), [PressEvent::Hold(_, _, key)] if *key == chosen));
    let PressEvent::Hold(id, _, _) = held[0] else { unreachable!() };
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.step(&ScreenEvent::PressHold(id), &fixture.cx(input.engine.current(OWNER)),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(out.iter().any(|effect| matches!(&effect.fx,
        Fx::App(AppFx::Library(LibraryReq::ItemMenu { sid, rk, from_deck: false }))
            if *sid == plx_plex::plex::ServerId::from_raw(0) && rk == "2")));
    assert!(input.hit.resolve(Some(EntryId(99)), PointerKind::Click, stop.rect.cx(), y, Some(chosen)).miss,
        "a menu owner cannot click through to a retained Library stop");
}

#[test]
fn actual_sort_menu_traps_engine_navigation_in_its_own_entry() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let page = fixture.screen();
    let entry = EntryId(99);
    let owner = InputOwner::Entry(entry);
    let arg = crate::registry::LibraryMenuArg { host: InstanceId(19),
        target: page.address(&fixture.cx(None)).unwrap(), kind: crate::registry::LibraryMenuKind::Sort,
        anchor: [0; 4], folded: Vec::new(), opener: None };
    let mut menu = menu::LibraryMenu::new(entry, arg);
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    menu.step(&ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::ContainerGroup(GroupId(0)) }), &fixture.cx(None),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(20)), &mut present));
    let mut engine = FocusEngine::new();
    engine.enter(owner, &menu, FocusTarget::ContainerGroup(GroupId(0)), None, &fixture.cx(None));
    let first = engine.current(owner).expect("actual menu has a sort row");
    for dir in [Dir::Up, Dir::Left, Dir::Right, Dir::Down] {
        engine.move_dir(owner, &menu, &[], dir, &fixture.cx(Some(first)));
        assert_eq!(engine.current(owner).unwrap().entry, entry);
    }
    assert_eq!(engine.current(OWNER), None, "menu navigation never writes Library focus");
}

#[test]
fn a_compact_menu_keeps_the_host_store_pump_and_deferred_commit_live() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    page.wanted_kind = None;
    let target = page.address(&fixture.cx(None)).unwrap();
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present);
    page.step(&ScreenEvent::Cover, &fixture.cx(None), &mut fx);
    page.step(&ScreenEvent::App(AppMsg::LibraryEdit { target,
        edit: plx_data::stores::browse::QueryEdit::Unwatched(true) }), &fixture.cx(None), &mut fx);
    page.step(&ScreenEvent::Tick(Tick { ms: 80, dt_us: 80_000 }), &fixture.cx(None), &mut fx);
    drop(fx);
    assert!(out.iter().any(|effect| matches!(&effect.fx,
        Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::Addressed {
            work: LibraryWork::Commit { query: Some(plx_data::stores::browse::QueryEdit::Unwatched(true)), .. }, .. }))))));
    assert!(out.iter().any(|effect| matches!(&effect.fx, Fx::App(AppFx::StoreWork(StoreWork::Browse)))),
        "the store must fetch the just-committed query while the compact menu remains open");
}

/// The page asks Browse to re-run a listing whose membership a read-back made stale ONLY once a
/// PAGE has buried it (`WillLeave(Deeper)`, then `Cover`): not on the notice that makes it stale
/// while it is visible, not under a compact menu (which leaves the page visible and ticking), and
/// not again once the re-query has cleared the flag.
#[test]
fn a_stale_listing_is_requeried_when_a_page_buries_it_never_while_visible() {
    use plx_machine::machine::Leave;
    let _guard = plx_base::testlock::serial();
    let requeries = |stale: bool, events: Vec<ScreenEvent<HostFixture>>| {
        let mut fixture = Fixture::new();
        if stale { fixture.listing = fixture.listing.clone().with_membership_stale(); }
        let mut page = fixture.screen();
        page.wanted_kind = None;
        let mut out = Vec::new();
        let mut present = plx_machine::present::Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present);
        for ev in &events { page.step(ev, &fixture.cx(None), &mut fx); }
        drop(fx);
        out.iter().filter(|effect| matches!(&effect.fx, Fx::App(AppFx::Store(StoreId::Browse,
            StoreCmd::Browse(BrowseCmd::Addressed { work: LibraryWork::Requery, .. }))))).count()
    };
    let notice = || ScreenEvent::StoreChanged(StoreId::ViewReconcile.ord(), 1);
    let buried = || vec![ScreenEvent::WillLeave(Leave::Deeper), ScreenEvent::Cover];
    assert_eq!(requeries(true, vec![notice(), ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 })]), 0,
        "a visible page never re-queries under the cursor");
    assert_eq!(requeries(true, buried()), 1, "a page push re-queries once");
    let mut and_back = buried();
    and_back.extend([ScreenEvent::Uncover, ScreenEvent::Enter(Enter::Restored)]);
    assert_eq!(requeries(true, and_back), 1, "Uncover never does");
    let mut and_notice = buried();
    and_notice.push(notice());
    assert_eq!(requeries(true, and_notice), 2,
        "a notice that finds the buried listing still stale asks again; the store clears the flag on the first");
    assert_eq!(requeries(false, buried()), 0, "a listing whose membership is current is left alone");
    let mut returned = buried();
    returned.extend([ScreenEvent::Uncover, ScreenEvent::Enter(Enter::Restored), notice()]);
    assert_eq!(requeries(true, returned), 1, "back on top, a notice no longer re-queries");
}

/// A compact menu (Sort, Filter, the item menu) covers the page without burying it: the page stays
/// visible and ticking, so a read-back that arrives under it never re-queries the grid beneath.
#[test]
fn a_compact_menu_over_a_stale_listing_never_requeries_it() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.listing = fixture.listing.clone().with_membership_stale();
    let mut page = fixture.screen();
    page.wanted_kind = None;
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present);
    for ev in [ScreenEvent::Cover, ScreenEvent::StoreChanged(StoreId::ViewReconcile.ord(), 1),
        ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 })] {
        page.step(&ev, &fixture.cx(None), &mut fx);
    }
    drop(fx);
    assert_eq!(out.iter().filter(|effect| matches!(&effect.fx, Fx::App(AppFx::Store(StoreId::Browse,
        StoreCmd::Browse(BrowseCmd::Addressed { work: LibraryWork::Requery, .. }))))).count(), 0);
}

#[test]
fn published_library_projection_and_layout_enter_canonical_state() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let hash = |page: &LibraryScreen| { let mut c = Canon::new(); page.write(&mut c); c.finish() };
    let mut page = fixture.screen();
    let initial = hash(&page);
    page.pair.detail.swap_for_test(0, 1);
    assert_ne!(initial, hash(&page), "published item order determines the next navigation step");
    let prior = hash(&page);
    page.target_layout.rows += 1;
    assert_ne!(prior, hash(&page), "target document geometry determines reveal and placement");
    let prior = hash(&page);
    page.libraries.push((MORE, usize::MAX));
    assert_ne!(prior, hash(&page), "bounded favorite window determines available controls");
    let prior = hash(&page);
    page.readout = Readout::Failed;
    assert_ne!(prior, hash(&page), "failure control availability is logical state");
}

#[test]
fn a_pending_library_switch_enters_canonical_state() {
    let hash = |page: &LibraryScreen| {
        let mut c = Canon::new();
        page.write(&mut c);
        c.finish()
    };
    let mut page = LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Movie);
    let initial = hash(&page);
    page.switched = true;
    assert_ne!(initial, hash(&page), "the header-focus-keeping switch changes subsequent frames");
}

#[test]
fn pending_semantic_commits_change_the_library_state_hash() {
    fn hash(page: &LibraryScreen) -> u64 {
        let mut canon = Canon::new();
        page.write(&mut canon);
        canon.finish()
    }
    let mut page = LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Movie);
    let empty = hash(&page);
    let section = SectionTarget { epoch: 1, index: 0,
        identity: LibrarySectionIdentity { sid: plx_plex::plex::ServerId::from_raw(0), key: 1 }, kind: SecKind::Movie };
    page.pending.request_section(section.clone());
    let with_section = hash(&page);
    assert_ne!(empty, with_section, "a next-frame section commit must enter canonical state");
    page.pending.request_section(SectionTarget { epoch: 2, ..section.clone() });
    assert_ne!(with_section, hash(&page), "epoch refusal changes the next commit");
    page.pending.request_section(SectionTarget { identity: LibrarySectionIdentity {
        sid: plx_plex::plex::ServerId::from_raw(1), key: 1 }, ..section });
    assert_ne!(with_section, hash(&page), "same section key on another server is a different commit");
    let target = GridTarget { epoch: 1, sid: plx_plex::plex::ServerId::from_raw(0), section: 1, query: 1 };
    let actions = [GridAction::Unwatched { desired: true }, GridAction::Unwatched { desired: false },
        GridAction::Sort { key: "titleSort".into(), desc: false }, GridAction::Sort { key: "titleSort".into(), desc: true },
        GridAction::Sort { key: "addedAt".into(), desc: true }, GridAction::Genre { id: None },
        GridAction::Genre { id: Some("7".into()) }];
    let mut hashes = vec![empty, with_section];
    for action in actions {
        page.pending.request_grid(target.clone(), action);
        let next = hash(&page);
        assert!(!hashes.contains(&next), "distinct semantic actions must have distinct encodings");
        hashes.push(next);
    }
}

// `pub(super)`, not private: `selector_matrix_tests.rs` (a sibling test module) reuses this
// harness wholesale rather than duplicating it, per the guide at the top of that file.
pub(super) struct Fixture {
    stores: Option<plx_data::stores::Stores>,
    listing: plx_data::stores::browse::ListingSnapshot,
    pub(super) directory: plx_data::stores::browse::DirectorySnapshot,
    hubs: plx_data::stores::browse::HubsSnapshot,
    measure: FixtureMeasure,
}

#[test]
fn discovery_failure_retry_targets_the_source_without_a_section() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let sid = plx_plex::plex::ServerId::from_raw(7);
    fixture.listing = plx_data::stores::browse::ListingSnapshot::empty_for_test();
    fixture.directory = plx_data::browse::view::DirectorySnapshot::fixture_source(4, sid,
        plx_data::browse::SrcGroup { name: "Cinema server".into(), handle: "friend".into(),
            state: plx_data::browse::SourceState::Unreachable, tier: None }, SecFetch::Failed);
    let mut page = fixture.screen();
    assert!(fixture.listing.view().id().is_none());
    assert_eq!(page.readout, Readout::Failed);
    assert_eq!(page.status_text(&fixture.cx(None)).0.to_str().unwrap(), "Can\u{2019}t reach Cinema server");
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.activate(RETRY, false, &fixture.cx(Some(page.key(RETRY))),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(out.iter().any(|effect| matches!(&effect.fx,
        Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::RetrySource { epoch: 4, sid: target }))) if *target == sid)),
        "Retry must issue work even when discovery never produced a section address");
}

/// **A failed source whose server discovery offers "Connect without encryption?" for asks it**
/// (PLX-NATIVE-10, code review 3) — the SAME question Home and the sign-in ask
/// (`screens::plaintext_question`): the reason says why, *Connect* replaces *Try again* and opens
/// the question seated on *Not now*, and its *Connect* sends one answer, re-finding this source's
/// slot. Another server's offer does not change this source's read-out.
#[test]
fn a_failed_source_over_an_offered_server_asks_the_shared_question() {
    use plx_plex::plex::session::PlaintextChoice;
    use super::super::plaintext_question::connect;
    let _guard = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    plx_plex::plex::grant::reset_for_test();
    let sid = plx_plex::plex::register_pinned_with_client_id("lan-machine", &plx_plex::plex::Origin::http("192.168.1.50", 32400), "", None, "client", Default::default());
    let mut fixture = Fixture::new();
    fixture.listing = plx_data::stores::browse::ListingSnapshot::empty_for_test();
    fixture.directory = plx_data::browse::view::DirectorySnapshot::fixture_source(4, sid,
        plx_data::browse::SrcGroup { name: "Cinema server".into(), handle: String::new(),
            state: plx_data::browse::SourceState::Unreachable, tier: None }, SecFetch::Failed);
    let mut page = fixture.screen();
    let offer = |machine: &str| plx_plex::plex::grant::offered(plx_plex::plex::grant::scope(),
        plx_plex::plex::grant::PlaintextVerdict { machine_id: machine.into(), name: "nas".into(), shared_by: String::new(),
            eligibility: plx_plex::plex::probe::PlaintextEligibility::Eligible, choice: PlaintextChoice::Undecided });
    let tick = |page: &mut LibraryScreen| {
        let mut out = Vec::new();
        let mut present = plx_machine::present::Present::new();
        page.step(&ScreenEvent::Tick(Tick::default()), &fixture.cx(None),
            &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    };
    offer("another-machine");
    tick(&mut page);
    let cx = fixture.cx(Some(page.key(RETRY)));
    let (caption, reason) = page.status_text(&cx);
    assert_eq!(page.status_overlay(&cx, &caption, reason.as_deref()).action, Some(c"Try again"),
        "another server's offer is not this source's");

    offer("lan-machine");
    tick(&mut page);
    let (caption, reason) = page.status_text(&cx);
    assert!(reason.as_ref().and_then(|r| r.to_str().ok()).is_some_and(|r| r.contains("Select Connect")), "{reason:?}");
    assert_eq!(page.status_overlay(&cx, &caption, reason.as_deref()).action, Some(connect()));
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.activate(RETRY, false, &cx, &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(!out.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::Store(..)))), "Connect asks; it does not retry");
    assert!(page.plaintext_alert.is_open());
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.step(&ScreenEvent::PressCommit(plx_machine::machine::PressId(1)), &fixture.cx(Some(page.key(PLAINTEXT_CONNECT))),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    let answers: Vec<_> = out.iter().filter_map(|e| match &e.fx {
        Fx::App(AppFx::Session(plx_session::auth::SessionCmd::AnswerPlaintext { machine_id, choice, sid: target }))
            if machine_id == "lan-machine" => Some((*choice, *target)),
        _ => None,
    }).collect();
    assert_eq!(answers, [(PlaintextChoice::Allowed, Some(sid))]);
    plx_plex::plex::grant::reset_for_test();
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn failed_and_empty_readouts_offer_only_their_real_owned_controls() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let original = fixture.listing.clone();
    // An empty answer keeps the heading row (its TYPE chip is how the reader leaves the empty
    // listing), so `head` is not `grid`.
    for (fetch, total, expected, grid, head, retry) in [
        (SecFetch::Failed, 36, Readout::Grid, true, true, false),
        (SecFetch::Ready, 0, Readout::Empty, false, true, false),
        (SecFetch::Failed, -1, Readout::Failed, false, false, true),
        (SecFetch::Loading, -1, Readout::Loading, false, false, false),
    ] {
        fixture.listing = original.clone().with_fetch(fetch, total);
        let page = fixture.screen();
        let cx = fixture.cx(None);
        assert_eq!(page.readout, expected);
        let mut groups = Vec::new();
        page.groups(&cx, &mut groups);
        // (A loading listing declares its grid group EMPTY, `PageRegions::declare_pending`: no control.)
        assert_eq!(groups.iter().any(|g| g.id == page.pair.groups_config().detail && g.len > 0), grid);
        // The header row is the way out of every state, so its controls are always placed.
        let _ = head;
        for control in [SORTFILTER] {
            assert!(page.place(&control, &cx, At::SpringTarget).is_some());
        }
        assert_eq!(page.place(&RETRY, &cx, At::SpringTarget).is_some(), retry);
        if !grid {
            assert!(!groups.iter().any(|g| g.id == page.pair.groups_config().master), "stale letters are not a live rail");
        }
        if retry {
            let mut engine = FocusEngine::new();
            engine.enter(OWNER, &page, FocusTarget::ContainerGroup(STATUS_GROUP), None, &cx);
            assert_eq!(engine.current(OWNER), Some(page.key(RETRY)));
        }
    }
}

#[test]
fn foreign_section_replacement_upgrades_a_grid_fade_once() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let mut page = fixture.screen();
    page.grid_fade.reload();
    fixture.listing = fixture.listing.clone().with_section(2, 1);
    page.sync(&fixture.cx(None));
    assert!(page.page_fade.is_swapping(), "foreign replacement remounts the entire document");
    assert_eq!(page.page_fade.alpha(), 0.0);
    assert!(!page.grid_fade.is_swapping());
    page.page_fade.tick(0.04, true);
    let alpha = page.page_fade.alpha();
    page.sync(&fixture.cx(None));
    assert_eq!(page.page_fade.alpha(), alpha, "the same publication must not remount twice");
}

#[test]
fn header_controls_sit_in_the_header_row_left_to_right() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let page = fixture.screen();
    let cx = fixture.cx(None);
    let mut last = f32::MIN;
    for elem in page.toolbar_elems() {
        let rect = page.place(elem, &cx, At::Drawn).unwrap().rect;
        assert!(rect.x >= last, "{elem}: controls read left to right without overlap");
        let lowered = if page.libraries.is_empty() { layout::SINGLE_LIBRARY_DY } else { 0.0 };
        assert_eq!(rect.y + rect.h * 0.5, layout::HEADER_Y + lowered + plx_ui::page_header::HEIGHT * 0.5 - page.scroll.pos);
        last = rect.x + rect.w;
    }
}

#[test]
fn section_grid_memories_do_not_overwrite_one_another() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let first = fixture.listing.clone();
    let mut page = fixture.screen();
    let mut engine = FocusEngine::new();
    let group_a = page.pair.groups_config().detail;
    let card_a = page.key(page.pair.detail.elem_at(17).unwrap());
    engine.set(OWNER, card_a, Some(group_a), By::Restore);
    fixture.listing = first.clone().with_section(1, 2);
    page.sync(&fixture.cx(engine.current(OWNER)));
    let group_b = page.pair.groups_config().detail;
    assert_ne!(group_a, group_b, "each section needs its own engine memory slot");
    let card_b = page.key(page.pair.detail.elem_at(23).unwrap());
    engine.set(OWNER, card_b, Some(group_b), By::Restore);
    fixture.listing = first;
    page.sync(&fixture.cx(engine.current(OWNER)));
    assert_eq!(page.pair.groups_config().detail, group_a);
    assert!(engine.remembered_for(ENTRY).contains(&(group_a, card_a.elem)));
    assert!(engine.remembered_for(ENTRY).contains(&(group_b, card_b.elem)));
    engine.enter(OWNER, &page, FocusTarget::ContainerGroup(group_a), None, &fixture.cx(engine.current(OWNER)));
    assert_eq!(engine.current(OWNER), Some(card_a), "Remembered seating restores the exact section card");
}

#[test]
fn section_viewport_bookmarks_survive_switch_and_evicted_body() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let first = fixture.listing.clone();
    let mut page = fixture.screen();
    let y = page.layout.row_reveal(3);
    page.scroll.jump(y);
    page.scroll_target = y;
    fixture.listing = first.clone().with_section(1, 2);
    page.sync(&fixture.cx(None));
    assert_eq!(page.scroll.pos, 0.0, "a new section starts at its own head");
    let PageMemory::Library(memory) = <LibraryScreen as Screen<HostFixture>>::memory(&page) else { panic!() };
    let mut evicted = LibraryScreen::new(ENTRY, InstanceId(20), SecKind::Movie);
    evicted.restore(&memory);
    evicted.sync(&fixture.cx(None));
    fixture.listing = first;
    for page in [&mut page, &mut evicted] {
        page.sync(&fixture.cx(None));
        assert_eq!(page.scroll.pos, y, "viewport belongs to the section, including after body eviction");
        assert_eq!(page.scroll_target, y);
    }
}
impl Fixture {
    fn shelves(titles: &[&str], count: usize) -> Self {
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
        let listing = fixture.listing.view().id().unwrap();
        let hubs = fixture.hubs.view().id().unwrap();
        assert_eq!((listing.epoch, listing.sid, listing.section), (hubs.epoch, hubs.sid, hubs.section));
        assert_eq!(fixture.hubs.view().shelves().len(), titles.len());
        fixture
    }

    /// 1200 cards, each with a poster (`/poster/{i}`) and a still (`/still/{i}`): a grid that scrolls
    /// far past its window. `episodes` makes it an Episodes library (stills), else Primary (posters).
    fn art_listing(episodes: bool) -> Self {
        let sid = plx_plex::plex::ServerId::from_raw(0);
        let mut fixture = Self::new();
        fixture.listing = plx_data::browse::view::ListingSnapshot::fixture(sid,
            (0..1200).map(|i| Some(plx_data::pms::PmsMovie {
                sid, rk: i.to_string(), kind: if episodes { 3 } else { 0 },
                thumb: format!("/poster/{i}"), still: format!("/still/{i}"),
                ..Default::default()
            })).collect(), Vec::new()).with_library_type(if episodes {
                plx_data::browse::LibraryType::Episodes
            } else { plx_data::browse::LibraryType::Primary });
        fixture
    }

    pub(super) fn new() -> Self {
        let sid = plx_plex::plex::ServerId::from_raw(0);
        let listing = plx_data::browse::view::ListingSnapshot::fixture(sid, (0..36).map(|i|
            Some(plx_data::pms::PmsMovie { sid, rk: format!("{}", i + 1), title: format!("s{i:04x}"), ..Default::default() })).collect(),
            vec![("A".into(), 18), ("Z".into(), 18)]);
        let directory = plx_data::browse::view::DirectorySnapshot::fixture(1, 0, vec![
            plx_data::browse::view::SectionView { sid: Some(sid), key: 1, kind: SecKind::Movie,
                row: plx_data::browse::SrcRow { section: 0, title: "Cinema".into(), pinned: true, current: true, ..Default::default() } }]);
        Self {
            stores: None,
            listing,
            directory,
            hubs: plx_data::stores::browse::HubsSnapshot::empty_for_test(),
            measure: FixtureMeasure,
        }
    }
    pub(super) fn cx(&self, focus: Option<FocusKey<u32>>) -> Cx<'_, HostFixture> {
        Cx { views: Views { listing: self.listing.view(), directory: self.directory.view(), hubs: self.hubs.view() },
            tick: Tick::default(), measure: &self.measure, focus: FocusRead { current: focus , ..Default::default() },
            press: PressRead::default(), owner: OWNER }
    }
    pub(super) fn screen(&self) -> LibraryScreen {
        let mut page = LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Movie);
        page.sync(&self.cx(None));
        page
    }
    /// A source whose discovery failed, with no section address: the Failed read-out. `handle` is
    /// the sharing owner's ("" for the viewer's own server).
    fn failed_source(sid: plx_plex::plex::ServerId, handle: &str) -> Self {
        let mut fixture = Self::new();
        fixture.listing = plx_data::stores::browse::ListingSnapshot::empty_for_test();
        fixture.directory = plx_data::browse::view::DirectorySnapshot::fixture_source(4, sid,
            plx_data::browse::SrcGroup { name: "Cinema server".into(), handle: handle.into(),
                state: plx_data::browse::SourceState::Unreachable, tier: None }, SecFetch::Failed);
        fixture
    }
}
/// One Tick through the screen: whether it damaged the frame.
fn tick_damaged(page: &mut LibraryScreen, fixture: &Fixture) -> bool {
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    present.take(0);
    page.step(&ScreenEvent::Tick(Tick::default()), &fixture.cx(None),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    present.changed()
}
fn deliver(page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture, event: ScreenEvent<HostFixture>) -> usize {
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
fn direction(page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture, dir: Dir) -> usize {
    let mut links = Vec::new();
    <LibraryScreen as Screen<HostFixture>>::links(page, &mut links);
    let outcome = engine.move_dir(OWNER, page, &links, dir, &fixture.cx(engine.current(OWNER)));
    if let Outcome::Moved { from, to, by } = outcome {
        deliver(page, engine, fixture, ScreenEvent::FocusMoved { from, to, by })
    } else { 0 }
}

#[test]
fn projected_entry_preserves_the_last_item_within_a_letter_then_live_move_jumps() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    let mut engine = FocusEngine::new();
    let exact = page.key(page.pair.detail.elem_at(17).unwrap());
    engine.set(OWNER, exact, Some(page.pair.groups_config().detail), By::Restore);
    assert_eq!(direction(&mut page, &mut engine, &fixture, Dir::Right), 0);
    assert_eq!(engine.current_group(OWNER), Some(page.pair.groups_config().master));
    assert_eq!(page.pair.master.start_for_elem(engine.current(OWNER).unwrap().elem), Some(0));
    assert_eq!(engine.remembered_for(ENTRY).iter().find(|(g, _)| *g == page.pair.groups_config().detail).unwrap().1, exact.elem);
    direction(&mut page, &mut engine, &fixture, Dir::Left);
    assert_eq!(engine.current(OWNER), Some(exact));
    direction(&mut page, &mut engine, &fixture, Dir::Right);
    assert_eq!(direction(&mut page, &mut engine, &fixture, Dir::Down), 1);
    direction(&mut page, &mut engine, &fixture, Dir::Left);
    assert_eq!(page.grid_position(engine.current(OWNER)), Some((3, 0)));
}

#[test]
fn removed_grid_key_keeps_its_typed_master_detail_reconciliation_path() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let mut page = fixture.screen();
    let original = page.key(page.pair.detail.elem_at(17).unwrap());
    let sid = plx_plex::plex::ServerId::from_raw(0);
    fixture.listing = plx_data::browse::view::ListingSnapshot::fixture(sid,
        (0..35).map(|i| Some(plx_data::pms::PmsMovie { sid, rk: format!("{}", i + 100), ..Default::default() })).collect(),
        vec![("A".into(), 18), ("Z".into(), 17)]);
    page.sync(&fixture.cx(Some(original)));
    assert_eq!(<LibraryScreen as Focusable<HostFixture>>::group_of(&page, &original.elem, &fixture.cx(Some(original))), None);
    let recovered = <LibraryScreen as Focusable<HostFixture>>::reconcile(&page, original, &fixture.cx(Some(original)));
    assert_eq!(page.grid_position(Some(recovered)), Some((2, 5)));
}

#[test]
fn direct_rail_activation_jumps_even_when_the_letter_was_already_selected() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    let mut engine = FocusEngine::new();
    let exact = page.key(page.pair.detail.elem_at(17).unwrap());
    engine.set(OWNER, exact, Some(page.pair.groups_config().detail), By::Restore);
    direction(&mut page, &mut engine, &fixture, Dir::Right);
    let letter = engine.current(OWNER).unwrap().elem;
    assert_eq!(deliver(&mut page, &mut engine, &fixture, ScreenEvent::Activate(letter)), 1);
    direction(&mut page, &mut engine, &fixture, Dir::Left);
    assert_eq!(page.grid_position(engine.current(OWNER)), Some((0, 0)));
}

#[test]
fn sort_chosen_during_section_fade_commits_to_the_incoming_library() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let sid = plx_plex::plex::ServerId::from_raw(0);
    fixture.directory = plx_data::browse::view::DirectorySnapshot::fixture(1, 0, (0..2).map(|i|
        plx_data::browse::view::SectionView { sid: Some(sid), key: i as i64 + 1, kind: SecKind::Movie,
            row: plx_data::browse::SrcRow { section: i, title: format!("s{i:04x}"), pinned: true, current: i == 0, ..Default::default() } }).collect());
    let mut page = fixture.screen();
    let mut output = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let mut fx = Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present);
    page.activate(page.libraries[1].0, false, &fixture.cx(None), &mut fx);
    page.activate(SORTFILTER, false, &fixture.cx(None), &mut fx);
    drop(fx);
    let incoming = SectionAddress { epoch: 1, sid, section: 2 };
    let target = output.iter().find_map(|effect| match &effect.fx {
        Fx::App(AppFx::Library(LibraryReq::Menu { target, .. })) => Some(*target), _ => None,
    }).unwrap();
    assert_eq!(target, incoming, "menu intent follows the requested page during its outgoing fade");
    output.clear();
    let mut fx = Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present);
    page.step(&ScreenEvent::App(AppMsg::LibraryEdit { target,
        edit: plx_data::stores::browse::QueryEdit::Sort { key: "titleSort".into(), desc: true } }), &fixture.cx(None), &mut fx);
    page.step(&ScreenEvent::WillLeave(plx_machine::machine::Leave::Deeper), &fixture.cx(None), &mut fx);
    drop(fx);
    assert!(output.iter().any(|effect| matches!(&effect.fx,
        Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::Addressed {
            target, work: LibraryWork::Commit { select: true, query: Some(plx_data::stores::browse::QueryEdit::Sort { key, desc: true }), .. }
        }))) if *target == incoming && key == "titleSort")), "leaving flushes selection and semantic sort in one addressed store command");
}

/// Issue #100/#165: a Guest or managed profile's own household server always arrives from
/// `/resources` with `owned: false` (`plex/account.rs:1128-1157`), so on such a profile EVERY
/// section it sees would once have read as `borrowed`. The 0.6.x server picker's old singleton
/// exception — carried forward wholesale into the restructure by `3c2de7ad` — read that as "still
/// ambiguous" and left the legacy `Library · <name> ⌄` chip up, opening the old Sources popover.
/// That is exactly the symptom the owner saw on a real TV's TV Shows page under `--guest`, and
/// exactly the gap the maintainer named on #100: "the picker only handles multiple servers, not
/// multiple libraries per section."
///
/// The fix is stronger than "ownership no longer decides this": `SectionView` carries no
/// ownership bit at all any more, so there is nothing left FOR ownership to decide — a lone
/// favourite clears the selector unconditionally, on every profile, because the published view
/// has no field left to ask the old exception's question. Before the `borrowed` field's deletion
/// this test proved the weaker claim by looping over `[true, false]`; with the field gone the loop
/// has nothing to vary, so this now asserts the one remaining case directly.
#[test]
fn a_single_favourite_library_draws_no_selector() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.directory = plx_data::browse::view::DirectorySnapshot::fixture(1, 0, vec![
        plx_data::browse::view::SectionView { sid: Some(plx_plex::plex::ServerId::from_raw(0)), key: 1,
            kind: SecKind::Movie, row: plx_data::browse::SrcRow { section: 0, title: "Cinema".into(),
                pinned: true, current: true, ..Default::default() } }]);
    let page = fixture.screen();
    assert!(page.libraries.is_empty(), "a lone favourite must clear the selector");
    assert!(page.layout.libraries, "the header row is always reserved, names or not");
    let cx = fixture.cx(None);
    let mut groups = Vec::new();
    page.groups(&cx, &mut groups);
    assert_eq!(page.header_elems(), page.toolbar_elems(), "the header row holds only the controls");
    let mut draw = DrawFrame::new(&cx, plx_ui::Painter::root());
    page.record_stops(&mut draw);
    let stops = draw.into_stops();
    assert!(!stops.iter().any(|stop| region_of_elem(stop.key.elem) == Some(KeyRegion::Library)),
        "no pointer stop is registered for the selector row");
}

#[test]
fn favorite_library_row_uses_shared_strip_geometry_and_incoming_type() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let sid = plx_plex::plex::ServerId::from_raw(0);
    fixture.directory = plx_data::browse::view::DirectorySnapshot::fixture(1, 0, (0..4).map(|i|
        plx_data::browse::view::SectionView { sid: Some(sid), key: i as i64 + 1,
            kind: if i < 2 { SecKind::Movie } else { SecKind::Show },
            row: plx_data::browse::SrcRow { section: i, title: format!("Library {i}"), pinned: true,
                current: i == 0, ..Default::default() } }).collect());
    let mut page = fixture.screen();
    let cx = fixture.cx(None);
    for (elem, _) in page.libraries.clone() {
        let rect = page.place(&elem, &cx, At::Drawn).unwrap().rect;
        assert_eq!(rect.y + rect.h * 0.5, layout::HEADER_Y + plx_ui::page_header::HEIGHT * 0.5, "names sit in the header row");
    }
    page.pending.request_section(SectionTarget { epoch: 1, index: 3,
        identity: LibrarySectionIdentity { sid, key: 4 }, kind: SecKind::Show });
    page.sync(&cx);
    assert_eq!(page.libraries.iter().map(|(_, section)| *section).collect::<Vec<_>>(), vec![2, 3],
        "the favorite row must not keep movie libraries beneath the incoming Shows type");
}

/// **The page glyph and the Library's own live tab strip never overlap.** They used to (measured
/// 254-216 = 38px, `status.rs`'s own comment: "the Library keeps its chrome live ABOVE the
/// read-out" while the glyph box sat fixed at 216..328, anchor 372) whenever a failed section's
/// page still carried other, populated tabs; with the anchor at 540 the natural box starts at 384,
/// clear of the strip with room to spare. `status_overlay` still passes `glyph_ceiling` at the tab strip's
/// bottom (`CONTENT_TOP..CONTENT_TOP+CTRL_H` = library/layout's `CONTENT_TOP` =
/// `ui::consts::GRID_TOP_Y` = 194, height `StatusOverlay::CTRL_H` = 60, so screen y 194..254)
/// whenever `self.libraries` — the tabs — is non-empty, and `StatusOverlay::glyph_rect` would shrink
/// the glyph box to clear it rather than let the two overlap. At this anchor it never has to: with
/// or without a live strip the glyph draws at its natural, unshrunk 384..496 box.
#[test]
fn the_page_glyph_and_the_librarys_live_tab_strip_never_overlap() {
    use plx_ui::widgets::StatusOverlay;
    let _guard = plx_base::testlock::serial();
    let tab_strip_bottom = CONTENT_TOP + StatusOverlay::CTRL_H;
    assert_eq!(tab_strip_bottom, 208.0, "the tab strip band moved — re-measure the fix against it");

    // A live strip: two pinned sections of the current kind. `favorite_sections_for` only
    // populates `self.libraries` for 2+ candidates (see
    // `favorite_library_row_uses_shared_strip_geometry_and_incoming_type` above; a single
    // favourite clears it), so this is the minimal fixture that actually turns the strip on.
    let sid = plx_plex::plex::ServerId::from_raw(0);
    let sections = vec![
        plx_data::browse::view::SectionView { sid: Some(sid), key: 1, kind: SecKind::Movie,
            row: plx_data::browse::SrcRow { section: 0, title: "Cinema".into(), pinned: true, current: true, ..Default::default() } },
        plx_data::browse::view::SectionView { sid: Some(sid), key: 2, kind: SecKind::Movie,
            row: plx_data::browse::SrcRow { section: 1, title: "Anime".into(), pinned: true, ..Default::default() } },
    ];
    let mut fixture = Fixture::new();
    fixture.directory = plx_data::browse::view::DirectorySnapshot::fixture(1, 0, sections);
    fixture.listing = fixture.listing.clone().with_fetch(SecFetch::Failed, -1);
    let page = fixture.screen();
    assert_eq!(page.readout, Readout::Failed);
    assert!(!page.libraries.is_empty(), "fixture must actually produce a live tab strip, or this test proves nothing");
    let cx = fixture.cx(Some(page.key(RETRY)));
    let (caption, reason) = page.status_text(&cx);
    let overlay = page.status_overlay(&cx, &caption, reason.as_deref());
    let glyph = overlay.glyph_frame().expect("a live strip never omits the glyph at this anchor");
    assert!(glyph.y >= tab_strip_bottom + StatusOverlay::GLYPH_CEILING_MARGIN,
        "the glyph box (top y={}) must start clear of the tab strip's bottom (y={tab_strip_bottom})", glyph.y);
    assert_eq!(glyph.h, StatusOverlay::GLYPH_SIZE, "the anchor sits low enough that the strip no longer reaches \
        the glyph, so a live strip costs it nothing — the ceiling is only a guard now");

    // No live strip (`Fixture::new`'s single, un-favourited-enough section clears
    // `self.libraries`): the glyph is unaffected, at its natural, unshrunk position.
    let mut solo = Fixture::new();
    solo.listing = solo.listing.clone().with_fetch(SecFetch::Failed, -1);
    let page = solo.screen();
    assert!(page.libraries.is_empty(), "fixture must NOT have a live strip, or this half proves nothing");
    let cx = solo.cx(Some(page.key(RETRY)));
    let (caption, reason) = page.status_text(&cx);
    let overlay = page.status_overlay(&cx, &caption, reason.as_deref());
    let glyph = overlay.glyph_frame().expect("no ceiling at all must never omit the glyph");
    assert_eq!(glyph.y, StatusOverlay::FULL_ANCHOR_TOP - StatusOverlay::GLYPH_GAP - StatusOverlay::GLYPH_SIZE,
        "without a live strip the glyph keeps its natural, unshrunk position");
    assert_eq!(glyph.h, StatusOverlay::GLYPH_SIZE, "without a live strip the glyph keeps its natural, unshrunk size");
}

mod type_tests { include!("type_tests.rs"); }

mod collections_listing_tests { include!("collections_listing_tests.rs"); }

mod art_admission_tests { include!("art_admission_tests.rs"); }
mod lookahead_tests { include!("lookahead_tests.rs"); }

mod grid_motion_tests { include!("grid_motion_tests.rs"); }
mod keys_window_tests { include!("keys_window_tests.rs"); }

/// BACK from deep in the document returns to the top on the scroll SPRING, the way Home's BACK
/// glides its page back to the hero — it must not snap. The reseat BACK raises arrives as
/// `By::Restore`, and `reveal` used to treat every `Restore` as "the saved viewport": it jumped the
/// spring onto its target even when no viewport had been saved, so BACK on Movies and TV Shows cut
/// to the head while Home animated.
#[test]
fn back_to_the_top_glides_on_the_scroll_spring_instead_of_snapping() {
    let _guard = plx_base::testlock::serial();
    assert_back_glides(&Fixture::new(), 35);
    let session = plx_plex::plex::session::TempSession::new("library-back-glide");
    session.watching("u-library-back-glide");
    assert_back_glides(&Fixture::shelves(&["movie.inprogress.1", "movie.recent.1"], 12), 100);
}

fn assert_back_glides(fixture: &Fixture, deep_index: usize) {
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    let elem = deep(&mut page, fixture, deep_index);
    let deep = page.key(elem);
    page.relayout(Some(deep));
    page.scroll_target = page.target_layout.row_reveal(deep_index / page.layout.cols());
    page.scroll.jump(page.scroll_target);
    page.relayout(Some(deep));
    engine.set(OWNER, deep, Some(page.pair.groups_config().detail), By::Restore);
    let deep_scroll = page.scroll.pos;
    assert!(deep_scroll > 100.0, "the fixture must be scrolled well down the document");

    // BACK, then the reseat it raises routed the way the dispatcher routes it.
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let back = ScreenEvent::Input(plx_machine::machine::InputEvent {
        at: Tick::default(),
        source: plx_machine::machine::Source::Script,
        kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
    });
    page.step(&back, &fixture.cx(Some(deep)),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    let mut moved = false;
    for effect in out {
        let Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus }))) = effect.fx else { continue };
        let outcome = engine.enter(OWNER, &page, focus, None, &fixture.cx(engine.current(OWNER)));
        if let Outcome::Moved { from, to, by } = outcome {
            deliver(&mut page, &mut engine, fixture, ScreenEvent::FocusMoved { from, to, by });
            moved = true;
        }
    }
    assert!(moved, "BACK must move focus to the head of the page");
    assert_eq!(page.scroll_target, 0.0, "BACK targets the top");
    assert_eq!(page.scroll.pos, deep_scroll, "the spring has not been teleported onto its target");

    // …and the spring then carries it there over frames, monotonically.
    let mut last = page.scroll.pos;
    let mut frames = 0;
    while page.scroll.pos > 0.5 && frames < 240 {
        frames += 1;
        let mut out = Vec::new();
        page.step(&ScreenEvent::Tick(Tick { ms: frames * 16, dt_us: 16_000 }), &fixture.cx(engine.current(OWNER)),
            &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
        assert!(page.scroll.pos <= last + 0.01, "the glide never moves back down");
        last = page.scroll.pos;
    }
    assert!(frames > 4, "reaching the top takes frames (took {frames}), it is not a cut");
    assert!(page.scroll.pos <= 0.5, "and it arrives");
}
