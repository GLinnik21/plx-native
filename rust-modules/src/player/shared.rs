//! player::shared — the ONLY cross-thread state. Every field replaces a `volatile`
//! global from playback.c and is an atomic or a Mutex (never a bare value). One
//! long-lived `static SHARED` in mod.rs: it outlives every start/stop cycle and is
//! *reset*, never freed — so a late library callback after teardown writes to a
//! live object, exactly as the C static globals behaved.
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicPtr, AtomicU32, Ordering};
use std::sync::Mutex;
use crate::stream::HttpStream;

#[derive(Clone, Copy)]
pub(crate) struct CueEnt {
    pub t_ns: i64,
    pub byte: i64,
} // was struct cue_ent

/// one client-rendered subtitle cue (content-time ns), demuxed from the MKV
pub(crate) struct SubCue {
    pub start_ns: i64,
    pub end_ns: i64,
    pub text: String,
}

pub(crate) struct Shared {
    // library callback thread (K) -> main (M)
    pub playpos_ns: AtomicI64,               // g_playpos_ns
    // the presented frame's fed PTS (0-based, raw `num` from the type=0 callback). The feed
    // loop throttles on max_fed_pts - pres_fed so it stays ~MAX_FEED_AHEAD ahead of the
    // decoder (feeding further overfills the 4K HEVC DPB/CPB and stalls the sink).
    pub pres_fed: AtomicI64,
    pub frames: AtomicI32,                    // bf_frames
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
    pub seek_byte: AtomicI64,                 // g_seek_byte (-1 = none)
    pub seek_to_ns: AtomicI64,                // direct-play demux seek target ns (-1=none); pump -> ff demux
    // a transcode SEEK re-points the demux at a NEW start.mkv?&offset= URL (a live
    // transcode has no byte-Cues); byte-Range seeks leave this None. Taken on re-open.
    pub next_url: Mutex<Option<String>>,
    // pending audio-track switch: the Plex audioStreamID to switch to (-1 = none). The
    // pump forces a fresh transcode with that source audio at the current position.
    pub pending_audio_sid: AtomicI64,
    // the next re-open is a fresh full stream (a switch from direct-play whose track
    // numbering differs from the transcode output) — re-parse Tracks (mkv_run) not
    // mkv_seek_run. A plain transcode seek leaves this false (same target = same tracks).
    pub reparse_next: AtomicBool,
    // pending "re-transcode at the current position with the current audio + subtitle" —
    // set when a subtitle is (de)selected while transcoding, so Plex re-burns (or drops) it.
    pub pending_retranscode: AtomicBool,
    // tells the /:/timeline progress-reporter thread to exit (set in stop_bufferfeed).
    pub report_stop: AtomicBool,

    // client-rendered subtitles: selected track index (-1 = off) + the demuxed cues.
    // demux (D) pushes cues; main (M) reads the active one for the current playpos.
    pub desired_sub_idx: AtomicI32,
    pub sub_cues: Mutex<Vec<SubCue>>,

    // demux (D) -> main (M)
    pub file_size: AtomicI64,                 // g_file_size
    pub duration_ns: AtomicI64,               // was g_mkv.duration_ns (published)

    // cue preflight (C) <-> main (M)
    pub cues: Mutex<Vec<CueEnt>>,             // g_cues (+ g_ncues = .len())
    pub cues_ready: AtomicBool,               // g_cues_ready
    pub cues_abort: AtomicBool,               // g_cues_abort
    pub segment_pos: AtomicI64,               // g_segment_pos

    // close-to-interrupt handles: raw ptrs to the Engine-owned HttpStream boxes, so
    // the pump/teardown can close(fd) to unblock a blocked recv. The boxes outlive
    // the worker threads (Engine drops after join), so the ptrs stay valid.
    pub hs_ptr: AtomicPtr<HttpStream>,
    pub hs2_ptr: AtomicPtr<HttpStream>,

    // --- soft WebVTT subtitle sidecar (transcode only) ---
    // close-to-interrupt handle for the subs socket (like hs_ptr/hs2_ptr)
    pub hs3_ptr: AtomicPtr<HttpStream>,
    // teardown flag for the subs thread (like cues_abort)
    pub subs_abort: AtomicBool,
    // a seek/retranscode/track-switch re-points the subs stream at a new subtitles?…&offset=
    // URL (Some => the thread re-opens on it; taken on re-open, like next_url).
    pub subs_next_url: Mutex<Option<String>>,
    // desired soft-sub Plex stream id for the CURRENT transcode; 0 = none/off. Set by
    // track_menu (write), reconciled by the pump (read) to spawn/re-point/stop the thread.
    pub subs_want_sid: AtomicI64,
}

impl Shared {
    pub const fn new() -> Self {
        Shared {
            playpos_ns: AtomicI64::new(0),
            pres_fed: AtomicI64::new(0),
            frames: AtomicI32::new(0),
            load_completed: AtomicBool::new(false),
            media_id: Mutex::new(None),
            source_info: Mutex::new(None),
            pts_shift: AtomicI64::new(0),
            disp_base: AtomicI64::new(0),
            seek_byte: AtomicI64::new(-1),
            seek_to_ns: AtomicI64::new(-1),
            next_url: Mutex::new(None),
            pending_audio_sid: AtomicI64::new(-1),
            reparse_next: AtomicBool::new(false),
            pending_retranscode: AtomicBool::new(false),
            report_stop: AtomicBool::new(false),
            desired_sub_idx: AtomicI32::new(-1),
            sub_cues: Mutex::new(Vec::new()),
            file_size: AtomicI64::new(0),
            duration_ns: AtomicI64::new(0),
            cues: Mutex::new(Vec::new()),
            cues_ready: AtomicBool::new(false),
            cues_abort: AtomicBool::new(false),
            segment_pos: AtomicI64::new(0),
            hs_ptr: AtomicPtr::new(std::ptr::null_mut()),
            hs2_ptr: AtomicPtr::new(std::ptr::null_mut()),
            hs3_ptr: AtomicPtr::new(std::ptr::null_mut()),
            subs_abort: AtomicBool::new(false),
            subs_next_url: Mutex::new(None),
            subs_want_sid: AtomicI64::new(0),
        }
    }
    /// reset per-file state on stop (mirrors the tail of stop_bufferfeed); does NOT
    /// touch the cue table (that has its own keep_cues rule).
    pub fn reset_session(&self) {
        self.playpos_ns.store(0, Ordering::Relaxed);
        self.pres_fed.store(0, Ordering::Relaxed);
        self.frames.store(0, Ordering::Relaxed);
        self.load_completed.store(false, Ordering::Relaxed);
        *self.media_id.lock().unwrap() = None;
        *self.source_info.lock().unwrap() = None;
        self.pts_shift.store(0, Ordering::Relaxed);
        self.disp_base.store(0, Ordering::Relaxed);
        self.seek_byte.store(-1, Ordering::Relaxed);
        self.seek_to_ns.store(-1, Ordering::Relaxed);
        *self.next_url.lock().unwrap() = None;
        self.pending_audio_sid.store(-1, Ordering::Relaxed);
        self.reparse_next.store(false, Ordering::Relaxed);
        self.pending_retranscode.store(false, Ordering::Relaxed);
        self.report_stop.store(false, Ordering::Relaxed);
        self.desired_sub_idx.store(-1, Ordering::Relaxed);
        self.sub_cues.lock().unwrap().clear();
        self.file_size.store(0, Ordering::Relaxed);
        self.duration_ns.store(0, Ordering::Relaxed);
        self.hs_ptr.store(std::ptr::null_mut(), Ordering::Release);
        self.hs2_ptr.store(std::ptr::null_mut(), Ordering::Release);
        self.hs3_ptr.store(std::ptr::null_mut(), Ordering::Release);
        self.subs_abort.store(false, Ordering::Relaxed);
        *self.subs_next_url.lock().unwrap() = None;
        self.subs_want_sid.store(0, Ordering::Relaxed);
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
        }
    }
    /// reset on stop (mirrors the transport tail of stop_bufferfeed).
    pub fn reset(&self) {
        self.started.store(false, Ordering::Relaxed);
        self.paused.store(false, Ordering::Relaxed);
        self.resume_pend.store(false, Ordering::Relaxed);
        self.scrub_ns.store(-1, Ordering::Relaxed);
        self.seek_to_ns.store(-1, Ordering::Relaxed);
    }
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Stage {
    Idle = 0,
    Loading,
    Playing,
    Bound,
    Streaming,
}
