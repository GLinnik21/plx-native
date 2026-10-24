use super::*;

use plx_data::pms::HubsView;
use plx_machine::machine::{Chrome, FocusRead, Host, InputOwner, InstanceId, PressId, PressRead, ScreenId, Stamped};
use plx_ui::fixture::FixtureMeasure;
use plx_ui::screen::ScreenArg;

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
        ScreenId(801)
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
    type Msg = crate::registry::AppMsg;
    type Elem = u32;
    type Views<'a> = HubsView<'a>;
    type Init = crate::registry::NoInit;
    type Memory = crate::registry::PageMemory;
}

impl HomeLike for TestHost {
    fn hubs<'a>(cx: &Cx<'a, Self>) -> HubsView<'a> {
        cx.views
    }
}

const ENTRY: EntryId = EntryId(7);

fn cx<'a>(view: HubsView<'a>, focus: Option<u32>) -> Cx<'a, TestHost> {
    static MEASURE: FixtureMeasure = FixtureMeasure;
    Cx {
        views: view,
        tick: Tick::default(),
        measure: &MEASURE,
        press: PressRead::default(),
        focus: FocusRead { current: focus.map(|elem| FocusKey { entry: ENTRY, elem }), ..Default::default() },
        owner: InputOwner::Entry(ENTRY),
    }
}

fn step(s: &mut LiveTvScreen, view: HubsView<'_>, focus: Option<u32>, ev: &ScreenEvent<TestHost>) -> Vec<Stamped<TestHost>> {
    let context = cx(view, focus);
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let mut fx = Effects::new(&mut out, plx_machine::machine::MachineId::Instance(InstanceId(9)), &mut present);
    Machine::<TestHost>::step(s, ev, &context, &mut fx);
    out
}

fn asked(out: &[Stamped<TestHost>]) -> Vec<LiveTvOut> {
    out.iter()
        .filter_map(|stamped| match &stamped.fx {
            Fx::App(AppFx::LiveTv(req)) => Some(req.clone()),
            _ => None,
        })
        .collect()
}

fn channel(vcn: &str, name: &str) -> plx_plex::plex::livetv::LiveChannel {
    plx_plex::plex::livetv::LiveChannel {
        dvr: "7".into(),
        channel: format!("C{vcn}.242.ersatztv.org"),
        vcn: vcn.into(),
        name: name.into(),
        thumb: format!("http://192.0.2.10:8409/iptv/logos/{vcn}.jpg"),
        now: "Family Show".into(),
        ends_at: 0,
    }
}

#[test]
fn the_page_shows_the_catalogs_channels_and_ok_tunes_the_focused_one() {
    let _guard = plx_base::testlock::serial();
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_live_for_test(&mut state, &adapter, 2, &[channel("101", "Family Channel"), channel("102", "Cartoons")]);
    let snapshot = plx_data::pms::hubs_snapshot(&state);
    let view = snapshot.view();

    let mut s = LiveTvScreen::new(ENTRY);
    let seat = step(&mut s, view, None, &ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::ContainerGroup(GRID_GROUP) }));
    assert_eq!(
        s.channels().iter().map(|m| m.title.as_str()).collect::<Vec<_>>(),
        ["Family Channel", "Cartoons"],
        "only the channel tiles of the catalog, in its order"
    );
    assert!(
        seat.iter().any(|stamped| matches!(&stamped.fx, Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(_))))),
        "the arrival is seated in the grid"
    );
    step(&mut s, view, Some(FIRST_CARD_ELEM + 1), &ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 }));

    let ok = step(&mut s, view, Some(FIRST_CARD_ELEM + 1), &ScreenEvent::PressCommit(PressId(1)));
    assert_eq!(
        asked(&ok),
        [LiveTvOut::Tune {
            sid: s.channels()[1].sid,
            dvr: "7".into(),
            channel: "C102.242.ersatztv.org".into(),
            title: "Cartoons".into(),
        }]
    );
}

#[test]
fn the_strip_navigates_and_the_live_tv_pill_is_the_page_itself() {
    use plx_ui::widgets::{TAB_KEY_CHIP, TAB_KEY_HOME, TAB_KEY_LIVETV, TAB_KEY_WATCHLIST};
    assert_eq!(LiveTvScreen::strip_press(TAB_KEY_HOME), Some(Some(LiveTvOut::Tab(HomeTab::Home))));
    assert_eq!(LiveTvScreen::strip_press(TAB_KEY_WATCHLIST), Some(Some(LiveTvOut::Tab(HomeTab::Watchlist))));
    assert_eq!(LiveTvScreen::strip_press(TAB_KEY_CHIP), Some(Some(LiveTvOut::Account)));
    assert_eq!(LiveTvScreen::strip_press(TAB_KEY_LIVETV), Some(None));
    assert_eq!(LiveTvScreen::strip_press(FIRST_CARD_ELEM), None, "a card is not the strip");
}
