//! A Related window that lands is announced to the Detail page before the page is drawn on it.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// A page whose Related row is a 3-card head and a 24-card window of its tail starting at tail
/// position `from`; card `t{n}` is tail position `n`.
fn with_tail(sid: plx_plex::plex::ServerId, from: usize) -> plx_data::metadata::Detail {
    let card = |rk: String| plx_data::pms::PmsMovie { sid, rk, sec: 1, ..Default::default() };
    let mut related: Vec<_> = (0..3).map(|i| card(format!("h{i}"))).collect();
    related.extend((from..from + 24).map(|n| card(format!("t{n}"))));
    plx_data::metadata::Detail {
        sid,
        rk: "movie".into(),
        kind: "movie".into(),
        related,
        related_tail: plx_data::metadata::RelatedTail {
            hubs: vec![plx_data::metadata::TailHub { key: "/library/metadata/1/similar".into(), preview: 3, len: Some(100) }],
            head: 3,
            positions: (from..from + 24).collect(),
            offset: from,
            end: from + 24,
            total: 100,
            more: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// The page maps its focused key to a card through projections it rebuilds when it is told the
/// store changed (`DetailScreen::sync_keys`), and it reads the cards from the live store. The loop
/// installed a landed Related window between the frame's step and its draw, so for one drawn frame
/// the focused SLOT held the card twelve places on (seen on the simulator at every landing: the
/// focused poster and its caption changed for a frame and changed back). Each loop turn here is
/// the frame, then the pumps the loop runs before it draws; at that point the card in the slot the
/// page calls focused must be the card that holds focus.
#[test]
fn a_related_window_that_lands_is_never_drawn_before_the_page_has_heard_of_it() {
    let _guard = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("related-landing", "127.0.0.1", 9, "synthetic", "fixture");
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    // Installed before the mount: the page keys its element identities from the item it finds.
    plx_data::metadata::set_current_for_test(rig.stores.metadata.state_mut(), Some(with_tail(sid, 0)));
    d.request(MachineId::Nav, NavOp::Root(AppArg::Content(ContentArg::Detail { sid, rk: "movie".into() })));
    let mut now = 0u32;
    let mut turn = |d: &mut Dispatcher<AppHost>, rig: &mut Bridge| {
        now += 1;
        super::frame(d, rig, tick(now), Vec::new());
        // `app::run::loop_requests`, between the frame and the draw
        rig.metadata_pump_season();
    };
    for _ in 0..6 { turn(&mut d, &mut rig); }
    let entry = d.nav.top_page().unwrap().id;
    // the key of the card `t20`, found the way the item menu resolves a Related card
    let key_of = |d: &Dispatcher<AppHost>, rig: &Bridge, rk: &str| (0..256u32).map(|i| (1u32 << 31) + i).find(|&elem| {
        let focus = Some(FocusKey { entry, elem });
        rig.content_menu_arg(d, entry, &ReturnState { focus, ..Default::default() }).is_some_and(|arg| arg.rk == rk)
    }).expect("the card is on the page");
    let to = FocusKey { entry, elem: key_of(&d, &rig, "t20") };
    let from = d.focus();
    d.set_focus_in(Some(to), None);
    let instance = d.nav.instance_of(entry).unwrap();
    d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
        Delivery::Screen(ScreenEvent::FocusMoved { from, to, by: plx_machine::machine::By::Dir })));
    // the card in the Related slot the page reports focus on, read from the store the draw reads
    let shown = |d: &Dispatcher<AppHost>, rig: &Bridge| {
        let probe = content_probe(d, rig);
        let col: usize = probe.split(" col=").nth(1).and_then(|rest| rest.split(' ').next())
            .and_then(|col| col.parse().ok()).unwrap_or_else(|| panic!("no column in {probe}"));
        assert!(probe.contains(" sec=3 "), "focus is on the Related row: {probe}");
        rig.metadata_view().current().unwrap().related[col].rk.clone()
    };
    for _ in 0..4 { turn(&mut d, &mut rig); }
    assert_eq!((d.focus(), shown(&d, &rig).as_str()), (Some(to), "t20"), "premise: focus is on t20");

    // the read the row asked for answers: the window twelve further on
    plx_data::metadata::land_related_for_test(rig.stores.metadata.adapter_ref(), (0, 24), &with_tail(sid, 12));
    for n in 0..4 {
        turn(&mut d, &mut rig);
        assert_eq!(d.focus(), Some(to), "turn {n}: focus keeps its key");
        assert_eq!(shown(&d, &rig), "t20", "turn {n}: the slot drawn focused holds the focused card");
    }
    assert_eq!(rig.metadata_view().current().unwrap().related_tail.offset, 12, "the window did slide");
    plx_data::metadata::set_current_for_test(rig.stores.metadata.state_mut(), None);
    plx_plex::plex::reset_servers_for_test();
}
