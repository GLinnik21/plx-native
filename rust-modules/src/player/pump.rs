//! player::pump — the main-thread pump (was bufferfeed_pump). Runs each frame from
//! plex_run: the pending-seek handler, the ACB-bind state machine (Stage), and the
//! feed dispatch. All ACB/Starfish control calls happen here on the main thread.
use super::engine::{cue_byte_for, drain_aq, engine, feed_sample, feed_stream, Engine, Source};
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
                    subs_reopen(eng, secs); // re-point the soft-subs sidecar at the new offset
                    super::log(&format!("seek(transcode): t={t} offset={secs}s"));
                }
                None => super::log("seek(transcode): rebuild failed"),
            }
        } else if crate::ff::use_ff() {
            // in-place ff direct-play seek: reopen the AVFormatContext on the same part URL +
            // av_seek to t (the demux outer loop reopens on next_url/seek_byte, av_seeks on
            // seek_to_ns). eng.flushed → feed_stream fires setTimeToDecode+sendSegmentEvent on
            // the first post-seek keyframe. disp_base=0 (rebase to the landed keyframe).
            eng.flushed = true;
            *SHARED.next_url.lock().unwrap() = Some(crate::route::url());
            SHARED.seek_byte.store(0, Release); // reopen trigger
            SHARED.seek_to_ns.store(t, Release); // post-reopen av_seek target
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
        eng.max_fed_pts = 0;
        // legacy-mkv seek landing while already Streaming: its plane is bound to frames sf_flush
        // just dropped → "Playing error". Fall back to Bound so the pump re-sends
        // setMediaVideoData once post-seek frames decode. ff keeps the plane (sendSegmentEvent
        // re-anchors it, no rebind); transcode reloads via next_url.
        if !is_transcode && !crate::ff::use_ff() && eng.stage > Stage::Bound {
            eng.stage = Stage::Bound;
            eng.video_info_sent = false;
        }
        SHARED.frames.store(0, Relaxed); // count only POST-seek frames (rebind + resume re-pause gate)
        SHARED.playpos_ns.store(t, Relaxed); // displayed position jumps; wall clock takes over
    }

    // reconcile the soft-subs sidecar (spawn / re-point / stop) from subs_want_sid — after
    // the seek/retranscode arms so a same-tick offset change is already reflected in disp_base
    reconcile_soft_subs(eng);

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

/// Re-point a RUNNING soft-subs sidecar at `secs` on the same transcode session (used from
/// the transcode-seek and audio-switch/retranscode arms). Publish the new-offset subtitles
/// URL + clear stale cues + close hs3 to unblock the read — the thread re-opens on it.
/// Called AFTER disp_base is stored, so the first post-seek cue is rebased correctly.
fn subs_reopen(eng: &mut Engine, secs: i64) {
    if eng.subs_th.is_some() && eng.subs_active_sid > 0 {
        if let Some(surl) = crate::route::transcode_subtitles_url(eng.subs_active_sid, secs) {
            *SHARED.subs_next_url.lock().unwrap() = Some(surl);
            SHARED.sub_cues.lock().unwrap().clear();
            let p = SHARED.hs3_ptr.load(Acquire);
            if !p.is_null() {
                crate::stream::http_close(p);
            }
        }
    }
}

/// Drive the soft-WebVTT subtitle thread from SHARED.subs_want_sid vs eng.subs_active_sid:
/// spawn it when a sub is selected during a transcode, re-point it on a track switch, stop
/// it on Off / switch-to-direct-play / not-transcoding.
fn reconcile_soft_subs(eng: &mut Engine) {
    let is_transcode = !crate::route::transcode_session().is_empty();
    let want = if is_transcode { SHARED.subs_want_sid.load(Relaxed) } else { 0 };
    // the sidecar MUST use the video session's offset (disp_base), NOT the playpos: cue
    // alignment is store_ns = vtt_ns + disp_base with vtt_ns = content − offset, so the
    // subs offset must equal the video's offset for store_ns to land on content time.
    let off_secs = (SHARED.disp_base.load(Relaxed) / 1_000_000_000).max(0);

    // OFF (sub=Off, or not transcoding): stop the thread, drop cues.
    if want == 0 {
        if eng.subs_th.is_some() {
            SHARED.subs_abort.store(true, Release);
            let p = SHARED.hs3_ptr.load(Acquire);
            if !p.is_null() {
                crate::stream::http_close(p); // interrupt the blocked recv
            }
            if let Some(t) = eng.subs_th.take() {
                let _ = t.join();
            }
            SHARED.subs_abort.store(false, Release);
            SHARED.sub_cues.lock().unwrap().clear();
            eng.subs_active_sid = 0;
        }
        return;
    }

    // ON, not running yet: spawn on the current session at the current offset.
    if eng.subs_th.is_none() {
        if let Some(url) = crate::route::transcode_subtitles_url(want, off_secs) {
            let (h, p, pa) = super::engine::parse_stream_url(&url);
            let hs3_raw = &mut *eng.hs3 as *mut crate::stream::HttpStream;
            SHARED.hs3_ptr.store(hs3_raw, Release);
            SHARED.subs_abort.store(false, Release);
            let hs3p = super::threads::SendPtr(hs3_raw);
            eng.subs_th = Some(std::thread::spawn(move || super::threads::subs_thread(h, p, pa, hs3p)));
            eng.subs_active_sid = want;
        }
        return;
    }

    // ON, running, but the user switched to a DIFFERENT track: re-point (like a seek).
    if eng.subs_active_sid != want {
        if let Some(url) = crate::route::transcode_subtitles_url(want, off_secs) {
            *SHARED.subs_next_url.lock().unwrap() = Some(url);
            SHARED.sub_cues.lock().unwrap().clear();
            let p = SHARED.hs3_ptr.load(Acquire);
            if !p.is_null() {
                crate::stream::http_close(p);
            }
            eng.subs_active_sid = want;
        }
    }
}
