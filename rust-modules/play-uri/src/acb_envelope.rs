//! The ACB "video-data" envelope, derived from the LS2 service's own `videoInfo` event.
//!
//! **Why.** On the webOS 4.5 set the LS2 service (`com.webos.media`) never sends the ACB schema
//! (`sourceInfo` with a string `context` and a decodable `video`); it sends `videoInfo` with the
//! same facts spelt for LS2. The firmware makes the ACB spelling itself from those facts: the
//! type-4 `videoInfo` callback Feed binds with is built by libplayerAPIs from the same members
//! that `sendVideoInfoEvent` reports over LS2. The field map, with the firmware addresses and the
//! measured message shapes, is `docs/measurements/uri-acb-envelope-2026-11-05.md`.
//!
//! **Shape.** Pure: [`Ls2VideoInfo::decode`] validates one `videoInfo` object and keeps the
//! members the firmware re-spells; [`Ls2VideoInfo::envelope`] writes the envelope once the caller
//! knows the one fact that is not in the event, whether the Load it sent was adaptive. It fails
//! closed: a member the firmware always writes that the service did not report, or reported in a
//! shape the map does not cover, gives no envelope (the bind keeps waiting for one and times out
//! exactly as it does when the service sends nothing). Nothing is made up but the firmware's own
//! constants (`content`, `path`).
use crate::uri_protocol::{MediaId, VideoFacts};
use serde_json::{json, Map, Value};

/// The members of one validated LS2 `videoInfo` event, ready to be written as the ACB envelope.
#[derive(Clone, PartialEq)]
pub(crate) struct Ls2VideoInfo {
    /// The event's own (nested) `mediaId`: the envelope's `context`.
    pub(crate) media_id: MediaId,
    /// The same facts the bind sequence waits for.
    pub(crate) video: VideoFacts,
    /// The envelope's `video` members in the firmware's order. `adaptive` holds a placeholder:
    /// it depends on the Load, not on this event.
    members: Vec<(&'static str, Value)>,
}

impl std::fmt::Debug for Ls2VideoInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ls2VideoInfo(<opaque>)")
    }
}

impl Ls2VideoInfo {
    /// Validate the `videoInfo` object of an event. `None` for anything the firmware's conversion
    /// would not reproduce: a missing or mistyped member that it always writes, an LS2 3D string
    /// that is not in its table, a `pixelAspectRatio` that is not `W:H`, non-positive dimensions
    /// or frame rate, or an event that carries no `mediaId` of its own.
    pub(crate) fn decode(info: &Value) -> Option<Self> {
        let media_id = MediaId::from(info.get("mediaId")?.as_str()?);
        let video = VideoFacts::decode(info)?;
        let number = |key: &str| info.get(key).filter(|value| value.is_number()).cloned();
        let text = |key: &str| info.get(key).and_then(Value::as_str);
        // The service's own numbers go through verbatim (`24` stays `24`, a `0` stays `0`).
        let frame_rate = number("frameRate")?;
        let scan_type = if text("scanType")? == "progressive" { "VIDEO_PROGRESSIVE" } else { "VIDEO_INTERLACED" };
        let (width, height) = pixel_aspect_ratio(text("pixelAspectRatio")?)?;
        let (pattern, type_lr) = pattern_3d(text("mode3D")?)?;
        let mut members = vec![
            ("frameRate", frame_rate),
            ("scanType", json!(scan_type)),
            ("width", json!(video.width)),
            ("height", json!(video.height)),
            ("bitRate", number("bitRate")?),
            ("adaptive", Value::Null),
            ("path", json!("network")),
            ("pixelAspectRatio", json!({ "width": width, "height": height })),
            ("afd", number("afd")?),
            ("rotation", number("rotation")?),
            ("data3D", json!({ "originalPattern": pattern, "currentPattern": pattern, "typeLR": type_lr })),
        ];
        // The firmware writes `hfr` only for the first envelope of an instance, which is the only
        // one this sends; a service that reports none gets none.
        match info.get("hfr") {
            None => {}
            Some(Value::Bool(hfr)) => members.push(("hfr", json!(hfr))),
            Some(_) => return None,
        }
        members.push(("hdrType", json!(text("hdrType")?)));
        // SEI and VUI are written by the same builder under another key, and only when non-empty;
        // the service sends the keys it was given (`videoFullRangeFlag` only when it was).
        for (from, to) in [("SEI", "mediaSei"), ("VUI", "mediaVui")] {
            match info.get(from) {
                None | Some(Value::Null) => {}
                Some(Value::String(empty)) if empty.is_empty() => {}
                Some(Value::Object(object)) if object.is_empty() => {}
                Some(object @ Value::Object(_)) => members.push((to, object.clone())),
                Some(_) => return None,
            }
        }
        Some(Self { media_id, video, members })
    }

    /// The envelope text. `adaptive` is the firmware's rule applied to the Load that was sent
    /// (see [`crate::uri_protocol::AbrBps::is_adaptive`]).
    pub(crate) fn envelope(&self, adaptive: bool) -> String {
        let video: Map<String, Value> = self
            .members
            .iter()
            .map(|(key, value)| ((*key).to_owned(), if *key == "adaptive" { Value::Bool(adaptive) } else { value.clone() }))
            .collect();
        json!({ "context": self.media_id, "content": "movie", "video": Value::Object(video) }).to_string()
    }
}

/// `"W:H"` as the firmware's `"%d:%d"` writes it: two runs of digits and nothing else.
fn pixel_aspect_ratio(text: &str) -> Option<(u32, u32)> {
    let digits = |part: &str| (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())).then(|| part.parse::<u32>().ok()).flatten();
    let (width, height) = text.split_once(':')?;
    Some((digits(width)?, digits(height)?))
}

/// The inverse of the firmware's two 3D tables: the LS2 spelling (`find3DUMSInfoString`) to the
/// envelope's pattern (`find3DInfoString`, which has no `_LR`/`_RL` suffix and one name for
/// `top_bottom`) and its `typeLR`. Exactly the table, so a string the table does not hold (the
/// empty entries for modes 10 and 14 included) is `None` rather than a guess.
fn pattern_3d(ls2: &str) -> Option<(&'static str, &'static str)> {
    Some(match ls2 {
        "2d" => ("2d", "LR"),
        "side_side_half_LR" => ("side_side_half", "LR"),
        "side_side_half_RL" => ("side_side_half", "RL"),
        "top_bottom" => ("top_bottom", "LR"),
        "bottom_top" => ("top_bottom", "RL"),
        "check_board" => ("check_board", "LR"),
        // The firmware's own spelling of both frame-sequential modes.
        "frame_squential" => ("frame_squential", "LR"),
        "column_interleave" => ("column_interleave", "LR"),
        "side_side_full_LR" => ("side_side_full", "LR"),
        "side_side_full_RL" => ("side_side_full", "RL"),
        "line_interleave_half" => ("line_interleave_half", "LR"),
        "dual_stream" => ("dual_stream", "LR"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `videoInfo` object the 4.5 set sent for the measured 720p24 H.264 clip (2026-11-05).
    const CAPTURED: &str = r#"{"width":1280,"height":720,"aspectRatio":"16:9","frameRate":24,"bitRate":120,"mode3D":"2d","actual3D":"2d","scanType":"progressive","pixelAspectRatio":"1:1","afd":16,"rotation":0,"userData":"userData=(null)","specificRendering":"none","hfr":false,"hdrType":"none","SEI":{"displayPrimariesX0":0,"displayPrimariesX1":0,"displayPrimariesX2":0,"displayPrimariesY0":0,"displayPrimariesY1":0,"displayPrimariesY2":0,"whitePointX":0,"whitePointY":0,"minDisplayMasteringLuminance":0,"maxDisplayMasteringLuminance":0,"maxContentLightLevel":0,"maxPicAverageLightLevel":0},"VUI":{"transferCharacteristics":2,"colorPrimaries":2,"matrixCoeffs":2},"mediaId":"_M7yz1mOAwtVmU7"}"#;

    /// The captured `videoInfo` with `edit` applied to its object.
    fn edited(edit: impl FnOnce(&mut Map<String, Value>)) -> Value {
        let mut info: Value = serde_json::from_str(CAPTURED).unwrap();
        edit(info.as_object_mut().unwrap());
        info
    }

    fn envelope_of(info: &Value, adaptive: bool) -> Option<Value> {
        Ls2VideoInfo::decode(info).map(|info| serde_json::from_str(&info.envelope(adaptive)).unwrap())
    }

    #[test]
    fn the_captured_video_info_becomes_the_firmwares_envelope() {
        let info: Value = serde_json::from_str(CAPTURED).unwrap();
        assert_eq!(envelope_of(&info, false).unwrap(), json!({"context":"_M7yz1mOAwtVmU7","content":"movie","video":{
            "frameRate":24,"scanType":"VIDEO_PROGRESSIVE","width":1280,"height":720,"bitRate":120,
            "adaptive":false,"path":"network","pixelAspectRatio":{"width":1,"height":1},"afd":16,
            "rotation":0,"data3D":{"originalPattern":"2d","currentPattern":"2d","typeLR":"LR"},
            "hfr":false,"hdrType":"none",
            "mediaSei":{"displayPrimariesX0":0,"displayPrimariesX1":0,"displayPrimariesX2":0,"displayPrimariesY0":0,"displayPrimariesY1":0,"displayPrimariesY2":0,"whitePointX":0,"whitePointY":0,"minDisplayMasteringLuminance":0,"maxDisplayMasteringLuminance":0,"maxContentLightLevel":0,"maxPicAverageLightLevel":0},
            "mediaVui":{"transferCharacteristics":2,"colorPrimaries":2,"matrixCoeffs":2}}}));
        // The members the firmware does not write never appear.
        let text = Ls2VideoInfo::decode(&info).unwrap().envelope(false);
        for absent in ["specificRendering", "userData", "aspectRatio", "codec", "actual3D", "mode3D"] {
            assert!(!text.contains(absent), "{absent} in {text}");
        }
        assert!(text.contains("\"rotation\":0") && !text.contains("\"rotation\":\"0\""), "rotation is a number: {text}");
    }

    #[test]
    fn the_adaptive_member_follows_the_load() {
        let info: Value = serde_json::from_str(CAPTURED).unwrap();
        assert_eq!(envelope_of(&info, true).unwrap()["video"]["adaptive"], true);
        assert_eq!(envelope_of(&info, false).unwrap()["video"]["adaptive"], false);
    }

    #[test]
    fn members_are_written_in_the_firmwares_order() {
        let info = Ls2VideoInfo::decode(&serde_json::from_str(CAPTURED).unwrap()).unwrap();
        let keys: Vec<&str> = info.members.iter().map(|(key, _)| *key).collect();
        assert_eq!(keys, ["frameRate", "scanType", "width", "height", "bitRate", "adaptive", "path",
            "pixelAspectRatio", "afd", "rotation", "data3D", "hfr", "hdrType", "mediaSei", "mediaVui"]);
    }

    #[test]
    fn scan_type_is_progressive_or_interlaced() {
        for (ls2, firmware) in [("progressive", "VIDEO_PROGRESSIVE"), ("interlaced", "VIDEO_INTERLACED"), ("unknown", "VIDEO_INTERLACED")] {
            let info = edited(|o| { o.insert("scanType".into(), json!(ls2)); });
            assert_eq!(envelope_of(&info, false).unwrap()["video"]["scanType"], firmware, "{ls2}");
        }
    }

    #[test]
    fn pixel_aspect_ratio_splits_and_malformed_ones_give_no_envelope() {
        let info = edited(|o| { o.insert("pixelAspectRatio".into(), json!("4:3")); });
        assert_eq!(envelope_of(&info, false).unwrap()["video"]["pixelAspectRatio"], json!({"width":4,"height":3}));
        for bad in ["", "1", "1:", ":1", "1:1:1", "a:b", "1:-1", "+1:1", "1.5:1", "16/9", " 1:1"] {
            let info = edited(|o| { o.insert("pixelAspectRatio".into(), json!(bad)); });
            assert!(Ls2VideoInfo::decode(&info).is_none(), "{bad:?}");
        }
        let info = edited(|o| { o.insert("pixelAspectRatio".into(), json!({"width":1,"height":1})); });
        assert!(Ls2VideoInfo::decode(&info).is_none());
    }

    #[test]
    fn the_3d_table_inverts_with_the_type_lr_rule() {
        for (ls2, pattern, type_lr) in [
            ("2d", "2d", "LR"),
            ("side_side_half_LR", "side_side_half", "LR"),
            ("side_side_half_RL", "side_side_half", "RL"),
            ("top_bottom", "top_bottom", "LR"),
            ("bottom_top", "top_bottom", "RL"),
            ("check_board", "check_board", "LR"),
            ("frame_squential", "frame_squential", "LR"),
            ("column_interleave", "column_interleave", "LR"),
            ("side_side_full_LR", "side_side_full", "LR"),
            ("side_side_full_RL", "side_side_full", "RL"),
            ("line_interleave_half", "line_interleave_half", "LR"),
            ("dual_stream", "dual_stream", "LR"),
        ] {
            let info = edited(|o| { o.insert("mode3D".into(), json!(ls2)); });
            assert_eq!(envelope_of(&info, false).unwrap()["video"]["data3D"],
                json!({"originalPattern":pattern,"currentPattern":pattern,"typeLR":type_lr}), "{ls2}");
        }
    }

    #[test]
    fn a_3d_string_outside_the_table_gives_no_envelope() {
        // The empty slots of modes 10 and 14, the suffix-less half-width, a doubled suffix and
        // the corrected spelling of frame-sequential are all outside the firmware's table.
        for ls2 in ["", "side_side_half", "top_bottom_LR", "bottom_top_RL", "frame_sequential", "side_side_full", "3d", "2D"] {
            let info = edited(|o| { o.insert("mode3D".into(), json!(ls2)); });
            assert!(Ls2VideoInfo::decode(&info).is_none(), "{ls2:?}");
        }
        let info = edited(|o| { o.insert("mode3D".into(), json!(2)); });
        assert!(Ls2VideoInfo::decode(&info).is_none());
    }

    #[test]
    fn hlg_vui_passes_as_given_and_the_hdr_type_is_verbatim() {
        let info = edited(|o| {
            o.insert("hdrType".into(), json!("HLG"));
            o.insert("VUI".into(), json!({"transferCharacteristics":18,"colorPrimaries":9,"matrixCoeffs":9,"videoFullRangeFlag":false}));
        });
        let envelope = envelope_of(&info, false).unwrap();
        assert_eq!(envelope["video"]["hdrType"], "HLG");
        assert_eq!(envelope["video"]["mediaVui"], json!({"transferCharacteristics":18,"colorPrimaries":9,"matrixCoeffs":9,"videoFullRangeFlag":false}));
        // A service that did not send the flag does not get one.
        assert!(envelope_of(&serde_json::from_str(CAPTURED).unwrap(), false).unwrap()["video"]["mediaVui"].get("videoFullRangeFlag").is_none());
    }

    #[test]
    fn empty_sei_and_vui_and_an_absent_hfr_are_left_out() {
        for empty in [json!({}), json!(null), json!("")] {
            let info = edited(|o| {
                o.insert("SEI".into(), empty.clone());
                o.insert("VUI".into(), empty.clone());
                o.remove("hfr");
            });
            let envelope = envelope_of(&info, false).unwrap();
            for key in ["mediaSei", "mediaVui", "hfr"] {
                assert!(envelope["video"].get(key).is_none(), "{key} for {empty}");
            }
        }
        let info = edited(|o| { o.remove("SEI"); o.remove("VUI"); });
        assert!(envelope_of(&info, false).unwrap()["video"].get("mediaSei").is_none());
        // A non-empty one of another shape is not the service's SEI.
        let info = edited(|o| { o.insert("SEI".into(), json!("x")); });
        assert!(Ls2VideoInfo::decode(&info).is_none());
        let info = edited(|o| { o.insert("hfr".into(), json!(1)); });
        assert!(Ls2VideoInfo::decode(&info).is_none());
    }

    #[test]
    fn the_service_numbers_are_copied_not_rewritten() {
        let info = edited(|o| {
            o.insert("frameRate".into(), json!(23.976));
            o.insert("bitRate".into(), json!(0));
            o.insert("afd".into(), json!(8));
            o.insert("rotation".into(), json!(90));
        });
        let envelope = envelope_of(&info, false).unwrap();
        assert_eq!(envelope["video"]["frameRate"], json!(23.976));
        assert_eq!(envelope["video"]["bitRate"], json!(0));
        assert_eq!(envelope["video"]["afd"], json!(8));
        assert_eq!(envelope["video"]["rotation"], json!(90));
        // A whole frame rate keeps its integer spelling.
        let text = Ls2VideoInfo::decode(&serde_json::from_str(CAPTURED).unwrap()).unwrap().envelope(false);
        assert!(text.contains("\"frameRate\":24,"), "{text}");
    }

    #[test]
    fn a_missing_or_mistyped_member_the_firmware_always_writes_gives_no_envelope() {
        for key in ["mediaId", "width", "height", "frameRate", "scanType", "bitRate", "pixelAspectRatio", "afd", "rotation", "mode3D", "hdrType"] {
            assert!(Ls2VideoInfo::decode(&edited(|o| { o.remove(key); })).is_none(), "missing {key}");
        }
        for (key, wrong) in [("mediaId", json!(7)), ("width", json!("1280")), ("height", json!(0)), ("width", json!(0)),
            ("frameRate", json!(-24)), ("frameRate", json!("24")), ("frameRate", json!({"num":24,"den":1})),
            ("scanType", json!(1)), ("bitRate", json!("120")), ("afd", json!("16")), ("rotation", json!("0")), ("hdrType", json!(0))] {
            assert!(Ls2VideoInfo::decode(&edited(|o| { o.insert(key.into(), wrong.clone()); })).is_none(), "{key}={wrong}");
        }
        assert!(Ls2VideoInfo::decode(&json!("videoInfo")).is_none());
    }

    #[test]
    fn debug_never_shows_the_identity_or_the_facts() {
        let info = Ls2VideoInfo::decode(&serde_json::from_str(CAPTURED).unwrap()).unwrap();
        let shown = format!("{info:?}");
        assert!(!shown.contains("M7yz") && !shown.contains("1280"), "{shown}");
    }
}
