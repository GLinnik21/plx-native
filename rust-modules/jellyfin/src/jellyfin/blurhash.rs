//! BlurHash into the four ambient corner colours the UI paints behind a page.
//!
//! PMS sends `UltraBlurColors`, four sRGB corners of a blurred backdrop, and the ambient wash (Home's
//! backdrop, the detail page's ground below the hero, the person page) keys on them through
//! `plx_plex::plex::UltraBlurColors::corners`. The player does not: its dims are neutral.
//! Jellyfin sends no such thing, but every image tag comes with a BlurHash (`ImageBlurHashes`), a
//! few DCT coefficients of the same picture. Evaluating that at four points just inside the corners
//! gives the colours a blur would, with no image fetch.
//!
//! Decoder per the reference algorithm (github.com/woltapp/blurhash, MIT): base-83 digits; the size
//! digit names the component grid; the DC term is sRGB, the AC terms are signed-square quantised
//! against a maximum. Sampled at 15 % / 85 % rather than the very corner, which is where a blur's
//! own averaging window would sit. Adapted from sostk/native-jelly (GPL-3.0-or-later).

const ALPHABET: &[u8; 83] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz#$%*+,-.:;=?@[]^_{|}~";

fn decode83(s: &[u8]) -> Option<u32> {
    s.iter().try_fold(0u32, |acc, c| {
        let d = ALPHABET.iter().position(|a| a == c)? as u32;
        acc.checked_mul(83)?.checked_add(d)
    })
}

fn srgb_to_linear(v: u32) -> f32 {
    let x = v as f32 / 255.0;
    if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.003_130_8 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 }
}

fn sign_pow(v: f32, e: f32) -> f32 {
    v.abs().powf(e).copysign(v)
}

/// The decoded component grid: `(nx, ny, linear-RGB components)`.
fn components(hash: &str) -> Option<(usize, usize, Vec<[f32; 3]>)> {
    let b = hash.as_bytes();
    if b.len() < 6 {
        return None;
    }
    let size = decode83(&b[..1])? as usize;
    let (nx, ny) = (size % 9 + 1, size / 9 + 1);
    if b.len() != 4 + 2 * nx * ny {
        return None;
    }
    let max = (decode83(&b[1..2])? as f32 + 1.0) / 166.0;
    // Four digits reach past 24 bits and two past 19^3: either is not a hash an encoder wrote.
    let dc = decode83(&b[2..6]).filter(|v| *v <= 0xff_ffff)?;
    let mut out = Vec::with_capacity(nx * ny);
    out.push([srgb_to_linear(dc >> 16), srgb_to_linear((dc >> 8) & 255), srgb_to_linear(dc & 255)]);
    for i in 1..nx * ny {
        let v = decode83(&b[4 + i * 2..6 + i * 2]).filter(|v| *v < 19 * 19 * 19)?;
        let q = [v / (19 * 19), (v / 19) % 19, v % 19];
        out.push(q.map(|c| sign_pow((c as f32 - 9.0) / 9.0, 2.0) * max));
    }
    Some((nx, ny, out))
}

/// The sRGB colour at normalised `(u, v)` ∈ [0,1]².
fn sample(nx: usize, ny: usize, comps: &[[f32; 3]], u: f32, v: f32) -> [f32; 3] {
    let mut acc = [0.0f32; 3];
    for j in 0..ny {
        for i in 0..nx {
            let basis = (std::f32::consts::PI * u * i as f32).cos() * (std::f32::consts::PI * v * j as f32).cos();
            let c = comps[i + j * nx];
            for k in 0..3 {
                acc[k] += c[k] * basis;
            }
        }
    }
    acc.map(linear_to_srgb)
}

/// Four corners in `UltraBlurColors::corners` RING order — top-left, top-right, bottom-right,
/// bottom-left — or `None` for a malformed hash.
pub fn corners(hash: &str) -> Option<[[f32; 3]; 4]> {
    let (nx, ny, c) = components(hash)?;
    let (lo, hi) = (0.15, 0.85);
    Some([
        sample(nx, ny, &c, lo, lo),
        sample(nx, ny, &c, hi, lo),
        sample(nx, ny, &c, hi, hi),
        sample(nx, ny, &c, lo, hi),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_hash_gives_four_distinct_in_gamut_corners() {
        // the reference implementation's own sample hash
        let c = corners("LEHV6nWB2yk8pyo0adR*.7kCMdnj").expect("valid");
        for corner in c {
            assert!(corner.iter().all(|x| (0.0..=1.0).contains(x)), "{corner:?}");
        }
        assert_ne!(c[0], c[2], "a picture with structure has different corners");
    }

    #[test]
    fn malformed_hashes_are_none_never_a_panic() {
        // too short, one digit short of its grid, a digit outside the alphabet, multi-byte text, and
        // a DC term past 24 bits
        for h in ["", "abc", "LEHV6nWB2yk8pyo0adR*.7kCMdn", "LEHV6nWB2yk8pyo0adR*.7kCMdn\"", "\u{e9}\u{e9}\u{e9}", "00~~~~"] {
            assert!(corners(h).is_none(), "{h}");
        }
    }
}
