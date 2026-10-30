//! A QR code as a PNG, for the sign-in screen (which draws the PNG plex.tv used to send).
//!
//! Black modules on a transparent ground, like the code plex.tv served (the screen draws it as a
//! mask on its own white card). Gray + alpha, wrapped in zlib "stored" (uncompressed) blocks: no
//! compressor needed, and a QR at this size is a few hundred kilobytes at most.

use qrcodegen::{QrCode, QrCodeEcc};

/// `text` as a black-on-transparent QR, `scale` pixels per module, with the standard 4-module border.
pub fn qr_png(text: &str, scale: usize) -> Option<Vec<u8>> {
    let qr = QrCode::encode_text(text, QrCodeEcc::Medium).ok()?;
    let scale = scale.max(1);
    let border = 4;
    let modules = qr.size() as usize + 2 * border;
    let side = modules * scale;
    let mut raw = Vec::with_capacity((2 * side + 1) * side);
    for y in 0..side {
        raw.push(0); // filter: none
        for x in 0..side {
            let mx = (x / scale) as i32 - border as i32;
            let my = (y / scale) as i32 - border as i32;
            raw.extend_from_slice(if qr.get_module(mx, my) { &[0, 255] } else { &[0, 0] });
        }
    }
    Some(png_gray(side as u32, side as u32, &raw))
}

fn png_gray(w: u32, h: u32, filtered_rows: &[u8]) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 4, 0, 0, 0]); // 8-bit gray + alpha, deflate, no filter set, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib_stored(filtered_rows));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut z = vec![0x78, 0x01];
    let mut blocks = data.chunks(65_535).peekable();
    while let Some(b) = blocks.next() {
        z.push(u8::from(blocks.peek().is_none()));
        let len = b.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    z
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &x in data {
        c ^= u32::from(x);
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_png_reference() {
        assert_eq!(crc32(b"IEND"), 0xae42_6082);
    }

    #[test]
    fn a_qr_is_a_png_with_the_expected_size() {
        let png = qr_png("http://192.168.1.20:8096/web/#/quickconnect", 6).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
        assert_eq!(w % 6, 0);
        assert!(w >= 6 * (21 + 8));
    }
}
