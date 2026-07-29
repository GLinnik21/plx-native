//! Rust replacement for src/img.c — image decode + GL upload, same C ABI (img.h).
//! Runs on the poster worker threads (decode) + the GL/main thread (upload).
//!
//! **What the panic guard actually covers.** The `catch_unwind` below turns a decoder *panic* — a
//! bounds or arithmetic failure inside the pure-Rust JPEG/PNG decoders on a truncated or malformed
//! file — into a NULL return, so a bad poster is a missing tile instead of a dead app, and so no
//! unwind reaches the C caller. It does **not** make decoding "never crash the app", which is what
//! this doc used to claim flatly: Rust's allocation-failure path is `handle_alloc_error`, which
//! **aborts** the process. An abort unwinds nothing, so it sails straight past every `catch_unwind`
//! in the tree — and an unbounded decode on a device whose manifest declares `requiredMemory: 60`
//! is exactly how you reach one. Bounding the allocation is therefore a separate, explicit job that
//! the guard cannot do for us; see [`decode_limits`].
use std::os::raw::{c_int, c_uchar, c_uint, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
}

use crate::log;

/// The decode budget, sized for THIS device. `image`'s defaults are not one: `max_image_width` and
/// `max_image_height` default to `None` — no dimension cap at all — and `max_alloc` to **512 MiB**,
/// which a 32-bit ARM TV declaring `requiredMemory: 60` in `pkg/appinfo.json` cannot honour and
/// would abort trying to (see the module doc on why the `catch_unwind` cannot save us there).
///
/// Every image the app fetches is a `/photo/:/transcode?width=W&height=H&minSize=1` request whose
/// W×H **we** choose, so the legitimate ceiling is knowable rather than guessed:
///
/// | call site | box requested |
/// |---|---|
/// | `ui/detail.rs` hero art | 1920×1080 |
/// | `ui/home.rs` hero backdrop | 1280×720 |
/// | `posters.rs` clearLogo (PNG) | 600×240 |
/// | `ui/info_panel.rs` episode still | 480×270 |
/// | `ui/widgets.rs` catalog poster | 250×375 |
///
/// — plus the plex.tv sign-in QR PNG, a few hundred pixels square. 1920×1080 is thus the largest
/// thing we ever ASK for, but the cap cannot be set there: `minSize=1` means *cover*, not *fit*, so
/// the server scales until the box is filled and an extreme source aspect comes back longer on its
/// long edge (a 2.4:1 backdrop into a 16:9 box → ~2592×1080; a 2:3 poster into it → 1920×2880).
/// **4096 on each axis** is over twice the requested box, so no legitimate cover-scaled result can
/// trip it, while the classic decompression bomb (a PNG header declaring 64000×64000) is rejected
/// from its HEADER — before a single pixel buffer is allocated, which is the whole point of a
/// strict dimension limit over a byte budget alone.
///
/// **32 MiB** is the byte budget and the tighter of the two gates (4096² would need ~50 MiB even at
/// 3 bytes/px, so a bomb that squeaks under the dimension caps still dies here). It sits against a
/// largest-legitimate decode of ~21 MiB (1920×2880 RGBA, the cover-scaled poster case) and ~6 MiB
/// for the common 1920×1080 RGB JPEG. Note it bounds the DECODER's allocations only — the
/// `to_rgba8()` conversion and the `malloc`'d copy handed back to the caller are ours and sit
/// outside it, which is a second reason to keep this well under the device's headroom, not at it.
fn decode_limits() -> image::Limits {
    // Field assignment rather than a struct literal: `Limits` is `#[non_exhaustive]`.
    let mut l = image::Limits::default();
    l.max_image_width = Some(4096);
    l.max_image_height = Some(4096);
    l.max_alloc = Some(32 * 1024 * 1024);
    l
}

pub(crate) fn img_decode_rgba(buf: *const c_uchar, len: c_int,
                                  w: *mut c_int, h: *mut c_int) -> *mut c_uchar {
    if buf.is_null() || len <= 0 { return ptr::null_mut(); }
    let data = unsafe { std::slice::from_raw_parts(buf, len as usize) };
    let magic: String = data.iter().take(6).map(|b| format!("{b:02x}")).collect();
    // decode inside catch_unwind so a decoder panic can't unwind into C. `ImageReader` rather than
    // the `load_from_memory` one-liner purely so the limits above can be attached — that helper
    // hard-codes `Limits::default()` internally, with no way to pass any.
    //
    // The failure now carries its REASON, which it did not have to before: with limits in force,
    // "no image came back" covers both a corrupt file and one this device refuses to decode, and
    // those want opposite responses (ignore it vs. re-open the numbers in `decode_limits`). Same
    // `img: decode-none` prefix, so anything grepping the event log for it still matches.
    let decoded = catch_unwind(AssertUnwindSafe(|| -> Result<(c_int, c_int, Vec<u8>), String> {
        let mut rdr = image::ImageReader::new(std::io::Cursor::new(data))
            .with_guessed_format()
            .map_err(|e| format!("unreadable: {e}"))?;
        rdr.limits(decode_limits());
        let img = rdr.decode().map_err(|e| e.to_string())?;
        let r = img.to_rgba8();
        Ok((r.width() as c_int, r.height() as c_int, r.into_raw()))
    }));
    let (iw, ih, raw) = match decoded {
        Ok(Ok(t)) => t,
        Ok(Err(why)) => { log(&format!("img: decode-none len={len} magic={magic} — {why}")); return ptr::null_mut(); }
        Err(_)       => { log(&format!("img: PANIC len={len} magic={magic}")); return ptr::null_mut(); }
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
