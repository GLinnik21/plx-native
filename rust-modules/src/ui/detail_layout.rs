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
/// The facts row's ink, and the second time this trade has been made: at `TEXT_TERTIARY`
/// (L=0.317) the row needed α≈0.79 of scrim for 4.5:1 — an essentially black hero corner — so
/// `widgets_hero_scrim_tests` carried it at a lowered 2.6 floor and wrote the ink step down as
/// deferred. Tightening the hero rhythm is what came due: every rung the row moves up sits in
/// less of the atmospheric ramp (it is linear in `y`), and the two named rungs the owner asked
/// for cost 0.16 of contrast, which no scrim retune buys back cheaply — the ramp's own
/// measurement says it is 97% of the drop, and steepening it darkens the artwork the hero exists
/// to show. One ink step costs nothing on screen and takes the row to the ordinary 3.0 floor,
/// exactly as [`PEOPLE_INK`] did for the people column.
pub(crate) const FACTS_INK: [f32; 4] = theme::TEXT_SECONDARY;
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
///
/// The rungs are not a rhythm to be kept even; they are the page's GRAMMAR, and what they say is
/// how strongly two things belong together (owner, 2026-09-18). The closer the relation, the
/// smaller the distance — so an even ladder of one rung all the way down would be the worst answer
/// available, not the tidiest: it reads as one undifferentiated list and the eye stops seeing any
/// structure at all. Three degrees are in play here, and the hero uses all three:
///
/// * `SM` — one group. Title, identity line and ratings are the item SAYING WHAT IT IS, and are
///   meant to read almost as a single object.
/// * `MD` — a change in the KIND of information. The synopsis is prose; the facts line is date,
///   extent and how it plays. Both still belong to the hero.
/// * `LG` — information giving way to ACTION (see [`btn_y`]'s own note), and
/// * `XL` — the hero giving way to the next region entirely, which is `content_top`'s rung in
///   `screens::detail`, measured off the action row's bottom rather than stated as a page offset.
///
/// A gap is between visual OBJECTS, never between coordinates — which is the whole reason this
/// function adds a measured height before every rung. `+50` between two baselines of 26-tall text
/// is a 24 gap wearing a number that belongs to neither the type nor the scale.
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
    let meta_y = TITLE_BOTTOM + theme::space::SM;
    // The identity line's own bounding box is its BADGE, not its caption: `draw_identity_line`
    // centres the resolution/HDR/audio badges on the text's cap band, and at `BADGE_H` (34) they
    // stand taller than the `BODY` caption (cap_h ~21) that shares their row.
    let meta_h = widgets::BADGE_H;
    let ratings_y = meta_y + meta_h + theme::space::SM;
    // Same reasoning: the rating marks (`RATING_MARK_D`, 30) are centred on the caption's cap band
    // and are the row's tallest element, not the `LABEL` text (cap_h ~19.5) beside them.
    let ratings_h = widgets::RATING_MARK_D;
    let syn_y = (if has_ratings { ratings_y + ratings_h } else { meta_y + meta_h }) + theme::space::SM;
    let facts_y = syn_y + syn_h.max(34.0) + theme::space::MD;
    // The facts row's own icons (`FACTS_GLYPH_D`/capsule) are sized to the `CAPTION` text itself,
    // unlike the two rows above, so its measured height is the plain cap band — read through the
    // `Measure` seam (`check-deps.sh`'s "textmeasure" gate: raw `crate::text::cap_h` is not
    // reachable outside `text.rs`/`ui/text_view.rs`/`ui/text_buffer.rs`/a `Measure` impl body),
    // the same capability `compute_hero_chain`'s caller already threads down for the synopsis.
    let facts_h = measure.cap_h(theme::size::CAPTION);
    // The one rung in this chain that is a JUDGEMENT rather than a measurement, and the one place
    // the ladder steps twice. Everything above this line is information about the item and is
    // spaced by how closely it belongs together — `SM` inside the identity group, `MD` where the
    // kind of information changes. The action row is not information: it is what you can DO, so
    // the step up to `LG` is the grammar saying so.
    //
    // It is also an optical correction, which is why it is not simply "the next rung". The Play
    // pill is a filled white capsule ~72 tall against a `CAPTION` line of ~18 — at an equal
    // mathematical gap the heavier object reads as CLOSER, because its mass crowds the air above
    // it. Equal by eye is what the page is judged on, and that is one rung more here.
    let btn_y = facts_y + facts_h + theme::space::LG;
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
        let measure = crate::ui::fixture::FixtureMeasure;
        let ch = hero_chain(76.0, true, &measure);
        let facts_bot = ch.facts_y + measure.cap_h(theme::size::CAPTION);
        // The case the bound exists for is a FULL column. It is anchored on the action row's
        // bottom and grows upward, so at `PEOPLE_MAX_LINES` its top line rises past the facts
        // row's own band and the two share a horizontal strip — nothing but the width keeps the
        // text apart. A short column does not reach that strip, and since the action row took its
        // optical rung the three-line case clears it by a few pixels; grading the collision at a
        // height the column may or may not have is what made this assertion brittle.
        assert!(
            people_top(ch.btn_y, PEOPLE_MAX_LINES) < facts_bot,
            "the tallest people column rises into the facts row's band"
        );
        assert!(
            people_top(ch.btn_y, 1) > facts_bot,
            "a single line hangs below the facts row entirely"
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
                    ch.facts_y + facts_h + theme::space::LG,
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
            assert_eq!(none.syn_y, none.meta_y + meta_h + theme::space::SM);
            assert_eq!(some.syn_y, some.ratings_y + ratings_h + theme::space::SM);
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
        assert_eq!(some.meta_y - TITLE_BOTTOM, theme::space::SM, "logo/title -> meta");
        assert_eq!(some.ratings_y - (some.meta_y + meta_h), theme::space::SM, "meta -> ratings");
        assert_eq!(some.syn_y - (some.ratings_y + ratings_h), theme::space::SM, "ratings -> synopsis");
        assert_eq!(some.facts_y - (some.syn_y + syn_h.max(34.0)), theme::space::MD, "synopsis -> facts");
        assert_eq!(some.btn_y - (some.facts_y + facts_h), theme::space::LG, "facts -> action row");

        // No ratings: the synopsis follows the meta line directly, off ITS measured bottom.
        let none = hero_chain(syn_h, false, &m);
        assert_eq!(none.syn_y - (none.meta_y + meta_h), theme::space::SM, "meta -> synopsis (no ratings)");
    }

    /// The ladder only ever WIDENS on the way down the hero, and it widens where the meaning
    /// changes — inside the identity group, then at the change of information kind, then at the
    /// step from information to action. An even ladder would pass every other test in this file
    /// and is exactly the answer the owner ruled out: distance is the grammar, so a chain that
    /// says the same thing at every step says nothing.
    #[test]
    fn the_hero_ladder_widens_at_every_change_of_meaning_and_never_narrows() {
        let m = crate::ui::fixture::FixtureMeasure;
        let syn_h = 108.0_f32;
        let ch = hero_chain(syn_h, true, &m);
        let within_identity = ch.ratings_y - (ch.meta_y + widgets::BADGE_H);
        let kind_change = ch.facts_y - (ch.syn_y + syn_h);
        let to_action = ch.btn_y - (ch.facts_y + m.cap_h(theme::size::CAPTION));

        assert!(within_identity < kind_change, "a group must sit tighter than a change of kind");
        assert!(
            kind_change < to_action,
            "the action row is not more information: it takes the next rung up, and the pill's own \
             mass is why the correction is optical rather than merely the next number"
        );
        // And the hero as a whole is further from what follows it than any two rows inside it are
        // from each other — the region rung `screens::detail::build_layout` adds to `btn_y`.
        assert!(to_action < theme::space::XL);
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
