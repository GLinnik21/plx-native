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
        self.update_grid(view, cx, cx.focus.current, &mut fx, Tick { ms: 0, dt_us: (dt * 1_000_000.0).round() as u32 });
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
        (STRIP_ACCOUNT_ELEM + 1, HomeReq::Tab(HomeTab::Watchlist)),
        (STRIP_ACCOUNT_ELEM + 2, HomeReq::Tab(HomeTab::LiveTv)),
    ];
    for (elem, want) in cases {
        assert_eq!(HomeScreen::request_for_strip(elem), Some(want));
    }
    assert_eq!(HomeScreen::request_for_strip(STRIP_ACCOUNT_ELEM + 3), None);
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

/// A shelf draws at its real offset whatever the dive is doing (it used to draw the offset TIMES the
/// dive, `CardSource::sweep`, which made a retained offset of thousands of pixels a fast horizontal
/// slide through the whole late dive). The late dive's card motion is the dive's vertical alone, and
/// the same for a shelf that holds an offset as for one that does not.
#[test]
fn a_retained_shelf_offset_adds_no_motion_to_the_late_dive() {
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
    assert_eq!(late_dive_frame(4000.0), Verdict::Settled, "the retained offset is held under the dive: it adds no horizontal motion");
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
fn pointer_hit_column_matches_the_drawn_card_at_every_scroll() {
    for scroll in [0.0, 415.0, 830.0] {
        for col in 0..24 {
            assert_eq!(
                col_at(card_x(col, scroll) + CARD_W * 0.5, scroll, 24),
                Some(col)
            );
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
        context.press.owner = Some(key);
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
    let top = plx_ui::landing_hero::hero_stack(band, META_H, Some(synopsis)).title.y;
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
        let cap = ui_cards::cw_caption(&tile_facts::of(&m)).expect("a Continue Watching episode always captions");
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
        ui_cards::cw_caption(&tile_facts::of(&mid)).unwrap().to_str().unwrap(),
        "Laura \u{00b7} 25m left"
    );
}

#[test]
fn a_continue_watching_episode_is_captioned_by_its_address_not_its_show() {
    let m = |kind: i32, ep_index: i32, resume_ms: i64| PmsMovie {
        kind,
        show_title: "Laura".into(),
        season_index: 2,
        ep_index,
        dur_ns: 45 * 60 * 1_000_000_000,
        resume_ms,
        ..Default::default()
    };
    let cap = |m: &PmsMovie| {
        ui_cards::cw_caption(&tile_facts::of(m)).unwrap().to_str().unwrap().to_owned()
    };
    // the poster is the show's own art, so the show's name adds nothing: the address leads
    assert_eq!(cap(&m(3, 5, 20 * 60_000)), "S2 \u{00b7} E5 \u{00b7} 25m left");
    assert_eq!(cap(&m(3, 5, 0)), "S2 \u{00b7} E5 \u{00b7} New episode");
    // an unnumbered episode still names its show
    assert_eq!(cap(&m(3, 0, 20 * 60_000)), "Laura \u{00b7} 25m left");
    // a movie: the time left alone
    assert_eq!(cap(&m(1, 0, 20 * 60_000)), "25m left");
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
        GRID_TOP_Y + shelf_top_settled(row, focus_row, band_open_of(false)) - scroll,
        lift,
    )
}
/// The heading's place on the page without the focus pop's lift: what the chrome's clearance rule
/// judges it at (`draw_grid`).
fn heading_place(row: usize, focus_row: usize, scroll: f32) -> f32 {
    heading_y(GRID_TOP_Y + shelf_top_settled(row, focus_row, band_open_of(false)) - scroll, 0.0)
}

#[test]
fn the_first_shelfs_raised_heading_settles_clear_of_the_profile_chip() {
    assert!(heading_top(0, 0, rest_scroll(0, 5, band_open_of(false))) >= top_band_bottom());
}

/// **At rest a heading is whole or it is gone, never half under the chrome**, for every focused
/// shelf of every column height: the rest is ONE scroll per focused shelf (no history, and no
/// Under posters band in it), a heading below the chrome is drawn whole, and any other is faded to
/// nothing by where it stands (`scroll_chrome`'s clearance). The old check graded a heading against
/// the band's BOTTOM edge only, so a heading standing in the band (54..130) under the chip passed.
#[test]
fn no_shelf_heading_settles_inside_the_shared_top_band() {
    for rows in 1..=SWEEP_ROWS {
        for focus_row in 0..rows {
            let scroll = rest_scroll(focus_row, rows, band_open_of(false));
            for row in 0..rows {
                let place = heading_place(row, focus_row, scroll);
                let clear = plx_ui::scroll_chrome::clearance_under_shown_chrome(place);
                assert!(clear == 0.0 || clear == 1.0,
                    "rows {rows}, focus {focus_row}: shelf {row}'s heading rests at {place}, {clear} clear of the chrome");
                if clear == 1.0 {
                    assert!(heading_top(row, focus_row, scroll) >= top_band_bottom(), "rows {rows}, focus {focus_row}: shelf {row}");
                }
            }
        }
    }
}

/// The focused shelf's heading rests where the first shelf's does, at every focus the column's end
/// does not hold lower, and the Under posters band is not in that rest.
#[test]
fn the_focused_shelfs_heading_rests_at_one_y_whatever_the_band() {
    let rows = SWEEP_ROWS;
    let at_top = heading_place(0, 0, rest_scroll(0, rows, band_open_of(false)));
    for focus_row in 0..rows {
        let scroll = rest_scroll(focus_row, rows, band_open_of(false));
        if scroll < grid_max_scroll(rows, band_open_of(false)) {
            assert_eq!(heading_place(focus_row, focus_row, scroll), at_top, "focus {focus_row}");
        } else {
            assert!(heading_place(focus_row, focus_row, scroll) >= at_top, "focus {focus_row}: the end holds it lower, never higher");
        }
    }
}

#[test]
fn every_settled_row_keeps_its_focused_label_block_above_the_overscan_bottom() {
    use plx_plex::plex::session::{UnderPosters, UnderPostersDetails as Details};
    // Every block the Under posters setting can draw: two lines, one line, nothing.
    for (title, details) in [(true, Details::Genre), (false, Details::YearLength), (false, Details::None)] {
        crate::registry::captions::TEST_UNDER_POSTERS.with(|cell| cell.set(Some(UnderPosters { title, details })));
        let lines = crate::registry::captions::policy().max_lines();
        crate::registry::captions::TEST_UNDER_POSTERS.with(|cell| cell.set(None));
        let block = if lines == 0 { 0.0 } else { ui_cards::TileLabel::height(lines == 2) };
        for rows in 1..=SWEEP_ROWS {
            for focus_row in 0..rows {
                let row_y = GRID_TOP_Y + shelf_top_settled(focus_row, focus_row, band_open_of(false)) - rest_scroll(focus_row, rows, band_open_of(false));
                assert!(row_y + CARD_DY + CARD_H + block <= SCR_H - MARGIN_Y,
                    "title {title}, {details:?}: row {focus_row} of {rows} ends its {lines}-line block at {}", row_y + CARD_DY + CARD_H + block);
            }
        }
    }
}

#[test]
fn the_grids_resting_top_is_the_highest_a_shelf_may_settle() {
    assert_eq!(rest_scroll(0, 8, band_open_of(false)), 0.0);
    assert_eq!(rest_scroll(3, 8, band_open_of(false)), shelf_top_settled(3, 3, band_open_of(false)));
    assert!(heading_top(0, 0, 0.0) - top_band_bottom() >= theme::space::XS);
}

/// **The page rests by the ENGINE's focus, not by what the picture still shows.** BACK (or Up) from a
/// deep shelf moves the engine to the billboard at once, but the picture keeps the card focused until
/// the dive is half way (`visible_activation`), so a rest read from the picture kept the deep target
/// through the fold and the next Down drew the deep shelves on its way back to the first. A billboard
/// focus rests the first shelf, a card or a heading its own row, all by the one shared rule.
#[test]
fn the_page_rests_by_the_engines_focus_while_the_picture_still_shows_the_deep_shelf() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let snapshot = collection_home(&mut state, &adapter);
    let view = snapshot.view();
    let tick = |s: &mut HomeScreen, focus: FocusKey<u32>| {
        step(s, view, Some(focus), &ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 }));
    };
    let mut s = screen(view);
    let rows = s.rows.len();
    assert!(rows >= 3, "premise: the catalog has shelves below the first two ({rows})");
    let deep = rows - 1;
    assert!(rest_scroll(deep, rows, band_open_of(false)) > 0.0, "premise: the last shelf rests the page below the first");
    let card = FocusKey { entry: s.entry, elem: s.rows[deep].elems[0] };
    let hero = FocusKey { entry: s.entry, elem: HERO_PLAY_ELEM };
    // the dive has reached the last shelf and the page rests there
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    step(&mut s, view, Some(card), &ScreenEvent::FocusMoved { from: Some(hero), to: card, by: By::Dir });
    tick(&mut s, card);
    assert_eq!(s.grid.scroll_target, rest_scroll(deep, rows, band_open_of(false)), "a card's focus rests its own row");

    // the fold: the engine is on the billboard, and the picture is still on the shelves
    step(&mut s, view, Some(hero), &ScreenEvent::FocusMoved { from: Some(card), to: hero, by: By::Dir });
    tick(&mut s, hero);
    assert!(s.snap.pos > 0.5, "premise: the picture still shows the shelves ({})", s.snap.pos);
    assert_eq!(s.grid.scroll_target, rest_scroll(0, rows, band_open_of(false)), "a billboard focus rests the first shelf at once");

    // a linked heading rests its row by the engine's focus as well
    let row = (1..rows).find(|row| s.rows[*row].link.is_some()).expect("premise: a collection shelf has a linked heading");
    let heading = heading_key(&s, row);
    step(&mut s, view, Some(heading), &ScreenEvent::FocusMoved { from: Some(hero), to: heading, by: By::Dir });
    tick(&mut s, heading);
    assert_eq!(s.grid.scroll_target, rest_scroll(row, rows, band_open_of(false)), "a heading's focus rests its row");
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
        placeholders: false,
        link: None,
        title: String::new(),
        cards: Vec::new(),
        stale: false,
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
    assert_eq!(HERO_NBTN, 3);
    assert_eq!(plx_ui::cards::MAX_ROW_ITEMS, 24);
}

/// `search` caps its shelves at the data layer's `pms::MAX_SHELF_ITEMS` (`person` used to, and no
/// longer does: its shelves hold every credit); the card row that draws them owns
/// `ui::cards::MAX_ROW_ITEMS` springs. The data layer cannot name `ui` and
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

/// **Play carries the resume a stop left.** The stop's `Offset` patch reaches the Continue Watching
/// card, and OK on it under the Play setting asks for that point, not for the one the card was
/// drawn with before the stop (G8: a Play taken from a stale row resumed at the old offset).
#[test]
fn a_cards_play_carries_the_resume_point_a_stop_patched_in() {
    use plx_data::stores::hubs::HubsCmd;
    use plx_data::viewstate::{ViewAt, ViewEdit};
    let _guard = plx_base::testlock::serial();
    let _press = deck_press_is(plx_media::route::DeckPress::Play);
    let mut hubs = plx_data::stores::hubs::HubsStore::default();
    hubs.seed_for_test(2, plx_data::pms::HubState::Ready);
    let play = |hubs: &plx_data::stores::hubs::HubsStore| {
        let snapshot = hubs.snapshot();
        let mut s = screen(snapshot.view());
        let card = first_card(&s);
        let (_, out, _) = step(&mut s, snapshot.view(), Some(card),
            &ScreenEvent::PressCommit(plx_machine::machine::PressId(1)));
        out.into_iter().find_map(|e| match e.fx {
            Fx::App(AppFx::Home(HomeReq::Play { rk, resume_ns, .. })) => Some((rk, resume_ns)),
            _ => None,
        })
    };
    assert_eq!(play(&hubs), Some(("1".to_string(), 0)), "premise: the card has no resume point");
    let edit = hubs.run(HubsCmd::ApplyView {
        at: ViewAt::leaf(plx_plex::plex::ServerId::UNSET, "1"),
        edit: ViewEdit::Offset { ms: 150_000 },
    });
    assert!(edit.changed, "the stop's offset reached the card");
    assert_eq!(play(&hubs), Some(("1".to_string(), 150_000 * 1_000_000)));
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
    let titled = plx_ui::caption_policy::CaptionPolicy { title: true, ..Default::default() };
    let item = PmsMovie { title: "Show".into(), ..Default::default() };
    let deck_label = |is_deck: bool| crate::registry::captions::shelf_label(&titled, &item, is_deck);
    {
        let _press = deck_press_is(DeckPress::Play);
        assert!(deck_label(true).glyph);
        assert!(!deck_label(false).glyph, "a shelf that navigates never carries it");
    }
    let _press = deck_press_is(DeckPress::Details);
    assert!(!deck_label(true).glyph);
    assert!(!deck_label(false).glyph);
}

#[test]
fn ok_on_a_live_tv_channel_tunes_it_and_holding_it_opens_no_item_menu() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let channel = plx_plex::plex::livetv::LiveChannel {
        dvr: "7".into(),
        channel: "C101.242.ersatztv.org".into(),
        vcn: "101".into(),
        name: "Family Channel".into(),
        thumb: String::new(),
        now: "Family Show".into(),
        ends_at: 0,
    };
    plx_data::pms::seed_live_for_test(&mut state, &adapter, 1, &[channel]);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    let card = first_card(&s);
    let (_, ok, _) = step(&mut s, snapshot.view(), Some(card), &ScreenEvent::PressCommit(plx_machine::machine::PressId(1)));
    assert!(has_home(&ok, |r| matches!(r, HomeReq::Tune { dvr, channel, title, .. }
        if dvr == "7" && channel == "C101.242.ersatztv.org" && title == "Family Channel")));
    assert!(!has_home(&ok, |r| matches!(r, HomeReq::Play { .. } | HomeReq::Detail { .. })));
    let (_, held, _) = step(&mut s, snapshot.view(), Some(card), &ScreenEvent::PressHold(plx_machine::machine::PressId(2)));
    assert!(!has_home(&held, |r| matches!(r, HomeReq::ItemMenu { .. })), "a channel is no library item");
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

/// The item-menu opener asks the page for its focused card (`Screen::focused_card`) instead of
/// downcasting to Home: a grid card names the item the seeded hubs put there (a Continue Watching
/// press and its menu are account-affecting) and the rect the page places it at; the hero, which is
/// not a card, names none. Pinned at rest and during the hero-to-grid dive (`snap` <= 0.5).
#[test]
fn the_focused_card_is_the_grid_item_and_the_rect_the_opener_measured_before() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 2, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    // The seeded hubs are one shelf of two items, rating keys "1" and "2" on the unset server.
    assert_eq!(s.rows[0].elems.len(), 2);
    for snap in [1.0, 0.5, 0.2] {
        s.snap.jump(snap);
        s.layout_grid();
        for (col, rk) in ["1", "2"].into_iter().enumerate() {
            let key = FocusKey { entry: s.entry, elem: s.rows[0].elems[col] };
            let context = cx(snapshot.view(), Some(key));
            let card = Screen::<TestHost>::focused_card(&s, &context, Some(key), Some(At::Drawn))
                .unwrap_or_else(|| panic!("col {col} at snap {snap} is a focused card"));
            let item = card.item.downcast_ref::<PmsMovie>().unwrap();
            assert_eq!((item.sid, item.rk.as_str()), (plx_plex::plex::ServerId::UNSET, rk), "col {col} at snap {snap}");
            let placed = Focusable::<TestHost>::place(&s, &key.elem, &context, At::Drawn).unwrap();
            assert_eq!(card.rect, Some(placed.rect), "col {col} at snap {snap}");
            assert!(card.rect.is_some() && card.rest_rect.is_some());
            assert_eq!(Screen::<TestHost>::focused_card_rect(&s, &context, Some(key), At::Drawn), Some(placed.rect));
            assert!(Screen::<TestHost>::focused_card(&s, &context, Some(key), None).unwrap().rect.is_none());
        }
    }
    let first = first_card(&s);
    let foreign = FocusKey { entry: EntryId(s.entry.0 + 100), elem: first.elem };
    assert!(Screen::<TestHost>::focused_card(&s, &cx(snapshot.view(), Some(foreign)), Some(foreign), Some(At::Drawn)).is_none());
    let hero = FocusKey { entry: s.entry, elem: 0 };
    let context = cx(snapshot.view(), Some(hero));
    assert!(Screen::<TestHost>::focused_card_rect(&s, &context, Some(hero), At::Drawn).is_none());
    assert!(Screen::<TestHost>::focused_card(&s, &context, Some(hero), Some(At::Drawn)).is_none());
}

/// The opener only anchors a menu, so it must not need the item to resolve: a hubs snapshot that
/// shrank between the hold and the request drain leaves the card where it was drawn, and
/// `focused_card_rect` still answers its rect (the item-resolving `focused_card` does not).
#[test]
fn the_opener_rect_survives_a_hubs_snapshot_that_shrank_after_the_hold() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 2, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.layout_grid();
    let key = FocusKey { entry: s.entry, elem: s.rows[0].elems[1] };
    let held = Focusable::<TestHost>::place(&s, &key.elem, &cx(snapshot.view(), Some(key)), At::Drawn).unwrap().rect;
    plx_data::pms::seed_for_test(&mut state, &adapter, 1, plx_data::pms::HubState::Ready);
    let shrunk = plx_data::pms::hubs_snapshot(&state);
    let context = cx(shrunk.view(), Some(key));
    assert!(s.focused_item(Some(key), &context).is_none(), "the second card is gone from the snapshot");
    assert!(Screen::<TestHost>::focused_card(&s, &context, Some(key), Some(At::Drawn)).is_none());
    assert_eq!(Screen::<TestHost>::focused_card_rect(&s, &context, Some(key), At::Drawn), Some(held));
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

/// What the two syncs of ONE press cost when the ring moves a row on: the row entering gets its cards and the row
/// leaving gives them up (`rows` rows, `items` cards each). Taken on the second press, so the
/// placeholder keys of the row that entered on the first are the ones due to leave the table.
fn ring_step_cost(rows: usize, items: usize) -> SyncCost {
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    plx_data::pms::seed_ring_for_test(&mut state, &adapter, rows, items, &|_| 0, (0, 5));
    s.sync_catalog(&cx(plx_data::pms::hubs_snapshot(&state).view(), None));
    let mut last = SyncCost::default();
    for step in 1..3usize {
        let before = s.cost;
        // The press moves the band: the row that left gives its cards up (one publication) and the
        // read of the row that entered lands (another), each a StoreChanged for the page.
        assert!(plx_data::pms::hold_for_test(&mut state, step, step + 5));
        s.sync_catalog(&cx(plx_data::pms::hubs_snapshot(&state).view(), None));
        assert!(plx_data::pms::land_ring_rows_for_test(&mut state, items, &|_| 0), "step {step} of {rows} rows");
        s.sync_catalog(&cx(plx_data::pms::hubs_snapshot(&state).view(), None));
        last = SyncCost {
            index_inserts: s.cost.index_inserts - before.index_inserts,
            index_rebuilds: s.cost.index_rebuilds - before.index_rebuilds,
            elem_at_writes: s.cost.elem_at_writes - before.elem_at_writes,
            sweeps: s.cost.sweeps - before.sweeps,
        };
        assert_indexes_agree(&s);
    }
    last
}

#[test]
fn a_ring_step_costs_the_rows_it_touches_not_the_catalog() {
    let _guard = plx_base::testlock::serial();
    let items = 12;
    let (big, small) = (ring_step_cost(170, items), ring_step_cost(10, items));
    assert_eq!(big.index_rebuilds, 0, "a prune deletes in place: {big:?}");
    assert_eq!(small.index_rebuilds, 0, "{small:?}");
    // one row in, one row out: each re-keys its own cards and nothing else
    assert!(big.index_inserts <= 2 * items + 4, "index insertions follow the rows that changed: {big:?}");
    assert!(big.elem_at_writes <= 2 * items + 4, "so does the element map: {big:?}");
    assert_eq!(big.index_inserts, small.index_inserts, "and not the catalog's size: {big:?} vs {small:?}");
    assert_eq!(big.elem_at_writes, small.elem_at_writes, "{big:?} vs {small:?}");
}

/// Forty presses of a Down key through a long page: two StoreChanged each, the key table swept once
/// per batch of keys that left the page and not once per publication.
#[test]
fn the_key_table_is_swept_once_per_batch_of_departed_keys() {
    let _guard = plx_base::testlock::serial();
    let (rows, items, presses) = (170usize, 12usize, 40usize);
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    plx_data::pms::seed_ring_for_test(&mut state, &adapter, rows, items, &|_| 0, (0, 5));
    s.sync_catalog(&cx(plx_data::pms::hubs_snapshot(&state).view(), None));
    let before = s.cost;
    for step in 1..=presses {
        assert!(plx_data::pms::hold_for_test(&mut state, step, step + 5));
        s.sync_catalog(&cx(plx_data::pms::hubs_snapshot(&state).view(), None));
        assert!(plx_data::pms::land_ring_rows_for_test(&mut state, items, &|_| 0));
        s.sync_catalog(&cx(plx_data::pms::hubs_snapshot(&state).view(), None));
    }
    let sweeps = s.cost.sweeps - before.sweeps;
    // a press takes at most two rows' cards off the page: 2 * items keys
    let leaving = presses * 2 * items;
    assert!(sweeps <= leaving.div_ceil(KEY_PRUNE_BATCH) + 1, "{sweeps} sweeps in {} syncs", 2 * presses);
    assert!(sweeps < presses, "a sweep per press is the cost this batches away");
    // the placeholder keys that have left the page wait for the sweep, one batch at most (the
    // stable keys of cards that left stay by their own bound, `key_cap`)
    let waiting = s.items.iter()
        .filter(|key| matches!(key.identity, HomeItemIdentity::Slot { .. }) && !s.elem_at.contains_key(&key.elem))
        .count();
    assert!(waiting <= KEY_PRUNE_BATCH + 2 * items, "{waiting} placeholder keys wait for a sweep");
    assert_indexes_agree(&s);
}

#[test]
fn scrolling_inside_the_held_band_does_not_send_the_store_a_new_band() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_grid_for_test(&mut state, &adapter, 60, 12);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.pos = 1.0;
    s.snap_target = 1.0;
    s.layout_grid();
    let tick = ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 });
    let mut sent = holds(&step(&mut s, snapshot.view(), None, &tick).1);
    // Down, one row at a time: the shelves scroll by a row pitch per press.
    let presses: usize = 12;
    for press in 1..=presses {
        let scroll = press as f32 * ui_cards::ROW_PITCH_FIXED;
        s.grid.scroll_y.pos = scroll;
        s.grid.scroll_target = scroll;
        s.layout_grid();
        sent.extend(holds(&step(&mut s, snapshot.view(), None, &tick).1));
        let visible: Vec<usize> = (0..s.rows.len()).filter(|&row| {
            let shelf = &s.grid.shelves[row];
            let top = heading_y(shelf.base_y(), shelf.heading_lift());
            on_axis(top, shelf.base_y() + CARD_H + shelf.under_band() - top, SCR_H, 0.0)
        }).collect();
        let seen = (*visible.first().unwrap(), *visible.last().unwrap());
        let need = ui_cards::hold_band(seen, s.rows.len());
        let band = *sent.last().expect("the first tick sent a band");
        assert!(band.0 <= need.0 && band.1 >= need.1,
            "after press {press} the rows in view and {} either side are held: sent {band:?}, need {need:?}", ui_cards::HOLD_MARGIN);
    }
    assert!(sent.len() <= 1 + presses.div_ceil(ui_cards::HOLD_SLACK + 1),
        "{presses} one-row presses inside the band cost at most one new band per {} rows: {sent:?}",
        ui_cards::HOLD_SLACK + 1);
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
    for _ in 0..200 {
        republish_named(&mut s, &mut state, &adapter, &["", "", ""], 4, None);
    }
    // The shown publication and the one before it, and no more than one batch of keys that left
    // the page and wait for the sweep (`KEY_PRUNE_BATCH`): the table does not grow with the
    // number of publications, and the groups (few) go at once.
    assert!(s.items.len() <= 2 * 12 + KEY_PRUNE_BATCH, "{} keys after 200 publications", s.items.len());
    assert!(s.groups.len() <= 3, "{} groups after 200 publications", s.groups.len());
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
    // enough publications for a batch of keys to have left the page, and so for a sweep to run
    for _ in 0..KEY_PRUNE_BATCH {
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
    plx_ui::hold_hint::reset_shown_for_test();
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
fn the_hint_leaves_on_its_own_and_a_read_one_stays_gone_on_the_next_tile() {
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
        assert!(!s.hold_hint.visible(), "read once this run: a new tile does not bring it back");
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

#[test]
fn a_recent_page_keeps_the_focused_card_and_its_drawn_position() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, 0, 24, true);
    let old = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(old.view());
    s.snap.pos = 1.0;
    s.snap_target = 1.0;
    let key = FocusKey { entry: s.entry, elem: s.rows[0].elems[18] };
    pop_card(&mut s, old.view(), key, 90);
    let before = drawn_rect(&s, old.view(), Some(key), 0, 18);
    plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, 12, 24, true);
    let next = plx_data::pms::hubs_snapshot(&state);
    step(&mut s, next.view(), Some(key), &ScreenEvent::StoreChanged(StoreId::Hubs.ord(), next.view().generation));
    step(&mut s, next.view(), Some(key), &ScreenEvent::Tick(Tick { ms: 1600, dt_us: 16_667 }));
    assert_eq!(s.rows[0].elems[6], key.elem);
    assert_eq!(s.focused_item(Some(key), &cx(next.view(), Some(key))).unwrap().rk, "19");
    let after = drawn_rect(&s, next.view(), Some(key), 0, 6);
    assert!((after.x - before.x).abs() < 0.1);
    assert!((after.y - before.y).abs() < 0.1);
    assert!((after.w - before.w).abs() < 0.1);
}

#[test]
fn recent_rows_request_pages_only_when_the_shelves_are_visible() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, 0, 12, true);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    let is_page = |out: &[Stamped<TestHost>]| out.iter().any(|fx|
        matches!(fx.fx, Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Page { .. })))));
    let tick = ScreenEvent::Tick(Tick { ms: 16, dt_us: 0 });
    let focus = Some(FocusKey { entry: s.entry, elem: s.rows[0].elems[6] });
    assert!(!is_page(&step(&mut s, snapshot.view(), focus, &tick).1));
    s.snap.pos = 1.0;
    s.snap_target = 1.0;
    s.grid.scroll_y.pos = 10000.0;
    s.layout_grid();
    assert!(!is_page(&step(&mut s, snapshot.view(), focus, &tick).1));
    s.grid.scroll_y.pos = 0.0;
    s.layout_grid();
    assert!(is_page(&step(&mut s, snapshot.view(), focus, &tick).1));
}

/// The app's frame, which the other paging cases never produce: views are captured, THEN the
/// frame's landing is delivered to the store, THEN the screen ticks on the captured view. With a
/// key held, the row's ask is re-armed every step, so on the frame a page lands the tick asks from
/// the 12-card window (focus six from its end) while the store already holds 24 with focus in the
/// middle. The store moved the 24: the cards slid twelve places under a focus drawn at the right
/// of the screen, which a row scrolled to its start cannot keep there.
#[test]
fn a_tick_on_the_view_from_before_the_frames_landing_does_not_move_the_landed_window() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, 0, 12, true);
    let captured = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(captured.view());
    s.snap.pos = 1.0;
    s.snap_target = 1.0;
    s.layout_grid();
    // the landing: delivered after the capture, before the tick
    plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, 0, 24, true);
    let focus = Some(FocusKey { entry: s.entry, elem: s.rows[0].elems[10] });
    let tick = ScreenEvent::Tick(Tick { ms: 16, dt_us: 0 });
    let asks: Vec<_> = step(&mut s, captured.view(), focus, &tick).1.into_iter().filter_map(|fx| match fx.fx {
        Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Page { before, seen, .. }))) => Some((before, seen)),
        _ => None,
    }).collect();
    // The ask names the window it was computed from, which is what lets the store refuse it
    // (`an_ask_computed_from_a_window_the_store_has_since_moved_is_refused`, in `plx_data`).
    let landed = plx_data::pms::hubs_snapshot(&state);
    assert_eq!(asks, vec![(false, captured.view().generation)], "the captured window ends six cards past the focus");
    assert_ne!(captured.view().generation, landed.view().generation);
    // the next frame captures the landed window: focus is ten cards into 24, and nothing is due
    let _ = step(&mut s, landed.view(), focus, &ScreenEvent::StoreChanged(StoreId::Hubs.ord(), landed.view().generation));
    assert_eq!(s.rows[0].elems.len(), 24);
    let again = step(&mut s, landed.view(), focus, &tick).1.into_iter().filter(|fx|
        matches!(fx.fx, Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Page { .. }))))).count();
    assert_eq!(again, 0);
}

/// The path behind "the focus skipped twelve cards": the engine reseats a focus whose element is no
/// longer on the page through `reconcile`, which keeps the COLUMN the element last had in its row.
/// A landing that left focus outside the new window would therefore move it by the slide width; a
/// landing that holds the focused card follows the element. The first half is the happy path; that
/// no landing can do the second is `no_landing_ever_leaves_the_focused_card_outside_the_window`.
#[test]
fn a_landing_follows_the_focused_element() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, 0, 24, true);
    let first = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(first.view());
    let focus = FocusKey { entry: s.entry, elem: s.rows[0].elems[18] };
    let key = |s: &HomeScreen, focus: FocusKey<u32>| s.rows[0].elems.iter().position(|&e| e == focus.elem);
    // one slide: the card at column 18 of 0..24 is column 6 of 12..36, and focus follows it
    plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, 12, 24, true);
    let landed = plx_data::pms::hubs_snapshot(&state);
    let context = cx(landed.view(), Some(focus));
    s.sync_catalog(&context);
    let seated = Focusable::<TestHost>::reconcile(&s, focus, &context);
    assert_eq!((seated, key(&s, seated)), (focus, Some(6)), "the element followed its card");
}

#[test]
fn the_continue_watching_row_asks_for_a_page_at_its_trailing_edge() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_continue_window_for_test(&mut state, &adapter, 12, true);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.pos = 1.0;
    s.snap_target = 1.0;
    s.layout_grid();
    assert_eq!(s.rows[0].identity, HomeHubIdentity::ContinueWatching);
    let focus = Some(FocusKey { entry: s.entry, elem: s.rows[0].elems[10] });
    let tick = ScreenEvent::Tick(Tick { ms: 16, dt_us: 0 });
    let asks: Vec<_> = step(&mut s, snapshot.view(), focus, &tick).1.into_iter().filter_map(|fx| match fx.fx {
        Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Page { id, before, .. }))) => Some((id, before)),
        _ => None,
    }).collect();
    assert_eq!(asks, vec![("home.continue".to_string(), false)]);
}

#[test]
fn returning_to_a_previous_recent_page_does_not_load_the_next_page_again() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, 12, 24, true);
    let old = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(old.view());
    s.snap.pos = 1.0;
    s.snap_target = 1.0;
    let key = FocusKey { entry: s.entry, elem: s.rows[0].elems[5] };
    pop_card(&mut s, old.view(), key, 90);
    plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, 0, 24, true);
    let next = plx_data::pms::hubs_snapshot(&state);
    step(&mut s, next.view(), Some(key), &ScreenEvent::StoreChanged(StoreId::Hubs.ord(), next.view().generation));
    assert_eq!(s.rows[0].elems[17], key.elem);
    for frame in 0..90 {
        let (_, out, _) = step(&mut s, next.view(), Some(key),
            &ScreenEvent::Tick(Tick { ms: 1600 + frame * 17, dt_us: 16_667 }));
        assert!(!out.iter().any(|fx| matches!(fx.fx,
            Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Page { .. }))))));
    }
}

#[test]
fn a_pending_backward_page_is_repeated_on_a_ladder_not_every_tick() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, 12, 24, true);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.pos = 1.0;
    s.snap_target = 1.0;
    let key = FocusKey { entry: s.entry, elem: s.rows[0].elems[5] };
    let mut frames = Vec::new();
    for frame in 0..90 {
        let (_, out, _) = step(&mut s, snapshot.view(), Some(key),
            &ScreenEvent::Tick(Tick { ms: frame * 17, dt_us: 16_667 }));
        if out.iter().any(|fx| matches!(fx.fx,
            Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Page { before: true, .. }))))) { frames.push(frame); }
    }
    // unanswered for 1.5 s: asked at once, then repeated at 0.25 s and 0.75 s, never every tick
    assert_eq!(frames.first(), Some(&0));
    assert!(frames.len() <= 3, "{frames:?}");
    assert!(frames.windows(2).all(|w| w[1] - w[0] >= 14), "{frames:?}");
}

#[test]
fn leaving_a_backward_page_edge_allows_a_new_request_on_return() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, 12, 24, true);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.pos = 1.0;
    s.snap_target = 1.0;
    let key = FocusKey { entry: s.entry, elem: s.rows[0].elems[5] };
    let away = FocusKey { entry: s.entry, elem: s.rows[0].elems[12] };
    let mut requests = 0;
    for (frame, current) in [key, key, away, key, key].into_iter().enumerate() {
        let (_, out, _) = step(&mut s, snapshot.view(), Some(current),
            &ScreenEvent::Tick(Tick { ms: frame as u32 * 17, dt_us: 16_667 }));
        requests += out.iter().filter(|fx| matches!(fx.fx,
            Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Page { before: true, .. }))))).count();
    }
    assert_eq!(requests, 2);
}

#[test]
fn covering_home_cancels_its_page_even_after_focus_has_left_the_entry() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, 12, 24, true);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.pos = 1.0;
    s.snap_target = 1.0;
    let key = FocusKey { entry: s.entry, elem: s.rows[0].elems[5] };
    step(&mut s, snapshot.view(), Some(key), &ScreenEvent::Tick(Tick { ms: 0, dt_us: 16_667 }));
    let (_, out, _) = step(&mut s, snapshot.view(), None, &ScreenEvent::Cover);
    assert_eq!(out.iter().filter(|fx| matches!(fx.fx,
        Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::CancelPage { .. }))))).count(),1);
    step(&mut s, snapshot.view(), Some(key), &ScreenEvent::Uncover);
    let (_, out, _) = step(&mut s, snapshot.view(), Some(key), &ScreenEvent::Tick(Tick { ms: 1000, dt_us: 16_667 }));
    assert_eq!(out.iter().filter(|fx| matches!(fx.fx,
        Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Page { before: true, .. }))))).count(),1);
}

fn page_requests(out: &[Stamped<TestHost>]) -> usize {
    out.iter().filter(|fx| matches!(fx.fx,
        Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Page { .. }))))).count()
}

// A page the store gave up on leaves no store change behind, so the row's once-per-edge latch is
// still set when the owner presses again. A focus move or an at-edge press on that row re-asks.
#[test]
fn a_row_whose_page_was_given_up_asks_again_on_the_next_focus_move_or_edge_press() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, 12, 24, true);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.pos = 1.0;
    s.snap_target = 1.0;
    let key = FocusKey { entry: s.entry, elem: s.rows[0].elems[5] };
    let next = FocusKey { entry: s.entry, elem: s.rows[0].elems[4] };
    let tick = |ms| ScreenEvent::Tick(Tick { ms, dt_us: 16_667 });
    let mut requests = page_requests(&step(&mut s, snapshot.view(), Some(key), &tick(0)).1);
    requests += page_requests(&step(&mut s, snapshot.view(), Some(key), &tick(17)).1);
    assert_eq!(requests, 1, "the latch holds while nothing changed");
    step(&mut s, snapshot.view(), Some(next), &ScreenEvent::FocusMoved { from: Some(key), to: next, by: By::Dir });
    requests += page_requests(&step(&mut s, snapshot.view(), Some(next), &tick(34)).1);
    requests += page_requests(&step(&mut s, snapshot.view(), Some(next), &tick(51)).1);
    assert_eq!(requests, 2, "a focus move on the same row asks once more");
    step(&mut s, snapshot.view(), Some(next), &ScreenEvent::Input(InputEvent {
        at: Tick::default(),
        source: Source::Sdl,
        kind: InputKind::Key { key: Key::Left, sym: 0, wcode: 0, edge: Edge::Down, at_edge: true },
    }));
    requests += page_requests(&step(&mut s, snapshot.view(), Some(next), &tick(68)).1);
    requests += page_requests(&step(&mut s, snapshot.view(), Some(next), &tick(85)).1);
    assert_eq!(requests, 3, "an edge press asks once more");
}

fn holds(out: &[Stamped<TestHost>]) -> Vec<(usize, usize)> {
    out.iter().filter_map(|fx| match &fx.fx {
        Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Hold { lo, hi }))) => Some((*lo, *hi)),
        _ => None,
    }).collect()
}

#[test]
fn the_screen_holds_the_rows_in_view_and_two_either_side_and_says_so_only_when_that_moves() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_grid_for_test(&mut state, &adapter, 60, 12);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.pos = 1.0;
    s.snap_target = 1.0;
    s.layout_grid();
    let tick = ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 });
    let first = holds(&step(&mut s, snapshot.view(), None, &tick).1);
    assert_eq!(first.len(), 1, "the first tick tells the store which rows are held");
    assert_eq!(first[0].0, 0);
    assert!(first[0].1 <= 6 + ui_cards::HOLD_MARGIN + ui_cards::HOLD_SLACK, "a screenful plus the margin and the slack: {first:?}");
    assert!(holds(&step(&mut s, snapshot.view(), None, &tick).1).is_empty(), "an unmoved range is not sent again");
    s.grid.scroll_y.pos = 20_000.0;
    s.grid.scroll_target = 20_000.0;
    s.layout_grid();
    let moved = holds(&step(&mut s, snapshot.view(), None, &tick).1);
    assert_eq!(moved.len(), 1, "a moved range is sent once");
    assert!(moved[0].0 > first[0].1, "and it is the rows now in view: {moved:?}");
}

#[test]
fn a_focus_on_a_placeholder_keeps_its_row_column_and_cell_when_the_cards_land() {
    let _guard = plx_base::testlock::serial();
    let (rows, items, row, col) = (40, 12, 20, 5);
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    // Held Down faster than the reads land: the key reaches a row the ring has released.
    plx_data::pms::seed_ring_for_test(&mut state, &adapter, rows, items, &|_| 0, (0, 8));
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    s.sync_catalog(&cx(snapshot.view(), None));
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    s.layout_grid();
    let slot = FocusKey { entry: EntryId(7), elem: s.rows[row].elems[col] };
    assert!(s.items.iter().any(|k| k.elem == slot.elem && matches!(k.identity, HomeItemIdentity::Slot { .. })),
        "the released row is a row of placeholder slots");
    assert!(!CardSource::<TestHost>::loaded(&s.cards(snapshot.view(), row), col),
        "a placeholder is not a loaded card");
    let settle = |s: &mut HomeScreen, view: HubsView<'_>, key: FocusKey<u32>| {
        let context = cx(view, Some(key));
        for _ in 0..60 { s.update_grid_for_test(view, &context, 1.0 / 60.0); }
    };
    settle(&mut s, snapshot.view(), slot);
    let before = drawn_rect(&s, snapshot.view(), Some(slot), row, col);
    // OK on a placeholder opens nothing
    let (_, out, _) = step(&mut s, snapshot.view(), Some(slot), &ScreenEvent::Activate(slot.elem));
    assert!(!has_home(&out, |r| matches!(r, HomeReq::Detail { .. } | HomeReq::Play { .. })), "OK on a placeholder is inert");
    // the row's cards land (the ring moved onto it); the engine reconciles the slot key in the same frame
    plx_data::pms::seed_ring_for_test(&mut state, &adapter, rows, items, &|_| 0, (row - 4, row + 4));
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let context = cx(snapshot.view(), Some(slot));
    s.sync_catalog(&context);
    let landed = Focusable::<TestHost>::reconcile(&s, slot, &context);
    assert_ne!(landed.elem, slot.elem, "the slot key becomes the item's");
    assert_eq!(s.focused_grid(Some(landed)), Some((row, col)), "the same row and column");
    assert!(CardSource::<TestHost>::loaded(&s.cards(snapshot.view(), row), col));
    let after = drawn_rect(&s, snapshot.view(), Some(landed), row, col);
    assert!((before.cx() - after.cx()).abs() < 0.5 && (before.cy() - after.cy()).abs() < 0.5,
        "the cell under focus did not move: {before:?} -> {after:?}");
    settle(&mut s, snapshot.view(), landed);
    let later = drawn_rect(&s, snapshot.view(), Some(landed), row, col);
    assert!((before.cx() - later.cx()).abs() < 0.5 && (before.cy() - later.cy()).abs() < 0.5,
        "and stays there: {before:?} -> {later:?}");
}

/// Keys a projection mints when the ring moves one screenful, over a Home of `rows` rows.
fn keys_minted_by_one_ring_move(rows: usize) -> u32 {
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    plx_data::pms::seed_ring_for_test(&mut state, &adapter, rows, 12, &|_| 0, (0, 8));
    s.sync_catalog(&cx(plx_data::pms::hubs_snapshot(&state).view(), None));
    let before = s.next_elem;
    plx_data::pms::seed_ring_for_test(&mut state, &adapter, rows, 12, &|_| 0, (10, 18));
    s.sync_catalog(&cx(plx_data::pms::hubs_snapshot(&state).view(), None));
    s.next_elem - before
}

#[test]
fn a_ring_move_registers_keys_for_the_rows_it_changes_not_for_every_row() {
    let _guard = plx_base::testlock::serial();
    let (few, many) = (keys_minted_by_one_ring_move(40), keys_minted_by_one_ring_move(400));
    assert!(few <= 2 * 9 * 12, "9 rows leave and 9 enter, 12 cards each: {few}");
    assert_eq!(few, many, "ten times the rows, the same keys");
}

/// `cx` with the engine's remembered cursors, which the key pruning reads.
fn cx_remembering<'a>(view: HubsView<'a>, remembered: &[(GroupId, u32)]) -> Cx<'a, TestHost> {
    let mut context = cx(view, None);
    context.focus.remembered = remembered.into();
    context
}

#[test]
fn a_released_rows_remembered_card_is_seated_again_when_the_row_returns() {
    let _guard = plx_base::testlock::serial();
    let (rows, items) = (40, 12);
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    let mut s = HomeScreen::new(EntryId(7), InstanceId(9));
    let seed = |state: &mut plx_data::pms::PmsState, epoch: usize, hold: (usize, usize)| {
        let salt = move |row: usize| if row == 3 { 0 } else { epoch };
        plx_data::pms::seed_ring_for_test(state, &adapter, rows, items, &salt, hold);
    };
    seed(&mut state, 0, (0, 8));
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    s.sync_catalog(&cx(snapshot.view(), None));
    let (group, card) = (s.rows[3].group, s.rows[3].elems[7]);
    let remembered = [(group, card)];
    assert_eq!(s.elem_at.get(&card), Some(&(3, 7)));
    // the user goes Down forty rows, and the content of the rows they pass keeps changing
    for epoch in 1..=40 {
        seed(&mut state, epoch, (30, 38));
        let snapshot = plx_data::pms::hubs_snapshot(&state);
        s.sync_catalog(&cx_remembering(snapshot.view(), &remembered));
    }
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let context = cx_remembering(snapshot.view(), &remembered);
    let want = FocusKey { entry: EntryId(7), elem: card };
    assert_eq!(Focusable::<TestHost>::reconcile(&s, want, &context).elem, s.rows[3].elems[7],
        "while the row is a descriptor the remembered card still names its row and column");
    // and comes back with the cards it showed
    seed(&mut state, 41, (0, 8));
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let context = cx_remembering(snapshot.view(), &remembered);
    s.sync_catalog(&context);
    assert_eq!(s.elem_at.get(&card), Some(&(3, 7)), "the card the user left is on the card they left it at");
    assert_eq!(Focusable::<TestHost>::reconcile(&s, want, &context), want);
}

/// Right held for twenty seconds over a paged Recently Added row whose pages land after a latency,
/// frame by frame. A page landing slides the 24-card window by twelve; a key repeat can fall
/// between the same two ticks as the landing, before or after it. In every frame the row must be
/// the row of the frame before: each card that was on screen and is still in the window stands
/// within one frame's scroll of where it stood, the focused card is the one the keys walked to
/// and is on screen (so its caption is drawn), and a card that stayed keeps its artwork.
#[test]
fn a_held_right_over_a_sliding_window_never_moves_the_row_under_the_focus() {
    let _guard = plx_base::testlock::serial();
    const PITCH: f32 = CARD_W + GAP;
    let mut landings = 0;
    for (key_ms, page_ms, key_first) in [(100u32, 500u32, true), (100, 500, false), (100, 300, true), (100, 800, true), (130, 500, true), (90, 430, false)] {
        let mut state = plx_data::pms::PmsState::default();
        let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
        let mut offset = 0usize;
        plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, offset, 24, true);
        let mut snap = plx_data::pms::hubs_snapshot(&state);
        let mut s = screen(snap.view());
        s.snap.pos = 1.0;
        s.snap_target = 1.0;
        let mut key = FocusKey { entry: s.entry, elem: s.rows[0].elems[0] };
        pop_card(&mut s, snap.view(), key, 30);
        let mut landing: Option<u32> = None;
        let mut walked = 1usize;
        // rating key -> (drawn x, artwork) of the cards on screen in the frame before
        let mut shown: HashMap<String, (f32, String)> = HashMap::new();
        for frame in 1..1200u32 {
            let ms = frame * 1000 / 60;
            let key_due = ms / key_ms != (frame - 1) * 1000 / 60 / key_ms;
            let at = format!("keys {key_ms} ms, page {page_ms} ms, key first {key_first}, frame {frame}");
            macro_rules! press { () => {
                if let Step::Move(to) = Focusable::<TestHost>::neighbour(&s, key, Dir::Right, &cx(snap.view(), Some(key))) {
                    step(&mut s, snap.view(), Some(to), &ScreenEvent::FocusMoved { from: Some(key), to, by: By::Dir });
                    key = to;
                    walked += 1;
                }
            } }
            if key_due && key_first { press!(); }
            if landing.is_some_and(|due| ms >= due) {
                landing = None;
                landings += 1;
                offset += 12;
                plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, offset, 24, true);
                snap = plx_data::pms::hubs_snapshot(&state);
                step(&mut s, snap.view(), Some(key), &ScreenEvent::StoreChanged(StoreId::Hubs.ord(), snap.view().generation));
            }
            if key_due && !key_first { press!(); }
            let (_, out, _) = step(&mut s, snap.view(), Some(key), &ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
            if page_requests(&out) > 0 && landing.is_none() { landing = Some(ms + page_ms); }

            let view = snap.view();
            let col = s.item_col(0, key.elem).expect("the focused card is in its row");
            assert_eq!(s.item_at(view, 0, col).unwrap().rk, walked.to_string(), "{at}: focus is on the card the keys walked to");
            let focused = drawn_rect(&s, view, Some(key), 0, col);
            assert!(focused.x + focused.w > 0.0 && focused.x < SCR_W + PITCH,
                "{at}: the focused card and its caption left the screen: x {}", focused.x);
            let mut now = HashMap::new();
            for i in 0..s.rows[0].elems.len() {
                let rect = drawn_rect(&s, view, Some(key), 0, i);
                if rect.x + rect.w <= 0.0 || rect.x >= SCR_W { continue; }
                let item = s.item_at(view, 0, i).unwrap();
                if let Some((x, art)) = shown.get(&item.rk) {
                    assert!((rect.x - x).abs() < PITCH / 2.0,
                        "{at}: card {} jumped {} px in one frame (window offset {offset}, cell {i})", item.rk, rect.x - x);
                    assert_eq!(&item.thumb, art, "{at}: card {} changed its artwork", item.rk);
                }
                now.insert(item.rk.clone(), (rect.x, item.thumb.clone()));
            }
            let stayed = now.keys().filter(|rk| shown.contains_key(*rk)).count();
            assert!(stayed + 2 >= shown.len(), "{at}: {} of the {} cards on screen were replaced in one frame", shown.len() - stayed, shown.len());
            shown = now;
        }
        assert!(walked > 100, "the hold walked on through the pages: {walked}");
    }
    assert!(landings > 60, "the windows slid under the hold: {landings}");
}

use plx_ui::fixture::Walk;

/// The invariant behind "focus never moves under the user", driven over the real screen and a stand-in
/// store that applies the commands it is sent the way the hub store does: a `Page` ask is refused when
/// it names a window the store has since moved (`seen`) or one is already out, it lands after a
/// random delay by sliding the 24-card window twelve places, and a `CancelPage` drops the one out.
/// Random walks of presses (runs, bursts, reversals, 100 ms repeats) meet random landing delays
/// (one frame to five seconds); on EVERY frame the focused card changes only by a press and by
/// exactly one place, is inside the window the store holds, and no more than one page is out. A
/// held key then reaches the last card and walks back to the first.
#[test]
fn no_landing_ever_leaves_the_focused_card_outside_the_window() {
    let _guard = plx_base::testlock::serial();
    const TOTAL: usize = 24 + 12 * 8;
    for seed in 1..=8u64 {
        let mut rng = Walk(seed);
        let mut state = plx_data::pms::PmsState::default();
        let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
        let mut offset = 0usize;
        let seed_window = |state: &mut plx_data::pms::PmsState, offset: usize|
            plx_data::pms::seed_recent_window_for_test(state, &adapter, offset, 24, offset + 24 < TOTAL);
        seed_window(&mut state, offset);
        let mut latest = plx_data::pms::hubs_snapshot(&state);
        let mut s = screen(latest.view());
        s.snap.pos = 1.0;
        s.snap_target = 1.0;
        let mut key = FocusKey { entry: s.entry, elem: s.rows[0].elems[0] };
        pop_card(&mut s, latest.view(), key, 30);
        // (frame the page lands on, whether it slides toward the start)
        let mut pending: Option<(u32, bool)> = None;
        let rk_now = |s: &HomeScreen, view: HubsView<'_>, key: FocusKey<u32>| -> Option<usize> {
            let col = s.item_col(0, key.elem)?;
            s.item_at(view, 0, col)?.rk.parse().ok()
        };
        let (mut dir_right, mut run_left, mut cadence) = (true, 40u64, 6u32);
        let mut expected = 1usize;
        let mut reached = (false, false);
        let frames = 24_000u32;
        for frame in 1..frames + 6000 {
            let at = format!("seed {seed}, frame {frame}, offset {offset}");
            let ms = frame * 1000 / 60;
            let tail = frame >= frames;
            // the walk: runs in one direction at a cadence, then a reversal or a new cadence; the
            // tail holds Right to the last card, then Left to the first
            if !tail && run_left == 0 {
                dir_right = rng.below(3) != 0 && !(expected > TOTAL - 4);
                let longest = if rng.below(4) == 0 { 40 } else { 14 };
                run_left = 1 + rng.below(longest);
                cadence = [1, 2, 6, 6, 6, 12, 30][rng.below(7) as usize];
            }
            let (right, due) = if tail {
                (!reached.0, frame % 6 == 0)
            } else { (dir_right, frame % cadence == 0) };
            // the app's frame: the captured view predates this frame's landing
            let captured = latest.clone();
            if pending.is_some_and(|(due, _)| frame >= due) {
                let (_, before) = pending.take().unwrap();
                let next = if before { offset.saturating_sub(12) } else { (offset + 12).min(TOTAL - 24) };
                if next != offset {
                    offset = next;
                    seed_window(&mut state, offset);
                }
                latest = plx_data::pms::hubs_snapshot(&state);
            }
            if due {
                let dir = if right { Dir::Right } else { Dir::Left };
                if let Step::Move(to) = Focusable::<TestHost>::neighbour(&s, key, dir, &cx(captured.view(), Some(key))) {
                    step(&mut s, captured.view(), Some(to), &ScreenEvent::FocusMoved { from: Some(key), to, by: By::Dir });
                    key = to;
                    expected = if right { expected + 1 } else { expected - 1 };
                }
                run_left = run_left.saturating_sub(1);
            }
            let (_, out, _) = step(&mut s, captured.view(), Some(key), &ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
            for fx in &out {
                match &fx.fx {
                    Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Page { before, seen, .. }))) => {
                        let current = plx_data::pms::hubs_snapshot(&state).view().generation;
                        if *seen == current && pending.is_none() {
                            pending = Some((frame + 1 + rng.below(300) as u32, *before));
                        }
                    }
                    Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::CancelPage { .. }))) => pending = None,
                    _ => {}
                }
            }
            if latest.view().generation != captured.view().generation {
                step(&mut s, latest.view(), Some(key), &ScreenEvent::StoreChanged(StoreId::Hubs.ord(), latest.view().generation));
                // the engine reseats a focus whose element left the page; it must not have to
                let seated = Focusable::<TestHost>::reconcile(&s, key, &cx(latest.view(), Some(key)));
                assert_eq!(seated, key, "{at}: the landing took the focused card out of the window");
            }
            let rk = rk_now(&s, latest.view(), key);
            assert_eq!(rk, Some(expected), "{at}: the focused card is the one the keys walked to");
            assert!((offset + 1..=offset + 24).contains(&expected), "{at}: focus {expected} outside the window");
            if expected == TOTAL { reached.0 = true; }
            if reached.0 && expected == 1 { reached.1 = true; }
            if tail && reached.0 && !reached.1 {
                // walk back: the tail flips direction once the last card is reached
                dir_right = false;
            }
        }
        assert!(reached.0, "seed {seed}: the last card is reachable");
        assert!(reached.1, "seed {seed}: and so is the first");
    }
}

/// The focus read-out (`cdg`) names a card by its place in the whole listing: after one slide of
/// twelve, the window's last card (slot 23 of 24) sits at listing position 35, not 23.
#[test]
fn a_slid_row_names_its_cards_by_listing_position() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, 0, 24, true);
    let snap = plx_data::pms::hubs_snapshot(&state);
    let s = screen(snap.view());
    assert_eq!(ui_cards::CardSource::<TestHost>::global(&s.cards(snap.view(), 0), 23), 23);
    plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, 12, 24, true);
    let snap = plx_data::pms::hubs_snapshot(&state);
    let s = screen(snap.view());
    let cards = s.cards(snap.view(), 0);
    assert_eq!(ui_cards::CardSource::<TestHost>::global(&cards, 0), 12);
    assert_eq!(ui_cards::CardSource::<TestHost>::global(&cards, 23), 35);
}

// ---- the wash holds its colours for the length of a held direction key --------------------------
//
// The chrome glass reuses its blur snapshot only while the draw arguments beneath it are unchanged,
// and the wash is the bottom of every one of them: a wash re-keyed to each card a held key crosses
// changes them every frame wherever it is not the plain ground under the band (a plain wash is
// taken at composite, `backdrop::Ground`; hero art or a ramp knee under the band is not). These
// tests drive the real screen over a row whose cards each have a colour of their own and read the
// four corner colours the wash RECORDS (`AmbientWash::drawn_corners`).

/// Twelve cards, card `i` keyed to a colour that is its own: a red ramp up, a blue ramp down.
fn washed_snapshot() -> (plx_data::pms::PmsState, plx_data::pms::HubsSnapshot) {
    washed_snapshot_of(12)
}

fn washed_snapshot_of(cards: usize) -> (plx_data::pms::PmsState, plx_data::pms::HubsSnapshot) {
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_recent_window_for_test(&mut state, &adapter, 0, cards, false);
    plx_data::pms::blur_first_shelf_for_test(&mut state, |i| card_blur(i));
    let snap = plx_data::pms::hubs_snapshot(&state);
    (state, snap)
}

fn card_blur(i: usize) -> [[f32; 3]; 4] {
    [[0.1 + 0.07 * i as f32, 0.2, 0.9 - 0.07 * i as f32]; 4]
}

fn recorded_corners(s: &HomeScreen) -> [[u32; 3]; 4] {
    s.backdrop.wash.drawn_corners().map(|c| c.map(f32::to_bits))
}

/// The wash colours card `i`'s envelope keys the page to, as the 8-bit codes the GPU is sent.
fn card_ground(i: usize) -> [[u32; 3]; 4] {
    AmbientWash::keyed(card_blur(i), PageGround::CARD_W).map(|c| [0, 1, 2].map(|k| (c[k] * 255.0).round() as u32))
}
fn corner_codes(s: &HomeScreen) -> [[u32; 3]; 4] {
    recorded_corners(s).map(|c| c.map(|b| (f32::from_bits(b) * 255.0).round() as u32))
}

struct WashRig {
    s: HomeScreen,
    key: FocusKey<u32>,
    ms: u32,
}

impl WashRig {
    fn new(view: HubsView<'_>) -> Self {
        let mut s = screen(view);
        s.snap.pos = 1.0;
        s.snap_target = 1.0;
        let key = FocusKey { entry: s.entry, elem: s.rows[0].elems[0] };
        pop_card(&mut s, view, key, 1);
        let mut rig = Self { s, key, ms: 16 };
        rig.frames(view, 240); // the wash dissolves to card 0's ground and rests
        rig
    }
    fn frames(&mut self, view: HubsView<'_>, n: u32) {
        for _ in 0..n {
            self.ms += 16;
            step(&mut self.s, view, Some(self.key), &ScreenEvent::Tick(Tick { ms: self.ms, dt_us: 16_667 }));
        }
    }
    fn key_edge(&mut self, view: HubsView<'_>, edge: Edge) {
        let ev = ScreenEvent::Input(InputEvent {
            at: Tick { ms: self.ms, dt_us: 16_667 },
            source: Source::Sdl,
            kind: InputKind::Key { key: Key::Right, sym: 0, wcode: 0, edge, at_edge: false },
        });
        step(&mut self.s, view, Some(self.key), &ev);
    }
    fn right(&mut self, view: HubsView<'_>) {
        let from = self.key;
        let Step::Move(to) = Focusable::<TestHost>::neighbour(&self.s, from, Dir::Right, &cx(view, Some(from))) else {
            panic!("the row ends before the walk does");
        };
        step(&mut self.s, view, Some(to), &ScreenEvent::FocusMoved { from: Some(from), to, by: By::Dir });
        self.key = to;
    }
}

#[test]
fn a_held_direction_key_leaves_the_wash_alone_across_every_focus_move() {
    let _guard = plx_base::testlock::serial();
    let (_state, snap) = washed_snapshot();
    let view = snap.view();
    let mut rig = WashRig::new(view);
    assert_eq!(corner_codes(&rig.s), card_ground(0), "the wash rests on card 0's ground");
    let before = recorded_corners(&rig.s);
    rig.key_edge(view, Edge::Repeat); // the run is already repeating
    for _ in 0..8 {
        rig.right(view);
        for _ in 0..6 {
            rig.frames(view, 1);
            assert_eq!(recorded_corners(&rig.s), before, "the recorded wash changed under a held key");
        }
        rig.key_edge(view, Edge::Repeat);
    }
}

#[test]
fn the_wash_re_keys_to_the_resting_card_once_the_held_key_is_released() {
    let _guard = plx_base::testlock::serial();
    let (_state, snap) = washed_snapshot();
    let view = snap.view();
    let mut rig = WashRig::new(view);
    rig.key_edge(view, Edge::Repeat);
    for _ in 0..8 {
        rig.right(view);
        rig.frames(view, 6);
        rig.key_edge(view, Edge::Repeat);
    }
    assert_eq!(corner_codes(&rig.s), card_ground(0), "held: still card 0's ground");
    rig.key_edge(view, Edge::Up);
    rig.frames(view, 240);
    assert_eq!(corner_codes(&rig.s), card_ground(8), "released: the wash rests on the card focus rests on");
}

#[test]
fn a_single_press_still_re_keys_the_wash_to_the_card_it_lands_on() {
    let _guard = plx_base::testlock::serial();
    let (_state, snap) = washed_snapshot();
    let view = snap.view();
    let mut rig = WashRig::new(view);
    rig.key_edge(view, Edge::Down);
    rig.right(view);
    rig.key_edge(view, Edge::Up);
    rig.frames(view, 240);
    assert_eq!(corner_codes(&rig.s), card_ground(1), "a press with no repeat re-keys to its card");
}

#[test]
fn a_settled_wash_records_the_same_bits_frame_after_frame() {
    let _guard = plx_base::testlock::serial();
    let (_state, snap) = washed_snapshot();
    let view = snap.view();
    let mut rig = WashRig::new(view);
    rig.key_edge(view, Edge::Down);
    rig.right(view);
    rig.key_edge(view, Edge::Up);
    rig.frames(view, 60); // a second on, the spring's tail is far under one 8-bit code
    let settled = recorded_corners(&rig.s);
    for _ in 0..30 {
        rig.frames(view, 1);
        assert_eq!(recorded_corners(&rig.s), settled, "the spring's tail leaked into the recorded bits");
    }
}

// ---- the chrome glass keeps its blur source along a held row ------------------------------------
//
// Measured on the TV, a held RIGHT along the first row re-rendered the chrome glass's blur source on
// 993 of its frames, each ~6 ms of GPU. These tests walk the same held key through the real page and
// the same two chrome glasses (the profile chip and the tab track, one shared source) and count the
// frames on which that source would be re-rendered, `Sources` deciding exactly as it does on the TV.

/// The chrome's two glass surfaces at the geometry the TV logged (`region=8,0,1314,218` before the
/// non-rising margin): the profile chip's capsule and the tab track, bottom edge `TOP_BAR_BOTTOM`.
fn chrome_glasses() -> [Rect; 2] {
    let pad = plx_ui::widgets::TAB_TRACK_PAD;
    let top = plx_ui::widgets::TOP_BAR_Y - pad;
    let h = plx_ui::widgets::TAB_PILL_H + 2.0 * pad;
    let chip = plx_ui::widgets::CHIP_FRAME;
    [
        Rect::new(chip.x - pad, top, chip.w + 2.0 * pad, h),
        Rect::new(560.0, top, 674.0, h),
    ]
}

/// One drawn frame of the page plus the chrome glasses; `true` when the chrome source must be
/// re-rendered (the frame's compare found a difference, or it has never been captured).
fn chrome_source_stale(rig: &mut WashRig, view: HubsView<'_>, sources: &std::rc::Rc<std::cell::RefCell<plx_gfx::gfx::backdrop::Sources>>) -> bool {
    use plx_gfx::gfx::backdrop::{commit, discover, layer, Z};
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = discover(sources.clone());
        let context = cx(view, Some(rig.key));
        let mut f = DrawFrame::new(&context, Painter::root());
        plx_gfx::gfx::without_frame_clear(|| Screen::<TestHost>::draw(&mut rig.s, &mut f));
        let _band = layer(Z::CHROME, true);
        plx_ui::widgets::declare_chrome_glass_for_test(Painter::root(), &chrome_glasses());
    }
    sources.borrow_mut().resolve();
    let stale = !sources.borrow().entries[&Z::CHROME].valid;
    commit(sources);
    stale
}

/// What the first row's heading is drawn as: the pixel row its text is snapped to, and the 8-bit
/// code of the alpha it fades to by the place it is DRAWN at (`scroll_chrome`'s clearance: the lift
/// included, so no ink of it stands over the chip).
fn heading_px(s: &HomeScreen) -> (f32, u32) {
    let y = heading_y(s.grid.base_y(0), s.grid.lift(0));
    (plx_gfx::gfx::snap(y), (plx_ui::scroll_chrome::clearance_under_shown_chrome(y) * 255.0).round() as u32)
}

/// A held RIGHT along the first row: the chrome glass's blur source is re-rendered ONLY on frames
/// where the row heading's ink changes (it moves a whole pixel, or its fade steps an alpha code: it
/// fades by where it is drawn, so the settle of its lift is also a ramp of its alpha), and the
/// heading is the one thing under the chrome's
/// region that really changes during a hold. The focused card's pop is let go while the key is held,
/// so the heading (lifted to clear the popped tile, 17 px) settles back to its rest. That is a real
/// change under the glass's lens (its top is 14 px below the glass), so it is left to invalidate;
/// it lasts the 16 px of that settle and is over after the first moves of a run.
///
/// Everything else the TV logged as changing under a held key is not in this count: the cards'
/// tops are below the region (`BLUR_REACH` rather than `BLUR_MARGIN`), the wash neither re-keys nor
/// records below one 8-bit code, and the heading text records the pixel it lands on.
#[test]
fn a_held_right_along_the_first_row_re_renders_the_chrome_glass_source_only_for_the_heading_settle() {
    let _guard = plx_base::testlock::serial();
    let (_state, snap) = washed_snapshot_of(20);
    let view = snap.view();
    let mut rig = WashRig::new(view);
    let sources = std::rc::Rc::new(std::cell::RefCell::new(plx_gfx::gfx::backdrop::Sources::default()));
    // settle: the first capture, then the page at rest
    for _ in 0..3 {
        chrome_source_stale(&mut rig, view, &sources);
        rig.frames(view, 1);
    }
    assert!(!chrome_source_stale(&mut rig, view, &sources), "a resting page is already stable");
    rig.key_edge(view, Edge::Repeat);
    let mut frames = Vec::new(); // (stale, the heading's ink changed this frame)
    let mut heading = heading_px(&rig.s);
    let mut moves = 0;
    for _ in 0..16 {
        rig.right(view);
        moves += 1;
        for _ in 0..6 {
            rig.frames(view, 1);
            let now = heading_px(&rig.s);
            frames.push((chrome_source_stale(&mut rig, view, &sources), now != heading));
            heading = now;
        }
        rig.key_edge(view, Edge::Repeat);
    }
    assert!(moves >= 12);
    let stale = frames.iter().filter(|f| f.0).count();
    assert!(frames.iter().all(|&(stale, moved)| !stale || moved),
        "a stale frame the heading did not cause: {frames:?}");
    assert!(stale <= 17, "{stale} stale frames of {}", frames.len());
    assert!(frames[frames.len() / 2..].iter().all(|f| !f.0), "the source is still churning half-way through the run");
}

// ---- animated card landings (the Continue Watching row) ----

fn tick_n(s: &mut HomeScreen, view: HubsView<'_>, focus: Option<FocusKey<u32>>, from: u32, frames: u32) {
    for f in 0..frames {
        step(s, view, focus, &ScreenEvent::Tick(Tick { ms: (from + f) * 16, dt_us: 16_667 }));
    }
}

/// A Home on the shelves (snap at 1) showing `items` Continue Watching cards, settled.
fn landing_home(items: usize) -> (plx_data::pms::PmsState, HomeScreen) {
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, items, plx_data::pms::HubState::Ready);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    s.layout_grid();
    assert_eq!(s.rows[0].identity, HomeHubIdentity::ContinueWatching);
    tick_n(&mut s, snapshot.view(), None, 0, 6);
    (state, s)
}

#[test]
fn a_continue_watching_card_that_leaves_fades_out_as_a_ghost_of_the_previous_snapshot() {
    let _guard = plx_base::testlock::serial();
    let (mut state, mut s) = landing_home(6);
    assert!(!s.grid.shelves[0].landing_pending(), "premise: the first load played nothing");
    plx_data::pms::remove_test_item(&mut state, "2");
    let after = plx_data::pms::hubs_snapshot(&state);
    tick_n(&mut s, after.view(), None, 10, 2);
    assert!(s.grid.shelves[0].has_ghosts(), "the removed card is still drawn, from the card the screen kept");
    assert_eq!(s.ghost_cards.len(), 1, "the removed card is kept, by element");
    assert_eq!(s.rows[0].elems.len(), 5);
    tick_n(&mut s, after.view(), None, 20, 120);
    assert!(!s.grid.shelves[0].landing_pending(), "the landing settled");
    assert!(s.ghost_cards.is_empty(), "the removed cards are let go once nothing fades");
}

/// A ghost is the card that was removed, not a position in some publication: a later generation
/// that lands while it fades neither replaces it nor makes it show another card.
#[test]
fn a_ghost_keeps_its_own_card_when_another_publication_lands_mid_fade() {
    let _guard = plx_base::testlock::serial();
    let (mut state, mut s) = landing_home(6);
    plx_data::pms::remove_test_item(&mut state, "2");
    let first = plx_data::pms::hubs_snapshot(&state);
    tick_n(&mut s, first.view(), None, 10, 2);
    plx_data::pms::remove_test_item(&mut state, "5");
    let second = plx_data::pms::hubs_snapshot(&state);
    tick_n(&mut s, second.view(), None, 20, 2);
    let shelf = &s.grid.shelves[0];
    let ghosts = s.ghost_source(shelf);
    let mut rks: Vec<String> = (0..CardSource::<TestHost>::len(&ghosts)).map(|i| ghosts.item(i).expect("a kept card").rk.to_string()).collect();
    rks.sort();
    assert_eq!(rks, ["2", "5"], "each ghost is still the card that left");
    tick_n(&mut s, second.view(), None, 30, 200);
    assert!(s.ghost_cards.is_empty() && !s.grid.shelves[0].landing_pending());
}

#[test]
fn a_continue_watching_card_that_arrives_fades_in() {
    let _guard = plx_base::testlock::serial();
    let (mut state, mut s) = landing_home(5);
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 6, plx_data::pms::HubState::Ready);
    let after = plx_data::pms::hubs_snapshot(&state);
    tick_n(&mut s, after.view(), None, 10, 2);
    assert_eq!(s.rows[0].elems.len(), 6);
    assert!(s.grid.shelves[0].landing_pending(), "the arrival is playing");
    tick_n(&mut s, after.view(), None, 20, 120);
    assert!(!s.grid.shelves[0].landing_pending());
}

#[test]
fn a_dormant_home_adopts_a_landing_without_playing_it() {
    let _guard = plx_base::testlock::serial();
    let (mut state, mut s) = landing_home(6);
    s.snap.jump(0.0);
    s.snap_target = 0.0;
    plx_data::pms::remove_test_item(&mut state, "2");
    let after = plx_data::pms::hubs_snapshot(&state);
    tick_n(&mut s, after.view(), None, 10, 3);
    assert!(!s.grid.shelves[0].landing_pending());
    assert!(!s.grid.shelves[0].has_ghosts());
}

#[test]
fn a_row_that_is_not_continue_watching_cuts() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_grid_for_test(&mut state, &adapter, 2, 6);
    let before = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(before.view());
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    s.layout_grid();
    tick_n(&mut s, before.view(), None, 0, 6);
    plx_data::pms::remove_test_item(&mut state, "2");
    let after = plx_data::pms::hubs_snapshot(&state);
    tick_n(&mut s, after.view(), None, 10, 3);
    assert!(s.grid.shelves.iter().all(|sh| !sh.landing_pending() && !sh.has_ghosts()));
}

// ---- focus follows the show (layer-1 view state, 10b) ----

/// A Home on the shelves showing `items` Continue Watching EPISODES, each of its own show
/// (`show<rk>`), settled. A movie is the same card with `kind = 0` and no show.
fn show_home(items: usize) -> (plx_data::pms::PmsState, HomeScreen) {
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, items, plx_data::pms::HubState::Ready);
    for i in 1..=items {
        plx_data::pms::edit_test_item(&mut state, &i.to_string(), &|m| {
            m.kind = 3;
            m.show_rk = format!("show{}", m.rk);
        });
    }
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    s.layout_grid();
    tick_n(&mut s, snapshot.view(), None, 0, 6);
    (state, s)
}

/// Land `state` on `s` with `focus` held, the way a publication reaches a mounted Home, and play
/// the row's frames. Returns the key the engine would leave focus on (`Focusable::reconcile`).
fn land_following(s: &mut HomeScreen, state: &plx_data::pms::PmsState, focus: FocusKey<u32>) -> FocusKey<u32> {
    let after = plx_data::pms::hubs_snapshot(state);
    let context = cx(after.view(), Some(focus));
    s.sync_catalog(&context);
    let seated = Focusable::<TestHost>::reconcile(s, focus, &context);
    tick_n(s, after.view(), Some(seated), 10, 3);
    seated
}

fn card_rk(state: &plx_data::pms::PmsState, col: usize) -> String {
    let snapshot = plx_data::pms::hubs_snapshot(state);
    snapshot.view().hub(0).unwrap().items[col].rk.to_string()
}

#[test]
fn a_next_episode_replacing_the_focused_tile_keeps_its_element_and_the_focus() {
    let _guard = plx_base::testlock::serial();
    let (mut state, mut s) = show_home(6);
    let before = s.rows[0].elems.clone();
    let focus = FocusKey { entry: s.entry, elem: before[2] };
    // the server answers the finished episode "3" of show3 with its next one
    plx_data::pms::edit_test_item(&mut state, "3", &|m| m.rk = "31".into());
    let seated = land_following(&mut s, &state, focus);
    assert_eq!(s.rows[0].elems, before, "the row keeps every element");
    assert_eq!(seated, focus, "focus stays on the tile: no FocusMoved");
    assert_eq!(card_rk(&state, 2), "31", "premise: the tile now holds the next episode");
    assert!(!s.grid.shelves[0].landing_pending() && !s.grid.shelves[0].has_ghosts(),
        "the same card changing its art is not a remove plus an insert");
    assert!(s.ghost_cards.is_empty(), "no card was kept as a ghost");
    // the old key is retired, the new one names the element
    assert!(s.find_item(&HomeItemIdentity::Item { hub: HomeHubIdentity::ContinueWatching,
        sid: plx_plex::plex::ServerId::UNSET, rk: "3".into() }).is_none());
    assert!(s.find_item(&HomeItemIdentity::Item { hub: HomeHubIdentity::ContinueWatching,
        sid: plx_plex::plex::ServerId::UNSET, rk: "31".into() }).is_some());
}

/// [`show_home`] with Continue Watching as the DECK row the product has (row 0, above a Recently
/// Added shelf), which is what the store's visible-Home rules are phrased over.
fn deck_home(items: usize) -> (plx_data::pms::PmsState, std::sync::Arc<plx_data::pms::PmsAdapter>, HomeScreen) {
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_deck_home_for_test(&mut state, &adapter, items);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(snapshot.view());
    s.snap.jump(1.0);
    s.snap_target = 1.0;
    s.layout_grid();
    tick_n(&mut s, snapshot.view(), None, 0, 6);
    (state, adapter, s)
}

fn publish(state: &mut plx_data::pms::PmsState, adapter: &std::sync::Arc<plx_data::pms::PmsAdapter>,
    focused_hub: Option<usize>) -> bool {
    plx_data::pms::run(state, adapter, plx_data::stores::hubs::HubsCmd::Publish {
        home_top_visible: true, focused_hub, press_live: false }).changed
}

#[test]
fn focus_follows_show_after_cw_commit() {
    let _guard = plx_base::testlock::serial();
    let (mut state, adapter, mut s) = deck_home(6);
    assert_eq!(card_rk(&state, 2), "3", "premise: Continue Watching is the first row");
    let before = s.rows[0].elems.clone();
    let focus = FocusKey { entry: s.entry, elem: before[2] };
    // a quiet read answered the finished episode "3" of show3 with its next one; it waits staged
    plx_data::pms::stage_test_item(&mut state, "3", &|m| m.rk = "31".into());
    assert_eq!(card_rk(&state, 2), "3", "premise: the published row still holds the finished episode");
    // the focus is on another row and nothing is pressed: the Continue Watching change lands
    assert!(publish(&mut state, &adapter, Some(1)), "premise: the staged change was published");
    assert_eq!(card_rk(&state, 2), "31", "premise: the tile now holds the next episode");
    let seated = land_following(&mut s, &state, focus);
    assert_eq!(s.rows[0].elems, before, "the row keeps every element");
    assert_eq!(seated, focus, "focus stays on the show's tile: no FocusMoved");
    assert!(s.ghost_cards.is_empty() && !s.grid.shelves[0].has_ghosts(), "the same show is not a remove plus an insert");
}

#[test]
fn a_cw_change_waits_while_the_focus_is_in_continue_watching() {
    let _guard = plx_base::testlock::serial();
    let (mut state, adapter, mut s) = deck_home(6);
    let before = s.rows[0].elems.clone();
    let focus = FocusKey { entry: s.entry, elem: before[2] };
    plx_data::pms::stage_test_item(&mut state, "3", &|m| m.rk = "31".into());
    assert!(!publish(&mut state, &adapter, Some(0)), "the focus is on the row that would change: nothing lands");
    assert_eq!(card_rk(&state, 2), "3");
    let seated = land_following(&mut s, &state, focus);
    assert_eq!(s.rows[0].elems, before, "and the row's elements are exactly as they were");
    assert_eq!(seated, focus);
    assert!(s.ghost_cards.is_empty() && !s.grid.shelves[0].has_ghosts());
}

#[test]
fn the_show_leaving_the_row_seats_focus_at_the_same_index() {
    let _guard = plx_base::testlock::serial();
    let (mut state, mut s) = show_home(6);
    let before = s.rows[0].elems.clone();
    let focus = FocusKey { entry: s.entry, elem: before[2] };
    plx_data::pms::remove_test_item(&mut state, "3");
    let seated = land_following(&mut s, &state, focus);
    assert_eq!(s.rows[0].elems.len(), 5);
    assert_eq!(s.rows[0].elems.iter().position(|&e| e == seated.elem), Some(2),
        "the tile now at the same position takes the focus");
    assert_eq!(seated.elem, before[3]);
}

#[test]
fn the_last_show_leaving_the_row_seats_focus_on_the_new_last_tile() {
    let _guard = plx_base::testlock::serial();
    let (mut state, mut s) = show_home(4);
    let before = s.rows[0].elems.clone();
    let focus = FocusKey { entry: s.entry, elem: before[3] };
    plx_data::pms::remove_test_item(&mut state, "4");
    let seated = land_following(&mut s, &state, focus);
    assert_eq!(seated.elem, before[2], "the index clamps to the row's end");
}

#[test]
fn another_show_does_not_inherit_the_tile() {
    let _guard = plx_base::testlock::serial();
    let (mut state, mut s) = show_home(6);
    let before = s.rows[0].elems.clone();
    let focus = FocusKey { entry: s.entry, elem: before[2] };
    plx_data::pms::edit_test_item(&mut state, "3", &|m| { m.rk = "99".into(); m.show_rk = "showX".into(); });
    land_following(&mut s, &state, focus);
    assert_ne!(s.rows[0].elems[2], before[2], "a card of another show gets its own element");
    assert!(!s.rows[0].elems.contains(&before[2]));
    assert!(s.grid.shelves[0].landing_pending(), "it is an ordinary remove plus insert");
}

#[test]
fn the_same_rating_key_and_show_on_another_server_does_not_inherit() {
    let _guard = plx_base::testlock::serial();
    let (mut state, mut s) = show_home(6);
    let before = s.rows[0].elems.clone();
    let focus = FocusKey { entry: s.entry, elem: before[2] };
    plx_data::pms::edit_test_item(&mut state, "3", &|m| { m.rk = "31".into(); m.sid = plx_plex::plex::ServerId::from_raw(1); });
    land_following(&mut s, &state, focus);
    assert_ne!(s.rows[0].elems[2], before[2], "the next episode on a friend's server is not this server's tile");
}

#[test]
fn a_movie_has_no_show_and_keeps_the_clamp() {
    let _guard = plx_base::testlock::serial();
    let (mut state, mut s) = show_home(6);
    let before = s.rows[0].elems.clone();
    let focus = FocusKey { entry: s.entry, elem: before[2] };
    plx_data::pms::edit_test_item(&mut state, "3", &|m| { m.rk = "31".into(); m.kind = 0; m.show_rk.clear(); });
    land_following(&mut s, &state, focus);
    assert_ne!(s.rows[0].elems[2], before[2], "a card with no show never inherits");
}

#[test]
fn two_tiles_of_one_show_each_keep_their_own_element() {
    let _guard = plx_base::testlock::serial();
    let (mut state, mut s) = show_home(6);
    for rk in ["2", "5"] {
        plx_data::pms::edit_test_item(&mut state, rk, &|m| m.show_rk = "shared".into());
    }
    let first = plx_data::pms::hubs_snapshot(&state);
    s.sync_catalog(&cx(first.view(), None));
    let before = s.rows[0].elems.clone();
    plx_data::pms::edit_test_item(&mut state, "2", &|m| m.rk = "21".into());
    plx_data::pms::edit_test_item(&mut state, "5", &|m| m.rk = "51".into());
    let focus = FocusKey { entry: s.entry, elem: before[4] };
    let seated = land_following(&mut s, &state, focus);
    assert_eq!(s.rows[0].elems, before, "both replaced tiles keep their elements, in column order");
    assert_eq!(seated, focus);
}

#[test]
fn a_window_that_slid_is_paging_not_a_replacement() {
    let _guard = plx_base::testlock::serial();
    let (mut state, mut s) = show_home(6);
    let before = s.rows[0].elems.clone();
    let focus = FocusKey { entry: s.entry, elem: before[2] };
    plx_data::pms::slide_test_window(&mut state, 3);
    plx_data::pms::edit_test_item(&mut state, "3", &|m| m.rk = "31".into());
    land_following(&mut s, &state, focus);
    assert_ne!(s.rows[0].elems[2], before[2], "a card of the same show in a slid window does not inherit");
}

// ---- review round: the return half, order, and the existing tile ----

fn memory_of(s: &HomeScreen) -> HomeMemory {
    let PageMemory::Home(memory) = <HomeScreen as Screen<TestHost>>::memory(s) else { unreachable!() };
    memory
}

/// A Home evicted while the player ran and mounted again on the CURRENT catalog, then handed its
/// memory (`ScreenEvent::RestoreMemory`): the path every return to an evicted page takes.
fn remount(state: &plx_data::pms::PmsState, memory: HomeMemory) -> HomeScreen {
    let snapshot = plx_data::pms::hubs_snapshot(state);
    let mut s = screen(snapshot.view());
    step(&mut s, snapshot.view(), None, &ScreenEvent::RestoreMemory(PageMemory::Home(memory)));
    s
}

#[test]
fn an_evicted_home_remounted_on_the_next_episode_keeps_the_remembered_tile() {
    let _guard = plx_base::testlock::serial();
    let (mut state, s) = show_home(6);
    let before = s.rows[0].elems.clone();
    let memory = memory_of(&s);
    plx_data::pms::edit_test_item(&mut state, "3", &|m| m.rk = "31".into());
    let back = remount(&state, memory);
    assert_eq!(back.rows[0].elems, before, "the tile that held episode 3 holds episode 31 on the same element");
    assert!(back.find_item(&HomeItemIdentity::Item { hub: HomeHubIdentity::ContinueWatching,
        sid: plx_plex::plex::ServerId::UNSET, rk: "3".into() }).is_none(), "the old episode's key is retired, not re-added");
}

/// What a full projection of `view` over `s`'s key table would give, checked against what the
/// incremental sync left: every row's elements are the keys of the cards it shows (a descriptor's
/// are its slots), every shown card's key remembers the place it sits, and the element map is the
/// first-wins map of the rows. A row the sync kept whole without projecting it again must agree
/// with all three, so this is the invariant the fast path has to keep after ANY edit of the key
/// table: a publication, a remount's restore, an adopted show.
fn assert_matches_a_full_projection(s: &HomeScreen, view: HubsView<'_>) {
    assert_eq!(s.rows.len(), view.hub_count(), "one projection per published row");
    for row in 0..view.hub_count() {
        let hub = view.hub(row).expect("a published row");
        let identity = HomeScreen::hub_identity(view, row, hub);
        for (col, item) in hub.items.iter().take(MAX_ITEMS).enumerate().filter(|(_, item)| !s.rows[row].placeholders && !item.rk.is_empty()) {
            let at = s.item_slot(&identity, ItemRef::Item { sid: item.sid, rk: &item.rk }).unwrap_or_else(|| panic!("({row}, {col}) has no key"));
            let key = &s.items[at];
            assert_eq!((key.last_row, key.last_col), (row as u32, col as u32),
                "the key of the card at ({row}, {col}) remembers where it sits (rk {:?})", item.rk);
        }
    }
    assert_element_map_is_the_rows(s, view);
}

/// The rows hold the element of the card each shows, and the element map is the first-wins map of
/// those (which is all a screen that publishes one hub twice can ask of it: its two rows share
/// keys, so a key can remember only the place of the last row projected).
fn assert_element_map_is_the_rows(s: &HomeScreen, view: HubsView<'_>) {
    assert_eq!(s.rows.len(), view.hub_count(), "one projection per published row");
    let mut expected: HashMap<u32, (u32, u32)> = HashMap::new();
    for row in 0..view.hub_count() {
        let hub = view.hub(row).expect("a published row");
        let identity = HomeScreen::hub_identity(view, row, hub);
        let projection = &s.rows[row];
        assert_eq!(projection.identity, identity, "row {row} is the row the catalog publishes there");
        assert_eq!(projection.elems.len(), hub.items.len().min(MAX_ITEMS), "row {row} has a tile per card");
        for (col, item) in hub.items.iter().take(MAX_ITEMS).enumerate() {
            let item_ref = if item.rk.is_empty() {
                ItemRef::Slot { generation: view.generation, ordinal: col as u32 }
            } else {
                ItemRef::Item { sid: item.sid, rk: &item.rk }
            };
            let at = s.item_slot(&identity, item_ref).unwrap_or_else(|| panic!("({row}, {col}) has no key"));
            let key = &s.items[at];
            assert_eq!(projection.elems[col], key.elem,
                "the tile at ({row}, {col}) holds the element of the card it shows (rk {:?})", item.rk);
            expected.entry(key.elem).or_insert((row as u32, col as u32));
        }
    }
    assert_eq!(s.elem_at, expected, "the element map is the rows' own, first row winning");
}

/// A remount's restore on the next episode coming back FIRST (Continue Watching is ordered by last
/// viewed): the tile that held episode 3 in column 2 now holds 31 in column 0, and the key table
/// the restore leaves must say so, not just hand the old element to the new card while the row
/// still lists the elements the fresh page minted.
#[test]
fn an_evicted_home_remounted_on_a_next_episode_that_comes_first_moves_the_tile_with_its_card() {
    let _guard = plx_base::testlock::serial();
    let (mut state, s) = show_home(6);
    let before = s.rows[0].elems.clone();
    let memory = memory_of(&s);
    plx_data::pms::edit_test_item(&mut state, "3", &|m| m.rk = "31".into());
    plx_data::pms::promote_test_item(&mut state, "31");
    let back = remount(&state, memory);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    assert_eq!(card_rk(&state, 0), "31", "premise: the next episode is first");
    assert_eq!(back.rows[0].elems[0], before[2], "episode 31 is on the element that held episode 3");
    assert_matches_a_full_projection(&back, snapshot.view());
    assert_indexes_agree(&back);
}

/// xorshift64*: a repeatable stream for the histories below (a seed names a failure).
struct History(u64);

impl History {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as usize % n
    }
}

/// One random change to the catalog the server publishes: the first (Continue Watching) shelf gains,
/// loses, reorders or replaces a card, the next episode of a show arrives under another rating key,
/// every shelf reorders its cards, or a plain republication with no change at all.
fn publish_something(state: &mut plx_data::pms::PmsState, rng: &mut History, fresh: &mut u32, log: &mut Vec<String>) {
    let shelf = plx_data::pms::hubs_snapshot(state);
    let cards: Vec<(String, String)> = shelf.view().hub(0).expect("a first row").items.iter()
        .map(|card| (card.rk.clone(), card.show_rk.clone())).collect();
    let at = rng.below(cards.len());
    *fresh += 1;
    let new_rk = format!("n{fresh}");
    match rng.below(8) {
        0 => {
            log.push(format!("the next episode of {} arrives as {new_rk} in place", cards[at].0));
            let new_rk = new_rk.clone();
            plx_data::pms::edit_first_shelf_for_test(state, move |items| {
                let mut card = (*items[at]).clone();
                card.rk = new_rk;
                items[at] = Arc::new(card);
            });
        }
        1 => {
            log.push(format!("{new_rk} arrives first as the next episode of {}", cards[at].0));
            let (new_rk, show) = (new_rk.clone(), cards[at].1.clone());
            plx_data::pms::edit_first_shelf_for_test(state, move |items| {
                let mut card = (*items[at]).clone();
                card.rk = new_rk;
                card.show_rk = show;
                items.remove(at);
                items.insert(0, Arc::new(card));
            });
        }
        2 => {
            log.push(format!("{} moves to the front", cards[at].0));
            plx_data::pms::edit_first_shelf_for_test(state, move |items| { let card = items.remove(at); items.insert(0, card); });
        }
        3 if cards.len() > 2 => {
            log.push(format!("{} leaves", cards[at].0));
            plx_data::pms::edit_first_shelf_for_test(state, move |items| { items.remove(at); });
        }
        4 if cards.len() < MAX_ITEMS => {
            log.push(format!("{new_rk} joins at {at}"));
            let new_rk = new_rk.clone();
            plx_data::pms::edit_first_shelf_for_test(state, move |items| {
                let mut card = (*items[at]).clone();
                card.rk = new_rk.clone();
                card.show_rk = format!("show{new_rk}");
                items.insert(at, Arc::new(card));
            });
        }
        5 => {
            log.push("every shelf reorders".into());
            plx_data::pms::rotate_test_shelves_right(state);
        }
        6 => {
            log.push(format!("{} is answered again under {new_rk} on every shelf", cards[at].0));
            let new_rk = new_rk.clone();
            plx_data::pms::edit_test_item(state, &cards[at].0, &move |card| card.rk = new_rk.clone());
        }
        _ => {
            log.push("republished unchanged".into());
            plx_data::pms::edit_first_shelf_for_test(state, |_| {});
        }
    }
}

/// The incremental sync is a PROJECTION, not a history: after any run of publications, evictions
/// (the page remounted on the current catalog and handed its memory), in-place restores and
/// reorders, the rows, the element map and every shown card's remembered place are what a full
/// projection over the same key table gives. A row kept whole, a row inheriting a finished
/// episode's element, a restore swapping elements between keys: each is a way for the fast path to
/// leave a row that disagrees with the table, which `an_evicted_home_remounted_on_a_next_episode_that_comes_first_…`
/// was the one example of.
#[test]
fn the_incremental_sync_equals_a_full_projection_over_random_histories() {
    let _guard = plx_base::testlock::serial();
    for seed in 1..=150u64 {
        let mut rng = History(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut state = plx_data::pms::PmsState::default();
        let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
        plx_data::pms::seed_named_hubs_for_test(&mut state, &adapter, 6, &[
            ("home.continue", "", "Continue Watching"), ("home.movies.recent", "/hubs/recent", "Recent"),
            ("home.movies.genre", "/hubs/genre", "Genre"),
        ]);
        for rk in 1..=6 {
            plx_data::pms::edit_test_item(&mut state, &rk.to_string(), &|card| { card.kind = 3; card.show_rk = format!("show{}", card.rk); });
        }
        let mut s = screen(plx_data::pms::hubs_snapshot(&state).view());
        let (mut fresh, mut log) = (0u32, Vec::new());
        for step in 0..24 {
            let op = rng.below(5);
            if op == 0 {
                // evicted while the catalog moves on, then mounted on the current one and restored
                let memory = memory_of(&s);
                for _ in 0..rng.below(3) { publish_something(&mut state, &mut rng, &mut fresh, &mut log); }
                log.push("the page is remounted from its memory".into());
                s = remount(&state, memory);
            } else if op == 1 {
                log.push("the page restores its own memory".into());
                let memory = memory_of(&s);
                s.restore(&memory);
                s.sync_catalog(&cx(plx_data::pms::hubs_snapshot(&state).view(), None));
            } else {
                publish_something(&mut state, &mut rng, &mut fresh, &mut log);
                s.sync_catalog(&cx(plx_data::pms::hubs_snapshot(&state).view(), None));
            }
            let snapshot = plx_data::pms::hubs_snapshot(&state);
            let history = || format!("seed {seed}, step {step}: {:?}", log);
            let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_matches_a_full_projection(&s, snapshot.view());
                assert_indexes_agree(&s);
            }));
            if let Err(failure) = checked {
                eprintln!("{}", history());
                std::panic::resume_unwind(failure);
            }
        }
    }
}

/// A publication can carry one hub twice (the store does not dedupe `(hubIdentifier, key)`): the two
/// rows share their keys, and the element map keeps the first row's place for a shared element. When
/// the FIRST of the two changes and the second is kept whole, the element a card of the first row
/// left behind is still the second row's, so the map must keep it (it would be swept as gone while
/// on screen).
#[test]
fn a_hub_published_twice_keeps_its_second_rows_elements_when_the_first_row_changes() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_named_hubs_for_test(&mut state, &adapter, 4,
        &[("x.twice", "/hubs/twice", "Twice"), ("x.once", "/hubs/once", "Once"), ("x.twice", "/hubs/twice", "Twice")]);
    let first = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(first.view());
    assert_element_map_is_the_rows(&s, first.view());
    // the first row loses its first card; the second keeps all four
    plx_data::pms::edit_first_shelf_for_test(&mut state, |items| { items.remove(0); });
    let after = plx_data::pms::hubs_snapshot(&state);
    s.sync_catalog(&cx(after.view(), None));
    assert_eq!(s.rows[0].elems.len(), 3, "premise: the first row changed");
    assert_eq!(s.rows[2].elems.len(), 4, "premise: the second row is the one it was");
    assert_element_map_is_the_rows(&s, after.view());
    for (col, &elem) in s.rows[2].elems.iter().enumerate() {
        assert!(s.elem_at.contains_key(&elem), "the second row's card {col} is still on the page");
    }
}

#[test]
fn an_evicted_home_remounted_after_a_card_left_recovers_its_cursor_without_a_collision() {
    let _guard = plx_base::testlock::serial();
    let (mut state, s) = show_home(6);
    let memory = memory_of(&s);
    // the movie-less, show-less case: card 3 left the row and nothing replaced it, so the fresh
    // page numbers its cards from 1 again and the remembered numbers name different cards
    plx_data::pms::remove_test_item(&mut state, "3");
    let back = remount(&state, memory);
    assert_eq!(back.rows[0].elems.len(), 5);
    let mut seen = std::collections::HashSet::new();
    assert!(back.items.iter().all(|k| seen.insert(k.elem)), "no element names two cards");
}

#[test]
fn the_remembered_shows_are_in_column_order() {
    let _guard = plx_base::testlock::serial();
    let (_state, s) = show_home(6);
    let memory = memory_of(&s);
    let elems: Vec<u32> = memory.cw_shows.iter().map(|(elem, ..)| *elem).collect();
    assert_eq!(elems, s.rows[0].elems, "row order, never a hash map's");
}

#[test]
fn a_next_episode_that_is_already_in_the_row_takes_the_focus_of_the_finished_one() {
    let _guard = plx_base::testlock::serial();
    let (mut state, mut s) = show_home(6);
    // card 5 is another episode of card 3's show
    plx_data::pms::edit_test_item(&mut state, "5", &|m| m.show_rk = "show3".into());
    let first = plx_data::pms::hubs_snapshot(&state);
    s.sync_catalog(&cx(first.view(), None));
    tick_n(&mut s, first.view(), None, 0, 6);
    let before = s.rows[0].elems.clone();
    let focus = FocusKey { entry: s.entry, elem: before[2] };
    plx_data::pms::remove_test_item(&mut state, "3");
    let seated = land_following(&mut s, &state, focus);
    assert_eq!(seated.elem, before[4], "focus follows the show to its next episode, not to the same index");
}

#[test]
fn a_next_episode_that_jumps_to_column_zero_keeps_the_element_and_the_focus() {
    let _guard = plx_base::testlock::serial();
    let (mut state, mut s) = show_home(6);
    let before = s.rows[0].elems.clone();
    let focus = FocusKey { entry: s.entry, elem: before[2] };
    // Continue Watching is ordered by last viewed: the next episode comes first
    plx_data::pms::edit_test_item(&mut state, "3", &|m| m.rk = "31".into());
    plx_data::pms::promote_test_item(&mut state, "31");
    let seated = land_following(&mut s, &state, focus);
    assert_eq!(card_rk(&state, 0), "31", "premise: the next episode is first");
    assert_eq!(s.rows[0].elems[0], before[2], "the element moved to column 0 with its card");
    assert_eq!(seated, focus, "and the focus is still on it");
}

#[path = "hero_watchlist_tests.rs"]
mod hero_watchlist;

/// The billboard as it is on screen: how much of the page the hero art covers, from the
/// selected art and any art held beneath it while the selection changes. `0` is the bare wash.
fn billboard_cover(s: &HomeScreen) -> f32 {
    let incoming = reveal(s.backdrop.tex.0, &s.backdrop.art);
    let held = reveal(s.backdrop.outgoing_tex.0, &s.backdrop.outgoing_art);
    1.0 - (1.0 - incoming) * (1.0 - held)
}

/// What the screen's carousel names, by rating key.
fn carousel_rk(s: &HomeScreen) -> Option<String> {
    s.carousel.as_ref().map(|(_, rk)| rk.clone())
}

/// A Home of `items` Continue Watching cards whose billboard rests on `on` with its art up, the
/// way the player found it: the frame that covered it saw the art fully revealed. Delivers the
/// art the billboard asked for, except `withhold`.
fn home_resting_on(
    state: &plx_data::pms::PmsState,
    on: &str,
    withhold: &[&str],
) -> HomeScreen {
    let snapshot = plx_data::pms::hubs_snapshot(state);
    let view = snapshot.view();
    let at = (0..view.hero_count()).find(|&i| view.hero(i).unwrap().item.rk == on).expect("premise: a hero");
    let mut s = screen(view);
    s.carousel = Some((view.hero(at).unwrap().item.sid, on.to_string()));
    let _ = step(&mut s, view, None, &hero_tick(16));
    let probed = HERO_ART.with(|a| a.borrow().probed.clone());
    for path in probed.iter().collect::<std::collections::BTreeSet<_>>() {
        if !withhold.iter().any(|w| w == path) {
            deliver_hero_art(path);
        }
    }
    for frame in 2..=125u32 {
        let _ = step(&mut s, view, None, &hero_tick(frame * 16));
    }
    assert_eq!(carousel_rk(&s).as_deref(), Some(on), "premise: the billboard rests on {on}");
    assert!(reveal(s.backdrop.tex.0, &s.backdrop.art) > 0.999, "premise: its art is up");
    let _ = step(&mut s, view, None, &ScreenEvent::Cover);
    s
}

/// The return from the player over a catalog the refetch changed: the landing arrives under the
/// dip (`StoreChanged` while covered — `app/run.rs` holds the dip for it), then the page is
/// uncovered and runs `frames` frames. Answers each frame's (carousel rk, billboard cover).
fn return_over(s: &mut HomeScreen, state: &plx_data::pms::PmsState, from: u32, frames: u32) -> Vec<(Option<String>, f32)> {
    let snapshot = plx_data::pms::hubs_snapshot(state);
    let view = snapshot.view();
    let _ = step(s, view, None, &ScreenEvent::StoreChanged(StoreId::Hubs.ord(), view.generation));
    let _ = step(s, view, None, &ScreenEvent::Uncover);
    (from..from + frames)
        .map(|frame| {
            let _ = step(s, view, None, &hero_tick(frame * 16));
            (carousel_rk(s), billboard_cover(s))
        })
        .collect()
}

/// **Owner field report (2026-10-16): "after I exit the playback, I got an unpleasant home hero
/// reload" — the hero blinks through black, and its logo and text load again.** A played film
/// that was the billboard leaves Continue Watching when it is finished, so the post-playback
/// refetch (`app/run.rs`, `HubsCmd::RefetchHubs`) rebuilds the hero pool without it.
/// `reconcile_carousel` then restarted the rotation at hero 0 — another title whose backdrop the
/// billboard had no reason to hold (only the shown hero and its two neighbours are demand) — and
/// re-keyed the art to it with nothing beneath: the bare wash, then the new art dissolving in.
/// Reproduced on the simulator against the mock (`/:/scrobble` of the hero before BACK:
/// `reconcile_carousel reset from Some("101")`, `bind changed 101 -> 102`).
///
/// The owner's rule for the Continue Watching focus applies to the billboard too: the show's next
/// episode, else the SAME INDEX — the hero after it, whose backdrop the billboard already holds as
/// its next neighbour. And no frame of the change shows the bare wash.
#[test]
fn a_finished_hero_hands_the_billboard_to_its_successor_without_a_bare_frame() {
    let _guard = plx_base::testlock::serial();
    plx_ui::tex::install(&HeroArtSpy);
    plx_ui::tex::reset_for_test(64 << 20);
    HERO_ART.with(|a| *a.borrow_mut() = HeroArtState::default());

    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 5, plx_data::pms::HubState::Ready);
    let mut s = home_resting_on(&state, "3", &[]);

    plx_data::pms::edit_first_shelf_for_test(&mut state, |items| items.retain(|m| m.rk != "3"));
    let trace = return_over(&mut s, &state, 126, 40);
    assert!(
        trace.iter().all(|(rk, _)| rk.as_deref() == Some("4")),
        "the billboard goes to the hero that took the finished one's place, not back to the first: \
         (carousel, cover) per frame {trace:?}"
    );
    assert!(
        trace.iter().all(|&(_, cover)| cover > 0.999),
        "no frame of the return may show the bare wash under the billboard: {trace:?}"
    );
}

/// The same return when the replacement's art is NOT resident (a pool whose order the refetch
/// rewrote; a texture the cache let go): the art that was showing stays up under the billboard
/// until the new one is decoded and uploaded, and the new one then dissolves in OVER it. A genuinely
/// new picture may load; it may never load over black.
#[test]
fn a_replacement_hero_without_resident_art_loads_over_the_art_it_replaces() {
    let _guard = plx_base::testlock::serial();
    plx_ui::tex::install(&HeroArtSpy);
    plx_ui::tex::reset_for_test(64 << 20);
    HERO_ART.with(|a| *a.borrow_mut() = HeroArtState::default());

    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 5, plx_data::pms::HubState::Ready);
    let mut s = home_resting_on(&state, "3", &["/art/4"]);
    let shown = s.backdrop.tex.0;

    plx_data::pms::edit_first_shelf_for_test(&mut state, |items| items.retain(|m| m.rk != "3"));
    let waiting = return_over(&mut s, &state, 126, 20);
    assert!(
        waiting.iter().all(|&(_, cover)| cover > 0.999),
        "while the new art is in flight the old one must hold the billboard: {waiting:?}"
    );
    assert!(
        s.backdrop.outgoing_tex.0 == shown,
        "…and it is the art that was showing (held tex {}, shown {shown})",
        s.backdrop.outgoing_tex.0
    );

    deliver_hero_art("/art/4");
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let view = snapshot.view();
    let mut landing = Vec::new();
    for frame in 146..=270u32 {
        let _ = step(&mut s, view, None, &hero_tick(frame * 16));
        landing.push(billboard_cover(&s));
    }
    assert!(landing.iter().all(|&c| c > 0.999), "the new art dissolves in over the old: {landing:?}");
    assert!(s.backdrop.tex.0 != 0 && s.backdrop.tex.0 != shown, "the new art is the billboard's");
    assert_eq!(s.backdrop.outgoing_tex.0, 0, "and once it is up nothing is held beneath it");
}

/// A watched EPISODE is replaced in Continue Watching by the show's next one, under another rating
/// key, at the head of the deck. The billboard follows the show (the Continue Watching focus's own
/// rule, `rebind_shows`), and since an episode's backdrop is the show's, the art never changes.
#[test]
fn a_watched_episode_hands_the_billboard_to_the_shows_next_episode() {
    let _guard = plx_base::testlock::serial();
    plx_ui::tex::install(&HeroArtSpy);
    plx_ui::tex::reset_for_test(64 << 20);
    HERO_ART.with(|a| *a.borrow_mut() = HeroArtState::default());

    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 5, plx_data::pms::HubState::Ready);
    plx_data::pms::edit_first_shelf_for_test(&mut state, |items| {
        for m in items.iter_mut() {
            let m = std::sync::Arc::make_mut(m);
            m.kind = 3;
            m.show_rk = format!("show{}", m.rk);
        }
    });
    let mut s = home_resting_on(&state, "3", &[]);
    let shown = s.backdrop.tex.0;

    plx_data::pms::edit_first_shelf_for_test(&mut state, |items| {
        let at = items.iter().position(|m| m.rk == "3").unwrap();
        let mut next = (*items.remove(at)).clone();
        next.rk = "3n".into();
        items.insert(0, std::sync::Arc::new(next));
    });
    let trace = return_over(&mut s, &state, 126, 40);
    assert!(
        trace.iter().all(|(rk, cover)| rk.as_deref() == Some("3n") && *cover > 0.999),
        "the billboard follows the show to its next episode, art up throughout: {trace:?}"
    );
    assert_eq!(s.backdrop.tex.0, shown, "the show's backdrop, the one already showing");
}

/// The unchanged half of the contract: a refetch that brings the same catalog back moves nothing
/// on the billboard — the same hero, the same texture, fully up from the first uncovered frame.
#[test]
fn an_unchanged_refetch_leaves_the_billboard_exactly_as_it_was() {
    let _guard = plx_base::testlock::serial();
    plx_ui::tex::install(&HeroArtSpy);
    plx_ui::tex::reset_for_test(64 << 20);
    HERO_ART.with(|a| *a.borrow_mut() = HeroArtState::default());

    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 5, plx_data::pms::HubState::Ready);
    let mut s = home_resting_on(&state, "3", &[]);
    let shown = s.backdrop.tex.0;

    plx_data::pms::edit_first_shelf_for_test(&mut state, |_| {});
    let trace = return_over(&mut s, &state, 126, 20);
    assert!(
        trace.iter().all(|(rk, cover)| rk.as_deref() == Some("3") && *cover > 0.999),
        "{trace:?}"
    );
    assert_eq!(s.backdrop.tex.0, shown);
}

/// **Home hands the Continue Watching shelf the deck policy** (the owner's 2026-10-22 call: its
/// progress line is always shown), so under Details Nothing its band keeps one line while an
/// ordinary shelf's is none, and the band the focused deck opens carries the rows below it down.
#[test]
fn home_hands_the_continue_watching_shelf_the_deck_policy_under_nothing() {
    use plx_plex::plex::session::{UnderPosters, UnderPostersDetails};
    use crate::registry::captions::TEST_UNDER_POSTERS;
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_continue_window_for_test(&mut state, &adapter, 12, true);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    for (title, tall) in [(false, plx_ui::caption_policy::BAND_ONE_LINE), (true, plx_ui::caption_policy::BAND_TWO_LINES)] {
        TEST_UNDER_POSTERS.with(|cell| cell.set(Some(UnderPosters { title, details: UnderPostersDetails::None })));
        let s = screen(snapshot.view());
        assert_eq!(s.rows[0].identity, HomeHubIdentity::ContinueWatching);
        assert_eq!(s.grid.shelves[0].band_h(), tall, "title {title}: the deck keeps its progress line's band");
        assert_eq!(band_open_of(true), ui_cards::band_open_for(tall), "title {title}");
        s.grid.shelves.iter().zip(&s.rows).filter(|(_, row)| row.identity != HomeHubIdentity::ContinueWatching)
            .for_each(|(shelf, _)| assert_eq!(shelf.band_h(), if title { plx_ui::caption_policy::BAND_ONE_LINE } else { 0.0 },
                "title {title}: an ordinary shelf follows the setting"));
        TEST_UNDER_POSTERS.with(|cell| cell.set(None));
    }
}

/// **Home reserves the focused shelf's band from the Under posters setting, the same band_h the
/// Library grid reads.** The settled layout (`shelf_top_settled`: the row below the focused one is
/// carried down by what the band opens) and the end of the document both follow it, for a band of
/// nothing, one line and two lines.
#[test]
fn home_opens_its_focused_shelf_band_by_the_policys_band_h() {
    use plx_plex::plex::session::{UnderPosters, UnderPostersDetails};
    use crate::registry::captions::TEST_UNDER_POSTERS;
    for (title, details, tall) in [(false, UnderPostersDetails::None, 0.0), (false, UnderPostersDetails::Genre, 80.0),
        (true, UnderPostersDetails::YearLength, 112.0)] {
        TEST_UNDER_POSTERS.with(|cell| cell.set(Some(UnderPosters { title, details })));
        let opened = shelf_top_settled(1, 0, band_open_of(false)) - shelf_top_settled(1, 1, band_open_of(false));
        let policy = crate::registry::captions::policy();
        TEST_UNDER_POSTERS.with(|cell| cell.set(None));
        assert_eq!(plx_ui::caption_policy::band_h(&policy), tall, "premise: title {title}, details {details:?}");
        assert_eq!(opened, ui_cards::band_open_for(tall), "title {title}, details {details:?}: the row below is carried down by the band");
    }
}

/// **A caption's inputs are not in the projection, so a row the sync keeps whole cannot show a stale
/// one.** The incremental sync keeps a row while the cards it was projected from are the very same
/// cards (`Arc::ptr_eq`), and the caption is worded at the draw (`HomeCards::label`) from the card
/// the published row holds NOW and from the Under posters setting. So the two inputs of a caption
/// reach the drawn block by the two routes this pins: a FACT of a card (its title, genre, ...) that
/// changes is a new card, which projects its own row again and no other, and the block it words is
/// the new fact's; the SETTING changes nothing in the projection (no publication, no sync) and the
/// block, and the band the shelf reserves for it, follow it on the next layout.
#[test]
fn a_caption_input_that_changes_under_a_kept_row_reaches_the_drawn_block() {
    use plx_plex::plex::session::{UnderPosters, UnderPostersDetails as Details};
    use crate::registry::captions::TEST_UNDER_POSTERS;
    let _guard = plx_base::testlock::serial();
    let (rows, items) = (4usize, 5usize);
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_ring_for_test(&mut state, &adapter, rows, items, &|_| 0, (0, rows - 1));
    let setting = |title, details| TEST_UNDER_POSTERS.with(|cell| cell.set(Some(UnderPosters { title, details })));
    // The block the shelf draws under card `col` of `row`, as the card source words it.
    let block = |s: &HomeScreen, state: &plx_data::pms::PmsState, row: usize, col: usize| {
        let snapshot = plx_data::pms::hubs_snapshot(state);
        let label = ui_cards::CardSource::<TestHost>::label(&s.cards(snapshot.view(), row), col);
        let text = |line: &Option<std::ffi::CString>| line.as_ref().map(|line| line.to_string_lossy().into_owned());
        [text(&label.title), text(&label.caption)]
    };

    setting(true, Details::Genre);
    let first = plx_data::pms::hubs_snapshot(&state);
    let mut s = screen(first.view());
    drop(first);
    let (row, col) = (1usize, 2usize);
    assert_eq!(band_h_of(false), 112.0, "premise: Title on over a details line is the tall band");
    assert!(s.grid.shelves.iter().all(|shelf| shelf.band_h() == band_h_of(false)));
    let untouched = [(0, 0), (2, 3), (3, 4)].map(|(r, c)| (r, c, block(&s, &state, r, c)));
    let before = block(&s, &state, row, col);

    // The server answers row 1 again with card 2's facts changed: a new card, the same rating key.
    let rk = format!("0-{row}-{col}");
    plx_data::pms::edit_test_item(&mut state, &rk, &|m| {
        m.title = "Heat".into();
        m.genre = Some("Crime".into());
        m.video_resolution = Some("4k".into());
    });
    let cost = s.cost;
    let after = plx_data::pms::hubs_snapshot(&state);
    s.sync_catalog(&cx(after.view(), None));
    assert_eq!(s.cost.elem_at_writes - cost.elem_at_writes, items,
        "only the row whose card changed was projected again, and the others were kept whole");
    assert_matches_a_full_projection(&s, after.view());
    drop(after);
    assert_ne!(block(&s, &state, row, col), before, "premise: the fact is a caption input");
    assert_eq!(block(&s, &state, row, col), [Some("Heat".to_owned()), Some("Crime".to_owned())],
        "the block words the card the store publishes now");
    for (r, c, was) in untouched {
        assert_eq!(block(&s, &state, r, c), was, "({r}, {c}) is a card nobody changed");
    }

    // The setting changes: no publication reaches the page, so no sync runs and no row is projected.
    let cost = s.cost;
    setting(false, Details::Video);
    s.layout_grid();
    assert_eq!(s.cost.elem_at_writes, cost.elem_at_writes, "a setting is not a publication");
    let video = block(&s, &state, row, col);
    assert_eq!(video[0], None, "Title off draws no title line");
    assert_eq!(video[1].as_deref(), Some("4K"), "the Video details line reads the card's own resolution");
    assert_eq!(band_h_of(false), 80.0, "premise: one line is an 80 px band, the two lines it was were 112");
    assert!(s.grid.shelves.iter().all(|shelf| shelf.band_h() == band_h_of(false)), "every shelf reserves the new setting's band");

    // Nothing in it is a stale projection: a page remounted on the very same catalog words the same block.
    let remounted = {
        let snapshot = plx_data::pms::hubs_snapshot(&state);
        screen(snapshot.view())
    };
    assert_eq!(block(&remounted, &state, row, col), video);
    TEST_UNDER_POSTERS.with(|cell| cell.set(None));
}
