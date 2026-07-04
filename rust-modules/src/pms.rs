//! Rust port of src/pms.c — Plex library fetch/parse into the shared pms_movies[]
//! array, plus urlenc (shared by posters/playback). Same C ABI (pms.h): ui_home.c
//! reads pms_movies[]/pms_nmovies (defined here), the C callers call urlenc. The
//! hand-rolled JSON string-scrape is replaced with serde_json navigation.
use serde_json::Value;
use std::os::raw::{c_char, c_int};
use std::panic::catch_unwind;

const PMS_MAX_MOVIES: usize = 256;

// Layout MUST match `pms_movie` in src/pms.h. Fields are pub(crate) so ui_home
// (the Rust home screen) can read them; they carry NUL-terminated C strings.
#[repr(C)]
pub struct PmsMovie {
    pub(crate) title: [u8; 128],
    pub(crate) year: c_int,
    pub(crate) rating: [u8; 12],
    pub(crate) dur_ns: i64,
    pub(crate) part: [u8; 256],
    pub(crate) thumb: [u8; 128],
    pub(crate) art: [u8; 128],
    pub(crate) summary: [u8; 600],
    pub(crate) rk: [u8; 16],
    pub(crate) vcodec: [u8; 12],
    pub(crate) acodec: [u8; 12],
    pub(crate) blur: [[f32; 3]; 4],
    pub(crate) has_blur: c_int,
}
impl PmsMovie {
    const ZERO: PmsMovie = PmsMovie {
        title: [0; 128], year: 0, rating: [0; 12], dur_ns: 0, part: [0; 256],
        thumb: [0; 128], art: [0; 128], summary: [0; 600], rk: [0; 16],
        vcodec: [0; 12], acodec: [0; 12], blur: [[0.0; 3]; 4], has_blur: 0,
    };
}

// The catalog, shared with the C UI (ui_home.c reads these via pms.h externs).
#[no_mangle]
pub static mut pms_movies: [PmsMovie; PMS_MAX_MOVIES] = [PmsMovie::ZERO; PMS_MAX_MOVIES];
#[no_mangle]
pub static mut pms_nmovies: c_int = 0;

// ---- helpers ----
unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

fn jstr(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn hex3(hex: &str) -> [f32; 3] {
    let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
    [
        ((v >> 16) & 0xff) as f32 / 255.0,
        ((v >> 8) & 0xff) as f32 / 255.0,
        (v & 0xff) as f32 / 255.0,
    ]
}

/// copy `s` into a fixed C char buffer (truncate, NUL-terminate, newlines->spaces)
fn set_field(dst: &mut [u8], s: &str) {
    if dst.is_empty() {
        return;
    }
    let cleaned: Vec<u8> = s.bytes().map(|b| if b == b'\n' || b == b'\r' { b' ' } else { b }).collect();
    let n = cleaned.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&cleaned[..n]);
    dst[n] = 0;
}

/// percent-encode into a String (Rust callers, e.g. posters::poster_key)
pub(crate) fn urlenc_str(src: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(src.len());
    for &ch in src.as_bytes() {
        if ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_' | b'.' | b'~') {
            out.push(ch as char);
        } else {
            out.push('%');
            out.push(HEX[(ch >> 4) as usize] as char);
            out.push(HEX[(ch & 15) as usize] as char);
        }
    }
    out
}

/// percent-encode a Plex server-relative path for the transcode url= query value
#[no_mangle]
pub extern "C" fn urlenc(dst: *mut c_char, cap: usize, src: *const c_char) {
    if dst.is_null() || cap == 0 {
        return;
    }
    unsafe {
        let out = std::slice::from_raw_parts_mut(dst as *mut u8, cap);
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut o = 0usize;
        if !src.is_null() {
            let s = std::ffi::CStr::from_ptr(src).to_bytes();
            for &ch in s {
                if o + 4 >= cap {
                    break;
                }
                if ch.is_ascii_alphanumeric() || ch == b'-' || ch == b'_' || ch == b'.' || ch == b'~' {
                    out[o] = ch;
                    o += 1;
                } else {
                    out[o] = b'%';
                    out[o + 1] = HEX[(ch >> 4) as usize];
                    out[o + 2] = HEX[(ch & 15) as usize];
                    o += 3;
                }
            }
        }
        out[o] = 0;
    }
}

/// Fetch section <sec> ("Movies" is 1) and parse into pms_movies[]. Returns count.
#[no_mangle]
pub extern "C" fn pms_fetch_movies(host: *const c_char, port: c_int, token: *const c_char, sec: c_int) -> c_int {
    let r = catch_unwind(|| unsafe {
        std::ptr::write(std::ptr::addr_of_mut!(pms_nmovies), 0);
        let host_s = cstr(host);
        let token_s = cstr(token);
        let path = format!("/library/sections/{sec}/all?X-Plex-Token={token_s}");
        let body = match crate::stream::http_get(&host_s, port, &path, Some("Accept: application/json\r\n")) {
            Some(b) => b,
            None => return 0,
        };
        let json: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => return 0,
        };
        let meta = match json.get("MediaContainer").and_then(|m| m.get("Metadata")).and_then(|a| a.as_array()) {
            Some(m) => m,
            None => return 0,
        };
        let movies = std::slice::from_raw_parts_mut(
            std::ptr::addr_of_mut!(pms_movies) as *mut PmsMovie,
            PMS_MAX_MOVIES,
        );
        let mut n = 0usize;
        for item in meta {
            if n >= PMS_MAX_MOVIES {
                break;
            }
            let m = &mut movies[n];
            *m = PmsMovie::ZERO;
            set_field(&mut m.title, &jstr(item.get("title")));
            m.year = item.get("year").and_then(|v| v.as_i64()).unwrap_or(0) as c_int;
            set_field(&mut m.rating, &jstr(item.get("contentRating")));
            let durms = item.get("duration").and_then(|v| v.as_i64()).unwrap_or(0);
            m.dur_ns = if durms > 0 { durms * 1_000_000 } else { 0 };
            set_field(&mut m.thumb, &jstr(item.get("thumb")));
            set_field(&mut m.art, &jstr(item.get("art")));
            set_field(&mut m.summary, &jstr(item.get("summary")));
            set_field(&mut m.rk, &jstr(item.get("ratingKey")));
            // Media[0]: codecs + Part[0].key
            if let Some(md) = item.get("Media").and_then(|a| a.as_array()).and_then(|a| a.first()) {
                set_field(&mut m.vcodec, &jstr(md.get("videoCodec")));
                set_field(&mut m.acodec, &jstr(md.get("audioCodec")));
                if let Some(p0) = md.get("Part").and_then(|a| a.as_array()).and_then(|a| a.first()) {
                    set_field(&mut m.part, &jstr(p0.get("key")));
                }
            }
            // UltraBlurColors -> ambient gradient
            if let Some(ub) = item.get("UltraBlurColors") {
                if let Some(tl) = ub.get("topLeft").and_then(|v| v.as_str()) {
                    m.blur[0] = hex3(tl);
                    m.blur[1] = hex3(ub.get("topRight").and_then(|v| v.as_str()).unwrap_or("000000"));
                    m.blur[2] = hex3(ub.get("bottomRight").and_then(|v| v.as_str()).unwrap_or("000000"));
                    m.blur[3] = hex3(ub.get("bottomLeft").and_then(|v| v.as_str()).unwrap_or("000000"));
                    m.has_blur = 1;
                }
            }
            if m.title[0] != 0 && m.part[0] != 0 {
                n += 1;
            }
        }
        std::ptr::write(std::ptr::addr_of_mut!(pms_nmovies), n as c_int);
        n as c_int
    });
    r.unwrap_or(0)
}
