//! Rust replacement for src/img.c — image decode + GL upload, same C ABI (img.h).
//! Decode is panic-safe: a malformed/unsupported image returns NULL (like stb),
//! never crashes the app. Runs on the poster worker threads (decode) + GL thread.
use std::os::raw::{c_int, c_uchar, c_uint, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
    fn glGenTextures(n: c_int, textures: *mut c_uint);
    fn glBindTexture(target: c_uint, texture: c_uint);
    fn glPixelStorei(pname: c_uint, param: c_int);
    fn glTexImage2D(target: c_uint, level: c_int, internalformat: c_int, width: c_int,
                    height: c_int, border: c_int, format: c_uint, ty: c_uint, pixels: *const c_void);
    fn glTexParameteri(target: c_uint, pname: c_uint, param: c_int);
}
const GL_TEXTURE_2D: c_uint = 0x0DE1;
const GL_RGBA: c_uint = 0x1908;
const GL_UNSIGNED_BYTE: c_uint = 0x1401;
const GL_UNPACK_ALIGNMENT: c_uint = 0x0CF5;
const GL_TEXTURE_MIN_FILTER: c_uint = 0x2801;
const GL_TEXTURE_MAG_FILTER: c_uint = 0x2800;
const GL_TEXTURE_WRAP_S: c_uint = 0x2802;
const GL_TEXTURE_WRAP_T: c_uint = 0x2803;
const GL_LINEAR: c_int = 0x2601;
const GL_CLAMP_TO_EDGE: c_int = 0x812F;

fn log(m: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/poc-events.log") {
        let _ = writeln!(f, "{m}");
    }
}

#[no_mangle]
pub extern "C" fn img_decode_rgba(buf: *const c_uchar, len: c_int,
                                  w: *mut c_int, h: *mut c_int) -> *mut c_uchar {
    if buf.is_null() || len <= 0 { return ptr::null_mut(); }
    let data = unsafe { std::slice::from_raw_parts(buf, len as usize) };
    let magic: String = data.iter().take(6).map(|b| format!("{b:02x}")).collect();
    // decode inside catch_unwind so a decoder panic can't unwind into C
    let decoded = catch_unwind(AssertUnwindSafe(|| {
        image::load_from_memory(data).ok().map(|img| {
            let r = img.to_rgba8();
            (r.width() as c_int, r.height() as c_int, r.into_raw())
        })
    }));
    let (iw, ih, raw) = match decoded {
        Ok(Some(t)) => t,
        Ok(None)  => { log(&format!("img: decode-none len={len} magic={magic}")); return ptr::null_mut(); }
        Err(_)    => { log(&format!("img: PANIC len={len} magic={magic}")); return ptr::null_mut(); }
    };
    let n = raw.len();
    let px = unsafe { malloc(n) } as *mut c_uchar;
    if px.is_null() { return ptr::null_mut(); }
    unsafe {
        ptr::copy_nonoverlapping(raw.as_ptr(), px, n);
        if !w.is_null() { *w = iw; }
        if !h.is_null() { *h = ih; }
    }
    px
}

#[no_mangle]
pub extern "C" fn img_free(px: *mut c_uchar) {
    if !px.is_null() { unsafe { free(px as *mut c_void) } }
}

#[no_mangle]
pub extern "C" fn img_upload_rgba(px: *const c_uchar, w: c_int, h: c_int) -> c_uint {
    if px.is_null() || w <= 0 || h <= 0 { return 0; }
    let mut t: c_uint = 0;
    unsafe {
        glGenTextures(1, &mut t);
        if t == 0 { return 0; }
        glBindTexture(GL_TEXTURE_2D, t);
        glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA as c_int, w, h, 0, GL_RGBA, GL_UNSIGNED_BYTE, px as *const c_void);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    }
    t
}

#[no_mangle]
pub extern "C" fn img_tex_from_memory(buf: *const c_uchar, len: c_int,
                                      out_w: *mut c_int, out_h: *mut c_int) -> c_uint {
    let (mut w, mut h) = (0, 0);
    let px = img_decode_rgba(buf, len, &mut w, &mut h);
    if px.is_null() { return 0; }
    let t = img_upload_rgba(px, w, h);
    img_free(px);
    unsafe { if !out_w.is_null() { *out_w = w } if !out_h.is_null() { *out_h = h } }
    t
}
