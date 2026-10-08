//! Search's document geometry. Both engine placement and rendering use these expressions.
use plx_data::search::Kind;
use plx_ui::cards::{self as ui_cards, RowStyle};
use plx_ui::consts::{CARD_H, CARD_W, MARGIN_X, MARGIN_Y, SCR_H, SCR_W};
use plx_machine::machine::GroupId;
use plx_ui::Rect;

/// `pub`, alongside this module itself (`screens/search/mod.rs`'s `pub mod layout`)
/// so `ui::consts`'s overscan-rects audit can reach it as `crate::search::layout::FIELD`
/// — the deleted legacy Search renderer's own `FIELD`/`CONTENT_TOP` this replaces.
pub const FIELD: Rect = Rect {
    x: MARGIN_X,
    y: 138.0,
    w: SCR_W - 2.0 * MARGIN_X,
    h: 80.0,
};
pub(super) const SCOPE_Y: f32 = FIELD.y + FIELD.h + 12.0;
/// See [`FIELD`]'s doc: also reachable as `crate::search::CONTENT_TOP`. Value pinned at
/// 300.0 — the overscan audit and `top()`'s own layering both depend on it.
pub const CONTENT_TOP: f32 = 300.0;
pub(super) const HEAD_TO_ROW: f32 = 60.0;
pub(super) const KEYBOARD_H: f32 = 324.0;
pub(super) const SCOPE_H: f32 = plx_ui::theme::size::CAPTION as f32 * 1.35;
pub(super) const CLEAR_H: f32 = 60.0;
pub(super) const RECENT_CAP: usize = plx_data::search::recents::CAP;

pub(super) fn ordinal(kind: Kind) -> u32 {
    match kind {
        Kind::Movie => 0,
        Kind::Show => 1,
        Kind::Episode => 2,
        Kind::Person => 3,
        Kind::Collection => 4,
    }
}
pub(super) fn group(kind: Kind) -> GroupId {
    GroupId(0x5345_4200 + ordinal(kind))
}
static EPISODE: RowStyle = RowStyle { w: 420.0, h: 236.0, ..RowStyle::HOME };
static PERSON: RowStyle = RowStyle { w: 250.0, h: 250.0, circular: true, ..RowStyle::HOME };
static POSTER: RowStyle = RowStyle { w: CARD_W, h: CARD_H, ..RowStyle::HOME };

/// The row style of `kind`'s shelf: the poster shelf's, with the still and the headshot sized to
/// their own aspect.
pub(super) fn style(kind: Kind) -> &'static RowStyle {
    match kind {
        Kind::Episode => &EPISODE,
        Kind::Person => &PERSON,
        _ => &POSTER,
    }
}
/// A shelf block's height under a caption band of `band` px (a shelf's live
/// [`Shelf::under_band`](plx_ui::cards::Shelf::under_band), or [`card_row::under_band`] of a
/// settled 0 / 1).
pub(super) fn block_h(kind: Kind, band: f32) -> f32 {
    HEAD_TO_ROW + style(kind).h + caption_band(band)
}
pub(super) fn caption_band(band: f32) -> f32 {
    plx_ui::consts::UNDER_LABEL_AIR + band
}
pub(super) fn top(kinds: &[Kind], index: usize, band: impl Fn(usize) -> f32) -> f32 {
    CONTENT_TOP
        + kinds
            .iter()
            .take(index)
            .enumerate()
            .map(|(i, kind)| block_h(*kind, band(i)))
            .sum::<f32>()
}
pub(super) fn reveal(scroll: f32, kinds: &[Kind], focused: usize) -> f32 {
    if focused >= kinds.len() {
        return 0.0;
    }
    let band = |i| ui_cards::under_band(if i == focused { 1.0 } else { 0.0 });
    let origin = top(kinds, focused, band);
    let height = block_h(kinds[focused], ui_cards::under_band(1.0));
    let content = top(kinds, kinds.len(), band) + MARGIN_Y;
    ui_cards::reveal(
        scroll,
        origin + height - (SCR_H - MARGIN_Y),
        origin - CONTENT_TOP,
        (content - SCR_H).max(0.0),
    )
}
pub(super) fn recent(slot: usize, scroll: f32) -> Rect {
    Rect::new(
        MARGIN_X,
        CONTENT_TOP + plx_ui::table::HDR_H + slot as f32 * plx_ui::table::ROW_H - scroll,
        820.0,
        plx_ui::table::ROW_H,
    )
}
pub(super) fn clear(terms: usize, scroll: f32, measure: &dyn plx_machine::machine::Measure) -> Rect {
    let width = plx_ui::widgets::Button::pill_w_measured(
        plx_platform::i18n::msg::browse_search_clear_c(),
        plx_ui::theme::size::BODY,
        false,
        false,
        measure,
    );
    Rect::new(
        MARGIN_X + plx_ui::table::CONTENT_X,
        recent_block_bottom(terms, scroll) - CLEAR_H,
        width,
        CLEAR_H,
    )
}

pub(super) fn recent_block_bottom(terms: usize, scroll: f32) -> f32 {
    recent(terms.min(RECENT_CAP), scroll).y + plx_ui::theme::space::MD + CLEAR_H
}

pub(super) fn empty_band(editing: bool) -> Rect {
    let bottom = if editing { SCR_H - KEYBOARD_H } else { SCR_H };
    Rect::new(0.0, CONTENT_TOP, SCR_W, bottom - CONTENT_TOP)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Measure;
    impl plx_machine::machine::Measure for Measure {
        fn width(&self, _: &std::ffi::CStr, _: i32, _: bool) -> f32 {
            0.0
        }
        fn cap_h(&self, _: i32) -> f32 {
            0.0
        }
        fn line_h(&self, _: i32) -> f32 {
            0.0
        }
    }

    #[test]
    fn clear_geometry_stays_at_the_recents_cap() {
        let measure = Measure;
        let capped = clear(RECENT_CAP, 0.0, &measure);
        let overlong = clear(RECENT_CAP + 1, 0.0, &measure);
        assert_eq!(overlong.y, capped.y);
    }

    fn open(_: usize) -> f32 {
        ui_cards::under_band(1.0)
    }
    fn full() -> f32 {
        ui_cards::under_band(1.0)
    }
    const ALL: [Kind; 5] = plx_data::search::KINDS;

    #[test]
    fn the_reserved_caption_band_holds_the_block_the_shared_component_draws() {
        let drawn = plx_ui::cards::TileLabel::height(true);
        assert!(
            drawn <= caption_band(full()),
            "the label block draws {drawn}px into a band of {}px",
            caption_band(full())
        );
    }

    #[test]
    fn shelves_stack_by_their_own_block_heights_from_the_content_top() {
        assert_eq!(top(&ALL, 0, open), CONTENT_TOP);
        for i in 1..ALL.len() {
            assert_eq!(
                top(&ALL, i, open) - top(&ALL, i - 1, open),
                block_h(ALL[i - 1], full()),
                "shelf {i} did not start one block below shelf {}",
                i - 1
            );
        }
        assert_eq!(block_h(Kind::Movie, full()), plx_ui::consts::ROW_PITCH);
        assert_eq!(block_h(Kind::Show, full()), plx_ui::consts::ROW_PITCH);
        assert_eq!(block_h(Kind::Collection, full()), plx_ui::consts::ROW_PITCH);
        assert_eq!(
            block_h(Kind::Episode, full()),
            HEAD_TO_ROW + 236.0 + caption_band(full())
        );
        assert_eq!(
            block_h(Kind::Person, full()),
            HEAD_TO_ROW + 250.0 + caption_band(full())
        );
        assert_eq!(
            block_h(Kind::Movie, full()) - block_h(Kind::Episode, full()),
            CARD_H - 236.0
        );
        assert_eq!(
            top(&[Kind::Episode, Kind::Movie], 1, open),
            CONTENT_TOP + block_h(Kind::Episode, full())
        );
        assert_eq!(top(&[], 99, open), CONTENT_TOP);
        assert_eq!(top(&ALL, 99, open), top(&ALL, ALL.len(), open));
    }

    #[test]
    fn the_first_shelfs_whole_row_clears_the_raised_keyboard() {
        let floor = SCR_H - KEYBOARD_H;
        for kind in ALL {
            let bottom = top(&[kind], 0, open) + HEAD_TO_ROW + style(kind).h;
            assert!(
                bottom <= floor,
                "{kind:?}: the first row ends at {bottom}, under the keyboard at {floor}"
            );
        }
        assert_eq!(top(&[Kind::Movie], 0, open) + HEAD_TO_ROW + CARD_H, 735.0);
        assert_eq!(floor, 756.0);
    }

    #[test]
    fn a_shelf_scrolls_only_as_far_as_its_own_block_needs() {
        assert_eq!(
            reveal(0.0, &ALL, 0),
            0.0,
            "the first shelf is already on screen"
        );
        let want = reveal(0.0, &ALL, 2);
        assert!(
            want > 0.0,
            "the third shelf is below the fold and must be revealed"
        );
        let e = |i| ui_cards::under_band((i == 2) as i32 as f32);
        let shelf_top = top(&ALL, 2, e);
        assert!(
            shelf_top + block_h(ALL[2], full()) - want <= SCR_H,
            "its block bottom is still off screen"
        );
        assert!(
            shelf_top - want >= CONTENT_TOP,
            "it scrolled past the minimum — the shelf overshot upward"
        );
        assert_eq!(reveal(want, &ALL, 2), want);
        let one = reveal(0.0, &ALL, 1);
        let one_top = top(&ALL, 1, |i| ui_cards::under_band((i == 1) as i32 as f32));
        assert!(
            one < one_top - CONTENT_TOP,
            "the reveal must undercut the pin, or it IS the pin"
        );
        assert_eq!(one, one_top + block_h(ALL[1], full()) - (SCR_H - MARGIN_Y));
        let last = ALL.len() - 1;
        let end = reveal(0.0, &ALL, last);
        let last_top = top(&ALL, last, |i| ui_cards::under_band((i == last) as i32 as f32));
        let content = last_top + block_h(ALL[last], full()) + MARGIN_Y;
        assert_eq!(
            content - end,
            SCR_H,
            "the last block rests one panel above the flow's end"
        );
        assert_eq!(last_top + block_h(ALL[last], full()) - end, SCR_H - MARGIN_Y);
        assert_eq!(
            reveal(0.0, &ALL, 99),
            0.0,
            "a shelf that is not there cannot scroll the page"
        );
    }

    /// Legacy `ui/search/mod.rs`'s `the_content_line_clears_the_documents_head`: the document's
    /// head is the field plus its scope line, and both are measured off [`FIELD`] — so this is the
    /// one assertion that keeps the three numbers agreeing when any of them moves. Without it
    /// shelf 0's heading would be drawn over the scope line at rest.
    #[test]
    fn the_content_line_clears_the_documents_head() {
        let head_bottom = SCOPE_Y + SCOPE_H;
        assert!(
            CONTENT_TOP > head_bottom,
            "content starts at {CONTENT_TOP} inside a head that ends at {head_bottom}"
        );
    }

    #[test]
    fn a_full_block_finishes_clear_of_the_raised_keyboard() {
        let kbd_top = SCR_H - KEYBOARD_H;
        let clearance = kbd_top - recent_block_bottom(RECENT_CAP, 0.0);
        assert!(
            clearance >= plx_ui::theme::space::LG,
            "a full block ends at {} and the keyboard starts at {kbd_top} — {clearance}px",
            recent_block_bottom(RECENT_CAP, 0.0)
        );
    }
}
