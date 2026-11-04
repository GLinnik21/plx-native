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
//! **Shared contract.** These began as the bodies of `screens::detail::hero` and were
//! re-based on the shared `HStack`. Both heroes now share the proposal's minimum Play width;
//! the Detail page also uses the dynamic row geometry
//! (`hero_layout_tests.rs` pins the arithmetic on its own).
use std::ffi::CStr;

use plx_data::metadata::{Detail, Episode};
use plx_machine::machine::Measure;
use plx_ui::hstack::{Align, HStack, Item};
use plx_ui::widgets::CircleButton;
use plx_ui::{theme, Rect};

/// Maximum row: Play, bookmark, Go to Show, Version, Restart, Trailer, Alt, watch face and More.
pub const MAX_CTLS: usize = 9;

/// How many of the row's controls are discs that unfurl to a verb on focus: Restart, Trailer, the
/// watch toggle, Play Version and Go to Show (the bookmark disc never unfurls).
pub const DISCS: usize = 5;

/// The Play/Resume pill's minimum width: a pathologically short label still gets a pill.
pub const PW: f32 = theme::hero::PLAY_MIN_W;

/// The row's inter-control air and its disc diameter: the SHARED control-family numbers
/// (`ui::widgets::CONTROL_GAP` / `StatusOverlay::CTRL_H`), not a second copy of them.
pub const CGAP: f32 = plx_ui::widgets::CONTROL_GAP;
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
pub fn pill_w(measure: &dyn Measure, label: &CStr, sz: i32, icon: bool, trailing: Option<plx_ui::icons::Icon>) -> f32 {
    // One run: the leading icon box, the label, the trailing icon's INK (its bearing is not air), `HERO_ICON_GAP` apart. An
    // icon the pill does not carry is ABSENT, so it leaves neither its width nor its gap.
    let box_w = sz as f32 * HERO_ICON_RATIO;
    let run = [
        if icon { Item::new(box_w) } else { Item::absent() },
        Item::new(measure.width(label, sz, true)),
        trailing.map_or_else(Item::absent, |acc| Item::glyph(acc, box_w)),
    ];
    HStack::new(&run, HERO_ICON_GAP, Align::Leading(0.0)).width() + plx_ui::widgets::BTN_PILL_AIR
}

/// **The one verb for the Play control: "Resume" when the title has a resume point, else "Play".**
/// The Home hero's pill, the Detail hero's pill and the card menu's first row all say it through
/// this function (and [`play_text`]), so one resume point can never be called "Continue" on one
/// page and "Resume" on the next.
pub fn play_label(resumes: bool) -> &'static CStr {
    if resumes {
        plx_platform::i18n::msg::browse_detail_resume_c()
    } else {
        plx_platform::i18n::msg::browse_detail_play_c()
    }
}

/// Play/Resume geometry for both heroes. Labels grow naturally beyond the design minimum.
pub fn play_width(measure: &dyn Measure, label: &CStr) -> f32 {
    pill_w(measure, label, theme::size::BODY, true, None).max(PW)
}

/// [`play_label`] as text, for the rows that take a `&str`.
pub fn play_text(resumes: bool) -> &'static str {
    if resumes {
        plx_platform::i18n::msg::browse_detail_resume()
    } else {
        plx_platform::i18n::msg::browse_detail_play()
    }
}

/// Every measured width the Detail row's accumulation needs, as one value.
#[derive(Clone, Copy, Debug)]
pub struct HeroWidths {
    pub pill: f32,
    pub alt: f32,
    /// the unfurling discs, in the page's disc slot order, each already unfurled
    pub disc: [f32; DISCS],
}

/// The accumulation itself, PURE: the drawn frame of control `i` in a row whose controls, in drawn
/// order, are `widths` wide, placed by the shared
/// [`HStack`]. Every control is PRESENT whatever its width: a disc springing between `CD` and its
/// open width never passes through 0 and so never pops its gap. Capsules reserve their focus
/// expansion on each side, keeping `CGAP` clear when focused. Plain discs retain their ordinary
/// gap; the first control keeps its base at the page margin. An index past the end lands one gap
/// after the reserved right edge of the last control.
///
/// `widths` is never empty from the Detail row (`hero_ctls` always yields Play and a watch face).
/// The Detail-only fallback the old inline loop had for an empty row (`cw.pill`) is therefore
/// unreachable and was not carried over: an empty slice yields a zero-width control at the margin.
pub fn row_rect(widths: &[f32], i: usize, y: f32) -> Rect {
    debug_assert!(widths.len() <= MAX_CTLS);
    let n = widths.len().min(MAX_CTLS);
    // A dense translated row may already spend nearly all the room before People. Give up
    // decorative air before letting the existing controls cross that boundary.
    let base_w = widths[..n].iter().sum::<f32>() + CGAP * n.saturating_sub(1) as f32;
    let air_w = widths[..n].iter().enumerate()
        .map(|(k, w)| capsule_focus_air(*w) * if k == 0 { 1.0 } else { 2.0 }).sum::<f32>();
    let room = (FACTS_R - plx_ui::consts::MARGIN_X - base_w).max(0.0);
    let air_scale = if air_w > 0.0 { (room / air_w).min(1.0) } else { 1.0 };
    // Fixed storage: this runs ~10-16 times a frame on Detail, so it allocates nothing. One extra,
    // zero-wide item (the sentinel) stands in for "past the end": the stack puts it one gap after
    // the last control, exactly where the old accumulation left `x`.
    let mut items = [Item::absent(); MAX_CTLS + 1];
    for (k, (slot, w)) in items.iter_mut().zip(&widths[..n]).enumerate() {
        let air = capsule_focus_air(*w) * air_scale;
        *slot = Item::new(*w + air + if k == 0 { 0.0 } else { air });
    }
    items[n] = Item::new(0.0);
    let stack = HStack::new(&items[..=n], CGAP, Align::Leading(plx_ui::consts::MARGIN_X));
    let at = i.min(n);
    let (x, _) = stack.span(at).unwrap_or((plx_ui::consts::MARGIN_X, 0.0));
    let w = widths.get(i).or(widths.last()).copied().unwrap_or(0.0);
    let leading_air = if at == 0 || at == n { 0.0 } else { capsule_focus_air(w) * air_scale };
    Rect::new(x + leading_air, y, w, CD)
}

/// Reserved right edge, including a capsule's focus expansion, for the unfurl budget.
pub fn row_right(widths: &[f32]) -> f32 {
    row_rect(widths, widths.len(), 0.0).x - if widths.is_empty() { 0.0 } else { CGAP }
}

/// Reserve the capsule's horizontal focus pop, opening continuously from a plain disc.
/// The excess-width bound avoids a jump on the first frame of the unfurl spring.
fn capsule_focus_air(w: f32) -> f32 {
    (w * (plx_ui::widgets::CTRL_FOCUS_SCALE - 1.0) * 0.5).min((w - CD).max(0.0))
}

/// Every disc's drawn width right now. `last_right(disc)` is the right edge of the row's LAST
/// control when the `N` discs are `disc` wide; `label_w` is each disc's measured verb and
/// `unfurl` its 0..1 open fraction.
///
/// Every active verb must fit wholly before the People column as a single extra, and the last
/// control of the trial layout must still sit at or before [`FACTS_R`], otherwise every disc
/// remains a circle. Every disc can be mid-unfurl at once (one opening while another closes), so a
/// pair-wide budget is not enough: two extras that each fit can still push a third across the
/// People column.
pub fn disc_caps_with<const N: usize>(
    unfurl: [f32; N],
    label_w: [f32; N],
    last_right: impl Fn([f32; N]) -> f32,
) -> [f32; N] {
    let budget = CircleButton::label_budget(CD, FACTS_R - last_right([CD; N]));
    if label_w.iter().zip(unfurl).any(|(label, open)| open > 0.0 && *label > budget) {
        return [CD; N];
    }
    let open = std::array::from_fn(|i| CircleButton::cap_w(CD, unfurl[i], label_w[i]));
    if last_right(open) > FACTS_R + 0.01 {
        return [CD; N];
    }
    open
}

/// The same episode lead on Home and Detail, including its prose separator.
pub fn episode_lead(season: i64, index: i64, title: &str) -> String {
    let kicker = plx_ui::fmt::episode_kicker(season, index, title);
    if kicker.is_empty() { kicker } else { format!("{kicker}: ") }
}

/// The date and duration shared by Home and Detail. A valid resume point replaces total duration.
pub fn time_facts(year: i64, aired: &str, episode: bool, dur_ms: i64, resume_ms: i64) -> (String, Option<String>) {
    let date = if episode { plx_ui::fmt::hero_date(aired, year) }
        else if year > 0 { year.to_string() } else { String::new() };
    let resumes = plx_data::metadata::resume_ns(resume_ms, dur_ms) > 0;
    let length = (dur_ms >= 60_000).then(|| {
        if resumes {
            let left = dur_ms.saturating_sub(resume_ms);
            if left < 3_600_000 { plx_platform::i18n::msg::browse_detail_minutes_left((left / 60_000).max(1)) }
            else { plx_ui::fmt::time_left(left) }
        } else { plx_ui::fmt::duration(dur_ms) }
    });
    (date, length)
}

pub fn listing_facts(item: &plx_data::pms::PmsMovie) -> (String, Option<String>) {
    time_facts(i64::from(item.year), &item.aired, item.kind == 3, item.dur_ns / 1_000_000, item.resume_ms)
}

/// An on-deck episode supplies its own date and runtime; an unstarted show keeps its extent.
pub fn meta_facts(d: &Detail, episode: Option<&Episode>) -> (String, Option<String>) {
    if let Some(ep) = episode {
        return time_facts(0, &ep.aired, true, ep.dur_ms, ep.resume_ms);
    }
    let facts = time_facts(d.year, &d.aired, d.kind == "episode", d.dur_ms, d.resume_ms);
    if !d.is_show || d.seasons.is_empty() { return facts; }
    let episodes: i64 = d.seasons.iter().map(|season| season.leaf_count).sum();
    let seasons = plx_platform::i18n::msg::browse_detail_seasons(d.seasons.len() as i64);
    let extent = if episodes > 0 {
        plx_platform::i18n::msg::browse_detail_extent(&plx_platform::i18n::msg::browse_detail_episodes(episodes), &seasons)
    } else { seasons };
    (facts.0, Some(extent))
}

#[cfg(test)]
#[path = "hero_layout_tests.rs"]
mod tests;
