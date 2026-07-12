//! Rust replacement for src/img.c — image decode + GL upload, same C ABI (img.h).
//! Decode is panic-safe: a malformed/unsupported image returns NULL (like stb),
//! never crashes the app. Runs on the poster worker threads (decode) + GL thread.
use std::os::raw::{c_int, c_uchar, c_uint, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
}

use crate::log;

pub(crate) fn img_decode_rgba(buf: *const c_uchar, len: c_int,
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

pub(crate) fn img_free(px: *mut c_uchar) {
    if !px.is_null() { unsafe { free(px as *mut c_void) } }
}

/// Upload decoded RGBA pixels into a fresh GL texture (gfx owns the GL bindings). Main thread.
pub(crate) fn img_upload_rgba(px: *const c_uchar, w: c_int, h: c_int) -> c_uint {
    if px.is_null() || w <= 0 || h <= 0 {
        return 0;
    }
    crate::gfx::upload_rgba(0, w, h, px)
}
