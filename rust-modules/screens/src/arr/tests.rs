use plx_data::integrations::{ArrFilter, ArrKind, ArrMediaItem};
use plx_machine::machine::{
    Edge, Effects, EntryId, FocusKey, FocusRead, FocusTarget, Fx, Handled, InputEvent, InputKind,
    InstanceId, Key, Machine, MachineId, PressId, ScreenEvent, Stamped, Tick,
};
use plx_machine::present::Present;
use plx_ui::screen::{Dir, Focusable, Placed, Step};
use plx_ui::widgets::{TAB_KEY_HOME, TAB_KEY_MOVIES};
use plx_ui::Rect;

use super::*;
use crate::registry::{AppFx, ContentReq, HomeTab, PreferenceCmd};

#[derive(Clone, Default, Debug)]
struct TestInit;
impl LogicalState for TestInit {
    fn write(&self, _: &mut Canon) {}
    fn probe(&self, _: &mut String) {}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct TestArg;
impl LogicalState for TestArg {
    fn write(&self, c: &mut Canon) {
        c.u32(0);
    }
    fn probe(&self, _: &mut String) {}
}
impl plx_machine::machine::ScreenArg for TestArg {
    fn chrome(&self) -> plx_machine::machine::Chrome {
        plx_machine::machine::Chrome::None
    }
    fn id(&self) -> plx_machine::machine::ScreenId {
        plx_machine::machine::ScreenId(999)
    }
    fn title(&self) -> Option<&str> {
        None
    }
    fn same_instance(&self, other: &Self) -> bool {
        self == other
    }
}

struct TestHost;
impl plx_machine::machine::Host for TestHost {
    type Arg = TestArg;
    type Fx = AppFx;
    type Msg = crate::registry::AppMsg;
    type Elem = u32;
    type Views<'a> = ();
    type Init = TestInit;
    type Memory = ();
}

fn test_cx(focus: Option<FocusKey<u32>>) -> plx_machine::machine::Cx<'static, TestHost> {
    plx_machine::machine::Cx {
        views: (),
        tick: Tick::default(),
        measure: &plx_ui::widgets::LegacyMeasure,
        press: Default::default(),
        focus: FocusRead { current: focus, ..Default::default() },
        owner: plx_machine::machine::InputOwner::Entry(EntryId(1)),
    }
}

fn mock_item(id: u32, title: &str, year: u32, monitored: bool, has_file: bool) -> ArrMediaItem {
    ArrMediaItem {
        service: ArrKind::Radarr,
        id,
        title: title.to_string(),
        year,
        overview: "Overview description".to_string(),
        poster_url: format!("https://image.test/{id}.jpg"),
        fanart_url: format!("https://image.test/{id}_fanart.jpg"),
        monitored,
        has_file,
        status: "released".to_string(),
        size_bytes: 1_000_000,
        quality_profile_id: 1,
        ..Default::default()
    }
}

fn sample_items() -> Vec<ArrMediaItem> {
    vec![
        mock_item(1, "Movie 1", 2021, true, true),   // Monitored, Downloaded
        mock_item(2, "Movie 2", 2022, true, false),  // Monitored, Missing
        mock_item(3, "Movie 3", 2023, false, true),  // Unmonitored, Downloaded
        mock_item(4, "Movie 4", 2024, false, false), // Unmonitored, Missing
    ]
}

fn step_screen(screen: &mut ArrScreen, ev: &ScreenEvent<TestHost>, c: &plx_machine::machine::Cx<'_, TestHost>) -> (Handled, Vec<Stamped<TestHost>>) {
    let mut out = Vec::new();
    let mut present = Present::new();
    let handled = {
        let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(1)), &mut present);
        screen.step(ev, c, &mut fx)
    };
    (handled, out)
}

#[test]
fn counts_and_filtered_indices_behave_correctly() {
    let mut screen = ArrScreen::with_items(EntryId(1), ArrKind::Radarr, sample_items());
    let (total, monitored, unmonitored, missing) = screen.counts();
    assert_eq!(total, 4);
    assert_eq!(monitored, 2);
    assert_eq!(unmonitored, 2);
    assert_eq!(missing, 1);

    screen.filter = ArrFilter::All;
    assert_eq!(screen.filtered_indices(), vec![0, 1, 2, 3]);

    screen.filter = ArrFilter::Monitored;
    assert_eq!(screen.filtered_indices(), vec![0, 1]);

    screen.filter = ArrFilter::Unmonitored;
    assert_eq!(screen.filtered_indices(), vec![2, 3]);

    screen.filter = ArrFilter::Missing;
    assert_eq!(screen.filtered_indices(), vec![1]);
}

#[test]
fn header_neighbour_navigation() {
    let screen = ArrScreen::with_items(EntryId(1), ArrKind::Radarr, sample_items());
    let c = test_cx(None);

    // Left and Right between header elements
    let k1 = FocusKey { entry: EntryId(1), elem: FILTER_ALL_ELEM };
    let k2 = FocusKey { entry: EntryId(1), elem: FILTER_MONITORED_ELEM };
    let k5 = FocusKey { entry: EntryId(1), elem: REFRESH_ELEM };

    match screen.neighbour(k1, Dir::Right, &c) {
        Step::Move(k) => assert_eq!(k.elem, FILTER_MONITORED_ELEM),
        _ => panic!("expected Move right"),
    }

    match screen.neighbour(k2, Dir::Left, &c) {
        Step::Move(k) => assert_eq!(k.elem, FILTER_ALL_ELEM),
        _ => panic!("expected Move left"),
    }

    match screen.neighbour(k1, Dir::Left, &c) {
        Step::Edge => {}
        _ => panic!("expected Edge at leftmost header element"),
    }

    match screen.neighbour(k5, Dir::Right, &c) {
        Step::Edge => {}
        _ => panic!("expected Edge at rightmost header element"),
    }
}

#[test]
fn grid_neighbour_navigation() {
    let items: Vec<_> = (0..14)
        .map(|i| mock_item(i, &format!("M{i}"), 2020, true, true))
        .collect();
    let screen = ArrScreen::with_items(EntryId(1), ArrKind::Radarr, items);
    let c = test_cx(None);

    let card0 = FocusKey { entry: EntryId(1), elem: FIRST_CARD_ELEM };
    let card5 = FocusKey { entry: EntryId(1), elem: FIRST_CARD_ELEM + 5 };
    let card6 = FocusKey { entry: EntryId(1), elem: FIRST_CARD_ELEM + 6 };

    // Move right from card 0 to card 1
    match screen.neighbour(card0, Dir::Right, &c) {
        Step::Move(k) => assert_eq!(k.elem, FIRST_CARD_ELEM + 1),
        _ => panic!("expected Move right to card 1"),
    }

    // Move left from card 0: Edge of column 0
    match screen.neighbour(card0, Dir::Left, &c) {
        Step::Edge => {}
        _ => panic!("expected Edge at start of row"),
    }

    // Move right from card 5 (end of row 0 in 6-col grid): Edge
    match screen.neighbour(card5, Dir::Right, &c) {
        Step::Edge => {}
        _ => panic!("expected Edge at end of row 0"),
    }

    // Move down from card 0: moves to card 6 (row 1, col 0)
    match screen.neighbour(card0, Dir::Down, &c) {
        Step::Move(k) => assert_eq!(k.elem, FIRST_CARD_ELEM + 6),
        _ => panic!("expected Move down to card 6"),
    }

    // Move up from card 6: moves to card 0 (row 0, col 0)
    match screen.neighbour(card6, Dir::Up, &c) {
        Step::Move(k) => assert_eq!(k.elem, FIRST_CARD_ELEM),
        _ => panic!("expected Move up to card 0"),
    }

    // Move up from card 0: Edge (links to header group)
    match screen.neighbour(card0, Dir::Up, &c) {
        Step::Edge => {}
        _ => panic!("expected Edge moving up from row 0"),
    }
}

#[test]
fn seating_and_reconciliation() {
    let screen = ArrScreen::with_items(EntryId(1), ArrKind::Radarr, sample_items());
    let c = test_cx(None);

    // Seating to header group returns current filter element
    let placed = Placed {
        rect: Rect::new(0.0, 0.0, 100.0, 50.0),
        rest_rect: Rect::new(0.0, 0.0, 100.0, 50.0),
        clip: Rect::FULL,
        index: Some(0),
    };
    let seat_header = screen.seat(HEADER_GROUP, placed, &c);
    assert_eq!(seat_header.elem, FILTER_ALL_ELEM);

    // Reconcile valid card preserves key
    let valid_card = FocusKey { entry: EntryId(1), elem: FIRST_CARD_ELEM + 2 };
    assert_eq!(screen.reconcile(valid_card, &c).elem, FIRST_CARD_ELEM + 2);

    // Reconcile out-of-bounds card falls back to first card
    let oob_card = FocusKey { entry: EntryId(1), elem: FIRST_CARD_ELEM + 99 };
    assert_eq!(screen.reconcile(oob_card, &c).elem, FIRST_CARD_ELEM);
}

#[test]
fn event_step_handling() {
    let mut screen = ArrScreen::with_items(EntryId(1), ArrKind::Radarr, sample_items());
    let c = test_cx(Some(FocusKey { entry: EntryId(1), elem: FIRST_CARD_ELEM }));

    // 1. Enter requests library
    let (h, out) = step_screen(&mut screen, &ScreenEvent::Enter(plx_machine::machine::Enter::Fresh { focus: FocusTarget::FirstInGroup(HEADER_GROUP) }), &c);
    assert_eq!(h, Handled::Yes);
    assert!(out.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::Preferences(PreferenceCmd::FetchArrLibrary { .. })))));

    // 2. Activate filter button
    let (h, _) = step_screen(&mut screen, &ScreenEvent::Activate(FILTER_MONITORED_ELEM), &c);
    assert_eq!(h, Handled::Yes);
    assert_eq!(screen.filter, ArrFilter::Monitored);

    // 3. PressCommit on filter button (Enter key on focused filter pill!)
    let c_filter = test_cx(Some(FocusKey { entry: EntryId(1), elem: FILTER_MISSING_ELEM }));
    let (h, _) = step_screen(&mut screen, &ScreenEvent::PressCommit(PressId(1)), &c_filter);
    assert_eq!(h, Handled::Yes);
    assert_eq!(screen.filter, ArrFilter::Missing);

    // 4. Activate card opens Detail Mode
    assert_eq!(screen.mode, ArrMode::Grid);
    let (h, _) = step_screen(&mut screen, &ScreenEvent::Activate(FIRST_CARD_ELEM), &c);
    assert_eq!(h, Handled::Yes);
    assert_eq!(screen.mode, ArrMode::Detail(1));

    // 5. In Detail Mode, activating MONITOR toggles monitored
    let (h, out) = step_screen(&mut screen, &ScreenEvent::Activate(DETAIL_BTN_MONITOR), &c);
    assert_eq!(h, Handled::Yes);
    assert!(out.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::Preferences(PreferenceCmd::ToggleArrMonitored { .. })))));

    // 6. In Detail Mode, activating SEARCH triggers search
    let (h, out) = step_screen(&mut screen, &ScreenEvent::Activate(DETAIL_BTN_SEARCH), &c);
    assert_eq!(h, Handled::Yes);
    assert!(out.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::Preferences(PreferenceCmd::TriggerArrSearch { .. })))));

    // 7. In Detail Mode, BACK key returns to Grid Mode
    let back_ev = ScreenEvent::Input(InputEvent {
        at: Tick { ms: 0, dt_us: 0 },
        source: plx_machine::machine::Source::Sdl,
        kind: InputKind::Key {
            key: Key::Back,
            sym: 0,
            wcode: 0,
            edge: Edge::Down,
            at_edge: true,
        },
    });
    let (h, _) = step_screen(&mut screen, &back_ev, &c);
    assert_eq!(h, Handled::Yes);
    assert_eq!(screen.mode, ArrMode::Grid);

    // 8. In Grid Mode, BACK key requests app ContentReq::Back
    let (h, out) = step_screen(&mut screen, &back_ev, &c);
    assert_eq!(h, Handled::Yes);
    assert!(out.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::Content(ContentReq::Back)))));

    // 9. PressCommit on Top Strip Navbar tab (Enter key on 'Movies' in navbar!)
    let c_navbar = test_cx(Some(FocusKey { entry: EntryId(1), elem: TAB_KEY_MOVIES }));
    let (h, out) = step_screen(&mut screen, &ScreenEvent::PressCommit(PressId(2)), &c_navbar);
    assert_eq!(h, Handled::Yes);
    assert!(out.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::Content(ContentReq::Tab(HomeTab::Movies))))));

    // 10. Strip navigation via direct Activate
    let (h, out) = step_screen(&mut screen, &ScreenEvent::Activate(TAB_KEY_HOME), &c);
    assert_eq!(h, Handled::Yes);
    assert!(out.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::Content(ContentReq::Tab(HomeTab::Home))))));
}

#[test]
fn max_scroll_and_row_reveal_clamping() {
    // 59 items = 10 rows (rows 0..=9)
    let items: Vec<_> = (0..59)
        .map(|i| mock_item(i, &format!("Movie {i}"), 2024, true, true))
        .collect();
    let screen = ArrScreen::with_items(EntryId(1), ArrKind::Radarr, items);

    let max_s = screen.max_scroll();
    let expected_max = (GRID_TOP + 10.0 * PITCH_Y - (SCR_H - MARGIN_Y)).max(0.0);
    assert!((max_s - expected_max).abs() < 0.01, "max_scroll should match document boundary");

    // Row 0 rests at 0.0
    let rev_0 = screen.row_reveal(0);
    assert_eq!(rev_0, 0.0);

    // Row 9 (last row) rests at exactly max_scroll
    let rev_last = screen.row_reveal(9);
    assert_eq!(rev_last, max_s);

    // Intermediate rows stay within 0.0..=max_scroll
    for r in 0..10 {
        let rev = screen.row_reveal(r);
        assert!(rev >= 0.0 && rev <= max_s);
    }
}

#[test]
fn header_links_to_strip_and_no_scroll_jerk() {
    let mut screen = ArrScreen::with_items(EntryId(1), ArrKind::Radarr, sample_items());
    let mut links = Vec::new();
    <ArrScreen as Screen<TestHost>>::links(&screen, &mut links);

    // Verify Link exists from HEADER_GROUP going Up to STRIP
    let up_to_strip = links.iter().any(|l| l.from == HEADER_GROUP && l.dir == Dir::Up && l.to == plx_ui::containers::tabs::STRIP);
    assert!(up_to_strip, "HEADER_GROUP must link Up to STRIP");

    // Verify Link exists from STRIP going Down to HEADER_GROUP
    let down_from_strip = links.iter().any(|l| l.from == plx_ui::containers::tabs::STRIP && l.dir == Dir::Down && l.to == HEADER_GROUP);
    assert!(down_from_strip, "STRIP must link Down to HEADER_GROUP");

    // When focus moves to the top navbar strip (elem >= STRIP_BASE), scroll_target must stay 0.0 (no jerk!)
    let c = test_cx(None);
    let strip_key = FocusKey { entry: EntryId(1), elem: plx_ui::dispatch::STRIP_BASE + 2 };
    let move_to_strip = ScreenEvent::FocusMoved {
        from: Some(FocusKey { entry: EntryId(1), elem: FILTER_ALL_ELEM }),
        to: strip_key,
        by: plx_machine::machine::By::Dir,
    };
    let (h, _) = step_screen(&mut screen, &move_to_strip, &c);
    assert_eq!(h, Handled::Yes);
    assert_eq!(screen.scroll_target, 0.0, "navigating to strip must not jerk scroll_target");

    // Reconciling a strip element must preserve the strip element and not snap to card
    let reconciled = screen.reconcile(strip_key, &c);
    assert_eq!(reconciled.elem, strip_key.elem, "reconcile must preserve strip focus");
}

#[test]
fn detail_button_geometry_and_non_overlap() {
    let mut screen = ArrScreen::with_items(EntryId(1), ArrKind::Radarr, sample_items());
    screen.mode = ArrMode::Detail(0);

    let r_mon = screen.detail_button_rect(DETAIL_BTN_MONITOR);
    let r_search = screen.detail_button_rect(DETAIL_BTN_SEARCH);

    assert_eq!(r_mon.h, plx_ui::widgets::StatusOverlay::CTRL_H);
    assert_eq!(r_search.h, plx_ui::widgets::StatusOverlay::CTRL_H);

    assert_eq!(r_mon.y, 450.0);
    assert_eq!(r_search.y, 450.0);

    assert!(r_mon.w >= 180.0);
    assert!(r_search.w >= 180.0);

    // Assert strictly non-overlapping with at least CONTROL_GAP
    assert!(r_search.x >= r_mon.x + r_mon.w + CONTROL_GAP - 0.01);
}

#[test]
fn detail_mode_links_to_strip_and_preserves_focus() {
    let mut screen = ArrScreen::with_items(EntryId(1), ArrKind::Radarr, sample_items());
    screen.mode = ArrMode::Detail(0);

    let mut links = Vec::new();
    <ArrScreen as Screen<TestHost>>::links(&screen, &mut links);

    // DETAIL_GROUP must link Up to STRIP
    let up_to_strip = links.iter().any(|l| l.from == DETAIL_GROUP && l.dir == Dir::Up && l.to == plx_ui::containers::tabs::STRIP);
    assert!(up_to_strip, "DETAIL_GROUP must link Up to STRIP");

    // STRIP must link Down to DETAIL_GROUP
    let down_to_detail = links.iter().any(|l| l.from == plx_ui::containers::tabs::STRIP && l.dir == Dir::Down && l.to == DETAIL_GROUP);
    assert!(down_to_detail, "STRIP must link Down to DETAIL_GROUP");

    // Reconciling strip element while in Detail mode must preserve strip key
    let c = test_cx(None);
    let strip_key = FocusKey { entry: EntryId(1), elem: plx_ui::dispatch::STRIP_BASE + 2 };
    let rec = screen.reconcile(strip_key, &c);
    assert_eq!(rec.elem, strip_key.elem, "reconcile must preserve strip focus in detail mode");

    // Seating DETAIL_GROUP returns the monitor button
    let placed = Placed {
        rect: Rect::new(0.0, 0.0, 100.0, 50.0),
        rest_rect: Rect::new(0.0, 0.0, 100.0, 50.0),
        clip: Rect::FULL,
        index: Some(0),
    };
    let seated = screen.seat(DETAIL_GROUP, placed, &c);
    assert_eq!(seated.elem, DETAIL_BTN_MONITOR);
}
