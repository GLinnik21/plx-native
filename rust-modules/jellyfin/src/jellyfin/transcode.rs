//! A transcode request in the app's terms (`plx_plex::plex::TranscodeSpec`) as a Jellyfin stream.
//!
//! The player asks for a transcode when its device profile says the file will not play as it is:
//! a container it cannot pull (REMUX: copy the codecs into Matroska, convert only what it cannot
//! decode), or a codec, size or rate it cannot take (RE-ENCODE). Jellyfin serves both from the
//! same progressive route, `/Videos/{id}/stream.mkv`, deciding per track from two things this
//! module sends: the codec list (copy a track whose codec is listed, else encode to the first) and
//! whether a copy is allowed at all.
//!
//! The answer the player builds its decoder from is the codec each track will ARRIVE in, so the
//! decision here names exactly what the request makes the server send: a copied track keeps its
//! codec, an encoded video is H.264 (an encoder every Jellyfin server has; HEVC encoding is off by
//! default there), and an encoded audio track is the device's first audio target.
//!
//! Jellyfin's HLS is not offered: the player's segmented route is built for Plex's playlist shape
//! and credential, so a request for it is refused and the player keeps to the progressive stream.

use super::paths::Transcode;
use super::playback_info::Encoders;
use super::time::ms_to_ticks;
use plx_plex::plex::{Ceiling, MediaContainer, Metadata, Stream, TranscodeDelivery, TranscodeOffset, TranscodeSpec};

/// What the device takes, read once per request from its capability table.
pub struct Device<'a> {
    /// Video codecs the device decodes, which a server may copy.
    pub video_copy: &'a [&'a str],
    /// The audio codec the server encodes to.
    pub audio_target: &'a str,
    /// Whether the device decodes this audio codec at this channel count.
    pub audio_ok: &'a dyn Fn(&str, i64, i64) -> bool,
    /// The server said it will re-encode the picture or the sound ([`super::playback_info`]):
    /// the decision must not claim a copy the stream will not carry.
    pub force_video_encode: bool,
    pub force_audio_encode: bool,
    /// What the user's policy lets the server encode ([`super::playback_info::Encoders`]). A
    /// stream it may not encode arrives copied, whatever was asked for.
    pub encoders: Encoders,
}

/// The encoder Jellyfin always has.
const VIDEO_TARGET: &str = "h264";

/// A planned transcode: the query for the stream and the decision the player reads.
pub struct Plan {
    pub media_source_id: String,
    pub video_codecs: String,
    pub audio_codecs: String,
    pub copy_video: bool,
    pub copy_audio: bool,
    pub audio_index: Option<i64>,
    pub burn_subtitle: Option<i64>,
    pub ceiling: Option<Ceiling>,
    pub start_ticks: i64,
    pub decision: MediaContainer,
    /// The server's own stream for this plan ([`super::playback_info::Verdict::url`]), when its
    /// copy and encode choices are the ones this plan describes.
    pub server_url: Option<String>,
}

impl Plan {
    pub fn request<'a>(&'a self, item_id: &'a str, play_session_id: &'a str) -> Transcode<'a> {
        Transcode {
            item_id,
            media_source_id: &self.media_source_id,
            play_session_id,
            device_id: "",
            video_codecs: &self.video_codecs,
            audio_codecs: &self.audio_codecs,
            copy_video: self.copy_video,
            copy_audio: self.copy_audio,
            audio_index: self.audio_index,
            burn_subtitle: self.burn_subtitle,
            max_bitrate: self.ceiling.map(|c| c.max_kbps.saturating_mul(1000)),
            max_width: self.ceiling.map(|c| c.max_w),
            max_height: self.ceiling.map(|c| c.max_h),
            start_ticks: self.start_ticks,
        }
    }
}

pub fn hls_refusal() -> MediaContainer {
    MediaContainer {
        general_decision_code: Some(2000),
        general_decision_text: "This server's streams are served as Matroska, not HLS.".into(),
        ..Default::default()
    }
}

/// The server may not encode a stream this device cannot decode, so it would send it as it is.
pub fn copy_refusal(kind: &str, codec: &str) -> MediaContainer {
    MediaContainer {
        general_decision_code: Some(2000),
        general_decision_text: format!(
            "This server only sends this {kind} as it is ({codec}), which this TV cannot play: its user may not transcode {kind}."
        ),
        ..Default::default()
    }
}

pub fn is_hls(spec: &TranscodeSpec<'_>) -> bool {
    matches!(spec.contract.delivery, TranscodeDelivery::FixedHls { .. })
}

/// The media source id a part key carries (`...&mediaSourceId=<id>`).
pub fn media_source_of(part_key: &str) -> Option<&str> {
    super::paths::query_value(part_key, "mediaSourceId").filter(|v| !v.is_empty())
}

/// Plan `spec` for item `m`: the version at `spec.media_index` (the player's `mediaIndex`), which a
/// seek or a change of quality asks for again, so a chosen version keeps playing.
pub fn plan(mut m: Metadata, spec: &TranscodeSpec<'_>, dev: &Device<'_>) -> Option<Plan> {
    if spec.media_index >= m.media.len() {
        return None;
    }
    m.media.swap(0, spec.media_index);
    m.media.truncate(1);
    let media = m.media.first_mut()?;
    media.part.truncate(1);
    let (bitrate, width, height) = (media.bitrate, media.width, media.height);
    let part = media.part.first_mut()?;
    let media_source_id = media_source_of(&part.key)?.to_string();
    let streams = std::mem::take(&mut part.stream);
    let mut video = None;
    let mut audio = None;
    let audio_at = pick_audio(&streams, spec.audio_stream_id);
    for (n, s) in streams.into_iter().enumerate() {
        if s.stream_type == 1 && video.is_none() {
            video = Some(s);
        } else if Some(n) == audio_at {
            audio = Some(s);
        }
    }
    let c = &spec.contract;

    // A remux is a copy by definition. A re-encode still copies when the server may and the
    // source already fits: the device decodes it and it is inside the ceiling.
    // A server whose user may not encode copies the stream whatever it was asked for.
    let video_decodable = video.as_ref().is_some_and(|v| dev.video_copy.contains(&v.codec.as_str()));
    let copy_video = video.as_ref().is_some_and(|_| {
        let fits = c.ceiling.is_none_or(|ceil| ceil.admits(bitrate, width, height));
        !dev.encoders.video || ((c.remux || (!c.no_video_copy && video_decodable && fits)) && !dev.force_video_encode)
    });
    let audio_decodable = audio.as_ref().is_some_and(|a| (dev.audio_ok)(&a.codec, a.channels, a.sampling_rate));
    let copy_audio = audio.is_some() && (!dev.encoders.audio || (audio_decodable && !dev.force_audio_encode));
    let refusal = match (&video, &audio) {
        (Some(v), _) if copy_video && !video_decodable && !c.remux => Some(copy_refusal("video", &v.codec)),
        (_, Some(a)) if copy_audio && !audio_decodable => Some(copy_refusal("audio", &a.codec)),
        _ => None,
    };
    let video_codecs = match &video {
        Some(v) if copy_video && v.codec != VIDEO_TARGET => format!("{},{VIDEO_TARGET}", v.codec),
        _ => VIDEO_TARGET.to_string(),
    };
    let audio_codecs = match &audio {
        Some(a) if copy_audio && a.codec != dev.audio_target => format!("{},{}", plx_platform::devcaps::audio_codec_id(&a.codec), dev.audio_target),
        _ => dev.audio_target.to_string(),
    };
    let audio_index = audio.as_ref().map(|a| a.index);

    let burn = (spec.subtitle_stream_id > 0 && !spec.client_subtitles).then_some(spec.subtitle_stream_id);
    let start_ticks = match spec.offset {
        TranscodeOffset::Fresh => 0,
        TranscodeOffset::AtMicros(us) => ms_to_ticks(us / 1000),
    };

    // The decision: the part, with the streams as they will arrive. The chosen audio track is the
    // only audio stream, because the player reads the first one as the stream's audio.
    part.decision = "transcode".into();
    part.container = "mkv".into();
    part.stream = video
        .map(|v| arriving(v, copy_video, VIDEO_TARGET))
        .into_iter()
        .chain(audio.map(|a| arriving(a, copy_audio, dev.audio_target)))
        .collect();
    media.container = "mkv".into();

    Some(Plan {
        media_source_id,
        video_codecs,
        audio_codecs,
        copy_video,
        copy_audio,
        audio_index,
        burn_subtitle: burn,
        // A ceiling makes Jellyfin re-encode a source above it, so a copied stream carries none.
        ceiling: if copy_video { None } else { c.ceiling },
        start_ticks,
        decision: refusal.unwrap_or(MediaContainer { size: 1, total_size: 1, metadata: vec![m], ..Default::default() }),
        server_url: None,
    })
}

/// The position of the audio track to carry: the one asked for, else the selected one, else the
/// first.
fn pick_audio(streams: &[Stream], asked: i64) -> Option<usize> {
    let audio = || streams.iter().enumerate().filter(|(_, s)| s.stream_type == 2);
    audio()
        .find(|(_, s)| asked > 0 && s.id == asked)
        .or_else(|| audio().find(|(_, s)| s.selected == 1))
        .or_else(|| audio().next())
        .map(|(n, _)| n)
}

fn arriving(mut s: Stream, copied: bool, target: &str) -> Stream {
    s.decision = if copied { "copy" } else { "transcode" }.into();
    if !copied {
        s.codec = target.into();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_plex::plex::{AudioEnhancements, EncodeContract, Media, MediaPart};

    fn stream(id: i64, kind: i64, codec: &str, channels: i64, selected: bool) -> Stream {
        Stream {
            id,
            index: id,
            stream_type: kind,
            codec: codec.into(),
            channels,
            selected: i64::from(selected),
            ..Default::default()
        }
    }

    fn item(video: &str, audios: &[(&str, i64)]) -> Metadata {
        let mut streams = vec![stream(0, 1, video, 0, true)];
        for (n, (codec, ch)) in audios.iter().enumerate() {
            streams.push(stream(n as i64 + 1, 2, codec, *ch, n == 0));
        }
        streams.push(stream(9, 3, "pgs", 0, false));
        Metadata {
            rating_key: "abc".into(),
            media: vec![Media {
                bitrate: 8000,
                width: 1920,
                height: 1080,
                part: vec![MediaPart {
                    key: "/Videos/abc/stream.avi?static=true&mediaSourceId=ms1".into(),
                    stream: streams,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn spec(remux: bool, ceiling: Option<Ceiling>) -> TranscodeSpec<'static> {
        static ASSUMED: std::sync::OnceLock<plx_platform::devcaps::Caps> = std::sync::OnceLock::new();
        TranscodeSpec {
            caps: ASSUMED.get_or_init(plx_platform::devcaps::Caps::assumed),
            rating_key: "abc",
            session: "s1",
            encoder_session: "s1",
            contract: EncodeContract {
                remux,
                delivery: TranscodeDelivery::ProgressiveMkv,
                no_video_copy: false,
                ceiling,
                audio: AudioEnhancements::NONE,
            },
            media_index: 0,
            audio_stream_id: 0,
            subtitle_stream_id: 0,
            client_subtitles: false,
            offset: TranscodeOffset::Fresh,
        }
    }

    fn device() -> Device<'static> {
        static OK: fn(&str, i64, i64) -> bool = |c, ch, _| matches!(c, "ac3" | "eac3" | "aac") && ch <= 6;
        Device { video_copy: &["h264", "hevc"], audio_target: "ac3", audio_ok: &OK, force_video_encode: false, force_audio_encode: false, encoders: Encoders::ALL }
    }

    fn codecs(p: &Plan) -> Vec<(i64, String, String)> {
        p.decision.metadata[0].media[0].part[0]
            .stream
            .iter()
            .map(|s| (s.stream_type, s.codec.clone(), s.decision.clone()))
            .collect()
    }

    #[test]
    fn a_remux_copies_the_video_and_converts_audio_the_tv_cannot_decode() {
        let p = plan(item("hevc", &[("truehd", 8)]), &spec(true, None), &device()).unwrap();
        assert!(p.copy_video && !p.copy_audio);
        assert_eq!((p.video_codecs.as_str(), p.audio_codecs.as_str()), ("hevc,h264", "ac3"));
        assert_eq!(
            codecs(&p),
            [(1, "hevc".into(), "copy".into()), (2, "ac3".into(), "transcode".into())]
        );
        assert_eq!(p.media_source_id, "ms1");
        assert_eq!(p.decision.metadata[0].media[0].part[0].decision, "transcode");
    }

    /// The version asked for by its position (`mediaIndex`) is the one transcoded, every time it
    /// is asked (a seek or a change of quality asks again); a position past the item's versions
    /// plans nothing rather than another version.
    #[test]
    fn the_version_at_the_media_index_is_the_one_transcoded() {
        let two = || {
            let mut m = item("hevc", &[("truehd", 8)]);
            m.media.extend(item("hevc", &[("truehd", 8)]).media);
            m.media[1].part[0].key = "/Videos/abc/stream.mkv?static=true&mediaSourceId=ms2".into();
            m
        };
        let at = |i: usize| TranscodeSpec { media_index: i, ..spec(true, None) };
        assert_eq!(plan(two(), &at(0), &device()).unwrap().media_source_id, "ms1");
        assert_eq!(plan(two(), &at(1), &device()).unwrap().media_source_id, "ms2");
        assert!(plan(two(), &at(2), &device()).is_none());
    }

    #[test]
    fn a_codec_the_tv_cannot_decode_is_encoded_to_h264() {
        let p = plan(item("mpeg4", &[("ac3", 6)]), &spec(false, None), &device()).unwrap();
        assert!(!p.copy_video && p.copy_audio);
        assert_eq!((p.video_codecs.as_str(), p.audio_codecs.as_str()), ("h264", "ac3"));
        assert_eq!(codecs(&p), [(1, "h264".into(), "transcode".into()), (2, "ac3".into(), "copy".into())]);
    }

    /// The server said it re-encodes (a bitrate limit, a subtitle to burn in): the decision names
    /// what arrives, not what the device could have taken.
    #[test]
    fn the_servers_encode_overrides_a_copy() {
        let dev = Device { force_video_encode: true, force_audio_encode: true, ..device() };
        let p = plan(item("hevc", &[("ac3", 6)]), &spec(false, None), &dev).unwrap();
        assert!(!p.copy_video && !p.copy_audio);
        assert_eq!(codecs(&p), [(1, "h264".into(), "transcode".into()), (2, "ac3".into(), "transcode".into())]);
        assert_eq!(p.server_url, None, "the plan alone never names a server stream");
    }

    /// The C1's AV1 failure: the server said it would encode the picture, but its user may not
    /// transcode video, so it copied the AV1 source. The decision names the copy the stream carries.
    #[test]
    fn a_server_that_may_not_encode_video_copies_it() {
        let dev = Device {
            video_copy: &["h264", "hevc", "av1"],
            force_video_encode: true,
            force_audio_encode: true,
            encoders: Encoders { video: false, audio: true },
            ..device()
        };
        let p = plan(item("av1", &[("opus", 8)]), &spec(false, None), &dev).unwrap();
        assert!(p.copy_video && !p.copy_audio);
        assert_eq!(codecs(&p), [(1, "av1".into(), "copy".into()), (2, "ac3".into(), "transcode".into())]);
        let low = Ceiling { max_kbps: 4000, max_w: 1280, max_h: 720 };
        let p = plan(item("av1", &[("ac3", 6)]), &spec(false, Some(low)), &dev).unwrap();
        assert!(p.copy_video && p.ceiling.is_none(), "a ceiling it cannot apply is not sent");
    }

    /// A source it would copy and the TV cannot decode is refused with the reason, before a byte moves.
    #[test]
    fn a_copy_the_tv_cannot_decode_is_refused_with_a_reason() {
        let no_video = Device { encoders: Encoders { video: false, audio: true }, ..device() };
        let p = plan(item("mpeg4", &[("ac3", 6)]), &spec(false, None), &no_video).unwrap();
        assert_eq!(p.decision.general_decision_code, Some(2000));
        assert!(p.decision.general_decision_text.contains("mpeg4"));
        let no_audio = Device { encoders: Encoders { video: true, audio: false }, ..device() };
        let p = plan(item("h264", &[("truehd", 8)]), &spec(false, None), &no_audio).unwrap();
        assert_eq!(p.decision.general_decision_code, Some(2000));
        assert!(p.decision.general_decision_text.contains("truehd"));
        let p = plan(item("h264", &[("ac3", 6)]), &spec(false, None), &no_audio).unwrap();
        assert_eq!(p.decision.general_decision_code, None, "a copy the TV decodes plays");
    }

    #[test]
    fn a_ceiling_the_source_exceeds_forces_an_encode_and_is_sent() {
        let low = Ceiling { max_kbps: 4000, max_w: 1280, max_h: 720 };
        let p = plan(item("h264", &[("aac", 2)]), &spec(false, Some(low)), &device()).unwrap();
        assert!(!p.copy_video);
        let r = p.request("abc", "s1");
        assert_eq!((r.max_bitrate, r.max_width, r.max_height), (Some(4_000_000), Some(1280), Some(720)));
    }

    #[test]
    fn the_asked_audio_track_is_the_one_carried_and_a_subtitle_is_burned() {
        let mut s = spec(false, None);
        s.audio_stream_id = 2;
        s.subtitle_stream_id = 9;
        s.offset = TranscodeOffset::from_seconds(90);
        let p = plan(item("h264", &[("aac", 2), ("dts", 6)]), &s, &device()).unwrap();
        assert_eq!(p.audio_index, Some(2));
        assert!(!p.copy_audio);
        assert_eq!(p.burn_subtitle, Some(9));
        assert_eq!(p.start_ticks, 90 * 10_000_000);
        s.client_subtitles = true;
        s.subtitle_stream_id = 0;
        assert_eq!(plan(item("h264", &[("aac", 2)]), &s, &device()).unwrap().burn_subtitle, None);
    }

    // The model spells DTS Plex's way ("dca"); the server matches a copy against "dts".
    #[test]
    fn a_copied_dts_track_is_named_the_way_the_server_names_it() {
        let dev = Device {
            video_copy: &["h264"],
            audio_target: "ac3",
            audio_ok: &|c, _, _| c == "dca",
            force_video_encode: false,
            force_audio_encode: false,
            encoders: Encoders::ALL,
        };
        let p = plan(item("h264", &[("dca", 6)]), &spec(false, None), &dev).unwrap();
        assert!(p.copy_audio);
        assert_eq!(p.audio_codecs, "dts,ac3");
    }

    // A remux under a quality ceiling still copies: the ceiling would make the server re-encode.
    #[test]
    fn a_copied_video_carries_no_ceiling() {
        let low = Ceiling { max_kbps: 4000, max_w: 1280, max_h: 720 };
        let p = plan(item("hevc", &[("ac3", 6)]), &spec(true, Some(low)), &device()).unwrap();
        assert!(p.copy_video);
        assert_eq!(p.request("abc", "s1").max_bitrate, None);
    }

    #[test]
    fn hls_is_refused_with_the_unplayable_code() {
        let r = hls_refusal();
        assert_eq!(r.general_decision_code, Some(2000));
        assert_eq!(media_source_of("/Videos/a/stream.mkv?static=true&mediaSourceId=x9"), Some("x9"));
        assert_eq!(media_source_of("/Videos/a/stream.mkv"), None);
    }
}
