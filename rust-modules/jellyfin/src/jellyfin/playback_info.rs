//! The server's own playback decision: `POST /Items/{id}/PlaybackInfo` with a device profile.
//!
//! The player decides direct play from its device profile and asks the server for a verdict
//! before playing (`Backend::mde_decision`). Asking Jellyfin with a profile built from the same
//! capability table (`plx_platform::devcaps`) lets the server apply what only it knows: the
//! bitrate limits set on the Dashboard for this user, and which subtitles must be burned into the
//! picture because the profile does not list them. Its answer is a verdict (direct play, or not)
//! and, when not, the `TranscodingUrl` it would serve: the progressive Matroska stream this
//! client already plays (the profile's only transcoding target), never Jellyfin's HLS.
//!
//! Taken from gnacho/butaca (GPL-3.0-or-later), which asks the server the same way.

use serde::Deserialize;
use serde_json::{json, Value};

/// The profile's own bitrate ceiling: high enough never to be the limit on a home network, so the
/// limit that applies is the server's (the user's remote limit on the Dashboard).
const PROFILE_MAX_BPS: i64 = 200_000_000;

/// Containers the demuxer plays as they are.
const DIRECT_CONTAINERS: &str = "mkv,mp4,m4v,mov,ts,m2ts,mpegts";

/// Text subtitle formats the player draws itself, embedded or as a sidecar. Image formats (PGS,
/// VobSub, DVB) are left out on purpose, so the server burns them in.
const TEXT_SUBTITLES: [&str; 7] = ["srt", "subrip", "ass", "ssa", "vtt", "webvtt", "mov_text"];

/// What the device decodes, in the server's codec names: the same lists the player's own
/// direct-play gate reads (`plx_plex::plex::direct_play_video_codecs`, `devcaps::Caps::audio`).
pub struct Device<'a> {
    /// Video codecs the device direct-plays, h264 first (`direct_play_video_codecs`). The
    /// transcoding profile lists them too, h264 at the head as the encode target and the rest as
    /// codecs the server may copy.
    pub video: Vec<&'a str>,
    /// The decoder's frame bound (every codec at once, as the direct-play gate applies it).
    pub max_w: u32,
    pub max_h: u32,
    /// A codec's own `(width, height)` row from the device table (vp9, av1), 0 on an axis the
    /// table did not state. Each tightens the shared bound for that codec only where it is
    /// strictly tighter, as the Plex profile's codec-scoped limitations do.
    pub rows: Vec<(&'a str, u32, u32)>,
    /// The deepest VP9 bit depth the device is known to decode (`Caps::vp9_bit_depth`).
    pub vp9_bit_depth: u32,
    /// Audio codecs the device decodes, comma-separated.
    pub audio: &'a str,
    /// Audio codecs the app converts to AC3 itself ("Convert audio: on the TV"), in the server's
    /// names, or empty while that is off. Offered for direct play only: the server never has to
    /// encode to them.
    pub converted_audio: &'a str,
    /// The audio codec the server should encode to.
    pub audio_target: &'a str,
}

/// What "Convert audio: on the TV" converts (`devcaps::TV_CONVERTED_AUDIO_CODECS`), as Jellyfin
/// names it: it spells PCM by sample layout, and its direct-play match is by exact name.
pub const CONVERTED_AUDIO: &str = "truehd,flac,alac,opus,vorbis,pcm_s16le,pcm_s16be,pcm_s24le,pcm_s24be,pcm_s32le,pcm_s32be,pcm_f32le,pcm_bluray,pcm_dvd";

/// The audio codecs a server may be asked to encode to.
const TRANSCODE_AUDIO: [&str; 4] = ["aac", "ac3", "eac3", "dts"];

/// The `DeviceProfile` for [`Device`].
pub fn device_profile(d: &Device<'_>) -> Value {
    // Encode to H.264 (every Jellyfin server has the encoder), copy the rest where it already fits.
    let video = d.video.join(",");
    // The server encodes to the device's usual codecs only, never to the ones the TV merely decodes
    // (FLAC, Opus, Vorbis): a transcode's audio has to come out as something every route can feed.
    let audio_targets: Vec<&str> = std::iter::once(d.audio_target)
        .chain(d.audio.split(',').filter(|a| *a != d.audio_target && TRANSCODE_AUDIO.contains(a)))
        .collect();
    // What the TV decodes, then what the app converts for it, each codec once.
    let mut direct_audio = d.audio.to_string();
    for codec in d.converted_audio.split(',').filter(|c| !c.is_empty() && !d.audio.split(',').any(|a| a == *c)) {
        direct_audio.push(',');
        direct_audio.push_str(codec);
    }
    let mut subtitles: Vec<Value> = Vec::new();
    for f in TEXT_SUBTITLES {
        subtitles.push(json!({"Format": f, "Method": "Embed"}));
        subtitles.push(json!({"Format": f, "Method": "External"}));
    }
    json!({
        "Name": "FinNative",
        "MaxStreamingBitrate": PROFILE_MAX_BPS,
        "MaxStaticBitrate": PROFILE_MAX_BPS,
        "DirectPlayProfiles": [
            {"Type": "Video", "Container": DIRECT_CONTAINERS, "VideoCodec": video, "AudioCodec": direct_audio}
        ],
        "TranscodingProfiles": [
            {"Type": "Video", "Container": "mkv", "Protocol": "http", "Context": "Streaming",
             "VideoCodec": video, "AudioCodec": audio_targets.join(","), "CopyTimestamps": false,
             "EnableSubtitlesInManifest": false}
        ],
        "CodecProfiles": codec_profiles(d),
        "SubtitleProfiles": subtitles,
        "ContainerProfiles": [],
        "ResponseProfiles": []
    })
}

/// The frame bound for every codec, then a codec-scoped profile for each listed codec whose row is
/// strictly tighter (never roomier than the shared bound), and VP9's bit depth where it is below
/// 10, the depth the shared profile otherwise leaves open.
fn codec_profiles(d: &Device<'_>) -> Vec<Value> {
    let le = |property: &str, value: u32| {
        json!({"Condition": "LessThanEqual", "Property": property, "Value": value.to_string(), "IsRequired": false})
    };
    let mut out = vec![json!({"Type": "Video", "Conditions": [le("Width", d.max_w), le("Height", d.max_h)]})];
    for &(codec, w, h) in &d.rows {
        if !d.video.contains(&codec) {
            continue;
        }
        let bound = |row: u32, shared: u32| if row > 0 { row.min(shared) } else { shared };
        let mut conditions = Vec::new();
        if bound(w, d.max_w) < d.max_w {
            conditions.push(le("Width", w));
        }
        if bound(h, d.max_h) < d.max_h {
            conditions.push(le("Height", h));
        }
        if codec == "vp9" && d.vp9_bit_depth < 10 {
            conditions.push(le("VideoBitDepth", d.vp9_bit_depth));
        }
        if !conditions.is_empty() {
            out.push(json!({"Type": "Video", "Codec": codec, "Conditions": conditions}));
        }
    }
    out
}

/// The part of `PlaybackInfoResponse` this client reads.
#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "PascalCase", default)]
pub struct Response {
    pub media_sources: Vec<Source>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "PascalCase", default)]
pub struct Source {
    pub id: String,
    pub supports_direct_play: bool,
    pub transcoding_url: Option<String>,
}

/// The server's verdict for one item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub direct_play: bool,
    /// The server's progressive stream, relative to its base (`/videos/{id}/stream.mkv?...`).
    pub url: Option<String>,
    /// The server will re-encode the picture: a codec, size or bitrate it will not send as it is,
    /// or a subtitle it has to burn in.
    pub video_encode: bool,
    /// The server will re-encode the sound.
    pub audio_encode: bool,
    /// What the user's policy lets the server encode at all.
    pub encoders: Encoders,
    /// The server's `TranscodeReasons`, as it named them: why it would not direct play.
    pub reasons: Vec<String>,
}

/// Transcode reasons that re-encode the picture. Jellyfin lists them in the URL's
/// `TranscodeReasons`; a burned-in subtitle and a bitrate over the limit both mean a new picture.
const VIDEO_REASONS: [&str; 15] = [
    "VideoCodecNotSupported", "VideoProfileNotSupported", "VideoLevelNotSupported",
    "VideoResolutionNotSupported", "VideoBitDepthNotSupported", "VideoFramerateNotSupported",
    "RefFramesNotSupported", "AnamorphicVideoNotSupported", "InterlacedVideoNotSupported",
    "VideoBitrateNotSupported", "ContainerBitrateExceedsLimit", "SubtitleCodecNotSupported",
    "VideoRangeTypeNotSupported", "VideoCodecTagNotSupported", "UnknownVideoStreamInfo",
];
const AUDIO_REASONS: [&str; 7] = [
    "AudioCodecNotSupported", "AudioChannelsNotSupported", "AudioProfileNotSupported",
    "AudioSampleRateNotSupported", "AudioBitrateNotSupported", "AudioBitDepthNotSupported",
    "SecondaryAudioNotSupported",
];

/// What the user's policy lets the server encode. A stream the server may not encode is copied as
/// it is, whatever its transcode reasons said (Jellyfin's `EncodingHelper::TryStreamCopy`: "force
/// stream copy, regardless of whether it will be compatible or not"), so the verdict must not
/// promise a new picture or sound the server will never make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Encoders {
    pub video: bool,
    pub audio: bool,
}

impl Encoders {
    pub const ALL: Encoders = Encoders { video: true, audio: true };
}

/// The verdict for the source `media_source_id`, else the first one.
pub fn verdict(r: &Response, media_source_id: Option<&str>, encoders: Encoders) -> Option<Verdict> {
    let s = media_source_id
        .and_then(|id| r.media_sources.iter().find(|s| s.id.eq_ignore_ascii_case(id)))
        .or_else(|| r.media_sources.first())?;
    let url = s.transcoding_url.clone().filter(|u| !u.is_empty());
    let reasons: Vec<String> = url
        .as_deref()
        .and_then(|u| super::paths::query_value(u, "TranscodeReasons"))
        .map(|v| v.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    let has = |set: &[&str]| reasons.iter().any(|r| set.contains(&r.as_str()));
    Some(Verdict {
        direct_play: s.supports_direct_play,
        video_encode: !s.supports_direct_play && encoders.video && has(&VIDEO_REASONS),
        audio_encode: !s.supports_direct_play && encoders.audio && has(&AUDIO_REASONS),
        encoders,
        url,
        reasons,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jellyfin::paths::with_param;

    fn device() -> Device<'static> {
        Device {
            video: vec!["h264", "hevc"],
            max_w: 3840,
            max_h: 2176,
            rows: vec![],
            vp9_bit_depth: 8,
            audio: "aac,ac3,eac3",
            converted_audio: "",
            audio_target: "aac",
        }
    }

    /// "Convert audio: on the TV" offers the converted codecs for direct play, and the transcoding
    /// profile, which the server encodes to, stays the device's own.
    #[test]
    fn converted_audio_is_offered_for_direct_play_only() {
        let p = device_profile(&Device { converted_audio: CONVERTED_AUDIO, ..device() });
        assert_eq!(p["DirectPlayProfiles"][0]["AudioCodec"], format!("aac,ac3,eac3,{CONVERTED_AUDIO}"));
        assert_eq!(p["TranscodingProfiles"][0]["AudioCodec"], "aac,ac3,eac3");
        for codec in CONVERTED_AUDIO.split(',') {
            assert!(plx_platform::devcaps::tv_convertible(codec), "{codec} is one the app converts");
        }
    }

    /// A TV that decodes FLAC, Opus and Vorbis itself lists them once for direct play, and the server
    /// is still never asked to encode to them.
    #[test]
    fn audio_the_tv_decodes_itself_is_direct_play_and_never_a_transcode_target() {
        let d = Device { audio: "aac,ac3,eac3,flac,opus,vorbis", converted_audio: CONVERTED_AUDIO, ..device() };
        let p = device_profile(&d);
        assert_eq!(
            p["DirectPlayProfiles"][0]["AudioCodec"],
            "aac,ac3,eac3,flac,opus,vorbis,truehd,alac,pcm_s16le,pcm_s16be,pcm_s24le,pcm_s24be,pcm_s32le,pcm_s32be,pcm_f32le,pcm_bluray,pcm_dvd"
        );
        assert_eq!(p["TranscodingProfiles"][0]["AudioCodec"], "aac,ac3,eac3");
        let p = device_profile(&Device { audio: "aac,ac3,eac3,flac,opus,vorbis", ..device() });
        assert_eq!(p["DirectPlayProfiles"][0]["AudioCodec"], "aac,ac3,eac3,flac,opus,vorbis");
    }

    #[test]
    fn the_profile_names_the_devices_codecs_and_no_image_subtitles() {
        let p = device_profile(&device());
        assert_eq!(p["DirectPlayProfiles"][0]["VideoCodec"], "h264,hevc");
        assert_eq!(p["DirectPlayProfiles"][0]["AudioCodec"], "aac,ac3,eac3");
        let t = &p["TranscodingProfiles"][0];
        assert_eq!((t["Container"].as_str(), t["Protocol"].as_str()), (Some("mkv"), Some("http")));
        assert_eq!(t["AudioCodec"], "aac,ac3,eac3");
        assert_eq!(p["CodecProfiles"][0]["Conditions"][0]["Value"], "3840");
        let subs: Vec<&str> = p["SubtitleProfiles"].as_array().unwrap().iter().filter_map(|s| s["Format"].as_str()).collect();
        assert!(subs.contains(&"srt") && subs.contains(&"ass"));
        assert!(!subs.iter().any(|f| ["pgssub", "pgs", "dvdsub", "dvbsub", "vobsub"].contains(f)));
        let h264_only = device_profile(&Device { video: vec!["h264"], ..device() });
        assert_eq!(h264_only["TranscodingProfiles"][0]["VideoCodec"], "h264");
    }

    /// An AV1 row puts av1 in both lists (direct play, and a copy lane after the h264 encode
    /// target), with no bit-depth cap: 10-bit AV1 direct-plays. A row tighter than the shared
    /// bound scopes its own limit to that codec; one as roomy adds nothing.
    #[test]
    fn av1_and_vp9_rows_are_offered_with_their_own_bounds() {
        let p = device_profile(&Device {
            video: vec!["h264", "hevc", "vp9", "av1"],
            rows: vec![("vp9", 3840, 2176), ("av1", 1920, 1080)],
            ..device()
        });
        assert_eq!(p["DirectPlayProfiles"][0]["VideoCodec"], "h264,hevc,vp9,av1");
        assert_eq!(p["TranscodingProfiles"][0]["VideoCodec"], "h264,hevc,vp9,av1");
        let profiles = p["CodecProfiles"].as_array().unwrap();
        let scoped = |c: &str| profiles.iter().find(|x| x["Codec"] == c).cloned();
        let av1 = scoped("av1").expect("a tighter AV1 row is scoped");
        let props: Vec<(&str, &str)> = av1["Conditions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["Property"].as_str().unwrap(), c["Value"].as_str().unwrap()))
            .collect();
        assert_eq!(props, [("Width", "1920"), ("Height", "1080")], "no bit-depth cap on AV1");
        let vp9 = scoped("vp9").expect("8-bit VP9 is capped");
        assert_eq!(vp9["Conditions"].as_array().unwrap().len(), 1);
        assert_eq!(vp9["Conditions"][0]["Property"], "VideoBitDepth");
        assert_eq!(vp9["Conditions"][0]["Value"], "8");

        let roomy = device_profile(&Device {
            video: vec!["h264", "hevc", "av1"],
            rows: vec![("av1", 4096, 2304)],
            ..device()
        });
        assert_eq!(roomy["CodecProfiles"].as_array().unwrap().len(), 1, "a roomier row adds nothing");
        let unlisted = device_profile(&Device { rows: vec![("av1", 1920, 1080)], ..device() });
        assert_eq!(unlisted["CodecProfiles"].as_array().unwrap().len(), 1, "no row for a codec not listed");
    }

    #[test]
    fn the_verdict_reads_the_sources_answer_and_its_reasons() {
        let r: Response = serde_json::from_str(r#"{"MediaSources":[
            {"Id":"a","SupportsDirectPlay":true},
            {"Id":"b","SupportsDirectPlay":false,"TranscodingUrl":"/videos/x/stream.mkv?DeviceId=d&PlaySessionId=s1&TranscodeReasons=SubtitleCodecNotSupported,AudioCodecNotSupported"}],
            "PlaySessionId":"s1"}"#).unwrap();
        let a = verdict(&r, None, Encoders::ALL).unwrap();
        assert!(a.direct_play && a.url.is_none() && !a.video_encode);
        let b = verdict(&r, Some("B"), Encoders::ALL).unwrap();
        assert!(!b.direct_play && b.video_encode && b.audio_encode);
        let r: Response = serde_json::from_str(r#"{"MediaSources":[{"Id":"c","SupportsDirectPlay":false,
            "TranscodingUrl":"/videos/x/stream.mkv?TranscodeReasons=ContainerNotSupported"}]}"#).unwrap();
        let c = verdict(&r, None, Encoders::ALL).unwrap();
        assert!(!c.direct_play && !c.video_encode && !c.audio_encode, "a remux copies both");
        assert_eq!(c.reasons, ["ContainerNotSupported"], "the server's reasons are kept for the log");
    }

    /// The C1's AV1 failure: the server named a video reason, but the user may not transcode video,
    /// so it copied the AV1 picture while the player expected H.264. A stream the policy forbids
    /// encoding is a copy.
    #[test]
    fn a_stream_the_user_may_not_encode_is_copied_whatever_the_reasons() {
        let r: Response = serde_json::from_str(r#"{"MediaSources":[{"Id":"m","SupportsDirectPlay":false,
            "TranscodingUrl":"/videos/x/stream.mkv?VideoCodec=h264,hevc&TranscodeReasons=VideoCodecNotSupported,AudioCodecNotSupported"}]}"#).unwrap();
        let v = verdict(&r, None, Encoders { video: false, audio: true }).unwrap();
        assert!(!v.direct_play && !v.video_encode && v.audio_encode);
        let a = verdict(&r, None, Encoders { video: true, audio: false }).unwrap();
        assert!(a.video_encode && !a.audio_encode);
    }

    #[test]
    fn a_param_is_set_once_whatever_its_case() {
        let u = with_param("/videos/x/stream.mkv?PlaySessionId=srv&a=1", "playSessionId", "ours");
        assert_eq!(u, "/videos/x/stream.mkv?a=1&playSessionId=ours");
        assert_eq!(with_param("/v", "k", "1"), "/v?k=1");
    }
}
