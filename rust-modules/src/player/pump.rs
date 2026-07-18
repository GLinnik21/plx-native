//! player::pump — the main-thread pump (was bufferfeed_pump). Runs each frame from
//! plex_run: the pending-seek handler, the ACB-bind state machine (Stage), and the
//! feed dispatch. All ACB/Starfish control calls happen here on the main thread.
use super::engine::{cue_byte_for, drain_aq, engine, feed_audio_lane, feed_sample, feed_stream, Source};
use super::shared::Stage;
use super::{ffi, ACB_OK, SHARED, TX};
use std::os::raw::c_char;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

pub(crate) fn pump(now: u32) {
    let eng = match engine() {
        Some(e) => e,
        None => return,
    };
    // wait for the media-thread ctor
    if unsafe { ffi::sf_ready() } == 0 {
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
        super::engine::switch_audio_native(nidx, pos);
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
            super::engine::reload_transcode(secs * 1_000_000_000);
            return;
        }
    }

    // ---------- an in-place seek is still resolving (flushed, not yet re-anchored): COALESCE.
    // Firing another now would race the demux reopen (rapid tap-seeks) and the rebase guard would
    // then eat everything → stuck. Hold the latest requested target (it stays in TX.seek_to_ns) and
    // process it once this seek anchors. If it hasn't anchored within SEEK_STUCK_MS the reopen was
    // lost — first RETRY the reopen cheaply (re-interrupt the demuxer + re-arm), and only fall back
    // to a full (slow) reload if the retries also fail.
    const SEEK_STUCK_MS: u32 = 500;
    if eng.flushed && eng.rebase_pending && now.wrapping_sub(eng.seek_armed_at) > SEEK_STUCK_MS {
        let tgt = SHARED.seek_target_ns.load(Relaxed).max(0);
        if eng.seek_retries < 2 {
            eng.seek_retries += 1;
            *SHARED.next_url.lock().unwrap() = Some(crate::route::url());
            SHARED.seek_byte.store(0, Release);
            SHARED.seek_to_ns.store(tgt, Release);
            let p = SHARED.hs_ptr.load(Acquire);
            if !p.is_null() {
                crate::stream::http_close(p); // re-interrupt: the first close raced the reopen
            }
            eng.seek_armed_at = now;
            super::log(&format!("seek: in-place stuck → retry reopen at {}s (#{})", tgt / 1_000_000_000, eng.seek_retries));
        } else {
            super::log(&format!("seek: in-place stuck → reload at {}s", tgt / 1_000_000_000));
            super::engine::reload_at(tgt); // REPLACES the engine — eng dangles, return
            return;
        }
    }
    // ---------- pending seek: flush, drop queued AUs, re-point the demux ----------
    let t = TX.seek_to_ns.load(Relaxed);
    // A live transcode has NO Content-Length / byte-Cues (file_size stays -1), so gate
    // it on duration only and seek by RESTARTING the transcode at a time &offset (below),
    // not a byte offset. Direct-play keeps the byte-Range path.
    let is_transcode = crate::route::is_transcoding();
    if stream
        && t >= 0
        && !(eng.flushed && eng.rebase_pending) // coalesce: don't stack in-place seeks
        && eng.stage >= Stage::Playing
        && SHARED.duration_ns.load(Relaxed) > 0
        && (is_transcode || SHARED.file_size.load(Relaxed) > 0)
    {
        TX.seek_to_ns.store(-1, Relaxed);
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
                super::engine::reload_transcode(t);
            } else {
                super::log("seek(transcode): rebuild failed");
            }
            return;
        }
        // DIRECT-PLAY seek: reload the whole pipeline at t. A plain flush()+refeed leaves a
        // STALE GStreamer segment → the HW sink stops draining ~48 s later → permanent
        // BufferFull + "Playing error" (root-caused by decompiling libpipeline/lxvideosink;
        // see engine::reload_at). A fresh Load re-establishes a correct segment — the
        // known-good fresh-play path. reload_at REPLACES the ENGINE, so `eng` dangles after
        // it: return immediately and let the next pump() tick drive the fresh engine.
        // Direct-play (ff): Kodi IN-PLACE seek — flush + reopen the demuxer + av_seek, and
        // (in feed_stream, on the first post-flush keyframe) setTimeToDecode + sendSegmentEvent
        // to re-anchor the GStreamer segment WITHOUT a reload/decoder re-init (no A/V-resync
        // glitch). Falls back to reload-per-seek if the pipeline isn't reachable
        // (INPLACE_SEEK_OK cleared by feed_stream when sendSegmentEvent can't find it).
        if !is_transcode && crate::ff::use_ff() && !super::INPLACE_SEEK_OK.load(Relaxed) {
            super::engine::reload_at(t);
            return;
        }
        unsafe {
            ffi::sf_flush(); // drop decoded/queued frames
            ffi::sf_pause(); // freeze the clock; feed_stream Plays once PRIME_NS is buffered
        }
        drain_aq(eng);
        if crate::ff::use_ff() {
            // in-place ff direct-play seek: reopen the AVFormatContext on the same part URL +
            // av_seek to t (the demux outer loop reopens on next_url/seek_byte, av_seeks on
            // seek_to_ns). eng.flushed → feed_stream fires setTimeToDecode+sendSegmentEvent on
            // the first post-seek keyframe. disp_base=0 (rebase to the landed keyframe).
            eng.flushed = true;
            *SHARED.next_url.lock().unwrap() = Some(crate::route::url());
            SHARED.seek_byte.store(0, Release); // reopen trigger
            SHARED.seek_to_ns.store(t, Release); // post-reopen av_seek target
            SHARED.seek_target_ns.store(t, Relaxed); // rebase guard: reject stale drifted keyframes
            SHARED.disp_base.store(0, Relaxed);
            super::log(&format!("seek(ff in-place): reopen+seek t={t}"));
        } else {
            // legacy-mkv direct-play seek: byte-Range reopen at the Cue offset (fallback demuxer)
            let dur = SHARED.duration_ns.load(Relaxed);
            let fsz = SHARED.file_size.load(Relaxed);
            let byte = cue_byte_for(t) // accurate: MKV Cue index
                .unwrap_or_else(|| (t as f64 / dur as f64 * fsz as f64) as i64) // else CBR estimate
                .max(0);
            SHARED.seek_byte.store(byte, Release); // publish BEFORE the close
            super::log(&format!("seek: t={t} byte={byte}"));
        }
        let p = SHARED.hs_ptr.load(Acquire);
        if !p.is_null() {
            crate::stream::http_close(p); // unblock the demux read -> it re-opens
        }
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
        // legacy-mkv seek landing while already Streaming: its plane is bound to frames sf_flush
        // just dropped → "Playing error". Fall back to Bound so the pump re-sends
        // setMediaVideoData once post-seek frames decode. ff keeps the plane (sendSegmentEvent
        // re-anchors it, no rebind). (Transcode seeks reload above and never reach here.)
        if !crate::ff::use_ff() && eng.stage > Stage::Bound {
            eng.stage = Stage::Bound;
            eng.video_info_sent = false;
        }
        SHARED.frames.store(0, Relaxed); // count only POST-seek frames (rebind + resume re-pause gate)
        SHARED.playpos_ns.store(t, Relaxed); // displayed position jumps; wall clock takes over
    }

    // ---------- load -> Play (decode fed frames as soon as loaded) ----------
    if eng.stage == Stage::Loading
        && (SHARED.load_completed.load(Relaxed) || unsafe { ffi::sf_is_load_completed() } != 0)
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
            unsafe { ffi::sf_play() };
            super::log("SMP Play");
        }
    }

    // ---------- ACB bind, Kodi/ss4s order: setSinkType(MAIN)+setMediaId+setState(LOADED) ----------
    if eng.stage == Stage::Playing && ACB_OK.load(Relaxed) {
        let id = SHARED.media_id.lock().unwrap().clone();
        if let Some(id) = id {
            unsafe { ffi::acb_bind(id.as_ptr()) };
            super::log(&format!("SMP ACB bound id={}", id.to_string_lossy()));
            eng.stage = Stage::Bound;
        }
    }

    // ---------- send the WHOLE sourceInfo envelope VERBATIM, once frames flow, then
    // window + PLAYING (setMediaVideoData -> setDisplayWindow -> setState PLAYING) ----------
    if eng.stage == Stage::Bound && !eng.video_info_sent && SHARED.frames.load(Relaxed) >= 2 {
        let bytes = SHARED.source_info.lock().unwrap().clone();
        if let Some(bytes) = bytes {
            let rv = unsafe { ffi::acb_send_video_data(bytes.as_ptr() as *const c_char) };
            super::log(&format!("setMediaVideoData rv={rv} frames={}", SHARED.frames.load(Relaxed)));
            if rv != -1 {
                // -1 = client-side isJsonError reject; else accepted
                eng.video_info_sent = true;
                unsafe { ffi::acb_start(0, 0, 1920, 1080) };
                eng.stage = Stage::Streaming;
                super::log("setMediaVideoData sent → window+PLAYING");
            }
        }
    }

    // ---------- feed AUs once playing (Feed only succeeds after Play). NOT while a seek
    // is armed: on a resume the seek is armed before PLAYING, so feeding first would
    // present the file start for a frame before the seek repositions — a visible jump. ----------
    if eng.stage >= Stage::Playing && !TX.paused.load(Relaxed) && TX.seek_to_ns.load(Relaxed) < 0 {
        if stream {
            // Two-lane feed: VIDEO lane first — it owns the seek rebase (clears rebase_pending +
            // publishes pts_shift) — then the AUDIO lane, which sees that fresh shift the same tick.
            // A BufferFull in one lane no longer stalls the other. On the legacy mkv path aq_audio
            // is empty, so feed_audio_lane feeds nothing and feed_stream drains the mixed queue.
            feed_stream(eng);
            feed_audio_lane(eng);
        } else {
            feed_sample(eng);
        }
    }

    // ---------- end-of-stream: the producer hit file EOF (eos_pushed → all AUs to the end were
    // fed) and the pipeline has now played out to within EOS_TAIL of the duration. Mark ended so
    // app.rs tears the player down at the credits instead of freezing on the last frame. Paused
    // at the end stays paused (playpos won't climb) — correct; it fires when resumed. Any seek
    // clears the flag (request_seek), so seeking back from the end doesn't re-trigger. ----------
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
