//! The playing item's chapters, as the HUD's Chapters section reads them. The strip itself is a
//! `cards::Shelf` the section's compiled state places (`player_hud::strip`); this module is the ONE read
//! of the chapter list and the one number the strip's drawing shares with it.
//!
//! Data comes from the PLAYING leaf (`metadata::playing_chapters`, loaded with `?includeChapters=1`
//! on the same fetch the track store already makes), never from `metadata::current()` — the same
//! identity rule `appkit/track_menu.rs` and `appkit/skip_pill.rs` state. Reading `current()` is what made
//! the Chapters tab vanish for every episode started from a show detail page: `current()` is then
//! the SHOW, and a show container carries no `Chapter[]`.
use plx_data::metadata;

/// Focus rim on the selected chapter. The card family's resting sheen ([`plx_ui::theme::CARD_SHEEN`] .22 /
/// 1px) washes out over the hardware video plane the same way an unkeyed control's edge did — pure
/// white and a thicker stroke are what keep "which chapter" readable from the couch while scrolling.
/// Colour is the unkeyed focus edge ([`plx_ui::theme::CONTROL_RIM_FOCUS_UNKEYED`]); width is deliberately a
/// step over that control's 1.25 so a 288-wide still reads as selected, not merely edged.
pub(crate) const CH_FOCUS_RING_W: f32 = 2.5;

/// the playing leaf's chapters — the ONE read, so within a frame the count, the open, the seek and
/// the draw cannot end up describing different items. ACROSS frames the store can still be replaced
/// (a new play retires it, `route::request_play`).
pub(crate) fn chapters<'a>(meta: metadata::MetadataView<'a>) -> &'a [metadata::Chapter] {
    meta.playing_chapters()
}

/// whether the PLAYING item has chapters — drives showing/hiding the Chapters tab
pub fn has_chapters(meta: metadata::MetadataView<'_>) -> bool {
    !chapters(meta).is_empty()
}
