//! Owned container color metadata; no codec handles, native parsing or presentation effects.

/// A rational as FFmpeg stores one (`AVRational`).
pub type Rat = (i32, i32);

/// `AVMasteringDisplayMetadata`, read out of the stream's coded side data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mastering {
    /// `display_primaries[3][2]`: R, G, B, each (x, y).
    pub primaries: [[Rat; 2]; 3],
    pub white_point: [Rat; 2],
    pub min_luminance: Rat,
    pub max_luminance: Rat,
    pub has_primaries: bool,
    pub has_luminance: bool,
}

/// What the container said about the picture's colour, as the demuxer read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContainerHdr {
    /// `AVCodecParameters.color_trc`, `color_primaries`, `color_space` (H.273 code points).
    pub trc: i32,
    pub pri: i32,
    pub mc: i32,
    pub mastering: Option<Mastering>,
    /// (MaxCLL, MaxFALL), from `AVContentLightMetadata`.
    pub cll: Option<(u32, u32)>,
}
