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
fn cur_audio_sid() -> i64 {
    unsafe { addr_of!(CUR_AUDIO_SID).read() }
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
            "/video/:/transcode/universal/stop?session={sess}&X-Plex-Client-Identifier={sess}&X-Plex-Token={}",
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

/// The offset-free transcode params for `rk`, carrying the CURRENT audio + subtitle
/// selection (CUR_AUDIO_SID / CUR_SUB_SID). Shared by build_stream + retranscode, and
/// (via TBASE) by transcode_seek — so every transcode of the item stays on the chosen
/// tracks. The subtitle, when set, is burned in (Plex's default decision for our profile).
fn transcode_base(rk: &str, cfg: &Cfg) -> String {
    let profe = crate::pms::urlenc_str(
        "add-transcode-target(type=videoProfile&context=streaming&protocol=http\
         &container=matroska&videoCodec=h264&audioCodec=ac3)",
    );
    let session = format!("plexpoc-{rk}");
    let audio_p = match cur_audio_sid() {
        0 => String::new(),
        a => format!("&audioStreamID={a}"),
    };
    let sub_p = match cur_sub_sid() {
        0 => String::new(),
        s => format!("&subtitleStreamID={s}&subtitleSize=100&subtitles=burn"),
    };
    format!(
        "path=%2Flibrary%2Fmetadata%2F{rk}&mediaIndex=0&partIndex=0&protocol=http\
         &directPlay=0&directStream=1&videoResolution=1920x1080&maxVideoBitrate=20000\
         {audio_p}{sub_p}\
         &session={session}&X-Plex-Session-Identifier={session}&X-Plex-Client-Identifier={session}\
         &X-Plex-Product=plexpoc&X-Plex-Version=1&X-Plex-Platform=Generic\
         &X-Plex-Client-Profile-Extra={profe}&X-Plex-Token={}",
        cfg.token
    )
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
    let directplay = vcodec == "h264" && acodec == "ac3";
    if (directplay || rk.is_empty()) && !part.is_empty() {
        return (format!("http://{}:{}{}?X-Plex-Token={}", cfg.host, cfg.port, part, cfg.token), String::new());
    }
    let session = format!("plexpoc-{rk}");
    let base = transcode_base(rk, cfg);
    // keep the offset-free base so a later seek can restart at start.mkv?...&offset=T
    unsafe { *addr_of_mut!(TBASE) = base.clone() };
    // the universal transcoder needs /decision to REGISTER the session before start.mkv streams
    let dpath = format!("/video/:/transcode/universal/decision?{base}");
    let _ = crate::stream::http_get(&cfg.host, cfg.port, &dpath, None);
    let url = format!("http://{}:{}/video/:/transcode/universal/start.mkv?{base}", cfg.host, cfg.port);
    (url, session)
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
    let (url, session) = build_stream(&rk, &cfield(&m.part), &cfield(&m.vcodec), &cfield(&m.acodec));
    unsafe {
        *addr_of_mut!(URL) = url;
        *addr_of_mut!(TSESSION) = session;
        *addr_of_mut!(CUR_RK) = rk;
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
    retranscode(offset_secs)
}
