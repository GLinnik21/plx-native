//! Search's document geometry. Both engine placement and rendering use these expressions.
use crate::search::Kind;
use crate::ui::card_row::{self, RowStyle};
use crate::ui::consts::{CARD_H, CARD_W, MARGIN_X, MARGIN_Y, SCR_H, SCR_W};
use crate::ui::machine::GroupId;
use crate::ui::Rect;

/// `pub(crate)`, alongside this module itself (`screens/search/mod.rs`'s `pub(crate) mod layout`)
/// so `ui::consts`'s overscan-rects audit can reach it as `crate::screens::search::layout::FIELD`
/// — the deleted legacy Search renderer's own `FIELD`/`CONTENT_TOP` this replaces.
pub(crate) const FIELD: Rect = Rect {
    x: MARGIN_X,
    y: 138.0,
    w: SCR_W - 2.0 * MARGIN_X,
    h: 80.0,
};
pub(super) const SCOPE_Y: f32 = FIELD.y + FIELD.h + 12.0;
/// See [`FIELD`]'s doc: also reachable as `crate::screens::search::CONTENT_TOP`. Value pinned at
/// 300.0 — the overscan audit and `top()`'s own layering both depend on it.
pub(crate) const CONTENT_TOP: f32 = 300.0;
pub(super) const HEAD_TO_ROW: f32 = 60.0;
pub(super) const KEYBOARD_H: f32 = 324.0;
pub(super) const SCOPE_H: f32 = crate::ui::theme::size::CAPTION as f32 * 1.35;
pub(super) const CLEAR_H: f32 = 60.0;
pub(super) const RECENT_CAP: usize = crate::search::recents::CAP;

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
pub(super) fn style(kind: Kind) -> RowStyle {
    let (w, h, circular) = match kind {
        Kind::Episode => (420.0, 236.0, false),
        Kind::Person => (250.0, 250.0, true),
        _ => (CARD_W, CARD_H, false),
    };
    RowStyle {
        w,
        h,
        circular,
        ..RowStyle::HOME
    }
}
pub(super) fn block_h(kind: Kind, expansion: f32) -> f32 {
    HEAD_TO_ROW + style(kind).h + caption_band(expansion)
}
pub(super) fn caption_band(expansion: f32) -> f32 {
    crate::ui::consts::UNDER_LABEL_AIR + card_row::under_band(expansion)
}
pub(super) fn top(kinds: &[Kind], index: usize, expansion: impl Fn(usize) -> f32) -> f32 {
    CONTENT_TOP
        + kinds
            .iter()
            .take(index)
            .enumerate()
            .map(|(i, kind)| block_h(*kind, expansion(i)))
            .sum::<f32>()
}
pub(super) fn reveal(scroll: f32, kinds: &[Kind], focused: usize) -> f32 {
    if focused >= kinds.len() {
        return 0.0;
    }
    let expansion = |i| if i == focused { 1.0 } else { 0.0 };
    let origin = top(kinds, focused, expansion);
    let height = block_h(kinds[focused], 1.0);
    let content = top(kinds, kinds.len(), expansion) + MARGIN_Y;
    card_row::reveal(
        scroll,
        origin + height - (SCR_H - MARGIN_Y),
        origin - CONTENT_TOP,
        (content - SCR_H).max(0.0),
    )
}
pub(super) fn recent(slot: usize, scroll: f32) -> Rect {
    Rect::new(
        MARGIN_X,
        CONTENT_TOP + crate::ui::table::HDR_H + slot as f32 * crate::ui::table::ROW_H - scroll,
        820.0,
        crate::ui::table::ROW_H,
    )
}
pub(super) fn clear(terms: usize, scroll: f32, measure: &dyn crate::ui::machine::Measure) -> Rect {
    let width = crate::ui::widgets::Button::pill_w_measured(
        c"Clear recent searches",
        crate::ui::theme::size::BODY,
        false,
        false,
        measure,
    );
    Rect::new(
        MARGIN_X + crate::ui::table::CONTENT_X,
        recent_block_bottom(terms, scroll) - CLEAR_H,
        width,
        CLEAR_H,
    )
}

pub(super) fn recent_block_bottom(terms: usize, scroll: f32) -> f32 {
    recent(terms.min(RECENT_CAP), scroll).y + crate::ui::theme::space::MD + CLEAR_H
}

pub(super) fn empty_band(editing: bool) -> Rect {
    let bottom = if editing { SCR_H - KEYBOARD_H } else { SCR_H };
    Rect::new(0.0, CONTENT_TOP, SCR_W, bottom - CONTENT_TOP)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Measure;
    impl crate::ui::machine::Measure for Measure {
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

    const OPEN: fn(usize) -> f32 = |_| 1.0;
    const ALL: [Kind; 5] = crate::search::KINDS;

    #[test]
    fn the_reserved_caption_band_holds_the_block_the_shared_component_draws() {
        let drawn = crate::ui::card_row::TileLabel::height(true);
        assert!(
            drawn <= caption_band(1.0),
            "the label block draws {drawn}px into a band of {}px",
            caption_band(1.0)
        );
    }

    #[test]
    fn shelves_stack_by_their_own_block_heights_from_the_content_top() {
        assert_eq!(top(&ALL, 0, OPEN), CONTENT_TOP);
        for i in 1..ALL.len() {
            assert_eq!(
                top(&ALL, i, OPEN) - top(&ALL, i - 1, OPEN),
                block_h(ALL[i - 1], 1.0),
                "shelf {i} did not start one block below shelf {}",
                i - 1
            );
        }
        assert_eq!(block_h(Kind::Movie, 1.0), crate::ui::consts::ROW_PITCH);
        assert_eq!(block_h(Kind::Show, 1.0), crate::ui::consts::ROW_PITCH);
        assert_eq!(block_h(Kind::Collection, 1.0), crate::ui::consts::ROW_PITCH);
        assert_eq!(
            block_h(Kind::Episode, 1.0),
            HEAD_TO_ROW + 236.0 + caption_band(1.0)
        );
        assert_eq!(
            block_h(Kind::Person, 1.0),
            HEAD_TO_ROW + 250.0 + caption_band(1.0)
        );
        assert_eq!(
            block_h(Kind::Movie, 1.0) - block_h(Kind::Episode, 1.0),
            CARD_H - 236.0
        );
        assert_eq!(
            top(&[Kind::Episode, Kind::Movie], 1, OPEN),
            CONTENT_TOP + block_h(Kind::Episode, 1.0)
        );
        assert_eq!(top(&[], 99, OPEN), CONTENT_TOP);
        assert_eq!(top(&ALL, 99, OPEN), top(&ALL, ALL.len(), OPEN));
    }

    #[test]
    fn the_first_shelfs_whole_row_clears_the_raised_keyboard() {
        let floor = SCR_H - KEYBOARD_H;
        for kind in ALL {
            let bottom = top(&[kind], 0, OPEN) + HEAD_TO_ROW + style(kind).h;
            assert!(
                bottom <= floor,
                "{kind:?}: the first row ends at {bottom}, under the keyboard at {floor}"
            );
        }
        assert_eq!(top(&[Kind::Movie], 0, OPEN) + HEAD_TO_ROW + CARD_H, 735.0);
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
        let e = |i| (i == 2) as i32 as f32;
        let shelf_top = top(&ALL, 2, e);
        assert!(
            shelf_top + block_h(ALL[2], 1.0) - want <= SCR_H,
            "its block bottom is still off screen"
        );
        assert!(
            shelf_top - want >= CONTENT_TOP,
            "it scrolled past the minimum — the shelf overshot upward"
        );
        assert_eq!(reveal(want, &ALL, 2), want);
        let one = reveal(0.0, &ALL, 1);
        let one_top = top(&ALL, 1, |i| (i == 1) as i32 as f32);
        assert!(
            one < one_top - CONTENT_TOP,
            "the reveal must undercut the pin, or it IS the pin"
        );
        assert_eq!(one, one_top + block_h(ALL[1], 1.0) - (SCR_H - MARGIN_Y));
        let last = ALL.len() - 1;
        let end = reveal(0.0, &ALL, last);
        let last_top = top(&ALL, last, |i| (i == last) as i32 as f32);
        let content = last_top + block_h(ALL[last], 1.0) + MARGIN_Y;
        assert_eq!(
            content - end,
            SCR_H,
            "the last block rests one panel above the flow's end"
        );
        assert_eq!(last_top + block_h(ALL[last], 1.0) - end, SCR_H - MARGIN_Y);
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
            clearance >= crate::ui::theme::space::LG,
            "a full block ends at {} and the keyboard starts at {kbd_top} — {clearance}px",
            recent_block_bottom(RECENT_CAP, 0.0)
        );
    }
}
