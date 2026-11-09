//! Buffer-feed Load declarations, native worker and byte-preserving payload construction.
//! Payload builders take resolved facts; the physical Load worker reads sound output and logs.

pub mod aq;
pub mod control;
pub mod dv_start;
pub mod era_callbacks;
pub mod demux_clock;
pub mod feed_driver;
pub mod feed_resources;
pub mod feed_pump;
pub mod feed_state;
/// The simulator's stand-in for the Starfish/ACB seam (`hostsim` builds only; see its module doc).
#[cfg(feature = "hostsim")]
pub mod ffi_host;
pub mod ff_pcm;
mod ff;
pub use ff::{boot, demux, pushed_any, set_feed_audio, set_pcm_out, DemuxTransport};
pub use ff::native_audio;

/// Physical mechanisms exercised by media's integration fixtures. This module exports no raw
/// callback state; the feed's process-lifetime owners are `shared::{FEED_SHARED, FEED_TX}`.
#[cfg(feature = "test-support")]
pub mod ff_test_support {
    pub mod acquisition {
        pub use crate::ff::acquisition::{AcquisitionRuntime, Phase, SegmentAcquisition,
            CHECK_SLICE};
        pub mod observe {
            pub use crate::ff::acquisition::observe::{on_nth_continue, unwatch, watch_this_thread};
        }
    }
    pub use acquisition::{AcquisitionRuntime, Phase, SegmentAcquisition};
    /// The diag sample's counters that live outside the mailboxes: the access units fed per lane
    /// and whether the demuxer has handed over a video one.
    pub fn store_diag_counters(fed_v: i64, fed_a: i64, pushed_any: bool) {
        crate::native_executor::store_fed_totals(fed_v, fed_a);
        crate::ff::store_pushed_any(pushed_any);
    }
    /// The Feed's half of a whole-session reset (`FeedStop::reset` with `ResetScope::Stop`): its
    /// mirrors and the diag counters, with `common` (the caller's own stores) run inside the
    /// callback/reset barrier.
    pub fn reset_session_for_test(common: &mut dyn FnMut()) {
        reset_scoped_for_test(plx_playback_contract::feed::ResetScope::Stop, common);
    }
    /// [`reset_session_for_test`] for a reload of the same item (`ResetScope::Reload`).
    pub fn reset_session_for_reload_for_test(common: &mut dyn FnMut()) {
        reset_scoped_for_test(plx_playback_contract::feed::ResetScope::Reload, common);
    }
    fn reset_scoped_for_test(scope: plx_playback_contract::feed::ResetScope, common: &mut dyn FnMut()) {
        crate::shared::FEED_SHARED.reset_scoped(scope, common);
        // The counters the diag sample reads outside the mailboxes: a fresh session's are zero.
        store_diag_counters(0, 0, false);
    }
    pub use crate::ff::subs::SubTracks;
    pub use crate::ff::{annexb_start, arm_active_stall_guard, candidate_reserve_deadline,
        classify_curl_open_err, classify_hls_avio_facts, classify_hls_deadline,
        classify_plaintext_open_failure, drain_keep_polling,
        exploration_snapshot, exploration_timeout_is_final,
        frame_read_failed, hls_acquisition, hls_body_complete, hls_buffer_fact_at, hls_buffer_fact,
        hls_cursor_open, hls_demux_segment, hls_duration_obligation_ms, hls_feed_failure,
        hls_open_source, hls_prefetch_is_fatal, hls_prefetch_same_encoder_with,
        hls_should_prefetch_after_abr, hls_source_read, hls_stall_abort_outcome, hls_wait,
        observe_hls_deadline, open_input_failure_note,
        open_plain_progressive, packet_has_adts, park_read_fails_closed,
        read_cb, seek_cb, unproductive_exit_failed, AvioState, BlockingDeadline, BlockingDeadlineOwner, BlockingWake,
        HealBudget, HlsAu, HlsCursor, HlsExit, HlsNet, HlsSegmentOutput,
        MediaOpenFail, ReserveDeadlineState, SegmentTransfer,
        Src, StallAction, StallGuard, TransportWatchdog, VideoFraming,
        AUDIO_AHEAD_HEADROOM, AVSEEK_SIZE, BODY_HEAL_ATTEMPTS,
        FEED_AUDIO, PRE_AUDIO_VIDEO_HEADROOM, SEEK_END, SEEK_SET,
        push_test_elapsed, drain_test_elapsed};
}
pub mod hls;
pub mod native_gate;
pub mod native_load;
pub mod native_stall;
pub mod native_executor;
pub mod native_release;
pub mod open;
pub mod physical;
pub mod seek;
pub mod sf_event;
pub mod sink_fence;
pub mod playurl;
pub mod readout;
pub mod pcm_lead;
pub mod side_reader;
pub mod shared;
pub mod vp9_hdr;
mod send_ptr;
pub use send_ptr::SendPtr;

// BUFFERSTREAM Load payloads (ss4s shape). Video-only for the local sample path;
// video+AC3 for streaming. Copied VERBATIM from playback.c.
//
// `@APPID@` is substituted by `with_app_id` at the single choke point every variant passes
// through. It is a PLACEHOLDER rather than the literal it used to be because two installs can now
// sit on one television (`paths::app_id`), and a developer build announcing the SHIPPED app's id
// would be a false statement to the media pipeline about which application it is.
//
// WHAT THIS FIELD IS ACTUALLY FOR IS NOT KNOWN HERE, and the distinction matters because the two
// app ids this app sends travel different paths into different libraries:
//
//   * the ACB id (`acb_create` -> `AcbAPI_initialize`) has real evidence behind it. LG's own
//     `libcbe` keys media metadata on `{"appId":…,"pipelineId":…}` — recovered from this
//     television's binaries, `docs/dolby-vision.md` §3 — so the id reaches a subsystem that reads
//     it. What it does with it there is still inferred, not traced.
//   * `option.appId` in the Load payload, this key, has NOT been traced into `libpf` at all.
//     `tools/fwcompat.py` is a symbol inventory and a JSON key path lives in `.rodata`, so no tool
//     in this repository can answer it; `.agents/skills/decompile-tv-lib/` is the only route, and
//     `docs/two-installs.md` §7 states the question rather than an answer.
//
// So: sending the true id is the conservative move on a field whose consumer is unknown, not a fix
// for a mechanism anyone here has read. The failure it guards against — if it guards against one —
// is a black video plane with working audio and no error line anywhere, which is why it is worth
// making correct by construction rather than finding out on a television.
//
// A placeholder rather than `format!`-ing the constant: `with_window_id` splices `option.windowId`
// onto this exact key, and deriving both from one source is what makes it impossible for the
// anchor and the payload to drift apart. For the shipped app the composed bytes and the key order
// are identical to what every release so far sent — asserted in the tests below, because the
// webOS 5+ splice path is one this project's 4.5 dev set cannot exercise.
pub const PAYLOAD_V: &str = r#"{"args":[{"mediaTransportType":"BUFFERSTREAM","option":{"appId":"@APPID@","externalStreamingInfo":{"contents":{"codec":{"video":"H264"},"esInfo":{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true},"format":"RAW","provider":"plxnative"},"streamQualityInfo":true,"streamQualityInfoNonFlushable":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{"minimum":1,"maximum":32768},"srcBufferLevelVideo":{"minimum":1,"maximum":8388608}}},"needAudio":false,"queryPosition":false,"lowDelayMode":true,"transmission":{"contentsType":"LIVE"},"adaptiveStreaming":{"audioOnly":false,"maxWidth":1920,"maxHeight":1080,"maxFrameRate":30}}}]}"#;
// NB: pauseAtDecodeTime stays FALSE here. Kodi uses true, but only alongside its decode-time
// trigger machinery (setTimeToDecode); with true and no trigger the decoder never starts
// (verified on-device: Load+Play OK but zero frames decoded). The feed-ahead throttle
// (MAX_FEED_AHEAD_NS in feed_stream) is the anti-stall mechanism; the other Kodi payload
// flags are being re-introduced one at a time.
pub const PAYLOAD_AV: &str = r#"{"args":[{"mediaTransportType":"BUFFERSTREAM","option":{"appId":"@APPID@","externalStreamingInfo":{"contents":{"codec":{"video":"H264","audio":"AC3"},"esInfo":{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true},"format":"RAW","provider":"plxnative"},"streamQualityInfo":true,"streamQualityInfoNonFlushable":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{"minimum":1,"maximum":1048576},"srcBufferLevelVideo":{"minimum":1,"maximum":8388608}}},"needAudio":true,"queryPosition":false,"lowDelayMode":false,"transmission":{"contentsType":"LIVE"},"adaptiveStreaming":{"audioOnly":false,"maxWidth":1920,"maxHeight":1080,"maxFrameRate":30}}}]}"#;
// Phase 0 HEVC probe payload — identical to PAYLOAD_V but codec video "H265", to isolate
// the single variable: does StarfishMediaAPIs BUFFERSTREAM decode HEVC on this panel?
/// **`streamQualityInfoNonFlushable: true` is the presented-frame counter, and it is the only
/// per-frame instrument this app has on a non-Dolby path.** Decompiled from libpf 2026-09-03
/// (`CustomPipeline::updatePeriodicalInfo`, the same 200 ms timer as the position tick):
/// with `streamQualityInfo` the pipeline reads the sink's `dropped-frames` every 200 ms and
/// forwards a non-zero count as callback type 46; with this second key it also reads
/// `non-flushable-displayed-frames` and forwards it as type 47. Both arrive at `sf_on_event`
/// and are logged as `smp_cb type=46/47 num=<count>`, so consecutive samples against the log's
/// millisecond stamps ARE the presented cadence — the number the 5 Hz `vtick` and our GL `fps=`
/// could never give. Whether this firmware's sink exposes the property is answered by the first
/// run: libpf logs a pmlog error naming the property if it does not.
pub const PAYLOAD_H265: &str = r#"{"args":[{"mediaTransportType":"BUFFERSTREAM","option":{"appId":"@APPID@","externalStreamingInfo":{"contents":{"codec":{"video":"H265"},"esInfo":{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true},"format":"RAW","provider":"plxnative"},"streamQualityInfo":true,"streamQualityInfoNonFlushable":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{"minimum":1,"maximum":32768},"srcBufferLevelVideo":{"minimum":1,"maximum":8388608}}},"needAudio":false,"queryPosition":false,"lowDelayMode":true,"transmission":{"contentsType":"LIVE"},"adaptiveStreaming":{"audioOnly":false,"maxWidth":3840,"maxHeight":2160,"maxFrameRate":60}}}]}"#;

/// Build the streamed BUFFERSTREAM Load payload from PAYLOAD_AV, substituting the item's real
/// video/audio codecs + a sink envelope. video = "H264"|"H265", audio = "AC3"|"AC3 PLUS"|"AAC"|"DTS"|"OPUS"|"PCM".
/// What the Load's `adaptiveStreaming` block declares: the sink the pipeline ALLOCATES for, on the
/// firmwares that allocate against the declaration rather than the bitstream (webOS 10.3.1,
/// measured — `docs/webos10-resource-allocation.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SinkEnvelope {
    pub w: u32,
    pub h: u32,
    pub fps: u32,
}

/// The declaration every set has ever been verified against: 4K60. Both codecs on the dev set
/// (webOS 4.10), HEVC on 10.3.1 (13 Loads, DV P8 + Atmos among them).
pub const ENVELOPE_UHD60: SinkEnvelope = SinkEnvelope {
    w: 3840,
    h: 2160,
    fps: 60,
};
/// The one SMALLER declaration with a device measurement behind it: the H.264 patch that loaded in
/// 117 ms on 10.3.1 where 4K60 was refused twice, and played through ABR commits afterwards.
pub const ENVELOPE_FHD60: SinkEnvelope = SinkEnvelope {
    w: 1920,
    h: 1080,
    fps: 60,
};

// The codec one Load declares lives with the device's codec table (`plx_platform::devcaps`), where
// the player names it without naming this crate; the old path stays for this crate's own code.
pub use plx_platform::devcaps::LoadVideo;

/// True when the Load must be refused: see [`LoadVideo::refused_by`].
pub fn load_video_refused(lv: LoadVideo, caps: &plx_platform::devcaps::Caps) -> bool {
    lv.refused_by(caps)
}

/// **The sink envelope for this session, and the rule is deliberately narrow.** Until 2026-09-03
/// the streamed A/V Load declared `ENVELOPE_UHD60` for every codec and every source, and on
/// webOS 10.3.1 that refuses EVERY H.264 Load — i.e. every server transcode — with `type=18
/// num=601 Resource Allocation Error` before a frame is decoded. The lab session that found it
/// also measured the fix for the common shape and nothing beyond it, so:
///
/// * **HEVC keeps 4K60.** Dolby Vision P5/P8 and HDR10 are device-verified only under that
///   declaration, and nothing smaller has ever been declared for HEVC on any set.
/// * **H.264 declares FHD60 when the session's widest raster fits FHD**, which is what the lab
///   measured (a 1080p source's transcode), and stays 4K60 otherwise — a real 4K H.264 file, or an
///   Auto session whose catalog can reach the 4K actuator, keeps today's behaviour and today's
///   known refusal on 10.3.1. "Fits FHD" is width ≤ 1920 and height ≤ 1088: a 1080p H.264 stream's
///   CODED height is 1088 (16-aligned), and PMS reports coded sizes, so 1088 has to read as 1080
///   or every 1080p file would be promoted to the 4K declaration it was meant to escape.
/// * **Unknown raster (0 on an axis) means 4K60**, today's value: "nobody said" must not
///   under-declare a file that might be 4K.
/// * **The device's own table clamps, per codec, only when it was actually READ.** On the dev set
///   and on 10.3.1 both rows claim ≥4K@60 and nothing changes; a set whose HEVC row says
///   1920x1088 (a Full HD panel — issue #63's hypothesis) gets FHD60 for HEVC too. The assumed
///   fallback is never a clamp. A clamped 1088 is normalised to 1080 for the same reason as
///   above, and `fps` is never raised above 60 by the table (120 is a claim nobody has tested).
///
/// `maxFrameRate`: **H.264 declares the stream's own rate class** (24/25/30/50/60, never below
/// the stream beyond one 1 ms container-timebase tick per frame, so 30.303 declares 30, issue
/// #665; 60 when the rate is unknown) — see the measurement inside; HEVC stays at 60, the
/// value every Dolby Vision verification was taken under. `/tmp/plxnative-sinkmax=WxH@F`
/// overrides the whole result so the remaining legs are an A/B with no rebuild. Pure; the
/// wrappers feed it the route.
///
/// AV1 and VP9 copy HEVC's rule because only H.264 was measured to need the FHD declaration on
/// webOS 10; both are unmeasured on that axis (issue #593).
pub fn sink_envelope(
    video: LoadVideo,
    max_raster: (u16, u16),
    stream_fps: f64,
    caps: &plx_platform::devcaps::Caps,
    measured: bool,
) -> SinkEnvelope {
    let fits_fhd = |w: u32, h: u32| w > 0 && h > 0 && w <= 1920 && h <= 1088;
    let (mw, mh) = (u32::from(max_raster.0), u32::from(max_raster.1));
    let mut env = if video == LoadVideo::H264 && fits_fhd(mw, mh) {
        ENVELOPE_FHD60
    } else {
        ENVELOPE_UHD60
    };
    // **H.264 declares the stream's own rate class, and this is device-measured, not guessed.**
    // On the dev set (webOS 4.10) a 4K H.264 24p direct play declared at 60 makes the pipeline
    // announce `frameRate:24` and then, about a second later, `frameRate:30` for the same stream
    // — a 24p picture on a 30 fps lattice, i.e. pulldown judder the viewer can see. Declared at
    // 24 or at 30 it announces 24 once and holds it (2026-09-03, `plxnative-sinkmax` A/B, three
    // legs; 4K HEVC under 60 announces 24 and holds it, and 1080p H.264 under 60 does too, so
    // the rate is left at 60 for HEVC and for an unknown stream rate). **And the sink's own
    // displayed-frame counter (callback type 47, see the payload note above) put a number on
    // what the eye saw: declared at 60 the 4K 24p stream was PRESENTED at 13.0 fps mean, worst
    // second 10; declared at 24, 24.1 fps.** The class, not the exact value: 23.976 → 24 keeps
    // the declaration a ceiling the stream fits under. The one deliberate exception is a
    // millisecond-timebase 30.303 → 30 (`fps_class`, issue #665), where the declaration sits
    // about 1% BELOW the fed rate. Suspected, NOT yet device-verified: the k6lp SoC's H.264
    // decoder is 4K@30-limited, so declaring 50 on a 30p 4K stream is refused with num=601;
    // `pipe_h264_uhd_30p_ms_timebase` and an owner retest on the 60UN7310 are the proof, with a
    // second `/tmp/plxnative-nofps` leg to tell a rate-pair refusal from the 4K@30 limit.
    if video == LoadVideo::H264 && stream_fps > 0.0 && stream_fps.is_finite() {
        env.fps = fps_class(stream_fps);
    }
    if measured {
        let (rw, rh, rf) = decoder_row(video, caps);
        let clamp = |v: u32, bound: u32| if bound == 0 { v } else { v.min(bound) };
        env.w = clamp(env.w, rw);
        env.h = clamp(env.h, rh);
        env.fps = clamp(env.fps, rf);
        if env.h == 1088 {
            env.h = 1080;
        }
    }
    env
}

/// The device's measured decoder row (`(w, h, fps)`, 0 = no bound) for the codec a Load declares:
/// what [`sink_envelope`] clamps to and [`speed_fps_ceiling`] reads.
pub fn decoder_row(video: LoadVideo, caps: &plx_platform::devcaps::Caps) -> (u32, u32, u32) {
    video.decoder_row(caps)
}

// The Load payload's plain facts live in the contract (`plx_playback_contract::feed`), where the
// player names them without naming this crate; the old paths stay for this crate's own code.
pub use plx_playback_contract::feed::{DolbyHdrInfo, PayloadFacts, PayloadOptions, PcmFormat};

pub struct BuiltPayload {
    pub json: String,
    /// Existing one-per-Load diagnostics, emitted by the media adapter.
    pub notices: Vec<String>,
}

pub fn static_envelope(video: LoadVideo) -> SinkEnvelope {
    match video {
        LoadVideo::H264 => SinkEnvelope {
            w: 1920,
            h: 1080,
            fps: 30,
        },
        LoadVideo::H265 | LoadVideo::Av1 | LoadVideo::Vp9 => ENVELOPE_UHD60,
    }
}

/// The `PAYLOAD_H265` template with its codec renamed: the static Load for an AV1 or VP9 stream on
/// the no-audio path, which keeps that template's envelope and transport.
pub fn renamed_static_payload(video: LoadVideo) -> String {
    let to = format!(r#""video":"{}""#, video.payload_name());
    PAYLOAD_H265.replacen(r#""video":"H265""#, &to, 1)
}

/// The broadcast/film rate class (24, 25, 30, 50, 60) a stream's reported rate belongs to, else
/// the rate rounded up.
///
/// A rate belongs to the smallest class `c` for which either `fps <= c + 0.01` (the exact and
/// NTSC rates: 23.976, 29.97, 59.94) or its mean frame duration is within **1 ms** of the class's
/// (`1000/fps > 1000/c - 1`). The 1 ms is the Matroska timebase: timestamps are integers in
/// `TimestampScale` = 1 ms, so a frame interval estimated from them is off by less than one tick,
/// and a 30p file reports `avg_frame_rate` 1000/33 (30.303) and PMS `frameRate` 30.303 (issue
/// #665). The error is constant in TIME, not in rate, so the tolerance grows with the class: the
/// windows top out at 24.59, 25.64, 30.93, 52.63 and 63.83 and never overlap the next class.
///
/// This is NOT a claim that the stream is exactly the class: a constant-33 ms file really is fed
/// at 30.303 fps, so the declaration can sit about 1% below the fed rate. libpf has never been
/// measured accepting `maxFrameRate` below `videoFps`; the esInfo rational deliberately stays the
/// raw rate (it is a timing input for libpf's PTS lattice, not a ceiling), and only the ceiling
/// is snapped. Only the H.264 path reaches this; HEVC and a video re-encode (no known rate) keep 60. A remux
/// copies the source video, so it arrives here with the source's rate like a direct play.
pub fn fps_class(fps: f64) -> u32 {
    for class in [24u32, 25, 30, 50, 60] {
        let c = f64::from(class);
        if fps <= c + 0.01 || 1000.0 / fps > 1000.0 / c - 1.0 {
            return class;
        }
    }
    fps.ceil() as u32
}

/// `WxH@F` — three positive integers; anything else is `None`.
pub fn parse_sinkmax(spec: &str) -> Option<SinkEnvelope> {
    let (wh, fps) = spec.trim().split_once('@')?;
    let (w, h) = wh.split_once('x')?;
    let env = SinkEnvelope {
        w: w.trim().parse().ok()?,
        h: h.trim().parse().ok()?,
        fps: fps.trim().parse().ok()?,
    };
    (env.w > 0 && env.h > 0 && env.fps > 0).then_some(env)
}

/// LG's buffer-feed vocabulary. libpf CustomPipeline::parseOptionStringSpi accepts DTS;
/// getAudioCaps selects audio/x-dts and setAdecSinkInfo_ES configures dts-seamless (issue #221).
/// Unsupported formats must never be mislabeled as AC3: that can stall the audio master clock.
pub fn audio_payload_codec(codec: &str) -> Option<(&'static str, u8)> {
    // PMS names DTS `dca`; the shared canonicaliser maps every spelling onto the pipeline's.
    match plx_platform::devcaps::audio_codec_id(codec).as_ref() {
        "ac3" => Some(("AC3", 1)),
        "eac3" => Some(("AC3 PLUS", 2)),
        "aac" => Some(("AAC", 3)),
        "dts" => Some(("DTS", 4)),
        // SMP's Opus: raw packets plus `contents.opusInfo` ([`with_opus_info`]), as ss4s feeds it.
        "opus" => Some(("OPUS", 6)),
        // decoded in the app (`ff::PcmDecoder`) and fed as PCM: libpf has no FLAC or ALAC input,
        // and LPCM arrives in containers it does not take. `PCM_AUDIO_CODECS` is the set.
        c if plx_platform::devcaps::PCM_AUDIO_CODECS.split(',').any(|p| p == c) => Some(("PCM", 5)),
        _ => None,
    }
}

/// RFC 7845 §5.1 `OpusHead` as libopus's surround encoder writes it: mapping family 0 for mono and
/// stereo, family 1 (Vorbis channel order) for 3-8 channels, pre-skip 312, 48 kHz, 0 dB gain.
/// The Load is built before the demuxer opens the file, so the file's own CodecPrivate is not
/// available here; a 5.1 Opus track of a real library carries exactly this header (see the test).
pub fn opus_head(channels: i64) -> Option<Vec<u8>> {
    // (streams, coupled streams, channel mapping) for 3..=8 channels — libopus `vorbis_mappings`.
    const FAMILY1: [(u8, u8, &[u8]); plx_platform::devcaps::OPUS_LOAD_CHANNELS as usize - 2] = [
        (2, 1, &[0, 2, 1]),
        (2, 2, &[0, 1, 2, 3]),
        (3, 2, &[0, 4, 1, 2, 3]),
        (4, 2, &[0, 4, 1, 2, 3, 5]),
        (4, 3, &[0, 4, 1, 2, 3, 5, 6]),
        (5, 3, &[0, 6, 1, 2, 3, 4, 5, 7]),
    ];
    let ch = u8::try_from(channels).ok().filter(|c| {
        (1..=plx_platform::devcaps::OPUS_LOAD_CHANNELS).contains(&u32::from(*c))
    })?;
    let mut head = b"OpusHead".to_vec();
    head.extend_from_slice(&[1, ch]);
    head.extend_from_slice(&312u16.to_le_bytes());
    head.extend_from_slice(&48_000u32.to_le_bytes());
    head.extend_from_slice(&0u16.to_le_bytes());
    if ch <= 2 {
        head.push(0);
    } else {
        let (streams, coupled, mapping) = FAMILY1[usize::from(ch) - 3];
        head.extend_from_slice(&[1, streams, coupled]);
        head.extend_from_slice(mapping);
    }
    Some(head)
}

/// The `contents.opusInfo` node an `OPUS` Load needs: SMP has no in-band way to learn a
/// multistream layout, so the channel count and the identification header travel in the payload
/// (`streamHeader` is standard padded base64, as ss4s sends it).
/// `None` when the count cannot be described or the payload has no `contents` anchor: the caller
/// refuses the Load rather than declaring a layout the stream does not have.
pub fn with_opus_info(p: &str, channels: i64) -> Option<String> {
    let head = opus_head(channels)?;
    let anchor = r#""provider":"plxnative""#;
    p.contains(anchor).then(|| {
        p.replace(
            anchor,
            &format!(
                r#"{anchor},"opusInfo":{{"channels":{channels},"sampleRate":48,"streamHeader":"{}"}}"#,
                plx_base::b64::encode(&head)
            ),
        )
    })
}

/// `hz` as the kilohertz number `pcmInfo.sampleRate` is matched as: `48`, `44.1`, `22.05`. libpf
/// reads the key with getFloat and looks it up in a kilohertz table; a rate in hertz matches
/// nothing and plays at the 44.1 kHz fallback. Declared as `48000`, a 1 kHz tone in a 48 kHz FLAC
/// was recorded from the room at 918.8 Hz on webOS 4.4.3 (OLED55B8), a 32 kHz one at 1378 Hz and
/// a 22.05 kHz one at about 2006 Hz: each is 1000 x 44100 / the file's rate.
/// PURE.
pub fn pcm_rate_khz(hz: u32) -> String {
    (f64::from(hz) / 1000.0).to_string()
}

/// The `contents.pcmInfo` node a PCM Load needs — the only way the pipeline learns the raw
/// stream's shape (libpf reads `pcmInfo.{format,channelMode,layout,sampleRate,bitsPerSample}`).
/// `sampleRate` is in kilohertz ([`pcm_rate_khz`]), the unit the speed decoder's node uses too.
/// PURE, spliced at the `provider` anchor like [`with_immersive`].
pub fn with_pcm_info(p: &str, audio: &str, pcm: Option<PcmFormat>) -> String {
    let (Some(f), "PCM") = (pcm, audio) else {
        return p.to_string();
    };
    let anchor = r#""provider":"plxnative""#;
    let format = if f.bits == 16 { "S16LE" } else { "S32LE" };
    let mode = if f.channels == 1 { "mono" } else { "stereo" };
    p.replace(
        anchor,
        &format!(
            r#"{anchor},"pcmInfo":{{"format":"{format}","channelMode":"{mode}","layout":"interleaved","sampleRate":{},"bitsPerSample":{}}}"#,
            pcm_rate_khz(f.sample_rate),
            f.bits
        ),
    )
}

/// The diagnostics line for an app-decoded PCM Load: the declared shape, and where the app resamples it
/// the track's own rate (` (resampled from 96000Hz)`; nothing when the two are the same).
/// PURE.
pub fn pcm_decode_line(codec: &str, f: &PcmFormat) -> String {
    let resampled = if f.source_rate != f.sample_rate { format!(" (resampled from {}Hz)", f.source_rate) } else { String::new() };
    format!("pcm: {codec} decoded in-app -> {}Hz {}-bit {}ch{resampled}", f.sample_rate, f.bits, f.channels)
}

pub fn fed_fps(fps: f64, rate_permille: i32) -> f64 {
    fps * rate_permille as f64 / 1000.0
}

/// [`fps_rational`] of the fed rate, scaled exactly: 24000/1001 at 1.5x is 36000/1001.
pub fn fed_fps_rational(fps: f64, rate_permille: i32) -> Option<(i64, i64)> {
    let (num, den) = fps_rational(fps)?;
    let (num, den) = (num * rate_permille as i64, den * 1000);
    let g = gcd(num, den);
    Some((num / g, den / g))
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a.abs().max(1) } else { gcd(b, a % b) }
}

pub fn fps_rational(fps: f64) -> Option<(i64, i64)> {
    if fps <= 0.0 {
        return None;
    }
    let near = |a: f64, tol: f64| (fps - a).abs() < tol;
    Some(if near(23.976, 0.01) {
        (24000, 1001)
    } else if near(29.97, 0.01) {
        (30000, 1001)
    } else if near(59.94, 0.02) {
        (60000, 1001)
    } else if near(47.952, 0.02) {
        (48000, 1001)
    } else if fps.fract().abs() < 0.001 {
        (fps.round() as i64, 1)
    } else {
        ((fps * 1000.0).round() as i64, 1000)
    })
}

pub fn build_av_payload(
    facts: &PayloadFacts, video: &str, audio: &str, env: SinkEnvelope, rate: i32, options: PayloadOptions,
) -> BuiltPayload {
    let mut notices = Vec::new();
    // WHO declares the PCM, decided once: FLAC at 1x (the app decodes it, `ff_pcm`) declares its
    // own shape below via `with_pcm_info`; every other PCM Load (the speed decoder, any codec,
    // `rate != 1x`) gets the fixed stereo node here. `audio == "PCM"` is true for BOTH, so the
    // fixed node is skipped when the app's shape exists, else the Load would carry two `pcmInfo`
    // keys that disagree on the rate's unit and the bit depth. (A FLAC track whose facts are
    // missing is `Err`: that Load is refused in `start_bufferfeed` before this payload is used.)
    let app_pcm = app_pcm_for_rate(rate, facts.pcm.clone()).and_then(Result::ok);
    let mut p = PAYLOAD_AV
        .replace(r#""video":"H264""#, &format!(r#""video":"{video}""#))
        .replace(r#""audio":"AC3""#, &format!(r#""audio":"{audio}""#))
        // The speed decoder's PCM (playback speed) needs its format. libpf 3.9.3 reads `sampleRate`
        // with getFloat and matches it against a KILOHERTZ table (48, 44.1, 32, …): `48000`
        // matches nothing and silently falls back to 44.1, which plays 48 kHz audio 9% slow and
        // resyncs every few seconds (measured 2026-10-15). `channelMode` is one of
        // mono/dual/stereo/joint stereo. So this node's rate is in kHz, and so is the FLAC node
        // `with_pcm_info` splices below (it declared hertz first: on a C1, webOS 6.5.3, every PCM
        // Load then ran at 44.1 kHz — GStreamer caps `rate=44100` — so a 48 kHz FLAC was 9% slow
        // and 32/22.05 kHz stuttered; an OLED55B8, webOS 4.4.3, did the same).
        .replace(r#""esInfo":"#, if audio == "PCM" && app_pcm.is_none() {
            r#""pcmInfo":{"bitsPerSample":16,"sampleRate":48,"channelMode":"stereo"},"esInfo":"#
        } else {
            r#""esInfo":"#
        })
        .replace(r#""maxWidth":1920"#, &format!(r#""maxWidth":{}"#, env.w))
        .replace(r#""maxHeight":1080"#, &format!(r#""maxHeight":{}"#, env.h))
        .replace(
            r#""maxFrameRate":30"#,
            &format!(r#""maxFrameRate":{}"#, env.fps),
        );
    // Real source frame rate (direct-play only; 0 on transcode → skip): give the pipeline the true
    // fps for A/V timing instead of the sink-envelope default, + adaptiveResolution so it adapts if
    // the coded dims change. libpf parses videoFpsValue/videoFpsScale/adaptiveResolution (verified).
    // `/tmp/plxnative-nofps` withholds the pair, for one experiment: the Dolby Vision display
    // -management lookup misses because the LUT ring is keyed ONE 90 kHz tick above what the
    // display firmware asks for (measured 2026-08-21 — 38 of 40 misses, written key == requested
    // + 1, with LG's own level-2 KADP logging armed mid-playback). Neither derivation is ours: the
    // fed PTS provably does not move the outcome (nudge A/B, alternating unseeked legs, 163/164/165
    // misses regardless), and the pipeline timestamps by NEAREST-rounding on the 1001/24000 lattice
    // rather than passing ours through. This rational is the one input we hand it that could be
    // what it builds that lattice FROM, so it is the one remaining lever on our side.
    if !options.with_fps {
        // The adapter owns the nofps trigger and its diagnostic line.
    } else if let Some((num, den)) = fed_fps_rational(facts.fps, rate) {
        p = p
            .replace(
                r#""seperatedPTS":true}"#,
                &format!(r#""seperatedPTS":true,"videoFpsValue":{num},"videoFpsScale":{den}}}"#),
            )
            .replace(
                r#""audioOnly":false"#,
                r#""audioOnly":false,"adaptiveResolution":true"#,
            );
        notices.push(format!(
            "esInfo: videoFps {num}/{den} + adaptiveResolution (src {:.3})",
            facts.fps
        ));
    }
    let (p, dv_notice) = with_dolby_hdr_info(&p, video, facts.dv, options.suppress_dv_node);
    notices.extend(dv_notice);
    // The app-decoded (FLAC, 1x) node: sampleRate in kHz and a format name. The speed decoder's
    // fixed node above is the only other `pcmInfo` a Load can carry, and never together with this.
    let p = with_pcm_info(&p, audio, app_pcm);
    let (json, immersive_notice) = with_immersive(&p, audio, facts.immersive);
    notices.extend(immersive_notice);
    BuiltPayload { json, notices }
}

/// Splice the firmware's Atmos declaration and return the adapter's original diagnostic.
pub fn with_immersive(p: &str, audio: &str, atmos: bool) -> (String, Option<String>) {
    if !atmos {
        return (p.to_string(), None);
    }
    if audio != "AC3 PLUS" {
        return (p.to_string(), Some(format!(
            "atmos: immersive NOT sent — payload audio codec is {audio}, not AC3 PLUS"
        )));
    }
    let anchor = r#""provider":"plxnative""#;
    if !p.contains(anchor) {
        return (p.to_string(), Some("atmos: payload has no provider anchor — immersive NOT spliced".into()));
    }
    (p.replace(anchor, &format!(r#"{anchor},"immersive":"ATMOS""#)),
     Some("atmos: sourceInfo contents.immersive=ATMOS".into()))
}

/// Splice the route's already decided Dolby Vision declaration.
pub fn with_dolby_hdr_info(
    p: &str, video: &str, dv: Option<DolbyHdrInfo>, suppress: bool,
) -> (String, Option<String>) {
    let Some(n) = dv else {
        return (p.to_string(), None);
    };
    if suppress {
        return (p.to_string(), Some(format!(
            "dv: DolbyHdrInfo P{} SUPPRESSED by diagnostic override (direct play kept)",
            n.profile_id
        )));
    }
    if video != "H265" && video != "AV1" {
        return (p.to_string(), Some(format!(
            "dv: DolbyHdrInfo P{} NOT sent — payload video codec is {video}, not H265 or AV1",
            n.profile_id
        )));
    }
    let anchor = r#""provider":"plxnative""#;
    if !p.contains(anchor) {
        return (p.to_string(), Some("dv: payload has no provider anchor — DolbyHdrInfo NOT spliced".into()));
    }
    let node = format!(
        r#","DolbyHdrInfo":{{"trackType":"{}","encryptionType":"{}","profileId":{}}}"#,
        n.track_type, n.encryption_type, n.profile_id
    );
    (p.replace(anchor, &format!("{anchor}{node}")), Some(format!(
        "dv: sourceInfo contents.DolbyHdrInfo profileId={} trackType={} encryptionType={} (codec {video})",
        n.profile_id, n.track_type, n.encryption_type
    )))
}

/// App-decoded PCM is replaced by the fixed speed-decoder declaration off normal speed.
pub fn app_pcm_for_rate<T>(session_rate: i32, pcm: Option<Result<T, String>>) -> Option<Result<T, String>> {
    if session_rate == 1000 { pcm } else { None }
}
