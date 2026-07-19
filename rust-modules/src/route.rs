//! play_movie route selection (direct-play vs transcode) + the stream URL, transcode
//! session, and HUD strings — all private module state. The player engine reads the
//! URL/session through the accessors here; ui::player_hud reads the HUD strings
//! through title_cptr()/ctxline_cptr().
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
fn transcode_spec<'a>(rk: &'a str, session: &'a str, remux: bool, offset_secs: i64) -> crate::plex::TranscodeSpec<'a> {
    crate::plex::TranscodeSpec {
        rating_key: rk,
        session,
        remux,
        audio_stream_id: cur_audio_sid(),
        subtitle_stream_id: cur_sub_sid(),
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
    let sp = transcode_spec(&rk, &session, unsafe { addr_of!(CUR_REMUX).read() }, offset_secs.max(0));
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
fn server_decision(rk: &str) -> Option<bool> {
    let mc = match crate::plex::client_opt()?.mde_decision(rk, &sess()) {
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
fn apply_decision_codecs(mc: &crate::plex::MediaContainer) {
    let streams = match mc.metadata.first().and_then(|m| m.media.first()).and_then(|md| md.part.first()) {
        Some(p) => &p.stream,
        None => return,
    };
    let (mut vc, mut ac) = (None, None);
    for s in streams {
        // first video/audio stream that CARRIES a codec (an empty codec keeps scanning)
        match s.stream_type {
            1 if vc.is_none() && !s.codec.is_empty() => vc = Some(s.codec.to_lowercase()),
            2 if ac.is_none() && !s.codec.is_empty() => ac = Some(s.codec.to_lowercase()),
            _ => {}
        }
    }
    unsafe {
        if let Some(vc) = vc {
            *addr_of_mut!(STREAM_VCODEC) = vc;
        }
        if let Some(ac) = ac {
            *addr_of_mut!(STREAM_ACODEC) = ac;
        }
    }
    crate::player::log(&format!("decision output: v={} a={}", stream_vcodec(), stream_acodec()));
}

/// Select the audio + subtitle streams server-side for the current part before a
/// transcode. The transcoder encodes the part's SELECTED audio and BURNS its SELECTED
/// subtitle (our client profile advertises no soft-sub support, so Plex's decision is
/// always burn) — a query-param subtitleStreamID does NOT suppress a default-selected
/// sub, only the PUT does. So we PUT subtitleStreamID=0 to keep subs OFF (no burn), or
/// the chosen id to burn it; audioStreamID only when the user switched (else keep default).
fn put_selection() {
    let part = unsafe { addr_of!(CUR_PART_ID).read() };
    if part <= 0 {
        return;
    }
    let c = match crate::plex::client_opt() {
        Some(c) => c,
        None => return,
    };
    let (aud, sub) = (cur_audio_sid(), cur_sub_sid());
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

/// Cache the server machineIdentifier (once) from GET /identity — needed for the PlayQueue uri.
fn ensure_machine_id() {
    if !unsafe { &*addr_of!(MACHINE_ID) }.is_empty() {
        return;
    }
    if let Some(mid) = crate::plex::client_opt().and_then(|c| c.machine_identity()) {
        unsafe { *addr_of_mut!(MACHINE_ID) = mid };
    }
}

/// Create a PlayQueue for `rk` so the session is a first-class, remote-controllable player and
/// the timeline can carry a real playQueueItemID. Best-effort: on failure the timeline still
/// works (just without the queue ids).
fn ensure_playqueue(rk: &str, session: &str) {
    unsafe {
        *addr_of_mut!(PQ_ID) = String::new();
        *addr_of_mut!(PQ_ITEM_ID) = String::new();
    }
    ensure_machine_id();
    let mid = unsafe { (*addr_of!(MACHINE_ID)).clone() };
    if mid.is_empty() {
        crate::player::log("playqueue: no machineIdentifier (skip)");
        return;
    }
    match crate::plex::client_opt().and_then(|c| c.create_play_queue(&mid, rk, session)) {
        Some((pq, it)) => {
            unsafe {
                *addr_of_mut!(PQ_ID) = if pq > 0 { pq.to_string() } else { String::new() };
                *addr_of_mut!(PQ_ITEM_ID) = if it > 0 { it.to_string() } else { String::new() };
            }
            crate::player::log(&format!("playqueue: id={pq} item={it}"));
        }
        None => crate::player::log("playqueue: POST failed"),
    }
}

/// Pick the stream URL for an item: direct-play only H264+AC3 (what the pipeline
/// decodes natively); else ask the server to transcode into progressive H264+AC3
/// Matroska (same MKV demuxer eats it). Returns (url, transcode session). On the
/// transcode path this also runs the /decision handshake and stores TBASE for seeks.
fn build_stream(rk: &str, part: &str, vcodec: &str, acodec: &str) -> (String, String) {
    let client = match crate::plex::client_opt() {
        Some(c) => c,
        None => return (String::new(), String::new()),
    };
    unsafe { *addr_of_mut!(STREAM_FPS) = 0.0 }; // set to the source fps only on the direct-play path below
    // fresh per-playback session id (BOTH direct-play and transcode report through it) +
    // a PlayQueue so the server tracks this as a real player with a playQueueItemID.
    let session = new_sess(rk);
    unsafe { *addr_of_mut!(SESS) = session.clone() };
    if !rk.is_empty() {
        ensure_playqueue(rk, &session);
    }
    // the playing item's OWN track lists (menu + audio pick + esInfo fps read them) — the
    // loaded detail can be a different item (show page / straight-from-Home play)
    crate::metadata::load_playing(rk);
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
    let audio_sel = if rk.is_empty() { None } else { pick_dp_audio(acodec) };
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
        server_decision(rk).unwrap_or_else(|| crate::plex::is_dp_audio(acodec))
    };
    if (directplay || rk.is_empty()) && !part.is_empty() {
        // direct-play: the pipeline decodes the SOURCE codecs natively, so the Load payload uses
        // them (h264/hevc + the chosen audio track's codec). If a specific track was picked
        // (aidx >= 0), tell the demuxer to feed that stream — by CONTAINER ordinal, not the
        // list position (audio_ordinal sorts on PMS Stream.index).
        let (aidx, achosen, asid) = audio_sel.unwrap_or((-1, acodec.to_string(), 0));
        // source fps for the Load esInfo — from the playing item's own store (present for the
        // straight-from-Home path too, which never ran load_detail)
        let fps = crate::metadata::playing().map(|p| p.video_fps).unwrap_or(0.0);
        set_stream_codecs(vcodec, &achosen);
        unsafe { *addr_of_mut!(STREAM_FPS) = fps };
        // record the picked track's stream id so the timeline reports what actually plays
        // (0 = default/unknown → the param is omitted, the server shows the part default)
        unsafe { addr_of_mut!(CUR_AUDIO_SID).write(asid) };
        if aidx >= 0 {
            let ord = crate::metadata::playing()
                .map(|p| crate::metadata::audio_ordinal(&p.audio, aidx as usize))
                .unwrap_or(aidx);
            crate::player::set_audio_track(ord); // feed the direct-playable non-default track
        }
        // direct-play: no transcode session (transcode_session() stays empty). Carry the
        // session id + identity on the file GET so PMS keys the /status/sessions entry by
        // SESS (not a token= fallback), keeping the timeline correlation consistent.
        return (client.direct_play_url(part, &session).to_url(), String::new());
    }
    // Transcode OR container-remux, both served via start.mkv. If the SOURCE video is
    // direct-playable (h264/hevc) we only reached here because the container isn't streamable, so
    // ask Plex to REMUX — copy both codecs into MKV, no re-encode (keeps 4K + HDR10); the Load
    // payload then uses the SOURCE codecs. Otherwise it's a real RE-ENCODE to the hevc+ac3 target
    // (HEVC target keeps 4K + HDR10; see profile_extra + the server's "HEVC encoding = Always").
    if video_dp {
        let achosen = audio_sel.as_ref().map(|(_, c, _)| c.clone()).unwrap_or_else(|| acodec.to_string());
        set_stream_codecs(vcodec, &achosen);
    } else {
        set_stream_codecs("hevc", "ac3");
    }
    // Carry the picked SOURCE track into the server-side selection (put_selection +
    // &audioStreamID on the transcode query): the remux copies — and the re-encode encodes —
    // the CHOSEN track instead of the part default. The demuxer is NOT pointed at a source
    // ordinal here (the old set_audio_track(aidx) indexed the SERVER's output, whose stream
    // layout is the transcoder's, not the source's) — the payload-codec match finds the lane.
    if let Some((_, _, asid)) = &audio_sel {
        unsafe { addr_of_mut!(CUR_AUDIO_SID).write(*asid) };
    }
    // keep the flavor so a later seek rebuilds the same query for start.mkv?...&offset=T
    unsafe { addr_of_mut!(CUR_REMUX).write(video_dp) };
    put_selection(); // audio/subtitle selection drives the encode/remux + burn
    let sp = transcode_spec(rk, &session, video_dp, -1);
    if let Some(mc) = client.transcode_decision(&sp) {
        apply_decision_codecs(&mc); // the Load payload must match the server's ACTUAL output
    }
    (client.transcode_start_url(&sp).to_url(), session)
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
fn pick_dp_audio(default_acodec: &str) -> Option<(i32, String, i64)> {
    let dp = crate::plex::is_dp_audio;
    let tracks = crate::metadata::playing().map(|p| p.audio.as_slice()).unwrap_or(&[]);
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

/// Set the stream URL + HUD strings from a selected movie (direct-play or transcode).
pub(crate) fn play_movie(m: &PmsMovie) {
    if m.part.is_empty() {
        return;
    }
    // HUD title + context line ("YEAR · RATING · Hh Mm")
    let rating = if m.rating.is_empty() { "NR" } else { &m.rating };
    let ctx = format!("{} \u{b7} {} \u{b7} {}", m.year, rating, crate::ui::fmt::dur_short(m.dur_ns / 1_000_000));
    unsafe {
        set_c(addr_of_mut!(TITLE) as *mut c_char, 128, &m.title);
        set_c(addr_of_mut!(CTXLINE) as *mut c_char, 96, &ctx);
    }
    // fresh item: default audio + no burned subtitle until the user picks one
    unsafe {
        addr_of_mut!(CUR_AUDIO_SID).write(0);
        addr_of_mut!(CUR_SUB_SID).write(0);
    }
    crate::player::reset_audio_track(); // default (best) audio stream until the user picks one
    crate::player::reset_subtitle(); // subs Off on a new item (selection persists across seeks/reloads)
    let (url, session) = build_stream(&m.rk, &m.part, &m.vcodec, &m.acodec);
    unsafe {
        *addr_of_mut!(URL) = url;
        *addr_of_mut!(TSESSION) = session;
        *addr_of_mut!(CUR_RK) = m.rk.clone();
        addr_of_mut!(CUR_PART_ID).write(part_id_of(&m.part));
    }
}

/// Set the stream URL + HUD strings for a TV episode (from the detail page).
pub(crate) fn play_episode(rk: &str, part: &str, vcodec: &str, acodec: &str, hud_title: &str, hud_ctx: &str) {
    if part.is_empty() && rk.is_empty() {
        return;
    }
    unsafe {
        set_c(addr_of_mut!(TITLE) as *mut c_char, 128, hud_title);
        set_c(addr_of_mut!(CTXLINE) as *mut c_char, 96, hud_ctx);
    }
    // fresh item: default audio + no burned subtitle until the user picks one
    unsafe {
        addr_of_mut!(CUR_AUDIO_SID).write(0);
        addr_of_mut!(CUR_SUB_SID).write(0);
    }
    crate::player::reset_audio_track(); // default (best) audio stream until the user picks one
    crate::player::reset_subtitle(); // subs Off on a new item (selection persists across seeks/reloads)
    let (url, session) = build_stream(rk, part, vcodec, acodec);
    unsafe {
        *addr_of_mut!(URL) = url;
        *addr_of_mut!(TSESSION) = session;
        *addr_of_mut!(CUR_RK) = rk.to_string();
        addr_of_mut!(CUR_PART_ID).write(part_id_of(part));
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
    put_selection(); // audio/subtitle selection drives the encode + burn
    let qsess = sess();
    let sp = transcode_spec(&rk, &qsess, false, offset_secs.max(0));
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
        put_selection();
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
        put_selection();
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
