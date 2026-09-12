//! Controlled-bootstrap host boundaries. The production SDL boot/loop is additionally exercised
//! by the fresh-root simulator artifact; these tests do not claim native rendering or network.
use super::*;

#[test]
fn filmography_initial_requires_complete_typed_inputs() {
    let mut value = serde_json::to_value(Initial::synthetic_home(1, 32498, None).unwrap()).unwrap();
    value["content"] = serde_json::json!({"detail":"1001", "detailsec":1,
        "detailok":true, "filmography":true, "personcredits":9, "nowan":true});
    for trigger in ["detail", "detailsec", "detailok", "filmography", "personcredits", "nowan"] {
        value["triggers"].as_array_mut().unwrap().push(serde_json::json!(format!("plxnative-{trigger}")));
    }
    let initial = Initial::from_value(value.clone()).expect("complete filmography boot");
    assert_ne!(initial.hash(), Initial::synthetic_home(1, 32498, None).unwrap().hash());
    value["content"].as_object_mut().unwrap().remove("detailsec");
    assert!(Initial::from_value(value).is_err());
}

#[test]
fn content_resources_deny_execution_and_require_exact_admissions() {
    let _guard = crate::testlock::serial();
    let initial = Initial::synthetic_home(1, 32498, None).unwrap();
    stores::init(&initial, true);
    let request = serde_json::json!({"store":"metadata","sid":0,"rk":"1001","gen":1,"client":1});
    let admission = serde_json::json!({"content_resource":true,"request":request,"admitted":false});
    stores::validate_admission(&admission, 1).unwrap();
    stores::begin([admission.clone()].into(), Default::default());
    assert!(!stores::admit(request.clone(), || panic!("replay executed a resource")));
    assert_eq!(stores::finish(), (vec![admission.clone()], None));
    let mut accepted = admission.clone();
    accepted["admitted"] = serde_json::json!(true);
    stores::begin([accepted.clone()].into(), Default::default());
    assert!(stores::admit(request.clone(), || panic!("admitted replay executed a resource")));
    assert!(stores::poll::<serde_json::Value>("person", 0,
        || panic!("empty replay polled a live mailbox")).is_none());
    assert_eq!(stores::finish(), (vec![accepted], None));
    stores::begin([admission].into(), Default::default());
    let mut wrong = request;
    wrong["rk"] = serde_json::json!("1002");
    assert!(!stores::admit(wrong, || panic!("mismatched request executed")));
    assert_eq!(stores::finish().1, Some("mismatched content resource admission"));
    stores::reset_for_test();
}

#[test]
fn controlled_script_input_roundtrips_without_claiming_a_physical_key() {
    for event in super::super::bridge::script_key(crate::ui::machine::Key::Down,
        crate::ui::machine::Tick { ms:646, dt_us:0 }) {
        let value = effects::input(&event).unwrap();
        assert_eq!(effects::input(&effects::decode_input(&value).unwrap()).unwrap(), value);
        let mut wrong = value;
        wrong["body"]["at_edge"] = serde_json::json!(true);
        assert!(effects::decode_input(&wrong).is_err());
    }
}

#[test]
fn settings_initial_is_typed_hashed_and_bound_to_its_trigger() {
    let initial = Initial::synthetic_home(17, 32517, Some("root".into())).unwrap();
    assert_eq!(initial.settings.as_deref(), Some("root"));
    assert!(initial.triggers.iter().any(|trigger| trigger == "plxnative-settings"));
    let encoded = serde_json::to_value(&initial).unwrap();
    let decoded = Initial::from_value(encoded).unwrap();
    assert_eq!(decoded.hash(), initial.hash());

    let mut bad_value = initial.clone();
    bad_value.settings = Some("other".into());
    assert_eq!(bad_value.validate(), Err("unsupported initial Settings input"));

    let mut missing_trigger = initial.clone();
    missing_trigger.triggers.retain(|trigger| trigger != "plxnative-settings");
    assert_eq!(missing_trigger.validate(), Err("incoherent initial Settings input"));

    let plain = Initial::synthetic_home(17, 32517, None).unwrap();
    assert_ne!(plain.hash(), initial.hash());
}
use crate::ui::machine::{InputEvent, InputKind, Key, Edge, Source, Tick};
use serde_json::json;

#[test]
fn typed_initial_roundtrip_and_hidden_input_hash_are_complete() {
    let initial = Initial::synthetic_home(17, 32517, None).unwrap();
    let value = serde_json::to_value(&initial).unwrap();
    assert_eq!(Initial::decode(value.clone(), initial.hash()).unwrap().hash(), initial.hash());
    let mut changed = initial.clone();
    changed.consent.errors = true;
    changed.validate().unwrap();
    assert_ne!(changed.hash(), initial.hash(), "automation hides the prompt, not its logical initial decision");
    let press = crate::ui::press::Press::new();
    assert_ne!(super::super::recorder::state_hash(&press,"home","","",0,0,0,initial.hash()),
        super::super::recorder::state_hash(&press,"home","","",0,0,0,changed.hash()));
    assert!(Initial::decode(serde_json::to_value(&changed).unwrap(),initial.hash()).is_err());
    let mut unknown = value.clone(); unknown["unrecognized"] = json!(true);
    assert!(Initial::from_value(unknown).is_err());
    let mut inconsistent = value.clone(); inconsistent["session"]["next_req"] = json!(1);
    assert!(Initial::from_value(inconsistent).is_err());
    let mut missing = value.clone(); missing.as_object_mut().unwrap().remove("entropy");
    assert!(Initial::from_value(missing).is_err());
    assert!(Initial::from_value(json!({})).is_err());
    let mut bad_seed = value; bad_seed["entropy"] = json!({"Seeded":18});
    assert!(Initial::from_value(bad_seed).is_err());
}

#[test]
fn screen_input_codec_retains_full_time_source_edge_and_hit_identity() {
    for source in [Source::Sdl,Source::RemoteFifo,Source::Script,Source::Replay] {
        for edge in [Edge::Down,Edge::Repeat,Edge::Up] {
            let input = InputEvent { at:Tick { ms:u32::MAX-3,dt_us:17 },source,
                kind:InputKind::Key { key:Key::Left,sym:1073741904,wcode:0,edge,at_edge:false } };
            let encoded = effects::input(&input).unwrap();
            let decoded = effects::decode_input(&encoded).unwrap();
            assert_eq!(effects::input(&decoded).unwrap(),encoded);
            let mut changed = input.clone(); changed.at.ms -= 1;
            assert_ne!(effects::input(&changed).unwrap(),encoded);
            let mut changed = input.clone();
            if let InputKind::Key { at_edge,.. } = &mut changed.kind { *at_edge = true; }
            assert_ne!(effects::input(&changed).unwrap(),encoded);
            assert!(effects::decode_input(&effects::input(&changed).unwrap()).is_err(),"an engine re-delivery is not external ingress");
        }
    }
    let pointer = |hit| InputEvent { at:Tick {ms:7,dt_us:0},source:Source::Sdl,
        kind:InputKind::Pointer {x:-0.0,y:1.5,hit} };
    assert_ne!(effects::input(&pointer(Some(1))).unwrap(), effects::input(&pointer(Some(2))).unwrap());
    assert_ne!(effects::input(&pointer(None)).unwrap(),effects::input(&pointer(Some(1))).unwrap());
}

fn activate(bridge: &mut super::super::bridge::Bridge) {
    let mut dispatcher = crate::ui::dispatch::Dispatcher::<super::super::bridge::AppHost>::new();
    super::super::bridge::execute_session_command(&mut dispatcher,crate::auth::SessionCmd::ActivateDevBootstrap);
    dispatcher.frame_with(bridge,Tick::default(),Vec::new(),Vec::new(),&mut crate::ui::dispatch::NoTap,false);
    assert!(bridge.take_session_ready().is_some());
}

#[test]
fn normal_and_controlled_activation_publish_the_owner_supplied_scope() {
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("controlled-profile-boundary");
    session.assert_only_target();
    let before = std::fs::read(session.path()).unwrap();
    struct RegistryCleanup;
    impl Drop for RegistryCleanup { fn drop(&mut self) { crate::plex::reset_servers_for_test(); } }
    let _cleanup = RegistryCleanup;
    crate::plex::reset_servers_for_test();
    let mt = unsafe { crate::task::MainThread::assume() };
    let initial = Initial::synthetic_home(19,9,None).unwrap();
    let mut live = super::super::bridge::Bridge::new(
        ||0, initial.session.clone(), initial.consent.clone(), &mt);
    activate(&mut live);
    let live_view = live.profile_resource_view().unwrap();
    let live_owner = live.snapshot_session_init();
    assert_eq!(live_view.generation,live_owner.profile_scope.0);
    assert_ne!(live_view.generation,0);
    let ambient = crate::plex::session::current_snapshot();
    crate::plex::reset_servers_for_test();
    crate::plex::Client::restore_generation_seed(initial.primary_client).unwrap();
    let mut controlled = super::super::bridge::Bridge::controlled_home(||0,&initial,&mt,true);
    let retained = controlled.profile_resource_view().unwrap();
    activate(&mut controlled);
    let scoped = controlled.profile_resource_view().unwrap();
    assert_eq!(scoped.generation,controlled.snapshot_session_init().profile_scope.0);
    assert_eq!(scoped.generation,live_view.generation);
    assert_eq!(serde_json::to_value(&scoped.user).unwrap(),serde_json::to_value(&live_view.user).unwrap());
    assert_eq!(retained.generation,0,"old scoped resource read remains immutable");
    assert!(std::sync::Arc::ptr_eq(&ambient,&crate::plex::session::current_snapshot()),"scoped replay never replaces ambient publication");
    assert_eq!(std::fs::read(session.path()).unwrap(),before);
    controlled.bind_primary(initial.primary_client).unwrap();
    let client = controlled.recorded_client(initial.primary_client).unwrap();
    assert_eq!(client.denied_data_requests(),0);
    assert!(client.sections().is_none(),"real Client GET is denied before HTTP");
    assert!(client.fetch_built("/synthetic").is_none(),"poster HTTP arm is denied too");
    assert_eq!(client.denied_data_requests(),2);
    assert_eq!(controlled.controlled_failure(),Some("unrecorded client IO attempted"));
}

fn recording(initial: &Initial) -> Recording {
    let mut header = crate::ui::rec::Header::new(super::super::recorder::state_fp(),initial);
    header.init_data = serde_json::to_value(initial).unwrap();
    header.features = super::super::recorder::features();
    header.triggers = initial.triggers.clone();
    Recording { header,frames:vec![crate::ui::rec::Frame { f:0,tick:Some(Tick::default()),st:Some(0),..Default::default() }],
        metrics:Default::default(),stopped_at:None }
}

#[test]
fn whole_record_preflight_rejects_bad_late_results_and_markers_without_resources() {
    let initial = Initial::synthetic_home(23,32517,None).unwrap();
    let mut record = recording(&initial);
    super::super::recorder::validate_controlled(&record,&initial).unwrap();
    record.frames[0].st = None;
    assert!(super::super::recorder::validate_controlled(&record,&initial).is_err(),"missing state grades cannot produce an empty SAME");
    record.frames[0].st = Some(0);
    record.frames.push(crate::ui::rec::Frame { f:1,tick:Some(Tick {ms:16,dt_us:16000}),
        results:vec![json!({"f":1,"t":"async","to":"store:1","req":1,"payload":{
            "kind":"hubs","version":1,"gen":1,"seq":1,"sid":0,"client":1,"token_gen":1,"build":null}})],
        lands:vec![(1,1,1)],st:Some(0),present:Some(false),..Default::default() });
    super::super::recorder::validate_controlled(&record,&initial).unwrap();
    record.frames[1].present = None;
    assert!(super::super::recorder::validate_controlled(&record,&initial).is_err());
    record.frames[1].present = Some(false);
    for field in ["client","token_gen","sid"] {
        let saved = record.frames[1].results[0].clone();
        record.frames[1].results[0]["payload"][field] = json!(33);
        assert!(super::super::recorder::validate_controlled(&record,&initial).is_err());
        record.frames[1].results[0] = saved;
    }
    record.frames[1].results.push(json!({"f":1,"t":"async","to":"store:1","req":2,"payload":{"kind":"unimplemented"}}));
    assert!(super::super::recorder::validate_controlled(&record,&initial).is_err(),"valid prefix cannot hide unsupported suffix");
    record.frames[1].results.pop();
    record.frames[1].effects.push(json!({"f":1,"t":"eff","e":"Deliver","from":"Nav",
        "payload":{"unsupported":"unsupported Home screen delivery"}}));
    assert!(super::super::recorder::validate_controlled(&record,&initial).is_err());
}

#[test]
fn controlled_hubs_commands_preserve_normal_store_notice_bookkeeping() {
    let _serial = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let mt = unsafe { crate::task::MainThread::assume() };
    let publisher = crate::plex::session::ProfilePublisher::scoped(&mt);
    let mut io = HomeIo { replay:true,preferences:Default::default(),requests:Vec::new(),admissions:Default::default(),
        failure:None,profile:publisher.snapshot() };
    for command in [crate::stores::hubs::HubsCmd::Reset,crate::stores::hubs::HubsCmd::RefetchHubs,
        crate::stores::hubs::HubsCmd::Retry] {
        let before = crate::stores::gen(crate::stores::StoreId::Hubs);
        let normal = crate::stores::hubs::apply(command.clone());
        let delta = crate::stores::gen(crate::stores::StoreId::Hubs).wrapping_sub(before);
        let before = crate::stores::gen(crate::stores::StoreId::Hubs);
        let controlled = io.hubs(Some(command),0.0);
        assert_eq!(controlled.changed,normal.changed);
        assert_eq!(crate::stores::gen(crate::stores::StoreId::Hubs).wrapping_sub(before),delta,
            "controlled resource execution must not drop the Store machine's notice");
    }
}

#[test]
fn controlled_admission_records_real_discovery_spawn_refusal() {
    let _serial = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    let sid = crate::plex::register_for_test("s00000001", "127.0.0.1", 9, "s00000002", "s00000003");
    assert!(crate::plex::set_current(sid));
    let mt = unsafe { crate::task::MainThread::assume() };
    let publisher = crate::plex::session::ProfilePublisher::scoped(&mt);
    let mut io = HomeIo { replay:false,preferences:Default::default(),requests:Vec::new(),admissions:Default::default(),
        failure:None,profile:publisher.snapshot() };
    crate::browse::with_refused_discovery_for_test(|| io.discovery());
    assert_eq!(io.requests.len(), 1);
    assert_eq!(io.requests[0]["admitted"], serde_json::json!(false), "real refusal must be recorded");
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::plex::reset_servers_for_test();
}

#[test]
fn controlled_admission_records_real_hubs_spawn_refusal() {
    let _serial = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    assert!(crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed);
    let sid = crate::plex::register_for_test("s00000001", "127.0.0.1", 9, "s00000002", "s00000003");
    assert!(crate::plex::set_current(sid));
    let mt = unsafe { crate::task::MainThread::assume() };
    let publisher = crate::plex::session::ProfilePublisher::scoped(&mt);
    let mut io = HomeIo { replay:false,preferences:Default::default(),requests:Vec::new(),admissions:Default::default(),
        failure:None,profile:publisher.snapshot() };
    assert!(crate::pms::with_refused_fetches_for_test(|| io.hubs(Some(crate::stores::hubs::HubsCmd::RefetchHubs), 0.0)).changed);
    assert_eq!(io.requests.len(), 1);
    assert_eq!(io.requests[0]["admitted"], serde_json::json!(false), "real refusal must be recorded");
    assert!(crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed);
    crate::plex::reset_servers_for_test();
}

#[test]
fn controlled_hubs_replays_refusal_retry_and_success() {
    let _serial = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let sid = crate::plex::register_for_test("s00000001", "127.0.0.1", 9, "s00000002", "s00000003");
    assert!(crate::plex::set_current(sid));
    let client = crate::plex::client_for(sid).unwrap();
    let mt = unsafe { crate::task::MainThread::assume() };
    let initial = crate::pms::initial::Initial::fresh();
    let mut transcript: Vec<Vec<serde_json::Value>> = Vec::new();
    let mut states = Vec::new();
    let mut completion = None;
    let mut parsed: Option<crate::ui::rec::Recording> = None;
    for replay in [false, true] {
        initial.restore_boot(&mt).unwrap();
        let publisher = crate::plex::session::ProfilePublisher::scoped(&mt);
        let mut io = HomeIo { replay,preferences:Default::default(),requests:Vec::new(),
            admissions:Default::default(),failure:None,profile:publisher.snapshot() };
        let mut attempts = 0;
        let sink = crate::ui::rec::MemSink::default();
        let bytes = sink.segments.clone();
        let header = crate::ui::rec::Header::new(17,&crate::ui::press::Press::new());
        let mut writer = crate::ui::rec::Writer::open(Box::new(sink),&header,0).unwrap();
        for frame in 0..160 {
            if replay {
                io.admissions = parsed.as_ref().unwrap().frames[frame].effects.iter()
                    .map(|effect| effect["payload"].clone()).collect();
            }
            let command = (frame == 0).then_some(crate::stores::hubs::HubsCmd::RefetchHubs);
            let outcome = io.hubs_with(command, 0.05, &mut |request| {
                assert!(!replay, "no live executor during replay");
                attempts += 1;
                if attempts == 1 { return false; }
                let (epoch,req,sid,instance,token_gen) = request.descriptor();
                completion = Some((frame,serde_json::json!({"kind":"hubs","version":1,
                    "gen":epoch,"seq":req,"sid":sid,"client":instance,"token_gen":token_gen,
                    "build":{"cw":[],"shelves":[]}})));
                true
            });
            if let Some((at,wire)) = &completion {
                if *at == frame {
                    let result = crate::pms::record::decode(wire.clone(), |id|
                        (id == client.instance_gen()).then_some(client)).unwrap();
                    assert!(crate::pms::land(&result).endpoints.iter().next().is_none());
                }
            }
            let state = serde_json::to_value(crate::pms::initial::Initial::capture()).unwrap();
            assert!(io.failure.is_none() && io.admissions.is_empty());
            let requests = std::mem::take(&mut io.requests);
            if replay {
                assert_eq!(requests,transcript[frame]);
                assert!(state == states[frame], "real Home retry and result state diverged at {frame}");
            } else {
                writer.tick(frame as u64,crate::ui::machine::Tick {ms:frame as u32 * 50,dt_us:50_000});
                for request in &requests { writer.effect_payload(frame as u64,"Cache","Request",request.clone()); }
                writer.flush_frame().unwrap();
                transcript.push(requests); states.push(state);
            }
            if frame == 0 { assert_eq!(outcome.endpoints.iter().count(),1); }
        }
        assert_eq!(attempts,if replay {0} else {2});
        assert!(completion.is_some(),"backoff must recover with an admitted result");
        writer.finish().unwrap();
        if !replay {
            let bytes = bytes.borrow();
            parsed = Some(crate::ui::rec::Recording::parse(&header.to_json().to_string(),
                &bytes.iter().map(Vec::as_slice).collect::<Vec<_>>(),17).unwrap());
        }
    }
    assert_eq!(transcript[0][0]["admitted"],serde_json::json!(false));
    assert_eq!(transcript.iter().flatten().filter(|r| r["admitted"] == true).count(),1);
    assert!(crate::stores::hubs::apply(crate::stores::hubs::HubsCmd::Reset).changed);
    crate::plex::reset_servers_for_test();
}

#[test]
fn admission_replay_requires_full_request_identity_and_boolean_outcome() {
    let _serial = crate::testlock::serial();
    let mt = unsafe { crate::task::MainThread::assume() };
    let publisher = crate::plex::session::ProfilePublisher::scoped(&mt);
    let request = serde_json::json!({"kind":"hubs","epoch":u32::MAX,"req":u32::MAX,
        "sid":0,"client":u32::MAX,"token_gen":u32::MAX});
    let mut recorded = request.clone(); recorded["admitted"] = serde_json::json!(false);
    validate_admission(&recorded,u32::MAX).unwrap();
    for field in ["epoch","req","sid","client","token_gen","admitted"] {
        let mut wrong = recorded.clone(); wrong[field] = serde_json::json!(u64::from(u32::MAX)+1);
        assert!(validate_admission(&wrong,u32::MAX).is_err());
        let mut io = HomeIo { replay:true,preferences:Default::default(),requests:Vec::new(),
            admissions:vec![wrong].into(),failure:None,profile:publisher.snapshot() };
        assert!(!io.admit(request.clone(),||panic!("replay must not execute resource")));
        assert!(io.failure.is_some());
    }
    let mut io = HomeIo { replay:true,preferences:Default::default(),requests:Vec::new(),
        admissions:vec![recorded.clone()].into(),failure:None,profile:publisher.snapshot() };
    assert!(!io.admit(request,||panic!("recorded refusal is not a live execution")));
    assert!(io.failure.is_none());
    assert_eq!(io.requests,vec![recorded]);
}
