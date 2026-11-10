//! The Live TV page's requests on the Bridge's ordered FIFO (`AppFx::LiveTv`, `Bridge::livetv_deliver`).
//! A tune reaches the playback owner and is staged for the play runner under its address; the strip's
//! tabs and BACK navigate through the container; the profile chip leaves for the loop; and the tune's
//! answer comes back to the asking instance as one addressed `AppMsg::PlayReply`.

use plx_machine::machine::{Addr, RequestId, ScreenEvent, Stamped};
use plx_plex::plex::ServerId;
use plx_screens::registry::{LiveTvOut, PlayIntent};

use super::*;

/// The page that asks: a mounted instance, the only sender a Live TV request may carry.
const PAGE: MachineId = MachineId::Instance(InstanceId(7));

/// A channel's typed play, the shape the Live TV page and Home's Live TV row both build.
fn live() -> PlayIntent {
    PlayIntent::Live {
        sid: ServerId::from_raw(0),
        dvr: "7".into(),
        channel: "C101.242.ersatztv.org".into(),
        title: "Family Channel".into(),
    }
}

/// The dispatcher's parts for a call that reads no focus and no press.
fn parts() -> CxParts<u32> {
    CxParts {
        tick: Tick::default(),
        press: Default::default(),
        focus: Default::default(),
        owner: InputOwner::Entry(EntryId(0)),
    }
}

/// One addressed message to `to`, as the dispatcher delivers it: what the step answered and what it
/// pushed.
fn deliver(rig: &mut Bridge, to: MachineId, msg: AppMsg) -> (Handled, Vec<Stamped<AppHost>>) {
    let mut out = Vec::new();
    let mut present = Present::default();
    let handled = {
        let mut fx = Effects::new(&mut out, MachineId::Nav, &mut present);
        rig.deliver(to, &msg, &parts(), &mut fx)
    };
    (handled, out)
}

/// One application effect `from` emits, as a screen's step does: what the Bridge pushed for it.
fn asked(rig: &mut Bridge, from: MachineId, fx: AppFx) -> Vec<Stamped<AppHost>> {
    let mut out = Vec::new();
    let mut present = Present::default();
    {
        let mut effects = Effects::new(&mut out, MachineId::Nav, &mut present);
        rig.app_fx(from, fx, &parts(), &mut effects);
    }
    out
}

/// A tune is the playback owner's: it is delivered on the FIFO, and no request is parked on a side
/// queue of the Bridge (`Bridge::app_effect`'s `LiveTv` arm).
#[test]
fn a_live_tv_request_is_delivered_on_the_fifo_and_parked_nowhere() {
    let _serial = plx_base::testlock::serial();
    let mut rig = Bridge::for_test(|| 0);

    let out = asked(&mut rig, PAGE, AppFx::LiveTv(LiveTvOut::Tune(live())));
    assert!(matches!(out.as_slice(), [Stamped {
        fx: Fx::Deliver(MachineId::Player, Delivery::Machine(AppMsg::LiveTv { from, out: LiveTvOut::Tune(p), .. })),
        ..
    }] if *from == PAGE && *p == live()));
    assert!(rig.take_play_starts().is_empty());
    assert!(rig.take_reqs().is_empty());

    for req in [LiveTvOut::Tab(HomeTab::Search), LiveTvOut::Back, LiveTvOut::Account] {
        let out = asked(&mut rig, PAGE, AppFx::LiveTv(req));
        assert!(matches!(out.as_slice(), [Stamped {
            fx: Fx::Deliver(MachineId::Nav, Delivery::Machine(AppMsg::LiveTv { .. })),
            ..
        }]));
    }
    assert!(rig.take_play_starts().is_empty());
    assert!(rig.take_reqs().is_empty());
}

/// The playback owner admits a tune under a fresh correlation and stages it for the play runner with
/// the asking instance as its address. Nothing is emitted for it.
#[test]
fn a_tune_reaches_the_playback_owner_and_is_staged_under_its_address() {
    let _serial = plx_base::testlock::serial();
    let mut rig = Bridge::for_test(|| 0);
    let tune = || AppMsg::LiveTv { from: PAGE, out: LiveTvOut::Tune(live()), ret: Default::default() };

    let (handled, out) = deliver(&mut rig, MachineId::Player, tune());
    assert_eq!(handled, Handled::Yes);
    assert!(out.is_empty(), "a tune is staged for the play runner, not emitted");
    let starts = rig.take_play_starts();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].to, Addr { to: PAGE, req: RequestId(1) });
    assert_eq!(starts[0].play, live());

    let (handled, _) = deliver(&mut rig, MachineId::Player, tune());
    assert_eq!(handled, Handled::Yes);
    let starts = rig.take_play_starts();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].to, Addr { to: PAGE, req: RequestId(2) });
}

/// Each request has one owner: a tune is the playback owner's and navigation the container's. A
/// request at the other address is refused, and nothing is staged for it.
#[test]
fn a_live_tv_request_at_the_wrong_owner_is_refused() {
    let _serial = plx_base::testlock::serial();
    let mut rig = Bridge::for_test(|| 0);

    let tune = AppMsg::LiveTv { from: PAGE, out: LiveTvOut::Tune(live()), ret: Default::default() };
    let (handled, out) = deliver(&mut rig, MachineId::Nav, tune);
    assert_eq!(handled, Handled::No);
    assert!(out.is_empty());
    assert!(rig.take_play_starts().is_empty());

    let back = AppMsg::LiveTv { from: PAGE, out: LiveTvOut::Back, ret: Default::default() };
    let (handled, out) = deliver(&mut rig, MachineId::Player, back);
    assert_eq!(handled, Handled::No);
    assert!(out.is_empty());
}

/// BACK and the strip's tabs leave through the container's navigation, on the FIFO. The Live TV pill
/// is the page already on screen, so it leaves nothing.
#[test]
fn back_and_a_strip_tab_navigate_through_the_container_on_the_fifo() {
    let _serial = plx_base::testlock::serial();
    let mut rig = Bridge::for_test(|| 0);
    let step = |out: LiveTvOut| AppMsg::LiveTv { from: PAGE, out, ret: Default::default() };

    let (handled, out) = deliver(&mut rig, MachineId::Nav, step(LiveTvOut::Back));
    assert_eq!(handled, Handled::Yes);
    assert!(matches!(out.as_slice(), [Stamped { fx: Fx::Nav(NavOp::SelectTab(AppArg::Home)), .. }]));

    let (handled, out) = deliver(&mut rig, MachineId::Nav, step(LiveTvOut::Tab(HomeTab::Search)));
    assert_eq!(handled, Handled::Yes);
    assert!(matches!(out.as_slice(), [Stamped { fx: Fx::Nav(NavOp::SelectTab(AppArg::Search)), .. }]));

    let (handled, out) = deliver(&mut rig, MachineId::Nav, step(LiveTvOut::Tab(HomeTab::LiveTv)));
    assert_eq!(handled, Handled::Yes);
    assert!(out.is_empty());
}

/// The profile chip is the loop's: its surface needs the container's style handshake, so the Bridge
/// queues the request for the loop and pushes no effect.
#[test]
fn the_profile_chip_leaves_for_the_loop() {
    let _serial = plx_base::testlock::serial();
    let mut rig = Bridge::for_test(|| 0);
    let chip = AppMsg::LiveTv { from: PAGE, out: LiveTvOut::Account, ret: Default::default() };

    let (handled, out) = deliver(&mut rig, MachineId::Nav, chip);
    assert_eq!(handled, Handled::Yes);
    assert!(out.is_empty());
    assert_eq!(rig.take_reqs(), vec![LoopReq::AccountMenu]);
}

/// The play runner's answer is one addressed event to the instance that asked, under the correlation
/// the playback owner gave the start.
#[test]
fn the_play_runner_answers_the_asking_instance_with_one_addressed_reply() {
    let reply = crate::app::content::play_reply(Addr { to: PAGE, req: RequestId(3) }, true);
    assert!(matches!(reply, Fx::Deliver(MachineId::Instance(InstanceId(7)), Delivery::Screen(
        ScreenEvent::Async(RequestId(3), AppMsg::PlayReply { correlation: 3, accepted: true })))));
}

/// The same tune through the real dispatcher: the drain delivers it to the playback owner, which
/// stages one start for the asking instance.
#[test]
fn a_tune_rides_the_dispatcher_drain_to_the_playback_owner() {
    let _serial = plx_base::testlock::serial();
    let mut rig = Bridge::for_test(|| 0);
    let mut d = Dispatcher::<AppHost>::new();

    d.emit(PAGE, Fx::App(AppFx::LiveTv(LiveTvOut::Tune(live()))));
    d.frame_with(&mut rig, Tick::default(), Vec::new(), Vec::new(), &mut NoTap, false);

    let starts = rig.take_play_starts();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].to.to, PAGE);
}
