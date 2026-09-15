//! Section identity versus visual order.
//!
//! Lookups and saved-column memory index by [`SectionId`]. Visual order is a separate list.
//! A new section that is not a hide anchor does not move the compact title. A section without a
//! slot cannot be stored by accident: the slot index is this enum's discriminant, and
//! [`crate::metadata::SPOT_SECTION_SLOTS`] is that count.

use crate::metadata::SPOT_SECTION_SLOTS;

pub(crate) const SLOTS: usize = SPOT_SECTION_SLOTS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum SectionId {
    Hero = 0,
    Season = 1,
    Episode = 2,
    Related = 3,
    Cast = 4,
    About = 5,
    Extras = 6,
}

const _: () = assert!(SLOTS == (SectionId::Extras as usize) + 1);

impl SectionId {
    pub(crate) const fn raw(self) -> i32 {
        self as i32
    }

    pub(crate) fn from_raw(n: i32) -> Option<Self> {
        Some(match n {
            0 => Self::Hero,
            1 => Self::Season,
            2 => Self::Episode,
            3 => Self::Related,
            4 => Self::Cast,
            5 => Self::About,
            6 => Self::Extras,
            _ => return None,
        })
    }

    /// Cast, Related, About. Extras is not one — it sits between Cast and Related in visual order
    /// (`DetailScreen::sections`) but must not itself move the compact title's appear/disappear
    /// threshold, which walks the visual-order array by anchor membership, not by adjacency to
    /// Extras. `is_hide_anchor` checks identity, so this holds regardless of where Extras sits.
    pub(crate) const fn is_hide_anchor(self) -> bool {
        matches!(self, Self::Cast | Self::Related | Self::About)
    }
}

pub(crate) fn is_hide_anchor(section: i32) -> bool {
    SectionId::from_raw(section).is_some_and(SectionId::is_hide_anchor)
}
