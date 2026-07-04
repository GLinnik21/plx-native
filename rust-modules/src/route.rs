//! Rust-first step 3: play_movie route selection (direct-play vs transcode),
//! moved from playback.c to Rust. Owns the stream URL, the transcode session, and
//! the HUD strings (g_*) as #[no_mangle] statics; the still-C start/stop_bufferfeed
//! + draw_hud read them via extern until those move to Rust too (steps 4-5).
#![allow(non_upper_case_globals)]
use crate::pms::PmsMovie;
use std::os::raw::{c_char, c_int};
use std::ptr::{addr_of, addr_of_mut};

// stream URL + transcode session — private backing storage; the Rust player engine
// reads them via the accessors below (nothing C references them since step 5).
static mut g_url: [c_char; 1024] = [0; 1024];
static mut g_transcode_session: [c_char; 64] = [0; 64];
// offset-free transcode query params, kept so a seek can rebuild start.mkv?...&offset=T
static mut g_tbase: [c_char; 1024] = [0; 1024];
// HUD strings — crate-visible so ui::player_hud can read them.
pub(crate) static mut g_title: [c_char; 128] = [0; 128];
pub(crate) static mut g_ctxline: [c_char; 96] = [0; 96];

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

/// read a C-string field (the [c_char;N] backing buffers) as an owned String.
unsafe fn get_c(src: *const c_char, cap: usize) -> String {
    cfield(std::slice::from_raw_parts(src as *const u8, cap))
}

// ---- engine-facing accessors (the player module reads/owns the stream URL) ----
pub(crate) fn url() -> String {
    unsafe { get_c(addr_of!(g_url) as *const c_char, 1024) }
}
pub(crate) fn set_url(s: &str) {
    unsafe { set_c(addr_of_mut!(g_url) as *mut c_char, 1024, s) }
}
pub(crate) fn clear_url() {
    unsafe { (*addr_of_mut!(g_url))[0] = 0 }
}
pub(crate) fn transcode_session() -> String {
    unsafe { get_c(addr_of!(g_transcode_session) as *const c_char, 64) }
}
pub(crate) fn demo_url() -> String {
    unsafe { (*addr_of!(CFG)).as_ref().map(|c| c.demo_url.clone()).unwrap_or_default() }
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
        (*addr_of_mut!(g_transcode_session))[0] = 0;
        (*addr_of_mut!(g_tbase))[0] = 0;
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
    let base = unsafe { get_c(addr_of!(g_tbase) as *const c_char, 1024) };
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

/// Set the stream URL + HUD strings from a selected movie (direct-play or transcode).
pub(crate) fn play_movie(m: *mut PmsMovie) {
    let m = match unsafe { m.as_ref() } {
        Some(m) => m,
        None => return,
    };
    if m.part[0] == 0 {
        return;
    }
    let cfg = match unsafe { (*addr_of!(CFG)).as_ref() } {
        Some(c) => c,
        None => return,
    };

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
        set_c(addr_of_mut!(g_title) as *mut c_char, 128, &title);
        set_c(addr_of_mut!(g_ctxline) as *mut c_char, 96, &ctx);
    }

    // direct-play only H264+AC3 (what the pipeline decodes natively); else ask the
    // server to transcode into progressive H264+AC3 Matroska (same MKV demuxer eats it).
    let vcodec = cfield(&m.vcodec);
    let acodec = cfield(&m.acodec);
    let rk = cfield(&m.rk);
    let part = cfield(&m.part);
    let directplay = vcodec == "h264" && acodec == "ac3";
    let (url, session) = if directplay || rk.is_empty() {
        (format!("http://{}:{}{}?X-Plex-Token={}", cfg.host, cfg.port, part, cfg.token), String::new())
    } else {
        let profe = crate::pms::urlenc_str(
            "add-transcode-target(type=videoProfile&context=streaming&protocol=http\
             &container=matroska&videoCodec=h264&audioCodec=ac3)",
        );
        let session = format!("plexpoc-{rk}");
        // params shared by the /decision handshake and the /start.mkv stream
        let base = format!(
            "path=%2Flibrary%2Fmetadata%2F{rk}&mediaIndex=0&partIndex=0&protocol=http\
             &directPlay=0&directStream=1&videoResolution=1920x1080&maxVideoBitrate=20000\
             &session={session}&X-Plex-Session-Identifier={session}&X-Plex-Client-Identifier={session}\
             &X-Plex-Product=plexpoc&X-Plex-Version=1&X-Plex-Platform=Generic\
             &X-Plex-Client-Profile-Extra={profe}&X-Plex-Token={}",
            cfg.token
        );
        // keep the offset-free base so a later seek can restart at start.mkv?...&offset=T
        unsafe { set_c(addr_of_mut!(g_tbase) as *mut c_char, 1024, &base) };
        // the universal transcoder needs /decision to REGISTER the session before start.mkv streams
        let dpath = format!("/video/:/transcode/universal/decision?{base}");
        let _ = crate::stream::http_get(&cfg.host, cfg.port, &dpath, None);
        let url = format!("http://{}:{}/video/:/transcode/universal/start.mkv?{base}", cfg.host, cfg.port);
        (url, session)
    };
    unsafe {
        set_c(addr_of_mut!(g_url) as *mut c_char, 1024, &url);
        set_c(addr_of_mut!(g_transcode_session) as *mut c_char, 64, &session);
    }
}
