use super::*;

use plx_data::livetv::{Channel, LiveTvView};
use plx_data::stores::livetv::LiveTvStore;
use plx_machine::machine::{Chrome, FocusRead, Host, InputOwner, InstanceId, PressId, PressRead, ScreenId, Stamped};
use plx_ui::fixture::FixtureMeasure;
use plx_ui::screen::ScreenArg;

use crate::registry::PlayIntent;

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
    type Views<'a> = LiveTvView<'a>;
    type Init = crate::registry::NoInit;
    type Memory = crate::registry::PageMemory;
}

impl LiveTvLike for TestHost {
    fn livetv<'a>(cx: &Cx<'a, Self>) -> LiveTvView<'a> {
        cx.views
    }
}

const ENTRY: EntryId = EntryId(7);

fn cx<'a>(view: LiveTvView<'a>, focus: Option<u32>) -> Cx<'a, TestHost> {
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

fn step(s: &mut LiveTvScreen, view: LiveTvView<'_>, focus: Option<u32>, ev: &ScreenEvent<TestHost>) -> Vec<Stamped<TestHost>> {
    step_handled(s, view, focus, ev).1
}

/// [`step`], also answering whether the screen handled the event.
fn step_handled(s: &mut LiveTvScreen, view: LiveTvView<'_>, focus: Option<u32>, ev: &ScreenEvent<TestHost>) -> (Handled, Vec<Stamped<TestHost>>) {
    let context = cx(view, focus);
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    let mut fx = Effects::new(&mut out, plx_machine::machine::MachineId::Instance(InstanceId(9)), &mut present);
    let handled = Machine::<TestHost>::step(s, ev, &context, &mut fx);
    (handled, out)
}

fn channel(vcn: &str, name: &str) -> Channel {
    Channel { sid: plx_plex::plex::ServerId::UNSET, dvr: "7".into(), channel: format!("C{vcn}"),
        vcn: vcn.into(), name: name.into(), thumb: format!("/logos/{vcn}.jpg"), now: "Family Show".into(),
        now_thumb: format!("/programmes/{vcn}.jpg"), ends_at: 100 }
}

fn seeded() -> (LiveTvStore, LiveTvScreen) {
    let mut store = LiveTvStore::default();
    let mut silent = channel("103", "Music");
    silent.now.clear();
    store.install_for_test(vec![channel("101", "Family Channel"), channel("102", "Cartoons"), silent]);
    let mut screen = LiveTvScreen::new(ENTRY);
    step(&mut screen, store.view(), None, &ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::ContainerGroup(CHANNELS_GROUP) }));
    (store, screen)
}

fn asked_plays(out: &[Stamped<TestHost>]) -> Vec<PlayIntent> {
    out.iter().filter_map(|s| match &s.fx {
        Fx::App(AppFx::Content(ContentReq::Play { play, resume_ns: 0 })) => Some(play.clone()), _ => None,
    }).collect()
}

#[test]
fn own_store_supplies_adjacent_channels_and_on_now_shelves_with_distinct_art() {
    let _policy = captions::PolicyPin::default_setting();
    let _language = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let (store, screen) = seeded();
    let context = cx(store.view(), None);
    let mut sections = Vec::new();
    StackPage::<TestHost>::sections(&screen.page, &context, &mut sections);
    assert_eq!(sections.iter().map(|s| s.key).collect::<Vec<_>>(), [Sec::Head, Sec::Channels, Sec::Now, Sec::Status]);
    assert!(matches!(sections[1].kind, Kind::Shelf { .. }) && matches!(sections[2].kind, Kind::Shelf { .. }));
    assert_eq!(StackPage::<TestHost>::shelf_foot(&screen.page, &context, Sec::Channels), UNDER_LABEL_AIR);
    let channels = StackPage::<TestHost>::cards(&screen.page, &context, Sec::Channels).unwrap();
    let now = StackPage::<TestHost>::cards(&screen.page, &context, Sec::Now).unwrap();
    assert_eq!(CardSource::<TestHost>::len(&channels), 3, "the whole lineup includes a channel without programme data");
    assert_eq!(CardSource::<TestHost>::len(&now), 2);
    let Art::Poster(Some(logo)) = CardSource::<TestHost>::art(&channels, 0) else { panic!("channel logo") };
    let Art::Poster(Some(airing)) = CardSource::<TestHost>::art(&now, 0) else { panic!("programme artwork") };
    assert_eq!((logo.thumb, logo.title), ("/logos/101.jpg", "101 Family Channel"));
    assert_eq!((airing.thumb, airing.title), ("/programmes/101.jpg", "Family Show"));
    assert_eq!(CardSource::<TestHost>::label(&channels, 0).title.as_ref().unwrap().to_str().unwrap(), "101 Family Channel");
    assert_eq!(CardSource::<TestHost>::label(&now, 0).caption.as_ref().unwrap().to_str().unwrap(),
        "Channel 101 · Family Channel");
}

#[test]
fn channel_and_airing_ok_use_the_same_content_play_without_a_page_tuning_guard() {
    let (store, mut screen) = seeded();
    for (press, elem) in [FIRST_CARD_ELEM + 2, FIRST_CARD_ELEM + 3, FIRST_CARD_ELEM + 3].into_iter().enumerate() {
        let focus = FocusKey { entry: ENTRY, elem };
        step(&mut screen, store.view(), Some(elem), &ScreenEvent::FocusMoved { from: None, to: focus, by: plx_ui::screen::By::Dir });
        let out = step(&mut screen, store.view(), Some(elem), &ScreenEvent::PressCommit(PressId(press as u32 + 1)));
        assert_eq!(asked_plays(&out), [PlayIntent::Live { sid: store.view().channels[1].sid, dvr: "7".into(), channel: "C102".into(), title: "Cartoons".into() }]);
    }
}

#[test]
fn strip_and_profile_use_home_requests_and_back_at_the_top_selects_home() {
    use plx_ui::widgets::{TAB_KEY_CHIP, TAB_KEY_HOME, TAB_KEY_LIVETV, TAB_KEY_WATCHLIST};
    let (store, mut screen) = seeded();
    for (elem, expected) in [(TAB_KEY_HOME, HomeReq::Tab(HomeTab::Home)), (TAB_KEY_WATCHLIST, HomeReq::Tab(HomeTab::Watchlist)), (TAB_KEY_CHIP, HomeReq::Account)] {
        let out = step(&mut screen, store.view(), Some(elem), &ScreenEvent::Activate(elem));
        assert!(out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Home(req)) if *req == expected)));
    }
    assert_eq!(LiveTvScreen::strip_press(TAB_KEY_LIVETV), Some(None));
    let back = ScreenEvent::Input(InputEvent { at: Tick::default(), source: plx_machine::machine::Source::Script,
        kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false } });
    let out = step(&mut screen, store.view(), Some(FIRST_CARD_ELEM), &back);
    assert!(out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Home(HomeReq::Tab(HomeTab::Home))))));
}

#[test]
fn store_revisions_and_notices_refresh_programmes_and_status_retry_targets_live_tv() {
    let (mut store, mut screen) = seeded();
    let mut changed = channel("101", "Family Channel");
    changed.now.clear();
    store.install_for_test(vec![changed]);
    step(&mut screen, store.view(), None, &ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 }));
    assert!(screen.page.airings.is_empty(), "the store revision is read on Tick");
    for status in [LiveTvStatus::Loading, LiveTvStatus::Empty, LiveTvStatus::Failed] {
        let view = LiveTvView::for_test(status, &[], &[], 100 + status as u32);
        step(&mut screen, view, None, &ScreenEvent::StoreChanged(StoreId::LiveTv.ord(), 1));
        assert_eq!(screen.page.status, status);
        let overlay = screen.page.status_overlay(0);
        assert_eq!(overlay.kind, match status { LiveTvStatus::Loading => StatusKind::Working, LiveTvStatus::Empty => StatusKind::Empty, _ => StatusKind::Failed });
        if status == LiveTvStatus::Empty { assert_eq!(overlay.caption, plx_platform::i18n::msg::browse_livetv_empty_c()); }
        assert_eq!(overlay.action.is_some(), status == LiveTvStatus::Failed);
    }
    let failed = LiveTvView::for_test(LiveTvStatus::Failed, &[], &[], 200);
    let out = step(&mut screen, failed, Some(RETRY_ELEM), &ScreenEvent::Activate(RETRY_ELEM));
    assert!(out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Store(StoreId::LiveTv, StoreCmd::LiveTv(LiveTvCmd::Retry))))));
}
