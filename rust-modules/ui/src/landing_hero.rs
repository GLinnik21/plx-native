//! Shared geometry and atmospheric curve for a bottom-anchored landing-page hero. Keeping these
//! pure lets the renderer, route narrative and legibility tests use one definition.

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

pub fn stack_top(title_h: f32, meta_h: f32, synopsis_h: f32) -> f32 {
    TEXT_BOTTOM - (title_h + theme::space::MD + meta_h + synopsis_h)
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
