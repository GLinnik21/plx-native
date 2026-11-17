//! Pure owner observations are tape data; durable preference writes are not.
use super::*;
use plx_machine::machine::{Fx, HoldHintEvent, InstanceId, MachineId, Stamped};
use plx_ui::dispatch::Tap;

fn input_hash(input: &plx_ui::input::InputMachine<u32>) -> u64 {
    let mut canon = Canon::new();
    input.write_with(&mut canon, &|elem, canon| { canon.u32(*elem); });
    canon.finish()
}
fn header(initial: &super::super::bootstrap::Initial) -> Header {
    let mut header = Header::new(state_fp(), initial);
    header.init_data = initial.to_v6_value().unwrap();
    header.clock_start_ms = initial.clock_start;
    header.features = features();
    header.triggers = initial.triggers.clone();
    header
}
fn replay(recording: Recording) -> Recplay {
    Recplay::Replaying(Replay { resolution: Default::default(), rec: recording,
        at:0, graded:0, diverged:0, present_diffs:0, result_diffs:0, land_diffs:0,
        result_at:0, effect_at:0, input_at:0, input_diffs:0, effect_diffs:0,
        started:false, failure:None })
}
fn observation(event: HoldHintEvent) -> Stamped<super::super::bridge::AppHost> {
    Stamped { from: MachineId::Instance(InstanceId(1)), fx: Fx::HoldHint(event) }
}
fn one_frame() -> (super::super::bootstrap::Initial, Recording) {
    let initial = super::super::bootstrap::Initial::synthetic_home(17, 32517, None).unwrap();
    let payload = super::super::bootstrap::effects::encode(&Fx::HoldHint(HoldHintEvent::ReadCard)).unwrap();
    let recording = Recording { header: header(&initial), frames: vec![plx_ui::rec::Frame {
        f:0, tick:Some(Tick::default()), st:Some(7), effects:vec![json!({
            "f":0,"t":"eff","from":"inst:1","e":"HoldHint","payload":payload})],
        ..Default::default() }], metrics:Default::default(), stopped_at:None };
    (initial, recording)
}

#[test]
fn owned_hold_hint_shape_refuses_predecessors_before_boot() {
    assert!(plx_ui::input::STATE_SHAPE.contains("hold_hint:HoldHintRead{learned:bool,card:bool,pill:bool,context:u32}"));
    for old in [FP_PRIOR, FP_V6, state_fp_v7(), state_fp_v10(), state_fp_v11()] {
        assert_ne!(state_fp(), old);
        let manifest = json!({"schema":plx_ui::rec::SCHEMA,"state_fp":old}).to_string();
        assert_eq!(Recording::parse(&manifest, &[], state_fp()).err(),
            Some(RecError::StateShape { theirs:old, ours:state_fp() }));
        // Invalid/missing boot data does not get decoded before the shape rejection.
        assert_eq!(super::super::bootstrap::Initial::decode_profile(Value::Null, 0, old).err(),
            Some("incompatible owner-input/presentation state; requires V13 capture"));
    }
    eprintln!("owned hold-hint V12 fingerprint={}", state_fp());
}

#[test]
fn hold_hint_preflight_rejects_structural_mutations() {
    let (initial, recording) = one_frame();
    assert_eq!(validate_controlled(&recording, &initial), Ok(()));
    for payload in [json!(null), json!("read_card"), json!({}),
        json!({"hold_hint":0}), json!({"hold_hint":"ReadCard"}),
        json!({"hold_hint":"learned","extra":false}),
        json!({"hold_hint":{"ReadCard":null}}), json!({"hold_hint":["read_card"]})] {
        let (_, mut changed) = one_frame();
        changed.frames[0].effects[0]["payload"] = payload;
        assert!(validate_controlled(&changed, &initial).is_err());
    }
    for (key, value) in [("e",json!("App")), ("from",json!("inst:01")),
        ("from",json!(true)), ("extra",json!(false)), ("f",json!(1))] {
        let (_, mut changed) = one_frame();
        changed.frames[0].effects[0][key] = value;
        assert!(validate_controlled(&changed, &initial).is_err());
    }
    let (_, mut changed) = one_frame();
    changed.frames[0].effects[0]["e"] = json!("App");
    changed.frames[0].effects[0]["payload"] = json!({"preferences":{"hold_hint":{"card":true,"pill":false}}});
    assert!(validate_controlled(&changed, &initial).is_err());
    for (event, expected) in [(HoldHintEvent::Learned,"learned"),
        (HoldHintEvent::ReadCard,"read_card"), (HoldHintEvent::ReadPill,"read_pill")] {
        let payload = super::super::bootstrap::effects::encode(&Fx::HoldHint(event)).unwrap();
        assert_eq!(payload, json!({"hold_hint":expected}));
        assert_eq!(super::super::bootstrap::effects::validate_hold_hint(&payload), Ok(()));
    }
}

#[test]
fn hold_hint_two_owners_replay_identical_prefix() {
    let _serial = plx_base::testlock::serial();
    let initial = super::super::bootstrap::Initial::synthetic_home(17, 32517, None).unwrap();
    let sink_a = plx_ui::rec::MemSink::default(); let a = sink_a.segments.clone();
    let sink_b = plx_ui::rec::MemSink::default(); let b = sink_b.segments.clone();
    let mut record_a = Recplay::recording_with_sink(&initial, Box::new(sink_a)).unwrap();
    let mut record_b = Recplay::recording_with_sink(&initial, Box::new(sink_b)).unwrap();
    let mut input_a = plx_ui::input::InputMachine::<u32>::new();
    let mut input_b = plx_ui::input::InputMachine::<u32>::new();
    let gate_a = plx_machine::landgate::Gate::default();
    let gate_b = plx_machine::landgate::Gate::default();
    let events = [HoldHintEvent::ReadCard, HoldHintEvent::ReadPill, HoldHintEvent::Learned];
    for (index, event) in events.into_iter().enumerate() {
        let previous_b = input_hash(&input_b);
        input_a.hold_hint.apply(event);
        assert_eq!(input_hash(&input_b), previous_b, "another owner cannot learn or mark shown");
        input_b.hold_hint.apply(event);
        assert_eq!(input_hash(&input_a), input_hash(&input_b), "every identical prefix");
        for (rec, input, gate) in [(&mut record_a,&input_a,&gate_a),(&mut record_b,&input_b,&gate_b)] {
            rec.tick(index as u32 * 16, if index == 0 { 0.0 } else { 0.016 });
            rec.capture_readiness(Readiness::default());
            Tap::focus(rec, index as u64, None);
            Tap::effect(rec, index as u64, &observation(event));
            rec.present(false);
            rec.end_frame_with_gate(&|| input_hash(input), &|_| 0, gate);
        }
    }
    assert!(!record_a.finish(&gate_a)); assert!(!record_b.finish(&gate_b));
    assert_eq!(*a.borrow(), *b.borrow(), "independent owners write identical tape prefixes");
    let bytes = a.borrow().concat();
    let manifest = header(&initial).to_json().to_string();
    for changed in [false, true] {
        let recording = Recording::parse(&manifest, &[&bytes], state_fp()).unwrap();
        validate_controlled(&recording, &initial).unwrap();
        let mut replay = replay(recording);
        let mut input = plx_ui::input::InputMachine::<u32>::new();
        for (index, event) in events.into_iter().enumerate() {
            let event = if changed && index == 0 { HoldHintEvent::ReadPill } else { event };
            input.hold_hint.apply(event);
            replay.capture_readiness(Readiness::default());
            Tap::focus(&mut replay, index as u64, None);
            Tap::effect(&mut replay, index as u64, &observation(event));
            replay.present(false);
            replay.end_frame_with_gate(&|| input_hash(&input), &|_| 0, &gate_a);
        }
        let Recplay::Replaying(result) = replay else { unreachable!() };
        assert_eq!(result.same(), !changed, "same final learned state cannot excuse a changed prefix");
    }
}

#[test]
fn hold_hint_persistence_is_unsupported_and_stops_writer() {
    use plx_screens::registry::{AppFx, PreferenceCmd};
    let initial = super::super::bootstrap::Initial::synthetic_home(17, 32517, None).unwrap();
    let (reply, _receive) = std::sync::mpsc::channel();
    let fx = Fx::App(AppFx::Preferences(PreferenceCmd::HoldHint {
        taught:plx_plex::plex::session::HoldHintTaught { card:true, pill:false },
        live:std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)), reply,
    }));
    assert_eq!(super::super::bootstrap::effects::encode(&fx), Err("unsupported controlled preferences effect"));
    let mut rec = Recplay::recording_with_sink(&initial, Box::new(plx_ui::rec::MemSink::default())).unwrap();
    Tap::effect(&mut rec, 0, &Stamped { from:MachineId::Input, fx });
    assert_eq!(rec.failure(), Some("unsupported controlled preferences effect"));
    let Recplay::Recording(recorded) = rec else { unreachable!() };
    assert!(recorded.w.stopped());
}
