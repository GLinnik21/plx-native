//! VP9 container HDR metadata and FFmpeg side-data parsing. Presentation envelope rewriting
//! stays in the media adapter that owns sourceInfo and ACB publication.

pub use plx_playback_contract::video::{Rat, Mastering, ContainerHdr, TRC_PQ};

fn le_i32(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn rat(b: &[u8], at: usize) -> Option<Rat> {
    Some((le_i32(b, at)?, le_i32(b, at + 4)?))
}

/// An `AVMasteringDisplayMetadata` (88 bytes on 32-bit ARM and on the 64-bit host alike: eight
/// `AVRational` pairs then two ints). `None` when the payload is short.
pub fn parse_mastering(b: &[u8]) -> Option<Mastering> {
    if b.len() < 88 {
        return None;
    }
    let mut primaries = [[(0, 0); 2]; 3];
    for (i, p) in primaries.iter_mut().enumerate() {
        p[0] = rat(b, i * 16)?;
        p[1] = rat(b, i * 16 + 8)?;
    }
    Some(Mastering {
        primaries,
        white_point: [rat(b, 48)?, rat(b, 56)?],
        min_luminance: rat(b, 64)?,
        max_luminance: rat(b, 72)?,
        has_primaries: le_i32(b, 80)? != 0,
        has_luminance: le_i32(b, 84)? != 0,
    })
}

/// An `AVContentLightMetadata` (two unsigned ints: MaxCLL, MaxFALL).
pub fn parse_cll(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 8 {
        return None;
    }
    Some((
        u32::from_le_bytes(b[0..4].try_into().ok()?),
        u32::from_le_bytes(b[4..8].try_into().ok()?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(n: i32, d: i32) -> Rat {
        (n, d)
    }

    /// x265's `G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1)` as FFmpeg
    /// reads it back (rationals over 50000 / 10000).
    fn mastering() -> Mastering {
        Mastering {
            primaries: [
                [r(34000, 50000), r(16000, 50000)], // R
                [r(13250, 50000), r(34500, 50000)], // G
                [r(7500, 50000), r(3000, 50000)],   // B
            ],
            white_point: [r(15635, 50000), r(16450, 50000)],
            min_luminance: r(1, 10000),
            max_luminance: r(10_000_000, 10000),
            has_primaries: true,
            has_luminance: true,
        }
    }

    /// An `AVMasteringDisplayMetadata` laid out as FFmpeg's header lays it out (88 bytes).
    #[test]
    fn the_side_data_structs_parse_at_ffmpegs_offsets() {
        let m = mastering();
        let mut b = Vec::new();
        for p in m.primaries {
            for (n, d) in p {
                b.extend_from_slice(&n.to_le_bytes());
                b.extend_from_slice(&d.to_le_bytes());
            }
        }
        for (n, d) in m.white_point.iter().chain([&m.min_luminance, &m.max_luminance]) {
            b.extend_from_slice(&n.to_le_bytes());
            b.extend_from_slice(&d.to_le_bytes());
        }
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&1i32.to_le_bytes());
        assert_eq!(b.len(), 88);
        assert_eq!(parse_mastering(&b), Some(m));
        assert_eq!(parse_mastering(&b[..87]), None, "a short payload is refused, not read past");
        let mut c = 1000u32.to_le_bytes().to_vec();
        c.extend_from_slice(&400u32.to_le_bytes());
        assert_eq!(parse_cll(&c), Some((1000, 400)));
        assert_eq!(parse_cll(&c[..7]), None);
    }

}
