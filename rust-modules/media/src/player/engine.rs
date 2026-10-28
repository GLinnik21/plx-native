//! player::engine — the main-thread-confined session object (Engine) + lifecycle
//! (acb_init / start_bufferfeed / stop_bufferfeed) + the feed loops. No worker
//! thread ever names an Engine field: race-free by confinement (like the C
//! main-thread-only flags). The Engine owns the HttpStream box + the AuQueue
//! boxes; it hands raw ptrs to the workers and outlives them (drops after join).
//!
//! That confinement is **enforced, not just asserted**: the slot is `App.adapters.player`, a
//! [`crate::player::adapter::PlayerAdapter`] that consumes a [`MainThread`] at construction, so a
//! `&mut PlayerAdapter` borrow is what confines it now (see `player::adapter`). Which is also the
//! rule for the rest of this file — a function here takes the token **iff** it reaches the slot
//! or the ACB/Starfish seam, so the parameter carries information. `arm_seek` and `begin_resume`
//! run on the main thread too and deliberately do not take one: they only publish to `SHARED`.
use super::shared::{HlsPlayCompletion, HlsPrimeKind, Stage};
use super::SeekMode;
use super::{log, sink, threads, ACB_OK, PTYPE, SHARED, TX};
use crate::aq::{AuNode, AuQueue};
use plx_net::stream::HttpStream;
use plx_base::task::MainThread;
use plx_platform::devcaps::AudioStack;
use plx_platform::tv::sink::UnloadTicket;
use plx_telemetry::telemetry::classes::DemuxExitClass;
use plx_telemetry::telemetry::window::Stage as WindowStage;
use std::os::raw::{c_char, c_int, c_long, c_void};
use std::sync::atomic::{AtomicI64, Ordering};

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
const PAYLOAD_V: &str = r#"{"args":[{"mediaTransportType":"BUFFERSTREAM","option":{"appId":"@APPID@","externalStreamingInfo":{"contents":{"codec":{"video":"H264"},"esInfo":{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true},"format":"RAW","provider":"plxnative"},"streamQualityInfo":true,"streamQualityInfoNonFlushable":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{"minimum":1,"maximum":32768},"srcBufferLevelVideo":{"minimum":1,"maximum":8388608}}},"needAudio":false,"queryPosition":false,"lowDelayMode":true,"transmission":{"contentsType":"LIVE"},"adaptiveStreaming":{"audioOnly":false,"maxWidth":1920,"maxHeight":1080,"maxFrameRate":30}}}]}"#;
// NB: pauseAtDecodeTime stays FALSE here. Kodi uses true, but only alongside its decode-time
// trigger machinery (setTimeToDecode); with true and no trigger the decoder never starts
// (verified on-device: Load+Play OK but zero frames decoded). The feed-ahead throttle
// (MAX_FEED_AHEAD_NS in feed_stream) is the anti-stall mechanism; the other Kodi payload
// flags are being re-introduced one at a time.
const PAYLOAD_AV: &str = r#"{"args":[{"mediaTransportType":"BUFFERSTREAM","option":{"appId":"@APPID@","externalStreamingInfo":{"contents":{"codec":{"video":"H264","audio":"AC3"},"esInfo":{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true},"format":"RAW","provider":"plxnative"},"streamQualityInfo":true,"streamQualityInfoNonFlushable":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{"minimum":1,"maximum":1048576},"srcBufferLevelVideo":{"minimum":1,"maximum":8388608}}},"needAudio":true,"queryPosition":false,"lowDelayMode":false,"transmission":{"contentsType":"LIVE"},"adaptiveStreaming":{"audioOnly":false,"maxWidth":1920,"maxHeight":1080,"maxFrameRate":30}}}]}"#;
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
const PAYLOAD_H265: &str = r#"{"args":[{"mediaTransportType":"BUFFERSTREAM","option":{"appId":"@APPID@","externalStreamingInfo":{"contents":{"codec":{"video":"H265"},"esInfo":{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true},"format":"RAW","provider":"plxnative"},"streamQualityInfo":true,"streamQualityInfoNonFlushable":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{"minimum":1,"maximum":32768},"srcBufferLevelVideo":{"minimum":1,"maximum":8388608}}},"needAudio":false,"queryPosition":false,"lowDelayMode":true,"transmission":{"contentsType":"LIVE"},"adaptiveStreaming":{"audioOnly":false,"maxWidth":3840,"maxHeight":2160,"maxFrameRate":60}}}]}"#;

// ACCEPTED AUs — what the pipeline took. This is what `app::diagnostics` reports, because a count of
// attempts reads as healthy throughput through a stall: a full sink retains the AU and it is
// re-offered every tick.
static VTOT: AtomicI64 = AtomicI64::new(0);
static ATOT: AtomicI64 = AtomicI64::new(0);
// ATTEMPTS — the log's own cadence, kept separate because its `reply=` field is the only record
// of a REJECTED feed and `tests/run.py` greps `feed v#` / `feed a#`.
static VATT: AtomicI64 = AtomicI64::new(0);
static AATT: AtomicI64 = AtomicI64::new(0);

/// AUs fed to the Starfish pipeline this SESSION, video and audio.
///
/// Zeroed by `start_bufferfeed`, not cumulative since boot: the diagnostics read-out has to answer
/// "is this playback feeding?", and a number carried over from the last item answers a question
/// nobody asked. The log's `v <= 4 || v % 100 == 0` cadence restarting per session is the behaviour
/// that was wanted there too.
pub fn fed_totals() -> (i64, i64) {
    (VTOT.load(Ordering::Relaxed), ATOT.load(Ordering::Relaxed))
}

/// The AU queues' BASE byte caps — the denominators the read-out shows `dg_aq_*` against, so a viewer
/// can see backpressure (a video lane pinned at its cap is the demuxer outrunning the decoder;
/// both lanes empty while Stage is Playing is the opposite).
///
/// **Base, not the cap in force.** For the first seconds of a file whose audio starts so late that
/// the video lane fills to this cap before the first audio packet, the demuxer lifts the video
/// lane's cap by `ff::PRE_AUDIO_VIDEO_HEADROOM` (24 MiB) and then walks it back down as the
/// consumer drains, so the read-out can show a video lane ABOVE this number then. That is the
/// headroom doing its job, not an accounting error. An ordinary file never sees it: the grant is
/// taken only when a video push would park at this cap with no audio yet. The reachable-reserve
/// model (`abr::plant::b_max_est_ms`) wants exactly this steady-state number: the headroom is a
/// transient of the start of one kind of file, gone before the buffer-driven controller acts.
/// The audio lane's base cap is soft in the other direction: a file that stores more audio than
/// this ahead of its video (issue #600) runs the lane up to `ff::AUDIO_AHEAD_HEADROOM` (16 MiB)
/// beyond it for as long as the video lane holds less than `MAX_FEED_AHEAD_NS` of media.
pub const fn aq_caps() -> (i64, i64) {
    (AQ_VIDEO_BYTES as i64, AQ_AUDIO_BYTES as i64)
}

/// **The feed-ahead throttle's two leads, milliseconds** — video, then audio.
///
/// The other half of the reachable-reserve geometry. `B_max = lead + queue_bytes/rate` and
/// `aq_caps` above exposes only the `queue_bytes`; `abr/sim.rs` — the closed-loop plant the ABR
/// controller is graded against — carried these two BY VALUE with a comment, so half of `B_max`
/// could move here and nothing anywhere would fail.
///
/// That is not hypothetical. The same shape had already happened one level up: `sim.rs`'s
/// operating-point table was hand-transcribed, the fixture pack was rebuilt under it, and the
/// plant modelled a television that no longer existed at two of its three points for a month
/// without a single test going red. This closes the identical hole in the geometry.
pub const fn feed_leads_ms() -> (i64, i64) {
    (
        MAX_FEED_AHEAD_NS / 1_000_000,
        (MAX_FEED_AHEAD_NS + AUDIO_SLACK_NS) / 1_000_000,
    )
}

// Per-lane queue byte caps (two-lane feed). Audio is kept small (the TV is RAM-tight and audio
// frames are tiny) yet large enough to cushion the single demux thread briefly blocking on a full
// video lane.
//
// **The video cap is 10 MiB, and it stopped matching the Load payload's `srcBufferLevelVideo`
// (8 MiB) on purpose.** Those are two different buffers: this one sits between our demux thread
// and our pump, and that one is what Starfish will hold. They were equal because 8 MiB was copied
// from the payload, not because anything requires it — feeding past Starfish's own level returns
// `'B'` and the pump retries, which is the backpressure path every playback already uses.
//
// **What the 2 MiB buys, and why it is not a guess.** The reserve the adaptive controller can ever
// see is `B_max = feed_lead + queue_bytes*8 / R_elementary` — a queue term that falls as `1/R`
// while the upshift guard it has to clear is `Omega(D)`. R2 of the plan of record showed those
// crossing at about 15.7 Mbit/s of media at 8 MiB, i.e. **the top of the ladder is unreachable for
// any guard of that shape**, and no control law fixes a plant that cannot hold the reserve its own
// guard demands. `tools/abr-plant-sweep.py` prices this at +555 ms of climbable ceiling WITH the
// graded-reject feed and **+17 ms without it** — inside a measured 167 ms noise floor — so the two
// levers are one decision and neither is worth landing alone.
// `docs/measurements/p0-plant-sizing.md` is the record; `requiredMemory` is 160 MB, so +2 MiB RSS
// on a 32-bit set is inside the budget already declared.
const AQ_VIDEO_BYTES: c_long = 10 * 1024 * 1024;
const AQ_AUDIO_BYTES: c_long = 1024 * 1024;

pub struct SampleBuf {
    pub data: Vec<u8>,
    pub au: Vec<usize>, // AU start offsets
    pub next: usize,
    pub loops: i64,
}

pub enum Source {
    Stream, // host/port/path are consumed by the demux thread at spawn
    Sample(Box<SampleBuf>),
}

/// owns a popped au_node; frees on drop (paired with the malloc in aq_push).
pub struct AuBox(pub *mut AuNode);
impl Drop for AuBox {
    fn drop(&mut self) {
        unsafe { libc::free(self.0 as *mut c_void) }
    }
}

/// MAIN-THREAD-CONFINED. No worker thread ever names an Engine field.
pub struct Engine {
    /// Exact reducer attempt which constructed this native object. A duplicate start for this
    /// attempt is harmless; a different Prepared transaction must never be mistaken for it.
    pub route_start: crate::route::RouteStartAttempt,
    /// Firmware callback context for this exact native `Load`.
    pub native_epoch: u32,
    pub stage: Stage,
    pub video_info_sent: bool, // videoInfoSent
    /// ACB route: `SHARED.frames` when the plane bind completed (`Stage::Streaming`), until a tick
    /// beyond it is seen. A paused preroll re-pauses only after that tick: a picture presented
    /// before the bind is not on the panel.
    pub bound_at_frames: Option<i32>,
    /// webOS 5+ only: the source size the exported window was last placed with, so a corrective
    /// re-place happens exactly once if the demuxer publishes the real one later. 0 = never placed.
    pub placed_src: (c_int, c_int),
    pub eos_pushed: bool, // Kodi VIDEO_DRAIN: pushEOS() sent once at true EOF
    pub rebase_pending: bool, // g_rebase_pending
    // In-place seek only: keyframes this far AHEAD of the seek target are stale frames the demuxer
    // produced from its pre-flush read position before the reopen+av_seek won the race (playback
    // drifts forward during a long scrub) — reject them so the rebase anchors on the REAL post-seek
    // keyframe, not the drifted one. `rebase_drops` caps the rejections so a sparse-keyframe file or
    // a genuinely-failed av_seek can't hang the rebase.
    pub rebase_drops: i32,
    pub seek_armed_at: u32, // SDL-ticks when the current in-place seek was armed (stuck watchdog)
    pub seek_retries: i32,  // cheap reopen-retries attempted before escalating to a full reload
    pub flushed: bool,      // Kodi m_flushed: set on an in-place seek flush; the first
    // post-flush keyframe triggers setTimeToDecode + sendSegmentEvent (the fresh GStreamer
    // segment a bare flush() omits), then clears this.
    /// The post-seek keyframe has installed its segment but has not yet been accepted by
    /// Starfish. Native presentation callbacks remain fenced until that exact AU returns `O`.
    pub presentation_rearm_pending: bool,
    pub max_fed_video_pts: i64, // high-water fed pts, VIDEO lane (g_max_fed_pts)
    pub max_fed_audio_pts: i64, // high-water fed pts, AUDIO lane (two-lane feed)
    /// Fed pts of the AU that opened this timeline's audio — the one the demuxer marked as its
    /// first since the open or seek (`SHARED.first_audio_pts`) — once Starfish accepted it.
    /// `None` until then; reset with `max_fed_audio_pts`. See [`audio_lead_in`].
    pub audio_start_fp: Option<i64>,
    /// Added to the sink pts of every audio AU of THIS Load ([`audio_sink_pts`]): the active sound
    /// output's lead on a `liblxa` or 2018 stack, the one lead of the `lgaudiosink` stack
    /// ([`pcm_output_lead`]), or the dev override, when the Load declared `"audio":"PCM"`, 0 for a
    /// compressed Load and for a stack with no lead. Chosen once per Load, from the plan
    /// [`load_audio`] made with the declared audio ([`PcmLead`]); an in-place seek keeps it.
    pub pcm_sink_lead_ns: i64,
    /// The lead follows the sound output, which only the Load thread may read (the frame thread
    /// may not wait on the bus): `pcm_sink_lead_ns` is not final until [`settle_pcm_lead`] has
    /// taken it from the Load's return, and no audio is fed before.
    pub pcm_lead_pending: bool,
    /// The feed-ahead throttle's reference position, run on between the pipeline's 5 Hz
    /// position reports (see [`FeedPace`]).
    pub feed_pace: FeedPace,
    pub seek_base_pts: i64,     // fed pts of the first post-seek keyframe (prime measures buffer
    // depth as max_fed_video_pts - seek_base_pts, since the in-place seek feeds REAL pts, not 0-based)
    // prime-then-play: after a seek/resume the pipeline is PAUSED and data is buffered before
    // Play, so the clock doesn't free-run through the demux reopen / transcode-restart gap (that
    // gap is what makes video "fast-forward" to catch the audio clock on resume). feed_stream
    // fires Play once max_fed_pts reaches PRIME_NS.
    pub prime_play: bool,
    // Two-lane feed (Kodi m_messageQueueVideo/Audio): the demuxer routes es=1 video to aq_video
    // and es=2 audio to aq_audio; each lane is fed independently so a video BufferFull can't stall
    // the audio lane (the audioSync master clock). Both are allocated for a stream. (None only
    // pre-start and on the local-sample source.)
    pub aq_video: Option<Box<AuQueue>>, // g_aq (M owns; ptr handed to D)
    pub aq_audio: Option<Box<AuQueue>>, // audio lane
    // hs/payload are RAII: held alive for the workers (which hold raw ptrs into
    // them) and freed only after join — never read back through the field.
    #[allow(dead_code)]
    pub hs: Box<HttpStream>, // demux socket (M owns; D uses via raw ptr)
    pub pending_video: Option<AuBox>, // bf_pending, VIDEO lane (held across BufferFull)
    pub pending_audio: Option<AuBox>, // bf_pending, AUDIO lane
    #[allow(dead_code)]
    pub payload: std::ffi::CString, // bf_payload (kept alive for the session)
    pub source: Source,
    pub stream_th: Option<std::thread::JoinHandle<()>>,
    pub load_th: Option<std::thread::JoinHandle<()>>,
    pub report_th: Option<std::thread::JoinHandle<()>>, // /:/timeline progress reporter
    pub report_stop: Option<std::sync::Arc<threads::ReportStop>>, // ITS stop signal, not SHARED's
    /// Preview Load that must land unbound. The media join waits until the thread has returned.
    pub preview_abandon: bool,
    /// The foreground lifecycle follows this Engine's Load attempt ([`foreground_follows_load`]).
    /// It then owns the Load call's own synchronous result (`route_start_status` `Failed`, the
    /// refusal #1018 retries), so the pump's failed-open ladder leaves that one alone
    /// ([`super::pump`]'s `settled_by_foreground`).
    pub foreground_follows_load: bool,
    /// Whether this session's era-chosen sink counter has shown up ([`super::era_callbacks`]).
    pub(super) era_watch: super::era_callbacks::Watch,
}

impl Engine {
    pub fn uses_stream_queues(&self) -> bool {
        matches!(self.source, Source::Stream)
    }
}

/// Test builds only: how many Engines have dropped (the exit's leak is observed by it staying put).
#[cfg(test)]
static ENGINES_DROPPED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Test builds only: an Engine dropped while it still holds its demuxer's handle was never torn
/// down, so that thread may still be writing into `aq_video`/`aq_audio`, which this drop frees.
/// `teardown` always takes the handle and joins it first. The hostsim rigs used to drop a
/// reloaded Engine straight out of their `PlayerAdapter`: the demuxer's `aq_set_eof` then landed
/// in freed memory, a heap corruption that surfaced later as a crash in some unrelated free.
#[cfg(test)]
impl Drop for Engine {
    fn drop(&mut self) {
        ENGINES_DROPPED.fetch_add(1, Ordering::SeqCst);
        if self.stream_th.is_some() && !std::thread::panicking() {
            panic!("an Engine was dropped with its demuxer still owned: stop it with teardown first");
        }
    }
}

// The `ENGINE` slot is `App.adapters.player` since phase 9 — a field, reached as
// `pa: &mut PlayerAdapter` from the loop. `player::adapter` carries the confinement argument the
// four `static mut` accessors used to carry here, and says what the owned slot proves that the
// `&MainThread` argument could only assert.

/// Owns the explicit `Held(Initial)` clock state until the matching Engine is installed. Every
/// early return after worker creation runs this guard only after its local handles have been
/// stopped/joined, so a failed start cannot poison the next attempt with a phantom held clock.
struct ClockStartGuard {
    armed: bool,
}

impl ClockStartGuard {
    fn arm() -> Option<Self> {
        SHARED
            .arm_initial_clock_hold(TX.paused.load(Ordering::Acquire), TX.seek_preroll_active())
            .then_some(Self { armed: true })
    }

    fn commit(mut self) {
        self.armed = false;
    }
}

impl Drop for ClockStartGuard {
    fn drop(&mut self) {
        if self.armed {
            SHARED.cancel_clock_session_start();
        }
    }
}

/// Own the native callback generation until the matching Engine has been installed.  Every early
/// return then retires it automatically, and retirement is also a barrier for a callback already
/// executing on Starfish's library thread.
struct NativeSessionStartGuard {
    epoch: u32,
    armed: bool,
}

impl NativeSessionStartGuard {
    fn arm() -> Option<Self> {
        SHARED
            .begin_native_session()
            .map(|epoch| Self { epoch, armed: true })
    }

    fn epoch(&self) -> u32 {
        self.epoch
    }

    fn commit(mut self) {
        self.armed = false;
    }
}

impl Drop for NativeSessionStartGuard {
    fn drop(&mut self) {
        if self.armed {
            SHARED.retire_native_session(self.epoch);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeObjectDisposition {
    Destroy,
    Quarantine,
}

/// D1 frees private state which a late firmware hook dereferences before it reaches our Rust
/// callback. Every independent proof is therefore mandatory; this deliberately has no
/// best-effort or timeout branch.
fn native_object_disposition(
    unload_completed: bool,
    callback_gate_proven: bool,
    rust_epoch_retired: bool,
) -> NativeObjectDisposition {
    if unload_completed && callback_gate_proven && rust_epoch_retired {
        NativeObjectDisposition::Destroy
    } else {
        NativeObjectDisposition::Quarantine
    }
}

/// A native `Load` that teardown could not wait for.
///
/// The D.1.4 budget turns a `Load` that never returns into a failure read-out, but the object that
/// `Load` is running on cannot be touched until the call comes back — `Unload`/`Destroy` on it
/// while the load thread is still inside is the issue #74 race — and joining the media thread on
/// the SDL main thread froze the whole app on exactly the set where the Load was hung. So teardown
/// parks the thread, the payload it reads and the epoch here instead, and
/// [`reap_abandoned_load`] runs the ordinary native release (`Unload` → callback gate → epoch
/// retirement → D1 or quarantine) on the main thread once the thread has finished. Until then every
/// start is refused rather than handed the same object; if the `Load` never returns the object is
/// leaked for the life of the process, which is what the C seam would have to do anyway.
pub struct AbandonedLoad {
    epoch: u32,
    load_th: std::thread::JoinHandle<()>,
    /// The Load payload `sf_load` was handed as a raw pointer. Must outlive the thread.
    _payload: std::ffi::CString,
    abandoned_at: std::time::Instant,
}

/// Release a parked [`AbandonedLoad`] if its `Load` has returned. Cheap when there is nothing to
/// do; called once per app frame and at the top of every native start. `true` when the parked
/// object was released by this call.
pub fn reap_abandoned_load(pa: &mut super::adapter::PlayerAdapter) -> bool {
    let finished = match pa.abandoned_load_mut() {
        Some(parked) => parked.load_th.is_finished(),
        None => return false,
    };
    if !finished {
        return false;
    }
    let parked = pa.take_abandoned_load().expect("checked above");
    // The thread has already returned, so this join cannot block.
    plx_base::task::join("media", parked.load_th);
    log(&format!(
        "native lifecycle: abandoned Load epoch={} returned {}ms after teardown; releasing it",
        parked.epoch,
        parked.abandoned_at.elapsed().as_millis(),
    ));
    release_native_object(pa.mt(), parked.epoch);
    // `parked._payload` drops here, after the thread that read it has been joined.
    true
}

/// **A real stop's native object, handed to an off-frame `Unload`.** `StarfishMediaAPIs::Unload`
/// blocks for 110-150 ms on the dev set (webOS 4.5, release build, measured 2026-10-15 with
/// `teardown spans:`; the kernel sched trace of the same returns shows the main thread ~31-36 ms in
/// D state inside a driver `ioctl` and then ~72-74 ms in a futex until the pipeline thread woke it),
/// and on the main thread that was the whole of the frame that handled BACK: 140-170 ms on 8 of 8
/// returns. So a real stop issues it on a worker and keeps everything else where it was: the
/// callback gate, the epoch retirement, the ACB `setState(UNLOADED)`, D1 and the exported window
/// still run on the main thread, in the same order, once `Unload` has returned
/// ([`poll_release`], polled once a frame by `app::run`; the adapter hands out the token at once and
/// `player::sink()` fences the object meanwhile). A reload, an app-switch suspend and the decoder-revoked park keep the
/// synchronous release: each starts the next Load at once, so there is no frame to save.
///
/// While the worker owns the object its epoch is [`Shared::begin_native_release`]'s `Releasing`
/// phase, which admits only UNLOADCOMPLETED (the main thread has already reset the state any other
/// callback would write into), and no new native session may begin.
pub struct PendingRelease {
    epoch: u32,
    /// Taken by the join ([`join_finished_release`]); `None` afterwards.
    unload_th: Option<std::thread::JoinHandle<()>>,
    handed_off_at: std::time::Instant,
    /// The Engine the object belonged to. It used to drop at the end of `teardown`, after the
    /// release; it still drops after it (the Load payload in particular outlives the Unload).
    _engine: Engine,
}

impl PendingRelease {
    pub(crate) fn epoch(&self) -> u32 {
        self.epoch
    }

    pub(crate) fn handed_off_at(&self) -> std::time::Instant {
        self.handed_off_at
    }

    /// The `Unload` has returned, so joining its worker cannot block.
    pub(crate) fn is_finished(&self) -> bool {
        self.unload_th.as_ref().is_none_or(|t| t.is_finished())
    }
}

/// Test seam: make [`release_off_frame`] see the OS refuse the `sf-unload` thread.
#[cfg(test)]
static FAIL_UNLOAD_SPAWN_FOR_TEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Start the off-frame release of `epoch`'s object, or release it here when it cannot be handed
/// off (no dispatchable object, an epoch that no longer owns `Active`, or the OS refusing the
/// thread). `Some` = the worker is running and the caller must park it with the Engine.
fn release_off_frame(mt: &MainThread, epoch: u32, sp: &mut TeardownSpans) -> Option<std::thread::JoinHandle<()>> {
    if unsafe { super::raw_sink().ready(mt) } == 0 || !SHARED.begin_native_release(epoch) {
        release_native_object_timed(mt, epoch, sp);
        return None;
    }
    let ticket = UnloadTicket::issue(mt);
    #[cfg(test)]
    let refused = FAIL_UNLOAD_SPAWN_FOR_TEST.load(Ordering::Relaxed);
    #[cfg(not(test))]
    let refused = false;
    let th = if refused {
        None
    } else {
        #[cfg(all(test, feature = "hostsim"))]
        let worker = super::ffi_host::UnloadWorker::enter();
        plx_base::task::spawn("sf-unload", move || {
            #[cfg(all(test, feature = "hostsim"))]
            let _worker = worker;
            unsafe { super::raw_sink().unload(&ticket) }
        })
    };
    sp.mark("unload_handoff");
    if th.is_none() {
        log("native lifecycle: no thread for the off-frame Unload; unloading on the main thread");
        unsafe { super::raw_sink().unload(&UnloadTicket::issue(mt)) };
        sp.mark("sf_unload");
        finish_unloaded_object(mt, epoch, sp);
        destroy_native_window(mt, sp);
    }
    th
}

/// **The runner of [`super::release::NativeRelease`].** Once a frame (and at the head of a start):
/// join the `Unload` worker only if it has returned (so the join cannot block), feed that and the
/// clock to the machine, and execute what it asks: the main-thread tail, the start that was queued
/// behind the release, or the typed refusal once the release is overdue. `true` when a tail ran.
pub fn poll_release(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    now: std::time::Instant,
) -> bool {
    let (fx, joined) = drive_release(pa, now, None);
    run_release_effects(ps, pa, fx, joined, true).tail_ran
}

/// [`poll_release`] at the wall clock: the per-frame entry `app::run` calls.
pub fn poll_release_frame(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) -> bool {
    poll_release(ps, pa, std::time::Instant::now())
}

/// What one pass of the effects did.
#[derive(Default)]
struct ReleasePass {
    tail_ran: bool,
    exit: Option<super::release::ReleaseEffect>,
}

/// Join a returned worker, then step the machine with its return, the clock and (at the exit) the
/// exit request. Returns the effects and the joined release, whose tail the effects may run.
fn drive_release(
    pa: &mut super::adapter::PlayerAdapter,
    now: std::time::Instant,
    exit: Option<std::time::Instant>,
) -> (Vec<super::release::ReleaseEffect>, Option<PendingRelease>) {
    use super::release::ReleaseEvent as V;
    let mut fx = Vec::new();
    let joined = pa.take_finished_release().map(|mut p| {
        if let Some(th) = p.unload_th.take() {
            plx_base::task::join("sf-unload", th);
        }
        pa.release_step(&V::UnloadReturned { epoch: p.epoch }, &mut fx);
        p
    });
    pa.release_step(&V::Tick { now }, &mut fx);
    if let Some(at) = exit {
        pa.release_step(&V::Exit { now: at }, &mut fx);
    }
    (fx, joined)
}

fn run_release_effects(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    fx: Vec<super::release::ReleaseEffect>,
    mut joined: Option<PendingRelease>,
    may_start: bool,
) -> ReleasePass {
    use super::release::ReleaseEffect as E;
    let mut pass = ReleasePass::default();
    for effect in fx {
        match effect {
            E::RunTail { epoch } => {
                if let Some(p) = joined.take() {
                    debug_assert_eq!(p.epoch, epoch);
                    run_release_tail(pa.mt(), p);
                    pass.tail_ran = true;
                }
            }
            E::Start(q) if may_start => {
                let launched = match start_bufferfeed_for(ps, pa, q.route_start) {
                    BufferfeedStartOutcome::Launched(attempt) => Some(attempt),
                    _ => None,
                };
                pa.settle_preview_admission(launched.is_some(), true);
                pa.queued_start_ran(launched);
            }
            // The exit does not start playback; the transaction is settled instead.
            E::Start(q) => {
                pa.settle_preview_admission(false, false);
                let _ = crate::route::abort_route_start(q.route_start, crate::route::RouteStartResult::StartFailed);
            }
            E::AbortSuperseded(q) => {
                pa.settle_preview_admission(false, false);
                pa.queued_start_dropped();
                let _ = crate::route::abort_route_start(q.route_start, crate::route::RouteStartResult::StartFailed);
            }
            E::RefuseStart(q) => {
                refuse_start_for_release(ps, q);
                pa.settle_preview_admission(false, true);
                pa.queued_start_ran(None);
            }
            exit @ (E::ExitClean | E::ExitWaiting | E::ExitWorkerInFirmware) => pass.exit = Some(exit),
        }
    }
    pass
}

/// The deadline passed over a start: settle its transaction as failed and record where
/// `player::state` derives the `release_timeout` read-out from. No Engine, no second object.
fn refuse_start_for_release(ps: &mut crate::route::PlaybackSession, q: super::release::QueuedStart) {
    log("native lifecycle: the previous Unload is overdue (refusing native start)");
    let _ = crate::route::abort_route_start(q.route_start, crate::route::RouteStartResult::StartFailed);
    ps.native_start_refused = Some(crate::route::StartRefusal::ReleaseTimeout);
}

/// The main-thread tail of a finished off-frame release: gate, epoch, ACB, D1, window.
fn run_release_tail(mt: &MainThread, p: PendingRelease) {
    let mut sp = TeardownSpans::new();
    sp.mark("join");
    finish_unloaded_object(mt, p.epoch, &mut sp);
    destroy_native_window(mt, &mut sp);
    sp.emit("release");
    log(&format!(
        "native lifecycle: off-frame Unload epoch={} released {} ms after the stop",
        p.epoch,
        p.handed_off_at.elapsed().as_millis(),
    ));
    // `p._engine` drops here, after the object it belonged to is gone.
}

/// Whether the process may tear the display down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeExit {
    Clean,
    /// A native worker (the `sf-unload` worker, or a timed-out Load's media thread) is still
    /// inside firmware: `SDL_Quit` / the Wayland window destroy must not run under it.
    WorkerInFirmware,
}

/// The native half of `app::run::shutdown`, after the final stop. Waits for the `Unload` up to
/// the release deadline (the process is ending anyway; the old wait was unbounded).
pub fn exit_native(pa: &mut super::adapter::PlayerAdapter) -> NativeExit {
    exit_native_with(pa, std::time::Instant::now, || std::thread::sleep(std::time::Duration::from_millis(2)))
}

fn exit_native_with(
    pa: &mut super::adapter::PlayerAdapter,
    mut clock: impl FnMut() -> std::time::Instant,
    mut idle: impl FnMut(),
) -> NativeExit {
    reap_abandoned_load(pa);
    if pa.has_abandoned_load() {
        log("exit: a timed-out Load is still inside firmware; the display is left to the OS");
        leak_parked_for_exit(pa);
        return NativeExit::WorkerInFirmware;
    }
    let mut ps = crate::route::PlaybackSession::IDLE;
    loop {
        let now = clock();
        let (fx, joined) = drive_release(pa, now, Some(now));
        match run_release_effects(&mut ps, pa, fx, joined, false).exit {
            Some(super::release::ReleaseEffect::ExitClean) => return NativeExit::Clean,
            Some(super::release::ReleaseEffect::ExitWorkerInFirmware) => {
                log("exit: the native Unload is overdue; the display is left to the OS");
                leak_parked_for_exit(pa);
                return NativeExit::WorkerInFirmware;
            }
            _ => idle(),
        }
    }
}

/// **A worker still in firmware reads memory its owner is about to drop.** The caller skips
/// `SDL_Quit` and `app::run_and_shutdown` ends the process with `_exit` (no `App` drop, no libc
/// finalizers), but the stack above it may still unwind, so the parked
/// [`PendingRelease`] (its Engine: the Load payload Starfish holds a raw pointer into, the
/// handle set, the AU queues) and the parked [`AbandonedLoad`] (the payload its `media` thread
/// reads) would be freed under the live call. They are leaked instead, for the life of the process
/// (`release.rs`: the `_exit` that follows skips their destructors). Only `exit_native` calls this, and only on that outcome.
fn leak_parked_for_exit(pa: &mut super::adapter::PlayerAdapter) {
    if let Some(p) = pa.take_pending_release_any() {
        std::mem::forget(p);
    }
    if let Some(l) = pa.take_abandoned_load() {
        std::mem::forget(l);
    }
}

/// Test helper: finish the pending release the way the exit does, with no deadline concerns.
#[cfg(all(test, feature = "hostsim"))]
pub(crate) fn finish_release_for_test(pa: &mut super::adapter::PlayerAdapter) {
    let mut ps = crate::route::PlaybackSession::IDLE;
    while pa.release_busy() {
        let (fx, joined) = drive_release(pa, std::time::Instant::now(), None);
        let _ = run_release_effects(&mut ps, pa, fx, joined, false);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Unload and retire the native object of `epoch`, then destroy it only with complete evidence.
/// The engine's own teardown and the abandoned-Load release share this so the order below is one
/// definition.
fn release_native_object(mt: &MainThread, epoch: u32) {
    release_native_object_timed(mt, epoch, &mut TeardownSpans::new());
}

/// **Where a teardown's frame goes, one step at a time.** Each step's wall time is appended as
/// `label:ms` and the whole run is logged as ONE `teardown spans:` line at the end, so a slow BACK
/// frame names the call that held the main thread instead of being inferred from a sched trace.
/// Measuring costs two `Instant::now()` per step; the line is written once per teardown.
pub(crate) struct TeardownSpans {
    t0: std::time::Instant,
    last: std::time::Instant,
    out: String,
}

impl TeardownSpans {
    pub(crate) fn new() -> Self {
        let now = std::time::Instant::now();
        Self { t0: now, last: now, out: String::new() }
    }

    /// Close the step that ran since the previous mark under `label`.
    pub(crate) fn mark(&mut self, label: &str) {
        use std::fmt::Write as _;
        let now = std::time::Instant::now();
        let ms = now.duration_since(self.last).as_secs_f64() * 1000.0;
        self.last = now;
        let _ = write!(self.out, "{}{label}:{ms:.1}", if self.out.is_empty() { "" } else { "," });
    }

    pub(crate) fn emit(&self, what: &str) {
        log(&format!(
            "teardown spans: {what} total={:.1} {}",
            self.t0.elapsed().as_secs_f64() * 1000.0,
            self.out,
        ));
    }
}

fn release_native_object_timed(mt: &MainThread, epoch: u32, sp: &mut TeardownSpans) {
    // Stop the pipeline, close native callback admission, then drain the Rust epoch.
    //
    // `Unload` waits for the GStreamer state transition, but neither libpf's raw callback fields nor
    // type=23 is an exported producer/in-flight barrier. The executable therefore interposes the
    // exact callbackFunctionHook JUMP_SLOT. Each Load owns a never-reused object address; after
    // Unload the C gate rejects new calls and waits for calls already inside the real hook. Only
    // then can the Rust callback mutex be retired and D1 considered:
    //
    //     Unload -> gate inactive/drained -> retire/drain epoch -> D1.
    //
    // Both the synchronous type=23 observation and a non-zero per-object interposer counter are
    // runtime evidence that this firmware followed the audited path. If any proof is missing, C
    // retains the constructed object forever and permanently refuses another Load. Leaking one
    // object is preferable to letting D1 turn a late producer callback into a use-after-free.
    if unsafe { super::raw_sink().ready(mt) } != 0 {
        sp.mark("pre_unload");
        unsafe { super::raw_sink().unload(&UnloadTicket::issue(mt)) };
        sp.mark("sf_unload");
        finish_unloaded_object(mt, epoch, sp);
    } else if !SHARED.retire_native_session(epoch) {
        log(&format!(
            "native lifecycle: no dispatchable object and epoch={epoch} was already retired",
        ));
    }
    destroy_native_window(mt, sp);
}

/// Everything after `Unload` returns, in the audited order: close and drain the C callback gate,
/// retire the Rust epoch, unload the ACB plane, then D1 with complete evidence or quarantine. The
/// synchronous release and the off-frame release's main-thread tail ([`run_release_tail`]) share
/// it, so the order is one definition.
fn finish_unloaded_object(mt: &MainThread, epoch: u32, sp: &mut TeardownSpans) {
    {
        window_step(WindowStage::NativeUnload);
        let callback_gate_proven = unsafe { super::raw_sink().callback_gate_retire(mt) } != 0;
        sp.mark("gate_retire");
        let callback_intercepts = unsafe { super::raw_sink().callback_intercepts(mt) };
        let unload_completed = SHARED.native_unload_completed(epoch);
        let rust_epoch_retired = SHARED.retire_native_session(epoch);
        if !unload_completed {
            log(&format!(
                "native lifecycle: Unload returned without type=23 epoch={epoch}",
            ));
        }
        if !callback_gate_proven {
            log(&format!(
                "native lifecycle: callback interposition unproven epoch={epoch} intercepted={callback_intercepts}",
            ));
        }
        if !rust_epoch_retired {
            log(&format!(
                "native lifecycle: failed to retire epoch={epoch} after Unload",
            ));
        }
        sp.mark("retire");
        if ACB_OK.load(Ordering::Relaxed) {
            unsafe { super::raw_sink().plane_unload(mt) };
            sp.mark("acb_unload");
            window_step(WindowStage::AcbPlaneUnload);
        }
        match native_object_disposition(unload_completed, callback_gate_proven, rust_epoch_retired)
        {
            NativeObjectDisposition::Destroy => {
                let destroyed = unsafe { super::raw_sink().destroy(mt) } != 0;
                sp.mark("sf_destroy");
                if !destroyed {
                    log("native lifecycle: C seam rejected D1 and quarantined the object");
                    window_step(WindowStage::NativeQuarantined);
                } else {
                    window_step(WindowStage::NativeDestroyed);
                }
            }
            NativeObjectDisposition::Quarantine => {
                unsafe { super::raw_sink().quarantine(mt) };
                sp.mark("quarantine");
                window_step(WindowStage::NativeQuarantined);
            }
        }
    }
}

fn destroy_native_window(mt: &MainThread, sp: &mut TeardownSpans) {
    // The webOS 5+ counterpart of acb_unload, and the other half of `with_window_id`'s create.
    // Outside the sf_ready guard because readiness means dispatchable, not "object exists", and
    // because the window is created BEFORE Load — a session that failed between the two would
    // otherwise leak it, and the next Load would ask for another. Unguarded because the seam
    // already no-ops in the other modes (see starfish.h).
    unsafe { super::raw_sink().destroy_window(mt) };
    sp.mark("destroy_window");
    if super::raw_sink().window_mode() == super::VP_EXPORTED {
        window_step(WindowStage::ExportedWindowDestroy);
    }
}

/// One closed teardown/exported-window breadcrumb on a native crash event, **only inside an app
/// switch's background or foreground cycle** (`window::record_in_cycle`): this runs for every
/// engine that comes and goes (a trailer, a seek or quality reload, an abandoned Load), and those
/// would fill the shared 16-slot ring with steps of no cycle. Consent-gated inside
/// `window::record` and a no-op while the crash backend is not running; carries no data beyond the
/// label, so a crash in the next step shows which step had already returned.
fn window_step(stage: WindowStage) {
    plx_telemetry::telemetry::window::record_in_cycle(
        plx_telemetry::telemetry::window::Observation::step(stage, None),
    );
}

/// Bind the decoded video sink to the display plane, whichever way this television does it.
///
/// **Boot-scoped.** Called once, from `plex_run`, and never again — which is why the webOS 5
/// exported window is NOT created here: that one is created per session in [`start_bufferfeed`],
/// beside the payload it has to be spliced into, and destroyed by the matching `teardown`.
///
///   - `VP_ACB` — create + initialize the ACB. We deliberately DON'T register our own
///     com.webos.media client; it collides with the pipeline's uMS connection. The handle lives
///     for the process; what teardown calls is `acb_unload`, a session-scoped state change.
///   - `VP_EXPORTED` — nothing to do at boot. There is no handle, and no state mirroring:
///     webOS 5 deleted that sequence outright rather than replacing it.
pub fn acb_init(mt: &MainThread) {
    match sink().window_mode() {
        super::VP_EXPORTED => {
            log("vplane: exported window (webOS 5+) — created per session");
            // Nothing to bind later; the pump's ACB stages are skipped by ACB_OK staying false.
            ACB_OK.store(false, Ordering::Relaxed);
        }
        super::VP_NONE => {
            log(
                "vplane: this device has NEITHER libAcbAPI nor SDL's exported window — audio \
                 will play and the picture will not appear. Please report your webOS version.",
            );
            ACB_OK.store(false, Ordering::Relaxed);
        }
        _ => acb_init_acb(mt),
    }
}

/// The webOS 4.x half of [`acb_init`].
fn acb_init_acb(mt: &MainThread) {
    if let Some(s) = plx_base::devtrig::read("ptype") {
        if let Ok(p) = s.parse::<c_int>() {
            PTYPE.store(p, Ordering::Relaxed);
        }
    }
    let pt = PTYPE.load(Ordering::Relaxed);
    log(&format!("ptype={pt}"));
    // Which app ACB is being initialized for. The third argument of `AcbAPI_initialize` is an app
    // id — that much is certain, and both reference implementations pass one (Kodi's webOS port
    // passes `getenv("APPID")`; see docs/distribution.md). What ACB does with it has not been
    // decompiled here, so the rule is simply that it must be the install SAM actually launched —
    // and with a developer build able to sit beside the shipped one, "the id compiled in" and
    // "the id we were launched as" are no longer the same question.
    //
    // It used to be `env::var("APPID")` with a NULL fallback, which `starfish.c` then turned into
    // the shipped app's literal id. That is a double fallback whose failure is invisible: on any
    // launch where SAM did not export APPID, a developer install would announce itself as the
    // shipped app. `paths::app_id` reads the install directory instead, which is the id webOS
    // registered by definition. The environment is still logged, one line at boot, as the
    // independent witness for whether SAM sets it at all on this firmware.
    let app_c = std::ffi::CString::new(plx_base::paths::app_id()).ok();
    let app_ptr = app_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let acb = unsafe { sink().plane_create(mt, app_ptr, pt) };
    ACB_OK.store(acb != 0, Ordering::Relaxed);
    log(&format!("acb create={acb}"));
}

/// split Annex-B into AUs on the 5-byte AUD prefix 00 00 00 01 <aud5>
/// (H264 AUD = 0x09; HEVC AUD is NAL type 35 → first header byte 0x46).
fn bf_split(data: &[u8], aud5: u8) -> Vec<usize> {
    let mut au = Vec::new();
    let mut i = 0usize;
    while i + 4 < data.len() {
        if data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
            && data[i + 4] == aud5
        {
            au.push(i);
            i += 4;
        }
        i += 1;
    }
    au
}

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
const ENVELOPE_UHD60: SinkEnvelope = SinkEnvelope {
    w: 3840,
    h: 2160,
    fps: 60,
};
/// The one SMALLER declaration with a device measurement behind it: the H.264 patch that loaded in
/// 117 ms on 10.3.1 where 4K60 was refused twice, and played through ABR commits afterwards.
const ENVELOPE_FHD60: SinkEnvelope = SinkEnvelope {
    w: 1920,
    h: 1080,
    fps: 60,
};

/// The video codec one Load declares: what the payload's `codec.video` names, and what the
/// `dg_load_v` diagnostic code says. Written once here so the three never disagree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LoadVideo {
    H264,
    H265,
    Av1,
    Vp9,
}

impl LoadVideo {
    /// The route's stream vcodec -> what the Load declares, case-insensitively (PMS emits lowercase;
    /// the route's own comparisons elsewhere are case-insensitive too). **An unknown codec is `None`,
    /// not H.264**: the old catch-all declared an H.264 Load for whatever the route held, so a codec
    /// the pipeline cannot feed arrived as an H.264 declaration over a bitstream it could not parse,
    /// i.e. a black screen with nothing in the log naming the cause. The empty string is the one
    /// deliberate H.264: the route holds it until a decision installs a codec, which is the pipeline
    /// tier's and the local-sample feeds' baseline.
    pub(crate) fn of_stream(vcodec: &str) -> Option<Self> {
        match vcodec.to_ascii_lowercase().as_str() {
            "" | "h264" => Some(Self::H264),
            "hevc" | "h265" => Some(Self::H265),
            "av1" => Some(Self::Av1),
            "vp9" => Some(Self::Vp9),
            _ => None,
        }
    }
    /// The payload's `codec.video` string. libpf selects its AV1 caps for `"AV1"`; the pipeline
    /// has no av1parse, so an AV1 Load carries its configuration in-band (see `ff.rs`). `"VP9"`
    /// is the name in libpf's codec table beside `"VP8"`; its `video/x-vp9` appsrc caps carry no
    /// `codec_data`, and a VP9 frame carries its own headers, so nothing is added to the packets.
    pub(crate) fn payload_name(self) -> &'static str {
        match self {
            Self::H264 => "H264",
            Self::H265 => "H265",
            Self::Av1 => "AV1",
            Self::Vp9 => "VP9",
        }
    }
    /// `SHARED.dg_load_v`'s code (video 1 = H264, 2 = H265, 3 = AV1, 4 = VP9).
    pub(crate) fn diag_code(self) -> u8 {
        match self {
            Self::H264 => 1,
            Self::H265 => 2,
            Self::Av1 => 3,
            Self::Vp9 => 4,
        }
    }
}

/// True when the Load must be refused: an AV1 or VP9 stream on a set whose table lists no such
/// decoder. Only reachable when a server ignores the profile we sent, so the refusal is a refusal
/// to declare a codec the set may not have, never a fallback to H.264 for another bitstream.
pub(crate) fn load_video_refused(lv: LoadVideo, caps: &plx_platform::devcaps::Caps) -> bool {
    match lv {
        LoadVideo::Av1 => !caps.av1,
        LoadVideo::Vp9 => !caps.vp9,
        LoadVideo::H264 | LoadVideo::H265 => false,
    }
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
fn sink_envelope(
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
fn decoder_row(video: LoadVideo, caps: &plx_platform::devcaps::Caps) -> (u32, u32, u32) {
    match video {
        LoadVideo::H264 => caps.h264_row,
        LoadVideo::H265 => caps.hevc_row,
        LoadVideo::Av1 => caps.av1_row,
        LoadVideo::Vp9 => caps.vp9_row,
    }
}

/// **The frame rate a playback speed may feed this stream at**: [`super::SPEED_MAX_FPS`], or the
/// measured decoder row's rate when that is lower. A speed feeds every frame `rate` times closer
/// together and the Load declares that fed rate, which [`sink_envelope`] then clamps to the row; a
/// speed whose fed rate passes the row is one the Load cannot declare, so the menu does not offer
/// it. Only a table that was actually READ bounds anything (`measured`,
/// the same condition [`sink_envelope_now`] clamps under), a stream whose codec the Load cannot
/// declare has no row, and a row with no rate (0) is no bound. The `+ 0.5` is the 60.5 ceiling's own
/// tolerance, so a 29.97 stream is not turned away from a 30 row. Pure; the wrapper feeds it the
/// route.
pub(crate) fn speed_fps_ceiling(
    video: Option<LoadVideo>,
    caps: &plx_platform::devcaps::Caps,
    measured: bool,
) -> f64 {
    let row_fps = match video {
        Some(video) if measured => decoder_row(video, caps).2,
        _ => 0,
    };
    if row_fps == 0 {
        return super::SPEED_MAX_FPS;
    }
    (f64::from(row_fps) + 0.5).min(super::SPEED_MAX_FPS)
}

/// [`speed_fps_ceiling`] for the playback on screen, from the live caps the way
/// [`sink_envelope_now`] reads them.
pub(crate) fn speed_fps_ceiling_now(ps: &crate::route::PlaybackSession) -> f64 {
    speed_fps_ceiling(
        LoadVideo::of_stream(&crate::route::stream_vcodec(ps)),
        &plx_platform::devcaps::caps(),
        plx_platform::devcaps::measured() && !crate::route::forced_direct_play(ps),
    )
}

/// The sink envelope for THIS Load, from the route (main thread), with the dev override applied.
/// **What the two STATIC payloads declare**, as the numbers written into their strings — used on
/// every arm that sends one (the local-sample feeds and the streamed `/tmp/plxnative-noaudio`
/// path), so the `load:` line's `max=` reports what was SENT rather than the streamed builder's
/// initializer. The test below pins these to the strings themselves.
fn static_envelope(video: LoadVideo) -> SinkEnvelope {
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
fn renamed_static_payload(video: LoadVideo) -> String {
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
fn fps_class(fps: f64) -> u32 {
    for class in [24u32, 25, 30, 50, 60] {
        let c = f64::from(class);
        if fps <= c + 0.01 || 1000.0 / fps > 1000.0 / c - 1.0 {
            return class;
        }
    }
    fps.ceil() as u32
}

fn sink_envelope_now(ps: &crate::route::PlaybackSession, video: LoadVideo) -> SinkEnvelope {
    if let Some(spec) = plx_base::devtrig::read("sinkmax") {
        if let Some(env) = parse_sinkmax(&spec) {
            #[cfg(feature = "devtriggers")]
            log(&format!(
                "sinkmax: envelope OVERRIDDEN to {}x{}@{} by /tmp/plxnative-sinkmax",
                env.w, env.h, env.fps
            ));
            return env;
        }
        log(&format!("sinkmax: unparseable spec {spec:?} — ignored"));
    }
    sink_envelope(
        video,
        crate::route::sink_max_raster(ps),
        fed_fps(crate::route::stream_fps(ps), super::applied_rate_permille()),
        &plx_platform::devcaps::caps(),
        plx_platform::devcaps::measured() && !crate::route::forced_direct_play(ps),
    )
}

/// `WxH@F` — three positive integers; anything else is `None`.
fn parse_sinkmax(spec: &str) -> Option<SinkEnvelope> {
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
fn audio_payload_codec(codec: &str) -> Option<(&'static str, u8)> {
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

/// The audio name and diagnostic code a Load declares for `codec`: PCM whenever the app decodes the
/// stream itself (`pcm` is the shape [`crate::route::stream_pcm`] declared), whatever the codec
/// is — DTS is `DTS` only when the set's own decoder takes it — else [`audio_payload_codec`]'s.
/// PURE.
fn audio_declaration(codec: &str, pcm: Option<&crate::route::PcmFormat>) -> Option<(&'static str, u8)> {
    if pcm.is_some() {
        return Some(("PCM", 5));
    }
    audio_payload_codec(codec)
}

/// RFC 7845 §5.1 `OpusHead` as libopus's surround encoder writes it: mapping family 0 for mono and
/// stereo, family 1 (Vorbis channel order) for 3-8 channels, pre-skip 312, 48 kHz, 0 dB gain.
/// The Load is built before the demuxer opens the file, so the file's own CodecPrivate is not
/// available here; a 5.1 Opus track of a real library carries exactly this header (see the test).
fn opus_head(channels: i64) -> Option<Vec<u8>> {
    // (streams, coupled streams, channel mapping) for 3..=8 channels — libopus `vorbis_mappings`.
    const FAMILY1: [(u8, u8, &[u8]); 6] = [
        (2, 1, &[0, 2, 1]),
        (2, 2, &[0, 1, 2, 3]),
        (3, 2, &[0, 4, 1, 2, 3]),
        (4, 2, &[0, 4, 1, 2, 3, 5]),
        (4, 3, &[0, 4, 1, 2, 3, 5, 6]),
        (5, 3, &[0, 6, 1, 2, 3, 4, 5, 7]),
    ];
    let ch = u8::try_from(channels).ok().filter(|c| (1..=8).contains(c))?;
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
fn with_opus_info(p: &str, channels: i64) -> Option<String> {
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

/// **Who owns the PCM a Load declares.** Audio the app decodes at 1x (FLAC, ALAC, LPCM, and a lossy codec the attempt decodes in
/// the app: `ff_pcm`)
/// declares its own shape; at any other speed the speed decoder (`ff::SpeedDecoder`) decodes every
/// codec, those included, and feeds stereo PCM that [`build_av_payload`] declares with its own
/// fixed `pcmInfo` (kilohertz). The app-decoded shape is then dropped and, at 1x, that fixed node is dropped
/// instead, so the Load carries ONE `pcmInfo` and the demuxer opens ONE decoder.
/// PURE.
fn app_pcm_for_rate(
    session_rate: i32, pcm: Option<Result<crate::route::PcmFormat, String>>,
) -> Option<Result<crate::route::PcmFormat, String>> {
    if session_rate == super::RATE_NORMAL { pcm } else { None }
}

/// `hz` as the kilohertz number `pcmInfo.sampleRate` is matched as: `48`, `44.1`, `22.05`. libpf
/// reads the key with getFloat and looks it up in a kilohertz table; a rate in hertz matches
/// nothing and plays at the 44.1 kHz fallback. Declared as `48000`, a 1 kHz tone in a 48 kHz FLAC
/// was recorded from the room at 918.8 Hz on webOS 4.4.3 (OLED55B8), a 32 kHz one at 1378 Hz and
/// a 22.05 kHz one at about 2006 Hz: each is 1000 x 44100 / the file's rate.
/// PURE.
fn pcm_rate_khz(hz: u32) -> String {
    (f64::from(hz) / 1000.0).to_string()
}

/// The `contents.pcmInfo` node a PCM Load needs — the only way the pipeline learns the raw
/// stream's shape (libpf reads `pcmInfo.{format,channelMode,layout,sampleRate,bitsPerSample}`).
/// `sampleRate` is in kilohertz ([`pcm_rate_khz`]), the unit the speed decoder's node uses too.
/// PURE, spliced at the `provider` anchor like [`with_immersive`].
fn with_pcm_info(p: &str, audio: &str, pcm: Option<crate::route::PcmFormat>) -> String {
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
fn pcm_decode_line(codec: &str, f: &crate::route::PcmFormat) -> String {
    let resampled = if f.source_rate != f.sample_rate { format!(" (resampled from {}Hz)", f.source_rate) } else { String::new() };
    format!("pcm: {codec} decoded in-app -> {}Hz {}-bit {}ch{resampled}", f.sample_rate, f.bits, f.channels)
}

/// The pipeline reads the true dimensions from the SPS (Phase 0 HEVC probe), so mw/mh are only
/// the sink envelope.
fn build_av_payload(ps: &crate::route::PlaybackSession, video: &str, audio: &str, env: SinkEnvelope) -> String {
    build_av_payload_at(ps, video, audio, env, super::applied_rate_permille())
}

/// [`build_av_payload`] at an explicit session speed (`rate`, permille), so the host tests can pin
/// it without touching the process-wide `applied_rate_permille`.
fn build_av_payload_at(
    ps: &crate::route::PlaybackSession, video: &str, audio: &str, env: SinkEnvelope, rate: i32,
) -> String {
    // WHO declares the PCM, decided once: FLAC at 1x (the app decodes it, `ff_pcm`) declares its
    // own shape below via `with_pcm_info`; every other PCM Load (the speed decoder, any codec,
    // `rate != 1x`) gets the fixed stereo node here. `audio == "PCM"` is true for BOTH, so the
    // fixed node is skipped when the app's shape exists, else the Load would carry two `pcmInfo`
    // keys that disagree on the rate's unit and the bit depth. (A FLAC track whose facts are
    // missing is `Err`: that Load is refused in `start_bufferfeed` before this payload is used.)
    let app_pcm = app_pcm_for_rate(rate, crate::route::stream_pcm(ps)).and_then(Result::ok);
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
    if plx_base::devtrig::flag("nofps") {
        #[cfg(feature = "devtriggers")]
        log("esInfo: videoFps WITHHELD by /tmp/plxnative-nofps");
    } else if let Some((num, den)) = fed_fps_rational(crate::route::stream_fps(ps), rate) {
        p = p
            .replace(
                r#""seperatedPTS":true}"#,
                &format!(r#""seperatedPTS":true,"videoFpsValue":{num},"videoFpsScale":{den}}}"#),
            )
            .replace(
                r#""audioOnly":false"#,
                r#""audioOnly":false,"adaptiveResolution":true"#,
            );
        log(&format!(
            "esInfo: videoFps {num}/{den} + adaptiveResolution (src {:.3})",
            crate::route::stream_fps(ps)
        ));
    }
    let p = with_dolby_hdr_info(&p, video, crate::route::stream_dv_presentation(ps));
    // The app-decoded (FLAC, 1x) node: sampleRate in kHz and a format name. The speed decoder's
    // fixed node above is the only other `pcmInfo` a Load can carry, and never together with this.
    let p = with_pcm_info(&p, audio, app_pcm);
    with_immersive(&p, audio, crate::route::stream_immersive(ps))
}

/// The `contents.immersive` node — **the Dolby Atmos half of the same envelope**, and the reason
/// the television's Atmos read-out never appeared while its Dolby Vision one did.
///
/// PURE, so the splice is host-testable, and spliced at the same `provider` anchor and in the same
/// shape as [`with_dolby_hdr_info`], which is its sibling in every sense: one key inside
/// `externalStreamingInfo.contents`, one string, and the whole difference between a stream the
/// pipeline treats as ordinary E-AC3 and one it treats as immersive.
///
/// **The key and the value are both read off the television's own binaries** (2026-08-21), which
/// matters because neither is guessable and a wrong one is silent:
/// - `libpf-1.0.so.1.0.0` holds the literal key path `option.externalStreamingInfo.contents.immersive`,
///   logs it as `PF_EXT_IMMERSIVE : %s`, and carries it onto the audio caps it builds —
///   `audio mediaInfo … channels[%d] language[%s] … immersive[%s] role[%s]`. It is a `%s` all the
///   way down: libpf validates nothing and passes the string through.
/// - The VALUE therefore has to come from whoever fills it, and **the bare literal `ATMOS` exists in
///   exactly one library on this device: `libcbe.so`** — Chromium's media backend, i.e. the path
///   LG's own web apps (Plex's included) take. In its string pool that literal sits immediately
///   after `immersive` and in the same run as `externalStreamingInfo`, `esInfo`, `seperatedPTS`,
///   `provider`, `DolbyHdrInfo`, `encryptionType`, `profileId` and `contents` — the payload we
///   build, key for key. So `"ATMOS"` is not our invention; it is the value the working client
///   sends, recovered from the binary that sends it.
///
/// `libplayerAPIs` injects `platformSupportDolbyATMOS` itself from its configd cache, exactly as it
/// does for Dolby Vision, so this node states a fact about the STREAM and never about the set.
fn with_immersive(p: &str, audio: &str, atmos: bool) -> String {
    if !atmos {
        return p.to_string();
    }
    if audio != "AC3 PLUS" {
        // Same consistency guard as DolbyHdrInfo's H265 check.  The only Atmos elementary-stream
        // path this player feeds is E-AC3 JOC, named "AC3 PLUS" in LG's Load vocabulary.  AAC is
        // the HLS transcode output and cannot inherit the source track's immersive declaration.
        log(&format!(
            "atmos: immersive NOT sent — payload audio codec is {audio}, not AC3 PLUS"
        ));
        return p.to_string();
    }
    let anchor = r#""provider":"plxnative""#;
    if !p.contains(anchor) {
        log("atmos: payload has no provider anchor — immersive NOT spliced");
        return p.to_string();
    }
    log("atmos: sourceInfo contents.immersive=ATMOS");
    p.replace(anchor, &format!(r#"{anchor},"immersive":"ATMOS""#))
}

/// The `contents.DolbyHdrInfo` node, spliced into the Load payload only for a route whose frozen
/// decision declared Dolby Vision after affirmative platform capability.
///
/// PURE, so the splice is host-testable; the decision arrives from route state and is the SAME
/// frozen value `route::build_stream` gated direct play on
/// ([`plx_data::metadata::Dovi::presentation`]).
///
/// **Why this one node is the fix, from the television's own binaries** (decompiled 2026-08-21,
/// webOS 4.10.2 `libpf`): `CustomPipeline::parseOptionStringSpi` builds the literal key
/// `option.externalStreamingInfo.contents.DolbyHdrInfo`, asks `Options::checkKeyExistance` for it,
/// and on a hit sets `hasDolbyHdrInfo` — unconditionally, before a single sub-field is read and
/// with no platform gate. `getVideoCaps` then ends by adding `dolby-vision=TRUE` (plus
/// `dolby-vision-profile` when `profileId != -1`) to the caps it was already building. Without the
/// node, appsrc gets plain `video/x-h265` and nothing downstream can engage Dolby Vision. The
/// pipeline has parsed this all along; we simply never sent it.
///
/// Three things that look like they should change and do not:
/// - **the codec string stays `"H265"`.** `getVideoCaps` maps H265 to `video/x-h265` and that
///   branch falls THROUGH into the Dolby Vision tail; there is no DVHE/DVH1 entry in its codec
///   table (those literals belong to AdaptivePipeline's RFC-6381 parser, a different pipeline).
///   LG's own Chromium client also reports `codec.video = "H265"` for a Dolby Vision stream. The
///   codec guard below is therefore a consistency check, not a translation. It also admits
///   `"AV1"` (Profile 10) with the codec string unchanged likewise; that the AV1 branch reaches
///   the same tail is measured on a webOS 6 set, not decompiled
///   ([`plx_data::metadata::Dovi::presentation`]).
/// - **`profileId` must be a JSON integer** (`getInt`). Quoting it would leave the `-1` sentinel.
/// - **the library's injected platform metadata is not a safety gate.** libplayerAPIs injects
///   `platformSupportDolbyVision` / `supportDolbyTVATMOS` at the root, but libpf enables DV from
///   our node's presence regardless. We do not duplicate those root fields; `webos::caps` reads
///   the same public configd key independently and route policy requires an exact `true` before
///   this function can receive `Declare`.
///
/// The anchor is `"provider":"plxnative"` — the last key of `contents` and, by the test below,
/// present exactly once in `PAYLOAD_AV`. A `replace` that finds nothing is a silent no-node, which
/// is why the miss is logged rather than assumed away.
fn with_dolby_hdr_info(p: &str, video: &str, dv: plx_data::metadata::DvPresentation) -> String {
    let Some(n) = dv.declared() else {
        return p.to_string();
    };
    if plx_data::metadata::dv_node_suppressed() {
        #[cfg(feature = "devtriggers")]
        log(&format!(
            "dv: DolbyHdrInfo P{} SUPPRESSED by /tmp/plxnative-dvnonode (direct play kept)",
            n.profile_id
        ));
        return p.to_string();
    }
    if video != "H265" && video != "AV1" {
        // Unreachable by construction — `route` records the DV layering on the direct-play branch
        // only, and that branch's payload codec is the file's own hevc or av1 — so this is the
        // guard that says so out loud rather than declaring Dolby Vision over an H264 elementary
        // stream. A malformed sourceInfo does not fail loudly; it wedges the sink.
        log(&format!(
            "dv: DolbyHdrInfo P{} NOT sent — payload video codec is {video}, not H265 or AV1",
            n.profile_id
        ));
        return p.to_string();
    }
    let anchor = r#""provider":"plxnative""#;
    if !p.contains(anchor) {
        log("dv: payload has no provider anchor — DolbyHdrInfo NOT spliced");
        return p.to_string();
    }
    let node = format!(
        r#","DolbyHdrInfo":{{"trackType":"{}","encryptionType":"{}","profileId":{}}}"#,
        n.track_type, n.encryption_type, n.profile_id
    );
    // ONE line, and it is the answer to "what did we actually send" — the only place the emitted
    // values exist as fact rather than as intent.
    log(&format!(
        "dv: sourceInfo contents.DolbyHdrInfo profileId={} trackType={} encryptionType={} (codec {video})",
        n.profile_id, n.track_type, n.encryption_type
    ));
    p.replace(anchor, &format!("{anchor}{node}"))
}

/// `"appId":"<this install's id>"` — the payload key, and the anchor `with_window_id` splices onto.
///
/// One expression, called from both places, so the two cannot disagree.
fn app_id_key() -> String {
    format!(r#""appId":"{}""#, plx_base::paths::app_id())
}

/// Substitute the `@APPID@` placeholder with the id of the install this process actually is.
///
/// Exactly once per payload, asserted at build time: a `replace` that matched twice would leave a
/// second `appId` key for `with_window_id` to splice a duplicate `windowId` onto, and one that
/// matched nothing would hand LG's JSON parser the placeholder as a literal app id. Both are
/// silent — the pipeline reports no error for either — which is why this is graded in the host
/// suite rather than left to be noticed on a television.
fn with_app_id(p: &str) -> String {
    p.replace("@APPID@", plx_base::paths::app_id())
}

/// Create the exported window and splice its id into the Load payload — the webOS 5+ binding, in
/// one place.
///
/// **Session-scoped, and that is the point.** The window is created HERE rather than at boot
/// because `teardown` destroys it, and the two ends have to sit at the same level: created once at
/// boot and destroyed per session, the second play would splice nothing and show no picture, in
/// silence. Creating it beside its only consumer also makes the "must exist before Load" ordering
/// locally visible instead of a promise made across two files.
///
/// On webOS 5 this one string IS the video-plane binding: the compositor assigned it to the window
/// we just created, and the pipeline imports that window by name and punches through to it.
/// Everything else about the BUFFERSTREAM payload is unchanged between the eras — which is what
/// both reference implementations show (ss4s compiles the same payload builder for webOS 3, 4 and
/// 5 and swaps only the resource file; Kodi adds this key and nothing else).
///
/// Inserted after `"appId":"…"` because that is a stable sibling inside `option` in every payload
/// variant here — via [`app_id_key`], so the anchor cannot drift from what `with_app_id` composed.
/// A no-op on every webOS 4.x set, where `vp_create_window` returns NULL.
fn with_window_id(mt: &MainThread, p: &str) -> String {
    if sink().window_mode() != super::VP_EXPORTED {
        return p.to_string();
    }
    let id = unsafe { sink().create_window(mt) };
    window_step(WindowStage::ExportedWindowCreate);
    if id.is_null() {
        log("windowId: no exported window — video will not bind");
        return p.to_string();
    }
    let id = unsafe { std::ffi::CStr::from_ptr(id) }.to_string_lossy();
    let anchor = app_id_key();
    if !p.contains(&anchor) {
        log("windowId: payload has no appId anchor — NOT spliced; video will not bind");
        return p.to_string();
    }
    log(&format!(
        "vplane: exported windowId={id} spliced into the Load payload"
    ));
    p.replace(&anchor, &format!(r#"{anchor},"windowId":"{id}""#))
}

/// Plex decimal fps → (value, scale) rational for the Load esInfo. Broadcast rates map to their
/// exact NTSC/film ratios; integer rates to n/1; anything else to milli-fps. None if fps is unknown.
/// The frame rate the pipeline is actually FED: at a playback speed every frame is fed, `rate`
/// times closer together (`player::to_sink_ns`), so a 30p film at 2x arrives at 60. What the Load
/// declares (the envelope's `maxFrameRate`, `videoFpsValue`/`videoFpsScale`) must be this, not the
/// file's: newer sets allocate the decoder against the declaration.
fn fed_fps(fps: f64, rate_permille: i32) -> f64 {
    fps * rate_permille as f64 / 1000.0
}

/// [`fps_rational`] of the fed rate, scaled exactly: 24000/1001 at 1.5x is 36000/1001.
fn fed_fps_rational(fps: f64, rate_permille: i32) -> Option<(i64, i64)> {
    let (num, den) = fps_rational(fps)?;
    let (num, den) = (num * rate_permille as i64, den * 1000);
    let g = gcd(num, den);
    Some((num / g, den / g))
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a.abs().max(1) } else { gcd(b, a % b) }
}

fn fps_rational(fps: f64) -> Option<(i64, i64)> {
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

/// Start one native Engine and settle any route candidate which was prepared before (or, for a
/// dev fixture, during) this call.  A pre-existing Engine is a successful no-op only when there is
/// no candidate waiting for a *new* Load; otherwise accepting it would falsely settle the new
/// route with the old decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferfeedStartOutcome {
    AlreadyRunning,
    Launched(crate::route::RouteStartAttempt),
    /// The previous object's `Unload` is still running: the start is queued in the release
    /// machine and runs the frame its tail has run (or fails typed at the deadline). Accepted:
    /// the route stays on the Player and the HUD shows Connecting.
    Deferred,
    Failed,
}

impl BufferfeedStartOutcome {
    pub fn accepted(self) -> bool {
        !matches!(self, Self::Failed)
    }
}

pub fn start_bufferfeed(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) -> bool {
    let outcome = start_bufferfeed_tracked(ps, pa);
    ps.jail_load_blocked || outcome.accepted()
}

/// Start the native Engine while retaining the identity of the exact asynchronous `sf_load`.
/// Foreground recovery uses this to wait for the media-thread result instead of treating thread
/// creation as proof that the television accepted the payload.
pub fn start_bufferfeed_tracked(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) -> BufferfeedStartOutcome {
    if plx_platform::tv::sandbox::blocks_native_video() {
        ps.jail_load_blocked = true;
        log("start_bufferfeed: refusing native video; affected chassis cannot read /dev/rtkmem");
        if let Some(ticket) = crate::route::begin_route_start() {
            let _ = crate::route::abort_route_start(ticket, crate::route::RouteStartResult::StartFailed);
        }
        return BufferfeedStartOutcome::Failed;
    }
    ps.jail_load_blocked = false;
    ps.native_start_refused = None;
    // A fresh play (not a `reload_at`, which reaches `start_bufferfeed_for` directly) is a new
    // playback: it gets to try the native Seek again.
    if pa.engine().is_none() {
        super::set_native_seek_refused(false);
    }
    if let Some(existing) = pa.engine().map(|engine| engine.route_start) {
        match crate::route::classify_live_engine_start(existing) {
            crate::route::LiveEngineStartRelation::CurrentAttempt => {
                // The exact Load is already in flight. Return its identity so foreground can
                // observe that same attempt instead of treating an idempotent rediscovery as an
                // untracked Engine and entering an endless AlreadyRunning retry.
                log("start_bufferfeed: following already-running Load attempt");
                return BufferfeedStartOutcome::Launched(existing);
            }
            crate::route::LiveEngineStartRelation::NoPendingRoute => {
                log("start_bufferfeed: already running (no-op)");
                return BufferfeedStartOutcome::AlreadyRunning;
            }
            crate::route::LiveEngineStartRelation::Conflict(ticket) => {
                log("start_bufferfeed: live Engine belongs to another Load attempt");
                let _ = crate::route::abort_route_start(
                    ticket,
                    crate::route::RouteStartResult::StartFailed,
                );
                return BufferfeedStartOutcome::Failed;
            }
        }
    }
    let Some(route_start) = crate::route::begin_route_start() else {
        log(&format!(
            "start_bufferfeed: route reducer refused a start owner (phase={})",
            crate::route::control_phase_label()
        ));
        return BufferfeedStartOutcome::Failed;
    };
    let outcome = start_bufferfeed_for(ps, pa, route_start);
    // The refusal for an outstanding abandoned Load builds no Engine, so no pump will ever publish
    // why the player is empty. Record it where `player::state` derives the read-out from; the start
    // itself stays refused.
    if outcome == BufferfeedStartOutcome::Failed && pa.has_abandoned_load() {
        ps.native_start_refused = Some(crate::route::StartRefusal::LoadTimeout);
    }
    outcome
}

/// Builds the "stream: <origin> path=<path>" diagnostic line for the freshly opened
/// transcode/direct-stream URL. `plx_base::eventlog::log()` runs every line through
/// `eventlog::scrub::scrub_local()`, which already redacts `X-Plex-Token=` (and other
/// credential query params) by value only, stopping at the next `&` — so the token,
/// which `Client::with_token` always appends last, never reaches the log. Truncating
/// `path` here on top of that is redundant, and for a burn transcode URL it cuts the
/// line off before `subtitleStreamID=`/`subtitles=burn`, which is exactly the evidence
/// `tests/run.py`'s `op_audio_enhancement_burn` grader looks for.
fn stream_open_log_line(origin_log_form: &str, path: &str) -> String {
    format!("stream: {origin_log_form} path={path}")
}

fn start_bufferfeed_for(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    route_start: crate::route::RouteStartTransaction,
) -> BufferfeedStartOutcome {
    // The last real stop's object may still be inside its off-frame `Unload`. A tail that is ready
    // runs now; one that is not defers this start into the release machine (never a blocking join:
    // PLX-NATIVE-1Q), which starts it the frame the tail has run or refuses it at the deadline.
    if pa.release_busy() {
        poll_release(ps, pa, std::time::Instant::now());
    }
    let mut fx = Vec::new();
    pa.release_step(
        &super::release::ReleaseEvent::StartRequested(super::release::QueuedStart { route_start }),
        &mut fx,
    );
    let mut go = false;
    let mut refused = false;
    for effect in fx {
        match effect {
            super::release::ReleaseEffect::Start(_) => go = true,
            super::release::ReleaseEffect::RefuseStart(q) => {
                refuse_start_for_release(ps, q);
                refused = true;
            }
            super::release::ReleaseEffect::AbortSuperseded(q) => {
                pa.settle_preview_admission(false, false);
                pa.queued_start_dropped();
                let _ = crate::route::abort_route_start(q.route_start, crate::route::RouteStartResult::StartFailed);
            }
            _ => {}
        }
    }
    if refused {
        return BufferfeedStartOutcome::Failed;
    }
    if !go {
        log("start_bufferfeed: the previous Unload is still running; start deferred");
        return BufferfeedStartOutcome::Deferred;
    }
    match start_bufferfeed_inner(ps, pa, route_start) {
        Ok(attempt) => {
            // Every start funnels through here (fresh, reload and the rollback's reload alike), so
            // the refusal's read-out ends at the first start that is NOT refused. Clearing it only
            // in `start_bufferfeed_tracked` left a reload landing after a refused fresh start
            // (the foreground restore's rollback) with an Error over the video it began.
            ps.native_start_refused = None;
            // The request's failed-open ladder rules on this Engine's open too: a Load it did not
            // ask for (the foreground restore after a Home during a recovery flight or over the
            // read-out) takes it out of `Flight` or `Failed`. The recovery landing's own reload has
            // already moved it (`FlightLanded`), so this is a no-op there.
            let _ = ps.open_recovery().step(&super::recovery::RecoveryEvent::LoadStarted, &mut Vec::new());
            BufferfeedStartOutcome::Launched(attempt)
        }
        Err(result) => {
            let _ = crate::route::abort_route_start(route_start, result);
            BufferfeedStartOutcome::Failed
        }
    }
}

fn start_bufferfeed_inner(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    route_start: crate::route::RouteStartTransaction,
) -> Result<crate::route::RouteStartAttempt, crate::route::RouteStartResult> {
    // The previous object's release is finished or the start was deferred before this point
    // (`start_bufferfeed_for`), so the epoch is retired and the object destroyed already.
    // A timed-out Load from an earlier Engine may still own the one native object the C seam
    // holds. Release it now if it has returned; otherwise refuse before anything below (the
    // exported window in particular) can touch state that object is still using.
    reap_abandoned_load(pa);
    if pa.has_abandoned_load() {
        log("start_bufferfeed: an abandoned Load has not returned yet (refusing native start)");
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    // Guard a double-start: overwriting a live ENGINE slot would DROP the running
    // Engine, detaching its worker threads and freeing the hs/aq boxes those
    // threads still hold raw ptrs into -> use-after-free. If already running, no-op.
    // (Reachable via a PLAY key landing in the WILL->DID foreground window.)
    // Per-SESSION, not per-boot: both the log's every-100th cadence and the diagnostics read-out
    // mean "this playback", and a count carried in from the last item answers neither question.
    VTOT.store(0, Ordering::Relaxed);
    ATOT.store(0, Ordering::Relaxed);
    VATT.store(0, Ordering::Relaxed);
    AATT.store(0, Ordering::Relaxed);
    super::native_stall::reset();
    // resolve the URL, in precedence order: route (a selected movie) wins, then
    // /tmp/plxnative-playurl (a URL + its declaration), then /tmp/plxnative-url (a URL
    // alone), then a local sample.
    let mut url = crate::route::url(ps);
    if url.is_empty() {
        // dev: /tmp/plxnative-playurl — a URL *and the Load declaration to play it with*, with no
        // library item behind it. This is the player-PIPELINE test tier's entry (tests/README.md):
        // `plxnative-url` below hands over a URL only, which leaves the payload describing
        // whatever the route happened to hold — an empty string, i.e. H264 + "AC3" — so a 4K HEVC
        // or a Dolby file could never be declared honestly without Plex. Read HERE rather than at
        // the app.rs entry point because the declaration has to be in `route` before the payload
        // is composed a few lines below, and because a reload (a seek that escalates) comes back
        // through this same function and must re-apply it.
        //
        // `route::url()` still wins: a real selection is never overridden by a stale trigger.
        match crate::player::playurl::playurl() {
            Some(Ok(p)) => {
                url = install_synthetic_playurl(ps, &p)?;
                if let Some([w, h]) = p.source_raster {
                    crate::route::set_stream_source_raster(ps, w, h);
                }
                if p.auto_source_kbps > 0 && !p.auto_hls_base.is_empty() {
                    // A fixture that starts in HLS hands back the playlist to open: the route's
                    // own `url` has already moved, and this local copy is what everything below
                    // reads.
                    if let Some(hls) = crate::route::arm_auto_fixture(
                        ps,
                        &p.url,
                        p.auto_source_kbps,
                        &p.auto_hls_base,
                        p.auto_start_hls,
                        p.source_raster.map_or((1_920, 1_080), |[w, h]| (w, h)),
                    ) {
                        url = hls;
                    }
                }
            }
            // Log and fall through rather than refuse: the next candidate may well be armed. The
            // harness grades this as a failure anyway, because the `load:` line below will not say
            // what the case expected.
            Some(Err(e)) => log(&format!("playurl: unreadable spec ({e}) — ignored")),
            None => {}
        }
    }
    if url.is_empty() {
        if let Some(t) = plx_base::devtrig::read("url") {
            if !t.is_empty() {
                url = t;
                crate::route::set_url(ps, &url);
            }
        }
    }
    let mut sample: Option<Box<SampleBuf>> = None;
    let mut is_h265 = false;
    if url.is_empty() {
        if let Some(data) = plx_base::devtrig::read_sample("sample.h264") {
            let au = bf_split(&data, 0x09);
            log(&format!(
                "bf_split h264: {} AUs in {} bytes",
                au.len(),
                data.len()
            ));
            if au.len() < 2 {
                return Err(crate::route::RouteStartResult::StartFailed);
            }
            sample = Some(Box::new(SampleBuf {
                data,
                au,
                next: 0,
                loops: 0,
            }));
        } else if let Some(data) = plx_base::devtrig::read_sample("sample.h265") {
            // Phase 0 probe: feed a local HEVC Annex-B sample to test native HEVC decode.
            let au = bf_split(&data, 0x46);
            log(&format!(
                "bf_split h265: {} AUs in {} bytes",
                au.len(),
                data.len()
            ));
            if au.len() < 2 {
                return Err(crate::route::RouteStartResult::StartFailed);
            }
            is_h265 = true;
            sample = Some(Box::new(SampleBuf {
                data,
                au,
                next: 0,
                loops: 0,
            }));
        } else {
            // nothing to play: no selected item, no /tmp/plxnative-url, no local sample. (The old
            // baked-in demo-movie fallback is gone — the binary carries no URLs/credentials.)
            #[cfg(feature = "devtriggers")]
            log("start_bufferfeed: no URL — select an item (or set /tmp/plxnative-url)");
            return Err(crate::route::RouteStartResult::NoRoute);
        }
    }
    if !crate::route::prepare_route_start(route_start) {
        log("start_bufferfeed: prepared route no longer owns the transaction");
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    let stream = sample.is_none();
    // For a streamed direct-play/transcode, pick the Load codecs from the item: video H264 vs
    // H265 (native HEVC direct-play), audio AC3/EAC3/AAC/DTS. (The local sample paths keep their
    // fixed payloads.)
    // dev A/B: /tmp/plxnative-noaudio feeds video only (needAudio:false + skip es=2) to isolate
    // whether the audio ES (E-AC3/Atmos) is what stalls the sink on 4K HEVC.
    let no_audio = super::no_audio_armed();
    crate::ff::set_feed_audio(!no_audio);
    let stream_payload;
    // Every arm below assigns this — the static payloads through `static_envelope`, the streamed
    // one through `sink_envelope_now` — so a new arm that forgets it fails to compile.
    let sink_env: SinkEnvelope;
    // The LG-side audio name the payload ended up carrying, hoisted so the `load:` line below can
    // report it. "-" is the video-only case, where there is no audio ES to name.
    let mut audio_declared: &str = "-";
    // ...and the plan for the sink lead that declaration carries (`load_audio`), none for a
    // compressed or absent track.
    let mut audio_lead = PcmLead::Off;
    // ...and the video name beside it, for the same reason and to keep the codec->name mapping
    // written ONCE (`LoadVideo`): the `load:` line below would otherwise re-read the route and
    // re-derive it.
    let mut video_declared: &str = "-";
    // Playback speed for THIS Load (`player::session_rate_for`), published before the demuxer is
    // spawned: it reads it to decide whether to decode the audio, and every timestamp crossing to
    // the pipeline is scaled by it (`player::to_sink_ns`). After any `reset_session`, which puts
    // it back to 1x.
    let session_rate = if stream { super::session_rate_for(ps) } else { super::RATE_NORMAL };
    SHARED.applied_rate_permille.store(session_rate, Ordering::Relaxed);
    if session_rate != super::RATE_NORMAL {
        log(&format!("speed: this Load plays at {session_rate} (audio decoded in the app, fed as PCM)"));
    }
    let payload_str: &str = if stream {
        let stream_vcodec = crate::route::stream_vcodec(ps);
        let Some(lv) = LoadVideo::of_stream(&stream_vcodec) else {
            log(&format!(
                "start_bufferfeed: the pipeline cannot feed video codec {stream_vcodec:?}; refusing the Load instead of declaring it H.264"
            ));
            SHARED.fail_demux(DemuxExitClass::LoadRefused);
            return Err(crate::route::RouteStartResult::StartFailed);
        };
        if load_video_refused(lv, &plx_platform::devcaps::caps()) {
            log(&format!(
                "start_bufferfeed: {} stream but the device table lists no such decoder; refusing the Load",
                lv.payload_name()
            ));
            SHARED.fail_demux(DemuxExitClass::LoadRefused);
            return Err(crate::route::RouteStartResult::StartFailed);
        }
        video_declared = lv.payload_name();
        // Record what the payload ACTUALLY says, for the diagnostics read-out — including the
        // video-only case, where `dg_load_a == 0` is the whole explanation for silence.
        SHARED.dg_load_v.store(lv.diag_code(), Ordering::Relaxed);
        if no_audio {
            SHARED.dg_load_a.store(0, Ordering::Relaxed);
            // A static payload declares its own envelope; say so on the `load:` line.
            sink_env = static_envelope(lv);
            match lv {
                LoadVideo::H264 => PAYLOAD_V,
                LoadVideo::H265 => PAYLOAD_H265,
                LoadVideo::Av1 | LoadVideo::Vp9 => {
                    stream_payload = renamed_static_payload(lv);
                    stream_payload.as_str()
                }
            }
        } else {
            // LG's pipeline names E-AC3 "AC3 PLUS" (Dolby Digital Plus), NOT "EAC3" — the
            // wrong string leaves the audio ES unconfigured, and with audioSync the video
            // sink slaves to the dead audio clock and stalls (verified: video-only plays).
            let codec = crate::route::stream_acodec(ps);
            // App-decoded audio: the Load must state the PCM shape, and the demuxer must produce
            // exactly that. Missing facts refuse the Load — a guessed rate plays at the wrong pitch.
            let pcm = match app_pcm_for_rate(session_rate, crate::route::stream_pcm(ps)) {
                Some(Ok(f)) => Some(f),
                Some(Err(why)) => {
                    log(&format!("start_bufferfeed: PCM audio refused — {why}"));
                    SHARED.demux_failed.store(true, Ordering::Release);
                    return Err(crate::route::RouteStartResult::StartFailed);
                }
                None => None,
            };
            // Dev trigger `pcmavoffset=<ms>` (debug builds, read per Load): how the lead is measured
            // on an output the default was not filmed on.
            let lead_override = plx_base::devtrig::read("pcmavoffset");
            let Some(audio) = load_audio(
                &codec, session_rate, pcm.as_ref(), lead_override.as_deref(), plx_platform::devcaps::audio_stack(),
            ) else {
                log(&format!("start_bufferfeed: unsupported audio codec {codec:?}; refusing incorrect Load declaration"));
                SHARED.fail_demux(DemuxExitClass::LoadRefused);
                return Err(crate::route::RouteStartResult::StartFailed);
            };
            crate::ff::set_pcm_out(
                pcm.map(|f| crate::ff_pcm::PcmOut {
                    rate: f.sample_rate, bits: f.bits, channels: f.channels, source_rate: f.source_rate,
                }),
                pcm.is_some() && crate::route::stream_pcm_is_lossy(ps),
            );
            if let Some(f) = pcm {
                log(&pcm_decode_line(&codec, &f));
            }
            SHARED.dg_load_a.store(audio.diagnostic, Ordering::Relaxed);
            // App-decoded audio (`pcm`) and a speed session (`ff::SpeedDecoder`) both declare PCM;
            // `load_audio` has already put that into the declared name and the lead plan with it.
            let ac = audio.name;
            audio_declared = ac;
            audio_lead = audio.lead;
            // A lead that follows the sound output is read and logged by the Load thread, which may
            // block on the bus (`resolve` asks nothing for the other plans); the dev override, a
            // stack whose lead is the same on every output (`Fixed`) and a stack with no lead
            // (`NoLead`) are settled here and say so now.
            if let Some(line) = audio_lead.start_log_line() {
                log(&line);
            }
            // The sink envelope — `adaptiveStreaming`'s maxWidth/maxHeight/maxFrameRate — used
            // to be the panel max (4K60) for every codec and every source, on the reasoning that
            // the pipeline reads the true dims from the SPS and the block is "just a ceiling".
            // True on the dev set (webOS 4.10); MEASURED FALSE on 10.3.1, which allocates against
            // the declaration and refuses every H.264 Load at 4K60 (`smp_cb type=18 num=601`) —
            // i.e. every server transcode. `sink_envelope` is the rule now, and it is deliberately
            // no wider than what was measured: `docs/webos10-resource-allocation.md`.
            // `lv` is the STREAM's declared codec; `is_h265` above belongs to the local-sample
            // path and is false here for every streamed HEVC playback (device-measured 2026-09-03:
            // three HEVC cells declared FHD before this read the right flag).
            sink_env = sink_envelope_now(ps, lv);
            let av = build_av_payload(ps, lv.payload_name(), ac, sink_env);
            stream_payload = if ac == "OPUS" {
                let channels = crate::route::stream_achannels(ps);
                let Some(p) = with_opus_info(&av, channels) else {
                    log(&format!("start_bufferfeed: no Opus layout for {channels} channel(s); refusing the Load"));
                    SHARED.demux_failed.store(true, Ordering::Release);
                    return Err(crate::route::RouteStartResult::StartFailed);
                };
                log(&format!("start_bufferfeed: opusInfo channels={channels}"));
                p
            } else {
                av
            };
            &stream_payload
        }
    } else if is_h265 {
        sink_env = static_envelope(LoadVideo::H265);
        PAYLOAD_H265
    } else {
        sink_env = static_envelope(LoadVideo::H264);
        PAYLOAD_V
    };
    // The windowId splice happens HERE, at the single point every payload variant passes through,
    // rather than inside build_av_payload — three of the four variants are static strings that
    // never see the builder, and a binding that works only for streamed A/V would be the kind of
    // bug that reproduces on some content and not others.
    // ...and the appId substitution happens here for the same reason, and BEFORE the splice —
    // `with_window_id`'s anchor is the composed `"appId":"<id>"`, so the placeholder has to be
    // gone by then.
    let payload_c = std::ffi::CString::new(with_window_id(pa.mt(), &with_app_id(payload_str))).unwrap();
    if stream {
        // What the payload ACTUALLY declared, once, in the event log. Not test scaffolding: until
        // this line, the only answer to "what did we tell the television this stream was" lived in
        // the on-screen diagnostics read-out (`dg_load_v`/`dg_load_a` above), i.e. nowhere a log
        // could settle it — and the `"AC3 PLUS"` renaming a few lines up is exactly the kind of
        // thing a log should be able to settle, since getting it wrong leaves the audio ES
        // unconfigured and stalls the video sink through audioSync. Streamed only: the two static
        // sample payloads declare nothing chosen and would be claiming a declaration they never
        // consulted. Carries no URL and no token, and costs one line per playback.
        let dv = crate::route::stream_dovi(ps);
        log(&format!(
            "load: v={} a={:?} fps={:.3} dv=present:{} P{}/{} el:{} atmos:{} max={}x{}@{}",
            video_declared,
            audio_declared,
            crate::route::stream_fps(ps),
            dv.present as i32,
            dv.profile,
            dv.bl_compat,
            dv.el_present as i32,
            crate::route::stream_immersive(ps) as i32,
            sink_env.w,
            sink_env.h,
            sink_env.fps
        ));
    }

    // The native object starts stopped. Publish that physical fact before either worker can arm an
    // ABR trial or observe a user command; the guard rolls it back on every construction failure.
    let clock_start = match ClockStartGuard::arm() {
        Some(guard) => guard,
        None => {
            log("start_bufferfeed: clock state is not idle (refusing overlapping start)");
            return Err(crate::route::RouteStartResult::StartFailed);
        }
    };
    let native_start = match NativeSessionStartGuard::arm() {
        Some(guard) => guard,
        None => {
            log("start_bufferfeed: native session is not idle (refusing overlapping Load)");
            return Err(crate::route::RouteStartResult::StartFailed);
        }
    };
    let Some(route_attempt) = crate::route::claim_route_start_attempt(route_start) else {
        log("start_bufferfeed: prepared route lost before native construction");
        return Err(crate::route::RouteStartResult::StartFailed);
    };

    // fd = -1 (CLOSED) so a teardown before/without http_open doesn't close(0)
    let mut hs = plx_net::stream::http_stream_boxed();
    let mut aqv_box: Option<Box<AuQueue>> = None;
    let mut aqa_box: Option<Box<AuQueue>> = None;
    let mut stream_th = None;
    let source;
    // Whether the side subtitle reader will run for this engine run, decided BEFORE the demuxer is
    // spawned: the demuxer reads `side_subs_owner` once, as it opens the stream, and must never
    // also open subtitle decoders or ASS sources the reader replaces.
    let side_target = if stream { crate::route::side_reader_target(ps) } else { None };

    if stream {
        let su = plx_plex::plex::StreamUrl::parse(&url); // the typed layer's URL splitter
                                                      // **The whole ORIGIN goes down, not a `(host, port)` pair, because the SCHEME chooses the
                                                      // transport**: `ff::demux` reads http through `plx_net::stream`'s cleartext socket and https
                                                      // through `crate::curlio`. This used to REFUSE an https origin outright — cleartext to a
                                                      // TLS port is a hang or a garbage response with nothing in the log — and that refusal is
                                                      // what a remote QA reviewer, with no PMS on their LAN, would have hit on every Play.
                                                      // Rebuilding the origin from an address would put the refusal back in a subtler form: the
                                                      // certificate is issued for the `plex.direct` NAME, so a TLS connection to the dotted quad
                                                      // behind it fails validation however well the packets flow (`net/origin.rs`).
        if !plx_plex::http::credential_transport_allowed(&su.origin, &su.path, &[]) {
            plx_base::eventlog::log("stream: refused insecure credential transport");
            return Err(crate::route::RouteStartResult::StartFailed);
        }
        let path = su.path;
        log(&stream_open_log_line(&su.origin.log_form(), &path));
        let origin = su.origin;
        // Two-lane feed: the demuxer routes es=1 video to aq_video and es=2 audio to
        // aq_audio, each with its own cap + feeder.
        let mut qv = crate::aq::aq_new(AQ_VIDEO_BYTES);
        let mut qa = crate::aq::aq_new(AQ_AUDIO_BYTES);
        let aqv_raw = &mut *qv as *mut AuQueue;
        let aqa_raw = &mut *qa as *mut AuQueue;
        let hs_raw = &mut *hs as *mut HttpStream;
        SHARED.hs_ptr.store(hs_raw, Ordering::Release);
        {
            let aqp = threads::SendPtr(aqv_raw);
            let aqap = threads::SendPtr(aqa_raw);
            let hsp = threads::SendPtr(hs_raw);
            // The Load payload's audio codec, captured BY VALUE here on the main thread.
            // `ff::demux` used to call `route::stream_acodec()` from the demux thread, cloning
            // a `static mut String` that main-thread route transitions and native audio switches
            // reassign — a data race, and a use-after-free if the reassignment dropped the old
            // buffer mid-clone.
            // Capturing is free: every one of those writers is followed by
            // `teardown(true) + start_bufferfeed()` (reload_at / reload_transcode /
            // switch_audio_native), which respawns this thread with the new value.
            let acodec = crate::route::stream_acodec(ps);
            let abr = crate::route::hls_abr_control(ps);
            let auto_original = crate::route::auto_original_watch(ps);
            // Before the demuxer exists, so what it publishes is this session's.
            crate::ff::native_audio::reset();
            SHARED.side_subs_owner.store(side_target.is_some(), Ordering::Release);
            stream_th = plx_base::task::spawn_off_frame_keeping("demux", move |off| {
                crate::ff::demux(off, origin, path, acodec, abr, auto_original, aqp, aqap, hsp)
            });
            if stream_th.is_none() {
                SHARED.side_subs_owner.store(false, Ordering::Release);
                // Nothing will ever fill the AU queues, so there is no session to start. `hs` is
                // about to drop with this early return, so retract the pointer first — the pump
                // and teardown both read it straight off SHARED.
                SHARED.hs_ptr.store(std::ptr::null_mut(), Ordering::Release);
                return Err(crate::route::RouteStartResult::StartFailed);
            }
        }
        aqv_box = Some(qv);
        aqa_box = Some(qa);
        source = Source::Stream;
    } else {
        source = Source::Sample(sample.unwrap());
    }

    // the media thread constructs + loads + runs the loop (owns the GMainContext)
    let payload_ptr = threads::SendPtr(payload_c.as_ptr() as *mut c_char);
    let native_epoch = native_start.epoch();
    let load_th = plx_base::task::spawn("media", move || {
        threads::load_thread(payload_ptr, native_epoch, Some(route_attempt), audio_lead)
    });
    if load_th.is_none() {
        // Without the media thread nothing is ever Loaded and nothing drains the AU queues, so the
        // demuxer would park in aq_push forever holding raw pointers into locals this return is
        // about to drop. Stop it exactly the way teardown's stream branch does — abort both lanes,
        // shutdown to wake the recv, join, and only then the real close.
        for q in [aqv_box.as_mut(), aqa_box.as_mut()].into_iter().flatten() {
            crate::aq::aq_abort(&mut **q);
        }
        let p = SHARED.hs_ptr.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if !p.is_null() {
            plx_net::stream::http_shutdown(p);
        }
        super::subside::stop("start-failed"); // before the demux join, like teardown
        crate::curlio::abort_active(); // the https demuxer's equivalent — see teardown
        if let Some(t) = stream_th.take() {
            plx_base::task::join("demux", t);
        }
        if !p.is_null() {
            plx_net::stream::http_close(p); // sole owner now: the reader is joined
        }
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    // The side subtitle reader (dev trigger `plxnative-subside`), started once the demuxer is
    // spawned and the media thread exists: it waits for the demuxer's first keyframe before it
    // reads anything. (Its ownership flag was set before the demux spawn, below.)
    if let Some(target) = side_target {
        start_side_reader(&target);
    }

    // progress reporter: post the play position to /:/timeline (updates resume + watched).
    // The complete report projection and Engine epoch are captured atomically now. The reporter
    // never reads route::Session: a stale lease stops at teardown, while active encoder changes
    // remain synchronized with the projection under PlayerControl.
    let report_stop = threads::ReportStop::new();
    // `begin_timeline_reporting` is the gate: it refuses any session resolved for a preview.
    let report_th = if stream {
        if let Some(lease) = crate::route::begin_timeline_reporting(ps) {
            // best-effort: refused, the only loss is that the resume point stops being posted
            let st = report_stop.clone();
            plx_base::task::spawn("timeline", move || threads::timeline_thread(lease, st))
        } else {
            None
        }
    } else {
        None
    };

    let (lead_at_start, lead_pending) = audio_lead.at_engine_start();
    let eng = Engine {
        route_start: route_attempt,
        native_epoch,
        stage: Stage::Loading,
        video_info_sent: false,
        bound_at_frames: None,
        placed_src: (0, 0),
        eos_pushed: false,
        // if a seek is armed for the FIRST open (resume, or reload_at), rebase the first
        // post-seek keyframe to fed-pts 0 so the pipeline sees a 0-based timeline identical
        // to fresh play (disp_base carries the content offset). Plain fresh play leaves this
        // false (first keyframe is already ~0).
        rebase_pending: SHARED.seek_to_ns.load(Ordering::Relaxed) >= 0,
        rebase_drops: 0,
        seek_armed_at: 0,
        seek_retries: 0,
        flushed: false,
        presentation_rearm_pending: false,
        max_fed_video_pts: 0,
        max_fed_audio_pts: 0,
        audio_start_fp: None,
        pcm_sink_lead_ns: lead_at_start,
        pcm_lead_pending: lead_pending,
        feed_pace: FeedPace::default(),
        seek_base_pts: 0,
        prime_play: false,
        aq_video: aqv_box,
        aq_audio: aqa_box,
        hs,
        pending_video: None,
        pending_audio: None,
        payload: payload_c,
        source,
        stream_th,
        load_th,
        report_th,
        report_stop: Some(report_stop),
        preview_abandon: false,
        foreground_follows_load: false,
        era_watch: super::era_callbacks::Watch::new(),
    };
    // Select this session's direct-play seek mode (`SeekMode`, mod.rs, whose SCOPE note argues why
    // it is per session). `feed_stream` can downgrade it to `Reload` mid-session (the pipeline was
    // unreachable, or LG refused a `Seek`), and that is a fact about the StarfishMediaAPIs object
    // — which `sf_load` constructs and teardown's `sf_destroy` destructs, once per session — not
    // about the TV, so it is re-selected here for every session. Placed HERE rather than at the
    // top of this function on purpose: the `engine_is_live` no-op return above must NOT re-select,
    // or a stray PLAY landing mid-session would cancel a downgrade that is still needed.
    let major = plx_platform::tv::device::info().major;
    let layout_known = inplace_layout_known(major, sink().feed_abi_is_cow());
    let old_abi_seek = old_abi_seek_tested(major);
    let mode = session_seek_mode(layout_known, old_abi_seek, sink().window_mode(), dev_seek_mode());
    log(&format!("seek mode: {mode:?} (layout_known={layout_known} old_abi_seek={old_abi_seek})"));
    super::set_seek_mode(mode);
    pa.install(eng);
    native_start.commit();
    clock_start.commit();
    TX.started.store(true, Ordering::Relaxed);
    log(&format!(
        "SMP: media thread spawned, stream={}",
        stream as i32
    ));
    Ok(route_attempt)
}

/// Validate and publish the no-Plex synthetic route as one admission step. The URL must remain
/// absent when declaration policy refuses it: a later PLAY uses URL presence to decide whether to
/// revisit this validation, so publishing first turns a one-time refusal into a retry bypass.
fn install_synthetic_playurl(
    ps: &mut crate::route::PlaybackSession,
    play: &crate::player::playurl::PlayUrl,
) -> Result<String, crate::route::RouteStartResult> {
    if !crate::route::set_stream_declaration(
        ps,
        &play.vcodec,
        &play.acodec,
        play.fps,
        play.dovi.to_dovi(),
        play.atmos,
    ) {
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    let url = play.url.clone();
    crate::route::set_url(ps, &url);
    Ok(url)
}

/// Arm the demuxer to open+seek to `target_ns` on the NEXT Load, displaying honest content
/// time. disp_base=0 and (via start_bufferfeed) rebase_pending=true, so feed_stream rebases
/// the landed keyframe K to fed-pts 0 and the presented position reads as num+K = content
/// time. Call BEFORE start_bufferfeed (resume) or via reload_at (mid-play seek).
pub fn arm_seek(target_ns: i64) {
    let t = target_ns.max(0);
    SHARED.seek_to_ns.store(t, Ordering::Release);
    SHARED.disp_base.store(0, Ordering::Relaxed);
    SHARED.playpos_ns.store(t, Ordering::Relaxed); // instant HUD feedback until frames land
}

/// Resume/seek AT the first Load. A direct-play item seeks the demuxer (av_seek via arm_seek).
/// A TRANSCODE item's stream is 0-based and NOT seekable (no byte-index, Content-Length=-1), so
/// av_seek fails — instead restart the encode at `&offset=secs` and display content time via
/// disp_base. Call BEFORE start_bufferfeed, AFTER route::play_movie has run the decision (so the
/// transcode session + flavor are set). Used for viewOffset resume.
///
/// **There is one entry point, [`begin_resume`] / [`land_resume`], and it is a flight** (the cold
/// resume of a freshly resolved play, and the foreground restore of a parked one): a transcode's
/// rebuild at the saved position is planned on the frame thread and its PMS half runs on a worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "resume preparation is an external route effect and must be settled"]
pub enum ResumeOutcome {
    Prepared,
    NoRoute,
    RebuildRejected,
}

/// What [`begin_resume`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "a pending resume owes a start when it lands"]
pub enum ResumeStart {
    /// Decided on the spot (no rebuild is owed): the caller starts the Load (or fails the start)
    /// now.
    Settled(ResumeOutcome),
    /// A flight owns the rebuild at the saved position (`route::dispatch_resume_rebase`): the start
    /// stays owed, `player::state` answers `Resolving` (via `route::resume_flight_outstanding`), and [`land_resume`] delivers the
    /// outcome the caller starts the Load on.
    Pending,
}

/// The part of a resume that never reaches PMS.
enum ResumePrelude {
    Settled(ResumeOutcome),
    /// A transcode with a saved position: its encode must restart at the offset.
    Rebuild,
}

fn resume_prelude(ps: &mut crate::route::PlaybackSession, resume_ns: i64) -> ResumePrelude {
    if resume_ns <= 0 {
        return ResumePrelude::Settled(ResumeOutcome::Prepared);
    }
    if crate::route::url(ps).is_empty() {
        return ResumePrelude::Settled(ResumeOutcome::NoRoute);
    }
    if !crate::route::is_transcoding(ps) {
        arm_seek(resume_ns); // direct-play: av_seek the file at the first open
        return ResumePrelude::Settled(ResumeOutcome::Prepared);
    }
    ResumePrelude::Rebuild
}

/// The transcode half of a resume, once its encode restarts at the offset (0-based): `disp_base`
/// carries the offset so the HUD reads content time.
fn note_transcode_resumed(resume_ns: i64) {
    SHARED.disp_base.store(resume_ns, Ordering::Relaxed);
    SHARED.playpos_ns.store(resume_ns, Ordering::Relaxed);
    log(&format!(
        "resume(transcode): restart at offset {}s",
        resume_ns / 1_000_000_000
    ));
}

/// Prepare a resume at `resume_ns`: a cold start (a play whose plan just landed) or a foreground
/// restore (the parked session's start transaction is `Stable`/`Prepared`/`Failed`, or an Original
/// trial's own `Prepared` one for a session suspended mid-trial, which the flight flies over without
/// touching the trial's rollback; an `Idle` one has no URL and answers `NoRoute` here, never
/// reaching the rebuild). No Engine exists. Everything but a transcode settles on the spot; a transcode's
/// rebuild at the saved position is a FLIGHT — planned here on the frame thread, the PMS half on a
/// worker — and answers [`ResumeStart::Pending`]. The frame never reaches the server.
pub fn begin_resume(ps: &mut crate::route::PlaybackSession, resume_ns: i64) -> ResumeStart {
    match resume_prelude(ps, resume_ns) {
        ResumePrelude::Settled(outcome) => ResumeStart::Settled(outcome),
        ResumePrelude::Rebuild => match crate::route::dispatch_resume_rebase(ps, resume_ns) {
            crate::route::RecoveryDispatch::Flying { .. } => ResumeStart::Pending,
            // Nothing to rebuild or the worker could not start: a refusal.
            crate::route::RecoveryDispatch::Refused => ResumeStart::Settled(ResumeOutcome::RebuildRejected),
        },
    }
}

/// Drain a pending resume's landing, once a frame while [`crate::route::resume_flight_outstanding`].
/// `None` while the worker is still running, or when the flight went stale (a Back, a teardown, a
/// newer play request: the landing is discarded and its replacement stopped — here, by the stale
/// reap this runs first, since a flight with no Engine has no pump to do it — and whoever ended the
/// flight owns what happens next). `Some` is the outcome of the rebuild: the
/// route is installed and `Prepared` (the caller starts the Load), or the rebuild was refused and
/// the transaction is `Failed`.
pub fn land_resume(ps: &mut crate::route::PlaybackSession) -> Option<ResumeOutcome> {
    crate::route::discard_stale_flight_landing();
    if !crate::route::resume_flight_outstanding() {
        return None;
    }
    let landing = crate::route::take_ready_recovery_flight(ps)?;
    Some(match landing.verdict {
        crate::route::RecoveryVerdict::Install => {
            note_transcode_resumed(landing.offset_ns);
            ResumeOutcome::Prepared
        }
        crate::route::RecoveryVerdict::Refused => ResumeOutcome::RebuildRejected,
    })
}

/// Direct-play seek = tear down the pipeline and start a FRESH Load at `target_ns`. The old
/// flush()+refeed path left a STALE GStreamer segment (decompiled ground truth: the no-arg
/// StarfishMediaAPIs::flush() → CustomPipeline::flush() is a degenerate gst_element_seek to
/// GST_CLOCK_TIME_NONE with NO FLUSH_START/STOP and NO fresh SEGMENT; the HW sink/decoder
/// only re-anchor their segment/basetime on a real SEGMENT/FLUSH event). Post-seek buffers
/// were then scheduled against the pre-seek segment, the sink stopped draining, and the fixed
/// ~14.7 MB of upstream buffers filled in ~48 s → permanent BufferFull + "Playing error". A
/// fresh Load re-establishes a correct segment by construction — the known-good fresh-play
/// path, which never wedges. Heavier than a flush (a ~1 s re-preroll) but correct.
/// Synchronous result of replacing the current native Engine.  `Started` means the new Engine owns
/// a Load; the other states are explicit effect outcomes for the route reducer, not log-only
/// failures which leave an `OriginalTrial` waiting forever.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "a reload is an external state-machine effect and its result must be settled"]
pub enum ReloadOutcome {
    Started,
    NoRoute,
    StartFailed,
}

/// Settle the route-start transaction a reload effect promised but can no longer perform.
///
/// **Observe rather than reserve.** `begin_route_start` MINTS a transaction from a reducer which
/// owns none — from `Stable`, and, since a completed stop now publishes `Idle`
/// ([`crate::route::finish_engine_teardown`]), from `Idle` too — and aborting a transaction no
/// caller ever used would publish `Failed` for a route that never had a `Load` to lose. Every
/// phase that really does own one (`Preparing`/`Prepared`/`Starting`, ordinary or Original) is
/// exactly the set `pending_route_start` returns without minting, so nothing leaks.
fn settle_missing_route_start() {
    if let Some(ticket) = crate::route::pending_route_start() {
        let _ = crate::route::abort_route_start(ticket, crate::route::RouteStartResult::NoRoute);
    }
}

fn prepare_reload_transaction() -> Option<crate::route::RouteStartTransaction> {
    let ticket = crate::route::begin_route_start()?;
    if crate::route::prepare_route_start(ticket) {
        Some(ticket)
    } else {
        log("reload: route start transaction was superseded before teardown");
        None
    }
}

fn reload_start_outcome(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    ticket: crate::route::RouteStartTransaction,
) -> ReloadOutcome {
    if start_bufferfeed_for(ps, pa, ticket).accepted() {
        ReloadOutcome::Started
    } else {
        ReloadOutcome::StartFailed
    }
}

/// Start the side subtitle reader for the live remux this engine run plays, drawing the track
/// the route's target names.
fn start_side_reader(target: &crate::route::SideReaderTarget) {
    let started = super::subside::spec_for(target).is_some_and(super::subside::start);
    if !started {
        log("subside: could not start the reader thread");
        // Nothing reads beside the stream: the demuxer must not leave the subtitle to a reader
        // that is not there.
        SHARED.side_subs_owner.store(false, Ordering::Release);
        super::subside::raise_failure(); // the route falls back to the server's burn
    }
}

fn reload_transcode_start(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, offset_ns: i64) -> Option<BufferfeedStartOutcome> {
    let ticket = prepare_reload_transaction()?;
    log(&format!(
        "reload_transcode: fresh Load at offset {}s",
        offset_ns / 1_000_000_000
    ));
    teardown(ps, pa, true); // keep the session; reload mode
    SHARED.disp_base.store(offset_ns, Ordering::Relaxed); // transcode is 0-based at content=offset
    SHARED.playpos_ns.store(offset_ns, Ordering::Relaxed);
    Some(start_bufferfeed_for(ps, pa, ticket))
}

/// Every target `reload_at` was asked for (test probe, #568): proves a refused native Seek reloads
/// exactly once, at the user's target.
#[cfg(any(test, feature = "test-support"))]
pub(super) static RELOAD_AT_LOG: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());

/// Whether the paused-seek preroll was open as each `reload_at` / `reload_transcode` was entered,
/// before its teardown (test probe): the reload keeps it, so what the fresh session's feed gate and
/// prime see is whatever the dispatcher left open here.
#[cfg(any(test, feature = "test-support"))]
pub(super) static RELOAD_PREROLL_LOG: std::sync::Mutex<Vec<bool>> = std::sync::Mutex::new(Vec::new());

#[cfg(any(test, feature = "test-support"))]
fn note_reload_preroll() {
    RELOAD_PREROLL_LOG.lock().unwrap_or_else(|e| e.into_inner()).push(TX.seek_preroll_active());
}

pub fn reload_at(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, target_ns: i64) -> ReloadOutcome {
    #[cfg(any(test, feature = "test-support"))]
    {
        RELOAD_AT_LOG.lock().unwrap_or_else(|e| e.into_inner()).push(target_ns);
        note_reload_preroll();
    }
    if crate::route::url(ps).is_empty() {
        log("reload_at: no url (ignored)");
        settle_missing_route_start();
        return ReloadOutcome::NoRoute;
    }
    let Some(ticket) = prepare_reload_transaction() else {
        return ReloadOutcome::StartFailed;
    };
    log(&format!(
        "reload_at: fresh Load at {}s",
        target_ns / 1_000_000_000
    ));
    teardown(ps, pa, true); // reload mode: preserve the session (no url-clear / stop-scrobble)
    arm_seek(target_ns);
    reload_start_outcome(ps, pa, ticket)
}

/// NATIVE audio-track switch (direct-play, NO transcode): select the Nth audio stream from the
/// same MKV and reload the direct-play pipeline at the current position (route::stream_acodec
/// was already set to the chosen track's codec, so the fresh Load configures the right audio
/// decoder). desired_audio_idx persists across the reload, so the demuxer keeps feeding the
/// chosen stream and the choice survives later seeks.
pub fn switch_audio_native(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, audio_idx: i32, pos_ns: i64) -> ReloadOutcome {
    SHARED.desired_audio_idx.store(audio_idx, Ordering::Relaxed);
    log(&format!(
        "switch_audio_native: audio_idx={audio_idx} at {}s",
        pos_ns / 1_000_000_000
    ));
    reload_at(ps, pa, pos_ns) // fresh direct-play Load at the current position, new audio stream
}

/// Reload the pipeline for a MODE/CODEC change — an audio-track switch on a direct-play HEVC
/// item forces a transcode (H264/AC3), so the pipeline must be re-Loaded with the H264 payload
/// (feeding H264 into the H265-configured pipeline stalls). Unlike reload_at, the transcode
/// start.mkv is already 0-based at `&offset`, so no av_seek — just set disp_base to the offset.
/// route::retranscode has already set the URL + session + STREAM_VCODEC=h264 before this call.
pub fn reload_transcode(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, offset_ns: i64) -> ReloadOutcome {
    #[cfg(any(test, feature = "test-support"))]
    note_reload_preroll();
    if crate::route::url(ps).is_empty() {
        // The other abandonment path with the same hole: `request_seek` armed the spinner and
        // nothing here would ever disarm it. See `player::abandon_seek`.
        crate::player::abandon_seek();
        log("reload_transcode: no url (ignored)");
        settle_missing_route_start();
        return ReloadOutcome::NoRoute;
    }
    match reload_transcode_start(ps, pa, offset_ns) {
        Some(outcome) if outcome.accepted() => ReloadOutcome::Started,
        Some(BufferfeedStartOutcome::Failed) | None => ReloadOutcome::StartFailed,
        // `start_bufferfeed_for` always creates a fresh attempt, but keep the conversion total if
        // that implementation changes.
        Some(BufferfeedStartOutcome::AlreadyRunning) => ReloadOutcome::Started,
        Some(BufferfeedStartOutcome::Launched(_) | BufferfeedStartOutcome::Deferred) => {
            unreachable!("accepted handled above")
        }
    }
}

/// The same reload edge as [`reload_transcode`], retaining the exact physical Load token for the
/// foreground state machine. Used only after a synchronous Original-open failure has restored its
/// HLS candidate; ordinary callers keep the coarser [`ReloadOutcome`].
pub fn reload_transcode_tracked(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, offset_ns: i64) -> BufferfeedStartOutcome {
    if crate::route::url(ps).is_empty() {
        crate::player::abandon_seek();
        log("reload_transcode_tracked: no url (ignored)");
        settle_missing_route_start();
        return BufferfeedStartOutcome::Failed;
    }
    reload_transcode_start(ps, pa, offset_ns).unwrap_or(BufferfeedStartOutcome::Failed)
}

/// Stop playback: unblock+join threads, reset all state so a fresh start_bufferfeed() can
/// restart, and release the pipeline and the video plane. With a ready object the native Unload is
/// handed to the `sf-unload` worker and the destroy / plane release finish later in
/// [`poll_release`] (`PendingRelease`), so the object may still exist when this returns; anything
/// that needs it gone is queued behind it in the release machine (`Deferred`), never waited for.
pub fn stop_bufferfeed(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) {
    teardown(ps, pa, false);
}

/// Suspend for an app-switch: tear the pipeline down (webOS reclaims the video plane while we're
/// backgrounded) but PRESERVE the playback session — keep the URL + transcode session, and
/// don't scrobble "stopped". This makes the foreground restore a clean same-item reload
/// (begin_resume + start_bufferfeed), the known-good path, instead of resurrecting a stopped session.
pub fn suspend_bufferfeed(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) {
    teardown(ps, pa, true);
}

/// Set where the pump ends an Engine for good under the failure read-out; spent by
/// [`release_failed_engine`] or by any teardown.
static FAILED_ENGINE_RELEASE_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The pump just failed the Engine for good ([`crate::route::fail_current_engine`] and the Error
/// state): release it next frame. Armed, not done here, because the pump still holds the Engine.
pub(super) fn arm_failed_engine_release() {
    FAILED_ENGINE_RELEASE_ARMED.store(true, Ordering::Release);
}

/// **A terminal playback error lets go of the pipeline.** The failure read-out stands on the
/// session, but an Engine left loaded behind it keeps the native pipeline and the video plane,
/// which the television reads as playback still going: its screensaver never starts.
///
/// Releases them the frame after the Engine failed, the same teardown an app switch uses
/// ([`suspend_bufferfeed`]: the session, URL and transcode session stay, nothing is reported as
/// stopped). What the read-out and its actions need survives it, in the session: the cause
/// ([`crate::route::note_engine_failure`], so `state()` and `error_now` still answer) and the
/// playhead a retry resumes from (the unpresented-resume slot `retry_failed_playback` reads).
/// Try again, Play automatically and a quality pick start a new playback from scratch, and Back
/// ends the session with a real stop, so none of them needs the loaded pipeline.
/// Returns whether an Engine was released.
pub fn release_failed_engine(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) -> bool {
    if !FAILED_ENGINE_RELEASE_ARMED.load(Ordering::Acquire) {
        return false;
    }
    // Armed by an Engine that has since been replaced (an automatic retry started a new playback
    // in the same frame): not this Engine's end, and nothing left to release.
    if !pa.is_live() {
        FAILED_ENGINE_RELEASE_ARMED.store(false, Ordering::Release);
        return false;
    }
    // Not this Engine's end: a preview, one a recovery is still flying for, or one that never put
    // a picture up (a Load that failed, an open that never produced a frame) and so holds nothing
    // the television mistakes for playback.
    if crate::route::is_preview(ps)
        || SHARED.pb_state.load(Ordering::Relaxed) != super::shared::PlaybackState::Error as u8
        || SHARED.frames.load(Ordering::Relaxed) == 0
        || crate::route::recovery_flight_outstanding()
        || crate::route::engineless_flight_outstanding()
    {
        return false;
    }
    FAILED_ENGINE_RELEASE_ARMED.store(false, Ordering::Release);
    let cause = super::live_runtime_failure(ps);
    crate::route::note_engine_failure(ps, cause, super::intended_pos_ns(ps));
    log("player: the failed Engine's pipeline is released under the failure read-out");
    teardown(ps, pa, true);
    true
}

/// Retire a failed foreground Engine only if it still owns the exact Load the foreground reducer
/// observed. A later route action may have replaced it between `player::pump` and the app poll;
/// tearing down unconditionally there would destroy the healthy replacement.
pub fn suspend_bufferfeed_if_attempt(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    attempt: crate::route::RouteStartAttempt,
) -> bool {
    let owns_attempt = pa.engine()
        .map(|engine| engine.route_start == attempt)
        .unwrap_or(false);
    if owns_attempt {
        teardown(ps, pa, true);
    }
    owns_attempt
}

/// The foreground lifecycle follows `attempt` (it launched that Load, or took over following it):
/// from now on the Load call's own synchronous refusal is the lifecycle's to settle
/// ([`Engine::foreground_follows_load`]). True when the live Engine owns that exact attempt; a
/// replaced Engine (a different attempt) is not marked.
pub fn foreground_follows_load(pa: &mut super::adapter::PlayerAdapter, attempt: crate::route::RouteStartAttempt) -> bool {
    match pa.engine() {
        Some(engine) if engine.route_start == attempt => {
            engine.foreground_follows_load = true;
            true
        }
        _ => false,
    }
}

/// The teardown body. `for_reload` = this is a direct-play seek reload (reload_at), NOT a real
/// stop: preserve the playback session so start_bufferfeed can restart the SAME item — skip
/// the "stopped" timeline scrobble, the server transcode stop, and the URL clear.
fn teardown(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, for_reload: bool) {
    FAILED_ENGINE_RELEASE_ARMED.store(false, Ordering::Release);
    // A start still queued behind the previous object's `Unload` is the playback this stop ends.
    // Always sent: the machine owns whether anything is queued (a no-op otherwise), and no copy of
    // that fact in the session can go stale across a landing that replaced the session.
    {
        let mut fx = Vec::new();
        pa.release_step(&super::release::ReleaseEvent::CancelQueued, &mut fx);
        let _ = run_release_effects(ps, pa, fx, None, false);
    }
    // A prepared route whose native start failed has no Engine but still owns its exact PMS
    // resource and Session projection. A real stop must retire that ownership exactly once;
    // returning here used to leak the candidate forever while the reducer remained Failed.
    if !pa.is_live() {
        // An Engine-less flight (a resume, or a rollback no Engine waits on) has no Engine to take down with it, and an app-switch suspend
        // keeps the session for the foreground restore, which reserves its own start transaction:
        // drop the flight as a live Engine's suspend would (`Stable`, landing discarded, only its
        // replacement stopped). A full stop below clears it itself.
        if for_reload && crate::route::engineless_flight_outstanding() {
            crate::route::begin_engine_teardown(true);
        }
        if !for_reload {
            // A real stop under the failure read-out whose Engine was released
            // ([`release_failed_engine`]) ends the playback the Engine would have ended: the
            // position it stopped at is the one the server keeps.
            let mut final_report = None;
            if crate::route::engine_failure(ps).is_some() {
                let dur = SHARED.duration_ns.load(Ordering::Relaxed);
                let pos = crate::route::unpresented_resume_ns(ps);
                crate::player::report::ended(ps, pos, dur);
                let rk = crate::route::cur_rk(ps);
                final_report = (!rk.is_empty() && dur > 0).then(|| (rk, pos / 1_000_000, dur / 1_000_000));
            }
            crate::route::scrobble_stop(ps, final_report, None);
            crate::route::begin_engine_teardown(false);
            crate::route::drop_original_recovery(ps);
            crate::route::clear_url(ps);
            SHARED.reset_session();
            TX.reset();
            super::claim_hold::clear();
            // Nothing here is asynchronous, so the fence `begin_engine_teardown` just raised is
            // already spent. Publish the completed stop rather than latching `Stopping` — see
            // `route::finish_engine_teardown`.
            crate::route::finish_engine_teardown();
        }
        return;
    }
    // A preview Load that has not returned yet must not be joined on this frame. Bind is skipped
    // while the flag is set, so the picture never reaches the plane. The next halt joins once
    // the thread has already finished.
    if crate::route::is_preview(ps) {
        if let Some(eng) = pa.engine() {
            let loading = eng.stage == Stage::Loading;
            let finished = eng.load_th.as_ref().is_none_or(|t| t.is_finished());
            if crate::player::preview::defer_media_join(loading, finished) {
                eng.preview_abandon = true;
                return;
            }
        }
    }
    // Revoke every worker ticket before asking those workers to stop. This is the synchronization
    // boundary that makes a late HLS candidate/fallback an ordinary stale event instead of a
    // mutation of the route the next Load is about to own.
    let mut sp = TeardownSpans::new();
    let mut unload_th = None;
    crate::route::begin_engine_teardown(for_reload);
    SHARED.stop_hls_clock();
    let mut eng = pa.take().expect("main-thread live check and take are one transaction");
    let stream = matches!(eng.source, Source::Stream { .. });

    // capture the final-position report BEFORE teardown zeroes playpos/duration (a reload is
    // not a stop — don't scrobble "stopped", it would falsely pause/mark-watched the item)
    // **The one place a playback really ENDS.** `for_reload` covers a direct-play seek reload, an
    // ABR rung change and an app-switch suspend — each of which destroys an ENGINE and keeps the
    // playback — so reporting an ending on any of them would turn the completion rate into a
    // measure of how often people scrub. Read before the teardown below zeroes both values, for
    // the same reason `final_report` is.
    if !for_reload {
        crate::player::report::ended(
            ps,
            SHARED.playpos_ns.load(Ordering::Relaxed),
            SHARED.duration_ns.load(Ordering::Relaxed),
        );
    }
    let final_report = if for_reload {
        None
    } else {
        let rk = crate::route::cur_rk(ps);
        let dur = SHARED.duration_ns.load(Ordering::Relaxed);
        if !rk.is_empty() && dur > 0 {
            Some((
                rk,
                SHARED.playpos_ns.load(Ordering::Relaxed) / 1_000_000,
                dur / 1_000_000,
            ))
        } else {
            None
        }
    };

    // The side subtitle reader first: it reads the original Part over its own transport and must
    // be gone (its socket shut, its cues cleared) before the demux it was paired with is joined.
    sp.mark("begin");
    super::subside::stop(if for_reload { "reload" } else { "stop" });
    sp.mark("subside");
    // 1. unblock every thread (abort queues, close the demux socket)
    if let Some(st) = eng.report_stop.take() {
        st.stop(); // this reporter's own signal — unaffected by the reset_session below
    }
    if stream {
        // abort BOTH lanes: unblock the demux if it's parked in aq_push on a full lane
        for q in [eng.aq_video.as_mut(), eng.aq_audio.as_mut()]
            .into_iter()
            .flatten()
        {
            crate::aq::aq_abort(&mut **q);
        }
        let p = SHARED.hs_ptr.load(Ordering::Acquire);
        if !p.is_null() {
            // shutdown, not close: the demux thread is still inside recv here and is joined
            // below. Closing now would free the fd number for another thread to claim while
            // this one is still reading it. The real close happens after the join.
            plx_net::stream::http_shutdown(p);
        }
        // …and the same interrupt for the OTHER transport. An https demux is parked in
        // `curl_multi_wait`, where no `shutdown(2)` of ours can reach it — this writes a byte to
        // the wake pipe it is polling. A no-op when the live source is a plaintext socket, or when
        // there is none; exactly one of these two lines ever has anything to do. `curlio`'s module
        // doc explains why the handle lives in a registry rather than being passed up to here.
        crate::curlio::abort_active();
    }
    sp.mark("unblock");
    // 2. JOIN every worker before freeing anything they hold raw ptrs into
    // Through `task::join` so a stall leaves a number behind: this is the main thread, and every
    // teardown freeze the engine has had was one of these three.
    if let Some(t) = eng.stream_th.take() {
        plx_base::task::join("demux", t);
    }
    sp.mark("join_demux");
    // A Load the D.1.4 budget already gave up on, still inside `sf_load`: do NOT join it here.
    // This is the main thread, and on the set where the budget fires the call may never return —
    // BACK from the timeout read-out used to freeze the app on this line. The native object is
    // parked instead and released by `reap_abandoned_load` once the thread has returned; nothing
    // below may touch it before then (issue #74). Every other Load keeps the ordinary ordering:
    // one that has returned joins at once, and one that is merely slow is still joined.
    let abandoned_load = match eng.load_th.take() {
        Some(t) if !t.is_finished() && SHARED.load_timed_out.load(Ordering::Acquire) => Some(t),
        Some(t) => {
            plx_base::task::join("media", t);
            None
        }
        None => None,
    };
    sp.mark("join_media");
    // 2b. every reader is now joined, so this thread is the sole owner: do the real close.
    // (Before the join it could only shutdown — see step 1.)
    if stream {
        let p = SHARED.hs_ptr.load(Ordering::Acquire);
        if !p.is_null() {
            plx_net::stream::http_close(p);
        }
    }
    // Final position report (state=stopped, so the server commits the resume point) + the
    // server-side transcode stop, both dispatched to a worker. They used to run inline HERE and
    // twelve lines below — two blocking PMS round trips on the SDL thread, ~17 s each worst case,
    // on 100% of real stops. `scrobble_stop` reads and clears route's session statics on THIS
    // thread and hands the worker owned copies; `plex_run` drains it at exit so the report still
    // lands. Skipped for a reload, which is not a stop.
    // The reporter is NOT joined here. Its handle rides out with the scrobble worker, which
    // joins it before posting `stopped` — so the last `playing` report still lands first, but the
    // MAIN thread stops paying for it. It parked the frame loop for the remainder of SO_RCVTIMEO
    // whenever that POST had stalled (measured: `THREADJOIN timeline 6974ms`, tools/netcond.py in
    // `stall@/:/timeline` — whose scope was broken until 2026-08-23, so that run stalled EVERY
    // connection. The named `THREADJOIN` line is what keeps the attribution good, and step 1 above
    // is why: demux and the AU lanes are woken before they are joined, so `timeline` was the only
    // one that could park). Safe to outlive the Engine: the reporter owns `ReportStop` and a
    // `TimelineLease`, samples only SHARED clock state, and the route reducer revalidates the lease
    // before each network effect.
    sp.mark("http_close");
    if !for_reload {
        crate::route::scrobble_stop(ps, final_report, eng.report_th.take());
    }
    sp.mark("scrobble");
    // 3. Release the native object (see `release_native_object` for the proof chain), unless its
    // Load is still in flight — then hand it, its thread and its payload to the adapter.
    if let Some(load_th) = abandoned_load {
        if !SHARED.abandon_native_session(eng.native_epoch) {
            log(&format!(
                "native lifecycle: abandoned Load epoch={} no longer owned the Active phase",
                eng.native_epoch,
            ));
        }
        log(&format!(
            "native lifecycle: Load epoch={} still in flight at teardown — not joining; the \
             object stays parked (and native playback refused) until Load returns",
            eng.native_epoch,
        ));
        pa.park_abandoned_load(AbandonedLoad {
            epoch: eng.native_epoch,
            load_th,
            _payload: std::mem::take(&mut eng.payload),
            abandoned_at: std::time::Instant::now(),
        });
    } else if !for_reload {
        unload_th = release_off_frame(pa.mt(), eng.native_epoch, &mut sp);
    } else {
        release_native_object_timed(pa.mt(), eng.native_epoch, &mut sp);
    }
    // 4. drain + destroy both queues (drain_aq also clears both pendings)
    if stream {
        drain_aq(&mut eng);
        for q in [eng.aq_video.as_mut(), eng.aq_audio.as_mut()]
            .into_iter()
            .flatten()
        {
            crate::aq::aq_destroy(&mut **q);
        }
    }
    sp.mark("drain_aq");
    // 5. reset shared + transport. On a real stop also stop the server transcode + clear the
    // URL; on a reload KEEP them so start_bufferfeed restarts the same item (a direct-play
    // reload has no transcode session anyway, so the skip only matters for the URL).
    // The DURATION is a fact about the FILE, and a reload plays the same file: keep it across
    // the reset so the playbar has a denominator while the demuxer reopens. Without this every
    // reload-based seek (a refused native Seek, a speed seek, a transcode seek) drew
    // the playhead at position ÷ 0, i.e. the far left, for the few hundred ms until `ff.rs`
    // re-published it, while the clock beside it (position alone, seeded by `arm_seek`) read
    // correctly. Reported on an LG C3 (webOS 23). A real stop still zeroes it: the next item is
    // a new file.
    if for_reload {
        SHARED.reset_session_for_reload();
        TX.reset_for_reload();
    } else {
        SHARED.reset_session();
        TX.reset();
        super::claim_hold::clear();
    }
    if !for_reload {
        // **The carried link estimate dies with the PLAYBACK, not with the engine** (I8). A reload
        // is the same item on the same link at a new position — a seek, a quality pick, an
        // app-switch resume — and re-measuring the link from nothing across one is what made every
        // skip re-ramp the ladder for ten to twenty seconds.
        SHARED.clear_abr_seed();
        // The failure is retained across the rollback reload only; a new playback must not carry
        // another item's PMS response into its diagnostics.
        SHARED.clear_abr_failure();
        // A deferred Original recovery outlives nothing: the playback it belonged to is over, so
        // there is no frame left that could confirm it and no route left to roll back to. The
        // encoder it was holding is still running on the server, which is why this RETIRES rather
        // than forgets — see `route::drop_original_recovery`.
        crate::route::drop_original_recovery(ps);
        crate::route::clear_url(ps); // the transcode stop rode out with `scrobble_stop` above
    } else if let Some(t) = eng.report_th.take() {
        // A reload posts no `stopped`, so there is nothing to order against: detach. Its own
        // ReportStop is already set, so it exits after its current POST and cannot be revived by
        // the next session — which is exactly what the shared flag could not guarantee.
        drop(t);
    }
    // Every worker `begin_engine_teardown` fenced is joined and the URL is gone: the stop is
    // COMPLETE as far as the route is concerned, so retire the fence too. The native object may
    // still be inside its off-frame `Unload`; that is the adapter's fence (`PendingRelease`), not
    // the route's, and every native start finishes it first. A reload keeps its own phase
    // (`Prepared`/`OriginalTrial`), which is the transaction the caller is about to start.
    if !for_reload {
        crate::route::finish_engine_teardown();
    }
    sp.mark("reset");
    sp.emit(if for_reload { "reload" } else { "stop" });
    match unload_th {
        Some(unload_th) => {
            log(&format!(
                "stop_bufferfeed: torn down (native Unload epoch={} continues off-frame)",
                eng.native_epoch,
            ));
            pa.park_release(PendingRelease {
                epoch: eng.native_epoch,
                unload_th: Some(unload_th),
                handed_off_at: std::time::Instant::now(),
                _engine: eng,
            });
        }
        // Engine (hs/aq boxes, payload) drops here — after all joins
        None => log("stop_bufferfeed: torn down"),
    }
}

/// free every queued AU + the held pending one, BOTH lanes (seek + teardown).
pub fn drain_aq(eng: &mut Engine) {
    drain_one(eng.aq_video.as_mut());
    drain_one(eng.aq_audio.as_mut());
    eng.pending_video = None;
    eng.pending_audio = None;
}

fn drain_one(q: Option<&mut Box<AuQueue>>) {
    if let Some(q) = q {
        let qp = &mut **q as *mut AuQueue;
        let mut eof: c_int = 0;
        loop {
            let n = crate::aq::aq_pop(qp, &mut eof);
            if n.is_null() {
                break;
            }
            unsafe { libc::free(n as *mut c_void) };
        }
    }
}

/// feed streamed AUs from the demux queue; hold the current AU across ticks on
/// BufferFull (backpressure); zero-base the fed timeline on the first post-seek
/// keyframe; drop stale AUs past the B-frame reorder distance.
/// prime-then-play buffer depth: how much of the post-seek stream to buffer (paused) before
/// starting the clock. Enough to cover the pipeline's decode latency so the first frame is ready.
const PRIME_NS: i64 = 700_000_000;
// Prime the AUDIO lane too before starting the (audioSync master) clock — else a rapid-seek drain
// can start Play on an empty audio queue and leave audio silent until the next seek. Fallback:
// start anyway once video buffers PRIME_VIDEO_MAX_NS without audio (audioless / briefly starved),
// so a genuinely audioless region can't hang.
const PRIME_AUDIO_NS: i64 = 300_000_000;
pub(crate) const PRIME_VIDEO_MAX_NS: i64 = 2_500_000_000;
// Feed-ahead throttle (Kodi-parity): keep the VIDEO lane at most this far ahead of the presented
// position (SHARED.pres_fed) instead of feeding greedily to BufferFull. Bounding the buffer to
// ~1.6s (was ~10-20s: aq 6MB + the pipeline's own ~8MB) makes seeks flush far less, keeps the
// clock from running ahead, and cuts latency. AUDIO gets a looser bound so it can ride slightly
// ahead (audio buffer is cheap and it's the master clock) without unbounded race on odd muxes.
pub(crate) const MAX_FEED_AHEAD_NS: i64 = 1_600_000_000;
pub(crate) const AUDIO_SLACK_NS: i64 = 2_000_000_000;
// A fed pts this far below a lane's high-water is a stale pre-seek AU (past the B-frame reorder
// distance) → drop it rather than feed a backward jump.
const STALE_BACKJUMP_NS: i64 = 2_000_000_000;
// In-place-seek rebase guard: a first-keyframe this far from the seek target is a stale pre-reopen
// frame from the drifted read position, not the real post-seek keyframe. av_seek(BACKWARD) lands
// AT/BEFORE the target, so a valid anchor is never ahead and is at most ~one GOP behind — hence a
// tight AHEAD bound and a looser BEHIND bound (to tolerate sparse keyframes). Capped by MAX_REBASE_DROPS.
const SEEK_STALE_AHEAD_NS: i64 = 6_000_000_000;
const SEEK_STALE_BEHIND_NS: i64 = 30_000_000_000;
const MAX_REBASE_DROPS: i32 = 240;
// Audio counterpart of the rebase guard: an audio AU this far AHEAD of the video high-water is a
// stale drifted frame (the demuxer's pre-reopen audio after a seek). Feeding it poisons
// max_fed_audio_pts (and then the backjump guard drops all LEGIT audio until the demux catches
// up to the poisoned mark → seconds of silence). Legit audio lead is bounded by the throttle to
// ~MAX_FEED_AHEAD+AUDIO_SLACK (3.6s), so 5s has margin. It was 15s, and a stuck-retry seek fed
// stale audio 14.9s ahead — 13s of user-audible silence after a rapid 10s-back tap burst.
const AUDIO_STALE_AHEAD_NS: i64 = 5_000_000_000;
/// How far past the start of its timeline the audio lane may feed whatever the clock says.
///
/// **A track whose audio starts late must still reach Starfish before the clock can move.** An
/// MKV audio `Delay` (an 11.008 s E-AC-3 track on a WEB-DL, 2026-10) puts the first audio AU
/// seconds past the first video one. Starfish's clock waits for audio, so `pres_fed` sits at 0
/// and the video lane stops [`MAX_FEED_AHEAD_NS`] in — and the audio AU, far past both, was
/// dropped as stale by [`AUDIO_STALE_AHEAD_NS`] and would have been held by the throttle anyway.
/// Nothing moved again: the picture buffered forever, while a resume past the gap played. The
/// AU the demuxer marked as its first since the open or seek is the real start, never stale
/// pre-seek audio (that was pushed before the mark), so it and the next 2 s are fed regardless.
/// 2 s is under the throttle's own audio lead ([`MAX_FEED_AHEAD_NS`] + [`AUDIO_SLACK_NS`]), so a
/// track that starts on time is fed exactly as before.
///
/// **The demuxer has to be able to READ that much audio**, which for a late track means with the
/// video lane already past its base cap and nobody draining it (no clock yet): `ff::demux` keeps
/// the video lane's pre-audio headroom until the audio lane has been pushed this much plus a
/// margin (`ff::LEAD_IN_MARGIN_NS`), and only then releases it as the consumer drains.
pub(crate) const AUDIO_LEAD_IN_NS: i64 = 2_000_000_000;
// The lead-in is only sound between these two: under the throttle's own audio lead (a track that
// starts on time is fed exactly as before), and over the audio the clock is primed with (else
// "ahead of the clock" would not be enough audio to start it).
const _: () = assert!(AUDIO_LEAD_IN_NS < MAX_FEED_AHEAD_NS + AUDIO_SLACK_NS);
const _: () = assert!(AUDIO_LEAD_IN_NS > PRIME_AUDIO_NS);
// Sentinel for SHARED.pres_fed meaning "no post-seek frame has presented yet" — the feed-ahead
// throttle treats it as feed-freely (don't compare the new fed pts against a stale pre-seek
// presented position). Set on a seek; the first presented frame overwrites it with a real pts.
pub const PRES_NONE: i64 = i64::MIN;

/// The longest the throttle's reference runs on past the last position report, in ms. The
/// pipeline reports every ~201 ms, so against this cap the run-on bridges a report up to ~49 ms
/// late (not a missed one); a paused or stalled clock stops changing `pres_fed`, and the
/// reference then stops this far ahead of it instead of opening the lane without bound.
const FEED_PACE_CAP_MS: u32 = 250;

/// **The feed-ahead throttle's reference: the presented position, run on at 1x between reports.**
///
/// `SHARED.pres_fed` moves in ~200 ms steps (the pipeline's 5 Hz position callback). Measured
/// against that step function, the throttle stayed shut for eleven frames and then opened by
/// 200 ms of media at once, so every fifth of a second one frame fed ~5 video and ~7 audio AUs to
/// Starfish, and `Feed()` blocks: on the TV with the track menu open, `FRAMEDROP … results=14.6`
/// with `feed:14.4` inside it, 4–15 ms on every 12th frame. Run on from the last report at 1x,
/// the same budget opens a frame's worth of media per frame, so the same AUs reach
/// Starfish one or two per frame. The bound is the same lead,
/// `MAX_FEED_AHEAD_NS`, past the ESTIMATED playhead; the reserve can therefore exceed the raw
/// report's by at most [`FEED_PACE_CAP_MS`], and that cap stops the estimate when the reports
/// stop changing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeedPace {
    /// The last distinct `pres_fed` seen, and when it was first seen (player clock, ms).
    last: Option<(i64, u32)>,
}

impl FeedPace {
    /// The position to throttle against at `now_ms`, given the current raw `pres_fed`.
    /// [`PRES_NONE`] passes through untouched (feed freely) and forgets the last report.
    pub fn reference(&mut self, pres: i64, now_ms: u32) -> i64 {
        self.reference_at(pres, now_ms, super::RATE_NORMAL)
    }

    /// [`Self::reference`] for a pipeline playing at `rate_permille` (1500 = 1.5x): between two
    /// reports the playhead moves that much media per wall-clock ms, so the run-on scales with it.
    /// Run on at 1x under a faster pipeline, the estimate would trail the real playhead and the
    /// lane would feed less lead than the throttle means to keep.
    pub fn reference_at(&mut self, pres: i64, now_ms: u32, rate_permille: i32) -> i64 {
        if pres == PRES_NONE {
            self.last = None;
            return pres;
        }
        let at = match self.last {
            Some((p, at)) if p == pres => at,
            _ => {
                self.last = Some((pres, now_ms));
                now_ms
            }
        };
        let run_on = now_ms.saturating_sub(at).min(FEED_PACE_CAP_MS) as i64;
        pres.saturating_add(run_on * 1_000 * i64::from(rate_permille.max(0)))
    }
}

/// The most wall-clock time ONE lane may spend feeding in one tick, in µs, once it has fed an AU
/// **and its lead is healthy** (see [`lane_starved`]).
///
/// `Feed()` blocks on the frame thread, 0.3–0.7 ms per AU on the TV. In steady state a lane feeds
/// one or two AUs a frame and never comes near this. The throttle does not apply while priming,
/// though, and a post-prime lane is a full lead behind, so on 2026-10-01 the first frames of
/// every playback fed the whole demuxed backlog at once: `FRAMEDROP total=89.1 … sfvx62:19.4,
/// sfax120:50.8` (62 video and 120 audio AUs in one frame), then `sfax53:38.3` the frame after,
/// with the player's HUD and track menu on screen. A seek re-primes the same way. Two lanes at
/// this slice cost at most ~6 ms of a 16.7 ms frame plus one AU's overrun each.
/// The slice is per LANE, so the video lane cannot spend the audio lane's share. The old greedy
/// video lane did exactly that: it fed 2.5 s of video before audio got its turn, and the prime
/// then fell through to the video-only escape (`primed: v=2541ms a=0ms`).
///
/// **The slice spreads a BACKLOG; it must never throttle a lane that is short of media.** A
/// lane below its low-water mark ([`lane_starved`]) is exempt: while priming, that is until the
/// lane holds its prime depth ([`PRIME_NS`] / [`PRIME_AUDIO_NS`], 17–42 AUs) — otherwise a slow
/// `Feed()` (3 ms or more, so one AU a lane a tick) stretches time-to-first-frame by that many
/// ticks — and once playing, until it is [`FEED_LOW_WATER_NS`] ahead of the playhead — otherwise
/// at the 50 Hz present rate 60 fps content drains its lead. The exemption is bounded by the
/// low-water mark itself, so the bulk of a backlog is still fed a slice a tick, and the prime
/// finishes on the tick its depth is reached, one tick later at most than a greedy feed would.
const FEED_LANE_SLICE_US: u64 = 3_000;

/// A playing lane whose lead over the presented position is below this is exempt from
/// [`FEED_LANE_SLICE_US`]. 250 ms is [`FEED_PACE_CAP_MS`]: the position reports arrive every
/// ~200 ms, so a lead under it can be gone before the next report says so, and it is ~6 AUs of
/// 24 fps video — a handful of `Feed()` calls, bounded cost, never a backlog.
const FEED_LOW_WATER_NS: i64 = 250_000_000;

/// Whether a lane is short of media, and so must be fed past its slice.
///
/// While priming (`priming`) the lane is short until it holds `prime_ns` past the seek base;
/// otherwise it is short when its lead over the presented position `pres` ([`PRES_NONE`]: nothing
/// presented, no lead to judge, not short) is under [`FEED_LOW_WATER_NS`].
fn lane_starved(priming: bool, max_fed: i64, seek_base: i64, prime_ns: i64, pres: i64) -> bool {
    if priming {
        max_fed - seek_base < prime_ns
    } else {
        pres != PRES_NONE && max_fed - pres < FEED_LOW_WATER_NS
    }
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// The test feed clock: each read advances it by `FEED_TEST_CLOCK_STEP_US`, which is 0 unless
    /// a test sets it. At 0, the host seam's near-free `sf_feed` never runs out the slice, so a
    /// slow or preempted test machine cannot change what one tick feeds.
    static FEED_TEST_CLOCK_US: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static FEED_TEST_CLOCK_STEP_US: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Monotonic µs for [`FeedSlice`]; only ever differenced.
fn feed_clock_us() -> u64 {
    #[cfg(any(test, feature = "test-support"))]
    {
        let step = FEED_TEST_CLOCK_STEP_US.with(|s| s.get());
        FEED_TEST_CLOCK_US.with(|c| {
            c.set(c.get() + step);
            c.get()
        })
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        use std::sync::OnceLock;
        use std::time::Instant;
        static T0: OnceLock<Instant> = OnceLock::new();
        T0.get_or_init(Instant::now).elapsed().as_micros() as u64
    }
}

/// One lane's share of a tick's feeding: [`FEED_LANE_SLICE_US`] from the lane's start.
struct FeedSlice {
    start: u64,
}

impl FeedSlice {
    fn begin() -> Self {
        Self { start: feed_clock_us() }
    }

    /// Whether the lane should stop before its next AU. A lane that has fed nothing yet always
    /// gets one AU, so the backlog drains on every tick, however slow the sink is; a `starved`
    /// lane ([`lane_starved`]) is never spent.
    fn spent(&self, fed: usize, starved: bool) -> bool {
        fed > 0 && !starved && feed_clock_us().saturating_sub(self.start) >= FEED_LANE_SLICE_US
    }
}

/// VIDEO lane feeder (aq_video is video-only). Owns the seek rebase + in-place-seek handshake + prime→Play, all of
/// which key off the first post-seek VIDEO keyframe. A BufferFull/over-budget breaks THIS lane
/// only — the audio lane (feed_audio_lane) keeps flowing so the audioSync master clock advances.
/// Nanoseconds added to every fed VIDEO PTS — **a one-tick rounding repair for LG's Dolby Vision
/// display-management lookup**, and inert everywhere else.
///
/// The fault is arithmetic and it is entirely on the television's side; this is the only lever we
/// have on it. `gstdualsequencer.c:606` (DWARF-confirmed, `libgstdualsequencer.so` 0x25b0–0x25e0)
/// keys the LUT entry it hands the display firmware with a DOUBLE truncation of the buffer's
/// nanosecond PTS:
///
/// ```text
/// ulTimeStamp = trunc(trunc(pts_ns) * 9 / 100000)      // ns -> 90 kHz ticks
/// ```
///
/// and `DOVI_SWSync_SetDoviLUTnMap` (`libkadaptor` 0xe30e8) then scans all 95 slots for **exact
/// 32-bit equality** — no tolerance, no nearest match. At 24000/1001 fps neither unit is exact:
/// a frame time is a whole number of nanoseconds only every **3rd** frame and a whole number of
/// 90 kHz ticks only every **4th**, so on `n ≡ 4, 8 (mod 12)` — and on no other frame — the two
/// truncations disagree by exactly one tick, the lookup misses, and the panel reuses the previous
/// frame's tone mapping. `lcm(3,4) = 12` frames is **0.5005 s**, which is the period of the
/// stutter as seen.
///
/// Measured on this set, uninstrumented, 85 s of Profile 5: 340 misses, and **340 of 340** equal
/// the double-truncated key while **0 of 340** equal the exact tick value — residues `{4: 170,
/// 8: 170}` and nothing else. It is a derivation that reproduces the data with no free parameter,
/// not a fit.
///
/// The repair is to hand the pipeline a PTS whose truncation lands in the right bin. The loss is
/// the fractional nanosecond FFmpeg's rescale already dropped, so it is strictly less than 1 ns,
/// and **one** nanosecond recovers it. The upper bound is `100000/9 ≈ 11111 ns` — beyond that an
/// already-correct frame would be pushed a tick the other way — so 1 sits at the safe end of a
/// wide range. As an A/V offset it is nothing: 1 ns against a 41.7 ms frame.
///
/// **Video only.** The key is computed from the video buffer's own PTS; the audio lane never
/// reaches this code and shifting it would be a skew for no reason.
///
/// **THE SIGN WAS BACKWARDS, AND −1 IS THE FIX.** Read the history below for what was tried; the
/// short version is that the model was right about the mechanism, wrong about which side rounds,
/// and the device settled it. Measured with LG's own level-2 KADP logging armed mid-playback
/// (`tools/logmprobe`): for **38 of 40** misses the key written into the LUT ring is exactly the
/// key the display firmware requested **plus one**. The ring is a tick HIGH, so the fed PTS goes
/// DOWN. Alternating unseeked legs, same title, same binary:
///
/// ```text
///   nudge = -1   misses 1        nudge = 0   misses 81
///   nudge = -1   misses 1        nudge = 0   misses 81
/// ```
///
/// Reproducible, 81:1, and the arithmetic that predicts −1 also predicted +1 would help; +1 was
/// measured at 163/165 against 164 for zero — i.e. inert. **Trust the measurement here, not the
/// derivation**: our fed PTS is not passed through, the pipeline re-timestamps by NEAREST-rounding
/// on the 1001/24000 lattice (measured: alternating −0.333/+0.333 ns against the exact rational),
/// so exactly how one nanosecond on our side moves a tick on theirs is not something this comment
/// can honestly claim to model. What it can claim is 81:1, twice, with the scene controlled by
/// alternation.
///
/// # The history, kept because three of its steps were wrong and each cost a run
///
/// **AND THE DEVICE REFUTED THE FIRST ATTEMPT, which is why the default was 0 for a while.** The one step
/// the disassembly could not settle was whether the OTHER side of that exact-equality comparison
/// moves with us. It does. Controlled A/B, same title, same seek to 900 s, same 45 s window, only
/// this value differing: **nudge 0 → 118 misses, nudge 1 → 230**. Worse, not fixed. A constant
/// offset cannot repair a *relative* truncation difference when both sides derive from the same
/// fed timestamp — which is now measured rather than assumed, and which also retires the tidiest
/// explanation this investigation has produced.
///
/// What survives the refutation is the arithmetic, and it is not small: 340 of 340 misses in the
/// unseeked run equal the double-truncated key and 0 of 340 equal the exact tick value, residues
/// `{4, 8}` mod 12 and nothing else. So the key IS computed that way and the comparison IS exact.
/// What is now open is why the slot the firmware asks for — using, we measured, dualsequencer's
/// own key — is not in the ring. That points at pairing or at slot lifetime, not at rounding.
///
/// `/tmp/plxnative-ptsnudge=<ns>` is kept because it is the instrument that produced that result
/// and the next candidate value is one run away. Anything from 1 to ~11110 is in range; beyond
/// that an already-correct frame is pushed a tick the other way.
///
/// It is also how the PCM lead (#1086) was first measured: `ptsnudge=-390000000` (the VIDEO 390 ms
/// earlier) read -0.017 to -0.041 s on a PCM Load, against -0.377 s with no correction. The correction
/// ships on the audio side instead ([`PCM_SINK_LEAD_NS`]), so the two are not to be combined in a
/// run: the video's would be taken twice.
fn pts_nudge_ns() -> i64 {
    const DEFAULT: i64 = -1;
    static NUDGE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(i64::MIN);
    let v = NUDGE.load(Ordering::Relaxed);
    if v != i64::MIN {
        return v;
    }
    // Latched at the first feed rather than read per AU: this is the hottest path in the app and
    // the trigger surface is a filesystem open. Same shape as every other `devtrig::` read here.
    let v = plx_base::devtrig::read("ptsnudge")
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT);
    NUDGE.store(v, Ordering::Relaxed);
    if v != DEFAULT {
        log(&format!(
            "ptsnudge: video PTS nudge overridden to {v} ns (default {DEFAULT})"
        ));
    }
    v
}

/// How much later than the picture an app-decoded audio unit is stamped for the sink, in
/// nanoseconds, on LG's audio stacks when the sound output is not one that was filmed: 400 ms. The
/// set plays a Load that declares `"audio":"PCM"` through a `PCMMC` mixer channel instead of its
/// `ADEC` decoder (libplayerAPIs `allocateResource`, libpf's resource manager), and LG's renderer
/// applies its DSP lip-sync stage to the hardware decoder and not to a mixer channel. That stage holds
/// the decoder path back by an offset that a PCM Load never is; this lead gives it the same.
///
/// **Where it applies** ([`plx_platform::devcaps::audio_stack`], fingerprinted by the sink plugin; the
/// table is [`pcm_output_lead`]). Everything below is firmware read by hand and by decompile passes
/// (OTA IDs and releases named), never pasted; ONLY the dev set was filmed.
///
/// | Stack | Sets | Effective lip-sync offset | Per-output part |
/// |---|---|---|---|
/// | `liblxa` | 2019-2021 LG chips (dev set W19H 4.10.2), 2022/23 OLEDs before webOS 11 (inferred from the `tools/fwcompat.py` inventories, not from an image read) | 4.5: 505 with a +105 app bias = 400 (MEASURED); W21O 6.5.3: template 400 raw, no +105, effective 400 by the same convention | wireless 150 => Bluetooth 250 (MEASURED) |
/// | `lgaudiosink` | 2022/23 OLEDs on webOS 11 (W22O 11.2.0-74, W23O 11.2.0-3711), 2024 (W24G 9.2.4), 2025 (W25G 10.3.1) | template `{150,150,400,5000,5000}`, 400 raw, no bias | none: 400 on every output |
/// | `audiosink_new` | 2018 (OLED C8 W18O, LCD SK8500 W18H, 4.4.3) | `libkadaptor` default 475 with a +75 app bias = 400 (4.5 inherits it), hand-verified | decoder-only -150 for a wireless output (decompile only) => Bluetooth 250 |
/// | `adecsink` | 2016 (W16M 3.4.3, element `audio_sink`), 2017 (W17H 3.9.2, element `adecsink`) | 2016: default 550, AUTO/mixer rule not in the plugin (not determined). 2017: default 442/472 ms, bias unknown, so differential and DSP bias cannot be split | 2017: decoder only, -100 DD / -150 wireless (a measurement must be per output); 2016: none found |
/// | `other` | Realtek, MStar | their lip sync is NOT decoder-only: no mixer-off rule | none |
///
/// The rule behind the lead is that lip-sync mode AUTO turns the DSP stage ON for the decoder and OFF
/// for a mixer channel, found in `liblxa`'s `lxao_play`, in 2018's `libkadaptor` `Media_Render_Play`
/// (AUTO plus a mixer channel: `EnableLipsync(fd, 0)`) and, with the renderer compiled into the sink and
/// renamed `lgao_*`, in `libgstlgaudiosink.so`. A stack that gets no lead is one where that rule was not
/// found or its constants cannot be separated: Realtek (`rtkalsasink`) and MStar (`omxaudiosink`) have
/// no mixer-off rule (their lip sync is not decoder-only), Realtek Bluetooth is not determined, and
/// MStar may be up to about 128 ms early (its SoC delay floor), which is not "stays right". A set with
/// no lead (`adecsink` on 2016/2017, not determined; Realtek; MStar; an unknown stack) gets NO lead
/// (the behaviour before #1087, 0 ms), because if its PCM path was already in step, 400 ms makes it
/// 0.4 s late. `pcmavoffset` is how such a chip gets measured, and it wins on any stack.
///
/// **`lgaudiosink`: 400 ms on every output, Bluetooth included, no output read.** The offset is the
/// template's 400 raw (webOS 4.5 had 505 with +105; webOS 11 dropped both), and the sink's own 475 ms
/// render delay is applied on both paths and cancels. libpf's `CustomPipeline` never sets the sink's
/// `lipsync` property: CONFIRMED on webOS 11 (in W22O libpf every cross-reference of the string is
/// inside `GenericPipeline::deepElementAddedCallback`). That the per-output table
/// (`LIPSYNC_OFFSET_VALUES`) is unreferenced is checked in part, not proven: on W22O it has no
/// relocation and no `movw`/`movt` pair for its address and its getter `lgao_get_delay` has no direct
/// call site (W24G the same); a pc-relative reference was not ruled out. If it were read after all,
/// Bluetooth would need 250 as on `liblxa` and this gives it 400, about 150 ms late. Per-output
/// adjustment moved to the VIDEO plane (avSync keys by `soundOutput`), which moves both paths equally.
///
/// **The one link NOT proven on webOS 11: the `audio-port` value the `PCMMC` resource reaches the sink
/// with.** The sink's rule is `audio-port == -1` and `mixer-port >= 0` => mixer path. In W22O
/// libplayerAPIs `acquireResources` retypes the ADEC entry to `"PCMMC"` in place and libpf
/// `CustomPipeline::setResource` forwards ports verbatim, so the decision and the sink's rule are the
/// same as on 4.5, where the measurement shows the mixer path; the literal value was not read. If the
/// port were 0 or 1 the sink would stay off the mixer path, AUTO would resolve to ON, app PCM would
/// get the decoder's own 400 ms and be in step, and this lead would make it 0.4 s late. Settled on a
/// set by the sink's own log: `mixer mode changing %s -> %s` and `mixer-port is %d`.
///
/// **`liblxa` on newer firmware** (2021 OLED, 6.5.3): the template is 400 raw with no +105 (effective
/// 400 by the same convention) and its per-output table walk is still there; that the mixer channel is
/// forced to type PCM (0) there was hand-verified on 4.5 only.
///
/// The lead is the mechanism and playback stays straight and lossless: re-encoding to a codec the set
/// decodes itself (which would make the set's own compensation apply) was considered and not taken.
///
/// On `liblxa` and a TV-speaker output that is the whole story. On any other output the set also
/// advances its OWN decoder's audio by a per-output delay that a mixer channel never gets (liblxa's
/// `_lxao_find_lipsync_output_type`, read as reasoning and not copied: wireless 150 ms, Dolby
/// bitstream 160, Atmos 170, DTS 160, WISA 100, ES 30), so the right lead is 400 ms minus the
/// active output's delay. [`PCM_LEAD_BY_OUTPUT`] holds the outputs that were filmed; this is what
/// every other output on `liblxa`, and no answer from the settings service, keeps. 2018's
/// `audiosink_new` is read the same way (all cells not measured); `lgaudiosink` has no such part.
///
/// Filmed ON THE DEV SET ONLY (a `liblxa` set, webOS 4.10.2; flash and beep every 2.5 s, offset = beep minus flash,
/// negative = audio early, FLAC clip): TV speakers, no correction -0.377 s; 330 ms -0.089; 390 ms
/// +0.047 (24 fps camera); 400 ms -0.026 to -0.028 (30 fps), and in a later run -0.03 to +0.08; the
/// set's own decoder on a DTS clip -0.02. Bluetooth headphones (`bt_soundbar`), earcup against the
/// phone's microphone: 400 ms +0.13 to +0.19 s late; 250 ms +0.017 s, three of three; the set's own DTS
/// decoder -0.028. NOT measured: every other stack (`lgaudiosink`, 2018 and 2016/2017 included), any
/// other output (optical, ARC, wired headphones, LG Sound Sync), the user's "AV Sync Adjust" setting
/// (its effect on a mixer channel is not known), and a seek (read from the sink's code, not filmed: it
/// re-derives its render base at every SEGMENT and never from a buffer's pts, so the relative lead
/// survives).
///
/// Applied to the SINK pts of the audio lane's one Feed ([`audio_sink_pts`]), after the speed
/// division, and nowhere else: content-time bookkeeping stays in content time.
const PCM_SINK_LEAD_NS: i64 = 400_000_000;

/// The leads that were filmed on `liblxa` (the dev set), by the name the settings service gives the output
/// (`getSystemSettings {"category":"sound","keys":["soundOutput"]}`, `tv::sound`). The TV speakers
/// are listed so that "measured" is a property of the table, not of the default happening to equal
/// them.
const PCM_LEAD_BY_OUTPUT: [(&str, i64); 2] = [("tv_speaker", 400_000_000), ("bt_soundbar", 250_000_000)];

/// The lead for a PCM Load on `stack` with the sound output named `output` (`None`: no answer), and
/// whether that cell was measured: THE table, one cell per stack and output.
///
/// | Stack | Lead | Measured |
/// |---|---|---|
/// | [`AudioStack::Liblxa`] | follows the output: [`PCM_LEAD_BY_OUTPUT`] (400 TV speaker, 250 Bluetooth), else 400 | the two listed outputs |
/// | [`AudioStack::AudioSinkNew`] (2018) | the same figures | none |
/// | [`AudioStack::LgAudioSink`] | 400 on every output and for no answer | none |
/// | [`AudioStack::AdecSink`] (2016/2017), `Other`, `Unknown` | 0 whatever the output | none |
///
/// PURE.
fn pcm_output_lead(stack: AudioStack, output: Option<&str>) -> (i64, bool) {
    let by_output = || {
        output.and_then(|name| PCM_LEAD_BY_OUTPUT.iter().find(|(known, _)| *known == name)).map(|&(_, ns)| ns)
    };
    match stack {
        AudioStack::Liblxa => by_output().map_or((PCM_SINK_LEAD_NS, false), |ns| (ns, true)),
        AudioStack::AudioSinkNew => (by_output().unwrap_or(PCM_SINK_LEAD_NS), false),
        AudioStack::LgAudioSink => (PCM_SINK_LEAD_NS, false),
        AudioStack::AdecSink | AudioStack::Other | AudioStack::Unknown => (0, false),
    }
}

/// The audio lane's sink pts: content time `fp` moved onto the pipeline's timeline
/// ([`super::to_sink_ns`]) plus the Load's `pcm_lead_ns`. The lead is real time the set takes, so it
/// is added AFTER the speed division: at 1.5x a 400 ms lead is still 400 ms, not 267. PURE.
fn audio_sink_pts(fp: i64, pcm_lead_ns: i64) -> i64 {
    super::to_sink_ns(fp).saturating_add(pcm_lead_ns).max(0)
}

/// How a Load's audio lead is to be decided, made with the declared audio and the set's audio stack
/// by [`load_audio`] on the frame thread and finished by [`PcmLead::resolve`], where the sound output
/// may be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PcmLead {
    /// A compressed or absent track: the set's own decoder compensates, so no lead.
    Off,
    /// The `pcmavoffset` dev trigger's value, on the stack it was measured on; it wins over the table
    /// and over the stack.
    Override(i64, AudioStack),
    /// App-decoded audio on a stack with no lead (2016/2017's `adecsink`, Realtek, MStar, or not
    /// known): none was read from the firmware or measured there, so there is none, and the output is
    /// not read.
    NoLead(AudioStack),
    /// App-decoded audio on a stack whose lead is the same on every sound output: the table's answer
    /// for the stack, settled on the frame thread, with no read of the output.
    Fixed(AudioStack),
    /// App-decoded audio on a stack whose lead follows the active sound output (`liblxa`, 2018's
    /// `audiosink_new`), read on the Load thread; the stack is carried for the table and the log line.
    FromOutput(AudioStack),
}

/// What a plan came to: the lead and, for [`PcmLead::FromOutput`], the output it was read from.
struct ResolvedLead {
    ns: i64,
    plan: PcmLead,
    /// The output's name, or `None` when the plan did not ask or the settings service gave none.
    output: Option<String>,
}

impl PcmLead {
    /// What the Engine starts with: the lead the frame thread can settle without reading the output,
    /// and whether the Load thread's read is still owed (then the lead is final only once
    /// [`settle_pcm_lead`] has taken it). PURE.
    fn at_engine_start(self) -> (i64, bool) {
        match self {
            Self::Off | Self::NoLead(_) => (0, false),
            Self::Override(ns, _) => (ns, false),
            Self::Fixed(stack) => (pcm_output_lead(stack, None).0, false),
            Self::FromOutput(_) => (0, true),
        }
    }

    /// What the frame thread logs when it starts the Load: the plans it settles itself (the dev
    /// override, a stack with no lead, a stack whose lead is the same on every output). A lead that follows the output is logged by the Load thread
    /// with the output it read, so a Load never writes a second, guessed line first. PURE.
    fn start_log_line(self) -> Option<String> {
        match self {
            Self::Override(..) | Self::NoLead(_) | Self::Fixed(_) => self.resolve(|| None).log_line(),
            Self::Off | Self::FromOutput(_) => None,
        }
    }

    /// Settle the plan. `read_output` runs only for [`PcmLead::FromOutput`] (a compressed Load, one
    /// with the dev override and one on a stack with no lead never ask the platform) and is a blocking
    /// read, so only the Load thread may pass one that reaches the bus. PURE apart from that closure.
    fn resolve(self, read_output: impl FnOnce() -> Option<String>) -> ResolvedLead {
        match self {
            Self::Off | Self::NoLead(_) => ResolvedLead { ns: 0, plan: self, output: None },
            Self::Override(ns, _) => ResolvedLead { ns, plan: self, output: None },
            Self::Fixed(stack) => ResolvedLead { ns: pcm_output_lead(stack, None).0, plan: self, output: None },
            Self::FromOutput(stack) => {
                let output = read_output();
                let ns = pcm_output_lead(stack, output.as_deref()).0;
                ResolvedLead { ns, plan: self, output }
            }
        }
    }
}

impl ResolvedLead {
    /// The line the Load logs for its lead, once per Load; `None` for a track with no lead.
    fn log_line(&self) -> Option<String> {
        let ms = self.ns / 1_000_000;
        match self.plan {
            PcmLead::Off => None,
            PcmLead::Override(_, stack) => {
                Some(format!("pcm: audio sink pts +{ms}ms (pcmavoffset override, stack {})", stack.label()))
            }
            PcmLead::NoLead(stack) => {
                Some(format!("pcm: audio sink pts +{ms}ms (stack {}, not measured on this chip)", stack.label()))
            }
            PcmLead::Fixed(stack) => {
                let note = if pcm_output_lead(stack, None).1 { "" } else { ", not measured" };
                Some(format!("pcm: audio sink pts +{ms}ms (stack {}, every output{note})", stack.label()))
            }
            PcmLead::FromOutput(stack) => {
                let measured = pcm_output_lead(stack, self.output.as_deref()).1;
                let name = self.output.as_deref().unwrap_or("unknown");
                let note = if measured { "" } else { ", not measured" };
                let default_ms = PCM_SINK_LEAD_NS / 1_000_000;
                Some(format!(
                    "pcm: audio sink pts +{ms}ms (stack {}, output {name}, default {default_ms}ms{note})",
                    stack.label()
                ))
            }
        }
    }
}

/// Resolve `plan` on the calling (Load) thread with the real platform read, and log what it decided.
/// The one place the platform is asked for the output.
pub(super) fn resolve_pcm_lead_on_load_thread(plan: PcmLead) -> Option<i64> {
    if !matches!(plan, PcmLead::FromOutput(_)) {
        return None;
    }
    let resolved = plan.resolve(plx_platform::tv::sound::output);
    if let Some(line) = resolved.log_line() {
        log(&line);
    }
    Some(resolved.ns)
}

/// The plan for a Load that declared `audio` on a set whose audio stack is `stack`, given the dev
/// override's text (milliseconds): nothing for anything but `"PCM"` (the override moves only the PCM
/// lead), the override when it is an integer (on any stack: it is how another chip gets measured),
/// otherwise no lead unless the stack follows the sound output (`liblxa`, 2018's `audiosink_new`:
/// [`PcmLead::FromOutput`] with the stack) or is the `lgaudiosink` one, where the lead is the same on
/// every output and needs no read. PURE.
fn pcm_lead_plan(audio: &str, override_ms: Option<&str>, stack: AudioStack) -> PcmLead {
    if audio != "PCM" {
        return PcmLead::Off;
    }
    if let Some(ms) = override_ms.and_then(|s| s.trim().parse::<i64>().ok()) {
        return PcmLead::Override(ms.saturating_mul(1_000_000), stack);
    }
    match stack {
        AudioStack::Liblxa | AudioStack::AudioSinkNew => PcmLead::FromOutput(stack),
        AudioStack::LgAudioSink => PcmLead::Fixed(stack),
        AudioStack::AdecSink | AudioStack::Other | AudioStack::Unknown => PcmLead::NoLead(stack),
    }
}

/// What a Load declares for its audio, and the plan for the sink lead that follows from it: ONE
/// value, so the payload's `"audio"` name and the Engine's lead cannot disagree. PURE.
struct LoadAudio {
    /// The LG-side name the payload carries; `"PCM"` for app-decoded audio.
    name: &'static str,
    /// The `dg_load_a` diagnostics code for the codec (`audio_payload_codec`).
    diagnostic: u8,
    /// [`pcm_lead_plan`] of `name` on the set's stack.
    lead: PcmLead,
}

/// [`LoadAudio`] for a track of `codec` at `session_rate` (permille); `None` only at 1x, when the pipeline has
/// no name for the codec and the app does not decode it. A speed session decodes and stretches the sound in the app
/// (`ff::SpeedDecoder`), so at any speed but 1x every codec declares `"PCM"`; so does a track the app
/// decodes itself at 1x (`app_pcm`, the shape [`crate::route::stream_pcm`] declared: DTS under the
/// lossy gate, FLAC and the like), whatever its codec, and that name is what the lead plan is made
/// from, so every Load that declares `"audio":"PCM"` is planned a lead. `lead_override_ms` is
/// the dev trigger's text and `stack` the set's audio stack ([`plx_platform::devcaps::audio_stack`]).
fn load_audio(
    codec: &str, session_rate: i32, app_pcm: Option<&crate::route::PcmFormat>, lead_override_ms: Option<&str>,
    stack: AudioStack,
) -> Option<LoadAudio> {
    let declared = audio_declaration(codec, app_pcm);
    // A speed session declares PCM whatever the codec is called, so it needs no payload name for it
    // (`mp3`, `vorbis` and `truehd` have none): its diagnostic code stays the codec's when it has one.
    let (name, diagnostic) = if session_rate != super::RATE_NORMAL {
        ("PCM", declared.map_or(5, |(_, code)| code))
    } else {
        declared?
    };
    Some(LoadAudio { name, diagnostic, lead: pcm_lead_plan(name, lead_override_ms, stack) })
}

/// Take the output's lead from the Load's return, once, before the first audio unit of this Load is
/// fed. `false` while the Load has not returned (the audio waits; nothing can be fed before it
/// anyway). A lead that was settled on the frame thread has nothing to take.
fn settle_pcm_lead(eng: &mut Engine) -> bool {
    if !eng.pcm_lead_pending {
        return true;
    }
    let Some(ns) = SHARED.native_load_pcm_lead(eng.native_epoch) else {
        return false;
    };
    eng.pcm_sink_lead_ns = ns;
    eng.pcm_lead_pending = false;
    true
}

fn hls_candidate_snapshot_stable(start: u64, end: u64) -> bool {
    start & 1 == 0 && start == end
}

/// Arm a prime at the current physical playhead without moving the clock. Runtime rebuffer and a
/// viewer Resume from a queued stream are the same pipeline boundary: Starfish is already Paused,
/// accepted media may remain ahead of the presented tail, and both application queues may continue
/// the same validated timeline behind it.
pub fn arm_live_clock_prime(eng: &mut Engine) {
    let presented = SHARED.pres_fed.load(Ordering::Relaxed);
    eng.seek_base_pts = if presented == PRES_NONE {
        eng.max_fed_video_pts.min(eng.max_fed_audio_pts.max(0))
    } else {
        presented
    };
    eng.prime_play = true;
}

/// **Start the clock once both lanes are buffered past the seek base** — video to [`PRIME_NS`]
/// AND audio to [`PRIME_AUDIO_NS`], with a video-only escape at [`PRIME_VIDEO_MAX_NS`] so an
/// audioless or briefly-starved stream still starts.
///
/// **This is called from TWO places, and the second one is the whole point.** It used to live
/// inline in [`feed_stream`]'s per-AU loop and nowhere else, which made it unreachable in exactly
/// the case that matters. A pump tick runs the video lane and then the audio lane, so when a whole
/// HLS segment lands at once the video lane drains all of it while `abuf` is still zero — the
/// `abuf` arm cannot be true yet, and a 2-second segment never reaches the 2500 ms video-only
/// escape. The audio lane then fills, but evaluating nothing. On every later tick `aq_pop` returns
/// null and the loop breaks before reaching the check, so Play ends up waiting on a video AU that
/// only the next segment can supply. On 2026-08-29 that held a viewer's picture for 94 seconds
/// with both lanes holding a playable buffer the entire time.
///
/// Calling it again at the end of [`feed_audio_lane`] closes that hole at the only moment both
/// lanes' extents are known. It is idempotent — `prime_play` latches false on the first success —
/// so the in-loop call is kept for the case it already served: a direct-play stream that reaches
/// the threshold mid-lane should start on that AU, not a tick later.
pub fn try_prime(mt: &MainThread, eng: &mut Engine) {
    if !eng.prime_play {
        return;
    }
    let Some(prime_kind) = SHARED.hls_prime_kind() else {
        return;
    };
    // If the clock is already held, do not restart it in the middle of an exploratory transaction.
    // The transaction's own deadline is consuming its discretionary B-max(R,D) budget; starting
    // the playhead too would spend that balance a second time. This marker never pauses a running
    // clock — it only delays release of an existing prime hold until commit/reject. Dropping the
    // trial marker releases this gate, and the next ordinary pump tick retries the prime.
    if SHARED.hls_trial_reserve_ms.load(Ordering::Acquire) >= 0 {
        return;
    }
    let candidate_generation = SHARED.hls_candidate_generation.load(Ordering::Acquire);
    if !hls_candidate_snapshot_stable(candidate_generation, candidate_generation) {
        return;
    }
    let accepted_vbuf = eng.max_fed_video_pts - eng.seek_base_pts;
    let accepted_abuf = eng.max_fed_audio_pts - eng.seek_base_pts;
    let observed_runway_ns = SHARED
        .hls_prime_runway_ms
        .load(Ordering::Relaxed)
        .max(0)
        .saturating_mul(1_000_000);
    // During an internal hold or a user ResumePrime, Starfish may stop accepting AUs while its own
    // source buffers are full. The already-demuxed application queues behind it are still
    // guaranteed playable media: they have passed transport, completeness, demux and timestamp
    // validation, and feeding is open while the clock remains held. Requiring the entire observed
    // runway to be Starfish-accepted can therefore deadlock at the native buffer cap with fuller
    // application queues behind it.
    //
    // Decoder prime remains an accepted-data condition. Only a continuation boundary (automatic
    // rebuffer or ResumePrime) reads the full pipeline high-water. Initial Load/seek remains
    // accepted-only because it is a fresh clock boundary whose old queued tail is not proof about
    // the newly loaded/landed timeline.
    let queued_runway = matches!(prime_kind, HlsPrimeKind::Rebuffer | HlsPrimeKind::Resume);
    let shift = SHARED.pts_shift.load(Ordering::Relaxed);
    let queued_video = SHARED.hls_video_tail_ns.load(Ordering::Acquire);
    let queued_audio = SHARED.hls_audio_tail_ns.load(Ordering::Acquire);
    let playable_video_high = if queued_runway && queued_video >= 0 {
        eng.max_fed_video_pts
            .max(queued_video.saturating_add(shift))
    } else {
        eng.max_fed_video_pts
    };
    let playable_audio_high = if queued_runway && queued_audio >= 0 {
        eng.max_fed_audio_pts
            .max(queued_audio.saturating_add(shift))
    } else {
        eng.max_fed_audio_pts
    };
    let playable_vbuf = playable_video_high - eng.seek_base_pts;
    let playable_abuf = playable_audio_high - eng.seek_base_pts;
    // An A/V stream requires both lanes even while audio is temporarily empty. Only a segment the
    // demuxer proved audioless may use the video-only escape; `abuf <= 0` alone is starvation, not
    // evidence that no audio exists.
    let audio_expected = SHARED.hls_audio_expected.load(Ordering::Acquire) || playable_abuf > 0;
    let decoder_ready = if audio_expected {
        accepted_vbuf >= PRIME_NS && accepted_abuf >= PRIME_AUDIO_NS
    } else {
        accepted_vbuf >= PRIME_VIDEO_MAX_NS
    };
    let recovery = SHARED.hls_recovery();
    // A downshift has no discretionary trial reserve, yet it can queue candidate AUs while an
    // internal hold is active. Validate one seqlock generation around the ENTIRE tail/decoder/
    // recovery snapshot: an odd or changed value means candidate publication overlapped it. A
    // bool cannot prove this because a complete false->true->false transition is an ABA. Re-read
    // the up-trial gate at the same final boundary so neither transaction type has a check/use gap.
    if SHARED.hls_trial_reserve_ms.load(Ordering::Acquire) >= 0
        || !hls_candidate_snapshot_stable(
            candidate_generation,
            SHARED.hls_candidate_generation.load(Ordering::Acquire),
        )
    {
        return;
    }
    let balanced_runway_ready = if audio_expected {
        playable_vbuf >= observed_runway_ns && playable_abuf >= observed_runway_ns
    } else {
        playable_vbuf >= observed_runway_ns
    };
    let boundary_ready = match prime_kind {
        HlsPrimeKind::Rebuffer => {
            let playable = if audio_expected {
                playable_vbuf.min(playable_abuf)
            } else {
                playable_vbuf
            };
            recovery.ready(playable)
        }
        HlsPrimeKind::Fresh | HlsPrimeKind::Resume => balanced_runway_ready,
    };
    if decoder_ready && boundary_ready && TX.feed_allowed() {
        let Some((play_token, recovery)) =
            SHARED.reserve_hls_prime_play(prime_kind, candidate_generation, recovery)
        else {
            return;
        };
        // A paused preroll plays one picture for a viewer who is paused: keep that window silent.
        if TX.seek_preroll_active() {
            super::mute_for_preroll(mt);
        }
        let played = unsafe { sink().play(mt) };
        match SHARED.complete_hls_prime_play(play_token, played != 0) {
            HlsPlayCompletion::Accepted { resume_acb } => {
                if resume_acb {
                    super::acb_mirror_playstate_at(mt, eng.stage, true);
                }
                eng.prime_play = false;
                SHARED.seeking.store(false, Ordering::Relaxed); // playback resumed at the new position → HUD spinner off
                if prime_kind == HlsPrimeKind::Rebuffer {
                    log(&format!(
                        "primed: v={}ms a={}ms recovery_n={} debt={}ms runway={}ms -> Play",
                        playable_vbuf / 1_000_000,
                        playable_abuf / 1_000_000,
                        recovery.completed,
                        recovery.debt_us / 1_000,
                        recovery.runway_us / 1_000,
                    ));
                } else {
                    log(&format!(
                        "primed: v={}ms a={}ms runway={}ms -> Play",
                        playable_vbuf / 1_000_000,
                        playable_abuf / 1_000_000,
                        observed_runway_ns / 1_000_000,
                    ));
                }
            }
            HlsPlayCompletion::Refused => {
                log("primed: Starfish refused Play; keeping the clock held and retrying");
            }
            HlsPlayCompletion::Stale => {
                log("primed: Play result lost its clock token; teardown/transition won");
            }
        }
    }
}

/// **One tick's feeding, as a unit: video lane, audio lane, then the prime attempt.**
///
/// The order is load-bearing and predates this function — the VIDEO lane owns the seek rebase, so
/// it must clear `rebase_pending` and publish `pts_shift` before the audio lane reads them in the
/// same tick. What is new is the third step.
///
/// [`try_prime`] is called HERE rather than at the end of [`feed_audio_lane`] because that
/// function returns early on `rebase_pending`, and a tail call would then be skipped on exactly
/// the ticks a reload passes through.
///
/// **The first version of this doc justified the placement with the audioless case, and that
/// argument is FALSE.** Every `Source::Stream` allocates BOTH queues whether or not audio is
/// produced, so `aq_audio == None` is not a state a real stream reaches; and with no audio a
/// 2-second segment cannot satisfy the `PRIME_VIDEO_MAX_NS` escape from this call either, so the
/// placement neither helps nor harms it. The reason for the wrapper is that one function should
/// own what a tick does to the lanes — not that it rescues a case it never sees.
/// **The two lane feeders are PRIVATE so that this is the only way in.** A review found the hole:
/// with them `pub`, reverting `pump` to two separate lane calls restored the livelock while
/// the regression test — which calls this wrapper — stayed green. Private, that revert does not
/// compile.
pub fn feed_both_lanes(mt: &MainThread, eng: &mut Engine) {
    feed_stream(mt, eng);
    feed_audio_lane(mt, eng);
    try_prime(mt, eng);
}

fn feed_stream(mt: &MainThread, eng: &mut Engine) {
    let qp = match eng.aq_video.as_mut() {
        Some(q) => &mut **q as *mut AuQueue,
        None => return,
    };
    let slice = FeedSlice::begin();
    let mut fed = 0;
    while fed < 120 {
        // Feed each AU, throttled to ~MAX_FEED_AHEAD_NS ahead of the presented position per lane
        // (see the throttle below) rather than greedily to BufferFull.
        if eng.pending_video.is_none() {
            let mut eof: c_int = 0;
            let n = crate::aq::aq_pop(qp, &mut eof);
            if n.is_null() {
                // true EOF (producer done + video lane drained): signal end-of-stream ONCE so the
                // pipeline drains its last frames instead of hanging on them (Kodi keys EOS to the
                // video drain). Keyed on the video lane only.
                if eof != 0 && !eng.eos_pushed && eng.stage >= Stage::Streaming {
                    unsafe { sink().push_eos(mt) };
                    eng.eos_pushed = true;
                    log("EOS pushed at true EOF");
                }
                // Nothing to send. THE case the read-out's Feed row exists to name: a dead
                // PRODUCER, which every other field on the panel cannot tell from a dead sink.
                SHARED.dg_feed_state.store(5, Ordering::Relaxed);
                break;
            }
            eng.pending_video = Some(AuBox(n));
        }
        let n = eng.pending_video.as_ref().unwrap().0;
        let (es, key, pts, len, data) = unsafe { crate::aq::au_fields(n) };
        if eng.rebase_pending {
            if es == 1 && key != 0 {
                let completing_in_place_seek = eng.flushed;
                let mut segment_installed = false;
                // In-place seek: drop a keyframe that landed well AHEAD of the target — it's a stale
                // frame from the pre-flush read position (the reopen+av_seek hasn't taken effect yet),
                // not the real post-seek keyframe. Capped so a failed av_seek can't hang the rebase.
                if eng.flushed {
                    let target = SHARED.seek_target_ns.load(Ordering::Relaxed);
                    let stale = target >= 0
                        && (pts - target > SEEK_STALE_AHEAD_NS
                            || target - pts > SEEK_STALE_BEHIND_NS);
                    if stale && eng.rebase_drops < MAX_REBASE_DROPS {
                        eng.rebase_drops += 1;
                        if eng.rebase_drops == 1 {
                            log(&format!(
                                "rebase: dropping stale kf pts={}s (target {}s)",
                                pts / 1_000_000_000,
                                target / 1_000_000_000
                            ));
                        }
                        eng.pending_video = None;
                        continue;
                    }
                }
                if eng.flushed && super::seek_mode() == SeekMode::NativeSeek {
                    // NATIVE in-place seek (#568): feed the REAL content PTS (pts_shift=0,
                    // disp_base=0 -> playpos = presented real pts = content time, as in the
                    // Segment branch below) and anchor the pipeline with LG's public
                    // `StarfishMediaAPIs::Seek(millis)`. Seek is a flushing `gst_element_seek`
                    // whose new segment starts at EXACTLY the millis passed, so the PTS fed from
                    // here on is on that timeline by construction; the pump skipped its own
                    // `flush()` for this mode and nothing may flush after this (a `flush()` is
                    // itself a seek to 0 and re-creates the stale-segment stall). `feed_stream`
                    // runs on the main thread and is the only Feed caller, and this keyframe is
                    // not fed yet, so Seek cannot race a Feed. Never a negative millis: Seek
                    // refuses it, and a negative PTS is unparseable by Feed.
                    SHARED.pts_shift.store(0, Ordering::Relaxed);
                    let ms = super::to_sink_ns(pts.max(0)) / 1_000_000;
                    let rv = unsafe { sink().seek(mt, ms) };
                    log(&format!("in-place seek(native): Seek({ms}) rv={rv}"));
                    if rv == 0 {
                        // A refused Seek can leave the pipeline refusing every Feed. Drop the rest
                        // of THIS SESSION's seeks to the reload path and redo THIS one as a
                        // reload at the user's target (the pump reads the mailbox next tick;
                        // `reload_at` REPLACES the engine). `eng.flushed`/`rebase_pending` stay
                        // set so the audio lane keeps holding and no AU reaches a pipeline that
                        // is rejecting Feed.
                        super::set_seek_mode(SeekMode::Reload);
                        // Latched across the reload this very seek is about to do, so the fresh
                        // session's `select_seek_mode` does not offer the same firmware another
                        // refused Seek on every later seek of this playback.
                        super::set_native_seek_refused(true);
                        log(
                            "in-place seek DOWNGRADE: native Seek refused -> reload-per-seek for \
                             the rest of this playback",
                        );
                        let tgt = SHARED.seek_target_ns.load(Ordering::Relaxed).max(0);
                        TX.seek_reqs.store(1, Ordering::Release);
                        TX.seek_to_ns.store(tgt, Ordering::Release);
                        eng.pending_video = None;
                        break;
                    }
                    segment_installed = true;
                    eng.flushed = false;
                } else if eng.flushed && super::seek_mode() != SeekMode::Segment {
                    // Flushed but not in a mode that anchors in place: a refused native `Seek`
                    // downgraded the session to `Reload` and the pump's reload (`reload_at`)
                    // is on its way. The private Segment path (`sf_pipeline`'s decompile-derived
                    // offsets, proven on webOS 4.5 only) must NEVER run here, so this keyframe
                    // anchors nothing: drop it and leave `flushed`/`rebase_pending` set, which
                    // keeps the audio lane holding until the reload replaces the engine.
                    eng.pending_video = None;
                    break;
                } else if eng.flushed {
                    // Kodi IN-PLACE seek (exact): feed the REAL content PTS (no rebase), tell the
                    // pipeline the real decode position, then inject a fresh GStreamer SEGMENT —
                    // this re-anchors the sink WITHOUT a reload/decoder re-init. disp_base=0 +
                    // pts_shift=0 → playpos = presented real pts = content time.
                    SHARED.pts_shift.store(0, Ordering::Relaxed);
                    let ok = unsafe { sink().set_time_to_decode(mt, super::to_sink_ns(pts)) };
                    // setTimeToDecode returns 0 on webOS<11 (it needs PausedState); fall back to
                    // the content-info path (loadSpi_getInfo + setContentInfo(ptsToDecode)), which
                    // re-anchors the decode position while Playing. Then always inject the fresh
                    // GStreamer SEGMENT so the sink re-bases instead of stalling.
                    let ci = if ok == 0 {
                        unsafe { sink().set_content_info(mt, super::to_sink_ns(pts)) }
                    } else {
                        1
                    };
                    let seg = unsafe { sink().send_segment(mt) };
                    segment_installed = seg != 0;
                    log(&format!("in-place seek: setTimeToDecode({pts}) rv={ok} setContentInfo={ci} sendSegment={seg}"));
                    if seg == 0 {
                        // The pipeline ptr wasn't reachable, so NO fresh GStreamer segment was
                        // injected and THIS seek's buffers are scheduled against the pre-seek
                        // segment — the stale-segment state that stops the sink draining and
                        // fills ~14.7 MB of upstream buffers in ~48 s (permanent BufferFull +
                        // "Playing error"; see reload_at's decompiled ground truth). So drop the
                        // rest of THIS SESSION's seeks to the reload path, which rebuilds the
                        // segment by construction; start_bufferfeed re-arms the flag for the next
                        // session (the SCOPE note in mod.rs argues why that is the right scope).
                        // Logged loudly because this is the single most consequential state
                        // change in the seek path and it used to be invisible: instant seeks
                        // silently became multi-second reloads with nothing in the event log
                        // naming the cause — only the `sendSegment=0` above, whose consequence
                        // was not stated anywhere.
                        super::set_seek_mode(SeekMode::Reload);
                        log(
                            "in-place seek DOWNGRADE: sendSegment=0 (CustomPipeline unreachable) \
                             -> reload-per-seek for the rest of this session",
                        );
                    }
                    eng.flushed = false;
                } else {
                    // reload / initial-resume seek: rebase the landed keyframe to fed-pts 0 (the
                    // fresh Load's pipeline expects a 0-based feed; disp_base carries the offset).
                    SHARED.pts_shift.store(-pts, Ordering::Relaxed);
                }
                eng.rebase_pending = false; // releases the AUDIO lane (which holds until this clears)
                eng.seek_base_pts = pts + SHARED.pts_shift.load(Ordering::Relaxed); // fed-pts base
                                                                                    // Installing a SEGMENT is not acceptance of the AU which establishes it. Keep
                                                                                    // callbacks fenced across BufferFull/error and arm only below, after this exact
                                                                                    // retained keyframe has returned `O` from sf_feed.
                eng.presentation_rearm_pending = completing_in_place_seek && segment_installed;
                log(&format!(
                    "rebase: first post-seek keyframe pts={pts} -> pts_shift={}",
                    SHARED.pts_shift.load(Ordering::Relaxed)
                ));
            } else {
                eng.pending_video = None; // drop pre-keyframe AUs
                continue;
            }
        }
        let mut fp = pts + SHARED.pts_shift.load(Ordering::Relaxed) + pts_nudge_ns();
        if fp < eng.max_fed_video_pts - STALE_BACKJUMP_NS {
            eng.pending_video = None; // stale (a big backward jump)
            continue;
        }
        if fp < 0 {
            fp = 0;
        }
        // Feed-ahead throttle: don't feed an AU that's already more than its lane's budget ahead
        // of the presented position — keep it pending and retry once the pipeline presents more.
        // Skipped while priming (to reach PRIME_NS before Play; the per-tick slice below is waived
        // until the lane holds its prime depth, then spreads the rest). Each lane's queue is
        // pts-ordered, so if the head is over budget everything behind it is too; breaking is right.
        let pres = eng.feed_pace.reference_at(
            SHARED.pres_fed.load(Ordering::Relaxed),
            super::vclock_ms(),
            SHARED.applied_rate_permille.load(Ordering::Relaxed),
        );
        if !eng.prime_play {
            let budget = if es == 1 {
                MAX_FEED_AHEAD_NS
            } else {
                MAX_FEED_AHEAD_NS + AUDIO_SLACK_NS
            };
            if pres != PRES_NONE && fp - pres > budget {
                SHARED.dg_feed_state.store(4, Ordering::Relaxed); // waiting for a frame
                break;
            }
        }
        // Out of this tick's slice: the AU stays pending, as on a throttle stop, for the next tick.
        // A lane short of media is exempt: it is priming or its lead is nearly gone.
        let starved = lane_starved(
            eng.prime_play,
            eng.max_fed_video_pts,
            eng.seek_base_pts,
            PRIME_NS,
            pres,
        );
        if slice.spent(fed, starved) {
            break;
        }
        let presentation_probe = if eng.presentation_rearm_pending {
            let armed = SHARED.begin_native_presentation_probe(eng.native_epoch);
            if !armed {
                log("rebase: native presentation epoch retired before post-seek Feed");
            }
            armed
        } else {
            false
        };
        let sink_pts = super::to_sink_ns(fp);
        let r = plx_base::diag::spans::span("sfv", || unsafe { sink().feed(mt, data, len as u32, sink_pts, es) });
        if presentation_probe {
            if (r as u8) == b'O' {
                if !SHARED.commit_native_presentation_probe(eng.native_epoch, |num| {
                    super::sf_on_event_inner(0, num, std::ptr::null(), super::shared::EventScope::Live)
                }) {
                    log("rebase: native presentation probe lost its epoch after accepted Feed");
                }
            } else if !SHARED.reject_native_presentation_probe(eng.native_epoch) {
                log("rebase: native presentation probe lost its epoch after rejected Feed");
            }
        }
        // **Only an ACCEPTED feed advances the mark.** `max_fed_*_pts` is the high-water of what
        // the PIPELINE holds and `try_prime` starts the clock off it, so counting a rejected AU
        // primes against media the decoder never received. This advanced before the reply was read
        // for as long as the check lived inside this loop — where it was video-only, and one
        // frame early. `feed_both_lanes` made the same slip reachable from the AUDIO lane, which
        // the old check could not prime from at all, so the latent sloppiness became a defect.
        if (r as u8) == b'O' && fp > eng.max_fed_video_pts {
            eng.max_fed_video_pts = fp;
        }
        // prime-then-play: once PRIME_NS of the fresh (post-seek/resume) stream is buffered,
        // start the clock. The pipeline was paused through the reopen gap, so it now presents
        // from the seek point in A/V sync instead of fast-forwarding to a clock that ran ahead.
        // Start the clock once BOTH lanes are buffered past the seek base: video to PRIME_NS AND
        // audio to PRIME_AUDIO_NS. Priming on video ALONE started the audioSync MASTER clock with
        // an empty audio queue, so a rapid-seek drain could leave audio silent until the next seek.
        // The video-buffer fallback still starts an audioless/briefly-starved stream (no hang).
        try_prime(mt, eng);
        // The log cadence counts ATTEMPTS (its `reply=` field is the only record of a rejected
        // feed, and tests/run.py greps `feed v#`), so it keeps its own counter. VTOT counts what
        // was ACCEPTED — see below.
        if es == 1 {
            let v = VATT.fetch_add(1, Ordering::Relaxed) + 1;
            if v <= 4 || v % 100 == 0 {
                let qb = crate::aq::aq_bytes(qp);
                log(&format!(
                    "feed v#{v} sz={len} fed={fp} reply={} qbytes={qb}",
                    r as u8 as char
                ));
            }
        }
        if (r as u8) != b'O' {
            // 'B' BufferFull -> keep pending, retry next tick (VIDEO lane only)
            SHARED
                .dg_feed_state
                .store(if (r as u8) == b'B' { 2 } else { 3 }, Ordering::Relaxed);
            break;
        }
        if eng.presentation_rearm_pending {
            eng.presentation_rearm_pending = false;
        }
        SHARED.dg_feed_state.store(1, Ordering::Relaxed); // accepting
                                                          // COUNTED HERE, below the reply test, so `Fed` means AUs the pipeline TOOK. Counted above
                                                          // it, a permanently-full sink re-offered and re-counted the same retained AU every tick,
                                                          // and the read-out showed a brisk feed rate through a stall.
        if es == 1 {
            VTOT.fetch_add(1, Ordering::Relaxed);
        }
        eng.pending_video = None;
        fed += 1;
    }
}

/// Whether an audio AU at fed pts `fp` is in its timeline's lead-in: it is the AU the demuxer
/// marked as the first since its open or seek (`opens`), or lies within [`AUDIO_LEAD_IN_NS`] past
/// the one that was accepted (`start`). Lead-in audio is fed ahead of the clock: neither the
/// stale-ahead drop nor the feed-ahead throttle applies to it.
fn audio_lead_in(fp: i64, opens: bool, start: Option<i64>) -> bool {
    opens || start.is_some_and(|s| fp >= s && fp - s < AUDIO_LEAD_IN_NS)
}

/// AUDIO lane feeder (two-lane ff path only). Independent of the video lane: its own queue, its
/// own fed-pts high-water, its own BufferFull retry — so a video BufferFull never starves audio.
/// HOLDS while a seek rebase is pending (the VIDEO lane sets pts_shift on its first post-seek
/// keyframe; feeding audio before that would use a stale shift → A/V desync). No prime/Play here —
/// only the video lane starts the clock. Called AFTER feed_stream each tick, so a same-tick rebase
/// is already visible.
fn feed_audio_lane(mt: &MainThread, eng: &mut Engine) {
    if eng.rebase_pending {
        return; // wait for the video lane to publish pts_shift
    }
    if !settle_pcm_lead(eng) {
        return; // the output's lead is not here yet: stamping with a guess would move later
    }
    let qp = match eng.aq_audio.as_mut() {
        Some(q) => &mut **q as *mut AuQueue,
        None => return,
    };
    // hoisted out of the loop: pts_shift is stable once rebase clears (only the video lane's rebase
    // arm writes it, on this same thread), and one pres_fed sample per tick is plenty against the
    // multi-second audio budget.
    let shift = SHARED.pts_shift.load(Ordering::Relaxed);
    let pres = eng.feed_pace.reference_at(
        SHARED.pres_fed.load(Ordering::Relaxed),
        super::vclock_ms(),
        SHARED.applied_rate_permille.load(Ordering::Relaxed),
    );
    let slice = FeedSlice::begin();
    let mut fed = 0;
    while fed < 120 {
        if eng.pending_audio.is_none() {
            let mut eof: c_int = 0;
            let n = crate::aq::aq_pop(qp, &mut eof);
            if n.is_null() {
                break;
            }
            eng.pending_audio = Some(AuBox(n));
        }
        let n = eng.pending_audio.as_ref().unwrap().0;
        let (es, _key, pts, len, data) = unsafe { crate::aq::au_fields(n) };
        let mut fp = pts + shift;
        let first_pts = SHARED.first_audio_pts.load(Ordering::Acquire);
        let opens = first_pts != i64::MIN && pts == first_pts;
        let lead_in = audio_lead_in(fp, opens, eng.audio_start_fp);
        // Stale drifted audio from before a seek's reopen: far AHEAD of the freshly-anchored video.
        // Drop it (else it poisons max_fed_audio_pts and stalls the audio clock → playback sticks).
        // A late-starting track is far ahead too, legitimately, so it is measured from its own
        // start as well as from the video (see AUDIO_LEAD_IN_NS).
        let stale_ref = eng.max_fed_video_pts.max(eng.audio_start_fp.unwrap_or(i64::MIN));
        if !lead_in && eng.max_fed_video_pts > 0 && fp > stale_ref + AUDIO_STALE_AHEAD_NS {
            eng.pending_audio = None;
            continue;
        }
        if fp < eng.max_fed_audio_pts - STALE_BACKJUMP_NS {
            eng.pending_audio = None; // stale (a big backward jump)
            continue;
        }
        if fp < 0 {
            fp = 0;
        }
        if !eng.prime_play
            && !lead_in
            && pres != PRES_NONE
            && fp - pres > MAX_FEED_AHEAD_NS + AUDIO_SLACK_NS
        {
            break;
        }
        let starved = lane_starved(eng.prime_play, eng.max_fed_audio_pts, eng.seek_base_pts, PRIME_AUDIO_NS, pres);
        if slice.spent(fed, starved) {
            break;
        }
        let sink_pts = audio_sink_pts(fp, eng.pcm_sink_lead_ns);
        let r = plx_base::diag::spans::span("sfa", || unsafe { sink().feed(mt, data, len as u32, sink_pts, es) });
        // Accepted only — see the video lane's note.
        if (r as u8) == b'O' && fp > eng.max_fed_audio_pts {
            eng.max_fed_audio_pts = fp;
        }
        if (r as u8) == b'O' && opens {
            if eng.audio_start_fp.is_none() && fp - eng.max_fed_video_pts > MAX_FEED_AHEAD_NS {
                log(&format!(
                    "feed: audio starts {}ms after the video fed so far; fed ahead of the clock",
                    (fp - eng.max_fed_video_pts) / 1_000_000
                ));
            }
            eng.audio_start_fp = Some(fp);
        }
        let a = AATT.fetch_add(1, Ordering::Relaxed) + 1;
        if a <= 4 || a % 200 == 0 {
            let qb = crate::aq::aq_bytes(qp);
            log(&format!(
                "feed a#{a} sz={len} fed={fp} reply={} qbytes={qb}",
                r as u8 as char
            ));
        }
        if (r as u8) == b'O' {
            ATOT.fetch_add(1, Ordering::Relaxed); // accepted, not attempted — see feed_stream
            let mut sizes = super::native_stall::SIZES.lock().unwrap_or_else(|e| e.into_inner());
            if sizes.open() && crate::ff::native_audio::fed().is_some() {
                if let Some(line) = sizes.record(len as u32) {
                    log(&format!("native audio: packet sizes of the first AUs fed: {line}"));
                }
            }
        }
        if (r as u8) != b'O' {
            break; // 'B' BufferFull -> keep pending, retry next tick (AUDIO lane only)
        }
        eng.pending_audio = None;
        fed += 1;
    }
}

/// feed the looped `sample.h264` validation sample from the install's runtime root
/// (continuous PTS @ 23.976).
pub fn feed_sample(mt: &MainThread, eng: &mut Engine) {
    let s = match &mut eng.source {
        Source::Sample(s) => s,
        _ => return,
    };
    let naus = s.au.len();
    if naus < 2 {
        return;
    }
    let mut fed = 0;
    while fed < 60 {
        if s.next >= naus - 1 {
            s.next = 0;
            s.loops += 1;
        }
        let off = s.au[s.next];
        let end = s.au[s.next + 1];
        let pts = (s.loops * (naus as i64 - 1) + s.next as i64) * 41708333;
        let r = unsafe { sink().feed(mt, s.data[off..].as_ptr(), (end - off) as u32, pts, 1) };
        if (r as u8) != b'O' {
            break;
        }
        s.next += 1;
        fed += 1;
    }
}

/// Whether the decompiled StarfishMediaAPIs offsets (see the NOR comment in `start_bufferfeed_inner`)
/// may be trusted for in-place seek. The old-ABI `Feed` is the primary gate: it proves
/// libplayerAPIs is the pre-C++11 build, so 3.9.x is excluded without listing releases. A known
/// release below 4 is refused too; `major == 0` (release unknown) defers to the Feed evidence.
fn inplace_layout_known(major: u32, feed_abi_is_cow: bool) -> bool {
    !feed_abi_is_cow && (major == 0 || major >= 4)
}

/// **Is LG's public `Seek` device-tested where the private layout is not known?** Only on a KNOWN
/// webOS 3 release: 80 seeks on platform release 3.9.3 (`docs/webos5-port.md`, "The public
/// `Seek`"). It reads the release alone, not the Feed ABI: every 3.x library seen has the old
/// (COW) one, and a release-3 set with the cxx11 Feed would get `Seek` too. webOS 2, an old-ABI
/// 4.x build and an unknown release (`major == 0`) keep reloading, because an accepted `Seek` that
/// anchors wrong is caught by no net ([`select_seek_mode`]).
fn old_abi_seek_tested(major: u32) -> bool {
    major == 3
}

/// [`select_seek_mode`] for a session that is starting: a `NativeSeek` this playback already had
/// REFUSED stays `Reload` (the latch `feed_stream` sets, which `reload_at` preserves and a fresh
/// play clears), instead of paying reopen + keyframe + refused `Seek` + full `Load` on every seek.
pub(super) fn session_seek_mode(
    layout_known: bool,
    old_abi_seek: bool,
    window_mode: c_int,
    dev: Option<SeekMode>,
) -> SeekMode {
    match select_seek_mode(layout_known, old_abi_seek, window_mode, dev) {
        SeekMode::NativeSeek if super::native_seek_refused() => SeekMode::Reload,
        mode => mode,
    }
}

/// How a direct-play seek re-anchors the pipeline in a new session (see [`SeekMode`]).
///
/// - `VP_EXPORTED` (webOS 5+) -> [`SeekMode::NativeSeek`]: LG's PUBLIC `StarfishMediaAPIs::Seek`.
///   The Kodi in-place path (`Segment`) is NOT used there: it reaches LG-private C++ objects
///   through two decompile-derived offsets (`StarfishMediaAPIs::player` at object+0x4c, then
///   `AbstractPlayer::pipeline` at +0x04, plus `MEDIA_CUSTOM_CONTENT_INFO`'s `ptsToDecode` at
///   +0x28) read off a webOS 4.5 binary, which nothing in a symbol table can confirm on another
///   firmware: a field added to StarfishMediaAPIs by any 5.x build moves +0x4c and the very next
///   seek dereferences whatever now lives there. `Seek` is an exported symbol, present on every
///   firmware 1.2 to 11.2, so it assumes no layout at all.
/// - A known layout without `VP_EXPORTED` (webOS 4.x) -> [`SeekMode::Segment`], unchanged.
/// - NOT `Segment` when the library is the pre-C++11 build (`inplace_layout_known` false): its
///   StarfishMediaAPIs is a different object from the 4.5 one, and the first fast-forward on a
///   3.4.0 set crashed through those offsets. The old-ABI Feed (`sf_feed_abi_is_cow`) is the
///   direct evidence, and it covers 3.9.x too. A KNOWN release below 4 is refused as well; an
///   UNKNOWN one (`major == 0`, os_info unreadable) with the C++11 Feed keeps in-place seek, as
///   before #578, because that Feed says the library is 4.x or later.
/// - A layout-unknown library on the ACB plane in a KNOWN webOS 3 release
///   ([`old_abi_seek_tested`]) -> [`SeekMode::NativeSeek`] too. `Seek` takes a `const char*`, so
///   its symbol and calling convention are the same on both C++ ABIs; that it also behaves the
///   same rests on the device test in `docs/webos5-port.md` ("The public `Seek`"), not on the
///   decompile, which is of 4.5.
///   The nets are the ones webOS 5+ has: a refused `Seek` drops the playback to `Reload`
///   (`native_seek_refused`), and a seek that does not anchor within `SEEK_STUCK_MS` is retried,
///   then reloaded. Neither catches a `Seek` that is accepted and then anchors wrong (the
///   stale-segment stall, about 48 s later), which is why the release is gated to the one tested.
/// - Any other layout-unknown library (webOS 2, an old-ABI 4.x build, an unknown release) and no
///   video plane (`VP_NONE`) -> [`SeekMode::Reload`], as before.
/// `dev` is the `plxnative-seekmode` dev trigger and wins when present, so a webOS 4.5 set can
/// exercise `NativeSeek`.
pub(super) fn select_seek_mode(
    layout_known: bool,
    old_abi_seek: bool,
    window_mode: c_int,
    dev: Option<SeekMode>,
) -> SeekMode {
    if let Some(forced) = dev {
        return forced;
    }
    if window_mode == super::VP_EXPORTED {
        SeekMode::NativeSeek
    } else if layout_known {
        SeekMode::Segment
    } else if old_abi_seek && window_mode == super::VP_ACB {
        SeekMode::NativeSeek
    } else {
        SeekMode::Reload
    }
}

/// `/tmp/plxnative-seekmode`: `native`, `segment` or `reload`. `None` without the `devtriggers`
/// feature (`devtrig::read` folds away), so a release build cannot be steered.
fn dev_seek_mode() -> Option<SeekMode> {
    match plx_base::devtrig::read("seekmode")?.trim() {
        "native" => Some(SeekMode::NativeSeek),
        "segment" => Some(SeekMode::Segment),
        "reload" => Some(SeekMode::Reload),
        _ => None,
    }
}

#[cfg(test)]
mod feed_pace_tests {
    use super::{FeedPace, FEED_PACE_CAP_MS, MAX_FEED_AHEAD_NS, PRES_NONE};

    const MS: i64 = 1_000_000;

    /// Video AUs (24 fps, 41.7 ms apart) the throttle admits at each 60 Hz frame of a 1x clock
    /// whose position reports arrive every 200 ms — with `reference` as the throttle's position.
    fn admitted_per_frame(reference: impl Fn(&mut FeedPace, i64, u32) -> i64) -> Vec<u32> {
        let mut pace = FeedPace::default();
        let au_ns = 41_708_333i64;
        let mut next_pts = MAX_FEED_AHEAD_NS; // the lane starts full
        let mut out = Vec::new();
        for frame in 0..120u32 {
            let now = 1_000 + frame * 1000 / 60;
            let true_pos = (now as i64 - 1_000) * MS;
            let pres = true_pos - true_pos % (200 * MS); // the 5 Hz step function
            let refpos = reference(&mut pace, pres, now);
            let mut n = 0;
            while next_pts - refpos <= MAX_FEED_AHEAD_NS {
                next_pts += au_ns;
                n += 1;
            }
            out.push(n);
        }
        out
    }

    /// **Regression: the 5 Hz feed burst.** Against the raw report the lane opens 200 ms at a
    /// time: ~5 AUs land on one frame and none on the eleven around it. Run on at 1x it never
    /// admits more than one video AU a frame, and admits the same media over the same time.
    #[test]
    fn the_reference_spreads_the_five_hertz_burst_across_frames() {
        let raw = admitted_per_frame(|_, pres, _| pres);
        let paced = admitted_per_frame(|p, pres, now| p.reference(pres, now));
        assert!(raw.iter().max().copied().unwrap_or(0) >= 4, "the raw step feeds in bursts: {raw:?}");
        assert!(paced.iter().all(|&n| n <= 1), "paced: at most one AU a frame: {paced:?}");
        let (r, p): (u32, u32) = (raw.iter().sum(), paced.iter().sum());
        assert!(p.abs_diff(r) <= 6, "same media over the same two seconds: raw {r}, paced {p}");
    }

    /// A `now` BEFORE the stamp of the last report (the throttle read its clock earlier than the
    /// report was first seen) is no run-on, not a full-cap one.
    #[test]
    fn a_clock_behind_the_report_stamp_does_not_run_on() {
        let mut pace = FeedPace::default();
        assert_eq!(pace.reference(5_000 * MS, 1_000), 5_000 * MS);
        assert_eq!(pace.reference(5_000 * MS, 900), 5_000 * MS);
    }

    /// A clock that stops reporting new positions (paused, stalled) opens the lane by at most
    /// the cap, never without bound; `PRES_NONE` passes through and forgets the last report.
    #[test]
    fn a_frozen_report_runs_on_at_most_the_cap_and_none_passes_through() {
        let mut pace = FeedPace::default();
        assert_eq!(pace.reference(5_000 * MS, 100), 5_000 * MS);
        assert_eq!(pace.reference(5_000 * MS, 150), 5_050 * MS);
        assert_eq!(pace.reference(5_000 * MS, 100_000), 5_000 * MS + FEED_PACE_CAP_MS as i64 * MS);
        assert_eq!(pace.reference(PRES_NONE, 100_001), PRES_NONE);
        assert_eq!(pace.reference(5_000 * MS, 100_002), 5_000 * MS, "a fresh report restarts the run-on");
    }

    /// At 1.5x the playhead moves 1.5 ms of media per wall-clock ms between reports, so the
    /// estimate runs on that much faster; capped by the same wall-clock bound.
    #[test]
    fn the_run_on_follows_the_pipeline_rate() {
        let mut pace = FeedPace::default();
        assert_eq!(pace.reference_at(5_000 * MS, 100, 1500), 5_000 * MS);
        assert_eq!(pace.reference_at(5_000 * MS, 150, 1500), 5_075 * MS);
        assert_eq!(pace.reference_at(5_000 * MS, 200, 500), 5_050 * MS, "half speed runs on at half");
        assert_eq!(
            pace.reference_at(5_000 * MS, 100_000, 2000),
            5_000 * MS + 2 * FEED_PACE_CAP_MS as i64 * MS,
            "the cap is wall-clock time; at 2x it covers twice the media"
        );
        assert_eq!(
            FeedPace::default().reference(5_000 * MS, 100),
            FeedPace::default().reference_at(5_000 * MS, 100, crate::player::RATE_NORMAL)
        );
    }
}

#[cfg(test)]
mod native_lifecycle_proof_tests {
    use super::{native_object_disposition, NativeObjectDisposition};

    #[test]
    fn destructor_requires_every_independent_runtime_proof() {
        assert_eq!(
            native_object_disposition(true, true, true),
            NativeObjectDisposition::Destroy,
        );
        for (unload_completed, callback_gate_proven, rust_epoch_retired) in [
            (false, true, true),
            (true, false, true),
            (true, true, false),
            (false, false, false),
        ] {
            assert_eq!(
                native_object_disposition(
                    unload_completed,
                    callback_gate_proven,
                    rust_epoch_retired,
                ),
                NativeObjectDisposition::Quarantine,
            );
        }
    }
}

#[cfg(all(test, feature = "hostsim"))]
mod native_lifecycle_host_seam_tests {
    use super::*;

    struct FreshProcess;

    impl Drop for FreshProcess {
        fn drop(&mut self) {
            crate::player::ffi_host::reset_native_lifecycle_for_test();
            crate::player::ffi_host::force_clocksink_for_test(false);
            SHARED.reset_session();
        }
    }

    #[test]
    fn host_seam_destroys_only_with_evidence_and_latches_quarantine() {
        let _serial = plx_base::testlock::serial();
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        SHARED.reset_session();
        let _fresh_process = FreshProcess;
        let mt = unsafe { MainThread::assume() };

        let first = SHARED.begin_native_session().expect("first native epoch");
        assert_eq!(unsafe { crate::player::sink().load(c"{}".as_ptr(), first) }, 1);
        assert!(unsafe { crate::player::sink().callback_intercepts(&mt) } > 0);
        unsafe { crate::player::sink().unload(&plx_platform::tv::sink::UnloadTicket::issue(&mt)) };
        assert!(SHARED.native_unload_completed(first));
        assert_eq!(unsafe { crate::player::sink().callback_gate_retire(&mt) }, 1);
        assert!(SHARED.retire_native_session(first));
        assert_eq!(unsafe { crate::player::sink().destroy(&mt) }, 1);
        assert_eq!(unsafe { crate::player::sink().ready(&mt) }, 0);

        SHARED.reset_session();
        let unproven = SHARED
            .begin_native_session()
            .expect("unproven native epoch");
        assert_eq!(unsafe { crate::player::sink().load(c"{}".as_ptr(), unproven) }, 1);
        crate::player::ffi_host::force_callback_intercepts_for_test(0);
        unsafe { crate::player::sink().unload(&plx_platform::tv::sink::UnloadTicket::issue(&mt)) };
        assert_eq!(unsafe { crate::player::sink().callback_gate_retire(&mt) }, 0);
        assert!(SHARED.retire_native_session(unproven));
        unsafe { crate::player::sink().quarantine(&mt) };

        SHARED.reset_session();
        let refused = SHARED.begin_native_session().expect("refused native epoch");
        assert_eq!(
            unsafe { crate::player::sink().load(c"{}".as_ptr(), refused) },
            0,
            "quarantine must remain a process-long refusal",
        );
        assert!(SHARED.retire_native_session(refused));
    }

    /// S12a: the window breadcrumbs `release_native_object` leaves, as a host can see them. The
    /// capture sits behind the consent check, so the test grants the crash-report answer first and
    /// puts the previous one back.
    struct Consented(Option<plx_telemetry::telemetry::consent::Consent>);

    impl Consented {
        fn grant() -> Self {
            use plx_telemetry::telemetry::consent::{self, Consent};
            let previous = consent::current();
            let mut granted = Consent::default();
            granted.errors = true;
            granted.asked_version = consent::POLICY_VERSION;
            consent::install(granted);
            // Another test's lifecycle edges may have left a cycle open on this process-wide flag.
            plx_telemetry::telemetry::window::close_cycle_for_test();
            plx_telemetry::telemetry::window::take_recorded_for_test();
            Self(previous)
        }

        /// Consent granted with a background cycle open, as the teardown and create steps are
        /// recorded only inside one (`window::record_in_cycle`). The lifecycle edge's own crumb is
        /// drained, so a test sees only the steps it runs.
        fn grant_in_cycle() -> Self {
            let granted = Self::grant();
            plx_telemetry::telemetry::window::lifecycle(0x103, true);
            plx_telemetry::telemetry::window::take_recorded_for_test();
            granted
        }
    }

    impl Drop for Consented {
        fn drop(&mut self) {
            plx_telemetry::telemetry::consent::install(self.0.take().unwrap_or_default());
            plx_telemetry::telemetry::window::close_cycle_for_test();
            plx_telemetry::telemetry::window::take_recorded_for_test();
        }
    }

    fn loaded_epoch(mt: &MainThread) -> u32 {
        let epoch = SHARED.begin_native_session().expect("native epoch");
        assert_eq!(unsafe { crate::player::sink().load(c"{}".as_ptr(), epoch) }, 1);
        assert_eq!(unsafe { crate::player::sink().ready(mt) }, 1);
        epoch
    }

    #[test]
    fn a_proven_release_leaves_unload_destroyed_and_window_destroy_in_order() {
        let _serial = plx_base::testlock::serial();
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        SHARED.reset_session();
        let _fresh_process = FreshProcess;
        let consent = Consented::grant_in_cycle();
        let mt = unsafe { MainThread::assume() };
        let epoch = loaded_epoch(&mt);
        release_native_object(&mt, epoch);
        assert_eq!(
            plx_telemetry::telemetry::window::take_recorded_for_test(),
            ["native_unload", "native_destroyed", "exported_window_destroy"],
        );
        drop(consent);
    }

    #[test]
    fn an_unproven_release_records_quarantine_not_destroyed() {
        let _serial = plx_base::testlock::serial();
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        SHARED.reset_session();
        let _fresh_process = FreshProcess;
        let consent = Consented::grant_in_cycle();
        let mt = unsafe { MainThread::assume() };
        let epoch = loaded_epoch(&mt);
        // No callback ever crossed the gate: the evidence is incomplete and the object is kept.
        crate::player::ffi_host::force_callback_intercepts_for_test(0);
        release_native_object(&mt, epoch);
        assert_eq!(
            plx_telemetry::telemetry::window::take_recorded_for_test(),
            ["native_unload", "native_quarantined", "exported_window_destroy"],
        );
        drop(consent);
    }

    #[test]
    fn a_seam_rejected_destroy_is_recorded_as_quarantine() {
        let _serial = plx_base::testlock::serial();
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        SHARED.reset_session();
        let _fresh_process = FreshProcess;
        let consent = Consented::grant_in_cycle();
        let mt = unsafe { MainThread::assume() };
        let epoch = loaded_epoch(&mt);
        // Rust's three proofs hold, but the C seam's own evidence does not: `destroy` answers 0,
        // and the crumb must say so rather than "destroyed".
        crate::player::ffi_host::force_destroy_refusal_for_test(true);
        release_native_object(&mt, epoch);
        let recorded = plx_telemetry::telemetry::window::take_recorded_for_test();
        assert!(recorded.contains(&"native_quarantined"), "{recorded:?}");
        assert!(!recorded.contains(&"native_destroyed"), "{recorded:?}");
        drop(consent);
    }

    #[test]
    fn the_acb_plane_unload_is_a_step_only_when_acb_is_the_plane() {
        let _serial = plx_base::testlock::serial();
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        SHARED.reset_session();
        let _fresh_process = FreshProcess;
        let consent = Consented::grant_in_cycle();
        let mt = unsafe { MainThread::assume() };
        let epoch = loaded_epoch(&mt);
        ACB_OK.store(true, Ordering::Relaxed);
        release_native_object(&mt, epoch);
        ACB_OK.store(false, Ordering::Relaxed);
        let recorded = plx_telemetry::telemetry::window::take_recorded_for_test();
        let at = |label| recorded.iter().position(|l| *l == label);
        assert!(
            at("native_unload") < at("acb_plane_unload") && at("acb_plane_unload") < at("native_destroyed"),
            "{recorded:?}",
        );
        drop(consent);
    }

    #[test]
    fn without_a_dispatchable_object_or_an_exported_window_nothing_is_recorded() {
        let _serial = plx_base::testlock::serial();
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(false);
        SHARED.reset_session();
        let _fresh_process = FreshProcess;
        let consent = Consented::grant_in_cycle();
        let mt = unsafe { MainThread::assume() };
        let epoch = SHARED.begin_native_session().expect("native epoch");
        release_native_object(&mt, epoch);
        assert!(
            plx_telemetry::telemetry::window::take_recorded_for_test().is_empty(),
            "the seam's destroy_window is a no-op outside VP_EXPORTED, so it is not a step",
        );
        drop(consent);
    }

    #[test]
    fn creating_the_exported_window_is_a_step_only_in_that_mode() {
        let _serial = plx_base::testlock::serial();
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        SHARED.reset_session();
        let _fresh_process = FreshProcess;
        let consent = Consented::grant_in_cycle();
        let mt = unsafe { MainThread::assume() };
        crate::player::ffi_host::force_clocksink_for_test(false);
        let _ = with_window_id(&mt, "{}");
        assert!(plx_telemetry::telemetry::window::take_recorded_for_test().is_empty());
        crate::player::ffi_host::force_clocksink_for_test(true);
        let _ = with_window_id(&mt, "{}");
        assert_eq!(
            plx_telemetry::telemetry::window::take_recorded_for_test(),
            ["exported_window_create"],
        );
        drop(consent);
    }

    /// One engine's native life through the real create and release paths: what a trailer's start
    /// and stop, a seek or quality reload's teardown and an abandoned Load's release all run.
    fn engine_comes_and_goes(mt: &MainThread) {
        let _ = with_window_id(mt, "{}");
        let epoch = loaded_epoch(mt);
        release_native_object(mt, epoch);
    }

    /// S12a: **the 16-slot ring belongs to the lifecycle cycle.** The webOS 5 exported-window path
    /// costs a trailer four steps, so browsing four trailers used to push `will_foreground`,
    /// `wm_*` and `first_frame` out of the ring before the next background cycle. Teardown and
    /// window steps are crumbs of a lifecycle cycle only: this drives the REAL create and release
    /// paths through the longest cycle (webOS 5, playing, foreground reload) with trailer churn
    /// before, between and after, and checks the cycle's own steps are all the ring ever holds.
    #[test]
    fn trailer_starts_and_reloads_add_no_window_crumbs_and_a_whole_cycle_fits_the_ring() {
        use plx_telemetry::telemetry::window::{self, Observation, Stage};
        let _serial = plx_base::testlock::serial();
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        SHARED.reset_session();
        let _fresh_process = FreshProcess;
        let consent = Consented::grant();
        let mt = unsafe { MainThread::assume() };
        let step = |stage| window::record(Observation::step(stage, Some(true)));
        let mut ring = Vec::new();

        for _ in 0..6 {
            engine_comes_and_goes(&mt);
        }
        assert_eq!(
            window::take_recorded_for_test(),
            Vec::<&str>::new(),
            "browsing trailers and reloading engines is not a lifecycle cycle",
        );

        // Background, with playback: the engine is released inside the cycle.
        window::lifecycle(0x103, true);
        step(Stage::WaylandReleased);
        window::lifecycle(0x104, true);
        let epoch = loaded_epoch(&mt);
        release_native_object(&mt, epoch);
        step(Stage::BackgroundDone);
        ring.extend(window::take_recorded_for_test());

        // Foreground: the window is grabbed, then the restoring Load creates its exported window.
        window::lifecycle(0x105, true);
        window::lifecycle(0x106, true);
        step(Stage::WmQuery);
        step(Stage::WmReady);
        let _ = with_window_id(&mt, "{}");
        step(Stage::ForegroundDone);
        ring.extend(window::take_recorded_for_test());

        // The app is up again: trailers come and go before and after the first resumed frame.
        engine_comes_and_goes(&mt);
        step(Stage::FirstFrame);
        step(Stage::FirstSwapComplete);
        engine_comes_and_goes(&mt);
        ring.extend(window::take_recorded_for_test());

        assert_eq!(
            ring,
            [
                "will_background", "wayland_released", "did_background", "native_unload",
                "native_destroyed", "exported_window_destroy", "background_done",
                "will_foreground", "did_foreground", "wm_query", "wm_ready",
                "exported_window_create", "foreground_done", "first_frame", "first_swap_complete",
            ],
        );
        assert!(ring.len() <= window::LIMIT, "{} steps would evict the start of the cycle", ring.len());
        drop(consent);
    }
}

#[cfg(test)]
mod payload_tests {
    use super::{build_av_payload, with_dolby_hdr_info, with_immersive, SinkEnvelope, PAYLOAD_AV, PAYLOAD_H265, PAYLOAD_V};
    use plx_data::metadata::Dovi;

    fn p5() -> Dovi {
        Dovi {
            present: true,
            profile: 5,
            bl_compat: 0,
            el_present: false,
            ..Dovi::NONE
        }
    }

    fn p8() -> Dovi {
        Dovi {
            present: true,
            profile: 8,
            bl_compat: 1,
            el_present: false,
            ..Dovi::NONE
        }
    }

    #[test]
    fn dv_payload_matches_frozen_route_decision() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        assert!(crate::route::set_stream_declaration_for_test(
            &mut ps,
            "hevc",
            "eac3",
            23.976,
            p8(),
            false,
            plx_platform::devcaps::dv::DvCapability::Supported,
        ));
        let payload = build_av_payload(
            &ps,
            "H265",
            "AC3 PLUS",
            SinkEnvelope { w: 3840, h: 2160, fps: 60 },
        );
        assert!(payload.contains("DolbyHdrInfo"), "{payload}");
        assert!(payload.contains(r#""profileId":8"#), "{payload}");

        assert!(crate::route::set_stream_declaration_for_test(
            &mut ps,
            "hevc",
            "eac3",
            23.976,
            p8(),
            false,
            plx_platform::devcaps::dv::DvCapability::Unsupported,
        ));
        let payload = build_av_payload(
            &ps,
            "H265",
            "AC3 PLUS",
            SinkEnvelope { w: 3840, h: 2160, fps: 60 },
        );
        assert!(!payload.contains("DolbyHdrInfo"), "{payload}");
    }

    /// An AV1 Profile 10.1 direct play (HDR10-compatible base) on a set with confirmed Dolby Vision
    /// support carries the node like an HEVC Profile 8 does, with the codec string left at `"AV1"`.
    /// VP9 never does: no Dolby Vision profile rides on it.
    #[test]
    fn an_av1_profile_10_direct_play_declares_dolby_vision() {
        let p10 = Dovi {
            present: true,
            profile: 10,
            bl_compat: 1,
            el_present: false,
            ..Dovi::NONE
        };
        let mut ps = crate::route::PlaybackSession::IDLE;
        assert!(crate::route::set_stream_declaration_for_test(
            &mut ps,
            "av1",
            "eac3",
            23.976,
            p10,
            false,
            plx_platform::devcaps::dv::DvCapability::Supported,
        ));
        assert_eq!(crate::route::stream_dovi(&ps).profile, 10);
        let payload = build_av_payload(
            &ps,
            "AV1",
            "AC3 PLUS",
            SinkEnvelope { w: 3840, h: 2160, fps: 60 },
        );
        assert!(payload.contains(r#""video":"AV1""#), "{payload}");
        assert!(
            payload.contains(r#""DolbyHdrInfo":{"trackType":"single","encryptionType":"clear","profileId":10}"#),
            "{payload}"
        );

        assert!(crate::route::set_stream_declaration_for_test(
            &mut ps,
            "vp9",
            "eac3",
            23.976,
            p10,
            false,
            plx_platform::devcaps::dv::DvCapability::Supported,
        ));
        assert!(crate::route::stream_dv_presentation(&ps).declared().is_none());
        let payload = build_av_payload(
            &ps,
            "VP9",
            "AC3 PLUS",
            SinkEnvelope { w: 3840, h: 2160, fps: 60 },
        );
        assert!(!payload.contains("DolbyHdrInfo"), "{payload}");
    }

    #[test]
    fn late_capability_result_does_not_change_installed_decision() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        assert!(crate::route::set_stream_declaration_for_test(
            &mut ps,
            "hevc",
            "eac3",
            23.976,
            p8(),
            false,
            plx_platform::devcaps::dv::DvCapability::Unknown,
        ));
        let fresh = p8().presentation(
            true,
            plx_platform::devcaps::dv::DvCapability::Supported,
            true,
        );
        assert!(fresh.declared().is_some(), "a subsequent decision sees Supported");
        assert_eq!(
            crate::route::stream_dv_presentation(&ps),
            plx_data::metadata::DvPresentation::NotDv,
        );
        let payload = build_av_payload(
            &ps,
            "H265",
            "AC3 PLUS",
            SinkEnvelope { w: 3840, h: 2160, fps: 60 },
        );
        assert!(!payload.contains("DolbyHdrInfo"), "{payload}");
    }

    #[test]
    fn refused_synthetic_p5_start_cannot_be_retried_past_validation() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let play = crate::player::playurl::PlayUrl {
            url: "http://192.0.2.1/refused-p5.mkv".into(),
            vcodec: "hevc".into(),
            acodec: "eac3".into(),
            fps: 23.976,
            dovi: crate::player::playurl::PlayDovi {
                profile: 5,
                bl_compat: 0,
                el_present: false,
            },
            atmos: false,
            auto_source_kbps: 0,
            auto_hls_base: String::new(),
            auto_start_hls: false,
            source_raster: None,
        };

        for retry in 0..2 {
            assert_eq!(
                super::install_synthetic_playurl(&mut ps, &play),
                Err(crate::route::RouteStartResult::StartFailed),
                "retry {retry} must stop before Load",
            );
            assert!(
                !crate::route::has_url(&ps),
                "retry {retry} left the rejected URL installed",
            );
        }
    }

    /// **Every Load payload must carry the `appId` placeholder, exactly once**, because that key
    /// is what `with_window_id` splices the webOS 5+ `option.windowId` onto — without it the
    /// decoded video has nowhere to bind on every set from 5.0 up — and because a second one would
    /// splice a second `windowId`.
    ///
    /// This replaces a panel row that was proposed and rejected: the "not spliced — no anchor" arm
    /// is UNREACHABLE at runtime (the placeholder is in all three constants and `build_av_payload`
    /// never touches it), so a row reporting it would print the same constant on a working and a
    /// broken television. The real risk is a payload edited a year from now that drops it — a
    /// build-time risk, caught at build time. The placeholder is a strictly better witness than the
    /// literal id it replaced: an id can be spelled correctly by accident, `@APPID@` cannot.
    #[test]
    fn every_load_payload_carries_the_app_id_placeholder_exactly_once() {
        for (name, p) in [
            ("PAYLOAD_V", PAYLOAD_V),
            ("PAYLOAD_AV", PAYLOAD_AV),
            ("PAYLOAD_H265", PAYLOAD_H265),
        ] {
            assert_eq!(
                p.matches(r#""appId":"@APPID@""#).count(),
                1,
                "{name} must carry the appId placeholder exactly once — webOS 5+ video binds on it"
            );
        }
    }

    /// **The shipped app's payload must be byte-identical to what every release so far sent**, and
    /// the splice anchor must be the composed key rather than a second spelling of it.
    ///
    /// This is the whole safety argument for making the id dynamic. The webOS 5+ `windowId` path
    /// cannot be exercised on this project's 4.5 dev set (`vp_mode()` returns `VP_ACB` there and
    /// `with_window_id` returns early), so its failure — a black video plane with working audio,
    /// and no error line — would not be seen until somebody else's television. Pinning the
    /// composed bytes here is the only gate available for it.
    #[test]
    fn the_shipped_app_composes_the_payload_it_always_did() {
        let want = r#""appId":"com.beb.plxnative""#;
        for (name, p) in [
            ("PAYLOAD_V", PAYLOAD_V),
            ("PAYLOAD_AV", PAYLOAD_AV),
            ("PAYLOAD_H265", PAYLOAD_H265),
        ] {
            let composed = p.replace("@APPID@", plx_base::paths::STABLE_APP_ID);
            assert!(
                composed.contains(want),
                "{name} no longer composes the shipped key"
            );
            // key ORDER too: `appId` stays the first key of `option`, where it has always been.
            assert!(
                composed.contains(&format!(r#""option":{{{want},"#)),
                "{name} moved the appId key"
            );
        }
        // And the anchor the splice looks for is the composed key, not a restatement of it.
        assert_eq!(
            super::app_id_key(),
            format!(r#""appId":"{}""#, plx_base::paths::app_id())
        );
        assert!(super::with_app_id(PAYLOAD_V).contains(&super::app_id_key()));
        assert!(!super::with_app_id(PAYLOAD_V).contains("@APPID@"));
    }

    /// The Dolby Vision node's anchor, with the same reasoning as the one above: `provider` is the
    /// LAST key of `contents`, which is where `DolbyHdrInfo` has to land — the pipeline reads it at
    /// `option.externalStreamingInfo.contents.DolbyHdrInfo` and nowhere else. Exactly once, or a
    /// `replace` would splice two nodes into one payload.
    #[test]
    fn the_av_payload_carries_exactly_one_dolby_hdr_info_anchor() {
        assert_eq!(PAYLOAD_AV.matches(r#""provider":"plxnative""#).count(), 1);
        // and it really is the last key of `contents` — the next character after it closes the
        // object, so appending a key there stays INSIDE `contents`
        assert!(PAYLOAD_AV.contains(r#""provider":"plxnative"},"streamQualityInfo""#));
    }

    /// **What we actually send for a Profile 5 direct play.** The three fields at the path the
    /// television's own parser reads, `profileId` as a bare JSON integer (`getInt` — a quoted "5"
    /// would leave the pipeline's -1 sentinel), and the whole node inside `contents`.
    #[test]
    fn a_declared_profile_5_splices_the_node_into_contents() {
        // what `build_av_payload` hands it: the AV template with the codec already set to H265,
        // which is what a native HEVC direct play — the only kind that can be Dolby Vision — sends
        let base = PAYLOAD_AV.replace(r#""video":"H264""#, r#""video":"H265""#);
        let out = with_dolby_hdr_info(&base, "H265", p5().presentation(true, plx_platform::devcaps::dv::DvCapability::Supported, true));
        assert!(
            out.contains(r#""provider":"plxnative","DolbyHdrInfo":{"trackType":"single","encryptionType":"clear","profileId":5}}"#),
            "{out}"
        );
        // the trailing `}` above is `contents` closing: the node is the last key INSIDE it, not a
        // sibling of `contents` in `externalStreamingInfo`
        assert!(out.contains(r#""DolbyHdrInfo":{"trackType":"single","encryptionType":"clear","profileId":5}},"streamQualityInfo""#));
        assert!(
            !out.contains(r#""profileId":"5""#),
            "getInt wants an integer, not a string"
        );
        // the codec string stays `H265` — `getVideoCaps` maps it to `video/x-h265` and falls
        // THROUGH into its Dolby Vision tail; there is no DVHE/DVH1 entry in that table, and
        // inventing one would describe a stream the pipeline has no decoder row for
        assert!(out.contains(r#""video":"H265""#), "{out}");
        assert!(!out.contains("DVHE") && !out.contains("dvh1"), "{out}");
        // and NOTHING else moved: take the node back out and the payload is what came in
        const NODE: &str =
            r#","DolbyHdrInfo":{"trackType":"single","encryptionType":"clear","profileId":5}"#;
        assert_eq!(out.replace(NODE, ""), base);
    }

    /// The three ways the node is NOT sent, each of which must leave the payload byte-identical:
    /// a file with no Dolby Vision, a Dolby Vision file we refuse (a dual layer without a
    /// cross-compatible base layer — declaring a layer we cannot feed is worse than refusing it),
    /// and the disarmed trigger, which is what a `RELEASE=1` build compiles in and what every boot
    /// without `/tmp/plxnative-dv` does today.
    #[test]
    fn nothing_is_spliced_unless_the_stream_is_declared() {
        let unconvertible_p7 = Dovi {
            present: true,
            profile: 7,
            bl_compat: 0,
            el_present: true,
            ..Dovi::NONE
        };
        for dv in [
            Dovi::NONE.presentation(true, plx_platform::devcaps::dv::DvCapability::Supported, true),
            unconvertible_p7.presentation(true, plx_platform::devcaps::dv::DvCapability::Supported, true),
            p5().presentation(false, plx_platform::devcaps::dv::DvCapability::Supported, true),
        ] {
            assert_eq!(with_dolby_hdr_info(PAYLOAD_AV, "H265", dv), PAYLOAD_AV);
        }
    }

    /// A Profile 7 with a cross-compatible base layer is fed as the 8.1 `ff::packet_to_annexb`
    /// makes of it, and the node says so: `profileId` 8, one track — never 7, never `"dual"`.
    #[test]
    fn a_converted_profile_7_declares_profile_8_single() {
        let p7 = Dovi {
            present: true,
            profile: 7,
            bl_compat: 6,
            el_present: true,
            ..Dovi::NONE
        };
        let out = with_dolby_hdr_info(
            PAYLOAD_AV,
            "H265",
            p7.presentation(true, plx_platform::devcaps::dv::DvCapability::Supported, true),
        );
        const NODE: &str =
            r#","DolbyHdrInfo":{"trackType":"single","encryptionType":"clear","profileId":8}"#;
        assert!(out.contains(NODE), "{out}");
        assert_eq!(out.replace(NODE, ""), PAYLOAD_AV);
    }

    /// **What we actually send for a Dolby Atmos track**, at the key path `libpf` reads
    /// (`option.externalStreamingInfo.contents.immersive`) and with the value `libcbe` — the
    /// television's own working client — puts there. Same anchor and same shape as the Dolby
    /// Vision node, which is the point: they are one envelope with two statements in it.
    #[test]
    fn an_atmos_track_splices_immersive_into_contents() {
        let out = with_immersive(PAYLOAD_AV, "AC3 PLUS", true);
        assert!(
            out.contains(r#""provider":"plxnative","immersive":"ATMOS"}"#),
            "{out}"
        );
        // the trailing `}` is `contents` closing — the node is INSIDE it, not a sibling of
        // `contents` in `externalStreamingInfo`, which is where libpf would never look
        assert!(
            out.contains(r#""immersive":"ATMOS"},"streamQualityInfo""#),
            "{out}"
        );
        // and nothing else moved
        assert_eq!(out.replace(r#","immersive":"ATMOS""#, ""), PAYLOAD_AV);
    }

    /// The other side of it, and the one that matters for the whole library: a track with no
    /// Atmos leaves the payload byte-identical. Every ordinary AAC/AC3 film takes this path, so a
    /// splice that fired unconditionally would tell the television that all of them are immersive.
    #[test]
    fn a_plain_track_splices_nothing() {
        assert_eq!(with_immersive(PAYLOAD_AV, "AC3 PLUS", false), PAYLOAD_AV);
    }

    /// A source Atmos flag that accidentally survives a full transcode must still be rejected at
    /// the final payload seam.  HLS carries AAC and ordinary AC3 carries no E-AC3 JOC extension;
    /// telling the system player either is immersive is a false stream declaration.
    #[test]
    fn an_atmos_declaration_never_rides_a_non_eac3_payload() {
        for audio in ["AAC", "AC3"] {
            assert_eq!(
                with_immersive(PAYLOAD_AV, audio, true),
                PAYLOAD_AV,
                "{audio}"
            );
        }
    }

    /// **Both nodes at once**, which is the real case — the Profile 5 test item is Dolby Vision
    /// AND Dolby Atmos, and it is what the television shows two read-outs for. They are spliced by
    /// two independent functions at ONE anchor, so this is the test that says the second does not
    /// land inside the first: `immersive` must be a sibling KEY of `DolbyHdrInfo` inside
    /// `contents`, never a fourth field of the `DolbyHdrInfo` object.
    #[test]
    fn dolby_vision_and_atmos_are_siblings_inside_contents() {
        let base = PAYLOAD_AV.replace(r#""video":"H264""#, r#""video":"H265""#);
        let out = with_immersive(
            &with_dolby_hdr_info(&base, "H265", p5().presentation(true, plx_platform::devcaps::dv::DvCapability::Supported, true)),
            "AC3 PLUS",
            true,
        );
        assert!(
            out.contains(
                r#""provider":"plxnative","immersive":"ATMOS","DolbyHdrInfo":{"trackType":"single","encryptionType":"clear","profileId":5}}"#
            ),
            "{out}"
        );
        // the DolbyHdrInfo object still has exactly its three fields — `immersive` did not get
        // swept inside it by an anchor both functions matched
        assert!(!out.contains(r#""profileId":5,"immersive""#), "{out}");
        assert_eq!(out.matches(r#""immersive":"ATMOS""#).count(), 1);
        assert_eq!(out.matches("DolbyHdrInfo").count(), 1);
    }

    /// The consistency guard: a Dolby Vision declaration only ever rides an HEVC elementary
    /// stream, so a payload built for H264 must not carry one. Unreachable by construction — the
    /// route records the DV record on the direct-play branch, whose codec is the file's own hevc —
    /// which is exactly why it is asserted rather than trusted: the `sourceInfo` envelope is
    /// parsed before anything decodes, and a malformed one wedges the sink instead of failing.
    #[test]
    fn a_declaration_never_rides_a_non_hevc_payload() {
        assert_eq!(
            with_dolby_hdr_info(PAYLOAD_AV, "H264", p5().presentation(true, plx_platform::devcaps::dv::DvCapability::Supported, true)),
            PAYLOAD_AV
        );
    }
}

/// A cropped diagnostic line cannot report what it never wrote down. `op_audio_enhancement_burn`
/// in `tests/run.py` proves a Burn actually happened by grepping the app's own
/// `stream: <origin> path=...` line for `subtitleStreamID=` and `subtitles=burn` — the only
/// on-device evidence of the query PMS actually received. Device runs of
/// `audio_enhancement_burns_embedded_subtitle` (rk 1804, 2026-09) show the grader failing even
/// though [`crate::route::decision`]'s own tests prove the Burn route is chosen correctly: the
/// line was cut to 80 characters, which lands inside `mediaIndex=`/`partIndex=`/`protocol=...`
/// for a realistic transcode URL, well before either marker.
#[cfg(test)]
mod stream_open_log_line_tests {
    use super::stream_open_log_line;

    /// A transcode URL shaped like the one PMS returns for this project's Burn route: video
    /// re-encode params, then `audioStreamID`, then `boostDialog`/`normalizeLoudness`, then
    /// `subtitleStreamID`/`subtitles=burn` last — matching `transcoder::transcode_query`'s actual
    /// build order — followed by the `X-Plex-Token` `Client::with_token` always appends last.
    fn burn_path() -> String {
        "/video/:/transcode/universal/start.mkv?path=%2Flibrary%2Fmetadata%2F1804&mediaIndex=0&\
         partIndex=0&protocol=http&directPlay=0&directStream=0&videoResolution=3840x2160&\
         maxVideoBitrate=40000&videoCodec=hevc&audioCodec=eac3&audioStreamID=10976&\
         boostDialog=0&normalizeLoudness=1&subtitleStreamID=10980&subtitleSize=100&\
         subtitles=burn"
            .to_string()
    }

    #[test]
    fn logged_line_carries_the_burn_evidence_the_grader_looks_for() {
        let path = burn_path();
        let line = stream_open_log_line("plex.direct:32400", &path);
        assert!(
            line.contains("subtitleStreamID=10980"),
            "log line dropped subtitleStreamID before the grader could see it: {line}"
        );
        assert!(
            line.contains("subtitles=burn"),
            "log line dropped subtitles=burn before the grader could see it: {line}"
        );
    }

    /// The un-truncated path still passes through `plx_base::eventlog::log`'s `scrub_local` pass, which is
    /// the ONLY place a token may be redacted — so leaving the full path in is safe precisely
    /// because `Client::with_token` always appends `X-Plex-Token=` last and `scrub_local` already
    /// redacts it by value. This is the end-to-end check that removing the truncation did not
    /// also remove the credential backstop.
    #[test]
    fn full_path_still_has_its_token_redacted_by_the_existing_scrub_pass() {
        let path = format!("{}&X-Plex-Token=aBcD1234xyzQ", burn_path());
        let line = stream_open_log_line("plex.direct:32400", &path);
        let scrubbed = plx_base::eventlog::scrub::scrub_local(&line);
        assert!(
            !scrubbed.contains("aBcD1234xyzQ"),
            "token leaked into the event log: {scrubbed}"
        );
        assert!(
            scrubbed.contains("subtitleStreamID=10980") && scrubbed.contains("subtitles=burn"),
            "scrub pass ate the burn evidence along with the token: {scrubbed}"
        );
    }
}

/// **The 94-second freeze of 2026-08-29, reduced to the mechanism that caused it.**
///
/// A viewer on a remote 4K Dolby Vision stream was downshifted into HLS, and the picture then did
/// not start for 94 seconds. The event log shows the reason plainly once you know where to look:
/// `primed: v=1999ms a=1962ms -> Play` was eventually printed with EXACTLY the fed extents of the
/// segment that had landed 94 seconds earlier — so the condition Play waits on had been satisfied
/// the whole time and was simply never EVALUATED.
///
/// The evaluation lived in one place only: inside [`feed_stream`]'s per-AU loop, after each
/// `sf_feed` — accepted or not, which was a second bug in the same lines. A pump tick runs the video lane and then the audio lane
/// ([`super::pump::pump`]), so when a whole HLS segment lands at once the video lane drains all of
/// it with `abuf` still at zero — `vbuf` tops out below [`PRIME_VIDEO_MAX_NS`] and the
/// `abuf >= PRIME_AUDIO_NS` arm cannot be true yet. The audio lane then fills, but it deliberately
/// carries no prime check. On every later tick `aq_pop` returns null and the loop breaks BEFORE
/// the check is reached. Play is now waiting for a video AU that only the next segment can supply.
///
/// That is why direct play is unaffected and HLS is not: direct play trickles AUs in over many
/// ticks, so audio keeps pace and the check passes on an ordinary tick. A segment arriving whole
/// is the shape that breaks it — and since [`PRIME_VIDEO_MAX_NS`] is 2500 ms, a 2-second segment
/// can never take the video-only escape, which makes this structural for every 2 s-segment reload
/// rather than a rare race.
///
/// **This test drives the real [`feed_stream`] and [`feed_audio_lane`] against the real host feed
/// seam** — it is not a restatement of the predicate, which is correct and would pass on the
/// broken build. What it grades is WHERE the predicate is evaluated.
#[cfg(all(test, feature = "hostsim"))]
mod prime_livelock_tests {
    use super::*;

    /// One 2-second HLS segment, as the device delivered it: 48 video AUs at 24 fps and 93 AAC
    /// frames. These are the counts the failing run's `hls: segment=458 … v=48 a=93` line records.
    const VIDEO_AUS: i64 = 48;
    const AUDIO_AUS: i64 = 93;
    const V_STEP_NS: i64 = 41_666_666; // 24 fps
    const A_STEP_NS: i64 = 21_333_333; // 1024 samples at 48 kHz

    /// An engine in the state a fresh `Load` leaves behind: streaming, nothing fed yet, and
    /// `prime_play` armed so the next sufficient buffer starts the clock.
    pub(super) fn engine_after_reload() -> Engine {
        SHARED.ensure_initial_clock_hold_for_test();
        Engine {
            route_start: crate::route::RouteStartAttempt::fixture(),
            native_epoch: 0,
            stage: Stage::Streaming,
            video_info_sent: true,
            bound_at_frames: None,
            placed_src: (0, 0),
            eos_pushed: false,
            rebase_pending: false,
            rebase_drops: 0,
            seek_armed_at: 0,
            seek_retries: 0,
                flushed: false,
            presentation_rearm_pending: false,
            max_fed_video_pts: 0,
            max_fed_audio_pts: 0,
            audio_start_fp: None,
            pcm_sink_lead_ns: 0,
            pcm_lead_pending: false,
            feed_pace: FeedPace::default(),
            seek_base_pts: 0,
            prime_play: true,
            aq_video: Some(crate::aq::aq_new(AQ_VIDEO_BYTES)),
            aq_audio: Some(crate::aq::aq_new(AQ_AUDIO_BYTES)),
            hs: plx_net::stream::http_stream_boxed(),
            pending_video: None,
            pending_audio: None,
            payload: std::ffi::CString::new("").unwrap(),
            source: Source::Stream,
            stream_th: None,
            load_th: None,
            report_th: None,
            report_stop: None,
            preview_abandon: false,
            foreground_follows_load: false,
            era_watch: crate::player::era_callbacks::Watch::new(),
        }
    }

    /// Push one whole segment into both lanes before any tick runs — which is what a segmented
    /// demux does, and the entire difference from direct play.
    fn push_one_segment(eng: &mut Engine) {
        push_segment_at(eng, 0);
    }

    fn push_segment_at(eng: &mut Engine, base_ns: i64) {
        let au = [0u8; 64];
        let qv = &mut **eng.aq_video.as_mut().unwrap() as *mut crate::aq::AuQueue;
        for i in 0..VIDEO_AUS {
            crate::aq::aq_push(
                qv,
                au.as_ptr(),
                au.len() as c_int,
                base_ns + i * V_STEP_NS,
                c_int::from(i == 0),
                1,
            );
        }
        let qa = &mut **eng.aq_audio.as_mut().unwrap() as *mut crate::aq::AuQueue;
        for i in 0..AUDIO_AUS {
            crate::aq::aq_push(
                qa,
                au.as_ptr(),
                au.len() as c_int,
                base_ns + i * A_STEP_NS,
                1,
                2,
            );
        }
    }

    /// One pump tick — the real one. [`super::pump::pump`] calls exactly this.
    ///
    /// **This comment used to claim the test therefore "cannot drift from the shipped ordering",
    /// and that was false**: the test calls the wrapper, so a `pump` reverted to two separate lane
    /// calls would have restored the bug with this test still green. What actually enforces it is
    /// that [`feed_stream`] and [`feed_audio_lane`] are private — the revert no longer compiles.
    /// The test pins the BEHAVIOUR; the visibility pins the dispatch.
    fn tick(mt: &plx_base::task::MainThread, eng: &mut Engine) {
        feed_both_lanes(mt, eng);
    }

    #[test]
    fn a_segment_that_lands_whole_still_starts_the_picture() {
        let _serial = plx_base::testlock::serial();
        // Arm the host clock sink so `sf_feed` accepts AUs. See `ffi_host::FORCE_ENABLED` for why
        // this is an explicit override and not the `plxnative-clocksink` trigger file. RESTORED on
        // the way out, panic or not: it is a process-global, and leaving it armed would silently
        // change what every later test's `sf_feed` returns.
        struct DisarmSink;
        impl Drop for DisarmSink {
            fn drop(&mut self) {
                crate::player::ffi_host::force_clocksink_for_test(false);
            }
        }
        crate::player::ffi_host::force_clocksink_for_test(true);
        let _disarm = DisarmSink;

        let mt = unsafe { plx_base::task::MainThread::assume() };
        SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        SHARED.seek_to_ns.store(-1, Ordering::Relaxed);

        let mut eng = engine_after_reload();
        push_one_segment(&mut eng);

        // Tick 1: the segment is there in full. Tick 2: the next segment has not arrived, so both
        // queues are empty — the state the device sat in for 94 seconds.
        tick(&mt, &mut eng);
        tick(&mt, &mut eng);

        let vbuf = eng.max_fed_video_pts - eng.seek_base_pts;
        let abuf = eng.max_fed_audio_pts - eng.seek_base_pts;

        // **Precondition, asserted rather than assumed.** If the host feed seam refused the AUs
        // then nothing was fed, and a `prime_play` that is still armed would mean "the sink was
        // not armed", not "the bug". Distinguishing those is the whole lesson of this project's
        // silent-instrument traps, so it gets its own message.
        assert!(
            vbuf > 0 && abuf > 0,
            "PRECONDITION FAILED, not the defect: the host feed seam accepted nothing \
             (v={vbuf}ns a={abuf}ns). The clock sink is not armed, so this test graded nothing."
        );
        assert!(
            vbuf >= PRIME_NS,
            "video lane should hold a whole segment, got {vbuf}ns"
        );
        assert!(
            abuf >= PRIME_AUDIO_NS,
            "audio lane should hold a whole segment, got {abuf}ns"
        );

        assert!(
            !eng.prime_play,
            "the picture never started: both lanes hold enough to play \
             (v={}ms >= {}ms, a={}ms >= {}ms) but Play was never called. \
             The prime check is only reachable from inside feed_stream's per-AU loop, and by the \
             time the audio lane has caught up the video queue is empty, so the check is never \
             evaluated again. This is the 94-second freeze.",
            vbuf / 1_000_000,
            PRIME_NS / 1_000_000,
            abuf / 1_000_000,
            PRIME_AUDIO_NS / 1_000_000,
        );
    }

    /// Arm the host clock sink and a per-read feed-clock cost; restored on drop (both are
    /// process-globals).
    struct SlowFeed;
    impl SlowFeed {
        fn arm(step_us: u64) -> Self {
            crate::player::ffi_host::force_clocksink_for_test(true);
            FEED_TEST_CLOCK_STEP_US.with(|s| s.set(step_us));
            SlowFeed
        }
    }
    impl Drop for SlowFeed {
        fn drop(&mut self) {
            FEED_TEST_CLOCK_STEP_US.with(|s| s.set(0));
            crate::player::ffi_host::force_clocksink_for_test(false);
        }
    }

    /// Ticks a freshly reloaded engine takes to start the clock on one whole segment.
    fn ticks_to_prime(step_us: u64) -> (u32, Engine) {
        let _slow = SlowFeed::arm(step_us);
        let mt = unsafe { plx_base::task::MainThread::assume() };
        SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        SHARED.seek_to_ns.store(-1, Ordering::Relaxed);
        let mut eng = engine_after_reload();
        push_one_segment(&mut eng);
        let mut ticks = 0;
        while eng.prime_play && ticks < 100 {
            tick(&mt, &mut eng);
            ticks += 1;
        }
        (ticks, eng)
    }

    /// **A slow `Feed()` does not stretch the prime.** With each feed-clock read costing 4 ms
    /// (past [`FEED_LANE_SLICE_US`]) a sliced lane feeds one AU a tick, and the prime's 17 video
    /// and ~15 audio AUs took that many ticks to first frame. A lane short of its prime depth is
    /// exempt, so the prime takes the same ticks as a greedy feed, and still on BOTH lanes.
    #[test]
    fn a_slow_feed_does_not_stretch_the_prime() {
        let _serial = plx_base::testlock::serial();
        let (greedy, _) = ticks_to_prime(0);
        let (slow, eng) = ticks_to_prime(4_000);
        assert!(!eng.prime_play, "the prime never completed in {slow} ticks");
        assert!(
            slow <= greedy + 1,
            "a 4 ms Feed() took {slow} ticks to prime; greedy feeding takes {greedy}"
        );
        assert!(
            eng.max_fed_video_pts - eng.seek_base_pts >= PRIME_NS
                && eng.max_fed_audio_pts - eng.seek_base_pts >= PRIME_AUDIO_NS,
            "Play started without both lanes primed"
        );
    }

    /// **Once a lane holds its prime depth, the rest of the backlog is fed a slice per lane per
    /// tick, not all in one frame.** On the TV every `Feed()` blocks the frame thread, and the
    /// first tick of a playback fed 62 video and 120 audio AUs at once (an 89 ms frame). Each
    /// feed-clock read costs 1 ms here, so a lane above low-water may feed only up to
    /// [`FEED_LANE_SLICE_US`] worth per tick: the first tick primes (exempt, to the prime depth)
    /// and the clock starts on both lanes, then the leftover backlog drains a slice a tick.
    #[test]
    fn a_post_prime_backlog_is_fed_a_slice_per_lane_per_tick() {
        let _serial = plx_base::testlock::serial();
        let _slow = SlowFeed::arm(1_000);
        let mt = unsafe { plx_base::task::MainThread::assume() };
        SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        SHARED.seek_to_ns.store(-1, Ordering::Relaxed);

        let mut eng = engine_after_reload();
        push_one_segment(&mut eng);
        let per_lane_max = (FEED_LANE_SLICE_US / 1_000) as i64 + 1;

        tick(&mt, &mut eng);
        assert!(!eng.prime_play, "the first tick primes: both lanes reach their depth");
        let vbuf = eng.max_fed_video_pts - eng.seek_base_pts;
        let abuf = eng.max_fed_audio_pts - eng.seek_base_pts;
        assert!(
            vbuf >= PRIME_NS && abuf >= PRIME_AUDIO_NS,
            "Play started without both lanes primed (v={}ms a={}ms)",
            vbuf / 1_000_000,
            abuf / 1_000_000,
        );
        let v_after_prime = (vbuf + V_STEP_NS - 1) / V_STEP_NS;
        assert!(
            v_after_prime <= PRIME_NS / V_STEP_NS + per_lane_max,
            "the video lane fed {v_after_prime} AUs: the slice must apply past the prime depth"
        );

        // The presented position sits at the base, so the lead (>= the prime depth) is healthy.
        SHARED.pres_fed.store(0, Ordering::Relaxed);
        let (v0, a0) = (eng.max_fed_video_pts, eng.max_fed_audio_pts);
        tick(&mt, &mut eng);
        let v_fed = (eng.max_fed_video_pts - v0) / V_STEP_NS;
        let a_fed = (eng.max_fed_audio_pts - a0) / A_STEP_NS;
        assert!(
            eng.max_fed_video_pts > v0 && eng.max_fed_audio_pts > a0,
            "both lanes must keep draining the backlog"
        );
        assert!(
            v_fed <= per_lane_max && a_fed <= per_lane_max,
            "one tick fed {v_fed} video and {a_fed} audio AUs; the slice allows {per_lane_max} per lane",
        );
        SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
    }

    /// **A playing lane below low-water is fed past its slice**: at 4 ms a `Feed()` the slice
    /// alone would feed one AU a tick, which at a 50 Hz present rate cannot keep 60 fps content's
    /// lead. Below [`FEED_LOW_WATER_NS`] the lane is topped up in one tick; above it, the slice.
    #[test]
    fn a_playing_lane_below_low_water_is_fed_past_its_slice() {
        let _serial = plx_base::testlock::serial();
        let _slow = SlowFeed::arm(4_000);
        let mt = unsafe { plx_base::task::MainThread::assume() };
        const PRES: i64 = 10_000_000_000;
        SHARED.pres_fed.store(PRES, Ordering::Relaxed);
        SHARED.seek_to_ns.store(-1, Ordering::Relaxed);

        let mut eng = engine_after_reload();
        eng.prime_play = false;
        eng.seek_base_pts = 0;
        let lead0 = 100_000_000;
        eng.max_fed_video_pts = PRES + lead0;
        eng.max_fed_audio_pts = PRES + lead0;
        // This fixture holds the presented position fixed. Keep FeedPace's existing anchor
        // ahead of the real clock so host preemption cannot extrapolate that position while
        // the simulated feed clock is testing the slice. FeedPace's run-on has its own tests.
        eng.feed_pace.last = Some((PRES, u32::MAX));
        push_segment_at(&mut eng, PRES + lead0 + 1);

        tick(&mt, &mut eng);
        let v_fed = (eng.max_fed_video_pts - (PRES + lead0)) / V_STEP_NS;
        let want = (FEED_LOW_WATER_NS - lead0 + V_STEP_NS - 1) / V_STEP_NS;
        assert_eq!(v_fed, want, "the video lane is topped up to low-water in one tick, not one AU");
        assert!(eng.max_fed_video_pts - PRES >= FEED_LOW_WATER_NS);

        // Above low-water the slice is back in force: one AU a tick at 4 ms a Feed().
        let v0 = eng.max_fed_video_pts;
        tick(&mt, &mut eng);
        assert_eq!((eng.max_fed_video_pts - v0) / V_STEP_NS, 1, "a healthy lane is sliced");
        SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
    }

    /// Starting with less playable media than the next observed acquisition costs recreates the
    /// freeze/catch-up loop: the clock empties the segment before its replacement can be credited.
    /// The demuxer's runway is a measured duration, so it must raise the decoder-only prime floor.
    #[test]
    fn an_hls_clock_waits_for_the_observed_acquisition_runway() {
        let _serial = plx_base::testlock::serial();
        struct Restore {
            runway: i64,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .hls_prime_runway_ms
                    .store(self.runway, Ordering::Relaxed);
                crate::player::ffi_host::force_clocksink_for_test(false);
            }
        }
        crate::player::ffi_host::force_clocksink_for_test(true);
        let old = SHARED.hls_prime_runway_ms.swap(3_000, Ordering::Relaxed);
        let _restore = Restore { runway: old };
        SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        SHARED.seek_to_ns.store(-1, Ordering::Relaxed);

        let mt = unsafe { plx_base::task::MainThread::assume() };
        let mut eng = engine_after_reload();
        push_one_segment(&mut eng);
        tick(&mt, &mut eng);

        let vbuf = eng.max_fed_video_pts - eng.seek_base_pts;
        let abuf = eng.max_fed_audio_pts - eng.seek_base_pts;
        assert!(
            vbuf > PRIME_NS && abuf > PRIME_AUDIO_NS,
            "the decoder prime is satisfied"
        );
        assert!(
            vbuf < 3_000_000_000 && abuf < 3_000_000_000,
            "the measured runway is not"
        );
        assert!(
            eng.prime_play,
            "Play started on decoder latency alone even though the next acquisition costs 3000ms",
        );
    }

    #[test]
    fn automatic_rebuffer_cannot_resume_without_fresh_media() {
        let _serial = plx_base::testlock::serial();
        struct Restore {
            runway: i64,
            rebuffering: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .hls_prime_runway_ms
                    .store(self.runway, Ordering::Relaxed);
                SHARED
                    .hls_rebuffering
                    .store(self.rebuffering, Ordering::Relaxed);
                SHARED.finish_hls_recovery();
                crate::player::ffi_host::force_clocksink_for_test(false);
            }
        }
        crate::player::ffi_host::force_clocksink_for_test(true);
        let restore = Restore {
            runway: SHARED.hls_prime_runway_ms.swap(500, Ordering::Relaxed),
            rebuffering: SHARED.hls_rebuffering.swap(false, Ordering::Relaxed),
        };
        let _restore = restore;
        SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        SHARED.seek_to_ns.store(-1, Ordering::Relaxed);

        let mt = unsafe { plx_base::task::MainThread::assume() };
        let mut eng = engine_after_reload();
        push_one_segment(&mut eng);
        tick(&mt, &mut eng);
        assert!(
            !eng.prime_play,
            "ordinary initial prime should have started"
        );

        eng.prime_play = true;
        SHARED.begin_hls_recovery();
        SHARED.hls_rebuffering.store(true, Ordering::Relaxed);
        tick(&mt, &mut eng);
        assert!(
            eng.prime_play,
            "the old high-water is not newly buffered media"
        );

        SHARED.observe_hls_recovery(500_000, std::time::Duration::from_secs(2));
        push_segment_at(&mut eng, 2_000_000_000);
        tick(&mt, &mut eng);
        assert!(
            !eng.prime_play,
            "a fresh segment covering the measured runway may resume"
        );
        assert!(!SHARED.hls_rebuffering.load(Ordering::Relaxed));
    }

    /// A candidate transaction owns the discretionary reserve its deadline is currently spending.
    /// Starting an already-held native clock in the middle would spend the same balance at the
    /// playhead too. The transaction must reach commit/reject before already-buffered media may
    /// release an initial or internal prime; a running clock is never stopped by this marker.
    #[test]
    fn an_inflight_hls_trial_cannot_release_the_prime_hold() {
        let _serial = plx_base::testlock::serial();
        struct Restore {
            runway: i64,
            trial_reserve: i64,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .hls_prime_runway_ms
                    .store(self.runway, Ordering::Relaxed);
                SHARED
                    .hls_trial_reserve_ms
                    .store(self.trial_reserve, Ordering::Relaxed);
                crate::player::ffi_host::force_clocksink_for_test(false);
            }
        }

        crate::player::ffi_host::force_clocksink_for_test(true);
        let _restore = Restore {
            runway: SHARED.hls_prime_runway_ms.swap(500, Ordering::Relaxed),
            trial_reserve: SHARED.hls_trial_reserve_ms.swap(500, Ordering::Relaxed),
        };
        SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        SHARED.seek_to_ns.store(-1, Ordering::Relaxed);

        let mt = unsafe { plx_base::task::MainThread::assume() };
        let mut eng = engine_after_reload();
        push_one_segment(&mut eng);
        tick(&mt, &mut eng);
        assert!(
            eng.prime_play,
            "Play started while the candidate still owned its measured reserve",
        );

        SHARED.hls_trial_reserve_ms.store(-1, Ordering::Relaxed);
        tick(&mt, &mut eng);
        assert!(
            !eng.prime_play,
            "the completed transaction did not release an otherwise-ready prime",
        );
    }

    #[test]
    fn a_complete_candidate_cycle_invalidates_an_even_prime_snapshot() {
        assert!(hls_candidate_snapshot_stable(40, 40));
        assert!(
            !hls_candidate_snapshot_stable(40, 42),
            "an even->odd->next-even cycle is not the same stable snapshot",
        );
        assert!(!hls_candidate_snapshot_stable(41, 41));
    }

    /// A recovery downshift has no discretionary up-trial reserve. Its first queued candidate
    /// AUs can nevertheless make the shared tails satisfy the OLD rung's recovery certificate
    /// before controller and route ownership commit. The structural publication fence must hold
    /// Play across that window, and a successful transition must begin a fresh actuator epoch
    /// rather than releasing from the mixed snapshot.
    #[test]
    fn a_downshift_candidate_cannot_release_an_old_recovery_epoch_mid_commit() {
        let _serial = plx_base::testlock::serial();
        struct Restore {
            runway: i64,
            trial_reserve: i64,
            settled_generation: u64,
            rebuffering: bool,
            video_tail: i64,
            audio_tail: i64,
            audio_expected: bool,
            pts_shift: i64,
            paused: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .hls_prime_runway_ms
                    .store(self.runway, Ordering::Relaxed);
                SHARED
                    .hls_trial_reserve_ms
                    .store(self.trial_reserve, Ordering::Relaxed);
                SHARED
                    .hls_candidate_generation
                    .store(self.settled_generation, Ordering::Release);
                SHARED
                    .hls_rebuffering
                    .store(self.rebuffering, Ordering::Relaxed);
                SHARED
                    .hls_video_tail_ns
                    .store(self.video_tail, Ordering::Relaxed);
                SHARED
                    .hls_audio_tail_ns
                    .store(self.audio_tail, Ordering::Relaxed);
                SHARED
                    .hls_audio_expected
                    .store(self.audio_expected, Ordering::Relaxed);
                SHARED.pts_shift.store(self.pts_shift, Ordering::Relaxed);
                TX.commit_paused(self.paused);
                SHARED.finish_hls_recovery();
                crate::player::ffi_host::force_clocksink_for_test(false);
            }
        }

        crate::player::ffi_host::force_clocksink_for_test(true);
        let old_paused = TX.paused.load(Ordering::Acquire);
        TX.commit_paused(false);
        let stable_generation = SHARED.hls_candidate_generation.load(Ordering::Acquire);
        assert_eq!(stable_generation & 1, 0, "test starts outside a transition");
        let active_generation = stable_generation.wrapping_add(1);
        let settled_generation = active_generation.wrapping_add(1);
        SHARED
            .hls_candidate_generation
            .compare_exchange(
                stable_generation,
                active_generation,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .expect("test arms one candidate generation");
        let _restore = Restore {
            runway: SHARED.hls_prime_runway_ms.swap(500, Ordering::Relaxed),
            trial_reserve: SHARED.hls_trial_reserve_ms.swap(-1, Ordering::Relaxed),
            settled_generation,
            rebuffering: SHARED.hls_rebuffering.swap(true, Ordering::Relaxed),
            video_tail: SHARED
                .hls_video_tail_ns
                .swap(2_000_000_000, Ordering::Relaxed),
            audio_tail: SHARED
                .hls_audio_tail_ns
                .swap(2_000_000_000, Ordering::Relaxed),
            audio_expected: SHARED.hls_audio_expected.swap(true, Ordering::Relaxed),
            pts_shift: SHARED.pts_shift.swap(0, Ordering::Relaxed),
            paused: old_paused,
        };
        SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        SHARED.seek_to_ns.store(-1, Ordering::Relaxed);
        SHARED.begin_hls_recovery();
        SHARED.observe_hls_recovery(500_000, std::time::Duration::from_secs(2));

        let mt = unsafe { plx_base::task::MainThread::assume() };
        let mut eng = engine_after_reload();
        push_one_segment(&mut eng);
        tick(&mt, &mut eng);
        assert!(
            eng.prime_play,
            "candidate tails released Play before their ownership transition committed",
        );

        // The successful commit publishes a fresh new-rung epoch before the release store.
        SHARED.begin_hls_recovery();
        SHARED
            .hls_candidate_generation
            .store(settled_generation, Ordering::Release);
        tick(&mt, &mut eng);
        assert!(
            eng.prime_play,
            "clearing the fence reused the old actuator's completed recovery sample",
        );

        SHARED.observe_hls_recovery(500_000, std::time::Duration::from_secs(2));
        SHARED
            .hls_video_tail_ns
            .store(4_000_000_000, Ordering::Release);
        SHARED
            .hls_audio_tail_ns
            .store(4_000_000_000, Ordering::Release);
        push_segment_at(&mut eng, 2_000_000_000);
        tick(&mt, &mut eng);
        assert!(
            !eng.prime_play,
            "one complete active-rung acquisition should now release the fresh exact epoch",
        );
    }

    /// Paused Starfish source buffers are finite. Media already demuxed into our AU queues is
    /// nonetheless guaranteed playable, so a pause-local recovery certificate may span both
    /// layers once the native decoder itself has its ordinary prime. Otherwise a full native
    /// buffer plus a full application queue can never release the hold.
    #[test]
    fn internal_rebuffer_certificate_counts_the_whole_playable_pipeline() {
        let _serial = plx_base::testlock::serial();
        struct Restore {
            runway: i64,
            rebuffering: bool,
            video_tail: i64,
            audio_tail: i64,
            audio_expected: bool,
            pts_shift: i64,
            seeking: bool,
            paused: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .hls_prime_runway_ms
                    .store(self.runway, Ordering::Relaxed);
                SHARED
                    .hls_rebuffering
                    .store(self.rebuffering, Ordering::Relaxed);
                SHARED
                    .hls_video_tail_ns
                    .store(self.video_tail, Ordering::Relaxed);
                SHARED
                    .hls_audio_tail_ns
                    .store(self.audio_tail, Ordering::Relaxed);
                SHARED
                    .hls_audio_expected
                    .store(self.audio_expected, Ordering::Relaxed);
                SHARED.pts_shift.store(self.pts_shift, Ordering::Relaxed);
                SHARED.seeking.store(self.seeking, Ordering::Relaxed);
                TX.paused.store(self.paused, Ordering::Relaxed);
                SHARED.finish_hls_recovery();
                crate::player::ffi_host::force_clocksink_for_test(false);
            }
        }
        crate::player::ffi_host::force_clocksink_for_test(true);
        let _restore = Restore {
            runway: SHARED.hls_prime_runway_ms.swap(3_000, Ordering::Relaxed),
            rebuffering: SHARED.hls_rebuffering.swap(true, Ordering::Relaxed),
            video_tail: SHARED
                .hls_video_tail_ns
                .swap(3_500_000_000, Ordering::Relaxed),
            audio_tail: SHARED
                .hls_audio_tail_ns
                .swap(3_500_000_000, Ordering::Relaxed),
            audio_expected: SHARED.hls_audio_expected.swap(true, Ordering::Relaxed),
            pts_shift: SHARED.pts_shift.swap(0, Ordering::Relaxed),
            seeking: SHARED.seeking.swap(false, Ordering::Relaxed),
            paused: TX.paused.swap(false, Ordering::Relaxed),
        };

        let mt = unsafe { plx_base::task::MainThread::assume() };
        let mut eng = engine_after_reload();
        eng.max_fed_video_pts = PRIME_NS;
        eng.max_fed_audio_pts = PRIME_AUDIO_NS;
        SHARED.begin_hls_recovery();
        SHARED.observe_hls_recovery(3_000_000, std::time::Duration::from_secs(3));
        try_prime(&mt, &mut eng);

        assert!(
            !eng.prime_play,
            "three seconds already demuxed behind a decoder-prime must release a 3s runway hold",
        );
        assert!(!SHARED.hls_rebuffering.load(Ordering::Relaxed));
    }

    /// Reproduces the device sequence: Pause, let the demux queues fill, then Resume. The user
    /// intent may reopen feeding, but it must not run the native/ACB clock until Starfish has
    /// accepted a balanced decoder prime. The measured runway may extend into the validated queue
    /// tails because a paused native source buffer is finite and cannot accept the whole runway.
    #[test]
    fn pause_to_fill_resume_primes_both_lanes_before_native_play() {
        let _serial = plx_base::testlock::serial();
        struct Restore {
            runway: i64,
            video_tail: i64,
            audio_tail: i64,
            audio_expected: bool,
            pts_shift: i64,
            presented: i64,
            paused: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .hls_prime_runway_ms
                    .store(self.runway, Ordering::Relaxed);
                SHARED
                    .hls_video_tail_ns
                    .store(self.video_tail, Ordering::Relaxed);
                SHARED
                    .hls_audio_tail_ns
                    .store(self.audio_tail, Ordering::Relaxed);
                SHARED
                    .hls_audio_expected
                    .store(self.audio_expected, Ordering::Relaxed);
                SHARED.pts_shift.store(self.pts_shift, Ordering::Relaxed);
                SHARED.pres_fed.store(self.presented, Ordering::Relaxed);
                TX.commit_paused(self.paused);
                SHARED.reset_hls_clock_for_test();
                crate::player::ffi_host::force_clocksink_for_test(false);
            }
        }

        crate::player::ffi_host::force_clocksink_for_test(true);
        SHARED.reset_hls_clock_for_test();
        let base = 10_000_000_000;
        let _restore = Restore {
            runway: SHARED.hls_prime_runway_ms.swap(3_000, Ordering::Relaxed),
            video_tail: SHARED
                .hls_video_tail_ns
                .swap(base + 3_500_000_000, Ordering::Relaxed),
            audio_tail: SHARED
                .hls_audio_tail_ns
                .swap(base + 3_500_000_000, Ordering::Relaxed),
            audio_expected: SHARED.hls_audio_expected.swap(true, Ordering::Relaxed),
            pts_shift: SHARED.pts_shift.swap(0, Ordering::Relaxed),
            presented: SHARED.pres_fed.swap(base, Ordering::Relaxed),
            paused: TX.paused.swap(true, Ordering::Relaxed),
        };
        let pause = SHARED.prepare_hls_user_pause().expect("reserve user Pause");
        let pause = match pause {
            super::super::shared::HlsUserPause::Issue(token) => token,
            other => panic!("unexpected Pause reservation: {other:?}"),
        };
        assert_eq!(
            SHARED.complete_hls_user_pause(pause, true),
            super::super::shared::HlsPauseCompletion::Accepted
        );
        assert_eq!(
            SHARED.prepare_hls_user_resume(true),
            Some(super::super::shared::HlsUserResume::Prime)
        );

        let mt = unsafe { plx_base::task::MainThread::assume() };
        let mut eng = engine_after_reload();
        eng.prime_play = false;
        arm_live_clock_prime(&mut eng);
        assert_eq!(eng.seek_base_pts, base);
        push_segment_at(&mut eng, base);
        let before = crate::player::ffi_host::play_calls_for_test();
        try_prime(&mt, &mut eng);
        assert_eq!(
            crate::player::ffi_host::play_calls_for_test(),
            before,
            "Resume intent itself issued Play before feeding reopened",
        );

        TX.commit_paused(false);
        tick(&mt, &mut eng);

        let accepted_video = eng.max_fed_video_pts - base;
        let accepted_audio = eng.max_fed_audio_pts - base;
        assert!(accepted_video >= PRIME_NS && accepted_audio >= PRIME_AUDIO_NS);
        assert!(
            accepted_video < 3_000_000_000 && accepted_audio < 3_000_000_000,
            "the fixture must leave part of the measured runway in the application queues",
        );
        assert_eq!(crate::player::ffi_host::play_calls_for_test(), before + 1);
        assert!(
            !eng.prime_play,
            "balanced accepted A/V plus validated queued runway did not release ResumePrime",
        );
    }

    /// A `Play` request is an actuator command, not confirmation that the media clock moved. If
    /// Starfish refuses it, clearing the internal hold would publish smooth playback while the
    /// clock is still stopped and leave no path that retries the command.
    #[test]
    fn a_refused_play_keeps_the_internal_rebuffer_hold_armed() {
        let _serial = plx_base::testlock::serial();
        struct Restore {
            runway: i64,
            rebuffering: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .hls_prime_runway_ms
                    .store(self.runway, Ordering::Relaxed);
                SHARED
                    .hls_rebuffering
                    .store(self.rebuffering, Ordering::Relaxed);
                SHARED.finish_hls_recovery();
                crate::player::ffi_host::force_play_result_for_test(None);
                crate::player::ffi_host::force_clocksink_for_test(false);
            }
        }

        crate::player::ffi_host::force_clocksink_for_test(true);
        crate::player::ffi_host::force_play_result_for_test(Some(0));
        let _restore = Restore {
            runway: SHARED.hls_prime_runway_ms.swap(500, Ordering::Relaxed),
            rebuffering: SHARED.hls_rebuffering.swap(true, Ordering::Relaxed),
        };
        SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        SHARED.seek_to_ns.store(-1, Ordering::Relaxed);

        let mt = unsafe { plx_base::task::MainThread::assume() };
        let mut eng = engine_after_reload();
        SHARED.begin_hls_recovery();
        SHARED.observe_hls_recovery(500_000, std::time::Duration::from_secs(2));
        push_one_segment(&mut eng);
        tick(&mt, &mut eng);

        assert!(
            eng.prime_play,
            "a refused Play must remain pending for the next pump tick"
        );
        assert!(
            SHARED.hls_rebuffering.load(Ordering::Relaxed),
            "the UI/runtime hold was cleared without a successful clock transition",
        );
    }
}

#[cfg(all(test, feature = "hostsim"))]
mod lifecycle_clock_tests {
    use super::*;

    #[test]
    fn stop_without_an_engine_cannot_poison_the_next_initial_hold() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe {
            plx_base::task::MainThread::assume()
        });
        assert!(
            !pa.is_live(),
            "test requires an empty native-session slot"
        );
        SHARED.reset_hls_clock_for_test();

        stop_bufferfeed(&mut ps, &mut pa);

        assert!(
            SHARED.arm_initial_clock_hold(false, false),
            "a no-op stop left ClockState in Stopping"
        );
        SHARED.cancel_clock_session_start();
    }
}

/// **LG App Self Checklist #46 — replay after completion.** A `plxnative-playurl` stream that
/// reaches EOS is started AGAIN, and that second start does not travel the UI's asynchronous
/// request path: `app.rs` re-arms `auto_tried`, the fixture entry calls `start_playback`, and
/// `start_bufferfeed_tracked` asks the reducer for a start owner directly. So the phase a
/// completed stop leaves behind is the whole contract between the two halves.
/// Gated on `hostsim` for the same reason [`lifecycle_clock_tests`] is: `stop_bufferfeed` reaches
/// the video sink, and only the simulator's (`ffi_host::HostSink`) keeps the state this asserts;
/// the default build's `NoSink` answers every verb with nothing.
#[cfg(all(test, feature = "hostsim"))]
mod replay_after_stop_tests {
    use super::*;

    /// Device-observed 2026-09-02 (`pipe_replay_after_eos`): `stop_bufferfeed: torn down` was
    /// followed by `replay: starting the finished stream again (0 left)` and then
    /// `start_bufferfeed: route reducer refused a start owner` — one Load and one HTTP fetch
    /// where the case requires two. The stop parks the reducer in `Stopping`, which is the fence
    /// for the *duration* of the synchronous main-thread teardown; nothing moved it out once that
    /// teardown returned, and `begin_route_start` mints a transaction only from a phase which owns
    /// no live Engine to preserve.
    #[test]
    fn a_completed_stop_grants_the_next_start_a_route_owner() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe {
            plx_base::task::MainThread::assume()
        });
        crate::route::reset_player_control_for_test(&ps);
        assert!(
            !pa.is_live(),
            "test requires an empty native-session slot"
        );

        stop_bufferfeed(&mut ps, &mut pa);

        let start = crate::route::begin_route_start()
            .expect("a completed stop must grant the replay a start owner");
        assert!(
            crate::route::prepare_route_start(start),
            "the replay owns the transaction it was just granted"
        );
        let attempt = crate::route::claim_route_start_attempt(start)
            .expect("a granted transaction must mint one physical Load attempt");
        assert!(crate::route::settle_route_start(&mut ps, 
            attempt,
            crate::route::RouteStartResult::Started
        ));

        crate::route::reset_player_control_for_test(&ps);
    }
}

/// Test-only: an Engine parked at `Stage::Streaming` on the host sink, for the pump-level rigs
/// that live outside this module (`pump::flight_tests`).
#[cfg(all(test, feature = "hostsim"))]
pub(super) fn streaming_engine_for_test(native_epoch: u32) -> Engine {
    let mut eng = prime_livelock_tests::engine_after_reload();
    eng.native_epoch = native_epoch;
    eng
}

/// **BACK's frame does not pay for `Unload`** (`PendingRelease`). On the dev set
/// `StarfishMediaAPIs::Unload` held the frame that handled BACK for 113-150 ms on every return
/// (`teardown spans: … sf_unload:135.7`). The host sink is given a slow Unload of its own
/// (`delay_unload_for_test`) and the contract is graded where the device's cannot be: the stop
/// returns long before the Unload does, nothing the stopped object says reaches the state the next
/// session owns, no native session can begin while the object is still being released, and a start
/// issued meanwhile is queued behind the release instead of racing it — and the release still ends in a
/// D1 with complete evidence, not a quarantine.
#[cfg(all(test, feature = "hostsim"))]
mod off_frame_release_tests {
    use super::*;

    const UNLOAD_MS: u64 = 400;

    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::player::ffi_host::reset_native_lifecycle_for_test();
            crate::player::ffi_host::force_clocksink_for_test(false);
            SHARED.test_force_native_idle();
            SHARED.reset_session();
            crate::route::reset_player_control_for_test(crate::route::idle_session_for_test());
        }
    }

    fn ready_now() -> c_int {
        unsafe { crate::player::sink().ready(&MainThread::assume()) }
    }

    /// A live Engine over a ready host object whose Load has returned: what BACK tears down.
    fn playing_engine(pa: &mut crate::player::adapter::PlayerAdapter) -> u32 {
        let epoch = SHARED.begin_native_session().expect("native session");
        assert_eq!(unsafe { crate::player::sink().load(c"{}".as_ptr(), epoch) }, 1);
        assert!(SHARED.mark_native_load_returned(epoch, None).is_some());
        let mut eng = streaming_engine_for_test(epoch);
        eng.load_th = None;
        pa.install(eng);
        assert_ne!(
            ready_now(),
            0,
            "PRECONDITION FAILED: the object must be dispatchable, or the stop has nothing to Unload"
        );
        epoch
    }

    fn arm(ps: &crate::route::PlaybackSession) -> crate::player::adapter::PlayerAdapter {
        SHARED.reset_session();
        crate::route::reset_player_control_for_test(ps);
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        crate::player::adapter::PlayerAdapter::new(unsafe { MainThread::assume() })
    }

    #[test]
    fn a_stop_returns_before_the_unload_and_the_next_start_is_deferred_until_its_tail_ran() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        let epoch = playing_engine(&mut pa);
        crate::player::ffi_host::delay_unload_for_test(UNLOAD_MS);

        let started = std::time::Instant::now();
        stop_bufferfeed(&mut ps, &mut pa);
        let stop = started.elapsed();
        assert!(
            stop < std::time::Duration::from_millis(UNLOAD_MS / 4),
            "the stop held the frame for {stop:?} on a {UNLOAD_MS} ms Unload: BACK pays for it again"
        );
        assert!(!pa.is_live(), "the stop must still complete from the route's point of view");
        assert!(pa.has_pending_release(), "the object must be parked for its main-thread tail");

        // A callback the stopped object delivers now (a late error, say) is dropped: the session
        // state was already reset for whatever plays next.
        let before = SHARED.dg_cb_count.load(Ordering::Relaxed);
        super::super::sf_on_event(epoch, 5, 0, std::ptr::null());
        assert_eq!(
            SHARED.dg_cb_count.load(Ordering::Relaxed),
            before,
            "a callback from the object being released reached the next session's state"
        );
        assert!(
            SHARED.begin_native_session().is_none(),
            "a native session began while the previous object was still inside Unload"
        );
        assert!(
            !poll_release(&mut ps, &mut pa, std::time::Instant::now()),
            "the per-frame poll must not wait for a running Unload"
        );

        // A start now (no URL, so it would end NoRoute) is deferred: it neither waits for the
        // Unload nor runs over the object, and the frame poll starts it once the tail has run.
        let before_start = std::time::Instant::now();
        let outcome = start_bufferfeed_tracked(&mut ps, &mut pa);
        assert_eq!(outcome, BufferfeedStartOutcome::Deferred);
        assert!(
            before_start.elapsed() < std::time::Duration::from_millis(UNLOAD_MS / 4),
            "the start waited for the Unload"
        );
        assert!(pa.has_pending_release(), "the Unload is still running");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !poll_release(&mut ps, &mut pa, std::time::Instant::now()) {
            assert!(std::time::Instant::now() < deadline, "the returned Unload was never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(UNLOAD_MS),
            "the tail ran before the Unload returned"
        );
        assert!(!pa.has_pending_release(), "the tail must leave no release pending");
        assert_eq!(ready_now(), 0, "the released object must be destroyed before a new one may load");
        assert!(
            !crate::player::ffi_host::lifecycle_blocked_for_test(),
            "the off-frame release must end in a D1 with complete evidence, not a quarantine"
        );
        let next = SHARED.begin_native_session().expect("native sessions resume after the release");
        assert!(SHARED.retire_native_session(next));
    }

    #[test]
    fn the_frame_poll_finishes_a_returned_unload_and_exit_waits_for_a_running_one() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;

        // The ordinary return: the Unload comes back while Home draws; a later frame's poll runs
        // the tail.
        playing_engine(&mut pa);
        crate::player::ffi_host::delay_unload_for_test(50);
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.has_pending_release());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !poll_release(&mut ps, &mut pa, std::time::Instant::now()) {
            assert!(std::time::Instant::now() < deadline, "the returned Unload was never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!pa.has_pending_release());
        assert_eq!(ready_now(), 0);
        assert!(!crate::player::ffi_host::lifecycle_blocked_for_test(), "the poll's tail must D1");

        // The exit (`app::run::shutdown`): a release still running is waited for, not abandoned.
        crate::route::reset_player_control_for_test(&ps);
        playing_engine(&mut pa);
        crate::player::ffi_host::delay_unload_for_test(UNLOAD_MS);
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.has_pending_release());
        finish_release_for_test(&mut pa);
        assert!(!pa.has_pending_release(), "finish_release must not return with the Unload running");
        assert_eq!(ready_now(), 0);
        assert!(!crate::player::ffi_host::lifecycle_blocked_for_test());
    }

    #[test]
    fn the_releases_own_unloadcompleted_does_not_count_into_the_next_sessions_diagnostics() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        playing_engine(&mut pa);
        crate::player::ffi_host::delay_unload_for_test(50);
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.has_pending_release());
        finish_release_for_test(&mut pa);
        assert!(!crate::player::ffi_host::lifecycle_blocked_for_test(), "the release must still D1");
        assert_eq!(
            SHARED.dg_cb_count.load(Ordering::Relaxed),
            0,
            "the stopped object's UNLOADCOMPLETED (after reset_session) counted into the next session's callback census"
        );
        assert_eq!(SHARED.dg_cb_err_at.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn taking_the_token_or_splitting_never_waits_for_a_running_unload() {
        for use_split in [false, true] {
            let mut ps = crate::route::PlaybackSession::IDLE;
            let _serial = plx_base::testlock::serial();
            let mut pa = arm(&ps);
            let _cleanup = Cleanup;
            playing_engine(&mut pa);
            crate::player::ffi_host::delay_unload_for_test(UNLOAD_MS);
            let started = std::time::Instant::now();
            stop_bufferfeed(&mut ps, &mut pa);
            assert!(pa.has_pending_release());
            let before = std::time::Instant::now();
            if use_split {
                let _ = pa.split();
            } else {
                let _ = pa.mt();
            }
            assert!(
                before.elapsed() < std::time::Duration::from_millis(UNLOAD_MS / 4),
                "{} waited for the running Unload (PLX-NATIVE-1Q)",
                if use_split { "split()" } else { "mt()" }
            );
            // …and a verb through the fenced sink does not reach the object the worker is inside.
            assert_eq!(
                unsafe { crate::player::sink().pause(pa.mt()) },
                0,
                "a main-thread verb reached the object while it was Releasing"
            );
            assert!(pa.has_pending_release(), "nothing may finish the release but its own poll");
            finish_release_for_test(&mut pa);
            assert!(
                started.elapsed() >= std::time::Duration::from_millis(UNLOAD_MS),
                "the release finished before the Unload returned"
            );
            assert_eq!(ready_now(), 0);
            assert!(!crate::player::ffi_host::lifecycle_blocked_for_test());
        }
    }

    #[test]
    fn a_refused_unload_thread_unloads_on_the_main_thread_and_still_d1s() {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                FAIL_UNLOAD_SPAWN_FOR_TEST.store(false, Ordering::Relaxed);
            }
        }
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        let _reset = Reset;
        playing_engine(&mut pa);
        crate::player::ffi_host::delay_unload_for_test(150);
        FAIL_UNLOAD_SPAWN_FOR_TEST.store(true, Ordering::Relaxed);
        let started = std::time::Instant::now();
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(150),
            "with no thread the Unload must have run synchronously inside the stop"
        );
        assert!(!pa.has_pending_release(), "no worker exists, so nothing may be parked");
        assert_eq!(ready_now(), 0, "the object must be destroyed");
        assert!(!crate::player::ffi_host::lifecycle_blocked_for_test(), "the fallback must end in a D1");
    }

    /// Lets the held host `Unload` go when a test unwinds, so a failed assertion cannot leave
    /// the next test's release hanging.
    struct ReleaseUnloadOnDrop;
    impl Drop for ReleaseUnloadOnDrop {
        fn drop(&mut self) {
            crate::player::ffi_host::release_unload_for_test();
        }
    }

    /// PLX-NATIVE-1Q: a start over an `Unload` that never returns froze the UI thread in the
    /// join. It must come back at once, deferred, and fail typed at the deadline.
    #[test]
    fn a_start_over_an_unload_that_never_returns_does_not_block_and_fails_typed_at_the_deadline() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        let _release = ReleaseUnloadOnDrop;
        playing_engine(&mut pa);
        crate::player::ffi_host::hold_unload_for_test();
        let t_stop = std::time::Instant::now();
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.has_pending_release());
        // A helper lets the hold go after 3 s, so the unfixed code ends instead of hanging.
        let helper = std::thread::Builder::new()
            .name("test-unload-release".into())
            .spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(3));
                crate::player::ffi_host::release_unload_for_test();
            })
            .expect("spawn helper");

        let started = std::time::Instant::now();
        let outcome = start_bufferfeed_tracked(&mut ps, &mut pa);
        let took = started.elapsed();
        assert!(
            took < std::time::Duration::from_millis(50),
            "the start blocked the UI thread for {took:?} over an Unload that had not returned"
        );
        assert_eq!(outcome, BufferfeedStartOutcome::Deferred);
        assert!(!pa.is_live());
        assert!(SHARED.begin_native_session().is_none());
        assert!(pa.has_pending_release());
        assert_eq!(
            crate::player::state(&ps),
            crate::player::PlaybackState::Resolving,
            "a deferred start must read as Connecting, not as an empty player"
        );

        // The deadline, on the machine's injected clock: the queued start fails typed.
        let t0 = t_stop;
        assert!(!poll_release(&mut ps, &mut pa, t0 + crate::player::release::RELEASE_DEADLINE - std::time::Duration::from_millis(1)));
        assert_ne!(
            crate::player::error_now(&ps).kind,
            crate::player::FailureKind::ReleaseTimeout,
            "the read-out appeared before the deadline"
        );
        assert!(!poll_release(&mut ps, &mut pa, t0 + crate::player::release::RELEASE_DEADLINE + std::time::Duration::from_secs(5)));
        assert_eq!(crate::player::state(&ps), crate::player::PlaybackState::Error);
        let shape = crate::player::error_now(&ps);
        assert_eq!(shape.kind, crate::player::FailureKind::ReleaseTimeout);
        assert_eq!(shape.kind.code(), "release_timeout");
        let actions = crate::player::failure_actions(
            shape.kind,
            crate::player::FailureContext { forced: false, can_retry: true, repair_idle: true },
        );
        assert!(actions.contains(&crate::player::FailureAction::TryAgain), "{actions:?}");

        // A second start is refused with no new object; the leaked object was not D1'd.
        assert_eq!(start_bufferfeed_tracked(&mut ps, &mut pa), BufferfeedStartOutcome::Failed);
        assert!(!pa.is_live());
        assert_eq!(crate::player::error_now(&ps).kind, crate::player::FailureKind::ReleaseTimeout);
        assert_ne!(
            unsafe { crate::player::raw_sink().ready(&MainThread::assume()) },
            0,
            "the overdue object was destroyed under a worker that may still be inside it"
        );
        assert!(pa.has_pending_release(), "the overdue release must stay parked");
        assert!(pa.release_overdue());

        // The Unload finally returns: the tail runs, nothing quarantines, the next start goes.
        helper.join().unwrap(); // released by the helper after 3 s
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !poll_release(&mut ps, &mut pa, std::time::Instant::now()) {
            assert!(std::time::Instant::now() < give_up, "the late Unload was never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!pa.has_pending_release());
        assert_eq!(ready_now(), 0);
        assert!(!crate::player::ffi_host::lifecycle_blocked_for_test(), "the late tail must D1");
        assert_ne!(
            start_bufferfeed_tracked(&mut ps, &mut pa),
            BufferfeedStartOutcome::Deferred,
            "the slot must be free again"
        );
        assert_ne!(crate::player::error_now(&ps).kind, crate::player::FailureKind::ReleaseTimeout);
    }

    /// A stop over a deferred start ends that playback: the queued start must not run after the tail.
    #[test]
    fn a_stop_over_a_deferred_start_cancels_it() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        playing_engine(&mut pa);
        crate::player::ffi_host::delay_unload_for_test(50);
        stop_bufferfeed(&mut ps, &mut pa);
        assert_eq!(start_bufferfeed_tracked(&mut ps, &mut pa), BufferfeedStartOutcome::Deferred);
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(!pa.release_has_queued_start());
        assert_ne!(crate::player::state(&ps), crate::player::PlaybackState::Resolving);
        // The frame poll (may_start = true) is what would launch a surviving queued start.
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pa.release_busy() {
            assert!(std::time::Instant::now() < give_up, "the Unload never returned");
            poll_release(&mut ps, &mut pa, std::time::Instant::now());
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!pa.is_live(), "the cancelled start ran after the tail");
        assert_eq!(ready_now(), 0);
    }

    /// A landing replaces the whole session (`apply_plan` rebuilds it). The queued start lives in the
    /// release machine, not in the session, so a stop after that landing still cancels it.
    #[test]
    fn a_stop_after_a_landing_replaced_the_session_still_cancels_the_queued_start() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        playing_engine(&mut pa);
        crate::player::ffi_host::delay_unload_for_test(50);
        stop_bufferfeed(&mut ps, &mut pa);
        assert_eq!(start_bufferfeed_tracked(&mut ps, &mut pa), BufferfeedStartOutcome::Deferred);
        assert_eq!(crate::player::state(&ps), crate::player::PlaybackState::Resolving, "derived from the machine");
        ps = crate::route::PlaybackSession::IDLE; // the landing's rebuilt session
        assert_eq!(crate::player::state(&ps), crate::player::PlaybackState::Resolving, "the session carries nothing");
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(!pa.release_has_queued_start());
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pa.release_busy() {
            assert!(std::time::Instant::now() < give_up, "the Unload never returned");
            poll_release(&mut ps, &mut pa, std::time::Instant::now());
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!pa.is_live(), "the queued start survived a stop and ran after the tail");
    }

    /// The foreground restore over a held `Unload` neither blocks nor loses the start: it parks, and
    /// lands once the release goes Idle, or fails typed at the deadline.
    #[test]
    fn a_foreground_start_over_a_held_unload_parks_without_blocking_then_lands() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        let _release = ReleaseUnloadOnDrop;
        playing_engine(&mut pa);
        crate::player::ffi_host::hold_unload_for_test();
        stop_bufferfeed(&mut ps, &mut pa);
        let t = std::time::Instant::now();
        assert_eq!(start_bufferfeed_tracked(&mut ps, &mut pa), BufferfeedStartOutcome::Deferred);
        pa.watch_queued_start();
        assert_eq!(pa.take_queued_landing(), crate::player::adapter::QueuedLanding::Waiting);
        poll_release(&mut ps, &mut pa, std::time::Instant::now());
        assert!(t.elapsed() < std::time::Duration::from_millis(50), "the restore blocked the frame");
        assert_eq!(pa.take_queued_landing(), crate::player::adapter::QueuedLanding::Waiting);
        crate::player::ffi_host::release_unload_for_test();
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let landing = loop {
            poll_release(&mut ps, &mut pa, std::time::Instant::now());
            match pa.take_queued_landing() {
                crate::player::adapter::QueuedLanding::Waiting => {
                    assert!(std::time::Instant::now() < give_up, "the deferred start never ran");
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                other => break other,
            }
        };
        assert!(
            !matches!(landing, crate::player::adapter::QueuedLanding::Waiting | crate::player::adapter::QueuedLanding::Stale),
            "{landing:?}"
        );
    }

    /// A preview whose start is queued behind the release is not admitted while queued, and a
    /// deadline that refuses it (or a run that fails) settles the admission instead of leaving it counted.
    /// (Admission on a SUCCESSFUL launch needs a route fixture the rig does not have.)
    #[test]
    fn a_deferred_preview_start_is_settled_when_it_runs_not_when_queued() {
        use crate::player::preview::{phase_for_test, set_phase_for_test, reset_for_test, Phase};
        for refuse in [false, true] {
            let mut ps = crate::route::PlaybackSession::IDLE;
            let _serial = plx_base::testlock::serial();
            let mut pa = arm(&ps);
            let _cleanup = Cleanup;
            let _release = ReleaseUnloadOnDrop;
            reset_for_test();
            playing_engine(&mut pa);
            crate::player::ffi_host::hold_unload_for_test();
            stop_bufferfeed(&mut ps, &mut pa);
            set_phase_for_test(Phase::Fetching);
            assert_eq!(start_bufferfeed_tracked(&mut ps, &mut pa), BufferfeedStartOutcome::Deferred);
            pa.admit_preview_at_launch(true);
            assert_eq!(phase_for_test(), Phase::Fetching, "queued is not admitted");
            if refuse {
                let later = std::time::Instant::now() + crate::player::release::RELEASE_DEADLINE + std::time::Duration::from_secs(1);
                poll_release(&mut ps, &mut pa, later);
                assert_eq!(phase_for_test(), Phase::Idle, "a refused start gives the slot back");
            } else {
                crate::player::ffi_host::release_unload_for_test();
                let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while phase_for_test() == Phase::Fetching {
                    assert!(std::time::Instant::now() < give_up, "the queued start never launched");
                    poll_release(&mut ps, &mut pa, std::time::Instant::now());
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                // The fixture session has no URL, so the start runs and ends NoRoute: the admission is
                // settled when the start RUNS (here refused, the slot given back), not when queued.
                assert_eq!(phase_for_test(), Phase::Idle, "settled at the run, not before");
            }
            reset_for_test();
        }
    }

    /// …and past the deadline the parked restore fails typed (`release_timeout`), still without blocking.
    #[test]
    fn a_foreground_start_over_an_overdue_unload_fails_typed() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        let _release = ReleaseUnloadOnDrop;
        playing_engine(&mut pa);
        crate::player::ffi_host::hold_unload_for_test();
        stop_bufferfeed(&mut ps, &mut pa);
        assert_eq!(start_bufferfeed_tracked(&mut ps, &mut pa), BufferfeedStartOutcome::Deferred);
        pa.watch_queued_start();
        let later = std::time::Instant::now() + crate::player::release::RELEASE_DEADLINE + std::time::Duration::from_secs(1);
        poll_release(&mut ps, &mut pa, later);
        assert_eq!(pa.take_queued_landing(), crate::player::adapter::QueuedLanding::Failed);
        assert_eq!(crate::player::error_now(&ps).kind, crate::player::FailureKind::ReleaseTimeout);
    }

    /// The exit with an `Unload` that never returns is bounded by the deadline and says a worker
    /// is still in firmware, so the display is not torn down under it.
    #[test]
    fn exit_with_an_unload_that_never_returns_is_bounded_and_reports_a_worker_in_firmware() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        let _release = ReleaseUnloadOnDrop;
        playing_engine(&mut pa);
        crate::player::ffi_host::hold_unload_for_test();
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.has_pending_release());
        // An injected clock that jumps a second per look: no real sleep, the deadline in ~11 reads.
        let t0 = std::time::Instant::now();
        let mut ticks = 0u64;
        let exit = exit_native_with(
            &mut pa,
            || {
                ticks += 1;
                t0 + std::time::Duration::from_secs(ticks)
            },
            || {},
        );
        assert_eq!(exit, NativeExit::WorkerInFirmware);
        assert!(ticks >= 10, "the exit gave up before the deadline ({ticks} looks)");
        // The object is still in firmware: the adapter must not own it (dropping the adapter would
        // free the Engine under the live Unload), and its drop must never run.
        assert!(!pa.has_pending_release(), "the overdue object must leave the adapter, leaked");
        let dropped = ENGINES_DROPPED.load(Ordering::SeqCst);
        drop(pa);
        assert_eq!(ENGINES_DROPPED.load(Ordering::SeqCst), dropped, "dropping the adapter freed the Engine");
        crate::player::ffi_host::release_unload_for_test();
    }

    /// The flake behind `exit_after_a_load_timeout_never_quits_the_display_under_a_hung_load`: a
    /// test that ends with its `sf-unload` worker still held leaves a DETACHED thread whose
    /// `sf_unload` later reads the process-global `ACTIVE_EPOCH` (by then the NEXT test's session)
    /// and delivers UNLOADCOMPLETED into it. The reset every test opens and closes with must wait
    /// the worker out, so none outlives its test.
    #[test]
    fn the_lifecycle_reset_waits_out_a_released_unload_worker() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        let _release = ReleaseUnloadOnDrop;
        playing_engine(&mut pa);
        crate::player::ffi_host::hold_unload_for_test();
        stop_bufferfeed(&mut ps, &mut pa);
        assert_eq!(
            crate::player::ffi_host::unload_workers_for_test(),
            1,
            "PRECONDITION FAILED: the stop must have a worker inside the held Unload"
        );
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        assert_eq!(
            crate::player::ffi_host::unload_workers_for_test(),
            0,
            "the reset returned with an sf-unload worker still alive: it could fire into the next test"
        );
    }

    #[test]
    fn an_unloadcompleted_from_the_wrong_epoch_during_the_release_is_not_evidence() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        let epoch = playing_engine(&mut pa);
        crate::player::ffi_host::delay_unload_for_test(UNLOAD_MS);
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.has_pending_release());
        let before = SHARED.dg_cb_count.load(Ordering::Relaxed);
        super::super::sf_on_event(epoch + 1, 23, 0, std::ptr::null());
        super::super::sf_on_event(0, 23, 0, std::ptr::null());
        assert_eq!(SHARED.dg_cb_count.load(Ordering::Relaxed), before, "a wrong-epoch event was processed");
        assert!(
            !SHARED.native_unload_completed(epoch),
            "a wrong-epoch UNLOADCOMPLETED counted as the release's D1 evidence"
        );
        finish_release_for_test(&mut pa);
        assert!(!crate::player::ffi_host::lifecycle_blocked_for_test());
    }
}

/// issue #74 D.1 regression tests. Real v0.6.0 dispatches every Starfish/ACB verb through
/// `sf_ready_object()` alone (`src/starfish.c`), which went live at `SMP_SET_READY(1)` BEFORE the
/// real, blocking `sf_load` call (`g_load_with_context`) returned — nothing waited for that
/// return. So the whole duration of a Load was a window in which `sf_is_load_completed`/
/// `sf_play`/`sf_feed`/etc. could dispatch into an object whose LoadCommon ctor path was still
/// running on the load thread: investigation.md's run A (a null-arg SIGSEGV on the priming path)
/// and run B (a Play-vs-LoadCommon mutex deadlock ending in an OMX_VideoComp abort).
#[cfg(all(test, feature = "hostsim"))]
mod load_in_flight_tests {
    use super::*;

    /// The Engine itself needs no cleanup since phase 9 — it lives in the test's own
    /// `PlayerAdapter`, which drops with the test. What is still process-wide is `SHARED`, the
    /// native lifecycle test seams, and the route reducer's projection.
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::player::ffi_host::reset_native_lifecycle_for_test();
            crate::player::ffi_host::force_clocksink_for_test(false);
            crate::player::ffi_host::set_load_in_flight_for_test(false);
            SHARED.test_force_native_idle();
            SHARED.reset_session();
            crate::route::reset_player_control_for_test(crate::route::idle_session_for_test());
        }
    }

    /// An engine parked in `Stage::Loading`, native session installed, not yet primed. Mirrors
    /// [`prime_livelock_tests::engine_after_reload`] but for the state a fresh `Load` is in
    /// BEFORE it returns, rather than after.
    ///
    /// `rebase_pending: true` stands in for a segmented-HLS route fixture: `route::is_segmented_hls`
    /// is decided by `route.rs`, which another package in this run owns. `rebase_pending` drives
    /// the exact same `prime_before_play` branch at the pump's loadCompleted arm
    /// (`pump.rs::prime_before_play`) that segmented HLS does — the branch investigation.md §A.2
    /// (run A) actually took — so the code path under test is identical.
    fn engine_loading(epoch: u32) -> Engine {
        let mut eng = super::prime_livelock_tests::engine_after_reload();
        eng.native_epoch = epoch;
        eng.stage = Stage::Loading;
        eng.rebase_pending = true;
        eng.prime_play = false;
        eng.max_fed_video_pts = 0;
        eng.max_fed_audio_pts = 0;
        eng
    }

    /// Waits (bounded — this is a HOST test, never the television) for the host seam's
    /// `LOAD_IN_FLIGHT` window to actually open. This project's silent-instrument rule
    /// (AGENTS.md): a seam that never entered the in-flight window would score zero dispatches
    /// and prove nothing, so the precondition is its own assertion with its own message, kept
    /// separate from the real assertion below.
    fn wait_for_load_in_flight() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !crate::player::ffi_host::load_in_flight_for_test() {
            assert!(
                std::time::Instant::now() < deadline,
                "PRECONDITION FAILED: sf_load never entered its in-flight window (loadCompleted \
                 emitted, not yet returned). A run that never opens this window would score zero \
                 seam dispatches and prove nothing about the gate — see AGENTS.md's \
                 silent-instrument rule."
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// The primary test: real concurrency, a real second thread running `threads::load_thread`,
    /// a real held `sf_load`.
    #[test]
    fn nothing_reaches_the_seam_while_load_is_still_in_flight() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        SHARED.reset_session();
        crate::route::reset_player_control_for_test(&ps);
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;

        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe {
            plx_base::task::MainThread::assume()
        });
        let epoch = SHARED.begin_native_session().expect("native session");
        pa.install(engine_loading(epoch));

        // Push a whole segment's worth onto both lanes now, the way a demuxer already running
        // ahead of Load would (investigation.md §A.4 names exactly this as what differed between
        // the dev set's clean runs and the reporter's segmented-HLS one) — enough to clear both
        // `PRIME_NS` (video) and `PRIME_AUDIO_NS` (audio), so the positive half below can reach a
        // real `sf_play` once the gate opens, not just an engine that primed and stalled.
        {
            const V_STEP_NS: i64 = 41_666_666; // 24 fps
            const A_STEP_NS: i64 = 21_333_333; // 1024 samples at 48 kHz
            const VIDEO_AUS: i64 = 30; // ~1.25s > PRIME_NS (700ms)
            const AUDIO_AUS: i64 = 20; // ~427ms > PRIME_AUDIO_NS (300ms)
            let eng = pa.engine().expect("engine installed above");
            let au = [0u8; 32];
            let qv = &mut **eng.aq_video.as_mut().unwrap() as *mut crate::aq::AuQueue;
            for i in 0..VIDEO_AUS {
                crate::aq::aq_push(
                    qv,
                    au.as_ptr(),
                    au.len() as c_int,
                    i * V_STEP_NS,
                    c_int::from(i == 0),
                    1,
                );
            }
            let qa = &mut **eng.aq_audio.as_mut().unwrap() as *mut crate::aq::AuQueue;
            for i in 0..AUDIO_AUS {
                crate::aq::aq_push(qa, au.as_ptr(), au.len() as c_int, i * A_STEP_NS, 1, 2);
            }
        }

        crate::player::ffi_host::hold_load_for_test();
        let payload = std::ffi::CString::new("{}").unwrap();
        let payload_ptr = threads::SendPtr(payload.as_ptr() as *mut c_char);
        let load_thread = std::thread::Builder::new()
            .name("test-load-thread".into())
            .spawn(move || threads::load_thread(payload_ptr, epoch, None, PcmLead::Off))
            .expect("spawn load_thread");

        wait_for_load_in_flight();

        // The primary assertion: several real pump ticks, none of which may reach the seam.
        for _ in 0..5 {
            crate::player::pump::pump(&mut ps, &mut pa, 1_000);
        }
        assert_eq!(
            crate::player::ffi_host::in_flight_calls_for_test(), 0,
            "the pump reached the Starfish/ACB seam ({:?}) while StarfishMediaAPIs::Load was \
             still executing on the load thread. In issue #74 that window is where the app died: \
             a SIGSEGV in libc with a null first argument on the priming path, and a \
             Play-vs-LoadCommon deadlock ending in an OMX_VideoComp abort on the immediate-Play \
             path.",
            crate::player::ffi_host::in_flight_verb_for_test()
        );
        assert!(
            !SHARED.native_load_returned(epoch),
            "sanity: the Rust-side gate must still read closed while sf_load has not returned"
        );
        assert!(
            pa.engine().is_some_and(|e| e.stage == Stage::Loading),
            "the pump must not have advanced past Loading while Load was still in flight"
        );

        // Release the held Load and let it actually return.
        crate::player::ffi_host::release_load_for_test();
        load_thread.join().expect("load_thread joins");

        // The positive half: the gate isn't just permanently closed. A few more ticks must now
        // advance past Loading and reach Play — proving the fix doesn't just refuse forever.
        let mut advanced = false;
        for _ in 0..20 {
            crate::player::pump::pump(&mut ps, &mut pa, 1_000);
            if pa.engine().is_some_and(|e| e.stage != Stage::Loading) {
                advanced = true;
                break;
            }
        }
        assert!(
            SHARED.native_load_returned(epoch),
            "the Rust-side gate must open once threads::load_thread has actually returned"
        );
        assert!(
            advanced,
            "the gate opening must let the pump leave Stage::Loading"
        );
        assert!(
            crate::player::ffi_host::play_calls_for_test() > 0,
            "once Load has genuinely returned, Play must eventually be reached — the gate must \
             not simply stay closed forever"
        );
    }

    /// `load-budget-skipped-when-the-gate-can-never-open` (a fix-wave finding, not a P1a
    /// acceptance line): the D.1.4 budget below shares its precondition with the gate it exists
    /// to bound — both `native_load_elapsed` and `native_load_returned` require the epoch to
    /// still own `NativeSessionPhase::Active`. A synchronous UnloadCompleted callback for the
    /// live epoch (firmware type=23) flips the phase to `Unloaded` without `load_call` ever
    /// having reached `Returned`, which — before this fix — left `pump.rs`'s loadCompleted arm
    /// deferring forever with no deadline: exactly the permanent Connecting spinner D.1.4 exists
    /// to make impossible, on the one path where the gate can never open again.
    #[test]
    fn losing_the_active_phase_while_still_deferred_fires_the_budget_instead_of_hanging() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        SHARED.reset_session();
        crate::route::reset_player_control_for_test(&ps);
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;

        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe {
            plx_base::task::MainThread::assume()
        });
        let epoch = SHARED.begin_native_session().expect("native session");
        pa.install(engine_loading(epoch));

        // `sf_ready(mt)` must already read nonzero (SMP_READY true) BEFORE the loadCompleted arm
        // is even reached — pump.rs's own `sf_ready == 0 => Connecting` early return sits ahead
        // of it — so this needs the same real held-Load rig as
        // `nothing_reaches_the_seam_while_load_is_still_in_flight`, not a bare phase flip.
        crate::player::ffi_host::hold_load_for_test();
        let payload = std::ffi::CString::new("{}").unwrap();
        let payload_ptr = threads::SendPtr(payload.as_ptr() as *mut c_char);
        let load_thread = std::thread::Builder::new()
            .name("test-load-thread-2".into())
            .spawn(move || threads::load_thread(payload_ptr, epoch, None, PcmLead::Off))
            .expect("spawn load_thread");
        wait_for_load_in_flight();

        assert!(
            !SHARED.native_load_returned(epoch),
            "sanity: the gate starts closed"
        );

        // Simulate the firmware's synchronous UnloadCompleted callback for this epoch while the
        // pump is still deferring loadCompleted (Load is genuinely still executing on the load
        // thread) — the phase leaves Active for Unloaded without `load_call` ever reaching
        // `Returned`.
        SHARED.with_native_session(
            epoch,
            crate::player::shared::NativeEventClass::UnloadCompleted,
            23,
            || {},
        );
        assert!(
            SHARED.native_load_elapsed(epoch).is_none(),
            "PRECONDITION FAILED: the epoch must have actually left Active, or this test proves \
             nothing about the hazard (AGENTS.md's silent-instrument rule)"
        );

        assert!(!SHARED.load_failed.load(std::sync::atomic::Ordering::Acquire));
        crate::player::pump::pump(&mut ps, &mut pa, 1_000);
        assert!(
            SHARED.load_failed.load(std::sync::atomic::Ordering::Acquire),
            "losing Active while loadCompleted was still deferred must fire the D.1.4 budget \
             immediately rather than silently disabling it — the permanent Connecting spinner \
             this exists to rule out"
        );

        // Unblock and join the held load thread so the process doesn't leak it into later tests.
        crate::player::ffi_host::release_load_for_test();
        load_thread.join().expect("load_thread joins");
    }

    /// `load-budget-expiry-path-itself-has-no-test` (a fix-wave finding): of this arm's three
    /// match arms, `losing_the_active_phase_while_still_deferred_fires_the_budget_instead_of_hanging`
    /// above pins the `None` arm, but `Some(elapsed) if elapsed >= NATIVE_LOAD_BUDGET` — the arm
    /// the D.1.4 acceptance criterion is actually about — was reachable only by waiting a real 20s,
    /// so nothing drove it: inverting the comparison to `elapsed < NATIVE_LOAD_BUDGET` left the
    /// host suite green. `Shared::test_backdate_native_load_issued` (test-only) rewinds the
    /// recorded `load_issued_at` past the budget instead, so the same real held-Load rig as
    /// `nothing_reaches_the_seam_while_load_is_still_in_flight` can drive the actual comparison
    /// without a sleep.
    #[test]
    fn native_load_budget_expiry_fires_load_failed() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        SHARED.reset_session();
        crate::route::reset_player_control_for_test(&ps);
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;

        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe {
            plx_base::task::MainThread::assume()
        });
        let epoch = SHARED.begin_native_session().expect("native session");
        pa.install(engine_loading(epoch));

        crate::player::ffi_host::hold_load_for_test();
        let payload = std::ffi::CString::new("{}").unwrap();
        let payload_ptr = threads::SendPtr(payload.as_ptr() as *mut c_char);
        let load_thread = std::thread::Builder::new()
            .name("test-load-thread-budget".into())
            .spawn(move || threads::load_thread(payload_ptr, epoch, None, PcmLead::Off))
            .expect("spawn load_thread");
        wait_for_load_in_flight();

        assert!(
            SHARED.test_backdate_native_load_issued(epoch, crate::player::pump::NATIVE_LOAD_BUDGET),
            "PRECONDITION FAILED: the epoch must still own the Active phase to backdate its \
             load_issued_at, or this test proves nothing about the arm under test"
        );
        assert!(
            SHARED
                .native_load_elapsed(epoch)
                .is_some_and(|e| e >= crate::player::pump::NATIVE_LOAD_BUDGET),
            "PRECONDITION FAILED: the backdate must actually push elapsed past the budget — a \
             silent-instrument trap (AGENTS.md) if it doesn't"
        );
        assert!(
            !SHARED.load_failed.load(std::sync::atomic::Ordering::Acquire),
            "sanity: load_failed starts clear"
        );

        crate::player::pump::pump(&mut ps, &mut pa, 1_000);

        assert!(
            SHARED.load_failed.load(std::sync::atomic::Ordering::Acquire),
            "elapsed >= NATIVE_LOAD_BUDGET must publish load_failed — an inverted or otherwise \
             broken comparison here would leave a Load that never returns spinning forever \
             instead of reaching the failure read-out"
        );

        // Unblock and join the held load thread so the process doesn't leak it into later tests.
        crate::player::ffi_host::release_load_for_test();
        load_thread.join().expect("load_thread joins");
    }

    /// `load-returned-log-is-conditional-on-loadcompleted-and-unbounded-after`: the OTHER half of
    /// D.1.4's budget. `native_load_budget_expiry_fires_load_failed` above pins "Load never
    /// returns"; this pins "Load returns, but `loadCompleted` never arrives" — a wait that was
    /// completely unbounded before this fix, since `pump.rs`'s D.1.4 arm only runs while
    /// `!native_load_returned`, and stops being reachable the instant Load returns.
    ///
    /// Drives the gate by hand (like `the_gate_is_epoch_scoped` below): no real `sf_load` call is
    /// made at all — the host stub's `sf_load` unconditionally emits the `loadCompleted` callback
    /// before it can return, so it cannot represent "Load returned but loadCompleted never
    /// arrived". `force_object_ready_for_test` gives `sf_ready()` its `true` without that call,
    /// leaving `LOADED` (and so `sf_is_load_completed()`) at its default `false` — the "quiet
    /// pipeline" this fix is about.
    #[test]
    fn native_load_returned_loadcompleted_never_arrives_fires_load_failed() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        SHARED.reset_session();
        crate::route::reset_player_control_for_test(&ps);
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        crate::player::ffi_host::force_object_ready_for_test(true);
        let _cleanup = Cleanup;

        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe {
            plx_base::task::MainThread::assume()
        });
        let epoch = SHARED.begin_native_session().expect("native session");
        pa.install(engine_loading(epoch));

        assert!(
            SHARED.mark_native_load_returned(epoch, None).is_some(),
            "PRECONDITION FAILED: Load must actually be marked Returned, or this test proves \
             nothing about the second-half budget"
        );
        assert!(
            SHARED.native_load_returned(epoch),
            "PRECONDITION FAILED: the gate must read Returned"
        );
        assert!(
            !SHARED.load_completed.load(std::sync::atomic::Ordering::Relaxed),
            "PRECONDITION FAILED: loadCompleted must genuinely never have arrived"
        );
        assert_ne!(
            unsafe { crate::player::sink().ready(pa.mt()) },
            0,
            "PRECONDITION FAILED: sf_ready() must read true (force_object_ready_for_test), or \
             pump() bails out at its own sf_ready wait before ever reaching the arm under test"
        );
        assert_eq!(
            unsafe { crate::player::sink().is_load_completed(pa.mt()) },
            0,
            "PRECONDITION FAILED: sf_is_load_completed() must read false — if it reads true, the \
             OTHER arm (loadCompleted arrived) fires instead and this test proves nothing about \
             the arm under test"
        );

        assert!(
            SHARED.test_backdate_native_load_returned(
                epoch,
                crate::player::pump::NATIVE_LOAD_BUDGET
            ),
            "PRECONDITION FAILED: the epoch must still own the Active phase to backdate when it \
             returned, or this test proves nothing about the arm under test"
        );
        assert!(
            SHARED
                .native_load_returned_elapsed(epoch)
                .is_some_and(|e| e >= crate::player::pump::NATIVE_LOAD_BUDGET),
            "PRECONDITION FAILED: the backdate must actually push the returned-elapsed past the \
             budget — a silent-instrument trap (AGENTS.md) if it doesn't"
        );
        assert!(
            !SHARED.load_failed.load(std::sync::atomic::Ordering::Acquire),
            "sanity: load_failed starts clear"
        );

        crate::player::pump::pump(&mut ps, &mut pa, 1_000);

        assert!(
            SHARED.load_failed.load(std::sync::atomic::Ordering::Acquire),
            "loadCompleted never arriving within NATIVE_LOAD_BUDGET of Load RETURNING must \
             publish load_failed — an unbounded wait here is exactly the permanent-Connecting- \
             spinner shape D.1.4 exists to rule out, on the half that was still open"
        );
    }

    /// A narrow SIMULATION of the epoch-scoping property, not a historical reproduction of the
    /// crash — it drives both sides of the gate by hand (no real second thread, no real held
    /// `sf_load`) to prove a mark for a DIFFERENT (superseded) epoch can never open the current
    /// one's gate. See `nothing_reaches_the_seam_while_load_is_still_in_flight` for the
    /// real-concurrency test that this one deliberately does not attempt to replace.
    #[test]
    fn the_gate_is_epoch_scoped() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        SHARED.reset_session();
        crate::route::reset_player_control_for_test(&ps);
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;

        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe {
            plx_base::task::MainThread::assume()
        });
        let epoch = SHARED.begin_native_session().expect("native session");
        pa.install(engine_loading(epoch));

        // Get the host seam's OBJECT_READY (so `sf_ready(mt)` reads true and the pump reaches
        // the loadCompleted arm at all) without arming HOLD_LOAD — this call returns at once.
        let payload = std::ffi::CString::new("{}").unwrap();
        assert_eq!(unsafe { crate::player::sink().load(payload.as_ptr(), epoch) }, 1);
        // Hand-set the in-flight window and publish loadCompleted via the existing sf_on_event
        // path — simulating "Load has not returned" without a real second thread.
        crate::player::ffi_host::set_load_in_flight_for_test(true);
        super::super::sf_on_event(epoch, 2, 0, c"{\"loadCompleted\":true}".as_ptr());

        crate::player::pump::pump(&mut ps, &mut pa, 1_000);
        assert_eq!(
            crate::player::ffi_host::in_flight_calls_for_test(), 0,
            "loadCompleted alone, with the Rust-side gate not yet marked Returned for this \
             epoch, must not let any seam verb dispatch ({:?})",
            crate::player::ffi_host::in_flight_verb_for_test()
        );
        assert!(
            pa.engine().is_some_and(|e| e.stage == Stage::Loading),
            "the gate must still be closed"
        );

        // Mark a DIFFERENT epoch as returned. Since it never owns the Active phase, this must be
        // a complete no-op for the session actually running.
        let other_epoch = epoch.wrapping_add(1).max(1);
        assert_ne!(other_epoch, epoch, "the test must pick a genuinely different epoch");
        assert!(
            SHARED.mark_native_load_returned(other_epoch, None).is_none(),
            "marking an epoch that does not own the Active phase must be a no-op"
        );
        assert!(
            !SHARED.native_load_returned(epoch),
            "a mark for a DIFFERENT epoch must never open THIS epoch's gate — this is the \
             epoch-scoping property that stops a late thread from a superseded session opening \
             the gate for a newer one"
        );

        crate::player::pump::pump(&mut ps, &mut pa, 1_000);
        assert_eq!(
            crate::player::ffi_host::in_flight_calls_for_test(), 0,
            "the pump must still defer after a differently-epoched mark ({:?})",
            crate::player::ffi_host::in_flight_verb_for_test()
        );
        assert!(
            pa.engine().is_some_and(|e| e.stage == Stage::Loading),
            "marking an epoch other than the pump's own must never open ITS gate"
        );

        crate::player::ffi_host::set_load_in_flight_for_test(false);
    }

    #[test]
    fn native_source_correction_ctor_wait_has_an_issued_budget() {
        let _serial = plx_base::testlock::serial();
        SHARED.reset_session();
        crate::route::reset_player_control_for_test(&crate::route::PlaybackSession::IDLE);
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let mut pa = super::super::adapter::PlayerAdapter::new(unsafe { plx_base::task::MainThread::assume() });
        let mut ps = crate::route::PlaybackSession::IDLE;
        let epoch = SHARED.begin_native_session().unwrap();
        pa.install(engine_loading(epoch));
        assert_eq!(unsafe { crate::player::sink().ready(pa.mt()) }, 0);
        crate::player::pump::pump(&mut ps, &mut pa, 1000);
        assert!(!SHARED.load_failed.load(Ordering::Acquire));
        assert!(SHARED.test_backdate_native_load_issued(epoch, crate::player::pump::NATIVE_LOAD_BUDGET));
        crate::player::pump::pump(&mut ps, &mut pa, 1001);
        assert!(SHARED.load_failed.load(Ordering::Acquire), "constructor readiness must not bypass the issued budget");
    }
    #[test]
    fn native_source_correction_stale_load_refusal_cannot_fail_new_epoch() {
        let _serial = plx_base::testlock::serial();
        SHARED.reset_session();
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let mt = unsafe { plx_base::task::MainThread::assume() };
        unsafe { crate::player::sink().quarantine(&mt); }
        let old = SHARED.begin_native_session().unwrap();
        assert!(SHARED.retire_native_session(old));
        let current = SHARED.begin_native_session().unwrap();
        threads::load_thread(threads::SendPtr(c"{}".as_ptr() as *mut c_char), old, None, PcmLead::Off);
        assert!(!SHARED.load_failed.load(Ordering::Acquire), "a stale worker must not poison the newer epoch");
        assert!(!SHARED.native_load_returned(current));
        threads::load_thread(threads::SendPtr(c"{}".as_ptr() as *mut c_char), current, None, PcmLead::Off);
        assert!(SHARED.load_failed.load(Ordering::Acquire), "a current synchronous refusal must still fail");
    }

    #[test]
    fn native_ready_demux_failure_keeps_precedence_over_the_load_wait() {
        let _serial = plx_base::testlock::serial();
        SHARED.reset_session();
        crate::route::reset_player_control_for_test(&crate::route::PlaybackSession::IDLE);
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        crate::player::ffi_host::force_object_ready_for_test(true);
        let _cleanup = Cleanup;
        let mut pa = super::super::adapter::PlayerAdapter::new(unsafe { plx_base::task::MainThread::assume() });
        let mut ps = crate::route::PlaybackSession::IDLE;
        let epoch = SHARED.begin_native_session().unwrap();
        pa.install(engine_loading(epoch));
        crate::player::pump::pump(&mut ps, &mut pa, 1000);
        assert_eq!(crate::player::state(&ps), crate::player::PlaybackState::Connecting);
        SHARED.demux_failed.store(true, Ordering::Release);
        crate::player::pump::pump(&mut ps, &mut pa, 1001);
        assert_eq!(crate::player::state(&ps), crate::player::PlaybackState::Error);
    }

    /// PR #105 review finding: the D.1.4 budget turns a `Load` that never returns into a
    /// failure read-out, but BACK from that read-out ran `teardown`, which JOINED the media
    /// thread on the SDL main thread before anything else — so a genuinely hung `sf_load` froze
    /// the whole app the moment the user tried to leave the screen that told them about it.
    ///
    /// The held host `sf_load` is the hang. A watchdog releases it after `WATCHDOG` so a broken
    /// teardown FAILS this test (elapsed ≈ WATCHDOG) instead of hanging the suite.
    #[test]
    fn teardown_after_a_load_timeout_does_not_block_on_the_hung_load() {
        const WATCHDOG: std::time::Duration = std::time::Duration::from_secs(3);
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        SHARED.reset_session();
        crate::route::reset_player_control_for_test(&ps);
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;

        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe {
            plx_base::task::MainThread::assume()
        });
        let epoch = SHARED.begin_native_session().expect("native session");
        let mut eng = engine_loading(epoch);
        crate::player::ffi_host::hold_load_for_test();
        let payload_ptr = threads::SendPtr(eng.payload.as_ptr() as *mut c_char);
        eng.load_th = Some(
            std::thread::Builder::new()
                .name("test-load-thread-hung".into())
                .spawn(move || threads::load_thread(payload_ptr, epoch, None, PcmLead::Off))
                .expect("spawn load_thread"),
        );
        pa.install(eng);
        wait_for_load_in_flight();

        assert!(
            SHARED.test_backdate_native_load_issued(epoch, crate::player::pump::NATIVE_LOAD_BUDGET),
            "PRECONDITION FAILED: the epoch must still own Active to backdate it"
        );
        crate::player::pump::pump(&mut ps, &mut pa, 1_000);
        assert!(
            SHARED.load_timed_out.load(Ordering::Acquire),
            "PRECONDITION FAILED: the budget must have fired, or this test is not about a timed-out Load"
        );

        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watchdog = {
            let (done, fired) = (done.clone(), fired.clone());
            std::thread::Builder::new().name("test-load-watchdog".into()).spawn(move || {
                let deadline = std::time::Instant::now() + WATCHDOG;
                while std::time::Instant::now() < deadline {
                    if done.load(Ordering::Acquire) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                fired.store(true, Ordering::Release);
                crate::player::ffi_host::release_load_for_test();
            }).expect("spawn watchdog")
        };

        let started = std::time::Instant::now();
        stop_bufferfeed(&mut ps, &mut pa);
        let elapsed = started.elapsed();
        done.store(true, Ordering::Release);
        watchdog.join().unwrap();

        assert!(
            !fired.load(Ordering::Acquire) && elapsed < std::time::Duration::from_secs(1),
            "teardown blocked the main thread for {elapsed:?} on a Load that never returned \
             (watchdog had to release it: {}) — BACK from the timeout read-out freezes the app",
            fired.load(Ordering::Acquire)
        );
        assert_eq!(
            crate::player::ffi_host::in_flight_calls_for_test(),
            0,
            "teardown reached the Starfish seam ({:?}) while Load was still in flight — the #74 race",
            crate::player::ffi_host::in_flight_verb_for_test()
        );
        assert!(!pa.is_live(), "the stop must still complete from the route's point of view");
        assert!(pa.has_abandoned_load(), "the in-flight Load must be parked, not dropped");
        assert_ne!(
            unsafe { crate::player::sink().ready(pa.mt()) },
            0,
            "the object must be neither unloaded, destroyed nor quarantined while Load is in flight"
        );
        assert!(
            SHARED.begin_native_session().is_none(),
            "no new native session may begin while the seam still owns the abandoned object"
        );

        // While Load is still in flight the per-frame release is a no-op and a start is refused.
        for _ in 0..3 {
            assert!(!reap_abandoned_load(&mut pa));
        }
        assert!(pa.has_abandoned_load());
        let start = crate::route::begin_route_start().expect("a completed stop grants a start owner");
        // No URL is set, so an unguarded start would answer `NoRoute`; `StartFailed` is the refusal.
        assert_eq!(
            start_bufferfeed_inner(&mut ps, &mut pa, start).err(),
            Some(crate::route::RouteStartResult::StartFailed),
            "a later playback must not be handed the object an abandoned Load still owns"
        );
        let _ = crate::route::abort_route_start(start, crate::route::RouteStartResult::StartFailed);
        assert_eq!(crate::player::ffi_host::in_flight_calls_for_test(), 0, "{:?}", crate::player::ffi_host::in_flight_verb_for_test());

        // Now the Load returns. The next reap releases the object with full evidence (D1, not a
        // quarantine), and native sessions are available again.
        crate::player::ffi_host::release_load_for_test();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !reap_abandoned_load(&mut pa) {
            assert!(std::time::Instant::now() < deadline, "the returned Load was never released");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!pa.has_abandoned_load());
        assert_eq!(unsafe { crate::player::sink().ready(pa.mt()) }, 0, "the released object must be gone");
        assert!(
            !crate::player::ffi_host::lifecycle_blocked_for_test(),
            "the release must be a D1 with complete evidence, not a process-long quarantine"
        );
        let next = SHARED.begin_native_session().expect("native sessions resume after the release");
        assert!(SHARED.retire_native_session(next));
    }

    /// Releases the held host `sf_load` when a test unwinds, so a failed assertion cannot leave
    /// the next test's Load hanging.
    struct ReleaseLoadOnDrop;
    impl Drop for ReleaseLoadOnDrop {
        fn drop(&mut self) {
            crate::player::ffi_host::release_load_for_test();
        }
    }

    /// An Engine whose `sf_load` is held past the budget: the pump's budget arm has fired
    /// (`load_failed` + `load_timed_out`), the Load thread is still inside `sf_load`, and the
    /// Engine is installed in `pa`. The state the Auto Original's recovery starts from.
    fn install_engine_with_timed_out_load(
        ps: &mut crate::route::PlaybackSession,
        pa: &mut crate::player::adapter::PlayerAdapter,
    ) {
        let epoch = SHARED.begin_native_session().expect("native session");
        let mut eng = engine_loading(epoch);
        crate::player::ffi_host::hold_load_for_test();
        let payload_ptr = threads::SendPtr(eng.payload.as_ptr() as *mut c_char);
        eng.load_th = Some(
            std::thread::Builder::new()
                .name("test-load-thread-hung".into())
                .spawn(move || threads::load_thread(payload_ptr, epoch, None, PcmLead::Off))
                .expect("spawn load_thread"),
        );
        pa.install(eng);
        wait_for_load_in_flight();
        assert!(
            SHARED.test_backdate_native_load_issued(epoch, crate::player::pump::NATIVE_LOAD_BUDGET),
            "PRECONDITION FAILED: the epoch must still own Active to backdate it"
        );
        crate::player::pump::pump(ps, pa, 1_000);
        assert!(
            SHARED.load_timed_out.load(Ordering::Acquire),
            "PRECONDITION FAILED: the budget must have fired, or this test is not about a timed-out Load"
        );
    }

    /// The exit after a playback error (`load_timeout`): `app::run::shutdown` then quits SDL and
    /// destroys the Wayland display. A Load still inside firmware on the `media` thread must make
    /// the exit say so, so the display is left to the OS rather than torn down under it.
    #[test]
    fn exit_after_a_load_timeout_never_quits_the_display_under_a_hung_load() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        SHARED.reset_session();
        crate::route::reset_player_control_for_test(&ps);
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _release = ReleaseLoadOnDrop;
        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe {
            plx_base::task::MainThread::assume()
        });
        install_engine_with_timed_out_load(&mut ps, &mut pa);
        stop_bufferfeed(&mut ps, &mut pa);
        let exit = exit_native(&mut pa);
        assert!(
            exit == NativeExit::WorkerInFirmware || !crate::player::ffi_host::load_in_flight_for_test(),
            "the exit's native half returned with a Load still inside firmware: SDL_Quit would run under it"
        );
        assert_eq!(exit, NativeExit::WorkerInFirmware);
        // The payload the `media` thread reads is leaked, not owned by the adapter that is dropped next.
        assert!(!pa.has_abandoned_load(), "the parked Load must leave the adapter, leaked");
        crate::player::ffi_host::release_load_for_test();
    }

    /// PLX-NATIVE-15: the Auto Original's Load passes its budget, the HLS recovery reloads while
    /// the Load is still inside `sf_load`, the reload's teardown parks that Load (clearing
    /// `load_timed_out` with the rest of the session) and the start is refused. The failure was
    /// published with no cause, so the report said `unspecified`; the cause is the timeout.
    #[test]
    fn a_recovery_reload_refused_for_an_abandoned_load_still_reports_the_timeout() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        SHARED.reset_session();
        crate::route::reset_player_control_for_test(&ps);
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _release = ReleaseLoadOnDrop;
        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe {
            plx_base::task::MainThread::assume()
        });
        crate::route::set_url(&mut ps, "http://127.0.0.1:1/hls/start.m3u8");
        install_engine_with_timed_out_load(&mut ps, &mut pa);

        // `land_recovery_flight`'s own two calls: the reload (whose teardown parks the
        // still-running Load) and the settle that publishes the outcome.
        let started = crate::player::pump::settle_reload(
            reload_transcode(&mut ps, &mut pa, 0),
            "test HLS recovery",
        );
        assert!(!started, "the recovery start must be refused while the Load is outstanding");
        assert!(pa.has_abandoned_load(), "PRECONDITION FAILED: the Load must still be parked");
        assert_eq!(crate::player::state(&ps), crate::player::PlaybackState::Error);
        assert_eq!(
            crate::player::error_now(&ps).kind,
            crate::player::FailureKind::LoadTimeout,
            "the refusal must keep the timeout as its cause, not report `unspecified`"
        );
    }

    /// The fresh-start half of the same hole: a Retry or Back+Play while the parked Load is still
    /// inside `sf_load` is refused with no Engine to publish anything, so the player sat in Idle
    /// and drew nothing: no read-out, no report, no Try Again. The refusal stays (a second native
    /// object must not be started), but the viewer is told why.
    #[test]
    fn a_fresh_start_refused_for_an_abandoned_load_shows_an_error_with_try_again() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        SHARED.reset_session();
        crate::route::reset_player_control_for_test(&ps);
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _release = ReleaseLoadOnDrop;
        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe {
            plx_base::task::MainThread::assume()
        });
        install_engine_with_timed_out_load(&mut ps, &mut pa);
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.has_abandoned_load(), "PRECONDITION FAILED: the Load must be parked");

        // `finish_landed_play`'s entry. A refused start answers false and creates no Engine.
        assert!(!crate::player::start_bufferfeed(&mut ps, &mut pa));
        assert!(!pa.is_live(), "the start stays refused: no second native object");
        assert!(pa.has_abandoned_load());

        assert_eq!(
            crate::player::state(&ps),
            crate::player::PlaybackState::Error,
            "a refused fresh start left the player Idle: a black screen with no read-out"
        );
        let shape = crate::player::error_now(&ps);
        assert_eq!(shape.kind, crate::player::FailureKind::LoadTimeout);
        let actions = crate::player::failure_actions(
            shape.kind,
            crate::player::FailureContext { forced: false, can_retry: true, repair_idle: true },
        );
        assert!(
            actions.contains(&crate::player::FailureAction::TryAgain),
            "the read-out must offer Try Again: {actions:?}"
        );

        // The Load returns and is reaped: the next start is no longer refused for it, and the
        // refusal's read-out does not outlive the refusal.
        crate::player::ffi_host::release_load_for_test();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !reap_abandoned_load(&mut pa) {
            assert!(std::time::Instant::now() < deadline, "the returned Load was never released");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = crate::player::start_bufferfeed(&mut ps, &mut pa);
        assert_ne!(
            crate::player::state(&ps),
            crate::player::PlaybackState::Error,
            "a start that is no longer refused must not keep the refusal's read-out"
        );
    }

    /// The latch's other edge: a start refused for the parked Load sets `native_start_refused`,
    /// and a RELOAD that then starts (the foreground restore's rollback landing after the Load was
    /// reaped: `land_engineless_rollback` -> `reload_transcode_tracked`) goes through
    /// `start_bufferfeed_for`, not `start_bufferfeed_tracked`, which is where the flag used to be
    /// cleared. The flag outlived the refusal and, once the flight ended, `state()` answered Error
    /// over playing video and `report::tick` sent a bogus `load_timeout`.
    #[test]
    fn a_reload_that_starts_after_a_refused_fresh_start_does_not_keep_the_refusals_error() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        SHARED.reset_session();
        crate::route::reset_player_control_for_test(&ps);
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _release = ReleaseLoadOnDrop;
        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe {
            plx_base::task::MainThread::assume()
        });
        install_engine_with_timed_out_load(&mut ps, &mut pa);
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.has_abandoned_load(), "PRECONDITION FAILED: the Load must be parked");
        crate::route::set_url(&mut ps, "http://127.0.0.1:1/hls/start.m3u8");
        crate::route::set_stream_acodec(&mut ps, "aac");

        // The resumed foreground's `PlayerForegroundActuator::start_load`.
        assert_eq!(
            start_bufferfeed_tracked(&mut ps, &mut pa),
            BufferfeedStartOutcome::Failed,
            "PRECONDITION FAILED: the fresh start must be refused for the parked Load"
        );
        assert!(ps.native_start_refused.is_some(), "PRECONDITION FAILED: the refusal must be recorded");

        // The parked Load returns and is reaped; the rollback's reload then starts.
        crate::player::ffi_host::release_load_for_test();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !reap_abandoned_load(&mut pa) {
            assert!(std::time::Instant::now() < deadline, "the returned Load was never released");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let reload = reload_transcode_tracked(&mut ps, &mut pa, 0);
        assert!(
            matches!(reload, BufferfeedStartOutcome::Launched(_)),
            "PRECONDITION FAILED: the reload must start once the Load is reaped ({reload:?})"
        );
        assert!(pa.is_live(), "the reload must have built an Engine");

        assert_ne!(
            crate::player::state(&ps),
            crate::player::PlaybackState::Error,
            "a reload that started kept the refused fresh start's Error over live video \
             (and report::tick would send a bogus load_timeout)"
        );
        stop_bufferfeed(&mut ps, &mut pa);
    }


}

#[cfg(test)]
mod pcm_info_tests {
    use super::*;
    use crate::route::PcmFormat;

    const BASE: &str = r#"{"contents":{"codec":{"audio":"PCM"},"provider":"plxnative"}}"#;

    #[test]
    fn a_pcm_load_states_the_shape_the_decoder_produces() {
        let f = PcmFormat { sample_rate: 48_000, bits: 16, channels: 2, source_rate: 48_000 };
        let p = with_pcm_info(BASE, "PCM", Some(f));
        assert!(p.contains(r#""pcmInfo":{"format":"S16LE","channelMode":"stereo","layout":"interleaved","sampleRate":48,"bitsPerSample":16}"#), "{p}");
        let wide = with_pcm_info(BASE, "PCM", Some(PcmFormat { sample_rate: 48_000, bits: 32, channels: 1, source_rate: 96_000 }));
        assert!(wide.contains(r#""format":"S32LE","channelMode":"mono""#) && wide.contains(r#""bitsPerSample":32"#), "{wide}");
        // the payload stays parseable JSON
        serde_json::from_str::<serde_json::Value>(&p).unwrap();
    }

    /// **A hi-res track the app resamples is declared as what the decoder emits**, never as the file's own rate:
    /// the Load is built from the format `route::pcm_format_of` returns, which names the PCM input's table
    /// rate in S32 (a 16-bit source included), so the payload carries 48 or 44.1 kHz and 32 bits and none of 96, 192 or 88.2.
    #[test]
    fn a_resampled_hi_res_track_declares_the_tables_rate_and_a_32_bit_sample() {
        let track = |rate: i64, bits: i64| crate::route::CarriedAudio {
            sid: 7, ordinal: 4, codec: "flac".into(), channels: 2,
            can_normalize_loudness: false, immersive: false, sample_rate: rate, bit_depth: bits,
        };
        for (rate, bits, khz, never) in [(96_000, 24, "48", ["96", "192", "88.2"]), (192_000, 24, "48", ["96", "192", "88.2"]), (88_200, 16, "44.1", ["96", "192", "88.2"])] {
            let f = crate::route::pcm_format_of(Some(&track(rate, bits)), "flac").unwrap();
            let p = with_pcm_info(BASE, "PCM", Some(f));
            let v: serde_json::Value = serde_json::from_str(&p).unwrap();
            let info = &v["contents"]["pcmInfo"];
            assert_eq!(info["format"], "S32LE", "{rate}/{bits}: {p}");
            assert_eq!(info["bitsPerSample"], 32, "{rate}/{bits}: {p}");
            assert_eq!(info["sampleRate"].as_f64(), khz.parse::<f64>().ok(), "{rate}/{bits}: {p}");
            for n in never {
                assert!(!p.contains(&format!(r#""sampleRate":{n}"#)), "{rate}/{bits}: the file's own rate {n} must not be declared: {p}");
            }
        }
    }

    /// The diagnostics line names the track's own rate only where the app resamples it.
    #[test]
    fn the_decode_log_line_names_the_source_rate_only_when_it_is_resampled() {
        let exact = PcmFormat { sample_rate: 48_000, bits: 16, channels: 2, source_rate: 48_000 };
        assert_eq!(pcm_decode_line("flac", &exact), "pcm: flac decoded in-app -> 48000Hz 16-bit 2ch");
        let hires = PcmFormat { sample_rate: 48_000, bits: 32, channels: 2, source_rate: 96_000 };
        assert_eq!(pcm_decode_line("flac", &hires), "pcm: flac decoded in-app -> 48000Hz 32-bit 2ch (resampled from 96000Hz)");
        assert_eq!(pcm_decode_line("dca", &PcmFormat::STEREO_48K), "pcm: dca decoded in-app -> 48000Hz 16-bit 2ch");
    }

    /// Every rate the PCM path admits is declared in kilohertz, the unit libpf's table is in. In
    /// hertz each of them played at the 44.1 kHz fallback on the B8, with no error anywhere.
    #[test]
    fn the_sample_rate_is_declared_in_kilohertz() {
        let want = ["8", "12", "16", "22.05", "24", "32", "44.1", "48"];
        for (hz, khz) in plx_platform::devcaps::PCM_SAMPLE_RATES.into_iter().zip(want) {
            assert_eq!(pcm_rate_khz(hz), khz);
            let p = with_pcm_info(BASE, "PCM", Some(PcmFormat { sample_rate: hz, bits: 16, channels: 2, source_rate: hz }));
            let v: serde_json::Value = serde_json::from_str(&p).unwrap();
            let declared = v["contents"]["pcmInfo"]["sampleRate"].as_f64().unwrap();
            assert!((declared * 1000.0 - f64::from(hz)).abs() < 1e-6, "{hz}: {p}");
        }
        assert_eq!(pcm_rate_khz(11_025), "11.025");
    }

    #[test]
    fn a_speed_session_leaves_the_pcm_to_the_speed_decoder() {
        let f = || Some(Ok(PcmFormat { sample_rate: 44_100, bits: 16, channels: 2, source_rate: 44_100 }));
        assert_eq!(app_pcm_for_rate(crate::player::RATE_NORMAL, f()), f(), "1x: the FLAC decoder's shape");
        assert!(app_pcm_for_rate(1500, f()).is_none(), "1.5x: the speed decoder declares its own");
        // a FLAC track whose facts are missing only fails at 1x, where the FLAC decoder needs them
        let missing = || Some(Err("unknown".to_string()));
        assert!(app_pcm_for_rate(crate::player::RATE_NORMAL, missing()).is_some_and(|r| r.is_err()));
        assert!(app_pcm_for_rate(750, missing()).is_none());
    }

    /// **DTS the attempt decodes in the app** is declared as PCM — stereo 48 kHz S16, one `pcmInfo` — and
    /// where it does not is `DTS` exactly as before. The shape is the session's own
    /// (`route::stream_pcm`), so a native DTS Load can never carry a PCM node nor the reverse.
    #[test]
    fn app_decoded_dts_is_declared_as_pcm_and_native_dts_as_dts() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        crate::route::set_stream_codecs(&mut ps, "h264", "dts");
        assert_eq!(audio_declaration("dts", crate::route::stream_pcm(&ps).and_then(Result::ok).as_ref()), Some(("DTS", 4)));
        let off = build_av_payload_at(&ps, "H264", "DTS", ENVELOPE_FHD60, crate::player::RATE_NORMAL);
        assert!(off.contains(r#""audio":"DTS""#) && !off.contains("pcmInfo"), "{off}");

        crate::route::begin_attempt_for_test(
            &mut ps, std::sync::Arc::new(plx_platform::devcaps::Caps::assumed().with_app_lossy()),
        );
        let shape = crate::route::stream_pcm(&ps).and_then(Result::ok);
        assert_eq!(shape, Some(crate::route::PcmFormat::STEREO_48K));
        assert_eq!(audio_declaration("dts", shape.as_ref()), Some(("PCM", 5)));
        let on = build_av_payload_at(&ps, "H264", "PCM", ENVELOPE_FHD60, crate::player::RATE_NORMAL);
        assert!(on.contains(r#""audio":"PCM""#), "{on}");
        assert_eq!(on.matches("pcmInfo").count(), 1, "{on}");
        assert!(on.contains(r#""format":"S16LE","channelMode":"stereo","layout":"interleaved","sampleRate":48,"bitsPerSample":16"#), "{on}");
        // at a speed the speed decoder declares PCM itself, from the same codec: still one node
        let fast = build_av_payload_at(&ps, "H264", "PCM", ENVELOPE_FHD60, 1500);
        assert_eq!(fast.matches("pcmInfo").count(), 1, "{fast}");
        serde_json::from_str::<serde_json::Value>(&on).unwrap();
    }

    /// **Force Direct Play on a set with no DTS row declares a STEREO DTS track as stereo 48 kHz PCM**: the
    /// app decodes it (the table's lossy policy admits DTS, the set has no native path, and a 2.0 source
    /// is no downmix, `Caps::app_may_mix_down`), so the Load carries one `pcmInfo` and `PCM` instead of a
    /// `DTS` the pipeline would hang on. Under Auto the same value still declares `DTS`. The carried track
    /// supplies the channel count the rule reads; a 5.1 one is refused before any Load
    /// (`forced_resolve_refuses_a_surround_dts_track_instead_of_mixing_it_down`).
    #[test]
    fn a_forced_dts_with_no_row_is_declared_as_stereo_48k_pcm() {
        let caps = std::sync::Arc::new(plx_platform::devcaps::Caps {
            app_lossy: plx_platform::devcaps::AppLossy::Measured,
            ..plx_platform::devcaps::Caps::assumed()
        });
        let mut ps = crate::route::PlaybackSession::IDLE;
        crate::route::begin_attempt_for_test(&mut ps, caps);
        crate::route::set_stream_codecs(&mut ps, "h264", "dts");
        crate::route::set_carried_audio_for_test(
            &mut ps,
            crate::route::CarriedAudio {
                sid: 7, ordinal: 1, codec: "dts".into(), channels: 2,
                can_normalize_loudness: false, immersive: false, sample_rate: 48_000, bit_depth: 0,
            },
        );
        assert_eq!(crate::route::stream_pcm(&ps).and_then(Result::ok), None, "Auto: the native decoder's");
        crate::route::set_direct_play_mode_for_test(&mut ps, crate::route::DirectPlayMode::Forced);
        let shape = crate::route::stream_pcm(&ps).and_then(Result::ok);
        assert_eq!(shape, Some(crate::route::PcmFormat::STEREO_48K));
        assert_eq!(audio_declaration("dts", shape.as_ref()), Some(("PCM", 5)));
        let on = build_av_payload_at(&ps, "H264", "PCM", ENVELOPE_FHD60, crate::player::RATE_NORMAL);
        assert!(on.contains(r#""audio":"PCM""#) && !on.contains(r#""audio":"DTS""#), "{on}");
        assert_eq!(on.matches("pcmInfo").count(), 1, "{on}");
        serde_json::from_str::<serde_json::Value>(&on).unwrap();
    }

    /// MP3 and Vorbis in force (S4-4) are declared exactly as DTS is: `PCM`, stereo 48 kHz S16.
    #[test]
    fn app_decoded_mp3_and_vorbis_are_declared_as_pcm_and_native_ones_are_refused() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        for codec in ["mp3", "vorbis"] {
            crate::route::set_stream_codecs(&mut ps, "h264", codec);
            let off = crate::route::stream_pcm(&ps).and_then(Result::ok);
            assert_eq!(off, None, "{codec}: gate off");
            assert_eq!(audio_declaration(codec, off.as_ref()).map(|d| d.0).filter(|n| *n == "PCM"), None, "{codec}");
        }
        crate::route::begin_attempt_for_test(
            &mut ps, std::sync::Arc::new(plx_platform::devcaps::Caps::assumed().with_app_lossy()),
        );
        for codec in ["mp3", "vorbis"] {
            crate::route::set_stream_codecs(&mut ps, "h264", codec);
            let shape = crate::route::stream_pcm(&ps).and_then(Result::ok);
            assert_eq!(shape, Some(crate::route::PcmFormat::STEREO_48K), "{codec}");
            assert_eq!(audio_declaration(codec, shape.as_ref()), Some(("PCM", 5)), "{codec}");
            let on = build_av_payload_at(&ps, "H264", "PCM", ENVELOPE_FHD60, crate::player::RATE_NORMAL);
            assert_eq!(on.matches("pcmInfo").count(), 1, "{codec}: {on}");
            // declared PCM, so planned the stack's lead like a FLAC Load (`load_audio` is the one value)
            let planned =
                load_audio(codec, crate::player::RATE_NORMAL, shape.as_ref(), None, AudioStack::Liblxa).unwrap();
            assert_eq!((planned.name, planned.lead), ("PCM", PcmLead::FromOutput(AudioStack::Liblxa)), "{codec}");
        }
    }

    /// TrueHD in force (S4-5) is declared as `PCM`, stereo 48 kHz S16, like the other lossy ones.
    #[test]
    fn app_decoded_truehd_is_declared_as_pcm() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        crate::route::set_stream_codecs(&mut ps, "h264", "truehd");
        assert_eq!(crate::route::stream_pcm(&ps).and_then(Result::ok), None, "not in force");
        crate::route::begin_attempt_for_test(
            &mut ps, std::sync::Arc::new(plx_platform::devcaps::Caps::assumed().with_app_lossy()),
        );
        crate::route::set_stream_codecs(&mut ps, "h264", "truehd");
        let shape = crate::route::stream_pcm(&ps).and_then(Result::ok);
        assert_eq!(shape, Some(crate::route::PcmFormat::STEREO_48K));
        assert_eq!(audio_declaration("truehd", shape.as_ref()), Some(("PCM", 5)));
        let on = build_av_payload_at(&ps, "H264", "PCM", ENVELOPE_FHD60, crate::player::RATE_NORMAL);
        assert_eq!(on.matches("pcmInfo").count(), 1, "{on}");
        let planned = load_audio("truehd", crate::player::RATE_NORMAL, shape.as_ref(), None, AudioStack::Liblxa).unwrap();
        assert_eq!((planned.name, planned.lead), ("PCM", PcmLead::FromOutput(AudioStack::Liblxa)), "planned the stack's lead");
    }

    /// **An app-decoded lossy track at a playback speed still loads.** At a speed the speed decoder
    /// declares PCM itself and the app-decoded shape is dropped ([`app_pcm_for_rate`]), so `load_audio`
    /// is asked for a codec the pipeline has no payload name for (`mp3`, `vorbis`, `truehd`): the Load must still
    /// declare PCM and take the stack's lead, as a speed session on any other codec does, not be
    /// refused as an "unsupported audio codec". The `pcm` argument is derived exactly as
    /// `start_bufferfeed_inner` derives it, so this goes through the Load's own path (the speed
    /// assertion of `app_decoded_dts_is_declared_as_pcm_and_native_dts_as_dts` hands `"PCM"` straight
    /// to the payload builder and cannot see this).
    #[test]
    fn an_app_decoded_lossy_track_loads_as_pcm_at_a_speed() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        crate::route::begin_attempt_for_test(
            &mut ps, std::sync::Arc::new(plx_platform::devcaps::Caps::assumed().with_app_lossy()),
        );
        for codec in ["mp3", "vorbis", "truehd"] {
            crate::route::set_stream_codecs(&mut ps, "h264", codec);
            for rate in [500, 1500, 2000] {
                let pcm = app_pcm_for_rate(rate, crate::route::stream_pcm(&ps)).and_then(Result::ok);
                assert_eq!(pcm, None, "{codec} at {rate}: the speed decoder declares the shape");
                let planned = load_audio(codec, rate, pcm.as_ref(), None, AudioStack::Liblxa)
                    .map(|a| (a.name, a.diagnostic, a.lead));
                assert_eq!(
                    planned,
                    Some(("PCM", 5, PcmLead::FromOutput(AudioStack::Liblxa))),
                    "{codec} at {rate}: a speed session declares PCM whatever the codec is called"
                );
            }
        }
    }

    #[test]
    fn only_a_pcm_load_carries_pcm_info() {
        let f = PcmFormat { sample_rate: 48_000, bits: 16, channels: 2, source_rate: 48_000 };
        assert_eq!(with_pcm_info(BASE, "AC3", Some(f)), BASE);
        assert_eq!(with_pcm_info(BASE, "PCM", None), BASE);
    }
}

/// **The webOS 10.3.1 refusal, replayed.** `docs/webos10-lab-report.md` §3.2 records the exact
/// callback order the pipeline produced when it refused the 4K60 H.264 envelope: `13/1`, `14/0`,
/// the `type=5` sink echo, `15/0`, `8/0`, then `18/601 Resource Allocation Error` — with `Load()`
/// itself having returned `ok=1`. §3.5 records what the app did about it: nothing. `load_failed`
/// stayed false, the state machine sat in Connecting, and no failure read-out was ever reached.
///
/// This test is that artifact, host-side, watched RED before the fix (2026-09-03): the red is the
/// lab set's measured sequence, NOT a reproduction of issue #63, whose set nobody here has seen a
/// log from. Under `hostsim` only because it borrows `prime_livelock_tests`' engine constructor.
/// The envelope rule, cell by cell. Every value here is either a measured declaration
/// (`ENVELOPE_UHD60`, `ENVELOPE_FHD60`) or today's behaviour; the test exists so the narrow rule
/// stays narrow — a "smarter" envelope that changes HEVC or the unknown-raster case fails here.
#[cfg(test)]
mod sink_envelope_tests {
    use super::*;

    fn caps(h264: (u32, u32, u32), hevc: (u32, u32, u32)) -> plx_platform::devcaps::Caps {
        let mut c = plx_platform::devcaps::Caps::assumed();
        c.h264_row = h264;
        c.hevc_row = hevc;
        c
    }
    /// The synthesized header is byte-for-byte the CodecPrivate of a real 5.1 Opus MKV track
    /// (`ffprobe -show_entries stream=extradata`, libopus surround layout), and stereo is the
    /// 19-byte family-0 form; a width SMP cannot be told about is refused, not guessed.
    #[test]
    fn opus_head_matches_a_real_surround_track_and_refuses_unknown_widths() {
        let real_51: [u8; 27] = [
            0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 0x01, 0x06, 0x38, 0x01, 0x80, 0xbb,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x04, 0x02, 0x00, 0x04, 0x01, 0x02, 0x03, 0x05,
        ];
        assert_eq!(opus_head(6).unwrap(), real_51);
        let stereo = opus_head(2).unwrap();
        assert_eq!(stereo.len(), 19);
        assert_eq!(&stereo[8..10], &[1, 2]);
        assert_eq!(stereo[18], 0, "mapping family 0");
        for channels in 1..=8 {
            let h = opus_head(channels).unwrap();
            assert_eq!(h.len(), if channels <= 2 { 19 } else { 21 + channels as usize });
        }
        for bad in [0, -1, 9, 255] {
            assert_eq!(opus_head(bad), None);
        }
    }

    /// An OPUS Load declares the codec and carries exactly one `opusInfo` inside `contents`, and
    /// the result is still one valid JSON document.
    #[test]
    fn an_opus_load_carries_one_opus_info_node() {
        assert_eq!(audio_payload_codec("opus"), Some(("OPUS", 6)));
        let ps = crate::route::PlaybackSession::IDLE;
        let p = with_opus_info(&build_av_payload(&ps, "AV1", "OPUS", ENVELOPE_FHD60), 6).unwrap();
        assert_eq!(p.matches(r#""opusInfo""#).count(), 1);
        let v: serde_json::Value = serde_json::from_str(&p).expect("valid JSON");
        let contents = &v["args"][0]["option"]["externalStreamingInfo"]["contents"];
        assert_eq!(contents["codec"]["audio"], "OPUS");
        assert_eq!(contents["opusInfo"]["channels"], 6);
        assert_eq!(contents["opusInfo"]["streamHeader"], plx_base::b64::encode(&opus_head(6).unwrap()));
        assert_eq!(with_opus_info(&p, 0), None);
    }

    /// The speed menu offers only what the Load can declare: on a set whose measured H.264 row is
    /// 4K@30, a 24p stream at 1.5x is fed at 36 fps and the envelope clamps the declaration to 30,
    /// so 1.5x and above are not offered. Without a measured table, for a codec the Load cannot
    /// declare, or for a row with no rate, the flat [`SPEED_MAX_FPS`] ceiling stands.
    #[test]
    fn the_speed_offer_stops_at_the_measured_rows_frame_rate() {
        let row30 = plx_platform::devcaps::Caps {
            h264_row: (3840, 2160, 30),
            ..plx_platform::devcaps::Caps::assumed()
        };
        let offered = |fps: f64, video: Option<LoadVideo>, caps: &plx_platform::devcaps::Caps, measured: bool| {
            crate::player::speeds_within(fps, speed_fps_ceiling(video, caps, measured))
        };
        assert_eq!(
            offered(24.0, Some(LoadVideo::H264), &row30, true),
            vec![500, 750, 1000, 1250],
            "24p at 1.5x is fed at 36 fps, past a 30 fps row"
        );
        assert_eq!(
            offered(29.97, Some(LoadVideo::H264), &row30, true),
            vec![500, 750, 1000],
            "29.97p is within the 30 row at 1x only"
        );
        assert_eq!(
            offered(24.0, Some(LoadVideo::H265), &row30, true),
            crate::player::PLAY_RATES.to_vec(),
            "the H.264 row bounds H.264 only"
        );
        for (what, video, caps, measured) in [
            ("an unmeasured table", Some(LoadVideo::H264), &row30, false),
            ("a codec the Load cannot declare", None, &row30, true),
            (
                "a row with no rate",
                Some(LoadVideo::H264),
                &plx_platform::devcaps::Caps { h264_row: (3840, 2160, 0), ..plx_platform::devcaps::Caps::assumed() },
                true,
            ),
        ] {
            assert_eq!(speed_fps_ceiling(video, caps, measured), crate::player::SPEED_MAX_FPS, "{what}");
        }
        let row120 = plx_platform::devcaps::Caps {
            h264_row: (3840, 2160, 120),
            ..plx_platform::devcaps::Caps::assumed()
        };
        assert_eq!(
            speed_fps_ceiling(Some(LoadVideo::H264), &row120, true),
            crate::player::SPEED_MAX_FPS,
            "a faster row never raises the ceiling"
        );
    }

    #[test]
    fn audio_payload_names_dts_and_refuses_unsupported_formats() {
        assert_eq!(audio_payload_codec("dts"), Some(("DTS", 4)));
        assert_eq!(audio_payload_codec("eac3"), Some(("AC3 PLUS", 2)));
        // PMS's own spelling of DTS, and the other spellings the shared canonicaliser folds.
        assert_eq!(audio_payload_codec("dca"), Some(("DTS", 4)));
        assert_eq!(audio_payload_codec("DCA"), Some(("DTS", 4)));
        assert_eq!(audio_payload_codec("ec-3"), Some(("AC3 PLUS", 2)));
        assert_eq!(audio_payload_codec("flac"), Some(("PCM", 5)));
        assert_eq!(audio_payload_codec("FLAC"), Some(("PCM", 5)));
        // ALAC and every LPCM flavour the bundled FFmpeg decodes are fed as the same PCM
        for codec in ["alac", "ALAC", "pcm", "lpcm", "pcm_s16le", "pcm_s24le", "pcm_s32be"] {
            assert_eq!(audio_payload_codec(codec), Some(("PCM", 5)), "{codec}");
        }
        for codec in ["truehd", "", "unknown", "pcm_f32le", "pcm_bluray", "pcm_u8"] {
            assert_eq!(audio_payload_codec(codec), None);
        }
        let ps = crate::route::PlaybackSession::IDLE;
        let payload = build_av_payload(&ps, "H264", "DTS", ENVELOPE_FHD60);
        assert!(payload.contains(r#""audio":"DTS""#));
    }

    const DEV_SET: ((u32, u32, u32), (u32, u32, u32)) = ((4096, 2304, 60), (4096, 2176, 60));
    const FHD_SET: ((u32, u32, u32), (u32, u32, u32)) = ((1920, 1088, 60), (1920, 1088, 60));

    #[test]
    fn h264_declares_fhd_only_when_the_widest_raster_fits_fhd() {
        let c = caps(DEV_SET.0, DEV_SET.1);
        for (raster, want) in [
            ((720, 480), ENVELOPE_FHD60),
            ((1280, 720), ENVELOPE_FHD60),
            ((1920, 1080), ENVELOPE_FHD60),
            ((1920, 1088), ENVELOPE_FHD60), // coded height of a 1080p stream
            ((1918, 802), ENVELOPE_FHD60),  // scope film
            ((3840, 2160), ENVELOPE_UHD60),
            ((4096, 2176), ENVELOPE_UHD60),
            ((0, 0), ENVELOPE_UHD60),  // nobody said: today's value
            ((1920, 0), ENVELOPE_UHD60),
        ] {
            assert_eq!(sink_envelope(LoadVideo::H264, raster, 0.0, &c, true), want, "h264 {raster:?}");
            assert_eq!(
                sink_envelope(LoadVideo::H264, raster, 0.0, &c, false),
                want,
                "h264 {raster:?} unmeasured"
            );
        }
    }

    #[test]
    fn hevc_keeps_the_verified_4k60_on_a_4k_set_whatever_the_source() {
        let c = caps(DEV_SET.0, DEV_SET.1);
        for raster in [(720, 480), (1920, 1080), (3840, 2160), (0, 0)] {
            assert_eq!(sink_envelope(LoadVideo::H265, raster, 0.0, &c, true), ENVELOPE_UHD60, "{raster:?}");
            assert_eq!(sink_envelope(LoadVideo::H265, raster, 0.0, &c, false), ENVELOPE_UHD60, "{raster:?}");
        }
    }

    #[test]
    fn a_measured_full_hd_table_clamps_both_codecs_and_the_assumed_table_clamps_nothing() {
        let c = caps(FHD_SET.0, FHD_SET.1);
        assert_eq!(sink_envelope(LoadVideo::H265, (3840, 2160), 0.0, &c, true), ENVELOPE_FHD60);
        assert_eq!(sink_envelope(LoadVideo::H264, (3840, 2160), 0.0, &c, true), ENVELOPE_FHD60);
        assert_eq!(sink_envelope(LoadVideo::H265, (0, 0), 0.0, &c, true), ENVELOPE_FHD60);
        // the same rows, not measured: never a clamp
        assert_eq!(sink_envelope(LoadVideo::H265, (3840, 2160), 0.0, &c, false), ENVELOPE_UHD60);
        assert_eq!(sink_envelope(LoadVideo::H265, (0, 0), 0.0, &plx_platform::devcaps::Caps::assumed(), true), ENVELOPE_UHD60);
        // a table stating a lower frame rate clamps it; a higher one never raises it
        let slow = caps((3840, 2160, 30), (3840, 2160, 120));
        assert_eq!(sink_envelope(LoadVideo::H264, (3840, 2160), 0.0, &slow, true).fps, 30);
        assert_eq!(sink_envelope(LoadVideo::H265, (3840, 2160), 0.0, &slow, true).fps, 60);
    }

    /// The device measurement behind the rate rule: 4K H.264 declared at 60 is re-announced at
    /// 30 by the pipeline; at its own class it is not. HEVC and an unknown rate keep 60.
    #[test]
    fn h264_declares_the_streams_rate_class_and_hevc_keeps_60() {
        let c = caps(DEV_SET.0, DEV_SET.1);
        for (fps, want) in [(24.0, 24), (23.976, 24), (25.0, 25), (29.97, 30), (30.0, 30), (50.0, 50), (59.94, 60), (60.0, 60), (120.0, 60), (0.0, 60)] {
            assert_eq!(sink_envelope(LoadVideo::H264, (3840, 2160), fps, &c, true).fps, want, "h264 {fps}");
            assert_eq!(sink_envelope(LoadVideo::H264, (1920, 1080), fps, &c, true).fps, want, "h264 fhd {fps}");
            assert_eq!(sink_envelope(LoadVideo::H265, (3840, 2160), fps, &c, true).fps, 60, "hevc {fps}");
        }
        // the table still clamps a class from above
        let slow = caps((3840, 2160, 25), (3840, 2160, 60));
        assert_eq!(sink_envelope(LoadVideo::H264, (3840, 2160), 30.0, &slow, true).fps, 25);
    }

    /// Issue #665: an MKV's 1 ms timebase stores a 30p frame as 33 ms, so PMS reports 30.303 and
    /// the strict class rounded it up to 50, which a 4K@30 H.264 decoder refuses. A rate within
    /// the timebase rounding error above a class snaps to it; a genuinely higher one still rounds up.
    #[test]
    fn a_millisecond_timebase_rate_snaps_to_its_class() {
        let c = caps(DEV_SET.0, DEV_SET.1);
        for (fps, want) in [
            (30.303, 30), (24.39, 24), (23.81, 24), (25.0, 25), (50.0, 50), (58.82, 60), (59.94, 60),
            (31.0, 50), (26.0, 30),
        ] {
            assert_eq!(sink_envelope(LoadVideo::H264, (3840, 2160), fps, &c, false).fps, want, "h264 {fps}");
        }
    }

    /// Issue #665: the re-encode of a 4K source at 480p declares FHD60 for H.264.
    #[test]
    fn a_480p_ceiling_on_a_4k_source_declares_fhd60_for_h264() {
        let c = caps(DEV_SET.0, DEV_SET.1);
        assert_eq!(sink_envelope(LoadVideo::H264, (854, 480), 0.0, &c, false), ENVELOPE_FHD60);
    }

    #[test]
    fn the_payload_carries_all_three_envelope_numbers() {
        let ps = crate::route::PlaybackSession::IDLE;
        let p = build_av_payload(&ps, "H264", "AAC", ENVELOPE_FHD60);
        assert!(p.contains(r#""maxWidth":1920"#), "{p}");
        assert!(p.contains(r#""maxHeight":1080"#), "{p}");
        assert!(p.contains(r#""maxFrameRate":60"#), "{p}");
        let p = build_av_payload(&ps, "H265", "AC3 PLUS", ENVELOPE_UHD60);
        assert!(p.contains(r#""maxWidth":3840"#) && p.contains(r#""maxHeight":2160"#), "{p}");
        assert!(!p.contains(r#""maxFrameRate":30"#), "the template's 30 must be replaced: {p}");
    }

    /// A playback-speed Load declares PCM with its format, the sample rate in KILOHERTZ (libpf
    /// matches `48`; `48000` silently became 44.1 on the 3.9.3 set). Compressed audio carries no
    /// `pcmInfo` at all, so every playback without a speed is byte-for-byte what it was.
    #[test]
    fn a_pcm_load_declares_its_format_and_compressed_audio_does_not() {
        let ps = crate::route::PlaybackSession::IDLE;
        let p = build_av_payload(&ps, "H264", "PCM", ENVELOPE_FHD60);
        assert!(p.contains(r#""audio":"PCM""#), "{p}");
        assert!(
            p.contains(r#""pcmInfo":{"bitsPerSample":16,"sampleRate":48,"channelMode":"stereo"},"esInfo":"#),
            "{p}"
        );
        assert_eq!(p.matches("pcmInfo").count(), 1);
        for audio in ["AAC", "AC3", "AC3 PLUS", "DTS"] {
            assert!(!build_av_payload(&ps, "H264", audio, ENVELOPE_FHD60).contains("pcmInfo"), "{audio}");
        }
    }

    /// A real session playing 44.1 kHz / 16-bit stereo FLAC, as the route carries it.
    fn flac_session() -> crate::route::PlaybackSession {
        let mut ps = crate::route::PlaybackSession::IDLE;
        assert!(crate::route::set_stream_declaration_for_test(
            &mut ps,
            "h264",
            "flac",
            0.0,
            plx_data::metadata::Dovi::NONE,
            false,
            plx_platform::devcaps::dv::DvCapability::Supported,
        ));
        crate::route::set_carried_audio_for_test(
            &mut ps,
            crate::route::CarriedAudio {
                sid: 7, ordinal: 1, codec: "flac".into(), channels: 2,
                can_normalize_loudness: false, immersive: false, sample_rate: 44_100, bit_depth: 16,
            },
        );
        ps
    }

    /// The Load carries EXACTLY ONE `pcmInfo`. At 1x a FLAC session declares `audio:"PCM"` too
    /// (`audio_payload_codec`), and the speed decoder's fixed kilohertz node used to be inserted
    /// as well, so the Load held two nodes that disagreed on the rate's unit and the bit depth.
    #[test]
    fn a_flac_load_at_1x_carries_only_its_own_pcm_info() {
        let p = build_av_payload_at(&flac_session(), "H264", "PCM", ENVELOPE_FHD60, crate::player::RATE_NORMAL);
        assert_eq!(p.matches("pcmInfo").count(), 1, "{p}");
        assert!(
            p.contains(r#""pcmInfo":{"format":"S16LE","channelMode":"stereo","layout":"interleaved","sampleRate":44.1,"bitsPerSample":16}"#),
            "{p}"
        );
        assert!(!p.contains(r#""sampleRate":48,"#), "the speed decoder's node must be absent: {p}");
        serde_json::from_str::<serde_json::Value>(&p).unwrap();
    }

    /// At a speed the speed decoder owns the PCM, FLAC included: main's fixed node, and only it.
    #[test]
    fn a_flac_load_at_a_speed_carries_the_speed_decoders_pcm_info() {
        for rate in [500, 1500, 2000] {
            let p = build_av_payload_at(&flac_session(), "H264", "PCM", ENVELOPE_FHD60, rate);
            assert_eq!(p.matches("pcmInfo").count(), 1, "{rate}: {p}");
            assert!(
                p.contains(r#""pcmInfo":{"bitsPerSample":16,"sampleRate":48,"channelMode":"stereo"},"esInfo":"#),
                "{rate}: {p}"
            );
            assert!(!p.contains("S16LE"), "{rate}: {p}");
        }
    }

    /// Compressed audio carries no `pcmInfo`, whatever the session's own track is.
    #[test]
    fn compressed_audio_carries_no_pcm_info_even_on_a_flac_session() {
        for rate in [crate::player::RATE_NORMAL, 1500] {
            for audio in ["AAC", "AC3", "AC3 PLUS", "DTS"] {
                let p = build_av_payload_at(&flac_session(), "H264", audio, ENVELOPE_FHD60, rate);
                assert!(!p.contains("pcmInfo"), "{rate} {audio}: {p}");
            }
        }
    }

    /// At a speed the Load declares the rate actually fed, exactly: 23.976 at 1.5x is 36000/1001,
    /// and at 1x every declaration is what it was.
    #[test]
    fn a_speed_declares_the_fed_frame_rate() {
        assert_eq!(super::fed_fps_rational(23.976, 1000), Some((24000, 1001)));
        assert_eq!(super::fed_fps_rational(23.976, 1500), Some((36000, 1001)));
        assert_eq!(super::fed_fps_rational(25.0, 2000), Some((50, 1)));
        assert_eq!(super::fed_fps_rational(29.97, 500), Some((15000, 1001)));
        assert_eq!(super::fed_fps_rational(0.0, 1500), None);
        assert!((super::fed_fps(30.0, 2000) - 60.0).abs() < 1e-9);
    }

    /// The static payloads' declared envelopes, read out of the strings that are actually sent.
    #[test]
    fn the_static_envelopes_are_the_static_payloads_own_numbers() {
        for (video, payload) in [(LoadVideo::H264, PAYLOAD_V), (LoadVideo::H265, PAYLOAD_H265)] {
            let e = static_envelope(video);
            assert!(payload.contains(&format!(r#""maxWidth":{}"#, e.w)), "{video:?}");
            assert!(payload.contains(&format!(r#""maxHeight":{}"#, e.h)), "{video:?}");
            assert!(payload.contains(&format!(r#""maxFrameRate":{}"#, e.fps)), "{video:?}");
        }
    }

    #[test]
    fn the_sinkmax_override_parses_exactly_wxh_at_f() {
        assert_eq!(
            parse_sinkmax("1920x1080@30\n"),
            Some(SinkEnvelope { w: 1920, h: 1080, fps: 30 })
        );
        assert_eq!(parse_sinkmax("1920x1080"), None);
        assert_eq!(parse_sinkmax("0x1080@60"), None);
        assert_eq!(parse_sinkmax("abc"), None);
    }

    #[test]
    fn load_video_maps_exactly_today_plus_av1_and_vp9() {
        for (vcodec, want) in [
            ("hevc", LoadVideo::H265),
            ("h264", LoadVideo::H264),
            ("av1", LoadVideo::Av1),
            ("vp9", LoadVideo::Vp9),
            ("", LoadVideo::H264),
            ("HEVC", LoadVideo::H265),
            ("H264", LoadVideo::H264),
            ("h265", LoadVideo::H265),
        ] {
            assert_eq!(LoadVideo::of_stream(vcodec), Some(want), "{vcodec:?}");
        }
        // an unknown codec is refused, never declared as H.264
        for vcodec in ["mpeg2video", "vc1", "vp8", "theora", "mpeg4", "avc", "x"] {
            assert_eq!(LoadVideo::of_stream(vcodec), None, "{vcodec:?}");
        }
        let all = [LoadVideo::H264, LoadVideo::H265, LoadVideo::Av1, LoadVideo::Vp9];
        assert_eq!(all.map(LoadVideo::payload_name), ["H264", "H265", "AV1", "VP9"]);
        assert_eq!(all.map(LoadVideo::diag_code), [1, 2, 3, 4]);
    }

    #[test]
    fn vp9_envelope_follows_hevc_and_its_own_row() {
        let mut c = caps(DEV_SET.0, DEV_SET.1);
        c.vp9 = true;
        c.vp9_row = (3840, 2160, 30);
        assert_eq!(sink_envelope(LoadVideo::Vp9, (1920, 1080), 23.976, &c, false), ENVELOPE_UHD60);
        assert_eq!(
            sink_envelope(LoadVideo::Vp9, (1920, 1080), 23.976, &c, true),
            SinkEnvelope { w: 3840, h: 2160, fps: 30 }
        );
        c.vp9_row = (0, 0, 0);
        assert_eq!(sink_envelope(LoadVideo::Vp9, (1920, 1080), 23.976, &c, true), ENVELOPE_UHD60);
    }

    #[test]
    fn av1_envelope_follows_hevc_and_its_own_row() {
        let mut c = caps(DEV_SET.0, DEV_SET.1);
        c.av1 = true;
        c.av1_row = (3840, 2160, 30);
        assert_eq!(sink_envelope(LoadVideo::Av1, (1920, 1080), 23.976, &c, false), ENVELOPE_UHD60);
        assert_eq!(
            sink_envelope(LoadVideo::Av1, (1920, 1080), 23.976, &c, true),
            SinkEnvelope { w: 3840, h: 2160, fps: 30 }
        );
        c.av1_row = (0, 0, 0);
        assert_eq!(sink_envelope(LoadVideo::Av1, (1920, 1080), 23.976, &c, true), ENVELOPE_UHD60);
    }

    #[test]
    fn static_envelope_av1_and_vp9_are_uhd60() {
        assert_eq!(static_envelope(LoadVideo::Av1), ENVELOPE_UHD60);
        assert_eq!(static_envelope(LoadVideo::Vp9), ENVELOPE_UHD60);
        assert_eq!(static_envelope(LoadVideo::H265), ENVELOPE_UHD60);
        assert_eq!(static_envelope(LoadVideo::H264), SinkEnvelope { w: 1920, h: 1080, fps: 30 });
    }

    #[test]
    fn the_renamed_static_payloads_declare_their_codec() {
        assert_eq!(PAYLOAD_H265.matches(r#""video":"H265""#).count(), 1);
        for (video, name) in [(LoadVideo::Av1, "AV1"), (LoadVideo::Vp9, "VP9")] {
            let p = renamed_static_payload(video);
            assert!(p.contains(&format!(r#""video":"{name}""#)), "{p}");
            assert!(!p.contains("H265"), "{p}");
            assert!(p.contains(r#""maxWidth":3840"#) && p.contains(r#""maxHeight":2160"#), "{p}");
        }
    }

    #[test]
    fn load_video_refused_only_for_av1_and_vp9_without_their_rows() {
        let assumed = plx_platform::devcaps::Caps::assumed();
        assert!(load_video_refused(LoadVideo::Av1, &assumed));
        assert!(load_video_refused(LoadVideo::Vp9, &assumed));
        assert!(!load_video_refused(LoadVideo::H264, &assumed));
        assert!(!load_video_refused(LoadVideo::H265, &assumed));
        let with_row = plx_platform::devcaps::Caps {
            av1: true,
            vp9: true,
            ..plx_platform::devcaps::Caps::assumed()
        };
        for video in [LoadVideo::H264, LoadVideo::H265, LoadVideo::Av1, LoadVideo::Vp9] {
            assert!(!load_video_refused(video, &with_row), "{video:?}");
        }
        // one row never admits the other codec
        let av1_only = plx_platform::devcaps::Caps { av1: true, ..plx_platform::devcaps::Caps::assumed() };
        assert!(load_video_refused(LoadVideo::Vp9, &av1_only));
    }

    #[test]
    fn load_v_name_is_the_name_the_payload_declares() {
        use crate::player::load_v_name;
        assert_eq!(load_v_name(1), "H264");
        assert_eq!(load_v_name(2), "H265");
        assert_eq!(load_v_name(3), "AV1");
        assert_eq!(load_v_name(4), "VP9");
        assert_eq!(load_v_name(0), "—");
        for video in [LoadVideo::H264, LoadVideo::H265, LoadVideo::Av1, LoadVideo::Vp9] {
            assert_eq!(load_v_name(video.diag_code()), video.payload_name());
        }
    }
}

#[cfg(all(test, feature = "hostsim"))]
mod load_refusal_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// The strings the lab log carries at each callback, sanitised exactly as the report has them.
    const ECHO_SINK: &std::ffi::CStr =
        c"0 video/x-h264 (null) (null) 3840 2160 (null) 60.000000 0 0 0";
    const REFUSAL: &std::ffi::CStr = c"Resource Allocation Error";

    /// The Engine itself needs no cleanup since phase 9 — it lives in the test's own
    /// `PlayerAdapter`, which drops with the test. What is still process-wide is `SHARED` and the
    /// route reducer's projection.
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            SHARED.reset_session();
            // A `Drop` takes no arguments and this test's own session is being dropped beside it;
            // the reducer only needs SOME idle projection to be put back.
            crate::route::reset_player_control_for_test(crate::route::idle_session_for_test());
        }
    }

    #[test]
    fn an_asynchronous_load_refusal_is_a_verdict_not_a_black_screen() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        SHARED.reset_session();
        crate::route::reset_player_control_for_test(&ps);
        let _cleanup = Cleanup;
        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe {
            plx_base::task::MainThread::assume()
        });
        let epoch = SHARED.begin_native_session().expect("native session");
        let mut eng = super::prime_livelock_tests::engine_after_reload();
        eng.native_epoch = epoch;
        pa.install(eng);

        // `Load()` returned ok=1 — nothing here says otherwise — and then the pipeline talked.
        super::super::sf_on_event(epoch, 13, 1, c"".as_ptr());
        super::super::sf_on_event(epoch, 14, 0, c"1".as_ptr());
        super::super::sf_on_event(epoch, 5, 0, ECHO_SINK.as_ptr());
        super::super::sf_on_event(epoch, 15, 0, c"1".as_ptr());
        super::super::sf_on_event(epoch, 8, 0, c"audio/mpeg".as_ptr());
        super::super::sf_on_event(epoch, 18, 601, REFUSAL.as_ptr());

        crate::player::pump::pump(&mut ps, &mut pa, 1_000);

        assert!(
            SHARED.load_failed.load(Ordering::Acquire),
            "type=18 num=601 before any picture must publish load_failed; the lab set sat on a \
             black screen for 70 s because it did not"
        );
        assert_eq!(
            super::super::state(&ps),
            crate::player::PlaybackState::Error,
            "the pump must turn a refused Load into the Error state the read-out renders from"
        );
        assert_eq!(
            super::super::error_now(&ps).kind,
            super::super::FailureKind::TvPipeline,
            "and the verdict must name the television's pipeline, not a generic stop"
        );
    }
}

#[cfg(test)]
mod inplace_layout_tests {
    use super::inplace_layout_known;

    #[test]
    fn in_place_seek_needs_the_cxx11_feed_and_a_release_that_is_unknown_or_4_plus() {
        assert!(inplace_layout_known(4, false));
        assert!(inplace_layout_known(0, false));
        assert!(inplace_layout_known(11, false));
        assert!(!inplace_layout_known(3, false));
        assert!(!inplace_layout_known(3, true));
        assert!(!inplace_layout_known(4, true));
        assert!(!inplace_layout_known(0, true));
    }
}

#[cfg(test)]
mod audio_lead_in_tests {
    use super::{audio_lead_in, AUDIO_LEAD_IN_NS};

    #[test]
    fn the_opening_au_and_the_window_after_it_are_lead_in_and_nothing_else_is() {
        let start = 11_008_000_000;
        assert!(audio_lead_in(start, true, None), "the marked first AU opens the timeline");
        assert!(!audio_lead_in(start, false, None), "an unmarked AU with no start is not");
        assert!(audio_lead_in(start + AUDIO_LEAD_IN_NS - 1, false, Some(start)));
        assert!(!audio_lead_in(start + AUDIO_LEAD_IN_NS, false, Some(start)), "the window ends");
        assert!(!audio_lead_in(start - 1, false, Some(start)), "nothing before the start");
    }
}

/// **A film whose audio starts 11 s after its video must still start.** The device log of the
/// failing WEB-DL (E-AC-3 with an MKV `Delay` of 11.008 s): `SMP Play`, `feed v#1..v#4`, the
/// demuxer's `first audio at 11008ms`, then no `feed a#` line at all and `timeline playing t=0s`
/// for good. Starfish's clock waits for audio, so `pres_fed` stays 0 and the video lane stops
/// [`MAX_FEED_AHEAD_NS`] in; the audio lane dropped every AU as stale because each was more than
/// [`AUDIO_STALE_AHEAD_NS`] past that video. These tests drive the real lanes through
/// [`feed_both_lanes`] against the host feed seam, in the state a progressive start leaves:
/// Play already issued, nothing presented (`pres_fed` 0, not [`PRES_NONE`]).
#[cfg(all(test, feature = "hostsim"))]
mod late_audio_start_tests {
    use super::*;

    const V_STEP_NS: i64 = 41_708_333; // 23.976 fps
    const A_STEP_NS: i64 = 32_000_000; // E-AC-3: 1536 samples at 48 kHz
    const AUDIO_DELAY_NS: i64 = 11_008_000_000;

    /// Process-wide state these tests set, restored on the way out, panic or not.
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::player::ffi_host::force_clocksink_for_test(false);
            SHARED.first_audio_pts.store(i64::MIN, Ordering::Relaxed);
            SHARED.pres_fed.store(0, Ordering::Relaxed);
        }
    }

    /// An engine past `SMP Play` at offset 0 holding the film's first 12 s: video from 0, audio
    /// from [`AUDIO_DELAY_NS`]. `first_audio` is what the demuxer published as its first audio.
    fn started_with_late_audio(first_audio: i64) -> Engine {
        let mut eng = super::prime_livelock_tests::engine_after_reload();
        eng.prime_play = false;
        let au = [0u8; 64];
        let qv = &mut **eng.aq_video.as_mut().unwrap() as *mut crate::aq::AuQueue;
        for i in 0..(12_000_000_000 / V_STEP_NS) {
            crate::aq::aq_push(qv, au.as_ptr(), au.len() as c_int, i * V_STEP_NS, c_int::from(i == 0), 1);
        }
        let qa = &mut **eng.aq_audio.as_mut().unwrap() as *mut crate::aq::AuQueue;
        for i in 0..200 {
            crate::aq::aq_push(qa, au.as_ptr(), au.len() as c_int, AUDIO_DELAY_NS + i * A_STEP_NS, 1, 2);
        }
        SHARED.first_audio_pts.store(first_audio, Ordering::Relaxed);
        eng
    }

    fn run_ticks(eng: &mut Engine) {
        let mt = unsafe { plx_base::task::MainThread::assume() };
        for _ in 0..8 {
            feed_both_lanes(&mt, eng);
        }
    }

    fn arm() -> Restore {
        crate::player::ffi_host::force_clocksink_for_test(true);
        SHARED.pres_fed.store(0, Ordering::Relaxed);
        SHARED.seek_to_ns.store(-1, Ordering::Relaxed);
        Restore
    }

    #[test]
    fn a_track_whose_audio_starts_late_reaches_the_sink() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        let mut eng = started_with_late_audio(AUDIO_DELAY_NS);
        run_ticks(&mut eng);

        // Precondition: the video lane fed and then stopped at the throttle, as on the device.
        assert!(
            eng.max_fed_video_pts > 0 && eng.max_fed_video_pts < AUDIO_DELAY_NS - AUDIO_STALE_AHEAD_NS,
            "PRECONDITION FAILED, not the defect: video fed to {}ns — the host sink is not armed \
             or the throttle did not hold the video lane short of the audio",
            eng.max_fed_video_pts
        );
        assert_eq!(eng.audio_start_fp, Some(AUDIO_DELAY_NS), "the first audio AU was fed");
        assert!(
            eng.max_fed_audio_pts >= AUDIO_DELAY_NS + AUDIO_LEAD_IN_NS - A_STEP_NS,
            "the lead-in was fed ahead of the clock (audio to {}ns)",
            eng.max_fed_audio_pts
        );
        // Past the lead-in the throttle holds the audio — held, not dropped as stale.
        let next = eng.pending_audio.as_ref().map(|n| unsafe { crate::aq::au_fields(n.0) }.2);
        assert_eq!(next, Some(eng.max_fed_audio_pts + A_STEP_NS), "the next AU waits for the clock");
    }

    #[test]
    fn audio_far_ahead_that_does_not_open_the_timeline_is_still_dropped_as_stale() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        // The demuxer has not marked these AUs: stale pre-seek audio looks exactly like this.
        let mut eng = started_with_late_audio(i64::MIN);
        run_ticks(&mut eng);

        assert!(eng.max_fed_video_pts > 0, "PRECONDITION FAILED: the host sink fed no video");
        assert_eq!(eng.max_fed_audio_pts, 0, "stale audio must not reach the sink");
        assert_eq!(eng.audio_start_fp, None);
    }
}

/// **Issue #568: on webOS 5+ (`VP_EXPORTED`) every direct-play seek was a full fresh Starfish
/// `Load` (black screen, the DV/HDR badge re-popping).** `scrub: tap commit 905s` was followed by
/// `reload_at: fresh Load at 905s`, because the Kodi in-place path reaches LG-private objects and
/// was switched off there. The seek now goes through LG's public `StarfishMediaAPIs::Seek`.
///
/// These drive the real mode selection, the pump's reload-or-in-place decision and the real
/// [`feed_stream`] re-anchor against the host sink with the clock sink armed, which reports
/// `VP_EXPORTED` exactly as a webOS 5+ set does.
#[cfg(all(test, feature = "hostsim"))]
mod native_seek_tests {
    use super::*;
    use crate::player::ffi_host;

    const V_STEP_NS: i64 = 41_708_333;
    const TARGET_NS: i64 = 905_000_000_000;

    /// Process-wide state these tests set, restored on the way out, panic or not.
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            ffi_host::force_clocksink_for_test(false);
            ffi_host::reset_seek_probe_for_test(None);
            crate::player::set_seek_mode(SeekMode::Segment);
            SHARED.pts_shift.store(0, Ordering::Relaxed);
            SHARED.seek_target_ns.store(-1, Ordering::Relaxed);
            SHARED.pres_fed.store(0, Ordering::Relaxed);
            TX.seek_to_ns.store(-1, Ordering::Relaxed);
            TX.seek_reqs.store(0, Ordering::Relaxed);
        }
    }

    fn arm(force_seek: Option<c_int>) -> Restore {
        ffi_host::force_clocksink_for_test(true);
        ffi_host::reset_seek_probe_for_test(force_seek);
        SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        TX.seek_to_ns.store(-1, Ordering::Relaxed);
        TX.seek_reqs.store(0, Ordering::Relaxed);
        Restore
    }

    /// An engine that has applied a direct-play seek and is waiting for the keyframe: flushed,
    /// rebase pending, the video queue holding a keyframe at `TARGET_NS` and the AUs after it.
    fn engine_seeking() -> Engine {
        let mut eng = super::prime_livelock_tests::engine_after_reload();
        eng.flushed = true;
        eng.rebase_pending = true;
        SHARED.seek_target_ns.store(TARGET_NS, Ordering::Relaxed);
        let au = [0u8; 64];
        let qv = &mut **eng.aq_video.as_mut().unwrap() as *mut crate::aq::AuQueue;
        for i in 0..60 {
            crate::aq::aq_push(qv, au.as_ptr(), au.len() as c_int, TARGET_NS + i * V_STEP_NS, c_int::from(i == 0), 1);
        }
        eng
    }

    fn tick(eng: &mut Engine) {
        let mt = unsafe { plx_base::task::MainThread::assume() };
        feed_both_lanes(&mt, eng);
    }

    /// The mode a set on release `major` (old-ABI Feed when `cow`) gets from a window of kind
    /// `window_mode`, composed from the same facts `start_bufferfeed_inner` composes.
    fn mode_for(major: u32, cow: bool, window_mode: c_int) -> SeekMode {
        select_seek_mode(inplace_layout_known(major, cow), old_abi_seek_tested(major), window_mode, None)
    }

    #[test]
    fn a_webos5_session_seeks_natively_and_never_reloads() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm(None);
        let sink_mode = sink().window_mode();
        assert_eq!(sink_mode, crate::player::VP_EXPORTED, "PRECONDITION: the armed host sink is the webOS 5+ one");
        let mode = mode_for(5, false, sink_mode);
        assert_eq!(mode, SeekMode::NativeSeek, "VP_EXPORTED selects LG's public Seek");
        assert!(
            !crate::player::pump::seek_reloads(mode),
            "a mid-play direct-play seek on VP_EXPORTED must NOT be a fresh Load (#568)"
        );
    }

    #[test]
    fn each_firmware_gets_its_seek_mode() {
        use crate::player::{VP_ACB, VP_EXPORTED, VP_NONE};
        assert_eq!(mode_for(4, false, VP_ACB), SeekMode::Segment, "webOS 4.x is unchanged");
        assert_eq!(mode_for(5, false, VP_EXPORTED), SeekMode::NativeSeek, "webOS 5+ is unchanged");
        assert_eq!(mode_for(3, true, VP_ACB), SeekMode::NativeSeek, "webOS 3 (a 3.9.3 set: COW Feed) uses LG's public Seek");
        assert_eq!(mode_for(2, true, VP_ACB), SeekMode::Reload, "webOS 2 is untested and reloads, as before");
        assert_eq!(mode_for(4, true, VP_ACB), SeekMode::Reload, "an old-ABI 4.x build is untested and reloads");
        assert_eq!(mode_for(0, true, VP_ACB), SeekMode::Reload, "an unknown release with the old ABI reloads");
        assert_eq!(mode_for(3, true, VP_NONE), SeekMode::Reload, "no video plane, nothing to seek in place");
        assert_eq!(select_seek_mode(true, false, VP_ACB, Some(SeekMode::NativeSeek)), SeekMode::NativeSeek, "the dev trigger wins");
        assert!(crate::player::pump::seek_reloads(SeekMode::Reload));
        assert!(!crate::player::pump::seek_reloads(SeekMode::Segment));
        assert!(!crate::player::pump::seek_reloads(SeekMode::NativeSeek));
    }

    /// A webOS 3 playback whose `Seek` was refused stays on the reload path across the reload's
    /// fresh session, as a webOS 5+ one does (`pump`'s refused-Seek test); a fresh play clears it.
    #[test]
    fn a_refused_seek_on_webos3_keeps_the_playback_on_reload() {
        let _serial = plx_base::testlock::serial();
        let webos3 = || session_seek_mode(inplace_layout_known(3, true), old_abi_seek_tested(3), crate::player::VP_ACB, None);
        crate::player::set_native_seek_refused(true);
        assert_eq!(webos3(), SeekMode::Reload, "the downgrade survives reload_at");
        crate::player::set_native_seek_refused(false);
        assert_eq!(webos3(), SeekMode::NativeSeek, "a fresh play offers Seek again");
    }

    /// A speed is applied only to a freshly loaded pipeline, so a seek that carries a speed
    /// change, and every seek while the speed is not 1x, reloads in EVERY mode; at 1x with no
    /// change pending each mode keeps its own answer.
    #[test]
    fn a_speed_makes_every_direct_play_seek_a_reload() {
        use crate::player::pump::seek_must_reload;
        let normal = crate::player::RATE_NORMAL;
        for mode in [SeekMode::Reload, SeekMode::Segment, SeekMode::NativeSeek] {
            assert!(seek_must_reload(mode, 1500, false), "{mode:?} at 1.5x");
            assert!(seek_must_reload(mode, 500, false), "{mode:?} at 0.5x");
            assert!(seek_must_reload(mode, normal, true), "{mode:?}: the change back to 1x reloads too");
            assert_eq!(seek_must_reload(mode, normal, false), crate::player::pump::seek_reloads(mode), "{mode:?} at 1x");
        }
    }

    #[test]
    fn the_landed_keyframe_is_the_native_seek_and_nothing_flushes() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm(None);
        crate::player::set_seek_mode(SeekMode::NativeSeek);
        let mut eng = engine_seeking();
        tick(&mut eng);

        assert_eq!(ffi_host::seek_log_for_test(), vec![TARGET_NS / 1_000_000], "Seek(<keyframe ms>), once");
        assert_eq!(ffi_host::flush_calls_for_test(), 0, "Seek flushes itself; a flush after it re-anchors at 0");
        assert!(!eng.flushed && !eng.rebase_pending, "the seek is applied");
        assert_eq!(SHARED.pts_shift.load(Ordering::Relaxed), 0, "content time, no rebase");
        assert!(eng.max_fed_video_pts >= TARGET_NS, "the keyframe and the AUs after it were fed on the Seek's timeline");
        assert_eq!(crate::player::seek_mode(), SeekMode::NativeSeek);
    }

    #[test]
    fn a_pre_zero_keyframe_seeks_to_zero_not_negative() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm(None);
        crate::player::set_seek_mode(SeekMode::NativeSeek);
        let mut eng = super::prime_livelock_tests::engine_after_reload();
        eng.flushed = true;
        eng.rebase_pending = true;
        SHARED.seek_target_ns.store(0, Ordering::Relaxed);
        let au = [0u8; 64];
        let qv = &mut **eng.aq_video.as_mut().unwrap() as *mut crate::aq::AuQueue;
        crate::aq::aq_push(qv, au.as_ptr(), au.len() as c_int, -80_000_000, 1, 1);
        tick(&mut eng);
        assert_eq!(ffi_host::seek_log_for_test(), vec![0], "Seek refuses a negative millis");
    }

    #[test]
    fn a_refused_seek_reloads_this_one_and_every_later_one() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm(Some(0));
        crate::player::set_seek_mode(SeekMode::NativeSeek);
        let mut eng = engine_seeking();
        tick(&mut eng);

        assert_eq!(crate::player::seek_mode(), SeekMode::Reload, "native seek is off for the rest of the session");
        assert!(crate::player::pump::seek_reloads(crate::player::seek_mode()));
        assert_eq!(
            TX.seek_to_ns.load(Ordering::Acquire),
            TARGET_NS,
            "this seek is re-posted at the user's target for the pump's reload"
        );
        assert_eq!(eng.max_fed_video_pts, 0, "nothing is fed into a pipeline that refused the seek");
        assert!(eng.pending_video.is_none());
        assert_eq!(ffi_host::flush_calls_for_test(), 0);
    }
}

/// **App-decoded (PCM) audio must reach the sink later than the picture it belongs with** (#1086,
/// #1088): the set renders a PCM Load through a mixer channel that lacks the lip sync its own
/// decoder gets (400 ms, less the active output's own delay: [`PCM_SINK_LEAD_NS`]). The correction
/// is the audio lane's sink pts and nothing else.
#[cfg(test)]
mod pcm_sink_lead_pure_tests {
    use super::*;

    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            SHARED.applied_rate_permille.store(crate::player::RATE_NORMAL, Ordering::Relaxed);
        }
    }

    #[test]
    fn no_lead_is_the_pipeline_timeline_and_a_lead_is_real_time_after_the_speed_division() {
        let _serial = plx_base::testlock::serial();
        let _restore = Restore;
        SHARED.applied_rate_permille.store(crate::player::RATE_NORMAL, Ordering::Relaxed);
        assert_eq!(audio_sink_pts(7_000_000_000, 0), crate::player::to_sink_ns(7_000_000_000));
        assert_eq!(audio_sink_pts(7_000_000_000, PCM_SINK_LEAD_NS), 7_000_000_000 + PCM_SINK_LEAD_NS);

        SHARED.applied_rate_permille.store(1500, Ordering::Relaxed);
        assert_eq!(audio_sink_pts(3_000_000_000, 0), crate::player::to_sink_ns(3_000_000_000));
        assert_eq!(
            audio_sink_pts(3_000_000_000, 400_000_000),
            2_000_000_000 + 400_000_000,
            "film time is divided by the speed, the set's 400 ms is not"
        );
        SHARED.applied_rate_permille.store(500, Ordering::Relaxed);
        assert_eq!(audio_sink_pts(3_000_000_000, 400_000_000), 6_000_000_000 + 400_000_000);
    }

    #[test]
    fn a_sink_pts_never_goes_below_zero_or_overflows() {
        let _serial = plx_base::testlock::serial();
        let _restore = Restore;
        SHARED.applied_rate_permille.store(crate::player::RATE_NORMAL, Ordering::Relaxed);
        assert_eq!(audio_sink_pts(0, 0), 0);
        assert_eq!(audio_sink_pts(-500_000_000, 400_000_000), 0, "a unit before the origin is not sent negative");
        assert_eq!(audio_sink_pts(100_000_000, -400_000_000), 0, "a negative dev override clamps too");
        assert_eq!(audio_sink_pts(i64::MAX - 1, PCM_SINK_LEAD_NS), i64::MAX);
    }

    /// The stacks with no lead at all: 2016/2017's `adecsink`, Realtek and MStar (`Other`), and a set
    /// nobody probed.
    const NO_LEAD_STACKS: [AudioStack; 3] = [AudioStack::AdecSink, AudioStack::Other, AudioStack::Unknown];
    /// The stacks whose lead follows the sound output (so it is read on the Load thread).
    const OUTPUT_STACKS: [AudioStack; 2] = [AudioStack::Liblxa, AudioStack::AudioSinkNew];
    const ALL_STACKS: [AudioStack; 6] = [
        AudioStack::Liblxa,
        AudioStack::LgAudioSink,
        AudioStack::AudioSinkNew,
        AudioStack::AdecSink,
        AudioStack::Other,
        AudioStack::Unknown,
    ];

    #[test]
    fn only_a_pcm_load_gets_a_lead_and_the_dev_override_moves_only_that() {
        let liblxa = AudioStack::Liblxa;
        assert_eq!(PCM_SINK_LEAD_NS, 400_000_000);
        assert_eq!(pcm_lead_plan("PCM", None, liblxa), PcmLead::FromOutput(liblxa));
        assert_eq!(pcm_lead_plan("PCM", Some("0"), liblxa), PcmLead::Override(0, liblxa));
        assert_eq!(pcm_lead_plan("PCM", Some(" 200\n"), liblxa), PcmLead::Override(200_000_000, liblxa));
        assert_eq!(pcm_lead_plan("PCM", Some("-50"), liblxa), PcmLead::Override(-50_000_000, liblxa), "an experiment may go the other way");
        assert_eq!(pcm_lead_plan("PCM", Some("loud"), liblxa), PcmLead::FromOutput(liblxa), "garbage is no override: the output decides");
        assert_eq!(pcm_lead_plan("PCM", Some(""), liblxa), PcmLead::FromOutput(liblxa));
        let new2018 = AudioStack::AudioSinkNew;
        assert_eq!(pcm_lead_plan("PCM", None, new2018), PcmLead::FromOutput(new2018), "2018 follows the output and carries its own stack");
        assert_eq!(pcm_lead_plan("PCM", Some("150"), new2018), PcmLead::Override(150_000_000, new2018));
        assert_eq!(pcm_lead_plan("PCM", Some("loud"), new2018), PcmLead::FromOutput(new2018));
        let adec = AudioStack::AdecSink;
        assert_eq!(pcm_lead_plan("PCM", None, adec), PcmLead::NoLead(adec), "2016/2017: no lead");
        assert_eq!(pcm_lead_plan("PCM", Some("300"), adec), PcmLead::Override(300_000_000, adec), "pcmavoffset is how it gets measured");
        let sink = AudioStack::LgAudioSink;
        assert_eq!(pcm_lead_plan("PCM", None, sink), PcmLead::Fixed(sink), "the output does not change the lead there: no read");
        assert_eq!(pcm_lead_plan("PCM", Some("0"), sink), PcmLead::Override(0, sink), "the override wins on every stack");
        assert_eq!(pcm_lead_plan("PCM", Some("loud"), sink), PcmLead::Fixed(sink));
        for stack in ALL_STACKS {
            for compressed in ["DTS", "AC3", "AC3 PLUS", "AAC", "OPUS", "-"] {
                assert_eq!(pcm_lead_plan(compressed, None, stack), PcmLead::Off, "{compressed} on {stack:?}");
                assert_eq!(pcm_lead_plan(compressed, Some("400"), stack), PcmLead::Off, "{compressed} on {stack:?}: the override is PCM-only");
            }
        }
    }

    /// The table, one cell per stack x output: the filmed outputs of the `liblxa` stack, one nobody
    /// filmed, no answer; 2018's the same figures, none of them measured; every output of the LG audio
    /// sink (one lead); every output of a stack with no lead.
    #[test]
    fn the_lead_follows_the_stack_and_then_the_sound_output() {
        let liblxa = AudioStack::Liblxa;
        assert_eq!(pcm_output_lead(liblxa, Some("tv_speaker")), (400_000_000, true), "filmed: -0.03 to +0.08 s");
        assert_eq!(pcm_output_lead(liblxa, Some("bt_soundbar")), (250_000_000, true), "filmed: +0.017 s at 250 ms, +0.13 to +0.19 s at 400");
        for unmeasured in ["external_optical", "external_arc", "headphone", "lg_sound_sync_optical", "other"] {
            assert_eq!(pcm_output_lead(liblxa, Some(unmeasured)), (PCM_SINK_LEAD_NS, false), "liblxa + {unmeasured}");
        }
        assert_eq!(pcm_output_lead(liblxa, None), (PCM_SINK_LEAD_NS, false), "no answer keeps the liblxa default");

        let y2018 = AudioStack::AudioSinkNew;
        assert_eq!(pcm_output_lead(y2018, Some("tv_speaker")), (400_000_000, false), "2018: 475 less the +75 bias, read from firmware");
        assert_eq!(pcm_output_lead(y2018, Some("bt_soundbar")), (250_000_000, false), "2018: the decoder's -150 for a wireless output, decompile only");
        for unmeasured in ["external_optical", "external_arc", "headphone", "lg_sound_sync_optical", "other"] {
            assert_eq!(pcm_output_lead(y2018, Some(unmeasured)), (PCM_SINK_LEAD_NS, false), "2018 + {unmeasured}");
        }
        assert_eq!(pcm_output_lead(y2018, None), (PCM_SINK_LEAD_NS, false), "2018 + no answer");

        for output in [Some("tv_speaker"), Some("bt_soundbar"), Some("external_arc"), None] {
            assert_eq!(
                pcm_output_lead(AudioStack::LgAudioSink, output),
                (PCM_SINK_LEAD_NS, false),
                "lgaudiosink + {output:?}: 400 ms on every output, Bluetooth included, never measured"
            );
        }
        for stack in NO_LEAD_STACKS {
            for output in [Some("tv_speaker"), Some("bt_soundbar"), Some("external_arc"), None] {
                assert_eq!(pcm_output_lead(stack, output), (0, false), "{stack:?} + {output:?}: the behaviour before #1087");
            }
        }
        assert_eq!(PCM_LEAD_BY_OUTPUT.iter().filter(|(name, _)| *name == "tv_speaker").count(), 1);
    }

    /// Resolution is where the output is read, and the only place: the read is a closure, so what
    /// asks for it is countable.
    #[test]
    fn the_output_is_read_only_for_a_load_that_needs_it() {
        let reads = std::cell::Cell::new(0);
        let read = |answer: Option<&str>| {
            let reads = &reads;
            let answer = answer.map(str::to_string);
            move || {
                reads.set(reads.get() + 1);
                answer
            }
        };
        let off = PcmLead::Off.resolve(read(Some("bt_soundbar")));
        assert_eq!((off.ns, off.output), (0, None));
        assert_eq!(reads.get(), 0, "a compressed Load never asks");

        for stack in ALL_STACKS {
            let overridden = PcmLead::Override(330_000_000, stack).resolve(read(Some("bt_soundbar")));
            assert_eq!((overridden.ns, overridden.output), (330_000_000, None), "{stack:?}: the override wins on any stack");
        }
        assert_eq!(reads.get(), 0, "the dev override wins over the table without asking");

        for stack in NO_LEAD_STACKS {
            let unmeasured = PcmLead::NoLead(stack).resolve(read(Some("bt_soundbar")));
            assert_eq!((unmeasured.ns, unmeasured.output), (0, None), "{stack:?}");
        }
        assert_eq!(reads.get(), 0, "a stack with no lead does not pay for the bus read");

        let sink = PcmLead::Fixed(AudioStack::LgAudioSink).resolve(|| panic!("the LG audio sink's lead does not depend on the output"));
        assert_eq!((sink.ns, sink.output), (PCM_SINK_LEAD_NS, None));

        for stack in OUTPUT_STACKS {
            let before = reads.get();
            let bt = PcmLead::FromOutput(stack).resolve(read(Some("bt_soundbar")));
            assert_eq!((bt.ns, bt.output.as_deref()), (250_000_000, Some("bt_soundbar")), "{stack:?}");
            assert_eq!(reads.get(), before + 1, "a PCM Load on {stack:?} asks once");
            let speaker = PcmLead::FromOutput(stack).resolve(read(Some("tv_speaker")));
            assert_eq!(speaker.ns, 400_000_000, "{stack:?}");
            let optical = PcmLead::FromOutput(stack).resolve(read(Some("external_optical")));
            assert_eq!((optical.ns, optical.output.as_deref()), (PCM_SINK_LEAD_NS, Some("external_optical")), "{stack:?}");
            let none = PcmLead::FromOutput(stack).resolve(read(None));
            assert_eq!((none.ns, none.output), (PCM_SINK_LEAD_NS, None), "{stack:?}");
            assert_eq!(reads.get(), before + 4, "{stack:?}");
        }
    }

    /// The same, from the declaration: the plan a PCM Load gets on each stack, resolved with a read
    /// that counts, asks only on a stack whose lead follows the output (`liblxa`, 2018); the LG audio
    /// sink settles 400 ms without asking, and the stacks with no lead never ask.
    #[test]
    fn a_pcm_load_asks_for_the_output_only_on_a_stack_whose_lead_follows_it() {
        for stack in ALL_STACKS {
            let reads = std::cell::Cell::new(0);
            let lead = load_audio("flac", crate::player::RATE_NORMAL, None, None, stack).unwrap().lead;
            let resolved = lead.resolve(|| {
                reads.set(reads.get() + 1);
                Some("bt_soundbar".to_string())
            });
            if OUTPUT_STACKS.contains(&stack) {
                assert_eq!((reads.get(), resolved.ns), (1, 250_000_000), "{stack:?}");
            } else if stack == AudioStack::LgAudioSink {
                assert_eq!((reads.get(), resolved.ns), (0, PCM_SINK_LEAD_NS), "{stack:?}");
            } else {
                assert_eq!((reads.get(), resolved.ns), (0, 0), "{stack:?}");
            }
        }
    }

    #[test]
    fn a_resolved_lead_says_what_it_decided_and_whether_it_was_measured() {
        let line = |plan: PcmLead, answer: Option<&str>| {
            plan.resolve(|| answer.map(str::to_string)).log_line()
        };
        let said = |text: &str| Some(text.to_string());
        let liblxa = AudioStack::Liblxa;
        assert_eq!(line(PcmLead::FromOutput(liblxa), Some("tv_speaker")), said("pcm: audio sink pts +400ms (stack liblxa, output tv_speaker, default 400ms)"));
        assert_eq!(line(PcmLead::FromOutput(liblxa), Some("bt_soundbar")), said("pcm: audio sink pts +250ms (stack liblxa, output bt_soundbar, default 400ms)"));
        assert_eq!(line(PcmLead::FromOutput(liblxa), Some("external_arc")), said("pcm: audio sink pts +400ms (stack liblxa, output external_arc, default 400ms, not measured)"));
        assert_eq!(line(PcmLead::FromOutput(liblxa), None), said("pcm: audio sink pts +400ms (stack liblxa, output unknown, default 400ms, not measured)"));
        let y2018 = PcmLead::FromOutput(AudioStack::AudioSinkNew);
        assert_eq!(line(y2018, Some("tv_speaker")), said("pcm: audio sink pts +400ms (stack audiosink_new, output tv_speaker, default 400ms, not measured)"));
        assert_eq!(line(y2018, Some("bt_soundbar")), said("pcm: audio sink pts +250ms (stack audiosink_new, output bt_soundbar, default 400ms, not measured)"));
        assert_eq!(line(y2018, None), said("pcm: audio sink pts +400ms (stack audiosink_new, output unknown, default 400ms, not measured)"));
        assert_eq!(line(PcmLead::NoLead(AudioStack::AdecSink), None), said("pcm: audio sink pts +0ms (stack adecsink, not measured on this chip)"));
        assert_eq!(line(PcmLead::Override(330_000_000, AudioStack::AudioSinkNew), None), said("pcm: audio sink pts +330ms (pcmavoffset override, stack audiosink_new)"));
        assert_eq!(
            line(PcmLead::Fixed(AudioStack::LgAudioSink), None),
            said("pcm: audio sink pts +400ms (stack lgaudiosink, every output, not measured)")
        );
        assert_eq!(line(PcmLead::NoLead(AudioStack::Other), None), said("pcm: audio sink pts +0ms (stack other, not measured on this chip)"));
        assert_eq!(line(PcmLead::NoLead(AudioStack::Unknown), None), said("pcm: audio sink pts +0ms (stack unknown, not measured on this chip)"));
        assert_eq!(line(PcmLead::Override(330_000_000, AudioStack::Other), None), said("pcm: audio sink pts +330ms (pcmavoffset override, stack other)"));
        assert_eq!(line(PcmLead::Override(330_000_000, AudioStack::Liblxa), None), said("pcm: audio sink pts +330ms (pcmavoffset override, stack liblxa)"));
        assert_eq!(line(PcmLead::Off, None), None, "a compressed Load says nothing");
        assert_eq!(
            PcmLead::Override(330_000_000, AudioStack::Other).start_log_line(),
            said("pcm: audio sink pts +330ms (pcmavoffset override, stack other)")
        );
        assert_eq!(
            PcmLead::NoLead(AudioStack::Other).start_log_line(),
            said("pcm: audio sink pts +0ms (stack other, not measured on this chip)"),
            "the frame thread settles it, so it says so now"
        );
        assert_eq!(
            PcmLead::Fixed(AudioStack::LgAudioSink).start_log_line(),
            said("pcm: audio sink pts +400ms (stack lgaudiosink, every output, not measured)"),
            "settled on the frame thread, so it says so now, once"
        );
        assert_eq!(
            PcmLead::NoLead(AudioStack::AdecSink).start_log_line(),
            said("pcm: audio sink pts +0ms (stack adecsink, not measured on this chip)")
        );
        for stack in OUTPUT_STACKS {
            assert_eq!(PcmLead::FromOutput(stack).start_log_line(), None, "{stack:?}: the Load thread logs it, with the output it read");
        }
        assert_eq!(PcmLead::Off.start_log_line(), None);
    }

    /// The Load's declared audio and the plan for its lead come out of ONE value ([`load_audio`], the
    /// pair `start_bufferfeed_inner` puts in the payload and on the Engine), speed override included:
    /// a 1.5x session on an AC3 track declares PCM and takes the lead, a 1x AC3 session does not, a
    /// 1x FLAC session does. Only the shape of the plan follows the stack: a lead that follows the
    /// output, the LG audio sink's one lead, or no lead at all.
    #[test]
    fn the_engines_lead_follows_the_declared_audio_the_speed_override_and_the_stack() {
        use crate::player::RATE_NORMAL;
        let plan = |codec: &str, rate: i32| load_audio(codec, rate, None, None, AudioStack::Liblxa).map(|a| (a.name, a.lead));
        let from_output = PcmLead::FromOutput(AudioStack::Liblxa);
        assert_eq!(plan("flac", RATE_NORMAL), Some(("PCM", from_output)));
        assert_eq!(plan("alac", RATE_NORMAL), Some(("PCM", from_output)));
        assert_eq!(plan("ac3", RATE_NORMAL), Some(("AC3", PcmLead::Off)));
        assert_eq!(plan("dca", RATE_NORMAL), Some(("DTS", PcmLead::Off)), "the set's own DTS decoder keeps its own compensation");
        assert_eq!(plan("eac3", RATE_NORMAL), Some(("AC3 PLUS", PcmLead::Off)));
        assert_eq!(plan("ac3", 1500), Some(("PCM", from_output)), "a speed session declares PCM, so it takes the lead");
        assert_eq!(plan("dca", 500), Some(("PCM", from_output)));
        assert_eq!(plan("flac", 1500), Some(("PCM", from_output)));
        assert_eq!(plan("not-a-codec", RATE_NORMAL), None, "an undeclarable codec still refuses the Load");
        // At a speed the codec's name is not declared at all, so it cannot refuse: the guard is upstream,
        // at every Load (`player::session_rate_for` -> `speed_offer` -> `ff::speed_decoder_available`).
        assert_eq!(plan("not-a-codec", 1500), Some(("PCM", from_output)), "a speed session declares PCM whatever the codec is called");
        let sink = AudioStack::LgAudioSink;
        let on_sink = |codec: &str, rate: i32| load_audio(codec, rate, None, None, sink).map(|a| (a.name, a.lead));
        assert_eq!(on_sink("flac", RATE_NORMAL), Some(("PCM", PcmLead::Fixed(sink))));
        assert_eq!(on_sink("ac3", 1500), Some(("PCM", PcmLead::Fixed(sink))), "a speed session declares PCM on this stack too");
        assert_eq!(on_sink("ac3", RATE_NORMAL), Some(("AC3", PcmLead::Off)), "a compressed Load keeps the set's own decoder");
        let y2018 = AudioStack::AudioSinkNew;
        let on_2018 = |codec: &str, rate: i32| load_audio(codec, rate, None, None, y2018).map(|a| (a.name, a.lead));
        assert_eq!(on_2018("flac", RATE_NORMAL), Some(("PCM", PcmLead::FromOutput(y2018))));
        assert_eq!(on_2018("ac3", 1500), Some(("PCM", PcmLead::FromOutput(y2018))));
        assert_eq!(on_2018("ac3", RATE_NORMAL), Some(("AC3", PcmLead::Off)));
        for stack in NO_LEAD_STACKS {
            let on = |codec: &str, rate: i32| load_audio(codec, rate, None, None, stack).map(|a| (a.name, a.lead));
            assert_eq!(on("flac", RATE_NORMAL), Some(("PCM", PcmLead::NoLead(stack))), "{stack:?}");
            assert_eq!(on("ac3", 1500), Some(("PCM", PcmLead::NoLead(stack))), "{stack:?}");
            assert_eq!(on("ac3", RATE_NORMAL), Some(("AC3", PcmLead::Off)), "{stack:?}");
        }
    }

    /// **App-decoded DTS** (S4, the lossy gate on) declares `"audio":"PCM"` and so takes the lead of the
    /// set's stack exactly like FLAC; the same track left to the set's own decoder (`a="DTS"`) gets none.
    /// `load_audio` is given the shape the session declared (`route::stream_pcm`), so the name the
    /// payload carries and the lead plan cannot disagree.
    #[test]
    fn app_decoded_dts_takes_the_stacks_lead_and_native_dts_none() {
        use crate::player::RATE_NORMAL;
        let shape = crate::route::PcmFormat::STEREO_48K;
        for stack in ALL_STACKS {
            let app = load_audio("dts", RATE_NORMAL, Some(&shape), None, stack).unwrap();
            let like_flac = load_audio("flac", RATE_NORMAL, Some(&shape), None, stack).unwrap();
            assert_eq!((app.name, app.diagnostic), ("PCM", 5), "{stack:?}");
            assert_eq!(app.lead, like_flac.lead, "{stack:?}: the same plan as a FLAC Load");
            assert_eq!(app.lead, pcm_lead_plan("PCM", None, stack), "{stack:?}");
            let native = load_audio("dts", RATE_NORMAL, None, None, stack).unwrap();
            assert_eq!((native.name, native.lead), ("DTS", PcmLead::Off), "{stack:?}: a compressed Load gets none");
        }
        assert_eq!(
            load_audio("dts", RATE_NORMAL, Some(&shape), None, AudioStack::Liblxa).unwrap().lead,
            PcmLead::FromOutput(AudioStack::Liblxa)
        );
    }

    /// The per-Load override reaches the Engine's lead through the same value, for a PCM Load only,
    /// and on every stack (it is how another chip gets measured).
    #[test]
    fn the_dev_override_reaches_the_engines_lead_through_the_declaration() {
        use crate::player::RATE_NORMAL;
        for stack in ALL_STACKS {
            assert_eq!(load_audio("flac", RATE_NORMAL, None, Some("330"), stack).unwrap().lead, PcmLead::Override(330_000_000, stack));
            assert_eq!(load_audio("ac3", 1500, None, Some("0"), stack).unwrap().lead, PcmLead::Override(0, stack));
            assert_eq!(load_audio("ac3", RATE_NORMAL, None, Some("330"), stack).unwrap().lead, PcmLead::Off);
        }
        let flac = load_audio("flac", RATE_NORMAL, None, None, AudioStack::Liblxa).unwrap();
        assert_eq!(flac.diagnostic, 5, "the diagnostics code is the same pair's");
    }

    /// What the frame thread can settle itself and what has to wait for the Load thread's read.
    #[test]
    fn only_a_lead_that_follows_the_output_waits_for_the_load_thread() {
        assert_eq!(PcmLead::Off.at_engine_start(), (0, false));
        assert_eq!(PcmLead::Override(330_000_000, AudioStack::Other).at_engine_start(), (330_000_000, false));
        assert_eq!(PcmLead::NoLead(AudioStack::Other).at_engine_start(), (0, false), "nothing to wait for: no lead");
        assert_eq!(
            PcmLead::Fixed(AudioStack::LgAudioSink).at_engine_start(),
            (PCM_SINK_LEAD_NS, false),
            "the lead is there from the start and nothing is pending"
        );
        for stack in OUTPUT_STACKS {
            assert_eq!(PcmLead::FromOutput(stack).at_engine_start(), (0, true), "{stack:?}: the audio waits for the Load thread's read");
        }
        for stack in NO_LEAD_STACKS {
            assert_eq!(PcmLead::NoLead(stack).at_engine_start(), (0, false), "{stack:?}");
        }
    }
}

/// The line from the set's audio stack to the Engine's lead, through the production start itself:
/// `start_bufferfeed_inner` reads `devcaps::audio_stack()` and hands it to `load_audio`. A start that
/// stopped passing it on (or passed a fixed one) leaves every other test green, because they all
/// build the plan or the Engine by hand.
#[cfg(all(test, feature = "hostsim"))]
mod pcm_stack_wiring_tests {
    use super::*;
    use plx_platform::devcaps::{audio_stack_on_this_thread_for_test, AudioStack};

    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::player::ffi_host::reset_native_lifecycle_for_test();
            crate::player::ffi_host::force_clocksink_for_test(false);
            SHARED.test_force_native_idle();
            SHARED.reset_session();
            crate::route::reset_player_control_for_test(crate::route::idle_session_for_test());
        }
    }

    /// Start a FLAC Load (declares `"audio":"PCM"`) on a set whose stack is `stack`, let its Load
    /// thread return, take what the Engine took from it, and give back `(pending before the take,
    /// lead after it)`. `unload_ms` is how long the host `Unload` of this start's stop takes, and
    /// `linger_ms` how long the Load is left returned before the stop (a late thread of an earlier
    /// start has that long to reach it).
    fn lead_of_a_flac_start_on(stack: AudioStack, unload_ms: u64, linger_ms: u64) -> (bool, i64) {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        SHARED.reset_session();
        crate::route::reset_player_control_for_test(&ps);
        crate::player::ffi_host::reset_native_lifecycle_for_test();
        crate::player::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _stack = audio_stack_on_this_thread_for_test(stack);
        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe { plx_base::task::MainThread::assume() });
        crate::route::set_url(&mut ps, "http://127.0.0.1:1/track.flac");
        crate::route::set_stream_acodec(&mut ps, "flac");
        crate::route::set_carried_audio_for_test(
            &mut ps,
            crate::route::CarriedAudio {
                sid: 7, ordinal: 1, codec: "flac".into(), channels: 2,
                can_normalize_loudness: false, immersive: false, sample_rate: 48_000, bit_depth: 16,
            },
        );

        let started = start_bufferfeed_tracked(&mut ps, &mut pa);
        assert!(
            matches!(started, BufferfeedStartOutcome::Launched(_)),
            "PRECONDITION FAILED: the FLAC start must launch ({started:?})"
        );
        let eng = pa.engine().expect("a launched start installs its Engine");
        assert!(
            eng.payload.to_string_lossy().contains("PCM"),
            "PRECONDITION FAILED: a FLAC Load must declare PCM: {:?}",
            eng.payload
        );
        let pending = eng.pcm_lead_pending;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !SHARED.native_load_returned(eng.native_epoch) {
            assert!(std::time::Instant::now() < deadline, "the Load never returned");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(settle_pcm_lead(eng), "a returned Load has settled its lead");
        let lead = eng.pcm_sink_lead_ns;
        std::thread::sleep(std::time::Duration::from_millis(linger_ms));
        assert!(
            SHARED.native_load_returned(eng.native_epoch),
            "a Load that returned stays returned until its stop: another session's Unload reached this one"
        );
        crate::player::ffi_host::delay_unload_for_test(unload_ms);
        stop_bufferfeed(&mut ps, &mut pa);
        finish_release_for_test(&mut pa);
        (pending, lead)
    }

    /// `liblxa` and 2018's stack yield a lead for a PCM Load once the Load thread has read the output
    /// (the hostsim bus has no answer, so the 400 ms default), pending until then; the LG audio sink
    /// has its 400 ms from the start with nothing pending; 2016/2017's `adecsink`, any other stack and
    /// one nobody probed yield none and have nothing to wait for.
    #[test]
    fn a_pcm_start_gets_the_lead_its_audio_stack_has_and_none_where_it_has_none() {
        assert_eq!(lead_of_a_flac_start_on(AudioStack::Liblxa, 0, 0), (true, PCM_SINK_LEAD_NS));
        assert_eq!(lead_of_a_flac_start_on(AudioStack::AudioSinkNew, 0, 0), (true, PCM_SINK_LEAD_NS));
        assert_eq!(lead_of_a_flac_start_on(AudioStack::LgAudioSink, 0, 0), (false, PCM_SINK_LEAD_NS));
        assert_eq!(lead_of_a_flac_start_on(AudioStack::AdecSink, 0, 0), (false, 0));
        assert_eq!(lead_of_a_flac_start_on(AudioStack::Other, 0, 0), (false, 0));
        assert_eq!(lead_of_a_flac_start_on(AudioStack::Unknown, 0, 0), (false, 0));
    }

    /// A stop hands its object to an `sf-unload` worker, and the host `Unload` names the object by
    /// the process-global active epoch. The worker of one start that runs late (a loaded CI runner)
    /// therefore unloads whichever session is active when it gets there: the next start's. This
    /// makes the worker late on purpose: the next start's Load returns, and must still be returned
    /// after the earlier worker's `Unload` has had its time. (The start helper used to drop its
    /// adapter, and with it the handle of the parked release, without `finish_release`.)
    #[test]
    fn an_earlier_starts_late_unload_never_reaches_the_next_start() {
        const UNLOAD_MS: u64 = 150;
        assert_eq!(lead_of_a_flac_start_on(AudioStack::Liblxa, UNLOAD_MS, 0), (true, PCM_SINK_LEAD_NS));
        assert_eq!(lead_of_a_flac_start_on(AudioStack::Other, 0, 2 * UNLOAD_MS), (false, 0));
    }
}

#[cfg(all(test, feature = "hostsim"))]
mod pcm_sink_lead_tests {
    use super::*;
    use crate::player::ffi_host;

    const V_STEP_NS: i64 = 41_708_333; // 23.976 fps
    const A_STEP_NS: i64 = 32_000_000;
    const VIDEO_AUS: i64 = 30;
    const AUDIO_AUS: i64 = 40;
    /// Where the in-place seek and the reload land, in content time.
    const T_NS: i64 = 905_000_000_000;

    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            ffi_host::force_clocksink_for_test(false);
            ffi_host::reset_seek_probe_for_test(None);
            crate::player::set_seek_mode(SeekMode::Segment);
            SHARED.pts_shift.store(0, Ordering::Relaxed);
            SHARED.seek_target_ns.store(-1, Ordering::Relaxed);
            SHARED.pres_fed.store(0, Ordering::Relaxed);
            SHARED.first_audio_pts.store(i64::MIN, Ordering::Relaxed);
            SHARED.applied_rate_permille.store(crate::player::RATE_NORMAL, Ordering::Relaxed);
        }
    }

    fn arm() -> Restore {
        ffi_host::force_clocksink_for_test(true);
        ffi_host::reset_seek_probe_for_test(None);
        crate::player::set_seek_mode(SeekMode::Segment);
        SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        SHARED.seek_to_ns.store(-1, Ordering::Relaxed);
        SHARED.pts_shift.store(0, Ordering::Relaxed);
        SHARED.first_audio_pts.store(i64::MIN, Ordering::Relaxed);
        Restore
    }

    /// A started engine (`lead` as the Load chose it) holding a keyframe at `at_ns` and the AUs
    /// after it, and audio from the same instant.
    fn engine_at(at_ns: i64, lead: i64) -> Engine {
        let mut eng = super::prime_livelock_tests::engine_after_reload();
        eng.prime_play = false;
        eng.pcm_sink_lead_ns = lead;
        let au = [0u8; 64];
        let qv = &mut **eng.aq_video.as_mut().unwrap() as *mut crate::aq::AuQueue;
        for i in 0..VIDEO_AUS {
            crate::aq::aq_push(qv, au.as_ptr(), au.len() as c_int, at_ns + i * V_STEP_NS, c_int::from(i == 0), 1);
        }
        let qa = &mut **eng.aq_audio.as_mut().unwrap() as *mut crate::aq::AuQueue;
        for i in 0..AUDIO_AUS {
            crate::aq::aq_push(qa, au.as_ptr(), au.len() as c_int, at_ns + i * A_STEP_NS, 1, 2);
        }
        eng
    }

    fn tick(eng: &mut Engine) {
        let mt = unsafe { plx_base::task::MainThread::assume() };
        feed_both_lanes(&mt, eng);
    }

    /// What the sink saw, with the preconditions asserted so a refused feed cannot pass for a pass.
    fn first_feeds() -> (i64, i64, Vec<i64>) {
        let v = ffi_host::feed_log_for_test(1);
        let a = ffi_host::feed_log_for_test(2);
        assert!(
            !v.is_empty() && !a.is_empty(),
            "PRECONDITION FAILED, not the defect: the host sink saw v={} a={} feeds, so it is not armed",
            v.len(),
            a.len()
        );
        (v[0], a[0], a)
    }

    /// Audio minus video at the first Feed of each, within the 1 ns the video lane's nudge moves it.
    fn assert_lead(eng: &Engine, lead: i64, what: &str) {
        let (v0, a0, audio) = first_feeds();
        let gap = a0 - v0;
        assert!((gap - lead).abs() <= 1, "{what}: audio is {gap} ns after the picture, want {lead} ns");
        assert!(
            audio.windows(2).all(|w| w[1] - w[0] > 0),
            "{what}: the lead is one constant, so audio sink pts still ascend: {audio:?}"
        );
        // Bookkeeping stays in content time: the sink saw the lead, the high-water mark did not.
        assert_eq!(
            eng.max_fed_audio_pts,
            audio.last().unwrap() - lead,
            "{what}: max_fed_audio_pts is content time, without the lead"
        );
    }

    fn pcm_first_load(lead: i64) -> Engine {
        let mut eng = engine_at(0, lead);
        tick(&mut eng);
        eng
    }

    #[test]
    fn a_pcm_load_plays_its_first_audio_a_lead_after_the_picture_on_a_first_load() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        let eng = pcm_first_load(PCM_SINK_LEAD_NS);
        assert_lead(&eng, PCM_SINK_LEAD_NS, "first Load");
    }

    #[test]
    fn a_pcm_load_keeps_the_lead_after_a_reload_rebase() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        let mut eng = engine_at(T_NS, PCM_SINK_LEAD_NS);
        eng.rebase_pending = true;
        tick(&mut eng);
        assert_eq!(SHARED.pts_shift.load(Ordering::Relaxed), -T_NS, "PRECONDITION: the reload rebased the keyframe to 0");
        assert_lead(&eng, PCM_SINK_LEAD_NS, "reload rebase");
    }

    #[test]
    fn a_pcm_load_keeps_the_lead_after_an_in_place_segment_seek() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        crate::player::set_seek_mode(SeekMode::Segment);
        let mut eng = engine_at(T_NS, PCM_SINK_LEAD_NS);
        eng.flushed = true;
        eng.rebase_pending = true;
        SHARED.seek_target_ns.store(T_NS, Ordering::Relaxed);
        tick(&mut eng);
        assert!(!eng.rebase_pending, "PRECONDITION: the seek's keyframe was accepted");
        assert_eq!(SHARED.pts_shift.load(Ordering::Relaxed), 0, "PRECONDITION: content time, no rebase");
        assert_lead(&eng, PCM_SINK_LEAD_NS, "in-place Segment seek");
    }

    #[test]
    fn a_compressed_load_gets_no_lead_anywhere() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        assert_lead(&pcm_first_load(0), 0, "first Load");
        ffi_host::reset_seek_probe_for_test(None);

        let mut eng = engine_at(T_NS, 0);
        eng.rebase_pending = true;
        tick(&mut eng);
        assert_lead(&eng, 0, "reload rebase");
        ffi_host::reset_seek_probe_for_test(None);

        let mut eng = engine_at(T_NS, 0);
        eng.flushed = true;
        eng.rebase_pending = true;
        SHARED.seek_target_ns.store(T_NS, Ordering::Relaxed);
        tick(&mut eng);
        assert_lead(&eng, 0, "in-place Segment seek");
    }

    /// A lead that follows the sound output is read on the Load thread (the frame thread may not
    /// block on the bus) and handed over WITH the Load's return. Until then the Engine feeds the
    /// picture and holds the audio back: stamping it with a guessed lead and moving it afterwards
    /// would jump the audio timeline inside one Load.
    #[test]
    fn audio_waits_for_the_output_lead_then_takes_exactly_that() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        SHARED.reset_session();
        let epoch = SHARED.begin_native_session().expect("native session");
        let mut eng = engine_at(0, 0);
        eng.native_epoch = epoch;
        eng.pcm_lead_pending = true;
        tick(&mut eng);
        assert!(!ffi_host::feed_log_for_test(1).is_empty(), "PRECONDITION: the picture is fed");
        assert!(ffi_host::feed_log_for_test(2).is_empty(), "no audio before the Load thread's lead is published");
        assert!(eng.pcm_lead_pending);

        assert!(SHARED.mark_native_load_returned(epoch, Some(250_000_000)).is_some());
        tick(&mut eng);
        assert!(!eng.pcm_lead_pending, "taken once, at the first feed after the Load returned");
        assert_eq!(eng.pcm_sink_lead_ns, 250_000_000);
        assert_lead(&eng, 250_000_000, "a Bluetooth Load");
        SHARED.reset_session();
    }

    #[test]
    fn the_load_thread_publishes_the_lead_it_resolved_with_the_loads_return() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        SHARED.reset_session();
        ffi_host::reset_native_lifecycle_for_test();
        let load = |lead: PcmLead| {
            let epoch = SHARED.begin_native_session().expect("native session");
            assert_eq!(SHARED.native_load_pcm_lead(epoch), None, "nothing before the Load returns");
            threads::load_thread(threads::SendPtr(c"{}".as_ptr() as *mut c_char), epoch, None, lead);
            assert!(SHARED.native_load_returned(epoch), "PRECONDITION: the Load returned");
            let published = SHARED.native_load_pcm_lead(epoch);
            SHARED.reset_session();
            ffi_host::reset_native_lifecycle_for_test();
            published
        };
        // A host has no platform to ask, so "no answer" is what a PCM Load resolves to here.
        assert_eq!(load(PcmLead::FromOutput(AudioStack::Liblxa)), Some(PCM_SINK_LEAD_NS));
        assert_eq!(load(PcmLead::FromOutput(AudioStack::AudioSinkNew)), Some(PCM_SINK_LEAD_NS));
        assert_eq!(load(PcmLead::Off), None, "a Load that needs no read publishes none");
        assert_eq!(load(PcmLead::Override(330_000_000, AudioStack::Liblxa)), None, "an override is settled before the thread starts");
        assert_eq!(load(PcmLead::NoLead(AudioStack::Other)), None, "a stack with no lead is settled before the thread starts");
        assert_eq!(load(PcmLead::NoLead(AudioStack::AdecSink)), None, "2016/2017: no lead, no read");
        assert_eq!(load(PcmLead::Fixed(AudioStack::LgAudioSink)), None, "a lead that ignores the output is settled before the thread starts");
    }

    /// At 1.5x the picture's pts and the audio's both run on the divided timeline, and the lead on
    /// top of it is still the output's, not scaled.
    #[test]
    fn the_lead_is_not_scaled_at_a_speed() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        SHARED.applied_rate_permille.store(1500, Ordering::Relaxed);
        let mut eng = engine_at(T_NS, PCM_SINK_LEAD_NS);
        eng.flushed = true;
        eng.rebase_pending = true;
        SHARED.seek_target_ns.store(T_NS, Ordering::Relaxed);
        tick(&mut eng);
        let (v0, a0, audio) = first_feeds();
        assert_eq!(a0, crate::player::to_sink_ns(T_NS) + PCM_SINK_LEAD_NS);
        assert!((a0 - v0 - PCM_SINK_LEAD_NS).abs() <= 1, "audio is {} ns after the picture", a0 - v0);
        let last_content = crate::player::from_sink_ns(audio.last().unwrap() - PCM_SINK_LEAD_NS);
        assert!(
            (eng.max_fed_audio_pts - last_content).abs() <= 2,
            "max_fed_audio_pts {} is content time, the sink's last audio gives {last_content}",
            eng.max_fed_audio_pts
        );
    }
}
