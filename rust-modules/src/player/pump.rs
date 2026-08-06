//! player::pump — the main-thread pump (was bufferfeed_pump). Runs each frame from
//! plex_run: the pending-seek handler, the ACB-bind state machine (Stage), and the
//! feed dispatch. All ACB/Starfish control calls happen here on the main thread.
use super::engine::{drain_aq, engine, feed_audio_lane, feed_sample, feed_stream, Source};
use super::shared::Stage;
use super::{ffi, ACB_OK, SHARED, TX};
use crate::task::MainThread;
use std::os::raw::c_char;
use std::sync::atomic::Ordering::{Relaxed, Release};

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

pub(crate) fn pump(mt: &MainThread, now: u32) {
    use super::shared::PlaybackState;
    let eng = match engine(mt) {
        Some(e) => e,
        None => {
            set_state(PlaybackState::Idle);
            return;
        }
    };
    // wait for the media-thread ctor
    if unsafe { ffi::sf_ready(mt) } == 0 {
        set_state(PlaybackState::Connecting);
        return;
    }
    // The producer died before publishing a duration: the EOS path is gated on `duration_ns > 0`
    // so it can NEVER fire, and the player used to sit on a black screen forever with no error
    // and no exit. Surface it instead — BACK is already the escape.
    if SHARED.demux_failed.load(Relaxed) && SHARED.frames.load(Relaxed) == 0 {
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
        SHARED.playpos_ns.store(t, Relaxed); // displayed position jumps; wall clock takes over
    }

    // ---------- load -> Play (decode fed frames as soon as loaded) ----------
    if eng.stage == Stage::Loading
        && (SHARED.load_completed.load(Relaxed) || unsafe { ffi::sf_is_load_completed(mt) } != 0)
    {
        SHARED.load_completed.store(true, Relaxed);
        super::log("SMP loadCompleted");
        eng.stage = Stage::Playing;
        // A fresh Load for a seek/resume (rebase_pending) primes before Play so the clock does
        // not free-run through the demux av_seek reopen gap (fast-forward on resume). Initial
        // play-from-0 has no such gap — Play immediately.
        if eng.rebase_pending {
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

    // ---------- webOS 5+ (VP_EXPORTED): place the video rect once frames flow ----------
    //
    // The counterpart of the whole ACB block above, and it is one call, because that is all webOS
    // 5 kept: the binding itself already happened when `option.windowId` went into the Load
    // payload. There is no sourceInfo to forward and no LOADED/PLAYING state to mirror — that
    // sequence was deleted, not replaced.
    //
    // Gated on the same `frames >= 2` as the ACB path so the sink exists before we size it, and
    // done once per session. UNTESTED ON HARDWARE.
    if eng.stage == Stage::Playing
        && !eng.video_info_sent
        && ffi::vp_mode() == ffi::VP_EXPORTED
        && SHARED.frames.load(Relaxed) >= 2
    {
        // src is the decoded frame size, dst the on-screen rect; the pair also expresses scaling.
        // Both are the full panel here for the same reason acb_start passes 0,0,1920,1080 — the
        // app authors at a fixed 1080p and plays full-screen.
        let rv = unsafe { ffi::vp_place(mt, 1920, 1080, 0, 0, 1920, 1080) };
        eng.video_info_sent = true;
        eng.stage = Stage::Streaming;
        super::log(&format!("vplane: exported window placed rv={rv} → streaming"));
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
