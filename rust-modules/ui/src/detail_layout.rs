//! Shared detail hero geometry, also used by the contrast and safe-area audits.
use super::{consts::{MARGIN_X, MARGIN_Y}, theme, widgets};
#[cfg(any(test, feature = "test-support"))]
use super::{consts::SCR_W, Rect};
#[cfg(test)]
use super::consts::SCR_H;

pub const HERO_TEXT_W: f32 = theme::hero::TEXT_W;
pub const TITLE_BOTTOM: f32 = 566.0;
pub const PEOPLE_W: f32 = 560.0;
pub const PEOPLE_LEAD: f32 = 32.0;
pub const PEOPLE_MAX_LINES: usize = 4;
pub const PEOPLE_INK: [f32; 4] = theme::TEXT_SECONDARY;
/// The facts row's ink, and the second time this trade has been made: at `TEXT_TERTIARY`
/// (L=0.317) the row needed α≈0.79 of scrim for 4.5:1 — an essentially black hero corner — so
/// `widgets_hero_scrim_tests` carried it at a lowered 2.6 floor and wrote the ink step down as
/// deferred. Tightening the hero rhythm is what came due: every rung the row moves up sits in
/// less of the atmospheric ramp (each segment is linear in `y`), and the two named rungs the owner asked
/// for cost 0.16 of contrast, which no scrim retune buys back cheaply — the ramp's own
/// measurement says it is 97% of the drop, and steepening it darkens the artwork the hero exists
/// to show. One ink step costs nothing on screen and takes the row to the ordinary 3.0 floor,
/// exactly as [`PEOPLE_INK`] did for the people column.
pub const FACTS_INK: [f32; 4] = theme::TEXT_SECONDARY;
pub const COMPACT_TITLE_BOT: f32 = MARGIN_Y + theme::logo::COMPACT_H_MAX;
pub const TOP_MARGIN: f32 = COMPACT_TITLE_BOT + 26.0;


/// Top-left anchor for the hero logo while a trailer plays in the background — independent of
/// scroll, unlike the pinned [`COMPACT_TITLE_BOT`] title (which answers "how far down have I
/// scrolled", not "is a trailer playing"). Sits inside the safe-area margins.
pub const PREVIEW_LOGO_X: f32 = MARGIN_X;
pub const PREVIEW_LOGO_Y: f32 = MARGIN_Y;
/// Keeps a wide wordmark from reaching toward the center of the screen once it has shrunk.
pub const PREVIEW_LOGO_MAX_W: f32 = 480.0;

#[derive(Clone, Copy)]
pub struct HeroChain {
    pub meta_y: f32,
    pub ratings_y: f32,
    pub syn_y: f32,
    pub facts_y: f32,
    pub btn_y: f32,
}

/// Shared hero ladder, with Detail anchored at the title's foot. Home uses the same row bands
/// and gaps while anchoring its actions. Ratings are optional; their absence leaves no gap.
pub fn hero_chain(
    syn_h: f32,
    has_ratings: bool,
    measure: &dyn plx_machine::machine::Measure,
) -> HeroChain {
    let _ = measure;
    let rows = super::landing_hero::hero_rows(
        theme::logo::HERO_BAND_H, widgets::BADGE_H,
        has_ratings.then_some(widgets::RATING_MARK_D), Some(syn_h.max(34.0)),
        super::landing_hero::HeroAnchor::TitleBottom(TITLE_BOTTOM),
    );
    HeroChain {
        meta_y: rows.meta.y,
        ratings_y: rows.ratings.map_or(rows.synopsis.unwrap().y, |r| r.y),
        syn_y: rows.synopsis.unwrap().y, facts_y: rows.facts.y, btn_y: rows.actions.y,
    }
}

/// Bottom of the credits' full leading boxes, aligned by the last cap band to the facts.
pub fn people_box_bottom(btn_y: f32, cap_h: f32) -> f32 {
    btn_y - theme::space::MD + PEOPLE_LEAD - (theme::hero::FACTS_H + cap_h) * 0.5
}

pub fn people_top(btn_y: f32, lines: usize, cap_h: f32) -> f32 {
    people_box_bottom(btn_y, cap_h) - lines as f32 * PEOPLE_LEAD
}

pub fn base_scrim_a(y: f32, hero_vis: f32) -> f32 {
    super::landing_hero::base_scrim_a(y, hero_vis)
}

#[cfg(any(test, feature = "test-support"))]
pub fn overscan_rects(out: &mut Vec<(&'static str, Rect)>) {
    let tall = theme::logo::COMPACT_H_MAX;
    out.push(("detail pinned compact title (tallest logo)",
        Rect::new(MARGIN_X, COMPACT_TITLE_BOT - tall, SCR_W - 2.0 * MARGIN_X, tall)));
    out.push(("detail hero text column", Rect::new(MARGIN_X, TITLE_BOTTOM - 200.0, HERO_TEXT_W, 200.0)));
    out.push(("detail people column (right edge)", Rect::new(SCR_W - MARGIN_X - PEOPLE_W, 700.0, PEOPLE_W, 100.0)));
    out.push(("detail below-hero flow, at rest", Rect::new(MARGIN_X, TOP_MARGIN, SCR_W - 2.0 * MARGIN_X, 100.0)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_proposal_detail_rows_follow_the_shared_ladder() {
        let m = crate::fixture::FixtureMeasure;
        let ch = hero_chain(72.0, true, &m);
        assert_eq!(ch.meta_y, 582.0);
        assert_eq!(ch.ratings_y, 624.0);
        assert_eq!(ch.syn_y, 694.0);
        assert_eq!(ch.facts_y, 790.0);
        assert_eq!(ch.btn_y, 838.0);
        let bare = hero_chain(72.0, false, &m);
        assert_eq!(bare.syn_y, ch.syn_y - widgets::RATING_MARK_D - theme::space::XS);
        assert_eq!(bare.btn_y, ch.btn_y - widgets::RATING_MARK_D - theme::space::XS);
    }

    #[test]
    fn hero_people_share_the_facts_cap_band_and_grow_upward() {
        let ch = hero_chain(72.0, true, &crate::fixture::FixtureMeasure);
        let minimum = hero_chain(34.0, false, &crate::fixture::FixtureMeasure);
        assert_eq!(minimum.facts_y, 714.0);
        assert_eq!(people_top(minimum.btn_y, PEOPLE_MAX_LINES, 18.0), 621.0);
        for cap_h in [16.0, 18.0, 24.0] {
            for lines in 1..=PEOPLE_MAX_LINES {
                assert_eq!(people_top(ch.btn_y, lines, cap_h) + (lines - 1) as f32 * PEOPLE_LEAD + cap_h * 0.5,
                    ch.facts_y + theme::hero::FACTS_H * 0.5,
                    "last credit cap band must share the facts centre");
            }
        }
        let facts_r = SCR_W - MARGIN_X - PEOPLE_W - theme::space::SM;
        assert!(facts_r < SCR_W - MARGIN_X - PEOPLE_W);
    }

    #[test]
    fn base_scrim_uses_the_shared_proposal_foot_and_clamps_inputs() {
        assert_eq!(base_scrim_a(0.0, 1.0), 0.0);
        assert_eq!(base_scrim_a(SCR_H, 1.0), theme::hero::SCRIM_FOOT_A);
        assert_eq!(base_scrim_a(SCR_H + 100.0, 2.0), theme::hero::SCRIM_FOOT_A);
        assert_eq!(base_scrim_a(SCR_H, -1.0), 0.0);
    }

    #[test]
    fn overscan_rects_keep_the_legacy_detail_regions() {
        let mut got = Vec::new();
        overscan_rects(&mut got);
        assert_eq!(got.len(), 4);

        let want = [
            (
                "detail pinned compact title (tallest logo)",
                Rect::new(
                    MARGIN_X,
                    COMPACT_TITLE_BOT - theme::logo::COMPACT_H_MAX,
                    SCR_W - 2.0 * MARGIN_X,
                    theme::logo::COMPACT_H_MAX,
                ),
            ),
            (
                "detail hero text column",
                Rect::new(MARGIN_X, TITLE_BOTTOM - 200.0, HERO_TEXT_W, 200.0),
            ),
            (
                "detail people column (right edge)",
                Rect::new(SCR_W - MARGIN_X - PEOPLE_W, 700.0, PEOPLE_W, 100.0),
            ),
            (
                "detail below-hero flow, at rest",
                Rect::new(MARGIN_X, TOP_MARGIN, SCR_W - 2.0 * MARGIN_X, 100.0),
            ),
        ];
        for (got, want) in got.iter().zip(want.iter()) {
            assert_eq!(got.0, want.0);
            assert_eq!((got.1.x, got.1.y, got.1.w, got.1.h), (want.1.x, want.1.y, want.1.w, want.1.h));
        }
    }
}
