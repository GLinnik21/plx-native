//! FFmpeg demux and feed orchestration over the bundled FFmpeg 9.0 ABI.
//! Codec bindings, validation and the capture encoder live in `plx_media_codecs`.
//! Design record: docs/ffmpeg-demuxer-plan.md.
#![allow(dead_code)]
#![deny(clippy::debug_assert_with_mut_call)]
use crate::aq::AuQueue;
use plx_playback_contract::adaptive::{Acquisition, ActiveAction, CommitReadout, ActiveFeedPort, ActiveObservation, AdaptivePhysicalPort, AdaptiveRoundPort, AutomaticPort, AutomaticTransition, BufferFact, CandidateFencePort, CandidateMediaPort, ExplorationFact, FeedAction, FeedFailure, FeedObservation, FeedPort, PauseFact, RoundId, TrialActuatorPort, TrialClock, TrialId, VariantFact};
use plx_playback_contract::demux::{DemuxCallbackEffect, DemuxClock, DemuxEffect, DemuxObserver, DemuxSession, HlsBufferClocks, PauseLane};
use plx_playback_contract::SourceKey;
use crate::SendPtr;
use plx_net::stream::HttpStream;
use plx_net::net::origin::Origin;
use plx_net::range_io::{self, RangeFactory, RangeReader};
use plx_playback_contract::demux::DemuxFault;
use std::ffi::CString;
use std::os::raw::{c_int, c_long, c_uchar, c_uint, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
pub use plx_media_codecs::ffi::*;

fn audio_declaration_mismatch(declared: u8, actual: u8) -> bool {
    // PCM describes decoded output; the container still carries the compressed input.
    declared != 0 && declared != 5 && actual != 0 && declared != actual
}

#[test]
fn pcm_output_does_not_mismatch_the_compressed_source() {
    for codec in ["ac3", "eac3", "aac", "dts"] {
        let actual = crate::audio_payload_codec(codec).unwrap().1;
        assert!(!audio_declaration_mismatch(5, actual), "{codec} is decoded before PCM is fed");
        assert!(!audio_declaration_mismatch(actual, actual));
    }
    assert!(audio_declaration_mismatch(2, 3));
    assert!(!audio_declaration_mismatch(0, 3));
}

/// Captured by the media composition and moved into one demux worker. Authorization is checked
/// afresh on every request by the injected range factory and plaintext redirect gate.
pub struct DemuxTransport {
    range: Arc<dyn RangeFactory>,
    credential_gate: plx_net::stream::redirect::CredentialGate,
}
impl DemuxTransport {
    pub fn new(range: Arc<dyn RangeFactory>,
        credential_gate: plx_net::stream::redirect::CredentialGate) -> Self {
        Self { range, credential_gate }
    }
}

/// Feed audio (es=2) to the pipeline. Cleared by the /tmp/plxnative-noaudio dev trigger to
/// A/B whether the audio ES (E-AC3/Atmos) is what stalls the sink on 4K HEVC.
pub static FEED_AUDIO: AtomicBool = AtomicBool::new(true);
/// Did this demux session hand a single video AU to a lane? Cleared by `demux` on entry.
///
/// The tail uses it to tell "ended cleanly" from "ended having produced nothing", which EOF alone
/// cannot express — and which is the difference between a real end-of-file and the player sitting
/// on "Buffering…" forever with no error.
static PUSHED_ANY: AtomicBool = AtomicBool::new(false);
pub fn set_feed_audio(on: bool) {
    FEED_AUDIO.store(on, Ordering::Relaxed);
}

/// The PCM the current Load declared for app-decoded audio (`route::stream_pcm`), packed into one word:
/// channels in bits 0-7, sample bits in 8-15, the declared rate in 16-31, [`PCM_LOSSY_BIT`] at 32 and the
/// track's own rate ([`ff_pcm::PcmOut::source_rate`](crate::ff_pcm::PcmOut), at most `i32::MAX`: 31 bits) in
/// 33-63; 0 = this Load declares no PCM.
static PCM_OUT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Bit set in [`PCM_OUT`] when the Load's PCM is the LOSSY app decode's (a lossy codec the attempt decodes in the app).
const PCM_LOSSY_BIT: u64 = 1 << 32;
/// Where [`PCM_OUT`] keeps the track's own rate.
const PCM_SOURCE_SHIFT: u32 = 33;
/// Set by `start_bufferfeed` beside the Load, before the demuxer is spawned. `lossy`: the Load
/// declared the lossy decode's shape (`route::stream_pcm_is_lossy`), so a lossy stream is the app's
/// to decode only then, never because some lossless PCM Load happens to be 48 kHz stereo.
pub fn set_pcm_out(f: Option<crate::ff_pcm::PcmOut>, lossy: bool) {
    let v = f.map_or(0, |o| {
        u64::from(o.channels & 0xff)
            | u64::from(o.bits & 0xff) << 8
            | u64::from(o.rate & 0xffff) << 16
            | u64::from(o.source_rate & 0x7fff_ffff) << PCM_SOURCE_SHIFT
            | if lossy { PCM_LOSSY_BIT } else { 0 }
    });
    PCM_OUT.store(v, Ordering::Relaxed);
}
fn pcm_lossy() -> bool {
    PCM_OUT.load(Ordering::Relaxed) & PCM_LOSSY_BIT != 0
}
fn pcm_out() -> Option<crate::ff_pcm::PcmOut> {
    let v = PCM_OUT.load(Ordering::Relaxed);
    (v != 0).then(|| crate::ff_pcm::PcmOut {
        channels: (v & 0xff) as u32,
        bits: ((v >> 8) & 0xff) as u32,
        rate: ((v >> 16) & 0xffff) as u32,
        source_rate: (v >> PCM_SOURCE_SHIFT) as u32,
    })
}

/// PURE: read an [`AVDOVIDecoderConfigurationRecord`] out of the raw side-data bytes.
///
/// Split from the pointer walk in [`dovi_conf`] on purpose — this half is the only half worth
/// testing, and it is testable precisely because it never touches FFmpeg. (A test that had to
/// enter the library would take `dlopen`'s `None` branch on Darwin and pass without executing
/// anything, which is the failure shape `docs/agent-reference.md` warns about by name.)
///
/// A SHORT buffer yields `None` rather than a partial record. FFmpeg allocates these with
/// `av_dovi_alloc` and its own demuxers always write all nine bytes, so this cannot happen today
/// — but the record is public ABI whose size "is not a part of the public ABI" by its own header's
/// admission, and reading nine bytes out of a seven-byte allocation is a heap overread that would
/// report a plausible profile number rather than crash. A longer buffer is fine and expected: a
/// future FFmpeg may append fields, and the nine we read keep their meaning.
pub fn parse_dovi_conf(bytes: &[u8]) -> Option<AVDOVIDecoderConfigurationRecord> {
    if bytes.len() < 9 {
        return None;
    }
    Some(AVDOVIDecoderConfigurationRecord {
        dv_version_major: bytes[0],
        dv_version_minor: bytes[1],
        dv_profile: bytes[2],
        dv_level: bytes[3],
        rpu_present_flag: bytes[4],
        el_present_flag: bytes[5],
        bl_present_flag: bytes[6],
        dv_bl_signal_compatibility_id: bytes[7],
        dv_md_compression: bytes[8],
    })
}

/// The Dolby Vision configuration record attached to this stream, if the demuxer found one.
///
/// `coded_side_data` is the STREAM-level side data (as opposed to `AVPacket::side_data`, which is
/// per-packet), and both bundled demuxers populate it: `mov` from the `dvcC`/`dvvC` sample-entry
/// box, `matroska` from the `DolbyVisionConfiguration` block-additions element. So this answers
/// "what is this file really" for every container the app direct-plays, without decoding a frame.
unsafe fn dovi_conf(cp: *const AVCodecParameters) -> Option<AVDOVIDecoderConfigurationRecord> {
    let (list, n) = ((*cp).coded_side_data, (*cp).nb_coded_side_data);
    if list.is_null() || n <= 0 {
        return None;
    }
    for i in 0..n as usize {
        let sd = &*list.add(i);
        if sd.type_ != AV_PKT_DATA_DOVI_CONF || sd.data.is_null() {
            continue;
        }
        return parse_dovi_conf(std::slice::from_raw_parts(sd.data, sd.size));
    }
    None
}
/// What the container said about a stream's colour: the three H.273 code points from
/// `AVCodecParameters`, plus the mastering display and content light level from the stream's coded
/// side data when the demuxer found them (Matroska's `Colour` element fills them; each only when the
/// file states it). Read for VP9, whose bitstream cannot carry the transfer function.
unsafe fn container_hdr(cp: *const AVCodecParameters) -> crate::vp9_hdr::ContainerHdr {
    use crate::vp9_hdr::{parse_cll, parse_mastering, ContainerHdr};
    let mut h = ContainerHdr {
        trc: (*cp).color_trc,
        pri: (*cp).color_primaries,
        mc: (*cp).color_space,
        mastering: None,
        cll: None,
    };
    let (list, n) = ((*cp).coded_side_data, (*cp).nb_coded_side_data);
    if list.is_null() || n <= 0 {
        return h;
    }
    for i in 0..n as usize {
        let sd = &*list.add(i);
        if sd.data.is_null() {
            continue;
        }
        let bytes = std::slice::from_raw_parts(sd.data, sd.size);
        match sd.type_ {
            AV_PKT_DATA_MASTERING_DISPLAY_METADATA => h.mastering = parse_mastering(bytes),
            AV_PKT_DATA_CONTENT_LIGHT_LEVEL => h.cll = parse_cll(bytes),
            _ => {}
        }
    }
    h
}
/// The container's own name for this track — `title` (Matroska and most formats), else `name`
/// (MP4's per-track `udta` name box, which is what FFmpeg's mov demuxer exposes it as).
///
/// Deliberately NOT `handler_name`: MP4 sets it to a constant per media type (`"SubtitleHandler"`,
/// `"SoundHandler"`), so it is the same string on every track and reading it would give the picker
/// nine identical sub-lines instead of none — the failure it exists to fix, dressed as a fix.
///
/// The returned `String` is a COPY. The entry points into the stream's dictionary, which
/// `avformat_close_input` frees.
unsafe fn stream_name(s: *mut AVStream) -> String {
    [c"title", c"name"].into_iter().find_map(|key| stream_tag(s, key)).unwrap_or_default()
}

/// The stream's `language` tag (ISO 639-2 in Matroska/MP4: `chi`, `jpn`, `und`), or None when it
/// has none. A COPY, like [`stream_name`]. Only used to pick a legacy text-subtitle charset.
unsafe fn stream_language(s: *mut AVStream) -> Option<String> {
    stream_tag(s, c"language")
}

/// One metadata tag of the stream, trimmed, or None when absent or empty. A COPY: the entry
/// points into the stream's dictionary, which `avformat_close_input` frees.
unsafe fn stream_tag(s: *mut AVStream, key: &std::ffi::CStr) -> Option<String> {
    let dict = *((s as *const u8).add(OFF_STREAM_METADATA) as *const *const AVDictionary);
    if dict.is_null() {
        return None;
    }
    let e = av_dict_get(dict, key.as_ptr(), std::ptr::null(), 0);
    if e.is_null() || (*e).value.is_null() {
        return None;
    }
    // `to_string_lossy` rather than a strict decode: these are user-authored tags off a
    // stranger's file, and a mis-encoded byte must cost that character, not the whole value.
    let v = std::ffi::CStr::from_ptr((*e).value).to_string_lossy().trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// Every track's container name, by TYPE, in file order — the two lists
/// `metadata::audio_ordinal` / `metadata::sub_render_ordinal` index into.
///
/// File order is the join, and it is the mapping this file already relies on twice
/// (`nth_audio_stream`, and the subtitle enumeration that feeds `desired_sub_idx`). It is worth
/// stating why that is sound rather than convenient: PMS lists a part's streams in container order
/// too, and both ordinal helpers skip the SIDECAR streams that exist only on the server — which is
/// exactly the set that is not in this file. So position N here and position N there name one
/// track, and a file with no tags simply yields empty strings in the right slots.
unsafe fn track_names(fmt: *mut AVFormatContext) -> (Vec<String>, Vec<String>) {
    let (mut audio, mut subs) = (Vec::new(), Vec::new());
    let streams = (*fmt).streams;
    for i in 0..(*fmt).nb_streams {
        let st = *streams.add(i as usize);
        match (*stream_codecpar(st)).codec_type {
            AVMEDIA_TYPE_AUDIO => audio.push(stream_name(st)),
            AVMEDIA_TYPE_SUBTITLE => subs.push(stream_name(st)),
            _ => {}
        }
    }
    (audio, subs)
}

#[inline]
unsafe fn stream_index(s: *mut AVStream) -> c_int {
    *((s as *const u8).add(OFF_STREAM_INDEX) as *const c_int)
}

/// The ffmpeg stream index of the first audio stream whose codec matches `want` (the Load
/// payload's audio codec, e.g. "ac3"), or None. `av_find_best_stream` picks the "highest
/// quality" audio (on an 8-track file it chose DTS over the AC3 default) — but the Load payload
/// carries Media[0].audioCodec, so feeding a different-codec track leaves the audio ES
/// unconfigured and, with audioSync, wedges the video (BufferFull forever). Matching the fed
/// track to the payload codec avoids that.
unsafe fn audio_stream_matching(fmt: *mut AVFormatContext, want: &str) -> Option<c_int> {
    if want.is_empty() {
        return None;
    }
    let streams = (*fmt).streams;
    for i in 0..(*fmt).nb_streams {
        let cp = stream_codecpar(*streams.add(i as usize));
        if (*cp).codec_type == AVMEDIA_TYPE_AUDIO {
            let name = std::ffi::CStr::from_ptr(avcodec_get_name((*cp).codec_id)).to_string_lossy();
            if audio_name_matches(&name, want) {
                return Some(i as c_int);
            }
        }
    }
    None
}

/// FFmpeg's codec name against the payload codec, through the shared canonicaliser: FFmpeg says
/// `dts` where PMS (and so the payload codec) says `dca`.
fn audio_name_matches(ffmpeg_name: &str, want: &str) -> bool {
    plx_platform::devcaps::audio_codec_id(ffmpeg_name) == plx_platform::devcaps::audio_codec_id(want)
}

/// The ffmpeg stream index of the `n`-th audio stream in file order (for native audio-track
/// selection), or None if there are fewer than n+1 audio streams. metadata.audio is filtered
/// in the same file order, so the track menu's 0-based audio index maps 1:1 here.
unsafe fn nth_audio_stream(fmt: *mut AVFormatContext, n: i32) -> Option<c_int> {
    let streams = (*fmt).streams;
    let mut count = 0i32;
    for i in 0..(*fmt).nb_streams {
        let cp = stream_codecpar(*streams.add(i as usize));
        if (*cp).codec_type == AVMEDIA_TYPE_AUDIO {
            if count == n {
                return Some(i as c_int);
            }
            count += 1;
        }
    }
    None
}

pub use plx_media_codecs::SpeedDecoder;

/// Copy the embedded ASS headers before FFmpeg can release them, and hand over the `fonts` that
/// `detach_fonts` took out of the format when it was opened (issue #1328). The codec parameter and
/// dictionary layouts are already compile-asserted against our bundled FFmpeg headers; the
/// attachment media-type constant is asserted beside them. No firmware layout is inferred.
unsafe fn begin_ass_sources(
    session: &mut dyn DemuxSession,
    media_key: &str,
    streams: *mut *mut AVStream,
    subs: &SubTracks,
    fonts: Vec<plx_playback_contract::subtitle::EmbeddedFont>,
) -> Option<u64> {
    use plx_media_codecs::subtitle_extract::{collect_manifest, AssTrack};
    let tracks: Vec<AssTrack> = subs.entries().iter().zip(subs.plan()).map(|(info, kind)| AssTrack {
        stream_index: info.stream_index,
        authored: matches!(kind, plx_playback_contract::subtitle::DecodeKind::Ass { .. }),
    }).collect();
    let max_header_bytes = session.subtitle().max_header_bytes();
    let manifest = collect_manifest(streams, &tracks, max_header_bytes, fonts);
    session.subtitle().begin_sources(media_key, manifest)
}

/// Did the CURRENT demux session hand over a single video AU? Distinguishes "the demuxer produced
/// nothing" from "it produced AUs that never reached the decoder" — two stalls that present
/// identically as a spinner. See [`PUSHED_ANY`].
pub fn pushed_any() -> bool {
    PUSHED_ANY.load(std::sync::atomic::Ordering::Relaxed)
}

/// A test's stand-in for a demuxer that has (or has not) handed over a video access unit.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn store_pushed_any(pushed: bool) {
    PUSHED_ANY.store(pushed, std::sync::atomic::Ordering::Relaxed);
}

/// Boot smoke test + optional ABI probe. Called once at startup.
pub fn boot() {
    // The https media transport's own table, resolved here so the `dlopen` and its log line land
    // on the MAIN thread at start-up rather than inside a demux worker — and after `app.rs` has
    // run `net::global_init`, which is what makes `curl_global_init` main-thread-only as its doc
    // requires. Independent of FFmpeg: a set with no libcurl multi still demuxes local samples,
    // and a set with no FFmpeg still signs in.
    range_io::boot();
    boot_probe_with(
        || { plx_media_codecs::ffi::boot(); abi_ok() },
        || plx_base::devtrig::read("ffprobe"),
        probe,
    );
}

// Loader failure must fence every FFmpeg entry point, including the diagnostic trigger.
// Keep the runner injectable so the failure path is exercised without calling an absent ABI.
fn boot_probe_with(
    load: impl FnOnce() -> bool,
    trigger: impl FnOnce() -> Option<String>,
    inspect: impl FnOnce(&str),
) {
    if !load() { return; }
    if let Some(url) = trigger().filter(|url| !url.is_empty()) { inspect(&url); }
}

#[cfg(test)]
mod boot_tests {
    use super::boot_probe_with;

    #[test]
    fn a_failed_loader_fences_an_armed_probe() {
        boot_probe_with(
            || false,
            || panic!("a failed loader must not read a diagnostic source"),
            |_| panic!("a failed loader must never enter FFmpeg"),
        );
    }

    #[test]
    fn a_loaded_abi_inspects_only_a_nonempty_trigger() {
        let calls = std::cell::Cell::new(0);
        boot_probe_with(|| true, || Some(String::new()), |_| calls.set(calls.get() + 1));
        assert_eq!(calls.get(), 0);
        boot_probe_with(|| true, || Some("fixture".to_owned()), |source| {
            assert_eq!(source, "fixture");
            calls.set(calls.get() + 1);
        });
        assert_eq!(calls.get(), 1);
    }
}

/// Open `url` via libavformat, find streams, and log codec_id/dims/time_base for each —
/// the on-device confirmation that the FFI struct layout is correct. Uses libavformat's
/// own HTTP for the probe (the demuxer proper will use a custom AVIO over stream.rs), so that one
/// request names libavformat (`Lavf/...`), not the installed `net::user_agent`: it is reachable only with
/// the `devtriggers` feature, which the shipping set does not have, and it is the one transport
/// `ci/check-user-agent.py` cannot see.
fn probe(url: &str) {
    ensure_registered();
    unsafe {
        let cu = match std::ffi::CString::new(url) {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut fmt: *mut AVFormatContext = std::ptr::null_mut();
        let r = avformat_open_input(
            &mut fmt,
            cu.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if r < 0 || fmt.is_null() {
            plx_base::eventlog::log(&format!("ffprobe: avformat_open_input failed r={r}"));
            return;
        }
        let r2 = avformat_find_stream_info(fmt, std::ptr::null_mut());
        if r2 < 0 {
            plx_base::eventlog::log(&format!("ffprobe: find_stream_info failed r={r2}"));
            avformat_close_input(&mut fmt);
            return;
        }
        let ns = (*fmt).nb_streams;
        plx_base::eventlog::log(&format!(
            "ffprobe: nb_streams={ns} duration_us={} (expect HEVC codec_id=174 3840x1920)",
            fmt_duration(fmt)
        ));
        let streams = (*fmt).streams;
        for i in 0..ns {
            let st = *streams.add(i as usize);
            let cp = stream_codecpar(st);
            let tb = stream_time_base(st);
            plx_base::eventlog::log(&format!(
                "ffprobe: stream[{}] idx={} type={} codec_id={} {}x{} sr={} tb={}/{}",
                i,
                stream_index(st),
                (*cp).codec_type,
                (*cp).codec_id,
                (*cp).width,
                (*cp).height,
                (*cp).sample_rate,
                tb.num,
                tb.den
            ));
        }
        // BSF presence check (Phase A): both mp4toannexb filters must be compiled in.
        let hn = std::ffi::CString::new("hevc_mp4toannexb").unwrap();
        let an = std::ffi::CString::new("h264_mp4toannexb").unwrap();
        plx_base::eventlog::log(&format!(
            "ffprobe: bsf hevc={} h264={}",
            !av_bsf_get_by_name(hn.as_ptr()).is_null(),
            !av_bsf_get_by_name(an.as_ptr()).is_null()
        ));
        avformat_close_input(&mut fmt);
    }
}

// ==== the demuxer (Phases B–D) ===================================================

// whence values for the AVIO seek callback
pub const SEEK_SET: c_int = 0;
const SEEK_CUR: c_int = 1;
pub const SEEK_END: c_int = 2;
pub const AVSEEK_SIZE: c_int = 0x10000;

/// **Which transport is under the AVIO.** The two are not interchangeable and the choice is made
/// from the SCHEME of the URL actually served — the part URL's, unless a plaintext open was
/// redirected to https (`stream::redirect`), in which case the socket open hands over to curl.
/// Never guessed per call.
///
/// * [`Src::Socket`] is the original and the default: `stream.rs`'s raw TCP socket wrapping the
///   ENGINE-owned `HttpStream` — cleartext, numeric address, seeking by closing and re-opening
///   with a byte `Range`. Its behaviour here is byte for byte what it was before this enum
///   existed.
/// * [`Src::Curl`] is the injected HTTPS range reader, which the demux thread owns outright.
///   `ff.rs` never learns curl-multi mechanics: all it can ask is `read`/`seek`/`size`/`status`/
///   `abort`, deliberately the same five questions the socket answers.
/// * [`Src::Idle`] is the placeholder after HLS HTTPS recycles the reader into [`HlsNet`].
///   `avformat_close_input` may still invoke AVIO; this arm refuses I/O instead of dialing a
///   dummy socket.
pub enum Src {
    Socket {
        /// The Engine's stream — NOT owned here. `player::engine` allocates it, publishes it in
        /// `FEED_SHARED.hs_ptr`, `http_shutdown`s it at teardown and closes it after the join.
        hs: *mut HttpStream,
        host: CString,
        port: c_int,
        path: CString,
    },
    /// Owned by this state, and so by the demux thread — which is why teardown reaches it through
    /// `curlio`'s registry instead of a pointer the engine holds. `curlio`'s module doc says why
    /// that is not the accident it looks like.
    Curl(Box<dyn RangeReader>),
    Idle,
}

impl Src {
    /// What the transport has RECEIVED of the current body beyond what FFmpeg has read — the
    /// one completion question both sources answer (`stream::BodyReceipt`).
    fn body_receipt(&mut self) -> plx_net::stream::BodyReceipt {
        match self {
            Src::Socket { hs, .. } => plx_net::stream::http_body_receipt(*hs),
            Src::Curl(cs) => cs.body_receipt(),
            Src::Idle => plx_net::stream::BodyReceipt {
                ahead: 0,
                finished: false,
                stepped: false,
            },
        }
    }

    pub fn take_curl(&mut self) -> Option<Box<dyn RangeReader>> {
        match std::mem::replace(self, Src::Idle) {
            Src::Curl(cs) => Some(cs),
            other => {
                *self = other;
                None
            }
        }
    }
}

/// One transport slot for the HLS session: the engine's plaintext socket plus at most one curl
/// multi handle reused across playlist and segment URLs.
pub struct HlsNet {
    pub hs: *mut HttpStream,
    pub curl: Option<Box<dyn RangeReader>>,
    pub range: Arc<dyn RangeFactory>,
    pub credential_gate: plx_net::stream::redirect::CredentialGate,
    pub observer: Arc<dyn DemuxObserver>,
}

/// **The live-transfer terminal rule.** A partial response is right-censored evidence.  PMS may
/// open a sized HLS response, deliver a prefix, wait for its JIT encoder (`segmentWait` in the
/// server's own SessionReport), then burst the remainder.  Consequently neither
/// `prefix_bytes / prefix_time` nor `Content-Length - prefix_bytes` identifies the unseen
/// acquisition time.  The former attained-rate projection falsely abandoned sustainable 4K
/// responses on the remote PMS.
///
/// There is one coefficient-free fact available before completion: the playable reserve has
/// actually reached zero.  The main thread observes that physical boundary and holds Starfish's
/// clock; this guard observes the same hold (or the equivalent playhead spend) on every blocking
/// leg of the fetch — the open, the `NotReady` wait, the probe and body reads — through the
/// acquisition runtime's checkpoint (`ff_acquisition.rs`), within one `CHECK_SLICE` of a wait
/// already blocked.  Above the ladder floor it abandons the still-incomplete object so the
/// controller can fetch a smaller one, even before any body byte exists.  At the floor it leaves
/// the only useful response alive.  No prefix becomes a capacity sample in either direction.
#[derive(Clone)]
pub struct StallGuard {
    pub observer: Arc<dyn DemuxObserver>,
    /// The playable reserve when this fetch began, in ms of media. `None` is never stored — an
    /// unknowable reserve (`BufferSnapshot::buffered_ms`'s `None`, the audio lane silent after an
    /// open or a seek) arms no guard at all, because the comparison has no left-hand side.
    pub reserve_ms_at_start: i64,
    /// Playhead at the fetch boundary.  Wall time is deliberately absent: queued media is spent
    /// by presentation, not by a blocked request, pause, seek or server wait.
    pub playhead_at_start_ns: i64,
    /// `FEED_SHARED.hls_internal_hold_epoch` when armed. A hold the main thread accepted AND already
    /// released between two checks still moved it, so a short hold cannot slip between samples.
    pub hold_epoch_at_arm: u64,
    pub action: StallAction,
}

impl std::fmt::Debug for StallGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StallGuard")
            .field("attempt", &self.observer.attempt())
            .field("reserve_ms_at_start", &self.reserve_ms_at_start)
            .field("playhead_at_start_ns", &self.playhead_at_start_ns)
            .field("hold_epoch_at_arm", &self.hold_epoch_at_arm)
            .field("action", &self.action)
            .finish()
    }
}

impl PartialEq for StallGuard {
    fn eq(&self, other: &Self) -> bool {
        self.observer.attempt() == other.observer.attempt()
            && self.reserve_ms_at_start == other.reserve_ms_at_start
            && self.playhead_at_start_ns == other.playhead_at_start_ns
            && self.hold_epoch_at_arm == other.hold_epoch_at_arm
            && self.action == other.action
    }
}
impl Eq for StallGuard {}

/// What happens after the same in-flight conservation inequality is crossed. Above the floor a
/// smaller object exists, so abandoning the response releases the controller to fetch it. At the
/// floor abandoning can only re-request the same bytes; keep that response alive and hold the A/V
/// clock instead.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StallAction {
    AbortFetch,
    HoldClock,
}

impl StallGuard {
    /// **Arm the guard, or decline to.** A reserve of zero AT THE START of a fetch is not an
    /// emergency — it is what every session looks like before its first segment has been
    /// delivered, and the rule's premise is that continuing projects a stall the abort could
    /// avoid. With no picture yet there is nothing to stall and the abort has nothing to buy: it
    /// simply abandons the fetch, pushes the segment back, and abandons it again.
    ///
    /// **That is not hypothetical.** Armed without this gate, device-measured 2026-08-28:
    /// `abr: stall abort seq=0 bytes=2896 of 0ms reserve` — playback never started at all, one
    /// segment fetched, the video plane never bound. It is the same shape as the plan's I3
    /// (the first-segment false downshift): a predicate keyed on the reserve, evaluated at the one
    /// moment the reserve is empty for a reason that is not failure.
    ///
    /// Mid-playback a genuinely drained reserve declines the guard too, and that is the
    /// conservative direction rather than a gap: by then the picture has already stopped, the
    /// abort can no longer prevent anything, and `starving()` is the predicate that owns it.
    pub fn arm(reserve_ms_at_start: i64, observer: Arc<dyn DemuxObserver>) -> Option<Self> {
        if reserve_ms_at_start <= 0 { return None; }
        Some(Self {
            playhead_at_start_ns: observer.playpos_ns()?,
            hold_epoch_at_arm: observer.hold_epoch()?,
            observer,
            reserve_ms_at_start,
            action: StallAction::AbortFetch,
        })
    }

    /// At the terminal actuator the same observed boundary may stop presentation, but it must not
    /// discard the only response capable of producing more media.
    pub fn arm_clock_hold(reserve_ms_at_start: i64, observer: Arc<dyn DemuxObserver>) -> Option<Self> {
        Self::arm(reserve_ms_at_start, observer).map(|mut guard| {
            guard.action = StallAction::HoldClock;
            guard
        })
    }

    pub fn aborts_fetch(&self) -> bool {
        self.action == StallAction::AbortFetch
    }

    /// Has an incomplete fetch crossed an OBSERVED terminal boundary? `body_complete` exempts a
    /// response whose declared bytes all arrived (or whose unsized body reached a confirmed end);
    /// before the body exists nothing is complete. `terminal_hold_started` is the main thread's
    /// terminal observation — an exact arithmetic `B=0`, or a presentation clock that has
    /// physically stopped (`player::feed_pump::native_clock_stopped`) — and a hold epoch that moved since
    /// arming is the same observation, already released.
    /// The playhead form is the same arithmetic sampled by this worker, and on this pipeline it is
    /// a bound rather than a trigger: `reserve_ms_at_start` is measured to the demuxed tail, and
    /// the television parks the clock a few hundred ms short of that tail (the last fed access
    /// units cannot present until the ones behind them arrive), so `spent_ms` stops accruing just
    /// below the reserve and the hold has to come from the main thread. Device-measured
    /// 2026-09-02 (`pipe_abr_down_collapse`): reserve 5668 ms, playhead spent 5410 ms, then 77 s
    /// of `vtick=0` with neither branch firing. Neither branch predicts an unseen byte.
    pub fn should_abort(&self, body_complete: bool, terminal_hold_started: bool) -> bool {
        if body_complete {
            return false;
        }
        terminal_hold_started
            || self.observer.hold_epoch() != Some(self.hold_epoch_at_arm)
            || self.spent_ms() >= self.reserve_ms_at_start
    }

    pub fn spent_ms(&self) -> i64 {
        self.observer.playpos_ns().unwrap_or(i64::MAX)
            .saturating_sub(self.playhead_at_start_ns)
            .max(0)
            / 1_000_000
    }

    /// The wall instant at which the playhead, at one speed, would spend what remains. A wake-up
    /// only: a paused or stopped playhead makes it stale, and the next check simply re-projects.
    pub fn projected_boundary(&self) -> std::time::Instant {
        let left_ms = self
            .reserve_ms_at_start
            .saturating_sub(self.spent_ms())
            .max(0);
        let now = std::time::Instant::now();
        now.checked_add(std::time::Duration::from_millis(left_ms as u64))
            .unwrap_or(now)
    }
}

/// **The one way a guard asks the main thread to hold Starfish's A/V clock** — from whichever
/// blocking leg saw the boundary. The main thread may already have held it on its own exact B=0
/// sample; asking again is harmless.
/// An already-held clock is a recovery state: the active response is rebuilding reserve and may
/// not be abandoned by the boundary that has already fired. Kept pure at the call boundary so a
/// regression cannot silently turn B=0 into an abort loop again.
pub fn arm_active_stall_guard(
    reserve_ms: Option<i64>,
    at_floor: bool,
    already_held: bool,
    observer: Arc<dyn DemuxObserver>,
) -> Option<StallGuard> {
    if already_held {
        return None;
    }
    reserve_ms.and_then(|reserve_ms| {
        if at_floor {
            StallGuard::arm_clock_hold(reserve_ms, observer)
        } else {
            StallGuard::arm(reserve_ms, observer)
        }
    })
}

// `SegmentAcquisition` lives in its own module so its payload is not nameable from `ff` — see
// `ff_acquisition.rs` for why (the bypass a review round found: everything in `ff` could build
// `Active { stall: None, .. }` by hand even with the role-typed enum in place).
#[path = "ff_acquisition.rs"]
pub mod acquisition;
#[path = "ff_subs.rs"]
pub mod subs;
#[path = "ff_native_audio.rs"]
pub mod native_audio;
use plx_playback_contract::subtitle as subside;
use subs::SubTracks;
use acquisition::{AcquisitionRuntime, Phase, SegmentAcquisition};

/// A rollback-reserve deadline is monotone in one direction: a floor downshift that reaches an
/// actual internal rebuffer may stop protecting the old cursor, but a later user Resume cannot
/// make that already-spent reserve exist again. One instance follows the whole segment transaction
/// through open, wait, probe, body and final validation; promotion is latched as an unarmed clock.
#[derive(Clone)]
pub struct ReserveDeadlineState {
    clock: ReserveDeadlineClock,
    observer: Arc<dyn DemuxObserver>,
    yields_to_rebuffer: bool,
    hold_epoch_at_start: Option<u64>,
    pub expired: bool,
}

#[derive(Clone, Copy, Debug)]
enum ReserveDeadlineClock {
    Retired,
    Wall(Option<std::time::Instant>),
    Playhead {
        started_ns: i64,
        budget: std::time::Duration,
    },
    /// A bounded recovery transfer spends every elapsed interval except native-accepted user
    /// Pause. Unlike a playhead clock it therefore continues through involuntary starvation and
    /// the internal rebuffer hold; unlike a wall deadline it preserves Pause exactly, including a
    /// complete Pause->Resume cycle hidden inside one blocked network call.
    UnpausedElapsed {
        started_at: std::time::Instant,
        paused_at_start: std::time::Duration,
        budget: std::time::Duration,
    },
}

impl ReserveDeadlineState {
    pub fn new(at: Option<std::time::Instant>, yields_to_rebuffer: bool,
        observer: Arc<dyn DemuxObserver>) -> Self {
        let hold_epoch_at_start = observer.hold_epoch();
        Self {
            clock: if hold_epoch_at_start.is_some() {
                ReserveDeadlineClock::Wall(at)
            } else {
                ReserveDeadlineClock::Retired
            },
            observer,
            yields_to_rebuffer,
            hold_epoch_at_start,
            expired: false,
        }
    }

    /// A media-reserve deadline spends presentation time, not wall time. User Pause, an internal
    /// rebuffer hold and a genuinely starved native clock all freeze the same physical balance.
    pub fn from_playhead(budget: std::time::Duration, yields_to_rebuffer: bool,
        observer: Arc<dyn DemuxObserver>) -> Self {
        let Some(started_ns) = observer.playpos_ns() else {
            return Self {
                clock: ReserveDeadlineClock::Retired,
                observer,
                yields_to_rebuffer,
                hold_epoch_at_start: None,
                expired: false,
            };
        };
        Self::from_playhead_at(started_ns, budget, yields_to_rebuffer, observer)
    }

    pub fn from_playhead_at(
        started_ns: i64,
        budget: std::time::Duration,
        yields_to_rebuffer: bool,
        observer: Arc<dyn DemuxObserver>,
    ) -> Self {
        let hold_epoch_at_start = observer.hold_epoch();
        Self {
            clock: if hold_epoch_at_start.is_some() { ReserveDeadlineClock::Playhead {
                started_ns: started_ns.max(0),
                budget,
            }} else { ReserveDeadlineClock::Retired },
            observer,
            yields_to_rebuffer,
            hold_epoch_at_start,
            expired: false,
        }
    }

    pub fn from_unpaused_elapsed(budget: std::time::Duration, yields_to_rebuffer: bool,
        observer: Arc<dyn DemuxObserver>) -> Self {
        let pause = observer.pause_clock();
        let hold_epoch_at_start = observer.hold_epoch();
        Self {
            clock: if let (Some(pause), Some(_)) = (pause, hold_epoch_at_start) {
                ReserveDeadlineClock::UnpausedElapsed {
                started_at: pause.at,
                paused_at_start: pause.elapsed,
                budget,
            }} else { ReserveDeadlineClock::Retired },
            observer,
            yields_to_rebuffer,
            hold_epoch_at_start,
            expired: false,
        }
    }

    /// Remaining reserve in the clock's own units. Unlike [`Self::active`], this deliberately
    /// still returns the balance while the user is paused: callers use it to carry one fixed
    /// end-to-end exploration budget from the control plane into the media leg without turning a
    /// pause into either spent reserve or a newly granted budget.
    pub fn remaining(&self) -> Option<std::time::Duration> {
        match self.clock {
            ReserveDeadlineClock::Retired => None,
            ReserveDeadlineClock::Wall(at) => {
                at.map(|at| at.saturating_duration_since(std::time::Instant::now()))
            }
            ReserveDeadlineClock::Playhead { started_ns, budget } => {
                let spent_ns = self.observer.playpos_ns()?
                    .max(0)
                    .saturating_sub(started_ns);
                let spent =
                    std::time::Duration::from_nanos(u64::try_from(spent_ns).unwrap_or(u64::MAX));
                Some(budget.saturating_sub(spent))
            }
            ReserveDeadlineClock::UnpausedElapsed {
                started_at,
                paused_at_start,
                budget,
            } => {
                let pause = self.observer.pause_clock()?;
                let elapsed = pause.at.saturating_duration_since(started_at);
                let paused = pause.elapsed.saturating_sub(paused_at_start);
                Some(budget.saturating_sub(elapsed.saturating_sub(paused)))
            }
        }
    }

    pub fn release_to_rebuffer(&mut self, rebuffering: bool) {
        if matches!(self.clock, ReserveDeadlineClock::Retired) { return; }
        let Some(hold_epoch) = self.observer.hold_epoch() else {
            self.clock = ReserveDeadlineClock::Retired;
            return;
        };
        let hold_happened = Some(hold_epoch) != self.hold_epoch_at_start;
        if self.yields_to_rebuffer && (rebuffering || hold_happened) {
            self.clock = ReserveDeadlineClock::Wall(None);
        }
    }

    /// Whether the conservation clock itself is spent. A wall projection returned by `active`
    /// is only a wake-up mechanism; it is never evidence for a playhead-funded budget, because
    /// wall time may pass while the native presentation clock is stopped.
    pub fn clock_is_spent(&self) -> bool {
        match self.clock {
            ReserveDeadlineClock::Retired => true,
            ReserveDeadlineClock::Wall(at) => at.is_some_and(|at| std::time::Instant::now() >= at),
            ReserveDeadlineClock::Playhead { started_ns, budget } => {
                let Some(position_ns) = self.observer.playpos_ns() else {
                    return true;
                };
                let spent_ns = position_ns
                    .max(0)
                    .saturating_sub(started_ns);
                let spent =
                    std::time::Duration::from_nanos(u64::try_from(spent_ns).unwrap_or(u64::MAX));
                spent >= budget
            }
            ReserveDeadlineClock::UnpausedElapsed { .. } => {
                self.remaining().is_none_or(|left| left.is_zero())
            }
        }
    }

    pub fn active(&mut self, rebuffering: bool) -> Option<std::time::Instant> {
        self.release_to_rebuffer(rebuffering);
        match self.clock {
            ReserveDeadlineClock::Retired => Some(std::time::Instant::now()),
            ReserveDeadlineClock::Wall(at) => at,
            ReserveDeadlineClock::Playhead { .. }
            | ReserveDeadlineClock::UnpausedElapsed { .. } => {
                let mut left = self.remaining().unwrap_or(std::time::Duration::ZERO);
                if self.observer.paused().unwrap_or(true) {
                    // Pause is published only after the native call succeeds, but the final
                    // playhead sample can land between our first read and that flag. Re-read once
                    // after observing Pause so equality cannot be hidden by the state change.
                    left = self.remaining().unwrap_or(std::time::Duration::ZERO);
                }
                // This wall instant schedules a re-check; it never spends presentation reserve.
                // Keeping that wake armed while paused closes Pause-at-issue -> Resume-in-flight:
                // if the playhead starts moving again, the existing wait still wakes no later
                // than the remaining one-speed balance. If Pause remains, retrospective
                // classification sees an unchanged playhead and retries without renewing the
                // independent transport watchdog.
                if left.is_zero() {
                    return Some(std::time::Instant::now());
                }
                std::time::Instant::now().checked_add(left)
            }
        }
    }

    pub fn expire_if_due(&mut self, rebuffering: bool) -> bool {
        self.release_to_rebuffer(rebuffering);
        if self.expired || self.clock_is_spent() {
            self.expired = true;
            true
        } else {
            false
        }
    }

    /// Record the result of a read started with `attempted_deadline`. `false` means that wall wake
    /// did not spend the clock which owns the reserve. Pause or a naturally stopped playhead may
    /// have made the projection stale; a floor rebuffer hold may instead have released that clock.
    /// The caller retries with whatever reserve remains armed, composed with the unchanged
    /// transport-liveness epoch.
    pub fn note_transport_deadline(
        &mut self,
        _attempted_deadline: Option<std::time::Instant>,
        rebuffering: bool,
    ) -> bool {
        self.release_to_rebuffer(rebuffering);
        // The transport waited on a wall projection. Classify the result only from the clock that
        // owns the budget: Δplayhead >= E for a media reserve, or the actual wall boundary for a
        // legacy wall state. Nanoseconds spent between two `Instant::now()` calls cannot turn a
        // positive presentation balance into censored evidence.
        if self.expired || self.clock_is_spent() {
            self.expired = true;
            true
        } else {
            false
        }
    }
}

/// Independent transport liveness for one blocking HTTP leg. Reserve is presentation-funded and
/// can freeze; liveness is wall-clock inactivity and cannot. The effective wake is their minimum,
/// but only the clock that actually reached its own boundary classifies the result. Progress
/// resets liveness exactly as `SO_RCVTIMEO`/curl low-speed do; an obsolete reserve retry does not.
#[derive(Clone, Copy, Debug)]
pub struct TransportWatchdog {
    pub inactivity: std::time::Duration,
    pub deadline: std::time::Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockingDeadlineOwner {
    /// Reserve wins equality: when both clocks name the same instant and presentation really
    /// spends its balance, the completed observation is exactly reserve-censored.
    Reserve,
    Transport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockingDeadline {
    pub at: std::time::Instant,
    pub owner: BlockingDeadlineOwner,
}

impl TransportWatchdog {
    pub fn for_origin(origin: &Origin) -> Self {
        let inactivity = if origin.is_tls() {
            range_io::media_stall_budget()
        } else {
            plx_net::stream::media_stall_budget()
        };
        Self::with_inactivity(inactivity)
    }

    pub fn with_inactivity(inactivity: std::time::Duration) -> Self {
        Self {
            inactivity,
            deadline: std::time::Instant::now()
                .checked_add(inactivity)
                .unwrap_or_else(std::time::Instant::now),
        }
    }

    pub fn reset(&mut self) {
        self.deadline = std::time::Instant::now()
            .checked_add(self.inactivity)
            .unwrap_or_else(std::time::Instant::now);
    }

    pub fn effective(&self, reserve: Option<std::time::Instant>) -> BlockingDeadline {
        match reserve {
            Some(at) if at <= self.deadline => BlockingDeadline {
                at,
                owner: BlockingDeadlineOwner::Reserve,
            },
            _ => BlockingDeadline {
                at: self.deadline,
                owner: BlockingDeadlineOwner::Transport,
            },
        }
    }

    pub fn expired(&self) -> bool {
        std::time::Instant::now() >= self.deadline
    }
}

/// Facts captured immediately when a typed deadline sentinel returns. Keeping the observation
/// separate from its later use prevents scheduler delay, AQ locking or native-clock catch-up from
/// relabelling the boundary which actually ended this I/O operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockingWake {
    pub owner: BlockingDeadlineOwner,
    pub reserve_spent: bool,
    pub transport_due: bool,
}

pub fn observe_hls_deadline(
    reserve: Option<&mut ReserveDeadlineState>,
    attempted: BlockingDeadline,
    watchdog: &TransportWatchdog,
    rebuffering: bool,
) -> BlockingWake {
    let reserve_spent = attempted.owner == BlockingDeadlineOwner::Reserve
        && reserve.is_some_and(|reserve| {
            reserve.note_transport_deadline(Some(attempted.at), rebuffering)
        });
    BlockingWake {
        owner: attempted.owner,
        reserve_spent,
        transport_due: watchdog.expired(),
    }
}

/// Classify only the captured boundary cause. A transport-owned wake stays a circumstance even
/// if reserve is spent later; that later fact can gate the next I/O but cannot turn an RST/stall
/// into censored rung evidence. Reserve wins an issued equality, but only when its owning clock
/// was actually spent at wake; a stale reserve projection then yields to due liveness.
pub fn classify_hls_deadline(wake: BlockingWake) -> Option<HlsExit> {
    match wake.owner {
        BlockingDeadlineOwner::Transport if wake.transport_due => {
            Some(HlsExit::Failed("HLS transport stalled"))
        }
        BlockingDeadlineOwner::Reserve if wake.reserve_spent => Some(HlsExit::PrimeExpired),
        BlockingDeadlineOwner::Reserve if wake.transport_due => {
            Some(HlsExit::Failed("HLS transport stalled"))
        }
        _ => None,
    }
}

/// Preserve the one exploration clock across its media leg. An upshift starts its playhead clock
/// before PMS registration and the playlist requests; replacing it here with their last wall
/// snapshot made a user Pause expire unspent playable reserve. A downshift's formula is known only
/// after its actual candidate EXTINF and fresh reserve are known, so its media-only clock starts at
/// this boundary and spends unpaused elapsed time: internal starvation is recovery cost, accepted
/// user Pause is not.
pub fn candidate_reserve_deadline(
    exploration: Option<ReserveDeadlineState>,
    media_budget: Option<std::time::Duration>,
    yields_to_rebuffer: bool,
    started_ns: i64,
    clock: TrialClock,
    observer: Arc<dyn DemuxObserver>,
) -> ReserveDeadlineState {
    exploration.unwrap_or_else(|| {
        let unbounded_observer = observer.clone();
        media_budget.map_or_else(
            || ReserveDeadlineState::new(None, yields_to_rebuffer, unbounded_observer),
            |budget| match clock {
                TrialClock::Playhead => {
                    ReserveDeadlineState::from_playhead_at(started_ns, budget,
                        yields_to_rebuffer, observer)
                }
                TrialClock::UnpausedElapsed => {
                    ReserveDeadlineState::from_unpaused_elapsed(budget,
                        yields_to_rebuffer, observer)
                }
            },
        )
    })
}

pub fn exploration_snapshot(
    exploration: &mut Option<ReserveDeadlineState>,
) -> Option<std::time::Instant> {
    exploration
        .as_mut()
        .and_then(|reserve| reserve.active(false))
}

fn exploration_expired(exploration: &mut Option<ReserveDeadlineState>) -> bool {
    exploration
        .as_mut()
        .is_some_and(|reserve| reserve.expire_if_due(false))
}

/// Whether a blocking leg really consumed its playhead-funded exploration reserve. `false` means
/// the leg returned on an obsolete wall-clock snapshot after Pause or a naturally stopped native
/// clock moved the physical boundary; idempotent HTTP legs may retry with a fresh snapshot.
pub fn exploration_timeout_is_final(
    exploration: &mut Option<ReserveDeadlineState>,
    attempted: Option<std::time::Instant>,
) -> bool {
    exploration.as_mut().map_or(true, |reserve| {
        reserve.note_transport_deadline(attempted, false)
    })
}

/// AVIO backing state: wraps the demux transport so libavformat reads through it and can seek by
/// byte offset. Boxed so its address is stable for the C callbacks.
pub struct AvioState {
    pub src: Src,
    pub range: Arc<dyn RangeFactory>,
    pub credential_gate: plx_net::stream::redirect::CredentialGate,
    pub observer: Arc<dyn DemuxObserver>,
    pub aq: *mut AuQueue,
    pub off: i64,
    pub size: i64,
    /// A callback saw a curl I/O failure during the current enclosing libavformat operation.
    /// FFmpeg may heal it through `seek_cb`; only an operation that still returns failure may
    /// publish it to the player.
    pub io_failed: bool,
    /// Time spent inside successful body reads only. Request setup, PMS JIT production, TTFB and
    /// demux/probe work intentionally stay out of this network-rate clock and remain in the
    /// enclosing segment's total acquisition clock.
    pub body_active_us: u64,
    pub body_bytes: u64,
    /// When the FIRST body byte reached the demuxer. Together with the open above it this splits
    /// acquisition into connect / server-think / transfer, which is the only way to see PMS
    /// just-in-time production as its own term: for a JIT encoder the wait before the first byte
    /// IS the production cost, and it is otherwise indistinguishable from a slow link.
    pub first_byte_at: Option<std::time::Instant>,
    /// Only ABR candidate segments carry a reserve deadline. The active movie and every progressive
    /// stream retain their transport's normal stall budgets. A floor downshift may promote to
    /// unbounded-by-reserve while it is in flight; the state latches that transition.
    pub reserve_deadline: ReserveDeadlineState,
    /// Independent wall-clock inactivity for HLS media. Its deadline is composed with the
    /// reserve projection on every blocking read, but only actual bytes reset it. Progressive
    /// sources keep `None` and use their transport's native liveness directly.
    pub transport_watchdog: Option<TransportWatchdog>,
    /// Armed for the ACTIVE cursor whenever its reserve is knowable and the internal clock is not
    /// already held. Its action distinguishes an abort above the ladder floor from a clock-only
    /// hold at the terminal actuator. Candidate transactions use `reserve_deadline` instead: an
    /// ordinary candidate owns a playhead-funded clock projected onto each blocking wait, while a
    /// terminal-floor recovery may omit that reserve clock. A partial PMS response remains
    /// right-censored and cannot prove an earlier completion time from its prefix rate.
    ///
    /// This is the SAME runtime the segment's open and `NotReady` wait consulted, moved here
    /// unchanged; progressive playback has none. It also latches the abort (`stall_aborted`).
    pub acquisition: Option<AcquisitionRuntime>,
    /// Bounded socket/curl overflow while Original is parked in `aq_push`. HLS does not use it:
    /// a segment body is already complete before feed.
    pub bounce: Vec<u8>,
    pub bounce_pos: usize,
    /// `seek_cb` answered a seek to (or past) the known size without fetching, so the transport's
    /// stream is somewhere else and `read_cb` must answer the clean end itself. Any other read at
    /// the end still asks the transport, which is how a failure there stays an error.
    pub parked_at_end: bool,
    /// Maximum silence while healing an incomplete progressive body. Captured
    /// with this AVIO so fixture timing never changes production behavior.
    pub body_silence: std::time::Duration,
    /// Reopens of a cut direct-play body that brought nothing back. See [`HealBudget`].
    pub heal: HealBudget,
}

/// Reopens [`read_cb`] has spent at one byte of a cut body, across calls: libavformat retries a
/// failed read with a fresh call, so a budget per call let a cut no reopen crosses be reopened
/// round after round and then read as the end of the file. A byte read refills it.
#[derive(Clone, Copy, Default)]
pub struct HealBudget {
    pub at: i64,
    pub spent: u8,
}

impl HealBudget {
    /// Charge one reopen at byte `at`, or `false` when the budget there is spent.
    pub fn take(&mut self, at: i64) -> bool {
        if self.at != at {
            *self = HealBudget { at, spent: 0 };
        }
        if self.spent >= BODY_HEAL_ATTEMPTS {
            return false;
        }
        self.spent += 1;
        true
    }
}

// TEST ONLY: a deterministic stand-in for the two `Instant::elapsed()` calls that measure
// `body_active_us` — `AvioState::note_received`'s receipt timer and `read_cb`'s read timer. A
// real scheduler can preempt either interval independently, so a test that compares
// `body_active_us` to a wall-clock read can flake on a loaded runner even though the accounting
// is correct (see `ff_stall_guard_tests::transfer_work_a_receipt_takes_is_counted_in_body_time`,
// pre-seam). Tests push exact durations here and assert exact sums instead. Empty queue (every
// non-test build, and every test that never pushes) falls straight through to the real clock, so
// this changes no production behaviour.
#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static TEST_ELAPSED: std::cell::RefCell<std::collections::VecDeque<std::time::Duration>> =
        std::cell::RefCell::new(std::collections::VecDeque::new());
}

#[cfg(any(test, feature = "test-support"))]
pub fn push_test_elapsed(duration: std::time::Duration) {
    TEST_ELAPSED.with(|queue| queue.borrow_mut().push_back(duration));
}

/// Drain and return whatever a test left queued, so one test's leftover injection can never leak
/// into the next one sharing this OS thread.
#[cfg(any(test, feature = "test-support"))]
pub fn drain_test_elapsed() -> Vec<std::time::Duration> {
    TEST_ELAPSED.with(|queue| queue.borrow_mut().drain(..).collect())
}

/// Elapsed time since `started`: the real `Instant::elapsed()`, except under test when a duration
/// is queued — then that queued value, once, so a test can assert `body_active_us` exactly rather
/// than by a wall-clock ratio.
fn elapsed_since(started: std::time::Instant) -> std::time::Duration {
    #[cfg(any(test, feature = "test-support"))]
    {
        if let Some(queued) = TEST_ELAPSED.with(|queue| queue.borrow_mut().pop_front()) {
            return queued;
        }
    }
    started.elapsed()
}

impl AvioState {
    /// **Fold the AU lane's abort into the transport's own.**
    ///
    /// There are two abort signals in the media path and they arrive by different routes. The AU
    /// lane's flag (`aq_is_aborted`) is what these callbacks have always checked on entry, and it
    /// is set by every stopper — teardown, and `start_bufferfeed`'s early return when the media
    /// thread will not spawn. `curlio`'s wake pipe is what reaches a thread already BLOCKED inside
    /// `curl_multi_wait`, which the AU flag cannot do; teardown fires it separately.
    ///
    /// This is the join between them, in the one place both are visible: once the lane is aborted,
    /// the curl source is latched too, so it refuses on its own terms afterwards rather than
    /// depending on every future caller re-checking the lane. The socket source needs no
    /// equivalent — closing it IS the latch, and the engine owns that.
    fn latch_abort(&self) {
        if let Src::Curl(cs) = &self.src {
            cs.abort();
        }
    }

    fn clear_bounce(&mut self) {
        self.bounce.clear();
        self.bounce_pos = 0;
    }

    fn take_bounce(&mut self, dst: &mut [u8]) -> usize {
        let avail = self.bounce.len().saturating_sub(self.bounce_pos);
        if avail == 0 || dst.is_empty() {
            return 0;
        }
        let n = avail.min(dst.len());
        dst[..n].copy_from_slice(&self.bounce[self.bounce_pos..self.bounce_pos + n]);
        self.bounce_pos += n;
        if self.bounce_pos == self.bounce.len() {
            self.clear_bounce();
        }
        n
    }

    pub fn bounce_remaining(&self) -> bool {
        self.bounce_pos < self.bounce.len()
    }

    /// **Credit the acquisition with every body byte already RECEIVED**, not merely read: what
    /// FFmpeg consumed (`off`), plus the park drain's bounce, plus what the transport holds ahead
    /// (`Src::body_receipt`). PMS pauses a sized response and then bursts the remainder; once
    /// that burst is in hand, a hold must not abandon it. An unsized body completes only on the
    /// transport's proven end. Called before every guard decision `read_cb` makes.
    ///
    /// A receipt that took a transfer step received body bytes the later read only copies out, so
    /// the step's time is counted here, once, as body time — the same accounting those bytes
    /// would have had had the read itself received them. Nothing else in the query is body work.
    fn note_received(&mut self) {
        if self.acquisition.is_none() {
            return;
        }
        let asked = std::time::Instant::now();
        let receipt = self.src.body_receipt();
        if receipt.stepped {
            self.body_active_us = self
                .body_active_us
                .saturating_add(elapsed_since(asked).as_micros().max(1) as u64);
        }
        let bounced = self.bounce.len().saturating_sub(self.bounce_pos) as i64;
        let received = self
            .off
            .saturating_add(bounced)
            .saturating_add(receipt.ahead);
        if let Some(acquisition) = &mut self.acquisition {
            acquisition.note_body(self.size, received, receipt.finished);
        }
    }

    fn transfer_finished(&self) -> bool {
        match &self.src {
            Src::Idle => true,
            Src::Socket { hs, .. } => plx_net::stream::http_body_done(*hs),
            Src::Curl(cs) => cs.body_complete(),
        }
    }

    /// Whether a cut at the byte reached may be healed by reopening there: a direct-play
    /// (no acquisition, so not an HLS segment) plaintext socket with a known length and `off`
    /// still inside it. Unsized transcodes cannot be reopened at a byte, curl heals on its own
    /// terms, and a position at or past the end is a position, not a cut. One predicate for every
    /// heal site, so the drain path cannot drift from the read path.
    fn may_heal_at_offset(&self) -> bool {
        self.acquisition.is_none()
            && self.size > 0
            && self.off >= 0
            && self.off < self.size
            && matches!(self.src, Src::Socket { .. })
    }

    /// Drain the live media socket into [`Self::bounce`] without blocking. Called from
    /// [`crate::aq::aq_push_with_drain`] so Original's TCP window stays open while the AU
    /// queues are full. Recv time is network work; the park wait around it is not.
    ///
    /// Returns whether the park should poll again soon: `true` only while bounce has room and
    /// more body is still expected. Cap, abort, I/O fail, Idle, or a finished body `cond_wait`.
    /// A long pause at CAP can let the server keep-alive timer FIN the socket; reuse already
    /// redials on `Transport`. Do not raise CAP.
    pub fn drain_wire(&mut self) -> bool {
        const CAP: usize = 384 * 1024;
        if matches!(self.src, Src::Idle) {
            return false;
        }
        if unsafe { crate::aq::aq_is_aborted(self.aq) } {
            self.clear_bounce();
            return false;
        }
        if self.bounce_pos > 0 {
            self.bounce.drain(..self.bounce_pos);
            self.bounce_pos = 0;
        }
        if self.bounce.len() >= CAP {
            return false;
        }
        loop {
            if unsafe { crate::aq::aq_is_aborted(self.aq) } {
                self.clear_bounce();
                return false;
            }
            if self.bounce.len() >= CAP {
                return false;
            }
            let start = self.bounce.len();
            self.bounce.resize(start + (CAP - start), 0);
            let read_started = std::time::Instant::now();
            let n = match &mut self.src {
                Src::Socket { hs, .. } => {
                    plx_net::stream::http_drain_available(*hs, &mut self.bounce[start..])
                }
                Src::Curl(cs) => cs.drain_available(&mut self.bounce[start..]),
                Src::Idle => 0,
            };
            if n > 0 {
                self.bounce.truncate(start + n as usize);
                self.body_active_us = self
                    .body_active_us
                    .saturating_add(read_started.elapsed().as_micros().max(1) as u64);
                if self.first_byte_at.is_none() {
                    self.first_byte_at = Some(std::time::Instant::now());
                }
                if !self.observer.publish(DemuxCallbackEffect::NetworkBytes(n as i64)) {
                    self.io_failed = true;
                    return false;
                }
                continue;
            }
            self.bounce.truncate(start);
            if n < 0 {
                self.io_failed = true;
                return false;
            }
            return drain_keep_polling(self.bounce.len() < CAP, self.transfer_finished());
        }
    }
}

pub fn drain_keep_polling(bounce_under_cap: bool, body_finished: bool) -> bool {
    bounce_under_cap && !body_finished
}

pub fn hls_should_prefetch_after_abr(delivered: bool, fetch_abandoned: bool, stay: bool) -> bool {
    delivered && !fetch_abandoned && stay
}

pub fn park_read_fails_closed(bounce_remaining: bool, io_failed: bool) -> bool {
    io_failed && !bounce_remaining
}

/// Settle one enclosing FFmpeg operation against the exact callback state it accumulated.
///
/// Abort is checked by the caller immediately after the native call and therefore has priority
/// outside this helper. Of the remaining transaction terminals, deliberate active-stream stall
/// and a spent reserve outrank generic I/O; callback I/O is published only when FFmpeg itself did
/// not recover the operation. Using this one classifier for open, stream discovery and frame read
/// prevents the same callback event from changing meaning with the FFmpeg phase it happened in.
fn classify_hls_avio_operation(
    state: &AvioState,
    operation_failed: bool,
    request_started: std::time::Instant,
    audio_expected: bool,
) -> Option<HlsExit> {
    let stall_aborted = state
        .acquisition
        .as_ref()
        .is_some_and(AcquisitionRuntime::stall_aborted);
    let stall = stall_aborted.then(|| SegmentTransfer {
        bytes: state.body_bytes,
        active_us: state.body_active_us,
        total_us: request_started.elapsed().as_micros().max(1) as u64,
        audio_expected,
    });
    classify_hls_avio_facts(
        stall,
        state.reserve_deadline.expired,
        state.io_failed,
        operation_failed,
    )
}

/// Pure terminal reduction for one completed native operation. The callback records facts while
/// libavformat owns the thread; this reducer is the state-machine edge taken when control returns.
/// Keeping the priority here makes `open`, `find_stream_info` and `read_frame` observe the same
/// transition table, and lets the table be tested without constructing an FFmpeg AVIO context.
pub fn classify_hls_avio_facts(
    stall: Option<SegmentTransfer>,
    reserve_expired: bool,
    io_failed: bool,
    operation_failed: bool,
) -> Option<HlsExit> {
    if let Some(transfer) = stall {
        Some(HlsExit::StallAbort(transfer))
    } else if reserve_expired {
        Some(HlsExit::PrimeExpired)
    } else if operation_failed && io_failed {
        Some(HlsExit::Failed("segment body transport failed"))
    } else {
        None
    }
}

/// `read_cb`'s answer once its acquisition runtime has stopped. Teardown ends the operation as it
/// always has; a latched stall abort is `AVERROR_EOF` with `stall_aborted` for
/// `classify_hls_avio_operation` to report. A stop with no latched reason cannot come from the
/// runtime; it is reported as the transport failure it would otherwise be mistaken for.
fn avio_stopped(s: &mut AvioState) -> c_int {
    if unsafe { crate::aq::aq_is_aborted(s.aq) } {
        s.latch_abort();
        s.clear_bounce();
        return AVERROR_EOF;
    }
    if s.acquisition
        .as_ref()
        .is_some_and(AcquisitionRuntime::stall_aborted)
    {
        return AVERROR_EOF;
    }
    s.io_failed = true;
    AVERROR_IO
}

/// Reopens [`read_cb`] tries at one byte, after the transfer is cut mid-body, before the cut is a
/// transport failure.
pub const BODY_HEAL_ATTEMPTS: u8 = 3;

/// How long a direct-play socket read waits for a byte before the transfer counts as cut.
const BODY_SILENCE: std::time::Duration = std::time::Duration::from_secs(15);

/// Reopen the source at the byte reached, through the same path a seek takes.
fn heal_at_offset(s: &mut AvioState) -> bool {
    let at = s.off;
    plx_base::eventlog::log(&format!("ff: body ended early at byte {at} of {}, reopening there", s.size));
    seek_cb(s as *mut AvioState as *mut c_void, at, SEEK_SET) == at
}

pub extern "C" fn read_cb(op: *mut c_void, dst: *mut u8, n: c_int) -> c_int {
    unsafe {
        let s = &mut *(op as *mut AvioState);
        loop {
            // interrupt: bail out of a blocked read on teardown (aborted) only. A seek does NOT
            // interrupt the read — the demux thread services it itself between two av_read_frame
            // calls (see the read loop), so there is nothing to unblock and nothing to race.
            if crate::aq::aq_is_aborted(s.aq) {
                s.latch_abort();
                s.clear_bounce();
                return AVERROR_EOF;
            }
            let bounced = s.take_bounce(std::slice::from_raw_parts_mut(dst, n.max(0) as usize));
            if bounced > 0 {
                s.off += bounced as i64;
                s.body_bytes = s.body_bytes.saturating_add(bounced as u64);
                return bounced as c_int;
            }
            if park_read_fails_closed(s.bounce_remaining(), s.io_failed) {
                // The failure was parked by `drain_wire` (the demuxer waits in
                // `aq_push_with_drain` almost all the time, so it is the drain that meets a LAN
                // reset or an early close), not by this read. The bounce is empty, so `s.off` is
                // the first byte not yet delivered: reopen there as a cut read would. `seek_cb`
                // clears `io_failed` on success and refuses a reopen that is not a 206 from the
                // byte; the per-offset budget bounds a server that keeps cutting.
                if s.may_heal_at_offset() && s.heal.take(s.off) && heal_at_offset(s) {
                    continue;
                }
                return AVERROR_IO;
            }
            // The abort rule, evaluated where the numbers are — through the same runtime check
            // the open and the transport waits consult. See `StallGuard`.
            s.note_received();
            if let Some(acquisition) = &mut s.acquisition {
                if plx_base::checkpoint::Checkpoint::check(acquisition)
                    == plx_base::checkpoint::Flow::Stop
                {
                    return avio_stopped(s);
                }
            }
            let Some(rebuffering) = s.observer.rebuffering() else {
                return AVERROR_EOF;
            };
            if s.reserve_deadline.expire_if_due(rebuffering) {
                return AVERROR_EOF;
            }
            let reserve_snapshot = s.reserve_deadline.active(rebuffering);
            let blocking_deadline = s
                .transport_watchdog
                .map(|watchdog| watchdog.effective(reserve_snapshot))
                .or_else(|| {
                    reserve_snapshot.map(|at| BlockingDeadline {
                        at,
                        owner: BlockingDeadlineOwner::Reserve,
                    })
                });
            // A direct-play socket read has no other clock, so a server that stops sending without
            // closing would block it forever. Silence this long is answered like a cut: a reopen.
            // Sized bodies only: a progressive transcode (`start.mkv`) has no length, may be
            // silent that long while the PMS catches up, and cannot be reopened at a byte (its
            // restart would answer 200 from the head). It keeps the pre-heal behaviour.
            let direct_play = s.acquisition.is_none();
            let silence_deadline = (s.may_heal_at_offset() && blocking_deadline.is_none()).then(|| std::time::Instant::now() + s.body_silence);
            let live_deadline = blocking_deadline.map(|deadline| deadline.at).or(silence_deadline);
            // Both sources use the same three-way return — >0 bytes, 0 clean end, <0 error — so
            // the EOF decision below stays one branch rather than one per transport.
            let read_started = std::time::Instant::now();
            let mut unarmed = plx_base::checkpoint::NoCheckpoint;
            let checkpoint: &mut dyn plx_base::checkpoint::Checkpoint = match &mut s.acquisition {
                Some(acquisition) => acquisition,
                None => &mut unarmed,
            };
            let r = match &mut s.src {
                // Parked at the end by `seek_cb`: the clean end, without asking a transport whose
                // stream sits elsewhere.
                _ if s.parked_at_end => 0,
                Src::Socket { hs, .. } => plx_net::stream::http_read_until(
                    *hs,
                    dst as *mut c_uchar,
                    n,
                    live_deadline,
                    checkpoint,
                ),
                // The null/length guard `stream::http_read` does for itself.
                Src::Curl(_) if dst.is_null() || n <= 0 => 0,
                Src::Curl(cs) => cs.read_until(
                    std::slice::from_raw_parts_mut(dst, n as usize),
                    live_deadline,
                    checkpoint,
                ),
                Src::Idle => return AVERROR_EOF,
            };
            let wake =
                if r == plx_net::stream::HTTP_READ_DEADLINE || r == range_io::READ_DEADLINE {
                    let Some(current_rebuffering) = s.observer.rebuffering() else {
                        return AVERROR_EOF;
                    };
                    match (blocking_deadline, s.transport_watchdog.as_ref()) {
                        (Some(attempted), Some(watchdog)) => Some(observe_hls_deadline(
                            Some(&mut s.reserve_deadline),
                            attempted,
                            watchdog,
                            current_rebuffering,
                        )),
                        (Some(attempted), None) => Some(BlockingWake {
                            owner: attempted.owner,
                            reserve_spent: s
                                .reserve_deadline
                                .note_transport_deadline(Some(attempted.at), current_rebuffering),
                            transport_due: false,
                        }),
                        _ => None,
                    }
                } else {
                    None
                };
            // Teardown may have arrived while the transport was blocked. Its wake commonly
            // surfaces as socket EOF or curl failure; classify from the owning AU abort flag
            // before either transport result can become an active-stream error.
            if crate::aq::aq_is_aborted(s.aq) {
                s.latch_abort();
                return AVERROR_EOF;
            }
            // Every unsuccessful read is SETTLED by a fresh check before it is classified: a
            // deadline shorter than the checkpoint slice ends the wait without asking again, and a
            // stopped wait has already latched why. What the transport received during the wait
            // is credited first, so the fresh check sees the same completion the read would have.
            if r < 0 {
                s.note_received();
                if let Some(acquisition) = &mut s.acquisition {
                    if acquisition.settle_stopped() {
                        return avio_stopped(s);
                    }
                }
            }
            if r == plx_net::stream::HTTP_READ_STOPPED || r == range_io::READ_STOPPED {
                return avio_stopped(s);
            }
            if let Some(wake) = wake {
                match classify_hls_deadline(wake) {
                    Some(HlsExit::PrimeExpired) => return AVERROR_EOF,
                    Some(_) => {
                        s.io_failed = true;
                        return AVERROR_IO;
                    }
                    None => {}
                }
                // A stale reserve projection is neither evidence nor progress. Retry in this
                // frame (not recursively) with the same inactivity epoch and a fresh projection.
                continue;
            }
            if r < 0 {
                // A server that drops the connection mid-body (a PMS that reaps the socket while
                // the viewer is paused, a reset from the LAN) is answered by reopening at the byte
                // we stopped at. libavformat only heals through `seek_cb` when the demuxer itself
                // seeks; a plain read failure reaches `av_read_frame` as EIO and ends the session,
                // which is how pausing for the subtitle menu stopped playback on a webOS 3.4 set.
                // Stall timeouts (rc=28) stay with the stall guard; only cut transfers heal here.
                // Direct play only: an HLS segment read carries an acquisition runtime whose
                // checkpoint and deadlines bound the transfer, but `cs.seek` reopens through
                // `start()` with neither, and a failed segment is the HLS retry policy's to answer.
                // The plaintext socket heals sized bodies only (see the silence deadline above).
                if s.may_heal_at_offset() && s.heal.take(s.off) && heal_at_offset(s) {
                    continue;
                }
                if let Src::Curl(cs) = &mut s.src {
                    let cut = cs
                        .body_failure()
                        .is_some_and(|f| matches!(f.rc, 18 | 55 | 56 | 92));
                    let inside = s.off >= 0 && (s.size < 0 || s.off < s.size);
                    if direct_play && cut && inside && s.heal.take(s.off) {
                        if cs.seek(s.off) {
                            continue;
                        }
                    }
                }
                // Teardown was handled above through the AU flag and remains EOF. A negative
                // result here is therefore a real transport/range failure.
                s.io_failed = true;
                return AVERROR_IO;
            }
            // A clean close before the length is a cut too (a server that ends the response early
            // rather than resetting it): reopen at the byte reached. One that no reopen crosses is
            // still not the end of the file: it is the transfer failing, for the pump to answer.
            if r == 0 && direct_play && s.size > 0 && s.off >= 0 && s.off < s.size {
                if s.heal.take(s.off) && heal_at_offset(s) {
                    continue;
                }
                plx_base::eventlog::log(&format!(
                    "ff: body cut at byte {} of {} and no reopen crossed it; transport failure, not EOF",
                    s.off, s.size
                ));
                s.io_failed = true;
                return AVERROR_IO;
            }
            if r == 0 {
                if let Some(acquisition) = &mut s.acquisition {
                    acquisition.note_body(s.size, s.off, true);
                }
                return AVERROR_EOF;
            }
            if let Some(watchdog) = &mut s.transport_watchdog {
                watchdog.reset();
            }
            s.heal = HealBudget::default();
            s.body_active_us = s
                .body_active_us
                .saturating_add(elapsed_since(read_started).as_micros().max(1) as u64);
            if s.first_byte_at.is_none() {
                s.first_byte_at = Some(std::time::Instant::now());
            }
            s.body_bytes = s.body_bytes.saturating_add(r as u64);
            s.off += r as i64;
            if !s.observer.publish(DemuxCallbackEffect::NetworkBytes(r as i64)) {
                s.io_failed = true;
                return -1;
            }
            return r;
        }
    }
}

pub extern "C" fn seek_cb(op: *mut c_void, offset: i64, whence: c_int) -> i64 {
    unsafe {
        let s = &mut *(op as *mut AvioState);
        if whence == AVSEEK_SIZE {
            return s.size;
        }
        // Teardown may have raced us here — the same race the reopen site in `demux` guards, and
        // this is the third way in. Teardown aborts the lanes, fires ONE `http_shutdown` at this
        // socket, then JOINS this thread from the main thread. Our AVIO is SEEKABLE, so
        // libavformat treats the broken read as recoverable and heals it by calling US (the
        // 5938b5f mechanism, spelled out in the read loop's note below). Without this check it
        // gets a BRAND NEW connection the already-fired shutdown cannot touch — and because
        // `read_cb` then reports EOF again, that recovery repeats: measured on the host at ONE
        // full `http_open` per hop, so the main thread's join is held for a whole reopen every
        // time round, not just once. `read_cb` has bailed on this flag since it was written; a
        // seek was the way around it.
        //
        // Placed AFTER the AVSEEK_SIZE branch on purpose: a size query is a field read, not I/O,
        // and answering it during teardown costs nothing.
        if crate::aq::aq_is_aborted(s.aq) {
            s.latch_abort();
            s.clear_bounce();
            return -1;
        }
        s.clear_bounce();
        let target = match whence {
            SEEK_SET => offset,
            SEEK_CUR => s.off + offset,
            SEEK_END => s.size + offset,
            _ => return -1,
        };
        if target < 0 {
            return -1;
        }
        // **A seek to the end is a position, not a request.** libavformat restores an
        // `avio_tell` taken at EOF this way — `mov_read_chapters` does it after reading a chapter
        // track, and a moov-at-end MP4 leaves the AVIO at exactly the size — and the only answer
        // a server can give `Range: bytes=<size>-` is 416. Reopening for it closed the live
        // stream and failed, while libavformat's buffer still held the chapter samples: the first
        // video sample just past them read a dead transport ("partial file"), and the player sat
        // at t=0. Nothing here is fetched, so nothing is closed; `read_cb` answers EOF while
        // `parked_at_end`, and the next seek into the file reopens as it always has
        // (`ff_eof_seek_tests.rs`). An unsized source has no end to compare with and still asks.
        if s.size >= 0 && target >= s.size {
            s.off = target;
            s.io_failed = false;
            s.parked_at_end = true;
            return target;
        }
        s.parked_at_end = false;
        let aq = s.aq;
        let sized = s.size > 0;
        let mut hopped_to_tls = None;
        let reopen_began = std::time::Instant::now();
        let ok = match &mut s.src {
            // Redirects are followed here too (`stream::redirect`), and the effective target is
            // written back so the NEXT seek starts from it rather than replaying the chain. A hop
            // to https cannot stay on this socket; it becomes the curl source below.
            Src::Socket {
                hs,
                host,
                port,
                path,
            } => {
                plx_net::stream::http_close(*hs);
                let origin = Origin::http(&host.to_string_lossy(), *port);
                let from = path.to_string_lossy().into_owned();
                let req = plx_net::stream::redirect::Request {
                    origin: &origin,
                    path: &from,
                    credentials: None,
                    range_from: Some(target),
                    deadline: None,
                    same_origin_only: false,
                    credential_gate: s.credential_gate,
                };
                match plx_net::stream::redirect::open_following(
                    *hs,
                    &req,
                    &mut plx_base::checkpoint::NoCheckpoint,
                ) {
                    Ok(plx_net::stream::redirect::Opened::Socket(t)) => {
                        if let (Ok(h), Ok(p)) =
                            (CString::new(t.origin.host()), CString::new(t.path))
                        {
                            *host = h;
                            *port = t.origin.port() as c_int;
                            *path = p;
                        }
                        // `stream.rs` takes any 2xx, so a server that ignored the Range answers
                        // 200 from byte 0 and would be spliced in as the bytes at `target`:
                        // corruption that looks like success (the curl arm refuses it below). A
                        // sized body knows where it is going, so it is held to the same rule: a
                        // 206 that starts at the byte asked for, or a 200 for a request from 0.
                        // An unsized body (a transcode) has no such position to hold it to.
                        let start = plx_net::stream::hs_range_start(*hs);
                        let status = plx_net::stream::hs_status(*hs);
                        let crossed = !sized
                            || (status == 206 && start == target)
                            || (status == 200 && target == 0);
                        if !crossed {
                            plx_base::eventlog::log(&format!(
                                "ff: seek_cb reopen refused: status={status} range_start={start} wanted={target}"
                            ));
                            plx_net::stream::http_close(*hs);
                        }
                        crossed
                    }
                    Ok(plx_net::stream::redirect::Opened::Tls(t)) => {
                        match open_curl_hop(s.range.as_ref(), &t.url(), target, aq) {
                            Ok(cs) => {
                                hopped_to_tls = Some(cs);
                                true
                            }
                            Err(e) => {
                                plx_base::eventlog::log(&format!("ff: seek https hop FAILED: {e:?}"));
                                false
                            }
                        }
                    }
                    Err(_) => false,
                }
            }
            // `curlio` REFUSES a Range the server answered with a 200, where `stream.rs` accepts
            // any 2xx. That is the one behavioural difference between these two arms, and it is
            // deliberate: bytes from the head of the file, delivered as though they were the bytes
            // at `target`, are corruption that looks like success.
            Src::Curl(cs) => cs.seek(target),
            Src::Idle => false,
        };
        if let Some(cs) = hopped_to_tls {
            s.src = Src::Curl(cs);
        }
        // One line per reopen (a seek into the file, or libavformat healing a failed read).
        plx_base::eventlog::log(&format!(
            "ff: seek_cb reopen off={target} ok={ok} ms={}",
            reopen_began.elapsed().as_millis()
        ));
        if !ok {
            return -1;
        }
        s.off = target;
        // This may be libavformat healing a failed read. A source validated at the requested byte
        // has recovered; a later callback failure will arm the bit again.
        s.io_failed = false;
        target
    }
}

/// Settle one `av_read_frame` result against errors observed by its AVIO callbacks.
pub fn frame_read_failed(state: &mut AvioState, result: c_int) -> bool {
    if result >= 0 {
        return false;
    }
    if state.io_failed {
        // Issue #504: `r=-5` alone could not tell a proxy's cut from a reset from a stall. The
        // curl source remembers why its transfer died (until a reopen recovers), so the line
        // names it, and the same facts are published for the handled playback error.
        let failure = match &state.src {
            Src::Curl(cs) => cs.body_failure(),
            _ => None,
        };
        let cause = failure.map_or_else(String::new, |f| {
            format!(
                " curl_rc={}{}",
                f.rc,
                if f.reopen_failed { " reopen=failed" } else { "" }
            )
        });
        plx_base::eventlog::log(&format!(
            "ff: media transport failed during av_read_frame r={result}{cause}"
        ));
        if let Some(f) = failure {
            state.observer.publish(DemuxCallbackEffect::TransportFailure {
                rc: f.rc,
                age_ms: f.age_ms.min(i64::MAX as u64) as i64,
                reopen_failed: f.reopen_failed,
            });
        } else {
            state.observer.publish(DemuxCallbackEffect::IoFailed);
        }
    }
    true
}

use plx_media_codecs::subtitle_extract::{pts_ns, pts_ns_opt};

// -- AV1 framing ------------------------------------------------------------------------
//
// The TV pipeline is handed no codec_data for AV1 (libpf's AV1 caps carry none, and the pipeline
// has no av1parse element), so the configuration has to travel in-band. These helpers turn the
// demuxed packet into a Section-5 temporal unit: a temporal delimiter first, the av1C config OBUs
// in front of each keyframe, and every OBU carrying a size field. Everything here is pure, so
// the framing is tested on the host (ff_packet_framing_tests.rs).

const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_TEMPORAL_DELIMITER: u8 = 2;
const OBU_METADATA: u8 = 5;
/// A temporal delimiter OBU with an empty payload: header 0x12 (type 2, size field), size 0.
pub const AV1_TD: [u8; 2] = [0x12, 0x00];

/// Read one leb128 value from the front of `b` (AV1 section 4.10.5), returning the value and the
/// number of bytes it took. A run with no terminator inside eight bytes, a truncated run, and a
/// value wider than 32 bits are all rejected: the spec bounds every leb128 field to 32 bits.
pub fn read_leb128(b: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for (i, &byte) in b.iter().take(8).enumerate() {
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return (value <= u64::from(u32::MAX)).then_some((value, i + 1));
        }
    }
    None
}

/// Append `v` as a minimal leb128 encoding.
pub fn write_leb128(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// One OBU inside a byte run: where its header starts, how long the header is (one byte, plus
/// one when the extension flag is set), where its payload starts, and where it ends (exclusive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObuSpan {
    start: usize,
    header_len: usize,
    payload_start: usize,
    end: usize,
    obu_type: u8,
    has_size: bool,
}

/// Split an OBU run into its OBUs. An OBU without a size field runs to the end of the run and
/// is therefore the last one. Returns None for a set forbidden bit, a truncated header or size,
/// or a payload that overruns the run.
fn walk_obus(b: &[u8]) -> Option<Vec<ObuSpan>> {
    let mut spans = Vec::new();
    let mut p = 0usize;
    while p < b.len() {
        let h = b[p];
        if h & 0x80 != 0 {
            return None;
        }
        let obu_type = (h >> 3) & 0x0f;
        let header_len = 1 + usize::from((h >> 2) & 1);
        let has_size = (h >> 1) & 1 == 1;
        if p + header_len > b.len() {
            return None;
        }
        let body = p + header_len;
        let (payload_start, end) = if has_size {
            let (size, n) = read_leb128(&b[body..])?;
            let payload_start = body + n;
            // u64 arithmetic: on the 32-bit TV `payload_start + size` could wrap a usize.
            if payload_start as u64 + size > b.len() as u64 {
                return None;
            }
            (payload_start, payload_start + size as usize)
        } else {
            (body, b.len())
        };
        spans.push(ObuSpan {
            start: p,
            header_len,
            payload_start,
            end,
            obu_type,
            has_size,
        });
        p = end;
    }
    Some(spans)
}

/// Append one OBU with a size field. An OBU that already has one is copied verbatim; a sizeless
/// one (only ever the last in its run) gets the minimal leb128 of its payload length.
fn push_obu_sized(b: &[u8], s: &ObuSpan, out: &mut Vec<u8>) {
    if s.has_size {
        out.extend_from_slice(&b[s.start..s.end]);
        return;
    }
    out.push(b[s.start] | 0x02);
    if s.header_len == 2 {
        out.push(b[s.start + 1]);
    }
    write_leb128((s.end - s.payload_start) as u64, out);
    out.extend_from_slice(&b[s.payload_start..s.end]);
}

/// The av1C configOBUs to insert in front of a keyframe: every sequence header, then every
/// metadata OBU (HDR10 MDCV/CLL may live only in the record). Empty when the record carries no
/// sequence header, because metadata alone is never inserted. Accepts both the av1C record
/// (marker and version byte 0x81, three bytes of fixed fields, then the OBUs) and a bare OBU run.
pub fn parse_av1c(ed: &[u8]) -> Vec<u8> {
    let obus = if ed.len() >= 4 && ed[0] == 0x81 {
        &ed[4..]
    } else if !ed.is_empty() && ed[0] & 0x80 == 0 {
        ed
    } else {
        return Vec::new();
    };
    let Some(spans) = walk_obus(obus) else {
        return Vec::new();
    };
    if !spans.iter().any(|s| s.obu_type == OBU_SEQUENCE_HEADER) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for s in spans.iter().filter(|s| s.obu_type == OBU_SEQUENCE_HEADER) {
        push_obu_sized(obus, s, &mut out);
    }
    for s in spans.iter().filter(|s| s.obu_type == OBU_METADATA) {
        push_obu_sized(obus, s, &mut out);
    }
    out
}

/// Which pieces of the AV1 temporal-unit framing are emitted. DEFAULT is the conformant form.
/// The other settings exist so the reporter can A/B the Realtek decoder's expectations on a
/// device; they come from the `av1obu` dev trigger and are never set in a release build.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Av1Framing {
    /// Prepend a temporal delimiter when the packet has none.
    pub td: bool,
    /// Prepend the av1C config block on keyframes that carry no sequence header.
    pub seq: bool,
    /// Pass the packet bytes through exactly as demuxed.
    pub raw: bool,
}

impl Av1Framing {
    const DEFAULT: Av1Framing = Av1Framing {
        td: true,
        seq: true,
        raw: false,
    };

    /// Parse the `av1obu` trigger. Tokens are separated by commas or whitespace; each one sets
    /// one flag independently, and unknown tokens are ignored.
    fn from_trigger(s: Option<&str>) -> Av1Framing {
        let mut f = Self::DEFAULT;
        for tok in s
            .unwrap_or("")
            .split(|c: char| c == ',' || c.is_ascii_whitespace())
            .filter(|t| !t.is_empty())
        {
            match tok {
                "notd" => f.td = false,
                "noseq" => f.seq = false,
                "raw" => f.raw = true,
                _ => {}
            }
        }
        f
    }
}

/// Frame one demuxed AV1 packet as a temporal unit in `out`, returning `key` unchanged.
/// A packet that does not parse as OBUs is copied through untouched: it is never dropped.
pub fn av1_packet_to_tu(pkt: &[u8], key: bool, cfg: &[u8], f: Av1Framing, out: &mut Vec<u8>) -> bool {
    out.clear();
    let spans = match walk_obus(pkt) {
        Some(spans) if !f.raw => spans,
        _ => {
            out.extend_from_slice(pkt);
            return key;
        }
    };
    let has_td = spans
        .first()
        .is_some_and(|s| s.obu_type == OBU_TEMPORAL_DELIMITER);
    let has_seq = spans.iter().any(|s| s.obu_type == OBU_SEQUENCE_HEADER);
    let mut rest = &spans[..];
    if has_td {
        push_obu_sized(pkt, &spans[0], out);
        rest = &spans[1..];
    } else if f.td {
        out.extend_from_slice(&AV1_TD);
    }
    if key && f.seq && !has_seq && !cfg.is_empty() {
        out.extend_from_slice(cfg);
    }
    if let Some((last, body)) = rest.split_last() {
        for s in body {
            out.extend_from_slice(&pkt[s.start..s.end]);
        }
        push_obu_sized(pkt, last, out);
    }
    key
}

/// How a demuxed video packet is turned into the access unit the television's pipeline is fed:
/// the one place a codec id is mapped to a framing, so a codec with no arm is REFUSED instead of
/// falling into the Annex-B path (which on a bitstream that is not H.264/HEVC produces garbage the
/// decoder rejects with nothing in the log naming the cause).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoFraming {
    /// NAL units rewritten to Annex-B with the parameter sets prepended at each keyframe. How the
    /// packet delimits them is [`NalInput`]: a length before each NAL (`packet_to_annexb`), or start
    /// codes already (`annexb_h264_access_unit`, H.264 only).
    AnnexB { hevc: bool },
    /// Section-5 temporal units with the av1C configuration in-band (`av1_packet_to_tu`).
    Av1,
    /// The packet unchanged (`vp9_packet_to_frame`).
    Vp9,
}

impl VideoFraming {
    pub(crate) fn of_codec_id(id: c_int) -> Option<Self> {
        match id {
            AV_CODEC_ID_H264 => Some(Self::AnnexB { hevc: false }),
            AV_CODEC_ID_HEVC => Some(Self::AnnexB { hevc: true }),
            AV_CODEC_ID_AV1 => Some(Self::Av1),
            AV_CODEC_ID_VP9 => Some(Self::Vp9),
            _ => None,
        }
    }

    /// The Load declaration this framing belongs with. The Load is sent before the demuxer opens the
    /// file, so a disagreement can only abort and say so afterwards; that still beats the black
    /// screen it would otherwise be.
    pub(crate) fn load_video(self) -> crate::LoadVideo {
        use crate::LoadVideo;
        match self {
            Self::AnnexB { hevc: false } => LoadVideo::H264,
            Self::AnnexB { hevc: true } => LoadVideo::H265,
            Self::Av1 => LoadVideo::Av1,
            Self::Vp9 => LoadVideo::Vp9,
        }
    }
}

/// How the NAL units inside one demuxed H.264/HEVC packet are delimited, decided once per stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NalInput {
    /// A big-endian length before each NAL (avcC / hvcC): Matroska and MP4.
    LengthPrefixed,
    /// Start codes between the NALs and the parameter sets in-band: MPEG-TS, a raw elementary stream.
    StartCodes,
}

impl NalInput {
    /// FFmpeg's own rule (`ff_h264_decode_extradata`): extradata that is an avcC / hvcC record
    /// (its first byte is the version, 1) means length-prefixed packets; none, or parameter sets
    /// that kept their start codes, means the packets carry start codes too. Anything else keeps the
    /// length walk, so only a stream with NO config record changes behaviour. **Never decided from
    /// a packet:** a length-prefixed NAL of 256..=511 bytes begins `00 00 01 xx`, a valid 3-byte
    /// start code, and a small P-slice is exactly that.
    pub(crate) fn of_extradata(extradata: &[u8]) -> Self {
        if extradata.is_empty() || extradata.starts_with(&[0, 0, 1]) || extradata.starts_with(&[0, 0, 0, 1]) {
            Self::StartCodes
        } else {
            Self::LengthPrefixed
        }
    }
}

/// The clock a start-code stream's packet timestamps are rebased by, in ns: its `start_time`
/// (`AV_TIME_BASE` units, µs). The engine takes a fresh play's first keyframe to be near 0 and
/// `seek_to_ns` to be content time, and a broadcast capture's clock starts wherever the
/// transmitter's did (the report's first audio unit was at 45171.69 s). **Only a stream with no
/// config record is moved**: Matroska and MP4 keep the clock they have always had. An unknown
/// (`AV_NOPTS_VALUE`, `i64::MIN`) or negative start moves nothing; a stream is never moved forward.
pub fn stream_origin_ns(start_time_us: i64, input: NalInput) -> i64 {
    if input == NalInput::StartCodes && start_time_us > 0 {
        start_time_us.saturating_mul(1_000)
    } else {
        0
    }
}

/// A packet timestamp moved back by `by` ticks; "no value" stays no value.
pub fn rebase_ts(t: i64, by: i64) -> i64 {
    if t == AV_NOPTS_VALUE {
        t
    } else {
        t.saturating_sub(by)
    }
}

/// Move a demuxed packet onto the rebased clock ([`stream_origin_ns`]), in its stream's time base,
/// so everything downstream (the lanes' pts, subtitle cues, the side reader's anchor) reads 0-based
/// time without knowing about it. `origin_ns == 0` is a no-op.
unsafe fn rebase_packet(pkt: *mut AVPacket, st: *mut AVStream, origin_ns: i64) {
    if origin_ns == 0 {
        return;
    }
    let by = av_rescale_q(origin_ns, NS_TB, stream_time_base(st));
    (*pkt).pts = rebase_ts((*pkt).pts, by);
    (*pkt).dts = rebase_ts((*pkt).dts, by);
}

/// Hand one demuxed VP9 packet to the decoder as it came, returning `key` unchanged.
///
/// A VP9 frame is self-describing: every keyframe carries its profile, size and colour config in
/// its own uncompressed header, and libpf's `video/x-vp9` appsrc caps have no `codec_data` to
/// fill, so there is nothing to prepend (compare `av1_packet_to_tu`, where the sequence header
/// has to be put back in-band). A Matroska/MP4 packet may be a SUPERFRAME, a shown frame and its
/// hidden reference frames under one trailing index; it is passed whole, the way a WebM-aware
/// hardware decoder is fed.
pub fn vp9_packet_to_frame(pkt: &[u8], key: bool, out: &mut Vec<u8>) -> bool {
    out.clear();
    out.extend_from_slice(pkt);
    key
}

/// Parse an avcC (H264) / hvcC (HEVC) extradata record into a ready-to-prepend Annex-B
/// parameter-set blob (VPS/SPS/PPS with 4-byte start codes) + the NAL length-prefix size.
/// (Ported from the retired mkv.rs demuxer) — the format the Starfish decoder wants.
unsafe fn parse_extradata(ed: *const u8, len: usize, is_hevc: bool) -> (Vec<u8>, usize) {
    let mut blob = Vec::new();
    if ed.is_null() || len < 7 {
        return (blob, 4);
    }
    let e = std::slice::from_raw_parts(ed, len);
    let sc = [0u8, 0, 0, 1];
    if is_hevc {
        // HEVCDecoderConfigurationRecord: ver@0==1, nal_len@21, numArrays@22, arrays@23.
        if len < 23 || e[0] != 1 {
            return (blob, 4);
        }
        let nls = (e[21] & 3) as usize + 1;
        let narr = e[22] as usize;
        let mut p = 23usize;
        for _ in 0..narr {
            if p + 3 > len {
                break;
            }
            // array header: [completeness|reserved|NAL_type(&0x3f)] u16 count.
            // ONLY VPS(32)/SPS(33)/PPS(34) belong in the prepend blob; skip SEI(39/40) and
            // anything else — a stray SEI here corrupts the parameter-set sequence.
            let keep = matches!(e[p] & 0x3f, 32 | 33 | 34);
            let cnt = ((e[p + 1] as usize) << 8) | e[p + 2] as usize;
            p += 3;
            for _ in 0..cnt {
                if p + 2 > len {
                    break;
                }
                let nl = ((e[p] as usize) << 8) | e[p + 1] as usize;
                p += 2;
                if nl == 0 || p + nl > len {
                    break;
                }
                if keep {
                    blob.extend_from_slice(&sc);
                    blob.extend_from_slice(&e[p..p + nl]);
                }
                p += nl;
            }
        }
        (blob, nls)
    } else {
        // AVCDecoderConfigurationRecord: ver@0==1, nal_len@4, SPS list @5, then PPS list.
        if e[0] != 1 {
            return (blob, 4);
        }
        let nls = (e[4] & 3) as usize + 1;
        let nsps = (e[5] & 0x1f) as usize;
        let mut p = 6usize;
        for _ in 0..nsps {
            if p + 2 > len {
                break;
            }
            let nl = ((e[p] as usize) << 8) | e[p + 1] as usize;
            p += 2;
            if nl == 0 || p + nl > len {
                break;
            }
            blob.extend_from_slice(&sc);
            blob.extend_from_slice(&e[p..p + nl]);
            p += nl;
        }
        if p < len {
            let npps = e[p] as usize;
            p += 1;
            for _ in 0..npps {
                if p + 2 > len {
                    break;
                }
                let nl = ((e[p] as usize) << 8) | e[p + 1] as usize;
                p += 2;
                if nl == 0 || p + nl > len {
                    break;
                }
                blob.extend_from_slice(&sc);
                blob.extend_from_slice(&e[p..p + nl]);
                p += nl;
            }
        }
        (blob, nls)
    }
}

static SEI_STRIPPED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
static DV_EL_DROPPED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
static DV_RPU_REWRITTEN: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
static DV_RPU_FAILED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// HEVC `UNSPEC62`: the NAL type Dolby Vision carries its RPU (the dynamic metadata) in.
const HEVC_NAL_DV_RPU: u8 = 62;
/// HEVC `UNSPEC63`: the NAL type a single-track Dolby Vision dual layer (a UHD Blu-ray remux)
/// wraps each enhancement-layer NAL in. It is what `dovi_tool demux` splits the enhancement layer
/// out by, and what `dovi_tool convert --discard` drops.
const HEVC_NAL_DV_EL: u8 = 63;

/// `nuh_layer_id` of an HEVC NAL (`nal` starts at the 2-byte NAL header): bit 0 of byte 0 is the
/// high bit, the top five bits of byte 1 the rest. Zero throughout a single-layer stream.
pub fn hevc_layer_id(nal: &[u8]) -> u8 {
    if nal.len() < 2 {
        return 0;
    }
    ((nal[0] & 1) << 5) | (nal[1] >> 3)
}

/// True if this HEVC NAL belongs to a Dolby Vision enhancement layer: wrapped in `UNSPEC63`, as a
/// single-track dual layer stores it, or carried in a layer above the base (`nuh_layer_id > 0`),
/// which a single-layer decoder never decodes either. `nal` starts at the 2-byte NAL header.
pub fn is_dv_enhancement_layer(nal: &[u8]) -> bool {
    nal.len() >= 2 && (((nal[0] >> 1) & 0x3f) == HEVC_NAL_DV_EL || hevc_layer_id(nal) > 0)
}

/// PURE: does this configuration record describe a Dolby Vision **Profile 7 that is converted to
/// Profile 8.1 on the feed path** — dual layer (`el_present_flag`) with a cross-compatible base
/// layer? The demuxer-side twin of `plx_data::metadata::Dovi::converts_to_p81`, which decides the
/// route and the Load payload from what the server reported; this one decides what is fed, from
/// what the file itself carries.
pub fn dovi_converts_to_p81(d: &AVDOVIDecoderConfigurationRecord) -> bool {
    d.dv_profile == 7 && d.el_present_flag != 0 && d.dv_bl_signal_compatibility_id != 0
}

/// Rewrite a Dolby Vision RPU NAL (`UNSPEC62`; `nal` = 2-byte header + payload, emulation-
/// prevention bytes still in place) from **Profile 7 to Profile 8.1**, with quietvoid's
/// `dolby_vision` crate — the same parse, `convert_with_mode(2)` and write `dovi_tool -m 2`
/// performs. An RPU that is not Profile 7 comes back unchanged. `None` means the RPU could not be
/// parsed or converted; the caller DROPS it, because a frame without an RPU keeps the previous
/// frame's dynamic metadata, which is far better than a Profile 8 declaration followed by a
/// Profile 7 RPU.
pub fn rewrite_dv_rpu_p7_to_p81(nal: &[u8]) -> Option<Vec<u8>> {
    let mut rpu = dolby_vision::rpu::dovi_rpu::DoviRpu::parse_unspec62_nalu(nal).ok()?;
    if rpu.dovi_profile != 7 {
        return Some(nal.to_vec());
    }
    rpu.convert_with_mode(dolby_vision::rpu::ConversionMode::To81).ok()?;
    rpu.write_hevc_unspec62_nalu().ok()
}

/// What to do with one HEVC NAL on its way to the decoder.
#[derive(Debug, PartialEq, Eq)]
pub enum SeiStrip {
    /// Not an SEI, or an SEI without HDR10+: emit the original bytes (nothing allocated for a non-SEI).
    Keep,
    /// Every message was HDR10+ (or the SEI could not be parsed cleanly): emit nothing.
    Drop,
    /// The NAL minus its HDR10+ message(s), emulation prevention re-inserted.
    Rewrite(Vec<u8>),
}

/// Strip the ITU-T T.35 HDR10+ dynamic-metadata message (SEI payloadType 4, country_code 0xB5,
/// terminal provider 0x003C) from an HEVC SEI NAL (prefix=39 / suffix=40). webOS's HEVC decoder
/// does NOT support per-frame HDR10+ metadata and stalls on it — Kodi strips it unconditionally
/// ("webOS doesn't support HDR10+ and it can cause issues"). `nal` is the NAL payload WITHOUT the
/// start code (starts at the 2-byte NAL header). Only the HDR10+ message goes: the other messages
/// of the NAL, static HDR10 mastering-display / CLL (137/144) included, are kept byte for byte,
/// because on HEVC the panel's HDR envelope comes from the pipeline's own parse of them.
///
/// Every message is walked, not just the first: an SEI NAL may carry several, and an HDR10+
/// message behind another one reached the decoder. A list that does not parse cleanly (truncated
/// header or payload, bytes after the trailing bits) falls back to the whole-NAL rule: dropped if
/// an HDR10+ header was seen, kept unchanged otherwise.
pub fn strip_hdr10plus_sei(nal: &[u8]) -> SeiStrip {
    if nal.len() < 2 {
        return SeiStrip::Keep;
    }
    let nal_type = (nal[0] >> 1) & 0x3f;
    if nal_type != 39 && nal_type != 40 {
        return SeiStrip::Keep;
    }
    let rbsp = sei_rbsp(&nal[2..]);
    // Byte ranges of the messages to keep, and whether any HDR10+ message was seen.
    let mut kept: Vec<(usize, usize)> = Vec::new();
    let mut found = false;
    let mut clean = false;
    let mut p = 0usize;
    // Each message: payloadType and payloadSize as 0xFF runs plus a terminator, then the payload.
    // The 0x80 trailing-bits byte ends the list.
    while p < rbsp.len() {
        if rbsp[p] == 0x80 {
            clean = p + 1 == rbsp.len();
            break;
        }
        let start = p;
        let mut read = || {
            let mut v = 0usize;
            while p < rbsp.len() && rbsp[p] == 0xff {
                v += 255;
                p += 1;
            }
            let last = *rbsp.get(p)?;
            p += 1;
            Some(v + last as usize)
        };
        let (Some(ptype), Some(psize)) = (read(), read()) else {
            break;
        };
        let end = p.saturating_add(psize);
        let payload = rbsp.get(p..end.min(rbsp.len())).unwrap_or(&[]);
        let is_plus = ptype == 4 && payload.len() >= 3 && payload[..3] == [0xb5, 0x00, 0x3c];
        found |= is_plus;
        if end > rbsp.len() {
            break;
        }
        if !is_plus {
            kept.push((start, end));
        }
        p = end;
    }
    if !found {
        return SeiStrip::Keep;
    }
    if !clean || kept.is_empty() {
        return SeiStrip::Drop;
    }
    let mut out_rbsp = Vec::with_capacity(rbsp.len());
    for (s, e) in kept {
        out_rbsp.extend_from_slice(&rbsp[s..e]);
    }
    out_rbsp.push(0x80);
    // Emulation prevention (H.265 7.4.2): 03 after two zeros when the next byte is <= 03. The
    // RBSP ends in the 0x80 trailing byte, so the "ends in 00" case cannot arise.
    let mut out = Vec::with_capacity(out_rbsp.len() + 4);
    out.extend_from_slice(&nal[..2]);
    let mut zeros = 0;
    for b in out_rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    SeiStrip::Rewrite(out)
}

/// An SEI payload with its emulation-prevention bytes removed (`00 00 03` reads as `00 00`), so
/// message sizes count the bytes they describe.
pub fn sei_rbsp(ebsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ebsp.len());
    let mut zeros = 0;
    for &b in ebsp {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// End offset of a NAL of length `nl` starting at `i`, or None if it is empty or would run
/// past `size`. **The width is load-bearing**: `usize` is 32 bits on the TV, `nls` is up to 4,
/// so a hostile/corrupt length up to `0xFFFF_FFFF` makes the natural `i + nl > size` guard WRAP
/// to a small number, pass its own bounds check, and panic the demux thread inside the slice —
/// killing the producer before it can EOF the queues, which hangs playback with no error path.
/// Computing in `u64` makes the bound hold on every target. Never inline this back to `i + nl`.
pub fn nal_end(i: usize, nl: usize, size: usize) -> Option<usize> {
    if nl == 0 {
        return None;
    }
    let end = i as u64 + nl as u64;
    if end > size as u64 {
        return None;
    }
    Some(end as usize)
}

/// Is this H.264 non-IDR slice NAL (header byte included) an I-slice? Reads the slice header's
/// first two Exp-Golomb fields, `first_mb_in_slice` and `slice_type` (I is 2 or 7); a header too
/// short to hold them is not one. Both fields sit in the first bytes, ahead of any emulation-
/// prevention byte a valid header can need there.
fn h264_slice_is_intra(nal: &[u8]) -> bool {
    let bits = nal.get(1..).unwrap_or(&[]);
    let mut pos = 0usize;
    let mut ue = || -> Option<u32> {
        let bit = |p: usize| bits.get(p / 8).map(|b| (b >> (7 - p % 8)) & 1);
        let mut zeros = 0;
        while bit(pos)? == 0 {
            zeros += 1;
            pos += 1;
            if zeros > 31 {
                return None;
            }
        }
        pos += 1;
        let mut v = 0u32;
        for _ in 0..zeros {
            v = (v << 1) | u32::from(bit(pos)?);
            pos += 1;
        }
        Some((1u32 << zeros) - 1 + v)
    };
    ue().and_then(|_| ue()).is_some_and(|t| t == 2 || t == 7)
}

/// Does this H.264 NAL open a random access point: an IDR slice, or, when `intra_ok`, a non-IDR
/// I-slice. `intra_ok` is the caller's evidence that one is a keyframe here (the container flagged
/// the packet, or an SPS came ahead of the slice); both framings ask the same question.
fn h264_is_rap(nal: &[u8], intra_ok: bool) -> bool {
    match nal.first().map(|b| b & 0x1f) {
        Some(5) => true,
        Some(1) => intra_ok && h264_slice_is_intra(nal),
        _ => false,
    }
}

/// What a video packet is as a random access point, as `packet_to_annexb` reads it. The one place the
/// kind of an HEVC keyframe is known is the demux thread, so it is carried to the frame thread in the
/// access unit's `key` ([`Keyframe::au_key`], the encoding documented on `aq::AuNode::key`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Keyframe {
    /// Not a keyframe.
    No,
    /// A keyframe the Dolby Vision splitter takes first: an HEVC IDR, or any keyframe that is not HEVC.
    Clean,
    /// An HEVC IRAP that is not an IDR, with its `nal_unit_type`: a BLA (16 to 18) or a CRA (21), or a
    /// reserved IRAP type (22, 23).
    HevcIrapNotIdr(u8),
}

impl Keyframe {
    /// The class of a keyframe flag from a framing that does not tell the kinds apart.
    fn of(key: bool) -> Keyframe {
        if key { Keyframe::Clean } else { Keyframe::No }
    }

    fn is_key(self) -> bool {
        self != Keyframe::No
    }

    /// What the `ff: AU#0` line prints for the first access unit: `true`/`false` as before, with the
    /// HEVC NAL type of a non-IDR IRAP named (`irap=CRA(21)`), which would have named #1166 from the
    /// first log.
    fn label(self) -> String {
        match self {
            Keyframe::No => "false".to_string(),
            Keyframe::Clean => "true".to_string(),
            Keyframe::HevcIrapNotIdr(t) => format!("true irap={}", crate::aq::hevc_irap_label(t)),
        }
    }

    /// The `key` the access unit is queued with ([`crate::aq::AU_KEY`] and [`crate::aq::hevc_irap_not_idr`]).
    fn au_key(self) -> c_int {
        match self {
            Keyframe::No => 0,
            Keyframe::Clean => crate::aq::AU_KEY,
            Keyframe::HevcIrapNotIdr(t) => c_int::from(t),
        }
    }
}

/// Convert one length-prefixed video packet to Annex-B (4-byte start codes) into `out` (a packet
/// that carries start codes already is `annexb_h264_access_unit`'s: see [`NalInput`]),
/// prepending `param` (VPS/SPS/PPS) when the AU is a keyframe (H264 IDR type 5, or a non-IDR
/// I-slice at a random access point, see `container_key`; HEVC IRAP types 16-23). Returns what kind
/// of keyframe it is, if any ([`Keyframe`]). Mirrors mkv_handle_block.
///
/// `container_key`: the demuxer flagged the packet a keyframe (`AV_PKT_FLAG_KEY`). For H.264 that,
/// or an SPS ahead of the slice in the same AU, also makes a non-IDR I-slice one: a Blu-ray remux's
/// random access points after its first frame are I-slices behind a recovery point (with the
/// parameter sets repeated in band), and its Cues index them, so a seek lands on one and counting
/// only IDR left the post-seek rebase waiting for a keyframe that never came.
///
/// `dv_p7`: the stream is a Dolby Vision Profile 7 converted to 8.1 on the way to the pipeline
/// ([`dovi_converts_to_p81`]). Every enhancement-layer NAL ([`is_dv_enhancement_layer`]) is
/// dropped and every RPU is rewritten by [`rewrite_dv_rpu_p7_to_p81`], so what leaves here is the
/// single-layer stream the Load payload declares (`DolbyHdrInfo profileId 8`).
pub unsafe fn packet_to_annexb(
    data: *const u8,
    size: usize,
    nls: usize,
    is_hevc: bool,
    dv_p7: bool,
    container_key: bool,
    param: &[u8],
    out: &mut Vec<u8>,
) -> Keyframe {
    out.clear();
    if data.is_null() || size < nls + 1 {
        return Keyframe::No;
    }
    let d = std::slice::from_raw_parts(data, size);
    // pass 1: is this AU a keyframe?
    let mut is_key = Keyframe::No;
    let mut in_band_sps = false;
    let mut i = 0usize;
    while i + nls <= size {
        let mut nl = 0usize;
        for k in 0..nls {
            nl = (nl << 8) | d[i + k] as usize;
        }
        i += nls;
        if nal_end(i, nl, size).is_none() {
            break;
        }
        let b0 = d[i];
        let class = if is_hevc {
            // An IRAP picture's VCL NALs all share one type (HEVC 7.4.2.2), so the first IRAP NAL
            // names the picture. Only an IDR (19, 20) is `Clean`: a CRA or BLA (16 to 18, 21) and the
            // reserved IRAP types (22, 23) are not, because a Dolby Vision splitter may take only an
            // IDR as its first picture.
            match (b0 >> 1) & 0x3f {
                19 | 20 => Keyframe::Clean,
                t @ (16..=18 | 21..=23) => Keyframe::HevcIrapNotIdr(t),
                _ => Keyframe::No,
            }
        } else {
            in_band_sps |= (b0 & 0x1f) == 7;
            Keyframe::of(h264_is_rap(&d[i..i + nl], container_key || in_band_sps))
        };
        if class.is_key() {
            is_key = class;
            break;
        }
        i += nl;
    }
    if is_key.is_key() {
        out.extend_from_slice(param);
    }
    // pass 2: emit each NAL as 00 00 00 01 + bytes, dropping per-frame HDR10+ SEI (HEVC) and, on a
    // converted Dolby Vision Profile 7, the enhancement layer (its RPUs are rewritten instead)
    let sc = [0u8, 0, 0, 1];
    let mut i = 0usize;
    while i + nls <= size {
        let mut nl = 0usize;
        for k in 0..nls {
            nl = (nl << 8) | d[i + k] as usize;
        }
        i += nls;
        let Some(end) = nal_end(i, nl, size) else {
            break;
        };
        let nal = &d[i..end];
        if is_hevc && dv_p7 {
            if is_dv_enhancement_layer(nal) {
                let n = DV_EL_DROPPED.fetch_add(1, Ordering::Relaxed) + 1;
                if n <= 3 || n % 5000 == 0 {
                    plx_base::eventlog::log(&format!(
                        "ff: dropped DV enhancement-layer NAL #{n} ({nl} bytes)"
                    ));
                }
                i += nl;
                continue;
            }
            if ((nal[0] >> 1) & 0x3f) == HEVC_NAL_DV_RPU {
                match rewrite_dv_rpu_p7_to_p81(nal) {
                    Some(rpu) => {
                        let n = DV_RPU_REWRITTEN.fetch_add(1, Ordering::Relaxed) + 1;
                        if n <= 3 || n % 5000 == 0 {
                            plx_base::eventlog::log(&format!(
                                "ff: rewrote DV RPU #{n} P7 -> P8.1 ({nl} -> {} bytes)",
                                rpu.len()
                            ));
                        }
                        out.extend_from_slice(&sc);
                        out.extend_from_slice(&rpu);
                    }
                    None => {
                        let n = DV_RPU_FAILED.fetch_add(1, Ordering::Relaxed) + 1;
                        if n <= 3 || n % 500 == 0 {
                            plx_base::eventlog::log(&format!(
                                "ff: DV RPU #{n} could not be converted, dropped ({nl} bytes)"
                            ));
                        }
                    }
                }
                i += nl;
                continue;
            }
        }
        let mut nal = nal;
        let rewritten;
        if is_hevc {
            match strip_hdr10plus_sei(nal) {
                SeiStrip::Keep => {}
                strip => {
                    let n = SEI_STRIPPED.fetch_add(1, Ordering::Relaxed) + 1;
                    if n <= 3 || n % 500 == 0 {
                        plx_base::eventlog::log(&format!("ff: stripped HDR10+ SEI #{n} ({nl} bytes)"));
                    }
                    match strip {
                        SeiStrip::Rewrite(v) => {
                            rewritten = v;
                            nal = &rewritten;
                        }
                        _ => {
                            i += nl;
                            continue;
                        }
                    }
                }
            }
        }
        out.extend_from_slice(&sc);
        out.extend_from_slice(nal);
        i += nl;
    }
    is_key
}

static DIAG_FIRST: AtomicBool = AtomicBool::new(true);

/// Extract one DTS core access unit, dropping the DTS-HD extension substream. Matroska and
/// MOV demux return the complete packet; our bundled FFmpeg 9.0 has neither the dca parser nor
/// dca_core BSF enabled. The core size is the 14-bit FSIZE + 1 from its normalized header
/// (ETSI TS 102 114; FFmpeg 9.0 libavcodec/bsf/dca_core.c). No new FFmpeg ABI is needed.
/// Normalize the four core sync formats to 16-bit big endian before measuring. Extension-only,
/// truncated and malformed packets are rejected instead of sending HD data to the core decoder.
fn dts_core_packet(packet: &[u8]) -> Option<std::borrow::Cow<'_, [u8]>> {
    use std::borrow::Cow;
    let sync: [u8; 4] = packet.get(..4)?.try_into().ok()?;
    let normalized = match sync {
        [0x7f, 0xfe, 0x80, 0x01] => Cow::Borrowed(packet),
        [0xfe, 0x7f, 0x01, 0x80] => {
            let mut bytes = packet.to_vec();
            for word in bytes.chunks_exact_mut(2) { word.swap(0, 1); }
            Cow::Owned(bytes)
        }
        [0x1f, 0xff, 0xe8, 0x00] | [0xff, 0x1f, 0x00, 0xe8] => {
            let little = sync[0] == 0xff;
            let mut bytes = Vec::with_capacity(packet.len() * 7 / 8);
            let (mut bits, mut count) = (0u32, 0u32);
            for word in packet.chunks_exact(2) {
                let word = if little { u16::from_le_bytes([word[0], word[1]]) }
                    else { u16::from_be_bytes([word[0], word[1]]) };
                bits = (bits << 14) | u32::from(word & 0x3fff);
                count += 14;
                while count >= 8 {
                    count -= 8;
                    bytes.push((bits >> count) as u8);
                }
            }
            Cow::Owned(bytes)
        }
        _ => return None,
    };
    let header = normalized.get(..10)?;
    let size = (((usize::from(header[5]) & 3) << 12)
        | (usize::from(header[6]) << 4) | (usize::from(header[7]) >> 4)) + 1;
    // The standard's minimum core frame length is 96 bytes. Never accept a size that points
    // back into the header or beyond the packet, including a truncated 14-bit word.
    if size < 96 || size > normalized.len() { return None; }
    Some(match normalized {
        Cow::Borrowed(bytes) => Cow::Borrowed(&bytes[..size]),
        Cow::Owned(mut bytes) => { bytes.truncate(size); Cow::Owned(bytes) }
    })
}

#[cfg(test)]
mod dts_core_tests {
    use super::dts_core_packet;

    fn core() -> Vec<u8> {
        let mut frame = vec![0x55; 128];
        frame[..10].copy_from_slice(&[0x7f, 0xfe, 0x80, 0x01, 0xfc, 0x00, 0x07, 0xf0, 0, 0]);
        frame
    }

    /// The lossy bit travels with the declared PCM and never leaks into its shape: a lossless PCM
    /// Load of the same 48 kHz stereo S16 does not make a DTS stream the app's.
    #[test]
    fn the_lossy_bit_is_the_loads_own_and_does_not_change_the_shape() {
        use crate::ff_pcm::PcmOut;
        let _g = plx_base::testlock::serial();
        let shape = Some(PcmOut::exact(48_000, 16, 2));
        super::set_pcm_out(shape, true);
        assert!(super::pcm_lossy());
        let lossy = super::pcm_out().unwrap();
        super::set_pcm_out(shape, false);
        assert!(!super::pcm_lossy(), "a lossless PCM Load of the same shape");
        assert_eq!(super::pcm_out().unwrap(), lossy);
        super::set_pcm_out(None, true);
        assert!(!super::pcm_lossy() && super::pcm_out().is_none(), "no PCM declared, nothing lossy");
    }

    /// The declared PCM roundtrips through its one word, with the track's own rate in its 31 high bits: the
    /// largest rate FFmpeg can state (`i32::MAX`) comes back whole, and the lossy flag, which sits just below it,
    /// neither clobbers it nor is clobbered by it.
    #[test]
    fn the_declared_pcm_roundtrips_with_the_source_rate_and_the_lossy_flag_beside_it() {
        use crate::ff_pcm::PcmOut;
        let _g = plx_base::testlock::serial();
        for out in [
            PcmOut::exact(48_000, 16, 2),
            PcmOut { rate: 48_000, bits: 32, channels: 2, source_rate: 96_000 },
            PcmOut { rate: 44_100, bits: 32, channels: 1, source_rate: i32::MAX as u32 },
            PcmOut { rate: 48_000, bits: 32, channels: 2, source_rate: 1 << 30 },
        ] {
            for lossy in [false, true] {
                super::set_pcm_out(Some(out), lossy);
                assert_eq!(super::pcm_out(), Some(out), "{out:?} lossy={lossy}");
                assert_eq!(super::pcm_lossy(), lossy, "{out:?}");
            }
        }
        super::set_pcm_out(None, false);
        assert!(super::pcm_out().is_none());
    }

    #[test]
    fn preserves_the_generated_dts_5_1_access_unit() {
        let frame = include_bytes!("../../../tests/fixtures/dts-core-frame.dts");
        assert_eq!(dts_core_packet(frame).unwrap().as_ref(), frame);
        let mut with_extension = frame.to_vec();
        with_extension.extend_from_slice(&[0x64, 0x58, 0x20, 0x25, 0, 0, 0, 0]);
        assert_eq!(dts_core_packet(&with_extension).unwrap().as_ref(), frame);
    }

    #[test]
    fn preserves_core_and_strips_hd_extensions() {
        let core = core();
        assert_eq!(dts_core_packet(&core).unwrap().as_ref(), core);
        let mut hd = core.clone();
        hd.extend_from_slice(&[0x64, 0x58, 0x20, 0x25, 0x01, 0x02, 0x03, 0x04]);
        assert_eq!(dts_core_packet(&hd).unwrap().as_ref(), core);
    }

    #[test]
    fn refuses_extension_only_truncated_and_invalid_sizes() {
        assert!(dts_core_packet(&[0x64, 0x58, 0x20, 0x25]).is_none());
        let core = core();
        for n in 0..core.len() { assert!(dts_core_packet(&core[..n]).is_none()); }
        let mut invalid = core;
        invalid[6] = 0;
        invalid[7] = 0;
        assert!(dts_core_packet(&invalid).is_none());
    }

    #[test]
    fn normalizes_word_swapped_and_packed_core_formats() {
        let core = core();
        let mut swapped = core.clone();
        for word in swapped.chunks_exact_mut(2) { word.swap(0, 1); }
        assert_eq!(dts_core_packet(&swapped).unwrap().as_ref(), core);
        let mut packed = Vec::new();
        let (mut bits, mut count) = (0u32, 0u32);
        for &byte in core.iter().chain([0u8; 2].iter()) {
            bits = (bits << 8) | u32::from(byte);
            count += 8;
            if count >= 14 {
                count -= 14;
                let mut word = ((bits >> count) & 0x3fff) as u16;
                if packed.len() == 2 { word |= 0xc000; }
                packed.extend_from_slice(&word.to_be_bytes());
            }
        }
        assert_eq!(dts_core_packet(&packed).unwrap().as_ref(), core);
        for word in packed.chunks_exact_mut(2) { word.swap(0, 1); }
        assert_eq!(dts_core_packet(&packed).unwrap().as_ref(), core);
    }
}

/// ADTS sampling_frequency_index (4 bits) for a sample rate; None if not a standard AAC rate.
fn adts_freq_index(rate: c_int) -> Option<u8> {
    Some(match rate {
        96000 => 0,
        88200 => 1,
        64000 => 2,
        48000 => 3,
        44100 => 4,
        32000 => 5,
        24000 => 6,
        22050 => 7,
        16000 => 8,
        12000 => 9,
        11025 => 10,
        8000 => 11,
        7350 => 12,
        _ => return None,
    })
}

/// (freq_idx, chan_cfg) for the ADTS header, read from the stream's AudioSpecificConfig when
/// present. HE-AAC is why this parses the ASC instead of trusting codecpar: the ASC's FIRST
/// samplingFrequencyIndex is the AAC **core** rate (e.g. 24 kHz) while `codecpar.sample_rate`
/// reports the SBR **output** rate (48 kHz). ADTS must carry the core rate (+ LC profile, SBR
/// stays implicit) so the decoder up-samples through the SBR extension — a 48 kHz header makes
/// it decode each frame as 1024 plain-LC samples into a 2048-sample slot, an audible gap/crackle
/// every frame (Maxton Hall, HE-AAC 5.1). Falls back to codecpar for ASC-less streams.
unsafe fn adts_params(acp: *const AVCodecParameters) -> Option<(u8, u8)> {
    let chan_fallback = {
        let ch = (*acp).ch_layout.nb_channels;
        if (1..=7).contains(&ch) { ch as u8 } else { 2 }
    };
    // AudioSpecificConfig: aot(5) freqIdx(4) [+24-bit rate if idx==15] chanCfg(4) …
    let (ed, n) = ((*acp).extradata, (*acp).extradata_size);
    if !ed.is_null() && n >= 2 {
        let (b0, b1) = (*ed as u32, *ed.add(1) as u32);
        let aot = (b0 >> 3) & 0x1F;
        let freq_idx = (((b0 & 0x07) << 1) | (b1 >> 7)) as u8;
        if aot != 31 && freq_idx <= 12 {
            let chan = ((b1 >> 3) & 0x0F) as u8;
            let ch = if (1..=7).contains(&chan) {
                chan
            } else {
                chan_fallback
            };
            return Some((freq_idx, ch));
        }
        // escape-coded AOT / explicit 24-bit rate — exotic; fall through to codecpar
    }
    adts_freq_index((*acp).sample_rate).map(|fi| (fi, chan_fallback))
}

/// 7-byte ADTS header for a raw AAC frame of `payload_len` bytes. LG's Starfish decodes
/// ADTS-framed AAC (ss4s: aacInfo format="adts"); mp4/matroska carry RAW AAC, so we reframe.
/// Emits AAC-LC (ADTS profile 1 = object type 2 - 1); buffer fullness = VBR (0x7FF).
pub fn adts_header(freq_idx: u8, chan_cfg: u8, payload_len: usize) -> [u8; 7] {
    let frame_len = (payload_len + 7) as u32; // total incl. header, 13 bits
    [
        0xFF,
        0xF1, // sync(1111) | MPEG-4(0) | layer(00) | protection_absent(1)
        (1 << 6) | (freq_idx << 2) | (chan_cfg >> 2), // profile(01=LC) | freq_idx | chan hi bit
        ((chan_cfg & 3) << 6) | ((frame_len >> 11) as u8 & 0x03),
        ((frame_len >> 3) & 0xFF) as u8,
        (((frame_len & 0x07) as u8) << 5) | 0x1F, // frame_len lo 3 | fullness hi 5 (11111)
        0xFC,                                     // fullness lo 6 (111111) | num_blocks(00)
    ]
}

#[derive(Default)]
pub struct H264ParamSets {
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
}

pub fn annexb_start(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i + 3 <= data.len() {
        if data[i..].starts_with(&[0, 0, 1]) {
            return Some((i, 3));
        }
        if i + 4 <= data.len() && data[i..].starts_with(&[0, 0, 0, 1]) {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}

/// An HLS segment's access unit: [`annexb_h264_access_unit`] where only an IDR is a keyframe, which
/// is how PMS's x264 opens every segment.
pub fn ts_h264_access_unit(
    data: &[u8],
    params: &mut H264ParamSets,
    out: &mut Vec<u8>,
) -> Result<bool, &'static str> {
    annexb_h264_access_unit(data, Rap::IdrOnly, params, out)
}

/// What makes an H.264 slice a random access point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rap {
    /// An IDR slice only.
    IdrOnly,
    /// An IDR slice, or a non-IDR I-slice: one the container flagged (`container_key`), or one
    /// behind an in-band SPS. The rule `packet_to_annexb` applies to a length-prefixed packet
    /// ([`h264_is_rap`]).
    IdrOrIntra { container_key: bool },
}

impl Rap {
    /// Whether a non-IDR I-slice counts, `has_sps` being an SPS already seen in this unit.
    fn intra_ok(self, has_sps: bool) -> bool {
        matches!(self, Rap::IdrOrIntra { container_key } if container_key || has_sps)
    }
}

/// Validate an H.264 packet that already carries start codes (MPEG-TS, a raw elementary stream),
/// retain its in-band SPS/PPS and prepend whichever set is missing on a keyframe. `packet_to_annexb`
/// cannot take such a packet: it reads the first `00 00 00 01` as a NAL length of one and the next
/// four bytes as a length past the end, leaving every unit as `00 00 00 01 09` (#1084). Which of the
/// two a stream needs is [`NalInput`]'s decision. `Err` names why the packet is not a usable unit.
pub fn annexb_h264_access_unit(
    data: &[u8],
    rap: Rap,
    params: &mut H264ParamSets,
    out: &mut Vec<u8>,
) -> Result<bool, &'static str> {
    out.clear();
    let Some((first, _)) = annexb_start(data, 0) else {
        return Err("H.264 packet is not Annex-B");
    };
    if first != 0 {
        return Err("bytes precede the first Annex-B start code");
    }

    let mut cursor = 0;
    let mut is_key = false;
    let mut has_sps = false;
    let mut has_pps = false;
    while let Some((start, prefix)) = annexb_start(data, cursor) {
        let payload = start + prefix;
        if payload >= data.len() {
            return Err("empty Annex-B NAL");
        }
        let end = annexb_start(data, payload).map_or(data.len(), |(next, _)| next);
        let nal_type = data[payload] & 0x1f;
        match nal_type {
            5 | 1 if h264_is_rap(&data[payload..end], rap.intra_ok(has_sps)) => is_key = true,
            7 => {
                params.sps.clear();
                params.sps.extend_from_slice(&[0, 0, 0, 1]);
                params.sps.extend_from_slice(&data[payload..end]);
                has_sps = true;
            }
            8 => {
                params.pps.clear();
                params.pps.extend_from_slice(&[0, 0, 0, 1]);
                params.pps.extend_from_slice(&data[payload..end]);
                has_pps = true;
            }
            _ => {}
        }
        cursor = end;
        if cursor >= data.len() {
            break;
        }
    }

    if is_key {
        if !has_sps {
            if params.sps.is_empty() {
                return Err("IDR has no in-band or cached SPS");
            }
            out.extend_from_slice(&params.sps);
        }
        if !has_pps {
            if params.pps.is_empty() {
                return Err("IDR has no in-band or cached PPS");
            }
            out.extend_from_slice(&params.pps);
        }
    }
    out.extend_from_slice(data);
    Ok(is_key)
}

/// One AAC access unit as the pipeline wants it: ADTS-framed, with exactly one header. Matroska and
/// MP4 carry RAW AAC and need one added (`params`: the frequency index and channel configuration);
/// MPEG-TS carries ADTS itself, libavformat leaves the header on the packet, and a second one ahead
/// of it would put `FF F1` where the raw data block begins (#1084). `None`: raw AAC and nothing to
/// frame it with.
pub fn adts_framed(raw: &[u8], params: Option<(u8, u8)>) -> Option<std::borrow::Cow<'_, [u8]>> {
    if packet_has_adts(raw) {
        return Some(std::borrow::Cow::Borrowed(raw));
    }
    let (freq_idx, chan_cfg) = params?;
    let mut framed = Vec::with_capacity(7 + raw.len());
    framed.extend_from_slice(&adts_header(freq_idx, chan_cfg, raw.len()));
    framed.extend_from_slice(raw);
    Some(std::borrow::Cow::Owned(framed))
}

pub fn packet_has_adts(data: &[u8]) -> bool {
    data.len() >= 7 && data[0] == 0xff && data[1] & 0xf6 == 0xf0
}

pub fn adts_duration_ns(data: &[u8]) -> Option<i64> {
    if !packet_has_adts(data) {
        return None;
    }
    const RATES: [i64; 13] = [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000, 7_350,
    ];
    let rate = *RATES.get(((data[2] >> 2) & 0x0f) as usize)?;
    let blocks = i64::from((data[6] & 0x03) + 1);
    Some(
        1_024_i64
            .saturating_mul(blocks)
            .saturating_mul(1_000_000_000)
            / rate,
    )
}

#[derive(Clone, Copy, Debug)]
pub enum HlsExit {
    Aborted,
    NotReady,
    PrimeExpired,
    /// The terminal rule fired on the ACTIVE stream: the playable reserve reached zero while this
    /// fetch was still incomplete — waiting on its open, on a `NotReady` retry, or with body bytes
    /// outstanding — and a cheaper rung remained. Not a failure —
    /// the segment is abandoned on purpose after the reserve actually reaches its terminal
    /// boundary, so the controller gets a recovery decision it would otherwise never be asked
    /// for. See [`StallGuard`].
    ///
    /// It carries the transfer for diagnostics and censored-event accounting — before any body
    /// byte, zero bytes and zero body time over the request's elapsed time (see
    /// `AcquisitionRuntime::stopped_exit`). Its prefix is not a capacity or acquisition
    /// observation: PMS production, response pacing and network service are not identifiable
    /// separately before the response completes.
    StallAbort(SegmentTransfer),
    Failed(&'static str),
}

impl From<HlsExit> for plx_playback_contract::adaptive::PhysicalFailure {
    fn from(exit: HlsExit) -> Self {
        use plx_playback_contract::adaptive::{PhysicalFailure as F, TransferFact};
        match exit {
            HlsExit::Aborted => F::Aborted,
            HlsExit::NotReady => F::NotReady,
            HlsExit::PrimeExpired => F::PrimeExpired,
            HlsExit::StallAbort(transfer) => F::StallAbort(TransferFact {
                bytes: transfer.bytes,
                active_us: transfer.active_us,
                total_us: transfer.total_us,
                audio_expected: transfer.audio_expected,
            }),
            HlsExit::Failed(message) => F::Failed(message),
        }
    }
}

impl From<plx_playback_contract::adaptive::PhysicalFailure> for HlsExit {
    fn from(failure: plx_playback_contract::adaptive::PhysicalFailure) -> Self {
        use plx_playback_contract::adaptive::PhysicalFailure as F;
        match failure {
            F::Aborted => HlsExit::Aborted,
            F::NotReady => HlsExit::NotReady,
            F::PrimeExpired => HlsExit::PrimeExpired,
            F::StallAbort(transfer) => HlsExit::StallAbort(SegmentTransfer {
                bytes: transfer.bytes,
                active_us: transfer.active_us,
                total_us: transfer.total_us,
                audio_expected: transfer.audio_expected,
            }),
            F::Failed(message) => HlsExit::Failed(message),
        }
    }
}

/// **What a controlled stop means when its checkpoint latched no reason.** A leg ends in a stop
/// only because its checkpoint answered `Flow::Stop`, and the only checkpoint that ever does, the
/// acquisition runtime, latches why first — and its caller settles every failed leg through it
/// (`settle`) before this value can matter. So this is the bug path, and it takes the one
/// classification that asserts nothing: not `Aborted` (would end the session on no teardown), not
/// `PrimeExpired`/`StallAbort` (censored reserve evidence nobody observed), not `NotReady` (would
/// retry). A plain failure ends the segment the way any unexplained transport failure does.
const UNLATCHED_STOP: HlsExit = HlsExit::Failed("HLS acquisition stopped without a latched reason");

/// Lookahead of N+1 must not tear down a session that already fed N. Only teardown is fatal.
pub fn hls_prefetch_is_fatal(err: &HlsExit) -> bool {
    matches!(err, HlsExit::Aborted)
}

pub fn classify_plaintext_open_failure(error: plx_net::stream::HttpOpenError) -> HlsExit {
    match error {
        plx_net::stream::HttpOpenError::Deadline => HlsExit::PrimeExpired,
        plx_net::stream::HttpOpenError::Status(404) => HlsExit::NotReady,
        plx_net::stream::HttpOpenError::Aborted => HlsExit::Aborted,
        plx_net::stream::HttpOpenError::Stopped => UNLATCHED_STOP,
        plx_net::stream::HttpOpenError::Status(_) | plx_net::stream::HttpOpenError::Transport => {
            HlsExit::Failed("HTTP request failed")
        }
    }
}

pub fn classify_curl_open_err(e: range_io::OpenErr, observer: &dyn DemuxObserver) -> HlsExit {
    if e == range_io::OpenErr::Deadline {
        return HlsExit::PrimeExpired;
    }
    if let range_io::OpenErr::Status(status) = e {
        observer.publish(DemuxCallbackEffect::HttpStatus(status));
        if status == 404 {
            return HlsExit::NotReady;
        }
    }
    if e == range_io::OpenErr::Aborted {
        HlsExit::Aborted
    } else if e == range_io::OpenErr::Stopped {
        UNLATCHED_STOP
    } else {
        HlsExit::Failed("HTTPS request failed")
    }
}

fn hls_recycle_src(src: Src, net: &mut HlsNet) {
    if let Src::Curl(cs) = src {
        net.curl = Some(cs);
    }
}

pub fn hls_open_source(
    resource: &crate::hls::Resource,
    request_path: &str,
    aq: *mut AuQueue,
    net: &mut HlsNet,
    deadline: Option<std::time::Instant>,
    // Consulted by every blocking wait of the open with a deadline. Playlist fetches pass
    // `NoCheckpoint`; a segment fetch passes its acquisition runtime.
    checkpoint: &mut dyn plx_base::checkpoint::Checkpoint,
) -> Result<(Src, i64, crate::hls::Resource), HlsExit> {
    if unsafe { crate::aq::aq_is_aborted(aq) } {
        return Err(HlsExit::Aborted);
    }
    let origin = &resource.origin;
    if origin.is_tls() {
        let url = format!("{}{}", origin.base(), request_path);
        // `curlio` follows any redirect itself and does not expose where it landed; the requested
        // resource is the only base this path knows.
        let (src, size) = hls_open_curl(&url, aq, net, deadline, checkpoint)?;
        Ok((src, size, resource.clone()))
    } else {
        hls_open_plain(origin, request_path, aq, net, deadline, checkpoint)
    }
}

/// The https half of [`hls_open_source`]: reuse the session's curl multi when it has one, else
/// dial a fresh reserved source. Also where a plaintext open lands after a redirect to https.
fn hls_open_curl(
    url: &str,
    aq: *mut AuQueue,
    net: &mut HlsNet,
    deadline: Option<std::time::Instant>,
    checkpoint: &mut dyn plx_base::checkpoint::Checkpoint,
) -> Result<(Src, i64), HlsExit> {
    if let Some(mut cs) = net.curl.take() {
        let reopened = cs.reopen_until(url, deadline, &mut *checkpoint);
        if unsafe { crate::aq::aq_is_aborted(aq) } {
            return Err(HlsExit::Aborted);
        }
        match reopened {
            Ok(()) => {
                let (status, size) = (cs.status(), cs.size());
                net.observer.publish(DemuxCallbackEffect::HttpStatus(status));
                net.observer.publish(DemuxCallbackEffect::FileSize(size));
                return Ok((Src::Curl(cs), size));
            }
            Err(range_io::OpenErr::Aborted) => return Err(HlsExit::Aborted),
            Err(range_io::OpenErr::Deadline) => {
                net.curl = Some(cs);
                return Err(HlsExit::PrimeExpired);
            }
            // A controlled stop keeps the session and never falls through to a fresh dial.
            Err(e @ range_io::OpenErr::Stopped) => {
                net.curl = Some(cs);
                return Err(classify_curl_open_err(e, net.observer.as_ref()));
            }
            Err(range_io::OpenErr::Status(status)) => {
                net.observer.publish(DemuxCallbackEffect::HttpStatus(status));
                net.curl = Some(cs);
                return Err(if status == 404 {
                    HlsExit::NotReady
                } else {
                    HlsExit::Failed("HTTPS request failed")
                });
            }
            Err(_) => {
                // Idle TLS session or a broken multi: drop it and dial as a first open.
            }
        }
    }
    let reservation = net.range.reserve_open().map_err(|e| {
        if e == range_io::OpenErr::Aborted {
            HlsExit::Aborted
        } else {
            HlsExit::Failed("HTTPS reservation failed")
        }
    })?;
    if unsafe { crate::aq::aq_is_aborted(aq) } {
        return Err(HlsExit::Aborted);
    }
    let opened = net.range.open_reserved_checked(
        url,
        0,
        reservation,
        deadline,
        &mut *checkpoint,
    );
    if unsafe { crate::aq::aq_is_aborted(aq) } {
        drop(opened);
        return Err(HlsExit::Aborted);
    }
    let cs = opened.map_err(|e| classify_curl_open_err(e, net.observer.as_ref()))?;
    let (status, size) = (cs.status(), cs.size());
    net.observer.publish(DemuxCallbackEffect::HttpStatus(status));
    net.observer.publish(DemuxCallbackEffect::FileSize(size));
    Ok((Src::Curl(cs), size))
}

/// The plaintext half of [`hls_open_source`], redirects followed (`stream::redirect`) while they
/// stay on the PMS origin. Also returns the resource the open landed on: a playlist's children
/// resolve against it, not against the path that was requested.
fn hls_open_plain(
    origin: &Origin,
    request_path: &str,
    aq: *mut AuQueue,
    net: &mut HlsNet,
    deadline: Option<std::time::Instant>,
    checkpoint: &mut dyn plx_base::checkpoint::Checkpoint,
) -> Result<(Src, i64, crate::hls::Resource), HlsExit> {
    use plx_net::stream::redirect::{FollowError, Opened};
    let hs = net.hs;
    if deadline.is_some_and(|at| std::time::Instant::now() >= at) {
        return Err(HlsExit::PrimeExpired);
    }
    // Keep-alive: `http_open` reuses the live fd when the previous body was drained.
    let req = plx_net::stream::redirect::Request {
        origin,
        path: request_path,
        credentials: None,
        range_from: None,
        deadline,
        // The HLS contract (`crate::hls`): every request stays on the PMS origin.
        same_origin_only: true,
        credential_gate: net.credential_gate,
    };
    let opened = plx_net::stream::redirect::open_following(hs, &req, &mut *checkpoint);
    if unsafe { crate::aq::aq_is_aborted(aq) } {
        return Err(HlsExit::Aborted);
    }
    let target = match opened {
        Ok(Opened::Socket(t)) => t,
        // Unreachable from a plaintext origin with `same_origin_only`: https is another origin.
        Ok(Opened::Tls(_)) => return Err(HlsExit::Failed("HTTP redirect refused")),
        Err(error) => {
            net.observer.publish(DemuxCallbackEffect::HttpStatus(
                plx_net::stream::hs_status(hs),
            ));
            return Err(match error {
                FollowError::Open(error) => classify_plaintext_open_failure(error),
                FollowError::TooManyHops
                | FollowError::BadLocation(_)
                | FollowError::LeftOrigin => HlsExit::Failed("HTTP redirect refused"),
                FollowError::Refused => HlsExit::Failed("insecure credential transport refused"),
            });
        }
    };
    let landed = crate::hls::Resource::from_request_path(target.origin.clone(), &target.path)
        .map_err(|_| HlsExit::Failed("HTTP redirect refused"))?;
    let host =
        CString::new(target.origin.host()).map_err(|_| HlsExit::Failed("invalid PMS host"))?;
    let port = target.origin.port() as c_int;
    let path =
        CString::new(target.path).map_err(|_| HlsExit::Failed("invalid HLS request path"))?;
    let status = plx_net::stream::hs_status(hs);
    let size = plx_net::stream::hs_content_length(hs);
    net.observer.publish(DemuxCallbackEffect::HttpStatus(status));
    net.observer.publish(DemuxCallbackEffect::FileSize(size));
    Ok((
        Src::Socket {
            hs,
            host,
            port,
            path,
        },
        size,
        landed,
    ))
}

/// Why a progressive media open produced no source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaOpenFail {
    /// Teardown won; not a playback failure.
    Aborted,
    Failed,
}

/// Open an https redirect target on the curl source at byte `at`. The reservation publishes
/// teardown's wake target BEFORE the abort check — the order `demux`'s own TLS open keeps.
fn open_curl_hop(
    range: &dyn RangeFactory,
    url: &str,
    at: i64,
    aq: *mut AuQueue,
) -> Result<Box<dyn RangeReader>, range_io::OpenErr> {
    let reservation = range.reserve_open()?;
    if unsafe { crate::aq::aq_is_aborted(aq) } {
        return Err(range_io::OpenErr::Aborted);
    }
    range.open_reserved(url, at, reservation)
}

/// The progressive (direct-play) open of a PLAINTEXT part URL, redirects followed. A PMS-hosted
/// trailer answers its Part URL with a `302` to a presigned CDN URL — usually https, which lands
/// on the curl source. Publishes the same two diagnostics the TLS arm of `demux` does.
pub fn open_plain_progressive(
    hs_p: *mut HttpStream,
    origin: &Origin,
    path: &str,
    aq_p: *mut AuQueue,
    transport: &DemuxTransport,
    observer: &dyn DemuxObserver,
) -> Result<(Src, i64), MediaOpenFail> {
    use plx_net::stream::redirect::Opened;
    plx_net::stream::http_close(hs_p);
    let req = plx_net::stream::redirect::Request {
        origin,
        path,
        credentials: None,
        range_from: None,
        deadline: None,
        same_origin_only: false,
        credential_gate: transport.credential_gate,
    };
    match plx_net::stream::redirect::open_following(hs_p, &req, &mut plx_base::checkpoint::NoCheckpoint)
    {
        Ok(Opened::Socket(t)) => {
            let size = plx_net::stream::hs_content_length(hs_p);
            observer.publish(DemuxCallbackEffect::FileSize(size));
            let st = plx_net::stream::hs_status(hs_p);
            observer.publish(DemuxCallbackEffect::HttpStatus(st));
            plx_base::eventlog::log(&format!("ff: open status={st} clen={size}"));
            let (Ok(host), Ok(path)) = (CString::new(t.origin.host()), CString::new(t.path)) else {
                return Err(MediaOpenFail::Failed);
            };
            Ok((
                Src::Socket {
                    hs: hs_p,
                    host,
                    port: t.origin.port() as c_int,
                    path,
                },
                size,
            ))
        }
        Ok(Opened::Tls(t)) => match open_curl_hop(transport.range.as_ref(), &t.url(), 0, aq_p) {
            Ok(cs) => {
                let (st, size) = (cs.status(), cs.size());
                observer.publish(DemuxCallbackEffect::FileSize(size));
                observer.publish(DemuxCallbackEffect::HttpStatus(st));
                plx_base::eventlog::log(&format!("ff: open https status={st} clen={size}"));
                Ok((Src::Curl(cs), size))
            }
            Err(range_io::OpenErr::Aborted) => Err(MediaOpenFail::Aborted),
            Err(e) => {
                if let range_io::OpenErr::Status(st) = e {
                    observer.publish(DemuxCallbackEffect::HttpStatus(st));
                }
                plx_base::eventlog::log(&format!("ff: https open FAILED: {e:?}"));
                Err(MediaOpenFail::Failed)
            }
        },
        Err(e) => {
            if unsafe { crate::aq::aq_is_aborted(aq_p) } {
                return Err(MediaOpenFail::Aborted);
            }
            let st = plx_net::stream::hs_status(hs_p);
            observer.publish(DemuxCallbackEffect::HttpStatus(st));
            plx_base::eventlog::log(&format!("ff: http_open FAILED status={st} ({e:?})"));
            Err(MediaOpenFail::Failed)
        }
    }
}

pub fn hls_source_read(
    src: &mut Src,
    aq: *mut AuQueue,
    dst: &mut [u8],
    deadline: Option<std::time::Instant>,
    observer: &dyn DemuxObserver,
) -> Result<usize, HlsExit> {
    if unsafe { crate::aq::aq_is_aborted(aq) } {
        if let Src::Curl(cs) = src {
            cs.abort();
        }
        return Err(HlsExit::Aborted);
    }
    let read = match src {
        Src::Socket { hs, .. } => plx_net::stream::http_read_until(
            *hs,
            dst.as_mut_ptr(),
            dst.len() as c_int,
            deadline,
            &mut plx_base::checkpoint::NoCheckpoint,
        ),
        Src::Curl(cs) => cs.read_until(dst, deadline, &mut plx_base::checkpoint::NoCheckpoint),
        Src::Idle => return Err(HlsExit::Failed("HLS source idle")),
    };
    // The wake used to interrupt a blocked body read is deliberately transport-shaped (EOF for
    // shutdown(2), negative for curl). Re-read the lane flag before interpreting that shape so a
    // user teardown can never become failed/censored playback evidence.
    if unsafe { crate::aq::aq_is_aborted(aq) } {
        if let Src::Curl(cs) = src {
            cs.abort();
        }
        return Err(HlsExit::Aborted);
    }
    if observer.hold_epoch().is_none() {
        return Err(HlsExit::Aborted);
    }
    if read == plx_net::stream::HTTP_READ_DEADLINE || read == range_io::READ_DEADLINE {
        Err(HlsExit::PrimeExpired)
    } else if read < 0 {
        Err(HlsExit::Failed("HLS response body failed"))
    } else {
        // The progressive AVIO path increments this counter in `read_cb`; segmented HLS bypasses
        // that callback and used to leave Stats for Nerds' Network Activity lane flat at zero
        // while visibly downloading playlists and segments. Count at the shared source seam so
        // socket and curl HLS mean the same thing: bytes actually handed to the demux fetcher.
        if read > 0 {
            if !observer.publish(DemuxCallbackEffect::NetworkBytes(read as i64)) {
                return Err(HlsExit::Aborted);
            }
        }
        Ok(read as usize)
    }
}

fn hls_fetch_text(
    resource: &crate::hls::Resource,
    auth: &crate::hls::InheritedAuth,
    aq: *mut AuQueue,
    net: &mut HlsNet,
    mut reserve: Option<&mut ReserveDeadlineState>,
) -> Result<(String, u128, crate::hls::Resource), HlsExit> {
    const MAX_PLAYLIST_BYTES: usize = 1024 * 1024;
    let request_path = auth
        .request_path(resource)
        .map_err(|_| HlsExit::Failed("playlist credential rejected"))?;
    let started = std::time::Instant::now();
    let mut watchdog = TransportWatchdog::for_origin(&resource.origin);
    let (mut src, size, landed) = loop {
        if unsafe { crate::aq::aq_is_aborted(aq) } {
            return Err(HlsExit::Aborted);
        }
        if net.observer.hold_epoch().is_none() { return Err(HlsExit::Aborted); }
        if reserve
            .as_deref_mut()
            .is_some_and(|reserve| reserve.expire_if_due(false))
        {
            return Err(HlsExit::PrimeExpired);
        }
        let reserve_snapshot = reserve
            .as_deref_mut()
            .and_then(|reserve| reserve.active(false));
        let effective = watchdog.effective(reserve_snapshot);
        // A playlist is not a segment acquisition: it has no reserve-funded guard to consult, and
        // its own reserve deadline and watchdog already bound every wait. `NoCheckpoint` keeps it
        // exactly as it was.
        let opened = hls_open_source(
            resource,
            &request_path,
            aq,
            net,
            Some(effective.at),
            &mut plx_base::checkpoint::NoCheckpoint,
        );
        if net.observer.hold_epoch().is_none() { return Err(HlsExit::Aborted); }
        match opened {
            Ok(opened) => {
                // A complete response head is transport progress and begins a fresh inactivity
                // epoch for the body. Neither a stale reserve wake nor a reconnect attempt does.
                watchdog.reset();
                break opened;
            }
            Err(HlsExit::PrimeExpired) => {
                let wake =
                    observe_hls_deadline(reserve.as_deref_mut(), effective, &watchdog, false);
                if let Some(error) = classify_hls_deadline(wake) {
                    return Err(error);
                }
                // The wall projection fired while its playhead clock remained unspent. GET is
                // idempotent, so retry it with the same liveness epoch and a fresh projection.
            }
            Err(error) => return Err(error),
        }
    };
    if size > MAX_PLAYLIST_BYTES as i64 {
        return Err(HlsExit::Failed("playlist exceeds size cap"));
    }
    let mut body = Vec::with_capacity(if size > 0 { size as usize } else { 4096 });
    let mut chunk = [0u8; 16 * 1024];
    loop {
        if unsafe { crate::aq::aq_is_aborted(aq) } {
            return Err(HlsExit::Aborted);
        }
        if net.observer.hold_epoch().is_none() { return Err(HlsExit::Aborted); }
        if reserve
            .as_deref_mut()
            .is_some_and(|reserve| reserve.expire_if_due(false))
        {
            return Err(HlsExit::PrimeExpired);
        }
        let reserve_snapshot = reserve
            .as_deref_mut()
            .and_then(|reserve| reserve.active(false));
        let effective = watchdog.effective(reserve_snapshot);
        let n = match hls_source_read(&mut src, aq, &mut chunk, Some(effective.at),
            net.observer.as_ref()) {
            Ok(n) => n,
            Err(HlsExit::PrimeExpired) => {
                let wake =
                    observe_hls_deadline(reserve.as_deref_mut(), effective, &watchdog, false);
                if let Some(error) = classify_hls_deadline(wake) {
                    return Err(error);
                }
                // The source stays open across a body wake. Retrying here avoids both a new GET
                // and a renewed liveness lease.
                continue;
            }
            Err(error) => return Err(error),
        };
        if n == 0 {
            break;
        }
        watchdog.reset();
        if body.len().saturating_add(n) > MAX_PLAYLIST_BYTES {
            return Err(HlsExit::Failed("playlist exceeds size cap"));
        }
        body.extend_from_slice(&chunk[..n]);
    }
    if !hls_body_complete(size, body.len() as i64) {
        return Err(HlsExit::Failed("playlist body ended before Content-Length"));
    }
    hls_recycle_src(src, net);
    let elapsed = started.elapsed().as_millis();
    String::from_utf8(body)
        .map(|text| (text, elapsed, landed))
        .map_err(|_| HlsExit::Failed("playlist is not UTF-8"))
}

/// Sleep `duration` (never past `absolute_deadline`) in 50 ms slices, consulting `checkpoint` as
/// its `next_check` falls due. A recheck never moves the wait's end; a stop returns
/// [`UNLATCHED_STOP`], which a caller with a runtime replaces with the reason it latched.
pub fn hls_wait(
    aq: *mut AuQueue,
    duration: std::time::Duration,
    absolute_deadline: Option<std::time::Instant>,
    checkpoint: &mut dyn plx_base::checkpoint::Checkpoint,
) -> Result<(), HlsExit> {
    let nominal_end = std::time::Instant::now() + duration;
    let wait_until = absolute_deadline.map_or(nominal_end, |at| at.min(nominal_end));
    let mut pacer = plx_base::checkpoint::Pacer::new(checkpoint);
    while std::time::Instant::now() < wait_until {
        if unsafe { crate::aq::aq_is_aborted(aq) } {
            return Err(HlsExit::Aborted);
        }
        let Ok(recheck) = pacer.before_wait() else {
            return Err(UNLATCHED_STOP);
        };
        let now = std::time::Instant::now();
        let mut slice = wait_until
            .saturating_duration_since(now)
            .min(std::time::Duration::from_millis(50));
        if let Some(at) = recheck {
            slice = slice.min(
                at.saturating_duration_since(now)
                    .max(std::time::Duration::from_millis(1)),
            );
        }
        std::thread::sleep(slice);
    }
    if unsafe { crate::aq::aq_is_aborted(aq) } {
        Err(HlsExit::Aborted)
    } else if absolute_deadline.is_some_and(|at| std::time::Instant::now() >= at) {
        Err(HlsExit::PrimeExpired)
    } else {
        Ok(())
    }
}

/// One MPEG-TS segment's entirely private FFmpeg state. PMS HLS segments are independent decode
/// units; retaining an AVFormatContext across them teaches libavformat a byte stream that does not
/// exist and makes timestamp resets/container EOF ambiguous. The custom AVIO remains transport-
/// owned exactly as in the progressive path, but every field here is retired at the segment
/// boundary before the next request starts.
struct HlsInput {
    state: Box<AvioState>,
    avio: *mut AVIOContext,
    fmt: *mut AVFormatContext,
    pkt: *mut AVPacket,
}

impl Drop for HlsInput {
    fn drop(&mut self) {
        unsafe {
            if !self.pkt.is_null() {
                av_packet_free(&mut self.pkt);
            }
            if !self.fmt.is_null() {
                avformat_close_input(&mut self.fmt);
            }
            free_avio(self.avio);
        }
        // `state` deliberately drops last: AVIO's opaque pointer names it until `free_avio`.
        let _ = &self.state;
    }
}

/// **What the HLS demuxer actually found, logged when it CHANGES.** The progressive path has said
/// this since it was written (`ff: v=#0 codec=… WxH`); the adaptive path never has, so on every
/// HLS playback — both tiers — nothing in the log stated the codec or the raster of the stream
/// being decoded. `abr: committed … 1280x720` is the CATALOG raster of the rung that was asked
/// for, which is a different claim: it is what we requested, not what arrived.
/// `docs/adaptive-playback-plan.md` §7.B names that gap.
///
/// Emitted on change rather than per segment, because a segment open happens every two seconds
/// and the interesting event is the raster moving. The line's SHAPE is deliberately identical to
/// the progressive one so `tests/run.py`'s `RE_CODEC` reads both without a second regex — which
/// is also why the stream index is written `#0` rather than the discovered one: it is the video
/// stream, and every consumer of that field treats it that way.
static HLS_LAST_VIDEO: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);

unsafe fn log_hls_video_change(fmt: *mut AVFormatContext, duration_ns: i64) {
    let streams = (*fmt).streams;
    for i in 0..(*fmt).nb_streams {
        let cp = stream_codecpar(*streams.add(i as usize));
        if (*cp).codec_type != AVMEDIA_TYPE_VIDEO {
            continue;
        }
        let (id, w, h) = ((*cp).codec_id, (*cp).width, (*cp).height);
        // Pack (codec_id, width, height) into one word so the compare-and-store is a single
        // atomic and two threads cannot interleave a half-updated triple.
        let key =
            ((id as u64 & 0xffff) << 48) | ((w as u64 & 0xffff_ffff) << 16) | (h as u64 & 0xffff);
        if HLS_LAST_VIDEO.swap(key, Ordering::Relaxed) == key {
            return;
        }
        let cname = std::ffi::CStr::from_ptr(avcodec_get_name(id)).to_string_lossy();
        plx_base::eventlog::log(&format!(
            "ff: v=#0 codec={cname} codec_id={id} {w}x{h} trc={} pri={} spc={} a=#1 dur_ns={}",
            (*cp).color_trc,
            (*cp).color_primaries,
            (*cp).color_space,
            duration_ns
        ));
        return;
    }
}

unsafe fn hls_input(
    src: Src,
    observer: Arc<dyn DemuxObserver>,
    size: i64,
    aq: *mut AuQueue,
    reserve_deadline: ReserveDeadlineState,
    transport_watchdog: TransportWatchdog,
    mut acquisition: AcquisitionRuntime,
    net: &HlsNet,
) -> Result<HlsInput, HlsExit> {
    let request_started = acquisition.request_started();
    acquisition.enter(Phase::Body);
    let mut input = HlsInput {
        state: Box::new(AvioState {
            src,
            range: Arc::clone(&net.range),
            credential_gate: net.credential_gate,
            observer,
            aq,
            off: 0,
            size,
            io_failed: false,
            body_active_us: 0,
            body_bytes: 0,
            first_byte_at: None,
            reserve_deadline,
            transport_watchdog: Some(transport_watchdog),
            acquisition: Some(acquisition),
            bounce: Vec::new(),
            bounce_pos: 0,
            parked_at_end: false,
            body_silence: BODY_SILENCE,
            heal: HealBudget::default(),
        }),
        avio: std::ptr::null_mut(),
        fmt: std::ptr::null_mut(),
        pkt: std::ptr::null_mut(),
    };
    let buf = av_malloc(64 * 1024) as *mut u8;
    if buf.is_null() {
        return Err(HlsExit::Failed("segment AVIO buffer allocation failed"));
    }
    input.avio = avio_alloc_context(
        buf,
        64 * 1024,
        0,
        &mut *input.state as *mut AvioState as *mut c_void,
        Some(read_cb),
        None,
        // A segment is a forward-only object. Letting libavformat seek would generate hidden
        // Range requests and could accidentally splice bytes from a different encoder object.
        None,
    );
    if input.avio.is_null() {
        free_ptr(buf as *mut c_void);
        return Err(HlsExit::Failed("segment AVIO allocation failed"));
    }
    input.fmt = avformat_alloc_context();
    if input.fmt.is_null() {
        return Err(HlsExit::Failed("segment format allocation failed"));
    }
    (*input.fmt).pb = input.avio;
    input.state.io_failed = false;
    let opened = avformat_open_input(
        &mut input.fmt,
        std::ptr::null(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );
    if crate::aq::aq_is_aborted(aq) {
        return Err(HlsExit::Aborted);
    }
    let Some(audio_expected) = net.observer.audio_expected() else {
        return Err(HlsExit::Aborted);
    };
    if let Some(exit) = classify_hls_avio_operation(
        &input.state,
        opened < 0 || input.fmt.is_null(),
        request_started,
        audio_expected && FEED_AUDIO.load(Ordering::Relaxed),
    ) {
        return Err(exit);
    }
    if opened < 0 || input.fmt.is_null() {
        return Err(HlsExit::Failed("segment open/probe failed"));
    }
    // A successful enclosing FFmpeg operation healed any callback-level read retry.
    input.state.io_failed = false;
    let discovered = avformat_find_stream_info(input.fmt, std::ptr::null_mut());
    if crate::aq::aq_is_aborted(aq) {
        return Err(HlsExit::Aborted);
    }
    let Some(audio_expected) = net.observer.audio_expected() else {
        return Err(HlsExit::Aborted);
    };
    if let Some(exit) = classify_hls_avio_operation(
        &input.state,
        discovered < 0,
        request_started,
        audio_expected && FEED_AUDIO.load(Ordering::Relaxed),
    ) {
        return Err(exit);
    }
    if discovered < 0 {
        return Err(HlsExit::Failed("segment stream discovery failed"));
    }
    input.state.io_failed = false;
    let Some(duration_ns) = net.observer.duration_ns() else {
        return Err(HlsExit::Aborted);
    };
    log_hls_video_change(input.fmt, duration_ns);
    input.pkt = av_packet_alloc();
    if input.pkt.is_null() {
        return Err(HlsExit::Failed("segment packet allocation failed"));
    }
    Ok(input)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentTransfer {
    pub bytes: u64,
    pub active_us: u64,
    pub total_us: u64,
    pub audio_expected: bool,
}

pub struct HlsAu {
    pub data: Vec<u8>,
    pub pts_ns: i64,
    pub key: c_int,
    pub es: c_int,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AudioStamp {
    au: usize,
    raw_ns: Option<i64>,
    duration_ns: Option<i64>,
}

/// MPEG-TS may put a PES timestamp on the second AAC access unit while the first unit carries
/// only its frame duration. Resolve those holes inside one complete segment before mapping the
/// audio lane onto the normalized content timeline. We never borrow video PTS or wall time.
fn resolve_audio_stamps(stamps: &mut [AudioStamp]) -> Result<usize, &'static str> {
    let missing = stamps.iter().filter(|stamp| stamp.raw_ns.is_none()).count();
    if stamps.is_empty() || missing == 0 {
        return Ok(missing);
    }

    // A timestamped predecessor plus its own duration determines this packet's start.
    for index in 1..stamps.len() {
        if stamps[index].raw_ns.is_none() {
            if let (Some(previous), Some(duration)) =
                (stamps[index - 1].raw_ns, stamps[index - 1].duration_ns)
            {
                stamps[index].raw_ns = previous.checked_add(duration);
            }
        }
    }
    // A timestamped successor minus this packet's duration determines a leading hole.
    for index in (0..stamps.len().saturating_sub(1)).rev() {
        if stamps[index].raw_ns.is_none() {
            if let (Some(next), Some(duration)) =
                (stamps[index + 1].raw_ns, stamps[index].duration_ns)
            {
                stamps[index].raw_ns = next.checked_sub(duration);
            }
        }
    }
    // A segment with no PES timestamp at all still has an exact AAC frame clock. Anchor its
    // first frame locally at zero; SegmentClock supplies the content-time base.
    if stamps[0].raw_ns.is_none() && stamps.iter().all(|stamp| stamp.raw_ns.is_none()) {
        stamps[0].raw_ns = Some(0);
        for index in 1..stamps.len() {
            if let (Some(previous), Some(duration)) =
                (stamps[index - 1].raw_ns, stamps[index - 1].duration_ns)
            {
                stamps[index].raw_ns = previous.checked_add(duration);
            }
        }
    }
    if stamps.iter().any(|stamp| stamp.raw_ns.is_none()) {
        return Err("AAC timestamp hole has no duration anchor");
    }
    if stamps
        .windows(2)
        .any(|pair| pair[1].raw_ns.expect("resolved") < pair[0].raw_ns.expect("resolved"))
    {
        return Err("AAC timestamps move backwards");
    }
    Ok(missing)
}

pub struct HlsSegmentOutput {
    pub aus: Vec<HlsAu>,
    pub transfer: SegmentTransfer,
    pub video_width: i32,
    pub video_height: i32,
    pub video_tail_ns: i64,
    pub audio_tail_ns: Option<i64>,
}

/// A body with a declared length is complete only after every declared byte reached AVIO.  FFmpeg
/// finding some packets is not a substitute: a socket timeout used to look like EOF, after which
/// the caller credited the segment's full EXTINF and manufactured playable reserve that was never
/// downloaded.
pub fn hls_body_complete(declared_size: i64, delivered: i64) -> bool {
    declared_size < 0 || delivered >= declared_size
}

fn settle_hls_segment(
    runtime: &mut AcquisitionRuntime,
    error: HlsExit,
    observer: &dyn DemuxObserver,
) -> HlsExit {
    let Some(expected) = observer.audio_expected() else {
        return HlsExit::Aborted;
    };
    runtime.settle(error, expected && FEED_AUDIO.load(Ordering::Relaxed))
}

pub unsafe fn hls_demux_segment(
    segment: &crate::hls::Segment,
    auth: &crate::hls::InheritedAuth,
    clock: &mut crate::hls::SegmentClock,
    aq: *mut AuQueue,
    net: &mut HlsNet,
    acodec: &str,
    acquisition: SegmentAcquisition,
) -> Result<HlsSegmentOutput, HlsExit> {
    // The ONE runtime for this fetch: the open and the `NotReady` wait below consult it as their
    // checkpoint, then it moves into the AVIO for the probe and body reads — never re-armed.
    let (mut reserve_deadline, mut runtime) = acquisition.begin(aq);
    if crate::aq::aq_is_aborted(aq) {
        return Err(HlsExit::Aborted);
    }
    let path = auth
        .request_path(&segment.resource)
        .map_err(|_| HlsExit::Failed("segment credential rejected"))?;
    let request_started = runtime.request_started();
    let retry_budget = segment
        .duration
        .saturating_mul(3)
        .saturating_add(std::time::Duration::from_secs(2))
        .clamp(
            std::time::Duration::from_secs(3),
            std::time::Duration::from_secs(15),
        );
    let mut not_ready_retries = 0u32;
    let mut transport_watchdog = TransportWatchdog::for_origin(&segment.resource.origin);
    // The SUCCESSFUL attempt only. A `NotReady` retry is the server saying the segment does not
    // exist yet, which is production latency of a different kind and is already counted by
    // `not_ready=`; folding it in here would make one number mean two things.
    let open_us;
    let (src, size) = loop {
        if crate::aq::aq_is_aborted(aq) {
            return Err(HlsExit::Aborted);
        }
        if reserve_deadline.expire_if_due(
            net.observer.rebuffering().ok_or(HlsExit::Aborted)?
        ) {
            return Err(settle_hls_segment(&mut runtime, HlsExit::PrimeExpired,
                net.observer.as_ref()));
        }
        let attempt = std::time::Instant::now();
        let rebuffering = net.observer.rebuffering().ok_or(HlsExit::Aborted)?;
        let reserve_snapshot = reserve_deadline.active(rebuffering);
        let effective = transport_watchdog.effective(reserve_snapshot);
        runtime.enter(Phase::Opening);
        let opened = hls_open_source(
            &segment.resource,
            &path,
            aq,
            net,
            Some(effective.at),
            &mut runtime,
        );
        if net.observer.hold_epoch().is_none() { return Err(HlsExit::Aborted); }
        // Every unsuccessful open is SETTLED by a fresh check before it is classified: a transport
        // deadline shorter than the checkpoint slice ends the wait without asking again.
        let opened = opened.map_err(|error| settle_hls_segment(&mut runtime, error,
            net.observer.as_ref()));
        match opened {
            // A segment has no children, so where a redirect landed is of no further use.
            Ok((src, size, _landed)) => {
                transport_watchdog.reset();
                open_us = attempt.elapsed().as_micros() as u64;
                break (src, size);
            }
            // The blocking open may have returned on the old reserve deadline in the same pump
            // tick that established the internal hold. Re-open under the promoted policy; the
            // ordinary transport liveness bound remains, only the rollback deadline disappears.
            Err(HlsExit::PrimeExpired) => {
                let current_rebuffering = net.observer.rebuffering().ok_or(HlsExit::Aborted)?;
                let wake = observe_hls_deadline(
                    Some(&mut reserve_deadline),
                    effective,
                    &transport_watchdog,
                    current_rebuffering,
                );
                if let Some(error) = classify_hls_deadline(wake) {
                    return Err(error);
                }
                continue;
            }
            // **The retry is INSIDE the caller's deadline, and it was not.** `retry_budget` runs
            // to 15 s and was the only bound here, while `deadline` reached only as far as
            // `hls_input` — which is constructed after this loop exits. So a candidate whose
            // deadline was three seconds could spend fifteen looping on `NotReady` before the
            // deadline had any effect at all, which is R19's "the `NotReady` retry has no leg of
            // its own" seen from the enforcement side rather than the accounting side. A retry
            // budget that can outlive the reserve it is spending is not a budget.
            Err(HlsExit::NotReady) if request_started.elapsed() < retry_budget => {
                not_ready_retries = not_ready_retries.saturating_add(1);
                transport_watchdog.reset();
                if crate::aq::aq_is_aborted(aq) {
                    return Err(HlsExit::Aborted);
                }
                if reserve_deadline.expire_if_due(
                    net.observer.rebuffering().ok_or(HlsExit::Aborted)?
                ) {
                    return Err(settle_hls_segment(&mut runtime, HlsExit::PrimeExpired,
                        net.observer.as_ref()));
                }
                // Never sleep past the deadline. A fixed 250 ms wait against a deadline 40 ms away
                // overshoots by 210 ms of reserve, every time, for no information — the poll after
                // it can only be discarded.
                let mut wait = std::time::Duration::from_millis(250);
                let rebuffering = net.observer.rebuffering().ok_or(HlsExit::Aborted)?;
                let reserve_snapshot = reserve_deadline.active(rebuffering);
                let effective = transport_watchdog.effective(reserve_snapshot);
                wait = wait.min(
                    effective
                        .at
                        .saturating_duration_since(std::time::Instant::now()),
                );
                runtime.enter(Phase::RetryWaiting);
                let waited = hls_wait(aq, wait, Some(effective.at), &mut runtime)
                    .map_err(|error| settle_hls_segment(&mut runtime, error,
                        net.observer.as_ref()));
                match waited {
                    Err(HlsExit::PrimeExpired) => {
                        let current_rebuffering = net.observer.rebuffering().ok_or(HlsExit::Aborted)?;
                        let wake = observe_hls_deadline(
                            Some(&mut reserve_deadline),
                            effective,
                            &transport_watchdog,
                            current_rebuffering,
                        );
                        if let Some(error) = classify_hls_deadline(wake) {
                            return Err(error);
                        }
                    }
                    result => result?,
                }
            }
            Err(HlsExit::NotReady) => {
                return Err(HlsExit::Failed("HLS segment was not produced in time"));
            }
            Err(error) => return Err(error),
        }
    };
    let body_started = std::time::Instant::now();
    let mut input = hls_input(src, net.observer.clone(), size, aq, reserve_deadline, transport_watchdog, runtime, net)?;
    let probe_done = std::time::Instant::now();
    let streams = (*input.fmt).streams;
    let vi = av_find_best_stream(
        input.fmt,
        AVMEDIA_TYPE_VIDEO,
        -1,
        -1,
        std::ptr::null_mut(),
        0,
    );
    let ai = audio_stream_matching(input.fmt, acodec).unwrap_or_else(|| {
        av_find_best_stream(
            input.fmt,
            AVMEDIA_TYPE_AUDIO,
            -1,
            -1,
            std::ptr::null_mut(),
            0,
        )
    });
    if vi < 0 {
        return Err(HlsExit::Failed("HLS segment has no video stream"));
    }
    let vst = *streams.add(vi as usize);
    let vcp = stream_codecpar(vst);
    if (*vcp).codec_id != AV_CODEC_ID_H264 {
        return Err(HlsExit::Failed("HLS segment video is not H.264"));
    }
    let aac_adts = if ai >= 0 {
        let acp = stream_codecpar(*streams.add(ai as usize));
        if (*acp).codec_id != AV_CODEC_ID_AAC {
            return Err(HlsExit::Failed("HLS segment audio is not AAC"));
        }
        adts_params(acp)
    } else {
        None
    };

    let video_width = (*vcp).width;
    let video_height = (*vcp).height;
    if video_width <= 0 || video_height <= 0 {
        return Err(HlsExit::Failed("HLS segment has invalid video dimensions"));
    }
    let mut params = H264ParamSets::default();
    let mut aubuf = Vec::with_capacity(4 * 1024 * 1024);
    let mut video_packets = 0usize;
    let mut audio_packets = 0usize;
    let mut audio_stamps = Vec::new();
    let mut first_au_at = None;
    let mut aus = Vec::new();
    let mut video_tail_ns = -1;
    let mut audio_tail_ns = None;

    loop {
        input.state.io_failed = false;
        let read = av_read_frame(input.fmt, input.pkt);
        if crate::aq::aq_is_aborted(aq) {
            return Err(HlsExit::Aborted);
        }
        if let Some(exit) = classify_hls_avio_operation(
            &input.state,
            read < 0,
            request_started,
            ai >= 0 && FEED_AUDIO.load(Ordering::Relaxed),
        ) {
            return Err(exit);
        }
        if read < 0 {
            break;
        }
        let si = (*input.pkt).stream_index;
        if si == vi {
            let Some(raw_pts) = pts_ns_opt(input.pkt, vst) else {
                av_packet_unref(input.pkt);
                return Err(HlsExit::Failed("HLS video packet has no timestamp"));
            };
            let is_key = ts_h264_access_unit(
                std::slice::from_raw_parts((*input.pkt).data, (*input.pkt).size.max(0) as usize),
                &mut params,
                &mut aubuf,
            )
            .map_err(HlsExit::Failed)?;
            if video_packets == 0 && !is_key {
                av_packet_unref(input.pkt);
                return Err(HlsExit::Failed("HLS segment does not begin with an IDR"));
            }
            let pts = clock.normalize_video(raw_pts).0.saturating_mul(1_000_000);
            aus.push(HlsAu {
                data: aubuf.clone(),
                pts_ns: pts,
                key: if is_key { 1 } else { 0 },
                es: 1,
            });
            video_tail_ns = video_tail_ns.max(pts);
            video_packets += 1;
            first_au_at.get_or_insert_with(std::time::Instant::now);
            av_packet_unref(input.pkt);
        } else if si == ai && FEED_AUDIO.load(Ordering::Relaxed) {
            let ast = *streams.add(ai as usize);
            let raw_pts = pts_ns_opt(input.pkt, ast);
            let packet_duration = ((*input.pkt).duration > 0)
                .then(|| av_rescale_q((*input.pkt).duration, stream_time_base(ast), NS_TB))
                .filter(|duration| *duration > 0);
            let aac_duration = aac_adts.and_then(|(freq_idx, _)| {
                const RATES: [i64; 13] = [
                    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000,
                    11_025, 8_000, 7_350,
                ];
                RATES
                    .get(freq_idx as usize)
                    .map(|rate| 1_024_i64.saturating_mul(1_000_000_000) / rate)
            });
            let raw =
                std::slice::from_raw_parts((*input.pkt).data, (*input.pkt).size.max(0) as usize);
            let duration_ns = packet_duration
                .or_else(|| adts_duration_ns(raw))
                .or(aac_duration);
            let Some(data) = adts_framed(raw, aac_adts).map(std::borrow::Cow::into_owned) else {
                av_packet_unref(input.pkt);
                return Err(HlsExit::Failed("AAC packet is neither ADTS nor reframable"));
            };
            let au = aus.len();
            aus.push(HlsAu {
                data,
                pts_ns: 0,
                key: 1,
                es: 2,
            });
            audio_stamps.push(AudioStamp {
                au,
                raw_ns: raw_pts,
                duration_ns,
            });
            audio_packets += 1;
            av_packet_unref(input.pkt);
        } else {
            av_packet_unref(input.pkt);
        }
        if crate::aq::aq_is_aborted(aq) {
            return Err(HlsExit::Aborted);
        }
    }
    if crate::aq::aq_is_aborted(aq) {
        return Err(HlsExit::Aborted);
    }
    if input
        .state
        .reserve_deadline
        .expire_if_due(net.observer.rebuffering().ok_or(HlsExit::Aborted)?)
    {
        return Err(HlsExit::PrimeExpired);
    }

    if !hls_body_complete(input.state.size, input.state.off) {
        plx_base::eventlog::log(&format!(
            "hls: short-body segment={} got={} want={}",
            segment.sequence, input.state.off, input.state.size,
        ));
        return Err(HlsExit::Failed("segment body ended before Content-Length"));
    }

    if video_packets == 0 {
        return Err(HlsExit::Failed(
            "HLS segment produced no video access units",
        ));
    }
    let synthesized_audio = resolve_audio_stamps(&mut audio_stamps).map_err(HlsExit::Failed)?;
    for stamp in &audio_stamps {
        let pts = clock
            .normalize_audio(stamp.raw_ns.expect("audio stamps resolved"))
            .0
            .saturating_mul(1_000_000);
        aus[stamp.au].pts_ns = pts;
        audio_tail_ns = Some(audio_tail_ns.map_or(pts, |tail: i64| tail.max(pts)));
    }
    // Every AU in this independently decodable segment has been read successfully. Buffer health
    // is the end of decoded content, not the start timestamp of the final video frame; using the
    // latter made a complete 2 s / 24 fps segment look 42 ms short and forced an immediate drop.
    let segment_end_ns = clock.end().0.saturating_mul(1_000_000);
    video_tail_ns = video_tail_ns.max(segment_end_ns);
    if audio_packets > 0 {
        audio_tail_ns = Some(audio_tail_ns.map_or(segment_end_ns, |tail| tail.max(segment_end_ns)));
    }
    let first_ms = first_au_at
        .map(|at| at.duration_since(request_started).as_millis())
        .unwrap_or_default();
    let transfer = SegmentTransfer {
        bytes: input.state.body_bytes,
        active_us: input.state.body_active_us,
        total_us: request_started.elapsed().as_micros().max(1) as u64,
        audio_expected: ai >= 0 && FEED_AUDIO.load(Ordering::Relaxed),
    };
    plx_base::eventlog::log(&format!(
        "hls: segment={} bytes={} raster={}x{} v={} a={} tail_skew_ms={} audio_pts_recovered={} \
         not_ready={} open_ms={} ttfb_ms={} open_probe_ms={} first_au_ms={} total_ms={}",
        segment.sequence,
        transfer.bytes,
        video_width,
        video_height,
        video_packets,
        audio_packets,
        audio_tail_ns.map_or(0, |audio| (video_tail_ns - audio) / 1_000_000),
        synthesized_audio,
        not_ready_retries,
        open_us / 1_000,
        input
            .state
            .first_byte_at
            .map(|at| at.duration_since(body_started).as_millis() as i64)
            .unwrap_or(-1),
        probe_done.duration_since(body_started).as_millis(),
        first_ms,
        transfer.total_us / 1_000,
    ));
    if let Some(cs) = input.state.src.take_curl() {
        net.curl = Some(cs);
    }
    Ok(HlsSegmentOutput {
        aus,
        transfer,
        video_width,
        video_height,
        video_tail_ns,
        audio_tail_ns,
    })
}

fn hls_feed_segment(
    output: &HlsSegmentOutput,
    aq: *mut AuQueue,
    aqa: *mut AuQueue,
    session: &mut dyn DemuxSession,
) -> Result<(), HlsExit> {
    session.media().publish(DemuxEffect::AudioExpected(output.transfer.audio_expected));
    // Close the race with the main-thread pump: before the first AU can satisfy the decoder prime,
    // publish how long this acquisition actually cost.  The pump then requires at least that much
    // playable media.  Do NOT lower the published value at the end of this function: the AUs are
    // then in our queues, not necessarily accepted by Starfish yet, and the controller has not yet
    // replaced the previous finite-bag runway with the window that includes this acquisition.  A
    // store here opened a small but real interval in which the pump could start against only the
    // latest (cheaper) sample.  `Controller::prime_runway_ms` publishes the new complete value
    // immediately after `observe` below.
    let acquisition_ms =
        i64::try_from(output.transfer.total_us.saturating_add(999) / 1_000).unwrap_or(i64::MAX);
    session.media().publish(DemuxEffect::PrimeRunwayMax(acquisition_ms));
    // A candidate is demuxed before it owns playback. Publish one coherent raster only after its
    // first video AU is accepted by the active queue; a rejected candidate and a failed first push
    // must remain invisible to the video-plane actuator.
    let mut raster_published = false;
    for au in &output.aus {
        let queue = if au.es == 1 { aq } else { aqa };
        if crate::aq::aq_push(
            queue,
            au.data.as_ptr(),
            au.data.len() as c_int,
            au.pts_ns,
            au.key,
            au.es,
        ) != 0
        {
            return Err(HlsExit::Aborted);
        }
        if au.es == 1 {
            if !raster_published {
                session.media().publish(DemuxEffect::VideoRaster {
                    width: output.video_width, height: output.video_height,
                });
                raster_published = true;
            }
            PUSHED_ANY.store(true, Ordering::Relaxed);
            session.media().publish(DemuxEffect::VideoTailNs(au.pts_ns));
        } else {
            session.media().publish(DemuxEffect::AudioTailNs(au.pts_ns));
        }
    }
    Ok(())
}

pub struct HlsCursor {
    /// Whether this cursor's media playlist is the one describing WHAT IS PLAYING.
    ///
    /// A candidate cursor opened during a quality transaction reads a freshly created encoder
    /// session, whose playlist lists only the segments PMS has produced so far — so its
    /// `total_duration()` is a fraction of the film's. Publishing that clobbers
    /// `FEED_SHARED.duration_ns`, and on a reject nothing ever restores it: the remaining-playback
    /// estimate, the HUD's total and the scrobble all inherit the truncated number for the rest
    /// of the playback. The candidate is promoted to publishing at the moment it is committed.
    pub publishes_duration: bool,
    /// **The rate this rendition's master playlist DECLARED**, bit/s, `#EXT-X-STREAM-INF:BANDWIDTH`.
    ///
    /// Retained rather than logged and dropped, which is what happened to it until now. It is the
    /// only per-rung rate this app can obtain that is not the catalog's `expected_wire_kbps`, whose
    /// error is +5.2% to +31.6%, item-dependent and non-injective (rungs 18000 and 20000 both
    /// declare 16 150). Live candidate acceptance uses measured `(A,D,B_post)`, not this declaration;
    /// keeping it attached to the cursor makes the transaction trace describe the actual PMS
    /// variant and preserves the historical corpus comparator.
    ///
    /// Available at VALIDATION and nowhere else: a rung's `BANDWIDTH` cannot be read without first
    /// creating a PMS encoder session for it, so selection over the ladder still has no per-rung
    /// rate. That asymmetry is the specification's, not this field's.
    pub declared_bps: u64,
    pub auth: crate::hls::InheritedAuth,
    pub media: crate::hls::Resource,
    pub tracker: crate::hls::MediaTracker,
    pub pending: std::collections::VecDeque<crate::hls::Segment>,
    pub ended: bool,
    pub target_duration_secs: u64,
    pub start_applied: bool,
}

/// One worker-local physical owner for the active object and any candidate
/// trial in this round. Active and trial commands address separate generations;
/// the URL, cursor, staged AUs and reserve clock remain private here.
struct HlsRoundPort<'a> {
    round: Option<RoundId>,
    trial: Option<TrialId>,
    origin: &'a Origin,
    cursor: &'a mut HlsCursor,
    timeline: &'a mut crate::hls::SegmentTimeline,
    prefetched: &'a mut Option<ActiveMedia>,
    session: &'a mut dyn DemuxSession,
    aq: *mut AuQueue,
    aqa: *mut AuQueue,
    publication_gate: AqPublicationPort,
    net: &'a mut HlsNet,
    acodec: &'a str,
    current: Option<ActiveMedia>,
    reserve: Option<ReserveDeadlineState>,
    source: Option<(SourceKey, String)>,
    candidate: Option<HlsCursor>,
    segment: Option<crate::hls::Segment>,
    candidate_timeline: Option<crate::hls::SegmentTimeline>,
    clock: Option<crate::hls::SegmentClock>,
    output: Option<HlsSegmentOutput>,
    staged: Vec<(HlsSegmentOutput, crate::hls::SegmentClock, std::time::Duration)>,
}

pub fn hls_feed_failure(error: HlsExit) -> FeedFailure {
    match error {
        HlsExit::Aborted => FeedFailure::Aborted,
        HlsExit::PrimeExpired | HlsExit::StallAbort(_) => FeedFailure::Deadline,
        HlsExit::NotReady => FeedFailure::NotReady,
        HlsExit::Failed(_) => FeedFailure::Parse,
    }
}

impl FeedPort for HlsRoundPort<'_> {
    type Locator = String;

    fn bind_source(&mut self, source: SourceKey, locator: Self::Locator) {
        self.source = Some((source, locator));
    }

    fn step(&mut self, action: FeedAction) -> FeedObservation {
        let trial = match action {
            FeedAction::CheckAbort { trial } | FeedAction::CheckExpired { trial }
            | FeedAction::BudgetSnapshot { trial }
            | FeedAction::PrimeDeadline { trial } | FeedAction::DeadlineFinal { trial, .. }
            | FeedAction::Open { trial, .. } | FeedAction::ReadNext { trial }
            | FeedAction::AcquireFirst { trial, .. }
            | FeedAction::AcquireFollowup { trial }
            | FeedAction::StageAcquired { trial }
            | FeedAction::CheckFollowupReserve { trial }
            | FeedAction::QueueStaged { trial } => trial,
        };
        if Some(trial) != self.trial {
            return FeedObservation::Failed { trial, failure: FeedFailure::Protocol };
        }
        match action {
            FeedAction::CheckAbort { trial } => FeedObservation::AbortStatus {
                trial, aborted: unsafe { crate::aq::aq_is_aborted(self.aq) },
            },
            FeedAction::CheckExpired { trial } => FeedObservation::ExpiryStatus {
                trial, expired: exploration_expired(&mut self.reserve),
            },
            FeedAction::BudgetSnapshot { trial } => {
                let Some((buffer, started_ns)) = hls_buffer_snapshot_with_playhead(
                    self.session.observer().as_ref(), None,
                ) else {
                    return FeedObservation::Failed { trial, failure: FeedFailure::Aborted };
                };
                FeedObservation::BudgetSnapshot {
                    trial, buffered_ms: buffer, started_ns,
                    remaining: self.reserve.as_ref().and_then(ReserveDeadlineState::remaining),
                    rebuffering: self.session.clock().rebuffering(),
                }
            }
            FeedAction::PrimeDeadline { trial } => FeedObservation::PrimeDeadline {
                trial, deadline: exploration_snapshot(&mut self.reserve),
            },
            FeedAction::DeadlineFinal { trial, snapshot } => FeedObservation::DeadlineFinal {
                trial, final_expiry: exploration_timeout_is_final(&mut self.reserve, snapshot),
            },
            FeedAction::Open { trial, source } => self.open(trial, source),
            FeedAction::ReadNext { trial } => self.read_next(trial),
            FeedAction::AcquireFirst {
                trial, media_budget, yields_to_rebuffer, started_ns, clock,
            } => self.acquire_first(
                trial, media_budget, yields_to_rebuffer, started_ns, clock,
            ),
            FeedAction::AcquireFollowup { trial } => self.acquire_followup(trial),
            FeedAction::StageAcquired { trial } => self.stage_acquired(trial),
            FeedAction::CheckFollowupReserve { trial } => self.check_followup_reserve(trial),
            FeedAction::QueueStaged { trial } => self.queue_staged(trial),
        }
    }
}

impl HlsRoundPort<'_> {
    fn open(&mut self, trial: TrialId, source: SourceKey) -> FeedObservation {
        let Some((bound, locator)) = self.source.take() else {
            return FeedObservation::Failed { trial, failure: FeedFailure::Protocol };
        };
        if bound != source {
            return FeedObservation::Failed { trial, failure: FeedFailure::Protocol };
        }
        let (parsed_origin, parsed_path) = plx_net::net::origin::split(&locator);
        if parsed_origin != *self.origin {
            return FeedObservation::Failed { trial, failure: FeedFailure::SourceMoved };
        }
        let parsed_path = if parsed_path.is_empty() { "/" } else { parsed_path };
        match hls_cursor_open(
            &parsed_origin, parsed_path, self.aq, self.net, false, self.reserve.as_mut(),
        ) {
            Ok(candidate) => {
                let declared_bps = candidate.declared_bps;
                self.candidate = Some(candidate);
                FeedObservation::Opened { trial, declared_bps }
            }
            Err(error) => {
                FeedObservation::Failed { trial, failure: hls_feed_failure(error) }
            }
        }
    }

    fn read_next(&mut self, trial: TrialId) -> FeedObservation {
        let Some(candidate) = self.candidate.as_mut() else {
            return FeedObservation::Failed { trial, failure: FeedFailure::Protocol };
        };
        match hls_cursor_next(candidate, self.aq, self.net, self.reserve.as_mut()) {
            Ok(Some(segment)) => {
                let duration = segment.duration;
                self.segment = Some(segment);
                FeedObservation::SegmentReady { trial, duration }
            }
            Ok(None) => FeedObservation::Failed { trial, failure: FeedFailure::Ended },
            Err(error) => {
                FeedObservation::Failed { trial, failure: hls_feed_failure(error) }
            }
        }
    }

    fn acquire_first(
        &mut self, trial: TrialId, media_budget: Option<std::time::Duration>,
        yields_to_rebuffer: bool, started_ns: i64, clock: TrialClock,
    ) -> FeedObservation {
        let deadline = candidate_reserve_deadline(
            self.reserve.clone(), media_budget, yields_to_rebuffer, started_ns, clock,
            self.session.observer(),
        );
        self.acquire(trial, deadline)
    }

    fn acquire_followup(&mut self, trial: TrialId) -> FeedObservation {
        let Some(deadline) = self.reserve.clone() else {
            return FeedObservation::Failed { trial, failure: FeedFailure::Protocol };
        };
        self.acquire(trial, deadline)
    }

    fn check_followup_reserve(&mut self, trial: TrialId) -> FeedObservation {
        let funded = self.reserve.is_some() && !exploration_expired(&mut self.reserve);
        let remaining_ms = self.reserve.as_ref()
            .and_then(ReserveDeadlineState::remaining)
            .map(|left| u64::try_from(left.as_millis()).unwrap_or(u64::MAX));
        FeedObservation::FollowupReserve { trial, funded, remaining_ms }
    }

    fn stage_acquired(&mut self, trial: TrialId) -> FeedObservation {
        let (Some(output), Some(clock), Some(segment)) = (
            self.output.take(), self.clock.take(), self.segment.take(),
        ) else {
            return FeedObservation::Failed { trial, failure: FeedFailure::Protocol };
        };
        let Some(buffer) = hls_buffer_fact(self.session.observer().as_ref(), Some(&output)) else {
            return FeedObservation::Failed { trial, failure: FeedFailure::Aborted };
        };
        self.staged.push((output, clock, segment.duration));
        FeedObservation::Staged { trial, buffer, count: self.staged.len() }
    }

    fn acquire(&mut self, trial: TrialId, deadline: ReserveDeadlineState) -> FeedObservation {
        let (Some(candidate), Some(segment), Some(mut timeline)) = (
            self.candidate.as_ref(), self.segment.as_ref(), self.candidate_timeline.take(),
        ) else {
            return FeedObservation::Failed { trial, failure: FeedFailure::Protocol };
        };
        let mut clock = match timeline.begin(segment.duration) {
            Ok(clock) => clock,
            Err(_) => return FeedObservation::Failed {
                trial, failure: FeedFailure::TimelineOverflow,
            },
        };
        let output = match unsafe { hls_demux_segment(
            segment, &candidate.auth, &mut clock, self.aq, self.net, self.acodec,
            SegmentAcquisition::candidate(deadline),
        ) } {
            Ok(output) => output,
            Err(error) => {
                return FeedObservation::Failed { trial, failure: hls_feed_failure(error) };
            }
        };
        timeline.commit(clock);
        let acquisition = hls_acquisition(&output, segment.duration, candidate.declared_bps);
        self.candidate_timeline = Some(timeline);
        self.clock = Some(clock);
        self.output = Some(output);
        FeedObservation::Acquired { trial, acquisition }
    }

    fn queue_staged(&mut self, trial: TrialId) -> FeedObservation {
        if self.staged.is_empty() {
            return FeedObservation::Failed { trial, failure: FeedFailure::Protocol };
        }
        for (output, _, _) in &self.staged {
            if let Err(error) = hls_feed_segment(output, self.aq, self.aqa, self.session) {
                return FeedObservation::Failed { trial, failure: hls_feed_failure(error) };
            }
        }
        FeedObservation::Queued { trial }
    }
}

impl CandidateMediaPort for HlsRoundPort<'_> {
    fn candidate_declared_bps(&self) -> Option<u64> {
        self.candidate.as_ref().map(|cursor| cursor.declared_bps)
    }

    fn stage_queued_timeline(&mut self) -> bool {
        let Some(staged) = self.candidate_timeline.take() else { return false; };
        *self.timeline = staged;
        true
    }

    fn promote_candidate(&mut self, committed: &CommitReadout) -> bool {
        let Some(mut candidate) = self.candidate.take() else { return false; };
        candidate.publishes_duration = true;
        *self.prefetched = None;
        *self.cursor = candidate;
        // The promoted rung is the active one: its request and its declaration are the demux facts.
        self.session.media().publish(DemuxEffect::AbrCommitted {
            kbps: i64::from(committed.target_kbps),
            declared_kbps: i64::try_from(committed.declared_bps / 1_000).unwrap_or(i64::MAX),
        });
        true
    }

    fn publication_gate(&mut self) -> &mut dyn plx_playback_contract::adaptive::PublicationGate {
        &mut self.publication_gate
    }
}

impl AdaptivePhysicalPort for HlsRoundPort<'_> {
    fn begin_round(&mut self, round: RoundId) { self.round = Some(round); }

    fn finish_active(&mut self) -> Option<VariantFact> {
        let (_, output, _, _) = self.current.take()?;
        Some(VariantFact {
            width: output.video_width,
            height: output.video_height,
            declared_bps: self.cursor.declared_bps,
        })
    }

    fn begin_trial(&mut self, trial: TrialId, exploration: Option<ExplorationFact>) {
        self.trial = Some(trial);
        self.candidate_timeline = Some(*self.timeline);
        let observer = self.session.observer();
        self.reserve = exploration.map(|clock| {
            plx_base::eventlog::log(&format!(
                "abr: exploration target={}kbps buf={}ms runway={}ms budget={}ms",
                clock.target_kbps, clock.buffered_ms, clock.runway_ms, clock.budget_ms,
            ));
            ReserveDeadlineState::from_playhead_at(
                clock.started_ns,
                std::time::Duration::from_millis(
                    u64::try_from(clock.budget_ms).unwrap_or(u64::MAX),
                ),
                false,
                observer,
            )
        });
    }

    fn candidate_offset_micros(&self) -> Option<i64> {
        self.session.clock().display_base_ns().max(0)
            .checked_add(self.timeline.end_ns()).map(|ns| ns / 1_000)
    }

    fn buffer_sampler(&self) -> plx_playback_contract::demux::AddressedSampler<Option<i64>> {
        self.session.clock().hls_buffer_sampler()
    }

    fn remaining_playback_sampler(&self) -> plx_playback_contract::demux::AddressedSampler<i64> {
        self.session.clock().remaining_playback_sampler()
    }

    fn user_sequence_sampler(&self) -> plx_playback_contract::demux::AddressedSampler<u64> {
        self.session.clock().user_sequence_sampler()
    }
}

pub fn hls_cursor_open(
    origin: &Origin,
    path: &str,
    aq: *mut AuQueue,
    net: &mut HlsNet,
    publishes_duration: bool,
    reserve: Option<&mut ReserveDeadlineState>,
) -> Result<HlsCursor, HlsExit> {
    let master_resource = crate::hls::Resource::new(origin.clone(), path)
        .map_err(|_| HlsExit::Failed("invalid HLS master URL"))?;
    let auth = crate::hls::InheritedAuth::capture(&master_resource)
        .map_err(|_| HlsExit::Failed("HLS master has no unique credential"))?;
    // Parsed against where the fetch LANDED: a redirected master's children are relative to it.
    let (master_text, master_ms, master_landed) =
        hls_fetch_text(&master_resource, &auth, aq, net, reserve)?;
    let master = crate::hls::parse_master(&master_landed, &master_text).map_err(|e| {
        plx_base::eventlog::log(&format!("hls: master rejected: {e}"));
        HlsExit::Failed("HLS master rejected")
    })?;
    plx_base::eventlog::log(&format!(
        "hls: master one-variant bandwidth={} fetch_ms={master_ms}",
        master.variant.bandwidth
    ));
    Ok(HlsCursor {
        publishes_duration,
        declared_bps: master.variant.bandwidth,
        auth,
        media: master.variant.resource,
        tracker: crate::hls::MediaTracker::default(),
        pending: std::collections::VecDeque::new(),
        ended: false,
        target_duration_secs: 1,
        start_applied: false,
    })
}

fn hls_cursor_next(
    cursor: &mut HlsCursor,
    aq: *mut AuQueue,
    net: &mut HlsNet,
    mut reserve: Option<&mut ReserveDeadlineState>,
) -> Result<Option<crate::hls::Segment>, HlsExit> {
    loop {
        if let Some(segment) = cursor.pending.pop_front() {
            return Ok(Some(segment));
        }
        if cursor.ended {
            return Ok(None);
        }
        // Refreshed from the variant's own URL; its segments resolve against where it landed.
        let (media_text, media_ms, media_landed) =
            hls_fetch_text(&cursor.media, &cursor.auth, aq, net, reserve.as_deref_mut())?;
        let media = crate::hls::parse_media(&media_landed, &media_text).map_err(|e| {
            plx_base::eventlog::log(&format!("hls: media rejected: {e}"));
            HlsExit::Failed("HLS media playlist rejected")
        })?;
        cursor.target_duration_secs = media.target_duration_secs;
        let start_index = media
            .preferred_start_index()
            .map_err(|_| HlsExit::Failed("HLS start offset is outside the supported timeline"))?;
        let total_ns = i64::try_from(
            media
                .total_duration()
                .map_err(|_| HlsExit::Failed("HLS playlist duration overflow"))?
                .as_nanos(),
        )
        .map_err(|_| HlsExit::Failed("HLS playlist duration overflow"))?;
        if cursor.publishes_duration {
            net.observer.publish(DemuxCallbackEffect::DurationNs(total_ns));
        }
        let mut refresh = cursor.tracker.apply(&media).map_err(|e| {
            plx_base::eventlog::log(&format!("hls: refresh rejected: {e}"));
            HlsExit::Failed("HLS media refresh rejected")
        })?;
        let mut skipped = 0usize;
        if !cursor.start_applied {
            let first_sequence = media
                .segments
                .get(start_index)
                .map(|segment| segment.sequence)
                .unwrap_or_else(|| {
                    media
                        .media_sequence
                        .saturating_add(media.segments.len() as u64)
                });
            let before = refresh.new_segments.len();
            refresh
                .new_segments
                .retain(|segment| segment.sequence >= first_sequence);
            skipped = before.saturating_sub(refresh.new_segments.len());
            cursor.start_applied = true;
        }
        plx_base::eventlog::log(&format!(
            "hls: refresh new={} skipped={} total_ms={} end={} fetch_ms={media_ms}",
            refresh.new_segments.len(),
            skipped,
            total_ns / 1_000_000,
            refresh.end_list
        ));
        cursor.pending.extend(refresh.new_segments);
        cursor.ended = refresh.end_list;
        if cursor.pending.is_empty() && !cursor.ended {
            let poll = std::time::Duration::from_millis(
                cursor
                    .target_duration_secs
                    .saturating_mul(500)
                    .clamp(250, 1_000),
            );
            let reserve_snapshot = reserve
                .as_deref_mut()
                .and_then(|reserve| reserve.active(false));
            match hls_wait(
                aq,
                poll,
                reserve_snapshot,
                &mut plx_base::checkpoint::NoCheckpoint,
            ) {
                Err(HlsExit::PrimeExpired) => {
                    if reserve.as_deref_mut().map_or(true, |reserve| {
                        reserve.note_transport_deadline(reserve_snapshot, false)
                    }) {
                        return Err(HlsExit::PrimeExpired);
                    }
                }
                result => result?,
            }
        }
    }
}

/// Make the rollback cursor's next media obligation explicit without consuming it.
///
/// An ABR/source experiment runs between current-stream objects. If it fails, the response that
/// must survive is therefore the NEXT parsed EXTINF, not the duration of the object which just
/// completed. Variable-duration HLS makes those different physical facts. When the next object is
/// not already queued, perform the playlist refresh the outer loop would otherwise perform on its
/// next iteration, then put the object back at the front unchanged.
pub fn hls_duration_obligation_ms(duration: std::time::Duration) -> Option<u32> {
    // BufferSnapshot lives on a whole-millisecond lattice and floors the playable reserve.  A
    // media obligation must therefore round in the opposite direction: flooring both B and an
    // EXTINF such as 1999.9 ms can manufacture almost one millisecond of exploration reserve.
    // The parser's nanoseconds remain exact; this is the conservative projection onto the
    // controller's current unit, not a re-parse of the playlist decimal.
    let nanos = duration.as_nanos();
    let millis = nanos.checked_add(999_999)?.checked_div(1_000_000)?;
    u32::try_from(millis).ok()
}

fn hls_cursor_next_duration_ms(
    cursor: &mut HlsCursor,
    aq: *mut AuQueue,
    net: &mut HlsNet,
) -> Result<Option<u32>, HlsExit> {
    if let Some(segment) = cursor.pending.front() {
        return hls_duration_obligation_ms(segment.duration)
            .map(Some)
            .ok_or(HlsExit::Failed("HLS rollback duration overflow"));
    }
    let Some(segment) = hls_cursor_next(cursor, aq, net, None)? else {
        return Ok(None);
    };
    let duration_ms = hls_duration_obligation_ms(segment.duration)
        .ok_or(HlsExit::Failed("HLS rollback duration overflow"))?;
    cursor.pending.push_front(segment);
    Ok(Some(duration_ms))
}

pub fn hls_buffer_fact_at(
    clocks: HlsBufferClocks,
    output: Option<&HlsSegmentOutput>,
) -> BufferFact {
    let mut video = clocks.video_tail_ns;
    let mut audio = clocks.audio_tail_ns;
    let mut audio_expected = audio >= 0;
    if let Some(candidate) = output {
        video = video.max(candidate.video_tail_ns);
        if let Some(tail) = candidate.audio_tail_ns {
            audio = audio.max(tail);
        }
        audio_expected |= candidate.transfer.audio_expected;
    }
    // Starfish consumes a zero-based feed after a resume/seek while `playpos_ns` is already on the
    // movie timeline (`disp_base + fed PTS`). Translate both demux tails by the same display base
    // before the controller compares them; raw FFmpeg PTS still never cross this boundary.
    let display_base = clocks.display_base_ns.max(0);
    BufferFact {
        playback_ms: clocks.playhead_ns.max(0) / 1_000_000,
        video_tail_ms: video.max(0).saturating_add(display_base) / 1_000_000,
        audio_tail_ms: (audio >= 0).then_some(
            audio.max(0).saturating_add(display_base) / 1_000_000,
        ),
        audio_expected,
    }
}

fn hls_buffer_snapshot_with_playhead(
    observer: &dyn DemuxObserver,
    output: Option<&HlsSegmentOutput>,
) -> Option<(Option<i64>, i64)> {
    let clocks = observer.hls_buffer_clocks()?;
    Some((hls_buffer_fact_at(clocks, output).buffered_ms(), clocks.playhead_ns.max(0)))
}

pub fn hls_buffer_fact(
    observer: &dyn DemuxObserver,
    output: Option<&HlsSegmentOutput>,
) -> Option<BufferFact> {
    Some(hls_buffer_fact_at(observer.hls_buffer_clocks()?, output))
}

/// Progressive Original uses absolute movie PTS, unlike offset-zero HLS, so no display-base
/// translation belongs here. Both lanes must have produced post-open/post-seek timestamps before
/// an A/V stream can claim a buffer duration; otherwise the absent lane is exactly the starvation
/// the metric is meant to notice, but not yet enough evidence to start an encoder.
fn progressive_buffered_ms(clock: &dyn DemuxClock, audio_expected: bool) -> Option<i64> {
    let video = clock.video_tail_ns();
    if video < 0 {
        return None;
    }
    let audio = clock.audio_tail_ns();
    let tail = if audio_expected {
        if audio < 0 {
            return None;
        }
        video.min(audio)
    } else {
        video
    };
    Some(
        tail.saturating_sub(clock.playpos_ns().max(0))
            .max(0)
            / 1_000_000,
    )
}

pub fn hls_acquisition(
    output: &HlsSegmentOutput, duration: std::time::Duration, declared_bps: u64,
) -> Acquisition {
    Acquisition {
        bytes: output.transfer.bytes,
        active_us: output.transfer.active_us,
        total_us: output.transfer.total_us,
        duration,
        width: output.video_width,
        height: output.video_height,
        declared_bps,
    }
}

/// Content still to play, from the one published duration and position. Feeds the utility model's
/// benefit scaling: a visible mode switch has to earn back its cost out of what is LEFT, which is
/// why "twenty seconds remaining" needs no special case anywhere below.
impl CandidateFencePort for HlsRoundPort<'_> {
    type Fence = plx_playback_contract::adaptive::FenceHandle;

    fn arm(&mut self, automatic_token: u64)
        -> Result<Self::Fence, plx_playback_contract::adaptive::CandidateFenceError> {
        self.session.adaptive().arm_candidate(automatic_token)
    }

    fn settle(&mut self, fence: Self::Fence) { fence.settle(); }
}

/// The physical feed's AQ-abort serialization port. Adaptive policy never names a queue.
struct AqPublicationPort(*mut AuQueue);

impl plx_playback_contract::adaptive::PublicationGate for AqPublicationPort {
    fn if_not_aborted(&mut self, transition: &mut dyn FnMut()) -> bool {
        crate::aq::aq_if_not_aborted(self.0, transition).is_some()
    }
}

impl AutomaticPort for HlsRoundPort<'_> {
    type Lease = plx_playback_contract::adaptive::LeaseHandle;

    fn begin(
        &mut self, kind: AutomaticTransition, expected_user_sequence: u64,
    ) -> Option<Self::Lease> {
        self.session.adaptive().begin(kind, expected_user_sequence)
    }

    fn authorize(&mut self, lease: &Self::Lease) -> bool {
        self.session.adaptive().authorize(lease.token())
    }

    fn aborted(&self) -> bool { unsafe { crate::aq::aq_is_aborted(self.aq) } }

    fn gate(&mut self) -> &mut dyn plx_playback_contract::adaptive::PublicationGate {
        &mut self.publication_gate
    }
}

impl TrialActuatorPort for HlsRoundPort<'_> {
    type TrialReserve = plx_playback_contract::adaptive::LeaseHandle;

    fn arm_trial_reserve(
        &mut self, runway_ms: i64, expected_user_sequence: u64,
    ) -> Option<Self::TrialReserve> {
        self.session.adaptive().arm_trial(runway_ms, expected_user_sequence)
    }

    fn lease_token(&self, lease: &Self::Lease) -> u64 { lease.token() }

    fn trial_token(&self, reserve: &Self::TrialReserve) -> u64 { reserve.token() }
}

impl AdaptiveRoundPort for HlsRoundPort<'_> {
    fn position_sampler(&self) -> plx_playback_contract::demux::AddressedSampler<i64> {
        self.session.clock().playpos_sampler()
    }

    fn transition_permitted(&self, kind: AutomaticTransition) -> bool {
        self.session.adaptive_ref().transition_permitted(kind)
    }

    fn rebuffer_state(&self) -> (bool, bool) {
        (
            self.session.clock().rebuffer_requested(),
            self.session.clock().rebuffering(),
        )
    }

    fn buffer_snapshot(&self) -> Option<(Option<i64>, i64)> {
        hls_buffer_snapshot_with_playhead(self.session.observer().as_ref(), None)
    }

    fn pause_fact(&mut self) -> PauseFact {
        let commit_pause = self.session.clock_mut().pause_event(PauseLane::Hls);
        PauseFact {
            paused: commit_pause.paused,
            completed_ms: commit_pause.completed_ms,
        }
    }
}

/// **One shared reducer for a `StallAbort` returned against the ACTIVE cursor.** The ordinary
/// fetch and the same-encoder lookahead observe the identical terminal boundary and must handle
/// it identically: log it, requeue the segment (exactly once — it is put back here, and nowhere
/// else, whichever path called this), and hand back a zero-AU output carrying the transfer so
/// the caller's ordinary controller/transact path can treat it as an abandoned sample. Never call
/// this against a `Candidate` acquisition's result: a candidate never carries a `StallGuard`, so
/// it cannot produce `StallAbort`.
pub fn hls_stall_abort_outcome(
    segment: crate::hls::Segment,
    clock: crate::hls::SegmentClock,
    transfer: SegmentTransfer,
    reserve_ms_at_start: i64,
    cursor: &mut HlsCursor,
) -> (
    crate::hls::Segment,
    HlsSegmentOutput,
    crate::hls::SegmentClock,
    bool,
) {
    plx_base::eventlog::log(&format!(
        "abr: stall abort seq={} bytes={} active={}ms reserve={}ms censored=1 \
         cause=terminal_reserve — abandoning the incomplete fetch so a cheaper rung \
         can be decided",
        segment.sequence,
        transfer.bytes,
        transfer.active_us / 1_000,
        reserve_ms_at_start,
    ));
    cursor.pending.push_front(segment.clone());
    (
        segment,
        HlsSegmentOutput {
            aus: Vec::new(),
            transfer,
            video_width: 0,
            video_height: 0,
            video_tail_ns: -1,
            audio_tail_ns: None,
        },
        clock,
        true,
    )
}

/// The same-encoder lookahead's control flow, generic over the demux call itself so a test can
/// inject a fake one and prove this flow's WIRING — not a hand-built `AvioState` a regression
/// could bypass entirely. Production's only caller is [`hls_prefetch_same_encoder`] below, which
/// closes over the real [`hls_demux_segment`]; a test closes over a fake that records what
/// [`SegmentAcquisition`] it was handed and can return [`HlsExit::StallAbort`] on demand.
///
/// Takes `n1` (the already-dequeued next segment) and `cursor` separately rather than dequeuing
/// itself, so a test can drive this exact flow without also standing up an `HlsNet`/`AuQueue`
/// just to satisfy [`hls_cursor_next`].
pub fn hls_prefetch_same_encoder_with<F>(
    cursor: &mut HlsCursor,
    n1: crate::hls::Segment,
    timeline: crate::hls::SegmentTimeline,
    n_clock: crate::hls::SegmentClock,
    // The adaptive owner's current floor decision, sampled at this acquisition boundary.
    at_floor: Option<bool>,
    observer: Arc<dyn DemuxObserver>,
    demux: F,
) -> Result<
    Option<(
        crate::hls::Segment,
        HlsSegmentOutput,
        crate::hls::SegmentClock,
        bool,
    )>,
    HlsExit,
>
where
    F: FnOnce(
        &crate::hls::Segment,
        &mut crate::hls::SegmentClock,
        SegmentAcquisition,
    ) -> Result<HlsSegmentOutput, HlsExit>,
{
    let mut peek = timeline;
    peek.commit(n_clock);
    let mut clock = match peek.begin(n1.duration) {
        Ok(clock) => clock,
        Err(_) => {
            cursor.pending.push_front(n1);
            return Ok(None);
        }
    };
    // **The lookahead is still an ACTIVE-cursor fetch and must arm the same guard the ordinary
    // branch would.** `SegmentAcquisition::for_cursor` is the one constructor that evaluates
    // `arm_active_stall_guard` from the live buffer/floor/hold state; there is no path through it
    // that skips the evaluation. (This is precisely what was missing before: the lookahead used
    // to build `ReserveDeadlineState::new(None, false), None` here unconditionally, which is how
    // `pipe_abr_down_collapse` never aborted a fetch that had already exhausted the reserve.)
    let acquisition = SegmentAcquisition::for_cursor(at_floor, observer)
        .ok_or(HlsExit::Aborted)?;
    let stall_reserve_ms = acquisition
        .stall()
        .map(|guard| guard.reserve_ms_at_start)
        .unwrap_or(-1);
    match demux(&n1, &mut clock, acquisition) {
        Ok(output) => Ok(Some((n1, output, clock, false))),
        // Do NOT let this escape via `?` (that would exit `hls_demux` entirely) and do NOT
        // silently requeue-and-`Ok(None)` (that is exactly how the old code swallowed the abort).
        // Route it through the same reducer the ordinary branch uses, so the caller's main loop
        // sees an abandoned round — not a completed prefetch, not nothing.
        Err(HlsExit::StallAbort(transfer)) => Ok(Some(hls_stall_abort_outcome(
            n1,
            clock,
            transfer,
            stall_reserve_ms,
            cursor,
        ))),
        Err(error) if hls_prefetch_is_fatal(&error) => Err(error),
        Err(_) => {
            cursor.pending.push_front(n1);
            Ok(None)
        }
    }
}

/// Thin production wrapper over [`hls_prefetch_same_encoder_with`]: dequeues the next segment and
/// closes over the real [`hls_demux_segment`]. All of the actual prefetch decision-making lives
/// in the function above, which is what the regression test drives.
unsafe fn hls_prefetch_same_encoder(
    cursor: &mut HlsCursor,
    aq: *mut AuQueue,
    net: &mut HlsNet,
    acodec: &str,
    timeline: crate::hls::SegmentTimeline,
    n_clock: crate::hls::SegmentClock,
    at_floor: Option<bool>,
) -> Result<
    Option<(
        crate::hls::Segment,
        HlsSegmentOutput,
        crate::hls::SegmentClock,
        bool,
    )>,
    HlsExit,
> {
    let n1 = match hls_cursor_next(cursor, aq, net, None) {
        Ok(Some(segment)) => segment,
        Ok(None) => return Ok(None),
        Err(error) if hls_prefetch_is_fatal(&error) => return Err(error),
        Err(_) => return Ok(None),
    };
    let auth = cursor.auth.clone();
    hls_prefetch_same_encoder_with(cursor, n1, timeline, n_clock, at_floor, net.observer.clone(), |segment, clock, acquisition| {
        // SAFETY: this closure is only ever invoked (synchronously, within this call) by
        // `hls_prefetch_same_encoder_with` above, under the same `unsafe` obligations the caller
        // of this whole function already carries (a live `aq`/`net` for the duration of the call).
        unsafe { hls_demux_segment(segment, &auth, clock, aq, net, acodec, acquisition) }
    })
}

type ActiveMedia = (
    crate::hls::Segment,
    HlsSegmentOutput,
    crate::hls::SegmentClock,
    bool,
);

/// The physical read shared by adaptive and fixed HLS. It leaves an aborted
/// object on the cursor and returns its censored transfer without feeding it.
fn hls_read_active(
    cursor: &mut HlsCursor, net: &mut HlsNet, timeline: crate::hls::SegmentTimeline,
    prefetched: &mut Option<ActiveMedia>, aq: *mut AuQueue, acodec: &str,
    at_floor: Option<bool>,
) -> Result<Option<ActiveMedia>, HlsExit> {
    if let Some(round) = prefetched.take() { return Ok(Some(round)); }
    let Some(segment) = hls_cursor_next(cursor, aq, net, None)? else { return Ok(None); };
    let mut clock = timeline.begin(segment.duration)
        .map_err(|_| HlsExit::Failed("HLS content timeline overflow"))?;
    let acquisition = SegmentAcquisition::for_cursor(at_floor, net.observer.clone())
        .ok_or(HlsExit::Aborted)?;
    let stall_reserve_ms = acquisition.stall()
        .map(|guard| guard.reserve_ms_at_start).unwrap_or(-1);
    match unsafe { hls_demux_segment(
        &segment, &cursor.auth, &mut clock, aq, net, acodec, acquisition,
    ) } {
        Ok(output) => Ok(Some((segment, output, clock, false))),
        Err(HlsExit::StallAbort(transfer)) => Ok(Some(hls_stall_abort_outcome(
            segment, clock, transfer, stall_reserve_ms, cursor,
        ))),
        Err(other) => Err(other),
    }
}

impl HlsRoundPort<'_> {
    fn step_active_native(&mut self, action: ActiveAction) -> Result<ActiveObservation, HlsExit> {
        if Some(action.round()) != self.round {
            return Err(HlsExit::Failed("adaptive active round address mismatch"));
        }
        let round = action.round();
        match action {
            ActiveAction::Read { at_floor, .. } => {
                if self.current.is_some() {
                    return Err(HlsExit::Failed("adaptive active round read repeated"));
                }
                let Some(media) = hls_read_active(
                    self.cursor, self.net, *self.timeline, self.prefetched,
                    self.aq, self.acodec, at_floor,
                )? else { return Ok(ActiveObservation::Ended { round }); };
                let acquisition = hls_acquisition(&media.1, media.0.duration, self.cursor.declared_bps);
                let delivered = !media.1.aus.is_empty();
                let abandoned = media.3;
                self.current = Some(media);
                Ok(ActiveObservation::Ready { round, acquisition, delivered, abandoned })
            }
            ActiveAction::Feed { .. } => {
                let Some((segment, output, clock, _)) = self.current.as_ref() else {
                    return Err(HlsExit::Failed("adaptive active feed without read"));
                };
                if !output.aus.is_empty() {
                    hls_feed_segment(output, self.aq, self.aqa, self.session)?;
                    self.session.media().publish(DemuxEffect::RecoverySegment {
                        total_us: output.transfer.total_us, duration: segment.duration,
                    });
                    self.timeline.commit(*clock);
                }
                Ok(ActiveObservation::Fed { round })
            }
            ActiveAction::Buffer { .. } => {
                let Some((_, output, _, _)) = self.current.as_ref() else {
                    return Err(HlsExit::Failed("adaptive active buffer without read"));
                };
                let fact = hls_buffer_fact(self.session.observer().as_ref(), Some(output))
                    .ok_or(HlsExit::Aborted)?;
                Ok(ActiveObservation::Buffer { round, fact })
            }
            ActiveAction::Pause { .. } => {
                let snapshot = self.session.clock_mut().pause_event(PauseLane::Hls);
                Ok(ActiveObservation::Pause {
                    round, paused: snapshot.paused, sequence: snapshot.sequence,
                    completed_ms: snapshot.completed_ms,
                })
            }
            ActiveAction::RollbackDuration { .. } => {
                let duration_ms = hls_cursor_next_duration_ms(self.cursor, self.aq, self.net)?;
                Ok(ActiveObservation::RollbackDuration { round, duration_ms })
            }
            ActiveAction::Prefetch { at_floor, .. } => {
                let Some((_, _, clock, _)) = self.current.as_ref() else {
                    return Err(HlsExit::Failed("adaptive active prefetch without read"));
                };
                *self.prefetched = unsafe { hls_prefetch_same_encoder(
                    self.cursor, self.aq, self.net, self.acodec, *self.timeline, *clock, at_floor,
                )? };
                Ok(ActiveObservation::Prefetched { round })
            }
        }
    }
}

impl ActiveFeedPort for HlsRoundPort<'_> {
    type Error = plx_playback_contract::adaptive::PhysicalFailure;

    fn step_active(&mut self, action: ActiveAction) -> Result<ActiveObservation, Self::Error> {
        self.step_active_native(action).map_err(Into::into)
    }

    fn note_prime_runway(&mut self, ms: i64) {
        self.session.media().publish(DemuxEffect::PrimeRunway(ms));
    }
}

fn hls_demux(
    off: &plx_base::task::OffFrame,
    origin: &Origin,
    path: &str,
    acodec: &str,
    abr: Option<Box<dyn plx_playback_contract::adaptive::AdaptiveSession>>,
    aq: *mut AuQueue,
    aqa: *mut AuQueue,
    hs: *mut HttpStream,
    transport: DemuxTransport,
    session: &mut dyn DemuxSession,
) -> Result<(), HlsExit> {
    // The change detector is process-global, so a second playback of the same stream would
    // otherwise log nothing at all. Cleared here rather than on teardown: this is the one place
    // that runs exactly once per HLS demux, whatever ended the previous one.
    HLS_LAST_VIDEO.store(u64::MAX, Ordering::Relaxed);
    if let Some(start) = abr.as_ref() {
        session.media().publish(DemuxEffect::AbrHlsStart {
            kbps: i64::from(start.initial_kbps()),
        });
    }
    let mut net = HlsNet { hs, curl: None, range: transport.range, credential_gate: transport.credential_gate, observer: session.observer() };
    let mut cursor = hls_cursor_open(origin, path, aq, &mut net, true, None)?;
    if abr.is_some() {
        session.media().publish(DemuxEffect::AbrHlsDeclared {
            kbps: i64::try_from(cursor.declared_bps / 1_000).unwrap_or(i64::MAX),
        });
    }
    let mut timeline = crate::hls::SegmentTimeline::default();
    let mut adaptive = abr;
    // Consume native-accepted pause EVENTS, not a sampled bool. The worker can spend a complete
    // Pause→Resume cycle blocked in aq_push/network I/O and must still age the first post-resume
    // estimate by the exact cumulative interval.
    session.clock_mut().begin_pause_lane(PauseLane::Hls);
    let mut prefetched: Option<ActiveMedia> = None;

    loop {
        let Some(adaptive_session) = adaptive.as_mut() else {
            let Some((segment, output, clock, fetch_abandoned)) = hls_read_active(
                &mut cursor, &mut net, timeline, &mut prefetched, aq, acodec, None,
            )? else { break; };
            let delivered = !output.aus.is_empty();
            if delivered {
                hls_feed_segment(&output, aq, aqa, session)?;
                session.media().publish(DemuxEffect::RecoverySegment {
                    total_us: output.transfer.total_us, duration: segment.duration,
                });
                timeline.commit(clock);
            }
            if hls_should_prefetch_after_abr(delivered, fetch_abandoned, true) {
                prefetched = unsafe {
                    hls_prefetch_same_encoder(
                        &mut cursor,
                        aq,
                        &mut net,
                        acodec,
                        timeline,
                        clock,
                        None,
                    )?
                };
            }
            continue;
        };
        let mut physical = HlsRoundPort {
            round: None, trial: None, origin,
            cursor: &mut cursor, timeline: &mut timeline,
            prefetched: &mut prefetched, session,
            aq, aqa, publication_gate: AqPublicationPort(aq),
            net: &mut net, acodec, current: None, reserve: None,
            source: None, candidate: None, segment: None,
            candidate_timeline: None, clock: None, output: None, staged: Vec::new(),
        };
        match adaptive_session.drive_round(off, &mut physical) {
            plx_playback_contract::adaptive::AdaptiveProgress::Continue => continue,
            plx_playback_contract::adaptive::AdaptiveProgress::Ended
            | plx_playback_contract::adaptive::AdaptiveProgress::OriginalPublished => break,
            plx_playback_contract::adaptive::AdaptiveProgress::Aborted =>
                return Err(HlsExit::Aborted),
            plx_playback_contract::adaptive::AdaptiveProgress::PhysicalFailure(error) =>
                return Err(error.into()),
            plx_playback_contract::adaptive::AdaptiveProgress::Failed(message) =>
                return Err(HlsExit::Failed(message)),
        }
    }
    Ok(())
}

/// Whether a demux exit that published no access unit is a playback failure — the ONE rule both
/// the HLS and the progressive tails read. A demuxer that produced nothing and ended on its own
/// failed; one whose lane teardown aborted was told to stop (a reload, a seek, BACK), whatever
/// early exit that took, and must neither log a failure nor raise `demux_failed`.
pub fn unproductive_exit_failed(pushed_any: bool, lane_aborted: bool) -> bool {
    !pushed_any && !lane_aborted
}

/// The log line for an `avformat_open_input` that returned no context. Teardown aborts the lanes
/// and `read_cb` answers the abort with EOF, so an open cut short that way returns whatever the
/// truncated probe made of it (`AVERROR_INVALIDDATA` on a Matroska header) — a stop that must not
/// read as the source failing to open. Issue #266's normalize run was misread exactly so.
pub fn open_input_failure_note(r: c_int, lane_aborted: bool) -> String {
    if lane_aborted {
        format!("ff: aborted during open_input r={r}")
    } else {
        format!("ff: open_input failed r={r}")
    }
}

/// **How much further the VIDEO lane may run ahead while the AUDIO lane has nothing yet.**
///
/// The two lanes are filled from ONE read of the file, in file order, and each has its own byte cap
/// (`player::engine`'s `AQ_VIDEO_BYTES`, 10 MiB). A Matroska file whose audio track carries a
/// container `Delay` has no audio block before that many seconds of video, so the video lane can
/// fill before the first audio packet is even read. Measured on the device, the video lane held
/// 10,171,164 bytes when the first audio AU was read for the reported film (an E-AC-3 `Delay` of
/// 11.008 s at ~9.5 Mbit/s, after the pump had popped some) and 13,628,369 bytes for the
/// `pipe_late_audio_eac3` fixture (12 Mbit/s, the same delay). The demuxer parked on the full video
/// lane, the pipeline (which will not start its clock without audio) took no more video, and the
/// first audio block was never read: a start that buffered forever, while a resume past the delay
/// played at once. FFmpeg's own probe said as much on stderr: no E-AC-3 parameters within its 5 MB
/// `probesize`.
///
/// So from the first video push that WOULD PARK at the base cap while no audio AU has been pushed in
/// this open or seek, the video lane's cap is its base cap PLUS this much: **10 MiB + 24 MiB =
/// 34 MiB (~35.6 MB) in total.** The grant is lazy on purpose: an ordinary file (video a few
/// packets ahead of the first audio, B-frames, a muxer that leads with video) never fills the base
/// cap before its first audio packet, so it never sees the grant and keeps the memory behaviour it
/// always had; on a 60-100 Mbit/s UHD stream an eager grant would have cost 18-30 MiB of extra
/// queue on a RAM-tight 32-bit set for files that never needed it. That is a BOUND, not a
/// measurement of what a file needs: at an 11 s delay it holds video up to ~26 Mbit/s,
/// and a longer delay or a higher rate outruns it (23.8 s at 12 Mbit/s, 11.4 s at 25 Mbit/s). It is
/// large enough for the delays seen in the field and small enough for a 32-bit set that is
/// RAM-tight, and a file whose audio never comes cannot grow the lane without limit.
///
/// **Exhausting it fails the open** ([`PreAudioHeadroom::before_video`], [`VideoStep::Fail`]): the
/// lane is full, no audio has been read, and the consumer has popped nothing since the grant, so
/// parking would be a spinner nobody can leave. The demuxer says so in the log and raises the same
/// `demux_io_failed` the pump turns into a route recovery (an Auto Original falls back to HLS, which
/// the server remuxes with the delay applied) or the failure read-out. A lane the consumer IS
/// draining (a seek into an audioless stretch: the pump pops video while the demuxer reads at LAN
/// speed, and the engine's video-only prime escape plays it) is not that hang: the grant is
/// released and the lane parks like any full lane, as it did before the grant existed.
///
/// **The grant never outlives its epoch.** It is released when the audio lead-in has been pushed
/// (below), when the consumer is seen draining a full lane (above), at a seek (the next epoch
/// starts from the base cap) and when the read loop ends.
///
/// **The grant is not withdrawn when the first audio AU lands.** The engine feeds a late track's
/// first AU AND the next `AUDIO_LEAD_IN_NS` of audio ahead of a clock that cannot start without
/// them (`player::engine`), so the demuxer must READ that much audio with the video lane still full
/// and undrained. Withdrawing the headroom at the first audio push dropped the video cap under what
/// the lane held, the next video push parked, and the audio lane got two packets and no more (device
/// log: `feed a#1`, `feed a#2`, then nothing). The grant is therefore released once the audio lane
/// has been pushed [`AUDIO_LEAD_IN_NS`](crate::feed_state::AUDIO_LEAD_IN_NS) plus
/// [`LEAD_IN_MARGIN_NS`] of audio, and the release is `aq_release_headroom`: the cap follows the
/// consumer's drain down to the base rather than dropping under the lane's occupancy.
pub const PRE_AUDIO_VIDEO_HEADROOM: c_long = 24 * 1024 * 1024;

/// **How far the AUDIO lane may run past its soft cap while the video lane is starved** (issue
/// #600): the mirror image of [`PRE_AUDIO_VIDEO_HEADROOM`], and unlike it a standing rule rather
/// than a one-shot grant.
///
/// A Matroska file whose muxer stored its audio ahead of its video (the reported film: ~48 s of
/// 768 kbit/s E-AC-3, 4.6 MB, before the first video block, whose timestamp is still 0) fills the
/// audio lane (`player::engine`'s `AQ_AUDIO_BYTES`, 1 MiB, ~10.9 s of that audio) before the first
/// video packet is read. The demuxer parked in `aq_push` on it, nothing drains audio before the
/// pipeline has video to start its clock on, and the first video block was never read: a start that
/// buffered forever (`feed a#1..4` in the log, no `ff: AU#0`).
///
/// That skew does not end at the first video block. A muxer that wrote audio 48 s ahead keeps it
/// for the whole file, so the audio lane has to hold ~the whole skew (3.7 MB at 640 kbit/s) for the
/// whole film, and the demuxer must never park on it while the consumer is short of VIDEO: before
/// Play the clock needs `PRIME_VIDEO_MAX_NS` of it, afterwards the pump refills the video lane to
/// its feed-ahead target (`MAX_FEED_AHEAD_NS`). So every audio push past the 1 MiB soft cap is
/// admitted, up to this hard ceiling, **while the video lane holds less than `MAX_FEED_AHEAD_NS`
/// of media** ([`audio_push_gate`], `aq::PushGate`), and parks (timed, see `PushGate`) otherwise.
/// Nothing is granted or latched: the soft cap is back as soon as the video lane is stocked or the
/// audio lane drains under it, so an ordinary file (which never has 1 MiB of audio queued while its
/// video is starved) never sees any of this. For a skewed file the consequence is a read-ahead
/// that is just in time: the video lane is held at about the pump's own feed-ahead target instead
/// of filling to its 10 MiB cap.
///
/// **The ceiling is base + this = 1 MiB + 16 MiB = 17 MiB.** It is a bound, not a measurement of
/// what a file needs: 16 MiB holds ~2.9 min of the reported film's 768 kbit/s E-AC-3, ~3.5 min of
/// 640 kbit/s AC-3, ~1.4 min of a 1.5 Mbit/s DTS core and ~22 s of a 6 Mbit/s E-AC-3 (the top of
/// the codec's range), while a file whose video never comes cannot grow the lane without limit on
/// a RAM-tight 32-bit set. Audio is the cheap lane per second, which is why it is smaller than the
/// video grant.
///
/// **At the ceiling the stream fails** (`demux_io_failed`, the same surface as the late-audio
/// failure: Auto Original falls back to HLS, which the server remuxes with the audio in step; the
/// failure read-out otherwise) when the consumer provably cannot have started: the video read in
/// this open or seek spans less than `PRIME_VIDEO_MAX_NS`, so neither of the engine's prime arms is
/// satisfiable ([`PreAudioHeadroom::video_reach_ns`]). That test deliberately does not use the
/// audio lane's pops: the engine pops audio before any video exists (`feed_audio_lane` feeds until
/// the pipeline answers BufferFull), so a pop proves nothing about a clock that has not started.
/// Once the video read spans the prime depth the clock may be running and the audio lane is
/// draining at real time, so a ceiling reached then (an audio-only stretch, a sparse video track) is
/// an ordinary full-lane park, not a failure.
pub const AUDIO_AHEAD_HEADROOM: c_long = 16 * 1024 * 1024;

/// Audio pushed past the engine's lead-in before the video lane's headroom is released: the lead-in
/// is measured from the AU the engine ACCEPTS, in its own timeline, and the lane is pushed in whole
/// AUs, so the demuxer reads a little beyond it rather than exactly to it.
const LEAD_IN_MARGIN_NS: i64 = 500_000_000;

/// The per-open (and per-seek) state of the video lane's pre-audio headroom. The decisions
/// ([`before_video`](Self::before_video), [`after_audio`](Self::after_audio)) are PURE; the two
/// methods that take a lane ([`withdraw`](Self::withdraw), [`restart`](Self::restart)) only call
/// `aq_release_headroom`.
///
/// `granted` latches at the first video push that WOULD PARK at the base cap while an audio lane is
/// being fed and no audio has been read yet; a file whose audio comes first, or whose video never
/// fills the base cap before its audio, never gets a grant. `pops_at_grant` is the lane's pop
/// counter then, the evidence for "the consumer is draining". `first_audio_ns` is the pts of the
/// first audio AU pushed since the open or seek. `released` latches once the grant has been given
/// back, whichever way. Every field restarts on a seek ([`restart`](Self::restart)).
#[derive(Default, Debug)]
struct PreAudioHeadroom {
    granted: bool,
    released: bool,
    first_audio_ns: Option<i64>,
    pops_at_grant: u64,
    /// The first and the latest video AU pts pushed since the open or seek: how much video the
    /// consumer has been given ([`video_reach_ns`](Self::video_reach_ns)).
    video_first_ns: Option<i64>,
    video_last_ns: i64,
}

/// The admission rule for an audio push past the audio lane's soft cap: see
/// [`AUDIO_AHEAD_HEADROOM`]. `video_reach_ns` is [`PreAudioHeadroom::video_reach_ns`].
fn audio_push_gate(video_lane: *mut AuQueue, video_reach_ns: i64) -> crate::aq::PushGate {
    crate::aq::PushGate {
        other: video_lane,
        starved_below_ns: crate::feed_state::MAX_FEED_AHEAD_NS,
        headroom: AUDIO_AHEAD_HEADROOM,
        fail_at_ceiling: video_reach_ns < crate::feed_state::PRIME_VIDEO_MAX_NS,
    }
}

/// What the video push about to be made means for the lane's headroom.
#[derive(Debug, PartialEq, Eq)]
enum VideoStep {
    /// Push as usual.
    Proceed,
    /// Raise the lane's cap by this much, then push: the base cap is full and no audio has come.
    Grant(c_long),
    /// The consumer is draining a lane that is full under the grant: give the grant back
    /// (`aq_release_headroom`) and push, parking like any full lane.
    Release,
    /// Full, no audio, and nothing popped since the grant: parking is a spinner nobody can leave.
    Fail,
}

impl PreAudioHeadroom {
    /// A video AU is about to be pushed. `audio_lane` is whether an audio lane is being fed at all,
    /// `would_park` whether the push would park at the cap now in force (`aq_would_park`), `pops`
    /// the lane's pop counter (`aq_pops`).
    fn before_video(&mut self, audio_lane: bool, would_park: bool, pops: u64) -> VideoStep {
        if !audio_lane || self.released || !would_park {
            return VideoStep::Proceed;
        }
        if !self.granted {
            if self.first_audio_ns.is_some() {
                // Audio came first: the ordinary backpressure applies.
                return VideoStep::Proceed;
            }
            self.granted = true;
            self.pops_at_grant = pops;
            return VideoStep::Grant(PRE_AUDIO_VIDEO_HEADROOM);
        }
        // The grant stands and the push would park: the cap it gave is spent.
        if pops != self.pops_at_grant {
            self.released = true;
            VideoStep::Release
        } else if self.first_audio_ns.is_none() {
            VideoStep::Fail
        } else {
            // Audio is in and the lead-in logic owns the release; nothing has been popped yet.
            VideoStep::Proceed
        }
    }

    /// A video AU at `pts_ns` was pushed since the open or seek.
    fn video_pushed(&mut self, pts_ns: i64) {
        self.video_first_ns.get_or_insert(pts_ns);
        self.video_last_ns = self.video_last_ns.max(pts_ns);
    }

    /// How much video the consumer has been given since the open or seek (0 before the first AU).
    fn video_reach_ns(&self) -> i64 {
        self.video_first_ns.map_or(0, |f| (self.video_last_ns - f).max(0))
    }

    /// An audio AU at `pts_ns` was pushed since the open or seek.
    fn after_audio(&mut self, pts_ns: i64) -> AudioStep {
        let first = self.first_audio_ns.is_none();
        let start = *self.first_audio_ns.get_or_insert(pts_ns);
        let release = self.granted
            && !self.released
            && pts_ns - start >= crate::feed_state::AUDIO_LEAD_IN_NS + LEAD_IN_MARGIN_NS;
        self.released |= release;
        AudioStep { first, release }
    }

    /// Give the lane's grant back if one stands: an exit from the granted state that no audio push
    /// or draining consumer reached (a seek, the end of the file, a stop, audio whose timestamps
    /// never span the lead-in). The cap follows the consumer down to the base.
    fn withdraw(&mut self, video_lane: *mut AuQueue) {
        if self.granted && !self.released {
            crate::aq::aq_release_headroom(video_lane);
            self.released = true;
        }
    }

    /// A seek starts a new epoch: whatever grant the last one left standing goes back, and every
    /// field starts over (the next video push that would park grants afresh).
    fn restart(&mut self, video_lane: *mut AuQueue) {
        self.withdraw(video_lane);
        *self = PreAudioHeadroom::default();
    }
}

/// What an audio push means for the video lane's headroom.
struct AudioStep {
    /// The first audio AU since the open or seek: the caller logs how far video had to run.
    first: bool,
    /// The audio lead-in is in: release the headroom now.
    release: bool,
}

#[cfg(test)]
mod audio_codec_name_tests {
    use super::*;

    /// FFmpeg names DTS `dts`; PMS (and so the payload codec reaching the matcher) says `dca`.
    #[test]
    fn the_payload_codec_matches_ffmpegs_name_across_spellings() {
        assert!(audio_name_matches("dts", "dca"));
        assert!(audio_name_matches("dts", "DTS"));
        assert!(audio_name_matches("eac3", "ec-3"));
        assert!(audio_name_matches("ac3", "ac3"));
        assert!(!audio_name_matches("truehd", "dca"));
        assert!(!audio_name_matches("ac3", "eac3"));
    }
}

/// PURE: whether the demuxer would hand the TV this stream's packets RAW although the Load declared
/// PCM. `pcm_declared`: `pcm_out()` is set (1x, an app-decoded codec was planned); `audio_fed`:
/// audio goes to the TV at all; `app_decoded`: the stream's own FFmpeg codec has an app decoder
/// (`devcaps::app_decoded_by_ffmpeg_name`, and for a lossy one the Load's lossy claim).
///
/// The planner's `pcm` is PMS's spelling and covers LPCM flavours the app cannot decode (float,
/// 8-bit); the TV reads raw bytes under a PCM Load as the declared S16/S32, which is full-scale
/// noise or the wrong speed with no error. Such a stream must be refused, never fed.
fn pcm_load_feeds_raw(pcm_declared: bool, audio_fed: bool, app_decoded: bool) -> bool {
    pcm_declared && audio_fed && !app_decoded
}

#[cfg(test)]
mod pcm_load_guard_tests {
    use super::pcm_load_feeds_raw;

    /// Every FFmpeg codec name a track PMS calls `pcm` (or any other planned-as-PCM track) can
    /// open as, under a PCM Load: only the ones with an app decoder may go on.
    #[test]
    fn a_pcm_load_never_feeds_a_stream_the_app_cannot_decode() {
        for (name, decodable) in [
            ("flac", true), ("alac", true), ("pcm_s16le", true), ("pcm_s24be", true), ("pcm_s32le", true),
            ("pcm_f32le", false), ("pcm_f64le", false), ("pcm_s8", false), ("pcm_u8", false),
            ("pcm_bluray", false), ("pcm_dvd", false), ("ac3", false), ("eac3", false), ("aac", false),
            ("dts", false), // lossy: the app's only when the Load itself claimed the lossy decode
            ("mp3", false), ("vorbis", false), ("truehd", false), // likewise
        ] {
            let app_decoded = plx_platform::devcaps::app_decoded_by_ffmpeg_name(name).is_some_and(|c| !c.lossy);
            assert_eq!(app_decoded, decodable, "{name}: the table the guard reads");
            assert_eq!(pcm_load_feeds_raw(true, true, app_decoded), !decodable, "{name}");
        }
    }

    #[test]
    fn only_a_pcm_load_that_feeds_audio_is_guarded() {
        assert!(!pcm_load_feeds_raw(false, true, false), "a native Load feeds compressed audio as it always did");
        assert!(!pcm_load_feeds_raw(true, false, false), "no audio fed (the dev video-only switch)");
        assert!(!pcm_load_feeds_raw(true, true, true), "decoded in the app");
    }
}

#[cfg(test)]
mod averror_errno_tests {
    use super::*;

    /// `AVERROR(e)` is `-e` of the host libc: `EAGAIN` is 11 on Linux and 35 on macOS, so the drain
    /// loop's "no more output yet" return must follow the platform, not a Linux literal.
    #[test]
    fn averror_eagain_is_the_negated_host_errno() {
        assert_eq!(AVERROR_EAGAIN, -libc::EAGAIN);
        #[cfg(target_os = "macos")]
        assert_eq!(AVERROR_EAGAIN, -35);
        #[cfg(target_os = "linux")]
        assert_eq!(AVERROR_EAGAIN, -11);
    }
}

#[cfg(test)]
mod pre_audio_headroom_tests {
    use super::*;

    const LEAD: i64 = crate::feed_state::AUDIO_LEAD_IN_NS + LEAD_IN_MARGIN_NS;

    const GRANT: VideoStep = VideoStep::Grant(PRE_AUDIO_VIDEO_HEADROOM);

    /// The audio gate fails at its ceiling only while the video read since the open or seek is
    /// shorter than the engine's prime, restarts with a seek, and always asks for the video
    /// lane to be starved below the pump's own feed-ahead target.
    #[test]
    fn the_audio_gate_follows_how_much_video_the_consumer_has_been_given() {
        let prime = crate::feed_state::PRIME_VIDEO_MAX_NS;
        let mut h = PreAudioHeadroom::default();
        assert_eq!(h.video_reach_ns(), 0);
        let lane = std::ptr::null_mut();
        assert!(audio_push_gate(lane, h.video_reach_ns()).fail_at_ceiling, "no video yet: the clock cannot start");
        h.video_pushed(1_000);
        h.video_pushed(1_000 + prime - 1);
        assert!(audio_push_gate(lane, h.video_reach_ns()).fail_at_ceiling, "just under the prime depth");
        h.video_pushed(1_000 + prime);
        let g = audio_push_gate(lane, h.video_reach_ns());
        assert!(!g.fail_at_ceiling, "the prime depth has been read: the clock may be running");
        assert_eq!(g.starved_below_ns, crate::feed_state::MAX_FEED_AHEAD_NS);
        assert_eq!(g.headroom, AUDIO_AHEAD_HEADROOM);
        h.restart(std::ptr::null_mut());
        assert_eq!(h.video_reach_ns(), 0, "a seek starts the count over");
    }

    /// The audio bound's geometry: the ceiling is base plus headroom, the reported film's 48 s of
    /// 768 kbit/s fits with room, the top of the codec's range does not run unbounded.
    #[test]
    fn the_audio_ceiling_holds_the_reported_film_and_not_everything() {
        let cap = crate::feed_state::aq_caps().1 + AUDIO_AHEAD_HEADROOM as i64;
        assert_eq!(cap, 17 * 1024 * 1024);
        let bytes = |kbit: f64, secs: f64| (kbit * 1e3 / 8.0 * secs) as i64;
        assert!(bytes(768.0, 48.0) < cap, "issue #600's 48 s lead");
        assert!(bytes(768.0, 48.0) > crate::feed_state::aq_caps().1, "and the base cap alone cannot hold it");
        assert!(bytes(768.0, 180.0) > AUDIO_AHEAD_HEADROOM as i64 && bytes(768.0, 170.0) < AUDIO_AHEAD_HEADROOM as i64, "16 MiB is ~2.9 min of 768 kbit/s");
        assert!(bytes(6_000.0, 60.0) > cap, "a bound, not a promise");
    }

    /// The grant is LAZY and made once: only at a video push that would park at the base cap with an
    /// audio lane being fed and no audio read. A push that fits (an ordinary file: video a few
    /// packets ahead of its audio), a file with no audio lane, or one whose audio came first never
    /// gets one.
    #[test]
    fn the_grant_is_made_once_and_only_when_video_would_park_ahead_of_the_audio() {
        let mut h = PreAudioHeadroom::default();
        assert_eq!(h.before_video(true, false, 0), VideoStep::Proceed, "room left: no grant");
        assert!(!h.granted);
        assert_eq!(h.before_video(true, true, 0), GRANT);
        assert_eq!(h.before_video(true, false, 0), VideoStep::Proceed, "the cap has room again");

        assert_eq!(
            PreAudioHeadroom::default().before_video(false, true, 0),
            VideoStep::Proceed,
            "no audio lane: nothing to wait for"
        );

        let mut audio_first = PreAudioHeadroom::default();
        assert!(audio_first.after_audio(0).first);
        assert_eq!(audio_first.before_video(true, true, 0), VideoStep::Proceed, "audio came first: no grant");
        assert!(!audio_first.after_audio(LEAD).release, "and so nothing to release");
    }

    /// The first audio AU does NOT release the grant: the engine needs `AUDIO_LEAD_IN_NS` of audio
    /// more, and the demuxer can only read it while the video lane still has room. The release
    /// comes exactly when that much audio, plus the margin, has been pushed.
    #[test]
    fn the_grant_is_held_until_the_audio_lead_in_has_been_pushed() {
        let mut h = PreAudioHeadroom::default();
        assert_eq!(h.before_video(true, true, 0), GRANT);
        let first = h.after_audio(11_008_000_000);
        assert!(first.first && !first.release, "the first AU alone releases nothing");
        assert!(!h.after_audio(11_008_000_000 + crate::feed_state::AUDIO_LEAD_IN_NS).release,
            "the engine's lead-in without the margin is not yet enough");
        let done = h.after_audio(11_008_000_000 + LEAD);
        assert!(!done.first && done.release, "lead-in plus margin pushed: release");
        assert!(!h.after_audio(11_008_000_000 + LEAD + 32_000_000).release, "released once");
    }

    /// A full lane under the grant with no audio and nothing popped since the grant is the hang
    /// the bound exists for; once audio is in, a full lane is ordinary backpressure (the lead-in
    /// logic owns what follows).
    #[test]
    fn a_full_lane_with_no_audio_and_no_pops_fails_the_open() {
        let mut h = PreAudioHeadroom::default();
        assert_eq!(h.before_video(true, true, 7), GRANT);
        assert_eq!(h.before_video(true, false, 7), VideoStep::Proceed, "room left");
        assert_eq!(h.before_video(true, true, 7), VideoStep::Fail, "full, no audio, nothing popped");
        h.after_audio(0);
        assert_eq!(h.before_video(true, true, 7), VideoStep::Proceed, "audio arrived: not this case");
    }

    /// **F1.** A lane whose consumer IS draining is not the hang: after a seek into an audioless
    /// stretch the pump pops video while the demuxer reads at LAN speed, and main parks that lane
    /// (and plays through the engine's video-only prime escape) instead of failing the open. The
    /// pop counter is read from a real lane.
    #[test]
    fn a_lane_that_is_draining_is_released_not_failed() {
        let mut q = crate::aq::aq_new(100);
        let qp = &mut *q as *mut AuQueue;
        let au = [0u8; 100];
        let push = || assert_eq!(crate::aq::aq_push(qp, au.as_ptr(), au.len() as c_int, 0, 1, 1), 0);
        let mut h = PreAudioHeadroom::default();
        for _ in 0..2 {
            push(); // 200 bytes held over the base cap of 100: a push would park
        }
        assert!(crate::aq::aq_would_park(qp));
        let VideoStep::Grant(extra) = h.before_video(true, crate::aq::aq_would_park(qp), crate::aq::aq_pops(qp)) else {
            panic!("a push that would park with no audio grants");
        };
        crate::aq::aq_set_headroom(qp, extra);
        // Fill past the standing cap: the next push would park again.
        while !crate::aq::aq_would_park(qp) {
            push();
        }
        assert_eq!(
            h.before_video(true, true, crate::aq::aq_pops(qp)),
            VideoStep::Fail,
            "full, no audio and no pops: the hang"
        );
        // The consumer pops, then the lane fills again.
        for _ in 0..3 {
            let n = crate::aq::aq_pop(qp, std::ptr::null_mut());
            assert!(!n.is_null());
            unsafe { libc::free(n as *mut c_void) };
        }
        while !crate::aq::aq_would_park(qp) {
            push();
        }
        assert_eq!(
            h.before_video(true, crate::aq::aq_would_park(qp), crate::aq::aq_pops(qp)),
            VideoStep::Release,
            "pops happened since the grant: the consumer is draining, so this is not the hang"
        );
        assert!(h.released);
        assert_eq!(h.before_video(true, true, crate::aq::aq_pops(qp)), VideoStep::Proceed, "released once");
    }

    /// **F3.** The seek reset must take the lane's grant away. Restarting the struct alone left a
    /// seek that landed while the grant was unreleased with the video lane at base + 24 MiB for the
    /// rest of the session (the next epoch may be audio-first, or carry audio whose pts never span
    /// the lead-in), with the exhaustion check off.
    #[test]
    fn a_seek_with_the_grant_outstanding_returns_the_lane_to_its_base_cap() {
        let mut q = crate::aq::aq_new(100);
        let qp = &mut *q as *mut AuQueue;
        let mut h = PreAudioHeadroom::default();
        let VideoStep::Grant(extra) = h.before_video(true, true, 0) else { panic!("grant") };
        crate::aq::aq_set_headroom(qp, extra);
        assert_eq!(crate::aq::aq_cap(qp), 100 + extra as i64);
        h.restart(qp);
        assert_eq!(crate::aq::aq_cap(qp), 100, "the lane is back at its base cap after the seek reset");
        assert_eq!(h.before_video(false, true, 0), VideoStep::Proceed, "and the state started over");
        assert!(!h.granted && !h.released && h.first_audio_ns.is_none());
        // The next epoch grants afresh.
        assert_eq!(h.before_video(true, true, 0), GRANT);
    }

    /// With the lane holding more than its base cap, the seek reset still never drops the cap under
    /// what the lane holds (`aq_release_headroom`), and a grant that was already released (or never
    /// made) is left alone.
    #[test]
    fn restart_releases_through_the_drain_and_leaves_settled_states_alone() {
        let mut q = crate::aq::aq_new(100);
        let qp = &mut *q as *mut AuQueue;
        let au = [0u8; 100];
        crate::aq::aq_set_headroom(qp, 1_000);
        for _ in 0..5 {
            assert_eq!(crate::aq::aq_push(qp, au.as_ptr(), au.len() as c_int, 0, 1, 1), 0);
        }
        let mut h = PreAudioHeadroom { granted: true, ..Default::default() };
        h.restart(qp);
        assert_eq!(crate::aq::aq_cap(qp), 500, "never under the lane's occupancy");
        // Never granted: no touch.
        crate::aq::aq_set_headroom(qp, 1_000);
        PreAudioHeadroom::default().restart(qp);
        assert_eq!(crate::aq::aq_cap(qp), 1_100, "nothing granted by this state, nothing withdrawn");
    }

    /// The geometry the constants claim, pinned: the cap is the base plus the headroom, an 11 s delay
    /// at the measured rates fits, and the fixture-sized read-ahead plus the engine's lead-in still
    /// fits, while a delay at ~35 Mbit/s does not (the bound is real).
    #[test]
    fn the_bound_holds_the_measured_cases_and_not_everything() {
        let base = crate::feed_state::aq_caps().0;
        let cap = base + PRE_AUDIO_VIDEO_HEADROOM as i64;
        assert_eq!(cap, 34 * 1024 * 1024);
        let bytes = |mbit: f64, secs: f64| (mbit * 1e6 / 8.0 * secs) as i64;
        // the reported film: 11.008 s of ~9.5 Mbit/s video
        let film = bytes(9.495_770, 11.008);
        assert!(film > base, "the plain cap alone cannot hold it");
        assert!(film < cap);
        // the fixture: 12 Mbit/s, same delay, and then the lead-in's worth of video as well
        let lead_s = (crate::feed_state::AUDIO_LEAD_IN_NS + LEAD_IN_MARGIN_NS) as f64 / 1e9;
        assert!(bytes(12.0, 11.008 + lead_s) < cap, "headroom spans the pre-audio stretch AND the lead-in");
        assert!(bytes(35.0, 11.008) > cap, "a bound, not a promise");
    }
}

/// The demux thread body (spawned by `engine::start_bufferfeed`).
///
/// Takes an [`Origin`](Origin) rather than a `(host, port)` pair because **the scheme
/// decides the transport**: `http` reads through the Engine's `stream.rs` socket, `https` through
/// the injected range reader. An origin is parsed from a URL and never rebuilt from an address, which is
/// what keeps the `plex.direct` hostname TLS validates against intact all the way down here
/// (`net/origin.rs`). `hs` is still passed on both paths — it is the Engine's, and it stays
/// unused (fd = -1, published as `FEED_SHARED.hs_ptr`) when the origin turns out to be https.
pub fn demux(
    off: &plx_base::task::OffFrame,
    origin: Origin,
    path: String,
    acodec: String,
    transport: DemuxTransport,
    mut session: Box<dyn DemuxSession>,
    abr: Option<Box<dyn plx_playback_contract::adaptive::AdaptiveSession>>,
    auto_original: Option<Box<dyn plx_playback_contract::adaptive::OriginalSession>>,
    aq: SendPtr<AuQueue>,
    aqa: SendPtr<AuQueue>,
    hs: SendPtr<HttpStream>,
) {
    PUSHED_ANY.store(false, Ordering::Relaxed);
    session.media().publish(DemuxEffect::SideAnchor(None));
    // Refuse rather than read a struct whose shape we do not know (see `boot`). EOF must still be
    // set on both lanes or `pump` waits forever on a queue nothing will ever fill — the same
    // contract the panic barrier below exists to keep.
    if !abi_ok() {
        session.media().record("demux: refusing — bundled FFmpeg ABI is not the one built for");
        crate::aq::aq_set_eof(aq.0);
        crate::aq::aq_set_eof(aqa.0);
        return;
    }
    DIAG_FIRST.store(true, Ordering::Relaxed);
    ensure_registered();
    let aq_p = aq.0; // VIDEO lane (also the AVIO abort ptr + EOF marker)
    let aqa_p = aqa.0; // AUDIO lane (es=2) — always a distinct queue on the ff (two-lane) path
    let hs_p = hs.0;
    if path
        .split_once('?')
        .map_or(path.as_str(), |(plain, _)| plain)
        .to_ascii_lowercase()
        .ends_with(".m3u8")
    {
        session.media().record("hls: segmented demux start");
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            hls_demux(off, &origin, &path, &acodec, abr, aq_p, aqa_p, hs_p, transport, session.as_mut())
        }));
        match outcome {
            Ok(Ok(())) | Ok(Err(HlsExit::Aborted)) => {}
            Ok(Err(HlsExit::Failed(why))) => {
                session.media().record(&format!("hls: demux failed: {why}"));
                if PUSHED_ANY.load(Ordering::Relaxed) {
                    session.media().publish(DemuxEffect::IoFailed);
                } else {
                    session.media().publish(DemuxEffect::FailFault(DemuxFault::HlsFailed));
                }
            }
            Ok(Err(HlsExit::NotReady)) => {
                session.media().record("hls: demux failed: playlist resource was not ready");
                if PUSHED_ANY.load(Ordering::Relaxed) {
                    session.media().publish(DemuxEffect::IoFailed);
                } else {
                    session.media().publish(DemuxEffect::FailFault(DemuxFault::HlsNotReady));
                }
            }
            Ok(Err(HlsExit::PrimeExpired)) => {
                session.media().record("hls: demux failed: unexpected active-stream prime deadline");
                session.media().publish(DemuxEffect::IoFailed);
            }
            Ok(Err(HlsExit::StallAbort(_))) => {
                // The abort rule is handled where it fires, inside the segment loop; reaching here
                // means it escaped one of the candidate paths, which do not arm it.
                session.media().record("hls: demux failed: stall abort escaped the segment loop");
                session.media().publish(DemuxEffect::IoFailed);
            }
            Err(_) => {
                session.media().record("hls: demux panicked");
                session.media().publish(DemuxEffect::FailFault(DemuxFault::DemuxPanicked));
            }
        }
        if unproductive_exit_failed(PUSHED_ANY.load(Ordering::Relaxed), unsafe {
            crate::aq::aq_is_aborted(aq_p)
        }) {
            session.media().publish(DemuxEffect::FailFault(DemuxFault::NoAccessUnits));
        }
        crate::aq::aq_set_eof(aq_p);
        crate::aq::aq_set_eof(aqa_p);
        session.media().record("hls: segmented demux ended");
        return;
    }
    let mut original_watch = auto_original;
    if let Some(watch) = original_watch.as_ref() {
        // Publish Auto's Original source before its first measurement window.
        session.media().publish(DemuxEffect::AbrOriginalStart {
            kbps: i64::from(watch.source_kbps()),
        });
    }
    if let Some(watch) = original_watch.as_mut() { watch.begin_on_worker(); }
    // Same accepted-event cursor as HLS. Progressive reads and queue pushes can hide an entire
    // pause cycle, so a local `paused_since` sampled around packets is not a valid clock.
    session.clock_mut().begin_pause_lane(PauseLane::Original);
    // `base()` re-brackets a v6 host, which is what a URL needs; the plaintext arm hands the
    // `Origin` itself to `open_plain_progressive`, which dials its BARE `host()`.
    let url = format!("{}{}", origin.base(), path); // delivery identity; may carry a token, never logged

    // PANIC BARRIER around the whole producer body. Not about the unwind itself — this thread is
    // started by `task::spawn`, so std already catches a panic at the thread boundary and turns it
    // into the `Err` that `task::join` logs. The problem is what the unwind SKIPS: the two
    // `aq_set_eof` calls in the tail below. The consumer is a one-way FIFO with no other liveness
    // signal — `aq_pop` reports EOF only from the `eof` flag the producer sets — so a producer that
    // dies without setting it leaves `pump` feeding a queue that will never fill and never end:
    // no EOS is ever pushed (`engine.rs`'s EOS is keyed on the video lane's true EOF), no error is
    // ever surfaced, and the app sits on a frozen picture until BACK. That is strictly worse than
    // the crash it came from — a rare panic became a common-looking hang with no log line pointing
    // at it. So: catch it, name it in the event log, and then run the SAME teardown the normal exit
    // runs, so the pump terminates the way it does at end-of-stream.
    //
    // `AssertUnwindSafe` is honest here rather than a rubber stamp: the state the closure mutates
    // across the boundary is the two AU queues (their own pthread mutexes, and this is their only
    // producer, which is now dead) and `FEED_SHARED`'s atomics. What a panic DOES leak is the FFmpeg
    // side — `AVFormatContext`/`AVIOContext`/`AVPacket` are freed at the end of the block, not by a
    // `Drop`, so an unwind past them leaks them plus the socket buffer. That is deliberate: the
    // alternative is calling `avformat_close_input` on a context whose invariants a panicking
    // libavformat may have left broken, and a one-off leak on a path that is meant never to run is
    // cheaper than a segfault. The session ends here either way.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Run-once breakable block, NOT a retry loop: every `break` below is an early exit to the
        // EOF/cleanup tail after it. It used to be a real loop that reopened the whole
        // AVFormatContext on a fresh URL, which is how a direct-play seek was supposed to reach its
        // target — but the reopen could never be triggered (see the seek note in the read loop), and
        // both remaining callers replace the engine outright instead: a transcode seek and an
        // audio-track switch each build a new start.mkv and `reload_transcode`, which spawns a new
        // demux thread. So the reopen had no live writer and no live caller.
        loop {
            unsafe {
                // Publish curl's wake target BEFORE the final AU-abort check. These two operations
                // close each other's race: teardown either sees and signals the reservation, or it
                // aborts the lane first and this check refuses to start I/O. Creating/registering the
                // source only after the check leaves a window where teardown's one wake sees nothing
                // and the demuxer then opens a fresh connection under the main thread's join.
                let mut curl_open = if origin.is_tls() {
                    match transport.range.reserve_open() {
                        Ok(r) => Some(r),
                        Err(e) => {
                            session.media().record(&format!("ff: https reservation FAILED: {e:?}"));
                            session.media().publish(DemuxEffect::FailFault(DemuxFault::HttpsReserveFailed));
                            break;
                        }
                    }
                } else {
                    None
                };
                // Teardown may have raced us here (it aborts the lanes, then signals both transports,
                // then JOINS this thread on the main thread). The reservation above makes the HTTPS
                // side as race-free as the socket's already-published `hs_ptr`.
                if crate::aq::aq_is_aborted(aq_p) {
                    session.media().record("ff: aborted before reopen");
                    break;
                }
                // ONE decision, here, from the scheme; the only later switch is a plaintext open
                // redirected to https, which `open_plain_progressive` hands to curl. Both arms publish the same two diagnostics (`dg_http_status`, `file_size`)
                // before anything else can fail, because the read-out panel is the first thing anybody
                // looks at when a part will not play and it must mean the same thing either way.
                let open_began = std::time::Instant::now();
                let (src, size) = if origin.is_tls() {
                    let reservation = curl_open
                        .take()
                        .expect("TLS reserved its abort handle above");
                    match transport.range.open_reserved(&url, 0, reservation) {
                        Ok(cs) => {
                            let (st, size) = (cs.status(), cs.size());
                            session.media().publish(DemuxEffect::FileSize(size));
                            session.media().publish(DemuxEffect::HttpStatus(st));
                            session.media().record(&format!("ff: open https status={st} clen={size}"));
                            (Src::Curl(cs), size)
                        }
                        Err(e) => {
                            // A status we could not stream is worth publishing; a transport failure has
                            // no status, and 0 is how the panel already spells "never answered".
                            if let range_io::OpenErr::Status(st) = e {
                                session.media().publish(DemuxEffect::HttpStatus(st));
                            }
                            session.media().record(&format!("ff: https open FAILED: {e:?}"));
                            session.media().publish(DemuxEffect::FailFault(DemuxFault::HttpsOpenFailed));
                            break;
                        }
                    }
                } else {
                    // Redirects followed; an https hop comes back as the curl source.
                    match open_plain_progressive(hs_p, &origin, &path, aq_p, &transport,
                        session.observer().as_ref()) {
                        Ok(opened) => opened,
                        Err(MediaOpenFail::Aborted) => {
                            session.media().record("ff: aborted during open");
                            break;
                        }
                        Err(MediaOpenFail::Failed) => {
                            session.media().publish(DemuxEffect::FailFault(DemuxFault::HttpOpenFailed));
                            break;
                        }
                    }
                };

                session.media().record(&format!("ff: open ms={}", open_began.elapsed().as_millis()));

                let mut state = Box::new(AvioState {
                    src,
                    range: Arc::clone(&transport.range),
                    credential_gate: transport.credential_gate,
                    observer: session.observer(),
                    aq: aq_p,
                    off: 0,
                    size,
                    io_failed: false,
                    body_active_us: 0,
                    body_bytes: 0,
                    first_byte_at: None,
                    reserve_deadline: ReserveDeadlineState::new(None, false, session.observer()),
                    transport_watchdog: None,
                    // A progressive part is not on a ladder, so there is no cheaper rung to run to
                    // and the abort rule's escape does not exist. R12's terminal case, structurally.
                    acquisition: None,
                    bounce: Vec::new(),
                    bounce_pos: 0,
                    parked_at_end: false,
                    body_silence: BODY_SILENCE,
                    heal: HealBudget::default(),
                });
                let buf = av_malloc(65536) as *mut u8;
                if buf.is_null() {
                    session.media().record("ff: av_malloc failed");
                    session.media().publish(DemuxEffect::NoteFault(DemuxFault::Other));
                    break;
                }
                let avio = avio_alloc_context(
                    buf,
                    65536,
                    0,
                    &mut *state as *mut AvioState as *mut c_void,
                    Some(read_cb),
                    None,
                    Some(seek_cb),
                );
                if avio.is_null() {
                    session.media().record("ff: avio_alloc_context failed");
                    session.media().publish(DemuxEffect::NoteFault(DemuxFault::Other));
                    free_ptr(buf as *mut c_void);
                    break;
                }
                let mut fmt = avformat_alloc_context();
                if fmt.is_null() {
                    session.media().record("ff: avformat_alloc_context failed");
                    session.media().publish(DemuxEffect::NoteFault(DemuxFault::Other));
                    free_avio(avio);
                    break;
                }
                (*fmt).pb = avio;
                let r = avformat_open_input(
                    &mut fmt,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
                if r < 0 || fmt.is_null() {
                    let lane_aborted = crate::aq::aq_is_aborted(aq_p);
                    session.media().record(&open_input_failure_note(r, lane_aborted));
                    // A stop teardown asked for is not the source failing to open.
                    if !lane_aborted {
                        session.media().publish(DemuxEffect::NoteFault(DemuxFault::OpenInputFailed));
                    }
                    free_avio(avio);
                    break;
                }
                // The file's font attachments leave the format's stream table now, into the buffers
                // the source store shares with every renderer (issue #1328): left in
                // `codecpar->extradata` they would sit beside the demuxer's own copy (the Matroska
                // element buffer, the only one left once `find_stream_info` rebuilds the codec
                // context from the emptied parameters) for the whole playback. Nothing reads an attachment's extradata between here and the
                // hand-over to `begin_ass_sources` below.
                let font_attachments = plx_media_codecs::subtitle_extract::detach_fonts(fmt);
                if avformat_find_stream_info(fmt, std::ptr::null_mut()) < 0 {
                    session.media().record("ff: find_stream_info failed");
                    if !crate::aq::aq_is_aborted(aq_p) {
                        session.media().publish(DemuxEffect::NoteFault(DemuxFault::FindStreamInfoFailed));
                    }
                    avformat_close_input(&mut fmt);
                    free_avio(avio);
                    break;
                }
                // The container's own track names, published before anything else can fail: the track
                // menu is the only reader and it wants them whether or not this part turns out to have
                // a video stream. See `stream_name` for why they are read at all — for an MP4 they are
                // the ONLY place a track's identity exists, PMS having dropped it.
                {
                    let (audio, subs) = track_names(fmt);
                    let named = audio
                        .iter()
                        .chain(subs.iter())
                        .filter(|n| !n.is_empty())
                        .count();
                    if named > 0 {
                        session.media().record(&format!(
                            "ff: container names {named}/{} tracks (a={} s={})",
                            audio.len() + subs.len(),
                            audio.len(),
                            subs.len()
                        ));
                    }
                    session.media().publish(DemuxEffect::TrackNames { audio, subs });
                }
                let vi =
                    av_find_best_stream(fmt, AVMEDIA_TYPE_VIDEO, -1, -1, std::ptr::null_mut(), 0);
                // native audio-track selection: feed the chosen Nth audio stream (FEED_SHARED.desired_
                // audio_idx, set by the track menu), else the pipeline's best/default audio.
                let want_aidx = session.clock().desired_audio_idx();
                let ai = if want_aidx >= 0 {
                    nth_audio_stream(fmt, want_aidx).unwrap_or_else(|| {
                        av_find_best_stream(
                            fmt,
                            AVMEDIA_TYPE_AUDIO,
                            -1,
                            -1,
                            std::ptr::null_mut(),
                            0,
                        )
                    })
                } else {
                    // default: feed the track matching the Load payload's codec (Media[0].audioCodec),
                    // NOT av_find_best_stream — a codec mismatch stalls the audio ES and wedges video.
                    audio_stream_matching(fmt, &acodec).unwrap_or_else(|| {
                        av_find_best_stream(
                            fmt,
                            AVMEDIA_TYPE_AUDIO,
                            -1,
                            -1,
                            std::ptr::null_mut(),
                            0,
                        )
                    })
                };
                if vi < 0 {
                    // "No video stream" alone reads as a demuxer fault. When audio IS present the
                    // demuxer worked fine — the stream itself arrived without video, which on a
                    // transcode URL means the server dropped the track (no usable video target —
                    // issue #22: an HEVC-only target on a server without Plex Pass). Record which
                    // shape this is so the error the user sees can say so; the main thread words it.
                    if ai >= 0 {
                        session.media().publish(DemuxEffect::NoVideo);
                        session.media().record("ff: no video stream — the stream carries audio only");
                    } else {
                        session.media().record("ff: no video stream");
                    }
                    session.media().publish(DemuxEffect::NoteFault(DemuxFault::NoVideoStream));
                    avformat_close_input(&mut fmt);
                    free_avio(avio);
                    break;
                }
                let streams = (*fmt).streams;
                let vst = *streams.add(vi as usize);
                let vcp = stream_codecpar(vst);
                // AAC needs ADTS framing for LG's decoder (mp4/mkv carry raw AAC). Precompute the
                // per-frame ADTS fields (freq index + channel config) for the selected audio stream;
                // None => not AAC (or a non-standard rate) => fed verbatim.
                let aac_adts: Option<(u8, u8)> = if ai >= 0 {
                    let acp = stream_codecpar(*streams.add(ai as usize));
                    if (*acp).codec_id == AV_CODEC_ID_AAC {
                        adts_params(acp)
                    } else {
                        None
                    }
                } else {
                    None
                };
                // Native DTS: the set's own decoder takes the core. Not when the Load declared the lossy PCM,
                // which is the app decoding DTS itself (`pcm_dec` below, `Caps::app_decodes_lossy_track`).
                let dts_audio = ai >= 0 && !pcm_lossy() && std::ffi::CStr::from_ptr(avcodec_get_name(
                    (*stream_codecpar(*streams.add(ai as usize))).codec_id
                )).to_bytes() == b"dts";
                if dts_audio {
                    session.media().record("ff: DTS core extraction on (DTS-HD extensions discarded)");
                }
                // Playback speed: this session's Load declared PCM, so the audio must be decoded,
                // stretched and fed as PCM. A stream with no bundled decoder cannot honour it — the
                // engine refused the speed at Load (`player::session_rate`), so this is a guard.
                let speed_permille = session.clock().applied_rate_permille();
                // App-decoded audio (FLAC, ALAC, LPCM, and the lossy rows of `devcaps::APP_DECODED` the attempt decodes in the app): decode to the PCM the
                // Load declared, or refuse the stream — the Load and the file must agree, or the
                // panel plays the wrong pitch. Not at a speed: that session's Load declares the
                // speed decoder's own PCM (`engine::app_pcm_for_rate`), `SpeedDecoder` below decodes
                // it too, and the two must never both decode one stream.
                let app_decoded = if ai >= 0 {
                    let name = std::ffi::CStr::from_ptr(avcodec_get_name(
                        (*stream_codecpar(*streams.add(ai as usize))).codec_id
                    )).to_string_lossy();
                    // A lossy codec is the app's only when the Load declared PCM for it (the attempt
                    // decodes it in the app); otherwise it is native audio and the set's decoder takes it.
                    plx_platform::devcaps::app_decoded_by_ffmpeg_name(&name)
                        .filter(|c| !c.lossy || pcm_lossy())
                        .map(|_| name.into_owned())
                } else {
                    None
                };
                // The Load said PCM but this stream has no app decoder (float or 8-bit LPCM under
                // PMS's `pcm`): refuse it the way an open refusal ends. Fed raw, the TV would play
                // it as the declared S16/S32 — noise, with no error to recover from.
                if ai >= 0 && pcm_load_feeds_raw(
                    pcm_out().is_some(), FEED_AUDIO.load(Ordering::Relaxed), app_decoded.is_some(),
                ) {
                    let name = std::ffi::CStr::from_ptr(avcodec_get_name(
                        (*stream_codecpar(*streams.add(ai as usize))).codec_id
                    )).to_string_lossy();
                    session.media().record(&format!(
                        "ff: {name} refused — the Load declared PCM but the app has no {name} decoder"
                    ));
                    session.media().publish(DemuxEffect::Failed);
                    avformat_close_input(&mut fmt);
                    free_avio(avio);
                    break;
                }
                let mut pcm_dec: Option<crate::ff_pcm::PcmDecoder> = None;
                if let Some(name) = app_decoded.as_deref().filter(|_| {
                    FEED_AUDIO.load(Ordering::Relaxed) && speed_permille == crate::shared::RATE_NORMAL
                }) {
                    let opened = match pcm_out() {
                        Some(out) => crate::ff_pcm::PcmDecoder::open(
                            stream_codecpar(*streams.add(ai as usize)), out,
                        ),
                        None => Err(format!("the Load declared no PCM for a {name} track")),
                    };
                    match opened {
                        Ok(d) => {
                            session.media().record(&format!(
                                "ff: {name} -> PCM decoding on{}",
                                if d.core_only() { " (core only)" } else { "" }
                            ));
                            pcm_dec = Some(d);
                        }
                        Err(why) => {
                            session.media().record(&format!("ff: {name} refused — {why}"));
                            session.media().publish(DemuxEffect::Failed);
                            avformat_close_input(&mut fmt);
                            free_avio(avio);
                            break;
                        }
                    }
                }
                // Decoded blocks shorter than 10 ms (TrueHD's 1/1200 s access units) go to the lane
                // gathered into AUs of about that length (`ff_pcm::AuBatcher`).
                let mut pcm_batch = pcm_dec.as_ref().map(|d| d.au_batcher());
                if let Some((fi, ch)) = aac_adts {
                    session.media().record(&format!(
                        "ff: AAC → ADTS reframing on (freq_idx={fi} ch={ch})"
                    ));
                }
                let mut speed_dec = if ai >= 0 && speed_permille != crate::shared::RATE_NORMAL {
                    let d = SpeedDecoder::open(stream_codecpar(*streams.add(ai as usize)), speed_permille);
                    session.media().record(&format!(
                        "speed: audio decoder for {speed_permille} {}",
                        if d.is_some() { "open" } else { "UNAVAILABLE" }
                    ));
                    d
                } else {
                    None
                };
                // Opus goes to the television's own decoder (`ff_native_audio.rs`; its header travels
                // in the Load). What is fed natively is recorded for the player, which learns a
                // refusal from it. FLAC, ALAC and LPCM are decoded in the app (`pcm_dec`) and are not native.
                if ai >= 0 && FEED_AUDIO.load(Ordering::Relaxed) && speed_dec.is_none() {
                    let acp = stream_codecpar(*streams.add(ai as usize));
                    let aname = std::ffi::CStr::from_ptr(avcodec_get_name((*acp).codec_id)).to_string_lossy().into_owned();
                    if let Some(kind) = native_audio::Kind::from_ffmpeg_name(&aname) {
                        let channels = u32::try_from((*acp).ch_layout.nb_channels).unwrap_or(0);
                        session.media().record(&format!(
                            "ff: {aname} fed to the TV's own decoder (channels={channels} rate={})",
                            (*acp).sample_rate
                        ));
                        native_audio::publish_fed(kind, channels);
                    }
                }
                let dur = fmt_duration(fmt);
                if dur > 0 {
                    session.media().publish(DemuxEffect::DurationNs(dur.saturating_mul(1000)));
                }
                // The NAME as well as the id, and the name is what anything downstream should grade.
                // A raw AV_CODEC_ID is an FFmpeg enum, and that enum RENUMBERS between majors — H264
                // is 28 on the n3.3 the televisions ship, 27 on FFmpeg 6, and HEVC is 174 / 173 / 172
                // across n3.3 / 6 / 9. The on-device suite asserted the bare number, so bundling our
                // own FFmpeg failed all 21 cases at once on a codec the app had identified perfectly
                // well. `avcodec_get_name` is stable, means what the assertion meant, and is free.
                // Publish the coded size: the webOS 5+ exported window needs the frame it is being
                // fed, and this is the only place that knows it for certain — a transcode's declared
                // dimensions and its actual output need not agree.
                session.media().publish(DemuxEffect::VideoRaster {
                    width: (*vcp).width, height: (*vcp).height,
                });
                let cname =
                    std::ffi::CStr::from_ptr(avcodec_get_name((*vcp).codec_id)).to_string_lossy();
                // Refuse a stream the pipeline cannot be fed, BEFORE anything else is opened for it
                // (subtitle decoders, ASS sources): the exit then has the same shape as the "no video
                // stream" one above, with nothing to release but the input and its AVIO.
                let Some(framing) = VideoFraming::of_codec_id((*vcp).codec_id) else {
                    session.media().record(&format!(
                        "ff: no framing for video codec {cname} (codec_id={}); refusing the stream",
                        (*vcp).codec_id
                    ));
                    session.media().publish(DemuxEffect::FailFault(DemuxFault::NoFraming));
                    avformat_close_input(&mut fmt);
                    free_avio(avio);
                    break;
                };
                // How the stream delimits its NAL units, from its extradata (`NalInput`). The route
                // trusts the part's file NAME, so a transport or elementary stream called `.mp4`
                // arrives here with no avcC/hvcC record and start codes in every packet; walked by
                // length, each access unit came out as the five bytes `00 00 00 01 09` (#1084).
                // H.264 is framed in place (`annexb_h264_access_unit`); HEVC has no such framing, so
                // it is refused here, like a codec with no framing, rather than misframed.
                let extradata: &[u8] = if (*vcp).extradata.is_null() || (*vcp).extradata_size <= 0 {
                    &[]
                } else {
                    std::slice::from_raw_parts((*vcp).extradata, (*vcp).extradata_size as usize)
                };
                let nal_input = match framing {
                    VideoFraming::AnnexB { .. } => NalInput::of_extradata(extradata),
                    VideoFraming::Av1 | VideoFraming::Vp9 => NalInput::LengthPrefixed,
                };
                // Such a stream's clock is moved to 0 here, once (`stream_origin_ns`).
                let origin_ns = stream_origin_ns(fmt_start_time(fmt), nal_input);
                if origin_ns != 0 {
                    session.media().record(&format!(
                        "ff: the stream's clock starts at {}ms; rebased to 0",
                        origin_ns / 1_000_000
                    ));
                }
                if nal_input == NalInput::StartCodes && framing == (VideoFraming::AnnexB { hevc: true }) {
                    session.media().record(
                        "ff: HEVC with start codes and no hvcC record (a transport or elementary stream) has no framing here; refusing the stream",
                    );
                    session.media().publish(DemuxEffect::FailFault(DemuxFault::NoFraming));
                    avformat_close_input(&mut fmt);
                    free_avio(avio);
                    break;
                }
                // What the Load declared (`dg_load_v`, 0 when this start sent a static payload) must
                // be what the file is: a Load that says one codec over another codec's bitstream is a
                // black screen, so stop and name both.
                let Some(declared) = session.observer().declared_video_code() else {
                    avformat_close_input(&mut fmt);
                    free_avio(avio);
                    break;
                };
                // BOTH halves of the same check in one pass. A stale PMS declaration (a file
                // replaced under the server, never re-analyzed) can lie on BOTH axes at once:
                // Casablanca 4K arrives declared H264/AC3 while the file carries HEVC/AAC. If the
                // open refused axis-by-axis, the first axis hit would break before the second was
                // ever probed, the one-shot retry would reload with the corrected video, get
                // refused again on the audio the first pass never saw, and exhaust its budget
                // without having played a frame. Probe both, refuse once, and hand the pump every
                // axis the file really carries so ONE retry reload fixes them together.
                //
                // The audio half matters on its own too: a declared audio codec no track in the
                // file carries makes `ai` land on the track that IS there and feed it into an
                // elementary stream configured for the declared codec — the audio clock stalls
                // and the video, which syncs to it, wedges at t=0 with no error at all.
                let mismatch_v = declared != 0 && declared != framing.load_video().diag_code();
                let declared_a = session.observer().declared_audio_code().unwrap_or(0);
                let (actual_a, a_name) = if declared_a != 0 && ai >= 0 {
                    let acp = stream_codecpar(*streams.add(ai as usize));
                    let aname = std::ffi::CStr::from_ptr(avcodec_get_name((*acp).codec_id));
                    (
                        match aname.to_bytes() {
                            b"ac3" => 1,
                            b"eac3" => 2,
                            b"aac" => 3,
                            b"dts" => 4,
                            _ => 0,
                        },
                        aname.to_string_lossy().into_owned(),
                    )
                } else {
                    (0, String::new())
                };
                let mismatch_a = audio_declaration_mismatch(declared_a, actual_a);
                if mismatch_v || mismatch_a {
                    // The conditional probe's answer: what the file REALLY is, in the Load's own
                    // code space, for every axis that lied. The pump's one-shot retry reloads with
                    // this declaration; the happy path never reaches this line.
                    session.media().publish(DemuxEffect::ProbeActual {
                        video: mismatch_v.then(|| framing.load_video().diag_code()),
                        audio: mismatch_a.then_some(actual_a),
                    });
                    session.media().record(&format!(
                        "ff: the Load declared {dv} / audio {da} but the file is {av} / {aa}; refusing the stream",
                        dv = crate::LoadVideo::from_diag_code(declared)
                            .map_or("—", crate::LoadVideo::payload_name),
                        da = if declared_a != 0 { acodec.as_str() } else { "—" },
                        av = cname,
                        aa = if a_name.is_empty() { "—" } else { a_name.as_str() }
                    ));
                    session.media().publish(DemuxEffect::FailFault(DemuxFault::LoadMismatch));
                    avformat_close_input(&mut fmt);
                    free_avio(avio);
                    break;
                }
                // WHAT THE STREAM ACTUALLY IS, not just what it is called. `codec=hevc` is the same
                // four letters for an SDR file, an HDR10 file, a Dolby Vision Profile 8.1 file whose
                // base layer is that same HDR10, and a Profile 5 file that will display in visibly
                // wrong colours — `avcodec_get_name` cannot tell them apart and neither could this log
                // line or any assertion built on it. These fields can. `trc`/`pri`/`spc` are the raw
                // AVCOL_* enum values, logged as NUMBERS because naming them would mean binding three
                // more FFmpeg symbols for a diagnostic: trc 16 = smpte2084 (PQ/HDR10), 18 = arib-std-b67
                // (HLG), spc 9 = bt2020nc, pri 9 = bt2020, and **2 = UNSPECIFIED on all three** (every
                // one of those six numbers checked against the vendored `libavutil/pixfmt.h`, not
                // recalled). A Profile 5 file is expected to read 2/2/2, since IPT-PQ signals no
                // ordinary transfer at all — inferred rather than seen from here, but from the same
                // probe: PMS reports NO `colorTrc`, `colorSpace` or `colorPrimaries` at all on the dev
                // server's P5 item while sending all three on every P8 (swept 2026-08-21). This line
                // is where a television settles it.
                // All three were declared in the offsets table above and read NOWHERE until now.
                let dv = match dovi_conf(vcp) {
                    // `bl_compat` is the field that decides whether the base layer means anything on
                    // its own, so it is logged beside the profile rather than left to be inferred
                    // from it — 0 is Profile 5's "none", 1 the HDR10 of a Profile 8.1.
                    Some(d) => format!(
                        " dovi=P{} level={} bl_compat={} rpu={} el={} bl={}",
                        d.dv_profile,
                        d.dv_level,
                        d.dv_bl_signal_compatibility_id,
                        d.rpu_present_flag,
                        d.el_present_flag,
                        d.bl_present_flag
                    ),
                    None => String::new(),
                };
                let line = format!(
                    "ff: v=#{vi} codec={cname} codec_id={} {}x{} trc={} pri={} spc={}{dv} a=#{ai} dur_ns={}",
                    (*vcp).codec_id,
                    (*vcp).width,
                    (*vcp).height,
                    (*vcp).color_trc,
                    (*vcp).color_primaries,
                    (*vcp).color_space,
                    session.clock().duration_ns()
                );
                session.media().record(&line);

                // Client-rendered subtitles (direct-play only): enumerate subtitle streams in FILE
                // order so their 0-based position maps 1:1 to the track menu's desired_sub_idx (which
                // indexes metadata d.subs, i.e. every streamType==3 stream in document order). The
                // selected track is read LIVE in the loop below, so switching subtitles mid-play takes
                // effect with no reopen (parity with mkv.rs's active_sub_track).
                // A side reader that owns this playback's subtitles (`ff_subside.rs`) demuxes them
                // from the original Part; this stream's own enumeration and ASS source would be
                // replaced by it.
                let side_owner = session.clock().side_subtitle_owner();
                let mut sub_tracks = if side_owner {
                    SubTracks::empty()
                } else {
                    let infos = SubTracks::inspect(fmt, streams);
                    let plan = session.subtitle().plan(&infos);
                    SubTracks::open(streams, infos, plan)
                };
                let ass_generation = if side_owner {
                    // The side reader has its own copy of the fonts; this one has no reader.
                    drop(font_attachments);
                    0
                } else {
                    begin_ass_sources(session.as_mut(), &url, streams, &sub_tracks, font_attachments).unwrap_or(0)
                };
                if !side_owner { session.subtitle().record_tracks(); }

                // Hand-roll length-prefix -> Annex-B + prepend VPS/SPS/PPS at every keyframe (the
                // format Starfish decodes). FFmpeg's hevc_mp4toannexb does NOT reliably prepend the
                // parameter sets on this 3.3 build (it leaves the keyframe starting with SEI), so we
                // build the AU from the codecpar extradata; libavformat still owns demux + seeking.
                // AV1 is framed by av1_packet_to_tu as Section-5 temporal units with the av1C config
                // in-band on keyframes, because the TV pipeline is given no codec_data for AV1.
                let is_hevc = framing == VideoFraming::AnnexB { hevc: true };
                // A Dolby Vision Profile 7 with a cross-compatible base layer is converted to
                // Profile 8.1 while it is fed (`packet_to_annexb` drops the enhancement layer and
                // rewrites the RPUs), so the pipeline is handed the single-layer stream the Load
                // payload declares (`plx_data::metadata::Dovi::converts_to_p81`). Armed whether or
                // not a node was sent: undeclared, what remains is the HDR10 base layer.
                let dv_p7 = is_hevc && dovi_conf(vcp).is_some_and(|d| dovi_converts_to_p81(&d));
                if dv_p7 {
                    session.media().record(
                        "ff: Dolby Vision P7 -> P8.1 conversion armed (EL dropped, RPUs rewritten)",
                    );
                }
                let is_av1 = framing == VideoFraming::Av1;
                let is_vp9 = framing == VideoFraming::Vp9;
                let (param_blob, nal_len_size) = if is_av1 || is_vp9 {
                    (Vec::new(), 0)
                } else {
                    parse_extradata(extradata.as_ptr(), extradata.len(), is_hevc)
                };
                let av1_cfg = if is_av1 { parse_av1c(extradata) } else { Vec::new() };
                let av1_framing = if is_av1 {
                    Av1Framing::from_trigger(plx_base::devtrig::read("av1obu").as_deref())
                } else {
                    Av1Framing::DEFAULT
                };
                #[cfg(feature = "devtriggers")]
                {
                    if is_av1 && av1_framing != Av1Framing::DEFAULT {
                        session.media().record(&format!(
                            "ff: av1obu trigger: td={} seq={} raw={}",
                            av1_framing.td, av1_framing.seq, av1_framing.raw
                        ));
                    }
                }
                // A VP9 stream's PQ is in the container only; hand it to the pump, which writes it into
                // the sourceInfo envelope ACB is sent. Always replaced, so a previous stream's colour
                // cannot outlive it.
                session.media().publish(DemuxEffect::ContainerHdr(
                    if is_vp9 { Some(container_hdr(vcp)) } else { None },
                ));
                session.media().record(&format!(
                    "ff: param_sets={} bytes nal_len={} is_hevc={} is_av1={} is_vp9={} av1_cfg={}B nal_input={:?}",
                    param_blob.len(),
                    nal_len_size,
                    is_hevc,
                    is_av1,
                    is_vp9,
                    av1_cfg.len(),
                    nal_input
                ));
                let mut aubuf: Vec<u8> = Vec::with_capacity(4 * 1024 * 1024);
                // A start-code stream's own parameter sets, cached from the units that carry them so
                // a keyframe without its own can take them (`annexb_h264_access_unit`).
                let mut annexb_params = H264ParamSets::default();
                // Such a stream can open mid-GOP (a capture cut where it was): pictures that need
                // parameter sets the pipeline has not been given are held back until the first
                // keyframe, here and after every seek, where the file is not a length-prefixed one
                // whose first packet is a keyframe by construction. `Some(n)`: still waiting, `n`
                // pictures held back so far.
                let mut annexb_wait_key = (nal_input == NalInput::StartCodes).then_some(0u32);
                let mut annexb_rejected = 0u32;

                let pkt = av_packet_alloc();
                if pkt.is_null() {
                    session.media().record("ff: packet_alloc failed");
                    avformat_close_input(&mut fmt);
                    free_avio(avio);
                    break;
                }

                let mut anchor_taken = false;
                // The video lane's pre-audio headroom (`PreAudioHeadroom`): granted at the first
                // video push that WOULD PARK at the base cap with no audio yet, held until the
                // engine's audio lead-in has been pushed, then released as the consumer drains.
                // Restarted (and the lane's grant withdrawn) by every seek and at loop exit.
                let mut headroom = PreAudioHeadroom::default();
                session.media().publish(DemuxEffect::FirstAudioPts(i64::MIN));
                // INNER read loop
                loop {
                    // Direct-play seek (and the armed resume, which is just a seek published before
                    // the first read): the pump leaves a target ns in FEED_SHARED.seek_to_ns and THIS
                    // thread — the only one that ever touches `fmt` — av_seek_frame's between two
                    // av_read_frame calls. A transcode leaves seek_to_ns=-1 (start.mkv is already
                    // 0-based at &offset) and so skips this.
                    //
                    // It used to be an INTERRUPT instead: the pump shutdown(2)'d the socket to break
                    // the read, and the outer loop reopened the URL and seeked the fresh context.
                    // That could not work, and the test suite hid it behind inherited resume offsets
                    // until 2026-07-28. Our AVIO is SEEKABLE (`seek_cb` reopens with a byte Range),
                    // so libavformat treats a read error as recoverable, calls seek_cb, and gets a
                    // brand-new connection at the same offset — av_read_frame never returned an
                    // error, the inner loop never broke, and no reopen ever happened. Every
                    // direct-play seek ran out the stuck-watchdog on PRE-seek packets
                    // (`rebase: dropping stale kf` at the old position) and escalated to a full
                    // reload. Seeking here needs no interrupt at all, so nothing can race it.
                    let seek_ns = session.clock().take_seek_ns().unwrap_or(-1);
                    if seek_ns >= 0 {
                        // These are content from the old seek epoch until the first new packets land.
                        // Reset both the buffer facts and the transfer-window hysteresis together so
                        // a seek cannot splice old reserve into a new low-rate observation.
                        session.media().publish(DemuxEffect::VideoTailNs(-1));
                        session.media().publish(DemuxEffect::AudioTailNs(-1));
                        let ticket_refreshed = original_watch.as_mut().is_some_and(|watch| {
                            watch.seek(state.body_bytes, state.body_active_us)
                        });
                        if ticket_refreshed {
                            // A PARTIAL reset, and the split is the point: the link did not change
                            // because the viewer jumped, so the delivery estimate survives — while the
                            // buffer, the deficit history and the byte counters all describe a position
                            // that no longer exists.
                        } else {
                            // The route/engine moved, not merely the playhead. This worker cannot
                            // adopt that ownership; teardown will replace it.
                            original_watch = None;
                        }
                        // Content time to the stream's own clock: a rebased stream's clock starts at
                        // `origin_ns`, so a seek to 0 lands on its first packet, not before it.
                        let ts = av_rescale_q(seek_ns.saturating_add(origin_ns), NS_TB, stream_time_base(vst));
                        // Fence completed renders from the old position before new packets arrive.
                        session.subtitle().seek(ass_generation);
                        session.media().record(&format!("ff: seek begin {}s", seek_ns / 1_000_000_000));
                        let seek_began = std::time::Instant::now();
                        let sr = av_seek_frame(fmt, vi, ts, AVSEEK_FLAG_BACKWARD);
                        session.media().record(&format!(
                            "ff: seek {}s rv={sr} ms={}",
                            seek_ns / 1_000_000_000,
                            seek_began.elapsed().as_millis()
                        ));
                        headroom.restart(aq_p);
                        annexb_wait_key = (nal_input == NalInput::StartCodes).then_some(0u32);
                        if let Some(dec) = pcm_dec.as_mut() {
                            dec.reset_stream();
                        }
                        if let Some(batch) = pcm_batch.as_mut() {
                            batch.clear();
                        }
                        session.media().publish(DemuxEffect::FirstAudioPts(i64::MIN));
                    }
                    let r = av_read_frame(fmt, pkt);
                    if frame_read_failed(&mut state, r) {
                        // Genuine end of stream, teardown, or an unrecovered I/O error published by
                        // `frame_read_failed`. NOT an app seek — that is serviced at the top of this
                        // loop and never surfaces as a read error.
                        if let Some((au_pts, au)) = pcm_batch.as_mut().and_then(|b| b.take()) {
                            crate::aq::aq_push_gated(
                                aqa_p, au.as_ptr(), au.len() as c_int, au_pts, 1, 2, audio_push_gate(aq_p, headroom.video_reach_ns()),
                                || state.drain_wire(),
                            );
                        }
                        break;
                    }
                    let si = (*pkt).stream_index;
                    // The array is read LIVE, with its bound: a transport stream can grow new streams
                    // while it is read (`AVFMTCTX_NOHEADER`), which reallocates `streams`, so the
                    // pointer taken at open is not the one the live `nb_streams` describes.
                    if origin_ns != 0 {
                        let live = (*fmt).streams;
                        if si >= 0 && (si as c_uint) < (*fmt).nb_streams && !live.is_null() {
                            rebase_packet(pkt, *live.add(si as usize), origin_ns);
                        }
                    }
                    if si == vi {
                        // The side subtitle reader's clock anchor: the first keyframe this run reads,
                        // fingerprinted BEFORE the Annex-B conversion rewrites it.
                        if !anchor_taken && (*pkt).flags & AV_PKT_FLAG_KEY != 0 && !(*pkt).data.is_null() {
                            anchor_taken = true;
                            let raw = std::slice::from_raw_parts((*pkt).data, (*pkt).size.max(0) as usize);
                            session.media().publish(DemuxEffect::SideAnchor(Some(
                                subside::Anchor::of(pts_ns(pkt, vst), raw),
                            )));
                        }
                        let pkt_bytes: &[u8] = if (*pkt).data.is_null() {
                            &[]
                        } else {
                            std::slice::from_raw_parts((*pkt).data, (*pkt).size.max(0) as usize)
                        };
                        // What kind of keyframe this packet is, if one: only a length-prefixed HEVC packet
                        // can be a non-IDR IRAP ([`Keyframe`]); every other framing answers a bool.
                        let kf = if is_av1 {
                            Keyframe::of(av1_packet_to_tu(
                                pkt_bytes,
                                (*pkt).flags & AV_PKT_FLAG_KEY != 0,
                                &av1_cfg,
                                av1_framing,
                                &mut aubuf,
                            ))
                        } else if is_vp9 {
                            Keyframe::of(vp9_packet_to_frame(pkt_bytes, (*pkt).flags & AV_PKT_FLAG_KEY != 0, &mut aubuf))
                        } else if nal_input == NalInput::StartCodes {
                            let rap = Rap::IdrOrIntra { container_key: (*pkt).flags & AV_PKT_FLAG_KEY != 0 };
                            Keyframe::of(match annexb_h264_access_unit(pkt_bytes, rap, &mut annexb_params, &mut aubuf) {
                                Ok(key) => key,
                                Err(why) => {
                                    // An empty unit is the signal below: nothing is pushed for it.
                                    aubuf.clear();
                                    annexb_rejected += 1;
                                    if annexb_rejected <= 3 {
                                        session.media().record(&format!(
                                            "ff: dropped a start-code video packet ({} bytes): {why}",
                                            pkt_bytes.len()
                                        ));
                                    }
                                    false
                                }
                            })
                        } else {
                            packet_to_annexb(
                                (*pkt).data,
                                (*pkt).size.max(0) as usize,
                                nal_len_size,
                                is_hevc,
                                dv_p7,
                                (*pkt).flags & AV_PKT_FLAG_KEY != 0,
                                &param_blob,
                                &mut aubuf,
                            )
                        };
                        let is_key = kf.is_key();
                        let pts = pts_ns(pkt, vst);
                        if nal_input == NalInput::StartCodes {
                            // Neither a packet the framing refused (`aubuf` is empty) nor a picture
                            // ahead of the first keyframe reaches the lane. A skipped packet also
                            // skips this iteration's Auto Original bookkeeping below, which the next
                            // packet runs; the run is bounded by a GOP, or by the audio lane's ceiling
                            // (`NoUsableVideo`) for a stream with no keyframe at all.
                            if aubuf.is_empty() {
                                av_packet_unref(pkt);
                                continue;
                            }
                            if let Some(held_back) = annexb_wait_key.as_mut() {
                                if !is_key {
                                    *held_back += 1;
                                    av_packet_unref(pkt);
                                    continue;
                                }
                                if *held_back > 0 {
                                    session.media().record(&format!(
                                        "ff: start-code stream: first keyframe after {held_back} video packets that needed parameter sets not yet seen"
                                    ));
                                }
                                annexb_wait_key = None;
                            }
                        }
                        if DIAG_FIRST.swap(false, Ordering::Relaxed) {
                            let n = aubuf.len().min(40);
                            let head: String = aubuf[..n]
                                .iter()
                                .map(|b| format!("{:02x}", b))
                                .collect::<Vec<_>>()
                                .join(" ");
                            session.media().record(&format!(
                                "ff: AU#0 size={} key={} head=[{}]",
                                aubuf.len(),
                                kf.label(),
                                head
                            ));
                        }
                        match headroom.before_video(
                            ai >= 0 && FEED_AUDIO.load(Ordering::Relaxed),
                            crate::aq::aq_would_park(aq_p),
                            crate::aq::aq_pops(aq_p),
                        ) {
                            VideoStep::Proceed => {}
                            VideoStep::Grant(extra) => crate::aq::aq_set_headroom(aq_p, extra),
                            VideoStep::Release => {
                                // The lane is full under the grant but the consumer is draining it
                                // (a seek into an audioless stretch): not the hang the grant is
                                // bounded for. Park like any full lane, as before the grant.
                                crate::aq::aq_release_headroom(aq_p);
                                session.media().record(&format!(
                                    "ff: video headroom released with {} bytes queued: the lane is draining and no audio has come",
                                    crate::aq::aq_bytes(aq_p)
                                ));
                            }
                            VideoStep::Fail => {
                                // The grant is spent, no audio AU has been read, and nothing has
                                // been popped since it was given: the consumer cannot drain video
                                // before its clock starts (which needs audio), so parking here is
                                // a spinner nobody can leave. Fail the open instead, the way an
                                // unfeedable audio track does, so the route recovers (Auto
                                // Original -> HLS) or the failure read-out shows.
                                session.media().record(&format!(
                                    "ff: no audio AU after {} bytes of video queued (cap {} = base + the {} byte pre-audio headroom); failing the open",
                                    crate::aq::aq_bytes(aq_p),
                                    crate::aq::aq_cap(aq_p),
                                    PRE_AUDIO_VIDEO_HEADROOM
                                ));
                                session.media().publish(DemuxEffect::FailFault(DemuxFault::NoAudioAu));
                                session.media().publish(DemuxEffect::IoFailed);
                                av_packet_unref(pkt);
                                break;
                            }
                        }
                        let pushed = crate::aq::aq_push_with_drain(
                            aq_p,
                            aubuf.as_ptr(),
                            aubuf.len() as c_int,
                            pts,
                            kf.au_key(),
                            1,
                            || state.drain_wire(),
                        );
                        av_packet_unref(pkt);
                        if pushed != 0 {
                            break;
                        }
                        PUSHED_ANY.store(true, Ordering::Relaxed);
                        session.media().publish(DemuxEffect::VideoTailNs(pts));
                        headroom.video_pushed(pts);
                    } else if si == ai && FEED_AUDIO.load(Ordering::Relaxed) {
                        let ast = *streams.add(ai as usize);
                        let pts = pts_ns(pkt, ast);
                        if headroom.first_audio_ns.is_none() {
                            // Before the push, so the feed thread that pops this AU sees it: the
                            // audio lane feeds a late-starting track's first AU on this mark.
                            session.media().publish(DemuxEffect::FirstAudioPts(pts));
                        }
                        // The audio lane runs past its soft cap while the video lane is starved
                        // ([`AUDIO_AHEAD_HEADROOM`]); the gate is what makes that decision, here.
                        let gate = audio_push_gate(aq_p, headroom.video_reach_ns());
                        let pushed = if let Some(dec) = speed_dec.as_mut() {
                            let mut rv = 0;
                            for (au_pts, pcm) in dec.decode(pkt, pts) {
                                rv = crate::aq::aq_push_gated(
                                    aqa_p, pcm.as_ptr(), pcm.len() as c_int, au_pts, 1, 2, gate,
                                    || state.drain_wire(),
                                );
                                if rv != 0 {
                                    break;
                                }
                            }
                            rv
                        } else if aac_adts.is_some() {
                            // one 7-byte ADTS header so LG's decoder can frame the AAC: added to the raw
                            // frames of Matroska/MP4, not stacked on a transport stream's own (#1084)
                            let raw = std::slice::from_raw_parts((*pkt).data, (*pkt).size.max(0) as usize);
                            let framed = adts_framed(raw, aac_adts).unwrap_or(std::borrow::Cow::Borrowed(raw));
                            crate::aq::aq_push_gated(
                                aqa_p,
                                framed.as_ptr(),
                                framed.len() as c_int,
                                pts,
                                1,
                                2,
                                gate,
                                || state.drain_wire(),
                            )
                        } else if let Some(dec) = pcm_dec.as_mut() {
                            let (pcm_pts, pcm) = match dec.decode(pkt, pts) {
                                Ok(decoded) => decoded,
                                Err(why) => {
                                    session.media().record(&format!(
                                        "ff: {} decode failed — {why}; refusing audio feed",
                                        app_decoded.as_deref().unwrap_or("audio")
                                    ));
                                    session.media().publish(DemuxEffect::Failed);
                                    session.media().publish(DemuxEffect::IoFailed);
                                    av_packet_unref(pkt);
                                    break;
                                }
                            };
                            match pcm_batch.as_mut().and_then(|b| b.push(pcm_pts, pcm)) {
                                None => 0,
                                Some((au_pts, au)) => crate::aq::aq_push_gated(
                                    aqa_p, au.as_ptr(), au.len() as c_int, au_pts, 1, 2, gate,
                                    || state.drain_wire(),
                                ),
                            }
                        } else if dts_audio {
                            let raw = std::slice::from_raw_parts((*pkt).data, (*pkt).size.max(0) as usize);
                            let Some(core) = dts_core_packet(raw) else {
                                session.media().record("ff: DTS packet has no complete supported core; refusing audio feed");
                                session.media().publish(DemuxEffect::FailFault(DemuxFault::DtsCoreMissing));
                                session.media().publish(DemuxEffect::IoFailed);
                                av_packet_unref(pkt);
                                break;
                            };
                            crate::aq::aq_push_gated(
                                aqa_p, core.as_ptr(), core.len() as c_int, pts, 1, 2, gate,
                                || state.drain_wire(),
                            )
                        } else {
                            crate::aq::aq_push_gated(
                                aqa_p,
                                (*pkt).data,
                                (*pkt).size,
                                pts,
                                1,
                                2,
                                gate,
                                || state.drain_wire(),
                            )
                            // AUDIO lane
                        };
                        av_packet_unref(pkt);
                        if pushed == crate::aq::PUSH_CEILING {
                            // The audio lane is at its ceiling, the video lane is starved and the
                            // video read so far cannot prime the clock: parking would be a spinner
                            // nobody can leave. Fail the way the video side does, so the route
                            // recovers (Auto Original -> HLS) or the failure read-out shows.
                            session.media().record(&format!(
                                "ff: no usable video after {} bytes of audio queued (ceiling {} = base + the {} byte audio headroom; {} ms of video read); failing the stream",
                                crate::aq::aq_bytes(aqa_p),
                                crate::aq::aq_cap(aqa_p) + AUDIO_AHEAD_HEADROOM,
                                AUDIO_AHEAD_HEADROOM,
                                headroom.video_reach_ns() / 1_000_000
                            ));
                            session.media().publish(DemuxEffect::FailFault(DemuxFault::NoUsableVideo));
                            session.media().publish(DemuxEffect::IoFailed);
                            break;
                        }
                        if pushed != 0 {
                            break;
                        }
                        let step = headroom.after_audio(pts);
                        if step.first && headroom.granted {
                            // The late audio arrived: say how far ahead video had to run.
                            session.media().record(&format!(
                                "ff: first audio at {}ms after {} bytes of video queued ahead of it",
                                pts / 1_000_000,
                                crate::aq::aq_bytes(aq_p)
                            ));
                        }
                        if step.release {
                            // The engine's lead-in is on the audio lane: from here the video lane
                            // falls back to its base cap as the consumer drains it, never under what
                            // it holds (see `PRE_AUDIO_VIDEO_HEADROOM`).
                            crate::aq::aq_release_headroom(aq_p);
                            session.media().record(&format!(
                                "ff: audio lead-in pushed ({}ms past the first); video headroom released with {} bytes queued",
                                (pts - headroom.first_audio_ns.unwrap_or(pts)) / 1_000_000,
                                crate::aq::aq_bytes(aq_p)
                            ));
                        }
                        session.media().publish(DemuxEffect::AudioTailNs(pts));
                    } else if let Some(sub_pos) = sub_tracks.position(si) {
                        // Subtitle packet: `SubTracks::decode` yields a cue for EVERY text track
                        // (not just the selected one) and decodes every image track while subtitles
                        // are on; see ff_subs.rs. Subtitles carry no ES -> never fed to the pipeline.
                        let decode_image = session.subtitle().selected_track() >= 0;
                        let bitmap_budget = session.subtitle().bitmap_budget();
                        if let Some(cue) = sub_tracks.decode(sub_pos, pkt, *streams.add(si as usize), decode_image, bitmap_budget) {
                            session.subtitle().emit(ass_generation, cue);
                        }
                        av_packet_unref(pkt);
                    } else {
                        av_packet_unref(pkt);
                    }
                    if !session.clock().seeking() {
                        if let Some(watch) = original_watch.as_mut() {
                            let pause_state = session.clock_mut().pause_event(PauseLane::Original);
                            let completed_pause_ms = pause_state.completed_ms;
                            let (buffered_ms, remaining_ms) = if pause_state.paused {
                                (None, None)
                            } else {
                                let audio_expected = ai >= 0 && FEED_AUDIO.load(Ordering::Relaxed);
                                (progressive_buffered_ms(session.clock(), audio_expected),
                                 session.clock().remaining_playback_sampler()())
                            };
                            let quantum = plx_playback_contract::adaptive::OriginalQuantum {
                                bytes: state.body_bytes,
                                active_us: state.body_active_us,
                                buffered_ms,
                                remaining_ms,
                                paused: pause_state.paused,
                                completed_pause_ms,
                            };
                            let position_ns = session.clock().playpos_sampler();
                            match watch.observe(quantum, &|| position_ns()) {
                                plx_playback_contract::adaptive::OriginalProgress::Continue => {}
                                plx_playback_contract::adaptive::OriginalProgress::Published => break,
                                plx_playback_contract::adaptive::OriginalProgress::Retired => {
                                    original_watch = None;
                                }
                            }
                        }
                    }
                    if crate::aq::aq_is_aborted(aq_p) {
                        break;
                    }
                }

                // Whatever way the read loop ended, the video lane does not keep a standing grant.
                headroom.withdraw(aq_p);

                // cleanup this stream (we own pb, so close_input won't free the AVIO)
                sub_tracks.close(); // reopened fresh on the next outer pass
                let mut pkt_m = pkt;
                av_packet_free(&mut pkt_m);
                avformat_close_input(&mut fmt);
                free_avio(avio);
                let _ = &state; // keep the AvioState alive until after free_avio
            }

            if unsafe { crate::aq::aq_is_aborted(aq_p) } {
                break;
            }
            break;
        }
    })); // end panic barrier. The body above is deliberately NOT re-indented into the closure: a
    // ~340-line whitespace diff would bury every real change this file ever gets again.

    // The panic path joins the normal one HERE, so both leave the consumer in the same state.
    if let Err(e) = &outcome {
        // The payload is almost always a `&str`/`String` (a slice index, an `unwrap`), and the
        // message is worth far more than the bare fact of a panic: this thread walks packet buffers
        // whose sizes come off the wire, so knowing WHICH one gave way is most of the triage. The
        // C tracer in `main.c` cannot help here — nothing faults, so there is no PC to symbolize.
        let what = e
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| e.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string payload>".to_string());
        session.media().record(&format!("ff: demux PANICKED — {what}"));
        // The producer is gone before it could publish a duration or a single frame, which is the
        // one case EOF alone cannot resolve: `engine.rs` pushes EOS only once the pipeline has
        // reached `Stage::Streaming`, so a panic during open/find_stream_info would otherwise leave
        // the pump waiting on a stage it can never reach. This is the same flag the `http_open`
        // failure above raises, and `pump` turns it into `PlaybackState::Error` (gated on
        // `frames == 0`, so a panic MID-playback is left to the EOF/EOS path below instead).
        session.media().publish(DemuxEffect::FailFault(DemuxFault::DemuxPanicked));
    }
    // A demux that ends having produced NOTHING is a failure, whatever route it took out.
    // Only two exits used to say so — the http_open failure and the panic handler — while every
    // other early `break` in the open sequence (the ABI refusal, av_malloc, avio_alloc_context,
    // avformat_alloc_context, open_input, find_stream_info, no video stream) simply set EOF and
    // returned. EOF with zero AUs is indistinguishable from a zero-length file: the pump keeps
    // waiting, `frames` stays 0, and the HUD says "Buffering…" forever with nothing in the log to
    // say why. That is precisely the report that came back from webOS 6 and 10, and it is why the
    // report could not name a cause. Failing loudly here does not fix any of those bugs; it makes
    // the next one diagnosable. An exit teardown asked for is not one of them: see
    // `unproductive_exit_failed`.
    if unproductive_exit_failed(PUSHED_ANY.load(Ordering::Relaxed), unsafe {
        crate::aq::aq_is_aborted(aq_p)
    }) {
        session.media().record("ff: demux produced no access units — treating as a failure");
        session.media().publish(DemuxEffect::FailFault(DemuxFault::NoAccessUnits));
    }
    crate::aq::aq_set_eof(aq_p);
    crate::aq::aq_set_eof(aqa_p); // EOF on the audio lane too
    session.media().record("ff: demux ended");
}

#[cfg(any(test, feature = "test-support"))]
#[path = "ff_fixture_bytes.rs"]
pub mod test_support;
#[cfg(test)]
#[path = "ff_aac_timestamp_tests.rs"]
mod aac_timestamp_tests;
#[cfg(test)]
#[path = "ff_packet_framing_tests.rs"]
mod packet_framing_tests;
