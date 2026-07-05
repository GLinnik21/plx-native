//! player::pump — the main-thread pump (was bufferfeed_pump). Runs each frame from
//! plex_run: the pending-seek handler, the ACB-bind state machine (Stage), and the
//! feed dispatch. All ACB/Starfish control calls happen here on the main thread.
use super::engine::{cue_byte_for, drain_aq, engine, feed_sample, feed_stream, Source};
use super::shared::Stage;
use super::{ffi, ACB_OK, SHARED, TX};
use std::os::raw::c_char;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

pub(crate) fn pump(now: u32) {
    let _ = now;
    let eng = match engine() {
        Some(e) => e,
        None => return,
    };
    // wait for the media-thread ctor
    if eng.stage == Stage::Idle || unsafe { ffi::sf_ready() } == 0 {
        return;
    }
    let stream = matches!(eng.source, Source::Stream);

    // ---------- pending audio-track switch OR subtitle-burn refresh: force a fresh
    // transcode with the current audio + subtitle at the CURRENT position (same arm as a
    // transcode seek), and flag a full Track re-parse on re-open (the transcode output's
    // track numbering may differ from a direct-play file's, and mkv_seek_run does not
    // re-parse Tracks). switch_audio already carries the current subtitle, so an audio
    // switch also (re)burns it; a pure subtitle change uses retranscode. ----------
    let asid = SHARED.pending_audio_sid.swap(-1, Relaxed);
    let refresh = SHARED.pending_retranscode.swap(false, Relaxed);
    if stream && (asid >= 0 || refresh) && eng.stage >= Stage::Playing {
        let secs = (SHARED.playpos_ns.load(Relaxed) / 1_000_000_000).max(0);
        let rebuilt = if asid >= 0 { crate::route::switch_audio(asid, secs) } else { crate::route::retranscode(secs) };
        if let Some(url) = rebuilt {
            unsafe {
                ffi::sf_flush();
                ffi::sf_set_playtime(0);
                ffi::sf_play();
            }
            drain_aq(eng);
            *SHARED.next_url.lock().unwrap() = Some(url);
            SHARED.seek_byte.store(0, Release); // fresh stream from byte 0
            SHARED.disp_base.store(secs * 1_000_000_000, Relaxed);
            SHARED.reparse_next.store(true, Release);
            let p = SHARED.hs_ptr.load(Acquire);
            if !p.is_null() {
                crate::stream::http_close(p);
            }
            eng.rebase_pending = true;
            eng.max_fed_pts = 0;
            SHARED.frames.store(0, Relaxed);
            SHARED.playpos_ns.store(secs * 1_000_000_000, Relaxed);
            super::log(&format!("re-transcode: asid={asid} refresh={refresh} offset={secs}s"));
        }
    }

    // ---------- pending seek: flush, drop queued AUs, re-point the demux ----------
    let t = TX.seek_to_ns.load(Relaxed);
    // A live transcode has NO Content-Length / byte-Cues (file_size stays -1), so gate
    // it on duration only and seek by RESTARTING the transcode at a time &offset (below),
    // not a byte offset. Direct-play keeps the byte-Range path.
    let is_transcode = !crate::route::transcode_session().is_empty();
    if stream
        && t >= 0
        && eng.stage >= Stage::Playing
        && SHARED.duration_ns.load(Relaxed) > 0
        && (is_transcode || SHARED.file_size.load(Relaxed) > 0)
    {
        TX.seek_to_ns.store(-1, Relaxed);
        let t = t.max(0);
        unsafe {
            ffi::sf_flush(); // drop decoded/queued frames; resets the clock to ~0
            ffi::sf_set_playtime(0);
            ffi::sf_play(); // resume presentation after the flush
        }
        drain_aq(eng);
        if is_transcode {
            // restart the transcode at &offset=SECS; re-point the demux at the new start.mkv
            // (opened from byte 0). The fed timeline rebases on the first keyframe as usual;
            // the new stream is 0-based at content=SECS, so add SECS back for the displayed
            // position (integer seconds — the granularity the server's fastSeek honors).
            let secs = t / 1_000_000_000;
            match crate::route::transcode_seek(secs) {
                Some(url) => {
                    *SHARED.next_url.lock().unwrap() = Some(url);
                    SHARED.seek_byte.store(0, Release); // fresh stream from byte 0
                    SHARED.disp_base.store(secs * 1_000_000_000, Relaxed);
                    super::log(&format!("seek(transcode): t={t} offset={secs}s"));
                }
                None => super::log("seek(transcode): rebuild failed"),
            }
        } else {
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
        eng.max_fed_pts = 0;
        SHARED.frames.store(0, Relaxed); // count only POST-seek frames (resume re-pause gate)
        SHARED.playpos_ns.store(t, Relaxed); // displayed position jumps; wall clock takes over
    }

    // ---------- load -> Play (decode fed frames as soon as loaded) ----------
    if eng.stage == Stage::Loading
        && (SHARED.load_completed.load(Relaxed) || unsafe { ffi::sf_is_load_completed() } != 0)
    {
        SHARED.load_completed.store(true, Relaxed);
        super::log("SMP loadCompleted");
        unsafe { ffi::sf_play() };
        eng.stage = Stage::Playing;
        super::log("SMP Play");
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
            feed_stream(eng);
        } else {
            feed_sample(eng);
        }
    }
}
