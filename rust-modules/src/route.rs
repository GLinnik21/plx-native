//! play_movie route selection (direct-play vs transcode) + the stream URL, transcode
//! session, and HUD strings — all private module state. The player engine reads the
//! URL/session through the accessors here; ui::player_hud reads the HUD strings
//! through title_cptr()/ctxline_cptr().
use crate::pms::PmsMovie;
use std::os::raw::{c_char, c_int};
use std::ptr::{addr_of, addr_of_mut};

// stream URL + transcode session + offset-free transcode base (for &offset= seeks).
static mut URL: String = String::new();
static mut TSESSION: String = String::new();
static mut TBASE: String = String::new();
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
// A STABLE device id for X-Plex-Client-Identifier (NEVER varies per item) — one binary is
// one device to the server. Fixes the old bug of sending the transcode session string here.
const DEVICE_ID: &str = "9b7d2f1a-4c63-4e18-a5d0-7f3b8c2e6a94";
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
// When true, a selected subtitle is BURNED into the transcode (server-side) — the
// pre-WebVTT behavior, kept as an escape hatch. When false (default), a selected
// subtitle rides a soft WebVTT sidecar (player::request_soft_subs + transcode_subtitles_url)
// and the video transcode carries no subtitle at all.
pub(crate) const BURN_FALLBACK: bool = true;
// HUD strings as fixed NUL-terminated C buffers, so title_cptr()/ctxline_cptr() hand
// draw_text (extern "C", *const c_char) a pointer that stays valid for the whole frame.
static mut TITLE: [c_char; 128] = [0; 128];
static mut CTXLINE: [c_char; 96] = [0; 96];

struct Cfg {
    host: String,
    port: c_int,
    token: String,
    demo_url: String,
}
static mut CFG: Option<Cfg> = None;

/// Called once at startup with the PMS config (from the C boot shim via plex_run).
pub(crate) fn set_config(host: &str, port: c_int, token: &str, demo_url: &str) {
    unsafe {
        *addr_of_mut!(CFG) =
            Some(Cfg { host: host.to_owned(), port, token: token.to_owned(), demo_url: demo_url.to_owned() });
    }
}

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
/// The §3b identity query params (stable device id + product/platform/model/…), appended to
/// every playback request so the server names + groups this client and shows a proper Player.
pub(crate) fn identity_qs() -> String {
    format!(
        "&X-Plex-Client-Identifier={DEVICE_ID}&X-Plex-Product=Plex%20POC&X-Plex-Version=0.1.0\
         &X-Plex-Platform=webOS&X-Plex-Platform-Version=4.5&X-Plex-Device=webOS\
         &X-Plex-Device-Name=Living%20Room%20TV&X-Plex-Model=49SM9000PLA&X-Plex-Provides=player"
    )
}
pub(crate) fn demo_url() -> String {
    unsafe { (*addr_of!(CFG)).as_ref().map(|c| c.demo_url.clone()).unwrap_or_default() }
}
/// PMS (host, port, token) — used by the metadata layer for detail/children/related fetches.
pub(crate) fn config() -> Option<(String, c_int, String)> {
    unsafe { (*addr_of!(CFG)).as_ref().map(|c| (c.host.clone(), c.port, c.token.clone())) }
}
/// pointers into the module-owned HUD buffers (valid for the whole frame draw_text uses them)
pub(crate) fn title_cptr() -> *const c_char {
    addr_of!(TITLE) as *const c_char
}
pub(crate) fn ctxline_cptr() -> *const c_char {
    addr_of!(CTXLINE) as *const c_char
}
/// free the server-side transcode encoder if this playback was a transcode.
pub(crate) fn stop_transcode() {
    let sess = transcode_session();
    if sess.is_empty() {
        return;
    }
    if let Some(cfg) = unsafe { (*addr_of!(CFG)).as_ref() } {
        let sp = format!(
            "/video/:/transcode/universal/stop?session={sess}&X-Plex-Client-Identifier={DEVICE_ID}&X-Plex-Token={}",
            cfg.token
        );
        let _ = crate::stream::http_get(&cfg.host, cfg.port, &sp, None);
    }
    unsafe {
        (*addr_of_mut!(TSESSION)).clear();
        (*addr_of_mut!(TBASE)).clear();
    }
}

/// Seek within a LIVE TRANSCODE by restarting it at a time offset — a transcode has
/// no byte-Cues, so a byte-Range seek can't work (docs/plex-api.md). Stops the current
/// encoder, then re-registers (/decision) and re-points the stream at
/// start.mkv?...&offset={secs}. Returns the new URL (the demux re-opens it from byte 0),
/// or None if this playback isn't a transcode. Blocks on two HTTP round-trips (like
/// play_movie's /decision), which is fine during a seek (the pipeline is flushed).
pub(crate) fn transcode_seek(offset_secs: i64) -> Option<String> {
    let sess = transcode_session();
    if sess.is_empty() {
        return None;
    }
    let base = unsafe { (*addr_of!(TBASE)).clone() };
    if base.is_empty() {
        return None;
    }
    let cfg = unsafe { (*addr_of!(CFG)).as_ref()? };
    // NB: do NOT explicitly /stop the old encoder here — the session id is reused, so a
    // stop would race the demux (it cuts the stream the demux is still reading; if the
    // demux hits EOF + checks seek_byte before the pump sets it, it exits). Instead the
    // pump closes the demux socket AFTER arming the seek, which drops the old connection
    // (stopping the old transcode), and this new start.mkv?&offset= (same session)
    // repositions. /decision is just a query and doesn't cut the streaming connection.
    let obase = format!("{base}&offset={}", offset_secs.max(0));
    let dpath = format!("/video/:/transcode/universal/decision?{obase}");
    let _ = crate::stream::http_get(&cfg.host, cfg.port, &dpath, None);
    let url = format!("http://{}:{}/video/:/transcode/universal/start.mkv?{obase}", cfg.host, cfg.port);
    set_url(&url);
    Some(url)
}

fn cfield(b: &[u8]) -> String {
    let n = b.iter().position(|&x| x == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..n]).into_owned()
}

unsafe fn set_c(dst: *mut c_char, cap: usize, s: &str) {
    let out = std::slice::from_raw_parts_mut(dst as *mut u8, cap);
    let b = s.as_bytes();
    let n = b.len().min(cap - 1);
    out[..n].copy_from_slice(&b[..n]);
    out[n] = 0;
}

/// Capability profile (X-Plex-Client-Profile-Extra, URL-decoded form): direct-play an MKV
/// whose video is H264 or HEVC and audio AAC/AC3/EAC3, subs SRT/ASS, up to 4K — plus the
/// H264/AC3 transcode fallback target. HEVC direct-plays natively now (Phase 3 demuxer + the
/// panel decodes it, incl. 4K HDR10 auto-detected from the bitstream).
fn profile_extra() -> String {
    crate::pms::urlenc_str(
        "add-direct-play-profile(type=videoProfile&container=mkv&videoCodec=h264,hevc\
         &audioCodec=aac,ac3,eac3&subtitleCodec=srt,subrip,ass,ssa)\
         +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.width&value=3840&replace=true)\
         +add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.height&value=2176&replace=true)\
         +add-transcode-target(type=videoProfile&context=streaming&protocol=http\
         &container=matroska&videoCodec=h264&audioCodec=ac3)",
    )
}

/// The /decision request params for `rk`: asks the Media Decision Engine (hasMDE=1) whether
/// the item direct-plays given our profile. directPlay=1 = "I can direct-play if it matches".
fn decision_base(rk: &str, cfg: &Cfg) -> String {
    format!(
        "path=%2Flibrary%2Fmetadata%2F{rk}&mediaIndex=0&partIndex=0&protocol=http&hasMDE=1\
         &directPlay=1&directStream=1&directStreamAudio=1&mediaBufferSize=20971\
         &session={session}&X-Plex-Session-Identifier={session}{id}\
         &X-Plex-Client-Profile-Name=Generic&X-Plex-Client-Profile-Extra={profe}&X-Plex-Token={tok}",
        session = sess(),
        id = identity_qs(),
        profe = profile_extra(),
        tok = cfg.token
    )
}

/// Ask PMS whether `rk` should direct-play (Some(true) → serve the raw Part) or transcode
/// (Some(false) → start.mkv). None when the server returns no usable Media decision, so the
/// caller falls back to the local codec test. Registers the session as a side effect.
fn server_decision(rk: &str, cfg: &Cfg) -> Option<bool> {
    let dpath = format!("/video/:/transcode/universal/decision?{}", decision_base(rk, cfg));
    let body = crate::stream::http_get(&cfg.host, cfg.port, &dpath, Some("Accept: application/json\r\n"))?;
    let s = String::from_utf8_lossy(&body);
    if !s.contains("\"Part\"") {
        crate::player::log(&format!(
            "decision: no media (general={:?}) -> local heuristic",
            find_num(&s, "generalDecisionCode")
        ));
        return None;
    }
    // Part.decision is the first "decision" at/after the "Part" array (Media/container carry none)
    let after_part = s.find("\"Part\"").map(|i| &s[i..]).unwrap_or(&s);
    let part_dec = find_str(after_part, "decision").unwrap_or_default();
    let direct = part_dec == "directplay";
    crate::player::log(&format!(
        "decision: part={part_dec} general={:?} mde={:?} -> {}",
        find_num(&s, "generalDecisionCode"),
        find_num(&s, "mdeDecisionCode"),
        if direct { "DIRECT PLAY" } else { "TRANSCODE" }
    ));
    Some(direct)
}

/// The offset-free transcode params for `rk`, carrying the CURRENT audio + subtitle
/// selection (CUR_AUDIO_SID / CUR_SUB_SID). Shared by build_stream + retranscode, and
/// (via TBASE) by transcode_seek — so every transcode of the item stays on the chosen
/// tracks. The subtitle, when set, is burned in (Plex's default decision for our profile).
fn transcode_base(rk: &str, cfg: &Cfg) -> String {
    let profe = profile_extra();
    let session = sess(); // per-playback id (set by build_stream); shared with the timeline
    let audio_p = match cur_audio_sid() {
        0 => String::new(),
        a => format!("&audioStreamID={a}"),
    };
    // subtitles ride the soft WebVTT sidecar by default — the video transcode carries
    // none (never baked). Only BURN_FALLBACK re-adds the burn block.
    let sub_p = if BURN_FALLBACK {
        match cur_sub_sid() {
            0 => String::new(),
            s => format!("&subtitleStreamID={s}&subtitleSize=100&subtitles=burn"),
        }
    } else {
        String::new()
    };
    format!(
        "path=%2Flibrary%2Fmetadata%2F{rk}&mediaIndex=0&partIndex=0&protocol=http\
         &directPlay=0&directStream=1&videoResolution=1920x1080&maxVideoBitrate=20000\
         {audio_p}{sub_p}\
         &session={session}&X-Plex-Session-Identifier={session}{id}\
         &X-Plex-Client-Profile-Name=Generic&X-Plex-Client-Profile-Extra={profe}&X-Plex-Token={tok}",
        id = identity_qs(),
        tok = cfg.token
    )
}

/// Select the audio + subtitle streams server-side for the current part before a
/// transcode. The transcoder encodes the part's SELECTED audio and BURNS its SELECTED
/// subtitle (our client profile advertises no soft-sub support, so Plex's decision is
/// always burn) — a query-param subtitleStreamID does NOT suppress a default-selected
/// sub, only the PUT does. So we PUT subtitleStreamID=0 to keep subs OFF (no burn), or
/// the chosen id to burn it; audioStreamID only when the user switched (else keep default).
fn put_selection(cfg: &Cfg) {
    let part = unsafe { addr_of!(CUR_PART_ID).read() };
    if part <= 0 {
        return;
    }
    // subtitleStreamID=0 keeps the video burn-free (the soft WebVTT sidecar carries the
    // chosen sub instead); only BURN_FALLBACK selects a sub for the server to burn.
    let (aud, sub) = (cur_audio_sid(), if BURN_FALLBACK { cur_sub_sid() } else { 0 });
    let mut p = format!("/library/parts/{part}?allParts=1&subtitleStreamID={sub}");
    if aud > 0 {
        p.push_str(&format!("&audioStreamID={aud}"));
    }
    p.push_str(&format!("&X-Plex-Token={}", cfg.token));
    let st = crate::stream::http_put(&cfg.host, cfg.port, &p);
    crate::player::log(&format!("select streams: part={part} audio={aud} sub={sub} -> HTTP {st}"));
}

/// Build the soft-WebVTT sidecar URL for `sub_sid` at `offset_secs` on the CURRENT
/// transcode session. Same universal params as start.mkv (TBASE, burn-free) plus
/// &subtitleStreamID=…&subtitles=auto (NOT burn) + &offset — Plex returns text/vtt
/// streamed in lock-step with the video. None if not transcoding / no sub selected.
pub(crate) fn transcode_subtitles_url(sub_sid: i64, offset_secs: i64) -> Option<String> {
    if transcode_session().is_empty() || sub_sid <= 0 {
        return None;
    }
    let base = unsafe { (*addr_of!(TBASE)).clone() };
    if base.is_empty() {
        return None;
    }
    let cfg = unsafe { (*addr_of!(CFG)).as_ref()? };
    let q = format!("{base}&subtitleStreamID={sub_sid}&subtitles=auto&offset={}", offset_secs.max(0));
    Some(format!("http://{}:{}/video/:/transcode/universal/subtitles?{q}", cfg.host, cfg.port))
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
    format!("plexpoc-{rk}-{}", CTR.fetch_add(1, Ordering::Relaxed))
}

/// Find `"key":<number>` in a JSON body (attributes come back as ints with Accept: json).
fn find_num(body: &str, key: &str) -> Option<i64> {
    let pat = format!("\"{key}\":");
    let i = body.find(&pat)? + pat.len();
    let rest = &body[i..];
    let end = rest.find(|c: char| !c.is_ascii_digit() && c != '-').unwrap_or(rest.len());
    rest[..end].trim().parse::<i64>().ok()
}
/// Find `"key":"<value>"` in a JSON body.
fn find_str(body: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let i = body.find(&pat)? + pat.len();
    let rest = &body[i..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Cache the server machineIdentifier (once) from GET /identity — needed for the PlayQueue uri.
fn ensure_machine_id(cfg: &Cfg) {
    if !unsafe { &*addr_of!(MACHINE_ID) }.is_empty() {
        return;
    }
    let p = format!("/identity?X-Plex-Token={}", cfg.token);
    if let Some(body) = crate::stream::http_get(&cfg.host, cfg.port, &p, Some("Accept: application/json\r\n")) {
        if let Some(mid) = find_str(&String::from_utf8_lossy(&body), "machineIdentifier") {
            unsafe { *addr_of_mut!(MACHINE_ID) = mid };
        }
    }
}

/// Create a PlayQueue for `rk` so the session is a first-class, remote-controllable player and
/// the timeline can carry a real playQueueItemID. Best-effort: on failure the timeline still
/// works (just without the queue ids).
fn ensure_playqueue(cfg: &Cfg, rk: &str, session: &str) {
    unsafe {
        *addr_of_mut!(PQ_ID) = String::new();
        *addr_of_mut!(PQ_ITEM_ID) = String::new();
    }
    ensure_machine_id(cfg);
    let mid = unsafe { (*addr_of!(MACHINE_ID)).clone() };
    if mid.is_empty() {
        crate::player::log("playqueue: no machineIdentifier (skip)");
        return;
    }
    let uri = crate::pms::urlenc_str(&format!(
        "server://{mid}/com.plexapp.plugins.library/library/metadata/{rk}"
    ));
    let p = format!(
        "/playQueues?type=video&uri={uri}&continuous=1&shuffle=0&repeat=0\
         &X-Plex-Session-Identifier={session}{id}&X-Plex-Token={tok}",
        id = identity_qs(),
        tok = cfg.token
    );
    match crate::stream::http_post(&cfg.host, cfg.port, &p, Some("Accept: application/json\r\n")) {
        Some(body) => {
            let s = String::from_utf8_lossy(&body);
            let pq = find_num(&s, "playQueueID").unwrap_or(0);
            let it = find_num(&s, "playQueueSelectedItemID").unwrap_or(0);
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
    let cfg = match unsafe { (*addr_of!(CFG)).as_ref() } {
        Some(c) => c,
        None => return (String::new(), String::new()),
    };
    // fresh per-playback session id (BOTH direct-play and transcode report through it) +
    // a PlayQueue so the server tracks this as a real player with a playQueueItemID.
    let session = new_sess(rk);
    unsafe { *addr_of_mut!(SESS) = session.clone() };
    if !rk.is_empty() {
        ensure_playqueue(cfg, rk, &session);
    }
    // Server-adjudicated: the Media Decision Engine decides direct-play vs transcode from our
    // capability profile. Falls back to the local codec test if the server returns no usable
    // decision; the local-sample/demo path (rk empty) skips the decision entirely.
    // Server-adjudicated (Phase 2). HEVC now direct-plays (Phase 3 demuxer + native decode);
    // the guard that forced non-h264 to transcode is gone.
    let directplay = if rk.is_empty() {
        false
    } else {
        server_decision(rk, cfg).unwrap_or(vcodec == "h264" && acodec == "ac3")
    };
    if (directplay || rk.is_empty()) && !part.is_empty() {
        // direct-play: the pipeline decodes the SOURCE codecs natively, so the Load payload
        // uses them (h264/hevc + the source audio).
        unsafe {
            *addr_of_mut!(STREAM_VCODEC) = vcodec.to_string();
            *addr_of_mut!(STREAM_ACODEC) = acodec.to_string();
        }
        // direct-play: no transcode session (transcode_session() stays empty). Carry the
        // session id + identity on the file GET so PMS keys the /status/sessions entry by
        // SESS (not a token= fallback), keeping the timeline correlation consistent.
        return (
            format!(
                "http://{}:{}{}?X-Plex-Token={}&X-Plex-Session-Identifier={}{}",
                cfg.host, cfg.port, part, cfg.token, session, identity_qs()
            ),
            String::new(),
        );
    }
    // transcode: PMS re-encodes to H264/AC3 in MKV regardless of the source codec, so the
    // Load payload must be H264/AC3 (NOT the source hevc/eac3).
    unsafe {
        *addr_of_mut!(STREAM_VCODEC) = "h264".to_string();
        *addr_of_mut!(STREAM_ACODEC) = "ac3".to_string();
    }
    let base = transcode_base(rk, cfg);
    // keep the offset-free base so a later seek can restart at start.mkv?...&offset=T
    unsafe { *addr_of_mut!(TBASE) = base.clone() };
    put_selection(cfg); // audio/subtitle selection drives the encode + burn
    // the universal transcoder needs /decision to REGISTER the session before start.mkv streams
    let dpath = format!("/video/:/transcode/universal/decision?{base}");
    let _ = crate::stream::http_get(&cfg.host, cfg.port, &dpath, None);
    let url = format!("http://{}:{}/video/:/transcode/universal/start.mkv?{base}", cfg.host, cfg.port);
    (url, session)
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
pub(crate) fn play_movie(m: *mut PmsMovie) {
    let m = match unsafe { m.as_ref() } {
        Some(m) => m,
        None => return,
    };
    if m.part[0] == 0 {
        return;
    }
    // HUD title + context line ("YEAR · RATING · Hh Mm")
    let title = cfield(&m.title);
    let mut rating = cfield(&m.rating);
    if rating.is_empty() {
        rating = "NR".into();
    }
    let mins = m.dur_ns / 60_000_000_000;
    let (hh, mm) = ((mins / 60) as i32, (mins % 60) as i32);
    let ctx = if hh > 0 {
        format!("{} \u{b7} {} \u{b7} {}h {}m", m.year, rating, hh, mm)
    } else {
        format!("{} \u{b7} {} \u{b7} {}m", m.year, rating, mm)
    };
    unsafe {
        set_c(addr_of_mut!(TITLE) as *mut c_char, 128, &title);
        set_c(addr_of_mut!(CTXLINE) as *mut c_char, 96, &ctx);
    }
    let rk = cfield(&m.rk);
    // fresh item: default audio + no burned subtitle until the user picks one
    unsafe {
        addr_of_mut!(CUR_AUDIO_SID).write(0);
        addr_of_mut!(CUR_SUB_SID).write(0);
    }
    let part = cfield(&m.part);
    let (url, session) = build_stream(&rk, &part, &cfield(&m.vcodec), &cfield(&m.acodec));
    unsafe {
        *addr_of_mut!(URL) = url;
        *addr_of_mut!(TSESSION) = session;
        *addr_of_mut!(CUR_RK) = rk;
        addr_of_mut!(CUR_PART_ID).write(part_id_of(&part));
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
    let cfg = unsafe { (*addr_of!(CFG)).as_ref()? };
    let rk = unsafe { (*addr_of!(CUR_RK)).clone() };
    if rk.is_empty() {
        return None;
    }
    let session = format!("plexpoc-{rk}");
    let base = transcode_base(&rk, cfg);
    unsafe {
        *addr_of_mut!(TBASE) = base.clone();
        *addr_of_mut!(TSESSION) = session;
    }
    put_selection(cfg); // audio/subtitle selection drives the encode + burn
    let obase = format!("{base}&offset={}", offset_secs.max(0));
    let dpath = format!("/video/:/transcode/universal/decision?{obase}");
    let _ = crate::stream::http_get(&cfg.host, cfg.port, &dpath, None);
    let url = format!("http://{}:{}/video/:/transcode/universal/start.mkv?{obase}", cfg.host, cfg.port);
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
