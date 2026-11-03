//! `cbuf` — fixed NUL-terminated C-string buffer helpers (route HUD buffers, poster keys,
//! the text glyph-cache keys). ONE read + write pair so the truncate-and-NUL rules can't
//! drift between hand-rolled copies.
use std::os::raw::c_char;

/// borrow the NUL-terminated prefix of a fixed byte-array field (no copy, no UTF-8 check).
pub fn as_bytes(b: &[u8]) -> &[u8] {
    let n = b.iter().position(|&x| x == 0).unwrap_or(b.len());
    &b[..n]
}

/// write raw bytes into the fixed byte-array field, zero-filled then truncated (always
/// NUL-terminated). The byte-slice variant of [`set_bytes`] — no UTF-8 requirement.
pub fn set_bytes_raw(dst: &mut [u8], b: &[u8]) {
    dst.fill(0);
    let n = b.len().min(dst.len().saturating_sub(1));
    dst[..n].copy_from_slice(&b[..n]);
}

/// write `s` into the fixed byte-array field, zero-filled then truncated at a UTF-8 boundary
/// (always NUL-terminated).
pub fn set_bytes(dst: &mut [u8], s: &str) {
    set_bytes_raw(dst, text_prefix(s, dst.len().saturating_sub(1)));
}

fn text_prefix(s: &str, limit: usize) -> &[u8] {
    let mut n = s.len().min(limit);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    &s.as_bytes()[..n]
}

/// write `s` into a raw `c_char` buffer of `cap` bytes, truncated at a UTF-8 boundary + NUL-terminated.
/// # Safety: `dst` must point to at least `cap` writable bytes.
pub unsafe fn set(dst: *mut c_char, cap: usize, s: &str) {
    if dst.is_null() || cap == 0 {
        return;
    }
    let out = std::slice::from_raw_parts_mut(dst as *mut u8, cap);
    let prefix = text_prefix(s, cap - 1);
    out[..prefix.len()].copy_from_slice(prefix);
    out[prefix.len()] = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_writers_truncate_only_at_utf8_boundaries() {
        let title = "Надзвычай доўгі раздзел, у якім апошні цмок вяртаецца праз заснежаныя горы";
        for text in [title, "Aé中😀Z", "plain ASCII"] {
            for cap in 0..=text.len() + 2 {
                let expected = text.char_indices().map(|(i, _)| i).chain([text.len()])
                    .filter(|&i| i <= cap.saturating_sub(1)).max().unwrap_or(0);
                let mut bytes = vec![0x7f; cap];
                set_bytes(&mut bytes, text);
                assert_eq!(as_bytes(&bytes), &text.as_bytes()[..expected], "slice cap={cap}");
                let mut raw = vec![0x7f_u8; cap];
                unsafe { set(raw.as_mut_ptr().cast(), cap, text); }
                assert_eq!(as_bytes(&raw), &text.as_bytes()[..expected], "pointer cap={cap}");
                if cap > 0 {
                    assert_eq!(bytes[expected], 0);
                    assert_eq!(raw[expected], 0);
                }
            }
        }
    }

    #[test]
    fn byte_keys_keep_their_exact_prefix_even_inside_a_codepoint() {
        let mut key = [0; 2];
        set_bytes_raw(&mut key, "é".as_bytes());
        assert_eq!(key, [0xc3, 0]);
    }
}
