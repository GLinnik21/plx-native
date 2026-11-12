//! Notify's real Bridge cursor with boot-selected finite ports. No live account or SSE.
use super::*;
use plx_data::stores::{tape, notify_job::{NotifyStreamPort, NotifyStreamReq, NotifyTerminal}};
use plx_machine::{job::Port, machine::Addr};
use plx_plex::plex::{self, Client, ClientKey, GrantEvidence, ServerId};
use plx_plex::plex::notify::{Notification, NotifyEnd, PlaySession, PlayState};
use crate::app::adapters::stores::{StoreRunner, NotifyTransport, LiveKind};
use std::cell::RefCell;

#[derive(Default)]
struct Script { first: Option<(Addr, NotifyStreamPort)>, launches: Vec<ServerId>, eligible: bool }
thread_local! { static SCRIPT: RefCell<Script> = RefCell::new(Script::default()); }
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        SCRIPT.with(|s| *s.borrow_mut() = Script::default());
        tape::reset_for_test(); plex::reset_servers_for_test();
    }
}
fn servers() -> (ServerId, ServerId) {
    plex::reset_servers_for_test();
    let a = plex::register_for_test("notify-bridge-a", "127.0.0.1", 9, "script-a", "script-client");
    let b = plex::register_for_test("notify-bridge-b", "127.0.0.1", 9, "script-b", "script-client");
    for sid in [a, b] { plex::describe_server(sid, "Script", "", GrantEvidence::ours()); }
    (a, b)
}
fn capture(bridge: &mut Bridge, slots: &[ServerId], allowed: bool) {
    bridge.client_keys.refill_roster_with_stream_policy(slots[0], 1, 1, slots.iter().copied(),
        plex::client_for, plex::server_facts, |_| None, |_| allowed);
}
fn eligible(req: &NotifyStreamReq) -> Option<&'static Client> {
    SCRIPT.with(|s| s.borrow().eligible).then(|| plex::client_for(req.sid)).flatten()
        .filter(|c| ClientKey::of(c) == req.key)
}
fn post(port: &NotifyStreamPort, addr: Addr, rk: &str) {
    port.post(addr, Notification::Play(vec![PlaySession {
        session_key: Some(rk.into()), client_identifier: Some("other".into()),
        rating_key: Some(rk.into()), key: None, view_offset_ms: None, state: PlayState::Playing,
    }]));
}
fn ordered(port: &NotifyStreamPort, addr: Addr, _: &'static Client, req: NotifyStreamReq) -> bool {
    SCRIPT.with(|s| {
        let mut s = s.borrow_mut(); s.launches.push(req.sid);
        if let Some((first_addr, first)) = &s.first {
            post(first, *first_addr, "a-late"); // after A's drain: MUST wait until next pass
            post(port, addr, "b-first");
            port.done(addr, NotifyTerminal::End(NotifyEnd::Closed));
            post(port, addr, "b-after-end"); // ignored, including after the terminal is taken
        } else {
            post(port, addr, "a-first");
            s.first = Some((addr, port.clone()));
        }
    });
    true
}
fn boot(bridge: &mut Bridge, held: bool) {
    SCRIPT.with(|s| s.borrow_mut().eligible = true);
    bridge.store_runner = if held { StoreRunner::held_all() } else {
        StoreRunner::Held { parked: Vec::new(), live: &[LiveKind::NotifyStream], notify: NotifyTransport::Refuse, watchlist_write: None, watchlist_streams_held: false }
    };
    bridge.store_runner.set_notify_transport(NotifyTransport::Script { eligible, run: ordered });
}
#[test]
fn direct_stream_bypasses_supplied_home_ledger_and_tape_in_record_and_replay() {
    let _serial = plx_base::testlock::serial(); let _reset = Reset;
    let (a, _) = servers();
    let mt = unsafe { plx_base::task::MainThread::assume() };
    for replay in [false, true] {
        let initial = crate::app::bootstrap::Initial::synthetic_home(17, 32517, None).unwrap();
        let mut bridge = Bridge::controlled_home(|| 0, &initial, &mt, replay);
        boot(&mut bridge, false); capture(&mut bridge, &[a], true);
        tape::init(None, replay); tape::begin(Default::default(), Default::default());
        let mut source = plx_data::stores::notify::NotifyStore::default();
        let input = plx_data::stores::notify::NotifyInput { keys: &bridge.client_keys, armed: false };
        let mut pass = source.begin(&input);
        while let Some(mut turn) = source.prepare_next(&mut pass, &input) {
            for job in source.take_jobs() {
                assert_eq!(job.admission(), plx_data::stores::Admission::Direct);
                bridge.store_runner.start_admitted(job, bridge.home_io.as_mut());
            }
            source.settle_admission(&mut pass, &mut turn);
            source.finish_turn(&mut pass, turn, &input);
        }
        let _ = source.end(pass);
        assert!(bridge.take_resource_requests().is_empty());
        assert_eq!(tape::finish(), (Vec::new(), None));
        source.suspend(); tape::reset_for_test();
        SCRIPT.with(|s| *s.borrow_mut() = Script::default());
        clean(&bridge);
    }
}

#[test]
fn fresh_host_fill_rechecks_data_gate_without_key_or_generation_change() {
    let _serial = plx_base::testlock::serial(); let _reset = Reset;
    let (a, _) = servers(); let mut bridge = Bridge::for_test(|| 0);
    crate::app::adapters::stores::refill_client_keys(&mut bridge.client_keys);
    let client = plex::client_for(a).unwrap();
    let key = bridge.client_keys.key(a);
    let generations = (bridge.client_keys.roster_gen(), bridge.client_keys.facts_gen());
    assert_eq!(bridge.client_keys.stream_allowed(a),
        client.has_token() && !client.data_io_disabled() && client.credential_transport_open());
    client.disable_data_io();
    crate::app::adapters::stores::refill_client_keys(&mut bridge.client_keys);
    assert_eq!(bridge.client_keys.key(a), key);
    assert_eq!((bridge.client_keys.roster_gen(), bridge.client_keys.facts_gen()), generations);
    assert!(!bridge.client_keys.stream_allowed(a));
    let (_, endpoints) = bridge.notify_pump_captured(&[], Default::default());
    assert!(endpoints.is_empty());
    assert!(bridge.take_held_store_jobs_for_test().is_empty());
    clean(&bridge);
}

fn clean(bridge: &Bridge) {
    bridge.assert_no_store_jobs_left_for_test();
    assert_eq!(bridge.late_store_jobs, 0);
}

#[test]
fn cursor_posts_and_terminal_map_in_connection_order_without_second_drain_or_late_jobs() {
    let _serial = plx_base::testlock::serial(); let _reset = Reset;
    let (a, b) = servers(); let mut bridge = Bridge::for_test(|| 0); boot(&mut bridge, false);
    capture(&mut bridge, &[a, b], true);
    let pages = [(a, "a-first"), (a, "a-late"), (b, "b-first"), (b, "b-after-end")]
        .map(|(sid, rk)| ContentArg::Detail { sid, rk: rk.into() });
    let (resynced, endpoints) = bridge.notify_pump_captured(&pages, Default::default());
    assert!(!resynced);
    assert!(endpoints.is_empty());
    assert_eq!(bridge.stores.reconcile.pending_for_test(), [(a, "a-first".into()), (b, "b-first".into())]);
    clean(&bridge);
    let (_, endpoints) = bridge.notify_pump_captured(&pages, Default::default());
    assert!(endpoints.is_empty());
    assert_eq!(bridge.stores.reconcile.pending_for_test(),
        [(a, "a-first".into()), (b, "b-first".into()), (a, "a-late".into())]);
    SCRIPT.with(|s| assert_eq!(s.borrow().launches, [a, b]));
    assert_eq!(plx_data::stores::Stores::NOTIFY_FILLS, [StoreId::Browse, StoreId::Hubs, StoreId::Watchlist]);
    assert!(StoreId::Hubs.ord().0 < StoreId::Notify.ord().0);
    clean(&bridge); bridge.notify_suspend();
}

#[test]
fn default_test_boot_refuses_streams_and_refills_before_begin_without_live_accounts() {
    let _serial = plx_base::testlock::serial(); let _reset = Reset;
    let (a, _) = servers(); let mut bridge = Bridge::for_test(|| 0);
    assert!(matches!(bridge.store_runner, StoreRunner::Held { notify: NotifyTransport::Refuse, .. }));
    let (_, endpoints) = bridge.notify_pump(&[], Default::default());
    assert!(endpoints.is_empty());
    assert!(bridge.client_keys.key(a).is_some(), "host capture preceded begin");
    assert!(bridge.take_held_store_jobs_for_test().is_empty(), "default refusal never parks an indefinite stream");
    assert_eq!(bridge.run_held_store_jobs_for_test(), 0);
    clean(&bridge);
}

#[test]
fn parked_acceptance_release_cancel_and_fresh_permission_loss_are_deliberate() {
    let _serial = plx_base::testlock::serial(); let _reset = Reset;
    let (a, _) = servers(); let mut bridge = Bridge::for_test(|| 0); boot(&mut bridge, true);
    capture(&mut bridge, &[a], true);
    let (_, endpoints) = bridge.notify_pump_captured(&[], Default::default());
    assert!(endpoints.is_empty());
    SCRIPT.with(|s| assert!(s.borrow().launches.is_empty()));
    assert_eq!(bridge.run_held_store_jobs_of_for_test(StoreId::Notify), 1);
    SCRIPT.with(|s| assert_eq!(s.borrow().launches, [a]));
    let (_, endpoints) = bridge.notify_pump_captured(&[], Default::default());
    assert!(endpoints.is_empty());
    bridge.notify_suspend(); bridge.notify_resume();
    let (_, endpoints) = bridge.notify_pump_captured(&[], Default::default()); // second accepted-held incarnation
    assert!(endpoints.is_empty());
    let generations = (bridge.client_keys.roster_gen(), bridge.client_keys.facts_gen());
    capture(&mut bridge, &[a], false); // policy expiry, unchanged key and generations
    assert_eq!(generations, (bridge.client_keys.roster_gen(), bridge.client_keys.facts_gen()));
    let (_, endpoints) = bridge.notify_pump_captured(&[], Default::default()); // cancels held mail before release
    assert!(endpoints.is_empty());
    assert_eq!(bridge.run_held_store_jobs_of_for_test(StoreId::Notify), 1);
    SCRIPT.with(|s| assert_eq!(s.borrow().launches, [a], "cancelled release never calls transport"));
    clean(&bridge);
}

#[test]
fn armed_record_and_replay_have_no_notify_home_or_tape_bytes_and_no_held_claim() {
    let _serial = plx_base::testlock::serial(); let _reset = Reset;
    let (a, _) = servers();
    for replay in [false, true] {
        let mut bridge = Bridge::for_test(|| 0); boot(&mut bridge, true); capture(&mut bridge, &[a], true);
        let (_, endpoints) = bridge.notify_pump_captured(&[], Default::default()); // explicitly accepted and parked
        assert!(endpoints.is_empty());
        bridge.stores.landgate.arm_recording();
        tape::init(None, replay); tape::begin(Default::default(), Default::default());
        let (_, endpoints) = bridge.notify_pump_captured(&[], Default::default());
        assert!(endpoints.is_empty());
        assert_eq!(bridge.run_held_store_jobs_of_for_test(StoreId::Notify), 1);
        assert_eq!(tape::finish(), (Vec::new(), None));
        SCRIPT.with(|s| assert!(s.borrow().launches.is_empty()));
        clean(&bridge); bridge.stores.landgate.disarm(); tape::reset_for_test();
    }
}
