//! player::pump — the main-thread pump (was bufferfeed_pump). Runs each frame from
//! plex_run: the pending-seek handler, the ACB-bind state machine (Stage), and the
//! feed dispatch. All ACB/Starfish control calls happen here on the main thread.
use super::engine::{drain_aq, engine, feed_audio_lane, feed_sample, feed_stream, Engine, Source};
use super::shared::Stage;
use super::{ffi, ACB_OK, SHARED, TX};
use crate::task::MainThread;
use std::os::raw::c_char;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

/// Publish the one value the HUD renders from. Pure derivation off signals the workers already
/// maintain — no new cross-thread plumbing, and it runs on every path out of `pump` (including
/// the early returns) so the state can never go stale behind a spinner that never resolves.
fn set_state(s: super::shared::PlaybackState) {
    SHARED.pb_state.store(s as u8, Relaxed);
}

/// How many seek requests this pump seek MERGED — i.e. requests that arrived while an earlier
/// seek was still resolving and so never got a seek of their own. Reset as it's read, so each
/// logged seek reports only the requests it consumed.
///
/// The count is what makes coalescing observable. `seek_to_ns` only ever holds the newest target,
/// so a burst of six taps and a single tap leave identical state behind; and the burst is applied
/// by whichever of two paths wins a race with the stuck-watchdog, which log different lines.
/// Counting either line therefore measures PMS latency, not merging — `coalesced=` measures
/// merging directly, and both paths report it.
fn take_coalesced() -> u32 {
    TX.seek_reqs.swap(0, Relaxed).saturating_sub(1)
}

fn prime_before_play(rebase_pending: bool, segmented_hls: bool) -> bool {
    rebase_pending || segmented_hls
}

/// Place the webOS 5+ exported window: source frame -> on-screen rect.
///
/// No-op on the ACB path. `src` is the frame being FED and `dst` where it lands, and the pair is
/// what expresses scaling — so a 4K direct play must not be described as 1080p, which passing the
/// authoring canvas for both would do. The coded size comes from the demuxer, the only thing that
/// knows it for certain; before it has published, the canvas is the least-wrong fallback and the
/// caller re-places once the real one arrives.
fn place_exported(mt: &MainThread, eng: &mut super::engine::Engine) {
    if ffi::vp_mode() != ffi::VP_EXPORTED {
        return;
    }
    let (w, h) = (SHARED.video_w.load(Relaxed), SHARED.video_h.load(Relaxed));
    let src = if w > 0 && h > 0 { (w, h) } else { (1920, 1080) };
    let rv = unsafe { ffi::vp_place(mt, src.0, src.1, 0, 0, 1920, 1080) };
    eng.placed_src = src;
    // RECORD it, do not merely log it. These three fields had no writer anywhere in the tree, so
    // `dg_place_rv` sat at its `i32::MIN` "never called" sentinel for the life of the process and
    // the diagnostics read-out rendered `Placed: not placed` in DANGER bold on every webOS 5+ set
    // — a fabricated fault, on the one firmware family nobody here can test, pointing every reader
    // at the video plane. The dev TV takes the ACB path, so no amount of looking at it would have
    // caught this.
    SHARED.dg_place_rv.store(rv, Relaxed);
    SHARED.dg_placed_w.store(src.0, Relaxed);
    SHARED.dg_placed_h.store(src.1, Relaxed);
    super::log(&format!("vplane: exported window placed src={}x{} rv={rv}", src.0, src.1));
}

/// Mirror the Engine-confined diagnostics into `Shared` for the render path.
///
/// The read-out cannot call `engine(&MainThread)` itself — that hands out a `&'static mut` to a
/// `static mut`, and the draw runs inside a frame where the pump's borrow may still be live, so a
/// second one is instant UB. This is the only bridge, it is one-way, and nothing in the playback
/// state machine may read these fields back.
///
/// `aq_bytes` takes each queue's pthread mutex, which is why it is sampled HERE, once per tick on
/// the main thread, and never from a draw.
/// Last `frames` we saw, so a CHANGE can be stamped. A decrease counts: `frames` is seek-scoped
/// and the pump zeroes it applying a seek, which is motion, not a freeze.
static LAST_FRAMES: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

fn publish_diag(eng: &Engine, now: u32) {
    // Nobody is looking: skip it. `aq_bytes` takes each queue's pthread mutex, and the read-out
    // samples at 2 Hz, so publishing at 60 Hz is 30x more often than anything can observe. Costs
    // no freshness — the loop order is pump → stats::update → stats::draw, so the frame the panel
    // is switched on has already republished.
    if !crate::ui::stats::enabled() {
        return;
    }
    SHARED.dg_stage.store(eng.stage as u8, Relaxed);
    let qv = eng.aq_video.as_ref().map_or(0, |q| {
        crate::aq::aq_bytes(&**q as *const _ as *mut _) as i64
    });
    let qa = eng.aq_audio.as_ref().map_or(0, |q| {
        crate::aq::aq_bytes(&**q as *const _ as *mut _) as i64
    });
    SHARED.dg_aq_video.store(qv, Relaxed);
    SHARED.dg_aq_audio.store(qa, Relaxed);
    SHARED.dg_fed_v_pts.store(eng.max_fed_video_pts, Relaxed);
    SHARED.dg_fed_a_pts.store(eng.max_fed_audio_pts, Relaxed);
    // Stamp when the frame count MOVES. The panel needs "how long has it been stuck" and a
    // photograph has no time axis; stamping here rather than in `ui::stats` is what makes the
    // clock measure the STALL rather than how long the panel has been open.
    let f = SHARED.frames.load(Relaxed);
    if LAST_FRAMES.swap(f, Relaxed) != f {
        SHARED.dg_frame_at.store(now, Relaxed);
    }
}

pub(crate) fn pump(mt: &MainThread, now: u32) {
    use super::shared::PlaybackState;
    let eng = match engine(mt) {
        Some(e) => e,
        None => {
            set_state(PlaybackState::Idle);
            return;
        }
    };
    // Republish the Engine-confined values the diagnostics read-out needs, BEFORE the early
    // returns below. Placed here, immediately after the borrow, for two reasons: it is the one
    // spot that cannot be forgotten as the pump grows, and it therefore keeps reporting through
    // the `sf_ready` and producer-died bail-outs — which is precisely when a stalled user is
    // looking at the panel. See `Shared`'s diagnostics-mirror block for the one-way rule.
    publish_diag(eng, now);
    // wait for the media-thread ctor
    if unsafe { ffi::sf_ready(mt) } == 0 {
        set_state(PlaybackState::Connecting);
        return;
    }
    // Auto began on a proven-fast direct Remote, but the live transfer later lost both rate and
    // content reserve. The demuxer has stopped cleanly at a packet boundary; replace the route and
    // pipeline at the current movie position. This precedes the generic producer-death gates so
    // the intentional handoff cannot be mistaken for EOF/Error.
    let fallback_kbps = SHARED.auto_fallback_kbps.swap(0, Acquire);
    if fallback_kbps > 0 {
        let secs = (SHARED.playpos_ns.load(Relaxed) / 1_000_000_000).max(0);
        let measured_kbps = u32::try_from(fallback_kbps).unwrap_or(u32::MAX);
        if crate::route::fallback_auto_to_hls(measured_kbps, secs).is_some() {
            super::engine::reload_transcode(mt, secs * 1_000_000_000);
            return;
        }
        super::log("auto: Original watchdog handoff was stale or the HLS rebuild failed");
        SHARED.demux_io_failed.store(true, Relaxed);
    }
    // The symmetric Auto handoff. The HLS worker stopped only after two successful probes of the
    // actual source; restore either direct play (source seek + native payload) or a zero-video-
    // encode remux (offset transcode + matching payload). Both replace the Engine, so return.
    let recover_kbps = SHARED.auto_recover_kbps.swap(0, Acquire);
    if recover_kbps > 0 {
        let pos_ns = SHARED.playpos_ns.load(Relaxed).max(0);
        let secs = pos_ns / 1_000_000_000;
        match crate::route::recover_auto_to_original(secs) {
            Some(crate::route::AutoOriginalReload::Direct) => {
                super::engine::reload_at(mt, pos_ns);
                return;
            }
            Some(crate::route::AutoOriginalReload::Remux) => {
                super::engine::reload_transcode(mt, pos_ns);
                return;
            }
            None => {
                super::log("auto: Original recovery handoff was stale or the source rebuild failed");
                SHARED.demux_io_failed.store(true, Relaxed);
            }
        }
    }
    // The producer died before publishing a duration: the EOS path is gated on `duration_ns > 0`
    // so it can NEVER fire, and the player used to sit on a black screen forever with no error
    // and no exit. Surface it instead — BACK is already the escape.
    if SHARED.demux_io_failed.load(Relaxed) {
        set_state(PlaybackState::Error);
        return;
    }
    if SHARED.demux_failed.load(Relaxed) && SHARED.frames.load(Relaxed) == 0 {
        set_state(PlaybackState::Error);
        return;
    }
    // The same escape for a Load the pipeline REFUSED. `loadCompleted` can never arrive after
    // that, so without this the pump stays in Stage::Loading and the user watches a spinner with
    // no error and no way to tell it apart from a slow server.
    if SHARED.load_failed.load(Relaxed) {
        set_state(PlaybackState::Error);
        return;
    }
    let stream = matches!(eng.source, Source::Stream);

    // ---------- pending NATIVE audio-track switch (direct-play, no transcode): reload
    // direct-play feeding the chosen audio stream from the same MKV. REPLACES the ENGINE, so
    // `eng` dangles — return immediately. ----------
    let nidx = SHARED.pending_audio_idx.swap(-1, Relaxed);
    if stream && nidx >= 0 && eng.stage >= Stage::Playing {
        let pos = SHARED.playpos_ns.load(Relaxed).max(0);
        super::log(&format!("audio switch (native): idx={nidx} at {}s → reload", pos / 1_000_000_000));
        super::engine::switch_audio_native(mt, nidx, pos);
        return;
    }

    // ---------- pending audio-track switch OR subtitle-burn refresh: force a fresh transcode
    // (H264/AC3) with the selected audio + subtitle at the CURRENT position, then RELOAD the
    // pipeline. A direct-play item is Loaded for its native codec (e.g. H265); the transcode
    // output is H264, so we MUST re-Load with the H264 payload — flush+refeeding H264 into the
    // H265 pipeline stalls. reload_transcode REPLACES the ENGINE, so `eng` dangles after it:
    // return immediately. ----------
    let asid = SHARED.pending_audio_sid.swap(-1, Relaxed);
    let refresh = SHARED.pending_retranscode.swap(false, Relaxed);
    if stream && (asid >= 0 || refresh) && eng.stage >= Stage::Playing {
        let secs = (SHARED.playpos_ns.load(Relaxed) / 1_000_000_000).max(0);
        let rebuilt = if asid >= 0 { crate::route::switch_audio(asid, secs) } else { crate::route::retranscode(secs) };
        if rebuilt.is_some() {
            super::log(&format!("re-transcode: asid={asid} refresh={refresh} offset={secs}s → reload"));
            super::engine::reload_transcode(mt, secs * 1_000_000_000);
            return;
        }
    }

    // ---------- an in-place seek is still resolving (flushed, not yet re-anchored): COALESCE.
    // Firing another now would race the demux reopen (rapid tap-seeks) and the rebase guard would
    // then eat everything → stuck. Hold the latest requested target (it stays in TX.seek_to_ns) and
    // process it once this seek anchors. If it hasn't anchored within SEEK_STUCK_MS the reopen was
    // lost — first RETRY the reopen cheaply (re-interrupt the demuxer + re-arm), and only fall back
    // to a full (slow) reload if the retries also fail.
    // SEEK_STUCK_MS must comfortably exceed a real reopen+av_seek+first-keyframe on 4K HEVC over
    // PMS (~700ms): the retry closes the demux socket, so a premature watchdog KILLS an open that
    // was about to succeed and restarts it — at 500ms a rapid tap-burst self-DoS'd into the
    // reload fallback every time (caught by the seek_rapid harness cases).
    const SEEK_STUCK_MS: u32 = 1200;
    if eng.flushed && eng.rebase_pending && now.wrapping_sub(eng.seek_armed_at) > SEEK_STUCK_MS {
        // Adopt the NEWEST coalesced target if later taps landed while this seek was resolving
        // (TX.seek_to_ns holds the latest request): retrying/reloading at the original armed
        // target would land where the user already tapped away from, then seek AGAIN.
        let pending = TX.seek_to_ns.swap(-1, Relaxed);
        let tgt = if pending >= 0 { pending } else { SHARED.seek_target_ns.load(Relaxed) }.max(0);
        if eng.seek_retries < 2 {
            eng.seek_retries += 1;
            SHARED.seek_to_ns.store(tgt, Release); // re-publish; the demux thread seeks on it
            SHARED.seek_target_ns.store(tgt, Relaxed); // rebase guard keys on the retried target
            eng.seek_armed_at = now;
            super::log(&format!(
                "seek: in-place stuck → retry reopen at {}s (#{}) coalesced={}",
                tgt / 1_000_000_000,
                eng.seek_retries,
                take_coalesced()
            ));
        } else {
            super::log(&format!("seek: in-place stuck → reload at {}s", tgt / 1_000_000_000));
            super::engine::reload_at(mt, tgt); // REPLACES the engine — eng dangles, return
            return;
        }
    }
    // ---------- pending seek: flush, drop queued AUs, re-point the demux ----------
    let t = TX.seek_to_ns.load(Relaxed);
    // A live transcode has NO Content-Length (file_size stays -1), so gate it on duration
    // only and seek by RESTARTING the transcode at a time &offset (below). Direct-play
    // seeks in place via av_seek.
    let is_transcode = crate::route::is_transcoding();
    if stream
        && t >= 0
        && !(eng.flushed && eng.rebase_pending) // coalesce: don't stack in-place seeks
        && eng.stage >= Stage::Playing
        && SHARED.duration_ns.load(Relaxed) > 0
        && (is_transcode || SHARED.file_size.load(Relaxed) > 0)
    {
        TX.seek_to_ns.store(-1, Relaxed);
        let coalesced = take_coalesced(); // read here so BOTH branches consume this seek's requests
        let t = t.max(0);
        // TRANSCODE seek: restart the encode at the new &offset and do a FULL RELOAD (fresh Load).
        // A flush()+refeed of the new start.mkv left a STALE GStreamer segment → visual artifacts on
        // the jump (the same class of bug the in-place seek cured for direct-play; a transcode can't
        // use the in-place path — the stream content itself changes at the new offset). A fresh Load
        // rebuilds the segment by construction. reload_transcode REPLACES the ENGINE, so `eng`
        // dangles after it — return immediately and let the next pump() tick drive the fresh engine.
        if is_transcode {
            let secs = t / 1_000_000_000;
            if crate::route::transcode_seek(secs).is_some() {
                super::engine::reload_transcode(mt, t);
            } else {
                super::log("seek(transcode): rebuild failed");
            }
            return;
        }
        // DIRECT-PLAY seek: Kodi IN-PLACE seek — flush + av_seek the demuxer, and
        // (in feed_stream, on the first post-flush keyframe) setTimeToDecode + sendSegmentEvent
        // to re-anchor the GStreamer segment WITHOUT a reload/decoder re-init (no A/V-resync
        // glitch). A plain flush()+refeed with NO fresh segment left a STALE GStreamer segment
        // → the HW sink stops draining ~48 s later → permanent BufferFull + "Playing error"
        // (root-caused by decompiling libpipeline/lxvideosink; see engine::reload_at). Falls
        // back to reload-per-seek if the pipeline isn't reachable (INPLACE_SEEK_OK cleared by
        // feed_stream when sendSegmentEvent can't find it). reload_at REPLACES the ENGINE, so
        // `eng` dangles after it: return immediately.
        if !super::INPLACE_SEEK_OK.load(Relaxed) {
            super::engine::reload_at(mt, t);
            return;
        }
        unsafe {
            ffi::sf_flush(mt); // drop decoded/queued frames
            ffi::sf_pause(mt); // freeze the clock; feed_stream Plays once PRIME_NS is buffered
        }
        drain_aq(eng);
        // in-place direct-play seek: publish the target and let the DEMUX THREAD av_seek_frame
        // between two reads (ff.rs's inner loop). Nothing here interrupts the socket: the pump
        // used to shutdown(2) it to break the read so the outer loop would reopen+seek, but our
        // AVIO is seekable, so libavformat just healed the broken read through `seek_cb` and read
        // on — no reopen, no seek, and every direct-play seek escalated to a reload. See ff.rs.
        // eng.flushed → feed_stream fires setTimeToDecode+sendSegmentEvent on the first post-seek
        // keyframe. disp_base=0 (rebase to the landed keyframe).
        eng.flushed = true;
        SHARED.seek_to_ns.store(t, Release); // the demux thread's av_seek target
        SHARED.seek_target_ns.store(t, Relaxed); // rebase guard: reject stale drifted keyframes
        SHARED.disp_base.store(0, Relaxed);
        super::log(&format!("seek(in-place): av_seek t={t} coalesced={coalesced}"));
        // zero-base the fed timeline on the first post-seek keyframe (feed_stream), so it
        // presents against the flush-reset clock immediately — no catch-up freeze
        eng.rebase_pending = true;
        eng.rebase_drops = 0;
        eng.seek_armed_at = now; // stuck-watchdog start (see the coalesce/escape above)
        eng.seek_retries = 0;
        eng.max_fed_video_pts = 0;
        eng.max_fed_audio_pts = 0;
        // Drop any AU held from BEFORE the seek (the per-lane BufferFull retry). drain_aq cleared the
        // QUEUES but not these pending AUs; feeding a stale pre-seek AU after the seek re-bases it to
        // the NEW pts_shift → a big A/V desync — the stale audio pts advances max_fed_audio_pts, then
        // the STALE_BACKJUMP guard drops the fresh post-seek audio as "too far back" → no sound. The
        // reopened stream supplies fresh post-seek AUs.
        eng.pending_video = None;
        eng.pending_audio = None;
        eng.prime_play = true; // paused above; Play once PRIME_NS is buffered (no fast-forward)
        // "no post-seek frame presented yet" — the feed-ahead throttle feeds freely until the first
        // real presented pts lands, instead of comparing the new fed pts against the STALE pre-seek
        // presented position (which would wrongly break feeding on a forward in-place seek).
        SHARED.pres_fed.store(super::engine::PRES_NONE, Relaxed);
        SHARED.frames.store(0, Relaxed); // count only POST-seek frames (rebind + resume re-pause gate)
        // …and forget the last presentation stamp with it. The plane goes dark across a seek by
        // design, so the first frame after one would otherwise post a gap the size of the seek and
        // read as a stutter (`Shared`'s `dg_vpres_*` block).
        SHARED.dg_vpres_at.store(0, Relaxed);
        SHARED.playpos_ns.store(t, Relaxed); // displayed position jumps; wall clock takes over
    }

    // ---------- load -> Play (decode fed frames as soon as loaded) ----------
    if eng.stage == Stage::Loading
        && (SHARED.load_completed.load(Relaxed) || unsafe { ffi::sf_is_load_completed(mt) } != 0)
    {
        SHARED.load_completed.store(true, Relaxed);
        // when it completed, so the panel can say how long we have been waiting for a frame since
        SHARED.dg_load_at.store(now, Relaxed);
        super::log("SMP loadCompleted");
        eng.stage = Stage::Playing;
        // webOS 5+: place the exported window HERE, the instant Load reports success — which is
        // exactly where ss4s does it (smp_player.c calls StarfishResourcePostLoad synchronously
        // on the line after StarfishMediaAPIs_load returns true).
        //
        // It used to wait for `frames >= 2`, copied from the ACB block below, and that is a
        // DEADLOCK on this path: the pipeline does not emit frames until its sink is bound, and
        // the sink is not placed until frames arrive. The symptom is a player stuck in buffering
        // forever, which is what webOS 6 and 10 reported. The gate is right for ACB and only for
        // ACB — `setMediaVideoData` there needs the sourceInfo envelope, which cannot exist until
        // the pipeline has produced something. The exported window has no such dependency: the
        // BINDING already happened via `option.windowId` in the Load payload, and this call is
        // pure geometry.
        place_exported(mt, eng);
        if ffi::vp_mode() == ffi::VP_EXPORTED {
            // Playing -> Streaming directly: there is no bind sequence to sit in the middle of.
            // The transition is load-bearing beyond bookkeeping — `pushEOS` is gated on
            // `>= Streaming`, so without it the last frames never drain and Up Next never fires.
            eng.stage = Stage::Streaming;
        }
        // A fresh Load for a seek/resume primes before Play so the clock does not free-run through
        // the demux reopen gap. Segmented HLS needs the same gate even at offset zero: its video
        // and AAC arrive through independent queues, and starting Starfish's audio-master clock
        // before AAC is present produced silent initial playback that recovered only after a seek
        // (the seek path already primed both lanes). Progressive initial play keeps its proven
        // immediate-start behavior.
        if prime_before_play(eng.rebase_pending, crate::route::is_segmented_hls()) {
            eng.prime_play = true;
            super::log("SMP loadCompleted (priming before Play)");
        } else {
            unsafe { ffi::sf_play(mt) };
            super::log("SMP Play");
        }
    }

    // ---------- ACB bind, Kodi/ss4s order: setSinkType(MAIN)+setMediaId+setState(LOADED) ----------
    if eng.stage == Stage::Playing && ACB_OK.load(Relaxed) {
        let id = SHARED.media_id.lock().unwrap().clone();
        if let Some(id) = id {
            unsafe { ffi::acb_bind(mt, id.as_ptr()) };
            super::log(&format!("SMP ACB bound id={}", id.to_string_lossy()));
            // **Dolby Atmos, to ACB, exactly here** — this is where LG's own client fires it
            // (`libcbe` 0x1b98d78, on LOADCOMPLETED): after setMediaId, and BEFORE the
            // frame-gated setMediaVideoData below. It needs no decoded frame and reads no
            // state; `context` is the `id` we have in hand, which is the whole reason the call
            // exists rather than a forward of the pipeline's own AUDIO_INFO callback (that
            // string carries `track` and `dualMono` and no context). See `acb_send_atmos`.
            //
            // **This looks like it breaks the "never feed audio to ACB" rule and it does not** —
            // the rule is about the audio ELEMENTARY STREAM, which the pipeline owns and which we
            // still never hand ACB. This is a two-key metadata descriptor, and the distinction is
            // now readable rather than remembered (see `acb_send_atmos` in `src/starfish.c` for
            // the addresses). The rule's stated consequence, `SOUND_ERROR_019`, is a literal that
            // exists in NO library on this device.
            //
            // Ran behind `/tmp/plxnative-atmosacb` first, then measured, then made the default:
            // `rv=1` accepted, 1600 audio AUs fed with `reply=O` and no error of any kind, and the
            // television's own read-out — "Dolby Vision / Dolby Atmos", both lines — photographed
            // in a DISPLAY capture at 11 s. `/tmp/plxnative-noatmosacb` is the way back out.
            if crate::route::stream_immersive() && !crate::dev::flag("noatmosacb") {
                let rv = unsafe { ffi::acb_send_atmos(mt, id.as_ptr()) };
                super::log(&format!("atmos: acb setMediaAudioData rv={rv}"));
            }
            eng.stage = Stage::Bound;
        }
    }

    // ---------- send the WHOLE sourceInfo envelope VERBATIM, once frames flow, then
    // window + PLAYING (setMediaVideoData -> setDisplayWindow -> setState PLAYING) ----------
    if eng.stage == Stage::Bound && !eng.video_info_sent && SHARED.frames.load(Relaxed) >= 2 {
        let bytes = SHARED.source_info.lock().unwrap().clone();
        if let Some(bytes) = bytes {
            let rv = unsafe { ffi::acb_send_video_data(mt, bytes.as_ptr() as *const c_char) };
            super::log(&format!("setMediaVideoData rv={rv} frames={}", SHARED.frames.load(Relaxed)));
            if rv != -1 {
                // -1 = client-side isJsonError reject; else accepted
                eng.video_info_sent = true;
                unsafe { ffi::acb_start(mt, 0, 0, 1920, 1080) };
                eng.stage = Stage::Streaming;
                super::log("setMediaVideoData sent → window+PLAYING");
            }
        }
    }

    // ---------- webOS 5+ (VP_EXPORTED): correct the placement if the real frame size only
    // became known after Load. The demuxer publishes the coded size when it opens the stream,
    // which races the load thread — so the placement above may have used the fallback. Re-placing
    // costs one call, and Kodi re-places on every render-area change anyway.
    // ----------
    if eng.stage >= Stage::Playing && eng.placed_src != (0, 0) {
        let now = (SHARED.video_w.load(Relaxed), SHARED.video_h.load(Relaxed));
        if now.0 > 0 && now.1 > 0 && now != eng.placed_src {
            place_exported(mt, eng);
        }
    }

    // ---------- feed AUs once playing (Feed only succeeds after Play). NOT while a seek
    // is armed: on a resume the seek is armed before PLAYING, so feeding first would
    // present the file start for a frame before the seek repositions — a visible jump. ----------
    if eng.stage >= Stage::Playing && !TX.paused.load(Relaxed) && TX.seek_to_ns.load(Relaxed) < 0 {
        if stream {
            // Two-lane feed: VIDEO lane first — it owns the seek rebase (clears rebase_pending +
            // publishes pts_shift) — then the AUDIO lane, which sees that fresh shift the same tick.
            // A BufferFull in one lane no longer stalls the other.
            feed_stream(mt, eng);
            feed_audio_lane(mt, eng);
        } else {
            feed_sample(mt, eng);
        }
    }

    // ---------- end-of-stream: the producer hit file EOF (eos_pushed → all AUs to the end were
    // fed) and the pipeline has now played out to within EOS_TAIL of the duration. Mark ended so
    // app.rs tears the player down at the credits instead of freezing on the last frame. Paused
    // at the end stays paused (playpos won't climb) — correct; it fires when resumed. Any seek
    // clears the flag (request_seek), so seeking back from the end doesn't re-trigger. ----------
    // ---------- publish the derived UI state (see PlaybackState's doc) ----------
    // Order matters: a seek in flight outranks "we have frames", because the frames on the panel
    // are the PRE-seek ones and the HUD must show the target, not them.
    set_state(if SHARED.seeking.load(Relaxed) {
        PlaybackState::Seeking
    } else if eng.stage == Stage::Loading {
        PlaybackState::Connecting
    } else if SHARED.frames.load(Relaxed) == 0 {
        PlaybackState::Buffering
    } else {
        PlaybackState::Playing
    });

    const EOS_TAIL_NS: i64 = 1_000_000_000;
    if eng.eos_pushed && !SHARED.ended.load(Relaxed) {
        let dur = SHARED.duration_ns.load(Relaxed);
        let pos = SHARED.playpos_ns.load(Relaxed);
        if dur > 0 && pos >= dur - EOS_TAIL_NS {
            SHARED.ended.store(true, Relaxed);
            super::log(&format!("EOS reached: playpos={}s/{}s → ended", pos / 1_000_000_000, dur / 1_000_000_000));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::prime_before_play;

    #[test]
    fn segmented_hls_primes_both_lanes_even_without_a_seek() {
        assert!(prime_before_play(false, true));
        assert!(prime_before_play(true, true));
        assert!(prime_before_play(true, false));
        assert!(!prime_before_play(false, false));
    }
}
