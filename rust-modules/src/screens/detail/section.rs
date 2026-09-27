//! Section identity versus visual order.
//!
//! Lookups and saved-column memory index by [`SectionId`]. Visual order is a separate list.
//! The compact title no longer has named hide ANCHORS — it fades out as whatever section is first
//! in visual order begins to travel (`super::compact_title_alpha`), so adding a section cannot
//! leave a 72px wordmark drawn over it. A section without a
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
/// The hero is slot 0, which is what `DetailScreen::sections` and every `Spot` walk assume.
const _: () = assert!(SectionId::Hero.raw() == 0);

impl SectionId {
    pub(crate) const fn raw(self) -> i32 {
        self as i32
    }

}
