//! The pipeline's event callback, `sf_on_event`: the one entry LG's library thread (and the
//! simulator's clock sink, [`crate::ffi_host`]) calls with `(epoch, type, num, text)`.
//!
//! It runs on the LIBRARY thread, is `#[no_mangle]` because `src/starfish.c` calls it by name, has no
//! user-data pointer (the firmware hands back only the epoch the `Load` carried), and touches ONLY
//! [`FEED_SHARED`]. Everything it publishes is read back by the main thread through that mailbox.
//! The one thing it needs from the player's common state, which this crate may not name, arrives as
//! an [`SfEventHooks`] value the composition installs once ([`install`]): a callback with no
//! user-data pointer cannot be handed a reference, so a process-lifetime value is the only way in.

use crate::shared::{self, FEED_SHARED};
use plx_base::eventlog::log;
use plx_playback_contract::RATE_NORMAL;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uint};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Once, OnceLock};

/// What [`sf_on_event`] reads or writes in the player's common state, which this crate may not name.
pub struct SfEventHooks {
    /// A picture arrived: if a viewer's audio pick had a rebuild in flight, it has put its picture
    /// up and the switch is over (the common player's `AudioSwitch::Reloading` -> `None`).
    pub audio_rebuild_presented: fn(),
}

static HOOKS: OnceLock<&'static SfEventHooks> = OnceLock::new();
static HOOK_ABSENT: Once = Once::new();

/// Install the composition's hooks, once, before the first `Load`. A second call is refused and
/// changes nothing. Without one the callback runs, says so once in the event log, and leaves the
/// common player's audio-switch state alone.
pub fn install(hooks: &'static SfEventHooks) -> Result<(), &'static SfEventHooks> {
    HOOKS.set(hooks)
}

/// `type=18`'s `num` when the pipeline releases the video decoder from an established session
/// (`"VDEC resource is released by policy action"`). See `Shared::decoder_revocations`.
const DECODER_REVOKED_BY_POLICY: i64 = 600;

/// `type=18`'s `num` for `"Audio Codec Not Supported"`: libpf's `handleBusWarningMessage` raises it
/// when a gst stream warning (`GST_STREAM_ERROR_CODEC_NOT_FOUND`) names audio. Every native Vorbis
/// Load on a C1 ended in it (2026-10-21).
const AUDIO_CODEC_NOT_SUPPORTED: i64 = 200;

/// `PF_EVENT_TYPE_STR_STATE_UPDATE__ENDOFSTREAM` (0x1c): the last numbered type before the
/// renumbering between webOS 4 and 5+, so the same on both. See `Shared::pipeline_eos`.
const PIPELINE_END_OF_STREAM: c_int = 28;

fn find(h: &[u8], n: &[u8]) -> bool {
    !n.is_empty() && h.windows(n.len()).any(|w| w == n)
}
/// bytes between `prefix` and the next `term`, or None if `prefix` absent.
fn between(h: &[u8], prefix: &[u8], term: u8) -> Option<Vec<u8>> {
    let start = h.windows(prefix.len()).position(|w| w == prefix)? + prefix.len();
    let rest = &h[start..];
    let end = rest.iter().position(|&b| b == term).unwrap_or(rest.len());
    Some(rest[..end].to_vec())
}

/// Parse one JSON number into thousandths without allocating a JSON tree on the pipeline callback
/// thread. SourceInfo is firmware-owned and may contain integer (`24`) or fractional (`23.976`)
/// frame rates; malformed, zero and non-finite values remain "not reported".
fn source_fps_milli(h: &[u8]) -> Option<i64> {
    let prefix = b"\"frameRate\":";
    let start = h.windows(prefix.len()).position(|w| w == prefix)? + prefix.len();
    let rest = &h[start..];
    let first = rest.iter().position(|b| !b.is_ascii_whitespace())?;
    let number = &rest[first..];
    let end = number
        .iter()
        .position(|b| !b.is_ascii_digit() && !matches!(*b, b'.' | b'-' | b'+'))
        .unwrap_or(number.len());
    let value = std::str::from_utf8(&number[..end])
        .ok()?
        .parse::<f64>()
        .ok()?;
    let milli = value * 1_000.0;
    (value.is_finite() && value > 0.0 && milli <= i64::MAX as f64).then(|| milli.round() as i64)
}

/// The picture's display aspect as a reduced `(numerator, denominator)`: the sourceInfo video object's
/// raster times its pixel aspect ratio. Parses only the `video` object, never a similarly named nested
/// field. Observed on the TV as `video:{width,height,pixelAspectRatio:{width,height}}`. This runs only on
/// metadata callbacks, not on the frame/position callback path.
pub fn source_aspect(bytes: &[u8]) -> Option<(u32, u32)> {
    let root: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let video = root.get("video")?;
    let positive = |v: &serde_json::Value| v.as_u64().filter(|&n| n > 0 && n <= 65_536);
    let w = positive(video.get("width")?)?;
    let h = positive(video.get("height")?)?;
    let (pw, ph) = match video.get("pixelAspectRatio") {
        Some(par) => (positive(par.get("width")?)?, positive(par.get("height")?)?),
        None => (1, 1),
    };
    let (num, den) = (w * pw, h * ph);
    let (mut a, mut b) = (num, den);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    Some((u32::try_from(num / a).ok()?, u32::try_from(den / a).ok()?))
}

/// What the pipeline said it set up for a speed session's audio, from its audio-info callback
/// (`audio/x-raw … <kHz> <channels> …`). 48 kHz stereo is what the payload declared; anything
/// else means the declaration was not understood — measured: `"sampleRate":48000` came back as
/// 44.099998 and the audio drifted and resynced every few seconds.
fn note_speed_audio_report(b: &[u8]) {
    if FEED_SHARED.applied_rate_permille.load(Relaxed) == RATE_NORMAL || !b.starts_with(b"audio/") {
        return;
    }
    let text = String::from_utf8_lossy(b);
    let mut f = text.split_whitespace();
    let kind = f.next().unwrap_or("");
    let khz: f64 = f.nth(2).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let channels: i32 = f.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let ok = kind == "audio/x-raw" && (khz - 48.0).abs() < 0.01 && channels == 2;
    FEED_SHARED.speed_audio_verdict.store(if ok { shared::SPEED_AUDIO_OK } else { shared::SPEED_AUDIO_BAD }, Relaxed);
    log(&format!("speed: pipeline audio {kind} {khz} kHz x{channels} -> {}", if ok { "ok" } else { "NOT what was declared" }));
}

/// pipeline event on the LIBRARY thread. type 0 = `PF_EVENT_TYPE_FRAMEREADY` (num = fed pts).
///
/// **`FRAMEREADY` is NOT one callback per decoded frame on this firmware, and reading it that way
/// is how a stutter investigation gets the wrong answer.** Kodi's Starfish path treats it as one
/// picture per event, which is where the old "frame presented" gloss here came from. Measured on
/// webOS 4.10.2 (2026-08-21), it is a **~5 Hz position tick**: a 1080p H264 direct play, a 4K HEVC
/// direct play and a visibly stuttering Dolby Vision direct play all deliver it 5 times a second,
/// 201 ms apart, to the millisecond. So `frames` counts
/// TICKS, not frames — which is what `pump`'s ACB bind gate (`frames >= 2`, one tick while a
/// paused preroll is open) really means (≈400 ms of playback, not two pictures) and what `app::diagnostics` really shows.
///
/// The consequence for diagnosis: this callback can say the pipeline still believes it is
/// presenting, and cannot say the picture is smooth. The video plane's real cadence is not
/// observable from this process at all — the evidence for that lives in the TV's own kernel log
/// (`kad-hdr`).
/// Panic-guarded (unwinding into C is UB); touches only FEED_SHARED.
#[no_mangle]
pub extern "C" fn sf_on_event(epoch: c_uint, ty: c_int, num: i64, s: *const c_char) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // The mutex is deliberately held through the complete callback. A bare equality check
        // would let teardown retire A, reset the process-long FEED_SHARED storage for B, and then let
        // a callback which had already validated A publish into B. Firmware supplies `epoch`
        // through the device-proven callback-context overload of StarfishMediaAPIs::Load.
        let class = match ty {
            0 => shared::NativeEventClass::Presentation,
            23 => shared::NativeEventClass::UnloadCompleted,
            _ => shared::NativeEventClass::Other,
        };
        FEED_SHARED.with_native_session_scoped(epoch, class, num, |scope| sf_on_event_inner(ty, num, s, scope));
    }));
}
/// The two libpf frame counters a callback type can name, once the webOS 4 vs 5+ numbering
/// shift is taken out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkCounter {
    Dropped,
    Displayed,
}

/// The inverse of the rebase `sf_on_event_inner` applies to a reported position (`playpos = fed -
/// pts_shift + disp_base`): the fed PTS at which the app reads `media_ns`. The simulator's sink asks
/// it from its own clock thread, which holds no session, so it reads the feed's two members here,
/// beside the forward rebase, instead of the player carrying copies.
pub fn media_to_fed(media_ns: i64) -> i64 {
    media_ns
        .saturating_add(FEED_SHARED.pts_shift.load(Relaxed))
        .saturating_sub(FEED_SHARED.disp_base.load(Relaxed))
}

/// PURE: which sink counter a raw callback type is, on a firmware of this major. Decompiled from
/// libpf on webOS 4.10 (`CustomPipeline::updateDroppedFrame` → 0x2e = 46,
/// `updateDisplayedFrame` → 0x2f = 47); every type above 0x1c is two higher on webOS 5+
/// (Kodi's `if (webOSVersion < 5 && type > ENDOFSTREAM) type += 2`), so 48/49 there. An unknown
/// major (0: neither `os_info.json` nor `/usr/lib/os-release` named a release) is read as the
/// numbering this project has actually measured.
pub fn sink_counter_kind(ty: c_int, major: u32) -> Option<SinkCounter> {
    let normalised = if major == 0 || major < 5 { ty + 2 } else { ty };
    match normalised {
        48 => Some(SinkCounter::Dropped),
        49 => Some(SinkCounter::Displayed),
        _ => None,
    }
}

/// The raw callback type [`sink_counter_kind`] reads as the displayed-frame counter on this major,
/// found through it so the two cannot drift apart.
pub fn displayed_counter_type(major: u32) -> c_int {
    (46..=49)
        .find(|&ty| sink_counter_kind(ty, major) == Some(SinkCounter::Displayed))
        .unwrap_or(0)
}

/// Account one callback type 46 to 49 against `shared`, reading the numbering shift through
/// `major` (the callback thread passes the device's own). A displayed-frame counter with a
/// positive `num` is the only presentation instrument measured on the device (`presented: 24.1
/// fps` in `docs/measurements/`); `FRAMEREADY` (type 0) is a 5 Hz position tick and never counts.
pub(crate) fn note_sink_counter(shared: &shared::Shared, ty: c_int, num: i64, major: u32) {
    match sink_counter_kind(ty, major) {
        Some(SinkCounter::Displayed) => {
            shared.dg_sink_displayed_cb.fetch_add(1, Relaxed);
            shared.note_displayed_frames(num);
            log(&format!("sink: displayed={num} (type={ty})"));
        }
        Some(SinkCounter::Dropped) => log(&format!("sink: dropped={num} (type={ty})")),
        None if (46..=49).contains(&ty) => {
            shared.dg_sink_other_cb.fetch_add(1, Relaxed);
        }
        None => {}
    }
}

pub fn sf_on_event_inner(ty: c_int, num: i64, s: *const c_char, scope: shared::EventScope) {
    if scope == shared::EventScope::Release {
        // The lifecycle evidence of an object being released after the main thread reset the
        // session state (`UNLOADCOMPLETED` arrives ~110 ms into the off-frame Unload): log it, but
        // let nothing reach the census, error latch or load verdict the NEXT session owns.
        log(&format!("smp_cb type={ty} num={num} (release evidence, not counted)"));
        return;
    }
    if ty != 0 {
        // The diagnostics census, beside the log line that already records every event. A COUNT is
        // what the read-out needs: "Load completed and then nothing ever called us" is the sharpest
        // symptom the stuck-buffering reports could carry, and it is invisible in a log the user
        // cannot reach. Kept here rather than in the `ty` dispatch below so an event we do not
        // handle still counts — an unhandled callback is still the pipeline talking.
        let n = FEED_SHARED.dg_cb_count.fetch_add(1, Relaxed) + 1;
        // Latch the FIRST error, with the callback index it arrived at. Sticky, because a later
        // healthy callback must not erase the one event that explains the session — and the index
        // separates "refused immediately" from "died after a long healthy run". Only `ty == 18`:
        // it is the one ERROR type this project has ever acted on, and it sits below the 0x1c point
        // where the numbering shifts between webOS 4 and 5+, so it means the same on both. Naming
        // any higher type would be a confident lie on the firmware we cannot test.
        //
        // The firmware's own error NUMBER rides in the same latch (`601` = resource allocation),
        // clamped to the `i32` range: it is the one integer a handled `tv_pipeline` report carries
        // about the refusal. The callback's STRING (`s`, logged below) is never kept: it is free
        // text and can hold a URI or a path.
        if ty == 18 && FEED_SHARED.dg_cb_err.load(Relaxed) == 0 {
            FEED_SHARED.dg_cb_err.store(ty, Relaxed);
            FEED_SHARED.dg_cb_err_at.store(n, Relaxed);
            FEED_SHARED
                .dg_cb_err_num
                .store(num.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32, Relaxed);
        }
        let preview = if s.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(s) }
                .to_string_lossy()
                .chars()
                .take(1400)
                .collect()
        };
        log(&format!("smp_cb type={ty} num={num} str={preview}"));
        // The video sink's own frame counters, forwarded by libpf every 200 ms (see the payload
        // note in engine.rs: `streamQualityInfo` / `streamQualityInfoNonFlushable`). Logged under
        // a firmware-independent name because these two sit ABOVE the 0x1c point where the
        // callback numbering shifts by two between webOS 4 and 5+ (`docs/webos5-port.md` §5):
        // 46/47 on this set are 48/49 on a webOS 5+ set. The harness reads THIS line, never
        // the raw type.
        note_sink_counter(&FEED_SHARED, ty, num, plx_platform::tv::device::info().major);
        // **A refusal before the first picture is a VERDICT, not a statistic.** Until 2026-09-03
        // the latch above was all this arm did, and `docs/webos10-lab-report.md` §3.5 records the
        // result on a set that refused the Load asynchronously (`Load()` returned ok=1, then
        // `type=18 num=601 Resource Allocation Error`): `load_failed` stayed false, the pump waited
        // in Connecting for a `loadCompleted` that could never come, the demuxer kept downloading,
        // the adaptive controller stepped down against an estimator that would never see a frame,
        // and the failure read-out — the one screen built to survive a phone photograph — never
        // ran, for ~70 s, on the failure it most exists for. Publishing the same flag the
        // synchronous refusal publishes (`native_load::load_thread` on `sf_load == 0`) hands it to the
        // pump's existing `load_failed` arm, whose failed-open ladder (`recovery::OpenRecovery`)
        // picks: the HLS rollback if one is pending, else the conversion of a stream the attempt can
        // do without (the DTS of #695), else the cold Auto Original's HLS contingency, else `Error`
        // with `FailureKind::TvPipeline`.
        //
        // Scoped to `!seen_frame` — SESSION-scoped, never `frames == 0`, which a seek zeroes (see
        // player/CLAUDE.md, "`frames` is SEEK-scoped"). A `loadCompleted` already received is NOT
        // an exemption: a completed Load is the pipeline accepting a declaration, and a refusal
        // that lands after it but before any picture is still a session that never started. A
        // type=18 after a picture stays diagnostic-only here except for the one number that has
        // been measured (the decoder release, below): what the others mean mid-play is unmeasured,
        // and the lab could not say whether 18 is the only refusal type either — so their `num` is
        // logged rather than filtered on.
        if ty == 18 && !FEED_SHARED.seen_frame.load(Relaxed) {
            log(&format!(
                "smp: Load refused by the pipeline before any picture (type=18 num={num}) — failing the source"
            ));
            FEED_SHARED.note_refusal_path(shared::RefusalPath::Async);
            FEED_SHARED.load_failed.store(true, std::sync::atomic::Ordering::Release);
            if num == AUDIO_CODEC_NOT_SUPPORTED && crate::ff::native_audio::fed().is_some() {
                FEED_SHARED.raise_native_audio_refused();
            }
        } else if ty == 18 && num == DECODER_REVOKED_BY_POLICY {
            // **After a picture, ONE number is measured: the decoder taken back.** A paused direct
            // play left until the screensaver came on (dev set, 2026-10-13) got exactly this, with
            // no app-switch edge, and the pipeline refused every Play after it for good — so this
            // is not a statistic either. The app loop parks the session the way an app switch does
            // and rebuilds it on the viewer's next press (`take_decoder_revoked`). Every other
            // number after a picture stays diagnostic-only, as above.
            log("smp: the pipeline released the video decoder (type=18 num=600) — parking the session");
            FEED_SHARED.raise_decoder_revoked();
        }
    }
    // The pipeline's end-of-stream (`Shared::pipeline_eos`): only after a picture, where it can
    // mean the film was played out.
    if ty == PIPELINE_END_OF_STREAM
        && FEED_SHARED.seen_frame.load(Relaxed)
        && !FEED_SHARED.pipeline_eos.swap(true, Relaxed)
    {
        log("smp: the pipeline reached end-of-stream");
    }
    if ty == 0 {
        // a POSITION UPDATE — map fed pts -> real content position.
        //
        // **It is not one per presented frame, and `vtick`/`vgap` cannot see a dropped frame.**
        // Measured 2026-08-21 across every Profile 5 run: `vtick=5 vgap=201ms`, unvarying, on
        // clean and visibly stuttering playback alike. The pipeline emits this at 5 Hz — it is a
        // position report, not a vsync. This comment used to say "a frame was PRESENTED" and the
        // `dg_vpres_*` block below still describes a cadence probe; both were written from the
        // callback's NAME rather than from its rate, and an instrument that reads a flat 201 ms
        // through the fault it exists to catch is worse than no instrument, because it is quoted.
        //
        // The real per-frame cadence is only observable from LG's own tracing — `GST_DEBUG=
        // dualsequencer:6` via `/tmp/plxnative-gstlog`, whose `push_dual` and `lxvideosink`
        // timestamps give one line per frame. That was long avoided as perturbing; it is not, at
        // level 6: the same scene measured 123 LUT misses uninstrumented and 122 with the trace
        // running. Level 9 IS perturbing and is what that reputation came from.
        //
        // `pres_fed` below is still sound — the feed-ahead throttle wants a position, and a 200 ms
        // granularity against a 1.6 s budget is ample. Only the TIMING half was wrong.
        let t = shared::vclock_ms();
        let prev = FEED_SHARED.dg_vpres_at.swap(t, Relaxed);
        FEED_SHARED.dg_vpres_ct.fetch_add(1, Relaxed);
        if prev != 0 {
            let gap = t.saturating_sub(prev);
            FEED_SHARED.dg_vpres_gap_sum.fetch_add(gap, Relaxed);
            FEED_SHARED.dg_vpres_gap_ct.fetch_add(1, Relaxed);
        }
        FEED_SHARED.frames.fetch_add(1, Relaxed);
        FEED_SHARED.seen_frame.store(true, Relaxed); // session-scoped: unlike `frames`, a seek won't clear it
        // the rebuild an audio pick asked for has put its picture up: the switch is over
        match HOOKS.get() {
            Some(hooks) => (hooks.audio_rebuild_presented)(),
            None => HOOK_ABSENT.call_once(|| log("smp: no SfEventHooks installed - an audio pick's rebuild is never marked finished")),
        }
        // back from the pipeline's timeline to film time (`from_sink_ns`; identity at 1x)
        let num = crate::shared::from_sink_ns(num);
        FEED_SHARED.pres_fed.store(num, Relaxed); // raw fed pts, for the feed-ahead throttle
        let content_ns = num - FEED_SHARED.pts_shift.load(Relaxed) + FEED_SHARED.disp_base.load(Relaxed);
        FEED_SHARED.playpos_ns.store(content_ns, Relaxed);
        FEED_SHARED.reported_playpos_ns.store(content_ns, Relaxed);
        FEED_SHARED.dg_pos_ticks.fetch_add(1, std::sync::atomic::Ordering::Release);
    }
    if s.is_null() {
        return;
    }
    let b = unsafe { CStr::from_ptr(s) }.to_bytes();

    if let Some(fps_milli) = source_fps_milli(b) {
        FEED_SHARED.video_fps_milli.store(fps_milli, Relaxed);
    }
    note_speed_audio_report(b);
    if find(b, b"\"video\"") {
        if let Some((num, den)) = source_aspect(b) {
            FEED_SHARED.publish_video_aspect(num, den);
        }
    }

    {
        let mut mid = FEED_SHARED.media_id.lock().unwrap();
        if mid.is_none() {
            if let Some(id) =
                between(b, b"\"context\":\"", b'"').or_else(|| between(b, b"\"mediaId\":\"", b'"'))
            {
                if let Ok(c) = std::ffi::CString::new(id.clone()) {
                    log(&format!(
                        "SMP context/mediaId={}",
                        String::from_utf8_lossy(&id)
                    ));
                    *mid = Some(c);
                }
            }
        }
    }

    if !FEED_SHARED.load_completed.load(Relaxed) && (find(b, b"loadCompleted") || find(b, b"\"loaded\""))
    {
        FEED_SHARED.load_completed.store(true, Relaxed);
        log("SMP loadCompleted");
    }

    {
        // capture the WHOLE sourceInfo envelope VERBATIM (byte-for-byte + NUL), never re-encoded
        let mut si = FEED_SHARED.source_info.lock().unwrap();
        if si.is_none() && find(b, b"\"video\":") && find(b, b"\"context\":") {
            let mut v = Vec::with_capacity(b.len() + 1);
            v.extend_from_slice(b);
            v.push(0);
            log(&format!("SMP sourceInfoRaw captured ({} bytes)", b.len()));
            *si = Some(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::{SPEED_AUDIO_BAD, SPEED_AUDIO_OK, SPEED_AUDIO_UNKNOWN};

    #[test]
    fn source_info_reports_the_stream_fps_not_the_position_tick_rate() {
        assert_eq!(
            source_fps_milli(br#"{"video":{"frameRate":24,"width":3840}}"#),
            Some(24_000),
        );
        assert_eq!(
            source_fps_milli(br#"{"video":{"frameRate": 23.976,"width":1920}}"#),
            Some(23_976),
        );
        assert_eq!(source_fps_milli(br#"{"video":{"frameRate":0}}"#), None);
        assert_eq!(source_fps_milli(br#"{"video":{"width":1920}}"#), None);
    }


    #[test]
    fn the_sink_counters_are_read_through_the_numbering_shift() {
        use SinkCounter::{Displayed, Dropped};
        assert_eq!(sink_counter_kind(46, 4), Some(Dropped));
        assert_eq!(sink_counter_kind(47, 4), Some(Displayed));
        assert_eq!(sink_counter_kind(47, 0), Some(Displayed), "unknown major reads as measured");
        assert_eq!(sink_counter_kind(48, 5), Some(Dropped));
        assert_eq!(sink_counter_kind(49, 10), Some(Displayed));
        assert_eq!(sink_counter_kind(47, 10), None, "47 is something else on webOS 5+");
        assert_eq!(sink_counter_kind(49, 4), None);
        assert_eq!(sink_counter_kind(18, 4), None);
    }


    /// The safety net's evidence: the pipeline's audio-info report for a speed session. 48 kHz
    /// stereo raw is what the payload declared; the 44.1 a mis-declared rate produced, or any
    /// compressed type, is not. At 1x the report is not judged at all.
    #[test]
    fn the_pipeline_audio_report_judges_a_speed_session() {
        let _serial = plx_base::testlock::serial();
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                FEED_SHARED.applied_rate_permille.store(RATE_NORMAL, Relaxed);
                FEED_SHARED.speed_audio_verdict.store(SPEED_AUDIO_UNKNOWN, Relaxed);
            }
        }
        let _restore = Restore;
        let verdict = |rate: i32, report: &[u8]| {
            FEED_SHARED.applied_rate_permille.store(rate, Relaxed);
            FEED_SHARED.speed_audio_verdict.store(SPEED_AUDIO_UNKNOWN, Relaxed);
            note_speed_audio_report(report);
            FEED_SHARED.speed_audio_verdict.load(Relaxed)
        };
        // both are the 3.9.3 set's own strings, from its event log
        let good = b"audio/x-raw (null) (null) 48.000000 2 (null) 0 0 0 0 (null) 0 (null) 0 ";
        let drift = b"audio/x-raw (null) (null) 44.099998 2 (null) 0 0 0 0 (null) 0 (null) 0 ";
        assert_eq!(verdict(1500, good), SPEED_AUDIO_OK);
        assert_eq!(verdict(1500, drift), SPEED_AUDIO_BAD);
        assert_eq!(verdict(1500, b"audio/mpeg (null) (null) 0.000000 0 (null)"), SPEED_AUDIO_BAD);
        assert_eq!(verdict(RATE_NORMAL, drift), SPEED_AUDIO_UNKNOWN, "1x is never judged");
        assert_eq!(verdict(1500, b"{\"video\":1}"), SPEED_AUDIO_UNKNOWN, "not an audio report");
    }

    #[test]
    fn missing_or_invalid_geometry_does_not_publish_an_aspect() {
        for value in [
            br#"{}"#.as_slice(),
            br#"{"video":{"width":0,"height":1080}}"#,
            br#"{"video":{"width":1920,"height":1080,"pixelAspectRatio":{"width":0,"height":1}}}"#,
            br#"{"video":{"width":-1,"height":1080}}"#,
            br#"{"video":{"width":4294967295,"height":1080}}"#,
        ] {
            assert_eq!(source_aspect(value), None);
        }
        assert_eq!(source_aspect(br#"{"video":{"width":1440,"height":1080}}"#), Some((4, 3)));
        assert_eq!(
            source_aspect(br#"{"video":{"width":720,"height":576,"pixelAspectRatio":{"width":64,"height":45}}}"#),
            Some((16, 9))
        );
    }
}
