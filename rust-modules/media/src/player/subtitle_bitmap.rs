//! Presentation-owned bitmap cues, independent of the physical playback executor.
/// one rect of a decoded image-subtitle display set: `w`×`h` at position (`x`,`y`) **in the
/// subtitle stream's own authoring canvas** (see [`SubBitmap::cw`]) — NOT in screen pixels; the
/// renderer scales it into the video rect.
///
/// **Held INDEXED, the way the decoder produced it** — one palette index per pixel plus the
/// set's 256-entry palette — and expanded to RGBA only when the renderer uploads it
/// ([`SubRect::to_rgba`], once per cue change). A quarter of the RGBA bytes is what lets the
/// store hold the whole window a delayed caption needs (`player::SUB_BITMAP_BUDGET` states the
/// arithmetic).
#[derive(Clone)]
pub struct SubRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// `w*h` palette indices, row-major, no padding.
    pub index: Vec<u8>,
    /// Straight-alpha RGBA per palette entry. All 256 entries, so any index byte is in range.
    pub palette: Box<[[u8; 4]; 256]>,
}

impl SubRect {
    /// the bytes this rect holds in the store (what the byte budget counts): its index buffer as
    /// ALLOCATED (a `Vec` that grew by doubling holds its spare capacity) plus its palette. The
    /// store holds only this indexed form; the RGBA expansion exists for the one set on screen.
    pub fn bytes(&self) -> usize {
        self.index.capacity() + std::mem::size_of::<[[u8; 4]; 256]>()
    }

    /// The straight-alpha RGBA bitmap of `w`×`h` the renderer uploads.
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(self.index.len() * 4);
        for &i in &self.index {
            rgba.extend_from_slice(&self.palette[i as usize]);
        }
        rgba
    }
}

/// one decoded image-subtitle display set (PGS/VobSub/DVB) — every rect of it, not just the
/// first: a two-line dialogue or a sign-plus-dialogue set is authored as several rects and they
/// belong to the same on-screen moment. `end_ns` is i64::MAX until a CLEAR display-set (or a
/// superseding set) truncates it. Pushed for EVERY image track while subtitles are on (ff.rs
/// decodes them all so a switch between image tracks is instant) and keyed by `start_ns` for the
/// renderer; the store's byte budget and eviction order (`player::push_subtitle_bitmap`) keep the
/// selected track's sets first.
pub struct SubBitmap {
    pub track: i32,
    pub start_ns: i64,
    pub end_ns: i64,
    /// authoring-canvas width/height the rects' coords are expressed in — 1920×1080 for
    /// Blu-ray PGS, 720×480/576 for a DVD VobSub rip, 3840×2160 for some 4K PGS. `0` = the
    /// decoder never declared one, in which case the renderer falls back to 1:1.
    pub cw: i32,
    pub ch: i32,
    pub rects: Vec<SubRect>,
}
impl SubBitmap {
    /// total bytes held by this display set (what the store's byte budget counts)
    pub fn bytes(&self) -> usize {
        self.rects.iter().map(SubRect::bytes).sum()
    }
}

