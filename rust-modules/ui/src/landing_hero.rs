//! Shared geometry and atmospheric curve for a bottom-anchored landing-page hero. Keeping these
//! pure lets the renderer, route narrative and legibility tests use one definition.

use super::hstack::Item;
use super::vstack::{Anchor, VSpan, VStack};
use super::{consts::SCR_H, theme, widgets::HERO_BASE_SCRIM_Y0};

pub const TEXT_BOTTOM: f32 = 692.0;
pub const COL_W: f32 = 660.0;
pub const KNEE_Y: f32 = 0.65 * SCR_H;
pub const MID_WEIGHT: f32 = 0.55;
/// How much a bound preview multiplies the hero scrim curve. The shape stays the wedge, the
/// bottom ramp and the feathered corner. 1.35 is the video-bound row the legibility table grades.
///
/// The player publishes the same number as `player::preview::PREVIEW_FIELD` (its `View::field` once
/// a picture is up). `player` may not name `ui` and `ui` may not name `player`, so each owns a copy
/// and `screens::detail`'s `preview_plane_tests` pin them equal.
pub const PREVIEW_FIELD: f32 = 1.35;

/// Where a landing hero's text stands: title, meta line and (when there is one) synopsis, stacked UP
/// from [`TEXT_BOTTOM`]. The screen draws from these spans and nothing re-sums the ladder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeroStack {
    pub title: VSpan,
    pub meta: VSpan,
    /// `None` when the hero has no synopsis (it then takes no room and no gap).
    pub synopsis: Option<VSpan>,
}

/// The hero text ladder: title, [`theme::space::MD`], meta, [`theme::space::SM`], synopsis, its
/// bottom edge on [`TEXT_BOTTOM`]. `synopsis_h` is the synopsis TEXT's height, `None` for no synopsis.
pub fn hero_stack(title_h: f32, meta_h: f32, synopsis_h: Option<f32>) -> HeroStack {
    let items = [
        Item::new(title_h),
        Item::new(meta_h),
        synopsis_h.map_or(Item::absent(), |h| Item::new(h).before(theme::space::SM)),
    ];
    let stack = VStack::new(&items, theme::space::MD, Anchor::Bottom(TEXT_BOTTOM));
    let mut spans = stack.placed();
    let (Some(title), Some(meta)) = (spans.next(), spans.next()) else {
        unreachable!("the title and the meta line are always present")
    };
    HeroStack { title, meta, synopsis: spans.next() }
}

pub fn base_scrim_bottom_a(hero_a: f32) -> f32 { 0.30 + 0.64 * hero_a.clamp(0.0, 1.0) }

pub fn base_scrim_ramp(hero_a: f32) -> [f32; 4] {
    let foot = base_scrim_bottom_a(hero_a);
    [HERO_BASE_SCRIM_Y0, KNEE_Y, foot * MID_WEIGHT, foot]
}

pub fn base_scrim_a(y: f32, hero_a: f32) -> f32 {
    let foot = base_scrim_bottom_a(hero_a);
    let mid = foot * MID_WEIGHT;
    if y <= HERO_BASE_SCRIM_Y0 { 0.0 }
    else if y < KNEE_Y { mid * (y - HERO_BASE_SCRIM_Y0) / (KNEE_Y - HERO_BASE_SCRIM_Y0) }
    else { mid + (foot - mid) * ((y - KNEE_Y) / (SCR_H - KNEE_Y)).clamp(0.0, 1.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hero text stands MD under the title, SM under the meta, and its last line on
    /// `TEXT_BOTTOM`; without a synopsis the meta line takes that place and no gap is left behind.
    #[test]
    fn the_hero_text_is_one_ladder_with_an_absent_synopsis() {
        let with = hero_stack(100.0, 24.0, Some(66.0));
        assert!((with.meta.y - (with.title.y + 100.0 + theme::space::MD)).abs() < 0.01);
        let syn = with.synopsis.expect("present");
        assert!((syn.y - (with.meta.y + 24.0 + theme::space::SM)).abs() < 0.01);
        assert!((syn.y + syn.h - TEXT_BOTTOM).abs() < 0.01);
        let bare = hero_stack(100.0, 24.0, None);
        assert!(bare.synopsis.is_none());
        assert!((bare.meta.y + bare.meta.h - TEXT_BOTTOM).abs() < 0.01);
    }
}
