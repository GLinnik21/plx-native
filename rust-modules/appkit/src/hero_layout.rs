//! The hero's shared geometry and meta-line helpers: the numbers and the pure arithmetic a
//! title's hero row (the Detail page's action row today, the catalog page's tomorrow) shares, so a
//! second page lays its row out with the SAME constants instead of a copy.
//!
//! **What lives here.** Only decisions that name no screen and no control enum: the row's
//! constants ([`CGAP`], [`CD`], [`PW`], [`FACTS_R`]), a pill's measured width ([`pill_w`]), the
//! left-to-right accumulation of a row of known widths ([`row_rect`]), the disc-unfurl budget
//! ([`disc_caps_with`]) and the date/runtime-or-extent line ([`meta_facts`]). Which controls a
//! given page shows (`HeroCtl`, `HeroSet`) stays with that page and feeds these functions its
//! widths.
//!
//! **Behaviour-neutral.** These bodies moved out of `screens::detail::hero` unchanged; the
//! Detail page calls them back, so its pixels and its tests are the contract
//! (`hero_layout_tests.rs` pins the arithmetic on its own).
use std::ffi::CStr;

use plx_data::metadata::{Detail, Episode};
use plx_machine::machine::Measure;
use plx_ui::widgets::CircleButton;
use plx_ui::{theme, Rect};

/// The Play/Resume pill's minimum width: a pathologically short label still gets a pill.
pub const PW: f32 = 168.0;

/// The row's inter-control air and its disc diameter: the SHARED control-family numbers
/// (`ui::widgets::CTRL_GAP` / `StatusOverlay::CTRL_H`), not a second copy of them.
pub const CGAP: f32 = plx_ui::widgets::CTRL_GAP;
pub const CD: f32 = plx_ui::widgets::StatusOverlay::CTRL_H;

/// The right edge the hero's controls and facts line may reach: the People column's left edge less
/// one small gap.
pub const FACTS_R: f32 = plx_ui::consts::SCR_W
    - plx_ui::consts::MARGIN_X
    - plx_ui::detail_layout::PEOPLE_W
    - theme::space::SM;

/// Duplicated from `widgets::Button`'s own PRIVATE `BTN_ICON_RATIO`/`BTN_ICON_GAP` (1.15, 12.0):
/// those two constants are `widgets.rs`-internal and this layer names only `ui/`'s public surface.
/// `BTN_PILL_AIR` IS `pub` and is reused directly.
///
/// A drift here is COSMETIC, not a hit-test miss: the width this formula computes feeds BOTH a
/// page's `place` and its `draw`, so the two can disagree with the REAL widget's own preferred
/// width, never with EACH OTHER.
const HERO_ICON_RATIO: f32 = 1.15;
const HERO_ICON_GAP: f32 = 12.0;

/// `widgets::Button::pill_w_full`, reproduced over [`Measure`] rather than `plx_gfx::text`
/// directly (host-test-safe geometry shared by `place` and `draw`).
pub fn pill_w(measure: &dyn Measure, label: &CStr, sz: i32, icon: bool, trailing: bool) -> f32 {
    let (isz, gap) = if icon {
        (sz as f32 * HERO_ICON_RATIO, HERO_ICON_GAP)
    } else {
        (0.0, 0.0)
    };
    let (tsz, tgap) = if trailing {
        (sz as f32 * HERO_ICON_RATIO, HERO_ICON_GAP)
    } else {
        (0.0, 0.0)
    };
    isz + gap + measure.width(label, sz, true) + tgap + tsz + plx_ui::widgets::BTN_PILL_AIR
}

/// Every measured width the Detail row's accumulation needs, as one value.
#[derive(Clone, Copy, Debug)]
pub struct HeroWidths {
    pub pill: f32,
    pub alt: f32,
    /// the three discs, in the page's disc slot order, each already unfurled
    pub disc: [f32; 3],
}

/// The accumulation itself, PURE: the drawn frame of control `i` in a row whose controls, in drawn
/// order, are `widths` wide (`CGAP` apart, from the page margin). An index past the end lands one
/// gap after the last control, as the Detail row always has.
///
/// `widths` is never empty from the Detail row (`hero_ctls` always yields Play and a watch face).
/// The Detail-only fallback the old inline loop had for an empty row (`cw.pill`) is therefore
/// unreachable and was not carried over: an empty slice yields a zero-width control at the margin.
pub fn row_rect(widths: &[f32], i: usize, y: f32) -> Rect {
    let mut x = plx_ui::consts::MARGIN_X;
    let mut w = widths.first().copied().unwrap_or(0.0);
    for (k, &cw) in widths.iter().enumerate() {
        w = cw;
        if k >= i {
            break;
        }
        x += w + CGAP;
    }
    Rect::new(x, y, w, CD)
}

/// Every disc's drawn width right now. `last_right(disc)` is the right edge of the row's LAST
/// control when the three discs are `disc` wide; `label_w` is each disc's measured verb and
/// `unfurl` its 0..1 open fraction.
///
/// The widest verb must fit wholly before the People column as a single extra, and the last
/// control of the trial layout must still sit at or before [`FACTS_R`], otherwise every disc
/// remains a circle. Three discs can unfurl at once, so a pair-wide budget is not enough: two
/// extras that each fit can still push the third across the People column.
pub fn disc_caps_with(
    unfurl: [f32; 3],
    label_w: [f32; 3],
    last_right: impl Fn([f32; 3]) -> f32,
) -> [f32; 3] {
    let budget = CircleButton::label_budget(CD, FACTS_R - last_right([CD; 3]));
    if label_w.iter().copied().fold(0.0f32, f32::max) > budget {
        return [CD; 3];
    }
    let open = [
        CircleButton::cap_w(CD, unfurl[0], label_w[0]),
        CircleButton::cap_w(CD, unfurl[1], label_w[1]),
        CircleButton::cap_w(CD, unfurl[2], label_w[2]),
    ];
    if last_right(open) > FACTS_R + 0.01 {
        return [CD; 3];
    }
    open
}

/// The hero's meta line, as `(date, runtime or extent)`. `episode` is the on-deck episode the hero
/// is showing, if any: it then supplies the aired date and its own runtime; otherwise the item's
/// own date, and a show's seasons/episodes extent or a title's runtime.
pub fn meta_facts(d: &Detail, episode: Option<&Episode>) -> (String, Option<String>) {
    if let Some(ep) = episode {
        let runtime = (ep.dur_ms >= 60_000).then(|| plx_ui::fmt::dur_long(ep.dur_ms));
        return (plx_ui::fmt::pretty_date(&ep.aired, 0), runtime);
    }
    let date = plx_ui::fmt::pretty_date(&d.aired, d.year);
    if d.is_show {
        let seasons = d.seasons.len();
        if seasons == 0 {
            return (date, None);
        }
        let episodes: i64 = d.seasons.iter().map(|season| season.leaf_count).sum();
        let seasons = plx_platform::i18n::msg::browse_detail_seasons(seasons as i64);
        let extent = if episodes > 0 {
            plx_platform::i18n::msg::browse_detail_extent(
                &plx_platform::i18n::msg::browse_detail_episodes(episodes),
                &seasons,
            )
        } else {
            seasons
        };
        return (date, Some(extent));
    }
    (
        date,
        (d.dur_ms >= 60_000).then(|| plx_ui::fmt::dur_long(d.dur_ms)),
    )
}

#[cfg(test)]
#[path = "hero_layout_tests.rs"]
mod tests;
