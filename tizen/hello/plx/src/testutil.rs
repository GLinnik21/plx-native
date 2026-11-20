//! Helpers shared by the unit tests (compiled for tests only).

use crate::h5::Label;

/// Write the RGBA image as a 32-bit BMP so a human can look at it (`sips -s format png in.bmp --out
/// out.png` converts on macOS). Used by `PLX_H5_DUMP` (the H5 label test) and `PLX_PANEL_DUMP` (the panel
/// tests).
#[cfg_attr(not(plx_sdl), allow(dead_code))]
pub fn dump_bmp(label: &Label, path: &str) {
    let (w, h) = (label.width as u32, label.height as u32);
    let mut out = Vec::new();
    let size = 54 + w * h * 4;
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes()); // positive height: bottom-up rows
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for y in (0..h as usize).rev() {
        for x in 0..w as usize {
            let p = &label.rgba[(y * w as usize + x) * 4..][..4];
            out.extend_from_slice(&[p[2], p[1], p[0], p[3]]); // BGRA
        }
    }
    std::fs::write(path, out).unwrap();
}
