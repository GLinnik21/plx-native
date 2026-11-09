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

/// Home's hero ladder (owner's board CW-13 v2): the title, ONE block under it, and the action row,
/// all growing up from [`theme::hero::HOME_ACTIONS_Y`]. Home owns its ladder because it dropped the
/// identity line, the ratings row and the facts line that [`hero_rows`] stacks; Detail keeps those
/// and its own anchor, and this function never moves it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HomeStack {
    pub title: VSpan,
    /// An episode's one-line label, or the movie's / show's synopsis (at most
    /// [`theme::hero::HOME_SYNOPSIS_LINES`] lines); `None` when the item has neither.
    pub block: Option<VSpan>,
    /// A zero-height span at the action row's top ([`theme::hero::HOME_ACTIONS_Y`]).
    pub actions: VSpan,
}

/// The height of Home's one-line episode label: the synopsis' own leading, so a one-line synopsis
/// and an episode label stand on the same line box.
pub const HOME_LABEL_H: f32 = crate::HERO_SYN_LEAD;

/// Home's ladder for a title `title_h` tall over a block `block_h` tall (`None`: no block). The
/// block's foot is [`theme::hero::HOME_ACTIONS_GAP`] above the actions and the title's foot is
/// [`theme::hero::HOME_TITLE_GAP`] above the block; an absent block leaves no phantom gap, so the
/// title then stands the 30 px above the actions itself.
pub fn home_rows(title_h: f32, block_h: Option<f32>) -> HomeStack {
    let items = [
        Item::new(title_h),
        block_h.map_or(Item::absent(), |h| Item::new(h).before(theme::hero::HOME_TITLE_GAP)),
        Item::new(0.0).before(theme::hero::HOME_ACTIONS_GAP),
    ];
    let stack = VStack::new(&items, 0.0, Anchor::Bottom(theme::hero::HOME_ACTIONS_Y));
    let span = |i| stack.span(i).map(|(y, h)| VSpan { i, y, h, box_y: y });
    HomeStack { title: span(0).unwrap(), block: span(1), actions: span(2).unwrap() }
}

/// Home's synopsis: the shared reading copy ([`crate::hero_synopsis`]'s `LABEL` 26 on a 36 px
/// leading in `TEXT_READING`) cut to [`theme::hero::HOME_SYNOPSIS_LINES`] lines. Draw it at
/// [`theme::hero::HOME_SYNOPSIS_W`] wide.
pub fn home_synopsis(summary: &str) -> crate::text_view::TextView<'_> {
    crate::hero_synopsis(summary, "").max_lines(theme::hero::HOME_SYNOPSIS_LINES)
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

    /// The owner's v2 ladder, pixel for pixel: an episode's one-line label tops out at 602 with the
    /// 120 px logo's foot at 586, a two-line synopsis at 566 with the title's foot at 550, a one-line
    /// synopsis comes down a line (and its title with it), and every block ends at 638.
    #[test]
    fn home_ladder_positions() {
        let title_h = theme::logo::HERO_BAND_H;
        let one = home_rows(title_h, Some(HOME_LABEL_H));
        assert_eq!(one.actions.y, 668.0);
        let b = one.block.unwrap();
        assert_eq!((b.y, b.y + b.h), (602.0, 638.0), "the label's line box ends 30 above the actions");
        assert_eq!(one.title.y + one.title.h, 586.0, "the logo's foot is 16 above the label");
        let two = home_rows(title_h, Some(2.0 * HOME_LABEL_H));
        let b = two.block.unwrap();
        assert_eq!((b.y, b.y + b.h), (566.0, 638.0));
        assert_eq!(two.title.y + two.title.h, 550.0);
        assert_eq!(two.title.y + two.title.h, one.title.y + one.title.h - HOME_LABEL_H, "the second line moves the title up by one line");
        let bare = home_rows(title_h, None);
        assert!(bare.block.is_none());
        assert_eq!(bare.title.y + bare.title.h, 638.0, "no block: the title stands the 30 above the actions");
    }

    /// Detail is untouched by Home's ladder: its rows and its anchors are the ones the shared test
    /// above pins, and Home's actions no longer stand where the shared ladder put them.
    #[test]
    fn detail_ladder_is_not_home_ladder() {
        let detail = hero_rows(120.0, 34.0, Some(30.0), Some(72.0), HeroAnchor::TitleBottom(566.0));
        assert_eq!((detail.title.y, detail.actions.y), (446.0, 838.0));
        assert_ne!(home_rows(120.0, Some(72.0)).actions.y, hero_stack(120.0, 34.0, Some(72.0)).actions.y);
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
