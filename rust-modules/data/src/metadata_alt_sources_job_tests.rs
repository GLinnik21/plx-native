//! M4 contracts: no worker at emission, generation-safe terminals, ordered key fencing,
//! and the same-call pre-registration roster transaction. No ambient dev files.
use super::*;
use plx_machine::job::Port;
use plx_plex::plex::{Client, ClientKeys, KeyedClients, ServerId};
use std::sync::{Arc, atomic::Ordering};

fn roster() -> (ServerId, ServerId, ClientKeys) {
    plx_plex::plex::reset_servers_for_test();
    let a = plx_plex::plex::register_for_test("m4-a", "127.0.0.1", 9, "a", "cid");
    let b = plx_plex::plex::register_for_test("m4-b", "127.0.0.1", 9, "b", "cid");
    (a, b, ClientKeys::of([a, b].map(|s| (s, plx_plex::plex::client_for(s).unwrap()))))
}
fn ask(state: &mut MetadataState, adapter: &Arc<MetadataAdapter>, sid: ServerId, keys: &ClientKeys)
    -> (plx_machine::machine::Addr, MetaAltSourcesReq, MetaAltSourcesPort) {
    request_alt_sources(state, adapter, sid, "item", "plex://movie/m4", keys);
    let mut jobs = std::mem::take(&mut state.jobs);
    assert_eq!(jobs.len(), 1);
    let crate::stores::StoreJob::MetaAltSources(job, port) = jobs.remove(0) else { panic!("alt job") };
    (job.addr, job.req, port)
}
fn copy(sid: ServerId, rk: &str) -> AltCopy { AltCopy { sid, rk: rk.into(), ..Default::default() } }
fn aligned(adapter: &MetadataAdapter) {
    adapter.alt_roster_gen.store(plx_plex::plex::server_roster_gen(), Ordering::SeqCst);
    adapter.alt_facts_gen.store(plx_plex::plex::server_facts_gen(), Ordering::SeqCst);
}

#[test]
fn exact_job_addr_whole_snapshot_and_origin_inclusive_roster_start_nothing() {
    let _serial = plx_base::testlock::serial();
    let (a, b, mut keys) = roster();
    let extra = ServerId::from_raw(999);
    keys.refill(keys.clone().iter().chain([(extra, plx_plex::plex::ClientKey { instance: 7, token: 8, grant: 9 })]));
    let mut state = MetadataState::default();
    let adapter = Arc::new(MetadataAdapter::default());
    let (addr, req, port) = ask(&mut state, &adapter, a, &keys);
    assert_eq!(addr, alt_addr(1));
    assert_eq!((req.sid, req.rk.as_str(), req.guid.as_str()), (a, "item", "plex://movie/m4"));
    assert_eq!(req.roster_gen, plx_plex::plex::server_roster_gen());
    assert_eq!(req.others, vec![a, b]);
    assert_eq!(req.keys, keys, "whole snapshot, not only roster's matching slots");
    assert_eq!(adapter.alt_done.load(Ordering::SeqCst), 0);
    assert!(adapter.alt_slot.lock().unwrap().is_none(), "emission has run no worker");
    let job = crate::stores::StoreJob::MetaAltSources(plx_machine::job::Job { addr, req }, port);
    assert_eq!(job.store(), crate::stores::StoreId::Metadata);
    assert_eq!(job.admission(), crate::stores::Admission::Direct);
}

#[test]
fn no_guid_or_one_roster_slot_settles_without_a_job() {
    let _serial = plx_base::testlock::serial();
    let (a, _, keys) = roster();
    let mut state = MetadataState::default();
    let adapter = Arc::new(MetadataAdapter::default());
    request_alt_sources(&mut state, &adapter, a, "x", "", &keys);
    assert!(state.jobs.is_empty());
    assert_eq!(adapter.alt_done.load(Ordering::SeqCst), 1);
    plx_plex::plex::reset_servers_for_test();
    let a = plx_plex::plex::register_for_test("one", "127.0.0.1", 9, "a", "cid");
    request_alt_sources(&mut state, &adapter, a, "x", "guid", &keys);
    assert!(state.jobs.is_empty());
    assert_eq!(adapter.alt_done.load(Ordering::SeqCst), 2);
}

#[test]
fn panic_answers_empty_once_and_done_stays_owed_until_the_gated_take() {
    let _serial = plx_base::testlock::serial();
    let (a, _, keys) = roster();
    let mut state = MetadataState::default();
    let adapter = Arc::new(MetadataAdapter::default());
    let (addr, req, port) = ask(&mut state, &adapter, a, &keys);
    finish_alt_sources_with(&port, addr, req, |_| panic!("scripted resolver panic"));
    assert_eq!(adapter.alt_done.load(Ordering::SeqCst), 0, "posting is not taking");
    assert!(adapter.alt_slot.lock().unwrap().as_ref().unwrap().list.is_empty());
    pump_alt_sources(&mut state, &adapter, &keys, true, None);
    assert_eq!(adapter.alt_done.load(Ordering::SeqCst), addr.req.0);
    assert!(adapter.alt_slot.lock().unwrap().is_none());
    assert!(!pump_alt_sources(&mut state, &adapter, &keys, true, None));
}

#[test]
fn refusal_settles_immediately_without_slot_record_or_retry_and_never_regresses_done() {
    let _serial = plx_base::testlock::serial();
    let (a, _, keys) = roster();
    let mut state = MetadataState::default();
    let adapter = Arc::new(MetadataAdapter::default());
    crate::stores::tape::init(None, false);
    crate::stores::tape::begin(Default::default(), Default::default());
    let (old, _, refused) = ask(&mut state, &adapter, a, &keys);
    refused.refused(old);
    assert_eq!(adapter.alt_done.load(Ordering::SeqCst), old.req.0);
    assert!(adapter.alt_slot.lock().unwrap().is_none());
    let gate = plx_machine::landgate::Gate::default();
    gate.arm_dump(std::time::Duration::from_millis(100));
    pump_alt_sources_with_gate(&mut state, &adapter, &keys, true, None, &gate);
    assert!(state.jobs.is_empty(), "refusal does not invent retries or leave dump owed starving");
    let (new, _, port) = ask(&mut state, &adapter, a, &keys);
    port.done(new, vec![copy(a, "new")]);
    pump_alt_sources(&mut state, &adapter, &keys, true, None);
    refused.refused(old);
    assert_eq!(adapter.alt_done.load(Ordering::SeqCst), new.req.0);
    assert!(adapter.alt_slot.lock().unwrap().is_none());
    assert!(crate::stores::tape::take_results().is_empty(), "refusal/Direct result has no recorded mailbox");
    assert_eq!(crate::stores::tape::finish(), (Vec::new(), None));
    crate::stores::tape::reset_for_test();
}

#[test]
fn a_stale_writer_blocked_on_the_slot_cannot_replace_a_newer_ready_result() {
    let _serial = plx_base::testlock::serial();
    let (a, _, keys) = roster();
    let mut state = MetadataState::default();
    let adapter = Arc::new(MetadataAdapter::default());
    let (old, _, port) = ask(&mut state, &adapter, a, &keys);
    let mut slot = adapter.alt_slot.lock().unwrap();
    let writer = std::thread::spawn(move || port.done(old, vec![copy(a, "stale")]));
    // Deterministically interleave the generation writer under the SAME slot-first lock as
    // request/invalidation, while the old publisher cannot enter its critical section.
    let gen = adapter.alt_gen.fetch_add(1, Ordering::SeqCst) + 1;
    *slot = Some(AltResult { gen, roster_gen: plx_plex::plex::server_roster_gen(), sid: a,
        rk: "item".into(), list: vec![copy(a, "fresh")] });
    drop(slot);
    writer.join().unwrap();
    assert_eq!(adapter.alt_slot.lock().unwrap().as_ref().unwrap().list[0].rk, "fresh");
    pump_alt_sources(&mut state, &adapter, &keys, true, None);
    assert_eq!(alt_copies(&state, a, "item")[0].rk, "fresh");
}

static SLOTS: std::sync::Mutex<Vec<(ServerId, &'static Client)>> = std::sync::Mutex::new(Vec::new());
fn lookup(sid: ServerId) -> Option<&'static Client> {
    SLOTS.lock().unwrap().iter().find(|(s, _)| *s == sid).map(|(_, c)| *c)
}
fn register(slot: u16) -> (ServerId, &'static Client) {
    let sid = ServerId::from_raw(slot);
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    // Use Plex's public test registration, not its crate-private Client constructor. The
    // scripted lookup's logical slot is deliberately independent of the registry's dense ids.
    let actual = plx_plex::plex::register_for_test(&format!("m4-client-{slot}-{}",
        NEXT.fetch_add(1, Ordering::SeqCst)), "127.0.0.1", 9, "t", "cid");
    let c = plx_plex::plex::client_for(actual).unwrap();
    let mut slots = SLOTS.lock().unwrap();
    slots.retain(|(s, _)| *s != sid);
    slots.push((sid, c));
    (sid, c)
}
#[test]
fn each_server_is_key_checked_in_order_and_only_that_server_is_skipped() {
    let _serial = plx_base::testlock::serial();
    SLOTS.lock().unwrap().clear();
    plx_plex::plex::reset_servers_for_test();
    let original: Vec<_> = (800..808).map(register).collect();
    let keys = ClientKeys::of(original[..7].iter().copied()); // final slot was never captured
    original[1].1.set_token("new"); // same object, different token
    original[2].1.set_token(""); // grant revoked
    register(803); // same slot, different instance
    SLOTS.lock().unwrap().retain(|(s, _)| *s != original[4].0); // live client removed
    let order = [original[0].0, original[1].0, original[2].0, original[3].0, original[4].0,
        original[7].0, original[5].0, original[6].0];
    let mut seen = Vec::new();
    let list = resolve_alt_sources_with(&order, "guid", KeyedClients::new(&keys, lookup), |sid, _, guid| {
        assert_eq!(guid, "guid");
        seen.push(sid);
        if sid == original[5].0 { return None; }
        Some(serde_json::from_str::<plx_plex::plex::Envelope>(
            r#"{"MediaContainer":{"Metadata":[{"ratingKey":"copy","type":"movie","duration":60000,"librarySectionTitle":"Shelf"}]}}"#
        ).unwrap().media_container)
    });
    assert_eq!(seen, vec![original[0].0, original[5].0, original[6].0], "origin included; later matching servers still run once");
    assert_eq!(list.len(), 2);
    assert_eq!((list[0].sid, list[0].library.as_str(), list[0].dur_ms), (original[0].0, "Shelf", 60000));
}

#[test]
fn ready_real_result_wins_registration_in_the_same_pass_and_next_pass_sees_new_roster() {
    let _serial = plx_base::testlock::serial();
    let (a, b, keys) = roster();
    let mut state = MetadataState::default();
    state.current = Some(Detail { sid: a, rk: "item".into(), ..Default::default() });
    let adapter = Arc::new(MetadataAdapter::default());
    let (addr, _, port) = ask(&mut state, &adapter, a, &keys);
    aligned(&adapter);
    port.done(addr, vec![copy(a, "real"), copy(b, "real-b")]);
    let mut pass = begin_alt_sources(&mut state, &adapter, &keys, false);
    let request = pass.take_request().unwrap();
    let new = plx_plex::plex::register_for_test("m4-fallback", "127.0.0.1", 9, "c", "cid");
    let answer = request.answer(Some(alt_stand_in("friend", "Shelf", "item", "1080", 0, a, new)));
    assert!(pass.finish(Some(answer), plx_machine::landgate::fixture_gate()));
    assert_eq!(adapter.alt_gen.load(Ordering::SeqCst), addr.req.0, "no invalidation between begin/finish");
    assert_eq!(state.alt.copies[0].rk, "real", "real replaces stand-in in SAME pass");
    assert!(!state.alt.stand_in);
    begin_alt_sources(&mut state, &adapter, &keys, false).finish(None, plx_machine::landgate::fixture_gate());
    assert!(adapter.alt_gen.load(Ordering::SeqCst) > addr.req.0, "NEXT pass observes registration");
}

#[test]
fn controlled_no_item_and_rk_only_chance_cadence_and_no_list_are_explicit() {
    let _serial = plx_base::testlock::serial();
    let (a, b, keys) = roster();
    let mut state = MetadataState::default();
    let adapter = MetadataAdapter::default();
    let mut pass = begin_alt_sources(&mut state, &adapter, &keys, false);
    assert!(pass.take_request().is_none());
    pass.finish(None, plx_machine::landgate::fixture_gate());
    state.current = Some(Detail { sid: a, rk: "item".into(), ..Default::default() });
    let mut pass = begin_alt_sources(&mut state, &adapter, &keys, true);
    assert!(pass.take_request().is_none());
    pass.finish(None, plx_machine::landgate::fixture_gate());
    let mut pass = begin_alt_sources(&mut state, &adapter, &keys, false);
    let request = pass.take_request().expect("controlled did not consume chance");
    assert!(pass.take_request().is_none(), "single use request");
    pass.finish(Some(request.answer(None)), plx_machine::landgate::fixture_gate());
    state.current.as_mut().unwrap().sid = b;
    let mut pass = begin_alt_sources(&mut state, &adapter, &keys, false);
    assert!(pass.take_request().is_none(), "chance identity remains rk alone, even after no list");
    pass.finish(None, plx_machine::landgate::fixture_gate());
    alt_clear(&mut state);
    let mut pass = begin_alt_sources(&mut state, &adapter, &keys, false);
    assert!(pass.take_request().is_some(), "clear permits a new chance");
    pass.finish(None, plx_machine::landgate::fixture_gate());
}

#[test]
fn another_pass_answer_is_rejected_before_install_or_take() {
    let _serial = plx_base::testlock::serial();
    let (a, _, keys) = roster();
    let mut first = MetadataState::default();
    first.current = Some(Detail { sid: a, rk: "item".into(), ..Default::default() });
    let mut second = MetadataState::default();
    second.current = Some(Detail { sid: a, rk: "item".into(), ..Default::default() });
    let adapter = MetadataAdapter::default();
    let other_adapter = MetadataAdapter::default();
    let mut one = begin_alt_sources(&mut first, &adapter, &keys, false);
    let wrong = one.take_request().unwrap().answer(Some(vec![copy(a, "wrong")]));
    one.finish(None, plx_machine::landgate::fixture_gate());
    let two = begin_alt_sources(&mut second, &other_adapter, &keys, false);
    assert!(!two.finish(Some(wrong), plx_machine::landgate::fixture_gate()));
    assert!(second.alt.copies.is_empty());
}

#[test]
fn source_override_is_owned_armed_empty_and_serde_neutral() {
    let _serial = plx_base::testlock::serial();
    let mut detail = Detail::default();
    let neutral = serde_json::to_vec(&detail).unwrap();
    detail.source_override = Some("friend".into());
    assert_eq!(detail.source(), "friend");
    assert_eq!(serde_json::to_vec(&detail).unwrap(), neutral, "frozen terminal bytes unchanged");
    let decoded: Detail = serde_json::from_slice(&serde_json::to_vec(&detail).unwrap()).unwrap();
    assert!(decoded.source_override.is_none(), "replay never retains a dev override");
    detail.source_override = Some(String::new());
    assert_eq!(detail.source(), "", "armed empty is not unarmed");
}

#[test]
fn a_port_cannot_answer_or_refuse_another_requests_address() {
    let _serial = plx_base::testlock::serial();
    let (sid, _, keys) = roster();
    let mut state = MetadataState::default();
    let adapter = Arc::new(MetadataAdapter::default());
    let (_, _, old) = ask(&mut state, &adapter, sid, &keys);
    let (addr, _, current) = ask(&mut state, &adapter, sid, &keys);
    old.refused(addr);
    old.done(addr, vec![copy(sid, "wrong")]);
    assert!(alt_sources_owed_for_test(&adapter), "foreign terminal cannot settle current wait");
    assert!(!alt_sources_ready_for_test(&adapter), "foreign address cannot publish through old port");
    current.refused(addr);
    assert!(!alt_sources_owed_for_test(&adapter));
}

#[test]
fn reset_rotates_alt_identity_and_a_retired_completion_cannot_reach_the_same_item() {
    let _serial = plx_base::testlock::serial();
    let (sid, _, keys) = roster();
    let mut store = crate::stores::metadata::MetadataStore::default();
    let old = store.adapter_for_test();
    let (addr, _, port) = ask(store.state_mut(), &old, sid, &keys);
    store.run(crate::stores::metadata::MetadataCmd::Reset);
    assert!(!Arc::ptr_eq(&old, &store.adapter_for_test()));
    set_current_for_test(store.state_mut(), Some(Detail { sid, rk: "item".into(), ..Default::default() }));
    port.done(addr, vec![copy(sid, "retired")]);
    assert!(alt_sources_ready_for_test(&old), "old worker really completed, only to its old adapter");
    store.begin_alt_sources(&keys, true).finish(None, plx_machine::landgate::fixture_gate());
    assert!(store.view().alt_copies(sid, "item").is_empty());
    assert!(!alt_sources_owed_for_test(store.adapter_ref()));
    assert!(!alt_sources_ready_for_test(store.adapter_ref()));
    let mut witness = MetadataState::default();
    assert!(pump_alt_sources(&mut witness, &old, &keys, true, None));
    assert_eq!(alt_copies(&witness, sid, "item")[0].rk, "retired");
    assert!(!alt_sources_owed_for_test(&old));
}
