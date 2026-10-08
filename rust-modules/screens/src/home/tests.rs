use super::*;

use plx_ui::fixture::FixtureMeasure;
use plx_ui::focus::{FocusEngine, Outcome};
use plx_ui::hit::{HitMap, PointerKind};
use plx_machine::machine::{
    Chrome, FocusRead, Host, InputOwner, PressRead, ScreenId, Source, Stamped, Tick,
};
use plx_ui::screen::{ScreenArg, ScreenEvent};

/// How many rows the layout sweeps cover: past the data layer's own cap, which is not Home's.
const SWEEP_ROWS: usize = 40;

#[derive(Clone, PartialEq, Eq)]
struct TestArg;

impl LogicalState for TestArg {
    fn write(&self, c: &mut Canon) {
        c.u32(0);
    }
    fn probe(&self, _: &mut String) {}
}

impl ScreenArg for TestArg {
    fn chrome(&self) -> Chrome {
        Chrome::TabBar
    }
    fn id(&self) -> ScreenId {
        ScreenId(800)
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
    type Msg = super::super::registry::AppMsg;
    type Elem = u32;
    type Views<'a> = HubsView<'a>;
    type Init = super::super::family::NoInit;
    type Memory = PageMemory;
}

impl HomeLike for TestHost {
    fn hubs<'a>(cx: &Cx<'a, Self>) -> HubsView<'a> {
        cx.views
    }
}

fn cx<'a>(view: HubsView<'a>, focus: Option<FocusKey<u32>>) -> Cx<'a, TestHost> {
    static MEASURE: FixtureMeasure = FixtureMeasure;
    Cx {
        views: view,
        tick: Tick::default(),
        measure: &MEASURE,
        press: PressRead::default(),
        focus: FocusRead { current: focus , ..Default::default() },
        owner: InputOwner::Entry(focus.map_or(EntryId(7), |k| k.entry)),
    }
}

fn screen(view: HubsView<'_>) -> HomeScreen {
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    s.sync_catalog(&cx(view, None));
    s.layout_grid();
    s
}

fn step(
    s: &mut HomeScreen,
    view: HubsView<'_>,
    focus: Option<FocusKey<u32>>,
    event: &ScreenEvent<TestHost>,
) -> (Handled, Vec<Stamped<TestHost>>, bool) {
    let context = cx(view, focus);
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let handled = {
        let mut fx = Effects::new(
            &mut out,
            plx_machine::machine::MachineId::Instance(InstanceId(9)),
            &mut present,
        );
        Machine::<TestHost>::step(s, event, &context, &mut fx)
    };
    (handled, out, present.peek(0))
}

/// Pop `key`'s card the way a deliberate move does: the move, then `frames` ticks with the card
/// focused (the page must already be on the shelves, `snap` at 1).
fn pop_card(s: &mut HomeScreen, view: HubsView<'_>, key: FocusKey<u32>, frames: u32) {
    step(s, view, Some(key), &ScreenEvent::FocusMoved { from: None, to: key, by: By::Dir });
    for frame in 0..frames {
        step(s, view, Some(key), &ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_667 }));
    }
}

/// The live pop scale of the card at (`row`, `col`), no press.
fn pop_of(s: &HomeScreen, view: HubsView<'_>, focus: Option<FocusKey<u32>>, row: usize, col: usize) -> f32 {
    let elem = s.rows[row].elems[col];
    s.grid.shelves[row].scale_of(&cx(view, focus), &s.cards(view, row), &elem).unwrap()
}

/// The rect the shelf paints card (`row`, `col`) at: `place(.., At::Drawn)`.
fn drawn_rect(s: &HomeScreen, view: HubsView<'_>, focus: Option<FocusKey<u32>>, row: usize, col: usize) -> Rect {
    Focusable::<TestHost>::place(s, &s.rows[row].elems[col], &cx(view, focus), At::Drawn).unwrap().rect
}

impl HomeScreen {
    /// Pop the first card of shelf 0 by feeding the shelf itself a deliberate move and a few
    /// ticks, leaving every other part of the screen untouched.
    fn pop_shelf_for_test(&mut self, view: HubsView<'_>) {
        let key = first_card(self);
        let context = cx(view, Some(key));
        let mut shelves = std::mem::take(&mut self.grid.shelves);
        let src = self.cards(view, 0);
        let mut out = Vec::new();
        let mut present = plx_machine::present::Present::new();
        let mut fx = Effects::new(&mut out, plx_machine::machine::MachineId::Instance(InstanceId(9)), &mut present);
        shelves[0].on(&ScreenEvent::FocusMoved { from: None, to: key, by: By::Dir }, &context, &src, &mut fx);
        for frame in 0..3 {
            shelves[0].on(&ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_667 }), &context, &src, &mut fx);
        }
        drop(src);
        self.grid.shelves = shelves;
    }

    /// `update_grid` with a throwaway effect sink and a tick of `dt` seconds.
    fn update_grid_for_test(&mut self, view: HubsView<'_>, cx: &Cx<'_, TestHost>, dt: f32) {
        let mut out = Vec::new();
        let mut present = plx_machine::present::Present::new();
        let mut fx = Effects::new(&mut out, plx_machine::machine::MachineId::Instance(InstanceId(9)), &mut present);
        self.update_grid(view, cx, &mut fx, Tick { ms: 0, dt_us: (dt * 1_000_000.0).round() as u32 });
    }
}

fn first_card(s: &HomeScreen) -> FocusKey<u32> {
    FocusKey {
        entry: s.entry,
        elem: s.rows[0].elems[0],
    }
}

fn has_home(out: &[Stamped<TestHost>], pred: impl Fn(&HomeReq) -> bool) -> bool {
    out.iter().any(|stamped| match &stamped.fx {
        Fx::App(AppFx::Home(req)) => pred(req),
        _ => false,
    })
}

#[test]
fn every_tab_pill_round_trips_through_the_focus_packing() {
    let cases = [
        (STRIP_HOME_ELEM, HomeReq::Tab(HomeTab::Home)),
        (STRIP_MOVIES_ELEM, HomeReq::Tab(HomeTab::Movies)),
        (STRIP_SHOWS_ELEM, HomeReq::Tab(HomeTab::Shows)),
        (STRIP_SEARCH_ELEM, HomeReq::Tab(HomeTab::Search)),
        (STRIP_ACCOUNT_ELEM, HomeReq::Account),
    ];
    for (elem, want) in cases {
        assert_eq!(HomeScreen::request_for_strip(elem), Some(want));
    }
    assert_eq!(HomeScreen::request_for_strip(STRIP_ACCOUNT_ELEM + 1), None);
    for page_key in [HERO_PLAY_ELEM, HERO_INFO_ELEM, FIRST_ITEM_ELEM] {
        assert_eq!(HomeScreen::request_for_strip(page_key), None);
    }
}

#[test]
fn top_band_focus_walks_to_the_last_section_whatever_the_count() {
    assert_eq!(STRIP_MOVIES_ELEM - STRIP_HOME_ELEM, 1);
    assert_eq!(STRIP_SHOWS_ELEM - STRIP_HOME_ELEM, 2);
    assert_eq!(STRIP_SEARCH_ELEM - STRIP_HOME_ELEM, 3);
    assert_eq!(STRIP_ACCOUNT_ELEM - STRIP_HOME_ELEM, 4);
}

#[test]
fn set_hero_focus_clamps_onto_the_last_drawable_pill() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let s = screen(snapshot.view());
    let mut groups = Vec::new();
    Focusable::<TestHost>::groups(&s, &cx(snapshot.view(), None), &mut groups);
    assert_eq!(groups[0].id, HERO_GROUP);
    assert_eq!(groups[0].len, 2);
    assert!(groups
        .iter()
        .all(|g| g.id != plx_ui::containers::tabs::STRIP));
}

#[test]
fn the_pager_is_not_a_focus_stop_and_the_rows_end_pages_instead() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    let context = cx(
        snapshot.view(),
        Some(FocusKey {
            entry: s.entry,
            elem: HERO_INFO_ELEM,
        }),
    );
    let owner = InputOwner::Entry(s.entry);
    let mut engine = FocusEngine::new();
    engine.set(
        owner,
        context.focus.current.unwrap(),
        Some(HERO_GROUP),
        By::Restore,
    );
    let mut links = Vec::new();
    <HomeScreen as Screen<TestHost>>::links(&s, &mut links);
    assert_eq!(
        engine.move_dir(owner, &s, &links, Dir::Right, &context),
        Outcome::Edge(EdgeRule::Screen)
    );
    let before = s.carousel.clone();
    assert!(s.flip(snapshot.view(), 1));
    assert_ne!(s.carousel, before);
    assert_eq!(engine.current(owner).unwrap().elem, HERO_INFO_ELEM);
}

#[test]
fn the_top_band_reports_the_chip_and_the_pills_as_one_answer() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 1, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let s = screen(snapshot.view());
    let mut links = Vec::new();
    <HomeScreen as Screen<TestHost>>::links(&s, &mut links);
    assert!(links.contains(&Link {
        from: plx_ui::containers::tabs::STRIP,
        dir: Dir::Down,
        to: HERO_GROUP
    }));
    assert!(links.contains(&Link {
        from: HERO_GROUP,
        dir: Dir::Up,
        to: plx_ui::containers::tabs::STRIP
    }));
}

#[test]
fn the_top_band_walks_permanent_pills_not_the_section_table() {
    assert_ne!(STRIP_HOME_ELEM, STRIP_MOVIES_ELEM);
    assert_ne!(STRIP_MOVIES_ELEM, STRIP_SHOWS_ELEM);
    assert_ne!(STRIP_SHOWS_ELEM, STRIP_SEARCH_ELEM);
    assert!(matches!(
        HomeScreen::request_for_strip(STRIP_SEARCH_ELEM),
        Some(HomeReq::Tab(HomeTab::Search))
    ));
}

#[test]
fn step_row_stays_inside_the_addressable_rows() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let s = screen(snapshot.view());
    let first = first_card(&s);
    assert!(matches!(
        Focusable::<TestHost>::neighbour(&s, first, Dir::Left, &cx(snapshot.view(), Some(first))),
        Step::Edge
    ));
    let last = FocusKey {
        entry: s.entry,
        elem: s.rows[0].elems[2],
    };
    assert!(matches!(
        Focusable::<TestHost>::neighbour(&s, last, Dir::Right, &cx(snapshot.view(), Some(last))),
        Step::Edge
    ));
}

#[test]
fn the_status_readout_tells_loading_empty_and_failed_apart() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    for (hub_state, kind, action) in [
        (plx_data::pms::HubState::Loading, StatusKind::Working, false),
        (plx_data::pms::HubState::Failed, StatusKind::Failed, true),
        (plx_data::pms::HubState::Ready, StatusKind::Empty, true),
    ] {
        plx_data::pms::seed_for_test(&mut state, &adapter, 0, hub_state);
        let snapshot = plx_data::pms::hubs_snapshot(&state);
        let (_, got, got_action) = status_read(snapshot.view()).unwrap();
        assert_eq!(got, kind);
        assert_eq!(got_action.is_some(), action);
    }
    for hub_state in [plx_data::pms::HubState::Ready, plx_data::pms::HubState::Failed] {
        plx_data::pms::seed_for_test(&mut state, &adapter, 3, hub_state);
        assert!(status_read(plx_data::pms::hubs_snapshot(&state).view()).is_none(),
            "a failed refresh retains playable content, not a replacement readout");
    }
}

/// **A failed Home stands on the page read-out's anchor** — the verdict hanging from
/// `StatusOverlay::FULL_ANCHOR_TOP` and *Try again* stacked `space::LG` under it (no reason), the
/// same anchor a failed Library section and a failed sign-in use (each screen's own test pins its
/// side), in the one wording every screen uses for an unreachable server. The hit rect is the drawn pill: it is
/// built from the same overlay the draw uses.
#[test]
fn a_failed_home_stands_on_the_page_readout_lines() {
    use plx_ui::widgets::StatusOverlay;
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 0, plx_data::pms::HubState::Failed);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let (caption, kind, action) = status_read(snapshot.view()).unwrap();
    assert_eq!((caption.to_str().unwrap(), kind), ("Can\u{2019}t reach your Plex server", StatusKind::Failed));
    assert_eq!(action.unwrap().to_str().unwrap(), "Try again");
    let measure = plx_ui::fixture::FixtureMeasure;
    let no_offer = OfferWatch::default();
    let overlay = status_overlay(snapshot.view(), &no_offer, &ClockWatch::default()).unwrap();
    let verdict = overlay.verdict_band_measured(&measure);
    assert_eq!(verdict.y, StatusOverlay::FULL_ANCHOR_TOP);
    let drawn = overlay.action_frame_measured(&measure).unwrap();
    assert_eq!(drawn.y, verdict.y + verdict.h + plx_ui::theme::space::LG);
    let hit = screen(snapshot.view()).hero_button_rect(snapshot.view(), 0, &measure).unwrap();
    assert_eq!([hit.x, hit.y, hit.w, hit.h], [drawn.x, drawn.y, drawn.w, drawn.h]);
}

/// **A failed Home says why a wrong clock is the likely cause** — the clock glyph over the verdict
/// and a reason line in the reserved slot, only while key mode cannot help (`net::keypin::blocked`).
/// No fact is byte-for-byte today's read-out; only a Failed read-out takes the reason; an offered
/// plaintext server's reason outranks it; and the hit rect is the pill the SAME frame draws, the
/// reason having moved the action row.
#[test]
fn a_failed_home_names_a_wrong_clock_when_key_mode_cannot_help() {
    use plx_net::net::keypin::{self, Blocked};
    use plx_ui::icons::Icon;
    let _guard = plx_base::testlock::serial();
    plx_plex::plex::grant::reset_for_test();
    let key = keypin::key_of("home-clock.invalid", 32400);
    let _scoped = keypin::Scoped::watch_machine("home-clock-machine", &key);
    let _current = current_server_for_test("home-clock-machine");
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 0, plx_data::pms::HubState::Failed);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let view = snapshot.view();
    let mut s = screen(view);
    let measure = FixtureMeasure;

    step(&mut s, view, None, &ScreenEvent::Tick(Tick::default()));
    let plain = status_overlay(view, &s.plaintext, &s.clock).unwrap();
    assert_eq!((plain.reason, plain.glyph, plain.action), (None, Some(Icon::ServerBadgeMinus), Some(c"Try again")));
    let plain_row = plain.action_frame_measured(&measure).unwrap();
    let plain_caption = plain.caption.to_owned();

    keypin::strict_failure(&key, 60, Some(10));
    let before_tick = status_overlay(view, &s.plaintext, &s.clock).unwrap();
    assert_eq!(before_tick.reason, None, "the held fact does not change between ticks");
    step(&mut s, view, None, &ScreenEvent::Tick(Tick::default()));
    let overlay = status_overlay(view, &s.plaintext, &s.clock).unwrap();
    assert_eq!(overlay.reason, Some(plx_platform::i18n::msg::browse_clock_no_key_c()));
    assert_eq!((overlay.glyph, overlay.action), (Some(Icon::ClockBadgeAlert), Some(c"Try again")));
    assert_eq!(overlay.caption, plain_caption.as_c_str(), "the verdict is unchanged");
    let drawn = overlay.action_frame_measured(&measure).unwrap();
    assert!(drawn.y > plain_row.y, "the reason moves the action row");
    let hit = s.hero_button_rect(view, 0, &measure).unwrap();
    assert_eq!([hit.x, hit.y, hit.w, hit.h], [drawn.x, drawn.y, drawn.w, drawn.h], "the hit rect is the drawn pill");

    keypin::key_changed(&key);
    step(&mut s, view, None, &ScreenEvent::Tick(Tick::default()));
    let overlay = status_overlay(view, &s.plaintext, &s.clock).unwrap();
    assert_eq!(s.clock.blocked(), Some(Blocked::KeyChanged));
    assert_eq!(overlay.reason, Some(plx_platform::i18n::msg::browse_clock_key_changed_c()));

    // A read-out that has not failed takes no reason, whatever the fact says.
    for hub_state in [plx_data::pms::HubState::Loading, plx_data::pms::HubState::Ready] {
        plx_data::pms::seed_for_test(&mut state, &adapter, 0, hub_state);
        let snapshot = plx_data::pms::hubs_snapshot(&state);
        let overlay = status_overlay(snapshot.view(), &s.plaintext, &s.clock).unwrap();
        assert_eq!((overlay.reason, overlay.glyph), (None, None), "{hub_state:?}");
    }

    // The plaintext offer's reason (and its glyph) win: its own cause is the one the person can act on.
    let verdict = plx_plex::plex::grant::PlaintextVerdict {
        machine_id: "lan-machine".into(), name: "Home".into(), shared_by: String::new(),
        eligibility: plx_plex::plex::probe::PlaintextEligibility::Eligible,
        choice: plx_plex::plex::session::PlaintextChoice::Undecided,
    };
    plx_plex::plex::grant::offered(plx_plex::plex::grant::scope(), verdict.clone());
    step(&mut s, view, None, &ScreenEvent::Tick(Tick::default()));
    let overlay = status_overlay(view, &s.plaintext, &s.clock).unwrap();
    let offer = plx_session::auth::plaintext_copy(Some(&verdict), plx_session::auth::ReadoutSurface::SignedIn);
    assert_eq!(overlay.reason.and_then(|r| r.to_str().ok()), Some(offer.as_ref()));
    assert_eq!((overlay.glyph, overlay.action), (Some(Icon::ServerBadgeMinus), Some(plaintext_question::connect())));
    plx_plex::plex::grant::reset_for_test();
}

/// Make `machine` the current server (the one a failed Home speaks about) for a test, and put the
/// registry back when it ends.
fn current_server_for_test(machine: &str) -> impl Drop {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            plx_plex::plex::reset_servers_for_test();
        }
    }
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_pinned_with_client_id(machine, &plx_plex::plex::Origin::http("192.168.1.53", 32400),
        "", None, "client", Default::default());
    assert!(plx_plex::plex::set_current(sid) || plx_plex::plex::current_server() == sid, "the test server is current");
    Reset
}

/// **A fact about ANOTHER server does not colour a failed Home** (scenario B): Home asks
/// `net::keypin::blocked_for` about the current server only.
#[test]
fn a_failed_home_ignores_a_clock_fact_about_another_server() {
    use plx_net::net::keypin;
    let _guard = plx_base::testlock::serial();
    plx_plex::plex::grant::reset_for_test();
    let elsewhere = keypin::key_of("home-elsewhere.invalid", 32400);
    let _scoped = keypin::Scoped::watch_machine("home-elsewhere-machine", &elsewhere);
    let _current = current_server_for_test("home-here-machine");
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 0, plx_data::pms::HubState::Failed);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let view = snapshot.view();
    let mut s = screen(view);

    keypin::strict_failure(&elsewhere, 60, Some(10));
    keypin::key_changed(&elsewhere);
    step(&mut s, view, None, &ScreenEvent::Tick(Tick::default()));
    assert_eq!(s.clock.blocked(), None);
    let overlay = status_overlay(view, &s.plaintext, &s.clock).unwrap();
    assert_eq!((overlay.reason, overlay.glyph), (None, Some(plx_ui::icons::Icon::ServerBadgeMinus)));
}

// The observer consumes the same final geometry as widgets::card; the screen
// supplies no independent motion signal that could omit one of these terms.
fn observe_card(h: &mut plx_ui::card_motion::History, s: &HomeScreen, view: HubsView<'_>, ms: u32) -> plx_ui::card_motion::Verdict {
    h.begin();
    h.observe(plx_ui::card_motion::Identity { owner: 1, asset: 1 }, drawn_rect(s, view, None, 0, 0), ms)
}

#[test]
fn a_retained_shelf_offset_makes_the_late_dive_read_as_fast() {
    use plx_ui::card_motion::{History, Verdict};
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let late_dive_frame = |offset: f32| {
        let mut s = screen(snapshot.view());
        s.snap.jump(0.9);
        s.snap_target = 1.0;
        s.grid.shelves[0].restore_scroll(offset, ui_cards::MAX_ROW_ITEMS);
        s.layout_grid();
        let mut h = History::default();
        assert_eq!(observe_card(&mut h, &s, snapshot.view(), 0), Verdict::Unknown);
        step(&mut s, snapshot.view(), None, &ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 }));
        observe_card(&mut h, &s, snapshot.view(), 16)
    };
    assert_eq!(late_dive_frame(0.0), Verdict::Settled, "vertical late-dive control is under 120px/s");
    assert_eq!(late_dive_frame(4000.0), Verdict::Moving, "the retained-offset product moves the card fast");
}

#[test]
fn the_hero_to_grid_dive_is_observed_from_card_placement_and_then_settles() {
    use plx_ui::card_motion::{History, Verdict};
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    let mut h = History::default();
    s.snap.jump(0.0);
    s.snap_target = 1.0;
    s.layout_grid();
    observe_card(&mut h, &s, snapshot.view(), 0);
    step(&mut s, snapshot.view(), None, &ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 }));
    assert!(s.snap.vel.abs() < 10.0, "the spring remains a dimensionless fraction");
    assert_eq!(observe_card(&mut h, &s, snapshot.view(), 16), Verdict::Moving);
    let mut last = Verdict::Unknown;
    for i in 2..602 {
        step(&mut s, snapshot.view(), None, &ScreenEvent::Tick(Tick { ms: i * 16, dt_us: 16_667 }));
        last = observe_card(&mut h, &s, snapshot.view(), i * 16);
    }
    assert_eq!(last, Verdict::Settled);
}

#[test]
fn no_shelves_means_no_grid_snap() {
    assert_eq!(pinned_snap(1.0, 0), 0.0);
    assert_eq!(pinned_snap(0.0, 0), 0.0);
    assert_eq!(pinned_snap(1.0, 1), 1.0);
    assert_eq!(pinned_snap(0.0, 1), 0.0);
    assert_eq!(pinned_snap(1.0, SWEEP_ROWS), 1.0);
}

#[test]
fn the_status_screen_takes_ok_but_never_the_top_band() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 0, plx_data::pms::HubState::Failed);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    let entry = s.entry;
    let (_, retry, _) = step(
        &mut s,
        snapshot.view(),
        Some(FocusKey {
            entry,
            elem: HERO_PLAY_ELEM,
        }),
        &ScreenEvent::Activate(HERO_PLAY_ELEM),
    );
    assert!(retry.iter().any(|stamped| matches!(
        &stamped.fx,
        Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Retry)))
    )));
    for elem in [STRIP_HOME_ELEM, STRIP_MOVIES_ELEM, STRIP_SHOWS_ELEM, STRIP_SEARCH_ELEM, STRIP_ACCOUNT_ELEM] {
        let (_, out, _) = step(&mut s, snapshot.view(), None, &ScreenEvent::Activate(elem));
        let expected = HomeScreen::request_for_strip(elem).unwrap();
        assert!(has_home(&out, |r| *r == expected));
        assert!(!out.iter().any(|s| matches!(&s.fx,
            Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Retry))))));
    }
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Failed);
    let populated = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(populated.view());
    let (_, out, _) = step(&mut s, populated.view(), None, &ScreenEvent::Activate(HERO_PLAY_ELEM));
    assert!(has_home(&out, |r| matches!(r, HomeReq::Play { .. })));
    assert!(!out.iter().any(|s| matches!(&s.fx,
        Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Retry))))));
}

#[test]
fn pointer_hit_column_matches_the_drawn_card_at_every_snap_phase() {
    for scroll in [0.0, 415.0, 830.0] {
        for snap in [0.0, 0.37, 1.0] {
            let effective = scroll * snap;
            for col in 0..24 {
                assert_eq!(
                    col_at(card_x(col, effective) + CARD_W * 0.5, effective, 24),
                    Some(col)
                );
            }
        }
    }
}

#[test]
fn drawn_hero_geometry_follows_slide_and_the_captured_press_scale() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.outgoing = s.carousel.clone();
    assert!(s.outgoing.is_some());
    for _ in 0..80 { s.hero_pop.step(Some(0), 0.016); }
    let key = FocusKey { entry: s.entry, elem: HERO_PLAY_ELEM };
    for dir in [-1.0, 1.0] {
        s.hero_dir = dir;
        for slide in [0.3, 0.8, 1.0] {
            s.hero_slide.jump(slide);
            for press in [0.93, 1.0, 1.025] {
                let mut context = cx(snapshot.view(), Some(key));
                context.press.scale = press;
                let base = s.hero_button_rect(snapshot.view(), 0, context.measure).unwrap();
                let drawn = Focusable::<TestHost>::place(&s, &key.elem, &context, At::Drawn).unwrap();
                let target = Focusable::<TestHost>::place(&s, &key.elem, &context, At::SpringTarget).unwrap();
                assert!((drawn.rect.cx() - base.cx() - dir * (1.0 - slide) * SCR_W).abs() < 0.01);
                assert!((drawn.rect.w - base.w * plx_ui::widgets::CTRL_FOCUS_SCALE * press).abs() < 0.01);
                assert!((target.rect.cx() - base.cx()).abs() < 0.01);
                if dir == 1.0 && slide == 0.8 {
                    let mut frame = DrawFrame::new(&context, Painter::root());
                    s.record_stops(&mut frame, snapshot.view());
                    let mut map = HitMap::new();
                    map.fill(frame.into_stops());
                    map.swap();
                    assert_eq!(map.resolve(Some(s.entry), PointerKind::Click,
                        drawn.rect.cx(), drawn.rect.cy(), Some(key)).hit, Some(key));
                    assert_eq!(map.resolve(Some(s.entry), PointerKind::Click,
                        base.cx(), base.cy(), Some(key)).hit, None);
                }
            }
        }
    }
}

#[test]
fn status_action_geometry_does_not_inherit_the_previous_hero_pop_or_slide() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 0, plx_data::pms::HubState::Failed);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    for _ in 0..80 { s.hero_pop.step(Some(0), 0.016); }
    s.outgoing = Some((plx_plex::plex::ServerId::UNSET, "old".into()));
    s.hero_slide.jump(0.5);
    let key = FocusKey { entry: s.entry, elem: HERO_PLAY_ELEM };
    let mut context = cx(snapshot.view(), Some(key));
    context.press.scale = 0.93;
    let base = s.hero_button_rect(snapshot.view(), 0, context.measure).unwrap();
    let drawn = Focusable::<TestHost>::place(&s, &key.elem, &context, At::Drawn).unwrap();
    assert_eq!((drawn.rect.x, drawn.rect.y, drawn.rect.w, drawn.rect.h),
        (base.x, base.y, base.w, base.h));
}

#[test]
fn drawn_card_geometry_includes_press_but_its_rest_anchor_does_not() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    let key = first_card(&s);
    pop_card(&mut s, snapshot.view(), key, 80);
    s.layout_grid();
    for press in [0.93, 1.0, 1.025] {
        let mut context = cx(snapshot.view(), Some(key));
        context.press.scale = press;
        let placed = Focusable::<TestHost>::place(&s, &key.elem, &context, At::Drawn).unwrap();
        assert!((placed.rect.w - CARD_W * pop_of(&s, snapshot.view(), Some(key), 0, 0) * press).abs() < 0.01);
        assert!((placed.rest_rect.w - CARD_W * RowStyle::HOME.focus_scale).abs() < 0.01);
        context.press = PressRead::default();
        let opener = s.focused_rect(Some(key), &context, At::Drawn).unwrap();
        assert!((opener.w - CARD_W * pop_of(&s, snapshot.view(), Some(key), 0, 0)).abs() < 0.01);
    }
}

#[test]
fn the_card_anchor_rect_tracks_the_drawn_card_through_scroll_and_snap() {
    for &scroll in &[0.0f32, 415.0, 830.0] {
        for &sp in &[0.0f32, 0.37, 1.0] {
            let es = scroll * sp;
            for c in 0..8usize {
                for &focus in &[1.0f32, RowStyle::HOME.focus_scale] {
                    let base = Rect::new(card_x(c, es), 176.0 + CARD_DY, CARD_W, CARD_H);
                    let r = base.scaled(focus);
                    // magnification is about the card's CENTRE — the anchor must not slide
                    assert!(
                        (r.cx() - base.cx()).abs() < 0.001
                            && (r.cy() - base.cy()).abs() < 0.001,
                        "card {c} (scroll={scroll}, sp={sp}, focus={focus}) moved its centre"
                    );
                    // …and the rect the panel is placed beside is the one the pointer would hit
                    assert_eq!(
                        col_at(r.cx(), es, 8),
                        Some(c),
                        "anchor centre for card {c} (scroll={scroll}, sp={sp}) is not over that card"
                    );
                    assert!(
                        r.w >= CARD_W && r.h >= CARD_H,
                        "focus must not shrink the card"
                    );
                }
            }
        }
    }
}

fn blurred(blur: [[f32; 3]; 4]) -> PmsMovie {
    PmsMovie {
        has_blur: true,
        blur,
        ..Default::default()
    }
}

#[test]
fn the_wash_floors_on_the_app_surface_and_never_dims_toward_black() {
    let flat = [theme::SURFACE_APP; 4];
    // "no artwork" IS the app's own ground, with no special case
    assert_eq!(
        wash_corners(None, flat, 0.0),
        flat,
        "an empty pool is the app's ground"
    );
    assert_eq!(
        wash_corners(Some(&PmsMovie::default()), flat, 0.0),
        flat,
        "…and so is an item with no envelope"
    );
    // the grid end of the snap lands on exactly the colour `frame_clear` already laid down —
    // where the old `dim` ramp landed on black
    let bright = blurred([
        [0.95, 0.93, 0.90],
        [1.0, 1.0, 1.0],
        [0.8, 0.75, 0.2],
        [0.1, 0.1, 0.12],
    ]);
    assert_eq!(
        wash_corners(Some(&bright), flat, 1.0),
        flat,
        "the dive resolves to the app ground, not to black"
    );
    assert_eq!(
        wash_corners(Some(&bright), flat, 1.5),
        flat,
        "…and an overshooting snap cannot push past it"
    );

    // **The grid end held at the app's own surface, which is what makes every assertion below
    // hold VERBATIM through the change that gave this function a second subject.** With
    // `grid = SURFACE_APP` the lerp `mix(hero_t, SURFACE, sp)` is algebraically the old
    // `keyed(blur, W * (1 - sp))` weight ramp, corner for corner — so these are still the same
    // measurements of the same behaviour, not loosened ones. The grid end's own bound is
    // `the_grid_end_of_the_dive_is_the_focused_tiles_ground` below.
    const FLAT_GRID: [[f32; 4]; 4] = [theme::SURFACE_APP; 4];

    // The tail of the dive — the exact frames that used to be near-black — for a DARK envelope
    // as much as a bright one. The bound is the one invariant that separates a WEIGHT scale
    // from a brightness ramp: a corner can only travel as far from the app surface as the lean
    // weight STILL IN PLAY allows it to, so the deviation is capped by that corner's own
    // remaining weight times the furthest a mix could take it (toward white above the surface,
    // toward black below it). Do not loosen this to a flat percentage — the whole point is that
    // it tightens to nothing as `sp → 1`, which is what pins the grid end of the snap.
    //
    // For scale: at sp=0.9 this allows ±0.046 on the two top corners, and the old `dim` ramp
    // put them at 0.010 — a deviation of 0.163, off by three and a half times the budget.
    for m in [&bright, &blurred([[0.0; 3]; 4])] {
        for &sp in &[0.9f32, 0.99, 0.996] {
            for (c, w) in wash_corners(Some(m), FLAT_GRID, sp).iter().zip(HERO_WASH_W) {
                let lean = w * (1.0 - sp);
                for (ch, s) in c.iter().zip(theme::SURFACE_APP) {
                    let budget = lean * s.max(1.0 - s);
                    assert!(
                        (ch - s).abs() <= budget + 1e-6,
                        "at sp={sp} the ground is {ch} against a {s} surface — that is {} off a \
                         {budget} budget, so the old dim ramp is back",
                        (ch - s).abs()
                    );
                }
                assert_eq!(c[3], 1.0, "a wash corner is opaque");
            }
        }
    }

    // the lean is a WEIGHT SCALE on the shared mix, not a brightness scale over it. Re-deriving
    // it as `dim(keyed(blur, W), lean)` would darken the surface itself and fails here.
    //
    // Compared to a float epsilon rather than exactly, because the two spellings of the same
    // algebra — the weight ramp on the right, the lerp between two finished targets on the left
    // — associate their multiplies differently and land up to one ULP apart. The failure this
    // guards against is a brightness scale, which is wrong by three and a half TIMES the budget
    // asserted above, so an ULP of slack costs it nothing.
    for (c, w) in wash_corners(Some(&bright), FLAT_GRID, 0.5)
        .iter()
        .zip(AmbientWash::keyed(bright.blur, HERO_WASH_W.map(|w| w * 0.5)))
    {
        for (a, b) in c.iter().zip(w) {
            assert!(
                (a - b).abs() <= 1e-6,
                "half-dived is half the lean toward the artwork ({b}), not half the \
                 brightness ({a})"
            );
        }
    }
    // …and the lean does go TOWARD the artwork: a bright envelope lifts the panel off the
    // surface rather than sinking it.
    let lit = wash_corners(Some(&bright), FLAT_GRID, 0.0);
    assert!(
        lit[1][0] > theme::SURFACE_APP[0],
        "a white corner must brighten the ground it keys"
    );
    assert!(
        lit[1][0] < 1.0,
        "…but never all the way: it is a wash, not the photograph"
    );
}

#[test]
fn the_grid_end_of_the_dive_is_the_focused_tiles_ground() {
    let hero = blurred([[0.95, 0.1, 0.1]; 4]); // red billboard
    let tile = AmbientWash::keyed([[0.1, 0.1, 0.95]; 4], PageGround::CARD_W); // blue shelf tile
    // A lerp evaluated at its own endpoints lands one ULP off them, so these are graded to a
    // float epsilon. The failure they guard against is a wash carrying the WRONG SUBJECT'S
    // colours, which is a whole ground apart.
    let same = |got: [[f32; 4]; 4], want: [[f32; 4]; 4], what: &str| {
        for (c, w) in got.iter().zip(want) {
            for (a, b) in c.iter().zip(w) {
                assert!((a - b).abs() <= 1e-6, "{what}: got {a}, wanted {b}");
            }
        }
    };

    // at the top the grid end contributes NOTHING…
    same(
        wash_corners(Some(&hero), tile, 0.0),
        AmbientWash::keyed(hero.blur, HERO_WASH_W),
        "the billboard is the hero's alone",
    );
    // …and in the grid the hero contributes nothing.
    same(
        wash_corners(Some(&hero), tile, 1.0),
        tile,
        "the shelves are the focused tile's alone",
    );
    // An overshooting snap cannot push past the tile's own ground either — the clamp that used
    // to stop the lean going negative now stops the lerp extrapolating past the far end.
    same(
        wash_corners(Some(&hero), tile, 1.5),
        tile,
        "an overshooting snap",
    );

    // The grid end is still floored on the app surface, which is what keeps "no artwork" the
    // flat clear down here exactly as it is up top — and keeps `is_flat` able to skip the pass
    // on a library with no envelopes at all.
    same(
        wash_corners(Some(&hero), [theme::SURFACE_APP; 4], 1.0),
        [theme::SURFACE_APP; 4],
        "an artless shelf is the app's own ground, not a leftover hero tint",
    );

    // Halfway is halfway between the two grounds — a lerp, not a sum. Summing the two leans
    // would make the middle of the dive the BRIGHTEST part of the animation, which is the one
    // place neither subject is the page's.
    for (c, (h, g)) in wash_corners(Some(&hero), tile, 0.5)
        .iter()
        .zip(AmbientWash::keyed(hero.blur, HERO_WASH_W).iter().zip(tile))
    {
        for (i, v) in c.iter().enumerate() {
            let want = h[i] + (g[i] - h[i]) * 0.5;
            assert!(
                (v - want).abs() <= 1e-6,
                "mid-dive corner channel is {v}, halfway between the two grounds is {want}"
            );
        }
    }
}

#[test]
fn prefetch_order_wraps_and_never_warms_the_page_on_screen() {
    let mut out = [0 as i32; 2 * HERO_PREFETCH];
    assert_eq!(
        prefetch_order(0, 0, &mut out),
        0,
        "an empty pool has no neighbours"
    );
    assert_eq!(
        prefetch_order(0, 1, &mut out),
        0,
        "…and neither does a one-page pool"
    );
    assert_eq!(
        prefetch_order(0, 2, &mut out),
        1,
        "in a two-page pool +1 and -1 are the same page"
    );
    assert_eq!(out[0], 1);

    for n in 2..=8 as i32 {
        for cur in 0..n {
            let k = prefetch_order(cur, n, &mut out);
            assert!(
                k <= 2 * HERO_PREFETCH,
                "wrote {k} entries into a {}-slot buffer",
                2 * HERO_PREFETCH
            );
            assert_eq!(
                out[0],
                (cur + 1).rem_euclid(n),
                "forward first (n={n}, cur={cur})"
            );
            for i in 0..k {
                assert!(
                    (0..n).contains(&out[i]),
                    "page {} is outside a {n}-page pool",
                    out[i]
                );
                assert_ne!(
                    out[i], cur,
                    "warmed the page already on screen (n={n}, cur={cur})"
                );
                assert!(!out[..i].contains(&out[i]), "warmed page {} twice", out[i]);
            }
        }
    }
}

#[test]
fn the_prefetch_is_armed_only_from_a_settled_hero() {
    assert!(prefetch_armed(0.0, false), "billboard up, nothing moving");
    assert!(
        !prefetch_armed(0.0, true),
        "mid-flip, the incoming layer IS the thing being waited on"
    );
    for &sp in &[0.05f32, 0.2, 1.0] {
        assert!(
            !prefetch_armed(sp, false),
            "at sp={sp} the billboard is on its way out"
        );
    }
}

#[test]
fn the_ground_is_skipped_only_when_opaque_art_covers_it() {
    assert!(
        wash_hidden(0.0, 1.0, None),
        "one fully revealed layer at rest covers the panel"
    );
    assert!(wash_hidden(0.0, 1.0, Some(1.0)), "…and so do two, mid-flip");
    assert!(
        !wash_hidden(0.0, 0.7, None),
        "a layer still dissolving in shows the ground through"
    );
    assert!(
        !wash_hidden(0.0, 1.0, Some(0.7)),
        "…and so does the OTHER layer of a flip"
    );
    assert!(
        !wash_hidden(0.0, 0.0, None),
        "no art at all is exactly what the ground is for"
    );
    // the snap both fades the art by (1 - sp) and slides it up off the bottom of the panel, so
    // full reveals do not mean coverage once the dive has begun
    assert!(
        !wash_hidden(0.2, 1.0, Some(1.0)),
        "a diving hero uncovers the panel however revealed its art is"
    );
    assert!(
        !wash_hidden(1.0, 1.0, None),
        "the grid shows the ground (which by then is the flat surface)"
    );
}

#[test]
fn the_home_hero_logo_never_reaches_the_top_bar() {
    const META_H: f32 = 28.0 * 1.32;
    let synopsis = plx_ui::hero_syn_h(plx_ui::HERO_SYN_MAXLINES);
    let band = hero_logo::band_h(LogoRung::Hero);
    let top = hero_stack_top(band, META_H, theme::space::SM + synopsis);
    assert!(top - (theme::logo::HERO_H_MAX - band) > plx_ui::widgets::TOP_BAR_BOTTOM);
}

#[test]
fn the_hero_logo_key_is_the_shows_for_an_episode() {
    let ep = PmsMovie {
        kind: 3,
        rk: "42".into(),
        show_rk: "7".into(),
        ..Default::default()
    };
    assert_eq!(
        hero_logo_rk(&ep),
        "7",
        "an episode's hero wears the show's logotype"
    );
    let orphan = PmsMovie {
        kind: 3,
        rk: "42".into(),
        ..Default::default()
    };
    assert_eq!(
        hero_logo_rk(&orphan),
        "42",
        "…falling back to its own when the server sent no parent"
    );
    let movie = PmsMovie {
        kind: 0,
        rk: "42".into(),
        show_rk: "7".into(),
        ..Default::default()
    };
    assert_eq!(
        hero_logo_rk(&movie),
        "42",
        "a movie is never keyed to a stray parent"
    );
}

#[test]
fn the_continue_watching_caption_promises_time_left_only_when_the_bar_is_drawn() {
    let ep = |resume_ms: i64| PmsMovie {
        kind: 3,
        dur_ns: 45 * 60 * 1_000_000_000,
        resume_ms,
        show_title: "Laura".into(),
        ..Default::default()
    };
    // never started, stopped exactly at the end, and a stale offset PAST it
    for m in [ep(0), ep(45 * 60_000), ep(60 * 60_000)] {
        assert!(
            m.resume_frac().is_none(),
            "offset {} is not in progress",
            m.resume_ms
        );
        let cap = ui_cards::focused_caption(&tile_facts::of(&m), true).expect("a Continue Watching episode always captions");
        assert!(
            !cap.to_str().unwrap().contains("left"),
            "offset {}: no bar, so the caption must not promise time remaining ({cap:?})",
            m.resume_ms
        );
    }
    let mid = ep(20 * 60_000);
    assert!(
        mid.resume_frac().is_some(),
        "20 minutes into 45 IS in progress"
    );
    assert_eq!(
        ui_cards::focused_caption(&tile_facts::of(&mid), true).unwrap().to_str().unwrap(),
        "Laura \u{00b7} 25 min left"
    );
}

fn top_band_bottom() -> f32 {
    let chip = plx_ui::widgets::CHIP_CAP_MAX;
    assert_eq!(chip.y + chip.h, plx_ui::widgets::TOP_BAR_BOTTOM,
        "the chip capsule and the tab track are one band");
    chip.y + chip.h
}
fn heading_top(row: usize, focus_row: usize, scroll: f32) -> f32 {
    let lift = if row == focus_row {
        ui_cards::heading_lift_max(&RowStyle::HOME)
    } else {
        0.0
    };
    heading_y(
        GRID_TOP_Y + shelf_top_settled(row, focus_row) - scroll,
        lift,
    )
}
fn settled_scroll(rows: usize, focus_row: usize, current: f32) -> f32 {
    let (lo, hi) = row_reveal_band(shelf_top_settled(focus_row, focus_row));
    ui_cards::reveal(current, lo, hi, grid_max_scroll(rows))
}
fn from_below(rows: usize) -> f32 {
    grid_max_scroll(rows) + ROW_PITCH
}

#[test]
fn the_first_shelfs_raised_heading_settles_clear_of_the_profile_chip() {
    assert!(heading_top(0, 0, settled_scroll(5, 0, from_below(5))) >= top_band_bottom());
}

#[test]
fn no_shelf_heading_settles_inside_the_shared_top_band() {
    const HEADING_MAX_H: f32 = 2.0 * theme::size::HEADLINE as f32;
    for rows in 1..=SWEEP_ROWS {
        for focus_row in 0..rows {
            let scrolls = [
                settled_scroll(rows, focus_row, 0.0),
                settled_scroll(rows, focus_row, from_below(rows)),
            ];
            for row in 0..rows {
                let ys = [
                    heading_top(row, focus_row, scrolls[0]),
                    heading_top(row, focus_row, scrolls[1]),
                ];
                let (lo, hi) = (ys[0].min(ys[1]), ys[0].max(ys[1]));
                assert!(lo >= top_band_bottom() || hi + HEADING_MAX_H <= top_band_bottom());
            }
        }
    }
}

#[test]
fn every_settled_row_keeps_its_focused_label_block_above_the_overscan_bottom() {
    for rows in 1..=SWEEP_ROWS {
        for focus_row in 0..rows {
            for scroll in [
                settled_scroll(rows, focus_row, 0.0),
                settled_scroll(rows, focus_row, from_below(rows)),
            ] {
                let row_y = GRID_TOP_Y + shelf_top_settled(focus_row, focus_row) - scroll;
                assert!(row_y + CARD_DY + CARD_H + ui_cards::UNDER_LABEL_H <= SCR_H - MARGIN_Y);
            }
        }
    }
}

#[test]
fn the_grids_resting_top_is_the_highest_a_shelf_may_settle() {
    assert_eq!(row_reveal_band(0.0).1, 0.0);
    assert_eq!(
        row_reveal_band(shelf_top_settled(3, 3)).1,
        shelf_top_settled(3, 3)
    );
    assert!(heading_top(0, 0, 0.0) - top_band_bottom() >= theme::space::XS);
}

struct Run {
    text: String,
    dx: f32,
    sz: i32,
    bold: i32,
    ink: [f32; 4],
}
fn width_of(text: &str, size: i32, bold: i32) -> f32 {
    text.chars().count() as f32 * (size as f32 + 6.0 * bold as f32)
}
fn heading_flow(title: &str, source: &str) -> (f32, Vec<Run>) {
    let mut runs = Vec::new();
    let width = ui_cards::heading_flow(title, source, |text, dx, size, bold, ink| {
        runs.push(Run {
            text: text.into(),
            dx,
            sz: size,
            bold,
            ink,
        });
        width_of(text, size, bold)
    });
    (width, runs)
}

#[test]
fn a_shelf_with_no_source_draws_exactly_the_title_and_nothing_else() {
    let (w, runs) = heading_flow("Recently Added", "");
    assert_eq!(
        runs.len(),
        1,
        "an empty source must produce no further runs at all"
    );
    assert_eq!(runs[0].text, "Recently Added");
    assert_eq!(runs[0].dx, 0.0, "the title starts at the heading origin");
    assert_eq!(
        w,
        width_of("Recently Added", theme::size::HEADLINE, 1),
        "the title's own advance, exactly"
    );
}

#[test]
fn a_shared_source_extends_the_heading_past_the_title() {
    let (bare, _) = heading_flow("Recently Added in Film Club", "");
    let (annotated, runs) = heading_flow("Recently Added in Film Club", "friend");
    assert!(
        annotated > bare,
        "the annotation must extend the heading ({annotated} vs {bare})"
    );
    assert_eq!(runs.len(), 3, "title, separator, handle");
    assert_eq!(runs[0].dx, 0.0, "the title still starts at the origin");
    assert_eq!(
        annotated,
        bare + 2.0 * SOURCE_PAD
            + width_of("\u{b7}", theme::size::BODY, 0)
            + width_of("friend", theme::size::BODY, 0),
        "the growth is exactly the dot, the handle and one pad either side of the dot"
    );
    assert_eq!(
        runs[1].dx,
        bare + SOURCE_PAD,
        "the dot is one pad past the title"
    );
    assert_eq!(
        runs[2].dx,
        runs[1].dx + width_of("\u{b7}", theme::size::BODY, 0) + SOURCE_PAD,
        "the handle is one pad past the dot"
    );
}

#[test]
fn each_heading_run_carries_its_own_size_weight_and_ink() {
    let (_, runs) = heading_flow("Recently Added in Film Club", "friend");
    assert_eq!(
        (runs[0].sz, runs[0].bold),
        (theme::size::HEADLINE, 1),
        "the title is HEADLINE bold"
    );
    assert_eq!(
        runs[0].ink,
        theme::TEXT_HEADING,
        "…in the shared section-heading ink"
    );
    assert_eq!(runs[1].text, "\u{b7}");
    assert_eq!(
        (runs[1].sz, runs[1].bold),
        (theme::size::BODY, 0),
        "the separator is measured at BODY regular"
    );
    assert_eq!(
        runs[1].ink,
        theme::TEXT_SEPARATOR,
        "…at the separator token's own .45"
    );
    assert_eq!(runs[2].text, "friend");
    assert_eq!(
        (runs[2].sz, runs[2].bold),
        (theme::size::BODY, 0),
        "the handle is BODY regular, not the title's"
    );
    assert_eq!(runs[2].ink, theme::TEXT_TERTIARY);
    assert!(
        (runs[1].sz, runs[1].bold) == (runs[2].sz, runs[2].bold),
        "the dot and the handle are one annotation: same rung, same weight, so they drop onto the title's baseline together"
    );
    assert!(runs[1].sz < runs[0].sz, "the annotation is a rung DOWN from the title — the drop is why it must be baseline-aligned");
}

struct MetaRun {
    text: String,
    dx: f32,
    budget: f32,
    sz: i32,
    bold: i32,
    ink: [f32; 4],
}
fn meta_flow(base: f32, source: &str) -> (f32, Vec<MetaRun>) {
    let mut runs = Vec::new();
    let width = meta_source_flow(base, source, |text, dx, budget, size, bold, ink| {
        runs.push(MetaRun {
            text: text.into(),
            dx,
            budget,
            sz: size,
            bold,
            ink,
        });
        width_of(text, size, bold).min(budget.max(0.0))
    });
    (width, runs)
}

#[test]
fn a_hero_from_our_own_server_draws_no_source_run_at_all() {
    let (w, runs) = meta_flow(420.0, "");
    assert!(
        runs.is_empty(),
        "an empty source must produce no runs, no pad and no dot"
    );
    assert_eq!(w, 420.0, "…and must not move the line's own end by a pixel");
}

#[test]
fn a_borrowed_hero_states_its_owner_as_the_last_run_on_the_line() {
    let base = 420.0;
    let (w, runs) = meta_flow(base, "friend");
    assert_eq!(runs.len(), 2, "the separator and the run, and nothing else");
    assert_eq!(runs[0].text, "\u{b7}");
    assert_eq!(
        runs[0].dx,
        base + SOURCE_PAD,
        "the dot is one pad past the facts"
    );
    assert_eq!(
        runs[1].text, "Shared by friend",
        "the person, not the machine"
    );
    assert_eq!(
        runs[1].dx,
        runs[0].dx + width_of("\u{b7}", theme::size::BODY, 0) + SOURCE_PAD,
        "the run is one pad past the dot"
    );
    assert_eq!(
        w,
        base + 2.0 * SOURCE_PAD
            + width_of("\u{b7}", theme::size::BODY, 0)
            + width_of("Shared by friend", theme::size::BODY, 0),
        "the line grows by exactly the dot, the run and one pad either side of the dot"
    );
}

#[test]
fn the_hero_source_run_keeps_the_lines_rung_and_takes_one_step_of_ink() {
    let (_, runs) = meta_flow(420.0, "friend");
    for r in &runs {
        assert_eq!(
            (r.sz, r.bold),
            (theme::size::BODY, 0),
            "'{}' left the meta line's own rung",
            r.text
        );
        assert_ne!(
            r.sz,
            theme::size::CAPTION,
            "the hero line is BODY — it is not E's line and must not shrink to it"
        );
    }
    assert_eq!(
        runs[0].ink,
        theme::TEXT_SEPARATOR,
        "the middot carries the separator token's own .45"
    );
    assert_eq!(
        runs[1].ink,
        theme::TEXT_TERTIARY,
        "one step under the line's TEXT_SECONDARY, never level with it"
    );
    assert_ne!(runs[1].ink, theme::TEXT_SECONDARY);
}

#[test]
fn an_over_long_handle_truncates_rather_than_wrapping() {
    let ridiculous = "a-very-long-plex-account-handle-that-nobody-would-ever-choose";
    for base in [0.0f32, 420.0, HERO_COL_W] {
        let (w, runs) = meta_flow(base, ridiculous);
        assert_eq!(
            runs.len(),
            2,
            "base {base}: a long handle is still ONE run — there is no second line to go to"
        );
        for r in &runs {
            assert!(
                r.budget > 0.0,
                "base {base}: '{}' was given no room at all ({})",
                r.text,
                r.budget
            );
            assert!(
                r.dx + r.budget <= META_FLOW_W + 1e-3,
                "base {base}: '{}' may elide past the bound",
                r.text
            );
        }
        assert!(
            w <= META_FLOW_W + 1e-3,
            "base {base}: the line ran to {w}, past its {META_FLOW_W} bound"
        );
    }
    // …and the room left at that worst case is a real annotation's worth rather than a stub —
    // a QUARTER of the whole line, which is the guard that a future widening of the hero column
    // (or a tightening of the bound) cannot quietly starve the run into a bare ellipsis. It is
    // stated as a share of the line and not in pixels of text because the synthetic metric here
    // is roughly twice the shipped font's advance; the share is a claim about the geometry,
    // which is the part the host can actually speak for.
    let (_, worst) = meta_flow(HERO_COL_W, "friend");
    assert!(
        worst[1].budget >= 0.25 * META_FLOW_W,
        "a meta line whose facts fill the column leaves the run only {} of {META_FLOW_W}",
        worst[1].budget
    );
}

#[test]
fn the_meta_lines_bound_keeps_the_run_inside_the_hero_wedge() {
    use plx_ui::widgets::hero_scrim_a;
    assert!(
        hero_scrim_a(HERO_META_R, 1.0) > 0.0,
        "the line ends where the wedge has already given up"
    );
    let wedge_end = (0..=SCR_W as i32)
        .map(|x| x as f32)
        .find(|&x| hero_scrim_a(x, 1.0) <= 0.0)
        .expect("the wedge must end inside the frame");
    assert!(
        HERO_META_R <= wedge_end - 0.2 * SCR_W,
        "the bound ({HERO_META_R}) must stay a fifth of the panel short of the wedge's end ({wedge_end})"
    );
    assert!(
        HERO_META_R > MARGIN_X + HERO_COL_W,
        "…and past the text column, or the run has nowhere to go"
    );
}

// Additional phase-8 ownership proofs: real engine movement, stable identity, effects and map.

#[test]
fn engine_links_hero_to_the_first_shelf_and_back() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let s = screen(snapshot.view());
    let owner = InputOwner::Entry(s.entry);
    let hero = FocusKey {
        entry: s.entry,
        elem: HERO_PLAY_ELEM,
    };
    let context = cx(snapshot.view(), Some(hero));
    let mut engine = FocusEngine::new();
    engine.set(owner, hero, Some(HERO_GROUP), By::Restore);
    let mut links = Vec::new();
    <HomeScreen as Screen<TestHost>>::links(&s, &mut links);
    let Outcome::Moved { to, .. } = engine.move_dir(owner, &s, &links, Dir::Down, &context) else {
        panic!("hero DOWN must move")
    };
    assert_eq!(to.elem, s.rows[0].elems[0]);
}

#[test]
fn down_from_the_first_shelf_chooses_the_next_shelf_not_the_folded_hero() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 2, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    let second_elem = s.elem_for(HomeItemIdentity::Item {
        hub: HomeHubIdentity::Key {
            sid: plx_plex::plex::ServerId::UNSET,
            key: "/hubs/second".into(),
        },
        sid: plx_plex::plex::ServerId::UNSET,
        rk: "second".into(),
    });
    s.rows.push(HubProjection {
        identity: HomeHubIdentity::Key {
            sid: plx_plex::plex::ServerId::UNSET,
            key: "/hubs/second".into(),
        },
        group: GroupId(FIRST_HUB_GROUP + 1),
        elems: vec![second_elem],
        link: None,
    });
    s.elem_at.insert(second_elem, (1, 0));
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    s.layout_grid();

    let first = first_card(&s);
    let owner = InputOwner::Entry(s.entry);
    let context = cx(snapshot.view(), Some(first));
    let mut engine = FocusEngine::new();
    engine.set(owner, first, Some(s.rows[0].group), By::Restore);
    let mut links = Vec::new();
    <HomeScreen as Screen<TestHost>>::links(&s, &mut links);
    let Outcome::Moved { to, .. } = engine.move_dir(owner, &s, &links, Dir::Down, &context) else {
        panic!("first-shelf DOWN must reach the second shelf");
    };
    assert_eq!(to.elem, second_elem);
}

#[test]
fn down_from_the_last_shelf_never_reenters_the_offscreen_hero() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 2, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    s.layout_grid();
    let last = FocusKey {
        entry: s.entry,
        elem: *s.rows[0].elems.last().unwrap(),
    };
    let owner = InputOwner::Entry(s.entry);
    let context = cx(snapshot.view(), Some(last));
    let mut engine = FocusEngine::new();
    engine.set(owner, last, Some(s.rows[0].group), By::Restore);
    let mut links = Vec::new();
    <HomeScreen as Screen<TestHost>>::links(&s, &mut links);
    assert_eq!(
        engine.move_dir(owner, &s, &links, Dir::Down, &context),
        Outcome::Nothing
    );
}

#[test]
fn repeated_item_keys_are_scoped_by_hub_identity() {
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    let a = HomeItemIdentity::Item {
        hub: HomeHubIdentity::ContinueWatching,
        sid: plx_plex::plex::ServerId::UNSET,
        rk: "7".into(),
    };
    let b = HomeItemIdentity::Item {
        hub: HomeHubIdentity::Key {
            sid: plx_plex::plex::ServerId::UNSET,
            key: "/hubs/new".into(),
        },
        sid: plx_plex::plex::ServerId::UNSET,
        rk: "7".into(),
    };
    let ka = s.elem_for(a.clone());
    assert_eq!(s.elem_for(a), ka);
    assert_ne!(s.elem_for(b), ka);
}

#[test]
fn unknown_provider_identity_is_explicitly_generation_scoped() {
    assert_ne!(
        HomeHubIdentity::Ephemeral {
            generation: 4,
            ordinal: 2
        },
        HomeHubIdentity::Ephemeral {
            generation: 5,
            ordinal: 2
        },
    );
}

#[test]
fn memory_round_trip_preserves_registries_and_carousel_identity() {
    let mut a = HomeScreen::new(EntryId(7), InstanceId(9));
    let hub = HomeHubIdentity::ContinueWatching;
    a.group_for(&hub);
    a.elem_for(HomeItemIdentity::Item {
        hub,
        sid: plx_plex::plex::ServerId::UNSET,
        rk: "42".into(),
    });
    a.carousel = Some((plx_plex::plex::ServerId::UNSET, "42".into()));
    a.strip_chosen = true;
    let memory = match <HomeScreen as Screen<TestHost>>::memory(&a) {
        PageMemory::Home(m) => m,
        _ => unreachable!(),
    };
    let mut b = HomeScreen::new(EntryId(7), InstanceId(10));
    b.restore(&memory);
    assert_eq!(b.groups, a.groups);
    assert_eq!(b.items, a.items);
    assert_eq!(b.carousel, a.carousel);
    assert_eq!(b.strip_chosen, a.strip_chosen);
}

#[test]
fn activation_across_the_snap_midpoint_is_not_a_canonical_collision() {
    let _guard = plx_base::testlock::serial();
    let _press = deck_press_is(plx_media::route::DeckPress::Play);
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 2, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut hero_picture = screen(snapshot.view());
    let mut grid_picture = screen(snapshot.view());
    for s in [&mut hero_picture, &mut grid_picture] {
        s.carousel = Some((plx_plex::plex::ServerId::UNSET, "2".into()));
        s.snap_target = 1.0;
        s.visible_activation = Some(HERO_PLAY_ELEM);
    }
    hero_picture.snap.jump(0.49);
    grid_picture.snap.jump(0.51);
    let card = first_card(&hero_picture);
    let event = ScreenEvent::PressCommit(plx_machine::machine::PressId(1));
    let (_, a, _) = step(&mut hero_picture, snapshot.view(), Some(card), &event);
    let (_, b, _) = step(&mut grid_picture, snapshot.view(), Some(card), &event);
    assert!(has_home(&a, |r| matches!(r, HomeReq::Play { rk, .. } if rk == "2")));
    assert!(has_home(&b, |r| matches!(r, HomeReq::Play { rk, .. } if rk == "1")));
    assert_ne!(hero_picture.hash(), grid_picture.hash(),
        "the same input activates different items, so these cannot be the same logical state");
}

#[test]
fn the_home_census_covers_input_motion_and_current_projection() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let baseline = screen(snapshot.view()).hash();
    let changes: &[fn(&mut HomeScreen, HubsView<'_>)] = &[
        |s, _| s.snap.vel = 1.0,
        |s, _| s.hero_slide.pos = 0.4,
        |s, _| s.hero_slide.vel = 1.0,
        |s, _| s.hero_dir = -1.0,
        |s, _| s.outgoing = Some((plx_plex::plex::ServerId::UNSET, "old".into())),
        |s, _| s.grid.scroll_y.vel = 1.0,
        |s, _| s.grid.scroll_target = 100.0,
        |s, _| s.rows[0].elems.swap(0, 1),
        |s, _| Arc::make_mut(&mut s.items)[0].last_row += 1,
        |s, _| Arc::make_mut(&mut s.items)[0].last_col += 1,
        |s, _| s.projected_generation = None,
        |s, _| s.hero_pop.step(Some(0), 0.016),
        |s, _| s.grid.shelves[0].set_base_y(1.0),
        // a pop spring (and the focus it follows) moved by the shelf's own events
        |s, v| { s.snap.jump(1.0); s.pop_shelf_for_test(v) },
    ];
    for (i, change) in changes.iter().enumerate() {
        let mut s = screen(snapshot.view());
        change(&mut s, snapshot.view());
        assert_ne!(s.hash(), baseline, "census omitted input-state variation {i}");
    }
    // The pop and scroll springs are hashed through the shelf itself: with the page on the shelves,
    // popping one card moves its canon bytes and nothing else is touched.
    let shelf_canon = |s: &HomeScreen| { let mut c = Canon::new(); s.grid.shelves[0].write(&mut c); c.finish() };
    let mut popped = screen(snapshot.view());
    popped.snap.jump(1.0);
    let rest = shelf_canon(&popped);
    popped.pop_shelf_for_test(snapshot.view());
    assert_ne!(shelf_canon(&popped), rest, "a pop spring is part of the canon");
    // These extents are part of SHAPE, not merely runtime sequence lengths.
    assert_eq!(HERO_NBTN, 2);
    assert_eq!(plx_ui::cards::MAX_ROW_ITEMS, 24);
}

/// `person` and `search` cap their shelves at the data layer's `pms::MAX_SHELF_ITEMS`; the card row
/// that draws them owns `ui::card_row::MAX_ROW_ITEMS` springs. The data layer cannot name `ui` and
/// `ui` cannot name the data layer, so there are two constants for one number and this is the only
/// place that sees both.
#[test]
fn the_data_shelf_cap_is_the_card_rows_capacity() {
    assert_eq!(plx_data::pms::MAX_SHELF_ITEMS, plx_ui::cards::MAX_ROW_ITEMS);
}

#[test]
fn paint_only_backdrop_and_spinner_state_do_not_change_the_canonical_hash() {
    let mut a = HomeScreen::new(EntryId(7), InstanceId(9));
    let mut b = HomeScreen::new(EntryId(7), InstanceId(9));
    b.status_ms = 900.0;
    b.backdrop.art.pos = 0.5;
    assert_eq!(a.hash(), b.hash());
    a.carousel = Some((plx_plex::plex::ServerId::UNSET, "a".into()));
    assert_ne!(a.hash(), b.hash());
}

#[test]
fn an_explicit_hero_reseat_keeps_the_fold_animation_unlike_page_restoration() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 2, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    let from = first_card(&s);
    let to = FocusKey { entry: s.entry, elem: HERO_PLAY_ELEM };
    step(&mut s, snapshot.view(), Some(to), &ScreenEvent::FocusMoved {
        from: Some(from), to, by: By::Restore,
    });
    assert_eq!(s.snap_target, 0.0);
    assert_eq!(s.snap.pos, 1.0, "fresh reseating still animates the door");
}

/// A restore arrives already popped, whether or not the page idled at the hero first: the shelves
/// slept through those ticks, and the wake must not turn the restore into a deliberate arrival.
#[test]
fn restore_after_n_idle_ticks_at_the_hero_arrives_popped() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let view = snapshot.view();
    let full = RowStyle::HOME.focus_scale;
    for idle in [0, 3] {
        let mut s = screen(view);
        s.restore_reveal = true;
        let hero = FocusKey { entry: s.entry, elem: HERO_PLAY_ELEM };
        for frame in 0..idle {
            step(&mut s, view, Some(hero), &ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_667 }));
        }
        let card = first_card(&s);
        step(&mut s, view, Some(card), &ScreenEvent::FocusMoved {
            from: Some(hero), to: card, by: By::Restore,
        });
        assert_eq!(s.snap.pos, 1.0, "the restore reveal opens on the shelves");
        step(&mut s, view, Some(card), &ScreenEvent::Tick(Tick { ms: 100, dt_us: 16_667 }));
        assert_eq!(pop_of(&s, view, Some(card), 0, 0), full, "{idle} idle ticks: the restored card is popped at once");
    }
}

/// The dive from the hero by a deliberate move still grows the card from rest when the shelves
/// wake, after the same idle ticks.
#[test]
fn a_deliberate_dive_after_idle_ticks_at_the_hero_grows_from_rest() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let view = snapshot.view();
    let full = RowStyle::HOME.focus_scale;
    let mut s = screen(view);
    let hero = FocusKey { entry: s.entry, elem: HERO_PLAY_ELEM };
    for frame in 0..3 {
        step(&mut s, view, Some(hero), &ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_667 }));
    }
    let card = first_card(&s);
    step(&mut s, view, Some(card), &ScreenEvent::FocusMoved { from: Some(hero), to: card, by: By::Dir });
    let mut first_awake = None;
    for frame in 0..120 {
        step(&mut s, view, Some(card), &ScreenEvent::Tick(Tick { ms: 100 + frame * 16, dt_us: 16_667 }));
        if s.snap.pos > 0.5 {
            first_awake = Some(pop_of(&s, view, Some(card), 0, 0));
            break;
        }
    }
    let first = first_awake.expect("the dive reaches the shelves");
    assert!(first > 1.0 && first < full - 0.02, "the pop grows from rest: {first}");
}

/// A landing the row cannot honour: the focused card moves from column 1 to column 6 of an
/// 8-card row. The scroll lands at the row's end, never outside [0, max] on any frame, the pop
/// follows the element, and no other tile is lifted.
#[test]
fn a_landing_the_row_cannot_honour_clamps_the_scroll_and_the_pop_follows() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_grid_for_test(&mut state, &adapter, 1, 8);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    let key = FocusKey { entry: s.entry, elem: s.rows[0].elems[1] };
    pop_card(&mut s, snapshot.view(), key, 120);
    let max = RowStyle::HOME.max_scroll(8);
    assert!(max > 0.0);
    plx_data::pms::reverse_test_shelves(&mut state);
    let landed = plx_data::pms::hubs_snapshot(&state);
    let view = landed.view();
    s.sync_catalog(&cx(view, Some(key)));
    assert_eq!(s.rows[0].elems.iter().position(|&e| e == key.elem), Some(6), "the card moved to column 6");
    for frame in 0..40 {
        step(&mut s, view, Some(key), &ScreenEvent::Tick(Tick { ms: 5_000 + frame * 16, dt_us: 16_667 }));
        let scroll = s.grid.shelves[0].scroll();
        assert!((0.0..=max).contains(&scroll), "frame {frame}: scroll {scroll} outside [0, {max}]");
        for col in 0..8 {
            let pop = pop_of(&s, view, Some(key), 0, col);
            if col == 6 {
                assert!(pop > 1.05, "frame {frame}: the pop follows the element, {pop}");
            } else {
                assert!((pop - 1.0).abs() < 0.001, "frame {frame}: column {col} is lifted, {pop}");
            }
        }
    }
    assert!((s.grid.shelves[0].scroll() - max).abs() < 0.5, "the scroll lands at the row's end");
}

#[test]
fn shelf_viewports_follow_identity_across_a_catalog_reorder() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_grid_for_test(&mut state, &adapter, 2, 24);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    let identity = s.rows[0].identity.clone();
    s.grid.shelves[0].restore_scroll(900.0, 24);
    plx_data::pms::reverse_test_hubs(&mut state);
    let changed = plx_data::pms::hubs_snapshot(&state);
    s.sync_catalog(&cx(changed.view(), None));
    assert_eq!(s.rows[1].identity, identity);
    assert_eq!(s.grid.shelves[1].scroll(), 900.0);
    assert_eq!(s.grid.shelves[0].scroll(), 0.0);
}

#[test]
fn the_grid_holds_one_motion_row_per_published_row() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_grid_for_test(&mut state, &adapter, 5, 24);
    let five = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(five.view());
    assert_eq!(s.rows.len(), 5);
    assert_eq!(s.grid.shelves.len(), s.rows.len());
    let kept = s.rows[1].identity.clone();
    s.grid.shelves[1].restore_scroll(900.0, 24);
    plx_data::pms::seed_grid_for_test(&mut state, &adapter, 3, 24);
    let three = plx_data::pms::hubs_snapshot(&state);
    s.sync_catalog(&cx(three.view(), None));
    assert_eq!(s.rows.len(), 3);
    assert_eq!(s.grid.shelves.len(), 3, "the grid shrinks with the published rows");
    assert_eq!(s.rows[1].identity, kept);
    assert_eq!(s.grid.shelves[1].scroll(), 900.0, "a surviving row keeps its motion");
    assert_eq!(s.grid.shelves[0].scroll(), 0.0);
    assert_eq!(s.grid.scroll_x(9), 0.0, "a stale row index reads a resting row");
}

#[test]
fn an_empty_loading_publication_does_not_consume_restored_viewports() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_grid_for_test(&mut state, &adapter, 2, 24);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut original = screen(snapshot.view());
    original.grid.shelves[1].restore_scroll(900.0, 24);
    let PageMemory::Home(memory) = <HomeScreen as Screen<TestHost>>::memory(&original) else { unreachable!() };
    plx_data::pms::seed_for_test(&mut state, &adapter, 0, plx_data::pms::HubState::Loading);
    let loading = plx_data::pms::hubs_snapshot(&state);
    let mut restored = screen(loading.view());
    restored.restore(&memory);
    restored.sync_catalog(&cx(loading.view(), None));
    // Another eviction while loading must not discard the still-unmatched saved rows.
    let PageMemory::Home(pending) = <HomeScreen as Screen<TestHost>>::memory(&restored) else { unreachable!() };
    let mut restored = screen(loading.view());
    restored.restore(&pending);
    plx_data::pms::seed_grid_for_test(&mut state, &adapter, 2, 24);
    let ready = plx_data::pms::hubs_snapshot(&state);
    restored.sync_catalog(&cx(ready.view(), None));
    assert_eq!(restored.grid.shelves[1].scroll(), 900.0);
}

#[test]
fn viewport_memory_is_canonical_because_return_reuses_it() {
    let a = HomeScreen::new(EntryId(7), InstanceId(9));
    let mut b = HomeScreen::new(EntryId(7), InstanceId(9));
    b.grid.scroll_y.pos = 320.0;
    assert_ne!(a.hash(), b.hash());
    assert_ne!(<HomeScreen as Screen<TestHost>>::memory(&a).hash(),
        <HomeScreen as Screen<TestHost>>::memory(&b).hash());
}

#[test]
fn visible_tick_emits_both_store_work_requests_after_the_step() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 0, plx_data::pms::HubState::Loading);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    let (_, out, _) = step(
        &mut s,
        snapshot.view(),
        None,
        &ScreenEvent::Tick(Tick {
            ms: 16,
            dt_us: 16_000,
        }),
    );
    let work: Vec<_> = out
        .iter()
        .filter_map(|stamped| match stamped.fx {
            Fx::App(AppFx::StoreWork(work)) => Some(work),
            _ => None,
        })
        .collect();
    assert_eq!(work, vec![StoreWork::Hubs, StoreWork::BrowseDiscovery]);
}

/// Sets the Continue Watching setting for one test and puts the prior value back. The caller
/// holds `testlock::serial()`, which `restore_deck_press` asserts.
struct DeckPressGuard(plx_media::route::DeckPress);
impl Drop for DeckPressGuard {
    fn drop(&mut self) {
        plx_media::route::restore_deck_press(self.0);
    }
}
fn deck_press_is(mode: plx_media::route::DeckPress) -> DeckPressGuard {
    let prior = DeckPressGuard(plx_media::route::deck_press());
    plx_media::route::restore_deck_press(mode);
    prior
}

#[test]
fn continue_watching_commit_follows_the_setting_while_an_ordinary_shelf_opens_detail() {
    use plx_media::route::DeckPress;
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 2, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    let card = first_card(&s);
    let commit = |s: &mut HomeScreen, id| {
        step(s, snapshot.view(), Some(card), &ScreenEvent::PressCommit(plx_machine::machine::PressId(id))).1
    };
    {
        let _press = deck_press_is(DeckPress::Play);
        let play = commit(&mut s, 1);
        assert!(has_home(&play, |r| matches!(r, HomeReq::Play { rk, .. } if rk == "1")));
    }
    {
        // the default: OK on a deck card opens the page, exactly as on any other shelf
        let _press = deck_press_is(DeckPress::Details);
        let detail = commit(&mut s, 2);
        assert!(has_home(&detail, |r| matches!(r, HomeReq::Detail { rk, .. } if rk == "1")));
        assert!(!has_home(&detail, |r| matches!(r, HomeReq::Play { .. })));
    }
    let _press = deck_press_is(DeckPress::Play);
    s.rows[0].identity = HomeHubIdentity::Key {
        sid: plx_plex::plex::ServerId::UNSET,
        key: "/hubs/recent".into(),
    };
    let detail = commit(&mut s, 3);
    assert!(has_home(&detail, |r| matches!(r, HomeReq::Detail { rk, .. } if rk == "1")));
}

/// The amber ▶ promises the press plays, so the deck's title line carries it in Play mode only.
#[test]
fn the_deck_title_carries_the_play_glyph_only_while_the_press_plays() {
    use plx_media::route::DeckPress;
    let _guard = plx_base::testlock::serial();
    {
        let _press = deck_press_is(DeckPress::Play);
        assert!(deck_label(true, "Show").glyph);
        assert!(!deck_label(false, "Show").glyph, "a shelf that navigates never carries it");
    }
    let _press = deck_press_is(DeckPress::Details);
    assert!(!deck_label(true, "Show").glyph);
    assert!(!deck_label(false, "Show").glyph);
}

#[test]
fn holding_a_shelf_card_opens_the_item_menu_without_activation() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 1, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    let card = first_card(&s);
    let (_, out, _) = step(
        &mut s,
        snapshot.view(),
        Some(card),
        &ScreenEvent::PressHold(plx_machine::machine::PressId(1)),
    );
    assert!(has_home(
        &out,
        |r| matches!(r, HomeReq::ItemMenu { rk, .. } if rk == "1")
    ));
    assert!(!has_home(&out, |r| matches!(
        r,
        HomeReq::Play { .. } | HomeReq::Detail { .. }
    )));
}

#[test]
fn a_partially_visible_focused_row_allows_hover_to_its_neighbors_only() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 2, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    let first = first_card(&s);
    pop_card(&mut s, snapshot.view(), first, 80);
    s.layout_grid();
    s.grid.shelves[0].set_base_y(-20.0);
    let neighbor = s.rows[0].elems[1];
    for (focus, expected) in [(Some(first), Hover::Focus), (None, Hover::OnlyIfFocused)] {
        let context = cx(snapshot.view(), focus);
        let mut frame = DrawFrame::new(&context, Painter::root());
        s.record_stops(&mut frame, snapshot.view());
        let stops = frame.into_stops();
        let stop = stops.iter().find(|stop| stop.key.elem == neighbor).unwrap();
        assert!(stop.rest_rect.y < 40.0);
        assert_eq!(stop.hover, expected);
    }
}

#[test]
fn drawn_stops_feed_the_real_hit_map_with_scoped_card_keys() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 2, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    let card = first_card(&s);
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    pop_card(&mut s, snapshot.view(), card, 80);
    s.layout_grid();
    let context = cx(snapshot.view(), Some(card));
    let mut frame = DrawFrame::new(&context, Painter::root());
    s.record_stops(&mut frame, snapshot.view());
    let mut map = HitMap::new();
    map.fill(frame.into_stops());
    map.swap();
    let placed = Focusable::<TestHost>::place(&s, &card.elem, &context, At::Drawn).unwrap();
    let resolution = map.resolve(Some(card.entry), PointerKind::Click,
        placed.rect.cx(),
        placed.rect.cy(),
        Some(card),
    );
    assert_eq!(resolution.hit, Some(card));
    assert_eq!(resolution.activate, Some((card, Activate::Press)));
}

#[test]
fn quick_down_then_ok_activates_the_hero_still_visible_before_the_snap_midpoint() {
    let _guard = plx_base::testlock::serial();
    let _press = deck_press_is(plx_media::route::DeckPress::Play);
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 2, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.carousel = Some((plx_plex::plex::ServerId::UNSET, "2".into()));
    let hero = FocusKey {
        entry: s.entry,
        elem: HERO_PLAY_ELEM,
    };
    let card = first_card(&s);
    let move_event = ScreenEvent::FocusMoved {
        from: Some(hero),
        to: card,
        by: By::Dir,
    };
    let _ = step(&mut s, snapshot.view(), Some(card), &move_event);
    assert_eq!(s.snap_target, 1.0);
    assert!(s.snap.pos < 0.5);
    let (_, out, _) = step(
        &mut s,
        snapshot.view(),
        Some(card),
        &ScreenEvent::PressCommit(plx_machine::machine::PressId(7)),
    );
    assert!(has_home(
        &out,
        |req| matches!(req, HomeReq::Play { rk, .. } if rk == "2")
    ));
    assert!(!has_home(
        &out,
        |req| matches!(req, HomeReq::Play { rk, .. } if rk == "1")
    ));
}

#[test]
fn back_from_grid_folds_to_hero_before_root_back() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 1, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    let card = first_card(&s);
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    let back = ScreenEvent::Input(InputEvent {
        at: Tick::default(),
        source: Source::Sdl,
        kind: InputKind::Key {
            key: Key::Back,
            sym: 0,
            wcode: 0,
            edge: Edge::Down,
            at_edge: false,
        },
    });
    let (_, fold, _) = step(&mut s, snapshot.view(), Some(card), &back);
    assert_eq!(s.snap_target, 0.0);
    assert!(has_home(&fold, |req| matches!(req, HomeReq::FoldToHero)));
    assert!(!fold
        .iter()
        .any(|stamped| matches!(&stamped.fx, Fx::App(AppFx::Loop(LoopReq::BackAtRoot)))));

    s.snap.jump(0.0);
    let hero = FocusKey {
        entry: s.entry,
        elem: HERO_PLAY_ELEM,
    };
    let (_, root, _) = step(&mut s, snapshot.view(), Some(hero), &back);
    assert!(root
        .iter()
        .any(|stamped| matches!(&stamped.fx, Fx::App(AppFx::Loop(LoopReq::BackAtRoot)))));
}

fn enter_target(out: &[Stamped<TestHost>]) -> Option<FocusTarget<u32>> {
    out.iter().find_map(|stamped| match &stamped.fx {
        Fx::Deliver(
            MachineId::Instance(InstanceId(9)),
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus })),
        ) => Some(*focus),
        _ => None,
    })
}

#[test]
fn addressed_focus_commands_reseat_only_through_enter_fresh() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 2, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    let card = first_card(&s);

    let (_, grid, _) = step(
        &mut s,
        snapshot.view(),
        None,
        &ScreenEvent::App(AppMsg::Home(HomeCmd::FocusGrid { row: 0, col: 0 })),
    );
    assert!(matches!(enter_target(&grid), Some(FocusTarget::Elem(key)) if key == card));

    let (_, hero, _) = step(
        &mut s,
        snapshot.view(),
        Some(card),
        &ScreenEvent::App(AppMsg::Home(HomeCmd::Hero)),
    );
    assert!(matches!(
        enter_target(&hero),
        Some(FocusTarget::ContainerGroup(HERO_GROUP))
    ));

    for (tab, elem) in [
        (HomeTab::Home, STRIP_HOME_ELEM),
        (HomeTab::Movies, STRIP_MOVIES_ELEM),
        (HomeTab::Shows, STRIP_SHOWS_ELEM),
        (HomeTab::Search, STRIP_SEARCH_ELEM),
    ] {
        let (_, strip, _) = step(
            &mut s,
            snapshot.view(),
            None,
            &ScreenEvent::App(AppMsg::Home(HomeCmd::FocusStrip(tab))),
        );
        assert!(matches!(enter_target(&strip), Some(FocusTarget::Elem(key)) if key.elem == elem));
    }
}

#[test]
fn addressed_carousel_commands_mutate_the_owned_identity_not_focus() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());

    let (handled, _, _) = step(
        &mut s,
        snapshot.view(),
        None,
        &ScreenEvent::App(AppMsg::Home(HomeCmd::SelectHero(2))),
    );
    assert_eq!(handled, Handled::Yes);
    assert_eq!(s.carousel.as_ref().map(|(_, rk)| rk.as_str()), Some("3"));

    s.hero_flip_cd = 0.0;
    let before = s.carousel.clone();
    let _ = step(
        &mut s,
        snapshot.view(),
        None,
        &ScreenEvent::App(AppMsg::Home(HomeCmd::Flip(1))),
    );
    assert_ne!(s.carousel, before);
}

#[test]
fn loading_has_no_phantom_hero_and_terminal_status_has_one_action() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    for (hub_state, expected) in [
        (plx_data::pms::HubState::Loading, 0),
        (plx_data::pms::HubState::Failed, 1),
        (plx_data::pms::HubState::Ready, 1),
    ] {
        plx_data::pms::seed_for_test(&mut state, &adapter, 0, hub_state);
        let snapshot = plx_data::pms::hubs_snapshot(&state);
        let s = screen(snapshot.view());
        let context = cx(snapshot.view(), None);
        let mut groups = Vec::new();
        Focusable::<TestHost>::groups(&s, &context, &mut groups);
        assert_eq!(groups[0].len, expected);
        let retry = FocusKey {
            entry: s.entry,
            elem: HERO_PLAY_ELEM,
        };
        assert!(matches!(
            Focusable::<TestHost>::neighbour(&s, retry, Dir::Right, &context),
            Step::Edge
        ));
        assert_eq!(
            Focusable::<TestHost>::group_of(&s, &HERO_INFO_ELEM, &context),
            None
        );
    }
}

#[test]
fn first_catalog_landing_reseats_the_default_cta_unless_the_strip_was_chosen() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 0, plx_data::pms::HubState::Loading);
    let empty = plx_data::pms::hubs_snapshot(&state);
    let mut automatic = screen(empty.view());
    let fallback = FocusKey {
        entry: automatic.entry,
        elem: STRIP_ACCOUNT_ELEM,
    };

    plx_data::pms::seed_for_test(&mut state, &adapter, 1, plx_data::pms::HubState::Ready);
    let landed = plx_data::pms::hubs_snapshot(&state);
    let (_, out, _) = step(
        &mut automatic,
        landed.view(),
        Some(fallback),
        &ScreenEvent::Tick(Tick {
            ms: 16,
            dt_us: 16_000,
        }),
    );
    assert!(matches!(
        enter_target(&out),
        Some(FocusTarget::ContainerGroup(HERO_GROUP))
    ));

    plx_data::pms::seed_for_test(&mut state, &adapter, 0, plx_data::pms::HubState::Loading);
    let empty = plx_data::pms::hubs_snapshot(&state);
    let mut chosen = screen(empty.view());
    let moved = ScreenEvent::FocusMoved {
        from: None,
        to: fallback,
        by: By::Pointer,
    };
    let _ = step(&mut chosen, empty.view(), Some(fallback), &moved);
    plx_data::pms::seed_for_test(&mut state, &adapter, 1, plx_data::pms::HubState::Ready);
    let landed = plx_data::pms::hubs_snapshot(&state);
    let (_, out, _) = step(
        &mut chosen,
        landed.view(),
        Some(fallback),
        &ScreenEvent::Tick(Tick {
            ms: 16,
            dt_us: 16_000,
        }),
    );
    assert!(enter_target(&out).is_none());
}

#[test]
fn addressed_item_menu_uses_the_current_owned_grid_item() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 1, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    let card = first_card(&s);
    let (_, out, _) = step(
        &mut s,
        snapshot.view(),
        Some(card),
        &ScreenEvent::App(AppMsg::Home(HomeCmd::ItemMenu)),
    );
    assert!(has_home(
        &out,
        |req| matches!(req, HomeReq::ItemMenu { rk, .. } if rk == "1")
    ));
}

#[test]
fn parent_read_only_api_projects_engine_focus_without_setters() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 2, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let s = screen(snapshot.view());
    let card = first_card(&s);
    let context = cx(snapshot.view(), Some(card));

    assert!(SHAPE.contains("visible_activation:Option<u32>"));
    assert_eq!(
        s.hero_item(&context).map(|item| item.rk.as_str()),
        Some("1")
    );
    assert_eq!(
        s.focused_item(Some(card), &context)
            .map(|item| item.rk.as_str()),
        Some("1")
    );
    assert_eq!(s.grid_position(Some(card), &context), Some((0, 0)));
    assert_eq!(s.snap_target(), 0.0);
    assert!(s.focused_rect(Some(card), &context, At::Drawn).is_some());

    // The lifted redraw query is safe with no focused key and never reaches a global cursor.
    let empty_context = cx(snapshot.view(), None);
    let mut frame = DrawFrame::new(&empty_context, Painter::root());
    s.redraw_focused(&mut frame, None);
}

/// The billboard's 8 s auto-advance is a TIMER, not an animation: nothing it counts is drawn, and
/// the dispatcher ticks the page every loop iteration whether or not the frame presents. So a
/// settled hero must let the present gate close while it counts down, and the flip it ends in must
/// still happen on time — and wake the gate itself.
///
/// D4 (phase 12) made the countdown note `Motion` on every tick, which kept a still Home
/// presenting at the full frame rate indefinitely (measured on the TV, 2026-09-19): every modal
/// dismiss and page pop returned to a Home that never went idle, and the next transition's first
/// frame paid that GPU queue. The test that pinned the old behaviour stepped through a fresh
/// `Present`, whose first-frame `dirty` answers `true` whatever the screen reports — it could not
/// have failed.
#[test]
fn a_settled_hero_counting_down_lets_the_gate_close_and_still_flips() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let view = snapshot.view();
    assert!(view.hero_count() > 1, "the auto-advance only arms with more than one hero slot");
    let mut s = screen(view);
    let context = cx(view, None);
    let mut present = plx_machine::present::Present::new();
    // `take` at tick_ms 0 throughout keeps the keepalive term out: this counts only what the
    // screen itself asked for. The first take drains the fresh gate's first-frame `dirty`.
    present.take(0);
    let mut asked_while_counting = 0u32;
    let mut flipped_at = None;
    // 10 s of 16 ms ticks: past HERO_AUTO_S, so the countdown expires exactly once.
    for frame in 1..=625u32 {
        let mut out = Vec::new();
        {
            let mut fx = Effects::new(
                &mut out,
                plx_machine::machine::MachineId::Instance(InstanceId(9)),
                &mut present,
            );
            let tick = ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_000 });
            Machine::<TestHost>::step(&mut s, &tick, &context, &mut fx);
        }
        let asked = present.take(0);
        if s.outgoing.is_some() {
            flipped_at.get_or_insert(frame);
            assert!(asked, "the flip's slide is motion and must present");
        } else if flipped_at.is_none() && asked {
            asked_while_counting += 1;
        }
    }
    let flipped_at = flipped_at.expect("the countdown must still end in a flip");
    assert!(
        (flipped_at as f32 * 0.016 - HERO_AUTO_S).abs() < 0.05,
        "flipped after {flipped_at} ticks, not after {HERO_AUTO_S} s"
    );
    assert_eq!(
        asked_while_counting, 0,
        "a settled hero asked for {asked_while_counting} presents while only counting down"
    );
}

#[test]
fn the_hero_does_not_advance_while_a_modal_covers_home() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let view = snapshot.view();
    assert!(view.hero_count() > 1, "the auto-advance only arms with more than one hero slot");
    let mut s = screen(view);
    let selected = s.carousel.clone();

    step(&mut s, view, None, &ScreenEvent::Cover);
    for frame in 1..=625u32 {
        step(
            &mut s,
            view,
            None,
            &ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_000 }),
        );
    }

    assert_eq!(
        s.carousel, selected,
        "a Compact modal still ticks its host, but covering Home must pause its hero"
    );
    assert!(s.outgoing.is_none(), "no hidden hero slide was started under the modal");
}

#[test]
fn the_hero_countdown_restarts_when_the_last_modal_is_dismissed() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let view = snapshot.view();
    let mut s = screen(view);
    s.hero_auto = 0.25;
    let selected = s.carousel.clone();

    step(&mut s, view, None, &ScreenEvent::Cover);
    // Navigation emits Uncover only when the last modal surface is dismissed.
    step(&mut s, view, None, &ScreenEvent::Uncover);
    step(
        &mut s,
        view,
        None,
        &ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_000 }),
    );

    assert_eq!(s.carousel, selected, "dismissal must not trigger an immediate hero jump");
    assert!(s.outgoing.is_none(), "dismissal must not begin a hidden hero slide");
    assert!(
        (s.hero_auto - (HERO_AUTO_S - 0.016)).abs() < f32::EPSILON,
        "the fresh countdown should have one ordinary tick consumed, got {}",
        s.hero_auto
    );
}

/// `HomeCmd::PinHero` — the screenshot pipeline's hero pin (`/tmp/plxnative-heropin=<n>`) —
/// selects that slot and HOLDS it: the auto-advance never fires, however long the page sits
/// uncovered, so a capture taken at any settled moment shows the same billboard.
#[test]
fn a_pinned_hero_never_auto_advances() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let view = snapshot.view();
    let mut s = screen(view);
    let (handled, _, _) = step(&mut s, view, None, &ScreenEvent::App(AppMsg::Home(HomeCmd::PinHero(1))));
    assert_eq!(handled, Handled::Yes);
    assert_eq!(s.carousel.as_ref().map(|(_, rk)| rk.as_str()), Some("2"));
    // 20 s of 16 ms ticks: more than twice HERO_AUTO_S.
    for frame in 1..=1250u32 {
        step(&mut s, view, None, &ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_000 }));
        assert!(s.outgoing.is_none(), "the pinned hero began a slide at tick {frame}");
    }
    assert_eq!(s.carousel.as_ref().map(|(_, rk)| rk.as_str()), Some("2"));
}

/// **A signed-in person has a consent path from Home** (PLX-NATIVE-10, code review 3). A failed
/// Home while discovery offers "Connect without encryption?" for a server says why in the reason
/// slot and makes *Connect* the primary; *Connect* asks the SHARED question
/// (`screens::plaintext_question`) seated on *Not now* instead of retrying, and only its answer
/// reaches Session. Once answered *Not now*, the primary is *Try again* again and the reason
/// points at Settings.
#[test]
fn a_failed_home_over_an_offered_server_asks_the_shared_question() {
    use plx_plex::plex::session::PlaintextChoice;
    let _guard = plx_base::testlock::serial();
    plx_plex::plex::grant::reset_for_test();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 0, plx_data::pms::HubState::Failed);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let view = snapshot.view();
    let mut s = screen(view);
    let entry = s.entry;
    let verdict = plx_plex::plex::grant::PlaintextVerdict {
        machine_id: "lan-machine".into(), name: "Home".into(), shared_by: String::new(),
        eligibility: plx_plex::plex::probe::PlaintextEligibility::Eligible, choice: PlaintextChoice::Undecided,
    };
    plx_plex::plex::grant::offered(plx_plex::plex::grant::scope(), verdict.clone());
    step(&mut s, view, None, &ScreenEvent::Tick(Tick::default()));
    let measure = FixtureMeasure;
    let overlay = status_overlay(view, &s.plaintext, &s.clock).unwrap();
    assert_eq!(overlay.action, Some(plaintext_question::connect()));
    let reason = plx_session::auth::plaintext_copy(Some(&verdict), plx_session::auth::ReadoutSurface::SignedIn);
    assert_eq!(overlay.reason.and_then(|r| r.to_str().ok()), Some(reason.as_ref()));
    let drawn = overlay.action_frame_measured(&measure).unwrap();
    let hit = s.hero_button_rect(view, 0, &measure).unwrap();
    assert_eq!([hit.x, hit.y, hit.w, hit.h], [drawn.x, drawn.y, drawn.w, drawn.h], "the hit rect is the drawn pill");

    let hero = Some(FocusKey { entry, elem: HERO_PLAY_ELEM });
    let (_, opened, _) = step(&mut s, view, hero, &ScreenEvent::Activate(HERO_PLAY_ELEM));
    let retries = |out: &[Stamped<TestHost>]| out.iter().any(|st| matches!(&st.fx,
        Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Retry)))));
    assert!(!retries(&opened), "Connect asks; it does not retry");
    assert!(s.plaintext_alert.is_open());
    assert!(opened.iter().any(|st| matches!(&st.fx, Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(
        Enter::Fresh { focus: FocusTarget::ContainerGroup(g) }))) if *g == PLAINTEXT_GROUP)));
    let context = cx(view, None);
    let mut groups = Vec::new();
    Focusable::<TestHost>::groups(&s, &context, &mut groups);
    assert_eq!(groups.iter().map(|g| g.id).collect::<Vec<_>>(), [PLAINTEXT_GROUP], "the question traps focus");
    let from = Placed { rect: Rect::FULL, rest_rect: Rect::FULL, clip: Rect::FULL, index: None };
    assert_eq!(Focusable::<TestHost>::seat(&s, PLAINTEXT_GROUP, from, &context).elem, PLAINTEXT_CANCEL_ELEM,
        "seated on Not now");

    let connect = Some(FocusKey { entry, elem: PLAINTEXT_CONNECT_ELEM });
    let (_, answered, _) = step(&mut s, view, connect, &ScreenEvent::PressCommit(plx_machine::machine::PressId(1)));
    let answers: Vec<_> = answered.iter().filter_map(|st| match &st.fx {
        Fx::App(AppFx::Session(plx_session::auth::SessionCmd::AnswerPlaintext { machine_id, choice, .. }))
            if machine_id == "lan-machine" => Some(*choice),
        _ => None,
    }).collect();
    assert_eq!(answers, [PlaintextChoice::Allowed]);
    assert!(!s.plaintext_alert.is_open());

    plx_plex::plex::grant::answer("account", "lan-machine", PlaintextChoice::Declined);
    step(&mut s, view, None, &ScreenEvent::Tick(Tick::default()));
    let overlay = status_overlay(view, &s.plaintext, &s.clock).unwrap();
    assert_eq!(overlay.action, Some(plaintext_question::try_again()));
    assert!(overlay.reason.and_then(|r| r.to_str().ok()).is_some_and(|r|
        r.contains("Settings \u{2192} Unencrypted connections")), "{:?}", overlay.reason);
    let (_, retried, _) = step(&mut s, view, hero, &ScreenEvent::Activate(HERO_PLAY_ELEM));
    assert!(retries(&retried), "an answered question is not put again from a failure");
    assert!(!s.plaintext_alert.is_open());
    plx_plex::plex::grant::reset_for_test();
}

// ---- linked collection shelves (#205) -------------------------------------------------------

const COLLECTION_ROWS: [(&str, &str, &str); 3] = [
    ("movie.recentlyadded.1", "/hubs/sections/1/recentlyAdded", "Recently Added"),
    ("custom.collection.1.50001.50001", "/library/collections/50001/children", "Toy Story Collection"),
    ("movie.recentlyviewed.1", "/hubs/sections/1/recentlyViewed", "Recently Viewed"),
];

fn collection_home(
    state: &mut plx_data::pms::PmsState,
    adapter: &std::sync::Arc<plx_data::pms::PmsAdapter>,
) -> plx_data::pms::HubsSnapshot {
    plx_data::pms::seed_named_hubs_for_test(state, adapter, 6, &COLLECTION_ROWS);
    plx_data::pms::hubs_snapshot(state)
}

fn on_grid(s: &mut HomeScreen) {
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    s.layout_grid();
}

fn heading_key(s: &HomeScreen, row: usize) -> FocusKey<u32> {
    FocusKey { entry: s.entry, elem: heading_elem(s.rows[row].group) }
}

fn move_from(
    s: &HomeScreen,
    view: HubsView<'_>,
    engine: &mut FocusEngine<u32>,
    dir: Dir,
) -> Outcome<u32> {
    let owner = InputOwner::Entry(s.entry);
    let focus = engine.read(owner);
    let context = Cx { focus, ..cx(view, None) };
    let mut links = Vec::new();
    <HomeScreen as Screen<TestHost>>::links(s, &mut links);
    engine.move_dir(owner, s, &links, dir, &context)
}

#[test]
fn only_a_promoted_collection_shelf_gets_a_linked_heading() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let snapshot = collection_home(&mut state, &adapter);
    let s = screen(snapshot.view());
    assert_eq!(s.rows.len(), 3);
    assert!(s.linked(0).is_none() && s.linked(2).is_none());
    let linked = s.linked(1).expect("the custom.collection row links to its collection");
    assert_eq!((linked.sec, linked.rk.as_str()), (1, "50001"));
    assert_eq!(s.locate(heading_elem(s.rows[0].group)), None,
        "an unlinked shelf has no heading stop to land on");
    assert_eq!(s.locate(heading_key(&s, 1).elem), Some(Located::Heading(1)));
    let mut groups = Vec::new();
    let context = cx(snapshot.view(), None);
    Focusable::<TestHost>::groups(&s, &context, &mut groups);
    let headings: Vec<_> = groups.iter().filter(|g| g.id.0 & HEADING_BASE != 0).collect();
    assert_eq!(headings.len(), 1, "exactly one heading stop: the collection's");
    assert_eq!(headings[0].id, heading_group(s.rows[1].group));
    assert_eq!(headings[0].len, 1);
    // Member cards stay exactly as they were: no collection tile, no SEE ALL tile.
    assert_eq!(s.rows[1].elems.len(), 6);
}

#[test]
fn up_from_any_collection_card_reaches_the_heading_and_down_returns_to_that_card() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let snapshot = collection_home(&mut state, &adapter);
    let mut s = screen(snapshot.view());
    on_grid(&mut s);
    let owner = InputOwner::Entry(s.entry);
    let card = FocusKey { entry: s.entry, elem: s.rows[1].elems[4] };
    let mut engine = FocusEngine::new();
    engine.set(owner, card, Some(s.rows[1].group), By::Restore);

    let Outcome::Moved { to, .. } = move_from(&s, snapshot.view(), &mut engine, Dir::Up) else {
        panic!("UP from a collection card must move");
    };
    assert_eq!(to, heading_key(&s, 1), "UP lands on the linked heading, not the shelf above");
    for dir in [Dir::Left, Dir::Right] {
        assert_eq!(move_from(&s, snapshot.view(), &mut engine, dir), Outcome::Nothing,
            "LEFT/RIGHT are inert on the heading");
    }
    let Outcome::Moved { to, .. } = move_from(&s, snapshot.view(), &mut engine, Dir::Down) else {
        panic!("DOWN from the heading must return to the shelf");
    };
    assert_eq!(to, card, "DOWN restores the remembered card, not the one under the heading");

    // UP from the heading goes on to the shelf above.
    engine.set(owner, heading_key(&s, 1), Some(heading_group(s.rows[1].group)), By::Restore);
    let Outcome::Moved { to, .. } = move_from(&s, snapshot.view(), &mut engine, Dir::Up) else {
        panic!("UP from the heading must reach the shelf above");
    };
    assert!(s.rows[0].elems.contains(&to.elem));
}

#[test]
fn down_from_the_shelf_above_stops_on_the_heading_and_plain_shelves_keep_projection() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let snapshot = collection_home(&mut state, &adapter);
    let mut s = screen(snapshot.view());
    on_grid(&mut s);
    let owner = InputOwner::Entry(s.entry);
    let mut engine = FocusEngine::new();
    engine.set(owner, FocusKey { entry: s.entry, elem: s.rows[0].elems[2] },
        Some(s.rows[0].group), By::Restore);
    let Outcome::Moved { to, .. } = move_from(&s, snapshot.view(), &mut engine, Dir::Down) else {
        panic!("DOWN from the shelf above must move");
    };
    assert_eq!(to, heading_key(&s, 1));

    // The plain shelf below the collection is entered from its cards and has no heading stop.
    engine.set(owner, FocusKey { entry: s.entry, elem: s.rows[2].elems[0] },
        Some(s.rows[2].group), By::Restore);
    let Outcome::Moved { to, .. } = move_from(&s, snapshot.view(), &mut engine, Dir::Up) else {
        panic!("UP from the plain shelf must move");
    };
    assert!(s.rows[1].elems.contains(&to.elem),
        "UP from a plain shelf enters the collection's cards, not its heading");
}

#[test]
fn ok_on_the_linked_heading_opens_the_collection_page() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let snapshot = collection_home(&mut state, &adapter);
    let mut s = screen(snapshot.view());
    on_grid(&mut s);
    let heading = heading_key(&s, 1);
    let (_, out, _) = step(&mut s, snapshot.view(), Some(heading), &ScreenEvent::Activate(heading.elem));
    let pushed: Vec<_> = out.iter().filter_map(|stamped| match &stamped.fx {
        Fx::App(AppFx::Content(ContentReq::Push(arg))) => Some(arg.clone()),
        _ => None,
    }).collect();
    assert_eq!(pushed.len(), 1);
    assert!(matches!(&pushed[0], ContentArg::Collection(id)
        if id.rk == "50001" && id.sec == 1 && id.tag == 0 && id.name == "Toy Story Collection"), "{:?}", pushed[0]);
    assert!(!has_home(&out, |r| matches!(r, HomeReq::Detail { .. } | HomeReq::Play { .. })),
        "the heading opens the collection, never a member");
}

#[test]
fn the_heading_keeps_focus_on_the_grid_and_reveals_its_row() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let snapshot = collection_home(&mut state, &adapter);
    let mut s = screen(snapshot.view());
    on_grid(&mut s);
    let from = FocusKey { entry: s.entry, elem: s.rows[1].elems[0] };
    let heading = heading_key(&s, 1);
    step(&mut s, snapshot.view(), Some(heading),
        &ScreenEvent::FocusMoved { from: Some(from), to: heading, by: By::Dir });
    assert_eq!(s.snap_target, 1.0, "a heading is on the shelves, not the hero");
    assert!(s.visible_activation.is_none());
    // BACK from the heading folds to the hero like any shelf focus.
    let back = ScreenEvent::Input(InputEvent {
        at: Tick::default(),
        source: Source::Sdl,
        kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
    });
    let (_, out, _) = step(&mut s, snapshot.view(), Some(heading), &back);
    assert!(has_home(&out, |r| matches!(r, HomeReq::FoldToHero)));
}

#[test]
fn the_linked_heading_is_a_hover_focus_stop_that_wins_over_the_cards() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let snapshot = collection_home(&mut state, &adapter);
    let mut s = screen(snapshot.view());
    on_grid(&mut s);
    let card = FocusKey { entry: s.entry, elem: s.rows[1].elems[0] };
    let context = cx(snapshot.view(), Some(card));
    let mut frame = DrawFrame::new(&context, Painter::root());
    s.record_stops(&mut frame, snapshot.view());
    let stops = frame.into_stops();
    let heading = heading_key(&s, 1);
    let stop = stops.iter().find(|stop| stop.key == heading).expect("the heading registers a stop");
    assert_eq!(stop.hover, Hover::Focus);
    assert!(stops.iter().all(|stop| stop.key.elem != heading_elem(s.rows[0].group)),
        "an unlinked heading is not a pointer target");
    let placed = Focusable::<TestHost>::place(&s, &heading.elem, &context, At::Drawn).unwrap();
    assert_eq!((stop.rect.x, stop.rect.y, stop.rect.w, stop.rect.h),
        (placed.rect.x, placed.rect.y, placed.rect.w, placed.rect.h), "the hit rect is the drawn face");
    let mut map = HitMap::new();
    map.fill(stops);
    map.swap();
    let resolution = map.resolve(Some(heading.entry), PointerKind::Click,
        placed.rect.cx(), placed.rect.cy(), Some(card));
    assert_eq!(resolution.hit, Some(heading));
}

#[test]
fn back_from_the_collection_page_returns_focus_to_the_heading() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let snapshot = collection_home(&mut state, &adapter);
    let original = screen(snapshot.view());
    let heading = heading_key(&original, 1);
    let PageMemory::Home(memory) = <HomeScreen as Screen<TestHost>>::memory(&original) else {
        unreachable!()
    };
    // An evicted page remounts from its memory; the return state names the heading key.
    let mut restored = HomeScreen::new(EntryId(7), InstanceId(9));
    restored.restore(&memory);
    restored.sync_catalog(&cx(snapshot.view(), None));
    let context = cx(snapshot.view(), Some(heading));
    assert_eq!(Focusable::<TestHost>::reconcile(&restored, heading, &context), heading,
        "the heading is a stable key: the restored page lands back on it");
}

/// A shelf whose cards are still below the screen's bottom edge already has its heading on screen:
/// the heading sits `TITLE_DY` ABOVE the cards. Culling the shelf by its card rect alone left that
/// heading undrawn until the first card pixel crossed the edge, so scrolling down the grid made the
/// next shelf's title pop in at full ink instead of sliding up with the page.
#[test]
fn a_shelf_heading_on_screen_is_drawn_while_its_cards_are_still_below_the_edge() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let view = snapshot.view();
    let mut s = screen(view);
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    s.layout_grid();
    let title = view.hub(0).unwrap().title.to_string();
    let drawn = |s: &HomeScreen| {
        plx_gfx::text::capture_text_runs_for_test(|| {
            let env = s.env(0.0);
            let context = cx(view, None);
            let frame = DrawFrame::new(&context, Painter::recording());
            s.draw_grid(&frame, view, &env, Painter::recording(), None);
        })
        .iter()
        .any(|run| run.contains(title.as_str()))
    };
    // Cards 4px under the bottom edge; the heading's line is well inside the screen.
    s.grid.shelves[0].set_base_y(SCR_H + 4.0);
    assert!(heading_y(s.grid.base_y(0), s.grid.lift(0)) < SCR_H - 20.0);
    assert!(drawn(&s), "the heading above an off-screen card row must still be drawn");
    // …and a shelf whose heading has not reached the edge yet is still culled.
    s.grid.shelves[0].set_base_y(SCR_H + TITLE_DY + 4.0);
    assert!(!drawn(&s), "a shelf entirely below the screen draws nothing");
}

thread_local! {
    /// What [`HeroArtSpy`] was asked, and which backdrops it has "decoded". Thread-local, like the
    /// source slot and the render cache it stands in front of.
    static HERO_ART: std::cell::RefCell<HeroArtState> = std::cell::RefCell::new(HeroArtState::default());
}

#[derive(Default)]
struct HeroArtState {
    /// Every DRAW probe (`resolve_tex_wh_on`) — demand, LRU-touched and evict-protected.
    probed: Vec<String>,
    /// Every speculative warm (`warm_tex_on`).
    warmed: Vec<String>,
    /// Paths whose pixels were handed to the render cache; a path's index is its `PosterKey`.
    delivered: Vec<String>,
    /// Shelf posters still in flight — Home's ordinary state for its first seconds on screen.
    busy: bool,
}

/// A poster source that answers READY only for what the test delivered, and records the rest.
struct HeroArtSpy;
impl plx_ui::tex::Source for HeroArtSpy {
    fn probe(&self, _: u16, path: &str, _: i32, _: i32, _: bool) -> Option<plx_machine::machine::PosterKey> {
        HERO_ART.with(|a| {
            let mut a = a.borrow_mut();
            a.probed.push(path.into());
            a.delivered
                .iter()
                .position(|p| p == path)
                .map(|i| plx_machine::machine::PosterKey(i as u32))
        })
    }
    fn warm(&self, _: u16, path: &str, _: i32, _: i32, _: bool) -> plx_ui::tex::Warm {
        HERO_ART.with(|a| a.borrow_mut().warmed.push(path.into()));
        plx_ui::tex::Warm::Claimed
    }
    fn logo(&self, _: u16, _: &str) -> Option<plx_machine::machine::PosterKey> {
        None
    }
    fn logo_warm(&self, _: u16, _: &str) -> plx_ui::tex::Warm {
        plx_ui::tex::Warm::Known
    }
    fn unresident(&self, _: plx_machine::machine::PosterKey, _: bool) {}
    fn idle(&self) -> bool {
        HERO_ART.with(|a| !a.borrow().busy)
    }
}

fn hero_tick(ms: u32) -> ScreenEvent<TestHost> {
    ScreenEvent::Tick(Tick { ms, dt_us: 16_000 })
}

/// Right at the edge of the hero's controls: the press that flips the billboard by hand.
fn hero_edge_right() -> ScreenEvent<TestHost> {
    ScreenEvent::Input(InputEvent {
        at: Tick::default(),
        source: Source::Sdl,
        kind: InputKind::Key { key: Key::Right, sym: 0, wcode: 0, edge: Edge::Down, at_edge: true },
    })
}

/// Decode and upload `path` through the real render cache, as the poster workers and the frame's
/// PREPARE step would: from here on a draw probe of it resolves to a resident texture.
fn deliver_hero_art(path: &str) {
    struct StubUp(u32);
    impl plx_ui::tex::Uploader for StubUp {
        fn upload(&mut self, d: &plx_ui::tex::Decoded) -> plx_ui::tex::Tex {
            self.0 += 1;
            plx_ui::tex::Tex { id: 100 + self.0, w: d.w, h: d.h }
        }
        fn warm(&mut self, _: plx_ui::tex::Tex) {}
        fn free(&mut self, _: plx_ui::tex::Tex) {}
    }
    let key = HERO_ART.with(|a| {
        let mut a = a.borrow_mut();
        if let Some(i) = a.delivered.iter().position(|p| p == path) {
            return i;
        }
        a.delivered.push(path.into());
        a.delivered.len() - 1
    });
    plx_ui::tex::accept(plx_ui::tex::PosterReady {
        key: plx_machine::machine::PosterKey(key as u32),
        result: Ok(plx_ui::tex::Decoded { w: 16, h: 9, rgba: vec![0; 16 * 9 * 4].into_boxed_slice() }),
    });
    let mut budget = plx_ui::frame::Budget::new();
    budget.begin_frame(0);
    let mut present = plx_machine::present::Present::new();
    let mut handle = plx_machine::machine::PresentHandle::of(&mut present);
    let mut up = StubUp(key as u32 * 10);
    plx_ui::tex::prepare(&mut budget, &mut up, &mut handle, || 0);
}

/// **Owner field report (2026-09-30): a MANUAL hero flip blinks; the 8 s auto-advance never does.**
/// A phone capture showed the outgoing backdrop sliding off over the flat ground and the new one
/// fading in only afterwards: the incoming backdrop was not resident when the flip began.
///
/// Both paths call the same `flip`, so the difference was purely WHEN the neighbour's backdrop had
/// been asked for. The ±1 prefetch was a speculative `warm` that ran only on a settled billboard
/// AND a completely idle poster pipeline — never while a shelf was still loading, never during the
/// slide — and a warmed slot is the source's first LRU victim, which a warm never revives. Eight
/// seconds of idling hid all of that from the auto-advance; a press a second after the previous
/// flip found nothing requested.
///
/// The billboard now holds both neighbours' backdrops as DRAW demand (the one request path the
/// source LRU-protects and re-arms) from the moment an index is shown, busy pipeline or not, and
/// re-arms the new neighbours on the flip itself — the same mechanism for both paths.
#[test]
fn a_manual_hero_flip_lands_on_a_preloaded_backdrop_with_no_ground_frame() {
    let _guard = plx_base::testlock::serial();
    plx_ui::tex::install(&HeroArtSpy);
    plx_ui::tex::reset_for_test(64 << 20);
    HERO_ART.with(|a| *a.borrow_mut() = HeroArtState { busy: true, ..Default::default() });

    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 4, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let view = snapshot.view();
    let n = view.hero_count();
    assert!(n >= 3, "the test needs two distinct neighbours, got {n} hero slots");
    let art = |i: usize| view.hero(i % n).unwrap().item.art.clone();
    let tick = hero_tick;

    let mut s = screen(view);
    assert_eq!(s.carousel_index(view), 0);
    let _ = step(&mut s, view, None, &tick(16));
    let probed = HERO_ART.with(|a| a.borrow().probed.clone());
    for (which, i) in [("next", 1), ("previous (wrapped)", n - 1)] {
        assert!(
            probed.contains(&art(i)),
            "showing hero 0 with shelf posters still loading must already request the {which} \
             backdrop {} as demand; draw probes were {probed:?}",
            art(i)
        );
    }

    // The workers finish what was asked for.
    for path in probed.iter().collect::<std::collections::BTreeSet<_>>() {
        deliver_hero_art(path);
    }
    // Two seconds on hero 0: its own backdrop fades fully in, well inside the 8 s countdown.
    for frame in 2..=125u32 {
        let _ = step(&mut s, view, None, &tick(frame * 16));
    }
    assert!(s.outgoing.is_none(), "no flip yet");
    assert!(reveal(s.backdrop.tex.0, &s.backdrop.art) > 0.999, "hero 0's backdrop is up");

    // A MANUAL flip: Right at the edge of the hero's controls.
    let play = FocusKey { entry: s.entry, elem: HERO_PLAY_ELEM };
    let right = hero_edge_right();
    let (handled, _, _) = step(&mut s, view, Some(play), &right);
    assert_eq!(handled, Handled::Yes, "Right at the hero's edge flips the billboard");
    HERO_ART.with(|a| a.borrow_mut().probed.clear());
    let _ = step(&mut s, view, Some(play), &tick(126 * 16));

    assert!(s.outgoing.is_some(), "the slide is running");
    assert_eq!(s.carousel_index(view), 1);
    let incoming_a = reveal(s.backdrop.tex.0, &s.backdrop.art);
    let outgoing_a = reveal(s.backdrop.outgoing_tex.0, &s.backdrop.outgoing_art);
    assert!(
        s.backdrop.tex.0 != 0 && incoming_a > 0.999,
        "the incoming backdrop must slide in fully revealed (tex {}, alpha {incoming_a}), not \
         fade in over the ground after the slide",
        s.backdrop.tex.0
    );
    assert!(
        wash_hidden(s.snap.pos, incoming_a, Some(outgoing_a)),
        "no frame of the flip may show the flat ground (incoming {incoming_a}, outgoing {outgoing_a})"
    );

    // …and the new neighbours are asked for on the flip itself, not after the slide settles.
    let probed = HERO_ART.with(|a| a.borrow().probed.clone());
    assert!(
        probed.contains(&art(2)),
        "mid-slide, hero 1's next backdrop {} must already be requested; probes were {probed:?}",
        art(2)
    );
    assert!(probed.contains(&art(0)), "…and its previous one, the outgoing hero");
}

/// **Owner field report (2026-09-30, after 4fbeacfd6): the FIRST frame of a manual flip draws the
/// wrong backdrop.** Phone frames at 30 fps: pressing Right on Top Gear while the previous flip's
/// slide was still settling showed Top Gear's logo and text over FAMILY GUY's backdrop — the hero
/// from two flips ago — full-bright for one frame; pressing on a settled billboard showed one
/// frame of the outgoing hero's text over the bare wash.
///
/// The mechanism is frame ORDER, not texture residency. A manual flip is the hero's at-edge
/// Right/Left, which the dispatcher re-delivers to the page only after the focus engine reports
/// the edge — behind the frame's `Tick` in the queue (`ui::dispatch`, `Outcome::Edge`). So the
/// press frame ran `Backdrop::update` BEFORE `flip`: the art layers were resolved for the old
/// (outgoing, selected) pair, and the draw then read the NEW pair's slide offsets. The outgoing
/// slot at x = 0 drew whatever the previous tick had called outgoing — the hero two flips back
/// mid-slide, nothing at all on a settled billboard — and the real outgoing art was bound to the
/// incoming slot, a full screen off to the side. The auto-advance flips inside `tick`, before
/// `update`, which is why it never showed this.
///
/// The test drives the dispatcher's order: tick, THEN the at-edge press, then the draw, and reads
/// which textures the recording painter was handed.
#[test]
fn the_first_frame_of_a_manual_flip_draws_only_the_outgoing_and_incoming_backdrops() {
    let _guard = plx_base::testlock::serial();
    plx_ui::tex::install(&HeroArtSpy);
    plx_ui::tex::reset_for_test(64 << 20);
    HERO_ART.with(|a| *a.borrow_mut() = HeroArtState::default());

    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 4, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let view = snapshot.view();
    let n = view.hero_count();
    assert!(n >= 3, "the test needs three distinct heroes, got {n}");
    let hero = |i: usize| view.hero(i % n).unwrap();
    for i in 0..n {
        deliver_hero_art(&hero(i).item.art);
    }
    let tex_of = |i: usize| {
        let h = hero(i);
        plx_ui::widgets::resolve_tex_wh_on(h.item.sid.raw(), &h.item.art, 1280, 720, 0).0
    };
    let who = |tex: u32| (0..n).find(|&i| tex_of(i) == tex);
    let tick = hero_tick;
    let play = |s: &HomeScreen| FocusKey { entry: s.entry, elem: HERO_PLAY_ELEM };
    let right = hero_edge_right();
    // What the frame puts on screen: every backdrop-sized textured quad that reaches the panel.
    let backdrops = |s: &mut HomeScreen| {
        let context = cx(view, Some(play(s)));
        plx_ui::draw_census::capture_tex(|| {
            let mut f = DrawFrame::new(&context, Painter::recording());
            plx_gfx::gfx::without_frame_clear(|| Screen::<TestHost>::draw(s, &mut f));
        })
        .into_iter()
        .filter(|(_, r, a)| r.w >= SCR_W && *a > 0.01 && r.x < SCR_W && r.x + r.w > 0.0)
        .collect::<Vec<_>>()
    };

    let mut s = screen(view);
    let mut ms = 0u32;
    let mut frames = |s: &mut HomeScreen, count: u32| {
        for _ in 0..count {
            ms += 16;
            let key = play(s);
            let _ = step(s, view, Some(key), &tick(ms));
        }
        ms
    };
    // Two seconds on hero 0: its backdrop is fully up.
    frames(&mut s, 125);
    assert!(s.outgoing.is_none());

    // Press 1 on a settled billboard, then press 2 while slide 1 is still settling (past the
    // flip cooldown, short of the rest threshold) — the owner's rhythm.
    for (press, settle) in [(1usize, 30u32), (2, 0)] {
        frames(&mut s, 1);
        let key = play(&s);
        let (handled, _, _) = step(&mut s, view, Some(key), &right);
        assert_eq!(handled, Handled::Yes, "press {press}: Right at the hero's edge flips");
        assert_eq!(s.carousel_index(view), press % n);
        let drawn = backdrops(&mut s);
        let (outgoing, incoming) = (press - 1, press);
        for &(tex, r, a) in &drawn {
            assert!(
                who(tex) == Some(outgoing % n) || who(tex) == Some(incoming % n),
                "press {press}: the first flip frame drew hero {:?}'s backdrop (tex {tex} at x \
                 {}, alpha {a}); only the outgoing hero {outgoing} and the incoming hero \
                 {incoming} may be on screen",
                who(tex),
                r.x
            );
        }
        assert!(
            drawn.iter().any(|&(tex, r, a)| who(tex) == Some(outgoing % n) && r.x.abs() < 1.0 && a > 0.99),
            "press {press}: the outgoing hero {outgoing}'s backdrop must still fill the panel on \
             the flip's first frame, not the bare ground; drew {drawn:?}"
        );
        frames(&mut s, settle);
        if settle > 0 {
            assert!(s.outgoing.is_some(), "press 2 must land while slide 1 is still running");
            assert_eq!(s.hero_flip_cd, 0.0, "…and past the flip cooldown");
        }
    }
}

/// The page-freeze holds Home's snapshot until the page is quiescent so first poster admission can
/// settle behind it. The ambient wash dissolve and the hero focus pop are decorative — neither
/// moves layout or the grid — so they must be visible pixel motion (`page_moving`) yet not page
/// layout motion (`page_layout_moving`), or Home rides the whole 600 ms hold cap.
#[test]
fn decorative_wash_and_hero_pop_do_not_hold_page_quiescence() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    s.layout_grid();
    let key = FocusKey { entry: s.entry, elem: HERO_PLAY_ELEM };
    let mut saw_decor = false;
    let mut layout_moving_frames = 0;
    for i in 1..40u32 {
        plx_machine::idle::frame_begin(1.0 / 60.0);
        step(&mut s, snapshot.view(), Some(key), &ScreenEvent::Tick(Tick { ms: i * 16, dt_us: 16_667 }));
        saw_decor |= plx_machine::idle::page_moving();
        layout_moving_frames += usize::from(plx_machine::idle::page_layout_moving());
    }
    assert!(saw_decor, "the hero pop / wash dissolve must still report visible page motion");
    assert_eq!(layout_moving_frames, 0, "decorative springs must not count as page layout motion");
}

#[test]
fn locate_agrees_with_a_row_scan() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_grid_for_test(&mut state, &adapter, 5, 7);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let s = screen(snapshot.view());
    let scan = |elem: u32| s.rows.iter().enumerate().find_map(|(row, hub)| {
        hub.elems.iter().position(|&e| e == elem).map(|col| Located::Item(row, col))
    });
    let mut seen = 0;
    for hub in &s.rows {
        for &elem in &hub.elems {
            assert_eq!(s.locate(elem), scan(elem));
            seen += 1;
        }
    }
    assert_eq!(seen, 35);
    // A key the table holds but no row lists (a card that left the catalog), and one it never held.
    for stale in [s.next_elem, s.next_elem + 1] {
        assert_eq!(s.locate(stale), scan(stale));
    }
}

/// Publish a `rows` x `items` grid into `s`, as one more provider publication.
fn republish(s: &mut HomeScreen, state: &mut plx_data::pms::PmsState, adapter: &std::sync::Arc<plx_data::pms::PmsAdapter>, rows: usize, items: usize) {
    plx_data::pms::seed_grid_for_test(state, adapter, rows, items);
    let snapshot = plx_data::pms::hubs_snapshot(state);
    s.sync_catalog(&cx(snapshot.view(), None));
}

/// Every shelf rests where `layout_grid` puts it for the current dive and scroll.
fn assert_shelves_laid_out(s: &HomeScreen) {
    let top = PEEK_Y + (GRID_TOP_Y - PEEK_Y) * s.snap.pos;
    let mut flow = 0.0;
    for (row, shelf) in s.grid.shelves.iter().enumerate() {
        let want = top + flow - s.grid.scroll_y.pos * s.snap.pos;
        assert_eq!(shelf.base_y(), want, "row {row} is not where the layout puts it");
        flow += ui_cards::ROW_PITCH_FIXED + shelf.under_band();
    }
}

/// A catalog that lands on an event (`Mount`, `StoreChanged`) is drawn before the next `Tick`'s
/// `update_grid` has laid it out; a fresh `CardRow` rests at `base_y = 0`, which is the strip of
/// tiles drawn over the hero for one frame. The shelves must be placed by the landing itself.
#[test]
fn a_catalog_landing_on_an_event_is_laid_out_before_any_tick() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    plx_data::pms::seed_grid_for_test(&mut state, &adapter, 3, 4);
    let first = plx_data::pms::hubs_snapshot(&state);
    step(&mut s, first.view(), None, &ScreenEvent::Mount);
    assert_eq!(s.grid.shelves.len(), 3);
    assert_shelves_laid_out(&s);
    assert_eq!(s.grid.shelves[0].base_y(), PEEK_Y, "the billboard shows the first row as a peek");

    plx_data::pms::seed_grid_for_test(&mut state, &adapter, 5, 4);
    let wider = plx_data::pms::hubs_snapshot(&state);
    let ord = StoreId::Hubs.ord();
    step(&mut s, wider.view(), None, &ScreenEvent::StoreChanged(ord, wider.view().generation));
    assert_eq!(s.grid.shelves.len(), 5);
    assert_shelves_laid_out(&s);
}

#[test]
fn item_keys_are_stable_across_republication() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    republish(&mut s, &mut state, &adapter, 4, 5);
    let first: Vec<Vec<u32>> = s.rows.iter().map(|r| r.elems.clone()).collect();
    let table = s.items.clone();
    // The same catalog again, and a wider one: no card already keyed changes its element, and the
    // new cards take the next elements in publication order.
    republish(&mut s, &mut state, &adapter, 4, 5);
    assert_eq!(s.rows.iter().map(|r| r.elems.clone()).collect::<Vec<_>>(), first);
    assert_eq!(s.items, table);
    let next = s.next_elem;
    republish(&mut s, &mut state, &adapter, 6, 5);
    for (row, elems) in first.iter().enumerate() {
        assert_eq!(&s.rows[row].elems, elems);
    }
    assert_eq!(s.items[..table.len()].iter().map(|k| k.elem).collect::<Vec<_>>(),
        table.iter().map(|k| k.elem).collect::<Vec<_>>());
    let fresh: Vec<u32> = s.rows[4..].iter().flat_map(|r| r.elems.iter().copied()).collect();
    assert_eq!(fresh, (next..next + 10).collect::<Vec<_>>());
}

#[test]
fn restore_merges_saved_keys_without_duplicates() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let mut a = HomeScreen::new(EntryId(7), InstanceId(9));
    republish(&mut a, &mut state, &adapter, 3, 4);
    let memory = match <HomeScreen as Screen<TestHost>>::memory(&a) {
        PageMemory::Home(m) => m,
        _ => unreachable!(),
    };
    // Restoring into an empty screen reproduces the table; restoring again, or into the screen
    // that wrote it, adds nothing.
    let mut b = HomeScreen::new(EntryId(7), InstanceId(10));
    b.restore(&memory);
    assert_eq!(b.items, a.items);
    assert_eq!(b.groups, a.groups);
    b.restore(&memory);
    assert_eq!((b.items.len(), b.groups.len()), (a.items.len(), a.groups.len()));
    let before = a.items.clone();
    a.restore(&memory);
    assert_eq!(a.items, before);
    // A screen that restored first republishes onto the saved elements.
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    b.sync_catalog(&cx(snapshot.view(), None));
    assert_eq!(b.rows.iter().map(|r| r.elems.clone()).collect::<Vec<_>>(),
        a.rows.iter().map(|r| r.elems.clone()).collect::<Vec<_>>());
    assert_eq!(b.items.len(), a.items.len());
}

#[test]
fn key_index_agrees_with_a_scan_of_the_table() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    for (rows, items) in [(3, 4), (5, 4), (2, 6)] {
        republish(&mut s, &mut state, &adapter, rows, items);
    }
    let memory = match <HomeScreen as Screen<TestHost>>::memory(&s) {
        PageMemory::Home(m) => m,
        _ => unreachable!(),
    };
    let mut r = HomeScreen::new(EntryId(7), InstanceId(10));
    r.restore(&memory);
    republish(&mut r, &mut state, &adapter, 4, 4);
    for screen in [&s, &r] {
        for key in screen.items.iter() {
            assert_eq!(screen.find_item(&key.identity), screen.items.iter().position(|k| k.identity == key.identity));
        }
        for key in &screen.groups {
            assert_eq!(screen.find_group(&key.identity), screen.groups.iter().position(|k| k.identity == key.identity));
        }
        assert_eq!(screen.item_index.values().map(HubKeys::len).sum::<usize>(), screen.items.len());
        assert_eq!(screen.group_index.len(), screen.groups.len());
    }
}

#[test]
fn stops_are_recorded_only_for_rows_on_screen_or_focused() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_grid_for_test(&mut state, &adapter, 16, 3);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    s.layout_grid();
    let on_screen = |s: &HomeScreen, row: usize| {
        let shelf = &s.grid.shelves[row];
        let top = heading_y(shelf.base_y(), shelf.heading_lift());
        on_axis(top, shelf.base_y() + CARD_H + shelf.under_band() - top, SCR_H, 0.0)
    };
    let visible: Vec<usize> = (0..s.rows.len()).filter(|&r| on_screen(&s, r)).collect();
    assert!(!visible.is_empty() && visible.len() < s.rows.len(), "the fixture must straddle the screen edge");
    let far = s.rows.len() - 1;
    assert!(!visible.contains(&far));
    let elems = |s: &HomeScreen, rows: &[usize]| {
        let mut e: Vec<u32> = rows.iter().flat_map(|&r| s.rows[r].elems.clone()).collect();
        e.sort_unstable();
        e
    };
    for (focus_row, expected_rows) in [
        (None, visible.clone()),
        (Some(far), { let mut v = visible.clone(); v.push(far); v }),
    ] {
        let focus = focus_row.map(|r| FocusKey { entry: s.entry, elem: s.rows[r].elems[0] });
        let context = cx(snapshot.view(), focus);
        let mut frame = DrawFrame::new(&context, Painter::root());
        s.record_stops(&mut frame, snapshot.view());
        let mut recorded: Vec<u32> = frame.into_stops().iter()
            .map(|stop| stop.key.elem)
            .filter(|e| e & HEADING_BASE == 0)
            .collect();
        recorded.sort_unstable();
        assert_eq!(recorded, elems(&s, &expected_rows));
    }
}

/// Publish `hubs` named hubs of `items` cards into `s`; an empty id (and no key) makes the hub
/// `Ephemeral`.
fn republish_named(
    s: &mut HomeScreen,
    state: &mut plx_data::pms::PmsState,
    adapter: &std::sync::Arc<plx_data::pms::PmsAdapter>,
    hubs: &[&str],
    items: usize,
    focus: Option<FocusKey<u32>>,
) {
    let rows: Vec<(&str, &str, &str)> = hubs.iter().map(|id| (*id, "", "Row")).collect();
    plx_data::pms::seed_named_hubs_for_test(state, adapter, items, &rows);
    let snapshot = plx_data::pms::hubs_snapshot(state);
    s.sync_catalog(&cx(snapshot.view(), focus));
}

fn assert_indexes_agree(s: &HomeScreen) {
    for key in s.items.iter() {
        assert_eq!(s.find_item(&key.identity), s.items.iter().position(|k| k.identity == key.identity));
    }
    for key in &s.groups {
        assert_eq!(s.find_group(&key.identity), s.groups.iter().position(|k| k.identity == key.identity));
    }
    assert_eq!(s.item_index.values().map(HubKeys::len).sum::<usize>(), s.items.len());
    assert_eq!(s.group_index.len(), s.groups.len());
}

#[test]
fn volatile_keys_do_not_accumulate_across_publications() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    for _ in 0..20 {
        republish_named(&mut s, &mut state, &adapter, &["", "", ""], 4, None);
    }
    // Only the shown publication and the one before it are kept.
    assert!(s.items.len() <= 2 * 12, "{} keys after 20 publications", s.items.len());
    assert!(s.groups.len() <= 3, "{} groups after 20 publications", s.groups.len());
    assert_eq!(s.rows.len(), 3);
    assert_indexes_agree(&s);
}

#[test]
fn the_focused_vanished_key_survives_a_prune() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    republish_named(&mut s, &mut state, &adapter, &[""], 4, None);
    let (focused, other) = (s.rows[0].elems[1], s.rows[0].elems[2]);
    let focus = Some(FocusKey { entry: EntryId(7), elem: focused });
    for _ in 0..4 {
        republish_named(&mut s, &mut state, &adapter, &[""], 4, focus);
    }
    assert!(s.items.iter().any(|k| k.elem == focused), "the focused key was pruned");
    assert!(!s.items.iter().any(|k| k.elem == other), "an unfocused vanished key was kept");
    assert_indexes_agree(&s);
}

#[test]
fn focus_recovers_to_the_same_row_after_its_card_rotates_out() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    republish(&mut s, &mut state, &adapter, 4, 5);
    let focused = s.rows[2].elems[3];
    let focus = FocusKey { entry: EntryId(7), elem: focused };
    // The card (rk "4" in every row) leaves the catalog, and a few publications pass.
    for _ in 0..4 {
        plx_data::pms::seed_grid_for_test(&mut state, &adapter, 4, 5);
        plx_data::pms::remove_test_item(&mut state, "4");
        let snapshot = plx_data::pms::hubs_snapshot(&state);
        s.sync_catalog(&cx(snapshot.view(), Some(focus)));
    }
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    assert_eq!(s.locate(focused), None, "the card is gone");
    let got = Focusable::<TestHost>::reconcile(&s, focus, &cx(snapshot.view(), Some(focus)));
    assert!(s.rows[2].elems.contains(&got.elem), "recovered outside the row");
}

#[test]
fn memory_shares_the_key_table() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    republish(&mut s, &mut state, &adapter, 4, 5);
    let memory = match <HomeScreen as Screen<TestHost>>::memory(&s) {
        PageMemory::Home(m) => m,
        _ => unreachable!(),
    };
    assert!(Arc::ptr_eq(&memory.items, &s.items));
    // The same catalog republished moves no card and mints no key: still one table.
    republish(&mut s, &mut state, &adapter, 4, 5);
    assert!(Arc::ptr_eq(&memory.items, &s.items), "an unchanged republication copied the table");
    // A new row writes the table, which must not reach what memory took.
    let before = memory.items.len();
    republish(&mut s, &mut state, &adapter, 5, 5);
    assert!(!Arc::ptr_eq(&memory.items, &s.items));
    assert_eq!(memory.items.len(), before);
    assert_eq!(s.items.len(), before + 5);
}

/// A Home whose hubs carry identifiers and whose cards carry rating keys never loses a key while
/// it stays under the cap: the table is exactly what it was before pruning existed (replay
/// fixtures hash it).
#[test]
fn a_stable_home_under_the_cap_prunes_nothing() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    let mut table: Vec<HomeItemKey> = Vec::new();
    for (rows, items) in [(3, 4), (5, 4), (2, 6), (5, 4), (1, 1), (4, 5), (4, 5)] {
        republish(&mut s, &mut state, &adapter, rows, items);
        // Every earlier key is still there, in the same place, with the same elem; only
        // `last_row`/`last_col` follow the cards.
        assert!(s.items.len() >= table.len());
        for (kept, old) in s.items.iter().zip(&table) {
            assert_eq!((&kept.identity, kept.elem), (&old.identity, old.elem));
        }
        table = s.items.to_vec();
        assert_indexes_agree(&s);
    }
}

/// Focus walking down the page leaves every row it visited parked at exact rest once the motion
/// settles, so `update_grid` skips them: a visited row used to stall one ulp from rest and be
/// stepped on every frame for the rest of the session.
#[test]
fn walking_focus_down_leaves_every_released_row_at_exact_rest() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    let rows = 12;
    republish(&mut s, &mut state, &adapter, rows, 4);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    let dt = 1.0 / 60.0;
    for row in 0..rows {
        let context = cx(snapshot.view(), Some(FocusKey { entry: EntryId(7), elem: s.rows[row].elems[0] }));
        for _ in 0..30 {
            s.update_grid_for_test(snapshot.view(), &context, dt);
        }
    }
    let context = cx(snapshot.view(), Some(FocusKey { entry: EntryId(7), elem: s.rows[rows - 1].elems[0] }));
    for _ in 0..120 {
        s.update_grid_for_test(snapshot.view(), &context, dt);
    }
    for row in 0..rows - 1 {
        assert!(s.grid.shelves[row].at_rest(), "row {row} never parked");
    }
    assert!(!s.grid.shelves[rows - 1].at_rest(), "the focused row is live");
}

#[test]
fn stable_keys_are_bounded_oldest_first() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    let ids: Vec<String> = (0..100).map(|i| format!("hub.{i}")).collect();
    for id in &ids {
        republish_named(&mut s, &mut state, &adapter, &[id.as_str()], 4, None);
    }
    let cap = 2 * 4 + 256;
    assert!(s.items.len() <= cap, "{} keys", s.items.len());
    assert!(s.items.len() > 4 * 2, "the window is not just the last two publications");
    // The newest survive and the table is still in elem order.
    let elems: Vec<u32> = s.items.iter().map(|k| k.elem).collect();
    assert!(elems.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(elems.last().copied(), s.rows[0].elems.last().copied());
    assert_indexes_agree(&s);
}

// ---- The standing "Hold OK for options" hint (`plx_ui::hold_hint`) ---------------------------------
//
// The schedule itself is `hold_hint`'s own tests; these are Home's half: WHICH focus and WHICH
// frames feed it. One 16 ms tick is `HINT_TICK_MS`.

const HINT_TICK_MS: u32 = 16;

/// A Home seated on the grid with `rows` shelves, nothing focused yet.
fn hint_home(view: HubsView<'_>) -> HomeScreen {
    plx_ui::hold_hint::reset_learned_for_test();
    let mut s = screen(view);
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    s
}

/// Tick `frames` times with `focus` and a card held `held_ms` (None = no press), from `ms0`;
/// returns the next tick's clock.
fn hint_ticks(
    s: &mut HomeScreen,
    view: HubsView<'_>,
    focus: Option<FocusKey<u32>>,
    held_ms: Option<u32>,
    ms0: u32,
    frames: u32,
) -> u32 {
    let mut context = cx(view, focus);
    context.press.held_ms = held_ms;
    let mut present = plx_machine::present::Present::new();
    let mut out = Vec::new();
    let mut ms = ms0;
    for _ in 0..frames {
        ms += HINT_TICK_MS;
        let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(9)), &mut present);
        let tick = ScreenEvent::Tick(Tick { ms, dt_us: HINT_TICK_MS * 1000 });
        Machine::<TestHost>::step(s, &tick, &context, &mut fx);
    }
    ms
}

fn hint_secs(secs: f32) -> u32 {
    (secs * 1000.0 / HINT_TICK_MS as f32).round() as u32
}

fn second_card(s: &HomeScreen) -> FocusKey<u32> {
    FocusKey { entry: s.entry, elem: s.rows[0].elems[1] }
}

/// Run `body` against a hint-ready Home over three ready shelves, holding the serial lock the
/// shared stores and the idle gate need for the whole test.
fn with_hint_home(body: impl FnOnce(HomeScreen, HubsView<'_>)) {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let view = snapshot.view();
    body(hint_home(view), view);
}

#[test]
fn the_hint_appears_only_after_focus_has_rested_on_a_shelf_tile() {
    with_hint_home(|mut s, view| {
        let card = Some(first_card(&s));
        let ms = hint_ticks(&mut s, view, card, None, 0, hint_secs(1.2));
        assert!(!s.hold_hint.visible(), "not before the dwell");
        hint_ticks(&mut s, view, card, None, ms, hint_secs(4.0));
        assert!(s.hold_hint.visible(), "once focus has rested, the hint stands");
        assert!(s.hold_hint.opacity() > 0.9);
    });
}

#[test]
fn the_hint_leaves_on_its_own_and_a_focus_move_rearms_it() {
    with_hint_home(|mut s, view| {
        let card = Some(first_card(&s));
        let mut ms = hint_ticks(&mut s, view, card, None, 0, hint_secs(5.0));
        assert!(s.hold_hint.visible());
        ms = hint_ticks(&mut s, view, card, None, ms, hint_secs(9.0));
        assert!(!s.hold_hint.visible(), "its life is spent");
        // moving to the next tile starts the dwell again, and the new tile gets its own life
        let next = Some(second_card(&s));
        ms = hint_ticks(&mut s, view, next, None, ms, hint_secs(1.0));
        assert!(!s.hold_hint.visible(), "inside the new tile's dwell");
        hint_ticks(&mut s, view, next, None, ms, hint_secs(5.0));
        assert!(s.hold_hint.visible(), "back after the dwell on the new tile");
    });
}

#[test]
fn the_hint_never_shows_off_the_shelf_or_under_a_menu() {
    with_hint_home(|mut s, view| {
        let strip = Some(FocusKey { entry: s.entry, elem: STRIP_MOVIES_ELEM });
        let hero = Some(FocusKey { entry: s.entry, elem: HERO_PLAY_ELEM });
        let mut ms = hint_ticks(&mut s, view, strip, None, 0, hint_secs(5.0));
        assert!(!s.hold_hint.visible(), "tab strip focused");
        ms = hint_ticks(&mut s, view, hero, None, ms, hint_secs(5.0));
        assert!(!s.hold_hint.visible(), "hero action focused");
        ms = hint_ticks(&mut s, view, None, None, ms, hint_secs(5.0));
        assert!(!s.hold_hint.visible(), "no focus");
        let card = Some(first_card(&s));
        ms = hint_ticks(&mut s, view, card, None, ms, hint_secs(5.0));
        assert!(s.hold_hint.visible(), "control: the same page does show it on a tile");
        // a menu opens over the page: the container covers Home
        step(&mut s, view, card, &ScreenEvent::Cover);
        ms = hint_ticks(&mut s, view, card, None, ms, hint_secs(1.5));
        assert!(!s.hold_hint.visible(), "never while a menu or modal is open");
        step(&mut s, view, card, &ScreenEvent::Uncover);
        hint_ticks(&mut s, view, card, None, ms, hint_secs(1.0));
        assert!(!s.hold_hint.visible(), "and the dwell starts over once it closes");
    });
}

#[test]
fn the_hint_never_shows_while_the_page_is_still_gliding() {
    with_hint_home(|mut s, view| {
        let card = Some(first_card(&s));
        hint_ticks(&mut s, view, card, None, 0, hint_secs(3.0));
        assert!(s.hold_hint.visible());
        // a shelf scroll still in flight under the same tile holds the dwell at zero
        s.grid.scroll_target = s.grid.scroll_y.pos + 400.0;
        let moving = s.hint_input(&cx(view, card), false);
        assert!(!moving.settled, "a pending vertical reveal is not rest");
        let at_rest_input = {
            s.grid.scroll_target = s.grid.scroll_y.pos;
            s.hint_input(&cx(view, card), false)
        };
        assert!(at_rest_input.settled);
        assert!(!s.hint_input(&cx(view, card), true).settled, "a snap in flight is not rest");
    });
}

#[test]
fn a_held_ok_fills_the_cap_from_the_press_clock_and_shows_the_hint_early() {
    with_hint_home(|mut s, view| {
        let card = Some(first_card(&s));
        let half = plx_ui::press::LONG_MS / 2;
        hint_ticks(&mut s, view, card, Some(half), 0, 30);
        assert!(s.hold_hint.visible(), "the hold shows it without waiting out the dwell");
        assert!((s.hold_hint.fill() - 0.5).abs() < 1e-6, "half of LONG_MS held is a half-filled cap");
        let input = s.hint_input(&{ let mut c = cx(view, card); c.press.held_ms = Some(plx_ui::press::LONG_MS * 2); c }, false);
        assert_eq!(input.hold, Some(1.0), "the constructor clamps the fraction; no screen divides");
        // released early: the fill drains
        hint_ticks(&mut s, view, card, None, 0, hint_secs(0.4));
        assert_eq!(s.hold_hint.fill(), 0.0);
    });
}

#[test]
fn the_hint_is_retired_for_good_once_a_menu_has_been_opened() {
    with_hint_home(|mut s, view| {
        let card = Some(first_card(&s));
        let ms = hint_ticks(&mut s, view, card, None, 0, hint_secs(5.0));
        assert!(s.hold_hint.visible());
        plx_ui::hold_hint::mark_learned();
        let ms = hint_ticks(&mut s, view, card, None, ms, hint_secs(1.5));
        assert!(!s.hold_hint.visible(), "learned: a standing hint goes");
        let other = Some(second_card(&s));
        hint_ticks(&mut s, view, other, Some(100), ms, hint_secs(10.0));
        assert!(!s.hold_hint.visible(), "and neither dwell nor a hold brings it back");
        plx_ui::hold_hint::reset_learned_for_test();
    });
}

#[test]
fn a_settled_hint_lets_the_gate_close() {
    with_hint_home(|mut s, view| {
        let card = Some(first_card(&s));
        let ms = hint_ticks(&mut s, view, card, None, 0, hint_secs(6.0));
        assert!(s.hold_hint.visible());
        // Home settled with the hint standing still: nothing in the tick may ask for a present.
        let context = cx(view, card);
        let mut present = plx_machine::present::Present::new();
        present.take(0);
        let mut out = Vec::new();
        let mut asked = 0;
        for i in 1..=60u32 {
            {
                let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(9)), &mut present);
                let tick = ScreenEvent::Tick(Tick { ms: ms + i * HINT_TICK_MS, dt_us: HINT_TICK_MS * 1000 });
                Machine::<TestHost>::step(&mut s, &tick, &context, &mut fx);
            }
            asked += u32::from(present.take(0));
        }
        assert_eq!(asked, 0, "a hint at rest must not hold the loop at full rate");
    });
}

/// The screenshot pipeline's `stillclock` pins every time-driven animation, and the standing hint
/// is one: it must not appear on its own in a held-clock capture, while a real hold still shows it.
#[cfg(feature = "devtriggers")]
#[test]
fn the_hint_does_not_stand_up_by_itself_while_the_screenshot_clocks_are_held() {
    with_hint_home(|mut s, view| {
        let card = Some(first_card(&s));
        plx_machine::motion::hold_phase_clocks(Some(4000));
        let ms = hint_ticks(&mut s, view, card, None, 0, hint_secs(8.0));
        assert!(!s.hold_hint.visible(), "pinned clocks: no self-appearing hint in a documentation figure");
        hint_ticks(&mut s, view, card, Some(100), ms, 30);
        assert!(s.hold_hint.visible(), "a real hold is input and still shows it");
        plx_machine::motion::hold_phase_clocks(None);
    });
}

/// Home's focus groups are what the focus engine's remembered cursors and the stored session are
/// keyed on: moving a row onto `cards::Shelf` does not renumber them, reorder them or change where
/// a fresh page puts focus.
#[test]
fn the_focus_groups_and_the_first_focus_keep_their_ids_and_order() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_grid_for_test(&mut state, &adapter, 3, 4);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let s = screen(snapshot.view());
    let mut groups = Vec::new();
    Focusable::<TestHost>::groups(&s, &cx(snapshot.view(), None), &mut groups);
    let ids: Vec<u32> = groups.iter().map(|g| g.id.0).collect();
    assert_eq!(ids, [0, FIRST_HUB_GROUP, FIRST_HUB_GROUP + 1, FIRST_HUB_GROUP + 2], "hero, then one group per row in order");
    assert_eq!(groups[0].seat, Seat::First, "a fresh page opens on the hero");
    let first = s.rows[0].elems[0];
    assert_eq!(first, FIRST_ITEM_ELEM, "the first card keeps the first element id");
    assert_eq!(s.rows[0].elems.iter().map(|e| e - FIRST_ITEM_ELEM).collect::<Vec<_>>(), [0, 1, 2, 3]);
    assert_eq!(
        Focusable::<TestHost>::group_of(&s, &first, &cx(snapshot.view(), None)),
        Some(GroupId(FIRST_HUB_GROUP))
    );
    let from = Placed { rect: Rect::new(0.0, 0.0, 10.0, 10.0), rest_rect: Rect::new(0.0, 0.0, 10.0, 10.0),
        clip: Rect::FULL, index: Some(0) };
    assert_eq!(
        Focusable::<TestHost>::seat(&s, GroupId(FIRST_HUB_GROUP), from, &cx(snapshot.view(), None)).elem,
        first
    );
}

fn drawn_stops(s: &mut HomeScreen, context: &Cx<'_, TestHost>) -> Vec<Stop<u32>> {
    let mut f = DrawFrame::new(context, Painter::recording());
    plx_ui::screen::record_stops_while_recording(|| {
        plx_gfx::gfx::without_frame_clear(|| Screen::<TestHost>::draw(s, &mut f));
    });
    f.into_stops()
}

/// **The pointer's rect is the rect the screen's own `draw` registers**, for every card of every
/// row on screen (plain headings and a promoted collection's linked heading alike), popped or not,
/// and mid-press: the stops come out of the real `Screen::draw`, not a re-derivation, and equal
/// what `place(.., At::Drawn)` reports.
#[test]
fn every_shelf_cards_stop_from_the_real_draw_is_the_placed_rect() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let snapshot = collection_home(&mut state, &adapter);
    let mut s = screen(snapshot.view());
    on_grid(&mut s);
    let key = FocusKey { entry: s.entry, elem: s.rows[1].elems[1] };
    pop_card(&mut s, snapshot.view(), key, 80);
    for press in [1.0, 0.93] {
        let mut context = cx(snapshot.view(), Some(key));
        context.press.scale = press;
        let stops = drawn_stops(&mut s, &context);
        let mut checked = 0;
        for row in 0..s.rows.len() {
            if !s.shelf_on_screen(row) {
                continue;
            }
            for &elem in &s.rows[row].elems {
                let Some(stop) = stops.iter().find(|stop| stop.key.elem == elem) else { continue };
                let placed = Focusable::<TestHost>::place(&s, &elem, &context, At::Drawn).unwrap();
                assert_eq!((stop.rect.x, stop.rect.y, stop.rect.w, stop.rect.h),
                    (placed.rect.x, placed.rect.y, placed.rect.w, placed.rect.h), "row {row} elem {elem}, press {press}");
                checked += 1;
            }
        }
        assert!(checked >= 4, "the stops under test are the page's, not an empty set: {checked}");
        let popped = stops.iter().find(|stop| stop.key == key).unwrap();
        assert!(popped.rect.w > CARD_W, "the focused card's stop is its popped rect");
    }
}
