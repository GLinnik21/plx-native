//! Shared detail hero geometry, also used by the contrast and safe-area audits.
use super::{consts::{MARGIN_X, MARGIN_Y, SCR_H}, theme, widgets};
#[cfg(test)]
use super::{consts::SCR_W, Rect};

pub(crate) const HERO_TEXT_W: f32 = 943.0;
pub(crate) const TITLE_BOTTOM: f32 = 566.0;
pub(crate) const PEOPLE_W: f32 = 560.0;
pub(crate) const PEOPLE_LEAD: f32 = 32.0;
pub(crate) const PEOPLE_MAX_LINES: usize = 4;
pub(crate) const PEOPLE_INK: [f32; 4] = theme::TEXT_SECONDARY;
pub(crate) const COMPACT_TITLE_BOT: f32 = MARGIN_Y + theme::logo::COMPACT_H_MAX;
pub(crate) const TOP_MARGIN: f32 = COMPACT_TITLE_BOT + 26.0;

/// Top-left anchor for the hero logo while a trailer plays in the background — independent of
/// scroll, unlike the pinned [`COMPACT_TITLE_BOT`] title (which answers "how far down have I
/// scrolled", not "is a trailer playing"). Sits inside the safe-area margins.
pub(crate) const PREVIEW_LOGO_X: f32 = MARGIN_X;
pub(crate) const PREVIEW_LOGO_Y: f32 = MARGIN_Y;
/// Keeps a wide wordmark from reaching toward the center of the screen once it has shrunk.
pub(crate) const PREVIEW_LOGO_MAX_W: f32 = 480.0;

#[derive(Clone, Copy)]
pub(crate) struct HeroChain {
    pub(crate) meta_y: f32,
    pub(crate) ratings_y: f32,
    pub(crate) syn_y: f32,
    pub(crate) facts_y: f32,
    pub(crate) btn_y: f32,
}

/// Every row below the title is placed the same way: the previous row's own MEASURED bottom edge
/// plus one named rung from [`theme::space`] — never an absolute offset picked to match a mock.
/// "Measured" means the row's real bounding box, which for the identity and ratings lines is
/// their badge/mark (`widgets::BADGE_H`, `widgets::RATING_MARK_D` — both centred on, and taller
/// than, the caption text beside them), and for the facts line is the `CAPTION` cap band its own
/// glyphs match. An item with a taller or shorter line at any step still lands the next one a
/// clean gap below it, and the gaps read the same whether or not the trailer preview has faded a
/// label's alpha (this function never looks at preview state — see `compute_hero_chain`'s caller,
/// which only ever asks "does the item HAVE this content", never "is it currently visible").
pub(crate) fn hero_chain(
    syn_h: f32,
    has_ratings: bool,
    measure: &dyn crate::ui::machine::Measure,
) -> HeroChain {
    let meta_y = TITLE_BOTTOM + theme::space::MD;
    // The identity line's own bounding box is its BADGE, not its caption: `draw_identity_line`
    // centres the resolution/HDR/audio badges on the text's cap band, and at `BADGE_H` (34) they
    // stand taller than the `BODY` caption (cap_h ~21) that shares their row.
    let meta_h = widgets::BADGE_H;
    let ratings_y = meta_y + meta_h + theme::space::SM;
    // Same reasoning: the rating marks (`RATING_MARK_D`, 30) are centred on the caption's cap band
    // and are the row's tallest element, not the `LABEL` text (cap_h ~19.5) beside them.
    let ratings_h = widgets::RATING_MARK_D;
    let syn_y = (if has_ratings { ratings_y + ratings_h } else { meta_y + meta_h }) + theme::space::MD;
    let facts_y = syn_y + syn_h.max(34.0) + theme::space::MD;
    // The facts row's own icons (`FACTS_GLYPH_D`/capsule) are sized to the `CAPTION` text itself,
    // unlike the two rows above, so its measured height is the plain cap band — read through the
    // `Measure` seam (`check-deps.sh`'s "textmeasure" gate: raw `crate::text::cap_h` is not
    // reachable outside `text.rs`/`ui/text_view.rs`/`ui/text_buffer.rs`/a `Measure` impl body),
    // the same capability `compute_hero_chain`'s caller already threads down for the synopsis.
    let facts_h = measure.cap_h(theme::size::CAPTION);
    let btn_y = facts_y + facts_h + theme::space::MD;
    HeroChain { meta_y, ratings_y, syn_y, facts_y, btn_y }
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
    use crate::ui::machine::Measure;

    /// The facts row and the people column are LEVEL, so the thing that keeps them apart is a
    /// width bound and nothing else.
    #[test]
    fn the_facts_row_stops_short_of_the_people_column() {
        let facts_r = SCR_W - MARGIN_X - PEOPLE_W - theme::space::SM;
        assert!(
            facts_r < SCR_W - MARGIN_X - PEOPLE_W,
            "the facts row's bound must sit LEFT of the column it is bounded against"
        );
        let ch = hero_chain(76.0, true, &crate::ui::fixture::FixtureMeasure);
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

    /// The crew credit takes no vertical space: the action row hangs off the facts line's own
    /// measured height by a flat [`theme::space::MD`], whether or not PMS sent a Director[] credit
    /// (the credit is drawn as one more `Bit` inside the same `CAPTION`-sized facts row, so it
    /// never changes that row's height).
    #[test]
    fn the_crew_credit_costs_the_chain_no_vertical_space() {
        let m = crate::ui::fixture::FixtureMeasure;
        let facts_h = m.cap_h(theme::size::CAPTION);
        for syn_h in [0.0_f32, 108.0] {
            for ratings in [false, true] {
                let ch = hero_chain(syn_h, ratings, &m);
                assert_eq!(
                    ch.btn_y,
                    ch.facts_y + facts_h + theme::space::MD,
                    "the action row hangs off the facts line's measured height, credit or no credit"
                );
            }
        }
    }

    /// The ratings row is the chain's conditional band: it moves everything below it and nothing
    /// above it, by exactly its own measured height plus the ratings→synopsis rung.
    #[test]
    fn the_ratings_band_is_reserved_when_there_are_scores_and_never_otherwise() {
        let m = crate::ui::fixture::FixtureMeasure;
        let meta_h = widgets::BADGE_H;
        let ratings_h = widgets::RATING_MARK_D;
        for syn_h in [0.0_f32, 108.0] {
            let none = hero_chain(syn_h, false, &m);
            let some = hero_chain(syn_h, true, &m);

            assert_eq!(some.meta_y, none.meta_y);
            assert_eq!(none.syn_y, none.meta_y + meta_h + theme::space::MD);
            assert_eq!(some.syn_y, some.ratings_y + ratings_h + theme::space::MD);
            let shift = some.syn_y - none.syn_y;
            assert_eq!(shift, ratings_h + theme::space::SM);
            assert_eq!(some.facts_y - none.facts_y, shift);
            assert_eq!(some.btn_y - none.btn_y, shift);
        }
    }

    /// Each rung in the hero chain is a gap between two MEASURED bounding boxes — the previous
    /// row's own cap-top→baseline height plus a named [`theme::space`] rung — never a difference of
    /// absolute Y coordinates copied off a mock. This is the systematic form the owner asked for
    /// in place of the old `+30`/`+50`/`+54` literals, which happened to reproduce one mock's ys
    /// for one synopsis length and nothing else.
    #[test]
    fn every_hero_row_sits_a_named_rung_below_the_previous_rows_measured_bottom() {
        let syn_h = 72.0_f32;
        let m = crate::ui::fixture::FixtureMeasure;
        let meta_h = widgets::BADGE_H;
        let ratings_h = widgets::RATING_MARK_D;
        let facts_h = m.cap_h(theme::size::CAPTION);

        let some = hero_chain(syn_h, true, &m);
        assert_eq!(some.meta_y - TITLE_BOTTOM, theme::space::MD, "logo/title -> meta");
        assert_eq!(some.ratings_y - (some.meta_y + meta_h), theme::space::SM, "meta -> ratings");
        assert_eq!(some.syn_y - (some.ratings_y + ratings_h), theme::space::MD, "ratings -> synopsis");
        assert_eq!(some.facts_y - (some.syn_y + syn_h.max(34.0)), theme::space::MD, "synopsis -> facts");
        assert_eq!(some.btn_y - (some.facts_y + facts_h), theme::space::MD, "facts -> action row");

        // No ratings: the synopsis follows the meta line directly, off ITS measured bottom.
        let none = hero_chain(syn_h, false, &m);
        assert_eq!(none.syn_y - (none.meta_y + meta_h), theme::space::MD, "meta -> synopsis (no ratings)");
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
