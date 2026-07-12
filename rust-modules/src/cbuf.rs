//! `cbuf` — fixed NUL-terminated C-string buffer helpers, shared by the C-ABI data structs
//! (pms catalog rows, route HUD buffers, poster keys). ONE read + write pair so the
//! truncate-and-NUL rules can't drift between hand-rolled copies.
use std::os::raw::c_char;

/// read a NUL-terminated C-string field into a Rust String (lossy UTF-8).
pub(crate) fn get(b: &[u8]) -> String {
    let n = b.iter().position(|&x| x == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..n]).into_owned()
}

/// write `s` into the fixed byte-array field, zero-filled then truncated (always NUL-terminated).
pub(crate) fn set_bytes(dst: &mut [u8], s: &str) {
    dst.fill(0);
    let b = s.as_bytes();
    let n = b.len().min(dst.len().saturating_sub(1));
    dst[..n].copy_from_slice(&b[..n]);
}

/// write `s` into a raw `c_char` buffer of `cap` bytes, truncated + NUL-terminated.
/// # Safety: `dst` must point to at least `cap` writable bytes.
pub(crate) unsafe fn set(dst: *mut c_char, cap: usize, s: &str) {
    if dst.is_null() || cap == 0 {
        return;
    }
    let out = std::slice::from_raw_parts_mut(dst as *mut u8, cap);
    let b = s.as_bytes();
    let n = b.len().min(cap - 1);
    out[..n].copy_from_slice(&b[..n]);
    out[n] = 0;
}
