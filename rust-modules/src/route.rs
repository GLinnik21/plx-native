//! play_movie route selection (direct-play vs transcode) + the stream URL, transcode
//! session, and HUD strings — all private module state. The player engine reads the
//! URL/session through the accessors here; ui::player_hud reads the HUD strings
//! through title_cptr()/ctxline_cptr().
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use crate::pms::PmsMovie;
use std::os::raw::c_char;
use std::ptr::{addr_of, addr_of_mut};

// stream URL + transcode session (empty = direct-play).
static mut URL: String = String::new();
static mut TSESSION: String = String::new();
/// The server's PRE-FLIGHT refusal for the last resolve, or None.
///
/// `Some(sentence)` means `/decision` answered "neither direct play nor conversion is available"
/// (`generalDecisionCode` 2000) BEFORE a byte of video moved, so this playback never got a URL —
/// see [`refusal`]. The String is the server's OWN sentence, carried so the player's read-out can
/// quote it verbatim; it is `""` when the server named a code but no reason, which is why the
/// refusal itself lives in the `Option` and not in the emptiness of the text.
///
/// Main thread only, and written exactly where every other route static is: [`apply_plan`] installs
/// it and [`request_play`] retires it, so it always describes the item the player is showing.
static mut PLAY_VERDICT: Option<String> = None;
// this playback's transcode flavor: true = container-only remux, false = re-encode. A seek or
// retranscode rebuilds the identical start.mkv query from (CUR_RK, SESS, CUR_*_SID, this flag)
// via plex::TranscodeSpec — replaces the old stored offset-free TBASE query string.
static mut CUR_REMUX: bool = false;
// ratingKey of the currently-playing item (movie or episode), so an audio-track
// switch can force a fresh transcode of the same item.
static mut CUR_RK: String = String::new();
// current audio/subtitle selection carried by any TRANSCODE of the current item
// (0 = server default / none). The subtitle is BURNED into the video (our client
// profile advertises no soft-sub support, so Plex's decision is burn); direct-play
// subtitles are separate (client-rendered from the demuxer, player::request_subtitle).
static mut CUR_AUDIO_SID: i64 = 0;
static mut CUR_SUB_SID: i64 = 0;
// the playing item's Part id (from the part key), so an audio switch can PUT the
// server-side stream selection — the transcoder encodes the part's SELECTED audio.
static mut CUR_PART_ID: i64 = 0;
// Opaque per-PLAYBACK session id: used as BOTH the transcode session= param AND the timeline
// X-Plex-Session-Identifier — they must match byte-for-byte so /status/sessions correlates the
// transcode with the report. Regenerated on each play_movie/play_episode.
static mut SESS: String = String::new();
// GET /identity machineIdentifier, cached once — needed for the PlayQueue uri.
static mut MACHINE_ID: String = String::new();
// This playback's PlayQueue ids for the timeline (empty if /playQueues failed).
static mut PQ_ID: String = String::new();
static mut PQ_ITEM_ID: String = String::new();
// The streamed item's Media video/audio codec (h264/hevc, ac3/eac3/aac), so the player picks
// the H265 Load payload for a native HEVC direct-play and the matching audio codec.
/// The item's OWN codecs, as the file has them — captured once per playback and never overwritten
/// by `apply_decision_codecs`, which replaces `STREAM_*` with the transcode OUTPUT.
///
/// Two different questions, and the diagnostics read-out needs both: "what is this file" and "what
/// is the server actually sending". With only the second recorded, a transcode reported its output
/// as though it were the source and the whole server-side transform was invisible.
static mut SRC_VCODEC: String = String::new();
static mut SRC_ACODEC: String = String::new();
static mut STREAM_VCODEC: String = String::new();
static mut STREAM_ACODEC: String = String::new();
// Direct-play source video frame rate (0 = unknown/transcode → omit from the Load esInfo).
static mut STREAM_FPS: f64 = 0.0;
// HUD strings as fixed NUL-terminated C buffers, so title_cptr()/ctxline_cptr() hand
// draw_text (extern "C", *const c_char) a pointer that stays valid for the whole frame.
static mut TITLE: [c_char; 128] = [0; 128];
static mut CTXLINE: [c_char; 96] = [0; 96];

// ---- accessors: the player reads the URL/session; the HUD reads the title/ctxline ----
pub(crate) fn url() -> String {
    unsafe { (*addr_of!(URL)).clone() }
}
/// Is there a stream URL at all? The in-place twin of [`url`], for the callers that only want the
/// emptiness — [`is_transcoding`]'s idiom, and for the same reason: a universal-transcode
/// `start.mkv` URL is several hundred bytes, and the player route is exempt from the idle present
/// gate, so a `!url().is_empty()` in a draw is a heap allocation and a memcpy at ~60/s.
pub(crate) fn has_url() -> bool {
    unsafe { !(&*addr_of!(URL)).is_empty() }
}
pub(crate) fn set_url(s: &str) {
    unsafe { *addr_of_mut!(URL) = s.to_owned() }
}
pub(crate) fn clear_url() {
    unsafe { (*addr_of_mut!(URL)).clear() }
}
pub(crate) fn transcode_session() -> String {
    unsafe { (*addr_of!(TSESSION)).clone() }
}
/// Forget the cached `machineIdentifier` — the app is now pointed at a DIFFERENT server.
///
/// [`MACHINE_ID`] is fetched once and reused for every PlayQueue's `server://{id}/…` uri, which was
/// sound while one process meant one server. It no longer is: `browse::set_cur` moves the current
/// server when you pick a library on another one, and a queue built with the previous server's id
/// names a machine that does not hold the item. Clearing it makes the next resolve re-fetch it from
/// whichever server is current (`resolve_playqueue`'s `cached.is_empty()` branch).
pub(crate) fn forget_server_identity() {
    unsafe { (*addr_of_mut!(MACHINE_ID)).clear() }
}
/// true while this playback is a server transcode (a live transcode session exists). Cheap
/// in-place check — the pump polls it every tick, so no String clone here.
pub(crate) fn is_transcoding() -> bool {
    unsafe { !(&*addr_of!(TSESSION)).is_empty() }
}
/// Did the server REFUSE this item at `/decision`, before playback? Cheap in-place check —
/// `player::state()` derives `Error` from it on every frame of the player route.
pub(crate) fn play_refused() -> bool {
    unsafe { (*addr_of!(PLAY_VERDICT)).is_some() }
}
/// The refusal's own sentence for the read-out to quote — `None` when the server did not refuse,
/// `Some("")` when it refused without saying why. MAIN THREAD (see [`PLAY_VERDICT`]).
///
/// Borrowed, not cloned: the read-out asks for this 2–3× on every frame of a failure (the HUD
/// caption, the read-out itself, and the diagnostics panel when it is open), and every one of them
/// only reads it. The borrow lives until the next main-thread write, which is `apply_plan` or
/// `request_play` — neither of which can run inside a frame's draw.
pub(crate) fn play_verdict() -> Option<&'static str> {
    unsafe { (*addr_of!(PLAY_VERDICT)).as_deref() }
}
/// Retire the refusal — "this playback request is withdrawn", the one thing besides a fresh
/// resolve that ends a verdict's life. [`request_play`] clears it because a NEW item is being
/// resolved; this is the other half, for leaving the player entirely.
///
/// Without it a refusal outlived the player: `player::state()` derives `Error` from this static
/// and takes no route, so a verdict left standing described the item the user walked away from —
/// on Home, in the Library, on any detail page — until they happened to start something else.
fn clear_play_verdict() {
    unsafe { *addr_of_mut!(PLAY_VERDICT) = None }
}
/// select the subtitle to BURN into any transcode of the current item (0 = none). This
/// is the transcode path; direct-play uses the client renderer (player::request_subtitle).
pub(crate) fn set_subtitle(sid: i64) {
    unsafe { addr_of_mut!(CUR_SUB_SID).write(sid) }
}
/// the subtitle stream id currently burned into the transcode (0 = none).
pub(crate) fn cur_sub_sid() -> i64 {
    unsafe { addr_of!(CUR_SUB_SID).read() }
}
/// ratingKey of the currently-playing item (for /:/timeline progress reports).
pub(crate) fn cur_rk() -> String {
    unsafe { (*addr_of!(CUR_RK)).clone() }
}
pub(crate) fn cur_audio_sid() -> i64 {
    unsafe { addr_of!(CUR_AUDIO_SID).read() }
}
/// The currently-playing item's Part id. Written once per item by `build_stream` from its own
/// `part` argument. In-playback callers (audio switch, subtitle toggle, retranscode) want this;
/// `build_stream` must pass its freshly-derived local instead, since this is not yet updated
/// for the item being started.
fn cur_part_id() -> i64 {
    unsafe { addr_of!(CUR_PART_ID).read() }
}
/// The current playback's session id (X-Plex-Session-Identifier == transcode session=).
pub(crate) fn sess() -> String {
    unsafe { (*addr_of!(SESS)).clone() }
}
pub(crate) fn pq_id() -> String {
    unsafe { (*addr_of!(PQ_ID)).clone() }
}
pub(crate) fn pq_item_id() -> String {
    unsafe { (*addr_of!(PQ_ITEM_ID)).clone() }
}
/// The streamed item's Media video/audio codec, so the player picks the H265 Load payload for a
/// native HEVC direct-play and the matching audio codec.
pub(crate) fn stream_vcodec() -> String {
    unsafe { (*addr_of!(STREAM_VCODEC)).clone() }
}
pub(crate) fn stream_acodec() -> String {
    unsafe { (*addr_of!(STREAM_ACODEC)).clone() }
}
/// direct-play source video fps for the Load esInfo (0 = unknown/transcode → omit)
pub(crate) fn stream_fps() -> f64 {
    unsafe { *addr_of!(STREAM_FPS) }
}
/// Override the audio codec used to build the Load payload — set by a native audio-track
/// switch to the chosen track's codec before the direct-play reload.
pub(crate) fn set_stream_acodec(codec: &str) {
    unsafe { *addr_of_mut!(STREAM_ACODEC) = codec.to_owned() }
}
/// Record the streamed item's video+audio codec pair in one write (the Load-payload source of
/// truth) — outside `apply_decision_codecs`, the two fields are only ever set together.
pub(crate) fn set_stream_codecs(vc: &str, ac: &str) {
    unsafe {
        *addr_of_mut!(STREAM_VCODEC) = vc.to_owned();
        *addr_of_mut!(STREAM_ACODEC) = ac.to_owned();
    }
}

/// Record the SOURCE file's codecs for this playback. Called from the plan install only — never
/// from `apply_decision_codecs`, whose whole job is to replace the stream codecs with the
/// transcode's output. See [`SRC_VCODEC`].
pub(crate) fn set_source_codecs(vc: &str, ac: &str) {
    unsafe {
        *addr_of_mut!(SRC_VCODEC) = vc.to_owned();
        *addr_of_mut!(SRC_ACODEC) = ac.to_owned();
    }
}
/// Was this playback's transcode a container-only REMUX (codecs copied) rather than a re-encode?
/// Meaningless unless `is_transcoding()`. The diagnostics read-out's three-way Source row turns on
/// it: "the server touched the pixels" and "the server repackaged the bytes" are different facts
/// and only one of them can explain a decode problem.
pub(crate) fn is_remux() -> bool {
    unsafe { addr_of!(CUR_REMUX).read() }
}
pub(crate) fn source_vcodec() -> String {
    unsafe { (*addr_of!(SRC_VCODEC)).clone() }
}
pub(crate) fn source_acodec() -> String {
    unsafe { (*addr_of!(SRC_ACODEC)).clone() }
}
/// pointers into the module-owned HUD buffers (valid for the whole frame draw_text uses them)
pub(crate) fn title_cptr() -> *const c_char {
    addr_of!(TITLE) as *const c_char
}
pub(crate) fn ctxline_cptr() -> *const c_char {
    addr_of!(CTXLINE) as *const c_char
}
/// This playback's universal-transcoder spec, rebuilt from the module state (rk + session are
/// borrowed from the caller's locals; audio/subtitle ride the CURRENT selection) — so every
/// (re)start of the item's transcode carries identical params.
fn transcode_spec<'a>(rk: &'a str, session: &'a str, remux: bool, offset_secs: i64, aud: i64, sub: i64) -> crate::plex::TranscodeSpec<'a> {
    crate::plex::TranscodeSpec {
        rating_key: rk,
        session,
        remux,
        audio_stream_id: aud,
        subtitle_stream_id: sub,
        offset_secs,
    }
}

/// The end-of-playback PMS work, moved OFF the main thread: the `state=stopped` timeline report
/// (which commits the server-side resume point and watched state) and the server-side transcode
/// stop. Replaces the inline `report_timeline` + `stop_transcode` pair in `engine::teardown`.
///
/// Both are fire-and-forget POSTs whose results are discarded, and both ran inline on the SDL
/// thread — two blocking PMS round trips, each bounded by `CONNECT_TIMEOUT_MS` + `SO_RCVTIMEO`
/// (~17 s), on **100% of real stops**. That was the largest guaranteed main-loop park left in the
/// engine, and strictly bigger than the rare in-flight-POST window at the joins above it.
///
/// Everything the worker needs is read HERE, on the main thread, and the statics are cleared here
/// too: route's session state is `static mut`, and what keeps it sound is that the main thread is
/// its only writer. The worker gets owned copies and touches none of it — the same capture the
/// demux thread's `acodec` does, and for the same reason.
pub(crate) fn scrobble_stop(
    final_report: Option<(String, i64, i64)>,
    report_th: Option<std::thread::JoinHandle<()>>,
) {
    let (session, pq, pqi) = (sess(), pq_id(), pq_item_id());
    let (aud, sub) = (cur_audio_sid(), cur_sub_sid()); // the selection this playback reported under
    let tsession = transcode_session();
    unsafe {
        (*addr_of_mut!(TSESSION)).clear();
        addr_of_mut!(CUR_REMUX).write(false);
    }
    if final_report.is_none() && tsession.is_empty() && report_th.is_none() {
        return; // nothing to post and nobody to wait for
    }
    let Some(c) = crate::plex::client_opt() else { return };
    // Serialise against a previous stop still in flight: these carry a position for a specific
    // item, and letting two race would let an older one land last. Normally free — the measured
    // baseline for a finished worker is 0 ms.
    drain_scrobble();
    let h = crate::task::spawn_small_keeping("scrobble", move || {
        // The progress reporter's last `playing` POST must land BEFORE this `stopped` one, or the
        // server is left believing playback continues. That ordering is why teardown used to join
        // it — on the main thread. Waiting for it HERE keeps the guarantee and moves the cost off
        // the frame loop.
        if let Some(t) = report_th {
            crate::task::join("timeline", t);
        }
        if let Some((rk, t_ms, d_ms)) = final_report {
            c.timeline(&crate::plex::TimelineReport {
                rating_key: &rk,
                state: crate::plex::TimelineState::Stopped,
                time_ms: t_ms,
                duration_ms: d_ms,
                session: &session,
                play_queue_id: &pq,
                play_queue_item_id: &pqi,
                audio_stream_id: aud,
                subtitle_stream_id: sub,
            });
            crate::log(&format!("timeline stopped t={}s/{}s", t_ms / 1000, d_ms / 1000));
        }
        if !tsession.is_empty() {
            c.transcode_stop(&tsession);
        }
    });
    *SCROBBLE.lock().unwrap_or_else(|e| e.into_inner()) = h;
}

/// The final scrobble still in flight, if any.
static SCROBBLE: std::sync::Mutex<Option<std::thread::JoinHandle<()>>> = std::sync::Mutex::new(None);

/// Wait for a pending [`scrobble_stop`] to reach the server.
///
/// Called from exactly two places: the next stop (so two reports for different items cannot land
/// out of order), and `plex_run`'s exit — because the process is about to die and a detached
/// worker dies with it, which would silently drop the resume point the user just earned. Blocking
/// there is the same cost the old inline call paid, except it is now paid ONCE at exit instead of
/// on every BACK out of a movie.
pub(crate) fn drain_scrobble() {
    let h = SCROBBLE.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(h) = h {
        crate::task::join("scrobble", h);
    }
}

/// Seek within a LIVE TRANSCODE by restarting it at a time offset — a transcode has
/// no byte-Cues, so a byte-Range seek can't work (docs/plex-api.md). Stops the current
/// encoder, then re-registers (/decision) and re-points the stream at
/// start.mkv?...&offset={secs}. Returns the new URL (the demux re-opens it from byte 0),
/// or None if this playback isn't a transcode. Blocks on two HTTP round-trips (like
/// play_movie's /decision), which is fine during a seek (the pipeline is flushed).
pub(crate) fn transcode_seek(offset_secs: i64) -> Option<String> {
    if transcode_session().is_empty() {
        return None;
    }
    let rk = cur_rk();
    if rk.is_empty() {
        return None;
    }
    let c = crate::plex::client_opt()?;
    // NB: do NOT explicitly /stop the old encoder here — the session id is reused, so a stop
    // would cut the stream the demux thread is still reading out from under it. The caller
    // (the pump) instead reloads onto this new start.mkv?&offset= (same session), which tears
    // the old engine down — dropping its connection, and with it the old transcode.
    // /decision is just a query and doesn't cut the streaming connection.
    let session = sess();
    let sp = transcode_spec(&rk, &session, is_remux(), offset_secs.max(0),
                            cur_audio_sid(), cur_sub_sid());
    // same session, same output codecs — no payload rebuild here, so the body is unused
    let _ = c.transcode_decision(&sp);
    let url = c.transcode_start_url(&sp).to_url();
    set_url(&url);
    Some(url)
}

use crate::cbuf::set as set_c; // shared fixed-C-buffer write (the HUD TITLE/CTXLINE buffers)

/// Ask PMS whether `rk` should direct-play (Some(true) → serve the raw Part) or transcode
/// (Some(false) → start.mkv). None when the server returns no usable Media decision, so the
/// caller falls back to the local codec test. Registers the session as a side effect.
fn server_decision(rk: &str, session: &str) -> Option<bool> {
    let mc = match crate::plex::client_opt()?.mde_decision(rk, session) {
        Some(mc) => mc,
        None => {
            // failed fetch OR unparseable (XML/truncated) body — keep the fallback visible
            // in the event log, like the old raw-body scan did
            crate::player::log("decision: no/unparseable response -> local heuristic");
            return None;
        }
    };
    // Part.decision is the authoritative verdict (Media/container carry none)
    let part = match mc.metadata.first().and_then(|m| m.media.first()).and_then(|md| md.part.first()) {
        Some(p) => p,
        None => {
            crate::player::log(&format!(
                "decision: no media (general={:?}) -> local heuristic",
                mc.general_decision_code
            ));
            return None;
        }
    };
    let direct = part.decision == "directplay";
    crate::player::log(&format!(
        "decision: part={} general={:?} mde={:?} -> {}",
        part.decision,
        mc.general_decision_code,
        mc.mde_decision_code,
        if direct { "DIRECT PLAY" } else { "TRANSCODE" }
    ));
    Some(direct)
}

/// Read the transcoder's OUTPUT codecs from a /decision response and store them as the stream
/// codecs the Load payload is built from. The decision's Part.Stream[].codec is the codec each
/// lane will actually ARRIVE in (it equals the source codec only when that lane is copied).
/// Assuming "a container remux copies the audio" broke mp4 items whose audio PMS re-encodes to
/// the transcode-target's AC3: the payload said AAC, the stream carried AC3, and the
/// configured-for-AAC pipeline played silence (the `movie_hevc_aac_mp4` harness case).
/// PURE: the codec pair the server's /decision OUTPUT actually declares, or None if it names
/// neither. The Load payload must match this, not the source file — a transcode changes the
/// codec and rate, and describing the source to the decoder gives silent audio.
fn decision_codecs(mc: &crate::plex::MediaContainer) -> Option<(String, String)> {
    let streams = mc.metadata.first().and_then(|m| m.media.first()).and_then(|md| md.part.first())
        .map(|p| &p.stream)?;
    let (mut vc, mut ac) = (None, None);
    for s in streams {
        match s.stream_type {
            1 if vc.is_none() && !s.codec.is_empty() => vc = Some(s.codec.to_lowercase()),
            2 if ac.is_none() && !s.codec.is_empty() => ac = Some(s.codec.to_lowercase()),
            _ => {}
        }
    }
    match (vc, ac) { (Some(v), Some(a)) => Some((v, a)), _ => None }
}

/// `generalDecisionCode` 2000 — "Neither direct play nor conversion is available." The server has
/// adjudicated the whole request and can serve NEITHER lane; there is nothing left for the client
/// to try, which is what makes it a stop rather than another fallback.
const DECISION_UNPLAYABLE: i64 = 2000;

/// PURE: the server's pre-flight refusal, or None.
///
/// `/decision` is asked BEFORE a byte of video moves, and it can answer "no" — verified live
/// against PMS 1.43.3 on a VP9 source: `generalDecisionCode 2000` beside
/// `transcodeDecisionCode 4007, "Cannot convert this item. Implementation for video encoder 'vp9'
/// not found."`. The app used to parse `general_decision_code` and only LOG it, then hand
/// `start.mkv` to the pipeline anyway — so a server that had already said no produced "Buffering…"
/// followed by a generic failure, and the one sentence that explained it was in a log the user
/// cannot reach.
///
/// **The CODE is authoritative and the text is only the human sentence.** Grading on the text would
/// be grading on server copy that is localised, versioned and free to change; grading on the code
/// is why a server that refuses without saying why still stops us (`Some("")`).
///
/// Of the two sentences the body carries, the TRANSCODE one is preferred: `generalDecisionText`
/// restates the code ("Neither direct play nor conversion is available") while
/// `transcodeDecisionText` names the actual cause. The general one is the fallback for a server
/// that sends only it.
fn refusal(mc: &crate::plex::MediaContainer) -> Option<String> {
    if mc.general_decision_code != Some(DECISION_UNPLAYABLE) {
        return None;
    }
    let text = if !mc.transcode_decision_text.is_empty() {
        &mc.transcode_decision_text
    } else {
        &mc.general_decision_text
    };
    Some(text.trim().to_string())
}

fn apply_decision_codecs(mc: &crate::plex::MediaContainer) {
    if let Some((vc, ac)) = decision_codecs(mc) {
        set_stream_codecs(&vc, &ac);
        crate::player::log(&format!("decision output: v={vc} a={ac}"));
    }

}

/// Select the audio + subtitle streams server-side for the current part before a
/// transcode. The transcoder encodes the part's SELECTED audio and BURNS its SELECTED
/// subtitle (our client profile advertises no soft-sub support, so Plex's decision is
/// always burn) — a query-param subtitleStreamID does NOT suppress a default-selected
/// sub, only the PUT does. So we PUT subtitleStreamID=0 to keep subs OFF (no burn), or
/// the chosen id to burn it; audioStreamID only when the user switched (else keep default).
fn put_selection(part: i64, aud: i64, sub: i64) {
    if part <= 0 {
        return;
    }
    let c = match crate::plex::client_opt() {
        Some(c) => c,
        None => return,
    };
    let st = c.select_streams(&crate::plex::StreamSelection {
        part_id: part,
        audio_stream_id: aud,
        subtitle_stream_id: sub,
    });
    crate::player::log(&format!("select streams: part={part} audio={aud} sub={sub} -> HTTP {st}"));
}

/// Fresh opaque session id per playback. Reads the kernel UUID (the TV is Linux); falls
/// back to a ratingKey + monotonic-counter token if that read fails.
fn new_sess(rk: &str) -> String {
    if let Ok(u) = std::fs::read_to_string("/proc/sys/kernel/random/uuid") {
        let t = u.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(1);
    format!("plxnative-{rk}-{}", CTR.fetch_add(1, Ordering::Relaxed))
}

/// The episode queued after the one now playing — everything the Up Next control draws AND
/// everything [`request_play`] needs to start it, so playing it costs no PMS round trip either.
///
/// It comes free with the `continuous=1` PlayQueue every playback already creates (see
/// [`crate::plex::Client::create_play_queue`]); nothing here asks the server "what's next".
#[derive(Clone, Default)]
pub(crate) struct UpNext {
    pub(crate) rk: String,
    pub(crate) part: String,
    pub(crate) vcodec: String,
    pub(crate) acodec: String,
    pub(crate) show_title: String, // grandparentTitle
    pub(crate) ep_title: String,
    pub(crate) season: i64,
    pub(crate) index: i64,
    pub(crate) thumb: String,
    pub(crate) dur_ms: i64,
    pub(crate) resume_ms: i64,
}

/// What the queue told us, as owned data for `apply_plan` to install. `machine_id` is `""` when
/// the cached one is still good.
#[derive(Default)]
struct QueueInfo {
    machine_id: String,
    id: String,
    item_id: String,
    up_next: Option<UpNext>,
    rows: Vec<crate::plex::QueueRow>,
}

/// The next episode of the item now playing, or None (a movie, the last episode, or a queue that
/// failed). Installed by `apply_plan`; `request_play` retires it the moment a new item resolves.
static mut UP_NEXT: Option<UpNext> = None;

/// The whole queue behind the item now playing — the playing row included, in queue order,
/// projected to `plex::QueueRow` ON THE RESOLVE WORKER (a `Metadata` row carries its entire
/// Media/Part/Stream/Role tree; a show's queue is dozens of them, and this device is 32-bit).
/// Same lifecycle as `UP_NEXT`: installed by `apply_plan`, retired by `request_play`.
///
/// Whatever the server sent is kept, uncapped — a projected row is ~300 bytes on this device, so
/// even a whole show is tens of KB. The capping that matters is the DRAWING (a still per row is a
/// GL texture); that belongs to the overlay, and `apply_plan` deliberately warms only `up_next`'s.
static mut QUEUE: Vec<crate::plex::QueueRow> = Vec::new();

/// The queued next episode. Main-thread only, and — like `metadata::playing()` — it hands out a
/// `&'static` the Up Next control reads across a frame, so `apply_plan` (main thread) staying its
/// only writer is what keeps that reference sound. A caller that STARTS the next episode must
/// clone first: `request_play` clears this before the new plan lands.
pub(crate) fn up_next() -> Option<&'static UpNext> {
    unsafe { (*addr_of!(UP_NEXT)).as_ref() }
}

/// The current playback's queue rows, in queue order, the row now playing among them — locate it
/// with `plex::queue_index_of(rows, PQ_ITEM_ID.parse().unwrap_or(0), &cur_rk())`, which is the ONE
/// implementation of the identity rule (item id, rating key as the fallback). Empty until a plan
/// lands, and whenever the queue POST failed.
///
/// MAIN THREAD ONLY. Unlike [`up_next`] this lends the rows to a closure instead of handing out a
/// `&'static`, and that is deliberate: `request_play` FREES this Vec as its first act, so a
/// borrowed row used to start playback would be read after free — exactly the aliasing bug
/// `request_play_up_next`'s by-value signature exists to make unrepresentable. A caller that wants
/// to keep a row past the call clones it out (`with_queue(|q| q.get(i).cloned())`); the borrow
/// checker cannot police a `&'static`, but it does police this.
#[allow(dead_code)] // nothing reads the rows yet — the queue overlay that draws them is its own batch
pub(crate) fn with_queue<R>(f: impl FnOnce(&[crate::plex::QueueRow]) -> R) -> R {
    f(unsafe { &*addr_of!(QUEUE) })
}

/// Build the Up Next descriptor from a queue row. Episodes only: `continuous=1` on a movie
/// returns just the movie itself (verified live — total count 1), and "up next" is a show idea.
/// The gate belongs HERE, on the one-item control — the retained row list is deliberately not
/// episode-gated, because a queue list has to be able to show whatever the queue holds.
fn up_next_of(r: &crate::plex::QueueRow) -> Option<UpNext> {
    if r.kind != "episode" || r.rk.is_empty() {
        return None;
    }
    Some(UpNext {
        rk: r.rk.clone(),
        part: r.part.clone(),
        vcodec: r.vcodec.clone(),
        acodec: r.acodec.clone(),
        show_title: r.show_title.clone(),
        ep_title: r.title.clone(),
        season: r.season,
        index: r.index,
        thumb: r.thumb.clone(),
        dur_ms: r.dur_ms,
        resume_ms: r.resume_ms,
    })
}

/// Create a PlayQueue for `rk` so the session is a first-class, remote-controllable player and
/// the timeline can carry a real playQueueItemID. Best-effort: on failure the timeline still
/// works, just without the queue ids (and the player without an Up Next).
///
/// PURE: returns owned data for `apply_plan` to install.
fn resolve_playqueue(rk: &str, session: &str, cached: &str) -> QueueInfo {
    let mid = if cached.is_empty() {
        crate::plex::client_opt().and_then(|c| c.machine_identity()).unwrap_or_default()
    } else {
        String::new() // unchanged — apply_plan's "" means "leave the cache alone"
    };
    let effective = if mid.is_empty() { cached } else { &mid };
    if effective.is_empty() {
        crate::player::log("playqueue: no machineIdentifier (skip)");
        return QueueInfo::default();
    }
    match crate::plex::client_opt().and_then(|c| c.create_play_queue(effective, rk, session)) {
        Some(q) => {
            let up_next = q.next.as_ref().and_then(up_next_of);
            crate::player::log(&format!(
                "playqueue: id={} item={} remaining={} rows={} next={}",
                q.id, q.selected_item_id, q.remaining, q.items.len(),
                up_next.as_ref().map(|u| format!("S{}E{} {}", u.season, u.index, u.rk))
                    .unwrap_or_else(|| "-".into())
            ));
            QueueInfo {
                machine_id: mid,
                id: if q.id > 0 { q.id.to_string() } else { String::new() },
                item_id: if q.selected_item_id > 0 { q.selected_item_id.to_string() } else { String::new() },
                up_next,
                rows: q.items,
            }
        }
        None => {
            crate::player::log("playqueue: POST failed");
            QueueInfo { machine_id: mid, ..Default::default() }
        }
    }
}

/// Every `static mut` the resolve used to READ, captured on the main thread and passed by value.
///
/// Making the worker WRITE-pure was not enough: it still cloned `MACHINE_ID` and `SESS` — Strings
/// that `apply_plan` reassigns on every landing — so a superseded worker could clone a buffer as
/// it was being dropped (heap corruption on a device with no debugger), and read the two sids as
/// non-atomic i64s, which on armv7 is a tearable two-word load.
#[derive(Clone, Default)]
pub(crate) struct ResolveEnv {
    pub machine_id: String,
    pub audio_sid: i64,
    pub sub_sid: i64,
    /// the loaded detail's streams when it IS this item — saves the worker a GET
    pub cached_item: Option<crate::metadata::PlayingItem>,
}

impl ResolveEnv {
    /// MAIN THREAD ONLY.
    fn snapshot(rk: &str) -> ResolveEnv {
        ResolveEnv {
            machine_id: unsafe { (*addr_of!(MACHINE_ID)).clone() },
            audio_sid: cur_audio_sid(),
            sub_sid: cur_sub_sid(),
            cached_item: crate::metadata::cached_playing(rk),
        }
    }
}

/// Everything `resolve` DECIDES, as owned data. No `static mut`, no `SHARED`, no ACB/Starfish —
/// so it is `Send` and the resolve can run on a worker. `apply_plan` (main thread) is the ONLY
/// code that installs it. Adding a field here is how you add a resolve output; writing a static
/// from the worker is how you reintroduce the races the audit found.
#[derive(Default)]
pub(crate) struct Plan {
    pub url: String,
    pub tsession: String,
    pub sess: String,
    pub part_id: i64,
    pub pq_id: String,
    pub pq_item_id: String,
    pub machine_id: String,   // "" = leave the cached one alone
    pub vcodec: String,
    pub acodec: String,
    /// The SOURCE file's codecs, kept beside the ones above because on a transcode those are the
    /// server's OUTPUT. "hevc → h264" is the whole server-side transform, and it is invisible if
    /// only one half is recorded. Equal to `vcodec`/`acodec` for a direct play and for a remux.
    pub src_vcodec: String,
    pub src_acodec: String,
    pub fps: f64,
    pub audio_sid: i64,
    pub remux: bool,
    /// demuxer stream ordinal to feed (direct-play, non-default track). None = leave as-is.
    pub feed_audio_ordinal: Option<i32>,
    /// the subtitle stream the server already had selected for this part (0 = none/off), so the
    /// menu checkmark and the timeline report agree with what is on screen — and a later
    /// transcode of this item burns the subtitle the user was already watching.
    pub sub_sid: i64,
    /// client-renderer ordinal for that subtitle (`metadata::sub_render_ordinal`). None = subs off.
    pub sub_render_ordinal: Option<i32>,
    /// the playing item's track store, fetched off-thread and installed by apply_plan
    pub playing: Option<crate::metadata::PlayingItem>,
    /// The server's PRE-FLIGHT refusal (see [`refusal`]), when `/decision` said it can neither
    /// direct play nor convert this item. A plan carrying one has an EMPTY `url` by construction —
    /// that is how it fails, on the same path as every other unresolvable plan — and the sentence
    /// rides along so the read-out can quote the server instead of guessing. `None` on every other
    /// plan, including one that simply failed to reach the server.
    pub verdict: Option<String>,
    /// the episode queued after this one, straight off the `continuous=1` PlayQueue
    pub up_next: Option<UpNext>,
    /// that same PlayQueue's whole returned window, projected on the worker (see `QUEUE`)
    pub queue: Vec<crate::plex::QueueRow>,
}

/// Pick the stream URL for an item: direct-play only what the pipeline decodes natively (H264/
/// HEVC + a direct-playable audio track); else ask the server to remux or transcode into
/// progressive MKV. On the transcode path this also runs the /decision handshake.
///
/// PURE: runs on the resolve worker. It must neither WRITE nor READ any `static mut` — every
/// input arrives in `ResolveEnv`, every output leaves in `Plan`, and `apply_plan` installs both
/// on the main thread. Write-purity alone is not enough: `apply_plan` reassigns the `MACHINE_ID`
/// and `SESS` Strings, so a still-running superseded worker reading them is a use-after-free.
fn build_stream(rk: &str, part: &str, vcodec: &str, acodec: &str, env: &ResolveEnv) -> Plan {
    // The part id is derived from THIS call's `part`, before anything else runs, and published
    // here rather than by the caller after we return. It used to be written by play_movie /
    // play_episode *after* build_stream finished, so `put_selection` — which runs inside this
    // function — read the PREVIOUS item's part (or 0, and silently skipped, on the first play
    // of the process). Every non-MKV item takes the remux branch, so that mis-targeted PUT
    // failed to suppress a server-default subtitle and burned it into the transcode.
    // The arguments ARE the source codecs, whatever this function goes on to choose — captured
    // once, here, so no later branch has to remember to.
    let mut plan = Plan {
        part_id: part_id_of(part),
        src_vcodec: vcodec.to_string(),
        src_acodec: acodec.to_string(),
        ..Default::default()
    };
    let client = match crate::plex::client_opt() {
        Some(c) => c,
        None => return plan,
    };
    // fresh per-playback session id (BOTH direct-play and transcode report through it) +
    // a PlayQueue so the server tracks this as a real player with a playQueueItemID.
    let session = new_sess(rk);
    plan.sess = session.clone();
    if !rk.is_empty() {
        let q = resolve_playqueue(rk, &session, &env.machine_id);
        plan.machine_id = q.machine_id;
        plan.pq_id = q.id;
        plan.pq_item_id = q.item_id;
        plan.up_next = q.up_next;
        plan.queue = q.rows;
    }
    // the playing item's OWN track lists (menu + audio pick + esInfo fps read them) — the
    // loaded detail can be a different item (show page / straight-from-Home play)
    // detail already had this item's streams — no GET
    plan.playing = env.cached_item.clone().or_else(|| crate::metadata::fetch_playing_item(rk));
    // Server-adjudicated: the Media Decision Engine decides direct-play vs transcode from our
    // capability profile. Falls back to the local codec test if the server returns no usable
    // decision; the local-sample/demo path (rk empty) skips the decision entirely.
    // Server-adjudicated (Phase 2). HEVC now direct-plays (Phase 3 demuxer + native decode);
    // the guard that forced non-h264 to transcode is gone.
    // Smart direct-play: the video decodes natively (H264/HEVC) AND some audio track is
    // direct-playable (AAC/AC3/E-AC3) — even if the DEFAULT track isn't. We own the demuxer, so
    // we direct-play the raw file and FEED a direct-playable track (e.g. a 4K HEVC item: TrueHD
    // default + an AC3 track → native 4K HEVC + AC3, no transcode — beats the server's
    // video-downscaling transcode). Falls back to the server /decision (then the local codec
    // test) when the video isn't direct-playable or NO audio track is (TrueHD/DTS-only → transcode).
    // The video gate consults the DEVICE's own decoder table (devcaps), not this codebase's
    // memory of the dev TV: "the panel decodes HEVC" was the last dev-environment claim still
    // asserted as universal (issue #22's bug class — docs/plex-pass-audit.md, closing section).
    // This is belt-and-braces with the profile — a no-hevc profile means PMS should never
    // *offer* hevc direct-play, but the smart-DP branch below can bypass the server's /decision
    // entirely, so the local gate must agree with the profile on BOTH axes it asserts: the codec
    // AND the width/height bound. Codec agreement alone left the resolution half open — the
    // profile's `*`-scoped limitation makes PMS transcode a 4K source down for a 1080p-bounded
    // SoC, but a branch that never asks the server never meets the limitation, so a 4K file with
    // any AAC/AC3 track (nearly every file has one) direct-played straight onto the bounded
    // decoder. See `video_direct_plays` for the gate itself.
    let (src_w, src_h) = plan.playing.as_ref().map(|p| (p.width, p.height)).unwrap_or((0, 0));
    let video_dp = video_direct_plays(vcodec, src_w, src_h, crate::devcaps::caps());
    // MKV and MP4 both direct-play. MP4 once died after AU#0 (b1002de) because the mov demuxer's
    // random access needed seeks the then-unseekable AVIO could not serve; `ff.rs::seek_cb` has
    // reopened with a byte Range since, and mp4 was re-measured on-device 2026-08-11: sequential
    // play, a 140s in-place seek and the harness's rapid burst all pass (issue #22 — the mkv-only
    // gate was sending every mp4 to the transcoder, which a server without Plex Pass then failed).
    // Anything else (.mov/.avi/…) still goes to Plex for a container-only REMUX to progressive
    // MKV (copy the codecs, no re-encode — keeps 4K/HDR).
    let streamable = part_is_streamable(part);
    // snapshot the track list on the MAIN thread and pass it by reference — the resolve worker
    // (step 7) gets an owned copy instead, and never touches the `&'static` store.
    let tracks = plan.playing.as_ref().map(|p| p.audio.as_slice()).unwrap_or(&[]);
    let audio_sel = if rk.is_empty() { None } else { pick_dp_audio(tracks, acodec) };
    let directplay = if !video_dp {
        // The buffer-feed pipeline only decodes what the Load payload declares — H264/H265,
        // and H265 only on a SoC whose table lists the decoder (devcaps). Anything else
        // (AV1/VP9/MPEG-2/…) MUST transcode: we can't feed it even if the server's /decision
        // says directplay (it adjudicates the panel's decoders, not our payload). This gate is
        // why the local sample path (rk empty) is the only other non-transcode case. A source
        // exceeding the device's width/height bound lands here too, and deliberately on the
        // RE-ENCODE side of the branch below (a remux would copy the too-big pixels verbatim);
        // its /decision carries the profile's own bound, so PMS scales the video down.
        false
    } else if !streamable {
        false // non-MKV container → remux (the transcode branch copies the source codecs)
    } else if audio_sel.is_some() {
        true
    } else if rk.is_empty() {
        false
    } else {
        server_decision(rk, &session).unwrap_or_else(|| crate::plex::is_dp_audio(acodec))
    };
    if (directplay || rk.is_empty()) && !part.is_empty() {
        // direct-play: the pipeline decodes the SOURCE codecs natively, so the Load payload uses
        // them (h264/hevc + the chosen audio track's codec). If a specific track was picked
        // (aidx >= 0), tell the demuxer to feed that stream — by CONTAINER ordinal, not the
        // list position (audio_ordinal sorts on PMS Stream.index).
        let (aidx, achosen, asid) = audio_sel.unwrap_or((-1, acodec.to_string(), 0));
        // source fps for the Load esInfo — from the playing item's own store (present for the
        // straight-from-Home path too, which never ran load_detail)
        let fps = plan.playing.as_ref().map(|p| p.video_fps).unwrap_or(0.0);
        plan.vcodec = vcodec.to_string();
        plan.acodec = achosen.clone();
        plan.fps = fps;
        // record the picked track's stream id so the timeline reports what actually plays
        // (0 = default/unknown → the param is omitted, the server shows the part default)
        plan.audio_sid = asid;
        if aidx >= 0 {
            // NB this used to call player::set_audio_track, which stores SHARED.desired_audio_idx —
            // read by the DEMUX THREAD on every reopen. A worker writing it would change the audio
            // track of whatever is currently on screen. apply_plan does it, on the main thread.
            plan.feed_audio_ordinal = Some(
                plan.playing.as_ref()
                    .map(|p| crate::metadata::audio_ordinal(&p.audio, aidx as usize))
                    .unwrap_or(aidx),
            );
        }
        // honour a subtitle the server already has selected for this part (chosen on another
        // client, or by this app in an earlier session) — free here, since the direct-play path
        // renders subtitles itself. apply_plan installs it on the main thread.
        let sub_sel = plan.playing.as_ref().and_then(|p| pick_dp_subtitle(&p.subs));
        if let Some((ssid, ord)) = sub_sel {
            plan.sub_sid = ssid;
            plan.sub_render_ordinal = Some(ord);
        }
        // direct-play: no transcode session (transcode_session() stays empty). Carry the
        // session id + identity on the file GET so PMS keys the /status/sessions entry by
        // SESS (not a token= fallback), keeping the timeline correlation consistent.
        plan.url = client.direct_play_url(part, &session).to_url();
        return plan;
    }
    // Transcode OR container-remux, both served via start.mkv. If the SOURCE video is
    // direct-playable (h264/hevc) we only reached here because the container isn't streamable, so
    // ask Plex to REMUX — copy both codecs into MKV, no re-encode (keeps 4K + HDR10); the Load
    // payload then uses the SOURCE codecs. Otherwise it's a real RE-ENCODE to the profile's
    // target chain (hevc first when the SoC decodes it — keeps 4K + HDR10 — else h264; see
    // profile_for). The guess below is only the /decision-unreachable fallback: decision_codecs
    // overrides it with the server's ACTUAL output, but the guess still tracks devcaps because
    // a payload naming hevc on a SoC without the decoder configures a pipeline that cannot start.
    if video_dp {
        let achosen = audio_sel.as_ref().map(|(_, c, _)| c.clone()).unwrap_or_else(|| acodec.to_string());
        plan.vcodec = vcodec.to_string();
        plan.acodec = achosen;
    } else {
        plan.vcodec = crate::devcaps::caps().encode_vcodec().into();
        plan.acodec = "ac3".into();
    }
    // Carry the picked SOURCE track into the server-side selection (put_selection +
    // &audioStreamID on the transcode query): the remux copies — and the re-encode encodes —
    // the CHOSEN track instead of the part default. The demuxer is NOT pointed at a source
    // ordinal here (the old set_audio_track(aidx) indexed the SERVER's output, whose stream
    // layout is the transcoder's, not the source's) — the payload-codec match finds the lane.
    if let Some((_, _, asid)) = &audio_sel {
        plan.audio_sid = *asid;
    }
    // keep the flavor so a later seek rebuilds the same query for start.mkv?...&offset=T
    plan.remux = video_dp;
    put_selection(plan.part_id, env.audio_sid, env.sub_sid); // audio/subtitle selection drives the encode/remux + burn
    let sp = transcode_spec(rk, &session, video_dp, -1, env.audio_sid, env.sub_sid);
    if let Some(mc) = client.transcode_decision(&sp) {
        // The server has already answered, and it is allowed to answer NO. Stop here rather than
        // stream a `start.mkv` it has just said it cannot produce: the plan leaves with no URL —
        // the ordinary "this did not resolve" failure — and carries the verdict so the read-out can
        // quote the server's own sentence instead of the generic "Playback failed" this used to be.
        if let Some(v) = refusal(&mc) {
            crate::player::log(&format!(
                "decision: REFUSED general={:?} transcode={:?} — {v}",
                mc.general_decision_code, mc.transcode_decision_code
            ));
            plan.verdict = Some(v);
            return plan;
        }
        // the Load payload must match the server's ACTUAL output codecs
        if let Some((v, a)) = decision_codecs(&mc) {
            plan.vcodec = v;
            plan.acodec = a;
        }
    }
    plan.url = client.transcode_start_url(&sp).to_url();
    plan.tsession = session;
    plan
}

/// Preferred audio language (ISO-639 code). Content is often authored with a foreign default
/// dub (e.g. The Office ships a Russian "kubik" track flagged default); we prefer the English
/// track when the item has one, rather than following the file's default flag.
const PREF_AUDIO_LANG: &str = "eng";

/// Pick the audio track to DIRECT-PLAY from the playing item's track store
/// (metadata::playing(), loaded by build_stream), returning (list_idx, codec, stream_id):
/// list_idx -1 = codec-default (demuxer matches by payload codec — only when the track list is
/// unavailable), else the index into `playing().audio`, with that track's Plex stream id so the
/// timeline can report the truth. Order of preference:
///   1. the stream the SERVER already has selected for this part (PMS `Stream.selected`), when
///      that selection is a real CHOICE and direct-playable — a track picked on another Plex
///      client (phone, web, another TV) or here in an earlier session outranks our own defaults,
///      which used to silently overwrite it on every play;
///   2. a direct-playable track in PREF_AUDIO_LANG (English), so English shows don't open in a
///      foreign default dub — the Load payload uses THAT track's codec so there is no mismatch;
///   3. the file's flagged default track, if its codec is direct-playable — by EXPLICIT index
///      (matching by codec alone fed the first same-codec stream, not the flagged default, when
///      another track of that codec preceded it);
///   4. any other direct-playable track (TrueHD/DTS-default item with an AC3 sibling — smart-DP).
/// None when NO audio track is direct-playable (→ transcode).
///
/// Rung 1 carries TWO gates, and both are load-bearing, because PMS reports a selected AUDIO
/// stream on essentially every part — there is no "nothing selected" state for audio (verified
/// against the live server: parts this client has never PUT a selection for still come back with
/// the file's default flagged `selected`).
///   - **It must differ from the file's `default` flag.** A selection that merely echoes the
///     container default is not evidence that anyone chose anything, and honouring it verbatim
///     would delete the English rung below — whose whole reason to exist is that a foreign dub is
///     often the file default (The Morning Show reports its Russian default as `selected`). When
///     the server's pick is a DIFFERENT stream, something actually chose it: a user on another
///     client, or this app's own `put_selection` in an earlier session. The cost of the gate is
///     that a choice which LANDS on the default is indistinguishable from no choice at all and
///     falls through to the ladder — that covers both an account-language preference matching the
///     default and a user here picking the default-flagged track by hand, so neither round-trips.
///     Fixing it needs state the part does not carry: the account's own defaultAudioLanguage, or
///     a remembered per-item pick. Both are separate gaps; neither is guessable from this flag.
///   - **It must be direct-playable.** Otherwise we fall through instead of forcing a transcode to
///     obey it, which would drop the whole smart-direct-play class (a TrueHD/DTS pick with an AC3
///     sibling) onto the server's video-downscaling encoder for one audio track.
/// PURE: takes the playing item's audio tracks explicitly instead of reaching into
/// `metadata::playing()`. That matters twice over. (a) `playing()` hands out a `&'static
/// PlayingItem` whose `Vec`s `ui/track_menu.rs` and `ui/info_panel.rs` hold slices into during
/// playback — a worker replacing the store would drop those out from under the draw path, so the
/// resolve must never touch it. (b) Being pure makes the selection ladder host-testable, which it
/// has never been; see the tests at the foot of this file.
fn pick_dp_audio(tracks: &[crate::metadata::Stream], default_acodec: &str) -> Option<(i32, String, i64)> {
    let dp = crate::plex::is_dp_audio;
    if tracks.is_empty() {
        // no track info — fall back to the codec-default (or transcode if that isn't DP)
        return if dp(default_acodec) { Some((-1, default_acodec.to_string(), 0)) } else { None };
    }
    let pick = |i: usize| (i as i32, tracks[i].codec.to_lowercase(), tracks[i].id);
    // 1. the server's own current selection, when it is a real pick (differs from the file's
    //    default flag — see the doc) and direct-playable: honours a choice made elsewhere
    if let Some(i) = tracks.iter().position(|s| s.selected && !s.default && dp(&s.codec.to_lowercase())) {
        return Some(pick(i));
    }
    // 2. preferred-language, direct-playable
    if let Some(i) = tracks.iter().position(|s| dp(&s.codec.to_lowercase()) && s.lang_code == PREF_AUDIO_LANG) {
        return Some(pick(i));
    }
    // 3. the file's flagged default track, if direct-playable (explicit index)
    if let Some(i) = tracks.iter().position(|s| s.default && dp(&s.codec.to_lowercase())) {
        return Some(pick(i));
    }
    if dp(default_acodec) && !tracks.iter().any(|s| s.default) {
        // Media[0].audioCodec is DP but no stream carries the default flag — codec-match
        return Some((-1, default_acodec.to_string(), 0));
    }
    // 4. any direct-playable track (smart direct-play over a non-DP default)
    tracks.iter().position(|s| dp(&s.codec.to_lowercase())).map(pick)
}

/// The subtitle to turn ON at the start of a DIRECT-PLAY, from the server's own per-part
/// selection — returning (stream id, embedded-subtitle ordinal for the client renderer), or
/// None to start with subtitles off (the shipped behaviour when the server has no selection).
///
/// This is the read-back half of `put_selection`: we have always written the user's pick to
/// `/library/parts/…` and never consulted the one already there, so a subtitle enabled from Plex
/// Web or a phone was dropped on the floor at every play. The ordinal is
/// `metadata::sub_render_ordinal`, i.e. the SAME identifier space the track menu commits and the
/// demuxer enumerates (embedded streams only, sorted on PMS `Stream.index`) — not a list position.
///
/// Unlike the audio rung this carries no "is it a real pick?" gate, because subtitles do have a
/// "nothing selected" state and use it: probed against the live server, parts carrying a
/// `default`-flagged subtitle come back with no selection at all, so a selection is a choice even
/// when it lands on the container default. The case that would blur it is an ACCOUNT-level
/// subtitle mode (always-show / auto-select forced), which makes PMS select a stream nobody
/// picked on this part — subtitles would then come up on every direct play of a foreign-audio
/// item. That is self-correcting (turning them off PUTs `subtitleStreamID=0`, which is a real
/// per-part override) and it is arguably the account setting working, but if it ever needs
/// suppressing, the gate belongs here — not on the flag itself.
///
/// Two deliberate limits, both about what the client renderer can actually deliver:
///   - an EXTERNAL (sidecar) selection returns None. It is not in the container, so nothing would
///     render; only a server burn can show it, and silently forcing a transcode to obey a stored
///     flag is not a trade the user asked for.
///   - this is the direct-play path only. The transcode path keeps PUTting `subtitleStreamID=0`
///     (subs off) as before: honouring a selection there means a server-side BURN, i.e. a
///     re-encode carrying a picture-quality cost, which is a trade to put behind the settings
///     surface this app does not have yet rather than to make silently at every play. Once a
///     direct-played item DOES go to the transcoder mid-session (a DTS/TrueHD audio pick), the
///     seeded `CUR_SUB_SID` rides along, so the subtitle already on screen keeps burning. Note the
///     read-back is therefore ONE-WAY on that path: an item that starts as a transcode still PUTs
///     `subtitleStreamID=0`, which not only suppresses the burn but CLEARS the server's selection
///     for everyone. That predates this change; honouring it instead is the same burn decision.
fn pick_dp_subtitle(subs: &[crate::metadata::Stream]) -> Option<(i64, i32)> {
    let i = subs.iter().position(|s| s.selected && !s.external)?;
    let ord = crate::metadata::sub_render_ordinal(subs, i);
    // Both halves must be usable or neither is: the id is what the menu checkmark and the
    // timeline report key on, so rendering a stream we cannot NAME would show a subtitle while
    // the menu says Off. (`ord < 0` is unreachable through the `!external` filter above — it is
    // kept so a change on either side degrades to "off" instead of feeding the renderer a -1.)
    if ord < 0 || subs[i].id <= 0 {
        return None;
    }
    Some((subs[i].id, ord))
}

/// PURE: the local direct-play VIDEO test — the codec and the source's stated frame size must
/// BOTH clear the device's own decode table (`devcaps`).
///
/// The codec half: h264 unconditionally (every webOS SoC decodes it), hevc only when the table
/// lists the decoder — anything else the pipeline cannot feed at all. The resolution half is the
/// local agreement with the profile's `*`-scoped `video.width`/`video.height` limitation: the
/// profile makes PMS transcode a 4K source down for a 1080p-bounded SoC, but the smart-DP branch
/// never asks PMS, so without this test a 4K file with one direct-playable audio track was fed
/// verbatim to a decoder whose table says 1920x1088 — the wrong-side failure devcaps' own doc
/// names (issue #22's over-claim class), invisible on the dev TV, whose bound is 4096x2176.
///
/// Unknown dimensions (0) PASS: PMS omitting a Media attribute is not evidence of 4K, and
/// failing open is yesterday's behavior for every file the server never measured — the same
/// misread-degrades-to-assumed rule `devcaps::parse` applies.
fn video_direct_plays(vcodec: &str, src_w: i64, src_h: i64, caps: &crate::devcaps::Caps) -> bool {
    let codec_ok = vcodec == "h264" || (vcodec == "hevc" && caps.hevc);
    let (bw, bh) = caps.hevc_max;
    codec_ok && src_w <= bw as i64 && src_h <= bh as i64
}

/// The detail page's "how this plays" answer, BEFORE anything is played — the same three gates
/// `build_stream` will apply (codec+resolution via [`video_direct_plays`], container via
/// [`part_is_streamable`], one direct-playable audio track), asked of the loaded `Detail`.
/// An approximation by design: the real decision can still consult the server (`server_decision`
/// when no DP audio track is found), so this leans the same way that fallback usually lands.
/// It exists for `Details Screen.dc.html`'s facts row and must stay a READ-ONLY preview —
/// nothing in the playback path may branch on it (the path re-derives for itself).
///
/// **THREE answers, not two, and the third is the one a two-valued preview got wrong.** "The
/// server has to do something" and "the server has to re-encode the picture" are different facts
/// (`is_remux`'s doc says so for the LIVE session; this is the same distinction before Play), and
/// the UI hangs a Plex Pass claim on the difference: hardware conversion and HDR tone mapping are
/// both properties of an ENCODE, so naming either one for a stream where no encoder runs points
/// the user at a purchase that would fix nothing — `player::error_shape`'s own rule, and the
/// polarity issue #22 is about.
#[derive(PartialEq, Clone, Copy, Debug)]
pub(crate) enum Preview {
    DirectPlay,
    /// Container-only REMUX — Plex's own "Direct Stream". The video (and usually the audio) is
    /// COPIED into progressive MKV because the container is not one the demuxer streams, or
    /// because no audio track direct-plays; the pixels arrive untouched, 4K and HDR10 intact.
    /// `build_stream` spells this exact case `plan.remux = video_dp` on the transcode branch.
    Remux,
    /// A real re-encode: the server decodes and re-encodes the video.
    Converts,
}
pub(crate) fn playback_preview(d: &crate::metadata::Detail) -> Option<Preview> {
    // A SHOW's container carries no file of its own, so the page answers for the episode its Play
    // button would start — the one the hero is already about. Its frame size and audio list are
    // the show Detail's, which `fetch_item_streams` backfilled from that same episode.
    let (part, vcodec) = match d.on_deck.as_ref().filter(|_| d.part.is_empty()) {
        Some(ep) => (ep.part.as_str(), ep.vcodec.as_str()),
        None => (d.part.as_str(), d.vcodec.as_str()),
    };
    playback_preview_of(part, vcodec, d.width, d.height, &d.audio)
}

/// [`playback_preview`]'s pure core — the three-way answer from the fields it actually needs, so
/// a caller holding an EPISODE's file and a show's stream list can ask the same question.
pub(crate) fn playback_preview_of(
    part: &str,
    vcodec: &str,
    width: i64,
    height: i64,
    audio_streams: &[crate::metadata::Stream],
) -> Option<Preview> {
    if part.is_empty() {
        return None; // nothing playable loaded (a show still resolving its episode)
    }
    let video = video_direct_plays(vcodec, width, height, crate::devcaps::caps());
    let audio = audio_streams.iter().any(|a| crate::plex::is_dp_audio(&a.codec));
    // Mirrors `build_stream`'s own ladder: the video gate decides whether an ENCODER runs at all,
    // and only once it has passed do the container and the audio decide between pulling the file
    // ourselves and asking the server to repackage it.
    Some(if !video {
        Preview::Converts
    } else if part_is_streamable(part) && audio {
        Preview::DirectPlay
    } else {
        Preview::Remux
    })
}

/// True when the part's container is one the buffer-feed demuxer streams over HTTP: MKV, or
/// MP4/M4V since the AVIO became seekable (see the `streamable` note at the decision site — the
/// old mkv-only gate was measured obsolete on-device 2026-08-11). Other containers (mov/avi/…)
/// are sent to Plex for a container remux instead of direct-play. Matches the container
/// extension in the part-key filename; the m4v spelling is the same mov demuxer and the same
/// `container=mp4` in PMS metadata.
fn part_is_streamable(part_key: &str) -> bool {
    let name = part_key.rsplit('/').next().unwrap_or(part_key);
    let name = name.split('?').next().unwrap_or(name);
    name.ends_with(".mkv") || name.ends_with(".mp4") || name.ends_with(".m4v")
}

/// Extract the numeric Part id from a Plex part key (/library/parts/{id}/…/file.mkv).
fn part_id_of(part_key: &str) -> i64 {
    let mut it = part_key.split('/');
    while let Some(seg) = it.next() {
        if seg == "parts" {
            return it.next().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
        }
    }
    0
}

// ---- async resolve: worker computes an owned Plan, main thread installs it ------------------
// The house idiom (metadata::load_season / browse.rs): generation counter + single-flight +
// a monotone one-slot mailbox + a per-frame pump that applies on the MAIN thread.
//
// Cancellation is FLAG-ONLY by design: `cancel_play` bumps the generation so a landing is
// discarded, but it cannot wake a worker blocked in recv(2) — publishing the socket fd to make
// that possible broke the seek path and was reverted (docs/async-model-decision.md). That costs
// nothing here: the freeze is fixed by getting the resolve OFF the loop, and a worker lingering
// in the background is invisible once the UI has already moved on.
static PLAY_GEN: AtomicU32 = AtomicU32::new(0);
static PLAY_BUSY: AtomicBool = AtomicBool::new(false);
static PLAY_SLOT: Mutex<Option<(u32, Plan, String)>> = Mutex::new(None);

/// True while a resolve is in flight — the HUD renders `PlaybackState::Resolving` from this.
pub(crate) fn play_pending() -> bool { PLAY_BUSY.load(Ordering::SeqCst) }

/// MAIN THREAD, NON-BLOCKING. Publishes the HUD strings immediately, supersedes any in-flight
/// resolve, and spawns a worker. The caller flips the route this same frame.
pub(crate) fn request_play(rk: &str, part: &str, vcodec: &str, acodec: &str, title: &str, ctx: &str) {
    if part.is_empty() && rk.is_empty() {
        return;
    }
    unsafe {
        set_c(addr_of_mut!(TITLE) as *mut c_char, 128, title);
        set_c(addr_of_mut!(CTXLINE) as *mut c_char, 96, ctx);
        addr_of_mut!(CUR_AUDIO_SID).write(0);
        addr_of_mut!(CUR_SUB_SID).write(0);
        // Retire the OUTGOING item's queue before its successor resolves: this names the episode
        // after the one that WAS playing, and leaving it up would offer the Up Next control a
        // stale "next" for the whole resolve window — including, when the user just started that
        // very episode from here, the one now on screen. The retained rows go with it, for the
        // same reason and because a fresh `Vec` also hands their strings back to the allocator.
        *addr_of_mut!(UP_NEXT) = None;
        *addr_of_mut!(QUEUE) = Vec::new();
        // …and the PREVIOUS item's refusal, for the same reason and one more: `player::state()`
        // derives `Error` from it, so a verdict left standing would put the failure read-out over
        // the item now being resolved. `play_pending()` outranks it for this frame either way, but
        // a resolve that never lands (a refused spawn) would leave nothing else to clear it.
        *addr_of_mut!(PLAY_VERDICT) = None;
    }
    // …and the outgoing item's track/marker/chapter store, for exactly the reason above: it stays
    // the PREVIOUS leaf's until this resolve lands. See `metadata::retire_playing_item`.
    crate::metadata::retire_playing_item();
    crate::player::reset_audio_track();
    crate::player::reset_subtitle();
    // captured HERE, on the main thread, and moved into the worker — see ResolveEnv
    let env = ResolveEnv::snapshot(rk);
    let gen = PLAY_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    PLAY_BUSY.store(true, Ordering::SeqCst);
    let (rk, part, vc, ac) = (rk.to_string(), part.to_string(), vcodec.to_string(), acodec.to_string());
    let spawned = crate::task::spawn_small("resolve", move || {
        // catch_unwind OUTSIDE the mailbox write, like load_season: a panicking resolve must still
        // land (as !ok) or PLAY_BUSY latches and the screen wedges on a spinner forever.
        let plan = std::panic::catch_unwind(|| build_stream(&rk, &part, &vc, &ac, &env))
            .unwrap_or_default();
        let mut slot = PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner());
        // MONOTONE: an older resolve landing late must never clobber a newer unconsumed one.
        if slot.as_ref().map(|(g, _, _)| *g < gen).unwrap_or(true) {
            *slot = Some((gen, plan, rk));
        }
    });
    if !spawned {
        // there is no worker, so nothing will ever land: releasing this is what keeps the screen
        // from wedging on a spinner that can never resolve
        PLAY_BUSY.store(false, Ordering::SeqCst);
    }
}

/// ASYNC twins of `play_movie` / `play_episode`: identical HUD strings and inputs, but the
/// network work runs on a worker and the caller flips the route THIS frame. `app.rs` drains
/// `pump_play` once a frame and starts the engine when the plan lands.
pub(crate) fn request_play_movie(m: &PmsMovie) {
    if m.part.is_empty() {
        return;
    }
    let rating = if m.rating.is_empty() { "NR" } else { &m.rating };
    let ctx = format!("{} \u{b7} {} \u{b7} {}", m.year, rating, crate::ui::fmt::dur_short(m.dur_ns / 1_000_000));
    request_play(&m.rk, &m.part, &m.vcodec, &m.acodec, &m.title, &ctx);
}

/// Start the queued next episode. Takes the descriptor BY VALUE, and that is load-bearing rather
/// than stylistic: [`up_next`] hands out a `&'static`, `request_play` clears `UP_NEXT` as its
/// first act, and a `&UpNext` argument would therefore be pointing at a dropped `String` by the
/// time this reads it — an aliasing bug the borrow checker cannot see through a `'static`. Callers
/// clone (`route::up_next().cloned()`); the signature is what forces them to.
///
/// The HUD strings mirror the episode layout `draw_hud` uses once `now_playing` lands, so the
/// pre-roll doesn't change shape underneath the user when it does.
pub(crate) fn request_play_up_next(u: UpNext) {
    let ctx = crate::ui::fmt::episode_kicker(u.season, u.index, &u.ep_title);
    let title = if u.show_title.is_empty() { &u.ep_title } else { &u.show_title };
    request_play(&u.rk, &u.part, &u.vcodec, &u.acodec, title, &ctx);
}

/// Supersede an in-flight resolve (BACK during a load). The landing is dropped by generation.
pub(crate) fn cancel_play() {
    PLAY_GEN.fetch_add(1, Ordering::SeqCst);
    PLAY_BUSY.store(false, Ordering::SeqCst);
    *PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    // …and the refusal, because this is the statement that the withdrawn request is over. It is
    // the ONLY place that can retire one: a refused plan builds no engine, so `scrobble_stop` —
    // where the rest of the session state is cleared — is never reached (teardown returns at
    // `engine_take`). Both callers are exactly "playback is being abandoned": `exit_player` and
    // the app-switch to background.
    clear_play_verdict();
}

/// MAIN THREAD, once a frame. Returns true when a fresh plan was installed and playback should
/// start. A stale landing (superseded) is dropped.
pub(crate) fn pump_play() -> bool {
    let taken = PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some((gen, plan, rk)) = taken else { return false };
    if gen != PLAY_GEN.load(Ordering::SeqCst) {
        return false; // superseded while in flight
    }
    PLAY_BUSY.store(false, Ordering::SeqCst);
    let ok = !plan.url.is_empty();
    apply_plan(plan, &rk);
    ok
}

/// MAIN THREAD ONLY: the sole writer of the route statics + the player's audio-track request.
/// Everything here was previously written from inside `build_stream`, i.e. from whatever thread
/// ran it.
fn apply_plan(plan: Plan, rk: &str) {
    crate::metadata::install_playing(plan.playing);
    // main thread only — `up_next()`/`queue()` hand out `&'static`s (see their docs). The rows
    // arrive already projected: the worker never retained a `Metadata` tree to install here.
    unsafe {
        *addr_of_mut!(UP_NEXT) = plan.up_next;
        *addr_of_mut!(QUEUE) = plan.queue;
        // Installed on EVERY landing, not only a refusing one: a plan that resolved is itself the
        // statement that the last refusal is over, and assigning unconditionally is what makes that
        // true without a second clear anyone can forget.
        *addr_of_mut!(PLAY_VERDICT) = plan.verdict;
    }
    // Warm the next episode's still NOW rather than at first draw. The URL has been known since
    // this plan resolved — tens of minutes before the credits — and the fetch is async, so touching
    // it here costs nothing and spares the control a skeleton for one image-transcode round trip at
    // exactly the moment it appears in front of the user. `warm_tex`, not `resolve_tex`: this wants
    // the fetch and nothing else, and a slot warmed tens of minutes early must NOT be carrying the
    // evict-protection a draw takes (see `posters::poster_warm`). At the tile's OWN 480×270 —
    // `(path, w, h, png)` IS the store key, so a warm at any other size buys nothing.
    if let Some(u) = up_next() {
        crate::ui::widgets::warm_tex(&u.thumb, 480, 270, 0);
    }
    if !plan.vcodec.is_empty() {
        set_stream_codecs(&plan.vcodec, &plan.acodec); // the pair is only ever set together
        // …and what the FILE is, before any /decision output overwrites the pair above
        set_source_codecs(&plan.src_vcodec, &plan.src_acodec);
    }
    unsafe {
        addr_of_mut!(CUR_PART_ID).write(plan.part_id);
        *addr_of_mut!(SESS) = plan.sess;
        if !plan.machine_id.is_empty() {
            *addr_of_mut!(MACHINE_ID) = plan.machine_id;
        }
        *addr_of_mut!(PQ_ID) = plan.pq_id;
        *addr_of_mut!(PQ_ITEM_ID) = plan.pq_item_id;
        *addr_of_mut!(STREAM_FPS) = plan.fps;
        addr_of_mut!(CUR_AUDIO_SID).write(plan.audio_sid);
        // the server-selected subtitle (0 = none), so the menu checkmark, the timeline report and
        // any later transcode of this item all agree with what the renderer was told below
        addr_of_mut!(CUR_SUB_SID).write(plan.sub_sid);
        addr_of_mut!(CUR_REMUX).write(plan.remux);
        *addr_of_mut!(URL) = plan.url;
        *addr_of_mut!(TSESSION) = plan.tsession;
        *addr_of_mut!(CUR_RK) = rk.to_string();
    }
    // SHARED.desired_audio_idx is read by the DEMUX THREAD on every reopen — main thread only.
    if let Some(ord) = plan.feed_audio_ordinal {
        crate::player::set_audio_track(ord);
    }
    // `request_play` turned subtitles off for the new item; turn the server's selection back on
    // AFTER that reset (this lands a frame or more later, on the main thread, before the engine
    // starts — so the demuxer's per-block `desired_sub_idx` gate sees it from the first cue).
    if let Some(ord) = plan.sub_render_ordinal {
        crate::player::log(&format!("server-selected subtitle: sid={} render_idx={ord}", plan.sub_sid));
        crate::player::request_subtitle(ord);
    }
    // A landing is a DISCRETE change to what is on screen, so it owes the present gate a poke —
    // `ui::idle::invalidate`'s call-site list is that module's correctness argument. The caller
    // (`app.rs`'s pump) invalidates only when `pump_play` returns TRUE, and a REFUSING plan returns
    // false by construction (empty url) while flipping the player from Resolving to Error. That it
    // still repainted was an accident of the player route bypassing the gate entirely; here it is
    // the rule instead.
    crate::ui::idle::invalidate();
}

/// Re-transcode the current item (CUR_RK) at `offset_secs`, carrying the CURRENT audio +
/// subtitle selection (transcode_base). Used by an audio switch AND by a subtitle
/// (de)select while transcoding. Works from a direct-play OR transcode state — the result
/// is always a transcode (server always emits AC3, so the pipeline's Loaded codec is
/// unchanged). Sets URL/TSESSION/TBASE, runs /decision, and returns the new start.mkv URL
/// (the demux re-opens it from byte 0), or None.
pub(crate) fn retranscode(offset_secs: i64) -> Option<String> {
    let c = crate::plex::client_opt()?;
    let rk = unsafe { (*addr_of!(CUR_RK)).clone() };
    if rk.is_empty() {
        return None;
    }
    // NB: TSESSION becomes this synthetic marker while the transcoder QUERY keeps riding the
    // per-playback sess() — matching the shipped behavior (is_transcoding()/stop key off
    // TSESSION; the server session correlation stays on sess()).
    let session = format!("plxnative-{rk}");
    unsafe {
        addr_of_mut!(CUR_REMUX).write(false);
        *addr_of_mut!(TSESSION) = session;
    }
    // the transcode output is the profile target's head (`Caps::encode_vcodec` — the ONE
    // definition, shared with build_stream's guess and profile_for) + AC3 — record it so a
    // pipeline RELOAD (audio switch) builds a Load payload matching the re-encoded stream. A
    // guess: apply_decision_codecs below replaces it with the server's actual output, but it
    // must still track devcaps, not the dev TV (issue #22's bug class).
    set_stream_codecs(crate::devcaps::caps().encode_vcodec(), "ac3");
    put_selection(cur_part_id(), cur_audio_sid(), cur_sub_sid()); // drives the encode + burn
    let qsess = sess();
    let sp = transcode_spec(&rk, &qsess, false, offset_secs.max(0), cur_audio_sid(), cur_sub_sid());
    if let Some(mc) = c.transcode_decision(&sp) {
        apply_decision_codecs(&mc); // reload builds a fresh Load payload — match the real output
    }
    let url = c.transcode_start_url(&sp).to_url();
    set_url(&url);
    // NEVER log the URL. `transcode_start_url` ends in `X-Plex-Token=…`, and this line is reached
    // by an ordinary audio-track switch — so the app's own support channel ("send us
    // /tmp/plxnative-events.log") was asking users to paste a live PMS credential into a public
    // issue thread. The rk, the track ids and the offset are the whole diagnostic value here; the
    // URL added nothing that is not derivable from them.
    crate::player::log(&format!(
        "retranscode rk={rk} audio={} sub={} offset={offset_secs} -> transcode start",
        cur_audio_sid(),
        cur_sub_sid()
    ));
    Some(url)
}

/// Switch the audio track: set the current source audio (&audioStreamID) and re-transcode
/// at the current position (which also (re)burns the current subtitle, if one is selected).
pub(crate) fn switch_audio(stream_id: i64, offset_secs: i64) -> Option<String> {
    unsafe { addr_of_mut!(CUR_AUDIO_SID).write(stream_id) };
    // retranscode -> put_selection PUTs the audio (+ subtitle) selection server-side; the
    // transcoder encodes the part's SELECTED audio, and only a PUT changes it (a query-param
    // or GET is a no-op).
    retranscode(offset_secs)
}

// ---- selection commits: playback POLICY for the in-player track menu. The menu only reports
// what row was picked; whether that means a native stream switch, a server re-transcode, or a
// burn refresh is decided HERE, next to the codec sets and the transcode state it depends on. ----

/// Commit an audio-track pick: NATIVE switch (feed the chosen stream from the same direct-play
/// file — no transcode, keeps 4K HEVC) when the item direct-plays AND the target codec is
/// direct-playable; else a server re-transcode with that stream selected. `idx` is the
/// CONTAINER audio ordinal (the menu converts its row via metadata::audio_ordinal).
pub(crate) fn commit_audio_selection(idx: i32, codec: &str, stream_id: i64) {
    if !is_transcoding() && crate::plex::is_dp_audio(codec) {
        // record the pick: the timeline then reports the stream that actually plays, and a
        // later transcode event (subtitle burn refresh / transcode seek) keeps this track
        unsafe { addr_of_mut!(CUR_AUDIO_SID).write(stream_id) };
        // persist the USER's pick server-side (official-client behavior): /status/sessions'
        // selected-stream display keys on the part selection, not the timeline report. Only
        // user picks persist — the start-of-play auto-pick (eng preference) reports only.
        put_selection(cur_part_id(), cur_audio_sid(), cur_sub_sid());
        crate::player::request_audio_track(idx, codec);
    } else {
        crate::player::request_audio_switch(stream_id);
    }
}

/// Commit a subtitle pick (`sub_idx` -1 = Off): gate the client-side renderer (direct-play path)
/// and select the burn stream for any transcode of the item — refreshing a live transcode so the
/// server re-burns (or drops) it.
pub(crate) fn commit_subtitle_selection(sub_idx: i32, stream_id: i64) {
    crate::player::request_subtitle(sub_idx);
    set_subtitle(stream_id);
    if is_transcoding() {
        crate::player::request_transcode_refresh(); // retranscode PUTs the selection itself
    } else {
        // persist the pick server-side (and subs Off PUTs subtitleStreamID=0, clearing a
        // stale server-side selection that would otherwise burn on the next transcode)
        put_selection(cur_part_id(), cur_audio_sid(), cur_sub_sid());
    }
}

/// POST one /:/timeline progress report for `rk`, carrying this playback's session + PlayQueue
/// + selected-stream state — so /status/sessions shows the right track and the Direct Play vs
/// Transcode badge. The ONE timeline call site (the ~10s reporter thread and the final
/// state=stopped report both come through here).
pub(crate) fn report_timeline(rk: &str, state: crate::plex::TimelineState, t_ms: i64, d_ms: i64) {
    let c = match crate::plex::client_opt() {
        Some(c) => c,
        None => return,
    };
    let (session, pq, pqi) = (sess(), pq_id(), pq_item_id());
    c.timeline(&crate::plex::TimelineReport {
        rating_key: rk,
        state,
        time_ms: t_ms,
        duration_ms: d_ms,
        session: &session,
        play_queue_id: &pq,
        play_queue_item_id: &pqi,
        audio_stream_id: cur_audio_sid(),
        subtitle_stream_id: cur_sub_sid(),
    });
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// `part_id_of` gates the server-side stream selection: `put_selection` returns early on
    /// `<= 0`, so a parse miss silently disables subtitle suppression and audio selection for
    /// the whole item — no error, no log line, just a burned-in subtitle nobody asked for.

    // ---- pick_dp_audio: the direct-play audio selection ladder ------------------------------
    // Never host-testable before: it read `metadata::playing()`'s `&'static` store. Making it
    // take the tracks explicitly (step 6 of docs/async-model-decision.md) turned the ladder into
    // a pure function, and these pin the order the comments claim.

    fn trk(id: i64, codec: &str, lang: &str, default: bool) -> crate::metadata::Stream {
        crate::metadata::Stream {
            id,
            index: id,
            lang: String::new(),
            lang_code: lang.into(),
            codec: codec.into(),
            channels: 2,
            layout: String::new(),
            title: String::new(),
            sdh: false,
            ad: false,
            forced: false,
            default,
            external: false,
            selected: false,
        }
    }

    /// Mark a track as the server's CURRENT pick (PMS `Stream.selected`) — the flag a pick made
    /// on a phone / Plex Web / another TV arrives on.
    fn server_selected(mut s: crate::metadata::Stream) -> crate::metadata::Stream {
        s.selected = true;
        s
    }

    /// A subtitle stream, spelled out because the ordinal maths depends on `index` (container
    /// order, which PMS may report out of document order) and on `external` (sidecars are not in
    /// the container at all, so the client renderer cannot count them).
    fn sub(id: i64, index: i64, lang: &str, external: bool) -> crate::metadata::Stream {
        crate::metadata::Stream { index, external, ..trk(id, "srt", lang, false) }
    }

    #[test]
    fn an_empty_track_list_falls_back_to_the_codec_default() {
        assert_eq!(pick_dp_audio(&[], "ac3").map(|(i, c, _)| (i, c)), Some((-1, "ac3".into())));
        assert!(pick_dp_audio(&[], "truehd").is_none(), "a non-direct-playable default must transcode");
    }

    #[test]
    fn english_wins_over_the_files_default_track() {
        // The Office ships a Russian "kubik" track flagged default; we must not open in it.
        let tracks = [trk(1, "ac3", "rus", true), trk(2, "ac3", "eng", false)];
        assert_eq!(pick_dp_audio(&tracks, "ac3"), Some((1, "ac3".into(), 2)));
    }

    #[test]
    fn the_flagged_default_wins_when_no_english_track_is_direct_playable() {
        let tracks = [trk(1, "ac3", "deu", false), trk(2, "ac3", "fra", true)];
        assert_eq!(pick_dp_audio(&tracks, "ac3"), Some((1, "ac3".into(), 2)));
    }

    #[test]
    fn smart_dp_takes_a_playable_sibling_over_a_non_playable_default() {
        // A 4K HEVC item: TrueHD default + an AC3 sibling — direct-play beats the server's
        // video-downscaling transcode.
        let tracks = [trk(1, "truehd", "eng", true), trk(2, "ac3", "eng", false)];
        assert_eq!(pick_dp_audio(&tracks, "truehd"), Some((1, "ac3".into(), 2)));
    }

    #[test]
    fn no_direct_playable_track_means_transcode() {
        let tracks = [trk(1, "truehd", "eng", true), trk(2, "dts", "eng", false)];
        assert!(pick_dp_audio(&tracks, "truehd").is_none());
    }

    // ---- rung 1: the selection the SERVER already holds --------------------------------------
    // `Stream.selected` is the part's current pick — what `put_selection` writes and what a pick
    // made on a phone / Plex Web / another TV shows up as. We wrote it for a long time and never
    // read it, so our own ladder silently overwrote every cross-client choice on the next play.
    // The shapes below are the ones the live server actually serves (probed per-identity while
    // this landed), which is where the two gates on the rung come from.

    #[test]
    fn the_servers_selected_track_outranks_the_english_preference() {
        // A user picks the second Russian dub on their phone. English is still the
        // FIRST direct-playable track, so the old ladder handed back English on every play.
        let tracks = [
            trk(2693, "ac3", "rus", true),
            server_selected(trk(2694, "ac3", "rus", false)),
            trk(2695, "ac3", "eng", false),
        ];
        assert_eq!(pick_dp_audio(&tracks, "ac3"), Some((1, "ac3".into(), 2694)));
    }

    #[test]
    fn a_selection_that_only_echoes_the_files_default_does_not_beat_english() {
        // THE gate that keeps the English rung alive. PMS reports a selected audio stream on
        // every part — for one nobody has touched it is just the container's default flag coming
        // back (The Morning Show: the Russian default reads `selected`). Treating that as a
        // choice would reinstate exactly the foreign-dub-on-open bug rung 2 exists to prevent.
        let tracks = [
            server_selected(trk(10975, "eac3", "rus", true)),
            trk(10976, "eac3", "eng", false),
        ];
        assert_eq!(pick_dp_audio(&tracks, "eac3"), Some((1, "eac3".into(), 10976)));
    }

    #[test]
    fn a_selected_track_that_cannot_direct_play_falls_through_to_the_ladder() {
        // A live shape off the server: it holds the English DTS track (a real pick — it is
        // not the file default), which this pipeline cannot decode. Honouring it would force a
        // whole-video transcode for one audio track, so the ladder runs on instead.
        let tracks = [
            trk(2663, "ac3", "rus", true),
            server_selected(trk(2669, "dca", "eng", false)),
            trk(2673, "ac3", "eng", false),
        ];
        assert_eq!(pick_dp_audio(&tracks, "dca"), Some((2, "ac3".into(), 2673)));
    }

    /// The whole ladder, rung by rung, with the selected flag switched on and off — the order is
    /// the contract, and every row here is a shape the live server actually serves.
    #[test]
    fn the_audio_ladder_walks_its_rungs_in_order() {
        let cases: [(&str, Vec<crate::metadata::Stream>, &str, Option<(i32, String, i64)>); 7] = [
            (
                "rung 1: a real server pick wins even against English",
                vec![
                    trk(1, "eac3", "rus", true),
                    server_selected(trk(2, "eac3", "deu", false)),
                    trk(3, "eac3", "eng", false),
                ],
                "eac3",
                Some((1, "eac3".into(), 2)),
            ),
            (
                "rung 1 needs a real pick: the default echoed back is not one",
                vec![server_selected(trk(1, "eac3", "rus", true)), trk(2, "eac3", "eng", false)],
                "eac3",
                Some((1, "eac3".into(), 2)),
            ),
            (
                "rung 1 is skipped when the pick can't direct-play, not obeyed by transcoding",
                vec![
                    trk(1, "ac3", "rus", true),
                    server_selected(trk(2, "dca", "eng", false)),
                    trk(3, "ac3", "eng", false),
                ],
                "ac3",
                Some((2, "ac3".into(), 3)), // rung 2 (English) still applies
            ),
            (
                "rung 2: no selection at all → the English preference, as before",
                vec![trk(1, "ac3", "rus", true), trk(2, "ac3", "eng", false)],
                "ac3",
                Some((1, "ac3".into(), 2)),
            ),
            (
                "rung 3: no English → the file's flagged default",
                vec![trk(1, "ac3", "deu", false), trk(2, "ac3", "fra", true)],
                "ac3",
                Some((1, "ac3".into(), 2)),
            ),
            (
                "rung 4: a selected non-DP track with only a foreign DP sibling — smart-DP",
                vec![server_selected(trk(1, "truehd", "eng", false)), trk(2, "ac3", "fra", false)],
                "truehd",
                Some((1, "ac3".into(), 2)),
            ),
            (
                "nothing direct-playable, selected or not → transcode",
                vec![server_selected(trk(1, "truehd", "eng", false)), trk(2, "dts", "rus", true)],
                "truehd",
                None,
            ),
        ];
        for (what, tracks, acodec, want) in cases {
            assert_eq!(pick_dp_audio(&tracks, acodec), want, "{what}");
        }
    }

    // ---- pick_dp_subtitle: the read-back half of put_selection -------------------------------

    #[test]
    fn the_selected_subtitle_resolves_to_the_renderers_embedded_ordinal() {
        // Document order is NOT container order and a sidecar sits in the middle of the list:
        // the renderer counts only embedded streams, sorted on PMS `Stream.index` — the same
        // identifier space the track menu commits (metadata::sub_render_ordinal).
        let subs = [
            sub(10, 7, "fra", true),  // sidecar — not in the container, not counted
            sub(11, 3, "rus", false), // embedded, container-first
            server_selected(sub(12, 4, "eng", false)),
        ];
        assert_eq!(pick_dp_subtitle(&subs), Some((12, 1)));
    }

    #[test]
    fn an_external_selected_subtitle_is_left_off() {
        // A sidecar can only be shown by a server burn; forcing a transcode to obey a stored
        // flag is not a trade the user asked for, so the direct-play path leaves subs off.
        let subs = [server_selected(sub(10, 3, "eng", true)), sub(11, 4, "rus", false)];
        assert_eq!(pick_dp_subtitle(&subs), None);
    }

    #[test]
    fn no_selected_subtitle_means_subtitles_stay_off() {
        assert_eq!(pick_dp_subtitle(&[]), None);
        let subs = [sub(10, 3, "eng", false), sub(11, 4, "rus", false)];
        assert_eq!(pick_dp_subtitle(&subs), None, "the file's own tracks are not an instruction");
    }

    #[test]
    fn a_selection_with_no_stream_id_is_left_off_rather_than_half_applied() {
        // id and ordinal travel together: the id is what the menu checkmark and the timeline
        // report key on, so an id-less stream would render subtitles while the menu said Off.
        let subs = [server_selected(sub(0, 3, "eng", false))];
        assert_eq!(pick_dp_subtitle(&subs), None);
    }

    // ---- video_direct_plays: the local codec + resolution direct-play gate -------------------

    /// The RESOLUTION half of the gate (issue #22's over-claim class): the smart-DP branch never
    /// asks PMS, so the profile's `*`-scoped width/height limitation cannot save a 4K source from
    /// direct-playing onto a 1080p-bounded decoder — the client must refuse it locally. Invisible
    /// on the dev TV (bound 4096x2176); this drives the gate with the reviewer-class caps.
    #[test]
    fn a_source_beyond_the_device_bound_does_not_direct_play() {
        let caps = crate::devcaps::Caps {
            hevc: true,
            hevc_max: (1920, 1088),
            vp9: false,
            audio: "aac,ac3,eac3".into(),
        };
        // the codec agrees; the frame size must still refuse — on either codec
        assert!(!video_direct_plays("h264", 3840, 2160, &caps));
        assert!(!video_direct_plays("hevc", 3840, 2160, &caps));
        // one axis over is over (per-axis bound, not an area heuristic)
        assert!(!video_direct_plays("h264", 4096, 1080, &caps));
        // within the bound plays, exactly at it included (1088 IS the table's number)
        assert!(video_direct_plays("h264", 1920, 1088, &caps));
    }

    /// Unknown dimensions fail OPEN (0 = PMS never measured the file — not evidence of 4K, and
    /// yesterday's behavior for it), while the codec half keeps gating regardless.
    #[test]
    fn unknown_dimensions_fail_open_and_the_codec_half_still_gates() {
        let caps =
            crate::devcaps::Caps { hevc: false, hevc_max: (1920, 1088), vp9: false, audio: "aac".into() };
        assert!(video_direct_plays("h264", 0, 0, &caps));
        assert!(!video_direct_plays("hevc", 1280, 720, &caps), "no decoder row, no direct play");
        assert!(!video_direct_plays("av1", 1280, 720, &caps), "the pipeline cannot feed it at any size");
    }

    #[test]
    fn part_id_is_read_from_the_parts_segment() {
        assert_eq!(part_id_of("/library/parts/98765/1712345678/file.mkv"), 98765);
        assert_eq!(part_id_of("/library/parts/1/0/file.mp4"), 1);
        // a query string rides along on the real keys
        assert_eq!(part_id_of("/library/parts/42/17/file.mkv?download=0"), 42);
    }

    #[test]
    fn part_id_is_zero_when_there_is_no_parts_segment() {
        assert_eq!(part_id_of(""), 0);
        assert_eq!(part_id_of("/library/metadata/1234"), 0);
        assert_eq!(part_id_of("/library/parts"), 0, "trailing `parts` with no id");
        assert_eq!(part_id_of("/library/parts/notanumber/file.mkv"), 0);
    }

    /// The direct-play gate: MKV and MP4/M4V parts are fed to the demuxer untouched — everything
    /// else takes the remux branch. mp4 moved sides on 2026-08-11 (issue #22): the mkv-only gate
    /// dated from an unseekable AVIO, and on a server that cannot transcode it turned every mp4
    /// into a failure.
    #[test]
    fn mkv_and_mp4_parts_are_direct_playable() {
        assert!(part_is_streamable("/library/parts/1/2/movie.mkv"));
        assert!(part_is_streamable("/library/parts/1/2/movie.mkv?x=1"), "the query must not defeat it");
        assert!(part_is_streamable("/library/parts/1/2/movie.mp4"));
        assert!(part_is_streamable("/library/parts/1/2/movie.m4v"));
        assert!(!part_is_streamable("/library/parts/1/2/movie.mov"), "mov still remuxes");
        assert!(!part_is_streamable(""));
        assert!(!part_is_streamable("/library/parts/1/2/mkv.avi"), "the extension, not a substring");
        assert!(!part_is_streamable("/library/parts/1/2/mp4.avi"), "the extension, not a substring");
    }

    /// The preview's THIRD answer, which is the one the UI hangs a Plex Pass claim on.
    ///
    /// While `Preview` had two values, everything that was not a direct play collapsed into
    /// `Converts` — and `detail::play_note` read that as "the server re-encodes the picture", which
    /// is false for the two cases below: `build_stream` answers both of them with
    /// `plan.remux = video_dp`, i.e. ask Plex to copy the codecs into MKV. So a 4K HDR HEVC file in
    /// a `.mov`, and any mkv whose only fault is an audio track that must be converted, drew
    /// "HDR → SDR · tone-mapping needs \[PLEX PASS\]" on a proven-Pass-less server while the picture
    /// arrived HDR10 intact. This grades the SPLIT; the truth table it feeds is `detail.rs`'s.
    ///
    /// The device table is `Caps::assumed` here (nothing in the host suite calls `devcaps::probe`),
    /// so h264 at 3840×2160 clears the codec and resolution gates and the container/audio halves
    /// are what move.
    #[test]
    fn the_preview_tells_a_container_remux_apart_from_a_re_encode() {
        fn item(vcodec: &str, part: &str, acodec: &str) -> crate::metadata::Detail {
            crate::metadata::Detail {
                vcodec: vcodec.to_string(),
                part: part.to_string(),
                width: 3840,
                height: 2160,
                audio: vec![crate::metadata::Stream { codec: acodec.to_string(), ..Default::default() }],
                ..Default::default()
            }
        }
        const MKV: &str = "/library/parts/1/2/file.mkv";
        const MOV: &str = "/library/parts/1/2/file.mov";
        // we pull the file ourselves — nothing on the server touches it
        assert_eq!(playback_preview(&item("h264", MKV, "aac")), Some(Preview::DirectPlay));
        // the container is one the buffer-feed demuxer cannot stream → the server REPACKAGES it
        assert_eq!(playback_preview(&item("h264", MOV, "aac")), Some(Preview::Remux));
        // …and so it does for a streamable container whose only audio track has to be converted
        assert_eq!(playback_preview(&item("h264", MKV, "truehd")), Some(Preview::Remux));
        // a codec the pipeline cannot decode at all is the only real re-encode
        assert_eq!(playback_preview(&item("vp9", MKV, "aac")), Some(Preview::Converts));
        // …including when the container and the audio would otherwise have been fine
        assert_eq!(playback_preview(&item("vp9", MOV, "truehd")), Some(Preview::Converts));
        // nothing playable loaded (a show still resolving its episode) answers nothing at all
        assert_eq!(playback_preview(&item("h264", "", "aac")), None);
    }

    /// The pre-flight refusal, graded off a real `/decision` body. Four properties, and each one is
    /// a way the old "parse it and only log it" behaviour went wrong:
    ///   * a `2000` verdict IS a refusal, and it hands back the TRANSCODE sentence — the one that
    ///     names the cause — rather than the general text that merely restates the code;
    ///   * a healthy decision (`1001`, "conversion OK") is not one, or every transcode in the
    ///     library would stop;
    ///   * a body with no verdict at all is not one either — absent is not a refusal, and it is
    ///     what an older server and every failed/unparseable fetch look like;
    ///   * a refusal with no sentence still refuses. The CODE is the decision; the text is only
    ///     the human line, and a server that stays quiet must not thereby become playable.
    #[test]
    fn a_2000_decision_is_a_refusal_and_quotes_the_reason_the_server_named() {
        fn mc(json: &[u8]) -> crate::plex::MediaContainer {
            serde_json::from_slice::<crate::plex::Envelope>(json).expect("parse").media_container
        }
        // the live PMS 1.43.3 answer for a VP9 source
        let refused = mc(br#"{"MediaContainer":{"generalDecisionCode":2000,
            "generalDecisionText":"Neither direct play nor conversion is available.",
            "transcodeDecisionCode":4007,
            "transcodeDecisionText":"Cannot convert this item. Implementation for video encoder 'vp9' not found."}}"#);
        assert_eq!(
            refusal(&refused).as_deref(),
            Some("Cannot convert this item. Implementation for video encoder 'vp9' not found."),
            "the transcode sentence names the cause; the general one only restates the code"
        );

        // only the general sentence came back — quote that instead of nothing
        let general_only = mc(br#"{"MediaContainer":{"generalDecisionCode":"2000",
            "generalDecisionText":"Neither direct play nor conversion is available."}}"#);
        assert_eq!(refusal(&general_only).as_deref(), Some("Neither direct play nor conversion is available."));

        // refused, and said nothing about why: still a stop, with no line to quote
        let silent = mc(br#"{"MediaContainer":{"generalDecisionCode":2000}}"#);
        assert_eq!(refusal(&silent).as_deref(), Some(""), "the CODE is the decision, not the text");

        // "Direct play not available; Conversion OK." — the ordinary transcode, which must proceed
        let ok = mc(br#"{"MediaContainer":{"generalDecisionCode":1001,"transcodeDecisionCode":1001,
            "transcodeDecisionText":"Direct play not available; Conversion OK."}}"#);
        assert!(refusal(&ok).is_none());

        // no verdict block at all (an older server, or a body we could not parse into one)
        assert!(refusal(&mc(br#"{"MediaContainer":{"size":1}}"#)).is_none(), "absent is not a refusal");
    }
}
