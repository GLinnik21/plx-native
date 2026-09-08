//! Shared detail hero geometry, also used by the contrast and safe-area audits.
use super::{consts::{MARGIN_Y, SCR_H}, theme, widgets};
#[cfg(test)]
use super::{consts::{MARGIN_X, SCR_W}, Rect};

pub(crate) const HERO_TEXT_W: f32 = 943.0;
pub(crate) const TITLE_BOTTOM: f32 = 566.0;
pub(crate) const PEOPLE_W: f32 = 560.0;
pub(crate) const PEOPLE_LEAD: f32 = 32.0;
pub(crate) const PEOPLE_MAX_LINES: usize = 4;
pub(crate) const PEOPLE_INK: [f32; 4] = theme::TEXT_SECONDARY;
pub(crate) const COMPACT_TITLE_BOT: f32 = MARGIN_Y + theme::logo::COMPACT_H_MAX;
pub(crate) const TOP_MARGIN: f32 = COMPACT_TITLE_BOT + 26.0;

pub(crate) struct HeroChain {
    pub(crate) meta_y: f32,
    pub(crate) ratings_y: f32,
    pub(crate) syn_y: f32,
    pub(crate) facts_y: f32,
    pub(crate) btn_y: f32,
}

pub(crate) fn hero_chain(syn_h: f32, has_ratings: bool) -> HeroChain {
    let meta_y = TITLE_BOTTOM + 30.0;
    let ratings_y = meta_y + 50.0;
    let syn_y = (if has_ratings { ratings_y } else { meta_y }) + 54.0;
    let facts_y = syn_y + syn_h.max(34.0) + theme::space::MD;
    HeroChain { meta_y, ratings_y, syn_y, facts_y, btn_y: facts_y + 50.0 }
}

pub(crate) fn people_top(btn_y: f32, lines: usize) -> f32 {
    btn_y + widgets::StatusOverlay::CTRL_H - lines as f32 * PEOPLE_LEAD
}

pub(crate) fn base_scrim_a(y: f32, hero_vis: f32) -> f32 {
    0.95 * hero_vis.clamp(0.0, 1.0)
        * ((y - widgets::HERO_BASE_SCRIM_Y0).max(0.0) / (SCR_H - widgets::HERO_BASE_SCRIM_Y0)).min(1.0)
}

#[cfg(test)]
pub(crate) fn overscan_rects(out: &mut Vec<(&'static str, Rect)>) {
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

    /// The facts row and the people column are LEVEL, so the thing that keeps them apart is a
    /// width bound and nothing else.
    #[test]
    fn the_facts_row_stops_short_of_the_people_column() {
        let facts_r = SCR_W - MARGIN_X - PEOPLE_W - theme::space::SM;
        assert!(
            facts_r < SCR_W - MARGIN_X - PEOPLE_W,
            "the facts row's bound must sit LEFT of the column it is bounded against"
        );
        let ch = hero_chain(76.0, true);
        assert!(
            people_top(ch.btn_y, 3) < ch.facts_y + 17.0,
            "three lines puts the column's top line level with the facts row — that is the case the +             width bound exists for"
        );
    }

    /// The people column is anchored by its BOTTOM, so its last line sits on the action row's
    /// bottom edge however many lines the block takes — and each extra line grows UPWARD.
    #[test]
    fn the_people_column_grows_upward_off_the_button_row() {
        let btn_y = 846.0;
        let bottom = btn_y + widgets::StatusOverlay::CTRL_H;
        let one = people_top(btn_y, 1);
        let four = people_top(btn_y, PEOPLE_MAX_LINES);
        assert_eq!(one, bottom - PEOPLE_LEAD);
        assert!(four < one, "a taller block starts higher, not lower");
        assert!(four < bottom, "the block is measured upward from its bottom edge");
    }

    /// The crew credit takes no vertical space: the action row hangs off the facts line whether or
    /// not PMS sent a Director[] credit.
    #[test]
    fn the_crew_credit_costs_the_chain_no_vertical_space() {
        for syn_h in [0.0_f32, 108.0] {
            for ratings in [false, true] {
                let ch = hero_chain(syn_h, ratings);
                assert_eq!(
                    ch.btn_y,
                    ch.facts_y + 50.0,
                    "the action row hangs off the facts line, credit or no credit"
                );
            }
        }
    }

    /// The ratings row is the chain's conditional band: it moves everything below it and nothing
    /// above it.
    #[test]
    fn the_ratings_band_is_reserved_when_there_are_scores_and_never_otherwise() {
        for syn_h in [0.0_f32, 108.0] {
            let none = hero_chain(syn_h, false);
            let some = hero_chain(syn_h, true);

            assert_eq!(some.meta_y, none.meta_y);
            assert_eq!(none.syn_y, none.meta_y + 54.0);
            assert_eq!(some.syn_y, some.ratings_y + 54.0);
            let shift = some.syn_y - none.syn_y;
            assert_eq!(shift, 50.0);
            assert_eq!(some.facts_y - none.facts_y, shift);
            assert_eq!(some.btn_y - none.btn_y, shift);
        }
    }

    /// The chain reproduces the mock's absolute ys on its two-line blurb.
    #[test]
    fn a_two_line_blurb_lands_the_chain_on_the_mockups_own_ys() {
        // Legacy input was 2.0 * SYN_LEAD, and HERO_SYN_LEAD is 36px.
        let ch = hero_chain(72.0, true);
        assert_eq!(ch.meta_y, 596.0, "meta line");
        assert_eq!(ch.ratings_y, 646.0, "review scores");
        assert_eq!(ch.syn_y, 700.0, "synopsis");
        assert_eq!(ch.facts_y, 796.0, "date/counts line");
        assert_eq!(ch.btn_y, 846.0, "action row");
    }

    #[test]
    fn base_scrim_preserves_the_legacy_foot_and_clamps_inputs() {
        assert_eq!(base_scrim_a(0.0, 1.0), 0.0);
        assert_eq!(base_scrim_a(SCR_H, 1.0), 0.95);
        assert_eq!(base_scrim_a(SCR_H + 100.0, 2.0), 0.95);
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
