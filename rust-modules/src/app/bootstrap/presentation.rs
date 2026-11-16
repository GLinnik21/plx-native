//! Captured visible paint is pure external evidence, never playback/resource permission.
use plx_machine::machine::{EntryId, InstanceId};
use plx_screens::registry::AppMsg;
use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) const SHAPE: &str = "PurePlayerPresentationInputV1{paint_frame:u64,entry:u32,instance:u32,message:PlayerPresentedV1{watch:Option<OfferId>,paired:bool}};positive_final_swap;next_frame_ingress;canonical_latest_source:Option<(u64,EntryId,InstanceId)>;queued_owner_sealed_Presented;carry_until_superseded_or_retired;bounded_one;no_resource_permission";

pub(crate) fn encode(frame: u64, entry: EntryId, instance: InstanceId, message: &AppMsg)
    -> Result<Value, &'static str> {
    if !matches!(message, AppMsg::PlayerPresented { .. }) {
        return Err("unsupported presentation observation");
    }
    Ok(json!({"kind":"player_presentation_v1","paint_frame":frame,"entry":entry.0,
        "instance":instance.0,"message":super::effects::message(message)?}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Watch {
    scope: u64,
    entry: u32,
    playback: u32,
    marker: plx_data::metadata::Marker,
}

pub(crate) fn decode(value: &Value) -> Result<(u64, EntryId, InstanceId, AppMsg), &'static str> {
    let invalid = "invalid player presentation observation";
    let u32_at = |key: &str| value[key].as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n != 0).ok_or(invalid);
    let frame = value["paint_frame"].as_u64().filter(|frame| *frame != 0 && frame.checked_add(1).is_some()).ok_or(invalid)?;
    let entry = EntryId(u32_at("entry")?);
    let instance = InstanceId(u32_at("instance")?);
    let body = &value["message"]["player_presented"];
    if body["codec"] != 1 { return Err(invalid); }
    let paired = body["paired"].as_bool().ok_or(invalid)?;
    let watch: Option<Watch> = serde_json::from_value(body["watch"].clone()).map_err(|_| invalid)?;
    let watch = watch.map(|id| plx_appkit::skip_policy::OfferId {
        scope: id.scope, entry: id.entry, playback: id.playback, marker: id.marker,
    });
    if watch.is_some_and(|id| id.scope == 0 || id.entry != entry.0
        || id.marker.start_ms < 0 || id.marker.end_ms <= id.marker.start_ms
        || (paired && (id.marker.kind != plx_data::metadata::MarkerKind::Credits || !id.marker.final_seg))) {
        return Err(invalid);
    }
    let message = AppMsg::PlayerPresented { watch, paired };
    let mut canonical = value.clone();
    if let Some(object) = canonical.as_object_mut() { object.remove("f"); object.remove("t"); }
    if encode(frame, entry, instance, &message)? != canonical { return Err(invalid); }
    Ok((frame, entry, instance, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value() -> Value {
        encode(7, EntryId(42), InstanceId(3), &AppMsg::PlayerPresented {
            paired: false, watch: Some(plx_appkit::skip_policy::OfferId {
                scope: 9, entry: 42, playback: 2,
                marker: plx_data::metadata::Marker { kind: plx_data::metadata::MarkerKind::Intro,
                    start_ms: 1_000, end_ms: 60_000, final_seg: false },
            }),
        }).unwrap()
    }

    #[test]
    fn presentation_codec_is_exact_and_has_no_resource_permission() {
        let original = value();
        let (frame, entry, instance, message) = decode(&original).unwrap();
        assert_eq!(encode(frame, entry, instance, &message).unwrap(), original);
        for (key, changed) in [("kind", json!("player_presentation")), ("paint_frame", json!(-1)),
            ("entry", json!(0)), ("instance", json!(0)), ("extra", json!(false)),
            ("message", json!({"player_transport_reset":false}))] {
            let mut value = original.clone(); value[key] = changed;
            assert!(decode(&value).is_err(), "{key}");
        }
        for (key, changed) in [("codec", json!(0)), ("paired", json!(true)),
            ("watch", json!({})), ("resource", json!(true))] {
            let mut value = original.clone(); value["message"]["player_presented"][key] = changed;
            assert!(decode(&value).is_err(), "{key}");
        }
        let mut value = original;
        value["message"]["player_presented"]["watch"]["marker"]["extra"] = json!(false);
        assert!(decode(&value).is_err());
        assert!(encode(7, EntryId(42), InstanceId(3), &AppMsg::PlayerTransportReset { for_reload:false }).is_err());
    }

    #[test]
    fn presentation_preflight_refuses_old_codec_duplicate_and_changed_frame_schedule() {
        use crate::app::{bootstrap::Initial, recorder};
        use plx_ui::rec::{Frame, Header, Recording};
        let initial = Initial::synthetic_home(17, 32517, None).unwrap();
        let mut header = Header::new(recorder::state_fp(), &initial);
        header.init_data = initial.to_v6_value().unwrap();
        header.features = recorder::features(); header.triggers = initial.triggers.clone();
        let mut first = value(); first["paint_frame"] = json!(7);
        let mut second = value(); second["paint_frame"] = json!(8);
        let recording = Recording { header, frames:vec![
            Frame { f:0, tick:Some(plx_machine::machine::Tick { ms:initial.clock_start, dt_us:0 }), st:Some(7),
                focus:Some(None), present:Some(false), readiness:Some(Default::default()), ..Default::default() },
            Frame { f:1, tick:Some(plx_machine::machine::Tick { ms:initial.clock_start + 16, dt_us:16_000 }),
                st:Some(7), focus:Some(None), present:Some(false), readiness:Some(Default::default()), inputs:vec![first], ..Default::default() },
            Frame { f:2, tick:Some(plx_machine::machine::Tick { ms:initial.clock_start + 32, dt_us:16_000 }),
                st:Some(7), focus:Some(None), present:Some(false), readiness:Some(Default::default()), inputs:vec![second], ..Default::default() },
        ], metrics:Default::default(), stopped_at:None };
        assert_eq!(recorder::validate_controlled(&recording, &initial), Ok(()));
        for mutation in 0..5 {
            let mut changed = Recording { header:recording.header.clone(), frames:recording.frames.clone(),
                metrics:recording.metrics.clone(), stopped_at:recording.stopped_at };
            match mutation {
                0 => { let duplicate = changed.frames[1].inputs[0].clone(); changed.frames[1].inputs.push(duplicate); }
                1 => changed.frames[2].inputs[0]["paint_frame"] = json!(7),
                2 => changed.frames[2].inputs[0]["paint_frame"] = json!(9),
                3 => changed.frames[1].inputs[0]["message"]["player_presented"]["codec"] = json!(0),
                _ => changed.frames[1].inputs[0]["message"]["player_presented"]["resource"] = json!(true),
            }
            assert!(recorder::validate_controlled(&changed, &initial).is_err(), "mutation {mutation}");
        }
        // Grade the actual typed observation prefix, not just a converged final state.
        for mutation in 0..4 {
            let expected = Recording { header:recording.header.clone(), frames:recording.frames.clone(),
                metrics:recording.metrics.clone(), stopped_at:recording.stopped_at };
            let mut replay = recorder::Recplay::controlled(crate::app::bootstrap::Preflight::Replay {
                initial:initial.clone(), recording:expected, mode:recorder::ReplayMode::Targets }, &initial).unwrap();
            for index in 0..3 {
                replay.capture_readiness(Default::default());
                plx_ui::dispatch::Tap::focus(&mut replay, index, None);
                let mut inputs = recording.frames[index as usize].inputs.clone();
                if index == 1 {
                    match mutation {
                        1 => inputs[0]["message"]["player_presented"]["watch"]["scope"] = json!(10),
                        2 => inputs.clear(),
                        3 => inputs.push(inputs[0].clone()),
                        _ => {},
                    }
                }
                for input in inputs {
                    let (frame, entry, instance, message) = decode(&input).unwrap();
                    plx_ui::dispatch::Tap::presentation(&mut replay, index, frame, entry, instance, &message);
                }
                replay.present(false); replay.end_frame(&|| 7);
            }
            assert_eq!(replay.outcome_failed(), mutation != 0, "typed prefix mutation {mutation}");
        }
        let old = 0x3ed9_2e0f_25bd_7f8e;
        assert_ne!(recorder::state_fp(), old);
        assert_eq!(Initial::decode_profile(Value::Null, 0, old).err(),
            Some("incompatible owner-input/presentation state; requires V13 capture"));
    }
}

