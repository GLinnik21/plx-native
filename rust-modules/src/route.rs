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
pub(crate) fn set_url(s: &str) {
    unsafe { *addr_of_mut!(URL) = s.to_owned() }
}
pub(crate) fn clear_url() {
    unsafe { (*addr_of_mut!(URL)).clear() }
}
pub(crate) fn transcode_session() -> String {
    unsafe { (*addr_of!(TSESSION)).clone() }
}
/// true while this playback is a server transcode (a live transcode session exists). Cheap
/// in-place check — the pump polls it every tick, so no String clone here.
pub(crate) fn is_transcoding() -> bool {
    unsafe { !(&*addr_of!(TSESSION)).is_empty() }
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

/// free the server-side transcode encoder if this playback was a transcode.
pub(crate) fn stop_transcode() {
    let sess = transcode_session();
    if sess.is_empty() {
        return;
    }
    if let Some(c) = crate::plex::client_opt() {
        c.transcode_stop(&sess);
    }
    unsafe {
        (*addr_of_mut!(TSESSION)).clear();
        addr_of_mut!(CUR_REMUX).write(false);
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
    // NB: do NOT explicitly /stop the old encoder here — the session id is reused, so a
    // stop would race the demux (it cuts the stream the demux is still reading; if the
    // demux hits EOF + checks seek_byte before the pump sets it, it exits). Instead the
    // pump closes the demux socket AFTER arming the seek, which drops the old connection
    // (stopping the old transcode), and this new start.mkv?&offset= (same session)
    // repositions. /decision is just a query and doesn't cut the streaming connection.
    let session = sess();
    let sp = transcode_spec(&rk, &session, unsafe { addr_of!(CUR_REMUX).read() }, offset_secs.max(0),
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
/// configured-for-AAC pipeline played silence (Hannah Montana).
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

/// Create a PlayQueue for `rk` so the session is a first-class, remote-controllable player and
/// the timeline can carry a real playQueueItemID. Best-effort: on failure the timeline still
/// works, just without the queue ids.
///
/// PURE: returns `(machine_id, pq_id, pq_item_id)` for `apply_plan` to install. `machine_id` is
/// `""` when the cached one is still good.
fn resolve_playqueue(rk: &str, session: &str, cached: &str) -> (String, String, String) {
    let mid = if cached.is_empty() {
        crate::plex::client_opt().and_then(|c| c.machine_identity()).unwrap_or_default()
    } else {
        String::new() // unchanged — apply_plan's "" means "leave the cache alone"
    };
    let effective = if mid.is_empty() { cached } else { &mid };
    if effective.is_empty() {
        crate::player::log("playqueue: no machineIdentifier (skip)");
        return (String::new(), String::new(), String::new());
    }
    match crate::plex::client_opt().and_then(|c| c.create_play_queue(effective, rk, session)) {
        Some((pq, it)) => {
            crate::player::log(&format!("playqueue: id={pq} item={it}"));
            (mid,
             if pq > 0 { pq.to_string() } else { String::new() },
             if it > 0 { it.to_string() } else { String::new() })
        }
        None => {
            crate::player::log("playqueue: POST failed");
            (mid, String::new(), String::new())
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
    pub cached_tracks: Option<crate::metadata::PlayingTracks>,
}

impl ResolveEnv {
    /// MAIN THREAD ONLY.
    fn snapshot(rk: &str) -> ResolveEnv {
        ResolveEnv {
            machine_id: unsafe { (*addr_of!(MACHINE_ID)).clone() },
            audio_sid: cur_audio_sid(),
            sub_sid: cur_sub_sid(),
            cached_tracks: crate::metadata::cached_playing(rk),
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
    pub fps: f64,
    pub audio_sid: i64,
    pub remux: bool,
    /// demuxer stream ordinal to feed (direct-play, non-default track). None = leave as-is.
    pub feed_audio_ordinal: Option<i32>,
    /// the playing item's track store, fetched off-thread and installed by apply_plan
    pub playing: Option<crate::metadata::PlayingTracks>,
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
    let mut plan = Plan { part_id: part_id_of(part), ..Default::default() };
    let client = match crate::plex::client_opt() {
        Some(c) => c,
        None => return plan,
    };
    // fresh per-playback session id (BOTH direct-play and transcode report through it) +
    // a PlayQueue so the server tracks this as a real player with a playQueueItemID.
    let session = new_sess(rk);
    plan.sess = session.clone();
    if !rk.is_empty() {
        let (mid, pq, it) = resolve_playqueue(rk, &session, &env.machine_id);
        plan.machine_id = mid;
        plan.pq_id = pq;
        plan.pq_item_id = it;
    }
    // the playing item's OWN track lists (menu + audio pick + esInfo fps read them) — the
    // loaded detail can be a different item (show page / straight-from-Home play)
    // detail already had this item's streams — no GET
    plan.playing = env.cached_tracks.clone().or_else(|| crate::metadata::fetch_playing_tracks(rk));
    // Server-adjudicated: the Media Decision Engine decides direct-play vs transcode from our
    // capability profile. Falls back to the local codec test if the server returns no usable
    // decision; the local-sample/demo path (rk empty) skips the decision entirely.
    // Server-adjudicated (Phase 2). HEVC now direct-plays (Phase 3 demuxer + native decode);
    // the guard that forced non-h264 to transcode is gone.
    // Smart direct-play: the video decodes natively (H264/HEVC) AND some audio track is
    // direct-playable (AAC/AC3/E-AC3) — even if the DEFAULT track isn't. We own the demuxer, so
    // we direct-play the raw file and FEED a direct-playable track (e.g. Toy Story 3: TrueHD
    // default + an AC3 track → native 4K HEVC + AC3, no transcode — beats the server's
    // video-downscaling transcode). Falls back to the server /decision (then the local codec
    // test) when the video isn't direct-playable or NO audio track is (TrueHD/DTS-only → transcode).
    let video_dp = matches!(vcodec, "h264" | "hevc");
    // Our buffer-feed demuxer streams MKV/Matroska (sequential clusters). MP4/MOV need per-sample
    // random seeks our reopen-per-seek HTTP AVIO can't sustain — it dies after AU#0 (black screen).
    // So a direct-playable codec in a non-MKV container is NOT direct-played; it goes to Plex for a
    // container-only REMUX to progressive MKV (copy the codecs, no re-encode — keeps 4K/HDR).
    let streamable = part_is_mkv(part);
    // snapshot the track list on the MAIN thread and pass it by reference — the resolve worker
    // (step 7) gets an owned copy instead, and never touches the `&'static` store.
    let tracks = plan.playing.as_ref().map(|p| p.audio.as_slice()).unwrap_or(&[]);
    let audio_sel = if rk.is_empty() { None } else { pick_dp_audio(tracks, acodec) };
    let directplay = if !video_dp {
        // The buffer-feed pipeline only decodes what the Load payload declares — H264/H265.
        // Anything else (AV1/VP9/MPEG-2/…) MUST transcode: we can't feed it even if the server's
        // /decision says directplay (it adjudicates the panel's decoders, not our payload). This
        // gate is why the local sample path (rk empty) is the only other non-transcode case.
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
        // direct-play: no transcode session (transcode_session() stays empty). Carry the
        // session id + identity on the file GET so PMS keys the /status/sessions entry by
        // SESS (not a token= fallback), keeping the timeline correlation consistent.
        plan.url = client.direct_play_url(part, &session).to_url();
        return plan;
    }
    // Transcode OR container-remux, both served via start.mkv. If the SOURCE video is
    // direct-playable (h264/hevc) we only reached here because the container isn't streamable, so
    // ask Plex to REMUX — copy both codecs into MKV, no re-encode (keeps 4K + HDR10); the Load
    // payload then uses the SOURCE codecs. Otherwise it's a real RE-ENCODE to the hevc+ac3 target
    // (HEVC target keeps 4K + HDR10; see profile_extra + the server's "HEVC encoding = Always").
    if video_dp {
        let achosen = audio_sel.as_ref().map(|(_, c, _)| c.clone()).unwrap_or_else(|| acodec.to_string());
        plan.vcodec = vcodec.to_string();
        plan.acodec = achosen;
    } else {
        plan.vcodec = "hevc".into();
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
///   1. a direct-playable track in PREF_AUDIO_LANG (English), so English shows don't open in a
///      foreign default dub — the Load payload uses THAT track's codec so there is no mismatch;
///   2. the file's flagged default track, if its codec is direct-playable — by EXPLICIT index
///      (matching by codec alone fed the first same-codec stream, not the flagged default, when
///      another track of that codec preceded it);
///   3. any other direct-playable track (TrueHD/DTS-default item with an AC3 sibling — smart-DP).
/// None when NO audio track is direct-playable (→ transcode).
/// PURE: takes the playing item's audio tracks explicitly instead of reaching into
/// `metadata::playing()`. That matters twice over. (a) `playing()` hands out a `&'static
/// PlayingTracks` whose `Vec`s `ui/track_menu.rs` and `ui/info_panel.rs` hold slices into during
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
    // 1. preferred-language, direct-playable
    if let Some(i) = tracks.iter().position(|s| dp(&s.codec.to_lowercase()) && s.lang_code == PREF_AUDIO_LANG) {
        return Some(pick(i));
    }
    // 2. the file's flagged default track, if direct-playable (explicit index)
    if let Some(i) = tracks.iter().position(|s| s.default && dp(&s.codec.to_lowercase())) {
        return Some(pick(i));
    }
    if dp(default_acodec) && !tracks.iter().any(|s| s.default) {
        // Media[0].audioCodec is DP but no stream carries the default flag — codec-match
        return Some((-1, default_acodec.to_string(), 0));
    }
    // 3. any direct-playable track (smart direct-play over a non-DP default)
    tracks.iter().position(|s| dp(&s.codec.to_lowercase())).map(pick)
}

/// True when the part's container is MKV/Matroska — the only container our buffer-feed demuxer
/// streams reliably (sequential clusters). Non-MKV (mp4/mov/…) is sent to Plex for a container
/// remux instead of direct-play. Matches the container extension in the part-key filename.
fn part_is_mkv(part_key: &str) -> bool {
    let name = part_key.rsplit('/').next().unwrap_or(part_key);
    let name = name.split('?').next().unwrap_or(name);
    name.ends_with(".mkv")
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
    }
    crate::player::reset_audio_track();
    crate::player::reset_subtitle();
    // captured HERE, on the main thread, and moved into the worker — see ResolveEnv
    let env = ResolveEnv::snapshot(rk);
    let gen = PLAY_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    PLAY_BUSY.store(true, Ordering::SeqCst);
    let (rk, part, vc, ac) = (rk.to_string(), part.to_string(), vcodec.to_string(), acodec.to_string());
    let _ = std::thread::Builder::new().stack_size(256 * 1024).spawn(move || {
        // catch_unwind OUTSIDE the mailbox write, like load_season: a panicking resolve must still
        // land (as !ok) or PLAY_BUSY latches and the screen wedges on a spinner forever.
        let plan = std::panic::catch_unwind(|| build_stream(&rk, &part, &vc, &ac, &env))
            .unwrap_or_default();
        let mut slot = PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner());
        // MONOTONE: an older resolve landing late must never clobber a newer unconsumed one.
        if slot.as_ref().map(|(g, _, _)| *g < gen).unwrap_or(true) {
            *slot = Some((gen, plan, rk));
        }
    }).inspect_err(|e| {
        // Builder::spawn returns Result (thread::spawn PANICS on EAGAIN). Swallowing it would
        // latch PLAY_BUSY true forever behind a spinner that can never resolve.
        crate::player::log(&format!("resolve: spawn failed ({e}) — this play is dropped"));
        PLAY_BUSY.store(false, Ordering::SeqCst);
    });
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

/// Supersede an in-flight resolve (BACK during a load). The landing is dropped by generation.
pub(crate) fn cancel_play() {
    PLAY_GEN.fetch_add(1, Ordering::SeqCst);
    PLAY_BUSY.store(false, Ordering::SeqCst);
    *PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
    if !plan.vcodec.is_empty() {
        set_stream_codecs(&plan.vcodec, &plan.acodec); // the pair is only ever set together
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
        addr_of_mut!(CUR_REMUX).write(plan.remux);
        *addr_of_mut!(URL) = plan.url;
        *addr_of_mut!(TSESSION) = plan.tsession;
        *addr_of_mut!(CUR_RK) = rk.to_string();
    }
    // SHARED.desired_audio_idx is read by the DEMUX THREAD on every reopen — main thread only.
    if let Some(ord) = plan.feed_audio_ordinal {
        crate::player::set_audio_track(ord);
    }
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
    // the transcode output is HEVC + AC3 — record it so a pipeline RELOAD (audio switch)
    // builds the H265 Load payload matching the re-encoded stream.
    set_stream_codecs("hevc", "ac3");
    put_selection(cur_part_id(), cur_audio_sid(), cur_sub_sid()); // drives the encode + burn
    let qsess = sess();
    let sp = transcode_spec(&rk, &qsess, false, offset_secs.max(0), cur_audio_sid(), cur_sub_sid());
    if let Some(mc) = c.transcode_decision(&sp) {
        apply_decision_codecs(&mc); // reload builds a fresh Load payload — match the real output
    }
    let url = c.transcode_start_url(&sp).to_url();
    set_url(&url);
    crate::player::log(&format!(
        "retranscode rk={rk} audio={} sub={} offset={offset_secs} -> {url}",
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
        }
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
        // Toy Story 3: TrueHD default + an AC3 sibling — direct-play beats the server's
        // video-downscaling transcode.
        let tracks = [trk(1, "truehd", "eng", true), trk(2, "ac3", "eng", false)];
        assert_eq!(pick_dp_audio(&tracks, "truehd"), Some((1, "ac3".into(), 2)));
    }

    #[test]
    fn no_direct_playable_track_means_transcode() {
        let tracks = [trk(1, "truehd", "eng", true), trk(2, "dts", "eng", false)];
        assert!(pick_dp_audio(&tracks, "truehd").is_none());
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

    /// The direct-play gate: only an MKV part is fed to the demuxer untouched — everything else
    /// takes the remux branch, which is why the mis-targeted PUT hit every mp4 item.
    #[test]
    fn only_an_mkv_part_is_direct_playable() {
        assert!(part_is_mkv("/library/parts/1/2/movie.mkv"));
        assert!(part_is_mkv("/library/parts/1/2/movie.mkv?x=1"), "the query must not defeat it");
        assert!(!part_is_mkv("/library/parts/1/2/movie.mp4"));
        assert!(!part_is_mkv("/library/parts/1/2/movie.mov"));
        assert!(!part_is_mkv(""));
        assert!(!part_is_mkv("/library/parts/1/2/mkv.avi"), "the extension, not a substring");
    }
}
