//! Migration inventory for the 73 legacy `ui/detail.rs` regressions.
//!
//! Geometry entries marked `ui/detail_layout.rs` are intentionally owned by Luna's shared-helper
//! port because Home and diagnostics consume the same contract. Every other entry remains in this
//! package, either in this module or beside the factored helper it exercises.

use super::*;
use crate::ui::machine::{Chrome, Host, InputEvent, PressRead, ScreenId};
use crate::ui::screen::{At, Focusable, ScreenArg};

#[derive(Clone, PartialEq, Eq)]
struct TestArg;
impl LogicalState for TestArg {
    fn write(&self, c: &mut Canon) { c.u32(0); }
    fn probe(&self, _: &mut String) {}
}

impl ScreenArg for TestArg {
    fn chrome(&self) -> Chrome {
        Chrome::None
    }
    fn id(&self) -> ScreenId {
        ScreenId(700)
    }
    fn title(&self) -> Option<&str> {
        None
    }
    fn same_instance(&self, other: &Self) -> bool {
        self == other
    }
}

struct TestHost;

impl Host for TestHost {
    type Arg = TestArg;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = ();
    // `super::super::` (detail -> screens -> family) rather than the absolute spelling: `family`
    // is the Settings family's shared vocabulary, not a sibling screen — see `screens::family`'s
    // own module doc, and `screens::legal`/`screens::settings`'s identical `super::family::` use.
    type Init = super::super::family::NoInit;
    type Memory = PageMemory;
}

fn cx<'a>(measure: &'a dyn crate::ui::machine::Measure, elem: Option<u32>) -> Cx<'a, TestHost> {
    Cx {
        views: (),
        tick: Default::default(),
        measure,
        press: PressRead::default(),
        focus: crate::ui::machine::FocusRead {
            current: elem.map(|elem| FocusKey {
                entry: EntryId(7),
                elem,
            }),
        ..Default::default() },
        owner: crate::ui::machine::InputOwner::Entry(EntryId(7)),
    }
}

// Synchronizing the identity registry and querying/hash-writing a screen read shared stores
// and legacy panels. Require the caller's guard; acquiring one here would deadlock install().
fn bare(_guard: &crate::testlock::Serial, sid: ServerId, rk: &str) -> DetailScreen {
    let mut screen = DetailScreen {
        entry: EntryId(7),
        sid,
        rk: rk.into(),
        keys: vec![], next_elem: FIRST_ITEM_ELEM, key_by_local: Default::default(),
        local_by_key: Default::default(), return_pending: false,
        pending_season: None,
        season_settle: 0.0,
        preview_dwell: 0.0,
        preview_promoted: false,
        preview_art: 1.0,
        preview_prose: 1.0,
        preview_synopsis: 1.0,
        preview_chrome: 1.0,
        preview_field: 1.0,
        preview_base_scrim: 1.0,
        preview_logo: Spring::at(0.0),
        preview_played_for: None,
        preview_started_for: None,
        preview_had_picture: false,
        trailer_ctl: trailer::Transport::IDLE,
        refresh: DetailRefreshPhase::None,
        restore_intent: None,
        scroll: Spring::at(0.0),
        scroll_target: 0.0,
        episode_scroll: Spring::at(0.0),
        tab_scroll: Spring::at(0.0),
        episode_scale: [Spring::at(1.0); EP_SCALE_MAX],
        related: CardRow::new(),
        extras: CardRow::new(),
        cast: CardRow::new(),
        tabs: TabStrip::new(),
        season_pop: CtlPop::new(),
        ctl_pop: CtlPop::new(),
        disc_unfurl: [Spring::at(0.0); 3],
        season_metrics: season::Metrics::new(),
        about_rows: about::Rows::new(),
        ground: AmbientWash::flat(theme::SURFACE_APP),
        selected: {
            let pms_state = crate::pms::PmsState::default();
            crate::pms::movie(&pms_state, crate::pms::index_of_rk(&pms_state, sid, rk).max(0) as usize)
                .filter(|_| crate::pms::index_of_rk(&pms_state, sid, rk) >= 0)
                .cloned()
        },
        spin_ms: 0.0,
        spin_phase: crate::ui::motion::Phase::default(),
        layout: std::cell::Cell::new(None),
    };
    screen.sync_keys();
    screen
}

fn season(index: i64) -> crate::metadata::Season {
    crate::metadata::Season {
        rk: format!("season-{index}"),
        index,
        title: format!("Season {index}"),
        leaf_count: 2,
        viewed_leaf_count: 0,
    }
}

fn episode(rk: &str, index: i64) -> crate::metadata::Episode {
    crate::metadata::Episode {
        rk: rk.into(),
        index,
        season: 1,
        title: format!("Episode {index}"),
        dur_ms: 60_000,
        ..Default::default()
    }
}

fn detail(sid: ServerId, rk: &str) -> Detail {
    Detail {
        sid,
        rk: rk.into(),
        is_show: true,
        kind: "show".into(),
        seasons: vec![season(1), season(2)],
        episodes: vec![episode("e1", 1), episode("e2", 2)],
        ..Default::default()
    }
}

fn install(d: Detail) -> crate::testlock::Serial {
    let guard = crate::testlock::serial();
    crate::metadata::set_current_for_test(Some(d));
    guard
}

fn clear() {
    crate::metadata::set_current_for_test(None);
    // The *Also available* store outlives a page, so a test that seeded it hands the next one an
    // empty one — the addressed store cannot MIS-answer, but it can answer for an item a later
    // test happens to reuse the pair of.
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::AltInstall {
        sid: crate::plex::ServerId::UNSET,
        rk: String::new(),
        copies: Vec::new(),
    });
}

fn step(
    screen: &mut DetailScreen,
    event: &ScreenEvent<TestHost>,
    focus: Option<u32>,
) -> (Handled, Vec<crate::ui::machine::Stamped<TestHost>>) {
    screen.sync_keys();
    let focus = focus.and_then(|key| screen.engine_key(key).or(Some(key)));
    let translated;
    let event = match event {
        ScreenEvent::Activate(elem) => {
            translated = ScreenEvent::Activate(screen.engine_key(*elem).unwrap_or(*elem));
            &translated
        }
        ScreenEvent::FocusMoved { from, to, by } => {
            translated = ScreenEvent::FocusMoved { from: *from,
                to: FocusKey { entry: to.entry, elem: screen.engine_key(to.elem).unwrap_or(to.elem) }, by: *by };
            &translated
        }
        _ => event,
    };
    let measure = crate::ui::fixture::FixtureMeasure;
    let context = cx(&measure, focus);
    let mut effects = Vec::new();
    let mut present = crate::ui::present::Present::new();
    let handled = {
        let mut sink = Effects::new(
            &mut effects,
            crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(1)),
            &mut present,
        );
        Machine::<TestHost>::step(screen, event, &context, &mut sink)
    };
    (handled, effects)
}

#[test]
fn logical_hash_names_the_mounted_item() {
    let _guard = crate::testlock::serial();
    assert_ne!(
        bare(&_guard, ServerId::UNSET, "a").hash(),
        bare(&_guard, ServerId::UNSET, "b").hash()
    );
}

#[test]
fn logical_hash_names_the_full_restore_target() {
    let _guard = crate::testlock::serial();
    let mut a = bare(&_guard, ServerId::UNSET, "show");
    let mut b = bare(&_guard, ServerId::UNSET, "show");
    let mut left = Spot {
        section: 2,
        col: 1,
        ep_text: true,
        saved_col: [0, 1, 2, 3, 4, 5, 0],
        season: Some(2),
    };
    let right = left.clone();
    left.col = 0;
    a.restore_episode(&left, Some("e1"));
    b.restore_episode(&right, Some("e2"));
    assert_ne!(a.hash(), b.hash());
    let settled = b.hash();
    b.refresh = DetailRefreshPhase::Deferred;
    assert_ne!(settled, b.hash(), "a future reconciliation request is logical state");
}

#[test]
fn logical_hash_names_the_debounce_deadline() {
    let _guard = crate::testlock::serial();
    let mut a = bare(&_guard, ServerId::UNSET, "show");
    let mut b = bare(&_guard, ServerId::UNSET, "show");
    a.pending_season = Some(1);
    b.pending_season = Some(1);
    a.season_settle = 0.05;
    b.season_settle = 0.15;
    assert_ne!(a.hash(), b.hash());
}

#[test]
fn refresh_obligation_is_logical_state_without_a_restore_intent() {
    let guard = crate::testlock::serial();
    let mut screen = bare(&guard, ServerId::UNSET, "show");
    let none = screen.hash();
    screen.refresh = DetailRefreshPhase::Deferred;
    let deferred = screen.hash();
    screen.refresh = DetailRefreshPhase::Requested;
    let requested = screen.hash();
    assert_ne!(none, deferred);
    assert_ne!(none, requested);
    assert_ne!(deferred, requested);
    let mut probe = String::new();
    screen.probe(&mut probe);
    assert!(probe.contains("restore=false") && probe.contains("refresh=Requested"));
}

#[test]
fn detail_enter_preserves_the_refresh_truth_table_without_focus_restoration() {
    let guard = crate::testlock::serial();
    let sid = ServerId::UNSET;
    for cached in [false, true] {
        for phase in [DetailRefreshPhase::None, DetailRefreshPhase::Deferred, DetailRefreshPhase::Requested] {
            for status in [None, Some(true), Some(false)] {
                apply_metadata(MetadataCmd::Clear);
                crate::metadata::set_current_for_test(cached.then(|| detail(sid, "show")));
                let pending = status.and_then(|loading| {
                    let generation = crate::metadata::begin_detail_for_test(sid, "show");
                    if !loading {
                        crate::metadata::land_detail_for_test(sid, "show", generation, None);
                    }
                    loading.then_some(generation)
                });
                assert_eq!(crate::metadata::detail_request_status(sid, "show"), status);
                let mut screen = bare(&guard, sid, "show");
                screen.refresh = phase;
                let generation = crate::metadata::detail_generation_for_test();
                step(&mut screen, &ScreenEvent::Enter(crate::ui::screen::Enter::Restored), None);
                let requests = match phase {
                    DetailRefreshPhase::Deferred => 1,
                    DetailRefreshPhase::Requested => u32::from(status.is_none()),
                    DetailRefreshPhase::None => u32::from(!cached && status != Some(true)),
                };
                assert_eq!(crate::metadata::detail_generation_for_test(), generation + requests,
                    "cached={cached} phase={phase:?} status={status:?}");
                screen.pump_restore();
                let expected = match (phase, status) {
                    (DetailRefreshPhase::None, _) | (DetailRefreshPhase::Requested, Some(false)) => DetailRefreshPhase::None,
                    _ => DetailRefreshPhase::Requested,
                };
                assert_eq!(screen.refresh, expected,
                    "cached={cached} phase={phase:?} status={status:?} after={:?}",
                    crate::metadata::detail_request_status(sid, "show"));
                assert!(screen.restore_intent.is_none());
                // Synthetic workers still owe a terminal acknowledgment after supersession;
                // otherwise this matrix exhausts the production reservation budget itself.
                if let Some(generation) = pending {
                    crate::metadata::land_detail_for_test(sid, "show", generation, None);
                }
                if requests > 0 {
                    crate::metadata::land_detail_for_test(sid, "show", generation + requests, None);
                }
                crate::stores::metadata::pump_detail();
            }
        }
    }
    apply_metadata(MetadataCmd::Clear);
    clear();
}

#[test]
fn restore_memory_cannot_rewind_a_newer_refresh_obligation() {
    let guard = install(detail(ServerId::UNSET, "show"));
    for phase in [DetailRefreshPhase::Deferred, DetailRefreshPhase::Requested] {
        for cancelled in [false, true] {
            let mut screen = bare(&guard, ServerId::UNSET, "show");
            let PageMemory::Detail(memory) = Screen::<TestHost>::memory_at(&screen, None) else { unreachable!() };
            let spot = Spot { section: 2, season: Some(2), ..Default::default() };
            screen.restore_episode(&spot, Some("e2"));
            screen.refresh = phase;
            if cancelled { screen.restore_intent = None; }
            step(&mut screen, &ScreenEvent::RestoreMemory(PageMemory::Detail(memory)), None);
            assert_eq!(screen.refresh, phase, "old navigation memory cannot discharge the write");
            if !cancelled {
                let intent = screen.restore_intent.as_ref().unwrap();
                assert_eq!(intent.spot, spot);
                assert_eq!(intent.episode.as_deref(), Some("e2"));
            }
        }
    }
    clear();
}

#[test]
fn cancelled_focus_restoration_still_terminates_reconciliation_on_success_or_failure() {
    let guard = crate::testlock::serial();
    let sid = ServerId::UNSET;
    for success in [false, true] {
        apply_metadata(MetadataCmd::Clear);
        crate::metadata::set_current_for_test(Some(detail(sid, "show")));
        let generation = crate::metadata::begin_detail_for_test(sid, "show");
        let mut screen = bare(&guard, sid, "show");
        screen.restore_episode(&Spot::default(), Some("e2"));
        screen.refresh = DetailRefreshPhase::Requested;
        step(&mut screen, &ScreenEvent::Input(crate::ui::machine::InputEvent {
            at: Default::default(), source: crate::ui::machine::Source::Script,
            kind: InputKind::Key { key: Key::Down, edge: Edge::Down, sym: 0, wcode: 0, at_edge: false },
        }), None);
        assert!(screen.restore_intent.is_none());
        assert_eq!(screen.refresh, DetailRefreshPhase::Requested);
        assert_eq!(crate::metadata::land_detail_for_test(sid, "show", generation,
            success.then(|| detail(sid, "show"))), success);
        step(&mut screen, &ScreenEvent::StoreChanged(StoreId::Metadata.ord(), generation), None);
        assert_eq!(screen.refresh, DetailRefreshPhase::None);
        assert!(screen.restore_intent.is_none());
        screen.pump_restore();
        assert_eq!(crate::metadata::detail_generation_for_test(), generation);
    }
    apply_metadata(MetadataCmd::Clear);
    clear();
}

#[test]
fn a_spot_round_trips_through_the_page_it_describes() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.cur_season = 1;
    d.related = (0..6)
        .map(|i| crate::pms::PmsMovie {
            sid,
            rk: format!("r{i}"),
            ..Default::default()
        })
        .collect();
    let _guard = install(d);
    let mut screen = bare(&_guard, sid, "show");
    let measure = crate::ui::fixture::FixtureMeasure;
    for (elem, section, col, text) in [
        (hero::ELEM_PLAY, 0, 0, false),
        (hero::ELEM_MARK_WATCHED, 0, 1, false),
        (season::elem(1).unwrap(), 1, 1, false),
        (
            episodes::elem(1, episodes::Row::Still).unwrap(),
            2,
            1,
            false,
        ),
        (episodes::elem(1, episodes::Row::Text).unwrap(), 2, 1, true),
        (related::elem(5).unwrap(), 3, 5, false),
        (about::CARD_ELEM, 5, 0, false),
    ] {
        let elem = screen.engine_key(elem).expect("fixture item key");
        let spot = screen.spot(Some(FocusKey {
            entry: EntryId(7),
            elem,
        }));
        assert_eq!((spot.section, spot.col, spot.ep_text), (section, col, text));
        screen.restore(&spot);
        let restored = Focusable::<TestHost>::reconcile(
            &screen,
            FocusKey {
                entry: EntryId(7),
                elem: hero::ELEM_PLAY,
            },
            &cx(&measure, None),
        );
        assert_eq!(restored.elem, elem);
        screen.restore_intent = None;
    }
    crate::metadata::set_current_for_test(Some(Detail {
        sid,
        rk: "movie".into(),
        kind: "movie".into(),
        part: "/library/parts/1".into(),
        ..Default::default()
    }));
    let movie = bare(&_guard, sid, "movie");
    let languages = movie.spot(Some(FocusKey {
        entry: EntryId(7),
        elem: about::LANGUAGES_ELEM,
    }));
    assert_eq!((languages.section, languages.col), (5, 2));
    clear();
}

#[test]
fn a_restored_spot_clamps_onto_an_item_whose_lists_shrank() {
    let sid = ServerId::UNSET;
    let _guard = crate::testlock::serial();
    crate::metadata::set_current_for_test(None);
    let mut screen = bare(&_guard, sid, "show");
    let measure = crate::ui::fixture::FixtureMeasure;
    let want = FocusKey {
        entry: EntryId(7),
        elem: hero::ELEM_PLAY,
    };
    let spot = Spot {
        section: 3,
        col: 5,
        season: Some(1),
        ..Default::default()
    };
    screen.restore(&spot);
    assert_eq!(
        Focusable::<TestHost>::reconcile(&screen, want, &cx(&measure, None)).elem,
        hero::ELEM_PLAY,
        "a restore with no Detail yet stays on the safe hero fallback"
    );

    let mut d = detail(sid, "show");
    d.related = vec![Default::default(), Default::default()];
    crate::metadata::set_current_for_test(Some(d));
    screen.restore(&spot);
    assert_eq!(
        Focusable::<TestHost>::reconcile(&screen, want, &cx(&measure, None)).elem,
        screen.engine_key(related::elem(1).unwrap()).unwrap(),
        "a remembered column clamps to the last item in the surviving row"
    );
    let mut no_related = detail(sid, "show");
    no_related.related.clear();
    crate::metadata::set_current_for_test(Some(no_related));
    screen.restore(&spot);
    assert_eq!(
        Focusable::<TestHost>::reconcile(&screen, want, &cx(&measure, None)).elem,
        hero::ELEM_PLAY,
        "a missing remembered row falls back to the always-present hero"
    );
    clear();
}

#[test]
fn a_movie_spot_does_not_wait_for_a_season_that_will_never_land() {
    let sid = ServerId::UNSET;
    let _guard = install(Detail {
        sid,
        rk: "movie".into(),
        kind: "movie".into(),
        ..Default::default()
    });
    let mut screen = bare(&_guard, sid, "movie");
    screen.restore(&Spot {
        section: 5,
        season: None,
        ..Default::default()
    });
    let measure = crate::ui::fixture::FixtureMeasure;
    let restored = Focusable::<TestHost>::reconcile(
        &screen,
        FocusKey {
            entry: EntryId(7),
            elem: hero::ELEM_PLAY,
        },
        &cx(&measure, None),
    );
    assert_eq!(restored.elem, about::CARD_ELEM);
    assert!(screen
        .restore_intent
        .as_ref()
        .is_some_and(|r| !r.season_requested));
    clear();
}

#[test]
fn the_filmstrips_text_row_opens_that_episodes_own_page() {
    let d = detail(ServerId::UNSET, "show");
    let key = episodes::elem(1, episodes::Row::Text).unwrap();
    assert_eq!(
        episodes::action(&d, key, false),
        episodes::Action::OpenDetail(ServerId::UNSET, "e2".into())
    );
    let _guard = install(d);
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    let (_, effects) = step(
        &mut screen,
        &ScreenEvent::PressCommit(crate::ui::machine::PressId(1)),
        Some(key),
    );
    assert!(effects.iter().any(|effect| matches!(
        &effect.fx,
        Fx::App(AppFx::Content(ContentReq::Push(ContentArg::Detail { rk, .. }))) if rk == "e2"
    )));
    clear();
}

#[test]
fn the_related_shelf_raises_the_same_open_request() {
    let mut empty = detail(ServerId::UNSET, "show");
    empty.related = vec![Default::default()];
    assert_eq!(
        related::action(&empty, related::elem(0).unwrap()),
        related::Action::None,
        "a row with no ratingKey is not a destination"
    );
    let mut d = detail(ServerId::UNSET, "show");
    d.related = vec![crate::pms::PmsMovie {
        sid: ServerId::UNSET,
        rk: "related".into(),
        ..Default::default()
    }];
    assert_eq!(
        related::action(&d, related::elem(0).unwrap()),
        related::Action::OpenDetail(ServerId::UNSET, "related".into())
    );
    let _guard = install(d);
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    let (_, effects) = step(
        &mut screen,
        &ScreenEvent::PressCommit(crate::ui::machine::PressId(1)),
        Some(related::elem(0).unwrap()),
    );
    assert!(effects.iter().any(|effect| matches!(
        &effect.fx,
        Fx::App(AppFx::Content(ContentReq::Push(ContentArg::Detail { rk, .. }))) if rk == "related"
    )));
    clear();
}

#[test]
fn an_open_request_does_not_outlive_its_page() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.related = vec![crate::pms::PmsMovie {
        sid,
        rk: "related".into(),
        ..Default::default()
    }];
    let _guard = install(d);
    let mut screen = bare(&_guard, sid, "show");
    let press = ScreenEvent::PressCommit(crate::ui::machine::PressId(1));
    let (_, first) = step(&mut screen, &press, Some(related::elem(0).unwrap()));
    assert!(first
        .iter()
        .any(|effect| matches!(&effect.fx, Fx::App(AppFx::Content(ContentReq::Push(_))))));
    let mut replacement = bare(&_guard, sid, "replacement");
    let (_, replacement_effects) = step(&mut replacement, &press, Some(related::elem(0).unwrap()));
    assert!(
        !replacement_effects
            .iter()
            .any(|effect| matches!(&effect.fx, Fx::App(AppFx::Content(ContentReq::Push(_))))),
        "a replacement page cannot inherit an earlier instance's action"
    );
    step(&mut screen, &ScreenEvent::Unmount, None);
    let (_, after) = step(&mut screen, &press, Some(related::elem(0).unwrap()));
    assert!(!after
        .iter()
        .any(|effect| matches!(&effect.fx, Fx::App(AppFx::Content(ContentReq::Push(_))))));
    clear();
}

#[test]
fn the_episode_text_highlight_fits_the_block_the_flow_already_reserves() {
    let d = detail(ServerId::UNSET, "show");
    for (i, ep) in d.episodes.iter().enumerate() {
        let r = episodes::meta_rect(ep, i, 0.0, 0.0, &crate::ui::fixture::FixtureMeasure);
        assert!(r.y + r.h <= episodes::block_h(&d, &crate::ui::fixture::FixtureMeasure) + theme::space::SM);
    }
}

/// **The Languages column is the PAGE's answer about the PAGE's item, and no surface is involved.**
///
/// Red-first, and SIMULATED rather than historical: the old spelling was
/// `ui::tracks_panel::is_available()`, a function of a module this commit deletes. Narrow
/// `DetailScreen::tracks_available` to `crate::metadata::current().is_some_and(describes)` — which
/// is exactly what that function did — and this fails on the first leg: a Detail page mounted on an
/// item whose own fetch has not landed answers from whatever landed LAST, which after a step
/// through a Person page is another film. The About footer would then draw a MORE affordance and
/// publish a fourth pressable element for a column describing a file this page has never seen.
///
/// The panel is never presented in either leg, which is the other half of the claim: the
/// availability question is settled entirely on the page's own state.
#[test]
fn tracks_availability_is_detail_state_not_surface_state() {
    // A leaf with a file — the item some OTHER page loaded, still standing in the store.
    let elsewhere = Detail {
        sid: ServerId::UNSET,
        rk: "elsewhere".into(),
        part: "/library/parts/751/1745595530/file.mp4".into(),
        ..Default::default()
    };
    let _guard = install(elsewhere);

    // This page is standing on a different item and its own fetch has not landed.
    let screen = DetailScreen::new(EntryId(7), ServerId::UNSET, "here".into(),
        crate::pms::HubsSnapshot::empty_for_test().view());
    assert!(
        !screen.tracks_available(),
        "the page has no item of its own yet, so there is no file it can describe"
    );
    assert!(
        screen.locate(about::LANGUAGES_ELEM).is_none(),
        "…and the About footer publishes no Languages element to press"
    );

    // The page's OWN item lands, and it is a show: its streams are episode 1's, so still no file.
    crate::metadata::set_current_for_test(Some(detail(ServerId::UNSET, "here")));
    assert!(!screen.tracks_available(), "a show has no file of its own");
    assert!(screen.locate(about::LANGUAGES_ELEM).is_none());

    // A leaf with a part, on this page's own key: now the column is pressable.
    crate::metadata::set_current_for_test(Some(Detail {
        sid: ServerId::UNSET,
        rk: "here".into(),
        part: "/library/parts/751/1745595530/file.mp4".into(),
        ..Default::default()
    }));
    assert!(screen.tracks_available());
    assert!(
        matches!(screen.locate(about::LANGUAGES_ELEM), Some(Located::About(1))),
        "the page's own leaf has a file, so the column is the About footer's second element"
    );
    clear();
    apply_metadata(MetadataCmd::Clear);
}

#[test]
fn opening_a_catalog_row_mounts_on_it_without_blocking_on_the_fetch() {
    let _guard = crate::testlock::serial();
    let mut pms_state = crate::pms::PmsState::default();
    let pms_adapter = std::sync::Arc::new(crate::pms::PmsAdapter::default());
    crate::pms::seed_for_test(&mut pms_state, &pms_adapter, 3, crate::pms::HubState::Ready);
    apply_metadata(MetadataCmd::Clear);
    let row = crate::pms::movie(&pms_state, 1).expect("seeded catalog row");
    let (sid, rk) = (row.sid, row.rk.clone());
    let hubs_snap = crate::pms::hubs_snapshot(&pms_state);
    let screen = DetailScreen::new(EntryId(7), sid, rk.clone(), hubs_snap.view());
    assert_eq!((screen.sid, screen.rk.as_str()), (sid, rk.as_str()));
    assert!(
        crate::metadata::current().is_none(),
        "construction must not run the fetch to completion inline"
    );
    assert!(
        crate::metadata::detail_loading(),
        "the asynchronous request is in flight"
    );
    apply_metadata(MetadataCmd::Clear);
}

#[test]
fn a_crew_only_item_still_gets_the_cast_and_crew_shelf() {
    let _guard = crate::testlock::serial();
    let mut d = detail(ServerId::UNSET, "show");
    d.cast.clear();
    d.crew.push(crate::metadata::Cast {
        tag: "Writer".into(),
        role: "Writer".into(),
        thumb: String::new(),
        id: 1,
        tag_key: String::new(),
    });
    let screen = bare(&_guard, ServerId::UNSET, "show");
    let (sections, n) = screen.sections(Some(&d));
    assert!(sections[..n].contains(&4));
}

#[test]
fn hero_focus_is_clamped_when_the_control_set_shrinks_under_it() {
    let sid = ServerId::UNSET;
    let _guard = install(Detail {
        sid,
        rk: "show".into(),
        resume_ms: 0,
        ..Default::default()
    });
    let screen = bare(&_guard, sid, "show");
    let measure = crate::ui::fixture::FixtureMeasure;
    let got = Focusable::<TestHost>::reconcile(
        &screen,
        FocusKey {
            entry: EntryId(7),
            elem: hero::ELEM_RESTART,
        },
        &cx(&measure, None),
    );
    assert_eq!(got.elem, hero::ELEM_PLAY);
    clear();
}

#[test]
fn hero_focus_follows_its_control_when_the_set_grows_under_it() {
    let sid = ServerId::UNSET;
    let _guard = install(Detail {
        sid,
        rk: "show".into(),
        resume_ms: 30,
        dur_ms: 100,
        ..Default::default()
    });
    let screen = bare(&_guard, sid, "show");
    let measure = crate::ui::fixture::FixtureMeasure;
    let want = FocusKey {
        entry: EntryId(7),
        elem: hero::ELEM_MARK_WATCHED,
    };
    assert_eq!(
        Focusable::<TestHost>::reconcile(&screen, want, &cx(&measure, None)),
        want
    );
    clear();
}

#[test]
fn hero_focus_survives_a_control_appearing_in_the_middle_of_the_row() {
    let before = hero::HeroSet {
        restart: true,
        trailer: false,
        alt: false,
        mark: PosterMark::None,
    };
    let after = hero::HeroSet {
        alt: true,
        ..before
    };
    let before_index = hero::watch_index(before).unwrap();
    let after_index = hero::watch_index(after).unwrap();
    assert_eq!(
        after_index,
        before_index + 1,
        "Alt was inserted before the tail"
    );
    assert_eq!(
        hero::ctl_at(before, before_index).unwrap().elem(),
        hero::ctl_at(after, after_index).unwrap().elem(),
        "the engine key follows the watch control rather than its shifted position"
    );
}

#[test]
fn restart_drops_the_resume_both_play_paths_bake_in() {
    for (resume_ms, duration_ms, expected) in [
        (1_800_000, 7_200_000, 1_800_000_000_000),
        (600_000, 2_700_000, 600_000_000_000),
    ] {
        assert_eq!(
            play_resume_ns(false, resume_ms, duration_ms),
            expected,
            "ordinary Play keeps the resume produced by the Plex rule"
        );
        assert_eq!(
            play_resume_ns(true, resume_ms, duration_ms),
            0,
            "Restart drops the resume after either the movie or episode request resolves"
        );
    }
}

#[test]
fn a_watched_toggle_holds_the_filmstrips_place_and_a_stale_latch_never_steers_a_tab_switch() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.cur_season = 1;
    d.episodes.push(episode("e3", 3));
    let _guard = install(d);
    let measure = crate::ui::fixture::FixtureMeasure;
    let want = FocusKey {
        entry: EntryId(7),
        elem: episodes::elem(0, episodes::Row::Still).unwrap(),
    };

    let mut screen = bare(&_guard, sid, "show");
    let spot = Spot {
        section: 2,
        col: 2,
        season: Some(2),
        ..Default::default()
    };
    screen.restore_episode(&spot, Some("e3"));
    let kept = Focusable::<TestHost>::reconcile(&screen, want, &cx(&measure, None));
    assert_eq!(screen.locate(kept.elem).and_then(Located::local_key).and_then(episodes::locate), Some((2, episodes::Row::Still)));

    let mut changed_season = detail(sid, "show");
    changed_season.cur_season = 0;
    crate::metadata::set_current_for_test(Some(changed_season));
    screen.restore_intent = Some(RestoreIntent {
        spot: Spot {
            season: Some(2),
            ..spot.clone()
        },
        episode: Some("e3".into()),
        season_requested: true,
    });
    screen.pump_restore();
    assert!(
        screen.restore_intent.is_none(),
        "a different-season landing retires the latch"
    );

    let mut removed = detail(sid, "show");
    removed.cur_season = 1;
    crate::metadata::set_current_for_test(Some(removed));
    screen.restore_episode(&spot, Some("gone"));
    let fallback = Focusable::<TestHost>::reconcile(&screen, want, &cx(&measure, None));
    assert_eq!(
        screen.locate(fallback.elem).and_then(Located::local_key).and_then(episodes::locate),
        Some((0, episodes::Row::Still))
    );
    assert!(
        bare(&_guard, sid, "replacement").restore_intent.is_none(),
        "a replacement page inherits no latch"
    );
    clear();
}

#[test]
fn a_landed_view_state_refresh_puts_the_browsed_season_back_and_never_steers_another_page() {
    let sid = ServerId::UNSET;
    let mut landed = detail(sid, "show");
    landed.cur_season = 1;
    let _guard = install(landed);
    let mut screen = bare(&_guard, sid, "show");
    screen.restore_episode(
        &Spot {
            section: 2,
            season: Some(2),
            ..Default::default()
        },
        Some("e2"),
    );
    let measure = crate::ui::fixture::FixtureMeasure;
    let got = Focusable::<TestHost>::reconcile(
        &screen,
        FocusKey {
            entry: EntryId(7),
            elem: episodes::elem(0, episodes::Row::Still).unwrap(),
        },
        &cx(&measure, None),
    );
    assert_eq!(screen.locate(got.elem).and_then(Located::local_key).and_then(episodes::locate), Some((1, episodes::Row::Still)));

    crate::metadata::set_current_for_test(Some(Detail {
        sid,
        rk: "other".into(),
        ..Default::default()
    }));
    let before = screen.hash();
    let unchanged = Focusable::<TestHost>::reconcile(
        &screen,
        FocusKey {
            entry: EntryId(7),
            elem: hero::ELEM_PLAY,
        },
        &cx(&measure, None),
    );
    assert_eq!(
        unchanged.elem,
        hero::ELEM_PLAY,
        "another page's item is never steered"
    );
    assert_eq!(
        screen.hash(),
        before,
        "reconcile is pure and cannot consume the latch"
    );

    screen.pending_season = Some(1);
    step(&mut screen, &ScreenEvent::Unmount, None);
    assert!(screen.restore_intent.is_none());
    assert!(screen.pending_season.is_none());
    clear();
}

#[test]
fn a_pointer_lands_on_the_capsule_the_unfurl_drew() {
    let y = 812.0;
    let set = hero::HeroSet {
        restart: true,
        trailer: false,
        alt: false,
        mark: PosterMark::InProgress,
    };
    let index = hero::watch_index(set).unwrap();
    assert_eq!(hero::ctl_at(set, index - 1), Some(hero::HeroCtl::Restart));
    for unfurl in [0.2, 0.6, 1.0] {
        let disc = hero::disc_caps_at(set, 230.0, 0.0, [0.0, 0.0, unfurl], [201.0, 0.0, 267.0]);
        let widths = hero::HeroWidths {
            pill: 230.0,
            alt: 0.0,
            disc,
        };
        let previous = hero::hero_btn_rect_at(set, index - 1, y, widths);
        let capsule = hero::hero_btn_rect_at(set, index, y, widths);
        assert!(capsule.w > hero::CD);
        assert!(capsule.contains(capsule.x + capsule.w - 1.0, capsule.cy()));
        assert!(!previous.contains(capsule.x + 1.0, capsule.cy()));
        assert_eq!(capsule.y, y);
    }
}

#[test]
fn hero_action_row_hit_matches_the_drawn_controls_at_every_set_size() {
    let _guard = crate::testlock::serial();
    use crate::ui::hit::{HitMap, PointerKind};
    crate::plex::reset_servers_for_test();
    let sid = crate::plex::register_for_test("hero-hit-own", "127.0.0.1", 1, "t", "c1");
    let other = crate::plex::register_for_test("hero-hit-other", "127.0.0.1", 2, "t", "c2");
    let measure = crate::ui::fixture::FixtureMeasure;
    let context = cx(&measure, None);
    let mut sizes = std::collections::BTreeSet::new();
    let mut cases = 0;
    for restart in [false, true] {
        for alt in [false, true] {
            for watched in [false, true] {
                for trailer in [false, true] {
                    crate::metadata::set_current_for_test(Some(Detail {
                        sid, rk: "hero-hit".into(), kind: "movie".into(), watched,
                        resume_ms: if restart { 30_000 } else { 0 }, dur_ms: 120_000,
                        extras: trailer
                            .then(|| crate::metadata::Extra {
                                rk: "trailer".into(),
                                part: "/library/parts/trailer".into(),
                                subtype: "trailer".into(),
                                extra_type: 1,
                                ..Default::default()
                            })
                            .into_iter()
                            .collect(),
                        ..Default::default()
                    }));
                    // The *Also available* control's gate is the STORE, addressed by the page's own
                    // pair — seeded here the way a landed cross-source resolve seeds it, never by
                    // opening the panel.
                    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::AltInstall {
                        sid,
                        rk: "hero-hit".into(),
                        copies: if alt {
                            vec![
                                crate::metadata::AltCopy { sid, rk: "hero-hit".into(), ..Default::default() },
                                crate::metadata::AltCopy { sid: other, rk: "hero-copy".into(), ..Default::default() },
                            ]
                        } else {
                            Vec::new()
                        },
                    });
                    let mut screen = bare(&_guard, sid, "hero-hit");
                    let set = screen.hero_set();
                    assert_eq!(
                        (set.restart, set.alt, set.trailer),
                        (restart, alt, false),
                        "the preview path replaced the Trailer disc"
                    );
                    let (controls, n) = hero::hero_ctls(set);
                    assert_eq!(n, 2 + usize::from(restart) + usize::from(alt));
                    sizes.insert(n);
                    for unfurl in [
                        [0.0, 0.0, 0.0],
                        [0.0, 0.0, 1.0],
                        [0.0, 1.0, 0.0],
                        [0.3, 0.0, 0.7],
                        [1.0, 0.0, 0.0],
                        [0.0, 1.0, 1.0],
                    ] {
                        screen.disc_unfurl = unfurl.map(Spring::at);
                        for scroll in [0.0, 48.0] {
                            screen.scroll = Spring::at(scroll);
                            let widths = hero::hero_widths(&measure, set, restart, unfurl, false);
                            let mut stops = Vec::new();
                            let mut drawn = Vec::new();
                            for (i, control) in controls[..n].iter().enumerate() {
                                let key = FocusKey { entry: EntryId(7), elem: control.elem() };
                                let placed = Focusable::<TestHost>::place(&screen, &key.elem, &context, At::Drawn).expect("every drawn control places");
                                // Same primitive geometry used by draw_buttons; its painter scroll
                                // translation must match the screen-space hit placement exactly.
                                let mut painted = hero::hero_btn_rect_at(set, i, screen.hero_chain(&crate::ui::fixture::FixtureMeasure).btn_y, widths);
                                painted.y -= scroll;
                                assert_eq!((placed.rect.x, placed.rect.y, placed.rect.w, placed.rect.h),
                                    (painted.x, painted.y, painted.w, painted.h),
                                    "set={set:?} control={control:?} unfurl={unfurl:?}");
                                assert_eq!(placed.index, Some(i as u32));
                                drawn.push((key, painted));
                                stops.push(Stop { key, rect: placed.rect, rest_rect: placed.rest_rect,
                                    clip: placed.clip, hover: Hover::Focus, activate: Activate::Press });
                            }
                            let mut hit = HitMap::new();
                            hit.fill(stops);
                            hit.swap();
                            for (key, rect) in &drawn {
                                let resolved = hit.resolve(Some(key.entry), PointerKind::Click, rect.cx(), rect.cy(), None);
                                assert_eq!(resolved.hit, Some(*key));
                                assert_eq!(resolved.activate.map(|(key, _)| key), Some(*key));
                            }
                            for pair in drawn.windows(2) {
                                let left = pair[0].1;
                                let right = pair[1].1;
                                assert!(left.x + left.w < right.x, "controls must not overlap");
                                let gutter = (left.x + left.w + right.x) * 0.5;
                                assert!(hit.resolve(Some(pair[0].0.entry), PointerKind::Click, gutter, left.cy(), None).miss,
                                    "the painted gutter must not activate either control");
                            }
                            cases += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(sizes.into_iter().collect::<Vec<_>>(), vec![2, 3, 4]);
    assert_eq!(cases, 192);
    clear();
    crate::plex::reset_servers_for_test();
}

/// **A pointer click must not be able to reach the hero row — Play included — while full-trailer
/// mode owns the screen.** `hero::focusable`/`valid()` keep Play "valid" so the engine has a
/// legitimate keyboard anchor to stand on, but `draw_buttons` fades the whole row (Play too) to
/// alpha 0 there. Before the fix, `record_stops` still registered Play's rect as a Stop, so a
/// magic-remote click on the old pill position resolved and activated it — starting the FEATURE
/// from a screen showing only a trailer. This drives the real `player::preview` singleton (behind
/// `testlock::serial()`, reset before returning) because `full_trailer()` reads it live.
#[test]
fn full_trailer_mode_registers_no_hero_stops_at_all() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    let mut screen = bare(&_guard, sid, "show");
    screen.preview_promoted = true;
    crate::player::preview::force_playing_for_test();
    assert!(screen.full_trailer(), "the fixture must land in full-trailer mode for this test to mean anything");
    let measure = crate::ui::fixture::FixtureMeasure;
    let context = cx(&measure, Some(hero::HeroCtl::Play.elem()));
    let mut draw = DrawFrame::new(&context, crate::ui::Painter::root());
    screen.record_stops(&mut draw);
    let stops = draw.into_stops();
    let set = screen.hero_set();
    let (all, n) = hero::hero_ctls(set);
    for ctl in &all[..n] {
        assert!(
            !stops.iter().any(|stop| stop.key.elem == ctl.elem()),
            "{ctl:?} must not register a pointer stop while full_trailer() is up"
        );
    }
    crate::player::preview::reset_for_test();
    clear();
}

#[test]
fn an_item_with_no_ultrablur_keeps_the_flat_app_ground() {
    let wash = AmbientWash::flat(theme::SURFACE_APP);
    assert!(wash.is_flat(theme::SURFACE_APP, AmbientWash::FLAT_EPS));
}

#[test]
fn the_page_ground_is_the_items_own_corner_arrangement() {
    let blur = [
        [0.1, 0.2, 0.3],
        [0.2, 0.3, 0.4],
        [0.3, 0.4, 0.5],
        [0.4, 0.5, 0.6],
    ];
    let target = AmbientWash::keyed(blur, [AmbientWash::GROUND_W; 4]);
    assert_ne!(target[0], target[1]);
    assert_ne!(target[1], target[2]);
}

#[test]
fn an_episode_page_leads_with_the_episodes_own_still() {
    let sid = ServerId::UNSET;
    let _guard = install(Detail {
        sid,
        rk: "episode".into(),
        kind: "episode".into(),
        thumb: "episode-still".into(),
        art: "show-art".into(),
        ..Default::default()
    });
    let screen = bare(&_guard, sid, "episode");
    assert_eq!(screen.art_identity(screen.detail()).2, "episode-still");
    clear();
}

#[test]
fn a_shows_hero_still_outranks_every_other_art() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.art = "show-art".into();
    d.seasons[0].viewed_leaf_count = 1;
    d.on_deck = Some(crate::metadata::Episode {
        thumb: "next-still".into(),
        resume_ms: 1,
        ..Default::default()
    });
    let _guard = install(d);
    let screen = bare(&_guard, sid, "show");
    assert_eq!(screen.art_identity(screen.detail()).2, "next-still");
    clear();
}

#[test]
fn a_long_synopsis_keeps_the_first_section_one_region_gap_below_the_buttons() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.summary = "A long synopsis whose wrapped lines make the hero taller. ".repeat(20);
    d.crew.push(crate::metadata::Cast {
        tag: "Writer".into(),
        role: "Writer".into(),
        thumb: String::new(),
        id: 1,
        tag_key: String::new(),
    });
    let _guard = install(d);
    let screen = bare(&_guard, sid, "show");
    let detail = screen.detail().unwrap();
    let chain = screen.hero_chain(&crate::ui::fixture::FixtureMeasure);
    assert_eq!(
        screen.section_top(1, detail, &crate::ui::fixture::FixtureMeasure),
        chain.btn_y + hero::CD + theme::space::XL
    );
    assert_eq!(screen.content_top(&crate::ui::fixture::FixtureMeasure), screen.section_top(1, detail, &crate::ui::fixture::FixtureMeasure));
    clear();
}

#[test]
fn only_holdable_media_and_season_cards_request_the_item_menu() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.related = vec![Default::default()];
    d.cast.push(crate::metadata::Cast {
        tag: "Actor".into(),
        role: "Role".into(),
        thumb: String::new(),
        id: 1,
        tag_key: String::new(),
    });
    let _guard = install(d);
    let event = ScreenEvent::PressHold(crate::ui::machine::PressId(9));
    for (elem, expected) in [
        (season::elem(0).unwrap(), true),
        (episodes::elem(0, episodes::Row::Still).unwrap(), true),
        (episodes::elem(0, episodes::Row::Text).unwrap(), false),
        (related::elem(0).unwrap(), true),
        (cast::elem(0).unwrap(), false),
    ] {
        let mut screen = bare(&_guard, sid, "show");
        let (handled, effects) = step(&mut screen, &event, Some(elem));
        assert_eq!(handled == Handled::Yes, expected, "elem={elem}");
        assert_eq!(
            effects
                .iter()
                .any(|effect| matches!(effect.fx, Fx::App(AppFx::Content(ContentReq::ItemMenu)))),
            expected,
            "elem={elem}"
        );
    }
    clear();
}

#[test]
fn episode_text_ok_activates_on_down_without_arming_a_holdable_press() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    let mut screen = bare(&_guard, sid, "show");
    let elem = episodes::elem(1, episodes::Row::Text).unwrap();
    let event = ScreenEvent::Input(InputEvent {
        at: Default::default(),
        source: crate::ui::machine::Source::Script,
        kind: InputKind::Key {
            key: Key::Ok,
            sym: 0,
            wcode: 0,
            edge: Edge::Down,
            at_edge: false,
        },
    });
    let (handled, effects) = step(&mut screen, &event, Some(elem));
    assert_eq!(handled, Handled::Yes);
    assert!(effects.iter().any(|effect| matches!(
        &effect.fx,
        Fx::App(AppFx::Content(ContentReq::Push(ContentArg::Detail { rk, .. }))) if rk == "e2"
    )));
    clear();
}

/// Both BACK and DOWN collapse full-trailer mode through the SAME shared helper
/// (`collapse_full_trailer`), and neither one does anything unusual when it was already off — the
/// regression this guards is the naive first draft, which duplicated the same four-line body
/// under two different key guards instead of sharing one.
#[test]
fn back_and_down_both_collapse_full_trailer_mode_and_are_a_no_op_otherwise() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    fn key_event(key: Key) -> ScreenEvent<TestHost> {
        ScreenEvent::Input(InputEvent {
            at: Default::default(),
            source: crate::ui::machine::Source::Script,
            kind: InputKind::Key { key, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
        })
    }
    for key in [Key::Back, Key::Down] {
        let mut screen = bare(&_guard, sid, "show");
        screen.preview_promoted = true;
        let (handled, _) = step(&mut screen, &key_event(key), None);
        assert_eq!(handled, Handled::Yes, "{key:?} must collapse full-trailer mode");
        assert!(!screen.preview_promoted, "{key:?} left preview_promoted set");
    }
    // DOWN with full-trailer mode already off falls through to ordinary navigation instead of
    // being swallowed — `collapse_full_trailer` returning `false` must not itself count as handled.
    let mut screen = bare(&_guard, sid, "show");
    screen.preview_promoted = false;
    let (handled, _) = step(&mut screen, &key_event(Key::Down), Some(hero::HeroCtl::Play.elem()));
    assert_ne!(
        handled,
        Handled::Yes,
        "DOWN with nothing promoted must not be swallowed by the collapse arm"
    );
    clear();
}

/// Drive one full-trailer key's EFFECT (`trailer_act`) without the live `player::preview`
/// singleton: `full_trailer()` reads that global, so the input arm's guard cannot be reached in a
/// host test, but what the guard leads to is this page's own method and is graded here. The key
/// ladder that chooses the action is pure and graded in `screens::detail::trailer`.
fn trailer_act(
    screen: &mut DetailScreen,
    act: trailer::TrailerKey,
) -> Vec<crate::ui::machine::Stamped<TestHost>> {
    let mut effects = Vec::new();
    let mut present = crate::ui::present::Present::new();
    let mut sink = Effects::new(
        &mut effects,
        crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(1)),
        &mut present,
    );
    screen.trailer_act::<TestHost>(act, &mut sink);
    drop(sink);
    effects
}

fn transport_reqs(
    effects: &[crate::ui::machine::Stamped<TestHost>],
) -> Vec<Option<bool>> {
    effects
        .iter()
        .filter_map(|effect| match &effect.fx {
            Fx::App(AppFx::Content(ContentReq::PreviewTransport(play))) => Some(*play),
            _ => None,
        })
        .collect()
}

/// **Full-trailer mode's transport keys.** OK/PLAYPAUSE ask for the toggle, the remote's dedicated
/// PLAY and PAUSE ask for their own direction, and every one of the three leaves the controls on
/// screen — the request carries no position, because a preview is never seeked.
#[test]
fn ok_toggles_the_trailers_pause_and_play_pause_pick_a_direction() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    for (act, want) in [
        (trailer::TrailerKey::Toggle, None),
        (trailer::TrailerKey::Play, Some(true)),
        (trailer::TrailerKey::Pause, Some(false)),
    ] {
        let mut screen = bare(&_guard, sid, "show");
        screen.preview_promoted = true;
        let effects = trailer_act(&mut screen, act);
        assert_eq!(transport_reqs(&effects), vec![want], "act={act:?}");
        assert!(
            screen.trailer_ctl.revealed(),
            "act={act:?} must leave the controls on screen"
        );
        assert!(screen.preview_promoted, "act={act:?} must not leave the mode");
    }
    clear();
}

/// A direction key the mode keeps only REVEALS: it asks for no transport at all, which is the
/// whole of the "no seek on a preview session" rule at this layer.
#[test]
fn a_revealing_key_asks_for_no_transport() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    let mut screen = bare(&_guard, sid, "show");
    screen.preview_promoted = true;
    let effects = trailer_act(&mut screen, trailer::TrailerKey::Reveal);
    assert!(transport_reqs(&effects).is_empty());
    assert!(screen.trailer_ctl.revealed());
    clear();
}

/// The mode's own collapse key goes through `collapse_full_trailer` — the one collapse body BACK
/// and DOWN already share — and takes the transport down with it. Nothing is resumed here because
/// nothing is paused: `preview::paused()` is false with no live session, which is exactly the
/// state a host test is in.
#[test]
fn the_collapse_key_leaves_the_mode_and_dismisses_the_transport() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    let mut screen = bare(&_guard, sid, "show");
    screen.preview_promoted = true;
    screen.trailer_ctl.reveal();
    let effects = trailer_act(&mut screen, trailer::TrailerKey::Collapse);
    assert!(!screen.preview_promoted, "the mode must be over");
    assert!(!screen.trailer_ctl.revealed(), "the controls go with it");
    assert!(transport_reqs(&effects).is_empty(), "nothing was paused to resume");
    clear();
}

/// BACK's second stage, `collapse_background_preview`: with no live trailer picture up (the
/// default host-test state — driving the live `player::preview` singleton is deliberately avoided
/// here, same as `preview_completed_naturally`'s extraction reasons above), BACK must not be
/// swallowed by the new arm and must still reach ordinary `ContentReq::Back` navigation exactly as
/// before this stage existed.
#[test]
fn back_falls_through_to_navigation_when_no_background_preview_is_up() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    let mut screen = bare(&_guard, sid, "show");
    let event = ScreenEvent::Input(InputEvent {
        at: Default::default(),
        source: crate::ui::machine::Source::Script,
        kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
    });
    let (handled, effects) = step(&mut screen, &event, None);
    assert_eq!(handled, Handled::Yes);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect.fx, Fx::App(AppFx::Content(ContentReq::Back)))),
        "BACK with no background preview up must still leave the page"
    );
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect.fx, Fx::App(AppFx::Content(ContentReq::PreviewStop)))),
        "there is no preview to stop"
    );
    clear();
}

/// `preview_completed_naturally` is the pure core of §8.2's play-once suppression, extracted
/// specifically so it is testable without driving the live, process-wide `player::preview`
/// singleton (which needs `plex::session::peek()` file I/O to even start a Load — exactly the
/// fragility the original plan's §5 rejected a live-Machine integration test over). This table
/// pins the outside-voice-found ordering bug: the check must fire when full-trailer mode was
/// active (`promoted=true`) even though `hero_active` is irrelevant there, AND when the viewer is
/// simply sitting still (`hero_active=true`, `promoted=false`) — and must NOT fire on abandonment
/// (`hero_active=false`, `promoted=false`) or when there was no picture to lose.
#[test]
fn preview_completed_naturally_covers_both_full_trailer_and_sitting_still() {
    // (had_picture, has_picture, promoted, hero_active) -> expected
    let cases = [
        (true, false, true, false, true, "full-trailer-mode completion — the ordering bug's case"),
        (true, false, false, true, true, "sitting still, unpromoted completion"),
        (true, false, false, false, false, "abandonment (scrolled off / hero lost) must not count"),
        (true, true, false, true, false, "still playing — no transition yet"),
        (false, false, false, true, false, "nothing was playing to begin with"),
        (true, false, true, true, true, "full-trailer AND still hero-active — either reason suffices"),
    ];
    for (had, has, promoted, hero_active, expected, why) in cases {
        assert_eq!(
            super::preview_completed_naturally(had, has, promoted, hero_active),
            expected,
            "{why}"
        );
    }
}

#[test]
fn preview_already_played_is_a_plain_key_match() {
    assert!(!super::preview_already_played(None, "rk1"));
    assert!(!super::preview_already_played(Some("rk2"), "rk1"));
    assert!(super::preview_already_played(Some("rk1"), "rk1"));
}

/// Closes the outside-voice-found gap: `preview_played_for`/`preview_started_for` must reset on
/// leave, matching the existing `preview_dwell`/`preview_promoted` convention at the same call
/// site, or §8.2's own "resets whenever you leave and re-enter" decision silently does not hold.
#[test]
fn leaving_the_page_resets_play_once_state_alongside_the_existing_preview_fields() {
    let _guard = install(detail(ServerId::UNSET, "show"));
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    screen.preview_dwell = 1.5;
    screen.preview_promoted = true;
    screen.preview_played_for = Some("some-rk".into());
    screen.preview_started_for = Some("some-rk".into());
    step(&mut screen, &ScreenEvent::Unmount, None);
    assert_eq!(screen.preview_dwell, 0.0);
    assert!(!screen.preview_promoted);
    assert_eq!(screen.preview_played_for, None);
    assert_eq!(screen.preview_started_for, None);
    clear();
}

#[test]
fn a_watch_disc_press_emits_an_addressed_viewstate_effect_without_global_apply() {
    let _guard = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let sid = crate::plex::register_for_test("detail-watch", "127.0.0.1", 1, "t", "c");
    crate::metadata::set_current_for_test(Some(Detail {
        sid,
        rk: "movie".into(),
        kind: "movie".into(),
        watched: false,
        ..Default::default()
    }));
    let mut screen = bare(&_guard, sid, "movie");
    let (_, effects) = step(
        &mut screen,
        &ScreenEvent::PressCommit(crate::ui::machine::PressId(1)),
        Some(hero::ELEM_MARK_WATCHED),
    );
    let addressed: Vec<_> = effects.iter().filter_map(|effect| match &effect.fx {
        Fx::App(AppFx::Store(StoreId::ViewState,
            crate::stores::StoreCmd::ViewState(ViewStateCmd::Request {
                sid, rk, write, detail, guid,
            }))) => Some((*sid, rk.as_str(), *write, detail.as_ref(), guid.as_str())),
        _ => None,
    }).collect();
    assert_eq!(addressed, [(sid, "movie", crate::viewstate::Write::Watched,
        Some(&crate::stores::viewstate::DetailRefresh {
            sid, rk: "movie".into(), keep: None,
        }), "")],
        "Detail must address the typed ViewState command to its owning Bridge");
    assert!(!crate::metadata::current().unwrap().watched,
        "the screen must not call the process-global compatibility facade itself");
    clear();
    crate::plex::reset_servers_for_test();
}

#[test]
fn season_focus_debounces_the_load_without_storing_a_focus_cursor() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    let mut screen = bare(&_guard, sid, "show");
    let to = FocusKey {
        entry: EntryId(7),
        elem: season::elem(1).unwrap(),
    };
    step(
        &mut screen,
        &ScreenEvent::FocusMoved {
            from: Some(FocusKey {
                entry: EntryId(7),
                elem: season::elem(0).unwrap(),
            }),
            to,
            by: By::Dir,
        },
        Some(to.elem),
    );
    assert_eq!(screen.pending_season, Some(1));
    assert_eq!(screen.season_settle, 0.0);
    let first_hash = screen.hash();
    step(
        &mut screen,
        &ScreenEvent::Tick(crate::ui::machine::Tick {
            ms: 100,
            dt_us: 100_000,
        }),
        Some(to.elem),
    );
    assert_eq!(screen.pending_season, Some(1));
    assert!(screen.season_settle > 0.0);
    assert_ne!(
        screen.hash(),
        first_hash,
        "the future load deadline is logical state"
    );
    clear();
}

/// **The frozen-animator regression class, closed for the season-settle countdown (phase 12 D4).**
/// `season_settle` used to be a raw `+= dt` accumulator; it is now driven by `motion::Ramp`, which
/// reports `Motion` from inside its own `advance`. This drives the exact same FocusMoved → Tick
/// sequence as the debounce test above but keeps its own `Present` alive across both steps to
/// check the report directly, rather than only the resulting value.
#[test]
fn a_pending_season_settle_reports_motion_from_inside_advance() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    let mut screen = bare(&_guard, sid, "show");
    let to = FocusKey {
        entry: EntryId(7),
        elem: season::elem(1).unwrap(),
    };
    step(
        &mut screen,
        &ScreenEvent::FocusMoved {
            from: Some(FocusKey {
                entry: EntryId(7),
                elem: season::elem(0).unwrap(),
            }),
            to,
            by: By::Dir,
        },
        Some(to.elem),
    );
    assert_eq!(screen.pending_season, Some(1));
    let measure = crate::ui::fixture::FixtureMeasure;
    let context = cx(&measure, Some(to.elem));
    let mut present = crate::ui::present::Present::new();
    let _ = present.take(0);
    let mut effects = Vec::new();
    for ms in [50, 100, 150] {
        let mut sink = Effects::new(
            &mut effects,
            crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(1)),
            &mut present,
        );
        Machine::<TestHost>::step(
            &mut screen,
            &ScreenEvent::Tick(crate::ui::machine::Tick { ms, dt_us: 50_000 }),
            &context,
            &mut sink,
        );
        assert!(
            present.take(ms),
            "a pending season settle must present every frame it is on screen (ms={ms})"
        );
    }
    clear();
}

/// **The frozen-animator regression class, closed for the page's own loading spinner (phase 12
/// D4).** `spin_ms` used to be a raw `+= dt` accumulator with `fx.note(Motion)` gated on `!loaded`
/// a few lines below it. Now it is `motion::Phase`. A `bare` screen with no metadata installed
/// (unlike every other test in this file, which calls `install` first) has `detail()` answer
/// `None` — `!loaded` — which is exactly the skeleton-spinner state.
#[test]
fn the_loading_spinner_reports_motion_on_every_tick_while_unloaded() {
    let sid = ServerId::UNSET;
    let guard = crate::testlock::serial();
    let mut screen = bare(&guard, sid, "show");
    assert!(screen.detail().is_none(), "no metadata installed for this test");
    let measure = crate::ui::fixture::FixtureMeasure;
    let context = cx(&measure, None);
    let mut present = crate::ui::present::Present::new();
    let _ = present.take(0);
    let mut effects = Vec::new();
    for ms in [16, 32, 48] {
        let mut sink = Effects::new(
            &mut effects,
            crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(1)),
            &mut present,
        );
        Machine::<TestHost>::step(
            &mut screen,
            &ScreenEvent::Tick(crate::ui::machine::Tick { ms, dt_us: 16_667 }),
            &context,
            &mut sink,
        );
        assert!(
            present.take(ms),
            "an unloaded detail page's spinner must present every frame (ms={ms})"
        );
    }
}

fn trailer_extra() -> crate::metadata::Extra {
    crate::metadata::Extra {
        rk: "9".into(),
        part: "/library/parts/trailer".into(),
        vcodec: "h264".into(),
        acodec: "aac".into(),
        title: "Official Trailer".into(),
        subtype: "trailer".into(),
        extra_type: 1,
        dur_ms: 120_000,
        bitrate: 2_500,
        thumb: String::new(),
    }
}

fn play_item(effects: &[crate::ui::machine::Stamped<TestHost>]) -> Option<(&PlayIntent, i64)> {
    effects.iter().find_map(|effect| match &effect.fx {
        Fx::App(AppFx::Content(ContentReq::Play { play, resume_ns })) => Some((play, *resume_ns)),
        _ => None,
    })
}

#[test]
fn a_movie_trailer_disc_plays_the_extra_from_the_start() {
    let extra = trailer_extra();
    let _guard = install(Detail {
        sid: ServerId::UNSET,
        rk: "movie".into(),
        kind: "movie".into(),
        title: "Movie".into(),
        part: "/library/parts/movie".into(),
        extras: vec![extra.clone()],
        ..Default::default()
    });
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::SetNowPlaying(Some(
        crate::metadata::NowPlaying {
            is_episode: true,
            is_real_episode: true,
            title: "Show".into(),
            ep_title: "Pilot".into(),
            season: 1,
            index: 1,
            summary: String::new(),
            year: 2020,
            dur_ms: 1_800_000,
            rating: String::new(),
            thumb: String::new(),
            detail_rk: "show".into(),
        },
    )));
    let mut screen = bare(&_guard, ServerId::UNSET, "movie");
    assert!(!screen.hero_set().trailer, "the preview path replaced the Trailer disc");
    let (_, effects) = step(
        &mut screen,
        &ScreenEvent::Activate(hero::ELEM_TRAILER),
        Some(hero::ELEM_TRAILER),
    );
    let (play, resume_ns) = play_item(&effects).expect("Trailer activate emits Play");
    match play {
        PlayIntent::Item {
            rk,
            part,
            context,
            title,
            ..
        } => {
            assert_eq!(rk, "9");
            assert_eq!(part, "/library/parts/trailer");
            assert_eq!(context, crate::metadata::TRAILER_CONTEXT);
            assert_eq!(title, "Official Trailer");
        }
        _ => panic!("expected PlayIntent::Item for Trailer"),
    }
    assert_eq!(resume_ns, 0);
    assert_eq!(crate::metadata::current().unwrap().rk, "movie");
    assert!(
        crate::metadata::now_playing().is_some_and(|n| n.detail_rk == "show"),
        "activate queues Play; NowPlaying is installed only after request_play accepts"
    );
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::SetNowPlaying(None));
    clear();
}

#[test]
fn a_show_trailer_disc_plays_the_extra_not_the_on_deck_episode() {
    let extra = trailer_extra();
    let mut d = detail(ServerId::UNSET, "show");
    d.extras = vec![extra];
    d.on_deck = Some(crate::metadata::Episode {
        rk: "ep".into(),
        part: "/library/parts/ep".into(),
        vcodec: "hevc".into(),
        acodec: "ac3".into(),
        title: "Episode".into(),
        ..Default::default()
    });
    let _guard = install(d);
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    let (_, trailer_fx) = step(
        &mut screen,
        &ScreenEvent::Activate(hero::ELEM_TRAILER),
        Some(hero::ELEM_TRAILER),
    );
    match play_item(&trailer_fx).unwrap() {
        (
            PlayIntent::Item {
                rk,
                part,
                context,
                ..
            },
            0,
        ) => {
            assert_eq!(rk, "9");
            assert_eq!(part, "/library/parts/trailer");
            assert_eq!(context, crate::metadata::TRAILER_CONTEXT);
        }
        _ => panic!("show Trailer must play the extra"),
    }
    let (_, play_fx) = step(
        &mut screen,
        &ScreenEvent::Activate(hero::ELEM_PLAY),
        Some(hero::ELEM_PLAY),
    );
    match play_item(&play_fx).unwrap() {
        (PlayIntent::Item { rk, part, context, .. }, _) => {
            assert_ne!(rk.as_str(), "9");
            assert_ne!(part.as_str(), "/library/parts/trailer");
            assert_ne!(context.as_str(), crate::metadata::TRAILER_CONTEXT);
        }
        (PlayIntent::Movie(_), _) => {}
    }
    clear();
}

#[test]
fn a_movie_without_extras_does_not_offer_a_trailer_disc() {
    let _guard = install(Detail {
        sid: ServerId::UNSET,
        rk: "movie".into(),
        kind: "movie".into(),
        ..Default::default()
    });
    let screen = bare(&_guard, ServerId::UNSET, "movie");
    assert!(!screen.hero_set().trailer);
    assert!(hero::index_of(screen.hero_set(), hero::HeroCtl::Trailer).is_none());
    clear();
}

#[test]
fn a_trailer_disc_requires_both_rk_and_part() {
    let _guard = crate::testlock::serial();
    let cases = [
        crate::metadata::Extra {
            rk: String::new(),
            part: "/library/parts/trailer".into(),
            ..Default::default()
        },
        crate::metadata::Extra {
            rk: "9".into(),
            part: String::new(),
            ..Default::default()
        },
    ];
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::SetNowPlaying(Some(
        crate::metadata::NowPlaying {
            is_episode: true,
            is_real_episode: true,
            title: "Show".into(),
            ep_title: "Pilot".into(),
            season: 1,
            index: 1,
            summary: String::new(),
            year: 2020,
            dur_ms: 1_800_000,
            rating: String::new(),
            thumb: String::new(),
            detail_rk: "show".into(),
        },
    )));
    for extra in cases {
        crate::metadata::set_current_for_test(Some(Detail {
            sid: ServerId::UNSET,
            rk: "movie".into(),
            kind: "movie".into(),
            extras: vec![extra],
            ..Default::default()
        }));
        let screen = bare(&_guard, ServerId::UNSET, "movie");
        assert!(
            !screen.hero_set().trailer,
            "visibility must match Extra::playable, not a nonempty part alone"
        );
        assert!(hero::index_of(screen.hero_set(), hero::HeroCtl::Trailer).is_none());
        let mut screen = screen;
        let (_, effects) = step(
            &mut screen,
            &ScreenEvent::Activate(hero::ELEM_TRAILER),
            Some(hero::ELEM_TRAILER),
        );
        assert!(
            play_item(&effects).is_none(),
            "an unplayable extra must not emit Play"
        );
        assert!(
            crate::metadata::now_playing().is_some_and(|n| n.detail_rk == "show"),
            "a Trailer no-op must not wipe a leftover episode NowPlaying"
        );
    }
    crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::SetNowPlaying(None));
    clear();
}

#[test]
fn extras_landing_does_not_grow_a_trailer_disc_or_move_play_identity() {
    let sid = ServerId::UNSET;
    let _guard = install(Detail {
        sid,
        rk: "movie".into(),
        kind: "movie".into(),
        ..Default::default()
    });
    let screen = bare(&_guard, sid, "movie");
    let before = screen.hero_set();
    assert!(!before.trailer);
    assert_eq!(hero::index_of(before, hero::HeroCtl::Play), Some(0));
    let play_elem = hero::HeroCtl::Play.elem();
    crate::metadata::set_current_for_test(Some(Detail {
        sid,
        rk: "movie".into(),
        kind: "movie".into(),
        extras: vec![trailer_extra()],
        ..Default::default()
    }));
    let after = screen.hero_set();
    assert!(!after.trailer, "the preview path replaced the Trailer disc");
    assert_eq!(hero::index_of(after, hero::HeroCtl::Play), Some(0));
    assert_eq!(hero::HeroCtl::Play.elem(), play_elem);
    clear();
}

#[test]
fn section_tops_do_not_remeasure_per_credit() {
    use std::cell::Cell;
    use std::ffi::CStr;

    struct CountMeasure {
        widths: Cell<u32>,
    }
    impl crate::ui::machine::Measure for CountMeasure {
        fn width(&self, s: &CStr, sz: i32, bold: bool) -> f32 {
            self.widths.set(self.widths.get() + 1);
            crate::ui::fixture::FixtureMeasure.width(s, sz, bold)
        }
        fn cap_h(&self, sz: i32) -> f32 {
            crate::ui::fixture::FixtureMeasure.cap_h(sz)
        }
        fn line_h(&self, sz: i32) -> f32 {
            crate::ui::fixture::FixtureMeasure.line_h(sz)
        }
    }

    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.summary = "word ".repeat(80);
    d.episodes = (0..16).map(|i| episode(&format!("e{i}"), i as i64)).collect();
    d.cast = (0..77)
        .map(|i| crate::metadata::Cast {
            tag: format!("Actor {i}"),
            role: "Role".into(),
            thumb: String::new(),
            id: i as i64 + 1,
            tag_key: String::new(),
        })
        .collect();
    let _guard = install(d);
    let screen = bare(&_guard, sid, "show");
    let detail = screen.detail().expect("installed");
    let measure = CountMeasure {
        widths: Cell::new(0),
    };
    let first_top = screen.section_top(4, detail, &measure);
    let after_first = measure.widths.get();
    assert!(after_first > 0, "building layout must measure text");
    for _ in 0..77 {
        assert_eq!(screen.section_top(4, detail, &measure), first_top);
    }
    assert_eq!(
        measure.widths.get(),
        after_first,
        "cached section tops must not remeasure per credit"
    );

    let context = cx(&measure, None);
    for i in 0..77 {
        let elem = screen
            .engine_key(cast::elem(i).expect("cast elem"))
            .expect("cast identity");
        let placed = Focusable::<TestHost>::place(&screen, &elem, &context, At::Drawn)
            .expect("cast credit must place");
        assert_eq!(
            placed.rect.y,
            first_top + cast::LABEL_H,
            "credit {i} must share the cached cast strip"
        );
    }
    assert_eq!(
        measure.widths.get(),
        after_first,
        "placing every credit must not remeasure synopsis or episode text"
    );
    clear();
}

#[test]
fn cached_section_tops_match_the_stacking_walk() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.summary = "word ".repeat(40);
    d.episodes = (0..8)
        .map(|i| {
            let mut ep = episode(&format!("e{i}"), i as i64);
            ep.title = "A wrapping episode title with enough words to take two lines".into();
            ep
        })
        .collect();
    d.cast = vec![crate::metadata::Cast {
        tag: "Actor".into(),
        role: "Role".into(),
        thumb: String::new(),
        id: 1,
        tag_key: String::new(),
    }];
    d.related = vec![Default::default()];
    let _guard = install(d);
    let screen = bare(&_guard, sid, "show");
    let detail = screen.detail().expect("installed");
    let measure = crate::ui::fixture::FixtureMeasure;

    let cast_first = screen.section_top(4, detail, &measure);
    let seasons = screen.section_top(1, detail, &measure);
    let episodes = screen.section_top(2, detail, &measure);
    let related = screen.section_top(3, detail, &measure);
    let about = screen.section_top(5, detail, &measure);
    assert_eq!(
        screen.section_top(4, detail, &measure),
        cast_first,
        "asking a later section first must not change earlier tops"
    );
    assert_eq!(seasons, screen.content_top(&measure));
    assert_eq!(episodes, seasons + season::ROW_H + super::TAB_EP_GAP);
    assert_eq!(
        cast_first,
        episodes + screen.block_h(2, detail, &measure) + super::SECTION_GAP
    );
    assert_eq!(
        related,
        cast_first + screen.block_h(4, detail, &measure) + super::SECTION_GAP
    );
    assert_eq!(
        about,
        related + screen.block_h(3, detail, &measure) + super::SECTION_GAP
    );
    clear();
}

#[test]
fn a_replaced_episode_list_moves_the_cast_row() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.cast = vec![crate::metadata::Cast {
        tag: "Actor".into(),
        role: "Role".into(),
        thumb: String::new(),
        id: 1,
        tag_key: String::new(),
    }];
    let _guard = install(d);
    let screen = bare(&_guard, sid, "show");
    let measure = crate::ui::fixture::FixtureMeasure;
    let before = screen.section_top(4, screen.detail().expect("installed"), &measure);

    let mut taller = detail(sid, "show");
    taller.cast = vec![crate::metadata::Cast {
        tag: "Actor".into(),
        role: "Role".into(),
        thumb: String::new(),
        id: 1,
        tag_key: String::new(),
    }];
    taller.episodes = (0..8)
        .map(|i| {
            let mut ep = episode(&format!("tall{i}"), i as i64);
            ep.title = "Wrapping title words enough to grow the filmstrip ".repeat(8);
            ep.summary = "Wrapping episode prose that also grows the strip height. ".repeat(12);
            ep
        })
        .collect();
    crate::metadata::set_current_for_test(Some(taller));
    let after = screen.section_top(4, screen.detail().expect("replaced"), &measure);
    assert!(
        after > before,
        "replacing CURRENT must miss the cached walk, not keep the short-episode cast top ({after} vs {before})"
    );
    clear();
}

#[test]
fn a_movie_without_a_filmstrip_sits_its_first_block_on_content_top() {
    let sid = ServerId::UNSET;
    let _guard = install(Detail {
        sid,
        rk: "movie".into(),
        kind: "movie".into(),
        summary: "word ".repeat(40),
        cast: vec![crate::metadata::Cast {
            tag: "Actor".into(),
            role: "Role".into(),
            thumb: String::new(),
            id: 1,
            tag_key: String::new(),
        }],
        related: vec![Default::default()],
        ..Default::default()
    });
    let screen = bare(&_guard, sid, "movie");
    let detail = screen.detail().expect("installed");
    let measure = crate::ui::fixture::FixtureMeasure;
    let top = screen.content_top(&measure);
    assert_eq!(screen.section_top(4, detail, &measure), top);
    assert_eq!(
        screen.section_top(1, detail, &measure),
        screen.section_top(2, detail, &measure),
        "absent filmstrip sections must share the walk's end"
    );
    assert!(
        screen.section_top(1, detail, &measure) > screen.section_top(5, detail, &measure),
        "an absent section's fallback is past About, not About's own top"
    );
    assert!(
        screen.section_top(3, detail, &measure) > top,
        "related follows cast"
    );
    clear();
}

#[test]
fn ticking_the_page_allows_layout_to_remeasure() {
    use std::cell::Cell;
    use std::ffi::CStr;

    struct CountMeasure {
        widths: Cell<u32>,
    }
    impl crate::ui::machine::Measure for CountMeasure {
        fn width(&self, s: &CStr, sz: i32, bold: bool) -> f32 {
            self.widths.set(self.widths.get() + 1);
            crate::ui::fixture::FixtureMeasure.width(s, sz, bold)
        }
        fn cap_h(&self, sz: i32) -> f32 {
            crate::ui::fixture::FixtureMeasure.cap_h(sz)
        }
        fn line_h(&self, sz: i32) -> f32 {
            crate::ui::fixture::FixtureMeasure.line_h(sz)
        }
    }

    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.summary = "word ".repeat(40);
    let _guard = install(d);
    let mut screen = bare(&_guard, sid, "show");
    let measure = CountMeasure {
        widths: Cell::new(0),
    };
    let detail = screen.detail().expect("installed");
    let _ = screen.section_top(1, detail, &measure);
    let after_first = measure.widths.get();
    assert!(after_first > 0);
    let _ = screen.section_top(1, detail, &measure);
    assert_eq!(measure.widths.get(), after_first, "still cached before tick");
    step(
        &mut screen,
        &ScreenEvent::Tick(crate::ui::machine::Tick {
            ms: 16,
            dt_us: 16_667,
        }),
        None,
    );
    let _ = screen.section_top(1, screen.detail().expect("still installed"), &measure);
    assert!(
        measure.widths.get() > after_first,
        "tick must drop the walk so the next present can remeasure"
    );
    clear();
}

#[test]
fn extras_sit_after_cast_and_crew_and_do_not_move_the_compact_title() {
    let extra = crate::metadata::Extra {
        rk: "9".into(),
        part: "/p".into(),
        title: "Clip".into(),
        subtype: "behindTheScenes".into(),
        extra_type: 5,
        ..Default::default()
    };
    let mut show = detail(ServerId::UNSET, "show");
    show.cast.push(crate::metadata::Cast {
        tag: "Actor".into(),
        role: "Lead".into(),
        thumb: String::new(),
        id: 1,
        tag_key: String::new(),
    });
    show.related.push(Default::default());
    show.extras = vec![extra.clone(), extra];
    let _guard = install(show);
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    let loaded = screen.detail().expect("installed");
    let (sections, n) = screen.sections(Some(loaded));
    assert_eq!(
        &sections[..n],
        &[0, 1, 2, 4, 6, 3, 5],
        "extras sits after Cast and before Related"
    );
    let hide = super::compact_title_hide_pos(&sections, n, true).unwrap();
    assert_eq!(sections[hide], 4, "a show still hides the compact title at Cast");

    let mut spot = crate::metadata::Spot::default();
    spot.section = 6;
    spot.col = 1;
    screen.restore(&spot);
    let key = screen.restore_focus().expect("extras column restores");
    assert!(matches!(screen.locate(key), Some(Located::Extras(1))));

    let movie = Detail {
        sid: ServerId::UNSET,
        rk: "movie".into(),
        kind: "movie".into(),
        extras: vec![
            crate::metadata::Extra {
                rk: "9".into(),
                part: "/p".into(),
                title: "Clip".into(),
                subtype: "behindTheScenes".into(),
                extra_type: 5,
                ..Default::default()
            },
            crate::metadata::Extra {
                rk: "8".into(),
                part: "/q".into(),
                title: "Trailer".into(),
                subtype: "trailer".into(),
                extra_type: 1,
                ..Default::default()
            },
        ],
        cast: vec![crate::metadata::Cast {
            tag: "Actor".into(),
            role: "Lead".into(),
            thumb: String::new(),
            id: 1,
            tag_key: String::new(),
        }],
        related: vec![Default::default()],
        ..Default::default()
    };
    let (sections, n) = screen.sections(Some(&movie));
    assert_eq!(&sections[..n], &[0, 4, 6, 3, 5]);
    let hide = super::compact_title_hide_pos(&sections, n, false).unwrap();
    assert_eq!(sections[hide], 3, "a movie still hides the compact title at Related");
    clear();
}

/// Pins the exact hazard `docs/trailer-ux-plan.md` §8's eng review verified was already retired
/// (the `detail-sections-array-position-traps` prior learning) with a test rather than only a
/// source read: `LayoutCache.top`/`.seen` must be indexed by `SectionId as usize` (identity), not
/// by the section's ARRAY-WALK position. This movie fixture puts `Cast` at array position 1 —
/// deliberately NOT equal to `SectionId::Cast`'s own discriminant (4) — so a regression to
/// position-indexing would write the measured Y into `top[1]`/`seen`'s bit 1 instead of `top[4]`/
/// bit 4, leaving `section_top(SectionId::Cast.raw(), ..)` unable to find it and falling through to
/// the "not found" fallback (`c.end`, the whole-layout height) instead of Cast's real, much smaller
/// top position.
#[test]
fn cast_section_top_is_keyed_by_identity_not_array_position() {
    let movie = Detail {
        sid: ServerId::UNSET,
        rk: "movie-cast-pos".into(),
        kind: "movie".into(),
        cast: vec![crate::metadata::Cast {
            tag: "Actor".into(),
            role: "Lead".into(),
            thumb: String::new(),
            id: 1,
            tag_key: String::new(),
        }],
        related: vec![Default::default()],
        ..Default::default()
    };
    let _guard = install(movie);
    let screen = bare(&_guard, ServerId::UNSET, "movie-cast-pos");
    let loaded = screen.detail().expect("installed");
    let (sections, n) = screen.sections(Some(loaded));
    assert_eq!(
        &sections[..n],
        &[0, 4, 3, 5],
        "Cast sits at array position 1, three away from its own SectionId discriminant (4)"
    );
    let measure = crate::ui::fixture::FixtureMeasure;
    let cast_top = screen.section_top(section::SectionId::Cast.raw(), loaded, &measure);
    let content_top = screen.content_top(&measure);
    let layout_end = screen.ensure_layout(loaded, &measure).end;
    assert!(
        cast_top >= content_top && cast_top < layout_end,
        "Cast's real top ({cast_top}) must lie between content_top ({content_top}) and the whole \
         layout's end ({layout_end}) — a position-indexed regression would instead return \
         layout_end itself (the 'not found' fallback), since slot 4 would never get written"
    );
    clear();
}

/// Explicit source→destination inventory. This is bookkeeping, not behavioral proof; every named
/// destination also carries its own discriminating assertion.
const LEGACY_TEST_MAP: &[(&str, &str)] = &[
    (
        "the_backdrop_dithers_only_while_the_scroll_is_at_rest",
        "ui/detail_layout.rs",
    ),
    (
        "the_two_spellings_of_the_conversion_notice_are_the_same_bytes",
        "screens/detail/hero.rs",
    ),
    (
        "the_latch_waits_for_the_show_then_asks_once_and_retires_on_the_season",
        "screens/detail/season.rs",
    ),
    (
        "a_failed_season_fetch_retires_the_latch_instead_of_asking_again",
        "screens/detail/season.rs",
    ),
    (
        "a_superseding_open_and_a_missing_season_both_retire_it",
        "screens/detail/season.rs",
    ),
    (
        "how_it_plays_resolves_the_full_docs_truth_table",
        "screens/detail/hero.rs",
    ),
    (
        "the_pass_note_judges_the_items_own_server_not_the_browsed_one",
        "screens/detail/hero.rs",
    ),
    (
        "an_item_on_your_own_server_gets_no_source_run_at_all",
        "screens/detail/hero.rs",
    ),
    (
        "the_source_credit_is_the_last_run_on_the_line_after_the_play_mode_fragment",
        "screens/detail/hero.rs",
    ),
    (
        "a_credit_with_nothing_in_front_of_it_opens_the_line_bare",
        "screens/detail/hero.rs",
    ),
    (
        "the_trailing_credit_is_the_first_member_the_row_gives_up",
        "screens/detail/hero.rs",
    ),
    (
        "the_crew_credit_names_the_right_job_for_the_kind_of_item",
        "screens/detail/hero.rs",
    ),
    (
        "the_facts_row_stops_short_of_the_people_column",
        "ui/detail_layout.rs",
    ),
    (
        "the_people_column_grows_upward_off_the_button_row",
        "ui/detail_layout.rs",
    ),
    (
        "a_movie_hides_across_its_second_below_hero_block_not_its_first",
        "ui/detail_layout.rs",
    ),
    (
        "a_movie_with_only_one_below_hero_block_keeps_the_first_block_rule",
        "ui/detail_layout.rs",
    ),
    (
        "no_below_hero_section_means_nothing_to_hide_across",
        "ui/detail_layout.rs",
    ),
    (
        "the_cast_pop_is_clearly_visible_and_never_touches_a_neighbour",
        "screens/detail/cast.rs",
    ),
    (
        "cast_pop_drop_tracks_the_live_scale_and_never_goes_negative",
        "screens/detail/cast.rs",
    ),
    (
        "cast_under_h_covers_the_worst_case_label_drop",
        "screens/detail/cast.rs",
    ),
    (
        "an_item_with_no_ultrablur_keeps_the_flat_app_ground",
        "screens/detail/mod.rs",
    ),
    (
        "the_page_ground_is_the_items_own_corner_arrangement",
        "screens/detail/mod.rs",
    ),
    (
        "the_crew_credit_costs_the_chain_no_vertical_space",
        "ui/detail_layout.rs",
    ),
    (
        "a_shows_hero_is_about_the_servers_on_deck_episode_or_the_series",
        "screens/detail/hero.rs",
    ),
    (
        "the_ratings_band_is_reserved_when_there_are_scores_and_never_otherwise",
        "ui/detail_layout.rs",
    ),
    (
        "a_two_line_blurb_lands_the_chain_on_the_mockups_own_ys",
        "ui/detail_layout.rs",
    ),
    (
        "a_crew_only_item_still_gets_the_cast_and_crew_shelf",
        "screens/detail/mod.rs",
    ),
    (
        "ok_on_a_crew_tile_opens_that_crew_members_page_not_an_actor_at_the_same_index",
        "screens/detail/cast.rs",
    ),
    (
        "restart_disc_tracks_the_resume_the_play_would_actually_apply",
        "screens/detail/hero.rs",
    ),
    (
        "restart_and_the_watch_tail_are_independent",
        "screens/detail/hero.rs",
    ),
    (
        "the_optimistic_flip_settles_a_leaf_at_once_and_a_container_a_round_trip_late",
        "screens/detail/hero.rs",
    ),
    (
        "the_watch_state_resolver_answers_leaf_and_container_by_their_own_rules",
        "screens/detail/hero.rs",
    ),
    (
        "each_watch_disc_writes_its_own_verb",
        "screens/detail/hero.rs",
    ),
    (
        "hero_indices_mean_different_actions_in_the_two_control_sets",
        "screens/detail/hero.rs",
    ),
    (
        "hero_focus_is_clamped_when_the_control_set_shrinks_under_it",
        "screens/detail/mod.rs",
    ),
    (
        "restart_drops_the_resume_both_play_paths_bake_in",
        "screens/detail/mod.rs",
    ),
    (
        "hero_focus_follows_its_control_when_the_set_grows_under_it",
        "screens/detail/mod.rs",
    ),
    (
        "the_actions_row_grows_an_also_available_control_only_for_a_second_source",
        "screens/detail/hero.rs",
    ),
    (
        "ok_on_another_copy_asks_for_that_servers_page_and_leaves_this_one_alone",
        "screens/detail/mod.rs",
    ),
    (
        "ok_on_the_copy_you_are_on_dismisses_and_navigates_nowhere",
        "screens/detail/mod.rs",
    ),
    (
        "hero_focus_survives_a_control_appearing_in_the_middle_of_the_row",
        "screens/detail/mod.rs",
    ),
    (
        "the_hero_row_carries_no_track_information_disc",
        "screens/detail/hero.rs",
    ),
    (
        "the_actions_row_accumulates_around_a_variable_width_control",
        "screens/detail/hero.rs",
    ),
    (
        "an_unfurling_disc_never_crosses_the_people_column",
        "screens/detail/hero.rs",
    ),
    (
        "the_real_verbs_all_fit_the_widest_row",
        "screens/detail/hero.rs",
    ),
    (
        "a_verb_that_does_not_fit_is_dropped_whole",
        "screens/detail/hero.rs",
    ),
    (
        "the_unfurled_verbs_are_the_menus_own",
        "screens/detail/hero.rs",
    ),
    (
        "the_row_offers_exactly_one_watched_toggle",
        "screens/detail/hero.rs",
    ),
    (
        "the_widest_action_row_clears_the_people_column",
        "screens/detail/hero.rs",
    ),
    (
        "strip_x_matches_the_shared_shelf_formula",
        "screens/detail/episodes.rs",
    ),
    (
        "the_related_menus_anchor_is_the_tile_the_shelf_drew",
        "screens/detail/related.rs",
    ),
    (
        "pointer_hit_index_matches_the_drawn_tile_in_every_strip",
        "screens/detail/geometry_tests.rs",
    ),
    (
        "a_pointer_lands_on_the_capsule_the_unfurl_drew",
        "screens/detail/mod.rs",
    ),
    (
        "hero_action_row_hit_matches_the_drawn_controls_at_every_set_size",
        "screens/detail/mod.rs",
    ),
    (
        "a_watched_toggle_holds_the_filmstrips_place_and_a_stale_latch_never_steers_a_tab_switch",
        "screens/detail/mod.rs",
    ),
    (
        "a_landed_view_state_refresh_puts_the_browsed_season_back_and_never_steers_another_page",
        "screens/detail/mod.rs",
    ),
    (
        "an_episode_still_resolves_its_three_states_into_one_mark",
        "screens/detail/episodes.rs",
    ),
    (
        "an_episode_resolves_the_same_three_states_for_the_menu_opened_on_it",
        "screens/detail/episodes.rs",
    ),
    (
        "the_state_line_clears_the_full_bleed_bar_at_every_pop_phase",
        "screens/detail/episodes.rs",
    ),
    (
        "season_tab_pills_cover_their_note_and_never_overlap",
        "screens/detail/season.rs",
    ),
    (
        "an_episode_page_leads_with_the_episodes_own_still",
        "screens/detail/mod.rs",
    ),
    (
        "a_shows_hero_still_outranks_every_other_art",
        "screens/detail/mod.rs",
    ),
    (
        "the_filmstrips_still_and_its_text_pair_up_and_down",
        "screens/detail/geometry_tests.rs",
    ),
    (
        "about_focus_only_ever_lands_on_the_card_and_a_clickable_languages_column",
        "screens/detail/about.rs",
    ),
    (
        "the_filmstrips_text_row_opens_that_episodes_own_page",
        "screens/detail/tests.rs",
    ),
    (
        "the_related_shelf_raises_the_same_open_request",
        "screens/detail/tests.rs",
    ),
    (
        "an_open_request_does_not_outlive_its_page",
        "screens/detail/tests.rs",
    ),
    (
        "a_spot_round_trips_through_the_page_it_describes",
        "screens/detail/tests.rs",
    ),
    (
        "a_restored_spot_clamps_onto_an_item_whose_lists_shrank",
        "screens/detail/tests.rs",
    ),
    (
        "a_movie_spot_does_not_wait_for_a_season_that_will_never_land",
        "screens/detail/tests.rs",
    ),
    (
        "the_episode_text_highlight_fits_the_block_the_flow_already_reserves",
        "screens/detail/tests.rs",
    ),
    (
        "the_pointer_lands_on_the_filmstrip_row_that_is_drawn_at_that_y",
        "screens/detail/geometry_tests.rs",
    ),
    (
        "opening_a_catalog_row_mounts_on_it_without_blocking_on_the_fetch",
        "screens/detail/tests.rs",
    ),
];

#[test]
fn legacy_detail_inventory_has_73_unique_source_names() {
    assert_eq!(LEGACY_TEST_MAP.len(), 73);
    let mut names: Vec<&str> = LEGACY_TEST_MAP.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 73, "every legacy test is mapped exactly once");
    assert!(LEGACY_TEST_MAP
        .iter()
        .all(|(_, destination)| !destination.is_empty()));
}
