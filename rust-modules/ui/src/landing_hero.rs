//! Shared landing-page geometry and the atmospheric curve used by both full-bleed heroes. Keeping these
//! pure lets the renderer, route narrative and legibility tests use one definition.

use super::hstack::Item;
use super::vstack::{Anchor, VSpan, VStack};
use super::{consts::SCR_H, theme, widgets::HERO_BASE_SCRIM_Y0};

pub const TEXT_BOTTOM: f32 = 692.0;
pub const COL_W: f32 = theme::hero::TEXT_W;
pub const KNEE_Y: f32 = theme::hero::SCRIM_MID_Y;
pub const MID_WEIGHT: f32 = theme::hero::SCRIM_MID_A / theme::hero::SCRIM_FOOT_A;
/// How much a bound preview multiplies the hero scrim curve. The shape stays the wedge, the
/// bottom ramp and the feathered corner. 1.35 is the video-bound row the legibility table grades.
///
/// The player publishes the same number as `player::preview::PREVIEW_FIELD` (its `View::field` once
/// a picture is up). `player` may not name `ui` and `ui` may not name `player`, so each owns a copy
/// and `screens::detail`'s `preview_plane_tests` pin them equal.
pub const PREVIEW_FIELD: f32 = 1.35;

/// The shared ladder. Home pins its actions; Detail pins the foot of its title.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HeroAnchor {
    Actions(f32),
    TitleBottom(f32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeroStack {
    pub title: VSpan,
    pub meta: VSpan,
    pub ratings: Option<VSpan>,
    pub synopsis: Option<VSpan>,
    pub facts: VSpan,
    pub actions: VSpan,
}

/// One measured ladder for both screens. Identity and ratings form a tight group; prose and
/// facts get block air, and facts stand close to their action. Absent rows leave no phantom gap.
pub fn hero_rows(title_h: f32, meta_h: f32, ratings_h: Option<f32>, synopsis_h: Option<f32>, anchor: HeroAnchor) -> HeroStack {
    let items = [
        Item::new(title_h),
        Item::new(meta_h).before(theme::space::SM),
        ratings_h.map_or(Item::absent(), |h| Item::new(h).before(theme::space::XS)),
        synopsis_h.map_or(Item::absent(), |h| Item::new(h).before(theme::space::LG)),
        // TextView measures full leading boxes. Their trailing air belongs to the prose;
        // the proposal spends the larger block gap above it, not a second time below it.
        Item::new(theme::hero::FACTS_H).before(if synopsis_h.is_some() { theme::space::MD } else { theme::space::LG }),
        Item::new(0.0).before(theme::space::MD),
    ];
    let anchor = match anchor {
        HeroAnchor::Actions(y) => Anchor::Bottom(y),
        HeroAnchor::TitleBottom(y) => Anchor::Top(y - title_h),
    };
    let stack = VStack::new(&items, 0.0, anchor);
    let span = |i| stack.span(i).map(|(y, h)| VSpan { i, y, h, box_y: y });
    HeroStack {
        title: span(0).unwrap(), meta: span(1).unwrap(), ratings: span(2),
        synopsis: span(3), facts: span(4).unwrap(), actions: span(5).unwrap(),
    }
}

/// Home's shared ladder grows up from the same action anchor, independently of its synopsis.
pub fn hero_stack(title_h: f32, meta_h: f32, synopsis_h: Option<f32>) -> HeroStack {
    hero_rows(title_h, meta_h, None, synopsis_h, HeroAnchor::Actions(TEXT_BOTTOM + theme::space::MD))
}

pub fn base_scrim_bottom_a(hero_a: f32) -> f32 { theme::hero::SCRIM_FOOT_A * hero_a.clamp(0.0, 1.0) }

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

    #[test]
    fn the_hero_text_is_one_ladder_with_an_absent_synopsis() {
        let with = hero_stack(120.0, 34.0, Some(72.0));
        assert_eq!(with.actions.y, 716.0);
        assert_eq!(with.meta.y, 498.0);
        assert_eq!(with.synopsis.unwrap().y, 572.0);
        assert_eq!(with.facts.y, 668.0);
        let bare = hero_stack(120.0, 34.0, None);
        assert!(bare.synopsis.is_none());
        assert_eq!(bare.facts.y, with.facts.y);
        assert_eq!(bare.facts.y - (bare.meta.y + bare.meta.h), theme::space::LG);
    }

    #[test]
    fn home_and_detail_share_rows_and_differ_only_in_the_anchor() {
        let home = hero_rows(120.0, 34.0, Some(30.0), Some(72.0), HeroAnchor::Actions(716.0));
        let detail = hero_rows(120.0, 34.0, Some(30.0), Some(72.0), HeroAnchor::TitleBottom(566.0));
        let shift = detail.title.y - home.title.y;
        for (a,b) in [(home.meta,detail.meta),(home.ratings.unwrap(),detail.ratings.unwrap()),(home.synopsis.unwrap(),detail.synopsis.unwrap()),(home.facts,detail.facts),(home.actions,detail.actions)] {
            assert_eq!(b.y-a.y,shift);
        }
    }

    #[test]
    fn pdf_synopsis_padding_preserves_the_facts_and_action_anchors() {
        for ratings in [None, Some(30.0)] {
            for anchor in [HeroAnchor::Actions(716.0), HeroAnchor::TitleBottom(566.0)] {
                let rows = hero_rows(120.0, 34.0, ratings, Some(72.0), anchor);
                let previous = rows.ratings.unwrap_or(rows.meta);
                let synopsis = rows.synopsis.unwrap();
                assert_eq!(synopsis.y - (previous.y + previous.h), 40.0,
                    "the proposal gives prose a full block gap above it");
                assert_eq!(rows.facts.y - (synopsis.y + synopsis.h), 24.0,
                    "the synopsis line boxes already include trailing leading");
                assert_eq!(rows.actions.y - (rows.facts.y + rows.facts.h), 24.0);
                match anchor {
                    HeroAnchor::Actions(y) => assert_eq!(rows.actions.y, y),
                    HeroAnchor::TitleBottom(y) => {
                        assert_eq!(rows.title.y + rows.title.h, y);
                        assert_eq!(rows.facts.y, if ratings.is_some() { 790.0 } else { 752.0 });
                        assert_eq!(rows.actions.y, if ratings.is_some() { 838.0 } else { 800.0 });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[test]
fn proposal_atmosphere_has_identical_stops_on_both_heroes() {
    let origin = HERO_BASE_SCRIM_Y0;
    for strength in [0.0, 0.25, 1.0] {
        for (y, alpha) in [(origin, 0.0), (KNEE_Y, 0.55), (SCR_H, 0.86)] {
            let want = alpha * strength;
            assert!((base_scrim_a(y, strength) - want).abs() < 1e-6);
            assert!((crate::detail_layout::base_scrim_a(y, strength) - want).abs() < 1e-6);
        }
    }
    assert_eq!(base_scrim_a(0.0, 1.0), 0.0);
    assert_eq!(base_scrim_a(SCR_H + 100.0, 1.0), theme::hero::SCRIM_FOOT_A);
}

#[cfg(test)]
#[test]
fn pdf_proposal_home_and_detail_use_the_same_column() {
    assert_eq!(COL_W, crate::detail_layout::HERO_TEXT_W);
}
