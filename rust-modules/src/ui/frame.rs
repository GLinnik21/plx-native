//! The frame scheduler's `Budget` — admission control, not sampling (spec §8.1).
//!
//! Phase 2-i / 2 ship the MINIMAL type: the struct, `take(class)` in the clock-before-every-take
//! shape, one class (`Poster`, quota 3 per frame) and `has_queued_work`. Phase 11 adds the
//! remaining classes with their device-measured `worst_us`, the solo-frame rule and the text
//! prewarm occupancy bound. The shape is fixed now because §2.2 makes `Budget` the owner of the
//! poster quota and §3.3 step 8 reads `has_queued_work` from it.
#![allow(dead_code)] // phase 2-i: no consumer until phase 2 (spec §13)

/// ≈25 % of the measured ~7.8 ms discretionary headroom on a Home frame.
pub const PREPARE_MAX_US: u64 = 2000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    /// One decoded poster's GL upload. Quota 3 per frame; `worst_us` is phase 11's measurement.
    Poster,
}

impl Class {
    /// The device-measured worst case, pinned by a test in phase 11. A placeholder until then,
    /// chosen so three uploads fit the ceiling: the quota is the binding bound today.
    pub const fn worst_us(self) -> u64 {
        match self {
            Class::Poster => 400,
        }
    }

    const fn quota(self) -> u8 {
        match self {
            Class::Poster => 3,
        }
    }
}

pub struct Budget {
    frame_start_us: u64,
    poster_left: u8,
    queued: bool,
    admitted: u32,
    refused: u32,
}

impl Budget {
    pub fn new() -> Self {
        Self {
            frame_start_us: 0,
            poster_left: 0,
            queued: false,
            admitted: 0,
            refused: 0,
        }
    }

    /// Opens a frame: quotas reset, the clock origin recorded.
    pub fn begin_frame(&mut self, now_us: u64) {
        self.frame_start_us = now_us;
        self.poster_left = Class::Poster.quota();
        self.admitted = 0;
        self.refused = 0;
    }

    /// Admission. The caller reads the clock BEFORE every take and hands it in — one
    /// `SDL_GetPerformanceCounter` per take, never a stale value — and is admitted only if the
    /// class's worst case still fits the ceiling and its quota is not spent. Forward progress:
    /// the FIRST take of a class that fits is always admitted.
    pub fn take(&mut self, class: Class, now_us: u64) -> bool {
        let left = match class {
            Class::Poster => &mut self.poster_left,
        };
        if *left == 0 {
            self.refused += 1;
            return false;
        }
        let fits = now_us.saturating_sub(self.frame_start_us) + class.worst_us() <= PREPARE_MAX_US;
        let first = self.admitted == 0;
        if fits || first {
            *left -= 1;
            self.admitted += 1;
            true
        } else {
            self.refused += 1;
            false
        }
    }

    /// Whether prepare work is waiting — the second term of §3.3 step 8's present decision.
    pub fn has_queued_work(&self) -> bool {
        self.queued
    }

    /// Reported by whoever holds a queue (`TexCache`) at the top of the frame.
    pub fn note_queued(&mut self, queued: bool) {
        self.queued = queued;
    }

    pub fn admitted(&self) -> u32 {
        self.admitted
    }

    pub fn refused(&self) -> u32 {
        self.refused
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::new()
    }
}

/// The residency ceiling for every render alive in one frame (§8.3): a placeholder until phase 11
/// sets it from the 160 MB `requiredMemory` measurement.
pub const RENDER_BYTES_MAX: usize = 48 << 20;
/// One full-viewport `FrameCache` (1920×1080×4), the single one a Cached host is served from.
pub const FRAME_CACHE_BYTES: usize = 1920 * 1080 * 4;

/// Every `ScreenRender` alive this frame (§8.3): the frame plan's list, checked as a whole rather
/// than as the page pair alone.
#[derive(Clone, Debug, Default)]
pub struct RenderSet {
    /// Page renders drawn: the top page, plus the level beneath it under a push.
    pub pages: u32,
    /// `(surface, renders)` per Active surface.
    pub surfaces: Vec<(super::machine::EntryId, u32)>,
    /// The sum of every render's backing-texture bytes.
    pub bytes: usize,
    /// The one shared `FrameCache`, when a Cached host is being served from it.
    pub frame_cache_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderBreach {
    /// More than two page renders.
    Pages(u32),
    /// A surface holding more than one render.
    Surface(super::machine::EntryId, u32),
    /// The sum of every render plus the `FrameCache` is over `RENDER_BYTES_MAX`.
    Bytes(usize),
}

impl std::fmt::Display for RenderBreach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderBreach::Pages(n) => write!(f, "{n} page renders (max 2)"),
            RenderBreach::Surface(e, n) => write!(f, "surface {} holds {n} renders (max 1)", e.0),
            RenderBreach::Bytes(b) => write!(f, "{b} render bytes (max {RENDER_BYTES_MAX})"),
        }
    }
}

impl RenderSet {
    /// (a) pages ≤ 2, (b) one render per surface, (c) bytes + the `FrameCache` under the ceiling.
    pub fn check(&self) -> Result<(), RenderBreach> {
        if self.pages > 2 {
            return Err(RenderBreach::Pages(self.pages));
        }
        if let Some((e, n)) = self.surfaces.iter().find(|(_, n)| *n > 1) {
            return Err(RenderBreach::Surface(*e, *n));
        }
        let total = self.bytes + self.frame_cache_bytes;
        if total > RENDER_BYTES_MAX {
            return Err(RenderBreach::Bytes(total));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_poster_quota_is_three_per_frame_and_the_ceiling_refuses_late_work() {
        let mut b = Budget::new();
        b.begin_frame(1000);
        assert!(b.take(Class::Poster, 1000));
        assert!(b.take(Class::Poster, 1100));
        assert!(b.take(Class::Poster, 1200));
        assert!(!b.take(Class::Poster, 1300), "quota spent");
        b.begin_frame(5000);
        assert!(b.take(Class::Poster, 5000 + PREPARE_MAX_US), "the first take always fits");
        assert!(!b.take(Class::Poster, 5000 + PREPARE_MAX_US), "the second does not");
        assert_eq!(b.refused(), 1);
    }
}
