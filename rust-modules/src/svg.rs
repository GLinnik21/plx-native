//! Runtime SVG rasterizer FFI (src/svg.c / nanosvg). Rasterizes a vector icon *asset* to an
//! RGBA mask at a target pixel size; `crate::ui::icons` uploads that as a GL texture and tints
//! it per state. White where drawn (author icons in #ffffff), alpha = coverage.
use std::os::raw::{c_char, c_int, c_uchar};

extern "C" {
    fn svg_rasterize_rgba(svg: *const c_char, len: c_int, w: c_int, h: c_int) -> *mut c_uchar;
    fn svg_free(p: *mut c_uchar);
}

/// Rasterize `svg` to a `w*h` RGBA buffer (row-major, 4 bytes/px). `None` on bad args or a
/// parse/alloc failure. The C side copies the input, so `svg` need not be NUL-terminated.
pub(crate) fn rasterize(svg: &str, w: i32, h: i32) -> Option<Vec<u8>> {
    if svg.is_empty() || w <= 0 || h <= 0 {
        return None;
    }
    let px = unsafe { svg_rasterize_rgba(svg.as_ptr() as *const c_char, svg.len() as c_int, w, h) };
    if px.is_null() {
        return None;
    }
    let n = (w as usize) * (h as usize) * 4;
    let out = unsafe { std::slice::from_raw_parts(px, n) }.to_vec();
    unsafe { svg_free(px) };
    Some(out)
}
