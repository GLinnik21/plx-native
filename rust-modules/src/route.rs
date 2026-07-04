//! Rust-first step 3: play_movie route selection (direct-play vs transcode),
//! moved from playback.c to Rust. Owns the stream URL, the transcode session, and
//! the HUD strings (g_*) as #[no_mangle] statics; the still-C start/stop_bufferfeed
//! + draw_hud read them via extern until those move to Rust too (steps 4-5).
#![allow(non_upper_case_globals)]
use crate::pms::PmsMovie;
use std::os::raw::{c_char, c_int};
use std::ptr::{addr_of, addr_of_mut};

// stream URL + transcode session + HUD strings, read by the C playback engine
#[no_mangle]
pub static mut g_url: [c_char; 1024] = [0; 1024];
#[no_mangle]
pub static mut g_transcode_session: [c_char; 64] = [0; 64];
#[no_mangle]
pub static mut g_title: [c_char; 128] = [0; 128];
#[no_mangle]
pub static mut g_ctxline: [c_char; 96] = [0; 96];

struct Cfg {
    host: String,
    port: c_int,
    token: String,
}
static mut CFG: Option<Cfg> = None;

/// Called once at startup with the PMS config (from the C boot shim via plex_run).
pub(crate) fn set_config(host: &str, port: c_int, token: &str) {
    unsafe {
        *addr_of_mut!(CFG) = Some(Cfg { host: host.to_owned(), port, token: token.to_owned() });
    }
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
