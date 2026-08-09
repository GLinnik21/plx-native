//! player::shared — the ONLY cross-thread state. Every field replaces a `volatile`
//! global from playback.c and is an atomic or a Mutex (never a bare value). One
//! long-lived `static SHARED` in mod.rs: it outlives every start/stop cycle and is
//! *reset*, never freed — so a late library callback after teardown writes to a
//! live object, exactly as the C static globals behaved.
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicPtr, AtomicU32, AtomicU8, Ordering};
use std::sync::Mutex;
use crate::stream::HttpStream;

/// one client-rendered subtitle cue (content-time ns). `track` is the 0-based subtitle-stream
/// index it belongs to; the demuxer pushes cues for ALL text tracks and the render filters by
/// the selected `desired_sub_idx`, so switching tracks is instant (no re-demux of the buffered
/// region). Content-time ns matches the fed video PTS timeline (see active_subtitle).
pub(crate) struct SubCue {
    pub track: i32,
    pub start_ns: i64,
    pub end_ns: i64,
    pub text: String,
}

/// one rect of a decoded image-subtitle display set. `rgba` is a straight-alpha bitmap of
/// `w`×`h` at position (`x`,`y`) **in the subtitle stream's own authoring canvas** (see
/// [`SubBitmap::cw`]) — NOT in screen pixels; the renderer scales it into the video rect.
#[derive(Clone)]
pub(crate) struct SubRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub rgba: Vec<u8>,
}

/// one decoded image-subtitle display set (PGS/VobSub/DVB) — every rect of it, not just the
/// first: a two-line dialogue or a sign-plus-dialogue set is authored as several rects and they
/// belong to the same on-screen moment. `end_ns` is i64::MAX until a CLEAR display-set (or a
/// superseding set) truncates it. Unlike text cues we push ONLY the selected track (bitmaps are
/// heavier than text on this RAM-tight TV), keyed by `start_ns` for the renderer.
pub(crate) struct SubBitmap {
    pub track: i32,
    pub start_ns: i64,
    pub end_ns: i64,
    /// authoring-canvas width/height the rects' coords are expressed in — 1920×1080 for
    /// Blu-ray PGS, 720×480/576 for a DVD VobSub rip, 3840×2160 for some 4K PGS. `0` = the
    /// decoder never declared one, in which case the renderer falls back to 1:1.
    pub cw: i32,
    pub ch: i32,
    pub rects: Vec<SubRect>,
}
impl SubBitmap {
    /// total RGBA bytes held by this display set (what the store's byte budget counts)
    pub fn bytes(&self) -> usize {
        self.rects.iter().map(|r| r.rgba.len()).sum()
    }
}

/// What playback is actually doing, as one value the UI can render from.
///
/// This replaces the old single `seeking` boolean, which was only ever set by `request_seek` —
/// so `player::loading()` was **false for the whole initial load** and the `Spinner` that has sat
/// in `player_hud.rs` all along never fired on first play. The HUD drew a live-looking transport
/// at 0:00 / -0:00 instead, which is the half of the frozen-HUD report that is not blocking I/O.
///
/// Derived once per frame in `pump::set_state` from signals the workers already publish; no
/// new cross-thread plumbing. Ordered so `>= Playing` reads as "actually on screen".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum PlaybackState {
    /// no engine
    Idle = 0,
    /// the route/plan resolve is in flight — DERIVED in `player::state()` from `route::play_pending()`
    Resolving = 1,
    /// engine started, pipeline not yet loaded — the HTTP GET + Starfish `Load` window
    Connecting = 2,
    /// loaded and fed, but no frame has been presented yet
    Buffering = 3,
    /// a seek is resolving (reopen → prime → Play)
    Seeking = 4,
    /// frames on the panel
    Playing = 5,
    /// the producer died (a failed open / no video stream). Terminal until teardown.
    Error = 6,
}

impl PlaybackState {
    pub fn from_u8(v: u8) -> PlaybackState {
        match v {
            1 => PlaybackState::Resolving,
            2 => PlaybackState::Connecting,
            3 => PlaybackState::Buffering,
            4 => PlaybackState::Seeking,
            5 => PlaybackState::Playing,
            6 => PlaybackState::Error,
            _ => PlaybackState::Idle,
        }
    }
    /// "something is happening and the user should see a spinner, not a dead transport".
    pub fn is_busy(self) -> bool {
        matches!(
            self,
            PlaybackState::Resolving
                | PlaybackState::Connecting
                | PlaybackState::Buffering
                | PlaybackState::Seeking
        )
    }
    /// The one-line status the HUD shows beside the spinner. A `&'static CStr` (the house idiom
    /// for UI strings) so the draw path allocates nothing — this is read 60x/second for the whole
    /// load window, and indefinitely in `Error`.
    pub fn caption(self) -> &'static std::ffi::CStr {
        match self {
            PlaybackState::Resolving => c"Preparing…",
            PlaybackState::Connecting => c"Connecting…",
            PlaybackState::Buffering => c"Buffering…",
            PlaybackState::Seeking => c"Seeking…",
            PlaybackState::Error => c"Playback failed",
            _ => c"",
        }
    }
}

pub(crate) struct Shared {
    // library callback thread (K) -> main (M)
    pub playpos_ns: AtomicI64,               // g_playpos_ns
    // the presented frame's fed PTS (0-based, raw `num` from the type=0 callback). The feed
    // loop throttles on max_fed_pts - pres_fed so it stays ~MAX_FEED_AHEAD ahead of the
    // decoder (feeding further overfills the 4K HEVC DPB/CPB and stalls the sink).
    pub pres_fed: AtomicI64,
    pub frames: AtomicI32,                    // bf_frames
    /// **Has this SESSION ever put a picture on the panel?** Set by the frame-presented callback
    /// beside `frames`, cleared ONLY by [`Shared::reset_session`] — so it survives a seek, which
    /// `frames` deliberately does not: `pump` zeroes `frames` as *part of applying* a seek, which
    /// makes "we have never shown a frame" and "we just seeked" indistinguishable through it. The
    /// HUD's one rule for which surface owns the "pipeline is working" read-out keys on this bit;
    /// see `ui::player_hud::busy_surface`. Monotone within a session (false→true only), which is
    /// what lets two readers in one frame sample it independently without disagreeing.
    pub seen_frame: AtomicBool,
    pub load_completed: AtomicBool,           // bf_loaded signal
    pub media_id: Mutex<Option<CString>>,     // bf_mediaId (captured once)
    pub source_info: Mutex<Option<Vec<u8>>>,  // sourceInfoRaw, VERBATIM incl NUL

    // main/pump (M) -> library callback thread (K)
    pub pts_shift: AtomicI64,                 // g_pts_shift
    // content-time offset added to the displayed position: a transcode SEEK restarts the
    // stream 0-based (its PTS loses the seek offset), so playpos = fed - pts_shift + this.
    // 0 for direct-play (file PTS already IS content time) and for the initial transcode.
    pub disp_base: AtomicI64,

    // main/pump (M) -> demux (D)
    // The demux seek target in content-ns (-1 = none). The pump publishes it and the demux
    // thread consumes it with an av_seek_frame between two reads — it is the ENTIRE direct-play
    // seek mechanism, and also carries the resume offset armed before the first read. Nothing
    // interrupts the demuxer to make this happen; a `seek_byte` reopen trigger and a `next_url`
    // used to sit alongside it for that, and neither could ever fire (see ff.rs).
    pub seek_to_ns: AtomicI64,
    // the in-place seek's target content-ns, for the feed's rebase guard (drop stale drifted
    // keyframes). Distinct from seek_to_ns, which the demuxer consumes as it seeks. -1 = none.
    pub seek_target_ns: AtomicI64,
    // UI loading state: true from a seek request until playback resumes at the new position (prime→
    // Play). The HUD shows a spinner + freezes the playhead at `seek_display_ns` so it doesn't
    // wobble through the seek/rebase. -1 display = not loading.
    pub seeking: AtomicBool,
    pub seek_display_ns: AtomicI64,
    // pending audio-track switch: the Plex audioStreamID to switch to (-1 = none). The
    // pump forces a fresh transcode with that source audio at the current position.
    pub pending_audio_sid: AtomicI64,
    // NATIVE audio-track switch (direct-play, no transcode): the 0-based audio stream index
    // to feed. `pending` (-1 = none) is consumed by the pump to trigger a reload; `desired`
    // (-1 = av_find_best_stream) is read by the demuxer to pick the Nth audio stream and
    // PERSISTS across seeks/reloads (reset only on a new item, not in reset_session).
    pub pending_audio_idx: AtomicI32,
    pub desired_audio_idx: AtomicI32,
    // pending "re-transcode at the current position with the current audio + subtitle" —
    // set when a subtitle is (de)selected while transcoding, so Plex re-burns (or drops) it.
    pub pending_retranscode: AtomicBool,
    // the derived UI state (a `PlaybackState`), published by the pump once a frame and read by
    // the HUD. Written only on the main thread; atomic because it is read from the draw path.
    pub pb_state: AtomicU8,
    // demux (D) -> main (M): the producer died before publishing a duration, so the EOS path
    // (which needs `duration_ns > 0`) can never fire and the player would sit on a black screen
    // forever. The pump turns this into `PlaybackState::Error` so the HUD can say so.
    pub demux_failed: AtomicBool,
    /// `sf_load` returned 0 — the pipeline REFUSED the Load payload.
    ///
    /// Without this the pump has no way to learn it: `load_thread` logged the result and dropped
    /// it, `loadCompleted` never arrives, and `Stage::Loading` is never left — so the player sits
    /// on a spinner forever with no error and no timeout. A rejected payload is the most likely
    /// shape of a webOS-5-specific failure (a key the newer pipeline will not accept), which makes
    /// this exactly the case that must be visible.
    pub load_failed: AtomicBool,

    // client-rendered subtitles: selected track index (-1 = off) + the demuxed cues.
    // demux (D) pushes cues; main (M) reads the active one for the current playpos.
    pub desired_sub_idx: AtomicI32,
    pub sub_cues: Mutex<Vec<SubCue>>,
    pub sub_bitmaps: Mutex<Vec<SubBitmap>>, // image-sub cues (selected track only)

    // demux (D) -> main (M)
    pub file_size: AtomicI64,                 // g_file_size
    /// The DECODED FRAME SIZE, published by the demuxer once the video stream is known.
    ///
    /// Exists for the webOS 5+ exported window, whose `SDL_webOSSetExportedWindow(id, src, dst)`
    /// wants the frame you are FEEDING as `src` and the on-screen rect as `dst` — the pair is what
    /// expresses scaling. webOS 4's `AcbAPI_setDisplayWindow` takes only a destination, so nothing
    /// needed this before and the exported path was written passing the authoring canvas (1920x1080)
    /// for both. That is wrong for every 4K direct play, which is most of them.
    ///
    /// 0 until the demuxer has opened the stream; readers must treat 0 as "not known yet".
    pub video_w: AtomicI32,
    pub video_h: AtomicI32,
    pub duration_ns: AtomicI64,               // was g_mkv.duration_ns (published)
    // set once the pipeline has drained to true end-of-stream (EOS pushed AND the last fed frame
    // has been presented). app.rs polls player::ended() to tear the player down at the credits.
    pub ended: AtomicBool,

    // close-to-interrupt handle: raw ptr to the Engine-owned HttpStream box, so
    // the pump/teardown can close(fd) to unblock a blocked recv. The box outlives
    // the worker threads (Engine drops after join), so the ptr stays valid.
    pub hs_ptr: AtomicPtr<HttpStream>,
}

impl Shared {
    pub const fn new() -> Self {
        Shared {
            playpos_ns: AtomicI64::new(0),
            pres_fed: AtomicI64::new(0),
            frames: AtomicI32::new(0),
            seen_frame: AtomicBool::new(false),
            load_completed: AtomicBool::new(false),
            media_id: Mutex::new(None),
            source_info: Mutex::new(None),
            pts_shift: AtomicI64::new(0),
            disp_base: AtomicI64::new(0),
            seek_to_ns: AtomicI64::new(-1),
            seek_target_ns: AtomicI64::new(-1),
            seeking: AtomicBool::new(false),
            seek_display_ns: AtomicI64::new(-1),
            pending_audio_sid: AtomicI64::new(-1),
            pending_audio_idx: AtomicI32::new(-1),
            desired_audio_idx: AtomicI32::new(-1),
            pending_retranscode: AtomicBool::new(false),
            pb_state: AtomicU8::new(PlaybackState::Idle as u8),
            demux_failed: AtomicBool::new(false),
            load_failed: AtomicBool::new(false),
            desired_sub_idx: AtomicI32::new(-1),
            sub_cues: Mutex::new(Vec::new()),
            sub_bitmaps: Mutex::new(Vec::new()),
            file_size: AtomicI64::new(0),
            video_w: AtomicI32::new(0),
            video_h: AtomicI32::new(0),
            duration_ns: AtomicI64::new(0),
            ended: AtomicBool::new(false),
            hs_ptr: AtomicPtr::new(std::ptr::null_mut()),
        }
    }
    /// reset per-file state on stop (mirrors the tail of stop_bufferfeed).
    pub fn reset_session(&self) {
        self.playpos_ns.store(0, Ordering::Relaxed);
        self.pres_fed.store(0, Ordering::Relaxed);
        self.frames.store(0, Ordering::Relaxed);
        // a fresh session has shown nothing yet — keep this adjacent to `frames` in all three
        // places (declaration, `new`, here): a reload that forgot the bit would silently suppress
        // the centred read-out for the rest of the app's life.
        self.seen_frame.store(false, Ordering::Relaxed);
        self.load_completed.store(false, Ordering::Relaxed);
        *self.media_id.lock().unwrap() = None;
        *self.source_info.lock().unwrap() = None;
        self.pts_shift.store(0, Ordering::Relaxed);
        self.disp_base.store(0, Ordering::Relaxed);
        self.seek_to_ns.store(-1, Ordering::Relaxed);
        self.seek_target_ns.store(-1, Ordering::Relaxed);
        self.seeking.store(false, Ordering::Relaxed);
        self.seek_display_ns.store(-1, Ordering::Relaxed);
        self.pending_audio_sid.store(-1, Ordering::Relaxed);
        self.pending_audio_idx.store(-1, Ordering::Relaxed);
        // NB: desired_audio_idx is NOT reset here — it persists across seeks/reloads so a
        // native audio-track choice survives seeking. It is reset on a new item (route).
        self.pending_retranscode.store(false, Ordering::Relaxed);
        self.pb_state.store(PlaybackState::Idle as u8, Ordering::Relaxed);
        self.demux_failed.store(false, Ordering::Relaxed);
        self.load_failed.store(false, Ordering::Relaxed);
        // NB: desired_sub_idx is NOT reset here — like desired_audio_idx it persists across
        // seeks/reloads so a reload-based seek keeps the chosen subtitle. It is reset on a new
        // item (player::reset_subtitle). The cue/bitmap STORES below are transient render state
        // and DO clear (the fresh demuxer re-populates them).
        self.sub_cues.lock().unwrap().clear();
        self.sub_bitmaps.lock().unwrap().clear();
        self.file_size.store(0, Ordering::Relaxed);
        self.duration_ns.store(0, Ordering::Relaxed);
        self.ended.store(false, Ordering::Relaxed);
        self.hs_ptr.store(std::ptr::null_mut(), Ordering::Release);
    }
}

/// UI-facing transport state. Main-thread-only in practice (plex_run + pump +
/// player_hud all run on M), but exposed as atomics so app.rs / player_hud.rs read
/// it with plain .load()/.store(). Replaces the #[no_mangle] transport globals.
pub(crate) struct Transport {
    pub started: AtomicBool,     // bf_started
    pub paused: AtomicBool,      // pl_paused
    pub resume_pend: AtomicBool, // resumePausePending
    pub hud_until: AtomicU32,    // pl_hud_until (SDL ticks)
    pub scrub_ns: AtomicI64,     // pl_scrub_ns (-1 = not scrubbing)
    pub seek_to_ns: AtomicI64,   // g_seek_to_ns (UI seek request, -1 = none)
    // Seek requests received since the pump last APPLIED one. seek_to_ns only ever holds the
    // newest target, so without this counter a coalesced burst is indistinguishable from a
    // single tap after the fact — the pump reports `coalesced=` from it (see pump.rs).
    pub seek_reqs: AtomicU32,
}
impl Transport {
    pub const fn new() -> Self {
        Transport {
            started: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            resume_pend: AtomicBool::new(false),
            hud_until: AtomicU32::new(0),
            scrub_ns: AtomicI64::new(-1),
            seek_to_ns: AtomicI64::new(-1),
            seek_reqs: AtomicU32::new(0),
        }
    }
    /// reset on stop (mirrors the transport tail of stop_bufferfeed).
    pub fn reset(&self) {
        self.started.store(false, Ordering::Relaxed);
        self.paused.store(false, Ordering::Relaxed);
        self.resume_pend.store(false, Ordering::Relaxed);
        self.scrub_ns.store(-1, Ordering::Relaxed);
        self.seek_to_ns.store(-1, Ordering::Relaxed);
        self.seek_reqs.store(0, Ordering::Relaxed);
    }
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
/// **`Bound` is the ACB bind, and the webOS 5 path SKIPS it** — `VP_EXPORTED` goes
/// `Playing -> Streaming` directly, because there is no bind sequence to be in the middle of.
/// So `stage >= Bound` means "bound or later" on webOS 4 and "is Streaming" on webOS 5; a new
/// test against it has to decide which of those it wants.
pub(crate) enum Stage {
    Loading = 0,
    Playing,
    Bound,
    Streaming,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one invariant that cannot be recovered from if it breaks, and breaks SILENTLY: a session
    /// boundary must forget this session's picture. `seen_frame` is set once, off-thread, and is
    /// never cleared anywhere else — so a `reset_session` that dropped it would leave it true for
    /// the rest of the process's life, and every subsequent cold start would show only the 12px
    /// transport mark instead of the centred read-out. Graded together with `frames`, because the
    /// two are only meaningful as a pair (see `Shared::seen_frame`).
    #[test]
    fn reset_session_forgets_this_sessions_picture() {
        let s = Shared::new();
        assert!(!s.seen_frame.load(Ordering::Relaxed), "a fresh session has shown nothing");
        s.seen_frame.store(true, Ordering::Relaxed);
        s.frames.store(9, Ordering::Relaxed);
        s.reset_session();
        assert!(!s.seen_frame.load(Ordering::Relaxed), "a reload/stop blanks the plane — say so");
        assert_eq!(s.frames.load(Ordering::Relaxed), 0, "and the two must be reset together");
    }
}
