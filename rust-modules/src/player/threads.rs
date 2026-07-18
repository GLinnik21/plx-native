//! player::threads — the worker threads beside the demuxer (load / timeline) + the
//! SendPtr spawn seam. The demux thread body is `ff::demux` (libavformat over a custom
//! AVIO on stream.rs); its HttpStream box lives in the Engine (main owns it, closes it
//! to interrupt, and outlives the threads).
use super::SHARED;
use std::os::raw::{c_char, c_int};
use std::sync::atomic::Ordering;

/// raw ptr we assert is Send for the spawn (the boxes/queue outlive the thread).
pub(crate) struct SendPtr<T>(pub *mut T);
unsafe impl<T> Send for SendPtr<T> {}

/// media/load thread: construct + Load (uid=NULL). The library owns its own
/// GMainContext + loop, so Load returns quickly and callbacks arrive on its thread.
pub(crate) fn load_thread(payload: SendPtr<c_char>) {
    super::log("SMP: calling Load (uid=NULL)");
    let ok = unsafe { super::ffi::sf_load(payload.0) };
    super::log(&format!("SMP: Load returned ok={ok}"));
}

/// progress-reporter thread: every ~10s, POST the current position to Plex's
/// /:/timeline so the server updates viewOffset (the resume point) + watched state.
/// `rk` is captured at spawn (fixed per playback session, no static-mut race). Exits
/// when SHARED.report_stop is set; the final state=stopped report is sent by
/// stop_bufferfeed (main thread) with the last position.
pub(crate) fn timeline_thread(host: String, port: c_int, token: String, rk: String) {
    loop {
        // sleep ~10s in 1s steps so we exit promptly on teardown
        for _ in 0..10 {
            if SHARED.report_stop.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        if SHARED.report_stop.load(Ordering::Acquire) {
            return;
        }
        let dur = SHARED.duration_ns.load(Ordering::Relaxed);
        if dur <= 0 || rk.is_empty() {
            continue;
        }
        let t = SHARED.playpos_ns.load(Ordering::Relaxed) / 1_000_000;
        let d = dur / 1_000_000;
        let state = if super::TX.paused.load(Ordering::Relaxed) { "paused" } else { "playing" };
        let path = timeline_path(&rk, state, t, d, &token);
        let _ = crate::stream::http_post(&host, port, &path, None);
        super::log(&format!("timeline {state} t={}s/{}s", t / 1000, d / 1000));
    }
}

/// Build the POST /:/timeline query string (the spec verb): identity + session + PlayQueue +
/// the SELECTED audio/subtitle stream ids, so /status/sessions shows the right track and the
/// Direct Play vs Transcode badge (correlated by X-Plex-Session-Identifier == transcode session=).
pub(crate) fn timeline_path(rk: &str, state: &str, t_ms: i64, d_ms: i64, token: &str) -> String {
    let sess = crate::route::sess();
    let (pq, pqi) = (crate::route::pq_id(), crate::route::pq_item_id());
    let (a, s) = (crate::route::cur_audio_sid(), crate::route::cur_sub_sid());
    let mut p = format!(
        "/:/timeline?ratingKey={rk}&key=%2Flibrary%2Fmetadata%2F{rk}\
         &identifier=com.plexapp.plugins.library&state={state}&time={t_ms}&duration={d_ms}\
         &X-Plex-Session-Identifier={sess}{id}&X-Plex-Token={token}",
        id = crate::route::identity_qs()
    );
    if !pq.is_empty() {
        p.push_str(&format!("&playQueueID={pq}"));
    }
    if !pqi.is_empty() {
        p.push_str(&format!("&playQueueItemID={pqi}"));
    }
    if a > 0 {
        p.push_str(&format!("&audioStreamID={a}"));
    }
    if s > 0 {
        p.push_str(&format!("&subtitleStreamID={s}"));
    }
    p
}

