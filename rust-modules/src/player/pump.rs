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

    // ---------- pending seek: flush, drop queued AUs, tell demux to re-open at byte ----------
    let t = TX.seek_to_ns.load(Relaxed);
    if stream
        && t >= 0
        && eng.stage >= Stage::Playing
        && SHARED.file_size.load(Relaxed) > 0
        && SHARED.duration_ns.load(Relaxed) > 0
    {
        TX.seek_to_ns.store(-1, Relaxed);
        let t = t.max(0);
        unsafe {
            ffi::sf_flush(); // drop decoded/queued frames; resets the clock to ~0
            ffi::sf_set_playtime(0);
            ffi::sf_play(); // resume presentation after the flush
        }
        drain_aq(eng);
        let dur = SHARED.duration_ns.load(Relaxed);
        let fsz = SHARED.file_size.load(Relaxed);
        let byte = cue_byte_for(t) // accurate: MKV Cue index
            .unwrap_or_else(|| (t as f64 / dur as f64 * fsz as f64) as i64) // else CBR estimate
            .max(0);
        SHARED.seek_byte.store(byte, Release); // publish BEFORE the close
        let p = SHARED.hs_ptr.load(Acquire);
        if !p.is_null() {
            crate::stream::http_close(p); // unblock the demux read -> it re-opens at byte
        }
        // zero-base the fed timeline on the first post-seek keyframe (feed_stream), so it
        // presents against the flush-reset clock immediately — no catch-up freeze
        eng.rebase_pending = true;
        eng.max_fed_pts = 0;
        SHARED.frames.store(0, Relaxed); // count only POST-seek frames (resume re-pause gate)
        SHARED.playpos_ns.store(t, Relaxed); // displayed position jumps; wall clock takes over
        super::log(&format!("seek: t={t} byte={byte}"));
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
