//! Migration inventory for the 73 legacy `ui/detail.rs` regressions.
//!
//! Geometry entries marked `ui/detail_layout.rs` are intentionally owned by Luna's shared-helper
//! port because Home and diagnostics consume the same contract. Every other entry remains in this
//! package, either in this module or beside the factored helper it exercises.

use super::*;
use plx_machine::machine::{Chrome, Host, InputEvent, PressRead, ScreenId};
use plx_ui::screen::{At, Focusable, ScreenArg};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct TestArg;
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

pub(super) struct TestHost;

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

thread_local! {
    // TEST ONLY. This file drives one screen per test through free helper fns (`install`,
    // `step`, `apply_metadata`, ...) that pre-date per-owner `MetadataStore`s, so — unlike
    // `stores/person.rs`'s tests, which thread an owned `PersonStore` through every call
    // explicitly — the owner lives here instead, confined to the thread each test body runs on.
    // Every access still goes through `MetadataStore`'s own `run`/`state_mut`/`view` (the sole
    // owner API); this only changes WHERE the owner lives, not a second mechanism for reaching
    // it. `testlock::serial()` (already required before any of these helpers may be called)
    // keeps two tests from ever overlapping even if the runner reuses this thread.
    static TEST_METADATA: std::cell::UnsafeCell<plx_data::stores::metadata::MetadataStore> =
        std::cell::UnsafeCell::new(plx_data::stores::metadata::MetadataStore::default());
}

pub(super) fn test_store() -> &'static mut plx_data::stores::metadata::MetadataStore {
    TEST_METADATA.with(|cell| unsafe { &mut *cell.get() })
}

impl crate::registry::CollectionLike for TestHost {
    fn collection<'a>(_cx: &Cx<'a, Self>) -> plx_data::collection::CollectionView<'a> {
        plx_data::collection::CollectionView::default()
    }
}

impl crate::registry::MetadataLike for TestHost {
    fn metadata<'a>(_cx: &Cx<'a, Self>) -> plx_data::metadata::MetadataView<'a> {
        test_store().view()
    }
}

fn cx<'a>(measure: &'a dyn plx_machine::machine::Measure, elem: Option<u32>) -> Cx<'a, TestHost> {
    Cx {
        views: (),
        tick: Default::default(),
        measure,
        press: PressRead::default(),
        focus: plx_machine::machine::FocusRead {
            current: elem.map(|elem| FocusKey {
                entry: EntryId(7),
                elem,
            }),
        ..Default::default() },
        owner: plx_machine::machine::InputOwner::Entry(EntryId(7)),
    }
}

// Synchronizing the identity registry and querying/hash-writing a screen read shared stores
// and legacy panels. Require the caller's guard; acquiring one here would deadlock install().
/// The stops one REAL `Screen::draw` of `screen` registers, in registration order. A host test has
/// no GL, so the draw runs through a recording painter with its stops switched on
/// (`record_stops_while_recording`); the frame clear is kept off the framebuffer.
pub(super) fn drawn_stops(screen: &mut DetailScreen, context: &Cx<'_, TestHost>) -> Vec<Stop<u32>> {
    let mut f = DrawFrame::new(context, plx_ui::Painter::recording());
    plx_ui::screen::record_stops_while_recording(|| {
        plx_gfx::gfx::without_frame_clear(|| Screen::<TestHost>::draw(screen, &mut f));
    });
    f.into_stops()
}

fn bare(_guard: &plx_base::testlock::Serial, sid: ServerId, rk: &str) -> DetailScreen {
    bare_held(sid, rk)
}

/// [`bare`] for a caller that already holds the serial guard in a way the type cannot show (the
/// card-conformance matrix runs every harness inside `run_all`'s guard, `cards_harness`).
pub(super) fn bare_held(sid: ServerId, rk: &str) -> DetailScreen {
    let mut screen = DetailScreen {
        entry: EntryId(7),
        sid,
        rk: rk.into(),
        keys: vec![], next_elem: FIRST_ITEM_ELEM, key_by_local: Default::default(),
        local_by_key: Default::default(), prev_by_key: Default::default(), gone: Default::default(), return_pending: false,
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
        refresh_gen: 0,
        restore_intent: None,
        teardown_cleared: false,
        withdrawn: false,
        scroll: Spring::at(0.0),
        scroll_target: 0.0,
        free: false,
        episode_free: false,
        episode_scroll: Spring::at(0.0),
        tab_scroll: Spring::at(0.0),
        episode_cells: episodes::Cells::new(),
            ep_want: Default::default(),
            strip_seat: Default::default(),
        about_card_lift: plx_ui::text_lift::TextLift::new(),
        about_lang_lift: plx_ui::text_lift::TextLift::new(),
        related: plx_ui::cards::Shelf::new(EntryId(7), &plx_ui::cards::RowStyle::HOME),
        collection: plx_ui::cards::Shelf::new(EntryId(7), &plx_ui::cards::RowStyle::HOME),
        extras: plx_ui::cards::Shelf::new(EntryId(7), &plx_ui::cards::RowStyle::EPISODE),
        cast: plx_ui::cards::Shelf::new(EntryId(7), &plx_ui::cards::RowStyle::CAST),
        tabs: TabStrip::new(),
        season_pop: CtlPop::new(),
        ctl_pop: CtlPop::new(),
        disc_unfurl: [Spring::at(0.0); hero::DISCS],
        watchlist: plx_appkit::watchlist_disc::Disc::new(plx_data::catalog::Membership::Unknown), watchlist_hex: None,
        season_metrics: season::Metrics::new(),
        about_rows: about::Rows::new(),
        ground: AmbientWash::flat(theme::SURFACE_APP),
        selected: {
            let pms_state = plx_data::pms::PmsState::default();
            plx_data::pms::movie(&pms_state, plx_data::pms::index_of_rk(&pms_state, sid, rk).max(0) as usize)
                .filter(|_| plx_data::pms::index_of_rk(&pms_state, sid, rk) >= 0)
                .cloned()
        },
        spin_ms: 0.0,
        spin_phase: plx_machine::motion::Phase::default(),
        layout: std::cell::Cell::new(None),
        layout_pinned: std::cell::Cell::new(false),
        spot_facts: SpotFacts::default(),
        hold_hint: plx_ui::hold_hint::HoldHint::new(),
    };
    screen.sync_keys(test_store().view());
    screen
}

fn season(index: i64) -> plx_data::metadata::Season {
    plx_data::metadata::Season {
        rk: format!("season-{index}"),
        index,
        title: format!("Season {index}"),
        leaf_count: 2,
        viewed_leaf_count: 0,
    }
}

fn episode(rk: &str, index: i64) -> plx_data::metadata::Episode {
    plx_data::metadata::Episode {
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
        episodes: vec![episode("e1", 1), episode("e2", 2)].into(),
        ..Default::default()
    }
}

fn install(d: Detail) -> plx_base::testlock::Serial {
    let guard = plx_base::testlock::serial();
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(d));
    guard
}

fn clear() {
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
    // The *Also available* store outlives a page, so a test that seeded it hands the next one an
    // empty one — the addressed store cannot MIS-answer, but it can answer for an item a later
    // test happens to reuse the pair of.
    test_store().run(plx_data::stores::metadata::MetadataCmd::AltInstall {
        sid: plx_plex::plex::ServerId::UNSET,
        rk: String::new(),
        copies: Vec::new(),
    });
}

fn step(
    screen: &mut DetailScreen,
    event: &ScreenEvent<TestHost>,
    focus: Option<u32>,
) -> (Handled, Vec<plx_machine::machine::Stamped<TestHost>>) {
    screen.sync_keys(test_store().view());
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
    let measure = plx_ui::fixture::FixtureMeasure;
    let context = cx(&measure, focus);
    let mut effects = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let handled = {
        let mut sink = Effects::new(
            &mut effects,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(1)),
            &mut present,
        );
        Machine::<TestHost>::step(screen, event, &context, &mut sink)
    };
    (handled, effects)
}

/// `DetailScreen::pump_restore` now takes the same `fx: &mut Effects<'_, H>` every other
/// dispatch path does; this test file drives it with a throwaway sink, mirroring `step`'s own
/// scaffold, since none of the call sites below inspect the pushed effects.
fn pump_restore(screen: &mut DetailScreen) {
    let mut effects = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let mut sink = Effects::new(
        &mut effects,
        plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(1)),
        &mut present,
    );
    screen.pump_restore::<TestHost>(test_store().view(), &mut sink);
}

#[test]
fn logical_hash_names_the_mounted_item() {
    let _guard = plx_base::testlock::serial();
    assert_ne!(
        bare(&_guard, ServerId::UNSET, "a").hash(),
        bare(&_guard, ServerId::UNSET, "b").hash()
    );
}

#[test]
fn logical_hash_names_the_full_restore_target() {
    let _guard = plx_base::testlock::serial();
    let mut a = bare(&_guard, ServerId::UNSET, "show");
    let mut b = bare(&_guard, ServerId::UNSET, "show");
    let mut left = Spot {
        section: 2,
        col: 1,
        ep_text: true,
        saved_col: [0, 1, 2, 3, 4, 5, 0, 0],
        season: Some(2),
    };
    let right = left.clone();
    left.col = 0;
    a.restore_episode(&left, Some("e1"), test_store().view());
    b.restore_episode(&right, Some("e2"), test_store().view());
    assert_ne!(a.hash(), b.hash());
    let settled = b.hash();
    b.refresh = DetailRefreshPhase::Deferred;
    assert_ne!(settled, b.hash(), "a future reconciliation request is logical state");
}

#[test]
fn logical_hash_names_the_debounce_deadline() {
    let _guard = plx_base::testlock::serial();
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
    let guard = plx_base::testlock::serial();
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

/// **A server-driven re-read moves nothing.** `DetailReconcile` is the obligation alone: unlike
/// `DetailRestore` (the deliberate re-seat after playback) it neither sets a focus-restore intent
/// nor sends the page back to the top.
#[test]
fn a_reconcile_owes_the_re_read_but_moves_neither_focus_nor_scroll() {
    let sid = ServerId::UNSET;
    let guard = install(detail(sid, "show"));
    for phase in [DetailRefreshPhase::Deferred, DetailRefreshPhase::Requested] {
        let mut screen = bare(&guard, sid, "show");
        screen.sync_keys(test_store().view());
        let focus = episode_key(&screen, 1, episodes::Row::Still).expect("the second episode is keyed");
        let want = FocusKey { entry: EntryId(7), elem: focus };
        let measure = plx_ui::fixture::FixtureMeasure;
        let held = |screen: &DetailScreen| Focusable::<TestHost>::reconcile(screen, want, &cx(&measure, Some(focus))).elem;
        screen.scroll_target = 480.0;
        screen.scroll.pos = 480.0;
        let _ = step(&mut screen, &ScreenEvent::App(AppMsg::DetailReconcile { refresh: phase }), Some(focus));
        assert_eq!(screen.refresh, phase);
        assert!(screen.restore_intent.is_none(), "no focus is re-seated ({phase:?})");
        assert!(!screen.return_pending, "nothing was queued to move focus ({phase:?})");
        assert_eq!(screen.scroll_target, 480.0, "the page does not scroll to the top ({phase:?})");
        assert_eq!(screen.scroll.pos, 480.0);
        assert_eq!(held(&screen), focus, "the obligation leaves the focus where it was ({phase:?})");

        // …and neither does the re-read it asks for, when it lands
        let generation = plx_data::metadata::begin_detail_for_test(test_store().adapter_ref(), sid, "show");
        {
            let (state, adapter) = test_store().split_for_test();
            plx_data::metadata::land_detail_for_test(state, adapter, sid, "show", generation, Some(detail(sid, "show")));
        }
        test_store().pump_detail();
        let _ = step(&mut screen, &ScreenEvent::Tick(plx_machine::machine::Tick { ms: 16, dt_us: 16_667 }), Some(focus));
        assert_eq!(episode_key(&screen, 1, episodes::Row::Still), Some(focus), "the episode keeps its key ({phase:?})");
        assert_eq!(held(&screen), focus, "the landing leaves the focus where it was ({phase:?})");
        assert!(screen.restore_intent.is_none() && !screen.return_pending, "and seats nothing ({phase:?})");
        assert_eq!((screen.scroll_target, screen.scroll.pos), (480.0, 480.0), "the landing does not scroll ({phase:?})");
    }
    clear();
}

#[test]
fn a_reconcile_landing_holds_the_focus_on_an_episode_past_the_first_page() {
    let total = 150;
    let guard = install(long_season(total, &[0, 1]));
    let sid = ServerId::UNSET;
    let mut screen = bare(&guard, sid, "show");
    screen.sync_keys(test_store().view());
    let focus = episode_key(&screen, 70, episodes::Row::Still).expect("page 1 is keyed");
    let measure = plx_ui::fixture::FixtureMeasure;
    let want = FocusKey { entry: EntryId(7), elem: focus };
    let held = |screen: &DetailScreen| Focusable::<TestHost>::reconcile(screen, want, &cx(&measure, Some(focus))).elem;
    assert_eq!(held(&screen), focus);

    let _ = step(&mut screen, &ScreenEvent::App(AppMsg::DetailReconcile { refresh: DetailRefreshPhase::Requested }), Some(focus));
    assert_eq!(held(&screen), focus, "the obligation alone moves nothing");

    // the re-read lands: the server's page 0 of the same season
    let generation = plx_data::metadata::begin_detail_for_test(test_store().adapter_ref(), sid, "show");
    {
        let (state, adapter) = test_store().split_for_test();
        plx_data::metadata::land_detail_for_test(state, adapter, sid, "show", generation, Some(long_season(total, &[0])));
    }
    test_store().pump_detail();
    screen.sync_keys(test_store().view());
    assert_eq!(episode_key(&screen, 70, episodes::Row::Still), Some(focus), "episode 70 keeps the key it had");
    assert_eq!(held(&screen), focus, "the landing left the focus on episode 70, not on the hero");
    clear();
}

#[test]
fn detail_enter_preserves_the_refresh_truth_table_without_focus_restoration() {
    let guard = plx_base::testlock::serial();
    let sid = ServerId::UNSET;
    for cached in [false, true] {
        for phase in [DetailRefreshPhase::None, DetailRefreshPhase::Deferred, DetailRefreshPhase::Requested] {
            for status in [None, Some(true), Some(false)] {
                test_store().run(MetadataCmd::Clear);
                plx_data::metadata::set_current_for_test(test_store().state_mut(), cached.then(|| detail(sid, "show")));
                let pending = status.and_then(|loading| {
                    let generation = plx_data::metadata::begin_detail_for_test(test_store().adapter_ref(), sid, "show");
                    if !loading {
                        let (__s, __a) = test_store().split_for_test();
                        plx_data::metadata::land_detail_for_test(__s, __a, sid, "show", generation, None);
                    }
                    loading.then_some(generation)
                });
                assert_eq!(test_store().view().detail_request_status(sid, "show"), status);
                let mut screen = bare(&guard, sid, "show");
                screen.refresh = phase;
                let generation = plx_data::metadata::detail_generation_for_test(test_store().adapter_ref());
                let (_, entered) = step(&mut screen, &ScreenEvent::Enter(plx_ui::screen::Enter::Restored), None);
                // Both request paths (the direct RequestDetail push and start_reconciliation's
                // own) only ENQUEUE the command; a real Bridge applies it on its next dispatch
                // turn. This file drives no dispatcher, so it must apply it itself before reading
                // the generation the request is expected to have minted.
                apply_metadata_effects(&entered);
                let requests = match phase {
                    DetailRefreshPhase::Deferred => 1,
                    DetailRefreshPhase::Requested => u32::from(status.is_none()),
                    DetailRefreshPhase::None => u32::from(!cached && status != Some(true)),
                };
                assert_eq!(plx_data::metadata::detail_generation_for_test(test_store().adapter_ref()), generation + requests,
                    "cached={cached} phase={phase:?} status={status:?}");
                pump_restore(&mut screen);
                let expected = match (phase, status) {
                    (DetailRefreshPhase::None, _) | (DetailRefreshPhase::Requested, Some(false)) => DetailRefreshPhase::None,
                    _ => DetailRefreshPhase::Requested,
                };
                assert_eq!(screen.refresh, expected,
                    "cached={cached} phase={phase:?} status={status:?} after={:?}",
                    test_store().view().detail_request_status(sid, "show"));
                assert!(screen.restore_intent.is_none());
                // Synthetic workers still owe a terminal acknowledgment after supersession;
                // otherwise this matrix exhausts the production reservation budget itself.
                if let Some(generation) = pending {
                    let (__s, __a) = test_store().split_for_test();
                    plx_data::metadata::land_detail_for_test(__s, __a, sid, "show", generation, None);
                }
                if requests > 0 {
                    let (__s, __a) = test_store().split_for_test();
                    plx_data::metadata::land_detail_for_test(__s, __a, sid, "show", generation + requests, None);
                }
                test_store().pump_detail();
            }
        }
    }
    test_store().run(MetadataCmd::Clear);
    clear();
}

/// **T2 pin.** `start_reconciliation` (`Enter(Restored)`'s promotion of a `Deferred` obligation)
/// pushes `RequestDetail` onto the deferred `AppFx::Store` queue and flips `self.refresh` to
/// `Requested` in the SAME synchronous step — but the store only admits that command on a later
/// drain iteration (`Bridge::app_fx` -> `Fx::Deliver` to the store machine, several queue pops
/// later; see `ui/dispatch.rs::drain`/`absorb`). If a `StoreChanged` or `Tick` reaches this
/// screen's `pump_restore` inside that window, the store still answers with the STALE terminal
/// (`Some(false)`) left by the PREVIOUS reconciliation at this exact `(sid, rk)` address.
///
/// Closed by IDENTITY, not synchrony: `DetailScreen::step` has no same-turn boundary the way
/// `app::content::reconcile_details` does for `DetailReconcile` (see C1fix, `3b628b23`) — `Cx`
/// publishes stores as read-only views, so there is no `&mut Stores` here by construction. Rather
/// than widen `Cx`/`Rig`, `start_reconciliation` now records `self.refresh_gen =
/// meta.detail_generation()` — the generation that already existed BEFORE this promotion's own
/// request is admitted — and `pump_restore`'s `(Requested, Some(false))` arm only retires the
/// obligation once `meta.detail_generation() > self.refresh_gen`, mirroring the generation check
/// completions already use for T3 (`metadata.rs`'s `if gen != adapter.detail_gen...`).
#[test]
fn enter_restored_promotion_survives_a_stale_terminal_before_admission_t2() {
    let guard = install(detail(ServerId::UNSET, "show"));
    // Seed a stale, already-completed reconciliation at this exact address so the store answers
    // `Some(false)` (a completed reconciliation to consume) before the NEW request is admitted.
    let generation = plx_data::metadata::begin_detail_for_test(test_store().adapter_ref(), ServerId::UNSET, "show");
    {
        let (__s, __a) = test_store().split_for_test();
        plx_data::metadata::land_detail_for_test(__s, __a, ServerId::UNSET, "show", generation, None);
    }
    assert_eq!(test_store().view().detail_request_status(ServerId::UNSET, "show"), Some(false));

    let mut screen = bare(&guard, ServerId::UNSET, "show");
    screen.refresh = DetailRefreshPhase::Deferred; // obligation carried while this page was covered

    // Enter(Restored) promotes Deferred -> Requested and pushes RequestDetail, synchronously with
    // the phase flip, but the pushed effect is not yet admitted to the store.
    let (_, entered) = step(&mut screen, &ScreenEvent::Enter(plx_ui::screen::Enter::Restored), None);
    assert_eq!(screen.refresh, DetailRefreshPhase::Requested, "Enter(Restored) must promote the obligation");
    assert!(
        entered.iter().any(|e| matches!(
            &e.fx,
            Fx::App(AppFx::Store(StoreId::Metadata, StoreCmd::Metadata(MetadataCmd::RequestDetail { .. })))
        )),
        "Enter(Restored) must have queued the reconciliation request"
    );

    // Simulate a StoreChanged/Tick landing BEFORE the drain admits that queued command:
    // deliberately do NOT run `apply_metadata_effects` first, unlike every other test in this file.
    pump_restore(&mut screen);

    assert_ne!(
        screen.refresh,
        DetailRefreshPhase::None,
        "T2: a freshly promoted reconciliation obligation must survive a stale terminal result that \
         predates the request which is still only queued, not yet admitted to the owning store"
    );
    test_store().run(MetadataCmd::Clear);
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
            screen.restore_episode(&spot, Some("e2"), test_store().view());
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

/// **T1 pin (contract).** A refresh/reconciliation obligation must survive a CANCELLED
/// focus-restore intent: directional input cancels `restore_intent` (the `ScreenEvent::Input`
/// arm below, `mod.rs`'s `Key::Up|Down|Left|Right` guard), but it must leave `self.refresh` alone
/// — the two are independent obligations, and only the store's own terminal landing may retire
/// the server one. Checked red by mutation: making that same guard also zero `self.refresh`
/// (simulating the T1 defect — an obligation folded into the cancellable intent) turns the first
/// assertion below red (`left: None, right: Requested`); reverted before commit, not left in the
/// tree. This test predates Stage C2 (landed with ViewState's own refactor, `4b390cdd`) but was
/// not labelled as the T1 pin the contract calls for until now.
#[test]
fn cancelled_focus_restoration_still_terminates_reconciliation_on_success_or_failure() {
    let guard = plx_base::testlock::serial();
    let sid = ServerId::UNSET;
    for success in [false, true] {
        test_store().run(MetadataCmd::Clear);
        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(detail(sid, "show")));
        let generation = plx_data::metadata::begin_detail_for_test(test_store().adapter_ref(), sid, "show");
        let mut screen = bare(&guard, sid, "show");
        screen.restore_episode(&Spot::default(), Some("e2"), test_store().view());
        screen.refresh = DetailRefreshPhase::Requested;
        step(&mut screen, &ScreenEvent::Input(plx_machine::machine::InputEvent {
            at: Default::default(), source: plx_machine::machine::Source::Script,
            kind: InputKind::Key { key: Key::Down, edge: Edge::Down, sym: 0, wcode: 0, at_edge: false },
        }), None);
        assert!(screen.restore_intent.is_none());
        assert_eq!(screen.refresh, DetailRefreshPhase::Requested);
        let (__s, __a) = test_store().split_for_test();
        assert_eq!(plx_data::metadata::land_detail_for_test(__s, __a, sid, "show", generation,
            success.then(|| detail(sid, "show"))), success);
        step(&mut screen, &ScreenEvent::StoreChanged(StoreId::Metadata.ord(), generation), None);
        assert_eq!(screen.refresh, DetailRefreshPhase::None);
        assert!(screen.restore_intent.is_none());
        pump_restore(&mut screen);
        assert_eq!(plx_data::metadata::detail_generation_for_test(test_store().adapter_ref()), generation);
    }
    test_store().run(MetadataCmd::Clear);
    clear();
}

#[test]
fn a_spot_round_trips_through_the_page_it_describes() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.guid = "plex://show/5d776b1d96b655001f2a9c3e".into();
    d.cur_season = 1;
    d.related = (0..6)
        .map(|i| plx_data::pms::PmsMovie {
            sid,
            rk: format!("r{i}"),
            ..Default::default()
        })
        .collect();
    let _guard = install(d);
    let mut screen = bare(&_guard, sid, "show");
    let measure = plx_ui::fixture::FixtureMeasure;
    for (elem, section, col, text) in [
        (hero::ELEM_PLAY, 0, 0, false),
        (hero::ELEM_MARK_WATCHED, 0, 2, false),
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
        }), SpotFacts::of(&screen, test_store().view()));
        assert_eq!((spot.section, spot.col, spot.ep_text), (section, col, text));
        screen.restore(&spot, test_store().view());
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
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(Detail {
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
    }), SpotFacts::of(&movie, test_store().view()));
    assert_eq!((languages.section, languages.col), (5, 2));
    clear();
}

#[test]
fn a_restored_spot_clamps_onto_an_item_whose_lists_shrank() {
    let sid = ServerId::UNSET;
    let _guard = plx_base::testlock::serial();
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
    let mut screen = bare(&_guard, sid, "show");
    let measure = plx_ui::fixture::FixtureMeasure;
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
    screen.restore(&spot, test_store().view());
    assert_eq!(
        Focusable::<TestHost>::reconcile(&screen, want, &cx(&measure, None)).elem,
        hero::ELEM_PLAY,
        "a restore with no Detail yet stays on the safe hero fallback"
    );

    let mut d = detail(sid, "show");
    d.related = vec![Default::default(), Default::default()];
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(d));
    screen.restore(&spot, test_store().view());
    assert_eq!(
        Focusable::<TestHost>::reconcile(&screen, want, &cx(&measure, None)).elem,
        screen.engine_key(related::elem(1).unwrap()).unwrap(),
        "a remembered column clamps to the last item in the surviving row"
    );
    let mut no_related = detail(sid, "show");
    no_related.related.clear();
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(no_related));
    screen.restore(&spot, test_store().view());
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
    }, test_store().view());
    let measure = plx_ui::fixture::FixtureMeasure;
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
        &ScreenEvent::PressCommit(plx_machine::machine::PressId(1)),
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
    d.related = vec![plx_data::pms::PmsMovie {
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
        &ScreenEvent::PressCommit(plx_machine::machine::PressId(1)),
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
    d.related = vec![plx_data::pms::PmsMovie {
        sid,
        rk: "related".into(),
        ..Default::default()
    }];
    let _guard = install(d);
    let mut screen = bare(&_guard, sid, "show");
    let press = ScreenEvent::PressCommit(plx_machine::machine::PressId(1));
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
    // Unmount only ENQUEUES its `MetadataCmd::Clear` (a real Bridge applies it on the next
    // dispatch turn); this test drives no dispatcher, so it must apply that effect itself before
    // asking whether the page still answers a press — otherwise `test_store()` still holds the
    // Detail this page unmounted from, and the assertion below would prove nothing.
    let (_, unmount_effects) = step(&mut screen, &ScreenEvent::Unmount, None);
    apply_metadata_effects(&unmount_effects);
    let (_, after) = step(&mut screen, &press, Some(related::elem(0).unwrap()));
    assert!(!after
        .iter()
        .any(|effect| matches!(&effect.fx, Fx::App(AppFx::Content(ContentReq::Push(_))))));
    clear();
}

/// Applies every `AppFx::Store(StoreId::Metadata, ..)` effect in `effects` to `test_store()` —
/// the store-side half of what a real `Bridge` does on its next dispatch turn, for tests that
/// drive a screen with no dispatcher around it (see `step`'s own module doc).
fn apply_metadata_effects(effects: &[plx_machine::machine::Stamped<TestHost>]) {
    for effect in effects {
        if let Fx::App(AppFx::Store(StoreId::Metadata, StoreCmd::Metadata(cmd))) = &effect.fx {
            test_store().run(cmd.clone());
        }
    }
}

#[test]
fn the_episode_text_highlight_fits_the_block_the_flow_already_reserves() {
    let d = detail(ServerId::UNSET, "show");
    for (i, ep) in d.episodes.iter_loaded() {
        let r = episodes::meta_rect(ep, i, 0.0, 0.0, &plx_ui::fixture::FixtureMeasure);
        assert!(r.y + r.h <= episodes::block_h(&d, &plx_ui::fixture::FixtureMeasure) + theme::space::SM);
    }
}

/// **The Languages column is the PAGE's answer about the PAGE's item, and no surface is involved.**
///
/// Red-first, and SIMULATED rather than historical: the old spelling was
/// `ui::tracks_panel::is_available()`, a function of a module this commit deletes. Narrow
/// `DetailScreen::tracks_available` to `test_store().view().current().is_some_and(describes)` — which
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
        plx_data::pms::HubsSnapshot::empty_for_test().view());
    assert!(
        !screen.tracks_available(test_store().view()),
        "the page has no item of its own yet, so there is no file it can describe"
    );
    assert!(
        screen.locate(about::LANGUAGES_ELEM, test_store().view()).is_none(),
        "…and the About footer publishes no Languages element to press"
    );

    // The page's OWN item lands, and it is a show: its streams are episode 1's, so still no file.
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(detail(ServerId::UNSET, "here")));
    assert!(!screen.tracks_available(test_store().view()), "a show has no file of its own");
    assert!(screen.locate(about::LANGUAGES_ELEM, test_store().view()).is_none());

    // A leaf with a part, on this page's own key: now the column is pressable.
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(Detail {
        sid: ServerId::UNSET,
        rk: "here".into(),
        part: "/library/parts/751/1745595530/file.mp4".into(),
        ..Default::default()
    }));
    assert!(screen.tracks_available(test_store().view()));
    assert!(
        matches!(screen.locate(about::LANGUAGES_ELEM, test_store().view()), Some(Located::About(1))),
        "the page's own leaf has a file, so the column is the About footer's second element"
    );
    clear();
    test_store().run(MetadataCmd::Clear);
}

#[test]
fn opening_a_catalog_row_mounts_on_it_without_blocking_on_the_fetch() {
    let _guard = plx_base::testlock::serial();
    let mut pms_state = plx_data::pms::PmsState::default();
    let pms_adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut pms_state, &pms_adapter, 3, plx_data::pms::HubState::Ready);
    test_store().run(MetadataCmd::Clear);
    let row = plx_data::pms::movie(&pms_state, 1).expect("seeded catalog row");
    let (sid, rk) = (row.sid, row.rk.clone());
    let hubs_snap = plx_data::pms::hubs_snapshot(&pms_state);
    let mut screen = DetailScreen::new(EntryId(7), sid, rk.clone(), hubs_snap.view());
    assert_eq!((screen.sid, screen.rk.as_str()), (sid, rk.as_str()));
    assert!(
        test_store().view().current().is_none(),
        "construction must not run the fetch to completion inline"
    );
    // The fetch itself starts on the mounted page's first Enter (T2: the request is admitted in
    // the same step that publishes it), not on construction — a real Bridge delivers Mount then
    // Enter right after the Push this test simulates by driving both directly.
    step(&mut screen, &ScreenEvent::Mount, None);
    let (_, entered) = step(&mut screen, &ScreenEvent::Enter(plx_ui::screen::Enter::Fresh {
        focus: plx_ui::screen::FocusTarget::ContainerGroup(plx_machine::machine::GroupId(0)),
    }), None);
    apply_metadata_effects(&entered);
    assert!(
        plx_data::metadata::detail_loading(test_store().adapter_ref()),
        "the asynchronous request is in flight"
    );
    test_store().run(MetadataCmd::Clear);
}

#[test]
fn a_crew_only_item_still_gets_the_cast_and_crew_shelf() {
    let _guard = plx_base::testlock::serial();
    let mut d = detail(ServerId::UNSET, "show");
    d.cast.clear();
    d.crew.push(plx_data::metadata::Cast {
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

/// A card shelf's focused card vanishing lands on the same place in THAT shelf, not on the hero.
/// `fill` writes the shelf's cards (one per name) into a Detail; `elem` is the shelf's local key.
fn shelf_recovery(fill: fn(&mut Detail, &[&str]), elem: fn(usize) -> Option<u32>) {
    let sid = ServerId::UNSET;
    let names = ["a", "b", "c", "d", "e"];
    let shelf = |names: &[&str]| {
        let mut d = detail(sid, "show");
        fill(&mut d, names);
        d
    };
    let _guard = install(shelf(&names));
    let mut screen = bare(&_guard, sid, "show");
    let measure = plx_ui::fixture::FixtureMeasure;
    let at = |screen: &DetailScreen, i: usize| screen.engine_key(elem(i).unwrap()).unwrap();
    let want = FocusKey { entry: EntryId(7), elem: at(&screen, 3) };
    let publish = |screen: &mut DetailScreen, d: Detail| {
        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(d));
        screen.sync_keys(test_store().view());
    };
    let recovered = |screen: &DetailScreen| Focusable::<TestHost>::reconcile(screen, want, &cx(&measure, None)).elem;

    // the card moved: still there, so still focused
    publish(&mut screen, shelf(&["d", "a"]));
    assert_eq!(recovered(&screen), want.elem, "a card still on the shelf keeps focus when its index moves");
    // the card is gone and the shelf shrank past its index: the last card of the shelf
    publish(&mut screen, shelf(&names));
    publish(&mut screen, shelf(&["a", "b", "c"]));
    assert_eq!(recovered(&screen), at(&screen, 2), "the same index clamped to the shelf's new length");
    // the card is gone but its index survives: whoever now stands there
    publish(&mut screen, shelf(&names));
    publish(&mut screen, shelf(&["a", "b", "c", "x", "e"]));
    assert_eq!(recovered(&screen), at(&screen, 3), "the same index in the shelf");
    // the shelf emptied: the page's fallback
    publish(&mut screen, shelf(&names));
    publish(&mut screen, shelf(&[]));
    assert_eq!(recovered(&screen), hero::ELEM_PLAY, "an emptied shelf falls to the hero");
    clear();
}

#[test]
fn a_vanished_related_card_falls_to_the_same_place_in_the_shelf() {
    shelf_recovery(
        |d, names| d.related = names.iter().map(|rk| plx_data::metadata::Related { rk: (*rk).into(), ..Default::default() }).collect(),
        related::elem,
    );
}

#[test]
fn a_vanished_extra_falls_to_the_same_place_in_the_shelf() {
    shelf_recovery(
        |d, names| d.extras = names.iter().map(|rk| plx_data::metadata::Extra { rk: (*rk).into(), ..Default::default() }).collect(),
        extras::elem,
    );
}

#[test]
fn a_vanished_credit_falls_to_the_same_place_in_the_shelf() {
    shelf_recovery(
        |d, names| {
            d.cast = names
                .iter()
                .map(|key| plx_data::metadata::Cast { tag: (*key).into(), role: String::new(), thumb: String::new(), id: 0, tag_key: (*key).into() })
                .collect()
        },
        cast::elem,
    );
}

/// A focused episode that vanishes from the landed list (a re-read, another season's page) lands on
/// the same place in the strip, not on the hero: the hero fallback scrolls the page to the top
/// (the jump seen on the television after RIGHT on the season strip, then DOWN).
#[test]
fn a_vanished_episode_falls_to_the_same_place_in_the_strip() {
    let sid = ServerId::UNSET;
    let names = ["a", "b", "c", "d", "e"];
    let season = |names: &[&str]| {
        let mut d = detail(sid, "show");
        d.episodes = names.iter().enumerate().map(|(i, rk)| episode(rk, i as i64 + 1)).collect::<Vec<_>>().into();
        d
    };
    let _guard = install(season(&names));
    let mut screen = bare(&_guard, sid, "show");
    let measure = plx_ui::fixture::FixtureMeasure;
    let at = |screen: &DetailScreen, i: usize, row| episode_key(screen, i, row).unwrap();
    for row in [episodes::Row::Still, episodes::Row::Text] {
        let want = FocusKey { entry: EntryId(7), elem: at(&screen, 3, row) };
        let publish = |screen: &mut DetailScreen, d: Detail| {
            plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(d));
            screen.sync_keys(test_store().view());
        };
        let recovered = |screen: &DetailScreen| Focusable::<TestHost>::reconcile(screen, want, &cx(&measure, None)).elem;
        publish(&mut screen, season(&["d", "a"]));
        assert_eq!(recovered(&screen), want.elem, "an episode still in the list keeps focus when its index moves");
        publish(&mut screen, season(&names));
        publish(&mut screen, season(&["a", "b", "c"]));
        assert_eq!(recovered(&screen), at(&screen, 2, row), "the same row of the last episode");
        publish(&mut screen, season(&names));
        publish(&mut screen, season(&["a", "b", "c", "x", "e"]));
        assert_eq!(recovered(&screen), at(&screen, 3, row), "the same index, whoever now stands there");
        publish(&mut screen, season(&names));
        publish(&mut screen, season(&[]));
        assert_eq!(recovered(&screen), hero::ELEM_PLAY, "an emptied season falls to the hero");
        publish(&mut screen, season(&names));
    }
    clear();
}

/// Back from a cast member's page: the covered body is live and still scrolled to the Cast row.
/// `restore_memory` must not send it to the top while the re-read is owed (`reveal_focus` skips a
/// waiting return, so the page would run to 0 and back once the refetch lands).
#[test]
fn a_restore_memory_keeps_the_scroll_of_a_live_body_while_the_re_read_is_owed() {
    let sid = ServerId::UNSET;
    let guard = install(detail(sid, "show"));
    let mut screen = bare(&guard, sid, "show");
    screen.sync_keys(test_store().view());
    screen.scroll_target = 839.0;
    screen.scroll.pos = 839.0;
    screen.refresh = DetailRefreshPhase::Deferred;
    let memory = crate::registry::DetailMemory {
        spot: plx_data::metadata::Spot { section: 4, col: 0, ..Default::default() },
        keys: screen.keys.clone(),
        next_elem: screen.next_elem,
    };
    screen.restore_memory(&memory, test_store().view());
    assert!(screen.return_pending && screen.restore_intent.is_some(), "the return still waits for its card");
    assert_eq!(screen.scroll_target, 839.0, "the live body stays where it was");
    clear();
}

/// The same return, but the store holds one detail at a time and closing a film opened from the
/// cast member's list emptied it: the body comes back with no detail, keeps its scroll while the
/// re-read is owed, and the re-read then FAILS. Nothing moves the scroll after that (`reveal_focus`
/// returns early without a detail) and the hero, the only thing drawn without one, is faded out past
/// `HERO_FADE`, so the page must go back to the top where the hero is.
#[test]
fn a_return_whose_re_read_fails_puts_the_page_back_at_the_top() {
    let guard = plx_base::testlock::serial();
    let sid = ServerId::UNSET;
    test_store().run(MetadataCmd::Clear);
    plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
    let mut screen = bare(&guard, sid, "show");
    screen.sync_keys(test_store().view());
    screen.scroll_target = 839.0;
    screen.scroll.pos = 839.0;
    screen.refresh = DetailRefreshPhase::Requested;
    let memory = crate::registry::DetailMemory {
        spot: plx_data::metadata::Spot { section: 4, col: 0, ..Default::default() },
        keys: screen.keys.clone(),
        next_elem: screen.next_elem,
    };
    screen.restore_memory(&memory, test_store().view());
    assert_eq!(screen.scroll_target, 839.0, "the return waits for its re-read where the body was left");
    let generation = plx_data::metadata::begin_detail_for_test(test_store().adapter_ref(), sid, "show");
    {
        let (state, adapter) = test_store().split_for_test();
        assert!(!plx_data::metadata::land_detail_for_test(state, adapter, sid, "show", generation, None));
    }
    assert!(screen.detail(test_store().view()).is_none());
    pump_restore(&mut screen);
    assert!(screen.restore_intent.is_none() && !screen.return_pending, "the return gave up");
    assert_eq!(screen.scroll_target, 0.0, "a page with no detail shows only the hero, so it goes back to it");
    test_store().run(MetadataCmd::Clear);
    clear();
}

/// Every section's element block is its own, wide, and below the engine keys: no two overlap, and
/// no section's list can run into its neighbour's ids. A block is 2^24 ids; episodes take two ids
/// each out of their 2^30 block.
#[test]
fn the_element_blocks_are_wide_disjoint_and_below_the_engine_keys() {
    const BLOCK: u32 = 1 << 24;
    let blocks: [(&str, u32, u32); 8] = [
        ("hero", 0, hero::HERO_ELEM_RANGE_END),
        ("season", season::SEASON_ELEM_RANGE_START, season::SEASON_ELEM_RANGE_END),
        ("episodes", episodes::EPISODES_ELEM_RANGE_START, episodes::EPISODES_ELEM_RANGE_END),
        ("related", related::RELATED_ELEM_RANGE_START, related::RELATED_ELEM_RANGE_END),
        ("cast", cast::CAST_ELEM_RANGE_START, cast::CAST_ELEM_RANGE_END),
        ("about", about::ABOUT_ELEM_RANGE_START, about::ABOUT_ELEM_RANGE_END),
        ("extras", extras::EXTRAS_ELEM_RANGE_START, extras::EXTRAS_ELEM_RANGE_END),
        ("collection", collection::COLLECTION_ELEM_RANGE_START, collection::COLLECTION_ELEM_RANGE_END),
    ];
    for (name, start, end) in blocks {
        assert!(start < end && end <= FIRST_ITEM_ELEM, "{name} sits below the engine keys");
        if name == "episodes" {
            assert_eq!(end - start, 1 << 30, "episodes own their 2^30 block");
        } else {
            assert_eq!(end - start, BLOCK, "{name} owns a 2^24 block");
        }
    }
    for (i, a) in blocks.iter().enumerate() {
        for b in &blocks[i + 1..] {
            assert!(a.2 <= b.1 || b.2 <= a.1, "the {} and {} blocks overlap", a.0, b.0);
        }
    }
    assert_eq!(FIRST_ITEM_ELEM, 1 << 31, "engine keys start above every local block");
}

/// The key a card answers to is its engine key, not its local id. A caller outside the crate (the
/// app's menu-opener route) asks about focus keys it did not mint, so it must look in the interned
/// range; a card's key sitting in the low range again, or a local id leaking out as a focus key,
/// is what this pins.
#[test]
fn card_focus_keys_live_in_the_interned_range_and_locals_are_not_focus_keys() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.related = vec![Default::default(), Default::default()];
    let _guard = install(d);
    let screen = bare(&_guard, sid, "show");
    let meta = test_store().view();
    for i in 0..2 {
        let local = related::elem(i).unwrap();
        let key = screen.engine_key(local).expect("a related card key");
        assert!(key >= FIRST_ITEM_ELEM, "card {i} is keyed from the interned range");
        assert!(screen.locate(key, meta) == Some(Located::Related(i)));
        assert!(screen.locate(local, meta).is_none(), "a local id is not a focus key");
    }
    clear();
}

/// A list that arrives whole is addressable whole. A 600-credit item has a card for every credit,
/// and Right walks the shelf to its last one; the old block clamped the shelf at 512.
#[test]
fn a_six_hundred_credit_item_addresses_every_cast_card_and_right_reaches_the_last() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.cast = (0..600)
        .map(|i| plx_data::metadata::Cast {
            tag: format!("person {i}"),
            role: String::new(),
            thumb: String::new(),
            id: i,
            tag_key: format!("p{i}"),
        })
        .collect();
    let _guard = install(d);
    let mut screen = bare(&_guard, sid, "show");
    screen.sync_keys(test_store().view());
    let measure = plx_ui::fixture::FixtureMeasure;
    let context = cx(&measure, None);
    let key_at = |screen: &DetailScreen, i: usize| {
        screen.engine_key(cast::elem(i).expect("a local id per credit")).expect("a key per credit")
    };
    let mut at = key_at(&screen, 0);
    for i in 1..600 {
        let Step::Move(next) = Focusable::<TestHost>::neighbour(
            &screen,
            FocusKey { entry: EntryId(7), elem: at },
            Dir::Right,
            &context,
        ) else {
            panic!("Right stopped at credit {}", i - 1);
        };
        assert_eq!(next.elem, key_at(&screen, i), "Right reaches credit {i} in order");
        at = next.elem;
    }
    clear();
}

/// The 600th credit is drawn when focus is on it, and the shelf registers stops only for the cards
/// in reach, so the draw work follows the screen and not the credit count.
#[test]
fn a_six_hundred_credit_shelf_draws_its_last_card_with_stops_for_the_visible_range_only() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.cast = (0..600)
        .map(|i| plx_data::metadata::Cast {
            tag: format!("person {i}"), role: String::new(), thumb: String::new(), id: i, tag_key: format!("p{i}"),
        })
        .collect();
    let _guard = install(d);
    let mut screen = bare(&_guard, sid, "show");
    screen.sync_keys(test_store().view());
    let measure = plx_ui::fixture::FixtureMeasure;
    let last = screen.engine_key(cast::elem(599).unwrap()).unwrap();
    let top = {
        let d = screen.detail(test_store().view()).unwrap();
        screen.section_top(4, d, &measure)
    };
    screen.scroll.jump(top);
    screen.scroll_target = top;
    let first = screen.engine_key(cast::elem(0).unwrap()).unwrap();
    let to = FocusKey { entry: EntryId(7), elem: last };
    step(&mut screen, &ScreenEvent::FocusMoved {
        from: Some(FocusKey { entry: EntryId(7), elem: first }), to, by: By::Dir,
    }, Some(last));
    let context = cx(&measure, Some(last));
    for n in 1..=240u32 {
        step(&mut screen, &ScreenEvent::Tick(plx_machine::machine::Tick { ms: 17 * n, dt_us: 16_667 }), Some(last));
        drawn_stops(&mut screen, &context);
    }
    let stops = drawn_stops(&mut screen, &context);
    let cast_stops: Vec<_> = stops.iter().filter(|stop| screen.locate(stop.key.elem, test_store().view()).is_some_and(|l| matches!(l, Located::Cast(_)))).collect();
    assert!(cast_stops.iter().any(|stop| stop.key.elem == last), "credit 600 is drawn");
    assert!(cast_stops.len() < 60, "stops follow the visible range, got {}", cast_stops.len());
    clear();
}

/// A 200-season show: Right walks the tab strip to its last tab, and the strip registers stops for
/// the tabs on the axis only, so the stop count does not grow with the season count.
#[test]
fn a_two_hundred_season_show_focuses_its_last_tab_and_stops_only_on_the_axis() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.seasons = (1..=200).map(season).collect();
    let _guard = install(d);
    let mut screen = bare(&_guard, sid, "show");
    screen.sync_keys(test_store().view());
    let measure = plx_ui::fixture::FixtureMeasure;
    let context = cx(&measure, None);
    let key_at = |screen: &DetailScreen, i: usize| {
        screen.engine_key(season::elem(i).expect("a local id per tab")).expect("a key per tab")
    };
    let last = key_at(&screen, 199);
    let mut at = key_at(&screen, 0);
    for i in 1..200 {
        let Step::Move(next) = Focusable::<TestHost>::neighbour(
            &screen,
            FocusKey { entry: EntryId(7), elem: at },
            Dir::Right,
            &context,
        ) else {
            panic!("Right stopped at tab {}", i - 1);
        };
        at = next.elem;
    }
    assert_eq!(at, last, "Right reaches the 200th tab");
    let season_keys: std::collections::HashSet<u32> = (0..200).map(|i| key_at(&screen, i)).collect();
    // the tab strip is drawn (and so registers stops) only while its section is on screen; its
    // pills are measured on the Tick and the landing, which a live frame has already run
    let top = {
        let d = screen.detail(test_store().view()).unwrap();
        screen.season_metrics.update(d, &measure);
        screen.section_top(1, d, &measure)
    };
    screen.scroll.jump(top);
    screen.scroll_target = top;
    let stops = drawn_stops(&mut screen, &cx(&measure, Some(last)));
    let on_strip = stops.iter().filter(|stop| season_keys.contains(&stop.key.elem)).count();
    assert!(on_strip > 0 && on_strip < 40, "only the on-axis tabs register stops, not all 200 (got {on_strip})");
    clear();
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
    let measure = plx_ui::fixture::FixtureMeasure;
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
    let measure = plx_ui::fixture::FixtureMeasure;
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
        watchlist: true,
        version: false,
        restart: true,
        trailer: false,
        alt: false,
        mark: PosterMark::None,
    };
    let after = hero::HeroSet {
        watchlist: true,
        version: false,
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
    d.episodes = vec![episode("e1", 1), episode("e2", 2), episode("e3", 3)].into();
    let _guard = install(d);
    let measure = plx_ui::fixture::FixtureMeasure;
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
    screen.restore_episode(&spot, Some("e3"), test_store().view());
    let kept = Focusable::<TestHost>::reconcile(&screen, want, &cx(&measure, None));
    assert_eq!(screen.locate(kept.elem, test_store().view()).and_then(Located::local_key).and_then(episodes::locate), Some((2, episodes::Row::Still)));

    let mut changed_season = detail(sid, "show");
    changed_season.cur_season = 0;
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(changed_season));
    screen.restore_intent = Some(RestoreIntent {
        spot: Spot {
            season: Some(2),
            ..spot.clone()
        },
        episode: Some("e3".into()),
        season_requested: true,
        seek: Default::default(),
    });
    pump_restore(&mut screen);
    assert!(
        screen.restore_intent.is_none(),
        "a different-season landing retires the latch"
    );

    let mut removed = detail(sid, "show");
    removed.cur_season = 1;
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(removed));
    screen.restore_episode(&spot, Some("gone"), test_store().view());
    let fallback = Focusable::<TestHost>::reconcile(&screen, want, &cx(&measure, None));
    assert_eq!(
        screen.locate(fallback.elem, test_store().view()).and_then(Located::local_key).and_then(episodes::locate),
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
        test_store().view(),
    );
    let measure = plx_ui::fixture::FixtureMeasure;
    let got = Focusable::<TestHost>::reconcile(
        &screen,
        FocusKey {
            entry: EntryId(7),
            elem: episodes::elem(0, episodes::Row::Still).unwrap(),
        },
        &cx(&measure, None),
    );
    assert_eq!(screen.locate(got.elem, test_store().view()).and_then(Located::local_key).and_then(episodes::locate), Some((1, episodes::Row::Still)));

    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(Detail {
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
        watchlist: true,
        version: false,
        restart: true,
        trailer: false,
        alt: false,
        mark: PosterMark::InProgress,
    };
    let index = hero::watch_index(set).unwrap();
    assert_eq!(hero::ctl_at(set, index - 1), Some(hero::HeroCtl::Restart));
    for unfurl in [0.2, 0.6, 1.0] {
        let disc = hero::disc_caps_at(set, 230.0, 0.0, [0.0, 0.0, unfurl, 0.0], [201.0, 0.0, 267.0, 0.0]);
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
    let _guard = plx_base::testlock::serial();
    use plx_ui::hit::{HitMap, PointerKind};
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("hero-hit-own", "127.0.0.1", 1, "t", "c1");
    let other = plx_plex::plex::register_for_test("hero-hit-other", "127.0.0.1", 2, "t", "c2");
    let measure = plx_ui::fixture::FixtureMeasure;
    let context = cx(&measure, None);
    let mut sizes = std::collections::BTreeSet::new();
    let mut cases = 0;
    for restart in [false, true] {
        for alt in [false, true] {
            for watched in [false, true] {
                for trailer in [false, true] {
                    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(Detail {
                        sid, rk: "hero-hit".into(), kind: "movie".into(), watched,
                        guid: "plex://movie/5d776b1d96b655001f2a9c3e".into(),
                        resume_ms: if restart { 30_000 } else { 0 }, dur_ms: 120_000,
                        extras: trailer
                            .then(|| plx_data::metadata::Extra {
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
                    test_store().run(plx_data::stores::metadata::MetadataCmd::AltInstall {
                        sid,
                        rk: "hero-hit".into(),
                        copies: if alt {
                            vec![
                                plx_data::metadata::AltCopy { sid, rk: "hero-hit".into(), ..Default::default() },
                                plx_data::metadata::AltCopy { sid: other, rk: "hero-copy".into(), ..Default::default() },
                            ]
                        } else {
                            Vec::new()
                        },
                    });
                    let mut screen = bare(&_guard, sid, "hero-hit");
                    let set = screen.hero_set(test_store().view());
                    assert_eq!(
                        (set.restart, set.alt, set.trailer),
                        (restart, alt, false),
                        "the preview path replaced the Trailer disc"
                    );
                    let (controls, n) = hero::hero_ctls(set);
                    assert_eq!(n, 3 + usize::from(restart) + usize::from(alt));
                    sizes.insert(n);
                    for unfurl in [
                        [0.0, 0.0, 0.0, 0.0],
                        [0.0, 0.0, 1.0, 0.0],
                        [0.0, 1.0, 0.0, 0.0],
                        [0.3, 0.0, 0.7, 0.0],
                        [1.0, 0.0, 0.0, 0.0],
                        [0.0, 1.0, 1.0, 0.0],
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
                                let mut painted = hero::hero_btn_rect_at(set, i, screen.hero_chain(&plx_ui::fixture::FixtureMeasure, test_store().view()).btn_y, widths);
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
    assert_eq!(sizes.into_iter().collect::<Vec<_>>(), vec![3, 4, 5]);
    assert_eq!(cases, 192);
    clear();
    plx_plex::plex::reset_servers_for_test();
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
    plx_media::player::preview::force_playing_for_test();
    assert!(screen.full_trailer(), "the fixture must land in full-trailer mode for this test to mean anything");
    let measure = plx_ui::fixture::FixtureMeasure;
    let context = cx(&measure, Some(hero::HeroCtl::Play.elem()));
    let mut draw = DrawFrame::new(&context, plx_ui::Painter::root());
    screen.record_stops(&mut draw);
    let stops = draw.into_stops();
    let set = screen.hero_set(test_store().view());
    let (all, n) = hero::hero_ctls(set);
    for ctl in &all[..n] {
        assert!(
            !stops.iter().any(|stop| stop.key.elem == ctl.elem()),
            "{ctl:?} must not register a pointer stop while full_trailer() is up"
        );
    }
    plx_media::player::preview::reset_for_test();
    clear();
}

/// **The mechanism, not a special case.** `preview_chrome` is the one scalar `draw_hero` already
/// fades the hero's own chrome through, and `draw`'s below-hero loop (season/episodes, Extras,
/// Related, Cast & Crew, About) now hands that SAME value to every section as `below_hero`'s
/// alpha, rather than a second predicate one of them could drift from. Proving `preview_chrome`
/// itself eases to 0 while `full_trailer()` holds and back to 1 on collapse is proving the
/// sections hide and reappear too — the page draws no rendering harness can drive here, but this
/// is the one number every one of them multiplies through.
#[test]
fn preview_chrome_drives_the_below_hero_sections_to_zero_in_full_trailer_mode_and_back() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    let mut screen = bare(&_guard, sid, "show");
    plx_media::player::preview::force_playing_for_test();
    screen.preview_promoted = true;
    assert!(screen.full_trailer(), "the fixture must land in full-trailer mode for this test to mean anything");

    let mut effects = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let mut sink = Effects::new(
        &mut effects,
        plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(1)),
        &mut present,
    );
    let hero_focus = Some(Located::Hero(hero::HeroCtl::Play));
    let mut now = 0u32;
    for _ in 0..300 {
        now += 16;
        screen.preview_tick::<TestHost>(now, 0.016, hero_focus, &mut sink, test_store().view());
    }
    assert!(
        screen.preview_chrome < 0.01,
        "preview_chrome={} should have eased to 0 in full-trailer mode — the value every \
         below-hero section now fades through",
        screen.preview_chrome
    );

    // Collapse: full-trailer mode ends, and every section's alpha must climb back to full.
    screen.preview_promoted = false;
    for _ in 0..300 {
        now += 16;
        screen.preview_tick::<TestHost>(now, 0.016, hero_focus, &mut sink, test_store().view());
    }
    assert!(
        screen.preview_chrome > 0.99,
        "preview_chrome={} should have eased back to full once full-trailer mode collapsed",
        screen.preview_chrome
    );

    drop(sink);
    plx_media::player::preview::reset_for_test();
    clear();
}

/// **Background autoplay must not fade the rows between the synopsis and Play.** `draw_hero`
/// used to gate the identity/meta line, the ratings row, the facts row and the people column on
/// `chrome * preview_prose` — and `preview_prose` tracks `player::preview::View::prose`, which
/// drops to 0 the instant ANY picture is up, background autoplay included. `preview_chrome` is
/// the value those rows are drawn through now (same as the buttons and the below-hero sections),
/// and it only leaves 1.0 in FULL-trailer mode (`preview_promoted && picture`), not for a picture
/// merely dwelling in the background. This pins that split: `preview_chrome` stays full while a
/// background trailer plays even though the OLD gating value (`preview_prose`) has already
/// dropped to zero underneath it, and only sinking into full-trailer mode (promoted) still takes
/// it to zero.
#[test]
fn background_autoplay_recedes_identity_and_ratings_but_holds_the_facts_row_and_people() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    let mut screen = bare(&_guard, sid, "show");
    plx_media::player::preview::force_playing_for_test();
    assert!(
        !screen.full_trailer(),
        "not promoted yet — this must be the background-autoplay case, not full-trailer"
    );

    let mut effects = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let mut sink = Effects::new(
        &mut effects,
        plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(1)),
        &mut present,
    );
    let hero_focus = Some(Located::Hero(hero::HeroCtl::Play));
    let mut now = 0u32;
    for _ in 0..300 {
        now += 16;
        screen.preview_tick::<TestHost>(now, 0.016, hero_focus, &mut sink, test_store().view());
    }
    assert!(
        screen.preview_prose < 0.01,
        "preview_prose={} must drop once a picture is up: draw_hero gates the identity/meta line \
         and the rating marks on it, and the owner wants those two out of the way while the \
         trailer answers the same question they do",
        screen.preview_prose
    );
    assert!(
        screen.preview_chrome > 0.99,
        "preview_chrome={} must stay full during background autoplay: draw_hero gates the facts \
         row (date · runtime · Direct Play) and the people column on it, and those must not \
         vanish and come back under a playing preview",
        screen.preview_chrome
    );

    // Promote to full-trailer mode: NOW everything hides, `preview_chrome` included.
    screen.preview_promoted = true;
    assert!(screen.full_trailer());
    for _ in 0..300 {
        now += 16;
        screen.preview_tick::<TestHost>(now, 0.016, hero_focus, &mut sink, test_store().view());
    }
    assert!(
        screen.preview_chrome < 0.01,
        "preview_chrome={} should still ease to 0 once full-trailer mode takes over — that part \
         is unchanged",
        screen.preview_chrome
    );

    drop(sink);
    plx_media::player::preview::reset_for_test();
    clear();
}

/// **Regression, 2026-09-18: `preview_tick`'s per-frame path must not read the session file every
/// frame.** `preview::blocked` (called unconditionally from `preview_tick`, hero-focused or not)
/// calls `preview::enabled()`, which called `session::peek()` (since P1c it reads the seated profile's
/// trailer cell instead, so this test now guards the REST of the per-frame path: any other `peek()`
/// reached from `preview_tick`) — and `peek` used to take
/// `session::IO` on every call, which on the television guards a `recv(2)` round trip to the
/// storage helper, measured at ~27 ms/frame: the whole gap between 60 fps and the 26 fps the
/// detail page actually drew. The fix is `session::peek()`'s own live read cache (`session::CACHE`,
/// next to `IO`): a durable write installs its outcome, so every later `peek()` is an uncontended
/// `Mutex` lock and an `Arc` clone, never a re-read. This drives 30 real frames with the hero
/// focused (dwelling toward a preview, same as
/// `preview_chrome_drives_the_below_hero_sections_to_zero_in_full_trailer_mode_and_back`'s setup)
/// and asserts the underlying session read never happens after the fixture's own `save` primed the
/// cache — not even once, let alone once per frame. Watched RED on 2026-09-18, when
/// `preview::enabled()` still called `session::peek()`: reverting `peek()` to its pre-cache,
/// always-reads-`IO` form failed this with `reads=1` (left) vs the expected `reads=0` (right). Since
/// P1c nothing on this path peeks, so that revert no longer turns it red; the peek cache's own guard
/// is `session_cache_tests`.
#[test]
fn preview_tick_does_not_read_the_session_file_every_frame() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    let mut screen = bare(&_guard, sid, "show");

    // A readable session, so a `peek()` anywhere on this path has real Ready bytes behind it,
    // rather than the trivially-cheap Missing/default path.
    let _session = plx_plex::plex::session::TempSession::new("detail-preview-fps");
    plx_plex::plex::session::save(&plx_plex::plex::session::Session {
        client_id: "cid-detail-preview-fps".into(),
        prefs: plx_plex::plex::session::Preferences { trailer_autoplay: true, ..Default::default() },
        ..Default::default()
    });

    let mut effects = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let mut sink = Effects::new(
        &mut effects,
        plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(1)),
        &mut present,
    );
    let hero_focus = Some(Located::Hero(hero::HeroCtl::Play));

    plx_plex::plex::session::reset_reads_for_test();
    let mut now = 0u32;
    for _ in 0..30 {
        now += 16;
        screen.preview_tick::<TestHost>(now, 0.016, hero_focus, &mut sink, test_store().view());
    }
    let reads = plx_plex::plex::session::reads_for_test();
    assert_eq!(
        reads, 0,
        "preview_tick must not re-read the session file every frame -- {reads} session reads over \
         30 frames of hero focus reproduces the 60->26 fps regression (2026-09-18); the write-through \
         cache installs on `save` above, so any peek() reached from here must be a hit"
    );

    drop(sink);
    plx_media::player::preview::reset_for_test();
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
    assert_eq!(screen.art_identity(screen.detail(test_store().view())).2, "episode-still");
    clear();
}

#[test]
fn a_shows_hero_still_outranks_every_other_art() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.art = "show-art".into();
    d.seasons[0].viewed_leaf_count = 1;
    d.on_deck = Some(plx_data::metadata::Episode {
        thumb: "next-still".into(),
        resume_ms: 1,
        ..Default::default()
    });
    let _guard = install(d);
    let screen = bare(&_guard, sid, "show");
    assert_eq!(screen.art_identity(screen.detail(test_store().view())).2, "next-still");
    clear();
}

#[test]
fn a_long_synopsis_keeps_the_first_section_one_region_gap_below_the_buttons() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.summary = "A long synopsis whose wrapped lines make the hero taller. ".repeat(20);
    d.crew.push(plx_data::metadata::Cast {
        tag: "Writer".into(),
        role: "Writer".into(),
        thumb: String::new(),
        id: 1,
        tag_key: String::new(),
    });
    let _guard = install(d);
    let screen = bare(&_guard, sid, "show");
    let detail = screen.detail(test_store().view()).unwrap();
    let chain = screen.hero_chain(&plx_ui::fixture::FixtureMeasure, test_store().view());
    assert_eq!(
        screen.section_top(1, detail, &plx_ui::fixture::FixtureMeasure),
        chain.btn_y + hero::CD + theme::space::XL
    );
    assert_eq!(screen.content_top(&plx_ui::fixture::FixtureMeasure, test_store().view()), screen.section_top(1, detail, &plx_ui::fixture::FixtureMeasure));
    clear();
}

/// **A landing must not move ground being read**, restated for the trailer preview: the
/// identity/meta line, the review scores and the playback note fade to zero alpha while a
/// trailer plays in the background (`preview_prose`/`preview_synopsis`/`preview_chrome`,
/// `screens::detail::mod::draw_hero`'s `chrome`/`prose` painters), and back on collapse. Nothing
/// about the hero's own Y chain may follow that fade: `compute_hero_chain` takes only the item's
/// content (the synopsis text, whether it has ratings) and must return byte-identical geometry
/// whichever way the same item's preview alphas sit.
#[test]
fn the_hero_chain_is_identical_whether_or_not_the_trailer_preview_has_faded_its_prose() {
    let sid = ServerId::UNSET;
    let d = detail(sid, "show");
    let _guard = install(d);
    let mut screen = bare(&_guard, sid, "show");

    let rest = screen.compute_hero_chain(screen.detail(test_store().view()), &plx_ui::fixture::FixtureMeasure);

    screen.preview_prose = 0.0;
    screen.preview_synopsis = 0.0;
    screen.preview_chrome = 0.0;
    screen.preview_field = 0.0;
    let faded = screen.compute_hero_chain(screen.detail(test_store().view()), &plx_ui::fixture::FixtureMeasure);

    assert_eq!(rest.meta_y, faded.meta_y, "meta line must not move when it fades");
    assert_eq!(rest.ratings_y, faded.ratings_y, "ratings row must not move when it fades");
    assert_eq!(rest.syn_y, faded.syn_y, "synopsis must not move");
    assert_eq!(rest.facts_y, faded.facts_y, "facts/playback-note line must not move when it fades");
    assert_eq!(rest.btn_y, faded.btn_y, "the action row must not move");
    clear();
}

#[test]
fn only_holdable_media_and_season_cards_request_the_item_menu() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.related = vec![Default::default()];
    d.cast.push(plx_data::metadata::Cast {
        tag: "Actor".into(),
        role: "Role".into(),
        thumb: String::new(),
        id: 1,
        tag_key: String::new(),
    });
    let _guard = install(d);
    let event = ScreenEvent::PressHold(plx_machine::machine::PressId(9));
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
        source: plx_machine::machine::Source::Script,
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

/// **An episode summary is never cut off for good.** The episode card clamps its summary to
/// `episodes::SUMMARY_MAX_LINES`; this walks the route a viewer takes to the rest: the card's text
/// block opens the episode's own page, that page's About card (OK, i.e. its MORE) asks for the About
/// panel, and the panel pages down to the end of a 2,000-character summary.
///
/// Covered: each hop's effect against the real `DetailScreen` and the real `AboutPanelScreen`, both
/// reading the one metadata store. NOT covered here: the container presenting
/// `ContentPanel::About` as `AboutPanelScreen` (`registry.rs`, one `match` arm) and the drawn
/// pixels, which need the app and the TV.
#[test]
fn a_long_episode_summary_is_readable_to_its_end_through_the_episode_route() {
    use crate::registry::AboutPanelScreen;
    let sid = ServerId::UNSET;
    let summary = "word ".repeat(400);

    // 1. the show page: the episode's text block opens the episode's page
    let mut show = detail(sid, "show");
    show.episodes = vec![episode("e1", 1), plx_data::metadata::Episode { summary: summary.clone(), ..episode("e2", 2) }].into();
    let _guard = install(show);
    let mut screen = bare(&_guard, sid, "show");
    let ok = ScreenEvent::Input(InputEvent {
        at: Default::default(),
        source: plx_machine::machine::Source::Script,
        kind: InputKind::Key { key: Key::Ok, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
    });
    let (_, effects) = step(&mut screen, &ok, Some(episodes::elem(1, episodes::Row::Text).unwrap()));
    assert!(effects.iter().any(|e| matches!(
        &e.fx,
        Fx::App(AppFx::Content(ContentReq::Push(ContentArg::Detail { rk, .. }))) if rk == "e2"
    )));

    // 2. the episode's page (landed with the same summary): the About card asks for the panel
    plx_data::metadata::set_current_for_test(
        test_store().state_mut(),
        Some(Detail { sid, rk: "e2".into(), kind: "episode".into(), summary: summary.clone(), ..Default::default() }),
    );
    let mut page = bare(&_guard, sid, "e2");
    let (_, effects) = step(&mut page, &ScreenEvent::Activate(about::CARD_ELEM), Some(about::CARD_ELEM));
    assert!(
        effects.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::Content(ContentReq::Panel(ContentPanel::About))))),
        "OK on the About card requests the About panel"
    );

    // 3. the panel over the same store pages to the last line
    let mut panel = AboutPanelScreen::new(EntryId(8));
    let measure = plx_ui::fixture::FixtureMeasure;
    assert!(!panel.last_line_in_view(&cx(&measure, None)), "the panel opens at the top of a long summary");
    let down = ScreenEvent::Input(InputEvent {
        at: Default::default(),
        source: plx_machine::machine::Source::Script,
        kind: InputKind::Key { key: Key::Down, sym: plx_ui::consts::SDLK_DOWN, wcode: 0, edge: Edge::Down, at_edge: false },
    });
    let run = |panel: &mut AboutPanelScreen, ev: &ScreenEvent<TestHost>| {
        let context = cx(&measure, None);
        let (mut out, mut present) = (Vec::new(), plx_machine::present::Present::new());
        let mut fx = Effects::new(&mut out, plx_machine::machine::MachineId::Nav, &mut present);
        plx_machine::machine::Machine::step(panel, ev, &context, &mut fx);
    };
    for _ in 0..40 {
        run(&mut panel, &down);
    }
    let tick = ScreenEvent::Tick(plx_machine::machine::Tick { ms: 0, dt_us: 16_667 });
    for _ in 0..600 {
        run(&mut panel, &tick);
    }
    assert!(panel.last_line_in_view(&cx(&measure, None)), "DOWN reaches the end of the episode summary");
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
            source: plx_machine::machine::Source::Script,
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

/// **The input arm's own claim: full-trailer mode answers a key it owns on EVERY edge, before any
/// other arm, but a `Reveal`-mapped key only ACTS on the DOWN edge.** UP is the probe:
/// `trailer::trailer_key` maps it to `Reveal`, which has no effect this test can mistake for
/// ordinary UP navigation, so a `revealed()` flip after the DOWN edge (and none after the UP edge)
/// can only have come from this arm running — and running before whatever ordinary UP handling
/// exists further down. (LEFT/RIGHT now map to `Scrub`, which — unlike `Reveal` — DOES act on
/// every edge; that contract is graded separately, by the scrub-specific tests below.)
#[test]
fn full_trailer_mode_swallows_every_edge_of_an_owned_key_but_acts_only_on_the_down_edge() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    let mut screen = bare(&_guard, sid, "show");
    screen.preview_promoted = true;
    plx_media::player::preview::force_playing_for_test();
    assert!(screen.full_trailer(), "the fixture must actually be in full-trailer mode");

    fn key_event(key: Key, edge: Edge) -> ScreenEvent<TestHost> {
        ScreenEvent::Input(InputEvent {
            at: Default::default(),
            source: plx_machine::machine::Source::Script,
            kind: InputKind::Key { key, sym: 0, wcode: 0, edge, at_edge: false },
        })
    }
    let play = Some(hero::HeroCtl::Play.elem());

    let (handled, _) = step(&mut screen, &key_event(Key::Up, Edge::Up), play);
    assert_eq!(handled, Handled::Yes, "the up edge of an owned key must still be swallowed");
    assert!(!screen.trailer_ctl.revealed(), "the up edge must not act");

    let (handled, _) = step(&mut screen, &key_event(Key::Up, Edge::Down), play);
    assert_eq!(handled, Handled::Yes, "the down edge must be swallowed too");
    assert!(
        screen.trailer_ctl.revealed(),
        "the down edge must act (Reveal) — proof this ran before ordinary UP handling"
    );

    plx_media::player::preview::reset_for_test();
    clear();
}

/// Drive one full-trailer key's EFFECT (`trailer_act`) directly. `full_trailer()`'s own guard
/// (swallowing every edge, acting only on Down) is covered end to end at the input-arm level by
/// `full_trailer_mode_swallows_every_edge_of_an_owned_key_but_acts_only_on_the_down_edge` above,
/// via the `player::preview::force_playing_for_test()`/`set_phase_for_test` seam; what is graded
/// here is only this page's own per-key EFFECT, which does not need the live singleton at all. The
/// key ladder that chooses the action is pure and graded in `screens::detail::trailer`.
fn trailer_act(
    screen: &mut DetailScreen,
    act: trailer::TrailerKey,
) -> Vec<plx_machine::machine::Stamped<TestHost>> {
    trailer_act_edge(screen, act, Edge::Down, 0)
}

/// The edge-aware twin, for `Scrub`'s own tests below — every other variant here only ever acts
/// on `Down`, which is what the plain [`trailer_act`] above always passes.
fn trailer_act_edge(
    screen: &mut DetailScreen,
    act: trailer::TrailerKey,
    edge: Edge,
    now: u32,
) -> Vec<plx_machine::machine::Stamped<TestHost>> {
    let mut effects = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let mut sink = Effects::new(
        &mut effects,
        plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(1)),
        &mut present,
    );
    screen.trailer_act::<TestHost>(act, edge, now, &mut sink);
    drop(sink);
    effects
}

fn transport_reqs(
    effects: &[plx_machine::machine::Stamped<TestHost>],
) -> Vec<Option<bool>> {
    effects
        .iter()
        .filter_map(|effect| match &effect.fx {
            Fx::App(AppFx::Content(ContentReq::PreviewTransport(play))) => Some(*play),
            _ => None,
        })
        .collect()
}

fn seek_reqs(effects: &[plx_machine::machine::Stamped<TestHost>]) -> Vec<i64> {
    effects
        .iter()
        .filter_map(|effect| match &effect.fx {
            Fx::App(AppFx::Content(ContentReq::PreviewSeek(target_ns))) => Some(*target_ns),
            _ => None,
        })
        .collect()
}

/// **Full-trailer mode's transport keys.** OK/PLAYPAUSE ask for the toggle, the remote's dedicated
/// PLAY and PAUSE ask for their own direction, and every one of the three leaves the controls on
/// screen. (LEFT/RIGHT's own `Scrub` request — `ContentReq::PreviewSeek` — is graded separately,
/// by `a_held_left_right_scrub_commits_a_preview_seek_on_key_up` below: unlike these three, it
/// only fires once the gesture ends, and only on some edges.)
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

/// `trailer_act`'s `Scrub` arm reads `player::duration_ns()`/`playpos_ns()` on `Down`
/// (`Transport::scrub_fresh`'s own doc: passed in rather than read inside `trailer.rs`, but
/// `trailer_act` is exactly the one caller that does the reading). Those are the crate-wide
/// `SHARED` atomics — held together with the `testlock::serial()` guard `install` already hands
/// back (construct the fixture AFTER it, never beside a second `serial()`: that lock is a plain
/// mutex and taking it twice on one thread hangs the suite rather than failing it), same as
/// `screens::player::mod`'s own scrub `Fixture`. `duration_ns` must be positive or
/// `scrub_fresh` is a deliberate no-op (nothing to scrub within).
struct DurationFixture(i64, i64);
impl DurationFixture {
    const DUR: i64 = 100_000_000_000;
    fn new() -> Self {
        use std::sync::atomic::Ordering::Relaxed;
        let was = DurationFixture(
            plx_media::player::SHARED.duration_ns.load(Relaxed),
            plx_media::player::SHARED.playpos_ns.load(Relaxed),
        );
        plx_media::player::SHARED.duration_ns.store(Self::DUR, Relaxed);
        plx_media::player::SHARED.playpos_ns.store(0, Relaxed);
        was
    }
}
impl Drop for DurationFixture {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        plx_media::player::SHARED.duration_ns.store(self.0, Relaxed);
        plx_media::player::SHARED.playpos_ns.store(self.1, Relaxed);
    }
}

/// **`TrailerKey::Scrub` is dispatched on every edge, not just `Down`** — `Down` hops the fixed
/// step, `Up` (with no repeat in between, i.e. a tap) arms the debounce rather than committing at
/// once, and it fires a `ContentReq::PreviewSeek` — never `PreviewTransport` — once the debounce's
/// own tick (driven by `preview_tick`, not exercised by this direct `trailer_act` harness) elapses.
/// This proves the wiring from the key ladder into `Transport::scrub_fresh`/`scrub_release`; the
/// gesture math itself (accumulation, the hold ramp, the lost-keyup net) is graded in
/// `screens::detail::trailer`'s own tests.
#[test]
fn left_right_scrub_hops_on_down_and_asks_for_no_transport_request() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    let _dur = DurationFixture::new();
    let mut screen = bare(&_guard, sid, "show");
    screen.preview_promoted = true;

    let effects = trailer_act_edge(&mut screen, trailer::TrailerKey::Scrub(true), Edge::Down, 0);

    assert!(transport_reqs(&effects).is_empty(), "a scrub press is not a pause/resume");
    assert!(seek_reqs(&effects).is_empty(), "a fresh press previews — it does not commit yet");
    assert!(screen.trailer_ctl.scrubbing(), "the gesture is now tracked on the transport");
    assert!(screen.trailer_ctl.revealed(), "any key the mode keeps re-arms the linger");
    assert!(screen.preview_promoted, "a scrub must not leave the mode");
    clear();
}

/// A HELD scrub commits once its key-up has survived the key-bounce window, through
/// `ContentReq::PreviewSeek` — never `route::request_seek`'s own `PlayerReq::SeekTo`/`CommitSeek`,
/// which is the whole point of routing a preview's seek through `player::preview::seek` instead
/// (see `ContentReq::PreviewSeek` and `player/preview.rs`'s own module doc for the watch-state
/// promise this keeps). The key-up itself commits nothing; the page's tick (`preview_tick`, not
/// exercised by this direct `trailer_act` harness) does, so the window is stepped by hand here.
#[test]
fn a_held_left_right_scrub_commits_a_preview_seek_on_key_up() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    let _dur = DurationFixture::new();
    let mut screen = bare(&_guard, sid, "show");
    screen.preview_promoted = true;

    trailer_act_edge(&mut screen, trailer::TrailerKey::Scrub(true), Edge::Down, 0);
    trailer_act_edge(&mut screen, trailer::TrailerKey::Scrub(true), Edge::Repeat, 0);
    let target = screen.trailer_ctl.preview_ns_for_test();
    let effects = trailer_act_edge(&mut screen, trailer::TrailerKey::Scrub(true), Edge::Up, 0);

    assert!(seek_reqs(&effects).is_empty(), "the key-up waits out the bounce window first");
    assert!(transport_reqs(&effects).is_empty(), "a seek is not a pause/resume");
    assert!(screen.trailer_ctl.scrubbing(), "the preview stays up while it waits");
    let commit = screen.trailer_ctl.step_scrub_hold(
        plx_appkit::player_hud::SCRUB_RELEASE_DEBOUNCE_MS,
        DurationFixture::DUR,
    );
    assert_eq!(commit, Some(target), "the hold's own preview position, verbatim");
    assert!(!screen.trailer_ctl.scrubbing(), "committing ends the gesture");
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

/// The collapse's OTHER branch: `preview::paused()` true means the transport was left paused when
/// the mode closed, so `collapse_full_trailer` asks for a resume on the way out. The empty case
/// above (nothing paused, nothing to resume) is the only one the pure `trailer_act` path can reach
/// on its own; `preview::paused()` reads the live singleton and `player::TX`, so this one drives
/// both — the same `force_playing_for_test`/`reset_for_test` seam as the input-arm test above, plus
/// `TX.commit_paused`/`TX.reset`, the same production seam `player::mod`'s own tests use.
#[test]
fn the_collapse_key_resumes_the_transport_when_it_was_left_paused() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    let mut screen = bare(&_guard, sid, "show");
    screen.preview_promoted = true;
    screen.trailer_ctl.reveal();
    plx_media::player::preview::force_playing_for_test();
    plx_media::player::TX.commit_paused(true);
    assert!(plx_media::player::preview::paused(), "the fixture must actually be paused");

    let effects = trailer_act(&mut screen, trailer::TrailerKey::Collapse);

    assert!(!screen.preview_promoted, "the mode must be over");
    assert!(!screen.trailer_ctl.revealed(), "the controls go with it");
    assert_eq!(
        transport_reqs(&effects),
        vec![Some(true)],
        "leaving the mode while paused must ask to resume"
    );

    plx_media::player::TX.reset();
    plx_media::player::preview::reset_for_test();
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
        source: plx_machine::machine::Source::Script,
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

/// The preview logo's own scroll fade: untouched at `t=0` (a normal hero has no preview to pin
/// into a corner, and the caller's own `hero_alpha` already governs it), full at the top even
/// while shrunk (`t=1`, `scroll_pos=0`), eased down as `scroll_pos` climbs toward `hero_extent`
/// (where the below-hero flow starts), pinned at 0 once past it, and — because it is a pure
/// function of the CURRENT `scroll_pos` with no memory — back to full the moment scroll returns
/// to 0, exactly the "come back when scrolled back up" the owner asked for.
#[test]
fn preview_logo_scroll_alpha_only_fades_the_shrunk_logo_past_the_hero() {
    let extent = 800.0_f32;

    // t=0: a normal hero position is untouched by this factor at any scroll.
    for scroll in [0.0, 400.0, 800.0, 2000.0] {
        assert_eq!(
            super::preview_logo_scroll_alpha(scroll, extent, 0.0),
            1.0,
            "t=0 (no preview) must not be touched by this fade at scroll={scroll}"
        );
    }

    // t=1: full at the very top, and monotonically non-increasing as scroll rises.
    assert_eq!(super::preview_logo_scroll_alpha(0.0, extent, 1.0), 1.0);
    let mut prev = 1.0;
    let mut s = 0.0;
    while s <= extent * 1.5 {
        let a = super::preview_logo_scroll_alpha(s, extent, 1.0);
        assert!((0.0..=1.0).contains(&a), "scroll={s}: alpha {a} outside 0..=1");
        assert!(a <= prev + 1e-6, "scroll={s}: alpha rose from {prev} to {a}");
        prev = a;
        s += extent / 16.0;
    }
    assert_eq!(
        super::preview_logo_scroll_alpha(extent, extent, 1.0),
        0.0,
        "fully hidden once scrolled exactly to the below-hero flow's own start"
    );
    assert_eq!(
        super::preview_logo_scroll_alpha(extent * 2.0, extent, 1.0),
        0.0,
        "clamped, not negative, once scrolled well past it"
    );

    // Scrolling back up restores it — a pure function of the current position, no hysteresis.
    assert_eq!(
        super::preview_logo_scroll_alpha(extent, extent, 1.0),
        0.0
    );
    assert_eq!(
        super::preview_logo_scroll_alpha(0.0, extent, 1.0),
        1.0,
        "back to full the instant scroll returns to the top"
    );

    // Partway through `t` (the spring mid-travel) blends the two: half-shrunk halves the fade.
    assert_eq!(
        super::preview_logo_scroll_alpha(extent, extent, 0.5),
        0.5,
        "at t=0.5 the fully-past-hero case should only be half faded"
    );
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
    let _guard = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("detail-watch", "127.0.0.1", 1, "t", "c");
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(Detail {
        sid,
        rk: "movie".into(),
        kind: "movie".into(),
        watched: false,
        ..Default::default()
    }));
    let mut screen = bare(&_guard, sid, "movie");
    let (_, effects) = step(
        &mut screen,
        &ScreenEvent::PressCommit(plx_machine::machine::PressId(1)),
        Some(hero::ELEM_MARK_WATCHED),
    );
    let addressed: Vec<_> = effects.iter().filter_map(|effect| match &effect.fx {
        Fx::App(AppFx::Store(StoreId::ViewState,
            plx_data::stores::StoreCmd::ViewState(ViewStateCmd::Request {
                sid, rk, write, guid, ..
            }))) => Some((*sid, rk.as_str(), *write, guid.as_str())),
        _ => None,
    }).collect();
    assert_eq!(addressed, [(sid, "movie", plx_data::viewstate::Write::Watched, "")],
        "Detail must address the typed ViewState command to its owning Bridge");
    assert!(!test_store().view().current().unwrap().watched,
        "the screen must not call the process-global compatibility facade itself");
    clear();
    plx_plex::plex::reset_servers_for_test();
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
        &ScreenEvent::Tick(plx_machine::machine::Tick {
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
    let measure = plx_ui::fixture::FixtureMeasure;
    let context = cx(&measure, Some(to.elem));
    let mut present = plx_machine::present::Present::new();
    let _ = present.take(0);
    let mut effects = Vec::new();
    for ms in [50, 100, 150] {
        let mut sink = Effects::new(
            &mut effects,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(1)),
            &mut present,
        );
        Machine::<TestHost>::step(
            &mut screen,
            &ScreenEvent::Tick(plx_machine::machine::Tick { ms, dt_us: 50_000 }),
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
    let guard = plx_base::testlock::serial();
    let mut screen = bare(&guard, sid, "show");
    assert!(screen.detail(test_store().view()).is_none(), "no metadata installed for this test");
    let measure = plx_ui::fixture::FixtureMeasure;
    let context = cx(&measure, None);
    let mut present = plx_machine::present::Present::new();
    let _ = present.take(0);
    let mut effects = Vec::new();
    for ms in [16, 32, 48] {
        let mut sink = Effects::new(
            &mut effects,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(1)),
            &mut present,
        );
        Machine::<TestHost>::step(
            &mut screen,
            &ScreenEvent::Tick(plx_machine::machine::Tick { ms, dt_us: 16_667 }),
            &context,
            &mut sink,
        );
        assert!(
            present.take(ms),
            "an unloaded detail page's spinner must present every frame (ms={ms})"
        );
    }
}

fn trailer_extra() -> plx_data::metadata::Extra {
    plx_data::metadata::Extra {
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

fn play_item(effects: &[plx_machine::machine::Stamped<TestHost>]) -> Option<(&PlayIntent, i64)> {
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
    test_store().run(plx_data::stores::metadata::MetadataCmd::SetNowPlaying(Some(
        plx_data::metadata::NowPlaying {
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
    assert!(!screen.hero_set(test_store().view()).trailer, "the preview path replaced the Trailer disc");
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
            assert_eq!(context, plx_data::metadata::TRAILER_CONTEXT);
            assert_eq!(title, "Official Trailer");
        }
        _ => panic!("expected PlayIntent::Item for Trailer"),
    }
    assert_eq!(resume_ns, 0);
    assert_eq!(test_store().view().current().unwrap().rk, "movie");
    assert!(
        test_store().view().now_playing().is_some_and(|n| n.detail_rk == "show"),
        "activate queues Play; NowPlaying is installed only after request_play accepts"
    );
    test_store().run(plx_data::stores::metadata::MetadataCmd::SetNowPlaying(None));
    clear();
}

#[test]
fn a_show_trailer_disc_plays_the_extra_not_the_on_deck_episode() {
    let extra = trailer_extra();
    let mut d = detail(ServerId::UNSET, "show");
    d.extras = vec![extra];
    d.on_deck = Some(plx_data::metadata::Episode {
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
            assert_eq!(context, plx_data::metadata::TRAILER_CONTEXT);
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
            assert_ne!(context.as_str(), plx_data::metadata::TRAILER_CONTEXT);
        }
        (PlayIntent::Movie { .. }, _) => {}
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
    assert!(!screen.hero_set(test_store().view()).trailer);
    assert!(hero::index_of(screen.hero_set(test_store().view()), hero::HeroCtl::Trailer).is_none());
    clear();
}

#[test]
fn a_trailer_disc_requires_both_rk_and_part() {
    let _guard = plx_base::testlock::serial();
    let cases = [
        plx_data::metadata::Extra {
            rk: String::new(),
            part: "/library/parts/trailer".into(),
            ..Default::default()
        },
        plx_data::metadata::Extra {
            rk: "9".into(),
            part: String::new(),
            ..Default::default()
        },
    ];
    test_store().run(plx_data::stores::metadata::MetadataCmd::SetNowPlaying(Some(
        plx_data::metadata::NowPlaying {
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
        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(Detail {
            sid: ServerId::UNSET,
            rk: "movie".into(),
            kind: "movie".into(),
            extras: vec![extra],
            ..Default::default()
        }));
        let screen = bare(&_guard, ServerId::UNSET, "movie");
        assert!(
            !screen.hero_set(test_store().view()).trailer,
            "visibility must match Extra::playable, not a nonempty part alone"
        );
        assert!(hero::index_of(screen.hero_set(test_store().view()), hero::HeroCtl::Trailer).is_none());
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
            test_store().view().now_playing().is_some_and(|n| n.detail_rk == "show"),
            "a Trailer no-op must not wipe a leftover episode NowPlaying"
        );
    }
    test_store().run(plx_data::stores::metadata::MetadataCmd::SetNowPlaying(None));
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
    let before = screen.hero_set(test_store().view());
    assert!(!before.trailer);
    assert_eq!(hero::index_of(before, hero::HeroCtl::Play), Some(0));
    let play_elem = hero::HeroCtl::Play.elem();
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(Detail {
        sid,
        rk: "movie".into(),
        kind: "movie".into(),
        extras: vec![trailer_extra()],
        ..Default::default()
    }));
    let after = screen.hero_set(test_store().view());
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
    impl plx_machine::machine::Measure for CountMeasure {
        fn width(&self, s: &CStr, sz: i32, bold: bool) -> f32 {
            self.widths.set(self.widths.get() + 1);
            plx_ui::fixture::FixtureMeasure.width(s, sz, bold)
        }
        fn cap_h(&self, sz: i32) -> f32 {
            plx_ui::fixture::FixtureMeasure.cap_h(sz)
        }
        fn line_h(&self, sz: i32) -> f32 {
            plx_ui::fixture::FixtureMeasure.line_h(sz)
        }
    }

    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.summary = "word ".repeat(80);
    d.episodes = (0..16).map(|i| episode(&format!("e{i}"), i as i64)).collect();
    d.cast = (0..77)
        .map(|i| plx_data::metadata::Cast {
            tag: format!("Actor {i}"),
            role: "Role".into(),
            thumb: String::new(),
            id: i as i64 + 1,
            tag_key: String::new(),
        })
        .collect();
    let _guard = install(d);
    let screen = bare(&_guard, sid, "show");
    let detail = screen.detail(test_store().view()).expect("installed");
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
    d.cast = vec![plx_data::metadata::Cast {
        tag: "Actor".into(),
        role: "Role".into(),
        thumb: String::new(),
        id: 1,
        tag_key: String::new(),
    }];
    d.related = vec![Default::default()];
    let _guard = install(d);
    let screen = bare(&_guard, sid, "show");
    let detail = screen.detail(test_store().view()).expect("installed");
    let measure = plx_ui::fixture::FixtureMeasure;

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
    // The walk asks for each gap the same way the flow does, because there are three of them and
    // which one applies is a property of the section ABOVE: a shelf (4 cast, 3 related) already
    // carries its own label band, so what follows it is `UNDER_LABEL_AIR` and not a second full
    // region gap stacked on top of it. Hard-coding `SECTION_GAP` here is what made this test read
    // the layout as 42px out when the shelves stopped double-spacing.
    let gap = |above: i32, below: i32| DetailScreen::section_gap(above, Some(below));
    assert_eq!(gap(4, 3), plx_ui::consts::UNDER_LABEL_AIR, "a shelf brings its own band");
    assert_eq!(gap(2, 4), super::SECTION_GAP, "a bare list does not");

    assert_eq!(seasons, screen.content_top(&measure, test_store().view()));
    assert_eq!(episodes, seasons + season::ROW_H + super::TAB_EP_GAP);
    assert_eq!(
        cast_first,
        episodes + screen.block_h(2, detail, &measure) + gap(2, 4)
    );
    assert_eq!(
        related,
        cast_first + screen.block_h(4, detail, &measure) + gap(4, 3)
    );
    assert_eq!(
        about,
        related + screen.block_h(3, detail, &measure) + gap(3, 5)
    );
    clear();
}

/// **A shelf section is followed by exactly the shared air, whatever section comes next.** The caption
/// rule counts the collapsed band and `UNDER_LABEL_AIR` under a card (`card_row::room_for`): Related,
/// Extras and the collection's members keep that much between their tiles and the next section's top
/// (their block's collapsed band and the gap after it); the cast row keeps more, because it draws a name
/// under every headshot (`cast::block_h`).
#[test]
fn a_shelf_is_followed_by_the_shared_air() {
    use plx_ui::consts::UNDER_LABEL_AIR;
    let air = ui_cards::LABEL_BAND_COLLAPSED + UNDER_LABEL_AIR;
    for above in [3, 4, 6, 7] {
        for below in [3, 4, 5, 6, 7] {
            assert_eq!(DetailScreen::section_gap(above, Some(below)), UNDER_LABEL_AIR, "section {above} above section {below}");
        }
        assert_eq!(DetailScreen::section_gap(above, None), UNDER_LABEL_AIR, "the last section's air is the same");
    }
    assert_eq!(related::block_h(0.0) - related::LABEL_H - ui_cards::RowStyle::HOME.h + UNDER_LABEL_AIR, air, "Related");
    assert_eq!(collection::block_h(0.0), related::block_h(0.0), "the collection's members");
    assert_eq!(extras::block_h(0.0) - extras::block_h(1.0), ui_cards::under_band(0.0) - ui_cards::under_band(1.0), "Extras collapses by the shared band");
    assert!(cast::block_h() - cast::LABEL_H - ui_cards::RowStyle::CAST.h + UNDER_LABEL_AIR >= air, "the cast row keeps at least the rule's room");
}

#[test]
fn a_replaced_episode_list_moves_the_cast_row() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.cast = vec![plx_data::metadata::Cast {
        tag: "Actor".into(),
        role: "Role".into(),
        thumb: String::new(),
        id: 1,
        tag_key: String::new(),
    }];
    let _guard = install(d);
    let screen = bare(&_guard, sid, "show");
    let measure = plx_ui::fixture::FixtureMeasure;
    let before = screen.section_top(4, screen.detail(test_store().view()).expect("installed"), &measure);

    let mut taller = detail(sid, "show");
    taller.cast = vec![plx_data::metadata::Cast {
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
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(taller));
    let after = screen.section_top(4, screen.detail(test_store().view()).expect("replaced"), &measure);
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
        cast: vec![plx_data::metadata::Cast {
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
    let detail = screen.detail(test_store().view()).expect("installed");
    let measure = plx_ui::fixture::FixtureMeasure;
    let top = screen.content_top(&measure, test_store().view());
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
    impl plx_machine::machine::Measure for CountMeasure {
        fn width(&self, s: &CStr, sz: i32, bold: bool) -> f32 {
            self.widths.set(self.widths.get() + 1);
            plx_ui::fixture::FixtureMeasure.width(s, sz, bold)
        }
        fn cap_h(&self, sz: i32) -> f32 {
            plx_ui::fixture::FixtureMeasure.cap_h(sz)
        }
        fn line_h(&self, sz: i32) -> f32 {
            plx_ui::fixture::FixtureMeasure.line_h(sz)
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
    let detail = screen.detail(test_store().view()).expect("installed");
    let _ = screen.section_top(1, detail, &measure);
    let after_first = measure.widths.get();
    assert!(after_first > 0);
    let _ = screen.section_top(1, detail, &measure);
    assert_eq!(measure.widths.get(), after_first, "still cached before tick");
    step(
        &mut screen,
        &ScreenEvent::Tick(plx_machine::machine::Tick {
            ms: 16,
            dt_us: 16_667,
        }),
        None,
    );
    let _ = screen.section_top(1, screen.detail(test_store().view()).expect("still installed"), &measure);
    assert!(
        measure.widths.get() > after_first,
        "tick must drop the walk so the next present can remeasure"
    );
    clear();
}

#[test]
fn extras_sit_after_cast_and_crew_and_do_not_move_the_compact_title() {
    let extra = plx_data::metadata::Extra {
        rk: "9".into(),
        part: "/p".into(),
        title: "Clip".into(),
        subtype: "behindTheScenes".into(),
        extra_type: 5,
        ..Default::default()
    };
    let mut show = detail(ServerId::UNSET, "show");
    show.cast.push(plx_data::metadata::Cast {
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
    let loaded = screen.detail(test_store().view()).expect("installed");
    let (sections, n) = screen.sections(Some(loaded));
    assert_eq!(
        &sections[..n],
        &[0, 1, 2, 4, 6, 3, 5],
        "extras sits after Cast and before Related"
    );
    // The pinned title is no longer anchored to a NAMED section: it leaves as soon as the first
    // block below the hero starts to travel, whichever section that is.
    let first_top = screen.section_top_settled(sections[1], loaded, &plx_ui::fixture::FixtureMeasure);
    let hide_at = first_top - plx_ui::detail_layout::TOP_MARGIN;
    assert_eq!(super::compact_title_alpha(hide_at, first_top, 0.0), 1.0);
    assert_eq!(super::compact_title_alpha(hide_at + 400.0, first_top, 0.0), 0.0);

    let mut spot = plx_data::metadata::Spot::default();
    spot.section = 6;
    spot.col = 1;
    screen.restore(&spot, test_store().view());
    let key = screen.restore_focus(test_store().view()).expect("extras column restores");
    assert!(matches!(screen.locate(key, test_store().view()), Some(Located::Extras(1))));

    let movie = Detail {
        sid: ServerId::UNSET,
        rk: "movie".into(),
        kind: "movie".into(),
        extras: vec![
            plx_data::metadata::Extra {
                rk: "9".into(),
                part: "/p".into(),
                title: "Clip".into(),
                subtype: "behindTheScenes".into(),
                extra_type: 5,
                ..Default::default()
            },
            plx_data::metadata::Extra {
                rk: "8".into(),
                part: "/q".into(),
                title: "Trailer".into(),
                subtype: "trailer".into(),
                extra_type: 1,
                ..Default::default()
            },
        ],
        cast: vec![plx_data::metadata::Cast {
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
    // Same rule on a movie, whose first below-hero section is Cast rather than the season strip:
    // the title is out by the time that block has moved a fraction of its own height.
    let first_top = screen.section_top_settled(sections[1], &movie, &plx_ui::fixture::FixtureMeasure);
    let hide_at = first_top - plx_ui::detail_layout::TOP_MARGIN;
    assert!(super::compact_title_alpha(hide_at - 1.0, first_top, 0.0) > 0.99);
    assert!(super::compact_title_alpha(hide_at + 400.0, first_top, 0.0) < 0.01);
    clear();
}

/// Choosing an episode keeps the pinned show logo: the page rests with the season strip just under
/// it (the scroll the strip's own focus gives), because resting with the strip scrolled up into the
/// logo's band drew the wordmark half-faded across the season pills (issue #679).
#[test]
fn focusing_an_episode_rests_the_season_strip_under_the_logo_and_keeps_the_logo() {
    let _guard = install(deck_show(3, 0, 8, None));
    let screen = bare(&_guard, ServerId::UNSET, "show");
    let d = screen.detail(test_store().view()).expect("installed");
    let measure = plx_ui::fixture::FixtureMeasure;
    let (sections, n) = screen.sections(Some(d));
    assert_eq!(&sections[..3.min(n)], &[0, 1, 2], "a show with seasons: hero, strip, episodes");
    let strip_top = screen.section_top_settled(1, d, &measure);
    for focused in [1, 2] {
        let target = screen.reveal_target(focused, d, &measure);
        assert_eq!(target, strip_top - plx_ui::detail_layout::TOP_MARGIN, "section {focused} rests with the strip under the logo");
        assert_eq!(super::compact_title_alpha(target, strip_top, 0.0), 1.0, "the logo is fully shown at rest on section {focused}");
    }
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
        cast: vec![plx_data::metadata::Cast {
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
    let loaded = screen.detail(test_store().view()).expect("installed");
    let (sections, n) = screen.sections(Some(loaded));
    assert_eq!(
        &sections[..n],
        &[0, 4, 3, 5],
        "Cast sits at array position 1, three away from its own SectionId discriminant (4)"
    );
    let measure = plx_ui::fixture::FixtureMeasure;
    let cast_top = screen.section_top(section::SectionId::Cast.raw(), loaded, &measure);
    let content_top = screen.content_top(&measure, test_store().view());
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
        "ui/src/detail_layout.rs",
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
        "ui/src/detail_layout.rs",
    ),
    (
        "the_people_column_grows_upward_off_the_button_row",
        "ui/src/detail_layout.rs",
    ),
    (
        "a_movie_hides_across_its_second_below_hero_block_not_its_first",
        "ui/src/detail_layout.rs",
    ),
    (
        "a_movie_with_only_one_below_hero_block_keeps_the_first_block_rule",
        "ui/src/detail_layout.rs",
    ),
    (
        "no_below_hero_section_means_nothing_to_hide_across",
        "ui/src/detail_layout.rs",
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
        "ui/src/detail_layout.rs",
    ),
    (
        "a_shows_hero_is_about_the_servers_on_deck_episode_or_the_series",
        "screens/detail/hero.rs",
    ),
    (
        "the_ratings_band_is_reserved_when_there_are_scores_and_never_otherwise",
        "ui/src/detail_layout.rs",
    ),
    (
        "a_two_line_blurb_lands_the_chain_on_the_mockups_own_ys",
        "ui/src/detail_layout.rs",
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

// ---- The collection shelf (a member movie's collection, split out of Related) ----

fn collection_member(sid: ServerId, rk: &str) -> plx_data::pms::PmsMovie {
    plx_data::pms::PmsMovie { sid, rk: rk.into(), title: format!("Film {rk}"), sec: 1, ..Default::default() }
}

fn collection_movie(sid: ServerId) -> Detail {
    Detail {
        sid,
        rk: "m1".into(),
        kind: "movie".into(),
        part: "/library/parts/1".into(),
        collection: Some(plx_data::metadata::CollectionShelf {
            title: "Example Trilogy".into(),
            section: 1,
            tag: 812,
            members: ["m1", "m2", "m3", "m4"].iter().map(|rk| collection_member(sid, rk)).collect(),
            count: 4,
        }),
        related: ["r1", "r2"].iter().map(|rk| collection_member(sid, rk)).collect(),
        ..Default::default()
    }
}

fn collection_move(
    screen: &DetailScreen,
    engine: &mut plx_ui::focus::FocusEngine<u32>,
    dir: Dir,
) -> plx_ui::focus::Outcome<u32> {
    let owner = plx_machine::machine::InputOwner::Entry(EntryId(7));
    let measure = plx_ui::fixture::FixtureMeasure;
    let context = Cx { focus: engine.read(owner), ..cx(&measure, None) };
    let mut links = Vec::new();
    Screen::<TestHost>::links(screen, &mut links);
    engine.move_dir(owner, screen, &links, dir, &context)
}

fn moved_to(outcome: plx_ui::focus::Outcome<u32>, why: &str) -> u32 {
    match outcome {
        plx_ui::focus::Outcome::Moved { to, .. } => to.elem,
        other => panic!("{why}: expected a move, got {other:?}"),
    }
}

#[test]
fn the_collection_shelf_sits_above_related_under_a_linked_heading() {
    let sid = ServerId::UNSET;
    let _guard = install(collection_movie(sid));
    let screen = bare(&_guard, sid, "m1");
    let meta = test_store().view();
    let d = screen.detail(meta).unwrap();
    let (sections, n) = screen.sections(Some(d));
    assert_eq!(&sections[..n], &[0, 7, 3, 5], "collection, then Related, then About");
    let measure = plx_ui::fixture::FixtureMeasure;
    assert!(screen.section_top(7, d, &measure) < screen.section_top(3, d, &measure));

    assert_eq!(screen.engine_key(collection::HEADING_ELEM), Some(collection::HEADING_ELEM),
        "the heading is a slot, not an interned item");
    assert!(screen.locate(collection::HEADING_ELEM, meta) == Some(Located::CollectionHeading));
    for i in 0..4 {
        let key = screen.engine_key(collection::elem(i).unwrap()).expect("a member card key");
        assert!(key >= FIRST_ITEM_ELEM);
        assert!(screen.locate(key, meta) == Some(Located::Collection(i)));
        assert!(screen.keys.iter().any(|k| k.elem == key
            && matches!(&k.identity, DetailIdentity::CollectionMember { rk, .. } if *rk == format!("m{}", i + 1))));
    }

    let mut groups = Vec::new();
    Focusable::<TestHost>::groups(&screen, &cx(&measure, None), &mut groups);
    let heading = groups.iter().find(|g| g.id == collection::HEADING_GROUP).expect("the heading group");
    let shelf = groups.iter().find(|g| g.id == collection::COLLECTION_GROUP).expect("the shelf group");
    assert_eq!((heading.len, shelf.len), (1, 4));
    assert!(heading.extent.y + heading.extent.h <= shelf.extent.y, "the heading sits above the cards");
    clear();
}

#[test]
fn a_page_without_a_collection_declares_no_collection_stops() {
    let sid = ServerId::UNSET;
    let mut d = collection_movie(sid);
    d.collection = None;
    let _guard = install(d);
    let screen = bare(&_guard, sid, "m1");
    let meta = test_store().view();
    let (sections, n) = screen.sections(screen.detail(meta));
    assert!(!sections[..n].contains(&7));
    let measure = plx_ui::fixture::FixtureMeasure;
    let mut groups = Vec::new();
    Focusable::<TestHost>::groups(&screen, &cx(&measure, None), &mut groups);
    assert!(groups.iter().all(|g| g.id != collection::HEADING_GROUP && g.id != collection::COLLECTION_GROUP));
    assert_eq!(Focusable::<TestHost>::group_of(&screen, &collection::HEADING_ELEM, &cx(&measure, None)), None,
        "a heading with no collection is no focus stop");
    clear();
}

/// The page's focus groups are the frame the engine keeps its remembered cursors against: their ids
/// and their order in `groups()` are frozen, and a page with nothing remembered seats on Play.
#[test]
fn the_pages_focus_group_ids_and_order_and_its_first_focus_are_frozen() {
    let sid = ServerId::UNSET;
    let mut d = collection_movie(sid);
    d.cast = vec![plx_data::metadata::Cast {
        tag: "Person".into(), role: String::new(), thumb: String::new(), id: 5, tag_key: String::new(),
    }];
    d.extras = vec![plx_data::metadata::Extra { rk: "x1".into(), ..Default::default() }];
    let _guard = install(d);
    let screen = bare(&_guard, sid, "m1");
    let measure = plx_ui::fixture::FixtureMeasure;
    let context = cx(&measure, None);
    let mut groups = Vec::new();
    Focusable::<TestHost>::groups(&screen, &context, &mut groups);
    let ids: Vec<u32> = groups.iter().map(|g| g.id.0).collect();
    assert_eq!(ids, [hero::HERO_GROUP.0, 4, 6, 8, 7, 3, about::ABOUT_GROUP.0],
        "hero, cast, extras, collection heading, collection, related, about");
    assert_eq!(
        (cast::CAST_GROUP.0, extras::EXTRAS_GROUP.0, collection::HEADING_GROUP.0, collection::COLLECTION_GROUP.0, related::RELATED_GROUP.0),
        (4, 6, 8, 7, 3));
    let first = Focusable::<TestHost>::reconcile(&screen, FocusKey { entry: EntryId(7), elem: 0 }, &context);
    assert_eq!(first.elem, hero::ELEM_PLAY, "a page with nothing remembered seats on Play");
    clear();
}

#[test]
fn up_from_a_member_reaches_the_heading_and_down_returns_to_that_member() {
    use plx_ui::focus::{FocusEngine, Outcome};
    let sid = ServerId::UNSET;
    let _guard = install(collection_movie(sid));
    let screen = bare(&_guard, sid, "m1");
    let owner = plx_machine::machine::InputOwner::Entry(EntryId(7));
    let mut engine = FocusEngine::new();
    let member = |i| screen.engine_key(collection::elem(i).unwrap()).unwrap();
    let key = |elem| FocusKey { entry: EntryId(7), elem };

    assert!(matches!(engine.set(owner, key(member(2)), Some(collection::COLLECTION_GROUP), By::Restore),
        Outcome::Moved { .. }));
    assert_eq!(moved_to(collection_move(&screen, &mut engine, Dir::Up), "UP from a member"),
        collection::HEADING_ELEM);
    assert!(matches!(collection_move(&screen, &mut engine, Dir::Left), Outcome::Nothing),
        "LEFT on the heading is inert");
    assert!(matches!(collection_move(&screen, &mut engine, Dir::Right), Outcome::Nothing),
        "RIGHT on the heading is inert");
    assert_eq!(moved_to(collection_move(&screen, &mut engine, Dir::Down), "DOWN from the heading"),
        member(2), "DOWN returns to the member the shelf remembered");

    // From the hero, DOWN stops on the heading first — the document order.
    engine.set(owner, key(hero::ELEM_PLAY), Some(hero::HERO_GROUP), By::Restore);
    assert_eq!(moved_to(collection_move(&screen, &mut engine, Dir::Down), "DOWN from the hero"),
        collection::HEADING_ELEM);
    // From Related, UP reaches the collection's CARDS; the heading is one more UP.
    let related = screen.engine_key(related::elem(0).unwrap()).unwrap();
    engine.set(owner, key(related), Some(related::RELATED_GROUP), By::Restore);
    let up = moved_to(collection_move(&screen, &mut engine, Dir::Up), "UP from Related");
    assert!(matches!(screen.locate(up, test_store().view()), Some(Located::Collection(_))));
    assert_eq!(moved_to(collection_move(&screen, &mut engine, Dir::Up), "UP again"),
        collection::HEADING_ELEM);
    clear();
}

/// The item-menu opener asks the page for its focused card (`Screen::focused_card`) instead of
/// downcasting to it: for a Related or collection card it must name the item `focused_related`
/// does and the rect `focused_rect` measures, and for anything else (Play, a heading) it names none.
#[test]
fn the_focused_card_is_the_related_item_and_the_rect_the_opener_measured_before() {
    let sid = ServerId::UNSET;
    let _guard = install(collection_movie(sid));
    let screen = bare(&_guard, sid, "m1");
    let meta = test_store().view();
    let measure = plx_ui::fixture::FixtureMeasure;
    let key = |elem| FocusKey { entry: EntryId(7), elem };
    for (elem, rk) in [
        (screen.engine_key(related::elem(1).unwrap()).unwrap(), "r2"),
        (screen.engine_key(collection::elem(2).unwrap()).unwrap(), "m3"),
    ] {
        let context = cx(&measure, Some(elem));
        let card = Screen::<TestHost>::focused_card(&screen, &context, Some(key(elem)), Some(At::Drawn))
            .expect("a related card is a focused card");
        let item = card.item.downcast_ref::<plx_data::pms::PmsMovie>().unwrap();
        assert_eq!(item.rk, rk);
        assert_eq!(item.rk, screen.focused_related(Some(key(elem)), meta).unwrap().rk);
        assert!(card.rect.is_some());
        assert_eq!(card.rect, screen.focused_rect::<TestHost>(Some(key(elem)), &context, At::Drawn));
        assert!(Screen::<TestHost>::focused_card(&screen, &context, Some(key(elem)), None).unwrap().rect.is_none());
    }
    for elem in [hero::ELEM_PLAY, collection::HEADING_ELEM] {
        let context = cx(&measure, Some(elem));
        assert!(Screen::<TestHost>::focused_card(&screen, &context, Some(key(elem)), Some(At::Drawn)).is_none());
    }
    clear();
}

/// A season tab, an episode, an Extras card and a Cast card are not catalog items the item menu
/// acts on through `Screen::focused_card`: none of them names a card, whatever the host asks.
#[test]
fn the_focused_card_is_none_for_season_episode_extras_and_cast() {
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.related = vec![Default::default()];
    d.extras = vec![Default::default()];
    d.cast.push(plx_data::metadata::Cast {
        tag: "Actor".into(),
        role: "Role".into(),
        thumb: String::new(),
        id: 1,
        tag_key: String::new(),
    });
    let _guard = install(d);
    let screen = bare(&_guard, sid, "show");
    let measure = plx_ui::fixture::FixtureMeasure;
    let key = |elem| FocusKey { entry: EntryId(7), elem };
    let related = screen.engine_key(related::elem(0).unwrap()).unwrap();
    let context = cx(&measure, Some(related));
    assert!(Screen::<TestHost>::focused_card(&screen, &context, Some(key(related)), Some(At::Drawn)).is_some(),
        "the fixture's Related card is a focused card, so the negatives below are the page's answer");
    for (name, local) in [
        ("season", season::elem(0).unwrap()),
        ("episode still", episodes::elem(0, episodes::Row::Still).unwrap()),
        ("episode text", episodes::elem(0, episodes::Row::Text).unwrap()),
        ("extras", extras::elem(0).unwrap()),
        ("cast", cast::elem(0).unwrap()),
    ] {
        let elem = screen.engine_key(local).unwrap_or(local);
        let context = cx(&measure, Some(elem));
        for at in [Some(At::Drawn), None] {
            assert!(Screen::<TestHost>::focused_card(&screen, &context, Some(key(elem)), at).is_none(), "{name}");
        }
    }
    clear();
}

#[test]
fn ok_on_the_heading_opens_the_collection_and_ok_on_a_member_opens_its_detail() {
    let sid = ServerId::UNSET;
    let _guard = install(collection_movie(sid));
    let mut screen = bare(&_guard, sid, "m1");
    let (_, effects) = step(&mut screen, &ScreenEvent::Activate(collection::HEADING_ELEM),
        Some(collection::HEADING_ELEM));
    assert!(effects.iter().any(|e| matches!(&e.fx,
        Fx::App(AppFx::Content(ContentReq::Push(ContentArg::Collection(id))))
            if id.rk.is_empty() && id.sec == 1 && id.tag == 812 && id.name == "Example Trilogy")),
        "the page opens by section and tag; the collection store resolves the rating key");
    let member = collection::elem(1).unwrap();
    let (_, effects) = step(&mut screen, &ScreenEvent::Activate(member), Some(member));
    assert!(effects.iter().any(|e| matches!(&e.fx,
        Fx::App(AppFx::Content(ContentReq::Push(ContentArg::Detail { rk, .. }))) if rk == "m2")));
    clear();
}

/// A collection bigger than the shelf's preview is reached through its heading: the shelf holds the
/// preview, the heading says the whole count, and OK on it opens the Collection page (which pages).
#[test]
fn a_collection_past_the_preview_is_reached_through_its_heading() {
    let sid = ServerId::UNSET;
    let mut d = collection_movie(sid);
    let c = d.collection.as_mut().unwrap();
    c.members = (1..=plx_data::metadata::COLLECTION_PREVIEW).map(|i| collection_member(sid, &format!("m{i}"))).collect();
    c.count = 45;
    let _guard = install(d);
    let mut screen = bare(&_guard, sid, "m1");
    assert_eq!(collection::len(screen.detail(test_store().view()).unwrap()), plx_data::metadata::COLLECTION_PREVIEW);
    assert_eq!(screen.detail(test_store().view()).unwrap().collection.as_ref().unwrap().count, 45,
        "the heading counts the whole collection");
    let (_, effects) = step(&mut screen, &ScreenEvent::Activate(collection::HEADING_ELEM),
        Some(collection::HEADING_ELEM));
    assert!(effects.iter().any(|e| matches!(&e.fx,
        Fx::App(AppFx::Content(ContentReq::Push(ContentArg::Collection(id))))
            if id.sec == 1 && id.tag == 812)),
        "the heading opens the Collection page, which holds all 45");
    clear();
}

#[test]
fn the_heading_is_a_hover_focus_stop_that_wins_over_the_member_cards() {
    use plx_ui::hit::{HitMap, PointerKind};
    let sid = ServerId::UNSET;
    let _guard = install(collection_movie(sid));
    let mut screen = bare(&_guard, sid, "m1");
    let measure = plx_ui::fixture::FixtureMeasure;
    let top = {
        let d = screen.detail(test_store().view()).unwrap();
        screen.section_top(7, d, &measure) - plx_ui::detail_layout::TOP_MARGIN
    };
    screen.scroll.jump(top);
    screen.scroll_target = top;
    let heading = FocusKey { entry: EntryId(7), elem: collection::HEADING_ELEM };
    let context = cx(&measure, Some(heading.elem));
    // the shelf registers its cards as the page draws it; the page registers the rest after
    let stops = drawn_stops(&mut screen, &context);
    let at = stops.iter().position(|s| s.key == heading).expect("the heading registers a stop");
    assert_eq!(stops[at].hover, Hover::Focus);
    let last_member = screen.engine_key(collection::elem(3).unwrap()).unwrap();
    assert!(stops.iter().position(|s| s.key.elem == last_member).unwrap() < at,
        "registered after the cards, so it wins where its focused face overlaps them");
    let placed = Focusable::<TestHost>::place(&screen, &heading.elem, &context, At::Drawn).unwrap();
    assert_eq!((stops[at].rect.x, stops[at].rect.y, stops[at].rect.w, stops[at].rect.h),
        (placed.rect.x, placed.rect.y, placed.rect.w, placed.rect.h), "the hit rect is the drawn face");
    let mut map = HitMap::new();
    map.fill(stops);
    map.swap();
    let hit = map.resolve(Some(heading.entry), PointerKind::Click, placed.rect.cx(), placed.rect.cy(), None);
    assert_eq!(hit.hit, Some(heading));
    clear();
}

/// **The hero, season, episode and about stops never overlap a shelf card's.** The shelves register
/// their cards inside the section loop of `draw`, BEFORE `record_stops` adds the hero, seasons,
/// episodes, the collection heading and About, and a later stop wins an overlap. The order is only
/// immaterial if those groups never share a pixel; this draws a page with all four shelves and
/// every other group, at a sweep of scroll positions and with each shelf's first card focused
/// (popped), and holds every shelf-card stop apart from every other on-screen stop but the heading
/// (which is meant to win over the cards under it).
#[test]
fn no_shelf_card_stop_overlaps_the_hero_season_episode_or_about_stops() {
    let sid = ServerId::UNSET;
    let mut page = detail(sid, "show");
    page.episodes = (1..=6).map(|i| episode(&format!("e{i}"), i)).collect();
    page.related = (1..=12).map(|i| collection_member(sid, &format!("r{i}"))).collect();
    page.collection = Some(plx_data::metadata::CollectionShelf {
        title: "Example Trilogy".into(),
        section: 1,
        tag: 812,
        members: (1..=12).map(|i| collection_member(sid, &format!("m{i}"))).collect(),
        count: 12,
    });
    page.extras = (1..=12)
        .map(|i| plx_data::metadata::Extra { rk: format!("x{i}"), ..Default::default() })
        .collect();
    page.cast = (1..=12)
        .map(|i| plx_data::metadata::Cast {
            tag: format!("c{i}"),
            id: 100 + i,
            role: String::new(),
            thumb: String::new(),
            tag_key: String::new(),
        })
        .collect();
    let _guard = install(page);
    let mut screen = bare(&_guard, sid, "show");
    let measure = plx_ui::fixture::FixtureMeasure;
    let shelves: [Vec<u32>; 4] = [
        (0..12).filter_map(|i| related::elem(i)).collect(),
        (0..12).filter_map(|i| collection::elem(i)).collect(),
        (0..12).filter_map(|i| extras::elem(i)).collect(),
        (0..12).filter_map(|i| cast::elem(i)).collect(),
    ];
    let keyed: Vec<Vec<u32>> = shelves
        .iter()
        .map(|s| s.iter().map(|&e| screen.engine_key(e).expect("every card has a key")).collect())
        .collect();
    let is_card = |elem: u32| keyed.iter().any(|s| s.contains(&elem));
    let last = {
        let d = screen.detail(test_store().view()).unwrap();
        let (sections, n) = screen.sections(Some(d));
        screen.section_top(sections[n - 1], d, &measure)
    };
    let mut focuses: Vec<Option<u32>> = vec![None];
    focuses.extend(keyed.iter().map(|s| Some(s[0])));
    focuses.push(season::elem(0).and_then(|e| screen.engine_key(e)));
    focuses.push(Some(hero::ELEM_PLAY));
    let screen_rect = plx_ui::Rect::FULL;
    let (mut cards_seen, mut others_seen) = ([0usize; 4], 0usize);
    for step_at in 0..=((last / 120.0).ceil() as i32) {
        let scroll = (step_at as f32 * 120.0).min(last);
        screen.scroll.jump(scroll);
        screen.scroll_target = scroll;
        for focus in &focuses {
            let context = cx(&measure, *focus);
            let stops = drawn_stops(&mut screen, &context);
            let shown = |s: &Stop<u32>| s.rect.intersect(s.clip).intersect(screen_rect);
            let on_screen = |s: &&Stop<u32>| {
                let r = shown(s);
                r.w > 0.0 && r.h > 0.0
            };
            let cards: Vec<_> = stops.iter().filter(|s| is_card(s.key.elem)).filter(on_screen).collect();
            let others: Vec<_> = stops
                .iter()
                .filter(|s| !is_card(s.key.elem) && s.key.elem != collection::HEADING_ELEM)
                .filter(on_screen)
                .collect();
            for (i, shelf) in keyed.iter().enumerate() {
                cards_seen[i] += cards.iter().filter(|s| shelf.contains(&s.key.elem)).count();
            }
            others_seen += others.len();
            for c in &cards {
                for o in &others {
                    let (a, b) = (shown(c), shown(o));
                    assert!(
                        a.intersect(b).w <= 0.0 || a.intersect(b).h <= 0.0,
                        "scroll {scroll}, focus {focus:?}: shelf card {} {a:?} overlaps stop {} {b:?}",
                        c.key.elem, o.key.elem
                    );
                }
            }
        }
    }
    assert!(cards_seen.iter().all(|&n| n > 0), "every shelf must show cards somewhere in the sweep: {cards_seen:?}");
    assert!(others_seen > 0, "the sweep must show hero, season, episode or About stops");
    clear();
}

/// **A draw between a landing and its `StoreChanged` still shows the cards that have keys.** The
/// app pumps the Metadata store AFTER the frame's dispatcher steps and draws in the same loop turn
/// (`app::run`: `loop_requests` then `draw`), while the store's notice reaches the screen as
/// `StoreChanged` only in the NEXT frame's `frame_ingest`. For that one draw the published list
/// can be longer than the key projection; the shelf must keep the keyed prefix rather than vanish.
#[test]
fn a_draw_before_the_landing_is_synced_keeps_the_keyed_prefix_of_the_shelf() {
    let sid = ServerId::UNSET;
    let mut page = collection_movie(sid);
    page.collection = None;
    let mut grown = page.clone();
    page.related = ["r1", "r2"].iter().map(|rk| collection_member(sid, rk)).collect();
    grown.related = ["r1", "r2", "r3", "r4", "r5"].iter().map(|rk| collection_member(sid, rk)).collect();
    let _guard = install(page);
    let mut screen = bare(&_guard, sid, "m1");
    let measure = plx_ui::fixture::FixtureMeasure;
    let context = cx(&measure, None);
    let keys: Vec<u32> = (0..2).map(|i| screen.engine_key(related::elem(i).unwrap()).unwrap()).collect();
    let shown = |stops: &[Stop<u32>]| stops.iter().filter(|s| keys.contains(&s.key.elem)).count();
    assert_eq!(shown(&drawn_stops(&mut screen, &context)), 2, "the synced shelf shows its cards");

    // the landing publishes five cards; `StoreChanged` has not reached the screen yet
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(grown));
    assert_eq!(shown(&drawn_stops(&mut screen, &context)), 2,
        "the cards that have keys keep drawing while the rest wait for their keys");
    screen.sync_keys(test_store().view());
    let all: Vec<u32> = (0..5).map(|i| screen.engine_key(related::elem(i).unwrap()).unwrap()).collect();
    let stops = drawn_stops(&mut screen, &context);
    assert_eq!(stops.iter().filter(|s| all.contains(&s.key.elem)).count(), 5, "all five once synced");
    clear();
}

#[test]
fn member_keys_follow_the_member_and_the_heading_spot_restores_the_heading() {
    let sid = ServerId::UNSET;
    let _guard = install(collection_movie(sid));
    let mut screen = bare(&_guard, sid, "m1");
    let m3 = screen.engine_key(collection::elem(2).unwrap()).unwrap();
    let mut reordered = collection_movie(sid);
    reordered.collection.as_mut().unwrap().members.reverse();
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(reordered));
    screen.sync_keys(test_store().view());
    assert_eq!(screen.engine_key(collection::elem(1).unwrap()), Some(m3),
        "a refetch that reorders the collection keeps each member's key");

    let spot = screen.spot(Some(FocusKey { entry: EntryId(7), elem: collection::HEADING_ELEM }),
        SpotFacts::of(&screen, test_store().view()));
    assert_eq!((spot.section, spot.col), (7, -1));
    screen.restore(&spot, test_store().view());
    let measure = plx_ui::fixture::FixtureMeasure;
    let restored = Focusable::<TestHost>::reconcile(&screen,
        FocusKey { entry: EntryId(7), elem: hero::ELEM_PLAY }, &cx(&measure, None));
    assert_eq!(restored.elem, collection::HEADING_ELEM, "Back from the collection page lands on the heading");
    clear();
}

/// **Issue 18: a show's walk must cost O(episodes), not O(episodes^2).** Every show detail page
/// held the television at 27 fps where a movie held 60, with the GPU's work per frame identical:
/// the cost was CPU, paid inside every walk of the page, and the frame walks it up to three times
/// (backdrop discovery, each blur source, the visible pass). `record_stops` places two stops per
/// episode, each placement asks for the section flow, and each ask re-derived [`LayoutStamp`] —
/// which hashes EVERY episode's title, synopsis and air date. One walk of this 60-episode season
/// hashed thousands of episodes. A walk validates the flow once and reads it from then on; the
/// whole draw (a recording painter, so no GL) and a bare `record_stops` are both held to one pass.
#[test]
fn a_show_walk_derives_the_layout_identity_once_not_once_per_stop() {
    const EPISODES: usize = 60;
    let sid = ServerId::UNSET;
    let mut show = detail(sid, "show");
    show.episodes = (1..=EPISODES as i64)
        .map(|i| plx_data::metadata::Episode {
            summary: "A synopsis long enough to wrap onto every line the strip allows. ".repeat(4),
            aired: "2024-03-14".into(),
            ..episode(&format!("e{i}"), i)
        })
        .collect();
    let _guard = install(show);
    let mut screen = bare(&_guard, sid, "show");
    let measure = plx_ui::fixture::FixtureMeasure;
    let context = cx(&measure, None);
    let stamped = |walk: &mut dyn FnMut()| {
        STAMPED_EPISODES.with(|n| n.set(0));
        walk();
        STAMPED_EPISODES.with(|n| n.get())
    };

    let bare_walk = stamped(&mut || {
        let mut f = DrawFrame::new(&context, plx_ui::Painter::root());
        screen.record_stops(&mut f);
        assert!(f.stops().len() >= 4, "the fixture must register a stop per visible episode row");
    });
    assert!(bare_walk <= EPISODES, "record_stops hashed {bare_walk} episodes for a {EPISODES}-episode season");

    let full_walk = stamped(&mut || {
        let mut f = DrawFrame::new(&context, plx_ui::Painter::recording());
        plx_gfx::gfx::without_frame_clear(|| Screen::<TestHost>::draw(&mut screen, &mut f));
    });
    assert!(full_walk <= EPISODES, "one draw hashed {full_walk} episodes for a {EPISODES}-episode season");
    clear();
}

/// **A walk that feeds no hit map places no stop.** The frame walks this page up to four times —
/// the text prewarm, backdrop discovery, each blur source, the visible pass — and only the visible
/// one's stops reach `Input`'s hit map. `record_stops` places two stops per episode, each through
/// the section flow, so every other walk paid a whole placement pass for a list the dispatcher
/// dropped. Pinned on a show, where that pass is largest: outside the visible walk it must derive
/// no layout identity and register nothing.
#[test]
fn record_stops_places_nothing_outside_the_visible_walk() {
    use plx_ui::frame::backdrop::{self, Z};
    const EPISODES: usize = 24;
    let sid = ServerId::UNSET;
    let mut show = detail(sid, "show");
    show.episodes = (1..=EPISODES as i64).map(|i| episode(&format!("e{i}"), i)).collect();
    let _guard = install(show);
    let screen = bare(&_guard, sid, "show");
    let measure = plx_ui::fixture::FixtureMeasure;
    let context = cx(&measure, None);
    let walk = |painter: plx_ui::Painter| {
        STAMPED_EPISODES.with(|n| n.set(0));
        let mut f = DrawFrame::new(&context, painter);
        screen.record_stops(&mut f);
        (f.stops().len(), STAMPED_EPISODES.with(|n| n.get()))
    };
    let (visible, _) = walk(plx_ui::Painter::root());
    assert!(visible >= 4, "the visible walk registers the episode rows on the axis ({visible})");
    let sources = std::rc::Rc::new(std::cell::RefCell::new(backdrop::Sources::default()));
    {
        let _discovery = backdrop::discover(sources.clone());
        assert_eq!(walk(plx_ui::Painter::root()), (0, 0), "backdrop discovery placed stops");
    }
    {
        let _source = backdrop::enter(sources, Z::OPENER);
        assert_eq!(walk(plx_ui::Painter::root()), (0, 0), "a blur source walk placed stops");
    }
    assert_eq!(walk(plx_ui::Painter::recording()), (0, 0), "the text prewarm walk placed stops");
    clear();
}

/// The hold hint (`ui::hold_hint`) on a Detail card shelf whose held OK opens an item menu
/// (Related, Collection): it stands after the dwell on a settled card, not on the hero, not while
/// a menu owns focus, and not a second time this run.
#[test]
fn the_hold_hint_stands_on_a_resting_related_card_once_per_install() {
    let sid = ServerId::UNSET;
    let _guard = install(collection_movie(sid));
    plx_ui::hold_hint::reset_learned_for_test();
    plx_ui::hold_hint::reset_shown_for_test();
    let mut screen = bare(&_guard, sid, "m1");
    screen.sync_keys(test_store().view());
    let card = screen.engine_key(related::elem(1).unwrap()).unwrap();
    let measure = plx_ui::fixture::FixtureMeasure;
    let mut ms = 100;
    let mut rest = |screen: &mut DetailScreen, focus: Option<FocusKey<u32>>, secs: f32| {
        for _ in 0..(secs * 60.0) as u32 {
            ms += 17;
            let mut context = cx(&measure, None);
            context.focus.current = focus;
            context.tick = plx_machine::machine::Tick { ms, dt_us: 16_667 };
            let mut effects = Vec::new();
            let mut present = plx_machine::present::Present::new();
            let mut sink = Effects::new(&mut effects,
                plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(1)), &mut present);
            Machine::<TestHost>::step(screen, &ScreenEvent::Tick(context.tick), &context, &mut sink);
        }
    };
    let on = |elem| Some(FocusKey { entry: EntryId(7), elem });
    rest(&mut screen, on(card), 1.2);
    assert!(!screen.hold_hint.visible(), "not before the dwell");
    // negatives first: a stand spends the run's latch and would make them vacuous
    rest(&mut screen, Some(FocusKey { entry: EntryId(900), elem: 0 }), 4.0);
    assert!(!screen.hold_hint.visible() && !plx_ui::hold_hint::shown_this_run(), "never while a menu owns focus");
    rest(&mut screen, on(hero::ELEM_PLAY), 4.0);
    assert!(!screen.hold_hint.visible() && !plx_ui::hold_hint::shown_this_run(), "never on a hero control");
    rest(&mut screen, on(card), 3.6);
    assert!(screen.hold_hint.visible(), "after the dwell, on a settled Related card");
    rest(&mut screen, Some(FocusKey { entry: EntryId(900), elem: 0 }), 1.0);
    assert!(!screen.hold_hint.visible(), "a menu taking focus hides it");
    rest(&mut screen, on(card), 4.0);
    assert!(!screen.hold_hint.visible(), "and not a second time this run");
    clear();
}

/// The hold hint on a season tab (`hold_hint::Surface::SeasonPill`): a held OK there opens the
/// season's item menu, so the pill teaches it, after the shorter pill dwell (well before the card
/// dwell would have), once per install on its own latch (the Card latch stays unspent).
#[test]
fn the_hold_hint_stands_on_a_resting_season_pill_after_its_shorter_dwell() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    plx_ui::hold_hint::reset_learned_for_test();
    plx_ui::hold_hint::reset_shown_for_test();
    let mut screen = bare(&_guard, sid, "show");
    screen.sync_keys(test_store().view());
    let measure = plx_ui::fixture::FixtureMeasure;
    let mut ms = 100;
    let mut rest = |screen: &mut DetailScreen, focus: Option<FocusKey<u32>>, secs: f32| {
        for _ in 0..(secs * 60.0) as u32 {
            ms += 17;
            let mut context = cx(&measure, None);
            context.focus.current = focus;
            context.tick = plx_machine::machine::Tick { ms, dt_us: 16_667 };
            let mut effects = Vec::new();
            let mut present = plx_machine::present::Present::new();
            let mut sink = Effects::new(&mut effects,
                plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(1)), &mut present);
            Machine::<TestHost>::step(screen, &ScreenEvent::Tick(context.tick), &context, &mut sink);
        }
    };
    let tab = screen.engine_key(season::elem(0).unwrap()).unwrap();
    let pill = Some(FocusKey { entry: EntryId(7), elem: tab });
    rest(&mut screen, pill, 0.3);
    assert!(!screen.hold_hint.visible(), "not before the pill dwell");
    rest(&mut screen, Some(FocusKey { entry: EntryId(900), elem: 0 }), 3.0);
    assert!(!screen.hold_hint.visible() && !plx_ui::hold_hint::shown_this_run(), "never while a menu owns focus");
    rest(&mut screen, pill, 1.4);
    assert!(screen.hold_hint.visible(), "1.4 s on the pill is past its dwell and the tab capsule's arrival, short of the card's");
    rest(&mut screen, pill, 2.0);
    assert!(plx_ui::hold_hint::shown(plx_ui::hold_hint::Surface::SeasonPill), "a read showing taught the pill");
    assert!(!plx_ui::hold_hint::shown(plx_ui::hold_hint::Surface::Card), "and left the card hint unspent");
    clear();
}

/// The dwell counts from FOCUS LANDING on the pill: the page scroll still gliding, the tab strip's
/// capsule still travelling and the season switch the focus move queued are motion that only gates
/// standing, they do not restart the clock. Pre-fix this took the motion's whole settle plus a
/// second dwell (about 1.5 s on the TV).
#[test]
fn the_pill_dwell_counts_from_focus_landing_not_from_the_motion_ending() {
    let sid = ServerId::UNSET;
    let _guard = install(detail(sid, "show"));
    plx_ui::hold_hint::reset_learned_for_test();
    plx_ui::hold_hint::reset_shown_for_test();
    let mut screen = bare(&_guard, sid, "show");
    screen.sync_keys(test_store().view());
    let measure = plx_ui::fixture::FixtureMeasure;
    let mut ms = 100;
    let tab = screen.engine_key(season::elem(0).unwrap()).unwrap();
    let pill = Some(FocusKey { entry: EntryId(7), elem: tab });
    // the page is mid-glide, the tab strip is travelling and a season switch is queued
    screen.scroll_target = screen.scroll.pos + 300.0;
    screen.tab_scroll.vel = 400.0;
    screen.pending_season = Some(0);
    for frame in 0..60u32 {
        ms += 17;
        let mut context = cx(&measure, None);
        context.focus.current = pill;
        context.tick = plx_machine::machine::Tick { ms, dt_us: 16_667 };
        let mut effects = Vec::new();
        let mut present = plx_machine::present::Present::new();
        let mut sink = Effects::new(&mut effects,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(1)), &mut present);
        Machine::<TestHost>::step(&mut screen, &ScreenEvent::Tick(context.tick), &context, &mut sink);
        if frame == 20 {
            assert!(!screen.hold_hint.visible(), "not before the pill dwell (0.34 s in)");
        }
    }
    // 60 frames at 17 ms is 1.02 s: dwell 0.5 s plus the glide and the fade, never a second dwell
    assert!(screen.hold_hint.visible(), "stands about one dwell after focus landed, motion notwithstanding");
    clear();
}

/// A held OK on a season pill opens a menu only through `focused_season`, which answers `None`
/// while the season has no `rk` (or a season fetch is in flight). The hint must not promise a hold
/// that does nothing, and a read of it there must not mark the pill taught for good.
#[test]
fn the_hold_hint_waits_for_a_season_whose_hold_opens_a_menu() {
    let sid = ServerId::UNSET;
    let mut unresolved = detail(sid, "show");
    unresolved.seasons[0].rk.clear();
    let _guard = install(unresolved);
    plx_ui::hold_hint::reset_learned_for_test();
    plx_ui::hold_hint::reset_shown_for_test();
    let mut screen = bare(&_guard, sid, "show");
    screen.sync_keys(test_store().view());
    let measure = plx_ui::fixture::FixtureMeasure;
    let mut ms = 100;
    let mut rest = |screen: &mut DetailScreen, focus: Option<FocusKey<u32>>, secs: f32| {
        for _ in 0..(secs * 60.0) as u32 {
            ms += 17;
            let mut context = cx(&measure, None);
            context.focus.current = focus;
            context.tick = plx_machine::machine::Tick { ms, dt_us: 16_667 };
            let mut effects = Vec::new();
            let mut present = plx_machine::present::Present::new();
            let mut sink = Effects::new(&mut effects,
                plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(1)), &mut present);
            Machine::<TestHost>::step(screen, &ScreenEvent::Tick(context.tick), &context, &mut sink);
        }
    };
    let tab = screen.engine_key(season::elem(0).unwrap()).unwrap();
    let pill = Some(FocusKey { entry: EntryId(7), elem: tab });
    rest(&mut screen, pill, 4.0);
    assert!(!screen.hold_hint.visible(), "a season with no rk has no menu to hold for");
    assert!(!plx_ui::hold_hint::shown(plx_ui::hold_hint::Surface::SeasonPill), "so reading nothing taught nothing");
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(detail(sid, "show")));
    screen.sync_keys(test_store().view());
    let tab = screen.engine_key(season::elem(0).unwrap()).unwrap();
    let pill = Some(FocusKey { entry: EntryId(7), elem: tab });
    rest(&mut screen, pill, 1.4);
    assert!(screen.hold_hint.visible(), "once the season resolves, the hint stands after its dwell");
    clear();
}

/// Every season switch lands a different set of episode identities. The key table keeps the
/// current pass, the previous one, the focus and the remembered keys, and above twice one pass plus
/// 256 drops the oldest rest, so a hundred switches stay bounded. The focused card's key never
/// changes, and a season that left and came back inside the bound gets the same keys.
#[test]
fn the_key_table_stays_bounded_over_a_hundred_season_switches_and_focus_never_moves() {
    let sid = ServerId::UNSET;
    let season_eps = |n: usize| -> Vec<_> {
        (0..40).map(|i| episode(&format!("s{n}e{i}"), i as i64 + 1)).collect()
    };
    let mut d = detail(sid, "show");
    d.related = vec![Default::default(), Default::default(), Default::default()];
    d.episodes = season_eps(0).into();
    let _guard = install(d.clone());
    let mut screen = bare(&_guard, sid, "show");
    screen.sync_keys(test_store().view());
    let focus = screen.engine_key(related::elem(1).unwrap()).unwrap();
    let ep_key = |screen: &DetailScreen, n: usize| {
        screen.keys.iter().find(|k| matches!(&k.identity,
            DetailIdentity::Episode { rk, text: false, .. } if *rk == format!("s{n}e5"))).map(|k| k.elem)
    };
    let first = ep_key(&screen, 0).expect("season 0 interned");
    let mut widest = 0;
    for n in 1..=100 {
        d.episodes = season_eps(n).into();
        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(d.clone()));
        screen.sync_keys(test_store().view());
        screen.prune_keys(&[focus]);
        let pass = screen.local_by_key.len();
        assert!(screen.keys.len() <= 2 * pass + 256, "switch {n}: {} keys for a pass of {pass}", screen.keys.len());
        assert_eq!(screen.engine_key(related::elem(1).unwrap()), Some(focus), "switch {n} moved the focused card's key");
        assert!(screen.keys.iter().any(|k| k.elem == focus), "the focus key is never pruned");
        widest = widest.max(screen.keys.len());
    }
    assert!(widest < 500, "the table grew with the switches: {widest}");
    // Out past the bound, season 0's keys are gone; coming back mints new ones, never reusing a live one.
    assert!(ep_key(&screen, 0).is_none());
    assert_ne!(first, 0);

    // Inside the bound the same season returns to the same key.
    d.episodes = season_eps(101).into();
    plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(d.clone()));
    screen.sync_keys(test_store().view());
    screen.prune_keys(&[focus]);
    let a = ep_key(&screen, 101).unwrap();
    for n in [102, 101] {
        d.episodes = season_eps(n).into();
        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(d.clone()));
        screen.sync_keys(test_store().view());
        screen.prune_keys(&[focus]);
    }
    assert_eq!(ep_key(&screen, 101), Some(a), "a season that left and returned within the bound keeps its key");
    clear();
}

/// An entry far below the top keeps one identity: the focused card's. The page that reloads on
/// return interns the same card under the same key, so focus seats on it.
#[test]
fn a_shed_detail_memory_holds_one_identity_and_the_reloaded_page_seats_the_same_card() {
    use crate::registry::{DetailMemory, PageMemory};
    let sid = ServerId::UNSET;
    let mut d = detail(sid, "show");
    d.related = (0..5).map(|i| plx_data::metadata::Related { rk: format!("rel-{i}"), ..Default::default() }).collect();
    let _guard = install(d);
    for n in 0..40usize {
        let mut screen = bare(&_guard, sid, "show");
        screen.sync_keys(test_store().view());
        let focus = screen.engine_key(related::elem(n % 5).unwrap()).unwrap();
        let mut memory = PageMemory::Detail(DetailMemory {
            spot: Default::default(), keys: screen.keys.clone(), next_elem: screen.next_elem,
        });
        assert!(matches!(&memory, PageMemory::Detail(m) if m.keys.len() > 1));
        memory.shed_to_identity(Some(focus));
        let PageMemory::Detail(shed) = &memory else { unreachable!() };
        assert_eq!(shed.keys.len(), 1, "route and one identity only");
        assert_eq!(shed.keys[0].elem, focus);
        let mut returned = bare(&_guard, sid, "show");
        returned.restore_memory(shed, test_store().view());
        assert_eq!(returned.engine_key(related::elem(n % 5).unwrap()), Some(focus), "the same card, the same key");
        assert!(returned.locate(focus, test_store().view()) == Some(Located::Related(n % 5)));
    }
    clear();
}

/// A season of `total` episodes of which only the listed 60-episode pages have landed.
fn long_season(total: usize, pages: &[usize]) -> Detail {
    let mut d = detail(ServerId::UNSET, "show");
    d.episodes = plx_data::metadata::EpisodeList::with_total(total);
    for &p in pages {
        d.episodes.set_page(p, episode_rows(total, p));
    }
    d
}

fn episode_rows(total: usize, page: usize) -> Vec<plx_data::metadata::Episode> {
    (page * 60..((page + 1) * 60).min(total)).map(|i| episode(&format!("e{i}"), i as i64 + 1)).collect()
}

fn land_page(total: usize, page: usize) {
    plx_data::metadata::edit_current_for_test(test_store().state_mut(), |d| d.episodes.set_page(page, episode_rows(total, page)));
}

fn episode_key(screen: &DetailScreen, i: usize, row: episodes::Row) -> Option<u32> {
    screen.engine_key(episodes::elem(i, row)?)
}

/// Episode 257 of a long season used to be unreachable (`MAX_ITEMS` 256). With the pages landing as
/// the walk reaches them, Right from the first episode reaches the 1,000th, and the focused cell's
/// identity changes only by the key.
#[test]
fn a_thousand_episode_season_is_walked_to_its_last_episode_with_right() {
    let total = 1000;
    let _guard = install(long_season(total, &[0]));
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    screen.sync_keys(test_store().view());
    let measure = plx_ui::fixture::FixtureMeasure;
    let mut at = episode_key(&screen, 0, episodes::Row::Still).expect("page 0 is keyed");
    let mut reached = 0;
    while reached + 1 < total {
        let context = cx(&measure, Some(at));
        match Focusable::<TestHost>::neighbour(&screen, FocusKey { entry: EntryId(7), elem: at }, Dir::Right, &context) {
            Step::Move(next) => {
                reached += 1;
                assert!(screen.locate(next.elem, test_store().view()) == Some(Located::Episode(reached, episodes::Row::Still)));
                at = next.elem;
            }
            Step::Edge => {
                // a hole: the press stops, the page lands, the same press goes on
                assert!(test_store().view().current().unwrap().episodes.get(reached + 1).is_none(), "Right stopped at {reached} with the next episode loaded");
                land_page(total, (reached + 1) / 60);
                screen.sync_keys(test_store().view());
                assert_eq!(episode_key(&screen, reached, episodes::Row::Still), Some(at), "landing did not rekey the focused episode");
            }
        }
    }
    assert!(screen.locate(at, test_store().view()) == Some(Located::Episode(999, episodes::Row::Still)));
    clear();
}

/// The strip keys, lays out and draws the cells on the axis only, however long the season.
#[test]
fn a_long_season_registers_stops_for_the_visible_cells_only() {
    let all: Vec<usize> = (0..17).collect();
    let _guard = install(long_season(1000, &all));
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    screen.sync_keys(test_store().view());
    let measure = plx_ui::fixture::FixtureMeasure;
    let top = {
        let d = screen.detail(test_store().view()).unwrap();
        screen.section_top(2, d, &measure)
    };
    screen.scroll.jump(top);
    screen.scroll_target = top;
    screen.episode_scroll.jump(episodes::strip_x(700) - plx_ui::consts::MARGIN_X);
    let stops = drawn_stops(&mut screen, &cx(&measure, None));
    let eps: Vec<usize> = stops
        .iter()
        .filter_map(|s| match screen.locate(s.key.elem, test_store().view()) {
            Some(Located::Episode(i, _)) => Some(i),
            _ => None,
        })
        .collect();
    assert!(!eps.is_empty() && eps.iter().all(|i| (696..709).contains(i)), "only the cells on the axis: {eps:?}");
    clear();
}

/// A hole in view draws the still's skeleton; the page landing fills it and the focused cell does
/// not move.
#[test]
fn a_hole_in_view_draws_a_skeleton_and_fills_on_landing_without_moving_the_focus() {
    let _guard = install(long_season(200, &[0]));
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    screen.sync_keys(test_store().view());
    let measure = plx_ui::fixture::FixtureMeasure;
    let focus = episode_key(&screen, 59, episodes::Row::Still).unwrap();
    let top = {
        let d = screen.detail(test_store().view()).unwrap();
        screen.section_top(2, d, &measure)
    };
    screen.scroll.jump(top);
    screen.scroll_target = top;
    // Scrolled so every cell on the axis is past the landed page: drawing a loaded cell needs the
    // GL text path the host does not have, a hole's skeleton does not.
    screen.episode_scroll.jump(episodes::strip_x(75) - plx_ui::consts::MARGIN_X);
    let context = cx(&measure, Some(focus));
    let skeletons = |screen: &mut DetailScreen| {
        let (_, frame) = plx_ui::placeholder::capture_declared(|| {
            let meta = test_store().view();
            let d = screen.detail(meta).unwrap();
            episodes::draw(plx_ui::Painter::root(), d, 0.0, screen.episode_scroll.pos, None, |_| 1.0, |_| 1.0, |_| false, |_| Default::default(), &measure, meta);
        });
        frame.of(plx_ui::placeholder::Reason::CardSkeleton)
    };
    let before_rect = Focusable::<TestHost>::place(&screen, &focus, &context, At::SpringTarget).unwrap().rect;
    assert!(skeletons(&mut screen) >= 2, "the cells past the landed page are skeletons");
    land_page(200, 1);
    screen.sync_keys(test_store().view());
    {
        let d = screen.detail(test_store().view()).unwrap();
        assert!(episodes::visible(screen.episode_scroll.pos, d.episodes.len(), 0).all(|i| d.episodes.get(i).is_some()), "the landing fills the holes in view");
    }
    assert_eq!(episode_key(&screen, 59, episodes::Row::Still), Some(focus), "the focused identity is unchanged");
    let after_rect = Focusable::<TestHost>::place(&screen, &focus, &context, At::SpringTarget).unwrap().rect;
    assert_eq!(before_rect, after_rect, "the focused cell did not move");
    clear();
}

/// Right and Left stop at a hole rather than seat on a cell that is not there.
#[test]
fn left_and_right_stop_at_a_hole() {
    let _guard = install(long_season(300, &[0, 2]));
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    screen.sync_keys(test_store().view());
    let measure = plx_ui::fixture::FixtureMeasure;
    let context = cx(&measure, None);
    let step_from = |i: usize, dir: Dir| {
        let at = episode_key(&screen, i, episodes::Row::Still).unwrap();
        Focusable::<TestHost>::neighbour(&screen, FocusKey { entry: EntryId(7), elem: at }, dir, &context)
    };
    assert!(matches!(step_from(59, Dir::Right), Step::Edge), "page 1 is a hole");
    assert!(matches!(step_from(120, Dir::Left), Step::Edge), "page 1 is a hole");
    assert!(matches!(step_from(58, Dir::Right), Step::Move(_)));
    clear();
}

/// A restore onto episode 700 seats it once its page is in, and waits for the page rather than
/// seating on another episode.
#[test]
fn a_restore_onto_episode_700_waits_for_its_page_and_then_seats_it() {
    let _guard = install(long_season(1000, &[0]));
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    screen.restore_intent = Some(RestoreIntent {
        spot: Spot { section: 2, col: 700, ..Default::default() },
        episode: Some("e700".into()),
        season_requested: true,
        seek: Default::default(),
    });
    screen.sync_keys(test_store().view());
    assert_eq!(screen.restore_focus(test_store().view()), None, "episode 700's page has not landed");
    land_page(1000, 11);
    screen.sync_keys(test_store().view());
    assert_eq!(screen.restore_focus(test_store().view()), episode_key(&screen, 700, episodes::Row::Still));
    clear();
}

/// The strip tells the store which episodes it needs, and says it again only when that changes (or
/// once a second while a page in it is still a hole), so a long season fetches as the strip moves.
#[test]
fn a_long_season_asks_for_the_pages_the_strip_reaches_only_when_the_range_changes() {
    let _guard = install(long_season(1000, &[0]));
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    screen.sync_keys(test_store().view());
    let measure = plx_ui::fixture::FixtureMeasure;
    let context = cx(&measure, None);
    let mut present = plx_machine::present::Present::new();
    let mut effects = Vec::new();
    let mut tick = |screen: &mut DetailScreen, ms: u32, effects: &mut Vec<plx_machine::machine::Stamped<TestHost>>| {
        let before = effects.len();
        let mut sink = Effects::new(
            effects,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(1)),
            &mut present,
        );
        Machine::<TestHost>::step(screen, &ScreenEvent::Tick(plx_machine::machine::Tick { ms, dt_us: 16_667 }), &context, &mut sink);
        effects.len() - before
    };
    let wants = |effects: &[plx_machine::machine::Stamped<TestHost>]| {
        effects
            .iter()
            .filter(|s| matches!(&s.fx, Fx::App(AppFx::Store(StoreId::Metadata, StoreCmd::Metadata(MetadataCmd::WantEpisodes { .. })))))
            .count()
    };
    tick(&mut screen, 0, &mut effects);
    assert_eq!(wants(&effects), 1, "the first tick says what the strip needs");
    tick(&mut screen, 16, &mut effects);
    assert_eq!(wants(&effects), 1, "the same range is not said again at once");
    screen.episode_scroll.jump(episodes::strip_x(500) - plx_ui::consts::MARGIN_X);
    tick(&mut screen, 32, &mut effects);
    assert_eq!(wants(&effects), 2, "a strip somewhere else is a new range");
    tick(&mut screen, 2_000, &mut effects);
    assert_eq!(wants(&effects), 3, "a page still missing a second later is asked for again");
    clear();
}

// ---- where the strips open: the season and the episode the viewer left off at ------------------

/// A show with `season_count` seasons, season `cur` selected and `total` episodes of it listed
/// (only the first page when `total` exceeds one), whose on-deck episode is the one named `deck`
/// (its rating key) with the episode NUMBER `deck_index`, or no on-deck at all.
fn deck_show(season_count: i64, cur: usize, total: usize, deck: Option<(&str, i64)>) -> Detail {
    let mut d = detail(ServerId::UNSET, "show");
    d.seasons = (1..=season_count).map(season).collect();
    d.cur_season = cur;
    d.episodes = plx_data::metadata::EpisodeList::with_total(total);
    d.episodes.set_page(0, episode_rows(total, 0));
    d.on_deck = deck.map(|(rk, index)| plx_data::metadata::Episode {
        rk: rk.into(),
        index,
        season: cur as i64 + 1,
        ..episode(rk, index)
    });
    d
}

/// One real `Tick` of `screen` with `focus` (a LOCAL element) held, and the effects it pushed.
fn tick_page(screen: &mut DetailScreen, focus: Option<u32>, ms: u32) -> Vec<plx_machine::machine::Stamped<TestHost>> {
    screen.sync_keys(test_store().view());
    let measure = plx_ui::fixture::FixtureMeasure;
    let context = cx(&measure, focus.and_then(|local| screen.engine_key(local)));
    let mut present = plx_machine::present::Present::new();
    let mut effects = Vec::new();
    let mut sink = Effects::new(
        &mut effects,
        plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(1)),
        &mut present,
    );
    Machine::<TestHost>::step(screen, &ScreenEvent::Tick(plx_machine::machine::Tick { ms, dt_us: 16_667 }), &context, &mut sink);
    effects
}

fn wanted_episodes(effects: &[plx_machine::machine::Stamped<TestHost>]) -> Vec<(usize, usize)> {
    effects
        .iter()
        .filter_map(|s| match &s.fx {
            Fx::App(AppFx::Store(StoreId::Metadata, StoreCmd::Metadata(MetadataCmd::WantEpisodes { lo, hi, .. }))) => Some((*lo, *hi)),
            _ => None,
        })
        .collect()
}

/// Opening a show the viewer is part way through leaves the page where it always opened, on the
/// hero, and only the two strips sit where they left off: season 12 of 20 is on its tab strip and
/// episode 14 of the season is in the filmstrip.
#[test]
fn a_started_show_opens_with_both_strips_scrolled_to_where_the_viewer_left_off() {
    let _guard = install(deck_show(20, 11, 30, Some(("e13", 14))));
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    for ms in [0, 16, 32] {
        tick_page(&mut screen, Some(hero::ELEM_PLAY), ms);
    }
    let len = 30;
    assert!(
        episodes::visible(screen.episode_scroll.pos, len, 0).contains(&13),
        "episode 14 is in the filmstrip's window (scroll {})",
        screen.episode_scroll.pos
    );
    assert!(screen.episode_scroll.pos > 0.0);
    assert_eq!(screen.episode_scroll.pos, episodes::seat_scroll(13, len), "and it stands at the leading edge");
    assert!(screen.tab_scroll.pos > 0.0, "twenty season tabs do not fit one screen");
    assert!(
        screen.season_metrics.visible(screen.tab_scroll.pos).contains(&11),
        "season 12's tab is on screen (tab scroll {})",
        screen.tab_scroll.pos
    );
    assert_eq!(screen.scroll_target, 0.0, "the page itself stays at the top, on the hero");
    assert_eq!(screen.scroll.pos, 0.0);
    assert!(screen.restore_intent.is_none() && !screen.return_pending, "nothing was queued to move focus");
    clear();
}

/// A show nobody has started — or finished, or whose server sent no on-deck — opens as it did:
/// season one, both strips at zero.
#[test]
fn a_show_without_an_on_deck_episode_opens_with_both_strips_at_zero() {
    let _guard = install(deck_show(20, 0, 30, None));
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    for ms in [0, 16, 32] {
        tick_page(&mut screen, Some(hero::ELEM_PLAY), ms);
    }
    assert_eq!((screen.episode_scroll.pos, screen.tab_scroll.pos), (0.0, 0.0));
    assert_eq!(test_store().view().current().map(|d| d.cur_season), Some(0));
    clear();
}

/// The on-deck episode of ANOTHER season is not this season's: a season the viewer is not on
/// opens at its first episode.
#[test]
fn an_on_deck_episode_in_another_season_does_not_scroll_this_seasons_strip() {
    let mut show = deck_show(4, 1, 30, Some(("e13", 14)));
    show.on_deck.as_mut().unwrap().season = 4;
    let _guard = install(show);
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    screen.episode_scroll.jump(900.0);
    tick_page(&mut screen, Some(hero::ELEM_PLAY), 0);
    assert_eq!(screen.episode_scroll.pos, 0.0);
    clear();
}

/// A user's tab switch lands a new season's list. Before the page seated its strips, the filmstrip
/// kept the PREVIOUS season's scroll offset across the switch (nothing but focus ever moved it),
/// so season 2 opened part way along. Now a season other than the on-deck one starts at 0, and the
/// on-deck season starts at the on-deck episode.
#[test]
fn switching_season_seats_the_filmstrip_at_the_new_seasons_start() {
    let mut show = deck_show(4, 0, 30, Some(("s3e14", 14)));
    show.on_deck.as_mut().unwrap().season = 3;
    let _guard = install(show);
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    tick_page(&mut screen, Some(hero::ELEM_PLAY), 0);
    screen.episode_scroll.jump(3_000.0); // the viewer scrolled season 1's strip
    let tab = season::elem(1).unwrap();
    plx_data::metadata::edit_current_for_test(test_store().state_mut(), |d| {
        d.cur_season = 1;
        d.episodes.set_page(0, episode_rows(30, 0));
    });
    tick_page(&mut screen, Some(tab), 16);
    assert_eq!(screen.episode_scroll.pos, 0.0, "season 2 starts at its first episode");

    // …and the on-deck season, switched to, starts at the on-deck episode.
    plx_data::metadata::edit_current_for_test(test_store().state_mut(), |d| {
        d.cur_season = 2;
        d.episodes = plx_data::metadata::EpisodeList::with_total(30);
        d.episodes.set_page(0, (0..30).map(|i| episode(&format!("s3e{}", i + 1), i as i64 + 1)).collect());
    });
    tick_page(&mut screen, Some(season::elem(2).unwrap()), 32);
    assert_eq!(screen.episode_scroll.pos, episodes::seat_scroll(13, 30));
    clear();
}

/// Neither a return (BACK, the player) nor a restore onto the filmstrip is seated over: both put
/// the strip where the viewer left it by their own rule.
#[test]
fn a_return_and_a_filmstrip_restore_are_not_seated_over() {
    let _guard = install(deck_show(4, 1, 30, Some(("e13", 14))));
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    screen.return_pending = true;
    screen.restore_intent = Some(RestoreIntent {
        spot: Spot { section: 0, ..Spot::default() },
        episode: None,
        season_requested: false,
        seek: Default::default(),
    });
    screen.episode_scroll.jump(123.0);
    tick_page(&mut screen, None, 0);
    assert_eq!(screen.episode_scroll.pos, 123.0, "a return in flight owns the strip");
    // Once the return is over, a page that was never seated (a fresh body) opens on the on-deck episode.
    screen.return_pending = false;
    screen.restore_intent = None;
    tick_page(&mut screen, Some(hero::ELEM_PLAY), 16);
    assert_eq!(screen.episode_scroll.pos, episodes::seat_scroll(13, 30));

    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    screen.restore_intent = Some(RestoreIntent {
        spot: Spot { section: 2, col: 3, season: Some(2), ..Spot::default() },
        episode: Some("e3".into()),
        season_requested: false,
        seek: Default::default(),
    });
    screen.episode_scroll.jump(123.0);
    tick_page(&mut screen, None, 0);
    assert_eq!(screen.episode_scroll.pos, 123.0, "a restore onto the filmstrip seats its own episode");
    clear();
}

/// A restore onto the HERO that names a season (`open_detail(.., Some(season), ..)`) is not a
/// filmstrip restore: the on-deck seat still applies, and the season restore then moves the tab.
#[test]
fn a_hero_restore_that_names_a_season_still_seats_the_filmstrip() {
    let _guard = install(deck_show(4, 1, 30, Some(("e13", 14))));
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    screen.restore_intent = Some(RestoreIntent {
        spot: Spot { section: 0, season: Some(2), ..Spot::default() },
        episode: None,
        season_requested: false,
        seek: Default::default(),
    });
    tick_page(&mut screen, Some(hero::ELEM_PLAY), 0);
    assert_eq!(screen.episode_scroll.pos, episodes::seat_scroll(13, 30));
    clear();
}

/// Once the viewer has put focus on the tabs or the filmstrip the page never moves them again,
/// however the data under them changes.
#[test]
fn a_viewer_who_took_the_filmstrip_is_not_seated_over() {
    let _guard = install(deck_show(4, 1, 30, Some(("e13", 14))));
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    tick_page(&mut screen, Some(episodes::elem(0, episodes::Row::Still).unwrap()), 0);
    assert_eq!(screen.episode_scroll.pos, 0.0, "focus was on the strip before the page seated it");
    // a re-read lands a fresh list for the same season
    plx_data::metadata::edit_current_for_test(test_store().state_mut(), |d| {
        d.episodes = plx_data::metadata::EpisodeList::with_total(30);
        d.episodes.set_page(0, episode_rows(30, 0));
    });
    tick_page(&mut screen, Some(hero::ELEM_PLAY), 16);
    assert_eq!(screen.episode_scroll.pos, 0.0);
    clear();
}

/// A long season lands as its first page: the on-deck episode on a later page is seated at the
/// number the server gave it, the page holding it is asked for, and the seat is corrected to the
/// episode's real place when the page lands. A viewer who took the strip meanwhile, or a page that
/// never lands, ends the correction.
#[test]
fn a_long_seasons_on_deck_episode_is_seated_by_hint_then_corrected_when_its_page_lands() {
    // Episode number 100, but it is the 91st listed (e90): a season with a gap in its numbering.
    let total = 200;
    {
        let _guard = install(deck_show(2, 0, total, Some(("e90", 100))));
        let mut screen = bare(&_guard, ServerId::UNSET, "show");
        let first = tick_page(&mut screen, Some(hero::ELEM_PLAY), 0);
        assert_eq!(screen.episode_scroll.pos, episodes::seat_scroll(99, total), "seated by the hint");
        assert!(
            wanted_episodes(&first).iter().any(|&(lo, hi)| lo <= 99 && 99 < hi),
            "the page holding the hint is asked for: {:?}",
            wanted_episodes(&first)
        );
        land_page(total, 1);
        tick_page(&mut screen, Some(hero::ELEM_PLAY), 100);
        assert_eq!(screen.episode_scroll.pos, episodes::seat_scroll(90, total), "corrected to e90's place");
        land_page(total, 2);
        tick_page(&mut screen, Some(hero::ELEM_PLAY), 200);
        assert_eq!(screen.episode_scroll.pos, episodes::seat_scroll(90, total), "and final");
        clear();
    }

    {
        // The viewer takes the strip before the page lands: no correction under their hands.
        let _guard = install(deck_show(2, 0, total, Some(("e90", 100))));
        let mut screen = bare(&_guard, ServerId::UNSET, "show");
        tick_page(&mut screen, Some(hero::ELEM_PLAY), 0);
        // (a hole has no cell to hold focus, so the viewer's stop is the first, loaded episode)
        tick_page(&mut screen, Some(episodes::elem(0, episodes::Row::Still).unwrap()), 16);
        let held = screen.episode_scroll.pos;
        land_page(total, 1);
        tick_page(&mut screen, Some(hero::ELEM_PLAY), 32);
        assert_eq!(screen.episode_scroll.pos, held);
        clear();
    }

    {
        // A page that never lands is given up on: the hint stands, and a late landing leaves it be.
        let _guard = install(deck_show(2, 0, total, Some(("e90", 100))));
        let mut screen = bare(&_guard, ServerId::UNSET, "show");
        tick_page(&mut screen, Some(hero::ELEM_PLAY), 0);
        tick_page(&mut screen, Some(hero::ELEM_PLAY), SEAT_GIVE_UP_MS + 1);
        land_page(total, 1);
        tick_page(&mut screen, Some(hero::ELEM_PLAY), SEAT_GIVE_UP_MS + 17);
        assert_eq!(screen.episode_scroll.pos, episodes::seat_scroll(99, total));
        clear();
    }
}

// ---- A short Related head: the row that paints pending cells asks for them -------------------

/// A member movie whose `/related` previews were mostly its own collection: the members go to the
/// collection shelf (`plx_data::metadata::CollectionShelf`), so the Related head that is left is
/// ONE card, and the similar hub still has more behind its preview (`related_tail.more`).
fn short_related_head(sid: ServerId) -> Detail {
    let mut d = collection_movie(sid);
    d.related = vec![plx_data::pms::PmsMovie { thumb: "/t/r1".into(), ..collection_member(sid, "r1") }];
    d.related_tail = plx_data::metadata::RelatedTail {
        hubs: vec![plx_data::metadata::TailHub { key: "/library/metadata/1/similar".into(), preview: 8, len: None }],
        exclude: ["m1", "m2", "m3", "m4"].iter().map(|rk| rk.to_string()).collect(),
        head: 1,
        more: true,
        ..Default::default()
    };
    d
}

/// The cells the Related row paints past its last loaded card (`Shelf::paint_pending`): skeletons
/// that stand in for no card (an empty key); the loaded card's own skeleton names its thumb.
fn related_pending_cells(screen: &DetailScreen, measure: &plx_ui::fixture::FixtureMeasure) -> usize {
    let context = cx(measure, None);
    let meta = test_store().view();
    let d = screen.detail(meta).unwrap();
    let (_, frame) = plx_ui::placeholder::capture_declared(|| {
        let mut f = DrawFrame::new(&context, plx_ui::Painter::root());
        let p = f.painter;
        let at = SectionFrame::new(600.0, Rect::FULL);
        screen.related.draw(&mut f, p, &screen.cards(cards::Which::Related, d), at);
    });
    frame.entries.iter().filter(|e| e.reason == plx_ui::placeholder::Reason::CardSkeleton && e.key.is_empty()).count()
}

/// **Regression (the site-video frame dump's `card-skeleton:<empty>` on Tears of Steel).** A Related
/// row whose head is shorter than the screen and whose tail goes on paints the skeletons of the
/// cards past its edge whoever is looking (`CardSource::more_after`). The tail used to be asked for
/// only with focus near the row's end, so a row on screen that nobody had focused showed a
/// screenful of skeletons for as long as the page was up: a promise of cards that never came. The
/// row that paints pending cells on screen asks for them, focused or not.
#[test]
fn an_unfocused_related_row_that_paints_pending_cells_asks_for_the_tail() {
    let sid = ServerId::UNSET;
    let _guard = install(short_related_head(sid));
    let mut screen = bare(&_guard, sid, "m1");
    let measure = plx_ui::fixture::FixtureMeasure;
    assert!(related_pending_cells(&screen, &measure) > 0, "the row paints the cells past its one card");
    let mut asks = Vec::new();
    for n in 0..60 {
        let tick = ScreenEvent::Tick(plx_machine::machine::Tick { ms: 17 * n, dt_us: 16_667 });
        // focus stands on the hero's Play, not on the Related row
        let (_, effects) = step(&mut screen, &tick, Some(hero::ELEM_PLAY));
        asks.extend(effects.into_iter().filter_map(|e| match e.fx {
            Fx::App(AppFx::Store(StoreId::Metadata, StoreCmd::Metadata(MetadataCmd::WantRelated { before, seen }))) => Some((before, seen)),
            _ => None,
        }));
    }
    assert_eq!(asks.first(), Some(&(false, (0, 0))),
        "the unfocused row asks for the tail its skeletons stand in for: {asks:?}");
    // The tail lands (24 cards from position 0, more behind them) and takes the cells over.
    let mut window = short_related_head(sid);
    window.related.extend((0..24).map(|n| plx_data::pms::PmsMovie { thumb: format!("/t/t{n}"), ..collection_member(sid, &format!("t{n}")) }));
    window.related_tail = plx_data::metadata::RelatedTail { positions: (0..24).collect(), end: 24, total: 0, ..window.related_tail };
    let (state, adapter) = test_store().split_for_test();
    plx_data::metadata::land_related_for_test(adapter, (0, 0), &window);
    assert!(plx_data::metadata::pump_related_pages(state, adapter), "the window installs");
    screen.sync_keys(test_store().view());
    assert_eq!(related_pending_cells(&screen, &measure), 0, "no skeleton is left in view");
    clear();
}

/// The other half of the rule: a row whose loaded cards already reach past the screen paints no
/// pending cell there, and an unfocused one asks for nothing (the tail is read as focus nears it).
#[test]
fn an_unfocused_related_row_that_fills_the_screen_does_not_ask() {
    let sid = ServerId::UNSET;
    let mut d = short_related_head(sid);
    d.related = (0..12).map(|i| plx_data::pms::PmsMovie { thumb: format!("/t/r{i}"), ..collection_member(sid, &format!("r{i}")) }).collect();
    d.related_tail.head = 12;
    let _guard = install(d);
    let mut screen = bare(&_guard, sid, "m1");
    let measure = plx_ui::fixture::FixtureMeasure;
    assert_eq!(related_pending_cells(&screen, &measure), 0, "no pending cell is on screen");
    for n in 0..60 {
        let tick = ScreenEvent::Tick(plx_machine::machine::Tick { ms: 17 * n, dt_us: 16_667 });
        let (_, effects) = step(&mut screen, &tick, Some(hero::ELEM_PLAY));
        assert!(!effects.iter().any(|e| matches!(&e.fx,
            Fx::App(AppFx::Store(StoreId::Metadata, StoreCmd::Metadata(MetadataCmd::WantRelated { .. }))))),
            "tick {n}: an unfocused row that fills the screen asks for nothing");
    }
    clear();
}

// ---- the bookmark disc on a DetailScreen (docs/watchlist-plan.md §1.4, §1.8) -----------------

const WL_HEX: [u8; 12] = [0x5d, 0x77, 0x6b, 0x1d, 0x96, 0xb6, 0x55, 0x00, 0x1f, 0x2a, 0x9c, 0x3e];
const WL_GUID: &str = "plex://movie/5d776b1d96b655001f2a9c3e";

fn movie_with_guid(guid: &str) -> Detail {
    Detail {
        sid: ServerId::UNSET,
        rk: "movie".into(),
        kind: "movie".into(),
        title: "Movie".into(),
        part: "/library/parts/movie".into(),
        guid: guid.into(),
        ..Default::default()
    }
}

fn item_menu_acts(effects: &[plx_machine::machine::Stamped<TestHost>]) -> Vec<crate::registry::ItemMenuAction> {
    effects
        .iter()
        .filter_map(|e| match &e.fx {
            Fx::App(AppFx::ItemMenu(req)) => Some(req.act.clone()),
            _ => None,
        })
        .collect()
}

/// A title the service cannot key (a local agent, personal media) gets NO disc: not drawn, not
/// focusable, and its element is not placeable in the row.
#[test]
fn a_non_plex_guid_has_no_bookmark_disc() {
    let _guard = install(movie_with_guid("local://1"));
    let mut screen = bare(&_guard, ServerId::UNSET, "movie");
    let set = screen.hero_set(test_store().view());
    assert!(!set.watchlist);
    assert!(!hero::hero_ctls(set).0[..hero::hero_ctls(set).1].contains(&hero::HeroCtl::Watchlist));
    assert!(!hero::visible_ctls(set, false).0[..hero::visible_ctls(set, false).1].contains(&hero::HeroCtl::Watchlist));
    assert_eq!(hero::index_of(set, hero::HeroCtl::Watchlist), None);
    let (_, fx) = step(&mut screen, &ScreenEvent::Activate(hero::ELEM_WATCHLIST), Some(hero::ELEM_WATCHLIST));
    assert!(item_menu_acts(&fx).is_empty());
}

/// A `plex://movie` title gets the disc second, and OK on it asks the item menu's own write: Add
/// from the outline face, Remove (keyed by the guid) from the filled one.
#[test]
fn the_bookmark_disc_press_emits_the_item_menu_write() {
    let _guard = install(movie_with_guid(WL_GUID));
    let mut screen = bare(&_guard, ServerId::UNSET, "movie");
    let set = screen.hero_set(test_store().view());
    assert!(set.watchlist);
    assert_eq!(hero::index_of(set, hero::HeroCtl::Watchlist), Some(1));
    tick_page(&mut screen, None, 16);
    assert_eq!(screen.watchlist_hex(), Some(WL_HEX));
    let (_, fx) = step(&mut screen, &ScreenEvent::Activate(hero::ELEM_WATCHLIST), Some(hero::ELEM_WATCHLIST));
    assert_eq!(item_menu_acts(&fx), vec![crate::registry::ItemMenuAction::WatchlistAdd { hex: WL_HEX }]);
    screen.set_watchlist(plx_data::catalog::Membership::On);
    let (_, fx) = step(&mut screen, &ScreenEvent::Activate(hero::ELEM_WATCHLIST), Some(hero::ELEM_WATCHLIST));
    assert_eq!(item_menu_acts(&fx), vec![crate::registry::ItemMenuAction::WatchlistRemove(WL_GUID.into())]);
}

/// The first membership answer is the disc's rest face: no fill/ribbon flip plays on open. A later
/// change from a known face does animate.
#[test]
fn the_first_membership_answer_snaps_and_a_later_change_animates() {
    let _guard = install(movie_with_guid(WL_GUID));
    let mut screen = bare(&_guard, ServerId::UNSET, "movie");
    screen.set_watchlist(plx_data::catalog::Membership::On);
    tick_page(&mut screen, None, 16);
    assert_eq!(screen.watchlist.pos(), 1.0, "open on an on-list title draws the filled face at rest");
    screen.set_watchlist(plx_data::catalog::Membership::Off);
    tick_page(&mut screen, None, 16);
    assert!(
        (0.0..1.0).contains(&screen.watchlist.pos()) && screen.watchlist.pos() > 0.0,
        "a flip from a known face animates"
    );
}

/// The Add pressed while the membership is still `Unknown` (the list unread) is the user's own
/// change: it unfurls instead of snapping. With no press, the same change is the first answer and snaps.
#[test]
fn an_add_pressed_from_an_unknown_membership_unfurls() {
    let _guard = install(movie_with_guid(WL_GUID));
    let mut screen = bare(&_guard, ServerId::UNSET, "movie");
    tick_page(&mut screen, None, 16);
    step(&mut screen, &ScreenEvent::Activate(hero::ELEM_WATCHLIST), Some(hero::ELEM_WATCHLIST));
    screen.set_watchlist(plx_data::catalog::Membership::On);
    let mut trace = Vec::new();
    for n in 0..8 {
        tick_page(&mut screen, None, 16 * n);
        trace.push(screen.watchlist.pos());
    }
    assert!(trace.iter().filter(|p| **p > 0.0 && **p < 1.0).count() >= 5, "the add unfurls over frames: {trace:?}");

    let mut unpressed = bare(&_guard, ServerId::UNSET, "movie");
    unpressed.set_watchlist(plx_data::catalog::Membership::On);
    assert_eq!(unpressed.watchlist.pos(), 1.0, "an unpressed first answer snaps");
}

/// A flip Off -> On, ticked until idle, rests EXACTLY on the filled face (not 0.999 drawing a clip).
#[test]
fn the_bookmark_flip_snaps_to_its_target_at_rest() {
    let _guard = install(movie_with_guid(WL_GUID));
    let mut screen = bare(&_guard, ServerId::UNSET, "movie");
    screen.set_watchlist(plx_data::catalog::Membership::Off);
    tick_page(&mut screen, None, 16);
    screen.set_watchlist(plx_data::catalog::Membership::On);
    for n in 0..600 {
        tick_page(&mut screen, None, 16 * n);
    }
    assert_eq!(screen.watchlist.pos(), 1.0);
    screen.set_watchlist(plx_data::catalog::Membership::Off);
    for n in 0..600 {
        tick_page(&mut screen, None, 16 * n);
    }
    assert_eq!(screen.watchlist.pos(), 0.0);
}

// ---- the hero's ratings row and identity line, read off the PAINTED runs ----------------------

/// Shipped advances scaled by `.0`: a way to make a ratings row overflow by a chosen amount.
struct ScaledMeasure(f32);
impl plx_machine::machine::Measure for ScaledMeasure {
    fn width(&self, s: &std::ffi::CStr, sz: i32, bold: bool) -> f32 {
        plx_base::fontcov::advances::ShippedMeasure.width(s, sz, bold) * self.0
    }
    fn cap_h(&self, sz: i32) -> f32 {
        plx_base::fontcov::advances::ShippedMeasure.cap_h(sz)
    }
    fn line_h(&self, sz: i32) -> f32 {
        plx_base::fontcov::advances::ShippedMeasure.line_h(sz)
    }
}

fn two_provider_ratings() -> Detail {
    use plx_data::metadata::{Rating, RatingArt};
    Detail {
        ratings: vec![
            Rating { art: RatingArt::Imdb, value: 8.1, critic: false },
            Rating { art: RatingArt::Tmdb, value: 7.5, critic: false },
        ],
        ..Default::default()
    }
}

/// The group a provider's rating row stands: width as `draw_ratings` measures it.
fn group_w(art: plx_data::metadata::RatingArt, value: f64, m: &dyn plx_machine::machine::Measure) -> f32 {
    let score = plx_ui::fmt::rating_score(rating_scale(art), value);
    let cells = [plx_ui::widgets::RatingCell {
        mark: rating_mark(art),
        value: &score,
        suffix: plx_ui::fmt::rating_suffix(rating_scale(art)),
    }];
    plx_ui::widgets::rating_group_w(art.provider(), &cells, m)
}

fn painted_x(runs: &[plx_ui::draw_census::Run], text: &str) -> Option<f32> {
    runs.iter().find(|r| r.text == text).map(|r| r.at.x)
}

/// **Two rating groups stand `RATING_GROUP_GAP` apart, read off the painted captions.**
#[test]
fn the_ratings_row_paints_its_groups_one_gap_apart() {
    use plx_data::metadata::RatingArt;
    let _guard = plx_base::testlock::serial();
    let screen = bare_held(ServerId::UNSET, "rk");
    let d = two_provider_ratings();
    let m = &plx_base::fontcov::advances::ShippedMeasure;
    let runs = plx_ui::draw_census::capture_runs(|| screen.draw_ratings(plx_ui::Painter::recording(), &d, 600.0, m));
    let first = painted_x(&runs, "IMDb").expect("first caption painted");
    let second = painted_x(&runs, "TMDB").expect("second caption painted");
    assert!((first - plx_ui::consts::MARGIN_X).abs() < 0.5, "the row starts at the margin: {first}");
    let gap = second - (first + group_w(RatingArt::Imdb, 8.1, m));
    assert!((gap - plx_ui::widgets::RATING_GROUP_GAP).abs() < 0.5, "groups a gap apart, got {gap}");
}

/// **A group that would overflow the right margin is not drawn.** The scale is bisected until the
/// second group is JUST past the margin (by less than a gap), so a row that tested the group
/// against `end()` (the last group's edge, without the gap it owes) would still draw it.
#[test]
fn a_ratings_group_that_would_overflow_the_margin_is_not_drawn() {
    use plx_data::metadata::RatingArt;
    let _guard = plx_base::testlock::serial();
    let screen = bare_held(ServerId::UNSET, "rk");
    let d = two_provider_ratings();
    let limit = plx_ui::consts::SCR_W - plx_ui::consts::MARGIN_X;
    let overshoot = |k: f32| {
        let m = ScaledMeasure(k);
        plx_ui::consts::MARGIN_X + group_w(RatingArt::Imdb, 8.1, &m) + plx_ui::widgets::RATING_GROUP_GAP + group_w(RatingArt::Tmdb, 7.5, &m) - limit
    };
    let (mut lo, mut hi) = (1.0f32, 12.0f32);
    assert!(overshoot(lo) < 0.0 && overshoot(hi) > 0.0, "the bracket straddles the margin");
    for _ in 0..40 {
        let mid = (lo + hi) * 0.5;
        if overshoot(mid) > 0.0 { hi = mid } else { lo = mid }
    }
    let m = ScaledMeasure(hi);
    let over = overshoot(hi);
    assert!(over > 0.0 && over < plx_ui::widgets::RATING_GROUP_GAP, "just past the margin, by less than a gap: {over}");
    let runs = plx_ui::draw_census::capture_runs(|| screen.draw_ratings(plx_ui::Painter::recording(), &d, 600.0, &m));
    assert!(painted_x(&runs, "IMDb").is_some(), "the group that fits is drawn");
    assert!(painted_x(&runs, "TMDB").is_none(), "the group past the margin is not: {runs:?}");
    // and just inside, both are
    let m = ScaledMeasure(lo);
    let runs = plx_ui::draw_census::capture_runs(|| screen.draw_ratings(plx_ui::Painter::recording(), &d, 600.0, &m));
    assert!(painted_x(&runs, "TMDB").is_some(), "a group that fits is drawn");
}

fn identity_detail(parts_empty: bool) -> Detail {
    use plx_data::metadata::Stream;
    let mut d = Detail {
        kind: if parts_empty { "episode".into() } else { "movie".into() },
        video_resolution: "1080".into(),
        subs: vec![Stream { sdh: true, ..Default::default() }],
        ..Default::default()
    };
    d.is_show = false;
    d
}

/// The left edge of the chip whose label was painted at `label_x`: a filled badge centres its
/// label in the chip, a keyline chip pads it by a fixed 7px.
fn filled_left(label_x: f32, text: &str, m: &dyn plx_machine::machine::Measure) -> (f32, f32) {
    let w = plx_ui::widgets::badge_w(text, None, m);
    let tw = m.width_str(text, theme::size::CAPTION, true);
    let left = label_x - (w - tw) * 0.5;
    (left, left + w)
}
fn keyline_left(label_x: f32, text: &str, m: &dyn plx_machine::machine::Measure) -> (f32, f32) {
    let left = label_x - 7.0;
    (left, left + plx_ui::widgets::keyline_chip_w(text, m))
}

/// **The identity line: the chips stand `SM` after the dotted run and `XS` apart.**
#[test]
fn the_identity_line_paints_sm_after_the_run_and_xs_between_chips() {
    let _guard = plx_base::testlock::serial();
    let screen = bare_held(ServerId::UNSET, "rk");
    let d = identity_detail(false);
    let m = &plx_base::fontcov::advances::ShippedMeasure;
    let runs = plx_ui::draw_census::capture_runs(|| screen.draw_identity_line(plx_ui::Painter::recording(), &d, 300.0, m));
    let kind = plx_platform::i18n::msg::browse_kind_movie();
    let kind_run = runs.iter().find(|r| r.text == kind).expect("the kind label is painted");
    let run_right = kind_run.at.x + kind_run.at.w;
    let res = filled_left(painted_x(&runs, "1080p").expect("resolution chip"), "1080p", m);
    let cc_label = plx_platform::i18n::msg::widgets_badge_cc();
    let sdh_label = plx_platform::i18n::msg::widgets_badge_sdh();
    let cc = keyline_left(painted_x(&runs, cc_label).expect("CC chip"), cc_label, m);
    let sdh = keyline_left(painted_x(&runs, sdh_label).expect("SDH chip"), sdh_label, m);
    assert!((res.0 - run_right - theme::space::SM).abs() < 0.5, "SM after the dotted run: {} vs {run_right}", res.0);
    assert!((cc.0 - res.1 - theme::space::XS).abs() < 0.5, "XS between the badge and the first chip");
    assert!((sdh.0 - cc.1 - theme::space::XS).abs() < 0.5, "XS between the chips");
}

/// **An empty dotted run owes no gap**: the first chip stands on the margin itself.
#[test]
fn an_empty_identity_run_leaves_no_gap_before_the_first_chip() {
    let _guard = plx_base::testlock::serial();
    let screen = bare_held(ServerId::UNSET, "rk");
    let d = identity_detail(true);
    let m = &plx_base::fontcov::advances::ShippedMeasure;
    let runs = plx_ui::draw_census::capture_runs(|| screen.draw_identity_line(plx_ui::Painter::recording(), &d, 300.0, m));
    let res = filled_left(painted_x(&runs, "1080p").expect("resolution chip"), "1080p", m);
    assert!((res.0 - plx_ui::consts::MARGIN_X).abs() < 0.5, "no run, no gap: chip at {}", res.0);
}

/// **The held episode still is drawn ONCE while its hold menu is up.** The page pass (a frame
/// carrying `lifted`, like a card section's) leaves the whole cell out - still AND label-block
/// platter - and says so (`popover::lift_owns`); the opener lift (`redraw_focused`, a frame with no
/// `lifted`) is the cell's only copy, at the press's own dip. Before, the page drew it at the
/// undipped pop under the lift's dipped copy (two sizes of one still), and drew its platter too
/// (two stacked plates while the menu was up, one the moment the lift stood down: a colour snap).
#[test]
fn the_held_episode_still_is_left_out_of_the_page_pass_and_drawn_once_by_the_lift() {
    let sid = ServerId::UNSET;
    let mut show = detail(sid, "show");
    show.episodes = (1..=3).map(|i| episode(&format!("e{i}"), i)).collect();
    let _guard = install(show);
    let mut screen = bare(&_guard, sid, "show");
    let measure = plx_ui::fixture::FixtureMeasure;
    let key = FocusKey { entry: EntryId(7), elem: episode_key(&screen, 0, episodes::Row::Still).unwrap() };
    for _ in 0..120 {
        screen.episode_cells.step(Some(0), None, 1.0 / 60.0);
    }
    const DIP: f32 = 0.918;
    let mut context = cx(&measure, Some(key.elem));
    context.press = PressRead { scale: DIP, is_long: true, held_ms: Some(400), owner: Some(key) };

    let mut census = |lifted: Option<FocusKey<u32>>, lift: bool| {
        plx_ui::popover::set_lift_owns(false);
        plx_ui::draw_census::capture(|| {
            let mut f = DrawFrame::new(&context, plx_ui::Painter::recording());
            f.lifted = lifted;
            if lift {
                screen.redraw_focused::<TestHost>(&mut f, Some(key));
            } else {
                plx_gfx::gfx::without_frame_clear(|| Screen::<TestHost>::draw(&mut screen, &mut f));
            }
        })
    };
    // The first cell, as a census reads it: its still is the one tag-21 art quad over the strip, its
    // label plate the one tag-2 rounded rect as wide as a cell.
    let stills = |v: &[(u64, plx_ui::Rect)]| -> Vec<plx_ui::Rect> {
        v.iter().filter(|(t, r)| *t == 21 && r.x < 540.0 && r.y > 900.0).map(|(_, r)| *r).collect()
    };
    let plates = |v: &[(u64, plx_ui::Rect)]| -> Vec<plx_ui::Rect> {
        v.iter().filter(|(t, r)| *t == 2 && (r.w - episodes::W).abs() < 8.0 && r.x < 540.0 && r.y > 900.0 && r.h < 200.0).map(|(_, r)| *r).collect()
    };

    // the menu is up: the page pass leaves the whole cell out and says so...
    let page_lifted = census(Some(key), false);
    assert!(plx_ui::popover::lift_owns(), "the page pass said the lift owns the cell");
    assert_eq!((stills(&page_lifted).len(), plates(&page_lifted).len()), (0, 0), "the page pass drew {page_lifted:?}");
    // ...and the lift (a frame with no `lifted`) is its only copy: one still, one plate.
    let lift = census(None, true);
    assert_eq!((stills(&lift).len(), plates(&lift).len()), (1, 1), "the lift drew {lift:?}");

    // the press is in: the page's own copy dips exactly as the lift's does (one size of one still)
    let page_pressed = census(None, false);
    let (page_still, lift_still) = (stills(&page_pressed), stills(&lift));
    assert_eq!(page_still.len(), 1);
    let (a, b) = (page_still[0], lift_still[0]);
    assert!(
        (a.x - b.x).abs() < 0.01 && (a.y - b.y).abs() < 0.01 && (a.w - b.w).abs() < 0.01 && (a.h - b.h).abs() < 0.01,
        "page still {a:?} vs lift still {b:?}"
    );
    assert!(a.w < episodes::W * theme::EP_CARD_FOCUS_SCALE - 1.0, "the still is dipped: {a:?}");

    // across the frame the lift stands down (`lifted` clears) there is one plate before and after
    let before = plates(&page_lifted).len() + plates(&lift).len();
    let after = plates(&page_pressed).len();
    assert_eq!((before, after), (1, 1), "plates before / after the lift stands down");
    assert_eq!(plates(&lift), plates(&page_pressed), "...and it is the same plate");
    clear();
}

/// A held OK on an episode's still opens its menu only through `focused_episode`, which answers
/// `None` while a season fetch is in flight. The hold hint must use that same answer: no promise
/// of a hold that opens nothing while a switched season reloads, and the hint is there again once
/// the season lands.
#[test]
fn the_hold_hint_over_an_episode_still_waits_while_the_season_loads() {
    let sid = ServerId::UNSET;
    let mut show = detail(sid, "show");
    show.episodes = (1..=3).map(|i| episode(&format!("e{i}"), i)).collect();
    let _guard = install(show);
    plx_ui::hold_hint::reset_learned_for_test();
    plx_ui::hold_hint::reset_shown_for_test();
    let mut screen = bare(&_guard, sid, "show");
    screen.sync_keys(test_store().view());
    let measure = plx_ui::fixture::FixtureMeasure;
    let elem = episode_key(&screen, 0, episodes::Row::Still).unwrap();
    let mut context = cx(&measure, Some(elem));
    context.press = PressRead { scale: 1.0, is_long: false, held_ms: Some(200), owner: None };
    let located = Some(Located::Episode(0, episodes::Row::Still));
    let hinted = |screen: &DetailScreen| screen.hint_input(&context, located).focus.is_some();

    assert!(hinted(&screen), "a loaded season's still has a menu to hold for");
    plx_data::metadata::set_season_loading_for_test(test_store().adapter_ref(), true);
    assert!(!hinted(&screen), "no hint while the season loads: the hold would open nothing");
    plx_data::metadata::set_season_loading_for_test(test_store().adapter_ref(), false);
    assert!(hinted(&screen), "the hint is back once the season has landed");
    clear();
}

/// The pointer's edge glide (a fractional wheel every frame) scrolls the detail page itself: a
/// page resting on a lower section glides up continuously, never past the top, and down no further
/// than the last section's reveal.
#[test]
fn a_wheel_glides_the_detail_page_within_its_range() {
    let sid = ServerId::UNSET;
    let mut page = detail(sid, "show");
    page.related = (1..=12).map(|i| collection_member(sid, &format!("r{i}"))).collect();
    let _guard = install(page);
    let mut screen = bare(&_guard, sid, "show");
    let wheel = |dy: f32| ScreenEvent::Input(InputEvent {
        at: Default::default(),
        source: plx_machine::machine::Source::Script,
        kind: InputKind::Wheel { dy },
    });
    let max = {
        let measure = plx_ui::fixture::FixtureMeasure;
        let d = screen.detail(test_store().view()).unwrap();
        let (sections, n) = screen.sections(Some(d));
        screen.reveal_target(sections[n - 1], d, &measure)
    };
    assert!(max > 300.0, "the fixture scrolls: {max}");
    screen.scroll.jump(max);
    screen.scroll_target = max;
    let mut last = max;
    for _ in 0..10 {
        assert_eq!(step(&mut screen, &wheel(0.05), None).0, Handled::Yes);
        assert!(screen.scroll_target < last, "every frame of the glide moves the page up");
        last = screen.scroll_target;
    }
    assert!((max - last - 0.5 * plx_ui::consts::ROW_PITCH).abs() < 0.5, "half a row: {max} -> {last}");
    for _ in 0..200 { step(&mut screen, &wheel(0.1), None); }
    assert_eq!(screen.scroll_target, 0.0, "never past the top");
    for _ in 0..400 { step(&mut screen, &wheel(-0.1), None); }
    assert_eq!(screen.scroll_target, max, "down no further than the last section's reveal");
    clear();
}

/// **A hover moves focus, never the detail page; a click brings the page to what it clicked**; a
/// key still brings its section into reading position. Before, a Magic Remote hover yanked the
/// section it touched up to the title band, so every hover moved the page and undid a glide.
#[test]
fn a_hover_moves_no_detail_page_and_a_click_brings_it_to_its_element() {
    let sid = ServerId::UNSET;
    let mut page = detail(sid, "show");
    page.related = (1..=12).map(|i| collection_member(sid, &format!("r{i}"))).collect();
    page.extras = (1..=12).map(|i| plx_data::metadata::Extra { rk: format!("x{i}"), ..Default::default() }).collect();
    let _guard = install(page);
    let mut screen = bare(&_guard, sid, "show");
    let related = screen.engine_key(related::elem(0).unwrap()).unwrap();
    let moved = |screen: &mut DetailScreen, from: u32, to: u32, by: By| {
        let ev = ScreenEvent::FocusMoved { from: Some(FocusKey { entry: screen.entry, elem: from }),
            to: FocusKey { entry: screen.entry, elem: to }, by };
        step(screen, &ev, Some(to));
    };
    moved(&mut screen, hero::ELEM_PLAY, related, By::Dir);
    let seated = screen.scroll_target;
    assert!(seated > 0.0, "a key brings Related into reading position");
    moved(&mut screen, related, hero::ELEM_PLAY, By::Pointer);
    assert_eq!(screen.scroll_target, seated, "a hover moves focus, not the page");
    screen.scroll_target = 0.0;
    screen.scroll.jump(0.0);
    moved(&mut screen, hero::ELEM_PLAY, related, By::Pointer);
    assert_eq!(screen.scroll_target, 0.0, "nor after a glide");
    let click = ScreenEvent::Input(InputEvent { at: Default::default(), source: plx_machine::machine::Source::Script,
        kind: InputKind::Click { x: 0.0, y: 0.0, hit: Some(related) } });
    step(&mut screen, &click, Some(related));
    assert_eq!(screen.scroll_target, seated, "a click brings the page to what it clicked");
    clear();
}

/// **A landing does not pull a glided detail page back.** A series page would jump
/// around while the pointer's edge glide scrolled it, because every landing (a season's episodes,
/// the next page of a shelf as the glide reached it, the cast, the ratings) brought the page back
/// to the focused section. Now a page the pointer moved stays where it is; a key's move still
/// brings it to the focus, and a landing after that follows the focus again.
#[test]
fn a_landing_does_not_pull_a_glided_detail_page_back() {
    let sid = ServerId::UNSET;
    let mut page = detail(sid, "show");
    page.related = (1..=12).map(|i| collection_member(sid, &format!("r{i}"))).collect();
    page.extras = (1..=12).map(|i| plx_data::metadata::Extra { rk: format!("x{i}"), ..Default::default() }).collect();
    let _guard = install(page);
    let mut screen = bare(&_guard, sid, "show");
    let wheel = |dy: f32| ScreenEvent::Input(InputEvent { at: Default::default(),
        source: plx_machine::machine::Source::Script, kind: InputKind::Wheel { dy } });
    let landing = ScreenEvent::StoreChanged(StoreId::Metadata.ord(), 1);
    let play = hero::ELEM_PLAY;
    // focus on the hero, glided well down the page
    for _ in 0..40 { step(&mut screen, &wheel(-0.05), Some(play)); }
    let glided = screen.scroll_target;
    assert!(glided > 300.0, "premise: the glide moved the page: {glided}");
    step(&mut screen, &landing, Some(play));
    assert_eq!(screen.scroll_target, glided, "a landing leaves the glided page where it is");
    // a key's move brings the page to the focus, and a landing then follows the focus again
    let related = screen.engine_key(related::elem(0).unwrap()).unwrap();
    let entry = screen.entry;
    step(&mut screen, &ScreenEvent::FocusMoved { from: Some(FocusKey { entry, elem: play }),
        to: FocusKey { entry, elem: related }, by: By::Dir }, Some(related));
    let seated = screen.scroll_target;
    screen.scroll_target = seated + 50.0;
    step(&mut screen, &landing, Some(related));
    assert_eq!(screen.scroll_target, seated, "with no glide, a landing keeps the focused section in reading position");
    clear();
}

/// **The pointer's sideways glide moves the row under it on a detail page**: the Related shelf, and
/// the episode strip, which then stays where the glide put it although an episode holds focus,
/// until a key moves focus.
#[test]
fn the_sideways_glide_moves_the_related_shelf_and_the_episode_strip() {
    let sid = ServerId::UNSET;
    let mut page = detail(sid, "show");
    page.episodes = (1..=12).map(|i| episode(&format!("e{i}"), i)).collect();
    page.related = (1..=12).map(|i| collection_member(sid, &format!("r{i}"))).collect();
    let _guard = install(page);
    let mut screen = bare(&_guard, sid, "show");
    let measure = plx_ui::fixture::FixtureMeasure;
    let row_y = |screen: &DetailScreen, sec: section::SectionId| {
        let d = screen.detail(test_store().view()).unwrap();
        screen.section_top(sec.raw(), d, &measure) - screen.scroll.pos + screen.block_h(sec.raw(), d, &measure) / 2.0
    };
    let pan = |y: f32| ScreenEvent::Input(InputEvent { at: Default::default(),
        source: plx_machine::machine::Source::Script, kind: InputKind::Pan { dx: 20.0, x: 1900.0, y } });
    let y = row_y(&screen, section::SectionId::Related);
    for _ in 0..10 { step(&mut screen, &pan(y), None); }
    assert_eq!(screen.related.scroll(), 200.0, "the Related shelf glides");
    assert_eq!(screen.episode_scroll.pos, 0.0, "the episode strip stays");
    let y = row_y(&screen, section::SectionId::Episode);
    let ep = screen.engine_key(episodes::elem(0, episodes::Row::Still).unwrap()).unwrap();
    for _ in 0..10 { step(&mut screen, &pan(y), Some(ep)); }
    assert_eq!(screen.episode_scroll.pos, 200.0, "the episode strip glides");
    for i in 1..=60 { step(&mut screen, &ScreenEvent::Tick(Tick { ms: i * 16, dt_us: 16_667 }), Some(ep)); }
    assert_eq!(screen.episode_scroll.pos, 200.0, "its focused episode does not pull it back");
    let entry = screen.entry;
    // a key's move to the first episode (off screen to the left now) brings the strip back to it
    step(&mut screen, &ScreenEvent::FocusMoved { from: Some(FocusKey { entry, elem: ep }),
        to: FocusKey { entry, elem: ep }, by: By::Dir }, Some(ep));
    for i in 61..=200 { step(&mut screen, &ScreenEvent::Tick(Tick { ms: i * 16, dt_us: 16_667 }), Some(ep)); }
    assert!(screen.episode_scroll.pos < 1.0, "a key's move brings the strip back to its focus: {}", screen.episode_scroll.pos);
    clear();
}

fn versioned(kind: &str, versions: usize) -> Detail {
    Detail {
        sid: ServerId::UNSET,
        rk: "leaf".into(),
        kind: kind.into(),
        title: "Leaf".into(),
        part: "/library/parts/leaf".into(),
        resume_ms: 600_000,
        dur_ms: 6_000_000,
        versions: plx_appkit::version_list::sample().into_iter().take(versions).collect(),
        ..Default::default()
    }
}

/// A version the panel reports plays THAT version of the page's item, resuming as Play does, on a
/// movie page and an episode page alike.
#[test]
fn a_picked_version_plays_it_resuming_like_play() {
    for kind in ["movie", "episode"] {
        let _guard = install(versioned(kind, 2));
        let mut screen = bare(&_guard, ServerId::UNSET, "leaf");
        let pick = plx_appkit::version_list::sample()[1].id;
        let (_, effects) = step(&mut screen, &ScreenEvent::App(AppMsg::PlayVersion(pick)), None);
        let (play, resume_ns) = play_item(&effects).expect("a pick plays");
        match play {
            PlayIntent::Item { rk, version, .. } => {
                assert_eq!(rk, "leaf", "{kind}");
                assert_eq!(*version, Some(pick), "{kind}");
            }
            PlayIntent::Movie { .. } => panic!("{kind}: the loaded item, not a hub row"),
        }
        assert_eq!(resume_ns, plx_data::metadata::resume_ns(600_000, 6_000_000), "{kind} resumes");
        let (_, effects) = step(&mut screen, &ScreenEvent::Activate(hero::ELEM_PLAY), Some(hero::ELEM_PLAY));
        assert!(
            matches!(play_item(&effects), Some((PlayIntent::Item { version: None, .. }, _))),
            "{kind}: the Play pill still plays the default version"
        );
        clear();
    }
}

/// On a show page the disc offers the versions of the episode the hero's Play plays (the on-deck
/// one once the show is started), and a pick plays THAT episode in that version.
#[test]
fn a_show_page_offers_and_plays_its_hero_episodes_versions() {
    let two = || plx_appkit::version_list::sample().into_iter().take(2).collect::<Vec<_>>();
    let on_deck = plx_data::metadata::Episode { resume_ms: 30_000, versions: two(), ..episode("e2", 2) };
    let mut d = detail(ServerId::UNSET, "show");
    d.episodes = vec![episode("e1", 1), on_deck.clone()].into();
    d.on_deck = Some(on_deck);
    let _guard = install(d);
    let mut screen = bare(&_guard, ServerId::UNSET, "show");
    assert!(screen.hero_set(test_store().view()).version, "the on-deck episode has two versions");
    let pick = plx_appkit::version_list::sample()[1].id;
    let (_, effects) = step(&mut screen, &ScreenEvent::App(AppMsg::PlayVersion(pick)), None);
    match play_item(&effects).map(|(play, _)| play) {
        Some(PlayIntent::Item { rk, version, .. }) => {
            assert_eq!(rk, "e2", "the hero's episode");
            assert_eq!(*version, Some(pick));
        }
        _ => panic!("a pick plays the hero's episode"),
    }
    clear();
    drop(_guard);

    // The first episode has one version and no episode is on deck: nothing to choose.
    let _guard = install(detail(ServerId::UNSET, "show"));
    let screen = bare(&_guard, ServerId::UNSET, "show");
    assert!(!screen.hero_set(test_store().view()).version);
    clear();
}
