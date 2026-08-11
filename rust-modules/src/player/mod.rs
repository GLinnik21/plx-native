//! player — the buffer-feed video engine (was src/playback.c). THREADING: everything
//! here except sf_on_event/acb_on_event runs on the SDL main thread. Those two are
//! #[no_mangle] and run on the StarfishMediaAPIs library thread; they touch ONLY
//! `SHARED`. All other cross-thread state is in shared.rs (atomics + Mutex); the
//! Engine (engine.rs) is main-thread-confined. Design: docs/engine-port-design.md.
//!
//! "Runs on the SDL main thread" is a **compile error to violate** for the two things where it
//! matters — the ACB/Starfish seam and the `ENGINE` slot. Both take a [`MainThread`] token,
//! which `plex_run` mints once and passes down; it is `!Send`, so a closure that captured one
//! cannot be handed to `task::spawn`. The exceptions are the honest ones: the two callbacks
//! above are `extern "C"` entry points *from* the library thread and touch only `SHARED`, and
//! `threads::load_thread` calls `sf_load` off-main by design (see `ffi`).
#![allow(non_upper_case_globals)]
pub(crate) mod engine;
mod ffi;
mod pump;
mod shared;
pub(crate) mod threads;

use crate::task::MainThread;
use shared::{Shared, SubBitmap, SubCue, Transport};
/// one rect of an image-subtitle display set — the demuxer builds them, the HUD draws them
pub(crate) use shared::SubRect;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_long};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::Relaxed};

pub(crate) static SHARED: Shared = Shared::new();
pub(crate) static TX: Transport = Transport::new();
static ACB_OK: AtomicBool = AtomicBool::new(false); // was the g_acb availability flag
// Kodi in-place seek (flush + reopen + re-anchor the decode position + sendSegmentEvent, NO
// reload/decoder re-init → no HDR-mode popup, no A/V-resync glitch). On webOS<11 (this 4.5)
// setTimeToDecode returns 0, so feed_stream falls back to the content-info path
// (loadSpi_getInfo + setContentInfo(ptsToDecode) — the same path the official app uses).
// Cleared to false if the pipeline can't be reached (sf_send_segment == 0), which drops seeks
// back to the robust reload-per-seek path.
//
// SCOPE: this is a **per-session probe, not a device-capability latch**, and
// `engine::start_bufferfeed` re-arms it to true for every new session. What `sf_send_segment`
// reports is whether `sf_pipeline()` could reach the CustomPipeline behind the CURRENT
// StarfishMediaAPIs object: `SMP_READY()` (constructed by `sf_load`, cleared by `sf_destroy`)
// plus two non-null shared_ptr hops, `g_smp+0x4c` -> `player+0x04` (src/starfish.c). Every one
// of those is a property of the live object this session builds and teardown destructs; none of
// them says anything about what this panel's pipeline can do. `sendSegmentEvent` itself returns
// void, so a 0 here NEVER means "the segment event was rejected", only "there was nothing to
// call it on" — a liveness/timing condition by construction.
// Latching it for the process was therefore a bug with a very long tail: one teardown-window
// race downgraded every later seek of every later item to a ~1 s reload until the app was
// restarted. Re-arming per session is self-healing rather than oscillating, too: the fallback
// the clear selects (`reload_at`) builds a fresh Starfish object, so the exact condition that
// produced the 0 cannot survive into the session that re-arms — and if a fresh session really
// can't reach its pipeline either, it re-clears after one seek and stays on the reload path.
// It remains a static rather than an `Engine` field only because `pump.rs` reads it without an
// Engine borrow at hand; its LIFETIME is the Engine's, since `start_bufferfeed` is the sole
// constructor and the flag is only ever read while a session is live.
pub(crate) static INPLACE_SEEK_OK: AtomicBool = AtomicBool::new(true);
static PTYPE: AtomicI32 = AtomicI32::new(10); // g_ptype (PLAYER_TYPE_MSE)

// ---- API app.rs calls (were extern "C" fns in playback.h) ----
pub(crate) use engine::{acb_init, resume_at, start_bufferfeed, stop_bufferfeed, suspend_bufferfeed};
pub(crate) use pump::pump;
pub(crate) use shared::PlaybackState;
pub(crate) fn pause(mt: &MainThread) {
    unsafe { ffi::sf_pause(mt); }
    acb_mirror_playstate(mt, false);
} // playback_pause
pub(crate) fn resume(mt: &MainThread) {
    unsafe { ffi::sf_play(mt); }
    acb_mirror_playstate(mt, true);
} // playback_resume

/// Kodi parity: mirror the ACB PLAYSTATE on transport pause/resume (the pipeline Pause/Play alone
/// leaves the app-owned sink's ACB state stale). Only once the plane is bound — firing
/// setState(PAUSED/PLAYING) before setMediaId/LOADED would corrupt the bind ordering.
fn acb_mirror_playstate(mt: &MainThread, playing: bool) {
    if !ACB_OK.load(Relaxed) {
        return;
    }
    if !engine::engine(mt).is_some_and(|e| e.stage >= shared::Stage::Bound) {
        return;
    }
    unsafe {
        if playing {
            ffi::acb_resume(mt);
        } else {
            ffi::acb_pause(mt);
        }
    }
}

// ---- transport accessors app.rs / player_hud.rs call ----
pub(crate) fn is_started() -> bool { TX.started.load(Relaxed) }
pub(crate) fn playpos_ns() -> i64 { SHARED.playpos_ns.load(Relaxed) }
pub(crate) fn frames() -> i32 { SHARED.frames.load(Relaxed) }
/// True once this SESSION has presented at least one frame. Deliberately NOT `frames() > 0`: the
/// pump zeroes `frames` as part of applying a seek (`pump.rs`), so that expression reads "no
/// picture" for the whole of every seek. Cleared only by `reset_session` — i.e. by a real stop or
/// a reload, both of which do blank the video plane. See [`shared::Shared::seen_frame`].
pub(crate) fn seen_frame() -> bool { SHARED.seen_frame.load(Relaxed) }
pub(crate) fn duration_ns() -> i64 { SHARED.duration_ns.load(Relaxed) }
pub(crate) fn seek_pending() -> i64 { TX.seek_to_ns.load(Relaxed) }
/// true once the pipeline has drained to true end-of-stream (see pump's EOS check). app.rs polls
/// this to tear the player down at the credits.
pub(crate) fn ended() -> bool { SHARED.ended.load(Relaxed) }
pub(crate) fn request_seek(ns: i64) {
    SHARED.ended.store(false, Relaxed); // seeking back from the end un-ends the stream
    SHARED.seeking.store(true, Relaxed); // HUD: spinner + freeze the playhead until it lands
    SHARED.seek_display_ns.store(ns, Relaxed);
    TX.seek_to_ns.store(ns, Relaxed);
    // Count the request even though the target it carries may be overwritten before the pump
    // ever sees it — that overwrite IS the coalescing, and this is the only place it's countable.
    TX.seek_reqs.fetch_add(1, Relaxed);
}
/// true while a seek is resolving (request → reopen/reload → prime → Play): the HUD shows a
/// spinner and freezes the playhead at `seek_display_ns` instead of wobbling through the reopen.
pub(crate) fn loading() -> bool { state().is_busy() }
/// true only while the pipeline is actually presenting frames — not resolving, connecting,
/// buffering or seeking. app.rs gates the heartbeat's `pos=` field on this: on a **direct-play**
/// resume `resume_at` only arms the seek (it does not seed `playpos_ns`, unlike the transcode
/// branch), so the position reads 0 until the first decoded frame lands at the resume offset.
/// Logging that pre-roll 0 would show the harness a 0→600 step and read as 600s of "climb"
/// inside one second — a false PASS on `min_timeline_climb_s`.
pub(crate) fn is_playing() -> bool { matches!(state(), shared::PlaybackState::Playing) }
/// The derived playback state — the ONE thing the HUD renders from. See `PlaybackState`.
pub(crate) fn state() -> shared::PlaybackState {
    // Resolving is DERIVED here rather than stored: the pump owns `pb_state` but only runs once
    // an engine exists, which is false for the whole resolve window. Deriving in the one reader
    // keeps a single writer instead of poking the state in from the frame loop.
    if crate::route::play_pending() {
        return shared::PlaybackState::Resolving;
    }
    shared::PlaybackState::from_u8(SHARED.pb_state.load(Relaxed))
}
/// Test-only: drive the derived playback state, returning the previous raw value to restore.
///
/// `pb_state` is the pump's field and `shared` is a private module, so a host test that needs the
/// app in a given state — `app.rs`'s HUD-visibility pair, which pins the bug that made the `…` disc
/// unreachable while stalled — sets it through here rather than widening the module for a test.
/// Callers must hold `crate::testlock::serial()`: this is a crate global.
#[cfg(test)]
pub(crate) fn swap_state_for_test(s: shared::PlaybackState) -> u8 {
    let prev = SHARED.pb_state.load(Relaxed);
    SHARED.pb_state.store(s as u8, Relaxed);
    prev
}
#[cfg(test)]
pub(crate) fn restore_state_for_test(raw: u8) {
    SHARED.pb_state.store(raw, Relaxed);
}

// ---- diagnostics --------------------------------------------------------------------------

pub(crate) use engine::aq_caps;
pub(crate) use ffi::{VP_ACB, VP_EXPORTED, VP_NONE};

/// One consistent read of everything the on-screen diagnostics overlay shows (`ui::stats`).
///
/// A struct rather than twenty accessors for one reason: the panel must not tell a story that
/// never happened. Sampled field-by-field across a frame it could report "no frames" beside a
/// callback count taken 16 ms later, and the whole point is that a maintainer trusts the
/// photograph. One call, one instant, main thread.
///
/// Everything here is a MIRROR — see `Shared`'s diagnostics block. Nothing in the playback state
/// machine may read a `Diag` back.
///
/// `Default` is the never-started session — all zero, no window, nothing fed — which is both the
/// honest pre-playback reading and what the host tests build, since the real [`diag`] reaches
/// `starfish.c` symbols that do not exist on the dev Mac.
#[derive(Default)]
pub(crate) struct Diag {
    pub vp_mode: c_int,
    pub window_id: String,
    pub acb_ok: bool,
    pub place_rv: i32,
    pub placed_w: i32,
    pub placed_h: i32,
    pub stage: u8,
    pub load_completed: bool,
    pub load_failed: bool,
    pub cb_count: u32,
    pub pushed_any: bool,
    pub fed_v: i64,
    pub fed_a: i64,
    pub frames: i32,
    pub seen_frame: bool,
    pub aq_video: i64,
    pub aq_audio: i64,
    pub fed_v_pts: i64,
    pub fed_a_pts: i64,
    pub load_v: u8,
    pub load_a: u8,
    pub feed_state: u8,
    pub cb_err: i32,
    pub cb_err_at: u32,
    pub http_status: i32,
    pub net_rx: i64,
    pub load_at: u32,
    pub frame_at: u32,
    pub video_w: i32,
    pub video_h: i32,
    pub pos_ns: i64,
    pub dur_ns: i64,
}

impl Diag {
    pub fn vp_mode_str(&self) -> &'static str {
        match self.vp_mode {
            VP_EXPORTED => "exported window (webOS 5+)",
            VP_ACB => "ACB (webOS 4)",
            _ => "NONE — no video path",
        }
    }
    /// What the Load payload named as the video codec, or `—` before one was built.
    pub fn load_v_str(&self) -> &'static str {
        match self.load_v {
            1 => "H264",
            2 => "H265",
            _ => "—",
        }
    }
    /// …and the audio codec. `needAudio:false` is its own answer, not an absence.
    pub fn load_a_str(&self) -> &'static str {
        match self.load_a {
            1 => "AC3",
            2 => "AC3 PLUS",
            3 => "AAC",
            _ => "NONE (needAudio:false)",
        }
    }
    /// Why the VIDEO feeder is where it is. The video lane specifically: the picture is what a
    /// user complains about, and a two-lane string overflows the value column.
    ///
    /// `queue empty` vs `BufferFull` is the row's whole point — a dead PRODUCER and a dead SINK
    /// look identical from every other field on the panel and want opposite fixes.
    ///
    /// The throttle state is worded as what it IS, not as what it is waiting for. It was
    /// "waiting for a frame", which is the literal truth and reads as a stall — and it is the
    /// state a healthy playback sits in most of the time, because the feeder deliberately stays
    /// within `MAX_FEED_AHEAD_NS` of the presented position. The first person to see the panel in
    /// the wild asked why playback was stuck; it was not.
    pub fn feed_state_str(&self) -> &'static str {
        match self.feed_state {
            1 => "accepting",
            2 => "BufferFull (sink is full)",
            3 => "REFUSED",
            4 => "holding ~1.6 s ahead",
            5 => "queue empty (no data)",
            _ => "— nothing fed yet",
        }
    }
    /// Only an outright refusal is a fault. BufferFull is the steady state under the feed-ahead
    /// throttle, and the throttle and an empty queue are ordinary moments in a healthy stream.
    pub fn feed_is_fault(&self) -> bool {
        self.feed_state == 3
    }
}

pub(crate) fn diag() -> Diag {
    let (fed_v, fed_a) = engine::fed_totals();
    // `vp_window_id` hands back the seam's own static buffer — never NULL, "" when no window was
    // created — so this is a copy of a bounded char[64], not a borrow with a lifetime to reason about.
    let window_id = unsafe { std::ffi::CStr::from_ptr(ffi::vp_window_id()) }
        .to_string_lossy()
        .into_owned();
    Diag {
        vp_mode: ffi::vp_mode(),
        window_id,
        acb_ok: ACB_OK.load(Relaxed),
        place_rv: SHARED.dg_place_rv.load(Relaxed),
        placed_w: SHARED.dg_placed_w.load(Relaxed),
        placed_h: SHARED.dg_placed_h.load(Relaxed),
        stage: SHARED.dg_stage.load(Relaxed),
        load_completed: SHARED.load_completed.load(Relaxed),
        load_failed: SHARED.load_failed.load(Relaxed),
        cb_count: SHARED.dg_cb_count.load(Relaxed),
        pushed_any: crate::ff::pushed_any(),
        fed_v,
        fed_a,
        frames: SHARED.frames.load(Relaxed),
        seen_frame: SHARED.seen_frame.load(Relaxed),
        aq_video: SHARED.dg_aq_video.load(Relaxed),
        aq_audio: SHARED.dg_aq_audio.load(Relaxed),
        fed_v_pts: SHARED.dg_fed_v_pts.load(Relaxed),
        fed_a_pts: SHARED.dg_fed_a_pts.load(Relaxed),
        load_v: SHARED.dg_load_v.load(Relaxed),
        load_a: SHARED.dg_load_a.load(Relaxed),
        feed_state: SHARED.dg_feed_state.load(Relaxed),
        cb_err: SHARED.dg_cb_err.load(Relaxed),
        cb_err_at: SHARED.dg_cb_err_at.load(Relaxed),
        http_status: SHARED.dg_http_status.load(Relaxed),
        net_rx: SHARED.dg_net_rx.load(Relaxed),
        load_at: SHARED.dg_load_at.load(Relaxed),
        frame_at: SHARED.dg_frame_at.load(Relaxed),
        video_w: SHARED.video_w.load(Relaxed),
        video_h: SHARED.video_h.load(Relaxed),
        pos_ns: SHARED.playpos_ns.load(Relaxed),
        dur_ns: SHARED.duration_ns.load(Relaxed),
    }
}

pub(crate) fn seek_display_ns() -> i64 { SHARED.seek_display_ns.load(Relaxed) }
/// The playhead the user INTENDS, which is not always the one being published: while a seek is
/// still resolving (request → reopen → prime → Play) `playpos_ns` keeps reporting the PRE-seek
/// spot, so anything snapshotting "where are we?" inside that window snapshots the position the
/// user just left. The rule — an in-flight seek target wins, else the published position — used to
/// be open-coded at each reader that remembered it and was simply MISSING at the one that did not
/// (the OS-background save; see `app::intended_pos`). This is that rule, once.
///
/// Use it at every reader that means "where the user is". Keep the raw `playpos_ns` only where the
/// PUBLISHED position is the point: the re-pause gate (already behind `seek_pending() < 0`) and the
/// heartbeat's `pos=`, which `tests/run.py` grades real playback progress from — feeding it an
/// intended position would let a seek that never lands read as playback that climbed.
///
/// `ui/player_hud.rs` deliberately does NOT call this: it needs the same outer two rungs with the
/// live scrub preview between them, so its expression is a superset rather than a caller.
pub(crate) fn intended_pos_ns() -> i64 {
    let t = seek_display_ns();
    if loading() && t >= 0 { t } else { playpos_ns() }
}
/// request an audio-track switch (Plex audioStreamID); the pump forces a fresh
/// transcode with that source audio at the current position next tick.
pub(crate) fn request_audio_switch(sid: i64) {
    SHARED.pending_audio_sid.store(sid, Relaxed);
    SHARED.sub_cues.lock().unwrap().clear(); // the fresh transcode carries no embedded subs
}
/// request a NATIVE audio-track switch (direct-play, NO transcode): feed the 0-based `audio_idx`
/// audio stream from the same MKV with codec `codec`. The pump reloads direct-play at the current
/// position next tick (switch_audio_native). Used when the item direct-plays and the target track
/// is a direct-playable codec (aac/ac3/eac3).
pub(crate) fn request_audio_track(audio_idx: i32, codec: &str) {
    crate::route::set_stream_acodec(codec); // the reload's Load payload uses this audio codec
    SHARED.pending_audio_idx.store(audio_idx, Relaxed);
    SHARED.sub_cues.lock().unwrap().clear();
}
/// reset to the default (best) audio stream — called on a new item so a prior track choice
/// does not leak across items (desired_audio_idx persists across seeks, not across items).
pub(crate) fn reset_audio_track() {
    SHARED.desired_audio_idx.store(-1, Relaxed);
}
/// reset the subtitle selection to Off — called on a NEW item. Like desired_audio_idx, the
/// subtitle selection PERSISTS across seeks/reloads (it is no longer cleared in reset_session),
/// so a reload-based seek (transcode, or the direct-play reload fallback) keeps the chosen sub
/// instead of silently turning subtitles off.
pub(crate) fn reset_subtitle() {
    SHARED.desired_sub_idx.store(-1, Relaxed);
}
/// select the audio stream index the demuxer feeds at the FIRST Load (before start_bufferfeed) —
/// used by the decision to direct-play a non-default direct-playable track (e.g. an AC3 track on
/// a TrueHD-default item). -1 = default/best.
pub(crate) fn set_audio_track(idx: i32) {
    SHARED.desired_audio_idx.store(idx, Relaxed);
}
/// request a re-transcode at the current position with the CURRENT audio + subtitle —
/// used when a subtitle is (de)selected while already transcoding, so the server
/// re-burns (or drops) it. No-op-ish if not transcoding (the caller gates on that).
pub(crate) fn request_transcode_refresh() {
    SHARED.pending_retranscode.store(true, Relaxed);
    SHARED.sub_cues.lock().unwrap().clear(); // burned/absent in the fresh transcode
}

// ---- client-rendered subtitles (direct-play only; a transcode carries no subs) ----
/// selected subtitle track index (-1 = off); the demuxer reads this per block.
pub(crate) fn desired_sub_idx() -> i32 { SHARED.desired_sub_idx.load(Relaxed) }
/// select a subtitle track by index (-1 = off). Does NOT clear the cue store: the demuxer
/// pushes cues for EVERY text track regardless of selection, so the buffered region's cues for
/// the newly-selected track are already present and the switch shows immediately. Clearing here
/// would reintroduce the ~10-20s buffer-gap delay (the demuxer runs well ahead of the playhead).
/// A new item / transcode re-point clears the store via reset_session / the pump.
pub(crate) fn request_subtitle(idx: i32) {
    SHARED.desired_sub_idx.store(idx, Relaxed);
    if idx < 0 {
        // subs Off: free the image-cue RGBA store now (the demuxer also stops decoding new
        // bitmap cues while off — see ff.rs's desired_sub_idx gate)
        SHARED.sub_bitmaps.lock().unwrap().clear();
    }
}
/// push a ready (already-clean) subtitle cue into the shared store, tagged with its 0-based
/// track index (the demux pushes for every text track).
/// Bounded by TIME rather than a fixed count: since every track is pushed regardless of
/// selection, drop cues already well behind the playhead and keep a generous forward window
/// (the demuxer reads ~10-20s ahead). A hard cap guards against a runaway.
pub(crate) fn push_subtitle_text(track: i32, start_ns: i64, end_ns: i64, text: String) {
    if text.is_empty() {
        return;
    }
    let mut cues = SHARED.sub_cues.lock().unwrap();
    let floor = SHARED.playpos_ns.load(Relaxed) - 2_000_000_000;
    cues.retain(|c| c.end_ns >= floor);
    if cues.len() >= 512 {
        cues.remove(0);
    }
    cues.push(SubCue { track, start_ns, end_ns, text });
}
/// demux (D-thread) pushes a subtitle cue (content-time ns) for track `track`. Called for
/// EVERY text track so a mid-play switch is instant; only the selected track's cues are logged.
pub(crate) fn push_subtitle_cue(track: i32, start_ns: i64, end_ns: i64, payload: &[u8], is_ass: bool) {
    let text = sub_text(payload, is_ass);
    if text.is_empty() {
        return;
    }
    if track == SHARED.desired_sub_idx.load(Relaxed) {
        log(&format!("sub cue [{}..{}ms] {:?}", start_ns / 1_000_000, end_ns / 1_000_000,
            text.chars().take(34).collect::<String>()));
    }
    push_subtitle_text(track, start_ns, end_ns, text);
}
/// the selected track's subtitle text active at `now_ns`, or None (also None when off).
pub(crate) fn active_subtitle(now_ns: i64) -> Option<String> {
    let sel = SHARED.desired_sub_idx.load(Relaxed);
    if sel < 0 {
        return None;
    }
    let cues = SHARED.sub_cues.lock().unwrap();
    cues.iter()
        .rev()
        .find(|c| c.track == sel && now_ns >= c.start_ns && now_ns < c.end_ns)
        .map(|c| c.text.clone())
}

/// Image-subtitle store (PGS/VobSub). The demux (D) thread decodes the SELECTED track's
/// bitmaps and pushes them here; the renderer (M) reads the active one for the playpos. A new
/// display-set supersedes any still-open cue on the same track (PGS signals the end via a later
/// CLEAR or a superseding set, both handled here). Bounded by time like the text store.
///
/// `cw`/`ch` are the stream's authoring canvas (0 = the decoder never declared one) and every
/// rect's coords are relative to it — the renderer scales the whole set into the video rect, so
/// a 720×480 VobSub and a 1920×1080 PGS land the same size on screen.
pub(crate) fn push_subtitle_bitmap(track: i32, start_ns: i64, cw: i32, ch: i32, rects: Vec<SubRect>) {
    if rects.is_empty() {
        return;
    }
    let mut v = SHARED.sub_bitmaps.lock().unwrap();
    for c in v.iter_mut() {
        if c.track == track && c.end_ns == i64::MAX {
            c.end_ns = start_ns; // this set replaces the one still showing
        }
    }
    let floor = SHARED.playpos_ns.load(Relaxed) - 2_000_000_000;
    v.retain(|c| c.end_ns >= floor);
    v.push(SubBitmap { track, start_ns, end_ns: i64::MAX, cw, ch, rects });
    // Hard RAM ceiling: decoding ALL image tracks means several are buffered at once, so bound
    // the store by total RGBA bytes (not count). ~24 MB is comfortable headroom on the direct-play
    // path. A multi-rect display set counts as the sum of its rects, which is why the budget is
    // bytes and not cue count — and which is what made the eviction ORDER start to matter.
    //
    // `v` is in demux (increasing-pts) order and the time-retain above has already dropped
    // everything more than 2s behind the playhead, so `v[0]` is the cue AT or just behind the
    // playhead — the one about to be drawn — while the tail is the demuxer's 10-20s read-ahead.
    // Evicting index 0 (what this did) therefore blanks the subtitle the viewer is reading and
    // keeps cues they have not reached. So: drop a cue the playhead has already passed first,
    // since it can never be shown again; only when none is left does the FAR END of the
    // read-ahead go, because that cue is at least not on screen yet.
    const BUDGET: usize = 24 * 1024 * 1024;
    let mut total: usize = v.iter().map(|c| c.bytes()).sum();
    let now = SHARED.playpos_ns.load(Relaxed);
    while total > BUDGET && v.len() > 1 {
        let i = v.iter().position(|c| c.end_ns <= now).unwrap_or(v.len() - 1);
        total -= v[i].bytes();
        v.remove(i);
    }
}
/// A CLEAR display-set (num_rects==0): close the currently-open cue on this track at `end_ns`.
pub(crate) fn close_subtitle_bitmap(track: i32, end_ns: i64) {
    let mut v = SHARED.sub_bitmaps.lock().unwrap();
    for c in v.iter_mut() {
        if c.track == track && c.end_ns == i64::MAX {
            c.end_ns = end_ns;
        }
    }
}
/// Cheap per-frame lookup: the `start_ns` key of the selected track's image cue active at
/// `now_ns`, or None. The renderer only re-uploads its GL texture when this key changes.
pub(crate) fn active_bitmap_key(now_ns: i64) -> Option<i64> {
    let sel = SHARED.desired_sub_idx.load(Relaxed);
    if sel < 0 {
        return None;
    }
    let v = SHARED.sub_bitmaps.lock().unwrap();
    v.iter()
        .rev()
        .find(|c| c.track == sel && now_ns >= c.start_ns && now_ns < c.end_ns)
        .map(|c| c.start_ns)
}
/// Fetch (canvas_w, canvas_h, rects) for the selected track's display set with this `start_ns`
/// key. Clones the bitmaps once (only when the renderer sees a new key), so the per-frame path
/// stays cheap.
pub(crate) fn bitmap_by_key(key: i64) -> Option<(i32, i32, Vec<SubRect>)> {
    let sel = SHARED.desired_sub_idx.load(Relaxed);
    let v = SHARED.sub_bitmaps.lock().unwrap();
    v.iter()
        .rev()
        .find(|c| c.track == sel && c.start_ns == key)
        .map(|c| (c.cw, c.ch, c.rects.clone()))
}
/// extract displayable text from a subtitle block (SRT = raw UTF-8; ASS = the field
/// after the 8th comma), stripping tags/override codes and normalizing line breaks.
fn sub_text(payload: &[u8], is_ass: bool) -> String {
    let raw = String::from_utf8_lossy(payload);
    let s = if is_ass {
        raw.splitn(9, ',').nth(8).unwrap_or("").to_string()
    } else {
        raw.into_owned()
    };
    let mut out = String::with_capacity(s.len());
    let mut ch = s.chars().peekable();
    while let Some(c) = ch.next() {
        match c {
            '<' => while let Some(x) = ch.next() { if x == '>' { break; } },   // <i></i>
            '{' => while let Some(x) = ch.next() { if x == '}' { break; } },   // {\an8}
            '\\' => match ch.peek() {
                Some('N') | Some('n') => { ch.next(); out.push('\n'); }
                Some('h') => { ch.next(); out.push(' '); }
                _ => out.push('\\'),
            },
            '\r' => {}
            _ => out.push(c),
        }
    }
    out.trim().to_string()
}

pub(crate) use crate::log; // event-log sink (crate-wide single copy in lib.rs)

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

/// pipeline event on the LIBRARY thread. type 0 = frame presented (num = fed pts).
/// Panic-guarded (unwinding into C is UB); touches only SHARED.
#[no_mangle]
pub extern "C" fn sf_on_event(ty: c_int, num: i64, s: *const c_char) {
    let _ = catch_unwind(AssertUnwindSafe(|| sf_on_event_inner(ty, num, s)));
}
fn sf_on_event_inner(ty: c_int, num: i64, s: *const c_char) {
    if ty != 0 {
        // The diagnostics census, beside the log line that already records every event. A COUNT is
        // what the read-out needs: "Load completed and then nothing ever called us" is the sharpest
        // symptom the stuck-buffering reports could carry, and it is invisible in a log the user
        // cannot reach. Kept here rather than in the `ty` dispatch below so an event we do not
        // handle still counts — an unhandled callback is still the pipeline talking.
        let n = SHARED.dg_cb_count.fetch_add(1, Relaxed) + 1;
        // Latch the FIRST error, with the callback index it arrived at. Sticky, because a later
        // healthy callback must not erase the one event that explains the session — and the index
        // separates "refused immediately" from "died after a long healthy run". Only `ty == 18`:
        // it is the one value this project has ever acted on, and it sits below the 0x1c point
        // where the numbering shifts between webOS 4 and 5+, so it means the same on both. Naming
        // any higher type would be a confident lie on the firmware we cannot test.
        if ty == 18 && SHARED.dg_cb_err.load(Relaxed) == 0 {
            SHARED.dg_cb_err.store(ty, Relaxed);
            SHARED.dg_cb_err_at.store(n, Relaxed);
        }
        let preview = if s.is_null() { String::new() } else {
            unsafe { CStr::from_ptr(s) }.to_string_lossy().chars().take(1400).collect()
        };
        log(&format!("smp_cb type={ty} num={num} str={preview}"));
    }
    if ty == 0 {
        // a frame was PRESENTED — map fed pts -> real content position
        SHARED.frames.fetch_add(1, Relaxed);
        SHARED.seen_frame.store(true, Relaxed); // session-scoped: unlike `frames`, a seek won't clear it
        SHARED.pres_fed.store(num, Relaxed); // raw fed pts, for the feed-ahead throttle
        SHARED.playpos_ns
            .store(num - SHARED.pts_shift.load(Relaxed) + SHARED.disp_base.load(Relaxed), Relaxed);
    }
    if s.is_null() {
        return;
    }
    let b = unsafe { CStr::from_ptr(s) }.to_bytes();

    {
        let mut mid = SHARED.media_id.lock().unwrap();
        if mid.is_none() {
            if let Some(id) = between(b, b"\"context\":\"", b'"').or_else(|| between(b, b"\"mediaId\":\"", b'"')) {
                if let Ok(c) = std::ffi::CString::new(id.clone()) {
                    log(&format!("SMP context/mediaId={}", String::from_utf8_lossy(&id)));
                    *mid = Some(c);
                }
            }
        }
    }

    if !SHARED.load_completed.load(Relaxed) && (find(b, b"loadCompleted") || find(b, b"\"loaded\"")) {
        SHARED.load_completed.store(true, Relaxed);
        log("SMP loadCompleted");
    }

    {
        // capture the WHOLE sourceInfo envelope VERBATIM (byte-for-byte + NUL), never re-encoded
        let mut si = SHARED.source_info.lock().unwrap();
        if si.is_none() && find(b, b"\"video\":") && find(b, b"\"context\":") {
            let mut v = Vec::with_capacity(b.len() + 1);
            v.extend_from_slice(b);
            v.push(0);
            log(&format!("SMP sourceInfoRaw captured ({} bytes)", b.len()));
            *si = Some(v);
        }
    }
}

#[no_mangle]
pub extern "C" fn acb_on_event(ev: c_long, reply: *const c_char) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let r = if reply.is_null() { String::new() } else {
            unsafe { CStr::from_ptr(reply) }.to_string_lossy().into_owned()
        };
        log(&format!("acb_cb ev={ev} reply={r}"));
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> SubRect {
        SubRect { x, y, w, h, rgba: vec![0u8; (w * h * 4) as usize] }
    }

    /// The image-subtitle store, exercised as a display SET rather than a single bitmap. Three
    /// invariants moved when multi-rect landed and none of them is observable on the host except
    /// here: every rect of a set survives the round trip under ONE key (so a two-line PGS cue is
    /// not silently halved); a later set still closes the one still showing; and the RAM ceiling
    /// counts a set's rects together, so a multi-rect cue cannot smuggle bytes past the budget.
    ///
    /// Takes the crate-wide `testlock` — `SHARED` is a process-global the whole player shares.
    #[test]
    fn an_image_display_set_round_trips_whole_and_is_superseded_as_a_unit() {
        let _g = crate::testlock::serial();
        SHARED.sub_bitmaps.lock().unwrap().clear();
        SHARED.playpos_ns.store(0, Relaxed);
        SHARED.desired_sub_idx.store(0, Relaxed);

        // a two-rect set (dialogue plus a sign), authored on a DVD canvas
        push_subtitle_bitmap(0, 1_000, 720, 480, vec![rect(60, 400, 600, 60), rect(100, 20, 200, 40)]);
        let key = active_bitmap_key(1_500).expect("the set should be active at its start");
        let (cw, ch, rects) = bitmap_by_key(key).expect("the active key must resolve");
        assert_eq!((cw, ch), (720, 480), "the authoring canvas travels with the set");
        assert_eq!(rects.len(), 2, "BOTH rects must survive — rect 0 only was the bug");
        assert_eq!((rects[1].x, rects[1].y), (100, 20));

        // the next set closes the open one AT ITS OWN START — a display set stays up until the
        // one that replaces it begins, so the handover is seamless and never double-shows
        push_subtitle_bitmap(0, 5_000, 720, 480, vec![rect(60, 400, 600, 60)]);
        assert_eq!(active_bitmap_key(4_999), Some(1_000), "the first set holds right up to the handover");
        assert_eq!(active_bitmap_key(5_000), Some(5_000), "and the second takes over on that exact ns");

        // an empty set is not a cue: it must not land and must not close what is showing
        push_subtitle_bitmap(0, 6_000, 720, 480, Vec::new());
        assert_eq!(active_bitmap_key(5_500), Some(5_000));

        // The byte budget charges a set for ALL its rects (2 x 4 MB here), and — the part that
        // only matters once a set can be big — it must not evict the cue the viewer is READING.
        // The playhead sits inside the 5_000 cue, so that one has to survive four 8 MB sets
        // arriving from the demuxer's read-ahead; what goes is the far end of that read-ahead.
        SHARED.playpos_ns.store(5_500, Relaxed);
        for i in 0..4 {
            push_subtitle_bitmap(0, 10_000 + i, 720, 480, vec![rect(0, 0, 1024, 1024), rect(0, 0, 1024, 1024)]);
        }
        let v = SHARED.sub_bitmaps.lock().unwrap();
        let total: usize = v.iter().map(|c| c.bytes()).sum();
        assert!(total <= 24 * 1024 * 1024, "the store stayed inside its ceiling ({total} bytes)");
        assert!(v.iter().any(|c| c.start_ns == 5_000), "the cue under the playhead was not evicted");
        drop(v);
        assert_eq!(active_bitmap_key(5_500), Some(5_000), "and it is still the one on screen");

        // leave the globals as they were found — `desired_sub_idx` deliberately survives a reset
        // (shared.rs), so a test that leaves it selected changes what the NEXT one sees
        SHARED.sub_bitmaps.lock().unwrap().clear();
        SHARED.desired_sub_idx.store(-1, Relaxed);
    }
}
